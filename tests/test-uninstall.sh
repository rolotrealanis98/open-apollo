#!/usr/bin/env bash
# test-uninstall.sh — unit test for scripts/uninstall.sh: flag parsing, --list,
# and whole --yes runs (default, --purge, --keep-dkms, nothing installed, a
# file that can't be removed) against a fake root.  Sourcing uninstall.sh only
# loads functions.  The runs see a PATH holding nothing but harmless tools and
# logging stubs (dkms, rmmod, systemctl, udevadm, runuser, ...), so nothing
# real is unloaded, removed or restarted, and nothing needs root.

set -uo pipefail

PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
set --
# shellcheck source=scripts/uninstall.sh
source "$PROJECT_DIR/scripts/uninstall.sh"

tmp_dir=$(mktemp -d)
trap 'rm -rf "$tmp_dir"' EXIT

failures=0
check() {
    local desc="$1"; shift
    if "$@"; then
        echo "PASS: $desc"
    else
        echo "FAIL: $desc"
        failures=$((failures + 1))
    fi
}
exists()  { [ -e "$1" ] || [ -L "$1" ]; }
missing() { ! exists "$1"; }
has()     { grep -qF -- "$2" "$1"; }
hasnt()   { ! grep -qF -- "$2" "$1"; }

# ---- sourcing and flags ----

check "sourcing leaves --yes off" test "$ASSUME_YES" = 0
check "sourcing leaves --purge off" test "$PURGE" = 0
check "sourcing points at no fake root" test -z "$ROOT"
check "sourcing resolves no user" test -z "$DESKTOP_USER$USER_HOME"
parse_args --yes --purge
check "--yes sets ASSUME_YES" test "$ASSUME_YES" = 1
check "--purge sets PURGE" test "$PURGE" = 1
ASSUME_YES=0 PURGE=0
parse_args -y --keep-dkms
check "-y is --yes" test "$ASSUME_YES" = 1
check "--keep-dkms sets KEEP_DKMS" test "$KEEP_DKMS" = 1
parse_args --list
check "--list sets LIST_ONLY" test "$LIST_ONLY" = 1
ASSUME_YES=0 KEEP_DKMS=0 LIST_ONLY=0
(parse_args --bogus) 2>"$tmp_dir/err"
check "an unknown option exits 2" test $? = 2
check "an unknown option is named" has "$tmp_dir/err" "--bogus"
(parse_args --help) >"$tmp_dir/help"
check "--help exits 0" test $? = 0
check "--help documents --purge" has "$tmp_dir/help" "--purge"
check "--help documents --yes" has "$tmp_dir/help" "-y, --yes"

# ---- stubs: the only commands the runs can find ----

bin="$tmp_dir/bin"
state="$tmp_dir/state"
log="$tmp_dir/calls.log"
mkdir -p "$bin" "$state"
for tool in bash env rmdir sed grep sort cat cut awk head tr dirname basename mkdir ls; do
    ln -s "$(command -v "$tool")" "$bin/$tool"
done
stub() {
    printf '#!/bin/bash\necho "%s $*" >> "%s"\n%s\n' "$1" "$log" "${2:-exit 0}" > "$bin/$1"
    chmod +x "$bin/$1"
}
# rm, failing on one udev rule when $state/rm_fails exists (root or not)
printf '#!/bin/bash\nif [ -e "%s/rm_fails" ]; then case "$*" in *91-ua-apollo.rules*) exit 1 ;; esac; fi\nexec %s "$@"\n' \
    "$state" "$(command -v rm)" > "$bin/rm"
chmod +x "$bin/rm"
stub dkms "case \"\$1\" in
    status) for v in \$(cat '$state/dkms' 2>/dev/null); do echo \"ua_apollo/\$v, 6.9.0-test, x86_64: installed\"; done ;;
    remove) v=\${2#ua_apollo/}; sed -i \"/^\$v\\\$/d\" '$state/dkms' ;;
esac"
stub lsmod "echo 'Module                  Size  Used by'; [ -e '$state/loaded' ] && echo 'ua_apollo 135168  0'; exit 0"
stub rmmod "rm -f '$state/loaded'"
stub modprobe
stub systemctl
stub udevadm
stub depmod
stub mkinitcpio
stub update-desktop-database
stub sleep
stub pgrep "[ -e '$state/running' ] && [ \"\$2\" = ua_mixer_daemon ]"
stub pkill "rm -f '$state/running'"
stub runuser 'while [ "$1" != -- ]; do shift; done; shift; exec "$@"'
stub sudo 'shift 2; exec env "$@"'
stub uname "echo 6.9.0-test"
stub logname "echo tester"
stub id 'case "$*" in
    -u) echo "${STUB_UID:-0}" ;;
    "-u tester") echo 1000 ;;
    -un|"-nu 1000") echo tester ;;
    *) exit 1 ;;
esac'
stub getent "[ \"\$2\" = tester ] && echo 'tester:x:1000:1000::$tmp_dir/root/home/tester:/bin/bash'"
usb_stub="$tmp_dir/uninstall-usb.sh"
printf '#!/bin/bash\necho "uninstall-usb.sh $*" >> "%s"\necho "── Removing udev rules ──"\n' "$log" > "$usb_stub"

R="$tmp_dir/root"
H="$R/home/tester"

# An install with everything an install (and older ones) can leave behind,
# plus files of other software that must survive.
populate() {
    rm -rf "$R" "${state:?}"/* "$log"
    mkdir -p "$R/tmp" "$H"
    local d f
    for d in usr/src/ua_apollo-0.1.0 usr/src/ua_apollo-0.2.0 \
             var/lib/dkms/ua_apollo/0.1.0 var/lib/dkms/ua_apollo/0.2.0 var/lib/dkms/ua_apollo/0.0.9 \
             etc/systemd/user/ua-mixer-daemon.service.d usr/share/alsa/ucm2/ua_apollo \
             usr/share/alsa/ucm2/conf.d/ua_apollo usr/share/alsa/ucm2/conf.d/other \
             usr/local/lib/ua-usb lib/firmware/universal-audio/dsp lib/firmware/ua_apollo; do
        mkdir -p "$R/$d"
    done
    ln -s 0.2.0 "$R/var/lib/dkms/ua_apollo/kernel-6.9.0-test-x86_64"
    for f in lib/modules/6.9.0-test/updates/dkms/ua_apollo.ko.zst \
             lib/modules/6.8.0-old/updates/dkms/ua_apollo.ko.zst \
             lib/modules/6.9.0-test/extra/ua_apollo.ko \
             lib/modules/6.9.0-test/kernel/sound/other.ko.zst \
             etc/modules-load.d/ua_apollo.conf \
             etc/systemd/user/ua-mixer-daemon.service etc/systemd/user/ua-mixer-daemon@.service \
             etc/systemd/user/ua-mixer-daemon.service.d/override.conf \
             etc/wireplumber/wireplumber.conf.d/51-ua-apollo.conf \
             etc/wireplumber/main.lua.d/51-ua-apollo.lua \
             etc/pipewire/pipewire-pulse.conf.d/50-apollo-pulse-rules.conf \
             usr/share/alsa/ucm2/ua_apollo/HiFi.conf usr/share/alsa/ucm2/conf.d/ua_apollo/ua_apollo.conf \
             etc/udev/rules.d/91-ua-apollo.rules etc/udev/rules.d/70-ua-apollo-access.rules \
             etc/udev/rules.d/60-thunderbolt-no-rtd3.rules etc/udev/rules.d/50-other.rules \
             usr/local/bin/apollo-setup-io usr/local/bin/open-apollo-profile-setup \
             usr/local/bin/open-apollo-setup-worker usr/local/bin/other-tool \
             usr/share/applications/open-apollo.desktop \
             lib/firmware/ua-apollo-plugin-chain.bin lib/firmware/other.bin \
             etc/udev/rules.d/99-apollo-usb.rules usr/local/lib/ua-usb/fx3-load \
             lib/firmware/universal-audio/manifest.json lib/firmware/universal-audio/dsp/0c2.bill \
             lib/firmware/ua_apollo/0c2.bill tmp/open-apollo-install-report.json; do
        mkdir -p "$(dirname "$R/$f")"
        echo x > "$R/$f"
    done
    for f in .config/systemd/user/ua-mixer-daemon.service.d/10-oa-test.conf \
             .config/systemd/user/apollo-setup-io.service \
             .config/wireplumber/wireplumber.conf.d/51-ua-apollo.conf \
             .config/wireplumber/wireplumber.conf.d/99-mine.conf \
             .config/pipewire/pipewire.conf.d/apollo-io-map.conf \
             .config/autostart/open-apollo-tray.desktop \
             .local/bin/uaconsole .local/bin/open-apollo-console .local/bin/other \
             .local/share/applications/uaconsole.desktop \
             .local/share/icons/hicolor/scalable/apps/uaconsole.svg \
             .local/share/open-apollo/manifest.json .local/share/open-apollo/console-resources/a.png \
             .cache/open-apollo/uaconsole-smoke.png; do
        mkdir -p "$(dirname "$H/$f")"
        echo x > "$H/$f"
    done
    mkdir -p "$H/.config/systemd/user/default.target.wants"
    # Dangling once the unit is gone: must still be found and removed
    ln -s /etc/systemd/user/ua-mixer-daemon.service \
        "$H/.config/systemd/user/default.target.wants/ua-mixer-daemon.service"
    printf '0.1.0\n0.2.0\n' > "$state/dkms"
    touch "$state/loaded" "$state/running"
}

# run [ENV=VALUE...] -- ARGS: main ARGS in a subshell on the fake root, as root via sudo.
run() {
    local -a envs=()
    while [ "$1" != -- ]; do envs+=("$1"); shift; done
    shift
    (
        unset XDG_DATA_HOME XDG_CONFIG_HOME XDG_CACHE_HOME PKEXEC_UID
        export HOME="$H" SUDO_USER=tester PATH="$bin"
        [ "${#envs[@]}" -gt 0 ] && export "${envs[@]}"
        ROOT="$R"
        USB_UNINSTALL_SH="$usb_stub"
        main "$@"
    ) < /dev/null > "$tmp_dir/out" 2>&1
}
line_of() { grep -nF -- "$1" "$log" | head -1 | cut -d: -f1; }

# Everything but the kept data is gone, other software's files are not.
removed_all_but_data() {
    local f bad=0
    for f in "$R"/usr/src/ua_apollo-0.1.0 "$R"/usr/src/ua_apollo-0.2.0 "$R"/var/lib/dkms/ua_apollo \
             "$R"/lib/modules/6.9.0-test/updates/dkms/ua_apollo.ko.zst \
             "$R"/lib/modules/6.8.0-old/updates/dkms/ua_apollo.ko.zst \
             "$R"/lib/modules/6.9.0-test/extra/ua_apollo.ko "$R"/etc/modules-load.d/ua_apollo.conf \
             "$R"/etc/systemd/user/ua-mixer-daemon.service "$R"/etc/systemd/user/ua-mixer-daemon@.service \
             "$R"/etc/systemd/user/ua-mixer-daemon.service.d \
             "$H"/.config/systemd/user/ua-mixer-daemon.service.d \
             "$H"/.config/systemd/user/default.target.wants/ua-mixer-daemon.service \
             "$H"/.config/systemd/user/apollo-setup-io.service \
             "$R"/etc/wireplumber/wireplumber.conf.d/51-ua-apollo.conf \
             "$R"/etc/wireplumber/main.lua.d/51-ua-apollo.lua \
             "$R"/etc/pipewire/pipewire-pulse.conf.d/50-apollo-pulse-rules.conf \
             "$H"/.config/wireplumber/wireplumber.conf.d/51-ua-apollo.conf \
             "$H"/.config/pipewire/pipewire.conf.d/apollo-io-map.conf \
             "$R"/usr/share/alsa/ucm2/ua_apollo "$R"/usr/share/alsa/ucm2/conf.d/ua_apollo \
             "$R"/etc/udev/rules.d/91-ua-apollo.rules "$R"/etc/udev/rules.d/70-ua-apollo-access.rules \
             "$R"/etc/udev/rules.d/60-thunderbolt-no-rtd3.rules "$R"/usr/local/bin/apollo-setup-io \
             "$R"/usr/local/bin/open-apollo-profile-setup "$R"/usr/local/bin/open-apollo-setup-worker \
             "$H"/.local/bin/uaconsole "$H"/.local/bin/open-apollo-console \
             "$H"/.local/share/applications/uaconsole.desktop \
             "$H"/.local/share/icons/hicolor/scalable/apps/uaconsole.svg \
             "$H"/.config/autostart/open-apollo-tray.desktop "$R"/usr/share/applications/open-apollo.desktop \
             "$R"/lib/firmware/ua-apollo-plugin-chain.bin \
             "$R"/etc/udev/rules.d/99-apollo-usb.rules "$R"/usr/local/lib/ua-usb \
             "$R"/tmp/open-apollo-install-report.json; do
        if exists "$f"; then echo "    left: ${f#"$R"}"; bad=1; fi
    done
    for f in "$R"/etc/udev/rules.d/50-other.rules "$R"/usr/local/bin/other-tool "$R"/lib/firmware/other.bin \
             "$R"/lib/modules/6.9.0-test/kernel/sound/other.ko.zst "$R"/usr/share/alsa/ucm2/conf.d/other \
             "$H"/.config/wireplumber/wireplumber.conf.d/99-mine.conf "$H"/.local/bin/other; do
        if missing "$f"; then echo "    other software's file removed: ${f#"$R"}"; bad=1; fi
    done
    return "$bad"
}

# ---- --list: what the setup wizard shows before it runs ----

populate
run STUB_UID=1000 -- --list
check "--list runs without root" test $? = 0
check "--list shows every DKMS version" has "$tmp_dir/out" "$(printf 'dkms\tua_apollo 0.1.0 (DKMS)')"
check "--list finds a version only DKMS's tree knows" has "$tmp_dir/out" "ua_apollo 0.0.9 (DKMS)"
check "--list shows the loaded driver" grep -q "^module" "$tmp_dir/out"
check "--list shows the user's daemon drop-in" \
    has "$tmp_dir/out" "$(printf 'daemon\t/home/tester/.config/systemd/user/ua-mixer-daemon.service.d')"
check "--list shows extracted firmware" has "$tmp_dir/out" "$(printf 'firmware\t/lib/firmware/universal-audio')"
check "--list shows imported art" has "$tmp_dir/out" "$(printf 'data\t/home/tester/.local/share/open-apollo')"
check "--list shows uaconsole" has "$tmp_dir/out" "$(printf 'console\t/home/tester/.local/bin/uaconsole')"
check "--list changes nothing" test -e "$R/etc/udev/rules.d/91-ua-apollo.rules" -a -e "$state/loaded"
check "--list calls no privileged command" test ! -e "$log" -o -z "$(grep -E '^(rmmod|systemctl|runuser|udevadm)' "$log" 2>/dev/null)"

# ---- not root: refuses ----

run STUB_UID=1000 -- --yes
check "without root it refuses" test $? = 1
check "without root nothing is removed" test -e "$R/etc/udev/rules.d/91-ua-apollo.rules"

# ---- default --yes: everything but the extracted firmware and imported art ----

populate
run -- --yes
rc=$?
check "default run exits 0" test "$rc" = 0
check "default run removes every installed file" removed_all_but_data
check "every DKMS version is removed through dkms" \
    test "$(grep -c '^dkms remove' "$log")" = 3
check "dkms remove covers 0.1.0, 0.2.0 and 0.0.9" \
    grep -q 'dkms remove ua_apollo/0.0.9 --all' "$log"
check "default run keeps the extracted firmware" test -e "$R/lib/firmware/universal-audio/manifest.json"
check "default run keeps the old extractor's output" test -d "$R/lib/firmware/ua_apollo"
check "default run keeps imported art" test -e "$H/.local/share/open-apollo/manifest.json"
check "default run keeps the cache" test -d "$H/.cache/open-apollo"
check "the module is unloaded" test ! -e "$state/loaded"
stop_pw=$(line_of "systemctl --user stop pipewire.service")
rmmod_at=$(line_of "rmmod ua_apollo")
check "PipeWire is stopped before rmmod" test -n "$stop_pw" -a -n "$rmmod_at" -a "${stop_pw:-0}" -lt "${rmmod_at:-0}"
check "the daemon unit is disabled as the desktop user" \
    has "$log" "runuser -u tester -- env HOME=$H XDG_RUNTIME_DIR=/run/user/1000"
check "the daemon is disabled" has "$log" "systemctl --user disable --now ua-mixer-daemon.service"
check "the hand-started daemon is stopped" has "$log" "pkill -f ua_mixer_daemon"
check "depmod runs for the running kernel" has "$log" "depmod -a 6.9.0-test"
check "depmod runs for the other kernel too" has "$log" "depmod -a 6.8.0-old"
check "the modules-load.d entry triggers an initramfs regen" has "$log" "mkinitcpio -P"
check "udev rules are reloaded" has "$log" "udevadm control --reload-rules"
check "the USB uninstaller runs with --yes" has "$log" "uninstall-usb.sh --yes"
check "without --purge the USB uninstaller keeps firmware" hasnt "$log" "uninstall-usb.sh --yes --purge"
check "the output has the installers' headers" has "$tmp_dir/out" "── Check ──"
check "the check passes" has "$tmp_dir/out" "[ OK ]  Nothing from open-apollo is left"
check "the check names what was kept" has "$tmp_dir/out" "Kept: /lib/firmware/universal-audio"
check "nothing was asked" hasnt "$tmp_dir/out" "[y/N]"

run -- --yes
check "a second run exits 0" test $? = 0
check "a second run has nothing to remove" has "$tmp_dir/out" "Nothing to remove"
check "a second run unloads nothing" test "$(grep -c '^rmmod' "$log")" = 1

# ---- --purge: the extracted firmware and imported art go too ----

populate
run -- --yes --purge
check "--purge run exits 0" test $? = 0
check "--purge removes every installed file" removed_all_but_data
check "--purge removes /lib/firmware/universal-audio" missing "$R/lib/firmware/universal-audio"
check "--purge removes /lib/firmware/ua_apollo" missing "$R/lib/firmware/ua_apollo"
check "--purge removes ~/.local/share/open-apollo" missing "$H/.local/share/open-apollo"
check "--purge removes ~/.cache/open-apollo" missing "$H/.cache/open-apollo"
check "--purge keeps other firmware" test -e "$R/lib/firmware/other.bin"
check "--purge is passed to the USB uninstaller" has "$log" "uninstall-usb.sh --yes --purge"

run -- --yes --purge
check "a second --purge run exits 0" test $? = 0
check "a second --purge run finds nothing" has "$tmp_dir/out" "Nothing from open-apollo is installed"

# ---- PKEXEC_UID (the wizard runs it through pkexec, which sets no SUDO_USER) ----

populate
run SUDO_USER= PKEXEC_UID=1000 -- --yes
check "under pkexec the desktop user is found" has "$log" "runuser -u tester --"
check "under pkexec the user's drop-in is removed" missing "$H/.config/systemd/user/ua-mixer-daemon.service.d"

# ---- --keep-dkms ----

populate
run -- --yes --keep-dkms
check "--keep-dkms exits 0" test $? = 0
check "--keep-dkms calls no dkms remove" hasnt "$log" "dkms remove"
check "--keep-dkms keeps the DKMS tree" test -d "$R/var/lib/dkms/ua_apollo"
check "--keep-dkms still removes the rest" missing "$R/etc/udev/rules.d/91-ua-apollo.rules"
check "--keep-dkms says what it kept" has "$tmp_dir/out" "Kept (--keep-dkms)"

# ---- nothing installed ----

rm -rf "$R" "${state:?}"/* "$log"
mkdir -p "$H"
run -- --yes
check "nothing installed: exits 0" test $? = 0
check "nothing installed: says so" has "$tmp_dir/out" "Nothing from open-apollo is installed"
check "nothing installed: touches no service" test ! -e "$log" -o -z "$(grep -E '^(rmmod|systemctl|depmod)' "$log" 2>/dev/null)"

# ---- something can't be removed: the check lists it and fails ----

populate
touch "$state/rm_fails"
run -- --yes
rc=$?
rm -f "$state/rm_fails"
check "a leftover makes the run exit 1" test "$rc" = 1
check "the leftover is listed" has "$tmp_dir/out" "Still here: /etc/udev/rules.d/91-ua-apollo.rules"
check "the rest is still removed" missing "$R/usr/src/ua_apollo-0.1.0"

if [ "$failures" -ne 0 ]; then
    echo "$failures check(s) failed"
    exit 1
fi
echo "All checks passed"
