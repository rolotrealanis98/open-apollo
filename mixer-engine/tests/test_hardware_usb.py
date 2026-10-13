"""N33/U39/J2: the daemon's USB backend (hardware_usb.HardwareUSB) on a fake
Apollo Solo USB: open/hot-plug, identity, settings writes through the
router, readback decode and meters, knob write-back and DSP params (both
opt-in), backend selection.  No hardware, no pyusb."""

import argparse
import json
import struct
import time

import pytest

import hardware_usb as hu
from conftest import ENGINE_DIR
from hardware import (MONITOR_CHANNEL_TYPE, PREAMP_CHANNEL_TYPE,
                      HardwareRouter)
from metering import SILENCE_DB, ReadbackMeter, readback_meter_table
from state_tree import StateTree
from usb_fake import FakeApollo, readback_words

SOLO_DESC = json.loads((ENGINE_DIR.parent / "devices" /
                        "apollo-solo-usb.json").read_text())


def make_backend(dev=None, **kw):
    dev = dev or FakeApollo()
    kw.setdefault("init_stamp", None)
    kw.setdefault("auto_flush", False)
    be = hu.HardwareUSB(opener=lambda pids: dev if dev.pid in pids else None,
                        **kw)
    assert be.open()
    dev.writes.clear()
    return be, dev


# ── open, identity, hot-plug ─────────────────────────────────────────────

def test_open_reads_identity_and_adopts_seq():
    be, dev = make_backend()
    assert be.connected and be.pid == 0x000D
    assert be.slug == "apollo-solo-usb" and be.num_preamps == 2
    assert be.device_path == "usb:2b5a:000d@3-7"
    assert be.protocol == 0x00010002
    assert be._mailbox.seq_wr == 6
    info = be.get_device_info()
    assert info["device_type"] == 0x1000D and info["subsystem_id"] == 0x000D
    assert info["fw_version"] == 0x01030003 and info["num_dsps"] == 1
    assert be.get_system_info() == {"serial": "20000000000001"}
    # read-only requests only at open
    assert all(c[0] == 0xC1 for c in dev.ctrl_log)


def test_daemon_resolves_usb_device_type_to_the_model():
    import ua_mixer_daemon as d
    assert d.find_descriptor(hu.usb_device_type(0x000D)) == \
        ("apollo-solo-usb", "Apollo Solo USB")
    assert d.find_descriptor(hu.usb_device_type(0x000F)) == \
        ("apollo-twin-x-usb", "Apollo Twin X USB")
    assert d.find_descriptor(hu.usb_device_type(0x0002)) == \
        (None, "Apollo Twin USB")
    assert d.device_model_name(hu.usb_device_type(0x000D)) == "Apollo Solo USB"
    assert not hu.is_usb_device_type(0x1F)
    # the PCIe lookup is unchanged
    assert d.find_descriptor(0x1F)[0] == "apollo-x4"


def test_open_waits_for_the_udev_init(tmp_path):
    stamp = tmp_path / "ua-usb-dsp-init.done"
    now = [100.0]
    dev = FakeApollo()
    be = hu.HardwareUSB(opener=lambda pids: dev, init_stamp=str(stamp),
                        stamp_grace=60.0, clock=lambda: now[0],
                        auto_flush=False)
    assert not be.open() and dev.ctrl_log == []
    now[0] += 30
    assert not be.reopen()
    stamp.write_text("")
    assert be.reopen()
    # without the stamp it opens anyway once the device has been present
    # for the grace time; time without a device does not count
    present = [False]
    dev2 = FakeApollo()
    be2 = hu.HardwareUSB(opener=lambda pids: dev2 if present[0] else None,
                         init_stamp=str(tmp_path / "none"), stamp_grace=60.0,
                         clock=lambda: now[0], auto_flush=False)
    assert not be2.open()
    now[0] += 120
    assert not be2.open()
    present[0] = True
    assert not be2.open() and dev2.ctrl_log == []
    now[0] += 61
    assert be2.open()


def test_probe_lost_and_reopen():
    be, dev = make_backend()
    assert be.probe()
    dev.gone = True
    assert not be.probe()
    be.mark_lost("unplugged")
    assert not be.connected and be.lost and dev.released
    assert be.get_device_info() is None and be.get_hw_readback() is None
    assert not be.set_mixer_param(MONITOR_CHANNEL_TYPE, 1, 0x01, 100)
    fresh = FakeApollo()
    be._opener = lambda pids: fresh
    assert be.reopen() and be.connected and not be.lost


def test_open_fails_cleanly_without_device_or_answer():
    be = hu.HardwareUSB(opener=lambda pids: None, init_stamp=None)
    assert not be.open() and not be.connected
    dev = FakeApollo()
    dev.fail_requests.add((0x00, 0))
    be = hu.HardwareUSB(opener=lambda pids: dev, init_stamp=None)
    assert not be.open() and dev.released


# ── settings ─────────────────────────────────────────────────────────────

def test_monitor_level_goes_out_as_a_settings_batch():
    be, dev = make_backend()
    assert be.set_mixer_param(MONITOR_CHANNEL_TYPE, 1, 0x01, 0x2A)
    assert dev.writes == []                 # batched, not written yet
    assert be.flush_settings() is True
    block = [0] * 32
    block[4] = 0x00FF002A                   # capture 82.264: [4]=00ff002a
    assert dev.writes == [(0x062D, block), (0x0602, [7])]


def test_gain_triplet_is_one_batch_like_the_capture():
    """Router order 0x0A, 0x09, 0x06 at 11 dB on input 1 -> settings 17,
    16 and 3 in one flush, as Windows wrote them at 70.689."""
    be, dev = make_backend()
    be._mailbox.value[3] = 0x40             # input 2's GainC from before
    for p, v in ((0x0A, 1), (0x09, 1), (0x06, 2)):
        assert be.set_mixer_param(PREAMP_CHANNEL_TYPE, 0, p, v)
    assert be.flush_settings() is True
    g0 = [0] * 32
    g0[6] = 0x003F0042
    g1 = [0] * 32
    g1[0] = g1[2] = 0x00FF0001
    assert dev.writes == [(0x062D, g0), (0x064F, g1), (0x0602, [7])]


def test_busy_dsp_defers_the_flush_to_the_readback_poll():
    be, dev = make_backend()
    dev.ack_delay = 3
    be.set_mixer_param(MONITOR_CHANNEL_TYPE, 1, 0x01, 0x40)
    assert be.flush_settings() is True       # SEQ_WR 7 written
    be.set_mixer_param(MONITOR_CHANNEL_TYPE, 0, 0x03, 2)   # mute
    assert be.flush_settings() is False       # DSP has not acked 7 yet
    assert be.settings_pending
    n = len(dev.writes)
    be.get_hw_readback()                      # second SEQ_RD read: still 6
    assert len(dev.writes) == n
    be.get_hw_readback()                      # acked: the flush goes out
    assert dev.writes[-1] == (0x0602, [8])
    assert dev.writes[-2][0] == 0x062D
    # mute under mask 0x30000; the cached level rides along unmasked in
    # word A (mask 0), as the Windows driver's cache does
    assert dev.writes[-2][1][4:6] == [0x00000040, 0x00030001]
    assert not be.settings_pending


def test_auto_flush_thread_sends_the_batch():
    dev = FakeApollo()
    be = hu.HardwareUSB(opener=lambda pids: dev, init_stamp=None,
                        auto_flush=True, sleep=lambda s: time.sleep(0.001))
    assert be.open()
    be.set_mixer_param(MONITOR_CHANNEL_TYPE, 1, 0x01, 0x30)
    be._flusher.join(2)
    assert dev.writes[-1] == (0x0602, [7])


def test_rejected_and_nop_params():
    be, dev = make_backend()
    assert not be.set_mixer_param(PREAMP_CHANNEL_TYPE, 2, 0x03, 1)
    assert be.last_errno == 22
    assert be.set_mixer_param(MONITOR_CHANNEL_TYPE, 0, 0x16, 1)   # cache-only
    assert not be.settings_pending
    be.paused = True
    assert not be.set_mixer_param(MONITOR_CHANNEL_TYPE, 1, 0x01, 1)


def test_router_on_the_solo_usb_map_writes_usb_settings(tmp_path):
    """A Console gain change on ANALOG 1 lands in settings 3/16/17."""
    from test_generated_device_maps import load_generator
    from model_info import ModelInfo
    gen = load_generator()
    mp = tmp_path / "solo.json"
    mp.write_text(gen.dump(gen.generate_for("apollo-solo-usb")))
    tree = StateTree()
    tree.load_device_map(mp)
    be, dev = make_backend()
    r = HardwareRouter(be, tree,
                       model=ModelInfo.from_descriptor("apollo-solo-usb",
                                                       SOLO_DESC))
    assert sorted(r.preamp_inputs()) == [0, 1]
    r.on_set("/devices/0/inputs/0/preamps/0/Gain/value", 30.0)
    be.flush_settings()
    flat = {}
    for addr, words in dev.writes:
        if addr in (0x062D, 0x064F):
            first = 0 if addr == 0x062D else 16
            for i in range(0, len(words), 2):
                if words[i]:
                    flat[first + i // 2] = words[i]
    assert flat == {3: 0x003F0015, 16: 0x00FF0014, 17: 0x00FF0014}
    dev.writes.clear()
    mon = r.monitor_output_index()
    r.on_set(f"/devices/0/outputs/{mon}/Mute/value", True)
    be.flush_settings()
    assert dev.writes[0][0] == 0x062D and dev.writes[0][1][5] == 0x00030001


# ── readback (J2) ────────────────────────────────────────────────────────

def test_readback_feeds_the_router_decode_and_meters():
    be, dev = make_backend()
    dev.readback = readback_words(w2=0x0001762A, w3=0x00001045, w0=0x08000009,
                                  w8=0x00310000, w18=0x02F002F0)
    status, words = be.get_hw_readback()
    assert status == 1 and words == dev.readback
    r = HardwareRouter(be, None)
    rb = r.poll_hw_readback()
    assert rb["monitor_level_raw"] == 0x2A and rb["monitor_mute"]
    assert not rb["monitor_mono"]
    assert rb["preamp_0_gain"] == 5 + 9 and rb["preamp_1_gain"] == 1 + 9
    assert rb["preamp_0_48v"] and rb["preamp_0_micline"]
    assert rb["clock_locked"]
    meter = ReadbackMeter(readback_meter_table(SOLO_DESC))
    meter.feed(rb["words"])
    ids = meter.channel_ids
    assert ids[:2] == [0x0001, 0x0002]
    lvl, _peak, _clip = meter.get_channel_meter(ids.index(0x0001))
    assert -70 < lvl < -60                  # 0x0031 / 0xFFFF (capture 72.459)
    assert meter.get_channel_meter(ids.index(0x0002))[0] == SILENCE_DB
    assert meter.get_channel_meter(ids.index(0x0901))[0] > -40   # monitor L


def test_bad_check_words_drop_the_snapshot():
    be, dev = make_backend()
    good = dev.readback
    dev.readback = list(dev.readback)
    dev.readback[2] ^= 0xFF
    r = HardwareRouter(be, None)
    # Reported as the PCIe "dropped" snapshot (check words all ones), so the
    # daemon's init retry runs instead of pushing the defaults to the unit
    status, words = be.get_hw_readback()
    assert status == 1 and words[38] == words[39] == 0xFFFFFFFF
    assert r.poll_hw_readback() is None
    assert r.readback_skipped is True
    dev.readback = good                      # the next snapshot is fine
    assert r.poll_hw_readback() is not None
    assert r.readback_skipped is False


def test_old_protocol_readback_path():
    be, dev = make_backend(FakeApollo(protocol=0x00010001))
    dev.regs[0x0604] = 1
    status, words = be.get_hw_readback()
    assert status == 1 and words == dev.readback
    assert dev.writes == [(0x0604, [0])]


# ── knob write-back (opt-in) ─────────────────────────────────────────────

def test_knob_writeback_mirrors_a_front_panel_change():
    now = [1000.0]
    be, dev = make_backend(knob_writeback=True, clock=lambda: now[0])
    be.get_hw_readback()                    # adopts level 0x00
    assert dev.writes == []
    dev.readback = readback_words(w2=0x00007650)
    be.get_hw_readback()                    # seen once
    assert dev.writes == []
    be.get_hw_readback()                    # twice: written back
    be.get_hw_readback()                    # the readback poll flushes it
    assert dev.writes[0][0] == 0x062D and dev.writes[0][1][4] == 0x00FF0050
    assert dev.writes[-1][0] == 0x0602


def test_knob_writeback_off_or_after_a_host_write():
    be, dev = make_backend()
    be.get_hw_readback()
    dev.readback = readback_words(w2=0x00007650)
    for _ in range(3):
        be.get_hw_readback()
    assert dev.writes == []                 # off by default
    now = [1000.0]
    be, dev = make_backend(knob_writeback=True, clock=lambda: now[0])
    be.get_hw_readback()
    be.set_mixer_param(MONITOR_CHANNEL_TYPE, 1, 0x01, 0x60)
    be.flush_settings()
    dev.writes.clear()
    dev.readback = readback_words(w2=0x00007650)   # still the old value
    be.get_hw_readback()
    be.get_hw_readback()
    assert dev.writes == []                 # within 0.5 s of the host write


# ── DSP commands on the bulk pipe (opt-in) ──────────────────────────────

def test_bus_params_need_the_opt_in():
    be, dev = make_backend()
    assert not be.set_mixer_bus_param(0, 0, 0.5)
    assert not be.set_bus_fader(0, 0.5, 0.5, 0.5)
    assert dev.bulk_out == []


def test_bus_params_and_dsp_input_params_as_dsp_commands():
    be, dev = make_backend(dsp_params=True)
    assert be.set_mixer_bus_param(0x02, 0, 1.0)
    f = dev.bulk_out[-1]
    assert f[:4] == bytes((4, 0, 0, 0xDC))
    assert struct.unpack("<4I", f[4:]) == (0x001D0004, 0x02, 0, 0x3F800000)
    assert be.set_mixer_bus_param(0x02, 0, 1.0, side=1, solo_mute=True)
    assert struct.unpack("<4I", dev.bulk_out[-1][4:]) == (0x001D0004, 2,
                                                          0x01000000, 0)
    assert be.set_mixer_param(PREAMP_CHANNEL_TYPE, 0, 0x13, 0x1FFFF)
    assert struct.unpack("<4I", dev.bulk_out[-1][4:]) == (0x001E0004, 0, 7,
                                                          0x1FFFF)
    assert [b[2] for b in dev.bulk_out] == [0, 1, 2]


# ── driver params, selection ─────────────────────────────────────────────

def test_driver_params():
    be, dev = make_backend()
    assert be.get_driver_param(0) == 48000
    assert be.get_driver_param(1) is None and be.get_driver_param(2) is None
    assert not be.set_driver_param(0, 96000)
    assert be.get_clock_sources() is None
    assert not be.has_feature(1) and be.features() == 0


def test_select_backend():
    s = hu.select_backend
    usb = [(0x000D, 3, 7, "/sys/x")]
    assert s(None, False, pcie_nodes=[], usb_apollos=usb) == "usb"
    assert s(None, False, pcie_nodes=["/dev/ua_apollo0"], usb_apollos=usb) == "pcie"
    assert s("/dev/ua_apollo0", False, pcie_nodes=[], usb_apollos=usb) == "pcie"
    assert s(None, False, pcie_nodes=[], usb_apollos=[]) == "pcie"
    assert s("/dev/ua_apollo0", True, pcie_nodes=[], usb_apollos=[]) == "usb"


def test_find_usb_apollos_reads_sysfs(tmp_path):
    for name, vid, pid in (("3-7", "2b5a", "000d"), ("3-8", "2b5a", "000c"),
                           ("1-1", "8087", "0026")):
        d = tmp_path / name
        d.mkdir()
        (d / "idVendor").write_text(vid + "\n")
        (d / "idProduct").write_text(pid + "\n")
        (d / "busnum").write_text("3\n")
        (d / "devnum").write_text("7\n")
    found = hu.find_usb_apollos(str(tmp_path))
    assert [(p, b, n) for p, b, n, _ in found] == [(0x000D, 3, 7)]


def test_pyusb_opener_claims_interface_only_for_bulk(monkeypatch):
    import sys
    import types
    from usb_fake import FakeUsb
    dev = FakeApollo()
    fake = FakeUsb(dev)
    fake.util.dispose_resources = staticmethod(lambda d: None)
    pkg = types.ModuleType("usb")
    pkg.core, pkg.util = fake.core, fake.util
    monkeypatch.setitem(sys.modules, "usb", pkg)
    monkeypatch.setitem(sys.modules, "usb.core", fake.core)
    monkeypatch.setitem(sys.modules, "usb.util", fake.util)
    be = hu.HardwareUSB(init_stamp=None, auto_flush=False, dsp_params=True)
    assert be.open()
    assert fake.claimed == []               # vendor requests need no claim
    be.set_mixer_param(MONITOR_CHANNEL_TYPE, 1, 0x01, 0x20)
    assert be.flush_settings() is True
    assert fake.claimed == []
    assert be.set_mixer_bus_param(0, 0, 1.0)
    assert fake.claimed == [0]              # the bulk pipe claims it once
    assert be.set_mixer_bus_param(0, 0, 0.5)
    assert fake.claimed == [0]
    be.close()
    assert fake.released == [0]


def test_daemon_make_backend_picks_usb(monkeypatch):
    import ua_mixer_daemon as d
    from hardware import HardwareBackend
    args = argparse.Namespace(device=None, usb=True, safe_mode=False,
                              usb_knob_writeback=True, usb_dsp_params=False)
    be = d.make_backend(args)
    assert isinstance(be, hu.HardwareUSB) and be.knob_writeback
    args.usb = False
    monkeypatch.setattr(hu, "select_backend", lambda dev, force: "pcie")
    assert isinstance(d.make_backend(args), HardwareBackend)
    monkeypatch.setattr(hu, "select_backend", lambda dev, force: "usb")
    assert isinstance(d.make_backend(args), hu.HardwareUSB)


def _fake_pyusb(monkeypatch, find):
    import sys
    import types
    from usb_fake import FakeUsb
    fake = FakeUsb(None)
    fake.core.find = staticmethod(find)
    pkg = types.ModuleType("usb")
    pkg.core, pkg.util = fake.core, fake.util
    monkeypatch.setitem(sys.modules, "usb", pkg)
    monkeypatch.setitem(sys.modules, "usb.core", fake.core)
    monkeypatch.setitem(sys.modules, "usb.util", fake.util)
    return fake


def test_missing_libusb_means_no_usb_device(monkeypatch, caplog):
    """usb.core.find raises NoBackendError without libusb: no device, one
    log line, no crash (so --require-hardware exits normally and the
    default mode keeps waiting)."""
    class NoBackendError(Exception):
        pass

    def find(**kw):
        raise NoBackendError("No backend available")
    fake = _fake_pyusb(monkeypatch, find)
    fake.core.NoBackendError = NoBackendError
    be = hu.HardwareUSB(init_stamp=None, auto_flush=False)
    with caplog.at_level("ERROR"):
        assert not be.open()
        assert not be.open()                # the next hot-plug poll
    assert not be.connected
    assert [r for r in caplog.records if "libusb" in r.getMessage()].__len__() == 1


def test_other_pyusb_errors_are_not_swallowed(monkeypatch):
    def find(**kw):
        raise RuntimeError("boom")
    _fake_pyusb(monkeypatch, find)
    be = hu.HardwareUSB(init_stamp=None, auto_flush=False)
    with pytest.raises(RuntimeError):
        be.open()
