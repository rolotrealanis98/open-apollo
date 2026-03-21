"""
State tree: load device map JSON, path-based get/set, subscription tracking.

The device map from ConsoleLink contains a flat list of 11,244 controls:

    {"path": "/devices/0/inputs/0/preamps/0/Gain",
     "type": "float", "min": -144.0, "max": 65.0, "value": 10.0}

We reconstruct a nested tree that mirrors the real UA Mixer Engine's
response format:

    {"properties": {"Gain": {"type": "float", "min": -144, ...}},
     "children":   {"0": {...nested...}}}

Path convention:
    /devices/0/inputs/0/preamps/0/Gain       → property node
    /devices/0/inputs/0/preamps/0/Gain/value  → the "value" field of that property

The real mixer engine treats "Gain" as a property with metadata (type, min, max)
and "Gain/value" as its current value. We replicate this.
"""

import json
import logging
import threading
import time
from collections import defaultdict
from pathlib import Path
from typing import Any, Callable

log = logging.getLogger(__name__)


class StateTree:
    """In-memory control tree with path-based access and subscriptions."""

    def __init__(self):
        # The nested tree: {"properties": {...}, "children": {...}}
        self.root: dict = {"properties": {}, "children": {}}
        # Flat index: path → property dict (for fast lookup)
        self._props: dict[str, dict] = {}
        # Subscriptions: path → set of callback IDs
        self._subs: dict[str, set[str]] = defaultdict(set)
        # Callback registry: client_id → callback function
        self._callbacks: dict[str, Callable] = {}
        # Hardware write hook: called when a SET changes a value
        # signature: on_set(path: str, value: Any) → None
        self.on_set: Callable | None = None
        # Device map metadata
        self.device_name: str = ""
        self.control_count: int = 0
        # Persistence: track modified values, debounced save
        self._dirty: dict[str, Any] = {}  # path → value (only modified controls)
        self._save_path: Path | None = None
        self._save_timer: threading.Timer | None = None
        self._save_delay = 2.0  # seconds

    # ── Loading ──────────────────────────────────────────────────────

    def load_device_map(self, path: str | Path):
        """Load a device map JSON and reconstruct the nested tree."""
        path = Path(path)
        log.info("Loading device map: %s", path)
        with open(path) as f:
            data = json.load(f)

        self.device_name = data.get("device_name", "Unknown")
        controls = data.get("controls", [])
        self.control_count = len(controls)

        # Build nested tree from flat controls
        for ctrl in controls:
            ctrl_path = ctrl["path"]
            self._insert_control(ctrl_path, ctrl)

        log.info("Loaded %d controls for %s", self.control_count, self.device_name)

    def add_runtime_property(self, ctrl_path: str, ctrl: dict):
        """Add a runtime-only property not in the device map.

        Used for paths the real mixer engine provides dynamically
        (e.g. /initialized, /ping) that aren't in captured device maps.
        """
        self._insert_control(ctrl_path, ctrl)
        self._props[ctrl_path] = self.get_prop_dict(ctrl_path)

    def update_property(self, ctrl_path: str, overrides: dict):
        """Update an existing property with override values.

        Only updates fields that have non-None values in overrides,
        preserving existing fields not mentioned in overrides.
        """
        prop = self.get_prop_dict(ctrl_path)
        if prop is None:
            return
        for key in ("type", "value", "min", "max", "values", "readonly"):
            if overrides.get(key) is not None:
                prop[key] = overrides[key]

    def get_prop_dict(self, path: str) -> dict | None:
        """Get the raw property dict for a path (for direct mutation)."""
        resolved, prop_name = self._resolve_path(path)
        if prop_name is not None:
            return resolved
        return None

    def enable_persistence(self, save_path: str | Path):
        """Enable persistent state: save modified values to disk, load on startup.

        Only stores values that differ from the device map defaults.
        The save file is a small JSON dict: {path: value, ...}
        """
        self._save_path = Path(save_path)
        if self._save_path.exists():
            self._load_saved_state()

    def _load_saved_state(self):
        """Load previously saved state and overlay onto current tree."""
        try:
            with open(self._save_path) as f:
                saved = json.load(f)
            count = 0
            for path, value in saved.items():
                resolved, prop_name = self._resolve_path(path)
                if resolved is not None and prop_name is not None:
                    resolved["value"] = value
                    self._dirty[path] = value
                    count += 1
            log.info("Restored %d saved values from %s", count, self._save_path)
        except (json.JSONDecodeError, OSError) as e:
            log.warning("Failed to load saved state from %s: %s", self._save_path, e)

    def _schedule_save(self):
        """Schedule a debounced save (2s after last change)."""
        if self._save_path is None:
            return
        if self._save_timer is not None:
            self._save_timer.cancel()
        self._save_timer = threading.Timer(self._save_delay, self._save_state)
        self._save_timer.daemon = True
        self._save_timer.start()

    def _save_state(self):
        """Write modified values to disk."""
        if self._save_path is None or not self._dirty:
            return
        try:
            self._save_path.parent.mkdir(parents=True, exist_ok=True)
            tmp = self._save_path.with_suffix(".tmp")
            with open(tmp, "w") as f:
                json.dump(self._dirty, f, separators=(",", ":"))
            tmp.rename(self._save_path)
            log.debug("Saved %d modified values to %s", len(self._dirty), self._save_path)
        except OSError as e:
            log.error("Failed to save state to %s: %s", self._save_path, e)

    def save_now(self):
        """Force an immediate save (e.g. on shutdown)."""
        if self._save_timer is not None:
            self._save_timer.cancel()
            self._save_timer = None
        self._save_state()

    def _insert_control(self, ctrl_path: str, ctrl: dict):
        """Insert a control into the nested tree."""
        parts = [p for p in ctrl_path.strip("/").split("/") if p]
        if not parts:
            return

        # The last component is the property name, everything before is children
        prop_name = parts[-1]
        container_parts = parts[:-1]

        # Navigate/create children to reach the container
        node = self.root
        for part in container_parts:
            children = node.setdefault("children", {})
            if part not in children:
                children[part] = {"properties": {}, "children": {}}
            node = children[part]

        # Insert as a property
        prop_data = {}
        if ctrl.get("type") is not None:
            prop_data["type"] = ctrl["type"]
        if ctrl.get("min") is not None:
            prop_data["min"] = ctrl["min"]
        if ctrl.get("max") is not None:
            prop_data["max"] = ctrl["max"]
        if ctrl.get("values") is not None:
            prop_data["values"] = ctrl["values"]
        if "value" in ctrl:
            prop_data["value"] = ctrl["value"]
        if ctrl.get("readonly"):
            prop_data["readonly"] = True

        props = node.setdefault("properties", {})
        props[prop_name] = prop_data

        # Index for fast lookup
        self._props[ctrl_path] = prop_data

    # ── Path Traversal ───────────────────────────────────────────────

    def _resolve_path(self, path: str) -> tuple[dict | None, str | None]:
        """Resolve a path to (node, None) for a child or (prop_dict, prop_name) for a property.

        Returns (None, None) if path not found.
        """
        if path == "/":
            return self.root, None

        parts = [p for p in path.strip("/").split("/") if p]
        if not parts:
            return self.root, None

        # Try resolving as a child path first
        node = self.root
        for i, part in enumerate(parts):
            children = node.get("children", {})
            if part in children:
                node = children[part]
            else:
                # Maybe this part is a property name
                props = node.get("properties", {})
                if part in props:
                    # Remaining parts after property name
                    remaining = parts[i + 1:]
                    if not remaining:
                        # Path points to the property itself
                        return props[part], part
                    elif remaining == ["value"]:
                        # Path points to /prop/value
                        return props[part], part
                    elif remaining == ["values"]:
                        # Path points to /prop/values (enum list)
                        return props[part], part
                    else:
                        return None, None
                return None, None

        return node, None

    # ── GET ───────────────────────────────────────────────────────────

    def get(self, path: str, recursive: bool = False,
            propfilter: list[str] | None = None) -> dict | None:
        """Handle a GET command. Returns the response data dict, or None."""
        resolved, prop_name = self._resolve_path(path)
        if resolved is None:
            return None

        # If it resolved to a property
        if prop_name is not None:
            # Check if path ends with /value
            if path.rstrip("/").endswith("/value"):
                return resolved.get("value")
            # Check if path ends with /values (enum list for dropdowns)
            if path.rstrip("/").endswith("/values"):
                vals = resolved.get("values")
                return vals if vals is not None else []
            # Return the property metadata
            return self._filter_props_dict(resolved, propfilter)

        # It's a node (children container)
        if recursive:
            return self._serialize_node(resolved, propfilter)
        else:
            return self._serialize_node_shallow(resolved, propfilter)

    def _serialize_node(self, node: dict,
                        propfilter: list[str] | None) -> dict:
        """Serialize a node recursively (for recursive GET)."""
        result = {}
        props = node.get("properties", {})
        if props:
            if propfilter:
                # propfilter selects which properties to include BY NAME
                pf_set = set(propfilter)
                filtered = {name: meta for name, meta in props.items()
                            if name in pf_set}
                if filtered:
                    result["properties"] = filtered
            else:
                result["properties"] = props

        children = node.get("children", {})
        if children:
            serialized_children = {}
            for name, child in children.items():
                serialized_children[name] = self._serialize_node(child, propfilter)
            result["children"] = serialized_children

        return result

    def _serialize_node_shallow(self, node: dict,
                                propfilter: list[str] | None) -> dict:
        """Serialize a node non-recursively (shallow GET)."""
        result = {}
        props = node.get("properties", {})
        if props:
            if propfilter:
                # propfilter selects which properties to include BY NAME
                pf_set = set(propfilter)
                filtered = {name: meta for name, meta in props.items()
                            if name in pf_set}
                if filtered:
                    result["properties"] = filtered
            else:
                result["properties"] = props

        # For non-recursive, just list children names (empty dicts)
        children = node.get("children", {})
        if children:
            result["children"] = {name: {} for name in children}

        return result

    def _filter_props_dict(self, meta: dict,
                           propfilter: list[str] | None) -> dict:
        """Filter a property metadata dict by propfilter list."""
        if not propfilter:
            return meta
        # Map lowercase filter names to actual keys
        # Common filters: Name, Value, value, type, min, max, values
        filtered = {}
        for key, val in meta.items():
            if key in propfilter or key.lower() in [p.lower() for p in propfilter]:
                filtered[key] = val
        return filtered

    # ── SET ───────────────────────────────────────────────────────────

    def set(self, path: str, value: Any, source_client: str | None = None) -> bool:
        """Handle a SET command. Returns True if value was changed.

        Args:
            path: Control path to set
            value: New value
            source_client: Client ID that initiated the SET (excluded from
                           subscription notifications to prevent echo)
        """
        # Special paths
        if path == "/Sleep":
            return True  # Keepalive, always accept
        if path == "/Dirty/value":
            return True

        resolved, prop_name = self._resolve_path(path)
        if resolved is None:
            log.warning("SET: path not found: %s", path)
            return False

        if prop_name is None:
            log.warning("SET: path is a container, not a value: %s", path)
            return False

        # Convert value to match the property type
        prop_type = resolved.get("type")
        value = self._coerce_value(value, prop_type, resolved)

        old_value = resolved.get("value")
        resolved["value"] = value

        if old_value != value:
            log.debug("SET %s: %r → %r", path, old_value, value)
            # Track modified value for persistence
            self._dirty[path] = value
            self._schedule_save()
            # Notify hardware backend
            if self.on_set:
                self.on_set(path, value)
            # Notify subscribers (exclude originator to prevent echo)
            self._notify_subscribers(path, value, exclude=source_client)

        return True

    def set_value(self, path: str, value: Any, source_client: str | None = None) -> bool:
        """Set a value directly (convenience alias for set())."""
        return self.set(path, value, source_client=source_client)

    def _coerce_value(self, value: Any, prop_type: str | None,
                      meta: dict) -> Any:
        """Coerce a value to the property's declared type."""
        if prop_type == "bool":
            if isinstance(value, str):
                return value.lower() in ("true", "1")
            return bool(value)
        elif prop_type == "float":
            try:
                v = float(value)
                if meta.get("min") is not None:
                    v = max(v, meta["min"])
                if meta.get("max") is not None:
                    v = min(v, meta["max"])
                return v
            except (ValueError, TypeError):
                return value
        elif prop_type == "int" or prop_type == "int64":
            try:
                v = int(value)
                if meta.get("min") is not None:
                    v = max(v, int(meta["min"]))
                if meta.get("max") is not None:
                    v = min(v, int(meta["max"]))
                return v
            except (ValueError, TypeError):
                return value
        elif prop_type == "string":
            value = str(value)
            allowed = meta.get("values")
            if allowed and value not in allowed:
                log.warning("SET: value %r not in allowed values %r", value, allowed)
            return value
        return value

    # ── SUBSCRIBE / UNSUBSCRIBE ──────────────────────────────────────

    def subscribe(self, client_id: str, path: str):
        """Subscribe a client to value changes on a path."""
        self._subs[path].add(client_id)
        log.debug("SUBSCRIBE %s → %s", client_id, path)

    def unsubscribe(self, client_id: str, path: str):
        """Unsubscribe a client from a path."""
        if path in self._subs:
            self._subs[path].discard(client_id)
            if not self._subs[path]:
                del self._subs[path]

    def unsubscribe_all(self, client_id: str):
        """Remove all subscriptions for a client (on disconnect)."""
        empty = []
        for path, clients in self._subs.items():
            clients.discard(client_id)
            if not clients:
                empty.append(path)
        for path in empty:
            del self._subs[path]

    def register_callback(self, client_id: str, callback: Callable):
        """Register a notification callback for a client."""
        self._callbacks[client_id] = callback

    def unregister_callback(self, client_id: str):
        """Remove a client's callback."""
        self._callbacks.pop(client_id, None)

    def _notify_subscribers(self, path: str, value: Any, exclude: str | None = None):
        """Send subscription notifications to all subscribed clients.

        Args:
            path: The path that changed
            value: The new value
            exclude: Client ID to skip (the originator of the SET)
        """
        # Check exact path match
        subscribers = set()
        if path in self._subs:
            subscribers.update(self._subs[path])

        # Also check if anyone subscribed to the parent path
        # (e.g., subscribed to /devices/0/inputs/0/Gain/value gets notified
        #  when /devices/0/inputs/0/Gain/value changes)
        # The path with /value suffix is the canonical subscription form
        if not path.endswith("/value"):
            value_path = path + "/value"
            if value_path in self._subs:
                subscribers.update(self._subs[value_path])
        else:
            # Also check without /value suffix
            base_path = path.rsplit("/value", 1)[0]
            if base_path in self._subs:
                subscribers.update(self._subs[base_path])

        # Remove the originator so they don't get echoed
        if exclude:
            subscribers.discard(exclude)

        for client_id in subscribers:
            cb = self._callbacks.get(client_id)
            if cb:
                try:
                    cb(path, value)
                except Exception:
                    log.exception("Subscription callback failed for %s", client_id)

    # ── Enumerate Values (for subscribe initial flood) ─────────────

    def enumerate_values(self, path: str, recursive: bool = False) -> list[tuple[str, Any]]:
        """Enumerate all (path/value, value) pairs under a path.

        When a client subscribes with recursive=1, the real mixer engine
        sends back every control value under that path as individual
        {path, data} messages. This method provides that list.

        For a leaf property path (e.g. /SampleRate), returns just that
        one value. For a container path with recursive=True, returns all
        descendant property values.
        """
        resolved, prop_name = self._resolve_path(path)
        if resolved is None:
            return []

        # Leaf property — return its value
        if prop_name is not None:
            val = resolved.get("value")
            if val is not None:
                # Canonical form: path ending in /value
                vpath = path if path.endswith("/value") else path + "/value"
                return [(vpath, val)]
            return []

        # Container node
        if not recursive:
            # Non-recursive subscribe on a container: send direct property values
            results = []
            for name, meta in resolved.get("properties", {}).items():
                if "value" in meta:
                    results.append((path.rstrip("/") + f"/{name}/value", meta["value"]))
            return results

        # Recursive: walk entire subtree
        results = []
        self._walk_values(path.rstrip("/"), resolved, results)
        return results

    def _walk_values(self, base: str, node: dict, out: list[tuple[str, Any]]):
        """Recursively collect all property values from a node."""
        for name, meta in node.get("properties", {}).items():
            if "value" in meta:
                out.append((f"{base}/{name}/value", meta["value"]))
        for name, child in node.get("children", {}).items():
            self._walk_values(f"{base}/{name}", child, out)

    def path_exists(self, path: str) -> bool:
        """Check if a path resolves to anything."""
        resolved, _ = self._resolve_path(path)
        return resolved is not None

    # ── Utility ──────────────────────────────────────────────────────

    def get_value(self, path: str) -> Any:
        """Quick helper: get the current value of a control path."""
        resolved, prop_name = self._resolve_path(path)
        if resolved is None:
            return None
        if prop_name is not None:
            return resolved.get("value")
        return None

    def all_paths(self) -> list[str]:
        """Return all control paths (flat list)."""
        return list(self._props.keys())
