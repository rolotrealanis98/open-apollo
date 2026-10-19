"""Console 1.3.1 host commands composed over the daemon's state tree."""
import logging
from pathlib import Path
import ua_catalogue
from console_history import History
from console_sessions import Sessions
from console_inserts import Inserts, SLOT
from console_presets import Presets, CHANNEL_STRIP
from console_network import Network
import re

log = logging.getLogger("ua-mixer-daemon")


class ConsoleCommands:
    def __init__(self, state, directory=None, *, software_only=False,
                 helper=None, presets_dir=None, factory_presets=None,
                 restore_values=True, catalogue=None):
        # catalogue: the user's UA import (ua_catalogue.apply), whose
        # plug-in list the daemon has already laid on /plugins; None
        # without one (no plug-ins, no factory preset names)
        self.state = state
        # Where RESCAN looks again: the import folder of the overlay in use
        # (None: the default one)
        self.catalogue_dir = catalogue.directory if catalogue else None
        self.scenes = {}
        self.history = History(state, self.publish)
        self.inserts = Inserts(state, self.history, self.publish, software_only)
        self.sessions = Sessions(state, self.history, self.publish,
                                 directory or Path.home() / "Documents/Universal Audio/Sessions",
                                 self.scenes, restore_values=restore_values)
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
                               factory_presets, catalogue)
        self.network = Network(state, helper, self.publish)
        self.inserts.refresh_paste()
        self.publish("/openapollo/PluginBackend", "software-model" if software_only else "unavailable", "string")
        for path in list(state._props):
            if re.fullmatch(r"/devices/\d+/inputs/\d+/Name", path):
                base = path.removesuffix("/Name")
                if state.get_prop_dict(base + "/ChannelHidden") is None:
                    self.publish(base + "/ChannelHidden", False)
            if re.fullmatch(r"/plugins/\d+/Name", path):
                self.plugin_defaults(path.removesuffix("/Name"))
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

    def plugin_defaults(self, base):
        """The footer state every /plugins/N entry carries for Console."""
        for key in ("Unison", "Demoing", "DemoAvailable"):
            if self.state.get_prop_dict(base + "/" + key) is None:
                self.publish(base + "/" + key, False)
        if self.state.get_prop_dict(base + "/DemoDaysRemaining") is None:
            self.publish(base + "/DemoDaysRemaining", 0, "int")

    def rescan_plugins(self):
        """UpdatePlugInStatus (Settings > Plug-Ins RESCAN): read the UA
        import again and lay it on the live tree, publishing the /plugins
        entries it adds (a newer `oa-assets import`) and the factory preset
        names.  Plug-ins already listed keep their index; licence state is
        never touched (ua_catalogue)."""
        cat = ua_catalogue.load(self.catalogue_dir)
        if cat is None:
            log.info("UpdatePlugInStatus: no UA catalogue imported; the "
                     "plug-in list stays as it is")
            return True
        before = set(self.state.all_paths())
        done = ua_catalogue.apply_to_state(self.state, cat, rescan=True)
        for path in sorted(set(self.state.all_paths()) - before):
            if re.fullmatch(r"/plugins/\d+/Name", path):
                self.plugin_defaults(path.removesuffix("/Name"))
        for path in sorted(set(self.state.all_paths()) - before):
            self.state._notify_subscribers(path + "/value", self.state.get_value(path))
        self.catalogue_dir = cat.directory
        self.presets.catalogue = ua_catalogue.factory_trees(cat, CHANNEL_STRIP)
        self.presets.refresh()
        log.info("UpdatePlugInStatus: %d new plug-ins from %s", done["plugins"], cat.directory)
        return True

    def publish(self, path, value, kind="bool"):
        meta = self.state.get_prop_dict(path)
        if path == "/Dirty" and meta is not None and meta.get("value") is value:
            return  # No redundant /Dirty pushes: a clean load stays silent.
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
        elif prop.endswith("/MixInSource") and value in ("default", "none") and \
                self.state.get_value(prop.removesuffix("/MixInSource") + "/IOType") == "Headphone":
            # A headphone output's MON / NONE: the hardware takes CUE 1 / CUE 2
            # only (other param 0x3f / 0x40, 0 / 1); the router would write
            # CUE 1 for either, so it is refused like CUE 3/4
            if not self.inserts.software_only:
                raise ValueError("headphone source encoding has not been verified")
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
                # With hardware a demo is the device's own record and only
                # UA's software starts one: nothing is sent, nothing changes
                # (the licence status shown is the device's, plugin_license)
                raise ValueError("demos are started in UA's own software; "
                                 "open-apollo never starts one")
            if not isinstance(plugin, str) or not re.fullmatch(r"/plugins/\d+", plugin) or not self.state.get_value(plugin + "/DemoAvailable"):
                raise ValueError("demo is unavailable")
            self.publish(plugin + "/Demoing", True)
            self.publish(plugin + "/DemoAvailable", False)
            self.publish(plugin + "/DemoDaysRemaining", 14, "int")
            return True  # A simulated footer state, never vendor authorization.
        if command == "UpdatePlugInStatus":
            return self.rescan_plugins()
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
