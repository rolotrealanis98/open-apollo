"""Gain / GainTapered companions in the negotiated framing (ported from the
Codex daemon's test_console_protocol.py, commit d418807).

A SET of one echoes the SET (state.set) and sends the companion: in UBJSON
to a UBJSON client (it used to go out as JSON text whatever the client
negotiated), kept in the tree, pushed once to the other subscribers, never
written to the hardware a second time, computed from the tree's clamped
value, and only for the exact preamp paths.
"""

import json

import pytest

from conftest import DEVICE_MAP_X4, ENGINE_DIR
from hardware import (monitor_hw_to_tapered, monitor_tapered_to_hw,
                      preamp_db_to_tapered, preamp_tapered_to_db)
from helper_tree import HelperTree
from protocol import Command
from state_tree import StateTree
from ua_mixer_daemon import MixerClient, MixerDaemon
from ubjson_codec import decode_frame

BASE = "/devices/0/inputs/0/preamps/0"
MON = "/devices/0/outputs/18"


class MemoryWriter:
    def __init__(self):
        self.frames = []

    def get_extra_info(self, name):
        return ("127.0.0.1", 4720)

    def write(self, data):
        self.frames.append(data)


def connection(binary, subscribe=("Gain", "GainTapered")):
    state = StateTree()
    state.load_device_map(DEVICE_MAP_X4)
    helper = HelperTree()
    helper.load(ENGINE_DIR / "device_maps" / "helper_tree.json")
    writes = []
    state.on_set = lambda path, value: writes.append((path, value))
    daemon = MixerDaemon(state, None, helper_tree=helper)
    writer = MemoryWriter()
    client = MixerClient(None, writer, ubjson=binary)
    client.ubjson = binary
    state.register_callback(
        client.id, lambda p, v: daemon._send_response(client, p, v))
    for control in subscribe:
        state.subscribe(client.id, f"{BASE}/{control}/value")
    return daemon, state, client, writer, writes


def decode(frames, binary):
    if not binary:
        return [json.loads(f.removesuffix(b"\0")) for f in frames]
    out = []
    for f in frames:
        msg, end = decode_frame(f)
        assert end == len(f)
        out.append(msg)
    return out


@pytest.mark.parametrize("binary", [False, True])
@pytest.mark.parametrize("control,value,companion,expected", [
    ("Gain", 32.0, "GainTapered", preamp_db_to_tapered(32)),
    ("GainTapered", 0.6, "Gain", preamp_tapered_to_db(0.6)),
])
def test_companion_in_the_clients_framing(binary, control, value, companion,
                                          expected):
    daemon, state, client, writer, writes = connection(binary)
    path = f"{BASE}/{control}/value"
    daemon._handle_set(client, Command("set", path, value))
    msgs = decode(writer.frames, binary)
    assert len(msgs) == 2
    assert msgs[-1] == {"path": f"{BASE}/{companion}/value", "data": expected}
    assert state.get_value(f"{BASE}/{companion}/value") == pytest.approx(
        expected)
    assert writes == [(path, value)]


def test_other_subscriber_gets_the_companion_once():
    daemon, state, client, writer, writes = connection(True)
    seen = []
    state.register_callback("observer", lambda p, v: seen.append((p, v)))
    state.subscribe("observer", f"{BASE}/GainTapered/value")
    daemon._handle_set(client, Command("set", f"{BASE}/Gain/value", 32.0))
    assert seen == [(f"{BASE}/GainTapered/value", preamp_db_to_tapered(32))]
    assert len(writes) == 1


def test_clamped_value_drives_the_companion():
    daemon, state, client, writer, writes = connection(True)
    daemon._handle_set(client, Command("set", f"{BASE}/GainTapered/value", 2.0))
    assert state.get_value(f"{BASE}/GainTapered/value") == 1.0
    assert state.get_value(f"{BASE}/Gain/value") == 65.0
    assert decode(writer.frames, True)[-1]["data"] == 65.0


def test_tapered_companion_is_whole_db():
    """The hardware takes whole dB, so the tapered value says 32 dB."""
    daemon, state, client, writer, writes = connection(False)
    daemon._handle_set(client, Command("set", f"{BASE}/Gain/value", 32.4))
    assert state.get_value(f"{BASE}/GainTapered/value") == \
        preamp_db_to_tapered(32)


def test_func_id_ack_keeps_binary_framing():
    daemon, state, client, writer, writes = connection(True)
    path = f"{BASE}/Gain/value"
    daemon._handle_set(client, Command("set", path, 32.0,
                                       raw_params={"func_id": "17"}))
    assert decode(writer.frames, True)[-1] == {
        "path": path, "data": 32.0, "parameters": {"func_id": 17}}


@pytest.mark.parametrize("path", [
    f"{BASE}/GainUnknown/value",
])
def test_no_companion_for_other_gain_paths(path):
    daemon, state, client, writer, writes = connection(True, subscribe=())
    daemon._handle_set(client, Command("set", path, -6.0))
    # a path the tree lacks is refused, with no companion pushed
    assert decode(writer.frames, True) == [
        {"path": path, "error": "Unable to resolve path for set."}]
    assert not any("GainTapered" in p or p.endswith("preamps/0/Gain/value")
                   for p, _ in writes)


def test_send_gain_companion_uses_the_fader_taper():
    """A send's Gain has a companion too, but on the fader taper (sends
    use the fader curve), never the preamp's."""
    from hardware import fader_db_to_tapered
    daemon, state, client, writer, writes = connection(True, subscribe=())
    path = "/devices/0/inputs/0/sends/0/Gain/value"
    daemon._handle_set(client, Command("set", path, -6.0))
    tapered = "/devices/0/inputs/0/sends/0/GainTapered/value"
    assert state.get_value(tapered) == pytest.approx(fader_db_to_tapered(-6.0))
    assert not any(p.endswith("preamps/0/Gain/value") for p, _ in writes)


def test_companion_survives_without_value_suffix():
    daemon, state, client, writer, writes = connection(False)
    daemon._handle_set(client, Command("set", f"{BASE}/Gain", 40.0))
    assert decode(writer.frames, False)[-1] == {
        "path": f"{BASE}/GainTapered/value", "data": preamp_db_to_tapered(40)}


@pytest.mark.parametrize("binary", [False, True])
def test_monitor_knob_publishes_db_without_second_write(binary):
    daemon, state, client, writer, writes = connection(binary, subscribe=())
    state.subscribe(client.id, f"{MON}/CRMonitorLevel/value")
    daemon._handle_set(client, Command(
        "set", f"{MON}/CRMonitorLevelTapered/value", 0.5))
    want = (monitor_tapered_to_hw(0.5) - 192) / 2
    assert state.get_value(f"{MON}/CRMonitorLevel/value") == want
    assert writes == [(f"{MON}/CRMonitorLevelTapered/value", 0.5)]
    assert decode(writer.frames, binary)[-1] == {
        "path": f"{MON}/CRMonitorLevel/value", "data": want}


def test_monitor_companion_from_the_clamped_value():
    daemon, state, client, writer, writes = connection(False, subscribe=())
    daemon._handle_set(client, Command("set", f"{MON}/CRMonitorLevel/value",
                                       12.0))
    assert state.get_value(f"{MON}/CRMonitorLevel/value") == 0.0
    assert state.get_value(f"{MON}/CRMonitorLevelTapered/value") == \
        monitor_hw_to_tapered(192)
