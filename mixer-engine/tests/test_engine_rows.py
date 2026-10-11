"""Mixer Engine property handlers of master-plan rows N27, N30 and N18:
each property reaches the param the engine sends, with its encoding, only
on the models whose MixerType class sends it.  x4 and x8p maps are the
captured ones."""

import pytest

from conftest import DEVICE_MAP_X4, FakeBackend
from hardware import HardwareRouter, set_monitor_taper
from model_features import load_descriptor

IN = "/devices/0/inputs"
OUT = "/devices/0/outputs"
DEV = "/devices/0"
PRE, OTHER = 1, 2
DEVICE_MAP_X8P = DEVICE_MAP_X4.with_name("device_map_apollo_x8p.json")


def sets(backend, ch_type=None, param=None):
    return [c for c in backend.mixer_params
            if (ch_type is None or c[0] == ch_type)
            and (param is None or c[2] == param)]


@pytest.fixture(autouse=True)
def restore_taper():
    yield
    set_monitor_taper(None)


@pytest.fixture
def x4(backend, tree):
    r = HardwareRouter(backend, tree)
    r.set_model_data(load_descriptor("apollo-x4"))
    tree.on_set = r.on_set
    return r


@pytest.fixture
def x8p():
    from state_tree import StateTree
    t = StateTree()
    t.load_device_map(DEVICE_MAP_X8P)
    be = FakeBackend()
    r = HardwareRouter(be, t)
    r.set_model_data(load_descriptor("apollo-x8p"))
    t.on_set = r.on_set
    return r


# ── N27: HiZ client override (input param 0x14) ─────────────────────

class TestHiZOverride:
    def test_jack_decides_while_sensing_is_on(self, x4, backend):
        x4.on_set(f"{IN}/0/preamps/0/HiZ/value", True)
        assert sets(backend, PRE, 0x14) == []

    def test_host_owns_it_with_sensing_off(self, x4, backend, tree):
        tree.set(f"{DEV}/InputJackSensingDisabled/value", True)
        backend.clear()
        x4.on_set(f"{IN}/1/preamps/0/HiZ/value", True)
        x4.on_set(f"{IN}/1/preamps/0/HiZ/value", False)
        assert sets(backend, PRE, 0x14) == [(PRE, 1, 0x14, 1),
                                            (PRE, 1, 0x14, 0)]

    def test_disabling_sensing_pushes_the_hiz_inputs(self, x4, backend, tree):
        tree.set(f"{IN}/0/preamps/0/HiZ/value", True)
        backend.clear()
        tree.set(f"{DEV}/InputJackSensingDisabled/value", True)
        assert sets(backend, OTHER, 0x45) == [(OTHER, 0, 0x45, 1)]
        assert sets(backend, PRE, 0x14) == [(PRE, 0, 0x14, 1),
                                            (PRE, 1, 0x14, 0)]

    def test_inputs_without_hiz(self, x4, backend, tree):
        tree.set(f"{DEV}/InputJackSensingDisabled/value", True)
        backend.clear()
        x4.on_set(f"{IN}/2/preamps/0/HiZ/value", True)
        assert sets(backend, PRE, 0x14) == []


# ── N27: knob steps / selected param / display text ─────────────────

class TestPreampDisplay:
    def test_knob_steps_param_0x10(self, x4, backend):
        x4.on_set(f"{IN}/2/preamps/0/PreampInputKnobSteps/value", 0x13)
        assert backend.mixer_params == [(PRE, 2, 0x10, 0x3)]

    def test_selected_param_0x12(self, x4, backend):
        x4.on_set(f"{IN}/3/preamps/0/PreampSelectedParam/value", 6)
        assert backend.mixer_params == [(PRE, 3, 0x12, 2)]

    def test_display_text_is_software_only(self, x4, backend):
        x4.on_set(f"{IN}/0/preamps/0/PreampGainCustomDisplayText/value", "x")
        assert backend.mixer_params == []

    def test_no_preamp_no_write(self, x4, backend):
        x4.on_set(f"{IN}/14/preamps/0/PreampInputKnobSteps/value", 3)
        assert backend.mixer_params == []


# ── N27: clip / peak hold, talkback mic select ──────────────────────

class TestRootParams:
    @pytest.mark.parametrize("value, idx", [
        ("NONE", 0), ("1 SEC", 1), ("3 SEC", 2), ("5 SEC", 3),
        ("10 SEC", 4), ("INFINITE", 5), (2, 2)])
    def test_clip_hold(self, x4, backend, value, idx):
        x4.on_set("/ClipHold/value", value)
        assert backend.mixer_params == [(OTHER, 0, 0x13, idx)]

    def test_peak_hold_has_no_infinite(self, x4, backend):
        x4.on_set("/PeakHold/value", "10 SEC")
        x4.on_set("/PeakHold/value", "INFINITE")
        assert backend.mixer_params == [(OTHER, 0, 0x14, 4)]

    def test_talkback_mic_select(self, x4, backend):
        x4.on_set("/TalkbackMicSelect/value", 5)
        assert backend.mixer_params == [(OTHER, 0, 0x65, 1)]

    def test_no_mic_select_without_talkback(self, x4, backend):
        x4.set_model_data(load_descriptor("arrow"))
        x4.on_set("/TalkbackMicSelect/value", 1)
        assert backend.mixer_params == []


# ── N27: a stereo-linked input pair drives two buses ────────────────

class TestStereoPair:
    """x4 map: S/PDIF L/R (inputs 12/13, buses 8/9) are linked, Pan -1 and
    Pan2 +1 on the leader."""

    def left_right(self, backend, bus):
        left = [v for b, s, v in backend.bus_params if b == bus and s == 0]
        right = [v for b, s, v in backend.bus_params_r if b == bus and s == 0]
        return left[-1] if left else None, right[-1] if right else None

    def test_leader_fader_drives_both_buses(self, x4, backend):
        x4.on_set(f"{IN}/12/FaderLevel/value", 0.0)
        l8, r8 = self.left_right(backend, 0x08)
        l9, r9 = self.left_right(backend, 0x09)
        # Pan -1: S/PDIF L all left; Pan2 +1: S/PDIF R all right
        assert l8 == pytest.approx(1.0) and r8 == pytest.approx(0.0, abs=1e-9)
        assert l9 == pytest.approx(0.0, abs=1e-9) and r9 == pytest.approx(1.0)

    def test_follower_writes_no_main_mix(self, x4, backend):
        x4.on_set(f"{IN}/13/FaderLevel/value", -6.0)
        x4.on_set(f"{IN}/13/Pan/value", 0.5)
        assert [b for b, s, _ in backend.bus_params if s == 0] == []

    def test_pan2_moves_the_second_bus(self, x4, backend):
        x4.on_set(f"{IN}/12/Pan2/value", 0.0)
        assert {b for b, s, _ in backend.bus_params} == {0x09}

    def test_pan2_on_a_mono_input_does_nothing(self, x4, backend):
        x4.on_set(f"{IN}/0/Pan2/value", 0.5)
        assert backend.bus_params == []

    def test_leader_mute_silences_both(self, x4, backend):
        x4.on_set(f"{IN}/12/Mute/value", True)
        assert self.left_right(backend, 0x08) == (0.0, 0.0)
        assert self.left_right(backend, 0x09) == (0.0, 0.0)

    def test_unlink_gives_the_bus_back(self, x4, backend, tree):
        tree.set(f"{IN}/12/Stereo/value", False)
        backend.clear()
        x4.on_set(f"{IN}/13/FaderLevel/value", 0.0)
        # S/PDIF R with its own (centre) pan
        l9, r9 = self.left_right(backend, 0x09)
        assert l9 == pytest.approx(r9) and l9 > 0

    def test_link_toggle_recalculates(self, x4, backend, tree):
        tree.set(f"{IN}/12/Stereo/value", False)
        backend.clear()
        tree.set(f"{IN}/12/Stereo/value", True)
        buses = {b for b, s, _ in backend.bus_params if s == 0}
        assert {0x08, 0x09} <= buses


# ── N30 / N18 on the captured x8p map ──────────────────────────────

class TestX8p:
    def test_func_switch_mode(self, x8p):
        x8p.on_set(f"{DEV}/FuncSwitchMode/value", "mono")
        x8p.on_set(f"{DEV}/FuncSwitchMode/value", "talkback")
        assert sets(x8p.backend, OTHER, 0x42) == [(OTHER, 0, 0x42, 2),
                                                  (OTHER, 0, 0x42, 4)]

    def test_func_switch_alt1_not_on_hexa(self, x8p):
        x8p.on_set(f"{DEV}/FuncSwitchMode/value", "alt 1")
        x8p.on_set(f"{DEV}/FuncSwitch2Mode/value", "mono")
        assert x8p.backend.mixer_params == []

    def test_func_switch_not_on_the_x4(self, x4, backend):
        x4.on_set(f"{DEV}/FuncSwitchMode/value", "mono")
        assert backend.mixer_params == []

    def test_pga_disabled_in_line_mode(self, x8p):
        x8p.on_set(f"{IN}/5/PGADisabledInLineMode/value", True)
        assert x8p.backend.mixer_params == [(PRE, 5, 0x15, 1)]

    def test_no_pga_disable_on_the_x4(self, x4, backend):
        x4.on_set(f"{IN}/1/PGADisabledInLineMode/value", True)
        assert backend.mixer_params == []

    def test_direct_out_allocates_signals(self, x8p):
        tree = x8p.state_tree
        tree.set(f"{IN}/0/OutputDestination/value", "Line 3")
        tree.set(f"{IN}/1/OutputDestination/value", "ADAT 1")
        routes = sets(x8p.backend, PRE, 0x13)
        # signal | dest synch-out << 8 | monitor mix (bit 16)
        assert routes == [(PRE, 0x00, 0x13, 0 | 10 << 8 | 0x10000),
                          (PRE, 0x01, 0x13, 1 | 16 << 8 | 0x10000)]

    def test_monitor_frees_the_signal(self, x8p):
        tree = x8p.state_tree
        tree.set(f"{IN}/0/OutputDestination/value", "Line 3")
        tree.set(f"{IN}/0/OutputDestination/value", "Monitor")
        tree.set(f"{IN}/1/OutputDestination/value", "Line 1")
        routes = [c[3] for c in sets(x8p.backend, PRE, 0x13)]
        assert routes[1] == 0x1FFFF            # ClearOutputDestination value
        assert routes[2] & 0xFF == 0           # signal 0 is free again

    def test_monitor_mix_enable_clears_bit_16(self, x8p):
        tree = x8p.state_tree
        tree.set(f"{IN}/2/OutputDestination/value", "Line 2")
        tree.set(f"{IN}/2/OutputMonitorMixEnable/value", False)
        routes = [c[3] for c in sets(x8p.backend, PRE, 0x13)]
        assert routes[-1] == 0 | 9 << 8

    def test_stereo_leader_bit_17(self, x8p):
        tree = x8p.state_tree       # x8p S/PDIF L/R (16/17) are linked
        tree.set(f"{IN}/16/OutputDestination/value", "Line 1")
        route = sets(x8p.backend, PRE, 0x13)[-1]
        assert route[1] == 0x08 and route[3] & 0x20000

    def test_no_direct_outs_on_the_x4(self, x4, backend):
        x4.on_set(f"{IN}/0/OutputDestination/value", "Line 1")
        assert backend.mixer_params == []

    @pytest.mark.parametrize("mode, idx", [("STEREO", 0), ("5.1", 5),
                                           ("7.1", 7)])
    def test_surround_monitor_mode(self, x8p, mode, idx):
        x8p.on_set(f"{DEV}/SurroundMonitorMode/value", mode)
        assert x8p.backend.mixer_params == [(OTHER, 0, 0x4a, idx)]

    def test_no_surround_on_the_x4(self, x4, backend):
        x4.on_set(f"{DEV}/SurroundMonitorMode/value", "5.1")
        assert backend.mixer_params == []

    def test_surround_fold_down(self, x8p):
        x8p.on_set(f"{OUT}/0/SurroundStereoFoldDown/value", True)
        assert x8p.backend.mixer_params == [(OTHER, 0, 0x63, 1)]

    def test_surround_trim_solo_mute_stay_in_the_tree(self, x8p):
        for ctl, v in (("SurroundTrimLevel", -3.0), ("SurroundSolo", True),
                       ("SurroundMute", True), ("SurroundOutputEnabled", True)):
            x8p.on_set(f"{OUT}/0/{ctl}/value", v)
        assert x8p.backend.mixer_params == []

    def test_monitor_is_output_22_with_the_48_step_taper(self, x8p):
        x8p.on_set(f"{OUT}/22/CRMonitorLevelTapered/value", 0.5)
        assert x8p.backend.mixer_params == [(OTHER, 1, 0x01, 192 + 2 * -12)]


# ── N30: line-input reference level ─────────────────────────────────

class TestLineReferenceLevel:
    def test_line_input_of_an_x8(self, backend, tree):
        r = HardwareRouter(backend, tree)
        r.set_model_data(load_descriptor("apollo-x8"))
        # x4 map input names, x8 model data: synch-ins 4-7 have the level
        tree.set(f"{IN}/2/Name/value", "ANALOG 5")
        r._input_bus_table = None
        r.on_set(f"{IN}/2/Pad/value", True)
        assert backend.mixer_params == [(PRE, 4, 0x01, 1)]

    def test_not_on_the_x4(self, x4, backend):
        x4.on_set(f"{IN}/2/Pad/value", True)
        assert backend.mixer_params == []


# ── state re-apply after a restart (A6) covers the new controls ─────

def test_reapply_includes_the_new_device_settings(x8p):
    tree = x8p.state_tree
    tree.set(f"{DEV}/FuncSwitchMode/value", "dim")
    tree.set("/ClipHold/value", "5 SEC")
    x8p.backend.clear()
    x8p.reapply_saved_state({f"{DEV}/FuncSwitchMode/value",
                             "/ClipHold/value"})
    assert (OTHER, 0, 0x42, 3) in x8p.backend.mixer_params
    assert (OTHER, 0, 0x13, 3) in x8p.backend.mixer_params
