"""--require-hardware, --preserve-hardware and --no-metering (ported from
the Codex daemon: d418807 test_hardware_required.py, c2d8f7b
test_preserve_hardware.py)."""

from contextlib import ExitStack
from unittest.mock import patch

import pytest

import ua_mixer_daemon as daemon_mod
from conftest import DEVICE_MAP_X4, make_readback
from init_recorder import RecordingBackend
from hardware import HardwareRouter
from model_features import load_descriptor
from state_tree import StateTree
from ua_mixer_daemon import MixerDaemon, RUNTIME_PROPERTIES, apply_runtime_properties


class Reached(Exception):
    pass


def start(args, stack, *, opened=False, stop_at="StateTree"):
    stack.enter_context(patch("sys.argv", ["ua_mixer_daemon.py", *args]))
    # make_backend picks HardwareBackend or the USB backend; whichever,
    # the device node is what fails or opens here (host independent)
    backend = stack.enter_context(patch.object(daemon_mod, "make_backend"))
    backend.return_value.open.return_value = opened
    backend.return_value.connected = opened
    backend.return_value.device_path = "/dev/ua_apollo0"
    stops = {}
    for name in ("StateTree", "MixerDaemon", "make_meter", "HardwareRouter",
                 "BonjourAnnouncer"):
        stops[name] = stack.enter_context(patch.object(
            daemon_mod, name, side_effect=Reached if name == stop_at else None))
    finder = stack.enter_context(patch.object(
        daemon_mod, "find_device_map", return_value=DEVICE_MAP_X4))
    return backend, stops, finder


def test_required_hardware_exits_before_map_tree_or_services(caplog):
    with ExitStack() as stack:
        backend, stops, finder = start(["--require-hardware"], stack)
        with pytest.raises(SystemExit) as e:
            daemon_mod.main()
    assert e.value.code == 1
    assert "Hardware required" in caplog.text
    backend.return_value.open.assert_called_once_with()
    finder.assert_not_called()
    for mock in stops.values():
        mock.assert_not_called()


def test_default_still_waits_for_the_device(caplog):
    with ExitStack() as stack:
        backend, stops, finder = start([], stack)
        with pytest.raises(Reached):
            daemon_mod.main()
    assert "waiting for" in caplog.text
    finder.assert_called_once_with(None, None)


def test_no_hardware_never_opens_the_device():
    with ExitStack() as stack:
        backend, stops, finder = start(["--no-hardware"], stack)
        with pytest.raises(Reached):
            daemon_mod.main()
    backend.assert_not_called()


def test_modes_are_mutually_exclusive():
    with ExitStack() as stack:
        backend, stops, finder = start(["--no-hardware",
                                        "--require-hardware"], stack)
        with pytest.raises(SystemExit) as e:
            daemon_mod.main()
    assert e.value.code == 2
    backend.assert_not_called()


@pytest.mark.parametrize("args,meter", [([], True), (["--no-metering"], False)])
def test_no_metering_opens_no_meter_source(args, meter, tmp_path):
    with ExitStack() as stack:
        stack.enter_context(patch.object(daemon_mod, "SCRIPT_DIR", tmp_path))
        backend, stops, finder = start(args, stack, opened=True,
                                       stop_at="MixerDaemon")
        stack.enter_context(patch.object(daemon_mod, "read_device_info",
                                         return_value=None))
        stops["StateTree"].side_effect = None
        stops["StateTree"].return_value = StateTree()
        with pytest.raises(Reached):
            daemon_mod.main()
        assert stops["make_meter"].called is meter
        metering = stops["MixerDaemon"].call_args.kwargs["metering"]
        if not meter:
            assert type(metering).__name__ == "NullMeter"


# ── --preserve-hardware ─────────────────────────────────────────────

def preserved(readback=True, saved=False):
    tree = StateTree()
    tree.load_device_map(DEVICE_MAP_X4)
    apply_runtime_properties(tree)
    tree.set_value("/devices/0/inputs/0/Mute/value", True)
    tree.restored = 10 if saved else 0      # values from a state file
    backend = RecordingBackend()
    if readback:
        backend.readback = (1, make_readback(
            mon_raw=150, mute=True, flags={0: {"48v"}},
            gains_c=[21, 31, 41, 51]))
    router = HardwareRouter(backend, tree)
    d = MixerDaemon(tree, router, preserve_hardware=True)
    return d, tree, backend


def mix_only(calls):
    """Preserve mode writes the inputs' mix bus coefficients and nothing else."""
    return [c for c in calls if c[0] != "bus"]


def test_preserve_publishes_the_readback_and_writes_only_the_mix():
    d, tree, backend = preserved()
    # The tree's mix reached the buses: input 1 muted, as Console shows it
    assert tree.get_value("/devices/0/inputs/0/Mute/value") is True
    assert ["bus", 0, 0, 0.0] in [c[:4] for c in backend.calls]
    assert mix_only(backend.calls) == []
    backend.calls.clear()
    get = tree.get_value
    assert get("/devices/0/outputs/18/Mute/value") is True
    assert get("/devices/0/outputs/18/CRMonitorLevel/value") == -21.0
    assert get("/devices/0/inputs/0/preamps/0/48V/value") is True
    assert get("/devices/0/inputs/0/preamps/0/Gain/value") == 30.0
    assert d._hw_init_done
    d._fire_init_complete()             # startup timer / Console handshake
    d.model_data = load_descriptor("apollo-x4")
    assert d._refresh_latency(force=True)   # figures computed and published
    assert backend.calls == []              # but no ADC delay comp written
    assert tree.get_value("/initialized/value") is True


def test_preserve_without_readback_writes_only_the_mix():
    d, tree, backend = preserved(readback=False)
    d._fire_init_complete()
    assert backend.calls and mix_only(backend.calls) == []


def test_preserve_pushes_a_saved_mix():
    """With saved state the DSP may be running something else (a reboot or
    power cycle while the daemon was down): the saved mix is pushed."""
    d, tree, backend = preserved(saved=True)
    assert not d._preserve_session and not d._hw_init_done
    d._fire_init_complete()
    assert mix_only(backend.calls), "the init sequence must be written"


def test_preserve_ends_with_the_connection():
    """A reconnect (hot-plug, resume) re-inits: the device lost its state."""
    d, tree, backend = preserved()
    d._on_device_resumed()
    assert backend.calls, "the resume push must write the init sequence"
    assert not d._preserve_session


def test_default_attach_still_runs_the_init_sequence():
    tree = StateTree()
    tree.load_device_map(DEVICE_MAP_X4)
    apply_runtime_properties(tree)
    backend = RecordingBackend()
    d = MixerDaemon(tree, HardwareRouter(backend, tree))
    d._fire_init_complete()
    assert backend.calls
    backend.calls.clear()
    d.model_data = load_descriptor("apollo-x4")
    d._refresh_latency(force=True)
    assert [c for c in backend.calls if c[0] == "mixer"], \
        "the ADC delay comp is written outside --preserve-hardware"


def test_runtime_properties_untouched():
    assert "/Dirty" in RUNTIME_PROPERTIES
