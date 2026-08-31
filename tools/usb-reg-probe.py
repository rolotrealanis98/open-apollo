#!/usr/bin/env python3
"""Probe Apollo Solo USB registers via vendor control transfers.

The ioctl request numbers map to USB vendor bRequest codes:
  0x10 = read register, 0x11 = write register, etc.

Usage: sudo python3 usb-reg-probe.py
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


# Key register offsets from Thunderbolt driver
REGS = {
    0x0000: "BAR0 base",
    0x0004: "BAR0+4",
    0x0008: "BAR0+8",
    0x3800: "MIXER_BASE",
    0x3804: "MIXER+4",
    0x3808: "MIXER_SEQ_WR",
    0x380C: "MIXER_SEQ_RD",
    0xC3F4: "CLI_ENABLE",
    0xC3F8: "CLI_STATUS",
}

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

    # Test 1: Read register via ctrl request 0x10
    # Try passing offset in wValue, wIndex, and in data
    print("=== Vendor ctrl request 0x10 (read reg) ===")
    print("\n-- wValue=offset, wIndex=0, read 64 bytes --")
    for offset, name in sorted(REGS.items()):
        try:
            data = dev.ctrl_transfer(0xC0, 0x10, offset & 0xFFFF, offset >> 16, 64, timeout=500)
            hex_str = data.tobytes().hex()
            # Parse first few words as LE uint32
            words = struct.unpack_from("<8I", data.tobytes())
            word_str = " ".join(f"{w:08x}" for w in words)
            print(f"  [{offset:#06x}] {name:16s}: {word_str}")
        except usb.core.USBError as e:
            print(f"  [{offset:#06x}] {name:16s}: {e}")

    # Test 2: Try with offset as data payload (8-byte struct like ioctl)
    print("\n-- Send offset as payload (like ioctl struct) --")
    for offset, name in [(0x380C, "MIXER_SEQ_RD"), (0xC3F4, "CLI_ENABLE")]:
        payload = struct.pack("<II", offset, 0)
        try:
            data = dev.ctrl_transfer(0xC0, 0x10, 0, 0, 64, timeout=500)
            words = struct.unpack_from("<4I", data.tobytes())
            print(f"  base read: {' '.join(f'{w:08x}' for w in words)}")
        except usb.core.USBError as e:
            print(f"  {name}: {e}")
        # Try sending payload as OUT then reading
        try:
            dev.ctrl_transfer(0x40, 0x10, offset & 0xFFFF, offset >> 16, b"", timeout=500)
            data = dev.ctrl_transfer(0xC0, 0x10, offset & 0xFFFF, offset >> 16, 64, timeout=500)
            words = struct.unpack_from("<4I", data.tobytes())
            print(f"  [{offset:#06x}] OUT+IN: {' '.join(f'{w:08x}' for w in words)}")
        except usb.core.USBError as e:
            print(f"  [{offset:#06x}] OUT+IN: {e}")

    # Test 3: Try other ioctl-mapped requests
    print("\n=== Other vendor requests ===")
    for req, name in [(0x11, "WRITE_REG"), (0x30, "SET_BUS_PARAM"),
                       (0x31, "SET_MIXER_PARAM"), (0x34, "SET_DRIVER_PARAM"),
                       (0x35, "GET_DRIVER_PARAM"), (0x36, "WRITE_SETTING"),
                       (0x37, "READ_SETTING"), (0x40, "CLI_CMD")]:
        try:
            data = dev.ctrl_transfer(0xC0, req, 0, 0, 64, timeout=300)
            words = struct.unpack_from("<4I", data.tobytes())
            print(f"  req={req:#04x} ({name:16s}): {' '.join(f'{w:08x}' for w in words)}")
        except usb.core.USBTimeoutError:
            print(f"  req={req:#04x} ({name:16s}): timeout")
        except usb.core.USBError as e:
            if "pipe" in str(e).lower():
                print(f"  req={req:#04x} ({name:16s}): STALL (not supported)")
            else:
                print(f"  req={req:#04x} ({name:16s}): {e}")

    # Test 4: Read a range of registers from 0x3800
    print("\n=== Register scan 0x3800-0x3840 ===")
    for offset in range(0x3800, 0x3844, 4):
        try:
            data = dev.ctrl_transfer(0xC0, 0x10, offset & 0xFFFF, offset >> 16, 8, timeout=300)
            val = struct.unpack_from("<II", data.tobytes())
            print(f"  [{offset:#06x}]: {val[0]:08x} {val[1]:08x}")
        except usb.core.USBError:
            print(f"  [{offset:#06x}]: error")

    usb.util.release_interface(dev, 0)
    print("\nDone.")

if __name__ == "__main__":
    probe()
