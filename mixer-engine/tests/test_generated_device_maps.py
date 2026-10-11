"""Generated device maps (tools/gen-device-map.py) and the daemon's map selection."""

import importlib.util
import json
import re
from pathlib import Path

import pytest

import ua_mixer_daemon
from conftest import DEVICE_MAP_X4, ENGINE_DIR
from state_tree import StateTree

ROOT = ENGINE_DIR.parent
DEVICES = sorted((ROOT / "devices").glob("*.json"))

# Plugin and licensing data must never appear in a generated map.
FORBIDDEN = re.compile(r"/plugins|/pluginscenes|/effects|auth|licen", re.I)


def load_generator():
    spec = importlib.util.spec_from_file_location(
        "gen_device_map", ROOT / "tools" / "gen-device-map.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


@pytest.fixture(scope="module")
def gen():
    return load_generator()


@pytest.mark.parametrize("path", DEVICES, ids=lambda p: p.stem)
def test_every_model_generates_a_loadable_clean_map(gen, tmp_path, path):
    slug = path.stem
    out = tmp_path / "map.json"
    out.write_text(gen.dump(gen.generate_for(slug)))
    data = json.loads(out.read_text())
    assert data["generated"] is True
    assert data["controls"], "empty map"

    bad = [c["path"] for c in data["controls"] if FORBIDDEN.search(c["path"])]
    assert not bad, bad[:5]

    tree = StateTree()
    tree.load_device_map(out)
    assert tree.control_count == len(data["controls"])

    model = json.loads(path.read_text())
    vals = {c["path"]: c["value"] for c in data["controls"]}
    assert vals["/devices/0/DeviceName"] == model["model"]
    n_pre = len({m.group(1) for p in vals
                 if (m := re.match(r"^/devices/0/inputs/(\d+)/preamps/0/Gain$", p))})
    n_hiz = len({m.group(1) for p in vals
                 if (m := re.match(r"^/devices/0/inputs/(\d+)/preamps/0/HiZ$", p))})
    assert n_pre == model["preamps"]
    assert n_hiz == model["hiz"]


def test_x4_is_reproduced_from_its_own_json(gen):
    """Generating the x4 gives exactly the control paths of the capture."""
    cap = {c["path"] for c in json.loads(DEVICE_MAP_X4.read_text())["controls"]
           if not FORBIDDEN.search(c["path"])}
    new = {c["path"] for c in gen.generate_for("apollo-x4")["controls"]}
    assert new == cap


def test_cache_key_follows_the_inputs(gen, tmp_path):
    root = tmp_path / "root"
    (root / "devices").mkdir(parents=True)
    (root / "mixer-engine" / "device_maps").mkdir(parents=True)
    (root / "devices" / "m.json").write_text('{"model": "M"}')
    (root / "mixer-engine" / "device_maps" / gen.TEMPLATE_NAME).write_text("{}")
    k1 = gen.cache_key("m", str(root))
    assert gen.cache_key("m", str(root)) == k1
    (root / "devices" / "m.json").write_text('{"model": "N"}')
    assert gen.cache_key("m", str(root)) != k1


def test_captured_maps_win(monkeypatch, tmp_path, caplog):
    monkeypatch.setenv("XDG_CACHE_HOME", str(tmp_path))
    assert ua_mixer_daemon.find_device_map(0x1F).name == "device_map_apollo_x4.json"
    assert ua_mixer_daemon.find_device_map(0x22).name == "device_map_apollo_x8p.json"
    assert not list(tmp_path.rglob("*.json")), "captured models must not fill the cache"


def test_other_models_are_generated_into_the_cache(monkeypatch, tmp_path, caplog):
    monkeypatch.setenv("XDG_CACHE_HOME", str(tmp_path))
    with caplog.at_level("WARNING"):
        first = ua_mixer_daemon.find_device_map(0x20)     # Apollo x6
    assert first.parent == tmp_path / "open-apollo" / "device_maps"
    assert first.name.startswith("device_map_apollo_x6.")
    assert "generated device map" in caplog.text
    data = json.loads(first.read_text())
    assert data["generated"] is True and data["device_name"] == "Apollo x6"

    mtime = first.stat().st_mtime_ns
    again = ua_mixer_daemon.find_device_map(0x20)
    assert again == first and again.stat().st_mtime_ns == mtime   # cache hit


def test_unknown_type_falls_back_to_x4(monkeypatch, tmp_path, caplog):
    monkeypatch.setenv("XDG_CACHE_HOME", str(tmp_path))
    with caplog.at_level("WARNING"):
        assert ua_mixer_daemon.find_device_map(0x77) == DEVICE_MAP_X4
    assert "falling back to Apollo x4" in caplog.text


def test_unwritable_cache_falls_back_to_x4(monkeypatch, tmp_path):
    blocker = tmp_path / "file"
    blocker.write_text("")
    monkeypatch.setenv("XDG_CACHE_HOME", str(blocker))     # cannot mkdir below a file
    assert ua_mixer_daemon.find_device_map(0x20) == DEVICE_MAP_X4
