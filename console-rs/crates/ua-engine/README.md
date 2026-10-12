# ua-engine

The protocol and model layer for the UA Mixer Engine on TCP 4710. The engine speaks NUL-framed
JSON text, and the same protocol is served by UA's own engine and by the open-apollo daemon
(`mixer-engine/ua_mixer_daemon.py`). This crate has no UI. It is a port of ConsoleLab's Swift
`UAEngineKit` (`~/github/ConsoleLab/Packages/ConsoleLabKit/Sources/UAEngineKit`), including its
M7 plug-in parameter additions.

Protocol background lives in `~/wine-ua-work/console-port/protocol/`
(`PROTOCOL.md`, `control-tree.md`, `ui-to-path.md`, `gaps.md`).

## What the UI binds to

```rust
use ua_engine::{Endpoint, Options, Store, tapers};

let mut store = Store::new();                       // owned by the UI thread
store.connect(Endpoint::parse("192.168.1.4"), Options::default()); // read-only
store.set_waker(move || proxy.wake());              // optional: called from the network thread

// every frame (and whenever the waker fires):
if store.drain() { /* values, topology or status changed: redraw */ }
let strip = &store.topology().inputs[0];            // reassigned only when the layout changes
let db = store.value(&strip.fader_db()).and_then(|v| v.as_f64()).unwrap_or(-144.0);
let label = tapers::format_db(db);
let meter = store.meters().handle(&strip.meters[0].base); // keep per strip; lock-free reads
let r = meter.reading();                            // level_db, peak_db, clip

store.set_writes_enabled(true);                     // writer gate (default off)
store.begin_gesture();                              // set /UndoRecording/value true
store.set(&strip.fader(), 0.5);                     // local value moves now; wire coalesced ~60 Hz
store.end_gesture();                                // flush, then /UndoRecording/value false
store.commit(&strip.mute(), true);                  // a button: its own undo step
```

| Type | Role |
|---|---|
| `Store` | The model the UI binds to, owned by the UI thread. It provides `drain() -> bool`, `status()`, `topology()` and `topology_revision()`. Reads go through `value`/`property`/`meta` (paths are accepted with or without `/value`). Change tracking: `revision()`, `changed_since(rev) -> Changes::{All, Paths}`, `path_revision(path)`, `tree_revision()`, `node_revision()`. It also offers `meters()`, `connect`/`disconnect`, `load`/`load_snapshot`/`snapshot`/`save_snapshot`, `retain_subscriptions`/`release_subscriptions` (counted, kept across reconnects), `refresh(node)` (a read-only recursive `get`), `next_deadline()` (when `drain` next has timed work) and `simulate_meters(on)`. |
| Writer (on `Store`) | `set`, `commit`, `begin_gesture`/`end_gesture` (nesting is counted) and `flush`. `writer()`/`writer_mut()` expose `writes_enabled` (default `false`), `interval` (16 ms), `auto_flush` and `updates_companions`, plus the counters `sent_count`, `suppressed_count`, `recent_suppressed` and `gesture_depth`. While the gate is closed, local values still move and the writes are counted as suppressed. dB companions are updated locally: `FaderLevel`, send `Gain`, preamp `Gain` and `CRMonitorLevel`. |
| `Value`, `Property { value, meta: Meta { ty, min, max, values, default, readonly, enabled }, extra }` | Engine values (`Number`/`Bool`/`String`/`Null`/`Json`) with accessors (`as_f64`, `as_bool`, `as_str`, `as_i64`, `wire_text`) and property metadata (`choices()`, `is_integral()`, `normalized(v)`). |
| `Tree`, `Snapshot` | `Tree` is a path-indexed flattened tree. `Snapshot::load` reads a ConsoleLab snapshot (`{"format","info","tree","version"}`), a raw `get /?recursive=1` reply, a bare node, an open-apollo flat device map or a `dump_x8p_tree.py` capture. `Tree::trimmed(catalogs)` drops the bulky catalogue lists. |
| `Topology` | `inputs` (visible) and `all_inputs`, with stereo pairs folded into the left input and kinds classified. Also `auxes`, `cues`, `headphones`, `monitor` (found by `CRMonitorLevelTapered`, not by index), `talkback` (`/TalkbackOn` at the root), `cue_bus_count` and `loaded_slots(tree)`. Path builders per strip cover fader, pan/pan2, solo, mute, sends (`gain`, `gain_db`, `bypass`, `pan`, `post_fader_override` and meters), inserts, preamp (`gain`, `phantom` 48V, `pad`, `low_cut`, `phase`, `hi_z`, `phantom_enabled`/`pad_enabled`, the Unison slot) and meters (`level`, `peak`, `MeterClip`). |
| `PluginParamPaths`, `PluginParameter` | `InsertPaths::parameter(k)` builds `<slot>/parameters/<k>/{NormalizedValue, StringValue, StepValue, Name}`. `PluginParameter::list(tree, slot)` lists a loaded slot's parameters with their step labels. |
| `tapers` | `fader` (the UATaper LUT below t = 0.5528 and 55t − 43 above it; unity is 43/55 = 0.7818182), `preamp` (round(10 + 55t)), `monitor` (0.5 dB codes from the captured LUT), `pan` (L100…C…R100), `format_db`. |
| `MeterStore` | `Send + Sync`. Each meter slot holds atomics in a map under a `RwLock`, and the network thread writes to them. It provides `reading(key)`, `readings(keys)`, `handle(key)` (a lock-free `MeterHandle`), `pulse()`, `generation()` (bumped on every meter push, so the UI knows when to redraw), `stats()` and `reset_stats()`. Keys are meter nodes (`…/meters/K`) or `…/CustomIntegratedPreampInputMeter`. Meter paths never enter the tree or the change stream. |
| `Engine::connect(endpoint, options) -> EngineHandle` | A tokio client running a current-thread runtime on its own thread. It handles NUL framing; identity (a random 32-hex `networkID` and `client_id` `uaconsole`); `get` correlated by `func_id`, falling back to path matching for errors that do not echo it; tree loading with a stub walk; reconnect with backoff (0.5 s doubling to 10 s); and TCP keepalive. The handle offers `status()`, `updates()` (an `mpsc::Receiver<Update>`), `send(Command)`, `request(path, recursive) -> PendingReply`, `get()`, `counters()` and `set_waker()`. |
| `SubscriptionPlan` | Console's per-strip `subscribe {"paths":[…]}` batches, filtered to the paths the tree has (48 batches and about 4,650 paths on an x4). |
| `MockEngine` | A loopback engine with the verified 12.0.0 wire behaviour. `MockOptions { daemon_style: true }` switches it to the open-apollo daemon's behaviour instead. It can also simulate meters. |
| `discovery::browse` | Bonjour browsing for `_uamixer._tcp` through `mdns-sd`, behind the `mdns` feature. |

**Live lifecycle.** The connection runs in this order:

1. Connect.
2. Send the identity.
3. Send `get /?func_id=N&recursive=1`. The network thread parses the reply into a `Tree` and seeds the meters.
4. The `Store` installs the tree from `Update::Tree` on the next `drain`, sends `subscribe mode=multi`, then sends the per-strip batches.

After that, pushes are handled like this:

- **Splitting.** Pushes are flattened from `multi` envelopes, or taken as plain messages from the daemon. Meter paths are split off to `MeterStore` on the network thread. Everything else reaches the store as `Update::Values`.
- **Topology changes.** A change to `Stereo`, `Active`, `IOType`, `Name`, `StereoName`, `ChannelHidden`, `EnabledByUser`, `CueBusCount`, `DeviceName` or a send's `ID` recomputes the topology and syncs subscriptions. Unsubscribes go one path at a time, because the engine ignores batch unsubscribe.
- **Re-reads.** A change to `Stereo`, `IOType` or `EffectName` re-reads that node 100 ms later with a read-only recursive `get`. This refreshes metadata such as `48V/enabled`, and a slot's `parameters` after a plug-in swap.
- **Echo hold.** After a local write, engine pushes for that path are held for 350 ms, and then the latest one is applied.

## Safety

- There are two write gates, and nothing is sent if either one is closed:
  - `Options::allow_writes` is checked in `EngineHandle::send` and again on the network thread.
  - The writer's `writes_enabled` gate.

  Both default to off.
- `ua-engine-probe` has no write option. Its live commands send only identity, `get`, `subscribe` and `unsubscribe`.
- During this work, only those commands were sent to the local daemon on `127.0.0.1:4710`, which drives the real Apollo, and to the real engine on `192.168.1.4:4710`. No `set` or `post` was sent to either. `set /Sleep false`, Console's wake-up call, is a `set`, so it is never sent. Both engines serve read-only clients without it.
- Writes were tested against `MockEngine` and against a separate software-only daemon:
  - It was launched as `--no-hardware --no-helper --no-ws --no-bonjour --host 127.0.0.1 --port 14710`.
  - It ran from a scratch copy of `mixer-engine`.
  - Before anything was written to it, it was confirmed to be software-only: its log reported `Hardware: software-only` and `Metering: disabled`, and its only open file was `/dev/null`.
  - `tests/daemon.rs` runs this round trip when `UA_ENGINE_DAEMON_PORT` is set, and refuses to run on port 4710.

## Live findings (2026-10-11, read-only)

| | Local daemon `127.0.0.1:4710` | Real engine `192.168.1.4:4710` |
|---|---|---|
| Server | open-apollo daemon (driver-v2 build), hardware attached | UA Mixer Engine on the Mac Studio |
| Device / `/Version` | Apollo x4 / 11.8.2 (the device map's capture), `DeviceOnline` true | Apollo x4 / 12.0.0, `DeviceOnline` false |
| `get /?recursive=1` | 1.36 MB in 0.04 s: 1,569 nodes, 9,950 properties, no stubs | 1.81 MB in 0.70 s over the LAN: 2,021 nodes, 13,308 properties, no stubs |
| Strips | 24 inputs fold to 21 strips (pairs 2/3 and 12/13), **8 visible**: ANALOG 1, ANALOG 2, ANALOG 3/4, S/PDIF, VIRTUAL 1-4 | 24 inputs fold to 19 strips (pairs 2/3, 12/13, 14/15, 16/17), **6 visible** (user-named) |
| Outputs | 2 auxes; cues = outputs 14-17 (`CueBusCount` 2); HP = 19-20 (hardware level); monitor = 18 at −52 dB; talkback mic = input 22 (inactive); `/TalkbackOn` at the root | identical |
| Subscription plan | 48 batches, 4,637 paths (615 meter paths) | 48 batches, 4,678 paths (657 meter paths) |
| `/MeterPulse` | **9.9 Hz**: interval p50 100.8 ms, min 100.1, p99 103.7, max 104.8 | **31.4 Hz**: interval p50 30.7 ms, p99 32.6 (LAN arrival jitter: min 0, when two ticks share a read; max 230) |
| Push framing | **plain messages only**: 284 frames in 10 s, 0 `multi` envelopes | **`multi` envelopes only**: 314 frames in 10 s, every one a `multi` envelope |
| Meter values | delta-gated (0.5 dB): 185 pushes in 10 s from one live meter (ANALOG 1 at about −52 dBFS) | 0 pushes in 10 s (unit offline, so every meter sits at −77) |
| Traffic while idle | 1.9 kB/s | 2.4 kB/s |
| Metadata | `readonly` on 8 properties, no `default`, no `enabled`, no root `commands` | `readonly` 3,161, `default` 862, `enabled` 56, 74 root `commands` |
| `Stereo/enabled`, `Name/default`, `48V/enabled` | error `Unable to resolve path for get.` | `true`, `"ANALOG 1"`, `false` |
| Hidden `inputs/1/Gain/value` | error | `10.0` |
| Errors | **no `parameters`, so `func_id` is not echoed** | `func_id` echoed |

**Does the daemon's tree match the real engine's shape?**

For the mixer, yes. Not one property shared by the two trees differs in `type`. The paths that the topology and the subscription plan use exist in both, with the same output indices (x4 to x4). `Topology` gives identical outputs for both, and the strip differences come only from the user's links, names and visibility.

The differences are the following, and the Rust Console has to handle each of them:

1. **No `multi` envelopes on the daemon.** It ignores `subscribe mode=multi`, and it answers a batch subscribe by flooding plain value messages. The connection flattens envelopes when they are present and takes plain pushes as they are, so both styles work. The `daemon_style` mock covers this case.
2. **Meter cadence.** `/MeterPulse` runs at 10 Hz on the daemon and at about 32 Hz on the engine. The daemon also gates meter values at 0.5 dB. The UI has to redraw meters on its own 30 Hz clock (watch `MeterStore::generation()`), not once per pulse.
3. **No `default` or `enabled` on the daemon.**
   - `InputStrip::default_name` is `None` there, so fall back to `Name`.
   - `phantom_enabled`, `pad_enabled` and `stereo_linkable` default to `true`.
   - `io_type_selectable` falls back to the number of choices.
   - On the daemon, metadata re-reads after an `IOType` change cannot change 48V/pad availability.
4. **The daemon's errors carry no `func_id`.** An awaited `get` used to hang there until its timeout. The connection now matches an error that has no `func_id` to the oldest pending request for the same path, so these errors come back at once. This was verified against the daemon.
5. **`func_id` is echoed as a string by the daemon** and as a number by the engine. Both are accepted.
6. **Paths only on the real engine:**
   - Plug-in parameters under loaded slots: 436 parameters with `Name`, `NormalizedValue`, `StringValue` and `Type`, 256 of them with `StepValue`.
   - The ADAT-preamp properties `AdatDeviceUid`, `AdatPreampDeviceOnline`, `SupportsVolt876ADATPreamps` and `inputs/N/AdatPreampState`.
   - `inputs/N/SelectedOnFront`.
   - Three more catalogue plug-ins.
   - The hidden `inputs/N/Gain`.

   Plug-in windows therefore have nothing to bind to on the daemon.
7. **Paths only on the daemon:** `/Sleep`, `/BufferSize*`, `/USBStreamingMode*`, `/Dante*`, `/CorrectionLicenseState`, `/errors`, `/initialized*`, `/status/timeout_ms`, `/dante/*` and `/openapollo/*` (latency and PCIe link). The subscription plan picks up the root ones automatically, and they are harmless.
8. **Out-of-range daemon state.** The daemon's ANALOG 2 preamp reports `Gain` 0 dB, which is outside the 10–65 dB Mic range. Do not assume `Gain` is in range.

The ConsoleLab findings still hold on 12.0.0. Pushes come in `multi` envelopes, batch unsubscribe is ignored, and `Active` is not visibility for outputs. A pair shows `StereoName` and a mono strip shows `Name`. Sends mute with `Bypass`, meters clip with `MeterClip`, and talkback is `/TalkbackOn` at the root. See ConsoleLab's `UAEngineKit/README.md`.

## Fixtures

| File | Source |
|---|---|
| `tests/fixtures/apollo-x4-12.0.0.snapshot.json` (432 KB) | A live, read-only capture of UA Mixer Engine 12.0.0 with an Apollo x4, taken with ConsoleLab `lab-engine dump` on 2026-10-12. **It has been sanitized:** the user's channel names are replaced (Vocal Mic, Talk HP, Guitar, Keys, DAW 1/2, DAW 3/4), and the MIDI device list, the modified-session list and `DeviceHwID` are cleared. It is trimmed like `Tree::trimmed(true)`: preset lists, `Snapshot` volumes, the plug-in catalogue, scenes, routes and ASIO channels are removed, and emptied catalogues are written as `{"children":{}}` so that they do not read back as stubs. It keeps the plug-in parameters of the loaded slots, the `readonly`/`default`/`enabled` metadata and the root `commands`. With the `bundled-snapshot` feature it is also available as `Snapshot::bundled_x4()`. |
| `../../../mixer-engine/device_maps/device_map_apollo_{x4,x8p}.json` | open-apollo's committed flat device maps (engine 11.8.2 x4 and 11.9.0 x8p), read in place. |

## ua-engine-probe

```
cargo run -p ua-engine --release --bin ua-engine-probe -- [--host H] [--port P] <command>
  dump --out f.json [--trim] [--catalogs]   read-only recursive snapshot
  topology [--snapshot f.json] [--all] [--paths]
  meters --seconds N                        pulse cadence, meter pushes, frames, readings
  subs [--snapshot f.json]                  subscription plan sizes
  get PATH [--recursive]
  compare a.json b.json                     tree shape differences + both topologies
  convert --in map.json --out snap.json [--trim] [--catalogs]
  mock-serve --snapshot f.json [--port N] [--meters]
  browse --seconds N                        (needs --features mdns)
```

## Tests

`cargo test -p ua-engine` runs 66 tests. They cover:

- framing and command text;
- decoding, trees and snapshot formats;
- tapers, against the documented numbers;
- topology on the live x4 capture and the x4 and x8p device maps, with every handed-out path checked against the tree;
- the subscription plan and plug-in parameters;
- the store (change tracking, topology revisions, meters, snapshots);
- the writer (the gate, coalescing, gestures, `commit`, integral formatting, companions, auto-flush, the echo hold);
- end to end against `MockEngine` in both its engine and daemon styles: going live read-only, the write gates, echoes, reconnect, `func_id` correlation, meters, retained subscriptions and the metadata refresh.

`UA_ENGINE_DAEMON_PORT=14710 cargo test -p ua-engine --test daemon` adds the software-daemon round trip.
