"""Host-side preamp autogain (autogain.py): the state machine on a fake
clock and a fake peak feed, then the daemon glue (/auto_gain on the Mixer
Helper tree, gating, the gain path)."""

import json

import pytest

import autogain as ag_mod
from autogain import (AutoGain, Settings, linear_to_db, round_half_away,
                      CD_DONE, CD_LOW_SIGNAL, CD_OVERLOAD_ERROR, CD_WAITING)
from conftest import DEVICE_MAP_X4, ENGINE_DIR, FakeBackend
from hardware import HardwareRouter
from helper_tree import HelperTree
from state_tree import StateTree
from ua_mixer_daemon import MixerDaemon

DT = 0.01


class FakeIO:
    """Preamp gain/pad store whose input level follows the gain: the
    peak is `source + gain` dBFS, less 20 dB with the pad in."""

    def __init__(self, gain=30, source_at_gain=None, pad_ok=False):
        self.gain = {0: float(gain)}
        self.pad = {0: False}
        self.pad_ok = pad_ok
        self.gone = set()
        self.writes = []            # (t, ch, db)
        self.clock = lambda: 0.0
        self.silent = True
        # peak(g) = base + (g - gain)
        self.base = source_at_gain
        self.ref = gain

    def exists(self, ch):
        return ch not in self.gone

    def get_gain(self, ch):
        return self.gain[ch]

    def set_gain(self, ch, db):
        self.gain[ch] = float(db)
        self.writes.append((self.clock(), ch, db))

    def can_use_pad(self, ch):
        return self.pad_ok and not self.pad[ch]

    def set_pad(self, ch, on):
        self.pad[ch] = on

    def peak(self, ch=0):
        if self.silent or self.base is None:
            return -144.0
        return self.base + (self.gain[ch] - self.ref) - \
            (20 if self.pad[ch] else 0)


class Rig:
    def __init__(self, gain=30, peak_at_gain=-20.0, pad_ok=False, **settings):
        self.io = FakeIO(gain, peak_at_gain, pad_ok)
        self.events = []
        self.s = Settings(**settings)
        self.ag = AutoGain(self.io, self.s,
                           on_change=lambda n, v: self.events.append((n, v)))
        self.t = 0.0
        self.io.clock = lambda: self.t
        self._last = {}
        self.statuses = []
        self.ag.on_change = self._record

    def _record(self, name, value):
        self.events.append((name, value))
        if name == "status":
            self.statuses.append(value)

    def start(self, channels=(0,), signal=True):
        for ch in channels:
            self.ag.add_channel(ch)
            self.io.gain.setdefault(ch, 30.0)
            self.io.pad.setdefault(ch, False)
        self.io.silent = not signal
        self.ag.t.now = self.t
        self.ag.start_all()

    def run(self, seconds):
        end = self.t + seconds
        while self.t < end - 1e-9:
            self.t += DT
            self.ag.advance(self.t)
            for ch in list(self.ag.channels):
                p = self.io.peak(ch)
                if self._last.get(ch) != p:
                    self._last[ch] = p
                    self.ag.on_peak(ch, p)

    def state(self, ch=0):
        return self.ag.channels[ch].state if ch in self.ag.channels else 0

    def gain_steps(self):
        return [(round(t, 3), db) for t, _, db in self.io.writes]


# ── Pure pieces ─────────────────────────────────────────────────────

def test_round_half_away_from_zero():
    assert [round_half_away(x) for x in (2.5, -2.5, 0.5, -0.5, 2.4, -2.4,
                                         3.5, -3.5, 0.0)] == \
        [3, -3, 1, -1, 2, -2, 4, -4, 0]


def test_linear_to_db_floor():
    assert linear_to_db(1.0) == 0.0
    assert linear_to_db(0.5) == pytest.approx(-6.0206, abs=1e-3)
    assert linear_to_db(0.0) == -144.0
    assert linear_to_db(6.0e-8) == -144.0
    assert linear_to_db(7.0e-8) > -144.0


def test_defaults_match_the_mixer_helper():
    s = Settings()
    assert (s.duration, s.dynamic_auto_gain, s.peak_target, s.threshold) == \
        (10, True, -8.0, -60.0)
    assert (s.timeout_ms, s.overload_db, s.listen_ms, s.ramp_ms,
            s.ramp_dyn_ms, s.pad_window_db) == (20000, -3.0, 2000, 3000,
                                                1000, 6.0)
    assert (ag_mod.MIN_GAIN, ag_mod.MAX_GAIN, ag_mod.GAIN_STEP) == (10, 65, 1)


def test_initial_properties():
    r = Rig()
    assert r.ag.properties() == {"status": "stopped", "status_str": "READY",
                                 "info": "", "count_down": CD_DONE}


# ── Normal path (3 channels: WHILE-LISTENING is off, so the listen is
#    `duration` long and the ramp takes 3 s) ─────────────────────────

def normal_rig(**kw):
    """Normal mode: dynamic_auto_gain off."""
    return Rig(dynamic_auto_gain=False, **kw)


def test_normal_path_up_to_target():
    r = normal_rig(gain=30, peak_at_gain=-20.0)         # needs +12 dB
    r.start()
    assert r.ag.status == "waiting_for_signal"
    assert r.ag.count_down == CD_WAITING
    assert r.state() == ag_mod.WAITING
    r.run(0.05)                                          # signal arrives
    assert r.state() == ag_mod.LISTENING
    assert r.ag.status == "listening"
    assert r.ag.count_down == 10
    assert r.ag.properties()["status_str"] == "LISTENING FOR 10 SECONDS"
    r.run(3.0)
    assert r.ag.count_down == 7                          # ticks each second
    assert r.io.writes == []                             # not adjusting yet
    r.run(7.1)
    assert r.state() in (ag_mod.ADJUSTING,)
    assert r.ag.status == "adjusting_gain"
    # 12 steps of 3000/12 = 250 ms, +1 dB each
    r.run(3.1)
    steps = r.gain_steps()
    assert [db for _, db in steps] == list(range(31, 43))
    gaps = [round(b[0] - a[0], 2) for a, b in zip(steps, steps[1:])]
    assert gaps == [0.25] * 11
    assert r.io.gain[0] == 42
    r.run(0.1)
    assert r.state() == ag_mod.COMPLETED
    r.run(1.0)                                           # 1 s settle
    assert r.ag.status == "done_listening"
    assert r.ag.count_down == CD_DONE
    assert r.ag.properties()["status_str"] == "DONE LISTENING"
    r.run(2.1)                                           # StopAll after 2 s
    assert r.ag.status == "stopped"
    assert r.ag.channels == {}


def test_normal_path_down_to_target():
    r = normal_rig(gain=40, peak_at_gain=-5.0)          # needs -3 dB
    r.start()
    r.run(0.05 + 10.0 + 0.05)
    assert r.state() == ag_mod.ADJUSTING
    r.run(4.0)
    steps = r.gain_steps()
    assert [db for _, db in steps] == [39, 38, 37]
    # |3| is not under 3 dB: 3000/3 = 1000 ms a step
    assert [round(b[0] - a[0], 2) for a, b in zip(steps, steps[1:])] == \
        [1.0, 1.0]


def test_ramp_interval_is_100ms_under_3_db():
    r = normal_rig(gain=40, peak_at_gain=-10.0)         # needs +2 dB
    r.start()
    r.run(10.2)
    r.run(1.0)
    steps = r.gain_steps()
    assert [db for _, db in steps] == [41, 42]
    assert round(steps[1][0] - steps[0][0], 2) == 0.1


def test_delta_rounds_half_away_from_zero():
    # target - max = -8 - (-10.5) = 2.5 -> +3 ; -8 - (-5.5) = -2.5 -> -3
    r = normal_rig(gain=30, peak_at_gain=-10.5)
    r.start()
    r.run(14.0)
    assert r.io.gain[0] == 33
    r = normal_rig(gain=30, peak_at_gain=-5.5)
    r.start()
    r.run(14.0)
    assert r.io.gain[0] == 27


def test_gain_clamped_to_max_and_nothing_to_do_ends_done():
    r = normal_rig(gain=60, peak_at_gain=-30.0)         # +22 wanted, 65 max
    r.start()
    r.run(30.0)
    assert [db for _, db in r.gain_steps()] == [61, 62, 63, 64, 65]
    r = normal_rig(gain=65, peak_at_gain=-30.0)         # already at the limit
    r.start()
    r.run(12.0)
    assert r.io.writes == []
    assert r.ag.status == "done_listening"


def test_delta_zero_is_done_listening():
    r = normal_rig(gain=30, peak_at_gain=-8.0)
    r.start()
    r.run(11.0)
    assert r.io.writes == []
    assert r.ag.status == "done_listening"


# ── WHILE-LISTENING ─────────────────────────────────────────────────

def test_while_listening_loop():
    r = Rig(gain=30, peak_at_gain=-20.0)                # dynamic on, 1 ch
    r.start()
    r.run(0.05)
    assert r.state() == ag_mod.LISTENING
    r.run(1.9)
    assert r.io.writes == []
    r.run(0.2)                                           # 2 s window over
    assert r.state() == ag_mod.ADJUSTING
    r.run(1.2)
    steps = r.gain_steps()
    assert [db for _, db in steps] == list(range(31, 43))
    # total ramp 1 s: 1000/12 ms per step
    assert round(steps[1][0] - steps[0][0], 3) == pytest.approx(0.083, abs=0.01)
    # listens again while the countdown lasts, then finishes
    r.run(0.1)
    seen = set()
    for _ in range(120):
        r.run(0.1)
        seen.add(r.ag.status)
    assert {"listening", "done_listening"} <= seen
    assert r.statuses.count("listening") >= 2           # the loop relistened
    assert r.io.gain[0] == 42
    assert r.ag.status in ("done_listening", "stopped")


def test_while_listening_unavailable_cases():
    r = Rig()
    for ch in (0, 1, 2):
        r.ag.add_channel(ch)
    assert not r.ag._wl_available()                      # more than 2
    r = Rig(duration=3)
    r.ag.add_channel(0)
    assert not r.ag._wl_available()                      # too short
    r = Rig(duration=4)
    r.ag.add_channel(0)
    assert r.ag._wl_available()
    r = Rig(dynamic_auto_gain=False)
    r.ag.add_channel(0)
    assert not r.ag._wl_available()


# ── Overload ────────────────────────────────────────────────────────

def test_overload_decrements_then_restarts():
    r = normal_rig(gain=40, peak_at_gain=-2.0)          # over -3 dBFS
    r.start()
    r.run(0.05)
    assert r.state() == ag_mod.OVERLOAD
    assert r.ag.status == "overload"
    assert r.ag.properties()["status_str"] == "CHANNEL OVERLOADED"
    # first decrement at once, then every 100 ms until the peak is <= -8
    first = r.gain_steps()
    assert first[0][1] == 39
    r.run(1.0)
    steps = r.gain_steps()
    assert [db for _, db in steps][:6] == [39, 38, 37, 36, 35, 34]
    assert steps[1][0] - steps[0][0] == pytest.approx(0.1, abs=0.011)
    assert r.io.gain[0] == 34                            # peak -8: stop
    assert r.state() in (ag_mod.DONE_OVERLOAD, ag_mod.WAITING_RESTART,
                         ag_mod.LISTENING)
    r.run(1.2)                                           # 1 s, then restart
    assert r.state() in (ag_mod.LISTENING, ag_mod.DONE_LISTENING,
                         ag_mod.ADJUSTING)
    assert "overload" in r.statuses
    assert r.statuses.index("listening") < r.statuses.index("overload") \
        or "listening" in r.statuses[r.statuses.index("overload"):]
    r.run(15.0)
    assert r.io.gain[0] == 34                            # already at target


def test_min_gain_overload_without_pad_is_an_error():
    r = normal_rig(gain=11, peak_at_gain=-2.0, pad_ok=False)
    r.start()
    r.run(0.05)
    r.run(0.5)
    assert r.io.gain[0] == 10
    assert r.ag.status == "overload_error"
    assert r.ag.count_down == CD_OVERLOAD_ERROR
    assert r.ag.properties()["status_str"] == "CHANNEL OVERLOAD"
    assert r.state() == ag_mod.DISPLAY_ERROR
    r.run(3.1)
    assert r.ag.status == "stopped"
    r.run(2.1)                                           # StopAll, no remove
    assert r.state() == ag_mod.STOPPED
    assert 0 in r.ag.channels
    assert r.io.pad[0] is False


def test_min_gain_overload_with_pad_engages_pad_and_restarts():
    r = normal_rig(gain=12, peak_at_gain=-2.0, pad_ok=False)
    r.start()
    r.run(0.13)
    assert r.io.gain[0] == 10 and r.state() == ag_mod.OVERLOAD
    r.io.pad_ok = True                                   # the pad is usable
    r.run(0.2)
    assert r.io.pad[0] is True
    assert r.state() == ag_mod.ADJUSTING_PAD
    r.run(1.0)                                           # 1 s, then restart
    assert r.state() == ag_mod.LISTENING
    r.run(14.0)                                          # peak -24 at gain 10
    assert r.io.gain[0] == 26                            # raised by 16 dB


def test_loud_signal_that_gain_cannot_absorb_pads_at_once():
    # excess 6 dB over the target, only 1 dB of gain range to give
    r = normal_rig(gain=11, peak_at_gain=-2.0, pad_ok=True)
    r.start()
    r.run(0.1)
    assert r.io.pad[0] is True
    assert r.io.writes == []


def test_pad_for_a_loud_signal_with_low_gain_near_target():
    # gain 15, signal peak -9 (within 6 dB of the -8 target), gain <= 17
    r = normal_rig(gain=15, peak_at_gain=-9.0, pad_ok=True)
    r.start()
    r.run(0.1)
    assert r.io.pad[0] is True


# ── Level too low and the timeout ───────────────────────────────────

def test_no_signal_times_out_after_20_seconds():
    r = Rig()
    r.start(signal=False)
    r.run(19.9)
    assert r.state() == ag_mod.WAITING
    assert r.ag.status == "waiting_for_signal"
    r.run(0.2)
    assert r.ag.status == "low_signal_error"
    assert r.ag.count_down == CD_LOW_SIGNAL
    assert r.ag.properties()["status_str"] == "LEVEL TOO LOW"
    assert r.state() == ag_mod.DISPLAY_ERROR
    r.run(3.0)
    assert r.ag.status == "stopped"
    r.run(2.1)
    assert r.state() == ag_mod.STOPPED
    assert r.io.writes == []


def test_quiet_signal_under_threshold_never_starts_listening():
    r = Rig(peak_at_gain=-70.0)                          # under -60
    r.start()
    r.run(10.0)
    assert r.state() == ag_mod.WAITING
    r.run(10.5)
    assert r.ag.status == "low_signal_error"


def test_level_too_low_after_listening():
    # Pad engages, then the source goes silent: nothing to measure
    r = normal_rig(gain=11, peak_at_gain=-2.0, pad_ok=True)
    r.start()
    r.run(0.6)
    assert r.io.pad[0] is True
    r.io.silent = True
    r.run(1.0 + 10.1)                                    # restart + listen
    assert r.ag.status == "low_signal_error"
    assert r.ag.count_down == CD_LOW_SIGNAL


def test_signal_dropping_out_returns_to_waiting_after_1s():
    r = normal_rig(gain=30, peak_at_gain=-20.0)
    r.start()
    r.run(0.1)
    assert r.state() == ag_mod.LISTENING
    r.io.base = -80.0                                    # signal vanishes
    r.run(0.1)
    assert r.state() == ag_mod.LISTENING
    r.run(1.0)
    assert r.state() == ag_mod.WAITING
    assert r.ag.status == "waiting_for_signal"


# ── Stop and apply ──────────────────────────────────────────────────

def test_stop_all_removes_channels_after_10ms():
    r = normal_rig(gain=30, peak_at_gain=-20.0)
    r.start()
    r.run(2.0)
    r.ag.stop_all()
    assert r.ag.status == "listening"
    r.run(0.05)
    assert r.ag.status == "stopped"
    assert r.ag.channels == {}
    r.run(15.0)
    assert r.io.writes == []                             # timers are dead


def test_stop_channel_mid_ramp_stops_writing():
    r = normal_rig(gain=30, peak_at_gain=-20.0)
    r.start()
    r.run(11.0)
    n = len(r.io.writes)
    assert 0 < n < 12
    r.ag.stop_channel(0)
    r.run(5.0)
    assert len(r.io.writes) == n
    assert r.ag.status == "stopped"


def test_apply_in_normal_mode_adjusts_now():
    r = normal_rig(gain=30, peak_at_gain=-20.0)
    r.start()
    r.run(2.0)
    r.ag.apply()
    assert r.ag.status == "adjusting_gain"
    r.run(3.5)
    assert r.io.gain[0] == 42


def test_apply_in_while_listening_mode_stops():
    r = Rig(gain=30, peak_at_gain=-20.0)
    r.start()
    r.run(1.0)
    r.ag.apply()
    r.run(0.05)
    assert r.ag.status == "stopped"


def test_two_channels_listen_together():
    r = Rig(dynamic_auto_gain=False, gain=30, peak_at_gain=-20.0)
    r.io.gain[1] = 30.0
    r.io.pad[1] = False
    r.start(channels=(0, 1))
    r.run(0.05)
    assert r.state(0) == r.state(1) == ag_mod.LISTENING


# ── Daemon glue ─────────────────────────────────────────────────────

@pytest.fixture
def stack(tmp_path):
    tree = StateTree()
    tree.load_device_map(DEVICE_MAP_X4)
    tree.enable_persistence(tmp_path / "x4.state.json")
    be = FakeBackend()
    router = HardwareRouter(be, tree)
    helper = HelperTree()
    helper.load(ENGINE_DIR / "device_maps" / "helper_tree.json")
    d = MixerDaemon(tree, router, helper_tree=helper)
    d.commands.set_store(tmp_path, "x4")
    d._hw_init_done = True
    d.be = be
    return d


class HClient:
    id = "h-1"
    helper = True
    ubjson = False

    def __init__(self):
        self.sent = []

    def send(self, data):
        self.sent.append(data)

    def last(self):
        return json.loads(self.sent[-1].rstrip(b"\0"))


def test_helper_tree_serves_auto_gain_status(stack):
    assert stack.helper_tree.get_value("/auto_gain/status") == "stopped"
    assert stack.helper_tree.get_value("/auto_gain/status_str") == "READY"
    assert stack.helper_tree.get_value("/auto_gain/count_down") == CD_DONE
    data = stack.helper_tree.get("/auto_gain", flatvalue=True)
    assert data["properties"]["status"] == "stopped"
    assert {c["path"] for c in data["commands"]} >= {"start_auto_gain",
                                                     "stop_auto_gain"}


def test_gated_off_by_default_on_a_gen1_map(stack):
    assert stack.state.get_value("/devices/0/SupportsAutogain/value") is False
    c = HClient()
    stack._dispatch(c, "post /auto_gain/start_auto_gain "
                       '{"all":true}')
    assert "error" in c.last()
    assert stack.autogain.ag.status == "stopped"


def test_any_model_option_allows_it(stack):
    stack.autogain.any_model = True
    c = HClient()
    stack._dispatch(c, "post /auto_gain/start_auto_gain?func_id=7 "
                       '{"all":true}')
    assert c.last()["data"] is True
    assert stack.helper_tree.get_value("/auto_gain/status") == \
        "waiting_for_signal"
    assert stack.helper_tree.get_value("/auto_gain/count_down") == CD_WAITING
    assert sorted(stack.autogain.ag.channels) == [0, 1, 2, 3]
    stack._dispatch(c, "post /auto_gain/stop_auto_gain "
                       '{"all":true}')
    assert c.last()["data"] is True


def test_supports_autogain_flag_allows_it(stack):
    stack.state.set("/devices/0/SupportsAutogain/value", True)
    assert stack.autogain.supported()
    c = HClient()
    stack._dispatch(c, "post /auto_gain/start_auto_gain "
                       '{"channel_index":2,"auto_start":true}')
    assert c.last()["data"] is True
    assert list(stack.autogain.ag.channels) == [2]
    assert stack.autogain.ag.status == "waiting_for_signal"


def test_helper_clients_are_notified_of_status(stack):
    seen = []
    stack.helper_tree.register_callback("t", lambda p, v: seen.append((p, v)))
    stack.helper_tree.subscribe("t", "/auto_gain")
    stack.autogain.any_model = True
    stack.autogain.execute("/auto_gain/start_auto_gain", {"all": True})
    assert ("/auto_gain/status", "waiting_for_signal") in seen
    assert ("/auto_gain/count_down", CD_WAITING) in seen


def test_gain_write_goes_through_the_state_tree_and_hardware(stack):
    stack.autogain.io.set_gain(1, 42)
    assert stack.state.get_value(
        "/devices/0/inputs/1/preamps/0/Gain/value") == 42.0
    assert stack.state.get_value(
        "/devices/0/inputs/1/preamps/0/GainTapered/value") == \
        pytest.approx((42 - 10) / 55.0)
    assert stack.be.mixer_params or stack.be.other   # reached the backend


def test_daemon_peak_source_reads_the_meter(stack):
    class M:
        channel_ids = [0x0001, 0x0002, 0x0003, 0x0004]

        def get_channel_meter(self, cap):
            return (-12.5 - cap, -3.0, False)

    stack._metering = M()
    assert stack.autogain.io.peak_db(0) == -12.5
    assert stack.autogain.io.peak_db(1) == -13.5


def test_prefs_file_sets_settings(stack, tmp_path):
    (tmp_path / "auto_gain_prefs.json").write_text(json.dumps(
        {"settings": {"duration": 6, "peak_target": -12, "bogus": 1}}))
    stack.autogain.load_settings(tmp_path)
    s = stack.autogain.settings
    assert (s.duration, s.peak_target) == (6, -12.0)


def test_stereo_input_peak_is_the_louder_side(stack):
    class M:
        channel_ids = [0x0001, 0x0002, 0x0003, 0x0004]

        def get_channel_meter(self, cap):
            return ((-20.0, -9.0, -30.0, -30.0)[cap], -3.0, False)

    stack._metering = M()
    assert stack.autogain.io.peak_db(0) == -20.0
    stack.state.set("/devices/0/inputs/0/Stereo/value", True)
    assert stack.autogain.io.peak_db(0) == -9.0     # ANALOG 2, the right


def test_pad_only_on_a_mic_input(stack):
    io = stack.autogain.io
    base = "/devices/0/inputs/0"
    assert stack.state.get_value(f"{base}/IOType/value") == "Mic"
    stack.state.set(f"{base}/preamps/0/Pad/value", False)
    stack.state.set(f"{base}/preamps/0/HiZ/value", False)
    assert io.can_use_pad(0)
    stack.state.set(f"{base}/IOType/value", "Line")
    assert not io.can_use_pad(0)
    stack.state.set(f"{base}/IOType/value", "Mic")
    stack.state.set(f"{base}/preamps/0/HiZ/value", True)
    assert not io.can_use_pad(0)
