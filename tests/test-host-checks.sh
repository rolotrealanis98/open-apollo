#!/usr/bin/env bash
# test-host-checks.sh — tests for scripts/apollo-hostcheck.sh (Thunderbolt
# authorization, Secure Boot/MOK guidance) and the PipeWire setup worker's
# udev rule.  Uses a fake sysfs tree and stub mokutil/modinfo; needs no
# hardware and does not touch the host.

set -uo pipefail

PROJECT_DIR="$(CDPATH='' cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

failures=0
check() {
    local desc="$1"; shift
    if "$@"; then echo "PASS: $desc"; else echo "FAIL: $desc"; failures=$((failures + 1)); fi
}
has() { grep -qF -- "$2" <<<"$1"; }

# --- Thunderbolt -----------------------------------------------------
mk_tb() {   # $1 root, $2 security level, $3 authorized, $4 vendor, $5 device
    local d="$1/bus/thunderbolt/devices"
    mkdir -p "$d/domain0" "$d/0-0" "$d/0-1"
    echo "$2" > "$d/domain0/security"
    echo "$3" > "$d/0-1/authorized"
    echo "$4" > "$d/0-1/vendor_name"
    echo "$5" > "$d/0-1/device_name"
    echo "12345678-90ab-cdef-1234-567890abcdef" > "$d/0-1/unique_id"
}

mk_tb "$tmp/tb-user" user 0 "Universal Audio" "Apollo x4"
out=$(HC_SYSFS="$tmp/tb-user" bash "$PROJECT_DIR/scripts/apollo-hostcheck.sh" tb); rc=$?
check "unauthorized Apollo returns 1" test "$rc" = "1"
check "names the security level" has "$out" "security level: user"
check "prints boltctl enroll with the uuid" \
    has "$out" "boltctl enroll --policy auto 12345678-90ab-cdef-1234-567890abcdef"
check "prints boltctl authorize with the uuid" \
    has "$out" "boltctl authorize 12345678-90ab-cdef-1234-567890abcdef"

mk_tb "$tmp/tb-ok" user 1 "Universal Audio" "Apollo x4"
out=$(HC_SYSFS="$tmp/tb-ok" bash "$PROJECT_DIR/scripts/apollo-hostcheck.sh" tb); rc=$?
check "authorized Apollo is silent and returns 0" test "$rc" = "0" -a -z "$out"

out=$(HC_SYSFS="$tmp/none" bash "$PROJECT_DIR/scripts/apollo-hostcheck.sh" tb); rc=$?
check "no Thunderbolt bus is silent and returns 0" test "$rc" = "0" -a -z "$out"

mk_tb "$tmp/tb-other" secure 0 "Acme" "Dock"
out=$(HC_SYSFS="$tmp/tb-other" bash "$PROJECT_DIR/scripts/apollo-hostcheck.sh" tb)
check "other unauthorized device is still reported" has "$out" "Unauthorized Thunderbolt device 'Acme Dock'"

# --- Secure Boot -----------------------------------------------------
bin="$tmp/bin"; mkdir -p "$bin"
cat > "$bin/mokutil" <<'EOF'
#!/bin/sh
case "$1" in
    --sb-state) echo "SecureBoot ${STUB_SB:-enabled}" ;;
    --test-key) echo "${STUB_KEY:-is not enrolled}" ;;
esac
EOF
cat > "$bin/modinfo" <<'EOF'
#!/bin/sh
# modinfo -F signer ua_apollo / modinfo -n ua_apollo
case "$1" in
    -F) echo "${STUB_SIGNER:-}" ;;
    -n) [ "${STUB_INSTALLED:-1}" = 1 ] && { echo /lib/modules/x/ua_apollo.ko; exit 0; }; exit 1 ;;
esac
exit 0
EOF
chmod +x "$bin/mokutil" "$bin/modinfo"

root="$tmp/root"; mkdir -p "$root/var/lib/dkms" "$root/etc"
echo "ID=ubuntu" > "$root/etc/os-release"
: > "$root/var/lib/dkms/mok.pub"
sb() { PATH="$bin:$PATH" HC_ROOT="$root" HC_SYSFS="$tmp/none" "$@" bash "$PROJECT_DIR/scripts/apollo-hostcheck.sh" sb "${SB_ARG:-}"; }

out=$(SB_ARG="modprobe: ERROR: could not insert 'ua_apollo': Key was rejected by service" sb env); rc=$?
check "rejected key returns 1" test "$rc" = "1"
check "prints the mokutil --import command" has "$out" "mokutil --import $root/var/lib/dkms/mok.pub"
check "mentions the reboot / Enroll MOK step" has "$out" "Enroll MOK"

out=$(SB_ARG="" sb env); rc=$?
check "unsigned installed module under Secure Boot is flagged" test "$rc" = "1"

out=$(SB_ARG="" sb env STUB_SIGNER="Open Apollo DKMS"); rc=$?
check "signed module with no complaint is silent" test "$rc" = "0" -a -z "$out"

out=$(SB_ARG="" sb env STUB_INSTALLED=0); rc=$?
check "module not installed yet is silent" test "$rc" = "0" -a -z "$out"

out=$(SB_ARG="Key was rejected by service" sb env STUB_SB=disabled); rc=$?
check "Secure Boot off is silent" test "$rc" = "0" -a -z "$out"

out=$(SB_ARG="Key was rejected by service" sb env STUB_KEY="is already enrolled")
check "already-enrolled key says rebuild, not import" has "$out" "dkms install ua_apollo"
check "already-enrolled key prints no import command" test -z "$(grep -F 'mokutil --import' <<<"$out")"

rm "$root/var/lib/dkms/mok.pub"
out=$(SB_ARG="Key was rejected by service" sb env)
check "missing key tells how to create one" has "$out" "dkms generate_mok"

# --- udev rule vs driver PCI id table --------------------------------
drv_ids=$(sed -n '/ua_pci_ids\[\]/,/};/p' "$PROJECT_DIR/driver/ua_core.c" \
    | grep -o 'PCI_DEVICE_ID_[A-Z0-9_]*' | sort -u)
missing=""
[ -n "$drv_ids" ] || missing=" (no ids parsed from ua_pci_ids)"
for macro in $drv_ids; do
    val=$(sed -n "s/^#define ${macro}[[:space:]]*0x\\([0-9A-Fa-f]*\\).*/\\1/p" "$PROJECT_DIR/driver/ua_apollo.h")
    [ -n "$val" ] || { missing="$missing $macro(unresolved)"; continue; }
    id=$(printf '0x%04x' "0x$val")
    grep -v '^#' "$PROJECT_DIR/configs/udev/91-ua-apollo.rules" | grep -q "$id" || missing="$missing $id"
done
check "91-ua-apollo.rules matches every id in ua_pci_ids${missing:+ (missing:$missing)}" test -z "$missing"

# --- setup worker ----------------------------------------------------
w="$PROJECT_DIR/configs/udev/open-apollo-setup-worker"
check "worker writes no fixed /tmp file" test -z "$(grep -n '/tmp/' "$w")"
check "worker does not pick the first session blindly" test -z "$(grep -n 'head -1' "$w" | grep -i session)"

# session choice: greeter, remote, inactive and root sessions must lose to the
# active local desktop user, whatever order loginctl lists them in
cat > "$bin/loginctl" <<'EOF'
#!/bin/sh
case "$1" in
    list-sessions) printf 'c1 0 root seat0\nc2 120 gdm seat0\nc3 1001 bob\nc4 1000 alice seat0\nc5 1002 carol seat0\n' ;;
    show-session)
        case "$2" in
            c1) printf 'User=0\nName=root\nActive=yes\nClass=user\nType=tty\nRemote=no\nSeat=seat0\n' ;;
            c2) printf 'User=120\nName=gdm\nActive=yes\nClass=greeter\nType=wayland\nRemote=no\nSeat=seat0\n' ;;
            c3) printf 'User=1001\nName=bob\nActive=yes\nClass=user\nType=tty\nRemote=yes\nSeat=\n' ;;
            c4) printf 'User=1000\nName=alice\nActive=yes\nClass=user\nType=wayland\nRemote=no\nSeat=seat0\n' ;;
            c5) printf 'User=1002\nName=carol\nActive=no\nClass=user\nType=wayland\nRemote=no\nSeat=seat0\n' ;;
        esac ;;
esac
EOF
chmod +x "$bin/loginctl"
mkdir -p "$tmp/run/1000" "$tmp/run/1001" "$tmp/run/1002" "$tmp/run/0" "$tmp/run/120"
fn=$(sed -n '/^pick_session_user() {/,/^}/p' "$PROJECT_DIR/configs/udev/open-apollo-setup-worker")
picked=$(PATH="$bin:$PATH" RUN_USER_BASE="$tmp/run" bash -c "$fn"$'\npick_session_user')
check "worker picks the active local desktop user via loginctl" test "$picked" = "1000 alice"

echo
if [ "$failures" -eq 0 ]; then echo "All checks passed"; else echo "$failures check(s) failed"; exit 1; fi
