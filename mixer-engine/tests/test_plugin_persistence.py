"""Persisted plugin intent must not masquerade as a restored DSP instance."""

import json
from pathlib import Path
import tempfile
import unittest
from plugin_lifecycle import is_plugin_slot_path
from state_tree import StateTree


class PluginPersistenceTests(unittest.TestCase):
    slot = "/devices/0/auxs/0/effects/0"
    gain = "/devices/0/inputs/0/preamps/0/Gain"

    def tree(self):
        state = StateTree()
        for path, kind, value in [
            (self.slot + "/EffectName", "string", ""),
            (self.slot + "/EffectInstance", "pointer", 0),
            (self.slot + "/Power", "bool", False),
            (self.gain, "float", 10.0),
        ]:
            state.add_runtime_property(path, {"type": kind, "value": value})
        return state

    def test_plugin_restore_deferred_and_preserved_across_mixer_save(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "session.json"
            captured = {
                self.slot + "/EffectName/value": "prior name",
                self.slot + "/EffectInstance/value": 123456,
                self.slot + "/Power/value": True,
                self.slot + "/parameters/0/NormalizedValue/value": 0.75,
                self.gain + "/value": 32.0,
            }
            original = json.dumps(captured).encode()
            path.write_bytes(original)
            state = self.tree()
            writes = []
            state.on_set = lambda *args: writes.append(args)
            state.enable_persistence(
                path, restore_filter=lambda key: not is_plugin_slot_path(key)
            )
            self.assertEqual(state.get_value(self.gain + "/value"), 32.0)
            self.assertEqual(
                state.get_value(self.slot + "/EffectName/value"), ""
            )
            self.assertEqual(
                state.get_value(self.slot + "/EffectInstance/value"), 0
            )
            self.assertFalse(state.get_value(self.slot + "/Power/value"))
            self.assertEqual(path.read_bytes(), original)
            self.assertEqual(writes, [])
            state.set(self.gain + "/value", 40.0)
            state.save_now()
            restored = json.loads(path.read_text())
            for key, value in captured.items():
                self.assertEqual(
                    restored[key],
                    40.0 if key == self.gain + "/value" else value,
                )
            self.assertEqual(writes, [(self.gain + "/value", 40.0)])
            restarted = self.tree()
            restarted.enable_persistence(
                path, restore_filter=lambda key: not is_plugin_slot_path(key)
            )
            self.assertEqual(
                restarted.get_value(self.slot + "/EffectInstance/value"), 0
            )
            self.assertEqual(restarted.get_value(self.gain + "/value"), 40.0)

    def test_unfiltered_persistence_retains_existing_api_behavior(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "session.json"
            path.write_text(json.dumps({self.gain + "/value": 22.0}))
            state = self.tree()
            state.enable_persistence(path)
            self.assertEqual(state.get_value(self.gain + "/value"), 22.0)


if __name__ == "__main__":
    unittest.main()
