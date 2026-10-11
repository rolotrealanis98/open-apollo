#!/usr/bin/env python3
"""Device-map selection regression checks, using only temporary descriptors."""
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "mixer-engine"))
import ua_mixer_daemon as daemon


class DeviceLookupTests(unittest.TestCase):
    def test_model_names_and_missing_map(self):
        for model, stem in [("Apollo Twin X", "apollo_twin_x"),
                            ("Apollo x4", "apollo_x4"),
                            ("Apollo x8p", "apollo_x8p"),
                            (None, "apollo_twin_x")]:
            with self.subTest(model=model), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                devices = root / "devices"
                maps = root / "device_maps"
                devices.mkdir()
                maps.mkdir()
                (devices / "apollo-twin-x.json").write_text(json.dumps({
                    "device_type": "0x23", "model": model,
                }))
                candidate = maps / f"device_map_{stem}.json"
                with patch.object(daemon, "DEVICES_DIR", devices), patch.object(daemon, "SCRIPT_DIR", root):
                    self.assertEqual(daemon.lookup_device(0x23), (model, None))
                    candidate.write_text("{}")
                    self.assertEqual(daemon.lookup_device(0x23), (model, candidate))
                    self.assertEqual(daemon.lookup_device(0x3A), (None, None))


if __name__ == "__main__":
    unittest.main()
