#!/bin/bash
# uninstall.sh — Remove Open Apollo driver, configs, and services
#
# Usage:
#   sudo bash scripts/uninstall.sh
#   sudo bash scripts/uninstall.sh --keep-dkms

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

KEEP_DKMS=0
for arg in "$@"; do
    case "$arg" in
        --keep-dkms) KEEP_DKMS=1 ;;
        -h|--help)
            echo "Usage: sudo bash scripts/uninstall.sh [--keep-dkms]"
            echo "  --keep-dkms   Keep DKMS registration (driver rebuilds on kernel updates)"
            exit 0
            ;;
    esac
done

# Require root
if [ "$(id -u)" -ne 0 ]; then
    fail "This script must be run with sudo"
    echo "  Usage: sudo bash scripts/uninstall.sh"
    exit 1
fi

echo ""
echo -e "${BOLD}Open Apollo — Uninstaller${NC}"
echo "========================="
echo ""

DESKTOP_USER="${SUDO_USER:-$(logname 2>/dev/null || echo "")}"
USER_UID=""
USER_HOME=""
if [ -n "$DESKTOP_USER" ]; then
    USER_UID=$(id -u "$DESKTOP_USER" 2>/dev/null || echo "")
    USER_HOME=$(eval echo "~$DESKTOP_USER")
fi

# Helper: run as desktop user with PipeWire env
pw_run() {
    if [ -n "$DESKTOP_USER" ] && [ -n "$USER_UID" ]; then
        sudo -u "$DESKTOP_USER" HOME="$USER_HOME" XDG_RUNTIME_DIR="/run/user/$USER_UID" "$@"
    fi
}

# Step 1: Stop and remove the mixer daemon (systemd user unit)
if [ -n "$DESKTOP_USER" ] && [ -n "$USER_UID" ]; then
    pw_run systemctl --user disable --now ua-mixer-daemon.service 2>/dev/null || true
fi
rm -f /etc/systemd/user/ua-mixer-daemon.service
[ -n "$USER_HOME" ] && rm -f "$USER_HOME/.config/systemd/user/ua-mixer-daemon.service" \
    "$USER_HOME/.config/systemd/user/default.target.wants/ua-mixer-daemon.service"
if [ -n "$DESKTOP_USER" ] && [ -n "$USER_UID" ]; then
    pw_run systemctl --user daemon-reload 2>/dev/null || true
fi
ok "Removed mixer daemon user unit"
# Also stop a daemon started by hand (older installers)
if pgrep -f "ua_mixer_daemon" > /dev/null 2>&1; then
    pkill -f "ua_mixer_daemon" 2>/dev/null || true
    sleep 1
    ok "Stopped mixer daemon"
else
    info "Mixer daemon not running"
fi

# Step 2: Stop tray indicator
if pgrep -f "open-apollo-tray" > /dev/null 2>&1; then
    pkill -f "open-apollo-tray" 2>/dev/null || true
    ok "Stopped tray indicator"
else
    info "Tray indicator not running"
fi

# Step 3: Disable and remove systemd user service
if [ -n "$DESKTOP_USER" ] && [ -n "$USER_UID" ]; then
    pw_run systemctl --user disable apollo-setup-io.service 2>/dev/null || true
    SVC_FILE="$USER_HOME/.config/systemd/user/apollo-setup-io.service"
    [ -f "$SVC_FILE" ] && rm -f "$SVC_FILE"
    ok "Removed systemd service"
fi

# Step 4: Remove PipeWire loopback config
if [ -n "$USER_HOME" ]; then
    CONF_FILE="$USER_HOME/.config/pipewire/pipewire.conf.d/apollo-io-map.conf"
    [ -f "$CONF_FILE" ] && rm -f "$CONF_FILE"
    ok "Removed PipeWire loopback config"
fi

# Step 5: Remove WirePlumber rules (both 0.4.x lua and 0.5.x conf formats)
rm -f /etc/wireplumber/main.lua.d/51-ua-apollo.lua 2>/dev/null
rm -f /etc/wireplumber/wireplumber.conf.d/51-ua-apollo.conf 2>/dev/null
rm -f /etc/pipewire/pipewire-pulse.conf.d/50-apollo-pulse-rules.conf 2>/dev/null
rm -f /etc/pipewire/pipewire.conf.d/10-apollo.conf 2>/dev/null
ok "Removed WirePlumber + PipeWire rules"

# Step 6: Remove UCM2 profile
rm -rf /usr/share/alsa/ucm2/ua_apollo 2>/dev/null
rm -f /usr/share/alsa/ucm2/conf.d/ua_apollo/ua_apollo.conf 2>/dev/null
rmdir /usr/share/alsa/ucm2/conf.d/ua_apollo 2>/dev/null || true
ok "Removed UCM2 profile"

# Step 7: Remove udev rules and scripts
rm -f /etc/udev/rules.d/91-ua-apollo.rules 2>/dev/null
rm -f /etc/udev/rules.d/70-ua-apollo-access.rules 2>/dev/null
rm -f /etc/udev/rules.d/60-thunderbolt-no-rtd3.rules 2>/dev/null
rm -f /usr/local/bin/open-apollo-profile-setup 2>/dev/null
rm -f /usr/local/bin/open-apollo-setup-worker 2>/dev/null
rm -f /usr/local/bin/apollo-setup-io 2>/dev/null
udevadm control --reload-rules 2>/dev/null || true
ok "Removed udev rules and scripts"

# Step 8: Remove autostart + app launcher
[ -n "$USER_HOME" ] && rm -f "$USER_HOME/.config/autostart/open-apollo-tray.desktop" 2>/dev/null
rm -f /usr/share/applications/open-apollo.desktop 2>/dev/null
ok "Removed autostart and app launcher"

# Step 9: Remove DKMS
if [ "$KEEP_DKMS" = "0" ] && command -v dkms &>/dev/null; then
    if dkms status ua_apollo 2>/dev/null | grep -q .; then
        VERSION=$(dkms status ua_apollo 2>/dev/null | head -1 | grep -oP '[0-9]+\.[0-9]+\.[0-9]+' | head -1)
        if [ -n "$VERSION" ]; then
            dkms remove "ua_apollo/$VERSION" --all 2>/dev/null || true
            ok "Removed DKMS registration (ua_apollo/$VERSION)"
        fi
    fi
    # Clean up source and internal DKMS state completely
    rm -rf /usr/src/ua_apollo-* 2>/dev/null
    rm -rf /var/lib/dkms/ua_apollo 2>/dev/null
    # Remove DKMS-installed module files
    rm -f "/lib/modules/$(uname -r)/updates/dkms/ua_apollo.ko"* 2>/dev/null
    depmod -a 2>/dev/null || true
    ok "Cleaned DKMS build tree and installed modules"
fi

# Step 10: Remove module auto-load and manually installed .ko files
HAD_MODULES_LOAD=0
[ -f /etc/modules-load.d/ua_apollo.conf ] && HAD_MODULES_LOAD=1
rm -f /etc/modules-load.d/ua_apollo.conf 2>/dev/null
rm -f "/lib/modules/$(uname -r)/extra/ua_apollo.ko" 2>/dev/null
rm -f "/lib/modules/$(uname -r)/extra/ua_apollo.ko.zst" 2>/dev/null
depmod -a 2>/dev/null || true
ok "Removed module auto-load config and extra module files"

# dracut bakes modules-load.d entries into the initramfs — without a regen the
# removed entry survives and keeps failing systemd-modules-load (issue #43)
if [ "$HAD_MODULES_LOAD" = "1" ]; then
    if command -v dracut &>/dev/null; then
        dracut -f 2>/dev/null && ok "Regenerated initramfs (dracut -f)" \
            || warn "dracut -f failed — run 'sudo dracut -f' manually"
    elif command -v update-initramfs &>/dev/null; then
        update-initramfs -u 2>/dev/null && ok "Regenerated initramfs" \
            || warn "update-initramfs failed — run 'sudo update-initramfs -u' manually"
    elif command -v mkinitcpio &>/dev/null; then
        mkinitcpio -P 2>/dev/null && ok "Regenerated initramfs" \
            || warn "mkinitcpio -P failed — run it manually"
    fi
fi

# Step 11: Unload kernel module
# Stop PipeWire first so nothing holds the ALSA device. Driver v2 unloads
# cleanly (disconnects the audio extension), so the Apollo can stay powered.
echo ""
if lsmod | grep -q '^ua_apollo'; then
    if [ -n "$DESKTOP_USER" ] && [ -n "$USER_UID" ]; then
        info "Stopping PipeWire to release audio device..."
        pw_run systemctl --user stop pipewire.service wireplumber.service \
            pipewire-pulse.service pipewire.socket pipewire-pulse.socket 2>/dev/null || true
        sleep 2
    fi

    if rmmod ua_apollo 2>/dev/null; then
        ok "Kernel module unloaded"
    else
        warn "Could not unload module (still in use) — reboot to fully remove"
    fi

    if [ -n "$DESKTOP_USER" ] && [ -n "$USER_UID" ]; then
        pw_run systemctl --user start pipewire.socket pipewire-pulse.socket 2>/dev/null || true
        ok "Restarted PipeWire"
    fi
else
    ok "Kernel module not loaded"
fi

# Restart PipeWire to clean up any remaining Apollo nodes
if [ -n "$DESKTOP_USER" ] && [ -n "$USER_UID" ]; then
    pw_run systemctl --user restart pipewire.service pipewire-pulse.socket pipewire-pulse.service wireplumber.service 2>/dev/null || true
fi

echo ""
echo -e "${GREEN}${BOLD}Uninstall complete.${NC}"
echo ""
rm -f /tmp/open-apollo-install-report.json 2>/dev/null
ok "Cleaned up install report"
echo ""
