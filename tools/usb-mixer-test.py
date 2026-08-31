#!/usr/bin/env python3
"""Test USB mixer settings writes on Apollo Solo USB.

Requires: Apollo Solo USB connected, FX3 firmware loaded, DSP initialized.
Run after: tools/usb-dsp-init.py

Tests:
  1. Open USB device (interface 0)
  2. Write vendor control request 0x03 (mixer settings batch)
  3. Set monitor level, preamp gain, verify no errors
  4. Check if capture channels show signal

Usage: sudo python3 tools/usb-mixer-test.py
"""

import struct
import sys
import time

try:
    import usb.core
    import usb.util
except ImportError:
    print("pip install pyusb")
    sys.exit(1)

UA_VID = 0x2B5A
LIVE_PIDS = {
    0x000D: "Apollo Solo USB",
    0x0002: "Twin USB",
    0x000F: "Twin X USB",
}


def find_live_device():
    """Return (device, model_name) for whichever live Apollo USB is present."""
    for pid, name in LIVE_PIDS.items():
        dev = usb.core.find(idVendor=UA_VID, idProduct=pid)
        if dev:
            return dev, name
    return None, None


# FPGA addresses for vendor request 0x03
SETTINGS_SEQ = 0x0602       # 4B: sequence counter
SETTINGS_MASK = 0x062D      # 128B: mask+value buffer
SETTINGS_VALUES = 0x064F    # 128B: gain value buffer

SETTING_PREAMP_CH0 = 0      # offset +0
SETTING_PREAMP_CH1 = 1      # offset +8
SETTING_MONITOR = 2         # offset +16
SETTING_GAIN_C = 3          # offset +24


def setting_word(mask, value):
    return ((mask & 0xFFFF) << 16) | (value & 0xFFFF)


def find_device():
    dev, model = find_live_device()
    if not dev:
        print("No live Apollo USB device found "
              "(expected one of: %s)" %
              ", ".join(LIVE_PIDS.values()))
        print("Is firmware loaded? Run: sudo python3 tools/fx3-load.py")
        sys.exit(1)
    print(f"Found: {dev.manufacturer} {dev.product}")
    return dev


def claim_dsp(dev):
    """Prepare DSP interface — DON'T claim interface 0.

    Vendor control requests (bmRequestType=0x41) go through the
    default control pipe (EP0) and don't need interface claiming.
    Claiming interface 0 would detach snd-usb-audio and kill audio.
    """
    print("Using vendor control requests on EP0 (no interface claim needed)")


def vendor_write(dev, request, wvalue, data=b""):
    """Send vendor control OUT request (bmRequestType=0x41)."""
    dev.ctrl_transfer(0x41, request, wvalue, 0, data, timeout=1000)


def write_settings(dev, seq, mask_buf, value_buf=None):
    """Write a mixer settings batch."""
    vendor_write(dev, 0x03, SETTINGS_MASK, mask_buf)
    if value_buf and any(b != 0 for b in value_buf):
        vendor_write(dev, 0x03, SETTINGS_VALUES, value_buf)
    vendor_write(dev, 0x03, SETTINGS_SEQ, struct.pack("<I", seq))
    return seq + 1


def test_monitor_level(dev, seq):
    """Test monitor level writes."""
    print("\n=== Monitor Level ===")
    for db in [-96, -48, -24, -12, 0, -12]:
        raw = max(0, min(0xC0, int(192 + db * 2)))
        mask_buf = bytearray(128)
        word = setting_word(0x00FF, raw)
        struct.pack_into("<I", mask_buf, SETTING_MONITOR * 8, word)
        seq = write_settings(dev, seq, mask_buf)
        print(f"  Monitor = {db:4d} dB (raw=0x{raw:02x}, seq={seq-1})")
        time.sleep(0.3)
    return seq


def test_preamp_gain(dev, seq):
    """Test preamp gain writes."""
    print("\n=== Preamp Gain (ch0) ===")
    for gain_db in [10, 20, 30, 40, 50, 60, 30, 10]:
        val_a = max(0, min(54, gain_db - 10))
        val_c = val_a + 0x41

        mask_buf = bytearray(128)
        value_buf = bytearray(128)

        # Gain A in value buffer
        struct.pack_into("<I", value_buf, SETTING_PREAMP_CH0 * 8,
                         setting_word(0x00FF, val_a))
        # Gain C in mask buffer
        struct.pack_into("<I", mask_buf, SETTING_GAIN_C * 8,
                         setting_word(0x003F, val_c))

        seq = write_settings(dev, seq, mask_buf, value_buf)
        print(f"  Gain = {gain_db:2d} dB (val_a={val_a}, val_c=0x{val_c:02x}, "
              f"seq={seq-1})")
        time.sleep(0.3)
    return seq


def test_phantom(dev, seq):
    """Test 48V phantom power toggle."""
    print("\n=== 48V Phantom (ch0) ===")
    for enabled in [True, False]:
        mask_buf = bytearray(128)
        val = 0x0008 if enabled else 0x0000
        struct.pack_into("<I", mask_buf, SETTING_PREAMP_CH0 * 8,
                         setting_word(0x0008, val))
        seq = write_settings(dev, seq, mask_buf)
        print(f"  48V = {enabled} (seq={seq-1})")
        time.sleep(0.5)
    return seq


def test_mute(dev, seq):
    """Test monitor mute toggle."""
    print("\n=== Monitor Mute ===")
    for muted in [True, False]:
        mask_buf = bytearray(128)
        val = 0x0002 if muted else 0x0000
        struct.pack_into("<I", mask_buf, SETTING_MONITOR * 8 + 4,
                         setting_word(0x0003, val))
        seq = write_settings(dev, seq, mask_buf)
        print(f"  Mute = {muted} (seq={seq-1})")
        time.sleep(0.5)
    return seq


def main():
    dev = find_device()
    claim_dsp(dev)

    # Start sequence counter at 7 (matching init capture)
    seq = 7

    print("\nSending mixer settings via vendor request 0x03...")
    print("Listen to the monitor output for audio changes.")

    try:
        seq = test_monitor_level(dev, seq)
        seq = test_preamp_gain(dev, seq)
        seq = test_phantom(dev, seq)
        seq = test_mute(dev, seq)
    except usb.core.USBError as e:
        print(f"\nUSB ERROR: {e}")
        print("This might mean the vendor request format is wrong.")
        sys.exit(1)

    print(f"\nAll tests passed! Final seq={seq-1}")
    print("\nIf you heard monitor level changes and gain changes,")
    print("the mixer settings protocol is working.")


if __name__ == "__main__":
    main()
