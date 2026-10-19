# Console controls → mixer daemon → Apollo

Every control uaconsole shows that can change the Apollo, traced from the button to the driver,
for an Apollo x4 on the open-apollo mixer daemon (`mixer-engine/ua_mixer_daemon.py`). Built
on 2026-10-13 from `rc/console-wiring`.

## How to read this

- **Console 1.3.1 sends**: what UA's Console puts on the wire for the control
  (`~/wine-ua-work/console-port/protocol/v131/MAP.md`; `set` carries `?func_id=n`, mixer
  controls also `context_type=main`; `post /<Command>`).
- **console-rs**: where uaconsole makes that write. `apply` is `engine-model/src/lib.rs`
  `EngineMixer::apply`, `surface` is `engine-model/src/surfaces.rs` `apply_surface`, `menu` is
  `engine-model/src/menus.rs` `apply_menu`, `settings` is `rack/src/settings.rs` `resolve`
  plus `engine-model/src/settings.rs` `write_settings`. All writes go out through
  `ua-engine` `Store::{set, commit, set_now, post}`.
- **Daemon → Apollo**: `_handle_set` stores the value, then `hardware.py`
  `HardwareRouter.on_set` picks the handler. Calls are
  `set_mixer_param(type, ch, param, value)` (type 1 preamp, type 2 monitor / "other" params),
  `set_mixer_bus_param(bus, sub, value)` and `set_driver_param(param, value)`.
- **Before / Now**:
  - `wired`: reaches the Apollo, and the console shows the engine's value afterwards.
  - `partial`: reaches it, with a defect (given).
  - `ignored`: the daemon stores the value but writes nothing.
  - `missing`: never sent.
  - `UI-only`: drawn, no write.
  - `host`: host-side by design, with no Apollo register.
  - `n/a`: Console does not show it on an x4.
- **Test**: the `engine-model/tests/daemon_hardware.rs` test that presses the control in the
  console model against `mixer-engine/tests/fake_hw_daemon.py`, and checks the driver or mixer
  call the daemon makes and what the console shows afterwards. `fake_hw_daemon.py` is the real
  daemon on a private loopback port with a recording backend. Daemon-side tests are in
  `mixer-engine/tests/test_console_wiring.py`.

**Writes: on for the user's console, off for scripts.** The **Open Apollo Console** menu entry and
the setup wizard's Launch button start `uaconsole --writes`; plain `uaconsole` (scripts, tests,
agents) starts read-only. Before this branch, Settings' **WRITES** switch only
opened the writer: the connection kept refusing `set` and `post`, so a console launched without
`--writes` changed nothing on the Apollo even with the switch on. The switch now opens both
gates (`Store::set_engine_writes`, `EngineHandle::set_allow_writes`; test
`writes_switch_opens_the_engine_connection`). Making writes the launcher's default is the
owner's decision; it is unchanged here.

## Totals

One row can cover a few closely related controls (MUTE / MONO, the CUE OUTPUTS row …).

| Rows | Before | Now |
|---|---|---|
| Listed | 72 | 72 |
| `host` (no Apollo register) or `n/a` on the x4 | 11 | 11 |
| Can reach the Apollo | 61 | 61 |
| … `wired` | 28 | 54 |
| … `partial` | 22 | 0 |
| … `ignored` / `missing` / `UI-only` | 11 | 7 |

The 7 still not wired, and why:
- BUFFER SIZE: PipeWire owns the quantum.
- UAD REC/MON, inserts and Unison: they need the plug-in DSP backend.
- CHANNEL DSP PAIRING's DSP side (spanning, 69 samples latency): it needs the DSP backend too.
- Monitor and headphone correction: there is no SoundID DSP.
- SOURCE CUE 3/4, and HP source MON or NONE: no hardware encoding is known. The daemon refuses
  them and the console snaps back.

## Global and status bar

| # | Control | Console 1.3.1 sends | console-rs | Daemon → Apollo | Before | Now | Test |
|---|---|---|---|---|---|---|---|
| 1 | RATE (status bar menu) | `set /SampleRate/value?func_id=n 96000` | surface `SampleRate(i)` → `commit` | `_handle_clock_set` → `set_global` → `set_driver_param(0, hz)` (or PipeWire `clock.force-rate` when PipeWire holds the streams); published only if the driver took it; read back from `UA_EVT_RATE_CHANGED` and a 1 Hz poll | partial: a refused rate stayed on screen | wired: a refusal snaps back. The daemon sends the rate in effect before its error, and the store drops the echo hold and re-reads (`Store::refusals`). With native plug-in hosting (2026-10-19): allowed while no plug-in is loaded (hosting pauses and comes back by itself at 48 kHz); refused with "Remove the plug-ins first" while one is. Before, every rate and clock set was refused silently whenever hosting was on | `sample_rate_and_clock_source_reach_the_driver`; `test_native_console_gate.py` |
| 2 | CLOCK (status bar menu) | `set /ClockSource/value?func_id=n "S/PDIF"` | surface `ClockSource(i)` → `commit` | `set_global` → `set_driver_param(1, code)` (Internal 0x0C, S/PDIF 0x00, ADAT 0x05); `/ClockLocked` from `UA_EVT_CLOCK_LOCK` and the readback | partial: nothing showed an unlocked clock | wired: an amber `Info-Focus` outline blinks round CLOCK (500 ms on, 500 ms off) while `/ClockLocked` is false, as `Rack_ClockInfo::paint` 0x181547d00 does. A pending rate shows the requested value, as in Console | same |
| 3 | Settings › SAMPLE RATE | as 1 | settings `""` kind → `write_settings` → `commit` | as 1 | partial (as 1) | wired | same |
| 4 | Settings › CLOCK SOURCE | as 2 | settings | as 2 | partial (as 2) | wired | same |
| 5 | Settings › BUFFER SIZE (shown because the daemon publishes `/BufferSize`) | `set /BufferSize/value` (Windows engines) | settings | while PipeWire answers: `pw-metadata -n settings 0 clock.force-quantum N` (`/openapollo/pipewire/ForceQuantum`), and `/BufferSize` follows PipeWire's effective quantum; without PipeWire stored only | ignored | wired on Linux (PipeWire's quantum); stored only without PipeWire | `mixer-engine/tests/test_pipewire_state.py` |
| 6 | Settings › PT MODE | `set /PTMode/value` | settings | tree only | host | host | — |
| 7 | TEMPO | `set /Tempo/value` | `surface_set_text` | tree only (plug-in tempo) | host | host | — |

## Input strip (ANALOG 1 shown; every preamp input alike)

| # | Control | Console 1.3.1 sends | console-rs | Daemon → Apollo | Before | Now | Test |
|---|---|---|---|---|---|---|---|
| 8 | Preamp gain | `set …/preamps/0/GainTapered/value` | apply `Preamp("preamp_gain")` → `set` (drag, bracketed) | `_handle_preamp` → `(1, ch, 0x0A/0x09/0x06, …)`; `Gain` companion echoed | wired | wired | `preamp_and_input_strip_controls_reach_the_mixer` |
| 9 | +48V | `set …/preamps/0/48V/value` | apply `Preamp("48v")` → `commit` | `(1, ch, 0x03, 1/0)` | wired | wired | same |
| 10 | PAD | `…/Pad` | apply | `(1, ch, 0x01, …)` | wired | wired | same |
| 11 | LOW CUT | `…/LowCut` | apply | `(1, ch, 0x04, …)` | wired | wired | same |
| 12 | Ø | `…/Phase?func_id=n` | apply | `(1, ch, 0x05, …)` | wired | wired | same |
| 13 | MIC / LINE | `set …/inputs/N/IOType/value "Line"` | surface `InputSource(i)` | `_handle_input` → `(1, ch, 0x00, 1/0)` | wired | wired | same |
| 14 | HI-Z (front jack) | read-only `…/preamps/0/HiZ` | `preamp()` source | readback pushes `HiZ` | missing: the button never showed HI-Z | wired: HI-Z on the button, ticked and disabled in SOURCE; a change re-reads the preamp's metadata | `hi_z_jack_shows_on_the_source_button` (MockEngine) |
| 15 | SR CONVERT (S/PDIF strip menu) | `set …/inputs/12/SRConvert/value` | menu `SrConvert` | `(2, 1, 0x1f, …)` | wired | wired | same as 8 |
| 16 | Link | `set …/Stereo/value` (undoable SetStereo) | menu `Link` | `_write_link` → `(1, leader, 0x02, …)` plus a bus recalc | wired | wired | same as 8 |
| 17 | Fader | `set …/FaderLevelTapered/value?context_type=main&func_id=n` | apply `Fader` → `set` (drag) | bus sub 0, L and R | partial: (a) moving a muted strip's fader made it audible; (b) `FaderLevel` stayed stale, so an unmute or a solo's end played the old level | wired: (a) a muted or solo-silenced strip holds its level; (b) `FaderLevel` follows `FaderLevelTapered` | same; `TestMutedStripHoldsItsLevel`, `TestLevelCompanions` |
| 18 | Pan (L / R) | `set …/Pan/value?context_type=main…` (`Pan2`) | apply `PanL` / `PanR` | `set_bus_pan` sub 0 | partial: panning a muted strip made it audible | wired | same |
| 19 | S (solo) | `set …/Solo/value` | apply `Solo` | `_handle_solo_recalc` (all buses) | partial: the recalc used stale `FaderLevel` | wired | same |
| 20 | M (mute) | `set …/Mute/value?context_type=main…` | apply `Mute` | bus 0.0 / recalc | partial: unmute restored stale `FaderLevel` | wired. With `input_strips=1` the strips run on DSP 1, which mixes the analog inputs; the driver now sends their bus coefficients (fader, pan, mute, solo, sends) to DSP 1 as well as DSP 0 (`ua_dsp_move_bus`). Before, only DSP 0 got them and analog inputs 1-4 reached MON at a fixed level whatever M, S or the fader said | same |
| 21 | Solo Safe, Isolate | `…/SoloSafe`, `…/Isolate` | menu / surface | recalc when a solo is on | partial: the recalc used stale `FaderLevel` | wired | daemon tests |
| 22 | S side section, Clear Solos | `post /ClearSolos {}` | surface `ClearSolos` | `commands._clear_solos` → `Solo` false → recalc | partial: as 21 | wired | same as 8 |
| 23 | OL, meter CLEAR | `post /ClearClips {path?}` | surface / menu | host meter clip holds | host | host | — |
| 24 | UAD REC / MON | `set …/RecordPreEffects/value?func_id=n` | apply `RecMon` | tree only (no insert DSP yet) | ignored | ignored: needs the plug-in DSP backend | — |
| 25 | Inserts, Unison (assign, power, remove, move, presets) | `set …/effects/K/EffectName`, `Power`; `post /MoveInsert` … | surface / menus | native hosting (`--native-plugin-bundles`): load, remove, swap and chains of up to 4 on inputs 1-4, on DSP 1 in place of the input strip; slot Power fails closed | missing | wired on the x4 at 48 kHz (HARDWARE-SESSIONS.md, NATIVE-HOSTING.md) | `test_plugin_native_chain.py` |
| 26 | COPY MIX TO | `post /CopyToSends {"send": i}` (global: every input but talkback) | menu `CopyMixTo` | `commands._copy_to_sends` root form → each input's send `Gain` (+ `GainTapered`, mono `Pan`) → `_handle_send` | ignored: the daemon knew only the per-channel form and answered an error | wired | `input_sends_reach_the_mixer`; `TestCopyMixToSend` |
| 27 | Name, Hide, Channel presets, scenes, sessions | `set …/Name`, `ChannelHidden`; `post /SceneOperation`, sessions… | various | host; a recall writes the hardware through the router | host | host | — |

## Sends (input strip cells, Sends window)

| # | Control | Console 1.3.1 sends | console-rs | Daemon → Apollo | Before | Now | Test |
|---|---|---|---|---|---|---|---|
| 28 | Send level (AUX 1/2, CUE 1/2) | `set …/sends/S/GainTapered/value` | surface `SendLevel(k)` | `_handle_send` → `(bus, sub 3/4/1/2, coeff)` | partial: `Gain` stayed stale, so later rewrites (M off, pre/post, a post-fader send after a fader move) used the old level | wired: `Gain` follows `GainTapered` | `input_sends_reach_the_mixer` |
| 29 | Send M | `…/sends/S/Bypass` | surface `SendMute(k)` | 0.0 / restore | partial: M off restored silence (stale `Gain`) | wired | same |
| 30 | Send PRE / POST | `set …/sends/S/PostFaderOverride/value 0/1` | surface `SendPre(k)` | `_handle_send` → `_rewrite_send` | ignored | wired: a post-fader send carries the fader | same; `TestSendPrePost` |
| 31 | Send pan (full send strips) | `set …/sends/S/Pan/value` | surface `SendPan(k)` → `set` | `_handle_send` Pan → L/R pair with `--stereo-sends`; without it the daemon writes one entry and has no pan | missing: knob stuck at centre, drags dropped | wired (`--stereo-sends`); the knob shows `Pan` | same |
| 32 | Sends window level / M / pan | as 28 / 29 / 31 | settings `fader:`, `toggle:`, `pan:` | as above | partial; pan UI-only | wired | rack `sends_window_pan_writes_the_send_pan` |

## Aux returns

| # | Control | Console 1.3.1 sends | console-rs | Daemon → Apollo | Before | Now | Test |
|---|---|---|---|---|---|---|---|
| 33 | AUX fader | `set …/auxs/A/FaderLevelTapered/value?context_type=main…` | apply `Fader` | `_handle_aux` → `(0x10 + 2A, 0, coeff)` | partial: unmute restored stale `FaderLevel` | wired | `aux_strip_controls_reach_the_mixer` |
| 34 | MUTE | `…/auxs/A/Mute` | apply `Mute` | 0.0 / restore | partial: restored stale `FaderLevel` | wired | same |
| 35 | MONO | `…/auxs/A/MixToMono` | apply `Mono` | `(2, 0, 0x0a / 0x0f, …)` | wired | wired | same |
| 36 | PRE / POST | `set …/auxs/A/SendPostFader/value` | apply `PrePost` | `_handle_aux` → `_rewrite_sends_following("auxN")` | ignored | wired | same; `TestSendPrePost` |
| 37 | Aux → CUE send level / M | `…/auxs/A/sends/S/GainTapered`, `Bypass` | apply `SendLevel` / `SendMute` | `_handle_aux_send` (sub 1/2/5/6) | partial: M off restored stale `Gain` | wired | same |

## Talkback strip

| # | Control | Console 1.3.1 sends | console-rs | Daemon → Apollo | Before | Now | Test |
|---|---|---|---|---|---|---|---|
| 38 | TALK | `set /TalkbackOn/value true\|false` on mouse **down** (the opposite value); after a hold of 0.5 s the release writes `false`. No `/UndoRecording` bracket (`Rack_Talkback` 0x1814975b0 / 0x1814977a0, `UAClnt_Values<bool>::Set`) | `HitKind::Latch` → surface `Talkback` (press) and `Set(Talkback, 0)` (release) → `set_now` | `_handle_device_global` → `(2, 7, 0x46, v)` and `(2, 0, 0x46, v)`; the driver writes setting 13 bit 8 whatever the channel index. Read back from readback word 6 bit 8 | partial: the 0x46 write happened, but the daemon muted every input at start, the talkback mic (input 22, bus 0x0A) included. Console has no mute for it, so TALK was silent. Also bracketed, and no momentary press | wired: the talkback mic keeps the map's −6 dB and is never force-muted (a saved Mute from the old default is dropped). Its CUE 1/2 sends are written at init. Latch, or momentary when held, unbracketed | `talkback_reaches_the_apollo`; `TestTalkback`; rack `talk_button_latches_and_is_momentary_when_held` |
| 39 | TALKBACK TO MONITOR | `set /TalkbackInPhysicalCR/value` on press (0x181489be0) | surface → `set_now` | `(2, 0, 0x47, v)` (TBConfig) | wired, bracketed | wired, unbracketed. Unverified: init writes TBConfig 1 while the tree says false, so the first press may change nothing | same |
| 40 | Talkback fader | `…/inputs/22/FaderLevelTapered` | apply `Fader` | talkback encoding, bus 0x0A sub 0 | partial: muted at start | wired | — |
| 41 | Talkback sends (Sends window) | `…/inputs/22/sends/S/…` | settings | `_handle_send` | partial (as 28) | wired | — |

## Monitor column, control room, CUE outputs

| # | Control | Console 1.3.1 sends | console-rs | Daemon → Apollo | Before | Now | Test |
|---|---|---|---|---|---|---|---|
| 42 | MONITOR knob | `set …/outputs/18/CRMonitorLevelTapered/value?context_type=main…` | apply `MonitorLevel` | `(2, 1, 0x01, raw)` | wired | wired | `monitor_controls_reach_the_mixer` |
| 43 | MUTE / MONO | `…/outputs/18/Mute`, `MixToMono` | apply | `(2, 0, 0x03, mute·2 \| mono)` | wired | wired | same |
| 44 | DIM | `…/outputs/18/DimOn` | surface `MonitorDim` | `(2, 0/7, 0x44, …)` | wired | wired | same |
| 45 | DIM amount | `set /devices/0/DimAttenuation/value` | surface `DimLevel` | `(2, 0, 0x43, step 1–7)` | wired | wired | same |
| 46 | SOURCE MON / CUE 1 / CUE 2 | `set …/outputs/18/MixInSource/value "cue1"` | surface `CrSource(k)` | `(2, 1, 0x04, 0/1/2)` | wired | wired | same |
| 47 | SOURCE CUE 3 / CUE 4 | as 46 | as 46 | refused with hardware: the encoding is unverified | missing | missing; the console snaps back | — |
| 48 | ALT 1 / ALT 2 | `set /devices/0/AltMonSelection/value` | surface `MonitorAlt(k)` | `(2, 0, 0x3a, k)` | wired | wired | same |
| 49 | HP 1 / HP 2 source CUE 1 / CUE 2 | `set …/outputs/19\|20/MixInSource/value` | surface `HeadphoneSource` | `(2, 1, 0x3f/0x40, 0/1)` | wired | wired | same |
| 50 | HP source MON / NONE / CUE 3/4 | as 49 (MON is the value `"default"`, which on the wire is also the reset literal) | as 49 | refused with hardware | partial: written as CUE 1 | missing: no known encoding; refused, and the console snaps back | same; `TestHeadphoneSource` |
| 51 | CUE OUTPUTS MIX / MONO / MIRROR TO | `…/outputs/14-17/MixInSource`, `MixToMono`, `OutputDestination` | settings `map:cue_source`, `toggle:`, `""` | `(2, 0, 0x05/0x06, …)`, mirror 0x2e/0x2f/0x3b | wired | wired | `settings_reach_the_apollo` |
| 52 | MONITOR / HEADPHONE CORRECTION (SoundID) | `…/SpeakerCorrectionPower` … | settings `toggle:` / `not:` | tree only | ignored | ignored: no SoundID correction DSP | — |
| 53 | Meters, Monitor Meters window | subscriptions | — | host meters (PipeWire / ALSA capture) | host | host | — |

## Settings › HARDWARE, device OPTIONS, OPTIONS

| # | Control | Console 1.3.1 sends | console-rs | Daemon → Apollo | Before | Now | Test |
|---|---|---|---|---|---|---|---|
| 54 | DIGITAL MIRROR | `set …/outputs/18/MirrorsToDigital/value` | settings `bool:` | `(2, 10, 0x1e, …)` | wired | wired | `settings_reach_the_apollo` |
| 55 | INPUT DELAY COMPENSATION | `set /MaxDelayComp/value` | settings | host latency figures; ADC delay comp `(2, 0, 0x6a, …)` when it changes | wired | wired | — |
| 56 | CUE BUS COUNT | `set /CueBusCount/value 4` | settings (`offered` filter) | root and device paths → enable buses 5/6 `(0, bus, 0x01, 1)`. The daemon mirrors the device's read-only `CueBusCount` and the CUE outputs' `Active` | partial: (a) the combo shows 2–4 but indexed the engine's 0–4, so choosing 4 wrote 2; (b) the root path was not routed, so CUE 3/4 never came up | wired | same; rack `cue_bus_count_writes_the_count_it_shows`; `TestCueBusCount` |
| 57 | ALT COUNT | `…/outputs/N/AltMonEnabled` per pair | settings `alt:` | `(2, 0, 0x3b/0x3c, …)` | wired | wired | same |
| 58 | DEVICE NAME | `set …/DeviceName/value` | settings | tree (empty names refused) | host | host | — |
| 59 | IDENTIFY | `post /Identify {"path"}` | settings `post:` | red/green 0x1d sequence | wired | wired | same |
| 60 | DIGITAL INPUT (TOSLink) | `…/TOSLink` | settings | device-mode word `(2, 0, 0x21, …)` | wired | wired | same |
| 61 | DIGITAL OUTPUT | `…/TOSLinkOutput` | settings | `(2, 0, 0x21, 8 = ADAT)` | wired | wired | same |
| 62 | MON LEVEL 14 / 20 dBu | `…/outputs/18/Pad` | settings `map:mon_level` | `(2, 1, 0x32, …)` | wired | wired | same |
| 63 | OUTPUT REFERENCE LEVELS 1-2, 3-4 | `…/outputs/0-3/Pad` | settings `pair:` | `(2, 0, 0x19-0x1c, …)` | wired | wired | same |
| 64 | CHANNEL DSP PAIRING | `set /devices/N/ActiveSpans/value` (MaxSpans − cell) | settings `spans:` (one hit per cell; off at ≥ 176.4 kHz) | tree only: the Virtual inputs' `Active` follow (MaxSpans − ActiveSpans virtual channels); the DSP side needs the plug-in DSP backend | UI-only | tree | `dsp_pairing_slider_writes_active_spans`, `TestChannelDspPairing` |
| 65 | OPTIONS › METERING pre / post | `set /PostFaderMetering/value` | settings `map:metering`, meter menu | host meters | host | host | — |
| 66 | OPTIONS › CLIP HOLD | `set /ClipHold/value` | settings | `(2, 0, 0x13, idx)` | wired | wired | same |
| 67 | OPTIONS › PEAK HOLD | `set /PeakHold/value` | settings | `(2, 0, 0x14, idx)` | wired | wired | same |
| 68 | WRITES (open-apollo footer) | — | surface `WritesEnabled` → `set_engine_writes` | — | partial: did not open the connection | wired | `writes_switch_opens_the_engine_connection` |
| 69 | HEADROOM (+24 dBu), FCN SWITCH, MONITOR MODE, BASS MANAGEMENT, USB SAFEGUARD, DANTE, MADI | various | settings | routes exist (0x49, 0x42, 0x4a …); the x4 driver refuses 0x49 and 0x42 | n/a | n/a | — |
| 70 | Input jack sensing, Hi-Z override | `InputJackSensingDisabled`, `preamps/0/HiZ` | no Console 1.3.1 control on the x4 | routes exist | n/a | n/a | — |
| 71 | I/O MATRIX (routes, presets, # INPUTS / OUTPUTS) | `set /inputroutes/N/{IOType,ChannelIndex}`, `post /SaveIOMapPreset` … | settings | daemon-held map; the driver's channel layout does not change | host | host | — |
| 72 | MIDI, NETWORK, CONTROLS MODE, MODIFIERS TIMEOUT | prefs, helper | client | — | host | host | — |

## What else changed under these rows

- The daemon no longer writes 0 dB to every input just before muting it at start, which made
  all inputs blip. The x4 init fixture drops those 44 writes and nothing else.
- Settings combos now write the option they show (`offered` in `rack/src/settings.rs`).
- A refused write is kept in `Store::refusals()` for diagnostics.
- The daemon runs Console's global `post /CopyToSends {"send": i}` (COPY MIX TO). It used to
  know only the per-channel form.
- The 1:1 render is unchanged. `uaconsole --render --preview`, both the default and the CTRL ROOM
  view, is byte-identical before and after (PNG and draw list). The CLOCK outline and HI-Z
  appear only in those states, and send pans show their value (centre by default).

## Hardware check (Apollo x4, by the owner)

None of this was run on hardware. Before you start:
- Install this branch's daemon and uaconsole, and restart the daemon: tray **Restart Daemon**,
  or `systemctl --user restart ua-mixer-daemon`.
- Start the console with `uaconsole --writes`, or start it normally and turn on Settings ›
  **WRITES**.
- Follow the daemon in a terminal with `journalctl --user -u ua-mixer-daemon -f`. Every write
  logs an `HW …` line.

1. **TALK.** Use headphones on HP 1 with CUE 1 at its default (MIX: the monitor mix), and speak
   into the x4's built-in talkback mic.
   - **Before pressing:** the mic must not be heard. If it is, the DSP does not gate the
     talkback bus; report that.
   - **Quick click on TALK:** TALK lights and stays lit. The log shows
     `TalkbackOn = True → 1`. The mic is heard in HP 1 and the monitors dim.
   - **Click again:** it turns off.
   - **Press and hold for more than 0.5 s, then release:** talkback is on while held and off on
     release.
   - **Hardware TALK button on the x4:** the console's TALK follows it.
   - Report if the talkback is much too quiet or too loud in the headphones. Its CUE sends use
     the regular send encoding at −6 dB.
2. **TALKBACK TO MONITOR.** With TALK on, toggle it. The log shows
   `TalkbackInPhysicalCR … (param=0x47)`, and the mic should come and go in the monitors. If the
   first press changes nothing and the second does, the init value of TBConfig is inverted;
   report that.
3. **Sample rate.**
   - Pick 96 kHz from the status bar RATE menu. The log shows `HW DRV: SampleRate → 96000 Hz`, or
     the PipeWire `clock.force-rate` line when PipeWire holds the streams.
   - The driver logs the rate change (`sudo dmesg | tail`).
   - While audio plays, the stream is at 96000: `pw-metadata -n settings 0 clock.rate`, or
     `cat /proc/asound/card*/pcm0p/sub0/hw_params`.
   - RATE reads 96 kHz.
   - Then pick 48 kHz from Settings › HARDWARE › SAMPLE RATE.
   - If the driver refuses a rate, RATE must return to the previous rate within about a
     second, and the log shows `SET /SampleRate/value = … refused`.
4. **Clock source.**
   - Pick S/PDIF from CLOCK with nothing connected to S/PDIF in. The log shows
     `ClockSource → S/PDIF (value=0)`.
   - The CLOCK item gets a blinking amber outline, the x4 shows no lock, and the audio may
     mute or drift.
   - Pick INTERNAL: the outline stops.
5. **Preamp, input 1** (no condenser mic needed).
   - **+48V:** the front-panel 48V LED comes on and the log shows
     `preamp.48V … paramID=0x03`. Then turn it off again.
   - **PAD, LOW CUT and Ø:** their LEDs or behaviour change.
   - **Gain knob:** the front-panel gain ring and the readout move.
   - **MIC → LINE:** the front panel shows LINE.
   - **Hi-Z:** plug an instrument cable into the front Hi-Z jack. The source button shows HI-Z,
     and the SOURCE menu shows Hi-Z ticked.
6. **Mute and fader.** Input 1 starts muted (M lit) with nothing heard.
   - Move its fader while muted: still nothing heard (this was the blip/unmute bug).
   - Unmute: the input is heard at the fader's level.
   - Solo input 2: input 1 goes silent. Then Clear Solos.
7. **Sends.**
   - Raise input 1's AUX 1 send, toggle its PRE: the level on AUX 1 changes by the fader's
     amount.
   - On the AUX 1 strip, toggle PRE / POST: every input's AUX 1 send follows.
   - Toggle the send's M off and on: the send comes back at its level, not silent.
   - On input 1's fader, right-click › COPY MIX TO › CUE 1: every input's CUE 1 send jumps to its
     fader level, and the CUE 1 mix in the headphones matches the monitor mix.
8. **Monitor.** Check each of these:
   - MONITOR knob
   - MUTE and MONO
   - DIM, and the DIM amount knob in CTRL ROOM
   - SOURCE CUE 1 (the monitors play CUE 1)
   - HP 1 source CUE 2. Choosing MON for a headphone is refused, and the console goes back to
     the previous source.
9. **CUE BUS COUNT 4** (Settings › HARDWARE).
   - CUE 3 and CUE 4 appear in the sends and in CUE OUTPUTS.
   - The log shows `CueBusCount = 4 → enable bus 5` and `bus 6`.
   - Set it back to 2.
10. **Device OPTIONS.**
    - IDENTIFY flashes the x4 red and green.
    - DIGITAL OUTPUT ADAT, then back to S/PDIF.
    - OUTPUT REFERENCE LEVELS 1-2 −10 dBV, then +4 dBu.
    - DIGITAL MIRROR ON, then OFF.
    - CLIP HOLD and PEAK HOLD change how long the front-panel meters hold.
11. **Read-only launch.** Start `uaconsole` (no `--writes`) and press MUTE: nothing
    happens on the Apollo. Turn on Settings › WRITES and press MUTE again: it mutes.
