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
  docs/status/plugin-library.json
                           every UA-built UAD-2 plug-in in scope and the stage
                           its control->DSP mapping has reached, exported from
                           the library ledger with --library
  docs/status/plugins/*.json
                           measured checks per plug-in and layout, written by
                           tools/plugin-qualify.py

Outputs (docs/status/): progress-dark.svg, progress-light.svg, the
badge-*.svg, README.md (the numbers per area and the open tasks) and
PLUGINS.md (every plug-in, every check, and what each one still needs).

Usage:
  tools/gen-status-graph.py                 redraw from status.json
  tools/gen-status-graph.py --notes DIR     export status.json from the RE
                                            notes (parity/ and
                                            master-implementation-plan.md),
                                            then redraw
  tools/gen-status-graph.py --library LEDGER.json
                                            export plugin-library.json from the
                                            library ledger, then redraw
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
LIBRARY_JSON = os.path.join(OUT, "plugin-library.json")
CHECKS_DIR = os.path.join(OUT, "plugins")

# Ledger tier -> (library stage, square colour)
LIBRARY_TIERS = {
    "live_controls": ("live", "verified"), "live_smoke": ("live", "verified"),
    "ready_unlicensed": ("ready", "done"), "bundled": ("hold", "partial"),
    "live_hold": ("hold", "partial"), "offline_blocked": ("blocked", "re"),
    "unison_track": ("blocked", "re"), "license_refused": ("unlicensed", "blocked"),
}
LIBRARY_STAGES = (
    ("live", "verified", "Live on the Apollo: passed the live gate (loads, controls read back, unloads)"),
    ("ready", "done", "Mapped offline, needs a licence to test live"),
    ("hold", "partial", "Mapped, live load on hold"),
    ("blocked", "re", "Blocked"),
    ("unlicensed", "blocked", "Not authorized on the test Apollo"),
)

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


def export_library(ledger_path):
    """plugin-library.json from the private library ledger: name and stage
    only (no receipts, paths or evidence).  Plug-ins the test Apollo does not
    authorize are counted, not named."""
    with open(ledger_path, encoding="utf-8") as f:
        d = json.load(f)
    items, n = [], 0
    for p in sorted(d["plugins"], key=lambda p: p["name"]):
        stage, _ = LIBRARY_TIERS[p["tier"]]
        name = re.sub(r"^UAD ", "", p["name"])
        if stage == "unlicensed":
            n += 1
            name = "not authorized on the test Apollo (%d)" % n
        items.append([name, stage])
    return {"exported": d["generated"][:10],
            "source": "the plug-in library ledger (every UA-built UAD-2 "
                      "plug-in in scope, by pipeline stage)",
            "scope": d["in_scope"], "items": items}


# --------------------------------------------------------------------------
# Plug-in checks (docs/status/plugins/*.json -> PLUGINS.md)
# --------------------------------------------------------------------------

# Check name, column title, what it proves, what to do when it is missing
CHECKS = (
    ("map", "map", "the mapper built a runtime bundle for this layout",
     "run the mapper (`--only NAME --retry-failures`) and report the "
     "`index.json` reason; see docs/plugin-mapping.md, Improving the emulator side"),
    ("panel", "panel", "every control and meter of the panel spec has a "
     "daemon parameter or meter source behind it",
     "bind the spec's unbound controls/meters to the bundle's parameters "
     "(mixer-engine / plugin-ui)"),
    ("load", "load", "the daemon loads it on the Apollo and the recipe reads back",
     "load it with the daemon's journal open and report the refused words"),
    ("controls", "controls", "every parameter at minimum, middle and maximum "
     "is accepted and read back", "find why the listed parameters are refused "
     "(journal: plan / readback lines)"),
    ("text", "text", "each live value's text matches the text the plug-in's "
     "own formatter gave at the same position", "compare the listed "
     "parameters' formatting against the bundle's control sweep"),
    ("meters", "meters", "every meter of the panel spec moves with a signal "
     "on the input", "publish the plug-in's meter words (dynamic_refs) "
     "through the daemon"),
    ("latency", "latency", "the measured latency matches the known-good entry",
     "measure with a loopback cable and report it"),
    ("audio", "audio", "a listener heard the controls do what they say, and "
     "bypass restore the dry signal", "listen and report with "
     "`tools/plugin-qualify.py mark`"),
)
CHECK_NAMES = tuple(c[0] for c in CHECKS)
RESULTS = ("pass", "fail", "n/a")
SYMBOL = {"pass": "✅", "fail": "❌", "n/a": "–", None: "·"}


def plugin_slug(name):
    return re.sub(r"[^a-z0-9]+", "-", re.sub(r"^UAD ", "", name).lower()).strip("-")


def load_checks(path=None):
    """{plug-in name: {layout: {check: result dict}}} from docs/status/plugins."""
    out = {}
    for f in sorted(glob.glob(os.path.join(path or CHECKS_DIR, "*.json"))):
        with open(f, encoding="utf-8") as fh:
            d = json.load(fh)
        name = re.sub(r"^UAD ", "", d["plugin"])
        if plugin_slug(name) + ".json" != os.path.basename(f):
            raise SystemExit("%s: file name must be %s.json" % (f, plugin_slug(name)))
        for layout, checks in d["layouts"].items():
            for check, r in checks.items():
                if check not in CHECK_NAMES or r.get("result") not in RESULTS:
                    raise SystemExit("%s: %s %s: unknown check or result %r"
                                     % (f, layout, check, r.get("result")))
        out[name] = d["layouts"]
    return out


def layout_works(checks):
    return all(checks.get(c, {}).get("result") in ("pass", "n/a")
               for c in CHECK_NAMES)


def plugin_works(layouts):
    return bool(layouts) and all(layout_works(c) for c in layouts.values())


def first_gap(checks):
    """(check, result or None) of the first check not passed, or None."""
    for c in CHECK_NAMES:
        r = checks.get(c, {}).get("result")
        if r not in ("pass", "n/a"):
            return c, r
    return None


def cell(r):
    """Symbol, plus the detail of a failure or a count like 44/44."""
    if not r:
        return SYMBOL[None]
    detail = r.get("detail", "")
    if r["result"] == "pass" and not re.fullmatch(r"\d+/\d+", detail):
        detail = ""
    return (SYMBOL[r["result"]] + " " + detail.replace("|", "\\|")).strip()


def plugins_md(library_items, checks):
    names = [n for n, s in library_items if s != "unlicensed"]
    hidden = sum(1 for _, s in library_items if s == "unlicensed")
    names += sorted(n for n in checks if n not in names)
    names.sort(key=str.lower)
    rows, untested = [], 0
    fails = {c: [] for c in CHECK_NAMES}
    unchecked = dict.fromkeys(CHECK_NAMES, 0)
    working = 0
    for name in names:
        layouts = checks.get(name, {})
        if plugin_works(layouts):
            working += 1
        if not layouts:
            untested += 1
            rows.append("| %s | – | %s | | not checked yet |"
                        % (name, " | ".join(SYMBOL[None] for _ in CHECKS)))
            continue
        for layout in sorted(layouts):
            c = layouts[layout]
            for check in CHECK_NAMES:
                r = c.get(check, {})
                if r.get("result") == "fail":
                    fails[check].append((name, layout, r.get("detail", "")))
                elif not r:
                    unchecked[check] += 1
            gap = first_gap(c)
            if gap:
                check, r = gap
                need = "%s: %s" % (check, c.get(check, {}).get("detail") or
                                   ("failed" if r else "not checked"))
            else:
                need = ""
            rows.append("| %s | %s | %s | %s | %s |" % (
                name, layout, " | ".join(cell(c.get(k)) for k in CHECK_NAMES),
                "**yes**" if not gap else "", need.replace("|", "\\|")))
    total = len(names) + hidden
    lines = [
        "# Plug-in status",
        "",
        "Generated by `tools/gen-status-graph.py` from `plugins/*.json`, which "
        "`tools/plugin-qualify.py` writes. Do not edit by hand: run the checks "
        "and commit the JSON (docs/plugin-mapping.md, Report it).",
        "",
        "A plug-in **works** only when every check below passed (or does not "
        "apply) for every layout that was tried. Nothing here is claimed by "
        "hand: each mark is a measured result with its date, Apollo, UAD "
        "version and who ran it in the JSON file.",
        "",
        "**%d of %d plug-ins work**; %d have not been checked yet%s." % (
            working, total, untested,
            ", %d more are not authorized on the test Apollo" % hidden
            if hidden else ""),
        "",
        "| Check | Passes when | What to do when it does not |",
        "|---|---|---|",
    ]
    lines += ["| %s | %s | %s |" % (t, what, todo) for _, t, what, todo in CHECKS]
    lines += ["", "%s pass, %s fail, %s does not apply, %s not checked." % (
        SYMBOL["pass"], SYMBOL["fail"], SYMBOL["n/a"], SYMBOL[None]), ""]
    lines += ["## What fails, by check", "",
              "The work queue: each list is what a contributor can pick up. "
              "Checks nobody has run yet are counted, not listed.", ""]
    for check, _, _, todo in CHECKS:
        lines += ["### %s: %d fail, %d layouts not checked" % (
            check, len(fails[check]), unchecked[check]), ""]
        if fails[check]:
            lines += [todo, ""]
            lines += ["- %s (%s)%s" % (n, l, ": " + d if d else "")
                      for n, l, d in fails[check]]
            lines.append("")
    lines += ["## Every plug-in", "",
              "| Plug-in | Layout | %s | Works | Next |"
              % " | ".join(t for _, t, _, _ in CHECKS),
              "|---|---|%s---|---|" % ("---|" * len(CHECKS))]
    lines += rows
    lines.append("")
    return "\n".join(lines), working, total


# --------------------------------------------------------------------------
# Drawing
# --------------------------------------------------------------------------

def library_date():
    with open(LIBRARY_JSON, encoding="utf-8") as f:
        return json.load(f)["exported"]


def library_items():
    with open(LIBRARY_JSON, encoding="utf-8") as f:
        return json.load(f)["items"]


def load_library():
    """Groups for the plug-in mapping panel, one per stage; [] without the file."""
    if not os.path.exists(LIBRARY_JSON):
        return []
    with open(LIBRARY_JSON, encoding="utf-8") as f:
        d = json.load(f)
    groups = []
    for stage, st, label in LIBRARY_STAGES:
        items = [[name, name, st] for name, s in d["items"] if s == stage]
        if items:
            groups.append({"name": label, "items": items})
    return groups


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
    library = load_library()
    panels = [
        ("UA driver behaviour", data["parity"], (10, 46, 690, 404)),
        ("Work plan", data["plan"], (720, 46, 550, 300)),
        ("Apollo models", models, (720, 386, 550, 64)),
    ]
    legend_y = 469
    if library:
        panels.append(("Plug-in mapping", library, (10, 496, 1260, 150)))
        legend_y = 669
    W, H = 1280, legend_y + 23
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
        elif title == "Plug-in mapping":
            head = ("%s: %d of %d UAD plug-ins live on the Apollo, "
                    "%d more mapped (as of %s)" % (
                        title, c["verified"], total,
                        c["done"] + c["partial"], library_date()))
        out.append('<text class="h" x="%d" y="%d">%s</text>'
                   % (x + 2, y - 10, escape(head)))
        draw_treemap(groups, x, y, w, h, out)
    lx = 12
    for st in ORDER:
        out.append('<rect class="%s" x="%d" y="%d" width="12" height="12"/>'
                   % (st, lx, legend_y))
        out.append('<text class="m" x="%d" y="%d">%s</text>'
                   % (lx + 17, legend_y + 10, escape(LEGEND[st])))
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
        "- **Plug-in mapping**: one square per UA-built UAD-2 plug-in in "
        "scope (`plugin-library.json`). Its control-to-DSP mapping is "
        "produced by running UA's own code offline, never written by hand; "
        "**live** = passed the live gate on the Apollo (it loads, controls "
        "read back, it unloads), which does not mean every control, meter "
        "and text is right: [PLUGINS.md](PLUGINS.md) has the measured "
        "checks per plug-in; **mapped** = the mapping passed offline and waits for a "
        "licence or a live load; **not authorized** = the test Apollo "
        "refuses that plug-in, so it cannot be checked here. Running "
        "plug-ins on the Apollo is still being built and is not in a "
        "release yet.",
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
    library = load_library()
    if library:
        c = counts(library)
        lines += ["", "## Plug-in mapping", "",
                  "%d of %d UA-built UAD-2 plug-ins are live on the Apollo; %d "
                  "more are mapped (as of %s)." % (
                      c["verified"], sum(c.values()), c["done"] + c["partial"],
                      library_date()),
                  "", "| Stage | Plug-ins |", "|---|---|"]
        lines += ["| %s | %d |" % (g["name"], len(g["items"])) for g in library]
        _, working, total = plugins_md(library_items(), load_checks())
        lines += ["", "Measured checks (every control, text, meters, latency, "
                  "audio): %d of %d plug-ins fully work; per plug-in and what "
                  "each still needs in [PLUGINS.md](PLUGINS.md)." % (working, total)]
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
        "tools/gen-status-graph.py --library LEDGER.json   # re-export plugin-library.json, redraw",
        "tools/plugin-qualify.py ...                        # measure plug-ins (writes plugins/*.json)",
        "tools/gen-status-graph.py                          # redraw only",
        "tools/gen-status-graph.py --check                  # CI: exit 1 if stale",
        "```",
        "",
    ]
    return "\n".join(lines)


def outputs(data):
    c_par, c_plan = counts(data["parity"]), counts(data["plan"])
    c_mod = counts(load_models())
    c_lib = counts(load_library())
    p_par, p_plan = pct(c_par), pct(c_plan)
    out = {
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
    if sum(c_lib.values()):
        md, working, total = plugins_md(library_items(), load_checks())
        out["PLUGINS.md"] = md
        out["badge-plugins-working.svg"] = badge(
            "plug-ins fully working", "%d/%d" % (working, total),
            badge_color(100.0 * working / total))
        out["badge-plugins.svg"] = badge(
            "plug-ins live on the Apollo",
            "%d/%d" % (c_lib["verified"], sum(c_lib.values())),
            badge_color(pct(c_lib, ("verified",))))
    return out


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--notes", help="RE notes directory to export status.json from")
    ap.add_argument("--library", help="library ledger JSON to export plugin-library.json from")
    ap.add_argument("--check", action="store_true",
                    help="exit 1 if docs/status/ is out of date")
    args = ap.parse_args(argv)
    if args.library:
        os.makedirs(OUT, exist_ok=True)
        with open(LIBRARY_JSON, "w", encoding="utf-8") as f:
            json.dump(export_library(args.library), f, indent=1, ensure_ascii=False)
            f.write("\n")
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
