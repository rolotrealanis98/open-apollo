"""Undo / Redo (U33) and the I/O map preset commands (U48)."""

import json

import pytest

import commands as cmdmod
import ua_catalogue
from commands import ERR_FAILED
from test_post_commands import Client, FakeLoop, ok, post, stack  # noqa: F401

IN = "/devices/0/inputs"
FADER = f"{IN}/0/FaderLevel/value"
MUTE = f"{IN}/0/Mute/value"
GAIN = f"{IN}/0/preamps/0/Gain/value"


def client_set(d, path, value, client="c1", wait=1.0):
    """What the daemon does for one client SET, `wait` seconds after the
    last activity (a new batch unless `wait` is below UNDO_GAP)."""
    loop = d.commands.scheduler
    loop.now += wait
    old = d.commands.value_of(path)
    d.state.set(path, value, source_client=client)
    d.commands.on_set(path, value, old=old, client=client)


def undo(d):
    return d.commands.execute("/Undo")


def redo(d):
    return d.commands.execute("/Redo")


class TestUndo:
    def test_empty_stacks_fail(self, stack):
        assert undo(stack) == (None, ERR_FAILED)
        assert redo(stack) == (None, ERR_FAILED)

    def test_undo_restores_the_old_value_and_redo_the_new(self, stack):
        before = stack.state.get_value(FADER)
        client_set(stack, FADER, -12.0)
        data, err = undo(stack)
        assert err is None and data["applied"] == 1
        assert stack.state.get_value(FADER) == before
        data, err = redo(stack)
        assert err is None
        assert stack.state.get_value(FADER) == -12.0

    def test_undo_reaches_the_hardware_through_the_set_path(self, stack):
        client_set(stack, FADER, -12.0)
        stack.be.clear()
        stack.state.on_set = stack.hw_router.on_set
        undo(stack)
        assert stack.be.bus_params       # the fader went back to the bus

    def test_steps_are_last_in_first_out(self, stack):
        client_set(stack, FADER, -3.0)
        client_set(stack, MUTE, True)
        undo(stack)
        assert stack.state.get_value(MUTE) is False
        assert stack.state.get_value(FADER) == -3.0
        undo(stack)
        assert stack.state.get_value(FADER) != -3.0

    def test_a_fast_run_from_one_client_is_one_step(self, stack):
        first = stack.state.get_value(FADER)
        client_set(stack, FADER, -1.0)
        for v in (-2.0, -3.0, -4.0):
            client_set(stack, FADER, v, wait=0.1)
        assert len(stack.commands._undo_stack) == 1
        undo(stack)
        assert stack.state.get_value(FADER) == first
        assert undo(stack)[1] == ERR_FAILED

    def test_another_client_starts_a_new_step(self, stack):
        client_set(stack, FADER, -1.0, client="a")
        client_set(stack, FADER, -2.0, client="b", wait=0.1)
        assert len(stack.commands._undo_stack) == 2

    def test_batch_undoes_every_path_of_it(self, stack):
        m0 = stack.state.get_value(MUTE)
        f0 = stack.state.get_value(FADER)
        client_set(stack, FADER, -5.0)
        client_set(stack, MUTE, True, wait=0.1)
        undo(stack)
        assert stack.state.get_value(MUTE) == m0
        assert stack.state.get_value(FADER) == f0

    def test_new_set_clears_redo(self, stack):
        client_set(stack, FADER, -5.0)
        undo(stack)
        client_set(stack, FADER, -7.0)
        assert redo(stack)[1] == ERR_FAILED

    def test_stack_is_bounded(self, stack):
        for i in range(cmdmod.UNDO_MAX + 10):
            client_set(stack, FADER, -float(i + 1))
        assert len(stack.commands._undo_stack) == cmdmod.UNDO_MAX
        n = 0
        while undo(stack)[1] is None:
            n += 1
        assert n == cmdmod.UNDO_MAX

    def test_unchanged_set_is_not_recorded(self, stack):
        v = stack.state.get_value(FADER)
        client_set(stack, FADER, v)
        assert undo(stack)[1] == ERR_FAILED

    @pytest.mark.parametrize("path,value", [
        ("/SampleRate/value", 96000),
        ("/ClockSource/value", "S/PDIF"),
        ("/AudioStreaming/value", False),
        ("/devices/0/DeviceOnline/value", False),
        (GAIN, 30.0),
        (f"{IN}/0/preamps/0/48V/value", True),
    ])
    def test_volatile_and_preamp_paths_are_skipped(self, stack, path, value):
        if not stack.state.path_exists(path):
            pytest.skip("not in the x4 map")
        client_set(stack, path, value)
        assert undo(stack)[1] == ERR_FAILED

    def test_monitor_level_is_skipped(self, stack):
        p = "/devices/0/outputs/18/CRMonitorLevel/value"
        if not stack.state.path_exists(p):
            pytest.skip("no monitor level")
        client_set(stack, p, -20.0)
        assert undo(stack)[1] == ERR_FAILED

    def test_undo_itself_is_not_recorded(self, stack):
        client_set(stack, FADER, -5.0)
        undo(stack)
        assert len(stack.commands._undo_stack) == 0
        assert len(stack.commands._redo_stack) == 1

    def test_over_the_daemon_set_handler(self, stack):
        c = Client()
        before = stack.state.get_value(FADER)
        stack.commands.scheduler.now += 1
        stack._dispatch(c, f"set {FADER} -9")
        assert stack.state.get_value(FADER) == -9.0
        c2 = post(stack, "post /Undo")
        assert c2.last().get("data", {}).get("applied") == 1
        assert stack.state.get_value(FADER) == before

    def test_reply_text_when_empty(self, stack):
        c = post(stack, "post /Undo")
        assert c.last()["error"] == "Unable to execute command."

    def test_new_session_clears_history(self, stack):
        client_set(stack, FADER, -5.0)
        ok(stack, "/NewSession")
        assert undo(stack)[1] == ERR_FAILED


# ── I/O map presets ─────────────────────────────────────────────────

ROUTE = "/inputroutes/0/Active/value"
OUTROUTE = "/outputroutes/0/Active/value"
# A factory I/O preset, as an import of the user's UAD install provides
FACTORY = "Factory Map A"


def with_factory(d):
    """The stack with an imported factory I/O preset list (the shipped map
    has none: UA's list comes from the user's install)."""
    cat = ua_catalogue.Catalogue(directory=None, io_presets=[FACTORY])
    ua_catalogue.apply_to_state(d.state, cat)
    return d


class TestIOMapPresets:
    def test_save_stores_routes_and_selects_the_preset(self, stack):
        stack.state.set(ROUTE, False)
        r = ok(stack, "/SaveIOMapPreset", {"name": "mine"})
        assert r == {"name": "mine", "presets": ["mine"]}
        assert stack.state.get_value("/IOMapPreset/value") == "mine"
        assert stack.state.get_value("/IOMapPresetDirty/value") is False
        assert {"value": "mine", "type": "file", "locked": False} in \
            stack.state.get_prop_dict("/IOMapPreset")["values"]
        data = json.loads((stack.commands.store_dir
                           / "x4.iomaps.json").read_text())
        assert data["format"] == cmdmod.FORMAT_IOMAPS
        vals = data["presets"]["mine"]["values"]
        assert vals[ROUTE] is False
        assert not [p for p in vals if p.endswith("/Type/value")]

    def test_nothing_reaches_the_driver(self, stack):
        stack.state.on_set = stack.hw_router.on_set
        stack.be.clear()
        ok(stack, "/SaveIOMapPreset", {"name": "mine"})
        stack.state.set("/IOMapPreset/value", "mine")
        assert (stack.be.mixer_params == [] and stack.be.bus_params == []
                and stack.be.driver_params == [])

    def test_setting_the_name_recalls_the_routes(self, stack):
        stack.state.set(ROUTE, False)
        ok(stack, "/SaveIOMapPreset", {"name": "mine"})
        stack.state.set(ROUTE, True)
        stack.state.set(OUTROUTE, False)
        post(stack, "set /IOMapPreset/value mine")
        assert stack.state.get_value(ROUTE) is False

    def test_recall_of_a_builtin_name_does_nothing(self, stack):
        with_factory(stack)
        stack.state.set(ROUTE, False)
        post(stack, f"set /IOMapPreset/value {FACTORY}")
        assert stack.state.get_value(ROUTE) is False

    def test_delete(self, stack):
        with_factory(stack)
        ok(stack, "/SaveIOMapPreset", {"name": "a"})
        ok(stack, "/SaveIOMapPreset", {"name": "b"})
        assert ok(stack, "/DeleteIOMapPreset", {"name": "a"}) == \
            {"presets": ["b"]}
        names = [v["value"] for v in
                 stack.state.get_prop_dict("/IOMapPreset")["values"]]
        assert "a" not in names and "b" in names
        assert FACTORY in names                  # the factory ones stay
        assert stack.commands.execute(
            "/DeleteIOMapPreset", {"name": "a"})[1] == ERR_FAILED

    def test_delete_of_the_selected_clears_the_selection(self, stack):
        ok(stack, "/SaveIOMapPreset", {"name": "a"})
        ok(stack, "/DeleteIOMapPreset", {"name": "a"})
        assert stack.state.get_value("/IOMapPreset/value") == ""

    def test_cannot_delete_a_builtin(self, stack):
        with_factory(stack)
        assert stack.commands.execute(
            "/DeleteIOMapPreset", {"name": FACTORY})[1] == ERR_FAILED
        assert FACTORY in [v["value"] for v in
                           stack.state.get_prop_dict("/IOMapPreset")["values"]]

    def test_rename(self, stack):
        ok(stack, "/SaveIOMapPreset", {"name": "a"})
        ok(stack, "/SaveIOMapPreset", {"name": "b"})
        r = ok(stack, "/RenameIOMapPreset", {"name": "b", "new_name": "c"})
        assert r == {"name": "c", "presets": ["a", "c"]}
        assert stack.state.get_value("/IOMapPreset/value") == "c"
        assert stack.commands.execute(
            "/RenameIOMapPreset", {"name": "a", "new_name": "c"})[1] == ERR_FAILED
        assert stack.commands.execute(
            "/RenameIOMapPreset", {"name": "zz", "new_name": "d"})[1] == ERR_FAILED

    def test_bad_names_fail(self, stack):
        for bad in ("", "a/b", ".x", "x" * 65):
            assert stack.commands.execute(
                "/SaveIOMapPreset", {"name": bad})[1] == ERR_FAILED

    def test_saved_presets_are_listed_after_a_restart(self, stack, tmp_path):
        ok(stack, "/SaveIOMapPreset", {"name": "kept"})
        stack.state.update_property("/IOMapPreset", {"values": [
            {"value": FACTORY, "type": "file", "locked": False}]})
        stack.commands._iomap_saved_names = []
        stack.commands.set_store(tmp_path, "x4")
        names = [v["value"] for v in
                 stack.state.get_prop_dict("/IOMapPreset")["values"]]
        assert names == [FACTORY, "kept"]

    def test_commands_are_known(self, stack):
        for n in ("SaveIOMapPreset", "DeleteIOMapPreset", "RenameIOMapPreset"):
            assert stack.commands.is_known(n)
            assert n.lower() in stack.commands._handlers

    def test_over_text_tcp(self, stack):
        c = post(stack, 'post /SaveIOMapPreset {"name": "tcp"}')
        assert c.last()["data"]["name"] == "tcp"
