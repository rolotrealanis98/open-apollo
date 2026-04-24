#!/bin/bash
# Initialize DSP on UA Apollo USB after firmware load.
# Called by udev when post-firmware device (PID 000d/0002/000f) appears.
#
# Runs the full 38-packet DSP init (including program load needed for
# capture), then rebinds so snd-usb-audio probes with DSP active.
# The DSP program persists in FPGA memory across SET_INTERFACE.
# No daemon needed — one-shot init is sufficient.

set -euo pipefail

LOG_TAG="ua-usb-dsp-init"
log() { logger -t "$LOG_TAG" "$*"; }

# Delay for USB enumeration and snd-usb-audio probe to settle.  2s wasn't
# enough on AMD xHCI / slow systems — snd-usb-audio was still claiming its
# interfaces while usb-full-init.py tried to drive interface 0, producing
# a wedged bulk endpoint and EIO/timeout on packet 0.
sleep 4

# Kill any leftover daemon from previous installs
pkill -f "usb-dsp-init.py --daemon" 2>/dev/null || true

# Run full DSP init (38 packets including DSP program load for capture).
# One retry on failure — packet-0 timeouts are usually transient state
# wedges; usb-full-init.py now self-drains and clear_halts before replay.
#
# `set -o pipefail` (top of file) makes the pipeline exit status reflect
# python3's, not the tail `while read` — without it every invocation would
# look successful even on a Python traceback.  The second attempt's failure
# must propagate out of this script so callers (udev, install-usb.sh) can
# see it; earlier versions masked it with `|| log ...` and exit 0.
run_init() {
    python3 /usr/local/lib/ua-usb/usb-full-init.py 2>&1 | while read -r line; do
        log "$line"
    done
}

log "Running full DSP init"
if ! run_init; then
    log "First init attempt failed — retrying after 3s"
    sleep 3
    if ! run_init; then
        log "Second init attempt failed — power-cycle the Apollo (unplug USB, wait 5s, replug)"
        exit 1
    fi
fi

# Rebind USB device so snd-usb-audio probes interfaces 1-3
DEVPATH=$(find /sys/bus/usb/devices/ -maxdepth 1 -name '[0-9]*' -exec sh -c \
    'cat "$1/idVendor" 2>/dev/null | grep -q 2b5a && basename "$1"' _ {} \; | head -1)
if [ -n "$DEVPATH" ]; then
    log "Rebinding $DEVPATH for audio enumeration"
    echo "$DEVPATH" > /sys/bus/usb/drivers/usb/unbind 2>/dev/null || true
    sleep 1
    echo "$DEVPATH" > /sys/bus/usb/drivers/usb/bind 2>/dev/null || true
fi

log "DSP init complete"
