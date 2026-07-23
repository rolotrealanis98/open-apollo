#!/bin/bash
# Deploy PipeWire/WirePlumber/UCM2 configs for Universal Audio Apollo
# Run on Ubuntu: sudo bash ~/apollo-linux/configs/deploy.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

echo "=== Deploying Apollo PipeWire integration configs ==="

# Step 1: Remove broken PipeWire config that causes crash
BROKEN_CONF="/etc/pipewire/pipewire.conf.d/90-disable-apollo.conf"
if [ -f "$BROKEN_CONF" ]; then
    echo "Removing broken config: $BROKEN_CONF"
    rm -f "$BROKEN_CONF"
else
    echo "No broken config to remove (OK)"
fi

# Step 2: Deploy WirePlumber rules (detect version for correct config format)
echo "Installing WirePlumber rules..."

# Detect WirePlumber version: 0.5+ uses .conf (JSON-like), 0.4.x uses .lua
# (patched: ask wireplumber/wpctl directly first; probe ALL package managers with
#  fall-through instead of elif — a host can have e.g. dpkg installed on Fedora
#  with no database entry, which previously aborted the script under `set -e`)
WP_VER=""
WP_VER=$(wireplumber --version 2>/dev/null | grep -oE '[0-9]+\.[0-9]+(\.[0-9]+)?' | head -1 || true)
if [ -z "$WP_VER" ]; then
    WP_VER=$(wpctl --version 2>/dev/null | grep -oE '[0-9]+\.[0-9]+(\.[0-9]+)?' | head -1 || true)
fi
if [ -z "$WP_VER" ] && command -v pacman &>/dev/null; then
    WP_VER=$(pacman -Q wireplumber 2>/dev/null | awk '{print $2}' | cut -d- -f1 || true)
fi
if [ -z "$WP_VER" ] && command -v dpkg &>/dev/null; then
    WP_VER=$(dpkg -s wireplumber 2>/dev/null | grep '^Version:' | awk '{print $2}' | cut -d- -f1 | cut -d: -f2 || true)
fi
if [ -z "$WP_VER" ] && command -v rpm &>/dev/null; then
    WP_VER=$(rpm -q --qf '%{VERSION}' wireplumber 2>/dev/null || true)
fi

WP_MAJOR=$(echo "$WP_VER" | cut -d. -f1)
WP_MINOR=$(echo "$WP_VER" | cut -d. -f2)

if [ "${WP_MAJOR:-0}" -gt 0 ] 2>/dev/null || [ "${WP_MINOR:-0}" -ge 5 ] 2>/dev/null; then
    # WirePlumber 0.5+ — use .conf format
    echo "  WirePlumber $WP_VER detected (0.5+ config format)"
    mkdir -p /etc/wireplumber/wireplumber.conf.d
    cp "$SCRIPT_DIR/wireplumber/51-ua-apollo.conf" /etc/wireplumber/wireplumber.conf.d/
    echo "  -> /etc/wireplumber/wireplumber.conf.d/51-ua-apollo.conf"
    # Remove old lua config if present (would trigger warnings)
    rm -f /etc/wireplumber/main.lua.d/51-ua-apollo.lua 2>/dev/null
else
    # WirePlumber 0.4.x or unknown — use .lua format
    echo "  WirePlumber ${WP_VER:-unknown} detected (0.4.x config format)"
    mkdir -p /etc/wireplumber/main.lua.d
    cp "$SCRIPT_DIR/wireplumber/51-ua-apollo.lua" /etc/wireplumber/main.lua.d/
    echo "  -> /etc/wireplumber/main.lua.d/51-ua-apollo.lua"
    # Remove new conf if present (shouldn't be, but clean up)
    rm -f /etc/wireplumber/wireplumber.conf.d/51-ua-apollo.conf 2>/dev/null
fi

# Step 3: Deploy UCM2 profile
echo "Installing UCM2 profile..."
mkdir -p /usr/share/alsa/ucm2/ua_apollo
mkdir -p /usr/share/alsa/ucm2/conf.d/ua_apollo
cp "$SCRIPT_DIR/ucm2/ua_apollo/ua_apollo.conf" /usr/share/alsa/ucm2/ua_apollo/
cp "$SCRIPT_DIR/ucm2/ua_apollo/HiFi.conf" /usr/share/alsa/ucm2/ua_apollo/
echo "  -> /usr/share/alsa/ucm2/ua_apollo/ua_apollo.conf"
echo "  -> /usr/share/alsa/ucm2/ua_apollo/HiFi.conf"

# Create symlink for driver name match (UCM2 looks up conf.d/${CardDriver}/${CardDriver}.conf)
ln -sf ../../ua_apollo/ua_apollo.conf /usr/share/alsa/ucm2/conf.d/ua_apollo/ua_apollo.conf
echo "  -> /usr/share/alsa/ucm2/conf.d/ua_apollo/ua_apollo.conf (symlink)"

# Step 4: Install PipeWire I/O mapping setup script
echo "Installing PipeWire I/O setup script..."
SETUP_SCRIPT="$SCRIPT_DIR/pipewire/setup-apollo-io.sh"
if [ -f "$SETUP_SCRIPT" ]; then
    cp "$SETUP_SCRIPT" /usr/local/bin/apollo-setup-io
    chmod +x /usr/local/bin/apollo-setup-io
    echo "  -> /usr/local/bin/apollo-setup-io"
else
    echo "  Warning: $SETUP_SCRIPT not found, skipping"
fi

# Step 5: Deploy udev rule and hotplug helper scripts
echo "Installing udev rule and hotplug scripts..."
UDEV_DIR="$SCRIPT_DIR/udev"
if [ -f "$UDEV_DIR/91-ua-apollo.rules" ]; then
    cp "$UDEV_DIR/91-ua-apollo.rules" /etc/udev/rules.d/
    echo "  -> /etc/udev/rules.d/91-ua-apollo.rules"

    cp "$UDEV_DIR/open-apollo-profile-setup" /usr/local/bin/
    chmod +x /usr/local/bin/open-apollo-profile-setup
    echo "  -> /usr/local/bin/open-apollo-profile-setup"

    cp "$UDEV_DIR/open-apollo-setup-worker" /usr/local/bin/
    chmod +x /usr/local/bin/open-apollo-setup-worker
    echo "  -> /usr/local/bin/open-apollo-setup-worker"

    udevadm control --reload-rules 2>/dev/null || true
    echo "  udev rules reloaded"
else
    echo "  Warning: udev rules not found, skipping"
fi

echo ""
echo "=== Deployment complete ==="
echo ""
echo "Next steps:"
echo "  1. Restart PipeWire:  systemctl --user restart pipewire wireplumber"
echo "  2. Set up I/O map:    apollo-setup-io"
echo "  3. Verify:            wpctl status"
echo "  4. Test playback:     pw-play /usr/share/sounds/freedesktop/stereo/complete.oga"
echo "  5. Test capture:      pw-record --rate 48000 --channels 2 --format s32 /tmp/test.wav"
