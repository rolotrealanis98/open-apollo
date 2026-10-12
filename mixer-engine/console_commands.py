"""Console 1.3.1 host commands composed over the daemon's state tree."""
from pathlib import Path
from console_history import History
from console_sessions import Sessions


class ConsoleCommands:
    def __init__(self, state, directory=None):
        self.state = state
        self.scenes = {}
        self.history = History(state, self.publish)
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

    def publish(self, path, value, kind="bool"):
        meta = self.state.get_prop_dict(path)
        if meta is None:
            self.state.add_runtime_property(path, {"type": kind, "value": value})
        else:
            meta["value"] = value
        self.state._notify_subscribers(path + "/value", value)

    def set(self, client, path, value):
        prop = path.removesuffix("/value")
        if prop == "/UndoRecording":
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
