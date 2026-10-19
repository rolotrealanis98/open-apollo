"""Link (Stereo) on an x4 preamp pair, as UA's engine applies it
(CMxInput::SetProperty case 0x45, UpdateHardwareState, _UpdateDSPBusParameters
isSecond): the right leg follows the leader, its bus is driven by the
leader's fader / mute / solo / pans / sends, its preamp is ganged, and an
unlink gives it back.  Fake backend, no sockets."""

import pytest

from conftest import make_readback
from hardware import (ARM_PARAM_IDS, LINK_PARAM, PREAMP_CHANNEL_TYPE as PRE,
                      encode_mix_coeff, encode_mix_coeff_r,
                      preamp_db_to_tapered)
from ua_mixer_daemon import MixerDaemon

IN = "/devices/0/inputs"
SUB_AUX1 = 3


@pytest.fixture
def wired(tree, router):
    tree.on_set = router.on_set
    return tree


def v(tree, path):
    return tree.get_value(f"{IN}/{path}/value")


def last(entries, bus, sub=0):
    vals = [x for b, s, x in entries if (b, s) == (bus, sub)]
    return vals[-1] if vals else None


def links(backend):
    return [p for p in backend.mixer_params if p[2] == LINK_PARAM]


@pytest.mark.parametrize("ch", [0, 1])
def test_link_from_either_leg(wired, router, backend, ch):
    wired.set(f"{IN}/{ch}/Stereo/value", True)
    assert links(backend) == [(PRE, 0, LINK_PARAM, 1)]
    assert v(wired, "0/Stereo") is True and v(wired, "1/Stereo") is True
    assert v(wired, "1/Active") is False
    assert (v(wired, "0/Pan"), v(wired, "0/Pan2")) == (-1.0, 1.0)
    assert router._pair_follower(1)


def test_right_leg_bus_follows_the_leader_not_its_own_mute(wired, backend):
    wired.set(f"{IN}/1/Mute/value", True)        # the right input's own
    wired.set(f"{IN}/0/Mute/value", True)
    wired.set(f"{IN}/0/Stereo/value", True)
    wired.set(f"{IN}/0/FaderLevel/value", -6.0)
    backend.clear()
    wired.set(f"{IN}/0/Mute/value", False)       # unmute the linked strip
    # bus 1 carries the leader's level at Pan2 +1, not input 1's mute
    assert last(backend.bus_params_r, 0x01) == pytest.approx(
        encode_mix_coeff_r(-6.0, 1.0))
    assert last(backend.bus_params, 0x00) == pytest.approx(
        encode_mix_coeff(-6.0, -1.0))
    # the right input's own controls no longer reach its bus
    backend.clear()
    wired.set(f"{IN}/1/FaderLevel/value", -30.0)
    wired.set(f"{IN}/1/Mute/value", False)
    wired.set(f"{IN}/1/Pan/value", 0.0)
    assert last(backend.bus_params_r, 0x01) is None


def test_preamp_is_ganged(wired, backend):
    wired.set(f"{IN}/0/preamps/0/48V/value", True)
    wired.set(f"{IN}/0/Stereo/value", True)
    # the link re-pushes the leader's preamp on the right channel
    assert (PRE, 1, ARM_PARAM_IDS["48V"], 1) in backend.mixer_params
    assert v(wired, "1/preamps/0/48V") is True
    backend.clear()
    wired.set(f"{IN}/0/preamps/0/Gain/value", 40.0)
    for ch in (0, 1):
        assert (PRE, ch, 0x06, 31) in backend.mixer_params
    assert v(wired, "1/preamps/0/Gain") == 40.0
    assert v(wired, "1/preamps/0/GainTapered") == pytest.approx(
        preamp_db_to_tapered(40))
    for control in ("Pad", "LowCut", "Phase"):
        backend.clear()
        wired.set(f"{IN}/0/preamps/0/{control}/value", True)
        assert (PRE, 1, ARM_PARAM_IDS[control], 1) in backend.mixer_params
    backend.clear()
    wired.set(f"{IN}/0/IOType/value", "Line")
    assert (PRE, 1, 0x00, 1) in backend.mixer_params
    assert v(wired, "1/IOType") == "Line"
    # an unlink keeps the ganged values on the right preamp
    wired.set(f"{IN}/0/Stereo/value", False)
    assert v(wired, "1/preamps/0/Gain") == 40.0


def test_sends_drive_both_buses(wired, router, backend):
    wired.set(f"{IN}/0/Stereo/value", True)
    backend.clear()
    wired.set(f"{IN}/0/sends/0/Gain/value", -6.0)
    left = [x for b, s, x in backend.bus_params if s == SUB_AUX1]
    assert {b for b, s, _ in backend.bus_params if s == SUB_AUX1} == {0, 1}
    assert left[0] == pytest.approx(left[1]) and left[0] > 0
    # the follower's own sends are ignored while linked
    backend.clear()
    wired.set(f"{IN}/1/sends/0/Gain/value", 0.0)
    assert backend.bus_params == []


def test_unlink_gives_the_right_leg_back(wired, router, backend):
    wired.set(f"{IN}/0/Stereo/value", True)
    wired.set(f"{IN}/1/FaderLevel/value", -10.0)
    wired.set(f"{IN}/1/Mute/value", False)
    wired.set(f"{IN}/1/sends/0/Gain/value", 0.0)
    backend.clear()
    wired.set(f"{IN}/0/Stereo/value", False)
    assert links(backend) == [(PRE, 0, LINK_PARAM, 0)]
    assert v(wired, "1/Stereo") is False and v(wired, "1/Active") is True
    for ch in (0, 1):
        assert (v(wired, f"{ch}/Pan"), v(wired, f"{ch}/Pan2")) == (0.0, 0.0)
    # bus 1 from input 1's own fader (centre) and its own send
    assert last(backend.bus_params, 0x01) == pytest.approx(
        encode_mix_coeff(-10.0, 0.0))
    assert last(backend.bus_params, 0x01, SUB_AUX1) == pytest.approx(
        router._input_send_linear(1, 0))


def test_unlink_from_the_right_leg(wired):
    wired.set(f"{IN}/0/Stereo/value", True)
    wired.set(f"{IN}/1/Stereo/value", False)
    assert v(wired, "0/Stereo") is False and v(wired, "1/Active") is True


def test_link_while_paused_updates_the_layout_only(wired, router, backend):
    router.set_paused(True)
    wired.set(f"{IN}/0/Stereo/value", True)
    assert v(wired, "1/Active") is False
    assert backend.mixer_params == [] and backend.bus_params == []


class TestDaemon:
    def test_meters_1_follows_a_runtime_link(self, wired, router):
        class Meter:
            channel_ids = [0x0001, 0x0002, 0x0003, 0x0004]

            def get_channel_meter(self, cap):
                return ((-20.0, -9.0, -30.0, -30.0)[cap], -3.0, False)

        d = MixerDaemon(wired, router, metering=Meter())
        path = f"{IN}/0/meters/1/MeterLevel/value"
        d._read_meter(d._parse_meter_path(path))       # cached while mono
        wired.set(f"{IN}/0/Stereo/value", True)
        assert d._read_meter(d._parse_meter_path(path)) == -9.0

    def test_front_panel_link_parks_the_right_leg(self, wired, router,
                                                  backend):
        d = MixerDaemon(wired, router)
        backend.readback = (1, make_readback(flags={0: {"link"},
                                                    1: {"link"}}))
        d._sync_hw_readback(router.poll_hw_readback(), {})
        assert v(wired, "0/Stereo") is True
        assert v(wired, "1/Stereo") is True and v(wired, "1/Active") is False
        assert v(wired, "0/Pan") == -1.0
        # the device linked itself: no link write back, buses rewritten
        assert links(backend) == []
        assert last(backend.bus_params_r, 0x01) is not None
