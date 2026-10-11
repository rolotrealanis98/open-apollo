#!/usr/bin/env python3
"""Probe Apollo USB registers via vendor control transfers (read-only by default).

Vendor request map, derived from the Windows UAD2Usb.sys (parity/05-windows.md
section 3.3) and the Solo USB captures; the decoders are in uausb/device.py.
Every request is bmRequestType 0x41 (OUT) / 0xC1 (IN), recipient = interface,
wIndex = 0, and wValue carries the argument:

  0x03 OUT  register write; wValue = FPGA word address, payload = 1 or N dwords
  0x04 IN   register read;  wValue = FPGA word address, length 4 or 4n
  0x10 IN   readback block: the 40-dword (160-byte) mixer readback, wValue = 0
  0x00 IN   4-byte protocol word (>= 0x00010002: readback by 0x10)
  0x0E/0x0F IN  4-byte version words
  0x06 IN   36-byte status block {magic, rate index, rate, clock sources,
            record channels, playback channels}
  0x08 IN   44-byte clock source entry wValue {magic, index, code, name}

FPGA word space:
  0x0602  SEQ_WR  (write here to bump the sequence and latch the settings)
  0x0603  SEQ_RD  (DSP acknowledges here)
  0x0604  readback status (old protocol), 0x0605.. readback data
  0x062D  settings 0-15, 0x064F settings 16-31, 0x0670 settings 32-37
  0x1000  DSP 0 register window (PCIe BAR0 0x0000 one dword per address):
          +0x04.. pool sizes, +0x08.. serial, +0x61.. pool bases,
          +0x65.. scratch, +0x69 boot word (0 / 0xF81A3C5E = not started)

Default mode only reads.  Writing needs --write ADDR=VALUE together with
--allow-write, and only reaches the settings words, SEQ_WR and the readback
status word: a write to the mixer block can change monitor, preamp or
routing state on a live unit, so only use it on purpose.

Usage:
  sudo python3 usb-reg-probe.py                     # read-only survey
  sudo python3 usb-reg-probe.py --read 0x0603       # one register
  sudo python3 usb-reg-probe.py --readback          # 40-dword block (0x10)
  sudo python3 usb-reg-probe.py --write 0x0602=1 --allow-write
"""
import argparse
import os
import sys

_script_dir = os.path.dirname(os.path.abspath(__file__))
if _script_dir not in sys.path:
    sys.path.insert(0, _script_dir)

from uausb import device as udev  # noqa: E402

UA_VID = 0x2B5A
LIVE_PIDS = {
    0x000D: "Apollo Solo USB",
    0x0002: "Twin USB",
    0x000F: "Twin X USB",
}

REGS = {
    udev.MIXER_BLOCK: "MIXER_BLOCK",
    udev.SEQ_WR: "SEQ_WR",
    udev.SEQ_RD: "SEQ_RD",
    udev.READBACK_STATUS: "READBACK_STATUS",
    udev.ID_WORD: "ID (0x0C00)",
    udev.ID_VERSION: "ID version (0x0C01)",
}


def find_device(usb):
    """Return (device, model_name) for whichever live Apollo USB is present."""
    for pid, name in LIVE_PIDS.items():
        dev = usb.core.find(idVendor=UA_VID, idProduct=pid)
        if dev:
            return dev, name
    return None, None


def dump_words(words):
    return " ".join(f"{w:08x}" for w in words)


def readback(regs, protocol=None):
    print("\n=== Readback block (40 dwords) ===")
    try:
        status, words = regs.readback(protocol)
    except Exception as e:  # noqa: BLE001
        print(f"  error: {e}")
        return
    if not words:
        print("  no snapshot (old protocol, status word not 1)")
        return
    for i in range(0, len(words), 4):
        print(f"  word {i:2d}: {dump_words(words[i:i + 4])}")
    print("  check words: " + ("ok (word 38 = XOR 0-37, word 39 = ~word 38)"
                               if status == 1 else "BAD"))


def survey(regs):
    print("=== Mixer / ID registers (request 0x04) ===")
    for addr, name in sorted(REGS.items()):
        try:
            print(f"  [{addr:#06x}] {name:20s}: {regs.read1(addr):08x}")
        except Exception as e:  # noqa: BLE001
            print(f"  [{addr:#06x}] {name:20s}: {e}")

    print("\n=== Info requests (read-only) ===")
    protocol = None
    try:
        v = regs.versions()
        protocol = v["protocol"]
        print(f"  protocol (0x00) {v['protocol']:08x}  version 0x0E "
              f"{v['version_a']:08x}  version 0x0F {v['version_b']:08x}")
    except Exception as e:  # noqa: BLE001
        print(f"  versions: {e}")
    try:
        st = regs.status_block()
        print(f"  status (0x06): {st}" if st else "  status (0x06): bad magic")
    except Exception as e:  # noqa: BLE001
        print(f"  status (0x06): {e}")
    try:
        menu = regs.clock_menu()
        print("  clock menu (0x08): " + (", ".join(
            f"{name} ({code:#04x})" for code, name in menu) or "empty"))
    except Exception as e:  # noqa: BLE001
        print(f"  clock menu (0x08): {e}")

    print("\n=== DSP 0 window (0x1000) ===")
    try:
        boot = regs.window_word(0, udev.WIN_BOOT_WORD)
        started = boot not in (0, udev.BOOT_NOT_STARTED)
        print(f"  boot word (0x1069): {boot:08x} "
              f"({'started' if started else 'NOT started'}"
              f"{', bit 0 clear' if started and not boot & 1 else ''})")
        print(f"  status (0x1000):    {regs.window_word(0, udev.WIN_STATUS):08x}")
        mm = regs.memory_map(0)
        print("  pool sizes " + dump_words(mm["sizes"]) + "  bases "
              + dump_words(mm["bases"]) + "  scratch " + dump_words(mm["scratch"]))
        print(f"  serial: {regs.serial(0)!r}")
    except Exception as e:  # noqa: BLE001
        print(f"  error: {e}")

    readback(regs, protocol)


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

    import usb.core
    import usb.util

    dev, model = find_device(usb)
    if not dev:
        print("No live Apollo USB device found (expected one of: %s)" %
              ", ".join(LIVE_PIDS.values()))
        sys.exit(1)
    print(f"Found: {model} - {dev.product}\n")

    if dev.is_kernel_driver_active(0):
        dev.detach_kernel_driver(0)
    usb.util.claim_interface(dev, 0)
    regs = udev.UsbRegs(dev)
    try:
        if args.write:
            print("WARNING: writing FPGA registers on a live device. A bad write"
                  " to the mixer block can mute or mis-route the unit.\n",
                  file=sys.stderr)
            for spec in args.write:
                addr_s, _, val_s = spec.partition("=")
                addr, val = int(addr_s, 0), int(val_s, 0)
                regs.write(addr, [val])
                print(f"  wrote [{addr:#06x}] = {val:#010x}")
        for a in args.read:
            addr = int(a, 0)
            print(f"  [{addr:#06x}]: {regs.read1(addr):08x}")
        if args.readback:
            readback(regs)
        if not (args.read or args.readback or args.write):
            survey(regs)
    finally:
        usb.util.release_interface(dev, 0)
    print("\nDone.")


if __name__ == "__main__":
    main()
