"""Bus / send / CUE / headphone routing against the Mixer Engine's tables.

Ground truth: apollo-linux/docs/re-notes/parity/04-engine-surface.md §0.1
(bus sub-params 0 mix, 1/2 CUE 1/2, 3/4 AUX 1/2, 5/6 CUE 3/4) and §4.
"""

import json

import pytest

from conftest import DEVICE_MAP_X4
from hardware import (_INPUT_IDX_TO_BUS, MONITOR_PARAM, build_input_bus_table,
                      bus_id_for_input_name, input_bus_id)

IN = "/devices/0/inputs"
OUT = "/devices/0/outputs"
AUX = "/devices/0/auxs"


def x4_input_names():
    names = {}
    for c in json.load(open(DEVICE_MAP_X4))["controls"]:
        p = c["path"].split("/")
        if len(p) == 6 and p[3] == "inputs" and p[5] == "Name":
            names[int(p[4])] = c["value"]
    return [names[i] for i in range(len(names))]


@pytest.fixture
def wired(router, backend, tree):
    """Router fed through the state tree, so cross-control reads work."""
    tree.on_set = router.on_set
    return tree


def subs(backend):
    return [(b, s) for b, s, _ in backend.bus_params]


class TestInputBusTable:
    def test_x4_map_order(self):
        # tree index -> bus: analog, ADAT 1-8, S/PDIF L/R, virtual, talkback
        assert list(_INPUT_IDX_TO_BUS[:22]) == [
            0x00, 0x01, 0x02, 0x03,
            0x0C, 0x0D, 0x0E, 0x0F, 0x14, 0x15, 0x16, 0x17,
            0x08, 0x09,
            0x18, 0x19, 0x1A, 0x1B, 0x1C, 0x1D, 0x1E, 0x1F]
        assert _INPUT_IDX_TO_BUS[22] == 0x0A      # TALKBACK
        assert _INPUT_IDX_TO_BUS[23] is None      # "N/A 1" placeholder

    def test_default_table_matches_the_device_map_names(self):
        names = x4_input_names()
        assert tuple(build_input_bus_table(names)) == _INPUT_IDX_TO_BUS

    def test_router_table_comes_from_the_loaded_map(self, router):
        assert router._input_buses() == _INPUT_IDX_TO_BUS

    def test_other_map_order_is_followed(self, backend):
        # A map that lists S/PDIF before ADAT gets its own table.
        from hardware import HardwareRouter
        from state_tree import StateTree
        t = StateTree()
        t.load_device_map(DEVICE_MAP_X4)
        for i, n in enumerate(["ANALOG 1", "S/PDIF L", "S/PDIF R", "ADAT 1"]):
            t.set(f"{IN}/{i}/Name/value", n)
        for i in range(4, 24):
            t.set(f"{IN}/{i}/Name/value", "N/A 1")
        r = HardwareRouter(backend, t)
        assert r._input_bus(1) == 0x08
        assert r._input_bus(3) == 0x0C
        assert r._input_bus(10) is None

    @pytest.mark.parametrize("name, bus", [
        ("ANALOG 4", 0x03), ("analog 1", 0x00), ("ADAT 5", 0x14),
        ("ADAT 8", 0x17), ("S/PDIF L", 0x08), ("S/PDIF R", 0x09),
        ("SPDIF R", 0x09), ("VIRTUAL 8", 0x1F), ("TALKBACK", 0x0A)])
    def test_names(self, name, bus):
        assert bus_id_for_input_name(name) == bus

    @pytest.mark.parametrize("name", [
        "N/A 1", "ANALOG 5", "ADAT 9", "VIRTUAL 0", "MADI 1", None, ""])
    def test_names_without_a_bus(self, name):
        assert bus_id_for_input_name(name) is None

    def test_input_bus_id_out_of_range(self):
        assert input_bus_id(99) is None

    @pytest.mark.parametrize("ch, bus", [
        (4, 0x0C), (7, 0x0F), (8, 0x14), (11, 0x17),   # ADAT
        (12, 0x08), (13, 0x09),                         # S/PDIF
        (14, 0x18), (21, 0x1F)])                        # virtual
    def test_fader_addresses_the_right_bus(self, router, backend, ch, bus):
        router.on_set(f"{IN}/{ch}/FaderLevel/value", -6.0)
        assert {b for b, _ in subs(backend)} == {bus}
        assert subs(backend)[0] == (bus, 0)

    def test_input_without_a_bus_writes_nothing(self, router, backend):
        router.on_set(f"{IN}/23/FaderLevel/value", -6.0)
        router.on_set(f"{IN}/23/Mute/value", True)
        assert backend.bus_params == []


UNITY = 0.7071067811865476      # encode_input_fader(0 dB)


class TestPostFaderSends:
    """Fader moves re-write the post-fader sends (engine: sub = send x fader)."""

    def test_default_fader_matches_the_macos_triplet(self, wired, backend):
        # Sends at 0 dB, AUX post-fader (map default): a fader change writes
        # sub 0, 3, 4 with the fader's linear value, as the IOKit capture.
        wired.set(f"{IN}/0/sends/0/Gain/value", 0.0)
        wired.set(f"{IN}/0/sends/1/Gain/value", 0.0)
        backend.clear()
        wired.set(f"{IN}/0/FaderLevel/value", -10.0)
        lin = 10 ** (-10 / 20) * UNITY
        assert backend.bus_params == [
            (0x00, 0, pytest.approx(lin)),
            (0x00, 3, pytest.approx(lin)),
            (0x00, 4, pytest.approx(lin))]

    def test_send_level_scales_with_fader(self, wired, backend):
        wired.set(f"{IN}/0/sends/0/Gain/value", -6.0)
        backend.clear()
        wired.set(f"{IN}/0/FaderLevel/value", -10.0)
        sub3 = [v for b, s, v in backend.bus_params if s == 3]
        assert sub3 == [pytest.approx(10 ** (-16 / 20) * UNITY)]

    def test_silent_send_stays_silent(self, wired, backend):
        backend.clear()
        wired.set(f"{IN}/0/FaderLevel/value", -10.0)
        assert [(s, v) for _, s, v in backend.bus_params] == [
            (0, pytest.approx(10 ** (-10 / 20) * UNITY)), (3, 0.0), (4, 0.0)]

    def test_cue_sends_are_pre_fader_by_default(self, wired, backend):
        wired.set(f"{IN}/0/sends/2/Gain/value", 0.0)
        backend.clear()
        wired.set(f"{IN}/0/FaderLevel/value", -10.0)
        assert 1 not in {s for _, s, _ in backend.bus_params}

    def test_cue_send_follows_hp_send_post_fader(self, wired, backend):
        wired.set("/devices/0/HP1SendPostFader/value", True)
        wired.set(f"{IN}/0/sends/2/Gain/value", 0.0)
        backend.clear()
        wired.set(f"{IN}/0/FaderLevel/value", -10.0)
        sub1 = [v for b, s, v in backend.bus_params if s == 1]
        assert sub1 == [pytest.approx(10 ** (-10 / 20) * UNITY)]

    def test_aux_pre_fader_setting(self, wired, backend):
        wired.set("/devices/0/auxs/0/SendPostFader/value", False)
        wired.set(f"{IN}/0/sends/0/Gain/value", 0.0)
        backend.clear()
        wired.set(f"{IN}/0/FaderLevel/value", -10.0)
        assert 3 not in {s for _, s, _ in backend.bus_params}
        assert 4 in {s for _, s, _ in backend.bus_params}   # AUX 2 still post

    def test_override_beats_the_bus_setting(self, wired, backend):
        wired.set(f"{IN}/0/sends/0/PostFaderOverride/value", 0)    # pre
        wired.set(f"{IN}/0/sends/2/PostFaderOverride/value", 1)    # post
        wired.set(f"{IN}/0/sends/0/Gain/value", 0.0)
        wired.set(f"{IN}/0/sends/2/Gain/value", 0.0)
        backend.clear()
        wired.set(f"{IN}/0/FaderLevel/value", -10.0)
        written = {s for _, s, _ in backend.bus_params}
        assert 3 not in written and 1 in written

    def test_mute_zeroes_post_fader_sends(self, wired, backend):
        wired.set(f"{IN}/0/sends/0/Gain/value", 0.0)
        backend.clear()
        wired.set(f"{IN}/0/Mute/value", True)
        assert backend.bus_params == [(0x00, 0, 0.0), (0x00, 3, 0.0),
                                      (0x00, 4, 0.0)]

    def test_fader_while_muted_keeps_sends_silent(self, wired, backend):
        wired.set(f"{IN}/0/sends/0/Gain/value", 0.0)
        wired.set(f"{IN}/0/Mute/value", True)
        backend.clear()
        wired.set(f"{IN}/0/FaderLevel/value", -3.0)
        assert [v for _, s, v in backend.bus_params if s == 3] == [0.0]

    def test_bypassed_send_stays_zero_on_fader_move(self, wired, backend):
        wired.set(f"{IN}/0/sends/0/Gain/value", 0.0)
        wired.set(f"{IN}/0/sends/0/Bypass/value", True)
        backend.clear()
        wired.set(f"{IN}/0/FaderLevel/value", -3.0)
        assert [v for _, s, v in backend.bus_params if s == 3] == [0.0]

    def test_send_gain_includes_the_fader_when_post(self, wired, backend):
        wired.set(f"{IN}/0/FaderLevel/value", -10.0)
        backend.clear()
        wired.set(f"{IN}/0/sends/0/Gain/value", -4.0)
        assert backend.bus_params == [
            (0x00, 3, pytest.approx(10 ** (-14 / 20) * UNITY))]

    def test_send_gain_ignores_the_fader_when_pre(self, wired, backend):
        wired.set(f"{IN}/0/FaderLevel/value", -10.0)
        backend.clear()
        wired.set(f"{IN}/0/sends/2/Gain/value", -4.0)
        assert backend.bus_params == [
            (0x00, 1, pytest.approx(10 ** (-4 / 20) * UNITY))]

    def test_send_gain_when_muted_post_is_zero(self, wired, backend):
        wired.set(f"{IN}/0/Mute/value", True)
        backend.clear()
        wired.set(f"{IN}/0/sends/0/Gain/value", 0.0)
        assert backend.bus_params == [(0x00, 3, 0.0)]

    def test_bypass_off_restores_with_the_fader(self, wired, backend):
        wired.set(f"{IN}/0/FaderLevel/value", -10.0)
        wired.set(f"{IN}/0/sends/0/Gain/value", -4.0)
        wired.set(f"{IN}/0/sends/0/Bypass/value", True)
        backend.clear()
        wired.set(f"{IN}/0/sends/0/Bypass/value", False)
        assert backend.bus_params == [
            (0x00, 3, pytest.approx(10 ** (-14 / 20) * UNITY))]

    def test_fader_uses_the_value_just_set_for_tapered(self, wired, backend):
        wired.set(f"{IN}/0/sends/0/Gain/value", 0.0)
        backend.clear()
        wired.set(f"{IN}/0/FaderLevelTapered/value", 0.0)   # -inf
        assert [v for _, s, v in backend.bus_params if s == 3] == [0.0]

    def test_pan_writes_sub0_only(self, router, backend):
        router.on_set(f"{IN}/0/Pan/value", 0.5)
        assert subs(backend) == [(0x00, 0)]

    def test_talkback_fader_sub0_only(self, router, backend):
        router.on_set(f"{IN}/22/FaderLevel/value", 0.0)
        assert backend.bus_params == [(0x0A, 0, pytest.approx(3.976353))]

    def test_solo_recalc_rewrites_post_fader_sends(self, wired, backend):
        wired.set(f"{IN}/0/sends/0/Gain/value", 0.0)
        wired.set(f"{IN}/0/FaderLevel/value", 0.0)
        backend.clear()
        wired.set(f"{IN}/1/Solo/value", True)       # input 0 goes silent
        sub3 = [v for b, s, v in backend.bus_params if b == 0x00 and s == 3]
        assert sub3 == [0.0]
        backend.clear()
        wired.set(f"{IN}/1/Solo/value", False)
        sub3 = [v for b, s, v in backend.bus_params if b == 0x00 and s == 3]
        assert sub3 == [pytest.approx(UNITY)]

    def test_solo_recalc_keeps_pan_in_the_mix_coefficient(self, wired,
                                                          backend):
        wired.set(f"{IN}/0/FaderLevel/value", 0.0)
        wired.set(f"{IN}/0/Pan/value", 1.0)
        backend.clear()
        wired.set(f"{IN}/5/Solo/value", True)      # silences input 0
        wired.set(f"{IN}/5/Solo/value", False)     # restores it
        restored = [v for b, s, v in backend.bus_params
                    if b == 0x00 and s == 0][-1]
        # hard right at 0 dB: cos(pi/2) ~ 0 (the pan law), not centre gain
        assert restored == pytest.approx(0.0, abs=1e-6)
        wired.set(f"{IN}/0/Pan/value", 0.0)
        backend.clear()
        wired.set(f"{IN}/5/Solo/value", True)
        wired.set(f"{IN}/5/Solo/value", False)
        restored = [v for b, s, v in backend.bus_params
                    if b == 0x00 and s == 0][-1]
        assert restored == pytest.approx(UNITY)

    def test_solo_recalc_talkback_uses_its_own_encoding(self, wired,
                                                        backend):
        wired.set(f"{IN}/22/FaderLevel/value", 0.0)
        backend.clear()
        wired.set(f"{IN}/1/Solo/value", True)
        wired.set(f"{IN}/1/Solo/value", False)
        tb = [v for b, s, v in backend.bus_params if b == 0x0A]
        assert tb[0] == 0.0
        assert tb[-1] == pytest.approx(3.976353)


class TestInputSends:
    @pytest.mark.parametrize("send, sub", [
        (0, 3), (1, 4),              # AUX 1, AUX 2
        (2, 1), (3, 2),              # CUE 1, CUE 2
        (4, 5), (5, 6)])             # CUE 3, CUE 4
    def test_send_sub_param(self, router, backend, send, sub):
        router.on_set(f"{IN}/12/sends/{send}/Gain/value", 0.0)
        assert backend.bus_params == [(0x08, sub, pytest.approx(0.7071067))]

    def test_send_does_not_touch_the_mix(self, router, backend):
        router.on_set(f"{IN}/4/sends/0/Gain/value", -20.0)
        assert subs(backend) == [(0x0C, 3)]

    def test_send_follows_the_map_id_not_the_index(self, wired, backend):
        wired.set(f"{IN}/0/sends/0/ID/value", "cue2")
        wired.set(f"{IN}/0/sends/0/Gain/value", 0.0)
        assert subs(backend) == [(0x00, 2)]

    def test_unknown_send_is_ignored(self, router, backend):
        router.on_set(f"{IN}/0/sends/9/Gain/value", 0.0)
        assert backend.bus_params == []

    def test_bypass_zeroes_then_restores(self, wired, backend):
        wired.set(f"{IN}/0/sends/2/Gain/value", -6.0)
        backend.clear()
        wired.set(f"{IN}/0/sends/2/Bypass/value", True)
        assert backend.bus_params == [(0x00, 1, 0.0)]
        backend.clear()
        wired.set(f"{IN}/0/sends/2/Bypass/value", False)
        [(bus, sub, v)] = backend.bus_params
        assert (bus, sub) == (0x00, 1)
        assert v == pytest.approx(10 ** (-6 / 20) * 0.7071067811865476)

    def test_gain_while_bypassed_is_not_written(self, wired, backend):
        wired.set(f"{IN}/0/sends/2/Bypass/value", True)
        backend.clear()
        wired.set(f"{IN}/0/sends/2/Gain/value", -3.0)
        assert backend.bus_params == []


class TestAuxSends:
    @pytest.mark.parametrize("aux, bus", [(0, 0x10), (1, 0x12)])
    @pytest.mark.parametrize("send, sub", [(0, 1), (1, 2), (2, 5), (3, 6)])
    def test_aux_to_cue_send(self, router, backend, aux, bus, send, sub):
        router.on_set(f"{AUX}/{aux}/sends/{send}/Gain/value", 0.0)
        assert backend.bus_params == [(bus, sub, pytest.approx(1.0))]

    def test_aux_send_never_writes_the_aux_fader(self, router, backend):
        for send in range(4):
            router.on_set(f"{AUX}/0/sends/{send}/Gain/value", -10.0)
        assert 0 not in {s for _, s in subs(backend)}

    def test_aux_fader_is_still_sub0(self, router, backend):
        router.on_set(f"{AUX}/0/FaderLevel/value", -10.0)
        assert subs(backend) == [(0x10, 0)]

    def test_aux_send_bypass(self, wired, backend):
        wired.set(f"{AUX}/1/sends/1/Gain/value", -20.0)
        backend.clear()
        wired.set(f"{AUX}/1/sends/1/Bypass/value", True)
        assert backend.bus_params == [(0x12, 2, 0.0)]
        backend.clear()
        wired.set(f"{AUX}/1/sends/1/Bypass/value", False)
        assert backend.bus_params == [(0x12, 2, pytest.approx(0.1))]


class TestCueOutputs:
    def mixer(self, backend, param):
        return [c for c in backend.mixer_params if c[2] == param]

    @pytest.mark.parametrize("ch, mono, mix", [(14, 0x06, 0x05),
                                               (15, 0x08, 0x07)])
    def test_mono_and_mix_go_to_the_right_cue(self, router, backend, ch,
                                              mono, mix):
        router.on_set(f"{OUT}/{ch}/MixToMono/value", True)
        router.on_set(f"{OUT}/{ch}/MixInSource/value", "cue")
        router.on_set(f"{OUT}/{ch}/MixInSource/value", "mon")
        assert backend.mixer_params == [
            (2, 0, mono, 1), (2, 0, mix, 0), (2, 0, mix, 2)]

    def test_cue2_is_output_15_not_16(self, router, backend):
        router.on_set(f"{OUT}/16/MixToMono/value", True)
        router.on_set(f"{OUT}/17/MixToMono/value", True)
        router.on_set(f"{OUT}/16/OutputDestination/value", "S/PDIF")
        assert backend.mixer_params == []     # CUE 3/4: no verified param

    @pytest.mark.parametrize("ch, src, en", [(14, 0x2E, 0x3B),
                                             (15, 0x2F, 0x3C)])
    def test_mirror(self, router, backend, ch, src, en):
        router.on_set(f"{OUT}/{ch}/OutputDestination/value", "ADAT 1-2")
        assert backend.mixer_params == [(2, 10, src, 16), (2, 10, en, 1)]
        backend.clear()
        router.on_set(f"{OUT}/{ch}/OutputDestination/value", "None")
        assert backend.mixer_params == [(2, 10, en, 0),
                                        (2, 10, src, 0xFFFFFFFF)]

    def test_cue_roles_follow_the_map_not_fixed_indices(self, router):
        assert [router._output_role(c) for c in (14, 15, 16, 17)] == [
            ("cue", 0), ("cue", 1), ("cue", 2), ("cue", 3)]
        assert router._output_role(18) == ("monitor", 0)
        assert router._output_role(19) == ("hp", 0)
        assert router._output_role(20) == ("hp", 1)
        assert router._output_role(0) is None
        assert router._output_role(21) is None   # monitor R for surround


class TestHeadphoneLevel:
    def test_hp1_uses_param_0x02(self, router, backend):
        router.on_set(f"{OUT}/19/CRMonitorLevel/value", -10.0)
        assert backend.mixer_params == [(2, 1, 0x02, 172)]

    def test_hp2_uses_param_0x66(self, router, backend):
        router.on_set(f"{OUT}/20/CRMonitorLevel/value", -10.0)
        assert backend.mixer_params == [(2, 1, 0x66, 172)]

    def test_tapered(self, router, backend):
        router.on_set(f"{OUT}/19/CRMonitorLevelTapered/value", 1.0)
        [(t, i, p, v)] = backend.mixer_params
        assert (t, i, p) == (2, 1, 0x02)
        assert v == 0xC0

    def test_monitor_level_unchanged(self, router, backend):
        router.on_set(f"{OUT}/18/CRMonitorLevel/value", -10.0)
        assert backend.mixer_params == [(2, 1, 0x01, 172)]

    def test_hp_sources_unchanged(self, router, backend):
        router.on_set(f"{OUT}/19/MixInSource/value", "cue2")
        router.on_set(f"{OUT}/20/MixInSource/value", "cue2")
        assert backend.mixer_params == [(2, 1, 0x3F, 1), (2, 1, 0x40, 1)]

    def test_param_ids(self):
        assert MONITOR_PARAM["HP1Level"] == 0x02
        assert MONITOR_PARAM["HP2Level"] == 0x66
