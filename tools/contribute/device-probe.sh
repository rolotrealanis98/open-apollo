#!/bin/bash
# device-probe.sh — Gather hardware info from a running Apollo system
#
# This script is READ-ONLY. It does not write to any hardware registers
# or modify any driver state. All output is saved to a local JSON file
# that you can review. Optionally uploads the report to the Open Apollo API.
#
# Usage:
#   ./device-probe.sh [--output path/to/report.json]

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
while [[ $# -gt 0 ]]; do
    case "$1" in
        --output)
            OUTPUT="$2"
            shift 2
            ;;
        *)
            echo "Unknown option: $1"
            echo "Usage: $0 [--output path/to/report.json]"
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
# Collect PCI device info
# ============================================================================
echo "Collecting PCI device info..."
# Universal Audio vendor ID is 0x14e4, device IDs vary by model
PCI_INFO=$(lspci -d ::0401 -nn 2>/dev/null || echo "not found")
PCI_VENDOR=""
PCI_DEVICE=""
PCI_SLOT=""

# Try to find the UA device in lspci output
UA_LINE=$(lspci -nn 2>/dev/null | grep -i "universal audio\|ua apollo\|1fc9:" | head -1 || echo "")
if [ -n "$UA_LINE" ]; then
    PCI_SLOT=$(echo "$UA_LINE" | awk '{print $1}')
    # Extract vendor:device from [xxxx:yyyy] format
    PCI_IDS=$(echo "$UA_LINE" | grep -o '\[[0-9a-f]*:[0-9a-f]*\]' | tail -1 || echo "")
    PCI_VENDOR=$(echo "$PCI_IDS" | tr -d '[]' | cut -d: -f1)
    PCI_DEVICE=$(echo "$PCI_IDS" | tr -d '[]' | cut -d: -f2)
fi

# ============================================================================
# Collect device type from dmesg
# ============================================================================
echo "Collecting device type from kernel log..."
DEVICE_TYPE=$(dmesg 2>/dev/null | grep -o 'device_type=0x[0-9a-fA-F]*' | tail -1 || echo "unknown")
DEVICE_NAME=$(dmesg 2>/dev/null | grep 'ua_apollo.*detected\|ua_apollo.*Apollo' | tail -1 || echo "")

# ============================================================================
# Collect ALSA card info
# ============================================================================
echo "Collecting ALSA device info..."
APLAY_OUT=$(aplay -l 2>/dev/null | grep -A2 'ua_apollo' || echo "not found")
ARECORD_OUT=$(arecord -l 2>/dev/null | grep -A2 'ua_apollo' || echo "not found")

# Count channels from ALSA
PLAY_CHANNELS=$(aplay -l 2>/dev/null | grep 'ua_apollo' | grep -o '[0-9]* ch' | head -1 || echo "unknown")
REC_CHANNELS=$(arecord -l 2>/dev/null | grep 'ua_apollo' | grep -o '[0-9]* ch' | head -1 || echo "unknown")

# ============================================================================
# Collect ALSA controls
# ============================================================================
echo "Collecting ALSA mixer controls..."
# Find the card number for ua_apollo
CARD_NUM=$(aplay -l 2>/dev/null | grep 'ua_apollo' | grep -o 'card [0-9]*' | head -1 | awk '{print $2}' || echo "")
ALSA_CONTROLS=""
if [ -n "$CARD_NUM" ]; then
    ALSA_CONTROLS=$(amixer -c "$CARD_NUM" scontrols 2>/dev/null || echo "not available")
fi

# ============================================================================
# Build JSON report
# ============================================================================
echo ""
echo "Building report..."

cat > "$OUTPUT" << ENDJSON
{
  "report_version": "1.0",
  "platform": "linux",
  "generated_at": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "system": {
    "kernel": "$KERNEL_VERSION",
    "distro": "$(cat /etc/os-release 2>/dev/null | grep '^PRETTY_NAME=' | cut -d= -f2 | tr -d '"' || echo "unknown")",
    "arch": "$(uname -m)"
  },
  "driver": {
    "version": "$MOD_VERSION",
    "srcversion": "$MOD_SRCVERSION"
  },
  "pci": {
    "slot": "$PCI_SLOT",
    "vendor_id": "$PCI_VENDOR",
    "device_id": "$PCI_DEVICE",
    "lspci_line": "$(echo "$UA_LINE" | sed 's/"/\\"/g')"
  },
  "device": {
    "type": "$DEVICE_TYPE",
    "dmesg_line": "$(echo "$DEVICE_NAME" | sed 's/"/\\"/g')"
  },
  "alsa": {
    "playback": "$(echo "$APLAY_OUT" | head -1 | sed 's/"/\\"/g')",
    "capture": "$(echo "$ARECORD_OUT" | head -1 | sed 's/"/\\"/g')",
    "play_channels": "$PLAY_CHANNELS",
    "rec_channels": "$REC_CHANNELS",
    "control_count": "$(echo "$ALSA_CONTROLS" | grep -c 'Simple mixer' || echo 0)"
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
ANSWER="n"
if [ -t 0 ]; then
    read -rp "Help improve Open Apollo — send this device report anonymously? [y/N] " ANSWER
else
    # Non-interactive (piped, SSH, etc.) — auto-send
    ANSWER="y"
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
fi

echo ""
echo "To also submit manually:"
echo "  1. Review the file — it contains only hardware identifiers, no personal data"
echo "  2. Go to: https://github.com/rolotrealanis98/open-apollo/issues/new?template=device-report.yml"
echo "  3. Attach the JSON file or paste its contents"
echo "  4. Add notes about what works and what doesn't"
echo ""
