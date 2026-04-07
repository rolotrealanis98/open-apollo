#!/usr/bin/env python3
"""Initialize DSP on UA Apollo USB devices.

Sends the DSP init command via bulk endpoint, sets the UAC2 clock
frequency, configures a safe default monitor level, and drains EP6
interrupt notifications to prevent xHCI buffer overruns on Intel
controllers.

Must run after firmware is loaded (device at PID 000d/0002/000f).
Can be called standalone or from udev via ua-usb-dsp-init.sh.

Usage:
  sudo python3 usb-dsp-init.py            # init + exit
  sudo python3 usb-dsp-init.py --daemon   # init + keep draining EP6
"""
import os
import struct
import sys
import threading
import time
import usb.core
import usb.util

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


def dsp_init(dev):
    """Send DSP init: activate FPGA, set hardware config, write identity
    routing table.

    Sequence verified from Cauldron 1.3 build 3 USBPcap capture (2026-04-05).
    """
    seq = 0

    # Packet 0: FPGA_ACTIVATE + CONFIG_A x2
    pkt = struct.pack("<HBB", 6, seq, MAGIC_CMD)
    pkt += struct.pack("<HHI", 0x0002, 0x0023, 0x00000001)
    pkt += struct.pack("<HHI", 0x0002, 0x0010, 0x01B7A1FA)
    pkt += struct.pack("<HHI", 0x0002, 0x0010, 0x01B7A1FA)
    dev.write(EP_BULK_OUT, pkt, timeout=1000)
    seq += 1

    # Packet 1: CONFIG_B
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

    # Packets 2-14: Identity routing table (slots 0x00-0x1F)
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

    # Detach kernel drivers from DSP + audio control interfaces
    for intf in [0, 1]:
        try:
            if dev.is_kernel_driver_active(intf):
                dev.detach_kernel_driver(intf)
        except Exception:
            pass

    usb.util.claim_interface(dev, 0)

    # Start EP6 drain BEFORE activating DSP (matches Windows driver behavior)
    # On Intel xHCI, EP6 notifications overflow the transfer ring if not consumed
    stop_event = threading.Event()
    ep6_thread = threading.Thread(target=drain_ep6, args=(dev, stop_event),
                                  daemon=True)
    ep6_thread.start()

    # Step 1: DSP init (activate FPGA + routing table)
    dsp_init(dev)
    print("DSP init done")

    # Step 2: Set clock
    freq = set_clock(dev, 48000, seq=0x10)
    print("Clock: {} Hz".format(freq))

    if not daemon_mode:
        # Interactive mode: release interface and exit
        # EP6 drain stops when the script exits (daemon thread)
        usb.util.release_interface(dev, 0)

    # Step 3: Set monitor level to -12 dB
    if set_monitor_level(dev, -12):
        print("Monitor: -12 dB")
    else:
        print("Monitor level set failed (non-fatal)")

    print("Ready")

    if daemon_mode:
        # Daemon mode: keep running to drain EP6 continuously
        # Interface 0 stays claimed — snd-usb-audio only needs interfaces 1-3
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
