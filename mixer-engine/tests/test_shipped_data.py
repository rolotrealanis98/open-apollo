"""The device maps and helper tree open-apollo ships are structure only.

They were captured from UA's Mixer Engine and Mixer Helper; what is
Universal Audio's own content (the plug-in catalogue, factory preset menus,
help text) or the capture machine's data (serial numbers, hardware ids, the
open session, MIDI devices, network addresses, home paths, licence state)
was removed.  The user's own import puts UA's content back at run time
(ua_catalogue.py).  This guard keeps it that way for future captures."""

import json
import re
import uuid

import pytest

from conftest import ENGINE_DIR

MAPS = sorted((ENGINE_DIR / "device_maps").glob("device_map_*.json"))
HELPER = ENGINE_DIR / "device_maps" / "helper_tree.json"
PINS = ENGINE_DIR / "device_maps" / "helper_tree.pins.json"

IPV4 = re.compile(r"(?<![\d.])(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})(?![\d.])")
HOME = re.compile(r"/Users/(?!Shared/)[^/\s\"]+|/home/[^/\s\"]+|[A-Za-z]:\\+Users\\+", re.I)
EMAIL = re.compile(r"[\w.+-]+@[\w-]+(\.[\w-]+)*\.[A-Za-z]{2,}")
SERIAL = re.compile(r"(?<![\w.-])\d{12,16}(?![\w.-])")
LICENCE = re.compile(r"auth|licen[cs]|demo", re.I)
NS = uuid.UUID("6ba7b811-9dad-11d1-80b4-00c04fd430c8")


def strings(x, path=""):
    if isinstance(x, str):
        yield path, x
    elif isinstance(x, dict):
        for k, v in x.items():
            yield from strings(v, f"{path}/{k}")
    elif isinstance(x, list):
        for i, v in enumerate(x):
            yield from strings(v, f"{path}/{i}")


def host_data(doc):
    bad = []
    for where, s in strings(doc):
        for m in IPV4.finditer(s):
            if m.group(0) not in ("127.0.0.1", "0.0.0.0") and \
                    all(int(g) < 256 for g in m.groups()) and "version" not in where:
                bad.append((where, m.group(0)))
        bad += [(where, m.group(0)) for r in (HOME, EMAIL, SERIAL)
                for m in r.finditer(s)]
    return bad


def preset_menu(values):
    return isinstance(values, list) and any(
        isinstance(v, dict) and (v.get("locked") or v.get("type") == "folder")
        for v in values)


@pytest.mark.parametrize("path", MAPS, ids=lambda p: p.stem)
def test_device_map_is_structure_only(path):
    data = json.loads(path.read_text())
    controls = {c["path"]: c for c in data["controls"]}
    assert not [p for p in controls if re.match(r"^/plugins/\d+/", p)]
    assert not [p for p in controls if LICENCE.search(p.rsplit("/", 1)[-1])]
    assert not [p for p, c in controls.items() if preset_menu(c.get("values"))]
    assert not controls["/IOMapPreset"].get("values")
    for p in ("/Session", "/MidiInputDevice"):
        assert controls[p]["value"] == "" and not controls[p].get("values")
    assert controls["/SerialNumber"]["value"] == "N/A"
    assert {c["value"] for p, c in controls.items()
            if p.endswith("/DeviceHwID")} == {1}
    for p, c in controls.items():
        if p.endswith("/ScenePluginUuid") and c["value"]:
            assert c["value"] == str(uuid.uuid5(NS, "open-apollo:" + p)), p
        if re.search(r"/effects/\d+/(EffectName|Preset)$", p):
            assert not c["value"], p
    assert host_data(data) == []


def test_helper_tree_is_structure_only():
    doc = json.loads(HELPER.read_text())
    assert host_data(doc) == []
    status = next(c for c in doc["children"] if c["path"] == "status")
    assert status["properties"]["feature_keys"]["value"] is None

    def walk(n):
        for c in n.get("commands") or []:
            params = (c.get("properties") or {}).get("parameters")
            for where, s in strings(params):
                # 1.2.x help strings and 1.3.x schema descriptions are empty
                if not params or "name" not in params or where.endswith("description"):
                    assert s == "", (c.get("path"), where)
        for k, meta in (n.get("properties") or {}).items():
            if isinstance(meta, dict):
                assert not meta.get("url_parameters"), k
                if LICENCE.search(k):
                    assert meta.get("value") in (None, "", False, 0), k
        for c in n.get("children") or []:
            walk(c)
    walk(doc)


def test_helper_pins_point_at_empty_strings():
    pins = json.loads(PINS.read_text())
    assert pins["format"] == "open-apollo-helper-text-pins"
    assert pins["tree"] == HELPER.name and pins["hash"] == "sha256"
    doc = json.loads(HELPER.read_text())
    for pointer, sha in pins["pins"].items():
        node = doc
        for t in pointer.split("/")[1:]:
            t = t.replace("~1", "/").replace("~0", "~")
            node = node[int(t)] if isinstance(node, list) else node[t]
        assert node == "" and re.fullmatch(r"[0-9a-f]{64}", sha), pointer
