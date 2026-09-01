#!/usr/bin/env python3
"""Try to initialize Apollo Solo USB clock via UAC 2.0 requests.

The firmware is waiting for sample rate configuration before the DSP
activates. Try setting the clock frequency via standard UAC 2.0
Set Clock Frequency control requests on the audio control interface.

Usage: sudo python3 usb-clock-init.py
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


# UAC 2.0 constants
UAC2_CS_CUR = 0x01
UAC2_CS_RANGE = 0x02
CS_SAM_FREQ_CONTROL = 0x01

# Clock source ID from descriptor
CLOCK_ID = 128  # 0x80

# Audio control interface
AC_INTERFACE = 1

def probe():
    dev, model = find_device()
    if not dev:
        print("No live Apollo USB device found "
              "(expected one of: %s)" %
              ", ".join(LIVE_PIDS.values()))
        sys.exit(1)
    print(f"Found: {model} — {dev.product}\n")

    # Detach kernel driver from audio interfaces if attached
    for intf in [0, 1, 2, 3]:
        try:
            if dev.is_kernel_driver_active(intf):
                dev.detach_kernel_driver(intf)
                print(f"Detached kernel driver from interface {intf}")
        except:
            pass

    # Claim all interfaces
    for intf in [0, 1, 2, 3]:
        try:
            usb.util.claim_interface(dev, intf)
        except:
            pass

    # UAC 2.0 Get Clock Frequency (CUR)
    # bmRequestType: 0xA1 = device-to-host, class, interface
    # bRequest: 0x01 (CUR)
    # wValue: CS_SAM_FREQ_CONTROL << 8 | CN (0)
    # wIndex: clock_id << 8 | interface (for clock, just clock_id)
    # Actually for UAC2: wIndex = (entity_id << 8) | interface_number
    wValue = (CS_SAM_FREQ_CONTROL << 8) | 0
    wIndex = (CLOCK_ID << 8) | AC_INTERFACE

    print(f"=== UAC 2.0 Clock Control (clock ID={CLOCK_ID:#x}, interface={AC_INTERFACE}) ===")

    # Try GET_CUR sample rate
    print(f"\n-- GET_CUR sample rate (wValue={wValue:#06x} wIndex={wIndex:#06x}) --")
    try:
        data = dev.ctrl_transfer(0xA1, UAC2_CS_CUR, wValue, wIndex, 4, timeout=1000)
        freq = struct.unpack("<I", bytes(data))[0]
        print(f"  Current sample rate: {freq} Hz")
    except usb.core.USBTimeoutError:
        print("  GET_CUR: timeout (clock not running)")
    except usb.core.USBError as e:
        print(f"  GET_CUR: {e}")

    # Try GET_RANGE sample rates
    print("\n-- GET_RANGE sample rates --")
    try:
        data = dev.ctrl_transfer(0xA1, UAC2_CS_RANGE, wValue, wIndex, 64, timeout=1000)
        raw = bytes(data)
        num_ranges = struct.unpack_from("<H", raw, 0)[0]
        print(f"  Number of ranges: {num_ranges}")
        for i in range(min(num_ranges, 10)):
            offset = 2 + i * 12
            if offset + 12 <= len(raw):
                min_f, max_f, res = struct.unpack_from("<III", raw, offset)
                print(f"  Range {i}: {min_f}-{max_f} Hz (res={res})")
    except usb.core.USBTimeoutError:
        print("  GET_RANGE: timeout")
    except usb.core.USBError as e:
        print(f"  GET_RANGE: {e}")

    # Try SET_CUR sample rate to 48000 Hz
    print("\n-- SET_CUR sample rate to 48000 Hz --")
    freq_data = struct.pack("<I", 48000)
    try:
        dev.ctrl_transfer(0x21, UAC2_CS_CUR, wValue, wIndex, freq_data, timeout=2000)
        print("  SET_CUR 48000: ACCEPTED!")
        time.sleep(1)

        # Try reading back
        try:
            data = dev.ctrl_transfer(0xA1, UAC2_CS_CUR, wValue, wIndex, 4, timeout=1000)
            freq = struct.unpack("<I", bytes(data))[0]
            print(f"  Readback sample rate: {freq} Hz")
        except usb.core.USBError as e:
            print(f"  Readback: {e}")
    except usb.core.USBTimeoutError:
        print("  SET_CUR: timeout")
    except usb.core.USBError as e:
        print(f"  SET_CUR: {e}")

    # Try with interface 0 (DSP) in wIndex instead
    wIndex_dsp = (CLOCK_ID << 8) | 0
    print(f"\n-- Try clock control on interface 0 (wIndex={wIndex_dsp:#06x}) --")
    try:
        data = dev.ctrl_transfer(0xA1, UAC2_CS_CUR, wValue, wIndex_dsp, 4, timeout=500)
        freq = struct.unpack("<I", bytes(data))[0]
        print(f"  Sample rate: {freq} Hz")
    except usb.core.USBError as e:
        print(f"  {e}")

    # Check all endpoints after clock attempt
    print("\n=== Post-clock endpoint check ===")
    for ep, name in [(0x81, "BULK IN"), (0x86, "INTR IN"), (0x85, "AUDIO INTR")]:
        for i in range(3):
            try:
                data = dev.read(ep, 1024, timeout=500)
                print(f"  {name} [{len(data)}B]: {bytes(data)[:32].hex()}")
            except usb.core.USBTimeoutError:
                if i == 0:
                    print(f"  {name}: no data")
                break
            except usb.core.USBError as e:
                if i == 0:
                    print(f"  {name}: {e}")
                break

    # Check vendor request 0x02 (status) after clock attempt
    print("\n=== Status after clock attempt ===")
    try:
        data = dev.ctrl_transfer(0xC0, 0x02, 0, 0, 4, timeout=300)
        print(f"  Status: {bytes(data).hex()}")
    except:
        print("  Status: error")

    # Release
    for intf in [0, 1, 2, 3]:
        try:
            usb.util.release_interface(dev, intf)
        except:
            pass
    print("\nDone.")

if __name__ == "__main__":
    probe()
