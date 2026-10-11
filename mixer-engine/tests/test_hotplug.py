"""Device hot-plug, network id persistence, helper identity, Bonjour."""

import errno
import os
from pathlib import Path

import pytest

import bonjour
import hardware
import ua_mixer_daemon as umd
from conftest import ENGINE_DIR, FakeBackend, make_readback
from hardware import HardwareBackend, HardwareRouter
from helper_tree import HelperTree
from ua_mixer_daemon import (DEVICE_REOPEN_MAX, DEVICE_REOPEN_MIN,
                             DEVICE_SETTLE, MixerDaemon, fill_helper_identity,
                             load_network_id)

ONLINE = "/devices/0/DeviceOnline/value"


# ── HardwareBackend: loss detection and reopen ──────────────────────

@pytest.fixture
def node(tmp_path):
    """A regular file standing in for /dev/ua_apollo0 (ioctl -> ENOTTY,
    which the backend treats as a live device on an old driver)."""
    p = tmp_path / "ua_apollo0"
    p.write_bytes(b"")
    return p


class TestBackendProbe:
    def test_alive_on_old_driver(self, node):
        be = HardwareBackend(str(node))
        assert be.open()
        assert be.probe()

    def test_node_removed(self, node):
        be = HardwareBackend(str(node))
        be.open()
        node.unlink()
        assert not be.probe()

    def test_node_replaced(self, node, tmp_path):
        be = HardwareBackend(str(node))
        be.open()
        # Keep the old inode alive so the new file cannot reuse its number
        node.rename(tmp_path / "old")
        node.write_bytes(b"")
        assert not be.probe()

    @pytest.mark.parametrize("err", [errno.ENODEV, errno.EIO])
    def test_driver_reports_removed(self, node, monkeypatch, err):
        be = HardwareBackend(str(node))
        be.open()

        def gone(*a, **k):
            raise OSError(err, "gone")
        monkeypatch.setattr(hardware.fcntl, "ioctl", gone)
        assert not be.probe()

    def test_other_errors_are_not_loss(self, node, monkeypatch):
        be = HardwareBackend(str(node))
        be.open()

        def busy(*a, **k):
            raise OSError(errno.EBUSY, "busy")
        monkeypatch.setattr(hardware.fcntl, "ioctl", busy)
        assert be.probe()

    def test_probe_closed(self):
        assert not HardwareBackend("/nonexistent").probe()


class TestBackendReopen:
    def test_mark_lost_then_reopen(self, node):
        be = HardwareBackend(str(node))
        be.open()
        be._cli_enabled = True
        be._cli_frozen = True
        be.mark_lost("test")
        assert not be.connected and be.lost and be.fd == -1
        assert be.reopen()
        assert be.connected and not be.lost and be.fd >= 0
        assert not be._cli_enabled and not be._cli_frozen
        be.close()

    def test_reopen_while_node_absent(self, node):
        be = HardwareBackend(str(node))
        be.open()
        be.mark_lost()
        node.unlink()
        assert not be.reopen()
        assert not be.connected

    def test_reopen_never_seen(self, tmp_path, monkeypatch):
        be = HardwareBackend(None)
        monkeypatch.setattr(be, "_find_device", lambda: None)
        assert not be.reopen()
        p = tmp_path / "ua_apollo0"
        p.write_bytes(b"")
        monkeypatch.setattr(be, "_find_device", lambda: str(p))
        assert be.reopen()
        assert be.device_path == str(p)
        be.close()

    def test_reopen_rejects_dead_driver(self, node, monkeypatch):
        be = HardwareBackend(str(node))

        def gone(*a, **k):
            raise OSError(errno.ENODEV, "gone")
        monkeypatch.setattr(hardware.fcntl, "ioctl", gone)
        assert not be.reopen()
        assert be.fd == -1

    def test_readback_error_marks_lost(self, node, monkeypatch):
        be = HardwareBackend(str(node))
        be.open()

        def gone(*a, **k):
            raise OSError(errno.ENODEV, "gone")
        monkeypatch.setattr(hardware.fcntl, "ioctl", gone)
        assert be.get_hw_readback() is None
        assert be.lost and not be.connected


# ── Daemon: the watchdog state machine ──────────────────────────────

class HotplugBackend(FakeBackend):
    """FakeBackend with a switchable device."""

    def __init__(self, present=True):
        super().__init__(connected=present)
        self.present = present
        self.reopen_calls = 0
        self.info = {"device_type": 0x1F, "fw_version": 7, "serial": 0xABCD,
                     "fw_v2": False}

    def probe(self):
        return self.connected and self.present

    def mark_lost(self, reason=""):
        self.connected = False

    def reopen(self):
        self.reopen_calls += 1
        if self.present:
            self.connected = True
        return self.present

    def get_device_info(self):
        return self.info if self.connected else None


@pytest.fixture
def hp_backend():
    return HotplugBackend()


@pytest.fixture
def hp_daemon(hp_backend, tree):
    d = MixerDaemon(tree, HardwareRouter(hp_backend, tree))
    d.init_calls = 0
    real = d._push_defaults_to_hardware

    def counted():
        d.init_calls += 1
        real()
    d._push_defaults_to_hardware = counted
    return d


class TestWatchdog:
    def test_unplug_reports_offline(self, hp_daemon, hp_backend, tree):
        seen = []
        tree.register_callback("c1", lambda p, v: seen.append((p, v)))
        tree.subscribe("c1", ONLINE)
        assert tree.get_value(ONLINE) in (True, False)
        hp_daemon._set_device_online(True)
        seen.clear()

        hp_backend.present = False
        assert hp_daemon._device_watchdog_step(10.0)
        assert tree.get_value(ONLINE) is False
        assert (ONLINE, False) in seen
        assert not hp_backend.connected

    def test_replug_reinitialises_and_reports_online(self, hp_daemon,
                                                     hp_backend, tree):
        seen = []
        tree.register_callback("c1", lambda p, v: seen.append((p, v)))
        tree.subscribe("c1", ONLINE)
        hp_backend.readback = (1, make_readback(mon_raw=150))
        hp_backend.driver_values[hardware.DRIVER_PARAM_SAMPLE_RATE] = 96000

        hp_backend.present = False
        hp_daemon._device_watchdog_step(10.0)
        seen.clear()
        init_before = hp_daemon.init_calls

        # Still gone: the retry delay doubles up to the cap
        t = 10.0 + DEVICE_REOPEN_MIN
        delays = []
        for _ in range(8):
            hp_daemon._device_watchdog_step(t)
            delays.append(hp_daemon._next_reopen - t)
            t = hp_daemon._next_reopen
        assert delays[0] == 2 * DEVICE_REOPEN_MIN
        assert delays == sorted(delays) and delays[-1] == DEVICE_REOPEN_MAX
        assert hp_daemon.init_calls == init_before

        # Node returns: reopened, but init waits for the settle time
        hp_backend.present = True
        hp_daemon._device_watchdog_step(t)
        assert hp_backend.connected
        assert tree.get_value(ONLINE) is False
        hp_daemon._device_watchdog_step(t + DEVICE_SETTLE - 0.1)
        assert hp_daemon.init_calls == init_before
        hp_daemon._device_watchdog_step(t + DEVICE_SETTLE + 0.1)

        assert hp_daemon.init_calls == init_before + 1
        assert tree.get_value(ONLINE) is True
        assert (ONLINE, True) in seen
        assert tree.get_value("/SampleRate/value") == 96000
        assert hp_daemon._hw_online

    def test_vanishes_while_settling(self, hp_daemon, hp_backend, tree):
        hp_backend.present = False
        hp_daemon._device_watchdog_step(1.0)
        hp_backend.present = True
        hp_daemon._device_watchdog_step(5.0)        # reopened
        hp_backend.present = False
        hp_daemon._device_watchdog_step(6.0)        # gone again
        assert hp_daemon._ready_at is None
        assert hp_daemon.init_calls == 0
        assert tree.get_value(ONLINE) is False

    def test_starts_without_device(self, tree):
        be = HotplugBackend(present=False)
        d = MixerDaemon(tree, HardwareRouter(be, tree))
        assert tree.get_value(ONLINE) is False
        assert d._hw_init_done          # the watchdog owns the first init
        init = []
        d._push_defaults_to_hardware = lambda: init.append(1)
        d._device_watchdog_step(0.0)    # nothing there yet
        be.present = True
        d._device_watchdog_step(1.0)
        d._device_watchdog_step(1.0 + DEVICE_SETTLE + 0.1)
        assert init == [1]
        assert tree.get_value(ONLINE) is True

    def test_online_state_not_persisted(self, hp_daemon, hp_backend, tree):
        hp_backend.present = False
        hp_daemon._device_watchdog_step(1.0)
        assert ONLINE not in tree._dirty

    def test_pending_gains_dropped_on_loss(self, hp_daemon, hp_backend):
        hp_daemon.hw_router._gain_pending[0] = 33
        hp_backend.present = False
        hp_daemon._device_watchdog_step(1.0)
        assert not hp_daemon.hw_router._gain_pending

    def test_different_model_requests_restart(self, hp_backend, tree,
                                              monkeypatch):
        d = MixerDaemon(tree, HardwareRouter(hp_backend, tree),
                        loaded_map=Path("/maps/x4.json"))
        monkeypatch.setattr(umd, "find_device_map",
                            lambda t, pt=None: Path("/maps/x8p.json"))
        hp_backend.present = False
        d._device_watchdog_step(1.0)
        hp_backend.present = True
        d._device_watchdog_step(5.0)
        assert d._device_watchdog_step(5.0 + DEVICE_SETTLE + 0.1) is False

    def test_same_model_keeps_running(self, hp_backend, tree, monkeypatch):
        m = Path("/maps/x4.json")
        d = MixerDaemon(tree, HardwareRouter(hp_backend, tree), loaded_map=m)
        monkeypatch.setattr(umd, "find_device_map", lambda t, pt=None: m)
        hp_backend.present = False
        d._device_watchdog_step(1.0)
        hp_backend.present = True
        d._device_watchdog_step(5.0)
        assert d._device_watchdog_step(5.0 + DEVICE_SETTLE + 0.1) is True


# ── Network id ──────────────────────────────────────────────────────

class TestNetworkId:
    def test_stable_across_restarts(self, tmp_path):
        f = tmp_path / "x4.network.json"
        first = load_network_id(f)
        assert len(first) == 32
        assert load_network_id(f) == first

    def test_garbage_gets_replaced(self, tmp_path):
        f = tmp_path / "x4.network.json"
        f.write_text("not json")
        nid = load_network_id(f)
        assert load_network_id(f) == nid and len(nid) == 32
        f.write_text('{"network_id": "short"}')
        assert len(load_network_id(f)) == 32

    def test_unwritable_path_still_returns_id(self, tmp_path):
        blocker = tmp_path / "file"
        blocker.write_text("")
        assert len(load_network_id(blocker / "sub" / "n.json")) == 32

    def test_daemon_uses_given_id(self, router, tree):
        assert MixerDaemon(tree, router, network_id="ab" * 16).network_id \
            == "ab" * 16


# ── Helper tree identity ────────────────────────────────────────────

class TestHelperIdentity:
    def tree_with_identity(self):
        t = HelperTree()
        t.load(ENGINE_DIR / "device_maps" / "helper_tree.json")
        return t

    def values(self, t):
        found = {}

        def walk(n):
            props = n.get("properties", {})
            if "serial_number" in props:
                for k in ("serial_number", "firmware_version",
                          "expected_firmware_version",
                          "firmware_compatibility"):
                    found[k] = props[k]["value"]
            for c in n.get("children", []):
                walk(c)
        walk(t.root)
        return found

    def test_fields_filled(self):
        t = self.tree_with_identity()
        assert self.values(t)["serial_number"] == ""
        n = fill_helper_identity(t, {"serial": 0xABCD, "fw_version": 42})
        assert n == 3
        v = self.values(t)
        assert v["serial_number"] == "0000ABCD"
        assert v["firmware_version"] == "42"
        assert v["expected_firmware_version"] == "42"
        assert v["firmware_compatibility"] == 0   # unchanged: no false prompt

    def test_no_info_leaves_tree(self):
        t = self.tree_with_identity()
        assert fill_helper_identity(t, None) == 0
        assert fill_helper_identity(None, {"serial": 1, "fw_version": 1}) == 0
        assert self.values(t)["serial_number"] == ""

    def test_refilled_for_a_new_device(self, hp_daemon, hp_backend):
        hp_daemon.helper_tree = self.tree_with_identity()
        hp_backend.info = {"device_type": 0x1F, "fw_version": 9,
                           "serial": 0x1234, "fw_v2": False}
        hp_backend.present = False
        hp_daemon._device_watchdog_step(1.0)
        hp_backend.present = True
        hp_daemon._device_watchdog_step(5.0)
        hp_daemon._device_watchdog_step(5.0 + DEVICE_SETTLE + 0.1)
        assert self.values(hp_daemon.helper_tree)["serial_number"] == "00001234"


# ── Bonjour ─────────────────────────────────────────────────────────

class FakeZeroconf:
    registered = []
    closed = False

    def register_service(self, info):
        FakeZeroconf.registered.append(info)

    def unregister_service(self, info):
        FakeZeroconf.registered.remove(info)

    def close(self):
        FakeZeroconf.closed = True


class FakeServiceInfo:
    def __init__(self, stype, name, port, **kw):
        self.type, self.name, self.port, self.kw = stype, name, port, kw


@pytest.fixture
def fake_zc(monkeypatch):
    FakeZeroconf.registered = []
    FakeZeroconf.closed = False
    monkeypatch.setattr(bonjour, "HAS_ZEROCONF", True)
    monkeypatch.setattr(bonjour, "Zeroconf", FakeZeroconf, raising=False)
    monkeypatch.setattr(bonjour, "ServiceInfo", FakeServiceInfo, raising=False)
    monkeypatch.setattr(bonjour, "_local_ipv4_addresses",
                        lambda: ["192.168.1.5"])


class TestBonjour:
    def test_mixer_and_helper_advertised(self, fake_zc):
        b = bonjour.BonjourAnnouncer(4710, "Studio", helper_port=4720)
        assert b.start()
        got = {(i.type, i.port) for i in FakeZeroconf.registered}
        assert got == {("_uamixer._tcp.local.", 4710),
                       ("_uamixerhelper._tcp.local.", 4720)}
        assert all(i.name.startswith("Studio.") for i in FakeZeroconf.registered)
        assert all(i.kw["parsed_addresses"] == ["192.168.1.5"]
                   for i in FakeZeroconf.registered)
        b.stop()
        assert FakeZeroconf.registered == [] and FakeZeroconf.closed

    def test_no_helper_port(self, fake_zc):
        b = bonjour.BonjourAnnouncer(4710, "Studio")
        assert b.start()
        assert [i.type for i in FakeZeroconf.registered] == \
            ["_uamixer._tcp.local."]

    def test_missing_zeroconf_falls_back(self, monkeypatch):
        monkeypatch.setattr(bonjour, "HAS_ZEROCONF", False)
        b = bonjour.BonjourAnnouncer(4710, "Studio", helper_port=4720)
        assert b.start() is False
        b.stop()    # harmless

    def test_registration_failure_cleans_up(self, fake_zc, monkeypatch):
        def boom(self, info):
            raise RuntimeError("mdns down")
        monkeypatch.setattr(FakeZeroconf, "register_service", boom)
        b = bonjour.BonjourAnnouncer(4710, "Studio", helper_port=4720)
        assert b.start() is False
        assert b.zc is None
