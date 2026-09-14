#!/bin/bash

set -uo pipefail

PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"

# shellcheck source=tools/contribute/macos/capture-lib.sh
source "$PROJECT_DIR/tools/contribute/macos/capture-lib.sh"
case $- in
    *e*) echo "FAIL: capture helpers enabled errexit"; exit 1 ;;
esac
# A denied result in an assignment must not terminate this test shell.
result=$(dtrace_allowed 'System Integrity Protection status: enabled.')
rc=$?
[[ $rc -ne 0 && -z $result ]] || { echo "FAIL: enabled protection accepted"; exit 1; }

fully_disabled='System Integrity Protection status: disabled.'
custom_allowed='System Integrity Protection status: unknown (Custom Configuration).

Configuration:
	Kext Signing: enabled
	DTrace Restrictions: disabled'
custom_blocked='System Integrity Protection status: unknown (Custom Configuration).

Configuration:
	Filesystem Protections: disabled
	DTrace Restrictions: enabled'

if ! dtrace_allowed "$fully_disabled"; then
	echo "FAIL: fully disabled protection was rejected"
	exit 1
fi

if ! dtrace_allowed "$custom_allowed"; then
	echo "FAIL: custom configuration with DTrace allowed was rejected"
	exit 1
fi

if dtrace_allowed "$custom_blocked"; then
	echo "FAIL: custom configuration with DTrace blocked was accepted"
	exit 1
fi

echo "PASS: capture checks the DTrace restriction specifically"
