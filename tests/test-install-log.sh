#!/usr/bin/env bash
# test-install-log.sh — every installer run leaves a redacted log, and the
# opt-in report's plug-in part holds nothing but authorized render ids.
#
#   1. scripts/oa-log.sh: a script's output goes on to where it went and,
#      redacted and without colour codes, to ~/.local/state/open-apollo
#      (XDG_STATE_HOME here); children log into the parent's file;
#      OA_INSTALL_LOG appends to a given file; OA_NO_LOG turns it off.
#   2. install.sh's add_plugin_report (after the user agreed to send the
#      report): runs mixer-engine/plugin_report.py only then, keeps only what
#      plugin_report.sanitize allows, and nothing with --no-plugin-report.
# Nothing needs root, a device or the network (curl is stubbed).

set -uo pipefail

PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tmp_dir=$(mktemp -d)
trap 'rm -rf "$tmp_dir"' EXIT

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
has()   { grep -qF -- "$2" "$1"; }
hasnt() { ! grep -qF -- "$2" "$1"; }

me=$(id -un)
home=$(python3 -c 'import os, pwd; print(pwd.getpwuid(os.getuid()).pw_dir)')
state="$tmp_dir/state"
logdir="$state/open-apollo"

# ---- 1. oa-log.sh ----

cat > "$tmp_dir/child.sh" <<EOF
#!/bin/bash
. "$PROJECT_DIR/scripts/oa-log.sh"
oa_log_start child ""
echo "child line from $home, serial: 20000000000123"
EOF
cat > "$tmp_dir/demo.sh" <<EOF
#!/bin/bash
. "$PROJECT_DIR/scripts/oa-log.sh"
oa_log_start demo "" --from "$home/win"
trap 'oa_log_finish "Log"' EXIT
printf '\033[0;32m[ OK ]\033[0m  Mixer daemon running as %s\n' "$me"
echo "to stderr: $home/.local/share" >&2
bash "$tmp_dir/child.sh"
exit 3
EOF
run_demo() {
    env -u OA_LOG_CAPTURED -u OA_INSTALL_LOG XDG_STATE_HOME="$state" "$@" bash "$tmp_dir/demo.sh" \
        > "$tmp_dir/out" 2>&1
}

run_demo
check "the script's exit status is kept" test $? = 3
check "one log per run" test "$(find "$logdir" -type f | wc -l | tr -d ' ')" = 1
log=$(find "$logdir" -name 'demo-*.log' | head -1)
check "the log is named after the script and the time" test -n "$log"
check "the log starts with what ran" has "$log" "open-apollo demo log: "
check "the log has stdout" has "$log" "[ OK ]  Mixer daemon running as <user>"
check "the log has stderr" has "$log" "to stderr: <home>/.local/share"
check "a child script logs into the same file" has "$log" "child line from <home>, serial: <redacted>"
check "a child script makes no log of its own" test -z "$(find "$logdir" -name 'child-*')"
check "no user name in the log" test -z "$(grep -w -- "$me" "$log")"
check "no home path in the log" hasnt "$log" "$home"
check "no serial in the log" hasnt "$log" "20000000000123"
check "no colour codes in the log" test -z "$(grep -l $'\x1b' "$log")"
check "the output is unchanged" has "$tmp_dir/out" "$(printf '\033[0;32m[ OK ]\033[0m')"
check "the output names the log" has "$tmp_dir/out" "Log: $log"
check "only the user can read it" test "$(stat -c %a "$log" 2>/dev/null || stat -f %Lp "$log")" = 600

given="$tmp_dir/wizard.log"
echo "the wizard's own line" > "$given"
rm -rf "$logdir"
run_demo OA_INSTALL_LOG="$given"
check "OA_INSTALL_LOG appends to that file" has "$given" "child line from <home>"
check "OA_INSTALL_LOG keeps what was there" has "$given" "the wizard's own line"
check "OA_INSTALL_LOG starts no other log" test ! -e "$logdir"
rm -rf "$logdir"
run_demo OA_NO_LOG=1
check "OA_NO_LOG=1 logs nothing" test ! -e "$logdir"
check "OA_NO_LOG=1 still shows the output" has "$tmp_dir/out" "child line from"

ln -s "$tmp_dir/elsewhere" "$tmp_dir/link.log"
run_demo OA_INSTALL_LOG="$tmp_dir/link.log"
check "the log is never written through a link" test ! -e "$tmp_dir/elsewhere"
check "a log that can't be written doesn't stop the script" has "$tmp_dir/out" "child line from"

# ---- 2. the plug-in part of the install report ----

set --
# shellcheck source=scripts/install.sh
source "$PROJECT_DIR/scripts/install.sh"
curl() { echo 200; }
info() { echo "[INFO]  $*"; }
REPORT_FILE="$tmp_dir/report.json"

# A plugin_report.py that answers with everything it shouldn't, next to the
# real sanitize().
fake="$tmp_dir/fake"
mkdir -p "$fake/mixer-engine"
cat > "$fake/mixer-engine/plugin_report.py" <<EOF
import importlib.util, json, sys
spec = importlib.util.spec_from_file_location("real_report", "$PROJECT_DIR/mixer-engine/plugin_report.py")
real = importlib.util.module_from_spec(spec)
sys.modules["real_report"] = real
spec.loader.exec_module(real)
sanitize = real.sanitize
if __name__ == "__main__":
    open("$tmp_dir/queried", "a").write(" ".join(sys.argv[1:]) + "\n")
    print(json.dumps({"schema": "x", "available": True, "serial": "2000000000123",
                      "authorized": [2276, 2100, "2300", 99, 3000, True],
                      "native": {"built_mono": [2276, 2400], "failed": [2100], "names": ["UAD Neve 1084"]},
                      "demo_days": {"2200": 13}, "account": "someone@example.com",
                      "host": "$(hostname)"}))
EOF
PROJECT_DIR="$fake"
SUDO_USER="$me"
report_run() {
    rm -f "$tmp_dir/queried"
    echo '{"version": 2, "success": true}' > "$REPORT_FILE"
    telemetry_prompt > "$tmp_dir/out" 2>&1
}

ASSUME_YES=1 SEND_REPORT=1 PLUGIN_REPORT=1
declare -A STEP_STATUS=([init]=ok)
report_run
check "a sent report asks for the plug-ins" test -e "$tmp_dir/queried"
check "it reads this user's bundles" has "$tmp_dir/queried" "--bundles $home/.local/share/open-apollo/plugin-bundles"
check "the report has exactly the documented keys" test "$(python3 -c '
import json, sys
d = json.load(open(sys.argv[1]))["plugins"]
print(sorted(d), sorted(d["native"]))' "$REPORT_FILE")" = "['authorized', 'available', 'native', 'schema'] ['built_mono', 'built_stereo', 'failed']"
check "only valid authorized render ids" test "$(python3 -c '
import json, sys; print(json.load(open(sys.argv[1]))["plugins"]["authorized"])' "$REPORT_FILE")" = "[2100, 2276]"
check "native ids must be authorized" test "$(python3 -c '
import json, sys; print(json.load(open(sys.argv[1]))["plugins"]["native"])' "$REPORT_FILE")" = "{'built_mono': [2276], 'built_stereo': [], 'failed': [2100]}"
for secret in 2000000000123 "UAD Neve" demo_days 13 someone@example.com "$(hostname)" serial account; do
    check "nothing else reaches the report: $secret" hasnt "$REPORT_FILE" "$secret"
done
check "the output says how many" has "$tmp_dir/out" "Authorized plug-ins in the report: 2 (render ids only)"

PLUGIN_REPORT=0
report_run
check "--no-plugin-report asks nothing" test ! -e "$tmp_dir/queried"
check "--no-plugin-report leaves them out" hasnt "$REPORT_FILE" '"plugins"'
check "--no-plugin-report says so" has "$tmp_dir/out" "left out of the report (--no-plugin-report)"

PLUGIN_REPORT=1 SEND_REPORT=0
report_run
check "without consent nothing is asked" test ! -e "$tmp_dir/queried"
check "without consent the local report has no plug-ins" hasnt "$REPORT_FILE" '"plugins"'

SEND_REPORT=1
STEP_STATUS[init]=skipped
report_run
check "without a driver nothing is asked" test ! -e "$tmp_dir/queried"
check "without a driver the report says why" test "$(python3 -c '
import json, sys; print(json.load(open(sys.argv[1]))["plugins"])' "$REPORT_FILE")" = "{'schema': 'open-apollo-plugin-report/1', 'available': False, 'reason': 'no_device'}"

parse_args --send-report --no-plugin-report
check "--no-plugin-report parses" test "$PLUGIN_REPORT" = 0

echo
if [ "$failures" -eq 0 ]; then
    echo "All install log checks passed."
else
    echo "$failures check(s) failed."
    exit 1
fi
