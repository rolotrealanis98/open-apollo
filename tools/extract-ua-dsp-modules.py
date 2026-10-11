#!/usr/bin/env python3
"""extract-ua-dsp-modules.py - copy the Console input-strip DSP modules out of
your own Universal Audio install, for the driver's input_strips option.

The Apollo's analog inputs reach the mixer and the computer through UA
Console's built-in input strips, small DSP programs ("Bill" modules) that
ship inside UA's software.  open-apollo does not distribute them: this tool
finds them in an install you already have (Windows install, also under Wine,
or macOS) and writes one file per module to /lib/firmware/ua_apollo/dsp/,
where the driver loads them with request_firmware().

Only the built-in Console strip modules are taken: 0EB, 0DA, 0A5, 0C2, 12B
(Mic Input Strip), 0DB (Input Strip) and 0C9 (Talkback Input Strip).

Usage:
    tools/extract-ua-dsp-modules.py [PATH ...] [--out DIR] [--dry-run]

PATH is a file or a directory (searched recursively).  Without PATH the
usual places are searched: Wine prefixes in your home directory, mounted
Windows partitions and, on macOS, /Library.  Run it with sudo to write to
/lib/firmware, or use --out to write elsewhere.  Exit status: 0 when every
Mic Input Strip module was found, 1 when some are missing, 2 on an
ambiguous module (two different copies of the same id).
"""

import argparse
import glob
import hashlib
import os
import struct
import sys
from pathlib import Path

MAGIC = b"Bill"
# Built-in Console strip modules (apollo-linux macos-dsp-module-load.md §A5)
MIC_STRIP = (0x0EB, 0x0DA, 0x0A5, 0x0C2, 0x12B)
STRIP_IDS = MIC_STRIP + (0x0DB, 0x0C9)
MAX_FILE = 256 * 1024 * 1024
SCAN_SUFFIXES = (".dll", ".vst3", ".vst", ".component", ".so", ".dylib",
                 ".bundle", "")


def default_roots() -> list[Path]:
    home = Path.home()
    pats = [
        str(home / ".wine*/drive_c/Program Files*/Universal Audio"),
        str(home / "wine-ua*/drive_c/Program Files*/Universal Audio"),
        str(home / ".local/share/wineprefixes/*/drive_c/Program Files*/Universal Audio"),
        "/mnt/*/Program Files*/Universal Audio",
        "/media/*/*/Program Files*/Universal Audio",
        "/run/media/*/*/Program Files*/Universal Audio",
        "/Library/Application Support/Universal Audio",
        "/Library/Frameworks/UAD-2 SDK Support.framework",
    ]
    out = []
    for p in pats:
        out += [Path(x) for x in glob.glob(p)]
    return out


def candidate_files(roots):
    for root in roots:
        if root.is_file():
            yield root
            continue
        for dirpath, _dirs, files in os.walk(root):
            for name in files:
                p = Path(dirpath) / name
                if p.suffix.lower() in SCAN_SUFFIXES or "Strip" in name \
                        or "SDK Support" in name:
                    yield p


def scan_bytes(data: bytes, wanted=STRIP_IDS):
    """(module id, blob bytes) for every valid Bill module in data."""
    i = data.find(MAGIC)
    while i >= 0:
        if i % 4 == 0 and i + 0x14 <= len(data):
            idw, w2, size, _w4 = struct.unpack_from("<IIII", data, i + 4)
            mid, arch = idw & 0xFFFF, idw >> 24
            end = i + size + 0x14
            if (w2 == 0x02000000 and arch == 0 and mid in wanted
                    and end <= len(data)):
                yield mid, data[i:end]
        i = data.find(MAGIC, i + 1)


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(
        description="Copy UA Console's input-strip DSP modules from your UA "
                    "install for the open-apollo driver")
    ap.add_argument("paths", nargs="*", type=Path,
                    help="files or directories to search (default: the "
                         "usual install locations)")
    ap.add_argument("--out", type=Path,
                    default=Path("/lib/firmware/ua_apollo/dsp"),
                    help="output directory (default /lib/firmware/ua_apollo/dsp)")
    ap.add_argument("--dry-run", action="store_true",
                    help="list what would be written, write nothing")
    args = ap.parse_args(argv)

    roots = args.paths or default_roots()
    if not roots:
        print("no UA install found; pass the install directory as PATH",
              file=sys.stderr)
        return 1
    found: dict[int, dict[str, bytes]] = {}
    where: dict[str, str] = {}
    # The strip header plugins first: the same modules are also built into
    # other plugin files, but these are where the Console strips come from
    files = sorted(candidate_files(roots),
                   key=lambda p: (0 if "Strip Header" in p.name
                                  or "SDK Support" in p.name else 1, str(p)))
    for f in files:
        try:
            if f.stat().st_size > MAX_FILE:
                continue
            data = f.read_bytes()
        except OSError:
            continue
        for mid, blob in scan_bytes(data):
            h = hashlib.sha256(blob).hexdigest()
            found.setdefault(mid, {})[h] = blob
            where.setdefault(h, str(f))

    status = 0
    for mid in STRIP_IDS:
        copies = found.get(mid, {})
        if not copies:
            print("%03X  not found" % mid)
            if mid in MIC_STRIP:
                status = max(status, 1)
            continue
        if len(copies) > 1:
            print("%03X  %d different copies, not written:" % (mid, len(copies)))
            for h in copies:
                print("      %s  %s" % (h[:16], where[h]))
            status = 2
            continue
        (h, blob), = copies.items()
        dest = args.out / ("%03x.bill" % mid)
        print("%03X  %5d bytes  sha256 %s  from %s -> %s"
              % (mid, len(blob), h[:16], where[h], dest))
        if not args.dry_run:
            args.out.mkdir(parents=True, exist_ok=True)
            tmp = dest.with_suffix(".tmp")
            tmp.write_bytes(blob)
            os.chmod(tmp, 0o644)
            tmp.replace(dest)
    return status


if __name__ == "__main__":
    sys.exit(main())
