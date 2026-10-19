"""The mixer daemon over a recording fake backend, for end-to-end tests of
clients (console-rs: console-rs/crates/engine-model/tests/daemon_hardware.rs).

It runs MixerDaemon with the Apollo x4 device map and a HardwareRouter whose
backend only records what it is asked to write.  It never opens a device
node, ALSA, PipeWire or /sys, saves no state, starts no helper, WebSocket or
Bonjour service, and listens on 127.0.0.1 only: on an ephemeral port, or on
--port when that is outside the daemon's own port ranges.  Once the server
is up it prints "LISTENING <port>" on stdout.

Every hardware write and every change the daemon publishes is appended to
--log as one JSON line:

    {"hw": "set_mixer_param", "args": [2, 0, 70, 1]}
    {"tree": "/TalkbackOn/value", "value": true}

Driver behaviour can be scripted: --refuse-rate HZ makes the driver refuse
that sample rate, --refuse-clock NAME that clock source, and a non-internal
clock source reports the clock unlocked (UA_EVT_CLOCK_LOCK 0) unless
--external-locks is given.  --stereo-sends is the daemon's option (send
pans move a left/right pair).  --catalogue DIR overlays a ua-catalogue
folder (console-rs/tools/drive/fixtures/catalogue is a synthetic one), so
the plug-in browser has rows.  --native-slots also publishes native hosting's
capability properties for that catalogue (NATIVE-HOSTING.md): "UAD Test Comp"
loads stereo only and "UAD Test EQ" mono only, on inputs 1-4; the rest none.
--pipewire-fixture FILE serves that pw-dump JSON as the PipeWire the daemon
polls (Settings › PIPEWIRE; tests/fixtures/pipewire_dump.json is a synthetic
one); its pw-metadata writes are logged as {"hw": "pw-metadata", ...} and
change the fixture.  Without it PipeWire is not available.
"""

import argparse
import asyncio
import json
import logging
import re
import sys
import tempfile
from pathlib import Path

ENGINE_DIR = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ENGINE_DIR))

import hardware  # noqa: E402
import pipewire_state  # noqa: E402
import ua_catalogue  # noqa: E402
import ua_mixer_daemon  # noqa: E402
from events import UA_EVT_CLOCK_LOCK, UA_EVT_RATE_CHANGED  # noqa: E402
from hardware import (CLOCK_SOURCE_MAP, DRIVER_PARAM_CLOCK_SOURCE,  # noqa: E402
                      DRIVER_PARAM_SAMPLE_RATE, HardwareBackend,
                      HardwareRouter)
from state_tree import StateTree  # noqa: E402
from ua_mixer_daemon import (MixerDaemon, apply_runtime_properties,  # noqa: E402
                             descriptor_input_buses, descriptor_mixer_class,
                             descriptor_model, load_descriptor,
                             OUTPUTS_ATTENUATE_CLASSES)

DEVICE_MAP = ENGINE_DIR / "device_maps" / "device_map_apollo_x4.json"
MODEL = "apollo-x4"
RATE_INDEX = {44100: 1, 48000: 2, 88200: 3, 96000: 4, 176400: 5, 192000: 6}

# Never a real daemon's port: 4710/4720/4721 plus 100 per device index
FORBIDDEN_PORTS = range(4700, 6000)


class RecordingBackend:
    """A HardwareBackend that writes nothing anywhere but the log."""

    # HardwareBackend's own fader/pan helpers, run on this recorder, so the
    # log shows the sub-params the real backend would write
    @property
    def set_bus_fader(self):
        return HardwareBackend.set_bus_fader.__get__(self)

    @property
    def set_bus_pan(self):
        return HardwareBackend.set_bus_pan.__get__(self)

    def __init__(self, log, refuse_rates=(), refuse_clocks=()):
        self.log = log
        self.connected = True
        self.paused = False
        self.device_path = None
        self.last_errno = 0
        self.refuse_rates = set(refuse_rates)
        self.refuse_clocks = {CLOCK_SOURCE_MAP[n] for n in refuse_clocks}
        self.driver_values = {DRIVER_PARAM_SAMPLE_RATE: 48000,
                              DRIVER_PARAM_CLOCK_SOURCE: 0x0C}
        self.on_driver_change = None

    def _record(self, name, *args):
        self.log({"hw": name, "args": list(args)})

    def set_mixer_param(self, channel_type, channel_idx, param_id, value):
        self._record("set_mixer_param", channel_type, channel_idx, param_id,
                     value)
        return True

    def set_mixer_bus_param(self, bus_id, sub_param, value, side=0,
                            solo_mute=False):
        self._record("set_mixer_bus_param", bus_id, sub_param,
                     round(float(value), 6), side)
        return True

    def set_driver_param(self, param_id, value):
        refused = (param_id == DRIVER_PARAM_SAMPLE_RATE
                   and value in self.refuse_rates) or (
            param_id == DRIVER_PARAM_CLOCK_SOURCE
            and value in self.refuse_clocks)
        self._record("set_driver_param", param_id, value, not refused)
        if refused:
            return False
        self.driver_values[param_id] = value
        if self.on_driver_change:
            self.on_driver_change(param_id, value)
        return True

    def get_driver_param(self, param_id):
        return self.driver_values.get(param_id)

    # Reads the daemon polls: nothing to report
    def get_hw_readback(self):
        return None

    def get_dsp_loads(self):
        return None

    def get_clock_sources(self):
        return None

    def get_system_info(self):
        return None

    def get_device_info(self):
        return None

    def toslink_in_bit_enabled(self):
        return False

    def has_feature(self, bit):
        return False

    def probe(self):
        return True

    def __getattr__(self, name):
        if name.startswith("__"):
            raise AttributeError(name)

        def record(*args, **kwargs):
            self._record(name, *[a if isinstance(a, (int, float, str, bool))
                                 else repr(a) for a in args])
            return True
        return record


def publish_native_slots(state):
    """Native hosting's capability properties, as the daemon publishes them
    (_publish_native_capabilities), for the synthetic catalogue."""
    def prop(path, value, kind):
        state.add_runtime_property(path, {"type": kind, "value": value})
    for key, value, kind in (("NativePluginHosting", True, "bool"),
                             ("NativeMaxInstances", 4, "int"),
                             ("NativeMaxInserts", 4, "int"),
                             ("NativeChainSupported", True, "bool"),
                             ("NativeReplacementSupported", True, "bool")):
        prop("/" + key, value, kind)
    slots = [f"/devices/0/inputs/{n}/effects/0" for n in range(4)]
    layouts = {"UAD Test Comp": ([], slots[0::2]),   # (mono, stereo)
               "UAD Test EQ": (slots, []),
               "UAD Test Pre": (slots, slots[0::2])}  # both: the only one the browser lists
    for path in state.all_paths():
        if not re.fullmatch(r"/plugins/\d+/Name", path):
            continue
        base = path.rsplit("/", 1)[0]
        mono, stereo = layouts.get(state.get_value(path), ([], []))
        prop(base + "/NativeLoadSupported", bool(mono or stereo), "bool")
        prop(base + "/NativeLoadSlots", ",".join(dict.fromkeys(mono + stereo)), "string")
        prop(base + "/NativeMonoSlots", ",".join(mono), "string")
        prop(base + "/NativeStereoSlots", ",".join(stereo), "string")


class FixturePipeWire:
    """A PipeWire made of a pw-dump object list (pipewire_state's `run`):
    pw-dump serves it, `pw-metadata -n settings 0 KEY VALUE` sets KEY in
    its settings metadata (and calls `log`), pw-top has nothing."""

    def __init__(self, objs, log=None):
        self.objs, self.log = objs, log or (lambda entry: None)

    def run(self, cmd, env):
        if cmd[0] == "pw-dump":
            return json.dumps(self.objs)
        if cmd[:4] == pipewire_state.SETTINGS and len(cmd) == 6:
            self.log({"hw": "pw-metadata", "args": cmd[4:]})
            for o in self.objs:
                for m in o.get("metadata") or []:
                    if m.get("key") == cmd[4]:
                        m["value"] = int(cmd[5])
            return ""
        return None


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--port", type=int, default=0)
    parser.add_argument("--log", type=Path, required=True)
    parser.add_argument("--refuse-rate", type=int, action="append",
                        default=[])
    parser.add_argument("--refuse-clock", action="append", default=[])
    parser.add_argument("--external-locks", action="store_true")
    parser.add_argument("--stereo-sends", action="store_true")
    parser.add_argument("--catalogue", type=Path,
                        help="overlay this ua-catalogue folder (a synthetic "
                             "one for browser tests; see ua_catalogue.py)")
    parser.add_argument("--native-slots", action="store_true",
                        help="publish native slot lists for the synthetic "
                             "catalogue (stereo-only and mono-only bundles)")
    parser.add_argument("--pipewire-fixture", type=Path,
                        help="serve this pw-dump JSON as PipeWire")
    parser.add_argument("-v", "--verbose", action="store_true")
    args = parser.parse_args()
    if args.port in FORBIDDEN_PORTS:
        parser.error(f"port {args.port} belongs to a real daemon")
    logging.basicConfig(
        level=logging.DEBUG if args.verbose else logging.WARNING,
        stream=sys.stderr, format="%(levelname)s %(name)s: %(message)s")

    # Nothing outside this process: no PipeWire rate request, no ALSA card
    hardware.pipewire_force_rate = lambda rate, device=None: False
    ua_mixer_daemon.find_apollo_card = lambda device=None: None

    out = args.log.open("a", buffering=1)

    def log(entry):
        out.write(json.dumps(entry, separators=(",", ":")) + "\n")

    state = StateTree()
    state.load_device_map(DEVICE_MAP)
    apply_runtime_properties(state)
    catalogue = ua_catalogue.apply(state, None, args.catalogue) if args.catalogue else None
    if args.native_slots:
        publish_native_slots(state)
    backend = RecordingBackend(log, args.refuse_rate, args.refuse_clock)
    router = HardwareRouter(backend, state, stereo_sends=args.stereo_sends,
                            input_buses=descriptor_input_buses(MODEL),
                            model=descriptor_model(MODEL))
    router.set_model_data(load_descriptor(MODEL))
    router.supports_outputs_attenuate = \
        descriptor_mixer_class(MODEL) in OUTPUTS_ATTENUATE_CLASSES
    state.register_change_observer(
        "fake-hw-log", lambda path, value: log({"tree": path, "value": value}))

    scratch = tempfile.TemporaryDirectory(prefix="fake-hw-daemon-")
    daemon = MixerDaemon(state, router, "127.0.0.1", args.port, helper_port=0,
                         sessions_dir=Path(scratch.name) / "sessions",
                         presets_dir=Path(scratch.name) / "presets",
                         catalogue=catalogue)
    daemon.model_data = router.model_data
    # snapshots and the daemon's own presets live in the scratch dir too
    daemon.commands.set_store(Path(scratch.name), "device")
    daemon.console.catalogue_dir = args.catalogue or daemon.console.catalogue_dir
    daemon.mute_on_rate_change = 0.0
    pw = (FixturePipeWire(json.loads(args.pipewire_fixture.read_text()), log)
          if args.pipewire_fixture else None)
    daemon.pipewire = pipewire_state.PipeWireState(
        run=pw.run if pw else lambda cmd, env: None,
        locate=lambda card: ({}, "pipewire (4242)" if pw else ""))
    daemon._publish_pipewire(daemon.pipewire.poll())

    async def run():
        loop = asyncio.get_running_loop()

        def driver_changed(param, value):
            # What the driver's event channel reports after a clock write
            if param == DRIVER_PARAM_SAMPLE_RATE:
                loop.call_later(0.05, daemon._on_driver_event,
                                UA_EVT_RATE_CHANGED, RATE_INDEX.get(value, 0))
            elif param == DRIVER_PARAM_CLOCK_SOURCE:
                locked = args.external_locks or \
                    value == CLOCK_SOURCE_MAP["Internal"]
                loop.call_later(0.05, daemon._on_driver_event,
                                UA_EVT_CLOCK_LOCK, int(locked))

        backend.on_driver_change = driver_changed
        tasks = [asyncio.create_task(daemon.start()),
                 asyncio.create_task(daemon._meter_pump()),
                 asyncio.create_task(daemon._meter_pulse_heartbeat())]
        while daemon.server is None:
            await asyncio.sleep(0.01)
        # The hardware init (defaults to the device) before any client, not
        # 4 s later in the middle of a test; then a marker line
        daemon._fire_init_complete()
        log({"ready": True})
        port = daemon.server.sockets[0].getsockname()[1]
        print(f"LISTENING {port}", flush=True)
        await asyncio.gather(*tasks)

    try:
        asyncio.run(run())
    except KeyboardInterrupt:
        pass
    finally:
        out.close()
        scratch.cleanup()


if __name__ == "__main__":
    main()
