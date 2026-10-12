#!/usr/bin/env bash
# Unit tests for distro-specific dependency selection in scripts/install.sh.

set -uo pipefail

PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
set -- --skip-init
# shellcheck source=scripts/install.sh
source "$PROJECT_DIR/scripts/install.sh"

failures=0
check_equal() {
    local description="$1" expected="$2" actual="$3"
    if [ "$actual" = "$expected" ]; then
        echo "PASS: $description"
    else
        echo "FAIL: $description (expected '$expected', got '$actual')"
        failures=$((failures + 1))
    fi
}
check_equal "uses the supported Ayatana package on apt distributions" \
    "gir1.2-ayatanaappindicator3-0.1" "$(apt_appindicator_package)"

# The installer asks Python which GI namespace is actually importable. Stub
# that one probe to exercise both supported namespaces without requiring GI in
# the test environment.
FAKE_GI_NAMESPACE=""
python3() {
    local last="${!#}"
    [ "$last" = "$FAKE_GI_NAMESPACE" ]
}

FAKE_GI_NAMESPACE="AyatanaAppIndicator3"
check_equal "prefers the Ayatana GI namespace when present" \
    "AyatanaAppIndicator3" "$(appindicator_namespace)"

FAKE_GI_NAMESPACE="AppIndicator3"
check_equal "falls back to the legacy GI namespace" \
    "AppIndicator3" "$(appindicator_namespace)"

if [ "$failures" -ne 0 ]; then
    echo "$failures check(s) failed"
    exit 1
fi
echo "PASS: apt AppIndicator dependency selection"
echo "# tests 3"
echo "# skipped 0"
