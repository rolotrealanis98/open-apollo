#!/bin/bash
# Setup PipeWire virtual I/O devices for the Apollo (Thunderbolt)
# Reads the channel layout from the driver (/proc/asound/cardN/io_map),
# discovers the Apollo ALSA nodes dynamically, sets the pro-audio profile
# and generates a loopback config with the matching channel targets.
#
# Run after PipeWire/WirePlumber start:
#   bash ~/apollo-linux/configs/pipewire/setup-apollo-io.sh
#
# Can also be called from a systemd user service.
#
# Options:
#   --dry-run        print the loopbacks that would be created and exit
#                    (no PipeWire needed, nothing is written or restarted)
#   --io-map FILE    read the channel map from FILE instead of /proc
#                    (a saved copy of /proc/asound/cardN/io_map)
#   --card N         use ALSA card N (default: the card driven by ua_apollo)
#   --virtual        create Virtual In/Out loopbacks for every Virtual pair
#                    of the io_map, strips or not
#   --no-virtual     do not create them
#                    (default: a pair 1+2, 3+4, ... gets its loopbacks when the
#                    driver has an Input Strip live on both of its Virtual
#                    inputs, as /proc/asound/cardN/dsp_state lists them;
#                    without a strip a Virtual channel mixes and records
#                    nothing.  With --io-map and no --dsp-state: none)
#   --dsp-state FILE read the live strips from FILE instead of /proc
#                    (a saved copy of /proc/asound/cardN/dsp_state)
#   --extra          also create the monitoring channels: CUE (headphone)
#                    outputs, AUX / MON / Talkback captures (off by default)
#   --output-delay NAME=FRAMES
#                    delay the output sink NAME (a node.name such as
#                    apollo_line_out_12 or apollo_monitor) by FRAMES frames
#                    (0-255) with a PipeWire filter-chain "delay" in front of
#                    the loopback, e.g. to line up outputs that feed gear
#                    with different latency.  Repeatable.  Default: none, and
#                    the generated config is then unchanged.
#   --output-delay-file FILE
#                    the same, one NAME=FRAMES per line (# comments allowed)
#   --rate HZ        sample rate used to turn FRAMES into the delay time the
#                    filter takes in seconds (default 48000); the delay is a
#                    fixed time, so at another rate re-run with that --rate
#   -v, --verbose    with --dry-run, also print the generated config
#   -h, --help       show this text

set -euo pipefail

CONF_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/pipewire/pipewire.conf.d"
CONF_FILE="$CONF_DIR/apollo-io-map.conf"

MAX_WAIT=10  # seconds to wait for PipeWire

DRY_RUN=0
VERBOSE=0
WITH_VIRTUAL=auto
WITH_EXTRA=0
IO_MAP_FILE=""
DSP_STATE_FILE=""
CARD=""
RATE=48000
DELAY_NAMES=()
DELAY_FRAMES=()

# ── Helpers ──

log() { echo "[apollo-io] $*"; }
die() { log "ERROR: $*" >&2; exit 1; }

usage() { sed -n '2,/^$/p' "$0" | sed 's/^# \{0,1\}//' | sed '$d'; }

add_delay() {   # NAME=FRAMES
    local spec=$1 name frames
    case "$spec" in *=*) ;; *) die "bad --output-delay '$spec' (want NAME=FRAMES)" ;; esac
    name=${spec%%=*}; frames=${spec#*=}
    [ -n "$name" ] || die "bad --output-delay '$spec': empty name"
    case "$frames" in ''|*[!0-9]*) die "bad --output-delay '$spec': FRAMES must be an integer" ;; esac
    [ "$frames" -le 255 ] || die "--output-delay $name: $frames frames is over the 255 frame limit"
    DELAY_NAMES+=("$name"); DELAY_FRAMES+=("$frames")
}

load_delay_file() {
    local f=$1 line
    [ -r "$f" ] || die "cannot read output-delay file: $f"
    while IFS= read -r line || [ -n "$line" ]; do
        line=${line%%#*}
        line=$(echo "$line" | tr -d '[:space:]')
        [ -n "$line" ] && add_delay "$line"
    done < "$f"
}

# Delay in frames for sink NAME; empty when none or 0 is configured
delay_for() {
    local i f=""
    for ((i = 0; i < ${#DELAY_NAMES[@]}; i++)); do
        [ "${DELAY_NAMES[i]}" = "$1" ] && f=${DELAY_FRAMES[i]}
    done
    [ "${f:-0}" -gt 0 ] && echo "$f"
    return 0
}

wait_for_pipewire() {
    local i=0
    while ! pw-cli info 0 &>/dev/null; do
        sleep 1
        i=$((i + 1))
        [ $i -ge $MAX_WAIT ] && die "PipeWire not responding after ${MAX_WAIT}s"
    done
}

while [ $# -gt 0 ]; do
    case "$1" in
        --dry-run) DRY_RUN=1 ;;
        --io-map)  [ $# -ge 2 ] || die "--io-map needs a file"; IO_MAP_FILE="$2"; shift ;;
        --card)    [ $# -ge 2 ] || die "--card needs a number"; CARD="$2"; shift ;;
        --virtual) WITH_VIRTUAL=1 ;;
        --no-virtual) WITH_VIRTUAL=0 ;;
        --dsp-state) [ $# -ge 2 ] || die "--dsp-state needs a file"; DSP_STATE_FILE="$2"; shift ;;
        --extra)   WITH_EXTRA=1 ;;
        --output-delay) [ $# -ge 2 ] || die "--output-delay needs NAME=FRAMES"; add_delay "$2"; shift ;;
        --output-delay-file) [ $# -ge 2 ] || die "--output-delay-file needs a file"; load_delay_file "$2"; shift ;;
        --rate)    [ $# -ge 2 ] || die "--rate needs a number"; RATE="$2"; shift ;;
        -v|--verbose) VERBOSE=1 ;;
        -h|--help) usage; exit 0 ;;
        *) die "unknown option: $1 (try --help)" ;;
    esac
    shift
done

# ── Step 1: Read the channel map from the driver ──
#
# /proc/asound/cardN/io_map has a "record:" and a "playback:" section with
# one line per ALSA channel:   ch  <n>  slot  <s>  0x<id>  <name>
# ALSA channel n is PipeWire position AUXn.  The ID is (type << 8) | number:
#   0x00/0x01/0x03 analog in (mic/line/HiZ, mic/line, line) or line out,
#   0x02 AUX bus, 0x04 ADAT, 0x05 S/PDIF, 0x06 AES/EBU, 0x07 Virtual,
#   0x08 MADI, 0x09 MON, 0x0a CUE, 0x0b Talkback, 0x0d Dante.
# A channel whose slot is 65535 has no DMA slot at the current rate (the
# firmware has fewer slots at 176.4/192 kHz) and records or plays silence;
# its position stays put, so it is kept and only flagged.
#
# The order and names depend on the driver layout (io_map=0 legacy labels,
# or the firmware descriptor, the default on the x4) and are read, not
# assumed.  Without a readable map (older driver, or a legacy layout with no
# channel IDs) the descriptor layout of the Apollo x4 in S/PDIF mode (22 rec /
# 24 play) is used, but only when the card is an Apollo x4; any other model
# stops with an error (use --io-map FILE to give it a map).

# Apollo x4, S/PDIF mode: the lines the driver prints for it
default_io_map() {
    cat << 'MAP'
record: 22 ALSA channels
  ch  0  slot  0  0x0001  MIC/LINE/HIZ 1
  ch  1  slot  1  0x0002  MIC/LINE/HIZ 2
  ch  2  slot  2  0x0103  MIC/LINE 3
  ch  3  slot  3  0x0104  MIC/LINE 4
  ch  4  slot  4  0x0501  S/PDIF L
  ch  5  slot  5  0x0502  S/PDIF R
  ch  6  slot  6  0x0701  VIRTUAL 1
  ch  7  slot  7  0x0702  VIRTUAL 2
  ch  8  slot  8  0x0703  VIRTUAL 3
  ch  9  slot  9  0x0704  VIRTUAL 4
  ch 10  slot 10  0x0705  VIRTUAL 5
  ch 11  slot 11  0x0706  VIRTUAL 6
  ch 12  slot 12  0x0707  VIRTUAL 7
  ch 13  slot 13  0x0708  VIRTUAL 8
  ch 14  slot 14  0x0901  MON L
  ch 15  slot 15  0x0902  MON R
  ch 16  slot 16  0x0201  AUX1 L
  ch 17  slot 17  0x0202  AUX1 R
  ch 18  slot 18  0x0203  AUX2 L
  ch 19  slot 19  0x0204  AUX2 R
  ch 20  slot 20  0x0b01  TALKBACK 1
  ch 21  slot 21  0x0b02  TALKBACK 2

playback: 24 ALSA channels
  ch  0  slot  0  0x0901  MON L
  ch  1  slot  1  0x0902  MON R
  ch  2  slot  2  0x0301  LINE 1
  ch  3  slot  3  0x0302  LINE 2
  ch  4  slot  4  0x0303  LINE 3
  ch  5  slot  5  0x0304  LINE 4
  ch  6  slot  6  0x0501  S/PDIF L
  ch  7  slot  7  0x0502  S/PDIF R
  ch  8  slot  8  0x0701  VIRTUAL 1
  ch  9  slot  9  0x0702  VIRTUAL 2
  ch 10  slot 10  0x0703  VIRTUAL 3
  ch 11  slot 11  0x0704  VIRTUAL 4
  ch 12  slot 12  0x0705  VIRTUAL 5
  ch 13  slot 13  0x0706  VIRTUAL 6
  ch 14  slot 14  0x0707  VIRTUAL 7
  ch 15  slot 15  0x0708  VIRTUAL 8
  ch 16  slot 16  0x0a01  CUE1 L
  ch 17  slot 17  0x0a02  CUE1 R
  ch 18  slot 18  0x0a03  CUE2 L
  ch 19  slot 19  0x0a04  CUE2 R
  ch 20  slot 20  0x0a05  CUE3 L
  ch 21  slot 21  0x0a06  CUE3 R
  ch 22  slot 22  0x0a07  CUE4 L
  ch 23  slot 23  0x0a08  CUE4 R
MAP
}

# Number of the ALSA card driven by ua_apollo (the PCI address and the card
# number vary between boots)
find_apollo_card() {
    local n="" d
    n=$(awk '/^ *[0-9]+ \[.*\]: *ua_apollo( |$)/ { print $1; exit }' \
        /proc/asound/cards 2>/dev/null || true)
    if [ -z "$n" ]; then
        for d in /sys/class/sound/card[0-9]*; do
            [ -e "$d/device/driver" ] || continue
            if [ "$(basename "$(readlink -f "$d/device/driver")")" = "ua_apollo" ]; then
                n=${d##*/card}
                break
            fi
        done
    fi
    echo "$n"
}

# Per ALSA channel (index = channel number) and direction
REC_N=0; PLAY_N=0
REC_ID=(); REC_SLOT=(); REC_NAME=()
PLAY_ID=(); PLAY_SLOT=(); PLAY_NAME=()
MAP_SOURCE=""
APOLLO_CARD=""

parse_io_map() {   # stdin: io_map text
    local dir ch slot hex name
    REC_N=0; PLAY_N=0
    REC_ID=(); REC_SLOT=(); REC_NAME=()
    PLAY_ID=(); PLAY_SLOT=(); PLAY_NAME=()
    while read -r dir ch slot hex name; do
        case "$ch" in ''|*[!0-9]*) continue ;; esac
        case "$slot" in ''|*[!0-9]*) continue ;; esac
        case "$hex" in 0x[0-9a-fA-F]*) ;; *) continue ;; esac
        if [ "$dir" = rec ]; then
            REC_ID[ch]=$((16#${hex#0x})); REC_SLOT[ch]=$slot; REC_NAME[ch]=$name
            [ $((ch + 1)) -gt $REC_N ] && REC_N=$((ch + 1))
        else
            PLAY_ID[ch]=$((16#${hex#0x})); PLAY_SLOT[ch]=$slot; PLAY_NAME[ch]=$name
            [ $((ch + 1)) -gt $PLAY_N ] && PLAY_N=$((ch + 1))
        fi
    done < <(awk '
        /^record:/   { d = "rec";  next }
        /^playback:/ { d = "play"; next }
        d != "" && $1 == "ch" && $3 == "slot" {
            name = ""
            for (i = 6; i <= NF; i++) name = name (i > 6 ? " " : "") $i
            print d, $2, $4, $5, name
        }')
}

# Number of channels with a usable (non-zero) ID in a direction
count_ids() {
    local dir=$1 n=0 i max
    if [ "$dir" = rec ]; then max=$REC_N; else max=$PLAY_N; fi
    for ((i = 0; i < max; i++)); do
        if [ "$dir" = rec ]; then
            [ "${REC_ID[i]:-0}" -ne 0 ] && n=$((n + 1))
        else
            [ "${PLAY_ID[i]:-0}" -ne 0 ] && n=$((n + 1))
        fi
    done
    echo "$n"
}

# Model name of the card: the "model" line of the io_map text, else the card's
# name in /proc/asound/cards ("N [id]: ua_apollo - <name>")
model_of() {   # file card
    local m=""
    if [ -n "$1" ] && [ -r "$1" ]; then
        m=$(awk '/^model[ \t]/ { sub(/^model[ \t]+/, ""); sub(/ \(device type.*$/, ""); print; exit }' "$1")
    fi
    if [ -z "$m" ] && [ -n "$2" ]; then
        m=$(awk -v n="$2" '$1 == n && /\]:/ { sub(/^.*\]: *[^ ]+ - /, ""); print; exit }' \
            /proc/asound/cards 2>/dev/null || true)
    fi
    echo "$m"
}

use_builtin_x4() {   # why
    log "$1, using the built-in Apollo x4 layout"
    parse_io_map < <(default_io_map)
    MAP_SOURCE="built-in Apollo x4 layout"
}

load_io_map() {
    local file="$IO_MAP_FILE" card="$CARD" model=""
    if [ -z "$file" ]; then
        [ -n "$card" ] || card=$(find_apollo_card)
        [ -n "$card" ] && file="/proc/asound/card$card/io_map"
    fi
    APOLLO_CARD="$card"

    if [ -n "$file" ] && [ -r "$file" ]; then
        parse_io_map < "$file"
        if [ "$(count_ids rec)" -gt 0 ] && [ "$(count_ids play)" -gt 0 ]; then
            MAP_SOURCE="$file"
            return
        fi
        model=$(model_of "$file" "$card")
        if [ "$model" = "Apollo x4" ]; then
            use_builtin_x4 "Warning: no channel IDs in $file"
            return
        fi
        die "no channel IDs in $file and the card is '${model:-unknown}', not an Apollo x4, so the built-in x4 layout would route to the wrong channels. Use a driver that prints channel IDs (io_map=-1), or pass the layout with --io-map FILE."
    elif [ -n "$IO_MAP_FILE" ]; then
        die "cannot read io_map file: $IO_MAP_FILE"
    fi

    [ -n "$card" ] || die "no ALSA card driven by ua_apollo found (is the driver loaded?). Use --card N or --io-map FILE."
    model=$(model_of "" "$card")
    if [ "$model" = "Apollo x4" ]; then
        use_builtin_x4 "No io_map from the driver (older driver?)"
        return
    fi
    die "no io_map from the driver for card $card ('${model:-unknown}'), and the built-in layout is only for an Apollo x4. Update the driver, or pass the layout with --io-map FILE."
}

# ── Step 2: Turn the channel map into a loopback list ──
#
# Each entry: kind|node.name|description|positions|silent|driver names
#   kind     src (Audio/Source, reads record channels) or
#            snk (Audio/Sink, writes playback channels)
#   silent   number of its channels without a DMA slot at the current rate
# Node names and descriptions are the ones earlier versions of this script
# created, so existing defaults keep working.

ITEMS=()
IDX=-1

# Sets IDX to the first channel carrying this ID; fails when there is none
find_ch() {   # dir id
    local dir=$1 id=$2 i max
    if [ "$dir" = rec ]; then max=$REC_N; else max=$PLAY_N; fi
    for ((i = 0; i < max; i++)); do
        if [ "$dir" = rec ]; then
            [ "${REC_ID[i]:-0}" -eq "$id" ] && { IDX=$i; return 0; }
        else
            [ "${PLAY_ID[i]:-0}" -eq "$id" ] && { IDX=$i; return 0; }
        fi
    done
    IDX=-1
    return 1
}

add_item() {   # kind dir name desc idx...
    local kind=$1 dir=$2 name=$3 desc=$4 pos="" silent=0 names="" i
    shift 4
    for i in "$@"; do
        pos="$pos${pos:+ }AUX$i"
        if [ "$dir" = rec ]; then
            [ "${REC_SLOT[i]:-0}" -ge 65535 ] && silent=$((silent + 1))
            names="$names${names:+ + }${REC_NAME[i]:-}"
        else
            [ "${PLAY_SLOT[i]:-0}" -ge 65535 ] && silent=$((silent + 1))
            names="$names${names:+ + }${PLAY_NAME[i]:-}"
        fi
    done
    ITEMS+=("$kind|$name|$desc|$pos|$silent|$names")
}

# Stereo pair from two channel IDs of one direction; skipped if either is absent
add_pair() {   # kind dir name desc idL idR
    local l r
    find_ch "$2" "$5" || return 0
    l=$IDX
    find_ch "$2" "$6" || return 0
    r=$IDX
    add_item "$1" "$2" "$3" "$4" "$l" "$r"
}

# Highest channel number (low ID byte) of a type in a direction; 0 when none
max_num() {   # dir type
    local dir=$1 t=$2 i max id m=0
    if [ "$dir" = rec ]; then max=$REC_N; else max=$PLAY_N; fi
    for ((i = 0; i < max; i++)); do
        if [ "$dir" = rec ]; then id=${REC_ID[i]:-0}; else id=${PLAY_ID[i]:-0}; fi
        if [ $((id >> 8)) -eq "$t" ] && [ $((id & 255)) -gt "$m" ]; then m=$((id & 255)); fi
    done
    echo "$m"
}

# Stereo pairs 1+2, 3+4, ... of one channel type (ID = type << 8 | number);
# a missing member skips that pair, an odd last channel gets no item
add_typed_pairs() {   # kind dir type node_prefix desc_prefix
    local kind=$1 dir=$2 t=$3 np=$4 dp=$5 n m
    m=$(max_num "$dir" "$t")
    for ((n = 1; n < m; n += 2)); do
        add_pair "$kind" "$dir" "${np}_$n$((n + 1))" "$dp $n+$((n + 1))" \
            $((t * 256 + n)) $((t * 256 + n + 1))
    done
}

# The pair (n, n+1) of a bus type numbered by pair: AUX1 L/R, CUE2 L/R ...
add_bus_pairs() {   # kind dir type node_fmt desc_fmt  (%d = pair number)
    local kind=$1 dir=$2 t=$3 nf=$4 df=$5 n m
    m=$(max_num "$dir" "$t")
    for ((n = 1; n < m; n += 2)); do
        # shellcheck disable=SC2059
        add_pair "$kind" "$dir" "$(printf "$nf" $(((n + 1) / 2)))" \
            "$(printf "$df" $(((n + 1) / 2)))" $((t * 256 + n)) $((t * 256 + n + 1))
    done
}

# Virtual inputs with a live Input Strip: VIRTUAL_LIVE[n] = 1 for Virtual n.
# The driver's dsp_state lists them as
#   strip 4: Virtual 1 (DSP input 24), instance 0x9d060, live
VIRTUAL_LIVE=()
VIRTUAL_WHY=""

load_virtual_strips() {
    local file="$DSP_STATE_FILE" n
    VIRTUAL_LIVE=()
    [ "$WITH_VIRTUAL" = auto ] || return 0
    if [ -z "$file" ]; then
        if [ -n "$IO_MAP_FILE" ] || [ -z "$APOLLO_CARD" ]; then
            VIRTUAL_WHY="no dsp_state to read (pass --dsp-state FILE or --virtual)"
            return 0
        fi
        file="/proc/asound/card$APOLLO_CARD/dsp_state"
    fi
    if [ ! -r "$file" ]; then
        VIRTUAL_WHY="cannot read $file"
        return 0
    fi
    while read -r n; do
        VIRTUAL_LIVE[n]=1
    done < <(awk '$1 == "strip" && $3 == "Virtual" && / live( |$)/ { print $4 + 0 }' "$file")
    VIRTUAL_WHY="Input Strips live on Virtual ${!VIRTUAL_LIVE[*]}"
    [ ${#VIRTUAL_LIVE[@]} -gt 0 ] || VIRTUAL_WHY="no Input Strip live on a Virtual input ($file)"
}

# Virtual pairs 1+2, 3+4, ... of one direction: every pair with --virtual,
# by default the pairs whose two inputs both have a live strip
add_virtual_pairs() {   # kind dir node_prefix desc_prefix
    local kind=$1 dir=$2 np=$3 dp=$4 n m
    [ "$WITH_VIRTUAL" = 0 ] && return 0
    m=$(max_num "$dir" 7)
    for ((n = 1; n < m; n += 2)); do
        if [ "$WITH_VIRTUAL" = auto ] &&
           { [ -z "${VIRTUAL_LIVE[n]:-}" ] || [ -z "${VIRTUAL_LIVE[n + 1]:-}" ]; }; then
            continue
        fi
        add_pair "$kind" "$dir" "${np}_$n$((n + 1))" "$dp $n+$((n + 1))" \
            $((0x0700 + n)) $((0x0700 + n + 1))
    done
}

# Types: 0x00 MIC/LINE/HIZ, 0x01 MIC/LINE, 0x03 LINE, 0x04 ADAT, 0x05 S/PDIF,
# 0x06 AES/EBU, 0x07 VIRTUAL, 0x08 MADI, 0x09 MON, 0x0a CUE, 0x0d DANTE
build_items() {
    local i t n p id idx
    local analog=()

    # Analog inputs (types 0x00 / 0x01 / 0x03), in channel order.  Mic/line
    # inputs (types 0, 1) are "Mic n", line-only inputs (type 3) "Line In n".
    for ((i = 0; i < REC_N; i++)); do
        id=${REC_ID[i]:-0}
        t=$((id >> 8))
        case "$t" in 0|1|3) [ "$id" -ne 0 ] && analog+=("$i") ;; esac
    done
    for ((n = 0; n < ${#analog[@]}; n++)); do
        idx=${analog[n]}
        if [ $((REC_ID[idx] >> 8)) -eq 3 ]; then
            add_item src rec "apollo_line_in_$((n + 1))" "Apollo Line In $((n + 1))" "$idx"
        else
            add_item src rec "apollo_mic_$((n + 1))" "Apollo Mic $((n + 1))" "$idx"
        fi
        if [ "$n" -eq 1 ]; then   # inputs 1+2 as a stereo pair right after input 2
            if [ $((REC_ID[analog[0]] >> 8)) -ne 3 ] && [ $((REC_ID[analog[1]] >> 8)) -ne 3 ]; then
                add_item src rec apollo_mic_stereo "Apollo Mic 1+2" "${analog[0]}" "${analog[1]}"
            else
                add_item src rec apollo_line_12 "Apollo Line In 1+2" "${analog[0]}" "${analog[1]}"
            fi
        fi
    done
    for ((p = 1; p * 2 + 1 < ${#analog[@]}; p++)); do   # Line In 3+4, 5+6, ...
        add_item src rec "apollo_line_$((p * 2 + 1))$((p * 2 + 2))" \
            "Apollo Line In $((p * 2 + 1))+$((p * 2 + 2))" \
            "${analog[p * 2]}" "${analog[p * 2 + 1]}"
    done

    add_typed_pairs src rec 4 apollo_adat_in "Apollo ADAT In"
    add_pair src rec apollo_spdif_in "Apollo S/PDIF In" $((0x0501)) $((0x0502))
    add_pair src rec apollo_aes_in "Apollo AES/EBU In" $((0x0601)) $((0x0602))
    add_typed_pairs src rec 8 apollo_madi_in "Apollo MADI In"
    add_typed_pairs src rec 13 apollo_dante_in "Apollo Dante In"
    add_virtual_pairs src rec apollo_virtual_in "Apollo Virtual In"
    if [ "$WITH_EXTRA" -eq 1 ]; then
        add_bus_pairs src rec 2 "apollo_aux%d_in" "Apollo AUX %d In"
        add_pair src rec apollo_monitor_in "Apollo Monitor In" $((0x0901)) $((0x0902))
        add_pair src rec apollo_talkback "Apollo Talkback" $((0x0b01)) $((0x0b02))
    fi

    # Playback: MON L/R, Line outs (type 0x03) in pairs, digital, Virtual
    add_pair snk play apollo_monitor "Apollo Monitor L/R" $((0x0901)) $((0x0902))
    add_typed_pairs snk play 3 apollo_line_out "Apollo Line Out"
    add_typed_pairs snk play 4 apollo_adat_out "Apollo ADAT Out"
    add_pair snk play apollo_spdif_out "Apollo S/PDIF Out" $((0x0501)) $((0x0502))
    add_pair snk play apollo_aes_out "Apollo AES/EBU Out" $((0x0601)) $((0x0602))
    add_typed_pairs snk play 8 apollo_madi_out "Apollo MADI Out"
    add_typed_pairs snk play 13 apollo_dante_out "Apollo Dante Out"
    add_virtual_pairs snk play apollo_virtual_out "Apollo Virtual Out"
    if [ "$WITH_EXTRA" -eq 1 ]; then
        add_bus_pairs snk play 10 "apollo_cue%d" "Apollo CUE %d"
    fi
    [ ${#ITEMS[@]} -gt 0 ] || die "no usable channels in the io_map"
}

# Every --output-delay name must be an output sink this run creates
check_delays() {
    local i name item kind iname rest ok sinks=""
    case "$RATE" in ''|*[!0-9]*|0) die "--rate must be a positive integer" ;; esac
    for item in "${ITEMS[@]}"; do
        IFS='|' read -r kind iname rest <<< "$item"
        [ "$kind" = snk ] && sinks="$sinks${sinks:+, }$iname"
    done
    for ((i = 0; i < ${#DELAY_NAMES[@]}; i++)); do
        name=${DELAY_NAMES[i]}; ok=0
        for item in "${ITEMS[@]}"; do
            IFS='|' read -r kind iname rest <<< "$item"
            [ "$kind" = snk ] && [ "$iname" = "$name" ] && ok=1
        done
        [ "$ok" -eq 1 ] || die "--output-delay: no output sink named '$name' (have: $sinks)"
    done
}

# ── Step 3: Config generation ──

# An output sink with a host delay: a filter-chain of builtin "delay" nodes,
# one per channel, between the Audio/Sink apps write to and the ALSA output.
# The delay is a fixed time (frames / RATE seconds).
emit_delayed_sink() {   # name desc pos names silent frames out_node
    local name=$1 desc=$2 pos=$3 names=$4 silent=$5 frames=$6 out_node=$7
    local secs maxsecs n c in_ports="" out_ports=""
    local -a p
    read -r -a p <<< "$pos"
    n=${#p[@]}
    secs=$(awk -v f="$frames" -v r="$RATE" 'BEGIN { printf "%.9f", f / r }')
    maxsecs=$(awk -v r="$RATE" 'BEGIN { printf "%.9f", 255 / r }')
    echo
    echo "    # $desc  [$names]  + output delay $frames frames ($secs s at $RATE Hz)"
    [ "$silent" -gt 0 ] && echo "    # $silent channel(s) have no DMA slot at the current rate (silent)"
    echo "    { name = libpipewire-module-filter-chain"
    echo "        args = {"
    echo "            node.description = \"$desc\""
    echo "            filter.graph = {"
    echo "                nodes = ["
    for ((c = 0; c < n; c++)); do
        echo "                    { type = builtin name = delay$c label = delay"
        echo "                      config = { \"max-delay\" = $maxsecs }"
        echo "                      control = { \"Delay (s)\" = $secs } }"
        in_ports="$in_ports${in_ports:+ }\"delay$c:In\""
        out_ports="$out_ports${out_ports:+ }\"delay$c:Out\""
    done
    echo "                ]"
    echo "                inputs  = [ $in_ports ]"
    echo "                outputs = [ $out_ports ]"
    echo "            }"
    echo "            capture.props = {"
    echo "                node.name           = \"$name\""
    echo "                media.class         = \"Audio/Sink\""
    echo "                audio.position      = [ FL FR ]"
    echo "            }"
    echo "            playback.props = {"
    echo "                node.target         = \"$out_node\""
    echo "                audio.position      = [ $pos ]"
    echo "                stream.dont-remix   = true"
    echo "            }"
    echo "        }"
    echo "    }"
}

emit_config() {   # input_node output_node
    local in_node=$1 out_node=$2 item kind name desc pos silent names delay
    local -a p
    echo "# Auto-generated by setup-apollo-io.sh — do not edit manually"
    echo "# Apollo virtual I/O devices mapped to pro-audio channels"
    echo "# Channel map: $MAP_SOURCE"
    echo "# Input: $in_node | Output: $out_node"
    echo
    echo "context.modules = ["
    for item in "${ITEMS[@]}"; do
        IFS='|' read -r kind name desc pos silent names <<< "$item"
        read -r -a p <<< "$pos"
        delay=""
        [ "$kind" = snk ] && delay=$(delay_for "$name")
        if [ -n "$delay" ]; then
            emit_delayed_sink "$name" "$desc" "$pos" "$names" "$silent" "$delay" "$out_node"
            continue
        fi
        echo
        echo "    # $desc  [$names]"
        [ "$silent" -gt 0 ] && echo "    # $silent channel(s) have no DMA slot at the current rate (silent)"
        echo "    { name = libpipewire-module-loopback"
        echo "        args = {"
        echo "            node.description = \"$desc\""
        if [ "$kind" = src ]; then
            # capture side targets ALSA input (passive), playback side is
            # the Audio/Source apps read from
            echo "            capture.props = {"
            echo "                audio.position      = [ $pos ]"
            echo "                stream.dont-remix   = true"
            echo "                node.target         = \"$in_node\""
            echo "                node.passive        = true"
            echo "            }"
            echo "            playback.props = {"
            echo "                node.name           = \"$name\""
            echo "                media.class         = \"Audio/Source\""
            if [ "${#p[@]}" -eq 1 ]; then
                echo "                audio.position      = [ MONO ]"
            else
                echo "                audio.position      = [ FL FR ]"
            fi
            echo "            }"
        else
            # capture side is the Audio/Sink apps write to, playback side
            # targets ALSA output (not passive, it must drive the output)
            echo "            capture.props = {"
            echo "                node.name           = \"$name\""
            echo "                media.class         = \"Audio/Sink\""
            echo "                audio.position      = [ FL FR ]"
            echo "            }"
            echo "            playback.props = {"
            echo "                node.target         = \"$out_node\""
            echo "                audio.position      = [ $pos ]"
            echo "                stream.dont-remix   = true"
            echo "            }"
        fi
        echo "        }"
        echo "    }"
    done
    echo "]"
}

print_items() {
    local item kind name desc pos silent names note
    log "Channel map: $MAP_SOURCE (record $REC_N ch, playback $PLAY_N ch)"
    for item in "${ITEMS[@]}"; do
        IFS='|' read -r kind name desc pos silent names <<< "$item"
        note=""
        [ "$silent" -gt 0 ] && note=" (silent at this rate: $silent)"
        if [ "$kind" = snk ] && [ -n "$(delay_for "$name")" ]; then
            note="$note (delay $(delay_for "$name") frames)"
        fi
        if [ "$kind" = src ]; then kind=source; else kind=sink; fi
        printf '[apollo-io] %-6s %-24s "%s" <- %s [%s]%s\n' \
            "$kind" "$name" "$desc" "$pos" "$names" "$note"
    done
}

summary_line() {   # kind
    local item kind name desc rest out=""
    for item in "${ITEMS[@]}"; do
        IFS='|' read -r kind name desc rest <<< "$item"
        [ "$kind" = "$1" ] && out="$out${out:+, }${desc#Apollo }"
    done
    echo "$out"
}

load_io_map
load_virtual_strips
build_items
check_delays
[ "$WITH_VIRTUAL" = auto ] && log "Virtual loopbacks: $VIRTUAL_WHY"

if [ "$DRY_RUN" -eq 1 ]; then
    print_items
    if [ "$VERBOSE" -eq 1 ]; then
        echo
        emit_config "<alsa_input.pci-ADDR.multichannel-input>" \
                    "<alsa_output.pci-ADDR.multichannel-output>"
    fi
    exit 0
fi

# ── Step 4: Wait for PipeWire ──

log "Waiting for PipeWire..."
wait_for_pipewire
log "PipeWire is up"

# ── Step 5: Find Apollo ALSA device and set pro-audio profile ──

# The Apollo is the card driven by ua_apollo whatever its name ("Apollo x4",
# "Apollo x8p", "Unknown UA Device"): match PipeWire's alsa.driver_name, and
# alsa.card when the card number is known.
PW_QUERY_PY='
import json, sys
want, mode = sys.argv[1], sys.argv[2]
try:
    objs = json.load(sys.stdin)
except Exception:
    sys.exit(0)
inp = out = dev = ""
for obj in objs:
    props = obj.get("info", {}).get("props", {})
    if props.get("alsa.driver_name") != "ua_apollo":
        continue
    if want and str(props.get("alsa.card", want)) != want:
        continue
    if obj.get("type") == "PipeWire:Interface:Device" and not dev:
        dev = str(obj["id"])
    name = props.get("node.name", "")
    if "input" in name and not inp:
        inp = name
    if "output" in name and not out:
        out = name
print({"device": dev, "node-in": inp, "node-out": out}[mode])
'
pw_query() { pw-dump 2>/dev/null | python3 -c "$PW_QUERY_PY" "$APOLLO_CARD" "$1" 2>/dev/null || true; }

# wpctl device id of the Apollo; wpctl status text only as a fallback
find_device_id() {
    local id name
    id=$(pw_query device)
    if [ -z "$id" ]; then
        name=$(model_of "" "$APOLLO_CARD")
        id=$(wpctl status 2>/dev/null | grep '\[alsa\]' | grep -F "${name:-Apollo}" \
            | grep -oP '[0-9]+' | head -1 || true)
    fi
    echo "$id"
}

DEVICE_ID=""
for _ in 1 2 3 4 5; do
    DEVICE_ID=$(find_device_id)
    [ -n "$DEVICE_ID" ] && break
    sleep 1
done

if [ -z "$DEVICE_ID" ]; then
    die "Apollo device (alsa.driver_name ua_apollo) not found in PipeWire. Is the driver loaded?"
fi

log "Found Apollo device ID: $DEVICE_ID"

# Set pro-audio profile (index 1) — required for multi-channel access
# WirePlumber's alsa_monitor.rules doesn't auto-select pro-audio
if wpctl set-profile "$DEVICE_ID" 1 2>/dev/null; then
    log "Set pro-audio profile on device $DEVICE_ID"
else
    log "Warning: failed to set pro-audio profile (may already be set)"
fi

# Brief pause for node creation after profile change
sleep 1

# ── Step 6: Discover ALSA node names ──

# Find Apollo pro-audio node names via pw-dump (reliable, handles any PCI address)
INPUT_NODE=$(pw_query node-in)
OUTPUT_NODE=$(pw_query node-out)

[ -z "$INPUT_NODE" ] && die "Apollo pro-audio input node not found. Is pro-audio profile active?"
[ -z "$OUTPUT_NODE" ] && die "Apollo pro-audio output node not found. Is pro-audio profile active?"

log "Input node:  $INPUT_NODE"
log "Output node: $OUTPUT_NODE"

# ── Step 7: Write the loopback config ──
#
# The loopbacks name no rate, so they follow the Apollo nodes (WirePlumber
# groups them as "ua-apollo" and lets them change rate).  The file is
# rewritten on every run, which replaces any earlier set of loopbacks.

mkdir -p "$CONF_DIR"
emit_config "$INPUT_NODE" "$OUTPUT_NODE" > "$CONF_FILE"

log "Generated config: $CONF_FILE"
log "  Sources: $(summary_line src)"
log "  Sinks:   $(summary_line snk)"

# ── Step 8: Restart PipeWire to load the config ──

# PipeWire reads conf.d on startup, so we need a restart to pick up the new config.
if systemctl --user is-active pipewire.service &>/dev/null; then
    log "Restarting PipeWire to load new config..."
    systemctl --user restart pipewire.service
    sleep 2
    wait_for_pipewire

    # Re-set pro-audio profile after restart (lost during restart)
    sleep 1
    NEW_DEVID=""
    for _ in 1 2 3 4 5 6 7 8 9 10; do
        NEW_DEVID=$(find_device_id)
        [ -n "$NEW_DEVID" ] && break
        sleep 1
    done
    if [ -n "$NEW_DEVID" ]; then
        wpctl set-profile "$NEW_DEVID" 1 2>/dev/null || true
        log "Re-set pro-audio profile (device $NEW_DEVID)"
    fi
fi

# ── Step 9: Verify ──

sleep 2
SOURCES=$(wpctl status 2>/dev/null | grep -c 'Apollo' || echo 0)
log "Apollo nodes visible in wpctl: $SOURCES"

if [ "$SOURCES" -gt 0 ]; then
    log "Setup complete!"
else
    log "Warning: no Apollo virtual nodes found. Check: wpctl status"
fi
