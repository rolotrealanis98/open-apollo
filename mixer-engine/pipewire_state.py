"""The Linux audio side of the Apollo, as the console's Settings › PIPEWIRE
panel shows it: open-apollo's own properties under /openapollo/pipewire/.

The daemon polls `pw-dump` (and `pw-top` for xruns and the running quantum)
while a client is subscribed to these paths, and publishes:

    Available       PipeWire answered (False on macOS / without PipeWire)
    Rate, Quantum   the graph's effective rate and quantum
    MinQuantum, MaxQuantum, ForceQuantum, ForceRate, AllowedRates
                    the `settings` metadata (as `pw-metadata -n settings 0`)
    Nodes           JSON list: the Apollo's ALSA nodes and the `apollo_*`
                    virtual nodes (id, name, description, class, state,
                    channels, rate, quantum)
    Streams         JSON list: client streams linked to those nodes (id, app,
                    direction, target, state, pid)
    Xruns           pw-top's ERR summed over those nodes (0 when unknown)
    Owner           the process holding the Apollo's PCM streams

ForceQuantum and ForceRate are writable: `pw-metadata -n settings 0
clock.force-quantum N` (0 releases it), and clock.force-rate likewise.
"""

import glob
import json
import os
import re
import shutil
import subprocess

PREFIX = "/openapollo/pipewire"
WRITABLE = ("ForceQuantum", "ForceRate")
_DEFAULTS = {
    "Available": ("bool", False), "Rate": ("int", 0), "Quantum": ("int", 0),
    "MinQuantum": ("int", 0), "MaxQuantum": ("int", 0),
    "ForceQuantum": ("int", 0), "ForceRate": ("int", 0),
    "AllowedRates": ("string", "[]"), "Nodes": ("string", "[]"),
    "Streams": ("string", "[]"), "Xruns": ("int", 0), "Owner": ("string", ""),
}
# The daemon's RUNTIME_PROPERTIES for the panel
PROPERTIES = {f"{PREFIX}/{k}": {"type": t, "value": v,
                                **({} if k in WRITABLE else {"readonly": True})}
              for k, (t, v) in _DEFAULTS.items()}
SETTINGS = ["pw-metadata", "-n", "settings", "0"]
_NODE = "PipeWire:Interface:Node"


def run_tool(cmd, env):
    """stdout of `cmd`, or None when it is missing, fails or hangs."""
    if shutil.which(cmd[0]) is None:
        return None
    try:
        r = subprocess.run(cmd, env=env, capture_output=True, text=True,
                           timeout=5)
    except (OSError, subprocess.TimeoutExpired):
        return None
    return r.stdout if r.returncode == 0 else None


def get_setting(key, env, run=run_tool):
    """A `settings` metadata value as pw-metadata prints it, or None."""
    m = re.search(rf"key:'{re.escape(key)}' value:'([^']*)'",
                  run([*SETTINGS, key], env) or "")
    return m.group(1) if m else None


def set_setting(key, value, env, run=run_tool) -> bool:
    return run([*SETTINGS, key, str(value)], env) is not None


def locate(card):
    """(env reaching the PipeWire to read, Owner text).  The PipeWire that
    holds the Apollo's streams, else the first user PipeWire socket (the
    Apollo's nodes stay listed while suspended); env None without one."""
    from metering import apollo_stream_owner, pipewire_env
    owner = apollo_stream_owner(card) if card is not None else None
    text = f"{owner[1]} ({owner[0]})" if owner else ""
    if owner and owner[1] == "pipewire":
        return pipewire_env(owner[0]), text
    socks = sorted(glob.glob("/run/user/*/pipewire-0"))
    if socks:
        return dict(os.environ, XDG_RUNTIME_DIR=os.path.dirname(socks[0])), text
    return None, text


def _int(v) -> int:
    try:
        return int(v)
    except (TypeError, ValueError):
        return 0


def _whole(v) -> int | None:
    """An int, an integral float or a digit string (not a bool) as an int."""
    if isinstance(v, bool):
        return None
    if isinstance(v, int):
        return v
    if isinstance(v, float) and v.is_integer():
        return int(v)
    if isinstance(v, str) and v.strip().isdigit():
        return int(v.strip())
    return None


def parse_top(text):
    """{node id: {quantum, rate, err}} from the last block of
    `pw-top -b -n 2` (the first block has no measurements yet)."""
    rows = {}
    for line in text.splitlines():
        f = line.split()
        if f[:2] == ["S", "ID"]:
            rows = {}
        elif len(f) >= 10 and f[1].isdigit():
            rest = f[9:]        # FORMAT (fmt, channels, rate) then NAME
            fmt_rate = _int(rest[2]) if len(rest) >= 4 else 0
            rows[int(f[1])] = {"quantum": _int(f[2]),
                               "rate": _int(f[3]) or fmt_rate,
                               "err": _int(f[8])}
    return rows


def is_apollo(props) -> bool:
    return (props.get("alsa.driver_name") == "ua_apollo"
            or str(props.get("node.name", "")).startswith("apollo_"))


def snapshot(objs, top=None, owner=""):
    """The panel's values from a pw-dump object list and parse_top rows."""
    top = top or {}

    def props(o):
        return (o.get("info") or {}).get("props") or {}

    nodes = {o["id"]: o for o in objs if o.get("type") == _NODE}
    clients = {o["id"]: props(o) for o in objs
               if o.get("type") == "PipeWire:Interface:Client"}
    settings = {}
    for o in objs:
        if (o.get("type") == "PipeWire:Interface:Metadata"
                and (o.get("props") or {}).get("metadata.name") == "settings"):
            settings = {m.get("key"): m.get("value")
                        for m in o.get("metadata") or []
                        if m.get("subject", 0) == 0}
    # the Apollo's ALSA nodes first, then the virtual ones
    apollo = sorted((i for i, o in nodes.items() if is_apollo(props(o))),
                    key=lambda i: (props(nodes[i]).get("alsa.driver_name")
                                   != "ua_apollo", i))

    node_rows = []
    for i in apollo:
        p, info, t = props(nodes[i]), nodes[i].get("info") or {}, top.get(i, {})
        cls = p.get("media.class", "")
        ports = info.get("n-output-ports" if "Source" in cls else "n-input-ports")
        node_rows.append({
            "id": i, "name": p.get("node.name", ""),
            "description": p.get("node.description") or p.get("node.nick") or "",
            "class": cls, "state": info.get("state") or "",
            "channels": _int(p.get("audio.channels")) or _int(ports),
            "rate": t.get("rate") or _int(p.get("audio.rate")),
            "quantum": t.get("quantum")
            or _int(str(p.get("node.latency", "")).split("/")[0])})

    streams, seen = [], set()
    for o in objs:
        if o.get("type") != "PipeWire:Interface:Link":
            continue
        info = o.get("info") or {}
        out_id, in_id = info.get("output-node-id"), info.get("input-node-id")
        for sid, target, direction in ((out_id, in_id, "playback"),
                                       (in_id, out_id, "capture")):
            s = nodes.get(sid)
            if (target not in apollo or s is None or (sid, target) in seen
                    or not props(s).get("media.class", "").startswith("Stream/")):
                continue
            seen.add((sid, target))
            p = props(s)
            pid = p.get("application.process.id") or (
                clients.get(_int(p.get("client.id"))) or {}).get(
                    "application.process.id")
            streams.append({
                "id": sid,
                "app": p.get("application.name") or p.get("node.description")
                or p.get("node.name", ""),
                "direction": direction,
                "target": props(nodes[target]).get("node.description")
                or props(nodes[target]).get("node.name", ""),
                "state": (s.get("info") or {}).get("state") or "",
                "pid": _int(pid)})

    # The graph's rate and quantum: forced, else what the Apollo's driver
    # node runs at, else the defaults
    alsa = [i for i in apollo
            if props(nodes[i]).get("alsa.driver_name") == "ua_apollo"]
    running = next((t for t in (top.get(_int(props(nodes[i]).get(
        "node.driver-id")) or i) for i in alsa) if t and t["quantum"]), {})
    force_q = _int(settings.get("clock.force-quantum"))
    force_r = _int(settings.get("clock.force-rate"))
    return {
        "Available": True,
        "Rate": force_r or running.get("rate") or _int(settings.get("clock.rate")),
        "Quantum": force_q or running.get("quantum")
        or _int(settings.get("clock.quantum")),
        "MinQuantum": _int(settings.get("clock.min-quantum")),
        "MaxQuantum": _int(settings.get("clock.max-quantum")),
        "ForceQuantum": force_q, "ForceRate": force_r,
        "AllowedRates": json.dumps([int(r) for r in re.findall(
            r"\d+", str(settings.get("clock.allowed-rates", "")))]),
        "Nodes": json.dumps(node_rows), "Streams": json.dumps(streams),
        "Xruns": sum(top.get(i, {}).get("err", 0) for i in apollo),
        "Owner": owner,
    }


class PipeWireState:
    """The last poll and the writes.  `run(cmd, env)` runs a PipeWire tool
    (stdout or None) and `locate(card)` finds it (see locate()); tests and
    the fake daemon pass their own."""

    def __init__(self, run=run_tool, locate=locate):
        self.run, self.locate = run, locate
        self.env = None
        self.values = {k: v for k, (_, v) in _DEFAULTS.items()}

    @property
    def available(self) -> bool:
        return bool(self.values.get("Available"))

    def poll(self, card=None) -> dict:
        """Blocking (pw-dump, pw-top): run it off the event loop."""
        env, owner = self.locate(card)
        dump = self.run(["pw-dump"], env) if env is not None else None
        try:
            objs = json.loads(dump) if dump else None
        except ValueError:
            objs = None
        if not isinstance(objs, list):
            self.env = None
            self.values = {k: v for k, (_, v) in _DEFAULTS.items()}
            self.values["Owner"] = owner
        else:
            self.env = env
            top = parse_top(self.run(["pw-top", "-b", "-n", "2"], env) or "")
            self.values = snapshot(objs, top, owner)
        return dict(self.values)

    def force(self, name, value) -> dict | None:
        """Write ForceQuantum / ForceRate: the values to publish, or None
        when refused (no PipeWire, an invalid value, pw-metadata failed).
        A quantum is 0 or a power of two within MinQuantum…MaxQuantum, a
        rate 0 or one the Apollo clocks at."""
        from hardware import VALID_SAMPLE_RATES
        n = _whole(value)
        if n is None:
            return None
        if name == "ForceQuantum":
            lo = self.values.get("MinQuantum") or 32
            hi = self.values.get("MaxQuantum") or 8192
            ok = n == 0 or (n > 0 and n & (n - 1) == 0 and lo <= n <= hi)
            key, effective = "clock.force-quantum", "Quantum"
        elif name == "ForceRate":
            ok = n == 0 or n in VALID_SAMPLE_RATES
            key, effective = "clock.force-rate", "Rate"
        else:
            return None
        if not ok or not self.available or \
                not set_setting(key, n, self.env, self.run):
            return None
        out = {name: n, **({effective: n} if n else {})}
        self.values.update(out)
        return out
