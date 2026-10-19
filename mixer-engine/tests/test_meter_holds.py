"""Settings > PEAK HOLD / CLIP HOLD reach the host meters, live."""

import pytest

import metering
from metering import SILENCE_DB, ReadbackMeter
from ua_mixer_daemon import MixerDaemon

QUIET, LOUD = [0] * 40, [0] * 40
LOUD[8] = 0xFFFF << 16        # analog 1 full scale: clips, peak ~0 dB


class Clock:
    now = 1000.0

    def __call__(self):
        return self.now


@pytest.fixture
def clock(monkeypatch):
    c = Clock()
    monkeypatch.setattr(metering.time, "monotonic", c)
    return c


@pytest.fixture
def meter():
    return ReadbackMeter()


@pytest.fixture
def daemon(router, tree, meter):
    return MixerDaemon(tree, router, metering=meter)


def after(clock, meter, seconds):
    """A clipping blip, then silence `seconds` later: (peak, clip)."""
    meter.feed(LOUD)
    clock.now += seconds
    meter.feed(QUIET)
    _, peak, clip = meter.get_channel_meter(0)
    return peak, clip


def test_default_follows_the_tree_value(daemon, meter):
    assert meter.peak_hold_sec == 3.0 and meter.clip_hold_sec == 3.0


@pytest.mark.parametrize("name,seconds", [
    ("NONE", 0.0), ("1 SEC", 1.0), ("3 SEC", 3.0), ("5 SEC", 5.0),
    ("10 SEC", 10.0)])
def test_clip_hold_follows_the_tree(daemon, tree, meter, clock, name, seconds):
    tree.set("/ClipHold/value", name, hardware=False)
    assert meter.clip_hold_sec == seconds
    held = after(clock, meter, seconds - 0.01 if seconds else 0.001)[1]
    assert held is bool(seconds)
    clock.now += 0.02
    assert meter.get_channel_meter(0)[2] is False


def test_clip_hold_infinite_until_cleared(daemon, tree, meter, clock):
    tree.set("/ClipHold/value", "INFINITE", hardware=False)
    assert after(clock, meter, 3600)[1] is True
    meter.clear_clips()
    assert meter.get_channel_meter(0)[2] is False


def test_peak_hold_follows_the_tree_on_change(daemon, tree, meter, clock):
    tree.set("/PeakHold/value", "10 SEC", hardware=False)
    assert after(clock, meter, 9.0)[0] > -1.0       # still held
    meter.feed(LOUD)
    tree.set("/PeakHold/value", "NONE", hardware=False)
    clock.now += 1.0
    meter.feed(QUIET)
    assert meter.get_channel_meter(0)[1] < -15.0    # decaying at once


def test_unknown_value_leaves_the_hold(daemon, tree, meter):
    daemon._on_meter_holds("/PeakHold/value", "bogus")
    assert meter.peak_hold_sec == 3.0


def test_null_meter_accepts_holds(router, tree):
    MixerDaemon(tree, router).state.set("/PeakHold/value", "1 SEC",
                                        hardware=False)


# ── Holds with the Mixer Engine's release (UADMeterValue::Update) ───────

def words(db):
    """A readback with analog 1 at `db` dBFS."""
    w = [0] * 40
    w[8] = round(65535 * 10 ** (db / 20)) << 16
    return w


@pytest.mark.parametrize("name,seconds", [
    ("NONE", 0.0), ("1 SEC", 1.0), ("3 SEC", 3.0), ("5 SEC", 5.0),
    ("10 SEC", 10.0)])
def test_peak_holds_then_falls_exponentially(daemon, tree, meter, clock,
                                             name, seconds):
    tree.set("/PeakHold/value", name, hardware=False)
    meter.feed(words(-6.0))
    meter.feed(QUIET)
    if seconds:
        clock.now += seconds - 0.01
        assert meter.get_channel_meter(0)[1] == pytest.approx(-6.0, abs=0.05)
        clock.now += 0.01
    for past in (0.25, 1.0, 2.0):       # (dB + 77) * 0.993 per 30.71 ms
        start = clock.now
        clock.now += past
        assert meter.get_channel_meter(0)[1] == pytest.approx(
            metering.meter_decay(-6.0, past), abs=0.05)
        clock.now = start
    # Not the old linear 20 dB/s: 2 s past the hold sits near -32, not -46
    clock.now += 2.0
    assert meter.get_channel_meter(0)[1] > -40.0


def test_peak_hold_infinite_never_falls(daemon, tree, meter, clock):
    tree.set("/PeakHold/value", "INFINITE", hardware=False)
    meter.feed(words(-6.0))
    clock.now += 3600
    meter.feed(QUIET)
    assert meter.get_channel_meter(0)[1] == pytest.approx(-6.0, abs=0.05)


def test_level_has_no_hold_and_falls_to_the_input(meter, clock):
    meter.feed(words(-3.0))
    clock.now += 0.5
    meter.feed(words(-40.0))
    assert meter.get_channel_meter(0)[0] == pytest.approx(
        metering.meter_decay(-3.0, 0.5), abs=0.05)
    clock.now += 10.0                   # the release stops at the input
    meter.feed(words(-40.0))
    assert meter.get_channel_meter(0)[0] == pytest.approx(-40.0, abs=0.05)


def test_linear_peak_decay_is_gone():
    assert not hasattr(metering, "PEAK_DECAY_DB_SEC")
