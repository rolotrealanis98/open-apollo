#!/usr/bin/env python3
"""Probe Apollo Solo USB DSP interface endpoints.

Reads from bulk IN (EP1) and interrupt (EP6) endpoints to see what
the device sends at idle. Also tries sending a simple status query.

Usage: sudo python3 usb-probe.py
"""
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


# DSP interface endpoints
EP_BULK_OUT = 0x01  # EP1 OUT - commands to device
EP_BULK_IN = 0x81   # EP1 IN  - responses from device
EP_INTR_IN = 0x86   # EP6 IN  - notifications from device

def probe():
    dev, model = find_device()
    if not dev:
        print("No live Apollo USB device found "
              "(expected one of: %s)" %
              ", ".join(LIVE_PIDS.values()))
        sys.exit(1)

    print(f"Found: {model} — {dev.product} (serial: {dev.serial_number})")
    print(f"Bus {dev.bus} Device {dev.address}")

    # Detach kernel driver from interface 0 (DSP)
    if dev.is_kernel_driver_active(0):
        dev.detach_kernel_driver(0)
        print("Detached kernel driver from interface 0")

    # Claim interface 0
    usb.util.claim_interface(dev, 0)
    print("Claimed DSP interface 0")

    # Try reading interrupt endpoint (notifications)
    print("\n--- Reading interrupt EP6 IN (notifications) ---")
    for i in range(5):
        try:
            data = dev.read(EP_INTR_IN, 32, timeout=500)
            print(f"  INT [{len(data)}B]: {data.tobytes().hex()}")
        except usb.core.USBTimeoutError:
            print(f"  INT: timeout ({i+1}/5)")
        except usb.core.USBError as e:
            print(f"  INT error: {e}")
            break

    # Try reading bulk IN (responses)
    print("\n--- Reading bulk EP1 IN (responses) ---")
    for i in range(3):
        try:
            data = dev.read(EP_BULK_IN, 1024, timeout=500)
            print(f"  BULK IN [{len(data)}B]: {data.tobytes().hex()}")
        except usb.core.USBTimeoutError:
            print(f"  BULK IN: timeout ({i+1}/3)")
        except usb.core.USBError as e:
            print(f"  BULK IN error: {e}")
            break

    # Try sending some probe commands on bulk OUT
    # These are guesses based on the Thunderbolt protocol
    probe_cmds = [
        ("Zero query", bytes(64)),  # all zeros
        ("0x01 cmd", bytes([0x01]) + bytes(63)),  # simple command byte
    ]

    print("\n--- Sending probe commands on bulk EP1 OUT ---")
    for name, cmd in probe_cmds:
        try:
            written = dev.write(EP_BULK_OUT, cmd, timeout=1000)
            print(f"  SENT {name}: {written}B written")
            # Read response
            try:
                data = dev.read(EP_BULK_IN, 1024, timeout=1000)
                print(f"  RESP [{len(data)}B]: {data.tobytes().hex()}")
            except usb.core.USBTimeoutError:
                print(f"  RESP: timeout")
        except usb.core.USBError as e:
            print(f"  SEND error ({name}): {e}")

    # Release
    usb.util.release_interface(dev, 0)
    print("\nDone.")

if __name__ == "__main__":
    probe()
