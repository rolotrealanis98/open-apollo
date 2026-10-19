#!/usr/bin/env bash
# setup.sh — build and start the open-apollo setup wizard
#
# Builds open-apollo-setup and oa-assets (console-rs, needs Rust 1.89 or
# newer), then starts the wizard. Run it as your normal user from a desktop
# session: the wizard asks for your password once, when it installs.
#
# Usage: bash scripts/setup.sh [wizard options]
#   --demo       walk through the wizard without changing anything
#   -h, --help   show this help
# Any other option is passed to open-apollo-setup.

set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CRATES="$REPO/console-rs"
MIN_RUST=1.89

if [[ -t 1 ]]; then
    RED='\033[0;31m' GREEN='\033[0;32m' CYAN='\033[0;36m' NC='\033[0m'
else
    RED='' GREEN='' CYAN='' NC=''
fi
info() { echo -e "${CYAN}[INFO]${NC}  $*"; }
ok()   { echo -e "${GREEN}[ OK ]${NC}  $*"; }
fail() { echo -e "${RED}[FAIL]${NC}  $*" >&2; }

case "${1:-}" in
    -h|--help) sed -n '2,/^set -euo/{/^set -euo/d;s/^# \{0,1\}//;p;}' "${BASH_SOURCE[0]}"; exit 0 ;;
esac

if [[ "$(id -u)" -eq 0 ]]; then
    fail "Run this as your normal user, not with sudo: the wizard asks for your password when it installs."
    exit 1
fi
if [[ -z "${WAYLAND_DISPLAY:-}" && -z "${DISPLAY:-}" ]]; then
    fail "No desktop session found (neither WAYLAND_DISPLAY nor DISPLAY is set)."
    echo "        Start it from a terminal in your desktop, or install from the command line:"
    echo "        see docs/install.md, \"Command-line install\"."
    exit 1
fi

if ! command -v cargo >/dev/null 2>&1; then
    fail "Rust is not installed. Install it with rustup (https://rustup.rs), then run this again:"
    echo "        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
    exit 1
fi
have=$(rustc --version 2>/dev/null | awk '{print $2}')
if [[ -n "$have" && "$(printf '%s\n%s\n' "$MIN_RUST" "$have" | sort -V | head -1)" != "$MIN_RUST" ]]; then
    fail "Rust $have is too old; the wizard needs $MIN_RUST or newer."
    echo "        rustup update stable   (distribution Rust packages are often too old)"
    exit 1
fi
if ! command -v cc >/dev/null 2>&1; then
    fail "No C compiler (cc). Install your distribution's build tools first:"
    echo "        Debian/Ubuntu: sudo apt install build-essential    Fedora: sudo dnf install gcc make"
    echo "        Arch: sudo pacman -S base-devel                    openSUSE: sudo zypper install gcc make"
    exit 1
fi

# cargo builds into $CARGO_TARGET_DIR when it is set
TARGET="${CARGO_TARGET_DIR:-$CRATES/target}"
[[ "$TARGET" == /* ]] || TARGET="$PWD/$TARGET"

info "Building the setup wizard (the first build takes a few minutes)..."
cargo build --release --manifest-path "$CRATES/Cargo.toml" -p oa-setup -p oa-assets
ok "Built $TARGET/release/open-apollo-setup"

args=("$@")
if [[ " $* " != *" --repo "* ]]; then
    args=(--repo "$REPO" "${args[@]}")
fi
exec "$TARGET/release/open-apollo-setup" "${args[@]}"
