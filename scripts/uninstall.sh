#!/bin/bash
# uninstall.sh — Remove every trace of an Open Apollo install
#
# Removes the ua_apollo driver (every DKMS version, their source trees and the
# modules they built), the mixer daemon's user units and drop-ins, the system
# and per-user PipeWire/WirePlumber rules, the UCM2 profile, the udev rules and
# helpers, the Console (uaconsole), the plug-in panel specs, old tray and
# launcher files, and a USB
# install's pieces; then checks that nothing is left. Running it twice, or
# with nothing installed, is fine.
#
# Usage:
#   sudo bash scripts/uninstall.sh [OPTIONS]
#   pkexec bash scripts/uninstall.sh --yes --purge   (setup: always a clean reinstall)
#   bash scripts/uninstall.sh --list                 (no root needed; changes nothing)
#
# Options:
#   -y, --yes     Ask nothing. Nothing is asked either when stdin isn't a terminal.
#   --purge       Also delete the extracted firmware (/lib/firmware/universal-audio,
#                 /lib/firmware/ua_apollo) and the desktop user's imported Console
#                 art and caches (~/.local/share/open-apollo, ~/.cache/open-apollo).
#                 Without it they stay, so a reinstall can use them again.
#   --keep-dkms   Keep the DKMS registration and the modules it built.
#   --list        Print what is installed, one "KIND<TAB>ITEM" line each, and exit.
#   -h, --help    Show this help.
#
# Exit status: 0 when no installed file is left (kept data aside), 1 when
# something is left (each is listed under "Check"), 2 for a bad option.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Prefixed to every system path. Always empty, except in tests/test-uninstall.sh.
ROOT=""
UACONSOLE_SH="$SCRIPT_DIR/install-uaconsole.sh"
USB_UNINSTALL_SH="$SCRIPT_DIR/uninstall-usb.sh"

# --- Flags (only main() parses them, so sourcing this file has no side effects) ---
ASSUME_YES=0
PURGE=0
KEEP_DKMS=0
LIST_ONLY=0

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
usage()  { sed -n '2,/^set -uo/{/^set -uo/d;s/^# \{0,1\}//;p}' "${BASH_SOURCE[0]}"; }

parse_args() {
    local arg
    for arg in "$@"; do
        case "$arg" in
            -y|--yes)    ASSUME_YES=1 ;;
            --purge)     PURGE=1 ;;
            --keep-dkms) KEEP_DKMS=1 ;;
            --list)      LIST_ONLY=1 ;;
            -h|--help)   usage; exit 0 ;;
            *) echo "Unknown option: $arg (try --help)" >&2; exit 2 ;;
        esac
    done
}

# --- The desktop user: owns the audio session, the daemon's user unit, ~/.local ---
DESKTOP_USER=""
USER_UID=""
USER_HOME=""

resolve_desktop_user() {
    local u=""
    if [ "$(id -u)" -ne 0 ]; then
        u=$(id -un 2>/dev/null || true)
    elif [ -n "${SUDO_USER:-}" ]; then
        u="$SUDO_USER"
    elif [ -n "${PKEXEC_UID:-}" ]; then
        # pkexec (the setup wizard) sets PKEXEC_UID instead of SUDO_USER
        u=$(id -nu "$PKEXEC_UID" 2>/dev/null || true)
    else
        u=$(logname 2>/dev/null || true)
    fi
    DESKTOP_USER="$u"
    USER_UID=""
    USER_HOME=""
    if [ -n "$u" ]; then
        USER_UID=$(id -u "$u" 2>/dev/null || true)
        USER_HOME=$(getent passwd "$u" 2>/dev/null | cut -d: -f6)
    fi
}

# Run a command as the desktop user, with their systemd --user bus.
as_user() {
    [ -n "$DESKTOP_USER" ] && [ -n "$USER_UID" ] && [ -n "$USER_HOME" ] || return 1
    local -a envs=(HOME="$USER_HOME" XDG_RUNTIME_DIR="/run/user/$USER_UID"
                   DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/$USER_UID/bus")
    if command -v runuser >/dev/null 2>&1; then
        runuser -u "$DESKTOP_USER" -- env "${envs[@]}" "$@"
    else
        sudo -u "$DESKTOP_USER" "${envs[@]}" "$@"
    fi
}

user_systemctl() { as_user systemctl --user "$@" >/dev/null 2>&1; }

# --- What an install leaves behind ---

# paths_of KIND: set PATHS to the files and folders of KIND (globs expand
# now; one that matches nothing stays literal and doesn't exist).
paths_of() {
    local h="$USER_HOME"
    PATHS=()
    case "$1" in
        dkms)
            PATHS=("$ROOT"/usr/src/ua_apollo-* "$ROOT"/var/lib/dkms/ua_apollo
                   "$ROOT"/lib/modules/*/updates/dkms/ua_apollo.ko*) ;;
        driver)
            PATHS=("$ROOT"/lib/modules/*/extra/ua_apollo.ko* "$ROOT"/etc/modules-load.d/ua_apollo.conf
                   "$ROOT"/etc/modprobe.d/open-apollo-plugin-hosting.conf) ;;
        daemon)
            PATHS=("$ROOT"/etc/systemd/user/ua-mixer-daemon.service
                   "$ROOT"/etc/systemd/user/ua-mixer-daemon@.service
                   "$ROOT"/etc/systemd/user/ua-mixer-daemon.service.d
                   "$ROOT"/etc/systemd/user/ua-mixer-daemon@*.service.d
                   "$ROOT"/etc/systemd/user/*.wants/ua-mixer-daemon*.service)
            [ -n "$h" ] && PATHS+=("$h"/.config/systemd/user/ua-mixer-daemon.service
                                   "$h"/.config/systemd/user/ua-mixer-daemon@.service
                                   "$h"/.config/systemd/user/ua-mixer-daemon.service.d
                                   "$h"/.config/systemd/user/ua-mixer-daemon@*.service.d
                                   "$h"/.config/systemd/user/*.wants/ua-mixer-daemon*.service
                                   "$h"/.config/systemd/user/apollo-setup-io.service
                                   "$h"/.config/systemd/user/*.wants/apollo-setup-io.service) ;;
        config)
            PATHS=("$ROOT"/etc/wireplumber/main.lua.d/51-ua-apollo.lua
                   "$ROOT"/etc/wireplumber/wireplumber.conf.d/51-ua-apollo.conf
                   "$ROOT"/etc/pipewire/pipewire-pulse.conf.d/50-apollo-pulse-rules.conf
                   "$ROOT"/etc/pipewire/pipewire.conf.d/10-apollo.conf
                   "$ROOT"/etc/pipewire/pipewire.conf.d/90-disable-apollo.conf
                   "$ROOT"/usr/share/alsa/ucm2/ua_apollo
                   "$ROOT"/usr/share/alsa/ucm2/conf.d/ua_apollo)
            [ -n "$h" ] && PATHS+=("$h"/.config/wireplumber/wireplumber.conf.d/51-ua-apollo.conf
                                   "$h"/.config/wireplumber/main.lua.d/51-ua-apollo.lua
                                   "$h"/.config/pipewire/pipewire.conf.d/apollo-io-map.conf
                                   "$h"/.config/pipewire/pipewire.conf.d/apollo-solo-usb-io.conf
                                   "$h"/.config/pipewire/pipewire-pulse.conf.d/50-apollo-pulse-rules.conf) ;;
        udev)
            PATHS=("$ROOT"/etc/udev/rules.d/91-ua-apollo.rules
                   "$ROOT"/etc/udev/rules.d/70-ua-apollo-access.rules
                   "$ROOT"/etc/udev/rules.d/60-thunderbolt-no-rtd3.rules
                   "$ROOT"/usr/local/bin/open-apollo-profile-setup
                   "$ROOT"/usr/local/bin/open-apollo-setup-worker
                   "$ROOT"/usr/local/bin/apollo-setup-io) ;;
        console)
            [ -n "$h" ] && PATHS=("$h"/.local/bin/uaconsole "$h"/.local/bin/open-apollo-console
                                  "$h"/.local/share/applications/uaconsole.desktop
                                  "$h"/.local/share/icons/hicolor/scalable/apps/uaconsole.svg) ;;
        panels)
            # install-panels.sh (setup's import step)
            PATHS=("$ROOT"/usr/share/open-apollo/plugin-specs) ;;
        legacy)
            # The old tray indicator, its launcher, and firmware blobs older installers wrote
            PATHS=("$ROOT"/usr/share/applications/open-apollo.desktop
                   "$ROOT"/lib/firmware/ua-apollo-plugin-chain.bin
                   "$ROOT"/lib/firmware/ua-apollo-mixer.bin)
            [ -n "$h" ] && PATHS+=("$h"/.config/autostart/open-apollo-tray.desktop) ;;
        usb)
            PATHS=("$ROOT"/etc/udev/rules.d/99-apollo-usb.rules
                   "$ROOT"/etc/udev/rules.d/70-ua-apollo-usb-access.rules
                   "$ROOT"/usr/local/lib/ua-usb
                   "$ROOT"/usr/local/bin/ua-usb-init
                   "$ROOT"/usr/local/bin/ua-usb-dsp-init
                   "$ROOT"/etc/systemd/system/ua-usb-init@.service
                   "$ROOT"/etc/systemd/system/ua-usb-dsp-init.service
                   "$ROOT/lib/modules/$(uname -r)/updates/snd-usb-audio.ko"*
                   "$ROOT"/root/.cache/open-apollo-snd-usb-build)
            [ -n "$h" ] && PATHS+=("$h"/.config/wireplumber/wireplumber.conf.d/50-apollo-solo-usb.conf
                                   "$h"/.cache/open-apollo-snd-usb-build) ;;
        firmware)
            # Extracted from the user's UAD install (kept unless --purge)
            PATHS=("$ROOT"/lib/firmware/universal-audio "$ROOT"/lib/firmware/ua_apollo) ;;
        data)
            # Imported Console and plug-in art, caches (kept unless --purge)
            [ -n "$h" ] && PATHS=("$h"/.local/share/open-apollo "$h"/.cache/open-apollo) ;;
    esac
    return 0
}

FILE_KINDS="dkms driver daemon config udev console panels legacy usb firmware data"

# Every ua_apollo version DKMS knows, one per line (both status formats:
# "ua_apollo/1.0, 6.9, x86_64: installed" and "ua_apollo, 1.0, 6.9, ...").
dkms_versions() {
    {
        if command -v dkms >/dev/null 2>&1; then
            dkms status ua_apollo 2>/dev/null | sed -n 's|^ua_apollo[/,] *\([^,: ]*\).*|\1|p'
        fi
        local d
        for d in "$ROOT"/var/lib/dkms/ua_apollo/*; do
            [ -d "$d" ] && [ ! -L "$d" ] || continue
            case "${d##*/}" in kernel-*|source|original_module) continue ;; esac
            echo "${d##*/}"
        done
    } | sort -u
}

module_loaded() { lsmod 2>/dev/null | awk '$1 == "ua_apollo" { found = 1 } END { exit !found }'; }

INV_KIND=()
INV_ITEM=()
inv_add() { INV_KIND+=("$1"); INV_ITEM+=("$2"); }

# take_inventory: fill INV_KIND/INV_ITEM with everything installed now.
take_inventory() {
    INV_KIND=()
    INV_ITEM=()
    local v k p
    while read -r v; do
        [ -n "$v" ] && inv_add dkms "ua_apollo $v (DKMS)"
    done < <(dkms_versions)
    module_loaded && inv_add module "the ua_apollo driver is loaded"
    pgrep -f ua_mixer_daemon >/dev/null 2>&1 && inv_add process "the mixer daemon is running"
    pgrep -f open-apollo-tray >/dev/null 2>&1 && inv_add process "the old tray indicator is running"
    for k in $FILE_KINDS; do
        paths_of "$k"
        for p in "${PATHS[@]}"; do
            if [ -e "$p" ] || [ -L "$p" ]; then
                inv_add "$k" "${p#"$ROOT"}"
            fi
        done
    done
    return 0
}

# Whether the inventory holds anything this run removes (kept data aside).
something_to_remove() {
    local k
    for k in "${INV_KIND[@]}"; do
        case "$k" in
            firmware|data) [ "$PURGE" = 1 ] && return 0 ;;
            dkms) [ "$KEEP_DKMS" = 1 ] || return 0 ;;
            *) return 0 ;;
        esac
    done
    return 1
}

# remove_paths KIND: delete what paths_of KIND lists, saying what went.
# Returns 1 when there was nothing to delete.
REMOVED=()
remove_paths() {
    local p shown n=0
    paths_of "$1"
    for p in "${PATHS[@]}"; do
        [ -e "$p" ] || [ -L "$p" ] || continue
        shown="${p#"$ROOT"}"
        if rm -rf -- "$p"; then
            ok "Removed $shown"
            REMOVED+=("$shown")
            n=$((n + 1))
        else
            fail "Could not remove $shown"
        fi
    done
    [ "$n" -gt 0 ]
}

# --- Removal steps ---

stop_daemon() {
    header "Mixer daemon"
    if [ -n "$DESKTOP_USER" ]; then
        user_systemctl disable --now ua-mixer-daemon.service
        user_systemctl stop 'ua-mixer-daemon@*.service'
        user_systemctl disable --now apollo-setup-io.service
    fi
    # A daemon started by hand (older installers)
    if pgrep -f ua_mixer_daemon >/dev/null 2>&1; then
        pkill -f ua_mixer_daemon 2>/dev/null || true
        sleep 1
        ok "Stopped the mixer daemon"
    fi
    remove_paths daemon || info "No mixer daemon units"
    [ -n "$DESKTOP_USER" ] && user_systemctl daemon-reload
    return 0
}

# PipeWire goes first so nothing holds the ALSA device. Driver v2 unloads
# cleanly (disconnects the audio extension), so the Apollo can stay powered.
unload_driver() {
    header "Unloading the driver"
    if ! module_loaded; then
        ok "Driver not loaded"
        return 0
    fi
    if [ -n "$DESKTOP_USER" ]; then
        info "Stopping PipeWire to release the audio device..."
        user_systemctl stop pipewire.service wireplumber.service \
            pipewire-pulse.service pipewire.socket pipewire-pulse.socket
        sleep 2
    fi
    if rmmod ua_apollo 2>/dev/null || modprobe -r ua_apollo 2>/dev/null; then
        ok "Driver unloaded"
    else
        warn "Could not unload the driver (still in use): restart the computer to finish"
    fi
    if [ -n "$DESKTOP_USER" ]; then
        user_systemctl start pipewire.socket pipewire-pulse.socket
        ok "PipeWire started again"
    fi
}

# install-uaconsole.sh --uninstall, as the desktop user so their menu cache
# stays theirs; whatever it leaves is removed directly.
remove_console() {
    header "Console"
    local p present=0
    paths_of console
    for p in "${PATHS[@]}"; do
        if [ -e "$p" ] || [ -L "$p" ]; then present=1; fi
    done
    if [ "$present" = 0 ]; then
        info "uaconsole is not installed"
        return 0
    fi
    if [ -n "$DESKTOP_USER" ] && [ -f "$UACONSOLE_SH" ]; then
        as_user bash "$UACONSOLE_SH" --uninstall --prefix "$USER_HOME/.local" 2>&1 \
            | sed -e '/^$/d' -e '/── Removing uaconsole ──/d' -e '/Kept your copied Console resources/d' \
            || warn "install-uaconsole.sh --uninstall failed; removing its files directly"
    fi
    remove_paths console || true
    return 0
}

remove_panels() {
    header "Plug-in panel specs"
    if remove_paths panels; then
        rmdir "$ROOT"/usr/share/open-apollo 2>/dev/null || true
    else
        info "No plug-in panel specs"
    fi
    return 0
}

remove_audio_config() {
    header "Audio settings"
    if remove_paths config; then
        rmdir "$ROOT"/usr/share/alsa/ucm2/conf.d 2>/dev/null || true
        # Drop the removed rules and any Apollo nodes
        if [ -n "$DESKTOP_USER" ] && user_systemctl restart pipewire.service pipewire-pulse.socket \
            pipewire-pulse.service wireplumber.service; then
            ok "Restarted PipeWire and WirePlumber"
        fi
    else
        info "No PipeWire, WirePlumber or UCM2 settings"
    fi
    return 0
}

remove_udev() {
    header "udev rules and helpers"
    if remove_paths udev; then
        udevadm control --reload-rules 2>/dev/null || true
    else
        info "No udev rules or helpers"
    fi
    return 0
}

remove_legacy() {
    header "Old tray and launcher"
    if pgrep -f open-apollo-tray >/dev/null 2>&1; then
        pkill -f open-apollo-tray 2>/dev/null || true
        ok "Stopped the tray indicator"
    fi
    remove_paths legacy || info "No old tray, launcher or firmware blob files"
    return 0
}

# A USB install (install-usb.sh): its own uninstaller restores stock snd-usb-audio.
remove_usb() {
    local p present=0
    paths_of usb
    for p in "${PATHS[@]}"; do
        if [ -e "$p" ] || [ -L "$p" ]; then present=1; fi
    done
    [ "$present" = 1 ] || return 0
    header "USB install"
    if [ -f "$USB_UNINSTALL_SH" ]; then
        local -a args=(--yes)
        [ "$PURGE" = 1 ] && args+=(--purge)
        SUDO_USER="$DESKTOP_USER" bash "$USB_UNINSTALL_SH" "${args[@]}" \
            || warn "uninstall-usb.sh did not finish cleanly (see above); restart the computer to finish"
    fi
    remove_paths usb || true
    return 0
}

remove_driver() {
    header "Driver (DKMS)"
    local v had_modules_load=0
    if [ "$KEEP_DKMS" = 1 ]; then
        info "Keeping the DKMS registration (--keep-dkms)"
    else
        if command -v dkms >/dev/null 2>&1; then
            while read -r v; do
                [ -n "$v" ] || continue
                if dkms remove "ua_apollo/$v" --all >/dev/null 2>&1; then
                    ok "Removed DKMS registration ua_apollo/$v"
                else
                    info "dkms could not remove ua_apollo/$v; removing its files directly"
                fi
            done < <(dkms_versions)
        fi
        remove_paths dkms || true
    fi
    [ -e "$ROOT/etc/modules-load.d/ua_apollo.conf" ] && had_modules_load=1
    remove_paths driver || true

    # Refresh module dependencies of every kernel a module was removed from
    local p k kernels=""
    for p in "${REMOVED[@]}"; do
        case "$p" in
            /lib/modules/*/*)
                k="${p#/lib/modules/}"
                k="${k%%/*}"
                case " $kernels " in *" $k "*) ;; *) kernels="$kernels $k" ;; esac ;;
        esac
    done
    for k in $kernels; do
        if [ -d "$ROOT/lib/modules/$k" ]; then
            depmod -a "$k" 2>/dev/null || true
        fi
    done

    # dracut bakes modules-load.d entries into the initramfs; without a regen
    # the removed entry survives and fails systemd-modules-load (issue #43)
    if [ "$had_modules_load" = 1 ]; then
        if command -v dracut >/dev/null 2>&1; then
            if dracut -f 2>/dev/null; then ok "Regenerated initramfs (dracut -f)"
            else warn "dracut -f failed: run 'sudo dracut -f' yourself"; fi
        elif command -v update-initramfs >/dev/null 2>&1; then
            if update-initramfs -u 2>/dev/null; then ok "Regenerated initramfs"
            else warn "update-initramfs failed: run 'sudo update-initramfs -u' yourself"; fi
        elif command -v mkinitcpio >/dev/null 2>&1; then
            if mkinitcpio -P 2>/dev/null; then ok "Regenerated initramfs"
            else warn "mkinitcpio -P failed: run it yourself"; fi
        fi
    fi
    return 0
}

remove_firmware_and_data() {
    header "Firmware and imported art"
    if [ "$PURGE" = 1 ]; then
        remove_paths firmware || info "No extracted firmware"
        remove_paths data || info "No imported art or caches"
        return 0
    fi
    local k p kept=0
    for k in firmware data; do
        paths_of "$k"
        for p in "${PATHS[@]}"; do
            if [ -e "$p" ]; then
                info "Kept ${p#"$ROOT"} (--purge removes it)"
                kept=1
            fi
        done
    done
    [ "$kept" = 1 ] || info "No extracted firmware or imported art"
    return 0
}

# --- Check ---

# List what is still installed; 0 when no file is left (kept data aside).
verify() {
    header "Check"
    take_inventory
    local i kind item left=0
    for i in "${!INV_KIND[@]}"; do
        kind="${INV_KIND[$i]}"
        item="${INV_ITEM[$i]}"
        case "$kind" in
            firmware|data)
                if [ "$PURGE" = 1 ]; then
                    fail "Still here: $item"
                    left=$((left + 1))
                else
                    info "Kept: $item"
                fi ;;
            dkms)
                if [ "$KEEP_DKMS" = 1 ]; then
                    info "Kept (--keep-dkms): $item"
                else
                    fail "Still here: $item"
                    left=$((left + 1))
                fi ;;
            module|process)
                warn "Still running: $item; restart the computer to finish" ;;
            *)
                fail "Still here: $item"
                left=$((left + 1)) ;;
        esac
    done
    if [ "$left" -eq 0 ]; then
        ok "Nothing from open-apollo is left"
        return 0
    fi
    fail "$left item(s) could not be removed"
    return 1
}

confirm() {
    [ "$ASSUME_YES" = 1 ] && return 0
    local answer=""
    if [ "$PURGE" = 1 ]; then
        warn "--purge also deletes the extracted firmware and your imported Console art."
    fi
    read -rp "Remove all of this? [y/N] " answer
    [[ "$answer" =~ ^[Yy] ]]
}

main() {
    parse_args "$@"

    if [ "$LIST_ONLY" = 1 ]; then
        resolve_desktop_user
        take_inventory
        local i
        for i in "${!INV_KIND[@]}"; do
            printf '%s\t%s\n' "${INV_KIND[$i]}" "${INV_ITEM[$i]}"
        done
        return 0
    fi

    if [ "$(id -u)" -ne 0 ]; then
        fail "This script must be run with sudo"
        echo "  Usage: sudo bash scripts/uninstall.sh"
        return 1
    fi
    # --yes, or no terminal to ask on: ask nothing
    if [ "$ASSUME_YES" = 1 ] || [ ! -t 0 ]; then
        ASSUME_YES=1
        exec </dev/null
    fi
    resolve_desktop_user

    echo ""
    echo -e "${BOLD}Open Apollo — Uninstaller${NC}"
    echo "========================="

    header "Earlier install"
    if [ -z "$DESKTOP_USER" ] || [ -z "$USER_HOME" ]; then
        warn "No desktop user found (run it with sudo from your account): only system files are removed"
    fi
    take_inventory
    local i
    for i in "${!INV_KIND[@]}"; do
        info "Found: ${INV_ITEM[$i]}"
    done
    if [ "${#INV_KIND[@]}" -eq 0 ]; then
        ok "Nothing from open-apollo is installed"
    elif ! something_to_remove; then
        ok "Nothing to remove (extracted firmware and imported art stay; --purge removes them)"
    elif ! confirm; then
        info "Nothing was changed."
        return 0
    else
        stop_daemon
        unload_driver
        remove_console
        remove_panels
        remove_audio_config
        remove_udev
        remove_legacy
        remove_usb
        remove_driver
        remove_firmware_and_data
    fi
    rm -f "$ROOT/tmp/open-apollo-install-report.json" 2>/dev/null

    local rc=0
    verify || rc=1
    echo ""
    if [ "$rc" = 0 ]; then
        echo -e "${GREEN}${BOLD}Uninstall complete.${NC}"
    else
        echo -e "${YELLOW}${BOLD}Uninstall finished, but something is left (see Check above).${NC}"
    fi
    echo ""
    return "$rc"
}

# Run only when executed directly; sourcing (tests) just loads the functions.
if [ "${BASH_SOURCE[0]}" = "$0" ]; then
    main "$@"
    exit $?
fi
