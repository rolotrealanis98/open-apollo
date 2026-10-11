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
#   --no-virtual     do not create the Virtual loopbacks
#   -v, --verbose    with --dry-run, also print the generated config
#   -h, --help       show this text

set -euo pipefail

CONF_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/pipewire/pipewire.conf.d"
CONF_FILE="$CONF_DIR/apollo-io-map.conf"

MAX_WAIT=10  # seconds to wait for PipeWire

DRY_RUN=0
VERBOSE=0
WITH_VIRTUAL=1
IO_MAP_FILE=""
CARD=""

# ── Helpers ──

log() { echo "[apollo-io] $*"; }
die() { log "ERROR: $*" >&2; exit 1; }

usage() { sed -n '2,/^$/p' "$0" | sed 's/^# \{0,1\}//' | sed '$d'; }

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
        --no-virtual) WITH_VIRTUAL=0 ;;
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
#   0x00/0x01/0x03 analog in or line out, 0x02 AUX bus, 0x04 ADAT,
#   0x05 S/PDIF, 0x07 Virtual, 0x09 MON, 0x0a CUE, 0x0b Talkback.
# A channel whose slot is 65535 has no DMA slot at the current rate (the
# firmware has fewer slots at 176.4/192 kHz) and records or plays silence;
# its position stays put, so it is kept and only flagged.
#
# The order and names depend on the driver layout (io_map=0 legacy labels,
# or the firmware descriptor, the default on the x4) and are read, not
# assumed.  Without a readable map (older driver) the descriptor layout of
# the Apollo x4 in S/PDIF mode (22 rec / 24 play) is used.

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

load_io_map() {
    local file="$IO_MAP_FILE" card="$CARD"
    if [ -z "$file" ]; then
        [ -n "$card" ] || card=$(find_apollo_card)
        [ -n "$card" ] && file="/proc/asound/card$card/io_map"
    fi

    if [ -n "$file" ] && [ -r "$file" ]; then
        parse_io_map < "$file"
        if [ "$(count_ids rec)" -gt 0 ] && [ "$(count_ids play)" -gt 0 ]; then
            MAP_SOURCE="$file"
            return
        fi
        log "Warning: no channel IDs in $file, using the built-in x4 layout"
    elif [ -n "$IO_MAP_FILE" ]; then
        die "cannot read io_map file: $IO_MAP_FILE"
    else
        log "No io_map from the driver (older driver?), using the built-in x4 layout"
    fi
    parse_io_map < <(default_io_map)
    MAP_SOURCE="built-in Apollo x4 layout"
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

build_items() {
    local i t n p id
    local analog=()

    # Analog inputs (types 0x00 / 0x01 / 0x03), in channel order
    for ((i = 0; i < REC_N; i++)); do
        id=${REC_ID[i]:-0}
        t=$((id >> 8))
        case "$t" in 0|1|3) [ "$id" -ne 0 ] && analog+=("$i") ;; esac
    done
    for ((n = 0; n < ${#analog[@]}; n++)); do
        add_item src rec "apollo_mic_$((n + 1))" "Apollo Mic $((n + 1))" "${analog[n]}"
        if [ "$n" -eq 1 ]; then   # Mic 1+2 stereo right after Mic 2
            add_item src rec apollo_mic_stereo "Apollo Mic 1+2" "${analog[0]}" "${analog[1]}"
        fi
    done
    for ((p = 1; p * 2 + 1 < ${#analog[@]}; p++)); do   # Line In 3+4, 5+6, ...
        add_item src rec "apollo_line_$((p * 2 + 1))$((p * 2 + 2))" \
            "Apollo Line In $((p * 2 + 1))+$((p * 2 + 2))" \
            "${analog[p * 2]}" "${analog[p * 2 + 1]}"
    done

    add_pair src rec apollo_spdif_in "Apollo S/PDIF In" $((0x0501)) $((0x0502))
    if [ "$WITH_VIRTUAL" -eq 1 ]; then
        for n in 1 3 5 7; do
            add_pair src rec "apollo_virtual_in_$n$((n + 1))" \
                "Apollo Virtual In $n+$((n + 1))" $((0x0700 + n)) $((0x0700 + n + 1))
        done
    fi

    # Playback: MON L/R, Line outs (type 0x03) in pairs, S/PDIF, Virtual
    add_pair snk play apollo_monitor "Apollo Monitor L/R" $((0x0901)) $((0x0902))
    for n in 1 3 5 7; do
        add_pair snk play "apollo_line_out_$n$((n + 1))" \
            "Apollo Line Out $n+$((n + 1))" $((0x0300 + n)) $((0x0300 + n + 1))
    done
    add_pair snk play apollo_spdif_out "Apollo S/PDIF Out" $((0x0501)) $((0x0502))
    if [ "$WITH_VIRTUAL" -eq 1 ]; then
        for n in 1 3 5 7; do
            add_pair snk play "apollo_virtual_out_$n$((n + 1))" \
                "Apollo Virtual Out $n+$((n + 1))" $((0x0700 + n)) $((0x0700 + n + 1))
        done
    fi
    [ ${#ITEMS[@]} -gt 0 ] || die "no usable channels in the io_map"
}

# ── Step 3: Config generation ──

emit_config() {   # input_node output_node
    local in_node=$1 out_node=$2 item kind name desc pos silent names
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
build_items

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

# Find the device ID by matching "Apollo" in wpctl device list
DEVICE_ID=$(wpctl status 2>/dev/null | grep 'Apollo x4' | grep '\[alsa\]' | grep -oP '[0-9]+' | head -1 || true)

# Fallback: search pw-dump for driver name
if [ -z "$DEVICE_ID" ]; then
    DEVICE_ID=$(pw-dump 2>/dev/null | python3 -c "
import json, sys
for obj in json.load(sys.stdin):
    props = obj.get('info', {}).get('props', {})
    if props.get('alsa.driver_name') == 'ua_apollo' and obj.get('type') == 'PipeWire:Interface:Device':
        print(obj['id']); break
" 2>/dev/null || true)
fi

if [ -z "$DEVICE_ID" ]; then
    die "Apollo device not found in PipeWire. Is the driver loaded?"
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
eval "$(pw-dump 2>/dev/null | python3 -c "
import json, sys
data = json.load(sys.stdin)
inp = out = ''
for obj in data:
    props = obj.get('info', {}).get('props', {})
    if props.get('alsa.driver_name') != 'ua_apollo': continue
    name = props.get('node.name', '')
    if 'input' in name and not inp: inp = name
    if 'output' in name and not out: out = name
print(f'INPUT_NODE=\"{inp}\"')
print(f'OUTPUT_NODE=\"{out}\"')
" 2>/dev/null || echo 'INPUT_NODE="" ; OUTPUT_NODE=""')"

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
    NEW_DEVID=$(wpctl status 2>/dev/null | grep 'Apollo x4' | grep '\[alsa\]' | grep -oP '[0-9]+' | head -1 || true)
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
