#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Check that plug-in panel elements draw their sprite frames at the frames' own shape.

plugin-ui stretches a sprite frame over its element's rectangle. When the element's
shape differs from the frame's (a 24x24 LED in a 72x24 element, one 65-px strip squeezed
into two narrower elements), the panel shows stretched or squashed art. This compares
every element of every panel spec with its sprite's frame size, as the recipes record it
(console-rs/crates/oa-assets/data/plugin-recipes, output w/h of the @2x image), and lists
the elements whose aspect ratios differ by more than 10 %.

Elements already reviewed and drawn correctly are listed in tools/sprite-fit-reviewed.txt
(`<panel id> <element id>`, one per line, `#` comments); the check fails only on new ones.

Usage:
  tools/check-sprite-fit.py --specs DIR      the panel specs (an open-apollo-panels release,
                                             e.g. installed with scripts/install-panels.sh --dest DIR)
  tools/check-sprite-fit.py --specs DIR --write-reviewed
                                             rewrite the reviewed list with every current finding

Standard library only. Exit 0 clean, 1 unreviewed findings, 2 bad input.
"""
import argparse
import glob
import json
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
RECIPES = os.path.join(ROOT, "console-rs", "crates", "oa-assets", "data", "plugin-recipes")
REVIEWED = os.path.join(ROOT, "tools", "sprite-fit-reviewed.txt")
# Invisible hit zones: any shape is fine.
INVISIBLE = re.compile(r"(?i)clear|transparent|blank")
TOLERANCE = 0.10


def sprite_sizes():
    """sprite name -> (w, h) in points (recipes record the @2x pixel size)."""
    sizes = {}
    for path in glob.glob(os.path.join(RECIPES, "*.json")):
        with open(path, encoding="utf-8") as f:
            recipe = json.load(f)
        for name, sprite in (recipe.get("sprites") or {}).items():
            out = sprite.get("output") if isinstance(sprite, dict) else None
            if isinstance(out, dict) and out.get("w") and out.get("h"):
                sizes[name] = (out["w"] / 2.0, out["h"] / 2.0)
    return sizes


def elements(spec):
    stack = [spec]
    while stack:
        x = stack.pop()
        if isinstance(x, dict):
            if x.get("sprite") and isinstance(x.get("w"), (int, float)) and x.get("h"):
                yield x
            stack.extend(x.values())
        elif isinstance(x, list):
            stack.extend(x)


def fits(el, size):
    w, h = size
    frames = el.get("frames") or 1
    shapes = [(w, h / frames), (w / frames, h)] if frames > 1 else [(w, h)]
    rw, rh = float(el["w"]), float(el["h"])
    return any(fw > 0 and fh > 0 and abs((rw / fw) / (rh / fh) - 1) <= TOLERANCE for fw, fh in shapes)


def findings(specs_dir, sizes):
    out = []
    paths = sorted(glob.glob(os.path.join(specs_dir, "**", "PluginSpec_*.json"), recursive=True))
    if not paths:
        raise SystemExit("check-sprite-fit: no PluginSpec_*.json under %s" % specs_dir)
    for path in paths:
        panel = os.path.basename(path)[len("PluginSpec_"):-len(".json")]
        with open(path, encoding="utf-8") as f:
            spec = json.load(f)
        for el in elements(spec):
            if INVISIBLE.search(el["sprite"]) or el["sprite"] not in sizes:
                continue
            if not fits(el, sizes[el["sprite"]]):
                fw, fh = sizes[el["sprite"]]
                out.append((panel, str(el.get("id")), "%s: element %gx%g, sprite %gx%g, %s frame(s)" % (
                    el["sprite"], el["w"], el["h"], fw, fh, el.get("frames") or 1)))
    return out


def load_reviewed():
    reviewed = set()
    if os.path.exists(REVIEWED):
        with open(REVIEWED, encoding="utf-8") as f:
            for line in f:
                line = line.split("#", 1)[0].split()
                if len(line) >= 2:
                    reviewed.add((line[0], line[1]))
    return reviewed


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--specs", required=True, help="folder with PluginSpec_*.json")
    ap.add_argument("--write-reviewed", action="store_true", help="rewrite the reviewed list")
    args = ap.parse_args(argv)
    sizes = sprite_sizes()
    if not sizes:
        print("check-sprite-fit: no sprite sizes in %s" % RECIPES, file=sys.stderr)
        return 2
    found = findings(args.specs, sizes)
    if args.write_reviewed:
        with open(REVIEWED, "w", encoding="utf-8") as f:
            f.write("# Panel elements whose shape differs from their sprite frame but render correctly\n"
                    "# (checked by eye). tools/check-sprite-fit.py fails only on elements not listed here.\n")
            for panel, el, why in found:
                f.write("%s %s  # %s\n" % (panel, el, why))
        print("check-sprite-fit: wrote %d reviewed elements" % len(found))
        return 0
    reviewed = load_reviewed()
    new = [f for f in found if (f[0], f[1]) not in reviewed]
    for panel, el, why in new:
        print("%s %s: %s" % (panel, el, why))
    print("check-sprite-fit: %d mismatches, %d reviewed, %d new" % (len(found), len(found) - len(new), len(new)))
    return 1 if new else 0


if __name__ == "__main__":
    sys.exit(main())
