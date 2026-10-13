"""StateTree: device map loading, get/set, coercion, subscriptions, persistence."""

import json
import logging

import pytest

from state_tree import StateTree

GAIN = "/devices/0/inputs/0/preamps/0/Gain"


class TestLoading:
    def test_device_map_loaded(self, tree):
        assert tree.device_name == "Apollo x4"
        assert tree.control_count == 7944
        assert len(tree.all_paths()) > 7900

    @pytest.mark.parametrize("name", ["device_map_apollo_x4.json",
                                      "device_map_apollo_x8p.json"])
    def test_captured_maps_carry_no_plugin_license_state(self, name):
        # The captures came from a real UA account; its per-plugin
        # authorization and demo state must never be committed.
        import pathlib
        import re
        f = pathlib.Path(__file__).parent.parent / "device_maps" / name
        leaves = {c["path"].rstrip("/").rsplit("/", 1)[-1]
                  for c in json.load(open(f))["controls"]}
        assert not [n for n in leaves if re.search(r"auth|licen|demo", n, re.I)]

    def test_get_value_and_metadata(self, tree):
        assert tree.get_value(GAIN + "/value") == 10.0
        assert tree.get_value("/SampleRate/value") == 48000
        meta = tree.get(GAIN)
        assert meta["type"] == "float"
        assert meta["min"] == -144.0 and meta["max"] == 65.0

    def test_get_value_via_get(self, tree):
        assert tree.get(GAIN + "/value") == 10.0

    def test_enum_values_listing(self, tree):
        vals = tree.get("/ClockSource/values")
        assert vals == ["Internal", "S/PDIF"]

    def test_unknown_path(self, tree):
        assert tree.get("/no/such/path") is None
        assert tree.get_value("/no/such/path") is None
        assert not tree.path_exists("/no/such/path")
        assert tree.path_exists("/devices/0/inputs/0")

    def test_runtime_property(self):
        t = StateTree()
        t.add_runtime_property("/initialized", {"type": "bool", "value": False})
        assert t.get_value("/initialized/value") is False
        assert t.set("/initialized/value", True)
        assert t.get_value("/initialized/value") is True

    def test_update_property_only_overrides_non_none(self):
        t = StateTree()
        t.add_runtime_property("/x", {"type": "int", "min": 0, "max": 9, "value": 3})
        t.update_property("/x", {"max": 20, "value": None})
        assert t.get("/x")["max"] == 20 and t.get_value("/x/value") == 3


class TestSet:
    def test_set_changes_value(self, tree):
        assert tree.set(GAIN + "/value", 33.0)
        assert tree.get_value(GAIN + "/value") == 33.0

    def test_set_unknown_path_fails(self, tree):
        assert tree.set("/nope/value", 1) is False

    def test_set_on_container_fails(self, tree):
        assert tree.set("/devices/0/inputs/0", 1) is False

    def test_special_paths_accepted(self, tree):
        assert tree.set("/Sleep", False) is True
        assert tree.set("/Dirty/value", True) is True

    def test_float_clamped_to_range(self, tree):
        tree.set(GAIN + "/value", 999)
        assert tree.get_value(GAIN + "/value") == 65.0
        tree.set(GAIN + "/value", -999)
        assert tree.get_value(GAIN + "/value") == -144.0

    def test_float_coerced_from_string(self, tree):
        tree.set(GAIN + "/value", "21.5")
        assert tree.get_value(GAIN + "/value") == 21.5

    def test_bool_coercion(self, tree):
        p = "/devices/0/inputs/0/preamps/0/48V/value"
        tree.set(p, "true")
        assert tree.get_value(p) is True
        tree.set(p, "0")
        assert tree.get_value(p) is False
        tree.set(p, 1)
        assert tree.get_value(p) is True

    def test_int_coercion(self, tree):
        tree.set("/SampleRate/value", "96000")
        assert tree.get_value("/SampleRate/value") == 96000

    def test_string_value_in_allowed_values(self, tree, caplog):
        with caplog.at_level(logging.WARNING, logger="state_tree"):
            tree.set("/ClockSource/value", "S/PDIF")
        assert tree.get_value("/ClockSource/value") == "S/PDIF"
        assert "not in allowed" not in caplog.text

    def test_string_value_outside_allowed_values_warns(self, tree, caplog):
        # The tree warns about a value the device map does not list but
        # still stores it (the router decides whether hardware accepts it)
        with caplog.at_level(logging.WARNING, logger="state_tree"):
            tree.set("/ClockSource/value", "ADAT")
        assert "not in allowed values" in caplog.text
        assert tree.get_value("/ClockSource/value") == "ADAT"


class TestOnSetHook:
    def test_called_with_path_and_coerced_value(self, tree):
        seen = []
        tree.on_set = lambda p, v: seen.append((p, v))
        tree.set(GAIN + "/value", "40")
        assert seen == [(GAIN + "/value", 40.0)]

    def test_not_called_when_value_unchanged(self, tree):
        seen = []
        tree.on_set = lambda p, v: seen.append((p, v))
        tree.set(GAIN + "/value", 10.0)
        assert seen == []

    def test_not_called_for_failed_set(self, tree):
        seen = []
        tree.on_set = lambda p, v: seen.append((p, v))
        tree.set("/nope/value", 1)
        assert seen == []


class TestSubscriptions:
    def _client(self, tree, cid):
        got = []
        tree.register_callback(cid, lambda p, v: got.append((p, v)))
        return got

    def test_subscriber_notified(self, tree):
        got = self._client(tree, "c1")
        tree.subscribe("c1", GAIN + "/value")
        tree.set(GAIN + "/value", 20.0)
        assert got == [(GAIN + "/value", 20.0)]

    def test_subscribe_without_value_suffix(self, tree):
        got = self._client(tree, "c1")
        tree.subscribe("c1", GAIN)
        tree.set(GAIN + "/value", 20.0)
        assert got == [(GAIN + "/value", 20.0)]

    def test_originator_excluded(self, tree):
        a, b = self._client(tree, "a"), self._client(tree, "b")
        tree.subscribe("a", GAIN + "/value")
        tree.subscribe("b", GAIN + "/value")
        tree.set(GAIN + "/value", 20.0, source_client="a")
        assert a == [] and b == [(GAIN + "/value", 20.0)]

    def test_no_notification_when_unchanged(self, tree):
        got = self._client(tree, "c1")
        tree.subscribe("c1", GAIN + "/value")
        tree.set(GAIN + "/value", 10.0)
        assert got == []

    def test_unsubscribe(self, tree):
        got = self._client(tree, "c1")
        tree.subscribe("c1", GAIN + "/value")
        tree.unsubscribe("c1", GAIN + "/value")
        tree.set(GAIN + "/value", 20.0)
        assert got == []

    def test_unsubscribe_all_and_callback_removal(self, tree):
        got = self._client(tree, "c1")
        tree.subscribe("c1", GAIN + "/value")
        tree.subscribe("c1", "/SampleRate/value")
        tree.unsubscribe_all("c1")
        tree.set(GAIN + "/value", 20.0)
        tree.set("/SampleRate/value", 96000)
        assert got == []
        tree.unregister_callback("c1")

    def test_failing_callback_does_not_break_set(self, tree):
        def boom(p, v):
            raise RuntimeError("x")
        tree.register_callback("bad", boom)
        good = self._client(tree, "good")
        tree.subscribe("bad", GAIN + "/value")
        tree.subscribe("good", GAIN + "/value")
        assert tree.set(GAIN + "/value", 20.0)
        assert good == [(GAIN + "/value", 20.0)]

    def test_enumerate_leaf(self, tree):
        assert tree.enumerate_values("/SampleRate") == [("/SampleRate/value", 48000)]

    def test_enumerate_container_recursive(self, tree):
        vals = dict(tree.enumerate_values("/devices/0/inputs/0/preamps/0",
                                          recursive=True))
        assert vals[GAIN + "/value"] == 10.0
        assert all(p.startswith("/devices/0/inputs/0/preamps/0/") for p in vals)


class TestPersistence:
    def _fresh(self, path):
        from conftest import DEVICE_MAP_X4
        t = StateTree()
        t.load_device_map(DEVICE_MAP_X4)
        t.enable_persistence(path)
        return t

    def test_save_and_restore(self, tmp_path):
        save = tmp_path / "state.json"
        t = self._fresh(save)
        t.set(GAIN + "/value", 44.0)
        t.set("/SampleRate/value", 96000)
        t.save_now()
        assert json.loads(save.read_text()) == {
            GAIN + "/value": 44.0, "/SampleRate/value": 96000}

        t2 = self._fresh(save)
        assert t2.get_value(GAIN + "/value") == 44.0
        assert t2.get_value("/SampleRate/value") == 96000
        assert t2._dirty[GAIN + "/value"] == 44.0

    def test_only_modified_values_saved(self, tmp_path):
        save = tmp_path / "state.json"
        t = self._fresh(save)
        t.save_now()
        assert not save.exists()     # nothing modified, nothing written

    def test_saved_unknown_paths_ignored(self, tmp_path):
        save = tmp_path / "state.json"
        save.write_text(json.dumps({"/gone/value": 1, GAIN + "/value": 12.0}))
        t = self._fresh(save)
        assert t.get_value(GAIN + "/value") == 12.0
        assert "/gone/value" not in t._dirty

    def test_corrupt_state_file_ignored(self, tmp_path):
        save = tmp_path / "state.json"
        save.write_text("{not json")
        t = self._fresh(save)
        assert t.get_value(GAIN + "/value") == 10.0

    def test_save_creates_parent_dir_and_leaves_no_tmp(self, tmp_path):
        save = tmp_path / "sub" / "state.json"
        t = self._fresh(save)
        t.set(GAIN + "/value", 30.0)
        t.save_now()
        assert save.exists()
        assert not list(save.parent.glob("*.tmp"))

    def test_debounced_save_is_scheduled(self, tmp_path):
        t = self._fresh(tmp_path / "state.json")
        t.set(GAIN + "/value", 30.0)
        assert t._save_timer is not None
        t.save_now()
        assert t._save_timer is None

    def test_no_persistence_means_no_timer(self, tree):
        tree.set(GAIN + "/value", 30.0)
        assert tree._save_timer is None
