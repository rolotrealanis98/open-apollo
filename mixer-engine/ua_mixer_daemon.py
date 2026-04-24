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
import json
import logging
import os
import re as re_mod
import signal
import sys
import time
from pathlib import Path

from protocol import (MessageFramer, Command, parse_command, encode_response_bytes,
                      encode_error_bytes, _parse_path_with_params)
from state_tree import StateTree
from helper_tree import HelperTree
from hardware import HardwareBackend, HardwareRouter, preamp_db_to_tapered
from metering import AlsaMeter, NullMeter, SILENCE_DB
from bonjour import BonjourAnnouncer
from ubjson_codec import encode_response as ubjson_encode_response, UbjsonFramer

try:
    from ws_server import WsServer, HAS_WEBSOCKETS
except ImportError:
    HAS_WEBSOCKETS = False
    WsServer = None

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
    "/DanteDevicesStatus": {"type": "string", "value": ""},
    "/DanteErrorStatus": {"type": "string", "value": ""},
    "/CorrectionLicenseState": {"type": "int", "value": 0},
}
# NOTE: Do NOT add "/BufferSize/values" or other "/Foo/values" paths here!
# set_value("/Foo/values", x) resolves to the /Foo property and overwrites its
# "value" field (not "values"), corrupting the actual control value.

# Default device map location (relative to this script)
SCRIPT_DIR = Path(__file__).parent
DEFAULT_DEVICE_MAP = SCRIPT_DIR / "device_maps" / "device_map_apollo_x4.json"
DEFAULT_HELPER_TREE = SCRIPT_DIR / "device_maps" / "helper_tree.json"
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
        self.framer = MessageFramer()
        self.peer = writer.get_extra_info("peername")
        self.server_port = (writer.get_extra_info("sockname") or (None, None))[1]
        self.closed = False
        self.trace = trace
        self.helper = ubjson  # True for 4720 (helper) clients
        self.ubjson = False   # Switched to True after command_format 2
        self._send_count = 0

    def send(self, data: bytes):
        """Queue data for sending to this client."""
        if not self.closed:
            try:
                self.writer.write(data)
                self._send_count += 1
                if self.trace:
                    if self.ubjson:
                        log.debug("SEND → %s: [UBJSON %d bytes]", self.id, len(data))
                    else:
                        text = data.decode("utf-8", errors="replace").rstrip("\x00")
                        log.debug("SEND → %s: %s", self.id, text[:200])
            except (ConnectionError, RuntimeError):
                self.closed = True

    def __repr__(self):
        if self.helper:
            proto = "UBJSON" if self.ubjson else "helper-text"
        else:
            proto = "text"
        return f"<MixerClient {self.id} :{self.server_port} {proto} {self.peer}>"


class MixerDaemon:
    """TCP server for UA Mixer Engine (:4710) and UA Mixer Helper (:4720)."""

    def __init__(self, state: StateTree, hw_router: HardwareRouter | None,
                 host: str = DEFAULT_HOST, port: int = DEFAULT_PORT,
                 helper_port: int = DEFAULT_HELPER_PORT,
                 trace: bool = False,
                 helper_tree: HelperTree | None = None,
                 metering=None):
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
        self._last_meter: dict[str, float | bool] = {}  # delta detection

        # Generate unique IDs matching real mixer engine format (hex UUIDs)
        import uuid
        self.network_id = uuid.uuid4().hex
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

    async def start(self):
        """Start TCP servers on both ports (runs forever)."""
        self.server = await asyncio.start_server(
            self._handle_text_client, self.host, self.port,
            reuse_address=True)

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
        r'/devices/0/(inputs|outputs)/(\d+)/'
        r'(?:sends/\d+/)?'
        r'meters/(\d+)/'
        r'(MeterLevel|MeterPeakLevel|MeterClip|Clip)/value$'
    )

    async def _meter_pump(self):
        """Send periodic meter values to subscribed clients.

        ConsoleLink subscribes to MeterLevel, MeterPeakLevel, and
        MeterClip paths and expects periodic updates.  Without these,
        it may consider the connection stale.

        Also syncs hardware readback -> console: when a physical knob
        is turned on the Apollo, the new value is pushed to all
        subscribed clients so the UI tracks the hardware.

        Meter values come from AlsaMeter (software metering from ALSA
        PCM capture), not from hardware readback registers.

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

            # Poll DSP load at ~1 Hz (every 50 ticks of the 50 Hz pump)
            dsp_tick += 1
            if dsp_tick >= 50:
                dsp_tick = 0
                self._poll_dsp_load()

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

        Returns dict with keys: path, kind, channel, meter_idx, prop
        or None if not a meter path.
        """
        m = cls._METER_PATH_RE.search(path)
        if not m:
            return None

        raw_prop = m.group(4)
        if raw_prop == 'MeterLevel':
            prop = 'level'
        elif raw_prop == 'MeterPeakLevel':
            prop = 'peak'
        else:  # MeterClip or Clip
            prop = 'clip'

        return {
            'path': path,
            'kind': m.group(1),       # 'inputs' or 'outputs'
            'channel': int(m.group(2)),
            'meter_idx': int(m.group(3)),
            'prop': prop,
        }

    def _read_meter(self, entry: dict):
        """Read a single meter value from the AlsaMeter.

        Maps device-map input/output indices to ALSA capture channels.
        """
        if entry['kind'] == 'inputs':
            level, peak, clip = self._metering.get_input_meter(
                entry['channel'])
        else:
            level, peak, clip = self._metering.get_output_meter(
                entry['channel'])

        if entry['prop'] == 'level':
            return level
        elif entry['prop'] == 'peak':
            return peak
        elif entry['prop'] == 'clip':
            return clip
        return SILENCE_DB

    def _readback_suppressed(self, path: str) -> bool:
        """Check if a readback push should be suppressed (recent client SET)."""
        ts = self._readback_suppress.get(path)
        if ts is not None:
            if time.monotonic() - ts < 2.0:  # 2s — suppress readback after SET
                return True
            del self._readback_suppress[path]
        return False

    def _sync_hw_readback(self, rb: dict, last_rb: dict):
        """Push hardware readback changes to helper tree + subscribers.

        Compares current readback with previous values. When a field
        changes (physical knob turned, button pressed), updates the
        helper tree and notifies all subscribed console clients.

        Readback pushes are suppressed for 300ms after a client SET
        to prevent echo/trailing on UI controls.
        """
        # Monitor level (tapered 0.0-1.0)
        tapered = rb.get("monitor_level_tapered", 0.0)
        path = "/devices/0/outputs/18/CRMonitorLevelTapered"
        if abs(tapered - last_rb.get("monitor_level_tapered", -1)) > 0.002:
            if not self._readback_suppressed(path):
                self._push_to_clients(path, tapered)

        # Monitor mute
        mute = rb.get("monitor_mute", False)
        if mute != last_rb.get("monitor_mute"):
            self._push_to_clients(
                "/devices/0/outputs/18/Mute", mute)

        # Monitor dim
        dim = rb.get("monitor_dim", False)
        if dim != last_rb.get("monitor_dim"):
            self._push_to_clients(
                "/devices/0/outputs/18/DimOn", dim)

        # HP1 level (tapered 0.0-1.0) — output 19 on Apollo x4
        hp1_path = "/devices/0/outputs/19/CRMonitorLevelTapered"
        hp1_tap = rb.get("hp1_level_tapered", 0.0)
        if abs(hp1_tap - last_rb.get("hp1_level_tapered", -1)) > 0.002:
            if not self._readback_suppressed(hp1_path):
                self._push_to_clients(hp1_path, hp1_tap)

        # HP2 level (tapered 0.0-1.0) — output 20 on Apollo x4
        hp2_path = "/devices/0/outputs/20/CRMonitorLevelTapered"
        hp2_tap = rb.get("hp2_level_tapered", 0.0)
        if abs(hp2_tap - last_rb.get("hp2_level_tapered", -1)) > 0.002:
            if not self._readback_suppressed(hp2_path):
                self._push_to_clients(hp2_path, hp2_tap)

        # Monitor mono
        mono = rb.get("monitor_mono", False)
        if mono != last_rb.get("monitor_mono"):
            self._push_to_clients(
                "/devices/0/outputs/18/MixToMono", mono)

        # Talkback active
        talkback = rb.get("talkback_active", False)
        if talkback != last_rb.get("talkback_active"):
            self._push_to_clients(
                "/devices/0/outputs/18/TalkbackOn", talkback)

        # Preamp gain readback — 4 individual channels.
        # rb_data[3]: ch0=[7:0], ch1=[15:8], ch2=[23:16], ch3=[31:24]
        for ch in range(4):
            gain_dB = rb.get(f"preamp_{ch}_gain", 10)
            last_gain = last_rb.get(f"preamp_{ch}_gain")
            if gain_dB != last_gain:
                gain_raw = rb.get(f"preamp_{ch}_gain_raw", 0)
                log.info("GAIN ch%d: raw=0x%02x dB=%d", ch, gain_raw, gain_dB)
                gain_path = f"/devices/0/inputs/{ch}/preamps/0/Gain"
                tapered_path = f"/devices/0/inputs/{ch}/preamps/0/GainTapered"
                dragging = (self._readback_suppressed(gain_path) or
                            self._readback_suppressed(tapered_path) or
                            self._readback_suppressed(gain_path + "/value") or
                            self._readback_suppressed(tapered_path + "/value"))
                if not dragging:
                    self._push_to_clients(gain_path, float(gain_dB))
                    self._push_to_clients(tapered_path, preamp_db_to_tapered(gain_dB))

        # Preamp controls (4 channels) — flags + IOType
        for ch in range(4):
            prefix = f"preamp_{ch}"
            path_base = f"/devices/0/inputs/{ch}/preamps/0"

            for ctrl, key in [("48V", "48v"), ("Pad", "pad"),
                              ("LowCut", "lowcut"), ("Phase", "phase")]:
                val = rb.get(f"{prefix}_{key}", False)
                if val != last_rb.get(f"{prefix}_{key}"):
                    self._push_to_clients(f"{path_base}/{ctrl}", val)

            # IOType is at input level, not preamps/0 level
            micline = rb.get(f"{prefix}_micline", False)
            if micline != last_rb.get(f"{prefix}_micline"):
                iotype = "Line" if micline else "Mic"
                self._push_to_clients(
                    f"/devices/0/inputs/{ch}/IOType", iotype)

    def _poll_dsp_load(self):
        """Poll DSP load and push to state tree at ~1 Hz.

        On macOS, DSP load comes from SEL170 (GetPluginDSPLoad) which
        returns an IEEE 754 float (0-100%) from the SHARC DSP.  On Linux,
        we query each DSP's running status via GET_DSP_INFO ioctl.

        The full SEL170-equivalent (which returns actual load percentage)
        would require a DSP ring buffer command — not yet implemented.
        For now, we report whether DSPs are active (binary 0/100%) and
        will refine to actual load percentages when we identify the
        correct ring buffer query or register.
        """
        if not self.hw_router or not self.hw_router.backend.connected:
            return

        # Real DSP load requires SEL170-equivalent (ring buffer query to
        # SHARC DSP).  The binary running/idle status from GET_DSP_INFO is
        # not useful — it's always 100% when DSP is alive.  Report 0% until
        # we implement actual load polling.
        dsp_load = 0.0

        # PGM/MEM load require deeper DSP queries (ring buffer commands)
        pgm_load = 0.0
        mem_load = 0.0

        # Push to state tree (only if changed)
        for path, val in [("/TotalDSPLoad", dsp_load),
                          ("/TotalPGMLoad", pgm_load),
                          ("/TotalMEMLoad", mem_load)]:
            current = self.state.get_value(path)
            if current is None or abs(float(current) - val) > 0.1:
                self._push_to_clients(path, val)

    def _push_to_clients(self, path: str, value):
        """Push a hardware readback change to helper tree and state tree.

        Updates the value and notifies subscribers, but does NOT trigger
        the state tree on_set callback (which is wired to hardware write).
        This prevents a readback → state → hardware → readback loop.
        """
        # Update helper tree (for 4720 console clients)
        if self.helper_tree:
            self.helper_tree.set_value(path, value)
            # Notify helper subscribers
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

                # Auto-detect UBJSON or switch framer when UBJSON data arrives.
                # After command_format 2, client.ubjson=True but framer may
                # still be text MessageFramer — switch when we see UBJS magic.
                #
                # CRITICAL: UBJS magic may appear MID-BUFFER when the client
                # sends trailing text commands and UBJSON frames in the same
                # TCP segment.  We must scan the combined buffer (framer
                # leftover + new data), process text before UBJS, then switch.
                if not isinstance(client.framer, UbjsonFramer):
                    should_scan = client.ubjson or (
                        len(data) >= 4 and data[:4] == b"UBJS")
                    if should_scan:
                        combined = client.framer.buf + data
                        ubjs_idx = combined.find(b"UBJS")
                        if ubjs_idx >= 0:
                            # Drain text commands before the UBJS boundary
                            text_part = combined[:ubjs_idx]
                            ubjs_part = combined[ubjs_idx:]
                            client.framer.buf = b""
                            if text_part:
                                for msg in client.framer.feed(text_part):
                                    if client.trace:
                                        log.debug("RECV ← %s: %s",
                                                  client.id, msg[:200])
                                    self._dispatch(client, msg)
                            # Switch to UBJSON framer
                            client.framer = UbjsonFramer()
                            if not client.ubjson:
                                client.ubjson = True
                                log.info("%s: Auto-detected UBJSON protocol",
                                         client.id)
                            else:
                                log.debug("%s: Framer switched to UBJSON "
                                          "(mid-buffer at offset %d)",
                                          client.id, ubjs_idx)
                            data = ubjs_part

                messages = client.framer.feed(data)
                for msg in messages:
                    if isinstance(msg, dict):
                        # UBJSON command dict from UbjsonFramer
                        if client.trace:
                            log.debug("RECV ← %s: %r", client.id, msg)
                        cmd = self._parse_ubjson_command(msg)
                        if cmd:
                            self._dispatch_cmd(client, cmd)
                        else:
                            log.warning("Unparseable UBJSON from %s: %r",
                                        client.id, msg)
                    else:
                        # Text message from MessageFramer
                        if client.trace:
                            log.debug("RECV ← %s: %s", client.id, msg[:200])
                        self._dispatch(client, msg)

                # Flush writes
                try:
                    await writer.drain()
                except ConnectionError:
                    break
        except (ConnectionError, asyncio.CancelledError):
            pass
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

    # ── Response helpers ────────────────────────────────────────────

    def _fire_init_complete(self):
        """Simulate initialization completing (false → true).

        The real UA Mixer Engine starts with initialized=false while loading
        the device tree, then fires a change to true.  UAD Console subscribes
        to /initialized and waits for the change notification before proceeding
        to load Rack.dll.
        """
        log.info("Firing initialization sequence: initialized → true")

        # Push state tree defaults to hardware BEFORE readback sync
        # takes over.  On cold boot, all mixer settings are zeroed; this
        # ensures hardware matches the desired state (device map defaults
        # or restored saved state).
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

    def _push_defaults_to_hardware(self):
        """Push device map defaults to hardware on init.

        After cold boot, all mixer settings are zeroed.  This pushes
        the ORIGINAL device map default values (not current state tree,
        which may have been overwritten by client SETs) to hardware.

        Includes bus coefficients for all 24 input channels — required
        for type 0x01 (post-DSP) capture channels (Mic 3-4, Virtual,
        S/PDIF, etc.) which route through the DSP mixer.
        """
        if not self.hw_router:
            return

        # Safe defaults — monitor volume at MINIMUM to protect speakers.
        # User must raise volume intentionally after boot.
        defaults = {
            "/devices/0/inputs/0/preamps/0/Gain/value": 10.0,
            "/devices/0/inputs/1/preamps/0/Gain/value": 10.0,
            "/devices/0/inputs/2/preamps/0/Gain/value": 10.0,
            "/devices/0/inputs/3/preamps/0/Gain/value": 10.0,
            "/devices/0/outputs/18/MixInSource/value": "mon",
            "/devices/0/outputs/18/CRMonitorLevel/value": -96.0,
        }

        # Push all 24 input fader defaults via state tree path.
        # This uses the proven FaderLevel→bus coefficient pipeline.
        # Mute all inputs by default — no input monitoring until user unmutes.
        for i in range(24):
            defaults[f"/devices/0/inputs/{i}/FaderLevel/value"] = 0.0
            defaults[f"/devices/0/inputs/{i}/Mute/value"] = True

        for path, value in defaults.items():
            self.hw_router.on_set(path, value)

        # ── Route + Level init (macOS sends these FIRST) ─────────────
        # Route bitmask (param 0x13) tells the DSP mixer which output
        # buses each input can route to.  Without it, no input reaches
        # the monitor output.  Level (param 0x16, 0xA0 = -16 dB) sets
        # the default DSP processing level per channel.
        # DTrace capture: macOS sends Route to 19 channels and
        # Level to all 32 channels before any other mixer params.
        # Currently limited to ch 0-3 (physical preamps) — kernel
        # setting encoding for extended ch_idx (8+) needs kext RE.
        if self.hw_router and self.hw_router.backend:
            be = self.hw_router.backend
            PREAMP = 1  # ch_type=1

            # Route bitmask for ALL input channels (macOS sends first).
            # 0x0001FFFF enables routing to buses 0-16 (MIX, CUE1,
            # CUE2, AUX, virtual outputs).
            route_chs = [0,1,2,3, 8,9,10, 12,13,14,15,
                         20,21,22,23, 24,25,26,27]
            for ch in route_chs:
                be.set_mixer_param(PREAMP, ch, 0x13, 0x0001FFFF)
            # S/PDIF L gets extended route mask
            be.set_mixer_param(PREAMP, 8, 0x13, 0x0003FFFF)

            # Default DSP processing level for ALL channels (-16 dB).
            level_chs = list(range(16)) + list(range(20, 32))
            for ch in level_chs:
                be.set_mixer_param(PREAMP, ch, 0x16, 0xA0)
            log.info("Sent Route (0x13) to %d channels + Level (0x16) "
                     "to %d channels", len(route_chs), len(level_chs))

            # TYPE0 DSP path routing (ch_type=0).
            # Enables S/PDIF, ADAT, and CUE bus routing paths.
            be.set_mixer_param(0, 1, 0x01, 1)  # S/PDIF output path
            be.set_mixer_param(0, 2, 0x01, 1)  # ADAT output path
            be.set_mixer_param(0, 5, 0x00, 1)  # CUE bus routing
            be.set_mixer_param(0, 6, 0x00, 1)  # CUE bus routing
            log.info("Sent TYPE0 DSP path routing (4 calls)")

        # Disable talkback + dim matching macOS init sequence.
        # DTrace (talkback-init capture): TB_CONFIG(0x47)=1 must precede
        # TALKBACK(0x46)=0. Send to ch_idx 0 and 1 (varies by capture).
        if self.hw_router and self.hw_router.backend:
            be = self.hw_router.backend
            for ch_idx in (0, 1, 7):
                be.set_mixer_param(2, ch_idx, 0x47, 1)   # TB_CONFIG
                be.set_mixer_param(2, ch_idx, 0x48, 0)   # param 0x48
                be.set_mixer_param(2, ch_idx, 0x46, 0)   # TALKBACK OFF
                be.set_mixer_param(2, ch_idx, 0x44, 0)   # DIM OFF
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
            for ch in range(4):
                be.set_mixer_param(PREAMP, ch, 0x13, 0x0001FFFF) # Route (leading)
            for ch in range(4):
                be.set_mixer_param(PREAMP, ch, 0x16, 0xA0)       # Level = -16 dB (leading)

            for ch in range(4):
                be.set_mixer_param(PREAMP, ch, 0x04, 0)          # LowCut=off
                be.set_mixer_param(PREAMP, ch, 0x00, 0)          # Mic/Line=Mic
                be.set_mixer_param(PREAMP, ch, 0x08, 0)          # Unknown
                be.set_mixer_param(PREAMP, ch, 0x08, 0)          # Unknown (2x)
                be.set_mixer_param(PREAMP, ch, 0x06, 1)          # GainC=1 (10dB)
                be.set_mixer_param(PREAMP, ch, 0x05, 0x3F800000) # Phase=+1.0f
                be.set_mixer_param(PREAMP, ch, 0x07, 0)          # Unknown
                if ch <= 1:  # HiZ-capable channels only
                    be.set_mixer_param(PREAMP, ch, 0x11, 1)      # HiZ flag
                be.set_mixer_param(PREAMP, ch, 0x01, 0)          # PAD=off
                be.set_mixer_param(PREAMP, ch, 0x03, 0)          # 48V=off
                if ch in (0, 2):  # Link on first of each pair
                    be.set_mixer_param(PREAMP, ch, 0x02, 0)      # Link=off
                be.set_mixer_param(PREAMP, ch, 0x13, 0x0001FFFF) # Route pass 2
                be.set_mixer_param(PREAMP, ch, 0x13, 0x0001FFFF) # Route pass 3

            for ch in range(4):
                be.set_mixer_param(PREAMP, ch, 0x16, 0xA0)       # Level = -16 dB (trailing)
            log.info("Sent full preamp init: 4 ch × (Route + Level + 10 flags + 2×Route) + Level")

            # Full monitor section init — replicate macOS UA Mixer Engine
            # connect sequence. DTrace capture shows macOS sends
            # 56 unique ch_type=2 params during init. Without all of them,
            # the DSP monitor module never activates (no dim/mono buttons,
            # volume changes ignored). Order matches macOS capture.
            mon = be.set_mixer_param  # shorthand
            MON = 2  # ch_type=2 (monitor)
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
            mon(MON, 1, 0x01, 0)       # Volume = 0 (minimum, SAFE)
            mon(MON, 0, 0x03, 0)       # Mute = off
            mon(MON, 10, 0x1e, 0)      # MirrorsToDigital = off
            mon(MON, 1, 0x32, 0)       # OutputRef = +4dBu
            mon(MON, 1, 0x44, 0)       # Dim = off
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
            log.info("Monitor init: full 57-param sequence sent")

            # Bus mute L/R (sub=5/6) for all 21 buses.
            # macOS sends 7 sub-params per bus; we were missing sub 5/6.
            # All buses init to 0.0 (muted) except talkback which gets
            # passthrough. The daemon's fader init handles talkback later.
            from hardware import SUB_PARAM_MUTE_L, SUB_PARAM_MUTE_R
            all_buses = [0x00, 0x01, 0x02, 0x03,       # Analog In 1-4
                         0x08, 0x09,                     # S/PDIF L/R
                         0x0a,                           # Talkback
                         0x0c, 0x0d, 0x0e, 0x0f,         # ADAT 1-4
                         0x10, 0x12,                     # AUX 1/2 Return
                         0x14, 0x15, 0x16, 0x17,         # ADAT 5-8
                         0x18, 0x19, 0x1a, 0x1b]         # Virtual 1-4
            for bus in all_buses:
                be.set_mixer_bus_param(bus, SUB_PARAM_MUTE_L, 0.0)
                be.set_mixer_bus_param(bus, SUB_PARAM_MUTE_R, 0.0)
            log.info("Sent bus mute L/R (sub=5/6) for %d buses", len(all_buses))

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
    def _parse_ubjson_command(msg: dict) -> Command | None:
        """Convert a UBJSON command dict to a Command object.

        UAD Console sends two formats:
            cmd/url:   {"cmd": "get", "url": "/ping"}
                       {"cmd": "set", "url": "/Sleep", "data": false}
            path/data: {"path": "network_id", "data": "<uuid>"}  (identification)

        Note: value field may be "value" or "data" depending on client.
        """
        # Try cmd/url format first (standard commands)
        verb = msg.get("cmd", "").lower()
        url = msg.get("url", "")
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
        if path:
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

        resp_params = dict(cmd.raw_params) if cmd.raw_params else {}

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
                if self.hw_router:
                    self.hw_router.on_set(hw_path, cmd.value)
                # Suppress readback echo for this path (300ms window)
                self._readback_suppress[path] = time.monotonic()
        else:
            self.state.set(path, cmd.value)
            # Suppress readback echo for 4710 clients too (prevents
            # gain LED blinking from readback pushing old values back)
            self._readback_suppress[path] = time.monotonic()

        # For gain SETs: push BOTH Gain and GainTapered to the client.
        # macOS Mixer Engine does this — every gain SET echoes both values
        # as subscription pushes within ~14ms.
        if "preamps/0/Gain" in path:
            import re as _re
            m = _re.search(r'/inputs/(\d+)/preamps/0/(GainTapered|Gain)', path)
            if m:
                ch, ctrl = int(m.group(1)), m.group(2)
                from hardware import preamp_tapered_to_db, preamp_db_to_tapered
                base = f"/devices/0/inputs/{ch}/preamps/0"
                if ctrl == "GainTapered":
                    db = int(round(preamp_tapered_to_db(float(cmd.value))))
                    tapered = float(cmd.value)
                elif ctrl == "Gain":
                    db = int(round(float(cmd.value)))
                    tapered = preamp_db_to_tapered(db)
                else:
                    db = None
                if db is not None:
                    # Only send the COMPANION value — the original path
                    # is already pushed by state.set() subscriber echo.
                    # macOS sends exactly 2 messages: Gain + GainTapered.
                    from protocol import encode_response_bytes
                    if ctrl == "GainTapered":
                        # iPad sent GainTapered → push Gain (dB) only
                        client.send(encode_response_bytes(
                            f"{base}/Gain/value", float(db)))
                    else:
                        # iPad sent Gain → push GainTapered only
                        client.send(encode_response_bytes(
                            f"{base}/GainTapered/value", tapered))

        # Send response if func_id present (real server echoes value + func_id)
        if "func_id" in resp_params:
            self._send_response(client, path, cmd.value, resp_params)

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
            if client.ubjson:
                # Already in UBJSON mode (auto-detected from UBJS magic).
                # Respond with data={} matching real Mixer Helper format.
                self._send_response(client, "command_format", {}, resp_params)
                log.info("%s: command_format acknowledged (already UBJSON)",
                         client.id)
            elif client.helper:
                # Helper port text client switching to UBJSON responses.
                # IMPORTANT: Do NOT switch the framer here! The client
                # continues sending text commands (set /sleep, post
                # /request_challenge, subscribe, etc.) after command_format.
                # The framer auto-switches to UbjsonFramer when the client
                # starts sending UBJS-framed data (detected in _handle_client).
                client.ubjson = True
                self._send_response(client, "command_format", {}, resp_params)
                log.info("%s: Switched to UBJSON responses (command_format 2, "
                         "framer stays text until UBJS detected)",
                         client.id)
            else:
                # Text client sending command_format 2 → switch response
                # format to UBJSON. Keep the text framer for incoming data
                # because the client may send a few more text commands
                # before switching to UBJSON. The auto-detection code
                # (UBJS magic check in _handle_client) will switch the
                # framer when actual UBJSON data arrives.
                client.ubjson = True
                self._send_response(client, "command_format", {}, resp_params)
                log.info("%s: Switched to UBJSON responses (command_format 2)",
                         client.id)
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

        log.debug("%s: POST %s (value=%r) — ignored", client.id, cmd.path, cmd.value)


def find_device_map() -> Path | None:
    """Find the best available device map."""
    # Check default location
    if DEFAULT_DEVICE_MAP.exists():
        return DEFAULT_DEVICE_MAP

    # Check ConsoleLink project
    consolelink_map = Path.home() / "Documents/GitHub/ConsoleLink/UAD Console Network/device_maps/device_map_apollo_x4.json"
    if consolelink_map.exists():
        return consolelink_map

    return None


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
""")
    parser.add_argument("--device-map", "-m", type=Path,
                        help="Path to device map JSON")
    parser.add_argument("--host", default=DEFAULT_HOST,
                        help=f"Bind address (default: {DEFAULT_HOST})")
    parser.add_argument("--port", "-p", type=int, default=DEFAULT_PORT,
                        help=f"Mixer Engine TCP port (default: {DEFAULT_PORT})")
    parser.add_argument("--helper-port", type=int, default=DEFAULT_HELPER_PORT,
                        help=f"Mixer Helper TCP port (default: {DEFAULT_HELPER_PORT})")
    parser.add_argument("--no-helper", action="store_true",
                        help="Disable Mixer Helper TCP:4720 server")
    parser.add_argument("--device", "-d",
                        help="Device path (default: auto-detect /dev/ua_apollo*)")
    parser.add_argument("--no-hardware", action="store_true",
                        help="Software-only mode (no hardware writes)")
    parser.add_argument("--safe-mode", action="store_true",
                        help="Enable safe mode (block unmapped DSP writes)")
    parser.add_argument("--ws-port", type=int, default=DEFAULT_WS_PORT,
                        help=f"WebSocket port for UA Connect (default: {DEFAULT_WS_PORT})")
    parser.add_argument("--no-ws", action="store_true",
                        help="Disable WebSocket server")
    parser.add_argument("--no-bonjour", action="store_true",
                        help="Disable Bonjour/mDNS announcement")
    parser.add_argument("--alsa-device",
                        help="ALSA device for metering (default: auto-detect)")
    parser.add_argument("--name", "-n",
                        help="Bonjour service name (default: hostname)")
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

    # Find device map
    device_map = args.device_map or find_device_map()
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
    for rpath, rprops in RUNTIME_PROPERTIES.items():
        if not state.path_exists(rpath):
            state.add_runtime_property(rpath, rprops)
            log.debug("Added runtime property: %s = %r", rpath, rprops.get("value"))
        else:
            # Force-set existing properties to runtime values
            # (e.g. DeviceOnline defaults to False in device map but must be True)
            state.set_value(rpath, rprops["value"])
            log.debug("Set runtime value: %s = %r", rpath, rprops.get("value"))

    # Enable persistent state (saves modified values across restarts)
    state_file = SCRIPT_DIR / "state" / f"{device_map.stem}.state.json"
    state_file.parent.mkdir(exist_ok=True)
    state.enable_persistence(state_file)

    # Load Mixer Helper tree (separate data model for 4720 clients)
    helper_tree = None
    helper_tree_path = DEFAULT_HELPER_TREE
    if helper_tree_path.exists():
        helper_tree = HelperTree()
        helper_tree.load(helper_tree_path)
    else:
        log.warning("Helper tree not found: %s — 4720 clients will use device map",
                     helper_tree_path)

    # Initialize hardware backend
    hw_router = None
    if not args.no_hardware:
        safe_mode = args.safe_mode
        backend = HardwareBackend(args.device, safe_mode=safe_mode)
        if backend.open():
            hw_router = HardwareRouter(backend, state)
            log.info("Hardware backend: %s", backend.device_path)
            log.info("Safe mode: %s (monitor+preamp+analog bus enabled, "
                     "digital/send/aux bus writes %s)",
                     "ON" if safe_mode else "OFF",
                     "blocked" if safe_mode else "enabled")
        else:
            log.warning("No hardware device found — running in software-only mode")
    else:
        log.info("Software-only mode (--no-hardware)")

    # Initialize software metering (ALSA PCM capture)
    if not args.no_hardware:
        metering = AlsaMeter(device=args.alsa_device, capture_channels=22)
        metering.start()
    else:
        metering = NullMeter()

    # Create and run daemon
    helper_port = args.helper_port if not args.no_helper else 0
    daemon = MixerDaemon(state, hw_router, args.host, args.port,
                         helper_port=helper_port, trace=args.trace,
                         helper_tree=helper_tree, metering=metering)

    # Create WebSocket server (shares state tree + hw router)
    ws_server = None
    if not args.no_ws and WsServer and HAS_WEBSOCKETS:
        ws_server = WsServer(
            state, hw_router,
            host=args.host, port=args.ws_port,
            trace=args.trace,
            network_id=daemon.network_id,
            process_id=daemon.process_id,
        )
    elif not args.no_ws and not HAS_WEBSOCKETS:
        log.warning("websockets package not installed — WS server disabled "
                     "(pip install websockets)")

    # Start Bonjour announcement
    bonjour = None
    if not args.no_bonjour:
        bonjour = BonjourAnnouncer(port=args.port, name=args.name)
        bonjour.start()

    log.info("Starting ua-mixer-daemon...")
    log.info("  Device map: %s", device_map)
    log.info("  Controls: %d", state.control_count)
    log.info("  Hardware: %s", "connected" if hw_router else "software-only")
    log.info("  Metering: %s", "ALSA capture" if metering.available else
             ("starting..." if not args.no_hardware else "disabled"))
    log.info("  TCP:4710 (Mixer Engine): tcp://%s:%d", args.host, args.port)
    log.info("  TCP:4720 (Mixer Helper): %s",
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
