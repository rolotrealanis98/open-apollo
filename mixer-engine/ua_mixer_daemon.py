#!/usr/bin/env python3
"""
ua-mixer-daemon — Linux mixer engine for Universal Audio Apollo.

Serves two TCP protocols:
  - TCP:4710 — UA Mixer Engine (null-terminated JSON text, for ConsoleLink)
  - TCP:4720 — UA Mixer Helper (text commands in, UBJS-framed UBJSON out,
               for UAD Console)

Optionally serves WebSocket for UA Connect on a separate port (default 4721).

Usage:
    # Run with default device map (Apollo x4)
    ./ua_mixer_daemon.py

    # Specify device map and port
    ./ua_mixer_daemon.py --device-map device_maps/device_map_apollo_x4.json --port 4710

    # Run without hardware (software-only mode, for testing on Mac)
    ./ua_mixer_daemon.py --no-hardware

    # Verbose logging
    ./ua_mixer_daemon.py -v
"""

import argparse
import asyncio
import importlib.util
import json
import logging
import os
import re as re_mod
import shutil
import sys
import time
from pathlib import Path

from protocol import (Command, parse_command, encode_response_bytes,
                      encode_error_bytes, _parse_path_with_params)
from state_tree import StateTree
from helper_tree import HelperTree
from autogain import AutoGainService
from events import (DriverEvents, OFFLINE as EVENTS_OFFLINE,
                    UNSUPPORTED as EVENTS_UNSUPPORTED, EVENT_NAMES,
                    UA_EVT_RATE_INFO, UA_EVT_IODESC_IN, UA_EVT_IODESC_OUT,
                    UA_EVT_RATE_CHANGED, UA_EVT_CLOCK_CHANGED,
                    UA_EVT_NOTIF_STATUS, UA_EVT_DEVICE_OFFLINE,
                    UA_EVT_CONNECTED, UA_EVT_LAYOUT_CHANGED,
                    UA_EVT_EVENTS_LOST, UA_EVT_CLOCK_LOCK,
                    UA_EVT_PAUSED, UA_EVT_RESUMED, UA_PAUSE_DEVICE)
from model_info import ModelInfo, monitor_param_allowed
from hardware import (CLOCK_CONTROL_PATHS, CLOCK_SOURCE_MAP,
                      DRIVER_PARAM_CLOCK_SOURCE,
                      DRIVER_PARAM_SAMPLE_RATE, DRIVER_PARAM_TRANSPORT_RUNNING,
                      HardwareBackend, HardwareRouter, clock_source_names,
                      device_clock_menu,
                      fader_db_to_tapered, normalize_global,
                      pcie_link_below_minimum,
                      pcie_link_text, preamp_db_to_tapered)
from metering import (NullMeter, ReadbackMeter, SILENCE_DB, find_apollo_card,
                      make_meter,
                      meter_channel)
from bonjour import BonjourAnnouncer, _default_name
from model_features import (LINES34_HP_CLASSES, MAX_DELAY_COMP_SAMPLES,
                            latency_figures, load_descriptor)
from commands import CommandEngine, parse_cmdparams, refused_set
from session_dirty import SessionDirty
from ubjson_codec import encode_response as ubjson_encode_response
from json_frame_codec import JsonFrameError, encode_frame as json_encode_frame
from wire_command_framer import WireCommandFramer, WireError

try:
    from ws_server import WsServer, HAS_WEBSOCKETS
except ImportError:
    HAS_WEBSOCKETS = False
    WsServer = None

try:    # USB Apollos (needs ../tools/uausb; pyusb only when a device opens)
    import hardware_usb
except ImportError:
    hardware_usb = None

log = logging.getLogger("ua-mixer-daemon")

# Runtime-only state properties that the real mixer engine provides
# but aren't in the captured device map JSON.
RUNTIME_PROPERTIES = {
    # Init state — not in device map, provided dynamically by real mixer engine
    "/initialized": {"type": "bool", "value": False},
    "/initialized_percent": {"type": "float", "min": 0.0, "max": 100.0, "value": 0.0},
    "/initialized_status": {"type": "string", "value": "Loading..."},
    "/status/timeout_ms": {"type": "int", "value": 5000},
    "/Sleep": {"type": "bool", "value": False},
    "/errors": {"type": "string", "value": ""},
    "/Dirty": {"type": "bool", "value": False},
    "/Session": {"type": "string", "value": ""},
    "/Progress": {"type": "string", "value": ""},
    # Properties not in device map but needed by UAD Console
    "/BufferSize": {"type": "int", "value": 256, "values": [
        {"value": 32}, {"value": 64}, {"value": 128}, {"value": 256},
        {"value": 512}, {"value": 1024}, {"value": 2048}]},
    "/BufferSizeListChanged": {"type": "bool", "value": False},
    "/USBStreamingMode": {"type": "string", "value": "", "values": []},
    "/USBStreamingModeListChanged": {"type": "bool", "value": False},
    # Forced runtime values (override device map defaults)
    "/devices/0/DeviceOnline": {"type": "bool", "value": True},
    "/DeviceConnectionMode": {"type": "string", "value": "TB"},
    "/ClockLocked": {"type": "bool", "value": True},
    "/AudioStreaming": {"type": "bool", "value": True},
    "/TotalDSPLoad": {"type": "float", "value": 0.0},
    "/TotalPGMLoad": {"type": "float", "value": 0.0},
    "/TotalMEMLoad": {"type": "float", "value": 0.0},
    # Dante paths (UAD Console queries these even for non-Dante devices)
    "/dante/primary_network_adapter": {"type": "string", "value": ""},
    "/dante/available_network_adapters": {"type": "string", "value": ""},
    "/dante/available_collections": {"type": "string", "value": ""},
    "/dante/available_presets": {"type": "string", "value": ""},
    "/dante/devices/device_list": {"type": "string", "value": ""},
    # Root ints in the Mixer Engine (kPropDanteDevicesStatus 0x166,
    # kPropDanteErrorStatus 0x170, set by CMxMixer::updateDanteInfo):
    # DevicesStatus 0 = no Dante device online (1 online, -1 another
    # device Dante-locked, -2 AES67 active); ErrorStatus is a bit mask,
    # 0 = no error.  A model without Dante (all but the x16D) stays at 0.
    "/DanteDevicesStatus": {"type": "int", "value": 0},
    "/DanteErrorStatus": {"type": "int", "value": 0},
    "/CorrectionLicenseState": {"type": "int", "value": 0},
    # open-apollo's own, read-only: the PCIe/Thunderbolt link the driver
    # negotiated (UA_FEAT_SYSINFO_EXT).  Empty/0 until the device reports it.
    "/openapollo/pcie_link": {"type": "string", "value": "",
                              "readonly": True},
    "/openapollo/pcie_link_speed": {"type": "int", "value": 0,
                                    "readonly": True},
    "/openapollo/pcie_link_width": {"type": "int", "value": 0,
                                    "readonly": True},
    # open-apollo's own, read-only: the converter latency of the attached
    # model at the current rate and analog board (devices/*.json latency,
    # the Mixer Engine's MixerType* tables; model_features.latency_figures)
    "/openapollo/latency/input_frames": {"type": "float", "value": 0.0,
                                         "readonly": True},
    "/openapollo/latency/output_frames": {"type": "float", "value": 0.0,
                                          "readonly": True},
    "/openapollo/latency/additional_input": {"type": "int", "value": 0,
                                             "readonly": True},
    "/openapollo/latency/additional_output": {"type": "int", "value": 0,
                                              "readonly": True},
    "/openapollo/latency/output_delay_comp": {"type": "string", "value": "",
                                              "readonly": True},
}


def apply_runtime_properties(state: StateTree):
    """Add RUNTIME_PROPERTIES to a freshly loaded tree; those the device
    map already has are forced to the runtime value (DeviceOnline defaults
    to False in the map but must be True)."""
    for rpath, rprops in RUNTIME_PROPERTIES.items():
        if not state.path_exists(rpath):
            state.add_runtime_property(rpath, rprops)
            log.debug("Added runtime property: %s = %r", rpath,
                      rprops.get("value"))
        else:
            state.set_value(rpath, rprops["value"])
            log.debug("Set runtime value: %s = %r", rpath,
                      rprops.get("value"))


# A preamp's Gain / GainTapered, exactly (not GainUnknown, not a send Gain)
_RE_PREAMP_GAIN = re_mod.compile(
    r"^(/devices/0/inputs/\d+/preamps/0)/(GainTapered|Gain)(?:/value)?$")


# NOTE: Do NOT add "/BufferSize/values" or other "/Foo/values" paths here!
# set_value("/Foo/values", x) resolves to the /Foo property and overwrites its
# "value" field (not "values"), corrupting the actual control value.

# Hot-plug: how the daemon follows the device node.  The watchdog probes the
# open fd every DEVICE_WATCH_INTERVAL; after a loss it retries the open
# with a doubling delay (MIN..MAX) and, once the node is back, waits
# DEVICE_SETTLE for the plugin chain/DSP before re-running the init path.
DEVICE_WATCH_INTERVAL = 0.25
DEVICE_REOPEN_MIN = 0.5
DEVICE_REOPEN_MAX = 5.0
DEVICE_SETTLE = 3.0
EXIT_RESTART = 75   # EX_TEMPFAIL: systemd's Restart=on-failure starts us again

# Firmware majors UA's Mixer Engine accepts (MixerType*::FWMajorVersion,
# parity 16 F43): 9 for the Apollo/8/8p/A16/Twin/Conrad family, 11 for
# Freud, Rene and Lavern and their subclasses (arm64 ME 11.9.0: Base
# 0x1000af088 returns 9; Freud 0x100427c70, Lavern 0x1001147f4, ReneBase
# 0x10029a4d8 return 11).
ACCEPTED_FW_MAJORS = (9, 11)
FW_MAJOR_11_CLASSES = frozenset({
    "Freud", "FreudG2", "Lavern", "LavernG2", "Shirley",
    "Rene2", "Rene4", "Rene8", "Rene16", "ReneLax",
    "Rene2G2", "Rene4G2", "Rene8G2", "Rene16G2"})
# CMxRoot::_checkFirmwareVersionCompatibility (0x100095af8) checks only the
# public types in 0x3ff3efdffe8 whose GetDeviceInfo carries a hardware id,
# which the macOS driver fills for the types in 0x3ff16fcf9a0.
FW_CHECK_TYPES = 0x3ff3efdffe8 & 0x3ff16fcf9a0


def firmware_text(fw_release: int) -> str:
    """The firmware version word as UA's Mixer Engine prints it."""
    return "%d.%02d.%03d" % ((fw_release >> 24) & 0xFF,
                             (fw_release >> 16) & 0xFF, fw_release & 0xFFFF)


def firmware_problem(info: dict, mixer_class: str | None) -> str | None:
    """Why UA's Mixer Engine would ask for a firmware update, or None.

    Needs the driver's fw_release (UA_FEAT_FW_RELEASE); None when it is
    missing or the public type is one the Mixer Engine does not check.
    The major must be the mixer class's (9 or 11; either one when the class
    is not known), then the per-device minimums
    (CheckDeviceSpecificFirmwareVersionCompatibility):
      ReneBase (x6/x8/x8p, Gen 2 too) 0x10029a4e0: dev_info[19:16] > 2
      Rene16 (x16, Gen 2 too)         0x100172a18: dev_info[19:18] != 0
      ReneLax (x16D)                  0x1002cf8c4: dev_info[19:17] > 2
      Armstrong (Twin)                0x100048688: on public type 0x10,
                                                   FPGA_REV[23:19] > 4
    """
    rel = info.get("fw_release")
    if not isinstance(rel, int) or rel in (0, 0xFFFFFFFF):
        return None
    pt = info.get("public_type")
    if isinstance(pt, int) and (pt > 0x29 or not (FW_CHECK_TYPES >> pt) & 1):
        return None
    major = (rel >> 24) & 0xFF
    if mixer_class is None:
        want = ACCEPTED_FW_MAJORS
    else:
        want = (11,) if mixer_class in FW_MAJOR_11_CLASSES else (9,)
    if major not in want:
        return "major %d, the model needs %s" % (
            major, " or ".join(map(str, want)))
    dev = info.get("dev_info")
    rev = ((dev >> 16) & 0xF) if isinstance(dev, int) else None
    if mixer_class in ("Rene2", "Rene4", "Rene8",
                       "Rene2G2", "Rene4G2", "Rene8G2"):
        ok = rev is not None and rev > 2
    elif mixer_class in ("Rene16", "Rene16G2"):
        ok = rev is not None and bool(rev & 0xC)
    elif mixer_class == "ReneLax":
        ok = rev is not None and (rev >> 1) > 2
    elif mixer_class == "Armstrong" and pt == 0x10:
        fpga = info.get("fpga_rev")
        ok = isinstance(fpga, int) and ((fpga >> 19) & 0x1F) > 4
    else:
        ok = True
    if not ok:
        return "below the %s minimum (device info 0x%08x)" % (
            mixer_class, dev if isinstance(dev, int) else 0)
    return None

# With the driver's event channel open, the 1 Hz driver-rate poll becomes a
# safety net: events carry the changes, this only catches a missed one.
EVENT_SAFETY_POLL = 10.0

# The Mixer Engine mutes the outputs for GetSecsToMuteOnSampleRateChange()
# seconds after a rate change (MuteUntilSettleDown); its default is not
# known, 1 s is a placeholder.  0 turns the mute off.
MUTE_ON_RATE_CHANGE_SECS = 1.0
# Mixer Engine mixer types (devices/*.json "mixer_class") whose
# SupportsOutputsAttenuate() returns 1, i.e. the models CMxRoot::
# MuteUntilSettleDown attenuates on a rate change.  From the vtable slot of
# every MixerType in the arm64 Mixer Engine: all classes except the ones
# that keep MixerTypeBase's 0 (Apollo, ApolloBlack, Peyton, M64).  Only the
# x4 (Freud) is hardware-tested.
OUTPUTS_ATTENUATE_CLASSES = frozenset({
    "A16", "A16Black", "Armstrong", "Stretch", "Lavern", "Shirley",
    "LavernG2", "Conrad", "ApolloSolo", "Cauldron", "Freud", "FreudG2",
    "Rene2", "Rene4", "Rene8", "Rene16", "ReneLax",
    "Rene2G2", "Rene4G2", "Rene8G2", "Rene16G2"})


def descriptor_mixer_class(slug: str | None) -> str | None:
    """The Mixer Engine mixer type of devices/<slug>.json, or None."""
    if not slug:
        return None
    try:
        desc = json.loads((DEVICES_DIR / f"{slug}.json").read_text())
        cls = desc.get("mixer_class")
        return cls if isinstance(cls, str) else None
    except (OSError, ValueError, AttributeError):
        return None
# The driver's rate index (UA_RATE_*) of a sample rate
RATE_INDEX = {44100: 1, 48000: 2, 88200: 3, 96000: 4, 176400: 5, 192000: 6}


def load_network_id(path: Path | None) -> str:
    """The persistent network id, created on first use.

    UA clients key their saved settings on the engine's network id; a new
    random id per start would make every restart look like a new engine.
    Stored as {"network_id": "<32 hex>"} in a small JSON file.  A missing
    or unreadable file gets a fresh id (and an unwritable path just means
    the id lasts for this run).
    """
    import uuid
    if path is not None:
        try:
            with open(path) as f:
                nid = json.load(f).get("network_id")
            if isinstance(nid, str) and re_mod.fullmatch(r"[0-9a-f]{32}", nid):
                return nid
        except (OSError, ValueError, AttributeError):
            pass
    nid = uuid.uuid4().hex
    if path is not None:
        try:
            path.parent.mkdir(parents=True, exist_ok=True)
            tmp = path.with_suffix(".tmp")
            with open(tmp, "w") as f:
                json.dump({"network_id": nid}, f)
            tmp.rename(path)
        except OSError as e:
            log.warning("Could not save the network id to %s: %s", path, e)
    return nid


def serial_text(info: dict) -> str:
    """The serial the daemon reports: the real one from GET_SYSTEM_INFO
    when the driver gave it, else the legacy 8-hex-digit BAR0 value."""
    real = info.get("serial_str")
    if isinstance(real, str) and real:
        return real
    return f"{info['serial']:08X}"


def read_device_info(backend) -> dict | None:
    """GET_DEVICE_INFO plus, when the driver has UA_FEAT_SYSTEM_INFO, the
    real serial string as info["serial_str"].  An old driver (or an
    unreadable/blank serial) leaves it out and callers fall back to the
    8-hex legacy serial."""
    info = backend.get_device_info()
    if not info:
        return info
    sysinfo = backend.get_system_info() \
        if hasattr(backend, "get_system_info") else None
    if isinstance(sysinfo, dict):
        serial = sysinfo.get("serial")
        if isinstance(serial, str) and serial.strip().strip("0 ") != "":
            info = dict(info, serial_str=serial.strip())
        # the kext's public type: the model when device_type has no map
        if isinstance(sysinfo.get("public_type"), int):
            info = dict(info, public_type=sysinfo["public_type"])
    return info


def fill_helper_identity(helper_tree: HelperTree | None,
                         info: dict | None) -> int:
    """Put the attached device's identity into the helper device tree.

    serial_number / firmware_version / expected_firmware_version are empty
    in the captured tree.  The driver reports the running firmware only, so
    the expected version is the running one (no false "update firmware"
    prompt; firmware_compatibility stays 0).  Format of the strings is not
    verified against a Console capture: firmware is the decimal register
    value, the serial is the driver's real 16-character serial
    (info["serial_str"], from GET_SYSTEM_INFO; see read_device_info) and,
    on an older driver, the legacy BAR0 value as 8 hex digits (which reads
    00000000 on the x4).
    Returns the number of properties set.
    """
    if not helper_tree or not info:
        return 0
    fw = str(info["fw_version"])
    values = {
        "serial_number": serial_text(info),
        "firmware_version": fw,
        "expected_firmware_version": fw,
    }
    count = 0

    def walk(node):
        nonlocal count
        if not isinstance(node, dict):
            return
        props = node.get("properties")
        if isinstance(props, dict) and "serial_number" in props:
            for key, val in values.items():
                if isinstance(props.get(key), dict):
                    props[key]["value"] = val
                    count += 1
        for child in node.get("children", []):
            walk(child)

    walk(helper_tree.root)
    return count


# Default device map location (relative to this script)
SCRIPT_DIR = Path(__file__).parent
DEFAULT_DEVICE_MAP = SCRIPT_DIR / "device_maps" / "device_map_apollo_x4.json"
DEFAULT_HELPER_TREE = SCRIPT_DIR / "device_maps" / "helper_tree.json"
# Device descriptors (devices/apollo-*.json) map a driver device_type to a model
# name, from which the matching device map filename is derived.
DEVICES_DIR = SCRIPT_DIR.parent / "devices"
DEFAULT_PORT = 4710
DEFAULT_HELPER_PORT = 4720
DEFAULT_WS_PORT = 4721
DEFAULT_HOST = "0.0.0.0"


class MixerClient:
    """State for a single connected TCP client."""

    _next_id = 0

    def __init__(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter,
                 trace: bool = False, ubjson: bool = False):
        MixerClient._next_id += 1
        self.id = f"client-{MixerClient._next_id}"
        self.reader = reader
        self.writer = writer
        # Text commands, UBJS and JSON frames in any order
        # (wire_command_framer)
        self.framer = WireCommandFramer()
        self.peer = writer.get_extra_info("peername")
        self.server_port = (writer.get_extra_info("sockname") or (None, None))[1]
        self.closed = False
        self.trace = trace
        self.helper = ubjson  # True for 4720 (helper) clients
        # Reply format: UBJS frames after command_format 2 (or a UBJS-framed
        # command), JSON frames after command_format 1 (or a JSON-framed
        # command), else NUL-terminated JSON text
        self.ubjson = False
        self.json_framed = False
        self._send_count = 0

    def send(self, data: bytes):
        """Queue data for sending to this client.  `data` is a UBJS frame
        or NUL-terminated JSON text; the latter goes out as a JSON frame
        while the client is in command_format 1."""
        if not self.closed:
            try:
                text = None
                if not self.ubjson:
                    text = data.decode("utf-8", errors="replace").rstrip("\x00")
                    if self.json_framed:
                        try:
                            data = json_encode_frame(json.loads(text))
                        except (ValueError, JsonFrameError) as e:
                            log.warning("%s: reply not sent as a JSON frame "
                                        "(%s): %s", self.id, e, text[:200])
                            return
                self.writer.write(data)
                self._send_count += 1
                if self.trace:
                    if self.ubjson:
                        log.debug("SEND → %s: [UBJSON %d bytes]", self.id, len(data))
                    else:
                        log.debug("SEND → %s: %s%s", self.id,
                                  "[JSON frame] " if self.json_framed else "",
                                  text[:200])
            except (ConnectionError, RuntimeError):
                self.closed = True

    def __repr__(self):
        if self.ubjson:
            proto = "UBJSON"
        elif self.json_framed:
            proto = "JSON-framed"
        else:
            proto = "helper-text" if self.helper else "text"
        return f"<MixerClient {self.id} :{self.server_port} {proto} {self.peer}>"


class MixerDaemon:
    """TCP server for UA Mixer Engine (:4710) and UA Mixer Helper (:4720)."""

    def __init__(self, state: StateTree, hw_router: HardwareRouter | None,
                 host: str = DEFAULT_HOST, port: int = DEFAULT_PORT,
                 helper_port: int = DEFAULT_HELPER_PORT,
                 trace: bool = False,
                 helper_tree: HelperTree | None = None,
                 metering=None,
                 network_id: str | None = None,
                 loaded_map: Path | None = None,
                 preserve_hardware: bool = False):
        self.state = state
        self.helper_tree = helper_tree  # Separate tree for 4720 Mixer Helper
        self.hw_router = hw_router
        self.host = host
        self.port = port
        self.helper_port = helper_port
        self.trace = trace
        self.clients: dict[str, MixerClient] = {}
        self.server: asyncio.Server | None = None
        self.helper_server: asyncio.Server | None = None
        self._metering = metering or NullMeter()
        self._meter_map: dict = {}   # (kind, ch, meter) → record ch
        self._meter_map_ids: tuple | None = None
        self._last_meter: dict[str, float | bool] = {}  # delta detection

        # Generate unique IDs matching real mixer engine format (hex UUIDs)
        import uuid
        self.network_id = network_id or uuid.uuid4().hex
        self.process_id = uuid.uuid4().hex

        self._init_fired = False  # Track if we've sent the init sequence

        # MeterPulse heartbeat counter (real UA Mixer Engine runs at ~32 Hz)
        self._meter_pulse_counter = 0

        # Readback suppression: when a client SETs a value that's also
        # read from hardware readback, suppress readback pushes for a
        # short window to prevent echo/trailing on the UI.
        self._readback_suppress: dict[str, float] = {}  # path → timestamp

        # Startup grace period: don't let readback overwrite state tree
        # defaults until we've had a chance to push them to hardware.
        self._readback_start_time = time.monotonic()
        self._hw_init_done = False

        # Wire state tree SET notifications to hardware router
        if hw_router:
            self.state.on_set = hw_router.on_set

        # The session's /Dirty flag: set by a session edit once the session
        # is up (_fire_init_complete), never by readback (session_dirty)
        self._session_ready = False
        self.session_dirty = SessionDirty(
            state, ready=lambda: self._session_ready, helper=helper_tree)

        # Hot-plug state (see _device_watchdog).  When the daemon starts
        # without the device, the watchdog owns the init push, so the
        # late-init paths (timer, meter pump) must leave it alone.
        self.loaded_map = loaded_map
        self._hw_online = bool(hw_router and hw_router.backend.connected)
        self._reopen_delay = DEVICE_REOPEN_MIN
        self._next_reopen = 0.0
        self._ready_at: float | None = None
        # Driver event channel (L1); polling stays the fallback
        self._events = DriverEvents(self._on_driver_event)
        self._events_loop = None
        self._last_rate_poll = 0.0
        self.mute_on_rate_change = MUTE_ON_RATE_CHANGE_SECS
        self._last_rate_index: int | None = None
        self._settle_timer = None
        self.io_layout: tuple[int, int] | None = None   # (play, rec) hw channels
        self.layout_changes = 0     # bumped by every LAYOUT_CHANGED event
        # True between UA_EVT_PAUSED and UA_EVT_RESUMED for the device
        # reason (system suspend): no hardware writes, the tree keeps going
        self.hw_paused = False
        # `post` object commands (commands.py)
        self.commands = CommandEngine(self)
        # Host-side preamp autogain, served as /auto_gain on the helper tree
        self.autogain = AutoGainService(self)
        # The model's /ClockSource list (the static fallback) and the
        # warnings already given
        values = (self.state.get_prop_dict("/ClockSource") or {}).get("values")
        self._model_clock_sources = list(values) if values else None
        self._fw_warned = False
        self._link_warned = False
        # devices/<model>.json of the attached model (main() fills it) and
        # the latency figures last published / ADC delay comp last sent
        self.model_data: dict = {}
        self._latency_key = None
        self._latency_fig: dict | None = None
        self._adc_comp_sent: tuple | None = None
        if hw_router and not self._hw_online:
            self._hw_init_done = True
            self._set_device_online(False)
        # --preserve-hardware: attach to a running Apollo without writing
        # to it.  This device session publishes what the readback shows
        # and sends no init sequence (and no ADC delay comp); a later
        # connection (hot-plug, resume) re-inits as always, since the
        # device lost its state then.
        self._preserve_session = False
        if preserve_hardware and self._hw_online:
            self._preserve_session = True
            self._hw_init_done = True
            rb = hw_router.poll_hw_readback()
            if rb:
                self._sync_hw_readback(rb, {})
            log.info("Preserving the hardware's current settings: no init "
                     "sequence for this connection")

    async def start(self):
        """Start TCP servers on both ports (runs forever)."""
        self.server = await asyncio.start_server(
            self._handle_text_client, self.host, self.port,
            reuse_address=True)

        self._events_attach()
        if self._hw_online:
            self._on_device_connected()

        addrs = [s.getsockname() for s in self.server.sockets]
        log.info("TCP:%d (Mixer Engine, text) listening on %s", self.port, addrs)
        log.info("Device: %s (%d controls)",
                 self.state.device_name, self.state.control_count)

        # Start helper (4720) listener — same command parsing, UBJSON responses
        if self.helper_port:
            try:
                self.helper_server = await asyncio.start_server(
                    self._handle_ubjson_client, self.host, self.helper_port,
                    reuse_address=True)
                log.info("TCP:%d (Mixer Helper, UBJSON) listening on %s:%d",
                         self.helper_port, self.host, self.helper_port)
            except OSError as e:
                log.warning("Could not bind helper port %d: %s",
                            self.helper_port, e)

        # Fire initialized=True after a short delay.  ConsoleLink (iPad)
        # subscribes to /initialized and waits for the false→true transition
        # before proceeding past the "connecting" screen.  The POST /response
        # path only fires for UAD Console (macOS), so we must also fire at
        # startup for clients that skip the challenge-response handshake.
        if not self._init_fired:
            # Delay 4s to let driver's plugin chain complete (~400ms)
            # before sending monitor init. Previous 1s delay was too early.
            asyncio.get_event_loop().call_later(
                4.0, self._fire_init_complete)

        async with self.server:
            if self.helper_server:
                asyncio.create_task(self.helper_server.serve_forever())
            await self.server.serve_forever()

    async def _handle_text_client(self, reader: asyncio.StreamReader,
                                  writer: asyncio.StreamWriter):
        """Handle a TCP:4710 client (text/JSON protocol)."""
        await self._handle_client(reader, writer, ubjson=False)

    async def _handle_ubjson_client(self, reader: asyncio.StreamReader,
                                    writer: asyncio.StreamWriter):
        """Handle a TCP:4720 client (text commands, UBJSON responses)."""
        await self._handle_client(reader, writer, ubjson=True)

    # Regex for parsing meter subscription paths — compiled once
    _METER_PATH_RE = re_mod.compile(
        r'/devices/0/(inputs|outputs|auxs)/(\d+)/'
        r'(?:sends/(\d+)/)?'
        r'meters/(\d+)/'
        r'(MeterLevel|MeterPeakLevel|MeterClip|Clip)/value$'
    )

    # ── Device hot-plug ──────────────────────────────────────────────

    def _set_device_online(self, online: bool):
        """Report /devices/0/DeviceOnline to clients (not persisted: it
        describes the hardware right now, not a setting)."""
        path = "/devices/0/DeviceOnline"
        if self.state.get_value(path + "/value") == online:
            return
        log.info("DeviceOnline -> %s", online)
        self._push_to_clients(path, online)
        self.state._dirty.pop(path + "/value", None)

    def _device_lost(self, now: float):
        log.warning("Apollo device lost — daemon stays up, reopening with backoff")
        self._events_detach()
        self._cancel_settle()
        self._set_hw_paused(False)   # the next connection starts clean
        self._hw_online = False
        self._ready_at = None
        # The watchdog owns the init push for the next connection
        self._hw_init_done = True
        self._reopen_delay = DEVICE_REOPEN_MIN
        self._next_reopen = now + DEVICE_REOPEN_MIN
        pending = getattr(self.hw_router, "_gain_pending", None)
        if pending:
            pending.clear()   # stale gains must not land on the new session
        self._set_device_online(False)

    def _device_ready(self):
        """The device is back and settled: run the init path again."""
        be = self.hw_router.backend
        info = read_device_info(be)
        if info:
            fill_helper_identity(self.helper_tree, info)
            if self.loaded_map is not None:
                new_map = find_device_map(info["device_type"],
                                          info.get("public_type"))
                if new_map and new_map != self.loaded_map:
                    log.warning("Device is a different model (type 0x%02x): "
                                "device map %s -> %s, restarting",
                                info["device_type"], self.loaded_map.name,
                                new_map.name)
                    return False
        log.info("Device ready: re-running the init sequence")
        self._preserve_session = False
        self._push_defaults_to_hardware()
        self._hw_init_done = True
        self._readback_start_time = time.monotonic()
        # Resync what the hardware reports now (rate, preamps, monitor)
        self._poll_driver_rate(force=True)
        rb = self.hw_router.poll_hw_readback()
        if rb:
            self._sync_hw_readback(rb, {})   # empty last_rb → push all
        self._events_attach()
        self._hw_online = True
        self._set_device_online(True)
        self._on_device_connected()
        return True

    def _device_watchdog_step(self, now: float) -> bool:
        """One tick of the hot-plug state machine.  Returns False when the
        daemon should exit for a restart (a different Apollo model came
        back, which needs another device map)."""
        be = self.hw_router.backend
        if self._hw_online:
            if be.connected and be.probe():
                return True
            if be.connected:
                be.mark_lost("probe failed")
            self._device_lost(now)
            return True

        if self._ready_at is None:
            if now < self._next_reopen:
                return True
            if be.reopen():
                log.info("Device node is back, waiting %.0fs for the DSP "
                         "to settle", DEVICE_SETTLE)
                self._ready_at = now + DEVICE_SETTLE
            else:
                self._reopen_delay = min(self._reopen_delay * 2,
                                         DEVICE_REOPEN_MAX)
                self._next_reopen = now + self._reopen_delay
            return True

        # Reopened, settling: it can vanish again before it is ready
        if not (be.connected and be.probe()):
            if be.connected:
                be.mark_lost("probe failed while settling")
            self._ready_at = None
            self._next_reopen = now + DEVICE_REOPEN_MIN
            self._reopen_delay = DEVICE_REOPEN_MIN
            return True
        if now >= self._ready_at:
            self._ready_at = None
            return self._device_ready()
        return True

    async def _device_watchdog(self):
        """Follow the device node: notice unplug/module reload, reopen when
        it returns, re-run the init path and tell clients."""
        be = self.hw_router.backend if self.hw_router else None
        if be is None or not hasattr(be, "reopen"):
            return
        while True:
            await asyncio.sleep(DEVICE_WATCH_INTERVAL)
            try:
                keep_going = self._device_watchdog_step(time.monotonic())
            except Exception:
                log.exception("Device watchdog step failed")
                keep_going = True
            if not keep_going:
                raise SystemExit(EXIT_RESTART)

    async def _meter_pump(self):
        """Send periodic meter values to subscribed clients.

        ConsoleLink subscribes to MeterLevel, MeterPeakLevel, and
        MeterClip paths and expects periodic updates.  Without these,
        it may consider the connection stale.

        Also syncs hardware readback -> console: when a physical knob
        is turned on the Apollo, the new value is pushed to all
        subscribed clients so the UI tracks the hardware.

        Meter values come from the software meter (metering.make_meter:
        the Apollo's record channels through PipeWire, or ALSA capture),
        not from hardware readback registers.

        DSP load is polled at ~1 Hz (every 50 ticks) and pushed to
        /TotalDSPLoad, /TotalPGMLoad, /TotalMEMLoad.
        """
        meter_cache: list[dict] | None = None
        cache_gen = 0
        last_rb: dict = {}
        dsp_tick = 0

        while True:
            await asyncio.sleep(0.02)  # 50 Hz — matches real UA Mixer Engine

            if not self.state._callbacks:
                meter_cache = None
                continue

            # Rebuild cache when subscriptions change
            sub_gen = len(self.state._subs)
            if meter_cache is None or sub_gen != cache_gen:
                meter_cache = []
                for path in self.state._subs:
                    entry = self._parse_meter_path(path)
                    if entry:
                        meter_cache.append(entry)
                cache_gen = sub_gen

            # Retry hardware init if device wasn't available at startup.
            # Apollo may connect after daemon starts (TB hot-plug).
            # Wait 3s after device appears for plugin chain to complete
            # (~400ms) + DSP settle before sending monitor init.
            if self.hw_router and not self._hw_init_done:
                be = self.hw_router.backend
                if be.connected and not hasattr(self, '_hw_appear_time'):
                    self._hw_appear_time = time.monotonic()
                    log.info("Device appeared, waiting 3s for plugin chain...")
                if (be.connected and hasattr(self, '_hw_appear_time')
                        and time.monotonic() - self._hw_appear_time >= 3.0):
                    self._push_defaults_to_hardware()
                    self._hw_init_done = True
                    self._readback_start_time = time.monotonic()
                    log.info("Late hardware init: monitor+preamp defaults sent")

            # Poll hardware readback and push changes to console
            # (monitor knob, mute, dim, preamp gains — NOT meters)
            # Skip during startup grace period (2s) to let init push
            # state tree defaults to hardware first.
            if self.hw_router and time.monotonic() - self._readback_start_time > 2.0:
                self.hw_router.flush_pending_gains()
                rb = self.hw_router.poll_hw_readback()
                if rb:
                    self._sync_hw_readback(rb, last_rb)
                    last_rb = rb
                    if isinstance(self._metering, ReadbackMeter):
                        self._metering.feed(rb["words"])

            # Poll DSP load at ~1 Hz (every 50 ticks of the 50 Hz pump)
            dsp_tick += 1
            if dsp_tick >= 50:
                dsp_tick = 0
                self._poll_dsp_load()
                self._poll_driver_rate()
                self._poll_transport()
                self._poll_buffer_size()
                self._refresh_latency()

            if not meter_cache:
                continue

            for entry in meter_cache:
                value = self._read_meter(entry)

                # Delta check: skip if value hasn't changed significantly
                last = self._last_meter.get(entry['path'])
                if last is not None:
                    if entry['prop'] == 'clip':
                        if value == last:
                            continue
                    else:
                        if abs(value - last) < 0.5:
                            continue

                self._last_meter[entry['path']] = value

                # Update the state tree's stored value so GET requests
                # return current meter readings
                prop = self.state.get_prop_dict(entry['path'].rsplit('/value', 1)[0])
                if prop is not None:
                    prop["value"] = value

                # Push to all subscribers on both state tree and helper tree
                subs = set()
                if entry['path'] in self.state._subs:
                    subs.update(self.state._subs[entry['path']])
                # Also check base path without /value suffix
                base_path = entry['path'].rsplit('/value', 1)[0]
                if base_path in self.state._subs:
                    subs.update(self.state._subs[base_path])

                for client_id in list(subs):
                    cb = self.state._callbacks.get(client_id)
                    if cb:
                        try:
                            cb(entry['path'], value)
                        except Exception:
                            pass

                # Also notify helper tree subscribers (cross-protocol)
                if self.helper_tree:
                    self.helper_tree._notify(entry['path'], value)

    async def _meter_pulse_heartbeat(self):
        """Send MeterPulse counter to subscribed 4710 clients at ~10 Hz.

        The real UA Mixer Engine sends /MeterPulse/value at ~32 Hz as an
        incrementing int64 counter. Clients use this as a heartbeat to
        know the daemon is alive and metering is active.

        We run at 10 Hz (configurable) which is sufficient for
        ConsoleLink/iPad to stay happy.
        """
        pulse_path = "/MeterPulse/value"

        while True:
            await asyncio.sleep(0.1)  # 10 Hz

            # Check if anyone is subscribed to MeterPulse
            subs = self.state._subs.get(pulse_path)
            if not subs:
                # Also check without /value suffix
                subs = self.state._subs.get("/MeterPulse")
            if not subs:
                continue

            self._meter_pulse_counter += 1

            # Update the state tree value (no notifications -- we push manually)
            prop = self.state.get_prop_dict("/MeterPulse")
            if prop is not None:
                prop["value"] = self._meter_pulse_counter

            # Push to all subscribed clients
            for client_id in list(subs):
                cb = self.state._callbacks.get(client_id)
                if cb:
                    try:
                        cb(pulse_path, self._meter_pulse_counter)
                    except Exception:
                        pass

    @classmethod
    def _parse_meter_path(cls, path: str) -> dict | None:
        """Parse a meter subscription path into structured components.

        Returns dict with keys: path, kind, channel, send (index or None),
        meter_idx, prop, or None if not a meter path.
        """
        m = cls._METER_PATH_RE.search(path)
        if not m:
            return None

        raw_prop = m.group(5)
        if raw_prop == 'MeterLevel':
            prop = 'level'
        elif raw_prop == 'MeterPeakLevel':
            prop = 'peak'
        else:  # MeterClip or Clip
            prop = 'clip'

        return {
            'path': path,
            'kind': m.group(1),       # 'inputs', 'outputs' or 'auxs'
            'channel': int(m.group(2)),
            'send': int(m.group(3)) if m.group(3) is not None else None,
            'meter_idx': int(m.group(4)),
            'prop': prop,
        }

    def _read_meter(self, entry: dict):
        """Read a single meter value from the software meter.

        Maps the device-map channel (by its Name) to the ALSA record
        channel that carries it; channels the Apollo doesn't record read
        as silence.
        """
        ids = tuple(self._metering.channel_ids)
        key = (entry['kind'], entry['channel'], entry['meter_idx'])
        if ids != self._meter_map_ids:
            self._meter_map = {}
            self._meter_map_ids = ids
        if key not in self._meter_map:
            base = f"/devices/0/{entry['kind']}/{entry['channel']}"
            self._meter_map[key] = meter_channel(
                entry['kind'], entry['channel'], entry['meter_idx'],
                self.state.get_value(f"{base}/Name/value"),
                bool(self.state.get_value(f"{base}/Stereo/value")), ids)
        cap = self._meter_map[key]
        if cap is None:
            level, peak, clip = SILENCE_DB, SILENCE_DB, False
        else:
            level, peak, clip = self._metering.get_channel_meter(cap)
            if entry['kind'] == 'inputs':
                level, peak, clip = self._apply_meter_gain(
                    entry, level, peak, clip)

        if entry['prop'] == 'level':
            return level
        elif entry['prop'] == 'peak':
            return peak
        elif entry['prop'] == 'clip':
            return clip
        return SILENCE_DB

    def _meter_gain_db(self, entry: dict) -> float | None:
        """The gain the host adds to an input's meter, or None (silent).

        As the Mixer Engine does it (DoMetersMono; nothing in the readback
        carries these): a send meter is the input level plus the send's
        gain, plus the fader for a post-fader send; with /PostFaderMetering
        the input meter itself is the level plus the fader.  A muted input
        reads silent wherever the fader counts, a bypassed send always.
        """
        get = self.state.get_value
        base = f"/devices/0/inputs/{entry['channel']}"

        def db(path, default):
            try:
                return float(get(path))
            except (TypeError, ValueError):
                return default

        fader = db(f"{base}/FaderLevel/value", 0.0)
        muted = bool(get(f"{base}/Mute/value"))
        send = entry.get('send')
        if send is None:
            if not get("/PostFaderMetering/value"):
                return 0.0
            return None if muted or fader <= -144.0 else fader

        sbase = f"{base}/sends/{send}"
        if get(f"{sbase}/Bypass/value"):
            return None
        gain = db(f"{sbase}/Gain/value", -144.0)
        if gain <= -144.0:
            return None
        # Pre/post fader: the send's own override, else its bus setting
        # (auxs/N/SendPostFader for AUX N, HPnSendPostFader for CUE n)
        override = get(f"{sbase}/PostFaderOverride/value")
        send_id = str(get(f"{sbase}/ID/value") or "")
        num = send_id[3:]
        if override in (0, 1):
            post = override == 1
        elif send_id.startswith("aux") and num.isdigit():
            post = bool(get(f"/devices/0/auxs/{int(num) - 1}"
                            "/SendPostFader/value"))
        elif send_id.startswith("cue") and num.isdigit():
            post = bool(get(f"/devices/0/HP{num}SendPostFader/value"))
        else:
            post = False
        if post:
            if muted or fader <= -144.0:
                return None
            gain += fader
        return gain

    def _apply_meter_gain(self, entry: dict, level: float, peak: float,
                          clip: bool):
        """Input meter -> post-fader or send meter (see _meter_gain_db)."""
        gain = self._meter_gain_db(entry)
        if gain is None:
            return SILENCE_DB, SILENCE_DB, False
        if not gain:
            return level, peak, clip

        def shift(v):
            # Silence stays silence; the meter tops out at 0 dBFS
            return v if v <= SILENCE_DB else max(SILENCE_DB, min(0.0, v + gain))

        return shift(level), shift(peak), clip or (
            level > SILENCE_DB and level + gain >= 0.0)

    def _readback_suppressed(self, path: str) -> bool:
        """Check if a readback push should be suppressed (recent client SET)."""
        ts = self._readback_suppress.get(path)
        if ts is not None:
            if time.monotonic() - ts < 2.0:  # 2s — suppress readback after SET
                return True
            del self._readback_suppress[path]
        return False

    def _push_if_present(self, path: str, value, force: bool = False):
        """Push a readback-derived value to a control this model's device
        map has (it changed, or `force`); a control the map lacks is
        skipped."""
        cur = self.state.get_value(path + "/value")
        if cur is not None and (force or cur != value):
            self._push_to_clients(path, value)

    def _sync_hw_readback(self, rb: dict, last_rb: dict):
        """Push hardware readback changes to helper tree + subscribers.

        Compares current readback with previous values. When a field
        changes (physical knob turned, button pressed), updates the
        helper tree and notifies all subscribed console clients.  An
        empty last_rb pushes everything.

        Readback pushes are suppressed for 2 s after a client SET
        to prevent echo/trailing on UI controls.

        Output and input indexes come from the device map (monitor x4 18 /
        x8p 22, HP outputs, the preamp inputs); readback preamp N is the
        N-th preamp input.
        """
        router = self.hw_router
        mon_out = router.monitor_output_index()
        if mon_out is not None:
            mon_base = f"/devices/0/outputs/{mon_out}"
            self._sync_level_readback(mon_base, "monitor", rb, last_rb)

            for ctrl, key in (("Mute", "monitor_mute"), ("DimOn", "monitor_dim"),
                              ("MixToMono", "monitor_mono")):
                val = rb.get(key, False)
                if val != last_rb.get(key):
                    self._push_to_clients(f"{mon_base}/{ctrl}", val)

        # HP1/HP2 level — x4 outputs 19 and 20
        for n, key in ((0, "hp1"), (1, "hp2")):
            hp_out = router.output_index("hp", n)
            if hp_out is not None:
                self._sync_level_readback(f"/devices/0/outputs/{hp_out}",
                                          key, rb, last_rb)

        # Talkback active (TalkbackOn is at the tree root in the device maps)
        talkback = rb.get("talkback_active", False)
        if talkback != last_rb.get("talkback_active"):
            self._push_to_clients("/TalkbackOn", talkback)

        # Clock lock (only when the hardware says the bit is valid)
        locked = rb.get("clock_locked")
        if locked is not None and locked != last_rb.get("clock_locked"):
            self._push_to_clients("/ClockLocked", locked)

        # Alt-monitor selection (word 6 [29:28]), as the Mixer Engine's
        # SetAltMonSelectionFromHardware: ignored right after a client SET
        alt = rb.get("altmon_selection")
        alt_path = "/devices/0/AltMonSelection"
        if alt is not None and alt != last_rb.get("altmon_selection") and \
                not self._readback_suppressed(alt_path) and \
                not self._readback_suppressed(alt_path + "/value"):
            self._push_if_present(alt_path, alt, force=True)

        # Device-level state with no x4 map control: pushed only on a model
        # whose map has it
        for ctrl, key, conv in (("LowNoisePreamps", "low_noise_preamps", bool),
                                ("Temperature", "temperature_raw", float),
                                ("AnalogBoardID", "analog_board_id", int)):
            if key in rb and rb[key] != last_rb.get(key):
                self._push_if_present(f"/devices/0/{ctrl}", conv(rb[key]),
                                      force=True)

        preamps = sorted(router.preamp_inputs())
        for ch, inp in enumerate(preamps):
            prefix = f"preamp_{ch}"
            if f"{prefix}_gain" not in rb:
                break           # readback carries no more preamps
            in_base = f"/devices/0/inputs/{inp}"
            path_base = f"{in_base}/preamps/0"

            # Preamp gain
            gain_dB = rb[f"{prefix}_gain"]
            if gain_dB != last_rb.get(f"{prefix}_gain"):
                gain_raw = rb.get(f"{prefix}_gain_raw", 0)
                log.info("GAIN ch%d: raw=0x%02x dB=%d", inp, gain_raw, gain_dB)
                gain_path = f"{path_base}/Gain"
                tapered_path = f"{path_base}/GainTapered"
                dragging = (self._readback_suppressed(gain_path) or
                            self._readback_suppressed(tapered_path) or
                            self._readback_suppressed(gain_path + "/value") or
                            self._readback_suppressed(tapered_path + "/value"))
                if not dragging:
                    self._push_to_clients(gain_path, float(gain_dB))
                    self._push_to_clients(tapered_path, preamp_db_to_tapered(gain_dB))
                    # The gain moved outside the daemon (front-panel knob):
                    # update the router's duplicate filter, or a client
                    # SET back to its previous value would be dropped.
                    router._gain_last[inp] = (gain_dB, time.monotonic())

            # Flags
            for ctrl, key in [("48V", "48v"), ("Pad", "pad"),
                              ("LowCut", "lowcut"), ("Phase", "phase"),
                              ("HiZ", "hiz")]:
                if f"{prefix}_{key}" not in rb:
                    continue    # HiZ: inputs 1-2 only
                val = rb[f"{prefix}_{key}"]
                if val != last_rb.get(f"{prefix}_{key}"):
                    self._push_to_clients(f"{path_base}/{ctrl}", val)

            # IOType and Link (Stereo) are at input level, not preamps/0
            micline = rb.get(f"{prefix}_micline", False)
            if micline != last_rb.get(f"{prefix}_micline"):
                iotype = "Line" if micline else "Mic"
                self._push_to_clients(f"{in_base}/IOType", iotype)
            link = rb.get(f"{prefix}_link", False)
            if link != last_rb.get(f"{prefix}_link"):
                self._push_if_present(f"{in_base}/Stereo", link, force=True)

        self._sync_front_panel(rb, last_rb, preamps)

    def _sync_level_readback(self, base: str, key: str, rb: dict,
                             last_rb: dict):
        """A monitor or headphone level from the readback: CRMonitorLevel
        (dB, what Console's number shows) and CRMonitorLevelTapered (the
        knob), both, whenever the raw level moves.

        The trigger is the raw byte: below -55.5 dB one 0.5 dB step moves
        the tapered value by 0.00044, so a tapered threshold (the old
        0.002) missed a slow knob turn there entirely.  Skipped for 2 s
        after a client SET of either one."""
        raw = rb.get(f"{key}_level_raw")
        if raw is None or raw == last_rb.get(f"{key}_level_raw"):
            return
        level, tapered = f"{base}/CRMonitorLevel", f"{base}/CRMonitorLevelTapered"
        if any(self._readback_suppressed(p) for p in
               (level, tapered, level + "/value", tapered + "/value")):
            return
        self._push_to_clients(level, (raw - 192) / 2.0)
        self._push_to_clients(tapered, rb[f"{key}_level_tapered"])

    def _sync_front_panel(self, rb: dict, last_rb: dict, preamps: list):
        """SelectedOnFront of the monitor, CUE 1 and the preamp inputs.

        As the Mixer Engine (applySettings1, applyInputGainSettings): when
        word 0 changes, bit 27 set means the front panel is on the
        headphones (bit 26; CUE 1, the cue with SynchOut 2 in every UA
        template) or else the monitor, and no input is selected; bit 27
        clear selects nothing on the outputs and the preamp in word 3
        [30:28] on the inputs.  A change of word 3 [30:28] alone re-selects
        the inputs.
        """
        words, last = rb.get("words"), last_rb.get("words")
        if not words:
            return
        router = self.hw_router
        force = last is None
        select_inputs = False
        if force or words[0] != last[0]:
            focus = rb["hp_focus_valid"]
            on_hp = focus and rb["hp_focus_headphone"]
            for out, val in ((router.monitor_output_index(), focus and not on_hp),
                             (router.output_index("cue", 0), on_hp)):
                if out is not None:
                    self._push_if_present(
                        f"/devices/0/outputs/{out}/SelectedOnFront", val, force)
            if focus:
                for inp in preamps:
                    self._push_if_present(
                        f"/devices/0/inputs/{inp}/SelectedOnFront", False, force)
            else:
                select_inputs = True
        if select_inputs or (not force and (words[3] ^ last[3]) & 0x70000000):
            sel = rb["front_panel_input"]
            for ch, inp in enumerate(preamps):
                self._push_if_present(
                    f"/devices/0/inputs/{inp}/SelectedOnFront", ch == sel, force)

    def _poll_driver_rate(self, force: bool = False):
        """Report the driver's sample rate when it changes under the daemon.

        An app re-preparing its streams, an ALSA control or a SET that
        didn't take changes the rate without a client SET; without this
        /SampleRate keeps showing the last value a client set.

        The pump calls this at 1 Hz.  While the driver's event channel is
        open the events carry the changes and only every EVENT_SAFETY_POLL
        seconds does a call do the read; `force` skips that wait.
        """
        if not self.hw_router or not self.hw_router.backend.connected:
            return
        now = time.monotonic()
        if (not force and self._events.active
                and now - self._last_rate_poll < EVENT_SAFETY_POLL):
            return
        self._last_rate_poll = now
        if self._readback_suppressed("/SampleRate/value"):
            return
        rate = self.hw_router.backend.get_driver_param(
            DRIVER_PARAM_SAMPLE_RATE)
        if rate and self.state.get_value("/SampleRate/value") != rate:
            log.info("Driver sample rate is now %d Hz", rate)
            self._push_to_clients("/SampleRate", rate)

    # ── Driver event channel (L1) ───────────────────────────────────

    def _events_attach(self):
        """Open the device's event channel and hand it to the event loop.

        Quietly does nothing without a connected device or on a driver
        without the channel: the 1 Hz polling then runs as before.  Called
        at start-up and again each time the watchdog brings the device back.
        """
        if self._events.active or not self.hw_router:
            return
        be = self.hw_router.backend
        path = getattr(be, "device_path", None)
        if not be.connected or not isinstance(path, str):
            return
        if not self._events.open(path):
            log.info("Driver has no event channel: polling the driver "
                     "(rate every second)")
            return
        try:
            loop = asyncio.get_running_loop()
        except RuntimeError:
            loop = None     # no loop yet (tests): drain by hand
        if loop is not None:
            try:
                loop.add_reader(self._events.fd, self._on_events_readable)
                self._events_loop = loop
            except (NotImplementedError, OSError, ValueError) as e:
                log.warning("Event channel not usable with this loop (%s): "
                            "polling", e)
                self._events.close()
                return
        rate = be.get_driver_param(DRIVER_PARAM_SAMPLE_RATE)
        self._last_rate_index = RATE_INDEX.get(rate)
        log.info("Driver event channel open: rate poll slowed to %.0f s",
                 EVENT_SAFETY_POLL)

    def _events_detach(self):
        if self._events_loop is not None and self._events.fd >= 0:
            try:
                self._events_loop.remove_reader(self._events.fd)
            except (OSError, ValueError):
                pass
        self._events_loop = None
        self._events.close()

    def _on_events_readable(self):
        """Event loop callback: the event fd has data (or hung up)."""
        status = self._events.drain()
        if status == EVENTS_OFFLINE:
            # ENODEV / hang-up: the last event normally arrived first,
            # this covers a queue that was lost
            self._events_detach()
            self._on_device_offline("event channel closed")
        elif status == EVENTS_UNSUPPORTED:
            log.info("Event channel stopped answering: polling the driver")
            self._events_detach()

    def _on_device_offline(self, why: str):
        """The driver says the device is gone: same state as a failed
        watchdog probe, the watchdog then reopens it."""
        if not self.hw_router or not self._hw_online:
            return
        log.warning("Device offline (%s)", why)
        be = self.hw_router.backend
        if be.connected:
            be.mark_lost(why)
        self._device_lost(time.monotonic())

    def _mute_until_settle(self):
        """MuteUntilSettleDown: attenuate the outputs (OutputsAttenuate) for
        a moment after the clock changed rate, so the converters' re-lock
        does not reach the speakers.  Needs the event loop to schedule the
        end; without one nothing is attenuated."""
        secs = self.mute_on_rate_change
        if secs <= 0 or not self.hw_router or not self._hw_online:
            return
        try:
            loop = asyncio.get_running_loop()
        except RuntimeError:
            return
        if not self.hw_router.begin_settle_mute():
            return
        if self._settle_timer is not None:
            self._settle_timer.cancel()
        self._settle_timer = loop.call_later(secs, self._end_settle)

    def _end_settle(self):
        self._settle_timer = None
        if self.hw_router:
            self.hw_router.end_settle_mute()

    def _cancel_settle(self):
        if self._settle_timer is not None:
            self._settle_timer.cancel()
            self._settle_timer = None
        if self.hw_router:
            self.hw_router.cancel_settle_mute()

    # ── Converter latency and delay compensation (A10/D5) ──────────

    def _refresh_latency(self, force: bool = False) -> bool:
        """Recompute the model's latency figures when the rate, the analog
        board id (readback word 33) or /MaxDelayComp changed: publish them
        under /openapollo/latency/ and send the ADC delay comp (other param
        0x6a) as CMxMixer::UpdateADCDelayComp does after every rate change.
        Returns True when it recomputed."""
        lat = self.model_data.get("latency") if self.model_data else None
        if not lat:
            return False
        rate = self.state.get_value("/SampleRate/value")
        board = self.state.get_value("/devices/0/AnalogBoardID/value")
        mdc = MAX_DELAY_COMP_SAMPLES.get(
            str(self.state.get_value("/MaxDelayComp/value")), 0)
        key = (rate, board, mdc)
        changed = False
        if key != self._latency_key or force:
            fig = latency_figures(lat, rate, board, mdc,
                                  lines34_hp=self.model_data.get("mixer_class")
                                  in LINES34_HP_CLASSES)
            if fig is None:
                return False
            self._latency_key, self._latency_fig = key, fig
            changed = True
            for name, value in (
                    ("input_frames", round(fig["adc"], 4)),
                    ("output_frames", round(fig["dac"], 4)),
                    ("additional_input", fig["additional_input"]),
                    ("additional_output", fig["additional_output"]),
                    ("output_delay_comp",
                     json.dumps(fig["dac_comp"], sort_keys=True))):
                self._push_to_clients(f"/openapollo/latency/{name}", value)
            log.info("Latency at %s Hz (board %s): in %.4f, out %.4f "
                     "frames; ADC delay comp %d; reported %d/%d", rate, board,
                     fig["adc"], fig["dac"], fig["adc_comp"],
                     fig["additional_input"], fig["additional_output"])
        fig = self._latency_fig
        # the init sequence writes its own 0x6a: send again after every
        # (re-)init, and whenever the figure changes
        sent = (fig["adc_comp"], self._readback_start_time) if fig else None
        router = self.hw_router
        if (fig and router and self._hw_init_done and self._hw_online
                and not self.hw_paused and not self._preserve_session
                and sent != self._adc_comp_sent):
            from hardware import ADC_DELAY_COMP_PARAM
            if router._set_other(ADC_DELAY_COMP_PARAM, fig["adc_comp"],
                                 what="ADC delay comp"):
                self._adc_comp_sent = sent
        return changed

    # ── Device status from GET_SYSTEM_INFO / the transport ──────────

    def _system_info(self) -> dict | None:
        be = self.hw_router.backend if self.hw_router else None
        if be is None or not be.connected:
            return None
        get = getattr(be, "get_system_info", None)
        info = get() if callable(get) else None
        return info if isinstance(info, dict) else None

    def _on_device_connected(self):
        """A device session starts (start-up, hot-plug return): one
        GET_SYSTEM_INFO feeds the clock-source list, the PCIe link report and
        the firmware check."""
        info = self._system_info()
        self._publish_clock_sources(info)
        self._publish_pcie_link(info)
        self._check_firmware(info)
        self._poll_transport()

    def _publish_clock_sources(self, info: dict | None = None,
                               notify: bool = True) -> bool:
        """Set the /ClockSource value list from the device's source mask.

        With UA_FEAT_SYSINFO_EXT the driver reports 0xC08C[31:16]; the list
        is the model's sources (devices/*.json) that the device offers
        (hardware.clock_source_names).  Without the mask, or without a
        device, it is the static model list.  When the list changes,
        /ClockSourceListChanged is pulsed so clients ask for it again.
        Returns True when the list changed.
        """
        model = self._model_clock_sources
        if not model or not self.state.path_exists("/ClockSource/value"):
            return False
        if info is None:
            info = self._system_info()
        # The device's own menu (UA_FEAT_CLOCK_SOURCES) wins: its names are
        # what the Mixer Engine shows (AES/EBU, MADI included)
        menu = {}
        if self.hw_router and self.hw_router.backend.connected and \
                hasattr(self.hw_router.backend, "get_clock_sources"):
            menu = device_clock_menu(
                self.hw_router.backend.get_clock_sources())
            # Only the sources the model's menu has (devices/*.json): the
            # x4 offers ADAT in its mask but UA's menu for it is Internal
            # and S/PDIF.  A model whose names the device does not use
            # keeps the device's whole menu.
            kept = {n: v for n, v in menu.items() if n in model}
            if len(kept) > 1:
                menu = kept
        if self.hw_router:
            self.hw_router.clock_sources = menu
        names = list(menu) or clock_source_names(
            info.get("clock_src_mask") if info else None, model) or model
        prop = self.state.get_prop_dict("/ClockSource")
        if prop is None or prop.get("values") == names:
            return False
        log.info("Clock sources are now %s (device mask 0x%04x)", names,
                 (info or {}).get("clock_src_mask") or 0)
        self.state.update_property("/ClockSource", {"values": names})
        if notify:
            for flag in (True, False):
                self._push_to_clients("/ClockSourceListChanged", flag)
            # a pulse, not a setting: keep it out of the saved state
            self.state._dirty.pop("/ClockSourceListChanged/value", None)
        return True

    def _publish_pcie_link(self, info: dict | None):
        """Report the PCIe link (UA_FEAT_SYSINFO_EXT) at
        /openapollo/pcie_link{,_speed,_width}; warn once when it is slower
        than x1 Gen1 (the Apollo endpoint's own link)."""
        if not info or "pcie_link_speed" not in info:
            return
        speed, width = info["pcie_link_speed"], info["pcie_link_width"]
        for path, val in (("/openapollo/pcie_link", pcie_link_text(speed, width)),
                          ("/openapollo/pcie_link_speed", speed),
                          ("/openapollo/pcie_link_width", width)):
            if self.state.path_exists(path + "/value"):
                self._push_to_clients(path, val)
                self.state._dirty.pop(path + "/value", None)
        if pcie_link_below_minimum(speed, width) and not self._link_warned:
            self._link_warned = True
            log.warning("PCIe link is %s, below x1 Gen1: audio and meters "
                        "can drop out (check the Thunderbolt cable, port "
                        "and any dock)", pcie_link_text(speed, width))

    def _check_firmware(self, info: dict | None):
        """Log one warning when UA's Mixer Engine would refuse the running
        firmware (firmware_problem()).  Informational only:
        firmware_compatibility stays 0 and nothing is refused.

        The firmware version is the driver's fw_release (BAR0 0x0008,
        UA_FEAT_FW_RELEASE).  fw_version is BAR0 0x0000, DSP 0's status
        word (0x1c00 on the x4); an older driver has only that, so only a
        value that can be a major (1-255) is judged there."""
        if self._fw_warned or not info:
            return
        rel = info.get("fw_release")
        if isinstance(rel, int) and rel not in (0, 0xFFFFFFFF):
            dt = info.get("device_type")
            cls = descriptor_mixer_class(find_descriptor(
                dt, info.get("public_type"))[0]) \
                if isinstance(dt, int) else None
            text = firmware_text(rel)
            problem = firmware_problem(info, cls)
            if problem:
                self._fw_warned = True
                log.warning("Firmware %s is not one UA's software accepts "
                            "for this model (%s): this combination is "
                            "untested", text, problem)
            else:
                log.info("Firmware %s", text)
            return
        fw = info.get("fw_version")
        if not isinstance(fw, int) or not 0 < fw < 256:
            log.debug("Firmware version word 0x%x is not a major number; "
                      "not checked", fw if isinstance(fw, int) else 0)
            return
        if fw not in ACCEPTED_FW_MAJORS:
            self._fw_warned = True
            log.warning("Firmware version %d is not one the UA driver "
                        "accepts (%s): this combination is untested",
                        fw, " or ".join(map(str, ACCEPTED_FW_MAJORS)))

    def _poll_buffer_size(self):
        """/BufferSize follows the period size of the Apollo's running
        stream (on Linux PipeWire picks it, the quantum; the Mixer Engine
        reports CoreAudio's buffer size).  Read-only: a client SET does not
        change the stream.  Nothing changes while no stream runs."""
        if not self.hw_router or not self.hw_router.backend.connected:
            return
        card = find_apollo_card(self.hw_router.backend.device_path)
        frames = stream_period_frames(card)
        if frames and self.state.get_value("/BufferSize/value") != frames:
            log.info("Buffer size -> %d frames (stream period)", frames)
            self._push_to_clients("/BufferSize", frames)
            # device status, not a setting: not saved
            self.state._dirty.pop("/BufferSize/value", None)

    def _poll_transport(self):
        """/AudioStreaming follows the driver's transport (running or
        not).  One small GET_DRIVER_PARAM at the 1 Hz pump rate; the driver
        has no event for it.  Without a device the value stays at its
        default (True)."""
        if (not self.hw_router or not self._hw_online
                or not self.hw_router.backend.connected):
            return
        running = self.hw_router.backend.get_driver_param(
            DRIVER_PARAM_TRANSPORT_RUNNING)
        if running is None:
            return
        running = bool(running)
        if self.state.get_value("/AudioStreaming/value") != running:
            log.info("Audio streaming -> %s", running)
            self._push_to_clients("/AudioStreaming", running)
            # device status, not a setting: not saved
            self.state._dirty.pop("/AudioStreaming/value", None)

    def _refresh_clock_source(self, source: int | None = None):
        """Push the driver's clock source (name) to /ClockSource."""
        if not self.hw_router or not self.hw_router.backend.connected:
            return
        self._publish_clock_sources()
        if source is None:
            source = self.hw_router.backend.get_driver_param(
                DRIVER_PARAM_CLOCK_SOURCE)
        menu = getattr(self.hw_router, "clock_sources", None) or {}
        for name, val in list(menu.items()) + list(CLOCK_SOURCE_MAP.items()):
            if val == source:
                if not self._readback_suppressed("/ClockSource/value"):
                    self._push_to_clients("/ClockSource", name)
                return

    def _refresh_all_from_driver(self):
        """A full refresh, after events were dropped: rate, clock source
        and lock state, from the same accessors the polling path uses."""
        self._poll_driver_rate(force=True)
        self._refresh_clock_source()
        rb = self.hw_router.poll_hw_readback() if self.hw_router else None
        if rb and rb.get("clock_locked") is not None:
            self._push_to_clients("/ClockLocked", bool(rb["clock_locked"]))

    def _on_driver_event(self, code: int, arg: int, time_ns: int = 0):
        """One struct ua_event from the driver (see events.py)."""
        name = EVENT_NAMES.get(code, f"0x{code:x}")
        log.debug("driver event %s arg=0x%x", name, arg)
        if code == UA_EVT_RATE_CHANGED:
            self._poll_driver_rate(force=True)
            if self._last_rate_index not in (None, arg):
                self._mute_until_settle()
            self._last_rate_index = arg
        elif code in (UA_EVT_CONNECTED, UA_EVT_RATE_INFO):
            self._poll_driver_rate(force=True)
        elif code == UA_EVT_CLOCK_CHANGED:
            self._refresh_clock_source(arg)
        elif code == UA_EVT_CLOCK_LOCK:
            self._push_to_clients("/ClockLocked", bool(arg))
        elif code == UA_EVT_DEVICE_OFFLINE:
            self._on_device_offline(
                "surprise removal" if arg else "driver unbind")
        elif code == UA_EVT_LAYOUT_CHANGED:
            # The daemon does not model the I/O layout itself; keep the
            # numbers for whoever does (the metering reads io_map on its
            # own) and count the change
            self.io_layout = (arg >> 16, arg & 0xFFFF)
            self.layout_changes += 1
            log.info("I/O layout changed: %d play / %d rec channels",
                     *self.io_layout)
        elif code == UA_EVT_PAUSED:
            if arg & UA_PAUSE_DEVICE:
                self._on_device_paused()
        elif code == UA_EVT_RESUMED:
            if arg & UA_PAUSE_DEVICE:
                self._on_device_resumed()
        elif code == UA_EVT_EVENTS_LOST:
            log.warning("Driver event queue overflowed (%d dropped): "
                        "refreshing everything", arg)
            self._refresh_all_from_driver()
        elif code in (UA_EVT_IODESC_IN, UA_EVT_IODESC_OUT,
                      UA_EVT_NOTIF_STATUS):
            pass    # raw firmware notifications: the decoded events follow

    # ── Sleep / wake (04 H6) ─────────────────────────────────────────

    def _set_hw_paused(self, paused: bool):
        self.hw_paused = bool(paused)
        if self.hw_router:
            self.hw_router.set_paused(paused)

    def _on_device_paused(self):
        """SystemSleep: the driver is suspending the device.  Whatever the
        daemon would still write (a settle mute to lift, an Identify blink)
        is finished first, then every hardware write is refused until
        RESUMED; the tree keeps taking client changes."""
        if self.hw_paused:
            return
        log.info("Device paused for system suspend: hardware writes stopped")
        self._cancel_settle()
        self.commands.on_pause()
        self._set_hw_paused(True)

    def _on_device_resumed(self):
        """SystemWake: push the whole tree again, as after a connect: the
        init sequence (preamp and monitor values from the hardware's own
        readback, as in a warm start) and the A6 re-apply of what the
        hardware cannot report back."""
        was_paused = self.hw_paused
        self._set_hw_paused(False)
        if not self.hw_router or not self._hw_online:
            return      # the watchdog owns the push for a new connection
        if not self.hw_router.backend.connected:
            return
        log.info("Device resumed%s: re-pushing the tree",
                 "" if was_paused else " (no pause was seen)")
        self._preserve_session = False
        self._push_defaults_to_hardware()
        self._hw_init_done = True
        self._readback_start_time = time.monotonic()
        self._poll_driver_rate(force=True)
        self._refresh_clock_source()
        rb = self.hw_router.poll_hw_readback()
        if rb:
            self._sync_hw_readback(rb, {})   # empty last_rb → push all

    def _poll_dsp_load(self):
        """Poll DSP load and push to state tree at ~1 Hz.

        UA's Mixer Engine sets /TotalDSPLoad, /TotalPGMLoad, /TotalMEMLoad
        from CUAD2Info::GetAllocatedLoads (kext selector 112, GetDSPInfo):
        host bookkeeping of the plugin loads and DSP memory it allocated
        plus a rate overhead, not a DSP counter.  With UA_FEAT_DSP_LOAD the
        driver reports the same figures for a DSP with no plugins (x4: DSP
        load 1.19 % at 48 kHz, PGM/MEM 0); an older driver gives 0.
        """
        if not self.hw_router or not self.hw_router.backend.connected:
            return

        get = getattr(self.hw_router.backend, "get_dsp_loads", None)
        loads = get() if callable(get) else None
        dsp_load, pgm_load, mem_load = loads or (0.0, 0.0, 0.0)

        # Push to state tree (only if changed); device status, not saved
        for path, val in [("/TotalDSPLoad", dsp_load),
                          ("/TotalPGMLoad", pgm_load),
                          ("/TotalMEMLoad", mem_load)]:
            current = self.state.get_value(path + "/value")
            try:
                same = abs(float(current) - val) <= 0.005
            except (TypeError, ValueError):
                same = False
            if not same:
                self._push_to_clients(path, val)
                self.state._dirty.pop(path + "/value", None)

    def _push_to_clients(self, path: str, value):
        """Push a hardware readback change to helper tree and state tree.

        Updates the value and notifies subscribers, but does NOT trigger
        the state tree on_set callback (which is wired to hardware write).
        This prevents a readback → state → hardware → readback loop.
        Not a session edit: /Dirty does not move.
        """
        with self.session_dirty.suppress():
            self._push_readback(path, value)

    def _push_readback(self, path: str, value):
        # Update helper tree (for 4720 console clients).  set_value()
        # notifies when it stores the value; a path the helper tree lacks
        # still reaches helper subscribers of that path once.
        if self.helper_tree:
            if self.helper_tree.set_value(path, value) != "ok":
                self.helper_tree._notify(path, value)

        # Update state tree value WITHOUT triggering on_set (hw write).
        # We bypass state.set() to avoid the on_set callback, but still
        # notify subscribers so 4710 clients see the change.
        val_path = path if path.endswith("/value") else path + "/value"
        resolved, prop = self.state._resolve_path(val_path)
        if resolved is not None and prop is not None:
            old = resolved.get("value")
            resolved["value"] = value
            if old != value:
                self.state._dirty[val_path] = value
                self.state._schedule_save()
                self.state._notify_subscribers(val_path, value, exclude=None)

    async def _handle_client(self, reader: asyncio.StreamReader,
                             writer: asyncio.StreamWriter,
                             ubjson: bool = False):
        """Handle a single client connection (either protocol)."""
        client = MixerClient(reader, writer, trace=self.trace, ubjson=ubjson)
        self.clients[client.id] = client
        log.info("Connected: %s", client)

        # Register subscription callback for this client
        def on_notify(path: str, value):
            if client.ubjson:
                resp = {"path": path, "data": value}
                client.send(ubjson_encode_response(resp))
            else:
                client.send(encode_response_bytes(path, value))

        self.state.register_callback(client.id, on_notify)

        # Push initial state to text clients (4710) on connect.
        # ConsoleLink (iPad) waits for the daemon to proactively send
        # initialized status before proceeding past the "connecting" screen.
        if not ubjson:
            for init_path in [
                "/initialized/value",
                "/initialized_percent/value",
                "/initialized_status/value",
                "/devices/0/DeviceOnline/value",
                "/AudioStreaming/value",
                "/ClockLocked/value",
                "/SampleRate/value",
                "/DeviceConnectionMode/value",
                "/TotalDSPLoad/value",
            ]:
                val = self.state.get_value(init_path)
                if val is not None:
                    client.send(encode_response_bytes(init_path, val))

            # Push current HW readback state (preamp flags, monitor, gain)
            if self.hw_router:
                rb = self.hw_router.poll_hw_readback()
                if rb:
                    self._sync_hw_readback(rb, {})  # empty last_rb → push all

            try:
                await writer.drain()
            except ConnectionError:
                pass

        try:
            while not client.closed:
                # Use wait_for so we periodically drain even without incoming data.
                # This ensures meter pump data reaches the client.
                try:
                    data = await asyncio.wait_for(reader.read(65536), timeout=0.5)
                except asyncio.TimeoutError:
                    # No incoming data — just drain pending writes (meter data etc.)
                    try:
                        await writer.drain()
                    except ConnectionError:
                        break
                    continue

                if not data:
                    break

                self._feed_client_data(client, data)

                # Flush writes
                try:
                    await writer.drain()
                except ConnectionError:
                    break
        except (ConnectionError, asyncio.CancelledError):
            pass
        except WireError as e:
            log.warning("%s: dropping the connection, unframeable input: %s",
                        client.id, e)
        finally:
            log.info("Disconnected: %s", client)
            client.closed = True
            self.state.unsubscribe_all(client.id)
            self.state.unregister_callback(client.id)
            self.clients.pop(client.id, None)
            try:
                writer.close()
                await writer.wait_closed()
            except Exception:
                pass

    def _feed_client_data(self, client: MixerClient, data: bytes):
        """Dispatch the complete commands in `data`, in order, across format
        changes.  A framed command (UBJS or JSON) sets the client's reply
        format to its own, as a UBJS frame always did; an envelope that is
        not a command is logged and skipped, the connection stays up.
        WireError (unframeable input) propagates: the caller drops the
        connection."""
        for kind, msg in client.framer.feed(data):
            if client.trace:
                log.debug("RECV ← %s: %s", client.id,
                          msg[:200] if kind == "text" else repr(msg)[:200])
            if kind == "text":
                self._dispatch(client, msg)
                continue
            cmd = self._parse_ubjson_command(msg)
            if cmd is None:
                log.warning("Unparseable %s command from %s: %r",
                            "UBJSON" if kind == "ubjson" else "JSON",
                            client.id, msg)
                continue
            if kind == "ubjson" and not client.ubjson:
                log.info("%s: Auto-detected UBJSON protocol", client.id)
            elif kind == "json" and not client.json_framed:
                log.info("%s: Auto-detected JSON-framed protocol", client.id)
            client.ubjson = kind == "ubjson"
            client.json_framed = kind == "json"
            self._dispatch_cmd(client, cmd)

    # ── Response helpers ────────────────────────────────────────────

    def _fire_init_complete(self):
        """Simulate initialization completing (false → true).

        The real UA Mixer Engine starts with initialized=false while loading
        the device tree, then fires a change to true.  UAD Console subscribes
        to /initialized and waits for the change notification before proceeding
        to load Rack.dll.
        """
        log.info("Firing initialization sequence: initialized → true")

        # Push the init sequence to hardware BEFORE readback sync takes
        # over, once per hardware connection: this runs both from the
        # startup timer and after a client's handshake.
        if not self._hw_init_done:
            self._push_defaults_to_hardware()
            if self.hw_router and self.hw_router.backend.connected:
                self._hw_init_done = True

        # Force value change even if saved state restored True —
        # reset to False first so the False→True transition fires
        # a subscriber notification that UAD Console needs.
        self.state.set_value("/initialized_percent", 0.0)
        self.state.set_value("/initialized_status", "Loading...")
        self.state.set_value("/initialized", False)
        # Now fire the real transition
        self.state.set_value("/initialized_percent", 100.0)
        self.state.set_value("/initialized_status", "")
        self.state.set_value("/initialized", True)
        self._session_ready = True

    def _push_defaults_to_hardware(self):
        """Push the init sequence (macOS order) to hardware.

        Preamp and monitor values come from the hardware readback when it
        is valid, so a daemon start keeps what the Apollo is doing,
        front-panel changes included (the readback level is the level the
        Apollo is already playing at).  Without a valid readback, safe
        defaults are used: monitor volume at minimum to protect speakers,
        preamps at 10 dB with every switch off.

        Input faders and mutes can't be read back: the ones a client set
        (the saved state) are restored, and every other input starts
        muted at 0 dB, which the state tree is told so clients show it.

        Includes bus coefficients for all 24 input channels — required
        for type 0x01 (post-DSP) capture channels (Mic 3-4, Virtual,
        S/PDIF, etc.) which route through the DSP mixer.
        """
        if not self.hw_router or self.hw_paused:
            return

        rb = self.hw_router.poll_hw_readback()
        # A snapshot dropped on its check words is not "no readback": wait
        # for the next one (the driver refreshes it at 100 Hz) rather than
        # resetting the Apollo to the defaults
        for _ in range(10):
            if rb or not self.hw_router.readback_skipped:
                break
            time.sleep(0.02)
            rb = self.hw_router.poll_hw_readback()
        rb = rb or {}
        if rb:
            log.info("Init keeps the hardware's preamp and monitor settings "
                     "(monitor %.1f dB)", rb["monitor_level_db"])
        else:
            log.info("No hardware readback: init uses safe defaults "
                     "(monitor at minimum)")
        mon_raw = rb.get("monitor_level_raw", 0)
        mon_mute = rb.get("monitor_mute", False)
        mon_mono = rb.get("monitor_mono", False)
        # Talkback also sets the dim bit, and init turns talkback off
        mon_dim = rb.get("monitor_dim", False) and not rb.get(
            "talkback_active", False)
        # Every preamp of the model (x4 0-3, x8p 0-7, Twin 0-1)
        model = self.hw_router.model
        preamps = sorted(self.hw_router.preamp_inputs())
        gain_db = {ch: rb.get(f"preamp_{ch}_gain", 10) for ch in preamps}

        def flag(ch, key):
            return 1 if rb.get(f"preamp_{ch}_{key}", False) else 0

        defaults = {
            f"/devices/0/inputs/{ch}/preamps/0/Gain/value": float(gain_db[ch])
            for ch in preamps
        }
        mon_out = self.hw_router.monitor_output_index()
        if mon_out is not None:   # x4 18, x8p 22
            defaults[f"/devices/0/outputs/{mon_out}/MixInSource/value"] = "mon"
            defaults[f"/devices/0/outputs/{mon_out}/CRMonitorLevel/value"] = \
                (mon_raw - 192) / 2.0

        # Push every input fader (x4: 24) via the state tree path (the
        # proven FaderLevel→bus coefficient pipeline).
        saved = self.state._dirty
        for i in range(self.hw_router.input_count()):
            base = f"/devices/0/inputs/{i}"
            fader = f"{base}/FaderLevel/value"
            tapered = f"{base}/FaderLevelTapered/value"
            mute = f"{base}/Mute/value"
            if fader in saved:
                defaults[fader] = saved[fader]
            elif tapered in saved:
                defaults[tapered] = saved[tapered]
            else:
                defaults[fader] = 0.0
                self._push_to_clients(fader, 0.0)
                self._push_to_clients(tapered, fader_db_to_tapered(0.0))
            if mute in saved:
                defaults[mute] = saved[mute]
            else:
                defaults[mute] = True
                self._push_to_clients(mute, True)

        for path, value in defaults.items():
            self.hw_router.on_set(path, value)

        # ── Route + Level init (macOS sends these FIRST) ─────────────
        # Route bitmask (param 0x13) tells the DSP mixer which output
        # buses each input can route to.  Without it, no input reaches
        # the monitor output.  Level (param 0x16, 0xA0 = -16 dB) sets
        # the default DSP processing level per channel.
        # DTrace capture: macOS sends Route to 19 channels and
        # Level to all 32 channels before any other mixer params.
        # The channel lists come from the model's synch-in numbers
        # (model_info.ModelInfo); the x4's are the DTrace's.
        if self.hw_router and self.hw_router.backend:
            be = self.hw_router.backend
            PREAMP = 1  # ch_type=1

            # Route bitmask for ALL input channels (macOS sends first).
            # 0x0001FFFF enables routing to buses 0-16 (MIX, CUE1,
            # CUE2, AUX, virtual outputs).
            route_chs = model.route_channels()
            for ch in route_chs:
                be.set_mixer_param(PREAMP, ch, 0x13, 0x0001FFFF)
            # S/PDIF L gets extended route mask
            spdif_l = model.spdif_left_channel()
            if spdif_l is not None:
                be.set_mixer_param(PREAMP, spdif_l, 0x13, 0x0003FFFF)

            # Default DSP processing level for ALL channels (-16 dB).
            level_chs = model.level_channels()
            for ch in level_chs:
                be.set_mixer_param(PREAMP, ch, 0x16, 0xA0)
            log.info("Sent Route (0x13) to %d channels + Level (0x16) "
                     "to %d channels", len(route_chs), len(level_chs))

            # TYPE0 DSP path routing (ch_type=0).
            # Enables S/PDIF, ADAT, and CUE bus routing paths.
            be.set_mixer_param(0, 1, 0x01, 1)  # S/PDIF output path
            be.set_mixer_param(0, 2, 0x01, 1)  # ADAT output path
            # The driver takes param_id as the enable value (SetBusEnable)
            be.set_mixer_param(0, 5, 0x01, 1)  # CUE bus routing
            be.set_mixer_param(0, 6, 0x01, 1)  # CUE bus routing
            log.info("Sent TYPE0 DSP path routing (4 calls)")

        # Disable talkback + dim matching macOS init sequence.
        # DTrace (talkback-init capture): TB_CONFIG(0x47)=1 must precede
        # TALKBACK(0x46)=0. Send to ch_idx 0 and 1 (varies by capture).
        # (The driver ignores the monitor ch_idx; 0, 1, 7 are the capture's.)
        # A model without talkback gets the dim write only.
        if self.hw_router and self.hw_router.backend:
            be = self.hw_router.backend
            for ch_idx in (0, 1, 7):
                if model.talkback:
                    be.set_mixer_param(2, ch_idx, 0x47, 1)   # TB_CONFIG
                    be.set_mixer_param(2, ch_idx, 0x48, 0)   # param 0x48
                    be.set_mixer_param(2, ch_idx, 0x46, 0)   # TALKBACK OFF
                be.set_mixer_param(2, ch_idx, 0x44, int(mon_dim))  # DIM
            log.info("Sent talkback/dim init sequence (TB_CONFIG + OFF)")

            # Full per-channel preamp init matching macOS DTrace capture
            # (apollo-linux/tools/captures/macos-full-init-20260322-decoded.txt).
            #
            # macOS order per preamp channel:
            #   1 leading  Route (0x13)       — all 4 channels first
            #   2 leading  Level (0x16 = 0xA0) — all 4 channels second
            #   3 flag block (LowCut → MicLine → 0x08×2 → GainC → Phase
            #                 → 0x07 → 0x11(ch 0-1) → PAD → 48V → Link(0,2))
            #   4 trailing Route × 2          — per channel, right after flags
            #   5 trailing Level (0x16 = 0xA0) — all 4 channels, later
            #
            # Prior Linux sequence sent only the flag block + a single
            # trailing Route and never sent Level (0x16) at all.  Symptom:
            # setting[0] bits 0/1/3 (MicLine, PAD, 48V) reached SRAM but
            # the ARM MCU never latched the physical relays — bits 4/5
            # (LowCut, Phase) worked because they are DSP-internal.
            #
            # Working hypothesis (plans/260424-0922-dsp-arm-relay-propagation/
            # research/findings.md): 0x16=0xA0 is the ARM-relay arm signal.
            # Matching macOS exactly makes hypothesis bisection unnecessary.
            link_chs = model.link_channels(preamps)
            for ch in preamps:
                be.set_mixer_param(PREAMP, ch, 0x13, 0x0001FFFF) # Route (leading)
            for ch in preamps:
                be.set_mixer_param(PREAMP, ch, 0x16, 0xA0)       # Level = -16 dB (leading)

            for ch in preamps:
                be.set_mixer_param(PREAMP, ch, 0x04, flag(ch, "lowcut"))
                be.set_mixer_param(PREAMP, ch, 0x00, flag(ch, "micline"))
                be.set_mixer_param(PREAMP, ch, 0x08, 0)          # Unknown
                be.set_mixer_param(PREAMP, ch, 0x08, 0)          # Unknown (2x)
                be.set_mixer_param(PREAMP, ch, 0x06,             # GainC = dB-9
                                   max(0, min(56, gain_db[ch] - 9)))
                be.set_mixer_param(PREAMP, ch, 0x05,             # Phase -1.0f=inv
                                   0xBF800000 if flag(ch, "phase")
                                   else 0x3F800000)
                be.set_mixer_param(PREAMP, ch, 0x07, 0)          # Unknown
                # (input param 0x11 is the HiZ impedance code, not a
                # flag: the macOS capture's value 1 only selected 1 Mohm.
                # A saved HiZImpedance is written after init; with none,
                # the hardware keeps what it has.)
                be.set_mixer_param(PREAMP, ch, 0x01, flag(ch, "pad"))
                be.set_mixer_param(PREAMP, ch, 0x03, flag(ch, "48v"))
                if ch in link_chs:  # Link on first of each pair
                    be.set_mixer_param(PREAMP, ch, 0x02, flag(ch, "link"))
                be.set_mixer_param(PREAMP, ch, 0x13, 0x0001FFFF) # Route pass 2
                be.set_mixer_param(PREAMP, ch, 0x13, 0x0001FFFF) # Route pass 3

            for ch in preamps:
                be.set_mixer_param(PREAMP, ch, 0x16, 0xA0)       # Level = -16 dB (trailing)
            log.info("Sent full preamp init: %d ch × (Route + Level + 10 flags + 2×Route) + Level",
                     len(preamps))

            # Full monitor section init — replicate macOS UA Mixer Engine
            # connect sequence. DTrace capture shows macOS sends
            # 56 unique ch_type=2 params during init. Without all of them,
            # the DSP monitor module never activates (no dim/mono buttons,
            # volume changes ignored). Order matches macOS capture.
            MON = 2  # ch_type=2 (monitor)
            # What the model does not have is not sent: params the mixer
            # kind refuses (0x87, 0x3e, ... ; model_info), talkback params
            # on a model without talkback, the HP2 params on a one-
            # headphone model, MirrorsToDigital without mirror-to-digital,
            # and DigOutMode (0x21) except on the Freud mixers, whose
            # DTrace sets it: the x8p / 8p default to ADAT and keep theirs.
            kind = self.hw_router.mixer_kind()
            skipped = set()
            if not model.talkback:
                skipped |= {0x46, 0x47, 0x48}
            if self.hw_router.output_index("hp", 1) is None:
                skipped |= {0x40, 0x4c, 0x54, 0x5c}
            if not model.mirror_to_digital:
                skipped.add(0x1e)
            if not model.digout_on_connect():
                skipped.add(0x21)
            nskipped = [0]

            def mon(ch_type, ch_idx, param, value):
                if param in skipped or not monitor_param_allowed(param, kind):
                    nskipped[0] += 1
                    return True
                return be.set_mixer_param(ch_type, ch_idx, param, value)
            mon(MON, 0, 0x6a, 5)       # Unknown init signal (sent 3x by macOS)
            mon(MON, 1, 0x21, 0)       # DigOutMode = SPDIF
            mon(MON, 1, 0x6a, 5)       # Unknown init signal
            mon(MON, 0, 0x41, 0)       # Unknown bool
            mon(MON, 1, 0x13, 2)       # Unknown config (setting[1])
            mon(MON, 1, 0x14, 2)       # Unknown config (setting[1])
            mon(MON, 0, 0x47, 1)       # TBConfig = 1
            mon(MON, 0, 0x48, 0)       # Unknown bool
            mon(MON, 1, 0x15, 0)       # Unknown (setting[2] bit28)
            mon(MON, 1, 0x20, 0)       # Unknown (setting[7] bit10)
            mon(MON, 1, 0x3a, 0)       # Unknown (setting[13])
            mon(MON, 1, 0x41, 0)       # Unknown (setting[15] bit7)
            mon(MON, 1, 0x43, 7)       # DimLevel = 7 (max attenuation)
            mon(MON, 1, 0x46, 0)       # Talkback = off
            mon(MON, 1, 0x47, 1)       # TBConfig = 1
            mon(MON, 1, 0x48, 0)       # Unknown
            mon(MON, 1, 0x67, 0)       # Unknown
            mon(MON, 1, 0x68, 0)       # Unknown
            mon(MON, 1, 0x1f, 1)       # SRConvert = on
            mon(MON, 1, 0x0a, 0)       # Unknown (setting[6])
            mon(MON, 1, 0x0f, 0)       # Unknown (setting[7])
            mon(MON, 1, 0x19, 0)       # OutPadA = off
            mon(MON, 10, 0x3b, 0)      # MirrorEnableA = off
            mon(MON, 10, 0x38, 60)     # Clock/HP routing A = 60
            mon(MON, 1, 0x1a, 0)       # OutPadB = off
            mon(MON, 10, 0x3c, 0)      # MirrorEnableB = off
            mon(MON, 10, 0x39, 60)     # Clock/HP routing B = 60
            mon(MON, 1, 0x87, 0)       # Unknown
            mon(MON, 1, 0x05, 0)       # CUE1 Mix = on (HP needs active CUE bus)
            mon(MON, 0, 0x06, 0)       # CUE1 Mono = off
            mon(MON, 10, 0x2e, 0xffffffff)  # MirrorA = disabled
            mon(MON, 1, 0x07, 0)       # CUE2 Mix = on (HP needs active CUE bus)
            mon(MON, 0, 0x08, 0)       # CUE2 Mono = off
            mon(MON, 10, 0x2f, 0xffffffff)  # MirrorB = disabled
            mon(MON, 1, 0x22, 0)       # Unknown (setting[8])
            mon(MON, 0, 0x23, 0)       # CUE1Mix alt = off
            mon(MON, 10, 0x30, 0xffffffff)  # Unknown mirror
            mon(MON, 1, 0x24, 0)       # Unknown (setting[8])
            mon(MON, 0, 0x25, 0)       # CUE2Mix alt = off
            mon(MON, 10, 0x31, 0xffffffff)  # Unknown mirror
            mon(MON, 1, 0x04, 0)       # MonitorSrc = MIX bus
            mon(MON, 1, 0x01, mon_raw) # Volume (0 = minimum without readback)
            mon(MON, 0, 0x03, 2 if mon_mute else int(mon_mono))  # 2 mute, 1 mono
            mon(MON, 10, 0x1e, 0)      # MirrorsToDigital = off
            mon(MON, 1, 0x32, 0)       # OutputRef = +4dBu
            mon(MON, 1, 0x44, int(mon_dim))  # Dim
            mon(MON, 1, 0x3e, 1)       # Unknown (setting[15] bit9)
            mon(MON, 1, 0x4b, 0)       # HP1 config (setting[33] byte0)
            mon(MON, 1, 0x64, 0)       # HP config (setting[32] byte1)
            mon(MON, 1, 0x53, 0)       # HP1 flag (setting[32] bit24)
            mon(MON, 1, 0x5b, 0)       # HP1 flag (setting[32] bit16)
            mon(MON, 1, 0x3f, 0)       # HP1 CueSrc = CUE1
            mon(MON, 1, 0x40, 1)       # HP2 CueSrc = CUE2
            mon(MON, 1, 0x4c, 0)       # HP2 config (setting[33] byte1)
            mon(MON, 1, 0x64, 0)       # HP config (setting[32] byte1, 2nd)
            mon(MON, 1, 0x54, 0)       # HP2 flag (setting[32] bit25)
            mon(MON, 1, 0x5c, 0)       # HP2 flag (setting[32] bit17)
            mon(MON, 0, 0x48, 1)       # Unknown = 1
            mon(MON, 0, 0x6a, 5)       # Channel config count (final)
            log.info("Monitor init: %d of 57 params sent (%d not for this "
                     "model)", 57 - nskipped[0], nskipped[0])

            # CUE 3 / CUE 4 sends (sub=5/6) for all 21 buses, start silent.
            # macOS sends 7 sub-params per bus; we were missing sub 5/6.
            # (The daemon used to call them "bus mute L/R"; the engine's
            # numbering makes them the HP3 / HP4 sends, parity/14 B12.)
            # Like every bus param, each has a left (side 0) and a right
            # (side 1) entry and the engine writes both; the state re-apply
            # below sets the saved CUE 3/4 send levels of a 4-cue model.
            # The buses are the map's inputs (x4: analog 1-4, S/PDIF, talkback,
            # ADAT 1-8, Virtual 1-4) plus the AUX returns of the map; a model
            # with no CUE 3 output in its map has no CUE 3/4 sends.
            from hardware import SUB_PARAM_CUE3, SUB_PARAM_CUE4
            all_buses = self.hw_router.init_send_buses() \
                if self.hw_router.output_index("cue", 2) is not None else []
            for bus in all_buses:
                for sub in (SUB_PARAM_CUE3, SUB_PARAM_CUE4):
                    be.set_mixer_bus_param(bus, sub, 0.0)
                    be.set_mixer_bus_param(bus, sub, 0.0, side=1)
            log.info("Sent CUE 3/4 sends (sub=5/6, both sides) silent for "
                     "%d buses", len(all_buses))

        # A6: what the hardware cannot report back (sends, aux, CUE, mirror,
        # alt monitors, talkback configuration, solo) comes from the saved
        # state, after the defaults so the saved value wins.  Preamp and
        # monitor values stay the readback's.
        if self.hw_router and self.hw_router.backend:
            if self.state.get_value("/TalkbackOn/value"):
                # init turned the talkback off; the tree must say so
                self._push_to_clients("/TalkbackOn", False)
            n = self.hw_router.reapply_saved_state(dict(self.state._dirty))
            log.info("Re-applied %d saved controls (sends, aux, CUE, "
                     "outputs, talkback configuration)", n)

        log.info("Pushed device map defaults to hardware "
                 "(preamps + monitor + bus coefficients)")

    @staticmethod
    def _coerce_params(params: dict | None) -> dict | None:
        """Convert query-string param values to appropriate types.

        message_id, levels, func_id etc. should be integers in UBJSON
        responses, not strings.
        """
        if not params:
            return params
        result = dict(params)
        for key in ("message_id", "levels", "cmd_id", "func_id", "recursive",
                    "commands", "flatvalue", "propinfo"):
            if key in result:
                try:
                    result[key] = int(result[key])
                except (ValueError, TypeError):
                    pass
        return result

    def _send_response(self, client: MixerClient, path: str, data,
                       params: dict | None = None):
        """Send a response in the client's protocol (JSON text or UBJSON)."""
        params = self._coerce_params(params)
        if client.ubjson:
            resp = {"path": path, "data": data}
            if params:
                resp["parameters"] = params
            client.send(ubjson_encode_response(resp))
        else:
            client.send(encode_response_bytes(path, data, params))

    def _send_error(self, client: MixerClient, path: str, verb: str):
        """Send an error response in the client's protocol."""
        if client.ubjson:
            resp = {"path": path, "error": f"Unable to resolve path for {verb}."}
            client.send(ubjson_encode_response(resp))
        else:
            client.send(encode_error_bytes(path, verb))

    # ── Command parsing ─────────────────────────────────────────────

    @staticmethod
    def _parse_ubjson_command(msg) -> Command | None:
        """Convert a framed (UBJSON or JSON) command dict to a Command.

        UAD Console sends two formats:
            cmd/url:   {"cmd": "get", "url": "/ping"}
                       {"cmd": "set", "url": "/Sleep", "data": false}
            path/data: {"path": "network_id", "data": "<uuid>"}  (identification)

        Note: value field may be "value" or "data" depending on client.
        None for anything else, a wrong-typed cmd/url/path/parameters
        included (those used to raise out of the connection handler).
        """
        if not isinstance(msg, dict):
            return None
        raw_verb, url = msg.get("cmd", ""), msg.get("url", "")
        if not isinstance(raw_verb, str) or not isinstance(url, str):
            return None
        if msg.get("parameters") is not None and \
                not isinstance(msg["parameters"], dict):
            return None
        # Try cmd/url format first (standard commands)
        verb = raw_verb.lower()
        if verb and url:
            path, recursive, propfilter, raw_params = _parse_path_with_params(url)
            value = msg.get("value")
            if value is None:
                value = msg.get("data")  # UAD Console uses "data" not "value"

            # Merge "parameters" dict if present
            params = msg.get("parameters")
            if params and isinstance(params, dict):
                raw_params.update({k: str(v) for k, v in params.items()})

            return Command(
                verb=verb,
                path=path,
                value=value,
                recursive=recursive,
                propfilter=propfilter,
                raw_params=raw_params,
            )

        # Try path/data format (JSON identification messages)
        path = msg.get("path", "")
        if isinstance(path, str) and path:
            data = msg.get("data")
            return Command(verb="json", path=path, value=data, json_data=msg)

        return None

    # ── Command dispatch ────────────────────────────────────────────

    def _dispatch(self, client: MixerClient, msg_text: str):
        """Parse a text command and dispatch it."""
        cmd = parse_command(msg_text)
        if cmd is None:
            log.warning("Unparseable message from %s: %r", client.id, msg_text[:200])
            return
        self._dispatch_cmd(client, cmd)

    def _dispatch_cmd(self, client: MixerClient, cmd: Command):
        """Dispatch a parsed Command object."""
        log.debug("%s: %s %s", client.id, cmd.verb, cmd.path)

        if cmd.verb == "get":
            self._handle_get(client, cmd)
        elif cmd.verb == "set":
            self._handle_set(client, cmd)
        elif cmd.verb == "subscribe":
            self._handle_subscribe(client, cmd)
        elif cmd.verb == "unsubscribe":
            self._handle_unsubscribe(client, cmd)
        elif cmd.verb == "json":
            self._handle_json(client, cmd)
        elif cmd.verb == "post":
            self._handle_post(client, cmd)
        else:
            log.warning("%s: unknown verb %r for path %s",
                        client.id, cmd.verb, cmd.path)
            self._send_error(client, cmd.path, cmd.verb)

    def _handle_get(self, client: MixerClient, cmd: Command):
        """Handle GET command — return control tree data."""
        # Build response params from query params (echo message_id etc.)
        resp_params = dict(cmd.raw_params) if cmd.raw_params else {}

        # Ping — keepalive health check, return immediately
        if cmd.path in ("/ping", "ping"):
            self._send_response(client, "/ping", True, resp_params or None)
            return

        # System queries (no leading slash) — match real mixer engine
        if cmd.path == "networkID":
            self._send_response(client, "networkID", self.network_id,
                                resp_params or None)
            return
        if cmd.path == "processID":
            self._send_response(client, "processID", self.process_id,
                                resp_params or None)
            return
        # Underscore variants — UAD Console uses snake_case on all ports
        if cmd.path in ("network_id", "process_id"):
            key = "network_id" if "network" in cmd.path else "process_id"
            val = self.network_id if "network" in cmd.path else self.process_id
            self._send_response(client, key, val, resp_params or None)
            return

        # Helper clients (4720) use the Mixer Helper tree
        if client.helper and self.helper_tree:
            self._handle_get_helper(client, cmd, resp_params)
            return

        data = self.state.get(cmd.path, recursive=cmd.recursive,
                              propfilter=cmd.propfilter or None)
        if data is None:
            self._send_error(client, cmd.path, "get")
            return

        if cmd.recursive:
            resp_params["recursive"] = 1
        if cmd.propfilter:
            resp_params["propfilter"] = ",".join(cmd.propfilter)

        self._send_response(client, cmd.path, data,
                            resp_params if resp_params else None)

    def _handle_get_helper(self, client: MixerClient, cmd: Command,
                           resp_params: dict):
        """Handle GET for helper clients using the Mixer Helper tree."""
        # Parse helper-specific query params
        try:
            levels = int(cmd.raw_params.get("levels", 0))
        except (ValueError, TypeError):
            levels = 0
        # recursive=1 means infinite depth (same as levels=99)
        if cmd.recursive and levels == 0:
            levels = 99
        flatvalue = str(cmd.raw_params.get("flatvalue", "0")) == "1"
        propinfo = str(cmd.raw_params.get("propinfo", "1")) != "0"
        commands = str(cmd.raw_params.get("commands", "1")) != "0"

        # Parse excluded_children (comma-separated, URL-decoded by parse_qs)
        excluded_raw = cmd.raw_params.get("excluded_children", "")
        excluded_children = None
        if excluded_raw:
            excluded_children = [c.strip() for c in excluded_raw.split(",")
                                 if c.strip()]

        data = self.helper_tree.get(
            cmd.path, levels=levels, flatvalue=flatvalue,
            propfilter=cmd.propfilter or None, propinfo=propinfo,
            commands=commands, excluded_children=excluded_children)

        if data is None:
            # Path not in helper tree — send error matching real mixer helper
            self._send_helper_error(client, cmd.path)
            return

        # Echo recursive and excluded_children in response params (real server does this)
        if cmd.recursive:
            resp_params["recursive"] = 1
        if excluded_children:
            resp_params["excluded_children"] = ",".join(excluded_children)

        self._send_response(client, cmd.path, data,
                            resp_params if resp_params else None)

    def _send_helper_error(self, client: MixerClient, path: str):
        """Send error in Mixer Helper format (uasrv.invalid_object)."""
        resp = {"path": path, "error": "uasrv.invalid_object"}
        if client.ubjson:
            client.send(ubjson_encode_response(resp))
        else:
            client.send(encode_error_bytes(path, "get"))

    def _handle_set(self, client: MixerClient, cmd: Command):
        """Handle SET command — update state and write to hardware.

        If the command has func_id, send a response echoing the value and
        func_id (matches real UA Mixer Helper behavior). SET without func_id
        gets no response.
        """
        # Some clients send lowercase /sleep instead of /Sleep
        path = cmd.path
        if path.lower() == "/sleep":
            path = "/Sleep"
        if refused_set(path, cmd.value):
            self._send_error(client, path, "set")
            return

        resp_params = dict(cmd.raw_params) if cmd.raw_params else {}
        if self._handle_clock_set(client, path, cmd.value, resp_params):
            return
        old_value = self.commands.value_of(path)

        if client.helper and self.helper_tree:
            # Try helper tree first, fall back to state tree
            result = self.helper_tree.set_value(path, cmd.value)
            if result == "read_only":
                # Read-only property — send error with func_id echoed
                if "func_id" in resp_params and client.ubjson:
                    resp = {"path": path, "parameters": self._coerce_params(resp_params),
                            "error": "uasrv.read_only"}
                    client.send(ubjson_encode_response(resp))
                return
            if result == "not_found":
                self.state.set(path, cmd.value)
            else:
                # Helper tree updated — also route to hardware.
                # Helper SET paths may omit /value suffix; normalize for
                # HardwareRouter regex patterns which expect it.
                hw_path = path if path.endswith("/value") else path + "/value"
                # A control both trees hold: mirror it into the state tree
                # (no second hardware write) so 4710 subscribers, the saved
                # state and /Dirty see the edit
                if self.state.path_exists(hw_path):
                    self.state.set(hw_path, cmd.value, source_client=client.id,
                                   hardware=False)
                if self.hw_router:
                    self.hw_router.on_set(hw_path, cmd.value)
                # Suppress readback echo for this path (300ms window)
                self._readback_suppress[path] = time.monotonic()
        else:
            self.state.set(path, cmd.value)
            # Suppress readback echo for 4710 clients too (prevents
            # gain LED blinking from readback pushing old values back)
            self._readback_suppress[path] = time.monotonic()

        # Gain and GainTapered are one hardware control.  The macOS Mixer
        # Engine echoes both on every gain SET, within ~14 ms; the SET's own
        # path is already echoed by state.set().  The companion is kept in
        # the tree (persisted, no second hardware write), pushed to the
        # other subscribers, and sent to this client in its negotiated
        # framing.  Exact preamp paths only, from the tree's (clamped) value.
        m = _RE_PREAMP_GAIN.match(path)
        if m:
            from hardware import preamp_db_to_tapered, preamp_tapered_to_db
            base, ctrl = m.groups()
            actual = self.state.get_value(f"{base}/{ctrl}/value")
            try:
                actual = float(actual)
            except (TypeError, ValueError):
                actual = None
            if actual is not None:
                if ctrl == "GainTapered":
                    companion = f"{base}/Gain/value"
                    value = preamp_tapered_to_db(actual)
                else:
                    # the hardware takes whole dB (_handle_preamp)
                    companion = f"{base}/GainTapered/value"
                    value = preamp_db_to_tapered(int(round(actual)))
                self.state.set(companion, value, source_client=client.id,
                               hardware=False)
                self._send_response(client, companion, value)

        # Monitor level SETs: keep the dB and tapered values in step (as
        # for gain), or a later readback push to the companion's stale
        # value is dropped as unchanged.  From the tree's (clamped) value
        # when the map has the control.
        m = re_mod.match(r'^(/devices/0/outputs/\d+)/'
                         r'(CRMonitorLevelTapered|CRMonitorLevel)(?:/value)?$',
                         path)
        if m:
            from hardware import (encode_monitor_level,
                                  monitor_hw_to_tapered, monitor_tapered_to_hw)
            base, ctrl = m.groups()
            stored = self.state.get_value(f"{base}/{ctrl}/value")
            try:
                v = float(cmd.value if stored is None else stored)
            except (TypeError, ValueError):
                v = None
            if v is not None and ctrl == "CRMonitorLevel":
                self._push_to_clients(f"{base}/CRMonitorLevelTapered",
                                      monitor_hw_to_tapered(
                                          encode_monitor_level(v)))
            elif v is not None:
                self._push_to_clients(f"{base}/CRMonitorLevel",
                                      (monitor_tapered_to_hw(v) - 192) / 2.0)

        # A Snapshot or channel Preset name that exists is recalled
        self.commands.on_set(path, cmd.value, old=old_value, client=client.id)

        # Send response if func_id present (real server echoes value + func_id)
        if "func_id" in resp_params:
            self._send_response(client, path, cmd.value, resp_params)

    def _handle_clock_set(self, client: MixerClient, path: str, value,
                          params: dict) -> bool:
        """/SampleRate and /ClockSource: written to the driver first and
        published only when it took the value (False for other paths).

        An invalid value never reaches the driver.  When the driver refuses
        (a source the device does not offer, no device, paused) or the
        ioctl fails, the tree, the saved state and the subscribers keep the
        previous value and the client gets the Mixer Engine's "Unable to
        set value." with its parameters (func_id).  Keeping the old value
        says nothing about what the hardware did.  Without hardware the
        value is validated against the model's list and published.
        """
        canonical = path if path.endswith("/value") else path + "/value"
        if canonical not in CLOCK_CONTROL_PATHS:
            return False
        helper_meta = None
        if client.helper and self.helper_tree:
            helper_meta = (self.helper_tree.root.get("properties") or {}).get(
                canonical.split("/")[1])
        if not self.state.path_exists(canonical) or (
                isinstance(helper_meta, dict) and helper_meta.get("read_only")):
            self._send_engine_error(client, path,
                                    "Unable to resolve path for set.", params)
            return True

        if self.hw_router:
            result = self.hw_router.set_global(canonical, value)
        else:
            names = set(CLOCK_SOURCE_MAP) | set(
                (self.state.get_prop_dict("/ClockSource") or {}).get("values")
                or [])
            result = normalize_global(canonical, value, names)
        if result is None or not result.accepted:
            log.info("%s: SET %s = %r refused (%s)", client.id, path, value,
                     result.reason if result else "no route")
            self._send_engine_error(client, path, "Unable to set value.",
                                    params)
            return True

        old_value = self.commands.value_of(canonical)
        self.state.set(canonical, result.value, hardware=False)
        if self.helper_tree:
            self.helper_tree.set_value(canonical, result.value)
        self._readback_suppress[canonical] = time.monotonic()
        self.commands.on_set(canonical, result.value, old=old_value,
                             client=client.id)
        if "func_id" in params:
            self._send_response(client, path, result.value, params)
        return True

    def _send_engine_error(self, client: MixerClient, path: str, error: str,
                           params: dict | None = None):
        """An error reply ({"path", "error"[, "parameters"]}) in the
        client's framing, with the request's parameters echoed."""
        resp: dict = {"path": path, "error": error}
        params = self._coerce_params(params)
        if params:
            resp["parameters"] = params
        if client.ubjson:
            client.send(ubjson_encode_response(resp))
        else:
            client.send((json.dumps(resp, separators=(",", ":"))
                         + "\0").encode("utf-8"))

    def _handle_subscribe(self, client: MixerClient, cmd: Command):
        """Handle SUBSCRIBE — register for notifications + send current values.

        Text clients (4710): flood all descendant property values as
        individual {path, data} messages (matches real UA Mixer Engine).

        UBJSON clients (4720): send a single acknowledgment with the value
        at the subscribed path, echoing message_id in parameters (matches
        real UA Mixer Helper).

        Batch subscribe: after "subscribe mode=multi", the client sends
        "subscribe {\"paths\":[\"/path1\",...]}" with a JSON body listing
        all paths to subscribe to at once.
        """
        path = cmd.path

        # Handle "subscribe mode=multi" — just acknowledge, enable batch mode
        if path == "mode=multi":
            log.debug("%s: subscribe mode=multi acknowledged", client.id)
            return

        # Handle batch subscribe: subscribe {"paths":["/path1","/path2",...]}
        if path.startswith("{"):
            try:
                batch = json.loads(path)
                paths = batch.get("paths", [])
            except (json.JSONDecodeError, AttributeError):
                log.warning("%s: failed to parse batch subscribe JSON", client.id)
                return
            log.debug("%s: batch subscribe for %d paths", client.id, len(paths))
            for p in paths:
                self._subscribe_single(client, p, cmd.raw_params)
            return

        self._subscribe_single(client, path, cmd.raw_params)

    def _subscribe_single(self, client: MixerClient, path: str,
                           raw_params: dict | None = None):
        """Subscribe to a single path and send current value."""
        if client.helper and self.helper_tree:
            # Helper clients subscribe against the helper tree
            self.helper_tree.subscribe(client.id, path)
            params = dict(raw_params) if raw_params else None
            # Recursive subscribes always get null (container subscription ack)
            recursive = str(params.get("recursive", "0")) if params else "0"
            if recursive and recursive != "0":
                self._send_response(client, path, None, params)
                log.debug("SUBSCRIBE %s: recursive ack (null) to %s", path, client.id)
                return
            # Non-recursive: get value from helper tree first, fall back to state tree
            node_val = self.helper_tree.get_value(path)
            if node_val is None:
                node_val = self.state.get_value(path)
            self._send_response(client, path, node_val, params)
            log.debug("SUBSCRIBE %s: ack sent to %s (helper)", path, client.id)
            return

        # Check if path exists in the tree
        if not self.state.path_exists(path):
            # Silently register so future SETs can notify
            self.state.subscribe(client.id, path)
            if client.ubjson:
                # UBJSON: acknowledge with null data + echoed params
                params = dict(raw_params) if raw_params else None
                self._send_response(client, path, None, params)
            return

        # Register for future change notifications
        self.state.subscribe(client.id, path)

        if client.ubjson:
            # UBJSON protocol (both 4710 and 4720): single ack with current
            # value + echoed params (matches real UA Mixer Helper behavior)
            params = dict(raw_params) if raw_params else None
            node_val = self.state.get_value(path)
            self._send_response(client, path, node_val, params)
            log.debug("SUBSCRIBE %s: ack sent to %s (ubjson)", path, client.id)
        else:
            # Text mode (Mixer Engine): flood all descendant values
            values = self.state.enumerate_values(path, recursive=True)
            for vpath, value in values:
                client.send(encode_response_bytes(vpath, value))
            if values:
                log.debug("SUBSCRIBE %s: sent %d current values to %s",
                          path, len(values), client.id)

    def _handle_unsubscribe(self, client: MixerClient, cmd: Command):
        """Handle UNSUBSCRIBE — stop notifications."""
        self.state.unsubscribe(client.id, cmd.path)

    def _handle_json(self, client: MixerClient, cmd: Command):
        """Handle raw JSON identification messages from clients.

        Clients send messages like:
            {"path":"networkID","data":"<uuid>"}
            {"path":"client_id","data":"mixer"}

        These identify the client to the server. We log them and
        optionally echo back our own identity.
        """
        log.info("%s: JSON ident: %s = %r", client.id, cmd.path, cmd.value)

    def _handle_post(self, client: MixerClient, cmd: Command):
        """Handle POST commands (auth challenge, command format, etc).

        Known post commands:
            post /request_challenge?func_id=N   — auth challenge request
            post command_format?func_id=N 2     — protocol version negotiation
        """
        resp_params = dict(cmd.raw_params) if cmd.raw_params else None

        if cmd.path == "command_format" or cmd.path == "/command_format":
            # Replies switch now (data={} as the real Mixer Helper sends);
            # the input framer keeps taking the trailing text commands the
            # client sends before its first frame (set /sleep, post
            # /request_challenge ...).  1 = JSON frames, 2 (and anything
            # else, as before) = UBJS frames: the two modes of UA's client
            # serializer (json_frame_codec).
            mode_one = type(cmd.value) in (int, str) and cmd.value in (1, "1")
            client.ubjson = not mode_one
            client.json_framed = mode_one
            self._send_response(client, "command_format", {}, resp_params)
            log.info("%s: command_format %s acknowledged", client.id,
                     "1 (JSON frames)" if mode_one else "2 (UBJSON)")
            return

        if cmd.path == "/request_challenge":
            # Auth challenge — return a valid UANP blob so the console can
            # parse it and post /response.  We accept any response (no real
            # crypto validation).  The blob is base64-encoded: "UANP" magic
            # + version 1 + 128 bytes of challenge data.  Captured from
            # real UA Mixer Engine on Windows.
            challenge = ("VUFOUAEAAAATrGLORDvLq9bW6lc1xqFjHYXUQQeGEJsa"
                         "RSu+yvgUZqlX190QaE1QhoAKBbXFZVcgAAAAAAAAAHuM"
                         "9gh1uMKwsorBqnN+s+WuVuHII6K0ROz0xK7PjQYs")
            self._send_response(client, "/request_challenge",
                                challenge, resp_params)
            log.debug("%s: POST /request_challenge → UANP challenge sent",
                      client.id)
            return

        if cmd.path == "/response":
            # Client sends solved challenge (base64 string).
            # Real server responds with {"path": "/response", "data": true}.
            # We don't validate the challenge — just accept it.
            self._send_response(client, "/response", True, resp_params)
            log.debug("%s: POST /response → accepted", client.id)
            # Trigger delayed initialization sequence (false → true)
            if not self._init_fired:
                self._init_fired = True
                asyncio.get_event_loop().call_later(
                    1.0, self._fire_init_complete)
            return

        # Mixer Helper /auto_gain/{start,stop}_auto_gain
        if client.helper and self.autogain.owns(cmd.path):
            data, error = self.autogain.execute(
                cmd.path, parse_cmdparams(cmd.value, cmd.raw_params))
            params = {k: v for k, v in (resp_params or {}).items()
                      if k != "cmdparams"} or None
            self._send_post_reply(client, cmd.path, data, error, params)
            return

        # Object commands (`post <object>/<CommandName>`): the engine's
        # result, or its error string
        data, error = self.commands.execute(cmd.path, cmd.value,
                                            cmd.raw_params)
        params = {k: v for k, v in (resp_params or {}).items()
                  if k != "cmdparams"} or None
        self._send_post_reply(client, cmd.path, data, error, params)

    def _send_post_reply(self, client: MixerClient, path: str, data,
                         error: str | None, params: dict | None):
        """Answer a `post` command.  The error reply is the get/set one
        ({"path", "error"}) with the engine's text; success is
        {"path", "data"}, data being true or the command's result.  The
        request's parameters (func_id) are echoed, as for set."""
        params = self._coerce_params(params)
        resp: dict = {"path": path}
        if error is not None:
            resp["error"] = error
        else:
            resp["data"] = data
        if params:
            resp["parameters"] = params
        if client.ubjson:
            client.send(ubjson_encode_response(resp))
        else:
            client.send((json.dumps(resp, separators=(",", ":"))
                         + "\0").encode("utf-8"))


def device_map_path(slug: str) -> Path:
    """Captured map file for a devices/<slug>.json model: apollo-x8p -> device_map_apollo_x8p.json."""
    return SCRIPT_DIR / "device_maps" / f"device_map_{slug.replace('-', '_')}.json"


def find_descriptor(device_type: int,
                    public_type: int | None = None) -> tuple[str | None, str | None]:
    """Resolve a driver device_type to (slug, model) via devices/*.json.

    Each devices/*.json descriptor lists its driver "device_types" (hex strings)
    and its "model" (e.g. "Apollo x8p"). (None, None) if no descriptor matches.

    When no descriptor lists device_type and the driver reported the kext's
    public type (GET_SYSTEM_INFO), the descriptor whose "public_types" holds
    it is used: UA's Mixer Engine picks the model from that type
    (CUAD2Info::CardType).  That names a type-0 unit on a driver that does
    not retype it, and the serial types 0x1E (2016, the Arrow), 0x2A (2032)
    and 0x3E (2110, the Solo) the driver leaves unnamed.
    """
    if hardware_usb and hardware_usb.is_usb_device_type(device_type):
        return hardware_usb.descriptor_for_device_type(device_type)
    if not DEVICES_DIR.is_dir():
        return None, None
    found = _find_descriptor_by(device_type, ("device_types", "device_type"))
    if found[0] is None and isinstance(public_type, int) and \
            0 < public_type <= 0x29:
        found = _find_descriptor_by(public_type, ("public_types",))
    return found


def _find_descriptor_by(value: int, keys: tuple) -> tuple[str | None, str | None]:
    """The first devices/*.json listing `value` under one of `keys`."""
    for desc_path in sorted(DEVICES_DIR.glob("*.json")):
        try:
            desc = json.loads(desc_path.read_text())
        except (OSError, ValueError) as e:   # JSONDecodeError is a ValueError
            log.warning("Ignoring unreadable device descriptor %s: %s", desc_path, e)
            continue
        if not isinstance(desc, dict):
            log.warning("Ignoring device descriptor %s: not a JSON object", desc_path)
            continue

        types = []
        for key in keys:
            v = desc.get(key)
            if isinstance(v, list):
                types.extend(v)
            elif v is not None:
                types.append(v)
        match = False
        for dt in types:
            try:
                # Descriptors store hex strings ("0x20"). Base 0 honours the prefix
                # rather than reading a bare "32" as hex (= 50).
                dt_value = dt if isinstance(dt, int) else int(str(dt), 0)
            except ValueError:
                log.warning("Ignoring device descriptor %s: bad %s %r",
                            desc_path, keys[0], dt)
                continue
            if dt_value == value:
                match = True
                break
        if not match:
            continue

        model = desc.get("model")
        if not isinstance(model, str) or not model.strip():
            model = None
        return desc_path.stem, model
    return None, None


def descriptor_input_buses(slug: str | None) -> dict:
    """Input name -> bus ID (devices/<slug>.json mixer_inputs synch_in).

    Handed to the HardwareRouter, whose name rule covers every model but the
    Twin MkII's talkback input (synch-in 2).  {} when there is no usable
    descriptor.
    """
    if not slug:
        return {}
    try:
        desc = json.loads((DEVICES_DIR / f"{slug}.json").read_text())
        return {e["name"]: int(e["synch_in"])
                for e in desc.get("mixer_inputs") or []
                if e.get("name") != "UNAVAILABLE"
                and e.get("synch_in") is not None}
    except (OSError, ValueError, KeyError, TypeError, AttributeError):
        return {}


def stream_period_frames(card: int | None) -> int | None:
    """Period size in frames of the card's running playback (else
    capture) stream, from /proc/asound/cardN/pcm0{p,c}/sub0/hw_params, or
    None when nothing runs."""
    if card is None:
        return None
    for d in ("pcm0p", "pcm0c"):
        try:
            text = Path(f"/proc/asound/card{card}/{d}/sub0/hw_params"
                        ).read_text()
        except OSError:
            continue
        for line in text.splitlines():
            if line.startswith("period_size:"):
                try:
                    return int(line.split(":", 1)[1])
                except ValueError:
                    return None
    return None


def descriptor_json(slug: str | None) -> dict | None:
    """devices/<slug>.json as a dict, or None."""
    if not slug:
        return None
    try:
        desc = json.loads((DEVICES_DIR / f"{slug}.json").read_text())
        return desc if isinstance(desc, dict) else None
    except (OSError, ValueError):
        return None


def descriptor_model(slug: str | None) -> ModelInfo | None:
    """The init-sequence facts of devices/<slug>.json, or None (the router
    then assumes the Apollo x4)."""
    if not slug:
        return None
    try:
        desc = json.loads((DEVICES_DIR / f"{slug}.json").read_text())
        return ModelInfo.from_descriptor(slug, desc)
    except (OSError, ValueError, KeyError, TypeError, AttributeError):
        return None


def lookup_device(device_type: int, public_type: int | None = None
                  ) -> tuple[str | None, Path | None]:
    """Resolve a driver device_type (else the public type) to
    (model, captured_map_path).

    The path is None if the model has no captured map (x4 and x8p do).
    """
    slug, model = find_descriptor(device_type, public_type)
    if slug is None:
        return None, None
    candidate = device_map_path(slug)
    return model, (candidate if candidate.exists() else None)


def generate_device_map(slug: str) -> Path | None:
    """Generate (or reuse) the cached map for a model, via tools/gen-device-map.py.

    The map lives in $XDG_CACHE_HOME/open-apollo/device_maps (default
    ~/.cache/open-apollo/device_maps), keyed on the x4 template, the model's
    JSON and the generator, so it is rebuilt when any of them changes.
    Returns None if the generator is missing or fails.
    """
    gen_path = SCRIPT_DIR.parent / "tools" / "gen-device-map.py"
    try:
        spec = importlib.util.spec_from_file_location("gen_device_map", gen_path)
        if spec is None or spec.loader is None:
            return None
        mod = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(mod)
        return Path(mod.ensure_cached(slug))
    except Exception as e:   # missing tools/, bad JSON, unwritable cache ...
        log.warning("Could not generate a device map for %s: %s", slug, e)
        return None


def device_model_name(device_type: int, public_type: int | None = None) -> str:
    """Human-readable model name for a device_type (for logging)."""
    model, _ = lookup_device(device_type, public_type)
    return model or f"device_type 0x{device_type:02x}"


def find_device_map(device_type: int | None = None,
                    public_type: int | None = None) -> Path | None:
    """Find the best available device map, preferring the attached device's own.

    The model is the descriptor listing device_type, else the one listing
    the driver's public type (find_descriptor).  Order, when the connected
    hardware's model is known:
      1. the model's captured map (live UA Mixer Engine session: x4, x8p);
      2. a map generated from devices/<model>.json (tools/gen-device-map.py:
         x4 structure with the model's channel counts), cached under
         ~/.cache/open-apollo/device_maps, with a warning;
      3. the Apollo x4 map, with a warning that controls/routing may not match.
    """
    if device_type is not None:
        slug, model = find_descriptor(device_type, public_type)
        label = model or f"device_type 0x{device_type:02x}"
        if slug is not None:
            captured = device_map_path(slug)
            if captured.exists():
                log.info("Selected device map for %s", label)
                return captured
            generated = generate_device_map(slug)
            if generated is not None and generated.exists():
                log.warning("Using a generated device map for %s (%s): structure "
                            "from the Apollo x4 capture, channels from devices/%s.json; "
                            "not captured from this model.", label, generated, slug)
                return generated
            log.warning("No device map for %s (device_type 0x%02x) — falling back "
                        "to Apollo x4. Controls/routing may not match this device.",
                        label, device_type)
        else:
            log.warning("Unrecognised device_type 0x%02x (no matching descriptor in "
                        "%s) — falling back to Apollo x4. Controls/routing may not "
                        "match this device.", device_type, DEVICES_DIR)

    # Default location (Apollo x4)
    if DEFAULT_DEVICE_MAP.exists():
        return DEFAULT_DEVICE_MAP

    # Check ConsoleLink project
    consolelink_map = Path.home() / "Documents/GitHub/ConsoleLink/UAD Console Network/device_maps/device_map_apollo_x4.json"
    if consolelink_map.exists():
        return consolelink_map

    return None


PORT_STRIDE = 100  # per-device-index offset for the default TCP/WS ports


def device_index(path) -> int:
    """N of a /dev/ua_apolloN node (symlinks resolved); 0 when unknown."""
    if not path:
        return 0
    m = re_mod.search(r"ua_apollo(\d+)$", os.path.realpath(str(path)))
    return int(m.group(1)) if m else 0


def state_file_for(device_map: Path, index: int,
                   serial: "int | str | None" = None) -> Path:
    """Per-device state file.  /dev/ua_apollo0 keeps the historical
    <map>.state.json name; other indexes get .devN (and the serial when the
    driver reported one) so two Apollos of one model never share a file.

    `serial` is the legacy numeric serial (suffix "-%08x", unchanged so
    existing files keep their names) or the real serial string from
    GET_SYSTEM_INFO (suffix "-<serial>", reduced to [A-Za-z0-9_]).
    """
    stem = device_map.stem
    if index:
        stem += f".dev{index}"
        if isinstance(serial, str):
            safe = re_mod.sub(r"[^A-Za-z0-9_]", "", serial)
            if safe:
                stem += f"-{safe}"
        elif serial:
            stem += f"-{serial:08x}"
    return SCRIPT_DIR / "state" / f"{stem}.state.json"


def state_serial(info: dict | None) -> "int | str | None":
    """Serial for state_file_for().  The legacy numeric serial wins when it
    is non-zero (the name an existing install already uses); it reads 0 on
    the x4, where the real string from GET_SYSTEM_INFO takes over."""
    if not info:
        return None
    return info.get("serial") or info.get("serial_str") or None


def seed_state_file(path: Path, legacy: Path) -> bool:
    """First start with a serial-suffixed name: copy the unsuffixed
    .devN state (what this index used before the serial was known) so the
    saved mixer settings carry over.  The old file stays; it is a copy, not
    a rename, because it may belong to another unit that used this index."""
    if path == legacy or path.exists() or not legacy.exists():
        return False
    try:
        path.parent.mkdir(exist_ok=True)
        shutil.copyfile(legacy, path)
    except OSError as e:
        log.warning("Could not seed %s from %s: %s", path, legacy, e)
        return False
    log.info("State file %s seeded from %s", path.name, legacy.name)
    return True


def make_backend(args):
    """HardwareUSB for a USB Apollo (--usb, or no /dev/ua_apollo* and one on
    the bus: hardware_usb.select_backend), else HardwareBackend."""
    if hardware_usb and hardware_usb.select_backend(
            args.device, getattr(args, "usb", False)) == "usb":
        log.info("Hardware: USB backend (%s)", "--usb" if args.usb else
                 "a USB Apollo is plugged in and no /dev/ua_apollo* exists")
        return hardware_usb.HardwareUSB(
            safe_mode=args.safe_mode,
            knob_writeback=getattr(args, "usb_knob_writeback", False),
            dsp_params=getattr(args, "usb_dsp_params", False))
    if getattr(args, "usb", False):
        log.error("--usb: USB backend unavailable (tools/uausb missing)")
    return HardwareBackend(args.device, safe_mode=args.safe_mode)


def main():
    parser = argparse.ArgumentParser(
        description="UA Mixer Engine daemon for Linux",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Examples:
  %(prog)s                          Run with auto-detected device map
  %(prog)s --no-hardware            Software-only mode (for testing)
  %(prog)s -v --port 4711           Verbose, alternate port
  %(prog)s --device-map custom.json Use custom device map

Two or more Apollos: one daemon per device.  Ports default to the base
port + 100 x the device index, so --device /dev/ua_apollo1 serves TCP 4810,
helper 4820, WS 4821 with its own state file; pass --port/--helper-port/
--ws-port to override, and --name to tell the Bonjour entries apart.
""")
    parser.add_argument("--device-map", "-m", type=Path,
                        help="Path to device map JSON")
    parser.add_argument("--host", default=DEFAULT_HOST,
                        help=f"Bind address (default: {DEFAULT_HOST})")
    parser.add_argument("--port", "-p", type=int, default=None,
                        help=f"Mixer Engine TCP port (default: {DEFAULT_PORT} "
                             f"+ {PORT_STRIDE} x device index)")
    parser.add_argument("--helper-port", type=int, default=None,
                        help=f"Mixer Helper TCP port (default: {DEFAULT_HELPER_PORT} "
                             f"+ {PORT_STRIDE} x device index)")
    parser.add_argument("--no-helper", action="store_true",
                        help="Disable Mixer Helper TCP:4720 server")
    parser.add_argument("--device", "-d",
                        help="Device node, e.g. /dev/ua_apollo1 (default: the first "
                             "/dev/ua_apollo*). One daemon serves one Apollo: "
                             "run one instance per device")
    hardware_mode = parser.add_mutually_exclusive_group()
    hardware_mode.add_argument("--no-hardware", action="store_true",
                               help="Software-only mode (no hardware writes)")
    hardware_mode.add_argument("--require-hardware", action="store_true",
                               help="Exit (status 1) when the device node "
                                    "cannot be opened at start, instead of "
                                    "waiting for it")
    parser.add_argument("--preserve-hardware", action="store_true",
                        help="Attach to a running Apollo without the init "
                             "sequence: publish what the hardware reports "
                             "and write nothing until a client changes "
                             "something (a reconnect re-inits as usual)")
    parser.add_argument("--no-metering", action="store_true",
                        help="Do not open a meter source (PipeWire/ALSA "
                             "capture or readback); meters read silence")
    parser.add_argument("--safe-mode", action="store_true",
                        help="Enable safe mode (block unmapped DSP writes)")
    parser.add_argument("--ws-port", type=int, default=None,
                        help=f"WebSocket port for UA Connect (default: {DEFAULT_WS_PORT} "
                             f"+ {PORT_STRIDE} x device index)")
    parser.add_argument("--no-ws", action="store_true",
                        help="Disable WebSocket server")
    parser.add_argument("--no-bonjour", action="store_true",
                        help="Disable Bonjour/mDNS announcement")
    parser.add_argument("--alsa-device",
                        help="ALSA device for metering (default: auto-detect)")
    parser.add_argument("--name", "-n",
                        help="Bonjour service name (default: hostname)")
    parser.add_argument("--mute-on-rate-change", type=float,
                        default=MUTE_ON_RATE_CHANGE_SECS, metavar="SECONDS",
                        help="Mute the monitor this long after the sample "
                             "rate changed, as the Mixer Engine does "
                             f"(default {MUTE_ON_RATE_CHANGE_SECS:g}, "
                             "0 = off)")
    parser.add_argument("--stereo-sends", action="store_true",
                        help="also write the right (side-1) entry of every "
                             "send and of talkback, with the sub-0 pan law "
                             "where the send has a pan (default off until a "
                             "listening test; needs a driver with "
                             "UA_FEAT_BUS_PARAM_SIDE)")
    parser.add_argument("--autogain-any-model", action="store_true",
                        help="offer /auto_gain on any model with preamps "
                             "(default: only where the device map has "
                             "SupportsAutogain, the Gen 2 models)")
    parser.add_argument("--usb", action="store_true",
                        help="drive a USB Apollo (Solo/Twin/Twin X USB) through "
                             "its vendor interface; chosen by itself when no "
                             "/dev/ua_apollo* exists and one is plugged in")
    parser.add_argument("--usb-knob-writeback", action="store_true",
                        help="USB: write a front-panel monitor change back "
                             "into the monitor setting (issue #59; unverified)")
    parser.add_argument("--usb-dsp-params", action="store_true",
                        help="USB: send fader/pan/send and DSP input params "
                             "as DSP commands on the bulk pipe (unverified)")
    parser.add_argument("--verbose", "-v", action="store_true",
                        help="Debug logging")
    parser.add_argument("--trace", "-t", action="store_true",
                        help="Protocol trace (log every message sent/received)")
    args = parser.parse_args()

    # Logging
    level = logging.DEBUG if args.verbose else logging.INFO
    logging.basicConfig(
        level=level,
        format="%(asctime)s %(name)s %(levelname)s %(message)s",
        datefmt="%H:%M:%S")

    # Open hardware early (unless --no-hardware) so we can identify the attached
    # Apollo model and load the device map that matches it.
    backend = None
    device_type = None
    public_type = None
    info = None
    if not args.no_hardware:
        backend = make_backend(args)
        if backend.open():
            info = read_device_info(backend)
            if info:
                device_type = info["device_type"]
                public_type = info.get("public_type")
                log.info("Detected %s (device_type=0x%02x, subsystem=0x%04x, "
                         "%d DSPs, serial=%s)",
                         device_model_name(device_type, public_type),
                         device_type,
                         info["subsystem_id"], info["num_dsps"],
                         serial_text(info))
            else:
                log.warning("Driver did not report device info "
                            "(pre-GET_DEVICE_INFO?) — using default device map")
        elif args.require_hardware:
            log.error("Hardware required but the device node could not be "
                      "opened (%s): check that the driver is loaded and the "
                      "node's permissions", args.device or "/dev/ua_apollo*")
            sys.exit(1)
        else:
            # Keep the backend: the daemon follows the device and picks it
            # up when it appears (udev starts us on add; replug/reload)
            log.warning("No hardware device found yet — waiting for "
                        "/dev/ua_apollo* (DeviceOnline=false until it appears)")

    index = device_index(backend.device_path if backend else args.device)
    serial = state_serial(info)
    if args.port is None:
        args.port = DEFAULT_PORT + PORT_STRIDE * index
    if args.helper_port is None:
        args.helper_port = DEFAULT_HELPER_PORT + PORT_STRIDE * index
    if args.ws_port is None:
        args.ws_port = DEFAULT_WS_PORT + PORT_STRIDE * index

    # Find device map (prefer one matching the attached device; --device-map wins)
    device_map = args.device_map or find_device_map(device_type, public_type)
    if not device_map or not device_map.exists():
        log.error("Device map not found. Searched:")
        log.error("  %s", DEFAULT_DEVICE_MAP)
        log.error("  ~/Documents/GitHub/ConsoleLink/.../device_map_apollo_x4.json")
        log.error("Use --device-map to specify the path.")
        sys.exit(1)

    # Load state tree
    state = StateTree()
    state.load_device_map(device_map)

    # Add runtime-only properties that the real mixer engine provides
    # but aren't in the captured device map (initialization state, etc.)
    apply_runtime_properties(state)

    # Enable persistent state (saves modified values across restarts)
    state_file = state_file_for(device_map, index, serial)
    if isinstance(serial, str):
        seed_state_file(state_file, state_file_for(device_map, index))
    state_file.parent.mkdir(exist_ok=True)
    state.enable_persistence(state_file)
    store_stem = state_file.name.removesuffix(".state.json")
    network_id = load_network_id(state_file.with_name(
        state_file.name.removesuffix(".state.json") + ".network.json"))

    # Load Mixer Helper tree (separate data model for 4720 clients)
    helper_tree = None
    helper_tree_path = DEFAULT_HELPER_TREE
    if helper_tree_path.exists():
        helper_tree = HelperTree()
        helper_tree.load(helper_tree_path)
    else:
        log.warning("Helper tree not found: %s — 4720 clients will use device map",
                     helper_tree_path)
    fill_helper_identity(helper_tree, info)

    # Wire up the hardware router (backend was opened above for device detection)
    hw_router = None
    if backend is not None:
        hw_router = HardwareRouter(
            backend, state, stereo_sends=args.stereo_sends,
            input_buses=descriptor_input_buses(
                find_descriptor(device_type, public_type)[0]
                if device_type is not None else None),
            model=descriptor_model(
                find_descriptor(device_type, public_type)[0]
                if device_type is not None else None))
        # Per-model facts (monitor taper, engine feature flags)
        hw_router.set_model_data(load_descriptor(
            find_descriptor(device_type)[0]
            if device_type is not None else None))
        # The settle mute uses OutputsAttenuate, which only some mixer
        # types support (OUTPUTS_ATTENUATE_CLASSES)
        hw_router.supports_outputs_attenuate = descriptor_mixer_class(
            find_descriptor(device_type, public_type)[0]
            if device_type is not None else None) \
            in OUTPUTS_ATTENUATE_CLASSES
        log.info("Hardware backend: %s", backend.device_path)
        log.info("Safe mode: %s (monitor+preamp+analog bus enabled, "
                 "digital/send/aux bus writes %s)",
                 "ON" if args.safe_mode else "OFF",
                 "blocked" if args.safe_mode else "enabled")
    elif args.no_hardware:
        log.info("Software-only mode (--no-hardware)")

    # Initialize software metering (through PipeWire when it holds the
    # Apollo, else ALSA capture)
    if not args.no_hardware and not args.no_metering:
        metering = make_meter(args.alsa_device, device_type,
                              backend.device_path
                              if backend and backend.connected else None,
                              model_desc=descriptor_json(
                                  find_descriptor(device_type)[0]
                                  if device_type is not None else None))
        metering.start()
    else:
        metering = NullMeter()

    # Create and run daemon
    helper_port = args.helper_port if not args.no_helper else 0
    daemon = MixerDaemon(state, hw_router, args.host, args.port,
                         helper_port=helper_port, trace=args.trace,
                         helper_tree=helper_tree, metering=metering,
                         network_id=network_id,
                         loaded_map=None if args.device_map else device_map,
                         preserve_hardware=args.preserve_hardware)
    daemon.autogain.any_model = args.autogain_any_model
    daemon.mute_on_rate_change = max(0.0, args.mute_on_rate_change)
    daemon.model_data = hw_router.model_data if hw_router else {}
    daemon.commands.set_store(state_file.parent, store_stem)

    # Create WebSocket server (shares state tree + hw router)
    ws_server = None
    if not args.no_ws and WsServer and HAS_WEBSOCKETS:
        ws_server = WsServer(
            state, hw_router,
            host=args.host, port=args.ws_port,
            trace=args.trace,
            network_id=daemon.network_id,
            process_id=daemon.process_id,
            commands=daemon.commands,
        )
    elif not args.no_ws and not HAS_WEBSOCKETS:
        log.warning("websockets package not installed — WS server disabled "
                     "(pip install websockets)")

    # Start Bonjour announcement
    bonjour = None
    if not args.no_bonjour:
        bonjour = BonjourAnnouncer(
            port=args.port,
            name=args.name or (f"{_default_name()} {index + 1}" if index else None),
            helper_port=helper_port or None)
        bonjour.start()

    log.info("Starting ua-mixer-daemon...")
    log.info("  Device map: %s", device_map)
    log.info("  Controls: %d", state.control_count)
    log.info("  Hardware: %s",
             ("connected" if backend.connected else "waiting for device")
             if hw_router else "software-only")
    log.info("  Metering: %s", type(metering).__name__
             if not args.no_hardware and not args.no_metering
             else "disabled")
    log.info("  TCP (Mixer Engine): tcp://%s:%d", args.host, args.port)
    log.info("  TCP (Mixer Helper): %s",
             f"tcp://{args.host}:{helper_port}" if helper_port else "disabled")
    log.info("  WS (UA Connect): %s",
             f"ws://{args.host}:{args.ws_port}" if ws_server else "disabled")
    log.info("  Bonjour: %s", "enabled" if bonjour and bonjour.zc else "disabled")

    async def run_all():
        """Run TCP servers, meter pump, heartbeat, and (optionally) WS server."""
        tasks = [
            asyncio.create_task(daemon.start()),
            asyncio.create_task(daemon._meter_pump()),
            asyncio.create_task(daemon._meter_pulse_heartbeat()),
            asyncio.create_task(daemon._device_watchdog()),
            asyncio.create_task(daemon.autogain.run()),
        ]
        if ws_server:
            tasks.append(asyncio.create_task(ws_server.start()))
        await asyncio.gather(*tasks)

    try:
        asyncio.run(run_all())
    except KeyboardInterrupt:
        log.info("Shutting down...")
    finally:
        metering.stop()
        state.save_now()
        if bonjour:
            bonjour.stop()


if __name__ == "__main__":
    main()
