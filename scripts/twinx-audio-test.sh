#!/usr/bin/env bash
#
# twinx-audio-test.sh — does audio actually come out?
#
# Scoping note that makes this worth trying before any routing work:
# ua_dsp_send_routing() is reachable ONLY from the UA_IOCTL_SEND_ROUTING
# ioctl. It is not called from probe, from connect, or from the PCM path, and
# rec_table/play_table/rec_names/play_names in struct ua_routing_config are
# referenced zero times in the driver's .c files. So the missing Twin X routing
# config does not block the ALSA path — it only means the on-device DSP mixer
# has not been told the channel layout.
#
# SAFETY: plays a deliberately quiet tone (-30 dBFS, not full scale) because
# channel mapping is unverified and we do not know which physical output a given
# DMA channel lands on.
#
#   TURN THE MONITOR KNOB DOWN AND UNPLUG HEADPHONES BEFORE RUNNING.
#
# Usage:  ./scripts/twinx-audio-test.sh [seconds]
#
set -uo pipefail

SECS="${1:-4}"
red() { printf '\033[31m%s\033[0m\n' "$*"; }
grn() { printf '\033[32m%s\033[0m\n' "$*"; }
ylw() { printf '\033[33m%s\033[0m\n' "$*"; }
hdr() { printf '\n\033[1m== %s ==\033[0m\n' "$*"; }

hdr "Preflight"
lsmod | grep -q '^ua_apollo\b' || { red "ua_apollo not loaded — run: sudo ./scripts/twinx-switch.sh claim"; exit 1; }
grn "module loaded"

CARD=$(awk '/\[/{card=$1; name=$0} /ua_apollo|Apollo/{print card; exit}' /proc/asound/cards 2>/dev/null)
if [[ -z "$CARD" ]]; then
    # fall back: match by driver dir
    for c in /proc/asound/card*; do
        [[ -r "$c/id" ]] || continue
        if grep -qi apollo "$c/id" 2>/dev/null; then CARD=$(basename "$c" | tr -dc 0-9); fi
    done
fi
[[ -n "$CARD" ]] || { red "no Apollo ALSA card found"; cat /proc/asound/cards; exit 1; }
grn "ALSA card index: $CARD  ($(cat /proc/asound/card$CARD/id 2>/dev/null))"

echo
echo "PCM devices:"
cat /proc/asound/card$CARD/pcm* 2>/dev/null | head || ls /dev/snd/ | grep "pcmC${CARD}" || true

hdr "Counters BEFORE playback"
before=$(journalctl -k --no-pager --since "-1 min" 2>/dev/null | grep -c "SAMPLE_POS" || echo 0)
echo "SAMPLE_POS log lines so far: $before"

hdr "Generating quiet test tone (-30 dBFS, ${SECS}s)"
TONE=$(mktemp --suffix=.wav) || exit 1
trap 'rm -f "$TONE"' EXIT
if command -v sox >/dev/null 2>&1; then
    sox -n -r 48000 -c 2 -b 24 "$TONE" synth "$SECS" sine 440 vol -30dB
    grn "generated with sox"
elif python3 -c "import wave,struct,math" 2>/dev/null; then
    python3 - "$TONE" "$SECS" <<'PY'
import wave, struct, math, sys
path, secs = sys.argv[1], float(sys.argv[2])
rate, amp = 48000, 10**(-30/20)          # -30 dBFS
w = wave.open(path, "wb"); w.setnchannels(2); w.setsampwidth(2); w.setframerate(rate)
frames = bytearray()
for i in range(int(rate*secs)):
    v = int(amp * 32767 * math.sin(2*math.pi*440*i/rate))
    frames += struct.pack("<hh", v, v)
w.writeframes(bytes(frames)); w.close()
PY
    grn "generated with python"
else
    red "need sox or python3 to generate a tone"; exit 1
fi

hdr "Attempt 1: through PipeWire (default route)"
if command -v pw-play >/dev/null 2>&1; then
    pw-play "$TONE" 2>&1 | tail -5 && grn "pw-play returned 0" || ylw "pw-play failed"
else
    ylw "pw-play not present"
fi

hdr "Attempt 2: direct ALSA to hw:$CARD,0"
ylw "PipeWire may hold the device; a busy error here is expected if so."
aplay -D "plughw:$CARD,0" "$TONE" 2>&1 | tail -6 || ylw "aplay returned non-zero"

hdr "Counters AFTER playback"
journalctl -k --no-pager --since "-2 min" 2>/dev/null \
  | grep -E "ua_apollo" \
  | grep -E "SAMPLE_POS|FRAME_CTR|xrun|underrun|pcm_prepare|START TRANSPORT|STOP TRANSPORT" \
  | tail -20

hdr "Errors during playback"
journalctl -k --no-pager --since "-2 min" 2>/dev/null | grep -E "ua_apollo" \
  | grep -iE "error|fail|xrun|underrun|timeout|stall" | tail -10 || grn "(none)"

hdr "Interpretation"
cat <<'EOF'
  SAMPLE_POS advancing while a stream is open  -> the DMA engine is running
  aplay/pw-play completing without error       -> the ALSA path is wired up
  audible tone                                 -> end-to-end audio works
  silence but clean counters                   -> DSP mixer/routing is the gap,
                                                  i.e. the io_desc work is next
EOF
