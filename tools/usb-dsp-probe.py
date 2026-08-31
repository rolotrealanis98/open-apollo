#!/usr/bin/env python3
"""Probe Apollo Solo USB DSP with register-style commands.

The FX3 firmware bridges USB bulk transfers to FPGA register operations.
Try various header formats to find the correct protocol.

Usage: sudo python3 usb-dsp-probe.py
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

# Known Thunderbolt register offsets
REG_MIXER_SEQ_RD = 0x380C  # DSP readback sequence counter
REG_CLI_ENABLE = 0xC3F4
REG_CLI_STATUS = 0xC3F8

def try_cmd(dev, name, data, timeout=500):
    """Send command and try to read response."""
    try:
        dev.write(EP_BULK_OUT, data, timeout=timeout)
        try:
            resp = dev.read(EP_BULK_IN, 1024, timeout=timeout)
            hex_str = resp.tobytes().hex()
            # Show first 64 bytes max
            if len(hex_str) > 128:
                hex_str = hex_str[:128] + f"... ({len(resp)}B total)"
            print(f"  {name}: RESPONSE [{len(resp)}B] {hex_str}")
            return resp
        except usb.core.USBTimeoutError:
            print(f"  {name}: sent OK, no response")
    except usb.core.USBError as e:
        print(f"  {name}: send error: {e}")
    return None

def probe():
    dev, model = find_device()
    if not dev:
        print("No live Apollo USB device found "
              "(expected one of: %s)" %
              ", ".join(LIVE_PIDS.values()))
        sys.exit(1)
    print(f"Found: {model} — {dev.product}")

    if dev.is_kernel_driver_active(0):
        dev.detach_kernel_driver(0)
    usb.util.claim_interface(dev, 0)

    # Check interrupt endpoint first (might have initial state)
    print("\n--- Checking interrupt endpoint ---")
    for i in range(3):
        try:
            data = dev.read(EP_INTR_IN, 32, timeout=300)
            print(f"  INT: {data.tobytes().hex()}")
        except usb.core.USBTimeoutError:
            if i == 0:
                print("  No interrupt data")
            break

    # Format 1: Raw 8-byte register read (offset, value=0)
    # Like struct ua_reg_io { uint32_t offset; uint32_t value; }
    print("\n--- Format 1: Raw offset+value (8B LE) ---")
    for reg_name, offset in [("SEQ_RD", REG_MIXER_SEQ_RD), ("CLI_EN", REG_CLI_ENABLE)]:
        cmd = struct.pack("<II", offset, 0)
        try_cmd(dev, f"read {reg_name} ({offset:#x})", cmd)

    # Format 2: Command type + offset + value (12B)
    # type: 0=read, 1=write
    print("\n--- Format 2: cmd_type(4B) + offset(4B) + value(4B) ---")
    for cmd_type in [0, 1, 2, 0x10, 0x11]:
        cmd = struct.pack("<III", cmd_type, REG_MIXER_SEQ_RD, 0)
        try_cmd(dev, f"type={cmd_type:#x} read SEQ_RD", cmd)

    # Format 3: Magic header + opcode + offset (like FPGA protocol)
    print("\n--- Format 3: Various magic headers ---")
    for magic in [0x55414432, 0x55414431, 0xCAFE, 0xDEAD, 0x0001, 0x4341]:
        cmd = struct.pack("<III", magic, 0, REG_MIXER_SEQ_RD)
        try_cmd(dev, f"magic={magic:#x}", cmd)

    # Format 4: Short commands (4 bytes)
    print("\n--- Format 4: Short 4-byte commands ---")
    for val in [0, 1, 2, 0x380C, 0xFFFFFFFF]:
        cmd = struct.pack("<I", val)
        try_cmd(dev, f"word={val:#x}", cmd)

    # Format 5: Longer block with UA signature
    print("\n--- Format 5: Longer blocks ---")
    # 64-byte block, first 4 bytes = length, then data
    block = struct.pack("<I", 8) + struct.pack("<II", 0, REG_MIXER_SEQ_RD) + bytes(52)
    try_cmd(dev, "len+offset block", block)

    # Try reading register 0 (base)
    cmd = struct.pack("<III", 0, 0, 0)
    try_cmd(dev, "all-zeros 12B", cmd)

    # Format 6: USB vendor control transfers (not bulk)
    print("\n--- Format 6: USB vendor control requests ---")
    for req in [0xA0, 0xA1, 0xB0, 0xB1, 0x01, 0x10, 0x11]:
        try:
            data = dev.ctrl_transfer(0xC0, req, 0, 0, 64, timeout=300)
            print(f"  ctrl req={req:#x}: {data.tobytes().hex()}")
        except usb.core.USBError as e:
            if "pipe" in str(e).lower() or "stall" in str(e).lower():
                pass  # Expected for invalid requests
            else:
                print(f"  ctrl req={req:#x}: {e}")

    usb.util.release_interface(dev, 0)
    print("\nDone.")

if __name__ == "__main__":
    probe()
