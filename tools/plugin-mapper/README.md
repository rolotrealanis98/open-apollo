# plugin-mapper

Builds the runtime bundles the mixer daemon hosts UA plug-ins with
(`mixer-engine/NATIVE-HOSTING.md`), on the user's machine, from their own UAD install. Setup runs
it when plug-in hosting is ticked (`scripts/setup-plugin-hosting.sh`); by hand:

```sh
python3 -m venv --system-site-packages ~/.local/share/open-apollo/plugin-venv
~/.local/share/open-apollo/plugin-venv/bin/pip install -r requirements.txt
cd tools/plugin-mapper
~/.local/share/open-apollo/plugin-venv/bin/python3 -m plugin_mapper --from /path/to/windows
```

`--from` is the Windows install root. It also needs what setup installed: the DSP modules
(`oa-assets dsp`, `/lib/firmware/universal-audio/plugins`), the panel specs
(`scripts/install-panels.sh`) and, for a few plug-ins whose step lists are preset names, the
texts `oa-assets plugins` imports. Options: `--all` (every installed plug-in with a panel spec,
not only `mixer-engine/device_maps/native_known_good.json`), `--only NAME` (repeatable),
`--workers N` (default: CPU cores - 1, or `OA_PLUGIN_MAP_WORKERS`), `--out DIR` (default
`~/.local/share/open-apollo-plugin-maps`), `--work DIR` (scratch, default
`~/.cache/open-apollo/plugin-mapper`; delete it freely), `--authorized auto|all|FILE` (default
`auto`: only what the Apollo authorizes, asked of the running daemon or else the device; with no
answer every selected plug-in), `--markers` (`::progress` lines for the setup wizard), `--force`
and `--retry-failures`.

`python3 -m plugin_mapper stage --from ROOT --to DIR` copies what mapping reads (the known-good
plug-in DLLs, `UAD2SDK.dll`, the few data files plug-ins open) into DIR, laid out like the
install; setup maps from that copy so mapping can go on after the Windows partition is closed.

Plug-ins are mapped one per worker, each line on stdout as it starts and ends (`Mapping 12/37:
UAD Pultec EQP-1A (mono, stereo)…`, then `done 41.0 s · ~6 min left`), with `progress.json` and
`index.json` rewritten after each one. A plug-in's maps are reused until its DLL, `UAD2SDK.dll`,
the mapper (`MAPPER_VERSION`) or its known-good entry changes; a plug-in that failed with the
same inputs is not tried again unless `--retry-failures`.

Nothing here loads a DLL into a process or talks to a device or the network: every plug-in
runs inside Unicorn, against the user's own `UAD2SDK.dll`. The output is made from UA's code and
stays on that machine; `tools/check-clean-repo.py` refuses it in the repository.

## How it works

| Stage | Code | What it does |
|---|---|---|
| inventory | `inventory.py` | RTTI and descriptor-producer candidates per DLL (static) |
| discovery | `discovery/run_catalog.py`, `discovery/stages/` | CHyp/JUCE plug-ins: parameter block, program loader, CRT/host context, parent binding, formatter ABI (one worker process per plug-in, static) |
| discovery | `discovery/*_profiles.py` | the hand-profiled families (DSPAudioProcessor, VST2 IUADPlug, BX, relab, VST-main, ParamObj, Sonnox) |
| execution | `execution/run_catalog.py`, `executor/` | constructs each plug-in, loads its Console defaults, reads the DSP descriptor, sweeps every control and checks each restore |
| bundle | `carrier/build_bundle.py` | descriptor, startup words, control sweep, parameter metadata and the executed profile |

Parameter names, step labels and Console defaults come from the panel specs, which match the
Console's own values for every live plug-in. Without a Console capture the program loader is
the authority on the default vector it accepts. Latency is not in the plug-in's code path the
executor runs; it comes from the known-good list (0 and `latency_source: unknown` otherwise).

`executor/win32-ctype.json` is Wine's Win32 character-type tables (from Wine 11.19's
`sortdefault.nls`, LGPL; `executor/build_win32_tables.py` rebuilds it), which the plug-ins' C
runtime reads.

## Limits

- The DSPAudioProcessor, VST-main, ParamObj and Sonnox profiles pin addresses in the DLLs of
  UAD 11.8.3 and refuse other versions. The others are found from the DLL itself; a different
  `UAD2SDK.dll` is used but recorded (`index.json`, `uad_sdk_sha256`).
- Softube and other third-party families, Unison preamps and UADx-only plug-ins are not mapped.
- 48 kHz only.
