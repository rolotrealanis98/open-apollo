# UAD REC / MON (`RecordPreEffects`) on native insert chains

Status, 2026-10-19: **proven on the x4 (bench, not a UA trace).** Bit `0x4` of a block's
word 4 is the record tap, and moving it is UAD REC / MON: on the last effect the record and
monitor are both wet (REC, today's recipe); on the Mic strip `m0` (row 1 word 4) the record is
dry and the monitor stays wet (MON). It works at CREATE and live through SERVICE, both ways,
with no fault (HARDWARE-SESSIONS.md, "record tap flag"). Not yet checked: the Unison case
(sub 0 is the Unison header), more than one insert, stereo, other rates.

Implemented 2026-10-19 for native insert chains (steps 2, 4, 5 and 6 below; no driver change):
`compose(record_pre_effects=)`, `ComposedChain.record_tap_plan`, `NativeChainBackend.set_record_tap`,
`MixerDaemon._native_record_tap`. Hardware-free tests only; the daemon path has not run on the x4
yet (check below).

## What the switch means (proven)

| Fact | Source |
|---|---|
| `RecordPreEffects` **false = UAD REC** (record wet: the inserts are printed), **true = UAD MON** (record dry: the inserts are heard in the monitor mix only). | UA's *UAD Plug-Ins Manual* (SDD-3000 FAQ: "UAD MON (print dry) instead of UAD REC (print wet)"); `console-rs/crates/rack/src/input.rs:54` |
| The **Unison insert is always recorded**, whatever the switch says. The switch affects only the regular INSERTS slots. | Same manual, amp chapter: "Unison insert processing is always recorded" |
| UA's engine default is **false** (UAD REC). | `console-rs/crates/ua-engine/tests/fixtures/apollo-x4-12.0.0.snapshot.json` |
| Console 1.3.1 writes it as an undoable set, one input at a time (`/UndoRecording true`, `set /devices/0/inputs/3/RecordPreEffects/value false`, `/UndoRecording false`); the Navigator's INSERT EFFECTS button writes every input. | `console-rs/crates/engine-model/tests/fixtures/console-trace/ui-writes.txt:145`; `surfaces.rs:423` |
| With no insert on the input, the switch changes nothing audible: record and monitor both take the plain strip's output. | follows from the above |

**Mismatch to fix separately:** `mixer-engine/device_maps/device_map_apollo_x4.json:943` (and
the x8p map) give `RecordPreEffects` the value **true** (UAD MON), UA's 12.0.0 snapshot gives
false. Since the daemon prints every insert today, a fresh state that says MON while
recording wet is wrong either way.

## What open-apollo does today (proven)

- The daemon stores the value and does nothing else (`mixer-engine/hardware.py:3626`,
  `console-rs/WIRING.md` row 24).
- Every native insert is printed: the chain replaces the input's strip on DSP 1, ahead of
  both the record channel and the monitor mix (HARDWARE-SESSIONS.md, 2026-10-14 runs).
- Delay compensation already separates the two paths: `InputLatency.record` is "latency
  through the recording tap" and `plan_compensation` computes `record = budget - record`
  and `monitor` from the group's total (`mixer-engine/plugin_delay.py:56`). The backend
  always passes `record = total` (`plugin_native_chain.py:169`, `:278`).

## The chain, as the recipe encodes it (proven from captured recipes)

Every block (the chain head, the Mic Input Strip's `m0`, each effect's root row) starts with
the same header: word 0 `0x40000000`, words 1/2 inputs/outputs, **word 4 flags**, then
block-specific words.

| Block | Word 4 | Where |
|---|---|---|
| Chain head (row 0), plain strip or chain | `0x22` | `driver/ua_strips.c:512`, `native_chain_composer.py:203` |
| Mic strip `m0` in a **plain** strip (the only sub) | `4` | `driver/ua_strips.c:464` (`ua_strip_usb_m0a`, mask `0x0f1f7fe4`) |
| Mic strip `m0` in a **chain** | not written (0) | `native_carrier.py:22` (`MIC_STRIP` mask `0x0f1fffec` skips words 3 and 4) |
| Last effect in a chain | own flags `\| 4` | `native_chain_composer.py:231` |
| Other effects | own flags `& ~4` | same |

So bit `0x4` of word 4 sits on exactly one sub, the last one, in every recipe we have
(plain: the Mic strip; chain: the last effect). The captured chains were made in UAD REC
as far as anyone recorded; their REC/MON state was not noted.

The Mic strip `m0` owns both compensation delay lines: words 6/7 point at two 2048-word
buffers with the **monitor** delay in word 8, words 14/15 at two more with the **record**
delay in word 16, word 5 is the active capacity (`native_carrier.py:37` `head_rows`,
`driver/ua_strips.c:945`). Two lines in one block means the strip produces the record and
monitor signals separately, so the record/monitor split is somewhere the strip can see.

The driver's SERVICE path refuses writes only to the chain directory (head words 0, 5 and
odd words ≥ 7, `driver/ua_plugin_instance.c:534`). Head word 4, `m0` word 4 and an effect's
word 4 are not link words, so **a live flip of any of them already passes the existing
INSTANCE SERVICE ABI**; only a topology change needs a new CREATE and swap.

## Candidates, ranked

| # | Candidate | For | Against |
|---|---|---|---|
| A | **The `0x4` flag moves**: REC puts it on the last effect (today), MON on sub 0 (the Mic strip, or the Unison header). | It is the only per-sub word that already differs between plain and chain recipes; "Unison is always recorded" matches a tap right after sub 0; the composer's author already named it `tap`. | It may only mean "last sub writes the container output". Then moving it would also cut the effects out of the monitor mix. |
| B | **A Mic strip `m0` word** selects where the record line taps (pre or post the subs that follow). | `m0` owns both delay lines, so it is where the split physically is. | No unexplained `m0` word is known to differ between captures. |
| C | **Head word 4** (`0x22` → another value). | A head-level flag that is never a directory word. | Same `0x22` in plain and chain heads; no other value ever seen. |
| D | **`{0x001E0004, input, sub, value}`** with a new sub. | It is the existing per-input DSP parameter command. | Known subs are 5 (scale), 7 (route), 8 (span), all strip I/O, not chain internals (`driver/ua_apollo.h:620`). Weakest. |
| E | **UA rebuilds the chain** (new CREATE with A/B/C baked in, then swap). | Always possible; UA's engine already rebuilds on delay-comp changes. | Would glitch; nothing says Console glitches on REC/MON. |

Bench result, 2026-10-19: **A is proven** for one mono insert: with the flag on `m0` the
record is dry and the monitor wet. B–E are not needed. A UA capture would still confirm that
UA does the same for Unison and for multiple inserts (steps 4 and 6 of the plan below).

## Capture plan (Titan's Windows VM, or a Mac with UA Console)

Prerequisite: the Apollo must be attached to the machine running UA's stack. The x4 is on
the Mac Studio now; the Windows VM on Titan needs an Apollo passed through, or use a Mac.
Use the same trace method as the earlier captures: on a Mac, the DTrace kext capture (wiki
"Device-Capture-macOS"); on Windows, the DSP ring / BAR0 capture that produced
`win-dsp0-ring-complete-20260319.json`. Capture outputs stay private, as all UA captures do.

1. 48 kHz, delay compensation **Off**, a fresh Console session, nothing on any insert.
2. Load **1176LN Legacy** on input 1, slot 1, in UAD REC (the default). Start the trace.
3. Toggle input 1 to **MON**, wait 3 s, back to **REC**, wait 3 s. Do it three times. Write
   down the wall-clock time of each click (or capture TCP 4710 alongside: the
   `set /devices/0/inputs/0/RecordPreEffects/value` lines are the timestamps).
4. Repeat 2–3 with **two** inserts (1176LN + LA-2A): shows whether a flag leaves the last
   effect, and for which sub.
5. Repeat 2–3 with delay compensation **Medium**: in MON the record path loses the plug-in
   latency, so `m0` word 16 should change (record delay = budget), word 8 should not.
6. If a Unison plug-in is authorized (e.g. 610-B): Unison + 1176LN, toggle. Shows how the
   tap is placed when sub 0 is the Unison header.
7. Sanity audio check: record a DAW track while toggling, with the 1176 Input high; the
   track must go dry in MON.

What to keep from the trace: every DSP command on all four DSPs from 50 ms before to 2 s
after each click: WRITEs (`0x0014`, row address, mask, values), `0x001E0004`, allocation /
CREATE / SYNCH / route commands, and the instance's memspec (row base addresses) from step 2.

How to diff: rewrite each WRITE address as `(row, word)` with the memspec (addresses change
between runs), then diff the REC→MON burst against the MON→REC burst. Expected outcomes:

- a burst of 1–3 WRITEs to word 4 of sub 0 and the last effect → **A** (record which way);
- a WRITE to another `m0` word → **B**; to head word 4 → **C**;
- a `0x001E0004` with a sub other than 5/7/8 → **D**;
- an allocation, full init and a swap → **E** (then diff the two recipes the same way).

### Bench test of A without UA

Done 2026-10-19 with `tools/rectap-bench.py` (HARDWARE-SESSIONS.md, "record tap flag").
The monitor mix kept the 1176's level and shape with the flag on `m0` while the record matched
the plain strip. The dry record also arrives 55 samples before the wet monitor: the 1176 Rev A
chain adds 55 samples that `native_known_good.json` lists as 0, so delay compensation
undercounts it today.

## Hardware check of the implementation (owner)

Daemon running this branch with plug-in hosting, PipeWire stopped (arecord needs `hw:x4`), input 1
unmuted and soloed, nothing else driving the mixer, delay compensation Off. Load the 1176 Rev A on
input 1 from the console, Input high. Then, measuring as `tools/rectap-bench.py` does (record ch 0 =
input 1, ch 14 = MON L; RMS and the MON-vs-record correlation):

1. UAD REC: record wet (well above the plain strip), MON wet.
2. Click MON on input 1: record back at the plain-strip level, MON unchanged (wet). The daemon log
   has no `record tap` warning and the console's LED stays on MON.
3. Click REC: record wet again. Unload and reload the insert while in MON: it comes up dry.
4. Delay compensation Medium, MON ↔ REC: the chain is rebuilt (`create`/`swap` in the log);
   record and MON keep their levels.

## Implementation, once the word is known

1. **Evidence.** Done for one mono insert: REC = `0x4` on the last effect's word 4 and 0 on
   `m0` word 4; MON = `0x4` on `m0` word 4 and cleared on every effect. Still to check on the
   bench: two or more inserts (expect every effect cleared), stereo, a Unison header as sub 0.
2. **Composer.** `compose(..., record_pre_effects=False)` in `native_chain_composer.py`.
   False must stay byte-identical to today's recipe. True writes the MON words (`0x4` on
   `(1, 4)`, cleared on every effect) and lists them in `observations`. Add
   `ComposedChain.record_tap_plan(pre)`: a `ControlPlan` of the live writes and their
   expected readback, for a flip without rebuild.
3. **Driver op.** None: the words are private, non-directory, and SERVICE already writes and
   reads them (proven live on the x4).
4. **Delay compensation.** `InputLatency(channels, total=latency, record=0 if pre else
   latency, ...)` at `plugin_native_chain.py:169` and `:278`; `plan_compensation` already
   handles it. A stereo pair takes the value of its first input (both are written by Console).
   Overload (`required`) stays on `total`: the monitor path still carries the latency.
5. **Daemon transaction.** In `hardware.py`'s `RecordPreEffects` branch, call the native
   backend `set_record_tap(channels, pre)`, serialized with insert transactions:
   - no chain on the input: store only, no device I/O;
   - chain present, record delay unchanged (compensation Off or zero latency): SERVICE the
     `record_tap_plan`, read back, publish on CONFIRMED;
   - record delay changes: rebuild with the new value through the existing stage/swap path,
     like `set_compensation`, also updating the retained strips' record delay if needed;
   - FAILED: keep and republish the old value; INDETERMINATE: quarantine the native session.
   New chains read the stored value when composed, so loading an insert in MON comes up dry.
6. **Console.** WIRING.md row 24 becomes "wired"; INSERT EFFECTS (all inputs) becomes one
   transaction per input that has a chain.
7. **Defaults.** Align the device maps' `RecordPreEffects` default with UA's (false), in its
   own commit.

## Tests that need no hardware

- `tests/test_plugin_delay.py`: `record=0` gives record delay = budget at every setting,
  monitor and overload unchanged; stereo groups.
- Composer: `record_pre_effects=False` matches a golden of today's recipe for 1–4 effects,
  gaps in `slots`, mono and stereo; True puts the MON words where the evidence says, passes
  `check()`, and lists them in `observations`; `record_tap_plan` writes only non-directory
  words. Keep the tap location in one table so a different capture result changes one
  constant, not the tests.
- `tests/test_plugin_native_chain.py` with its fake driver: a toggle with no chain does no
  I/O; with a chain it sends one SERVICE plan (delay unchanged) or one stage/swap (delay
  changed); readback mismatch → FAILED and the old value republished; driver exception →
  INDETERMINATE and quarantine; a chain created in MON is composed with the MON words.
- `tests/test_native_delay_daemon.py`: the published record/monitor delays follow the switch.
- Daemon handler: `set …/RecordPreEffects` publishes only after CONFIRMED; INSERT EFFECTS on
  four inputs runs four transactions.

## Where this was searched

The repository (driver comments, `NATIVE-HOSTING.md`, `HARDWARE-SESSIONS.md`, console traces,
device maps, git history), and the strings of UA's Windows install in `~/uad-windows` (read
only, nothing copied): `UAMixerHelper.exe` and `Rack.dll` name `RecordPreEffects`, but only
AutoGain uses it; the DSP graph code is not identifiable from strings. The private runs
(`runs/genuine-1176-native-allocation-20261011-2`, the parity notes) are not on this machine.
