"""Engine feature flags in devices/*.json (N30, N18), as the MixerType*
classes answer them."""

import json
from pathlib import Path

import pytest

DEVICES = Path(__file__).resolve().parents[2] / "devices"


def dev(slug):
    return json.loads((DEVICES / f"{slug}.json").read_text())


@pytest.mark.parametrize("slug, inputs", [
    ("apollo-8", [4, 5, 6, 7]), ("apollo-firewire-thunderbolt-silver",
                                 [4, 5, 6, 7]),
    ("apollo-x8", [4, 5, 6, 7]), ("apollo-x6", [2, 3, 4, 5]),
    ("apollo-x8p", []), ("apollo-x4", []), ("apollo-x16d", [])])
def test_line_reference_level_inputs(slug, inputs):
    assert dev(slug)["line_ref_level_inputs"] == inputs


def test_a16_and_x16_line_inputs_all_have_it():
    for slug in ("apollo-16", "apollo-16-mkii", "apollo-x16"):
        d = dev(slug)
        analog = sorted(e["synch_in"] for e in d["mixer_inputs"]
                        if e["name"].startswith("ANALOG"))
        assert d["line_ref_level_inputs"] == analog


@pytest.mark.parametrize("slug, on", [
    ("apollo-8", True), ("apollo-8p", True), ("apollo-x8p", True),
    ("apollo-x4-gen-2", True), ("apollo-twin-x-gen-2", True),
    ("apollo-x4", False), ("apollo-twin-x", False), ("apollo-x16", False),
    ("apollo-x16d", False), ("apollo-firewire-thunderbolt-silver", False)])
def test_line_pga_disable(slug, on):
    assert dev(slug)["line_pga_disable"] is on


@pytest.mark.parametrize("slug, outs", [
    ("apollo-x4", []), ("apollo-x4-gen-2", ["headphone", "cue", "monitor"]),
    ("apollo-x8p-gen-2", ["headphone", "cue", "monitor",
                          "monitor_r_surround"]),
    ("apollo-twin-x-gen-2", ["all"]), ("apollo-x16", ["all"]),
    ("apollo-x16d", ["all"]), ("apollo-x8p", [])])
def test_es9028_outputs(slug, outs):
    assert dev(slug)["es9028_outputs"] == outs


def test_function_switch():
    assert dev("apollo-8p")["func_switch"]["modes"] == [
        "none", "alt 2", "mono", "dim"]
    assert dev("apollo-x8p")["func_switch"]["modes"][-1] == "talkback"
    assert dev("apollo-x16d")["func_switch"]["second"] is True
    assert dev("apollo-x4")["func_switch"] is None


@pytest.mark.parametrize("slug, n", [
    ("apollo-x8p", 16), ("apollo-16", 8), ("apollo-8p", 8),
    ("apollo-x4", 0), ("apollo-twin-x", 0), ("arrow", 0)])
def test_direct_outputs(slug, n):
    assert dev(slug)["direct_outputs"] == n


def test_front_panel_switches_are_the_desktop_knob_models():
    for p in sorted(DEVICES.glob("*.json")):
        d = json.loads(p.read_text())
        if d["front_panel_switches"]:
            assert d["desktop"], p.name


def test_surround_modes():
    assert dev("apollo-x6")["surround_max_mode"] == 5
    assert dev("apollo-x8p")["surround_max_mode"] == 7
    assert dev("apollo-x16")["surround_max_mode"] == 10
    assert dev("apollo-x4")["surround_max_mode"] is None
