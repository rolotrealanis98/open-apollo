"""Feature gating from GET_VERSION, GET_SYSTEM_INFO and the real serial."""

import errno
import struct

import pytest

import hardware
import ua_mixer_daemon as umd
from conftest import FakeBackend
from hardware import (HardwareBackend, UA_BUS_PARAM_F_SIDE_R,
                      UA_BUS_PARAM_F_SOLO_MUTE, UA_BUS_PARAM_FLAGS,
                      UA_FEAT_BUS_PARAM_SIDE, UA_FEAT_BUS_SOLO_FLAG,
                      UA_FEAT_SYSTEM_INFO)
from helper_tree import HelperTree


class FakeDriver:
    """Patches hardware.fcntl.ioctl: answers GET_VERSION / GET_SYSTEM_INFO
    like a driver with `features` (None = a pre-GET_VERSION driver, ENOTTY)
    and records every bus-param ioctl."""

    def __init__(self, monkeypatch, features, serial=b"UA0123456789ABCD"):
        self.features = features
        self.serial = serial
        self.bus = []            # (bus, sub, value_u32, flags)
        self.calls = []
        monkeypatch.setattr(hardware.fcntl, "ioctl", self.ioctl)

    def ioctl(self, fd, req, buf):
        self.calls.append(req)
        if req == hardware.UA_IOCTL_GET_VERSION:
            if self.features is None:
                raise OSError(errno.ENOTTY, "no")
            return struct.pack('<IIII16s16x', 1, 1, 0x10000, self.features,
                               b"ua_apollo")
        if req == hardware.UA_IOCTL_GET_SYSTEM_INFO:
            head = struct.pack('<12I', 1, 0x1F, 0x20, 0x1234, 0x5678, 4, 99,
                               1, 0, 0xAA, 0xBB, 7)
            tail = struct.pack('<13I', 48000, 3, 12, 1, 1, 1, 0, 24, 22, 0,
                               0, 1, 0)
            return head + self.serial.ljust(16, b"\0") + tail + bytes(32)
        if req == hardware.UA_IOCTL_GET_DEVICE_INFO:
            return bytes(64)
        if req == hardware.UA_IOCTL_SET_MIXER_BUS_PARAM:
            self.bus.append(struct.unpack('<IIII', buf))
            return buf
        raise OSError(errno.ENOTTY, "unexpected")


def backend_on(monkeypatch, features, **kw):
    drv = FakeDriver(monkeypatch, features, **kw)
    be = HardwareBackend()
    be.fd = 3
    be.connected = True
    return be, drv


def as_float(u32):
    return struct.unpack('<f', struct.pack('<I', u32))[0]


class TestSideGate:
    def test_old_driver_drops_side_1(self, monkeypatch):
        be, drv = backend_on(monkeypatch, None)
        assert be.set_mixer_bus_param(0, 0, 0.5)
        assert be.set_mixer_bus_param(0, 0, 0.7, side=1)   # dropped, no error
        assert [e[3] for e in drv.bus] == [UA_BUS_PARAM_FLAGS]

    def test_driver_without_the_bit_drops_side_1(self, monkeypatch):
        be, drv = backend_on(monkeypatch, UA_FEAT_SYSTEM_INFO)
        be.set_mixer_bus_param(0, 0, 0.7, side=1)
        assert drv.bus == []

    def test_new_driver_sends_side_1(self, monkeypatch):
        be, drv = backend_on(monkeypatch, UA_FEAT_BUS_PARAM_SIDE)
        be.set_mixer_bus_param(0, 0, 0.7, side=1)
        assert [e[3] for e in drv.bus] == \
            [UA_BUS_PARAM_FLAGS | UA_BUS_PARAM_F_SIDE_R]

    def test_version_is_asked_once_per_session(self, monkeypatch):
        be, drv = backend_on(monkeypatch, UA_FEAT_BUS_PARAM_SIDE)
        for _ in range(3):
            be.set_mixer_bus_param(0, 0, 0.7, side=1)
        assert drv.calls.count(hardware.UA_IOCTL_GET_VERSION) == 1

    def test_reopen_asks_again(self, monkeypatch, tmp_path):
        node = tmp_path / "ua_apollo0"
        node.write_bytes(b"")
        drv = FakeDriver(monkeypatch, None)
        be = HardwareBackend(str(node))
        assert be.open()
        assert be.features() == 0
        drv.features = UA_FEAT_BUS_PARAM_SIDE      # driver reloaded
        be.mark_lost()
        assert be.reopen()
        assert be.features() == UA_FEAT_BUS_PARAM_SIDE

    def test_no_device_has_no_features_and_is_not_cached(self):
        be = HardwareBackend()
        assert be.features() == 0
        assert be._features is None

    def test_direct_write_side_1_is_gated_too(self, monkeypatch):
        be, drv = backend_on(monkeypatch, None)
        be.set_bus_param_direct(0, 0, 0.7, side=1)
        assert drv.bus == []


class TestSoloFlag:
    def test_flag_sent_with_the_normal_value(self, monkeypatch):
        be, drv = backend_on(monkeypatch, UA_FEAT_BUS_SOLO_FLAG)
        be.set_mixer_bus_param(0, 0, 0.5, solo_mute=True)
        [(_, _, val, flags)] = drv.bus
        assert flags == UA_BUS_PARAM_FLAGS | UA_BUS_PARAM_F_SOLO_MUTE
        assert as_float(val) == 0.5

    def test_old_driver_gets_zero_and_no_flag(self, monkeypatch):
        be, drv = backend_on(monkeypatch, UA_FEAT_BUS_PARAM_SIDE)
        be.set_mixer_bus_param(0, 0, 0.5, solo_mute=True)
        [(_, _, val, flags)] = drv.bus
        assert val == 0 and flags == UA_BUS_PARAM_FLAGS

    def test_flag_value_and_bit(self):
        assert UA_BUS_PARAM_F_SOLO_MUTE == 0x200
        assert UA_FEAT_BUS_SOLO_FLAG == 1 << 11

    def test_both_sides_of_a_fader(self, monkeypatch):
        be, drv = backend_on(monkeypatch,
                             UA_FEAT_BUS_SOLO_FLAG | UA_FEAT_BUS_PARAM_SIDE)
        be.set_bus_fader(0, 1.0, 0.5, 0.25, solo_mute=True)
        assert [e[3] & UA_BUS_PARAM_F_SOLO_MUTE for e in drv.bus] == \
            [UA_BUS_PARAM_F_SOLO_MUTE] * 2


class TestSystemInfo:
    def test_ioctl_number(self):
        assert hardware.UA_IOCTL_GET_SYSTEM_INFO == \
            (2 << 30) | (148 << 16) | (0x55 << 8) | 0x05

    def test_decodes_every_field(self, monkeypatch):
        be, _ = backend_on(monkeypatch, UA_FEAT_SYSTEM_INFO)
        info = be.get_system_info()
        assert info["serial"] == "UA0123456789ABCD"     # all 16, no NUL
        assert (info["device_type"], info["public_type"], info["fw_version"],
                info["hw_uid_lo"], info["hw_uid_hi"], info["mixer_kind"]) \
            == (0x1F, 0x20, 99, 0xAA, 0xBB, 7)
        assert (info["sample_rate"], info["play_channels"],
                info["rec_channels"], info["unit_count"]) == \
            (48000, 24, 22, 1)

    def test_short_serial_is_nul_padded(self, monkeypatch):
        be, _ = backend_on(monkeypatch, UA_FEAT_SYSTEM_INFO, serial=b"X4-77")
        assert be.get_system_info()["serial"] == "X4-77"

    def test_none_without_the_feature(self, monkeypatch):
        be, drv = backend_on(monkeypatch, UA_FEAT_BUS_PARAM_SIDE)
        assert be.get_system_info() is None
        assert hardware.UA_IOCTL_GET_SYSTEM_INFO not in drv.calls

    def test_none_on_an_old_driver(self, monkeypatch):
        be, _ = backend_on(monkeypatch, None)
        assert be.get_system_info() is None


class SerialBackend(FakeBackend):
    def __init__(self, sysinfo, serial=0):
        super().__init__()
        self._sysinfo = sysinfo
        self._serial = serial

    def get_device_info(self):
        return {"device_type": 0x1F, "fw_version": 7, "serial": self._serial}

    def get_system_info(self):
        return self._sysinfo


class TestSerialReporting:
    def test_real_serial_wins(self):
        info = umd.read_device_info(SerialBackend({"serial": "UA9876"}))
        assert umd.serial_text(info) == "UA9876"

    def test_old_driver_keeps_the_8_hex_fallback(self):
        info = umd.read_device_info(SerialBackend(None, serial=0xABCD))
        assert "serial_str" not in info
        assert umd.serial_text(info) == "0000ABCD"

    @pytest.mark.parametrize("blank", ["", "0000000000000000", "  "])
    def test_blank_serial_falls_back(self, blank):
        info = umd.read_device_info(SerialBackend({"serial": blank}))
        assert umd.serial_text(info) == "00000000"

    def test_no_device_info_passes_through(self):
        class B(FakeBackend):
            def get_device_info(self):
                return None
        assert umd.read_device_info(B()) is None

    def test_helper_tree_gets_the_real_serial(self):
        t = HelperTree()
        t.root = {"properties": {
            "serial_number": {"value": ""}, "firmware_version": {"value": ""},
            "expected_firmware_version": {"value": ""}}, "children": []}
        info = umd.read_device_info(SerialBackend({"serial": "UA9876"}))
        assert umd.fill_helper_identity(t, info) == 3
        assert t.root["properties"]["serial_number"]["value"] == "UA9876"


class TestStateFileName:
    MAP = umd.Path("/x/device_map_apollo_x4.json")

    def test_index_zero_never_changes(self):
        for serial in (None, 0x1234, "UA9876"):
            assert umd.state_file_for(self.MAP, 0, serial).name == \
                "device_map_apollo_x4.state.json"

    def test_numeric_serial_keeps_its_name(self):
        assert umd.state_file_for(self.MAP, 1, 0xAB).name == \
            "device_map_apollo_x4.dev1-000000ab.state.json"

    def test_string_serial_suffix_is_sanitized(self):
        assert umd.state_file_for(self.MAP, 2, "UA 98/76").name == \
            "device_map_apollo_x4.dev2-UA9876.state.json"

    def test_unsafe_only_serial_adds_nothing(self):
        assert umd.state_file_for(self.MAP, 1, "///").name == \
            "device_map_apollo_x4.dev1.state.json"

    def test_numeric_serial_wins_over_the_string(self):
        assert umd.state_serial({"serial": 0xAB, "serial_str": "UA1"}) == 0xAB

    def test_string_used_when_numeric_is_zero(self):
        assert umd.state_serial({"serial": 0, "serial_str": "UA1"}) == "UA1"
        assert umd.state_serial({"serial": 0}) is None
        assert umd.state_serial(None) is None

    def test_seed_copies_the_legacy_file_once(self, tmp_path):
        legacy = tmp_path / "a.dev1.state.json"
        new = tmp_path / "a.dev1-UA1.state.json"
        legacy.write_text('{"k": 1}')
        assert umd.seed_state_file(new, legacy)
        assert new.read_text() == '{"k": 1}' and legacy.exists()
        new.write_text('{"k": 2}')
        assert not umd.seed_state_file(new, legacy)     # never overwrites
        assert new.read_text() == '{"k": 2}'

    def test_seed_without_legacy_or_same_path(self, tmp_path):
        p = tmp_path / "a.state.json"
        assert not umd.seed_state_file(tmp_path / "n.json", p)
        p.write_text("{}")
        assert not umd.seed_state_file(p, p)
