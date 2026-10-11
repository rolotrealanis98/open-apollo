"""MixerDaemon._sync_hw_readback: readback -> state tree, per device map."""

import time

import pytest

from conftest import ENGINE_DIR, FakeBackend, make_readback
from hardware import HardwareRouter
from state_tree import StateTree
from ua_mixer_daemon import MixerDaemon

X8P = ENGINE_DIR / "device_maps" / "device_map_apollo_x8p.json"
OUT = "/devices/0/outputs"
IN = "/devices/0/inputs"


@pytest.fixture
def daemon(router, tree):
    return MixerDaemon(tree, router)


def sync(daemon, backend, last=None, **kw):
    backend.readback = (1, make_readback(**kw))
    rb = daemon.hw_router.poll_hw_readback()
    daemon._sync_hw_readback(rb, last if last is not None else {})
    return rb


def val(tree, path):
    return tree.get_value(path + "/value")


def selected(tree, outs=(14, 15, 16, 17, 18), ins=(0, 1, 2, 3)):
    return ({o for o in outs if val(tree, f"{OUT}/{o}/SelectedOnFront")},
            {i for i in ins if val(tree, f"{IN}/{i}/SelectedOnFront")})


class TestOutputIndexes:
    def test_x4_monitor_and_headphones(self, daemon, backend, tree):
        pushed = {}
        push = daemon._push_to_clients

        def record(path, value):
            pushed[path] = value
            push(path, value)
        daemon._push_to_clients = record
        sync(daemon, backend, mon_raw=172, hp1_raw=150, hp2_raw=120, mute=True)
        assert val(tree, f"{OUT}/18/CRMonitorLevel") == -10.0
        assert val(tree, f"{OUT}/18/Mute") is True
        # the HP levels have no state-tree control; they reach helper clients
        assert pushed[f"{OUT}/19/CRMonitorLevelTapered"] > 0.0
        assert pushed[f"{OUT}/20/CRMonitorLevelTapered"] > 0.0

    def test_x8p_monitor_is_output_22(self):
        tree = StateTree()
        tree.load_device_map(X8P)
        backend = FakeBackend()
        router = HardwareRouter(backend, tree)
        d = MixerDaemon(tree, router)
        cue1_before = val(tree, f"{OUT}/18/Mute")
        sync(d, backend, mon_raw=172, mute=True, mono=True)
        assert val(tree, f"{OUT}/22/CRMonitorLevel") == -10.0
        assert val(tree, f"{OUT}/22/Mute") is True
        assert val(tree, f"{OUT}/22/MixToMono") is True
        # output 18 is CUE 1 on the x8p
        assert val(tree, f"{OUT}/18/Mute") == cue1_before
        assert val(tree, f"{OUT}/18/CRMonitorLevel") is None


class TestFrontPanel:
    def test_focus_on_monitor(self, daemon, backend, tree):
        sync(daemon, backend, focus="monitor", sel_input=2)
        assert selected(tree) == ({18}, set())

    def test_focus_on_headphones_is_cue_1(self, daemon, backend, tree):
        sync(daemon, backend, focus="headphone")
        assert selected(tree) == ({14}, set())

    def test_focus_on_headphone_2_is_cue_2(self, daemon, backend, tree):
        # x4, titan 2026-10-11: word 5 bit 30 while the HP2 knob is selected
        cue2 = daemon.hw_router.output_index("cue", 1)
        assert cue2 is not None
        rb = sync(daemon, backend, focus="headphone")
        sync(daemon, backend, last=rb, focus="headphone2")
        assert selected(tree) == ({cue2}, set())
        rb2 = daemon.hw_router.poll_hw_readback()
        sync(daemon, backend, last=rb2, focus="headphone")
        assert selected(tree) == ({14}, set())

    def test_no_focus_selects_the_input(self, daemon, backend, tree):
        sync(daemon, backend, sel_input=2)
        assert selected(tree) == (set(), {2})

    def test_moving_from_input_to_monitor(self, daemon, backend, tree):
        rb = sync(daemon, backend, sel_input=1)
        sync(daemon, backend, last=rb, focus="monitor", sel_input=1)
        assert selected(tree) == ({18}, set())

    def test_moving_from_headphones_to_an_input(self, daemon, backend, tree):
        rb = sync(daemon, backend, focus="headphone", sel_input=0)
        sync(daemon, backend, last=rb, sel_input=3)
        assert selected(tree) == (set(), {3})

    def test_input_change_alone_reselects_inputs(self, daemon, backend, tree):
        # The Mixer Engine applies a word 3 [30:28] change even while word
        # 0 (and so the output focus) stays the same
        rb = sync(daemon, backend, sel_input=0)
        sync(daemon, backend, last=rb, sel_input=3)
        assert selected(tree) == (set(), {3})

    def test_other_word0_change_keeps_selection(self, daemon, backend, tree):
        rb = sync(daemon, backend, sel_input=2)
        sync(daemon, backend, last=rb, sel_input=2, flags={0: {"48v"}})
        assert selected(tree) == (set(), {2})

    def test_unchanged_readback_pushes_nothing(self, daemon, backend, tree):
        rb = sync(daemon, backend, sel_input=2)
        tree.set_value(f"{IN}/0/SelectedOnFront/value", True)  # a client
        sync(daemon, backend, last=rb, sel_input=2)
        assert val(tree, f"{IN}/0/SelectedOnFront") is True


class TestOtherFields:
    def test_altmon_selection(self, daemon, backend, tree):
        rb = sync(daemon, backend, altmon=0)
        assert val(tree, "/devices/0/AltMonSelection") == 0
        sync(daemon, backend, last=rb, altmon=2)
        assert val(tree, "/devices/0/AltMonSelection") == 2

    def test_altmon_ignored_right_after_a_client_set(self, daemon, backend, tree):
        rb = sync(daemon, backend, altmon=0)
        daemon._readback_suppress["/devices/0/AltMonSelection/value"] = \
            time.monotonic()
        sync(daemon, backend, last=rb, altmon=1)
        assert val(tree, "/devices/0/AltMonSelection") == 0

    def test_link_goes_to_stereo(self, daemon, backend, tree):
        sync(daemon, backend, flags={0: {"link"}})
        assert val(tree, f"{IN}/0/Stereo") is True

    def test_controls_the_x4_map_lacks_are_not_created(self, daemon, backend, tree):
        sync(daemon, backend, temperature=40, low_noise=True, board_id=3)
        for ctrl in ("Temperature", "LowNoisePreamps"):
            assert val(tree, f"/devices/0/{ctrl}") is None

    def test_analog_board_id(self, daemon, backend, tree):
        sync(daemon, backend, board_id=3)
        assert val(tree, "/devices/0/AnalogBoardID") == 3

    def test_present_control_is_pushed(self, daemon, backend, tree):
        tree.add_runtime_property("/devices/0/Temperature",
                                  {"type": "float", "value": 0.0})
        sync(daemon, backend, temperature=40)
        assert val(tree, "/devices/0/Temperature") == 40.0


class TestInitRetry:
    def test_dropped_snapshot_is_not_a_reset(self, daemon, backend):
        bad = make_readback(mon_raw=150)
        bad[39] = 0xFFFFFFFF
        seq = iter([(1, bad), (1, bad), (1, make_readback(mon_raw=150))])
        backend.get_hw_readback = lambda: next(seq, (1, make_readback(mon_raw=150)))
        daemon._push_defaults_to_hardware()
        levels = {c[3] for c in backend.mixer_params if c[:3] == (2, 1, 0x01)}
        assert levels == {150}

    def test_no_readback_does_not_wait(self, daemon, backend):
        backend.readback = None
        t0 = time.monotonic()
        daemon._push_defaults_to_hardware()
        assert time.monotonic() - t0 < 0.1
