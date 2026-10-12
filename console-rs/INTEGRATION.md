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
