#!/usr/bin/env bash
# setup-plugin-hosting.sh — run UA plug-ins on the Apollo's DSP (experimental, opt-in)
#
# Builds the control mappings for your Powered plug-ins by running your own
# UAD install's plug-in DLLs offline (tools/plugin-mapper: no Wine, no device,
# nothing leaves this computer), then makes the mixer daemon host them.
# Run it as your normal user after setup (setup-root.sh has already put the
# DSP modules in /lib/firmware/universal-audio/plugins). The driver must be
# loaded with plugin_host=1 input_strips=1: `sudo bash scripts/install.sh
# --plugin-hosting` does that and then runs this script.
#
# Usage: bash scripts/setup-plugin-hosting.sh --from ROOT [options]
#
#   --from ROOT    your Windows UAD install root (a mounted Windows partition
#                  or a Wine drive_c), as given to setup
#   --out DIR      where the bundles go (default ~/.local/share/open-apollo/plugin-bundles)
#   --all          map every installed plug-in, not only the known-good list
#   --workers N    parallel plug-ins (default: CPU count - 1, at most 8)
#   --no-daemon    build the bundles but leave the mixer daemon as it is
#   --remove       stop hosting: remove the daemon drop-in and the bundles
#   -h, --help     show this help
#
# A full build of the known-good list takes about 10-40 minutes, mostly
# single-threaded emulation. The bundles are made from UA's code on this
# machine: never share or commit them.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
DATA="${XDG_DATA_HOME:-$HOME/.local/share}/open-apollo"
VENV="$DATA/plugin-venv"
OUT="$DATA/plugin-bundles"
DROPIN_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/ua-mixer-daemon.service.d"
DROPIN="$DROPIN_DIR/plugin-hosting.conf"
FROM="" ALL=() WORKERS=() DAEMON=1 REMOVE=0
ARGS=("$@")

die() { echo "setup-plugin-hosting: $*" >&2; exit 1; }
while [ $# -gt 0 ]; do
    case "$1" in
        --from) FROM="${2:-}"; shift 2 ;;
        --out) OUT="${2:-}"; shift 2 ;;
        --all) ALL=(--all); shift ;;
        --workers) WORKERS=(--workers "${2:-}"); shift 2 ;;
        --no-daemon) DAEMON=0; shift ;;
        --remove) REMOVE=1; shift ;;
        -h|--help) sed -n '2,/^set -uo/{/^set -uo/d;s/^# \{0,1\}//;p;}' "${BASH_SOURCE[0]}"; exit 0 ;;
        *) die "unknown option $1 (see --help)" ;;
    esac
done
[ "$(id -u)" != 0 ] || die "run this as your normal user, not root"
# shellcheck source=scripts/oa-log.sh
. "$SCRIPT_DIR/oa-log.sh"
oa_log_start plugin-hosting "" "${ARGS[@]}"
trap 'oa_log_finish "Log"' EXIT

restart_daemon() {
    systemctl --user daemon-reload 2>/dev/null || true
    if systemctl --user is-enabled --quiet ua-mixer-daemon.service 2>/dev/null; then
        systemctl --user restart ua-mixer-daemon.service && echo "Mixer daemon restarted."
    fi
}

if [ "$REMOVE" = 1 ]; then
    rm -f "$DROPIN"
    rm -rf "$OUT"
    restart_daemon
    echo "Plug-in hosting removed (bundles and daemon drop-in). The driver options stay until"
    echo "you run: sudo rm /etc/modprobe.d/open-apollo-plugin-hosting.conf"
    exit 0
fi

[ -n "$FROM" ] || die "--from ROOT is required (your Windows UAD install root)"
[ -f /lib/firmware/universal-audio/plugins/manifest.json ] ||
    die "no DSP modules in /lib/firmware/universal-audio/plugins: run setup first (oa-assets dsp)"

echo "== Python environment ($VENV)"
if [ ! -x "$VENV/bin/python3" ]; then
    # System site packages stay visible: the daemon also runs from this interpreter.
    python3 -m venv --system-site-packages "$VENV" || die "python3 -m venv failed (install python's venv module)"
fi
"$VENV/bin/python3" -m pip install --quiet --disable-pip-version-check \
    -r "$PROJECT_DIR/tools/plugin-mapper/requirements.txt" || die "pip could not install numpy/unicorn/capstone/pefile"

echo "== Mapping your plug-ins (offline; this takes a while)"
(cd "$PROJECT_DIR/tools/plugin-mapper" &&
    "$VENV/bin/python3" -m plugin_mapper --from "$FROM" --out "$OUT" "${ALL[@]}" "${WORKERS[@]}")
rc=$?
[ "$rc" = 0 ] || die "no bundle was built (details: $OUT/index.json)"

[ "$DAEMON" = 1 ] || { echo "Bundles in $OUT. Daemon left unchanged (--no-daemon)."; exit 0; }
echo "== Mixer daemon"
mkdir -p "$DROPIN_DIR"
cat > "$DROPIN" <<EOF
# Written by scripts/setup-plugin-hosting.sh; remove with --remove.
[Service]
ExecStart=
ExecStart=$VENV/bin/python3 $PROJECT_DIR/mixer-engine/ua_mixer_daemon.py --native-plugin-bundles $OUT --preserve-hardware
EOF
restart_daemon
echo "Done. Plug-ins on the known-good list appear in the console's plug-in browser as"
echo "Native DSP loads; the Apollo's licence check still decides each load."
