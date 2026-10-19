"""`enabled` metadata: 48V/Pad/IOType/Stereo follow the Hi-Z jack and the
IOType, SRConvert follows the clock source; live, from the readback's
notification path."""

import pytest

from conftest import DEVICE_MAP_X4
from state_tree import StateTree
from ua_mixer_daemon import MixerDaemon

IN = "/devices/0/inputs"


@pytest.fixture
def tree():
    t = StateTree()
    t.load_device_map(DEVICE_MAP_X4)
    MixerDaemon(t, None)
    return t


def enabled(tree, path):
    return tree.get(path)["enabled"]


def readback(tree, path, value):
    """What the daemon's hardware readback does (bypasses state.set)."""
    tree.get_prop_dict(path)["value"] = value
    tree._notify_subscribers(path + "/value", value)


def test_published_on_the_engines_four_patterns(tree):
    published = {p.split("/")[-1] for p, prop in tree._props.items()
                 if "enabled" in prop}
    assert published == {"IOType", "Stereo", "48V", "Pad", "SRConvert"}
    # the engine dump: IOType and Stereo on every input, 48V/Pad on 4 preamps
    assert sum(p.endswith("/IOType") and "enabled" in prop
               for p, prop in tree._props.items() if "/inputs/" in p) == 24
    assert sum(p.endswith("/Stereo") and "enabled" in prop
               for p, prop in tree._props.items() if "/inputs/" in p) == 24


def test_mic_enables_48v_and_pad_line_disables(tree):
    assert tree.get_value(f"{IN}/0/IOType/value") == "Mic"
    assert enabled(tree, f"{IN}/0/preamps/0/48V") is True
    assert enabled(tree, f"{IN}/0/preamps/0/Pad") is True
    readback(tree, f"{IN}/0/IOType", "Line")
    assert enabled(tree, f"{IN}/0/preamps/0/48V") is False
    assert enabled(tree, f"{IN}/0/preamps/0/Pad") is False
    assert enabled(tree, f"{IN}/1/preamps/0/48V") is True


def test_hi_z_jack_disables_48v_pad_iotype_and_link(tree):
    base = f"{IN}/0"
    assert enabled(tree, f"{base}/IOType") is True
    readback(tree, f"{base}/preamps/0/HiZ", True)
    assert enabled(tree, f"{base}/preamps/0/48V") is False
    assert enabled(tree, f"{base}/preamps/0/Pad") is False
    assert enabled(tree, f"{base}/IOType") is False
    assert enabled(tree, f"{base}/Stereo") is False
    assert enabled(tree, f"{IN}/1/Stereo") is False      # its partner
    assert enabled(tree, f"{IN}/1/IOType") is True       # CH2 has no jack in
    assert enabled(tree, f"{IN}/2/Stereo") is True
    readback(tree, f"{base}/preamps/0/HiZ", False)
    assert enabled(tree, f"{base}/preamps/0/48V") is True
    assert enabled(tree, f"{base}/IOType") is True
    assert enabled(tree, f"{base}/Stereo") is True
    assert enabled(tree, f"{IN}/1/Stereo") is True


def test_an_existing_link_can_still_be_broken(tree):
    readback(tree, f"{IN}/0/Stereo", True)
    readback(tree, f"{IN}/0/preamps/0/HiZ", True)
    assert enabled(tree, f"{IN}/0/Stereo") is True


def test_sr_convert_follows_the_clock_source(tree):
    sr = f"{IN}/12/SRConvert"
    assert enabled(tree, sr) is True
    readback(tree, "/ClockSource", "S/PDIF")
    assert enabled(tree, sr) is False
    assert enabled(tree, f"{IN}/13/SRConvert") is False
    readback(tree, "/ClockSource", "Internal")
    assert enabled(tree, sr) is True
