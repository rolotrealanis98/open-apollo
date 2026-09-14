#!/usr/bin/env bash
#
# dev-module.sh — inspect, test, claim, or release the local Apollo module.
#
#   probe    load with probe_only=1, report identity, and unload
#   trace    load with connect and plugins disabled, report DMA state, and unload
#   status   report current ownership and detected autoload configuration
#   release  quiesce and unload so the cable can move to another host
#   claim    load the driver for Linux use
#
# Manual ownership applies only to insmod workflows without install.sh,
# installed modules, DKMS, or autoload rules. The default installer enables
# DKMS and allows automatic binding at boot. claim/release check for this.
#
# Project rule: "NEVER rmmod ua_apollo". After full initialization it can kill
# the Thunderbolt link on an x4, requiring a cold boot to recover.
# trace/release require --allow-unsafe-unload to acknowledge this hazard.
# probe unloads only its own probe_only=1 load.
#
# Usage:
#   sudo ./scripts/dev-module.sh probe
#   ./scripts/dev-module.sh status
#   sudo ./scripts/dev-module.sh {trace|release} --allow-unsafe-unload
#   sudo ./scripts/dev-module.sh claim [--force]
# claim uses apollo-init.sh without daemon or PipeWire setup; --force bypasses
# its stalled/frozen DSP gate but does not bypass failed reads or firmware errors.
#
set -uo pipefail

MOD=ua_apollo
PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
KO="$PROJECT_DIR/driver/$MOD.ko"

red() { printf '\033[31m%s\033[0m\n' "$*"; }
grn() { printf '\033[32m%s\033[0m\n' "$*"; }
ylw() { printf '\033[33m%s\033[0m\n' "$*"; }
hdr() { printf '\n\033[1m== %s ==\033[0m\n' "$*"; }

# Match the Apollo by vendor and device ID, as scripts/install.sh does:
# vendor 1a00 alone would also select UAD-2 PCIe DSP cards.
pci_addr() {
    lspci -D -d 1a00:0002 2>/dev/null | awk 'NR == 1 { print $1 }'
}

# PCI devices bound to the driver. The driver directory also holds a
# "module" symlink, so only domain:bus:slot.function names count.
bound_devices() {
    find "/sys/bus/pci/drivers/$MOD" -maxdepth 1 -type l -name '*:*' -printf '%f\n' 2>/dev/null || true
}

require_root() {
    [[ $EUID -eq 0 ]] || { red "run as root"; exit 1; }
}

require_built_and_unloaded() {
    [[ -f "$KO" ]] || { red "not built: $KO"; exit 1; }
    grep -q "^${MOD} " /proc/modules && { red "$MOD is already loaded"; exit 1; }
}

unload_probe_only() {
    if rmmod "$MOD"; then
        grn "unloaded cleanly"
    else
        red "rmmod failed; module is still loaded"
        dmesg | tail -40
        exit 1
    fi
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

    if grep -q "^${MOD} " /proc/modules; then
        ylw "module      : $MOD LOADED"
    else
        grn "module      : not loaded"
    fi

    local autoload=0
    check_autoload || autoload=1

    hdr "Verdict"
    if grep -q "^${MOD} " /proc/modules; then
        ylw "Linux owns the Apollo. Full-mode unload can break the Thunderbolt link."
    elif [[ $autoload -ne 0 ]]; then
        ylw "Module absent, but autoload may reclaim the Apollo when it appears."
    else
        grn "Module absent; no autoload references found."
    fi
}

check_autoload() {
    hdr "Autoload check (all of these should be empty)"
    local bad=0 output rc
    for p in /etc/modules-load.d /usr/lib/modules-load.d /etc/modprobe.d \
             /etc/udev/rules.d /usr/lib/udev/rules.d; do
        [[ -d "$p" ]] || continue
        # Capture all output: grep -q would close the pipe early and make
        # a matching producer fail with SIGPIPE under pipefail.
        output=$(grep -rl "$MOD" "$p" 2>/dev/null)
        rc=$?
        if [[ -n "$output" ]]; then
            red "  reference found in $p"; bad=1
        fi
        if [[ $rc -gt 1 ]]; then
            ylw "  could not inspect $p; autoload state is unknown"; bad=1
        fi
    done
    output=$(find "/lib/modules/$(uname -r)" -name "$MOD*" 2>/dev/null)
    rc=$?
    if [[ -n "$output" ]]; then
        red "  installed in /lib/modules — modprobe can find it"; bad=1
    fi
    if [[ $rc -ne 0 ]]; then
        ylw "  could not inspect installed modules; autoload state is unknown"; bad=1
    fi
    if command -v dkms >/dev/null; then
        output=$(dkms status 2>/dev/null)
        rc=$?
        if [[ ${output,,} == *apollo* ]]; then
            red "  DKMS entry present"; bad=1
        fi
        if [[ $rc -ne 0 ]]; then
            ylw "  could not inspect DKMS; autoload state is unknown"; bad=1
        fi
    fi
    if [[ $bad -eq 0 ]]; then
        grn "  no autoload references found"
    fi
    return "$bad"
}

do_probe() {
    require_built_and_unloaded

    hdr "Probe preflight"
    local addr mark bound
    addr=$(pci_addr)
    [[ -n "$addr" ]] && grn "PCIe endpoint: $addr" || ylw "no Apollo PCIe endpoint found"
    grep -qw iommu=pt /proc/cmdline && grn "iommu=pt present" || ylw "iommu=pt absent"

    mark="probe-test-$$-$(date +%s)"
    echo "$mark" > /dev/kmsg 2>/dev/null || true
    hdr "Loading with probe_only=1"
    if ! insmod "$KO" probe_only=1; then
        red "insmod failed"
        dmesg | tail -30
        exit 1
    fi
    grn "loaded"
    sleep 2

    hdr "Kernel log since load"
    dmesg | sed -n "/$mark/,\$p" | grep -vF "$mark" || true

    hdr "Probe side effects"
    bound=$(bound_devices)
    [[ -n "$bound" ]] && grn "claimed PCI device: $bound" || ylw "no PCI device claimed"
    compgen -G '/dev/ua_apollo*' >/dev/null && { red "unexpected character device"; unload_probe_only; exit 1; }
    grep -qi apollo /proc/asound/cards 2>/dev/null && { red "unexpected ALSA card"; unload_probe_only; exit 1; }
    grn "no character device or ALSA card created"

    hdr "Unloading probe-only module"
    unload_probe_only
    sleep 1
    grep -q "^${MOD} " /proc/modules && { red "module is still resident"; exit 1; }
    grn "probe cycle completed with no module left resident"
}

do_trace() {
    require_built_and_unloaded

    hdr "Trace preflight"
    local addr mark
    addr=$(pci_addr)
    [[ -n "$addr" ]] || { red "no Apollo PCIe endpoint found"; exit 1; }
    grn "PCIe endpoint: $addr"
    grep -qw iommu=pt /proc/cmdline && grn "iommu=pt present" || ylw "iommu=pt absent; rogue DMA can reach physical memory"
    sync

    mark="dma-trace-$$-$(date +%s)"
    echo "$mark" > /dev/kmsg 2>/dev/null || true
    hdr "Loading with no_connect=1 no_plugins=1"
    if ! insmod "$KO" no_connect=1 no_plugins=1; then
        red "insmod failed"
        dmesg | sed -n "/$mark/,\$p" | tail -40
        exit 1
    fi
    grn "loaded"
    sleep 3

    hdr "DMA control trace"
    dmesg | sed -n "/$mark/,\$p" | grep -iE 'DMA_CTRL|reset|strobe' || ylw "no DMA control lines"
    hdr "Transport and position registers"
    dmesg | sed -n "/$mark/,\$p" | grep -iE 'SAMPLE_POS|FRAME_CTR|transport|SEQ_WR|SEQ_RD|pos=' || ylw "no transport lines"
    hdr "Errors and warnings"
    dmesg | sed -n "/$mark/,\$p" | grep -iE 'error|fail|warn|timeout|-ENODATA|unable' || grn "none"
    hdr "ALSA card"
    grep -iE 'apollo|ua_' /proc/asound/cards 2>/dev/null || ylw "no Apollo ALSA card"

    hdr "Unloading fully initialized module"
    ylw "WARNING: rmmod after full initialization is the documented Apollo x4 brick path."
    ylw "The Thunderbolt link can drop and require a cold boot."
    if rmmod "$MOD"; then
        grn "unloaded cleanly"
    else
        red "rmmod failed; module is still loaded"
        dmesg | tail -30
        exit 1
    fi
    sleep 1
    dmesg | sed -n "/$mark/,\$p" | tail -8
    grep -q "^${MOD} " /proc/modules && { red "module is still resident"; exit 1; }
    grn "trace cycle completed with no module left resident"
}

do_release() {
    check_autoload || ylw "WARNING: autoload can reclaim the Apollo after release."
    hdr "Releasing"
    if ! grep -q "^${MOD} " /proc/modules; then
        grn "$MOD not loaded — already released"
    else
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
    check_autoload || ylw "WARNING: this host can claim the Apollo automatically."
    ylw "Reminder: turn monitor level down and unplug headphones."
    ylw "Routing is not yet mapped for this model, so output levels are not trustworthy."
    # The existing initializer diagnoses after loading, before firmware replay.
    # Keep its stalled/frozen checks and exit status, without changing the
    # user's audio services, profiles, or default output.
    APOLLO_SKIP_PIPEWIRE=1 bash "$PROJECT_DIR/tools/apollo-init.sh" --no-daemon "$@"
}

usage() {
    echo "usage: $0 {probe|status|claim [--force]|trace|release}"
    echo "trace/release require --allow-unsafe-unload."
    echo 'Project rule: "NEVER rmmod ua_apollo".'
    echo "Full-mode unload can kill the x4 Thunderbolt link and require a cold boot."
}

main() {
    [[ $# -ge 1 ]] || { usage; return 1; }
    local cmd=$1
    shift

    case "$cmd" in
        trace|release)
            if [[ $# -ne 1 || $1 != --allow-unsafe-unload ]]; then
                usage
                return 1
            fi
            require_root
            "do_$cmd"
            ;;
        probe|status)
            [[ $# -eq 0 ]] || { usage; return 1; }
            if [[ $cmd == probe ]]; then
                require_root
                do_probe
            else
                show_status
            fi
            ;;
        claim)
            [[ $# -eq 0 || ( $# -eq 1 && $1 == --force ) ]] || { usage; return 1; }
            require_root
            do_claim "$@"
            ;;
        *) usage; return 1 ;;
    esac
}

if [[ ${BASH_SOURCE[0]} == "$0" ]]; then
    main "$@"
fi
