#!/usr/bin/env python3
"""Scan all USB vendor control requests on Apollo Solo USB.

Tests request codes 0x00-0xFF in both directions to find supported commands.

Usage: sudo python3 usb-ctrl-scan.py
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

    # Scan device-to-host vendor requests (reads)
    print("=== Device-to-Host vendor requests (0xC0) ===")
    for req in range(0x00, 0x100):
        try:
            data = dev.ctrl_transfer(0xC0, req, 0, 0, 64, timeout=200)
            hex_preview = data.tobytes().hex()[:32]
            print(f"  req={req:#04x}: [{len(data)}B] {hex_preview}...")
        except usb.core.USBTimeoutError:
            print(f"  req={req:#04x}: TIMEOUT")
        except usb.core.USBError:
            pass  # STALL = not supported, skip silently

    # Scan host-to-device vendor requests (writes) with empty payload
    print("\n=== Host-to-Device vendor requests (0x40) with empty data ===")
    for req in range(0x00, 0x100):
        try:
            dev.ctrl_transfer(0x40, req, 0, 0, b"", timeout=200)
            print(f"  req={req:#04x}: ACCEPTED")
        except usb.core.USBTimeoutError:
            print(f"  req={req:#04x}: TIMEOUT")
        except usb.core.USBError:
            pass

    # Check if any bulk data appeared after the scans
    print("\n=== Check bulk/interrupt after scan ===")
    for ep, name in [(0x81, "BULK IN"), (0x86, "INTR IN")]:
        for i in range(3):
            try:
                data = dev.read(ep, 1024, timeout=300)
                print(f"  {name}: [{len(data)}B] {data.tobytes().hex()[:64]}")
            except usb.core.USBTimeoutError:
                if i == 0:
                    print(f"  {name}: no data")
                break
            except usb.core.USBError as e:
                print(f"  {name}: {e}")
                break

    usb.util.release_interface(dev, 0)
    print("\nDone.")

if __name__ == "__main__":
    scan()
