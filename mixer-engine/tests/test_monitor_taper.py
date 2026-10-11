"""Per-model monitor taper (N28): 54 steps on the desktop classes, 48 on the
Apollo/8/8p/16 and HEXA classes (MixerType*::GetCRPGATaperTodBTable)."""

import pytest

import hardware
import model_features as mf
from hardware import (monitor_hw_to_tapered, monitor_taper_steps,
                      monitor_tapered_to_hw, set_monitor_taper)
from model_features import load_descriptor, monitor_taper_table


@pytest.fixture(autouse=True)
def restore_taper():
    yield
    set_monitor_taper(None)


def test_x4_table_regenerates_the_captured_luts():
    fwd, rev = mf.taper_luts(monitor_taper_table(load_descriptor("apollo-x4")))
    assert fwd == hardware._MONITOR_TAPER_FWD
    assert rev == hardware._MONITOR_TAPER_REV


@pytest.mark.parametrize("slug, steps", [
    ("apollo-x4", 54), ("apollo-twin-x", 54), ("arrow", 54),
    ("apollo-x8p", 48), ("apollo-8", 48), ("apollo-x16", 48),
    ("apollo-firewire-thunderbolt-silver", 48)])
def test_steps_per_model(slug, steps):
    assert len(monitor_taper_table(load_descriptor(slug))) == steps + 1


def test_tables_differ_only_above_minus_20_db():
    a = monitor_taper_table(load_descriptor("apollo-x8p"))
    b = monitor_taper_table(load_descriptor("apollo-x4"))
    assert a[:20] == b[:20] and a[20] == -16 and b[20] == -17


@pytest.mark.parametrize("t, steps, step", [
    (0.0, 54, 0), (0.0111, 54, 1), (10 / 360, 54, 2), (1.0, 54, 54),
    (0.5, 48, 24), (-0.2, 48, 0), (2.0, 48, 48)])
def test_step_rounding(t, steps, step):
    assert mf.taper_step(t, steps) == step


def test_engine_reverse_mapping():
    table = monitor_taper_table(load_descriptor("apollo-x8p"))
    assert mf.taper_db_to_tapered(0.0, table) == 1.0
    assert mf.taper_db_to_tapered(-12.0, table) == 24 / 48
    assert mf.taper_db_to_tapered(-96.0, table) == 0.0


def test_default_is_the_x4_taper():
    assert monitor_taper_steps() == 54
    assert monitor_tapered_to_hw(0.5) == 192 + 2 * -13.5   # step 27


def test_48_step_model():
    assert set_monitor_taper(monitor_taper_table(
        load_descriptor("apollo-x8p"))) == 48
    # step round(0.5 * 48) = 24 -> -12 dB (the x4 table: step 27, -13.5)
    assert monitor_tapered_to_hw(0.5) == 192 + 2 * -12.0
    assert monitor_tapered_to_hw(1.0) == 192
    assert monitor_hw_to_tapered(192 + 2 * -12) == pytest.approx(0.5, abs=0.02)
    set_monitor_taper(None)
    assert monitor_taper_steps() == 54


def test_router_writes_the_models_taper(router, backend):
    router.set_model_data(load_descriptor("apollo-x8p"))
    router.on_set("/devices/0/outputs/18/CRMonitorLevelTapered/value", 0.5)
    assert (2, 1, 0x01, 192 + 2 * -12) in backend.mixer_params
    backend.clear()
    router.set_model_data(load_descriptor("apollo-x4"))
    router.on_set("/devices/0/outputs/18/CRMonitorLevelTapered/value", 0.5)
    assert (2, 1, 0x01, 165) in backend.mixer_params   # -13.5 dB


def test_unknown_model_keeps_the_x4_taper(router):
    router.set_model_data({})
    assert monitor_taper_steps() == 54
