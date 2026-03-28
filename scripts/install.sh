#!/bin/bash
# install.sh — One-command Open Apollo installer
#
# Detects distro, installs deps, builds driver, sets up DKMS,
# deploys configs, runs hardware init, generates install report.
#
# Usage:
#   sudo bash scripts/install.sh
#   sudo bash scripts/install.sh --skip-init --no-dkms
#   bash scripts/install.sh --help

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPORT_FILE="/tmp/open-apollo-install-report.json"
TELEMETRY_URL="https://open-apollo-api.rolotrealanis.workers.dev/reports"
VERSION="0.1.0"

# --- Flags ---
SKIP_INIT=0
NO_DKMS=0
for arg in "$@"; do
    case "$arg" in
        --skip-init) SKIP_INIT=1 ;;
        --no-dkms)   NO_DKMS=1 ;;
        -h|--help)
            cat <<EOF
Usage: sudo bash scripts/install.sh [OPTIONS]

Options:
  --skip-init   Skip hardware init (apollo-init.sh)
  --no-dkms     Skip DKMS setup
  -h, --help    Show this help

Steps performed:
  1. Detect distro and package manager
  2. Check and install missing dependencies
  3. Build kernel module
  4. (Optional) Set up DKMS for auto-rebuild on kernel updates
  5. Deploy PipeWire/WirePlumber/UCM2 configs
  6. Run apollo-init.sh if Apollo hardware detected
  7. Generate install report
  8. Opt-in anonymous telemetry
EOF
            exit 0
            ;;
        *) echo "Unknown option: $arg (try --help)"; exit 1 ;;
    esac
done

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

# --- Prompt helper ---
# Reads from /dev/tty so prompts work even when sudo password is piped via stdin
prompt() {
    local varname="$1"; shift
    if [ -e /dev/tty ]; then
        read -rp "$*" "$varname" < /dev/tty
    else
        eval "$varname=''"
    fi
}

# Returns true if we can show interactive prompts
can_prompt() { [ -e /dev/tty ]; }

# --- Sudo helper ---
run_sudo() {
    if [ "$(id -u)" -eq 0 ]; then
        "$@"
    else
        sudo "$@"
    fi
}

# --- Report state ---
declare -A STEP_STATUS=()
declare -A STEP_DETAIL=()
DISTRO="" DISTRO_ID="" PKG_MGR="" KERNEL="" ARCH="" RAM_MB="" CPU_MODEL=""
SECURE_BOOT="" IOMMU="" PW_VER="" WP_VER="" PA_RUNNING=""
EXISTING_CARDS="" SND_MODULES="" TB_CONTROLLER=""
APOLLO_PCIE="" APOLLO_DEV_TYPE="" APOLLO_DEV_NAME=""
NEEDS_REBOOT=0
DMESG_APOLLO="" DMESG_TB="" DMESG_IOMMU=""
DEPS_INSTALLED=""
BUILD_WARNINGS=0
DMESG_FULL="" IOMMU_GROUPS="" AUDIO_APLAY="" AUDIO_ARECORD="" AUDIO_PWDUMP=""

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
    else
        DISTRO="unknown"
        DISTRO_ID="unknown"
    fi

    case "$DISTRO_ID" in
        fedora|rhel|centos) PKG_MGR="dnf" ;;
        ubuntu|debian|pop|linuxmint) PKG_MGR="apt" ;;
        arch|manjaro|endeavouros) PKG_MGR="pacman" ;;
        opensuse*|sles) PKG_MGR="zypper" ;;
        nixos) PKG_MGR="nix" ;;
        *)
            PKG_MGR=""
            warn "Unsupported distro: $DISTRO_ID — dependency install will be manual"
            ;;
    esac

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
        info "Required packages: gcc, gnumake, linux-headers, python3, python3-websockets,"
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
            pacman) missing_pkgs+=("linux-headers") ;;
            zypper) missing_pkgs+=("kernel-devel") ;;
        esac
    else
        ok "Kernel headers"
    fi

    # gcc
    if ! command -v gcc &>/dev/null; then
        missing+=("gcc")
        missing_pkgs+=("gcc")
    else
        ok "gcc"
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
    # AppIndicator3 for tray indicator
    if ! python3 -c "import gi; gi.require_version('AppIndicator3','0.1')" 2>/dev/null; then
        missing+=("appindicator")
        case "$PKG_MGR" in
            dnf)    missing_pkgs+=("libappindicator-gtk3") ;;
            apt)    missing_pkgs+=("gir1.2-appindicator3-0.1") ;;
            pacman) missing_pkgs+=("libappindicator-gtk3") ;;
            zypper) missing_pkgs+=("typelib-1_0-AppIndicator3-0_1") ;;
        esac
    else
        ok "appindicator"
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

    local build_output kdir_arg=""

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
    fi

    make -C "$PROJECT_DIR/driver" $kdir_arg clean &>/dev/null || true

    build_output=$(make -C "$PROJECT_DIR/driver" $kdir_arg 2>&1)
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

    local dkms_src="/usr/src/ua_apollo-$VERSION"

    # Remove existing DKMS registration if present
    if dkms status ua_apollo/$VERSION 2>/dev/null | grep -q .; then
        info "Removing existing DKMS registration..."
        run_sudo dkms remove ua_apollo/$VERSION --all 2>/dev/null || true
    fi

    # Remove any manually installed .ko to avoid "Differences" warning
    run_sudo rm -f "/lib/modules/$(uname -r)/extra/ua_apollo.ko" 2>/dev/null || true

    # Copy source
    info "Copying driver source to $dkms_src"
    run_sudo rm -rf "$dkms_src"
    run_sudo mkdir -p "$dkms_src"
    run_sudo cp "$PROJECT_DIR"/driver/*.c "$PROJECT_DIR"/driver/*.h "$dkms_src/" 2>/dev/null || true
    run_sudo cp "$PROJECT_DIR"/driver/Makefile "$dkms_src/"

    # Create dkms.conf
    run_sudo tee "$dkms_src/dkms.conf" >/dev/null <<DKMSEOF
PACKAGE_NAME="ua_apollo"
PACKAGE_VERSION="$VERSION"
BUILT_MODULE_NAME[0]="ua_apollo"
DEST_MODULE_LOCATION[0]="/updates"
AUTOINSTALL="yes"
MAKE[0]="make -C /lib/modules/\${kernelver}/build M=\${dkms_tree}/ua_apollo/\${PACKAGE_VERSION}/build modules"
CLEAN="make -C /lib/modules/\${kernelver}/build M=\${dkms_tree}/ua_apollo/\${PACKAGE_VERSION}/build clean"
DKMSEOF

    # DKMS add/build/install
    if run_sudo dkms add ua_apollo/$VERSION 2>&1; then
        ok "DKMS: added"
    else
        fail "DKMS add failed"
        STEP_STATUS[dkms]="fail"
        return 1
    fi

    if run_sudo dkms build ua_apollo/$VERSION 2>&1; then
        ok "DKMS: built"
    else
        fail "DKMS build failed"
        STEP_STATUS[dkms]="fail"
        return 1
    fi

    if run_sudo dkms install ua_apollo/$VERSION 2>&1; then
        ok "DKMS: installed"
    else
        fail "DKMS install failed"
        STEP_STATUS[dkms]="fail"
        return 1
    fi

    # Auto-load on boot
    run_sudo tee /etc/modules-load.d/ua_apollo.conf >/dev/null <<< "ua_apollo"
    ok "Auto-load configured (/etc/modules-load.d/ua_apollo.conf)"

    STEP_STATUS[dkms]="ok"
}

# ================================================================
# Step 4b: Check IOMMU
# ================================================================
check_iommu() {
    # Intel VT-d (DMAR) blocks BAR0 access unless iommu=pt is set.
    # Without it, all register reads return 0xFFFFFFFF and the Apollo is unusable.
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
    else
        fail "Config deployment failed"
        STEP_STATUS[configs]="fail"
        return 1
    fi
}

# ================================================================
# Step 6: Verify hardware + PipeWire
# ================================================================
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
        if run_sudo modprobe ua_apollo 2>/dev/null || run_sudo insmod "$PROJECT_DIR/driver/ua_apollo.ko" 2>/dev/null; then
            ok "Driver loaded"
        else
            fail "Could not load driver — check dmesg for details"
            STEP_STATUS[init]="fail"
            STEP_DETAIL[init]="modprobe/insmod failed"
            return 1
        fi
    else
        ok "Driver already loaded"
    fi

    # Gate 2: Wait for device node (driver probe complete)
    info "Waiting for device node..."
    local tries=0
    while [ ! -e /dev/ua_apollo0 ] && [ $tries -lt 50 ]; do
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

    # Gate 3: Wait for ACEFACE connect (DSP handshake)
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

    # Gate 4: Verify ALSA card registered (retry up to 10s)
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

    # Gate 5: Verify hardware is responsive (read a register)
    # Check that BAR0 reads don't return 0xFFFFFFFF
    if dmesg 2>/dev/null | grep -q "device unreachable\|PCIe error detected"; then
        fail "Apollo PCIe link is down — power cycle Apollo and try again"
        STEP_STATUS[init]="fail"
        STEP_DETAIL[init]="PCIe link dead"
        return 1
    fi
    ok "Hardware responding"

    # Gate 6: Set up PipeWire virtual I/O devices
    local pw_user="${SUDO_USER:-$(logname 2>/dev/null || echo "")}"
    local pw_uid
    pw_uid=$(id -u "$pw_user" 2>/dev/null || echo "")

    if [ -n "$pw_user" ] && [ -n "$pw_uid" ] && command -v wpctl > /dev/null 2>&1; then
        local pw_home
        pw_home=$(eval echo "~$pw_user")

        # Restart PipeWire so newly-deployed WirePlumber rules take effect
        info "Restarting PipeWire to load new configs..."
        sudo -u "$pw_user" XDG_RUNTIME_DIR="/run/user/$pw_uid" \
            systemctl --user restart pipewire wireplumber 2>/dev/null || true
        sleep 3

        info "Setting up PipeWire audio devices..."
        sudo -u "$pw_user" HOME="$pw_home" XDG_RUNTIME_DIR="/run/user/$pw_uid" \
            /usr/local/bin/apollo-setup-io 2>&1 || true

        # Gate 7: Verify virtual devices appeared
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
    for key in distro deps build dkms configs init; do
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
    AUDIO_PWDUMP=""
    if command -v pw-dump &>/dev/null; then
        AUDIO_PWDUMP=$(pw-dump --no-colors 2>/dev/null | head -500 || echo "")
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
    REPORT_DMESG_FULL="$DMESG_FULL" \
    REPORT_IOMMU_GROUPS="$IOMMU_GROUPS" \
    REPORT_AUDIO_APLAY="$AUDIO_APLAY" \
    REPORT_AUDIO_ARECORD="$AUDIO_ARECORD" \
    REPORT_AUDIO_PWDUMP="$AUDIO_PWDUMP" \
    python3 -c '
import json, subprocess, os, datetime

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

e = os.environ
report = {
    "version": 2,
    "script_version": e["REPORT_SCRIPT_VERSION"],
    "timestamp": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
    "success": e["REPORT_SUCCESS"] == "true",
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
        "pw_dump": e.get("REPORT_AUDIO_PWDUMP", ""),
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
with open(e["REPORT_FILE"], "w") as f:
    json.dump(report, f, indent=2)
'

    ok "Report saved: $REPORT_FILE"
}

# ================================================================
# Step 8: Telemetry opt-in
# ================================================================
telemetry_prompt() {
    header "Telemetry"

    local answer="n"
    if [ -t 0 ]; then
        echo ""
        read -rp "Help improve Open Apollo — send anonymous install report? [y/N] " answer
    else
        # Non-interactive (piped sudo, SSH, etc.) — auto-send
        answer="y"
    fi

    if [[ "$answer" =~ ^[Yy] ]]; then
        # Optional GitHub username for follow-up
        local gh_user=""
        if [ -t 0 ]; then
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
    fi
}

# ================================================================
# Main
# ================================================================
echo ""
echo -e "${BOLD}Open Apollo — Installer${NC}"
echo "======================="
echo ""

# ── Phase 1: Software install (no hardware needed) ──

detect_distro
check_install_deps || true
build_driver       || true
setup_dkms         || true
check_iommu        || true
deploy_configs     || true

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

if [ -t 0 ]; then
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

# ── Start mixer daemon ──
if [ "${STEP_STATUS[init]:-}" = "ok" ]; then
    header "Mixer Daemon"

    pw_user="${SUDO_USER:-$(logname 2>/dev/null || echo "")}"
    daemon_script="$PROJECT_DIR/mixer-engine/ua_mixer_daemon.py"
    daemon_log="/tmp/ua-mixer-daemon.log"

    if [ -n "$pw_user" ] && [ -f "$daemon_script" ]; then
        # Kill any existing daemon
        pkill -f "ua_mixer_daemon" 2>/dev/null || true
        sleep 1

        info "Starting mixer daemon..."
        sudo -u "$pw_user" python3 "$daemon_script" --no-bonjour \
            > "$daemon_log" 2>&1 &
        disown

        # Wait for daemon to start
        sleep 3
        if pgrep -f "ua_mixer_daemon" > /dev/null 2>&1; then
            ok "Mixer daemon running (TCP:4710, WS:4720)"
            STEP_STATUS[daemon]="ok"
        else
            warn "Mixer daemon failed to start — check $daemon_log"
            STEP_STATUS[daemon]="fail"
        fi
    fi
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

# Launch tray indicator
pw_user="${SUDO_USER:-$(logname 2>/dev/null || echo "")}"
tray_script="$PROJECT_DIR/tools/open-apollo-tray.py"
if [ -n "$pw_user" ] && [ -f "$tray_script" ] && python3 -c "import gi; gi.require_version('AppIndicator3','0.1')" 2>/dev/null; then
    if ! pgrep -f "open-apollo-tray" > /dev/null 2>&1; then
        info "Starting Open Apollo tray indicator..."
        sudo -u "$pw_user" python3 "$tray_script" &>/dev/null &
        disown
        ok "Tray indicator running"
    fi
fi

# --- Summary ---
header "Summary"

ALL_OK=true
for key in deps build dkms configs init daemon; do
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

# Report + telemetry at the very end (captures all results including audio test)
generate_report
telemetry_prompt
echo ""
