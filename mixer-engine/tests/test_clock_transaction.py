"""/SampleRate and /ClockSource SETs are transactions: written to the driver
first, published only when it took the value (ported from the Codex
daemon's test_clock_protocol_outcomes.py and
test_global_hardware_transaction.py, d418807).

driver-v2 differences kept: the error text is the Mixer Engine's "Unable
to set value." / "Unable to resolve path for set." (Codex used its own
openapollo.* codes), the SET's echo to its sender is unchanged (driver-v2
never excludes a TCP sender), "Word Clock" and the device's clock menu
name the sources, and PipeWire's clock.force-rate counts as accepted.
"""

import copy
import json
from unittest.mock import Mock

import pytest

import hardware
from conftest import FakeBackend
from hardware import (CLOCK_SOURCE_MAP, DRIVER_PARAM_CLOCK_SOURCE,
                      DRIVER_PARAM_SAMPLE_RATE, HardwareRouter,
                      normalize_global)
from helper_tree import HelperTree
from protocol import Command
from state_tree import StateTree
from ua_mixer_daemon import MixerClient, MixerDaemon
from ubjson_codec import decode_frame
from ws_protocol import parse_ws_command
from ws_server import WsServer

PROPS = {"SampleRate": {"type": "int", "value": 48000},
         "ClockSource": {"type": "string", "value": "Internal",
                         "values": ["Internal", "S/PDIF", "ADAT"]}}


class Writer:
    def __init__(self):
        self.frames = []

    def get_extra_info(self, name):
        return ("127.0.0.1", 4720)

    def write(self, data):
        self.frames.append(data)


class Route:
    def __init__(self, binary, ok=True, software=False):
        self.state = StateTree()
        for name, prop in PROPS.items():
            self.state.add_runtime_property("/" + name, copy.deepcopy(prop))
        self.state._schedule_save = Mock()
        self.helper = HelperTree()
        self.backend = FakeBackend()
        self.backend.set_driver_param = Mock(return_value=ok)
        self.router = HardwareRouter(self.backend, self.state)
        self.daemon = MixerDaemon(self.state, None if software else self.router,
                                  helper_tree=self.helper)
        # a second write through the tree's on_set would be a regression
        self.state.on_set = Mock(wraps=self.router.on_set)
        self.writer = Writer()
        self.client = MixerClient(None, self.writer, ubjson=binary)
        self.client.ubjson = binary
        self.binary = binary
        self.seen = []
        self.state.register_callback("obs", lambda p, v: self.seen.append((p, v)))
        for name in PROPS:
            self.state.subscribe("obs", f"/{name}/value")

    def set(self, path, value, func_id="17"):
        self.daemon._handle_set(self.client, Command(
            "set", path, value, raw_params={"func_id": func_id}))

    def replies(self):
        if self.binary:
            return [decode_frame(f)[0] for f in self.writer.frames]
        return [json.loads(f.rstrip(b"\0")) for f in self.writer.frames]

    def snapshot(self):
        return copy.deepcopy(self.state.root), dict(self.state._dirty)


@pytest.fixture(autouse=True)
def no_pipewire(monkeypatch):
    monkeypatch.setattr(hardware, "pipewire_force_rate",
                        lambda rate, device=None: False)


@pytest.mark.parametrize("binary", [False, True])
@pytest.mark.parametrize("path,value,canonical,param,native", [
    ("/ClockSource", "S/PDIF", "S/PDIF", DRIVER_PARAM_CLOCK_SOURCE, 0x00),
    ("/ClockSource/value", "Word Clock", "Word Clock",
     DRIVER_PARAM_CLOCK_SOURCE, 0x07),
    ("/SampleRate/value", 96000, 96000, DRIVER_PARAM_SAMPLE_RATE, 96000),
    ("/SampleRate", "96000", 96000, DRIVER_PARAM_SAMPLE_RATE, 96000),
    ("/SampleRate", 96000.0, 96000, DRIVER_PARAM_SAMPLE_RATE, 96000),
])
def test_accepted_write_is_published_once(binary, path, value, canonical,
                                          param, native):
    r = Route(binary)
    r.set(path, value)
    vp = path if path.endswith("/value") else path + "/value"
    r.backend.set_driver_param.assert_called_once_with(param, native)
    r.state.on_set.assert_not_called()
    assert r.state.get_value(vp) == canonical
    assert type(r.state.get_value(vp)) is type(canonical)
    assert r.state._dirty[vp] == canonical
    assert r.seen == [(vp, canonical)]
    assert r.replies()[-1]["data"] == canonical
    assert r.replies()[-1]["parameters"]["func_id"] == 17
    assert vp in r.daemon._readback_suppress


@pytest.mark.parametrize("binary", [False, True])
@pytest.mark.parametrize("value,driver_ok,written", [
    ("S/PDIF", False, True),        # the driver refused (EINVAL, no device)
    ("MADI", True, False),          # no known value: never written
    ("WordClock", True, False),     # the old spelling
    (True, True, False),
])
def test_refused_source_changes_nothing(binary, value, driver_ok, written):
    r = Route(binary, ok=driver_ok)
    before = r.snapshot()
    r.set("/ClockSource/value", value, func_id="18")
    assert r.snapshot() == before
    assert r.seen == []
    r.state._schedule_save.assert_not_called()
    r.state.on_set.assert_not_called()
    assert r.backend.set_driver_param.call_count == int(written)
    reply = r.replies()[-1]
    assert reply["error"] == "Unable to set value." and "data" not in reply
    assert reply["parameters"]["func_id"] == 18
    assert "/ClockSource/value" not in r.daemon._readback_suppress


@pytest.mark.parametrize("bad", [True, None, 48000.5, float("nan"),
                                 float("inf"), "48000.5", "-48000",
                                 "9" * 10000, 123])
def test_invalid_rate_never_reaches_the_driver(bad):
    r = Route(False)
    r.set("/SampleRate/value", bad)
    r.backend.set_driver_param.assert_not_called()
    assert r.state.get_value("/SampleRate/value") == 48000
    assert r.replies()[-1]["error"] == "Unable to set value."


def test_ioctl_exception_publishes_nothing(caplog):
    r = Route(True)
    r.backend.set_driver_param.side_effect = OSError("ioctl failed")
    before = r.snapshot()
    r.set("/SampleRate", 96000, func_id="22")
    r.backend.set_driver_param.assert_called_once_with(
        DRIVER_PARAM_SAMPLE_RATE, 96000)
    assert r.snapshot() == before and r.seen == []
    assert r.replies()[-1]["error"] == "Unable to set value."
    assert "write failed" in caplog.text


def test_pipewire_takeover_counts_as_accepted(monkeypatch):
    monkeypatch.setattr(hardware, "pipewire_force_rate",
                        lambda rate, device=None: True)
    r = Route(False)
    r.set("/SampleRate/value", 96000)
    r.backend.set_driver_param.assert_not_called()
    assert r.state.get_value("/SampleRate/value") == 96000


def test_missing_control_is_unresolved():
    r = Route(False)
    del r.state.root["properties"]["ClockSource"]
    r.state._props.pop("/ClockSource", None)
    r.set("/ClockSource", "ADAT", func_id="21")
    r.backend.set_driver_param.assert_not_called()
    reply = r.replies()[-1]
    assert reply["error"] == "Unable to resolve path for set."
    assert reply["parameters"]["func_id"] == 21


def test_read_only_helper_property_is_not_written():
    r = Route(True)
    r.client.helper = True
    r.helper.root["properties"]["ClockSource"] = {"read_only": True,
                                                  "value": "Internal"}
    r.set("/ClockSource", "ADAT")
    r.backend.set_driver_param.assert_not_called()
    assert r.state.get_value("/ClockSource/value") == "Internal"


def test_device_menu_name_is_accepted():
    r = Route(False)
    r.router.clock_sources = {"Internal": 0x0C, "AES/EBU": 0x00}
    r.state.update_property("/ClockSource",
                            {"values": ["Internal", "AES/EBU"]})
    r.set("/ClockSource", "AES/EBU")
    r.backend.set_driver_param.assert_called_once_with(
        DRIVER_PARAM_CLOCK_SOURCE, 0x00)
    assert r.state.get_value("/ClockSource/value") == "AES/EBU"


def test_software_only_validates_and_publishes():
    r = Route(False, software=True)
    r.set("/ClockSource", "ADAT")
    assert r.state.get_value("/ClockSource/value") == "ADAT"
    r.set("/SampleRate", 48000.5)
    assert r.state.get_value("/SampleRate/value") == 48000
    assert r.replies()[-1]["error"] == "Unable to set value."
    r.backend.set_driver_param.assert_not_called()


def test_other_paths_keep_the_normal_route():
    r = Route(False)
    r.state.add_runtime_property("/PostFaderMetering",
                                 {"type": "bool", "value": False})
    r.set("/PostFaderMetering/value", True)
    r.state.on_set.assert_called_once_with("/PostFaderMetering/value", True)


# ── WS: the same transaction ────────────────────────────────────────

def test_ws_refused_clock_change_is_not_published():
    r = Route(False, ok=False)
    ws = WsServer(r.state, r.router)
    client = Mock(id="ws-1")
    out = ws._dispatch(client, parse_ws_command("set /ClockSource/value S/PDIF"))
    assert json.loads(out[0])["error"] == "Unable to set value."
    assert r.state.get_value("/ClockSource/value") == "Internal"


def test_ws_accepted_clock_change_is_written_once():
    r = Route(False)
    ws = WsServer(r.state, r.router)
    assert ws._dispatch(Mock(id="ws-1"),
                        parse_ws_command("set /SampleRate/value 96000")) == []
    r.backend.set_driver_param.assert_called_once_with(
        DRIVER_PARAM_SAMPLE_RATE, 96000)
    r.state.on_set.assert_not_called()
    assert r.state.get_value("/SampleRate/value") == 96000


# ── A SET to the value already in effect does not touch the driver ──

SAME = [("/SampleRate/value", 48000, 96000),
        ("/ClockSource/value", "Internal", "S/PDIF"),
        ("/ClockSource", "Internal", "ADAT")]


@pytest.mark.parametrize("binary", [False, True])
@pytest.mark.parametrize("path,same,other", SAME)
def test_tcp_same_value_is_echoed_not_written(binary, path, same, other):
    r = Route(binary)
    before = r.snapshot()
    r.set(path, same, func_id="21")
    r.backend.set_driver_param.assert_not_called()
    assert r.snapshot() == before and r.seen == []
    reply = r.replies()[-1]
    assert reply["data"] == same and reply["parameters"]["func_id"] == 21
    r.set(path, other)
    assert r.backend.set_driver_param.call_count == 1


@pytest.mark.parametrize("path,same,other", SAME)
def test_ws_same_value_is_not_written(path, same, other):
    r = Route(False)
    ws = WsServer(r.state, r.router)
    client = Mock(id="ws-1")
    assert ws._dispatch(client, parse_ws_command(f"set {path} {same}")) == []
    r.backend.set_driver_param.assert_not_called()
    assert r.seen == []
    ws._dispatch(client, parse_ws_command(f"set {path} {other}"))
    assert r.backend.set_driver_param.call_count == 1


def test_driver_value_wins_over_a_stale_tree():
    # The tree says 48000 but the driver already runs at 96000: no write,
    # and the tree catches up
    r = Route(False)
    r.backend.driver_values[DRIVER_PARAM_SAMPLE_RATE] = 96000
    r.set("/SampleRate/value", 96000)
    r.backend.set_driver_param.assert_not_called()
    assert r.state.get_value("/SampleRate/value") == 96000
    # and the tree's 48000 is not trusted over the driver: a SET back to it
    # is a real change
    r.set("/SampleRate/value", 48000)
    r.backend.set_driver_param.assert_called_once_with(
        DRIVER_PARAM_SAMPLE_RATE, 48000)


def test_tree_value_does_not_hide_a_driver_difference():
    r = Route(False)
    r.backend.driver_values[DRIVER_PARAM_CLOCK_SOURCE] = 0x00     # S/PDIF
    r.set("/ClockSource/value", "Internal")
    r.backend.set_driver_param.assert_called_once_with(
        DRIVER_PARAM_CLOCK_SOURCE, 0x0C)


# ── Primitives ──────────────────────────────────────────────────────

def test_normalize_global():
    assert normalize_global("/devices/0/SampleRate/value", 48000) is None
    assert normalize_global("/SampleRate/value", "44100").value == 44100
    for name in CLOCK_SOURCE_MAP:
        assert normalize_global("/ClockSource/value", name).accepted
    assert not normalize_global("/ClockSource/value", "AES/EBU").accepted
    assert normalize_global("/ClockSource/value", "AES/EBU",
                            {"AES/EBU"}).accepted


def test_driver_sync_never_writes_back():
    tree = StateTree()
    for name, prop in PROPS.items():
        tree.add_runtime_property("/" + name, copy.deepcopy(prop))
    writes = []
    tree.on_set = lambda *a: writes.append(a)
    be = FakeBackend()
    be.driver_values = {DRIVER_PARAM_SAMPLE_RATE: 96000,
                        DRIVER_PARAM_CLOCK_SOURCE: 0x0C}
    HardwareRouter(be, tree)
    assert tree.get_value("/SampleRate/value") == 96000
    assert tree.get_value("/ClockSource/value") == "Internal"
    assert writes == [] and be.driver_params == []


def test_published_value_decides_when_the_driver_does_not_answer():
    """Without a driver answer the caller's published value decides."""
    r = Route(False)
    r.backend.get_driver_param = Mock(side_effect=OSError("no answer"))
    assert r.router._clock_already_set("/SampleRate/value", 48000, 48000)
    assert not r.router._clock_already_set("/SampleRate/value", 96000, 48000)
    assert r.router._clock_already_set("/ClockSource/value", "Internal",
                                       "Internal")
    assert not r.router._clock_already_set("/ClockSource/value", "Internal")
