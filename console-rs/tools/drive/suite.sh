#!/usr/bin/env bash
# Runs every drive script in scripts/ against the recording fake daemon.
#   tools/drive/suite.sh [UACONSOLE]   (default: $UACONSOLE or console-rs/target/debug/uaconsole)
set -u
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" >/dev/null && pwd)"
BIN="${1:-${UACONSOLE:-$HERE/../../target/debug/uaconsole}}"
fail=0
for s in "$HERE"/scripts/*.txt; do
    extra=()
    grep -q "run.py --catalogue" "$s" && extra=(--catalogue)
    if python3 "$HERE/run.py" "$s" --uaconsole "$BIN" "${extra[@]}" >/tmp/drive-$$.log 2>&1; then
        echo "ok    $(basename "$s")"
    else
        echo "FAIL  $(basename "$s")"; cat /tmp/drive-$$.log; fail=1
    fi
done
rm -f /tmp/drive-$$.log
exit $fail
