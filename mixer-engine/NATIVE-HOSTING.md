# Experimental native plug-in hosting

Experimental and off by default (driver `plugin_host=1`, daemon `--native-plugin-provider`). It
connects the daemon's insert transactions to the kernel INSTANCE API. The bundles a provider
loads are built on your own machine from your own UAD install; open-apollo ships none. Select a trusted local provider explicitly and preserve the
running hardware configuration:

```sh
python3 mixer-engine/ua_mixer_daemon.py \
  --native-plugin-provider /absolute/path/to/provider.py \
  --preserve-hardware --require-hardware
```

The product ships no vendor recipes, coefficient pages, plug-in binaries or art.
A provider exposes `create_provider()` or `create_providers()`; the latter may
return a tuple subclass with an explicit `composer` callable. Device licensing
remains authoritative. Catalog visibility and a configured loader do not prove
successful device admission or hardware qualification.

## Hosting contract

Providers declare exact names, inputs, channel layouts and sample rates. Driver
feature bits select supported operations; an older driver never silently gains
stereo, replacement, multiple chains or compensation support.

- Independent analog input chains support mono and genuine stereo layouts.
- With CHAIN and SWAP support, a private composer can host up to four logical
  inserts per input chain. Each retains a stable public identity and its own
  controls when neighboring inserts are added, replaced or removed.
- Candidates are initialized and read back before routing changes. Replacement
  stages a candidate, swaps routing, publishes the accepted state, then retires
  the old allocation. Confirmed failures can reverse the swap; uncertain I/O
  quarantines the native session instead of claiming restoration.
- Every effect in a chain is authorized by a fresh device license query before
  allocation. Immutable chain directories cannot be rewritten through SERVICE.
- Parameter plans contain writes, exact expected observations, display values
  and proposed provider state. State is accepted only after hardware readback.
- Meter requests are explicit allowlists: scalar IDs or exact read-only opcode
  519 tuples. Arbitrary DSP locations and vendor writes are not exposed.
- Unload and owning-fd close restore retained plain input strips. Allocation
  ownership and scratch requirements include surviving and staged instances.
- Device/rate changes require a new native session. Saved slot properties never
  recreate DSP leases automatically.
- Bulk scenes, presets, undo, clock changes and link changes remain guarded.
  Slot Power needs its own provider contract; an effect's Power parameter does
  not implicitly implement slot Power.

## Input delay compensation

The recovered settings are Off, Short, Medium, Medium-Long and Long, with budgets
0, 100, 200, 300 and 1000 samples. This is an adjustable input setting, not a fixed
plug-in latency. Authoritative effect latency is summed once per chain, including
stereo chains. Original group policy determines record and monitor delays.

With DELAY_COMP support, a setting change stages affected native chains, updates
and verifies all retained plain strips, swaps the chains and preserves logical
controls. Insertion/removal also recalculates affected group members. The strip
API pins the current hardware epoch and construction generation. Uncertain
outcomes quarantine the session.

A chain exceeding the selected nonzero budget publishes
`InputDelayCompOverloaded` and `InputDelayCompWarning`. Rust Console shows the
warning and a shortcut to Hardware settings. Unsupported drivers keep the
adjustment disabled. Group editing remains guarded. Retained-strip record delay is physically measured at all five budgets. Pultec
chains verify nonzero-latency record alignment and overload behavior. Monitor delay
readback passes; physical monitor-output timing remains a separate gate.

## Capability metadata

Native mode publishes `NativePluginHosting`, `NativeMaxInstances`,
`NativeReplacementSupported`, `NativeMaxInserts`, `NativeChainSupported` and
`NativeDelayCompSupported`. Each `/plugins/N` entry adds:

- `NativeLoadSupported`: an explicitly configured provider exists.
- `NativeLoadSlots`, `NativeMonoSlots`, `NativeStereoSlots`: exact available paths.
- `NativeSampleRate`: configured rate, or zero when unavailable.
- `NativeLoadHint`: a readable support limitation.

These runtime properties never change authorization or demo state. Rust Console
checks source and destination capabilities, rate, layout and occupied slots before
assignment; the backend remains authoritative if state changes concurrently.
Engines without these extensions retain their existing behavior.

## Measured development coverage

Apollo x4 at 48 kHz:

- 1176LN Legacy, 1176SE Legacy and LA-2A Legacy pass mono load/control/audio/meter
  checks, plus genuine stereo on inputs 3/4 with distinct left/right tones.
- Simultaneous stereo chains on inputs 1/2 and 3/4 pass, including removing the
  older allocation while the surviving chain continues processing correctly.
- Cross-type and same-type stereo replacement pass, including control response
  and final dry-signal restoration.
- A four-insert stereo chain of 1176LN, 1176SE, LA-2A and LA-3A passes insertion,
  control retention, middle/first removal, LA-3A gain and dry restoration.
- LN/SE/LA-2A live raw meter samples pass original display-function comparisons
  and Rust headless image checks. LA-3A's corrected original meter curve passes
  offline numeric/image checks; its live meter source remains under diagnosis.
- Pultec Legacy passes mono load, all ten controls and restoration, LF/HF EQ and
  bypass response on input 3. One/four-instance captures verify approximately
  31/124 samples with compensation Off; Short leaves about 24 samples for four
  instances, and Medium aligns the record paths. The real daemon warning suggests
  Medium and clears after adjustment. Pultec stereo remains unavailable.
- Resource selection includes every required architecture-0/2 entry. Kind-2 data
  uses pool 2 and the original DATA upload/release contract; omitting those data
  dependencies caused the first Pultec recipe to be incomplete.

Pultec-Pro Legacy also passes mono input-3 hosting at 48 kHz: all 17 controls, measured EQ and both plugin power responses, four-insert control retention, overload warnings and physical record-delay compensation. Original-backed pure runtime math and pinned finite scalar data require no DLL/Wine runtime. Its private panel renders with all 17 parameter ordinals. Pro stereo and other inputs/rates remain unavailable.

These results do not qualify all catalog plugins, other sample rates, absolute
analog voltage calibration, transient meter ballistics, automatic reconnect or
release-ready recipe generation. Private run artifacts retain failures separately
from passing acceptance evidence. Tests contain synthetic I/O and no vendor pages.
