#!/bin/bash
# setup-root.sh — the setup wizard's one privileged run
#
# Everything open-apollo-setup needs administrator rights for, in one run, so
# the password is asked once. In order:
#   source    open the Windows partition read-only (--mount), then check that a
#             usable UAD install is there; nothing is changed before this passes
#   remove    remove the earlier install: uninstall.sh --yes --purge
#             (skipped with --keep-current)
#   import    copy the Console art as the desktop user (oa-assets import),
#             install the plug-in panel specs (install-panels.sh: the pinned
#             open-apollo-panels release, about 1 MB from GitHub) and make
#             the plug-in art (oa-assets plugins); the last two only warn
#   firmware  extract the DSP programs and firmware: oa-assets firmware
#   driver    build and install the driver: install.sh --yes
#   unmount   close the partition again (only when this run opened it)
# Each step prints a "── Header ──" and marker lines for the wizard:
#   ::step <id> start|done|warn|failed|skipped [message]
# It stops at the first failed step. Running it again is fine.
#
# Usage:
#   pkexec bash scripts/setup-root.sh --user NAME (--from DIR | --mount DEVICE) [OPTIONS]
#
# Options:
#   --user NAME       the desktop user (owns the imported art and the audio session)
#   --from DIR        the folder that holds Windows' C:\ (or a Wine prefix)
#   --mount DEVICE    mount this Windows partition read-only and use it as --from
#   --oa-assets PATH  the oa-assets program (root's PATH usually lacks it)
#   --data DIR        import into DIR instead of ~/.local/share/open-apollo
#   --keep-current    don't remove the earlier install first
#   --panels TARBALL  install these plug-in panel specs instead of downloading
#                     the pinned release (see install-panels.sh)
#   --panels-sha256 HEX  the SHA-256 --panels must have, when not the pinned one
#   --send-report     upload install.sh's anonymous report (opt-in)
#   --plugin-hosting  experimental: load the driver with its plug-in host on and
#                     map the user's plug-ins for it (install.sh --plugin-hosting)
#   -h, --help        show this help
#
# Exit status: 0 done (perhaps with warnings), 1 a step failed, 2 a bad option.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
FIRMWARE_DIR="/lib/firmware/universal-audio"

# --- Flags (only main() parses them, so sourcing this file has no side effects) ---
TARGET_USER=""
FROM=""
MOUNT_DEV=""
OA_ASSETS=""
DATA_DIR=""
KEEP_CURRENT=0
SEND_REPORT=0
PLUGIN_HOSTING=0
PANELS_FROM=""
PANELS_SHA=""

if [ -t 1 ]; then
    RED='\033[0;31m' GREEN='\033[0;32m' YELLOW='\033[1;33m' CYAN='\033[0;36m' BOLD='\033[1m' NC='\033[0m'
else
    RED='' GREEN='' YELLOW='' CYAN='' BOLD='' NC=''
fi

info()   { echo -e "${CYAN}[INFO]${NC}  $*"; }
ok()     { echo -e "${GREEN}[ OK ]${NC}  $*"; }
warn()   { echo -e "${YELLOW}[WARN]${NC}  $*"; }
fail()   { echo -e "${RED}[FAIL]${NC}  $*"; }
header() { echo -e "\n${BOLD}── $* ──${NC}"; }
marker() { echo "::step $*"; }
usage()  { sed -n '2,/^set -uo/{/^set -uo/d;s/^# \{0,1\}//;p}' "${BASH_SOURCE[0]}"; }
bad_usage() { echo "setup-root.sh: $* (try --help)" >&2; exit 2; }

parse_args() {
    while [ $# -gt 0 ]; do
        case "$1" in
            --user|--from|--mount|--oa-assets|--data|--panels|--panels-sha256)
                [ $# -ge 2 ] && [ -n "$2" ] || bad_usage "$1 needs a value"
                case "$1" in
                    --user) TARGET_USER="$2" ;;
                    --from) FROM="$2" ;;
                    --mount) MOUNT_DEV="$2" ;;
                    --oa-assets) OA_ASSETS="$2" ;;
                    --data) DATA_DIR="$2" ;;
                    --panels) PANELS_FROM="$2" ;;
                    --panels-sha256) PANELS_SHA="$2" ;;
                esac
                shift 2 ;;
            --keep-current) KEEP_CURRENT=1; shift ;;
            --send-report) SEND_REPORT=1; shift ;;
            --plugin-hosting) PLUGIN_HOSTING=1; shift ;;
            -h|--help) usage; exit 0 ;;
            *) bad_usage "unknown option $1" ;;
        esac
    done
    [ -n "$TARGET_USER" ] || bad_usage "--user is required"
    if [ -n "$FROM" ] && [ -n "$MOUNT_DEV" ]; then
        bad_usage "use --from or --mount, not both"
    fi
    [ -n "$FROM" ] || [ -n "$MOUNT_DEV" ] || bad_usage "--from or --mount is required"
    case "$MOUNT_DEV" in
        ""|/dev/*) ;;
        *) bad_usage "--mount takes a partition such as /dev/nvme0n1p3" ;;
    esac
    return 0
}

# oa-assets: --oa-assets, else on PATH, else built in this checkout.
find_oa_assets() {
    local c
    for c in "$OA_ASSETS" "$(command -v oa-assets 2>/dev/null)" \
             "$PROJECT_DIR/console-rs/target/release/oa-assets" "$PROJECT_DIR/console-rs/target/debug/oa-assets"; do
        if [ -n "$c" ] && [ -f "$c" ] && [ -x "$c" ]; then
            OA_ASSETS="$c"
            return 0
        fi
    done
    return 1
}

USER_UID=""
USER_GID=""
USER_HOME=""

# Run a command as the desktop user, with their session bus.
as_user() {
    local -a envs=(HOME="$USER_HOME" XDG_RUNTIME_DIR="/run/user/$USER_UID"
                   DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/$USER_UID/bus")
    if command -v runuser >/dev/null 2>&1; then
        runuser -u "$TARGET_USER" -- env "${envs[@]}" "$@"
    else
        sudo -u "$TARGET_USER" "${envs[@]}" "$@"
    fi
}

# --- source: open the partition, check the install ---

MOUNTED=0
MNT=""

mount_source() {
    local existing err
    existing=$(findmnt -n -o TARGET --source "$MOUNT_DEV" 2>/dev/null | head -1)
    if [ -n "$existing" ]; then
        info "$MOUNT_DEV is already open at $existing; reading it there"
        FROM="$existing"
        return 0
    fi
    MNT=$(mktemp -d "${TMPDIR:-/tmp}/open-apollo-windows.XXXXXX") || return 1
    # Read-only; the files show as the desktop user's (the import runs as them)
    # and as nobody else's.
    local opts="ro,nosuid,nodev,noexec,uid=$USER_UID,gid=$USER_GID,umask=077"
    if err=$(mount -t ntfs3 -o "$opts" "$MOUNT_DEV" "$MNT" 2>&1); then
        ok "Opened $MOUNT_DEV read-only (ntfs3)"
    elif err=$(mount -t ntfs-3g -o "$opts" "$MOUNT_DEV" "$MNT" 2>&1); then
        ok "Opened $MOUNT_DEV read-only (ntfs-3g)"
    else
        rmdir "$MNT" 2>/dev/null
        MNT=""
        fail "Couldn't open $MOUNT_DEV read-only: ${err:-mount failed}"
        info "If Windows is hibernated or used Fast Startup, start it and shut it down fully"
        info "(hold Shift while clicking Shut down), then try again."
        return 1
    fi
    MOUNTED=1
    FROM="$MNT"
}

# The install must have the Console's art and the DSP programs' sources: the
# Console strip-header plug-ins or UAD2SDK.dll (`oa-assets scan --json`).
check_source() {
    local json
    if ! json=$("$OA_ASSETS" scan --from "$FROM" --json 2>&1); then
        fail "oa-assets couldn't read $FROM"
        echo "$json" | head -5
        return 1
    fi
    local console=0 dsp=0 version plugins
    echo "$json" | grep -q '"console": {' && console=1
    if echo "$json" | grep -q '"firmware_inputs":'; then
        echo "$json" | grep -q '"firmware_inputs": true' && dsp=1
    elif echo "$json" | grep -q '"sdk_dll": "'; then
        dsp=1   # an oa-assets without firmware_inputs: UAD2SDK.dll is enough
    fi
    version=$(echo "$json" | sed -n 's/^ *"version": "\(.*\)",\{0,1\}$/\1/p' | head -1)
    plugins=$(echo "$json" | sed -n 's/^ *"powered_plugins": \([0-9]*\).*/\1/p' | head -1)
    if [ "$console" = 1 ]; then
        ok "UAD Console ${version:-(version unknown)}"
    else
        fail "No UAD Console in $FROM: the Console needs its art"
    fi
    if [ "$dsp" = 1 ]; then
        ok "DSP programs' sources found (${plugins:-0} Powered plug-in DLLs)"
    else
        fail "No Console strip plug-ins or UAD2SDK.dll in $FROM: the driver needs the DSP programs they hold"
    fi
    [ "$console" = 1 ] && [ "$dsp" = 1 ]
}

step_source() {
    marker source start
    header "Checking your UAD install"
    if ! find_oa_assets; then
        fail "oa-assets not found: build it (cargo build --release -p oa-assets) or pass --oa-assets PATH"
        marker source failed "oa-assets not found"
        return 1
    fi
    if [ -n "$MOUNT_DEV" ] && ! mount_source; then
        marker source failed "Couldn't open $MOUNT_DEV read-only"
        return 1
    fi
    if [ ! -d "$FROM" ]; then
        fail "$FROM is not a folder"
        marker source failed "$FROM is not a folder"
        return 1
    fi
    info "Reading $FROM (read-only)"
    if ! check_source; then
        marker source failed "No usable UAD install in $FROM"
        return 1
    fi
    marker source "done" "UAD install found in $FROM"
}

# --- remove, import, firmware, driver ---

step_remove() {
    if [ "$KEEP_CURRENT" = 1 ]; then
        marker remove skipped "Kept the current install (--keep-current)"
        return 0
    fi
    marker remove start
    header "Removing the earlier install"
    if ! bash "$SCRIPT_DIR/uninstall.sh" --yes --purge; then
        marker remove failed "Something of the earlier install could not be removed (see Check)"
        return 1
    fi
    marker remove "done" "The earlier install is gone"
}

# Whether this oa-assets has COMMAND (its help lists "oa-assets COMMAND").
oa_has() { "$OA_ASSETS" help 2>&1 | grep -q "oa-assets $1\b"; }

step_import() {
    marker import start
    header "Importing the Console art"
    local -a data=()
    [ -n "$DATA_DIR" ] && data=(--data "$DATA_DIR")
    if ! as_user "$OA_ASSETS" import --from "$FROM" "${data[@]}"; then
        fail "Couldn't copy the Console art"
        marker import failed "Couldn't copy the Console art"
        return 1
    fi
    # Plug-in panels need both the specs and the art; either missing only
    # means plug-in windows show a plain parameter list.
    local panels=1
    if ! install_panels; then
        warn "Couldn't install the plug-in panel specs; plug-in windows show a plain panel"
        panels=0
    fi
    if ! oa_has plugins; then
        info "Plug-in art: this oa-assets can't generate it yet; plug-in windows show a plain panel"
        marker import "done" "Console art imported"
        return 0
    fi
    if ! as_user "$OA_ASSETS" plugins --from "$FROM" "${data[@]}"; then
        warn "Couldn't generate the plug-in art; plug-in windows show a plain panel"
        marker import warn "Console art imported; plug-in art failed"
        return 0
    fi
    if [ "$panels" = 0 ]; then
        marker import warn "Console and plug-in art imported; plug-in panel specs not installed"
        return 0
    fi
    marker import "done" "Console art, plug-in panels and their art imported"
}

# The pinned plug-in panel specs into /usr/share/open-apollo/plugin-specs.
install_panels() {
    local -a args=()
    [ -n "$PANELS_FROM" ] && args+=(--from "$PANELS_FROM")
    [ -n "$PANELS_SHA" ] && args+=(--sha256 "$PANELS_SHA")
    bash "$SCRIPT_DIR/install-panels.sh" "${args[@]}"
}

step_firmware() {
    marker firmware start
    header "Extracting DSP programs and firmware"
    "$OA_ASSETS" firmware --from "$FROM" --out "$FIRMWARE_DIR"
    local rc=$?
    case "$rc" in
        0) marker firmware "done" "Extracted to $FIRMWARE_DIR" ;;
        1) marker firmware failed "A DSP program the driver needs is missing from your install" ;;
        2) marker firmware failed "A DSP program in your install is a version open-apollo doesn't know yet" ;;
        *) marker firmware failed "oa-assets firmware stopped (exit $rc)" ;;
    esac
    # Every plug-in's DSP modules, for plug-in hosting later.  Optional: a
    # failure here only warns.
    if [ "$rc" = 0 ] && oa_has dsp; then
        "$OA_ASSETS" dsp --from "$FROM" --out "$FIRMWARE_DIR/plugins" \
            || warn "Couldn't collect the plug-in DSP modules (only needed for future plug-in hosting)"
    fi
    [ "$rc" = 0 ]
}

DRIVER_LOG=""

step_driver() {
    marker driver start
    header "Installing the driver"
    local -a args=(--yes)
    [ "$SEND_REPORT" = 1 ] && args+=(--send-report)
    # The install is still open here (it closes on exit), so the plug-ins can be mapped from it.
    [ "$PLUGIN_HOSTING" = 1 ] && args+=("--plugin-hosting=$FROM")
    DRIVER_LOG=$(mktemp "${TMPDIR:-/tmp}/open-apollo-install.XXXXXX") || DRIVER_LOG=/dev/null
    bash "$SCRIPT_DIR/install.sh" "${args[@]}" 2>&1 | tee "$DRIVER_LOG"
    local rc=${PIPESTATUS[0]}
    if [ "$rc" != 0 ]; then
        marker driver failed "install.sh stopped (exit $rc)"
        return 1
    fi
    if grep -q "REBOOT REQUIRED" "$DRIVER_LOG"; then
        marker driver warn "Installed; restart the computer to finish"
    elif grep -q "finished with issues" "$DRIVER_LOG"; then
        marker driver warn "Installed, with problems (see the summary)"
    else
        marker driver "done" "Installed"
    fi
}

# --- unmount (also after a failure) ---

cleanup() {
    [ -n "$DRIVER_LOG" ] && [ "$DRIVER_LOG" != /dev/null ] && rm -f "$DRIVER_LOG"
    [ "$MOUNTED" = 1 ] || return 0
    MOUNTED=0
    marker unmount start
    header "Closing the Windows partition"
    if umount "$MNT" 2>/dev/null || { sleep 1; umount "$MNT" 2>/dev/null; }; then
        rmdir "$MNT" 2>/dev/null
        ok "Closed $MOUNT_DEV"
        marker unmount "done" "Closed $MOUNT_DEV"
    elif umount -l "$MNT" 2>/dev/null; then
        rmdir "$MNT" 2>/dev/null
        warn "$MOUNT_DEV was busy; it closes once nothing reads it"
        marker unmount warn "$MOUNT_DEV closes once nothing reads it"
    else
        warn "Couldn't close $MOUNT_DEV: run  sudo umount $MNT"
        marker unmount warn "Couldn't close $MOUNT_DEV"
    fi
}

main() {
    parse_args "$@"
    if [ "$(id -u)" -ne 0 ]; then
        fail "Run it as root: pkexec bash scripts/setup-root.sh ... (the setup wizard does)"
        return 1
    fi
    USER_UID=$(id -u "$TARGET_USER" 2>/dev/null)
    USER_GID=$(id -g "$TARGET_USER" 2>/dev/null)
    USER_HOME=$(getent passwd "$TARGET_USER" 2>/dev/null | cut -d: -f6)
    if [ -z "$USER_UID" ] || [ -z "$USER_GID" ] || [ -z "$USER_HOME" ]; then
        fail "No such user: $TARGET_USER"
        return 1
    fi
    # uninstall.sh and install.sh find the desktop user here
    export SUDO_USER="$TARGET_USER"
    # A closed wizard window must not kill the install halfway
    trap '' PIPE
    trap cleanup EXIT
    cd / || return 1
    exec </dev/null

    echo ""
    echo -e "${BOLD}Open Apollo — Setup${NC}"
    echo "==================="

    if ! step_source; then
        info "Nothing was changed on this computer."
        return 1
    fi
    step_remove || return 1
    step_import || return 1
    step_firmware || return 1
    step_driver || return 1
    return 0
}

# Run only when executed directly; sourcing (tests) just loads the functions.
if [ "${BASH_SOURCE[0]}" = "$0" ]; then
    main "$@"
    exit $?
fi
