# Helping with DSP plug-in mapping and loading

open-apollo can run UAD plug-ins on the Apollo's DSP from Linux (experimental, off by default;
[install guide](install.md#running-plug-ins-on-the-apollo-experimental)). It ships no plug-in
data: every machine builds its own control maps from its own UAD install. A plug-in is offered in
the Console only after its maps passed a live check on real hardware
(`mixer-engine/device_maps/native_known_good.json`, 119 plug-ins on an Apollo x4 at 48 kHz). This
page is for contributors who want to grow that list: by testing plug-ins, by improving the emulator
and the mapper so more plug-ins map and load, or by making loading need less of UA's code. All
three are welcome.

How it works in depth: [NATIVE-HOSTING.md](../mixer-engine/NATIVE-HOSTING.md) (daemon side) and
[plugin-mapper's README](../tools/plugin-mapper/README.md) (map building).

> **Nothing you build here goes into the repository.** Bundles, `*.defaults.json`, DLLs, captures,
> disassembly and WAV recordings are made from UA's code or your audio and stay on your machine.
> `python3 tools/check-clean-repo.py` refuses them (rule `plugin-bundle`). What a PR can carry is
> the known-good entry (names, layouts, latency, DSP-owned word positions), code, and tests with
> synthetic I/O.

## What you need

| Need | Why | Check |
|---|---|---|
| A Thunderbolt Apollo. Only the x4 is verified; other models are welcome as new results. USB Apollos have no plug-in host yet. | Plug-ins run on its DSP. | `lspci -d 1a00:` |
| The driver with `plugin_host=1 input_strips=1` | Turns on the kernel INSTANCE API and the plain input strips chains replace. | `cat /sys/module/ua_apollo/parameters/plugin_host` |
| A UAD install you own, on Windows or copied ([uad-install.md](uad-install.md)). UAD **11.8.3** for the DSPAudioProcessor, VST-main, ParamObj and Sonnox families; the others work from any version. | The mapper runs the Powered plug-in DLLs and `UAD2SDK.dll`; the DSP modules come from it too. | `--from` points at its root |
| The plug-in authorized on your Apollo (bought, or a running demo) | The device refuses to allocate anything else, and the mapper skips it. | `tools/plugin-host-check.py query "UAD Pultec EQP-1A"` |
| What setup installs: DSP modules (`oa-assets dsp`), panel specs (`scripts/install-panels.sh`), step-list texts (`oa-assets plugins`) | Parameter names, steps and defaults come from the panel specs; the modules are what gets uploaded. | `oa-assets status` |
| The plug-in venv: numpy, unicorn, capstone, pefile | Both the mapper and the daemon run the emulator. | `~/.local/share/open-apollo/plugin-venv/bin/python3 -c 'import unicorn, capstone, pefile, numpy'` |
| The mixer daemon with `--native-plugin-bundles` and `uaconsole` | Loading, controls and readback go through them. | `journalctl --user -u ua-mixer-daemon` |
| For signal checks: a source on an input (a mic, a synth, a line from another interface) and `arecord`, PipeWire stopped | ALSA capture of the input and the MON L/R record channels. | `arecord -l` lists the Apollo |
| 48 kHz | The only rate mapped and verified. | Console > Settings |

The easy way to all of it is setup with **Plug-ins on the Apollo (experimental)** ticked, or
`sudo bash scripts/install.sh --plugin-hosting=/path/to/windows`. `tools/plugin-host-check.py`
opens `/dev/ua_apollo0`: run it yourself, attended, never from CI or an agent.

## The pipeline, and the tool for each stage

```
UAD install ──► plugin-mapper ──► runtime bundle ──► daemon load ──► knob change ──► DSP words ──► readback ──► audio
               (emulator, offline)  (per layout)    (CREATE recipe)  (emulator, live)   (driver)    (driver)    (arecord)
```

### 1. Map: run the plug-in offline and record its commands

```bash
cd tools/plugin-mapper
~/.local/share/open-apollo/plugin-venv/bin/python3 -m plugin_mapper --from /path/to/windows --only "UAD Pultec EQP-1A"
```

The mapper loads each DLL into **Unicorn** (an x86-64 CPU emulator: no Wine, no device, no
network), finds the plug-in's parameter block and descriptor, constructs it against your
`UAD2SDK.dll`, loads its Console defaults and sweeps every control at 0, middle and maximum,
recording the DSP words each change writes and checking each restore puts the words back. It does
this twice, as a mono and as a stereo host. Output in `~/.local/share/open-apollo-plugin-maps`:

- `<plug-in>-mono-runtime-bundle.json` (and `-stereo-` for 2-in/2-out descriptors): the descriptor,
  startup words, the control sweep (`control_sweep[].samples[]`: integer, formatted text, events),
  parameter metadata and the executed profile.
- `index.json`: what was built, `failures` with a reason per layout (for example
  `restore changed original captured integer vector`), `mono_only`, `waiting_for_licence`.

Useful flags: `--all` maps every installed plug-in with a panel spec, not only the known-good list
(how a new plug-in gets its first map); `--force` maps again; `--retry-failures` retries a failure
with unchanged inputs; `--work DIR` keeps scratch you can inspect. A failure reason is the most
useful thing to report for a plug-in that does not map.

### 2. Load: the CREATE recipe and the load-time state

The daemon (`profiled_bundle_registry.py`) offers each bundle as a mono slot per input and a
stereo slot per linked pair; `native_carrier.py` builds the recipe (chain head, Mic Input Strip,
the plug-in's rows and startup words). Before routing switches, the driver writes the recipe and
**reads it back**; a word that comes back different fails the load, not hosting. Words the DSP
itself owns (meters, envelopes) are listed per plug-in as `dynamic_refs` in the known-good entry
so they are not verified: finding them is part of qualifying a plug-in.

The parameter values and texts shown at load come from `<bundle>.defaults.json`, learned the first
time the plug-in's emulator starts. To learn them ahead of time:

```bash
~/.local/share/open-apollo/plugin-venv/bin/python3 mixer-engine/native_profiled_carrier_provider.py ~/.local/share/open-apollo-plugin-maps/*-runtime-bundle.json
```

By default only the layouts in `native_known_good.json` are offered; start the daemon with
`--advertise-all-plugins` to try a plug-in that is not in it yet.

### 3. Control: every parameter, with readback

Turn each knob in `uaconsole`, or drive one from a script (the daemon listens on 4710 for
`/dev/ua_apollo0`; insert `K` on input `N`, parameter `I`):

```bash
python3 tools/oa-daemon-client.py --port 4710 set /devices/0/inputs/0/effects/0/parameters/3/NormalizedValue 0.5
python3 tools/oa-daemon-client.py --port 4710 watch /devices/0/inputs/0/effects/0/parameters/3/StringValue
```

Each change becomes a plan: the writes, the exact words to read back, the display text, the
proposed state. The state is accepted only when the hardware readback matches, so a refused value
shows up as an error in `journalctl --user -u ua-mixer-daemon`, never as a silently wrong knob.
Check that the text matches what UA's Console shows for the same position (the text comes from
the plug-in's own formatter) and that stepped controls list every step.

### 4. Signal: does the audio do what the knob says

Readback proves the DSP holds the words; only audio proves they are the right words. Stop
PipeWire (`systemctl --user stop pipewire.socket pipewire wireplumber`), then record the input and
MON L/R together:

```bash
arecord -D hw:x4 -f S32_LE -r 48000 -c 22 -d 5 take.wav
```

(22 record channels on an x4; `/proc/asound/x4/io_map` lists them, MON L/R are IDs 0x0901/0x0902),
and compare takes with the plug-in bypassed, at defaults and with a control at its extremes (a
compressor's gain reduction, an EQ band's boost, a reverb's tail). `tools/apollo-wavcheck.py`
generates a test tone and finds which record channel carries it; `tools/rectap-bench.py` is a
worked example of a scripted bench (builds a 1176 chain with the daemon's own composer, records
input 1 against MON L/R at each step, writes WAVs and `results.json`). Keep recordings outside
the repository, and turn the monitors down first.

### 5. Report it

The status of every plug-in is [docs/status/PLUGINS.md](status/PLUGINS.md): one row per plug-in and
layout, one column per check (map, panel, load, controls, text, meters, latency, audio), and a list
of what fails under each check. It is generated from `docs/status/plugins/*.json`, which
`tools/plugin-qualify.py` writes from measurements, never by hand. The live gate behind
`native_known_good.json` (it loads, three controls read back, it unloads) is only where that
table starts: a plug-in **works** when every check passes.

```bash
python3 tools/plugin-qualify.py offline                                # map, panel: no Apollo
python3 tools/plugin-qualify.py live --device "Apollo x4"              # load, controls, text, meters
python3 tools/plugin-qualify.py live --device "Apollo x4" --signal --only "UAD LA-2A Legacy"
python3 tools/plugin-qualify.py mark "LA-2A Legacy" mono audio pass --device "Apollo x4"
python3 tools/plugin-qualify.py mark "LA-2A Legacy" mono latency fail --detail "measured 55, entry 0" --device "Apollo x4"
python3 tools/gen-status-graph.py                                      # redraw PLUGINS.md
```

`live` loads each plug-in the daemon offers into an empty insert slot (never one in use), sets
every parameter to the three positions the mapper swept, checks each is accepted and that its text
matches what the plug-in's own formatter gave there, reads the panel's meters (with `--signal`, a
source on that input, checks they move), restores each value and unloads. Run it attended with the
monitors down; add `--by <github handle>` and `--uad <version>` so your results say who and what.
Audio and latency are measured by you (step 4) and recorded with `mark`. A PR with the changed
JSON files and the regenerated `docs/status/` is the report; if you can't run the tool, use the
**Plug-in report** issue form. Otherwise open an issue (or a PR adding the `native_known_good.json` entry) with:

- the plug-in name, layout (mono / stereo) and input(s); Apollo model and sample rate;
- UAD version, the `mapper` and `uad_sdk_sha256` fields of `index.json`;
- each gate step: pass, or what happened (the daemon's journal lines, the `index.json` reason);
- its latency in samples if you measured it, and any `dynamic_refs` you had to exclude.

Not the bundle, its defaults, or recordings. Code changes come with tests in `mixer-engine/tests`
that use synthetic I/O (`fake_capture.py`, `plugin_test_support.py`; the mapper's build in
`test_plugin_mapper_build.py`, the workers in `test_emulator_worker.py`):

```bash
python3 -m pytest mixer-engine/tests -k "native or plugin or emulator"
```

## Loading with the emulator, and without it

The emulator is used in two places, and they are worth keeping apart.

**When mapping (always).** The mapper is the emulator: it is how a plug-in's DSP commands are
extracted at all. It runs once per plug-in per UAD version, offline, and its output is the bundle.

**When hosting (for every control change, unless the plug-in has tables, below).** The bundle holds three samples per control
(0, middle, maximum). Between them, the DSP words a knob position needs are computed by the
plug-in's own code: coefficient math, tables, coupled parameters. So the daemon keeps one emulator
per loaded plug-in and, on each change, runs the plug-in's own setter in it to translate the knob
value into DSP words, then sends and verifies those words. The emulators run in worker processes
(`emulator_worker.py`, `--emulator-workers N`, default 2, `0` in the daemon) because building one
takes seconds of Python; once built, a change costs about two round trips (~2.5 ms). This is why
hosting needs the venv and the DLLs copied to `open-apollo-plugin-maps/source`.

Loading itself already avoids the emulator when it can: the CREATE recipe and the observations are
static (from the bundle) and the load-time values come from `*.defaults.json`, so a plug-in loads
at once and its emulator warms in the background for the first knob turn.

**Without the emulator (tables).** A plug-in whose words depend on few controls at a time can be
controlled from tables learned from its emulator once, on this machine:

```bash
cd tools/plugin-mapper
~/.local/share/open-apollo/plugin-venv/bin/python3 -m plugin_mapper tables --only "UAD Pultec HLF-3C"
```

For each of its bundles (`plugin_mapper/tables.py`):

1. **Probe.** Every control is set from several states (the load state, every control at 0, at its
   maximum, random states) while one other control at a time changes. A change's events are a fixed
   list per control: transaction brackets and writes of word runs (blocks). The probe finds which
   controls each block's words, and each formatter text, depend on.
2. **Contexts.** A block's stepped dependencies are its key (every step). Per key, at most one
   continuous control may matter, recorded at every value 0..maximum (the API Preamp's gain block:
   Input Select picks Mic Gain or Line Gain). A block on two continuous controls at once, coupled
   controls, history-dependent words or a text on another continuous control: refused.
3. **Load-time state.** From a freshly loaded plug-in, setting a control to the value it already has
   may change another control's words: the plug-in's own copy of it was not in sync at load (the API
   Preamp's Output writes no gain until Power is set once). Such a control reads as "not set yet" in
   that block's key until it is set. This is learned per control, not known per plug-in.
4. **Record** every key, every text (as runs over the parameter's value), every control's read-back
   value, and how the begin/end transaction count moves (one global count, or one per start word).
5. **Gate.** One random sequence per control, each from a freshly loaded plug-in and starting with
   that control, 3000 changes in all (`--changes`): the emulator and the tables must give the same
   events (counters included), values and texts at every step, and the same texts at load. Only then
   is `<bundle>.tables` written; otherwise `index.json` `tables` records why.

The file is a JSON header and zlib-compressed 32-bit words (`executor/table_capture.py`, standard
library only), tied to the bundle's SHA-256. The daemon checks it when it first plans a control: a
current, passing file gives a `TableCapture` (the surface `GenericCapture` and `RemoteCapture`
have) and that plug-in needs no venv, DLL or emulator worker; anything else uses the emulator as
before. A rebuilt bundle needs `tables` run again. Like the bundle, the file is UA-derived: it
stays next to the bundle and `tools/check-clean-repo.py` refuses it (rule `plugin-bundle`).

Learning is quick for all-stepped plug-ins (Studio D Chorus, Pultec HLF-3C, bx_tuner: about 15 s
to a minute each); each continuous axis costs one emulator sweep of 32769 changes (about two
minutes) per key it is recorded at, so the API Preamp takes about 15 minutes and 7 MB per layout.
Readback cannot tell a wrong table from a right one, only the gate and audio can: the gate is the
reason a table is trusted, so a change to the learner keeps its differential test.

The other route, a plug-in's math reimplemented in the repository and checked word for word against
the emulator (the Pultec-Pro Legacy was shown to run that way, NATIVE-HOSTING.md), has to meet the
same rule as everything else (no UA data, nothing copied from their code): open an issue first.

## Improving the emulator side

Most plug-ins that do not map or do not load fail in the emulator, not on the Apollo. Work here
needs a UAD install to run against but no Apollo until the live gate, and it is the most direct way
to add plug-ins:

- **Fix a failure.** Start from the `index.json` reason, rerun with `--only NAME --retry-failures
  --work DIR` and read the scratch it leaves. The usual causes: a host callback or import the
  plug-in calls that `offline_host.py` does not provide (unknown host methods fail on purpose, so
  the failure names it), a C runtime or static initializer the executor does not run
  (`crt_init.py`, `cxx_domain_init.py`, `thread_safe_statics.py`), or a restore that does not put
  the words back.
- **Add a family.** Each family has a discovery profile (`discovery/*_profiles.py`) and an executor
  (`executor/*_capture.py`, `*_host.py`); `generic_capture.py` covers most plug-ins. Softube and
  other third-party families, Unison preamps and UADx-only plug-ins have none yet
  (`softube_capture.py` already runs the Softube engine's VST2 startup; its plug-ins do not map yet).
- **Other UAD versions.** The DSPAudioProcessor, VST-main, ParamObj and Sonnox profiles pin
  addresses in UAD 11.8.3 and refuse other versions; finding those addresses from the DLL itself,
  as the other families do, lets more installs map them.
- **Stereo and coupled controls.** Plug-ins that map mono only (`mono_only`), or whose coupled
  parameters change more than the knob turned.
- **Speed.** Building an emulator parses the DLL and runs the constructor in pure Python: seconds
  per plug-in when mapping and on the first knob turn after a load. Faster builds (`pe_cache.py`,
  less work in the constructor path) help both the mapper and the daemon.
- **Other rates.** Everything is mapped and verified at 48 kHz.

An emulator change is good when the mapper builds the plug-in, its existing known-good siblings
still map to the same bundles (compare before and after with `--force`), and the live gate passes
on hardware, yours or the owner's: say in the PR which you could run.

## Where to start

- A plug-in you own that is not in `native_known_good.json`: map it with `--all --only NAME`, load it
  with `--advertise-all-plugins`, and run the gate.
- A known-good plug-in in the other layout (stereo where only mono passed), or on another Apollo model.
- A failure in `index.json`, or a family not mapped yet: see [Improving the emulator side](#improving-the-emulator-side).
