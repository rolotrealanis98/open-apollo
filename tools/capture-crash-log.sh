#!/bin/bash
# capture-crash-log.sh — Stream Apollo kernel logs from Ubuntu to local file
# Run from Mac: bash tools/capture-crash-log.sh
# Then reproduce the crash on Ubuntu. Logs save locally even if machine hangs.

HOST="dev@192.168.1.3"
LOG_DIR="/Users/rolo/Documents/Github/open-apollo/tools/captures/crash-logs"
TIMESTAMP=$(date +%Y%m%d-%H%M%S)
LOG_FILE="$LOG_DIR/crash-$TIMESTAMP.log"

mkdir -p "$LOG_DIR"

echo "=== Apollo Crash Logger ==="
echo "Logging to: $LOG_FILE"
echo "Reproduce the crash now. Ctrl+C to stop."
echo ""

# Stream dmesg -w (kernel log follow) via SSH, tee to local file
# Uses sudo on remote to access dmesg
ssh -t "$HOST" "echo 'O6TCB1k!' | sudo -S dmesg -w 2>/dev/null" 2>/dev/null | tee "$LOG_FILE"
