"""GET_SYSTEM_INFO extension (ABI 1.2) and what the daemon derives from it:
the /ClockSource list, the PCIe link report, the firmware major warning, and
/AudioStreaming following the transport."""

import logging
import struct

import pytest

import events
import hardware
from hardware import (CLOCK_SOURCE_MAP, DRIVER_PARAM_TRANSPORT_RUNNING,
                      HardwareBackend, UA_FEAT_SYSINFO_EXT,
                      UA_FEAT_SYSTEM_INFO, clock_source_names,
                      pcie_link_below_minimum, pcie_link_text)
from test_driver_features import FakeDriver
from ua_mixer_daemon import MixerDaemon, apply_runtime_properties

MASK_X4 = (1 << 12) | (1 << 0)               # Internal + S/PDIF
MASK_ALL = (1 << 12) | (1 << 0) | (1 << 5) | (1 << 7)


class ExtDriver(FakeDriver):
    def __init__(self, monkeypatch, features, ext=(2, 1, MASK_X4, 0x7E)):
        super().__init__(monkeypatch, features)
        self.ext = ext

    def ioctl(self, fd, req, buf):
        out = super().ioctl(fd, req, buf)
        if req == hardware.UA_IOCTL_GET_SYSTEM_INFO:
            out = out[:116] + struct.pack('<4I', *self.ext) + bytes(16)
            assert len(out) == 148
        return out


class TestSystemInfoExt:
    def test_decoded_with_the_feature_bit(self, monkeypatch):
        ExtDriver(monkeypatch, UA_FEAT_SYSTEM_INFO | UA_FEAT_SYSINFO_EXT)
        be = HardwareBackend()
        be.fd, be.connected = 3, True
        info = be.get_system_info()
        assert info["pcie_link_speed"] == 2
        assert info["pcie_link_width"] == 1
        assert info["clock_src_mask"] == MASK_X4
        assert info["fw_rate_mask"] == 0x7E
        assert info["unit_role"] == 0 and info["serial"]

    def test_ignored_without_the_feature_bit(self, monkeypatch):
        ExtDriver(monkeypatch, UA_FEAT_SYSTEM_INFO)
        be = HardwareBackend()
        be.fd, be.connected = 3, True
        info = be.get_system_info()
        assert "clock_src_mask" not in info and "pcie_link_speed" not in info

    def test_feature_bit_value(self):
        assert UA_FEAT_SYSINFO_EXT == 1 << 12


class TestClockSourceNames:
    MODEL = ["Internal", "S/PDIF", "ADAT", "Word Clock"]

    def test_mask_picks_the_sources_by_value(self):
        assert clock_source_names(MASK_X4, self.MODEL) == ["Internal", "S/PDIF"]
        assert clock_source_names(MASK_ALL, self.MODEL) == self.MODEL

    def test_model_limits_the_list(self):
        assert clock_source_names(MASK_ALL, ["Internal", "S/PDIF"]) == \
            ["Internal", "S/PDIF"]

    def test_internal_is_kept_without_its_bit(self):
        assert clock_source_names(1 << 0 | 1 << 5, self.MODEL) == \
            ["Internal", "S/PDIF", "ADAT"]

    def test_no_mask_is_the_fallback(self):
        assert clock_source_names(None, self.MODEL) is None
        assert clock_source_names(0, self.MODEL) is None
        assert clock_source_names(1 << 12, self.MODEL) is None

    def test_names_unknown_to_the_map_are_dropped(self):
        assert clock_source_names(0xFFFF, ["Internal", "AES/EBU", "S/PDIF"]) \
            == ["Internal", "S/PDIF"]

    def test_dante_is_bit_9(self):
        assert CLOCK_SOURCE_MAP["Dante"] == 9
        assert clock_source_names((1 << 9), ["Internal", "Dante"]) == \
            ["Internal", "Dante"]


class TestPcieLink:
    @pytest.mark.parametrize("speed,width,below", [
        (1, 1, False), (1, 2, False), (2, 1, False), (3, 1, False),
        (2, 2, False), (0, 0, False), (1, 0, False), (0, 4, False)])
    def test_minimum(self, speed, width, below):
        assert pcie_link_below_minimum(speed, width) is below

    def test_text(self):
        assert pcie_link_text(2, 1) == "x1 Gen2"
        assert pcie_link_text(0, 0) == ""


@pytest.fixture
def daemon(router, tree):
    apply_runtime_properties(tree)
    return MixerDaemon(tree, router)


def info(**kw):
    base = {"fw_version": 9, "pcie_link_speed": 2, "pcie_link_width": 1,
            "clock_src_mask": MASK_X4, "fw_rate_mask": 0x7E}
    base.update(kw)
    return base


def listed(tree):
    return tree.get_prop_dict("/ClockSource")["values"]


def pushes(daemon):
    seen = []
    daemon.state.register_callback("t", lambda p, v: seen.append((p, v)))
    for p in ("/ClockSourceListChanged/value", "/AudioStreaming/value"):
        daemon.state.subscribe("t", p)
    return seen


def three_sources(daemon, tree):
    daemon._model_clock_sources = ["Internal", "S/PDIF", "ADAT"]
    tree.update_property("/ClockSource",
                         {"values": ["Internal", "S/PDIF", "ADAT"]})


class TestClockSourceList:
    def test_static_list_without_sysinfo(self, daemon, tree):
        assert daemon._publish_clock_sources(None) is False
        assert listed(tree) == ["Internal", "S/PDIF"]

    def test_device_mask_narrows_the_model_list(self, daemon, tree):
        three_sources(daemon, tree)
        assert daemon._publish_clock_sources(info()) is True
        assert listed(tree) == ["Internal", "S/PDIF"]

    def test_change_pulses_list_changed(self, daemon, tree):
        three_sources(daemon, tree)
        seen = pushes(daemon)
        daemon._publish_clock_sources(info())
        assert [v for p, v in seen
                if p == "/ClockSourceListChanged/value"] == [True, False]
        assert "/ClockSourceListChanged/value" not in tree._dirty

    def test_no_change_no_push(self, daemon, tree):
        seen = pushes(daemon)
        assert daemon._publish_clock_sources(info()) is False
        assert seen == []

    def test_mask_loss_restores_the_static_list(self, daemon, tree):
        three_sources(daemon, tree)
        daemon._publish_clock_sources(info())
        daemon._publish_clock_sources(info(clock_src_mask=0))
        assert listed(tree) == ["Internal", "S/PDIF", "ADAT"]

    def test_clock_changed_event_republishes(self, daemon, tree, backend):
        three_sources(daemon, tree)
        backend.system_info = info(clock_src_mask=MASK_ALL)
        daemon._on_driver_event(events.UA_EVT_CLOCK_CHANGED, 0x05)
        assert listed(tree) == ["Internal", "S/PDIF", "ADAT"]
        backend.system_info = info(clock_src_mask=MASK_X4)
        daemon._on_driver_event(events.UA_EVT_CLOCK_CHANGED, 0x0C)
        assert listed(tree) == ["Internal", "S/PDIF"]

    def test_connect_publishes(self, daemon, tree, backend):
        three_sources(daemon, tree)
        backend.system_info = info()
        daemon._on_device_connected()
        assert listed(tree) == ["Internal", "S/PDIF"]


class TestPcieLinkReport:
    def test_published_and_not_saved(self, daemon, tree):
        daemon._publish_pcie_link(info(pcie_link_speed=2, pcie_link_width=4))
        assert tree.get_value("/openapollo/pcie_link/value") == "x4 Gen2"
        assert tree.get_value("/openapollo/pcie_link_speed/value") == 2
        assert tree.get_value("/openapollo/pcie_link_width/value") == 4
        assert not [p for p in tree._dirty if p.startswith("/openapollo/")]

    def test_not_reported_without_the_fields(self, daemon, tree):
        daemon._publish_pcie_link({"fw_version": 9})
        assert tree.get_value("/openapollo/pcie_link/value") in ("", None)

    def test_no_warning_for_the_apollo_x1_gen1_link(self, daemon, caplog):
        # titan's x4 reads speed 1 width 1: the endpoint's own link
        with caplog.at_level(logging.WARNING, logger="ua-mixer-daemon"):
            daemon._publish_pcie_link(info(pcie_link_speed=1, pcie_link_width=1))
        assert not [r for r in caplog.records if "PCIe link" in r.getMessage()]

    def test_no_warning_at_gen2_x1(self, daemon, caplog):
        with caplog.at_level(logging.WARNING, logger="ua-mixer-daemon"):
            daemon._publish_pcie_link(info())
        assert not caplog.records


class TestFirmwareMajor:
    @pytest.mark.parametrize("fw", [9, 11])
    def test_accepted(self, daemon, caplog, fw):
        with caplog.at_level(logging.WARNING, logger="ua-mixer-daemon"):
            daemon._check_firmware(info(fw_version=fw))
        assert not caplog.records

    def test_status_word_is_not_judged(self, daemon, caplog):
        # titan's x4: BAR0 0x0000 reads 0x1c00 (DSP 0 status word)
        with caplog.at_level(logging.WARNING, logger="ua-mixer-daemon"):
            daemon._check_firmware(info(fw_version=0x1c00))
        assert not caplog.records

    def test_other_major_warns_once(self, daemon, caplog):
        with caplog.at_level(logging.WARNING, logger="ua-mixer-daemon"):
            daemon._check_firmware(info(fw_version=7))
            daemon._check_firmware(info(fw_version=7))
        assert len([r for r in caplog.records
                    if "Firmware version 7" in r.getMessage()]) == 1

    def test_no_info_is_silent(self, daemon, caplog):
        daemon._check_firmware(None)
        assert not caplog.records


class TestAudioStreaming:
    def test_default_true_without_hardware(self, tree):
        apply_runtime_properties(tree)
        d = MixerDaemon(tree, None)
        d._poll_transport()
        assert tree.get_value("/AudioStreaming/value") is True

    def test_follows_the_transport(self, daemon, tree, backend):
        daemon._hw_online = True
        seen = pushes(daemon)
        backend.driver_values[DRIVER_PARAM_TRANSPORT_RUNNING] = 0
        daemon._poll_transport()
        assert tree.get_value("/AudioStreaming/value") is False
        backend.driver_values[DRIVER_PARAM_TRANSPORT_RUNNING] = 1
        daemon._poll_transport()
        assert tree.get_value("/AudioStreaming/value") is True
        assert [v for p, v in seen if p == "/AudioStreaming/value"] == \
            [False, True]
        assert "/AudioStreaming/value" not in tree._dirty

    def test_unreadable_param_leaves_it(self, daemon, tree, backend):
        daemon._hw_online = True
        daemon._poll_transport()      # driver_values has no entry: None
        assert tree.get_value("/AudioStreaming/value") is True

    def test_offline_device_leaves_it(self, daemon, tree, backend):
        daemon._hw_online = False
        backend.driver_values[DRIVER_PARAM_TRANSPORT_RUNNING] = 0
        daemon._poll_transport()
        assert tree.get_value("/AudioStreaming/value") is True
