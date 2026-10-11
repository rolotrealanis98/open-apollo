"""HardwareRouter: control path -> hardware write translation (fake backend)."""

import pytest

import hardware
from hardware import (CLOCK_SOURCE_MAP, DRIVER_PARAM_CLOCK_SOURCE,
                      DRIVER_PARAM_SAMPLE_RATE, HardwareRouter)

IN0 = "/devices/0/inputs/0"
PRE0 = IN0 + "/preamps/0"
MON = "/devices/0/outputs/18"


def only(backend, param):
    """Mixer-param calls for one param id."""
    return [c for c in backend.mixer_params if c[2] == param]


class TestPreampGain:
    @pytest.mark.parametrize("db, a, c", [
        (10, 0, 1),        # dB-10 floors at 0
        (30, 20, 21),
        (64, 54, 55),
        (65, 54, 56),      # val_a capped at 54, val_c at 56
        (5, 0, 0),         # dB-9 floors at 0
    ])
    def test_triplet(self, router, backend, db, a, c):
        router.on_set(PRE0 + "/Gain/value", float(db))
        assert backend.mixer_params == [
            (1, 0, 0x0A, a), (1, 0, 0x09, a), (1, 0, 0x06, c)]

    def test_channel_index_is_passed(self, router, backend):
        router.on_set("/devices/0/inputs/3/preamps/0/Gain/value", 40.0)
        assert {c[1] for c in backend.mixer_params} == {3}

    def test_float_is_rounded(self, router, backend):
        router.on_set(PRE0 + "/Gain/value", 30.6)
        assert backend.mixer_params[0] == (1, 0, 0x0A, 21)

    def test_duplicate_gain_not_resent(self, router, backend):
        router.on_set(PRE0 + "/Gain/value", 30.0)
        backend.clear()
        router.on_set(PRE0 + "/Gain/value", 30.0)
        assert backend.mixer_params == []

    def test_no_preamp_beyond_input_4(self, router, backend):
        router.on_set("/devices/0/inputs/4/preamps/0/Gain/value", 30.0)
        assert backend.mixer_params == []

    def test_through_state_tree(self, router, backend, tree):
        tree.on_set = router.on_set
        tree.set(PRE0 + "/Gain/value", 40.0)
        assert (1, 0, 0x0A, 30) in backend.mixer_params
        assert (1, 0, 0x06, 31) in backend.mixer_params


class TestPreampFlags:
    @pytest.mark.parametrize("control, param", [
        ("Pad", 0x01), ("48V", 0x03), ("LowCut", 0x04), ("Phase", 0x05)])
    @pytest.mark.parametrize("value, hw", [(True, 1), (False, 0)])
    def test_flag(self, router, backend, control, param, value, hw):
        router.on_set(f"{PRE0}/{control}/value", value)
        assert backend.mixer_params[0] == (1, 0, param, hw)

    def test_flag_resends_gain_afterwards(self, router, backend):
        router.on_set(PRE0 + "/Gain/value", 30.0)
        backend.clear()
        router.on_set(PRE0 + "/48V/value", True)
        assert backend.mixer_params == [
            (1, 0, 0x03, 1), (1, 0, 0x0A, 20), (1, 0, 0x09, 20), (1, 0, 0x06, 21)]

    @pytest.mark.parametrize("value, hw", [("Line", 1), ("Mic", 0), ("line", 1)])
    def test_iotype(self, router, backend, value, hw):
        router.on_set(IN0 + "/IOType/value", value)
        assert backend.mixer_params == [(1, 0, 0x00, hw)]

    def test_iotype_ignored_without_preamp(self, router, backend):
        router.on_set("/devices/0/inputs/6/IOType/value", "Line")
        assert backend.mixer_params == []


class TestMonitor:
    @pytest.mark.parametrize("db, raw", [(0, 192), (-20, 152), (-55, 82),
                                         (-96, 0), (-0.5, 191)])
    def test_level(self, router, backend, db, raw):
        router.on_set(MON + "/CRMonitorLevel/value", float(db))
        assert backend.mixer_params == [(2, 1, 0x01, raw)]

    @pytest.mark.parametrize("value, hw", [(True, 2), (False, 0)])
    def test_mute(self, router, backend, value, hw):
        router.on_set(MON + "/Mute/value", value)
        assert backend.mixer_params == [(2, 0, 0x03, hw)]

    @pytest.mark.parametrize("value, hw", [(True, 1), (False, 0)])
    def test_dim(self, router, backend, value, hw):
        router.on_set(MON + "/DimOn/value", value)
        assert backend.mixer_params == [(2, 7, 0x44, hw), (2, 0, 0x44, hw)]

    def test_mono_shares_mute_param(self, router, backend):
        router.on_set(MON + "/MixToMono/value", True)
        assert backend.mixer_params == [(2, 0, 0x03, 1)]

    def test_through_state_tree(self, router, backend, tree):
        tree.on_set = router.on_set
        tree.set(MON + "/CRMonitorLevel/value", -30.0)
        assert backend.mixer_params == [(2, 1, 0x01, 132)]

    def test_headphone_level_does_not_touch_monitor_level(self, router, backend):
        router.on_set("/devices/0/outputs/19/CRMonitorLevel/value", -10.0)
        assert backend.mixer_params == [(2, 1, 0x02, 172)]
        assert only(backend, 0x01) == []


class TestTalkback:
    @pytest.mark.parametrize("value, hw", [(True, 1), (False, 0)])
    def test_tree_root_path(self, router, backend, value, hw):
        router.on_set("/TalkbackOn/value", value)
        assert backend.mixer_params == [(2, 7, 0x46, hw), (2, 0, 0x46, hw)]

    def test_devices_path_also_accepted(self, router, backend):
        router.on_set("/devices/0/TalkbackOn/value", True)
        assert backend.mixer_params == [(2, 7, 0x46, 1), (2, 0, 0x46, 1)]

    def test_through_state_tree(self, router, backend, tree):
        tree.on_set = router.on_set
        tree.set("/TalkbackOn/value", True)
        assert backend.mixer_params == [(2, 7, 0x46, 1), (2, 0, 0x46, 1)]


class TestClockSource:
    @pytest.mark.parametrize("name, hw", [
        ("Internal", 0x0C), ("S/PDIF", 0x00), ("ADAT", 0x05), ("Word Clock", 0x07),
        ("Dante", 0x09)])
    def test_names(self, router, backend, name, hw):
        router.on_set("/ClockSource/value", name)
        assert backend.driver_params == [(DRIVER_PARAM_CLOCK_SOURCE, hw)]
        assert DRIVER_PARAM_CLOCK_SOURCE == 1

    def test_map_matches_expected_values(self):
        assert CLOCK_SOURCE_MAP == {
            "Internal": 0x0C, "S/PDIF": 0x00, "ADAT": 0x05, "Word Clock": 0x07,
            "Dante": 0x09}

    @pytest.mark.parametrize("name", ["AES/EBU", "MADI"])
    def test_sources_without_a_known_driver_value_are_not_written(
            self, router, backend, name):
        router.on_set("/ClockSource/value", name)
        assert backend.driver_params == []

    def test_unknown_name_ignored(self, router, backend):
        router.on_set("/ClockSource/value", "Bogus")
        assert backend.driver_params == []


class TestSampleRate:
    def test_ioctl_when_pipewire_does_not_hold_device(self, router, backend,
                                                      monkeypatch):
        calls = []
        monkeypatch.setattr(hardware, "pipewire_force_rate",
                            lambda r, device=None: calls.append(r) or False)
        router.on_set("/SampleRate/value", 96000)
        assert calls == [96000]
        assert backend.driver_params == [(DRIVER_PARAM_SAMPLE_RATE, 96000)]
        assert DRIVER_PARAM_SAMPLE_RATE == 0

    def test_no_ioctl_when_pipewire_takes_it(self, router, backend, monkeypatch):
        monkeypatch.setattr(hardware, "pipewire_force_rate", lambda r, device=None: True)
        router.on_set("/SampleRate/value", 96000)
        assert backend.driver_params == []

    @pytest.mark.parametrize("bad", [12345, "abc", None])
    def test_invalid_rate_never_reaches_hardware(self, router, backend,
                                                 monkeypatch, bad):
        monkeypatch.setattr(hardware, "pipewire_force_rate", lambda r, device=None: False)
        router.on_set("/SampleRate/value", bad)
        assert backend.driver_params == []

    @pytest.mark.parametrize("rate", sorted(hardware.VALID_SAMPLE_RATES))
    def test_all_valid_rates(self, router, backend, monkeypatch, rate):
        monkeypatch.setattr(hardware, "pipewire_force_rate", lambda r, device=None: False)
        router.on_set("/SampleRate/value", rate)
        assert backend.driver_params == [(0, rate)]


class TestDriverSync:
    def test_startup_syncs_rate_and_clock_into_tree(self, tree):
        from conftest import FakeBackend
        be = FakeBackend()
        be.driver_values = {DRIVER_PARAM_SAMPLE_RATE: 96000,
                            DRIVER_PARAM_CLOCK_SOURCE: 0x00}
        HardwareRouter(be, tree)
        assert tree.get_value("/SampleRate/value") == 96000
        assert tree.get_value("/ClockSource/value") == "S/PDIF"

    def test_no_sync_when_disconnected(self, tree):
        from conftest import FakeBackend
        be = FakeBackend(connected=False)
        be.driver_values = {DRIVER_PARAM_SAMPLE_RATE: 96000}
        HardwareRouter(be, tree)
        assert tree.get_value("/SampleRate/value") == 48000


def test_unmatched_path_is_software_only(router, backend):
    router.on_set("/BufferSize/value", 512)
    assert backend.mixer_params == [] and backend.driver_params == []
    assert backend.bus_params == []
