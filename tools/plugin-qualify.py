#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Measure what each UAD plug-in really does from Linux, and record it in
docs/status/plugins/<plug-in>.json (rendered to docs/status/PLUGINS.md by
tools/gen-status-graph.py).

  offline   map, panel, meters-n/a: from the mapper's bundles and the panel
            specs; needs no Apollo
  live      load, controls, text, meters: loads each plug-in the daemon offers
            into an EMPTY insert slot, sets every parameter to the three
            positions the mapper swept, compares the text with the plug-in's
            own formatter, reads its meters, unloads.  Run it attended, with
            the monitors down: it changes what the Apollo plays.
  mark      record a check measured by hand (audio, latency, or any other)

Each mark carries its date, Apollo, sample rate, UAD version and who ran it.
A new run replaces only the checks it ran.  Nothing from UA (names of
parameters, words, paths) is written: counts and parameter ordinals only.

Examples:
  tools/plugin-qualify.py offline
  tools/plugin-qualify.py live --device "Apollo x4" --only "UAD Pultec EQP-1A"
  tools/plugin-qualify.py live --device "Apollo x4" --signal   # tone on input 1
  tools/plugin-qualify.py mark "Pultec EQP-1A" mono audio pass --device "Apollo x4"
  tools/gen-status-graph.py

Standard library only.
"""
import argparse
import datetime
import glob
import json
import os
import re
import socket
import sys
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, "docs", "status", "plugins")
MAPS = os.path.expanduser("~/.local/share/open-apollo-plugin-maps")
SPECS = "/usr/share/open-apollo/plugin-specs"
CHECKS = ("map", "panel", "load", "controls", "text", "meters", "latency", "audio")
HOW = "plugin-qualify/1"
NUL = b"\x00"


def short(name):
    return re.sub(r"^UAD ", "", name)


def slug(name):
    return re.sub(r"[^a-z0-9]+", "-", short(name).lower()).strip("-")


def scrub(text):
    """No home paths or long digit runs in a committed detail."""
    text = re.sub(r"(/home|/Users)/[^/\s]+", "~", str(text))
    return re.sub(r"\d{12,}", "…", text)[:160]


def ordinals(ns):
    return ", ".join(str(n) for n in ns[:12]) + (" …" if len(ns) > 12 else "")


# --------------------------------------------------------------------------
# Result files
# --------------------------------------------------------------------------

class Recorder:
    def __init__(self, args):
        self.out = args.out
        self.meta = {k: v for k, v in (
            ("date", datetime.date.today().isoformat()),
            ("device", getattr(args, "device", None)),
            ("rate", getattr(args, "rate", None)),
            ("uad", args.uad), ("by", args.by), ("how", args.how)) if v}
        self.written = set()

    def record(self, name, layout, check, result, detail=""):
        assert check in CHECKS and result in ("pass", "fail", "n/a"), (check, result)
        path = os.path.join(self.out, slug(name) + ".json")
        d = {"plugin": short(name), "layouts": {}}
        if os.path.exists(path):
            with open(path, encoding="utf-8") as f:
                d = json.load(f)
        r = {"result": result}
        if detail:
            r["detail"] = scrub(detail)
        r.update(self.meta)
        d["layouts"].setdefault(layout, {})[check] = r
        d["layouts"] = {l: dict(sorted(c.items(), key=lambda kv: CHECKS.index(kv[0])))
                        for l, c in sorted(d["layouts"].items())}
        os.makedirs(self.out, exist_ok=True)
        with open(path, "w", encoding="utf-8") as f:
            json.dump(d, f, indent=1, ensure_ascii=False)
            f.write("\n")
        self.written.add(path)
        print("  %-8s %-6s %-4s %s" % (layout, check, result, detail))


# --------------------------------------------------------------------------
# Offline: bundles and panel specs
# --------------------------------------------------------------------------

def load_specs(specs_dir):
    out = {}
    for f in sorted(glob.glob(os.path.join(specs_dir, "PluginSpec_*.json"))):
        with open(f, encoding="utf-8") as fh:
            s = json.load(fh)
        out[s["effectName"]] = s
    return out


def spec_ordinals(o, acc=None):
    """Every parameter ordinal a panel element uses (param / params)."""
    acc = set() if acc is None else acc
    if isinstance(o, dict):
        for k, v in o.items():
            if k == "param" and isinstance(v, int):
                acc.add(v)
            elif k == "params" and isinstance(v, list):
                acc.update(x for x in v if isinstance(x, int))
            else:
                spec_ordinals(v, acc)
    elif isinstance(o, list):
        for v in o:
            spec_ordinals(v, acc)
    return acc


def spec_has_meters(spec):
    return bool(spec.get("meterSources")) or '"spectrum"' in json.dumps(spec)


def panel_check(spec, n_params):
    """(result, detail): every element's ordinal and every spec parameter
    exists in the bundle."""
    used = spec_ordinals(spec.get("elements"))
    unbound = sorted(k for k in used if k >= n_params)
    n_spec = len(spec.get("parameters") or [])
    if unbound:
        return "fail", "%d/%d panel controls bound; unbound ordinals %s" % (
            len(used) - len(unbound), len(used), ordinals(unbound))
    if n_spec != n_params:
        return "fail", "panel spec has %d parameters, bundle %d" % (n_spec, n_params)
    hidden = sorted(set(range(n_params)) - used)
    if hidden:
        return "fail", "%d/%d parameters have a panel control; none for ordinals %s" % (
            n_params - len(hidden), n_params, ordinals(hidden))
    return "pass", "%d/%d" % (n_params, n_params)


def load_bundle(maps, entry):
    with open(os.path.join(maps, os.path.basename(entry["file"])), encoding="utf-8") as f:
        return json.load(f)


def offline(args, rec):
    with open(os.path.join(args.maps, "index.json"), encoding="utf-8") as f:
        index = json.load(f)
    specs = load_specs(args.specs)
    names = set(args.only or [])
    want = (lambda n: not names or n in names or short(n) in names)
    for e in sorted(index["bundles"], key=lambda e: (e["name"], e["layout"])):
        if not want(e["name"]):
            continue
        print(e["name"])
        bundle = load_bundle(args.maps, e)
        n = len(bundle["control_sweep"])
        rec.record(e["name"], e["layout"], "map", "pass", "%d parameters" % n)
        spec = specs.get(e["name"])
        if spec is None:
            rec.record(e["name"], e["layout"], "panel", "fail", "no panel spec")
            continue
        rec.record(e["name"], e["layout"], "panel", *panel_check(spec, n))
        if not spec_has_meters(spec):
            rec.record(e["name"], e["layout"], "meters", "n/a", "no meters on the panel")
    for name, f in sorted(index.get("failures", {}).items()):
        if not want(name):
            continue
        print(name)
        for stage, reason in sorted(f.get("reasons", {}).items()):
            for layout in ((stage,) if stage in ("mono", "stereo") else ("mono", "stereo")):
                rec.record(name, layout, "map", "fail", "%s: %s" % (stage, reason))
    for name in index.get("waiting_for_licence", []):
        if want(name):
            print("%s: waits for a licence, not mapped" % name)


# --------------------------------------------------------------------------
# Live: the mixer daemon (port 4710, NUL-framed text commands)
# --------------------------------------------------------------------------

class DaemonError(Exception):
    def __init__(self, msg):
        self.code = msg.get("error", "")
        self.detail = msg.get("detail", "")
        super().__init__("%s %s" % (self.code, self.detail))


class Daemon:
    def __init__(self, host, port):
        self.sock = socket.create_connection((host, port), timeout=10)
        self.buf = b""
        self.func = 1000

    def _recv(self, timeout):
        end = time.monotonic() + timeout
        while True:
            while NUL in self.buf:
                raw, self.buf = self.buf.split(NUL, 1)
                if raw:
                    try:
                        msg = json.loads(raw.decode("utf-8", "replace"))
                    except ValueError:
                        continue
                    if isinstance(msg, dict):
                        return msg
            left = end - time.monotonic()
            if left <= 0:
                raise TimeoutError("daemon did not answer in %d s" % timeout)
            self.sock.settimeout(left)
            data = self.sock.recv(65536)
            if not data:
                raise ConnectionError("daemon closed the connection")
            self.buf += data

    def _wait(self, path, func, timeout):
        while True:
            msg = self._recv(timeout)
            if msg.get("path") != path:
                continue
            if func is not None and (msg.get("parameters") or {}).get("func_id") != func:
                continue
            if "error" in msg:
                raise DaemonError(msg)
            return msg.get("data")

    def get(self, path, query="", timeout=10):
        self.sock.sendall(("get %s%s" % (path, query)).encode() + NUL)
        return self._wait(path, None, timeout)

    def set(self, path, value, timeout=30):
        self.func += 1
        self.sock.sendall(("set %s?func_id=%d %s" % (path, self.func, json.dumps(value)))
                          .encode() + NUL)
        return self._wait(path, self.func, timeout)


def catalogue(d):
    """{name: {layout: [slot paths]}} of the plug-ins the daemon offers."""
    out, n = {}, 0
    while True:
        base = "/plugins/%d" % n
        try:
            name = d.get(base + "/Name/value")
        except DaemonError:
            return out
        n += 1
        try:
            if not d.get(base + "/NativeLoadSupported/value"):
                continue
            slots = {l: [s for s in (d.get(base + "/Native%sSlots/value" % l.title()) or "")
                         .split(",") if s] for l in ("mono", "stereo")}
        except DaemonError:
            continue
        out[name] = {l: s for l, s in slots.items() if s}


def free_slot(d, slots):
    for s in slots:
        try:
            if not d.get(s + "/EffectName/value"):
                return s
        except DaemonError:
            pass
    return None


def sweep_positions(bundle):
    """{ordinal: [(normalized value, expected text)]} from the mapper's sweep."""
    out = {}
    for c in bundle["control_sweep"]:
        m = c["maximum"] or 1
        out[c["index"]] = [(s["integer"] / m, s.get("formatted", {}).get("console_trimmed"))
                           for s in c["samples"]]
    return out


def meter_queries(spec):
    qs = []
    for src in (spec.get("meterSources") or {}).values():
        if "vendorSpecificFloat" in src:
            qs.append("?floats=%d" % src["vendorSpecificFloat"])
        elif "opcode" in src:
            op = src["opcode"]
            qs.append("?opcode=%d&p0=%d&p1=%d&p2=1" % (op["op"], op["p0"], op.get("p1", 1)))
    return qs


def check_controls(d, slot, positions, rec, name, layout):
    """Sets every swept position; controls = accepted, text = formatter matches."""
    params = (d.get(slot + "/parameters", "?recursive=1") or {}).get("children", {})
    n = len(params)
    if n != len(positions):
        rec.record(name, layout, "controls", "fail",
                   "daemon lists %d parameters, bundle %d" % (n, len(positions)))
    refused, wrong_text, tried_text = [], [], 0
    for k in sorted(positions):
        p = "%s/parameters/%d/NormalizedValue/value" % (slot, k)
        try:
            original = d.get(p)
        except DaemonError:
            refused.append(k)
            continue
        ok = True
        for value, text in positions[k]:
            try:
                d.set(p, value)
            except DaemonError as e:
                if "indeterminate" in e.code:
                    raise
                ok = False
                continue
            if text is not None:
                tried_text += 1
                live = d.get("%s/parameters/%d/StringValue/value" % (slot, k))
                if str(live).strip() != text.strip():
                    wrong_text.append(k)
        if not ok:
            refused.append(k)
        try:
            d.set(p, original)
        except DaemonError:
            pass
    total = len(positions)
    if n == total:
        rec.record(name, layout, "controls", "fail" if refused else "pass",
                   "%d/%d%s" % (total - len(refused), total,
                                "; refused ordinals " + ordinals(refused) if refused else ""))
    wrong = sorted(set(wrong_text))
    if tried_text:
        rec.record(name, layout, "text", "fail" if wrong else "pass",
                   "%d/%d%s" % (total - len(wrong), total,
                                "; differs at ordinals " + ordinals(wrong) if wrong else ""))


def check_meters(d, slot, spec, signal, rec, name, layout):
    qs = meter_queries(spec)
    if not qs:
        return
    reads = []
    for _ in range(10 if signal else 1):
        try:
            reads.append([d.get(slot + "/VendorSpecific/value", q) for q in qs])
        except DaemonError as e:
            rec.record(name, layout, "meters", "fail", "meter read refused: %s %s"
                       % (e.code.replace("openapollo.", ""), e.detail))
            return
        time.sleep(0.2)
    if not signal:
        print("  %-8s meters readable; rerun with --signal to check they move" % layout)
        return
    moving = sum(1 for i in range(len(qs)) if len({str(r[i]) for r in reads}) > 1)
    rec.record(name, layout, "meters", "pass" if moving == len(qs) else "fail",
               "%d/%d move with a signal" % (moving, len(qs)))


def live(args, rec):
    with open(os.path.join(args.maps, "index.json"), encoding="utf-8") as f:
        bundles = {(e["name"], e["layout"]): e for e in json.load(f)["bundles"]}
    specs = load_specs(args.specs)
    d = Daemon(args.host, args.port)
    if not d.get("/NativePluginHosting/value"):
        sys.exit("plug-in hosting is off: %s" % d.get("/NativePluginHostingReason/value"))
    rec.meta["rate"] = d.get("/SampleRate/value")
    offered = catalogue(d)
    names = set(args.only or [])
    for name, layouts in sorted(offered.items()):
        if names and name not in names and short(name) not in names:
            continue
        for layout, slots in sorted(layouts.items()):
            if args.layout and layout != args.layout:
                continue
            entry = bundles.get((name, layout))
            slot = free_slot(d, slots)
            if entry is None or slot is None:
                print("%s %s: skipped (%s)" % (name, layout,
                      "no bundle" if entry is None else "no empty slot"))
                continue
            print("%s (%s, %s)" % (name, layout, slot))
            try:
                d.set(slot + "/EffectName/value", name, timeout=600)
            except DaemonError as e:
                rec.record(name, layout, "load", "fail", e.detail or e.code)
                continue
            try:
                check_controls(d, slot, sweep_positions(load_bundle(args.maps, entry)),
                               rec, name, layout)
                if name in specs:
                    check_meters(d, slot, specs[name], args.signal, rec, name, layout)
            except DaemonError as e:
                rec.record(name, layout, "load", "fail", "hosting quarantined: " + e.code)
                sys.exit("hosting quarantined; restart the daemon (journalctl --user -u ua-mixer-daemon)")
            try:
                d.set(slot + "/EffectName/value", "", timeout=120)
                rec.record(name, layout, "load", "pass", "loads, reads back, unloads")
            except DaemonError as e:
                rec.record(name, layout, "load", "fail", "unload refused: " + (e.detail or e.code))


def mark(args, rec):
    rec.meta["how"] = "by hand"
    rec.record(args.plugin, args.layout, args.check, args.result, args.detail or "")


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0],
                                 formatter_class=argparse.RawDescriptionHelpFormatter,
                                 epilog=__doc__.split("\n", 1)[1])
    ap.add_argument("--out", default=OUT, help="result directory (default: %(default)s)")
    ap.add_argument("--maps", default=MAPS, help="mapper output (default: %(default)s)")
    ap.add_argument("--specs", default=SPECS, help="panel specs (default: %(default)s)")
    ap.add_argument("--uad", help="UAD version the maps were built from, e.g. 11.8.3")
    ap.add_argument("--by", help="your GitHub handle, recorded with each result")
    ap.set_defaults(how=HOW)
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("offline", help="map and panel checks; no Apollo")
    p.add_argument("--only", action="append", metavar="NAME")
    p = sub.add_parser("live", help="load, controls, text and meters on the Apollo")
    p.add_argument("--device", required=True, help='Apollo model, e.g. "Apollo x4"')
    p.add_argument("--only", action="append", metavar="NAME")
    p.add_argument("--layout", choices=("mono", "stereo"))
    p.add_argument("--signal", action="store_true",
                   help="a signal is on the input: check that meters move")
    p.add_argument("--host", default="127.0.0.1")
    p.add_argument("--port", type=int, default=4710)
    p = sub.add_parser("mark", help="record a check measured by hand")
    p.add_argument("plugin")
    p.add_argument("layout", choices=("mono", "stereo"))
    p.add_argument("check", choices=CHECKS)
    p.add_argument("result", choices=("pass", "fail", "n/a"))
    p.add_argument("--detail", help="what you measured, e.g. 55 samples")
    p.add_argument("--device", required=True)
    p.add_argument("--rate", type=int, default=48000)
    args = ap.parse_args(argv)
    rec = Recorder(args)
    {"offline": offline, "live": live, "mark": mark}[args.cmd](args, rec)
    if rec.written:
        print("\n%d file(s) in %s; now run tools/gen-status-graph.py"
              % (len(rec.written), os.path.relpath(args.out, ROOT)))


if __name__ == "__main__":
    main()
