#!/bin/bash
# Initialize DSP on UA Apollo USB after firmware load.
# Called by udev when post-firmware device (PID 000d/0002/000f) appears.
# Sends DSP init command, drains EP6 notifications, rebinds for ALSA.

set -euo pipefail

LOG_TAG="ua-usb-dsp-init"
log() { logger -t "$LOG_TAG" "$*"; }

# Small delay for USB enumeration to settle
sleep 2

# Kill any previous EP6 drain daemon
pkill -f "usb-dsp-init.py --daemon" 2>/dev/null || true
sleep 0.5

# Run DSP init + start EP6 drain daemon in background
# The daemon keeps Interface 0 claimed and drains EP6 notifications.
# Without this, Intel xHCI controllers flood dmesg with buffer overruns.
log "Running DSP init + EP6 drain"
python3 /usr/local/lib/ua-usb/usb-dsp-init.py --daemon </dev/null >/dev/null 2>&1 &
DRAIN_PID=$!

# Wait for init to complete (the "Ready" message)
sleep 3

if kill -0 "$DRAIN_PID" 2>/dev/null; then
    log "EP6 drain running (PID $DRAIN_PID)"
else
    log "WARNING: EP6 drain exited early"
fi

# Rebind USB device so snd-usb-audio re-enumerates with DSP active
DEVPATH=$(find /sys/bus/usb/devices/ -maxdepth 1 -name '[0-9]*' -exec sh -c \
    'cat "$1/idVendor" 2>/dev/null | grep -q 2b5a && basename "$1"' _ {} \; | head -1)
if [ -n "$DEVPATH" ]; then
    log "Rebinding $DEVPATH for audio enumeration"
    echo "$DEVPATH" > /sys/bus/usb/drivers/usb/unbind 2>/dev/null || true
    sleep 1
    echo "$DEVPATH" > /sys/bus/usb/drivers/usb/bind 2>/dev/null || true
fi

log "DSP init complete"
