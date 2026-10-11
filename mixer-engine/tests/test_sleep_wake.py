"""H6: system sleep / wake.  The driver posts UA_EVT_PAUSED / UA_EVT_RESUMED
(arg UA_PAUSE_DEVICE) around suspend; the daemon stops its hardware writes
and, on wake, pushes the tree again (init + A6 re-apply) keeping the
hardware's own preamp and monitor values."""

import pytest

import events
from conftest import make_readback
from events import (UA_EVT_PAUSED, UA_EVT_RESUMED, UA_PAUSE_DEVICE,
                    UA_PAUSE_IDLE, UA_PAUSE_RATE, EVENT_NAMES)
from hardware import HardwareRouter, SUB_PARAM_CUE1
from ua_mixer_daemon import MixerDaemon
from test_events import EventBackend, FifoNode, attach  # noqa: F401

IN = "/devices/0/inputs"


@pytest.fixture
def node(tmp_path):
    n = FifoNode(tmp_path)
    yield n
    n.close()


@pytest.fixture
def ebackend(node):
    return EventBackend(node.path)


@pytest.fixture
def edaemon(ebackend, tree):
    d = MixerDaemon(tree, HardwareRouter(ebackend, tree))
    d._hw_init_done = True
    yield d
    d._events_detach()


def fader(tree, ch, db):
    tree.set(f"{IN}/{ch}/FaderLevel/value", db)


class TestCodes:
    def test_codes_match_the_driver(self):
        assert UA_EVT_PAUSED == 0x1005
        assert UA_EVT_RESUMED == 0x1006
        assert UA_PAUSE_DEVICE == 4
        assert EVENT_NAMES[UA_EVT_PAUSED] == "PAUSED"
        assert EVENT_NAMES[UA_EVT_RESUMED] == "RESUMED"


class TestPause:
    def test_paused_stops_hardware_writes(self, edaemon, ebackend, node, tree):
        attach(edaemon)
        node.put((UA_EVT_PAUSED, UA_PAUSE_DEVICE))
        edaemon._on_events_readable()
        assert edaemon.hw_paused
        ebackend.clear()
        fader(tree, 3, -6.0)
        tree.set(f"{IN}/3/Mute/value", True)
        assert ebackend.mixer_params == []
        assert ebackend.bus_params == []
        # ... while the tree keeps the change
        assert tree.get_value(f"{IN}/3/FaderLevel/value") == -6.0
        assert tree.get_value(f"{IN}/3/Mute/value") is True

    def test_other_pause_reasons_do_not_stop_writes(self, edaemon, ebackend,
                                                    node, tree):
        attach(edaemon)
        node.put((UA_EVT_PAUSED, UA_PAUSE_RATE), (UA_EVT_PAUSED, UA_PAUSE_IDLE))
        edaemon._on_events_readable()
        assert not edaemon.hw_paused
        ebackend.clear()
        fader(tree, 3, -6.0)
        assert ebackend.bus_params or ebackend.bus_params_r

    def test_router_and_backend_flags(self, edaemon, ebackend):
        edaemon._on_driver_event(UA_EVT_PAUSED, UA_PAUSE_DEVICE)
        assert edaemon.hw_router.paused and ebackend.paused
        edaemon._on_driver_event(UA_EVT_RESUMED, UA_PAUSE_DEVICE)
        assert not edaemon.hw_router.paused and not ebackend.paused

    def test_second_paused_is_ignored(self, edaemon):
        edaemon._on_driver_event(UA_EVT_PAUSED, UA_PAUSE_DEVICE)
        edaemon._on_driver_event(UA_EVT_PAUSED, UA_PAUSE_DEVICE)
        assert edaemon.hw_paused

    def test_device_lost_clears_the_pause(self, edaemon, ebackend):
        edaemon._on_driver_event(UA_EVT_PAUSED, UA_PAUSE_DEVICE)
        edaemon._device_lost(0.0)
        assert not edaemon.hw_paused and not ebackend.paused


class TestResume:
    def test_resumed_repushes_the_tree(self, edaemon, ebackend, node, tree):
        attach(edaemon)
        fader(tree, 3, -6.0)
        node.put((UA_EVT_PAUSED, UA_PAUSE_DEVICE))
        edaemon._on_events_readable()
        ebackend.clear()
        # a change while asleep reaches the tree only
        fader(tree, 4, -12.0)
        assert ebackend.mixer_params == [] and not ebackend.bus_params
        node.put((UA_EVT_RESUMED, UA_PAUSE_DEVICE))
        edaemon._on_events_readable()
        assert not edaemon.hw_paused
        # the init sequence ran (route + level + monitor init) ...
        assert (1, 0, 0x13, 0x0001FFFF) in ebackend.mixer_params
        assert any(p[0] == 2 for p in ebackend.mixer_params)
        # ... and the saved faders are back on the DSP buses
        assert ebackend.bus_params or ebackend.bus_params_r

    def test_resumed_keeps_the_hardware_preamp_and_monitor(self, edaemon,
                                                           ebackend, tree):
        ebackend.readback = (1, make_readback(mon_raw=150,
                                              flags={0: {"48v"}},
                                              gains_c=[40, 0, 0, 0]))
        # the tree disagrees (a client set gain 20 and 48V off while asleep)
        edaemon._on_driver_event(UA_EVT_PAUSED, UA_PAUSE_DEVICE)
        tree.set(f"{IN}/0/preamps/0/Gain/value", 20.0)
        tree.set(f"{IN}/0/preamps/0/48V/value", False)
        ebackend.clear()
        edaemon._on_driver_event(UA_EVT_RESUMED, UA_PAUSE_DEVICE)
        # 48V (param 0x03) is what the readback says, not the tree
        v48 = [p for p in ebackend.mixer_params
               if p[0] == 1 and p[1] == 0 and p[2] == 0x03]
        assert v48 and v48[-1][3] == 1
        # monitor volume is the readback's raw level
        vol = [p for p in ebackend.mixer_params
               if p[0] == 2 and p[2] == 0x01]
        assert vol and vol[-1][3] == 150

    def test_resumed_reapplies_the_saved_sends(self, edaemon, ebackend, tree):
        tree.set(f"{IN}/0/sends/2/Gain/value", -10.0)      # CUE 1
        edaemon._on_driver_event(UA_EVT_PAUSED, UA_PAUSE_DEVICE)
        ebackend.clear()
        edaemon._on_driver_event(UA_EVT_RESUMED, UA_PAUSE_DEVICE)
        # A6: the send level is written again
        assert any(b == 0x00 and sub == SUB_PARAM_CUE1 and val != 0.0
                   for b, sub, val in ebackend.bus_params)

    def test_resumed_without_pause_still_pushes(self, edaemon, ebackend):
        ebackend.clear()
        edaemon._on_driver_event(UA_EVT_RESUMED, UA_PAUSE_DEVICE)
        assert (1, 0, 0x13, 0x0001FFFF) in ebackend.mixer_params

    def test_other_resume_reasons_do_nothing(self, edaemon, ebackend):
        edaemon._on_driver_event(UA_EVT_PAUSED, UA_PAUSE_DEVICE)
        ebackend.clear()
        edaemon._on_driver_event(UA_EVT_RESUMED, UA_PAUSE_RATE)
        assert edaemon.hw_paused
        assert ebackend.mixer_params == []

    def test_resumed_while_offline_leaves_the_push_to_the_watchdog(
            self, edaemon, ebackend):
        edaemon._on_driver_event(UA_EVT_PAUSED, UA_PAUSE_DEVICE)
        edaemon._hw_online = False
        ebackend.clear()
        edaemon._on_driver_event(UA_EVT_RESUMED, UA_PAUSE_DEVICE)
        assert not edaemon.hw_paused
        assert ebackend.mixer_params == []
