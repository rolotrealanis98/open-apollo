"""Console gesture undo and property snapshots; no transport or hardware access."""
from copy import deepcopy


TRANSIENT = {"Dirty", "Session", "ModifiedSessions", "Progress", "UndoRecording",
             "UndoRedo", "Sleep", "MixerState", "MeterPulse", "Type"}


def editable(path, meta):
    leaf = path.rsplit("/", 1)[-1]
    return (not meta.get("readonly") and leaf not in TRANSIENT
            and meta.get("value") is not None
            and "/meters/" not in path and not path.startswith("/plugins/")
            and meta.get("type") in ("bool", "int", "int64", "float", "string"))


class History:
    def __init__(self, state, publish):
        self.state, self.publish = state, publish
        self.groups = {}
        self.entries = []
        self.current = 0
        self.refresh()

    def snapshot(self):
        return {p: deepcopy(m.get("value")) for p, m in self.state._props.items()
                if editable(p, m)}

    def record(self, client, before):
        after = self.snapshot()
        delta = {p: (v, after[p]) for p, v in before.items()
                 if p in after and v != after[p]}
        if not delta:
            return
        self.publish("/Dirty", True)
        if client in self.groups:
            group = self.groups[client]
            for p, (old, new) in delta.items():
                group[p] = (group.get(p, (old, new))[0], new)
        else:
            self.append(delta)

    def append(self, delta):
        delta = {p: pair for p, pair in delta.items() if pair[0] != pair[1]}
        if not delta:
            return
        del self.entries[self.current:]
        self.entries.append(delta)
        if len(self.entries) > 50:
            self.entries.pop(0)
        self.current = len(self.entries)
        self.refresh()

    def bracket(self, client, value):
        if not isinstance(value, bool):
            raise ValueError("UndoRecording requires a boolean")
        if value:
            self.groups.setdefault(client, {})
        else:
            self.append(self.groups.pop(client, {}))
        self.publish("/UndoRecording", bool(self.groups))

    def disconnect(self, client):
        # Accepted changes survive disconnect and become an undo step.
        self.append(self.groups.pop(client, {}))
        self.publish("/UndoRecording", bool(self.groups))

    def move(self, target):
        if self.groups or type(target) is not int:
            raise ValueError("invalid undo target or an open gesture")
        if target == self.current - 1 and self.current > 0:
            delta, side = self.entries[target], 0
        elif target == self.current + 1 and target <= len(self.entries):
            delta, side = self.entries[self.current], 1
        else:
            raise ValueError("undo/redo requires current minus/plus one")
        for path, pair in delta.items():
            if path.startswith("@node:"):
                self.state.replace_node(path[6:], pair[side])
            else:
                self.state.set(path + "/value", pair[side])
        self.current = target
        self.publish("/Dirty", True)
        self.refresh()

    def refresh(self):
        self.publish("/UndoRedo", self.current, "int")
        prop = self.state.get_prop_dict("/UndoRedo")
        prop["values"] = ([{"type": "undo", "value": self.current - 1,
                            "enabled": self.current > 0},
                           {"type": "current", "value": self.current},
                           {"type": "redo", "value": self.current + 1,
                            "enabled": self.current < len(self.entries)}])
