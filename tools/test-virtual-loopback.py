#!/usr/bin/env python3
"""Synthetic test of virtual-loopback.py's analysis and io_map lookup (no device):
python3 tools/test-virtual-loopback.py"""
import importlib.util
from pathlib import Path

spec = importlib.util.spec_from_file_location("vl", Path(__file__).with_name("virtual-loopback.py"))
vl = importlib.util.module_from_spec(spec)
spec.loader.exec_module(vl)

IO_MAP = """record: 3 ALSA channels, DMA frame 3 slots
  ch  0  slot  0  0x0001  MIC/LINE/HIZ 1
  ch  1  slot 18  0x0701  VIRTUAL 1
  ch  2  slot 19  0x0702  VIRTUAL 2
record descriptor: 3 slots, word5 0x2
  slot  0  0x0001  MIC/LINE/HIZ 1
playback: 2 ALSA channels, DMA frame 2 slots
  ch  0  slot 20  0x0701  VIRTUAL 1
  ch  1  slot 21  0x0702  VIRTUAL 2
"""


def main():
    io, n_play, n_rec = vl.lanes_from_io_map(IO_MAP, [1, 2])
    assert io == {1: (0, 1), 2: (1, 2)} and (n_play, n_rec) == (2, 3), io
    x = vl.stimulus()
    take = 200                                  # the plain strip's record delay
    ref = [0.0] * take + x
    same = list(ref)
    late = [0.0] * 55 + [v * 0.5 for v in ref]  # 55 samples later, 6 dB down
    r = vl.analyze({1: late, 2: ref, 3: same, 4: [0.0] * len(ref)}, [1, 2, 3, 4], 2)
    assert r[2]["onset"] == take + int(vl.RATE * vl.IMPULSE_AT) and r[2]["present"], r
    # a sine's RMS is 3 dB under its peak
    assert r[1]["delay"] == 55 and abs(r[1]["level_db"] - (vl.SINE_DB - 3.01 - 6.02)) < 0.1, r[1]
    assert -6.1 < r[1]["difference_db"] < -5.9, r[1]       # half the reference: -6 dB
    assert r[3]["delay"] == 0 and r[3]["difference_db"] == -float("inf"), r[3]   # identical
    assert not r[4]["present"] and "delay" not in r[4], r[4]
    print("ok")


if __name__ == "__main__":
    main()
