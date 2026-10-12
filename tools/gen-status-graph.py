#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Draw the project status map in docs/status/.

Every square is one thing UA's own drivers do (or one task of the work
plan, or one Apollo model), coloured by how sure we are of it:

  verified  done, and seen working on real hardware
  done      done in code, not yet checked on hardware
  partial   partly done, done differently, or in progress
  re        still to figure out: needs reverse engineering
  todo      understood, not done yet
  plugin    part of plugin hosting / the DSP strip engine, which is being
            built
  blocked   needs hardware (or a test on it) or another decision

Inputs:
  docs/status/status.json  the parity map (each behaviour of UA's macOS kext,
                           Mixer Engine and Windows drivers, with the Linux
                           status) and the work plan, exported from the
                           project's reverse-engineering notes with --notes
  devices/*.json           the models and whether each is hardware-verified

Outputs (docs/status/): progress-dark.svg, progress-light.svg, the three
badge-*.svg and README.md (the numbers per area and the open tasks).

Usage:
  tools/gen-status-graph.py                 redraw from status.json
  tools/gen-status-graph.py --notes DIR     export status.json from the RE
                                            notes (parity/ and
                                            master-implementation-plan.md),
                                            then redraw
  tools/gen-status-graph.py --check         exit 1 if docs/status/ is stale

Standard library only.  Output is deterministic.
"""
import argparse
import datetime
import glob
import json
import math
import os
import re
import sys
from xml.sax.saxutils import escape, quoteattr

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, "docs", "status")
STATUS_JSON = os.path.join(OUT, "status.json")

ORDER = ("verified", "done", "partial", "re", "todo", "plugin", "blocked")
LEGEND = {
    "verified": "verified on hardware",
    "done": "done in code",
    "partial": "partial / in progress",
    "re": "still to figure out (RE)",
    "todo": "understood, not done",
    "plugin": "plugin hosting (in progress)",
    "blocked": "needs hardware",
}
THEMES = {
    "dark": {"bg": "#0d1117", "fg": "#e6edf3", "muted": "#8b949e",
             "verified": "#3fb950", "done": "#1f6f3a",
             "partial": "#d29922", "re": "#a371f7", "todo": "#484f58",
             "plugin": "#2f81f7", "blocked": "#f85149"},
    "light": {"bg": "#ffffff", "fg": "#1f2328", "muted": "#59636e",
              "verified": "#1a7f37", "done": "#7ccf8e",
              "partial": "#d4a72c", "re": "#8250df", "todo": "#c8d1da",
              "plugin": "#54aeff", "blocked": "#cf222e"},
}

# --------------------------------------------------------------------------
# Export from the RE notes
# --------------------------------------------------------------------------

PARITY_AREAS = {
    "01": "User client", "02": "Kext classes", "03": "HW interface",
    "04": "Mixer Engine", "05": "Windows+USB", "10": "Plugin Support",
    "11": "Lifecycle", "12": "Transport+clock", "13": "DSP+firmware",
    "14": "Mixer params", "15": "Device mgr", "16": "Model types",
    "17": "Services", "18": "Windows PCIe",
}
PLAN_AREAS = {
    "A": "Daemon", "B": "Daemon lifecycle", "C": "Installer",
    "D": "Transport+clock", "E": "HW interface", "F": "Lifecycle",
    "G": "DSP firmware", "H": "Other models", "I": "Digital I/O",
    "J": "USB", "K": "Multi-unit", "L": "Driver API", "M": "Mixer params",
    "N": "Audit gaps", "O": "Every Apollo", "S": "Input strips",
}
# Plugin hosting (P) needs a separate decision and is not tracked here
PLAN_SKIP = {"P"}
# Nothing about licensing or authorization goes into this repo
SCRUB = re.compile(r"licen[cs]|authori[sz]|crack|ilok|codex|demo", re.I)
HW = re.compile(r"titan|hardware[- ]verified|verified on|owners? (?:report|verif)",
                re.I)
NOT_HW = re.compile(r"untest|not run on|unverified|no titan|needs (?:a |an |the )?"
                    r"(?:real |titan|hw|hardware)", re.I)
NEEDS_RE = re.compile(r"\[L\]|\bunknown\b|needs (?:mac |windows )?RE\b|"
                      r"not decoded|undecoded|hypothes", re.I)
NEEDS_GEAR = re.compile(r"user hw|second apollo|usb unit|cable|other model",
                        re.I)
TOKEN = re.compile(r"\b(DONE|PARTIAL|DIFFERENT|WRONG|CACHE-ONLY|MISSING|N/A|"
                   r"OUT-OF-SCOPE|PLUGIN-HOST|BLOCKED-HW)\b")


def md_cells(line):
    return [c.strip() for c in re.split(r"(?<!\\)\|", line.strip())[1:-1]]


def md_tables(text):
    """(header cells, data rows) for every markdown table in text."""
    lines = text.split("\n")
    i = 0
    while i < len(lines) - 1:
        if (lines[i].startswith("|") and
                re.match(r"^\|\s*:?-{2,}", lines[i + 1])):
            head = [c.lower() for c in md_cells(lines[i])]
            rows = []
            i += 2
            while i < len(lines) and lines[i].startswith("|"):
                rows.append(md_cells(lines[i]))
                i += 1
            yield head, rows
        else:
            i += 1


def clean(s, n=72):
    s = re.sub(r"\[([^\]]*)\]\([^)]*\)", r"\1", s)
    s = re.sub(r"\*\*|`|\\", "", s)
    s = re.sub(r"\b0x[0-9a-fA-F]{5,}\b", "", s)
    s = re.sub(r"\s+", " ", s).strip(" ,;:.-")
    if SCRUB.search(s):
        return "(not listed)"
    return s if len(s) <= n else s[:n - 1].rstrip() + "…"


def hw_checked(text):
    return bool(HW.search(text)) and not NOT_HW.search(text)


def parity_status(tok, text):
    if tok == "DONE":
        return "verified" if hw_checked(text) else "done"
    if tok in ("PARTIAL", "DIFFERENT", "WRONG"):
        return "partial"
    if tok == "PLUGIN-HOST":
        return "plugin"
    if tok == "BLOCKED-HW":
        return "re" if NEEDS_RE.search(text) else "blocked"
    if tok in ("MISSING", "CACHE-ONLY"):    # CACHE-ONLY: nothing is sent
        return "re" if NEEDS_RE.search(text) else "todo"
    return None                     # N/A, OUT-OF-SCOPE: not counted


def export_parity(notes):
    groups = []
    for path in sorted(glob.glob(os.path.join(notes, "parity", "*.md"))):
        key = os.path.basename(path)[:2]
        items = []
        with open(path, encoding="utf-8") as f:
            text = f.read()
        for head, rows in md_tables(text):
            col = next((i for i, h in enumerate(head) if "status" in h), None)
            if not col:             # no status column, or a legend table
                continue
            for r in rows:
                if col >= len(r):
                    continue
                m = TOKEN.search(r[col].replace("*", ""))
                if not m:
                    continue
                st = parity_status(m.group(1), " ".join(r))
                if st is None:
                    continue
                desc = next((c for c in r[1:4] if len(clean(c)) > 3 and
                             not re.fullmatch(r"[0-9a-fA-Fx +/-]+", c)), "")
                items.append([clean(r[0], 16), clean(desc), st])
        if items:
            groups.append({"name": PARITY_AREAS.get(key, key), "items": items})
    return groups


def plan_status(needs, state):
    s = state.lower()
    if s.startswith(("split into", "moved to", "superseded", "reframed")):
        return None                 # bookkeeping rows
    if s.startswith(("in progress", "narrowed")):
        return "partial"
    if s.startswith(("blocked", "not now")):
        return "plugin" if re.search(r"strip|plugin", needs + " " + s,
                                     re.I) else "blocked"
    if s.startswith(("done", "closed", "answered", "decided", "checked",
                     "works", "not reproduced")):
        return "verified" if hw_checked(state) else "done"
    if "done" in s:
        return "partial"
    if NEEDS_GEAR.search(needs):
        return "blocked"
    if re.search(r"\bRE\b", needs):
        return "re"
    return "todo"


def export_plan(notes):
    path = os.path.join(notes, "master-implementation-plan.md")
    with open(path, encoding="utf-8") as f:
        text = f.read()
    groups, ws = [], None
    for line in text.split("\n"):
        m = re.match(r"^## ([A-Z])\. ", line)
        if m:
            ws = m.group(1)
            continue
        if line.startswith("## "):
            ws = None
        if not ws or ws in PLAN_SKIP or not re.match(r"^\| [A-Z]\d", line):
            continue
        c = md_cells(line)
        st = plan_status(c[-2], c[-1])
        if st is None:
            continue
        if not groups or groups[-1]["key"] != ws:
            groups.append({"key": ws, "name": PLAN_AREAS.get(ws, ws),
                           "items": []})
        groups[-1]["items"].append([c[0], clean(c[1]), st])
    for g in groups:
        del g["key"]
    return groups


def export(notes):
    return {
        "exported": datetime.date.today().isoformat(),
        "source": "the project's reverse-engineering notes: the parity map "
                  "(14 sections on UA's macOS kext, Mixer Engine and Windows "
                  "drivers) and the master implementation plan",
        "parity": export_parity(notes),
        "plan": export_plan(notes),
    }


# --------------------------------------------------------------------------
# Drawing
# --------------------------------------------------------------------------

def load_models():
    groups = {"Thunderbolt": [], "USB": []}
    for path in sorted(glob.glob(os.path.join(ROOT, "devices", "*.json"))):
        with open(path, encoding="utf-8") as f:
            d = json.load(f)
        st = "verified" if d.get("status") == "verified" else "done"
        bus = "USB" if "usb" in os.path.basename(path) else "Thunderbolt"
        groups[bus].append([os.path.basename(path)[:-5], d["model"], st])
    return [{"name": k, "items": v} for k, v in groups.items() if v]


def counts(groups):
    c = dict.fromkeys(ORDER, 0)
    for g in groups:
        for it in g["items"]:
            c[it[2]] += 1
    return c


def pct(c, keys=("verified", "done")):
    total = sum(c.values())
    return 100.0 * sum(c[k] for k in keys) / total if total else 0.0


def squarify(sizes, x, y, w, h):
    """Bruls/Huizing/van Wijk squarified treemap: one rect per size."""
    total = float(sum(sizes))
    areas = [s * w * h / total for s in sizes]
    rects, i = [], 0

    def worst(row, side):
        s = sum(row)
        return max(max(side * side * a / (s * s), s * s / (side * side * a))
                   for a in row)

    while i < len(areas):
        side = min(w, h)
        row = [areas[i]]
        i += 1
        while i < len(areas) and worst(row + [areas[i]], side) <= worst(row, side):
            row.append(areas[i])
            i += 1
        s = sum(row)
        if w >= h:
            rw, yy = s / h, y
            for a in row:
                rects.append((x, yy, rw, a / rw))
                yy += a / rw
            x, w = x + rw, w - rw
        else:
            rh, xx = s / w, x
            for a in row:
                rects.append((xx, y, a / rh, rh))
                xx += a / rh
            y, h = y + rh, h - rh
    return rects


def grid(n, w, h):
    """Columns and rows that give n cells the largest smaller side."""
    best = (0.0, 1, n)
    for cols in range(1, n + 1):
        rows = math.ceil(n / cols)
        score = min(w / cols, h / rows)
        if score > best[0]:
            best = (score, cols, rows)
    return best[1], best[2]


def f1(v):
    return ("%.1f" % v).rstrip("0").rstrip(".")


def draw_treemap(groups, x, y, w, h, out):
    groups = sorted(groups, key=lambda g: -len(g["items"]))
    rects = squarify([len(g["items"]) for g in groups], x, y, w, h)
    for g, (gx, gy, gw, gh) in zip(groups, rects):
        gx, gy, gw, gh = gx + 1, gy + 1, gw - 2, gh - 2
        items = sorted(g["items"], key=lambda it: ORDER.index(it[2]))
        cols, rows = grid(len(items), gw, gh)
        cw, ch = gw / cols, gh / rows
        gap = 1 if min(cw, ch) > 4 else 0
        for k, (ident, label, st) in enumerate(items):
            cx, cy = gx + (k % cols) * cw, gy + (k // cols) * ch
            tip = "%s: %s %s (%s)" % (g["name"], ident, label, LEGEND[st])
            out.append('<rect class="%s" x="%s" y="%s" width="%s" height="%s">'
                       '<title>%s</title></rect>'
                       % (st, f1(cx), f1(cy), f1(cw - gap), f1(ch - gap),
                          escape(tip)))
        chars = int((gw - 6) / 6.6)
        if chars >= 3 and gh >= 15:
            name = g["name"] if len(g["name"]) <= chars else g["name"][:chars]
            out.append('<text class="lbl" x="%s" y="%s">%s</text>'
                       % (f1(gx + 3), f1(gy + 12), escape(name)))


def render(data, theme):
    t = THEMES[theme]
    models = load_models()
    panels = [
        ("UA driver behaviour", data["parity"], (10, 46, 690, 404)),
        ("Work plan", data["plan"], (720, 46, 550, 300)),
        ("Apollo models", models, (720, 386, 550, 64)),
    ]
    W, H = 1280, 492
    out = ['<svg xmlns="http://www.w3.org/2000/svg" width="%d" height="%d" '
           'viewBox="0 0 %d %d" font-family="ui-monospace,SFMono-Regular,'
           'Menlo,Consolas,monospace">' % (W, H, W, H),
           "<title>open-apollo status map (%s)</title>" % escape(data["exported"]),
           "<style>"
           ".lbl{font-size:11px;fill:%s;paint-order:stroke;"
           "stroke:%s;stroke-width:3px;stroke-linejoin:round}"
           ".h{font-size:14px;fill:%s}.m{font-size:11px;fill:%s}"
           % (t["fg"], t["bg"], t["fg"], t["muted"])
           + "".join(".%s{fill:%s}" % (k, t[k]) for k in ORDER)
           + "</style>",
           '<rect width="%d" height="%d" rx="6" fill="%s"/>' % (W, H, t["bg"])]
    for title, groups, (x, y, w, h) in panels:
        c = counts(groups)
        total = sum(c.values())
        head = "%s: %s%% done (%d/%d)" % (
            title, f1(pct(c)), c["verified"] + c["done"], total)
        if title == "Apollo models":
            head = "%s: %d of %d verified on hardware" % (
                title, c["verified"], total)
        out.append('<text class="h" x="%d" y="%d">%s</text>'
                   % (x + 2, y - 10, escape(head)))
        draw_treemap(groups, x, y, w, h, out)
    lx = 12
    for st in ORDER:
        out.append('<rect class="%s" x="%d" y="469" width="12" height="12"/>'
                   % (st, lx))
        out.append('<text class="m" x="%d" y="479">%s</text>'
                   % (lx + 17, escape(LEGEND[st])))
        lx += 17 + int(len(LEGEND[st]) * 6.6) + 18
    out.append('<text class="m" x="%d" y="18" text-anchor="end">as of %s</text>'
               % (W - 12, escape(data["exported"])))
    out.append("</svg>")
    return "\n".join(out) + "\n"


def badge(label, value, color):
    lw = int(len(label) * 6.6) + 12
    vw = int(len(value) * 6.6) + 12
    w = lw + vw
    return (
        '<svg xmlns="http://www.w3.org/2000/svg" width="%d" height="20" '
        'role="img" aria-label=%s>' % (w, quoteattr("%s: %s" % (label, value))) +
        "<title>%s: %s</title>" % (escape(label), escape(value)) +
        '<rect width="%d" height="20" fill="#555"/>' % lw +
        '<rect x="%d" width="%d" height="20" fill="%s"/>' % (lw, vw, color) +
        '<g fill="#fff" font-family="Verdana,DejaVu Sans,sans-serif" '
        'font-size="11" text-anchor="middle">'
        '<text x="%s" y="14">%s</text><text x="%s" y="14">%s</text></g></svg>\n'
        % (f1(lw / 2), escape(label), f1(lw + vw / 2), escape(value)))


def badge_color(p):
    for limit, color in ((90, "#2ea44f"), (70, "#7aa83b"), (50, "#c9a227"),
                         (30, "#d4762c")):
        if p >= limit:
            return color
    return "#cf3d3d"


def area_table(groups):
    rows = ["| Area | " + " | ".join(LEGEND[s] for s in ORDER) +
            " | Total | Done |", "|---" * (len(ORDER) + 3) + "|"]
    for g in sorted(groups, key=lambda g: -len(g["items"])):
        c = counts([g])
        rows.append("| %s | %s | %d | %s%% |" % (
            g["name"], " | ".join(str(c[s]) for s in ORDER),
            sum(c.values()), f1(pct(c))))
    c = counts(groups)
    rows.append("| **All** | %s | **%d** | **%s%%** |" % (
        " | ".join("**%d**" % c[s] for s in ORDER), sum(c.values()),
        f1(pct(c))))
    return rows


def readme(data):
    models = load_models()
    lines = [
        "# Project status map",
        "",
        "Generated by `tools/gen-status-graph.py` from `status.json` (exported "
        "%s from %s) and `devices/*.json`. Do not edit by hand."
        % (data["exported"], data["source"]),
        "",
        '<picture>',
        '  <source media="(prefers-color-scheme: dark)" srcset="progress-dark.svg">',
        '  <img alt="Status map: one square per item, coloured by status" '
        'src="progress-light.svg">',
        '</picture>',
        "",
        "Each square is one item. Open `progress-light.svg` on its own to "
        "see each square's name when you point at it.",
        "",
        "- **UA driver behaviour**: one square per behaviour of UA's macOS "
        "kext, Mixer Engine and Windows drivers in the parity map, with the "
        "Linux status. Things Linux does not need (N/A) and UA's licensing "
        "(out of scope, never analysed) are not counted.",
        "- **Work plan**: one square per task of the master implementation "
        "plan. The plugin-hosting tasks are tracked separately and not shown.",
        "- **Apollo models**: one square per model in `devices/`; green ones "
        "have per-model support in the code but no owner has tested them yet.",
        "",
        "Colours: **verified** = done and seen working on real hardware (the "
        "note cites a hardware run); **done** = done in code; **partial** = "
        "partly done, done differently or in progress; **still to figure "
        "out** = needs reverse engineering; **understood, not done** = we "
        "know what UA's driver does, Linux does not do it yet; **plugin "
        "hosting** = part of hosting UA's DSP plugins and Console strips, "
        "which is being built; **needs hardware** = needs a "
        "test on an Apollo or hardware the project does not have (a second "
        "Apollo, other models, cables, USB units, an S3-capable host).",
        "",
        "The parity map was triaged against the code on 2026-10-11: rows the "
        "code already covers were marked done, rows Linux does not need N/A, "
        "and every remaining row says what it waits on.",
        "",
        "## UA driver behaviour (parity map)",
        "",
    ]
    lines += area_table(data["parity"])
    lines += ["", "## Work plan", ""]
    lines += area_table(data["plan"])
    for st, title in (("re", "Still to figure out"), ("partial", "In progress"),
                      ("plugin", "Plugin hosting (in progress)"),
                      ("blocked", "Needs hardware"),
                      ("todo", "Understood, not done")):
        open_items = [(g["name"], it) for g in data["plan"]
                      for it in g["items"] if it[2] == st]
        if not open_items:
            continue
        lines += ["", "### %s (%d)" % (title, len(open_items)), ""]
        lines += ["- **%s** %s (%s)" % (it[0], it[1], name)
                  for name, it in open_items]
    lines += ["", "## Apollo models", "", "| Model | Status |", "|---|---|"]
    for g in models:
        for slug, name, st in g["items"]:
            lines.append("| %s | %s |" % (
                name, "verified on hardware" if st == "verified"
                else "supported in code, untested"))
    lines += [
        "",
        "## Updating",
        "",
        "```",
        "tools/gen-status-graph.py --notes <RE notes dir>   # re-export status.json, redraw",
        "tools/gen-status-graph.py                          # redraw only",
        "tools/gen-status-graph.py --check                  # CI: exit 1 if stale",
        "```",
        "",
    ]
    return "\n".join(lines)


def outputs(data):
    c_par, c_plan = counts(data["parity"]), counts(data["plan"])
    c_mod = counts(load_models())
    p_par, p_plan = pct(c_par), pct(c_plan)
    return {
        "progress-dark.svg": render(data, "dark"),
        "progress-light.svg": render(data, "light"),
        "badge-parity.svg": badge("UA driver parity", "%s%%" % f1(p_par),
                                  badge_color(p_par)),
        "badge-plan.svg": badge("work plan", "%s%%" % f1(p_plan),
                                badge_color(p_plan)),
        "badge-models.svg": badge(
            "models verified", "%d/%d" % (c_mod["verified"], sum(c_mod.values())),
            "#0969da"),
        "README.md": readme(data),
    }


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--notes", help="RE notes directory to export status.json from")
    ap.add_argument("--check", action="store_true",
                    help="exit 1 if docs/status/ is out of date")
    args = ap.parse_args(argv)
    if args.notes:
        data = export(args.notes)
        os.makedirs(OUT, exist_ok=True)
        with open(STATUS_JSON, "w", encoding="utf-8") as f:
            json.dump(data, f, indent=1, ensure_ascii=False)
            f.write("\n")
    with open(STATUS_JSON, encoding="utf-8") as f:
        data = json.load(f)
    stale = []
    for name, text in outputs(data).items():
        path = os.path.join(OUT, name)
        old = None
        if os.path.exists(path):
            with open(path, encoding="utf-8") as f:
                old = f.read()
        if old == text:
            continue
        if args.check:
            stale.append(name)
        else:
            with open(path, "w", encoding="utf-8") as f:
                f.write(text)
    if stale:
        print("docs/status is out of date (%s): run tools/gen-status-graph.py"
              % ", ".join(stale), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
