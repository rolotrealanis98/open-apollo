"""Readback meter tables per model (metering.readback_meter_table).

The Mixer Engine reads every Apollo's meters but the MADI rack's with
CMxMixer::MeterTimerCallback_Base, so a model's layout follows from its
inputs' synch-in channels; the x4's captured table is the reference.
"""
import json
from pathlib import Path

import pytest

import metering
from metering import (X4_READBACK_METERS, ReadbackMeter, make_meter,
                      meter_channel, readback_meter_table)

DEVICES = Path(__file__).resolve().parents[2] / "devices"


def desc(slug):
    return json.loads((DEVICES / f"{slug}.json").read_text())


def as_set(table, remap=None):
    remap = remap or {}
    return {(w, sh, b, remap.get(c, c)) for w, sh, b, c in table}


def test_x4_table_matches_the_captured_one():
    # The captured table names analog 3/4 by type 0x00; the io_map (and the
    # route table) carry them as MIC/LINE 0x0103/0x0104.  meter_channel
    # matches either.
    got = readback_meter_table(desc("apollo-x4"))
    assert as_set(got) == as_set(X4_READBACK_METERS,
                                 {0x0003: 0x0103, 0x0004: 0x0104})


def test_x4_gen2_matches_the_x4():
    assert as_set(readback_meter_table(desc("apollo-x4-gen-2"))) == \
        as_set(readback_meter_table(desc("apollo-x4")))


def test_x8p_analog_5_to_8_in_words_10_11():
    t = {c: (w, sh) for w, sh, _b, c in readback_meter_table(desc("apollo-x8p"))}
    assert t[0x0305] == (10, 16) and t[0x0306] == (10, 0)
    assert t[0x0307] == (11, 16) and t[0x0308] == (11, 0)
    assert t[0x0401] == (12, 16) and t[0x0408] == (15, 0)


def test_x16_lines_and_aes():
    t = {c: (w, sh) for w, sh, _b, c in readback_meter_table(desc("apollo-x16"))}
    assert t[0x0301] == (8, 16) and t[0x0308] == (11, 0)
    assert t[0x0309] == (12, 16) and t[0x030C] == (13, 0)   # synch 12-15
    assert t[0x030D] == (14, 16) and t[0x0310] == (15, 0)   # synch 20-23
    assert t[0x0601] == (16, 16) and t[0x0602] == (16, 0)
    assert t[0x0B01] == (17, 16)


def test_the_madi_rack_has_no_table():
    assert readback_meter_table(desc("realtime-rack-madi")) is None


@pytest.mark.parametrize("path", sorted(DEVICES.glob("*.json")),
                         ids=lambda p: p.stem)
def test_every_table_names_recorded_channels_once(path):
    d = json.loads(path.read_text())
    t = readback_meter_table(d)
    if d.get("mixer_class") == "M64":
        assert t is None
        return
    assert t
    record = {int(e["ch_id"], 16) for e in d["routes"]["LoMid"]["record"]}
    ids = [c for *_x, c in t]
    assert len(ids) == len(set(ids))
    assert set(ids) <= record
    lanes = [(w, sh) for w, sh, *_x in t]
    assert len(lanes) == len(set(lanes))
    assert all(8 <= w <= 25 for w, _sh in lanes)


def test_make_meter_uses_the_model_table():
    m = make_meter(None, 0x22, None, model_desc=desc("apollo-x8p"))
    assert isinstance(m, ReadbackMeter)
    assert 0x0305 in m.channel_ids
    # The x4 keeps the captured table
    assert make_meter(None, 0x1F, None,
                      model_desc=desc("apollo-x4"))._table is X4_READBACK_METERS


def test_meter_channel_finds_aes_inputs():
    ids = [0x0601, 0x0602]
    assert meter_channel("inputs", 16, 0, "AES/EBU 1", True, ids) == 0
    assert meter_channel("inputs", 16, 1, "AES/EBU 1", True, ids) == 1
