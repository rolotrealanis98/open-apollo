#!/usr/bin/env bash
# test-install-yes.sh — unit test for install.sh --yes (the setup GUI's
# non-interactive mode): the flag parses, and the telemetry step never
# uploads under --yes even though stdin is not a terminal (without --yes a
# non-terminal stdin auto-sends).  Sourcing install.sh only loads functions;
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
parse_args --yes --no-dkms
check "--yes sets ASSUME_YES" test "$ASSUME_YES" = "1"
check "--yes combines with other flags" test "$NO_DKMS" = "1"
ASSUME_YES=0
parse_args -y
check "-y is --yes" test "$ASSUME_YES" = "1"

telemetry_prompt < /dev/null > "$tmp_dir/out" 2>&1
check "--yes never uploads the report" test ! -e "$tmp_dir/curl"
check "--yes says the report stayed local" grep -q "No data sent" "$tmp_dir/out"

if [ "$failures" -ne 0 ]; then
    echo "$failures check(s) failed"
    exit 1
fi
echo "All checks passed"
