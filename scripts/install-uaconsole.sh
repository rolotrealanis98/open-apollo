#!/usr/bin/env bash
# Build and install the native mixer console (uaconsole, from console-rs/) for
# the current user. No root needed.
#
# Nothing from Universal Audio is downloaded or installed. uaconsole reads the
# Console art at run time from your own UAD Console install; see console-rs/README.md.
#
# Usage: bash scripts/install-uaconsole.sh [options]
#
#   --prefix DIR      install into DIR/bin and DIR/share (default: ~/.local)
#   --no-build        install the binary already built in console-rs/target/release
#   --resources DIR   copy a UAD Console "Resources" folder to
#                     ~/.local/share/open-apollo/console-resources, then continue
#   --check           only look for Console resources, fonts and a GPU driver
#   --smoke-test      after installing, render one frame offscreen (needs a GPU)
#   --uninstall       remove what this script installed (keeps copied resources)
#   -h, --help        show this help
#
# Environment: CARGO (default: cargo), CONSOLE_RESOURCES, WINEPREFIX (default: ~/wine-ua)
set -euo pipefail

REPO="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
CRATES="$REPO/console-rs"
CARGO="${CARGO:-cargo}"
# Newest rust-version among console-rs dependencies when this was written.
# Cargo names the exact crate if a newer dependency needs more.
MIN_RUST=1.89
# houston (console-rs/crates/houston/src/resources.rs) looks here; the path is fixed.
RESOURCES_COPY="$HOME/.local/share/open-apollo/console-resources"
CONSOLE_SUBDIR="Program Files (x86)/Universal Audio/UAD Console"

if [[ -t 1 ]]; then
    RED='\033[0;31m' GREEN='\033[0;32m' YELLOW='\033[1;33m' CYAN='\033[0;36m' BOLD='\033[1m' NC='\033[0m'
else
    RED='' GREEN='' YELLOW='' CYAN='' BOLD='' NC=''
fi
info()   { echo -e "${CYAN}[INFO]${NC}  $*"; }
ok()     { echo -e "${GREEN}[ OK ]${NC}  $*"; }
warn()   { echo -e "${YELLOW}[WARN]${NC}  $*"; }
fail()   { echo -e "${RED}[FAIL]${NC}  $*" >&2; }
header() { echo -e "\n${BOLD}── $* ──${NC}"; }
usage()  { sed -n '2,/^set -euo/{/^set -euo/d;s/^# \{0,1\}//;p}' "${BASH_SOURCE[0]}"; }

PREFIX=""
BUILD=1
CHECK_ONLY=0
SMOKE=0
UNINSTALL=0
COPY_FROM=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --prefix)
            [[ $# -ge 2 && -n "$2" ]] || { fail "--prefix needs a directory"; exit 2; }
            PREFIX="$2"; shift 2 ;;
        --prefix=*) PREFIX="${1#--prefix=}"; shift ;;
        --no-build) BUILD=0; shift ;;
        --resources)
            [[ $# -ge 2 && -n "$2" ]] || { fail "--resources needs a directory"; exit 2; }
            COPY_FROM="$2"; shift 2 ;;
        --resources=*) COPY_FROM="${1#--resources=}"; shift ;;
        --check) CHECK_ONLY=1; shift ;;
        --smoke-test) SMOKE=1; shift ;;
        --uninstall) UNINSTALL=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) fail "unknown option: $1 (see --help)"; exit 2 ;;
    esac
done

if [[ -n "$PREFIX" ]]; then
    [[ "$PREFIX" == /* ]] || PREFIX="$PWD/$PREFIX"
    BINDIR="$PREFIX/bin"
    DATADIR="$PREFIX/share"
else
    BINDIR="$HOME/.local/bin"
    DATADIR="${XDG_DATA_HOME:-$HOME/.local/share}"
fi
APPS_DIR="$DATADIR/applications"
ICON_DIR="$DATADIR/icons/hicolor/scalable/apps"

# ── Console resources ─────────────────────────────────────────────────────────

# Same order as houston::Resources::candidates().
resource_candidates() {
    if [[ -n "${CONSOLE_RESOURCES:-}" ]]; then
        printf '%s\n' "$CONSOLE_RESOURCES"
    fi
    printf '%s\n' "${WINEPREFIX:-$HOME/wine-ua}/drive_c/$CONSOLE_SUBDIR/Resources"
    printf '%s\n' "/Applications/Universal Audio/UAD Console.app/Contents/Resources"
    printf '%s\n' "$RESOURCES_COPY"
}

is_resources() { [[ -f "$1/appearance/Appearance.json" ]]; }

find_resources() {
    local c
    while IFS= read -r c; do
        if is_resources "$c"; then
            printf '%s\n' "$c"
            return 0
        fi
    done < <(resource_candidates)
    return 1
}

# Console installs uaconsole does not search by itself (another Wine prefix,
# a mounted Windows partition).
other_resources() {
    local c
    local -a globs=(
        "$HOME/.wine/drive_c/$CONSOLE_SUBDIR/Resources"
        "$HOME"/.local/share/wineprefixes/*/"drive_c/$CONSOLE_SUBDIR/Resources"
        /mnt/*/"$CONSOLE_SUBDIR/Resources"
        /media/"$USER"/*/"$CONSOLE_SUBDIR/Resources"
        /run/media/"$USER"/*/"$CONSOLE_SUBDIR/Resources"
    )
    for c in "${globs[@]}"; do
        if is_resources "$c"; then
            printf '%s\n' "$c"
        fi
    done
}

copy_resources() {
    local src="$1"
    [[ "$src" == /* ]] || src="$PWD/$src"
    if ! is_resources "$src"; then
        fail "$src is not a UAD Console Resources folder (no appearance/Appearance.json)"
        exit 1
    fi
    if [[ "$(cd -- "$src" && pwd -P)" == "$(cd -- "$RESOURCES_COPY" 2>/dev/null && pwd -P)" ]]; then
        ok "Resources already at $RESOURCES_COPY"
        return
    fi
    header "Copying Console resources"
    info "$src -> $RESOURCES_COPY"
    mkdir -p "$(dirname -- "$RESOURCES_COPY")"
    local tmp="$RESOURCES_COPY.tmp.$$"
    rm -rf -- "$tmp"
    cp -R -- "$src" "$tmp"
    rm -rf -- "$RESOURCES_COPY"
    mv -- "$tmp" "$RESOURCES_COPY"
    ok "Copied ($(du -sh -- "$RESOURCES_COPY" | cut -f1)). This copy stays on this machine; do not share it."
}

report_resources() {
    header "Console resources"
    local found
    if found="$(find_resources)"; then
        ok "Found UAD Console resources: $found"
        if [[ ! -d "$found/Plug-In Icons" && ! -d "/Library/Application Support/Universal Audio/Plug-In Icons" ]]; then
            info "No 'Plug-In Icons' folder found; plug-in icons will be missing (optional)."
        fi
        return 0
    fi

    warn "UAD Console resources not found. Looked in:"
    local c
    while IFS= read -r c; do
        echo "        $c"
    done < <(resource_candidates)

    local -a others=()
    mapfile -t others < <(other_resources)
    if [[ ${#others[@]} -gt 0 ]]; then
        echo ""
        info "Found a Console install uaconsole does not search by default:"
        for c in "${others[@]}"; do
            echo "        $c"
        done
        echo "      Use it with:  export CONSOLE_RESOURCES=\"${others[0]}\""
        echo "      or copy it:   bash $REPO/scripts/install-uaconsole.sh --resources \"${others[0]}\""
    fi

    cat <<EOF

      uaconsole draws the mixer with the art from UA's own UAD Console.
      open-apollo does not include or download it. Get it from your own install:

      1. Install UA's Windows UAD Console into a Wine prefix (open-apollo's Wine
         tooling uses ~/wine-ua). uaconsole then finds
           \$WINEPREFIX/drive_c/$CONSOLE_SUBDIR/Resources
         by itself. For another prefix: export WINEPREFIX=/path/to/prefix

      2. Or copy the Resources folder from a computer where UAD Console is installed:
           Windows: C:\\Program Files (x86)\\Universal Audio\\UAD Console\\Resources
           macOS:   /Applications/Universal Audio/UAD Console.app/Contents/Resources
         then run:
           bash $REPO/scripts/install-uaconsole.sh --resources /path/to/Resources
         which copies it to $RESOURCES_COPY

      Or point uaconsole at any Resources folder: export CONSOLE_RESOURCES=/path/to/Resources
EOF
    return 1
}

font_packages() {
    if command -v apt-get >/dev/null 2>&1; then echo "sudo apt-get install fonts-urw-base35 fonts-liberation"
    elif command -v dnf >/dev/null 2>&1; then echo "sudo dnf install urw-base35-nimbus-sans-fonts liberation-sans-fonts"
    elif command -v pacman >/dev/null 2>&1; then echo "sudo pacman -S gsfonts ttf-liberation"
    elif command -v zypper >/dev/null 2>&1; then echo "sudo zypper install urw-base35-fonts liberation-fonts"
    else echo "install URW base35 (Nimbus Sans) and Liberation fonts with your package manager"
    fi
}

report_fonts() {
    header "Fonts"
    if ! command -v fc-list >/dev/null 2>&1; then
        info "fc-list not available; cannot check for Nimbus Sans and Liberation Sans."
        return 0
    fi
    local family missing=()
    for family in "Nimbus Sans" "Liberation Sans"; do
        if [[ -n "$(fc-list "$family" family 2>/dev/null)" ]]; then
            ok "$family"
        else
            missing+=("$family")
        fi
    done
    if [[ ${#missing[@]} -gt 0 ]]; then
        warn "Missing ${missing[*]} (stand-ins for Console's Helvetica and Arial; labels"
        warn "render in a fallback font with different widths). Install with:"
        echo "        $(font_packages)"
        return 1
    fi
}

report_gpu() {
    header "GPU"
    local d
    for d in /usr/share/vulkan/icd.d /etc/vulkan/icd.d /run/opengl-driver/share/vulkan/icd.d; do
        if compgen -G "$d/*.json" >/dev/null; then
            ok "Vulkan driver found ($d)"
            return 0
        fi
    done
    warn "No Vulkan driver found; uaconsole falls back to OpenGL ES. For Vulkan install"
    warn "mesa-vulkan-drivers (Debian/Ubuntu), mesa-vulkan-drivers (Fedora), vulkan-radeon or"
    warn "vulkan-intel (Arch), or your GPU vendor's driver."
}

# ── Install / uninstall ───────────────────────────────────────────────────────

do_uninstall() {
    header "Removing uaconsole"
    local f
    for f in "$BINDIR/uaconsole" "$BINDIR/open-apollo-console" \
             "$APPS_DIR/uaconsole.desktop" "$ICON_DIR/uaconsole.svg"; do
        if [[ -e "$f" ]]; then
            rm -f -- "$f"
            ok "Removed $f"
        fi
    done
    if command -v update-desktop-database >/dev/null 2>&1 && [[ -d "$APPS_DIR" ]]; then
        update-desktop-database -q "$APPS_DIR" 2>/dev/null || true
    fi
    if [[ -d "$RESOURCES_COPY" ]]; then
        info "Kept your copied Console resources: $RESOURCES_COPY (delete it yourself if unwanted)"
    fi
}

check_toolchain() {
    if ! command -v "$CARGO" >/dev/null 2>&1; then
        fail "cargo not found. Install Rust with rustup (https://rustup.rs):"
        echo "        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
        exit 1
    fi
    local have
    have="$(rustc --version 2>/dev/null | awk '{print $2}')"
    if [[ -n "$have" && "$(printf '%s\n%s\n' "$MIN_RUST" "$have" | sort -V | head -1)" != "$MIN_RUST" ]]; then
        fail "Rust $have is too old; console-rs needs $MIN_RUST or newer."
        echo "        rustup update stable   (distribution Rust packages are often too old)"
        exit 1
    fi
    ok "Rust ${have:-?} ($(command -v "$CARGO"))"
}

target_dir() {
    "$CARGO" metadata --format-version 1 --no-deps --manifest-path "$CRATES/Cargo.toml" 2>/dev/null |
        sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p'
}

build() {
    header "Building uaconsole (release)"
    check_toolchain
    local -a locked=(--locked)
    local err
    err="$(mktemp)"
    if ! "$CARGO" metadata --locked --format-version 1 --manifest-path "$CRATES/Cargo.toml" >/dev/null 2>"$err"; then
        if grep -q "lock file" "$err"; then
            warn "console-rs/Cargo.lock does not match the workspace; building without --locked."
            locked=()
        fi
    fi
    rm -f -- "$err"
    "$CARGO" build --release "${locked[@]}" --package uaconsole --manifest-path "$CRATES/Cargo.toml"
}

# A path as a desktop-entry Exec program: bare, double-quoted, or unusable.
desktop_exec() {
    if [[ "$1" =~ ^[A-Za-z0-9._/@+,=-]+$ ]]; then
        printf '%s' "$1"
    elif [[ "$1" =~ ^[A-Za-z0-9._/@+,=\ -]+$ ]]; then
        printf '"%s"' "$1"
    else
        return 1
    fi
}

install_files() {
    local tdir bin
    tdir="$(target_dir)"
    bin="${tdir:-$CRATES/target}/release/uaconsole"
    if [[ ! -x "$bin" ]]; then
        fail "No built binary at $bin. Run without --no-build."
        exit 1
    fi

    header "Installing"
    install -Dm755 -- "$bin" "$BINDIR/uaconsole"
    # The release profile keeps line tables for profiling; the installed copy needs none.
    if command -v strip >/dev/null 2>&1; then
        strip --strip-debug -- "$BINDIR/uaconsole" 2>/dev/null || true
    fi
    ok "$BINDIR/uaconsole"
    # open-apollo-console (the tray's launcher with a Wine fallback) is gone;
    # remove a copy an older install left behind.
    rm -f -- "$BINDIR/open-apollo-console"

    install -Dm644 -- "$CRATES/packaging/uaconsole.svg" "$ICON_DIR/uaconsole.svg"
    local exec_value icon_value line
    if ! exec_value="$(desktop_exec "$BINDIR/uaconsole")"; then
        warn "Install path has characters a desktop entry cannot quote; the menu entry runs 'uaconsole' from PATH."
        exec_value=uaconsole
    fi
    # Menus find the icon by name under ~/.local/share; a custom prefix may not be searched.
    if [[ -z "$PREFIX" ]]; then icon_value=uaconsole; else icon_value="$ICON_DIR/uaconsole.svg"; fi
    mkdir -p -- "$APPS_DIR"
    while IFS= read -r line; do
        case "$line" in
            # The menu entry is the user's mixer: it controls the Apollo.  Plain
            # uaconsole (scripts, tests) stays read-only.
            Exec=*) printf 'Exec=%s --writes\n' "$exec_value" ;;
            Icon=*) printf 'Icon=%s\n' "$icon_value" ;;
            *) printf '%s\n' "$line" ;;
        esac
    done < "$CRATES/packaging/uaconsole.desktop" > "$APPS_DIR/uaconsole.desktop.tmp"
    chmod 644 -- "$APPS_DIR/uaconsole.desktop.tmp"
    mv -- "$APPS_DIR/uaconsole.desktop.tmp" "$APPS_DIR/uaconsole.desktop"
    ok "$APPS_DIR/uaconsole.desktop"
    if command -v update-desktop-database >/dev/null 2>&1; then
        update-desktop-database -q "$APPS_DIR" 2>/dev/null || true
    fi

    case ":$PATH:" in
        *":$BINDIR:"*) ;;
        *) warn "$BINDIR is not on your PATH. Add to your shell profile:"
           echo "        export PATH=\"$BINDIR:\$PATH\"" ;;
    esac
}

smoke_test() {
    header "Smoke test"
    local out="${XDG_CACHE_HOME:-$HOME/.cache}/open-apollo/uaconsole-smoke.png"
    mkdir -p -- "$(dirname -- "$out")"
    if timeout 120 "$BINDIR/uaconsole" --render "$out" --scale 1; then
        ok "Rendered a frame offscreen: $out"
    else
        warn "Offscreen render failed (exit $?). Needs Console resources and a GPU; see console-rs/README.md, Troubleshooting."
        return 1
    fi
}

main() {
    if [[ "$UNINSTALL" == 1 ]]; then
        do_uninstall
        exit 0
    fi
    if [[ "$EUID" -eq 0 && -z "$PREFIX" ]]; then
        fail "Run this as your desktop user, not root: it installs into ~/.local."
        exit 1
    fi
    if [[ -n "$COPY_FROM" ]]; then
        copy_resources "$COPY_FROM"
    fi
    if [[ "$CHECK_ONLY" == 1 ]]; then
        local rc=0
        report_resources || rc=1
        report_fonts || true
        report_gpu || true
        exit "$rc"
    fi

    if [[ "$BUILD" == 1 ]]; then
        build
    fi
    install_files

    local have_resources=1
    report_resources || have_resources=0
    report_fonts || true
    report_gpu || true
    if [[ "$SMOKE" == 1 ]]; then
        if [[ "$have_resources" == 1 ]]; then
            smoke_test || true
        else
            warn "Skipping the smoke test: no Console resources."
        fi
    fi

    header "Done"
    echo "  Start the mixer daemon:  python3 $REPO/mixer-engine/ua_mixer_daemon.py"
    echo "  Open the console:        uaconsole   (or 'Open Apollo Console' in your menu)"
    echo "  It connects to the daemon on 127.0.0.1:4710; another engine: uaconsole --engine HOST[:PORT]"
    echo "  Remove it:               bash $REPO/scripts/install-uaconsole.sh --uninstall"
}

main
