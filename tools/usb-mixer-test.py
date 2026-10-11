#!/usr/bin/env python3
"""Test USB mixer settings writes on Apollo Solo USB.

Requires: Apollo Solo USB connected, FX3 firmware loaded, DSP initialized.
Run after: tools/usb-full-init.py (or the udev init)

Sends the daemon's SetMixerParam values through the same path the mixer
daemon's USB backend uses (uausb/mixer.py): the PCIe driver's param ->
setting mapping, the one-shot mailbox, the SEQ_WR/SEQ_RD handshake on
vendor requests 0x03/0x04, and reads the 40-word readback (request 0x10)
after each step to show what the hardware reports.  Earlier versions wrote
fixed buffers with a blind sequence number; GainB/GainA went to the wrong
settings for input 2 and "mute" set the mono bit.

Tests (listen to the monitor output; untested on hardware):
  1. Monitor level sweep (setting 2 bits 7:0)
  2. Preamp gain sweep on input 1 (settings 17, 16, 3)
  3. 48V on input 1 on/off (setting 0 bit 3)
  4. Monitor mute on/off (setting 2 bit 16)

Vendor control requests need no interface claim, so this runs next to
snd-usb-audio.  Stop the mixer daemon first if it uses the USB backend.

Usage: sudo python3 tools/usb-mixer-test.py
"""

import os
import sys
import time

_script_dir = os.path.dirname(os.path.abspath(__file__))
if _script_dir not in sys.path:
    sys.path.insert(0, _script_dir)

from uausb import device as udev  # noqa: E402
from uausb import mixer as umix  # noqa: E402

UA_VID = 0x2B5A
LIVE_PIDS = {
    0x000D: "Apollo Solo USB",
    0x0002: "Twin USB",
    0x000F: "Twin X USB",
}


def find_live_device(usb):
    """Return (device, model_name) for whichever live Apollo USB is present."""
    for pid, name in LIVE_PIDS.items():
        dev = usb.core.find(idVendor=UA_VID, idProduct=pid)
        if dev:
            return dev, name
    return None, None


class Mixer:
    def __init__(self, dev):
        self.regs = udev.UsbRegs(dev)
        self.mb = umix.SettingsMailbox()
        self.mb.adopt_seq(self.regs)
        self.protocol = self.regs.protocol_word()

    def set(self, channel_type, ch, param, value):
        res = umix.map_param(channel_type, ch, param, value)
        if isinstance(res, umix.SettingWrite):
            self.mb.merge(res.setting, res.value, res.mask)

    def commit(self, timeout=1.0):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.mb.flush(self.regs) is not False:
                return True
            time.sleep(0.02)
        print("  DSP did not acknowledge (SEQ_RD stuck)")
        return False

    def readback(self):
        status, words = self.regs.readback(self.protocol)
        return words if status == 1 else None


def show(mx, what):
    time.sleep(0.1)
    rb = mx.readback()
    if rb is None:
        print(f"  {what}: no readback")
        return
    print(f"  {what}: readback w0={rb[0]:08x} w2={rb[2]:08x} w3={rb[3]:08x}")


def test_monitor_level(mx):
    print("\n=== Monitor Level ===")
    for db in [-96, -48, -24, -12, 0, -12]:
        raw = max(0, min(0xC0, int(192 + db * 2)))
        mx.set(umix.MONITOR_CHANNEL_TYPE, 1, 0x01, raw)
        mx.commit()
        show(mx, f"monitor {db:4d} dB (raw 0x{raw:02x})")
        time.sleep(0.3)


def test_preamp_gain(mx):
    print("\n=== Preamp Gain (input 1) ===")
    for gain_db in [10, 20, 30, 40, 50, 60, 30, 10]:
        ab = max(0, min(54, gain_db - 10))
        c = max(0, min(56, gain_db - 9))
        for param, value in ((0x0A, ab), (0x09, ab), (0x06, c)):
            mx.set(umix.PREAMP_CHANNEL_TYPE, 0, param, value)
        mx.commit()
        show(mx, f"gain {gain_db:2d} dB")
        time.sleep(0.3)


def test_phantom(mx):
    print("\n=== 48V Phantom (input 1) ===")
    for enabled in [True, False]:
        mx.set(umix.PREAMP_CHANNEL_TYPE, 0, 0x03, int(enabled))
        mx.commit()
        show(mx, f"48V {'on' if enabled else 'off'}")
        time.sleep(0.5)


def test_mute(mx):
    print("\n=== Monitor Mute ===")
    for muted in [True, False]:
        mx.set(umix.MONITOR_CHANNEL_TYPE, 0, 0x03, 2 if muted else 0)
        mx.commit()
        show(mx, f"mute {'on' if muted else 'off'}")
        time.sleep(0.5)


def main():
    try:
        import usb.core
    except ImportError:
        print("pip install pyusb")
        return 1
    import usb as usb_pkg

    dev, model = find_live_device(usb_pkg)
    if not dev:
        print("No live Apollo USB device found "
              "(expected one of: %s)" % ", ".join(LIVE_PIDS.values()))
        print("Is firmware loaded? Run: sudo python3 tools/fx3-load.py")
        return 1
    print(f"Found: {model} ({dev.product})")
    mx = Mixer(dev)
    print(f"SEQ_RD {mx.mb.seq_wr}, protocol {mx.protocol:#010x}")
    show(mx, "start")
    try:
        test_monitor_level(mx)
        test_preamp_gain(mx)
        test_phantom(mx)
        test_mute(mx)
    except usb.core.USBError as e:
        print(f"\nUSB ERROR: {e}")
        return 1
    print(f"\nDone. Final SEQ_WR={mx.mb.seq_wr}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
