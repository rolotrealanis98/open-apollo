#!/usr/bin/env python3
"""Initialize DSP on UA Apollo USB devices.

Sends the DSP init command via bulk endpoint, sets the UAC2 clock
frequency, and configures a safe default monitor level so audio routes
to physical outputs.

Must run after firmware is loaded (device at PID 000d/0002/000f).
Can be called standalone or from udev via ua-usb-dsp-init.sh.

Usage: sudo python3 usb-dsp-init.py
"""
import struct
import sys
import usb.core
import usb.util

UA_VID = 0x2B5A
# Post-firmware PIDs
LIVE_PIDS = {
    0x000D: "Apollo Solo USB",
    0x0002: "Twin USB",
    0x000F: "Twin X USB",
}

EP_BULK_OUT = 0x01
EP_BULK_IN = 0x81

# DSP protocol constants
MAGIC_CMD = 0xDC
MAGIC_RSP = 0xDD

# Mixer settings FPGA addresses (vendor request 0x03)
SETTINGS_SEQ = 0x0602
SETTINGS_MASK = 0x062D
SETTING_MONITOR = 2  # offset in 128-byte mask buffer


def find_device():
    """Find any UA USB device in post-firmware state."""
    for pid, name in LIVE_PIDS.items():
        dev = usb.core.find(idVendor=UA_VID, idProduct=pid)
        if dev:
            return dev, name
    return None, None


def dsp_init(dev):
    """Send DSP init command: register 0x23=1 (activate FPGA)."""
    cmd = struct.pack("<HBB", 4, 0, MAGIC_CMD)
    cmd += struct.pack("<HHI", 0x0002, 0x0023, 0x00000001)
    cmd += struct.pack("<HHI", 0x0002, 0x0010, 0x01B71448)
    dev.write(EP_BULK_OUT, cmd, timeout=1000)

    # Drain response packets
    try:
        while True:
            dev.read(EP_BULK_IN, 1024, timeout=500)
    except usb.core.USBTimeoutError:
        pass


def set_clock(dev, rate=48000):
    """Set UAC2 clock frequency via SET_CUR."""
    try:
        dev.ctrl_transfer(0x21, 0x01, 0x0100, 0x8001,
                          struct.pack("<I", rate), timeout=2000)
    except usb.core.USBError:
        pass  # May timeout on first set, that's OK

    # Verify
    try:
        data = dev.ctrl_transfer(0xA1, 0x01, 0x0100, 0x8001, 4, timeout=1000)
        return struct.unpack("<I", bytes(data))[0]
    except usb.core.USBError:
        return 0


def set_monitor_level(dev, db=-12):
    """Set monitor output level via vendor control request 0x03.

    Without this, the FPGA boots with all mixer settings muted (0x80000000)
    and no audio reaches the physical outputs.
    Uses the same batch-write protocol as usb-mixer-test.py:
      1. Write 128-byte mask buffer to SETTINGS_MASK (0x062D)
      2. Bump sequence counter at SETTINGS_SEQ (0x0602)
    """
    raw = max(0, min(0xC0, int(192 + db * 2)))
    mask_buf = bytearray(128)
    # setting_word: (changed_mask << 16) | value
    word = (0x00FF << 16) | raw
    struct.pack_into("<I", mask_buf, SETTING_MONITOR * 8, word)

    try:
        dev.ctrl_transfer(0x41, 0x03, SETTINGS_MASK, 0,
                          bytes(mask_buf), timeout=1000)
        dev.ctrl_transfer(0x41, 0x03, SETTINGS_SEQ, 0,
                          struct.pack("<I", 1), timeout=1000)
    except usb.core.USBError:
        return False
    return True


def main():
    dev, name = find_device()
    if not dev:
        print("No UA USB device found (post-firmware)")
        sys.exit(1)

    print("Found: {}".format(name))

    # Detach kernel drivers from DSP + audio control interfaces
    for intf in [0, 1]:
        try:
            if dev.is_kernel_driver_active(intf):
                dev.detach_kernel_driver(intf)
        except Exception:
            pass

    usb.util.claim_interface(dev, 0)

    # Step 1: DSP init (activate FPGA)
    dsp_init(dev)
    print("DSP init done")

    # Step 2: Set clock
    freq = set_clock(dev, 48000)
    print("Clock: {} Hz".format(freq))

    usb.util.release_interface(dev, 0)

    # Step 3: Set monitor level to -12 dB (safe default)
    # Uses EP0 vendor control — no interface claim needed
    if set_monitor_level(dev, -12):
        print("Monitor: -12 dB")
    else:
        print("Monitor level set failed (non-fatal)")

    print("Ready")


if __name__ == "__main__":
    main()
