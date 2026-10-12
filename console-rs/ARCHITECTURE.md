# console-rs: a native UAD Console for open-apollo

A Rust rebuild of UA's UAD Console mixer that runs on Linux (where the Windows Console
cannot render under Wine), macOS and, later, iPad. It talks to a UA Mixer Engine on TCP
4710: either the open-apollo daemon (`mixer-engine/ua_mixer_daemon.py`) or the real engine.

## UA assets

UA's art, layouts and Appearance.json are **never** in this repository. They are loaded
at runtime from the user's installed UAD Console:

1. `$CONSOLE_RESOURCES`
2. the Wine install, `$WINEPREFIX` (default `~/wine-ua`) + `drive_c/Program Files (x86)/Universal Audio/UAD Console/Resources`
3. `/Applications/Universal Audio/UAD Console.app/Contents/Resources` (macOS)
4. `~/.local/share/open-apollo/console-resources` (a copied Resources folder)

Plug-in icons come from `Resources/Plug-In Icons` (Windows) or
`/Library/Application Support/Universal Audio/Plug-In Icons` (macOS).

## Crates

| crate | role | depends on |
|---|---|---|
| `ua-engine` | Engine protocol and model: NUL-framed JSON over TCP (tokio), path tree, per-strip topology, tapers, lock-free meters, gated coalescing writer, snapshots. A port of ConsoleLab's UAEngineKit. | — |
| `houston` | Skin and layout runtime (UA's "houston" JUCE layer): resource discovery, Appearance.json (lenient JSON, parent inheritance, colours), layouts (box_model / flex_box), PNG decoding and the image cache, plug-in icons. A port of ConsoleLab's HoustonKit. | — |
| `rack` | Render-agnostic Console UI: geometry recovered from Rack.dll (strip, cells, fader & meters, monitor column, window), Rack components, `MixerModel` trait, and **a `DrawList` per frame** plus hit regions and input handling. Pure CPU, unit-testable. | `houston` |
| `gfx` | wgpu renderer for a `DrawList`: lazily uploaded textures (one per image, filmstrip frames as sub-rects), instanced textured quads, 9-slice, fills, glyphon text; render to a window surface or offscreen to PNG. | `rack`, `houston` |
| `uaconsole` | Binary: winit window, engine connection, `MixerModel` adapter over `ua-engine`, input routing. CLI: `uaconsole --render out.png [--snapshot f.json]` for headless renders. | all |

Frame pipeline: engine pushes → `ua-engine::Store` (main-thread owned) and `MeterStore`
(atomics, written by the network task) → `rack` builds the whole window's `DrawList`
(≈2–3k quads, microseconds) → `gfx` draws it in a few draw calls. Redraw happens on engine
change, on input, and at 30 Hz while meters are active.

## Reference material (outside the repo)

- Geometry and skin bindings: `~/wine-ua-work/console-port/rack/mixer-geometry.md`, `layout-system.md`
- Protocol: `~/wine-ua-work/console-port/protocol/*`
- Swift reference implementation (ConsoleLab): `~/github/ConsoleLab/Packages/ConsoleLabKit/Sources/*`
- Reference screenshots and diff tools: `~/wine-ua-work/console-port/refshots/`, `~/github/ConsoleLab/tools/diff.py`

## Safety

The engine connection is read-only by default: `set`/`post` are refused unless writes
are explicitly enabled. Tests that write use a software-only daemon on a spare port, never
a daemon or engine driving real hardware.
