"""P0 host contracts on synthetic trees; no driver/device/vendor execution."""
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock

from console_commands import ConsoleCommands
from helper_tree import HelperTree
from hardware import HardwareRouter
from state_tree import StateTree

SRC = "/devices/0/inputs/0/effects/0"
DST = "/devices/0/inputs/1/effects/0"
UNI = "/devices/0/inputs/1/preamps/0/effects/0"


def fixture():
    state = StateTree()
    for base in ("/devices/0/inputs/0", "/devices/0/inputs/1"):
        for key, kind, value in [("Name", "string", "Fixture"), ("Pan", "float", 0.0),
                                 ("Mute", "bool", False), ("Preset", "string", "")]:
            state.add_runtime_property(base + "/" + key, {"type": kind, "value": value})
    for slot, name in [(SRC, "Fixture Unison"), (DST, ""), (UNI, "")]:
        state.add_runtime_property(slot + "/EffectName", {"type": "string", "value": name})
        state.add_runtime_property(slot + "/Power", {"type": "bool", "value": True})
    state.add_runtime_property(SRC + "/parameters/0/NormalizedValue", {"type": "float", "value": 0.25})
    for index, name, unison in [(0, "Fixture Unison", True), (1, "Fixture Other", False)]:
        for key, kind, value in [("Name", "string", name), ("Unison", "bool", unison),
                                 ("DemoAvailable", "bool", True)]:
            state.add_runtime_property(f"/plugins/{index}/{key}", {"type": kind, "value": value})
    return state


class ConsoleP0(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.state = fixture()
        self.helper = HelperTree()
        self.commands = ConsoleCommands(self.state, Path(self.directory.name) / "sessions",
                                        software_only=True, helper=self.helper,
                                        presets_dir=Path(self.directory.name) / "presets")

    def tearDown(self):
        self.directory.cleanup()

    def test_insert_copy_paste_move_and_structural_undo(self):
        self.commands.post("/CopyInsert", {"effectPath": SRC})
        self.commands.post("/PasteInsert", {"effectPaths": [DST, UNI]})
        self.assertEqual(self.state.get_value(DST + "/parameters/0/NormalizedValue"), 0.25)
        self.commands.post("/MoveInsert", {"srcPath": DST, "dstPath": SRC, "copy": False})
        self.assertEqual(self.state.get_value(DST + "/EffectName"), "")
        history = self.commands.history
        history.move(history.current - 1)
        self.assertEqual(self.state.get_value(DST + "/EffectName"), "Fixture Unison")
        self.assertEqual(self.state.get_value(DST + "/parameters/0/NormalizedValue"), 0.25)
        self.commands.post("/MoveInsert", {"srcPath": SRC, "dstPath": DST, "copy": True})
        self.assertEqual(self.state.get_value(SRC + "/EffectName"), "Fixture Unison")

    def test_invalid_destinations_are_atomic_and_unison_is_checked(self):
        self.state.set(SRC + "/EffectName/value", "Fixture Other")
        self.commands.post("/CopyInsert", {"effectPath": SRC})
        with self.assertRaises(ValueError):
            self.commands.post("/PasteInsert", {"effectPaths": [DST, UNI]})
        self.assertEqual(self.state.get_value(DST + "/EffectName"), "")
        with self.assertRaises(ValueError):
            self.commands.post("/MoveInsert", {"srcPath": SRC, "dstPath": DST, "copy": "false"})

    def test_settings_clipboard_tracks_plugin_identity(self):
        self.commands.post("/PluginBarCopySettings", {"effectPath": SRC})
        self.assertTrue(self.state.get_value(SRC + "/CanPastePluginBarSettings"))
        self.assertFalse(self.state.get_value(DST + "/CanPastePluginBarSettings"))
        self.state.set(SRC + "/parameters/0/NormalizedValue/value", 0.8)
        self.commands.post("/PluginBarPasteSettings", {"effectPath": SRC})
        self.assertEqual(self.state.get_value(SRC + "/parameters/0/NormalizedValue"), 0.25)
        with self.assertRaises(ValueError):
            self.commands.post("/PluginBarPasteSettings", {"effectPath": DST})

    def test_preset_save_load_rename_delete_and_folder_shapes(self):
        self.commands.post("/NewFolder", {"path": SRC + "/Preset", "folder": "Live"})
        self.commands.post("/SaveAsPreset", {"path": SRC, "pluginName": "Fixture Unison", "presetName": "Live/A"})
        self.state.set(SRC + "/parameters/0/NormalizedValue/value", 0.8)
        self.commands.set("test", SRC + "/Preset/value", "Live/A")
        self.assertEqual(self.state.get_value(SRC + "/parameters/0/NormalizedValue"), 0.25)
        meta = self.state.get_prop_dict(SRC + "/Preset")
        self.assertEqual(meta["plugin"], "Fixture Unison")
        self.assertEqual(meta["values"][0]["children"][0]["value"], "A")
        self.commands.post("/MovePreset", {"isFolder": False, "pluginName": "Fixture Unison",
                                            "origPresetName": "Live/A", "newPresetName": "Live/B"})
        self.commands.post("/DeletePreset", {"pluginName": "Fixture Unison", "presetName": "Live/B"})
        self.assertEqual(self.state.get_prop_dict(SRC + "/Preset")["values"][0]["children"], [])

    def test_native_channel_scalars_and_opaque_state_refusal(self):
        base = "/devices/0/inputs/1"
        data = b'<channel_strip version="1"><mixer_object type="kInput"><property id="kPropPan">0.6</property></mixer_object></channel_strip>'
        self.commands.presets.load_channel(base, data)
        self.assertEqual(self.state.get_value(base + "/Pan"), 0.6)
        bad = data.replace(b"</mixer_object>", b"<insert_effect><plugin_state>opaque</plugin_state></insert_effect></mixer_object>")
        with self.assertRaises(ValueError):
            self.commands.presets.load_channel(base, bad.replace(b"0.6", b"0.9"))
        self.assertEqual(self.state.get_value(base + "/Pan"), 0.6)

    def test_session_folder_rename_updates_loaded_path(self):
        sessions = self.commands.sessions
        sessions.post("NewFolder", {"folder": "A", "type": "session"})
        sessions.save("A/One")
        sessions.post("RenameSession", {"origName": "A", "newName": "B"})
        self.assertEqual(self.state.get_value("/Session"), "B/One")
        self.assertTrue((sessions.directory / "B/One.uadmix").is_file())

    def test_helper_interfaces_and_demo_simulation_are_explicit(self):
        entries = json.loads(self.helper.get_value("/dante/available_network_adapters"))
        self.assertTrue(entries)
        name = entries[0]["adapter_name"]
        self.commands.set("test", "/dante/primary_network_adapter/value", name)
        self.assertEqual(self.helper.get_value("/dante/primary_network_adapter"), name)
        self.commands.post("/PluginStartDemo", {"pluginPath": "/plugins/0"})
        self.assertTrue(self.state.get_value("/plugins/0/Demoing"))
        self.assertFalse(self.state.get_value("/plugins/0/DemoAvailable"))
        self.assertEqual(self.state.get_value("/openapollo/PluginBackend"), "software-model")

    def test_hardware_backend_does_not_claim_plugin_execution(self):
        self.commands.inserts.software_only = False
        with self.assertRaises(ValueError):
            self.commands.set("test", DST + "/EffectName/value", "Fixture Unison")
        with self.assertRaises(ValueError):
            self.commands.post("/PluginStartDemo", {"pluginPath": "/plugins/0"})
        self.assertEqual(self.state.get_value(DST + "/EffectName"), "")

    def test_talkback_routing_uses_root_paths_with_mock_hardware(self):
        router = HardwareRouter.__new__(HardwareRouter)
        router.backend = Mock()
        router.state_tree = self.state
        router.on_set("/TalkbackInPhysicalCR/value", True)
        router.backend.set_mixer_param.assert_called_with(2, 0, 0x47, 1)
        router.on_set("/TalkbackMaster/value", 0)
        router.backend.set_mixer_param.assert_called_with(2, 0, 0x48, 1)
        router.on_set("/TalkbackOn/value", True)
        self.assertEqual(router.backend.set_mixer_param.call_args_list[-1].args, (2, 0, 0x46, 1))


if __name__ == "__main__":
    unittest.main()
