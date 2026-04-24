#!/usr/bin/env python3
"""Full DSP + routing init for Apollo Solo USB.

Replays the complete 38-packet bulk init sequence captured from Windows
UA Console, then sets the UAC2 clock and initial monitor level.
This configures the FPGA routing matrix so capture channels receive signal.

Usage: sudo python3 tools/usb-full-init.py

After running, reload snd-usb-audio:
    sudo rmmod snd_usb_audio; sudo insmod /tmp/sound/usb/snd-usb-audio.ko
"""
import os
import struct
import sys
import time

import usb.core
import usb.util

UA_VID = 0x2B5A
SOLO_PID = 0x000D
EP_BULK_OUT = 0x01
EP_BULK_IN = 0x81
# Look for init sequence in both repo layout and installed layout
_script_dir = os.path.dirname(os.path.abspath(__file__))
INIT_BIN = os.path.join(_script_dir, "usb-re", "init-bulk-sequence.bin")
if not os.path.exists(INIT_BIN):
    INIT_BIN = os.path.join(_script_dir, "init-bulk-sequence.bin")


def replay_init_sequence(dev, bin_path):
    """Replay the captured init bulk sequence.

    The 38-packet sequence has three phases:
      0-9:   FPGA config + clock setup
      10-23: DSP program loads (large packets)
      24-37: Routing table + monitor config

    The DSP needs time to process program loads before accepting routing
    writes.  Windows UA Console leaves 1+ second gaps between phases.
    """
    # Packets after DSP program loads that are routing block writes —
    # the DSP must finish processing programs before these succeed.
    DSP_PROGRAM_END = 23      # last DSP program load packet
    ROUTING_START = 24        # first routing config packet

    with open(bin_path, "rb") as f:
        count = struct.unpack("<I", f.read(4))[0]
        print(f"Replaying {count} bulk OUT packets...")
        for i in range(count):
            pkt_len = struct.unpack("<I", f.read(4))[0]
            pkt_data = f.read(pkt_len)

            wc, cmd_type, magic = struct.unpack_from("<HBB", pkt_data, 0)

            # Phase transition delay — let DSP finish processing programs
            if i == ROUTING_START:
                print("  -- waiting for DSP to process program loads --")
                time.sleep(2.0)

            # All packets get generous timeout (AMD xHCI can be slow)
            timeout = 10000

            # Inter-packet pacing: small delay between all packets,
            # longer delay after large ones (DSP program uploads)
            if i > 0:
                if pkt_len > 512:
                    time.sleep(0.2)
                else:
                    time.sleep(0.05)

            for attempt in range(3):
                try:
                    dev.write(EP_BULK_OUT, pkt_data, timeout=timeout)
                    break
                except usb.core.USBTimeoutError:
                    if attempt == 2:
                        # Packet 0 timing out after all retries means the
                        # device is wedged — usually a halted EP that
                        # survived clear_halt, or stale state from a racing
                        # snd-usb-audio probe.  A one-time bulk-IN drain +
                        # halt-clear gives the device one last chance before
                        # we surface the error to the user.
                        if i == 0:
                            try:
                                usb.util.clear_halt(dev, EP_BULK_OUT)
                                usb.util.clear_halt(dev, EP_BULK_IN)
                            except Exception:
                                pass
                            try:
                                while True:
                                    dev.read(EP_BULK_IN, 1024, timeout=200)
                            except usb.core.USBTimeoutError:
                                pass
                            try:
                                dev.write(EP_BULK_OUT, pkt_data,
                                          timeout=timeout)
                                break
                            except usb.core.USBTimeoutError:
                                print("  [ 0] device unresponsive — "
                                      "power-cycle the Apollo (unplug "
                                      "USB, wait 5s, replug) and retry.")
                                raise
                        raise
                    wait = 1.0 * (attempt + 1)
                    print(f"  [{i:2d}] timeout, retrying ({attempt+1}/3) "
                          f"after {wait:.0f}s...")
                    time.sleep(wait)
                    # Drain any stale responses before retry
                    try:
                        while True:
                            dev.read(EP_BULK_IN, 1024, timeout=100)
                    except usb.core.USBTimeoutError:
                        pass

            print(f"  [{i:2d}] type={cmd_type:3d} words={wc:3d} len={pkt_len}")

            # Drain responses — wait longer after big packets
            drain_timeout = 1000 if pkt_len > 512 else 500
            try:
                while True:
                    dev.read(EP_BULK_IN, 1024, timeout=drain_timeout)
            except usb.core.USBTimeoutError:
                pass

    print(f"  Sent all {count} packets")


dev = usb.core.find(idVendor=UA_VID, idProduct=SOLO_PID)
if not dev:
    print("Apollo Solo USB not found")
    sys.exit(1)
print(f"Found: {dev.product}")

if not os.path.exists(INIT_BIN):
    print(f"Missing: {INIT_BIN}")
    sys.exit(1)

# Only detach Interface 0 (vendor DSP). Leave audio interfaces for snd-usb-audio.
try:
    if dev.is_kernel_driver_active(0):
        dev.detach_kernel_driver(0)
except Exception:
    pass
usb.util.claim_interface(dev, 0)

# Normalize device state before replay.  Three failure modes get cleared here:
#   1. Stale bulk-IN responses from a prior udev-triggered init — drain them
#      or the first bulk OUT times out waiting for the device to service us.
#   2. Halted bulk endpoints from an interrupted transfer (prior Ctrl-C,
#      reboot mid-init, or a racing snd-usb-audio probe) — clear_halt resets
#      the STALL condition so packet 0 can actually leave the host.
#   3. DSP already running from a prior boot init — the FPGA sequence counter
#      is mid-sequence, so re-running from packet 0 hangs.  A bulk-IN drain
#      pulls any pending state replies; a subsequent init overwrites FPGA
#      state cleanly because every packet carries an absolute write address.
try:
    usb.util.clear_halt(dev, EP_BULK_OUT)
except Exception:
    pass
try:
    usb.util.clear_halt(dev, EP_BULK_IN)
except Exception:
    pass
# Drain any stale responses queued from a prior init.
for _ in range(32):
    try:
        dev.read(EP_BULK_IN, 1024, timeout=50)
    except usb.core.USBTimeoutError:
        break

# Step 1: Full init sequence (FPGA + routing + DSP programs)
replay_init_sequence(dev, INIT_BIN)

# Step 2: Set clock
dev.ctrl_transfer(0x21, 0x01, 0x0100, 0x8001,
                  struct.pack("<I", 48000), timeout=2000)
data = dev.ctrl_transfer(0xA1, 0x01, 0x0100, 0x8001, 4, timeout=1000)
freq = struct.unpack("<I", bytes(data))[0]
print(f"Clock: {freq} Hz")

usb.util.release_interface(dev, 0)

# Step 3: Set monitor level to -12dB via vendor ctrl (no interface claim needed)
# Use high sequence counter (100) so FPGA processes it — the full init
# leaves the internal counter at ~38, so seq=7 gets ignored.
raw = int(192 + (-12) * 2)  # 0xa8
mask_buf = bytearray(128)
struct.pack_into("<I", mask_buf, 16, (0x00FF << 16) | raw)
dev.ctrl_transfer(0x41, 0x03, 0x062D, 0, bytes(mask_buf), timeout=1000)
dev.ctrl_transfer(0x41, 0x03, 0x0602, 0, struct.pack("<I", 100), timeout=1000)
print("Monitor: -12 dB")

print("Ready — run 'sudo modprobe snd_usb_audio' to get ALSA card")
