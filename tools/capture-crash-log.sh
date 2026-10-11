#!/bin/bash
# capture-crash-log.sh — Stream Apollo kernel logs from Ubuntu to local file
# Run from another machine: HOST=user@machine bash tools/capture-crash-log.sh
# Then reproduce the crash on Ubuntu. Logs save locally even if machine hangs.

# Usage: HOST=user@machine bash tools/capture-crash-log.sh
HOST="${HOST:?set HOST=user@machine (the Linux box with the Apollo)}"
LOG_DIR="${LOG_DIR:-$(dirname "$0")/captures/crash-logs}"
TIMESTAMP=$(date +%Y%m%d-%H%M%S)
LOG_FILE="$LOG_DIR/crash-$TIMESTAMP.log"

mkdir -p "$LOG_DIR"

echo "=== Apollo Crash Logger ==="
echo "Logging to: $LOG_FILE"
echo "Reproduce the crash now. Ctrl+C to stop."
echo ""

# Stream dmesg -w (kernel log follow) via SSH, tee to local file
# sudo asks for the remote password on the terminal (-t)
ssh -t "$HOST" "sudo dmesg -w" | tee "$LOG_FILE"
