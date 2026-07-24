# shellcheck shell=bash
# scripts/lib.sh — shared UI/util helpers for open-apollo install & maintenance scripts.
#
# SOURCED, never executed. This is a behavior-identical extraction of the colour
# variables and log/util helpers that were duplicated across scripts/install*.sh,
# scripts/uninstall*.sh, scripts/check-deps.sh, and tools/apollo-init.sh.
#
# Every sourcing script MUST immediately assert the source succeeded:
#     command -v die >/dev/null 2>&1 || { echo "FATAL: scripts/lib.sh not sourced" >&2; exit 1; }
# The install/uninstall scripts run under sudo without `set -e`, so a failed
# `. lib.sh` would otherwise continue with every helper undefined (exit-127,
# silently swallowed) — including the `die` gates that block unverified builds.
#
# NOTE on prompt(): install-usb.sh keeps a stricter local prompt() (it adds a
# `[ -t 0 ]` guard) and intentionally overrides the one below. The two have
# genuinely different behaviour when stdin is piped, so they are NOT unified.

# Double-source guard. The if-form is unambiguous under `set -e`; the
# `[ ... ] && return 0` idiom returns non-zero on first source and can trip
# `set -e` in check-deps.sh.
if [ -n "${_UA_LIB_SOURCED:-}" ]; then
    return 0
fi
_UA_LIB_SOURCED=1

# --- Colours ---
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

# --- Log helpers ---
info()   { echo -e "${CYAN}[INFO]${NC}  $*"; }
ok()     { echo -e "${GREEN}[ OK ]${NC}  $*"; }
warn()   { echo -e "${YELLOW}[WARN]${NC}  $*"; }
fail()   { echo -e "${RED}[FAIL]${NC}  $*"; }
header() { echo -e "\n${BOLD}── $* ──${NC}"; }
die()    { fail "$*"; exit 1; }

# --- Prompt / sudo helpers ---
# Reads from /dev/tty so prompts work even when a sudo password is piped on stdin.
prompt() {
    local varname="$1"; shift
    if [ -e /dev/tty ]; then
        read -rp "$*" "$varname" < /dev/tty
    else
        eval "$varname=''"
    fi
}

# True if we can show interactive prompts.
can_prompt() { [ -e /dev/tty ]; }

run_sudo() {
    if [ "$(id -u)" -eq 0 ]; then "$@"; else sudo "$@"; fi
}
