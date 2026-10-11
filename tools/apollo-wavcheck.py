#!/usr/bin/env python3
"""apollo-wavcheck.py - tone generator and capture classifier for apollo-selftest.sh

Python 3 standard library only.

  gen       write a multi-channel S32_LE WAV with a sine on chosen channels
  mapids    read /proc/asound/cardN/io_map text and print the record (or
            playback) ALSA channel numbers carrying given channel IDs
  classify  classify a recorded WAV as PASS / SILENT / DEAD / ROUTE

Verdicts (MON = the record channels with ID 0x0901 / 0x0902):
  PASS    the test tone is present on every MON channel
  SILENT  the capture is live (other channels carry data) but MON is all zero
  DEAD    nothing live: capture empty/short, every channel all zero, or a
          channel is a frozen repeat (STALE_PCT or more of its non-zero
          samples equal the sample 8192 frames earlier)
  ROUTE   MON carries data but not the tone on both channels (tone missing,
          on one side only, or showing up on a non-MON channel)
"""

import argparse
import array
import math
import re
import sys
import wave

STALE_LAG = 8192          # frames; the DMA ring period we test against
STALE_PCT = 90.0          # a channel this stale is a frozen buffer
TONE_HZ = 997.0           # prime-ish: its period never divides 8192 frames
TONE_DBFS = -12.0
TONE_FRAC_MIN = 0.5       # share of channel energy that must be the tone
TONE_AMP_MIN = 1e-4       # full-scale fraction; below this it is noise
FULL_SCALE = 2147483648.0


# ---------------------------------------------------------------- generate

def cmd_gen(a):
    rate, nch = a.rate, a.channels
    tone_ch = [int(c) for c in a.tone_channels.split(",") if c != ""]
    for c in tone_ch:
        if not 0 <= c < nch:
            sys.exit("tone channel %d outside 0..%d" % (c, nch - 1))
    amp = 10 ** (a.level / 20.0) * (FULL_SCALE - 1)
    frames = int(rate * a.seconds)
    one = [int(round(amp * math.sin(2 * math.pi * a.freq * n / rate)))
           for n in range(frames)]
    buf = array.array("i", bytes(4 * frames * nch))
    for c in tone_ch:
        buf[c::nch] = array.array("i", one)
    if sys.byteorder == "big":
        buf.byteswap()
    with wave.open(a.out, "wb") as w:
        w.setnchannels(nch)
        w.setsampwidth(4)
        w.setframerate(rate)
        w.writeframes(buf.tobytes())


# ------------------------------------------------------------------ io_map

def parse_io_map(text):
    """Return {'playback': [(ch, id, name)], 'record': [...]} from the
    ua_apollo io_map proc text (the 'ch N slot S 0xID name' lines)."""
    out = {"playback": [], "record": []}
    cur = None
    for line in text.splitlines():
        m = re.match(r"^(playback|record):\s+(\d+) ALSA channels", line)
        if m:
            cur = m.group(1)
            continue
        if re.match(r"^(playback|record) descriptor", line):
            cur = None          # slot lines below are descriptor slots
            continue
        m = re.match(r"^\s+ch\s+(\d+)\s+slot\s+\d+\s+0x([0-9a-fA-F]+)\s*(.*)$",
                     line)
        if m and cur:
            out[cur].append((int(m.group(1)), int(m.group(2), 16),
                             m.group(3).strip()))
    return out


def cmd_mapids(a):
    with open(a.io_map, encoding="utf-8", errors="replace") as f:
        tab = parse_io_map(f.read())[a.direction]
    ids = [int(x, 0) for x in a.ids.split(",")]
    found = []
    for want in ids:
        hit = [ch for ch, cid, _ in tab if cid == want]
        if not hit:
            sys.exit("channel ID 0x%04x not in the %s map" % (want, a.direction))
        found.append(str(hit[0]))
    print(",".join(found))


# ---------------------------------------------------------------- classify

def read_wav(path):
    with wave.open(path, "rb") as w:
        if w.getsampwidth() != 4:
            raise ValueError("not 32-bit samples")
        nch, rate = w.getnchannels(), w.getframerate()
        raw = w.readframes(w.getnframes())
    raw = raw[: len(raw) - len(raw) % (4 * nch)]
    s = array.array("i")
    s.frombytes(raw)
    if sys.byteorder == "big":
        s.byteswap()
    return nch, rate, s


def goertzel_frac(x, rate, freq):
    """Return (tone amplitude as full-scale fraction, share of energy)."""
    n = len(x)
    if n == 0:
        return 0.0, 0.0
    w = 2 * math.pi * freq / rate
    coeff = 2 * math.cos(w)
    s1 = s2 = 0.0
    energy = 0.0
    for v in x:
        s0 = v + coeff * s1 - s2
        s2, s1 = s1, s0
        energy += float(v) * v
    power = s1 * s1 + s2 * s2 - coeff * s1 * s2      # |X|^2
    amp = 2.0 * math.sqrt(max(power, 0.0)) / n
    tone_energy = amp * amp / 2.0 * n                # sum of squares in tone
    frac = tone_energy / energy if energy > 0 else 0.0
    return amp / FULL_SCALE, min(frac, 1.0)


def channel_stats(ch, rate, freq):
    n = len(ch)
    nz = n - ch.count(0)
    st = {"frames": n, "nonzero_pct": 100.0 * nz / n if n else 0.0,
          "stale_pct": 0.0, "tone_amp": 0.0, "tone_frac": 0.0, "tone": False}
    if nz == 0:
        return st
    if n > STALE_LAG:
        same = sum(1 for x, y in zip(ch[STALE_LAG:], ch[:-STALE_LAG])
                   if x == y and x != 0)
        st["stale_pct"] = 100.0 * same / nz
    # analyse the middle half: skips the stream start/stop edges
    win = ch[n // 4: n // 4 + min(n // 2, int(rate * 0.5))]
    st["tone_amp"], st["tone_frac"] = goertzel_frac(win, rate, freq)
    st["tone"] = (st["tone_frac"] >= TONE_FRAC_MIN
                  and st["tone_amp"] >= TONE_AMP_MIN)
    return st


def classify(nch, rate, samples, mon, freq=TONE_HZ, min_seconds=1.0):
    """Return (verdict, reason, per-channel stats list)."""
    frames = len(samples) // nch if nch else 0
    if frames < max(STALE_LAG * 2, int(rate * min_seconds)):
        return "DEAD", "capture too short (%d frames)" % frames, []
    for m in mon:
        if not 0 <= m < nch:
            return "DEAD", "MON channel %d not in a %d-channel capture" % (
                m, nch), []
    stats = [channel_stats(samples[c::nch], rate, freq) for c in range(nch)]
    if all(s["nonzero_pct"] == 0 for s in stats):
        return "DEAD", "every channel is all zero", stats
    frozen = [c for c, s in enumerate(stats) if s["stale_pct"] >= STALE_PCT]
    if frozen:
        return ("DEAD", "ch %s repeats the sample %d frames earlier"
                % (",".join(map(str, frozen)), STALE_LAG), stats)
    mon_live = [stats[m]["nonzero_pct"] > 0 for m in mon]
    if not any(mon_live):
        return "SILENT", "MON is all zero while other channels are live", stats
    mon_tone = [stats[m]["tone"] for m in mon]
    if all(mon_tone):
        return "PASS", "tone on every MON channel", stats
    elsewhere = [c for c, s in enumerate(stats) if s["tone"] and c not in mon]
    if elsewhere:
        return ("ROUTE", "tone on ch %s, not on MON"
                % ",".join(map(str, elsewhere)), stats)
    if any(mon_tone):
        return ("ROUTE", "tone on MON ch %s only"
                % ",".join(str(m) for m, t in zip(mon, mon_tone) if t), stats)
    return "ROUTE", "MON carries data but no tone", stats


def cmd_classify(a):
    try:
        nch, rate, samples = read_wav(a.wav)
    except (OSError, wave.Error, ValueError, EOFError) as e:
        print("DEAD\tcannot read capture: %s" % e)
        return 0
    mon = [int(c) for c in a.mon.split(",")]
    freq = a.freq
    verdict, reason, stats = classify(nch, rate, samples, mon, freq)
    print("%s\t%s" % (verdict, reason))
    if a.verbose:
        print("  ch  nonzero%  stale%   tone_dBFS  tone_frac")
        for c, s in enumerate(stats):
            db = (20 * math.log10(s["tone_amp"]) if s["tone_amp"] > 0
                  else float("-inf"))
            print("  %2d%s %7.1f %7.1f %10.1f %9.2f" % (
                c, "*" if c in mon else " ", s["nonzero_pct"],
                s["stale_pct"], db, s["tone_frac"]))
    return 0


def main():
    p = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = p.add_subparsers(dest="cmd", required=True)

    g = sub.add_parser("gen", help="write a tone WAV (S32_LE)")
    g.add_argument("--out", required=True)
    g.add_argument("--rate", type=int, required=True)
    g.add_argument("--channels", type=int, default=24)
    g.add_argument("--seconds", type=float, default=4.0)
    g.add_argument("--freq", type=float, default=TONE_HZ)
    g.add_argument("--level", type=float, default=TONE_DBFS,
                   help="tone level in dBFS (default %(default)s)")
    g.add_argument("--tone-channels", default="0,1",
                   help="comma list of 0-based channels (default %(default)s)")
    g.set_defaults(fn=cmd_gen)

    m = sub.add_parser("mapids", help="channel numbers for channel IDs")
    m.add_argument("--io-map", required=True)
    m.add_argument("--direction", choices=("record", "playback"),
                   default="record")
    m.add_argument("--ids", default="0x0901,0x0902")
    m.set_defaults(fn=cmd_mapids)

    c = sub.add_parser("classify", help="PASS/SILENT/DEAD/ROUTE a capture")
    c.add_argument("wav")
    c.add_argument("--mon", required=True,
                   help="comma list of MON record channels, 0-based")
    c.add_argument("--freq", type=float, default=TONE_HZ)
    c.add_argument("-v", "--verbose", action="store_true")
    c.set_defaults(fn=cmd_classify)

    a = p.parse_args()
    return a.fn(a) or 0


if __name__ == "__main__":
    sys.exit(main())
