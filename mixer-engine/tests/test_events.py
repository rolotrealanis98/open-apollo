"""Driver event channel consumer (L1): decoding, daemon reactions, fallback.

The device node is faked with a FIFO that the test writes struct-packed
events into; no hardware, no driver.
"""

import asyncio
import errno
import os
import struct
import time

import pytest

import events
import hardware
from conftest import FakeBackend, make_readback
from events import (DriverEvents, EVENT, UA_EVT_CLOCK_CHANGED,
                    UA_EVT_CLOCK_LOCK, UA_EVT_CONNECTED,
                    UA_EVT_DEVICE_OFFLINE, UA_EVT_EVENTS_LOST,
                    UA_EVT_IODESC_IN, UA_EVT_IODESC_OUT,
                    UA_EVT_LAYOUT_CHANGED, UA_EVT_NOTIF_STATUS,
                    UA_EVT_RATE_CHANGED, UA_EVT_RATE_INFO, decode_events)
from hardware import HardwareRouter
from ua_mixer_daemon import (DEVICE_SETTLE, EVENT_SAFETY_POLL, MixerDaemon)

ONLINE = "/devices/0/DeviceOnline"
RATE = hardware.DRIVER_PARAM_SAMPLE_RATE


def pack(*evs):
    return b"".join(EVENT.pack(code, arg, 1000 + i)
                    for i, (code, arg) in enumerate(evs))


class FifoNode:
    """A fake /dev/ua_apolloN: events written to `put` show up on read()."""

    def __init__(self, tmp_path):
        self.path = str(tmp_path / "ua_apollo_fake")
        os.mkfifo(self.path)
        # O_RDWR keeps the FIFO "has a writer" for as long as we live
        self.w = os.open(self.path, os.O_RDWR | os.O_NONBLOCK)

    def put(self, *evs):
        os.write(self.w, pack(*evs))

    def hang_up(self):
        os.close(self.w)
        self.w = -1

    def close(self):
        if self.w >= 0:
            os.close(self.w)
            self.w = -1


@pytest.fixture
def node(tmp_path):
    n = FifoNode(tmp_path)
    yield n
    n.close()


class EventBackend(FakeBackend):
    """FakeBackend with a device node path and the hot-plug hooks."""

    def __init__(self, path):
        super().__init__(connected=True)
        self.device_path = path
        self.present = True
        self.lost_reasons = []

    def probe(self):
        return self.connected and self.present

    def mark_lost(self, reason=""):
        self.connected = False
        self.lost_reasons.append(reason)

    def reopen(self):
        if self.present:
            self.connected = True
        return self.present

    def get_device_info(self):
        return {"device_type": 0x1F, "fw_version": 7, "serial": 1,
                "fw_v2": False} if self.connected else None


@pytest.fixture
def ebackend(node):
    return EventBackend(node.path)


@pytest.fixture
def edaemon(ebackend, tree):
    d = MixerDaemon(tree, HardwareRouter(ebackend, tree))
    yield d
    d._events_detach()


# ── Decoding and the reader ─────────────────────────────────────────

class TestDecode:
    def test_struct_is_16_bytes(self):
        assert EVENT.size == 16

    def test_decode_records(self):
        data = pack((UA_EVT_RATE_CHANGED, 4), (UA_EVT_CLOCK_LOCK, 1))
        assert decode_events(data) == [(0x67, 4, 1000), (0x1004, 1, 1001)]

    def test_partial_trailing_record_dropped(self):
        assert decode_events(pack((1, 2)) + b"\0\0\0") == [(1, 2, 1000)]

    def test_time_is_u64(self):
        ns = 2 ** 40 + 5
        assert decode_events(EVENT.pack(1, 2, ns)) == [(1, 2, ns)]


class TestReader:
    def test_open_and_drain_dispatch_in_order(self, node):
        seen = []
        node.put((0x67, 3), (0x1000, 0))
        r = DriverEvents(lambda c, a, t: seen.append((c, a)))
        assert r.open(node.path)       # the probe read dispatches too
        assert seen == [(0x67, 3), (0x1000, 0)]
        node.put((0x68, 12))
        assert r.drain() == events.OK
        assert seen[-1] == (0x68, 12)
        r.close()
        assert not r.active

    def test_empty_queue_is_ok(self, node):
        r = DriverEvents(lambda *a: None)
        assert r.open(node.path)
        assert r.drain() == events.OK
        r.close()

    def test_open_failure(self, tmp_path):
        r = DriverEvents(lambda *a: None)
        assert not r.open(str(tmp_path / "nope"))
        assert not r.active

    def test_old_driver_einval(self, node, monkeypatch):
        def einval(fd, n):
            raise OSError(errno.EINVAL, "no read op")
        monkeypatch.setattr(events.os, "read", einval)
        r = DriverEvents(lambda *a: None)
        assert not r.open(node.path)
        assert not r.active          # nothing left open

    def test_enodev_is_offline(self, node, monkeypatch):
        r = DriverEvents(lambda *a: None)
        assert r.open(node.path)

        def enodev(fd, n):
            raise OSError(errno.ENODEV, "gone")
        monkeypatch.setattr(events.os, "read", enodev)
        assert r.drain() == events.OFFLINE
        r.close()

    def test_hangup_is_offline(self, node):
        r = DriverEvents(lambda *a: None)
        assert r.open(node.path)
        node.hang_up()               # EPOLLHUP: read() returns 0
        assert r.drain() == events.OFFLINE
        r.close()

    def test_handler_exception_does_not_stop_the_batch(self, node):
        seen = []

        def handler(c, a, t):
            seen.append(c)
            if c == 1:
                raise RuntimeError("boom")
        r = DriverEvents(handler)
        assert r.open(node.path)
        node.put((1, 0), (2, 0))
        assert r.drain() == events.OK
        assert seen == [1, 2]
        r.close()


# ── The daemon's reactions ──────────────────────────────────────────

def attach(daemon):
    daemon._events_attach()
    assert daemon._events.active
    return daemon


class TestDaemonEvents:
    def test_rate_changed_refreshes_sample_rate(self, edaemon, ebackend,
                                                node, tree):
        attach(edaemon)
        ebackend.driver_values[RATE] = 96000
        node.put((UA_EVT_RATE_CHANGED, 4))
        edaemon._on_events_readable()
        assert tree.get_value("/SampleRate/value") == 96000

    def test_connected_refreshes_sample_rate(self, edaemon, ebackend, node,
                                             tree):
        attach(edaemon)
        ebackend.driver_values[RATE] = 44100
        node.put((UA_EVT_CONNECTED, 0))
        edaemon._on_events_readable()
        assert tree.get_value("/SampleRate/value") == 44100

    def test_rate_info_refreshes_sample_rate(self, edaemon, ebackend, node,
                                             tree):
        attach(edaemon)
        ebackend.driver_values[RATE] = 88200
        node.put((UA_EVT_RATE_INFO, 0xABC))
        edaemon._on_events_readable()
        assert tree.get_value("/SampleRate/value") == 88200

    def test_clients_are_told(self, edaemon, ebackend, node, tree):
        seen = []
        tree.register_callback("c1", lambda p, v: seen.append((p, v)))
        tree.subscribe("c1", "/SampleRate/value")
        attach(edaemon)
        ebackend.driver_values[RATE] = 192000
        node.put((UA_EVT_RATE_CHANGED, 6))
        edaemon._on_events_readable()
        assert ("/SampleRate/value", 192000) in seen

    def test_clock_lock_event_sets_property(self, edaemon, node, tree):
        attach(edaemon)
        node.put((UA_EVT_CLOCK_LOCK, 1))
        edaemon._on_events_readable()
        assert tree.get_value("/ClockLocked/value") is True
        node.put((UA_EVT_CLOCK_LOCK, 0))
        edaemon._on_events_readable()
        assert tree.get_value("/ClockLocked/value") is False

    def test_clock_lock_does_not_write_hardware(self, edaemon, ebackend,
                                                node):
        attach(edaemon)
        ebackend.clear()
        node.put((UA_EVT_CLOCK_LOCK, 1))
        edaemon._on_events_readable()
        assert not ebackend.mixer_params and not ebackend.driver_params

    def test_clock_changed_updates_source(self, edaemon, node, tree):
        attach(edaemon)
        node.put((UA_EVT_CLOCK_CHANGED, hardware.CLOCK_SOURCE_MAP["ADAT"]))
        edaemon._on_events_readable()
        assert tree.get_value("/ClockSource/value") == "ADAT"
        node.put((UA_EVT_CLOCK_CHANGED, hardware.CLOCK_SOURCE_MAP["Internal"]))
        edaemon._on_events_readable()
        assert tree.get_value("/ClockSource/value") == "Internal"

    def test_layout_changed_recorded(self, edaemon, node):
        attach(edaemon)
        node.put((UA_EVT_LAYOUT_CHANGED, (24 << 16) | 22))
        edaemon._on_events_readable()
        assert edaemon.io_layout == (24, 22)
        assert edaemon.layout_changes == 1

    def test_raw_notifications_ignored(self, edaemon, ebackend, node, tree):
        attach(edaemon)
        ebackend.clear()
        before = tree.get_value("/SampleRate/value")
        node.put((UA_EVT_NOTIF_STATUS, 0x10), (UA_EVT_IODESC_IN, 2),
                 (UA_EVT_IODESC_OUT, 1))
        edaemon._on_events_readable()
        assert tree.get_value("/SampleRate/value") == before
        assert edaemon._hw_online

    def test_unknown_code_ignored(self, edaemon, node):
        attach(edaemon)
        node.put((0x7777, 1))
        edaemon._on_events_readable()
        assert edaemon._hw_online

    def test_events_lost_does_a_full_refresh(self, edaemon, ebackend, node,
                                             tree):
        attach(edaemon)
        ebackend.driver_values[RATE] = 96000
        ebackend.driver_values[hardware.DRIVER_PARAM_CLOCK_SOURCE] = \
            hardware.CLOCK_SOURCE_MAP["S/PDIF"]
        ebackend.readback = (1, make_readback(mon_raw=150, clock_lock=True))
        node.put((UA_EVT_EVENTS_LOST, 7))
        edaemon._on_events_readable()
        assert tree.get_value("/SampleRate/value") == 96000
        assert tree.get_value("/ClockSource/value") == "S/PDIF"
        assert tree.get_value("/ClockLocked/value") is True

    def test_events_lost_without_valid_lock_leaves_it(self, edaemon,
                                                      ebackend, node, tree):
        attach(edaemon)
        tree.set("/ClockLocked/value", True)
        ebackend.readback = (1, make_readback(mon_raw=150))   # bit 30 clear
        node.put((UA_EVT_EVENTS_LOST, 1))
        edaemon._on_events_readable()
        assert tree.get_value("/ClockLocked/value") is True


class TestOffline:
    def test_offline_event_marks_device_lost(self, edaemon, ebackend, node,
                                             tree):
        seen = []
        tree.register_callback("c1", lambda p, v: seen.append((p, v)))
        tree.subscribe("c1", ONLINE)
        edaemon._set_device_online(True)
        attach(edaemon)
        seen.clear()
        node.put((UA_EVT_DEVICE_OFFLINE, 0))
        edaemon._on_events_readable()
        assert not edaemon._hw_online
        assert tree.get_value(ONLINE) is False
        assert (ONLINE + "/value", False) in seen
        assert not ebackend.connected and ebackend.lost_reasons
        assert not edaemon._events.active      # event fd released

    def test_surprise_removal_reason(self, edaemon, ebackend, node):
        attach(edaemon)
        node.put((UA_EVT_DEVICE_OFFLINE, 1))
        edaemon._on_events_readable()
        assert "surprise" in ebackend.lost_reasons[0]

    def test_hangup_alone_is_offline(self, edaemon, ebackend, node):
        attach(edaemon)
        node.hang_up()
        edaemon._on_events_readable()
        assert not edaemon._hw_online
        assert not ebackend.connected

    def test_enodev_is_offline(self, edaemon, ebackend, node, monkeypatch):
        attach(edaemon)

        def enodev(fd, n):
            raise OSError(errno.ENODEV, "gone")
        monkeypatch.setattr(events.os, "read", enodev)
        edaemon._on_events_readable()
        assert not edaemon._hw_online

    def test_offline_with_events_after_it_in_the_batch(self, edaemon, node,
                                                       ebackend):
        attach(edaemon)
        node.put((UA_EVT_DEVICE_OFFLINE, 0), (UA_EVT_RATE_CHANGED, 2))
        edaemon._on_events_readable()       # must not raise
        assert not edaemon._hw_online

    def test_watchdog_reopens_and_events_come_back(self, edaemon, ebackend,
                                                   node, tree):
        attach(edaemon)
        node.put((UA_EVT_DEVICE_OFFLINE, 0))
        edaemon._on_events_readable()
        assert not edaemon._events.active

        # The watchdog finds the node again, settles, re-runs init
        edaemon._push_defaults_to_hardware = lambda: None
        t = edaemon._next_reopen
        edaemon._device_watchdog_step(t)               # reopened
        edaemon._device_watchdog_step(t + DEVICE_SETTLE + 0.1)
        assert edaemon._hw_online
        assert edaemon._events.active                  # re-attached
        ebackend.driver_values[RATE] = 48000
        node.put((UA_EVT_RATE_CHANGED, 2))
        edaemon._on_events_readable()
        assert tree.get_value("/SampleRate/value") == 48000

    def test_device_lost_by_watchdog_closes_events(self, edaemon, ebackend):
        attach(edaemon)
        ebackend.present = False
        edaemon._device_watchdog_step(10.0)
        assert not edaemon._events.active


# ── Polling stays the fallback ──────────────────────────────────────

class TestFallback:
    def poll_count(self, daemon, backend, n=5):
        """Run n pump-style polls with the rate changing each time."""
        applied = 0
        for i in range(n):
            backend.driver_values[RATE] = 44100 if i % 2 else 48000
            daemon._poll_driver_rate()
            if daemon.state.get_value("/SampleRate/value") == \
                    backend.driver_values[RATE]:
                applied += 1
        return applied

    def test_no_node_polls_every_call(self, tmp_path, tree):
        be = EventBackend(str(tmp_path / "missing"))
        d = MixerDaemon(tree, HardwareRouter(be, tree))
        d._events_attach()
        assert not d._events.active
        assert self.poll_count(d, be) == 5

    def test_old_driver_polls_every_call(self, node, ebackend, tree,
                                         monkeypatch):
        d = MixerDaemon(tree, HardwareRouter(ebackend, tree))

        def einval(fd, n):
            raise OSError(errno.EINVAL, "no read op")
        monkeypatch.setattr(events.os, "read", einval)
        d._events_attach()
        assert not d._events.active
        assert self.poll_count(d, ebackend) == 5

    def test_no_device_path_polls(self, tree):
        be = FakeBackend()          # device_path None
        d = MixerDaemon(tree, HardwareRouter(be, tree))
        d._events_attach()
        assert not d._events.active
        assert self.poll_count(d, be) == 5

    def test_with_events_the_poll_slows_to_safety_interval(
            self, edaemon, ebackend, tree):
        attach(edaemon)
        edaemon._last_rate_poll = time.monotonic()
        ebackend.driver_values[RATE] = 96000
        edaemon._poll_driver_rate()
        assert tree.get_value("/SampleRate/value") != 96000   # skipped
        edaemon._last_rate_poll = time.monotonic() - EVENT_SAFETY_POLL - 1
        edaemon._poll_driver_rate()
        assert tree.get_value("/SampleRate/value") == 96000   # safety poll

    def test_force_skips_the_wait(self, edaemon, ebackend, tree):
        attach(edaemon)
        edaemon._last_rate_poll = time.monotonic()
        ebackend.driver_values[RATE] = 88200
        edaemon._poll_driver_rate(force=True)
        assert tree.get_value("/SampleRate/value") == 88200

    def test_event_node_stops_answering_falls_back(self, edaemon, ebackend,
                                                   node, monkeypatch):
        attach(edaemon)

        def einval(fd, n):
            raise OSError(errno.EINVAL, "no read op")
        monkeypatch.setattr(events.os, "read", einval)
        edaemon._on_events_readable()
        assert not edaemon._events.active
        assert edaemon._hw_online                   # still running
        assert self.poll_count(edaemon, ebackend) == 5


# ── Wired into the asyncio loop ─────────────────────────────────────

class TestEventLoop:
    def test_add_reader_delivers(self, ebackend, node, tree):
        d = MixerDaemon(tree, HardwareRouter(ebackend, tree))
        ebackend.driver_values[RATE] = 96000

        async def run():
            d._events_attach()
            assert d._events.active and d._events_loop is not None
            node.put((UA_EVT_RATE_CHANGED, 4), (UA_EVT_CLOCK_LOCK, 1))
            for _ in range(100):
                await asyncio.sleep(0.01)
                if tree.get_value("/SampleRate/value") == 96000:
                    break
            d._events_detach()

        asyncio.run(run())
        assert tree.get_value("/SampleRate/value") == 96000
        assert tree.get_value("/ClockLocked/value") is True

    def test_add_reader_offline(self, ebackend, node, tree):
        d = MixerDaemon(tree, HardwareRouter(ebackend, tree))

        async def run():
            d._events_attach()
            node.put((UA_EVT_DEVICE_OFFLINE, 1))
            for _ in range(100):
                await asyncio.sleep(0.01)
                if not d._hw_online:
                    break

        asyncio.run(run())
        assert not d._hw_online
        assert not d._events.active


# ── Mute-until-settle (A11 / 04 H5) ─────────────────────────────────
# The Mixer Engine's MuteUntilSettleDown sets OutputsAttenuate (other param
# 0x20) on mixer types that support it (the x4), never the monitor mute.

ATTEN = (2, 0, 0x20)
MUTE = (2, 0, 0x03)


def atten_writes(backend):
    return [c[3] for c in backend.mixer_params if c[:3] == ATTEN]


def mute_writes(backend):
    return [c[3] for c in backend.mixer_params if c[:3] == MUTE]


class TestMuteUntilSettle:
    def run_event(self, ebackend, node, tree, *evs, secs=0.05, after=0.12,
                  supported=True, setup=None):
        d = MixerDaemon(tree, HardwareRouter(ebackend, tree))
        d.hw_router.supports_outputs_attenuate = supported
        d.mute_on_rate_change = secs
        ebackend.driver_values[RATE] = 48000
        ebackend.readback = (1, make_readback(mon_raw=150))

        async def run():
            d._events_attach()
            if setup:
                setup(d)
            node.put(*evs)
            await asyncio.sleep(after)
            d._events_detach()

        asyncio.run(run())
        return d

    def test_rate_change_attenuates_then_restores(self, ebackend, node,
                                                  tree):
        self.run_event(ebackend, node, tree, (UA_EVT_RATE_CHANGED, 4))
        assert atten_writes(ebackend) == [1, 0]

    def test_monitor_mute_is_never_touched(self, ebackend, node, tree):
        self.run_event(ebackend, node, tree, (UA_EVT_RATE_CHANGED, 4))
        assert mute_writes(ebackend) == []

    def test_unsupported_model_does_nothing(self, ebackend, node, tree):
        self.run_event(ebackend, node, tree, (UA_EVT_RATE_CHANGED, 4),
                       supported=False)
        assert atten_writes(ebackend) == []

    def test_same_rate_does_not_attenuate(self, ebackend, node, tree):
        self.run_event(ebackend, node, tree, (UA_EVT_RATE_CHANGED, 2))
        assert atten_writes(ebackend) == []

    def test_disabled_with_zero_seconds(self, ebackend, node, tree):
        self.run_event(ebackend, node, tree, (UA_EVT_RATE_CHANGED, 4),
                       secs=0)
        assert atten_writes(ebackend) == []

    def test_first_event_after_a_rate_unknown_start_does_not_attenuate(
            self, ebackend, node, tree):
        d = MixerDaemon(tree, HardwareRouter(ebackend, tree))
        d.hw_router.supports_outputs_attenuate = True
        d.mute_on_rate_change = 0.05
        d._on_driver_event(UA_EVT_RATE_CHANGED, 4)
        assert atten_writes(ebackend) == []

    def test_no_event_loop_no_attenuation(self, edaemon, ebackend):
        edaemon.hw_router.supports_outputs_attenuate = True
        edaemon._last_rate_index = 2
        edaemon._on_driver_event(UA_EVT_RATE_CHANGED, 4)    # not in a loop
        assert atten_writes(ebackend) == []

    def test_connect_and_clock_events_do_not_attenuate(self, ebackend, node,
                                                       tree):
        self.run_event(ebackend, node, tree, (UA_EVT_CONNECTED, 0),
                       (UA_EVT_CLOCK_CHANGED, 12), (UA_EVT_RATE_INFO, 1))
        assert atten_writes(ebackend) == []

    def test_device_lost_cancels_without_writing(self, ebackend, node, tree):
        d = MixerDaemon(tree, HardwareRouter(ebackend, tree))
        d.hw_router.supports_outputs_attenuate = True
        d.mute_on_rate_change = 5.0
        ebackend.driver_values[RATE] = 48000

        async def run():
            d._events_attach()
            node.put((UA_EVT_RATE_CHANGED, 4))
            await asyncio.sleep(0.05)
            assert d.hw_router._settle is not None
            d._device_lost(0.0)
            assert d.hw_router._settle is None and d._settle_timer is None

        asyncio.run(run())
        assert atten_writes(ebackend) == [1]     # never restored on a lost fd


class TestSettleRouter:
    @pytest.fixture
    def srouter(self, router):
        router.supports_outputs_attenuate = True
        return router

    def test_begin_end(self, srouter, backend):
        assert srouter.begin_settle_mute()
        assert srouter.end_settle_mute()
        assert atten_writes(backend) == [1, 0]

    def test_second_begin_keeps_the_hold(self, srouter, backend):
        srouter.begin_settle_mute()
        assert srouter.begin_settle_mute()
        assert atten_writes(backend) == [1]

    def test_unsupported_writes_nothing(self, router, backend):
        assert not router.begin_settle_mute()
        assert backend.mixer_params == []
        assert not router.end_settle_mute()

    def test_user_attenuate_set_during_the_hold_is_kept(self, srouter,
                                                        backend):
        srouter.begin_settle_mute()
        backend.clear()
        srouter.on_set("/devices/0/OutputsAttenuate/value", True)
        assert atten_writes(backend) == []          # held, not written
        srouter.end_settle_mute()
        assert atten_writes(backend) == [1]         # stays attenuated

    def test_monitor_mute_during_the_hold_is_written(self, srouter, backend):
        mon = srouter.monitor_output_index()
        srouter.begin_settle_mute()
        backend.clear()
        srouter.on_set(f"/devices/0/outputs/{mon}/Mute/value", True)
        assert mute_writes(backend) == [2]          # not held any more

    def test_end_without_begin(self, srouter, backend):
        assert not srouter.end_settle_mute()
        assert backend.mixer_params == []


class TestOutputsAttenuateModels:
    def test_classes_from_the_descriptors(self):
        import ua_mixer_daemon as d
        assert d.descriptor_mixer_class("apollo-x4") == "Freud"
        assert "Freud" in d.OUTPUTS_ATTENUATE_CLASSES
        assert d.descriptor_mixer_class("apollo-x8p") in \
            d.OUTPUTS_ATTENUATE_CLASSES                      # Rene8
        assert d.descriptor_mixer_class("apollo-8p") not in \
            d.OUTPUTS_ATTENUATE_CLASSES                      # Peyton: Base
        assert d.descriptor_mixer_class(None) is None
        assert d.descriptor_mixer_class("no-such-model") is None
