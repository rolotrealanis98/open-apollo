"""Synthetic native-shaped XML fixtures and host-only session persistence."""
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock

from console_commands import ConsoleCommands
from state_tree import StateTree
import session_xml


def tree():
    state = StateTree()
    for path, kind, value in [
        ("/devices/0/inputs/0/Pan", "float", 0.0),
        ("/devices/0/inputs/0/sends/0/Gain", "float", -12.0),
        ("/devices/0/inputs/0/sends/2/Bypass", "bool", False),
        ("/devices/0/inputs/0/preamps/0/48V", "bool", False),
        ("/devices/0/inputs/0/effects/0/EffectName", "string", ""),
        ("/devices/0/inputs/0/effects/0/Power", "bool", True),
    ]:
        state.add_runtime_property(path, {"type": kind, "value": value})
    return state


class SessionTests(unittest.TestCase):
    def setUp(self):
        self.folder = tempfile.TemporaryDirectory()
        self.state = tree()
        self.commands = ConsoleCommands(self.state, self.folder.name)
        self.state.on_set = Mock()

    def tearDown(self):
        self.folder.cleanup()

    def test_native_scalar_and_flattened_send_preamp_mapping_preserves_opaque_xml(self):
        xml = b'''<uad_mixer_session><console_settings version="1"/>
          <mixer_tree version="4"><mixer_object type="kRoot" relative_index="0">
          <mixer_object type="kMixer" relative_index="0">
          <mixer_object type="kInput" relative_index="0">
          <property id="kPropPan" type="kPTFloat">0.75</property>
          <property id="kPropAux1SendLevel" type="kPTFloat">-5</property>
          <property id="kPropHP1Bypass" type="kPTBoolean">true</property>
          <property id="kProp48VEnable" type="kPTBoolean">true</property>
          <future value="keep"/><insert_effect relative_index="0" uuid="fixture">
          <plugin_name>Fixture Plugin</plugin_name><plugin_state>opaque-fixture</plugin_state>
          </insert_effect></mixer_object></mixer_object></mixer_object></mixer_tree>
          </uad_mixer_session>'''
        file = Path(self.folder.name) / "Original.uadmix"
        file.write_bytes(xml)
        sessions = self.commands.sessions
        sessions.open(file)
        self.assertEqual(self.state.get_value("/devices/0/inputs/0/sends/0/Gain"), -5)
        self.assertTrue(self.state.get_value("/devices/0/inputs/0/preamps/0/48V"))
        self.assertEqual(self.state.get_value("/devices/0/inputs/0/effects/0/EffectName"), "Fixture Plugin")
        self.state.set("/devices/0/inputs/0/Pan/value", -0.5)
        sessions.save()
        root = session_xml.parse(file.read_bytes())
        self.assertEqual(root.findtext(".//plugin_state"), "opaque-fixture")
        self.assertEqual(root.find(".//future").get("value"), "keep")
        self.assertEqual(root.find(".//property[@id='kPropPan']").text, "-0.5")

    def test_generated_xml_has_native_send_and_preamp_fields(self):
        data = self.commands.sessions.xml_bytes()
        root = session_xml.parse(data)
        self.assertIsNotNone(root.find(".//property[@id='kPropAux1SendLevel']"))
        self.assertIsNotNone(root.find(".//property[@id='kProp48VEnable']"))
        values = session_xml.read_values(root, self.state)
        self.assertEqual(values["/devices/0/inputs/0/sends/0/Gain"], -12.0)

    def test_last_used_restart_and_scenes_survive_session_round_trip(self):
        power = "/devices/0/inputs/0/effects/0/Power"
        self.commands.post("/SceneOperation", {"StoreScene": 4})
        self.commands.sessions.save("Saved")
        self.state.set(power + "/value", False)
        self.commands.post("/SaveLastUsedSession", {})
        other_state = tree()
        other = ConsoleCommands(other_state, self.folder.name)
        self.assertFalse(other_state.get_value(power))
        self.assertEqual(other_state.get_value("/Session"), "Saved")
        other.post("/SceneOperation", {"RecallScene": 4})
        self.assertTrue(other_state.get_value(power))

    def test_invalid_document_does_not_apply_partial_values(self):
        path = "/devices/0/inputs/0/Pan"
        xml = b'''<uad_mixer_session><mixer_tree version="4"><mixer_object type="kRoot" relative_index="0">
          <mixer_object type="kMixer" relative_index="0"><mixer_object type="kInput" relative_index="0">
          <property id="kPropPan">0.8</property><property id="kPropAux1SendLevel">bad</property>
          </mixer_object></mixer_object></mixer_object></mixer_tree></uad_mixer_session>'''
        file = Path(self.folder.name) / "Bad.uadmix"
        file.write_bytes(xml)
        with self.assertRaises(ValueError):
            self.commands.sessions.open(file)
        self.assertEqual(self.state.get_value(path), 0.0)
        self.state.on_set.assert_not_called()
        for data in [b"<!DOCTYPE x><uad_mixer_session/>", b"<malformed>"]:
            with self.assertRaises(ValueError):
                session_xml.parse(data)

    def test_undo_groups_keep_other_clients_changes_separate(self):
        history = self.commands.history
        pan = "/devices/0/inputs/0/Pan"
        gain = "/devices/0/inputs/0/sends/0/Gain"
        history.bracket("A", True)
        before = history.snapshot()
        self.state.set(pan + "/value", 0.4)
        history.record("A", before)
        before = history.snapshot()
        self.state.set(gain + "/value", -3.0)
        history.record("B", before)
        history.bracket("A", False)
        history.move(1)
        self.assertEqual(self.state.get_value(pan), 0.0)
        self.assertEqual(self.state.get_value(gain), -3.0)


if __name__ == "__main__":
    unittest.main()
