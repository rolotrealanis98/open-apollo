#!/usr/bin/env python3
"""Full DSP + routing init for Apollo Solo USB.

Builds the DSP bring-up stream from the DSP command language
(uausb/soloinit.py: strip teardown, Bill code loads placed in pool 0,
strip instances in pools 1-3, memspec, parameters, SYNCH enables), sends it
on the bulk pipe, then sets the UAC2 clock.  This configures the FPGA
routing matrix so capture channels receive signal.

The code blobs are UA's DSP modules from your own UAD install, extracted
by the setup wizard (open-apollo-setup) or `oa-assets firmware` into
/lib/firmware/universal-audio/dsp (--dsp-dir DIR elsewhere); each one's
SHA-256 must match uausb/solo-usb-init.json.  That file (the facts) holds
the rest of what a Windows capture of a Solo USB recorded: the strip
parameter blocks and the frame sizes.  At 48 kHz the frames sent are
byte-identical to that capture (the facts carry its SHA-256, and the log
says whether the stream matches).  --rate N puts N in a rate-ack frame
({0x001A0002, N}, sent first; also with --rate-ack at 48 kHz) and in the
Mic strips' ramp constants.  --replay sends the 48 kHz stream with the
old fixed timing.  --capture FILE takes the code, parameters and framing
from a capture of your own instead (with --replay: sends it as captured).
--check verifies the extracted modules and touches no device.

DSP-ready (issues #62, #69).  Before the stream the script polls the DSP
boot word over vendor request 0x04 (FPGA word 0x1069 = DSP 0 window +0x1A4)
every 300 ms until it leaves 0 / 0xF81A3C5E, as the Windows driver does,
and during the stream it waits for the DSP's ack of each code load instead
of the old fixed pauses.  If the word cannot be read, the DSP does not
start, or an ack does not come, it falls back to the old fixed waits
(including the 2 s pause before packet 24).

Untested on hardware: the generator, the boot-word poll and the ack waits
are checked against captures only.

Usage: sudo python3 tools/usb-full-init.py [--rate 48000] [--replay]
       sudo python3 tools/usb-full-init.py --part teardown,code   # U38 bisection
       python3 tools/usb-full-init.py --check [--dsp-dir DIR]
       python3 tools/usb-full-init.py --dry-run [--write-bin out.bin]

After running, reload snd-usb-audio:
    sudo rmmod snd_usb_audio; sudo insmod /tmp/sound/usb/snd-usb-audio.ko
"""
import argparse
import os
import struct
import sys
import time

_script_dir = os.path.dirname(os.path.abspath(__file__))
if _script_dir not in sys.path:
    sys.path.insert(0, _script_dir)

try:
    from uausb import device as udev
    from uausb import dspcmd, soloinit
except ImportError:             # an install with only this script copied
    udev = dspcmd = soloinit = None

UA_VID = 0x2B5A
LIVE_PIDS = {
    0x000D: "Apollo Solo USB",
    0x0002: "Twin USB",
    0x000F: "Twin X USB",
}
SOLO_PID = 0x000D

EP_BULK_OUT = 0x01
EP_BULK_IN = 0x81
# The extracted DSP modules (setup wizard / oa-assets firmware).  The facts
# are uausb/solo-usb-init.json, next to this script in the repository and
# in the installed layout (/usr/local/lib/ua-usb) alike.
DSP_DIR = "/lib/firmware/universal-audio/dsp"

# Old replay pacing (kept for --replay and as the generated path's fallback)
DSP_PROGRAM_END = 23            # last DSP program load packet
ROUTING_START = 24              # where the fixed 2 s pause goes
ROUTING_PAUSE = 2.0
ACK_TIMEOUT_MS = 1000
CAPTURE_RATE = 48000            # the rate of the captured session


def find_device(usb):
    """Return (device, model_name, pid) for whichever live Apollo USB is present."""
    for pid, name in LIVE_PIDS.items():
        dev = usb.core.find(idVendor=UA_VID, idProduct=pid)
        if dev:
            return dev, name, pid
    return None, None, None


def _drain(dev, usb, timeout_ms, limit=None):
    """Read bulk IN until a timeout; returns the frames read."""
    got = []
    try:
        while limit is None or len(got) < limit:
            got.append(bytes(dev.read(EP_BULK_IN, 1024, timeout=timeout_ms)))
    except usb.core.USBTimeoutError:
        pass
    return got


def _write_with_retries(dev, usb, i, pkt_data, sleep, timeout=10000):
    """One bulk OUT frame with the old three-attempt retry."""
    for attempt in range(3):
        try:
            dev.write(EP_BULK_OUT, pkt_data, timeout=timeout)
            return
        except usb.core.USBTimeoutError:
            if attempt == 2:
                # Packet 0 timing out after all retries means the
                # device is wedged — usually a halted EP that
                # survived clear_halt, or stale state from a racing
                # snd-usb-audio probe.  A one-time bulk-IN drain +
                # halt-clear gives the device one last chance before
                # we surface the error to the user.
                if i == 0:
                    try:
                        usb.util.clear_halt(dev, EP_BULK_OUT)
                        usb.util.clear_halt(dev, EP_BULK_IN)
                    except Exception:
                        pass
                    _drain(dev, usb, 200)
                    try:
                        dev.write(EP_BULK_OUT, pkt_data, timeout=timeout)
                        return
                    except usb.core.USBTimeoutError:
                        print("  [ 0] device unresponsive — "
                              "power-cycle the Apollo (unplug "
                              "USB, wait 5s, replug) and retry.")
                        raise
                raise
            wait = 1.0 * (attempt + 1)
            print(f"  [{i:2d}] timeout, retrying ({attempt+1}/3) "
                  f"after {wait:.0f}s...")
            sleep(wait)
            # Drain any stale responses before retry
            _drain(dev, usb, 100)


def read_capture(bin_path):
    """The frames of a capture file: u32 count, then u32 length + bytes
    per frame (little-endian).  Needs no uausb package."""
    with open(bin_path, "rb") as f:
        count = struct.unpack("<I", f.read(4))[0]
        out = []
        for _ in range(count):
            pkt_len = struct.unpack("<I", f.read(4))[0]
            out.append(f.read(pkt_len))
    return out


def replay_frames(dev, frames, usb, sleep=time.sleep):
    """Send frames with the old replay's fixed timing.

    The DSP needs time to process program loads before accepting routing
    writes.  Windows UA Console leaves 1+ second gaps between phases.
    """
    print(f"Replaying {len(frames)} bulk OUT packets...")
    for i, pkt_data in enumerate(frames):
        _send_old_paced(dev, usb, i, pkt_data, sleep)
    print(f"  Sent all {len(frames)} packets")


def replay_init_sequence(dev, bin_path, usb, sleep=time.sleep):
    """Replay a capture file of your own (the old path, unchanged)."""
    replay_frames(dev, read_capture(bin_path), usb, sleep)


def _send_old_paced(dev, usb, i, pkt_data, sleep, pause=True):
    """One frame with the old replay's pauses, retries and drains."""
    wc, cmd_type, magic = struct.unpack_from("<HBB", pkt_data, 0)
    pkt_len = len(pkt_data)

    # Phase transition delay — let DSP finish processing programs
    if i == ROUTING_START and pause:
        print("  -- waiting for DSP to process program loads --")
        sleep(ROUTING_PAUSE)

    # Inter-packet pacing: small delay between all packets,
    # longer delay after large ones (DSP program uploads)
    if i > 0:
        sleep(0.2 if pkt_len > 512 else 0.05)

    # All packets get generous timeout (AMD xHCI can be slow)
    _write_with_retries(dev, usb, i, pkt_data, sleep)
    print(f"  [{i:2d}] type={cmd_type:3d} words={wc:3d} len={pkt_len}")

    # Drain responses — wait longer after big packets
    _drain(dev, usb, 1000 if pkt_len > 512 else 500)


def load_template(capture=None, dsp_dir=None, facts=None,
                  accept_unknown=False):
    """The generator's template: from a capture of your own, else the
    extracted DSP modules plus the facts.  Raises soloinit.TemplateError
    (FirmwareError for a missing or unknown module) with the reason."""
    if capture:
        try:
            return soloinit.Template.from_file(capture)
        except OSError as e:
            raise soloinit.TemplateError(
                f"cannot read {capture}: {e.strerror or e}") from None
        except dspcmd.DspCommandError as e:
            raise soloinit.TemplateError(f"{capture}: {e}") from None
    return soloinit.Template.from_firmware(dsp_dir or DSP_DIR, facts,
                                           verify=not accept_unknown)


def build_plan(rate=48000, rate_ack=True, connect=False, template=None,
               now=time.time, parts=None):
    """The generated init (uausb.soloinit.InitPlan) for the Solo USB."""
    if template is None:
        template = load_template()
    date = dspcmd.connect_date_word(now()) if connect else None
    tag = int(now() * 1000) & 0xFFFFFFFF
    return soloinit.build_init(template, rate=rate, rate_ack=rate_ack,
                               connect_date=date, query_tag=tag, parts=parts)


def parse_parts(text):
    """--part teardown,code,... -> a set of soloinit.PARTS (None = all)."""
    if not text or text == "all":
        return None
    parts = {p.strip() for p in text.split(",") if p.strip()}
    bad = parts - set(soloinit.PARTS)
    if bad:
        raise SystemExit(f"--part: unknown {', '.join(sorted(bad))} "
                         f"(choose from {', '.join(soloinit.PARTS)})")
    return parts


def _completed_loads(plan):
    """{frame index: [module ids whose LOAD_CODE ends in that frame]}."""
    out, pos, k = {}, 0, 0
    ends = []
    for f in plan.frames:
        pos += len(f.words)
        ends.append(pos)
    pos = 0
    for c in plan.commands():
        pos += c.length
        while ends[k] < pos:
            k += 1
        if c.op == dspcmd.OP_LOAD_CODE:
            out.setdefault(k, []).append(c.args[2] & 0xFFFF)
    return out


def wait_ready(dev, log=print, sleep=time.sleep):
    """Poll the DSP boot word.  True: started (ack-driven pacing);
    False: fall back to the fixed waits."""
    res = udev.wait_dsp_started(udev.UsbRegs(dev), 0, sleep=sleep)
    if res.error:
        log(f"  DSP boot word unreadable ({res.error}): using the fixed waits")
        return False
    if not res.started:
        log(f"  DSP did not start in {res.tries} tries "
            f"(boot word {res.value:#010x}): using the fixed waits")
        return False
    log(f"  DSP started: boot word {res.value:#010x} after {res.tries} read(s)")
    if not res.ok:
        log("  warning: boot word bit 0 clear (the UA driver treats this as a "
            "failed DSP); continuing")
    return True


def connect_handshake(dev, usb, plan, log=print):
    """Send the preamble's connect frames and wait for the 0x8003 reply."""
    tag = plan.transactions[1][1].args[0]
    for f in plan.frames[:2]:
        dev.write(EP_BULK_OUT, f.to_bytes(), timeout=2000)
    for raw in _drain(dev, usb, 2000, limit=16):
        try:
            for op, args in dspcmd.parse_reply(raw):
                if op == 0x8003 and args and args[0] == tag:
                    log("  connect: DSP answered (0x8003 reply)")
                    return True
        except dspcmd.DspCommandError:
            continue
    log("  connect: no 0x8003 reply")
    return False


def send_generated(dev, usb, plan, ready, sleep=time.sleep, log=print,
                   skip_frames=0):
    """Send plan.frames[skip_frames:].  ready: ack-driven pacing; on a
    missing ack (or not ready) the old replay pacing takes over."""
    loads = _completed_loads(plan)
    body0 = plan.preamble
    paused = False
    acks_seen = 0
    for k in range(skip_frames, len(plan.frames)):
        data = plan.frames[k].to_bytes()
        idx = k - body0             # the old packet number for the body
        if not ready:
            if idx < 0:
                _write_with_retries(dev, usb, idx, data, sleep)
                log(f"  [pre] {len(data)} bytes")
                _drain(dev, usb, 500)
            else:
                _send_old_paced(dev, usb, idx, data, sleep, pause=not paused)
                paused |= idx == ROUTING_START
            continue
        if idx >= ROUTING_START and not paused and acks_seen == 0:
            paused = True           # nothing acked: the old pause
            log("  -- no code-load ack seen: waiting for the DSP --")
            sleep(ROUTING_PAUSE)
        _write_with_retries(dev, usb, max(idx, 0), data, sleep)
        log(f"  [{idx:2d}] words={len(plan.frames[k].words):3d} len={len(data)}")
        want = loads.get(k, [])
        if not want:
            _drain(dev, usb, 20)
            continue
        got = _wait_acks(dev, usb, want, log)
        acks_seen += len(got)
        if len(got) < len(want):
            missing = [m for m in want if m not in got]
            log("  no ack for module(s) " +
                ", ".join(f"{m:#x}" for m in missing) +
                ": falling back to the fixed waits")
            ready = False
            if idx < ROUTING_START:
                sleep(0.2)
            elif not paused:
                paused = True
                sleep(ROUTING_PAUSE)
    return ready


def _wait_acks(dev, usb, modules, log):
    """Read bulk IN until every module in `modules` was acked (code-load
    reply {0x80070004, status, id, word0}) or ACK_TIMEOUT_MS passes."""
    got = {}
    deadline = time.monotonic() + ACK_TIMEOUT_MS / 1000.0
    while len(got) < len(modules) and time.monotonic() < deadline:
        left = max(1, int((deadline - time.monotonic()) * 1000))
        try:
            raw = bytes(dev.read(EP_BULK_IN, 1024, timeout=left))
        except usb.core.USBTimeoutError:
            break
        try:
            replies = dspcmd.parse_reply(raw)
        except dspcmd.DspCommandError:
            continue
        for status, module, _w0 in dspcmd.code_load_ack(replies):
            got[module] = status
            if status:
                log(f"  warning: module {module:#x} acked with status "
                    f"{status:#010x}")
    return got


def set_clock(dev, rate):
    dev.ctrl_transfer(0x21, 0x01, 0x0100, 0x8001,
                      struct.pack("<I", rate), timeout=2000)
    data = dev.ctrl_transfer(0xA1, 0x01, 0x0100, 0x8001, 4, timeout=1000)
    return struct.unpack("<I", bytes(data))[0]


def want_rate_ack(args):
    """--rate-ack / --no-rate-ack, else only for a rate other than the
    captured session's 48 kHz."""
    if args.rate_ack is not None:
        return args.rate_ack
    return args.rate != CAPTURE_RATE


def identity_note(plan):
    """How the generated strip stream compares with the captured one."""
    if plan.capture_sha256 is None:
        return "no captured stream to compare with"
    if plan.byte_identical:
        return "byte-identical to the captured stream"
    return "differs from the captured stream" + (
        "" if plan.rate == CAPTURE_RATE else f" (rate {plan.rate})")


def template_args(args):
    return dict(capture=args.capture, dsp_dir=args.dsp_dir,
                facts=args.facts, accept_unknown=args.accept_unknown_modules)


def check(args):
    """--check: the modules and the facts are usable; no device."""
    template = load_template(**template_args(args))
    plan = build_plan(CAPTURE_RATE, False, template=template)
    if args.capture:
        print(f"Capture {args.capture}: modules "
              + " ".join(f"{m:03x}" for m in template.blobs))
    else:
        print(f"DSP modules OK in {args.dsp_dir}: "
              + " ".join(f"{m:03x}" for m in template.blobs))
    print(f"Init stream: {len(plan.frames)} frames, "
          f"{len(plan.commands())} commands, {identity_note(plan)}")
    return 0


def dry_run(args):
    template = load_template(**template_args(args))
    plan = build_plan(args.rate, want_rate_ack(args), args.connect,
                      template=template, parts=parse_parts(args.part))
    print(f"{len(plan.frames)} frames ({plan.preamble} preamble), "
          f"{len(plan.commands())} commands, rate {plan.rate}, "
          f"capture framing {'yes' if plan.capture_framing else 'no'}, "
          f"{identity_note(plan)}")
    for tx in plan.transactions:
        for c in tx:
            print("  " + c.describe())
    if args.write_bin:
        with open(args.write_bin, "wb") as f:
            f.write(dspcmd.build_urb_container(plan.urbs()))
        print(f"wrote {args.write_bin}")
    return 0


def main(argv=None, usb=None, sleep=time.sleep):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--rate", type=int, default=48000,
                    help="sample rate for the rate ack, the strip constants "
                         "and the UAC2 clock (default 48000; only 48000 "
                         "matches a capture)")
    ap.add_argument("--replay", action="store_true",
                    help="send the 48 kHz stream with the old fixed timing "
                         "(no rate ack, no ready poll); with --capture, the "
                         "capture file as it is")
    ap.add_argument("--dsp-dir", metavar="DIR", default=DSP_DIR,
                    help="the DSP modules extracted from your UAD install "
                         f"(default {DSP_DIR})")
    ap.add_argument("--facts", metavar="FILE",
                    help="the init facts (default uausb/solo-usb-init.json "
                         "next to this script)")
    ap.add_argument("--capture", metavar="FILE",
                    help="take the code, parameters and framing from a "
                         "capture of your own (u32 count, then u32 length + "
                         "bytes per bulk OUT frame) instead of the "
                         "extracted modules and the facts")
    ap.add_argument("--accept-unknown-modules", action="store_true",
                    help="use .bill files whose SHA-256 is not the captured "
                         "version (an experiment: the stream then differs "
                         "from the capture)")
    ack = ap.add_mutually_exclusive_group()
    ack.add_argument("--rate-ack", dest="rate_ack", action="store_true",
                     default=None,
                     help="send the {0x1A0002, rate} frame first (default: "
                          "only when --rate is not the captured 48000, so "
                          "the default stream is the capture's bytes)")
    ack.add_argument("--no-rate-ack", dest="rate_ack", action="store_false",
                     help="never send the rate-ack frame")
    ap.add_argument("--connect", action="store_true",
                    help="first send the Windows bring-up's connect "
                         "({0x230002, 1}, date word, 0x11 query) and wait "
                         "for its reply (experiment for issue #62)")
    ap.add_argument("--no-ready-poll", action="store_true",
                    help="skip the DSP boot-word poll and the ack waits "
                         "(old fixed timing)")
    ap.add_argument("--generate", action="store_true",
                    help="build the stream on a Twin USB / Twin X USB too "
                         "(the Solo USB's strip plan; see issue #69)")
    ap.add_argument("--part", metavar="LIST", default="all",
                    help="send only these phases of the generated init, "
                         "comma-separated: teardown, code, strips, enable "
                         "(the U38 bisection; default all)")
    ap.add_argument("--check", action="store_true",
                    help="check the extracted DSP modules and the facts, "
                         "touch no device")
    ap.add_argument("--dry-run", action="store_true",
                    help="print the generated commands, touch no device")
    ap.add_argument("--write-bin", metavar="PATH",
                    help="with --dry-run: write the frames as a capture "
                         "file (u32 count, then u32 length + bytes per frame)")
    args = ap.parse_args(argv)

    if args.check or args.dry_run:
        if soloinit is None:
            print("uausb package missing next to this script")
            return 1
        try:
            return check(args) if args.check else dry_run(args)
        except soloinit.TemplateError as e:
            print(f"error: {e}")
            return 1

    if usb is None:
        import usb.core
        import usb.util

    dev, model, pid = find_device(usb)
    if not dev:
        print("No live Apollo USB device found "
              "(expected one of: %s)" %
              ", ".join(LIVE_PIDS.values()))
        return 1
    print(f"Found: {model} — {dev.product}")

    replay = args.replay
    if pid != SOLO_PID:
        print(f"warning: the init stream is the Apollo Solo USB's; on a "
              f"{model} it is known to stall the DSP (issue #69)")
        if not args.generate:
            replay = True

    # What to send, before touching the device's interface
    frames = plan = None
    if args.capture and not os.path.exists(args.capture):
        print(f"Missing: {args.capture}")
        return 1
    if soloinit is None:
        if not (args.capture and replay):
            print("uausb package missing next to this script: reinstall "
                  "(scripts/install-usb.sh), or send a capture of your own "
                  "with --capture FILE --replay")
            return 1
        frames = read_capture(args.capture)
    elif replay and args.capture:
        frames = read_capture(args.capture)
    else:
        try:
            template = load_template(**template_args(args))
        except soloinit.TemplateError as e:
            print(f"error: {e}")
            return 1
        if replay:
            # the 48 kHz stream as the old replay sent it
            plan = build_plan(CAPTURE_RATE, False, template=template)
            frames = plan.urbs()
            print(f"Replay: {len(frames)} frames, {identity_note(plan)}")
        else:
            plan = build_plan(args.rate, want_rate_ack(args), args.connect,
                              template=template, parts=parse_parts(args.part))

    # Only detach Interface 0 (vendor DSP). Leave audio interfaces for snd-usb-audio.
    try:
        if dev.is_kernel_driver_active(0):
            dev.detach_kernel_driver(0)
    except Exception:
        pass
    usb.util.claim_interface(dev, 0)

    # Normalize device state before replay.  Three failure modes get cleared here:
    #   1. Stale bulk-IN responses from a prior udev-triggered init — drain them
    #      or the first bulk OUT times out waiting for the device to service us.
    #   2. Halted bulk endpoints from an interrupted transfer (prior Ctrl-C,
    #      reboot mid-init, or a racing snd-usb-audio probe) — clear_halt resets
    #      the STALL condition so packet 0 can actually leave the host.
    #   3. DSP already running from a prior boot init — the FPGA sequence counter
    #      is mid-sequence, so re-running from packet 0 hangs.  A bulk-IN drain
    #      pulls any pending state replies; a subsequent init overwrites FPGA
    #      state cleanly because every packet carries an absolute write address.
    try:
        usb.util.clear_halt(dev, EP_BULK_OUT)
    except Exception:
        pass
    try:
        usb.util.clear_halt(dev, EP_BULK_IN)
    except Exception:
        pass
    # Drain any stale responses queued from a prior init.
    _drain(dev, usb, 50, limit=32)

    rate = args.rate
    if replay:
        # Step 1: Full init sequence (FPGA + routing + DSP programs)
        replay_frames(dev, frames, usb, sleep)
        rate = CAPTURE_RATE
    else:
        print(f"Generated init: {len(plan.frames)} frames, "
              f"{len(plan.commands())} commands, rate {rate}"
              + ("" if rate == CAPTURE_RATE else
                 " (strip filters keep their 48 kHz coefficients)")
              + f"; {identity_note(plan)}")
        ready = False
        if not args.no_ready_poll:
            ready = wait_ready(dev, sleep=sleep)
        skip = 0
        if args.connect:
            connect_handshake(dev, usb, plan)
            skip = 2
        send_generated(dev, usb, plan, ready, sleep, skip_frames=skip)
        print(f"  Sent all {len(plan.frames) - skip} frames")

    # Step 2: Set clock
    freq = set_clock(dev, rate)
    print(f"Clock: {freq} Hz")

    usb.util.release_interface(dev, 0)

    # NOTE: deliberately NOT writing setting[2] (monitor core) here.
    # It holds volume/mute/source/dim state that the ARM MCU needs for the
    # front panel knob and buttons to work. Writing it with a non-zero mask
    # during init overwrites the firmware's own defaults and breaks physical
    # knob control — see the Register-Map and Initialization-Sequence pages on
    # the GitHub wiki.

    print("Ready — run 'sudo modprobe snd_usb_audio' to get ALSA card")
    return 0


if __name__ == "__main__":
    sys.exit(main())
