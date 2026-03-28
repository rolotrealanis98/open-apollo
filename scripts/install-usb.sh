#!/bin/bash
# install-usb.sh — Installer for Apollo Solo/Twin USB on Linux
#
# Handles: dependency install, firmware check, kernel module build,
# DSP init, PipeWire config, audio test.
#
# Usage:
#   sudo bash scripts/install-usb.sh
#
# Requires: Apollo USB plugged in, USB 3.0 port

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
FW_DIR="/lib/firmware/universal-audio"

# --- Colors ---
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

prompt() {
    local varname="$1"; shift
    if [ -t 0 ] && [ -e /dev/tty ]; then
        read -rp "$*" "$varname" < /dev/tty
    else
        eval "$varname=''"
    fi
}

die() { fail "$*"; exit 1; }

REAL_USER="${SUDO_USER:-$USER}"
REAL_HOME=$(eval echo "~$REAL_USER")
REAL_UID=$(id -u "$REAL_USER" 2>/dev/null || echo 1000)
SND_USB_BUILD="$REAL_HOME/.cache/open-apollo-snd-usb-build"

run_sudo() {
    if [ "$(id -u)" -eq 0 ]; then "$@"; else sudo "$@"; fi
}

# ================================================================
# Step 1: System detection
# ================================================================
header "Detecting System"

KERNEL=$(uname -r)
ARCH=$(uname -m)
DISTRO_ID="unknown"
PKG_MGR=""

if [ -f /etc/os-release ]; then
    DISTRO=$(. /etc/os-release && echo "${PRETTY_NAME:-$ID}")
    DISTRO_ID=$(. /etc/os-release && echo "${ID:-unknown}")
fi

case "$DISTRO_ID" in
    fedora|rhel|centos) PKG_MGR="dnf" ;;
    ubuntu|debian|pop|linuxmint) PKG_MGR="apt" ;;
    arch|manjaro|endeavouros) PKG_MGR="pacman" ;;
    opensuse*|sles) PKG_MGR="zypper" ;;
esac

ok "Distro: ${DISTRO:-unknown}"
info "Kernel: $KERNEL | Arch: $ARCH"

# ================================================================
# Step 2: Check for Apollo USB device
# ================================================================
header "Checking for Apollo USB"

# Check for loader PID (pre-firmware) or live PID (post-firmware)
LOADER_DEV=$(lsusb 2>/dev/null | grep -i "2b5a:000c\|2b5a:0001\|2b5a:000e" || true)
LIVE_DEV=$(lsusb 2>/dev/null | grep -i "2b5a:000d\|2b5a:0002\|2b5a:000f" || true)

if [ -n "$LIVE_DEV" ]; then
    ok "Apollo USB detected (firmware loaded): $LIVE_DEV"
    NEEDS_FW_LOAD=0
elif [ -n "$LOADER_DEV" ]; then
    ok "Apollo USB detected (needs firmware): $LOADER_DEV"
    NEEDS_FW_LOAD=1
else
    fail "No Apollo USB device found"
    info "Make sure the Apollo is plugged in via USB 3.0"
    info "Supported: Apollo Solo USB, Twin USB, Twin X USB"
    exit 1
fi

# ================================================================
# Step 3: Install dependencies
# ================================================================
header "Dependencies"

DEPS_NEEDED=()

# Check python3
if ! command -v python3 &>/dev/null; then DEPS_NEEDED+=(python3); fi

# Check pyusb
if ! python3 -c "import usb.core" 2>/dev/null; then DEPS_NEEDED+=(pyusb); fi

# Check kernel headers (needed for snd-usb-audio build)
if [ ! -d "/lib/modules/$KERNEL/build" ]; then DEPS_NEEDED+=(kernel-headers); fi

# Check gcc/make
if ! command -v gcc &>/dev/null; then DEPS_NEEDED+=(gcc); fi
if ! command -v make &>/dev/null; then DEPS_NEEDED+=(make); fi

# Check wget
if ! command -v wget &>/dev/null; then DEPS_NEEDED+=(wget); fi

if [ ${#DEPS_NEEDED[@]} -eq 0 ]; then
    ok "All dependencies present"
else
    info "Installing: ${DEPS_NEEDED[*]}"

    case "$PKG_MGR" in
        apt)
            run_sudo apt-get update -qq
            run_sudo apt-get install -y -qq python3 python3-pip \
                linux-headers-"$KERNEL" build-essential wget 2>/dev/null
            ;;
        dnf)
            run_sudo dnf install -y python3 python3-pip \
                kernel-devel gcc make wget 2>/dev/null
            ;;
        pacman)
            run_sudo pacman -S --noconfirm python python-pip \
                linux-headers gcc make wget 2>/dev/null
            ;;
    esac

    # Install pyusb
    if ! python3 -c "import usb.core" 2>/dev/null; then
        pip3 install --break-system-packages pyusb 2>/dev/null || \
            pip3 install pyusb 2>/dev/null || \
            run_sudo pip3 install --break-system-packages pyusb 2>/dev/null
    fi

    if python3 -c "import usb.core" 2>/dev/null; then
        ok "Dependencies installed"
    else
        fail "Could not install pyusb — install manually: pip3 install pyusb"
        exit 1
    fi
fi

# ================================================================
# Step 4: Firmware check
# ================================================================
header "Firmware"

# Detect which firmware file we need
if echo "$LOADER_DEV$LIVE_DEV" | grep -qi "000c\|000d"; then
    FW_FILE="ApolloSolo.bin"
elif echo "$LOADER_DEV$LIVE_DEV" | grep -qi "0001\|0002"; then
    FW_FILE="ApolloTwin.bin"
elif echo "$LOADER_DEV$LIVE_DEV" | grep -qi "000e\|000f"; then
    FW_FILE="ApolloTwinX.bin"
else
    FW_FILE="ApolloSolo.bin"
fi

if [ "$NEEDS_FW_LOAD" -eq 0 ]; then
    ok "Device already has firmware loaded — skipping firmware check"
elif [ -f "$FW_DIR/$FW_FILE" ]; then
    ok "Firmware found: $FW_DIR/$FW_FILE"
else
    warn "Firmware not found: $FW_DIR/$FW_FILE"
    echo ""
    info "The Apollo USB needs firmware uploaded on every power-on."
    info "UA doesn't allow redistribution, so you need to get it yourself:"
    echo ""
    echo "  Option A: Download from UA"
    echo "    1. Go to https://help.uaudio.com/hc/en-us/articles/26454031439892"
    echo "    2. Download the firmware package"
    echo "    3. Extract $FW_FILE"
    echo ""
    echo "  Option B: Copy from a Windows install of UA Connect"
    echo "    C:\\Program Files (x86)\\Universal Audio\\Powered Plugins\\Firmware\\USB\\"
    echo ""
    echo "  Then place it:"
    echo "    sudo mkdir -p $FW_DIR"
    echo "    sudo cp $FW_FILE $FW_DIR/"
    echo ""
    FWPATH=""
    prompt FWPATH "Enter path to $FW_FILE (or press Enter to skip): "
    if [ -n "${FWPATH:-}" ] && [ -f "${FWPATH:-}" ]; then
        run_sudo mkdir -p "$FW_DIR"
        run_sudo cp "$FWPATH" "$FW_DIR/$FW_FILE"
        ok "Firmware installed: $FW_DIR/$FW_FILE"
    else
        warn "Skipping firmware — you'll need to install it before audio works"
    fi
fi

# ================================================================
# Step 5: Build patched snd-usb-audio
# ================================================================
header "Building patched snd-usb-audio kernel module"

info "UA USB devices need a 3-line kernel patch for sample rate enumeration."
info "Building out-of-tree snd-usb-audio module..."

rm -rf "$SND_USB_BUILD"
su - "$REAL_USER" -c "mkdir -p '$SND_USB_BUILD'"

# Download kernel source (just sound/usb) as the real user
KVER_MAJOR=$(echo "$KERNEL" | grep -oP '^\d+\.\d+')
info "Downloading sound/usb source for kernel $KVER_MAJOR..."
su - "$REAL_USER" -c "cd '$SND_USB_BUILD' && wget -q 'https://cdn.kernel.org/pub/linux/kernel/v${KVER_MAJOR%%.*}.x/linux-${KVER_MAJOR}.tar.xz' -O - | xz -d | tar x --strip-components=1 'linux-${KVER_MAJOR}/sound/usb/' 2>/dev/null"

if [ ! -f "$SND_USB_BUILD/sound/usb/format.c" ]; then
    fail "Could not download kernel source for $KVER_MAJOR"
    info "You may need to build the patched module manually."
    info "See: tools/usb-re/0001-ALSA-usb-audio-Add-quirk-for-Universal-Audio-USB-devices.patch"
else
    # Apply patch and build as real user
    su - "$REAL_USER" -c "
        cd '$SND_USB_BUILD/sound/usb'

        # Apply the fixed-rate quirk patch
        sed -i '/case USB_ID(0x19f7, 0x0011):.*Rode Rodecaster Pro/a\\
\\tcase USB_ID(0x2b5a, 0x000d): /* Universal Audio Apollo Solo USB */\\
\\tcase USB_ID(0x2b5a, 0x0002): /* Universal Audio Twin USB */\\
\\tcase USB_ID(0x2b5a, 0x000f): /* Universal Audio Twin X USB */' format.c

        # Fix includes for out-of-tree build
        sed -i '1a #include <linux/usb.h>\n#include <linux/usb/audio.h>\n#include <linux/usb/audio-v2.h>\n#include <linux/usb/audio-v3.h>\n#include \"usbaudio.h\"\n#include \"mixer.h\"' mixer_maps.c

        # Create Makefile
        cat > Makefile << 'MKEOF'
KVER := \$(shell uname -r)
KDIR := /lib/modules/\$(KVER)/build

obj-m += snd-usb-audio.o
snd-usb-audio-objs := card.o clock.o endpoint.o format.o helper.o \\
    mixer.o mixer_maps.o mixer_quirks.o mixer_scarlett.o mixer_scarlett2.o \\
    mixer_us16x08.o mixer_s1810c.o pcm.o power.o proc.o quirks.o stream.o \\
    implicit.o media.o validate.o midi2.o fcp.o

ccflags-y := -O1 -Wno-error

all:
	\$(MAKE) -C \$(KDIR) M=\$(PWD) modules
MKEOF

        make 2>&1 | tail -3
    "

    # Check for the .ko file (make may return non-zero from warnings)
    if [ -f "$SND_USB_BUILD/sound/usb/snd-usb-audio.ko" ]; then
        ok "snd-usb-audio.ko built successfully"
        mkdir -p /lib/modules/"$KERNEL"/updates
        cp "$SND_USB_BUILD/sound/usb/snd-usb-audio.ko" /lib/modules/"$KERNEL"/updates/
        depmod -a
        ok "Module installed to /lib/modules/$KERNEL/updates/"
    else
        fail "Build failed — check kernel headers and gcc"
        info "Try building manually:"
        info "  cd $SND_USB_BUILD/sound/usb && make"
    fi
fi

# ================================================================
# Step 6: Load firmware and init DSP
# ================================================================
header "Initializing Apollo USB"

# Load firmware if needed
if [ "$NEEDS_FW_LOAD" -eq 1 ] && [ -f "$FW_DIR/$FW_FILE" ]; then
    info "Uploading firmware..."
    run_sudo python3 "$PROJECT_DIR/tools/fx3-load.py" 2>/dev/null
    sleep 3
    # Check if device re-enumerated
    if lsusb 2>/dev/null | grep -qi "2b5a:000d\|2b5a:0002\|2b5a:000f"; then
        ok "Firmware loaded — device re-enumerated"
    else
        warn "Device didn't re-enumerate — may need USB replug"
    fi
fi

# Reload snd-usb-audio with patched module
if [ -f "/lib/modules/$KERNEL/updates/snd-usb-audio.ko" ]; then
    info "Loading patched snd-usb-audio..."
    run_sudo rmmod snd_usb_audio 2>/dev/null || true
    sleep 1
    run_sudo modprobe snd_usb_audio
    sleep 1
    if grep -qi "Apollo" /proc/asound/cards 2>/dev/null; then
        ok "ALSA card detected: $(grep -i Apollo /proc/asound/cards | head -1 | xargs)"
    else
        warn "Apollo not in ALSA cards — may need firmware load first"
    fi
fi

# Run DSP init
if lsusb 2>/dev/null | grep -qi "2b5a:000d\|2b5a:0002\|2b5a:000f"; then
    info "Running DSP + routing init..."
    if run_sudo python3 "$PROJECT_DIR/tools/usb-full-init.py" 2>&1 | tail -3; then
        ok "DSP initialized with routing"
    else
        warn "DSP init had issues — check USB connection"
    fi

    # Reload snd-usb-audio after init (init claims interface 0)
    info "Reloading audio module..."
    run_sudo rmmod snd_usb_audio 2>/dev/null || true
    sleep 1
    run_sudo modprobe snd_usb_audio
    sleep 1
fi

# ================================================================
# Step 7: PipeWire config
# ================================================================
header "PipeWire Configuration"

if command -v pipewire &>/dev/null; then
    info "Setting up PipeWire virtual I/O devices..."

    # WirePlumber rule for pro-audio profile
    WP_CONF_DIR="$REAL_HOME/.config/wireplumber/wireplumber.conf.d"
    mkdir -p "$WP_CONF_DIR"
    cat > "$WP_CONF_DIR/50-apollo-solo-usb.conf" << 'WPEOF'
monitor.alsa.rules = [
    {
        matches = [
            { node.name = "~alsa_input.*Apollo_Solo_USB*" }
        ]
        actions = {
            update-props = {
                session.suspend-timeout-seconds = 0
                node.pause-on-idle = false
            }
        }
    }
    {
        matches = [
            { node.name = "~alsa_output.*Apollo_Solo_USB*" }
        ]
        actions = {
            update-props = {
                session.suspend-timeout-seconds = 0
                node.pause-on-idle = false
            }
        }
    }
    {
        matches = [
            { device.name = "~alsa_card.*Apollo_Solo_USB*" }
        ]
        actions = {
            update-props = {
                device.profile = "output:analog-surround-21+input:analog-surround-21"
            }
        }
    }
]
WPEOF
    chown -R "$REAL_USER":"$REAL_USER" "$WP_CONF_DIR" 2>/dev/null || true

    # Restart PipeWire so it discovers the new ALSA card, then run setup
    info "Restarting PipeWire to detect Apollo..."
    sudo -u "$REAL_USER" XDG_RUNTIME_DIR="/run/user/$REAL_UID" \
        DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/$REAL_UID/bus" \
        systemctl --user restart pipewire wireplumber 2>/dev/null || true
    sleep 3

    if sudo -u "$REAL_USER" XDG_RUNTIME_DIR="/run/user/$REAL_UID" \
        DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/$REAL_UID/bus" \
        bash "$PROJECT_DIR/configs/pipewire/setup-apollo-solo-usb.sh" 2>/dev/null; then
        ok "PipeWire virtual I/O configured"
    else
        warn "PipeWire setup had issues — run manually:"
        info "  bash configs/pipewire/setup-apollo-solo-usb.sh"
    fi
else
    warn "PipeWire not found — skipping virtual I/O setup"
    info "Audio still works via ALSA (aplay/arecord -D hw:USB)"
fi

# ================================================================
# Step 8: Audio test
# ================================================================
header "Audio Test"

if grep -qi "Apollo" /proc/asound/cards 2>/dev/null; then
    info "Recording 2 seconds from capture..."
    timeout 3 arecord -D plughw:USB -f S32_LE -r 48000 -c 2 /tmp/apollo-usb-test.wav 2>/dev/null || true

    if [ -f /tmp/apollo-usb-test.wav ]; then
        PEAK=$(python3 -c "
import wave, struct
w = wave.open('/tmp/apollo-usb-test.wav', 'r')
n = w.getnframes()
if n > 100:
    data = w.readframes(min(2000, n))
    samples = struct.unpack('<' + 'i' * (len(data)//4), data)
    print(max(abs(s) for s in samples))
else:
    print(0)
w.close()
" 2>/dev/null || echo "0")

        if [ "$PEAK" -gt 1000 ] 2>/dev/null; then
            ok "Capture test: SIGNAL detected (peak=$PEAK)"
        else
            ok "Capture test: stream works (silent — plug in a mic/instrument to test)"
        fi
    else
        warn "Could not record — ALSA device may need PipeWire restart"
    fi

    rm -f /tmp/apollo-usb-test.wav
else
    warn "No Apollo ALSA card — skipping audio test"
fi

# ================================================================
# Done
# ================================================================
header "Installation Complete"
echo ""
ok "Apollo USB setup finished!"
echo ""
info "Virtual I/O devices available in your sound settings:"
echo "  - Apollo Mic 1       (capture, mono)"
echo "  - Apollo Mic 2       (capture, mono)"
echo "  - Apollo Mic 1+2     (capture, stereo)"
echo "  - Apollo Monitor     (playback, stereo)"
echo "  - Apollo Headphone   (playback, stereo)"
echo ""
info "To control preamp gain, 48V, monitor level:"
echo "  sudo python3 tools/usb-mixer-test.py"
echo ""
info "Note: After reboot, run DSP init again:"
echo "  sudo python3 tools/usb-full-init.py"
echo "  sudo rmmod snd_usb_audio; sudo modprobe snd_usb_audio"
echo "  bash configs/pipewire/setup-apollo-solo-usb.sh"
echo ""
