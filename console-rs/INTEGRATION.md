# Linux console integration checkpoint — 2026-10-12

Completed the requested HANDOFF-CODEX.md integration on `feat/rust-console`.
Starting HEAD was `aef5d71`; existing changes were preserved in a git stash
and an independent own-change patch under `/tmp` before integrating history.

## History and ownership

- Merged finished `rc/wiring` at `5b3803d` with a merge commit. Includes live
  subscriptions, connect hello, per-connection Sleep hints, MIDI polling and
  Console 1.3.1 command wiring.
- Merged finished `rc/ledger` at `138a8ed` with a merge commit.
- Reapplied Codex fixes separately: clear buttons/settings writer gating,
  corrected ledger reset simulation, zero missing-in-tree subscription
  allowance, and sandbox endpoint guards. The finished wiring branch already
  corrected the obsolete hard-coded preference expectation.
- Committed plugin UI compatibility/discovery/clipping separately. The
  integrated 223-spec stage is discovered without copying proprietary assets.
- `rc/popups` and `rc/panels-align` remain for Claude. Neither was merged.
  No other worktree was modified; no driver code was imported or installed.
- No push or git-config changes. Commits use the identity specified in the
  handoff through per-command `git -c` arguments.

## Daemon compatibility

The daemon now parses JSON command arguments and supports these host-side
Console paths:

- `"default"` resets from explicit property metadata or known Pan/fader
  defaults. Unknown defaults fail rather than becoming string values.
- Query parameters are parsed; success/error replies echo request IDs.
- Gesture UndoRecording groups and integer UndoRedo current−1/current+1,
  including the values list and disconnected-client group completion.
- Scene Store/Recall/Overwrite/Clear, upper-case names and recall-index
  pushes. Saved scene snapshots survive daemon-owned session round trips.
- Scoped/global clip clear and global solo clear.
- New/Open/Save/SaveAs/Delete/Rename sessions, session folders, dirty-session
  retention/restore, progress, directory lists and last-used restart restore.
- Per-client Sleep hints suppress that client's periodic meter pushes; they
  do not modify global state. MixerState and MIDI device-list reads exist.

Session files use native-shaped `.uadmix` XML, with a bounded property mapping
for common scalar fields, flattened AUX/HP sends and preamp controls. Unknown
XML and opaque plugin chunks are retained when saving a loaded document.
An `open_apollo` extension holds paths/scenes not represented by the native
subset. **Full UA interoperability and plugin-chunk execution are not
verified.** Scene/session recall restores host metadata, not a DSP instance.
The tests use constructed XML; no UA capture or asset was committed.

Use `--sessions-dir DIR` to select a session directory. The default is
`~/Documents/Universal Audio/Sessions`. Empty names/paths and escaping folder
names are refused; unsupported commands return errors rather than opening
host dialogs. The directory list currently reflects disk changes at command
boundaries; MIDI values come from the available state metadata, without a
new OS MIDI discovery implementation.

## Verification

- `cargo build --release --workspace --locked`: passed.
- `cargo test --workspace --locked`: 235 passed, five ignored.
- `python3 -m unittest test_console_controls test_console_sessions`: 16
  passed, including ephemeral TCP tests and synthetic XML/file tests.
- Required p1 placement check: **158/158 aligned**.
- Required p2 placement check: **195/195 aligned**.
- Rust daemon round trip and ignored settings tests ran sequentially against
  the private daemon below; see final validation results in commit history.

Private daemon was a scratch copy under `/tmp/apollo-handoff-daemon`, with
hardware, ALSA metering, helper, WebSocket and Bonjour disabled:

```sh
python3 /tmp/apollo-handoff-daemon/ua_mixer_daemon.py --no-hardware \
  --no-helper --no-ws --no-bonjour --host 127.0.0.1 --port 34711 \
  --sessions-dir /tmp/apollo-handoff-daemon/session-fixtures
UA_ENGINE_DAEMON_PORT=34711 cargo test -p ua-engine --test daemon --locked
UA_SANDBOX_ENGINE=127.0.0.1:34711 cargo test -p engine-model \
  --test settings_sandbox --locked -- --ignored --test-threads=1
```

Only the private daemon was started/stopped. The real local daemon and the
Mac engines were not written to, stopped, restarted or driven through UI.
Reference renders and comparison outputs remain under `/tmp`; no UA artwork,
binaries, lunacomponent code or screenshots were added to git.

## Remaining work

- Claude owns popup/alignment merges and subsequent ConsoleLink rebuild.
- The plugin runtime still lacks some newer Swift interaction features;
  see PLUGIN-STATUS.md and the reproducible runtime-gap inventory tool.
- DSP upload/create/completion and plugin meter pushes require the native
  backend work; UI metadata and mock success do not demonstrate processing.
- Vendor `.uadmix` round trips require independent real-file acceptance;
  unsupported properties/chunks are preserved, not inferred or executed.
- Insert assignments retain an extra undo bracket versus captured traffic;
  rapid gestures can coalesce the delayed undo-list refresh.

## Continuation P0 — Console 1.3.1 host contracts

Task branch `codex/p0-console-daemon`, based on `50ace46`, is merged with
history. Added software slot-tree CopyInsert/PasteInsert/MoveInsert,
PluginBar settings clipboard and capability pushes, with atomic target/Unison
validation and structural undo. Plugin mutation on a hardware-backed daemon
fails closed until a real DSP backend exists. `/openapollo/PluginBackend`
explicitly distinguishes the software model; test clients require that marker.

Preset commands publish folder/file trees and support save/load/rename/delete
and folders for daemon-owned `.oapreset` snapshots. They are not disguised as
native UA files. The scalar-only native `channel_strip` XML subset is readable;
files carrying opaque plugin state are rejected before any scalar mutation.
Factory libraries are optional `--factory-presets-dir` inputs and read-only.
Static review found all 9,575 installed FXPs use opaque FPCh chunks: native
factory FXP restoration remains blocked on the plugin backend, not "verified"
by a successful filename set. Evidence: presets-decomp/1000dee38.c and
100056370.c, retained host-contract notes and source-derived session mapping.

Root TalkbackOn/InPhysicalCR/Master are published; verified SetOtherParam
mappings are tested only with mock hardware. Monitor CUE3/4 choices are
available in the software model; unverified hardware encodings are refused.
ChannelHidden persists. Session-folder rename updates the current/retained
paths. Helper adapter lists enumerate Linux interfaces; primary selection is
validated/persisted, while the listening address remains explicit `--host`.
PluginStartDemo updates a simulated footer only under explicit --no-hardware;
it cannot activate a vendor license/demo or claim native authorization.

Validation: 25 Python host tests; three sequential UA_SANDBOX_ENGINE Rust
round trips on private ports 34713/34723; 269 workspace tests; locked release
build; p1 158/158 and p2 195/195; windows_check and live-snapshot ledger tests;
scorecard regenerated under wine-ua-work. Round trips compare a second client,
retain parameter subscriptions explicitly, and avoid shared-state timing
assumptions. No real-engine writes, vendor UI input, push or UA assets in git.

P0 still needs genuine DSP-backed instance/preset/demo operations before
hardware acceptance. Those operations now return errors instead of false
success. The old daemon gaps are covered at the host/software contract layer.

## P1 — multiple-unit identity and offline projection

Task branch `codex/p1-multiple-apollos` adds ordered per-device topologies for
actual `/devices` indices (including sparse ids), subscribes each unit while
deduplicating shared root/catalogue/route subscriptions, and invalidates
layouts on DeviceOnline and MultiUnitOrder changes. Input strips are grouped
contiguously in cascade order. Show Offline Devices now changes the drawn
input projection and session navigator/layout counts; complete model lookups
keep hidden/offline strips addressable.

Send windows retain the owning device across content, anchor, navigation and
layer ids; CLI accepts `input-sends:DEVICE:INPUT` and `aux-sends:DEVICE:AUX`.
Send availability/pre-fader defaults and insert-label online state use the
owning unit. Native helper name joins are accepted only when unambiguous,
avoiding duplicate channel names crossing device boundaries.

The shared control-room/aux model still uses the first cascade unit; the exact
shared-bus ownership and native device-tab presentation need source/runtime
confirmation. No guessed multi-device hardware writes or new unverified tab
art were introduced. Full paths and input grouping are verified, not a
hardware cascade acceptance claim.

Validation: four synthetic/mocked sparse-device tests (2 and 7), correct second
unit wire targets, shared subscription deduplication, live reorder/online
pushes, offline filtering and device-aware send targets; 273 workspace tests;
locked release build; p1 158/158, p2 195/195, windows_check, live-snapshot ledger
and regenerated scorecard all pass. Existing helper tests also pass. No real
engines or UA GUI were driven.

## P1 — browser activation and client selection

Task branch `codex/p1-browser-selection` separates row selection from explicit
activation. Double clicks and the browser Assign command target the selected
client strip's first empty insert; an explicit assign header takes precedence.
Preset double clicks without a header load the selected channel. With no
selected channel, no default target is guessed.

Rack_Selection LoadProperties/SaveProperties (0x180a7cd40/0x180a7e940) and
AssignPluginToNextAvailableSlot (inserts-work/decomp/1812d44f0.c) establish
client selection rather than engine SelectedOnFront. The exact native
selection-changing gestures remain unknown: Rust explicitly records the last
mixer strip pressed/dragged. This policy is documented in code, not labelled
as verified native gesture parity. Selection is currently session-local.

Validation: three interaction/routing tests, 276 workspace tests, locked
release build, p1 158/158 and p2 195/195, windows_check, live-snapshot ledger
and regenerated scorecard pass. No real-engine writes or GUI automation.

## P1 — incoming MIDI buffers

Task branch `codex/p1-midi` adds bounded JUCE MemoryBlock decoding and native
MidiBuffer records (little-endian i32 position, u16 length, message bytes).
Provenance: Mac CMxGetMidiBuffers::execute 0x10028c168; Windows reply
0x180e10810 and iterator 0x181e003d0. RFC base64/raw-stream assumptions are
rejected. Malformed batches fail atomically, with byte/batch limits.

A selected MIDI input polls the audited GetMidiBuffers RPC with correlation,
backoff and duplicate watermarks reset on input/transport changes. Exact typed
read APIs also cover GetCorrectionImage's three valid slots; arbitrary posts
remain gated. MIDI writes use the normal writer. NOTE-on/nonzero CONTROLLER
messages matching configured tap channel/number feed four-tap tempo. Host
receipt time is used conservatively, at most one tap per batch: native event
position/timestamp units remain unproven. Program Change recalls only an
existing scene; zero-based program-to-scene indexing is an explicit Rust
policy awaiting native traffic parity. NOTE-to-scene mapping remains pending.

Validation: bounded codec/malformed record tests, tap filtering and duplicate
suppression, scene existence/channel filtering and closed writer gate, and
owned synthetic loopback RPC correlation/polling with writes enabled/disabled.
281 workspace tests (9 ignored), locked release build, p1 158/158, p2 195/195,
windows_check and live-snapshot ledger/scorecard pass at this task boundary. No live MIDI RPC
or hardware writes were sent.

A separate read-only Linux preflight reported Apollo x4 DeviceOnline=false.
The proposed supervised test is prepared outside git at
`/tmp/apollo-supervised-test-plan.md`; the user approved its bounded fader-and-restore limits and subsequently
authorized Apollo investigation while unused. A fresh preflight still reported
DeviceOnline=false, so no hardware writes were sent. The daemon remains running;
ALSA currently enumerates no Apollo, and Thunderbolt device enumeration is empty.

## P2 — approved hardware preflight (blocked before writes)

The user approved the bounded ANALOG 1 fader-and-restore test, confirmed the
channel is spare, and subsequently allowed Apollo investigation while unused.
No further permission is needed to run that approved test once its online
preconditions hold. No Mac Console automation is authorized or required.

Fresh Linux daemon snapshot: Apollo x4, DeviceOnline=false, ANALOG 1 Mute=true,
Stereo=false, FaderLevel=0 dB. Local process 1324 runs the exported
`/home/rolo/oa-test/driver-v2-latest/mixer-engine/ua_mixer_daemon.py`; the older
handoff's pid 1386 is stale. This running process was not restarted or modified.

Titan (hostname omarchy) currently has no Apollo ALSA card, no enumerated
Thunderbolt domain/device and no Thunderbolt NHI endpoint. PCI reports Intel
8086:1136 at 0b:00.0 behind root port 00:1c.4. The boot kernel reports:
`devices behind bridge are unusable because [bus 0c] cannot be assigned for them`.
The root port has subordinate bus 0b, while its downstream bridge needs bus 0c;
0c:00.0 is also reported as a Mellanox 15b3:1015 adapter. This is a host PCI
resource/enumeration failure before Apollo transport or DSP acceptance can be
assessed. Raw read-only snapshots remain outside git under /tmp.

After the user power-cycled the Apollo and reseated the Thunderbolt cable,
a targeted non-removal rescan of 0b:00.0 was performed. It reproduced the same
bus-assignment failure and detected no NHI or Apollo. No PCI remove, bridge
reset, bus-number rewriting, network-adapter reset, firmware change or reboot
was attempted. The bounded fader test aborted without Apollo writes;
its exact bounded plan remains `/tmp/apollo-supervised-test-plan.md`.

## P1 — monitor correction image

Task branch `codex/p1-correction-image` decodes GetCorrectionImage as standard
base64 UTF-8 SVG, with payload limits, empty-profile handling and retry backoff.
Native provenance: Rack response 0x1811685c0, decoder 0x181c02a30, SVG wrapper
0x18116b9e0; protocol/v131/MAP.md specifies the type argument and update counter.
The primary device's speaker/headphone capability flags control retrieval;
image-update counters and transport/device changes invalidate cached results.
The root RPC has no device argument, so a primary image is never attributed to
a different selected device. Every request uses the typed read RPC, without
opening the writer gate.

The main monitor graph renders the received SVG at 2x into an in-memory image.
Three stable image keys replace prior data and GPU textures, including resized
texture invalidation. Bounds and external image-reference refusal prevent an
engine image from following file/network URLs. The native curve CSS variable families use one Rust theme green, matching
the native wrapper's shared curveColor binding; tickmarks use a 10px fallback.
Exact calibrated-profile
pixel/color parity is not claimed. Missing/invalid images explicitly report
NO PROFILE IMAGE AVAILABLE. No numeric response curve is invented.

Headphone images are decoded/cached and exposed through the model, but the
current headphone panel has no verified graph region; placing that graph is
still open rather than inventing new window dimensions.

Verification: synthetic standard-base64/UTF-8/image rejection tests, loopback
initial load/cache/update-count refresh under a closed writer gate, and SVG
pixel/alpha/bounds/cache replacement checks. The explicit headless GPU test
`correction_refresh_reuses_gpu_slot_and_invalidates_resampling` passes with
red/green/red output pixels and a single reused full/resampled texture slot.
284 workspace tests pass (10 ignored, including the explicitly passed GPU
test above); locked release build, p1 158/158, p2 195/195, windows_check and
live-snapshot ledger/scorecard pass. No UA assets or native correction profiles were committed,
and no real-engine correction RPC or hardware write was sent.

## P2 — manual scorecard refresh

`tools/ledger/refresh_catalog.py` refreshes the external catalogue with a
bounded, idempotent managed block, preserving unrelated edits and a timestamped
backup outside git. It replaces stale claims that clipboard, presets,
scenes/session folders, drag routing, multi-device grouping, MIDI and
correction retrieval are entirely absent. Existing measured alignment labels
are retained; implemented drag views are rendered, not claimed pixel-aligned,
and missing native device tabs are explicit.

Statuses distinguish source/mock/software evidence from hardware acceptance.
A/B is still a chrome-only toggle with no engine action; sidechain/IN-OUT/
multi-out and typed-entry acceptance remain open. Native opaque preset state,
DSP execution, real demo activation, shared-bus ownership, NOTE-scene mapping,
native MIDI time units, headphone graph placement and calibrated-profile
pixel parity are not marked verified. SET DEFAULT is still a no-op; the exact
insert operation has not been established (numeric remote-default setters
alone do not prove the insert behavior).

Windows traffic capture is blocked separately: the existing wine-ua-131 prefix
still contains Console 1.2.9.905, not verified 1.3.x, and no private X server is
installed. The old launch script points at real ports and captures the user
session; it was not run. Live hardware acceptance remains blocked by the
recorded Thunderbolt PCI enumeration failure. No Mac Console was driven.

The catalogue is regenerated with build_ledger.py after the full §2 checks.

Validation: 284 workspace tests pass (10 ignored; GPU correction acceptance was
run explicitly in P1), locked release build, p1 158/158, p2 195/195 and
windows_check. Ledger tests and regeneration complete. The refreshed catalogue
reports 109 elements, 70 aligned, 76 wired and 75% with verification evidence;
that percentage combines unit/mock/trace evidence, not hardware acceptance.
The wire-profile report still flags undo-refresh and drag-uncoalesced false;
these are retained findings, not silently treated as fully matched protocol.
