"""Console-key posts and channel Preset recall through the real MixerDaemon
(protocol audit: scenes/sessions/presets, routes/IO matrix, aux)."""

import json

import pytest

from commands import ERR_FAILED
from conftest import DEVICE_MAP_X4, FakeBackend
from hardware import HardwareRouter
from protocol import Command
from state_tree import StateTree
from ua_mixer_daemon import MixerClient, MixerDaemon

CH = "/devices/0/inputs/0"


class Writer:
    def __init__(self):
        self.frames = []

    def get_extra_info(self, name):
        return ("127.0.0.1", 4710)

    def write(self, data):
        self.frames.append(data)


@pytest.fixture
def stack(tmp_path):
    tree = StateTree()
    tree.load_device_map(DEVICE_MAP_X4)
    router = HardwareRouter(FakeBackend(), tree)
    d = MixerDaemon(tree, router, sessions_dir=tmp_path / "sessions",
                    presets_dir=tmp_path / "presets")
    d.commands.set_store(tmp_path, "x4")
    d._hw_init_done = True
    return d


def send(d, verb, path, value=None):
    w = Writer()
    c = MixerClient(None, w, ubjson=False)
    cmd = Command(verb, path, value, raw_params={"func_id": "1"})
    (d._handle_post if verb == "post" else d._handle_set)(c, cmd)
    return [json.loads(f.rstrip(b"\0")) for f in w.frames][-1]


def post(d, path, body):
    return send(d, "post", path, body)


class TestChannelPresetRoundTrip:
    """A user-saved channel preset recalls on a real device: the values go
    through the normal set path, never the preamp or identity."""

    @pytest.mark.parametrize("base", [CH, "/devices/0/auxs/0"])
    @pytest.mark.parametrize("name", ["x", "Vocals/x"])
    def test_saved_preset_recalls(self, stack, base, name):
        d = stack
        d.state.set(base + "/FaderLevel/value", -12.0)
        assert post(d, "/SaveAsPreset", {
            "path": base, "pluginName": "Channel Strip",
            "presetName": name})["data"] is True
        d.state.set(base + "/FaderLevel/value", 0.0)
        r = send(d, "set", base + "/Preset/value", name)
        assert "error" not in r, r
        assert d.state.get_value(base + "/FaderLevel/value") == -12.0
        assert d.state.get_value(base + "/Preset/value") == name

    def test_recall_leaves_the_preamp_alone(self, stack):
        d = stack
        pre = CH + "/preamps/0/48V/value"
        d.state.set(pre, True)
        post(d, "/SaveAsPreset", {"path": CH, "pluginName": "Channel Strip",
                                  "presetName": "p"})
        d.state.set(pre, False)
        assert "error" not in send(d, "set", CH + "/Preset/value", "p")
        assert d.state.get_value(pre) is False

    def test_factory_name_without_file_refused(self, stack):
        r = send(stack, "set", CH + "/Preset/value", "nothing")
        assert r["error"] == "Unable to resolve path for set."
