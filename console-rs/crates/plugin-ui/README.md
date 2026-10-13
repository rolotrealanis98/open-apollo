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
| `spec`, `library` | the full spec contract as typed, `Serialize`-able fields (PIPELINE.md's base schema and every runtime addition; all 223 staged specs decode with no key dropped) and the spec folder index |
| `panel`, `generic` | spec panel (a port of the Swift runtime: values, UI states, actions, frames, visibility, readouts, modifiers, gestures, pop-ups, the "more" panel) and the parameter list: draw + hit regions + `apply(Action)` / own pointer gestures |
| `graph` | GraphValue evaluation, laws and their inverses, analog EQ/pass/shelf magnitudes, dynamics transfer, axes, and the graph scene (EQ, polar, transfer, spectrum, polylines, ranges, peak marker) |
| `raster` | tiny-skia rasters of graph scenes, rotated (`rotate`/`spin`) sprites and the "more" button, stored as `runtime:` images in houston's image cache |
| `readout` | `String(format:)` for one %f/%g/%e conversion, sprite-font cells, text fitting and wrapping |
| `window` | Console's window chrome and layout (`header_layout` = Rack.dll's mode 0/1/2 + overflow) |
| `params` | `ParamSource`, `MockParams`, `ParamSnapshot` |
| `engine` | `EngineSlot`: `ParamSource` over `ua_engine::Store`; `MeterPoller`: the panel's read-only meter gets |
| `meters` | VendorSpecific polling (floats= list + one opcode range incl. analyser bins), reply decoding, conditioning (integer bits, absolute, ignoreZero, derive direction), combine max, calibration and mode curves, ballistics, the analyser block |
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

Spec sprites are `Images/<sprite>@2x.png` (or `<sprite>.png`) next to the specs, resolved as
`spec:Images/…` keys through the image cache's `spec` root (`PluginSpecLibrary::attach`).
What the panel draws in code (graph curves, rotated sprites, the "more" button) is rasterised in
memory under `runtime:` keys; nothing is written to disk.

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
  `pointer` gives presses that land on a spec panel (not on chrome or a popover) to the panel's
  own gestures (`PluginWindow::panel_pointer_down/move/up`); everything else routes through
  `rack::Interaction` and `PluginWindow::apply`. `sync` ticks the panel's clock (meter ballistics,
  touch-state timeouts, long presses, repeats) and the window stays `live` while the panel
  animates. Opening passes the strip's stereo flag (`set_channel_layout`: state `channels`,
  `monoSize`). Closing releases the subscriptions. Power writes `<slot>/Power`. Windows are
  non-modal, like Console's.
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
default: nothing is sent). Meters: `PluginPanel::meter_queries` gives the read-only gets
(`<slot>/VendorSpecific/value?floats=ID,…` and one `?opcode=OP&p0=ID&p1=N&p2=1`),
[`MeterPoller`](src/engine.rs) sends them over the store's live connection (one in flight per
query, every 1/30 s, a second's back-off after an error) from the app's `sync`, and
`PluginPanel::ingest_reply` feeds the replies into `PluginPanel::meters` (`MeterFeed`).

Hit regions use `rack::Control` variants appended for plug-in windows: `PluginParam(k)` (drag),
`PluginElement(i)` (spec element tap; `u16::MAX` = the "more" button, `MAX−1` = a pop-up's
backdrop), `PluginStep(k, s)` (step tap: segments, menu rows, "more" steps), and
`PluginWindow(PluginWindowButton)` for the chrome (close, assign, preset prev/next, presets,
sidechain, power, in/out, A/B compare, copy/paste, mixer, more, footer buttons, favourite, help).

## Spec runtime

[`PluginPanel`](src/panel.rs) is a port of the validated Swift runtime (`PluginSpec.swift`,
`SpecPluginViewModel.swift`, `SpecPluginView.swift` in `~/wine-ua-work/plugin-specs/runtime`),
against the contract in `~/wine-ua-work/plugin-specs/pipeline/PIPELINE.md`. Where the two
disagree the Swift behaviour is kept.

| Area | Spec keys | Runtime |
|---|---|---|
| values | `parameters`, `default`, `steps` | the source's value, else a local value (spec default); readouts use Console's `StringValue`, else the step name, else a percentage |
| UI states | `states`, `setState`, `cycleState`, `touchState` (`timeout`, `fallback`, `hold`) | local, never sent; timeouts run on the panel clock; `channels` = 1/2 from the strip, `monoSize` on mono |
| actions | `set`, `nudge` (+`wrap`), `toggle`, `default`, `table` (`current` or `keys`), `multi`, `copy`, `random`, `snapshot` (`switch`/`store`/`recall`), `resend` | Swift's `perform`; the writes of one action are one undo step |
| frames | `frameSource` `param`/`table` (keys = params, `states`, `meters`)/`meter` (+`threshold`)/`blink`, `blink`, `reverse`, `pressedFrameOffset` | knobs show their own parameter (SpriteKnob); switches over their resting frames, held look while pressed; buttons frame 1 while pressed |
| visibility | `opacity`, `visibility` (params, `states`, `meters`), `visibleWhen` | nearest row (L1); hidden elements (≤ 0.01) take no touches |
| readouts | `text` style, `lines`, `string`, `select`, `value` + `format`, `names`, `meter` + `db`/`scale`/`offset`/`min`/`max`/`floor`/`floorText`, `spriteFont` (`charset`, `cell`, `align`, `decimalSprite`) | system text shrinks to fit (down to half) or wraps; LED/nixie strings from filmstrip cells |
| modifiers | `move`, `reveal`, `rotate`, `spin` | shift, clip in the unshifted frame, rotation about the centre (rasterised) |
| gestures | knob `drag` (vertical/horizontal, 0.005/pt), `tapAction`, zone `drag`/`swipe` (`axis`, `distance`), switch `segments`/`orientation`, `longPressAction` (0.45 s), button `repeat`, menus, graph handles (`drag`, `touchState`) | the panel's own recogniser (`pointer_down/move/up`, `tick`); also the `rack` hit route without long presses, repeats or handles |
| graphs | `graph` `eq`/`polar`/`transfer`: `bands` (filters, `filterParam`, `qMode`, `shelfQ`/`shelfOrder`/`shelfShift`, `shape`), `curves` (`groups`, `gain`, `fillToGain`, `hideBelow`, …), `handles`, `polar` + `traces` (patterns, `lobe`, `arc`, `mirror`) + `markers`, transfer `chain`, `ranges` (`yValue`), `spectrum` (`hz`, `reverse`, `fp16x2`, `fillGradient`, `smooth`), `peakMarker`, `polylines` | Swift's math and scene order; drawn into code rasters (`raster`), labels as text |
| GraphValue | number, `param`+`law` (+`scale`/`offset`), `params`+`table`, `sum`, `product`, `select`, `min`, `max`, `ratio`, `apply`, `meter`+`db` | Swift's precedence; `min`/`max` only as lists; inverse for handle drags |
| meters | `meterSources`: `vendorSpecificFloat`, `opcode`, `integer`, `absolute`, `ignoreZero`, `derive`, `combine`, `calibration`, `modeParam`/`modeCalibrations`, `smoothing` | one floats= query + one opcode range (analyser bins merged); smoothing ≤ 0 or ≥ 1 is instant |
| more panel | `panelless` and any parameter no element binds | a button at the top right opens rows with a slider or step segments |

Deliberate differences from Swift (each where Swift misbehaves or the spec asks for more):
table frames written as `true`/`false` decode as 1/0 (Swift would reject the spec); `nudge`
honours `wrap` (agent H's cycle buttons; Swift clamps); `qMode: "3dB"` bells under 3 dB of
gain use RBJ Q (Swift's `max(NaN, 1)` makes the whole curve NaN); fills close the curve and its
baseline as one polygon (Swift's `addLines` starts a new subpath, so shelves fill only to their
chord); a knob's double tap resets it; stepped parameters are quantised on drags; GraphValue
`meter` sources also resolve in `rotate`/`spin` (Swift evaluates those without its meter lookup;
no spec uses meters there). `frameSource.channel` and the top-level `meters` list are decoded or
ignored as information only, as in Swift.

## Examples

```
cargo run -p plugin-ui --example render -- ua_1176_rev_a out.png                 # spec panel at defaults + out.zero.png
cargo run -p plugin-ui --example render -- oxford_supresser_ds out.png --meters 0.6  # mock meters and analyser bins
cargo run -p plugin-ui --example sweeps [-- polar_sphere] [--every 2] [--out dir]  # parameter sweeps vs the live AU editor
cargo run -p plugin-ui --example render -- ua_1176_rev_a out.png --window --status "NO UAD-2 DEVICES FOUND"
cargo run -p plugin-ui --features webview --example web -- ua_1176_rev_a --snapshot gui.png [--zero] [--compare ref.png] [--file]
cargo run -p uaconsole -- --render out.png --open-plugin in2:1                     # the app, preview data, dbx 160 open
cargo run -p uaconsole -- --render out.png --mock …/apollo-x4-12.0.0.snapshot.json --open-plugin in0:1
```

The render example draws the live AU snapshots' state when agent F's parameter dumps are found
(`$PLUGIN_UI_LIVE_REFS` or `~/wine-ua-work/plugin-specs/agents/F`: AU values and display strings,
meters reading 0) and compares with `reference/<stem>.png` (defaults) and
`reference_zero/<stem>.png` (all minimum), and the zero render with the LUNA reference.

Checks (pixels whose largest channel differs by more than 24/255):

| what | pixels off |
|---|---|
| spec panel vs LUNA references (all 0): dbx 160, LA-2A Silver, UA 1176 Rev A | 0.000%, 0.045%, 0.125% (the "more" button) |
| spec panels vs live AU snapshots, whole catalog (219 with a reference) | median 0.50% at defaults (0.39% at minimum); 177 under 2%; 8 over 5% |
| Oxford EQ, Lexicon 224, Sphere Mic Collection (defaults) | 0.17%, 0.08%, 0.20% |
| Oxford SuprEsser DS, C-Vox, AMS Neve DFC, Precision Limiter (defaults) | 0.94%, 1.13%, 1.16%, 1.47% |
| Cyclosonic Panner (defaults) | 4.9% (its LED ring follows the running sweep in the snapshot) |
| parameter sweeps vs the live AU editor (`sweeps`, agent F's `r_renders`, 2149 shots, no audio) | median 1.09%, 90th percentile 2.17% |
| … Auto-Tune scale names, key/scale keyboard greying (336 shots) | 0.00% |
| … DFC dot-matrix level/Q readouts, EQ/FLT lamps (≈250 shots) | median 0.00%, worst 2.6% |
| … polar patterns per mic type and pattern, six mic plug-ins (≈1350 shots) | median 0.9–2.3%, worst 2.7% |
| … Oxford Dynamic EQ range line (68 shots) | median 0.96%, worst 2.1% |
| … Softube Amp Room pages at 0.5 / 1 (5 shots) | 41–75%: the live references were captured mid-scroll (each sits one ease step toward the spec's page); the spec mapping is correct, so these shots are excluded from parity |
| UA's 1176 Rev A HTML GUI in our webview, fed by our bridge, on `ua-content://` and on `file://` | 0.000% |
| header layout vs the 1.3.1 capture (SSL G, 304×710) | every button edge to the point |

## Tests

`cargo test -p plugin-ui` (add `--features webview` for the bridge transport tests):

- `tests/coverage.rs`: every catalog spec decoded, encoded and compared with its JSON; fails on any
  key the runtime drops (only `provenance`, `notes`, `_images` and the top-level `meters` list are
  ignored metadata).
- `tests/runtime.rs`: the Swift semantics feature by feature (actions, touch states and timeouts,
  frames, visibility, readouts, gestures with explicit times, menus, the "more" panel, graph
  handles, snapshots, channel layouts, move/reveal, rasters, meter queries and replies).
- `tests/catalog.rs`: every catalog spec built headless at defaults, 0 and 1 with mock meters and
  analyser blocks, and a tap, long press and drag on every hit plus a sweep of drags.
- unit tests in `graph` (GraphValue decoding and precedence, laws and inverses, band magnitudes,
  transfer curves, axes, half floats, Catmull-Rom, peak refinement, scenes), `meters`, `readout`
  and `raster`.

Plus (`tests/panel.rs`) spec parsing
(inline spec covering every element kind, plus every spec in the found folder), hit rects and
actions through `rack::Interaction`, stepped quantisation, local values, `MockParams`, the list,
meters, slot paths, lunacomponent matching, Console's header layout, the engine adapter against
the recorded x4 snapshot (gated writes, subscriptions), and the UANet dialect (parsing, subtree,
pushes, gated writes, touch gestures, step indices), the `ua-content` resolver (traversal,
allowed roots, headers) and the page keys as the GUI's two URL parsers read them. `cargo test -p uaconsole plugin` covers the
hook on engine and preview models.

## Gaps

The staged catalog (223 specs, 2026-10-13) decodes completely and every spec builds and takes
gestures headless; the Swift runtime's features are ported (above). Not covered:

- **Meters on hardware**: `MeterPoller` sends the panel's VendorSpecific gets over the store's
  connection and is tested against the mock engine; it has not run against a live Apollo here.
- **Text metrics** are estimated (shrink-to-fit and wrapping use average advances), and medium
  weights draw regular (rack has bold or regular).
- Rotation applies to sprite elements (the only ones the catalog rotates), not to text or graphs.

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
- **Spec panels through `rack` hits only** (hosts that do not route pointer events to the
  panel): horizontal faders reuse rack's vertical drag with a quarter span, and there are no long
  presses, repeats, held looks or graph handle drags.
- Not written: typed values, StepValue label sets, presets.
