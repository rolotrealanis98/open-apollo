# Native plugin investigation — 2026-10-12

## Located work and transcript

The Wine investigation is Codex thread `01a12885-2f60-7452-a616-f44fc44949e9`:
`/home/rolo/.codex/sessions/2026/10/10/rollout-2026-10-10T21-12-59-01a12885-2f60-7452-a616-f44fc44949e9.jsonl`.
The related work is in `/home/rolo/wine-ua-work`, with committed evidence under
`analysis/full-plugin-support/` and tools under `tools-wine/`. This checkout has
existing uncommitted changes; they were preserved.

The final transcript handoff cites `fbcf7ee`, which lives in a separate git
repository, `/home/rolo/open-apollo-plugin-native`, branch
`codex/plugin-native-query`. Its companion worktrees are
`/home/rolo/open-apollo-plugin-integration` (`codex/plugin-integration-479b42e`,
tip `5dc2455`) and `/home/rolo/open-apollo-plugin-daemon`
(`codex/plugin-daemon`, tip `53d3a66`). The native tree currently has uncommitted
bootstrap tracing declarations/calls in ua_apollo.h and ua_core.c. Do not
overwrite or mistake those edits for the committed baseline.

## Driver state

Fetching GitHub's driver-v2 yielded `9469f9a`. The separate native repository
has a newer Mac-exported driver-v2 foundation at `2d59108`, imported through
`/home/rolo/oa-test/mac-driver-v2-2d59108.bundle`. The driver-v2 history includes
firmware I/O descriptors and generated model tables, lifecycle/clock/rate
fixes, inherited input-strip preservation, diagnostics, and more daemon
commands. Its changelog says the earlier Codex hardening was reconciled by
behavior. Reapplying all of `199bb21` to this foundation would duplicate and
conflict with that work; the old console branch is still a different baseline.

The newer daemon implements host sessions/presets, clear commands, undo and
protocol fixes that the console checkout's older daemon lacks. These are
daemon-owned JSON files, not proof of UA session-file compatibility. The
plugin branches additionally model load/control/retirement transactions and
parameter-tree publication, with software backends and fail-closed production
admission. They do not yet provide a working production DSP loader.

At `fbcf7ee`, native work includes fresh board queries, opt-in retained board
registration, host backing, bounded page-DMA bindings, a four-pool per-owner
allocator, shared-code holds, fragmentation accounting and atomic resource
sets with rollback. Published plugin operations remain unavailable. The next
backend step is a verified bootstrap/ownership handoff tied to all strip and
dynamic resource placements, then real upload/create/terminal completion.
Warm inherited DSP state cannot be treated as an empty allocation ledger.

Verified this turn using an archive of committed `fbcf7ee` under
`/tmp/apollo-native-checkpoint`, preserving the original dirty checkout:

- 50 driver/board-query host tests passed.
- 109 daemon plugin tests and 85 subtests passed.
- Kernel module built for `7.2.8-5-omarchy-bore`.
- No module installed/loaded, hardware writes, live DSP upload, or audio test.
  This execution environment exposes no loaded `/sys/module/ua_apollo`.

## What the previous investigation proved

`wine-ua-work/analysis/full-plugin-support/mac-live-plugin-cycle-20261011/README.md`
records original Mac 1176LN Legacy load, quantized Attack SET/readback, power
and remove, reported DSP/memory changes and 824 consecutive driver records.
It explicitly says no audio signal was verified and Linux creation/processing
remains unverified.

`mac-plugin-survey-20261011/README.md` describes 138 authorized-plugin trials
with loaded metadata and cleanup observed, 131 distinct schemas (transcript
handoff), and retained longer-settle/pool/chain follow-ups. These are useful
reference captures for the Linux implementation, not Linux loading proof.
Pool-sharing and per-core saturation evidence motivated `fbcf7ee`.

## Rust UI changes and verification

Rust already hosts generated ConsoleLink panels independently of Windows
plugin editor instances. The seven-spec development sample was the default
discovered folder. The full integrated stage contains 223 specs and sprites:

`/home/rolo/wine-ua-work/plugin-specs/integrate/stage/Console Link/Console Link/PluginSpecs`

The loader now checks that stage before the small sample, after an explicit
environment override and the installed user catalog. No proprietary assets
were copied into the repository. The current source pipeline's AUDIT.txt says
162/223 specs are fully complete upstream; this is different from Rust runtime
compatibility and can change as Claude continues the source work.

Testing the actual newer stage initially failed on combined meter sources
(Marshall) and keyed action rows (Studer). Ported those types/semantics from
the local Swift runtime, along with integer meter decoding, absolute-value
conditioning and ignore-zero hold behavior. Derived meters do not poll an
invented variable ID. Inline regression tests cover these behaviors.

All 223 specs now decode, resolve their declared panel sprites, and open/build
native plugin-window draw lists. The 1176 and Marshall panels were also GPU
rendered and inspected. Marshall initially showed only the toolbar: Image
elements pushed a second enclosing clip instead of popping their own clip,
hiding subsequent art and hit regions. Fixed that stack imbalance and added
a generated-image regression covering later images and controls. Marshall
now renders its amp panel and exposes 37 hit regions instead of 9. Previews
are under `/tmp/apollo-plugin-previews`.
These tests use mock parameters, never a real engine.

Final locked all-target workspace build passed; workspace tests passed
(234 passed, four ignored). The optional webview plugin suite also passed
30 tests before the final clipping regression was added; the final workspace
suite includes that regression. The checkpoint was subsequently committed through HANDOFF-CODEX.md; see INTEGRATION.md for the connected branch history and final validation.

Opening a panel is not full runtime parity. A source-key audit found 75 specs
using additional unported features, including local state and state/meter
lookup keys, graphs, sprite-font/meter readouts, touch/held/blink/segment looks,
swipe/long-press input, and copy/random/snapshot actions. Unknown spec keys
remain tolerated by the decoder. The machine-readable per-plugin audit is
`/tmp/apollo-plugin-previews/runtime-gaps.json`; regenerate it with
`python3 console-rs/tools/ledger/plugin_runtime_gaps.py "SPEC_FOLDER"`
against the moving upstream catalog before planning the remaining port.

Optional HTML hosting (`cargo build -p uaconsole --features webview --locked`)
builds here, with local lunacomponents for 1176 Rev A and SSL G. The mock HTML
1176 test created its loopback bridge and control map, then stopped because
GTK cannot initialize without a display. This turn verifies compilation; a
live HTML screenshot remains unverified. HTML chrome remains separate from
the GTK window.

## Next implementation sequence

1. Bring the reconciled driver-v2 daemon baseline into an isolated console
   integration checkout, preserving current Rust changes and native branch
   edits. Re-run protocol/console tests against its software-only mode.
2. Port remaining Swift panel-runtime semantics with per-feature tests, then
   test UI-to-parameter-tree edits using captured schemas and independent
   observers. Keep simulation visibly distinct from hardware-loaded slots.
3. Continue the native bootstrap ledger and upload/completion boundary from
   `fbcf7ee`; use the Mac captures as comparison evidence. Do not declare load
   success from EffectName, elapsed time or aggregate DSP percentages.
4. Only after a genuine production instance path is ready, separately verify
   plugin parameter readback, power/remove, DSP signal processing, and meter
   polling end to end. Rust removes the Wine editor-instance dependency; it
   does not replace the Apollo's DSP loader.
