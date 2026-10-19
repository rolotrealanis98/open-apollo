#!/usr/bin/env python3
"""
Which UAD plug-ins this Apollo is authorized for, for the opt-in install
report (scripts/install.sh, only after the user agreed to send it).

The answer is the device's own: one read-only board query
(UA_IOCTL_BOARD_QUERY, hardware.HardwareBackend.board_query), decoded by
plugin_license exactly as the daemon decodes it for /plugins/N/Authorized.
Only the driver loaded with plugin_host=1 serves the query.

What the report gets, and nothing else (sanitize() enforces it):
  {"schema": "open-apollo-plugin-report/1",
   "available": true,
   "authorized": [2276, ...],          # render ids the device says are authorized
   "native": {"built_mono": [...],     # of those: bundles built and passed
              "built_stereo": [...],   #   offline on this machine (plugin-mapper)
              "failed": [...]}}        # of those: tried here and not built
or {"schema": ..., "available": false, "reason": "<one of REASONS>"}.

Render ids are UA's plug-in product numbers (0x800..0xAFF).  No licence
states other than "authorized", no demo days, no serial, no account, no
device or host ids, no plug-in names, no paths, no error text.

Usage: python3 plugin_report.py [--device PATH] [--manifest FILE] [--bundles DIR]
prints the JSON on stdout; exit 0 either way.
"""

from __future__ import annotations

import argparse
import errno
import json
import logging
import os
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import plugin_license as pl  # noqa: E402

SCHEMA = "open-apollo-plugin-report/1"
REASONS = ("no_device", "plugin_host_off", "query_failed", "bad_answer")
NATIVE_KEYS = ("built_mono", "built_stereo", "failed")
RENDER_MIN, RENDER_MAX = pl.BOARD_BASE, pl.BOARD_BASE + pl.BOARD_WORDS - 1
DEFAULT_BUNDLES = Path.home() / ".local/share/open-apollo/plugin-bundles"

_RETRY = (errno.EBUSY, errno.ESTALE, errno.ETIMEDOUT)


def unavailable(reason: str) -> dict:
    return {"schema": SCHEMA, "available": False,
            "reason": reason if reason in REASONS else "query_failed"}


def authorized_ids(words) -> list[int]:
    """Render ids whose board-table word says authorized."""
    if words is None or len(words) != pl.BOARD_WORDS:
        raise ValueError("board table of the wrong shape")
    return [pl.BOARD_BASE + i for i, w in enumerate(words)
            if pl.state_of(w) == pl.AUTHORIZED]


def licence_ids_by_name(manifest) -> dict[str, int]:
    """{plug-in name: the render id its licence is keyed by} from the plug-in
    DSP manifest; names with no or more than one id are left out."""
    out: dict[str, int] = {}
    bad: set[str] = set()
    plugins = manifest.get("plugins") if isinstance(manifest, dict) else None
    for entry in plugins if isinstance(plugins, list) else ():
        if not isinstance(entry, dict) or not isinstance(entry.get("name"), str):
            continue
        rid = pl._licence_id(entry)
        if isinstance(rid, bool) or not isinstance(rid, int):
            continue
        name = entry["name"]
        if out.get(name, rid) != rid:
            bad.add(name)
        out[name] = rid
    for name in bad:
        del out[name]
    return out


def native_status(index, ids_by_name: dict, authorized) -> dict:
    """Of the authorized render ids: which have bundles built here (per
    layout) and which the mapper tried and could not build."""
    auth = set(authorized)
    built = {"mono": set(), "stereo": set()}
    failed: set[int] = set()
    if isinstance(index, dict):
        for b in index.get("bundles") or ():
            if not isinstance(b, dict):
                continue
            rid = ids_by_name.get(b.get("name"))
            if rid in auth and b.get("layout") in built:
                built[b["layout"]].add(rid)
        failures = index.get("failures")
        for key, why in (failures.items() if isinstance(failures, dict) else ()):
            if why == "not installed" or not isinstance(key, str):
                continue
            name = key.rsplit(" (", 1)[0] if key.endswith((" (mono)", " (stereo)")) else key
            rid = ids_by_name.get(name)
            if rid in auth:
                failed.add(rid)
    failed -= built["mono"] | built["stereo"]
    return {"built_mono": sorted(built["mono"]), "built_stereo": sorted(built["stereo"]),
            "failed": sorted(failed)}


def build(words, manifest=None, index=None) -> dict:
    """The report section for a board table (and the manifest and bundle
    index, when there are any)."""
    try:
        auth = authorized_ids(words)
    except ValueError:
        return unavailable("bad_answer")
    return sanitize({"schema": SCHEMA, "available": True, "authorized": auth,
                     "native": native_status(index, licence_ids_by_name(manifest or {}), auth)})


def _ids(v) -> list[int]:
    if not isinstance(v, list):
        return []
    return sorted({x for x in v if type(x) is int and RENDER_MIN <= x <= RENDER_MAX})


def sanitize(doc) -> dict:
    """Exactly the documented shape: anything else in `doc` is dropped, and
    native ids that are not also authorized are dropped."""
    if not isinstance(doc, dict) or doc.get("available") is not True:
        reason = doc.get("reason") if isinstance(doc, dict) else None
        return unavailable(reason if isinstance(reason, str) else "query_failed")
    auth = _ids(doc.get("authorized"))
    native = doc.get("native") if isinstance(doc.get("native"), dict) else {}
    keep = set(auth)
    return {"schema": SCHEMA, "available": True, "authorized": auth,
            "native": {k: [x for x in _ids(native.get(k)) if x in keep] for k in NATIVE_KEYS}}


def query_device(device: str | None = None, attempts: int = 3):
    """The 768 board words, or the reason there are none."""
    import hardware
    be = hardware.HardwareBackend(device)
    if not be.open():
        return None, "no_device"
    try:
        if not pl.plugin_host_available(be):
            return None, "plugin_host_off"
        for i in range(attempts):
            try:
                _date, _epoch, words = be.board_query()
                return words, None
            except OSError as e:
                if e.errno == errno.EOPNOTSUPP:
                    return None, "plugin_host_off"
                if e.errno not in _RETRY or i == attempts - 1:
                    return None, "query_failed"
                time.sleep(1.0)
        return None, "query_failed"
    finally:
        be.close()


def _load(path: Path):
    try:
        with open(path, encoding="utf-8") as f:
            return json.load(f)
    except (OSError, ValueError):
        return None


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description="Authorized UAD plug-ins, for the install report")
    ap.add_argument("--device", help="the device node (default: the first /dev/ua_apollo*)")
    ap.add_argument("--manifest", type=Path, default=pl.DEFAULT_MANIFEST)
    ap.add_argument("--bundles", type=Path, default=DEFAULT_BUNDLES,
                    help="setup-plugin-hosting.sh's bundle folder (its index.json)")
    args = ap.parse_args(argv)
    logging.disable(logging.CRITICAL)       # nothing about the device on stderr
    words, reason = query_device(args.device)
    if words is None:
        doc = unavailable(reason)
    else:
        doc = build(words, _load(args.manifest), _load(args.bundles / "index.json"))
    json.dump(doc, sys.stdout, sort_keys=True)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    os.umask(0o077)
    sys.exit(main())
