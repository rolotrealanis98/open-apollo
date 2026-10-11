#!/usr/bin/env python3
"""Synthesize a /proc/asound/cardN/io_map text for a model from devices/*.json.

Used for the setup-apollo-io.sh --dry-run fixtures in configs/pipewire/tests/
(models nobody has captured an io_map from).  The text follows the driver's
ua_io_map_proc_read() format and its ua_chan_name() naming.  Channel set:
the JSON route list for the rate band, minus the S/PDIF channels in ADAT mode
and minus the ADAT channels in S/PDIF mode (this reproduces the JSON
channel_counts of every model with routes).  It is NOT a capture from
hardware: the real order and slots come from the firmware IO descriptor.

Usage:
  tools/gen-io-map-fixture.py devices/apollo-x8p.json --mode adat \
      > configs/pipewire/tests/x8p-io_map-adat-48k.txt
"""
import argparse
import json
import sys

TYPE_NAMES = ["MIC/LINE/HIZ", "MIC/LINE", "AUX", "LINE", "ADAT", "S/PDIF",
              "AES/EBU", "VIRTUAL", "MADI", "MON", "CUE", "TALKBACK",
              "RESERVED", "DANTE", "UNKNOWN"]
STEREO = 0x0664
NUMBERED = 0x7D9F


def chan_name(ch_id):
    """ua_chan_name() from driver/ua_audio.c"""
    t, num = ch_id >> 8, ch_id & 0xFF
    lr = "L" if num & 1 else "R"
    if ch_id > 0x0DFF:
        t = 14
    stereo = bool(STEREO & (1 << t))
    numbered = bool(NUMBERED & (1 << t))
    if stereo:
        num = (num + 1) // 2
    if numbered and stereo:
        return "%s%u %s" % (TYPE_NAMES[t], num, lr)
    if numbered:
        return "%s %u" % (TYPE_NAMES[t], num)
    if stereo:
        return "%s %s" % (TYPE_NAMES[t], lr)
    return TYPE_NAMES[t]


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("json")
    ap.add_argument("--mode", choices=("spdif", "adat"), default=None,
                    help="digital mode (default: legacy_digital_mode)")
    ap.add_argument("--band", choices=("LoMid", "Hi"), default="LoMid")
    ap.add_argument("--rate", type=int, default=48000)
    a = ap.parse_args()

    d = json.load(open(a.json))
    mode = a.mode or d.get("legacy_digital_mode", "spdif")
    routes = d["routes"][a.band]
    drop = 5 if mode == "adat" else 4   # S/PDIF in ADAT mode, ADAT in S/PDIF mode
    if d.get("digital_io") in ("aes", "madi"):
        drop = None

    print("# synthesized from %s (tools/gen-io-map-fixture.py), not a hardware capture" % a.json)
    print("io_map\t\t1 (descriptor layout, auto)")
    dtype = d.get("device_type") or (d.get("device_types") or ["0x00"])[0]
    print("model\t\t%s (device type %s)" % (d["model"], dtype.lower()))
    print("rate\t\t%u Hz" % a.rate)
    for direction in ("record", "playback"):
        ids = [int(r["ch_id"], 16) for r in routes[direction]]
        ids = [i for i in ids if drop is None or (i >> 8) != drop]
        want = d["channel_counts"][a.band][mode][direction]
        if len(ids) != want:
            sys.exit("%s %s: %d channels after filtering, JSON says %d"
                     % (a.json, direction, len(ids), want))
        print("\n%s: %d ALSA channels, DMA frame %d slots, %s mode"
              % (direction, len(ids), len(ids) + 1, "ADAT" if mode == "adat" else "S/PDIF"))
        for i, cid in enumerate(ids):
            print("  ch %2u  slot %2u  0x%04x  %s" % (i, i, cid, chan_name(cid)))
        print("%s descriptor: %d slots, word5 0x0" % (direction, len(ids)))


if __name__ == "__main__":
    main()
