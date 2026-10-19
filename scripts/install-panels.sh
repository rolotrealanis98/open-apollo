#!/usr/bin/env bash
# install-panels.sh — install the plug-in panel specs (open-apollo-panels)
#
# uaconsole draws a plug-in's editor panel from its spec: where each control
# sits and what it drives. This installs the release pinned in
# configs/plugin-panels.conf. It downloads the tarball (or takes --from),
# checks its SHA-256 and its contents, then swaps the specs into place.
# The specs hold no art; `oa-assets plugins` makes that from your own UAD
# install.
#
# Usage: bash scripts/install-panels.sh [options]
#
#   --from TARBALL   install this tarball instead of downloading the pinned one
#   --sha256 HEX     expect this SHA-256 instead of the pinned one (for a
#                    panels build of your own)
#   --dest DIR       install into DIR (default: /usr/share/open-apollo/plugin-specs
#                    as root, else ~/.local/share/open-apollo/plugin-specs)
#   --conf FILE      read the pin from FILE (default: configs/plugin-panels.conf)
#   --uninstall      remove DIR
#   -h, --help       show this help
#
# Exit status: 0 installed (or already current), 1 refused (wrong SHA-256 or
# contents), 2 a bad option, 3 couldn't download.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
MAX_BYTES=$((64 << 20))

FROM=""
SHA_OVERRIDE=""
DEST=""
CONF="$PROJECT_DIR/configs/plugin-panels.conf"
UNINSTALL=0

if [ -t 1 ]; then
    RED='\033[0;31m' GREEN='\033[0;32m' YELLOW='\033[1;33m' CYAN='\033[0;36m' NC='\033[0m'
else
    RED='' GREEN='' YELLOW='' CYAN='' NC=''
fi
info() { echo -e "${CYAN}[INFO]${NC}  $*"; }
ok()   { echo -e "${GREEN}[ OK ]${NC}  $*"; }
warn() { echo -e "${YELLOW}[WARN]${NC}  $*"; }
fail() { echo -e "${RED}[FAIL]${NC}  $*"; }
usage() { sed -n '2,/^set -uo/{/^set -uo/d;s/^# \{0,1\}//;p;}' "${BASH_SOURCE[0]}"; }
bad_usage() { echo "install-panels.sh: $* (try --help)" >&2; exit 2; }

parse_args() {
    while [ $# -gt 0 ]; do
        case "$1" in
            --from|--sha256|--dest|--conf)
                [ $# -ge 2 ] && [ -n "$2" ] || bad_usage "$1 needs a value"
                case "$1" in
                    --from) FROM="$2" ;;
                    --sha256) SHA_OVERRIDE="$2" ;;
                    --dest) DEST="$2" ;;
                    --conf) CONF="$2" ;;
                esac
                shift 2 ;;
            --uninstall) UNINSTALL=1; shift ;;
            -h|--help) usage; exit 0 ;;
            *) bad_usage "unknown option $1" ;;
        esac
    done
    if [ -z "$DEST" ]; then
        if [ "$(id -u)" -eq 0 ]; then
            DEST=/usr/share/open-apollo/plugin-specs
        else
            DEST="${XDG_DATA_HOME:-$HOME/.local/share}/open-apollo/plugin-specs"
        fi
    fi
    case "$DEST" in
        /*/*) ;;
        *) bad_usage "--dest must be an absolute path below a folder, not $DEST" ;;
    esac
    DEST="${DEST%/}"
    if [ -n "$SHA_OVERRIDE" ] && ! [[ "$SHA_OVERRIDE" =~ ^[0-9a-f]{64}$ ]]; then
        bad_usage "--sha256 takes 64 lowercase hex digits"
    fi
    return 0
}

PANELS_VERSION=""
PANELS_SHA256=""
PANELS_URL=""

# The pin: KEY=value lines (read, never sourced).
read_conf() {
    local k v
    [ -r "$CONF" ] || { fail "Can't read $CONF"; return 1; }
    while IFS='=' read -r k v; do
        case "$k" in
            PANELS_VERSION) PANELS_VERSION="$v" ;;
            PANELS_SHA256) PANELS_SHA256="$v" ;;
            PANELS_URL) PANELS_URL="$v" ;;
        esac
    done < "$CONF"
    if ! [[ "$PANELS_VERSION" =~ ^[0-9][0-9A-Za-z.+-]*$ ]] || ! [[ "$PANELS_SHA256" =~ ^[0-9a-f]{64}$ ]]; then
        fail "$CONF needs PANELS_VERSION and a 64-digit PANELS_SHA256"
        return 1
    fi
    return 0
}

# Ours to remove: holds our marker or only spec files.
looks_like_ours() {
    [ -d "$1" ] && [ ! -L "$1" ] || return 1
    [ -f "$1/.source" ] && return 0
    local f
    for f in "$1"/* "$1"/.[!.]*; do
        [ -e "$f" ] || continue
        case "${f##*/}" in PluginSpec_*.json|VERSION|LICENSE|NOTICE.md|README.md) ;; *) return 1 ;; esac
    done
    return 0
}

uninstall_panels() {
    if [ ! -e "$DEST" ]; then
        ok "No plug-in panel specs in $DEST"
        return 0
    fi
    if ! looks_like_ours "$DEST"; then
        fail "$DEST holds files these specs don't; not removing it"
        return 1
    fi
    rm -rf -- "$DEST"
    [ "$(basename "$(dirname "$DEST")")" = open-apollo ] && rmdir "$(dirname "$DEST")" 2>/dev/null
    ok "Removed $DEST"
    return 0
}

TMP=""
cleanup() { [ -n "$TMP" ] && rm -rf -- "$TMP"; }

download() {
    local url="${PANELS_URL//\{version\}/$PANELS_VERSION}" out="$1"
    case "$url" in
        https://*) ;;
        *) fail "PANELS_URL must be https: $url"; return 1 ;;
    esac
    info "Downloading the plug-in panel specs $PANELS_VERSION"
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL --proto '=https' --tlsv1.2 --retry 2 --connect-timeout 15 --max-time 300 \
            --max-filesize "$MAX_BYTES" -o "$out" "$url"
    elif command -v wget >/dev/null 2>&1; then
        wget -q --https-only --tries=3 --timeout=30 -O "$out" "$url"
    else
        fail "Neither curl nor wget is installed"
        return 1
    fi
}

# Every entry is a file or folder under one open-apollo-panels-<version>/ top.
check_listing() {
    local tarball="$1" line name top=""
    while IFS= read -r line; do
        case "$line" in
            -*|d*) ;;
            *) fail "The tarball holds a link or a special file: $line"; return 1 ;;
        esac
    done < <(tar -tvzf "$tarball")
    while IFS= read -r name; do
        if ! [[ "$name" =~ ^(open-apollo-panels-[0-9][0-9A-Za-z.+-]*)(/|$) ]]; then
            fail "Unexpected entry in the tarball: $name"
            return 1
        fi
        [ -z "$top" ] && top="${BASH_REMATCH[1]}"
        if [ "${BASH_REMATCH[1]}" != "$top" ]; then
            fail "The tarball has more than one top folder"
            return 1
        fi
        case "/$name/" in
            */../*|*/./*) fail "Unexpected path in the tarball: $name"; return 1 ;;
        esac
    done < <(tar -tzf "$tarball")
    [ -n "$top" ] || { fail "The tarball is empty"; return 1; }
    TOP="$top"
}

install_panels() {
    local expect="${SHA_OVERRIDE:-$PANELS_SHA256}" tarball got size src version f n=0
    # A given tarball must match even when that release is installed already
    if [ -n "$FROM" ] && [ -f "$FROM" ] && [ "$(sha256sum "$FROM" | cut -d' ' -f1)" != "$expect" ]; then
        fail "SHA-256 mismatch: $FROM is not the expected release ($expect)"
        return 1
    fi
    if [ -f "$DEST/.source" ] && [ "$(cat "$DEST/.source" 2>/dev/null)" = "$expect" ]; then
        ok "Plug-in panel specs $(cat "$DEST/VERSION" 2>/dev/null) are already in $DEST"
        return 0
    fi
    if [ -e "$DEST" ] && ! looks_like_ours "$DEST"; then
        fail "$DEST holds files these specs don't; not replacing it"
        return 1
    fi
    TMP=$(mktemp -d "${TMPDIR:-/tmp}/open-apollo-panels.XXXXXX") || return 1
    tarball="$TMP/panels.tar.gz"
    if [ -n "$FROM" ]; then
        [ -f "$FROM" ] || { fail "No such file: $FROM"; return 1; }
        cp -- "$FROM" "$tarball" || return 1
    elif ! download "$tarball"; then
        fail "Couldn't download the plug-in panel specs"
        return 3
    fi
    size=$(stat -c %s "$tarball" 2>/dev/null || echo 0)
    if [ "$size" -gt "$MAX_BYTES" ]; then
        fail "The tarball is larger than $((MAX_BYTES >> 20)) MB"
        return 1
    fi
    got=$(sha256sum "$tarball" | cut -d' ' -f1)
    if [ "$got" != "$expect" ]; then
        fail "SHA-256 mismatch: expected $expect, got $got"
        return 1
    fi
    check_listing "$tarball" || return 1
    if ! { mkdir "$TMP/x" && tar -xzf "$tarball" -C "$TMP/x" --no-same-owner --no-same-permissions; }; then
        fail "Couldn't unpack the tarball"
        return 1
    fi
    src="$TMP/x/$TOP"
    version=$(head -c 64 "$src/VERSION" 2>/dev/null | tr -d '[:space:]')
    if [ -z "$SHA_OVERRIDE" ] && [ "$version" != "$PANELS_VERSION" ]; then
        fail "The tarball is version '$version', the pin says $PANELS_VERSION"
        return 1
    fi
    for f in "$src"/specs/*; do
        [ -e "$f" ] || continue
        if ! [[ "${f##*/}" =~ ^PluginSpec_[a-z0-9_]+\.json$ ]] || [ ! -f "$f" ]; then
            fail "Unexpected file in specs/: ${f##*/}"
            return 1
        fi
        n=$((n + 1))
    done
    [ "$n" -gt 0 ] || { fail "The tarball holds no specs"; return 1; }

    local new="$DEST.new.$$" old="$DEST.old.$$"
    mkdir -p -- "$(dirname "$DEST")" || return 1
    rm -rf -- "$new" && mkdir -m 755 -- "$new" || return 1
    cp -- "$src"/specs/PluginSpec_*.json "$new/" || { rm -rf -- "$new"; return 1; }
    for f in VERSION LICENSE NOTICE.md README.md; do
        [ -f "$src/$f" ] && cp -- "$src/$f" "$new/"
    done
    echo "$expect" > "$new/.source"
    chmod 644 -- "$new"/* "$new/.source"
    if [ -e "$DEST" ]; then
        mv -- "$DEST" "$old" || { rm -rf -- "$new"; return 1; }
    fi
    if ! mv -- "$new" "$DEST"; then
        [ -e "$old" ] && mv -- "$old" "$DEST"
        rm -rf -- "$new"
        return 1
    fi
    rm -rf -- "$old"
    ok "Installed $n plug-in panel specs ($version) in $DEST"
    return 0
}

main() {
    parse_args "$@"
    if [ "$UNINSTALL" = 1 ]; then
        uninstall_panels
        return $?
    fi
    if [ -z "$SHA_OVERRIDE" ] || [ -z "$FROM" ]; then
        read_conf || return 1
    fi
    trap cleanup EXIT
    install_panels
}

TOP=""
if [ "${BASH_SOURCE[0]}" = "$0" ]; then
    main "$@"
    exit $?
fi
