#!/bin/bash
# apollo-report.sh - read-only diagnostic collector for Open Apollo testers
#
# Writes ONE text file you can review and attach to a bug report.  Nothing is
# written to the hardware or to the driver; the only hardware access is three
# read-only ioctls (GET_DEVICE_INFO, GET_SYSTEM_INFO, GET_HW_READBACK).
#
# Run it as your normal user.  sudo is only used if you pass --sudo, for
# things Linux hides from unprivileged users on some distros:
#   - lspci -vv  (link status / ASPM capability registers show
#                 "<access denied>" without root)
#   - dmesg      (kernel.dmesg_restrict=1 on Ubuntu and others)
#
# Privacy: no hostname, username, serial numbers (UA serial, PCI/Thunderbolt
# device serials) or network information are collected.  Read the file
# before sending it.
#
# Usage:
#   tools/apollo-report.sh [--out FILE] [--lines N] [--sudo]

set -u

OUT=""
DMESG_LINES=300
USE_SUDO=0

usage() {
    sed -n '2,/^$/p' "$0" | sed 's/^# \{0,1\}//'
    exit "${1:-0}"
}

while [ $# -gt 0 ]; do
    case "$1" in
        --out)   OUT="${2:?--out needs a file}"; shift 2 ;;
        --lines) DMESG_LINES="${2:?--lines needs a number}"; shift 2 ;;
        --sudo)  USE_SUDO=1; shift ;;
        -h|--help) usage 0 ;;
        *) echo "unknown option: $1" >&2; usage 1 ;;
    esac
done

case "$DMESG_LINES" in ''|*[!0-9]*) echo "--lines must be a number" >&2; exit 1 ;; esac

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
[ -n "$OUT" ] || OUT="apollo-report-$(date +%Y%m%d-%H%M%S).txt"

: > "$OUT" || { echo "cannot write $OUT" >&2; exit 1; }

have() { command -v "$1" >/dev/null 2>&1; }

priv() {
    if [ "$USE_SUDO" = 1 ] && [ "$(id -u)" != 0 ]; then
        sudo "$@"
    else
        "$@"
    fi
}

section() { printf '\n===== %s =====\n' "$1" >> "$OUT"; }
note()    { printf '%s\n' "$*" >> "$OUT"; }

# run CMD... : append "$ cmd" then its stdout+stderr, never abort the script
run() {
    printf '$ %s\n' "$*" >> "$OUT"
    "$@" >> "$OUT" 2>&1 || printf '(exit status %s)\n' "$?" >> "$OUT"
}

echo "Collecting into $OUT ..."

section "report"
note "tool:      apollo-report.sh"
note "date_utc:  $(date -u +%Y-%m-%dT%H:%M:%SZ)"
note "sudo:      $([ "$USE_SUDO" = 1 ] && echo yes || echo no)"

# ---------------------------------------------------------------- system
section "kernel and distro"
run uname -srvm
if [ -r /etc/os-release ]; then
    grep -E '^(PRETTY_NAME|NAME|VERSION|VERSION_ID|ID|ID_LIKE|BUILD_ID)=' \
        /etc/os-release >> "$OUT"
fi
if [ -r /proc/cmdline ]; then
    note "cmdline (only PCIe/Thunderbolt/IOMMU related options):"
    tr ' ' '\n' < /proc/cmdline \
        | grep -E '^(pcie_|pci=|thunderbolt|iommu|intel_iommu|amd_iommu|usbcore|nvme)' \
        >> "$OUT" || note "  (none)"
fi
have lsmod && { printf '$ lsmod | grep -E "ua_apollo|thunderbolt|snd_pcm"\n' >> "$OUT"
    lsmod | grep -E '^(ua_apollo|thunderbolt|snd_pcm) ' >> "$OUT" 2>&1; }

# ---------------------------------------------------------------- module
section "ua_apollo module"
if [ -d /sys/module/ua_apollo ]; then
    for f in version srcversion refcnt initstate taint; do
        [ -r "/sys/module/ua_apollo/$f" ] &&
            note "$f: $(cat "/sys/module/ua_apollo/$f" 2>/dev/null)"
    done
    note "parameters:"
    if [ -d /sys/module/ua_apollo/parameters ]; then
        for p in /sys/module/ua_apollo/parameters/*; do
            [ -r "$p" ] && note "  $(basename "$p") = $(cat "$p" 2>/dev/null)"
        done
    fi
else
    note "ua_apollo is NOT loaded (/sys/module/ua_apollo missing)"
fi
if have modinfo; then
    printf '$ modinfo ua_apollo (version, srcversion, vermagic, depends)\n' >> "$OUT"
    modinfo ua_apollo 2>&1 | grep -E '^(version|srcversion|vermagic|depends|description):|not found' >> "$OUT"
    note "signer: $(modinfo -F signer ua_apollo 2>/dev/null | head -1 | sed 's/^$/(none - unsigned)/')"
fi
if have mokutil; then
    printf '$ mokutil --sb-state\n' >> "$OUT"
    mokutil --sb-state >> "$OUT" 2>&1
    if mokutil --sb-state 2>/dev/null | grep -qi 'enabled' \
        && modinfo -n ua_apollo >/dev/null 2>&1 && [ -z "$(modinfo -F signer ua_apollo 2>/dev/null)" ]; then
        note "NOTE: Secure Boot is on and the module is unsigned - the kernel will reject it."
        note "      Run scripts/apollo-hostcheck.sh sb for the MOK enrollment steps."
    fi
fi

# ----------------------------------------------------------------- PCI
section "PCI: Apollo and Thunderbolt bridges"
APOLLO_ADDRS=""
if have lspci; then
    run lspci -nn -d 1a00:
    APOLLO_ADDRS="$(lspci -D -d 1a00: 2>/dev/null | awk '{print $1}')"
else
    note "lspci not installed (pciutils); install it to include link status"
fi

# lspci -vv hides the serial-number capability line; strip it anyway.
lspci_vv() {
    priv lspci -vv -s "$1" 2>&1 | grep -v -i 'Device Serial Number'
}

BRIDGES=""
for addr in $APOLLO_ADDRS; do
    path="$(readlink -f "/sys/bus/pci/devices/$addr" 2>/dev/null)"
    # every PCI address component on the sysfs path above the device
    chain="$(printf '%s\n' "$path" | tr '/' '\n' \
        | grep -E '^[0-9a-f]{4}:[0-9a-f]{2}:[0-9a-f]{2}\.[0-7]$')"
    for a in $chain; do
        [ "$a" = "$addr" ] || BRIDGES="$BRIDGES $a"
    done
    note ""
    note "-- Apollo $addr, upstream chain (root port first):"
    for a in $chain; do note "   $a"; done
done
if [ -n "$APOLLO_ADDRS" ] && have lspci; then
    for addr in $APOLLO_ADDRS; do
        note ""
        note "-- lspci -vv -s $addr (Apollo)"
        lspci_vv "$addr" >> "$OUT"
    done
    for a in $(printf '%s\n' "$BRIDGES" | tr ' ' '\n' | awk 'NF && !seen[$0]++'); do
        note ""
        note "-- lspci -vv -s $a (upstream bridge)"
        lspci_vv "$a" >> "$OUT"
    done
elif have lspci; then
    note "no PCI device with vendor 1a00 found - is the Apollo on and connected?"
    run lspci -nn
fi
if grep -q 'access denied' "$OUT" 2>/dev/null && [ "$USE_SUDO" != 1 ]; then
    note ""
    note "NOTE: lspci hid some registers (<access denied>). Re-run with --sudo to include them."
fi

section "Thunderbolt devices (no serials)"
if [ -d /sys/bus/thunderbolt/devices ]; then
    for d in /sys/bus/thunderbolt/devices/*; do
        [ -d "$d" ] || continue
        line="$(basename "$d")"
        for f in security vendor_name device_name authorized generation rx_speed rx_lanes tx_speed tx_lanes; do
            [ -r "$d/$f" ] && line="$line $f=$(cat "$d/$f" 2>/dev/null)"
        done
        note "$line"
        [ "$(cat "$d/authorized" 2>/dev/null)" = "0" ] && UNAUTH_TB=1
    done
    if [ "${UNAUTH_TB:-0}" = 1 ]; then
        note "NOTE: a Thunderbolt device is NOT authorized (authorized=0); an Apollo behind it will not"
        note "      appear on PCIe.  Run scripts/apollo-hostcheck.sh tb locally for the boltctl command"
        note "      (it contains the device uuid, which this report leaves out)."
    fi
else
    note "no /sys/bus/thunderbolt (kernel without thunderbolt support, or none attached)"
fi
if have boltctl; then
    printf '$ boltctl list (name, type, status, authflags only)\n' >> "$OUT"
    boltctl list 2>&1 | grep -E '^\s*[|├└●○ ]*[-├└]*\s*(name|type|status|authflags|generation|policy):' >> "$OUT"
fi

# ---------------------------------------------------------------- ALSA
section "ALSA"
run cat /proc/asound/cards
CARD=""
if [ -r /proc/asound/cards ]; then
    CARD="$(awk '/^ *[0-9]+ \[/ && /ua_apollo/ {print $1; exit}' /proc/asound/cards)"
fi
if [ -n "$CARD" ]; then
    note ""
    note "Apollo is ALSA card $CARD"
    section "ALSA card $CARD: io_map"
    if [ -r "/proc/asound/card$CARD/io_map" ]; then
        run cat "/proc/asound/card$CARD/io_map"
    else
        note "no io_map entry (driver older than v2)"
    fi
    section "ALSA card $CARD: dsp_state (DSP window, pool and ring registers; read only)"
    if [ -r "/proc/asound/card$CARD/dsp_state" ]; then
        run cat "/proc/asound/card$CARD/dsp_state"
    else
        note "no dsp_state entry (driver without the DSP diagnostics)"
    fi
    section "ALSA card $CARD: mixer controls (amixer -c $CARD contents)"
    if have amixer; then
        run amixer -c "$CARD" contents
    else
        note "amixer not installed (alsa-utils)"
    fi
    section "ALSA card $CARD: PCM streams"
    for f in /proc/asound/card"$CARD"/pcm*/info; do
        [ -r "$f" ] && { note "-- $f"; grep -E '^(id|name|subdevices_count|subdevices_avail)' "$f" >> "$OUT"; }
    done
else
    note "no ua_apollo ALSA card found"
fi

# --------------------------------------------------------------- dmesg
section "kernel log: ua_apollo (last $DMESG_LINES matching lines)"
KLOG=""
if KLOG="$(priv dmesg 2>/dev/null)" && [ -n "$KLOG" ]; then
    :
elif have journalctl && KLOG="$(journalctl -k -b --no-pager 2>/dev/null)" && [ -n "$KLOG" ]; then
    note "(from journalctl -k -b, dmesg is restricted)"
else
    KLOG=""
fi
if [ -n "$KLOG" ]; then
    printf '%s\n' "$KLOG" | grep -i 'ua_apollo' | tail -n "$DMESG_LINES" >> "$OUT"
else
    note "kernel log not readable as this user. Re-run with --sudo, or:"
    note "  sudo dmesg | grep -i ua_apollo | tail -n $DMESG_LINES"
fi

# --------------------------------------------------------- ioctl readback
section "device info and hardware readback (read-only ioctls)"
NODES=""
for n in /dev/ua_apollo[0-9]*; do [ -e "$n" ] && NODES="$NODES $n"; done
if [ -z "$NODES" ]; then
    note "no /dev/ua_apollo* node"
elif ! have python3; then
    note "python3 not installed"
else
    # Model name from the driver's io_map header (empty on an older driver)
    MODEL=""
    if [ -n "$CARD" ] && [ -r "/proc/asound/card$CARD/io_map" ]; then
        MODEL="$(awk '/^model[ \t]/ { sub(/^model[ \t]+/, ""); sub(/ \(device type.*$/, ""); print; exit }' \
            "/proc/asound/card$CARD/io_map")"
    fi
    for n in $NODES; do
        note "-- $n"
        python3 - "$n" "$MODEL" >> "$OUT" 2>&1 <<'PY'
import fcntl, os, struct, sys, time

def ior(nr, size):
    return (2 << 30) | (size << 16) | (ord("U") << 8) | nr

GET_DEVICE_INFO = ior(0x01, 64)    # struct ua_device_info: 16 x u32
GET_SYSTEM_INFO = ior(0x05, 148)   # struct ua_system_info (ABI 1.x)
GET_HW_READBACK = ior(0x41, 164)   # struct ua_hw_readback: status + 40 x u32

try:
    fd = os.open(sys.argv[1], os.O_RDWR)
except OSError as e:
    print("cannot open %s: %s" % (sys.argv[1], e))
    if e.errno == 13:
        print("(permission: the udev rule 70-ua-apollo-access.rules gives the "
              "desktop user and the audio group access; re-run as a user in "
              "the 'audio' group or with sudo)")
    sys.exit(0)

try:
    buf = bytearray(64)
    fcntl.ioctl(fd, GET_DEVICE_INFO, buf)
    f = struct.unpack("<16I", bytes(buf))
    names = ("fpga_rev", "device_type", "subsystem_id", "num_dsps",
             "fw_version", "fw_v2", "serial", "ext_caps", "public_type")
    for k, v in zip(names, f):
        if k == "serial":
            print("  serial        (omitted for privacy; %s)"
                  % ("non-zero" if v else "zero"))
        else:
            print("  %-13s 0x%08x (%d)" % (k, v, v))
except OSError as e:
    print("GET_DEVICE_INFO failed: %s" % e)

# GET_SYSTEM_INFO (driver ABI 1.x with UA_FEAT_SYSTEM_INFO): model identity.
# The serial and hardware uid in it are not printed.
try:
    buf = bytearray(148)
    fcntl.ioctl(fd, GET_SYSTEM_INFO, buf)
    w = struct.unpack("<12I", bytes(buf[:48]))
    (s_fpga, s_dtype, s_public, s_subsys, s_pci, s_dsps,
     s_fw, s_fw2, s_ext, _uid_lo, _uid_hi, s_kind) = w
    tail = struct.unpack("<13I", bytes(buf[64:116]))
    print("  system info:")
    print("    device_type   0x%02x   public_type 0x%08x%s"
          % (s_dtype, s_public, " (unresolved)" if s_public == 0xFFFFFFFF else ""))
    print("    mixer_kind    0x%08x%s"
          % (s_kind, " (none/ungated)" if s_kind == 0xFFFFFFFF else ""))
    print("    ext_caps      0x%08x" % s_ext)
    print("    pci_device    0x%04x  subsystem 0x%04x  dsps %d  fw_v2 %d"
          % (s_pci, s_subsys, s_dsps, s_fw2))
    print("    channels      play %d / rec %d" % (tail[7], tail[8]))
except OSError as e:
    print("GET_SYSTEM_INFO not available: %s (older driver; public_type and "
          "ext_caps above come from GET_DEVICE_INFO)" % e)

data = None
status = None
try:
    for _ in range(40):
        buf = bytearray(164)
        fcntl.ioctl(fd, GET_HW_READBACK, buf)
        words = struct.unpack("<41I", bytes(buf))
        status, data = words[0], list(words[1:])
        if status == 1:
            break
        time.sleep(0.05)
except OSError as e:
    print("GET_HW_READBACK failed: %s" % e)
finally:
    os.close(fd)

if data is None:
    sys.exit(0)
print("hw readback status=%d%s" % (status, "" if status == 1 else
      " (not valid after retries; decoded values below are not trustworthy)"))
print("  raw data[0..7]: " + " ".join("%08x" % w for w in data[:8]))
if status != 1:
    sys.exit(0)

# Bit layout: mixer-engine/hardware.py poll_hw_readback
d0, d2, d3, d6, d7 = data[0], data[2], data[3], data[6], data[7]
model = sys.argv[2] if len(sys.argv) > 2 else ""
if model == "Apollo x4":
    print("  decoded (Apollo x4 layout):")
else:
    print("  decoded with the Apollo x4 bit layout (this is %s; the bit "
          "positions are only hardware-confirmed on the x4, treat as raw):"
          % (model or "an unidentified model"))
b = [(d3 >> (8 * i)) & 0xFF for i in range(4)]
gc = [b[0] & 0x3F, ((b[1] & 0x1F) << 2) | (b[0] >> 6),
      b[2] & 0x3F, ((b[3] & 0x0F) << 2) | (b[2] >> 6)]
for ch in range(4):
    base = ch * 6
    flags = [n for i, n in enumerate(
        ("mic/line", "pad", "link", "48V", "lowcut", "phase"))
        if d0 & (1 << (base + i))]
    print("    preamp %d: gain %d dB (raw %d), flags: %s"
          % (ch + 1, min(65, max(10, gc[ch] + 9)), gc[ch],
             ", ".join(flags) or "none"))
print("    HiZ jack detect: in1=%s in2=%s"
      % (bool(d0 & (1 << 24)), bool(d0 & (1 << 25))))
mon = d2 & 0xFF
print("    monitor: level raw %d (%.1f dB), mute=%s mono=%s dim=%s"
      % (mon, (mon - 192) / 2.0, bool(d2 & (1 << 16)),
         bool(d2 & (1 << 17)), bool(d2 & (1 << 31))))
print("    headphone 1 raw %d, headphone 2 raw %d"
      % ((d2 >> 8) & 0xFF, d7 & 0xFF))
print("    talkback active=%s" % bool(d6 & (1 << 8)))
if d6 & (1 << 30):
    print("    clock locked=%s" % bool(d6 & (1 << 31)))
else:
    print("    clock lock: not reported")
PY
    done
fi

# ----------------------------------------------------- PipeWire/WirePlumber
section "PipeWire / WirePlumber"
if have pw-dump; then
    if pw-dump >/dev/null 2>&1; then
        run pw-cli --version
        if have python3; then
            pw-dump 2>/dev/null | python3 -c '
import json, sys
try:
    objs = json.load(sys.stdin)
except Exception as e:
    print("pw-dump output not parseable:", e); sys.exit(0)
def p(o): return (o.get("info") or {}).get("props") or {}
hit = 0
for o in objs:
    props = p(o)
    blob = " ".join(str(v) for v in props.values()).lower()
    if "ua_apollo" not in blob and "apollo" not in blob:
        continue
    t = o.get("type", "").split(":")[-1]
    if t not in ("Node", "Device"):
        continue
    hit += 1
    keys = ("node.name", "node.description", "media.class", "audio.rate",
            "audio.channels", "audio.format", "clock.quantum-limit",
            "api.alsa.path", "device.name", "device.api", "device.profile.name",
            "api.alsa.disable-mmap", "api.alsa.soft-mixer",
            "session.suspend-timeout-seconds", "node.pause-on-idle")
    print("%s id=%s" % (t, o.get("id")))
    for k in keys:
        if k in props:
            print("    %s = %s" % (k, props[k]))
    if t == "Device":
        pr = (o.get("info") or {}).get("params", {}).get("Profile") or []
        for x in pr:
            print("    active profile: %s" % x.get("name"))
if not hit:
    print("no Apollo nodes/devices in the PipeWire graph")
' >> "$OUT" 2>&1
        else
            note "python3 missing, skipping pw-dump filter"
        fi
    else
        note "pw-dump cannot reach a PipeWire daemon (not running, or run as a different user)"
    fi
else
    note "pw-dump not installed (PipeWire not present)"
fi
if have wpctl; then
    run wpctl --version
fi

note ""
note "WirePlumber Apollo rule files found:"
found_wp=0
for f in /etc/wireplumber/wireplumber.conf.d/*apollo* \
         "${XDG_CONFIG_HOME:-$HOME/.config}"/wireplumber/wireplumber.conf.d/*apollo* \
         /etc/wireplumber/main.lua.d/*apollo* \
         "${XDG_CONFIG_HOME:-$HOME/.config}"/wireplumber/main.lua.d/*apollo* \
         /usr/share/wireplumber/wireplumber.conf.d/*apollo*; do
    [ -f "$f" ] || continue
    found_wp=1
    case "$f" in "$HOME"/*) shown="~${f#"$HOME"}" ;; *) shown="$f" ;; esac
    note "-- $shown"
    grep -v '^[[:space:]]*#' "$f" | grep -v '^[[:space:]]*$' >> "$OUT"
done
[ "$found_wp" = 1 ] || note "  (none installed)"
note ""
note "PipeWire rule files mentioning apollo:"
found_pw=0
for f in /etc/pipewire/pipewire.conf.d/*apollo* /etc/pipewire/pipewire-pulse.conf.d/*apollo* \
         "${XDG_CONFIG_HOME:-$HOME/.config}"/pipewire/pipewire.conf.d/*apollo* \
         "${XDG_CONFIG_HOME:-$HOME/.config}"/pipewire/pipewire-pulse.conf.d/*apollo*; do
    [ -f "$f" ] || continue
    found_pw=1
    case "$f" in "$HOME"/*) shown="~${f#"$HOME"}" ;; *) shown="$f" ;; esac
    note "-- $shown"
done
[ "$found_pw" = 1 ] || note "  (none installed)"

# ------------------------------------------------------------ mixer daemon
section "mixer daemon (not connected to)"
if have ss; then
    printf '$ ss -ltnH (ports 4710, 4720, 4721)\n' >> "$OUT"
    ss -ltnH 2>/dev/null | awk '{print $4}' | grep -E ':(4710|4720|4721)$' >> "$OUT" \
        || note "  no listener on 4710/4720/4721"
else
    note "ss not installed, cannot check listening ports"
fi
if have pgrep; then
    pids="$(pgrep -f ua_mixer_daemon 2>/dev/null | tr '\n' ' ')"
    note "ua_mixer_daemon process: ${pids:-not running}"
fi
if have systemctl; then
    printf '$ systemctl units mentioning apollo/ua- (system and user)\n' >> "$OUT"
    { systemctl list-units --all --no-legend --plain 2>/dev/null
      systemctl --user list-units --all --no-legend --plain 2>/dev/null; } \
        | grep -E -i 'apollo|ua-usb|ua_mixer' | awk '{print $1, $3, $4}' >> "$OUT"
fi
# The daemon has no version constant: report a checksum of the script so the
# exact revision can be matched (read from disk, the daemon is not contacted).
for f in "$HERE/../mixer-engine/ua_mixer_daemon.py" /opt/open-apollo/mixer-engine/ua_mixer_daemon.py \
         /usr/local/share/open-apollo/mixer-engine/ua_mixer_daemon.py; do
    [ -f "$f" ] || continue
    if have sha256sum; then sum="$(sha256sum "$f" | cut -c1-12)"
    elif have shasum; then sum="$(shasum -a 256 "$f" | cut -c1-12)"
    else sum="(no sha256 tool)"; fi
    note "ua_mixer_daemon.py sha256[:12] = $sum"
    break
done

section "end of report"
LINES="$(wc -l < "$OUT" | tr -d ' ')"
echo "Done: $OUT ($LINES lines). Review it, then attach it to your report."
