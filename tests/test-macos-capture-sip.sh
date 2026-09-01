#!/bin/bash

set -uo pipefail

PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"

# Load capture helpers without running the capture workflow.
source <(sed '/^# SIP WARNING$/q' \
    "$PROJECT_DIR/tools/contribute/macos/capture.sh")

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
