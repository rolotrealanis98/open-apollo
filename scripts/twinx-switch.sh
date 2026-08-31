#!/usr/bin/env bash
#
# twinx-switch.sh — hand the Apollo cleanly between this Linux box, the Mac,
# and Windows.
#
#   status   what currently owns the device, and whether it is safe to unplug
#   release  quiesce and unload, so the cable can move to the Mac/PC
#   claim    load the driver for Linux use
#
# Why this is safe by construction: the module is NOT installed into
# /lib/modules, so `modprobe ua_apollo` cannot find it, and there are no
# modules-load.d, modprobe.d, udev or DKMS entries referencing it. Nothing on
# this host claims the Apollo unless you explicitly run `claim`. That is the
# property that makes switching seamless, and it is worth preserving —
# installing DKMS or an autoload rule would break it.
#
# WARNING: rmmod after full initialization is the documented brick path on the
# Apollo x4. The Thunderbolt link can drop and require a cold boot.
#
# Usage:  sudo ./scripts/twinx-switch.sh {status|release|claim} [extra insmod args]
#
set -uo pipefail

MOD=ua_apollo
KO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../driver" && pwd)/$MOD.ko"
UA_VENDOR=1a00

red() { printf '\033[31m%s\033[0m\n' "$*"; }
grn() { printf '\033[32m%s\033[0m\n' "$*"; }
ylw() { printf '\033[33m%s\033[0m\n' "$*"; }
hdr() { printf '\n\033[1m== %s ==\033[0m\n' "$*"; }

pci_addr() {
    lspci -Dnn 2>/dev/null | awk -v v="$UA_VENDOR" '$0 ~ "\\["v":" {print $1; exit}'
}

show_status() {
    local addr drv tb
    hdr "Apollo ownership"

    tb=""
    for d in /sys/bus/thunderbolt/devices/*/; do
        if grep -qi "universal audio" "$d/vendor_name" 2>/dev/null; then
            tb="$(cat "$d/device_name" 2>/dev/null) (auth=$(cat "$d/authorized" 2>/dev/null))"
        fi
    done
    [[ -n "$tb" ]] && grn "thunderbolt : $tb" || ylw "thunderbolt : no UA device — cable is elsewhere or unit is off"

    addr=$(pci_addr)
    if [[ -z "$addr" ]]; then
        ylw "pci         : no endpoint"
    else
        grn "pci         : $addr"
        if [[ -e "/sys/bus/pci/devices/$addr/driver" ]]; then
            drv=$(basename "$(readlink -f "/sys/bus/pci/devices/$addr/driver")")
            ylw "driver      : $drv  <-- Linux is holding the device"
        else
            grn "driver      : none (unclaimed)"
        fi
    fi

    if lsmod | grep -q "^${MOD}\b"; then
        ylw "module      : $MOD LOADED"
    else
        grn "module      : not loaded"
    fi

    # Anything that could grab it without being asked.
    hdr "Autoload check (all of these should be empty)"
    local bad=0
    for p in /etc/modules-load.d /usr/lib/modules-load.d /etc/modprobe.d \
             /etc/udev/rules.d /usr/lib/udev/rules.d; do
        if grep -rl "$MOD" "$p" 2>/dev/null | grep -q .; then
            red "  reference found in $p"; bad=1
        fi
    done
    find "/lib/modules/$(uname -r)" -name "$MOD*" 2>/dev/null | grep -q . && {
        red "  installed in /lib/modules — modprobe can find it"; bad=1; }
    command -v dkms >/dev/null && dkms status 2>/dev/null | grep -qi apollo && {
        red "  DKMS entry present"; bad=1; }
    [[ $bad -eq 0 ]] && grn "  clean — nothing can autoload this driver"

    hdr "Verdict"
    if lsmod | grep -q "^${MOD}\b"; then
        ylw "Linux owns the Apollo. Run 'release' before moving the cable."
    else
        grn "Linux is not touching the Apollo. Safe to unplug and move to Mac/PC."
    fi
}

do_release() {
    hdr "Releasing"
    if ! lsmod | grep -q "^${MOD}\b"; then
        grn "$MOD not loaded — already released"
    else
        # ua_remove() quiesces the device on the way out: it zeroes AX_CONTROL,
        # TRANSPORT and the IRQ enables, then waits 100ms for the firmware to
        # drain. That is what leaves the unit in a state the macOS/Windows
        # driver can re-initialise cleanly, so prefer rmmod over yanking the
        # cable while loaded.
        echo "unloading (this stops transport and disables interrupts)..."
        ylw "WARNING: rmmod after full initialization is the documented Apollo x4 brick path."
        ylw "The Thunderbolt link can drop and require a cold boot."
        if rmmod "$MOD"; then
            grn "unloaded cleanly"
        else
            red "rmmod FAILED — do not unplug yet"
            echo "Something still holds the device. Check for open PCM clients:"
            echo "  sudo fuser -v /dev/snd/* 2>/dev/null"
            echo "  systemctl --user stop pipewire.service wireplumber.service"
            exit 1
        fi
        sleep 1
    fi

    local addr; addr=$(pci_addr)
    if [[ -n "$addr" && -e "/sys/bus/pci/devices/$addr/driver" ]]; then
        red "device still has a driver bound — not safe"; exit 1
    fi

    hdr "Ready"
    grn "Unplug the Thunderbolt cable and move it to the Mac or PC."
    echo "On the Mac, UAD Console should pick it up normally; it re-initialises"
    echo "the DSP itself, so no extra step is needed there."
}

do_claim() {
    hdr "Claiming for Linux"
    [[ -f "$KO" ]] || { red "not built: $KO"; exit 1; }
    lsmod | grep -q "^${MOD}\b" && { ylw "already loaded"; exit 0; }

    local addr; addr=$(pci_addr)
    [[ -n "$addr" ]] || { red "no UA PCIe endpoint — is the cable here and the unit on?"; exit 1; }

    ylw "Reminder: turn monitor level down and unplug headphones."
    ylw "Routing is not yet mapped for this model, so output levels are not trustworthy."
    echo
    echo "insmod $KO ${*:-}"
    if insmod "$KO" "$@"; then
        grn "loaded"
        sleep 2
        cat /proc/asound/cards 2>/dev/null | grep -iE "apollo|ua_" || ylw "(no Apollo ALSA card yet)"
    else
        red "insmod failed"; exit 1
    fi
}

[[ $# -ge 1 ]] || { echo "usage: $0 {status|release|claim} [insmod args]"; exit 1; }
cmd=$1; shift

case "$cmd" in
    status)  show_status ;;
    release) [[ $EUID -eq 0 ]] || { red "run as root"; exit 1; }; do_release ;;
    claim)   [[ $EUID -eq 0 ]] || { red "run as root"; exit 1; }; do_claim "$@" ;;
    *)       red "unknown: $cmd"; echo "usage: $0 {status|release|claim}"; exit 1 ;;
esac
