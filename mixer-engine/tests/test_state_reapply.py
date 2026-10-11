"""A6: the persisted state that the hardware cannot report back is pushed
once on connect, in the Mixer Engine's order; readback-derived values keep
the hardware's."""

import json

import pytest

import hardware
from conftest import DEVICE_MAP_X4, FakeBackend, make_readback
from hardware import (HardwareRouter, SUB_PARAM_CUE1, SUB_PARAM_CUE3,
                      SUB_PARAM_CUE4, SUB_PARAM_MIX, encode_aux_fader,
                      encode_input_fader, encode_mix_coeff,
                      encode_mix_coeff_r)
from state_tree import StateTree
from ua_mixer_daemon import MixerDaemon

IN = "/devices/0/inputs"
OUT = "/devices/0/outputs"
AUX = "/devices/0/auxs"
DEV = "/devices/0"


class SeqBackend(FakeBackend):
    """FakeBackend that also keeps one ordered log of every write."""

    def __init__(self):
        super().__init__()
        self.seq = []

    def set_mixer_param(self, channel_type, channel_idx, param_id, value):
        self.seq.append(("param", channel_type, channel_idx, param_id, value))
        return super().set_mixer_param(channel_type, channel_idx, param_id,
                                       value)

    def set_mixer_bus_param(self, bus_id, sub_param, value, side=0):
        self.seq.append(("bus", bus_id, sub_param, side, value))
        return super().set_mixer_bus_param(bus_id, sub_param, value, side)


def build(tmp_path, saved, readback=None):
    """A daemon whose tree was restored from a state file with `saved`."""
    state_file = tmp_path / "state.json"
    state_file.write_text(json.dumps(saved))
    tree = StateTree()
    tree.load_device_map(DEVICE_MAP_X4)
    tree.enable_persistence(state_file)
    backend = SeqBackend()
    backend.readback = readback
    daemon = MixerDaemon(tree, HardwareRouter(backend, tree))
    daemon._push_defaults_to_hardware()
    return daemon, backend, tree


def last_param(backend, ch_type, param, ch_idx=None):
    hits = [e for e in backend.seq
            if e[0] == "param" and e[1] == ch_type and e[3] == param
            and (ch_idx is None or e[2] == ch_idx)]
    return hits[-1][4] if hits else None


def last_bus(backend, bus, sub, side=0):
    hits = [e for e in backend.seq
            if e[0] == "bus" and e[1] == bus and e[2] == sub and e[3] == side]
    return hits[-1][4] if hits else None


def position(backend, pred):
    idx = [i for i, e in enumerate(backend.seq) if pred(e)]
    return idx[-1] if idx else None


# ── Nothing saved ───────────────────────────────────────────────────

class TestNothingSaved:
    def test_no_extra_writes(self, tmp_path):
        d, be, tree = build(tmp_path, {})
        assert d.hw_router.reapply_saved_state({}) == 0


# ── Sends, aux, solo, faders ────────────────────────────────────────

class TestSendsAndAux:
    def test_input_cue_send_restored(self, tmp_path):
        saved = {f"{IN}/0/sends/2/Gain/value": -6.0}      # send 2 = CUE 1
        d, be, tree = build(tmp_path, saved)
        assert last_bus(be, 0x00, SUB_PARAM_CUE1) == \
            pytest.approx(encode_input_fader(-6.0))

    def test_cue3_cue4_sends_win_over_the_silent_init(self, tmp_path):
        saved = {f"{IN}/0/sends/4/Gain/value": -10.0,      # CUE 3
                 f"{IN}/0/sends/5/Gain/value": -20.0}      # CUE 4
        d, be, tree = build(tmp_path, saved)
        assert last_bus(be, 0x00, SUB_PARAM_CUE3) == \
            pytest.approx(encode_input_fader(-10.0))
        assert last_bus(be, 0x00, SUB_PARAM_CUE4) == \
            pytest.approx(encode_input_fader(-20.0))
        # ... and they come after the zeroing
        zero = position(be, lambda e: e[:3] == ("bus", 0x00, SUB_PARAM_CUE3)
                        and e[4] == 0.0)
        real = position(be, lambda e: e[:3] == ("bus", 0x00, SUB_PARAM_CUE3)
                        and e[4] != 0.0)
        assert real > zero

    def test_aux_send_of_an_input_restored(self, tmp_path):
        # AUX sends are post-fader: the input has to be unmuted to be heard
        saved = {f"{IN}/3/sends/0/Gain/value": -3.0,        # AUX 1
                 f"{IN}/3/Mute/value": False}
        d, be, tree = build(tmp_path, saved)
        assert last_bus(be, 0x03, hardware.SUB_PARAM_AUX1) == \
            pytest.approx(encode_input_fader(-3.0))

    def test_bypassed_send_is_written_silent(self, tmp_path):
        saved = {f"{IN}/0/sends/2/Gain/value": -6.0,
                 f"{IN}/0/sends/2/Bypass/value": True}
        d, be, tree = build(tmp_path, saved)
        assert last_bus(be, 0x00, SUB_PARAM_CUE1) == 0.0

    def test_gain_tapered_only_is_enough(self, tmp_path):
        # the tree keeps Gain and GainTapered; saving one restores both
        tapered = hardware.fader_db_to_tapered(-12.0)
        saved = {f"{IN}/1/sends/3/GainTapered/value": tapered,
                 f"{IN}/1/sends/3/Gain/value": -12.0}
        d, be, tree = build(tmp_path, saved)
        assert last_bus(be, 0x01, hardware.SUB_PARAM_CUE2) == \
            pytest.approx(encode_input_fader(-12.0), rel=1e-3)

    def test_aux_return_fader_and_cue_send_restored(self, tmp_path):
        saved = {f"{AUX}/0/FaderLevel/value": -4.0,
                 f"{AUX}/1/sends/0/Gain/value": -8.0}       # aux 2 -> CUE 1
        d, be, tree = build(tmp_path, saved)
        assert last_bus(be, 0x10, SUB_PARAM_MIX) == \
            pytest.approx(encode_aux_fader(-4.0))
        assert last_bus(be, 0x12, SUB_PARAM_CUE1) == \
            pytest.approx(encode_aux_fader(-8.0))

    def test_aux_mute_and_mono(self, tmp_path):
        saved = {f"{AUX}/0/Mute/value": True,
                 f"{AUX}/1/MixToMono/value": True}
        d, be, tree = build(tmp_path, saved)
        assert last_bus(be, 0x10, SUB_PARAM_MIX) == 0.0
        assert last_param(be, 2, 0x0F) == 1

    def test_cue4_send_of_an_aux(self, tmp_path):
        saved = {f"{AUX}/0/sends/3/Gain/value": -2.0}       # aux 1 -> CUE 4
        d, be, tree = build(tmp_path, saved)
        assert last_bus(be, 0x10, SUB_PARAM_CUE4) == \
            pytest.approx(encode_aux_fader(-2.0))


class TestFadersPanMuteSolo:
    def test_fader_and_pan_write_left_and_right_of_sub_0(self, tmp_path):
        saved = {f"{IN}/0/FaderLevel/value": -10.0,
                 f"{IN}/0/Pan/value": -1.0,
                 f"{IN}/0/Mute/value": False}
        d, be, tree = build(tmp_path, saved)
        assert last_bus(be, 0x00, SUB_PARAM_MIX, 0) == \
            pytest.approx(encode_mix_coeff(-10.0, -1.0))
        assert last_bus(be, 0x00, SUB_PARAM_MIX, 1) == \
            pytest.approx(encode_mix_coeff_r(-10.0, -1.0), abs=1e-6)
        assert last_bus(be, 0x00, SUB_PARAM_MIX, 0) > 0.1
        assert last_bus(be, 0x00, SUB_PARAM_MIX, 1) < 1e-6   # hard left

    def test_saved_mute_zeroes_both_sides(self, tmp_path):
        saved = {f"{IN}/1/FaderLevel/value": -5.0,
                 f"{IN}/1/Mute/value": True}
        d, be, tree = build(tmp_path, saved)
        assert last_bus(be, 0x01, SUB_PARAM_MIX, 0) == 0.0
        assert last_bus(be, 0x01, SUB_PARAM_MIX, 1) == 0.0

    def test_solo_silences_the_others_on_both_sides(self, tmp_path):
        saved = {f"{IN}/0/FaderLevel/value": 0.0,
                 f"{IN}/0/Mute/value": False,
                 f"{IN}/2/FaderLevel/value": 0.0,
                 f"{IN}/2/Mute/value": False,
                 f"{IN}/2/Solo/value": True}
        d, be, tree = build(tmp_path, saved)
        assert last_bus(be, 0x00, SUB_PARAM_MIX, 0) == 0.0
        assert last_bus(be, 0x00, SUB_PARAM_MIX, 1) == 0.0
        assert last_bus(be, 0x02, SUB_PARAM_MIX, 0) == \
            pytest.approx(encode_mix_coeff(0.0, 0.0))
        assert last_bus(be, 0x02, SUB_PARAM_MIX, 1) == \
            pytest.approx(encode_mix_coeff_r(0.0, 0.0))

    def test_inputs_without_saved_state_start_muted_at_0_db(self, tmp_path):
        d, be, tree = build(tmp_path, {})
        assert tree.get_value(f"{IN}/4/Mute/value") is True
        assert last_bus(be, 0x08, SUB_PARAM_MIX, 0) == 0.0


# ── Outputs, CUE, mirror, talkback config, device-level ─────────────

class TestOutputsAndConfig:
    def test_cue_mono_mix_and_mirror_restored(self, tmp_path):
        saved = {f"{OUT}/14/MixToMono/value": True,
                 f"{OUT}/14/MixInSource/value": "cue",
                 f"{OUT}/15/OutputDestination/value": "ADAT 1-2"}
        d, be, tree = build(tmp_path, saved)
        assert last_param(be, 2, 0x06) == 1                 # CUE 1 mono
        assert last_param(be, 2, 0x05) == hardware.CUE_MIX_ON
        assert last_param(be, 2, 0x2F) == hardware.MIRROR_SOURCE_MAP["ADAT 1-2"]
        assert last_param(be, 2, 0x3C) == 1                 # mirror enabled

    def test_headphone_source_restored_but_not_the_level(self, tmp_path):
        saved = {f"{OUT}/20/MixInSource/value": "cue1",
                 f"{OUT}/20/CRMonitorLevel/value": -20.0}
        d, be, tree = build(tmp_path, saved,
                            readback=(1, make_readback(mon_raw=150,
                                                       hp2_raw=140)))
        assert last_param(be, 2, 0x40) == 0                 # HP2 -> CUE 1
        # HP2 level is the readback's business: no write from the saved -20
        assert last_param(be, 2, 0x66) is None

    def test_monitor_pad_and_line_out_pad(self, tmp_path):
        saved = {f"{OUT}/18/Pad/value": True,
                 f"{OUT}/2/Pad/value": True}
        d, be, tree = build(tmp_path, saved)
        assert last_param(be, 2, 0x32) == 1
        assert last_param(be, 2, 0x1A) == 1

    def test_alt_monitors_restored(self, tmp_path):
        saved = {f"{OUT}/0/AltMonTrim/value": 3.0,
                 f"{OUT}/2/AltMonEnabled/value": True,
                 f"{DEV}/AltMonSelection/value": 2,
                 f"{DEV}/RemoteMonitorController/value": True}
        d, be, tree = build(tmp_path, saved)
        assert last_param(be, 2, 0x38) == 66
        assert last_param(be, 2, 0x3C) == 1
        assert last_param(be, 2, 0x3A) == 2
        assert last_param(be, 2, 0x41) == 1

    def test_talkback_configuration_restored(self, tmp_path):
        saved = {"/TalkbackInPhysicalCR/value": True,
                 f"{DEV}/TalkbackMaster/value": 0,
                 f"{DEV}/InputJackSensingDisabled/value": True}
        d, be, tree = build(tmp_path, saved)
        assert last_param(be, 2, 0x47) == 1
        assert last_param(be, 2, 0x48) == 1                # this unit
        assert last_param(be, 2, 0x45) == 1
        # OutputsAttenuate is not in the x4 map: nothing to restore
        assert last_param(be, 2, 0x20) == 0                # init default

    def test_talkback_is_never_switched_on(self, tmp_path):
        saved = {"/TalkbackOn/value": True}
        d, be, tree = build(tmp_path, saved)
        assert tree.get_value("/TalkbackOn/value") is False
        tb = [e for e in be.seq if e[0] == "param" and e[3] == 0x46]
        assert tb and all(e[4] == 0 for e in tb)

    def test_device_mode_and_dim_step_restored(self, tmp_path):
        saved = {f"{DEV}/TOSLinkOutput/value": "ADAT",
                 f"{DEV}/DimAttenuation/value": 34}
        d, be, tree = build(tmp_path, saved)
        assert last_param(be, 2, 0x21) == 8                # ADAT
        assert last_param(be, 2, 0x43) == 4                # step for 34 dB

    def test_hiz_impedance_and_src_restored(self, tmp_path):
        saved = {f"{IN}/0/HiZImpedance/value": 4,
                 f"{IN}/12/SRConvert/value": True}
        d, be, tree = build(tmp_path, saved)
        # HiZImpedance is not in the x4 map: the tree drops it on load, so
        # it only reaches hardware through a client SET (see test_mixer_params)
        assert last_param(be, 2, 0x1F) == 1

    def test_hiz_impedance_saved_state_pushed_when_the_map_has_it(
            self, tmp_path):
        d, be, tree = build(tmp_path, {})
        tree.add_runtime_property(f"{IN}/0/preamps/0/HiZImpedance",
                                  {"type": "int", "value": 3})
        d.hw_router.reapply_saved_state(
            {f"{IN}/0/preamps/0/HiZImpedance/value": 3})
        assert last_param(be, 1, 0x11) == 3

    def test_order_is_device_aux_sends_outputs(self, tmp_path):
        saved = {f"{OUT}/14/MixToMono/value": True,
                 f"{IN}/0/sends/2/Gain/value": -6.0,
                 f"{AUX}/0/FaderLevel/value": -4.0,
                 f"{DEV}/InputJackSensingDisabled/value": True}
        d, be, tree = build(tmp_path, saved)
        dev = position(be, lambda e: e[:1] == ("param",) and e[3] == 0x45
                       and e[4] == 1)
        aux = position(be, lambda e: e[:3] == ("bus", 0x10, SUB_PARAM_MIX)
                       and e[4] != 0.0)
        send = position(be, lambda e: e[:3] == ("bus", 0x00, SUB_PARAM_CUE1)
                        and e[4] != 0.0)
        cue = position(be, lambda e: e[:1] == ("param",) and e[3] == 0x06
                       and e[4] == 1)
        assert dev < aux < send < cue

    def test_nothing_is_pushed_twice_in_a_row(self, tmp_path):
        saved = {f"{IN}/0/sends/2/Gain/value": -6.0}
        d, be, tree = build(tmp_path, saved)
        hits = [e for e in be.seq
                if e[:3] == ("bus", 0x00, SUB_PARAM_CUE1) and e[3] == 0
                and e[4] != 0.0]
        assert len(hits) == 1


# ── Readback-derived values stay the hardware's ─────────────────────

class TestReadbackWins:
    def readback(self):
        return (1, make_readback(
            mon_raw=150, mute=False, mono=True, dim=False,
            flags={0: {"48v"}, 2: {"link"}}, gains_c=[21, 31, 41, 51]))

    def saved_preamp_and_monitor(self):
        return {f"{IN}/0/preamps/0/Gain/value": 55.0,
                f"{IN}/0/preamps/0/48V/value": False,
                f"{IN}/1/preamps/0/Pad/value": True,
                f"{IN}/0/Stereo/value": False,
                f"{IN}/2/Stereo/value": False,
                f"{OUT}/18/CRMonitorLevel/value": -10.0,
                f"{OUT}/18/Mute/value": True,
                f"{OUT}/18/DimOn/value": True,
                f"{OUT}/18/MixToMono/value": False}

    def test_preamps_keep_the_readback(self, tmp_path):
        d, be, tree = build(tmp_path, self.saved_preamp_and_monitor(),
                            readback=self.readback())
        gainc = {}
        for e in be.seq:
            if e[0] == "param" and e[1] == 1 and e[3] == 0x06:
                gainc[e[2]] = e[4]
        assert gainc == {0: 21, 1: 31, 2: 41, 3: 51}
        assert last_param(be, 1, 0x03, 0) == 1              # 48V from readback
        assert last_param(be, 1, 0x01, 1) == 0              # pad not saved's
        assert last_param(be, 1, 0x02, 0) == 0
        assert last_param(be, 1, 0x02, 2) == 1              # link from readback

    def test_monitor_keeps_the_readback(self, tmp_path):
        d, be, tree = build(tmp_path, self.saved_preamp_and_monitor(),
                            readback=self.readback())
        levels = {e[4] for e in be.seq
                  if e[0] == "param" and e[1] == 2 and e[3] == 0x01}
        assert levels == {150}
        assert last_param(be, 2, 0x03) == 1                 # mono, unmuted
        assert last_param(be, 2, 0x44, 1) == 0              # dim off

    def test_rate_and_clock_source_are_left_to_the_driver(self, tmp_path):
        saved = {"/SampleRate/value": 96000, "/ClockSource/value": "ADAT"}
        d, be, tree = build(tmp_path, saved)
        assert be.driver_params == []
