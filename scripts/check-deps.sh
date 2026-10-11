#!/bin/bash
# check-deps.sh — Check for required build dependencies
#
# Verifies that all tools needed to build and run Open Apollo are present.
# Reports missing dependencies with distribution-specific install commands.

set -euo pipefail

GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

MISSING=0

check() {
    local name="$1"
    local check_cmd="$2"
    local fedora_pkg="$3"
    local debian_pkg="$4"
    local arch_pkg="$5"

    if eval "$check_cmd" > /dev/null 2>&1; then
        printf '  %b[OK]%b  %s\n' "$GREEN" "$NC"  "$name"
    else
        printf '  %b[  ]%b  %s — not found\n' "$RED" "$NC"  "$name"
        printf "        Fedora:       sudo dnf install %s\n" "$fedora_pkg"
        printf "        Ubuntu/Debian: sudo apt install %s\n" "$debian_pkg"
        printf "        Arch:         sudo pacman -S %s\n" "$arch_pkg"
        echo ""
        MISSING=$((MISSING + 1))
    fi
}

echo ""
echo "Open Apollo — Dependency Check"
echo "==============================="
echo ""

# Build tools
echo "Build tools:"
check "gcc" "command -v gcc" "gcc" "gcc" "gcc"
check "make" "command -v make" "make" "make" "make"
check "kernel headers" "test -d /lib/modules/\$(uname -r)/build" \
    "kernel-devel" "linux-headers-\$(uname -r)" "linux-headers"

echo ""

# Runtime
echo "Runtime:"
check "python3 (>= 3.10)" \
    "python3 -c 'import sys; assert sys.version_info >= (3,10)'" \
    "python3" "python3" "python3"

echo ""

# Optional (informational only)
echo "Optional (for PipeWire integration):"
if command -v wpctl > /dev/null 2>&1; then
    printf '  %b[OK]%b  wireplumber\n' "$GREEN" "$NC"
else
    printf '  %b[--]%b  wireplumber — not found (optional, for PipeWire integration)\n' "$YELLOW" "$NC"
fi

if command -v pw-cli > /dev/null 2>&1; then
    printf '  %b[OK]%b  pipewire\n' "$GREEN" "$NC"
else
    printf '  %b[--]%b  pipewire — not found (optional, for audio routing)\n' "$YELLOW" "$NC"
fi

echo ""

# Host state that stops the driver from loading or the Apollo from appearing
# (informational: prints the exact fix, never changes anything)
echo "Host (Thunderbolt authorization, Secure Boot):"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if [ -f "$HERE/apollo-hostcheck.sh" ]; then
    # shellcheck source=scripts/apollo-hostcheck.sh
    . "$HERE/apollo-hostcheck.sh"
    hc_ok=1
    hostcheck_thunderbolt || hc_ok=0
    hostcheck_secureboot || hc_ok=0
    if [ "$hc_ok" = "1" ]; then printf '  %b[OK]%b  no Thunderbolt authorization or Secure Boot problem found\n' "$GREEN" "$NC"; fi
fi

echo ""

if [ "$MISSING" -gt 0 ]; then
    printf '%b%d required dependency(ies) missing.%b\n' "$RED" "$MISSING" "$NC"
    echo "Install them and re-run this script."
    exit 1
else
    printf '%bAll required dependencies found.%b\n' "$GREEN" "$NC"
    exit 0
fi
