#!/bin/bash
# Exercise dispatch and delegation without loading or unloading hardware.
set -uo pipefail
PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
# shellcheck source=scripts/dev-module.sh
source "$PROJECT_DIR/scripts/dev-module.sh"

fail() { echo "FAIL: $*"; exit 1; }
require_root() { echo root-check; }
do_trace() { echo trace-called; }
do_release() { echo release-called; }

for cmd in trace release; do
    output=$(main "$cmd") && fail "$cmd accepted without acknowledgement"
    [[ $output == *'NEVER rmmod ua_apollo'* && $output != *root-check* ]] || fail "$cmd reached execution before gate"
    output=$(main "$cmd" --allow-unsafe-unload) || fail "$cmd rejected acknowledgement"
    [[ $output == *"$cmd-called"* ]] || fail "$cmd not dispatched"
    output=$(main "$cmd" --allow-unsafe-unload extra) && fail "$cmd accepted extra arguments"
done

# Autoload cases use mocked commands, never host configuration changes.
(
    grep() { return 1; }
    find() { return 0; }
    dkms() { return 0; }
    check_autoload >/dev/null || fail "clean manual workflow rejected"
    grep() { echo 'ua_apollo fixture reference'; return 0; }
    check_autoload >/dev/null && fail "autoload references missed"
    exit 0
) || exit 1

# bound_devices lists only PCI addresses, never the driver's module symlink.
(
    fixture=$(mktemp -d)
    trap 'rm -rf "$fixture"' EXIT
    mkdir -p "$fixture/drivers/ua_apollo"
    ln -s /nonexistent "$fixture/drivers/ua_apollo/module"
    output=$(find() { command find "$fixture/drivers/ua_apollo" "${@:2}"; }; bound_devices)
    [[ -z $output ]] || fail "module symlink reported as a bound device: $output"
    ln -s /nonexistent "$fixture/drivers/ua_apollo/0000:3e:00.0"
    output=$(find() { command find "$fixture/drivers/ua_apollo" "${@:2}"; }; bound_devices)
    [[ $output == '0000:3e:00.0' ]] || fail "bound device not listed: $output"
    exit 0
) || exit 1

# Verify claim uses the initializer, forwards force, warns and propagates failure.
check_autoload() { return 1; }
bash() { printf 'delegate: %s\nskip-pipewire: %s\n' "$*" "${APOLLO_SKIP_PIPEWIRE:-unset}"; return 17; }
output=$(main claim --force)
rc=$?
[[ $rc == 17 ]] || fail "initializer failure not propagated"
[[ $output == *'host can claim the Apollo automatically'* ]] || fail "autoload warning missing"
[[ $output == *"$PROJECT_DIR/tools/apollo-init.sh --no-daemon --force"* ]] || fail "incorrect initializer arguments"
[[ $output == *'skip-pipewire: 1'* ]] || fail "claim allows audio-session changes"
output=$(main claim twinx_dsp=1) && fail "raw module arguments accepted"
echo "PASS: unsafe unload gate, autoload detection, bound-device listing, claim delegation and failure propagation"
