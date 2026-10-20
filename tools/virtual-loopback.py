#!/usr/bin/env python3
"""virtual-loopback.py - play a stimulus into Virtual inputs and measure what they record.

A Virtual input (DAW playback VIRTUAL n) runs through its Console strip, and through any
plug-in chain on it, to its own record channel (VIRTUAL n): a digital loopback with no cable.
This plays the same stimulus into each lane at once and reports, per lane, whether it came
back, its level, and its delay and difference against a reference lane (a Virtual input with
no plug-in): a plug-in's latency and whether it changes the sound.

  stimulus: 0.25 s silence, an impulse at -6 dBFS, 0.5 s silence, 997 Hz at -18 dBFS for 1 s

Needs the Apollo's ALSA card to itself (stop PipeWire first:
`systemctl --user stop pipewire.socket pipewire wireplumber`), and turn the monitors down:
the lanes also reach the monitor mix.

Examples:
  tools/virtual-loopback.py                       # Virtual 1 against Virtual 2
  tools/virtual-loopback.py --lanes 1,3,4 --ref 2 --json out.json

Python 3 standard library only.
"""
import argparse
import array
import importlib.util
import json
import math
import os
import subprocess
import sys
import tempfile
import wave

RATE = 48000
FS = 2147483648.0
IMPULSE_AT, SINE_AT, SINE_LEN, TAIL = 0.25, 0.75 + 0.25, 1.0, 0.5
SINE_HZ, SINE_DB, IMPULSE_DB = 997.0, -18.0, -6.0
ONSET = 10 ** (-30 / 20)     # first sample this loud is the impulse's arrival
PRESENT_DB = -60.0

_spec = importlib.util.spec_from_file_location(
    "wavcheck", os.path.join(os.path.dirname(os.path.abspath(__file__)), "apollo-wavcheck.py"))
wavcheck = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(wavcheck)


def stimulus():
    n = int(RATE * (SINE_AT + SINE_LEN + TAIL))
    x = [0.0] * n
    x[int(RATE * IMPULSE_AT)] = 10 ** (IMPULSE_DB / 20)
    a = 10 ** (SINE_DB / 20)
    start = int(RATE * SINE_AT)
    for i in range(int(RATE * SINE_LEN)):
        x[start + i] = a * math.sin(2 * math.pi * SINE_HZ * i / RATE)
    return x


def lanes_from_io_map(text, lanes):
    """{lane: (playback ch, record ch)} for VIRTUAL n, and the two channel counts."""
    tab = wavcheck.parse_io_map(text)
    out = {}
    for n in lanes:
        name = "VIRTUAL %d" % n
        play = [ch for ch, _, nm in tab["playback"] if nm == name]
        rec = [ch for ch, _, nm in tab["record"] if nm == name]
        if not play or not rec:
            sys.exit("%s has no playback or record channel in the io_map" % name)
        out[n] = (play[0], rec[0])
    return out, len(tab["playback"]), len(tab["record"])


def write_wav(path, nch, channels, x):
    buf = array.array("i", bytes(4 * len(x) * nch))
    for c in channels:
        buf[c::nch] = array.array("i", (int(round(v * (FS - 1))) for v in x))
    if sys.byteorder == "big":
        buf.byteswap()
    with wave.open(path, "wb") as w:
        w.setnchannels(nch)
        w.setsampwidth(4)
        w.setframerate(RATE)
        w.writeframes(buf.tobytes())


def read_channels(path, wanted):
    with wave.open(path, "rb") as w:
        nch = w.getnchannels()
        buf = array.array("i", w.readframes(w.getnframes()))
    if sys.byteorder == "big":
        buf.byteswap()
    return {c: [v / FS for v in buf[c::nch]] for c in wanted}


def db(v):
    return 20 * math.log10(v) if v > 0 else -math.inf


def rms(x):
    return math.sqrt(sum(v * v for v in x) / len(x)) if x else 0.0


def analyze(record, lanes, ref):
    """record: {lane: samples}.  Per lane: onset (samples into the take), sine level (dBFS),
    delay against the reference lane, and the difference from it (dB under its level)."""
    sine_from = int(RATE * (SINE_AT - IMPULSE_AT + 0.25))   # after the impulse, into the steady tone
    sine_len = int(RATE * (SINE_LEN - 0.5))
    result = {}
    for n in lanes:
        x = record[n]
        onset = next((i for i, v in enumerate(x) if abs(v) >= ONSET), None)
        level = db(rms(x[onset + sine_from:onset + sine_from + sine_len])) if onset is not None else -math.inf
        result[n] = {"onset": onset, "level_db": round(level, 2),
                     "present": onset is not None and level > PRESENT_DB}
    r = result[ref]
    for n in lanes:
        e = result[n]
        if n == ref or not (e["present"] and r["present"]):
            continue
        e["delay"] = e["onset"] - r["onset"]
        a = record[n][e["onset"] + sine_from:e["onset"] + sine_from + sine_len]
        b = record[ref][r["onset"] + sine_from:r["onset"] + sine_from + sine_len]
        e["difference_db"] = round(db(rms([u - v for u, v in zip(a, b)])) - db(rms(b)), 2)
    return result


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0],
                                 formatter_class=argparse.RawDescriptionHelpFormatter,
                                 epilog=__doc__.split("\n", 1)[1])
    ap.add_argument("--card", default="x4", help="ALSA card id (default: %(default)s)")
    ap.add_argument("--lanes", default="1", help="Virtual inputs to measure, e.g. 1,3")
    ap.add_argument("--ref", type=int, default=2, help="Virtual input with no plug-in (default: 2)")
    ap.add_argument("--json", help="also write the results here")
    a = ap.parse_args(argv)
    lanes = sorted({int(n) for n in a.lanes.split(",")} | {a.ref})
    with open("/proc/asound/%s/io_map" % a.card, encoding="utf-8") as f:
        io, n_play, n_rec = lanes_from_io_map(f.read(), lanes)
    x = stimulus()
    seconds = len(x) / RATE
    with tempfile.TemporaryDirectory() as tmp:
        play, take = os.path.join(tmp, "play.wav"), os.path.join(tmp, "take.wav")
        write_wav(play, n_play, [io[n][0] for n in lanes], x)
        rec = subprocess.Popen(["arecord", "-q", "-D", "hw:%s" % a.card, "-f", "S32_LE", "-r", str(RATE),
                                "-c", str(n_rec), "-d", str(math.ceil(seconds + 1)), take])
        played = subprocess.run(["aplay", "-q", "-D", "hw:%s" % a.card, play])
        if rec.wait() or played.returncode:
            sys.exit("aplay/arecord failed: is PipeWire still holding the card? "
                     "(systemctl --user stop pipewire.socket pipewire wireplumber)")
        channels = read_channels(take, [io[n][1] for n in lanes])
    result = analyze({n: channels[io[n][1]] for n in lanes}, lanes, a.ref)
    for n in lanes:
        e = result[n]
        extra = "" if n == a.ref else ("  delay %+d samples, difference %s dB" % (
            e["delay"], e["difference_db"]) if "delay" in e else "  (not compared)")
        print("Virtual %d%s: %s, %s dBFS%s" % (n, " (reference)" if n == a.ref else "",
              "present" if e["present"] else "SILENT", e["level_db"], extra))
    if a.json:
        with open(a.json, "w", encoding="utf-8") as f:
            json.dump({str(n): v for n, v in result.items()}, f, indent=1)
    return 0 if all(result[n]["present"] for n in lanes) else 1


if __name__ == "__main__":
    sys.exit(main())
