"""A /ClockSource name reaches the driver as its UAD2ClockSource value
(clock_config low byte), never as a menu index; an unknown name writes
nothing.

From the parallel Codex work (mixer-engine/test_clock_source_routing.py on
master), with driver-v2's names ("Word Clock") and map.
"""
import logging
from unittest.mock import Mock

import pytest

from hardware import CLOCK_SOURCE_MAP, DRIVER_PARAM_CLOCK_SOURCE, HardwareRouter

NATIVE = {"Internal": 0x0C, "S/PDIF": 0x00, "ADAT": 0x05,
          "Word Clock": 0x07, "Dante": 0x09}


def _router():
    router = HardwareRouter.__new__(HardwareRouter)
    router.backend = Mock()
    return router


def test_map_holds_the_native_values():
    assert CLOCK_SOURCE_MAP == NATIVE


@pytest.mark.parametrize("name,value", sorted(NATIVE.items()))
def test_native_value_reaches_the_driver(name, value):
    router = _router()
    router._handle_global("/ClockSource/value", "ClockSource", name)
    router.backend.set_driver_param.assert_called_once_with(
        DRIVER_PARAM_CLOCK_SOURCE, value)


@pytest.mark.parametrize("name", ["MADI", "AES/EBU", "WordClock", 3, ""])
def test_unknown_source_writes_nothing(name, caplog):
    router = _router()
    with caplog.at_level(logging.WARNING, logger="hardware"):
        router._handle_global("/ClockSource/value", "ClockSource", name)
    router.backend.set_driver_param.assert_not_called()
    assert "ClockSource unknown value" in caplog.text
