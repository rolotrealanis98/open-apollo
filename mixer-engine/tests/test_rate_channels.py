"""Per-rate digital/virtual channel counts in devices/*.json (N29), from the
Mixer Engine's MixerType*::ChannelCount: what each rate band carries."""

import json
from pathlib import Path

import pytest

DEVICES = Path(__file__).resolve().parents[2] / "devices"
HI = 4          # index of 176.4 kHz in "rates"


def rc(slug):
    return json.loads((DEVICES / f"{slug}.json").read_text())["rate_channels"]


@pytest.mark.parametrize("slug", ["apollo-8", "apollo-8p", "apollo-x6",
                                  "apollo-x8", "apollo-x8p",
                                  "apollo-x8p-gen-2"])
def test_smux4_models_keep_four_adat_channels_at_hi_rates(slug):
    for d in ("record", "playback"):
        assert rc(slug)[d]["adat"] == [8, 8, 8, 8, 4, 4]


@pytest.mark.parametrize("slug", ["apollo-x4", "apollo-twin-mkii",
                                  "apollo-twin-x-gen-2"])
def test_desktop_adat_input_halves_per_band(slug):
    assert rc(slug)["record"]["adat"] == [8, 8, 4, 4, 2, 2]


def test_x4_adat_output_side():
    # the output side keeps 8 at 88.2/96 kHz in the engine's channel count
    # (the route filter still hides ADAT 5-8 there) and 4 above
    assert rc("apollo-x4")["playback"]["adat"] == [8, 8, 8, 8, 4, 4]


@pytest.mark.parametrize("slug", ["apollo-x4", "apollo-8p", "apollo-x8p",
                                  "apollo-twin"])
def test_optical_spdif_input_stops_at_hi_rates(slug):
    assert rc(slug)["record"]["spdif"][HI:] == [0, 0]


@pytest.mark.parametrize("slug", ["apollo-8", "apollo-x6", "apollo-x8"])
def test_coax_spdif_input_runs_at_every_rate(slug):
    assert rc(slug)["record"]["spdif"] == [2] * 6


def test_virtual_channels_stop_at_hi_rates():
    for p in sorted(DEVICES.glob("*.json")):
        v = json.loads(p.read_text())["rate_channels"]["record"].get(
            "virtual")
        if v:
            assert v[HI:] == [0, 0], p.name


def test_madi_and_dante_blocks():
    assert rc("realtime-rack-madi")["record"]["madi"] == [16] * 6
    assert rc("apollo-x16d")["record"]["dante"] == [16] * 6
