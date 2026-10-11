#!/usr/bin/env python3
"""Print the events /dev/ua_apolloN delivers (struct ua_event, 16 bytes).

    ua-events.py [/dev/ua_apollo0] [--count N] [--timeout SECONDS]

Stdlib only.  Exits when the device goes offline (read fails with ENODEV
after the DEVICE_OFFLINE event), after --count events, or on Ctrl-C.
"""
import argparse
import errno
import os
import select
import struct
import sys
import time

EVENT = struct.Struct("=IIQ")  # code, arg, time_ns (CLOCK_MONOTONIC)

NAMES = {
    0x64: "RATE_INFO",
    0x65: "IODESC_IN",
    0x66: "IODESC_OUT",
    0x67: "RATE_CHANGED",
    0x68: "CLOCK_CHANGED",
    0x6E: "NOTIF_STATUS",
    0x1000: "DEVICE_OFFLINE",
    0x1001: "CONNECTED",
    0x1002: "LAYOUT_CHANGED",
    0x1003: "EVENTS_LOST",
}
RATES = {1: 44100, 2: 48000, 3: 88200, 4: 96000, 5: 176400, 6: 192000}


def describe(code, arg):
    if code == 0x67:
        return "rate index %d (%s Hz)" % (arg, RATES.get(arg, "?"))
    if code == 0x68:
        return "source %d" % arg
    if code in (0x64, 0x65, 0x66, 0x6E):
        return "status 0x%08x" % arg
    if code == 0x1000:
        return "surprise removal" if arg else "unbind"
    if code == 0x1002:
        return "play %d ch, rec %d ch" % (arg >> 16, arg & 0xFFFF)
    if code == 0x1003:
        return "%d events dropped" % arg
    return "arg 0x%x" % arg


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("node", nargs="?", default="/dev/ua_apollo0")
    ap.add_argument("--count", type=int, default=0, help="stop after N events")
    ap.add_argument("--timeout", type=float, default=0,
                    help="stop after this many seconds")
    args = ap.parse_args()

    fd = os.open(args.node, os.O_RDONLY | os.O_NONBLOCK)
    t0 = None
    seen = 0
    deadline = time.monotonic() + args.timeout if args.timeout else None
    try:
        while True:
            wait = None
            if deadline:
                wait = deadline - time.monotonic()
                if wait <= 0:
                    return 0
            select.select([fd], [], [], wait)
            try:
                data = os.read(fd, EVENT.size * 32)
            except BlockingIOError:
                continue
            except OSError as e:
                if e.errno == errno.ENODEV:
                    print("device gone (ENODEV)")
                    return 0
                raise
            for off in range(0, len(data) - EVENT.size + 1, EVENT.size):
                code, arg, ns = EVENT.unpack_from(data, off)
                if t0 is None:
                    t0 = ns
                print("+%9.3f s  0x%04x %-15s %s" % (
                    (ns - t0) / 1e9, code,
                    NAMES.get(code, "UNKNOWN"), describe(code, arg)))
                sys.stdout.flush()
                seen += 1
                if args.count and seen >= args.count:
                    return 0
    except KeyboardInterrupt:
        return 0
    finally:
        os.close(fd)


if __name__ == "__main__":
    sys.exit(main())
