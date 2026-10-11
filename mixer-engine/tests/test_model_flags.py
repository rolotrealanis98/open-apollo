"""Per-model data: /devices/0 capability flags, clock-source sets, JSON fixes.

Source: apollo-linux docs/re-notes/parity/16-mixertypes-models.md (sec. 3.4
capability table, finding 2 JSON fixes, finding 5 clock-source sets).
"""

import json

import pytest

from conftest import ENGINE_DIR
from test_generated_device_maps import DEVICES, ROOT, load_generator

CAPTURED = {
    "apollo-x4": ENGINE_DIR / "device_maps" / "device_map_apollo_x4.json",
    "apollo-x8p": ENGINE_DIR / "device_maps" / "device_map_apollo_x8p.json",
}
FLAG_PATHS = (
    "/devices/0/SupportsSpeakerCorrection",
    "/devices/0/SupportsBassManagement",
    "/devices/0/SupportsHeadphoneCorrection",
    "/devices/0/SupportsHeadphone2Correction",
    "/devices/0/SupportsAutogain",
    "/devices/0/SupportsDante",
    "/devices/0/MaxAltMons",
    "/devices/0/MaxSpans",
    "/devices/0/DefaultSpans",
    "/devices/0/DSPSpanning",
    "/devices/0/AvailableLLMDiskChannels",
    "/FirstMADIChannelBlockIndex",
)


@pytest.fixture(scope="module")
def gen():
    return load_generator()


def model(slug):
    return json.loads((ROOT / "devices" / f"{slug}.json").read_text())


def flags(gen, slug):
    m = gen.generate_for(slug)
    return {c["path"]: c["value"] for c in m["controls"]
            if c["path"] in FLAG_PATHS}, \
        {c["path"]: c for c in m["controls"]}


@pytest.mark.parametrize("slug", sorted(CAPTURED))
def test_flags_agree_with_the_captured_trees(gen, slug):
    cap = {c["path"]: c["value"]
           for c in json.loads(CAPTURED[slug].read_text())["controls"]}
    got, _ = flags(gen, slug)
    assert got.keys() == set(FLAG_PATHS)
    for path, value in got.items():
        assert value == cap[path], path


# (spk/bass, hp, hp2, autogain, dante, alt mons, spans max, spans default,
#  spanning, llm disk, first MADI block)
TABLE = {
    "apollo-firewire-thunderbolt-silver": (False, False, False, False, False, 2, 4, 4, True, 10, 0),
    "apollo-8": (False, False, False, False, False, 2, 4, 4, True, 16, 0),
    "apollo-8p": (False, False, False, False, False, 2, 4, 4, True, 16, 0),
    "apollo-16": (False, False, False, False, False, 2, 4, 4, True, 16, 0),
    "apollo-16-mkii": (False, False, False, False, False, 2, 4, 4, True, 16, 0),
    "apollo-twin": (False, False, False, False, False, 1, 2, 2, True, 10, 0),
    "apollo-twin-mkii": (False, False, False, False, False, 1, 2, 2, True, 10, 0),
    "apollo-twin-x": (True, True, False, False, False, 1, 2, 2, True, 10, 0),
    "apollo-twin-x-usb": (True, True, False, False, False, 1, 2, 2, True, 10, 0),
    "apollo-twin-x-gen-2": (True, True, False, True, False, 1, 2, 2, True, 10, 0),
    "arrow": (False, False, False, False, False, 0, 4, 4, False, 6, 0),
    "apollo-solo": (False, False, False, False, False, 0, 4, 4, False, 6, 0),
    "apollo-solo-usb": (False, False, False, False, False, 0, 4, 4, False, 6, 0),
    "apollo-x4": (True, True, False, False, False, 2, 4, 4, True, 16, 0),
    "apollo-x4-gen-2": (True, True, False, True, False, 2, 4, 4, True, 16, 0),
    "apollo-x6": (True, True, True, False, False, 2, 4, 4, True, 16, 0),
    "apollo-x8": (True, True, True, False, False, 2, 4, 4, True, 16, 0),
    "apollo-x8p": (True, True, True, False, False, 2, 4, 4, True, 16, 0),
    "apollo-x6-gen-2": (True, True, True, True, False, 2, 4, 4, True, 16, 0),
    "apollo-x8-gen-2": (True, True, True, True, False, 2, 4, 4, True, 16, 0),
    "apollo-x8p-gen-2": (True, True, True, True, False, 2, 4, 4, True, 16, 0),
    "apollo-x16": (True, False, False, False, False, 2, 4, 4, True, 16, 0),
    "apollo-x16-gen-2": (True, False, False, False, False, 2, 4, 4, True, 16, 0),
    "apollo-x16d": (True, False, False, False, True, 2, 4, 4, True, 16, 0),
    "realtime-rack-madi": (False, False, False, False, False, 0, 0, 0, True, 16, 1),
}
ORDER = ("SupportsSpeakerCorrection", "SupportsHeadphoneCorrection",
         "SupportsHeadphone2Correction", "SupportsAutogain", "SupportsDante",
         "MaxAltMons", "MaxSpans", "DefaultSpans", "DSPSpanning",
         "AvailableLLMDiskChannels")


def test_table_covers_every_model():
    assert {p.stem for p in DEVICES} == set(TABLE)


@pytest.mark.parametrize("slug", sorted(TABLE))
def test_capability_flags_per_model(gen, slug):
    got, _ = flags(gen, slug)
    row = TABLE[slug]
    for name, want in zip(ORDER, row):
        assert got[f"/devices/0/{name}"] == want, name
    # speaker correction and bass management share the output-DSP gate
    assert got["/devices/0/SupportsBassManagement"] == row[0]
    assert got["/FirstMADIChannelBlockIndex"] == row[10]


# ── devices/*.json fixes ─────────────────────────────────────────────

@pytest.mark.parametrize("slug", [
    "apollo-twin", "apollo-twin-mkii", "apollo-twin-x", "apollo-twin-x-gen-2",
    "apollo-twin-x-usb", "arrow", "apollo-solo", "apollo-solo-usb"])
def test_hiz_on_the_first_input_only(gen, slug):
    assert model(slug)["hiz"] == 1
    _, ctrls = flags(gen, slug)
    hiz_inputs = {p.split("/")[4] for p in ctrls
                  if p.endswith("/preamps/0/HiZ")}
    assert hiz_inputs == {"0"}


def test_x4_family_keeps_two_hiz_inputs():
    for slug in ("apollo-x4", "apollo-x8p", "apollo-8", "apollo-x6"):
        assert model(slug)["hiz"] == 2


@pytest.mark.parametrize("slug", ["apollo-twin-x", "apollo-twin-x-gen-2",
                                  "apollo-twin-x-usb"])
def test_twin_x_talkback_synch_in(slug):
    assert model(slug)["talkback_synch_in"] == 10


def test_apollo_silver_cue_max():
    assert model("apollo-firewire-thunderbolt-silver")["cue_buses"]["max"] == 4


def test_rack_is_madi():
    assert model("realtime-rack-madi")["digital_io"] == "madi"
