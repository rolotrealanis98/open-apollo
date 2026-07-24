#!/usr/bin/env python3
"""Minify committed device maps without changing what StateTree/HelperTree build.

Two code paths, auto-selected per file:

  device_map_*.json  (dict with a "controls" list)
      - minify (no pretty whitespace)
      - drop 6 never-read capture-metadata keys: timestamp, host, port,
        propfilter, message_count, tree_path (load_device_map reads only
        device_name + controls; see state_tree.py)
      - per control, drop type/min/max/values ONLY when null. NEVER drop
        "value": _insert_control gates it on presence, not non-null
        (state_tree.py), and 217 controls carry an explicit "value": null that
        must survive as 217 subscribable paths.

  helper_tree.json  (anything else)
      - minify ONLY. Its read_only:false and nulls are load-bearing
        (helper_tree.py); stripping would corrupt it.

Insertion order is preserved (no sort_keys) because ubjson_codec.encode depends
on it for byte-identical UBJSON. Writes are atomic (tmp + os.replace).

Idempotent: re-running on an already-minified file is a no-op in content.

Usage:  minify-device-map.py FILE [FILE ...]
"""
import json
import os
import sys

# Top-level keys that no code reads. host is 127.0.0.1; nothing sensitive lost.
_METADATA_KEYS = ("timestamp", "host", "port", "propfilter",
                  "message_count", "tree_path")
# The only control keys safe to drop when null. "value" is deliberately absent.
_STRIPPABLE = ("type", "min", "max", "values")


def _strip_control(ctrl):
    """Return ctrl with null type/min/max/values removed, order preserved,
    'value' always kept (even when null)."""
    out = {}
    for k, v in ctrl.items():
        if k in _STRIPPABLE and v is None:
            continue
        out[k] = v
    return out


def _process(obj):
    """Return (new_obj, is_device_map)."""
    if isinstance(obj, dict) and isinstance(obj.get("controls"), list):
        controls = obj["controls"]
        value_before = sum("value" in c for c in controls)
        new = {k: v for k, v in obj.items() if k not in _METADATA_KEYS}
        new["controls"] = [_strip_control(c) for c in controls]
        value_after = sum("value" in c for c in new["controls"])
        assert value_after == value_before, (
            f"value-key count changed: {value_before} -> {value_after}")
        assert len(new["controls"]) == len(controls), "control count changed"
        return new, True
    return obj, False


def _write_atomic(path, payload):
    tmp = path + ".tmp"
    with open(tmp, "w", encoding="utf-8") as f:
        f.write(payload)
    os.replace(tmp, path)  # atomic on POSIX


def minify_file(path):
    with open(path, encoding="utf-8") as f:
        obj = json.load(f)  # dict preserves insertion order (CPython 3.7+)
    new, is_dm = _process(obj)
    payload = json.dumps(new, separators=(",", ":"), ensure_ascii=False)
    _write_atomic(path, payload)
    kind = "device-map (stripped)" if is_dm else "helper-tree (minified only)"
    print(f"{os.path.basename(path)}: {kind}, {len(payload)} bytes")


def main(argv):
    if len(argv) < 2:
        print(__doc__)
        return 2
    for path in argv[1:]:
        minify_file(path)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
