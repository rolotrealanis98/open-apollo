"""Client SETs the engine refuses: a read-only value ("uasrv.read_only") and
a path the tree does not have ("Unable to resolve path for set."), with the
tree untouched; the daemon's own writers (state.set) are not subject to it.
`readonly` is published on the map's read-only patterns."""

import json
from types import SimpleNamespace

import pytest

from conftest import ENGINE_DIR
from protocol import Command
from state_tree import READONLY_PATTERNS
from test_console_gain import connection, decode
from ua_mixer_daemon import apply_runtime_properties

FUNC = {"func_id": "7"}
READONLY = ["/TotalDSPLoad/value", "/devices/0/inputs/0/Active/value",
            "/devices/0/outputs/0/Active/value", "/devices/0/CueBusCount/value",
            "/devices/0/SupportsDante/value"]


@pytest.mark.parametrize("binary", [True, False])
@pytest.mark.parametrize("path", READONLY)
def test_read_only_set_is_refused_and_changes_nothing(path, binary):
    daemon, state, client, writer, writes = connection(binary, subscribe=())
    apply_runtime_properties(state)
    writes.clear()
    before = state.get_value(path)
    pushed = []
    state.register_callback("watch", lambda p, v: pushed.append(p))
    state.subscribe("watch", path)
    value = (not before) if isinstance(before, bool) else 55
    daemon._handle_set(client, Command("set", path, value, raw_params=FUNC))
    assert decode(writer.frames, binary) == [
        {"path": path, "error": "uasrv.read_only",
         "parameters": {"func_id": 7}}]
    assert state.get_value(path) == before
    assert writes == [] and pushed == []


@pytest.mark.parametrize("binary", [True, False])
def test_unknown_path_is_refused_and_creates_nothing(binary):
    daemon, state, client, writer, writes = connection(binary, subscribe=())
    path = "/devices/0/inputs/0/NoSuchProp/value"
    daemon._handle_set(client, Command("set", path, 1, raw_params=FUNC))
    assert decode(writer.frames, binary) == [
        {"path": path, "error": "Unable to resolve path for set.",
         "parameters": {"func_id": 7}}]
    assert not state.path_exists(path) and writes == []


def test_writable_set_still_echoes():
    daemon, state, client, writer, writes = connection(False, subscribe=())
    path = "/devices/0/inputs/0/Mute/value"
    daemon._handle_set(client, Command("set", path, True, raw_params=FUNC))
    assert decode(writer.frames, False)[-1] == {
        "path": path, "data": True, "parameters": {"func_id": 7}}
    assert state.get_value(path) is True


def test_internal_writers_may_set_a_read_only_value():
    _, state, *_ = connection(False, subscribe=())
    seen = []
    state.register_callback("watch", lambda p, v: seen.append((p, v)))
    state.subscribe("watch", "/devices/0/inputs/0/Active/value")
    old = state.get_value("/devices/0/inputs/0/Active/value")
    assert state.is_readonly("/devices/0/inputs/0/Active/value")
    assert state.set("/devices/0/inputs/0/Active/value", not old,
                     hardware=False)
    assert state.get_value("/devices/0/inputs/0/Active/value") is (not old)
    assert seen


def test_readonly_is_published_on_the_read_only_patterns():
    _, state, *_ = connection(False, subscribe=())
    apply_runtime_properties(state)
    for path in READONLY:
        assert state.get(path.removesuffix("/value"))["readonly"] is True, path
    assert "readonly" not in state.get("/devices/0/inputs/0/Mute")


def test_pattern_list_is_names_only():
    doc = json.loads(READONLY_PATTERNS.read_text())
    assert len(doc["readonly"]) == 60
    assert all(isinstance(p, str) and p.startswith("/") for p in doc["readonly"])
    assert set(doc) <= {"_comment", "readonly"}


def test_websocket_set_refuses_both():
    from ws_protocol import WsCommand
    from ws_server import WsServer
    _, state, *_ = connection(False, subscribe=())
    ws = WsServer(state, None)
    for path, error in (("/devices/0/inputs/0/Active/value", "uasrv.read_only"),
                        ("/devices/0/inputs/0/NoSuchProp/value",
                         "Unable to resolve path for set.")):
        out = ws._handle_set(SimpleNamespace(id="c"), WsCommand(
            verb="set", path=path, value=1, message_id="m1"))
        assert len(out) == 1 and error in out[0]
    assert not state.path_exists("/devices/0/inputs/0/NoSuchProp/value")
