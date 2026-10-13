"""Report newer ConsoleLink runtime features that Rust has not implemented.

Reads local JSON only; never connects to an engine or copies proprietary assets.
Usage: python3 plugin_runtime_gaps.py SPEC_FOLDER > report.json
This is a feature inventory, not a visual or DSP acceptance test.

Superseded on branch rc/plugin-spec-v2: plugin-ui now ports every feature listed here (see
crates/plugin-ui/README.md, "Spec runtime"); crates/plugin-ui/tests/coverage.rs checks the whole
spec contract key by key and tests/catalog.rs builds and exercises every spec.
"""
import json
import sys
from collections import Counter
from pathlib import Path


def inventory(folder):
    report = []
    for file in sorted(folder.glob("PluginSpec_*.json")):
        spec = json.loads(file.read_text())
        gaps = Counter()

        def action(value):
            kind = value.get("type", "unknown")
            if kind not in ("set", "nudge", "toggle", "default", "table", "multi"):
                gaps["action:" + kind] += 1
            for sub in value.get("actions", []):
                action(sub)

        for element in spec.get("elements", []):
            if element["kind"] == "graph":
                gaps["graph"] += 1
            for key in ("touchState", "pressedFrameOffset", "segments", "blink", "longPressAction"):
                if element.get(key) is not None:
                    gaps[key] += 1
            for key in ("spriteFont", "meter"):
                if element.get("text", {}).get(key):
                    gaps["text:" + key] += 1
            for table in ("visibility", "frameSource"):
                for key in ("states", "meters"):
                    if element.get(table, {}).get(key):
                        gaps[table + ":" + key] += 1
            if element.get("frameSource", {}).get("type") == "blink":
                gaps["blinkFrame"] += 1
            if element.get("gesture") == "swipe":
                gaps["swipe"] += 1
            if element.get("action"):
                action(element["action"])
        report.append({"plugin": spec["effectName"], "file": file.name, "gaps": dict(gaps)})
    return {
        "specs": len(report),
        "with_unported_features": sum(bool(row["gaps"]) for row in report),
        "feature_counts": dict(sum((Counter(row["gaps"]) for row in report), Counter())),
        "plugins": report,
    }


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("usage: plugin_runtime_gaps.py SPEC_FOLDER")
    print(json.dumps(inventory(Path(sys.argv[1])), indent=2))
