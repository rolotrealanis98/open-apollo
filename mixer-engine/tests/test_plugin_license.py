"""L3: /plugins/N licence status from the device's own answer.

Everything runs against fakes: the board query is a Python callable or a
monkeypatched fcntl.ioctl, the manifest and the catalogue are tiny
synthetic files, the device node is a FIFO.  No driver, no device.
"""

import asyncio
import errno
import json
import logging
import struct
import threading
import time

import pytest

import hardware
import plugin_license as pl
import ua_catalogue as uac
from commands import ERR_FAILED, ERR_UNRESOLVED
from conftest import DEVICE_MAP_X4, FakeBackend
from console_commands import ConsoleCommands
from events import (UA_EVT_CONNECTED, UA_EVT_EVENTS_LOST, UA_EVT_RATE_CHANGED,
                    UA_EVT_RATE_INFO)
from hardware import HardwareBackend, HardwareRouter
from state_tree import StateTree
from test_events import FifoNode, attach
from test_post_commands import Client
from ua_mixer_daemon import MixerDaemon

FEATURES2 = hardware.UA_FEAT_FEATURES2
PLUGIN_HOST = hardware.UA_FEAT2_PLUGIN_HOST

AUTH_A, AUTH_B = 0x81000000, 0x82000000
NOT_STARTED, EXPIRED, UNAVAILABLE = 0x80000000, 0x00000000, 0x83000000

# A synthetic catalogue (/plugins/N in this order) and its render ids
PLUGINS = [
    {"name": "Test Authorized", "plugin_id": "t_auth"},
    {"name": "Test Demo", "plugin_id": "t_demo"},
    {"name": "Test New", "plugin_id": "t_new"},
    {"name": "Test Old", "plugin_id": "t_old"},
    {"name": "Test Gone", "plugin_id": "t_gone"},
    {"name": "Test No Render", "plugin_id": "t_none"},
    {"name": "Test Unlisted", "plugin_id": "t_unlisted"},
    {"name": "Test Low", "plugin_id": "t_low"},
    {"name": "Test High", "plugin_id": "t_high"},
    {"name": "Test Last", "plugin_id": "t_last"},
    {"name": "Test No Id"},
]
RENDER = {"t_auth": 0x810, "t_demo": 0x811, "t_new": 0x812, "t_old": 0x813,
          "t_gone": 0x814, "t_none": None, "t_low": 0x7FF, "t_high": 0xB00,
          "t_last": 0xAFF}

# 12 whole days and a partial one: 13 days left
DEMO_WORD = (12 << 12) | 300

AUTHORIZED = {"Authorized": True, "Status": "Authorized for all devices",
              "Demoing": True, "DemoDaysRemaining": 0}
DEMO_13 = {"Authorized": False, "Status": "Demo (13 days left)",
           "Demoing": True, "DemoDaysRemaining": 13}
NEW = {"Authorized": False, "Status": "Demo not started", "Demoing": False,
       "DemoDaysRemaining": 0}
OLD = {"Authorized": False, "Status": "Demo expired", "Demoing": False,
       "DemoDaysRemaining": 0}
UNKNOWN = {"Authorized": False, "Status": "", "Demoing": False,
           "DemoDaysRemaining": 0}

EXPECTED = [AUTHORIZED, DEMO_13, NEW, OLD, UNKNOWN, UNKNOWN, UNKNOWN,
            UNKNOWN, UNKNOWN, AUTHORIZED, UNKNOWN]


def table(**by_rid):
    """768 words, the driver's unfilled sentinel except where given
    (keyword r<hex render id>=word)."""
    words = [UNAVAILABLE] * 768
    for key, word in by_rid.items():
        words[int(key[1:], 16) - 0x800] = word
    return tuple(words)


def full_table():
    return table(r810=AUTH_A, r811=DEMO_WORD, r812=NOT_STARTED, r813=EXPIRED,
                 r814=UNAVAILABLE, rAFF=AUTH_B)


def write_manifest(path, render=RENDER, fmt=pl.MANIFEST_FORMAT, extra=()):
    entries = [{"plugin_id": pid, "render_id": rid, "name": "x"}
               for pid, rid in render.items()]
    entries += list(extra)
    path.write_text(json.dumps({"format": fmt, "plugins": entries,
                                "modules": []}))
    return path


class Recorder:
    """A publish callable that keeps the last value of every path."""

    def __init__(self):
        self.calls = []
        self.values = {}

    def __call__(self, path, value, kind):
        self.calls.append((path, value, kind))
        self.values[path] = value

    def status(self, index):
        return {key: self.values.get(f"/plugins/{index}/{key}")
                for key, _ in pl.FIELDS}


class Clock:
    def __init__(self, now=100.0):
        self.now = now

    def __call__(self):
        return self.now


def mirror(query, clock=None, render=RENDER):
    rec = Recorder()
    m = pl.LicenseMirror(rec, PLUGINS, {k: v for k, v in render.items()
                                        if v is not None},
                         query, clock=clock or Clock())
    return m, rec


# ── Decoding ────────────────────────────────────────────────────────

class TestStates:
    @pytest.mark.parametrize("word,state", [
        (0x81000000, pl.AUTHORIZED), (0x82000000, pl.AUTHORIZED),
        (0x80000000, pl.NOT_STARTED), (0x00000000, pl.EXPIRED),
        (0x83000000, pl.UNAVAILABLE), (DEMO_WORD, pl.DEMO), (1, pl.DEMO),
        (0x81000001, pl.DEMO), (0x84000000, pl.DEMO), (0xFFFFFFFF, pl.DEMO),
        (0x80001000, pl.DEMO)])
    def test_state_of(self, word, state):
        assert pl.state_of(word) == state

    @pytest.mark.parametrize("word,days", [
        (1 << 12, 1),                 # exactly one day
        ((1 << 12) | 1, 2),           # a minute more rounds up
        (0x001, 1),                   # a partial day only
        ((13 << 12) | 0xFFF, 14),
        (DEMO_WORD, 13),
        (365 << 12, 365),
        ((365 << 12) | 1, 365),       # capped as the SDK caps it
        (400 << 12, 365),
        (0x7FFFFFFF, 365),
        (0x80001000, 1),              # bit 31 is not part of the countdown
    ])
    def test_demo_days(self, word, days):
        assert pl.demo_days(word) == days

    def test_status_strings_are_the_mac_engines(self):
        assert pl.status_for(pl.AUTHORIZED) == AUTHORIZED
        assert pl.status_for(pl.DEMO, 13) == DEMO_13
        assert pl.status_for(pl.NOT_STARTED) == NEW
        assert pl.status_for(pl.EXPIRED) == OLD

    def test_demo_count_is_plural_and_clamped(self):
        assert pl.status_for(pl.DEMO, 1)["Status"] == "Demo (1 days left)"
        assert pl.status_for(pl.DEMO, 900)["DemoDaysRemaining"] == 365

    @pytest.mark.parametrize("state", [pl.UNAVAILABLE, None, 5, -1, True,
                                       "0", 0.0])
    def test_nothing_is_invented(self, state):
        assert pl.status_for(state, 10) == UNKNOWN

    def test_status_dicts_are_fresh(self):
        a = pl.status_for(None)
        a["Status"] = "changed"
        assert pl.status_for(None)["Status"] == ""


class TestRenderIdBounds:
    def test_first_and_last_word(self):
        words = table(r800=AUTH_A, rAFF=NOT_STARTED)
        assert pl.word_for(words, 0x800) == AUTH_A
        assert pl.word_for(words, 0xAFF) == NOT_STARTED

    @pytest.mark.parametrize("rid", [0x7FF, 0xB00, -1, 0, None, True, "2064",
                                     2064.0])
    def test_outside_the_table_is_unknown(self, rid):
        words = full_table()
        assert pl.word_for(words, rid) is None
        assert pl.decide(words, rid) == (None, UNKNOWN)

    def test_no_table_or_a_short_one_is_unknown(self):
        assert pl.decide(None, 0x810) == (None, UNKNOWN)
        assert pl.decide((AUTH_A,) * 767, 0x810) == (None, UNKNOWN)

    def test_decide(self):
        words = full_table()
        assert pl.decide(words, 0x810) == (pl.AUTHORIZED, AUTHORIZED)
        assert pl.decide(words, 0x811) == (pl.DEMO, DEMO_13)
        assert pl.decide(words, 0x814) == (pl.UNAVAILABLE, UNKNOWN)


# ── The manifest ────────────────────────────────────────────────────

class TestManifest:
    def test_loads_plugin_ids_and_render_ids(self, tmp_path):
        p = write_manifest(tmp_path / "manifest.json", extra=[
            {"plugin_id": None, "render_id": 0x900},
            {"render_id": 0x901},
            {"plugin_id": "t_str", "render_id": "2306"},
            {"plugin_id": "t_bool", "render_id": True},
            "not an entry"])
        ids = pl.load_render_ids(p)
        assert ids == {k: v for k, v in RENDER.items() if v is not None}

    def test_conflicting_entries_are_left_out(self, tmp_path, caplog):
        p = write_manifest(tmp_path / "manifest.json", extra=[
            {"plugin_id": "t_auth", "render_id": 0x820},
            {"plugin_id": "t_demo", "render_id": 0x811}])
        ids = pl.load_render_ids(p)
        assert "t_auth" not in ids
        assert ids["t_demo"] == 0x811
        assert "more than one render id" in caplog.text

    def test_missing_manifest(self, tmp_path):
        assert pl.load_render_ids(tmp_path / "nope.json") == {}

    def test_other_format_or_broken_file(self, tmp_path):
        assert pl.load_render_ids(write_manifest(
            tmp_path / "a.json", fmt="open-apollo-plugin-dsp/2")) == {}
        bad = tmp_path / "b.json"
        bad.write_text("{not json")
        assert pl.load_render_ids(bad) == {}
        bad.write_text(json.dumps({"format": pl.MANIFEST_FORMAT,
                                   "plugins": {}}))
        assert pl.load_render_ids(bad) == {}

    def test_default_path(self, tmp_path, monkeypatch):
        assert str(pl.DEFAULT_MANIFEST) == \
            "/lib/firmware/universal-audio/plugins/manifest.json"
        p = write_manifest(tmp_path / "manifest.json")
        monkeypatch.setattr(pl, "DEFAULT_MANIFEST", p)
        assert pl.load_render_ids()["t_last"] == 0xAFF

    def test_catalogue_order(self):
        assert pl.catalogue_render_ids(PLUGINS, {"t_demo": 0x811,
                                                 "t_auth": 0x810}) == \
            [0x810, 0x811] + [None] * (len(PLUGINS) - 2)


# ── The mirror ──────────────────────────────────────────────────────

class TestMirror:
    def test_publishes_every_plugin(self):
        m, rec = mirror(lambda: (0x1234, 9, full_table()))
        assert m.refresh()
        assert [rec.status(i) for i in range(len(PLUGINS))] == EXPECTED
        assert {(p.rsplit("/", 1)[1], k) for p, _, k in rec.calls} == \
            set(pl.FIELDS)
        assert len(rec.calls) == 4 * len(PLUGINS)
        assert m.showing_answer and m.last_error is None
        assert dict(m.last_counts) == {
            "authorized": 2, "demo": 1, "not_started": 1, "expired": 1,
            "unavailable": 1, "unknown": 5}

    def test_into_a_real_state_tree(self, tree):
        def publish(path, value, kind):
            prop = tree.get_prop_dict(path)
            if prop is None:
                tree.add_runtime_property(path, {"type": kind, "value": value})
            else:
                prop["value"] = value
        m = pl.LicenseMirror(publish, PLUGINS, RENDER, lambda: (
            0, 0, full_table()), clock=Clock())
        m.refresh()
        assert tree.get_value("/plugins/0/Authorized") is True
        assert tree.get_value("/plugins/1/Status") == "Demo (13 days left)"
        assert tree.get_value("/plugins/1/DemoDaysRemaining") == 13
        assert tree.get_prop_dict("/plugins/1/DemoDaysRemaining")["type"] \
            == "int"

    @pytest.mark.parametrize("failure", [
        OSError(errno.ETIMEDOUT, "no answer"), OSError(errno.EPROTO, "x"),
        OSError(errno.ESTALE, "x"), OSError(errno.EOPNOTSUPP, "x"),
        ValueError("odd"), RuntimeError("odd")])
    def test_failure_publishes_unknown_everywhere(self, failure):
        clock = Clock()
        answers = [full_table()]

        def query():
            if not answers:
                raise failure
            return 0, 0, answers.pop()
        m, rec = mirror(query, clock)
        m.refresh()
        assert rec.status(0) == AUTHORIZED
        clock.now += 10
        assert m.refresh()
        assert [rec.status(i) for i in range(len(PLUGINS))] == \
            [UNKNOWN] * len(PLUGINS)
        assert not m.showing_answer
        assert m.last_error == getattr(failure, "errno", errno.EIO)

    @pytest.mark.parametrize("answer", [
        None, (0, 0), (0, 0, (AUTH_A,) * 767), (0, 0, (AUTH_A,) * 769),
        (0, 0, (AUTH_A,) * 767 + (-1,)), (0, 0, (AUTH_A,) * 767 + (1 << 32,)),
        (0, 0, (AUTH_A,) * 767 + ("x",))])
    def test_answer_of_the_wrong_shape_is_a_failure(self, answer):
        m, rec = mirror(lambda: answer)
        m.refresh()
        assert rec.status(0) == UNKNOWN and m.last_error == errno.EIO

    def test_rate_limit(self):
        clock = Clock()
        calls = []

        def query():
            calls.append(clock.now)
            return 0, 0, full_table()
        m, _ = mirror(query, clock)
        assert m.refresh()
        clock.now += 4.9
        assert not m.refresh()
        assert m.wait_time() == pytest.approx(0.1)
        assert m.begin() is None
        clock.now += 0.1
        assert m.refresh()
        assert calls == [100.0, 105.0]
        assert m.queries == 2

    def test_invalidate_withdraws_and_drops_results_in_flight(self):
        m, rec = mirror(lambda: (0, 0, full_table()))
        m.refresh()
        assert rec.status(0) == AUTHORIZED
        m._last_query = None
        token = m.begin()
        words = m.fetch()
        n = len(rec.calls)
        m.invalidate()                      # e.g. RATE_CHANGED meanwhile
        assert rec.status(0) == UNKNOWN
        assert len(rec.calls) == n + 4 * len(PLUGINS)
        assert not m.apply(words, token)    # the old epoch's answer
        assert rec.status(0) == UNKNOWN
        m.invalidate()                      # nothing on show: nothing sent
        assert len(rec.calls) == n + 4 * len(PLUGINS)

    def test_render_ids_can_be_replaced(self):
        m, rec = mirror(lambda: (0, 0, full_table()))
        m.render_ids = {"t_new": 0x810}
        m.refresh()
        assert rec.status(0) == UNKNOWN
        assert rec.status(2) == AUTHORIZED

    def test_the_table_is_never_logged(self, caplog):
        caplog.set_level(logging.DEBUG)
        words = list(full_table())
        words[0x900 - 0x800] = 0x8ABCDEF1     # a word nothing else contains
        m, _ = mirror(lambda: (0x7E57, 0x5EED, tuple(words)))
        m.refresh()
        text = caplog.text
        for needle in ("8abcdef1", "8ABCDEF1", str(0x8ABCDEF1), "2164260864",
                       "81000000", str(DEMO_WORD), "7e57", str(0x7E57)):
            assert needle not in text
        assert ("2 authorized, 1 demo running, 1 demo not started, 1 demo "
                "expired, 1 unavailable, 5 unknown") in text


# ── The ioctl ───────────────────────────────────────────────────────

class FakeIoctl:
    """Patches hardware.fcntl.ioctl: GET_VERSION answers with `features`
    and reserved[0] = `features2` (None = a driver without GET_VERSION);
    BOARD_QUERY fills the caller's buffer or raises `error`."""

    def __init__(self, monkeypatch, features=0, features2=0, words=None,
                 error=None):
        self.features, self.features2 = features, features2
        self.words = words if words is not None else tuple(range(768))
        self.error = error
        self.calls = []
        monkeypatch.setattr(hardware.fcntl, "ioctl", self.ioctl)

    def ioctl(self, fd, req, buf, mutate=None):
        self.calls.append((fd, req, len(buf), type(buf), mutate,
                           bytes(buf)))
        if req == hardware.UA_IOCTL_GET_VERSION:
            if self.features is None:
                raise OSError(errno.ENOTTY, "no")
            return struct.pack('<IIII16s4I', 1, 8, 0x10000, self.features,
                               b"0.3.0", self.features2, 0, 0, 0)
        if req == hardware.UA_IOCTL_BOARD_QUERY:
            if self.error is not None:
                raise OSError(self.error, "fake")
            struct.pack_into('<IIIIQ', buf, 0, 3096, 0, 0x01234567, 0,
                             0x1122334455667788)
            struct.pack_into('<768I', buf, 24, *self.words)
            return 0
        raise OSError(errno.ENOTTY, "unexpected")


def backend(monkeypatch, **kw):
    drv = FakeIoctl(monkeypatch, **kw)
    be = HardwareBackend()
    be.fd = 3           # never used: every ioctl is the fake's
    be.connected = True
    return be, drv


class TestIoctl:
    def test_number_and_size(self):
        assert hardware._BOARD_QUERY_SIZE == 3096
        # _IOWR('U', 0x90, 3096): the number the C header computes
        assert hardware.UA_IOCTL_BOARD_QUERY == 0xCC185590
        assert hardware.UA_IOCTL_BOARD_QUERY == \
            (3 << 30) | (3096 << 16) | (0x55 << 8) | 0x90
        assert hardware.UA_BOARD_QUERY_WORDS == pl.BOARD_WORDS
        assert hardware.UA_BOARD_QUERY_BASE == pl.BOARD_BASE
        assert (FEATURES2, PLUGIN_HOST) == (pl._FEAT_FEATURES2,
                                            pl._FEAT2_PLUGIN_HOST)

    def test_board_query_packs_and_unpacks(self, monkeypatch):
        words = tuple((i * 0x01010101) & 0xFFFFFFFF for i in range(768))
        be, drv = backend(monkeypatch, words=words)
        date, epoch, got = be.board_query()
        assert (date, epoch, got) == (0x01234567, 0x1122334455667788, words)
        fd, req, n, kind, mutate, sent = drv.calls[-1]
        assert (fd, req, n, kind, mutate) == \
            (3, hardware.UA_IOCTL_BOARD_QUERY, 3096, bytearray, True)
        # struct_size in, everything else zero
        assert struct.unpack_from('<I', sent)[0] == 3096
        assert not any(sent[4:])

    @pytest.mark.parametrize("err", [errno.EOPNOTSUPP, errno.EBUSY,
                                     errno.ETIMEDOUT, errno.EPROTO,
                                     errno.ESTALE, errno.ERANGE, errno.ENODEV,
                                     errno.ENOTTY])
    def test_errors_raise_with_the_driver_errno(self, monkeypatch, err):
        be, _ = backend(monkeypatch, error=err)
        with pytest.raises(OSError) as e:
            be.board_query()
        assert e.value.errno == err

    def test_no_device_never_calls_the_driver(self, monkeypatch):
        drv = FakeIoctl(monkeypatch)
        be = HardwareBackend()
        with pytest.raises(OSError) as e:
            be.board_query()
        assert e.value.errno == errno.ENODEV and drv.calls == []

    def test_mirror_over_the_backend(self, monkeypatch):
        be, _ = backend(monkeypatch, words=full_table())
        m, rec = mirror(be.board_query)
        m.refresh()
        assert [rec.status(i) for i in range(len(PLUGINS))] == EXPECTED


class TestFeatureDetection:
    def test_plugin_host_on(self, monkeypatch):
        be, _ = backend(monkeypatch, features=FEATURES2 | 0x3FFFF,
                        features2=PLUGIN_HOST)
        assert be.get_version()["features2"] == PLUGIN_HOST
        assert be.features2() == PLUGIN_HOST
        assert be.plugin_host_available()
        assert pl.plugin_host_available(be)

    def test_plugin_host_off_keeps_the_old_answers(self, monkeypatch):
        be, _ = backend(monkeypatch, features=0x3FFFF, features2=0)
        v = be.get_version()
        assert "features2" not in v and v["features"] == 0x3FFFF
        assert be.features() == 0x3FFFF
        assert be.features2() == 0
        assert not be.plugin_host_available()
        assert not pl.plugin_host_available(be)

    def test_second_word_without_bit_31_is_ignored(self, monkeypatch):
        be, _ = backend(monkeypatch, features=0x3FFFF, features2=PLUGIN_HOST)
        assert be.features2() == 0 and not be.plugin_host_available()

    def test_bit_31_without_the_plugin_host_bit(self, monkeypatch):
        be, _ = backend(monkeypatch, features=FEATURES2, features2=0)
        assert be.features2() == 0 and not be.plugin_host_available()

    def test_old_driver_and_no_device(self, monkeypatch):
        be, _ = backend(monkeypatch, features=None)
        assert not be.plugin_host_available()
        assert not HardwareBackend().plugin_host_available()
        assert not pl.plugin_host_available(None)

    def test_cached_per_device_session(self, monkeypatch):
        be, drv = backend(monkeypatch, features=FEATURES2,
                          features2=PLUGIN_HOST)
        assert be.plugin_host_available()
        drv.features, drv.features2 = 0, 0
        assert be.plugin_host_available()       # same session
        be.fd = -1                              # (no os.close of a fake fd)
        assert not be.plugin_host_available()   # no device
        be.fd, be._features = 3, None           # what open() resets
        assert not be.plugin_host_available()   # asked again

    def test_fake_and_usb_backends_have_no_plugin_host(self):
        # conftest's FakeBackend answers any unknown call with True: the
        # check must not take that for the feature
        assert not pl.plugin_host_available(FakeBackend())
        from hardware_usb import HardwareUSB
        usb = HardwareUSB.__new__(HardwareUSB)
        assert usb.features2() == 0
        assert not usb.plugin_host_available()
        assert not pl.plugin_host_available(usb)
        with pytest.raises(OSError) as e:
            usb.board_query()
        assert e.value.errno == errno.EOPNOTSUPP


# ── The daemon ──────────────────────────────────────────────────────

class HostBackend(FakeBackend):
    """FakeBackend whose driver hosts plug-ins: board_query answers from
    `answers` (a table, an errno to raise, or a callable)."""

    def __init__(self, path=None, plugin_host=True):
        super().__init__(connected=True)
        self.device_path = path
        self.features = FEATURES2 if plugin_host else 0
        self._f2 = PLUGIN_HOST if plugin_host else 0
        self.answers = []
        self.default = full_table()
        self.queries = []           # thread ident of every query
        self.present = True

    def features2(self):
        return self._f2

    def board_query(self):
        self.queries.append(threading.get_ident())
        a = self.answers.pop(0) if self.answers else self.default
        if callable(a):
            a = a()
        if isinstance(a, int):
            raise OSError(a, "fake")
        return 0x4000, len(self.queries), a

    def probe(self):
        return self.connected and self.present

    def mark_lost(self, reason=""):
        self.connected = False

    def reopen(self):
        self.connected = self.present
        return self.present

    def get_device_info(self):
        return {"device_type": 0x1F, "fw_version": 7, "serial": 1,
                "fw_v2": False} if self.connected else None


def make_daemon(tmp_path, be, *, catalogue=True, manifest=True):
    tree = StateTree()
    tree.load_device_map(DEVICE_MAP_X4)
    cat = None
    if catalogue:
        cat = uac.Catalogue(directory=None, plugins=PLUGINS)
        uac.apply_to_state(tree, cat, unison_ids=frozenset())
    d = MixerDaemon(tree, HardwareRouter(be, tree), software_only=False,
                    sessions_dir=tmp_path / "sessions",
                    presets_dir=tmp_path / "presets", catalogue=cat)
    d.plugin_manifest = (write_manifest(tmp_path / "manifest.json")
                         if manifest else tmp_path / "absent.json")
    return d


def status(d, index):
    return {key: d.state.get_value(f"/plugins/{index}/{key}")
            for key, _ in pl.FIELDS}


def statuses(d):
    return [status(d, i) for i in range(len(PLUGINS))]


@pytest.fixture
def no_real_manifest(tmp_path, monkeypatch):
    monkeypatch.setattr(pl, "DEFAULT_MANIFEST", tmp_path / "no-manifest.json")


@pytest.mark.usefixtures("no_real_manifest")
class TestDaemon:
    def test_feature_off_changes_nothing(self, tmp_path):
        be = HostBackend(plugin_host=False)
        d = make_daemon(tmp_path, be)
        d._on_device_connected()
        d._on_driver_event(UA_EVT_RATE_CHANGED, 4)
        d._on_driver_event(UA_EVT_CONNECTED, 0)
        assert be.queries == [] and d.licenses is None
        assert d.state.get_prop_dict("/plugins/0/Authorized") is None
        assert d.state.get_value("/plugins/0/Status") == ""
        assert d.state.get_value("/plugins/0/Demoing") is False
        assert d.state.get_value("/plugins/0/DemoAvailable") is False

    def test_no_catalogue_no_query(self, tmp_path):
        be = HostBackend()
        d = make_daemon(tmp_path, be, catalogue=False)
        d._on_device_connected()
        assert be.queries == [] and d.licenses is None
        assert not [p for p in d.state.all_paths()
                    if p.startswith("/plugins/")]

    def test_session_start_publishes_the_answer(self, tmp_path):
        be = HostBackend()
        d = make_daemon(tmp_path, be)
        seen = []
        d.state.register_callback("c1", lambda p, v: seen.append((p, v)))
        d.state.subscribe("c1", "/plugins/0/Status/value")
        d._on_device_connected()
        assert len(be.queries) == 1
        assert statuses(d) == EXPECTED
        assert ("/plugins/0/Status/value", "Authorized for all devices") \
            in seen
        # device status: never saved, not a session edit, DemoAvailable
        # left as Console's default
        assert not [p for p in d.state._dirty if p.startswith("/plugins/")]
        assert d.state.get_value("/Dirty") is False
        assert d.state.get_value("/plugins/0/DemoAvailable") is False
        assert d.state.get_prop_dict("/plugins/0/Authorized")["type"] == \
            "bool"

    def test_unchanged_values_are_not_sent_again(self, tmp_path):
        be = HostBackend()
        d = make_daemon(tmp_path, be)
        d._on_device_connected()
        seen = []
        d.state.register_callback("c1", lambda p, v: seen.append(p))
        for i in range(len(PLUGINS)):
            d.state.subscribe("c1", f"/plugins/{i}/Status/value")
        d.licenses._clock = Clock(1e9)
        d._license_request()
        assert len(be.queries) == 2 and seen == []

    def test_without_a_manifest_everything_stays_unknown(self, tmp_path):
        be = HostBackend()
        d = make_daemon(tmp_path, be, manifest=False)
        d._on_device_connected()
        assert statuses(d) == [UNKNOWN] * len(PLUGINS)

    def test_failure_publishes_unknown(self, tmp_path):
        be = HostBackend()
        d = make_daemon(tmp_path, be)
        d._on_device_connected()
        assert status(d, 0) == AUTHORIZED
        be.answers = [errno.ETIMEDOUT]
        d.licenses._clock = Clock(1e9)
        d._on_driver_event(UA_EVT_CONNECTED, 0)
        assert statuses(d) == [UNKNOWN] * len(PLUGINS)

    @pytest.mark.parametrize("code", [UA_EVT_CONNECTED, UA_EVT_RATE_CHANGED,
                                      UA_EVT_EVENTS_LOST])
    def test_events_ask_again(self, tmp_path, code):
        be = HostBackend()
        d = make_daemon(tmp_path, be)
        d._on_device_connected()
        clock = Clock(1e9)
        d.licenses._clock = clock
        be.default = table(r810=NOT_STARTED)
        d._on_driver_event(code, 4)
        assert len(be.queries) == 2
        assert status(d, 0) == NEW and status(d, 1) == UNKNOWN

    def test_rate_info_does_not_ask(self, tmp_path):
        be = HostBackend()
        d = make_daemon(tmp_path, be)
        d._on_device_connected()
        d.licenses._clock = Clock(1e9)
        d._on_driver_event(UA_EVT_RATE_INFO, 0)
        assert len(be.queries) == 1

    def test_event_inside_the_rate_limit_withdraws_the_answer(self, tmp_path):
        be = HostBackend()
        d = make_daemon(tmp_path, be)
        clock = Clock()
        d._on_device_connected()
        d.licenses._clock = clock
        d.licenses._last_query = clock.now
        clock.now += 1.0
        d._on_driver_event(UA_EVT_RATE_CHANGED, 4)
        assert len(be.queries) == 1                 # not yet
        assert statuses(d) == [UNKNOWN] * len(PLUGINS)

    def test_events_from_the_device_node(self, tmp_path):
        node = FifoNode(tmp_path)
        be = HostBackend(node.path)
        d = make_daemon(tmp_path, be)
        try:
            d._on_device_connected()
            attach(d)
            d.licenses._clock = Clock(1e9)
            be.default = table(r811=(2 << 12))
            node.put((UA_EVT_RATE_CHANGED, 5))
            d._on_events_readable()
            assert len(be.queries) == 2
            assert status(d, 1)["Status"] == "Demo (2 days left)"
            d.licenses._clock.now += 10
            be.default = full_table()
            node.put((UA_EVT_CONNECTED, 0))
            d._on_events_readable()
            assert len(be.queries) == 3
            assert statuses(d) == EXPECTED
        finally:
            d._events_detach()
            node.close()

    def test_device_lost_withdraws_and_stops(self, tmp_path):
        be = HostBackend()
        d = make_daemon(tmp_path, be)
        d._on_device_connected()
        assert status(d, 0) == AUTHORIZED
        be.connected = False
        d._device_lost(time.monotonic())
        assert statuses(d) == [UNKNOWN] * len(PLUGINS)
        d.licenses._clock = Clock(1e9)
        d._on_driver_event(UA_EVT_RATE_CHANGED, 4)
        assert len(be.queries) == 1

    def test_reconnect_without_the_feature_withdraws(self, tmp_path):
        be = HostBackend()
        d = make_daemon(tmp_path, be)
        d._on_device_connected()
        assert status(d, 0) == AUTHORIZED
        be.features, be._f2 = 0, 0          # module reloaded, plugin_host=0
        d._on_device_connected()
        assert statuses(d) == [UNKNOWN] * len(PLUGINS)
        assert len(be.queries) == 1

    def test_catalogue_not_on_the_tree_is_left_alone(self, tmp_path):
        be = HostBackend()
        d = make_daemon(tmp_path, be)
        d.state.get_prop_dict("/plugins/3/Name")["value"] = "Other"
        d._on_device_connected()
        assert be.queries == [] and d.licenses is None


# ── The daemon on its event loop ────────────────────────────────────

async def until(cond, timeout=3.0):
    end = time.monotonic() + timeout
    while not cond():
        if time.monotonic() > end:
            raise AssertionError("timed out")
        await asyncio.sleep(0.005)


@pytest.mark.usefixtures("no_real_manifest")
class TestDaemonLoop:
    def test_query_runs_off_the_loop_thread(self, tmp_path):
        be = HostBackend()

        async def scenario():
            d = make_daemon(tmp_path, be)
            loop_thread = threading.get_ident()
            d._on_device_connected()
            assert d._license_busy and be.answers == []
            await until(lambda: not d._license_busy)
            assert statuses(d) == EXPECTED
            assert be.queries and loop_thread not in be.queries
            return d
        asyncio.run(scenario())

    def test_rate_limited_event_is_deferred_not_dropped(self, tmp_path):
        be = HostBackend()

        async def scenario():
            d = make_daemon(tmp_path, be)
            d._on_device_connected()
            await until(lambda: not d._license_busy)
            d.licenses.min_interval = 0.5
            be.default = table(r810=EXPIRED)
            d._on_driver_event(UA_EVT_RATE_CHANGED, 4)
            assert status(d, 0) == UNKNOWN          # withdrawn at once
            assert d._license_timer is not None and len(be.queries) == 1
            d._on_driver_event(UA_EVT_CONNECTED, 0)   # one more ask: merged
            await until(lambda: status(d, 0) == OLD)
            await asyncio.sleep(0.6)
            assert len(be.queries) == 2
        asyncio.run(scenario())

    def test_answer_from_before_an_event_is_dropped(self, tmp_path):
        be = HostBackend()
        release = threading.Event()
        started = threading.Event()

        def slow_old_table():
            started.set()
            release.wait(2.0)
            return full_table()
        be.answers = [slow_old_table]
        be.default = table(r810=NOT_STARTED)

        async def scenario():
            d = make_daemon(tmp_path, be)
            d._on_device_connected()
            await until(started.is_set)
            d.licenses.min_interval = 0.0
            d._on_driver_event(UA_EVT_RATE_CHANGED, 4)   # epoch changes
            release.set()
            await until(lambda: len(be.queries) == 2 and
                        not d._license_busy)
            # the first table never reached the tree
            assert status(d, 0) == NEW
        asyncio.run(scenario())

    def test_transient_failures_are_retried(self, tmp_path):
        be = HostBackend()
        be.answers = [errno.EBUSY, errno.ESTALE]

        async def scenario():
            d = make_daemon(tmp_path, be)
            d._on_device_connected()
            d.licenses.min_interval = 0.01
            await until(lambda: status(d, 0) == AUTHORIZED)
            assert len(be.queries) == 3
        asyncio.run(scenario())

    def test_retries_stop(self, tmp_path):
        be = HostBackend()
        be.answers = [errno.EBUSY] * 10

        async def scenario():
            d = make_daemon(tmp_path, be)
            d._on_device_connected()
            d.licenses.min_interval = 0.01
            await until(lambda: not d._license_busy and len(be.queries) > 3)
            await asyncio.sleep(0.1)
            assert len(be.queries) == 1 + d._LICENSE_RETRIES
            assert status(d, 0) == UNKNOWN
        asyncio.run(scenario())

    def test_hard_failures_are_not_retried(self, tmp_path):
        be = HostBackend()
        be.answers = [errno.EOPNOTSUPP]

        async def scenario():
            d = make_daemon(tmp_path, be)
            d._on_device_connected()
            d.licenses.min_interval = 0.01
            await until(lambda: not d._license_busy)
            await asyncio.sleep(0.1)
            assert len(be.queries) == 1 and status(d, 0) == UNKNOWN
        asyncio.run(scenario())

    def test_device_lost_while_asking(self, tmp_path):
        be = HostBackend()
        release = threading.Event()

        def slow_table():
            release.wait(2.0)
            return full_table()

        async def scenario():
            d = make_daemon(tmp_path, be)
            d._on_device_connected()
            await until(lambda: not d._license_busy)
            assert status(d, 0) == AUTHORIZED
            be.answers = [slow_table]
            d.licenses.min_interval = 0.0
            d._on_driver_event(UA_EVT_CONNECTED, 0)
            assert d._license_busy
            be.connected = False
            d._device_lost(time.monotonic())
            assert statuses(d) == [UNKNOWN] * len(PLUGINS)
            release.set()
            await until(lambda: not d._license_busy)
            await asyncio.sleep(0.05)
            # the answer that arrived after the loss is not shown
            assert statuses(d) == [UNKNOWN] * len(PLUGINS)
            assert len(be.queries) == 2
        asyncio.run(scenario())


# ── PluginStartDemo ─────────────────────────────────────────────────

@pytest.mark.usefixtures("no_real_manifest")
class TestStartDemo:
    def test_console_refuses_with_hardware(self, tree):
        c = ConsoleCommands(tree, None, software_only=False)
        with pytest.raises(ValueError, match="UA's own software"):
            c.post("/PluginStartDemo", {"pluginPath": "/plugins/0"})

    def test_daemon_answers_an_error_and_sends_nothing(self, tmp_path):
        be = HostBackend()
        d = make_daemon(tmp_path, be)
        d._on_device_connected()
        d.state.get_prop_dict("/plugins/1/DemoAvailable")["value"] = True
        before = statuses(d)
        n = len(be.queries)
        be.clear()
        client = Client()
        d._dispatch(client, 'post /PluginStartDemo {"pluginPath": '
                            '"/plugins/1"}')
        assert client.last()["error"] == ERR_FAILED
        d._dispatch(client, "post /plugins/1/PluginStartDemo")
        assert client.last()["error"] == ERR_UNRESOLVED
        assert statuses(d) == before
        assert d.state.get_value("/plugins/1/DemoAvailable") is True
        assert len(be.queries) == n
        assert (be.mixer_params, be.bus_params, be.driver_params,
                be.other) == ([], [], [], [])
