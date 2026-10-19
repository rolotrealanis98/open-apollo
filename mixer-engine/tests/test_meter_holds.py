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
