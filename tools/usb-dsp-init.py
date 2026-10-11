#!/usr/bin/env python3
"""Initialize DSP on UA Apollo USB devices.

Sends the DSP init command via bulk endpoint, sets the UAC2 clock
frequency, configures a safe default monitor level, and drains EP6
interrupt notifications to prevent xHCI buffer overruns on Intel
controllers.

Must run after firmware is loaded (device at PID 000d/0002/000f).
Can be called standalone or from udev via ua-usb-dsp-init.sh.

Before the first command it polls the DSP boot word (vendor request 0x04,
FPGA word 0x1069) until the DSP has started, as the Windows driver does
before its connect (uausb/device.py wait_dsp_started; untested on
hardware).  That is the readiness signal the fixed 30 s sleeps of the Twin
USB reports (#69) stand in for.  If the word cannot be read the init goes
ahead as before.

The commands, in the DSP command language (uausb/dspcmd.py): CONNECT
{0x00230002, 1}, the connect date word {0x00100002, d} twice, the query
{0x00110002, tag} (the DSP answers with an 0x8003 reply), then DSP input
params {0x001E0004, input, 8 (span), 0x20 (none)} for inputs 0x00-0x1F
except 0x10-0x13.  The date and tag words are the ones of the Cauldron 1.3
capture this sequence came from.

Usage:
  sudo python3 usb-dsp-init.py            # init + exit
  sudo python3 usb-dsp-init.py --daemon   # init + keep draining EP6
  sudo python3 usb-dsp-init.py --no-ready-poll
"""
import os
import struct
import sys
import threading
import time
import usb.core
import usb.util

_script_dir = os.path.dirname(os.path.abspath(__file__))
if _script_dir not in sys.path:
    sys.path.insert(0, _script_dir)
try:
    from uausb import device as udev
except ImportError:             # an install with only this script copied
    udev = None

UA_VID = 0x2B5A
LIVE_PIDS = {
    0x000D: "Apollo Solo USB",
    0x0002: "Twin USB",
    0x000F: "Twin X USB",
}

EP_BULK_OUT = 0x01
EP_BULK_IN = 0x81
EP_INTR_IN = 0x86  # EP6 IN — JFK notifications

MAGIC_CMD = 0xDC

# Mixer settings FPGA addresses (vendor request 0x03)
SETTINGS_SEQ = 0x0602
SETTINGS_MASK = 0x062D
SETTING_MONITOR = 2


def find_device():
    """Find any UA USB device in post-firmware state."""
    for pid, name in LIVE_PIDS.items():
        dev = usb.core.find(idVendor=UA_VID, idProduct=pid)
        if dev:
            return dev, name
    return None, None


def usb_link_speed_mbps(dev=None):
    """Return the Apollo's USB link speed in Mbps from sysfs, or None.

    Read-only. Matches the device by bus/address when given, otherwise by
    VID/PID (same live PIDs as scripts/install-usb.sh).
    """
    base = "/sys/bus/usb/devices"
    try:
        entries = os.listdir(base)
    except OSError:
        return None

    def rd(path):
        try:
            with open(path) as f:
                return f.read().strip()
        except OSError:
            return None

    for e in entries:
        d = os.path.join(base, e)
        if rd(os.path.join(d, "idVendor")) != "2b5a":
            continue
        if dev is not None:
            if (rd(os.path.join(d, "busnum")) != str(dev.bus)
                    or rd(os.path.join(d, "devnum")) != str(dev.address)):
                continue
        else:
            try:
                if int(rd(os.path.join(d, "idProduct")) or "", 16) not in LIVE_PIDS:
                    continue
            except ValueError:
                continue
        try:
            return int(float(rd(os.path.join(d, "speed")) or ""))
        except ValueError:
            return None
    return None


def warn_if_not_superspeed(dev=None):
    """Warn when the Apollo enumerated below SuperSpeed (5000 Mbps).

    Mirrors the check in scripts/install-usb.sh: at USB 2.0 the audio
    interfaces are not available, so a bad cable or port otherwise shows
    up only as a missing ALSA card.
    """
    speed = usb_link_speed_mbps(dev)
    if speed is None:
        return
    if speed < 5000:
        print("WARNING: Apollo USB link is {} Mbps (NOT SuperSpeed). Audio "
              "needs a USB 3.0 cable and port; use a USB 3.0 (blue) or "
              "USB-C/Thunderbolt connection.".format(speed), file=sys.stderr)
    else:
        print("USB link: {} Mbps (SuperSpeed)".format(speed))


def drain_ep6(dev, stop_event):
    """Continuously drain EP6 JFK notifications.

    The Apollo pushes interrupt packets on EP6 at up to 2000/sec once
    the DSP is activated (param 0x0023=1). On Intel xHCI controllers,
    if nothing reads these packets, the transfer ring overflows and
    floods dmesg with "buffer overrun event" messages — choking the
    entire USB stack and preventing ALSA card registration.

    The Windows driver pre-queues EP6 read URBs before activation.
    This thread does the same: start draining before dsp_init() sends
    the activate command.
    """
    while not stop_event.is_set():
        try:
            dev.read(EP_INTR_IN, 32, timeout=200)
        except usb.core.USBTimeoutError:
            pass
        except usb.core.USBError:
            break


def wait_dsp_ready(dev):
    """Poll the DSP boot word before the first command.  Returns True when
    the DSP reported started, False when it did not or the word could not
    be read (the init then goes ahead as before)."""
    if udev is None:
        return False
    res = udev.wait_dsp_started(udev.UsbRegs(dev), 0)
    if res.error:
        print("DSP boot word unreadable ({}): continuing".format(res.error))
        return False
    if not res.started:
        print("DSP did not start in {} tries (boot word {:#010x}): "
              "continuing".format(res.tries, res.value))
        return False
    print("DSP started (boot word {:#010x}, {} read(s))".format(
        res.value, res.tries))
    return True


def dsp_init(dev):
    """Send DSP init: connect, date words, query, input span params.

    Sequence verified from Cauldron 1.3 build 3 USBPcap capture (2026-04-05).
    """
    seq = 0

    # Packet 0: CONNECT {0x230002, 1} + date word {0x100002, d} x2
    pkt = struct.pack("<HBB", 6, seq, MAGIC_CMD)
    pkt += struct.pack("<HHI", 0x0002, 0x0023, 0x00000001)
    pkt += struct.pack("<HHI", 0x0002, 0x0010, 0x01B7A1FA)
    pkt += struct.pack("<HHI", 0x0002, 0x0010, 0x01B7A1FA)
    dev.write(EP_BULK_OUT, pkt, timeout=1000)
    seq += 1

    # Packet 1: query {0x110002, tag} (answered with an 0x8003 reply)
    pkt = struct.pack("<HBB", 2, seq, MAGIC_CMD)
    pkt += struct.pack("<HHI", 0x0002, 0x0011, 0x82607600)
    dev.write(EP_BULK_OUT, pkt, timeout=1000)
    seq += 1

    # Drain bulk responses
    try:
        while True:
            dev.read(EP_BULK_IN, 1024, timeout=200)
    except usb.core.USBTimeoutError:
        pass

    # Packets 2-14: DSP input span params {0x1E0004, input, 8, 0x20}
    # (inputs 0x00-0x1F, as the capture sends them)
    ROUTING_GROUPS = [
        [0x00, 0x01],
        [0x02, 0x03, 0x04, 0x05, 0x06],
        [0x07],
        [0x08],
        [0x09],
        [0x0A, 0x0B, 0x0C],
        [0x0D, 0x0E, 0x0F, 0x14],
        [0x15],
        [0x16],
        [0x17],
        [0x18, 0x19, 0x1A, 0x1B, 0x1C],
        [0x1D, 0x1E],
        [0x1F],
    ]

    for group in ROUTING_GROUPS:
        word_count = len(group) * 4
        pkt = struct.pack("<HBB", word_count, seq, MAGIC_CMD)
        for slot in group:
            pkt += struct.pack("<HHI", 0x0004, 0x001E, slot)
            pkt += struct.pack("<HHI", 0x0008, 0x0000, 0x00000020)
        dev.write(EP_BULK_OUT, pkt, timeout=1000)
        seq += 1

    # Drain routing responses
    try:
        while True:
            dev.read(EP_BULK_IN, 1024, timeout=200)
    except usb.core.USBTimeoutError:
        pass


def set_clock(dev, rate=48000, seq=16):
    """Set sample rate via DSP bulk protocol and UAC2 SET_CUR."""
    pkt = struct.pack("<HBB", 2, seq, MAGIC_CMD)
    pkt += struct.pack("<HHI", 0x0002, 0x001A, rate)
    try:
        dev.write(EP_BULK_OUT, pkt, timeout=1000)
    except usb.core.USBError:
        pass

    try:
        dev.ctrl_transfer(0x21, 0x01, 0x0100, 0x8001,
                          struct.pack("<I", rate), timeout=2000)
    except usb.core.USBError:
        pass

    try:
        data = dev.ctrl_transfer(0xA1, 0x01, 0x0100, 0x8001, 4, timeout=1000)
        return struct.unpack("<I", bytes(data))[0]
    except usb.core.USBError:
        return rate


def set_monitor_level(dev, db=-12):
    """Set monitor output level via vendor control request 0x03."""
    raw = max(0, min(0xC0, int(192 + db * 2)))
    mask_buf = bytearray(128)
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
    daemon_mode = "--daemon" in sys.argv

    dev, name = find_device()
    if not dev:
        print("No UA USB device found (post-firmware)")
        sys.exit(1)

    print("Found: {}".format(name))
    warn_if_not_superspeed(dev)

    # Detach kernel driver from Interface 0 (vendor DSP) only.
    # Do NOT touch interfaces 1-3 (audio) — snd-usb-audio needs them.
    try:
        if dev.is_kernel_driver_active(0):
            dev.detach_kernel_driver(0)
    except Exception:
        pass

    usb.util.claim_interface(dev, 0)

    # Start EP6 drain BEFORE activating DSP (matches Windows driver behavior)
    # On Intel xHCI, EP6 notifications overflow the transfer ring if not consumed
    stop_event = threading.Event()
    ep6_thread = threading.Thread(target=drain_ep6, args=(dev, stop_event),
                                  daemon=True)
    ep6_thread.start()

    # Step 0: wait for the DSP to report started (boot word)
    if "--no-ready-poll" not in sys.argv:
        wait_dsp_ready(dev)

    # Step 1: DSP init (connect + input span params)
    dsp_init(dev)
    print("DSP init done")

    # Step 2: Set clock
    freq = set_clock(dev, 48000, seq=0x10)
    print("Clock: {} Hz".format(freq))

    if not daemon_mode:
        # Interactive mode: release interface and exit
        # EP6 drain stops when the script exits (daemon thread)
        usb.util.release_interface(dev, 0)

    # NOTE: deliberately NOT calling set_monitor_level() here.
    # setting[2] (monitor core) holds volume/mute/source/dim state that the
    # ARM MCU needs for the front panel knob and buttons to work. Writing it
    # with a non-zero mask during init overwrites the firmware's own
    # defaults and breaks physical knob control — see the Register-Map and
    # Initialization-Sequence pages on the GitHub wiki.
    print("Ready")

    if daemon_mode:
        # Daemon mode: keep running to drain EP6 continuously.
        # DSP init already ran above — no periodic re-sends needed.
        # The daemon must start AFTER snd-usb-audio loads (SET_INTERFACE
        # wipes FPGA routing, so DSP init must come after probe).
        print("EP6 drain active (Ctrl+C to stop)")
        try:
            while True:
                time.sleep(1)
        except KeyboardInterrupt:
            pass
        finally:
            stop_event.set()
            ep6_thread.join(timeout=1)
            usb.util.release_interface(dev, 0)


if __name__ == "__main__":
    main()
