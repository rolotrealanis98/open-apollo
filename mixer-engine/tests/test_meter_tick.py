"""Meters go out as the Mixer Engine sends them: one tick every 30.71 ms with
/MeterPulse and the meters that changed (a `multi` envelope for a client in
`subscribe mode=multi`), released at the engine's rate."""

import asyncio
import json
import statistics

import pytest

import metering
from metering import (METER_DECAY_COEFFICIENT, METER_TICK_SEC, SILENCE_DB,
                      ReadbackMeter, meter_decay)
from protocol import encode_response_bytes
from ua_mixer_daemon import MixerClient, MixerDaemon

LEVEL = "/devices/0/inputs/0/meters/0/MeterLevel/value"
PEAK = "/devices/0/inputs/0/meters/0/MeterPeakLevel/value"
PULSE = "/MeterPulse/value"
QUIET = [0] * 40


def words(db):
    """A readback with analog 1 at `db` dBFS."""
    w = [0] * 40
    w[8] = round(65535 * 10 ** (db / 20)) << 16
    return w


class Clock:
    now = 1000.0

    def __call__(self):
        return self.now


@pytest.fixture
def clock(monkeypatch):
    c = Clock()
    monkeypatch.setattr(metering.time, "monotonic", c)
    return c


class Writer:
    def __init__(self):
        self.frames = []

    def get_extra_info(self, name):
        return ("127.0.0.1", 4710)

    def write(self, data):
        self.frames.append(data)

    def messages(self):
        out = [json.loads(f.rstrip(b"\0")) for f in self.frames]
        self.frames.clear()
        return out


@pytest.fixture
def meter():
    return ReadbackMeter()


@pytest.fixture
def daemon(router, tree, meter):
    return MixerDaemon(tree, router, metering=meter)


def connect(daemon, paths, multi):
    """A 4710 text client subscribed to `paths`, as _handle_client sets it up."""
    w = Writer()
    c = MixerClient(None, w)
    c.multi = multi
    daemon.clients[c.id] = c
    daemon.state.register_callback(
        c.id, lambda p, v: c.send(encode_response_bytes(p, v)))
    for p in paths:
        daemon.state.subscribe(c.id, p)
    return w


# ── The engine's release ────────────────────────────────────────────────

def test_release_is_the_engine_law():
    assert meter_decay(0.0, METER_TICK_SEC) == pytest.approx(
        77 * METER_DECAY_COEFFICIENT - 77)
    # About 1.3 s from 0 to -20 dB; silence stays silence
    assert meter_decay(0.0, 1.31) == pytest.approx(-20.0, abs=0.2)
    assert meter_decay(SILENCE_DB, 5.0) == SILENCE_DB


def test_level_rises_at_once_and_falls_at_the_engine_rate(meter, clock):
    meter.feed(words(-6.0))
    assert meter.get_channel_meter(0)[0] == pytest.approx(-6.0, abs=0.05)
    for _ in range(25):                 # 0.5 s of silence at 50 Hz
        clock.now += 0.02
        meter.feed(QUIET)
    level = meter.get_channel_meter(0)[0]
    assert level == pytest.approx(meter_decay(-6.0, 0.5), abs=0.05)
    assert level > -20.0                # not a drop to silence


@pytest.mark.parametrize("feed_hz", [20.0, 50.0, 200.0])
def test_release_does_not_depend_on_the_feed_rate(meter, clock, feed_hz):
    meter.feed(words(-3.0))
    t0 = clock.now
    while clock.now - t0 < 1.0:
        clock.now += 1.0 / feed_hz
        meter.feed(QUIET)
    clock.now = t0 + 1.0
    assert meter.get_channel_meter(0)[0] == pytest.approx(
        meter_decay(-3.0, 1.0), abs=0.05)


def test_peak_holds_then_falls_at_the_engine_rate(daemon, tree, meter, clock):
    tree.set("/PeakHold/value", "1 SEC", hardware=False)
    meter.feed(words(-6.0))
    clock.now += 0.9
    meter.feed(QUIET)
    assert meter.get_channel_meter(0)[1] == pytest.approx(-6.0, abs=0.05)
    clock.now += 0.6                    # 0.5 s past the hold
    assert meter.get_channel_meter(0)[1] == pytest.approx(
        meter_decay(-6.0, 0.5), abs=0.05)


# ── The tick ────────────────────────────────────────────────────────────

def test_multi_client_gets_one_envelope_per_tick(daemon, meter):
    w = connect(daemon, [PULSE, LEVEL, PEAK], multi=True)
    meter.feed(words(-6.0))
    daemon._meter_tick()
    [env] = w.messages()
    assert env["path"] == "multi"
    assert env["data"][0] == {"path": PULSE, "data": 1}
    got = {d["path"]: d["data"] for d in env["data"][1:]}
    assert got[LEVEL] == pytest.approx(-6.0, abs=0.05)
    assert got[PEAK] == pytest.approx(-6.0, abs=0.05)
    # Nothing moved: the envelope carries the pulse alone
    daemon._meter_tick()
    assert w.messages() == [{"path": "multi",
                             "data": [{"path": PULSE, "data": 2}]}]


def test_single_client_gets_separate_pushes(daemon, meter):
    w = connect(daemon, [PULSE, LEVEL], multi=False)
    meter.feed(words(-6.0))
    daemon._meter_tick()
    msgs = w.messages()
    assert [m["path"] for m in msgs] == [PULSE, LEVEL]


def test_sleeping_multi_client_gets_nothing(daemon, meter):
    w = connect(daemon, [PULSE, LEVEL], multi=True)
    daemon.clients[next(iter(daemon.clients))].sleep = True
    meter.feed(words(-6.0))
    daemon._meter_tick()
    assert w.messages() == []


def test_falling_meter_goes_out_every_tick(daemon, meter, clock):
    """The release moves a meter each tick: it is sent each tick (the old
    0.5 dB gate dropped every other one near the top and froze it below)."""
    w = connect(daemon, [LEVEL], multi=True)
    meter.feed(words(0.0))
    daemon._meter_tick()
    w.messages()
    sent = []
    for _ in range(40):
        clock.now += METER_TICK_SEC
        meter.feed(QUIET)
        daemon._meter_tick()
        sent += [d["data"] for env in w.messages() for d in env["data"]]
    assert len(sent) == 40
    assert all(b < a for a, b in zip(sent, sent[1:]))
    assert sent[-1] == pytest.approx(meter_decay(0.0, 40 * METER_TICK_SEC),
                                     abs=0.05)


def test_get_returns_the_published_reading(daemon, meter):
    connect(daemon, [LEVEL], multi=True)
    meter.feed(words(-6.0))
    daemon._meter_tick()
    assert daemon.state.get_value(LEVEL) == pytest.approx(-6.0, abs=0.05)


def test_ticks_at_the_engine_rate(daemon):
    """MeterPulse runs at ~32.6 Hz (30.71 ms), not 10 Hz."""
    w = connect(daemon, [PULSE], multi=True)
    stamps = []
    real = daemon._meter_tick

    def tick():
        stamps.append(asyncio.get_running_loop().time())
        real()

    daemon._meter_tick = tick

    async def run():
        task = asyncio.create_task(daemon._meter_pulse_heartbeat())
        await asyncio.sleep(0.62)
        task.cancel()

    asyncio.run(run())
    gaps = [b - a for a, b in zip(stamps, stamps[1:])]
    assert 17 <= len(stamps) <= 21
    assert statistics.median(gaps) == pytest.approx(METER_TICK_SEC, abs=0.004)
    pulses = [m["data"][0]["data"] for m in w.messages()]
    assert pulses == list(range(1, len(stamps) + 1))
