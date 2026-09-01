#!/bin/bash
# capture.sh — Capture Apollo routing and config data on macOS using DTrace
#
# This script uses DTrace to capture read-only data from the Universal Audio
# driver on macOS. It makes ZERO writes to hardware. All output is saved
# locally. Optionally uploads the report to the Open Apollo API.
#
# IMPORTANT: Requires System Integrity Protection (SIP) to be disabled.
# See WHAT-THIS-DOES.md for full details on what this captures and why.
#
# Usage:
#   sudo ./capture.sh [--output path/to/capture.json]

set -euo pipefail

TELEMETRY_URL="https://open-apollo-api.rolotrealanis.workers.dev/captures"

GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[1;33m'
NC='\033[0m'

dtrace_allowed() {
    local sip_status="$1"

    if printf '%s\n' "$sip_status" | grep -q '^[[:space:]]*DTrace Restrictions:'; then
        printf '%s\n' "$sip_status" |
            grep -q '^[[:space:]]*DTrace Restrictions: disabled$'
        return
    fi

    printf '%s\n' "$sip_status" |
        grep -q '^System Integrity Protection status: disabled'
}

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
if ! dtrace_allowed "$SIP_STATUS"; then
    printf "${RED}DTrace restrictions are enabled. DTrace will not work.${NC}\n"
    echo ""
    echo "Disable DTrace restrictions first (see instructions above), then re-run."
    exit 1
fi
printf "${GREEN}DTrace restrictions: disabled (OK)${NC}\n"
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
CAPTURE_SECONDS=30
while [[ $# -gt 0 ]]; do
    case "$1" in
        --output)
            OUTPUT="$2"
            shift 2
            ;;
        --seconds)
            CAPTURE_SECONDS="$2"
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

# Resolve alongside this script so apollo-selectors.d is found regardless of cwd.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

TMPDIR=$(mktemp -d)
trap 'rm -rf "$TMPDIR"' EXIT

echo "This script will capture:"
echo "  - Device configuration (model, channels, sample rate)"
echo "  - Routing table data (SEL171 — how audio channels are mapped)"
echo "  - Register snapshot (read-only status registers)"
echo ""
echo "Capture will run for approximately ${CAPTURE_SECONDS} seconds."
echo "Make sure UA Console / UA Connect is running."
echo ""
echo "IMPORTANT — the routing and config payloads are sent when the device is"
echo "programmed, not continuously. If Console sits idle for the whole window,"
echo "the capture comes back empty. During the window, change the sample rate"
echo "in the Console footer bar, then change it back."
echo ""
echo "Do NOT quit Console to trigger a re-attach: dtrace is attached to the"
echo "mixer engine process, so quitting Console ends the capture."
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
echo "Running DTrace capture (${CAPTURE_SECONDS} seconds)..."

# The probe script lives beside this one so it can also be run standalone.
# See apollo-selectors.d for the selector list and the sizing constraints.
PROBE_D="$SCRIPT_DIR/apollo-selectors.d"
RAW_OUT="${OUTPUT%.json}-selectors.txt"

MIXER_PID=$(pgrep -f "UA Mixer Engine" | head -1 || true)

if [ -z "$MIXER_PID" ]; then
    printf "${YELLOW}UA Mixer Engine is not running — skipping DTrace capture.${NC}\n"
    echo "Start UA Console (which launches the mixer engine) and re-run."
    echo "# UA Mixer Engine not running at capture time" > "$RAW_OUT"
elif [ ! -f "$PROBE_D" ]; then
    printf "${YELLOW}Probe script not found at $PROBE_D — skipping DTrace capture.${NC}\n"
    echo "# apollo-selectors.d missing" > "$RAW_OUT"
else
    echo "Attaching to UA Mixer Engine (pid $MIXER_PID)..."

    # dtrace exits non-zero if the target dies or the user interrupts, and the
    # partial output is still worth keeping — don't let `set -e` discard it.
    # stderr goes to its own file: with -o taking the trace data, an attach
    # failure would otherwise scroll past unnoticed.
    # Write straight to the final location, NOT into TMPDIR: the EXIT trap
    # wipes TMPDIR, so a Ctrl+C on a slow or stuck run would otherwise destroy
    # the very data the run was for. switchrate keeps this file current.
    : > "$RAW_OUT"
    dtrace -s "$PROBE_D" -p "$MIXER_PID" \
        -o "$RAW_OUT" 2> "$TMPDIR/dtrace_stderr.txt" &
    DTRACE_PID=$!

    echo ""
    echo "  ----------------------------------------------------------------"
    echo "  NOW: change the SAMPLE RATE in the UA Console footer bar."
    echo "  Switch it (e.g. 44.1k -> 48k), wait a few seconds, switch back."
    echo ""
    echo "  That reprograms the device, which is what emits the payloads."
    echo "  (Buffer size is a per-DAW setting on macOS, not a Console one.)"
    echo ""
    echo "  Do NOT quit Console. dtrace is attached to this exact process —"
    echo "  quitting it ends the capture instead of triggering one."
    echo "  ----------------------------------------------------------------"
    echo ""

    # Tick once a second: 45 seconds of silence is indistinguishable from a
    # hang, and bail immediately if dtrace dies rather than waiting it out.
    ELAPSED=0
    while [ "$ELAPSED" -lt "$CAPTURE_SECONDS" ]; do
        if ! kill -0 "$DTRACE_PID" 2>/dev/null; then
            printf "\n${YELLOW}dtrace exited on its own after ${ELAPSED}s.${NC}\n"
            break
        fi
        RECS=$(grep -c "^=== SEL" "$RAW_OUT" 2>/dev/null) || RECS=0
        printf "\r  %3ds / %ds — %s records " "$ELAPSED" "$CAPTURE_SECONDS" "$RECS"
        sleep 1
        ELAPSED=$((ELAPSED + 1))
    done
    printf "\n"

    # SIGINT so dtrace runs its END clause and flushes the buffer. It does not
    # always act on it -- detaching from the traced process can block -- so
    # escalate on a timer instead of blocking forever in `wait`. Records
    # already on disk are safe either way, since switchrate drains as we go.
    echo "Stopping capture..."
    kill -INT "$DTRACE_PID" 2>/dev/null || true

    WAITED=0
    while kill -0 "$DTRACE_PID" 2>/dev/null && [ "$WAITED" -lt 15 ]; do
        sleep 1
        WAITED=$((WAITED + 1))
    done

    if kill -0 "$DTRACE_PID" 2>/dev/null; then
        printf "${YELLOW}dtrace ignored SIGINT after 15s — sending TERM.${NC}\n"
        kill -TERM "$DTRACE_PID" 2>/dev/null || true
        WAITED=0
        while kill -0 "$DTRACE_PID" 2>/dev/null && [ "$WAITED" -lt 5 ]; do
            sleep 1
            WAITED=$((WAITED + 1))
        done
    fi

    if kill -0 "$DTRACE_PID" 2>/dev/null; then
        printf "${YELLOW}Still running — sending KILL.${NC}\n"
        kill -KILL "$DTRACE_PID" 2>/dev/null || true
    fi

    wait "$DTRACE_PID" 2>/dev/null || true

    if [ -s "$TMPDIR/dtrace_stderr.txt" ]; then
        printf "${YELLOW}dtrace reported:${NC}\n"
        sed 's/^/    /' "$TMPDIR/dtrace_stderr.txt"
        echo ""
    fi

    if grep -q "^=== SEL" "$RAW_OUT" 2>/dev/null; then
        SEL_COUNT=$(grep -c "^=== SEL" "$RAW_OUT") || SEL_COUNT=0
        printf "${GREEN}Captured $SEL_COUNT selector records.${NC}\n"
        if grep -q "OVERSIZE-NOT-DUMPED" "$RAW_OUT" 2>/dev/null; then
            printf "${YELLOW}Some payloads exceeded the dump limit — see the raw file.${NC}\n"
        fi
    else
        printf "${YELLOW}No selector traffic captured.${NC}\n"
        echo "Console was most likely idle. Re-run and relaunch UA Console"
        echo "during the capture window."
    fi

    # The hexdumps stay in this sibling file rather than being inlined in the
    # JSON: they run to hundreds of KB, and the JSON is what gets uploaded.
fi

echo "DTrace capture complete."
echo ""

# ============================================================================
# Build JSON output
# ============================================================================
echo "Building report..."

IOREG_CONTENT=$(cat "$TMPDIR/ioregistry.txt" 2>/dev/null | head -50 | sed 's/"/\\"/g' | tr '\n' ' ' || echo "not available")

# Per-selector summary. The hexdumps themselves stay in the sibling raw file —
# the JSON records only what was seen and how big it was, so the upload stays
# small and reviewable.
SEL_SUMMARY=$(awk '
    /^=== SEL/ {
        sel = $2; dir = $3; size = $4
        sub(/^SEL/, "", sel); sub(/^dir=/, "", dir); sub(/^size=/, "", size)
        key = sel "/" dir
        count[key]++
        if (size + 0 > max[key]) max[key] = size + 0
    }
    END {
        first = 1
        for (k in count) {
            split(k, p, "/")
            if (!first) printf ",\n"
            printf "    {\"selector\": %s, \"direction\": \"%s\", \"calls\": %d, \"max_bytes\": %d}",
                   p[1], p[2], count[k], max[k]
            first = 0
        }
    }
' "$RAW_OUT" 2>/dev/null || true)

if [ -z "$SEL_SUMMARY" ]; then
    ROUTING_STATUS="no selector traffic captured — was UA Console relaunched during the window?"
else
    ROUTING_STATUS="captured"
fi

# Only name the raw file in the report if it actually got written.
if [ -f "$RAW_OUT" ]; then
    RAW_BASENAME="$(basename "$RAW_OUT")"
else
    RAW_BASENAME=""
fi

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
    "status": "$ROUTING_STATUS",
    "capture_seconds": $CAPTURE_SECONDS,
    "raw_file": "$RAW_BASENAME",
    "notes": "Hexdumps are in the sibling *-selectors.txt file, not inlined here. See WHAT-THIS-DOES.md.",
    "selectors": [
$SEL_SUMMARY
    ]
  }
}
ENDJSON

# ============================================================================
# Done
# ============================================================================
echo ""
printf "${GREEN}Capture saved to: ${OUTPUT}${NC}\n"
if [ -n "$RAW_BASENAME" ]; then
    printf "${GREEN}Raw selector dumps: ${RAW_OUT}${NC}\n"
fi
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
echo "  1. Review the files — they contain only hardware identifiers and driver"
echo "     payloads, no personal data"
echo "  2. Go to: https://github.com/open-apollo/open-apollo/issues/new?template=device-report.yml"
echo "  3. Attach the JSON file, and the *-selectors.txt file if present —"
echo "     the raw dumps are the part that is actually useful for routing work"
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
