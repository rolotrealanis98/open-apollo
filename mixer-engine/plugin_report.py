#!/usr/bin/env python3
"""
Which UAD plug-ins this Apollo is authorized for, for the opt-in install
report (scripts/install.sh, only after the user agreed to send it).

The answer is the device's own, through plugin_authorization (the running
daemon's /plugins/N/Authorized first, else one read-only board query): the
same helper the plug-in hosting setup uses to choose what to build.  Names
are turned into render ids with the oa-assets DSP manifest.

What the report gets, and nothing else (sanitize() enforces it):
  {"schema": "open-apollo-plugin-report/1",
   "available": true,
   "authorized": [2276, ...],          # render ids the device says are authorized
   "native": {"built_mono": [...],     # of those: bundles built and passed
              "built_stereo": [...],   #   offline on this machine (plugin-mapper)
              "failed": [...]}}        # of those: tried here and not built
or {"schema": ..., "available": false, "reason": "<one of REASONS>"}.

Render ids are UA's plug-in product numbers (0x800..0xAFF).  Only plug-ins
the device calls authorized (not demos), no demo days, no serial, no
account, no device or host ids, no plug-in names, no paths, no error text.

Usage: python3 plugin_report.py [--device PATH] [--manifest FILE] [--bundles DIR]
prints the JSON on stdout; exit 0 either way.
"""

from __future__ import annotations

import argparse
import json
import logging
import os
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import plugin_authorization as pa  # noqa: E402
import plugin_license as pl  # noqa: E402

SCHEMA = "open-apollo-plugin-report/1"
REASONS = ("no_device", "no_answer")
NATIVE_KEYS = ("built_mono", "built_stereo", "failed")
RENDER_MIN, RENDER_MAX = pl.BOARD_BASE, pl.BOARD_BASE + pl.BOARD_WORDS - 1
# The plug-in maps (setup-plugin-hosting.sh): their own folder since maps v2.1,
# open-apollo/plugin-bundles before.
MAPS_DIRS = (Path.home() / ".local/share/open-apollo-plugin-maps",
             Path.home() / ".local/share/open-apollo/plugin-bundles")


def default_bundles() -> Path:
    """The maps folder that has an index, else the current one."""
    return next((d for d in MAPS_DIRS if (d / "index.json").is_file()), MAPS_DIRS[0])


def unavailable(reason: str) -> dict:
    return {"schema": SCHEMA, "available": False,
            "reason": reason if reason in REASONS else "no_answer"}


def authorized_ids(states: dict, ids_by_name: dict) -> list[int]:
    """Render ids of the plug-ins `states` ({name: state}, plugin_authorization)
    calls authorized; demos and names without a render id are left out."""
    return sorted({ids_by_name[n] for n, st in states.items()
                   if st == pa.AUTHORIZED and n in ids_by_name})


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
    layout) and which the mapper tried and could not build.  Reads both
    index schemas: "open-apollo-plugin-bundles/1" (failures keyed "Name" or
    "Name (layout)", a reason each) and /2 (keyed by name, {"reasons":
    {"mono"|"stereo"|"discovery"|"execution"|"setup": reason}}, where only a
    "setup" reason means it was never tried here)."""
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
            if not isinstance(key, str):
                continue
            if isinstance(why, dict):           # schema 2
                reasons = why.get("reasons")
                if not isinstance(reasons, dict) or not set(reasons) - {"setup"}:
                    continue
            elif why == "not installed":        # schema 1
                continue
            name = key.rsplit(" (", 1)[0] if key.endswith((" (mono)", " (stereo)")) else key
            rid = ids_by_name.get(name)
            if rid in auth:
                failed.add(rid)
    failed -= built["mono"] | built["stereo"]
    return {"built_mono": sorted(built["mono"]), "built_stereo": sorted(built["stereo"]),
            "failed": sorted(failed)}


def build(states, manifest=None, index=None) -> dict:
    """The report section for plugin_authorization's {name: state} (and the
    manifest and bundle index, when there are any)."""
    if not states:
        return unavailable("no_answer")
    ids = licence_ids_by_name(manifest or {})
    auth = authorized_ids(states, ids)
    return sanitize({"schema": SCHEMA, "available": True, "authorized": auth,
                     "native": native_status(index, ids, auth)})


def _ids(v) -> list[int]:
    if not isinstance(v, list):
        return []
    return sorted({x for x in v if type(x) is int and RENDER_MIN <= x <= RENDER_MAX})


def sanitize(doc) -> dict:
    """Exactly the documented shape: anything else in `doc` is dropped, and
    native ids that are not also authorized are dropped."""
    if not isinstance(doc, dict) or doc.get("available") is not True:
        reason = doc.get("reason") if isinstance(doc, dict) else None
        return unavailable(reason if isinstance(reason, str) else "no_answer")
    auth = _ids(doc.get("authorized"))
    native = doc.get("native") if isinstance(doc.get("native"), dict) else {}
    keep = set(auth)
    return {"schema": SCHEMA, "available": True, "authorized": auth,
            "native": {k: [x for x in _ids(native.get(k)) if x in keep] for k in NATIVE_KEYS}}


def _load(path: Path):
    try:
        with open(path, encoding="utf-8") as f:
            return json.load(f)
    except (OSError, ValueError):
        return None


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description="Authorized UAD plug-ins, for the install report")
    ap.add_argument("--device", default="/dev/ua_apollo0", help="the device node")
    ap.add_argument("--manifest", type=Path, default=pl.DEFAULT_MANIFEST)
    ap.add_argument("--bundles", type=Path, default=None,
                    help="the plug-in maps folder (its index.json); default: "
                         "~/.local/share/open-apollo-plugin-maps, else the older open-apollo/plugin-bundles")
    args = ap.parse_args(argv)
    logging.disable(logging.CRITICAL)       # nothing about the device on stderr
    answer = pa.authorized_plugins(device=args.device, manifest=args.manifest)
    doc = build(answer["plugins"], _load(args.manifest), _load((args.bundles or default_bundles()) / "index.json"))
    json.dump(doc, sys.stdout, sort_keys=True)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    os.umask(0o077)
    sys.exit(main())
