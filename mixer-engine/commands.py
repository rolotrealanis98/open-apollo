"""Object commands: the `post` verb of the Mixer Engine protocol (parity A8).

The engine registers ~68 command classes (`CMxRoot::InitializeCommands`) and
answers a `post <path>` with its result or one of two error strings:

    "Unable to resolve the command for the post."   no such command
    "Unable to execute command."                    the command failed

A post is `post <object path>/<CommandName> [<cmdparams>]`: the last path
component names the command, the rest is the object it works on
(`/devices/0/inputs/3/CopyChannel`, `/Identify`, `/SaveAsSnapshot`).  The
parameters are the engine's `cmdparams`.  Their wire format is not known, so
this module takes a JSON object, as the post body (`post /SaveAsSnapshot
{"name":"verse"}`), as the `cmdparams` query parameter, or both (merged); a
bare scalar body is the `value` key.  The keys below are the daemon's own
choice, not the engine's.

What is implemented needs no UA file format:

    Identify                       flash the front panel red/green as the
                                   Mixer Engine does (OP 0x1d), then off
    ClearClips, ClearSolos         reset clip holds; clear every Solo
    CopyChannel, PasteChannel,     a channel's property values, held in memory
    CopyToSends
    SaveAsSnapshot, MoveSnapshot,  named sets of property values, in
    DeleteSnapshot,                <state>/<device>.snapshots.json
    ExportSnapshots, ImportSnapshots
    NewSession, OpenSession,       the full persisted state, in the daemon's
    SaveSession, SaveSessionAs,    OWN JSON format (<state>/<device>.sessions/
    DeleteSession, RenameSession,  <name>.session.json).  It is not UA's
    SaveLastUsedSession            session XML and no UA tool reads it.
    SaveAsPreset, DeletePreset,    per-channel presets, in
    MovePreset                     <state>/<device>.presets.json
    SaveIOMapPreset,               I/O map presets (the values of the
    DeleteIOMapPreset,             IOMapPreset and route properties), in
    RenameIOMapPreset              <state>/<device>.iomaps.json; host-side,
                                   nothing goes to the driver
    Undo, Redo                     the last client SET batches (50 deep);
                                   volatile paths, the preamps and the
                                   monitor level are never undone

Import (an I/O preset file picked in a dialog on the engine's host) is
refused with the reason logged.  Every other command of the engine's list
is known and answers "Unable to execute command."; the licensing commands (PluginStartDemo,
SIDRLicenseStateUpdated, GetPluginAuth, anything about authorization or
licenses) and anything not in the list answer "Unable to resolve the command
for the post.".

Files named by a client (`file`) live in <state>/<device>.exports/ and must
be a plain file name: the port has no authentication, so a client does not
get to choose a path.
"""

import asyncio
import json
import logging
import os
import re
import threading
import time
from contextlib import nullcontext
from pathlib import Path
from typing import Any, Callable
from urllib.parse import quote, unquote

log = logging.getLogger("ua-mixer-daemon")

ERR_UNRESOLVED = "Unable to resolve the command for the post."
ERR_FAILED = "Unable to execute command."

_RE_DEVICE_NAME = re.compile(r"^/devices/\d+/DeviceName(?:/value)?$")


def refused_set(path: str, value: Any) -> bool:
    """A SET the engine refuses before it changes anything: an empty
    device name (CMxMixer::SetProperty, kPropDeviceName 0x6d, returns an
    error for "")."""
    return bool(_RE_DEVICE_NAME.match(path)) and \
        (value is None or str(value) == "")

IDENTIFY_PATH = "/devices/0/Identify/value"
IDENTIFY_MAX_SECONDS = 30.0
# The Mixer Engine's IdentifyDevice sequence (_gIdentifySequence, apollo-
# linux kext-re-rows §4): {OP 0x1d value, ms}.  On the x4 1 lights the
# whole front panel red, 2 green, 3 and 0 leave it normal (titan
# 2026-10-11), so it flashes red/green for about 1.4 s
IDENTIFY_SEQUENCE = ((3, 250), (1, 125), (2, 250), (1, 125), (3, 250),
                     (1, 125), (2, 250), (1, 1), (0, 1))
IDENTIFY_SEQUENCE_SECONDS = sum(ms for _, ms in IDENTIFY_SEQUENCE) / 1000.0

UNDO_MAX = 50           # batches kept
UNDO_GAP = 0.5          # seconds: SETs closer than this from one client are one batch
NAME_MAX = 64
FORMAT_SNAPSHOTS = "open-apollo-snapshots"
FORMAT_SESSION = "open-apollo-session"
FORMAT_PRESETS = "open-apollo-presets"
FORMAT_IOMAPS = "open-apollo-iomap-presets"

# Commands of CMxRoot::InitializeCommands that this daemon does not run
# (UA file formats, plugin inserts, speaker/headphone correction, Dante,
# factory tests, other devices).  The engine knows them, so a post gets
# "Unable to execute command.".
KNOWN_UNSUPPORTED = frozenset(n.lower() for n in """
    CloseWindow OpenWindow ShuffleInserts MoveInsert CopyInsert PasteInsert
    MoveChannel ToXML FromXML LEDTest AddDevice RemoveDevice
    CloseModalDialog DeleteFolder NewFolder UpdatePlugInStatus
    PluginBarCopySettings PluginBarPasteSettings SetTouch
    Export DumpChains
    GetMidiBuffers SetSpeakerCorrectionSpec GetSpeakerCorrectionSpec
    SetSpeakerTrims GetSpeakerTrims SetHeadphoneCorrectionSpec
    GetHeadphoneCorrectionSpec SetHeadphone2CorrectionSpec
    GetHeadphone2CorrectionSpec GetBassManagementSpec GetCorrectionImage
    ShowSoundIDReference BeginSpeakerMeasurement EndSpeakerMeasurement
    SceneOperation ResetDanteNetwork DumpDanteConnectionState
    ConnectToSpdif ConnectFromSpdif ConnectFromAes ConnectFromAndToSpdif
    SetParam SetMADIConverter SetHiZOnOff SetHiZImpedance SetHPLevel
""".split())

_LICENSING = re.compile(r"auth|licens|demo", re.IGNORECASE)

# Not part of a channel when it is copied, pasted, saved as a preset or
# snapshotted: identity and structure, front-panel state, the preamp (a
# pasted or recalled 48V must never reach a microphone), meters, plugins.
_CHANNEL_SKIP_NAMES = frozenset({
    "Type", "Name", "StereoName", "IOType", "Stereo", "Preset", "ID",
    "SelectedOnFront", "Latency", "Active", "ChannelHidden",
    "CustomIntegratedPreampInputMeter", "Solo"})
_CHANNEL_SKIP_CHILDREN = frozenset({"meters", "effects", "preamps"})
# A snapshot keeps Solo, but not the monitor level (a recall must not jump
# the speakers)
_SNAPSHOT_SKIP_NAMES = (_CHANNEL_SKIP_NAMES - {"Solo"}) | {
    "CRMonitorLevel", "CRMonitorLevelTapered"}
_KINDS = ("inputs", "auxs", "outputs")

_RE_CHANNEL = re.compile(r"^/devices/0/(inputs|auxs|outputs)/(\d+)$")
_RE_PRESET_PATH = re.compile(
    r"^/devices/0/(inputs|auxs|outputs)/(\d+)/Preset/value$")

# What a session does not keep: driver- and device-owned status
_VOLATILE = re.compile(
    r"^/(initialized|initialized_percent|initialized_status|AudioStreaming|"
    r"ClockLocked|SampleRate|ClockSource|TotalDSPLoad|TotalPGMLoad|"
    r"TotalMEMLoad|DeviceConnectionMode|Session|ModifiedSessions|Snapshot|"
    r"PrimedSnapshot|Sleep|Dirty|ClockSourceListChanged)/value$|"
    r"^/openapollo/|"
    r"^/devices/0/DeviceOnline/value$|/meters/|/effects/")
_RE_PREAMP = re.compile(r"^/devices/0/inputs/\d+/preamps/")
# Never undone or redone: a 48V/pad flipped back under a microphone, a
# monitor level jumping the speakers, or a recall (which sets many values
# itself).  Same rule as the channel copy/paste.
_UNDO_SKIP = re.compile(
    r"/preamps/|/(CRMonitorLevel|CRMonitorLevelTapered)/value$|"
    r"^/(Snapshot|PrimedSnapshot)/value$|/Preset/value$")
_MISSING = object()
_RE_INPUT_STRIP = re.compile(
    r"^/devices/0/inputs/(\d+)/(FaderLevel|FaderLevelTapered|Pan|Pan2|Mute)"
    r"/value$")
_RE_OUTPUT_CTL = re.compile(r"^/devices/0/outputs/(\d+)/[^/]+/value$")
_MONITOR_LEVEL = re.compile(r"/(CRMonitorLevel|CRMonitorLevelTapered)/value$")


class CommandError(Exception):
    """A known command that failed: the reply is ERR_FAILED."""


class UnknownCommand(Exception):
    """No such command: the reply is ERR_UNRESOLVED."""


# ── Parameters ──────────────────────────────────────────────────────

def parse_cmdparams(value: Any, raw_params: dict | None = None) -> dict:
    """The command parameters from a post body and its query parameters.

    The body may be a dict (UBJSON, WS), a JSON object in a string (text
    TCP), or a bare scalar (the `value` key).  `cmdparams` in the query (a
    JSON string) or inside the body is merged on top.
    """
    params: dict = {}
    body = value
    if isinstance(body, (str, bytes)):
        text = body.decode("utf-8", "replace") if isinstance(body, bytes) \
            else body
        text = text.strip()
        if text[:1] in ("{", "["):
            try:
                body = json.loads(text)
            except ValueError:
                body = text
        else:
            body = text
    if isinstance(body, dict):
        params.update(body)
    elif body is not None and body != "":
        params["value"] = body
    extra = params.pop("cmdparams", None)
    query = (raw_params or {}).get("cmdparams")
    for chunk in (extra, query):
        if isinstance(chunk, str):
            try:
                chunk = json.loads(chunk)
            except ValueError:
                chunk = None
        if isinstance(chunk, dict):
            params.update(chunk)
    return params


def _bool(v: Any, default: bool = False) -> bool:
    if v is None:
        return default
    if isinstance(v, str):
        return v.strip().lower() in ("1", "true", "yes", "on")
    return bool(v)


def clean_name(value: Any, folders: bool = False) -> str:
    """A snapshot, preset or session name: 1..64 printable characters, no
    path separators, not starting with a dot.  `folders` allows Console's
    "[folder/]name" (each part checked the same way; snapshots are keys of
    one JSON file, never file names)."""
    if isinstance(value, bool) or not isinstance(value, (str, int, float)):
        raise CommandError("name")
    name = str(value).strip()
    parts = name.split("/") if folders else [name]
    if (not name or len(name) > NAME_MAX or "\\" in name
            or any(not p.strip() or p != p.strip() or p[0] == "." or "/" in p
                   for p in parts)
            or any(ord(c) < 32 or ord(c) == 127 for c in name)):
        raise CommandError("bad name")
    return name


def _arg(params: dict, *keys: str) -> Any:
    """The first of `keys` the post carries: Console 1.3.1's own key first,
    then the keys this daemon took before it (MAP.md "Commands")."""
    for key in keys:
        if key in params:
            return params[key]
    return None


def _index(value: Any, upper: int) -> int:
    """A move target: an integer clamped to 0..upper."""
    try:
        i = int(value)
    except (TypeError, ValueError):
        raise CommandError("index")
    return max(0, min(upper, i))


def _now() -> str:
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


class JsonFile:
    """A small JSON document on disk, written atomically."""

    def __init__(self, path: Path):
        self.path = path

    def load(self, default: dict) -> dict:
        try:
            with open(self.path) as f:
                data = json.load(f)
            return data if isinstance(data, dict) else default
        except FileNotFoundError:
            return default
        except (OSError, ValueError) as e:
            log.warning("%s: unreadable (%s), starting empty", self.path, e)
            return default

    def save(self, data: dict):
        try:
            self.path.parent.mkdir(parents=True, exist_ok=True)
            tmp = self.path.with_name(self.path.name + ".tmp")
            with open(tmp, "w") as f:
                json.dump(data, f, separators=(",", ":"))
            os.replace(tmp, self.path)
        except OSError as e:
            log.error("%s: cannot write: %s", self.path, e)
            raise CommandError("write")


# ── The engine ──────────────────────────────────────────────────────

class CommandEngine:
    """Runs `post` object commands against the daemon's state."""

    def __init__(self, daemon):
        self.d = daemon
        self.store_dir: Path | None = None
        self.stem = "device"
        self.scheduler: Callable | None = None   # (delay, fn) -> handle
        self.clock: Callable = time.monotonic    # tests drive it by hand
        self._clipboard: dict | None = None
        self._identify = None       # pending timer handle
        self._identify_end = 0.0
        self._identify_on = False
        self._identify_step = 0
        self._handlers: dict[str, Callable] = {
            "identify": self._identify_cmd,
            "clearclips": self._clear_clips,
            "clearsolos": self._clear_solos,
            "copychannel": self._copy_channel,
            "pastechannel": self._paste_channel,
            "copytosends": self._copy_to_sends,
            "saveassnapshot": self._snapshot_save,
            "movesnapshot": self._snapshot_move,
            "deletesnapshot": self._snapshot_delete,
            "exportsnapshots": self._snapshot_export,
            "importsnapshots": self._snapshot_import,
            "newsession": self._session_new,
            "opensession": self._session_open,
            "savesession": self._session_save,
            "savesessionas": self._session_save_as,
            "deletesession": self._session_delete,
            "renamesession": self._session_rename,
            "savelastusedsession": self._session_save_last_used,
            "saveaspreset": self._preset_save,
            "deletepreset": self._preset_delete,
            "movepreset": self._preset_move,
            "saveiomappreset": self._iomap_save,
            "deleteiomappreset": self._iomap_delete,
            "renameiomappreset": self._iomap_rename,
            "import": self._import,
            "undo": self._undo,
            "redo": self._redo,
        }
        # Undo history: batches {client, t, values: {path: [old, new]}}
        self._iomap_saved_names: list[str] = []
        self._undo_stack: list[dict] = []
        self._redo_stack: list[dict] = []
        self._applying = False

    @property
    def state(self):
        return self.d.state

    @property
    def router(self):
        return self.d.hw_router

    def set_store(self, directory: Path | str, stem: str):
        """Where snapshots, presets and sessions live: <directory>/<stem>.*"""
        self.store_dir = Path(directory)
        self.stem = stem
        self._publish_iomap_list()

    def _import(self, obj: str, params: dict):
        """Console's I/O preset LOAD (Import {"type": "IOMapPreset"}): the
        engine opens a file dialog on its own host and loads the preset file
        picked there.  This daemon has no dialog and no such file, so it
        refuses (and says why); its saved presets are recalled by setting
        /IOMapPreset, which Console does for them."""
        log.warning("Import %r refused: the Mixer Engine loads an I/O preset "
                    "file picked in a host-side file dialog, and this daemon "
                    "has none; set /IOMapPreset to recall a saved preset",
                    params.get("type"))
        raise CommandError("no host file dialog")

    # ── Dispatch ───────────────────────────────────────────────────

    @staticmethod
    def split_path(path: str) -> tuple[str, str]:
        """(object path, command name) of a post path."""
        p = path.rstrip("/")
        obj, _, name = p.rpartition("/")
        return (obj or "/"), name

    def is_known(self, name: str) -> bool:
        key = name.lower()
        return key in self._handlers or key in KNOWN_UNSUPPORTED

    def execute(self, path: str, value: Any = None,
                raw_params: dict | None = None) -> tuple[Any, str | None]:
        """Run the command a post path names.

        Returns (data, None) on success and (None, error text) on failure,
        the error text being the engine's.
        """
        obj, name = self.split_path(path)
        key = name.lower()
        if _LICENSING.search(name) or not self.is_known(name):
            log.debug("post %s: no such command", path)
            return None, ERR_UNRESOLVED
        handler = self._handlers.get(key)
        if handler is None:
            log.info("post %s: %s is not supported by this daemon", path, name)
            return None, ERR_FAILED
        try:
            params = parse_cmdparams(value, raw_params)
            data = handler(obj, params)
        except CommandError as e:
            log.info("post %s failed: %s", path, e)
            return None, ERR_FAILED
        except Exception:
            log.exception("post %s crashed", path)
            return None, ERR_FAILED
        log.info("post %s ok", path)
        return (True if data is None else data), None

    def value_of(self, path: str) -> Any:
        """The current value at a client's SET path, to hand to on_set(old=)
        before the set is applied (_MISSING when there is none)."""
        vp = path if path.endswith("/value") else path + "/value"
        if not self.state.path_exists(vp):
            return _MISSING
        return self.state.get_value(vp)

    def on_set(self, path: str, value: Any, old: Any = _MISSING,
               client: str | None = None):
        """A client set a property: it joins the undo history (when `old`,
        the value before the set, is given), and a Snapshot or a channel's
        Preset name that exists is recalled (the engine recalls on the
        property set)."""
        if old is not _MISSING:
            self._record(path, old, client)
        if not path.endswith(("/Snapshot", "/Snapshot/value",
                              "/Preset", "/Preset/value",
                              "/IOMapPreset", "/IOMapPreset/value")):
            return
        try:
            if path in self._IOMAP_PATHS:
                if isinstance(value, str) and value:
                    self._iomap_recall(value)
                return
            if path in ("/Snapshot/value", "/Snapshot"):
                if isinstance(value, str) and value in self._snapshots()[
                        "snapshots"]:
                    self._snapshot_recall(value)
                return
            m = _RE_PRESET_PATH.match(path if path.endswith("/value")
                                      else path + "/value")
            if m and isinstance(value, str) and value:
                self._preset_recall(m.group(1), int(m.group(2)), value)
        except CommandError as e:
            log.info("recall on %s=%r failed: %s", path, value, e)
        except Exception:
            log.exception("recall on %s=%r crashed", path, value)

    def has_channel_preset(self, path: str, value: Any) -> bool:
        """A SET of a channel's Preset naming one of this daemon's own
        channel presets (<state>/<device>.presets.json): recalled by
        on_set rather than by the Console preset library."""
        vp = path if path.endswith("/value") else path + "/value"
        m = _RE_PRESET_PATH.match(vp)
        if not m or not isinstance(value, str) or not value \
                or self.store_dir is None:
            return False
        try:
            items = self._presets()["kinds"].get(m.group(1), {})
        except Exception:
            return False
        return value in (items.get("items") or {})

    # ── Undo / Redo (U33) ──────────────────────────────────────────

    def _record(self, path: str, old: Any, client: str | None):
        if self._applying:
            return
        vp = path if path.endswith("/value") else path + "/value"
        if _VOLATILE.search(vp) or _UNDO_SKIP.search(vp):
            return
        if not self.state.path_exists(vp):
            return
        new = self.state.get_value(vp)
        if new == old:
            return          # rejected by the tree, or no change
        now = self.clock()
        top = self._undo_stack[-1] if self._undo_stack else None
        if (top is not None and top["client"] == client
                and now - top["t"] <= UNDO_GAP):
            top["t"] = now
            if vp in top["values"]:
                top["values"][vp][1] = new      # keep the first old value
            else:
                top["values"][vp] = [old, new]
        else:
            self._undo_stack.append(
                {"client": client, "t": now, "values": {vp: [old, new]}})
            del self._undo_stack[:-UNDO_MAX]
        self._redo_stack.clear()

    def clear_history(self):
        self._undo_stack.clear()
        self._redo_stack.clear()

    def _apply_batch(self, batch: dict, which: int) -> int:
        """Set every value of a batch (which 0 = old, 1 = new) through the
        normal set path, last change first when undoing."""
        n = 0
        items = list(batch["values"].items())
        if which == 0:
            items.reverse()
        self._applying = True
        try:
            for p, pair in items:
                if self.state.path_exists(p):
                    self.state.set(p, pair[which])
                    n += 1
        finally:
            self._applying = False
        return n

    def _undo(self, obj: str, params: dict):
        if not self._undo_stack:
            raise CommandError("nothing to undo")
        batch = self._undo_stack.pop()
        n = self._apply_batch(batch, 0)
        self._redo_stack.append(batch)
        del self._redo_stack[:-UNDO_MAX]
        return {"applied": n, "undo": len(self._undo_stack),
                "redo": len(self._redo_stack)}

    def _redo(self, obj: str, params: dict):
        if not self._redo_stack:
            raise CommandError("nothing to redo")
        batch = self._redo_stack.pop()
        n = self._apply_batch(batch, 1)
        self._undo_stack.append(batch)
        del self._undo_stack[:-UNDO_MAX]
        return {"applied": n, "undo": len(self._undo_stack),
                "redo": len(self._redo_stack)}

    def on_pause(self):
        """The device is being paused: an Identify blink ends now."""
        self.cancel_identify(write=True)

    # ── Identify (O20) ─────────────────────────────────────────────

    def _schedule(self, delay: float, fn: Callable):
        if self.scheduler is not None:
            return self.scheduler(delay, fn)
        try:
            return asyncio.get_running_loop().call_later(delay, fn)
        except RuntimeError:
            t = threading.Timer(delay, fn)
            t.daemon = True
            t.start()
            return t

    @staticmethod
    def _cancel(handle):
        if handle is not None:
            try:
                handle.cancel()
            except Exception:
                pass

    def _write_identify(self, value: int) -> bool:
        router = self.router
        if router is None or router.paused:
            return False
        self._identify_on = bool(value)
        # OP 0x1d through the router's monitor-param path; the driver
        # masks the value to 3 bits
        set_identify = getattr(router, "set_identify", None)
        if callable(set_identify):
            set_identify(value)
        else:
            router.on_set(IDENTIFY_PATH, 1 if value else 0)
        return True

    def _identify_cmd(self, obj: str, params: dict):
        """post /Identify: the Mixer Engine's red/green sequence once, or
        repeated for {"seconds": N}; {"blink": false} holds red for N s;
        {"off": true} stops."""
        router = self.router
        if router is None or not router.backend.connected or router.paused:
            raise CommandError("no device")
        if "off" in params and _bool(params["off"]):
            self.cancel_identify(write=True)
            return None
        try:
            secs = float(params.get("seconds", IDENTIFY_SEQUENCE_SECONDS))
        except (TypeError, ValueError):
            raise CommandError("seconds")
        self.cancel_identify(write=False)
        if not _bool(params.get("blink"), True):
            secs = max(0.5, min(IDENTIFY_MAX_SECONDS, secs))
            self._identify_end = self.clock() + secs
            self._write_identify(1)
            self._identify = self._schedule(secs, self._identify_tick)
            self._identify_step = len(IDENTIFY_SEQUENCE)
            return None
        # at least one whole sequence, as the Mixer Engine always runs it
        secs = max(IDENTIFY_SEQUENCE_SECONDS,
                   min(IDENTIFY_MAX_SECONDS, secs))
        self._identify_end = self.clock() + secs
        self._identify_step = 0
        self._identify_tick()
        return None

    def _identify_tick(self):
        self._identify = None
        if self.clock() >= self._identify_end - 1e-3:
            if self._identify_on:
                self._write_identify(0)
            return
        if self._identify_step >= len(IDENTIFY_SEQUENCE):
            self._identify_step = 0
        value, ms = IDENTIFY_SEQUENCE[self._identify_step]
        self._identify_step += 1
        self._write_identify(value)
        self._identify = self._schedule(ms / 1000.0, self._identify_tick)

    def cancel_identify(self, write: bool = True):
        self._cancel(self._identify)
        self._identify = None
        if write and self._identify_on:
            self._write_identify(False)
        self._identify_on = False if write else self._identify_on

    # ── ClearClips / ClearSolos ────────────────────────────────────

    @staticmethod
    def _clear_scope(params: dict) -> str | None:
        """Console's `{"path": <object>}` limits ClearClips/ClearSolos to
        one strip (MxClnt_MixerData::ClearClips); none means every one."""
        scope = params.get("path")
        if scope is None:
            return None
        if not isinstance(scope, str) or not scope.startswith("/"):
            raise CommandError("path must be an object path")
        return scope.rstrip("/") + "/"

    def _clear_clips(self, obj: str, params: dict):
        d = self.d
        scope = self._clear_scope(params)
        clear = getattr(d._metering, "clear_clips", None)
        if callable(clear) and scope is None:
            clear()
        # clip holds kept as tree properties (software model, sessions)
        tree = self.state
        for p in tree.all_paths():
            if p.endswith("/MeterClip") and (scope is None
                                             or p.startswith(scope)):
                if tree.get_value(p + "/value"):
                    tree.set(p + "/value", False, hardware=False)
        count = 0
        for path in list(d._last_meter):
            if scope is not None and not path.startswith(scope):
                continue
            if path.endswith("/Clip/value") or path.endswith(
                    "/MeterClip/value"):
                if d._last_meter.get(path):
                    self.state._notify_subscribers(path, False)
                    count += 1
                d._last_meter[path] = False
            elif path.endswith("/MeterPeakLevel/value"):
                d._last_meter.pop(path, None)   # pushed again at the next tick
        log.info("ClearClips: %d clip indicators cleared", count)
        return None

    def _clear_solos(self, obj: str, params: dict):
        tree = self.state
        scope = self._clear_scope(params)
        paths = [p for p in tree.all_paths()
                 if (p.endswith("/Solo") or p.endswith("/SurroundSolo"))
                 and p.startswith(scope or "/devices/0/")]
        n = 0
        for p in paths:
            vp = p + "/value"
            if tree.get_value(vp):
                # state.set -> router.on_set; the Solo branch runs the
                # solo recalculation
                tree.set(vp, False)
                n += 1
        log.info("ClearSolos: %d solos cleared", n)
        return None

    # ── Channel values: copy / paste / presets / snapshots ─────────

    def _node(self, path: str):
        node, prop = self.state._resolve_path(path)
        if node is None or prop is not None:
            raise CommandError(f"no object {path}")
        return node

    def _collect(self, base: str, skip_names=_CHANNEL_SKIP_NAMES) -> dict:
        """{relative path: value} of the writable properties under `base`."""
        out: dict = {}

        def walk(prefix: str, node: dict):
            for name, meta in node.get("properties", {}).items():
                if (meta.get("readonly") or "value" not in meta
                        or name in skip_names):
                    continue
                out[prefix + name] = meta["value"]
            for cname, child in node.get("children", {}).items():
                if cname in _CHANNEL_SKIP_CHILDREN:
                    continue
                walk(f"{prefix}{cname}/", child)

        walk("", self._node(base))
        return out

    def _apply_rel(self, base: str, values: dict) -> int:
        """Set relative values on a channel through the normal set path."""
        n = 0
        for rel, v in values.items():
            p = f"{base}/{rel}/value"
            if self.state.path_exists(p):
                self.state.set(p, v)
                n += 1
        return n

    def _channel(self, obj: str) -> tuple[str, str, int]:
        m = _RE_CHANNEL.match(obj.rstrip("/"))
        if not m:
            raise CommandError(f"not a channel: {obj}")
        base = obj.rstrip("/")
        self._node(base)
        return base, m.group(1), int(m.group(2))

    def _copy_channel(self, obj: str, params: dict):
        base, kind, _ = self._channel(obj)
        self._clipboard = {"kind": kind, "from": base,
                           "values": self._collect(base)}
        return None

    def _paste_channel(self, obj: str, params: dict):
        clip = self._clipboard
        if not clip:
            raise CommandError("nothing copied")
        targets = params.get("targets")
        if targets is None:
            targets = [obj]
        elif not isinstance(targets, list):
            raise CommandError("targets")
        done = 0
        for t in targets:
            base, kind, _ = self._channel(str(t))
            if kind != clip["kind"]:
                raise CommandError("kind mismatch")
            done += self._apply_rel(base, clip["values"])
        return {"applied": done}

    def _set_send_gain(self, send_base: str, db) -> None:
        """A send's Gain (to the hardware) and its GainTapered companion
        (tree only), so a client's send knob follows."""
        from hardware import fader_db_to_tapered
        tree = self.state
        tree.set(f"{send_base}/Gain/value", db)
        tapered = f"{send_base}/GainTapered/value"
        if tree.path_exists(tapered):
            try:
                tree.set(tapered, fader_db_to_tapered(float(db)),
                         hardware=False)
            except (TypeError, ValueError):
                pass

    def _copy_to_sends(self, obj: str, params: dict):
        """Copy a channel's fader and pan to its sends (a cue mix that
        starts as the main mix).  `sends`: the send indexes, default all.

        On the root, Console's form `{"send": i}` (MAP.md, CopyToSends): for
        every input but the talkback mic, send i gets the input's FaderLevel
        and, on a mono input, its Pan."""
        if obj in ("", "/"):
            return self._copy_mix_to_send(params)
        base, kind, _ = self._channel(obj)
        if kind not in ("inputs", "auxs"):
            raise CommandError("no sends")
        tree = self.state
        fader = tree.get_value(f"{base}/FaderLevel/value")
        pan = tree.get_value(f"{base}/Pan/value")
        node = self._node(base).get("children", {}).get("sends")
        if node is None:
            raise CommandError("no sends")
        wanted = params.get("sends")
        n = 0
        for idx in sorted(node.get("children", {}), key=int):
            if wanted is not None and int(idx) not in [int(x) for x in wanted]:
                continue
            sp = f"{base}/sends/{idx}"
            if fader is not None and tree.path_exists(f"{sp}/Gain/value"):
                self._set_send_gain(sp, fader)
                n += 1
            if pan is not None and tree.path_exists(f"{sp}/Pan/value"):
                tree.set(f"{sp}/Pan/value", pan)
        return {"sends": n}

    def _copy_mix_to_send(self, params: dict):
        try:
            send = int(params.get("send"))
        except (TypeError, ValueError):
            raise CommandError("CopyToSends needs a send index")
        tree = self.state
        n = 0
        ch = 0
        while tree.path_exists(f"/devices/0/inputs/{ch}/Name/value"):
            base = f"/devices/0/inputs/{ch}"
            ch += 1
            if tree.get_value(f"{base}/IOType/value") == "TalkbackMic":
                continue
            sp = f"{base}/sends/{send}"
            fader = tree.get_value(f"{base}/FaderLevel/value")
            if fader is None or not tree.path_exists(f"{sp}/Gain/value"):
                continue
            self._set_send_gain(sp, fader)
            n += 1
            pan = tree.get_value(f"{base}/Pan/value")
            if pan is not None and not tree.get_value(f"{base}/Stereo/value") \
                    and tree.path_exists(f"{sp}/Pan/value"):
                tree.set(f"{sp}/Pan/value", pan)
        if not n:
            raise CommandError(f"no send {send}")
        return {"sends": n}

    # ── Persistent stores ──────────────────────────────────────────

    def _file(self, suffix: str) -> JsonFile:
        if self.store_dir is None:
            raise CommandError("no state directory")
        return JsonFile(self.store_dir / f"{self.stem}{suffix}")

    def _export_file(self, name: Any) -> Path:
        if self.store_dir is None:
            raise CommandError("no state directory")
        if (not isinstance(name, str) or not name or name[0] == "."
                or "/" in name or "\\" in name or "\0" in name
                or len(name) > 100):
            raise CommandError("file must be a plain file name")
        return self.store_dir / f"{self.stem}.exports" / name

    # snapshots ......................................................

    def _snapshots(self) -> dict:
        data = self._file(".snapshots.json").load({})
        snaps = data.get("snapshots")
        order = data.get("order")
        if not isinstance(snaps, dict):
            snaps = {}
        order = [n for n in (order if isinstance(order, list) else [])
                 if n in snaps]
        order += [n for n in snaps if n not in order]
        return {"format": FORMAT_SNAPSHOTS, "version": 1,
                "order": order, "snapshots": snaps}

    def _snapshots_save(self, data: dict):
        self._file(".snapshots.json").save(data)

    def _snapshot_values(self) -> dict:
        """{absolute /value path: value} of every channel (preamps, meters
        and the monitor level excluded)."""
        out: dict = {}
        root = self._node("/devices/0")
        for kind in _KINDS:
            for idx in root.get("children", {}).get(kind, {}).get(
                    "children", {}):
                base = f"/devices/0/{kind}/{idx}"
                for rel, v in self._collect(
                        base, _SNAPSHOT_SKIP_NAMES).items():
                    out[f"{base}/{rel}/value"] = v
        return out

    def _publish_snapshot_name(self, name: str):
        for prop in ("/Snapshot", "/PrimedSnapshot"):
            if self.state.path_exists(prop + "/value"):
                self.state.set(prop + "/value", name)

    def _snapshot_save(self, obj: str, params: dict):
        # Console: {"snapshotName", "isNew"}; isNew only tells its list
        # whether the row is new, a save of an existing name replaces it
        name = clean_name(_arg(params, "snapshotName", "name", "value"),
                          folders=True)
        data = self._snapshots()
        data["snapshots"][name] = {"saved": _now(),
                                   "values": self._snapshot_values()}
        if name not in data["order"]:
            data["order"].append(name)
        self._snapshots_save(data)
        self._publish_snapshot_name(name)
        return {"name": name, "snapshots": data["order"]}

    def _snapshot_recall(self, name: str) -> int:
        snap = self._snapshots()["snapshots"].get(name)
        if not snap:
            raise CommandError("no such snapshot")
        n = 0
        with self._session_load():
            for p, v in snap.get("values", {}).items():
                if self.state.path_exists(p):
                    self.state.set(p, v)
                    n += 1
        self._session_clean()       # as CMxRoot::setSnapshot
        log.info("Snapshot %r recalled: %d values", name, n)
        return n

    def _session_clean(self):
        """/Dirty back to False: a session or snapshot was loaded, created
        or saved (session_dirty)."""
        dirty = getattr(self.d, "session_dirty", None)
        if dirty is not None:
            dirty.clear()

    def _session_load(self):
        """The sets of a load are not edits (no /Dirty blip before the
        clear that follows it)."""
        dirty = getattr(self.d, "session_dirty", None)
        return dirty.suppress() if dirty is not None else nullcontext()

    def _snapshot_move(self, obj: str, params: dict):
        data = self._snapshots()
        if "origName" in params:
            # Console: {"origName", "newName", "shuffle"}: a rename (and
            # folder move) that keeps the snapshot's place in the list;
            # `shuffle` only orders Console's own list (ponytail: ignored)
            old = clean_name(params["origName"], folders=True)
            new = clean_name(params.get("newName"), folders=True)
            if old not in data["snapshots"]:
                raise CommandError("no such snapshot")
            if new != old:
                if new in data["snapshots"]:
                    raise CommandError("name taken")
                data["snapshots"][new] = data["snapshots"].pop(old)
                data["order"][data["order"].index(old)] = new
                self._snapshots_save(data)
                for prop in ("/Snapshot", "/PrimedSnapshot"):
                    if self.state.get_value(prop + "/value") == old:
                        self.state.set(prop + "/value", new)
            return {"name": new, "snapshots": data["order"]}
        name = clean_name(params.get("name"), folders=True)
        if name not in data["snapshots"]:
            raise CommandError("no such snapshot")
        order = data["order"]
        order.remove(name)
        order.insert(_index(params.get("index", params.get("to")),
                            len(order)), name)
        self._snapshots_save(data)
        return {"snapshots": order}

    def _snapshot_delete(self, obj: str, params: dict):
        name = clean_name(_arg(params, "snapshotName", "name", "value"),
                          folders=True)
        data = self._snapshots()
        if name not in data["snapshots"]:
            raise CommandError("no such snapshot")
        del data["snapshots"][name]
        data["order"].remove(name)
        self._snapshots_save(data)
        for prop in ("/Snapshot", "/PrimedSnapshot"):
            if self.state.get_value(prop + "/value") == name:
                self.state.set(prop + "/value", "")
        return {"snapshots": data["order"]}

    def _snapshot_export(self, obj: str, params: dict):
        data = self._snapshots()
        names = params.get("names")
        if names is not None:
            if not isinstance(names, list):
                raise CommandError("names")
            data["order"] = [n for n in data["order"] if n in names]
            data["snapshots"] = {n: data["snapshots"][n]
                                 for n in data["order"]}
        if "file" in params:
            JsonFile(self._export_file(params["file"])).save(data)
            return {"file": params["file"], "count": len(data["order"])}
        return data

    def _snapshot_import(self, obj: str, params: dict):
        if "file" in params:
            src = JsonFile(self._export_file(params["file"])).load({})
        else:
            src = params.get("snapshots_data", params)
        if (not isinstance(src, dict)
                or src.get("format") != FORMAT_SNAPSHOTS
                or not isinstance(src.get("snapshots"), dict)):
            raise CommandError("not a snapshot export")
        data = self._snapshots()
        overwrite = _bool(params.get("overwrite"), True)
        added = 0
        for name in src.get("order") or list(src["snapshots"]):
            snap = src["snapshots"].get(name)
            try:
                clean_name(name)
            except CommandError:
                continue
            if (not isinstance(snap, dict)
                    or not isinstance(snap.get("values"), dict)):
                continue
            if name in data["snapshots"] and not overwrite:
                continue
            if name not in data["snapshots"]:
                data["order"].append(name)
            data["snapshots"][name] = {
                "saved": str(snap.get("saved", _now())),
                "values": {str(k): v for k, v in snap["values"].items()
                           if isinstance(k, str) and k.startswith("/devices/")}}
            added += 1
        self._snapshots_save(data)
        return {"imported": added, "snapshots": data["order"]}

    # presets ........................................................

    def _presets(self) -> dict:
        data = self._file(".presets.json").load({})
        kinds = data.get("kinds")
        return {"format": FORMAT_PRESETS, "version": 1,
                "kinds": kinds if isinstance(kinds, dict) else {}}

    def _preset_kind(self, obj: str, params: dict) -> tuple[str, str | None]:
        """(kind, channel base or None) from the object path or `kind`."""
        m = _RE_CHANNEL.match(obj.rstrip("/"))
        if m:
            return m.group(1), obj.rstrip("/")
        kind = params.get("kind")
        if kind in _KINDS:
            return kind, None
        raise CommandError("preset needs a channel")

    @staticmethod
    def _kind_bucket(data: dict, kind: str) -> dict:
        b = data["kinds"].setdefault(kind, {})
        b.setdefault("items", {})
        b["order"] = [n for n in b.get("order", []) if n in b["items"]]
        b["order"] += [n for n in b["items"] if n not in b["order"]]
        return b

    def _preset_save(self, obj: str, params: dict):
        kind, base = self._preset_kind(obj, params)
        if base is None:
            raise CommandError("preset needs a channel")
        name = clean_name(params.get("name", params.get("value")))
        data = self._presets()
        b = self._kind_bucket(data, kind)
        b["items"][name] = {"saved": _now(), "values": self._collect(base)}
        if name not in b["order"]:
            b["order"].append(name)
        self._file(".presets.json").save(data)
        if self.state.path_exists(base + "/Preset/value"):
            self.state.set(base + "/Preset/value", name)
        return {"name": name, "presets": b["order"]}

    def _preset_recall(self, kind: str, idx: int, name: str):
        b = self._presets()["kinds"].get(kind, {}).get("items", {})
        item = b.get(name)
        if not item:
            return
        base = f"/devices/0/{kind}/{idx}"
        n = self._apply_rel(base, item.get("values", {}))
        log.info("Preset %r applied to %s: %d values", name, base, n)

    def _preset_delete(self, obj: str, params: dict):
        kind, base = self._preset_kind(obj, params)
        name = clean_name(params.get("name", params.get("value")))
        data = self._presets()
        b = self._kind_bucket(data, kind)
        if name not in b["items"]:
            raise CommandError("no such preset")
        del b["items"][name]
        b["order"].remove(name)
        self._file(".presets.json").save(data)
        return {"presets": b["order"]}

    def _preset_move(self, obj: str, params: dict):
        kind, base = self._preset_kind(obj, params)
        name = clean_name(params.get("name"))
        data = self._presets()
        b = self._kind_bucket(data, kind)
        if name not in b["items"]:
            raise CommandError("no such preset")
        b["order"].remove(name)
        b["order"].insert(_index(params.get("index", params.get("to")),
                                 len(b["order"])), name)
        self._file(".presets.json").save(data)
        return {"presets": b["order"]}

    # I/O map presets ................................................

    _IOMAP_ROOTS = ("inputroutes", "outputroutes", "asioinputs",
                    "asiooutputs")
    _IOMAP_PATHS = ("/IOMapPreset", "/IOMapPreset/value")

    def _iomaps(self) -> dict:
        data = self._file(".iomaps.json").load({})
        presets = data.get("presets")
        order = data.get("order")
        if not isinstance(presets, dict):
            presets = {}
        order = [n for n in (order if isinstance(order, list) else [])
                 if n in presets]
        order += [n for n in presets if n not in order]
        return {"format": FORMAT_IOMAPS, "version": 1,
                "order": order, "presets": presets}

    def _iomaps_save(self, data: dict):
        self._file(".iomaps.json").save(data)

    def _iomap_values(self) -> dict:
        """{absolute /value path: value} of the I/O routes (not their
        Type)."""
        out: dict = {}
        for root in self._IOMAP_ROOTS:
            if not self.state.path_exists(f"/{root}"):
                continue
            try:
                rel = self._collect(f"/{root}", frozenset({"Type"}))
            except CommandError:
                continue
            for r, v in rel.items():
                out[f"/{root}/{r}/value"] = v
        return out

    def _publish_iomap_list(self, current: str | None = None):
        """Put the daemon's saved presets after the map's own in the
        /IOMapPreset list (and point the property at `current`)."""
        if not self.state.path_exists("/IOMapPreset/value"):
            return
        try:
            saved = self._iomaps()["order"]
        except CommandError:
            saved = []
        prop = self.state.get_prop_dict("/IOMapPreset") or {}
        own = set(self._iomap_saved_names)
        base = [v for v in (prop.get("values") or [])
                if not (isinstance(v, dict) and v.get("value") in own)]
        entries = [{"value": n, "type": "file", "locked": False}
                   for n in saved]
        self._iomap_saved_names = list(saved)
        self.state.update_property("/IOMapPreset", {"values": base + entries})
        if current is not None:
            self.state.set("/IOMapPreset/value", current, hardware=False)
            if self.state.path_exists("/IOMapPresetDirty/value"):
                self.state.set("/IOMapPresetDirty/value", False,
                               hardware=False)

    def _iomap_save(self, obj: str, params: dict):
        name = clean_name(_arg(params, "ioPresetName", "name", "value"))
        data = self._iomaps()
        data["presets"][name] = {"saved": _now(),
                                 "values": self._iomap_values()}
        if name not in data["order"]:
            data["order"].append(name)
        self._iomaps_save(data)
        self._publish_iomap_list(name)
        return {"name": name, "presets": data["order"]}

    def _iomap_delete(self, obj: str, params: dict):
        name = clean_name(_arg(params, "ioPresetName", "name", "value"))
        data = self._iomaps()
        if name not in data["presets"]:
            raise CommandError("no such preset")
        del data["presets"][name]
        data["order"].remove(name)
        self._iomaps_save(data)
        cur = self.state.get_value("/IOMapPreset/value")
        self._publish_iomap_list("" if cur == name else None)
        return {"presets": data["order"]}

    def _iomap_rename(self, obj: str, params: dict):
        old = clean_name(_arg(params, "origPreset", "name", "from"))
        new = clean_name(_arg(params, "newPreset", "new_name", "to"))
        data = self._iomaps()
        if old not in data["presets"]:
            raise CommandError("no such preset")
        if new in data["presets"] and new != old:
            raise CommandError("name taken")
        if new != old:
            data["presets"][new] = data["presets"].pop(old)
            data["order"][data["order"].index(old)] = new
            self._iomaps_save(data)
        cur = self.state.get_value("/IOMapPreset/value")
        self._publish_iomap_list(new if cur == old else None)
        return {"name": new, "presets": data["order"]}

    def _iomap_recall(self, name: str):
        item = self._iomaps()["presets"].get(name)
        if not isinstance(item, dict):
            return      # one of the map's own presets: nothing stored
        n = 0
        for p, v in (item.get("values") or {}).items():
            if (isinstance(p, str) and self.state.path_exists(p)
                    and p.split("/")[1] in self._IOMAP_ROOTS):
                self.state.set(p, v)
                n += 1
        log.info("I/O map preset %r applied: %d values", name, n)

    # sessions .......................................................

    def _session_dir(self) -> Path:
        if self.store_dir is None:
            raise CommandError("no state directory")
        return self.store_dir / f"{self.stem}.sessions"

    def _session_file(self, name: str) -> JsonFile:
        return JsonFile(self._session_dir()
                        / (quote(name, safe=" -_.()") + ".session.json"))

    def session_names(self) -> list[str]:
        try:
            files = sorted(self._session_dir().glob("*.session.json"))
        except OSError:
            return []
        return [unquote(f.name[:-len(".session.json")]) for f in files
                if not f.name.startswith(".")]

    def _session_values(self) -> dict:
        return {p: v for p, v in self.state._dirty.items()
                if p.endswith("/value") and not _VOLATILE.search(p)}

    def _write_session(self, file: JsonFile, name: str):
        file.save({"format": FORMAT_SESSION, "version": 1, "name": name,
                   "saved": _now(), "device": self.state.device_name,
                   "values": self._session_values()})

    def _current_session(self) -> str:
        cur = self.state.get_value("/Session/value")
        if isinstance(cur, str) and cur.strip():
            # a map may carry a path from another machine: keep its last part
            cur = cur.replace("\\", "/").rstrip("/").rsplit("/", 1)[-1]
            return clean_name(cur)
        raise CommandError("no current session")

    def _publish_session(self, name: str):
        if self.state.path_exists("/Session/value"):
            self.state.set("/Session/value", name)

    def _session_save_as(self, obj: str, params: dict):
        name = clean_name(params.get("name", params.get("value")))
        self._write_session(self._session_file(name), name)
        self._publish_session(name)
        self._session_clean()       # SessionFileManager::Save, once written
        return {"name": name, "sessions": self.session_names()}

    def _session_save(self, obj: str, params: dict):
        if "name" in params:
            return self._session_save_as(obj, params)
        name = self._current_session()
        self._write_session(self._session_file(name), name)
        self._publish_session(name)
        self._session_clean()
        return {"name": name, "sessions": self.session_names()}

    def _session_save_last_used(self, obj: str, params: dict):
        # The engine reloads this one at its next start.  The daemon
        # restores its state file at start: write it now rather than at the
        # end of the save debounce (Console sends this on its way out), and
        # keep a session-format copy for tools.  /Dirty stays as it is
        # (SessionFileManager::SaveLastUsedSession does not touch it).
        self.state.save_now()
        self._write_session(JsonFile(self._session_dir()
                                     / ".last_used.session.json"),
                            "last used")
        return None

    def _session_delete(self, obj: str, params: dict):
        name = clean_name(params.get("name", params.get("value")))
        f = self._session_file(name)
        if not f.path.exists():
            raise CommandError("no such session")
        try:
            f.path.unlink()
        except OSError:
            raise CommandError("delete")
        return {"sessions": self.session_names()}

    def _session_rename(self, obj: str, params: dict):
        old = clean_name(_arg(params, "origName", "name", "from"))
        new = clean_name(_arg(params, "newName", "new_name", "to"))
        src = self._session_file(old)
        if not src.path.exists():
            raise CommandError("no such session")
        dst = self._session_file(new)
        if dst.path.exists() and new != old:
            raise CommandError("name taken")
        data = src.load({})
        data["name"] = new
        dst.save(data)
        if new != old:
            src.path.unlink()
        cur = self.state.get_value("/Session/value")
        if isinstance(cur, str) and cur.replace("\\", "/").rsplit(
                "/", 1)[-1] == old:
            self._publish_session(new)
        return {"name": new, "sessions": self.session_names()}

    def _session_new(self, obj: str, params: dict):
        """Back to the device map's values (every change made since), except
        the preamps and the monitor level, which stay the hardware's."""
        tree = self.state
        n = 0
        with self._session_load():
            for p, base in list(tree._base.items()):
                if (not p.endswith("/value") or _VOLATILE.search(p)
                        or _RE_PREAMP.match(p) or _MONITOR_LEVEL.search(p)):
                    continue
                if tree.get_value(p) != base:
                    tree.set(p, base)
                    n += 1
        name = ""
        if "name" in params:
            name = clean_name(params["name"])
        self._publish_session(name)
        self.clear_history()
        self._session_clean()       # CreateMixerFromFactoryDefault
        log.info("NewSession: %d controls back to their defaults", n)
        return {"name": name, "reset": n}

    def _session_open(self, obj: str, params: dict):
        name = clean_name(params.get("name", params.get("value")))
        f = self._session_file(name)
        if not f.path.exists():
            raise CommandError("no such session")
        data = f.load({})
        if (data.get("format") != FORMAT_SESSION
                or not isinstance(data.get("values"), dict)):
            raise CommandError("not a session file")
        values = {p: v for p, v in data["values"].items()
                  if isinstance(p, str) and p.endswith("/value")
                  and not _VOLATILE.search(p)}
        with self._session_load():
            n = self.apply_values(values)
        self._publish_session(name)
        self.clear_history()
        self._session_clean()       # LoadXml / SessionFileManager::Open
        return {"name": name, "applied": n}

    def apply_values(self, values: dict) -> int:
        """Load saved values: into the tree (subscribers are told), then to
        the hardware in the init order: preamps and monitor (which a
        daemon start keeps from the readback, but a session sets), input
        faders, pans and mutes, then the A6 re-apply for what the hardware
        cannot report back (sends, aux, outputs, solo)."""
        tree = self.state
        applied: dict = {}
        for p, v in values.items():
            if tree.path_exists(p) and tree.set(p, v, hardware=False):
                applied[p] = tree.get_value(p)
        router = self.router
        if router is None or router.paused:
            return len(applied)
        mon = router.monitor_output_index()
        preamp = [p for p in applied if _RE_PREAMP.match(p)]
        monitor = [p for p in applied
                   if (m := _RE_OUTPUT_CTL.match(p)) and mon is not None
                   and int(m.group(1)) == mon]
        strips = [p for p in applied if _RE_INPUT_STRIP.match(p)]
        for p in preamp + monitor:
            router.on_set(p, applied[p])
        for p in strips:
            router.on_set(p, applied[p])
        router.reapply_saved_state(dict(applied))
        return len(applied)
