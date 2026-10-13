#!/usr/bin/env bash
# Open the Apollo mixer console.
#
# Prefers the native console (uaconsole, built from console-rs/) when it is
# installed and falls back to UA's Windows UAD Console under Wine. Both talk to
# the local mixer daemon (mixer-engine/ua_mixer_daemon.py) on 127.0.0.1:4710.
#
# Usage: open-apollo-console.sh [--native | --wine] [--engine HOST[:PORT]] [--which] [-- ARGS...]
#
#   --native         use uaconsole; fail if it is not installed
#   --wine           use the Windows UAD Console under Wine
#   --engine ADDR    mixer engine for uaconsole (default 127.0.0.1:4710)
#   --which          print the console that would start ("native PATH", "wine CMD"
#                    or "none") and exit; exit status 1 for "none"
#   ARGS             passed to the console unchanged
#
# Environment:
#   OPEN_APOLLO_CONSOLE       auto (default), native or wine
#   UACONSOLE_BIN             uaconsole binary (default: next to this script, then
#                             on PATH, then ~/.local/bin)
#   UACONSOLE_ENGINE          engine address for uaconsole (default 127.0.0.1:4710)
#   OPEN_APOLLO_WINE_CONSOLE  executable that starts the Windows Console under Wine,
#                             e.g. a script that also installs a rendering shim.
#                             Default: wine "UAD Console.exe" from $WINEPREFIX.
#   WINEPREFIX                Wine prefix with UAD Console installed (default ~/wine-ua)
set -euo pipefail

DEFAULT_ENGINE=127.0.0.1:4710
HERE="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
WINEPREFIX="${WINEPREFIX:-$HOME/wine-ua}"
CONSOLE_DIR="$WINEPREFIX/drive_c/Program Files (x86)/Universal Audio/UAD Console"

MODE="${OPEN_APOLLO_CONSOLE:-auto}"
ENGINE="${UACONSOLE_ENGINE:-$DEFAULT_ENGINE}"
WHICH=0

usage() { sed -n '2,/^set -euo/{/^set -euo/d;s/^# \{0,1\}//;p}' "${BASH_SOURCE[0]}"; }
warn() { printf 'open-apollo-console: %s\n' "$*" >&2; }

while [[ $# -gt 0 ]]; do
    case "$1" in
        --native) MODE=native; shift ;;
        --wine) MODE=wine; shift ;;
        --engine)
            [[ $# -ge 2 && -n "$2" ]] || { warn "--engine needs HOST[:PORT]"; exit 2; }
            ENGINE="$2"; shift 2 ;;
        --engine=*) ENGINE="${1#--engine=}"; shift ;;
        --which) WHICH=1; shift ;;
        -h|--help) usage; exit 0 ;;
        --) shift; break ;;
        *) break ;;
    esac
done

case "$MODE" in
    auto|native|wine) ;;
    *) warn "OPEN_APOLLO_CONSOLE must be auto, native or wine (got '$MODE')"; exit 2 ;;
esac

find_native() {
    local c
    for c in "${UACONSOLE_BIN:-}" "$HERE/uaconsole" "$(command -v uaconsole 2>/dev/null || true)" \
             "$HOME/.local/bin/uaconsole"; do
        if [[ -n "$c" && -x "$c" && ! -d "$c" ]]; then
            printf '%s\n' "$c"
            return 0
        fi
    done
    return 1
}

find_wine() {
    if [[ -n "${OPEN_APOLLO_WINE_CONSOLE:-}" ]]; then
        [[ -x "$OPEN_APOLLO_WINE_CONSOLE" ]] || return 1
        printf '%s\n' "$OPEN_APOLLO_WINE_CONSOLE"
        return 0
    fi
    if command -v wine >/dev/null 2>&1 && [[ -f "$CONSOLE_DIR/UAD Console.exe" ]]; then
        printf 'wine\n'
        return 0
    fi
    return 1
}

# HOST[:PORT], [V6]:PORT or a bare IPv6 address -> "HOST PORT"
split_engine() {
    local addr="$1" host port=4710
    case "$addr" in
        \[*\]:*) host="${addr%:*}"; port="${addr##*:}" ;;
        \[*\]) host="$addr" ;;
        *:*:*) host="$addr" ;;
        *:*) host="${addr%:*}"; port="${addr##*:}" ;;
        *) host="$addr" ;;
    esac
    host="${host#[}"; host="${host%]}"
    printf '%s %s\n' "$host" "$port"
}

engine_listening() {
    # shellcheck disable=SC2016 # $1/$2 expand in the child shell
    timeout 1 bash -c 'exec 3<>"/dev/tcp/$1/$2"' _ "$1" "$2" 2>/dev/null
}

check_engine() {
    local host port
    read -r host port < <(split_engine "$1")
    if ! engine_listening "$host" "$port"; then
        warn "no mixer engine on $1. Start the daemon first:"
        warn "  python3 mixer-engine/ua_mixer_daemon.py   (or use the tray's Restart Daemon)"
    fi
}

not_installed() {
    case "$REQUESTED" in
        native) warn "uaconsole is not installed." ;;
        wine) warn "UAD Console under Wine not found (looked in $CONSOLE_DIR; is wine installed?)."
              warn "Set OPEN_APOLLO_WINE_CONSOLE to your Wine launcher, or WINEPREFIX to the prefix."
              return ;;
        *) warn "no console installed." ;;
    esac
    if [[ -f "$HERE/../scripts/install-uaconsole.sh" ]]; then
        warn "Install the native console:  bash $(cd -- "$HERE/.." && pwd)/scripts/install-uaconsole.sh"
    else
        warn "Install the native console with scripts/install-uaconsole.sh from the open-apollo repo."
    fi
    warn "See console-rs/README.md for where the Console art comes from."
}

REQUESTED="$MODE"
native=""
wine_cmd=""
native="$(find_native)" || native=""
wine_cmd="$(find_wine)" || wine_cmd=""

case "$MODE" in
    auto) if [[ -n "$native" ]]; then MODE=native; elif [[ -n "$wine_cmd" ]]; then MODE=wine; else MODE=none; fi ;;
    native) [[ -n "$native" ]] || MODE=none ;;
    wine) [[ -n "$wine_cmd" ]] || MODE=none ;;
esac

if [[ "$WHICH" == 1 ]]; then
    case "$MODE" in
        native) printf 'native %s\n' "$native" ;;
        wine)
            if [[ "$wine_cmd" == wine ]]; then
                printf 'wine %s\n' "$CONSOLE_DIR/UAD Console.exe"
            else
                printf 'wine %s\n' "$wine_cmd"
            fi ;;
        none) printf 'none\n'; exit 1 ;;
    esac
    exit 0
fi

case "$MODE" in
    native)
        check_engine "$ENGINE"
        exec "$native" --engine "$ENGINE" "$@"
        ;;
    wine)
        if [[ "$ENGINE" != "$DEFAULT_ENGINE" ]]; then
            warn "--engine is ignored by the Windows Console; it always uses 127.0.0.1:4710"
        fi
        check_engine "$DEFAULT_ENGINE"
        export WINEPREFIX
        if [[ "$wine_cmd" != wine ]]; then
            exec "$wine_cmd" "$@"
        fi
        warn "starting UAD Console under stock Wine; without a DirectComposition shim it"
        warn "may open blank. Set OPEN_APOLLO_WINE_CONSOLE to your Wine launcher, or use uaconsole."
        cd -- "$CONSOLE_DIR"
        exec wine "UAD Console.exe" "$@"
        ;;
    none)
        not_installed
        exit 1
        ;;
esac
