"""Property -> SetMixerParam mappings of parity/14 section 8 (M5, M6, M10,
A7, A11): each device-map property reaches the right param with the right
value encoding."""

import pytest

import hardware
from hardware import (HIZ_IMPEDANCES_OHM, HardwareRouter, encode_altmon_trim,
                      hiz_impedance_code)

IN = "/devices/0/inputs"
OUT = "/devices/0/outputs"
DEV = "/devices/0"
PRE = 1      # preamp channel type
OTHER = 2    # SetOtherParam type


def sets(backend, ch_type=None, param=None):
    return [c for c in backend.mixer_params
            if (ch_type is None or c[0] == ch_type)
            and (param is None or c[2] == param)]


# ── M5: HiZ impedance ───────────────────────────────────────────────

class TestHiZImpedance:
    def test_table_is_the_kext_order(self):
        assert HIZ_IMPEDANCES_OHM == (2_200_000, 1_000_000, 453_000, 232_000,
                                      100_000, 70_000, 47_000, 21_000)

    @pytest.mark.parametrize("code", range(8))
    def test_code_goes_to_param_0x11(self, router, backend, code):
        router.on_set(f"{IN}/0/preamps/0/HiZImpedance/value", code)
        assert backend.mixer_params == [(PRE, 0, 0x11, code)]

    @pytest.mark.parametrize("value, code", [
        ("2.2M", 0), ("1M", 1), ("453k", 2), ("232 kOhm", 3), ("100k", 4),
        ("70k", 5), ("47K", 6), ("21k", 7), (1_000_000, 1), (21000, 7),
        ("100 kΩ", 4)])
    def test_resistance_maps_to_the_code(self, value, code):
        assert hiz_impedance_code(value) == code

    @pytest.mark.parametrize("value", [8, -1, "banana", None, True, 12345])
    def test_unknown_value_is_not_written(self, router, backend, value):
        router.on_set(f"{IN}/1/preamps/0/HiZImpedance/value", value)
        assert backend.mixer_params == []

    def test_second_hiz_input(self, router, backend):
        router.on_set(f"{IN}/1/preamps/0/HiZImpedance/value", 5)
        assert backend.mixer_params == [(PRE, 1, 0x11, 5)]

    @pytest.mark.parametrize("ch", [2, 3, 8])
    def test_only_inputs_1_and_2(self, router, backend, ch):
        router.on_set(f"{IN}/{ch}/preamps/0/HiZImpedance/value", 1)
        router.on_set(f"{IN}/{ch}/HiZImpedance/value", 1)
        assert backend.mixer_params == []

    def test_input_level_property_works_too(self, router, backend):
        router.on_set(f"{IN}/0/HiZImpedance/value", "47k")
        assert backend.mixer_params == [(PRE, 0, 0x11, 6)]

    def test_init_no_longer_writes_the_hiz_flag(self, router, backend):
        from ua_mixer_daemon import MixerDaemon
        daemon = MixerDaemon(router.state_tree, router)
        daemon._push_defaults_to_hardware()
        assert sets(backend, PRE, 0x11) == []

    def test_hiz_bool_property_still_writes_nothing(self, router, backend):
        router.on_set(f"{IN}/0/preamps/0/HiZ/value", True)
        assert sets(backend, PRE, 0x11) == []


# ── M6: Link / Stereo ───────────────────────────────────────────────

class TestLink:
    @pytest.mark.parametrize("ch", [0, 2])
    def test_leader_carries_the_link(self, router, backend, ch):
        router.on_set(f"{IN}/{ch}/Stereo/value", True)
        router.on_set(f"{IN}/{ch}/Stereo/value", False)
        assert sets(backend, PRE, 0x02) == [(PRE, ch, 0x02, 1),
                                            (PRE, ch, 0x02, 0)]

    @pytest.mark.parametrize("ch", [1, 3])
    def test_odd_channel_forwards_to_the_leader(self, router, backend, ch):
        # UA's SetProperty 0x45 on the right leg links the pair from the left
        router.on_set(f"{IN}/{ch}/Stereo/value", True)
        assert sets(backend, PRE, 0x02) == [(PRE, ch - 1, 0x02, 1)]

    @pytest.mark.parametrize("ch", [4, 12, 20])
    def test_inputs_without_a_preamp_write_nothing(self, router, backend, ch):
        router.on_set(f"{IN}/{ch}/Stereo/value", True)
        assert backend.mixer_params == []


# ── M10: alt monitors, remote monitor controller ────────────────────

class TestAltMonitors:
    def test_trim_encoding(self):
        assert encode_altmon_trim(0.0) == 60
        assert encode_altmon_trim(-30.0) == 0
        assert encode_altmon_trim(30.0) == 0x78
        assert encode_altmon_trim(-99.0) == 0
        assert encode_altmon_trim(99.0) == 0x78
        assert encode_altmon_trim(1.0) == 62
        assert encode_altmon_trim(0.25) == 61

    @pytest.mark.parametrize("ch, param", [(0, 0x38), (1, 0x38),
                                           (2, 0x39), (3, 0x39)])
    def test_trim_per_alt_monitor(self, router, backend, ch, param):
        router.on_set(f"{OUT}/{ch}/AltMonTrim/value", -3.0)
        assert backend.mixer_params == [(OTHER, 0, param, 54)]

    @pytest.mark.parametrize("ch, param", [(0, 0x3B), (1, 0x3B),
                                           (2, 0x3C), (3, 0x3C)])
    def test_enabled_per_alt_monitor(self, router, backend, ch, param):
        router.on_set(f"{OUT}/{ch}/AltMonEnabled/value", True)
        router.on_set(f"{OUT}/{ch}/AltMonEnabled/value", False)
        assert backend.mixer_params == [(OTHER, 0, param, 1),
                                        (OTHER, 0, param, 0)]

    @pytest.mark.parametrize("value, hw", [(0, 0), (1, 1), (2, 2), (3, 3),
                                           (5, 1)])
    def test_selection(self, router, backend, value, hw):
        router.on_set(f"{DEV}/AltMonSelection/value", value)
        assert backend.mixer_params == [(OTHER, 0, 0x3A, hw)]

    def test_selection_garbage(self, router, backend):
        router.on_set(f"{DEV}/AltMonSelection/value", "x")
        assert backend.mixer_params == []

    def test_remote_monitor_controller(self, router, backend):
        router.on_set(f"{DEV}/RemoteMonitorController/value", True)
        router.on_set(f"{DEV}/RemoteMonitorController/value", False)
        assert backend.mixer_params == [(OTHER, 0, 0x41, 1),
                                        (OTHER, 0, 0x41, 0)]

    def test_names_in_the_monitor_table(self):
        mp = hardware.MONITOR_PARAM
        assert (mp["AltMon1Trim"], mp["AltMon2Trim"]) == (0x38, 0x39)
        assert mp["AltMonSelection"] == 0x3A
        assert (mp["AltMon1Enabled"], mp["AltMon2Enabled"]) == (0x3B, 0x3C)

    def test_outputs_that_are_not_line_outs_have_no_altmon(self, router,
                                                           backend):
        router.on_set(f"{OUT}/18/AltMonEnabled/value", True)
        router.on_set(f"{OUT}/19/AltMonTrim/value", 3.0)
        assert backend.mixer_params == []


# ── A7: CR PGA bypass, 24 dB, pads, attenuate, jack sensing, talkback ──

class TestOtherParams:
    def test_cr_pga_bypass_is_inverted(self, router, backend):
        router.on_set(f"{OUT}/18/CRPGABypass/value", True)
        router.on_set(f"{OUT}/18/CRPGABypass/value", False)
        assert backend.mixer_params == [(OTHER, 0, 0x3E, 0),
                                        (OTHER, 0, 0x3E, 1)]

    def test_outputs_attenuate(self, router, backend):
        router.on_set(f"{DEV}/OutputsAttenuate/value", True)
        assert backend.mixer_params == [(OTHER, 0, 0x20, 1)]

    def test_input_jack_sensing_disabled(self, router, backend):
        router.on_set(f"{DEV}/InputJackSensingDisabled/value", True)
        router.on_set(f"{DEV}/InputJackSensingDisabled/value", False)
        assert backend.mixer_params == [(OTHER, 0, 0x45, 1),
                                        (OTHER, 0, 0x45, 0)]

    @pytest.mark.parametrize("path", ["/TalkbackInPhysicalCR/value",
                                      f"{DEV}/TalkbackInPhysicalCR/value"])
    def test_talkback_in_physical_cr(self, router, backend, path):
        router.on_set(path, True)
        assert backend.mixer_params == [(OTHER, 0, 0x47, 1)]

    @pytest.mark.parametrize("path", ["/TalkbackMaster/value",
                                      f"{DEV}/TalkbackMaster/value"])
    def test_talkback_master_is_this_unit(self, router, backend, tree, path):
        # value is the order of the talkback master unit; this unit is 0
        router.on_set(path, 0)
        router.on_set(path, -1)
        router.on_set(path, 1)
        assert backend.mixer_params == [(OTHER, 0, 0x48, 1),
                                        (OTHER, 0, 0x48, 0),
                                        (OTHER, 0, 0x48, 0)]

    def test_talkback_master_follows_the_unit_order(self, router, backend,
                                                    tree):
        tree.set(f"{DEV}/MultiUnitOrder/value", 2)
        router.on_set(f"{DEV}/TalkbackMaster/value", 2)
        router.on_set(f"{DEV}/TalkbackMaster/value", 0)
        assert backend.mixer_params == [(OTHER, 0, 0x48, 1),
                                        (OTHER, 0, 0x48, 0)]

    @pytest.mark.parametrize("ch, param", [(0, 0x19), (1, 0x19),
                                           (2, 0x1A), (3, 0x1A)])
    def test_line_out_pads_are_per_pair(self, router, backend, ch, param):
        router.on_set(f"{OUT}/{ch}/Pad/value", True)
        router.on_set(f"{OUT}/{ch}/Pad/value", False)
        assert backend.mixer_params == [(OTHER, 0, param, 1),
                                        (OTHER, 0, param, 0)]

    def test_monitor_pad_is_still_output_ref(self, router, backend):
        router.on_set(f"{OUT}/18/Pad/value", True)
        assert backend.mixer_params == [(OTHER, 1, 0x32, 1)]

    def test_24db_mode(self, router, backend):
        router.on_set("/Enable24dBMode/value", True)
        assert backend.mixer_params == [(OTHER, 0, 0x49, 1)]

    def test_refused_param_is_not_retried(self, router, backend):
        # the x4 (kind 14) answers 0x49 with EINVAL
        import errno
        calls = []

        def refuse(ct, ci, param, value):
            calls.append(param)
            backend.last_errno = errno.EINVAL
            return False
        backend.set_mixer_param = refuse
        router.on_set("/Enable24dBMode/value", True)
        router.on_set("/Enable24dBMode/value", False)
        router.on_set(f"{DEV}/OutputsAttenuate/value", True)   # others go on
        assert calls == [0x49, 0x20]

    def test_other_errors_do_not_blacklist(self, router, backend):
        import errno
        calls = []

        def busy(ct, ci, param, value):
            calls.append(param)
            backend.last_errno = errno.EBUSY
            return False
        backend.set_mixer_param = busy
        router.on_set("/Enable24dBMode/value", True)
        router.on_set("/Enable24dBMode/value", False)
        assert calls == [0x49, 0x49]


# ── A11: CUE 3/4 on 4-cue models ────────────────────────────────────

class TestCueBusCount:
    def test_two_cue_models_enable_nothing_extra(self, router, backend):
        router.on_set(f"{DEV}/CueBusCount/value", 2)
        assert backend.mixer_params == []

    def test_four_cue_models_enable_buses_5_and_6(self, router, backend):
        router.on_set(f"{DEV}/CueBusCount/value", 4)
        assert backend.mixer_params == [(0, 5, 0x01, 1), (0, 6, 0x01, 1)]

    def test_three_cues_enable_bus_5_only(self, router, backend):
        router.on_set(f"{DEV}/CueBusCount/value", 3)
        assert backend.mixer_params == [(0, 5, 0x01, 1)]

    @pytest.mark.parametrize("send_idx, sub", [(4, 5), (5, 6)])
    def test_cue3_cue4_input_sends_use_sub_5_and_6(self, router, backend,
                                                   tree, send_idx, sub):
        router.on_set(f"{IN}/0/sends/{send_idx}/Gain/value", 0.0)
        assert backend.bus_params == [(0x00, sub, pytest.approx(0.7071067))]

    @pytest.mark.parametrize("send_idx, sub", [(2, 5), (3, 6)])
    def test_cue3_cue4_aux_sends_use_sub_5_and_6(self, router, backend,
                                                 send_idx, sub):
        router.on_set(f"/devices/0/auxs/0/sends/{send_idx}/Gain/value", -6.0)
        assert [c[:2] for c in backend.bus_params] == [(0x10, sub)]


class TestAuxMono:
    @pytest.mark.parametrize("aux, param", [(0, 0x0A), (1, 0x0F)])
    def test_aux_mix_to_mono(self, router, backend, aux, param):
        router.on_set(f"/devices/0/auxs/{aux}/MixToMono/value", True)
        assert backend.mixer_params == [(OTHER, 0, param, 1)]
