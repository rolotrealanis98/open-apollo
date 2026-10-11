#!/bin/bash
# apollo-hostcheck.sh - host checks that explain why the Apollo or the driver
# is not usable, and print the exact command that fixes it.  Sourced by
# scripts/install.sh, scripts/check-deps.sh, tools/apollo-selftest.sh; also
# runnable on its own (tb | sb | all).
#
#   hostcheck_thunderbolt   Apollo behind an unauthorized Thunderbolt device
#   hostcheck_secureboot    Secure Boot rejecting the unsigned ua_apollo module
#
# Both only read; they never enroll, authorize, sign or change anything.
# Output goes through hc_warn / hc_info / hc_cmd, which the caller may
# define first (install.sh maps them onto its warn/info helpers).  Return 0
# when nothing is wrong, 1 when a problem was reported.
#
# HC_SYSFS (default /sys) and HC_ROOT (default empty) relocate the files that
# are read, so tests can use a fake tree.

HC_SYSFS="${HC_SYSFS:-/sys}"
HC_ROOT="${HC_ROOT:-}"

if ! declare -F hc_warn >/dev/null; then hc_warn() { printf '[WARN]  %s\n' "$*"; }; fi
if ! declare -F hc_info >/dev/null; then hc_info() { printf '[INFO]  %s\n' "$*"; }; fi
if ! declare -F hc_cmd >/dev/null; then hc_cmd() { printf '        %s\n' "$*"; }; fi

hc_read() { [ -r "$1" ] && tr -d '\n' < "$1" 2>/dev/null; }

# --- Thunderbolt authorization ---------------------------------------
# Linux security levels (domainN/security): none and dponly never block PCIe
# tunnels; usbonly blocks them; user and secure need the device authorized
# (by bolt, a desktop prompt, or sysfs) before the Apollo shows up in lspci.
hostcheck_thunderbolt() {
    local tb="$HC_SYSFS/bus/thunderbolt/devices" d name level domain_level="" any=0
    local vendor devname uuid label u
    local -a uuids=()
    [ -d "$tb" ] || return 0

    for d in "$tb"/domain*; do
        [ -d "$d" ] || continue
        level=$(hc_read "$d/security")
        [ -n "$level" ] && domain_level="${domain_level:+$domain_level,}$level"
    done

    for d in "$tb"/*; do
        [ -d "$d" ] || continue
        name=$(basename "$d")
        case "$name" in domain*) continue ;; esac
        # The host router has no "authorized" attribute; only peripherals do.
        [ "$(hc_read "$d/authorized")" = "0" ] || continue
        any=1
        vendor=$(hc_read "$d/vendor_name")
        devname=$(hc_read "$d/device_name")
        uuid=$(hc_read "$d/unique_id")
        label="${vendor:+$vendor }${devname:-device}"
        if [[ "$vendor $devname" =~ [Uu]niversal[[:space:]]+[Aa]udio|[Aa]pollo ]]; then
            hc_warn "Thunderbolt device '$label' ($name) is not authorized${domain_level:+ (security level: $domain_level)}."
        else
            hc_warn "Unauthorized Thunderbolt device '$label' ($name)${domain_level:+ (security level: $domain_level)} - if this is the Apollo, it is not on the PCI bus yet."
        fi
        [ -n "$uuid" ] && uuids+=("$uuid")
    done
    [ "$any" = "1" ] || return 0

    hc_info "The Apollo cannot appear on PCIe (lspci -d 1a00:) until its Thunderbolt connection is authorized."
    if command -v boltctl >/dev/null 2>&1; then
        hc_info "Authorize it for good (recommended), then replug or power-cycle the Apollo:"
    else
        hc_info "boltctl is not installed (package 'bolt'). After installing it:"
    fi
    if [ "${#uuids[@]}" -eq 0 ]; then
        hc_cmd "boltctl list    # find the Apollo's uuid, then: boltctl enroll --policy auto <uuid>"
    else
        for u in "${uuids[@]}"; do hc_cmd "boltctl enroll --policy auto $u"; done
        hc_info "Or authorize for this connection only (forgotten at unplug):"
        for u in "${uuids[@]}"; do hc_cmd "boltctl authorize $u"; done
    fi
    hc_info "Without bolt (security=user only): echo 1 | sudo tee $tb/<device>/authorized"
    return 1
}

# --- Secure Boot -----------------------------------------------------
hc_secureboot_on() {
    local f
    if command -v mokutil >/dev/null 2>&1; then
        mokutil --sb-state 2>/dev/null | grep -qi 'enabled'
        return
    fi
    # No mokutil: the SecureBoot EFI variable is 4 attribute bytes + 1 value.
    for f in "$HC_SYSFS"/firmware/efi/efivars/SecureBoot-*; do
        [ -r "$f" ] || continue
        [ "$(od -An -tu1 -j4 -N1 "$f" 2>/dev/null | tr -d ' ')" = "1" ] && return 0
    done
    return 1
}

# First existing DER certificate DKMS / akmods signs with, per distro.
hc_mok_cert() {
    local conf="$HC_ROOT/etc/dkms/framework.conf" c f
    if [ -r "$conf" ]; then
        c=$(sed -n 's/^[[:space:]]*mok_certificate[[:space:]]*=[[:space:]]*["'"'"']\{0,1\}\([^"'"'"']*\).*/\1/p' "$conf" | tail -1)
        if [ -n "$c" ] && [ -r "$c" ]; then echo "$c"; return 0; fi
    fi
    for f in \
        /var/lib/dkms/mok.pub \
        /var/lib/shim-signed/mok/MOK.der \
        /etc/pki/akmods/certs/public_key.der \
        /usr/share/akmods/certs/public_key.der; do
        if [ -r "$HC_ROOT$f" ]; then echo "$HC_ROOT$f"; return 0; fi
    done
    return 1
}

# $1 (optional): output of a failed modprobe/insmod, to recognise a rejected key.
hostcheck_secureboot() {
    local modout="${1:-}" rejected=0 signer="" cert="" enrolled=0 distro=""
    hc_secureboot_on || return 0

    case "$modout" in
        *"Key was rejected"*|*"required key not available"*) rejected=1 ;;
    esac
    signer=$(modinfo -F signer ua_apollo 2>/dev/null | head -1)
    if [ "$rejected" = "0" ]; then
        # Quiet unless an installed module is unsigned.
        modinfo -n ua_apollo >/dev/null 2>&1 || return 0
        [ -z "$signer" ] || return 0
    fi

    if [ -n "$signer" ]; then
        hc_warn "Secure Boot is enabled and ua_apollo is signed by '$signer', but the kernel does not trust that key."
    else
        hc_warn "Secure Boot is enabled and the ua_apollo module is not signed; the kernel refuses to load it ('Key was rejected by service')."
    fi
    if [ -r "$HC_ROOT/etc/os-release" ]; then
        # shellcheck disable=SC1091
        distro=$(. "$HC_ROOT/etc/os-release" && echo "${ID:-}")
    fi

    cert=$(hc_mok_cert) || cert=""
    if [ -z "$cert" ]; then
        hc_info "No DKMS/akmods signing certificate found. Create one, then rebuild the module so DKMS signs it:"
        case "$distro" in
            fedora|rhel|centos) hc_cmd "sudo kmodgenca -a    # akmods key, or: sudo dkms generate_mok" ;;
            *)                  hc_cmd "sudo dkms generate_mok    # creates /var/lib/dkms/mok.key and mok.pub" ;;
        esac
        hc_cmd "sudo dkms install ua_apollo/<version> --force    # version: dkms status ua_apollo"
        cert="/var/lib/dkms/mok.pub"
    elif command -v mokutil >/dev/null 2>&1 && mokutil --test-key "$cert" 2>/dev/null | grep -qi 'already enrolled'; then
        enrolled=1
    fi

    if [ "$enrolled" = "1" ]; then
        hc_info "Your signing key ($cert) is already enrolled, so the module is unsigned or signed with another key. Rebuild it with DKMS:"
        hc_cmd "sudo dkms install ua_apollo/<version> --force    # version: dkms status ua_apollo"
        return 1
    fi

    hc_info "Enroll the signing key once (the installer never does this for you):"
    hc_cmd "sudo mokutil --import $cert"
    hc_info "It asks for a one-time password. Reboot, pick 'Enroll MOK' in the blue MOK manager screen, enter that password, continue, then boot normally."
    case "$distro" in
        ubuntu|debian|pop|linuxmint)      hc_info "Ubuntu/Debian: DKMS key is /var/lib/dkms/mok.{key,pub} (older installs: /var/lib/shim-signed/mok/MOK.der)." ;;
        fedora|rhel|centos)               hc_info "Fedora: akmods key is /etc/pki/akmods/certs/public_key.der; DKMS 3.x uses /var/lib/dkms/mok.pub." ;;
        arch|manjaro|endeavouros|cachyos) hc_info "Arch: no key exists by default; use 'dkms generate_mok' or set mok_signing_key / mok_certificate in /etc/dkms/framework.conf." ;;
    esac
    hc_info "Verify afterwards: mokutil --test-key $cert ; modinfo -F signer ua_apollo"
    return 1
}

if [ "${BASH_SOURCE[0]}" = "$0" ]; then
    rc=0
    case "${1:-all}" in
        tb)  hostcheck_thunderbolt || rc=1 ;;
        sb)  hostcheck_secureboot "${2:-}" || rc=1 ;;
        all) hostcheck_thunderbolt || rc=1; hostcheck_secureboot || rc=1 ;;
        *)   echo "usage: $0 [tb|sb|all]" >&2; exit 2 ;;
    esac
    exit "$rc"
fi
