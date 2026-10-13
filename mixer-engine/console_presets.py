"""Console preset commands and honestly labelled daemon-owned snapshots.

Native factory .fxp files use opaque FPCh chunks (presets-decomp/1000dee38.c).
They require a plugin backend; never pretend their bytes are scalar settings.
Native channel_strip XML supports a scalar-only subset (100056370.c).
Daemon snapshots use .oapreset, not a counterfeit .fxp/.uadchannel format.

Factory menus: a --factory-presets-dir folder when it holds the plug-in's
presets, else the names the user's UA catalogue import lists
(ua_catalogue.factory_trees); open-apollo ships none, so without either the
menus hold the user's own presets only.
"""
from copy import deepcopy
from pathlib import Path
from urllib.parse import quote
import json
import xml.etree.ElementTree as ET

import session_xml
import ua_catalogue

EXTENSIONS = (".oapreset", ".uadchannel", ".fxp")
CHANNEL_STRIP = "Channel Strip"


def listed(tree, name):
    """Whether `name` ("Folder/Preset") is a file entry of a preset tree."""
    entries, parts = tree or [], name.split("/")
    for i, part in enumerate(parts):
        last = i == len(parts) - 1
        entry = next((e for e in entries if e.get("value") == part
                      and (e.get("type") == "folder") != last), None)
        if entry is None:
            return False
        entries = entry.get("children") or []
    return True


class Presets:
    def __init__(self, state, history, publish, inserts, directory, factory=None,
                 catalogue=None):
        self.state, self.history, self.publish, self.inserts = state, history, publish, inserts
        self.directory = Path(directory).expanduser().resolve()
        self.factory = Path(factory).expanduser().resolve() if factory else None
        # Factory preset names from the user's UA import, by plug-in name
        self.catalogue = ua_catalogue.factory_trees(catalogue, CHANNEL_STRIP)
        self.refresh()

    def plugin(self, base):
        node, prop = self.state._resolve_path(base)
        if node is None or prop is not None:
            raise ValueError("unknown preset object")
        name = self.state.get_value(base + "/EffectName")
        if name is not None:
            if not name:
                raise ValueError("empty insert has no preset")
            return name
        if base.startswith("/plugins/"):
            return self.state.get_value(base + "/Name")
        if "/inputs/" in base or "/auxs/" in base:
            return CHANNEL_STRIP
        raise ValueError("unsupported preset object")

    def folder(self, plugin, factory=False):
        if not isinstance(plugin, str) or not plugin.strip() or plugin in (".", ".."):
            raise ValueError("plugin name is required")
        root = self.factory if factory else self.directory
        if root is None:
            raise ValueError("factory preset directory unavailable")
        # Factory installations use literal names; own folders encode separators.
        return root / (plugin if factory and "/" not in plugin else quote(plugin, safe=""))

    def path(self, plugin, name, extension=".oapreset", factory=False):
        if not isinstance(name, str) or not name.strip() or "\\" in name or Path(name).is_absolute():
            raise ValueError("relative preset name is required")
        folder = self.folder(plugin, factory)
        raw = folder / name
        if any(p.is_symlink() for p in [raw, *raw.parents] if p != self.directory.parent):
            raise ValueError("preset symlinks are unsupported")
        path = raw if extension is None or raw.suffix in EXTENSIONS else raw.with_name(raw.name + extension)
        path = path.resolve()
        if path == folder.resolve() or not path.is_relative_to(folder.resolve()):
            raise ValueError("preset path escapes its library")
        return path

    def entries(self, folder, locked):
        if not folder.is_dir() or folder.is_symlink():
            return []
        result = []
        for path in sorted(folder.iterdir()):
            if path.is_symlink() or path.name.startswith("."):
                continue
            if path.is_dir():
                result.append({"type": "folder", "value": path.name, "locked": locked,
                               "children": self.entries(path, locked)})
            elif path.suffix in EXTENSIONS:
                result.append({"type": "file", "value": path.stem, "locked": locked,
                               "format": path.suffix[1:]})
        return result

    def refresh(self):
        objects = set()
        for path in list(self.state._props):
            if path.endswith("/EffectName"):
                if self.state.get_value(path):
                    objects.add(path.removesuffix("/EffectName"))
            elif path.endswith("/Preset") or (path.startswith("/plugins/") and path.endswith("/Name")):
                objects.add(path.rsplit("/", 1)[0])
        for base in sorted(objects):
            try:
                plugin = self.plugin(base)
            except ValueError:
                continue
            path = base + "/Preset"
            self.publish(path, self.state.get_value(path) or "", "string")
            meta = self.state.get_prop_dict(path)
            meta["plugin"] = plugin
            values = self.entries(self.folder(plugin), False)
            factory = self.entries(self.folder(plugin, True), True) if self.factory else []
            meta["values"] = values + (factory or deepcopy(self.catalogue.get(plugin, [])))

    def load(self, base, name):
        plugin = self.plugin(base)
        file = None
        for factory in ([False, True] if self.factory else [False]):
            for extension in EXTENSIONS:
                candidate = self.path(plugin, name, extension, factory)
                if candidate.is_file():
                    file = candidate
                    break
            if file:
                break
        if file is None:
            if listed(self.catalogue.get(plugin), name):
                raise ValueError("factory preset file unavailable: the UA import lists "
                                 "its name only (--factory-presets-dir)")
            raise ValueError("preset does not exist")
        if file.stat().st_size > 8 * 1024 * 1024:
            raise ValueError("preset exceeds size limit")
        if file.suffix == ".fxp":
            raise ValueError("native FXP restoration requires the DSP plugin backend")
        if file.suffix == ".uadchannel":
            self.load_channel(base, file.read_bytes())
        else:
            value = json.loads(file.read_text())
            if value.get("format") != "open-apollo-preset/1" or value.get("plugin") != plugin:
                raise ValueError("preset format or plugin differs")
            node = value.get("node")
            if not isinstance(node, dict):
                raise ValueError("invalid preset node")
            self.state.validate_node(node)
            if "/effects/" in base:
                self.inserts.apply({base: node})
            elif self.inserts.software_only:
                before = deepcopy(self.state.get(base, recursive=True))
                self.state.replace_node(base, node)
                self.history.append({"@node:" + base: (before, deepcopy(node))})
                self.publish("/Dirty", True)
            else:
                raise ValueError("whole channel snapshots require the software backend")
        self.publish(base + "/Preset", name, "string")
        self.refresh()

    def load_channel(self, base, data):
        # DTD guard and bounded parse reused from the session mapper.
        text = data.decode("utf-8-sig")
        if "<!DOCTYPE" in text.upper() or "<!ENTITY" in text.upper():
            raise ValueError("DTD/entity declarations are unsupported")
        try:
            root = ET.fromstring(text)
        except ET.ParseError as error:
            raise ValueError("malformed channel preset") from error
        if root.tag != "channel_strip" or root.get("version") != "1":
            raise ValueError("unsupported channel preset")
        if any((chunk.text or "").strip() for chunk in root.iter("plugin_state")):
            raise ValueError("channel preset contains unsupported plugin state")
        source = root.find("mixer_object")
        if source is None or source.get("type") != "kInput":
            raise ValueError("channel preset has no input object")
        values = {}
        for property in source.findall("property"):
            path = session_xml.property_path(base, property.get("id", ""), self.state)
            if path:
                values[path] = session_xml.decode_value(property.text or "", self.state.get_prop_dict(path))
        before = self.history.snapshot()
        for path, value in values.items():
            self.state.set(path + "/value", value)
        self.history.record("preset", before)

    def post(self, command, args):
        if command == "NewFolder" and "path" in args:
            target = args["path"]
            if not isinstance(target, str) or not target.endswith("/Preset"):
                raise ValueError("preset folder requires an object/Preset path")
            plugin = self.plugin(target.removesuffix("/Preset"))
            self.path(plugin, args.get("folder", ""), extension=None).mkdir(parents=True)
        elif command == "SaveAsPreset":
            base = args.get("path", "")
            plugin = self.plugin(base)
            if args.get("pluginName") != plugin:
                raise ValueError("preset pluginName differs from object")
            file = self.path(plugin, args.get("presetName", ""))
            if file.suffix != ".oapreset":
                raise ValueError("daemon snapshots use .oapreset")
            value = {"format": "open-apollo-preset/1", "plugin": plugin,
                     "node": deepcopy(self.state.get(base, recursive=True))}
            file.parent.mkdir(parents=True, exist_ok=True)
            temporary = file.with_suffix(".tmp")
            temporary.write_text(json.dumps(value))
            temporary.replace(file)
            self.publish(base + "/Preset", args["presetName"], "string")
        elif command in ("MovePreset", "DeletePreset"):
            plugin = args.get("pluginName", "")
            folder = args.get("isFolder", False)
            if type(folder) is not bool:
                raise ValueError("isFolder must be boolean")
            name = args.get("origPresetName") if command == "MovePreset" else args.get("presetName")
            old = self.path(plugin, name, extension=None if folder else ".oapreset")
            if command == "DeletePreset":
                old.rmdir() if folder else old.unlink()
            else:
                new = self.path(plugin, args.get("newPresetName", ""), extension=None if folder else ".oapreset")
                if new.exists() or new.is_relative_to(old):
                    raise ValueError("invalid preset rename destination")
                new.parent.mkdir(parents=True, exist_ok=True)
                old.rename(new)
        else:
            return False
        self.refresh()
        return True
