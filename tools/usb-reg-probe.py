#!/usr/bin/env python3
"""Probe Apollo USB registers via vendor control transfers (read-only by default).

Vendor request map, derived from the Windows UAD2Usb.sys (parity/05-windows.md
section 3.3). Every request is bmRequestType 0x41 (OUT) / 0xC1 (IN), recipient
= interface, wIndex = 0, and wValue carries the argument:

  0x03 OUT  register write; wValue = FPGA word address, payload = 1 or N dwords
  0x04 IN   register read;  wValue = FPGA word address, length 4 or 4n
  0x10 IN   readback block: the 40-dword (160-byte) mixer readback, wValue = 0
  0x00 IN   4-byte run-state word        0x0E/0x0F IN  4-byte version/ID words
  0x06 IN   36-byte device/firmware info block

Mixer block in FPGA word space:
  0x0600  mixer settings block (block write via 0x03)
  0x0602  SEQ_WR  (write here to bump the sequence and latch the settings)
  0x0603  SEQ_RD  (DSP acknowledges here)
  0x0604  readback status

The old PCIe-style offsets (0x3800 MIXER_BASE, 0xC3F4 CLI_ENABLE, ...) are BAR0
addresses and do not exist in this FPGA address space; the earlier version of
this tool also treated 0x10 as "read register", which was wrong.

Default mode only reads. Writing needs --write ADDR=VALUE together with
--allow-write; a write to the mixer block can change monitor, preamp or routing
state on a live unit, so only use it on purpose.

Usage:
  sudo python3 usb-reg-probe.py                     # read-only survey
  sudo python3 usb-reg-probe.py --read 0x0603       # one register
  sudo python3 usb-reg-probe.py --readback          # 40-dword block (0x10)
  sudo python3 usb-reg-probe.py --write 0x0602=1 --allow-write
"""
import argparse
import struct
import sys

import usb.core
import usb.util

UA_VID = 0x2B5A
LIVE_PIDS = {
    0x000D: "Apollo Solo USB",
    0x0002: "Twin USB",
    0x000F: "Twin X USB",
}

REQ_OUT = 0x41
REQ_IN = 0xC1

REQ_STATUS = 0x00
REQ_WRITE_REG = 0x03
REQ_READ_REG = 0x04
REQ_FW_INFO = 0x06
REQ_READBACK = 0x10

# FPGA word addresses (Windows-derived, see docstring)
REGS = {
    0x0600: "MIXER_BLOCK",
    0x0602: "SEQ_WR",
    0x0603: "SEQ_RD",
    0x0604: "READBACK_STATUS",
}

# Read-only no-argument requests: (request, length, name)
INFO_REQUESTS = [
    (REQ_STATUS, 4, "run-state word"),
    (0x0E, 4, "version/ID word A"),
    (0x0F, 4, "version/ID word B"),
    (REQ_FW_INFO, 36, "device/firmware info"),
]


def find_device():
    """Return (device, model_name) for whichever live Apollo USB is present."""
    for pid, name in LIVE_PIDS.items():
        dev = usb.core.find(idVendor=UA_VID, idProduct=pid)
        if dev:
            return dev, name
    return None, None


def read_reg(dev, addr, length=4):
    data = dev.ctrl_transfer(REQ_IN, REQ_READ_REG, addr, 0, length, timeout=500)
    return data.tobytes()


def write_reg(dev, addr, value):
    dev.ctrl_transfer(REQ_OUT, REQ_WRITE_REG, addr, 0,
                      struct.pack("<I", value), timeout=500)


def dump_words(raw):
    n = len(raw) // 4
    return " ".join(f"{w:08x}" for w in struct.unpack_from(f"<{n}I", raw))


def readback(dev):
    print("\n=== Readback block (request 0x10, 40 dwords) ===")
    try:
        data = dev.ctrl_transfer(REQ_IN, REQ_READBACK, 0, 0, 160, timeout=500)
    except usb.core.USBError as e:
        print(f"  error: {e}")
        return
    raw = data.tobytes()
    for i in range(0, len(raw) // 4, 4):
        print(f"  word {i:2d}: {dump_words(raw[i * 4:i * 4 + 16])}")


def survey(dev):
    print("=== Mixer registers (request 0x04, wValue = FPGA word address) ===")
    for addr, name in sorted(REGS.items()):
        try:
            print(f"  [{addr:#06x}] {name:16s}: {dump_words(read_reg(dev, addr))}")
        except usb.core.USBError as e:
            print(f"  [{addr:#06x}] {name:16s}: {e}")

    print("\n=== Info requests (read-only) ===")
    for req, length, name in INFO_REQUESTS:
        try:
            data = dev.ctrl_transfer(REQ_IN, req, 0, 0, length, timeout=500)
            print(f"  req={req:#04x} ({name:20s}): {dump_words(data.tobytes())}")
        except usb.core.USBError as e:
            print(f"  req={req:#04x} ({name:20s}): {e}")

    readback(dev)


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--read", metavar="ADDR", action="append", default=[],
                    help="read one FPGA word address (request 0x04); repeatable")
    ap.add_argument("--readback", action="store_true",
                    help="only fetch the 40-dword readback block (request 0x10)")
    ap.add_argument("--write", metavar="ADDR=VALUE", action="append", default=[],
                    help="write a dword (request 0x03); needs --allow-write")
    ap.add_argument("--allow-write", action="store_true",
                    help="permit --write (changes live device state)")
    args = ap.parse_args()

    if args.write and not args.allow_write:
        ap.error("--write changes device state; add --allow-write to confirm")

    dev, model = find_device()
    if not dev:
        print("No live Apollo USB device found (expected one of: %s)" %
              ", ".join(LIVE_PIDS.values()))
        sys.exit(1)
    print(f"Found: {model} - {dev.product}\n")

    if dev.is_kernel_driver_active(0):
        dev.detach_kernel_driver(0)
    usb.util.claim_interface(dev, 0)
    try:
        if args.write:
            print("WARNING: writing FPGA registers on a live device. A bad write"
                  " to the mixer block can mute or mis-route the unit.\n",
                  file=sys.stderr)
            for spec in args.write:
                addr_s, _, val_s = spec.partition("=")
                addr, val = int(addr_s, 0), int(val_s, 0)
                write_reg(dev, addr, val)
                print(f"  wrote [{addr:#06x}] = {val:#010x}")
        for a in args.read:
            addr = int(a, 0)
            print(f"  [{addr:#06x}]: {dump_words(read_reg(dev, addr))}")
        if args.readback:
            readback(dev)
        if not (args.read or args.readback or args.write):
            survey(dev)
    finally:
        usb.util.release_interface(dev, 0)
    print("\nDone.")


if __name__ == "__main__":
    main()
