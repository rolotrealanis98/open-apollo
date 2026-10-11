#!/usr/bin/env python3
"""Unit tests for apollo-wavcheck.py on synthetic captures (no hardware).

Run:  python3 tools/test-apollo-wavcheck.py
"""

import array
import importlib.util
import math
import os
import random
import subprocess
import sys
import tempfile
import wave

HERE = os.path.dirname(os.path.abspath(__file__))
PATH = os.path.join(HERE, "apollo-wavcheck.py")
spec = importlib.util.spec_from_file_location("wavcheck", PATH)
wc = importlib.util.module_from_spec(spec)
spec.loader.exec_module(wc)

RATE, NCH, FRAMES, MON = 48000, 22, 48000 * 4, [14, 15]
rnd = random.Random(1)


def tone(freq=wc.TONE_HZ, amp=0.25):
    return [int(amp * 2 ** 31 * math.sin(2 * math.pi * freq * n / RATE))
            for n in range(FRAMES)]


def noise():
    return [rnd.randint(-2000, 2000) for _ in range(FRAMES)]


def build(cols, frames=FRAMES):
    buf = array.array("i", bytes(4 * frames * NCH))
    for ch, col in cols.items():
        buf[ch::NCH] = array.array("i", col[:frames])
    return buf


def stale_col():
    block = [rnd.randint(-2 ** 28, 2 ** 28) for _ in range(wc.STALE_LAG)]
    return (block * (FRAMES // wc.STALE_LAG + 1))[:FRAMES]


def main():
    t = tone()
    cases = [
        ("tone on MON L/R, noise elsewhere",
         build({14: t, 15: t, 0: noise()}), "PASS"),
        ("silence on MON, noise elsewhere",
         build({0: noise(), 1: noise()}), "SILENT"),
        ("every channel zero", build({}), "DEAD"),
        ("stale 8192-frame repeat on MON",
         build({14: stale_col(), 15: stale_col(), 0: noise()}), "DEAD"),
        ("tone on wrong channels (6,7)",
         build({6: t, 7: t, 14: noise(), 15: noise()}), "ROUTE"),
        ("tone on MON L only", build({14: t, 15: noise()}), "ROUTE"),
        ("MON noise, no tone", build({14: noise(), 15: noise()}), "ROUTE"),
        ("capture too short", build({14: t, 15: t}, frames=1000), "DEAD"),
    ]
    bad = 0
    for name, buf, want in cases:
        got, why, _ = wc.classify(NCH, RATE, buf, MON)
        ok = got == want
        bad += not ok
        print("%-4s %-34s want %-6s got %-6s (%s)"
              % ("ok" if ok else "FAIL", name, want, got, why))

    # gen -> classify round trip through the CLI, and the io_map parser
    with tempfile.TemporaryDirectory() as d:
        w = os.path.join(d, "g.wav")
        subprocess.run([sys.executable, PATH, "gen", "--out", w, "--rate",
                        "96000", "--seconds", "4"], check=True)
        with wave.open(w) as f:
            shape = (f.getnchannels(), f.getsampwidth(), f.getframerate(),
                     f.getnframes())
        okshape = shape == (24, 4, 96000, 384000)
        bad += not okshape
        print("%-4s gen shape %s" % ("ok" if okshape else "FAIL", shape))
        out = subprocess.run([sys.executable, PATH, "classify", w, "--mon",
                              "0,1"], check=True, capture_output=True,
                             text=True).stdout.split("\t")[0]
        ok = out == "PASS"
        bad += not ok
        print("%-4s gen file classified on its own tone channels: %s"
              % ("ok" if ok else "FAIL", out))

    text = ("io_map\t\t1\nplayback: 24 ALSA channels, DMA frame 24 slots\n"
            "  ch  0  slot  0  0x0901  MON L\n"
            "record: 22 ALSA channels, DMA frame 22 slots\n"
            "  ch  0  slot  0  0x0001  MIC/LINE/HIZ 1\n"
            "  ch 14  slot 14  0x0901  MON L\n"
            "  ch 15  slot 15  0x0902  MON R\n"
            "record descriptor: 22 slots, word5 0x0\n"
            "  slot  3  0x0901  MON L\n")
    m = wc.parse_io_map(text)
    ok = [c for c, i, _ in m["record"] if i in (0x0901, 0x0902)] == [14, 15]
    bad += not ok
    print("%-4s io_map parser finds record MON at 14,15" %
          ("ok" if ok else "FAIL"))
    print("%d failure(s)" % bad)
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
