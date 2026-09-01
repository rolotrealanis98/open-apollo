#!/usr/bin/env python3
"""Extract an Apollo USB init sequence from a USBPcap capture.

Reads a Windows USBPcap capture (.pcap or .pcapng) of the Apollo powering
on under the UA driver, and writes the host-to-device traffic that brings
the device up as a replayable sequence for init-replay.py:

  * bulk OUT packets on EP 0x01 (the 0xDC DSP command stream), and
  * vendor/class control OUT transfers (bmRequestType 0x40/0x41),

interleaved in their original order with the original inter-packet
timing, split into phases wherever the host paused for longer than
--gap seconds.  The FX3 firmware download (bRequest 0xA0) is excluded:
firmware is loaded separately by tools/fx3-load.py from the image UA
ships.

The control transfers matter.  On an Apollo Twin USB the init is 2341
bulk packets AND 45 control transfers; a bulk-only replay brings up audio
but not preamp state or the UAD plugin chain.

Output format ("APLO2"):
    b"APLO2", u32 count, then per record
    u32 delta_us   microseconds since the previous record (capped at 5 s)
    u16 phase      0-based, increments at every gap > --gap
    u8  kind       0 = bulk OUT, 1 = control OUT
    u8  bmRequestType, u8 bRequest, u8 pad      (control only)
    u16 wValue, u16 wIndex                      (control only)
    u16 len, then len bytes of data

The sequence is specific to the device model and firmware it was captured
from, and contains DSP program data UA ships with its driver.  Keep it
local, like the firmware image; see docs/device-capture-windows.

Usage:
    python3 pcap-init-extract.py capture.pcapng -o init.seq
    python3 pcap-init-extract.py capture.pcapng -o init.seq --address 11
    python3 pcap-init-extract.py capture.pcapng -o init.seq --end 1787645258.9

Requires: tshark (Wireshark) on PATH.
"""

import argparse
import collections
import struct
import subprocess
import sys

UA_VID = 0x2B5A
LIVE_PIDS = {
    0x000D: "Apollo Solo USB",
    0x0002: "Twin USB",
    0x000F: "Twin X USB",
}
FX3_FW_DOWNLOAD = 0xA0
MAGIC_CMD = 0xDC


def tshark(pcap, display_filter, fields):
    cmd = ["tshark", "-r", pcap, "-Y", display_filter, "-T", "fields"]
    for f in fields:
        cmd += ["-e", f]
    try:
        res = subprocess.run(cmd, capture_output=True, text=True, check=True)
    except FileNotFoundError:
        sys.exit("tshark not found — install Wireshark (tshark)")
    except subprocess.CalledProcessError as e:
        sys.exit(f"tshark failed: {e.stderr.strip()}")
    return [line.split("\t") for line in res.stdout.splitlines()]


def find_live_address(pcap):
    """Return (device_address, pid) of the post-firmware Apollo in the capture."""
    seen = {}
    for row in tshark(pcap, "usb.idVendor == 0x2b5a",
                      ["usb.device_address", "usb.idProduct"]):
        if len(row) < 2 or not row[0]:
            continue
        seen[int(row[0])] = int(row[1], 16)
    live = [(a, p) for a, p in seen.items() if p in LIVE_PIDS]
    if not live:
        pids = ", ".join(f"0x{p:04x}" for p in seen.values()) or "none"
        sys.exit(f"no live Apollo (PID 0x000d/0x0002/0x000f) in capture; "
                 f"VID 2b5a devices seen: {pids}")
    if len(live) > 1:
        sys.exit("more than one live Apollo in capture; pick one with --address: "
                 + ", ".join(f"{a} ({LIVE_PIDS[p]})" for a, p in live))
    return live[0]


def hexbytes(s):
    return bytes.fromhex(s.replace(":", "")) if s else b""


def extract(pcap, addr, start, end):
    events = []
    # Bulk OUT on EP 0x01 carrying a 0xDC command
    for row in tshark(pcap, f"usb.device_address == {addr} && "
                            f"usb.endpoint_address == 0x01 && usb.capdata",
                      ["frame.time_epoch", "usb.capdata"]):
        if len(row) < 2 or not row[1]:
            continue
        t = float(row[0])
        d = hexbytes(row[1])
        if len(d) < 4 or d[3] != MAGIC_CMD:
            continue
        if len(d) != 4 + d[0] * 4:
            print(f"  ! bulk packet at {t:.6f} has inconsistent length, skipped")
            continue
        events.append((t, 0, 0, 0, 0, 0, d))
    # The FX3 download goes to the loader PID, which has a different device
    # address, so count it across the whole capture rather than per address.
    fw_frames = len(tshark(pcap, "usb.bmRequestType == 0x40 && usb.setup.bRequest == 160"
                                 " && usb.control_stage == 0", ["frame.number"]))
    # Vendor/class control OUT, setup stage only
    for row in tshark(pcap, f"usb.device_address == {addr} && "
                            "(usb.bmRequestType == 0x41 || usb.bmRequestType == 0x40) "
                            "&& usb.control_stage == 0",
                      ["frame.time_epoch", "usb.bmRequestType", "usb.setup.bRequest",
                       "usb.setup.wValue", "usb.setup.wIndex", "usb.setup.wLength",
                       "usb.data_fragment"]):
        row = (row + [""] * 7)[:7]
        if not row[1]:
            continue
        t = float(row[0])
        brt = int(row[1], 16)
        breq = int(row[2])
        if breq == FX3_FW_DOWNLOAD:
            continue
        wv = int(row[3], 16)
        wi = int(row[4], 16) if row[4].startswith("0x") else int(row[4] or 0)
        wl = int(row[5] or 0)
        d = hexbytes(row[6])
        if wl and len(d) != wl:
            print(f"  ! control bReq={breq} wValue=0x{wv:04x} wLength={wl} "
                  f"but {len(d)} bytes captured, skipped")
            continue
        events.append((t, 1, brt, breq, wv, wi, d))
    events.sort(key=lambda e: e[0])
    if start is not None:
        events = [e for e in events if e[0] >= start]
    if end is not None:
        events = [e for e in events if e[0] < end]
    return events, fw_frames


def phase_registers(records):
    """Count bulk entries per register per phase.

    records: iterable of (phase, flags, payload_bytes) for bulk packets in
    order.  Entries are walked with their real length ((reg << 16) |
    wordcount, wordcount inclusive) and may continue across packets within
    the same flags stream, so a partial entry carried over from the previous
    packet of that stream is skipped rather than misread.  An entry counts
    toward the phase of the packet it starts in.
    """
    per_phase = collections.defaultdict(collections.Counter)
    carry = {}                      # flags -> dwords still owed to an open entry
    for phase, flags, payload in records:
        words = struct.unpack_from("<%dI" % (len(payload) // 4), payload, 0)
        i = carry.get(flags, 0)
        if i >= len(words):
            carry[flags] = i - len(words)
            continue
        while i < len(words):
            wc = words[i] & 0xFFFF
            if wc == 0:
                break
            per_phase[phase][words[i] >> 16] += 1
            i += wc
        carry[flags] = max(0, i - len(words))
    return per_phase


def describe_phases(per_phase):
    """Return ({phase: 'top registers'}, chain_phase_or_None)."""
    top = {}
    for phase, regs in per_phase.items():
        top[phase] = " ".join("0x%04x:%d" % (r, n) for r, n in regs.most_common(3))
    chain = max(per_phase, key=lambda p: per_phase[p][0x000C], default=None)
    if chain is not None and per_phase[chain][0x000C] < 100:
        chain = None
    return top, chain


def write_seq(events, out, gap):
    recs = []
    phase = 0
    for i, e in enumerate(events):
        delta = 0.0 if i == 0 else e[0] - events[i - 1][0]
        if i and delta > gap:
            phase += 1
        recs.append((delta, phase, e))
    with open(out, "wb") as f:
        f.write(b"APLO2")
        f.write(struct.pack("<I", len(recs)))
        for delta, ph, (t, kind, brt, breq, wv, wi, d) in recs:
            f.write(struct.pack("<IHBBBBHHH", min(int(delta * 1e6), 5_000_000),
                                ph, kind, brt, breq, 0, wv, wi, len(d)))
            f.write(d)
    return recs


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("pcap", help="USBPcap capture (.pcap or .pcapng)")
    ap.add_argument("-o", "--output", required=True, help="output .seq file")
    ap.add_argument("--address", type=int, default=None,
                    help="USB device address of the live Apollo (default: auto)")
    ap.add_argument("--start", type=float, default=None,
                    help="ignore events before this epoch time")
    ap.add_argument("--end", type=float, default=None,
                    help="ignore events at/after this epoch time (e.g. sweep start)")
    ap.add_argument("--gap", type=float, default=0.15,
                    help="idle gap that starts a new phase, seconds (default 0.15)")
    a = ap.parse_args()

    if a.address is None:
        addr, pid = find_live_address(a.pcap)
        print(f"Live Apollo: {LIVE_PIDS[pid]} (PID 0x{pid:04x}) at device address {addr}")
    else:
        addr = a.address

    events, fw_frames = extract(a.pcap, addr, a.start, a.end)
    nb = sum(1 for e in events if e[1] == 0)
    nc = len(events) - nb
    if not events:
        sys.exit("no init traffic found for that device address")
    print(f"{nb} bulk + {nc} control = {len(events)} events, "
          f"{events[-1][0] - events[0][0]:.1f} s of wall clock")
    if fw_frames:
        print(f"{fw_frames} FX3 firmware frames (bRequest 0xA0) excluded — "
              f"cold start captured")
    else:
        print("warning: no FX3 firmware download in this window — the device was "
              "already running firmware when the capture started, so this may not "
              "be a complete init")
    if nc == 0:
        print("warning: no control transfers captured; a bulk-only sequence "
              "replays roughly half of the init")

    recs = write_seq(events, a.output, a.gap)
    print(f"wrote {a.output}")
    counts = collections.Counter((ph, e[1]) for _, ph, e in recs)
    ctrl = collections.defaultdict(collections.Counter)
    for _, ph, e in recs:
        if e[1] == 1:
            ctrl[ph][f"bReq{e[3]}"] += 1
    top, chain = describe_phases(phase_registers(
        (ph, e[6][1], e[6][4:]) for _, ph, e in recs if e[1] == 0))
    print("PHASE   BULK  CTRL  top registers            control detail")
    for ph in sorted({p for p, _ in counts}):
        detail = " ".join(f"{k}:{v}" for k, v in sorted(ctrl[ph].items()))
        print(f"  {ph:3d} {counts.get((ph, 0), 0):6d} {counts.get((ph, 1), 0):5d}  "
              f"{top.get(ph, ''):<24} {detail}")
    # Phase numbers depend on where this capture's driver happened to pause,
    # so say which phase is the plugin-chain upload instead of assuming one.
    if chain is not None:
        print(f"plugin chain (reg 0x000c) is phase {chain}; "
              f"--phases 0-{chain - 1} on init-replay.py is the bring-up without it")
    else:
        print("no plugin-chain upload (reg 0x000c) found — Console had no plugins loaded, "
              "or the capture stopped early")


if __name__ == "__main__":
    main()
