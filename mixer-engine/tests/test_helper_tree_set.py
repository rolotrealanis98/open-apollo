"""Mixer Helper tree writes and notifications (ported from the Codex daemon,
d418807 helper_tree.py and _push_to_clients)."""

from conftest import FakeBackend
from hardware import HardwareRouter
from helper_tree import HelperTree
from ua_mixer_daemon import MixerDaemon


def helper():
    h = HelperTree()
    h.root = {
        "properties": {"initialized": {"read_only": True, "value": True},
                       "Dirty": {"value": False}},
        "children": [{"path": "status", "properties": {
            "timeout_ms": {"value": 5000}}}]}
    return h


def test_root_property_honours_metadata():
    h = helper()
    assert h.set_value("/initialized", False) == "read_only"
    assert h.get_value("/initialized") is True
    assert h.set_value("/Dirty/value", True) == "ok"
    assert h.get_value("/Dirty") is True
    assert h.set_value("/nothing", 1) == "not_found"


def test_nested_property_with_and_without_value_suffix():
    h = helper()
    assert h.set_value("/status/timeout_ms/value", 10) == "ok"
    assert h.get_value("/status/timeout_ms") == 10
    assert h.set_value("/status/timeout_ms", 11) == "ok"
    assert h.get_value("/status/timeout_ms/value") == 11
    assert h.set_value("/status/missing", 1) == "not_found"


def test_failing_subscriber_does_not_stop_the_others(caplog):
    h = helper()
    got = []

    def broken(path, value):
        raise RuntimeError("fixture failure")

    h.register_callback("a", broken)
    h.register_callback("b", lambda p, v: got.append((p, v)))
    h.subscribe("a", "/status")
    h.subscribe("b", "/status")
    h.set_value("/status/timeout_ms", 7)
    assert got == [("/status/timeout_ms", 7)]
    assert "fixture failure" in caplog.text


def test_readback_push_notifies_helper_subscribers_once(tree):
    h = helper()
    d = MixerDaemon(tree, HardwareRouter(FakeBackend(), tree), helper_tree=h)
    got = []
    h.register_callback("c", lambda p, v: got.append((p, v)))
    h.subscribe("c", "/status/timeout_ms")
    h.subscribe("c", "/devices/0/outputs/18/Mute")
    d._push_to_clients("/status/timeout_ms", 9)            # in the helper tree
    d._push_to_clients("/devices/0/outputs/18/Mute", True)  # state tree only
    assert got == [("/status/timeout_ms", 9),
                   ("/devices/0/outputs/18/Mute", True)]
