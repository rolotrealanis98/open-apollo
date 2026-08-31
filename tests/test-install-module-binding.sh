#!/bin/bash

set -uo pipefail

PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"

# Load installer functions without running the installer main routine.
source <(sed '/^# Main$/q' "$PROJECT_DIR/scripts/install.sh")

tmp_dir=$(mktemp -d)
trap 'rm -rf "$tmp_dir"' EXIT

mkdir -p "$tmp_dir/drivers/ua_apollo"
ln -s "$tmp_dir/drivers/ua_apollo" "$tmp_dir/bound"

if apollo_driver_bound "$tmp_dir/unbound"; then
	echo "FAIL: missing driver link accepted as bound"
	exit 1
fi

if ! apollo_driver_bound "$tmp_dir/bound"; then
	echo "FAIL: ua_apollo driver link rejected as unbound"
	exit 1
fi

echo "PASS: installer distinguishes bound and unbound modules"
