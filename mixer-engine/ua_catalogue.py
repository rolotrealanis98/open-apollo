"""
UA catalogue overlay: Universal Audio's own content, imported from the
user's UAD install, laid over the structural device map and helper tree.

open-apollo ships the captured Mixer Engine / Mixer Helper trees without
UA's plug-in catalogue, factory preset menus and help text (and without the
capture machine's serial numbers, session, MIDI devices and network
addresses).  `oa-assets import` rebuilds the UA-written parts from the
user's own Windows UAD install into

    $OA_UA_CATALOGUE, or
    $XDG_DATA_HOME/open-apollo/ua-catalogue  (default ~/.local/share/...)

    plugins.json          plug-in catalogue + factory preset trees
    channel-presets.json  factory channel-strip preset tree
    io-presets.json       factory I/O-map preset names
    helper-text.json      {sha256: text} Mixer Helper help strings

and the daemon applies them at start-up (apply()).  Without an import the
trees stay as shipped: an empty /plugins, empty factory preset menus, empty
help strings.  Nothing here reaches the hardware.
"""

import copy
import json
import logging
import os
import re
from dataclasses import dataclass, field
from pathlib import Path

log = logging.getLogger(__name__)

FORMAT = "open-apollo-ua-catalogue"
VERSION = 1
PINS_FORMAT = "open-apollo-helper-text-pins"

_RE_CHANNEL_PRESET = re.compile(r"^/devices/\d+/(inputs|auxs)/\d+/Preset$")
_RE_PLUGIN_ENTRY = re.compile(r"^/plugins/\d+/")


def default_dir() -> Path:
    """Where `oa-assets import` writes the catalogue."""
    env = os.environ.get("OA_UA_CATALOGUE")
    if env:
        return Path(env)
    xdg = os.environ.get("XDG_DATA_HOME")
    base = Path(xdg) if xdg else Path.home() / ".local" / "share"
    return base / "open-apollo" / "ua-catalogue"


@dataclass
class Catalogue:
    directory: Path
    plugins: list = field(default_factory=list)
    channel_presets: list = field(default_factory=list)
    io_presets: list = field(default_factory=list)
    helper_texts: dict = field(default_factory=dict)
    source: dict = field(default_factory=dict)

    def empty(self) -> bool:
        return not (self.plugins or self.channel_presets or self.io_presets
                    or self.helper_texts)


def _read(directory: Path, name: str, part: str):
    """The document `name` if it is a version-1 catalogue `part`, else None
    (missing files are normal; malformed ones are reported and skipped)."""
    path = directory / name
    if not path.is_file():
        return None
    try:
        with open(path, encoding="utf-8") as f:
            doc = json.load(f)
    except (OSError, ValueError) as e:
        log.warning("UA catalogue: cannot read %s: %s", path, e)
        return None
    if not isinstance(doc, dict) or doc.get("format") != f"{FORMAT}/{part}":
        log.warning("UA catalogue: %s is not a %s/%s file", path, FORMAT, part)
        return None
    if doc.get("version") != VERSION:
        log.warning("UA catalogue: %s has version %r, expected %d (re-run "
                    "oa-assets import)", path, doc.get("version"), VERSION)
        return None
    return doc


def _tree(x) -> list:
    """A preset tree from the import: [{value, type: file|folder, locked,
    children?}], dropping anything malformed."""
    out = []
    for e in x if isinstance(x, list) else []:
        if not isinstance(e, dict) or not isinstance(e.get("value"), str):
            continue
        if e.get("type") == "folder":
            out.append({"value": e["value"], "type": "folder",
                        "locked": bool(e.get("locked", True)),
                        "children": _tree(e.get("children"))})
        else:
            out.append({"value": e["value"], "type": "file",
                        "locked": bool(e.get("locked", True))})
    return out


def load(directory: str | Path | None = None) -> Catalogue | None:
    """The imported catalogue, or None when nothing usable was imported."""
    d = Path(directory) if directory else default_dir()
    if not d.is_dir():
        return None
    cat = Catalogue(directory=d)
    doc = _read(d, "plugins.json", "plugins")
    if doc:
        cat.source = doc.get("source") or {}
        for p in doc.get("plugins") or []:
            if isinstance(p, dict) and isinstance(p.get("name"), str) and p["name"]:
                cat.plugins.append(p)
    doc = _read(d, "channel-presets.json", "channel-presets")
    if doc:
        cat.channel_presets = _tree(doc.get("presets"))
    doc = _read(d, "io-presets.json", "io-presets")
    if doc:
        cat.io_presets = [n for n in doc.get("presets") or []
                          if isinstance(n, str) and n]
    doc = _read(d, "helper-text.json", "helper-text")
    if doc and isinstance(doc.get("texts"), dict):
        cat.helper_texts = {k: v for k, v in doc["texts"].items()
                            if isinstance(k, str) and isinstance(v, str)}
    return None if cat.empty() else cat


def factory_trees(cat: Catalogue | None,
                  channel_strip: str = "Channel Strip") -> dict[str, list]:
    """{plug-in name: factory preset tree} from the import, the channel
    strip's under `channel_strip`: the locked entries the Console preset
    menus list (console_presets).  Names only: the preset files themselves
    are not part of the import.  Empty without an import."""
    if cat is None:
        return {}
    out = {p["name"]: _tree(p.get("presets")) for p in cat.plugins
           if isinstance(p, dict) and isinstance(p.get("name"), str)}
    if cat.channel_presets:
        out[channel_strip] = _tree(cat.channel_presets)
    return {name: tree for name, tree in out.items() if tree}


# ── Device map (4710) ───────────────────────────────────────────────

def plugin_controls(plugins: list) -> list[tuple[str, dict]]:
    """The engine's /plugins/N controls for a catalogue, in the engine's
    field order.  Licence state is never invented: Status stays empty and
    no Authorized/Demo property is created."""
    out = []
    for i, p in enumerate(plugins):
        base = f"/plugins/{i}"
        shorts = [{"value": s} for s in p.get("short_names") or []
                  if isinstance(s, str)]
        cats = [c for c in p.get("categories") or [] if isinstance(c, str)]
        out += [
            (base + "/Type", {"type": "string", "value": "plugin"}),
            (base + "/Name", {"type": "string", "value": p["name"],
                              "values": shorts or None}),
            (base + "/Preset", {"type": "string", "value": "",
                                "values": _tree(p.get("presets"))}),
            (base + "/Status", {"type": "string", "value": ""}),
            (base + "/Categories", {"type": "string", "value": ",".join(cats)}),
            (base + "/Unison", {"type": "bool", "value": bool(p.get("unison"))}),
            (base + "/RemoteGUI", {"type": "string", "value": ""}),
            (base + "/VstID", {"type": "string",
                               "value": str(p.get("vst_id") or "")}),
            (base + "/DefaultPreset", {"type": "string", "value": ""}),
        ]
    return out


def apply_to_state(state, cat: Catalogue) -> dict:
    """Overlay the catalogue on a loaded StateTree.  Only fills what the
    shipped map leaves empty, so a map that still carries its own lists
    (an old capture) is not changed."""
    done = {"plugins": 0, "channel_presets": 0, "io_presets": 0}
    paths = state.all_paths()
    if cat.plugins and not any(_RE_PLUGIN_ENTRY.match(p) for p in paths):
        for path, ctrl in plugin_controls(cat.plugins):
            state.add_runtime_property(path, ctrl)
        done["plugins"] = len(cat.plugins)
    if cat.channel_presets:
        for path in paths:
            if _RE_CHANNEL_PRESET.match(path):
                prop = state.get_prop_dict(path)
                if prop is not None and not prop.get("values"):
                    prop["values"] = copy.deepcopy(cat.channel_presets)
                    done["channel_presets"] += 1
    if cat.io_presets and state.path_exists("/IOMapPreset/value"):
        prop = state.get_prop_dict("/IOMapPreset")
        have = [v for v in prop.get("values") or []]
        names = {v.get("value") for v in have if isinstance(v, dict)}
        factory = [{"value": n, "type": "file", "locked": False}
                   for n in cat.io_presets if n not in names]
        if factory:
            prop["values"] = factory + have
            done["io_presets"] = len(factory)
    return done


# ── Helper tree (4720) ──────────────────────────────────────────────

def _pointer_parent(doc, pointer: str):
    """(container, key) for an RFC 6901 pointer, or None."""
    parts = [t.replace("~1", "/").replace("~0", "~")
             for t in pointer.split("/")[1:]]
    if not parts:
        return None
    node = doc
    for t in parts[:-1]:
        if isinstance(node, list):
            if not t.isdigit() or int(t) >= len(node):
                return None
            node = node[int(t)]
        elif isinstance(node, dict) and t in node:
            node = node[t]
        else:
            return None
    last = parts[-1]
    if isinstance(node, list):
        return (node, int(last)) if last.isdigit() and int(last) < len(node) else None
    return (node, last) if isinstance(node, dict) and last in node else None


def load_pins(path: str | Path) -> dict:
    try:
        with open(path, encoding="utf-8") as f:
            doc = json.load(f)
    except (OSError, ValueError):
        return {}
    if not isinstance(doc, dict) or doc.get("format") != PINS_FORMAT:
        return {}
    pins = doc.get("pins")
    return pins if isinstance(pins, dict) else {}


def apply_to_helper(root: dict, cat: Catalogue, pins: dict) -> int:
    """Put the imported help strings back where the shipped tree has an
    empty one; the pins say which text (by sha256) belongs where."""
    n = 0
    for pointer, sha in pins.items():
        text = cat.helper_texts.get(sha)
        if text is None:
            continue
        hit = _pointer_parent(root, pointer)
        if hit is None:
            continue
        container, key = hit
        if container[key] == "":
            container[key] = text
            n += 1
    return n


def apply(state, helper_tree=None, directory: str | Path | None = None,
          pins_path: str | Path | None = None) -> Catalogue | None:
    """Load the imported catalogue (if any) and overlay it on the state
    tree and, with its pins file, on the helper tree.  Returns the
    catalogue, or None when nothing was imported."""
    cat = load(directory)
    if cat is None:
        log.info("No UA catalogue imported (%s): plug-in list, factory preset "
                 "menus and helper help text stay empty; run `oa-assets "
                 "import` to bring them in from your UAD install",
                 Path(directory) if directory else default_dir())
        return None
    done = apply_to_state(state, cat)
    texts = 0
    if helper_tree is not None and pins_path is not None and cat.helper_texts:
        texts = apply_to_helper(helper_tree.root, cat, load_pins(pins_path))
    log.info("UA catalogue from %s (UAD %s): %d plug-ins, factory presets on "
             "%d channels, %d I/O presets, %d helper texts",
             cat.directory, cat.source.get("uad") or "?", done["plugins"],
             done["channel_presets"], done["io_presets"], texts)
    return cat
