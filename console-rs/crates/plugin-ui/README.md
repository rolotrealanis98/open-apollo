# plugin-ui

Plug-in windows for console-rs. Tapping a loaded insert (or Unison) slot opens UAD Console's own
plug-in window, rebuilt from Rack.dll ([WINDOW.md](WINDOW.md)): the 22-pt title bar, the UAPW
toolbar (`Rack_NewPlugInHeader`) and the status footer, around the plug-in's editor. Edits go
to the UA Mixer Engine through ua-engine's gated writer.

The editor is, in order of preference:

1. **UA's own HTML/JS GUI** (a LUNA "lunacomponent", feature `webview`): hosted in WebKitGTK via
   wry, loaded as Console 1.3.1 loads it (`ua-content://gui/index.html?host=…&port=…&path=…&plugin_id=…`),
   and answered by our UANet bridge, a local WebSocket speaking the GUI's dialect over the
   engine's `parameters/<k>` ([WEBBRIDGE.md](WEBBRIDGE.md)).
2. **A ConsoleLink panel spec** (`PluginSpec_*.json` + `Images/*@2x.png`), drawn by
   [`PluginPanel`](src/panel.rs) into the frame's `rack::DrawList`.
3. **The generic parameter list** ([`GenericPanel`](src/generic.rs)).

No UA asset is in this repository: GUIs, specs, sprites and Console art are read at runtime.

| module | what |
|---|---|
| `spec`, `library` | spec types (all 208 of ConsoleLink's decode, boolean table frames included) and the spec folder index |
| `panel`, `generic` | spec panel and parameter list: draw + hit regions + `apply(Action)` |
| `window` | Console's window chrome and layout (`header_layout` = Rack.dll's mode 0/1/2 + overflow) |
| `params` | `ParamSource`, `MockParams`, `ParamSnapshot` |
| `engine` | `EngineSlot`: `ParamSource` over `ua_engine::Store` |
| `meters` | VendorSpecific meter queries, decoding, calibration, ballistics |
| `web`, `uanet` | lunacomponent discovery, the page URL and its keys (`GuiQuery`), and the UANet dialect (pure) |
| `content` | the `ua-content://` scheme: Console 1.3.1's path resolution, MIME table and headers (pure; WEBBRIDGE.md §8) |
| `bridge` (feature) | the WebSocket endpoint (tungstenite, a thread per connection) |
| `webview` (feature) | wry/WebKitGTK window, `ua-content://` handler (or `file://`, the 1.2.x layout), snapshots |

## Where things are found

| What | Search order |
|---|---|
| Panel specs | first of `$UACONSOLE_PLUGIN_SPECS`, `~/.local/share/open-apollo/plugin-specs`, `~/wine-ua-work/plugin-specs/integrate/stage/Console Link/Console Link/PluginSpecs`, `~/wine-ua-work/plugin-specs-local` |
| Lunacomponents | every existing folder of `$UACONSOLE_LUNA_COMPONENTS`, `~/.local/share/open-apollo/lunacomponents`, `/Library/Application Support/Universal Audio/Plug-Ins` (macOS), `~/wine-ua-work/lunacomponents` |
| Console skins, `images/uapw` | houston's `Resources` discovery (`$CONSOLE_RESOURCES`, the Wine install, …) |

The Windows Console in the Wine prefix ships no lunacomponents (`Program Files/Universal Audio`
has only `Powered Plugins` and a stub `UAMixerEngine.exe`). Copy them from a Mac
(`/Library/Application Support/Universal Audio/Plug-Ins/*.lunacomponent`; `gui.bundle` and
`algo.bundle/Contents/Resources` are enough) into `~/.local/share/open-apollo/lunacomponents`.
Specs come from ConsoleLink's `PluginSpecs` folder.

## App integration (uaconsole)

`uaconsole/src/plugin.rs` implements the overlay `PluginHook`:

- `on_open_plugin(strip, slot, model)` opens a window for a **loaded** slot (empty slots open
  nothing). The title is `"<channel>, Insert n"` or `", Unison"` (slot 255). Strip ids are engine
  base paths; `inN`/`auxN` also work (`--open-plugin in0:1`).
- The slot comes from `EngineMixer::insert_slot` via `MixerModel::as_any()`. Without an engine
  (preview data) the window edits local values.
- `Panel::sync` (called before every build) opens the `EngineSlot` (read-only refresh +
  retained subscriptions), re-reads values into a `ParamSnapshot` and the chrome's power/preset.
  `pointer` routes through `rack::Interaction` and `PluginWindow::apply`; closing releases the
  subscriptions. Power writes `<slot>/Power`. Windows are non-modal, like Console's.
- `cargo build -p uaconsole --features webview` hosts UA's GUIs. Each web window gets its own
  bridge on an ephemeral 127.0.0.1 port, serviced at 30 Hz from `sync` (the panel is `live`).

**Where the webview goes.** The GUI is a separate GTK top-level window, not part of the wgpu
surface. Wayland clients cannot position top-level windows and wgpu and GTK cannot share one
window portably, so the compositor places it. Our chrome stays in the mixer overlay, and its
editor area marks where the GUI belongs. Better options for later: on X11/XWayland, use wry's
`build_as_child` on the winit window at `WindowLayout::editor`; or make the whole plug-in window
a GTK window, with the chrome rendered by `gfx` offscreen above and below the webview.

## `ParamSource`: the engine contract

[`ParamSource`](src/params.rs) is one slot's parameters by ordinal `k`. [`EngineSlot`](src/engine.rs)
implements it over ua-engine; `<slot>` is `/devices/D/inputs/N/effects/K`, an aux's
`…/auxs/N/effects/K`, or Unison `/devices/D/inputs/N/preamps/0/effects/0`.

| Method | Engine | Notes |
|---|---|---|
| `count()` / `indices()` | children of `<slot>/parameters` | present only while a plug-in is loaded. `k` is the plug-in's ordinal and also the spec's `param` index (the snapshot's Vitalizer names match the spec index for index) |
| `name(k)` | `<slot>/parameters/<k>/Name` | |
| `normalized(k)` | `…/NormalizedValue` | 0…1; None when `k` is not published (panels keep a local value) |
| `display(k)` | `…/StringValue` | falls back to the step label |
| `steps(k)` | `…/StepValue` `values` | step `i` of `n` is `i/(n−1)` |
| `set(k, v)` | `set …/NormalizedValue/value v` | coalesced by the writer; the local value moves at once |
| `begin/end_gesture()` | `/UndoRecording` true … false | nested; only the outermost pair is sent |
| `commit(k, v)` | begin, set, end | one undo step |

Open: `get <slot>?recursive=1` (read-only), then subscribe `NormalizedValue`, `StringValue`,
`StepValue` of each parameter; release on close. Writes obey the store's gate (read-only by
default: nothing is sent). Meters: `MeterFeed` builds `VendorSpecific/value?floats=…` and
`?opcode=519…` queries, decodes replies and smooths needles into `PluginPanel::meters`.

Hit regions use `rack::Control` variants appended for plug-in windows: `PluginParam(k)` (drag),
`PluginElement(i)` (spec element tap), `PluginStep(k, s)` (step tap), and
`PluginWindow(PluginWindowButton)` for the chrome (close, assign, preset prev/next, presets,
sidechain, power, in/out, A/B compare, copy/paste, mixer, more, footer buttons, favourite, help).

## Examples

```
cargo run -p plugin-ui --example render -- ua_1176_rev_a out.png                 # spec panel at defaults + out.zero.png
cargo run -p plugin-ui --example render -- ua_1176_rev_a out.png --window --status "NO UAD-2 DEVICES FOUND"
cargo run -p plugin-ui --features webview --example web -- ua_1176_rev_a --snapshot gui.png [--zero] [--compare ref.png] [--file]
cargo run -p uaconsole -- --render out.png --open-plugin in2:1                     # the app, preview data, dbx 160 open
cargo run -p uaconsole -- --render out.png --mock …/apollo-x4-12.0.0.snapshot.json --open-plugin in0:1
```

Checks against the spec pipeline's LUNA references (all parameters 0, >24/255):

| what | pixels off |
|---|---|
| spec panel: dbx 160, UA 1176 Rev A, LA-2A Silver | 0.000% |
| UA's 1176 Rev A HTML GUI in our webview, fed by our bridge, on `ua-content://` and on `file://` | 0.000% |
| header layout vs the 1.3.1 capture (SSL G, 304×710) | every button edge to the point |

## Tests

`cargo test -p plugin-ui` (add `--features webview` for the bridge transport tests): spec parsing
(inline spec covering every element kind, plus every spec in the found folder), hit rects and
actions through `rack::Interaction`, stepped quantisation, local values, `MockParams`, the list,
meters, slot paths, lunacomponent matching, Console's header layout, the engine adapter against
the recorded x4 snapshot (gated writes, subscriptions), and the UANet dialect (parsing, subtree,
pushes, gated writes, touch gestures, step indices), the `ua-content` resolver (traversal,
allowed roots, headers) and the page keys as the GUI's two URL parsers read them. `cargo test -p uaconsole plugin` covers the
hook on engine and preview models.

## Gaps

The integrated ConsoleLink development catalog was checked on 2026-10-12:
223 specs decode, their referenced panel sprites exist, and each opens/builds
a Rust window. This validates loading, not complete Swift runtime parity or
Linux DSP processing. Combined meters, integer/absolute/hold conditioning,
and keyed action tables are now supported. The audit found 75 specs using
additional Swift features still absent in Rust: local UI states, state/meter
table keys, graphs, sprite-font/meter readouts, touch/held/blink/segment looks,
swipe/long-press gestures, and copy/random/snapshot actions. See
[the investigation checkpoint](../../PLUGIN-STATUS.md) for evidence and limits.

- **Web GUIs in the app** are a separate top-level window (above). The full path works in the
  `web` example; the app path builds and is wired but has not been run against a live engine.
- **Control id maps** come from the bundle's `ua-param` ids, joined to engine names by
  normalised name or prefix. That is exact for the 1176; other GUIs may need overrides
  (`ControlMap::build`'s map). Meters reach the GUI only as subtree nodes: VendorSpecific
  polling into `meter_push` is not wired yet.
- **Chrome**: preset browser, insert menu and "more" menu are stubs; preset prev/next,
  copy/paste, A/B and the footer buttons emit `WindowEvent::Button` for the shell but do nothing
  yet. Visibility predicates (sidechain, A/B, footer) are [unk] in WINDOW.md; the defaults
  reproduce the capture. Close is on the left (macOS, as captured); `close_on_left = false` gives
  the Windows layout.
- **Spec panels**: horizontal faders reuse rack's vertical drag with a quarter span; menus cycle
  on tap; momentary buttons show no pressed frame.
- Not written: typed values, StepValue label sets, presets.
