"""Host session files and modified-session restore. No DSP instance loader."""
from copy import deepcopy
from datetime import datetime
from pathlib import Path
import json
import os
import logging

import session_xml


class Sessions:
    def __init__(self, state, history, publish, directory, scenes=None):
        self.state, self.history, self.publish = state, history, publish
        self.directory = Path(directory).expanduser().resolve()
        self.original = None
        self.modified = {}
        self.scenes = scenes if scenes is not None else {}
        self.defaults = history.snapshot()
        self.refresh()
        last = self.directory / ".last-used.uadmix"
        if last.is_file():
            try:
                self.open(last)
                self.publish("/Session", self.original.get("openApolloSession", ""), "string")
            except (ValueError, OSError):
                logging.getLogger(__name__).warning("Last-used session cannot be restored")

    def path(self, name, extension=True):
        if not isinstance(name, str) or not name.strip():
            raise ValueError("session name/path is required")
        if "\\" in name or Path(name).is_absolute():
            raise ValueError("session names must be relative")
        if extension and not name.lower().endswith(".uadmix"):
            name += ".uadmix"
        if extension and (not Path(name).stem.strip() or Path(name).name.lower() == ".uadmix"):
            raise ValueError("empty session name")
        path = (self.directory / name).resolve()
        if path == self.directory or not path.is_relative_to(self.directory):
            raise ValueError("session path escapes the sessions directory")
        return path

    def refresh(self):
        def entries(folder):
            if not folder.exists():
                return []
            result = []
            for path in sorted(folder.iterdir()):
                if path.is_symlink() or path.name.startswith("."):
                    continue
                if path.is_dir():
                    result.append({"type": "folder", "value": path.name,
                                   "locked": False, "children": entries(path)})
                elif path.suffix.lower() == ".uadmix":
                    result.append({"type": "file", "value": path.stem, "locked": False})
            return result
        if self.state.get_prop_dict("/Session") is None:
            self.publish("/Session", "", "string")
        self.state.get_prop_dict("/Session")["values"] = entries(self.directory)
        self.publish("/ModifiedSessions", self.state.get_value("/ModifiedSessions") or "", "string")
        self.state.get_prop_dict("/ModifiedSessions")["values"] = [
            {"value": key, "name": value["name"], "time": value["time"]}
            for key, value in self.modified.items()]

    def retain_modified(self):
        if self.state.get_value("/Dirty"):
            key = str(len(self.modified) + 1)
            self.modified[key] = {"name": self.state.get_value("/Session") or "newSession",
                                  "time": datetime.now().strftime("%b %d, %Y %I:%M:%S %p"),
                                  "values": self.history.snapshot(),
                                  "scenes": deepcopy(self.scenes),
                                  "original": deepcopy(self.original)}

    def replace(self, values, name, original=None, scenes=None):
        self.retain_modified()
        self.publish("/Progress", "Loading session", "string")
        writable = self.history.snapshot()
        for path, value in values.items():
            meta = self.state.get_prop_dict(path)
            if meta and path in writable:
                self.state.set(path + "/value", value)
        self.original = original
        self.scenes.clear()
        self.scenes.update(scenes or {})
        self.publish("/Session", name, "string")
        self.publish("/Dirty", False)
        self.publish("/Progress", "", "string")
        self.refresh()

    def load(self, name):
        path = self.path(name)
        self.open(path)

    def open(self, path):
        if not isinstance(path, (str, Path)) or not str(path).strip():
            raise ValueError("OpenSession requires a path")
        path = Path(path).expanduser().resolve()
        if path.suffix.lower() != ".uadmix" or not path.is_file():
            raise ValueError("session file does not exist")
        with path.open("rb") as file:
            original = session_xml.parse(file.read(8 * 1024 * 1024 + 1))
        values = session_xml.read_values(original, self.state)
        scenes = {}
        writable = self.history.snapshot()
        for node in original.findall("open_apollo/scenes/scene"):
            index = int(node.get("index", "-1"))
            scene = json.loads(node.text or "{}")
            if not 0 <= index < 128 or not isinstance(scene, dict):
                raise ValueError("invalid saved scene")
            scenes[index] = {p: value for p, value in scene.items() if p in writable}
        # Validate the entire mapped subset before changing any properties.
        for prop, value in values.items():
            session_xml.decode_value(str(value), self.state.get_prop_dict(prop))
        name = path.relative_to(self.directory).with_suffix("").as_posix() if path.is_relative_to(self.directory) else str(path)
        self.replace(values, name, original, scenes)

    def xml_bytes(self):
        import xml.etree.ElementTree as ET
        data = session_xml.save_values(self.history.snapshot(), self.state, deepcopy(self.original))
        root = session_xml.parse(data)
        parent = ET.SubElement(root.find("open_apollo"), "scenes")
        for index, values in sorted(self.scenes.items()):
            ET.SubElement(parent, "scene", index=str(index)).text = json.dumps(values)
        return ET.tostring(root, encoding="utf-8", xml_declaration=True)

    def save(self, name=None):
        name = name if name is not None else self.state.get_value("/Session")
        if isinstance(name, str) and Path(name).is_absolute():
            # Saving an explicitly opened external file is allowed; SaveAs
            # always uses a relative name confined to the sessions directory.
            path = Path(name)
        else:
            path = self.path(name)
        path.parent.mkdir(parents=True, exist_ok=True)
        data = self.xml_bytes()
        self.publish("/Progress", "Saving session", "string")
        temporary = path.with_name(path.name + ".tmp")
        try:
            with temporary.open("wb") as file:
                file.write(data)
                file.flush()
                os.fsync(file.fileno())
            temporary.replace(path)
        finally:
            self.publish("/Progress", "", "string")
        self.original = session_xml.parse(data)
        current = path.relative_to(self.directory).with_suffix("").as_posix() if path.is_relative_to(self.directory) else str(path)
        self.publish("/Session", current, "string")
        self.publish("/Dirty", False)
        self.refresh()

    def post(self, command, args):
        if command == "NewSession":
            self.replace(self.defaults, "")
        elif command == "OpenSession":
            self.open(args.get("path", ""))
        elif command == "SaveSession":
            self.save()
        elif command == "SaveSessionAs":
            name = args.get("name", "")
            self.path(name)  # Reject an absolute SaveAs and empty names.
            self.save(name)
        elif command == "DeleteSession":
            path = self.path(args.get("name", ""))
            path.unlink()
            if self.state.get_value("/Session") == path.relative_to(self.directory).with_suffix("").as_posix():
                self.publish("/Session", "", "string")
                self.publish("/Dirty", True)
            self.refresh()
        elif command == "RenameSession":
            folder = self.path(args.get("origName", ""), extension=False)
            is_folder = folder.is_dir()
            old = folder if is_folder else self.path(args.get("origName", ""))
            new = self.path(args.get("newName", ""), extension=not is_folder)
            if new == old or (is_folder and new.is_relative_to(old)):
                raise ValueError("invalid session rename destination")
            if new.exists():
                raise ValueError("session already exists")
            new.parent.mkdir(parents=True, exist_ok=True)
            old.rename(new)
            old_name = old.relative_to(self.directory).as_posix() if is_folder else old.relative_to(self.directory).with_suffix("").as_posix()
            new_name = new.relative_to(self.directory).as_posix() if is_folder else new.relative_to(self.directory).with_suffix("").as_posix()
            current = self.state.get_value("/Session") or ""
            if current == old_name or (is_folder and current.startswith(old_name + "/")):
                self.publish("/Session", new_name + current[len(old_name):], "string")
            for retained in self.modified.values():
                name = retained["name"]
                if name == old_name or (is_folder and name.startswith(old_name + "/")):
                    retained["name"] = new_name + name[len(old_name):]
            self.refresh()
        elif command in ("NewFolder", "DeleteFolder"):
            if args.get("type") != "session":
                raise ValueError("session folder type is required")
            path = self.path(args.get("folder", ""), extension=False)
            path.mkdir(parents=True) if command == "NewFolder" else path.rmdir()
            self.refresh()
        elif command == "SaveLastUsedSession":
            self.directory.mkdir(parents=True, exist_ok=True)
            path = self.directory / ".last-used.uadmix"
            data = self.xml_bytes()
            root = session_xml.parse(data)
            root.set("openApolloSession", self.state.get_value("/Session") or "")
            import xml.etree.ElementTree as ET
            data = ET.tostring(root, encoding="utf-8", xml_declaration=True)
            temporary = path.with_suffix(".tmp")
            temporary.write_bytes(data)
            temporary.replace(path)
        else:
            return False
        return True

    def restore(self, key):
        kept = self.modified[str(key)]
        self.replace(kept["values"], kept["name"], deepcopy(kept["original"]), deepcopy(kept["scenes"]))
        self.publish("/ModifiedSessions", str(key), "string")
        self.publish("/Dirty", True)
