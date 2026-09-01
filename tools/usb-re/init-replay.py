#!/usr/bin/env python3
"""Replay an Apollo USB init sequence built by pcap-init-extract.py.

Sends the captured bulk OUT packets and vendor control OUT transfers to a
live Apollo (post-firmware PID) in their original order, honouring the
captured inter-packet timing up to --max-delay, and reports what the DSP
sent back per phase.

The sequence must come from the same model and firmware as the device it
is replayed on: DSP addresses inside it are firmware-specific.

Timing that matters, learned on an Apollo Twin USB:

  * Start no earlier than ~30 s after the device re-enumerates with its
    live PID.  Started ~5 s after enumeration the replay is acknowledged
    packet for packet, converters and routing come up, and the UAD plugin
    chain silently never instantiates (issue #62).
  * Inter-phase gaps are real.  The DSP needs time after program loads;
    clamping every gap to a small constant works from a warm DSP and
    fails from a cold one.  --max-delay only caps the longest pauses.

Usage:
    sudo python3 init-replay.py init.seq --dry-run      # list phases, send nothing
    sudo python3 init-replay.py init.seq                # replay every phase
    sudo python3 init-replay.py init.seq --phases 0-5   # bring-up only

Requires: pyusb.
"""

import argparse
import collections
import struct
import sys
import time

import usb.core
import usb.util

UA_VID = 0x2B5A
LIVE_PIDS = {
    0x000D: "Apollo Solo USB",
    0x0002: "Twin USB",
    0x000F: "Twin X USB",
}
EP_BULK_OUT = 0x01
EP_BULK_IN = 0x81
HANDSHAKE_REQUESTS = (5, 7, 12, 13)   # arm / commit / apply: the DSP replies to these


def find_device():
    for pid, name in LIVE_PIDS.items():
        dev = usb.core.find(idVendor=UA_VID, idProduct=pid)
        if dev:
            return dev, name
    return None, None


def read_seq(path):
    with open(path, "rb") as f:
        if f.read(5) != b"APLO2":
            sys.exit(f"{path}: not an APLO2 sequence (see pcap-init-extract.py)")
        count = struct.unpack("<I", f.read(4))[0]
        events = []
        for _ in range(count):
            delta, ph, kind, brt, breq, _pad, wv, wi, ln = \
                struct.unpack("<IHBBBBHHH", f.read(16))
            events.append((delta / 1e6, ph, kind, brt, breq, wv, wi, f.read(ln)))
    return events


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


def parse_phases(spec, available):
    if spec == "all":
        return set(available)
    lo, _, hi = spec.partition("-")
    lo = int(lo)
    hi = int(hi) if hi else lo
    return set(range(lo, hi + 1))


def drain(dev, timeout_ms):
    got = 0
    try:
        while True:
            r = dev.read(EP_BULK_IN, 1024, timeout=timeout_ms)
            if not len(r):
                break
            got += 1
    except usb.core.USBError:
        pass
    return got


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("seq", help="sequence file from pcap-init-extract.py")
    ap.add_argument("--phases", default="all",
                    help='phase range to send, e.g. "0-5" (default: all)')
    ap.add_argument("--max-delay", type=float, default=5.0,
                    help="cap on any single captured pause, seconds (default 5)")
    ap.add_argument("--dry-run", action="store_true",
                    help="list what would be sent and exit")
    a = ap.parse_args()

    events = read_seq(a.seq)
    phases_present = sorted({e[1] for e in events})
    want = parse_phases(a.phases, phases_present)
    sel = [e for e in events if e[1] in want]
    n_bulk = sum(1 for e in sel if e[2] == 0)
    print(f"{len(events)} events in {len(phases_present)} phases; "
          f"sending {len(sel)} ({n_bulk} bulk, {len(sel) - n_bulk} control)")

    if a.dry_run:
        per = collections.defaultdict(lambda: [0, 0, 0.0])
        for delta, ph, kind, *_ in events:
            per[ph][kind] += 1
            per[ph][2] += delta
        top, chain = describe_phases(phase_registers(
            (ph, data[1], data[4:]) for _, ph, kind, _b, _r, _v, _i, data in events if kind == 0))
        print("PHASE   BULK  CTRL  captured s  top registers")
        for ph in phases_present:
            mark = "" if ph in want else "   (skipped)"
            print(f"  {ph:3d} {per[ph][0]:6d} {per[ph][1]:5d} {per[ph][2]:10.2f}  "
                  f"{top.get(ph, ''):<24}{mark}")
        if chain is not None:
            print(f"plugin chain (reg 0x000c) is phase {chain}")
        for delta, ph, kind, brt, breq, wv, wi, data in sel:
            if kind == 1:
                print(f"  phase {ph}: CTRL bmRequestType=0x{brt:02x} bReq={breq} "
                      f"wValue=0x{wv:04x} wIndex=0x{wi:04x} len={len(data)}")
        return

    dev, name = find_device()
    if dev is None:
        sys.exit("no live Apollo USB found (PID 0x000d / 0x0002 / 0x000f)")
    print(f"Found {name}: {dev.product}")
    if dev.is_kernel_driver_active(0):
        dev.detach_kernel_driver(0)
    usb.util.claim_interface(dev, 0)

    stats = collections.defaultdict(lambda: {"bulk": 0, "ctrl": 0, "resp": 0, "err": 0, "t": 0.0})
    cur = None
    t0 = time.time()
    phase_t0 = t0
    drain(dev, 200)
    try:
        for i, (delta, ph, kind, brt, breq, wv, wi, data) in enumerate(sel):
            if ph != cur:
                wait = min(delta, a.max_delay) if delta > 0 else 0.25
                if cur is not None:
                    stats[cur]["t"] = time.time() - phase_t0
                cur = ph
                print(f"--- phase {ph} (gap {wait:.2f} s)")
                time.sleep(wait)
                phase_t0 = time.time()
            elif delta > 0:
                time.sleep(min(delta, a.max_delay))

            if kind == 0:
                try:
                    dev.write(EP_BULK_OUT, data, timeout=5000)
                    stats[ph]["bulk"] += 1
                except usb.core.USBError as e:
                    stats[ph]["err"] += 1
                    print(f"    !! bulk write failed in phase {ph}: {e}")
                    raise
                # Large packets (program loads) and register reads draw replies
                is_read = data[0] and (struct.unpack_from("<I", data, 4)[0] & 0xFFFF) == 1
                if len(data) > 512 or is_read:
                    stats[ph]["resp"] += drain(dev, 200)
                elif i % 32 == 0:
                    stats[ph]["resp"] += drain(dev, 5)
            else:
                try:
                    dev.ctrl_transfer(brt, breq, wv, wi, data if data else None, 3000)
                    stats[ph]["ctrl"] += 1
                except usb.core.USBError as e:
                    stats[ph]["err"] += 1
                    print(f"    !! control bReq={breq} wValue=0x{wv:04x} failed: {e}")
                time.sleep(0.02)
                if breq in HANDSHAKE_REQUESTS:
                    got = drain(dev, 150)
                    stats[ph]["resp"] += got
                    print(f"    ctrl bReq={breq} wValue=0x{wv:04x} -> {got} reply")
    finally:
        if cur is not None:
            stats[cur]["t"] = time.time() - phase_t0
        stats[cur if cur is not None else 0]["resp"] += drain(dev, 300)
        usb.util.release_interface(dev, 0)
        try:
            dev.attach_kernel_driver(0)
        except Exception:
            pass

    total_resp = sum(s["resp"] for s in stats.values())
    total_err = sum(s["err"] for s in stats.values())
    print(f"done in {time.time() - t0:.1f} s: {total_resp} responses, {total_err} errors")
    print("PHASE   BULK  CTRL  RESP  ERR  SECONDS")
    for ph in sorted(stats):
        s = stats[ph]
        print(f"  {ph:3d} {s['bulk']:6d} {s['ctrl']:5d} {s['resp']:5d} {s['err']:4d} {s['t']:8.2f}")


if __name__ == "__main__":
    main()
