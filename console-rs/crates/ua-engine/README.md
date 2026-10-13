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
store.connect(Endpoint::parse("192.0.2.4"), Options::default()); // read-only
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
| `HelperStore`, `HelperInfo`, `NativeInsert`, `MockHelper` | The read-only UA Mixer Helper client on 4720 (see [UA Mixer Helper](#ua-mixer-helper-4720)). `Options::helper()` gives `network_id` identity and a 5 s `get /ping`. |

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
- The helper client (`HelperStore`, `Options::helper()`) forces `allow_writes` off. It sends identity, `get`, `subscribe` and `get /ping`, never `post` (not even `command_format`) and no challenge or response.
- `ua-engine-probe` has no write option. Its live commands send only identity, `get`, `subscribe` and `unsubscribe`.
- During this work, only those commands were sent to the local daemon on `127.0.0.1:4710`, which drives the real Apollo, and to the real engine on `192.0.2.4:4710`. No `set` or `post` was sent to either. `set /Sleep false`, Console's wake-up call, is a `set`, so it is never sent. Both engines serve read-only clients without it.
- Writes were tested against `MockEngine` and against a separate software-only daemon:
  - It was launched as `--no-hardware --no-helper --no-ws --no-bonjour --host 127.0.0.1 --port 14710`.
  - It ran from a scratch copy of `mixer-engine`.
  - Before anything was written to it, it was confirmed to be software-only: its log reported `Hardware: software-only` and `Metering: disabled`, and its only open file was `/dev/null`.
  - `tests/daemon.rs` runs this round trip when `UA_ENGINE_DAEMON_PORT` is set, and refuses to run on port 4710.

## Live findings (2026-10-11, read-only)

| | Local daemon `127.0.0.1:4710` | Real engine `192.0.2.4:4710` |
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

## Sessions

`session` covers Console's session handling on 4710: the root properties `/Session`
(`[folder/]name`; `values` = the Sessions folder tree), `/ModifiedSessions`, `/Dirty` and
`/Progress`, and the commands that change them (`SessionCommand`, `SnapshotCommand`).

- **Loading** sets `/Session` (`set /Session/value "<[folder/]name>"`). **Restoring** a kept
  session sets `/ModifiedSessions`.
- **Everything else is a post:** `NewSession {}`, `OpenSession {"path"}`, `SaveSession {}`,
  `SaveSessionAs {"name":"….uadmix"}`, `DeleteSession`, `RenameSession`,
  `NewFolder`/`DeleteFolder`, `SaveLastUsedSession {}`.
- **Writes are gated.** `Store::session` sends through the writer gate (suppressed while writes
  are off).
- **List refresh.** The store re-reads `/Session/values` and `/ModifiedSessions/values` when the
  session identity changes.
- **Sources.** Shapes come from engine 12.0.0's `get /?commands=1` and Console 1.2.9 Rack.dll.
  The catalog with addresses is `console-port/surfaces/sessions.md`.
- **Session files** are `~/Documents/Universal Audio/Sessions/*.uadmix` on the engine host: XML
  `uad_mixer_session` / `mixer_tree` / `mixer_object` / `property id="kProp…"`.

## UA Mixer Helper (4720)

The helper (`mxhlpr`, Bonjour `_uamixerhelper._tcp`) was mapped read-only on 2026-10-12, on the Mac's helper 1.3.1.1152 (`192.0.2.4:4720`, Console 1.3.1) and on the local open-apollo daemon's helper (`127.0.0.1:4720`). Only identity, `get`, `subscribe` and `get /ping` were sent. No `set`, no `post` (not even `command_format`), and no challenge or response.

**Wire behaviour (Mac).**

- **Framing.** The helper answers text clients in plain NUL-terminated JSON text. UBJSON (`post command_format 2`) is not needed.
- **No challenge.** A client that sends only identity (`network_id`, `client_id`), `get` and `subscribe` is served without any challenge. The connection stayed up for 40 s with 5 s pings and for 45 s without them. `/status/timeout_ms` reads 25000.
- **Tree format.** The format differs from the engine's:
  - node `children` are arrays of `{"path": name, …}`, and a non-recursive `get` returns `{"path"}` stubs;
  - properties are `{"value", "read_only", "values"?}`, with snake_case names;
  - nodes carry `commands` lists, including a documented root catalogue (`clear_clips`, `create_emulated_device`, auto-gain, …).

  `helper::tree_from_helper` loads it into a `Tree`.
- **Replies.** `get` echoes `func_id`/`recursive` in `parameters`, and errors are `uasrv.invalid_object`, also with `func_id`.
  - `subscribe <property>` is answered with its current value.
  - A recursive node `subscribe` is answered with `null`.
  - No value change was pushed during 40 s of watching, so the push format is not yet verified. `MockHelper` assumes `{"path":"<p>/value","data":…}`.
- **Local daemon.** It replays a 1.2.8.797 capture (`device_maps/helper_tree.json`): 803 nodes, no `/drivers`, no sample rate. Its errors are engine-style and carry no `func_id`; the connection matches them by path.

**What lives there.** A `get /?recursive=1` returns 0.83 MB in 0.17 s: 1,064 nodes and 8,175 properties.

| Node | Content |
|---|---|
| `/` | `sample_rate` (+ `values`), `io_buffer_size` 128, `render_buffer_size` 512, `latency` 1024, `num_render_threads`, `driver_status` (`initializing`, `initialized_percent` crept from 29.66 to 72.1 over hours), `active_audio_engine` (`enabled: false`), `peak_hold`, `clip_hold`, `controls_mode`, `post_fader_meters`, `delay_comp_enabled`, `plugin_sdk_version` 5.4.8 |
| `status` | `name` mxhlpr, `version`, `timeout_ms`, feature keys |
| `drivers` | every CoreAudio device: the Apollo (`apollo_thunderbolt`, 22 in / 24 out, with Console's I/O names `MIC/LINE/HIZ 1` … `TALKBACK 2`), plus all of the Mac's other audio devices, and their sample rates |
| `devices/{uid}` | the native "supercore" engine's device: `buses` (CUE 1-4 = `cue_0..3`, MAIN = `master`, each with `controls`, `effects` slots `bus_input`, `console`, `reserved_0`, `effect_0..7`, `sidechain_filter` and `bus_compressor`, `outputs/main`, `meters`), plus `tracks` (empty), `inputs`/`outputs` (empty), `meters` (`render`, `renderio`, `mem`), `midi`, `banks`, `undo_queues` |
| `auto_gain` | `status` (`stopped`), `status_str`, `channels` |
| `dante`, `preferences`, `permissions`, `network`, `workspace`, `presets`, `logs`, `analysis`, `recording`, `plugins` (empty), `errors`, `stats` | helper and app state, not mixer state |

**Native inserts.** A native (UADx) plug-in would sit in a supercore effect slot, `devices/{uid}/{tracks|buses}/{uid}/effects/effect_N`. Each slot carries `plugin_name`, `plugin_id`, `plugin_power`, `enabled`, `insert_type`, `track_uid` and `bus_uid`. On the Mac every slot is empty, `tracks` has no children, and the native engine is off (`active_audio_engine.enabled: false`). `NativeInsert::list` reads these slots, and `HelperStore` subscribes their values and re-reads `tracks` every 10 s. engine-model merges them into empty insert slots, with a native flag, on the input strip whose name matches the track's name. That track-to-input mapping is an assumption: no populated track has been seen live, and the tests use a synthetic track copied from a captured bus.

**The "CL 1B mkII" in `refpairs/p1` was a UAD-2 insert on 4710, not a native one.** The UA Mixer Engine log on the Mac (`~/Library/Logs/Universal Audio/UA Mixer Engine_5.log`) shows the whole event:

1. At 22:26:33.306 local: `CMxPluginScenes::AddEffect(): uuid = c6ce2e71-858a-47fe-ad02-8813933d97e4 name = UAD Tube-Tech CL 1B mk II`. That uuid is `/devices/0/inputs/0/effects/0/ScenePluginUuid`, SM7B's insert 1.
2. At 22:26:36.217: `RemoveEffect`.
3. At 22:26:37.19: the plug-in is deleted.

The pair's `engine.json` was dumped at 02:26:24Z, nine seconds before the load, and the screenshot was taken at 02:26:37Z, as the plug-in was being removed. The same log shows dozens of `AddEffect`/`RemoveEffect` events on that slot a few seconds apart (another session was cycling plug-ins), and the slot's `ChangeCount` went from 145 to 165. Every slot's `ScenePluginUuid` is stable, which makes it the key for matching engine logs to slots.

**Slot labels in Console 1.3.1.** These come from reference shots:

- With the Apollo online, a UAD-2 plug-in is labelled upright with the catalogue's first short name, `/plugins/P/Name/values[0]` (for example "CL 1B mkII" for `UAD Tube-Tech CL 1B mk II`).
- With the Apollo offline (`DeviceOnline` false), the label is the engine name without "UAD ", in italics (for example "Millennia NSEQ-2", "SPL Vitalizer MK2-T").

`engine-model`'s `SlotInfo { label, italic, source }` follows these rules.

**Status bar.** Tempo, rate, clock, DSP/PGM/MEM and scene all come from 4710 (`/Tempo`, `/SampleRate`, `/ClockSource`, `/Total{DSP,PGM,MEM}Load`, `/pluginscenes/0/RecalledSceneIndex`). The helper's `meters/{render,renderio,mem}` belong to the native engine. engine-model only takes the helper's sample rate when the engine has none.

**Open questions.**

- The format of a pushed value change on 4720.
- How a supercore track maps to an Apollo input once the native engine is active.
- Whether the Windows helper enforces the challenge (it may: `gaps.md` §A).
- What `io_buffer_size` and `latency` mean for Console's settings sheet.

## Fixtures

| File | Source |
|---|---|
| `tests/fixtures/apollo-x4-12.0.0.snapshot.json` (432 KB) | A live, read-only capture of UA Mixer Engine 12.0.0 with an Apollo x4, taken with ConsoleLab `lab-engine dump` on 2026-10-12. **It has been sanitized:** the user's channel names are replaced (Vocal Mic, Talk HP, Guitar, Keys, DAW 1/2, DAW 3/4), and the MIDI device list, the modified-session list and `DeviceHwID` are cleared. It is trimmed like `Tree::trimmed(true)`: preset lists, `Snapshot` volumes, the plug-in catalogue, scenes, routes and ASIO channels are removed, and emptied catalogues are written as `{"children":{}}` so that they do not read back as stubs. It keeps the plug-in parameters of the loaded slots, the `readonly`/`default`/`enabled` metadata and the root `commands`. With the `bundled-snapshot` feature it is also available as `Snapshot::bundled_x4()`. |
| `../../../mixer-engine/device_maps/device_map_apollo_{x4,x8p}.json` | open-apollo's committed flat device maps (engine 11.8.2 x4 and 11.9.0 x8p), read in place. |
| `tests/fixtures/helper-1.3.1.json` (418 KB) | The Mac helper's `get /?recursive=1` reply, in the helper's own format (2026-10-12), sanitized. `drivers` keeps only the Apollo. `status/feature_keys` is cleared. The supercore `midi` and `banks` are dropped. `logs`, `dante`, `presets`, `workspace`, `network`, `permissions`, `preferences` and `analysis` are kept as empty folders. |

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
  helper [--seconds N] [--raw f.json] [--out f.json]   UA Mixer Helper on host:4720 (read-only)
```

## Tests

`cargo test -p ua-engine` runs 70 tests. They cover:

- framing and command text;
- decoding, trees and snapshot formats;
- tapers, against the documented numbers;
- topology on the live x4 capture and the x4 and x8p device maps, with every handed-out path checked against the tree;
- the subscription plan and plug-in parameters;
- the store (change tracking, topology revisions, meters, snapshots);
- the writer (the gate, coalescing, gestures, `commit`, integral formatting, companions, auto-flush, the echo hold);
- end to end against `MockEngine` in both its engine and daemon styles: going live read-only, the write gates, echoes, reconnect, `func_id` correlation, meters, retained subscriptions and the metadata refresh;
- the helper: format conversion, `HelperInfo` on the Mac capture, native-slot extraction, and `HelperStore` against `MockHelper` (read-only verbs only, pings, pushes, polled tracks).

`UA_ENGINE_DAEMON_PORT=14710 cargo test -p ua-engine --test daemon` adds the software-daemon round trip.
