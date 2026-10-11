"""State tree: thread-safe saves and internal change observers.

Ported from the Codex daemon (test_session_checkpoint.py,
test_session_dirty.py): the debounced save runs on a Timer thread while a
checkpoint (save_now) runs on the event loop; observers see changes without
being wire clients.
"""

import json
import threading
from unittest.mock import patch

import pytest

from state_tree import StateTree

GAIN = "/devices/0/inputs/0/preamps/0/Gain/value"
FADER = "/devices/0/inputs/0/FaderLevel/value"


@pytest.fixture
def saved_tree(tree, tmp_path):
    path = tmp_path / "state.json"
    tree.enable_persistence(path)
    tree._save_delay = 3600         # no timer fires during a test
    yield tree, path
    if tree._save_timer is not None:
        tree._save_timer.cancel()


def test_save_now_waits_for_a_save_in_progress(saved_tree):
    tree, path = saved_tree
    tree.set(GAIN, 32.0)
    tree._save_timer.cancel()
    entered, release = threading.Event(), threading.Event()
    real_dump = json.dump

    def slow_dump(*args, **kwargs):
        entered.set()
        assert release.wait(3), "test write never released"
        return real_dump(*args, **kwargs)

    with patch("state_tree.json.dump", side_effect=slow_dump), \
            patch("state_tree.log.error") as errors:
        pending = threading.Thread(target=tree._save_state)
        pending.start()
        assert entered.wait(3)
        checkpoint = threading.Thread(target=tree.save_now)
        checkpoint.start()
        release.set()
        pending.join(3)
        checkpoint.join(3)
        assert not pending.is_alive() and not checkpoint.is_alive()
        errors.assert_not_called()      # no lost rename of a shared .tmp
    assert json.loads(path.read_text()) == {GAIN: 32.0}
    assert not path.with_suffix(".tmp").exists()


def test_failed_write_keeps_the_previous_file(saved_tree, caplog):
    tree, path = saved_tree
    tree.set(GAIN, 32.0)
    tree.save_now()
    before = path.read_bytes()
    tree.set(GAIN, 40.0)
    with patch("state_tree.Path.rename", side_effect=OSError("disk full")):
        tree.save_now()
    assert path.read_bytes() == before
    assert "disk full" in caplog.text


def test_save_encodes_a_snapshot(saved_tree):
    """A change on the event loop while the Timer thread encodes must not
    break the save (json.dump walks the dict in Python)."""
    tree, path = saved_tree
    tree.set(GAIN, 32.0)
    real_dump = json.dump

    def mutating_dump(obj, *args, **kwargs):
        tree._dirty[FADER] = -6.0           # what the event loop may do
        return real_dump(obj, *args, **kwargs)

    with patch("state_tree.json.dump", side_effect=mutating_dump):
        tree.save_now()
    assert json.loads(path.read_text()) == {GAIN: 32.0}


def test_observer_sees_changes_without_a_subscription(tree):
    seen = []
    tree.register_change_observer("obs", lambda p, v: seen.append((p, v)))
    tree.set(FADER, -4.0, source_client="obs")    # exclusion is for clients
    tree.add_runtime_property("/devices/0/inputs/97/FaderLevel",
                              {"type": "float", "value": 0.0})
    tree.set("/devices/0/inputs/97/FaderLevel/value", -1.0)
    tree.set(FADER, -4.0)                         # unchanged: no notification
    assert seen == [(FADER, -4.0),
                    ("/devices/0/inputs/97/FaderLevel/value", -1.0)]
    tree.unsubscribe_all("obs")
    tree.unregister_callback("obs")
    tree.set(FADER, -5.0)
    assert seen[-1] == (FADER, -5.0)
    tree.unregister_change_observer("obs")
    tree.set(FADER, -6.0)
    assert seen[-1] == (FADER, -5.0)


def test_failing_observer_keeps_client_delivery(tree, caplog):
    got = []
    tree.register_callback("client", lambda p, v: got.append((p, v)))
    tree.subscribe("client", FADER)

    def broken(path, value):
        raise RuntimeError("observer fixture failure")

    tree.register_change_observer("broken", broken)
    tree.set(FADER, -4.0)
    assert got == [(FADER, -4.0)]
    assert "observer fixture failure" in caplog.text


def test_hardware_false_still_notifies_observers(tree):
    writes, seen = [], []
    tree.on_set = lambda *a: writes.append(a)
    tree.register_change_observer("obs", lambda p, v: seen.append(p))
    tree.set(FADER, -3.0, hardware=False)
    assert writes == [] and seen == [FADER]


def test_new_tree_has_no_observers():
    assert StateTree()._change_observers == {}
