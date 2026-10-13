#!/bin/bash
# device-probe.sh — Gather hardware info from a running Apollo system
#
# This script is READ-ONLY. It does not write to any hardware registers
# or modify any driver state. All output is saved to a local JSON file
# that you can review. Optionally uploads the report to the Open Apollo API.
#
# Usage:
#   ./device-probe.sh [--output path/to/report.json] [--send]
#   (--send uploads the report without asking; otherwise it is asked at a
#   terminal and never sent without one)

set -euo pipefail

TELEMETRY_URL="https://open-apollo-api.rolotrealanis.workers.dev/captures"

GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[1;33m'
NC='\033[0m'

# ============================================================================
# Header
# ============================================================================
echo ""
echo "Open Apollo — Device Probe"
echo "=========================="
echo ""
echo "This script collects hardware information about your Universal Audio"
echo "Apollo interface for the Open Apollo project. It is completely read-only"
echo "and makes no changes to your system or hardware."
echo ""

# ============================================================================
# Parse arguments
# ============================================================================
OUTPUT=""
SEND=0
while [[ $# -gt 0 ]]; do
    case "$1" in
        --output)
            OUTPUT="$2"
            shift 2
            ;;
        --send)
            SEND=1
            shift
            ;;
        *)
            echo "Unknown option: $1"
            echo "Usage: $0 [--output path/to/report.json] [--send]"
            exit 1
            ;;
    esac
done

if [ -z "$OUTPUT" ]; then
    OUTPUT="./open-apollo-report-$(date +%Y%m%d).json"
fi

# ============================================================================
# Check that the driver is loaded
# ============================================================================
echo "Checking for ua_apollo driver..."
if ! lsmod | grep -q ua_apollo; then
    printf "${RED}ua_apollo kernel module is not loaded.${NC}\n"
    echo ""
    echo "Load the driver first:"
    echo "  sudo insmod driver/ua_apollo.ko"
    echo ""
    echo "If you haven't built it yet, run:"
    echo "  ./scripts/install.sh"
    exit 1
fi
printf "${GREEN}Driver loaded.${NC}\n"
echo ""

# ============================================================================
# Collect kernel module info
# ============================================================================
echo "Collecting kernel module info..."
MODINFO=$(modinfo ua_apollo 2>/dev/null || echo "{}")
MOD_VERSION=$(echo "$MODINFO" | grep -m1 '^version:' | awk '{print $2}' || echo "unknown")
MOD_SRCVERSION=$(echo "$MODINFO" | grep -m1 '^srcversion:' | awk '{print $2}' || echo "unknown")
KERNEL_VERSION=$(uname -r)

# ============================================================================
# Helpers
# ============================================================================

# Escape a value for use inside a JSON string literal. Backslashes must be
# doubled before quotes are escaped, or the added backslashes get escaped too.
# Control characters (including newlines) are stripped so every value stays a
# single-line JSON string.
json_escape() {
    printf '%s' "${1:-}" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g' | tr -d '\000-\037'
}

# ============================================================================
# Collect PCI device info
# ============================================================================
echo "Collecting PCI device info..."
# Universal Audio's PCI vendor ID (PCI_VENDOR_ID_UA, driver/ua_apollo.h).
# Matching on it directly is the only reliable option: 1a00 is absent from
# pci.ids, so lspci prints "Device [1a00:0002]" with no vendor name to grep
# for, and Apollo units enumerate under class 0480 (Multimedia controller)
# rather than 0401 (Multimedia audio controller).
UA_PCI_VENDOR="1a00"

PCI_SLOT=""
PCI_VENDOR=""
PCI_DEVICE=""
PCI_SUBSYS_VENDOR=""
PCI_SUBSYS_DEVICE=""
PCI_CLASS=""

UA_LINE=$(lspci -nn -d "${UA_PCI_VENDOR}:" 2>/dev/null | head -1 || true)
if [ -n "$UA_LINE" ]; then
    PCI_SLOT=$(printf '%s' "$UA_LINE" | awk '{print $1}')

    # -vmm is the machine-readable form: one "Field:<TAB>value" per line. It is
    # the only lspci mode that reports the subsystem ID, which is what the
    # driver keys device detection off (see ua_core.c probe).
    PCI_VMM=$(lspci -nn -vmm -s "$PCI_SLOT" 2>/dev/null || true)

    # Pull the bracketed hex ID off a -vmm field, e.g. "Device [0002]" -> 0002.
    vmm_id() {
        printf '%s\n' "$PCI_VMM" \
            | awk -F'\t' -v key="$1:" '$1 == key { print $2; exit }' \
            | grep -o '\[[0-9a-f]*\]$' | tr -d '[]' || true
    }

    PCI_VENDOR=$(vmm_id Vendor)
    PCI_DEVICE=$(vmm_id Device)
    PCI_SUBSYS_VENDOR=$(vmm_id SVendor)
    PCI_SUBSYS_DEVICE=$(vmm_id SDevice)
    PCI_CLASS=$(vmm_id Class)
fi

# ============================================================================
# Collect device identity from dmesg
# ============================================================================
echo "Collecting device type from kernel log..."
DMESG_UA=$(dmesg 2>/dev/null | grep 'ua_apollo' || true)

# The driver's probe banner carries the richest identification in one line:
#   ua_apollo 0000:3d:00.0: Apollo x8p: FPGA rev 0x…, subsys 0x0014, 6 DSPs, FW v2
DEVICE_BANNER=$(printf '%s\n' "$DMESG_UA" | grep 'FPGA rev' | tail -1 || true)
DEVICE_NAME=$(printf '%s' "$DEVICE_BANNER" | sed -n 's/.*: \([^:]*\): FPGA rev.*/\1/p')
FPGA_REV=$(printf '%s' "$DEVICE_BANNER" | grep -o 'FPGA rev 0x[0-9a-fA-F]*' | awk '{print $3}' || true)
SUBSYS_LOGGED=$(printf '%s' "$DEVICE_BANNER" | grep -o 'subsys 0x[0-9a-fA-F]*' | awk '{print $2}' || true)
NUM_DSPS=$(printf '%s' "$DEVICE_BANNER" | grep -o '[0-9]* DSPs' | awk '{print $1}' || true)
FW_REV=$(printf '%s' "$DEVICE_BANNER" | grep -o 'FW v[0-9]*' | awk '{print $2}' || true)

# device_type is logged separately, at transport start (ua_audio.c), as
# "FW_VERSION: 0x…, device_type: 0xa" — colon-space, not "=".
DEVICE_TYPE=$(printf '%s\n' "$DMESG_UA" | grep -o 'device_type: 0x[0-9a-fA-F]*' | tail -1 | awk '{print $2}' || true)
[ -n "$DEVICE_TYPE" ] || DEVICE_TYPE="unknown"

# ============================================================================
# Collect ALSA card info
# ============================================================================
echo "Collecting ALSA device info..."
# /proc/asound/cards is the only place card->driver ("ua_apollo") is exposed.
# aplay -l prints card->shortname ("Apollo x8p") and the PCM name ("UA Apollo")
# instead, so it can only be matched on those.
CARD_NUM=$(awk '/ua_apollo/ { print $1; exit }' /proc/asound/cards 2>/dev/null || true)
if [ -z "$CARD_NUM" ]; then
    CARD_NUM=$(aplay -l 2>/dev/null | sed -n 's/^card \([0-9]*\):.*UA Apollo.*/\1/p' | head -1 || true)
fi

APLAY_OUT=$(aplay -l 2>/dev/null | grep 'UA Apollo' | head -1 || true)
[ -n "$APLAY_OUT" ] || APLAY_OUT="not found"
ARECORD_OUT=$(arecord -l 2>/dev/null | grep 'UA Apollo' | head -1 || true)
[ -n "$ARECORD_OUT" ] || ARECORD_OUT="not found"

# aplay -l reports no channel counts. Take them from the driver's own
# pcm_prepare log line, falling back to hw_params when a stream is open right
# now. Both are read-only; opening the PCM to query it is deliberately avoided.
read_channels() {
    local stream="$1" node="$2" val=""
    val=$(printf '%s\n' "$DMESG_UA" | grep "pcm_prepare: stream=$stream" | tail -1 \
        | grep -o 'channels=[0-9]*' | cut -d= -f2 || true)
    if [ -z "$val" ] && [ -n "$CARD_NUM" ]; then
        val=$(awk '/^channels:/ { print $2; exit }' \
            "/proc/asound/card$CARD_NUM/$node/sub0/hw_params" 2>/dev/null || true)
    fi
    printf '%s' "${val:-unknown}"
}
PLAY_CHANNELS=$(read_channels play pcm0p)
REC_CHANNELS=$(read_channels rec pcm0c)

# ============================================================================
# Collect ALSA controls
# ============================================================================
echo "Collecting ALSA mixer controls..."
ALSA_CONTROLS=""
CONTROL_COUNT=0
if [ -n "$CARD_NUM" ]; then
    ALSA_CONTROLS=$(amixer -c "$CARD_NUM" scontrols 2>/dev/null || true)
    # grep -c already prints 0 when there are no matches; it just exits 1 while
    # doing so, so the exit status is swallowed rather than a second 0 appended.
    CONTROL_COUNT=$(printf '%s\n' "$ALSA_CONTROLS" | grep -c 'Simple mixer' || true)
fi

# ============================================================================
# Build JSON report
# ============================================================================
echo ""
echo "Building report..."

DISTRO=$(grep -m1 '^PRETTY_NAME=' /etc/os-release 2>/dev/null | cut -d= -f2- | tr -d '"' || true)
[ -n "$DISTRO" ] || DISTRO="unknown"

cat > "$OUTPUT" << ENDJSON
{
  "report_version": "1.1",
  "platform": "linux",
  "generated_at": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "system": {
    "kernel": "$(json_escape "$KERNEL_VERSION")",
    "distro": "$(json_escape "$DISTRO")",
    "arch": "$(uname -m)"
  },
  "driver": {
    "version": "$(json_escape "$MOD_VERSION")",
    "srcversion": "$(json_escape "$MOD_SRCVERSION")"
  },
  "pci": {
    "slot": "$(json_escape "$PCI_SLOT")",
    "vendor_id": "$(json_escape "$PCI_VENDOR")",
    "device_id": "$(json_escape "$PCI_DEVICE")",
    "subsystem_vendor_id": "$(json_escape "$PCI_SUBSYS_VENDOR")",
    "subsystem_device_id": "$(json_escape "$PCI_SUBSYS_DEVICE")",
    "class_id": "$(json_escape "$PCI_CLASS")",
    "lspci_line": "$(json_escape "$UA_LINE")"
  },
  "device": {
    "type": "$(json_escape "$DEVICE_TYPE")",
    "name": "$(json_escape "$DEVICE_NAME")",
    "fpga_rev": "$(json_escape "$FPGA_REV")",
    "subsystem_id": "$(json_escape "$SUBSYS_LOGGED")",
    "num_dsps": "$(json_escape "$NUM_DSPS")",
    "fw_rev": "$(json_escape "$FW_REV")",
    "dmesg_line": "$(json_escape "$DEVICE_BANNER")"
  },
  "alsa": {
    "card": "$(json_escape "$CARD_NUM")",
    "playback": "$(json_escape "$APLAY_OUT")",
    "capture": "$(json_escape "$ARECORD_OUT")",
    "play_channels": "$(json_escape "$PLAY_CHANNELS")",
    "rec_channels": "$(json_escape "$REC_CHANNELS")",
    "control_count": "$CONTROL_COUNT"
  }
}
ENDJSON

# ============================================================================
# Done
# ============================================================================
echo ""
printf "${GREEN}Report saved to: ${OUTPUT}${NC}\n"
echo ""

# ============================================================================
# Telemetry — opt-in upload
# ============================================================================
# Opt-in only: sent when the user says yes at a terminal or passed --send.
# No terminal means no.
ANSWER="n"
if [ "$SEND" = "1" ]; then
    ANSWER="y"
elif [ -t 0 ]; then
    read -rp "Help improve Open Apollo — send this device report anonymously? [y/N] " ANSWER
fi

if [[ "$ANSWER" =~ ^[Yy] ]]; then
    echo "Sending report to $TELEMETRY_URL..."
    HTTP_CODE=$(curl -s -o /dev/null -w '%{http_code}' \
        -X POST "$TELEMETRY_URL" \
        -H "Content-Type: application/json" \
        -d @"$OUTPUT" 2>/dev/null || echo "000")

    if [ "$HTTP_CODE" = "200" ] || [ "$HTTP_CODE" = "201" ]; then
        printf "${GREEN}Report sent — thank you!${NC}\n"
    else
        printf "${YELLOW}Upload failed (HTTP $HTTP_CODE) — report saved locally at $OUTPUT${NC}\n"
    fi
else
    echo "No data sent. Report saved locally at $OUTPUT"
    [ -t 0 ] || echo "To send it: re-run with --send"
fi

echo ""
echo "To also submit manually:"
echo "  1. Review the file — it contains only hardware identifiers, no personal data"
echo "  2. Go to: https://github.com/rolotrealanis98/open-apollo/issues/new?template=device-report.yml"
echo "  3. Attach the JSON file or paste its contents"
echo "  4. Add notes about what works and what doesn't"
echo ""
