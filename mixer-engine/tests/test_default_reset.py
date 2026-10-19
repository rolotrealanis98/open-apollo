"""Console's reset gesture, `set <prop>/value "default"`: every control the
x4 engine publishes a default for resets to it (values of the engine's own
dump), and the property carries that `default`."""

import pytest

import hardware
from conftest import FakeBackend
from hardware import HardwareRouter
from protocol import Command
from ua_mixer_daemon import MixerClient, MixerDaemon

D = "/devices/0"
UNITY, TALKBACK_LEVEL = 43.0 / 55.0, 37.0 / 55.0


class Writer:
    def get_extra_info(self, name):
        return ("127.0.0.1", 4710)

    def write(self, data):
        pass


# (path, value moved to first, the engine's default)
RESETS = [
    (f"{D}/inputs/0/sends/0/GainTapered", 0.2, UNITY),
    (f"{D}/inputs/0/sends/0/Gain", -20.0, -144.0),
    (f"{D}/auxs/0/sends/0/GainTapered", 0.2, UNITY),
    (f"{D}/inputs/0/FaderLevelTapered", 0.2, UNITY),
    (f"{D}/auxs/0/FaderLevelTapered", 0.2, UNITY),
    (f"{D}/inputs/0/Pan", 0.5, 0.0),
    (f"{D}/inputs/0/Pan2", 0.5, 0.0),
    (f"{D}/inputs/0/sends/0/Pan", 0.5, 0.0),
    (f"{D}/inputs/0/preamps/0/GainTapered", 0.5, 0.0),
    (f"{D}/inputs/0/preamps/0/Gain", 40.0, 10.0),
    (f"{D}/outputs/18/CRMonitorLevelTapered", 0.5, 0.0),
    (f"{D}/outputs/18/CRMonitorLevel", -20.0, -96.0),
    (f"{D}/DimAttenuation", 40, 17),
    (f"{D}/outputs/0/AltMonTrim", 3.0, 0.0),
    (f"{D}/inputs/22/FaderLevelTapered", 0.2, TALKBACK_LEVEL),
    (f"{D}/inputs/22/FaderLevel", 0.0, -6.0),
    (f"{D}/inputs/22/sends/2/GainTapered", 0.2, TALKBACK_LEVEL),
]


@pytest.mark.parametrize("path,moved,default", RESETS)
def test_tree_resolves_default(tree, path, moved, default):
    tree.set(path + "/value", moved, hardware=False)
    assert tree.set(path + "/value", "default", hardware=False)
    assert tree.get_value(path + "/value") == pytest.approx(default)
    assert tree.get_prop_dict(path)["default"] == pytest.approx(default)


def test_tapered_defaults_match_their_db_companions(tree):
    assert hardware.fader_tapered_to_db(UNITY) == pytest.approx(0.0, abs=0.05)
    assert hardware.fader_tapered_to_db(TALKBACK_LEVEL) == pytest.approx(
        -6.0, abs=0.05)
    assert hardware.preamp_tapered_to_db(0.0) == 10.0


def test_linked_pair_resets_to_hard_left_and_right(tree):
    for ch in (0, 1):
        tree.set(f"{D}/inputs/{ch}/Stereo/value", True, hardware=False)
    for name, want in (("Pan", -1.0), ("Pan2", 1.0)):
        path = f"{D}/inputs/0/{name}/value"
        tree.set(path, 0.3, hardware=False)
        assert tree.set(path, "default", hardware=False)
        assert tree.get_value(path) == want


def test_map_default_wins_over_the_table(tree):
    path = f"{D}/DimAttenuation"
    tree.get_prop_dict(path)["default"] = 9
    assert tree.set(path + "/value", "default", hardware=False)
    assert tree.get_value(path + "/value") == 9


def test_property_without_a_default_is_refused(tree):
    assert not tree.set(f"{D}/inputs/0/Name/value", "default")


def test_daemon_reset_reaches_tree_companion_and_hardware(tree, backend):
    router = HardwareRouter(backend, tree)
    tree.on_set = router.on_set
    d = MixerDaemon(tree, router)
    client = MixerClient(None, Writer())
    path = f"{D}/inputs/0/sends/0/GainTapered/value"
    tree.set(path, 0.2, hardware=False)
    d._handle_set(client, Command("set", path, "default",
                                  raw_params={"func_id": "3"}))
    assert tree.get_value(path) == pytest.approx(UNITY)
    assert tree.get_value(f"{D}/inputs/0/sends/0/Gain/value") == \
        pytest.approx(0.0, abs=0.05)
