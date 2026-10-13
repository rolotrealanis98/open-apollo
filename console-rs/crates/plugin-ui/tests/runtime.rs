//! The spec runtime feature by feature, pinned to the Swift runtime's semantics
//! (`SpecPluginViewModel` / `SpecPluginView`): actions, local and touch states, frames,
//! visibility, readouts, gestures, pop-ups, graph handles, meters and channel layouts.
//! Times are explicit (`*_at`), so long presses, repeats and timeouts are deterministic.

use std::sync::Arc;

use houston::{Appearance, ImageCache, Rect, Resources};
use plugin_ui::spec::Action as SpecAction;
use plugin_ui::{MockEvent, MockParams, ParamSource, PluginPanel, PluginSpec};
use rack::{Assets, Control, Draw, Ui};

const SLOT: &str = "/devices/0/inputs/0/effects/2";

const SPEC: &str = r##"{
  "schema": 1, "effectName": "UAD Runtime Test", "namePattern": "Runtime Test",
  "size": {"w": 400, "h": 300}, "monoSize": {"w": 200, "h": 300},
  "states": {"page": 0, "display": -1, "hold": 0, "channels": 2, "snap": 0},
  "parameters": [
    {"index": 0, "name": "Gain", "default": 0.5},
    {"index": 1, "name": "Mode", "steps": ["A", "B", "C", "D"], "default": 0},
    {"index": 2, "name": "Power", "steps": ["Off", "On"], "default": 1},
    {"index": 3, "name": "Freq", "default": 0.5},
    {"index": 4, "name": "Q", "default": 0.25},
    {"index": 5, "name": "Hidden", "default": 0.3},
    {"index": 6, "name": "Bank", "steps": ["1", "2", "3"], "default": 0}
  ],
  "elements": [
    {"id": "gain-knob", "kind": "knob", "x": 10, "y": 10, "w": 40, "h": 40, "sprite": "k", "frames": 11, "param": 0,
     "touchState": {"state": "display", "value": 0, "timeout": 2, "hold": "hold"},
     "tapAction": {"type": "setState", "state": "page", "value": 1},
     "frameSource": {"type": "param", "param": 1}, "reverse": true},
    {"id": "fader", "kind": "knob", "x": 60, "y": 10, "w": 80, "h": 20, "sprite": "f", "frames": 11, "param": 3, "drag": "horizontal"},
    {"id": "mode-lever", "kind": "switch", "x": 150, "y": 10, "w": 20, "h": 80, "sprite": "s", "frames": 4, "param": 1, "segments": "vertical"},
    {"id": "mode-row", "kind": "switch", "x": 180, "y": 10, "w": 80, "h": 20, "sprite": "s", "frames": 4, "param": 1, "orientation": "horizontal"},
    {"id": "held", "kind": "switch", "x": 270, "y": 10, "w": 20, "h": 20, "sprite": "h", "frames": 4, "param": 2, "pressedFrameOffset": 2,
     "longPressAction": {"type": "set", "param": 0, "value": 1}},
    {"id": "page-zone", "kind": "zone", "x": 300, "y": 10, "w": 20, "h": 20,
     "action": {"type": "cycleState", "state": "page", "count": 3}, "longPressAction": {"type": "default", "param": 0}},
    {"id": "page1-only", "kind": "zone", "x": 300, "y": 10, "w": 20, "h": 20,
     "visibility": {"params": [], "states": ["page"], "table": [{"key": [0], "opacity": 0}, {"key": [1], "opacity": 1}, {"key": [2], "opacity": 0}]},
     "action": {"type": "set", "param": 2, "value": 0}},
    {"id": "up", "kind": "button", "x": 330, "y": 10, "w": 20, "h": 20, "sprite": "b", "frames": 2, "repeat": 0.1,
     "action": {"type": "nudge", "param": 1, "delta": 0.333334}},
    {"id": "cycle", "kind": "button", "x": 360, "y": 10, "w": 20, "h": 20, "sprite": "b", "frames": 2,
     "action": {"type": "nudge", "param": 6, "delta": -0.5, "wrap": true}, "longPressAction": {"type": "set", "param": 6, "value": 1}},
    {"id": "swipe", "kind": "zone", "x": 10, "y": 100, "w": 100, "h": 50, "gesture": "swipe", "param": 1, "distance": 50},
    {"id": "lamp", "kind": "filmstrip", "x": 120, "y": 100, "w": 10, "h": 10, "sprite": "l", "frames": 2,
     "frameSource": {"type": "table", "params": [2], "states": ["page"], "meters": ["vu"],
                     "table": [{"key": [1, 0, 0], "frame": 1}, {"key": [0, 0, 0], "frame": 0}, {"key": [1, 0, 1], "frame": 0}]}},
    {"id": "clip-led", "kind": "filmstrip", "x": 135, "y": 100, "w": 10, "h": 10, "sprite": "l", "frames": 2,
     "frameSource": {"type": "meter", "meter": "vu", "threshold": 0.5}},
    {"id": "blinker", "kind": "filmstrip", "x": 150, "y": 100, "w": 10, "h": 10, "sprite": "l", "frames": 3,
     "frameSource": {"type": "blink", "param": 2, "period": 1}},
    {"id": "eblink", "kind": "filmstrip", "x": 165, "y": 100, "w": 10, "h": 10, "sprite": "l", "frames": 4,
     "frameSource": {"type": "param", "param": 1}, "blink": {"period": 1}, "reverse": true},
    {"id": "readout", "kind": "text", "x": 10, "y": 200, "w": 60, "h": 12, "param": 0, "text": {"select": {"state": "display", "params": [3, 4]}}},
    {"id": "names", "kind": "text", "x": 10, "y": 215, "w": 60, "h": 12, "param": 1,
     "text": {"value": {"param": 1, "law": {"type": "steps", "values": [0, 1, 2, 7]}}, "names": ["C", "C#", "D"]}},
    {"id": "meter-text", "kind": "text", "x": 10, "y": 230, "w": 60, "h": 12,
     "text": {"meter": "lvl", "db": 20, "scale": 1, "offset": 3, "min": -60, "max": 10, "floor": -50, "floorText": "--", "format": "%.1f dB"}},
    {"id": "static", "kind": "text", "x": 80, "y": 200, "w": 60, "h": 12, "text": {"string": "Hello"}},
    {"id": "computed", "kind": "text", "x": 80, "y": 215, "w": 60, "h": 12, "param": 0, "text": {"value": {"param": 0, "scale": 100}, "format": "%.0f %%"}},
    {"id": "menu", "kind": "menu", "x": 200, "y": 200, "w": 60, "h": 14, "param": 1, "text": {"size": 10},
     "touchState": {"state": "page", "value": 2}},
    {"id": "visible-when", "kind": "image", "x": 0, "y": 0, "w": 10, "h": 10, "sprite": "img", "visibleWhen": {"meter": "lvl", "scale": 10}},
    {"id": "eq", "kind": "graph", "x": 150, "y": 150, "w": 200, "h": 100,
     "graph": {"type": "eq", "x": {"min": 20, "max": 20000}, "y": {"min": -20, "max": 20},
               "bands": [{"id": "b", "freq": {"param": 3, "law": {"type": "log", "min": 20, "max": 20000}},
                          "gain": {"param": 0, "law": {"type": "linear", "min": -20, "max": 20}}, "q": 1}],
               "curves": [{"stroke": "#FFFFFF"}],
               "handles": [{"band": "b", "drag": true, "touchState": {"state": "display", "value": 1}}]}},
    {"id": "moved", "kind": "image", "x": 0, "y": 250, "w": 10, "h": 10, "sprite": "img",
     "move": {"x": {"param": 0, "scale": 100}}, "reveal": {"right": {"param": 0, "scale": 10}}}
  ],
  "meterSources": {
    "vu": {"vendorSpecificFloat": 1, "smoothing": 0, "calibration": [[0, 0], [1, 1]]},
    "lvl": {"vendorSpecificFloat": 2, "smoothing": 0, "calibration": [[0, 0], [1, 1]]}
  }
}"##;

fn spec() -> Arc<PluginSpec> {
    Arc::new(PluginSpec::from_json(SPEC).expect("inline spec"))
}

fn assets() -> Assets {
    let resources = Resources { root: std::env::temp_dir().join("plugin-ui-runtime-no-console"), console_version: None };
    let assets = Assets::from_parts(resources.clone(), Appearance::default(), Arc::new(ImageCache::new(resources)));
    assets.images.register_root("spec", sprite_dir());
    assets
}

/// A spec folder with one 20×20 px sprite (`img`, 10×10 pt) for elements drawn at natural size.
fn sprite_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("plugin-ui-runtime-sprites-{}", std::process::id()));
    let images = dir.join("Images");
    if !images.join("img@2x.png").is_file() {
        std::fs::create_dir_all(&images).unwrap();
        let tmp = images.join(format!("img-{:?}.tmp", std::thread::current().id()));
        let file = std::fs::File::create(&tmp).unwrap();
        let mut encoder = png::Encoder::new(file, 20, 20);
        encoder.set_color(png::ColorType::Rgba);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&[200; 20 * 20 * 4]).unwrap();
        drop(writer);
        std::fs::rename(&tmp, images.join("img@2x.png")).unwrap();
    }
    dir
}

fn panel() -> PluginPanel {
    PluginPanel::with_sprites(spec(), SLOT, |name| Some(format!("spec:Images/{name}@2x.png").into()))
}

/// Builds at 1:1 (panel points = window points) and returns the draw list.
fn build(panel: &PluginPanel, params: &dyn ParamSource) -> Vec<Draw> {
    let assets = assets();
    let mut ui = Ui::new(&assets);
    let (w, h) = panel.size();
    panel.build(&mut ui, Rect::new(0.0, 0.0, w, h), params);
    ui.list.items
}

fn el(panel: &PluginPanel, id: &str) -> usize {
    panel.spec.elements.iter().position(|e| e.id == id).unwrap_or_else(|| panic!("no element {id}"))
}

/// A press and release at (x, y) from `t` to `t + held`.
fn tap(panel: &mut PluginPanel, params: &mut MockParams, x: f32, y: f32, t: f64, held: f64) {
    build(panel, params);
    assert!(panel.pointer_down_at(x, y, false, t, params));
    panel.pointer_up_at(x, y, t + held, params);
}

fn action(json: &str) -> SpecAction {
    serde_json::from_str(json).unwrap()
}

#[test]
fn knob_drag_tap_action_and_touch_state_timeouts() {
    let mut p = panel();
    let mut params = MockParams::from_spec(&p.spec);
    build(&p, &params);
    // Drag up 20 pt: +0.1 (0.005 per point); one gesture; the touch state shows the knob.
    assert!(p.pointer_down_at(30.0, 30.0, false, 0.0, &mut params));
    assert_eq!(p.ui_state["display"], 0.0);
    p.pointer_move_at(30.0, 10.0, 0.1, &mut params);
    p.pointer_up_at(30.0, 10.0, 0.2, &mut params);
    assert_eq!(params.value(0), Some(0.6));
    assert_eq!(params.events, vec![MockEvent::Begin, MockEvent::Set(0, 0.6), MockEvent::End]);
    assert_eq!(p.ui_state["page"], 0.0, "a drag is not a tap");
    // The touch state falls back to the state's spec default 2 s after the last touch.
    p.tick_at(2.05, &mut params);
    assert_eq!(p.ui_state["display"], 0.0);
    p.tick_at(2.2, &mut params);
    assert_eq!(p.ui_state["display"], -1.0);
    // A tap without drag runs the knob's tapAction.
    tap(&mut p, &mut params, 30.0, 30.0, 3.0, 0.05);
    assert_eq!(p.ui_state["page"], 1.0);
    // The hold state keeps the touched value.
    p.ui_state.insert("hold".into(), 1.0);
    p.tick_at(6.0, &mut params);
    assert_eq!(p.ui_state["display"], 0.0, "held: no fall-back");
    // Knobs show their own parameter: no frame source, no reverse (SpriteKnob).
    assert_eq!(p.display_frame(el(&p, "gain-knob"), &params), (6, 11));
    // Double tap resets to the default.
    build(&p, &params);
    p.pointer_down_at(30.0, 30.0, true, 7.0, &mut params);
    p.pointer_up_at(30.0, 30.0, 7.05, &mut params);
    assert_eq!(params.value(0), Some(0.5));
}

#[test]
fn horizontal_faders_and_stepped_quantisation() {
    let mut p = panel();
    let mut params = MockParams::from_spec(&p.spec);
    build(&p, &params);
    p.pointer_down_at(100.0, 20.0, false, 0.0, &mut params);
    p.pointer_move_at(120.0, 25.0, 0.1, &mut params);
    p.pointer_up_at(120.0, 25.0, 0.2, &mut params);
    assert!((params.value(3).unwrap() - 0.6).abs() < 1e-9, "horizontal drag reads x only");
}

#[test]
fn switch_segments_held_looks_and_long_presses() {
    let mut p = panel();
    let mut params = MockParams::from_spec(&p.spec);
    // Vertical lever: top segment = last step.
    tap(&mut p, &mut params, 160.0, 15.0, 0.0, 0.05);
    assert_eq!(params.value(1), Some(1.0));
    tap(&mut p, &mut params, 160.0, 85.0, 1.0, 0.05);
    assert_eq!(params.value(1), Some(0.0));
    // Horizontal row: the tapped segment.
    tap(&mut p, &mut params, 225.0, 20.0, 2.0, 0.05);
    assert!((params.value(1).unwrap() - 2.0 / 3.0).abs() < 1e-12);
    // Held look: frame + offset while the touch is down; the tap acts on release.
    build(&p, &params);
    p.pointer_down_at(280.0, 20.0, false, 3.0, &mut params);
    assert_eq!(p.display_frame(el(&p, "held"), &params), (3, 4));
    p.pointer_up_at(280.0, 20.0, 3.1, &mut params);
    assert_eq!(params.value(2), Some(0.0));
    assert_eq!(p.display_frame(el(&p, "held"), &params), (0, 4));
    // A held switch decides a long press on release.
    params.set(0, 0.2);
    tap(&mut p, &mut params, 280.0, 20.0, 4.0, 0.5);
    assert_eq!(params.value(0), Some(1.0));
    assert_eq!(params.value(2), Some(0.0), "the long press replaces the tap");
}

#[test]
fn zone_long_press_fires_while_held_and_hidden_controls_take_no_touches() {
    let mut p = panel();
    let mut params = MockParams::from_spec(&p.spec);
    params.set(0, 0.9);
    build(&p, &params);
    p.pointer_down_at(310.0, 20.0, false, 0.0, &mut params);
    p.tick_at(0.3, &mut params);
    assert_eq!(params.value(0), Some(0.9));
    p.tick_at(0.46, &mut params);
    assert_eq!(params.value(0), Some(0.5), "long press: default, while still held");
    p.pointer_up_at(310.0, 20.0, 0.6, &mut params);
    assert_eq!(p.ui_state["page"], 0.0, "no tap after a long press");
    // Taps cycle the page; on page 1 the overlapping page-1 zone is visible and on top.
    tap(&mut p, &mut params, 310.0, 20.0, 1.0, 0.05);
    assert_eq!(p.ui_state["page"], 1.0);
    tap(&mut p, &mut params, 310.0, 20.0, 2.0, 0.05);
    assert_eq!(p.ui_state["page"], 1.0, "the page-1 zone took the tap");
    assert_eq!(params.value(2), Some(0.0));
    p.ui_state.insert("page".into(), 2.0);
    tap(&mut p, &mut params, 310.0, 20.0, 3.0, 0.05);
    assert_eq!(p.ui_state["page"], 0.0, "cycleState wraps at count");
    // A drag past the slop is not a tap.
    build(&p, &params);
    p.pointer_down_at(310.0, 20.0, false, 4.0, &mut params);
    p.pointer_move_at(340.0, 20.0, 4.05, &mut params);
    p.pointer_up_at(340.0, 20.0, 4.1, &mut params);
    assert_eq!(p.ui_state["page"], 0.0);
}

#[test]
fn repeat_buttons_and_wrapping_nudges() {
    let mut p = panel();
    let mut params = MockParams::from_spec(&p.spec);
    build(&p, &params);
    p.pointer_down_at(340.0, 20.0, false, 0.0, &mut params);
    assert_eq!(params.value(1), Some(0.333334), "acts on press");
    assert_eq!(p.display_frame(el(&p, "up"), &params), (1, 2), "pressed frame");
    p.tick_at(0.39, &mut params);
    assert_eq!(params.value(1), Some(0.333334));
    p.tick_at(0.41, &mut params);
    assert_eq!(params.value(1), Some(0.666668), "repeats after 0.4 s");
    p.tick_at(0.52, &mut params);
    assert_eq!(params.value(1), Some(1.0), "then every 0.1 s (clamped)");
    p.pointer_up_at(340.0, 20.0, 0.55, &mut params);
    assert_eq!(params.value(1), Some(1.0), "no action on release");
    // `wrap`: a cycle button goes round (Swift would clamp at 0).
    tap(&mut p, &mut params, 370.0, 20.0, 1.0, 0.05);
    assert_eq!(params.value(6), Some(1.0));
    tap(&mut p, &mut params, 370.0, 20.0, 2.0, 0.05);
    assert_eq!(params.value(6), Some(0.5));
    // A button's long press is decided on release.
    tap(&mut p, &mut params, 370.0, 20.0, 3.0, 0.6);
    assert_eq!(params.value(6), Some(1.0));
}

#[test]
fn swipes_step_by_one_past_their_distance() {
    let mut p = panel();
    let mut params = MockParams::from_spec(&p.spec);
    build(&p, &params);
    p.pointer_down_at(20.0, 120.0, false, 0.0, &mut params);
    p.pointer_move_at(80.0, 120.0, 0.1, &mut params);
    p.pointer_up_at(80.0, 120.0, 0.2, &mut params);
    assert!((params.value(1).unwrap() - 1.0 / 3.0).abs() < 1e-12);
    build(&p, &params);
    p.pointer_down_at(80.0, 120.0, false, 1.0, &mut params);
    p.pointer_up_at(50.0, 120.0, 1.2, &mut params);
    assert!((params.value(1).unwrap() - 1.0 / 3.0).abs() < 1e-12, "30 pt is short of 50");
    build(&p, &params);
    p.pointer_down_at(80.0, 120.0, false, 2.0, &mut params);
    p.pointer_up_at(20.0, 120.0, 2.2, &mut params);
    assert_eq!(params.value(1), Some(0.0), "left = previous step");
}

#[test]
fn frames_from_tables_meters_thresholds_and_blinks() {
    let mut p = panel();
    let mut params = MockParams::from_spec(&p.spec);
    let lamp = el(&p, "lamp");
    // Key = [power, page, vu needle].
    assert_eq!(p.frame(&p.spec.elements[lamp].clone(), &params), 1);
    p.meters.set_needle("vu", 1.0);
    assert_eq!(p.frame(&p.spec.elements[lamp].clone(), &params), 0, "meter values extend the key");
    p.meters.set_needle("vu", 0.0);
    params.set(2, 0.0);
    p.ui_state.insert("page".into(), 1.0);
    assert_eq!(p.frame(&p.spec.elements[lamp].clone(), &params), 0, "nearest row (L1)");
    // Threshold lamps.
    let led = p.spec.elements[el(&p, "clip-led")].clone();
    p.meters.set_needle("vu", 0.6);
    assert_eq!(p.frame(&led, &params), 1);
    p.meters.set_needle("vu", 0.5);
    assert_eq!(p.frame(&led, &params), 0, "strictly above");
    // Blink sources: last frame for the first half period while the parameter is on.
    let blinker = p.spec.elements[el(&p, "blinker")].clone();
    params.set(2, 1.0);
    p.tick_at(0.2, &mut params);
    assert_eq!(p.frame(&blinker, &params), 2);
    p.tick_at(0.7, &mut params);
    assert_eq!(p.frame(&blinker, &params), 0);
    params.set(2, 0.0);
    p.tick_at(1.2, &mut params);
    assert_eq!(p.frame(&blinker, &params), 0, "off: frame 0");
    // Element blink alternates a lit frame with 0, before `reverse`.
    let eb = p.spec.elements[el(&p, "eblink")].clone();
    params.set(1, 1.0 / 3.0);
    p.tick_at(2.2, &mut params);
    assert_eq!(p.frame(&eb, &params), 2, "frame 1 reversed of 4");
    p.tick_at(2.7, &mut params);
    assert_eq!(p.frame(&eb, &params), 3, "blinked to 0, reversed");
    assert!(p.animating(&params), "blinking elements keep the panel ticking");
}

#[test]
fn readouts() {
    let p = panel();
    let mut params = MockParams::from_spec(&p.spec);
    // Unpublished parameters: the panel's own values and Swift's string fallbacks.
    params.params.clear();
    let text = |p: &PluginPanel, params: &MockParams, id: &str| p.readout_text(&p.spec.elements[el(p, id)].clone(), params);
    assert_eq!(text(&p, &params, "readout"), "50", "select state −1: the element's own parameter, as a percentage");
    let mut p = p;
    p.ui_state.insert("display".into(), 1.0);
    assert_eq!(text(&p, &params, "readout"), "25", "the selected parameter");
    p.ui_state.insert("display".into(), 5.0);
    assert_eq!(text(&p, &params, "readout"), "50", "out of range: own parameter");
    assert_eq!(text(&p, &params, "static"), "Hello");
    assert_eq!(text(&p, &params, "computed"), "50 %");
    assert_eq!(text(&p, &params, "names"), "C");
    p.set_all_local(1.0 / 3.0);
    assert_eq!(text(&p, &params, "names"), "C#");
    p.set_all_local(1.0);
    assert_eq!(text(&p, &params, "names"), "", "a value past the names prints nothing");
    // Meter readouts: db × log10(raw) × scale + offset, floored, clamped.
    assert_eq!(text(&p, &params, "meter-text"), "--", "no reply yet reads 0 → below the floor");
    p.meters.set_raw("lvl", 0.1);
    assert_eq!(text(&p, &params, "meter-text"), "-17.0 dB");
    p.meters.set_raw("lvl", 10.0);
    assert_eq!(text(&p, &params, "meter-text"), "10.0 dB", "max clamps");
    p.meters.set_raw("lvl", 1e-4);
    assert_eq!(text(&p, &params, "meter-text"), "--");
    // Published parameters read Console's StringValue first.
    let params = MockParams::from_spec(&p.spec);
    assert_eq!(p.display(1, &params), "A");
    assert_eq!(p.display(0, &params), "50%", "the source's display string");
}

#[test]
fn visibility_from_visible_when_meters() {
    let mut p = panel();
    let params = MockParams::from_spec(&p.spec);
    let e = p.spec.elements[el(&p, "visible-when")].clone();
    assert_eq!(p.opacity(&e, &params), 0.0);
    p.meters.set_raw("lvl", 0.06);
    assert_eq!(p.opacity(&e, &params), 1.0);
    p.meters.set_raw("lvl", 0.04);
    assert_eq!(p.opacity(&e, &params), 0.0);
}

#[test]
fn menus_pop_up_their_steps() {
    let mut p = panel();
    let mut params = MockParams::from_spec(&p.spec);
    tap(&mut p, &mut params, 220.0, 205.0, 0.0, 0.05);
    assert!(p.popup_open());
    let assets = assets();
    let mut ui = Ui::new(&assets);
    p.build(&mut ui, Rect::new(0.0, 0.0, 400.0, 300.0), &params);
    // Topmost: the pop-up's row (the horizontal switch has PluginStep(1, 2) hits too).
    let row = ui.hits.iter().rev().find(|h| h.id.control == Control::PluginStep(1, 2)).expect("row C").rect;
    let (x, y) = row.center();
    assert!(p.pointer_down_at(x, y, false, 1.0, &mut params));
    p.pointer_up_at(x, y, 1.05, &mut params);
    assert!((params.value(1).unwrap() - 2.0 / 3.0).abs() < 1e-12);
    assert_eq!(p.ui_state["page"], 2.0, "the menu's touch state");
    assert!(!p.popup_open());
    // A tap outside the open list closes it without acting.
    tap(&mut p, &mut params, 220.0, 205.0, 2.0, 0.05);
    tap(&mut p, &mut params, 30.0, 290.0, 3.0, 0.05);
    assert!(!p.popup_open());
    assert!((params.value(1).unwrap() - 2.0 / 3.0).abs() < 1e-12);
}

#[test]
fn graph_handles_drag_to_the_touch_point_through_the_laws() {
    let mut p = panel();
    let mut params = MockParams::from_spec(&p.spec);
    build(&p, &params);
    // The handle sits at (632 Hz, 0 dB) = element (100, 50) = window (250, 200).
    assert!(p.pointer_down_at(250.0, 200.0, false, 0.0, &mut params));
    assert_eq!(p.ui_state["display"], 1.0, "the handle's touch state");
    p.pointer_move_at(250.0, 175.0, 0.1, &mut params);
    assert!((params.value(0).unwrap() - 0.75).abs() < 1e-9, "25 pt up = +10 dB through the gain law");
    assert!((params.value(3).unwrap() - 0.5).abs() < 1e-9);
    p.pointer_move_at(200.0, 175.0, 0.2, &mut params);
    let f = 20.0 * 1000f64.powf(50.0 / 200.0);
    assert!((params.value(3).unwrap() - (f / 20.0).ln() / 1000f64.ln()).abs() < 1e-9, "x through the log axis and law");
    p.pointer_up_at(200.0, 175.0, 0.3, &mut params);
    assert_eq!(params.events.first(), Some(&MockEvent::Begin));
    assert_eq!(params.events.last(), Some(&MockEvent::End));
    // A spec that binds a parameter only through a draggable handle keeps it off the more panel.
    assert!(!p.unbound_parameters().contains(&3));
}

#[test]
fn more_panel_reaches_unbound_parameters() {
    let mut p = panel();
    let mut params = MockParams::from_spec(&p.spec);
    assert_eq!(p.unbound_parameters(), &[4, 5]);
    let draws = build(&p, &params);
    assert!(draws.iter().any(|d| matches!(d, Draw::Image { key, .. } if key.starts_with("runtime:plugin-ui/more"))), "the more button");
    tap(&mut p, &mut params, 385.0, 14.0, 0.0, 0.05);
    assert!(p.popup_open());
    let assets = assets();
    let mut ui = Ui::new(&assets);
    p.build(&mut ui, Rect::new(0.0, 0.0, 400.0, 300.0), &params);
    let slider = ui.hits.iter().find(|h| h.id.control == Control::PluginParam(5)).expect("slider for Hidden").rect;
    let y = slider.y + slider.h / 2.0;
    assert!(p.pointer_down_at(slider.max_x() - 8.0, y, false, 1.0, &mut params));
    p.pointer_up_at(slider.max_x() - 8.0, y, 1.1, &mut params);
    assert_eq!(params.value(5), Some(1.0), "a tap at the track's end");
    tap(&mut p, &mut params, 385.0, 14.0, 2.0, 0.05);
    assert!(!p.popup_open(), "the button (under the backdrop) closes it");
}

#[test]
fn actions_follow_swift_perform() {
    let mut p = panel();
    let mut params = MockParams::from_spec(&p.spec);
    // State actions never write parameters; an action with a state ignores its param.
    p.perform(&action(r#"{"type": "setState", "state": "page", "value": 2}"#), &mut params);
    p.perform(&action(r#"{"type": "cycleState", "state": "page", "count": 3}"#), &mut params);
    assert_eq!(p.ui_state["page"], 0.0);
    p.perform(&action(r#"{"type": "set", "state": "page", "param": 0, "value": 1}"#), &mut params);
    assert_eq!(params.value(0), Some(0.5));
    assert!(params.events.is_empty());
    // Toggle cycles n-step parameters and flips two-step ones.
    params.set(1, 1.0);
    params.events.clear();
    p.perform(&action(r#"{"type": "toggle", "param": 1}"#), &mut params);
    assert_eq!(params.value(1), Some(0.0));
    p.perform(&action(r#"{"type": "toggle", "param": 2}"#), &mut params);
    assert_eq!(params.value(2), Some(0.0));
    p.perform(&action(r#"{"type": "default", "param": 2}"#), &mut params);
    assert_eq!(params.value(2), Some(1.0));
    // Nudge clamps without wrap.
    p.perform(&action(r#"{"type": "nudge", "param": 0, "delta": 0.7}"#), &mut params);
    assert_eq!(params.value(0), Some(1.0));
    // Table on the current value; table keyed on other parameters.
    params.set(0, 0.4);
    p.perform(&action(r#"{"type": "table", "param": 0, "table": [{"current": 0, "value": 0.1}, {"current": 0.5, "value": 0.9}]}"#), &mut params);
    assert_eq!(params.value(0), Some(0.9));
    p.perform(&action(r#"{"type": "table", "param": 4, "keys": [1, 2], "table": [{"key": [0, 1], "value": 0.7}, {"key": [1, 1], "value": 0.2}]}"#), &mut params);
    assert_eq!(params.value(4), Some(0.7), "nearest row by the keys' values");
    // Copy skips its own parameter; multi reads values as it goes; one undo step.
    params.events.clear();
    p.perform(&action(r#"{"type": "multi", "actions": [{"type": "set", "param": 0, "value": 0.2}, {"type": "copy", "param": 0, "to": [3, 4, 0]}]}"#), &mut params);
    assert_eq!((params.value(3), params.value(4)), (Some(0.2), Some(0.2)));
    assert_eq!(params.events, vec![MockEvent::Begin, MockEvent::Set(0, 0.2), MockEvent::Set(3, 0.2), MockEvent::Set(4, 0.2), MockEvent::End]);
    // Resend sends unchanged values.
    params.events.clear();
    p.perform(&action(r#"{"type": "resend", "params": [3]}"#), &mut params);
    assert_eq!(params.events, vec![MockEvent::Begin, MockEvent::Set(3, 0.2), MockEvent::End]);
    // An unchanged set sends nothing.
    params.events.clear();
    p.perform(&action(r#"{"type": "set", "param": 3, "value": 0.2}"#), &mut params);
    assert!(params.events.is_empty());
    // Random: a step for stepped parameters.
    p.seed(7);
    for _ in 0..20 {
        p.perform(&action(r#"{"type": "random", "param": 1}"#), &mut params);
        let v = params.value(1).unwrap() * 3.0;
        assert!((v - v.round()).abs() < 1e-9);
    }
}

#[test]
fn snapshots_switch_store_and_recall() {
    let mut p = panel();
    let mut params = MockParams::from_spec(&p.spec);
    let switch = |v: u8| action(&format!(r#"{{"type": "snapshot", "op": "switch", "state": "snap", "value": {v}, "slots": ["A", "B"], "params": [0]}}"#));
    p.perform(&switch(1), &mut params);
    assert_eq!(p.ui_state["snap"], 1.0);
    assert_eq!(params.value(0), Some(0.5), "an empty slot keeps the current values");
    params.set(0, 0.9);
    p.perform(&switch(0), &mut params);
    assert_eq!(params.value(0), Some(0.5), "A recalled");
    assert_eq!(p.snapshot_slot("B").and_then(|s| s.get(&0)), Some(&0.9));
    p.perform(&switch(0), &mut params);
    assert_eq!(p.ui_state["snap"], 0.0, "switching to the current slot does nothing");
    p.perform(&action(r#"{"type": "snapshot", "op": "store", "slot": "B", "params": [0, 3]}"#), &mut params);
    params.set(3, 0.1);
    p.perform(&action(r#"{"type": "snapshot", "op": "recall", "slot": "B"}"#), &mut params);
    assert_eq!(params.value(3), Some(0.5));
}

#[test]
fn channel_layout_selects_the_mono_size() {
    let mut p = panel();
    assert_eq!(p.size(), (400.0, 300.0));
    p.set_channel_layout(false);
    assert_eq!(p.size(), (200.0, 300.0));
    assert_eq!(p.ui_state["channels"], 1.0);
    p.set_channel_layout(true);
    assert_eq!(p.size(), (400.0, 300.0));
    // Specs that do not declare the state are left alone.
    let mut json: serde_json::Value = serde_json::from_str(SPEC).unwrap();
    json["states"].as_object_mut().unwrap().remove("channels");
    let spec = Arc::new(PluginSpec::from_json(&json.to_string()).unwrap());
    let mut q = PluginPanel::with_sprites(spec, SLOT, |_| None);
    q.set_channel_layout(false);
    assert_eq!(q.size(), (400.0, 300.0));
    assert!(!q.ui_state.contains_key("channels"));
}

#[test]
fn move_shifts_and_reveal_clips_in_the_unshifted_frame() {
    let p = panel();
    let params = MockParams::from_spec(&p.spec);
    let draws = build(&p, &params);
    let i = draws.iter().position(|d| matches!(d, Draw::Image { dst, .. } if dst.y == 250.0)).expect("moved image");
    let Draw::Image { dst, .. } = &draws[i] else { unreachable!() };
    assert_eq!(dst.x, 50.0, "move x = 0.5 × 100");
    // The reveal clip (right edge 0.5 × 10 = 5) is pushed before the element's own clip.
    let clips: Vec<Rect> = draws[..i].iter().rev().filter_map(|d| if let Draw::Clip(Some(r)) = d { Some(*r) } else { None }).take(2).collect();
    assert!(clips.contains(&Rect::new(0.0, 250.0, 5.0, 10.0)), "{clips:?}");
}

#[test]
fn graphs_draw_code_rasters_and_meters_feed_the_panel() {
    let mut p = panel();
    let params = MockParams::from_spec(&p.spec);
    let assets = assets();
    let mut ui = Ui::new(&assets);
    p.build(&mut ui, Rect::new(0.0, 0.0, 400.0, 300.0), &params);
    let keys: Vec<String> = ui.list.items.iter().filter_map(|d| if let Draw::Image { key, .. } = d { Some(key.to_string()) } else { None }).collect();
    let curves = keys.iter().find(|k| k.ends_with("/curves")).expect("curve raster");
    let img = assets.images.get(curves).expect("stored");
    assert_eq!((img.width, img.height), (400, 200), "the graph at 2× its points");
    assert!(img.rgba.chunks(4).any(|px| px[3] > 0), "something drawn");
    assert!(keys.iter().any(|k| k.ends_with("/handles")));
    // Meter replies reach the needles through the panel (modes read the panel's values).
    p.ingest_floats("3f000000,3f800000", &params);
    p.meters.tick(&p.spec.clone(), 0.016);
    assert_eq!(p.meters.needle("vu"), 0.5);
    assert_eq!(p.meters.raw("lvl"), Some(1.0));
    // Dropping the panel releases its rasters.
    let key = curves.clone();
    drop(p);
    assert_eq!(assets.images.runtime_revision(&key), 0);
}
