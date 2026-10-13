#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Keep Universal Audio bytes and personal data out of the repository.

open-apollo ships no UA files: the user's own UAD install is imported on their
machine (console-rs/crates/oa-assets). This check fails when a tracked file
breaks that rule or carries personal data:

  ua-bill       a UA DSP module ("Bill" record: 'Bill', id, 0x02000000 or 0,
                payload size, w4, payload), as raw bytes or written out as C
                byte arrays, hex words, decimal arrays, hex strings or base64
  ua-pinned     a file, or a run of bytes inside one, whose SHA-256 is a UA file
                pinned in console-rs/crates/oa-assets/data/firmware-pins.json
  raster-image  a PNG/JPEG/GIF/BMP/WebP/ICO/TIFF file or data: URI that is not
                a reviewed image in the allowlist (screenshots show UA art)
  private-ipv4  10/8, 172.16/12 or 192.168/16 addresses; use TEST-NET
                (192.0.2.x, RFC 5737) in examples
  home-path     /home/<name>, /Users/<name>, C:\\Users\\<name> or
                drive_c/users/<name> with a real name; use /home/you
  email         an e-mail address other than *@users.noreply.github.com, the
                RFC 2606 example domains and the allowlisted project addresses
  serial        an Apollo-style serial number (14 digits, or 12-16 digits
                starting with 2) that is not a synthetic test serial in the
                allowlist
  host-name     a machine named after its owner ("<Name>'s Mac Studio")
  large-file    a binary over --max-binary-kib or any file over --max-text-kib
  panel-spec    a plug-in panel spec (PluginSpec_*.json); the specs live in
                the open-apollo-panels repository, installed by
                scripts/install-panels.sh

Reviewed exceptions live in tools/check-clean-repo.allow. Findings print as
path:line: [rule] message; the exit status is 1 when there are any.

Only hashes are read from the pins file; this script holds no UA bytes.

Usage:
  tools/check-clean-repo.py                 the tracked files of this checkout
  tools/check-clean-repo.py --rev main      the files of a commit (no checkout)
  tools/check-clean-repo.py --root DIR      another checkout
  tools/check-clean-repo.py FILE...         only these files (relative to root)
"""
import argparse
import base64
import binascii
import fnmatch
import hashlib
import ipaddress
import json
import re
import struct
import subprocess
import sys
from collections import Counter
from pathlib import Path

HERE = Path(__file__).resolve().parent
PINS_PATH = "console-rs/crates/oa-assets/data/firmware-pins.json"
ALLOW_PATH = "tools/check-clean-repo.allow"

RULES = ("ua-bill", "ua-pinned", "raster-image", "private-ipv4", "home-path",
         "email", "serial", "host-name", "large-file", "panel-spec")

# ---- Bill records -------------------------------------------------------

BILL = b"Bill"
BILL_HDR = 0x14
BILL_W2 = 0x02000000
BILL_MAX = 16 << 20


def bill_header(data, i):
    """(id, size, w4) when data[i:] starts with a Bill record header.

    The shape (oa-assets firmware.rs): 'Bill', an id word whose arch byte
    (offset 7) is 0 on disk or 2 in PCIe form, word 2 0x02000000 (0 in USB
    form), a payload size that is a multiple of 4, and w4 in 1..size. The
    header holds NUL bytes, so text never matches by accident.
    """
    if len(data) - i < BILL_HDR or data[i:i + 4] != BILL:
        return None
    ident, w2, size, w4 = struct.unpack_from("<IIII", data, i + 4)
    if ident >> 24 not in (0, 2) or ident & 0x00FF0000 or not ident & 0xFFFF:
        return None
    if w2 not in (BILL_W2, 0):
        return None
    if not 0 < size <= BILL_MAX or size % 4 or not 0 < w4 <= size:
        return None
    return ident & 0xFFFF, size, w4


def bill_records(data):
    """Every Bill header in data: (offset, id, size, w4, record or None).

    The record is the whole on-disk form (arch byte 0, word 2 0x02000000)
    when the payload follows the header contiguously, for hashing.
    """
    at = data.find(BILL)
    while at >= 0:
        hdr = bill_header(data, at)
        if hdr:
            ident, size, w4 = hdr
            rec = None
            end = at + BILL_HDR + size
            if end <= len(data):
                rec = bytearray(data[at:end])
                rec[7] = 0
                rec[8:12] = struct.pack("<I", BILL_W2)
                rec = bytes(rec)
            yield at, ident, size, w4, rec
        at = data.find(BILL, at + 1)


# Text encodings a module could be written out in. Each yields
# (offset of the run in the file, decoded bytes).
RE_C_BYTES = re.compile(rb"(?:0[xX][0-9a-fA-F]{2}(?![0-9a-fA-F])\s*,\s*){19,}0[xX][0-9a-fA-F]{2}(?![0-9a-fA-F])")
RE_HEX_WORDS = re.compile(rb"(?:0[xX][0-9a-fA-F]{8}(?![0-9a-fA-F])\s*,\s*){4,}0[xX][0-9a-fA-F]{8}(?![0-9a-fA-F])")
RE_DECIMALS = re.compile(rb"(?<![\w.])(?:\d{1,10}\s*,\s*){4,}\d{1,10}(?![\w.])")
RE_HEX_STRING = re.compile(rb"(?<![0-9a-fA-F])[0-9a-fA-F]{40,}(?![0-9a-fA-F])")
RE_SPACED_HEX = re.compile(rb"(?<![0-9a-zA-Z])(?:[0-9a-fA-F]{2}[ \t]+){19,}[0-9a-fA-F]{2}(?![0-9a-zA-Z])")
RE_BASE64 = re.compile(rb"[A-Za-z0-9+/]{40,}={0,2}")


def decoded_runs(data):
    for m in RE_C_BYTES.finditer(data):
        yield m.start(), bytes(int(t, 16) for t in re.findall(rb"0[xX]([0-9a-fA-F]{2})", m.group()))
    for m in RE_HEX_WORDS.finditer(data):
        words = [int(t, 16) for t in re.findall(rb"0[xX]([0-9a-fA-F]{8})", m.group())]
        yield m.start(), struct.pack(f"<{len(words)}I", *words)
        yield m.start(), struct.pack(f">{len(words)}I", *words)
    for m in RE_DECIMALS.finditer(data):
        nums = [int(t) for t in re.findall(rb"\d+", m.group())]
        if all(n < 256 for n in nums):
            if len(nums) >= BILL_HDR:
                yield m.start(), bytes(nums)
        elif all(n < 1 << 32 for n in nums):
            yield m.start(), struct.pack(f"<{len(nums)}I", *nums)
    for m in RE_HEX_STRING.finditer(data):
        run = m.group()
        for shift in (0, 1):
            part = run[shift:len(run) - (len(run) - shift) % 2]
            yield m.start() + shift, binascii.unhexlify(part)
    for m in RE_SPACED_HEX.finditer(data):
        yield m.start(), bytes(int(t, 16) for t in m.group().split())
    for m in RE_BASE64.finditer(data):
        run = m.group().rstrip(b"=")
        try:
            yield m.start(), base64.b64decode(run[:len(run) - len(run) % 4], validate=True)
        except (binascii.Error, ValueError):
            pass


# ---- the pinned UA files (hashes only) ----------------------------------

class Pins:
    def __init__(self, path=None, text=None):
        self.modules = {}   # sha256 -> "module 0a5"
        self.files = {}     # sha256 -> name
        self.lengths = {}   # length -> {sha256} for whole-file artifacts
        self.source = path
        if text is None:
            return
        doc = json.loads(text)
        for m in doc.get("dsp_modules", []):
            for k in m.get("known", []):
                self.modules[k["sha256"].lower()] = f"DSP module {m['id']}"
                self.files[k["sha256"].lower()] = f"DSP module {m['id']}"
        for f in doc.get("usb_fx3", []):
            for k in f.get("known", []):
                self.files[k["sha256"].lower()] = f["name"]
                self.lengths.setdefault(int(k["length"]), set()).add(k["sha256"].lower())

    def __bool__(self):
        return bool(self.files)


# ---- allowlist ----------------------------------------------------------

class Allow:
    """tools/check-clean-repo.allow: [section] lines, '#' comments."""

    def __init__(self, text=""):
        self.images = {}      # path -> sha256
        self.nets = []        # ip networks
        self.emails = set()   # exact addresses
        self.domains = set()  # @domain (and its subdomains)
        self.names = set()    # placeholder user names
        self.serials = set()
        self.large = set()    # path globs
        self.skip = []        # (rule, glob)
        section = None
        for n, raw in enumerate(text.splitlines(), 1):
            line = raw.split("#", 1)[0].strip()
            if not line:
                continue
            if line.startswith("[") and line.endswith("]"):
                section = line[1:-1].strip()
                continue
            f = line.split()
            if section == "image" and len(f) == 2:
                self.images[f[0]] = f[1].lower()
            elif section == "ipv4" and len(f) == 1:
                self.nets.append(ipaddress.ip_network(f[0], strict=False))
            elif section == "email" and len(f) == 1:
                (self.domains.add(f[0][1:].lower()) if f[0].startswith("@")
                 else self.emails.add(f[0].lower()))
            elif section == "home" and len(f) == 1:
                self.names.add(f[0])
            elif section == "serial" and len(f) == 1:
                self.serials.add(f[0])
            elif section == "large" and len(f) == 1:
                self.large.add(f[0])
            elif section == "skip" and len(f) == 2 and f[0] in RULES:
                self.skip.append((f[0], f[1]))
            else:
                raise SystemExit(f"{ALLOW_PATH}:{n}: cannot read {raw.strip()!r} in [{section}]")

    def skipped(self, rule, path):
        return any(r == rule and fnmatch.fnmatchcase(path, g) for r, g in self.skip)


# ---- personal data ------------------------------------------------------

RE_IPV4 = re.compile(r"(?<![\w.])(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})(?![\w]|\.\d)")
# 10/8, 172.16/12, 192.168/16 (built from numbers so this file passes its own check)
PRIVATE = [ipaddress.IPv4Network((a << 24 | b << 16, bits)) for a, b, bits in ((10, 0, 8), (172, 16, 12), (192, 168, 16))]

RE_HOME = re.compile(
    r"(?<![\w.])(?:/home/|/Users/|(?i:[a-z]:\\{1,2}users\\{1,2}|drive_c/users/))"
    r"([^/\\\s\"'`<>()\[\]{},;:|]+)")
# Not a person: placeholders, variables, globs, Wine's own and system users.
GENERIC_HOME = re.compile(r"^(?:[$<{%*~.]|\.\.\.)|^(?:public|default|all users|default user|"
                          r"steamuser|crossover|linuxbrew|runner|root|shared|guest)$", re.IGNORECASE)

RE_EMAIL = re.compile(r"(?<![\w.%+-])[A-Za-z0-9][A-Za-z0-9._%+-]*@((?:[A-Za-z0-9-]+\.)+[A-Za-z]{2,})(?![\w-])")
FILE_SUFFIXES = {
    "png", "jpg", "jpeg", "gif", "svg", "webp", "bmp", "ico", "service", "socket",
    "target", "timer", "mount", "path", "bin", "json", "txt", "md", "rs", "py", "sh",
    "so", "dll", "wav", "xml", "html", "css", "js", "ts", "toml", "yaml", "yml",
    "conf", "plist", "vst3", "component", "aaxplugin", "app", "bundle", "uadmix",
}
EXAMPLE_DOMAINS = ("example.com", "example.org", "example.net", "example", "test",
                   "invalid", "localhost", "users.noreply.github.com")

# Apollo serials are 14 digits (seen: 20..., 22...); 12-16 digits from a 2 also count.
RE_SERIAL = re.compile(r"(?<![\w.\-/#:+])(?:2\d{11,15}|\d{14})(?![\w\-])")

RE_HOST = re.compile(r"\b[A-Z][a-z]+(?:'|\u2019)s (?:Mac(?:Book(?: Pro| Air)?| Studio| mini| Pro)?|iMac|PC|"
                     r"Laptop|Desktop|iPad|iPhone)\b")

# ---- images -------------------------------------------------------------

IMAGE_SUFFIXES = {".png", ".jpg", ".jpeg", ".gif", ".bmp", ".webp", ".ico",
                  ".tif", ".tiff", ".psd", ".heic", ".avif", ".icns"}
RE_DATA_URI = re.compile(rb"data:image/(?:png|jpe?g|gif|bmp|webp|x-icon|vnd\.microsoft\.icon|tiff|avif)"
                         rb";base64,[A-Za-z0-9+/=\s]{200,}")


def image_magic(data):
    if data.startswith(b"\x89PNG\r\n\x1a\n"):
        return "PNG"
    if data.startswith(b"\xff\xd8\xff"):
        return "JPEG"
    if data[:6] in (b"GIF87a", b"GIF89a"):
        return "GIF"
    if data[:4] == b"RIFF" and data[8:12] == b"WEBP":
        return "WebP"
    if data[:4] in (b"II*\x00", b"MM\x00*"):
        return "TIFF"
    if data[:2] == b"BM" and len(data) >= 26 and struct.unpack_from("<I", data, 2)[0] == len(data):
        return "BMP"
    if data[:4] == b"\x00\x00\x01\x00" and len(data) >= 22 and 0 < data[4] and data[5] == 0:
        return "ICO"
    if data[:4] == b"icns":
        return "ICNS"
    return None


# ---- scanning -----------------------------------------------------------

def is_binary(data):
    if b"\0" in data[:8192]:
        return True
    try:
        data.decode("utf-8")
    except UnicodeDecodeError:
        return True
    return False


class Scanner:
    def __init__(self, allow, pins, max_binary, max_text):
        self.allow = allow
        self.pins = pins
        self.max_binary = max_binary
        self.max_text = max_text
        self.findings = []

    def add(self, rule, path, where, msg):
        if not self.allow.skipped(rule, path):
            self.findings.append((path, where, rule, msg))

    def scan(self, path, data):
        binary = is_binary(data)

        def line(off):
            return data.count(b"\n", 0, off) + 1

        def where(off):
            return f"{line(off)}" if not binary else f"0x{off:x}"

        # (e) size
        limit = self.max_binary if binary else self.max_text
        if len(data) > limit and not any(fnmatch.fnmatchcase(path, g) for g in self.allow.large):
            kind = "binary" if binary else "file"
            self.add("large-file", path, "-", f"{kind} of {len(data) // 1024} KiB (limit {limit // 1024} KiB)")

        # (f) plug-in panel specs belong in open-apollo-panels
        if fnmatch.fnmatchcase(Path(path).name, "PluginSpec_*.json"):
            self.add("panel-spec", path, "-", "plug-in panel specs live in open-apollo-panels "
                     "(scripts/install-panels.sh installs them)")

        # (b) whole file
        digest = hashlib.sha256(data).hexdigest()
        if digest in self.pins.files:
            self.add("ua-pinned", path, "-", f"the file is UA's {self.pins.files[digest]} (SHA-256 {digest[:16]}...)")

        # (a)+(b) Bill records, raw and written out as text
        seen = set()
        for off, ident, size, w4, rec in bill_records(data):
            self._bill(path, where(off), ident, size, rec, "raw bytes", seen)
        if not binary:
            for off, blob in decoded_runs(data):
                for _, ident, size, w4, rec in bill_records(blob):
                    self._bill(path, where(off), ident, size, rec, "written out as text", seen)
        # (b) embedded FX3 images: Cypress boot images start with 'CY'
        if binary and self.pins.lengths:
            at = data.find(b"CY")
            while at >= 0:
                for length, digests in self.pins.lengths.items():
                    if at + length <= len(data) and (at or length != len(data)):
                        d = hashlib.sha256(data[at:at + length]).hexdigest()
                        if d in digests:
                            self.add("ua-pinned", path, where(at), f"embeds UA's {self.pins.files[d]}")
                at = data.find(b"CY", at + 1)

        # (c) images
        kind = image_magic(data) if binary else None
        if Path(path).suffix.lower() in IMAGE_SUFFIXES or kind:
            reviewed = self.allow.images.get(path)
            if reviewed != digest:
                why = "changed since it was reviewed" if reviewed else "not a reviewed image"
                self.add("raster-image", path, "-", f"{kind or Path(path).suffix[1:].upper()} image, {why} "
                         f"(sha256 {digest})")
        if binary:
            return
        for m in RE_DATA_URI.finditer(data):
            self.add("raster-image", path, where(m.start()), "embedded data: image URI")

        # (d) personal data, text only
        text = data.decode("utf-8")
        offsets = _LineIndex(text)
        for m in RE_IPV4.finditer(text):
            octets = [int(g) for g in m.groups()]
            if max(octets) > 255:
                continue
            ip = ipaddress.ip_address(m.group())
            if any(ip in n for n in PRIVATE) and not any(ip in n for n in self.allow.nets):
                self.add("private-ipv4", path, offsets.line(m.start()),
                         f"private address {m.group()} (use 192.0.2.x, RFC 5737)")
        for m in RE_HOME.finditer(text):
            name = m.group(1)
            if GENERIC_HOME.search(name) or name in self.allow.names:
                continue
            self.add("home-path", path, offsets.line(m.start()), f"home path {m.group()} (use /home/you)")
        for m in RE_EMAIL.finditer(text):
            addr, domain = m.group().lower(), m.group(1).lower()
            if domain.rsplit(".", 1)[-1] in FILE_SUFFIXES:
                continue
            if addr in self.allow.emails or any(
                    domain == d or domain.endswith("." + d) for d in (*EXAMPLE_DOMAINS, *self.allow.domains)):
                continue
            self.add("email", path, offsets.line(m.start()), f"e-mail address {m.group()}")
        for m in RE_SERIAL.finditer(text):
            if m.group() not in self.allow.serials:
                self.add("serial", path, offsets.line(m.start()),
                         f"serial-like number {m.group()} (use a synthetic 2000000000000N)")
        for m in RE_HOST.finditer(text):
            self.add("host-name", path, offsets.line(m.start()), f"host name {m.group()!r} (use 'Studio Mac')")

    def _bill(self, path, where, ident, size, rec, how, seen):
        key = (where, ident, size)
        if key in seen:
            return
        seen.add(key)
        self.add("ua-bill", path, where, f"UA DSP module (Bill record id 0x{ident:03x}, {size}-byte payload, {how})")
        if rec is not None:
            d = hashlib.sha256(rec).hexdigest()
            if d in self.pins.modules:
                self.add("ua-pinned", path, where, f"it is UA's pinned {self.pins.modules[d]}")


class _LineIndex:
    def __init__(self, text):
        self.text = text
        self.last = (0, 1)

    def line(self, off):
        start, n = self.last if off >= self.last[0] else (0, 1)
        n += self.text.count("\n", start, off)
        self.last = (off, n)
        return str(n)


# ---- where the files come from ------------------------------------------

def git(root, *args, input=None):
    return subprocess.run(["git", "-C", str(root), *args], input=input,
                          capture_output=True, check=True).stdout


def worktree_files(root, only):
    paths = only or [p.decode() for p in git(root, "ls-files", "-z").split(b"\0") if p]
    for p in paths:
        f = root / p
        if f.is_symlink() or not f.is_file():
            continue
        yield p, f.read_bytes()


def rev_files(root, rev, only):
    entries = []
    for e in git(root, "ls-tree", "-r", "-z", "--full-tree", rev).split(b"\0"):
        if not e:
            continue
        meta, path = e.split(b"\t", 1)
        mode, kind, sha = meta.split()
        if kind == b"blob" and mode != b"120000":
            entries.append((path.decode(), sha.decode()))
    if only:
        entries = [e for e in entries if e[0] in set(only)]
    proc = subprocess.Popen(["git", "-C", str(root), "cat-file", "--batch"],
                            stdin=subprocess.PIPE, stdout=subprocess.PIPE)
    try:
        for path, sha in entries:
            proc.stdin.write(sha.encode() + b"\n")
            proc.stdin.flush()
            header = proc.stdout.readline().split()
            size = int(header[2])
            data = proc.stdout.read(size)
            proc.stdout.read(1)
            yield path, data
    finally:
        proc.stdin.close()
        proc.wait()


def read_in_tree(root, rev, path):
    """A file of the scanned tree (None when it has none)."""
    try:
        if rev:
            return git(root, "show", f"{rev}:{path}").decode()
        return (root / path).read_text()
    except (subprocess.CalledProcessError, OSError):
        return None


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0],
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("files", nargs="*", help="only these paths (relative to the root)")
    ap.add_argument("--root", type=Path, help="the checkout to scan (default: this one)")
    ap.add_argument("--rev", help="scan the files of this commit instead of the working tree")
    ap.add_argument("--allowlist", type=Path, help=f"default: <root>/{ALLOW_PATH}, else the one next to this script")
    ap.add_argument("--pins", type=Path, help=f"default: <root>/{PINS_PATH}, else this repository's")
    ap.add_argument("--max-binary-kib", type=int, default=256)
    ap.add_argument("--max-text-kib", type=int, default=2048)
    ap.add_argument("--summary-only", action="store_true", help="print the counts per rule only")
    args = ap.parse_args(argv)

    root = args.root or Path(git(HERE, "rev-parse", "--show-toplevel").decode().strip())
    root = root.resolve()

    if args.allowlist:
        allow_text = args.allowlist.read_text()
    else:
        allow_text = read_in_tree(root, args.rev, ALLOW_PATH)
        if allow_text is None:
            allow_text = (HERE / Path(ALLOW_PATH).name).read_text()
    allow = Allow(allow_text)

    if args.pins:
        pins = Pins(args.pins, args.pins.read_text())
    else:
        text = read_in_tree(root, args.rev, PINS_PATH)
        src = PINS_PATH
        if text is None and (HERE.parent / PINS_PATH).is_file():
            text, src = (HERE.parent / PINS_PATH).read_text(), str(HERE.parent / PINS_PATH)
        pins = Pins(src, text)
        if not pins:
            print(f"warning: no {PINS_PATH}; ua-pinned checks hashes of nothing", file=sys.stderr)

    scanner = Scanner(allow, pins, args.max_binary_kib * 1024, args.max_text_kib * 1024)
    files = rev_files(root, args.rev, args.files) if args.rev else worktree_files(root, args.files)
    count = 0
    for path, data in files:
        count += 1
        scanner.scan(path, data)

    findings = scanner.findings
    if not args.summary_only:
        for path, where, rule, msg in findings:
            loc = path if where == "-" else f"{path}:{where}"
            print(f"{loc}: [{rule}] {msg}")
    per_rule = Counter(f[2] for f in findings)
    files_hit = len({f[0] for f in findings})
    tree = f"{args.rev} in {root}" if args.rev else str(root)
    if findings:
        counts = ", ".join(f"{r} {per_rule[r]}" for r in RULES if per_rule[r])
        print(f"check-clean-repo: {len(findings)} finding(s) in {files_hit} of {count} files ({tree}): {counts}",
              file=sys.stderr)
        print("Fix them, or for a reviewed false positive add an entry with its reason to "
              f"{ALLOW_PATH}. Never allowlist UA bytes or real personal data.", file=sys.stderr)
        return 1
    print(f"check-clean-repo: {count} files clean ({tree})", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
