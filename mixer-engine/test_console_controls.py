"""Software-only native console command acceptance; never opens hardware."""
import asyncio
import json
import unittest
import tempfile
from pathlib import Path
from unittest.mock import Mock

from state_tree import StateTree
from ua_mixer_daemon import MixerDaemon


class ConsoleControls(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.state = StateTree()
        self.state.load_device_map(
            Path(__file__).parent / "device_maps/device_map_apollo_x4.json")
        self.hardware = Mock()
        self.state.on_set = self.hardware
        self.sessions_dir = tempfile.TemporaryDirectory()
        self.daemon = MixerDaemon(self.state, None, helper_port=0, sessions_dir=self.sessions_dir.name)
        self.server = await asyncio.start_server(
            self.daemon._handle_text_client, "127.0.0.1", 0)
        port = self.server.sockets[0].getsockname()[1]
        self.reader, self.writer = await asyncio.open_connection("127.0.0.1", port)
        # Consume connection bootstrap values before asserting command replies.
        await self.command("get /ping", "/ping")

    async def asyncTearDown(self):
        self.writer.close()
        await self.writer.wait_closed()
        self.server.close()
        await self.server.wait_closed()
        self.sessions_dir.cleanup()

    async def command(self, text, path):
        self.writer.write((text + "\0").encode())
        await self.writer.drain()
        while True:
            data = await asyncio.wait_for(self.reader.readuntil(b"\0"), 3)
            response = json.loads(data[:-1])
            if response.get("path") == path:
                return response

    async def echo(self, text, path, func_id):
        # Request query belongs on the target, before the payload.
        verb, target, *tail = text.split(" ", 2)
        wire = f"{verb} {target}?func_id={func_id}" + (" " + tail[0] if tail else "")
        self.writer.write((wire + "\0").encode())
        await self.writer.drain()
        while True:
            response = json.loads((await asyncio.wait_for(self.reader.readuntil(b"\0"), 3))[:-1])
            if response.get("path") == path and response.get("parameters", {}).get("func_id") == func_id:
                return response

    async def test_sleep_is_per_connection_and_does_not_dirty_mixer(self):
        await self.echo("set /Sleep true", "/Sleep", 20)
        self.assertTrue((await self.echo("get /Sleep", "/Sleep", 21))["data"])
        reader, writer = await asyncio.open_connection("127.0.0.1", self.server.sockets[0].getsockname()[1])
        try:
            writer.write(b"get /Sleep?func_id=22\0")
            await writer.drain()
            while True:
                value = json.loads((await asyncio.wait_for(reader.readuntil(b"\0"), 3))[:-1])
                if value.get("parameters", {}).get("func_id") == 22:
                    self.assertFalse(value["data"])
                    break
        finally:
            writer.close()
            await writer.wait_closed()
        self.assertFalse(self.state.get_value("/Dirty"))

    async def test_gesture_undo_redo_and_request_queries(self):
        pan = "/devices/0/inputs/0/Pan/value"
        initial = self.state.get_value(pan)
        await self.echo("set /UndoRecording/value true", "/UndoRecording/value", 30)
        await self.echo(f"set {pan} 0.2", pan, 31)
        await self.echo(f"set {pan} 0.4", pan, 32)
        await self.echo("set /UndoRecording/value false", "/UndoRecording/value", 33)
        self.assertEqual(self.state.get_value("/UndoRedo"), 1)
        values = await self.command("get /UndoRedo/values?cmdID=15&func_id=34", "/UndoRedo/values")
        self.assertEqual(values["parameters"]["func_id"], 34)
        self.assertEqual(values["data"][0]["enabled"], True)
        await self.echo("set /UndoRedo/value 0", "/UndoRedo/value", 35)
        self.assertEqual(self.state.get_value(pan), initial)
        await self.echo("set /UndoRedo/value 1", "/UndoRedo/value", 36)
        self.assertEqual(self.state.get_value(pan), 0.4)
        self.assertIn("error", await self.echo("set /UndoRedo/value 99", "/UndoRedo/value", 37))

    async def test_scenes_save_recall_overwrite_clear_and_rename(self):
        power = "/devices/0/inputs/0/effects/0/Power/value"
        await self.echo(f"set {power} true", power, 40)
        await self.echo('post /SceneOperation {"StoreScene":2}', "/SceneOperation", 41)
        name = "/pluginscenes/0/scenes/2/SceneName/value"
        await self.echo(f'set {name} " verse "', name, 42)
        self.assertEqual(self.state.get_value(name), "VERSE")
        await self.echo(f"set {power} false", power, 43)
        await self.echo('post /SceneOperation {"RecallScene":2}', "/SceneOperation", 44)
        self.assertTrue(self.state.get_value(power))
        self.assertEqual(self.state.get_value("/pluginscenes/0/RecalledSceneIndex"), 2)
        await self.echo('post /SceneOperation {"OverwriteScene":2}', "/SceneOperation", 45)
        await self.echo('post /SceneOperation {"ClearScene":2}', "/SceneOperation", 46)
        self.assertEqual(self.state.get_value(name), "EMPTY SCENE")
        self.assertIn("error", await self.echo('post /SceneOperation {"RecallScene":2}', "/SceneOperation", 47))

    async def test_session_xml_files_folders_load_and_modified_restore(self):
        pan = "/devices/0/inputs/0/Pan/value"
        await self.echo('post /NewFolder {"folder":"Live","type":"session"}', "/NewFolder", 50)
        await self.echo(f"set {pan} 0.7", pan, 51)
        await self.echo('post /SaveSessionAs {"name":"Live/Band.uadmix"}', "/SaveSessionAs", 52)
        file = Path(self.sessions_dir.name) / "Live/Band.uadmix"
        self.assertIn(b"uad_mixer_session", file.read_bytes())
        self.assertFalse(self.state.get_value("/Dirty"))
        await self.echo(f"set {pan} -0.6", pan, 53)
        await self.echo('set /Session/value "Live/Band"', "/Session/value", 54)
        self.assertEqual(self.state.get_value(pan), 0.7)
        modified = self.state.get_prop_dict("/ModifiedSessions")["values"]
        key = modified[-1]["value"]
        await self.echo(f'set /ModifiedSessions/value "{key}"', "/ModifiedSessions/value", 55)
        self.assertEqual(self.state.get_value(pan), -0.6)
        await self.echo('post /RenameSession {"origName":"Live/Band","newName":"Live/Renamed"}', "/RenameSession", 56)
        self.assertTrue(file.with_name("Renamed.uadmix").exists())
        await self.echo('post /DeleteSession {"name":"Live/Renamed"}', "/DeleteSession", 57)
        await self.echo('post /DeleteFolder {"folder":"Live","type":"session"}', "/DeleteFolder", 58)
        self.assertFalse(file.parent.exists())
        self.assertIn("error", await self.echo('post /SaveSessionAs {"name":""}', "/SaveSessionAs", 59))
        self.assertIn("error", await self.echo('post /NewFolder {"folder":"../bad","type":"session"}', "/NewFolder", 60))
        self.assertIn("error", await self.echo('post /OpenSession {"path":""}', "/OpenSession", 61))

    async def test_connect_properties_and_empty_midi_list(self):
        self.assertIn("data", await self.echo("get /MixerState/value", "/MixerState/value", 70))
        response = await self.echo("get /MidiInputDevice/values", "/MidiInputDevice/values", 71)
        self.assertIsInstance(response["data"], list)

    async def test_reset_pushes_numeric_default_and_echoes_it(self):
        path = "/devices/0/inputs/0/Pan/value"
        self.state.set(path, 0.75)
        self.hardware.reset_mock()
        await self.command(f"subscribe {path}", path)
        response = await self.command(f'set {path}?func_id=7 "default"', path)
        self.assertEqual(response["data"], 0.0)
        self.assertEqual(self.state.get_value(path), 0.0)
        self.hardware.assert_called_once_with(path, 0.0)
        echo = await asyncio.wait_for(self.reader.readuntil(b"\0"), 3)
        self.assertEqual(json.loads(echo[:-1])["data"], 0.0)

    async def test_unknown_default_is_refused_without_mutation(self):
        path = "/SampleRate/value"
        before = self.state.get_value(path)
        response = await self.command(f'set {path}?func_id=8 "default"', path)
        self.assertIn("error", response)
        self.assertEqual(self.state.get_value(path), before)
        self.hardware.assert_not_called()

    async def test_fader_reset_uses_unity_not_captured_current_value(self):
        path = "/devices/0/inputs/0/FaderLevelTapered/value"
        self.state.set(path, 0.1)
        response = await self.command(f'set {path}?func_id=13 "default"', path)
        self.assertAlmostEqual(response["data"], 43.0 / 55.0)

    async def test_malformed_clear_payload_is_refused(self):
        clip = "/devices/0/inputs/0/meters/0/MeterClip/value"
        self.state.set(clip, True)
        response = await self.command('post /ClearClips?func_id=14 "bad"', "/ClearClips")
        self.assertIn("error", response)
        self.assertTrue(self.state.get_value(clip))

    async def test_explicit_default_survives_loading(self):
        self.state.add_runtime_property("/TestGain", {
            "type": "float", "value": 10.0, "default": 4.0})
        path = "/TestGain/value"
        response = await self.command(f'set {path}?func_id=9 "default"', path)
        self.assertEqual(response["data"], 4.0)

    async def test_global_and_scoped_clear_commands(self):
        solo = "/devices/0/inputs/0/Solo/value"
        clip = "/devices/0/inputs/0/meters/0/MeterClip/value"
        other_clip = "/devices/0/inputs/1/meters/0/MeterClip/value"
        for path in (solo, clip, other_clip):
            self.state.set(path, True)
        self.assertTrue((await self.command(
            'post /ClearClips?func_id=10 {"path":"/devices/0/inputs/0"}',
            "/ClearClips"))["data"])
        self.assertFalse(self.state.get_value(clip))
        self.assertTrue(self.state.get_value(other_clip))
        await self.command('post /ClearClips?func_id=11 {}', "/ClearClips")
        await self.command('post /ClearSolos?func_id=12 {}', "/ClearSolos")
        self.assertFalse(self.state.get_value(other_clip))
        self.assertFalse(self.state.get_value(solo))


if __name__ == "__main__":
    unittest.main()
