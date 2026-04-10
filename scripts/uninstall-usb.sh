#!/bin/bash
# uninstall-usb.sh — Remove Open Apollo USB install artifacts
#
# Removes everything install-usb.sh creates:
#   - Patched snd-usb-audio.ko in /lib/modules/<KERNEL>/updates/
#   - /usr/local/lib/ua-usb/ helper library (fx3-load, init scripts)
#   - /usr/local/bin/ua-usb-init + ua-usb-dsp-init wrappers
#   - /etc/udev/rules.d/99-apollo-usb.rules
#   - WirePlumber override (~/.config/wireplumber/.../50-apollo-solo-usb.conf)
#   - Build cache (~/.cache/open-apollo-snd-usb-build/)
#   - /tmp install reports and wget logs
#
# Leaves firmware in place by default (use --purge to also remove it —
# UA doesn't allow firmware redistribution so it's expensive to get back).
#
# After uninstall, stock snd-usb-audio is reloaded so the Apollo still
# appears as a basic audio device (no DSP routing, no capture).
#
# Usage:
#   sudo bash scripts/uninstall-usb.sh            # keep firmware
#   sudo bash scripts/uninstall-usb.sh --purge    # also delete firmware

set -uo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

info()   { echo -e "${CYAN}[INFO]${NC}  $*"; }
ok()     { echo -e "${GREEN}[ OK ]${NC}  $*"; }
warn()   { echo -e "${YELLOW}[WARN]${NC}  $*"; }
fail()   { echo -e "${RED}[FAIL]${NC}  $*"; }
header() { echo -e "\n${BOLD}── $* ──${NC}"; }

PURGE=0
for arg in "$@"; do
    case "$arg" in
        --purge) PURGE=1 ;;
        -h|--help)
            echo "Usage: sudo bash scripts/uninstall-usb.sh [--purge]"
            echo "  --purge   Also delete firmware from /lib/firmware/universal-audio/"
            exit 0
            ;;
        *) warn "Unknown argument: $arg" ;;
    esac
done

if [ "$(id -u)" -ne 0 ]; then
    fail "This script must be run with sudo"
    echo "  Usage: sudo bash scripts/uninstall-usb.sh"
    exit 1
fi

echo ""
echo -e "${BOLD}Open Apollo USB — Uninstaller${NC}"
echo "============================="

KERNEL=$(uname -r)
REAL_USER="${SUDO_USER:-$USER}"
REAL_HOME=$(eval echo "~$REAL_USER")
REAL_UID=$(id -u "$REAL_USER" 2>/dev/null || echo 1000)

# ================================================================
# Step 1: Stop any running init processes / daemons
# ================================================================
header "Stopping init processes"

KILLED=0
for pattern in "usb-dsp-init" "usb-full-init" "ua-usb-init" "ua-usb-dsp-init"; do
    if pgrep -f "$pattern" >/dev/null 2>&1; then
        pkill -9 -f "$pattern" 2>/dev/null || true
        KILLED=1
    fi
done
if [ "$KILLED" = "1" ]; then
    ok "Killed init processes"
    sleep 1
else
    info "No init processes running"
fi

# ================================================================
# Step 2: Remove udev rules FIRST (so fresh device events don't
# re-spawn init scripts mid-uninstall)
# ================================================================
header "Removing udev rules"

if [ -f /etc/udev/rules.d/99-apollo-usb.rules ]; then
    rm -f /etc/udev/rules.d/99-apollo-usb.rules
    udevadm control --reload-rules 2>/dev/null || true
    udevadm trigger 2>/dev/null || true
    ok "Removed /etc/udev/rules.d/99-apollo-usb.rules"
else
    info "udev rules not installed"
fi

# ================================================================
# Step 3: Remove installed helper scripts and library
# ================================================================
header "Removing installed scripts"

REMOVED=0
for f in /usr/local/bin/ua-usb-init /usr/local/bin/ua-usb-dsp-init; do
    if [ -f "$f" ]; then rm -f "$f"; REMOVED=1; fi
done
if [ -d /usr/local/lib/ua-usb ]; then
    rm -rf /usr/local/lib/ua-usb
    REMOVED=1
fi
if [ "$REMOVED" = "1" ]; then
    ok "Removed /usr/local/bin/ua-usb-* and /usr/local/lib/ua-usb/"
else
    info "Installed scripts not present"
fi

# ================================================================
# Step 4: Unload patched snd-usb-audio, delete .ko, restore stock
# ================================================================
header "Restoring stock snd-usb-audio"

if lsmod 2>/dev/null | grep -q '^snd_usb_audio'; then
    modprobe -r snd_usb_audio 2>/dev/null || rmmod snd_usb_audio 2>/dev/null || true
    if lsmod 2>/dev/null | grep -q '^snd_usb_audio'; then
        warn "Could not unload snd_usb_audio (in use — close audio apps and retry)"
    else
        ok "Unloaded snd_usb_audio"
    fi
fi

PATCHED_KO="/lib/modules/${KERNEL}/updates/snd-usb-audio.ko"
for ext in "" ".zst" ".xz"; do
    if [ -f "${PATCHED_KO}${ext}" ]; then
        rm -f "${PATCHED_KO}${ext}"
        ok "Removed patched module: $(basename "${PATCHED_KO}${ext}")"
    fi
done

depmod -a 2>/dev/null || true
modprobe snd_usb_audio 2>/dev/null || true
if lsmod 2>/dev/null | grep -q '^snd_usb_audio'; then
    STOCK_PATH=$(modinfo snd_usb_audio 2>/dev/null | awk '/^filename:/ {print $2}')
    ok "Stock snd_usb_audio loaded: $STOCK_PATH"
fi

# ================================================================
# Step 5: Clean out-of-tree build cache
# ================================================================
header "Cleaning build cache"

CACHE_REMOVED=0
for h in /root "$REAL_HOME"; do
    cache="$h/.cache/open-apollo-snd-usb-build"
    if [ -d "$cache" ]; then
        rm -rf "$cache"
        ok "Removed $cache"
        CACHE_REMOVED=1
    fi
done
[ "$CACHE_REMOVED" = "0" ] && info "Build cache not present"

# ================================================================
# Step 6: Remove WirePlumber override + restart PipeWire
# ================================================================
header "Removing WirePlumber override"

WP_CONF="$REAL_HOME/.config/wireplumber/wireplumber.conf.d/50-apollo-solo-usb.conf"
if [ -f "$WP_CONF" ]; then
    rm -f "$WP_CONF"
    ok "Removed $WP_CONF"

    # Restart PipeWire so WirePlumber drops the cached rule.
    # Best-effort — no dbus session in non-interactive contexts is fine.
    sudo -u "$REAL_USER" XDG_RUNTIME_DIR="/run/user/$REAL_UID" \
        DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/$REAL_UID/bus" \
        systemctl --user restart pipewire wireplumber 2>/dev/null \
        && ok "Restarted PipeWire + WirePlumber" \
        || info "Could not restart PipeWire (no session) — restart manually or log out/in"
else
    info "WirePlumber override not present"
fi

# ================================================================
# Step 7: Remove install reports and diagnostic logs
# ================================================================
header "Removing install reports"

REPORT_REMOVED=0
for f in /tmp/open-apollo-usb-install-report.json /tmp/open-apollo-wget.log; do
    if [ -f "$f" ]; then
        rm -f "$f"
        ok "Removed $f"
        REPORT_REMOVED=1
    fi
done
[ "$REPORT_REMOVED" = "0" ] && info "No install reports to remove"

# ================================================================
# Step 8: Optional firmware purge (--purge only)
# ================================================================
if [ "$PURGE" = "1" ]; then
    header "Purging firmware (--purge)"
    if [ -d /lib/firmware/universal-audio ]; then
        rm -rf /lib/firmware/universal-audio
        ok "Removed /lib/firmware/universal-audio/"
        warn "You will need to re-provide firmware before the next install"
    else
        info "No firmware directory to purge"
    fi
fi

# ================================================================
# Done
# ================================================================
header "Uninstall Complete"
echo ""
ok "All USB install artifacts removed"
if [ "$PURGE" = "0" ]; then
    info "Firmware preserved at /lib/firmware/universal-audio/ (use --purge to wipe)"
fi
info "Stock snd-usb-audio is reloaded — Apollo still appears but with limited functionality"
info "Power-cycle the Apollo before the next install for a clean slate"
echo ""
