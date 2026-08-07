#!/usr/bin/env bash
#
# twinx-probe-test.sh — safe first-contact test for Apollo Twin X.
#
# Loads the locally built ua_apollo.ko with probe_only=1, captures what the
# driver saw, and unloads it again. Nothing is installed: no DKMS, no
# /etc/modules-load.d, no initramfs regeneration, no PipeWire changes. The
# module is gone again by the time this script exits, and nothing it does
# survives a reboot.
#
# probe_only=1 means the driver maps BAR0, reads identity registers and
# stops — no IRQ, no firmware load, no DSP programs, no DMA programming,
# no ALSA card. See the probe_only comment in driver/ua_core.c.
#
# Usage:  sudo ./scripts/twinx-probe-test.sh
#
set -uo pipefail

MOD_NAME="ua_apollo"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
KO="$SCRIPT_DIR/../driver/$MOD_NAME.ko"
UA_TB_VENDOR="0x1176"

red()  { printf '\033[31m%s\033[0m\n' "$*"; }
grn()  { printf '\033[32m%s\033[0m\n' "$*"; }
ylw()  { printf '\033[33m%s\033[0m\n' "$*"; }
hdr()  { printf '\n\033[1m== %s ==\033[0m\n' "$*"; }

if [[ $EUID -ne 0 ]]; then
    red "Must run as root (insmod/rmmod). Try: sudo $0"
    exit 1
fi

# ---------------------------------------------------------------- preflight
hdr "Preflight"

if [[ ! -f "$KO" ]]; then
    red "Module not built: $KO"
    echo "Build it first:  cd $(dirname "$KO") && make"
    exit 1
fi
grn "module present: $KO"

if lsmod | grep -q "^${MOD_NAME}\b"; then
    red "$MOD_NAME is already loaded. Unload it first: sudo rmmod $MOD_NAME"
    exit 1
fi
grn "$MOD_NAME not currently loaded"

# Thunderbolt presence. Not fatal — we still want the diagnostic if absent.
tb_found=0
for d in /sys/bus/thunderbolt/devices/*/; do
    [[ -r "$d/vendor_name" ]] || continue
    if grep -qi "universal audio" "$d/vendor_name" 2>/dev/null; then
        tb_found=1
        echo "  thunderbolt: $(cat "$d/device_name" 2>/dev/null) at $(basename "$d")" \
             "authorized=$(cat "$d/authorized" 2>/dev/null)"
    fi
done
if [[ $tb_found -eq 1 ]]; then
    grn "Apollo present on the Thunderbolt bus"
else
    ylw "No Universal Audio device on the Thunderbolt bus — is it powered and connected?"
fi

# iommu=pt is the documented prerequisite: with Intel VT-d present, BAR0
# access is blocked without it.
if grep -qw 'iommu=pt' /proc/cmdline; then
    grn "iommu=pt present on kernel command line"
else
    ylw "iommu=pt NOT on the kernel command line"
    if [[ -r /sys/firmware/acpi/tables/DMAR ]] || ls /sys/firmware/acpi/tables/ 2>/dev/null | grep -q DMAR; then
        ylw "  ...and a DMAR table IS present (Intel VT-d active in firmware)."
        ylw "  BAR0 access is expected to fail. This is the known prerequisite."
    fi
fi

iommu_groups=$(ls /sys/kernel/iommu_groups 2>/dev/null | wc -l)
echo "  iommu groups: $iommu_groups"

# The decisive check: is there a PCIe endpoint for the driver to bind to?
# The Apollo must appear as a PCIe endpoint behind the Thunderbolt bridges.
# vendor 0x1176 = Universal Audio.
ep=$(lspci -nn 2>/dev/null | grep -i "$UA_TB_VENDOR:" || true)
if [[ -n "$ep" ]]; then
    grn "PCIe endpoint found:"
    echo "  $ep"
else
    red "NO PCIe endpoint for vendor $UA_TB_VENDOR in lspci."
    echo
    echo "  Without a PCIe endpoint, ua_probe() never runs and this test can"
    echo "  only confirm the module loads and unloads cleanly — it will not"
    echo "  reach the device. The Thunderbolt link being up is not sufficient;"
    echo "  the PCIe tunnel must also carry an endpoint."
    echo
    echo "  Continuing anyway to verify load/unload hygiene."
fi

# ---------------------------------------------------------------- load
hdr "Loading $MOD_NAME with probe_only=1"

dmesg_mark="probe-test-$$-$(date +%s)"
echo "$dmesg_mark" > /dev/kmsg 2>/dev/null || true

if ! insmod "$KO" probe_only=1; then
    red "insmod failed"
    dmesg | tail -30
    exit 1
fi
grn "loaded"

sleep 2

# ---------------------------------------------------------------- capture
hdr "Kernel log since load"
dmesg | sed -n "/$dmesg_mark/,\$p" | grep -vF "$dmesg_mark" || true

hdr "Driver binding"
if [[ -d /sys/module/$MOD_NAME/drivers ]]; then
    find /sys/module/$MOD_NAME/drivers -maxdepth 2 -type l 2>/dev/null \
        | while read -r l; do echo "  bound: $(basename "$l")"; done
fi
bound=$(ls /sys/bus/pci/drivers/$MOD_NAME/ 2>/dev/null | grep -E '^[0-9a-f]{4}:' || true)
if [[ -n "$bound" ]]; then
    grn "claimed PCI device(s): $bound"
else
    ylw "no PCI device claimed (expected if there is no PCIe endpoint)"
fi

hdr "Side-effect check (probe_only should have created nothing)"
if compgen -G "/dev/ua_apollo*" >/dev/null; then
    red "  UNEXPECTED: /dev/ua_apollo* exists — probe_only should not create a chardev"
    ls -l /dev/ua_apollo*
else
    grn "  no /dev/ua_apollo* chardev (correct)"
fi
if grep -qi apollo /proc/asound/cards 2>/dev/null; then
    red "  UNEXPECTED: an ALSA card was registered under probe_only"
    cat /proc/asound/cards
else
    grn "  no ALSA card registered (correct)"
fi

# ---------------------------------------------------------------- unload
hdr "Unloading"
if rmmod "$MOD_NAME"; then
    grn "unloaded cleanly"
else
    red "rmmod FAILED — module is stuck loaded."
    red "Do NOT reboot with a wedged module if you can avoid it; capture this:"
    dmesg | tail -40
    exit 1
fi

sleep 1
hdr "Post-unload log"
dmesg | sed -n "/$dmesg_mark/,\$p" | tail -20 || true

if lsmod | grep -q "^${MOD_NAME}\b"; then
    red "module still listed after rmmod"
    exit 1
fi

hdr "Result"
grn "Load/unload cycle completed with no module left resident."
echo "Nothing was installed. Nothing persists across reboot."
