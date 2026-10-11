"""--stereo-sends (side-1 entries of sends / talkback) and the solo-mute flag."""

import math

import pytest

from hardware import (HardwareRouter, UA_FEAT_BUS_PARAM_SIDE,
                      UA_FEAT_BUS_SOLO_FLAG, encode_input_fader,
                      encode_mix_coeff, encode_mix_coeff_r)

IN = "/devices/0/inputs"
AUX = "/devices/0/auxs"
UNITY = 1 / math.sqrt(2)


@pytest.fixture
def stereo(backend, tree):
    r = HardwareRouter(backend, tree, stereo_sends=True)
    tree.on_set = r.on_set
    return r


@pytest.fixture
def wired(router, tree):
    tree.on_set = router.on_set
    return tree


def left(backend, bus, sub):
    return [v for b, s, v in backend.bus_params if (b, s) == (bus, sub)]


def right(backend, bus, sub):
    return [v for b, s, v in backend.bus_params_r if (b, s) == (bus, sub)]


class TestStereoSends:
    def test_default_is_left_only(self, wired, backend):
        wired.set(f"{IN}/0/sends/0/Gain/value", 0.0)
        wired.set(f"{AUX}/0/sends/0/Gain/value", 0.0)
        assert backend.bus_params_r == []

    def test_flag_default_off(self, router):
        assert router.stereo_sends is False

    def test_input_send_without_pan_is_r_equals_l(self, stereo, tree, backend):
        tree.set(f"{IN}/0/sends/0/Pan/value", None)
        backend.clear()
        tree.set(f"{IN}/0/sends/0/Gain/value", -4.0)
        want = encode_input_fader(-4.0)
        assert left(backend, 0x00, 3) == [pytest.approx(want)]
        assert right(backend, 0x00, 3) == [pytest.approx(want)]

    def test_centre_pan_equals_the_unpanned_value(self, stereo, tree, backend):
        tree.set(f"{IN}/0/sends/0/Gain/value", -4.0)
        want = encode_input_fader(-4.0)
        assert left(backend, 0x00, 3)[-1] == pytest.approx(want)
        assert right(backend, 0x00, 3)[-1] == pytest.approx(want)

    def test_pan_uses_the_sub0_law(self, stereo, tree, backend):
        tree.set(f"{IN}/0/sends/0/Pan/value", -1.0)       # hard left
        backend.clear()
        tree.set(f"{IN}/0/sends/0/Gain/value", 0.0)
        # linear x sqrt2 x cos / sin, the same law as encode_mix_coeff(_r)
        assert left(backend, 0x00, 3) == [
            pytest.approx(encode_mix_coeff(0.0, -1.0) * math.sqrt(2) * UNITY)]
        assert left(backend, 0x00, 3) == [pytest.approx(1.0)]
        assert right(backend, 0x00, 3) == [pytest.approx(0.0, abs=1e-9)]
        assert encode_mix_coeff_r(0.0, -1.0) == pytest.approx(0.0, abs=1e-9)

    def test_pan_is_power_preserving(self, stereo, tree, backend):
        tree.set(f"{IN}/0/sends/0/Pan/value", 0.4)
        backend.clear()
        tree.set(f"{IN}/0/sends/0/Gain/value", -6.0)
        l, = left(backend, 0x00, 3)
        r, = right(backend, 0x00, 3)
        assert l * l + r * r == pytest.approx(2 * encode_input_fader(-6.0) ** 2)

    def test_pan_change_rewrites_the_pair(self, stereo, tree, backend):
        tree.set(f"{IN}/0/sends/0/Gain/value", 0.0)
        backend.clear()
        tree.set(f"{IN}/0/sends/0/Pan/value", 1.0)       # hard right
        assert left(backend, 0x00, 3) == [pytest.approx(0.0, abs=1e-9)]
        assert right(backend, 0x00, 3) == [pytest.approx(1.0)]

    def test_pan_change_is_ignored_when_bypassed(self, stereo, tree, backend):
        tree.set(f"{IN}/0/sends/0/Gain/value", 0.0)
        tree.set(f"{IN}/0/sends/0/Bypass/value", True)
        backend.clear()
        tree.set(f"{IN}/0/sends/0/Pan/value", 1.0)
        assert backend.bus_params == [] and backend.bus_params_r == []

    def test_pan_change_without_stereo_writes_nothing(self, wired, backend):
        wired.set(f"{IN}/0/sends/0/Gain/value", 0.0)
        backend.clear()
        wired.set(f"{IN}/0/sends/0/Pan/value", 1.0)
        assert backend.bus_params == [] and backend.bus_params_r == []

    def test_cue_sends_get_both_sides(self, stereo, tree, backend):
        # input send 2 is CUE 1 (sub 1) on the x4 map
        tree.set(f"{IN}/0/sends/2/Gain/value", -4.0)
        assert right(backend, 0x00, 1) == [pytest.approx(
            encode_input_fader(-4.0))]

    def test_bypass_zeroes_both_sides(self, stereo, tree, backend):
        tree.set(f"{IN}/0/sends/0/Gain/value", 0.0)
        backend.clear()
        tree.set(f"{IN}/0/sends/0/Bypass/value", True)
        assert left(backend, 0x00, 3) == [0.0]
        assert right(backend, 0x00, 3) == [0.0]

    def test_post_fader_rewrite_has_both_sides(self, stereo, tree, backend):
        tree.set(f"{IN}/0/sends/0/Gain/value", 0.0)
        backend.clear()
        tree.set(f"{IN}/0/FaderLevel/value", -10.0)
        assert right(backend, 0x00, 3) == [pytest.approx(
            encode_input_fader(-10.0))]

    def test_aux_to_cue_send_is_r_equals_l(self, stereo, tree, backend):
        tree.set(f"{AUX}/0/sends/0/Gain/value", -3.0)
        sub = 1
        bus = 0x10
        assert left(backend, bus, sub) == [pytest.approx(10 ** (-3 / 20))]
        assert right(backend, bus, sub) == left(backend, bus, sub)

    def test_aux_send_bypass(self, stereo, tree, backend):
        tree.set(f"{AUX}/0/sends/0/Gain/value", -3.0)
        backend.clear()
        tree.set(f"{AUX}/0/sends/0/Bypass/value", True)
        assert left(backend, 0x10, 1) == [0.0]
        assert right(backend, 0x10, 1) == [0.0]

    def test_talkback_fader(self, stereo, backend):
        stereo.on_set(f"{IN}/22/FaderLevel/value", 0.0)
        assert backend.bus_params == [(0x0A, 0, pytest.approx(3.976353))]
        assert backend.bus_params_r == [(0x0A, 0, pytest.approx(3.976353))]

    def test_talkback_fader_default_left_only(self, router, backend):
        router.on_set(f"{IN}/22/FaderLevel/value", 0.0)
        assert backend.bus_params_r == []

    def test_talkback_in_solo_recalc(self, stereo, tree, backend):
        tree.set(f"{IN}/22/FaderLevel/value", 0.0)
        tree.set(f"{IN}/22/Isolate/value", False)   # the capture isolates it
        backend.clear()
        tree.set(f"{IN}/1/Solo/value", True)        # talkback goes silent
        assert [v for b, s, v in backend.bus_params_r if b == 0x0A][0] == 0.0

    def test_sends_do_not_touch_sub0(self, stereo, tree, backend):
        tree.set(f"{IN}/0/sends/0/Gain/value", 0.0)
        assert all(s != 0 for _, s, _ in backend.bus_params_r)


class TestSoloMuteFlag:
    def solo_on(self, router, backend, tree, features):
        backend.features = features
        tree.on_set = router.on_set
        tree.set(f"{IN}/0/FaderLevel/value", -6.0)
        tree.set(f"{IN}/0/sends/0/Gain/value", 0.0)
        backend.clear()
        tree.set(f"{IN}/1/Solo/value", True)        # input 0 is muted by it

    def writes(self, backend, bus):
        """(side, solo_mute) of the sub-0 writes to `bus`, in order."""
        return [(side, flag) for b, s, side, flag in backend.bus_flags
                if b == bus and s == 0]

    def test_feature_writes_normal_coefficients_with_the_flag(
            self, router, backend, tree):
        self.solo_on(router, backend, tree, UA_FEAT_BUS_SOLO_FLAG)
        assert self.writes(backend, 0x00) == [(0, True), (1, True)]
        assert left(backend, 0x00, 0) == [pytest.approx(
            encode_mix_coeff(-6.0, 0.0))]
        assert right(backend, 0x00, 0) == [pytest.approx(
            encode_mix_coeff_r(-6.0, 0.0))]

    def test_pan_is_kept_in_the_flagged_coefficients(
            self, router, backend, tree):
        tree.set(f"{IN}/0/Pan/value", 0.5)
        self.solo_on(router, backend, tree, UA_FEAT_BUS_SOLO_FLAG)
        assert left(backend, 0x00, 0) == [pytest.approx(
            encode_mix_coeff(-6.0, 0.5))]

    def test_without_the_feature_still_writes_zero(
            self, router, backend, tree):
        self.solo_on(router, backend, tree, UA_FEAT_BUS_PARAM_SIDE)
        assert left(backend, 0x00, 0) == [0.0]
        assert right(backend, 0x00, 0) == [0.0]
        assert self.writes(backend, 0x00) == [(0, False), (1, False)]

    def test_soloed_input_is_never_flagged(self, router, backend, tree):
        self.solo_on(router, backend, tree, UA_FEAT_BUS_SOLO_FLAG)
        # input 1 (bus 0x01) is the soloed one
        assert self.writes(backend, 0x01) == [(0, False), (1, False)]

    def test_own_mute_is_zero_not_flagged(self, router, backend, tree):
        backend.features = UA_FEAT_BUS_SOLO_FLAG
        tree.on_set = router.on_set
        tree.set(f"{IN}/0/Mute/value", True)
        backend.clear()
        tree.set(f"{IN}/1/Solo/value", True)
        assert left(backend, 0x00, 0) == [0.0]
        assert self.writes(backend, 0x00) == [(0, False), (1, False)]

    def test_unsolo_writes_the_coefficients_without_the_flag(
            self, router, backend, tree):
        self.solo_on(router, backend, tree, UA_FEAT_BUS_SOLO_FLAG)
        backend.clear()
        tree.set(f"{IN}/1/Solo/value", False)
        assert self.writes(backend, 0x00) == [(0, False), (1, False)]
        assert left(backend, 0x00, 0) == [pytest.approx(
            encode_mix_coeff(-6.0, 0.0))]

    def test_sends_of_a_solo_muted_input_stay_zero(
            self, router, backend, tree):
        self.solo_on(router, backend, tree, UA_FEAT_BUS_SOLO_FLAG)
        assert left(backend, 0x00, 3) == [0.0]

    def test_no_solo_no_flags(self, router, backend, tree):
        backend.features = UA_FEAT_BUS_SOLO_FLAG
        tree.on_set = router.on_set
        tree.set(f"{IN}/0/FaderLevel/value", -6.0)
        assert not any(f for *_, f in backend.bus_flags)
