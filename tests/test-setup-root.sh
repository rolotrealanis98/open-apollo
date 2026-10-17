#!/usr/bin/env bash
# test-setup-root.sh — unit test for scripts/setup-root.sh, the setup wizard's
# one privileged run: flag parsing, the order of its steps, the ::step markers,
# failing early on an unusable source (before anything is removed),
# --keep-current, the plug-in panel specs (--panels), --mount (ntfs3, the ntfs-3g fallback, an already open
# partition) and the unmount after success and failure.  setup-root.sh runs
# from a copy of scripts/ next to fake uninstall.sh, install.sh and
# install-panels.sh, with a fake
# oa-assets, on a PATH holding only harmless tools and logging stubs (id,
# runuser, mount, umount, findmnt): nothing real is mounted, removed or
# installed, and nothing needs root.

set -uo pipefail

PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
set --
# shellcheck source=scripts/setup-root.sh
source "$PROJECT_DIR/scripts/setup-root.sh"

tmp_dir=$(mktemp -d)
trap 'rm -rf "$tmp_dir"' EXIT

failures=0
check() {
    local desc="$1"; shift
    if "$@"; then
        echo "PASS: $desc"
    else
        echo "FAIL: $desc"
        if [ -n "${DEBUG_TEST:-}" ]; then sed 's/^/    out| /' "$tmp_dir/out"; sed 's/^/    log| /' "$log"; fi
        failures=$((failures + 1))
    fi
}
has()   { grep -qF -- "$2" "$1"; }
hasnt() { ! grep -qF -- "$2" "$1"; }

# ---- flags (sourced; bad ones exit 2) ----

check "sourcing sets no user" test -z "$TARGET_USER"
check "sourcing leaves --keep-current off" test "$KEEP_CURRENT" = 0
parse_args --user tester --from /mnt/w --keep-current --send-report --oa-assets /x/oa-assets --data /d
check "--user" test "$TARGET_USER" = tester
check "--from" test "$FROM" = /mnt/w
check "--keep-current" test "$KEEP_CURRENT" = 1
check "--send-report" test "$SEND_REPORT" = 1
check "--oa-assets" test "$OA_ASSETS" = /x/oa-assets
check "--data" test "$DATA_DIR" = /d
parse_args --user tester --from /mnt/w --panels /t/p.tar.gz --panels-sha256 abc
check "--panels" test "$PANELS_FROM" = /t/p.tar.gz
check "--panels-sha256" test "$PANELS_SHA" = abc
# shellcheck disable=SC2034  # parse_args reads them
bad() { (TARGET_USER="" FROM="" MOUNT_DEV=""; parse_args "$@") 2>/dev/null; test $? = 2; }
check "--user is required" bad --from /mnt/w
check "--from or --mount is required" bad --user tester
check "--from and --mount together are refused" bad --user tester --from /mnt/w --mount /dev/sda3
check "--mount takes a /dev path" bad --user tester --mount sda3
check "an option without its value is refused" bad --user
check "an unknown option is refused" bad --user tester --from /mnt/w --bogus
(parse_args --help) > "$tmp_dir/help"
check "--help documents the markers" has "$tmp_dir/help" "::step <id>"

# ---- the fake world ----

bin="$tmp_dir/bin"
state="$tmp_dir/state"
log="$tmp_dir/calls.log"
repo="$tmp_dir/repo"
win="$tmp_dir/win"
home="$tmp_dir/home"
mkdir -p "$bin" "$state" "$repo/scripts" "$win" "$home" "$tmp_dir/tmp"
cp "$PROJECT_DIR/scripts/setup-root.sh" "$repo/scripts/"
for tool in bash env rm rmdir sed grep head cut tee mktemp cat dirname mkdir touch; do
    ln -s "$(command -v "$tool")" "$bin/$tool"
done
stub() {
    printf '#!/bin/bash\necho "%s $*" >> "%s"\n%s\n' "$1" "$log" "${2:-exit 0}" > "$bin/$1"
    chmod +x "$bin/$1"
}
stub id 'case "$*" in -u) echo "${STUB_UID:-0}" ;; "-u tester"|"-g tester") echo 1000 ;; *) exit 1 ;; esac'
stub getent "[ \"\$2\" = tester ] && echo 'tester:x:1000:1000::$home:/bin/bash'"
stub runuser 'while [ "$1" != -- ]; do shift; done; shift; exec "$@"'
stub mount "case \"\$*\" in *'-t ntfs3 '*) [ -e '$state/ntfs3_fails' ] && { echo 'wrong fs type' >&2; exit 32; } ;; esac
[ -e '$state/mount_fails' ] && { echo 'mount failed' >&2; exit 32; }
touch '$state/is_mounted'"
stub umount "rm -f '$state/is_mounted'"
stub findmnt "cat '$state/mounted_at' 2>/dev/null; exit 0"
stub sleep

# oa-assets: scan prints serde_json's pretty JSON for $state/scan (good,
# noconsole, nodsp, or old: no firmware_inputs field yet)
oa="$tmp_dir/tools/oa-assets"
mkdir -p "$(dirname "$oa")"
cat > "$oa" <<EOF
#!/bin/bash
echo "oa-assets \$*" >> "$log"
st="$state"
case "\$1" in
    help)
        echo "  oa-assets scan   [--from PATH]... [--json]"
        echo "  oa-assets import [--from PATH] [--dry-run]"
        [ -e "\$st/plugins" ] && echo "  oa-assets plugins [--from PATH]   plug-in art"
        [ -e "\$st/dsp" ] && echo "  oa-assets dsp [--from PATH] [--out DIR]   plug-in DSP modules"
        echo "  oa-assets firmware [--from PATH] [--out DIR]" ;;
    scan)
        kind=\$(cat "\$st/scan" 2>/dev/null || echo good)
        console='"console": {
        "dir": "x",
        "resources": "x/Resources",
        "version": "1.3.1.120"
      },'
        [ "\$kind" = noconsole ] && console='"console": null,'
        inputs='"firmware_inputs": true,'
        [ "\$kind" = nodsp ] && inputs='"firmware_inputs": false,'
        [ "\$kind" = old ] && inputs=''
        sdk='"sdk_dll": null,'
        [ "\$kind" = old ] && sdk='"sdk_dll": "/w/Windows/System32/UAD2SDK.dll",'
        printf '{\n  "installs": [\n    {\n      %s\n      %s\n      "powered_plugins": 223,\n      %s\n      "uad_version": "12.0.0.602"\n    }\n  ]\n}\n' "\$console" "\$inputs" "\$sdk" ;;
    import)
        echo "HOME=\$HOME" >> "$log"
        [ -e "\$st/import_fails" ] && exit 1
        echo "Imported UAD Console 1.3.1.120 resources: 10 files, 1.0 MB" ;;
    plugins) echo "[ OK ]  plug-in art" ;;
    dsp) exit "\$(cat "\$st/dsp_rc" 2>/dev/null || echo 0)" ;;
    firmware)
        echo "── DSP modules ──"
        echo "dsp/0c2.bill     ok       sha256 abc  (x)"
        exit "\$(cat "\$st/firmware_rc" 2>/dev/null || echo 0)" ;;
esac
exit 0
EOF
chmod +x "$oa"
cat > "$repo/scripts/uninstall.sh" <<EOF
#!/bin/bash
echo "uninstall.sh \$* SUDO_USER=\$SUDO_USER" >> "$log"
echo "── Earlier install ──"
exit "\$(cat "$state/uninstall_rc" 2>/dev/null || echo 0)"
EOF
cat > "$repo/scripts/install-panels.sh" <<EOF
#!/bin/bash
echo "install-panels.sh \$*" >> "$log"
exit "\$(cat "$state/panels_rc" 2>/dev/null || echo 0)"
EOF
cat > "$repo/scripts/install.sh" <<EOF
#!/bin/bash
echo "install.sh \$* SUDO_USER=\$SUDO_USER" >> "$log"
echo "── Summary ──"
[ -e "$state/install_issues" ] && echo "Installation finished with issues — see above."
exit "\$(cat "$state/install_rc" 2>/dev/null || echo 0)"
EOF

# run [ENV=VALUE...] -- ARGS: setup-root.sh ARGS with a fresh state and log.
run() {
    local -a envs=()
    while [ "$1" != -- ]; do envs+=("$1"); shift; done
    shift
    rm -f "$log"
    (
        export PATH="$bin" TMPDIR="$tmp_dir/tmp"
        [ "${#envs[@]}" -gt 0 ] && export "${envs[@]}"
        exec bash "$repo/scripts/setup-root.sh" "$@"
    ) < /dev/null > "$tmp_dir/out" 2>&1
}
reset() { rm -f "${state:?}"/*; }
markers() { grep '^::step' "$tmp_dir/out" | cut -d' ' -f2,3 | tr '\n' ' ' | sed 's/ $//'; }
line_of() { grep -nF -- "$1" "$log" | head -1 | cut -d: -f1; }
before()  { local a b; a=$(line_of "$1"); b=$(line_of "$2"); [ -n "$a" ] && [ -n "$b" ] && [ "$a" -lt "$b" ]; }
ALL="source start source done remove start remove done import start import done firmware start firmware done driver start driver done"

# ---- a good install, read from a folder ----

reset
run -- --user tester --from "$win" --oa-assets "$oa"
check "a good run exits 0" test $? = 0
check "a good run's markers, in order" test "$(markers)" = "$ALL"
check "the source is checked first" before "oa-assets scan --from $win --json" "uninstall.sh"
check "the earlier install is removed with --yes --purge" has "$log" "uninstall.sh --yes --purge SUDO_USER=tester"
check "removal comes before the import" before "uninstall.sh" "oa-assets import"
check "the import runs as the desktop user" has "$log" "runuser -u tester -- env HOME=$home XDG_RUNTIME_DIR=/run/user/1000"
check "the import gets the user's HOME" has "$log" "HOME=$home"
check "the import comes before the firmware" before "oa-assets import --from $win" "oa-assets firmware"
check "firmware goes to /lib/firmware/universal-audio" \
    has "$log" "oa-assets firmware --from $win --out /lib/firmware/universal-audio"
check "the driver comes last" before "oa-assets firmware" "install.sh --yes SUDO_USER"
check "install.sh runs with --yes for the desktop user" has "$log" "install.sh --yes SUDO_USER=tester"
check "no report is sent unless asked" hasnt "$log" "--send-report"
check "nothing is mounted for a folder" hasnt "$log" "mount "
check "the output has headers" has "$tmp_dir/out" "── Checking your UAD install ──"
check "without a plugins command it says so" has "$tmp_dir/out" "can't generate it yet"
check "the plug-in art isn't attempted" hasnt "$log" "oa-assets plugins"
check "the panel specs are installed after the Console art" before "oa-assets import --from $win" "install-panels.sh"
check "the panel specs are installed as root, from the pinned release" grep -qx "install-panels.sh " "$log"
check "the panel specs come before the firmware" before "install-panels.sh" "oa-assets firmware"

reset
touch "$state/plugins"
run -- --user tester --from "$win" --oa-assets "$oa" --send-report
check "--send-report reaches install.sh" has "$log" "install.sh --yes --send-report"
check "with a plugins command it makes the plug-in art" has "$log" "oa-assets plugins --from $win"
check "the plug-in art is made as the user too" test "$(grep -c '^runuser -u tester' "$log")" = 2
check "with specs and art the import is done" has "$tmp_dir/out" "::step import done Console art, plug-in panels"

reset
touch "$state/plugins"; echo 3 > "$state/panels_rc"
run -- --user tester --from "$win" --oa-assets "$oa" --panels /t/p.tar.gz --panels-sha256 abc
check "a failed panel install doesn't fail setup" test $? = 0
check "--panels and --panels-sha256 reach install-panels.sh" has "$log" "install-panels.sh --from /t/p.tar.gz --sha256 abc"
check "a failed panel install only warns" \
    has "$tmp_dir/out" "::step import warn Console and plug-in art imported; plug-in panel specs not installed"
check "the plug-in art is still made" has "$log" "oa-assets plugins --from $win"
check "and setup goes on to the driver" has "$log" "install.sh --yes SUDO_USER=tester"

reset
check "without a dsp command no DSP modules are collected" hasnt "$log" "oa-assets dsp"
touch "$state/dsp"
run -- --user tester --from "$win" --oa-assets "$oa"
check "with a dsp command the plug-in DSP modules are collected" \
    has "$log" "oa-assets dsp --from $win --out /lib/firmware/universal-audio/plugins"
check "the DSP modules come after the firmware" before "oa-assets firmware" "oa-assets dsp"
reset
touch "$state/dsp"; echo 1 > "$state/dsp_rc"
run -- --user tester --from "$win" --oa-assets "$oa"
check "a failed DSP collection doesn't fail setup" test $? = 0
check "a failed DSP collection only warns" has "$tmp_dir/out" "Couldn't collect the plug-in DSP modules"
check "a failed DSP collection keeps every step" test "$(markers)" = "$ALL"

reset
echo old > "$state/scan"
run -- --user tester --from "$win" --oa-assets "$oa"
check "an oa-assets without firmware_inputs: UAD2SDK.dll is enough" test "$(markers)" = "$ALL"

# ---- an unusable source fails before anything is removed ----

for kind in noconsole nodsp; do
    reset
    echo "$kind" > "$state/scan"
    run -- --user tester --from "$win" --oa-assets "$oa"
    check "$kind: exits 1" test $? = 1
    check "$kind: stops at the source" test "$(markers)" = "source start source failed"
    check "$kind: removes nothing" hasnt "$log" "uninstall.sh"
    check "$kind: installs no driver" test -z "$(grep '^install.sh' "$log")"
    check "$kind: installs nothing" test -z "$(grep -E '^(install.sh|oa-assets (import|firmware))' "$log")"
    check "$kind: says nothing was changed" has "$tmp_dir/out" "Nothing was changed on this computer"
done
check "a missing Console is named" has "$tmp_dir/out" "No Console strip plug-ins or UAD2SDK.dll"

reset
run -- --user tester --from "$tmp_dir/nowhere" --oa-assets "$oa"
check "a missing folder fails at the source" test "$(markers)" = "source start source failed"

reset
run -- --user tester --from "$win" --oa-assets "$tmp_dir/no-oa-assets"
check "a missing oa-assets fails at the source" test "$(markers)" = "source start source failed"
check "a missing oa-assets is explained" has "$tmp_dir/out" "oa-assets not found"

# ---- --keep-current ----

reset
run -- --user tester --from "$win" --oa-assets "$oa" --keep-current
check "--keep-current exits 0" test $? = 0
check "--keep-current skips the removal" hasnt "$log" "uninstall.sh"
check "--keep-current marks the removal skipped" \
    test "$(markers)" = "source start source done remove skipped import start import done firmware start firmware done driver start driver done"

# ---- a failure in each later step stops the run there ----

reset
echo 1 > "$state/uninstall_rc"
run -- --user tester --from "$win" --oa-assets "$oa"
check "a failed removal exits 1" test $? = 1
check "a failed removal stops there" test "$(markers)" = "source start source done remove start remove failed"
check "after a failed removal nothing is imported" hasnt "$log" "oa-assets import"

reset
touch "$state/import_fails"
run -- --user tester --from "$win" --oa-assets "$oa"
check "a failed import stops there" \
    test "$(markers)" = "source start source done remove start remove done import start import failed"

for rc in 1 2; do
    reset
    echo "$rc" > "$state/firmware_rc"
    run -- --user tester --from "$win" --oa-assets "$oa"
    check "firmware exit $rc: exits 1" test $? = 1
    check "firmware exit $rc: stops at the firmware" test "$(markers | sed 's/.* firmware start/firmware start/')" = "firmware start firmware failed"
    check "firmware exit $rc: the driver isn't installed" test -z "$(grep '^install.sh' "$log")"
done
check "an unknown module version is explained" has "$tmp_dir/out" "a version open-apollo doesn't know"

reset
echo 1 > "$state/install_rc"
run -- --user tester --from "$win" --oa-assets "$oa"
check "a failed driver install exits 1" test $? = 1
check "a failed driver install is marked" has "$tmp_dir/out" "::step driver failed install.sh stopped (exit 1)"

reset
touch "$state/install_issues"
run -- --user tester --from "$win" --oa-assets "$oa"
check "an install with issues exits 0" test $? = 0
check "an install with issues is a warning" has "$tmp_dir/out" "::step driver warn"

# ---- --mount: opened read-only, closed again ----

reset
run -- --user tester --mount /dev/fake1 --oa-assets "$oa"
check "--mount exits 0" test $? = 0
check "--mount mounts read-only with ntfs3, as the user's files" \
    grep -qE "^mount -t ntfs3 -o ro,nosuid,nodev,noexec,uid=1000,gid=1000,umask=077 /dev/fake1 $tmp_dir/tmp/open-apollo-windows\.[A-Za-z0-9]+$" "$log"
mnt=$(sed -n 's/^mount .* \(\/.*open-apollo-windows\.[A-Za-z0-9]*\)$/\1/p' "$log" | head -1)
check "--mount reads the mounted partition" has "$log" "oa-assets scan --from $mnt --json"
check "--mount closes the partition at the end" has "$log" "umount $mnt"
check "--mount removes its mount point" test -n "$mnt" -a ! -e "$mnt"
check "--mount's markers end with the unmount" test "$(markers)" = "$ALL unmount start unmount done"
check "the partition is closed after the driver install" before "install.sh --yes SUDO_USER" "umount"

reset
touch "$state/ntfs3_fails"
run -- --user tester --mount /dev/fake1 --oa-assets "$oa"
check "without ntfs3 it falls back to ntfs-3g" has "$log" "mount -t ntfs-3g -o ro,"
check "the ntfs-3g mount works too" test "$(markers)" = "$ALL unmount start unmount done"

reset
echo noconsole > "$state/scan"
run -- --user tester --mount /dev/fake1 --oa-assets "$oa"
check "a mounted bad source: exits 1" test $? = 1
check "a mounted bad source: nothing removed" hasnt "$log" "uninstall.sh"
check "a mounted bad source: still closed again" test "$(markers)" = "source start source failed unmount start unmount done"

reset
echo 1 > "$state/firmware_rc"
run -- --user tester --mount /dev/fake1 --oa-assets "$oa"
check "a failed step still closes the partition" has "$log" "umount "

reset
touch "$state/mount_fails"
run -- --user tester --mount /dev/fake1 --oa-assets "$oa"
check "a partition that won't open: exits 1" test $? = 1
check "a partition that won't open: stops at the source" test "$(markers)" = "source start source failed"
check "a partition that won't open: nothing to unmount" hasnt "$log" "umount"
check "a partition that won't open: no mount point left" test -z "$(ls "$tmp_dir/tmp")"
check "a partition that won't open: hints at Fast Startup" has "$tmp_dir/out" "Fast Startup"

reset
echo "$win" > "$state/mounted_at"
run -- --user tester --mount /dev/fake1 --oa-assets "$oa"
check "an already open partition isn't mounted again" hasnt "$log" "mount -t"
check "an already open partition is read where it is" has "$log" "oa-assets scan --from $win --json"
check "an already open partition is left open" hasnt "$log" "umount"

# ---- not root ----

reset
run STUB_UID=1000 -- --user tester --from "$win" --oa-assets "$oa"
check "without root it exits 1" test $? = 1
check "without root nothing runs" test -z "$(grep -vE '^id ' "$log")"

reset
run -- --user nobody-here --from "$win" --oa-assets "$oa"
check "an unknown user exits 1 before anything" test $? = 1 -a -z "$(grep -E '^(oa-assets|mount)' "$log")"

if [ "$failures" -ne 0 ]; then
    echo "$failures check(s) failed"
    exit 1
fi
echo "All checks passed"
