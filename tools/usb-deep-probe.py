#!/usr/bin/env python3
"""Deep probe of Apollo Solo USB protocol.

Read all known vendor requests with full data, monitor interrupt endpoint,
and try to activate DSP communication.

Usage: sudo python3 usb-deep-probe.py
"""
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


def find_device():
    """Return (device, model_name) for whichever live Apollo USB is present."""
    for pid, name in LIVE_PIDS.items():
        dev = usb.core.find(idVendor=UA_VID, idProduct=pid)
        if dev:
            return dev, name
    return None, None


EP_BULK_OUT = 0x01
EP_BULK_IN = 0x81
EP_INTR_IN = 0x86

def hex_dump(data, prefix="    "):
    """Format hex dump with ASCII."""
    raw = bytes(data)
    for i in range(0, len(raw), 16):
        chunk = raw[i:i+16]
        hex_part = " ".join(f"{b:02x}" for b in chunk)
        ascii_part = "".join(chr(b) if 32 <= b < 127 else "." for b in chunk)
        print(f"{prefix}{i:04x}: {hex_part:<48s} {ascii_part}")

def probe():
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

    # Read all known active requests with max data
    known_reqs = {
        0x00: "protocol version",
        0x02: "status",
        0x04: "buffer/state",
        0x0A: "JKMK magic",
        0x0E: "config A",
        0x0F: "config B",
        0x10: "device info",
    }

    print("=== Known active vendor requests (full data) ===")
    for req, name in sorted(known_reqs.items()):
        try:
            data = dev.ctrl_transfer(0xC0, req, 0, 0, 512, timeout=500)
            print(f"\nreq={req:#04x} ({name}) [{len(data)}B]:")
            hex_dump(data)
        except usb.core.USBError as e:
            print(f"\nreq={req:#04x} ({name}): {e}")

    # Try request 0x0A with different wValue/wIndex
    print("\n=== Request 0x0A with varying wValue ===")
    for wval in [0, 1, 2, 3, 4, 0x100, 0x200]:
        try:
            data = dev.ctrl_transfer(0xC0, 0x0A, wval, 0, 64, timeout=300)
            preview = data.tobytes().hex()[:40]
            print(f"  wValue={wval:#06x}: [{len(data)}B] {preview}")
        except usb.core.USBError:
            print(f"  wValue={wval:#06x}: error")

    # Try request 0x10 with different wValue
    print("\n=== Request 0x10 with varying wValue ===")
    for wval in [0, 1, 2, 4, 8, 0x10, 0x1F, 0x41]:
        try:
            data = dev.ctrl_transfer(0xC0, 0x10, wval, 0, 64, timeout=300)
            words = struct.unpack_from("<4I", data.tobytes())
            print(f"  wValue={wval:#06x}: {' '.join(f'{w:08x}' for w in words)}")
        except usb.core.USBError:
            print(f"  wValue={wval:#06x}: error")

    # Drain interrupt endpoint
    print("\n=== Interrupt endpoint drain ===")
    for i in range(10):
        try:
            data = dev.read(EP_INTR_IN, 512, timeout=300)
            print(f"\nINT packet {i} [{len(data)}B]:")
            hex_dump(data)
        except usb.core.USBTimeoutError:
            print(f"  ({i} packets received)")
            break
        except usb.core.USBError as e:
            print(f"  INT error: {e}")
            break

    # Try sending known magic on bulk OUT
    print("\n=== Bulk OUT with magic headers ===")
    magics = [
        ("JFK header", b"JFK\x00" + b"\x00" * 60),
        ("JKMK header", b"JKMK" + b"\x00" * 60),
        ("JFK read cmd", b"JFK\x01\x00\x00\x00\x00" + struct.pack("<I", 0x380C) + b"\x00" * 52),
        ("JFK opcode 0", b"JFK\x00\x00\x00\x00\x00\x00\x00\x00\x00" + b"\x00" * 52),
        ("JFK opcode 1", b"JFK\x01\x01\x00\x00\x00\x00\x00\x00\x00" + b"\x00" * 52),
    ]
    for name, cmd in magics:
        try:
            dev.write(EP_BULK_OUT, cmd, timeout=500)
            # Check for response
            try:
                resp = dev.read(EP_BULK_IN, 1024, timeout=500)
                print(f"  {name}: RESPONSE [{len(resp)}B]")
                hex_dump(resp)
            except usb.core.USBTimeoutError:
                # Check interrupt too
                try:
                    resp = dev.read(EP_INTR_IN, 512, timeout=300)
                    print(f"  {name}: INT RESPONSE [{len(resp)}B]")
                    hex_dump(resp)
                except usb.core.USBTimeoutError:
                    print(f"  {name}: no response")
        except usb.core.USBError as e:
            print(f"  {name}: error: {e}")

    usb.util.release_interface(dev, 0)
    print("\nDone.")

if __name__ == "__main__":
    probe()
