#!/usr/bin/env python3
"""
read-io-descriptors.py — dump the Apollo IO descriptor tables.

Firmware populates two 72-word regions after connect: the input IO descriptor
at 0xC1A4 and the output one at 0xC2C4.  They are the DMA channel order — the
data ua_get_routing_config() needs, and what it currently lacks for anything
other than the x4.

Read-only.  Uses the driver's READ_REG ioctl, which is bounds-checked and
issues no writes, so this needs no rebuild, no module reload and no debugfs
(the last matters: with Secure Boot on, the kernel is in integrity lockdown
and /sys/kernel/debug is refused even to root).

  python3 tools/read-io-descriptors.py
  python3 tools/read-io-descriptors.py --json > x8p-io-desc.json

The driver creates /dev/ua_apollo0 world-accessible, so this needs no root on
a default install; prefix with sudo only if the node has been tightened.

Layout, as observed on an Apollo x8p (device_type 0x20, FPGA 0xa241c5ac):

  w0        version, 1
  w1        capacity, 70 entries
  w2        0xffffffff
  w3        channel count  (input block)
  w4        channel count  (output block)
  w5        flags, 2 on input / 0 on output
  w6..      u16 channel IDs in DMA order, low half-word first
            0x00ff pads the unused tail

The channel ID high byte is a group and the low byte a 1-based index within
it, so 0x0403 is ADAT 3.  Group names below are those the device reports in
its own routing table (macOS SEL171); groups seen in the descriptor but absent
from that table are left unnamed rather than guessed at.
"""
import argparse
import fcntl
import json
import struct
import sys

DEV = "/dev/ua_apollo0"

UA_IOCTL_MAGIC = ord('U')


def _IOC(direction, typ, nr, size):
    return (direction << 30) | (size << 16) | (typ << 8) | nr


# _IOWR('U', 0x10, struct ua_reg_io) — two u32, offset + value
UA_IOCTL_READ_REG = _IOC(3, UA_IOCTL_MAGIC, 0x10, 8)

# Register offsets from driver/ua_apollo.h
UA_REG_IN_NAMES_BASE = 0xC1A4
UA_REG_OUT_NAMES_BASE = 0xC2C4
UA_IO_DESC_WORDS = 72

PAD = 0x00FF          # unused-entry marker
ENTRY_START_WORD = 6  # channel IDs begin here

# Word holding the channel count differs per direction; this is why the driver
# logs in_buf[3] but out_buf[4].
COUNT_WORD = {"input": 3, "output": 4}

# Group -> (label, style).  Styles reproduce the naming the device itself
# reports in its routing table, so output here can be matched against it:
#   "plain"  -> "ADAT 3"            index used directly
#   "pair"   -> "MON L" / "MON R"   a single stereo pair
#   "pairs"  -> "AUX1 L", "AUX2 R"  numbered stereo pairs
GROUPS = {
    0x00: ("MIC/LINE/HIZ", "plain"),
    0x01: ("MIC/LINE", "plain"),
    0x02: ("AUX", "pairs"),
    0x03: ("LINE", "plain"),
    0x04: ("ADAT", "plain"),
    0x07: ("VIRTUAL", "plain"),
    0x09: ("MON", "pair"),
    0x0A: ("CUE", "pairs"),
    0x0B: ("TALKBACK", "plain"),
}


def read_reg(fd, offset):
    payload = struct.pack("II", offset, 0)
    result = fcntl.ioctl(fd, UA_IOCTL_READ_REG, payload)
    return struct.unpack("II", result)[1]


def read_block(fd, base):
    return [read_reg(fd, base + i * 4) for i in range(UA_IO_DESC_WORDS)]


def channel_ids(words, count):
    raw = struct.pack("<%dI" % len(words), *words)
    return list(struct.unpack_from("<%dH" % count, raw,
                                   ENTRY_START_WORD * 4))


def describe(cid):
    group, index = cid >> 8, cid & 0xFF
    entry = GROUPS.get(group)
    if entry is None:
        return "group 0x%02x index %d" % (group, index)

    label, style = entry
    if style == "pair":
        return "%s %s" % (label, "L" if index % 2 else "R")
    if style == "pairs":
        return "%s%d %s" % (label, (index + 1) // 2, "L" if index % 2 else "R")
    return "%s %d" % (label, index)


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--device", default=DEV, help="device node (default: %s)" % DEV)
    ap.add_argument("--json", action="store_true",
                    help="emit raw words and decoded IDs as JSON")
    args = ap.parse_args()

    try:
        fd = open(args.device, "rb", buffering=0)
    except PermissionError:
        sys.exit("%s: permission denied — run with sudo" % args.device)
    except FileNotFoundError:
        sys.exit("%s not found — is the Apollo powered on and ua_apollo loaded?"
                 % args.device)

    out = {}
    with fd:
        for direction, base in (("input", UA_REG_IN_NAMES_BASE),
                                ("output", UA_REG_OUT_NAMES_BASE)):
            words = read_block(fd, base)
            count = words[COUNT_WORD[direction]]

            if count > words[1]:
                print("warning: %s count %d exceeds capacity %d — "
                      "layout may differ on this model"
                      % (direction, count, words[1]), file=sys.stderr)
                count = min(count, UA_IO_DESC_WORDS * 2 - ENTRY_START_WORD * 2)

            ids = channel_ids(words, count)
            out[direction] = {
                "base": base,
                "version": words[0],
                "capacity": words[1],
                "count": count,
                "words": words,
                "channel_ids": ids,
            }

            if args.json:
                continue

            print("=== %s IO descriptor @ 0x%04X — version %d, %d of %d entries ==="
                  % (direction, base, words[0], count, words[1]))
            for i, cid in enumerate(ids):
                if cid == PAD:
                    continue
                print("  dma[%2d] -> 0x%04x  %s" % (i, cid, describe(cid)))
            print()

    if args.json:
        json.dump(out, sys.stdout, indent=2)
        sys.stdout.write("\n")


if __name__ == "__main__":
    main()
