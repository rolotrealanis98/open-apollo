"""The UA catalogue overlay (ua_catalogue.py): what `oa-assets import`
brings in from the user's UAD install is laid over the shipped maps.

Every name here is synthetic: the tests carry no UA content."""

import hashlib
import json
import logging
import re
from contextlib import ExitStack
from unittest.mock import patch

import pytest

import ua_catalogue as uac
import ua_mixer_daemon as daemon_mod
from conftest import DEVICE_MAP_X4, ENGINE_DIR
from helper_tree import HelperTree
from state_tree import StateTree
from test_post_commands import Client

HELPER = ENGINE_DIR / "device_maps" / "helper_tree.json"
PINS = ENGINE_DIR / "device_maps" / "helper_tree.pins.json"

PRESETS = [
    {"value": "Folder A", "type": "folder", "locked": True,
     "children": [{"value": "Preset 1", "type": "file", "locked": True}]},
    {"value": "Preset 2", "type": "file", "locked": True},
]
PLUGINS = [
    {"name": "Test Comp", "short_names": ["TComp", "Test C"],
     "categories": ["Dynamics", "Mixing"], "vst_id": "tst1", "render_id": 1,
     "plugin_id": "test_comp", "unison": None, "presets": PRESETS},
    {"name": "Test Pre", "short_names": [], "categories": ["Preamp"],
     "vst_id": "tst2", "render_id": 2, "plugin_id": "test_pre",
     "unison": True, "presets": []},
]
CHANNEL = [{"value": "Strips", "type": "folder", "locked": True,
            "children": [{"value": "Strip 1", "type": "file", "locked": True}]}]


def write_catalogue(d, *, plugins=PLUGINS, channel=CHANNEL, io=("Map A", "Map B"),
                    texts=None, version=1):
    d.mkdir(parents=True, exist_ok=True)

    def put(name, part, body):
        doc = {"format": f"{uac.FORMAT}/{part}", "version": version,
               "source": {"uad": "0.0.0.test"}}
        doc.update(body)
        (d / name).write_text(json.dumps(doc))
    put("plugins.json", "plugins", {"plugins": list(plugins)})
    put("channel-presets.json", "channel-presets", {"presets": channel})
    put("io-presets.json", "io-presets", {"presets": list(io)})
    put("helper-text.json", "helper-text", {"texts": texts or {}})
    return d


def x4():
    t = StateTree()
    t.load_device_map(DEVICE_MAP_X4)
    return t


def channel_preset_paths(t):
    return [p for p in t.all_paths() if uac._RE_CHANNEL_PRESET.match(p)]


# ── locating and loading ────────────────────────────────────────────

def test_default_dir(monkeypatch, tmp_path):
    monkeypatch.setenv("OA_UA_CATALOGUE", str(tmp_path / "x"))
    assert uac.default_dir() == tmp_path / "x"
    monkeypatch.delenv("OA_UA_CATALOGUE")
    monkeypatch.setenv("XDG_DATA_HOME", str(tmp_path))
    assert uac.default_dir() == tmp_path / "open-apollo" / "ua-catalogue"
    monkeypatch.delenv("XDG_DATA_HOME")
    monkeypatch.setenv("HOME", str(tmp_path))
    assert uac.default_dir() == \
        tmp_path / ".local" / "share" / "open-apollo" / "ua-catalogue"


def test_absent_or_empty_import_is_none(tmp_path):
    assert uac.load(tmp_path / "missing") is None
    (tmp_path / "empty").mkdir()
    assert uac.load(tmp_path / "empty") is None


def test_load_reads_all_parts(tmp_path):
    cat = uac.load(write_catalogue(tmp_path / "c", texts={"ab": "text"}))
    assert [p["name"] for p in cat.plugins] == ["Test Comp", "Test Pre"]
    assert cat.channel_presets == CHANNEL
    assert cat.io_presets == ["Map A", "Map B"]
    assert cat.helper_texts == {"ab": "text"}
    assert cat.source["uad"] == "0.0.0.test"


def test_other_versions_and_broken_files_are_skipped(tmp_path, caplog):
    d = write_catalogue(tmp_path / "c", version=2)
    with caplog.at_level(logging.WARNING):
        assert uac.load(d) is None
    assert "version 2" in caplog.text
    d = write_catalogue(tmp_path / "d")
    (d / "plugins.json").write_text("{not json")
    (d / "io-presets.json").write_text(json.dumps({"format": "other"}))
    cat = uac.load(d)
    assert cat.plugins == [] and cat.io_presets == []
    assert cat.channel_presets == CHANNEL       # the good parts still load


# ── the state tree (4710) ───────────────────────────────────────────

def test_shipped_map_has_no_catalogue():
    t = x4()
    assert not [p for p in t.all_paths() if p.startswith("/plugins/")]
    assert all(t.get_prop_dict(p)["values"] == [] for p in channel_preset_paths(t))
    assert t.get_prop_dict("/IOMapPreset")["values"] == []


def test_plugins_get_the_engine_shape():
    t = x4()
    done = uac.apply_to_state(t, uac.Catalogue(directory=None, plugins=PLUGINS))
    assert done["plugins"] == 2
    base = "/plugins/0"
    assert t.get_value(base + "/Type/value") == "plugin"
    assert t.get_value(base + "/Name/value") == "Test Comp"
    assert t.get_prop_dict(base + "/Name")["values"] == [{"value": "TComp"},
                                                         {"value": "Test C"}]
    assert t.get_prop_dict(base + "/Preset")["values"] == PRESETS
    assert t.get_value(base + "/Categories/value") == "Dynamics,Mixing"
    assert t.get_value(base + "/VstID/value") == "tst1"
    assert t.get_value(base + "/Unison/value") is False      # unknown -> False
    assert t.get_value("/plugins/1/Unison/value") is True
    assert "values" not in t.get_prop_dict("/plugins/1/Name")
    # licence state is never invented
    assert t.get_value(base + "/Status/value") == ""
    assert not [p for p in t.all_paths()
                if p.startswith("/plugins/") and re.search(
                    r"auth|licen|demo", p.rsplit("/", 1)[-1], re.I)]


def test_channel_presets_reach_every_input_and_aux_menu():
    t = x4()
    done = uac.apply_to_state(t, uac.Catalogue(directory=None,
                                               channel_presets=CHANNEL))
    paths = channel_preset_paths(t)
    assert done["channel_presets"] == len(paths) > 20
    assert all(t.get_prop_dict(p)["values"] == CHANNEL for p in paths)
    # each channel has its own copy; insert slots' Preset is not a menu
    t.get_prop_dict(paths[0])["values"].clear()
    assert t.get_prop_dict(paths[1])["values"] == CHANNEL
    assert "values" not in t.get_prop_dict(
        "/devices/0/inputs/0/effects/0/Preset")


def test_io_presets_come_first_and_saved_ones_follow(tmp_path):
    from hardware import HardwareRouter
    from conftest import FakeBackend
    t = x4()
    uac.apply_to_state(t, uac.Catalogue(directory=None, io_presets=["Map A"]))
    (tmp_path / "x4.iomaps.json").write_text(json.dumps(
        {"format": "open-apollo-iomap-presets", "version": 1,
         "order": ["mine"], "presets": {"mine": {"values": {}}}}))
    d = daemon_mod.MixerDaemon(t, HardwareRouter(FakeBackend(), t))
    d.commands.set_store(tmp_path, "x4")
    assert [v["value"] for v in t.get_prop_dict("/IOMapPreset")["values"]] \
        == ["Map A", "mine"]


def test_unison_comes_from_the_shipped_list(tmp_path, caplog):
    ids = uac.load_unison()
    assert ids, "the shipped Unison list is missing or malformed"
    listed_id = sorted(ids)[0]
    t = x4()
    uac.apply_to_state(t, uac.Catalogue(directory=None, plugins=[
        {"name": "Test Uni", "plugin_id": listed_id, "unison": None},
        {"name": "Test Off", "plugin_id": listed_id, "unison": False},
        {"name": "Test Other", "plugin_id": "test_other", "unison": None}]))
    # the import's own flag wins; without one the shipped list decides
    assert [t.get_value(f"/plugins/{i}/Unison/value") for i in range(3)] \
        == [True, False, False]
    bad = tmp_path / "unison.json"
    bad.write_text(json.dumps({"format": "other", "plugin_ids": [listed_id]}))
    with caplog.at_level(logging.WARNING):
        assert uac.load_unison(bad) == frozenset()
        assert uac.load_unison(tmp_path / "missing.json") == frozenset()
    assert "Unison" in caplog.text or uac.UNISON_FORMAT in caplog.text


def test_a_map_with_its_own_lists_is_left_alone():
    t = StateTree()
    t.add_runtime_property("/plugins/0/Name", {"type": "string", "value": "Own"})
    t.add_runtime_property("/devices/0/inputs/0/Preset",
                           {"type": "string", "value": "", "values": [
                               {"value": "own", "type": "file", "locked": True}]})
    t.add_runtime_property("/IOMapPreset", {"type": "string", "value": "",
                                            "values": [{"value": "Map A"}]})
    done = uac.apply_to_state(t, uac.Catalogue(
        directory=None, plugins=PLUGINS, channel_presets=CHANNEL,
        io_presets=["Map A", "Map C"]))
    assert done == {"plugins": 0, "channel_presets": 0, "io_presets": 1}
    assert t.get_value("/plugins/0/Name/value") == "Own"
    assert [v["value"] for v in t.get_prop_dict("/IOMapPreset")["values"]] \
        == ["Map C", "Map A"]


# ── the helper tree (4720) ──────────────────────────────────────────

def test_help_text_returns_to_its_pinned_place(tmp_path):
    pins = uac.load_pins(PINS)
    assert pins, "the shipped pins file is missing or malformed"
    pointer = sorted(pins)[0]
    text = "synthetic help text"
    sha = hashlib.sha256(text.encode()).hexdigest()
    h = HelperTree()
    h.load(HELPER)
    container, key = uac._pointer_parent(h.root, pointer)
    assert container[key] == ""                      # shipped empty
    cat = uac.Catalogue(directory=None, helper_texts={sha: text})
    assert uac.apply_to_helper(h.root, cat, {pointer: sha, "/nope/0": sha,
                                             sorted(pins)[1]: "0" * 64}) == 1
    assert container[key] == text
    # a string that is already set is not overwritten
    assert uac.apply_to_helper(h.root, cat, {pointer: sha}) == 0


def test_apply_end_to_end(tmp_path, monkeypatch, caplog):
    pins = uac.load_pins(PINS)
    pointer = sorted(pins)[0]
    sha = hashlib.sha256(b"help").hexdigest()
    pins_file = tmp_path / "pins.json"
    pins_file.write_text(json.dumps({"format": uac.PINS_FORMAT, "version": 1,
                                     "pins": {pointer: sha}}))
    d = write_catalogue(tmp_path / "c", texts={sha: "help"})
    monkeypatch.setenv("OA_UA_CATALOGUE", str(d))
    t, h = x4(), HelperTree()
    h.load(HELPER)
    with caplog.at_level(logging.INFO):
        cat = uac.apply(t, h, None, pins_file)
    assert cat is not None and "2 plug-ins" in caplog.text
    assert t.get_value("/plugins/1/Name/value") == "Test Pre"
    container, key = uac._pointer_parent(h.root, pointer)
    assert container[key] == "help"


def test_apply_without_import_changes_nothing(caplog):
    t = x4()
    before = json.dumps(t.root, sort_keys=True)
    with caplog.at_level(logging.INFO):
        assert uac.apply(t, None) is None
    assert "No UA catalogue imported" in caplog.text
    assert json.dumps(t.root, sort_keys=True) == before


# ── the daemon's start-up ───────────────────────────────────────────

class Reached(Exception):
    pass


@pytest.mark.parametrize("extra,called", [([], True),
                                          (["--no-ua-catalogue"], False),
                                          (["--ua-catalogue", "/x/cat"], True)])
def test_main_overlays_the_import(extra, called, tmp_path):
    with ExitStack() as stack:
        stack.enter_context(patch("sys.argv", ["ua_mixer_daemon.py",
                                               "--no-hardware", *extra]))
        stack.enter_context(patch.object(daemon_mod, "SCRIPT_DIR", tmp_path))
        stack.enter_context(patch.object(daemon_mod, "find_device_map",
                                         return_value=DEVICE_MAP_X4))
        apply = stack.enter_context(patch.object(daemon_mod.ua_catalogue, "apply"))
        daemon = stack.enter_context(patch.object(daemon_mod, "MixerDaemon",
                                                  side_effect=Reached))
        with pytest.raises(Reached):
            daemon_mod.main()
    assert apply.called is called
    # Console's commands get the same import (factory preset names)
    assert daemon.call_args.kwargs["catalogue"] is \
        (apply.return_value if called else None)
    if called:
        state, helper, directory, pins = apply.call_args.args
        assert isinstance(state, StateTree) and isinstance(helper, HelperTree)
        assert directory == (None if not extra else daemon_mod.Path("/x/cat"))
        assert pins == daemon_mod.DEFAULT_HELPER_PINS


# ── Console's commands (console_commands, console_presets) ──────────

STRIP = "/devices/0/inputs/0"
SLOT = STRIP + "/effects/0"


def console_daemon(tmp_path, cat, factory=None):
    """A software-model daemon over the shipped x4 map, with the import
    laid on as main() does (or none)."""
    t = x4()
    if cat is not None:
        uac.apply_to_state(t, cat)
    return daemon_mod.MixerDaemon(t, None, software_only=True,
                                  sessions_dir=tmp_path / "sessions",
                                  presets_dir=tmp_path / "presets",
                                  factory_presets=factory, catalogue=cat)


def full_catalogue():
    return uac.Catalogue(directory=None, plugins=PLUGINS, channel_presets=CHANNEL)


def test_console_serves_the_imported_catalogue(tmp_path):
    d = console_daemon(tmp_path, full_catalogue())
    c = Client()
    d._dispatch(c, "get /plugins/0/Name/value")
    assert c.last()["data"] == "Test Comp"
    # Console's own per-entry defaults; licence state stays unset
    assert d.state.get_value("/plugins/0/DemoAvailable") is False
    assert d.state.get_value("/plugins/0/Status") == ""


def test_console_preset_menus_list_the_imported_factory_names(tmp_path):
    d = console_daemon(tmp_path, full_catalogue())
    t = d.state
    assert t.get_prop_dict(STRIP + "/Preset")["values"] == CHANNEL
    assert t.get_prop_dict("/devices/0/auxs/0/Preset")["values"] == CHANNEL
    assert t.get_prop_dict("/plugins/0/Preset")["values"] == PRESETS
    # an insert lists its plug-in's factory presets once it holds one
    d.console.set("c", SLOT + "/EffectName/value", "Test Comp")
    assert t.get_prop_dict(SLOT + "/Preset")["values"] == PRESETS
    # the user's own presets come first
    assert d.console.post("/SaveAsPreset", {"path": STRIP, "presetName": "Mine",
                                            "pluginName": "Channel Strip"})
    values = t.get_prop_dict(STRIP + "/Preset")["values"]
    assert values[0]["value"] == "Mine" and not values[0]["locked"]
    assert values[1:] == CHANNEL
    # a factory name has no file to load: said so, nothing changes
    with pytest.raises(ValueError, match="names? only"):
        d.console.set("c", STRIP + "/Preset/value", "Strips/Strip 1")
    with pytest.raises(ValueError, match="does not exist"):
        d.console.set("c", STRIP + "/Preset/value", "Strips/Nope")


def test_a_factory_folder_lists_its_own_files(tmp_path):
    (tmp_path / "factory" / "Channel Strip").mkdir(parents=True)
    (tmp_path / "factory" / "Channel Strip" / "Disk.uadchannel").write_text("")
    d = console_daemon(tmp_path, full_catalogue(), factory=tmp_path / "factory")
    assert [v["value"] for v in
            d.state.get_prop_dict(STRIP + "/Preset")["values"]] == ["Disk"]
    # a plug-in without a factory folder still lists the imported names
    assert d.state.get_prop_dict("/plugins/0/Preset")["values"] == PRESETS


def test_console_without_an_import(tmp_path):
    d = console_daemon(tmp_path, None)
    t = d.state
    assert not [p for p in t.all_paths() if p.startswith("/plugins/")]
    assert t.get_prop_dict(STRIP + "/Preset")["values"] == []
    d.console.set("c", SLOT + "/EffectName/value", "Any Plug-in")
    assert t.get_prop_dict(SLOT + "/Preset")["values"] == []
    with pytest.raises(ValueError, match="does not exist"):
        d.console.set("c", STRIP + "/Preset/value", "Strips/Strip 1")


UNI = STRIP + "/preamps/0/effects/0"


def test_console_unison_assignment_follows_the_shipped_list(tmp_path):
    listed_id = sorted(uac.load_unison())[0]
    d = console_daemon(tmp_path, uac.Catalogue(directory=None, plugins=[
        {"name": "Test Pre", "plugin_id": listed_id, "unison": None},
        {"name": "Test Comp", "plugin_id": "test_comp", "unison": None}]))
    assert d.state.get_value("/plugins/0/Unison") is True
    d.console.set("c", UNI + "/EffectName/value", "Test Pre")
    assert d.state.get_value(UNI + "/EffectName") == "Test Pre"
    # not Unison, or not in the import: refused, the slot is unchanged
    for name in ("Test Comp", "Not Imported"):
        with pytest.raises(ValueError, match="Unison"):
            d.console.set("c", UNI + "/EffectName/value", name)
    assert d.state.get_value(UNI + "/EffectName") == "Test Pre"


def test_console_unison_assignment_without_an_import(tmp_path):
    d = console_daemon(tmp_path, None)
    d.console.set("c", UNI + "/EffectName/value", "Any Preamp")
    assert d.state.get_value(UNI + "/EffectName") == "Any Preamp"
    other = "/devices/0/inputs/1/preamps/0/effects/0"
    d.console.post("/CopyInsert", {"effectPath": UNI})
    d.console.post("/PasteInsert", {"effectPaths": [other]})
    assert d.state.get_value(other + "/EffectName") == "Any Preamp"
