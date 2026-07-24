#!/usr/bin/env python3
"""
Convert a raw UA Mixer Engine control-tree capture into a flat device map.

`dump_x8p_tree.py` captures the engine's control tree as a nested object
(``controls`` -> ``{properties, children, commands}`` recursively).  The mixer
daemon's :meth:`StateTree.load_device_map`, however, expects ``controls`` to be
a **flat list** of ``{path, type, min, max, values, value}`` entries — the same
shape as ``device_maps/device_map_apollo_x4.json``.

This tool flattens the tree into that list so a fresh capture can be dropped
straight into ``mixer-engine/device_maps/``.  It is device-agnostic: point it at
any ``dump_*_tree.py`` output.

Usage:
    python3 tree_to_device_map.py capture.json -o device_map_apollo_x8p.json
    python3 tree_to_device_map.py capture.json            # writes device_map_<name>.json
"""
import argparse
import json
import sys
from pathlib import Path

# Keys the daemon reads off every flat control entry (order matches the x4 map).
PROP_KEYS = ("type", "min", "max", "values", "value")


def flatten(node, path, out):
    """Depth-first walk: emit one flat entry per property, recurse into children.

    ``path`` is the node's slash path ("" at the root).  A property named ``P``
    on a node at ``/a/b`` becomes ``/a/b/P``; a child named ``c`` becomes the
    node ``/a/b/c``.  ``commands`` are engine RPCs, not state — skipped (the x4
    map has none either).
    """
    if not isinstance(node, dict):
        return

    props = node.get("properties") or {}
    for name, p in _items(props):
        if not isinstance(p, dict):
            continue
        entry = {"path": f"{path}/{name}"}
        for k in PROP_KEYS:
            entry[k] = p.get(k)
        out.append(entry)

    children = node.get("children") or {}
    for name, child in _items(children):
        flatten(child, f"{path}/{name}", out)


def _items(container):
    """Yield (name, value) whether the container is a dict or a list."""
    if isinstance(container, dict):
        return container.items()
    if isinstance(container, list):
        return enumerate(container)
    return []


def convert(capture: dict) -> dict:
    controls_tree = capture.get("controls", {})
    flat = []
    flatten(controls_tree, "", flat)
    return {
        "device_name": capture.get("device_name", "Unknown"),
        "host": capture.get("host"),
        "port": capture.get("port"),
        "timestamp": capture.get("timestamp"),
        "tree_path": capture.get("tree_path", "/"),
        "controls": flat,
    }


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("capture", type=Path, help="raw tree capture JSON (dump_*_tree.py output)")
    ap.add_argument("-o", "--out", type=Path,
                    help="output device map (default: device_map_<device_name>.json)")
    args = ap.parse_args()

    capture = json.loads(args.capture.read_text())
    if not isinstance(capture.get("controls"), dict):
        sys.exit("error: capture 'controls' is not a tree — is this already a flat device map?")

    device_map = convert(capture)

    out = args.out
    if out is None:
        slug = device_map["device_name"].lower().replace(" ", "_")
        out = args.capture.with_name(f"device_map_{slug}.json")
    out.write_text(json.dumps(device_map, indent=1))

    print(f"wrote {out}: {len(device_map['controls'])} controls "
          f"for {device_map['device_name']!r}", file=sys.stderr)


if __name__ == "__main__":
    main()
