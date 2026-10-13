#!/usr/bin/env bash
# test-install-panels.sh — install-panels.sh checks what it installs (the
# pinned SHA-256, one top folder, plain files only, spec names, the pinned
# version), swaps a release in and out, skips one already current, and never
# removes a folder that isn't its own.  Everything runs in a temp folder;
# downloads go through a fake curl.  No root, no network.

set -uo pipefail

PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="$PROJECT_DIR/scripts/install-panels.sh"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

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

# mk OUT VERSION KIND: a panels tarball; KIND good|other|traversal|symlink|twotops|badname
mk() {
    python3 -I - "$@" <<'EOF'
import io, sys, tarfile
out, version, kind = sys.argv[1:4]
top = f"open-apollo-panels-{version}"
def add(tar, name, data=b"", kind=tarfile.REGTYPE, link=""):
    info = tarfile.TarInfo(name)
    info.type, info.linkname, info.size = kind, link, len(data)
    tar.addfile(info, io.BytesIO(data) if data else None)
with tarfile.open(out, "w:gz") as tar:
    add(tar, f"{top}/VERSION", version.encode() + b"\n")
    add(tar, f"{top}/LICENSE", b"CC0\n")
    names = ["a", "b"] if kind != "other" else ["a", "c"]
    for n in names:
        add(tar, f"{top}/specs/PluginSpec_{n}.json", b'{"schema":2}\n')
    if kind == "traversal":
        add(tar, f"{top}/specs/../../evil", b"x")
    elif kind == "symlink":
        add(tar, f"{top}/specs/PluginSpec_l.json", kind=tarfile.SYMTYPE, link="/etc/passwd")
    elif kind == "twotops":
        add(tar, "open-apollo-panels-0.1/VERSION", b"0.1\n")
    elif kind == "badname":
        add(tar, f"{top}/specs/notes.txt", b"x")
EOF
}
sha() { sha256sum "$1" | cut -d' ' -f1; }

for k in good other traversal symlink twotops badname; do
    mk "$tmp/$k.tar.gz" 9.9 "$k"
done
mk "$tmp/old.tar.gz" 9.8 good
cat > "$tmp/panels.conf" <<EOF
# test pin
PANELS_VERSION=9.9
PANELS_SHA256=$(sha "$tmp/good.tar.gz")
PANELS_URL=https://example.invalid/v{version}/open-apollo-panels-{version}.tar.gz
EOF
dest="$tmp/share/open-apollo/plugin-specs"
run() { bash "$SCRIPT" --conf "$tmp/panels.conf" --dest "$dest" "$@" >"$tmp/out" 2>&1; }
rc() { run "$@"; echo $?; }

check "the repo's pin is well formed" bash -c \
    "source '$SCRIPT'; CONF='$PROJECT_DIR/configs/plugin-panels.conf'; read_conf >/dev/null"

check "installs the pinned tarball" test "$(rc --from "$tmp/good.tar.gz")" = 0
check "specs, VERSION and marker in place" test -f "$dest/PluginSpec_a.json" -a -f "$dest/PluginSpec_b.json" \
    -a "$(cat "$dest/VERSION" 2>/dev/null)" = 9.9 -a "$(cat "$dest/.source" 2>/dev/null)" = "$(sha "$tmp/good.tar.gz")"
check "no staging folders left" test -z "$(find "$tmp/share/open-apollo" -maxdepth 1 -name 'plugin-specs.*')"
check "files are world-readable" test "$(stat -c %a "$dest/PluginSpec_a.json")" = 644
check "a second run is a no-op" test "$(rc --from "$tmp/good.tar.gz")" = 0
check "  and says so" grep -q "already" "$tmp/out"

check "refuses a tarball that doesn't match the pin" test "$(rc --from "$tmp/other.tar.gz")" = 1
check "  and leaves the install alone" test "$(cat "$dest/.source")" = "$(sha "$tmp/good.tar.gz")"
for k in traversal symlink twotops badname; do
    check "refuses a $k tarball even with its own --sha256" \
        test "$(rc --from "$tmp/$k.tar.gz" --sha256 "$(sha "$tmp/$k.tar.gz")")" = 1
done
check "  and leaves the install alone" test -f "$dest/PluginSpec_b.json" -a ! -e "$tmp/share/evil"

sed "s/^PANELS_SHA256=.*/PANELS_SHA256=$(sha "$tmp/old.tar.gz")/" "$tmp/panels.conf" > "$tmp/stale.conf"
check "refuses a tarball whose VERSION isn't the pinned one" \
    test "$(bash "$SCRIPT" --conf "$tmp/stale.conf" --dest "$dest" --from "$tmp/old.tar.gz" >/dev/null 2>&1; echo $?)" = 1

check "replaces an older release (--sha256)" \
    test "$(rc --from "$tmp/other.tar.gz" --sha256 "$(sha "$tmp/other.tar.gz")")" = 0
check "  dropping specs the new one doesn't have" test -f "$dest/PluginSpec_c.json" -a ! -e "$dest/PluginSpec_b.json"

# Downloads: a fake curl that serves the good tarball, then one that fails.
mkdir -p "$tmp/bin"
cat > "$tmp/bin/curl" <<EOF
#!/bin/sh
out=""
while [ \$# -gt 0 ]; do [ "\$1" = -o ] && out="\$2"; last="\$1"; shift; done
echo "\$last" > "$tmp/url"
[ -n "\$FAKE_CURL_FAIL" ] && exit 22
cp "$tmp/good.tar.gz" "\$out"
EOF
chmod +x "$tmp/bin/curl"
check "downloads the pinned release" test "$(PATH="$tmp/bin:$PATH" rc)" = 0
check "  from the pinned URL" test "$(cat "$tmp/url")" = "https://example.invalid/v9.9/open-apollo-panels-9.9.tar.gz"
rm -rf "$dest"
check "a failed download exits 3" test "$(FAKE_CURL_FAIL=1 PATH="$tmp/bin:$PATH" rc)" = 3
check "  and installs nothing" test ! -e "$dest"

mkdir -p "$dest" && echo mine > "$dest/notes.txt"
check "won't replace a folder with other files" test "$(rc --from "$tmp/good.tar.gz")" = 1
check "won't remove it either" test "$(rc --uninstall)" = 1
check "  and keeps the file" test -f "$dest/notes.txt"
rm -rf "$dest"

run --from "$tmp/good.tar.gz"
check "--uninstall removes the specs" test "$(rc --uninstall)" = 0
check "  and the empty open-apollo folder" test ! -e "$tmp/share/open-apollo"
check "--uninstall with nothing installed is fine" test "$(rc --uninstall)" = 0

check "a relative --dest is a bad option" \
    test "$(bash "$SCRIPT" --dest specs --from "$tmp/good.tar.gz" >/dev/null 2>&1; echo $?)" = 2
check "a malformed --sha256 is a bad option" test "$(rc --sha256 xyz)" = 2

echo
if [ "$failures" -eq 0 ]; then
    echo "All install-panels checks passed."
else
    echo "$failures check(s) failed."
    exit 1
fi
