# plugin-ui

Plug-in windows for console-rs. Tapping a loaded insert (or Unison) slot opens that plug-in's UI
inside a rebuild of UAD Console's own plug-in window, and its edits go to the UA Mixer Engine.

The window's content area is, in order of preference:

1. **UA's own HTML/JS GUI** (a LUNA "lunacomponent"), hosted in a webview (`webview` feature,
   WebKitGTK via wry). The GUI talks to its host over a WebSocket; see [WEBBRIDGE.md](WEBBRIDGE.md).
2. **A ConsoleLink panel spec** (`PluginSpec_*.json` plus `Images/*@2x.png`), drawn by
   [`PluginPanel`](src/panel.rs) into the frame's `rack::DrawList`.
3. **The generic parameter list** ([`GenericPanel`](src/generic.rs)), built from the engine's
   parameter metadata.

The window chrome (title bar, UAPW toolbar, status footer) is documented in [WINDOW.md](WINDOW.md).

No UA asset is in this repository: GUIs, specs and sprites are read at runtime.

## Where things are found

| What | Search order (first hit wins) |
|---|---|
| Panel specs | `$UACONSOLE_PLUGIN_SPECS`, `~/.local/share/open-apollo/plugin-specs`, `~/wine-ua-work/plugin-specs-local` (a folder with `PluginSpec_*.json` and `Images/`) |
| Lunacomponents | every existing folder of `$UACONSOLE_LUNA_COMPONENTS`, `~/.local/share/open-apollo/lunacomponents`, `/Library/Application Support/Universal Audio/Plug-Ins` (macOS), `~/wine-ua-work/lunacomponents` |
| Console skins and `images/uapw` | houston's `Resources` discovery (`$CONSOLE_RESOURCES`, the Wine install, …) |

The Windows Console in the Wine prefix ships no lunacomponents (`Program Files/Universal Audio`
holds only `Powered Plugins` and `UAMixerEngine.exe`), so on Linux copy them from a Mac
(`/Library/Application Support/Universal Audio/Plug-Ins/*.lunacomponent`, the `gui.bundle` and
`algo.bundle/Contents/Resources` parts are enough) into `~/.local/share/open-apollo/lunacomponents`.
Specs come from ConsoleLink's `PluginSpecs` folder.

## Opening a window (app shell)

```rust
// Once: find specs and GUIs, and let gfx resolve spec sprites (`spec:Images/…` keys).
let specs = plugin_ui::PluginSpecLibrary::locate();
if let Some(lib) = &specs { lib.attach(&assets.images); }
let luna = plugin_ui::LunaLibrary::locate();

// On Action::Tap(ControlId { strip, control: Control::Insert(k) }) for a loaded slot, the
// shell resolves the slot path and EffectName from its model, then:
let slot = plugin_ui::SlotPath::input_insert(device, input, k);   // or aux_insert / unison
if let Some(gui) = luna.component_for_effect(&effect_name) {
    // web content: open the webview with gui.url(&GuiQuery::new(host, port, slot.as_str()))
} else if let Some(spec) = specs.as_ref().and_then(|l| l.spec_for_effect(&effect_name)) {
    let panel = plugin_ui::PluginPanel::new(spec, specs.as_ref().unwrap(), slot.as_str());
    // each frame: panel.build(&mut ui, rect, &params)
} else {
    let list = plugin_ui::GenericPanel::new(slot.as_str());
    // each frame: list.build(&mut ui, rect, &params)
}
// Input: route every rack::Action whose ControlId.strip is the slot path and whose control is
// PluginParam / PluginElement / PluginStep to `panel.apply(&action, &mut params)` (or
// `list.apply`). Meters: MeterFeed polls VendorSpecific (see src/meters.rs) into panel.meters.
```

Hit regions use `rack::Control` variants appended for plug-in windows:

| Control | Hit | Meaning |
|---|---|---|
| `PluginParam(k)` | drag | parameter `k`: `GestureBegin`, `Set(v)`…, `GestureEnd` (knobs, drag zones, list bars) |
| `PluginElement(i)` | tap | spec element `i` (switch, button, action zone, menu) |
| `PluginStep(k, s)` | tap | set parameter `k` to step `s` (segmented switches, list step selectors) |

## `ParamSource`: the contract for the engine adapter

[`ParamSource`](src/params.rs) is one slot's parameters by ordinal `k`. The engine-backed
implementation (in or over `ua-engine`) must map it like this; `<slot>` is
`/devices/D/inputs/N/effects/K`, `/devices/D/auxs/N/effects/K` or the Unison slot
`/devices/D/inputs/N/preamps/0/effects/0` ([`SlotPath`](src/paths.rs) builds these).

| Method | Engine | Notes |
|---|---|---|
| `count()` / `indices()` | children of `<slot>/parameters` | numeric child names, ascending; the node exists only while a plug-in is loaded. `k` is the plug-in's parameter ordinal, which is also the spec's `param` index (verified: 15 live plug-ins, 448 parameters) |
| `name(k)` | `<slot>/parameters/<k>/Name` | |
| `normalized(k)` | `<slot>/parameters/<k>/NormalizedValue` | float 0…1; **None** when the slot does not publish `k` (the panel then keeps a local value) |
| `display(k)` | `<slot>/parameters/<k>/StringValue` | the plug-in's text ("2.80", "Off"); pushed by the engine |
| `steps(k)` | `<slot>/parameters/<k>/StepValue` → its `values` list | only on stepped parameters; step `i` of `n` is `i/(n−1)` |
| `set(k, v)` | `set <slot>/parameters/<k>/NormalizedValue/value v` | clamp to 0…1; coalesce (latest value per path, ~60 Hz); update the local value immediately so the UI follows |
| `begin_gesture()` / `end_gesture()` | `set /UndoRecording/value true` … `false` | nestable: count depth, send only the outermost pair, flush pending sets before each |
| `commit(k, v)` | begin, set, end | default method; one undo step |

When the window opens: `get <slot>?recursive=1` (read-only) to refresh, then subscribe
`NormalizedValue`, `StringValue` and `StepValue` of every parameter
(`SlotPath::param(k).subscribed()`); unsubscribe them when it closes. Writes obey the
connection's write gate: with writes off, `set` moves only the local value. Slot state for the
chrome: `<slot>/EffectName`, `<slot>/Power` (bypass), `<slot>/Preset`.

Meters (spec panels): poll `get <slot>/VendorSpecific/value?floats=ID,…` and/or
`?opcode=519&p0=ID&p1=N&p2=1` while the window is open ([`MeterFeed`](src/meters.rs) builds the
queries, decodes the replies, calibrates and smooths into `PluginPanel::meters`).

## Examples

```
cargo run -p plugin-ui --example render -- ua_1176_rev_a out.png            # spec panel: out.png (defaults) + out.zero.png (all 0)
cargo run -p plugin-ui --example render -- ua_1176_rev_a out.png --generic  # the generic list instead
cargo run -p plugin-ui --features webview --example web -- ssl_g_bus_compressor --snapshot gui.png
```

`render` compares the all-zero render with the spec pipeline's LUNA reference when one exists
(`~/wine-ua-work/plugin-specs/render/uaudio_<stem>.ref.png`): dbx 160, UA 1176 Rev A and
Teletronix LA-2A Silver are 0.000% pixels off (>24/255).

## Tests

`cargo test -p plugin-ui`: an inline spec covering every element kind (parsing, boolean table
frames, hit rects at 2× and offset, drag/tap/step/multi actions through rack's `Interaction`,
stepped quantisation, local values for unpublished parameters), `MockParams`, the generic list,
meter decoding, slot paths, and lunacomponent matching. With a spec folder present they also
decode every spec in it (all 208 of ConsoleLink's decode; Dreamverb, Ocean Way and RealVerb Pro
have no elements upstream and fall back to the list) and check the 1176 Rev A hit map.

## Gaps

- Horizontal faders (`drag: horizontal`) reuse rack's vertical drag with a quarter span.
- Menus cycle steps on tap (no popup); momentary buttons show no pressed frame.
- Typed values, StepValue label sets, presets and slot power from the panel are not written.
