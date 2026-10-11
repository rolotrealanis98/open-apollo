#!/usr/bin/env python3
"""Scan vendor request 0x0A (read-only).

Request 0x0A returns JKMK-prefixed data indexed by wValue: magic
0x4B4D4B4A, the wValue echoed as two words, then nine data words (the
same "JK" magic family as request 0x06, 0x2A2B4B4A, and 0x08, 0x3B3C4B4A).
The Windows UA driver never sends 0x0A.  The write probes this tool used to
make (0x0A, 0x01, 0x05, 0x06, 0x08, 0x09 host-to-device) are gone: 0x05 is
the sample-rate index and 0x09 an unknown command, and a blind write can be
a firmware, EEPROM or flash write.

Usage: sudo python3 usb-0a-scan.py
"""
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


def find_device():
    """Return (device, model_name) for whichever live Apollo USB is present."""
    for pid, name in LIVE_PIDS.items():
        dev = usb.core.find(idVendor=UA_VID, idProduct=pid)
        if dev:
            return dev, name
    return None, None


def scan():
    dev, model = find_device()
    if not dev:
        print("No live Apollo USB device found "
              "(expected one of: %s)" %
              ", ".join(LIVE_PIDS.values()))
        sys.exit(1)
    print(f"Found: {model} — {dev.product}\n")

    if dev.is_kernel_driver_active(0):
        dev.detach_kernel_driver(0)
    usb.util.claim_interface(dev, 0)

    # Scan 0x0A reads: wValue low byte 0-255
    print("=== Request 0x0A scan (wValue low=0..31) ===")
    for lo in range(32):
        try:
            data = dev.ctrl_transfer(0xC0, 0x0A, lo, 0, 48, timeout=200)
            raw = bytes(data)
            # Skip the JKMK header (4 bytes) and the echoed index (8 bytes)
            payload = raw[12:]
            if any(b != 0 for b in payload):
                words = struct.unpack_from("<9I", raw, 0)
                word_str = " ".join(f"{w:08x}" for w in words[3:])
                print(f"  [{lo:3d}]: {word_str}")
        except usb.core.USBError:
            print(f"  [{lo:3d}]: error/stall")

    # Scan with wValue high byte (wIndex)
    print("\n=== Request 0x0A scan (wValue high=0..15, low=0) ===")
    for hi in range(16):
        try:
            data = dev.ctrl_transfer(0xC0, 0x0A, hi << 8, 0, 48, timeout=200)
            raw = bytes(data)
            payload = raw[12:]
            words = struct.unpack_from("<9I", raw, 0)
            hi_echo = words[1]
            lo_echo = words[2]
            word_str = " ".join(f"{w:08x}" for w in words[3:])
            has_data = any(b != 0 for b in payload)
            marker = " ***" if has_data else ""
            print(f"  [hi={hi:2d}]: echoed=({hi_echo:#x},{lo_echo:#x}) data={word_str}{marker}")
        except usb.core.USBError:
            print(f"  [hi={hi:2d}]: error")

    # No write probes: see the module docstring.

    # Check if anything appeared on the endpoints after the reads
    print("\n=== Endpoint check ===")
    for ep, name in [(0x81, "BULK IN"), (0x86, "INTR IN")]:
        for i in range(3):
            try:
                data = dev.read(ep, 1024, timeout=300)
                print(f"  {name} [{len(data)}B]: {bytes(data)[:32].hex()}")
            except usb.core.USBTimeoutError:
                if i == 0:
                    print(f"  {name}: no data")
                break

    usb.util.release_interface(dev, 0)
    print("\nDone.")

if __name__ == "__main__":
    scan()
