#!/usr/bin/env bash
#
# twinx-dma-trace.sh — step 3: normal load with connect and plugins disabled.
#
# Purpose: exercise the register-programming and ring/DMA-init path and capture
# the DMA_CTRL trace, to test whether the per-channel reset strobes [16:9] latch
# high on this device. That is the x8p symptom in upstream issue #46 (SAMPLE_POS
# and FRAME_CTR frozen at 0 with DMA_CTRL retaining 0x1FE00).
#
# What is deliberately NOT done:
#   no_connect=1  -> no ACEFACE connect handshake
#   no_plugins=1  -> no plugin chain submitted
#   twinx_dsp     -> left off, so no DSP program blocks are submitted
#
# Installs nothing. No DKMS, no modules-load.d, no initramfs, no PipeWire
# changes. Always attempts rmmod, including on failure.
#
# BEFORE RUNNING: turn the monitor level down and unplug headphones. A wrong
# routing/channel map cannot hurt the Apollo but can send full-scale digital to
# the outputs.
#
# Usage:  sudo ./scripts/twinx-dma-trace.sh
#
set -uo pipefail

MOD=ua_apollo
KO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../driver" && pwd)/$MOD.ko"

red() { printf '\033[31m%s\033[0m\n' "$*"; }
grn() { printf '\033[32m%s\033[0m\n' "$*"; }
ylw() { printf '\033[33m%s\033[0m\n' "$*"; }
hdr() { printf '\n\033[1m== %s ==\033[0m\n' "$*"; }

[[ $EUID -eq 0 ]] || { red "run as root"; exit 1; }
[[ -f "$KO" ]]     || { red "not built: $KO"; exit 1; }
lsmod | grep -q "^${MOD}\b" && { red "$MOD already loaded; rmmod first"; exit 1; }

hdr "Preflight"
ep=$(lspci -nn 2>/dev/null | grep -i "1a00:" || true)
[[ -n "$ep" ]] || { red "no UA PCIe endpoint present — nothing to probe"; exit 1; }
grn "endpoint: $ep"
grep -qw iommu=pt /proc/cmdline && grn "iommu=pt present (rogue DMA will fault, not corrupt)" \
    || ylw "iommu=pt ABSENT — rogue DMA would hit physical RAM directly"
echo "iommu groups: $(ls /sys/kernel/iommu_groups 2>/dev/null | wc -l)"

# Flush filesystem buffers: cheap insurance if DMA misbehaves with no IOMMU.
hdr "Syncing filesystems"
sync; sync; grn "synced"

MARK="dma-trace-$$"
echo "$MARK" > /dev/kmsg 2>/dev/null || true

hdr "Loading: no_connect=1 no_plugins=1"
if ! insmod "$KO" no_connect=1 no_plugins=1; then
    red "insmod failed"
    dmesg | sed -n "/$MARK/,\$p" | tail -40
    exit 1
fi
grn "loaded"
sleep 3

hdr "DMA_CTRL trace (the hypothesis under test)"
dmesg | sed -n "/$MARK/,\$p" | grep -iE "DMA_CTRL|reset|strobe" || ylw "(no DMA_CTRL lines emitted)"

hdr "Transport / position registers"
dmesg | sed -n "/$MARK/,\$p" | grep -iE "SAMPLE_POS|FRAME_CTR|transport|SEQ_WR|SEQ_RD|pos=" || ylw "(none)"

hdr "Errors and warnings"
dmesg | sed -n "/$MARK/,\$p" | grep -iE "error|fail|warn|timeout|-ENODATA|unable" || grn "(none)"

hdr "ALSA card"
cat /proc/asound/cards 2>/dev/null | grep -iE "apollo|ua_" || ylw "(no Apollo ALSA card)"

hdr "Full log for this load"
dmesg | sed -n "/$MARK/,\$p" | grep -vF "$MARK"

hdr "Unloading"
if rmmod "$MOD"; then
    grn "unloaded cleanly"
else
    red "rmmod FAILED — module stuck resident"
    dmesg | tail -30
    exit 1
fi
sleep 1
dmesg | sed -n "/$MARK/,\$p" | tail -8

lsmod | grep -q "^${MOD}\b" && { red "still resident"; exit 1; }
hdr "Result"
grn "load/unload completed, module not resident"
