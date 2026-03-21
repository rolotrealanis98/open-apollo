"""
UA Mixer Helper tree — serves the 4720 DSP mixer session data.

The UA Mixer Helper (4720) has a completely different data model from
the UA Mixer Engine (4710). Instead of a flat device map with CamelCase
properties and {type, min, max, value} metadata, it uses:

  - Children as ARRAYS of {"path": "name", "properties": {...}, "children": [...]}
  - Properties with {value, read_only} format (not type/min/max)
  - snake_case property names
  - "supercore" device sessions identified by UUID
  - Commands array for executable actions

This module provides the HelperTree class that loads the captured
real tree data and handles GET/subscribe queries with the correct
params: levels, flatvalue, propfilter, propinfo, commands.
"""

import json
import logging
from collections import defaultdict
from pathlib import Path
from typing import Any, Callable

log = logging.getLogger(__name__)


class HelperTree:
    """In-memory tree for the UA Mixer Helper protocol (port 4720).

    Data format per node:
        {
            "properties": {"key": {"value": val, "read_only": true}, ...},
            "commands": [{"path": "cmd", "properties": {...}}, ...],
            "children": [{"path": "name", "properties": {...}, "children": [...]}, ...]
        }
    """

    def __init__(self):
        self.root: dict = {"properties": {}, "children": []}
        # Subscriptions: path → set of subscriber IDs
        self._subs: dict[str, set[str]] = defaultdict(set)
        # Callback registry: subscriber_id → callback
        self._callbacks: dict[str, Callable] = {}

    def load(self, path: str | Path):
        """Load the helper tree from a JSON file."""
        path = Path(path)
        log.info("Loading helper tree: %s", path)
        with open(path) as f:
            self.root = json.load(f)
        # Count nodes
        count = self._count_nodes(self.root)
        log.info("Loaded helper tree: %d nodes", count)

    def _count_nodes(self, node: dict) -> int:
        count = 1
        for child in node.get("children", []):
            if isinstance(child, dict):
                count += self._count_nodes(child)
        return count

    # ── Path Resolution ───────────────────────────────────────────────

    def _resolve(self, path: str) -> dict | None:
        """Resolve a path like /devices/<uuid>/buses to a tree node.

        Also resolves through commands: /devices/<uuid>/undo resolves to the
        "undo" command's properties dict (which contains {properties, parameters}).
        """
        path = path.strip("/")
        if not path:
            return self.root

        parts = path.split("/")
        node = self.root
        for i, part in enumerate(parts):
            # Look in children array first
            children = node.get("children", [])
            found = False
            for child in children:
                if isinstance(child, dict) and child.get("path") == part:
                    node = child
                    found = True
                    break
            if not found:
                # Look in commands array as fallback
                commands = node.get("commands", [])
                for cmd in commands:
                    if isinstance(cmd, dict) and cmd.get("path") == part:
                        # Command found — its "properties" dict acts as a node
                        cmd_props = cmd.get("properties", {})
                        # Remaining parts resolve within the command's properties
                        remaining = parts[i + 1:]
                        if not remaining:
                            # Return the command as a pseudo-node
                            return cmd_props
                        # Resolve property sub-paths (e.g. "enabled")
                        return self._resolve_property_path(cmd_props, remaining)
                return None
        return node

    def _resolve_property_path(self, node: dict, parts: list[str]) -> dict | None:
        """Resolve a sub-path within a node's properties.

        Used for command property access like undo/enabled, where 'enabled'
        is inside the command's properties.properties dict.
        """
        props = node.get("properties", {})
        if len(parts) == 1 and parts[0] in props:
            # Return a pseudo-node wrapping the property value
            prop_val = props[parts[0]]
            if isinstance(prop_val, dict):
                return {"properties": {parts[0]: prop_val}}
            return {"properties": {parts[0]: {"value": prop_val}}}
        return None

    def path_exists(self, path: str) -> bool:
        return self._resolve(path) is not None

    # ── GET ────────────────────────────────────────────────────────────

    def get(self, path: str, levels: int = 0, flatvalue: bool = False,
            propfilter: list[str] | None = None, propinfo: bool = True,
            commands: bool = True,
            excluded_children: list[str] | None = None) -> dict | None:
        """Handle a GET request with query params.

        Args:
            path: Tree path like /devices or /devices/<uuid>/buses
            levels: How many levels deep to return (0 = just this node's properties)
            flatvalue: If True, properties are {name: value} instead of {name: {value: val}}
            propfilter: If set, only include these property names
            propinfo: If True, include property metadata (read_only etc.)
            commands: If True, include commands array
            excluded_children: If set, skip children with these path names
        """
        node = self._resolve(path)
        if node is None:
            return None

        return self._serialize(node, levels, flatvalue, propfilter, propinfo,
                               commands, current_depth=0, is_root=True,
                               excluded_children=excluded_children)

    def _serialize(self, node: dict, levels: int, flatvalue: bool,
                   propfilter: list[str] | None, propinfo: bool,
                   commands_flag: bool, current_depth: int,
                   is_root: bool = False,
                   excluded_children: list[str] | None = None) -> dict:
        """Serialize a node according to query params."""
        result = {}

        # Properties
        props = node.get("properties", {})
        if props:
            if flatvalue:
                # Flatten: {"name": "supercore"} instead of {"name": {"value": "supercore"}}
                flat = {}
                for k, v in props.items():
                    if isinstance(v, dict):
                        flat[k] = v.get("value")
                    else:
                        flat[k] = v
                if propfilter:
                    flat = {k: v for k, v in flat.items() if k in propfilter}
                if flat:
                    result["properties"] = flat
            else:
                if propfilter:
                    filtered = {k: v for k, v in props.items() if k in propfilter}
                    if not propinfo:
                        # Strip metadata, keep just value
                        filtered = {k: {"value": v.get("value") if isinstance(v, dict) else v}
                                    for k, v in filtered.items()}
                    if filtered:
                        result["properties"] = filtered
                else:
                    if not propinfo:
                        result["properties"] = {
                            k: {"value": v.get("value") if isinstance(v, dict) else v}
                            for k, v in props.items()
                        }
                    else:
                        result["properties"] = props

        # Commands
        if commands_flag:
            cmds = node.get("commands", [])
            if cmds:
                result["commands"] = cmds

        # Children (based on levels)
        children = node.get("children", [])
        if children and current_depth < levels:
            serialized = []
            for child in children:
                if isinstance(child, dict):
                    child_path = child.get("path", "")
                    # Skip excluded children
                    if excluded_children and child_path in excluded_children:
                        continue
                    entry = {"path": child_path}
                    # Recurse into child
                    child_data = self._serialize(
                        child, levels, flatvalue, propfilter, propinfo,
                        commands_flag, current_depth + 1,
                        excluded_children=excluded_children)
                    if "properties" in child_data:
                        entry["properties"] = child_data["properties"]
                    if "commands" in child_data:
                        entry["commands"] = child_data["commands"]
                    if "children" in child_data:
                        entry["children"] = child_data["children"]
                    serialized.append(entry)
                else:
                    serialized.append(child)
            result["children"] = serialized

        return result

    # ── Property Access ────────────────────────────────────────────────

    def get_value(self, path: str) -> Any:
        """Get the value of a specific property path.

        For paths like /devices/<uuid>/online, returns the value.
        For /initialized (root property), checks root node's properties.
        For container paths, returns None.
        """
        stripped = path.strip("/")

        # Also check /path/value pattern
        if stripped.endswith("/value"):
            return self.get_value("/" + stripped[:-6])

        # Try as a property on the parent node
        parts = stripped.rsplit("/", 1)
        if len(parts) == 2:
            parent_path, prop_name = parts
            parent = self._resolve(parent_path)
            if parent:
                props = parent.get("properties", {})
                if prop_name in props:
                    meta = props[prop_name]
                    if isinstance(meta, dict):
                        return meta.get("value")
                    return meta

        # Single-part path (e.g. "initialized") — check root properties
        if len(parts) == 1:
            prop_name = parts[0]
            if prop_name:
                props = self.root.get("properties", {})
                if prop_name in props:
                    meta = props[prop_name]
                    if isinstance(meta, dict):
                        return meta.get("value")
                    return meta

        # Container node — check for "value" property
        node = self._resolve(path)
        if node is not None:
            props = node.get("properties", {})
            if "value" in props:
                meta = props["value"]
                return meta.get("value") if isinstance(meta, dict) else meta
        return None

    def set_value(self, path: str, value: Any) -> str:
        """Set a property value.

        Returns:
            "ok" if the property was set successfully.
            "read_only" if the property exists but is read-only.
            "not_found" if the property does not exist.
        """
        stripped = path.strip("/")

        # Handle /foo/bar/value → try property "bar" on node "foo"
        if stripped.endswith("/value"):
            result = self.set_value("/" + stripped[:-6], value)
            if result != "not_found":
                return result

        parts = stripped.rsplit("/", 1)
        if len(parts) == 2:
            parent_path, prop_name = parts
            parent = self._resolve(parent_path)
            if parent:
                props = parent.get("properties", {})
                if prop_name in props:
                    meta = props[prop_name]
                    if isinstance(meta, dict) and meta.get("read_only"):
                        return "read_only"
                    if isinstance(meta, dict):
                        meta["value"] = value
                    else:
                        props[prop_name] = {"value": value}
                    self._notify(path, value)
                    return "ok"
        return "not_found"

    # ── Subscriptions ──────────────────────────────────────────────────

    def subscribe(self, sub_id: str, path: str):
        self._subs[path].add(sub_id)

    def unsubscribe(self, sub_id: str, path: str):
        if path in self._subs:
            self._subs[path].discard(sub_id)

    def register_callback(self, sub_id: str, callback: Callable):
        self._callbacks[sub_id] = callback

    def unregister_callback(self, sub_id: str):
        self._callbacks.pop(sub_id, None)
        # Remove from all subscription lists
        for subs in self._subs.values():
            subs.discard(sub_id)

    def _notify(self, path: str, value: Any):
        """Notify subscribers of a value change."""
        # Check exact path and ancestor paths
        for sub_path, sub_ids in self._subs.items():
            if path == sub_path or path.startswith(sub_path + "/"):
                for sub_id in sub_ids:
                    cb = self._callbacks.get(sub_id)
                    if cb:
                        cb(path, value)
