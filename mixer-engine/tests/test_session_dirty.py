"""The session /Dirty flag (ported from the Codex daemon's session_dirty.py,
test_session_dirty.py and test_session_checkpoint.py; clear points from the
Mixer Engine binary, see session_dirty.py)."""

import json

import pytest

from conftest import FakeBackend
from hardware import HardwareRouter
from helper_tree import HelperTree
from protocol import Command
from session_dirty import tracked
from ua_mixer_daemon import MixerClient, MixerDaemon

GAIN = "/devices/0/inputs/0/preamps/0/Gain/value"
FADER = "/devices/0/inputs/0/FaderLevel/value"


class MemoryWriter:
    def __init__(self):
        self.frames = []

    def get_extra_info(self, name):
        return ("127.0.0.1", 4710)

    def write(self, data):
        self.frames.append(data)


@pytest.fixture
def setup(tree, tmp_path):
    writes = []
    tree.on_set = lambda *a: writes.append(a)
    daemon = MixerDaemon(tree, None)
    daemon.commands.set_store(tmp_path, "x4")
    writer = MemoryWriter()
    client = MixerClient(None, writer)
    tree.register_callback(
        client.id, lambda p, v: daemon._send_response(client, p, v))
    tree.subscribe(client.id, "/Dirty/value")
    return daemon, tree, client, writer, writes


def dirty_pushes(writer):
    return [json.loads(f.rstrip(b"\0")) for f in writer.frames
            if b'"/Dirty/value"' in f]


def test_starts_clean_whatever_the_map_says(setup):
    daemon, tree, *_ = setup
    # the x4 capture has Dirty = true; the engine starts clean
    assert tree.get_value("/Dirty/value") is False
    assert "/Dirty/value" not in tree._dirty


def test_set_marks_dirty_once_without_a_hardware_write(setup):
    daemon, tree, client, writer, writes = setup
    daemon._session_ready = True
    daemon._handle_set(client, Command("set", GAIN, 32.0))
    assert dirty_pushes(writer) == [{"path": "/Dirty/value", "data": True}]
    assert writes == [(GAIN, 32.0)]
    daemon._handle_set(client, Command("set", GAIN, 40.0))
    assert len(dirty_pushes(writer)) == 1


def test_dirty_is_never_saved(setup):
    daemon, tree, client, writer, writes = setup
    daemon._session_ready = True
    tree.set(GAIN, 32.0)
    assert tree.get_value("/Dirty/value") is True
    assert "/Dirty/value" not in tree._dirty and GAIN in tree._dirty


def test_not_before_the_session_is_up_nor_suppressed_nor_unchanged(setup):
    daemon, tree, *_ = setup
    tree.set(GAIN, 32.0)
    assert tree.get_value("/Dirty/value") is False
    daemon._session_ready = True
    with daemon.session_dirty.suppress():
        tree.set(GAIN, 40.0)
    tree.set(GAIN, 40.0)                  # unchanged
    assert tree.get_value("/Dirty/value") is False
    tree.set(GAIN, 41.0)
    assert tree.get_value("/Dirty/value") is True


def test_init_complete_opens_the_session(setup):
    daemon, tree, *_ = setup
    daemon._fire_init_complete()
    tree.set(FADER, -3.0)
    assert tree.get_value("/Dirty/value") is True


def test_readback_and_status_pushes_are_not_edits(setup):
    daemon, tree, client, writer, writes = setup
    daemon._session_ready = True
    daemon._push_to_clients(GAIN, 32.0)
    daemon._push_to_clients("/devices/0/inputs/0/IOType", "Line")
    assert tree.get_value(GAIN) == 32.0
    assert tree.get_value("/Dirty/value") is False
    assert writes == []


@pytest.mark.parametrize("path", [
    "/devices/0/outputs/18/Mute/value",            # CMxOutput never tracks
    "/devices/0/inputs/0/preamps/0/Pad/value",     # flags 0
    "/devices/0/inputs/0/sends/0/Gain/value",      # sends: not tracked here
    "/devices/0/outputs/18/CRMonitorLevel/value",
    "/SampleRate/value",
])
def test_untracked_changes(setup, path):
    daemon, tree, *_ = setup
    daemon._session_ready = True
    cur = tree.get_value(path)
    tree.set(path, (not cur) if isinstance(cur, bool) else
             (96000 if path == "/SampleRate/value" else -12.0))
    assert tree.get_value("/Dirty/value") is False


@pytest.mark.parametrize("path", [
    GAIN, FADER, "/devices/0/inputs/0/preamps/0/48V/value",
    "/devices/0/inputs/0/preamps/0/LowCut/value",
    "/devices/0/inputs/0/preamps/0/Phase/value",
    "/devices/0/inputs/0/Mute/value", "/devices/0/inputs/0/Solo/value",
    "/devices/0/inputs/0/Pan/value", "/devices/0/inputs/0/Name/value",
    "/devices/0/inputs/0/FaderLevelTapered/value",
    "/devices/0/outputs/0/Name/value",
])
def test_tracked_paths(path):
    assert tracked(path)


def test_late_added_control_tracks_without_a_subscription(setup):
    daemon, tree, client, writer, writes = setup
    late = "/devices/0/inputs/97/FaderLevel/value"
    tree.add_runtime_property(late[:-6], {"type": "float", "value": 0.0})
    daemon._session_ready = True
    tree.unsubscribe_all(daemon.session_dirty.OBSERVER)
    tree.unregister_callback(daemon.session_dirty.OBSERVER)
    tree.set(late, -3.0, source_client=daemon.session_dirty.OBSERVER)
    assert tree.get_value("/Dirty/value") is True
    assert writes == [(late, -3.0)]


def test_client_can_set_dirty(setup):
    """CMxRoot::SetProperty writes the flag for kPropDirty (0xba)."""
    daemon, tree, client, writer, writes = setup
    daemon._session_ready = True
    tree.set(GAIN, 32.0)
    other = MixerClient(None, MemoryWriter())
    daemon._handle_set(other, Command("set", "/Dirty/value", False))
    assert tree.get_value("/Dirty/value") is False
    assert dirty_pushes(writer)[-1] == {"path": "/Dirty/value", "data": False}
    assert "/Dirty/value" not in tree._dirty
    # the router takes /Dirty as software-only: nothing reaches a device
    router = HardwareRouter(FakeBackend(), tree)
    router.on_set("/Dirty/value", True)
    assert router.backend.mixer_params == [] and \
        router.backend.driver_params == [] and router.backend.other == []


@pytest.mark.parametrize("post", [
    "post /NewSession", 'post /SaveSessionAs {"name": "a"}',
])
def test_session_commands_clear(setup, post):
    daemon, tree, client, writer, writes = setup
    daemon._session_ready = True
    tree.set(FADER, -3.0)
    assert tree.get_value("/Dirty/value") is True
    daemon._dispatch(client, post)
    assert tree.get_value("/Dirty/value") is False


def test_open_and_save_clear(setup):
    daemon, tree, client, writer, writes = setup
    daemon._session_ready = True
    daemon._dispatch(client, 'post /SaveSessionAs {"name": "a"}')
    tree.set(FADER, -7.0)
    assert tree.get_value("/Dirty/value") is True
    daemon._dispatch(client, "post /SaveSession")
    assert tree.get_value("/Dirty/value") is False
    tree.set(FADER, -9.0)
    daemon._dispatch(client, 'post /OpenSession {"name": "a"}')
    assert tree.get_value("/Dirty/value") is False
    assert tree.get_value(FADER) == -7.0


def test_snapshot_recall_clears(setup):
    daemon, tree, client, writer, writes = setup
    daemon._session_ready = True
    daemon._dispatch(client, 'post /SaveAsSnapshot {"name": "s"}')
    tree.set(FADER, -5.0)
    assert tree.get_value("/Dirty/value") is True
    daemon._handle_set(client, Command("set", "/Snapshot/value", "s"))
    assert tree.get_value("/Dirty/value") is False


def test_save_last_used_checkpoints_and_keeps_dirty(setup, tmp_path):
    daemon, tree, client, writer, writes = setup
    state_file = tmp_path / "x4.state.json"
    tree.enable_persistence(state_file)
    tree._save_delay = 3600
    daemon._session_ready = True
    tree.set(GAIN, 32.0)
    daemon._dispatch(client, "post /SaveLastUsedSession?func_id=2842 {}")
    assert json.loads(state_file.read_text())[GAIN] == 32.0
    assert tree._save_timer is None
    assert tree.get_value("/Dirty/value") is True


def test_helper_edit_mirrors_into_the_state_tree(tree):
    """A control both trees hold: 4710 subscribers, the saved state and
    /Dirty see a 4720 edit, and the hardware is written once."""
    helper = HelperTree()
    helper.root = {"properties": {}, "children": [{"path": "status",
                   "properties": {"timeout_ms": {"value": 5000}}}]}
    tree.add_runtime_property("/status/timeout_ms",
                              {"type": "int", "value": 5000})
    backend = FakeBackend()
    router = HardwareRouter(backend, tree)
    daemon = MixerDaemon(tree, router, helper_tree=helper)
    hw = []
    router.on_set = lambda p, v: hw.append((p, v))
    tree.on_set = router.on_set
    client = MixerClient(None, MemoryWriter(), ubjson=True)
    seen = []
    tree.register_callback("obs", lambda p, v: seen.append((p, v)))
    tree.subscribe("obs", "/status/timeout_ms/value")
    daemon._handle_set(client, Command("set", "/status/timeout_ms", 7000))
    assert tree.get_value("/status/timeout_ms/value") == 7000
    assert helper.get_value("/status/timeout_ms") == 7000
    assert seen == [("/status/timeout_ms/value", 7000)]
    assert hw == [("/status/timeout_ms/value", 7000)]
    assert tree._dirty["/status/timeout_ms/value"] == 7000


def test_a_load_does_not_blip_dirty(setup):
    """The sets of a recall or an open are not edits: a clean session stays
    clean without a true/false pair on the wire."""
    daemon, tree, client, writer, writes = setup
    daemon._session_ready = True
    daemon._dispatch(client, 'post /SaveAsSnapshot {"name": "s"}')
    daemon._dispatch(client, 'post /SaveSessionAs {"name": "a"}')
    tree.set(FADER, -5.0)
    daemon._dispatch(client, "post /SaveSession")
    tree.set(FADER, -8.0)
    daemon._dispatch(client, "post /SaveSession")
    writer.frames.clear()
    daemon._handle_set(client, Command("set", "/Snapshot/value", "s"))
    daemon._dispatch(client, 'post /OpenSession {"name": "a"}')
    daemon._dispatch(client, "post /NewSession")
    assert dirty_pushes(writer) == []
    assert tree.get_value("/Dirty/value") is False
