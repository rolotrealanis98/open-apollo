"""Experimental pure two-view plugin publication; NOT registered by daemon.

Ownership is an adapter attestation, never proof supplied by a client. The future
native loader must retain/release its lease transactionally; this model never
creates handles, changes UA's plugin state, calls hardware or dispatches notifications.
"""

from dataclasses import dataclass
import copy
import gc
import math
import pickle
import re
from plugin_lifecycle import is_plugin_slot_path
from state_tree import StateTree


def _clone(value):
    """copy.deepcopy of the plain (JSON-like) state and helper trees, faster.

    A publication copies both trees several times; each copy makes ~500k objects. On the
    daemon, whose heap holds the plug-in emulators, the collector passes those allocations
    trigger cost several times the copy itself (a control change took 0.5-1 s). A pickle round
    trip gives the same tree in a third of the time, with the collector paused meanwhile."""
    enabled = gc.isenabled()
    gc.disable()
    try:
        try:
            return pickle.loads(pickle.dumps(value, pickle.HIGHEST_PROTOCOL))
        except (pickle.PicklingError, TypeError, AttributeError):
            return copy.deepcopy(value)
    finally:
        if enabled:
            gc.enable()


class PublicationError(ValueError):
    pass


@dataclass(frozen=True)
class OwnedHandle:
    value: int
    owner: str
    generation: int


@dataclass(frozen=True)
class LoadedPlugin:
    name: str
    handle: OwnedHandle
    power: bool
    latency: int
    parameters: tuple  # Original fields include Type (or legacy type), Name and values.


@dataclass(frozen=True)
class ControlReadback:
    """Internal adapter observation, never a client-provided ownership proof."""

    handle: OwnedHandle
    field: str
    values: dict


class Publisher:
    def __init__(self, state, helper, *, owner, generation=0):
        if not isinstance(owner, str) or not owner:
            raise PublicationError("backend owner required")
        self.state, self.helper, self.owner = state, helper, owner
        self.generation = generation
        self.connected = True
        self.version = 0

    def disconnect(self):
        self.connected = False
        self.generation += 1
        self.version += 1

    def validate(self, result):
        if (
            not isinstance(result, LoadedPlugin)
            or not isinstance(result.name, str)
            or not result.name
        ):
            raise PublicationError("loaded name required")
        lease = result.handle
        if (
            not isinstance(lease, OwnedHandle)
            or type(lease.value) is not int
            or not 0 < lease.value <= 0xFFFFFFFFFFFFFFFF
            or lease.owner != self.owner
            or type(lease.generation) is not int
            or lease.generation != self.generation
        ):
            raise PublicationError(
                "current backend-owned nonzero handle required"
            )
        if (
            type(result.power) is not bool
            or type(result.latency) is not int
            or not 0 <= result.latency <= 100000000
        ):
            raise PublicationError("invalid power/latency")
        if (
            not isinstance(result.parameters, tuple)
            or len(result.parameters) > 4096
        ):
            raise PublicationError(
                "bounded immutable parameter tuple required"
            )
        ids = set()
        for parameter in result.parameters:
            if not isinstance(parameter, dict) or set(parameter) != {
                "id",
                "properties",
            }:
                raise PublicationError("invalid parameter schema")
            ident, props = parameter["id"], parameter["properties"]
            if (
                type(ident) is not int
                or not 0 <= ident <= 0xFFFFFFFF
                or ident in ids
            ):
                raise PublicationError("unique parameter IDs required")
            ids.add(ident)
            if (
                not isinstance(props, dict)
                or not {"Name", "NormalizedValue", "StringValue"} <= set(props)
                or len({"Type", "type"} & set(props)) != 1
                or set(props)
                - {
                    "Type",
                    "type",
                    "control_id",
                    "Name",
                    "NormalizedValue",
                    "StringValue",
                    "StepValue",
                }
            ):
                raise PublicationError(
                    "parameter fields must be validated captured schema"
                )
            # Genuine Rack metadata walker accepts these node kinds and resolves
            # optional control_id or actual ordinal plus matching genuine cached Name.
            # Numeric tree IDs are not synthesized control IDs.
            # Preserve the original spelling; dual aliases are ambiguous.
            kind = props["Type"] if "Type" in props else props["type"]
            control_id = props.get("control_id")
            if (
                not isinstance(kind, dict)
                or kind.get("type") != "string"
                or kind.get("value") not in ("parameter", "effect_control")
            ):
                raise PublicationError("original parameter node type required")
            if "control_id" in props and (
                not isinstance(control_id, dict)
                or control_id.get("type") != "string"
                or not isinstance(control_id.get("value"), str)
                or not control_id["value"]
                or "\x00" in control_id["value"]
            ):
                raise PublicationError(
                    "original nonempty control_id string required"
                )
            for field in ("Name", "StringValue"):
                if (
                    not isinstance(props[field], dict)
                    or props[field].get("type") != "string"
                    or not isinstance(props[field].get("value"), str)
                ):
                    raise PublicationError(
                        "string parameter metadata required"
                    )
            norm = props["NormalizedValue"]
            value = norm.get("value") if isinstance(norm, dict) else None
            if (
                not isinstance(norm, dict)
                or norm.get("type") != "float"
                or type(value) not in (int, float)
                or not math.isfinite(value)
                or not 0 <= value <= 1
            ):
                raise PublicationError("finite normalized value required")
            if norm.get("min", 0) != 0 or norm.get("max", 1) != 1:
                raise PublicationError("normalized bounds must be zero to one")
            if "StepValue" in props:
                step = props["StepValue"]
                if not isinstance(step, dict):
                    raise PublicationError("step metadata required")
                kind = step.get("type")
                value = step.get("value")
                allowed = step.get("values")
                if (
                    kind not in ("string", "int")
                    or (kind == "string" and not isinstance(value, str))
                    or (kind == "int" and type(value) is not int)
                    or not isinstance(allowed, list)
                    or value not in allowed
                ):
                    raise PublicationError("enumerated step metadata required")

    def stage(self, slot, result=None):
        if not self.connected:
            raise PublicationError("disconnected")
        slot = "/" + "/".join(part for part in slot.split("/") if part)
        if not is_plugin_slot_path(
            slot + "/EffectName"
        ) or not self.state.path_exists(slot):
            raise PublicationError("existing effect slot required")
        if result is not None:
            self.validate(result)
        state = StateTree()
        state.root = _clone(self.state.root)
        # Existing flat index is cached metadata, rebuilt to avoid detached dicts.
        state._props = {}
        node, _ = state._resolve_path(slot)
        node.setdefault("children", {}).pop("parameters", None)
        changes = {
            "EffectName": ("string", result.name if result else ""),
            "EffectInstance": (
                "pointer",
                result.handle.value if result else 0,
            ),
            "Power": ("bool", result.power if result else False),
            "Latency": ("int", result.latency if result else 0),
            "Status": ("string", ""),
        }  # Local empty placeholder; native success semantics unverified.
        for name, (kind, value) in changes.items():
            state.add_runtime_property(
                slot + "/" + name, {"type": kind, "value": value}
            )
        if result:
            for parameter in result.parameters:
                for name, meta in parameter["properties"].items():
                    state.add_runtime_property(
                        f"{slot}/parameters/{parameter['id']}/{name}",
                        copy.deepcopy(meta),
                    )
        helper = _clone(self.helper.root)
        target = helper
        for part in slot.strip("/").split("/"):
            children = target.setdefault("children", [])
            child = next(
                (child for child in children if child.get("path") == part),
                None,
            )
            if child is None:
                child = {"path": part, "properties": {}, "children": []}
                children.append(child)
            target = child
        target["children"] = [
            child
            for child in target.get("children", [])
            if child.get("path") != "parameters"
        ]
        for name, (kind, value) in changes.items():
            target.setdefault("properties", {})[name] = {
                "type": kind,
                "value": value,
                "read_only": name in ("EffectInstance", "Latency", "Status"),
            }
        if result:
            target["children"].append(
                {
                    "path": "parameters",
                    "properties": {},
                    "children": [
                        {
                            "path": str(p["id"]),
                            "properties": copy.deepcopy(p["properties"]),
                            "children": [],
                        }
                        for p in result.parameters
                    ],
                }
            )
        # Events are descriptions only; dispatch only after successful commit.
        events = [
            (slot + "/" + name + "/value", value)
            for name, (kind, value) in changes.items()
            if name != "EffectInstance"
        ]
        events.append(
            (slot + "/EffectInstance/value", changes["EffectInstance"][1])
        )
        return Transaction(
            self,
            self.version,
            self.generation,
            state.root,
            helper,
            events,
            _clone(self.state.root),
            _clone(self.helper.root),
        )

    def _control_target(self, slot, handle, field):
        if not self.connected:
            raise PublicationError("disconnected")
        if not isinstance(slot, str):
            raise PublicationError("existing effect slot required")
        slot = "/" + "/".join(part for part in slot.split("/") if part)
        if not is_plugin_slot_path(
            slot + "/EffectName"
        ) or not self.state.path_exists(slot):
            raise PublicationError("existing effect slot required")
        if (
            not isinstance(handle, OwnedHandle)
            or type(handle.value) is not int
            or not 0 < handle.value <= 0xFFFFFFFFFFFFFFFF
            or handle.owner != self.owner
            or type(handle.generation) is not int
            or handle.generation != self.generation
        ):
            raise PublicationError("current backend-owned handle required")
        name = self.state.get_value(slot + "/EffectName")
        state_handle = self.state.get_value(slot + "/EffectInstance")
        helper_handle = self.helper.get_value(slot + "/EffectInstance")
        if (
            not isinstance(name, str)
            or not name
            or type(state_handle) is not int
            or type(helper_handle) is not int
            or state_handle != handle.value
            or helper_handle != handle.value
            or self.helper.get_value(slot + "/EffectName") != name
        ):
            raise PublicationError("current identity must agree in both views")
        node, prop = self.state._resolve_path(slot)
        helper = self.helper.root
        for part in slot.strip("/").split("/"):
            helper = next(
                (
                    child
                    for child in helper.get("children", [])
                    if child.get("path") == part
                ),
                None,
            )
            if helper is None:
                raise PublicationError("helper target missing")
        if field == "Power":
            keys = ("Power",)
        else:
            match = (
                re.fullmatch(
                    r"parameters/(0|[1-9][0-9]{0,9})/(NormalizedValue|StepValue)",
                    field,
                )
                if isinstance(field, str)
                else None
            )
            if not match or int(match[1]) > 0xFFFFFFFF:
                raise PublicationError("existing parameter field required")
            ident = match[1]
            node = (
                node.get("children", {})
                .get("parameters", {})
                .get("children", {})
                .get(ident)
            )
            parent = next(
                (
                    child
                    for child in helper.get("children", [])
                    if child.get("path") == "parameters"
                ),
                None,
            )
            helper = (
                next(
                    (
                        child
                        for child in parent.get("children", [])
                        if child.get("path") == ident
                    ),
                    None,
                )
                if parent
                else None
            )
            if node is None or helper is None:
                raise PublicationError("parameter missing in state views")
            a, b = node.get("properties", {}), helper.get("properties", {})
            # Use the same backend metadata validator as initial publication.
            for props in (a, b):
                self.validate(
                    LoadedPlugin(
                        name,
                        handle,
                        True,
                        0,
                        ({"id": int(ident), "properties": props},),
                    )
                )
            keys = ("NormalizedValue", "StringValue") + (
                ("StepValue",) if "StepValue" in a else ()
            )
            if ("StepValue" in a) != ("StepValue" in b) or match[
                2
            ] not in keys:
                raise PublicationError("parameter schema views disagree")
            for key in a:
                if a[key] != b[key]:
                    raise PublicationError("parameter metadata views disagree")
        a, b = node.get("properties", {}), helper.get("properties", {})
        for key in keys:
            if (
                key not in a
                or key not in b
                or a[key].get("type") != b[key].get("type")
                or type(a[key].get("value")) is not type(b[key].get("value"))
                or a[key].get("value") != b[key].get("value")
            ):
                raise PublicationError("control value views disagree")
            if a[key].get("read_only", False) != b[key].get(
                "read_only", False
            ):
                raise PublicationError(
                    "control access metadata views disagree"
                )
        if field == "Power" and (
            a["Power"].get("type") != "bool"
            or type(a["Power"].get("value")) is not bool
        ):
            raise PublicationError("boolean Power required")
        return slot, node, helper, keys

    def snapshot_control(self, slot, handle, field):
        """Validate the existing target; this does not prove hardware ownership."""
        _, node, _, keys = self._control_target(slot, handle, field)
        return ControlReadback(
            handle,
            field,
            {
                key: copy.deepcopy(node["properties"][key]["value"])
                for key in keys
            },
        )

    def stage_control(self, slot, readback):
        """Stage only backend-observed runtime values, retaining all metadata."""
        slot, node, keys, values = self.check_control(slot, readback)
        state_root, helper_root = _clone((self.state.root, self.helper.root))
        return self._stage_control(slot, node, keys, values, readback, state_root, helper_root)

    def check_control(self, slot, readback):
        """What stage_control validates, without staging: no copy of either tree.

        Staging copies the whole state and helper trees several times (hundreds of ms on a
        live daemon); a control is checked four times before the one staging it commits."""
        if not isinstance(readback, ControlReadback):
            raise PublicationError("typed control readback required")
        slot, node, helper, keys = self._control_target(
            slot, readback.handle, readback.field
        )
        values = copy.deepcopy(readback.values)
        if not isinstance(values, dict) or set(values) != set(keys):
            raise PublicationError("complete coherent runtime values required")
        if readback.field == "Power":
            if type(values["Power"]) is not bool:
                raise PublicationError("boolean Power readback required")
        else:
            value = values["NormalizedValue"]
            try:
                valid = (
                    type(value) in (int, float)
                    and math.isfinite(value)
                    and 0 <= value <= 1
                )
            except (ValueError, OverflowError, TypeError):
                valid = False
            if not valid or not isinstance(values["StringValue"], str):
                raise PublicationError("valid parameter readback required")
            if "StepValue" in keys:
                meta = node["properties"]["StepValue"]
                value = values["StepValue"]
                if (
                    (meta["type"] == "int" and type(value) is not int)
                    or (
                        meta["type"] == "string" and not isinstance(value, str)
                    )
                    or value not in meta["values"]
                ):
                    raise PublicationError("enumerated step readback required")
        return slot, node, keys, values

    def _stage_control(self, slot, node, keys, values, readback, state_root, helper_root):
        events = []
        for state_props, helper_props, key, value, event in self._control_edits(
                slot, node, keys, values, readback, state_root, helper_root):
            state_props["value"] = value
            helper_props["value"] = copy.deepcopy(value)
            events.append(event)
        return Transaction(
            self,
            self.version,
            self.generation,
            state_root,
            helper_root,
            events,
            _clone(self.state.root),
            _clone(self.helper.root),
        )

    @staticmethod
    def _control_edits(slot, node, keys, values, readback, state_root, helper_root):
        """(state property, helper property, key, value, event) of each value that changes."""
        state_target = state_root
        helper_target = helper_root
        target_path = (
            slot
            if readback.field == "Power"
            else slot + "/" + readback.field.rsplit("/", 1)[0]
        )
        for part in target_path.strip("/").split("/"):
            state_target = state_target["children"][part]
            helper_target = next(
                child
                for child in helper_target["children"]
                if child.get("path") == part
            )
        edits = []
        for key in keys:
            old = node["properties"][key]["value"]
            if type(old) is not type(values[key]) or old != values[key]:
                edits.append((
                    state_target["properties"][key],
                    helper_target["properties"][key],
                    key,
                    values[key],
                    (target_path + "/" + key + "/value", copy.deepcopy(values[key])),
                ))
        return edits

    def commit_control(self, slot, readback, *, fault=None):
        """stage_control(slot, readback).commit(fault=fault), edited in place.

        For a caller that stages and commits in one step on the thread that owns the trees:
        nothing can change between the two, so the whole-tree copies and comparisons a staged
        Transaction needs (most of a control change's time on a live daemon) prove nothing
        here. Only this control's values change; a fault restores them in both views."""
        if not self.connected:
            raise PublicationError("stale/disconnected transaction")
        slot, node, keys, values = self.check_control(slot, readback)
        edits = self._control_edits(slot, node, keys, values, readback,
                                    self.state.root, self.helper.root)
        before = [(s, h, copy.deepcopy(s["value"]), copy.deepcopy(h["value"]))
                  for s, h, _, _, _ in edits]
        try:
            for state_props, _, _, value, _ in edits:
                state_props["value"] = value
            if fault:
                fault("state_swapped")
            for _, helper_props, _, value, _ in edits:
                helper_props["value"] = copy.deepcopy(value)
            if fault:
                fault("both_swapped")
        except Exception:
            for state_props, helper_props, old_state, old_helper in before:
                state_props["value"], helper_props["value"] = old_state, old_helper
            raise
        self.version += 1
        return [event for _, _, _, _, event in edits]


@dataclass
class Transaction:
    publisher: Publisher
    version: int
    generation: int
    state_root: dict
    helper_root: dict
    events: list
    expected_state: dict
    expected_helper: dict
    used: bool = False

    def __post_init__(self):
        # Public dataclass fields aid inspection but cannot change the validated
        # staged payload or its optimistic concurrency preconditions.
        self._sealed = _clone(
            (
                self.version,
                self.generation,
                self.state_root,
                self.helper_root,
                self.events,
                self.expected_state,
                self.expected_helper,
            )
        )

    def commit(self, *, fault=None):
        p = self.publisher
        if (
            self.version,
            self.generation,
            self.state_root,
            self.helper_root,
            self.events,
            self.expected_state,
            self.expected_helper,
        ) != self._sealed:
            raise PublicationError(
                "staged transaction was modified after validation"
            )
        if (
            self.used
            or not p.connected
            or p.version != self.version
            or p.generation != self.generation
        ):
            raise PublicationError("stale/disconnected transaction")
        if (
            p.state.root != self.expected_state
            or p.helper.root != self.expected_helper
        ):
            raise PublicationError(
                "state changed after staging; retry against fresh views"
            )
        old_state, old_helper, old_index = (
            p.state.root,
            p.helper.root,
            p.state._props,
        )
        try:
            p.state.root = _clone(self.state_root)
            if fault:
                fault("state_swapped")
            p.helper.root = _clone(self.helper_root)
            p.state._props = {}

            # _props is not used for traversal but callers use it for inspection.
            def index(node, path=""):
                for key, meta in node.get("properties", {}).items():
                    p.state._props[path + "/" + key] = meta
                for key, child in node.get("children", {}).items():
                    index(child, path + "/" + key)

            index(p.state.root)
            if fault:
                fault("both_swapped")
        except Exception:
            p.state.root, p.helper.root, p.state._props = (
                old_state,
                old_helper,
                old_index,
            )
            raise
        self.used = True
        p.version += 1
        return copy.deepcopy(self.events)
