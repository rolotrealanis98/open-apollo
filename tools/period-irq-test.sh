#!/bin/bash
# period-irq-test.sh - hardware validation of the ua_apollo period_irq mode
#
# period_irq=1 takes PCM period wakeups from the Apollo's period-timer
# interrupt (threaded IRQ handler) instead of the 1 ms SAMPLE_POS poll
# (kext audit gap D2).  This script measures both modes on real hardware
# and says whether period_irq=1 is ready to become the default.
#
# Run it as your normal user from a checkout, driver loaded, Apollo on.  It
# uses sudo for the module parameter writes (/sys/module/ua_apollo/...)
# and, if needed, for dmesg.  It never loads or unloads the module.
#
#   tools/period-irq-test.sh alsa       PipeWire STOPPED (the script offers
#                                       to stop it and restores it at exit)
#   tools/period-irq-test.sh pipewire   PipeWire RUNNING with the Apollo
#   tools/period-irq-test.sh all        alsa, then pipewire
#
# alsa:
#   1. duplex matrix: arecord + aplay on hw:CARD,0 at each rate (48/96/192k)
#      and period size (256/512/1024), --buffer-size 8192, for each mode
#      (period_irq=0 and 1): xruns (aplay/arecord -v), exit codes, the
#      owner of each direction, period wakeups per second (= rate/period),
#      period-timer interrupts per second (none with period_irq=0: the
#      timer is left unprogrammed and masked, as on macOS; with
#      period_irq=1 one per period, a second one for the same boundary
#      counted apart as timer_dups), the timer phase against SAMPLE_POS,
#      end-of-buffer interrupts per second (= rate/8192), /proc/interrupts
#      rate, dmesg
#   2. 192 -> 48 kHz with period_irq=1: a control run with
#      period_irq_settle=0 (its FAILs are INFO), then with the default 1;
#      the driver must not fall back with settle on
#   2b. mixed periods (period_irq=1): record period 1024 then play 256 (play
#      must fall back to the hrtimer), and 256 then 1024 (both on the IRQ)
#   Every ALSA run saves /proc/asound/cardN/period_irq_trace (the first 256
#   timer events of its last two transport sessions) as trace.txt when the
#   driver has period_irq_trace.
#   3. runtime switch: period_irq flipped while a capture stream runs
#   4. start/stop stress, each mode: N duplex open/close cycles; capture
#      continuous while playback cycles N times; and the reverse
# pipewire:
#   5. for each clock.force-quantum (256/512/1024): pw-play silence to the
#      Apollo output node and pw-record from its input node for --pw-seconds
#      (default 300); pw-top ERR delta on the Apollo nodes, owners,
#      interrupt rate, dmesg
#
# Not covered: system suspend (titan's S3 hangs without the driver too); the
# code cancels the watchdog and the hrtimer on suspend, untested.
#
# Logs go to --logdir (default /var/tmp/period-irq-<date>); summary.txt
# there has every PASS/FAIL/WARN line.
#
# Criteria for making period_irq=1 the default (all must hold):
#   - alsa matrix, mixed, switch and stress: no FAIL in either mode
#     (0 xruns, exit codes 0, expected owners, wakeups within 1% of
#     rate/period from the owning path and none from the other, no
#     interrupt storm in /proc/interrupts, no fallbacks/stops, no new kernel
#     errors; period_irq=0: no timer interrupts at all; period_irq=1: timer
#     interrupts other than same-boundary duplicates within 0.5% of
#     rate/timer_frames, EOB within 3% of rate/buffer (a WARN with
#     period_irq=0), EOB at the ring start).  A correctable AER reported
#     for another device (the Thunderbolt ports) is INFO, not a failure.
#   - pipewire: 0 new ERR on the Apollo nodes at every quantum, 5 min each
#   - WARN lines understood: a timer phase mean above 1/8 of the interval
#     means late wakeups, not lost ones (the driver moves the stream to the
#     hrtimer above 1/4); duplicates above 10% cost interrupts, not
#     wakeups
#   - then: the freeze reporters (#33, #38, #42) run period_irq=1 on their
#     machines; that part is outside this script
#
# Usage:
#   tools/period-irq-test.sh alsa|pipewire|all [--card N] [--seconds S]
#       [--rates "48000 96000 192000"] [--periods "256 512 1024"]
#       [--modes "0 1"] [--cycles N] [--pw-seconds S]
#       [--quanta "256 512 1024"] [--pw-modes "1"] [--logdir DIR] [--yes]

set -u

WHAT="${1:-}"
[ $# -gt 0 ] && shift
CARD=""
SECONDS_RUN=30
RATES="48000 96000 192000"
PERIODS="256 512 1024"
MODES="0 1"
CYCLES=100
PW_SECONDS=300
QUANTA="256 512 1024"
PW_MODES="1"
PW_RATE=48000
LOGDIR=""
ASSUME_YES=0

usage() {
    sed -n '2,/^$/p' "$0" | sed 's/^# \{0,1\}//'
    exit "${1:-0}"
}

while [ $# -gt 0 ]; do
    case "$1" in
        --card)       CARD="${2:?}"; shift 2 ;;
        --seconds)    SECONDS_RUN="${2:?}"; shift 2 ;;
        --rates)      RATES="${2:?}"; shift 2 ;;
        --periods)    PERIODS="${2:?}"; shift 2 ;;
        --modes)      MODES="${2:?}"; shift 2 ;;
        --cycles)     CYCLES="${2:?}"; shift 2 ;;
        --pw-seconds) PW_SECONDS="${2:?}"; shift 2 ;;
        --quanta)     QUANTA="${2:?}"; shift 2 ;;
        --pw-modes)   PW_MODES="${2:?}"; shift 2 ;;
        --logdir)     LOGDIR="${2:?}"; shift 2 ;;
        --yes)        ASSUME_YES=1; shift ;;
        -h|--help)    usage 0 ;;
        *) echo "unknown option: $1" >&2; usage 1 ;;
    esac
done
case "$WHAT" in alsa|pipewire|all) ;; -h|--help) usage 0 ;; *) usage 1 ;; esac
case "$SECONDS_RUN" in ''|*[!0-9]*) echo "--seconds: a whole number" >&2; exit 1 ;; esac
[ "$SECONDS_RUN" -ge 20 ] || { echo "--seconds must be at least 20" >&2; exit 1; }

have() { command -v "$1" >/dev/null 2>&1; }
die()  { echo "error: $*" >&2; exit 1; }

ask() {   # ask "question" -> 0 yes, 1 no
    [ "$ASSUME_YES" = 1 ] && { echo "$1 [y/N] y (--yes)"; return 0; }
    local a
    read -r -p "$1 [y/N] " a < /dev/tty || return 1
    case "$a" in y|Y|yes|YES) return 0 ;; *) return 1 ;; esac
}

# ------------------------------------------------------------------ setup
# Paths: environment overrides for running the script against a mock
ASOUND="${PERIOD_IRQ_ASOUND:-/proc/asound}"
INTERRUPTS="${PERIOD_IRQ_INTERRUPTS:-/proc/interrupts}"
PARAMS="${PERIOD_IRQ_PARAMS:-/sys/module/ua_apollo/parameters}"
if [ -z "$CARD" ]; then
    CARD="$(awk '/^ *[0-9]+ \[/ && /ua_apollo/ {print $1; exit}' "$ASOUND/cards" 2>/dev/null)"
fi
[ -n "$CARD" ] || die "no ua_apollo card in $ASOUND/cards; use --card N"
HW="hw:$CARD,0"
CARDDIR="$ASOUND/card$CARD"
PROC="$CARDDIR/period_irq"
[ -r "$PROC" ] && [ -e "$PARAMS/period_irq" ] ||
    die "this driver has no period_irq ($PROC, $PARAMS/period_irq missing)"

[ -n "$LOGDIR" ] || LOGDIR="/var/tmp/period-irq-$(date +%Y%m%d-%H%M%S)"
mkdir -p "$LOGDIR" || die "cannot create $LOGDIR"
SUMMARY="$LOGDIR/summary.txt"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/period-irq.XXXXXX")" || die "mktemp failed"

NPASS=0; NFAIL=0; NWARN=0
SOFT=0   # 1: a control run expected to fail, its FAILs are INFO
log() { echo "$*" | tee -a "$SUMMARY"; }
result() {   # result PASS|FAIL|WARN|INFO name detail
    if [ "$SOFT" = 1 ]; then
        case "$1" in
            FAIL|WARN) set -- INFO "$2" "(control) $3" ;;
            PASS) set -- INFO "$2" "(control) passed: $3" ;;
        esac
    fi
    case "$1" in
        PASS) NPASS=$((NPASS + 1)) ;;
        FAIL) NFAIL=$((NFAIL + 1)) ;;
        WARN) NWARN=$((NWARN + 1)) ;;
    esac
    log "$(printf '%-4s %-28s %s' "$1" "$2" "$3")"
}

if dmesg >/dev/null 2>&1; then DMESG=(dmesg); else DMESG=(sudo dmesg); fi
echo "sudo is used for the module parameter writes (and dmesg if restricted)."
sudo -n true 2>/dev/null || sudo -v || die "sudo needed for $PARAMS writes"
FWV2="$(awk '$1 == "fw_v2" { print $2 }' "$PROC")"

ORIG_IRQ="$(cat "$PARAMS/period_irq")"
ORIG_SETTLE="$(cat "$PARAMS/period_irq_settle" 2>/dev/null)"
ORIG_TRACE="$(cat "$PARAMS/period_irq_trace" 2>/dev/null)"

set_param() {   # set_param name value
    echo "$2" | sudo tee "$PARAMS/$1" >/dev/null ||
        die "cannot write $PARAMS/$1"
}

# ------------------------------------------------------------- PipeWire
PW_UNITS="pipewire.socket pipewire-pulse.socket pipewire.service pipewire-pulse.service wireplumber.service"
PW_STOPPED=""
restore_pipewire() {
    [ -n "$PW_STOPPED" ] || return 0
    echo "Restoring PipeWire units: $PW_STOPPED"
    # shellcheck disable=SC2086
    systemctl --user start $PW_STOPPED
    PW_STOPPED=""
}

FORCED_QUANTUM=0
cleanup() {
    jobs -p | xargs -r kill 2>/dev/null
    wait 2>/dev/null
    [ "$FORCED_QUANTUM" = 1 ] && pw-metadata -n settings 0 clock.force-quantum 0 >/dev/null 2>&1
    set_param period_irq "$ORIG_IRQ"
    [ -n "$ORIG_SETTLE" ] && set_param period_irq_settle "$ORIG_SETTLE"
    [ -n "$ORIG_TRACE" ] && set_param period_irq_trace "$ORIG_TRACE"
    restore_pipewire
    rm -rf "$WORK"
    echo "Logs: $LOGDIR"
}
trap cleanup EXIT
trap 'echo; echo "Interrupted."; exit 130' INT TERM

# ---------------------------------------------------------------- probes
declare -A S0 S1 A B

irq_total() {   # all CPUs, the ua_apollo line(s) of /proc/interrupts
    awk 'NR == 1 { n = NF; next }
         /ua_apollo/ { for (i = 2; i <= n + 1; i++) s += $i }
         END { print s + 0 }' "$INTERRUPTS"
}

snap() {   # snap ARRAY: period_irq proc + time + interrupt count
    local -n _s=$1
    local k v
    _s=()
    # shellcheck disable=SC2004  # associative: the key needs the $
    while read -r k v; do _s[$k]=$v; done < "$PROC"
    _s["t"]=$(date +%s.%N)
    _s["irq"]=$(irq_total)
}

calc() { awk "BEGIN { printf \"%.3f\", $* }"; }
per_s() {   # per_s KEY: rate of KEY between snapshots A and B
    calc "(${B[$1]:-0} - ${A[$1]:-0}) / (${B[t]} - ${A[t]})"
}
within() {   # within MEASURED EXPECTED TOLERANCE(fraction)
    awk -v m="$1" -v e="$2" -v t="$3" \
        'BEGIN { exit !(e > 0 && m >= e * (1 - t) && m <= e * (1 + t)) }'
}

KMARK=""
kmsg_mark() { KMARK="$("${DMESG[@]}" 2>/dev/null | tail -n 1)"; }
kmsg_new() {
    "${DMESG[@]}" 2>/dev/null |
        awk -v m="$KMARK" 'm == "" || found { print } $0 == m { found = 1 }'
}
KBAD='period_irq:|period timer:|BAR0|AER|refusing opens|transport stalled|unreachable|WARNING|BUG|Oops|Call Trace|hung task|blocked for more than'
# PCI address of the Apollo, to tell its AER reports from other devices
SYSCARD="${PERIOD_IRQ_SYSCARD:-/sys/class/sound/card$CARD}"
APOLLO_BDF="$(readlink -f "$SYSCARD/device" 2>/dev/null)"
APOLLO_BDF="${APOLLO_BDF##*/}"
check_kmsg() {   # check_kmsg NAME DIR
    kmsg_new > "$2/dmesg.txt"
    grep -E "$KBAD" "$2/dmesg.txt" > "$2/dmesg.all"
    # A correctable AER that names another device (e.g. a Thunderbolt
    # port) is not the driver's, and the settle restart after a rate change
    # ("clock locked at rate index", period_irq_settle) is expected: INFO.
    # Anything else, or an AER naming the Apollo: FAIL.
    : > "$2/dmesg.info"
    : > "$2/dmesg.bad"
    awk -v bdf="${APOLLO_BDF:-ua_apollo}" -v info="$2/dmesg.info" -v bad="$2/dmesg.bad" '
        /AER: Correctable/ && index($0, bdf) == 0 { print > info; next }
        /period_irq: clock locked at rate index/ { print > info; next }
        { print > bad }' "$2/dmesg.all"
    if [ -s "$2/dmesg.info" ]; then
        result INFO "$1" "kernel (info): $(head -n 1 "$2/dmesg.info")"
    fi
    if [ -s "$2/dmesg.bad" ]; then
        result FAIL "$1" "kernel: $(head -n 1 "$2/dmesg.bad")"
    fi
}

pcm_closed() {
    grep -q closed "$CARDDIR/pcm0p/sub0/status" 2>/dev/null &&
        grep -q closed "$CARDDIR/pcm0c/sub0/status" 2>/dev/null
}

wait_idle() {   # both directions closed, transport stopped (up to $1 s)
    local i
    for ((i = 0; i < ${1:-10} * 10; i++)); do
        if pcm_closed && grep -q '^transport 0' "$PROC"; then return 0; fi
        sleep 0.1
    done
    return 1
}

wait_running() {   # both owners not idle (up to $1 s)
    local i
    for ((i = 0; i < ${1:-15} * 10; i++)); do
        if ! grep -q '^owner_.* idle' "$PROC"; then return 0; fi
        sleep 0.1
    done
    return 1
}

xruns() { grep -c -E 'underrun|overrun' "$1" 2>/dev/null; }

max_channels() {   # max_channels aplay|arecord
    timeout 10 "$1" -D "$HW" -t raw -f S32_LE --dump-hw-params -d 1 /dev/zero 2>&1 |
        awk '/^CHANNELS:/ { gsub(/[][]/, ""); print $NF; exit }'
}

HAVE_SAMPLES=0
aplay --help 2>&1 | grep -q -- '--samples' && HAVE_SAMPLES=1

short_len() {   # aplay/arecord options for a ~0.25 s stream at rate $1
    if [ "$HAVE_SAMPLES" = 1 ]; then echo "--samples=$(($1 / 4))"; else echo "-d 1"; fi
}

record_env() {
    {
        echo "kernel: $(uname -r)"
        echo "card: $CARD ($HW)"
        for p in "$PARAMS"/*; do echo "param $(basename "$p")=$(cat "$p" 2>/dev/null)"; done
        echo "irq thread:"
        # shellcheck disable=SC2009  # want the scheduling class and priority
        ps -eLo pid,cls,rtprio,comm 2>/dev/null | grep -E 'irq/[0-9]+-ua_' || echo "  (none found)"
        grep ua_apollo "$INTERRUPTS"
        cat "$PROC"
    } > "$LOGDIR/env-$1.txt" 2>&1
}

# Owner the driver picks for a duplex run with the same period both ways
# (ua_period_irq_usable(): fw_v2, timer interval >= 1 ms dividing the
# 8192-frame ring and the period, at least four periods in the ring)
expect_owner() {   # expect_owner MODE RATE PERIOD
    if [ "$1" = 1 ] && [ "$FWV2" = 1 ] && [ $(($3 * 1000)) -ge "$2" ] &&
       [ $((8192 % $3)) = 0 ] && [ $(($3 * 4)) -le 8192 ]; then
        echo irq
    else
        echo hrtimer
    fi
}

# ------------------------------------------------- one duplex run (ALSA)
# run_duplex NAME MODE RATE PLAY_PERIOD REC_PERIOD SECONDS EXP_PLAY EXP_REC
# The record side starts first, so its period sets TIMER_CFG.
run_duplex() {
    local name=$1 mode=$2 rate=$3 pp=$4 rp=$5 secs=$6 ep=$7 er=$8
    local d="$LOGDIR/$name" rpid ppid rrc prc left dir own path per exp m
    local bad=0 tsev=FAIL
    [ "$mode" = 0 ] && tsev=WARN   # nothing uses the timer interrupt then
    A=(); B=()
    mkdir -p "$d"
    set_param period_irq "$mode"
    if ! wait_idle 10; then
        result FAIL "$name" "card busy before the run"
        return
    fi
    kmsg_mark
    snap S0
    arecord -D "$HW" -t raw -f S32_LE -r "$rate" -c "$RCH" \
        --period-size="$rp" --buffer-size=8192 -d "$secs" -v \
        --dump-hw-params /dev/null > "$d/arecord.log" 2>&1 &
    rpid=$!
    sleep 1
    aplay -D "$HW" -t raw -f S32_LE -r "$rate" -c "$PCH" \
        --period-size="$pp" --buffer-size=8192 -d "$((secs - 1))" -v \
        --dump-hw-params /dev/zero > "$d/aplay.log" 2>&1 &
    ppid=$!

    if wait_running 15; then
        sleep 2
        snap A
        cat "$PROC" > "$d/proc.mid"
        cat "$CARDDIR/pcm0p/sub0/hw_params" > "$d/hw_params.play" 2>/dev/null
        cat "$CARDDIR/pcm0c/sub0/hw_params" > "$d/hw_params.rec" 2>/dev/null
        left=$(calc "$secs - 4 - ($(date +%s.%N) - ${S0[t]})")
        awk -v l="$left" 'BEGIN { exit !(l > 5) }' && sleep "$left"
        snap B
    else
        result FAIL "$name" "streams did not start (see $d/*.log)"
        bad=1
    fi
    wait "$rpid"; rrc=$?
    wait "$ppid"; prc=$?
    snap S1
    cat "$PROC" > "$d/proc.end"
    [ -r "$CARDDIR/period_irq_trace" ] && cat "$CARDDIR/period_irq_trace" > "$d/trace.txt"
    check_kmsg "$name" "$d"
    [ "$bad" = 1 ] && return

    # exit codes, xruns
    if [ "$rrc" != 0 ] || [ "$prc" != 0 ]; then
        result FAIL "$name" "exit codes: arecord $rrc, aplay $prc"
        bad=1
    fi
    m="$(xruns "$d/aplay.log")/$(xruns "$d/arecord.log")"
    if [ "$m" != "0/0" ]; then
        result FAIL "$name" "xruns play/rec $m"
        bad=1
    fi

    # owners, and period wakeups per second from the owning path
    for dir in play rec; do
        own="${A[owner_$dir]:-}"
        if [ "$dir" = play ]; then exp=$ep; per=$pp; else exp=$er; per=$rp; fi
        if [ "$own" != "$exp" ]; then
            result FAIL "$name" "owner_$dir=$own, expected $exp"
            bad=1
            continue
        fi
        if [ "$own" = irq ]; then path=irq; else path=hrtimer; fi
        m=$(per_s "elapsed_${path}_$dir")
        if ! within "$m" "$(calc "$rate / $per")" 0.01; then
            result FAIL "$name" "$dir wakeups $m/s via $path, expected $(calc "$rate / $per")/s"
            bad=1
        fi
        # the other path must not serve it
        if [ "$path" = irq ]; then path=hrtimer; else path=irq; fi
        m=$(per_s "elapsed_${path}_$dir")
        if [ "$m" != "0.000" ]; then
            result FAIL "$name" "$dir also woken by $path ($m/s): double period_elapsed"
            bad=1
        fi
    done

    # interrupts
    local tf=${A[timer_frames]:-0} buf=${A[buffer_frames]:-8192}
    local ti ei it ex tr td
    ti=$(per_s timer_irqs)
    td=$(per_s timer_dups)
    tr=$(calc "$ti - $td")   # one per boundary
    ei=$(per_s eob_irqs)
    it=$(per_s irq)
    if [ "$mode" = 0 ]; then
        # period_irq=0: no timer programmed, vector 0x46 masked (macOS state)
        if [ "$tf" != 0 ] || ! awk -v m="$ti" 'BEGIN { exit !(m < 0.5) }'; then
            result FAIL "$name" "period timer active with period_irq=0: $ti/s, timer_frames $tf"
            bad=1
        fi
        ex=$(calc "$rate / $buf")
    else
        [ "$tf" -gt 0 ] || tf=$pp
        if ! within "$tr" "$(calc "$rate / $tf")" 0.005; then
            result "$tsev" "$name" "timer interrupts $tr/s without duplicates ($ti/s in all), expected $(calc "$rate / $tf")/s (timer_frames $tf, TIMER_CFG ${A[timer_cfg]:-?})"
            [ "$tsev" = FAIL ] && bad=1
        fi
        if awk -v d="$td" 'BEGIN { exit !(d > 0) }'; then
            if awk -v d="$td" -v t="$ti" 'BEGIN { exit !(d > t * 0.1) }'; then
                m=WARN
            else
                m=INFO
            fi
            result "$m" "$name" "$td/s duplicate timer interrupts (same boundary) of $ti/s"
        fi
        ex=$(calc "$rate / $tf + $td + $rate / $buf")
    fi
    if ! within "$ei" "$(calc "$rate / $buf")" 0.03; then
        result "$tsev" "$name" "end-of-buffer interrupts $ei/s, expected $(calc "$rate / $buf")/s"
        [ "$tsev" = FAIL ] && bad=1
    fi
    if ! awk -v m="$it" -v e="$ex" 'BEGIN { exit !(m <= e * 1.1 + 20) }'; then
        result FAIL "$name" "/proc/interrupts $it/s, expected about $ex/s"
        bad=1
    fi
    for m in eob_pos_odd fallbacks stops; do
        if [ "${S1[$m]:-}" != "${S0[$m]:-}" ]; then
            result FAIL "$name" "$m ${S0[$m]:-} -> ${S1[$m]:-}"
            bad=1
        fi
    done
    if [ "$mode" = 1 ] && [ "$ep$er" != hrtimerhrtimer ] &&
       ! awk -v p="${S1[phase_mean]:-0}" -v t="$tf" 'BEGIN { exit !(p <= t / 8) }'; then
        result WARN "$name" "timer phase mean ${S1[phase_mean]:-?} max ${S1[phase_max]:-?} of $tf frames (wakeups late)"
    fi
    [ "$bad" = 0 ] && result PASS "$name" \
        "owners $ep/$er, wakeups ok, timer $ti/s (dups $td) eob $ei/s irq $it/s (expected $ex), phase ${S1[phase_mean]:-0}/$tf"
}

# ------------------------------------------- 192 -> 48 kHz rate change
# The first period_irq=1 transport after 192 -> 48 kHz ran its timer fast
# and off the period boundaries on titan (2026-10-11).  Control run with
# period_irq_settle=0 (reproduces it; its FAILs are INFO), then the fix
# with period_irq_settle=1.  Both leave trace.txt with the timer events.
rate_down_check() {
    local settle
    log ""
    log "== 192 -> 48 kHz with period_irq=1 (settle off: control, then on)"
    if [ "$FWV2" != 1 ] || [ -z "$ORIG_SETTLE" ]; then
        result INFO rate-down "skipped: no period_irq_settle in this driver or no fw_v2"
        return
    fi
    for settle in 0 1; do
        set_param period_irq_settle "$settle"
        run_duplex "down-pre-s$settle" 1 192000 256 256 20 irq irq
        [ "$settle" = 0 ] && SOFT=1
        run_duplex "down-s$settle" 1 48000 256 256 "$SECONDS_RUN" irq irq
        SOFT=0
        log "     settle=$settle: settles ${S0[settles]:-?} -> ${S1[settles]:-?}, fallbacks ${S0[fallbacks]:-?} -> ${S1[fallbacks]:-?}"
    done
    set_param period_irq_settle 1
}

# ------------------------------------------------------ runtime switch
switch_check() {
    local d="$LOGDIR/switch" rpid rc i bad=0 own
    mkdir -p "$d"
    log ""
    log "== runtime switch: period_irq flipped under a running capture stream"
    [ "$FWV2" = 1 ] || { result INFO switch "skipped: no 64-vector interrupt block (fw_v2=0)"; return; }
    set_param period_irq 1
    wait_idle 10 || { result FAIL switch "card busy"; return; }
    kmsg_mark
    snap S0
    arecord -D "$HW" -t raw -f S32_LE -r 48000 -c "$RCH" --period-size=512 \
        --buffer-size=8192 -d 40 -v /dev/null > "$d/arecord.log" 2>&1 &
    rpid=$!
    sleep 3
    [ "$(awk '$1 == "owner_rec" { print $2 }' "$PROC")" = irq ] ||
        { result FAIL switch "capture not on the IRQ with period_irq=1"; bad=1; }
    for mode in 0 1; do
        set_param period_irq "$mode"
        for i in 1 2 3 4 5; do
            aplay -D "$HW" -t raw -f S32_LE -r 48000 -c "$PCH" --period-size=512 \
                --buffer-size=8192 -d 2 /dev/zero >> "$d/aplay-$mode.log" 2>&1 &
            sleep 1
            own="$(awk '$1 == "owner_play" { print $2 } $1 == "owner_rec" { r = $2 } END { print r }' "$PROC" | tr '\n' /)"
            wait $! || { result FAIL switch "aplay failed (period_irq=$mode)"; bad=1; }
            case "$mode:$own" in
                0:hrtimer/irq/|1:irq/irq/) ;;
                *) result FAIL switch "period_irq=$mode: owners play/rec $own"; bad=1 ;;
            esac
        done
    done
    wait "$rpid"; rc=$?
    snap S1
    check_kmsg switch "$d"
    [ "$rc" = 0 ] || { result FAIL switch "arecord exit $rc"; bad=1; }
    [ "$(xruns "$d/arecord.log")" = 0 ] || { result FAIL switch "capture overruns"; bad=1; }
    [ "$(cat "$d"/aplay-*.log | grep -c -E 'underrun|overrun')" = 0 ] ||
        { result FAIL switch "playback underruns"; bad=1; }
    [ "${S1[stops]:-}" = "${S0[stops]:-}" ] || { result FAIL switch "stops ${S0[stops]:-} -> ${S1[stops]:-}"; bad=1; }
    [ "$bad" = 0 ] && result PASS switch "running stream kept its owner, new streams followed the parameter"
}

# --------------------------------------------------------------- stress
stress() {   # stress MODE
    local mode=$1
    local d="$LOGDIR/stress-$mode" i fails=0 rpid ppid rc bad=0
    local len
    len=$(short_len 48000)
    mkdir -p "$d"
    log ""
    log "== start/stop stress, period_irq=$mode, $CYCLES cycles per pattern"
    set_param period_irq "$mode"
    wait_idle 10 || { result FAIL "stress-$mode" "card busy"; return; }
    kmsg_mark
    snap S0

    # 1: duplex open/close (the transport stops and restarts every cycle)
    for ((i = 1; i <= CYCLES; i++)); do
        # shellcheck disable=SC2086
        arecord -D "$HW" -t raw -f S32_LE -r 48000 -c "$RCH" --period-size=512 \
            --buffer-size=8192 $len /dev/null >> "$d/duplex-rec.log" 2>&1 &
        rpid=$!
        # shellcheck disable=SC2086
        aplay -D "$HW" -t raw -f S32_LE -r 48000 -c "$PCH" --period-size=512 \
            --buffer-size=8192 $len /dev/zero >> "$d/duplex-play.log" 2>&1
        rc=$?
        wait "$rpid" || fails=$((fails + 1))
        [ "$rc" = 0 ] || fails=$((fails + 1))
    done
    [ "$fails" = 0 ] || { result FAIL "stress-$mode" "duplex cycles: $fails failed opens/runs"; bad=1; }

    # 2 and 3: one direction continuous, the other cycling
    local cont
    for cont in rec play; do
        wait_idle 10
        fails=0
        if [ "$cont" = rec ]; then
            arecord -D "$HW" -t raw -f S32_LE -r 48000 -c "$RCH" --period-size=512 \
                --buffer-size=8192 -d $((CYCLES * 3 + 30)) -v /dev/null > "$d/cont-rec.log" 2>&1 &
        else
            aplay -D "$HW" -t raw -f S32_LE -r 48000 -c "$PCH" --period-size=512 \
                --buffer-size=8192 -d $((CYCLES * 3 + 30)) -v /dev/zero > "$d/cont-play.log" 2>&1 &
        fi
        ppid=$!
        sleep 2
        for ((i = 1; i <= CYCLES; i++)); do
            if [ "$cont" = rec ]; then
                # shellcheck disable=SC2086
                aplay -D "$HW" -t raw -f S32_LE -r 48000 -c "$PCH" --period-size=256 \
                    --buffer-size=8192 $len /dev/zero >> "$d/cycle-play.log" 2>&1 ||
                    fails=$((fails + 1))
            else
                # shellcheck disable=SC2086
                arecord -D "$HW" -t raw -f S32_LE -r 48000 -c "$RCH" --period-size=256 \
                    --buffer-size=8192 $len /dev/null >> "$d/cycle-rec.log" 2>&1 ||
                    fails=$((fails + 1))
            fi
        done
        kill -INT "$ppid" 2>/dev/null
        wait "$ppid"
        [ "$fails" = 0 ] || { result FAIL "stress-$mode" "$cont continuous: $fails cycling runs failed"; bad=1; }
        [ "$(xruns "$d/cont-$cont.log")" = 0 ] ||
            { result FAIL "stress-$mode" "$cont continuous: $(xruns "$d/cont-$cont.log") xruns while the other side cycled"; bad=1; }
    done
    wait_idle 10
    snap S1
    check_kmsg "stress-$mode" "$d"
    local m
    for m in fallbacks stops; do
        [ "${S1[$m]:-}" = "${S0[$m]:-}" ] || { result FAIL "stress-$mode" "$m ${S0[$m]:-} -> ${S1[$m]:-}"; bad=1; }
    done
    [ "$bad" = 0 ] && result PASS "stress-$mode" "3 x $CYCLES cycles clean"
}

# ------------------------------------------------------------- ALSA part
alsa_part() {
    local u active="" mode rate p e
    for c in aplay arecord timeout; do have "$c" || die "$c not found"; done
    log "== ALSA part, card $CARD, $(date)"
    if ! pcm_closed; then
        if have systemctl; then
            for u in $PW_UNITS; do
                systemctl --user is-active --quiet "$u" 2>/dev/null && active="$active $u"
            done
        fi
        active="${active# }"
        [ -n "$active" ] || die "hw:$CARD,0 is open by something other than PipeWire; close it first"
        echo "PipeWire holds the card ($active)."
        ask "Stop these user units for the ALSA part (restored at exit)?" ||
            die "the ALSA part needs PipeWire stopped"
        # shellcheck disable=SC2086
        systemctl --user stop $active || die "could not stop PipeWire"
        PW_STOPPED="$active"
        wait_idle 10 || die "hw:$CARD,0 still open after stopping PipeWire"
    fi
    PCH="$(max_channels aplay)"
    RCH="$(max_channels arecord)"
    [ -n "$PCH" ] && [ -n "$RCH" ] || die "could not read the channel counts from --dump-hw-params"
    log "     channels: play $PCH, record $RCH"
    [ -n "$ORIG_TRACE" ] && set_param period_irq_trace 1   # trace.txt per run
    record_env alsa

    log ""
    log "== duplex matrix (modes: $MODES; rates: $RATES; periods: $PERIODS; ${SECONDS_RUN}s each)"
    for mode in $MODES; do
        for rate in $RATES; do
            for p in $PERIODS; do
                e=$(expect_owner "$mode" "$rate" "$p")
                run_duplex "m$mode-$rate-p$p" "$mode" "$rate" "$p" "$p" "$SECONDS_RUN" "$e" "$e"
            done
        done
    done

    rate_down_check

    log ""
    log "== mixed periods (period_irq=1, 48000 Hz)"
    if [ "$FWV2" = 1 ]; then
        run_duplex "mix-r1024-p256" 1 48000 256 1024 "$SECONDS_RUN" hrtimer irq
        run_duplex "mix-r256-p1024" 1 48000 1024 256 "$SECONDS_RUN" irq irq
    else
        result INFO mixed "skipped: no 64-vector interrupt block (fw_v2=0), period_irq=1 stays on the hrtimer"
    fi
    switch_check
    for mode in $MODES; do stress "$mode"; done
    set_param period_irq "$ORIG_IRQ"
    restore_pipewire
}

# --------------------------------------------------------- PipeWire part
# Suspend both Apollo nodes (closes the PCM, stops the transport) and resume
# them, so the next start reads period_irq and reprograms TIMER_CFG.
force_reopen() {
    local rc
    have pactl || return 1
    pactl suspend-sink "$OUT_NODE" 1 && pactl suspend-source "$IN_NODE" 1 || return 1
    wait_idle 10
    rc=$?
    pactl suspend-sink "$OUT_NODE" 0
    pactl suspend-source "$IN_NODE" 0
    return "$rc"
}

pw_err() {   # "node err" per Apollo node from the second pw-top snapshot
    pw-top -b -n 2 2>/dev/null | tee -a "$1" |
        awk -v a="$OUT_NODE" -v b="$IN_NODE" '
            /^ *S +ID +QUANT/ { snap++; next }
            snap == 2 && (index($0, a) || index($0, b)) { print $NF, $9 }'
}

pw_part() {
    local mode q d ppid rpid i e0 e1 bad n err0 err1 own
    for c in pw-top pw-metadata pw-play pw-record pw-dump python3; do
        have "$c" || die "$c not found"
    done
    systemctl --user is-active --quiet pipewire.service ||
        die "PipeWire is not running (start it: systemctl --user start pipewire.socket wireplumber)"
    log ""
    log "== PipeWire part, card $CARD, $(date)"
    IN_NODE=""; OUT_NODE=""
    for i in $(seq 1 15); do   # PipeWire may just have been restarted
        eval "$(pw-dump 2>/dev/null | python3 -c '
import json, sys
inp = out = ""
for obj in json.load(sys.stdin):
    props = obj.get("info", {}).get("props", {})
    if props.get("alsa.driver_name") != "ua_apollo": continue
    name = props.get("node.name", "")
    if "input" in name and not inp: inp = name
    if "output" in name and not out: out = name
print(f"IN_NODE=\"{inp}\"; OUT_NODE=\"{out}\"")
' 2>/dev/null)"
        [ -n "$IN_NODE" ] && [ -n "$OUT_NODE" ] && break
        sleep 1
    done
    [ -n "$IN_NODE" ] && [ -n "$OUT_NODE" ] ||
        die "Apollo pro-audio nodes not found in pw-dump (pro-audio profile set? configs/pipewire/setup-apollo-io.sh)"
    log "     nodes: $OUT_NODE / $IN_NODE"
    python3 - "$WORK/silence.wav" "$((PW_SECONDS + 60))" "$PW_RATE" <<'EOF'
import sys, wave
path, secs, rate = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
with wave.open(path, "wb") as w:
    w.setnchannels(2); w.setsampwidth(2); w.setframerate(rate)
    chunk = bytes(4 * rate)
    for _ in range(secs):
        w.writeframes(chunk)
EOF
    record_env pipewire

    for mode in $PW_MODES; do
        set_param period_irq "$mode"
        for q in $QUANTA; do
            d="$LOGDIR/pw-m$mode-q$q"
            mkdir -p "$d"
            bad=0
            pw-metadata -n settings 0 clock.force-quantum "$q" >/dev/null
            FORCED_QUANTUM=1
            # the parameter applies at stream start: let the nodes suspend
            wait_idle 20 || force_reopen ||
                log "     note: Apollo PCM did not go idle; owners below show what applied"
            kmsg_mark
            snap S0
            pw-play --target "$OUT_NODE" "$WORK/silence.wav" > "$d/pw-play.log" 2>&1 &
            ppid=$!
            pw-record --target "$IN_NODE" --rate "$PW_RATE" --channels 2 --format s16 \
                "$WORK/cap.wav" > "$d/pw-record.log" 2>&1 &
            rpid=$!
            sleep 8
            snap A
            cat "$PROC" > "$d/proc.start"
            cat "$CARDDIR/pcm0p/sub0/hw_params" > "$d/hw_params.play" 2>/dev/null
            cat "$CARDDIR/pcm0c/sub0/hw_params" > "$d/hw_params.rec" 2>/dev/null
            e0="$(pw_err "$d/pw-top.log")"
            i=0
            while [ "$i" -lt "$PW_SECONDS" ]; do
                n=$((PW_SECONDS - i))
                [ "$n" -gt 60 ] && n=60
                sleep "$n"
                i=$((i + n))
                echo "--- ${i}s" >> "$d/err-progress.txt"
                pw_err "$d/pw-top.log" >> "$d/err-progress.txt"
            done
            e1="$(pw_err "$d/pw-top.log")"
            snap B
            cat "$PROC" > "$d/proc.end"
            kill "$ppid" "$rpid" 2>/dev/null
            wait "$ppid" "$rpid" 2>/dev/null
            rm -f "$WORK/cap.wav"
            snap S1
            check_kmsg "pw-m$mode-q$q" "$d"
            printf 'start:\n%s\nend:\n%s\n' "$e0" "$e1" > "$d/err.txt"

            for n in "$OUT_NODE" "$IN_NODE"; do
                err0=$(echo "$e0" | awk -v n="$n" '$1 == n { print $2; exit }')
                err1=$(echo "$e1" | awk -v n="$n" '$1 == n { print $2; exit }')
                if [ -z "$err0" ] || [ -z "$err1" ]; then
                    result FAIL "pw-m$mode-q$q" "$n not in pw-top (see $d/pw-top.log)"
                    bad=1
                elif [ "$err1" != "$err0" ]; then
                    result FAIL "pw-m$mode-q$q" "$n ERR $err0 -> $err1"
                    bad=1
                fi
            done
            own="${A[owner_play]:-}/${A[owner_rec]:-}"
            if [ "$mode" = 1 ] && [ "$FWV2" = 1 ] && [ "$own" != irq/irq ]; then
                result WARN "pw-m$mode-q$q" "owners $own with period_irq=1 (period $(awk '/period_size/ { print $2 }' "$d/hw_params.play" 2>/dev/null), timer_frames ${A[timer_frames]:-?})"
            fi
            for n in fallbacks stops eob_pos_odd; do
                [ "${S1[$n]:-}" = "${S0[$n]:-}" ] || { result FAIL "pw-m$mode-q$q" "$n ${S0[$n]:-} -> ${S1[$n]:-}"; bad=1; }
            done
            [ "$bad" = 0 ] && result PASS "pw-m$mode-q$q" \
                "${PW_SECONDS}s, ERR unchanged, owners $own, timer $(per_s timer_irqs)/s irq $(per_s irq)/s"
        done
    done
    pw-metadata -n settings 0 clock.force-quantum 0 >/dev/null
    FORCED_QUANTUM=0
}

# ----------------------------------------------------------------- main
log "period_irq validation, $(uname -r), card $CARD, logs in $LOGDIR"
case "$WHAT" in
    alsa)     alsa_part ;;
    pipewire) pw_part ;;
    all)      alsa_part; pw_part ;;
esac
log ""
log "== $NPASS PASS, $NFAIL FAIL, $NWARN WARN"
if [ "$NFAIL" = 0 ]; then
    log "No failures in this part. Default flip also needs: the other part, the TIMER_CFG check, freeze reporters."
else
    log "Failures: period_irq=1 must stay opt-in. Send $LOGDIR."
fi
[ "$NFAIL" = 0 ]   # exit status: 1 when anything failed
