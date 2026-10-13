#!/usr/bin/env bash
# test-install-report-privacy.sh — the install report carries no identity:
# no user name, host name, home path, machine id or device serials, and of
# PipeWire only the Apollo's own objects.  pw-dump and dmesg are faked with
# such data; sourcing install.sh only loads functions, nothing needs root.

set -uo pipefail

PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
set --
# shellcheck source=scripts/install.sh
source "$PROJECT_DIR/scripts/install.sh"

tmp_dir=$(mktemp -d)
trap 'rm -rf "$tmp_dir"' EXIT
REPORT_FILE="$tmp_dir/report.json"
me=$(id -un)
host=$(hostname 2>/dev/null || cat /proc/sys/kernel/hostname)
home=$(getent passwd "$me" | cut -d: -f6)

# Fake tools: a pw-dump with another app's user/host and a headset's serial
# next to an Apollo node, and a driver log line with the unit's serial.
mkdir -p "$tmp_dir/bin"
cat > "$tmp_dir/bin/pw-dump" <<EOF
#!/bin/sh
cat <<'JSON'
[
 {"id": 40, "type": "PipeWire:Interface:Node", "info": {"props": {
   "node.name": "alsa_output.pci-0000_44_00.0.pro-output-0", "media.class": "Audio/Sink",
   "alsa.card_name": "Apollo x4", "audio.channels": 24, "object.serial": 77,
   "application.process.user": "$me", "application.process.host": "$host"}}},
 {"id": 41, "type": "PipeWire:Interface:Device", "info": {"props": {
   "device.name": "bluez_card.AA_BB_CC_DD_EE_FF", "media.class": "Audio/Device",
   "device.description": "Fake Headset XYZ", "device.serial": "SN-HEADSET-123456",
   "api.bluez5.address": "AA:BB:CC:DD:EE:FF"}}},
 {"id": 42, "type": "PipeWire:Interface:Client", "info": {"props": {
   "application.process.user": "$me", "application.process.host": "$host",
   "application.process.binary": "$home/bin/thing"}}}
]
JSON
EOF
cat > "$tmp_dir/bin/dmesg" <<EOF
#!/bin/sh
echo "ua_apollo 0000:44:00.0: serial: 20000000000123"
echo "ua_apollo 0000:44:00.0: firmware from $home/Downloads on $host"
EOF
chmod +x "$tmp_dir/bin/pw-dump" "$tmp_dir/bin/dmesg"
PATH="$tmp_dir/bin:$PATH"
sudo() { [ "$1" = -u ] && shift 2; env "$@"; }
SUDO_USER="$me"
DISTRO=test KERNEL=test ARCH=x86_64 RAM_MB=0 CPU_MODEL=test SECURE_BOOT=false IOMMU=none
PA_RUNNING=false EXISTING_CARDS="" SND_MODULES="" APOLLO_PCIE="" TB_CONTROLLER="" VERSION=test SOURCE=test
declare -A STEP_STATUS=([deps]=ok [build]=ok) STEP_DETAIL=()

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
absent() { ! grep -qF -- "$1" "$REPORT_FILE"; }
present() { grep -qF -- "$1" "$REPORT_FILE"; }

generate_report >/dev/null 2>&1
check "report written" test -s "$REPORT_FILE"
check "no user name" absent "\"$me\""
check "no host name" absent "$host"
check "no home path" absent "$home"
[ -r /etc/machine-id ] && check "no machine id" absent "$(cat /etc/machine-id)"
check "Apollo serial redacted" absent "20000000000123"
check "other device's serial dropped" absent "SN-HEADSET-123456"
check "other device's MAC dropped" absent "AA:BB:CC:DD:EE:FF"
check "other device's name dropped" absent "Fake Headset XYZ"
check "Apollo node kept" present "alsa_output.pci-0000_44_00.0.pro-output-0"
check "other devices only counted" present "other_media_classes"
check "pw-dump temp file removed" test -z "$(find "${TMPDIR:-/tmp}" -maxdepth 1 -newer "$REPORT_FILE" -name 'tmp.*' -user "$me" -size +0 2>/dev/null | xargs -r grep -l 'Fake Headset' 2>/dev/null)"

echo
if [ "$failures" -eq 0 ]; then
    echo "All report privacy checks passed."
else
    echo "$failures check(s) failed."
    exit 1
fi
