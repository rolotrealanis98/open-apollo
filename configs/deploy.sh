#!/bin/bash
# Deploy PipeWire/WirePlumber/UCM2 configs for Universal Audio Apollo
# Run on Ubuntu: sudo bash ~/open-apollo/configs/deploy.sh

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

# Step 2: Deploy WirePlumber rules
echo "Installing WirePlumber rules..."
mkdir -p /etc/wireplumber/main.lua.d
cp "$SCRIPT_DIR/wireplumber/51-ua-apollo.lua" /etc/wireplumber/main.lua.d/
echo "  -> /etc/wireplumber/main.lua.d/51-ua-apollo.lua"

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

# Step 4: Deploy udev rule + profile setup script
echo "Installing udev rule for auto profile setup..."
cp "$SCRIPT_DIR/udev/91-ua-apollo.rules" /etc/udev/rules.d/
cp "$SCRIPT_DIR/udev/open-apollo-profile-setup" /usr/local/bin/
chmod +x /usr/local/bin/open-apollo-profile-setup
udevadm control --reload-rules 2>/dev/null || true
echo "  -> /etc/udev/rules.d/91-ua-apollo.rules"
echo "  -> /usr/local/bin/open-apollo-profile-setup"

# Step 5: Deploy PipeWire I/O mapping setup script
echo "Installing PipeWire I/O setup script..."
DESKTOP_USER="${SUDO_USER:-$(logname 2>/dev/null || echo "")}"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
cp "$SCRIPT_DIR/pipewire/setup-apollo-io.sh" /usr/local/bin/apollo-setup-io
chmod +x /usr/local/bin/apollo-setup-io
echo "  -> /usr/local/bin/apollo-setup-io"

# Step 6: Deploy tray indicator (autostart + app launcher)
echo "Installing tray indicator..."
if [ -n "$DESKTOP_USER" ]; then
    # Autostart on login
    AUTOSTART_DIR="$(eval echo ~"$DESKTOP_USER")/.config/autostart"
    mkdir -p "$AUTOSTART_DIR"
    sed "s|/opt/open-apollo|$REPO_DIR|g" "$SCRIPT_DIR/autostart/open-apollo-tray.desktop" \
        > "$AUTOSTART_DIR/open-apollo-tray.desktop"
    echo "  -> $AUTOSTART_DIR/open-apollo-tray.desktop (autostart)"
fi
# App launcher (shows in Activities/Search)
sed "s|/opt/open-apollo|$REPO_DIR|g" "$SCRIPT_DIR/autostart/open-apollo-tray.desktop" \
    > /usr/share/applications/open-apollo.desktop
echo "  -> /usr/share/applications/open-apollo.desktop (app launcher)"

echo ""
echo "=== Deployment complete ==="
echo ""
echo "Next steps:"
echo "  1. Restart PipeWire:  systemctl --user restart pipewire wireplumber"
echo "  2. Verify:            wpctl status"
echo "  3. Test playback:     pw-play /usr/share/sounds/freedesktop/stereo/complete.oga"
echo ""
echo "The Apollo will auto-configure as the default audio device when powered on."
