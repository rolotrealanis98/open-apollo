#!/bin/bash
# Upload firmware to UA Apollo USB FX3 stub.
# Called via systemd (see configs/systemd/ua-usb-init@.service) when the
# FX3 loader device appears.
# Usage: ua-usb-init <firmware-file>
#
# Device discovery is left to fx3-load.py (it scans for the FX3 loader PID
# by VID/PID itself), so this script doesn't need the udev device name —
# that avoids passing kernel device names through a systemd instance name.

set -euo pipefail

FIRMWARE="$1"
FW_DIR="/lib/firmware/universal-audio"
LOG_TAG="ua-usb-init"

log() { logger -t "$LOG_TAG" -- "$*"; }

FW_PATH="$FW_DIR/$FIRMWARE"
if [ ! -f "$FW_PATH" ]; then
    log "ERROR: Firmware not found: $FW_PATH"
    log "Download from UA Connect and place in $FW_DIR/"
    exit 1
fi

log "Loading firmware $FIRMWARE"

# Upload FX3 firmware using our loader script
python3 /usr/local/lib/ua-usb/fx3-load.py "$FW_PATH" 2>&1 | while read -r line; do
    log "$line"
done

log "Firmware uploaded, device will re-enumerate"
