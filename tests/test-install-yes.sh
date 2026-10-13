#!/usr/bin/env bash
# test-install-yes.sh — unit test for install.sh --yes (the setup GUI's
# non-interactive mode) and the opt-in install report: the flags parse, and
# the report is uploaded only with --send-report (stdin is not a terminal
# here, which never means yes).  Sourcing install.sh only loads functions;
# curl is stubbed, so nothing touches the network or needs root.

set -uo pipefail

PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
set --
# shellcheck source=scripts/install.sh
source "$PROJECT_DIR/scripts/install.sh"

tmp_dir=$(mktemp -d)
trap 'rm -rf "$tmp_dir"' EXIT
REPORT_FILE="$tmp_dir/report.json"
echo '{}' > "$REPORT_FILE"
curl() { echo called >> "$tmp_dir/curl"; echo 200; }

failures=0
check() {
    local desc="$1"; shift
    if "$@"; then
        echo "PASS: $desc"
    else
        echo "FAIL: $desc"
        failures=$((failures + 1))
    fi
}

check "sourcing leaves --yes off" test "$ASSUME_YES" = "0"
check "sourcing leaves --send-report off" test "$SEND_REPORT" = "0"
parse_args --yes --no-dkms
check "--yes sets ASSUME_YES" test "$ASSUME_YES" = "1"
check "--yes combines with other flags" test "$NO_DKMS" = "1"
ASSUME_YES=0
parse_args -y
check "-y is --yes" test "$ASSUME_YES" = "1"

telemetry_prompt < /dev/null > "$tmp_dir/out" 2>&1
check "--yes never uploads the report" test ! -e "$tmp_dir/curl"
check "--yes says the report stayed local" grep -q "No data sent" "$tmp_dir/out"

ASSUME_YES=0
SEND_REPORT=0
telemetry_prompt < /dev/null > "$tmp_dir/out" 2>&1
check "no terminal is not consent: nothing uploaded" test ! -e "$tmp_dir/curl"
check "no terminal: says how to send it" grep -q -- "--send-report" "$tmp_dir/out"

parse_args --yes --send-report
check "--send-report sets SEND_REPORT" test "$SEND_REPORT" = "1"
telemetry_prompt < /dev/null > "$tmp_dir/out" 2>&1
check "--send-report uploads, with --yes too" test -e "$tmp_dir/curl"
check "--send-report reports the upload" grep -q "Report sent" "$tmp_dir/out"
rm -f "$tmp_dir/curl"

if [ "$failures" -ne 0 ]; then
    echo "$failures check(s) failed"
    exit 1
fi
echo "All checks passed"
