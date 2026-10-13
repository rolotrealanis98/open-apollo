#!/bin/sh
# Read-only observer. No app launches, GUI input, CLI requests or SIP changes.
set -eu
if [ "$#" -lt 2 ] || [ "$#" -gt 3 ]; then
    echo "Usage: sh run_mac_cli_capture.sh PID NEW_ABSOLUTE_CAPTURE_DIR [text|packets]" >&2
    exit 2
fi
case "$1" in ''|*[!0-9]*) echo 'PID must be numeric' >&2; exit 2;; esac
[ "$(uname -s)" = Darwin ] && [ "$(uname -m)" = arm64 ] || {
    echo 'Requires the inspected arm64 Mac installation' >&2; exit 2;
}
case "$2" in /tmp/*|"$HOME/Library/Caches/"*) ;; *)
    echo 'Capture directory must be outside git, under /tmp or ~/Library/Caches' >&2; exit 2;;
esac
MODE=${3:-text}
case "$MODE" in text) TRACE_FILE=cli_text_calls.d;; packets) TRACE_FILE=cli_calls.d;;
    *) echo 'Mode must be text or packets' >&2; exit 2;; esac
TASK_PID=$1
CAPTURE_DIR=$2
SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
FRAMEWORK='/Library/Frameworks/UAD-2 Plugin Support.framework/Versions/A/UAD-2 Plugin Support'
EXPECTED_HASH=edcce3b1012b507c8052ed62e6855a9d1cb34055015fbbd8abb95364d660bd24
ACTUAL_HASH=$(shasum -a 256 "$FRAMEWORK" | awk '{print $1}')
[ "$ACTUAL_HASH" = "$EXPECTED_HASH" ] || { echo 'Framework pin mismatch' >&2; exit 2; }
DRIVER_INFO=$(ioreg -r -c com_uaudio_driver_UAD2System)
printf '%s\n' "$DRIVER_INFO" | grep -q '"BuildVersionInfo" = "12.0.0 build 464"' || {
    echo 'Unverified driver version' >&2; exit 2;
}
PROCESS=$(ps -p "$TASK_PID" -o comm=)
PROCESS_IDENTITY=$(ps -p "$TASK_PID" -o pid=,lstart=,comm=)
case "$PROCESS" in
    '/Library/Application Support/Universal Audio/Apollo/UA Mixer Engine.app/Contents/MacOS/UA Mixer Engine'|\
    '/Applications/Universal Audio/UAD Console.app/Contents/MacOS/UAD Console') ;;
    *) echo 'PID is not an inspected UA engine/Console executable' >&2; exit 2;;
esac
sudo -n true || { echo 'sudo access required; authenticate separately' >&2; exit 2; }
CAPTURE_PARENT=$(CDPATH= cd -- "$(dirname -- "$CAPTURE_DIR")" && pwd -P)
case "$CAPTURE_PARENT" in /private/tmp|/private/tmp/*|/tmp|/tmp/*|"$HOME/Library/Caches"|"$HOME/Library/Caches/"*) ;;
    *) echo 'Resolved capture parent is outside the allowed scratch locations' >&2; exit 2;; esac
CAPTURE_DIR=$CAPTURE_PARENT/$(basename -- "$CAPTURE_DIR")
umask 077
mkdir "$CAPTURE_DIR"  # refuse overwrite; missing parent is also an error
{
    date -u
    sw_vers
    uname -m
    csrutil status
    printf 'framework_sha256=%s\nprocess=%s\nmode=%s\n' "$ACTUAL_HASH" "$PROCESS" "$MODE"
    ps -p "$TASK_PID" -o pid=,lstart=,comm=
} > "$CAPTURE_DIR/metadata.txt" 2>&1
printf '%s\n' "$DRIVER_INFO" > "$CAPTURE_DIR/driver-registry.txt"
# List exact probes first: no -Z, so absent symbols cannot silently succeed.
if ! sudo -n dtrace -l -p "$TASK_PID" \
    -n 'pid$target::IOConnectCallStructMethod:entry' \
    -n 'pid$target::IOConnectCallStructMethod:return' \
    > "$CAPTURE_DIR/probes.txt" 2> "$CAPTURE_DIR/probes-diagnostics.txt"; then
    echo "Probe attachment failed; see $CAPTURE_DIR/probes-diagnostics.txt" >&2
    exit 3
fi
# Recheck identity just before attachment. Start times are retained for review.
[ "$(ps -p "$TASK_PID" -o pid=,lstart=,comm=)" = "$PROCESS_IDENTITY" ] || exit 3
RESULT=0
sudo -n dtrace -q -p "$TASK_PID" -s "$SCRIPT_DIR/$TRACE_FILE" \
    > "$CAPTURE_DIR/trace.txt" 2> "$CAPTURE_DIR/diagnostics.txt" || RESULT=$?
printf 'dtrace_exit=%s\n' "$RESULT" > "$CAPTURE_DIR/result.txt"
if [ "$RESULT" -ne 0 ] || ! grep -q '^TRACE_READY ' "$CAPTURE_DIR/trace.txt" \
    || ! grep -q '^TRACE_END .*faults=0$' "$CAPTURE_DIR/trace.txt" \
    || grep -Ei 'error|failed|drop' "$CAPTURE_DIR/diagnostics.txt" >/dev/null; then
    echo "Capture incomplete or faulted; inspect $CAPTURE_DIR" >&2
    exit 4
fi
printf 'Capture finished; inspect %s. Zero calls only describe this ten-second window.\n' "$CAPTURE_DIR"
