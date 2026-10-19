# shellcheck shell=bash
# oa-log.sh — every installer run leaves a log you can attach to a bug report.
#
# Sourced by install.sh, setup-root.sh, install-uaconsole.sh,
# setup-plugin-hosting.sh and uninstall.sh.  oa_log_start sends the script's
# output (stdout and stderr) on to where it went before and, through
# scripts/oa_redact.py, to a log file with the user name, home folder, host
# name, machine id, serial numbers and MAC addresses taken out:
#
#   ~/.local/state/open-apollo/<kind>-<UTC date>-<time>.log   (the desktop user's)
#   /var/log/open-apollo/<kind>-<UTC date>-<time>.log         (root, no desktop user)
#
# The file belongs to the desktop user (the redactor runs as them) and stays
# after the run.  Children of a logged script inherit OA_LOG_CAPTURED=1 and
# don't log twice.  OA_INSTALL_LOG=FILE appends to FILE instead of starting a
# new one (the setup wizard keeps one log per run this way); OA_NO_LOG=1
# turns logging off.

OA_LOG_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OA_LOG_FILE=""
OA_LOG_PID=""

# The log folder for USER (empty: this user, or root's /var/log).
oa_log_dir() {
    local user="${1:-}" home
    if [ "$(id -u)" != 0 ]; then
        echo "${XDG_STATE_HOME:-$HOME/.local/state}/open-apollo"
        return
    fi
    if [ -n "$user" ] && [ "$user" != root ] && home=$(getent passwd "$user" | cut -d: -f6) && [ -n "$home" ]; then
        echo "$home/.local/state/open-apollo"
    else
        echo /var/log/open-apollo
    fi
}

# Run a command as USER when root and USER is someone else; else as is.
oa_log_as() {
    local user="$1"; shift
    if [ "$(id -u)" = 0 ] && [ -n "$user" ] && [ "$user" != root ] && id -u "$user" >/dev/null 2>&1; then
        if command -v runuser >/dev/null 2>&1; then
            runuser -u "$user" -- "$@"
        else
            sudo -u "$user" -- "$@"
        fi
    else
        "$@"
    fi
}

# oa_log_start KIND [USER] [ARGS...] — start logging this script's output.
# ARGS (the script's arguments) go in the log's first line, redacted.
oa_log_start() {
    local kind="$1" user="${2:-}"
    shift 2 2>/dev/null || shift $#
    [ "${OA_LOG_CAPTURED:-0}" = 1 ] && return 0
    [ "${OA_NO_LOG:-0}" = 1 ] && return 0
    command -v python3 >/dev/null 2>&1 || return 0
    [ "$(id -u)" = 0 ] || user=""
    local file="${OA_INSTALL_LOG:-}"
    if [ -z "$file" ]; then
        local dir
        dir=$(oa_log_dir "$user")
        if [ "$dir" = /var/log/open-apollo ]; then
            install -d -m 0755 "$dir" 2>/dev/null || return 0
        else
            oa_log_as "$user" mkdir -p "$dir" 2>/dev/null || return 0
        fi
        file="$dir/$kind-$(date -u +%Y%m%d-%H%M%S).log"
    fi
    # The redactor opens the file itself (as the user, never through a link).
    exec > >(oa_log_as "$user" python3 -u "$OA_LOG_LIB_DIR/oa_redact.py" ${user:+--user "$user"} \
                 --append "$file" --tee) 2>&1 || return 0
    OA_LOG_PID=$!
    OA_LOG_FILE="$file"
    export OA_LOG_CAPTURED=1 OA_INSTALL_LOG="$file"
    local rev=""
    rev=$(git -C "$OA_LOG_LIB_DIR/.." rev-parse --short HEAD 2>/dev/null) || rev=""
    echo "open-apollo $kind log: $(date -u +%Y-%m-%dT%H:%M:%SZ), $(uname -sr), ${rev:+commit $rev, }$(basename "$0") $*"
}

# oa_log_finish [LABEL] — say where the log is ("LABEL: FILE") and let it
# catch up before the script exits (its EXIT trap).
oa_log_finish() {
    [ -n "$OA_LOG_PID" ] || return 0
    local pid="$OA_LOG_PID"
    OA_LOG_PID=""
    [ -n "${1:-}" ] && echo -e "\n$1: $OA_LOG_FILE"
    exec 1>&- 2>&-
    # At most 5 s: a program left running with our output open keeps it alive.
    for _ in $(seq 50); do
        kill -0 "$pid" 2>/dev/null || return 0
        sleep 0.1
    done
}
