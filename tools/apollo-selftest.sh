#!/bin/bash
# apollo-selftest.sh - opt-in duplex self-test for the ua_apollo driver
#
# This is an ACTIVE test: it plays a tone through the Apollo and records the
# monitor (MON) channels back.  It asks before every step and you can stop at
# any prompt.  Steps:
#   1. optionally stop PipeWire/WirePlumber (so the test owns the card), and
#      restore exactly what was running when the script ends
#   2. for each sample rate (44.1k .. 192k): aplay a generated S32_LE tone on
#      playback channels 1/2 while arecord captures all record channels, then
#      classify the MON L/R capture
#   3. print a summary table and append it to the report file
#
# Verdicts (details in tools/TESTING.md):
#   PASS    the tone came back on both MON channels
#   SILENT  capture is live but the MON channels are all zero
#   DEAD    nothing live: no data, all zero, or a frozen repeating buffer
#   ROUTE   MON has data but not the tone (or the tone is on the wrong channel)
#   SKIP    rate not run / not supported by the card
#   ERROR   aplay/arecord failed or the MON channels could not be located
#
# TURN YOUR SPEAKERS/MONITOR LEVEL DOWN FIRST.  The tone is -12 dBFS on the
# MON outputs.  Run it as your normal user (no sudo): the device nodes are
# owned by you via udev.  The script writes only temporary WAV files.
#
# Usage:
#   tools/apollo-selftest.sh [--card N] [--device D] [--report FILE]
#                            [--rates "44100 48000 ..."] [--seconds S]
#                            [--yes] [--keep]

set -u

CARD=""
DEVICE=0
REPORT=""
RATES="44100 48000 88200 96000 176400 192000"
CAP_SECONDS=3
ASSUME_YES=0
KEEP=0

usage() {
    sed -n '2,/^$/p' "$0" | sed 's/^# \{0,1\}//'
    exit "${1:-0}"
}

while [ $# -gt 0 ]; do
    case "$1" in
        --card)    CARD="${2:?--card needs a number}"; shift 2 ;;
        --device)  DEVICE="${2:?--device needs a number}"; shift 2 ;;
        --report)  REPORT="${2:?--report needs a file}"; shift 2 ;;
        --rates)   RATES="${2:?--rates needs a list}"; shift 2 ;;
        --seconds) CAP_SECONDS="${2:?--seconds needs a number}"; shift 2 ;;
        --yes)     ASSUME_YES=1; shift ;;
        --keep)    KEEP=1; shift ;;
        -h|--help) usage 0 ;;
        *) echo "unknown option: $1" >&2; usage 1 ;;
    esac
done

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WAVCHECK="$HERE/apollo-wavcheck.py"

have() { command -v "$1" >/dev/null 2>&1; }
die()  { echo "error: $*" >&2; exit 1; }

for c in aplay arecord python3 timeout; do
    have "$c" || die "$c not found (alsa-utils, python3 and coreutils are required)"
done
[ -r "$WAVCHECK" ] || die "missing $WAVCHECK"
case "$CAP_SECONDS" in ''|*[!0-9]*) die "--seconds must be a whole number" ;; esac
[ "$CAP_SECONDS" -ge 2 ] || die "--seconds must be at least 2"
TONE_SECONDS=$((CAP_SECONDS + 2))

# ------------------------------------------------------------------ prompts
ask() {   # ask "question" -> 0 yes, 1 no
    [ "$ASSUME_YES" = 1 ] && { echo "$1 [y/N] y (--yes)"; return 0; }
    local a
    if [ -r /dev/tty ]; then
        read -r -p "$1 [y/N] " a < /dev/tty || return 1
    else
        read -r -p "$1 [y/N] " a || return 1
    fi
    case "$a" in y|Y|yes|YES) return 0 ;; *) return 1 ;; esac
}

# -------------------------------------------------------------------- card
if [ -z "$CARD" ]; then
    CARD="$(awk '/^ *[0-9]+ \[/ && /ua_apollo/ {print $1; exit}' /proc/asound/cards 2>/dev/null)"
fi
if [ -z "$CARD" ] && [ -f "$HERE/../scripts/apollo-hostcheck.sh" ]; then
    # No card: say whether a locked-down Thunderbolt port or Secure Boot is why.
    # shellcheck source=scripts/apollo-hostcheck.sh
    . "$HERE/../scripts/apollo-hostcheck.sh"
    hostcheck_thunderbolt
    hostcheck_secureboot
fi
[ -n "$CARD" ] || die "no ua_apollo ALSA card in /proc/asound/cards (is the driver loaded and the Apollo on?). Use --card N to override."
HW="hw:$CARD,$DEVICE"
IO_MAP="${APOLLO_IO_MAP:-/proc/asound/card$CARD/io_map}"   # env override: for testing
[ -r "$IO_MAP" ] || die "$IO_MAP missing - this test needs the v2 driver"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/apollo-selftest.XXXXXX")" || die "mktemp failed"

# ---------------------------------------------------------------- PipeWire
PW_UNITS="pipewire.socket pipewire-pulse.socket pipewire.service pipewire-pulse.service wireplumber.service"
PW_STOPPED=""

restore_pipewire() {
    [ -n "$PW_STOPPED" ] || return 0
    echo "Restoring PipeWire units: $PW_STOPPED"
    # sockets first, so socket-activated services come back the usual way
    # shellcheck disable=SC2086
    systemctl --user start $PW_STOPPED 2>&1 | sed 's/^/  /'
    PW_STOPPED=""
}

cleanup() {
    restore_pipewire
    if [ "$KEEP" = 1 ]; then
        echo "Temporary files kept in $WORK"
    else
        rm -rf "$WORK"
    fi
}
trap cleanup EXIT
trap 'echo; echo "Interrupted."; kill $(jobs -p) 2>/dev/null; exit 130' INT TERM

echo "Open Apollo duplex self-test, ALSA card $CARD ($HW)"
echo "Turn your speakers / monitor level DOWN before continuing: a -12 dBFS tone"
echo "will be played on the MON outputs."
ask "Continue?" || { echo "Nothing was done."; exit 0; }

if have systemctl && systemctl --user show-environment >/dev/null 2>&1; then
    active=""
    for u in $PW_UNITS; do
        systemctl --user is-active --quiet "$u" 2>/dev/null && active="$active $u"
    done
    active="${active# }"
    if [ -n "$active" ]; then
        echo
        echo "PipeWire is running ($active). It holds the card open, which can make"
        echo "aplay/arecord fail with 'busy' or hide driver problems."
        if ask "Stop these user units for the test (they are restored at the end)?"; then
            # shellcheck disable=SC2086
            if systemctl --user stop $active; then
                PW_STOPPED="$active"
                sleep 1
            else
                echo "Could not stop them; continuing with PipeWire running."
            fi
        else
            echo "Leaving PipeWire running; 'busy' errors below are then expected."
        fi
    else
        echo "No user PipeWire units active."
    fi
fi

# ------------------------------------------------------------------ helpers
# highest channel count the PCM accepts, from aplay/arecord --dump-hw-params
max_channels() {   # max_channels aplay|arecord
    "$1" -D "$HW" -t raw -f S32_LE --dump-hw-params -d 1 /dev/zero 2>&1 \
        | awk '/^CHANNELS:/ {
            gsub(/[\[\]]/, ""); n = $NF; print n; exit }'
}

io_count() {   # io_count FILE playback|record
    awk -v d="$2" '$1 == d":" && $3 == "ALSA" {print $2; exit}' "$1" 2>/dev/null
}

uniq_words() { printf '%s\n' "$@" | awk 'NF && !seen[$0]++' | tr '\n' ' '; }

# ---------------------------------------------------------------------- run
cp "$IO_MAP" "$WORK/io_map.initial" 2>/dev/null
MAXP="$(max_channels aplay)"
MAXR="$(max_channels arecord)"
P_INIT="$(io_count "$WORK/io_map.initial" playback)"
R_INIT="$(io_count "$WORK/io_map.initial" record)"
echo
echo "Card reports: playback max ${MAXP:-?} ch, record max ${MAXR:-?} ch;"
echo "io_map: playback ${P_INIT:-?} ch, record ${R_INIT:-?} ch."

RESULTS="$WORK/results.tsv"
DETAILS="$WORK/details.txt"
: > "$RESULTS"
: > "$DETAILS"

record_result() {   # rate pch rch mon verdict reason
    printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$@" >> "$RESULTS"
}

run_rate() {
    local rate="$1" pch rch combos_err="" tone cap iomap mon out verdict reason
    local P_CANDS R_CANDS
    # shellcheck disable=SC2046
    P_CANDS="$(uniq_words "$MAXP" "$P_INIT" 24)"
    # shellcheck disable=SC2046
    R_CANDS="$(uniq_words "$MAXR" "$R_INIT" 22)"
    cap="$WORK/cap-$rate.wav"
    iomap="$WORK/io_map.$rate"
    for pch in $P_CANDS; do
        tone="$WORK/tone-$rate-$pch.wav"
        python3 "$WAVCHECK" gen --out "$tone" --rate "$rate" --channels "$pch" \
            --seconds "$TONE_SECONDS" || { combos_err="tone generation failed"; continue; }
        for rch in $R_CANDS; do
            rm -f "$cap" "$iomap"
            timeout $((TONE_SECONDS + 15)) arecord -q -D "$HW" -t wav -f S32_LE \
                -r "$rate" -c "$rch" -d "$CAP_SECONDS" "$cap" \
                2> "$WORK/arecord.err" &
            local rpid=$!
            sleep 0.5
            timeout $((TONE_SECONDS + 15)) aplay -q -D "$HW" -t wav "$tone" \
                2> "$WORK/aplay.err" &
            local ppid=$!
            sleep 1.5
            cp "$IO_MAP" "$iomap" 2>/dev/null     # layout while streaming
            local rrc=0 prc=0
            wait "$rpid" || rrc=$?
            wait "$ppid" || prc=$?
            if [ "$rrc" = 0 ] && [ "$prc" = 0 ]; then
                [ -s "$iomap" ] || cp "$WORK/io_map.initial" "$iomap"
                mon="$(python3 "$WAVCHECK" mapids --io-map "$iomap" \
                        --direction record --ids 0x0901,0x0902 2>&1)" || {
                    record_result "$rate" "$pch" "$rch" "-" ERROR "MON not in io_map: $mon"
                    return
                }
                out="$(python3 "$WAVCHECK" classify "$cap" --mon "$mon" -v)"
                verdict="$(printf '%s\n' "$out" | head -1 | cut -f1)"
                reason="$(printf '%s\n' "$out" | head -1 | cut -f2-)"
                record_result "$rate" "$pch" "$rch" "$mon" "$verdict" "$reason"
                {
                    echo "--- $rate Hz, play $pch ch, rec $rch ch, MON=$mon: $verdict"
                    printf '%s\n' "$out" | tail -n +2
                    echo
                } >> "$DETAILS"
                return
            fi
            combos_err="aplay(${pch}ch) rc=$prc: $(head -c 200 "$WORK/aplay.err" | tr '\n' ' ')"
            combos_err="$combos_err arecord(${rch}ch) rc=$rrc: $(head -c 200 "$WORK/arecord.err" | tr '\n' ' ')"
        done
    done
    if printf '%s' "$combos_err" | grep -q -i -E 'rate|not supported|invalid argument'; then
        record_result "$rate" "-" "-" "-" SKIP "rate not accepted: $combos_err"
    else
        record_result "$rate" "-" "-" "-" ERROR "$combos_err"
    fi
}

echo
ALL=0
for rate in $RATES; do
    if [ "$ALL" = 0 ]; then
        if [ "$ASSUME_YES" = 1 ]; then
            :
        else
            if [ -r /dev/tty ]; then
                read -r -p "Test $rate Hz? [y]es / [n]o skip / [a]ll remaining / [q]uit " a < /dev/tty || a=q
            else
                read -r -p "Test $rate Hz? [y]es / [n]o skip / [a]ll remaining / [q]uit " a || a=q
            fi
            case "$a" in
                a|A) ALL=1 ;;
                y|Y|yes) ;;
                q|Q) echo "Stopping."; break ;;
                *) record_result "$rate" "-" "-" "-" SKIP "skipped by user"; continue ;;
            esac
        fi
    fi
    echo "== $rate Hz: playing ${TONE_SECONDS}s tone, recording ${CAP_SECONDS}s ..."
    run_rate "$rate"
    tail -1 "$RESULTS" | awk -F'\t' '{printf "   -> %s  %s\n", $5, $6}'
done

# ------------------------------------------------------------------ summary
SUMMARY="$WORK/summary.txt"
{
    echo "Duplex self-test, ALSA card $CARD, $(date -u +%Y-%m-%dT%H:%M:%SZ)"
    echo "tone: 997 Hz, -12 dBFS, S32_LE, playback ch 1/2 (0-based 0,1);" \
         "capture ${CAP_SECONDS}s; MON = record channels with ID 0x0901/0x0902"
    printf '%-8s %-5s %-5s %-7s %-7s %s\n' RATE PLAY REC MON VERDICT DETAIL
    awk -F'\t' '{printf "%-8s %-5s %-5s %-7s %-7s %s\n", $1, $2, $3, $4, $5, $6}' "$RESULTS"
    if [ -s "$DETAILS" ]; then
        echo
        echo "Per-channel statistics (* = MON channel; stale% = share of non-zero"
        echo "samples equal to the sample 8192 frames earlier):"
        cat "$DETAILS"
    fi
    echo
    echo "io_map at the end of the run:"
    cp "$IO_MAP" "$WORK/io_map.final" 2>/dev/null && cat "$WORK/io_map.final"
} > "$SUMMARY"

echo
echo "================ summary ================"
sed -n '1,/^$/p' "$SUMMARY"

if [ -z "$REPORT" ]; then
    # names carry a timestamp, so the last glob match is the newest
    for f in apollo-report-*.txt; do [ -f "$f" ] && REPORT="$f"; done
fi
if [ -z "$REPORT" ]; then
    REPORT="apollo-selftest-$(date +%Y%m%d-%H%M%S).txt"
fi
{
    printf '\n===== duplex self-test =====\n'
    cat "$SUMMARY"
} >> "$REPORT"
echo "Summary appended to $REPORT"
