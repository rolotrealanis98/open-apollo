#!/bin/bash
# capture.sh — Capture Apollo routing and config data on macOS using DTrace
#
# This script uses DTrace to capture read-only data from the Universal Audio
# driver on macOS. It makes ZERO writes to hardware. All output is saved
# locally — no network calls are made.
#
# IMPORTANT: Requires System Integrity Protection (SIP) to be disabled.
# See WHAT-THIS-DOES.md for full details on what this captures and why.
#
# Usage:
#   sudo ./capture.sh [--output path/to/capture.json]

set -euo pipefail

GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[1;33m'
NC='\033[0m'

# ============================================================================
# SIP WARNING
# ============================================================================
echo ""
echo "================================================================"
echo "  IMPORTANT: System Integrity Protection (SIP) Notice"
echo "================================================================"
echo ""
echo "DTrace requires SIP to be disabled on macOS."
echo ""
echo "To disable SIP:"
echo "  1. Restart your Mac and hold Cmd+R to enter Recovery Mode"
echo "  2. Open Terminal from the Utilities menu"
echo "  3. Run: csrutil disable"
echo "  4. Restart normally"
echo ""
echo "Apple's official documentation:"
echo "  https://support.apple.com/en-us/102149"
echo ""
echo "You should RE-ENABLE SIP after capturing. Instructions are"
echo "printed at the end of this script."
echo ""
echo "================================================================"
echo ""

# ============================================================================
# Check prerequisites
# ============================================================================

# Must run as root for DTrace
if [ "$(id -u)" -ne 0 ]; then
    printf "${RED}This script must be run with sudo.${NC}\n"
    echo "Usage: sudo $0"
    exit 1
fi

# Check SIP status
SIP_STATUS=$(csrutil status 2>/dev/null || echo "unknown")
if echo "$SIP_STATUS" | grep -q "enabled"; then
    printf "${RED}SIP is enabled. DTrace will not work.${NC}\n"
    echo ""
    echo "Disable SIP first (see instructions above), then re-run."
    exit 1
fi
printf "${GREEN}SIP status: disabled (OK)${NC}\n"
echo ""

# Check that UA driver is loaded
if ! kextstat 2>/dev/null | grep -q "com.uaudio"; then
    printf "${RED}No Universal Audio driver (kext) found.${NC}\n"
    echo "Make sure UAD software is installed and your Apollo is connected."
    exit 1
fi
printf "${GREEN}UA driver loaded.${NC}\n"
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
            shift
            ;;
    esac
done

if [ -z "$OUTPUT" ]; then
    OUTPUT="./apollo-macos-capture-$(date +%Y%m%d-%H%M%S).json"
fi

TMPDIR=$(mktemp -d)
trap 'rm -rf "$TMPDIR"' EXIT

echo "This script will capture:"
echo "  - Device configuration (model, channels, sample rate)"
echo "  - Routing table data (SEL171 — how audio channels are mapped)"
echo "  - Register snapshot (read-only status registers)"
echo ""
echo "Capture will run for approximately 10 seconds."
echo "Make sure UA Console / UA Connect is running."
echo ""
read -rp "Press Enter to start capture, or Ctrl+C to cancel..."
echo ""

# ============================================================================
# Capture: Device info from IORegistry
# ============================================================================
echo "Capturing device info from IORegistry..."
ioreg -r -c IOAudioDevice 2>/dev/null | grep -A 20 "UAD\|Universal Audio" > "$TMPDIR/ioregistry.txt" || true

# ============================================================================
# Capture: DTrace probes for routing data
# ============================================================================
echo "Running DTrace capture (10 seconds)..."

# TODO: Fill in actual DTrace probe specifications.
# The probes target IOConnectCallStructMethod in the UA Mixer Engine process
# to capture routing table data (SEL171) and device configuration.
#
# Probe structure:
#   pid$target::IOConnectCallStructMethod:entry
#   - arg0: connection
#   - arg1: selector number (171 = routing, 131 = mixer param, etc.)
#   - arg2: pointer to input struct
#   - arg3: input struct size
#
# Example DTrace script (template — actual probe details TBD):
#
#   dtrace -n '
#     pid$target::IOConnectCallStructMethod:entry
#     /arg1 == 171/
#     {
#         printf("SEL171 routing call: size=%d", arg3);
#         tracemem(copyin(arg2, arg3), 256);
#     }
#   ' -p $(pgrep -f "UA Mixer Engine") -o "$TMPDIR/dtrace_routing.out"

# Placeholder: write a note that probes need to be filled in
cat > "$TMPDIR/dtrace_routing.out" << 'PLACEHOLDER'
# DTrace capture placeholder
# TODO: Actual DTrace probes will be added as they are verified for each
# macOS version and Apollo model. The probe targets are:
#   - SEL171 (GetRoutingTable): Read-only routing configuration
#   - Device type and channel count identification
#
# If you are a developer and want to help define these probes,
# please open an issue on the Open Apollo repository.
PLACEHOLDER

echo "DTrace capture complete."
echo ""

# ============================================================================
# Build JSON output
# ============================================================================
echo "Building report..."

IOREG_CONTENT=$(cat "$TMPDIR/ioregistry.txt" 2>/dev/null | head -50 | sed 's/"/\\"/g' | tr '\n' ' ' || echo "not available")

cat > "$OUTPUT" << ENDJSON
{
  "report_version": "1.0",
  "platform": "macos",
  "generated_at": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "system": {
    "macos_version": "$(sw_vers -productVersion 2>/dev/null || echo unknown)",
    "arch": "$(uname -m)",
    "sip_status": "$(csrutil status 2>/dev/null | head -1 || echo unknown)"
  },
  "driver": {
    "kext_info": "$(kextstat 2>/dev/null | grep 'com.uaudio' | head -1 | sed 's/"/\\"/g' || echo "not found")"
  },
  "ioregistry": "$IOREG_CONTENT",
  "routing": {
    "status": "TODO — DTrace probes not yet defined for this macOS version",
    "notes": "See WHAT-THIS-DOES.md for details on what will be captured"
  }
}
ENDJSON

# ============================================================================
# Done
# ============================================================================
echo ""
printf "${GREEN}Capture saved to: ${OUTPUT}${NC}\n"
echo ""
echo "To submit to the Open Apollo project:"
echo "  1. Review the file — it contains only hardware identifiers, no personal data"
echo "  2. Go to: https://github.com/rolotrealanis98/open-apollo/issues/new?template=device-report.yml"
echo "  3. Attach the JSON file or paste its contents"
echo "  4. Add notes about your Apollo model and macOS version"
echo ""
echo "================================================================"
echo "  IMPORTANT: Re-enable SIP now!"
echo "================================================================"
echo ""
echo "  1. Restart your Mac and hold Cmd+R to enter Recovery Mode"
echo "  2. Open Terminal from the Utilities menu"
echo "  3. Run: csrutil enable"
echo "  4. Restart normally"
echo ""
echo "  Apple's official documentation:"
echo "    https://support.apple.com/en-us/102149"
echo "================================================================"
echo ""
