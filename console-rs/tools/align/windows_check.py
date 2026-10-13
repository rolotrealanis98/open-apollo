#!/usr/bin/env python3
"""Checks Console's separate windows (popup menus, Settings, panels, dialogs) against drive
captures: renders each capture's state with `uaconsole --render --screen …` (composite of the
mixer and its windows, `rack::layers`) and compares every window rect with the capture's
`NAME.json` (`uadcap.sh`: `windows_window_pt`, mixer-window points).

    windows_check.py [--captures DIR] [--uaconsole BIN] [--out DIR] [NAME ...]

The engine snapshot is DIR/engine-before.json (the drive session's state). Screen geometry of
the drive Mac: mixer window at (853, 330), visible frame (0, 30, 3200, 1770) in global points,
so the screen is (-853, -300, 3200, 1770) in mixer points; `--screen` overrides it.

Prints one line per capture: each Console window, the nearest of ours, and the offset. Exit
status 1 if any window is off by more than --tol points (default 0.5).
"""

import argparse
import json
import os
import subprocess
import sys

I, A = "/devices/0/inputs", "/devices/0/auxs"
VIEW = "ctrl=1,sends=1,overview=1,scroll=1"
SETTINGS_POS = ["--window-pos", "settings=-393,-155"]

# capture name -> uaconsole arguments (besides --mock, --size, --screen, --render)
MENUS = {
    "lbl-mono": f"label:{I}/0", "lbl-mono2": f"label:{I}/1", "lbl-stereo": f"label:{I}/2", "lbl-spdif": f"label:{I}/12",
    "ins-in-empty": f"insert:{I}/0:0", "ins-aux-empty": f"insert:{A}/0:0", "unison": f"insert:{I}/0:unison",
    "solo-in": f"solo:{I}/0", "meter-in": f"meter:{I}/0@555,880", "meter-aux": f"meter:{A}/0@1056,880",
    "preamp-mic": f"preamp:{I}/0@516,151", "pan-in": f"pan:{I}/0@536,698", "pan-stereo": f"pan:{I}/2@707,698",
    "fader-in": f"fader:{I}/0@515,825", "monitor-meter": "monitor-meter@1421,300",
    "hov-lbl": f"label:{I}/0~2", "hov-ins": f"insert:{I}/0:0~1", "hov-meter": f"meter:{I}/0@555,880~1",
    "hov-pan": f"pan:{I}/0@536,698~2", "sb-rate": "rate", "sb-clock": "clock", "hov-rate": "rate~2",
}
SCENE_MENUS = {"browser-row": "scene:4@170,374", "browser-hdr": "scene:3@60,345"}
SETTING_MENUS = {
    "set-d-rate": ("hardware", "/SampleRate"), "set-d-rate-hov": ("hardware", "/SampleRate~1"),
    "set-d-clock": ("hardware", "/ClockSource"),
    "set-d-mirror": ("hardware", "bool:OFF:ON:/devices/0/outputs/18/MirrorsToDigital"),
    "set-d-idc": ("hardware", "/MaxDelayComp"), "set-d-cue": ("hardware", "/CueBusCount"),
    "set-d-alt": ("hardware", "alt:/devices/0"), "set-d-din": ("hardware", "/devices/0/TOSLink"),
    "set-d-dout": ("hardware", "/devices/0/TOSLinkOutput"),
    "set-d-mon": ("hardware", "map:mon_level:/devices/0/outputs/18/Pad"),
    "set-d-ref12": ("hardware", "pair:ref_level:/devices/0/outputs/0/Pad,/devices/0/outputs/1/Pad"),
    "set-d-ref34": ("hardware", "pair:ref_level:/devices/0/outputs/2/Pad,/devices/0/outputs/3/Pad"),
    "set-d-iomode": ("io-matrix", "map:io_mode:/IOMapUsingDefault"),
    "set-d-nin": ("io-matrix", "map:io_count:/TotalDriverInputRoutes"),
    "set-d-nout": ("io-matrix", "map:io_count:/TotalDriverOutputRoutes"),
    "set-d-metering": ("options", "map:metering:/PostFaderMetering"), "set-d-cliphold": ("options", "/ClipHold"),
    "set-d-peakhold": ("options", "/PeakHold"), "set-d-ctlmode": ("options", "/ControlsMode"),
    "set-d-mididev": ("midi", "midi:/MidiInputDevice"), "set-d-tapev": ("midi", "pref:tap_tempo_event"),
    "set-d-scenev": ("midi", "pref:scenes_event"),
}
SETTINGS_TABS = {"set-hw": "hardware", "set-tab-hw2": "hardware", "set-tab-io": "io-matrix", "set-tab-io2": "io-matrix",
                 "set-tab-options": "options", "set-tab-options2": "options", "set-tab-midi": "midi",
                 "set-tab-midi2": "midi", "set-tab-network": "network"}
PANELS = {
    "panel-cue": ["--open", "cue-outputs"],
    "panel-monctl": ["--open", "monitor-controller", "--window-pos", "panel:monitor-controller=-459,-300"],
    "sb-tempo": ["--open", "tempo"],
    "send-cell": ["--open", "input-sends:0"],
    "sends-aux": ["--open", "aux-sends:0"],
    # Windows Settings opens over itself (Settings stays open, at its remembered place).
    "set-adddev": ["--open", "settings:hardware+add-device", *SETTINGS_POS],
    "set-d-iopresets": ["--open", "settings:io-matrix+io-presets", *SETTINGS_POS],
    "set-d-insrc": ["--open", "settings:io-matrix+input-source:1", *SETTINGS_POS],
}


def args_for(name):
    if name in MENUS:
        return ["--view", VIEW, "--menu", MENUS[name]]
    if name in SCENE_MENUS:
        return ["--view", VIEW + ",scenes=1", "--menu", SCENE_MENUS[name]]
    if name in SETTING_MENUS:
        tab, key = SETTING_MENUS[name]
        return ["--view", VIEW, "--open", f"settings:{tab}", *SETTINGS_POS, "--menu", f"setting={key}"]
    if name in SETTINGS_TABS:
        return ["--view", VIEW, "--open", f"settings:{SETTINGS_TABS[name]}", *SETTINGS_POS]
    if name in PANELS:
        return ["--view", VIEW, *PANELS[name]]
    return None


def nearest(r, ours):
    def d(o):
        return abs(o["x"] - r["x"]) + abs(o["y"] - r["y"]) + abs(o["w"] - r["w"]) + abs(o["h"] - r["h"])
    return min(ours, key=d) if ours else None


def main():
    here = os.path.dirname(os.path.abspath(__file__))
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("names", nargs="*")
    ap.add_argument("--captures", default=os.path.expanduser("~/wine-ua-work/console-port/refshots/drive-20261012"))
    ap.add_argument("--uaconsole", default=os.path.join(here, "../../target/release/uaconsole"))
    ap.add_argument("--out", default="/tmp/windows_check")
    ap.add_argument("--screen", default="-853,-300,3200,1770")
    ap.add_argument("--tol", type=float, default=0.5)
    a = ap.parse_args()
    os.makedirs(a.out, exist_ok=True)
    names = a.names or sorted(n for n in {**MENUS, **SCENE_MENUS, **SETTING_MENUS, **SETTINGS_TABS, **PANELS}
                              if os.path.exists(os.path.join(a.captures, n + ".json")))
    bad = 0
    for n in names:
        extra = args_for(n)
        ref_path = os.path.join(a.captures, n + ".json")
        if extra is None or not os.path.exists(ref_path):
            print(f"{n}: no spec or capture")
            continue
        png = os.path.join(a.out, n + ".png")
        cmd = [a.uaconsole, "--mock", os.path.join(a.captures, "engine-before.json"), "--size", "1470x1105",
               "--screen", a.screen, *extra, "--render", png]
        p = subprocess.run(cmd, capture_output=True, text=True)
        if p.returncode != 0:
            print(f"{n}: render failed: {p.stderr.strip().splitlines()[-1] if p.stderr.strip() else p.returncode}")
            bad += 1
            continue
        ours = [w for w in json.load(open(os.path.join(a.out, n + ".layers.json")))["windows_window_pt"] if w["id"] != "main"]
        ref = json.load(open(ref_path))
        cons = [w for w in ref["windows_window_pt"] if w["layer"] != 0 or not w["title"].startswith("UAD Console")]
        cons = [w for w in cons if not (w["x"] == 0 and w["y"] == 0 and w["w"] == 1470)]
        parts = []
        for c in cons:
            o = nearest(c, ours)
            if o is None:
                parts.append(f"{c['title']}@{c['x']},{c['y']} {c['w']}x{c['h']}: ours none")
                bad += 1
                continue
            dx, dy, dw, dh = (o["x"] - c["x"], o["y"] - c["y"], o["w"] - c["w"], o["h"] - c["h"])
            ok = max(abs(dx), abs(dy), abs(dw), abs(dh)) <= a.tol
            bad += not ok
            parts.append(f"{c['title']}@{c['x']},{c['y']} {c['w']}x{c['h']} vs {o['id']}@{o['x']:g},{o['y']:g} {o['w']:g}x{o['h']:g}"
                         + ("" if ok else f"  OFF d=({dx:g},{dy:g},{dw:g},{dh:g})"))
        print(f"{n}: " + ("; ".join(parts) if parts else "no windows"))
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
