"""Digital mirror and output pad per model in the generated maps
(parity/16 F13, F22): tools/gen-device-map.py + devices/*.json."""

import json
import re

import pytest

from conftest import DEVICE_MAP_X4, ENGINE_DIR
from test_generated_device_maps import DEVICES, load_generator

DEVICE_MAP_X8P = ENGINE_DIR / "device_maps" / "device_map_apollo_x8p.json"


@pytest.fixture(scope="module")
def gen():
    return load_generator()


def paths_of(m):
    return {c["path"] for c in m["controls"]}


def monitor_index(m):
    return next(int(re.match(r"^/devices/0/outputs/(\d+)/", c["path"]).group(1))
                for c in m["controls"]
                if re.match(r"^/devices/0/outputs/\d+/IOType$", c["path"])
                and c["value"] == "Monitor")


def out_paths(m, suffix):
    return sorted(p for p in paths_of(m)
                  if "/outputs/" in p and p.endswith(suffix))


def line_pads(m):
    return sorted(int(p.split("/")[4]) for p in out_paths(m, "/Pad"))


def test_captured_x4_keeps_mirror_and_pads_exactly(gen):
    cap = json.loads(DEVICE_MAP_X4.read_text())
    new = gen.generate_for("apollo-x4")
    for suffix in ("/Pad", "/MirrorsToDigital"):
        assert out_paths(new, suffix) == out_paths(cap, suffix)


def test_x8p_gating_matches_its_capture(gen):
    """The x8p capture is not reproduced path for path (it never was), but
    nothing the generator emits is missing from it, and the controls this
    gating decides are the captured ones."""
    cap = json.loads(DEVICE_MAP_X8P.read_text())
    new = gen.generate_for("apollo-x8p")
    assert paths_of(new) <= paths_of(cap)
    for suffix in ("/Pad", "/MirrorsToDigital"):
        assert out_paths(new, suffix) == out_paths(cap, suffix)


@pytest.mark.parametrize("slug", [
    "apollo-twin", "apollo-twin-mkii", "apollo-twin-x", "apollo-twin-x-gen-2",
    "apollo-twin-x-usb", "apollo-solo", "apollo-solo-usb", "arrow"])
def test_twin_and_conrad_families_have_no_mirror_and_no_line_pad(gen, slug):
    m = gen.generate_for(slug)
    assert out_paths(m, "/MirrorsToDigital") == []
    assert line_pads(m) == [monitor_index(m)]    # the desktop monitor's pad


@pytest.mark.parametrize("slug", [
    "apollo-x4", "apollo-x4-gen-2", "apollo-x6", "apollo-x8p", "apollo-x16",
    "apollo-8", "apollo-8p", "apollo-16", "apollo-firewire-thunderbolt-silver"])
def test_models_with_a_mirror_have_it_on_the_monitor_only(gen, slug):
    m = gen.generate_for(slug)
    assert out_paths(m, "/MirrorsToDigital") == [
        f"/devices/0/outputs/{monitor_index(m)}/MirrorsToDigital"]


def test_pad_follows_the_model_data(gen):
    # Apollo 8p: line outputs only; Apollo 8: line outputs and the monitor
    p8 = gen.generate_for("apollo-8p")
    assert line_pads(p8) and monitor_index(p8) not in line_pads(p8)
    a8 = gen.generate_for("apollo-8")
    assert monitor_index(a8) in line_pads(a8) and len(line_pads(a8)) == 9


def test_rack_has_neither(gen):
    m = gen.generate_for("realtime-rack-madi")
    assert out_paths(m, "/Pad") == [] and out_paths(m, "/MirrorsToDigital") == []


def test_every_model_json_declares_both_fields():
    for path in DEVICES:
        d = json.loads(path.read_text())
        assert isinstance(d["mirror_to_digital"], bool), path.stem
        assert set(d["output_pad"]) == {"line", "monitor"}, path.stem


def test_missing_fields_keep_the_x4_behaviour(gen, tmp_path):
    """A model JSON without the keys still gets both (the old behaviour)."""
    d = json.loads((DEVICES[0].parent / "apollo-8.json").read_text())
    d["mirror_to_digital"] = False
    d["output_pad"] = {"line": False, "monitor": False}
    root = tmp_path / "root"
    (root / "devices").mkdir(parents=True)
    (root / "mixer-engine").mkdir()
    (root / "mixer-engine" / "device_maps").symlink_to(
        ENGINE_DIR / "device_maps")
    (root / "devices" / "m.json").write_text(json.dumps(d))
    gated = gen.generate_for("m", root=str(root))
    assert out_paths(gated, "/MirrorsToDigital") == []
    assert out_paths(gated, "/Pad") == []
    del d["mirror_to_digital"], d["output_pad"]
    (root / "devices" / "m.json").write_text(json.dumps(d))
    plain = gen.generate_for("m", root=str(root))
    assert out_paths(plain, "/MirrorsToDigital")
    assert out_paths(plain, "/Pad")


def test_generator_version_was_bumped(gen):
    assert gen.GENERATOR_VERSION >= 3       # cached maps predate the gating
