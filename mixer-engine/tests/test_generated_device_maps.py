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


def test_public_type_picks_the_model_when_device_type_has_none():
    find = ua_mixer_daemon.find_descriptor
    # the driver's retyped pre-x models list 0x80 + public type directly
    assert find(0x88)[0] == "apollo-twin"
    assert find(0x99)[0] == "apollo-twin-x"
    # an older driver leaves them at 0: the public type names them
    assert find(0, 0x08)[0] == "apollo-twin"
    assert find(0, 0x0C)[0] == "apollo-8"
    assert find(0, 0x12)[0] == "apollo-twin-mkii"
    # serial types without a descriptor: 2016 is the Arrow, 2032 the Solo
    assert find(0x1E, 0x13)[0] == "arrow"
    assert find(0x2A, 0x1C)[0] == "apollo-solo"
    # device_type wins over the public type
    assert find(0x1F, 0x08)[0] == "apollo-x4"
    # no public type, or one outside 1-0x29: no match
    assert find(0x77) == (None, None)
    assert find(0x77, 0xFFFFFFFF) == (None, None)
    assert find(0x77, 0) == (None, None)


def test_public_type_reaches_the_map(monkeypatch, tmp_path):
    monkeypatch.setenv("XDG_CACHE_HOME", str(tmp_path))
    m = ua_mixer_daemon.find_device_map(0, 0x1A)
    assert m.name == "device_map_apollo_x4.json"
    assert ua_mixer_daemon.device_model_name(0, 0x19) == "Apollo Twin X"


def test_read_device_info_carries_the_public_type():
    class Be:
        def get_device_info(self):
            return {"device_type": 0, "serial": 0}

        def get_system_info(self):
            return {"public_type": 0x08, "serial": "2017123456789012"}

    info = ua_mixer_daemon.read_device_info(Be())
    assert info["public_type"] == 0x08


def test_unwritable_cache_falls_back_to_x4(monkeypatch, tmp_path):
    blocker = tmp_path / "file"
    blocker.write_text("")
    monkeypatch.setenv("XDG_CACHE_HOME", str(blocker))     # cannot mkdir below a file
    assert ua_mixer_daemon.find_device_map(0x20) == DEVICE_MAP_X4
