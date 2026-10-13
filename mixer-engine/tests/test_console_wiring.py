"""Hardware routes behind Console controls that reached the tree but not the
Apollo: send pre/post (PostFaderOverride, an aux bus's PRE / POST, the CUE
defaults), the root CUE BUS COUNT, the talkback mic at init (TALK), and the
value a refused clock SET leaves in effect.  Fake backend, no sockets."""

import json

import pytest

import hardware
from conftest import FakeBackend
from hardware import (DRIVER_PARAM_SAMPLE_RATE, HardwareRouter,
                      encode_input_fader)
from protocol import Command
from ua_mixer_daemon import MixerClient, MixerDaemon

IN = "/devices/0/inputs"
AUX = "/devices/0/auxs"
TALKBACK_PARAM, TB_CONFIG = 0x46, 0x47
SUB_AUX1, SUB_CUE1, SUB_CUE2 = 3, 1, 2


def unmuted(tree, ch, fader_db=-10.0):
    tree.set_value(f"{IN}/{ch}/Mute/value", False)
    tree.set_value(f"{IN}/{ch}/FaderLevel/value", fader_db)


def sends(backend, bus, sub):
    return [v for b, s, v in backend.bus_params if (b, s) == (bus, sub)]


class TestSendPrePost:
    """A post-fader send carries the fader (the engine adds it in dB), a
    pre-fader one does not: switching must rewrite the coefficient."""

    def test_override_post_adds_the_fader(self, router, backend, tree):
        unmuted(tree, 0, -10.0)
        tree.set_value(f"{IN}/0/sends/0/Gain/value", 0.0)
        tree.set_value(f"{IN}/0/sends/0/PostFaderOverride/value", 1)
        router.on_set(f"{IN}/0/sends/0/PostFaderOverride/value", 1)
        assert sends(backend, 0, SUB_AUX1) == [
            pytest.approx(encode_input_fader(-10.0))]

    def test_override_pre_ignores_fader_and_mute(self, router, backend, tree):
        unmuted(tree, 0, -10.0)
        tree.set_value(f"{IN}/0/Mute/value", True)
        tree.set_value(f"{IN}/0/sends/0/Gain/value", -6.0)
        tree.set_value(f"{IN}/0/sends/0/PostFaderOverride/value", 0)
        router.on_set(f"{IN}/0/sends/0/PostFaderOverride/value", 0)
        assert sends(backend, 0, SUB_AUX1) == [
            pytest.approx(encode_input_fader(-6.0))]

    def test_post_send_of_a_muted_input_is_silent(self, router, backend,
                                                  tree):
        unmuted(tree, 0)
        tree.set_value(f"{IN}/0/Mute/value", True)
        tree.set_value(f"{IN}/0/sends/0/Gain/value", 0.0)
        tree.set_value(f"{IN}/0/sends/0/PostFaderOverride/value", 1)
        router.on_set(f"{IN}/0/sends/0/PostFaderOverride/value", 1)
        assert sends(backend, 0, SUB_AUX1) == [0.0]

    def test_bypassed_send_stays_silent(self, router, backend, tree):
        unmuted(tree, 0)
        tree.set_value(f"{IN}/0/sends/0/Bypass/value", True)
        tree.set_value(f"{IN}/0/sends/0/Gain/value", 0.0)
        router.on_set(f"{IN}/0/sends/0/PostFaderOverride/value", 0)
        assert sends(backend, 0, SUB_AUX1) == [0.0]

    def test_aux_pre_post_rewrites_the_sends_that_follow_it(
            self, router, backend, tree):
        for ch in (0, 1):
            unmuted(tree, ch, -10.0)
            tree.set_value(f"{IN}/{ch}/sends/0/Gain/value", 0.0)
        # input 1 has its own setting (pre): not rewritten
        tree.set_value(f"{IN}/1/sends/0/PostFaderOverride/value", 0)
        tree.set_value(f"{AUX}/0/SendPostFader/value", False)
        router.on_set(f"{AUX}/0/SendPostFader/value", False)
        assert sends(backend, 0, SUB_AUX1) == [
            pytest.approx(encode_input_fader(0.0))]
        assert sends(backend, 1, SUB_AUX1) == []
        backend.clear()
        tree.set_value(f"{AUX}/0/SendPostFader/value", True)
        router.on_set(f"{AUX}/0/SendPostFader/value", True)
        assert sends(backend, 0, SUB_AUX1) == [
            pytest.approx(encode_input_fader(-10.0))]
        # AUX 2 sends are not AUX 1's
        assert not [c for c in backend.bus_params if c[1] == 4]

    def test_aux_pre_post_through_the_tree(self, router, backend, tree):
        tree.on_set = router.on_set
        unmuted(tree, 2, -20.0)
        tree.set_value(f"{IN}/2/sends/1/Gain/value", 0.0)
        tree.set(f"{AUX}/1/SendPostFader/value",
                 not tree.get_value(f"{AUX}/1/SendPostFader/value"))
        assert sends(backend, 2, 4)

    def test_cue_default_pre_post(self, router, backend, tree):
        unmuted(tree, 3, -10.0)
        tree.set_value(f"{IN}/3/sends/2/Gain/value", 0.0)     # CUE 1
        tree.set_value("/devices/0/HP1SendPostFader/value", True)
        router.on_set("/devices/0/HP1SendPostFader/value", True)
        assert sends(backend, 3, SUB_CUE1) == [
            pytest.approx(encode_input_fader(-10.0))]
        assert sends(backend, 3, SUB_CUE2) == []

    def test_solo_silences_a_post_send_it_rewrites(self, router, backend,
                                                   tree):
        unmuted(tree, 0, 0.0)
        unmuted(tree, 1, 0.0)
        tree.set_value(f"{IN}/1/Solo/value", True)
        tree.set_value(f"{IN}/0/sends/0/Gain/value", 0.0)
        tree.set_value(f"{IN}/0/sends/0/PostFaderOverride/value", 1)
        router.on_set(f"{IN}/0/sends/0/PostFaderOverride/value", 1)
        assert sends(backend, 0, SUB_AUX1) == [0.0]


class TestCueBusCount:
    """Settings > CUE BUS COUNT writes the root /CueBusCount; the device's
    CueBusCount is read-only (engine 12 schema) and follows it."""

    def test_root_count_enables_the_cue_buses(self, router, backend):
        router.on_set("/CueBusCount/value", 4)
        assert backend.mixer_params == [(0, 5, 0x01, 1), (0, 6, 0x01, 1)]

    def test_daemon_mirrors_device_count_and_cue_outputs(self, tree):
        d = MixerDaemon(tree, None)
        assert tree.get_value("/devices/0/outputs/16/Active/value") is False
        tree.set("/CueBusCount/value", 4)
        assert tree.get_value("/devices/0/CueBusCount/value") == 4
        assert [tree.get_value(f"/devices/0/outputs/{n}/Active/value")
                for n in (14, 15, 16, 17)] == [True, True, True, True]
        tree.set("/CueBusCount/value", 2)
        assert [tree.get_value(f"/devices/0/outputs/{n}/Active/value")
                for n in (14, 15, 16, 17)] == [True, True, False, False]
        assert d.state is tree


class TestTalkback:
    def daemon(self, tree, backend):
        router = HardwareRouter(backend, tree)
        return MixerDaemon(tree, router), router

    def test_talkback_mic_is_not_muted_at_init(self, tree, backend):
        d, router = self.daemon(tree, backend)
        assert router.talkback_input() == 22
        d._push_defaults_to_hardware()
        assert tree.get_value(f"{IN}/22/Mute/value") is False
        assert tree.get_value(f"{IN}/5/Mute/value") is True   # the others are
        tb_bus = router.talkback_bus()
        main = [v for b, s, v in backend.bus_params if (b, s) == (tb_bus, 0)]
        assert main and main[-1] == pytest.approx(
            router._talkback_coeff(-6.0))
        # CUE 1/2 sends at the map's -6 dB: TALK is heard in the headphones
        for sub in (SUB_CUE1, SUB_CUE2):
            assert sends(backend, tb_bus, sub)[-1] == pytest.approx(
                encode_input_fader(-6.0))
        # CUE 3/4 stay silent with two cues
        assert all(v == 0.0 for v in sends(backend, tb_bus, 5))

    def test_saved_talkback_mute_from_an_older_daemon_is_dropped(
            self, tree, backend):
        d, router = self.daemon(tree, backend)
        tree.set_value(f"{IN}/22/Mute/value", True)
        tree._dirty[f"{IN}/22/Mute/value"] = True
        d._push_defaults_to_hardware()
        assert tree.get_value(f"{IN}/22/Mute/value") is False
        main = [v for b, s, v in backend.bus_params
                if (b, s) == (router.talkback_bus(), 0)]
        assert main[-1] > 0.0

    def test_saved_talkback_fader_is_kept(self, tree, backend):
        d, router = self.daemon(tree, backend)
        tree.set_value(f"{IN}/22/FaderLevel/value", -12.0)
        tree._dirty[f"{IN}/22/FaderLevel/value"] = -12.0
        d._push_defaults_to_hardware()
        main = [v for b, s, v in backend.bus_params
                if (b, s) == (router.talkback_bus(), 0)]
        assert main[-1] == pytest.approx(router._talkback_coeff(-12.0))

    @pytest.mark.parametrize("on", [True, False])
    def test_talk_writes_other_param_0x46(self, tree, backend, on):
        d, router = self.daemon(tree, backend)
        tree.on_set = router.on_set
        tree.set_value("/TalkbackOn/value", not on)
        backend.clear()
        client = MixerClient(None, Writer(), ubjson=False)
        d._handle_set(client, Command("set", "/TalkbackOn/value", on,
                                      raw_params={"func_id": "5"}))
        calls = [c for c in backend.mixer_params if c[2] == TALKBACK_PARAM]
        assert calls and {c[3] for c in calls} == {int(on)}
        assert {c[0] for c in calls} == {2}
        assert tree.get_value("/TalkbackOn/value") is on

    def test_talkback_to_monitor_writes_tb_config(self, tree, backend):
        d, router = self.daemon(tree, backend)
        tree.on_set = router.on_set
        client = MixerClient(None, Writer(), ubjson=False)
        d._handle_set(client, Command("set", "/TalkbackInPhysicalCR/value",
                                      True, raw_params={"func_id": "6"}))
        assert (2, 0, TB_CONFIG, 1) in backend.mixer_params


class Writer:
    def __init__(self):
        self.frames = []

    def get_extra_info(self, name):
        return ("127.0.0.1", 4710)

    def write(self, data):
        self.frames.append(data)


class TestRefusedClockSet:
    """Console updates its rate at once and ignores an error reply; only a
    value push snaps it back (Rack.dll UAClnt_Values::Set 0x180e33880)."""

    @pytest.fixture(autouse=True)
    def no_pipewire(self, monkeypatch):
        monkeypatch.setattr(hardware, "pipewire_force_rate",
                            lambda rate, device=None: False)

    def test_refused_rate_pushes_the_rate_in_effect_then_the_error(
            self, tree):
        backend = FakeBackend()
        backend.set_driver_param = lambda param, value: False
        backend.driver_values[DRIVER_PARAM_SAMPLE_RATE] = 48000
        router = HardwareRouter(backend, tree)
        d = MixerDaemon(tree, router)
        writer = Writer()
        client = MixerClient(None, writer, ubjson=False)
        d._handle_set(client, Command("set", "/SampleRate/value", 192000,
                                      raw_params={"func_id": "9"}))
        replies = [json.loads(f.rstrip(b"\0")) for f in writer.frames]
        assert replies[0] == {"path": "/SampleRate/value", "data": 48000}
        assert replies[-1]["error"] == "Unable to set value."
        assert replies[-1]["parameters"]["func_id"] == 9
        assert tree.get_value("/SampleRate/value") == 48000


class TestMutedStripHoldsItsLevel:
    """Moving the fader or pan of a muted strip (or one a solo silences)
    used to write its live coefficient: the input became audible while
    Console showed it muted."""

    def main_mix(self, backend, bus=0):
        return [v for b, s, v in backend.bus_params if (b, s) == (bus, 0)]

    def test_fader_of_a_muted_input_stays_silent(self, router, backend,
                                                 tree):
        tree.on_set = router.on_set
        tree.set(f"{IN}/0/Mute/value", True)
        backend.clear()
        tree.set(f"{IN}/0/FaderLevel/value", -10.0)
        assert all(v == 0.0 for v in self.main_mix(backend))
        backend.clear()
        tree.set(f"{IN}/0/Mute/value", False)
        assert self.main_mix(backend)[-1] == pytest.approx(
            encode_input_fader(-10.0))

    def test_pan_of_a_muted_input_stays_silent(self, router, backend, tree):
        tree.on_set = router.on_set
        tree.set(f"{IN}/0/Mute/value", True)
        backend.clear()
        tree.set(f"{IN}/0/Pan/value", -0.5)
        assert self.main_mix(backend) == []

    def test_fader_of_a_solo_silenced_input_stays_silent(self, router,
                                                         backend, tree):
        tree.on_set = router.on_set
        tree.set(f"{IN}/0/Mute/value", False)
        tree.set(f"{IN}/1/Solo/value", True)
        backend.clear()
        tree.set(f"{IN}/0/FaderLevel/value", -5.0)
        assert self.main_mix(backend) == []
        backend.clear()
        tree.set(f"{IN}/1/Solo/value", False)
        assert self.main_mix(backend)[-1] == pytest.approx(
            encode_input_fader(-5.0))

    def test_fader_of_a_live_input_is_written(self, router, backend, tree):
        tree.on_set = router.on_set
        tree.set(f"{IN}/0/Mute/value", False)
        backend.clear()
        tree.set(f"{IN}/0/FaderLevel/value", -10.0)
        assert self.main_mix(backend) == [pytest.approx(
            encode_input_fader(-10.0))]


class TestLevelCompanions:
    """Console writes FaderLevelTapered and a send's GainTapered; the dB
    values the router reads later (unmute, solo, pre/post, bypass) used to
    stay stale, so an unmute played the old level and a send's M off
    restored silence."""

    def setup(self, tree, backend):
        router = HardwareRouter(backend, tree)
        tree.on_set = router.on_set
        return MixerDaemon(tree, router), MixerClient(None, Writer())

    def set(self, d, client, path, value):
        d._handle_set(client, Command("set", path, value,
                                      raw_params={"func_id": "3"}))

    def test_fader_tapered_moves_fader_level(self, tree, backend):
        d, client = self.setup(tree, backend)
        tapered = hardware.fader_db_to_tapered(-10.0)
        self.set(d, client, f"{IN}/0/FaderLevelTapered/value", tapered)
        assert tree.get_value(f"{IN}/0/FaderLevel/value") == pytest.approx(
            -10.0, abs=0.01)
        # the sender hears the companion too
        echoes = [json.loads(f.rstrip(b"\0")) for f in client.writer.frames]
        assert any(e["path"] == f"{IN}/0/FaderLevel/value" for e in echoes)
        tree.set(f"{IN}/0/Mute/value", True)
        backend.clear()
        tree.set(f"{IN}/0/Mute/value", False)
        main = [v for b, s, v in backend.bus_params if (b, s) == (0, 0)]
        assert main[-1] == pytest.approx(encode_input_fader(-10.0), abs=1e-4)

    def test_send_gain_tapered_moves_gain(self, tree, backend):
        d, client = self.setup(tree, backend)
        tree.set_value(f"{IN}/0/sends/0/PostFaderOverride/value", 0)
        self.set(d, client, f"{IN}/0/sends/0/GainTapered/value",
                 hardware.fader_db_to_tapered(-6.0))
        assert tree.get_value(f"{IN}/0/sends/0/Gain/value") == pytest.approx(
            -6.0, abs=0.01)
        self.set(d, client, f"{IN}/0/sends/0/Bypass/value", True)
        backend.clear()
        self.set(d, client, f"{IN}/0/sends/0/Bypass/value", False)
        assert sends(backend, 0, SUB_AUX1)[-1] == pytest.approx(
            encode_input_fader(-6.0), abs=1e-4)

    def test_aux_fader_and_aux_send(self, tree, backend):
        d, client = self.setup(tree, backend)
        self.set(d, client, f"{AUX}/0/FaderLevelTapered/value",
                 hardware.fader_db_to_tapered(-3.0))
        assert tree.get_value(f"{AUX}/0/FaderLevel/value") == pytest.approx(
            -3.0, abs=0.01)
        self.set(d, client, f"{AUX}/0/sends/0/GainTapered/value", 0.5)
        assert tree.get_value(f"{AUX}/0/sends/0/Gain/value") == pytest.approx(
            hardware.fader_tapered_to_db(0.5))

    def test_db_set_moves_the_tapered_value(self, tree, backend):
        d, client = self.setup(tree, backend)
        self.set(d, client, f"{IN}/1/FaderLevel/value", -20.0)
        assert tree.get_value(f"{IN}/1/FaderLevelTapered/value") == \
            pytest.approx(hardware.fader_db_to_tapered(-20.0))
