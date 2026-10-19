#!/usr/bin/env bash
# setup-plugin-hosting.sh — run UA plug-ins on the Apollo's DSP (experimental, opt-in)
#
# Maps your Powered plug-ins for the Apollo by running your own UAD install's
# plug-in DLLs offline (tools/plugin-mapper: no Wine, no device, nothing
# leaves this computer), then makes the mixer daemon host them. Only the
# plug-ins your Apollo authorizes are mapped; the others are mapped when a
# licence appears (the daemon notices, or Settings > Plug-Ins > RESCAN).
#
# Mapping runs in the background (a user service, one worker per CPU core
# but one) and takes a while. Each plug-in appears in the Console's plug-in
# browser as soon as its maps are built. Maps are kept across reinstalls
# (uninstall.sh --purge keeps them unless --purge-plugin-maps): a rerun only
# maps plug-ins whose DLL, UAD2SDK.dll or mapper changed.
#
# Run it as your normal user after setup (setup-root.sh has already put the
# DSP modules in /lib/firmware/universal-audio/plugins). The driver must be
# loaded with plugin_host=1 input_strips=1: `sudo bash scripts/install.sh
# --plugin-hosting=ROOT` does that and then runs this script.
#
# Usage: bash scripts/setup-plugin-hosting.sh --from ROOT [options]
#
#   --from ROOT      your Windows UAD install root (a mounted Windows partition
#                    or a Wine drive_c), as given to setup
#   --out DIR        where the maps go (default ~/.local/share/open-apollo-plugin-maps)
#   --all            map every installed plug-in, not only the known-good list
#   --workers N      plug-ins mapped at once (default: CPU cores - 1)
#   --follow SECS    show the mapping progress for this long, then leave it
#                    running in the background (default 120; 0: don't wait)
#   --wait           show the progress until every plug-in is mapped
#   --no-daemon      map, but leave the mixer daemon as it is (implies --wait)
#   --remove         stop hosting: remove the daemon drop-in and the mapping
#                    service (the maps stay; add --purge-maps to delete them)
#   -h, --help       show this help
#
# The maps are made from UA's code on this machine: never share or commit them.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
DATA="${XDG_DATA_HOME:-$HOME/.local/share}"
VENV="$DATA/open-apollo/plugin-venv"
OUT="$DATA/open-apollo-plugin-maps"
UNIT_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
UNIT="open-apollo-plugin-maps.service"
DROPIN_DIR="$UNIT_DIR/ua-mixer-daemon.service.d"
DROPIN="$DROPIN_DIR/plugin-hosting.conf"
FROM="" ALL="" WORKERS="" DAEMON=1 REMOVE=0 PURGE_MAPS=0 FOLLOW=120
ARGS=("$@")

die() { echo "setup-plugin-hosting: $*" >&2; exit 1; }
while [ $# -gt 0 ]; do
    case "$1" in
        --from) FROM="${2:-}"; shift 2 ;;
        --out) OUT="${2:-}"; shift 2 ;;
        --all) ALL="--all"; shift ;;
        --workers) WORKERS="${2:-}"; shift 2 ;;
        --follow) FOLLOW="${2:-}"; shift 2 ;;
        --wait) FOLLOW=-1; shift ;;
        --no-daemon) DAEMON=0; FOLLOW=-1; shift ;;
        --remove) REMOVE=1; shift ;;
        --purge-maps) PURGE_MAPS=1; shift ;;
        -h|--help) sed -n '2,/^set -uo/{/^set -uo/d;s/^# \{0,1\}//;p;}' "${BASH_SOURCE[0]}"; exit 0 ;;
        *) die "unknown option $1 (see --help)" ;;
    esac
done
[ "$(id -u)" != 0 ] || die "run this as your normal user, not root"
# shellcheck source=scripts/oa-log.sh
. "$SCRIPT_DIR/oa-log.sh"
oa_log_start plugin-hosting "" "${ARGS[@]}"
trap 'oa_log_finish "Log"' EXIT
[[ "$FOLLOW" =~ ^-?[0-9]+$ ]] || die "--follow takes seconds"
[ -z "$WORKERS" ] || [[ "$WORKERS" =~ ^[1-9][0-9]*$ ]] || die "--workers takes a number"

have_user_systemd() { systemctl --user show-environment >/dev/null 2>&1; }

restart_daemon() {
    systemctl --user daemon-reload 2>/dev/null || true
    if systemctl --user is-enabled --quiet ua-mixer-daemon.service 2>/dev/null; then
        systemctl --user restart ua-mixer-daemon.service && echo "Mixer daemon restarted."
    fi
}

# Whether the restarted daemon could turn plug-in hosting on. A driver loaded while the Apollo
# was already on has not cold-started its DSPs: the daemon runs without plug-ins and says so
# until the Apollo is power-cycled, then turns them on by itself.
check_hosting() {
    local answer="" i
    for i in $(seq 20); do
        answer=$(python3 - <<'PY' 2>/dev/null
import json, socket
try:
    s = socket.create_connection(('127.0.0.1', 4710), 2); s.settimeout(3)
    out = {}
    for n, path in enumerate(('/NativePluginHosting', '/NativePluginHostingReason')):
        s.sendall(f'get {path}/value?func_id=h{n}'.encode() + b'\0')
        buf = b''
        while True:
            while b'\0' not in buf:
                chunk = s.recv(65536)
                if not chunk:
                    raise OSError
                buf += chunk
            raw, buf = buf.split(b'\0', 1)
            r = json.loads(raw) if raw else {}
            if str((r.get('parameters') or {}).get('func_id')) == f'h{n}':
                out[path] = r.get('data')
                break
    print('on' if out['/NativePluginHosting'] is True else
          'off ' + (out['/NativePluginHostingReason'] or '') if out['/NativePluginHosting'] is False else '')
except Exception:
    pass
PY
)
        [ -n "$answer" ] && break
        sleep 1
    done
    case "$answer" in
        on) echo "[ OK ]  Plug-in hosting is on." ;;
        off*) echo "[WARN]  ${answer#off }"
              echo "[WARN]  Power-cycle the Apollo (switch it off and on again): plug-ins turn on by themselves" \
                   "once the driver sees the cold start; no restart of anything else is needed." ;;
        *) echo "[INFO]  Power-cycle the Apollo once after setup if the Console's plug-in browser shows no" \
                "Native DSP loads: the driver's plug-in host needs a cold start of the DSPs." ;;
    esac
}

if [ "$REMOVE" = 1 ]; then
    systemctl --user stop "$UNIT" 2>/dev/null || true
    rm -f "$DROPIN" "$UNIT_DIR/$UNIT"
    [ "$PURGE_MAPS" = 1 ] && rm -rf "$OUT"
    restart_daemon
    echo "Plug-in hosting removed (daemon drop-in and mapping service)."
    [ "$PURGE_MAPS" = 1 ] && echo "Maps deleted ($OUT)." || echo "Maps kept in $OUT (--purge-maps deletes them)."
    echo "The driver options stay until you run: sudo rm /etc/modprobe.d/open-apollo-plugin-hosting.conf"
    exit 0
fi

[ -n "$FROM" ] || die "--from ROOT is required (your Windows UAD install root)"
[ -f /lib/firmware/universal-audio/plugins/manifest.json ] ||
    die "no DSP modules in /lib/firmware/universal-audio/plugins: run setup first (oa-assets dsp)"

echo "== Python environment ($VENV)"
if [ ! -x "$VENV/bin/python3" ]; then
    echo "Creating it (once)…"
    # System site packages stay visible: the daemon also runs from this interpreter.
    python3 -m venv --system-site-packages "$VENV" || die "python3 -m venv failed (install python's venv module)"
fi
echo "Installing numpy, unicorn, capstone and pefile into it (a minute the first time)…"
"$VENV/bin/python3" -m pip install --quiet --disable-pip-version-check \
    -r "$PROJECT_DIR/tools/plugin-mapper/requirements.txt" || die "pip could not install numpy/unicorn/capstone/pefile"

mapper() { (cd "$PROJECT_DIR/tools/plugin-mapper" && "$VENV/bin/python3" -m plugin_mapper "$@"); }

echo "== Copying what mapping reads from your install ($OUT/source)"
# Mapping goes on after setup closes the Windows partition, and again when a licence appears.
mapper stage --from "$FROM" --to "$OUT/source" $ALL || die "could not copy the plug-in DLLs from $FROM"

LOG="$OUT/build.log"
mkdir -p "$OUT"
touch "$LOG"
MAP_ARGS=(-m plugin_mapper --from "$OUT/source" --out "$OUT" --markers)
[ -n "$ALL" ] && MAP_ARGS+=(--all)
CORES=$(nproc 2>/dev/null || echo 2)
N=${WORKERS:-$(( CORES > 1 ? CORES - 1 : 1 ))}

if [ "$DAEMON" = 1 ]; then
    echo "== Mixer daemon"
    mkdir -p "$DROPIN_DIR"
    cat > "$DROPIN" <<EOF
# Written by scripts/setup-plugin-hosting.sh; remove with --remove.
[Service]
ExecStart=
ExecStart=$VENV/bin/python3 $PROJECT_DIR/mixer-engine/ua_mixer_daemon.py --native-plugin-bundles $OUT --preserve-hardware
EOF
    restart_daemon
    check_hosting
fi

echo "== Mapping your plug-ins in the background ($N worker$([ "$N" = 1 ] || echo s))"
OFFSET=$(stat -c %s "$LOG" 2>/dev/null || wc -c < "$LOG")
BACKGROUND=""
if [ "$DAEMON" = 1 ] && have_user_systemd; then
    mkdir -p "$UNIT_DIR"
    cat > "$UNIT_DIR/$UNIT" <<EOF
# Written by scripts/setup-plugin-hosting.sh; remove with --remove.
# The mixer daemon starts it again when the Apollo authorizes a plug-in that has no maps yet.
[Unit]
Description=Open Apollo: map UA plug-ins for the Apollo's DSP

[Service]
Type=oneshot
WorkingDirectory=$PROJECT_DIR/tools/plugin-mapper
Environment=OA_PLUGIN_MAP_WORKERS=$N
ExecStart=$VENV/bin/python3 ${MAP_ARGS[*]}
StandardOutput=append:$LOG
StandardError=append:$LOG
Nice=10
IOSchedulingClass=idle
EOF
    systemctl --user daemon-reload
    systemctl --user start --no-block "$UNIT" && BACKGROUND="systemd"
fi
if [ -z "$BACKGROUND" ]; then
    # No user systemd (or --no-daemon): run it detached, logging to the same file.
    (cd "$PROJECT_DIR/tools/plugin-mapper" &&
        OA_PLUGIN_MAP_WORKERS=$N nohup setsid "$VENV/bin/python3" "${MAP_ARGS[@]}" >> "$LOG" 2>&1 < /dev/null &)
    BACKGROUND="detached"
fi

# Show its progress: per-plug-in lines and the wizard's ::progress markers, as they come.
running() {
    if [ "$BACKGROUND" = systemd ]; then
        case "$(systemctl --user show -p ActiveState --value "$UNIT" 2>/dev/null)" in
            active|activating|reloading) return 0 ;; *) return 1 ;;
        esac
    else
        pgrep -f -- "--out $OUT --markers" >/dev/null 2>&1
    fi
}
started=$(date +%s)
sleep 1
while :; do
    size=$(stat -c %s "$LOG" 2>/dev/null || wc -c < "$LOG")
    if [ "$size" -gt "$OFFSET" ]; then
        tail -c +"$((OFFSET + 1))" "$LOG" | head -c "$((size - OFFSET))"
        OFFSET=$size
    fi
    running || break
    if [ "$FOLLOW" -ge 0 ] && [ $(( $(date +%s) - started )) -ge "$FOLLOW" ]; then
        echo "Mapping continues in the background; each plug-in appears in the Console's plug-in browser"
        echo "as soon as its maps are built. Progress: tail -f $LOG"
        exit 0
    fi
    sleep 2
done
grep -q "^Plug-in maps: " "$LOG" || die "mapping stopped early (see $LOG)"
echo "Done. Plug-ins your Apollo authorizes appear in the Console's plug-in browser as Native DSP"
echo "loads; the Apollo's licence check still decides each load."
