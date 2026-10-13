"""Console 1.3.1 insert clipboard shapes (engine-model/src/inserts.rs).

The software-only daemon models slot trees. Hardware mutation is refused
until a genuine DSP allocation/upload/retirement backend exists.
"""
from copy import deepcopy
import re

SLOT = re.compile(r"/devices/\d+/(?:inputs|auxs)/\d+/(?:preamps/0/)?effects/\d+$")


class Inserts:
    def __init__(self, state, history, publish, software_only=False):
        self.state, self.history, self.publish = state, history, publish
        self.software_only = software_only
        self.clipboard = None
        self.settings = None

    def slot(self, path):
        if not isinstance(path, str) or not SLOT.fullmatch(path):
            raise ValueError("invalid insert path")
        node, prop = self.state._resolve_path(path)
        if node is None or prop is not None or "EffectName" not in node.get("properties", {}):
            raise ValueError("insert does not exist")
        return node

    def validate(self, path, data):
        self.slot(path)
        if not self.software_only:
            raise ValueError("DSP plugin backend unavailable; use --no-hardware for software slot tests")
        name = data.get("properties", {}).get("EffectName", {}).get("value", "")
        if "/preamps/" in path and name and self.unison(name) is False:
            raise ValueError("plugin is not Unison-capable")

    def unison(self, name):
        """Whether `name` loads in a Unison preamp slot: the catalogue's
        /plugins/N/Unison (the user's UA import, its Unison flags from the
        shipped list of plug-in ids), False for a plug-in it does not list.
        None when there is no catalogue at all (nothing imported): the
        daemon cannot tell, and like the console's paste check
        (engine-model inserts.rs can_paste) it does not refuse what it
        cannot judge."""
        entries = [p.removesuffix("/Name") for p in self.state._props
                   if re.fullmatch(r"/plugins/\d+/Name", p)]
        if not entries:
            return None
        entry = next((e for e in entries if self.state.get_value(e + "/Name") == name), None)
        return entry is not None and self.state.get_value(entry + "/Unison") is True

    def apply(self, replacements):
        # Validate every target before making any change; one undo step.
        old = {path: deepcopy(self.slot(path)) for path in replacements}
        for path, data in replacements.items():
            self.validate(path, data)
            self.state.validate_node(data)
        for path, data in replacements.items():
            self.state.replace_node(path, data)
        self.history.append({"@node:" + path: (old[path], deepcopy(data))
                             for path, data in replacements.items()})
        self.publish("/Dirty", True)
        self.refresh_paste()

    def refresh_paste(self):
        for path, meta in list(self.state._props.items()):
            if path.endswith("/EffectName") and SLOT.fullmatch(path.removesuffix("/EffectName")):
                can = self.settings is not None and meta.get("value") == self.settings[0]
                self.publish(path.removesuffix("EffectName") + "CanPastePluginBarSettings", can)

    def assign(self, path, name):
        if not isinstance(name, str):
            raise ValueError("EffectName must be a string")
        data = deepcopy(self.slot(path))
        data["properties"]["EffectName"]["value"] = name
        if not name or name != self.state.get_value(path + "/EffectName"):
            data.get("children", {}).pop("parameters", None)
        self.apply({path: data})

    def post(self, command, args):
        if command in ("CopyInsert", "PluginBarCopySettings"):
            path = args.get("effectPath")
            data = deepcopy(self.slot(path))
            name = data["properties"]["EffectName"].get("value")
            if not name:
                raise ValueError("cannot copy an empty insert")
            if command == "CopyInsert":
                self.clipboard = data
            else:
                self.settings = (name, deepcopy(data.get("children", {}).get("parameters", {})))
                self.refresh_paste()
        elif command == "PasteInsert":
            paths = args.get("effectPaths")
            if self.clipboard is None or not isinstance(paths, list) or not paths or len(paths) > 128 or any(not isinstance(p, str) for p in paths):
                raise ValueError("clipboard and destination list are required")
            self.apply({p: deepcopy(self.clipboard) for p in paths})
        elif command == "MoveInsert":
            src, dst, copy = args.get("srcPath"), args.get("dstPath"), args.get("copy")
            if type(copy) is not bool or src == dst:
                raise ValueError("distinct slots and a boolean copy are required")
            data = deepcopy(self.slot(src))
            if not data["properties"]["EffectName"].get("value"):
                raise ValueError("source insert is empty")
            replacements = {dst: data}
            if not copy:
                empty = deepcopy(data)
                empty["properties"]["EffectName"]["value"] = ""
                empty.get("children", {}).pop("parameters", None)
                replacements[src] = empty
            self.apply(replacements)
        elif command == "PluginBarPasteSettings":
            path = args.get("effectPath")
            data = deepcopy(self.slot(path))
            if self.settings is None or data["properties"]["EffectName"].get("value") != self.settings[0]:
                raise ValueError("settings clipboard plugin differs")
            data.setdefault("children", {})["parameters"] = deepcopy(self.settings[1])
            self.apply({path: data})
        else:
            return False
        return True
