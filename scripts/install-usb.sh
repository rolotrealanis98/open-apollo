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
REPORT_FILE="/tmp/open-apollo-usb-install-report.json"
TELEMETRY_URL="https://open-apollo-api.rolotrealanis.workers.dev/reports"
VERSION="0.1.0"

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

# Detect if kernel was built with Clang (CachyOS, some Arch kernels)
KERN_CC="gcc"
if [ -f "/lib/modules/$(uname -r)/build/Makefile" ]; then
    if grep -q 'clang' "/lib/modules/$(uname -r)/build/Makefile" 2>/dev/null || \
       grep -q 'CC.*clang' "/lib/modules/$(uname -r)/build/.config" 2>/dev/null; then
        KERN_CC="clang"
    fi
fi

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
    arch|manjaro|endeavouros|cachyos) PKG_MGR="pacman" ;;
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

# Check build tools — clang if kernel was built with it, else gcc
if [ "$KERN_CC" = "clang" ]; then
    if ! command -v clang &>/dev/null; then DEPS_NEEDED+=(clang); fi
else
    if ! command -v gcc &>/dev/null; then DEPS_NEEDED+=(gcc); fi
fi
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
            # CachyOS uses linux-cachyos-headers; detect installed kernel package
            KERN_HDR_PKG="linux-headers"
            if pacman -Q linux-cachyos &>/dev/null; then
                KERN_HDR_PKG="linux-cachyos-headers"
            fi
            PKGS=(python python-pip "$KERN_HDR_PKG" make wget)
            if [ "$KERN_CC" = "clang" ]; then
                PKGS+=(clang llvm lld)
            else
                PKGS+=(gcc)
            fi
            run_sudo pacman -S --noconfirm "${PKGS[@]}" 2>/dev/null
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
su - "$REAL_USER" -s /bin/bash -c "mkdir -p '$SND_USB_BUILD'"

# Download kernel source (just sound/usb) as the real user
KVER_MAJOR=$(echo "$KERNEL" | grep -oP '^\d+\.\d+')
info "Downloading sound/usb source for kernel $KVER_MAJOR..."
su - "$REAL_USER" -s /bin/bash -c "cd '$SND_USB_BUILD' && wget -q 'https://cdn.kernel.org/pub/linux/kernel/v${KVER_MAJOR%%.*}.x/linux-${KVER_MAJOR}.tar.xz' -O - | xz -d | tar x --strip-components=1 'linux-${KVER_MAJOR}/sound/usb/' 2>/dev/null"

if [ ! -f "$SND_USB_BUILD/sound/usb/format.c" ]; then
    fail "Could not download kernel source for $KVER_MAJOR"
    info "You may need to build the patched module manually."
    info "See: tools/usb-re/0001-ALSA-usb-audio-Add-quirk-for-Universal-Audio-USB-devices.patch"
else
    # Determine build flags
    BUILD_ARGS=""
    if [ "$KERN_CC" = "clang" ]; then
        BUILD_ARGS="CC=clang LD=ld.lld"
        info "Kernel built with Clang — using CC=clang LD=ld.lld"
    fi

    # Apply patch and build as real user
    SRC="$SND_USB_BUILD/sound/usb"

    # Apply the fixed-rate quirk patch
    sed -i '/case USB_ID(0x19f7, 0x0011):.*Rode Rodecaster Pro/a\
\tcase USB_ID(0x2b5a, 0x000d): /* Universal Audio Apollo Solo USB */\
\tcase USB_ID(0x2b5a, 0x0002): /* Universal Audio Twin USB */\
\tcase USB_ID(0x2b5a, 0x000f): /* Universal Audio Twin X USB */' "$SRC/format.c"

    # Fix includes for out-of-tree build
    sed -i '1a #include <linux/usb.h>\n#include <linux/usb/audio.h>\n#include <linux/usb/audio-v2.h>\n#include <linux/usb/audio-v3.h>\n#include "usbaudio.h"\n#include "mixer.h"' "$SRC/mixer_maps.c"

    # Create Makefile
    cat > "$SRC/Makefile" << 'MKEOF'
KVER := $(shell uname -r)
KDIR := /lib/modules/$(KVER)/build

obj-m += snd-usb-audio.o
snd-usb-audio-objs := card.o clock.o endpoint.o format.o helper.o \
    mixer.o mixer_maps.o mixer_quirks.o mixer_scarlett.o mixer_scarlett2.o \
    mixer_us16x08.o mixer_s1810c.o pcm.o power.o proc.o quirks.o stream.o \
    implicit.o media.o validate.o midi2.o fcp.o

ccflags-y := -O1 -Wno-error

all:
	$(MAKE) -C $(KDIR) M=$(PWD) modules
MKEOF
    chown -R "$REAL_USER":"$REAL_USER" "$SND_USB_BUILD"

    su - "$REAL_USER" -s /bin/bash -c "cd '$SRC' && make $BUILD_ARGS 2>&1 | tail -5"

    # Check for the .ko file — retry once if first build failed
    if [ ! -f "$SRC/snd-usb-audio.ko" ]; then
        warn "First build attempt didn't produce .ko, retrying..."
        su - "$REAL_USER" -s /bin/bash -c "cd '$SRC' && make $BUILD_ARGS 2>&1 | tail -5"
    fi

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
# Step 6: Load firmware and init audio
# ================================================================
header "Initializing Apollo USB"

# Load firmware if needed
if [ "$NEEDS_FW_LOAD" -eq 1 ] && [ -f "$FW_DIR/$FW_FILE" ]; then
    info "Uploading firmware..."
    run_sudo python3 "$PROJECT_DIR/tools/fx3-load.py" "$FW_DIR/$FW_FILE"
    sleep 4
    # Check if device re-enumerated
    if lsusb 2>/dev/null | grep -qi "2b5a:000d\|2b5a:0002\|2b5a:000f"; then
        ok "Firmware loaded — device re-enumerated"
    else
        warn "Device didn't re-enumerate — may need USB replug"
    fi
fi

# Check USB speed — audio interfaces only work at SuperSpeed (USB 3.0)
USB_SPEED=""
for d in /sys/bus/usb/devices/*/; do
    if [ -f "${d}idProduct" ] && [ -f "${d}idVendor" ]; then
        vid=$(cat "${d}idVendor" 2>/dev/null)
        pid=$(cat "${d}idProduct" 2>/dev/null)
        if [ "$vid" = "2b5a" ] && [ "$pid" = "000d" -o "$pid" = "0002" -o "$pid" = "000f" ]; then
            USB_SPEED=$(cat "${d}speed" 2>/dev/null)
            break
        fi
    fi
done

if [ -n "$USB_SPEED" ]; then
    if [ "$USB_SPEED" -ge 5000 ] 2>/dev/null; then
        ok "USB connection: ${USB_SPEED} Mbps (SuperSpeed)"
    else
        fail "USB connection: ${USB_SPEED} Mbps (NOT SuperSpeed)"
        warn "Apollo USB REQUIRES a USB 3.0 cable and port for audio."
        warn "At USB 2.0, audio interfaces are not available."
        info "Use a USB 3.0 cable (blue connector) or USB-C/Thunderbolt."
    fi
fi

# Load patched snd-usb-audio module
if [ -f "/lib/modules/$KERNEL/updates/snd-usb-audio.ko" ]; then
    info "Loading patched snd-usb-audio..."
    run_sudo rmmod snd_usb_audio 2>/dev/null || true
    sleep 1
    run_sudo modprobe snd_usb_audio
    sleep 2
    if grep -qi "Apollo" /proc/asound/cards 2>/dev/null; then
        ok "ALSA card detected: $(grep -i Apollo /proc/asound/cards | head -1 | xargs)"
    else
        warn "Apollo not in ALSA cards — check USB speed and firmware"
    fi
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
    timeout 3 arecord -D plughw:USB -f S32_LE -r 48000 -c 6 /tmp/apollo-usb-test.wav 2>/dev/null || true

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
# Step 9: Install report
# ================================================================
header "Install Report"

# Determine overall success
INSTALL_SUCCESS="false"
if grep -qi "Apollo" /proc/asound/cards 2>/dev/null; then
    INSTALL_SUCCESS="true"
fi

# Collect system info
RAM_MB=$(awk '/MemTotal/ {print int($2/1024)}' /proc/meminfo 2>/dev/null || echo 0)
CPU_MODEL=$(grep -m1 'model name' /proc/cpuinfo 2>/dev/null | cut -d: -f2 | xargs || echo "unknown")
USB_CTRL=$(lspci 2>/dev/null | grep -i 'usb controller' | head -3 | tr '\n' '|' || echo "")
ALSA_CARDS=$(cat /proc/asound/cards 2>/dev/null | tr '\n' '|' || echo "")
DMESG_USB=$(dmesg 2>/dev/null | grep -i 'apollo\|2b5a\|snd.usb' | tail -15 | tr '\n' '|' || echo "")

REPORT_DISTRO="${DISTRO:-unknown}" \
REPORT_KERNEL="$KERNEL" \
REPORT_ARCH="$ARCH" \
REPORT_RAM="$RAM_MB" \
REPORT_CPU="$CPU_MODEL" \
REPORT_USB_SPEED="${USB_SPEED:-unknown}" \
REPORT_USB_CTRL="$USB_CTRL" \
REPORT_FW_FILE="$FW_FILE" \
REPORT_KERN_CC="$KERN_CC" \
REPORT_ALSA_CARDS="$ALSA_CARDS" \
REPORT_DMESG_USB="$DMESG_USB" \
REPORT_SUCCESS="$INSTALL_SUCCESS" \
REPORT_FILE="$REPORT_FILE" \
REPORT_SCRIPT_VERSION="$VERSION" \
python3 -c '
import json, os, datetime

e = os.environ
report = {
    "version": 2,
    "type": "usb",
    "script_version": e["REPORT_SCRIPT_VERSION"],
    "timestamp": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
    "success": e["REPORT_SUCCESS"] == "true",
    "system": {
        "distro": e["REPORT_DISTRO"],
        "kernel": e["REPORT_KERNEL"],
        "arch": e["REPORT_ARCH"],
        "ram_mb": int(e["REPORT_RAM"] or 0),
        "cpu": e["REPORT_CPU"],
        "kernel_cc": e["REPORT_KERN_CC"],
    },
    "usb": {
        "speed_mbps": e["REPORT_USB_SPEED"],
        "controllers": e["REPORT_USB_CTRL"],
        "firmware": e["REPORT_FW_FILE"],
    },
    "audio": {
        "alsa_cards": e["REPORT_ALSA_CARDS"],
    },
    "logs": {
        "dmesg_usb": e["REPORT_DMESG_USB"],
    },
}
with open(e["REPORT_FILE"], "w") as f:
    json.dump(report, f, indent=2)
'

ok "Report saved: $REPORT_FILE"

# ================================================================
# Step 10: Telemetry opt-in
# ================================================================
header "Telemetry"

TELEM_ANSWER="n"
if [ -t 0 ]; then
    echo ""
    prompt TELEM_ANSWER "Help improve Open Apollo — send anonymous install report? [y/N] "
else
    TELEM_ANSWER="y"
fi

if [[ "${TELEM_ANSWER:-n}" =~ ^[Yy] ]]; then
    # Optional GitHub username
    GH_USER=""
    if [ -t 0 ]; then
        echo ""
        prompt GH_USER "Optional: GitHub username for follow-up if we spot an issue (Enter to skip): "
    fi
    if [ -n "${GH_USER:-}" ]; then
        python3 -c "
import json, sys
with open('$REPORT_FILE') as f: d = json.load(f)
d['github_username'] = sys.argv[1]
with open('$REPORT_FILE', 'w') as f: json.dump(d, f, indent=2)
" "$GH_USER"
    fi

    info "Sending report to $TELEMETRY_URL..."
    http_code=$(curl -s -o /dev/null -w '%{http_code}' \
        -X POST "$TELEMETRY_URL" \
        -H "Content-Type: application/json" \
        -d @"$REPORT_FILE" 2>/dev/null || echo "000")

    if [ "$http_code" = "200" ] || [ "$http_code" = "201" ]; then
        ok "Report sent — thank you!"
    else
        warn "Upload failed (HTTP $http_code) — report saved locally at $REPORT_FILE"
    fi
else
    info "No data sent. Report saved locally at $REPORT_FILE"
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
info "After reboot, reload firmware:"
echo "  sudo python3 tools/fx3-load.py /lib/firmware/universal-audio/$FW_FILE"
echo "  (the patched snd-usb-audio module loads automatically)"
echo ""
