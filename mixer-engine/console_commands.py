"""Console 1.3.1 host commands composed over the daemon's state tree."""
from pathlib import Path
from console_history import History
from console_sessions import Sessions
from console_inserts import Inserts, SLOT
from console_presets import Presets
from console_network import Network
import re


class ConsoleCommands:
    def __init__(self, state, directory=None, *, software_only=False,
                 helper=None, presets_dir=None, factory_presets=None):
        self.state = state
        self.scenes = {}
        self.history = History(state, self.publish)
        self.inserts = Inserts(state, self.history, self.publish, software_only)
        self.sessions = Sessions(state, self.history, self.publish,
                                 directory or Path.home() / "Documents/Universal Audio/Sessions",
                                 self.scenes)
        self.publish("/MixerState", 0, "int")
        self.publish("/MidiInputDevice", "", "string")
        state.get_prop_dict("/MidiInputDevice").setdefault("values", [])
        self.publish("/Progress", "", "string")
        self.publish("/Dirty", False)
        for index in range(128):
            base = f"/pluginscenes/0/scenes/{index}"
            if state.get_prop_dict(base + "/SceneName") is None:
                self.publish(base + "/SceneName", "EMPTY SCENE", "string")
                self.publish(base + "/SceneTempo", 120.0, "float")
        self.publish("/pluginscenes/0/RecalledSceneIndex", -1, "int")
        self.presets = Presets(state, self.history, self.publish, self.inserts,
                               presets_dir or Path.home() / ".local/share/open-apollo/Presets",
                               factory_presets)
        self.network = Network(state, helper, self.publish)
        self.inserts.refresh_paste()
        self.publish("/openapollo/PluginBackend", "software-model" if software_only else "unavailable", "string")
        for path in list(state._props):
            if re.fullmatch(r"/devices/\d+/inputs/\d+/Name", path):
                base = path.removesuffix("/Name")
                if state.get_prop_dict(base + "/ChannelHidden") is None:
                    self.publish(base + "/ChannelHidden", False)
            if re.fullmatch(r"/plugins/\d+/Name", path):
                base = path.removesuffix("/Name")
                for key in ("Unison", "Demoing", "DemoAvailable"):
                    if state.get_prop_dict(base + "/" + key) is None:
                        self.publish(base + "/" + key, False)
                if state.get_prop_dict(base + "/DemoDaysRemaining") is None:
                    self.publish(base + "/DemoDaysRemaining", 0, "int")
            if path.endswith("/MixInSource"):
                meta = state.get_prop_dict(path)
                values = meta.get("values") or []
                names = [v.get("value") if isinstance(v, dict) else v for v in values]
                if "cue1" in names and "mon" in names:
                    for name in ("cue3", "cue4"):
                        if name not in names:
                            values.append({"value": name} if values and isinstance(values[0], dict) else name)
                    meta["values"] = values
        if state.get_prop_dict("/TalkbackOn") is None:
            self.publish("/TalkbackOn", bool(state.get_value("/devices/0/outputs/18/TalkbackOn")))
        if state.get_prop_dict("/TalkbackInPhysicalCR") is None:
            self.publish("/TalkbackInPhysicalCR", False)
        if state.get_prop_dict("/TalkbackMaster") is None:
            self.publish("/TalkbackMaster", 0, "int")

    def publish(self, path, value, kind="bool"):
        meta = self.state.get_prop_dict(path)
        if meta is None:
            self.state.add_runtime_property(path, {"type": kind, "value": value})
        else:
            meta["value"] = value
        self.state._notify_subscribers(path + "/value", value)

    def set(self, client, path, value):
        prop = path.removesuffix("/value")
        if prop.endswith("/EffectName") and SLOT.fullmatch(prop.removesuffix("/EffectName")):
            self.inserts.assign(prop.removesuffix("/EffectName"), value)
            self.presets.refresh()
        elif prop.endswith("/Preset"):
            self.presets.load(prop.removesuffix("/Preset"), value)
        elif prop == "/dante/primary_network_adapter":
            self.network.set_primary(value)
        elif prop.endswith("/MixInSource") and value in ("cue3", "cue4"):
            if not self.inserts.software_only:
                raise ValueError("CUE3/4 hardware encoding has not been verified")
            return False
        elif prop == "/UndoRecording":
            self.history.bracket(client, value)
        elif prop == "/UndoRedo":
            self.history.move(value)
        elif prop == "/Session":
            self.sessions.load(value)
        elif prop == "/ModifiedSessions":
            self.sessions.restore(value)
        else:
            return False
        return True

    def post(self, path, args):
        if not isinstance(args, dict):
            raise ValueError("command arguments must be an object")
        command = path.removeprefix("/")
        if self.inserts.post(command, args) or self.presets.post(command, args):
            self.presets.refresh()
            return True
        if command == "PluginStartDemo":
            plugin = args.get("pluginPath", "")
            if not self.inserts.software_only:
                raise ValueError("native demo activation requires the vendor backend")
            if not isinstance(plugin, str) or not re.fullmatch(r"/plugins/\d+", plugin) or not self.state.get_value(plugin + "/DemoAvailable"):
                raise ValueError("demo is unavailable")
            self.publish(plugin + "/Demoing", True)
            self.publish(plugin + "/DemoAvailable", False)
            self.publish(plugin + "/DemoDaysRemaining", 14, "int")
            return True  # A simulated footer state, never vendor authorization.
        if path != "/SceneOperation":
            return self.sessions.post(path.removeprefix("/"), args)
        if len(args) != 1:
            raise ValueError("one scene operation is required")
        operation, index = next(iter(args.items()))
        if operation not in ("StoreScene", "RecallScene", "ClearScene", "OverwriteScene") or type(index) is not int or not 0 <= index < 128:
            raise ValueError("invalid scene operation/index")
        base = f"/pluginscenes/0/scenes/{index}"
        before = self.history.snapshot()
        if operation in ("StoreScene", "OverwriteScene"):
            if operation == "OverwriteScene" and index not in self.scenes:
                raise ValueError("scene is empty")
            self.scenes[index] = {p: v for p, v in before.items() if "/effects/" in p or p == "/Tempo"}
            if self.state.get_value(base + "/SceneName") == "EMPTY SCENE":
                self.state.set(base + "/SceneName/value", f"SCENE {index + 1}")
            self.publish(base + "/SceneTempo", self.state.get_value("/Tempo") or 120.0, "float")
        elif operation == "ClearScene":
            self.scenes.pop(index, None)
            self.state.set(base + "/SceneName/value", "EMPTY SCENE")
            if self.state.get_value("/pluginscenes/0/RecalledSceneIndex") == index:
                self.publish("/pluginscenes/0/RecalledSceneIndex", -1, "int")
        else:
            if index not in self.scenes:
                raise ValueError("scene is empty")
            for prop, value in self.scenes[index].items():
                self.state.set(prop + "/value", value)
            self.publish("/pluginscenes/0/RecalledSceneIndex", index, "int")
        self.history.record("scene", before)
        self.publish("/Dirty", True)
        return True
