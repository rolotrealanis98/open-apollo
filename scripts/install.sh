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
TELEMETRY_URL="https://api.openapollo.org/reports"
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
DMESG_APOLLO="" DMESG_TB="" DMESG_IOMMU=""
DEPS_INSTALLED=""
BUILD_WARNINGS=0

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

    local missing=()
    local missing_pkgs=()

    # Kernel headers
    if [ ! -d "/lib/modules/$(uname -r)/build" ]; then
        missing+=("kernel headers")
        case "$PKG_MGR" in
            dnf)    missing_pkgs+=("kernel-devel") ;;
            apt)    missing_pkgs+=("linux-headers-$(uname -r)") ;;
            pacman) missing_pkgs+=("linux-headers") ;;
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

    # Optional: pipewire / wireplumber
    if ! command -v pw-cli &>/dev/null; then
        warn "pipewire not found (optional)"
    else
        ok "pipewire${PW_VER:+ ($PW_VER)}"
    fi
    if ! command -v wpctl &>/dev/null; then
        warn "wireplumber not found (optional)"
    else
        ok "wireplumber${WP_VER:+ ($WP_VER)}"
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

    make -C "$PROJECT_DIR/driver" clean &>/dev/null || true

    local build_output
    build_output=$(make -C "$PROJECT_DIR/driver" 2>&1)
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
# Step 6: Run apollo-init.sh
# ================================================================
run_init() {
    header "Hardware Init"

    if [ "$SKIP_INIT" = "1" ]; then
        info "Skipping hardware init (--skip-init)"
        STEP_STATUS[init]="skipped"
        return 0
    fi

    if [ -z "$APOLLO_PCIE" ]; then
        info "No Apollo hardware detected — skipping init"
        info "Connect Apollo via Thunderbolt and run: sudo bash tools/apollo-init.sh"
        STEP_STATUS[init]="skipped"
        STEP_DETAIL[init]="no hardware detected"
        return 0
    fi

    local init_script="$PROJECT_DIR/tools/apollo-init.sh"
    if [ ! -f "$init_script" ]; then
        warn "tools/apollo-init.sh not found — skipping"
        STEP_STATUS[init]="fail"
        STEP_DETAIL[init]="script not found"
        return 1
    fi

    info "Running apollo-init.sh..."
    if run_sudo bash "$init_script" 2>&1; then
        ok "Hardware initialized"
        STEP_STATUS[init]="ok"
    else
        fail "Hardware init failed (non-fatal — can retry later)"
        STEP_STATUS[init]="fail"
        STEP_DETAIL[init]="apollo-init.sh returned error"
    fi
}

# ================================================================
# Step 7: Generate install report
# ================================================================
generate_report() {
    header "Install Report"

    # Collect dmesg excerpts
    DMESG_APOLLO=$(dmesg 2>/dev/null | grep -i 'ua_apollo\|apollo' | tail -10 | tr '\n' '\\n' || echo "")
    DMESG_TB=$(dmesg 2>/dev/null | grep -i thunderbolt | tail -5 | tr '\n' '\\n' || echo "")

    # Build steps JSON
    local steps_json="{"
    local first=1
    for key in distro deps build dkms configs init; do
        local status="${STEP_STATUS[$key]:-skipped}"
        local detail="${STEP_DETAIL[$key]:-}"
        [ $first -eq 0 ] && steps_json+=","
        first=0
        if [ -n "$detail" ]; then
            steps_json+="\"$key\":{\"status\":\"$status\",\"detail\":\"$(echo "$detail" | sed 's/"/\\"/g')\"}"
        else
            steps_json+="\"$key\":{\"status\":\"$status\"}"
        fi
    done
    steps_json+="}"

    # Build cards array
    local cards_json="[]"
    if [ -n "$EXISTING_CARDS" ]; then
        cards_json="[$(echo "$EXISTING_CARDS" | tr '|' '\n' | grep -v '^$' | sed 's/^/"/;s/$/"/' | tr '\n' ',' | sed 's/,$//' )]"
    fi

    # Build snd_modules array
    local mods_json="[]"
    if [ -n "$SND_MODULES" ]; then
        mods_json="[$(echo "$SND_MODULES" | tr '|' '\n' | grep -v '^$' | sed 's/^/"/;s/$/"/' | tr '\n' ',' | sed 's/,$//' )]"
    fi

    # Overall success
    local overall="true"
    for key in deps build; do
        if [ "${STEP_STATUS[$key]:-}" = "fail" ]; then
            overall="false"
            break
        fi
    done

    cat > "$REPORT_FILE" <<REPORTEOF
{
  "version": 1,
  "timestamp": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "success": $overall,
  "system": {
    "distro": "$DISTRO",
    "kernel": "$KERNEL",
    "arch": "$ARCH",
    "ram_mb": ${RAM_MB:-0},
    "cpu": "$CPU_MODEL",
    "secure_boot": $( [ "$SECURE_BOOT" = "true" ] && echo true || echo false ),
    "iommu": "$IOMMU"
  },
  "audio": {
    "pipewire_version": "${PW_VER:-null}",
    "wireplumber_version": "${WP_VER:-null}",
    "pulseaudio_running": $PA_RUNNING,
    "existing_cards": $cards_json,
    "snd_modules": $mods_json
  },
  "apollo": {
    "detected": $( [ -n "$APOLLO_PCIE" ] && echo true || echo false ),
    "pcie_device": "$(echo "$APOLLO_PCIE" | sed 's/"/\\"/g')",
    "thunderbolt_controller": "$(echo "$TB_CONTROLLER" | sed 's/"/\\"/g')"
  },
  "steps": $steps_json,
  "logs": {
    "dmesg_apollo": "$DMESG_APOLLO",
    "dmesg_thunderbolt": "$DMESG_TB",
    "dmesg_iommu": "$DMESG_IOMMU"
  }
}
REPORTEOF

    ok "Report saved: $REPORT_FILE"
}

# ================================================================
# Step 8: Telemetry opt-in
# ================================================================
telemetry_prompt() {
    header "Telemetry"

    echo ""
    read -rp "Help improve Open Apollo — send anonymous install report? [y/N] " answer

    if [[ "$answer" =~ ^[Yy] ]]; then
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

detect_distro
check_install_deps || true
build_driver       || true
setup_dkms         || true
deploy_configs     || true
run_init           || true
generate_report
telemetry_prompt

# --- Summary ---
header "Summary"

ALL_OK=true
for key in deps build dkms configs init; do
    status="${STEP_STATUS[$key]:-skipped}"
    case "$status" in
        ok)      echo -e "  ${GREEN}[OK]${NC}    $key" ;;
        fail)    echo -e "  ${RED}[FAIL]${NC}  $key${STEP_DETAIL[$key]:+ — ${STEP_DETAIL[$key]}}"; ALL_OK=false ;;
        skipped) echo -e "  ${YELLOW}[SKIP]${NC}  $key" ;;
    esac
done

echo ""
if [ "$ALL_OK" = true ]; then
    echo -e "${GREEN}${BOLD}Installation complete.${NC}"
else
    echo -e "${YELLOW}${BOLD}Installation finished with issues — see above.${NC}"
fi

echo ""
info "Report: $REPORT_FILE"
if [ -n "$APOLLO_PCIE" ] && [ "$SKIP_INIT" = "0" ]; then
    info "Init:   sudo bash $PROJECT_DIR/tools/apollo-init.sh --status"
fi
echo ""
