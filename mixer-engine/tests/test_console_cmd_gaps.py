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


class TestConsoleKeys:
    """Console 1.3.1's argument keys (MAP.md "Commands"), through the
    daemon's post path; the daemon's older keys keep working."""

    def test_iomap_preset_save_rename_delete(self, stack):
        d = stack
        assert post(d, "/SaveIOMapPreset", {"ioPresetName": "a"})["data"]
        names = lambda: [v["value"] for v in
                         d.state.get_prop_dict("/IOMapPreset")["values"]]
        assert "a" in names()
        assert post(d, "/RenameIOMapPreset",
                    {"origPreset": "a", "newPreset": "b"})["data"]
        assert "b" in names() and "a" not in names()
        assert post(d, "/DeleteIOMapPreset", {"ioPresetName": "b"})["data"]
        assert "b" not in names()
        # a missing preset still fails
        assert post(d, "/DeleteIOMapPreset",
                    {"ioPresetName": "b"})["error"] == ERR_FAILED

    def test_iomap_old_keys(self, stack):
        d = stack
        assert post(d, "/SaveIOMapPreset", {"name": "a"})["data"]
        assert post(d, "/RenameIOMapPreset",
                    {"name": "a", "new_name": "b"})["data"]
        assert post(d, "/DeleteIOMapPreset", {"name": "b"})["data"]

    def test_snapshots(self, stack):
        d = stack
        for n in ("one", "Live/two"):
            r = post(d, "/SaveAsSnapshot", {"snapshotName": n, "isNew": "true"})
            assert r["data"]["name"] == n
        r = post(d, "/MoveSnapshot", {"origName": "one", "newName": "Live/uno",
                                      "shuffle": "false"})
        assert r["data"]["snapshots"] == ["Live/uno", "Live/two"]
        assert post(d, "/MoveSnapshot", {"origName": "nope",
                                         "newName": "x"})["error"] == ERR_FAILED
        # onto an existing name: Shuffle is refused, Overwrite (Console asked
        # first) replaces it
        assert post(d, "/MoveSnapshot", {"origName": "Live/uno",
                                         "newName": "Live/two",
                                         "shuffle": True})["error"] == ERR_FAILED
        r = post(d, "/MoveSnapshot", {"origName": "Live/two",
                                      "newName": "Live/uno", "shuffle": False})
        assert r["data"]["snapshots"] == ["Live/uno"]
        r = post(d, "/DeleteSnapshot", {"snapshotName": "Live/uno"})
        assert r["data"]["snapshots"] == []
        assert post(d, "/DeleteSnapshot",
                    {"snapshotName": "Live/uno"})["error"] == ERR_FAILED
        assert post(d, "/SaveAsSnapshot", {"snapshotName": "../x"})[
            "error"] == ERR_FAILED

    def test_snapshot_old_keys(self, stack):
        d = stack
        assert post(d, "/SaveAsSnapshot", {"name": "a"})["data"]
        assert post(d, "/DeleteSnapshot", {"name": "a"})["data"]

    def test_session_commands_take_console_keys(self, stack):
        d = stack
        post(d, "/NewFolder", {"folder": "Live", "type": "session"})
        assert post(d, "/DeleteFolder",
                    {"folder": "Live", "type": "session"})["data"] is True
        # the engine-side session commands (object posts) too
        data, err = d.commands.execute("/SaveSessionAs", {"name": "s1"})
        assert err is None
        data, err = d.commands.execute(
            "/RenameSession", {"origName": "s1", "newName": "s2"})
        assert err is None and data["sessions"] == ["s2"]
        data, err = d.commands.execute("/DeleteSession", {"name": "s2"})
        assert err is None and data["sessions"] == []

    def test_preset_folder_new_and_delete(self, stack):
        d = stack
        args = {"folder": "Vox", "path": CH + "/Preset"}
        assert post(d, "/NewFolder", args)["data"] is True
        assert (stack.console.presets.directory / "Channel%20Strip" / "Vox").is_dir()
        assert post(d, "/DeleteFolder", args)["data"] is True
        assert post(d, "/DeleteFolder", args)["error"] == ERR_FAILED


class TestRescanAndImport:
    def catalogue(self, tmp_path, names):
        from test_ua_catalogue import PLUGINS, write_catalogue
        extra = [dict(PLUGINS[0], name=n, plugin_id=n, vst_id=n[:4])
                 for n in names]
        return write_catalogue(tmp_path / "cat", plugins=PLUGINS + extra)

    def daemon(self, tmp_path, catalogue_dir=None):
        import ua_catalogue
        tree = StateTree()
        tree.load_device_map(DEVICE_MAP_X4)
        cat = ua_catalogue.apply(tree, None, catalogue_dir)
        d = MixerDaemon(tree, HardwareRouter(FakeBackend(), tree),
                        sessions_dir=tmp_path / "s", presets_dir=tmp_path / "p",
                        catalogue=cat)
        d.console.catalogue_dir = catalogue_dir
        return d

    def test_rescan_without_import_is_ok(self, tmp_path):
        d = self.daemon(tmp_path, tmp_path / "none")
        assert post(d, "/UpdatePlugInStatus", {})["data"] is True
        assert not d.state.path_exists("/plugins/0/Name/value")

    def test_rescan_picks_up_a_newer_import(self, tmp_path):
        d = self.daemon(tmp_path, self.catalogue(tmp_path, []))
        assert d.state.get_value("/plugins/1/Name") == "Test Pre"
        assert not d.state.path_exists("/plugins/2/Name/value")
        pushed = []
        d.state.register_change_observer(
            "t", lambda path, value: pushed.append(path))
        self.catalogue(tmp_path, ["Brand New"])    # `oa-assets import` again
        assert post(d, "/UpdatePlugInStatus", {})["data"] is True
        assert d.state.get_value("/plugins/2/Name") == "Brand New"
        assert d.state.get_value("/plugins/2/DemoAvailable") is False
        assert "/plugins/2/Name/value" in pushed
        # old entries keep their index, a second rescan changes nothing
        assert d.state.get_value("/plugins/0/Name") == "Test Comp"
        pushed.clear()
        assert post(d, "/UpdatePlugInStatus", {})["data"] is True
        assert not [p for p in pushed if p.endswith("/Name/value")]

    def test_rescan_fills_in_a_late_first_import(self, tmp_path):
        d = self.daemon(tmp_path, tmp_path / "cat")
        assert not d.state.path_exists("/plugins/0/Name/value")
        self.catalogue(tmp_path, [])
        assert post(d, "/UpdatePlugInStatus", {})["data"] is True
        assert d.state.get_value("/plugins/0/Name") == "Test Comp"
        # and the channel strip's factory names reach the preset menus
        assert d.console.presets.catalogue

    def test_import_is_refused_and_says_why(self, stack, caplog):
        r = post(stack, "/Import", {"type": "IOMapPreset"})
        assert r["error"] == ERR_FAILED
        assert "file dialog" in caplog.text
