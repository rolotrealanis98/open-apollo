#!/bin/bash
# install.sh — One-command Open Apollo installer
#
# Detects distro, installs deps, builds driver, sets up DKMS,
# deploys configs, runs hardware init, generates install report.
#
# Usage:
#   sudo bash scripts/install.sh
#   sudo bash scripts/install.sh --skip-init --no-dkms
#   pkexec bash scripts/install.sh --yes      (no questions; the setup GUI uses this)
#   bash scripts/install.sh --help

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPORT_FILE="/tmp/open-apollo-install-report.json"
TELEMETRY_URL="https://open-apollo-api.rolotrealanis.workers.dev/reports"
VERSION="0.2.0"
SOURCE="${OPEN_APOLLO_SOURCE:-user}"

# --- Flags ---
SKIP_INIT=0
NO_DKMS=0
ASSUME_YES=0
SEND_REPORT=0
PLUGIN_REPORT=1
PLUGIN_HOSTING_FROM=""
PLUGIN_HOSTING_CONF=/etc/modprobe.d/open-apollo-plugin-hosting.conf
# Only main() parses arguments, so sourcing this file (tests do) is
# side-effect free.
parse_args() {
    local arg
    for arg in "$@"; do
        case "$arg" in
            --skip-init) SKIP_INIT=1 ;;
            --no-dkms)   NO_DKMS=1 ;;
            -y|--yes)    ASSUME_YES=1 ;;
            --send-report) SEND_REPORT=1 ;;
            --no-plugin-report) PLUGIN_REPORT=0 ;;
            --plugin-hosting=*) PLUGIN_HOSTING_FROM="${arg#*=}" ;;
            -h|--help)
                cat <<EOF
Usage: sudo bash scripts/install.sh [OPTIONS]

Options:
  --skip-init   Skip hardware init (apollo-init.sh)
  --no-dkms     Skip DKMS setup
  -y, --yes     Ask nothing (for GUIs and scripts): take every question's
                default answer, skip the power-cycle pause and the audio
                test; the install report stays local unless
                --send-report is given too
  --send-report Upload the anonymous install report at the end (opt-in).
                Without it the report is uploaded only if you answer yes
                when asked at a terminal; otherwise it stays in
                /tmp/open-apollo-install-report.json.  Sent reports also
                list the plug-ins your Apollo is authorized for (render ids
                only; see --no-plugin-report)
  --no-plugin-report
                Leave the authorized plug-ins out of a sent report
  --plugin-hosting=ROOT
                Experimental: host your UA plug-ins on the Apollo's DSP.
                Loads the driver with plugin_host=1 input_strips=1 and runs
                scripts/setup-plugin-hosting.sh --from ROOT as you (ROOT is
                your Windows UAD install, as given to setup). Only plug-ins
                the Apollo authorizes are mapped, in the background, each one
                offered as soon as it is mapped. See mixer-engine/NATIVE-HOSTING.md
  -h, --help    Show this help

Steps performed:
  1. Detect distro and package manager
  2. Check and install missing dependencies
  3. Build kernel module
  4. (Optional) Set up DKMS for auto-rebuild on kernel updates
  5. Deploy udev, PipeWire/WirePlumber/UCM2 configs
  6. Load/verify the driver if Apollo hardware detected
     and enable the mixer daemon (systemd user unit)
  7. Generate install report
  8. Opt-in anonymous install report (asked at a terminal, or --send-report)

Every run leaves a log (user name, host name and serials taken out) in
~/.local/state/open-apollo/install-<date>-<time>.log of the user who ran
sudo (/var/log/open-apollo/ without one): attach it to a bug report.
EOF
                exit 0
                ;;
            *) echo "Unknown option: $arg (try --help)"; exit 1 ;;
        esac
    done
}

# --- Colors ---
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

info()    { echo -e "${CYAN}[INFO]${NC}  $*"; }
ok()      { echo -e "${GREEN}[ OK ]${NC}  $*"; }
warn()    { echo -e "${YELLOW}[WARN]${NC}  $*"; }
fail()    { echo -e "${RED}[FAIL]${NC}  $*"; }
header()  { echo -e "\n${BOLD}── $* ──${NC}"; }

# Thunderbolt-authorization and Secure Boot guidance (read-only; prints the
# exact boltctl / mokutil commands, never runs them).
hc_warn() { warn "$@"; }
hc_info() { info "$@"; }
hc_cmd()  { echo -e "          ${BOLD}$*${NC}"; }
# shellcheck source=scripts/oa-log.sh
. "$SCRIPT_DIR/oa-log.sh"
# shellcheck source=scripts/apollo-hostcheck.sh
[ -f "$SCRIPT_DIR/apollo-hostcheck.sh" ] && . "$SCRIPT_DIR/apollo-hostcheck.sh"
SB_GUIDANCE_SHOWN=0
# Print the Secure Boot / MOK guidance once per run; $1 = failed load output.
secureboot_guidance() {
    declare -F hostcheck_secureboot >/dev/null || return 0
    [ "$SB_GUIDANCE_SHOWN" = "1" ] && return 0
    hostcheck_secureboot "${1:-}" || SB_GUIDANCE_SHOWN=1
    return 0
}

# --- Prompt helper ---
# Reads from /dev/tty so prompts work even when sudo password is piped via stdin
# (--yes: never; the answer is the default)
prompt() {
    local varname="$1"; shift
    if [ "$ASSUME_YES" != "1" ] && [ -e /dev/tty ]; then
        read -rp "$*" "$varname" < /dev/tty
    else
        eval "$varname=''"
    fi
}

# Returns true if we can show interactive prompts
can_prompt() { [ "$ASSUME_YES" != "1" ] && [ -e /dev/tty ]; }

# --- Sudo helper ---
run_sudo() {
    if [ "$(id -u)" -eq 0 ]; then
        "$@"
    else
        sudo "$@"
    fi
}

# --- Desktop user helpers ---
# The user who ran sudo owns the audio session and the mixer daemon.
desktop_user() { echo "${SUDO_USER:-$(logname 2>/dev/null || echo "")}"; }

# Run a command as the desktop user with a working systemd --user bus.
as_desktop_user() {
    local u uid
    u=$(desktop_user)
    uid=$(id -u "$u" 2>/dev/null || echo "")
    [ -n "$u" ] && [ -n "$uid" ] || return 1
    sudo -u "$u" XDG_RUNTIME_DIR="/run/user/$uid" \
        DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/$uid/bus" "$@"
}

# --- Initramfs regen (dracut bakes /etc/modules-load.d into the initrd) ---
regen_initramfs() {
    if command -v dracut &>/dev/null; then
        info "Regenerating initramfs (dracut -f)..."
        run_sudo dracut -f 2>/dev/null || warn "dracut -f failed — run it manually"
    elif command -v update-initramfs &>/dev/null; then
        info "Regenerating initramfs (update-initramfs -u)..."
        run_sudo update-initramfs -u 2>/dev/null || warn "update-initramfs failed — run it manually"
    elif command -v mkinitcpio &>/dev/null; then
        info "Regenerating initramfs (mkinitcpio -P)..."
        run_sudo mkinitcpio -P 2>/dev/null || warn "mkinitcpio failed — run it manually"
    fi
}

# --- Report state ---
declare -A STEP_STATUS=()
declare -A STEP_DETAIL=()
DISTRO="" DISTRO_ID="" DISTRO_LIKE="" PKG_MGR="" KERNEL="" ARCH="" RAM_MB="" CPU_MODEL="" KERNEL_CC=""
SECURE_BOOT="" IOMMU="" PW_VER="" WP_VER="" PA_RUNNING=""
EXISTING_CARDS="" SND_MODULES="" TB_CONTROLLER=""
APOLLO_PCIE="" APOLLO_DEV_TYPE="" APOLLO_DEV_NAME=""
NEEDS_REBOOT=0
DMESG_APOLLO="" DMESG_TB="" DMESG_IOMMU=""
DEPS_INSTALLED=""
BUILD_WARNINGS=0
DMESG_FULL="" IOMMU_GROUPS="" AUDIO_APLAY="" AUDIO_ARECORD=""

# ================================================================
# Step 1: Detect distro
# ================================================================
detect_distro() {
    header "Detecting System"

    KERNEL=$(uname -r)
    ARCH=$(uname -m)
    RAM_MB=$(awk '/MemTotal/{printf "%d", $2/1024}' /proc/meminfo 2>/dev/null || echo "unknown")
    CPU_MODEL=$(grep -m1 'model name' /proc/cpuinfo 2>/dev/null | cut -d: -f2 | xargs || echo "unknown")

    # Secure boot
    if command -v mokutil &>/dev/null; then
        if mokutil --sb-state 2>/dev/null | grep -qi "enabled"; then
            SECURE_BOOT="true"
        else
            SECURE_BOOT="false"
        fi
    else
        SECURE_BOOT="unknown"
    fi

    # IOMMU
    IOMMU=$(dmesg 2>/dev/null | grep -oP '(AMD-Vi|Intel-IOMMU|IOMMU)' | head -1 || echo "none")
    DMESG_IOMMU=$(dmesg 2>/dev/null | grep -i iommu | head -5 | tr '\n' ' ' || echo "")

    # Distro detection
    if [ -f /etc/os-release ]; then
        DISTRO=$(. /etc/os-release && echo "${PRETTY_NAME:-$ID}")
        DISTRO_ID=$(. /etc/os-release && echo "${ID:-unknown}")
        DISTRO_LIKE=$(. /etc/os-release && echo "${ID_LIKE:-}")
    else
        DISTRO="unknown"
        DISTRO_ID="unknown"
        DISTRO_LIKE=""
    fi

    case "$DISTRO_ID" in
        fedora|rhel|centos) PKG_MGR="dnf" ;;
        ubuntu|debian|pop|linuxmint) PKG_MGR="apt" ;;
        arch|manjaro|endeavouros|cachyos) PKG_MGR="pacman" ;;
        opensuse*|sles) PKG_MGR="zypper" ;;
        nixos) PKG_MGR="nix" ;;
        *) PKG_MGR="" ;;
    esac
    # Derivatives (Omarchy: ID=omarchy, ID_LIKE=arch) use their parent's
    # package manager and package names.
    if [ -z "$PKG_MGR" ]; then
        for like in $DISTRO_LIKE; do
            case "$like" in
                fedora|rhel|centos) PKG_MGR="dnf" ;;
                debian|ubuntu) PKG_MGR="apt" ;;
                arch) PKG_MGR="pacman" ;;
                suse|opensuse*) PKG_MGR="zypper" ;;
            esac
            [ -n "$PKG_MGR" ] && break
        done
    fi
    [ -z "$PKG_MGR" ] && warn "Unsupported distro: $DISTRO_ID — dependency install will be manual"

    # Detect if kernel was built with Clang (needed for module compilation)
    local kbuild="/lib/modules/$KERNEL/build"
    if [ -f "$kbuild/include/config/CC_IS_CLANG" ]; then
        KERNEL_CC="clang"
        info "Kernel built with Clang — will use CC=clang for module build"
    else
        KERNEL_CC="gcc"
    fi

    # Audio info
    PW_VER=$(pipewire --version 2>/dev/null | grep -oP '[0-9]+\.[0-9]+\.[0-9]+' | head -1 || echo "")
    WP_VER=$(wpctl --version 2>/dev/null | grep -oP '[0-9]+\.[0-9]+\.[0-9]+' | head -1 || echo "")
    PA_RUNNING="false"
    if pgrep -x pulseaudio &>/dev/null; then PA_RUNNING="true"; fi
    EXISTING_CARDS=$(aplay -l 2>/dev/null | grep '^card' | sed 's/card [0-9]*: //' | cut -d, -f1 | tr '\n' '|' || echo "")
    SND_MODULES=$(lsmod 2>/dev/null | grep '^snd' | awk '{print $1}' | tr '\n' '|' || echo "")
    TB_CONTROLLER=$(lspci 2>/dev/null | grep -i thunderbolt | head -1 | sed 's/^[^ ]* //' || echo "")

    # Apollo detection
    APOLLO_PCIE=$(lspci -d 1a00: 2>/dev/null | head -1 || echo "")

    ok "Distro: $DISTRO"
    info "Kernel: $KERNEL | Arch: $ARCH | RAM: ${RAM_MB}MB"
    [ -n "$PKG_MGR" ] && info "Package manager: $PKG_MGR"
    [ -n "$TB_CONTROLLER" ] && info "Thunderbolt: $TB_CONTROLLER"
    if [ -n "$APOLLO_PCIE" ]; then
        ok "Apollo detected: $APOLLO_PCIE"
    else
        warn "No Apollo hardware detected (lspci -d 1a00:)"
        # An unauthorized Thunderbolt device keeps the Apollo off the PCI bus
        declare -F hostcheck_thunderbolt >/dev/null && hostcheck_thunderbolt
    fi

    STEP_STATUS[distro]="ok"
}

# ================================================================
# Step 2: Check and install dependencies
# ================================================================
check_install_deps() {
    header "Dependencies"

    # NixOS: dependencies should be declared in configuration.nix, not installed imperatively
    if [ "$PKG_MGR" = "nix" ]; then
        warn "NixOS detected — install dependencies declaratively in configuration.nix"
        info "Required packages: gcc, gnumake, linux-headers, python3, python3-websockets, python3-zeroconf,"
        info "  pipewire, wireplumber, alsa-utils, dkms, pciutils, usbutils"
        info "Then run: sudo nixos-rebuild switch"
        echo ""
        # Still check if the tools are present
        local nix_missing=()
        command -v gcc &>/dev/null    || nix_missing+=("gcc")
        command -v make &>/dev/null   || nix_missing+=("make")
        command -v python3 &>/dev/null || nix_missing+=("python3")
        [ -d "/lib/modules/$(uname -r)/build" ] || nix_missing+=("kernel headers")
        if [ ${#nix_missing[@]} -eq 0 ]; then
            ok "All required tools present"
            STEP_STATUS[deps]="ok"
            STEP_DETAIL[deps]="NixOS — tools verified"
        else
            warn "Missing: ${nix_missing[*]}"
            STEP_STATUS[deps]="manual"
            STEP_DETAIL[deps]="NixOS — add to configuration.nix: ${nix_missing[*]}"
        fi
        return 0
    fi

    local missing=()
    local missing_pkgs=()

    # Kernel headers
    if [ ! -d "/lib/modules/$(uname -r)/build" ]; then
        missing+=("kernel headers")
        case "$PKG_MGR" in
            dnf)    missing_pkgs+=("kernel-devel") ;;
            apt)    missing_pkgs+=("linux-headers-$(uname -r)") ;;
            # Asahi Linux (Apple Silicon) ships its kernel as linux-asahi
            pacman) if pacman -Qq linux-asahi &>/dev/null; then missing_pkgs+=("linux-asahi-headers")
                    else missing_pkgs+=("linux-headers"); fi ;;
            zypper) missing_pkgs+=("kernel-devel") ;;
        esac
    else
        ok "Kernel headers"
    fi

    # Compiler (clang if kernel was built with clang, otherwise gcc)
    if [ "$KERNEL_CC" = "clang" ]; then
        if ! command -v clang &>/dev/null; then
            missing+=("clang")
            case "$PKG_MGR" in
                dnf)    missing_pkgs+=("clang") ;;
                apt)    missing_pkgs+=("clang") ;;
                pacman) missing_pkgs+=("clang") ;;
                zypper) missing_pkgs+=("clang") ;;
            esac
        else
            ok "clang (kernel built with Clang)"
        fi
    else
        if ! command -v gcc &>/dev/null; then
            missing+=("gcc")
            missing_pkgs+=("gcc")
        else
            ok "gcc"
        fi
    fi

    # make
    if ! command -v make &>/dev/null; then
        missing+=("make")
        missing_pkgs+=("make")
    else
        ok "make"
    fi

    # python3 >= 3.10
    if python3 -c 'import sys; assert sys.version_info >= (3,10)' 2>/dev/null; then
        ok "python3 ($(python3 --version 2>&1 | awk '{print $2}'))"
    else
        missing+=("python3 (>= 3.10)")
        missing_pkgs+=("python3")
    fi

    # DKMS (needed for DKMS step)
    if [ "$NO_DKMS" = "0" ]; then
        if ! command -v dkms &>/dev/null; then
            missing+=("dkms")
            missing_pkgs+=("dkms")
        else
            ok "dkms"
        fi
    fi

    # Optional but recommended: pipewire, wireplumber, pavucontrol
    if ! command -v pw-cli &>/dev/null; then
        missing+=("pipewire")
        case "$PKG_MGR" in
            dnf)    missing_pkgs+=("pipewire") ;;
            apt)    missing_pkgs+=("pipewire") ;;
            pacman) missing_pkgs+=("pipewire" "pipewire-alsa" "pipewire-pulse") ;;
            zypper) missing_pkgs+=("pipewire") ;;
        esac
    else
        ok "pipewire${PW_VER:+ ($PW_VER)}"
    fi
    if ! command -v wpctl &>/dev/null; then
        missing+=("wireplumber")
        case "$PKG_MGR" in
            dnf)    missing_pkgs+=("wireplumber") ;;
            apt)    missing_pkgs+=("wireplumber") ;;
            pacman) missing_pkgs+=("wireplumber") ;;
            zypper) missing_pkgs+=("wireplumber") ;;
        esac
    else
        ok "wireplumber${WP_VER:+ ($WP_VER)}"
    fi
    if ! command -v pavucontrol &>/dev/null; then
        missing+=("pavucontrol")
        missing_pkgs+=("pavucontrol")
    else
        ok "pavucontrol"
    fi
    # websockets: the mixer daemon's WS:4720 server (Console/Connect clients)
    if ! python3 -c "import websockets" 2>/dev/null; then
        missing+=("python websockets")
        case "$PKG_MGR" in
            dnf|apt|zypper) missing_pkgs+=("python3-websockets") ;;
            pacman)         missing_pkgs+=("python-websockets") ;;
        esac
    else
        ok "python websockets"
    fi
    # zeroconf: the daemon's Bonjour announcement (_uamixer/_uamixerhelper)
    # so Console and ConsoleLink find it.  Without it the daemon still runs
    # and just logs that Bonjour is off.
    if ! python3 -c "import zeroconf" 2>/dev/null; then
        missing+=("python zeroconf")
        case "$PKG_MGR" in
            dnf|apt|zypper) missing_pkgs+=("python3-zeroconf") ;;
            pacman)         missing_pkgs+=("python-zeroconf") ;;
        esac
    else
        ok "python zeroconf"
    fi

    if [ ${#missing[@]} -eq 0 ]; then
        ok "All required dependencies present"
        STEP_STATUS[deps]="ok"
        STEP_DETAIL[deps]="all present"
        return 0
    fi

    echo ""
    warn "Missing: ${missing[*]}"

    if [ -z "$PKG_MGR" ]; then
        fail "Cannot auto-install on $DISTRO_ID — install manually: ${missing_pkgs[*]}"
        STEP_STATUS[deps]="fail"
        STEP_DETAIL[deps]="manual install needed: ${missing_pkgs[*]}"
        return 1
    fi

    echo ""
    local install_cmd=""
    case "$PKG_MGR" in
        dnf)    install_cmd="dnf install -y ${missing_pkgs[*]}" ;;
        apt)    install_cmd="apt-get install -y ${missing_pkgs[*]}" ;;
        pacman) install_cmd="pacman -S --noconfirm ${missing_pkgs[*]}" ;;
        zypper) install_cmd="zypper install -y ${missing_pkgs[*]}" ;;
    esac

    read -rp "Install missing deps with: sudo $install_cmd ? [Y/n] " answer
    if [[ "$answer" =~ ^[Nn] ]]; then
        warn "Skipping dependency install"
        STEP_STATUS[deps]="skipped"
        STEP_DETAIL[deps]="user declined: ${missing_pkgs[*]}"
        return 1
    fi

    info "Installing: ${missing_pkgs[*]}"
    if run_sudo $install_cmd; then
        ok "Dependencies installed"
        STEP_STATUS[deps]="ok"
        STEP_DETAIL[deps]="installed: ${missing_pkgs[*]}"
        DEPS_INSTALLED="${missing_pkgs[*]}"
    else
        fail "Package install failed"
        STEP_STATUS[deps]="fail"
        STEP_DETAIL[deps]="install failed: ${missing_pkgs[*]}"
        return 1
    fi
}

# ================================================================
# Step 3: Build driver
# ================================================================
build_driver() {
    header "Building Driver"

    local build_output kdir_arg="" cc_arg=""

    # Use clang if kernel was built with clang
    if [ "$KERNEL_CC" = "clang" ]; then
        cc_arg="CC=clang"
    fi

    # TEST_BUILD mode: find any installed kernel headers (for Docker containers
    # where the host kernel doesn't match the distro's headers package)
    if [ "${TEST_BUILD:-0}" = "1" ]; then
        local kdir
        kdir=$(find /usr/lib/modules /lib/modules -name "build" -type l 2>/dev/null | head -1)
        if [ -z "$kdir" ]; then
            warn "No kernel headers found — skipping build test"
            STEP_STATUS[build]="skipped"
            return 0
        fi
        kdir_arg="KDIR=$kdir"
        # Re-check clang for test build kernel
        if [ -f "$kdir/../include/config/CC_IS_CLANG" ] || [ -f "$(dirname "$kdir")/include/config/CC_IS_CLANG" ]; then
            cc_arg="CC=clang"
        fi
    fi

    make -C "$PROJECT_DIR/driver" $kdir_arg $cc_arg clean &>/dev/null || true

    build_output=$(make -C "$PROJECT_DIR/driver" $kdir_arg $cc_arg 2>&1)
    local rc=$?

    BUILD_WARNINGS=$(echo "$build_output" | grep -c 'warning:' || true)

    if [ $rc -eq 0 ]; then
        ok "Build successful ($BUILD_WARNINGS warning(s))"
        STEP_STATUS[build]="ok"
        STEP_DETAIL[build]="warnings: $BUILD_WARNINGS"
    else
        fail "Build failed"
        echo "$build_output" | tail -20
        STEP_STATUS[build]="fail"
        STEP_DETAIL[build]=$(echo "$build_output" | tail -5 | tr '\n' ' ')
        return 1
    fi
}

# ================================================================
# Step 4: DKMS setup
# ================================================================
setup_dkms() {
    header "DKMS Setup"

    if [ "$NO_DKMS" = "1" ]; then
        info "Skipping DKMS (--no-dkms)"
        STEP_STATUS[dkms]="skipped"
        return 0
    fi

    if ! command -v dkms &>/dev/null; then
        warn "dkms not installed — skipping"
        STEP_STATUS[dkms]="skipped"
        return 0
    fi

    read -rp "Set up DKMS for automatic rebuilds on kernel updates? [Y/n] " answer
    if [[ "$answer" =~ ^[Nn] ]]; then
        info "Skipping DKMS"
        STEP_STATUS[dkms]="skipped"
        return 0
    fi

    # The version comes from driver/dkms.conf so the registration, the
    # /usr/src tree and the config can never disagree.
    local dkms_ver
    dkms_ver=$(sed -n 's/^PACKAGE_VERSION="\(.*\)"/\1/p' "$PROJECT_DIR/driver/dkms.conf" 2>/dev/null)
    if [ -z "$dkms_ver" ]; then
        fail "driver/dkms.conf missing or has no PACKAGE_VERSION"
        STEP_STATUS[dkms]="fail"
        return 1
    fi
    local dkms_src="/usr/src/ua_apollo-$dkms_ver"

    # Remove every existing registration (any version) so an older build
    # cannot shadow this one
    local old_ver
    while read -r old_ver; do
        [ -n "$old_ver" ] || continue
        info "Removing existing DKMS registration ua_apollo/$old_ver..."
        run_sudo dkms remove "ua_apollo/$old_ver" --all 2>/dev/null || true
    done < <(dkms status ua_apollo 2>/dev/null | sed -n 's|^ua_apollo[/,] *\([^,: ]*\).*|\1|p' | sort -u)

    # Remove any manually installed .ko to avoid "Differences" warning
    run_sudo rm -f "/lib/modules/$(uname -r)/extra/ua_apollo.ko" 2>/dev/null || true

    # Copy source: exactly what driver/Makefile builds (ua_core, ua_audio, ua_dsp)
    # plus the headers and dkms.conf
    info "Copying driver source to $dkms_src"
    run_sudo rm -rf /usr/src/ua_apollo-*
    run_sudo mkdir -p "$dkms_src"
    if ! run_sudo cp "$PROJECT_DIR"/driver/*.c "$PROJECT_DIR"/driver/*.h \
            "$PROJECT_DIR"/driver/Makefile "$PROJECT_DIR"/driver/dkms.conf "$dkms_src/"; then
        fail "Could not copy driver source to $dkms_src"
        STEP_STATUS[dkms]="fail"
        return 1
    fi

    # DKMS add/build/install
    if run_sudo dkms add "ua_apollo/$dkms_ver" 2>&1; then
        ok "DKMS: added"
    else
        fail "DKMS add failed"
        STEP_STATUS[dkms]="fail"
        return 1
    fi

    if run_sudo dkms build "ua_apollo/$dkms_ver" 2>&1; then
        ok "DKMS: built"
    else
        fail "DKMS build failed"
        STEP_STATUS[dkms]="fail"
        return 1
    fi

    if run_sudo dkms install "ua_apollo/$dkms_ver" --force 2>&1; then
        ok "DKMS: installed"
    else
        fail "DKMS install failed"
        STEP_STATUS[dkms]="fail"
        return 1
    fi

    # Auto-load: the module carries PCI modaliases, so udev loads it when
    # the Thunderbolt device appears. Do NOT add a modules-load.d entry —
    # early-boot loading pulls snd_pcm whose Fedora install hook (dist-alsa.conf
    # -> snd-seq) fails inside the initramfs, failing systemd-modules-load
    # on every boot (issue #43).
    if [ -f /etc/modules-load.d/ua_apollo.conf ]; then
        run_sudo rm -f /etc/modules-load.d/ua_apollo.conf
        regen_initramfs
        ok "Removed legacy modules-load.d entry (udev modalias handles auto-load)"
    else
        ok "Auto-load via udev PCI modalias (no modules-load.d entry needed)"
    fi

    STEP_STATUS[dkms]="ok"
}

# ================================================================
# Step 4b: Check IOMMU
# ================================================================
check_iommu() {
    # Intel VT-d (DMAR) blocks BAR0 access unless iommu=pt is set.
    # Without it, all register reads return 0xFFFFFFFF and the Apollo is unusable.
    # Apple Silicon's IOMMUs (DART) also log "iommu" but have no passthrough mode;
    # the tunneled Apollo works translated there (docs/asahi-linux.md).
    [ "$(uname -m)" = aarch64 ] && return 0
    if ! dmesg 2>/dev/null | grep -qi 'DMAR\|IOMMU'; then
        return 0  # No IOMMU, nothing to do
    fi

    if grep -q 'iommu=pt' /proc/cmdline 2>/dev/null; then
        ok "IOMMU passthrough mode active"
        return 0
    fi

    warn "IOMMU (Intel VT-d) detected without passthrough mode"
    info "This will prevent the Apollo from working (BAR0 reads fail)"
    echo ""

    # NixOS: kernel params go in configuration.nix
    if [ "$DISTRO_ID" = "nixos" ]; then
        warn "NixOS: add iommu=pt to your configuration.nix:"
        info '  boot.kernelParams = [ "iommu=pt" ];'
        info "Then: sudo nixos-rebuild switch && reboot"
        NEEDS_REBOOT=1
    elif [ -f /etc/default/grub ]; then
        read -rp "Add iommu=pt to kernel command line? (requires reboot) [Y/n] " iommu_answer
        if [[ ! "$iommu_answer" =~ ^[Nn] ]]; then
            # Add iommu=pt to GRUB_CMDLINE_LINUX_DEFAULT
            if grep -q 'iommu=pt' /etc/default/grub; then
                ok "iommu=pt already in /etc/default/grub"
            else
                run_sudo sed -i 's/GRUB_CMDLINE_LINUX_DEFAULT="\(.*\)"/GRUB_CMDLINE_LINUX_DEFAULT="\1 iommu=pt"/' /etc/default/grub
                ok "Added iommu=pt to /etc/default/grub"
            fi

            # Try update-grub (may fail in chroot/overlayroot)
            if run_sudo update-grub 2>/dev/null; then
                ok "GRUB updated"
            elif run_sudo grub2-mkconfig -o /boot/grub2/grub.cfg 2>/dev/null; then
                ok "GRUB config regenerated"
            else
                warn "Could not update GRUB automatically"
                info "Run manually: sudo update-grub"
            fi

            echo ""
            warn "REBOOT REQUIRED for iommu=pt to take effect"
            info "After reboot, run: sudo bash tools/apollo-init.sh"
            NEEDS_REBOOT=1
        fi
    else
        warn "No /etc/default/grub found — add 'iommu=pt' to your bootloader manually"
    fi
}

# ================================================================
# Step 4c: Install firmware blobs to /lib/firmware/
# ================================================================
# The driver loads neither blob by default: the cold-start firmware stage
# is off (mixer_fw_stage=0, the x4's DSPs refuse it) and so is the captured
# plugin-chain init (no_plugins=1, it faulted the IOMMU and stalled DSP 0's
# command ring on the x4). PAD/48V/Mic-Line go through the mixer settings.
# A plugin-chain blob left by an older install is removed; the mixer blob
# is installed only when one sits in the repo root, for mixer_fw_stage=1.
install_firmware() {
    header "Installing Firmware Blobs"

    local mx_src="$PROJECT_DIR/ua-apollo-mixer.bin"
    local dst="/lib/firmware"
    local any=0

    run_sudo mkdir -p "$dst"

    if [ -f "$dst/ua-apollo-plugin-chain.bin" ]; then
        if run_sudo rm -f "$dst/ua-apollo-plugin-chain.bin"; then
            ok "removed old plugin-chain firmware (not used by default)"
        else
            warn "could not remove $dst/ua-apollo-plugin-chain.bin"
        fi
    fi

    if [ -f "$mx_src" ]; then
        if run_sudo install -m 644 "$mx_src" "$dst/ua-apollo-mixer.bin"; then
            ok "mixer firmware: $dst/ua-apollo-mixer.bin"
            any=1
        fi
    fi

    STEP_STATUS[firmware]=$([ "$any" = "1" ] && echo ok || echo skipped)
}

# ================================================================
# Step 5: Deploy configs
# ================================================================
deploy_configs() {
    header "Deploying Configs"

    if [ ! -f "$PROJECT_DIR/configs/deploy.sh" ]; then
        warn "configs/deploy.sh not found — skipping"
        STEP_STATUS[configs]="fail"
        STEP_DETAIL[configs]="deploy.sh not found"
        return 1
    fi

    if run_sudo bash "$PROJECT_DIR/configs/deploy.sh" 2>&1; then
        ok "PipeWire/WirePlumber/UCM2 configs deployed"
        STEP_STATUS[configs]="ok"
        # New WirePlumber rules only apply to a fresh WirePlumber
        if as_desktop_user systemctl --user restart wireplumber.service 2>/dev/null; then
            ok "WirePlumber restarted (new rules active)"
        else
            warn "Could not restart WirePlumber — run: systemctl --user restart wireplumber"
        fi
    else
        fail "Config deployment failed"
        STEP_STATUS[configs]="fail"
        return 1
    fi
}

# Driver v2 defaults (io_map=-1 auto, mixer_mailbox=1) follow the macOS driver
# on every model (hardware-verified on the x4 and x8p only); an old override in
# /etc/modprobe.d can silently pin the legacy layout.  The installer writes
# no modprobe options (except the opt-in --plugin-hosting file), so only warn.
check_modprobe_options() {
    local f hits=""
    for f in /etc/modprobe.d/*.conf; do
        [ -f "$f" ] || continue
        if grep -Eq '^[[:space:]]*options[[:space:]]+ua_apollo[[:space:]].*(io_map|mixer_mailbox)=' "$f"; then
            hits+=" $f"
        fi
    done
    if [ -n "$hits" ]; then
        warn "Old ua_apollo io_map/mixer_mailbox options found in:$hits"
        info "Driver v2 defaults (io_map=-1 auto, mixer_mailbox=1) are recommended — remove those options unless you set them on purpose"
    fi
}

# ================================================================
# Opt-in native plug-in hosting (--plugin-hosting=ROOT)
# ================================================================
# Before the driver loads: its plug-in host and input strips are off by default.
plugin_hosting_options() {
    [ -n "$PLUGIN_HOSTING_FROM" ] || return 0
    printf '%s\n' "# Written by scripts/install.sh --plugin-hosting (experimental native plug-in hosting)." \
        "options ua_apollo plugin_host=1 input_strips=1" | run_sudo tee "$PLUGIN_HOSTING_CONF" >/dev/null
    ok "Driver options for plug-in hosting: $PLUGIN_HOSTING_CONF"
}

# After the daemon: map the user's plug-ins (as them) and point the daemon at the bundles.
setup_plugin_hosting() {
    [ -n "$PLUGIN_HOSTING_FROM" ] || return 0
    header "Plug-ins on the Apollo (experimental)"
    info "Mapping the Powered plug-ins your Apollo authorizes, offline from $PLUGIN_HOSTING_FROM (nothing leaves this computer)"
    info "It goes on in the background after setup; each plug-in appears in the Console as soon as it is mapped"
    if as_desktop_user OA_LOG_CAPTURED="${OA_LOG_CAPTURED:-0}" \
        bash "$PROJECT_DIR/scripts/setup-plugin-hosting.sh" --from "$PLUGIN_HOSTING_FROM"; then
        ok "Plug-in mapping started (progress: ~/.local/share/open-apollo-plugin-maps/build.log)"
        STEP_STATUS[plugins]="ok"
    else
        warn "Plug-in mapping failed — rerun: bash scripts/setup-plugin-hosting.sh --from $PLUGIN_HOSTING_FROM"
        STEP_STATUS[plugins]="fail"
    fi
}

# ================================================================
# Daemon: systemd user unit (survives reboot, restarts on failure)
# ================================================================
setup_mixer_daemon() {
    header "Mixer Daemon"

    local u daemon_script="$PROJECT_DIR/mixer-engine/ua_mixer_daemon.py"
    u=$(desktop_user)
    if [ -z "$u" ] || [ "$u" = "root" ]; then
        warn "No desktop user (run via sudo from your login) — enable it yourself:"
        info "  systemctl --user enable --now ua-mixer-daemon.service"
        STEP_STATUS[daemon]="skipped"
        return 0
    fi
    if [ ! -f "$daemon_script" ] || [ ! -f /etc/systemd/user/ua-mixer-daemon.service ]; then
        warn "Mixer daemon script or user unit missing — skipping"
        STEP_STATUS[daemon]="skipped"
        return 0
    fi

    # The daemon persists state under mixer-engine/state; it runs as the
    # desktop user, so that directory must belong to them.
    run_sudo install -d -o "$u" -g "$(id -gn "$u")" "$PROJECT_DIR/mixer-engine/state"

    # Replace any hand-started daemon from older installers
    pkill -f "ua_mixer_daemon" 2>/dev/null || true

    as_desktop_user systemctl --user daemon-reload 2>/dev/null || true
    if as_desktop_user systemctl --user enable --now ua-mixer-daemon.service 2>&1; then
        sleep 2
        if as_desktop_user systemctl --user is-active --quiet ua-mixer-daemon.service; then
            ok "Mixer daemon running as $u (TCP:4710, WS:4720), enabled at login"
            STEP_STATUS[daemon]="ok"
        else
            warn "Mixer daemon not active — journalctl --user -u ua-mixer-daemon"
            STEP_STATUS[daemon]="fail"
        fi
    else
        warn "Could not enable ua-mixer-daemon.service for $u (no user systemd session?)"
        info "  After logging in: systemctl --user enable --now ua-mixer-daemon.service"
        STEP_STATUS[daemon]="fail"
    fi
}

# ================================================================
# Step 6: Verify hardware + PipeWire
# ================================================================
# Domain-qualified PCI address of the Apollo (vendor 1a00, device 0002),
# or "" when it is not on the bus.  Matching the vendor alone would also
# pick up UAD-2 PCIe DSP cards, which this driver binds as well.
apollo_pci_bdf() {
    lspci -D -d 1a00:0002 2>/dev/null | awk 'NR == 1 { print $1 }'
}

# Name of the driver behind a /sys/bus/pci/devices/<bdf>/driver link, or
# "" when nothing is bound (or the link path is empty/missing).
apollo_bound_driver() {
    local driver_link="$1"

    [ -L "$driver_link" ] || return 0
    basename "$(readlink -f "$driver_link")"
}

apollo_driver_bound() {
    [ "$(apollo_bound_driver "$1")" = "ua_apollo" ]
}

run_init() {
    header "Hardware Verification"

    if [ "$SKIP_INIT" = "1" ]; then
        info "Skipping verification (--skip-init)"
        STEP_STATUS[init]="skipped"
        return 0
    fi

    if [ -z "$APOLLO_PCIE" ]; then
        info "No Apollo hardware detected on Thunderbolt bus"
        info "Connect Apollo and it will auto-configure via udev"
        STEP_STATUS[init]="skipped"
        STEP_DETAIL[init]="no hardware detected"
        return 0
    fi

    # Stop PipeWire before loading driver to prevent race condition.
    # PipeWire auto-opens the ALSA device the moment it appears, which can
    # start transport before the TB link is fully stable → PCIe link death.
    local pw_user="${SUDO_USER:-$(logname 2>/dev/null || echo "")}"
    local pw_uid
    pw_uid=$(id -u "$pw_user" 2>/dev/null || echo "")
    local pw_home
    pw_home=$(eval echo "~$pw_user" 2>/dev/null || echo "")

    if [ -n "$pw_user" ] && [ -n "$pw_uid" ]; then
        info "Stopping PipeWire (prevents race during driver init)..."
        sudo -u "$pw_user" XDG_RUNTIME_DIR="/run/user/$pw_uid" \
            systemctl --user stop pipewire.service wireplumber.service \
            pipewire.socket pipewire-pulse.socket pipewire-pulse.service 2>/dev/null || true
        sleep 1
        ok "PipeWire stopped"
    fi

    # Gate 1: Load the driver
    if ! lsmod | grep -q ua_apollo; then
        info "Loading driver..."
        local load_err
        if load_err=$(run_sudo modprobe ua_apollo 2>&1 || run_sudo insmod "$PROJECT_DIR/driver/ua_apollo.ko" 2>&1); then
            ok "Driver loaded"
        else
            fail "Could not load driver — check dmesg for details"
            [ -n "$load_err" ] && info "$load_err"
            # Secure Boot rejecting the unsigned module: explain MOK enrollment
            secureboot_guidance "$load_err"
            STEP_STATUS[init]="fail"
            STEP_DETAIL[init]="modprobe/insmod failed"
            return 1
        fi
    else
        # A module from an older install keeps running until reloaded. v2
        # unloads cleanly, so swap it for the freshly installed one.
        local loaded_sv new_sv
        loaded_sv=$(cat /sys/module/ua_apollo/srcversion 2>/dev/null || echo "")
        new_sv=$(modinfo -F srcversion ua_apollo 2>/dev/null || echo "")
        if [ -n "$new_sv" ] && [ "$loaded_sv" != "$new_sv" ]; then
            info "Loaded driver is out of date — reloading..."
            pkill -f "ua_mixer_daemon" 2>/dev/null || true
            if run_sudo rmmod ua_apollo 2>/dev/null && run_sudo modprobe ua_apollo 2>/dev/null; then
                ok "Driver reloaded"
            else
                warn "Could not reload the driver — reboot to use the new build"
                NEEDS_REBOOT=1
            fi
        else
            ok "Driver already loaded"
        fi
    fi

    # Gate 2: Verify the loaded module claimed the Apollo.  The kernel
    # creates the sysfs driver link when probe starts and removes it if
    # probe fails, so no link here means probe already failed (or the
    # device left the bus) and a different name means another driver
    # owns the device.
    local apollo_bdf driver_link bound_driver
    apollo_bdf=$(apollo_pci_bdf)
    if [ -z "$apollo_bdf" ]; then
        fail "Apollo is no longer visible on the PCI bus — check the Thunderbolt link and power cycle"
        STEP_STATUS[init]="fail"
        STEP_DETAIL[init]="Apollo left PCI bus before driver bind"
        return 1
    fi
    driver_link="/sys/bus/pci/devices/$apollo_bdf/driver"
    bound_driver=$(apollo_bound_driver "$driver_link")
    if [ -z "$bound_driver" ]; then
        fail "Driver loaded but not bound to Apollo at $apollo_bdf — check dmesg for probe errors"
        STEP_STATUS[init]="fail"
        STEP_DETAIL[init]="module loaded but device unbound"
        return 1
    elif [ "$bound_driver" != "ua_apollo" ]; then
        fail "Apollo at $apollo_bdf is bound to '$bound_driver', not ua_apollo — unbind it first"
        STEP_STATUS[init]="fail"
        STEP_DETAIL[init]="device bound to $bound_driver"
        return 1
    fi
    ok "Driver bound to Apollo ($apollo_bdf)"

    # Gate 3: Wait for device node (driver probe complete).  Probe may
    # still be running when Gate 2 passes; if it fails now the kernel
    # drops the driver link, so bail out early instead of waiting it out.
    info "Waiting for device node..."
    local tries=0
    while [ ! -e /dev/ua_apollo0 ] && [ $tries -lt 50 ]; do
        if ! apollo_driver_bound "$driver_link"; then
            fail "Driver probe failed — Apollo unbound (check dmesg for the error)"
            STEP_STATUS[init]="fail"
            STEP_DETAIL[init]="probe failed after bind"
            return 1
        fi
        sleep 0.2
        tries=$((tries + 1))
    done
    if [ -e /dev/ua_apollo0 ]; then
        ok "Device node: /dev/ua_apollo0"
    else
        fail "Device node /dev/ua_apollo0 did not appear after 10s"
        STEP_STATUS[init]="fail"
        STEP_DETAIL[init]="no device node"
        return 1
    fi

    # Gate 4: Wait for ACEFACE connect (DSP handshake)
    # Check dmesg (needs sudo) or verify ALSA card appeared (indirect confirmation).
    info "Waiting for DSP handshake..."
    local aceface_ok=0
    for i in $(seq 1 30); do
        # Try dmesg with sudo first, fall back to checking ALSA card
        if run_sudo dmesg 2>/dev/null | grep -q "audio extension connected\|ACEFACE already done"; then
            aceface_ok=1
            break
        fi
        # ALSA card appearing means driver probed + ACEFACE completed
        if aplay -l 2>/dev/null | grep -qi apollo; then
            aceface_ok=1
            break
        fi
        sleep 1
    done
    if [ $aceface_ok -eq 1 ]; then
        ok "DSP handshake complete"
    else
        warn "DSP handshake not confirmed after 30s — proceeding cautiously"
    fi

    # Gate 5: Verify ALSA card registered (retry up to 10s)
    local alsa_ok=0
    for i in $(seq 1 10); do
        if aplay -l 2>/dev/null | grep -qi apollo; then
            alsa_ok=1
            break
        fi
        sleep 1
    done
    if [ $alsa_ok -eq 1 ]; then
        ok "ALSA card registered"
    else
        warn "ALSA card not found — audio may not work"
    fi

    # Gate 6: Verify hardware is responsive (read a register)
    # Check that BAR0 reads don't return 0xFFFFFFFF
    if dmesg 2>/dev/null | grep -q "device unreachable\|PCIe error detected"; then
        fail "Apollo PCIe link is down — power cycle Apollo and try again"
        STEP_STATUS[init]="fail"
        STEP_DETAIL[init]="PCIe link dead"
        return 1
    fi
    ok "Hardware responding"

    # Gate 7: Set up PipeWire virtual I/O devices
    local pw_user="${SUDO_USER:-$(logname 2>/dev/null || echo "")}"
    local pw_uid
    pw_uid=$(id -u "$pw_user" 2>/dev/null || echo "")

    if [ -n "$pw_user" ] && [ -n "$pw_uid" ] && command -v wpctl > /dev/null 2>&1; then
        local pw_home
        pw_home=$(eval echo "~$pw_user")

        # Restart PipeWire so newly-deployed WirePlumber rules take effect
        info "Restarting PipeWire to load new configs..."
        sudo -u "$pw_user" XDG_RUNTIME_DIR="/run/user/$pw_uid" \
            systemctl --user restart pipewire pipewire-pulse.socket pipewire-pulse.service wireplumber 2>/dev/null || true
        sleep 3

        info "Setting up PipeWire audio devices..."
        sudo -u "$pw_user" HOME="$pw_home" XDG_RUNTIME_DIR="/run/user/$pw_uid" \
            /usr/local/bin/apollo-setup-io 2>&1 || true

        # Gate 8: Verify virtual devices appeared
        sleep 2
        local vdev_count
        vdev_count=$(sudo -u "$pw_user" XDG_RUNTIME_DIR="/run/user/$pw_uid" \
            wpctl status 2>/dev/null | grep -ic 'apollo' || echo 0)
        if [ "$vdev_count" -ge 5 ]; then
            ok "PipeWire virtual devices active ($vdev_count Apollo nodes)"
        else
            warn "Expected 5+ Apollo nodes, found $vdev_count — try: apollo-setup-io"
        fi

        # Set Apollo Monitor as default sink
        local sink_id
        sink_id=$(sudo -u "$pw_user" XDG_RUNTIME_DIR="/run/user/$pw_uid" \
            wpctl status 2>/dev/null | grep -i 'apollo.*monitor\|apollo_monitor' | grep -oP '[0-9]+' | head -1 || true)
        if [ -n "$sink_id" ]; then
            sudo -u "$pw_user" XDG_RUNTIME_DIR="/run/user/$pw_uid" \
                wpctl set-default "$sink_id" 2>/dev/null || true
            ok "Apollo Monitor L/R set as default audio output"
        fi
    fi

    STEP_STATUS[init]="ok"
    echo ""
    info "Apollo will auto-configure on future boots and reconnects"
}

# ================================================================
# Step 7: Generate install report
# ================================================================
generate_report() {
    header "Install Report"

    # Overall success
    local overall="true"
    for key in deps build; do
        if [ "${STEP_STATUS[$key]:-}" = "fail" ]; then
            overall="false"
            break
        fi
    done

    # Write report data as env vars, then use python3 for clean JSON
    local _steps_csv=""
    for key in distro deps build dkms configs init daemon plugins; do
        _steps_csv+="${key}:${STEP_STATUS[$key]:-skipped}:${STEP_DETAIL[$key]:-}|"
    done

    # Collect extended diagnostics
    DMESG_FULL=$(dmesg 2>/dev/null | grep -i ua_apollo || echo "")
    IOMMU_GROUPS=""
    if [ -d /sys/kernel/iommu_groups ]; then
        IOMMU_GROUPS=$(for g in /sys/kernel/iommu_groups/*/devices/*; do
            group=$(echo "$g" | grep -o 'iommu_groups/[0-9]*' | grep -o '[0-9]*')
            dev=$(basename "$g")
            desc=$(lspci -s "$dev" 2>/dev/null | cut -d' ' -f2- || echo "unknown")
            echo "$group:$dev:$desc"
        done 2>/dev/null | tr '\n' '|')
    fi
    AUDIO_APLAY=$(aplay -l 2>/dev/null || echo "not available")
    AUDIO_ARECORD=$(arecord -l 2>/dev/null || echo "not available")
    # pw-dump as the desktop user (root has no PipeWire session), into a file:
    # the whole dump is too big for an environment variable.  The report keeps
    # only the Apollo's own nodes, with an allowlist of their properties.
    local pwdump_file
    pwdump_file=$(mktemp)
    if command -v pw-dump &>/dev/null; then
        as_desktop_user pw-dump --no-colors > "$pwdump_file" 2>/dev/null \
            || pw-dump --no-colors > "$pwdump_file" 2>/dev/null || true
    fi

    REPORT_DISTRO="$DISTRO" \
    REPORT_KERNEL="$KERNEL" \
    REPORT_ARCH="$ARCH" \
    REPORT_RAM="${RAM_MB:-0}" \
    REPORT_CPU="$CPU_MODEL" \
    REPORT_SECBOOT="$SECURE_BOOT" \
    REPORT_IOMMU="$IOMMU" \
    REPORT_PW_VER="${PW_VER:-}" \
    REPORT_WP_VER="${WP_VER:-}" \
    REPORT_PA="$PA_RUNNING" \
    REPORT_CARDS="$EXISTING_CARDS" \
    REPORT_MODS="$SND_MODULES" \
    REPORT_APOLLO_DET="$( [ -n "$APOLLO_PCIE" ] && echo true || echo false )" \
    REPORT_APOLLO_PCI="$APOLLO_PCIE" \
    REPORT_TB="$TB_CONTROLLER" \
    REPORT_STEPS="$_steps_csv" \
    REPORT_SUCCESS="$overall" \
    REPORT_FILE="$REPORT_FILE" \
    REPORT_SCRIPT_VERSION="$VERSION" \
    REPORT_SOURCE="$SOURCE" \
    REPORT_DMESG_FULL="$DMESG_FULL" \
    REPORT_IOMMU_GROUPS="$IOMMU_GROUPS" \
    REPORT_AUDIO_APLAY="$AUDIO_APLAY" \
    REPORT_AUDIO_ARECORD="$AUDIO_ARECORD" \
    REPORT_AUDIO_PWDUMP_FILE="$pwdump_file" \
    REPORT_USER="$(desktop_user)" \
    REPORT_LIB="$SCRIPT_DIR" \
    python3 -c '
import json, subprocess, os, sys, datetime

def dmesg_grep(pattern, lines=10):
    try:
        out = subprocess.run(["dmesg"], capture_output=True, text=True, timeout=5)
        return "\n".join([l for l in out.stdout.splitlines() if pattern.lower() in l.lower()][-lines:])
    except: return ""

def split_list(s, sep="|"):
    return [x for x in s.split(sep) if x] if s else []

def parse_steps(s):
    steps = {}
    for part in s.split("|"):
        if ":" not in part: continue
        fields = part.split(":", 2)
        key, status = fields[0], fields[1]
        entry = {"status": status}
        if len(fields) > 2 and fields[2]: entry["detail"] = fields[2]
        steps[key] = entry
    return steps

def parse_iommu_groups(s):
    groups = []
    if not s: return groups
    for entry in s.split("|"):
        if not entry: continue
        parts = entry.split(":", 2)
        if len(parts) == 3:
            groups.append({"group": parts[0], "device": parts[1], "description": parts[2]})
    return groups

# Properties of Apollo PipeWire objects that may go in the report.
PW_KEEP = {"media.class", "node.name", "node.description", "audio.channels", "audio.position",
           "audio.rate", "audio.format", "api.alsa.path", "api.alsa.pcm.card", "alsa.card",
           "alsa.card_name", "alsa.id", "device.api", "device.profile.name",
           "device.profile.description", "factory.name", "priority.session"}

def apollo_pw_nodes(path):
    """Apollo nodes and devices (allowlisted properties), plus a count of
    every other audio object by media class.  Nothing about other devices,
    clients, users or hosts."""
    try:
        with open(path) as f:
            objs = json.load(f)
    except Exception:
        return ""
    kept, others = [], {}
    for o in objs:
        props = (o.get("info") or {}).get("props") or {}
        names = " ".join(str(props.get(k, "")) for k in ("node.name", "device.name", "alsa.card_name"))
        if "apollo" in names.lower():
            kept.append({"type": str(o.get("type", "")).rsplit(":", 1)[-1],
                         "props": {k: v for k, v in props.items() if k in PW_KEEP}})
        elif props.get("media.class"):
            cls = props["media.class"]
            others[cls] = others.get(cls, 0) + 1
    return json.dumps({"apollo": kept, "other_media_classes": others}, sort_keys=True)

# The user, host, machine id, serial numbers and MAC addresses are removed
# from every string in the report (scripts/oa_redact.py, shared with the logs).
sys.path.insert(0, os.environ["REPORT_LIB"])
from oa_redact import identity_words, scrub

e = os.environ
report = {
    "version": 2,
    "script_version": e["REPORT_SCRIPT_VERSION"],
    "timestamp": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
    "success": e["REPORT_SUCCESS"] == "true",
    "source": e.get("REPORT_SOURCE", "user"),
    "type": "thunderbolt",
    "system": {
        "distro": e["REPORT_DISTRO"],
        "kernel": e["REPORT_KERNEL"],
        "arch": e["REPORT_ARCH"],
        "ram_mb": int(e["REPORT_RAM"] or 0),
        "cpu": e["REPORT_CPU"],
        "secure_boot": e["REPORT_SECBOOT"] == "true",
        "iommu": e["REPORT_IOMMU"],
    },
    "audio": {
        "pipewire_version": e["REPORT_PW_VER"] or None,
        "wireplumber_version": e["REPORT_WP_VER"] or None,
        "pulseaudio_running": e["REPORT_PA"] == "true",
        "existing_cards": split_list(e["REPORT_CARDS"]),
        "snd_modules": split_list(e["REPORT_MODS"]),
    },
    "audio_state": {
        "aplay": e.get("REPORT_AUDIO_APLAY", ""),
        "arecord": e.get("REPORT_AUDIO_ARECORD", ""),
        "pw_dump": apollo_pw_nodes(e.get("REPORT_AUDIO_PWDUMP_FILE", "")),
    },
    "apollo": {
        "detected": e["REPORT_APOLLO_DET"] == "true",
        "pcie_device": e["REPORT_APOLLO_PCI"],
        "thunderbolt_controller": e["REPORT_TB"],
    },
    "iommu_groups": parse_iommu_groups(e.get("REPORT_IOMMU_GROUPS", "")),
    "steps": parse_steps(e["REPORT_STEPS"]),
    "logs": {
        "dmesg_apollo": dmesg_grep("ua_apollo"),
        "dmesg_full": e.get("REPORT_DMESG_FULL", ""),
        "dmesg_thunderbolt": dmesg_grep("thunderbolt", 5),
        "dmesg_iommu": dmesg_grep("iommu", 5),
    },
}
report = scrub(report, identity_words(e.get("REPORT_USER") or ""))
with open(e["REPORT_FILE"], "w") as f:
    json.dump(report, f, indent=2)
'
    rm -f "$pwdump_file"

    ok "Report saved: $REPORT_FILE"
}

# ================================================================
# Step 8: Telemetry opt-in
# ================================================================
telemetry_prompt() {
    header "Telemetry"

    # Opt-in only: the report is sent when the user says yes at a terminal
    # or passed --send-report.  No terminal, or --yes, means no.
    local answer="n"
    if [ "$SEND_REPORT" = "1" ]; then
        answer="y"
    elif [ "$ASSUME_YES" != "1" ] && [ -t 0 ]; then
        echo ""
        read -rp "Help improve Open Apollo — send anonymous install report? [y/N] " answer
    fi

    if [[ "$answer" =~ ^[Yy] ]]; then
        # Optional GitHub username for follow-up
        local gh_user=""
        if [ "$ASSUME_YES" != "1" ] && [ -t 0 ]; then
            echo ""
            read -rp "Optional: GitHub username for follow-up if we spot an issue (Enter to skip): " gh_user
        fi
        if [ -n "$gh_user" ]; then
            python3 -c "
import json, sys
with open('$REPORT_FILE') as f: d = json.load(f)
d['github_username'] = sys.argv[1]
with open('$REPORT_FILE', 'w') as f: json.dump(d, f, indent=2)
" "$gh_user"
        fi

        add_plugin_report

        info "Sending report to $TELEMETRY_URL..."
        local http_code
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
        [ -t 0 ] || info "To send it: re-run with --send-report"
    fi
}

# The plug-ins this Apollo is authorized for, added to a report the user
# agreed to send (never to the local copy otherwise): the device's own
# answer, through the daemon or a read-only board query
# (mixer-engine/plugin_report.py on plugin_authorization.py), only render
# ids, and which of them built here for native hosting.  Needs the
# driver loaded with plugin_host=1; without it the report says why not.
add_plugin_report() {
    if [ "$PLUGIN_REPORT" != "1" ]; then
        info "Authorized plug-ins left out of the report (--no-plugin-report)"
        return 0
    fi
    local home bundles plugins_json
    home=$(getent passwd "$(desktop_user)" 2>/dev/null | cut -d: -f6)
    # The plug-in maps: their own folder since maps v2.1, open-apollo/plugin-bundles before
    bundles="${home:-/nonexistent}/.local/share/open-apollo-plugin-maps"
    [ -f "$bundles/index.json" ] || [ ! -f "${home:-/nonexistent}/.local/share/open-apollo/plugin-bundles/index.json" ] \
        || bundles="$home/.local/share/open-apollo/plugin-bundles"
    if [ "${STEP_STATUS[init]:-}" = "ok" ]; then
        plugins_json=$(timeout 30 python3 "$PROJECT_DIR/mixer-engine/plugin_report.py" \
            --bundles "$bundles" 2>/dev/null) || plugins_json=""
    fi
    [ -n "${plugins_json:-}" ] || plugins_json='{"available": false, "reason": "no_device"}'
    PLUGINS_JSON="$plugins_json" REPORT_FILE="$REPORT_FILE" ENGINE_DIR="$PROJECT_DIR/mixer-engine" python3 -c '
import json, os, sys
sys.path.insert(0, os.environ["ENGINE_DIR"])
from plugin_report import sanitize
try:
    doc = json.loads(os.environ["PLUGINS_JSON"])
except ValueError:
    doc = None
section = sanitize(doc)
with open(os.environ["REPORT_FILE"]) as f:
    report = json.load(f)
report["plugins"] = section
with open(os.environ["REPORT_FILE"], "w") as f:
    json.dump(report, f, indent=2)
if section["available"]:
    print("Authorized plug-ins in the report: %d (render ids only)" % len(section["authorized"]))
else:
    print("Authorized plug-ins not in the report: %s" % section["reason"])
' | while IFS= read -r line; do info "$line"; done
}

# ================================================================
# Main
# ================================================================
main() {
    parse_args "$@"
    # --yes: every read sees EOF and takes its default; [ -t 0 ] is false.
    [ "$ASSUME_YES" = "1" ] && exec </dev/null
    # pkexec (the setup GUI) sets PKEXEC_UID instead of SUDO_USER; the
    # desktop user owns the audio session and the mixer daemon's user unit.
    if [ -z "${SUDO_USER:-}" ] && [ -n "${PKEXEC_UID:-}" ]; then
        SUDO_USER=$(id -nu "$PKEXEC_UID" 2>/dev/null || echo "")
        export SUDO_USER
    fi
    oa_log_start install "$(desktop_user)" "$@"
    trap 'oa_log_finish "Install log (attach it to a bug report)"' EXIT

    echo ""
    echo -e "${BOLD}Open Apollo — Installer${NC}"
    echo "======================="
    echo ""

    # ── Phase 1: Software install (no hardware needed) ──

    detect_distro
    check_install_deps || true
    build_driver       || true
    setup_dkms         || true
    secureboot_guidance
    check_iommu        || true
    install_firmware   || true
    deploy_configs     || true
    plugin_hosting_options
    check_modprobe_options

    # Check if build succeeded before proceeding
    if [ "${STEP_STATUS[build]:-}" = "fail" ]; then
        fail "Build failed — cannot continue"
        generate_report
        telemetry_prompt
        echo ""
        exit 1
    fi

    # ── Phase 2: Hardware verification ──
    # Always do a guided power cycle to ensure clean TB enumeration
    # after all configs and DKMS are in place.  This prevents race
    # conditions where PipeWire opens the device before ACEFACE completes.

    header "Hardware Setup"
    echo ""

    if [ "${TEST_BUILD:-0}" = "1" ]; then
        # Build test (the Docker matrix): no Thunderbolt bus to wait for
        info "TEST_BUILD: not waiting for an Apollo"
        STEP_STATUS[init]="skipped"
        STEP_DETAIL[init]="build test, no hardware"
    elif [ -t 0 ]; then
        # Interactive mode — guide the user through power cycle
        if lspci -d 1a00: 2>/dev/null | grep -q .; then
            info "Apollo detected, but we need a clean start for reliable setup."
            echo ""
            echo -e "  ${BOLD}1.${NC} Power OFF your Apollo (unplug power or flip the switch)"
            echo -e "  ${BOLD}2.${NC} Wait 5 seconds"
            echo -e "  ${BOLD}3.${NC} Power it back ON and wait for the front panel to light up"
            echo ""
            read -rp "Press Enter once the Apollo is on and ready (~20s after power on)... "
        else
            info "Power on your Apollo and connect it via Thunderbolt."
            info "Wait ~20 seconds after power on for Thunderbolt to initialize."
            echo ""
            read -rp "Press Enter once the Apollo front panel is lit up... "
        fi

        # Poll for Apollo to appear on PCIe (TB enumeration takes 15-30s)
        info "Waiting for Apollo on Thunderbolt bus..."
        apollo_wait=0
        while ! lspci -d 1a00: 2>/dev/null | grep -q . && [ $apollo_wait -lt 60 ]; do
            sleep 2
            apollo_wait=$((apollo_wait + 2))
            [ $((apollo_wait % 10)) -eq 0 ] && info "  still waiting... (${apollo_wait}s)"
        done

        if lspci -d 1a00: 2>/dev/null | grep -q .; then
            APOLLO_PCIE=$(lspci -d 1a00: 2>/dev/null | head -1 || echo "")
            ok "Apollo detected: $APOLLO_PCIE"
            # Give TB link a few extra seconds to stabilize
            info "Waiting for Thunderbolt link to stabilize..."
            sleep 5
            ok "Ready"
        else
            warn "Apollo not detected after 60s"
            info "You can run this later: apollo-setup-io"
            STEP_STATUS[init]="skipped"
            STEP_DETAIL[init]="Apollo not connected"
        fi
    else
        # Non-interactive — poll silently
        info "Waiting up to 60s for Apollo on Thunderbolt bus..."
        apollo_wait=0
        while ! lspci -d 1a00: 2>/dev/null | grep -q . && [ $apollo_wait -lt 60 ]; do
            sleep 2
            apollo_wait=$((apollo_wait + 2))
        done

        if lspci -d 1a00: 2>/dev/null | grep -q .; then
            APOLLO_PCIE=$(lspci -d 1a00: 2>/dev/null | head -1 || echo "")
            ok "Apollo detected: $APOLLO_PCIE"
            sleep 5  # TB stabilization
        else
            warn "Apollo not detected"
            STEP_STATUS[init]="skipped"
            STEP_DETAIL[init]="Apollo not connected"
        fi
    fi

    run_init           || true

    # ── Mixer daemon (systemd user unit) ──
    if [ "${STEP_STATUS[init]:-}" = "ok" ] || [ "${STEP_STATUS[init]:-}" = "skipped" ]; then
        setup_mixer_daemon || true
        setup_plugin_hosting || true
    fi

    # ── Audio test ──
    AUDIO_VERIFIED=""
    if [ "${STEP_STATUS[init]:-}" = "ok" ] && [ -t 0 ]; then
        header "Audio Test"

        pw_user="${SUDO_USER:-$(logname 2>/dev/null || echo "")}"
        pw_uid=$(id -u "$pw_user" 2>/dev/null || echo "")
        if [ -n "$pw_user" ] && [ -n "$pw_uid" ]; then
            echo ""
            read -rp "Play a test tone through Apollo Monitor? [Y/n] " tone_answer
            if [[ ! "$tone_answer" =~ ^[Nn] ]]; then
                info "Playing test tone — you should hear it from your monitors..."
                if sudo -u "$pw_user" XDG_RUNTIME_DIR="/run/user/$pw_uid" \
                    pw-play --target apollo_monitor /usr/share/sounds/freedesktop/stereo/complete.oga 2>/dev/null; then
                    sleep 1
                    read -rp "Did you hear audio? [Y/n] " heard_answer
                    if [[ "$heard_answer" =~ ^[Nn] ]]; then
                        warn "No audio heard — check:"
                        info "  1. Apollo Monitor knob is turned up"
                        info "  2. Speakers/headphones connected to Monitor outputs"
                        info "  3. Run: wpctl status (verify Apollo Monitor L/R is default)"
                        AUDIO_VERIFIED="no"
                    else
                        ok "Audio verified!"
                        AUDIO_VERIFIED="yes"
                    fi
                else
                    warn "Could not play test tone"
                    info "Try manually: pw-play /usr/share/sounds/freedesktop/stereo/complete.oga"
                    AUDIO_VERIFIED="fail"
                fi
            fi
        fi
    fi

    # --- Summary ---
    header "Summary"

    ALL_OK=true
    local -a summary_keys=(deps build dkms configs init daemon)
    [ -n "$PLUGIN_HOSTING_FROM" ] && summary_keys+=(plugins)
    for key in "${summary_keys[@]}"; do
        status="${STEP_STATUS[$key]:-skipped}"
        case "$status" in
            ok)      echo -e "  ${GREEN}[OK]${NC}    $key" ;;
            fail)    echo -e "  ${RED}[FAIL]${NC}  $key${STEP_DETAIL[$key]:+ — ${STEP_DETAIL[$key]}}"; ALL_OK=false ;;
            skipped) echo -e "  ${YELLOW}[SKIP]${NC}  $key" ;;
        esac
    done
    [ -n "$AUDIO_VERIFIED" ] && echo -e "  ${GREEN}[OK]${NC}    audio test: $AUDIO_VERIFIED"

    echo ""
    if [ "$NEEDS_REBOOT" = "1" ]; then
        echo -e "${YELLOW}${BOLD}Installation complete — REBOOT REQUIRED (iommu=pt added).${NC}"
    elif [ "$ALL_OK" = true ]; then
        echo -e "${GREEN}${BOLD}Installation complete.${NC}"
    else
        echo -e "${YELLOW}${BOLD}Installation finished with issues — see above.${NC}"
    fi

    echo ""
    info "Optional: the native mixer console (uaconsole). Install it as your normal user, not with sudo:"
    echo "        bash $PROJECT_DIR/scripts/install-uaconsole.sh"

    # Report + telemetry at the very end (captures all results including audio test)
    generate_report
    telemetry_prompt
    echo ""
    return 0
}

# Run only when executed directly; sourcing (tests) just loads the
# functions.
if [ "${BASH_SOURCE[0]}" = "$0" ]; then
    main "$@"
fi
