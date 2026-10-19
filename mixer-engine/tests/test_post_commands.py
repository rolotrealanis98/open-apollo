"""A8: the `post` verb.  Unknown posts get the engine's error text, known
ones run (Identify, ClearClips/Solos, Copy/Paste, snapshots, sessions,
presets), over TCP text, UBJSON and WS with the same framing."""

import json

import pytest

import commands as cmdmod
import hardware
from commands import (CommandEngine, ERR_FAILED, ERR_UNRESOLVED,
                      parse_cmdparams, clean_name)
from conftest import DEVICE_MAP_X4, FakeBackend, make_readback
from hardware import HardwareRouter
from protocol import parse_command
from state_tree import StateTree
from ua_mixer_daemon import MixerDaemon
from ubjson_codec import decode_frame
from ws_protocol import parse_ws_command
from ws_server import WsServer

IN = "/devices/0/inputs"
AUX = "/devices/0/auxs"


class Client:
    """Stands in for MixerClient: records what is sent."""
    id = "t-1"
    helper = False

    def __init__(self, ubjson=False):
        self.ubjson = ubjson
        self.sent = []

    def send(self, data):
        self.sent.append(data)

    def replies(self):
        out = []
        for raw in self.sent:
            if self.ubjson:
                out.append(decode_frame(raw)[0])
            else:
                out.append(json.loads(raw.rstrip(b"\0")))
        return out

    def last(self):
        return self.replies()[-1]


class FakeLoop:
    """A scheduler that runs timers by hand."""

    def __init__(self):
        self.timers = []
        self.now = 1000.0

    def clock(self):
        return self.now

    def __call__(self, delay, fn):
        h = Handle(delay, fn)
        self.timers.append(h)
        return h

    def fire(self):
        h = self.timers.pop(0)
        self.now += h.delay
        if not h.cancelled:
            h.fn()


class Handle:
    def __init__(self, delay, fn):
        self.delay, self.fn, self.cancelled = delay, fn, False

    def cancel(self):
        self.cancelled = True


@pytest.fixture
def stack(tmp_path):
    tree = StateTree()
    tree.load_device_map(DEVICE_MAP_X4)
    tree.enable_persistence(tmp_path / "x4.state.json")
    be = FakeBackend()
    router = HardwareRouter(be, tree)
    d = MixerDaemon(tree, router)
    d.commands.set_store(tmp_path, "x4")
    d.commands.scheduler = FakeLoop()
    d.commands.clock = d.commands.scheduler.clock
    d._hw_init_done = True
    d.be = be
    return d


def post(d, line, client=None):
    c = client or Client()
    d._dispatch(c, line)
    return c


def ok(d, path, body=None):
    data, err = d.commands.execute(path, body)
    assert err is None, err
    return data


# ── Framing ─────────────────────────────────────────────────────────

class TestReplies:
    def test_unknown_command_text(self, stack):
        c = post(stack, "post /devices/0/NoSuchThing")
        assert c.last() == {"path": "/devices/0/NoSuchThing",
                            "error": ERR_UNRESOLVED}
        assert ERR_UNRESOLVED == "Unable to resolve the command for the post."

    def test_known_but_failing_command_text(self, stack):
        # PasteChannel with nothing copied
        c = post(stack, "post /devices/0/inputs/3/PasteChannel")
        assert c.last()["error"] == "Unable to execute command."

    def test_known_command_the_daemon_cannot_run(self, stack):
        c = post(stack, "post /devices/0/ToXML")
        assert c.last()["error"] == ERR_FAILED

    def test_success_text(self, stack):
        c = post(stack, "post /ClearSolos")
        assert c.last() == {"path": "/ClearSolos", "data": True}

    def test_func_id_is_echoed(self, stack):
        c = post(stack, "post /ClearSolos?func_id=7")
        assert c.last()["parameters"] == {"func_id": 7}
        c = post(stack, "post /Nope?func_id=8")
        assert c.last() == {"path": "/Nope", "error": ERR_UNRESOLVED,
                            "parameters": {"func_id": 8}}

    def test_ubjson_framing(self, stack):
        c = Client(ubjson=True)
        cmd = stack._parse_ubjson_command(
            {"cmd": "post", "url": "/Nope?func_id=3"})
        stack._dispatch_cmd(c, cmd)
        assert c.last() == {"path": "/Nope", "error": ERR_UNRESOLVED,
                            "parameters": {"func_id": 3}}
        cmd = stack._parse_ubjson_command(
            {"cmd": "post", "url": "/ClearSolos", "data": {}})
        stack._dispatch_cmd(c, cmd)
        assert c.last() == {"path": "/ClearSolos", "data": True}

    def test_ws_framing(self, stack):
        ws = WsServer(stack.state, stack.hw_router, commands=stack.commands)
        out = ws._dispatch(object(), parse_ws_command(
            "post /Nope?message_id=a:1"))
        assert json.loads(out[0]) == {"path": "/Nope",
                                      "error": ERR_UNRESOLVED,
                                      "parameters": {"message_id": "a:1"}}
        out = ws._dispatch(object(), parse_ws_command(
            "post /ClearSolos?message_id=a:2"))
        assert json.loads(out[0]) == {"path": "/ClearSolos", "data": True,
                                      "parameters": {"message_id": "a:2"}}

    def test_ws_without_engine_is_unresolved(self, stack):
        ws = WsServer(stack.state, stack.hw_router)
        out = ws._dispatch(object(), parse_ws_command("post /Identify"))
        assert json.loads(out[0])["error"] == ERR_UNRESOLVED

    def test_ws_challenge_handshake_unchanged(self, stack):
        ws = WsServer(stack.state, stack.hw_router, commands=stack.commands)
        assert ws._dispatch(object(), parse_ws_command(
            "post /response?func_id=1 abc")) == []

    def test_handshake_posts_still_handled(self, stack):
        c = post(stack, "post /request_challenge?func_id=1")
        assert "data" in c.last() and "error" not in c.last()
        c = post(stack, "post command_format?func_id=2 2")
        assert c.last()["path"] == "command_format"

    @pytest.mark.parametrize("name", [
        "PluginStartDemo", "SIDRLicenseStateUpdated", "GetPluginAuth",
        "PluginAuthorize", "AuthHardware"])
    def test_licensing_commands_stay_unresolved(self, stack, name):
        c = post(stack, f"post /plugins/0/{name}")
        assert c.last()["error"] == ERR_UNRESOLVED

    def test_command_names_are_case_insensitive_and_object_free(self, stack):
        assert post(stack, "post /clearsolos").last()["data"] is True
        assert post(stack, "post ClearSolos").last()["data"] is True

    def test_json_body_over_text_tcp(self, stack):
        c = post(stack, 'post /SaveAsSnapshot {"name": "two words"}')
        assert c.last()["data"]["name"] == "two words"

    def test_cmdparams_query(self, stack):
        c = post(stack, 'post /SaveAsSnapshot?cmdparams={"name":"q"}')
        assert c.last()["data"]["name"] == "q"
        assert "cmdparams" not in c.last().get("parameters", {})


class TestParams:
    def test_body_dict_and_cmdparams_merge(self):
        p = parse_cmdparams('{"a": 1, "b": 2}', {"cmdparams": '{"b": 3}'})
        assert p == {"a": 1, "b": 3}

    def test_scalar_is_value(self):
        assert parse_cmdparams(5) == {"value": 5}
        assert parse_cmdparams("hello") == {"value": "hello"}
        assert parse_cmdparams(None) == {}

    def test_bad_json_is_a_value(self):
        assert parse_cmdparams("{nope") == {"value": "{nope"}

    @pytest.mark.parametrize("bad", ["", "  ", "../x", "a/b", ".hid", "x" * 65,
                                     "a\nb", None, True, {}])
    def test_bad_names(self, bad):
        with pytest.raises(cmdmod.CommandError):
            clean_name(bad)

    def test_good_names(self):
        assert clean_name(" Verse 1 ") == "Verse 1"
        assert clean_name(3) == "3"


# ── Identify ────────────────────────────────────────────────────────

def identify_writes(be):
    return [p[3] for p in be.mixer_params if p[:3] == (2, 0, 0x1d)]


class TestIdentify:
    SEQ = [v for v, _ in cmdmod.IDENTIFY_SEQUENCE]

    def run_out(self, stack, limit=40):
        loop = stack.commands.scheduler
        for _ in range(limit):
            if not loop.timers:
                break
            loop.fire()

    def test_the_mixer_engine_sequence_once(self, stack):
        ok(stack, "/Identify")
        self.run_out(stack)
        # 3,1,2,1,3,1,2,1,0 (x4: 1 red, 2 green), nothing after the 0
        assert identify_writes(stack.be) == self.SEQ

    def test_sequence_holds(self, stack):
        ok(stack, "/Identify")
        loop = stack.commands.scheduler
        holds = []
        while loop.timers:
            holds.append(round(loop.timers[0].delay * 1000))
            loop.fire()
        # the last 1 ms step may be cut by the end time; the 0 still goes out
        want = [ms for _, ms in cmdmod.IDENTIFY_SEQUENCE]
        assert len(holds) >= len(want) - 1 and holds == want[:len(holds)]
        assert identify_writes(stack.be)[-1] == 0

    def test_seconds_repeat_the_sequence(self, stack):
        ok(stack, "/Identify", {"seconds": 3})
        self.run_out(stack, limit=60)
        w = identify_writes(stack.be)
        assert w[:len(self.SEQ)] == self.SEQ
        assert len(w) > len(self.SEQ) and w[-1] == 0

    def test_ends_with_off_even_when_timers_run_late(self, stack):
        ok(stack, "/devices/0/Identify", {"seconds": 1})
        stack.commands.scheduler.now += 10    # the end time has passed
        stack.commands.scheduler.fire()
        assert identify_writes(stack.be)[-1] == 0
        assert not stack.commands.scheduler.timers

    def test_steady_mode_holds_then_off(self, stack):
        ok(stack, "/Identify", {"seconds": 3, "blink": False})
        loop = stack.commands.scheduler
        assert loop.timers[0].delay == 3
        loop.fire()
        assert identify_writes(stack.be) == [1, 0]

    def test_second_identify_restarts(self, stack):
        ok(stack, "/Identify", {"seconds": 5})
        first = stack.commands.scheduler.timers[0]
        ok(stack, "/Identify", {"seconds": 5})
        assert first.cancelled

    def test_off(self, stack):
        ok(stack, "/Identify", {"seconds": 5})
        ok(stack, "/Identify", {"off": True})
        assert identify_writes(stack.be)[-1] == 0
        assert stack.commands.scheduler.timers[-1].cancelled

    def test_seconds_are_clamped(self, stack):
        ok(stack, "/Identify", {"seconds": 9999, "blink": False})
        assert stack.commands.scheduler.timers[0].delay == \
            cmdmod.IDENTIFY_MAX_SECONDS

    def test_bad_seconds_fails(self, stack):
        c = post(stack, 'post /Identify {"seconds": "soon"}')
        assert c.last()["error"] == ERR_FAILED

    def test_no_device_fails(self, stack):
        stack.be.connected = False
        c = post(stack, "post /Identify")
        assert c.last()["error"] == ERR_FAILED

    def test_paused_device_fails(self, stack):
        stack.hw_router.set_paused(True)
        c = post(stack, "post /Identify")
        assert c.last()["error"] == ERR_FAILED
        assert identify_writes(stack.be) == []

    def test_uses_the_router_monitor_param_path(self, stack, monkeypatch):
        seen = []
        orig = stack.hw_router.on_set
        monkeypatch.setattr(stack.hw_router, "on_set",
                            lambda p, v: (seen.append((p, v)), orig(p, v)))
        ok(stack, "/Identify")
        # the raw value goes out (set_identify), not a bool through on_set
        assert identify_writes(stack.be)[0] == 3

    def test_pause_ends_a_running_blink_first(self, stack):
        ok(stack, "/Identify", {"seconds": 5})
        stack.be.mixer_params.clear()
        stack._on_driver_event(0x1005, 4)         # UA_EVT_PAUSED, device
        assert identify_writes(stack.be) == [0]   # LED off, then the pause
        assert stack.commands.scheduler.timers[-1].cancelled
        stack.be.mixer_params.clear()
        stack.commands.scheduler.fire()           # the cancelled tick is dead
        assert identify_writes(stack.be) == []

    def test_software_only_daemon_fails(self, tmp_path):
        tree = StateTree()
        tree.load_device_map(DEVICE_MAP_X4)
        d = MixerDaemon(tree, None)
        assert d.commands.execute("/Identify")[1] == ERR_FAILED

    def test_led_test_is_not_runnable(self, stack):
        assert post(stack, "post /LEDTest").last()["error"] == ERR_FAILED


# ── ClearClips / ClearSolos ─────────────────────────────────────────

class TestClears:
    def test_clear_solos_runs_the_solo_recalc(self, stack):
        stack.state.set(f"{IN}/0/Solo/value", True)
        stack.state.set(f"{IN}/2/Solo/value", True)
        stack.be.clear()
        ok(stack, "/ClearSolos")
        assert stack.state.get_value(f"{IN}/0/Solo/value") is False
        assert stack.state.get_value(f"{IN}/2/Solo/value") is False
        # the last recalculation writes every channel's real gain again
        assert stack.be.bus_params

    def test_clear_solos_without_solos_is_a_noop(self, stack):
        stack.be.clear()
        ok(stack, "/ClearSolos")
        assert stack.be.bus_params == []

    def test_clear_solos_tells_subscribers(self, stack):
        seen = []
        stack.state.register_callback("c", lambda p, v: seen.append((p, v)))
        stack.state.subscribe("c", f"{IN}/1/Solo/value")
        stack.state.set(f"{IN}/1/Solo/value", True)
        seen.clear()
        ok(stack, "/ClearSolos")
        assert seen == [(f"{IN}/1/Solo/value", False)]

    def test_clear_clips_resets_state_and_calls_the_meter_hook(self, stack):
        called = []

        class M:
            def clear_clips(self):
                called.append(1)
        stack._metering = M()
        path = f"{IN}/0/meters/0/Clip/value"
        stack._last_meter[path] = True
        stack._last_meter[f"{IN}/0/meters/0/MeterPeakLevel/value"] = -3.0
        seen = []
        stack.state.register_callback("c", lambda p, v: seen.append((p, v)))
        stack.state._subs[path].add("c")
        ok(stack, "/ClearClips")
        assert called == [1]
        assert stack._last_meter[path] is False
        assert f"{IN}/0/meters/0/MeterPeakLevel/value" not in stack._last_meter
        assert seen == [(path, False)]

    def test_clear_clips_without_a_meter_hook(self, stack):
        ok(stack, "/ClearClips")        # NullMeter has no clear_clips


# ── Copy / paste ────────────────────────────────────────────────────

class TestCopyPaste:
    def test_copy_then_paste_applies_through_the_set_path(self, stack):
        t = stack.state
        t.set(f"{IN}/0/FaderLevel/value", -7.0)
        t.set(f"{IN}/0/Pan/value", 0.5)
        t.set(f"{IN}/0/sends/2/Gain/value", -12.0)
        ok(stack, f"{IN}/0/CopyChannel")
        stack.be.clear()
        data = ok(stack, f"{IN}/5/PasteChannel")
        assert data["applied"] > 0
        assert t.get_value(f"{IN}/5/FaderLevel/value") == -7.0
        assert t.get_value(f"{IN}/5/Pan/value") == 0.5
        assert t.get_value(f"{IN}/5/sends/2/Gain/value") == -12.0
        assert stack.be.bus_params       # reached the hardware router

    def test_identity_and_preamp_are_not_copied(self, stack):
        t = stack.state
        t.set(f"{IN}/0/preamps/0/48V/value", True)
        ok(stack, f"{IN}/0/CopyChannel")
        clip = stack.commands._clipboard["values"]
        assert "Name" not in clip and "IOType" not in clip
        assert not any(k.startswith("preamps") for k in clip)
        assert not any(k.startswith(("meters", "effects")) for k in clip)
        ok(stack, f"{IN}/1/PasteChannel")
        assert t.get_value(f"{IN}/1/preamps/0/48V/value") is False
        assert t.get_value(f"{IN}/1/Name/value") != \
            t.get_value(f"{IN}/0/Name/value")

    def test_paste_to_several_targets(self, stack):
        stack.state.set(f"{IN}/0/FaderLevel/value", -3.0)
        ok(stack, f"{IN}/0/CopyChannel")
        ok(stack, f"{IN}/1/PasteChannel", {"targets": [f"{IN}/6", f"{IN}/7"]})
        assert stack.state.get_value(f"{IN}/6/FaderLevel/value") == -3.0
        assert stack.state.get_value(f"{IN}/7/FaderLevel/value") == -3.0
        assert stack.state.get_value(f"{IN}/1/FaderLevel/value") != -3.0

    def test_paste_across_kinds_fails(self, stack):
        ok(stack, f"{IN}/0/CopyChannel")
        assert stack.commands.execute(f"{AUX}/0/PasteChannel")[1] == ERR_FAILED

    def test_paste_without_copy_fails(self, stack):
        assert stack.commands.execute(f"{IN}/0/PasteChannel")[1] == ERR_FAILED

    def test_copy_of_a_missing_channel_fails(self, stack):
        assert stack.commands.execute(f"{IN}/999/CopyChannel")[1] == ERR_FAILED
        assert stack.commands.execute("/CopyChannel")[1] == ERR_FAILED

    def test_copy_to_sends(self, stack):
        t = stack.state
        t.set(f"{IN}/2/FaderLevel/value", -9.0)
        t.set(f"{IN}/2/Pan/value", -0.25)
        data = ok(stack, f"{IN}/2/CopyToSends", {"sends": [2, 3]})
        assert data["sends"] == 2
        assert t.get_value(f"{IN}/2/sends/2/Gain/value") == -9.0
        assert t.get_value(f"{IN}/2/sends/3/Pan/value") == -0.25
        assert t.get_value(f"{IN}/2/sends/0/Gain/value") == -144.0

    def test_copy_to_sends_all(self, stack):
        stack.state.set(f"{IN}/2/FaderLevel/value", -9.0)
        assert ok(stack, f"{IN}/2/CopyToSends")["sends"] == 6


# ── Snapshots ───────────────────────────────────────────────────────

class TestSnapshots:
    def test_save_and_recall_by_setting_the_property(self, stack):
        t = stack.state
        t.set(f"{IN}/0/FaderLevel/value", -5.0)
        t.set(f"{IN}/1/Mute/value", True)
        assert ok(stack, "/SaveAsSnapshot", {"name": "A"})["snapshots"] == ["A"]
        assert t.get_value("/Snapshot/value") == "A"
        t.set(f"{IN}/0/FaderLevel/value", -20.0)
        t.set(f"{IN}/1/Mute/value", False)
        stack.be.clear()
        c = post(stack, "post /Dummy")      # nothing
        # a client sets /Snapshot: the daemon recalls it
        post(stack, "set /Snapshot/value A")
        assert t.get_value(f"{IN}/0/FaderLevel/value") == -5.0
        assert t.get_value(f"{IN}/1/Mute/value") is True
        assert stack.be.bus_params

    def test_snapshot_skips_preamps_monitor_level_and_names(self, stack):
        t = stack.state
        mon = stack.hw_router.monitor_output_index()
        t.set(f"{IN}/0/preamps/0/48V/value", True)
        t.set(f"/devices/0/outputs/{mon}/CRMonitorLevel/value", -20.0)
        ok(stack, "/SaveAsSnapshot", {"name": "A"})
        vals = stack.commands._snapshots()["snapshots"]["A"]["values"]
        assert not any("/preamps/" in p or "/meters/" in p for p in vals)
        assert not any(p.endswith("/CRMonitorLevel/value") for p in vals)
        assert not any(p.endswith("/Name/value") for p in vals)
        assert f"{IN}/0/FaderLevel/value" in vals

    def test_overwrite_same_name_keeps_position(self, stack):
        for n in ("A", "B"):
            ok(stack, "/SaveAsSnapshot", {"name": n})
        ok(stack, "/SaveAsSnapshot", {"name": "A"})
        assert stack.commands._snapshots()["order"] == ["A", "B"]

    def test_move_and_delete(self, stack):
        for n in ("A", "B", "C"):
            ok(stack, "/SaveAsSnapshot", {"name": n})
        assert ok(stack, "/MoveSnapshot",
                  {"name": "C", "index": 0})["snapshots"] == ["C", "A", "B"]
        assert ok(stack, "/MoveSnapshot",
                  {"name": "C", "index": 99})["snapshots"] == ["A", "B", "C"]
        assert ok(stack, "/DeleteSnapshot",
                  {"name": "B"})["snapshots"] == ["A", "C"]
        assert stack.commands.execute(
            "/DeleteSnapshot", {"name": "B"})[1] == ERR_FAILED
        assert stack.commands.execute(
            "/MoveSnapshot", {"name": "Z", "index": 0})[1] == ERR_FAILED

    def test_delete_clears_the_current_snapshot_name(self, stack):
        ok(stack, "/SaveAsSnapshot", {"name": "A"})
        ok(stack, "/DeleteSnapshot", {"name": "A"})
        assert stack.state.get_value("/Snapshot/value") == ""

    def test_survives_a_restart(self, stack, tmp_path):
        stack.state.set(f"{IN}/0/FaderLevel/value", -5.0)
        ok(stack, "/SaveAsSnapshot", {"name": "A"})
        other = CommandEngine(stack)
        other.set_store(tmp_path, "x4")
        assert list(other._snapshots()["snapshots"]) == ["A"]
        assert (tmp_path / "x4.snapshots.json").exists()

    def test_export_inline_and_import(self, stack, tmp_path):
        stack.state.set(f"{IN}/0/FaderLevel/value", -5.0)
        ok(stack, "/SaveAsSnapshot", {"name": "A"})
        ok(stack, "/SaveAsSnapshot", {"name": "B"})
        exp = ok(stack, "/ExportSnapshots", {"names": ["A"]})
        assert exp["format"] == cmdmod.FORMAT_SNAPSHOTS and exp["order"] == ["A"]
        ok(stack, "/DeleteSnapshot", {"name": "A"})
        res = ok(stack, "/ImportSnapshots", exp)
        assert res["imported"] == 1 and res["snapshots"] == ["B", "A"]

    def test_export_import_through_a_file(self, stack, tmp_path):
        ok(stack, "/SaveAsSnapshot", {"name": "A"})
        r = ok(stack, "/ExportSnapshots", {"file": "mine.json"})
        assert r == {"file": "mine.json", "count": 1}
        assert (tmp_path / "x4.exports" / "mine.json").exists()
        ok(stack, "/DeleteSnapshot", {"name": "A"})
        assert ok(stack, "/ImportSnapshots",
                  {"file": "mine.json"})["imported"] == 1

    @pytest.mark.parametrize("file", ["../evil", "/etc/passwd", "a/b", ".x", ""])
    def test_file_names_cannot_escape_the_exports_dir(self, stack, file):
        assert stack.commands.execute(
            "/ExportSnapshots", {"file": file})[1] == ERR_FAILED
        assert stack.commands.execute(
            "/ImportSnapshots", {"file": file})[1] == ERR_FAILED

    def test_import_rejects_other_data(self, stack):
        assert stack.commands.execute(
            "/ImportSnapshots", {"format": "nope"})[1] == ERR_FAILED

    def test_import_without_overwrite_keeps_existing(self, stack):
        stack.state.set(f"{IN}/0/FaderLevel/value", -5.0)
        ok(stack, "/SaveAsSnapshot", {"name": "A"})
        exp = ok(stack, "/ExportSnapshots")
        stack.state.set(f"{IN}/0/FaderLevel/value", -9.0)
        ok(stack, "/SaveAsSnapshot", {"name": "A"})
        body = dict(exp, overwrite=False)
        assert ok(stack, "/ImportSnapshots", body)["imported"] == 0

    def test_import_drops_foreign_paths(self, stack):
        body = {"format": cmdmod.FORMAT_SNAPSHOTS, "order": ["X"],
                "snapshots": {"X": {"values": {
                    "/etc/passwd": 1, f"{IN}/0/FaderLevel/value": -1.0}}}}
        ok(stack, "/ImportSnapshots", body)
        vals = stack.commands._snapshots()["snapshots"]["X"]["values"]
        assert list(vals) == [f"{IN}/0/FaderLevel/value"]

    def test_no_store_dir_fails(self, stack):
        stack.commands.store_dir = None
        assert stack.commands.execute(
            "/SaveAsSnapshot", {"name": "A"})[1] == ERR_FAILED

    def test_name_required(self, stack):
        assert stack.commands.execute("/SaveAsSnapshot")[1] == ERR_FAILED
        assert stack.commands.execute(
            "/SaveAsSnapshot", {"name": "../x"})[1] == ERR_FAILED

    def test_console_save_recall_and_prime_on_the_wire(self, stack):
        # The Snapshots section's requests, as console-rs sends them:
        # SAVE AS posts {"snapshotName"}, RECALL sets /Snapshot, a row
        # selection sets /PrimedSnapshot (a prime recalls nothing)
        t = stack.state
        t.set(f"{IN}/0/FaderLevel/value", -5.0)
        c = post(stack, 'post /SaveAsSnapshot {"snapshotName":"Live/A"}')
        assert "error" not in c.last(), c.last()
        assert t.get_prop_dict("/Snapshot")["values"] == [
            {"type": "folder", "value": "Live", "locked": False,
             "children": [{"type": "file", "value": "A", "locked": False}]}]
        t.set(f"{IN}/0/FaderLevel/value", -20.0)
        post(stack, 'set /PrimedSnapshot/value "Live/A"')
        assert t.get_value("/PrimedSnapshot/value") == "Live/A"
        assert t.get_value(f"{IN}/0/FaderLevel/value") == -20.0
        post(stack, 'set /Snapshot/value "Live/A"')
        assert t.get_value("/Snapshot/value") == "Live/A"
        assert t.get_value(f"{IN}/0/FaderLevel/value") == -5.0
        # a recall is not an edit
        assert t.get_value("/Dirty/value") is not True

    def test_folder_rename_and_delete_by_folder_name(self, stack):
        for n in ("Live/A", "Live/B", "C"):
            ok(stack, "/SaveAsSnapshot", {"snapshotName": n})
        post(stack, 'set /PrimedSnapshot/value "Live/B"')
        r = ok(stack, "/MoveSnapshot",
               {"origName": "Live", "newName": "Show", "shuffle": False})
        assert r["snapshots"] == ["Show/A", "Show/B", "C"]
        assert stack.state.get_value("/PrimedSnapshot/value") == "Show/B"
        assert ok(stack, "/DeleteSnapshot",
                  {"snapshotName": "Show"})["snapshots"] == ["C"]
        assert stack.state.get_value("/PrimedSnapshot/value") == ""
        assert [n["value"] for n in
                stack.state.get_prop_dict("/Snapshot")["values"]] == ["C"]

    def test_export_and_import_all_through_the_volume(self, stack, tmp_path):
        vol = stack.state.get_prop_dict("/Snapshot")["volumes"][0]["volume"]
        ok(stack, "/SaveAsSnapshot", {"snapshotName": "Live/A"})
        ok(stack, "/ExportSnapshots", {"volume": vol, "folder": "Backup"})
        assert (tmp_path / "x4.exports" / "Backup.json").exists()
        ok(stack, "/SaveAsSnapshot", {"snapshotName": "B"})
        r = ok(stack, "/ImportSnapshots",
               {"volume": vol, "folder": "Backup", "append": False})
        assert r["snapshots"] == ["Live/A"]
        r = ok(stack, "/ImportSnapshots",
               {"volume": vol, "folder": "Backup", "append": True})
        assert r["snapshots"] == ["Live/A"]
        assert stack.commands.execute(
            "/ExportSnapshots", {"volume": "elsewhere", "folder": "x"})[1] \
            == ERR_FAILED

    def test_setting_an_unknown_snapshot_name_does_nothing(self, stack):
        stack.state.set(f"{IN}/0/FaderLevel/value", -5.0)
        post(stack, "set /Snapshot/value Nope")
        assert stack.state.get_value(f"{IN}/0/FaderLevel/value") == -5.0


# ── Presets ─────────────────────────────────────────────────────────

class TestPresets:
    def test_save_names_the_channel_and_recall_applies(self, stack):
        t = stack.state
        t.set(f"{IN}/0/FaderLevel/value", -4.0)
        t.set(f"{IN}/0/Pan/value", 0.3)
        r = ok(stack, f"{IN}/0/SaveAsPreset", {"name": "vox"})
        assert r["presets"] == ["vox"]
        assert t.get_value(f"{IN}/0/Preset/value") == "vox"
        # recall on another channel by setting its Preset
        post(stack, f"set {IN}/3/Preset/value vox")
        assert t.get_value(f"{IN}/3/FaderLevel/value") == -4.0
        assert t.get_value(f"{IN}/3/Pan/value") == 0.3
        assert t.get_value(f"{IN}/3/Preset/value") == "vox"

    def test_presets_are_per_kind(self, stack):
        ok(stack, f"{IN}/0/SaveAsPreset", {"name": "vox"})
        stack.state.set(f"{AUX}/0/FaderLevel/value", -2.0)
        post(stack, f"set {AUX}/1/Preset/value vox")
        assert stack.state.get_value(f"{AUX}/1/FaderLevel/value") != -2.0

    def test_move_delete(self, stack):
        for n in ("a", "b", "c"):
            ok(stack, f"{IN}/0/SaveAsPreset", {"name": n})
        assert ok(stack, f"{IN}/0/MovePreset",
                  {"name": "c", "index": 0})["presets"] == ["c", "a", "b"]
        assert ok(stack, f"{IN}/9/DeletePreset",
                  {"name": "a"})["presets"] == ["c", "b"]
        assert stack.commands.execute(
            f"{IN}/0/DeletePreset", {"name": "a"})[1] == ERR_FAILED

    def test_kind_param_for_delete(self, stack):
        ok(stack, f"{IN}/0/SaveAsPreset", {"name": "a"})
        assert ok(stack, "/DeletePreset",
                  {"name": "a", "kind": "inputs"})["presets"] == []

    def test_save_needs_a_channel(self, stack):
        assert stack.commands.execute(
            "/SaveAsPreset", {"name": "a"})[1] == ERR_FAILED

    def test_plugin_preset_property_is_not_a_channel_preset(self, stack):
        ok(stack, f"{IN}/0/SaveAsPreset", {"name": "vox"})
        stack.state.set(f"{IN}/2/FaderLevel/value", -3.0)
        post(stack, f"set {IN}/0/effects/0/Preset/value vox")
        assert stack.state.get_value(f"{IN}/0/FaderLevel/value") != -3.0

    def test_presets_survive_restart(self, stack, tmp_path):
        ok(stack, f"{IN}/0/SaveAsPreset", {"name": "vox"})
        other = CommandEngine(stack)
        other.set_store(tmp_path, "x4")
        assert other._presets()["kinds"]["inputs"]["order"] == ["vox"]


# ── Sessions ────────────────────────────────────────────────────────

class TestSessions:
    def test_save_as_writes_own_json_not_xml(self, stack, tmp_path):
        stack.state.set(f"{IN}/0/FaderLevel/value", -5.0)
        r = ok(stack, "/SaveSessionAs", {"name": "Gig 1"})
        assert r == {"name": "Gig 1", "sessions": ["Gig 1"]}
        f = tmp_path / "x4.sessions" / "Gig 1.session.json"
        data = json.loads(f.read_text())
        assert data["format"] == cmdmod.FORMAT_SESSION
        assert data["values"][f"{IN}/0/FaderLevel/value"] == -5.0
        assert not f.read_text().lstrip().startswith("<")
        assert stack.state.get_value("/Session/value") == "Gig 1"

    def test_save_uses_the_current_session(self, stack):
        ok(stack, "/SaveSessionAs", {"name": "Gig"})
        stack.state.set(f"{IN}/0/FaderLevel/value", -8.0)
        ok(stack, "/SaveSession")
        data = stack.commands._session_file("Gig").load({})
        assert data["values"][f"{IN}/0/FaderLevel/value"] == -8.0

    def test_save_with_a_foreign_session_path_keeps_the_last_part(self, stack):
        # a Mixer Engine reports /Session as a path, possibly from another
        # machine (the shipped map's own is empty)
        assert stack.state.get_value("/Session/value") == ""
        stack.state.set("/Session/value", "../../../Desktop/Gigs/Main Room")
        r = ok(stack, "/SaveSession")
        assert r["name"] == "Main Room"

    def test_save_without_name_or_session_fails(self, stack):
        stack.state.set("/Session/value", "")
        assert stack.commands.execute("/SaveSession")[1] == ERR_FAILED

    def test_volatile_state_is_not_saved(self, stack):
        stack.state.set("/SampleRate/value", 96000)
        stack.state.set(f"{IN}/0/FaderLevel/value", -1.0)
        ok(stack, "/SaveSessionAs", {"name": "S"})
        vals = stack.commands._session_file("S").load({})["values"]
        assert "/SampleRate/value" not in vals
        assert "/Session/value" not in vals

    def test_open_restores_values_and_writes_the_hardware(self, stack):
        t = stack.state
        t.set(f"{IN}/0/FaderLevel/value", -5.0)
        t.set(f"{IN}/1/Mute/value", True)
        t.set(f"{IN}/0/sends/2/Gain/value", -12.0)
        t.set(f"{IN}/0/preamps/0/Gain/value", 33.0)
        mon = stack.hw_router.monitor_output_index()
        t.set(f"/devices/0/outputs/{mon}/CRMonitorLevel/value", -30.0)
        ok(stack, "/SaveSessionAs", {"name": "S"})
        t.set(f"{IN}/0/FaderLevel/value", 0.0)
        t.set(f"{IN}/1/Mute/value", False)
        t.set(f"{IN}/0/sends/2/Gain/value", -144.0)
        t.set(f"{IN}/0/preamps/0/Gain/value", 12.0)
        t.set(f"/devices/0/outputs/{mon}/CRMonitorLevel/value", -60.0)
        stack.be.clear()
        r = ok(stack, "/OpenSession", {"name": "S"})
        assert r["applied"] > 0
        assert t.get_value(f"{IN}/0/FaderLevel/value") == -5.0
        assert t.get_value(f"{IN}/1/Mute/value") is True
        assert t.get_value(f"{IN}/0/sends/2/Gain/value") == -12.0
        # preamp gain and monitor level come from the session ...
        assert t.get_value(f"{IN}/0/preamps/0/Gain/value") == 33.0
        raw = hardware.encode_monitor_level(-30.0)
        assert any(p[:3] == (2, 1, 0x01) or (p[0] == 2 and p[2] == 0x01
                                             and p[3] == raw)
                   for p in stack.be.mixer_params)
        # ... gain reaches the preamp, the A6 sends and the faders reach the DSP
        assert any(p[0] == 1 and p[1] == 0 for p in stack.be.mixer_params)
        assert any(b == 0x00 and sub == hardware.SUB_PARAM_CUE1
                   for b, sub, _v in stack.be.bus_params)
        assert t.get_value("/Session/value") == "S"

    def test_open_order_preamp_monitor_then_strips_then_reapply(self, stack):
        t = stack.state
        t.set(f"{IN}/0/FaderLevel/value", -5.0)
        t.set(f"{IN}/0/sends/2/Gain/value", -12.0)
        t.set(f"{IN}/0/preamps/0/Gain/value", 33.0)
        ok(stack, "/SaveSessionAs", {"name": "S"})
        t.set(f"{IN}/0/FaderLevel/value", 0.0)
        t.set(f"{IN}/0/sends/2/Gain/value", -144.0)
        calls = []
        orig = stack.hw_router.on_set
        stack.hw_router.on_set = lambda p, v: (calls.append(p), orig(p, v))
        reapplied = []
        orig_r = stack.hw_router.reapply_saved_state
        stack.hw_router.reapply_saved_state = \
            lambda s: (reapplied.append(len(calls)), orig_r(s))[1]
        ok(stack, "/OpenSession", {"name": "S"})
        names = [p.rsplit("/", 2)[-2] for p in calls]
        assert names.index("Gain") < names.index("FaderLevel")
        # the A6 re-apply runs after the preamp and the strip pushes
        assert reapplied and reapplied[0] >= names.index("FaderLevel") + 1

    def test_open_while_paused_updates_the_tree_only(self, stack):
        stack.state.set(f"{IN}/0/FaderLevel/value", -5.0)
        ok(stack, "/SaveSessionAs", {"name": "S"})
        stack.state.set(f"{IN}/0/FaderLevel/value", 0.0)
        stack.hw_router.set_paused(True)
        stack.be.clear()
        ok(stack, "/OpenSession", {"name": "S"})
        assert stack.state.get_value(f"{IN}/0/FaderLevel/value") == -5.0
        assert stack.be.bus_params == [] and stack.be.mixer_params == []

    def test_open_tells_subscribers_once(self, stack):
        stack.state.set(f"{IN}/0/FaderLevel/value", -5.0)
        ok(stack, "/SaveSessionAs", {"name": "S"})
        stack.state.set(f"{IN}/0/FaderLevel/value", 0.0)
        seen = []
        stack.state.register_callback("c", lambda p, v: seen.append((p, v)))
        stack.state.subscribe("c", f"{IN}/0/FaderLevel/value")
        ok(stack, "/OpenSession", {"name": "S"})
        assert seen == [(f"{IN}/0/FaderLevel/value", -5.0)]

    def test_open_unknown_or_foreign_file_fails(self, stack, tmp_path):
        assert stack.commands.execute(
            "/OpenSession", {"name": "nope"})[1] == ERR_FAILED
        d = tmp_path / "x4.sessions"
        d.mkdir(exist_ok=True)
        (d / "xml.session.json").write_text("<session/>")
        assert stack.commands.execute(
            "/OpenSession", {"name": "xml"})[1] == ERR_FAILED
        (d / "wrong.session.json").write_text('{"format":"x","values":{}}')
        assert stack.commands.execute(
            "/OpenSession", {"name": "wrong"})[1] == ERR_FAILED

    def test_open_ignores_volatile_and_unknown_paths(self, stack, tmp_path):
        d = tmp_path / "x4.sessions"
        d.mkdir(exist_ok=True)
        (d / "z.session.json").write_text(json.dumps({
            "format": cmdmod.FORMAT_SESSION, "version": 1, "values": {
                "/SampleRate/value": 96000, "/nope/value": 1,
                f"{IN}/0/FaderLevel/value": -2.0}}))
        r = ok(stack, "/OpenSession", {"name": "z"})
        assert r["applied"] == 1
        assert stack.state.get_value("/SampleRate/value") != 96000

    def test_new_session_goes_back_to_the_map_defaults(self, stack):
        t = stack.state
        default = t.get_value(f"{IN}/0/FaderLevel/value")
        t.set(f"{IN}/0/FaderLevel/value", -5.0)
        t.set(f"{IN}/0/preamps/0/48V/value", True)
        mon = stack.hw_router.monitor_output_index()
        mdefault = t.get_value(f"/devices/0/outputs/{mon}/CRMonitorLevel/value")
        t.set(f"/devices/0/outputs/{mon}/CRMonitorLevel/value", -3.0)
        r = ok(stack, "/NewSession", {"name": "fresh"})
        assert r["reset"] >= 1
        assert t.get_value(f"{IN}/0/FaderLevel/value") == default
        # preamps and the monitor level stay what they are
        assert t.get_value(f"{IN}/0/preamps/0/48V/value") is True
        assert t.get_value(
            f"/devices/0/outputs/{mon}/CRMonitorLevel/value") == -3.0 != mdefault
        assert t.get_value("/Session/value") == "fresh"

    def test_new_session_resets_a_value_restored_from_the_state_file(
            self, tmp_path):
        sf = tmp_path / "x4.state.json"
        sf.write_text(json.dumps({f"{IN}/0/FaderLevel/value": -9.0}))
        tree = StateTree()
        tree.load_device_map(DEVICE_MAP_X4)
        default = tree.get_value(f"{IN}/0/FaderLevel/value")
        tree.enable_persistence(sf)
        d = MixerDaemon(tree, HardwareRouter(FakeBackend(), tree))
        assert tree.get_value(f"{IN}/0/FaderLevel/value") == -9.0
        assert d.commands.execute("/NewSession")[1] is None
        assert tree.get_value(f"{IN}/0/FaderLevel/value") == default

    def test_delete_and_rename(self, stack):
        ok(stack, "/SaveSessionAs", {"name": "A"})
        ok(stack, "/SaveSessionAs", {"name": "B"})
        r = ok(stack, "/RenameSession", {"name": "A", "new_name": "A2"})
        assert r == {"name": "A2", "sessions": ["A2", "B"]}
        assert stack.commands.execute(
            "/RenameSession", {"name": "A", "new_name": "X"})[1] == ERR_FAILED
        assert stack.commands.execute(
            "/RenameSession", {"name": "A2", "new_name": "B"})[1] == ERR_FAILED
        assert ok(stack, "/DeleteSession", {"name": "B"})["sessions"] == ["A2"]
        assert stack.commands.execute(
            "/DeleteSession", {"name": "B"})[1] == ERR_FAILED

    def test_rename_of_the_current_session_follows(self, stack):
        ok(stack, "/SaveSessionAs", {"name": "A"})
        ok(stack, "/RenameSession", {"from": "A", "to": "Z"})
        assert stack.state.get_value("/Session/value") == "Z"
        assert stack.commands._session_file("Z").load({})["name"] == "Z"

    def test_names_with_odd_characters_round_trip(self, stack):
        ok(stack, "/SaveSessionAs", {"name": "50% Mix: v2 (live)"})
        assert stack.commands.session_names() == ["50% Mix: v2 (live)"]
        ok(stack, "/OpenSession", {"name": "50% Mix: v2 (live)"})

    @pytest.mark.parametrize("bad", ["../x", "a/b", ".hidden", "a\\b"])
    def test_session_names_cannot_escape(self, stack, bad):
        assert stack.commands.execute(
            "/SaveSessionAs", {"name": bad})[1] == ERR_FAILED

    def test_save_last_used(self, stack, tmp_path):
        ok(stack, "/SaveLastUsedSession")
        assert (tmp_path / "x4.sessions" / ".last_used.session.json").exists()
        assert stack.commands.session_names() == []   # not a user session

    def test_save_as_snapshots_do_not_leak_into_sessions(self, stack):
        ok(stack, "/SaveAsSnapshot", {"name": "A"})
        ok(stack, "/SaveSessionAs", {"name": "S"})
        vals = stack.commands._session_file("S").load({})["values"]
        assert "/Snapshot/value" not in vals
