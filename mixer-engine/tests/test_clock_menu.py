"""The device's clock-source menu (UA_IOCTL_GET_CLOCK_SOURCES) names the
sources as UA's Mixer Engine shows them: the device's own names, only for
the values the Mixer Engine knows (CMxRoot::UpdateClockNames)."""
import struct

import hardware
from hardware import (CLOCK_SOURCE_MAP, KNOWN_CLOCK_VALUES, HardwareRouter,
                      UA_CLOCK_SRC_F_DEVICE_NAME, device_clock_menu)


def test_known_values_are_the_engines_five():
    assert KNOWN_CLOCK_VALUES == {0x0C, 0x07, 0x05, 0x00, 0x09}


def test_menu_keeps_device_names_for_known_values():
    # An AES/EBU unit names its digital source itself
    menu = device_clock_menu([(0x0C, "Internal", True),
                              (0x00, "AES/EBU", True),
                              (0x07, "Word Clock", True),
                              (0x0A, "Peer", True),      # unknown: dropped
                              (0x03, "Mystery", True)])  # unknown: dropped
    assert menu == {"Internal": 0x0C, "AES/EBU": 0x00, "Word Clock": 0x07}


def test_menu_needs_more_than_internal():
    assert device_clock_menu([(0x0C, "Internal", True)]) == {}
    assert device_clock_menu(None) == {}


def test_router_sends_the_device_menu_value():
    from unittest.mock import Mock
    from hardware import DRIVER_PARAM_CLOCK_SOURCE
    router = HardwareRouter.__new__(HardwareRouter)
    router.backend = Mock()
    router.clock_sources = {"Internal": 0x0C, "AES/EBU": 0x00}
    router._handle_global("/ClockSource/value", "ClockSource", "AES/EBU")
    router.backend.set_driver_param.assert_called_once_with(
        DRIVER_PARAM_CLOCK_SOURCE, 0x00)


def test_get_clock_sources_parses_the_struct(monkeypatch):
    raw = bytearray(hardware._CLOCK_SOURCES_SIZE)
    struct.pack_into('<4I', raw, 0, hardware._CLOCK_SOURCES_SIZE, 2, 0x1021, 0)
    struct.pack_into('<II32s', raw, 16, 0x0C, UA_CLOCK_SRC_F_DEVICE_NAME,
                     b"Internal")
    struct.pack_into('<II32s', raw, 56, 0x00, UA_CLOCK_SRC_F_DEVICE_NAME,
                     b"S/PDIF")
    be = hardware.HardwareBackend.__new__(hardware.HardwareBackend)
    be.fd = 3
    be._features = hardware.UA_FEAT_CLOCK_SOURCES
    monkeypatch.setattr(hardware.fcntl, "ioctl", lambda fd, req, buf: bytes(raw))
    assert be.get_clock_sources() == [(0x0C, "Internal", True),
                                      (0x00, "S/PDIF", True)]
