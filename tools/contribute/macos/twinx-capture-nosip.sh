#!/usr/bin/env bash
#
# twinx-capture-nosip.sh — Apollo Twin X data capture on macOS, WITHOUT SIP.
#
# Run this FIRST, before considering DTrace. It needs no SIP change, no kernel
# tracing, and makes no network calls. Everything here is standard macOS
# introspection of data the OS already publishes about the connected device.
#
# What this closes: the DMA/CoreAudio channel counts and device identification
# (gap 2). The x4's authoritative 24/22 came from exactly this kind of IOKit
# property read; the Twin X's current 8/8 in ua_models[] is an unverified
# placeholder.
#
# What this CANNOT close: the routing tables and IO descriptors (gap 3) or the
# DSP program blobs (gap 4). Those are only visible while the UA driver is
# actively exchanging them with UA Console, which requires DTrace and therefore
# SIP. See twinx-dtrace-smoke.d before going down that road.
#
# Read-only. No writes, no installs, no uploads. Output goes to a directory you
# can review file by file before sharing anything.
#
# Usage:  ./twinx-capture-nosip.sh [outdir]
#
set -uo pipefail

OUT="${1:-./twinx-capture-$(date +%Y%m%d-%H%M%S)}"

grn() { printf '\033[32m%s\033[0m\n' "$*"; }
ylw() { printf '\033[33m%s\033[0m\n' "$*"; }
hdr() { printf '\n\033[1m== %s ==\033[0m\n' "$*"; }

if [[ "$(uname -s)" != "Darwin" ]]; then
    printf '\033[31mThis script is for macOS. Run it on the Mac with the Apollo attached.\033[0m\n'
    exit 1
fi

mkdir -p "$OUT" || exit 1
hdr "Apollo Twin X capture (no SIP required)"
echo "output: $OUT"
echo
echo "Before continuing, make sure:"
echo "  - the Apollo Twin X is connected via Thunderbolt and powered on"
echo "  - UA Console (or UA Connect) is running and shows the device"
echo "  - audio is playing through it, so the engine is active"
echo
read -rp "Press Enter to capture, Ctrl+C to cancel..."

run() {  # run <outfile> <description> <cmd...>
    local f="$OUT/$1"; local desc="$2"; shift 2
    printf '  %-42s' "$desc"
    if "$@" >"$f" 2>&1; then
        if [[ -s "$f" ]]; then grn "ok ($(wc -l <"$f" | tr -d ' ') lines)"; else ylw "empty"; fi
    else
        ylw "failed (kept stderr)"
    fi
}

hdr "System"
run system-version.txt      "macOS version"            sw_vers
run uname.txt               "kernel / arch"            uname -a
run sip-status.txt          "SIP status (informational)" csrutil status

hdr "Thunderbolt topology"
# Gives vendor/device IDs, link speed, and the device tree as macOS sees it.
run thunderbolt.json        "SPThunderboltDataType (json)"  system_profiler -json SPThunderboltDataType
run thunderbolt.txt         "SPThunderboltDataType (text)"  system_profiler SPThunderboltDataType

hdr "Audio device inventory"
# SPAudioDataType reports per-device input/output channel counts as CoreAudio
# sees them. This is the headline number for gap 2.
run audio.json              "SPAudioDataType (json)"    system_profiler -json SPAudioDataType
run audio.txt               "SPAudioDataType (text)"    system_profiler SPAudioDataType

hdr "IORegistry — the authoritative channel counts"
# The x4's 24/22 came from IOAudioEngine properties. Dump broadly: UA's engine
# class naming varies by driver version, so capture several angles rather than
# guessing one class name.
run ioreg-full.txt          "full ioreg tree (-l -w0)"        ioreg -l -w0
run ioreg-audioengine.txt   "IOAudioEngine subtree"           ioreg -l -w0 -r -c IOAudioEngine
run ioreg-audiodevice.txt   "IOAudioDevice subtree"           ioreg -l -w0 -r -c IOAudioDevice
run ioreg-tree.txt          "device tree (-l -w0 -b)"         ioreg -l -w0 -b

# Targeted greps over the full dump — cheap, and makes the useful lines obvious
# without needing to open a 100k-line file.
hdr "Extracted highlights"
if [[ -s "$OUT/ioreg-full.txt" ]]; then
    grep -iE "UAD|Universal Audio|Apollo|Twin" "$OUT/ioreg-full.txt" \
        > "$OUT/highlight-ua-lines.txt" 2>/dev/null || true
    grep -iE "Number of (Input|Output) Channels|IOAudio.*Channel|SampleRate|ChannelCount|NumChannels" \
        "$OUT/ioreg-full.txt" > "$OUT/highlight-channels.txt" 2>/dev/null || true
    printf '  %-42s' "UA/Apollo lines"
    grn "$(wc -l <"$OUT/highlight-ua-lines.txt" 2>/dev/null | tr -d ' ') lines"
    printf '  %-42s' "channel-count lines"
    grn "$(wc -l <"$OUT/highlight-channels.txt" 2>/dev/null | tr -d ' ') lines"
fi

hdr "UA driver presence"
run kextstat.txt            "kextstat (UA entries)"     sh -c 'kextstat 2>/dev/null | grep -i "uaudio\|uad" || echo "no UA kext matched"'
run kexts-installed.txt     "installed UA kexts"        sh -c 'ls -la /Library/Extensions /System/Library/Extensions 2>/dev/null | grep -i "uad\|uaudio" || echo none'
run systemextensions.txt    "system extensions"         sh -c 'systemextensionsctl list 2>/dev/null || echo "not available"'

hdr "UA preferences (may carry a device channel map)"
# UA stores per-device configuration in plists. These sometimes contain the
# channel layout and device model config we are trying to confirm. Converted to
# xml1 so they are reviewable as text.
for d in "/Library/Preferences" "$HOME/Library/Preferences" \
         "/Library/Application Support/Universal Audio" \
         "$HOME/Library/Application Support/Universal Audio"; do
    [[ -d "$d" ]] || continue
    find "$d" -maxdepth 3 \( -iname "*uaudio*" -o -iname "*uad*" -o -iname "*apollo*" \) \
        -type f -print0 2>/dev/null | while IFS= read -r -d '' f; do
        safe="ua-pref-$(echo "$f" | tr '/ ' '__' | tail -c 120)"
        if [[ "$f" == *.plist ]]; then
            plutil -convert xml1 -o "$OUT/$safe.xml" "$f" 2>/dev/null \
                || cp "$f" "$OUT/$safe.bin" 2>/dev/null || true
        else
            cp "$f" "$OUT/$safe" 2>/dev/null || true
        fi
    done
done
printf '  %-42s' "UA preference files copied"
grn "$(find "$OUT" -name 'ua-pref-*' 2>/dev/null | wc -l | tr -d ' ')"

hdr "Packaging"
TARBALL="$OUT.tar.gz"
tar czf "$TARBALL" -C "$(dirname "$OUT")" "$(basename "$OUT")" 2>/dev/null \
    && grn "archive: $TARBALL" || ylw "tar failed; the directory is still there"

cat <<EOF

$(hdr "Done")
Captured to: $OUT
Archive:     $TARBALL

Nothing was uploaded. Nothing was modified. SIP was not touched.

REVIEW BEFORE SHARING. ioreg-full.txt is a whole-system dump and can include
device serial numbers and, depending on what is attached, other hardware
identifiers. The two highlight-*.txt files plus thunderbolt.json and audio.json
are usually all that is actually needed.

Most useful files to look at first:
  highlight-channels.txt   <- the channel counts (gap 2)
  highlight-ua-lines.txt   <- device identification
  thunderbolt.json         <- vendor/device IDs, link speed
  audio.json               <- CoreAudio's view of in/out channel counts

EOF
