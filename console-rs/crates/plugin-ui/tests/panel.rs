//! Spec parsing, hit mapping and parameter round trips. Inline specs always run; the fetched
//! ConsoleLink specs are used when a spec folder is found (`$OA_PLUGIN_SPECS`, …).

use std::sync::Arc;

use houston::{Appearance, ImageCache, Rect, Resources};
use plugin_ui::meters::MeterFeed;
use plugin_ui::spec::{ElementKind, MeterSource, Row};
use plugin_ui::{GenericPanel, MockEvent, MockParams, ParamSource, PluginPanel, PluginSpec, PluginSpecLibrary, SlotPath};
use rack::{Action, Assets, Control, ControlId, HitKind, Interaction, Ui};

const SLOT: &str = "/devices/0/inputs/0/effects/1";

fn assets() -> Assets {
    let resources = Resources { root: std::env::temp_dir().join("plugin-ui-no-console"), console_version: None };
    Assets::from_parts(resources.clone(), Appearance::default(), Arc::new(ImageCache::new(resources)))
}

/// A small spec exercising every interactive element kind.
const SPEC: &str = r##"{
  "schema": 1, "effectName": "UAD Test Comp", "namePattern": "Test Comp",
  "size": {"w": 400, "h": 200}, "background": "bg",
  "parameters": [
    {"index": 0, "name": "Input", "steps": null, "default": 0.5},
    {"index": 1, "name": "Ratio", "steps": ["4", "8", "12", "20"], "default": 0.0},
    {"index": 2, "name": "Power", "steps": ["Off", "On"], "default": 1.0},
    {"index": 3, "name": "Mode", "steps": ["A", "B", "C"], "default": 0.0},
    {"index": 4, "name": "Mix", "steps": null, "default": 1.0}
  ],
  "elements": [
    {"id": "input-knob", "kind": "knob", "x": 10, "y": 10, "w": 60, "h": 60, "sprite": "knob", "frames": 101, "param": 0},
    {"id": "ratio-switch", "kind": "switch", "x": 100, "y": 10, "w": 80, "h": 20, "sprite": "ratio", "frames": 4, "param": 1, "orientation": "horizontal"},
    {"id": "power-switch", "kind": "switch", "x": 200, "y": 10, "w": 20, "h": 20, "sprite": "power", "frames": 2, "param": 2},
    {"id": "mode-menu", "kind": "menu", "x": 250, "y": 10, "w": 50, "h": 16, "param": 3,
     "text": {"size": 10, "color": "#FFCC00", "align": "center", "weight": "bold", "monospaced": false}},
    {"id": "ratio-20", "kind": "zone", "x": 100, "y": 40, "w": 20, "h": 20, "param": 1, "action": {"type": "set", "param": 1, "value": 1.0}},
    {"id": "mix-drag", "kind": "zone", "x": 300, "y": 100, "w": 40, "h": 40, "param": 4, "gesture": "drag"},
    {"id": "hidden", "kind": "knob", "x": 10, "y": 10, "w": 60, "h": 60, "sprite": "knob", "frames": 101, "param": 4, "opacity": 0},
    {"id": "lamp", "kind": "filmstrip", "x": 350, "y": 10, "w": 20, "h": 20, "sprite": "lamp", "frames": 2,
     "frameSource": {"type": "table", "params": [2], "table": [{"key": [0], "frame": false}, {"key": [1], "frame": true}]}},
    {"id": "reset", "kind": "button", "x": 350, "y": 150, "w": 30, "h": 30, "sprite": "btn", "frames": 2,
     "action": {"type": "multi", "actions": [{"type": "default", "param": 0}, {"type": "set", "param": 4, "value": 0.25}]}},
    {"id": "readout", "kind": "text", "x": 10, "y": 80, "w": 60, "h": 12, "param": 0}
  ],
  "meterSources": {"vu": {"vendorSpecificFloat": 162, "smoothing": 0.7, "calibration": [[0.0, 0.0], [1.0, 0.5], [2.0, 1.0]]}},
  "todo": []
}"##;

fn test_spec() -> Arc<PluginSpec> {
    Arc::new(PluginSpec::from_json(SPEC).expect("inline spec"))
}

fn panel() -> PluginPanel {
    PluginPanel::with_sprites(test_spec(), SLOT, |name| Some(format!("spec:Images/{name}@2x.png").into()))
}

fn id(control: Control) -> ControlId {
    ControlId { strip: SLOT.into(), control }
}

#[test]
fn parses_inline_spec_and_boolean_frames() {
    let spec = test_spec();
    assert_eq!(spec.elements.len(), 10);
    assert_eq!(spec.elements[0].kind, ElementKind::Knob);
    assert_eq!(spec.step_labels(1).map(|s| s.len()), Some(4));
    let table = spec.elements[7].frame_source.as_ref().and_then(|s| s.table.as_ref()).unwrap();
    assert_eq!((table[0].frame, table[1].frame), (Some(0), Some(1)), "boolean lamp frames decode as 0/1");
    assert_eq!(spec.unbound_parameters(), Vec::<u16>::new());
    assert_eq!(spec.poll_floats(), vec![162]);
    assert!(spec.sprite_names().contains("bg"));
    // Unknown element kinds and extra keys are tolerated.
    let lenient = SPEC.replace(r#""id": "readout", "kind": "text""#, r#""id": "readout", "kind": "hologram", "extra": 1"#);
    assert_eq!(PluginSpec::from_json(&lenient).unwrap().elements[9].kind, ElementKind::Other);
}

#[test]
fn hit_mapping() {
    let assets = assets();
    let panel = panel();
    let params = MockParams::from_spec(&panel.spec);
    let mut ui = Ui::new(&assets);
    // Drawn at 2× into a rect offset by (100, 50).
    panel.build(&mut ui, Rect::new(100.0, 50.0, 800.0, 400.0), &params);
    let find = |c: Control| ui.hits.iter().filter(|h| h.id == id(c.clone())).cloned().collect::<Vec<_>>();

    let knob = find(Control::PluginParam(0));
    assert_eq!(knob.len(), 1);
    assert_eq!(knob[0].rect, Rect::new(120.0, 70.0, 120.0, 120.0));
    assert_eq!(knob[0].kind, HitKind::Drag { value: 0.5, span: 400.0, reset: Some(0.5) });
    // The hidden (opacity 0) knob on param 4 adds no hit; the drag zone does.
    let mix = find(Control::PluginParam(4));
    assert_eq!(mix.len(), 1);
    assert_eq!(mix[0].rect, Rect::new(700.0, 250.0, 80.0, 80.0));
    // Horizontal 4-step switch: one hit per segment.
    for s in 0..4u16 {
        let seg = find(Control::PluginStep(1, s));
        assert_eq!(seg.len(), 1, "segment {s}");
        assert_eq!(seg[0].rect, Rect::new(300.0 + s as f32 * 40.0, 70.0, 40.0, 40.0));
    }
    for (i, kind) in [(2u16, "power switch"), (3, "menu"), (4, "zone"), (8, "button")] {
        assert_eq!(find(Control::PluginElement(i)).len(), 1, "{kind}");
    }
    assert!(find(Control::PluginElement(7)).is_empty(), "filmstrips are not interactive");
    assert_eq!(ui.hits.len(), 1 + 1 + 4 + 4);
    // Text readout drawn with the spec style.
    let text = ui.list.items.iter().find_map(|d| match d {
        rack::Draw::Text { text, style, .. } => Some((text.clone(), style.size)),
        _ => None,
    });
    assert_eq!(text, Some(("A".to_string(), 20.0)), "menu readout comes first in z-order");
}

/// Routes pointer events through rack's `Interaction`, as the app shell does.
fn click(panel: &mut PluginPanel, params: &mut MockParams, x: f32, y: f32) {
    let assets = assets();
    let mut ui = Ui::new(&assets);
    panel.build(&mut ui, Rect::new(0.0, 0.0, 400.0, 200.0), params);
    let mut it = Interaction::default();
    let mut actions = it.pointer_down(&ui.hits, x, y, false);
    actions.extend(it.pointer_up());
    for a in &actions {
        assert!(panel.apply(a, params), "{a:?}");
    }
}

#[test]
fn actions_round_trip_through_mock() {
    let mut panel = panel();
    let mut params = MockParams::from_spec(&panel.spec);

    // Knob drag: one gesture, values relative to the start.
    let assets = assets();
    let mut ui = Ui::new(&assets);
    panel.build(&mut ui, Rect::new(0.0, 0.0, 400.0, 200.0), &params);
    let mut it = Interaction::default();
    let mut actions = it.pointer_down(&ui.hits, 40.0, 40.0, false);
    actions.extend(it.pointer_move(40.0, 0.0, false));
    actions.extend(it.pointer_up());
    for a in &actions {
        assert!(panel.apply(a, &mut params));
    }
    assert_eq!(params.events, vec![MockEvent::Begin, MockEvent::Set(0, 0.7), MockEvent::End]);
    assert_eq!(params.value(0), Some(0.7));

    // Segmented switch: third segment → step 2 of 4.
    params.events.clear();
    click(&mut panel, &mut params, 145.0, 20.0);
    assert!((params.value(1).unwrap() - 2.0 / 3.0).abs() < 1e-9);
    assert_eq!(params.events.len(), 3, "a commit is its own gesture");
    // Zone action: set ratio to 1.0.
    click(&mut panel, &mut params, 110.0, 50.0);
    assert_eq!(params.value(1), Some(1.0));
    // Two-step switch toggles.
    click(&mut panel, &mut params, 210.0, 20.0);
    assert_eq!(params.value(2), Some(0.0));
    assert_eq!(panel.frame(&panel.spec.elements[7], &params), 0, "lamp follows power through the table");
    // A menu pops up its steps; picking one sets it and closes the list.
    click(&mut panel, &mut params, 260.0, 15.0);
    assert!(panel.popup_open());
    let menu_assets = self::assets();
    let mut ui = Ui::new(&menu_assets);
    panel.build(&mut ui, Rect::new(0.0, 0.0, 400.0, 200.0), &params);
    let row = ui.hits.iter().find(|h| h.id == id(Control::PluginStep(3, 1))).expect("menu row B").rect;
    let (x, y) = row.center();
    click(&mut panel, &mut params, x, y);
    assert_eq!(params.value(3), Some(0.5));
    assert_eq!(panel.display(3, &params), "B");
    assert!(!panel.popup_open());
    // Multi action: one gesture around both writes.
    params.events.clear();
    click(&mut panel, &mut params, 360.0, 160.0);
    assert_eq!(params.events, vec![MockEvent::Begin, MockEvent::Set(0, 0.5), MockEvent::Set(4, 0.25), MockEvent::End]);
    // Actions for another slot are ignored.
    let other = Action::Tap(ControlId { strip: "/devices/0/inputs/1/effects/0".into(), control: Control::PluginElement(2) });
    assert!(!panel.apply(&other, &mut params));
}

#[test]
fn stepped_knob_quantizes_and_unbound_params_stay_local() {
    let mut panel = panel();
    // A source publishing only parameters 0…2: 3 and 4 live in the panel.
    let mut params = MockParams::from_spec(&panel.spec);
    params.params.truncate(3);
    assert!(panel.apply(&Action::Set(id(Control::PluginParam(1)), 0.4), &mut params));
    assert!((params.value(1).unwrap() - 1.0 / 3.0).abs() < 1e-9, "4 steps: 0.4 → step 1");
    assert!(panel.apply(&Action::Set(id(Control::PluginParam(4)), 0.3), &mut params));
    assert_eq!(panel.value(4, &params), 0.3);
    assert!(params.events.iter().all(|e| !matches!(e, MockEvent::Set(4, _))));
}

#[test]
fn mock_params_contract() {
    let spec = test_spec();
    let mut p = MockParams::from_spec(&spec);
    assert_eq!(p.count(), 5);
    assert_eq!(p.indices(), vec![0, 1, 2, 3, 4]);
    assert_eq!(p.name(1).as_deref(), Some("Ratio"));
    assert_eq!(p.steps(2), vec!["Off".to_string(), "On".to_string()]);
    assert_eq!(p.display(2).as_deref(), Some("On"));
    assert_eq!(p.display(0).as_deref(), Some("50%"));
    p.commit(0, 1.5);
    assert_eq!(p.normalized(0), Some(1.0), "clamped");
    assert_eq!(p.events, vec![MockEvent::Begin, MockEvent::Set(0, 1.0), MockEvent::End]);
    assert_eq!(p.depth, 0);
    assert_eq!(p.normalized(9), None);
    p.set_all(0.0);
    assert!(p.params.iter().all(|x| x.value == 0.0));
}

#[test]
fn generic_panel_rows_and_steps() {
    let assets = assets();
    let spec = test_spec();
    let mut params = MockParams::from_spec(&spec);
    let mut generic = GenericPanel::new(SLOT);
    let (w, h) = generic.size(&params);
    let mut ui = Ui::new(&assets);
    generic.build(&mut ui, Rect::new(0.0, 0.0, w, h), &params);
    // Continuous rows get a drag bar; stepped rows a segment per step.
    assert_eq!(ui.hits.iter().filter(|h| matches!(h.id.control, Control::PluginParam(_))).count(), 2);
    assert_eq!(ui.hits.iter().filter(|h| matches!(h.id.control, Control::PluginStep(1, _))).count(), 4);
    let seg = ui.hits.iter().find(|h| h.id.control == Control::PluginStep(3, 2)).unwrap().rect;
    let mut it = Interaction::default();
    let (cx, cy) = seg.center();
    for a in it.pointer_down(&ui.hits, cx, cy, false) {
        assert!(generic.apply(&a, &mut params));
    }
    assert_eq!(params.value(3), Some(1.0));
    // Restricting rows.
    generic.only = Some([0u16, 4].into_iter().collect());
    assert_eq!(generic.rows(&params), vec![0, 4]);
}

#[test]
fn meters_decode_and_calibrate() {
    assert_eq!(MeterFeed::parse_floats("3f800000,0", 2), Some(vec![1.0, 0.0]));
    assert_eq!(MeterFeed::parse_floats("1,3f800000", 1), Some(vec![1.0]));
    assert_eq!(MeterFeed::parse_floats("ok,3f800000,0", 2), Some(vec![1.0, 0.0]));
    let reply = serde_json::Value::String("[\"3f800000\", \"0\"]".into());
    assert_eq!(MeterFeed::parse_array(&reply), Some(vec![1.0, 0.0]));
    let curve = vec![vec![0.0, 0.0], vec![1.0, 0.5], vec![2.0, 1.0]];
    assert_eq!(MeterSource::calibrate(1.5, &curve), 0.75);
    assert_eq!(MeterSource::calibrate(9.0, &curve), 1.0);
    let spec = test_spec();
    assert_eq!(MeterFeed::floats_query(&spec, SLOT).as_deref(), Some("/devices/0/inputs/0/effects/1/VendorSpecific/value?floats=162"));
    let mut feed = MeterFeed::new();
    feed.ingest_floats(&spec, "3fc00000", |_| 0.0);
    for _ in 0..40 {
        feed.tick(&spec, 0.016);
    }
    assert!((feed.needle("vu") - 0.75).abs() < 0.003);
    let rows = vec![Row { key: vec![0.0, 1.0], frame: Some(3), frame_normalized: None, opacity: None }];
    assert_eq!(Row::nearest(&rows, &[0.1, 0.9]).and_then(|r| r.frame), Some(3));
}

#[test]
fn combined_and_conditioned_meters_follow_consolelink() {
    let mut json: serde_json::Value = serde_json::from_str(SPEC).unwrap();
    json["meterSources"] = serde_json::json!({
        "left": {"vendorSpecificFloat": 1, "absolute": true, "ignoreZero": true, "smoothing": 1,
                 "calibration": [[0,0],[1,1]]},
        "right": {"vendorSpecificFloat": 2, "integer": true, "opcode": {"op":519,"p0":2,"p1":1},
                  "smoothing": 1, "calibration": [[0,0],[2,1]]},
        "clip": {"combine":{"op":"max","meters":["left","right"]}, "smoothing":1}
    });
    let spec = PluginSpec::from_json(&json.to_string()).unwrap();
    assert_eq!(spec.poll_floats(), [1], "derived meters must never poll an invented ID");
    let mut feed = MeterFeed::new();
    feed.ingest_floats(&spec, "bf000000", |_| 0.0); // -0.5 becomes +0.5
    feed.tick(&spec, 0.016);
    assert_eq!(feed.needle("clip"), 0.5);
    feed.ingest_floats(&spec, "0", |_| 0.0); // hold the previous nonzero target
    feed.tick(&spec, 0.016);
    assert_eq!(feed.needle("left"), 0.5);
    feed.ingest_opcode(&spec, 2, &[f32::from_bits(2) as f64], |_| 0.0);
    feed.tick(&spec, 0.016);
    assert_eq!(feed.needle("right"), 1.0);
    assert_eq!(feed.needle("clip"), 1.0);
}

#[test]
fn keyed_action_tables_select_using_all_key_parameters() {
    let mut json: serde_json::Value = serde_json::from_str(SPEC).unwrap();
    json["elements"][4]["action"] = serde_json::json!({
        "type":"table", "param":0, "keys":[1,2], "table":[
            {"key":[0,0],"value":0.2}, {"key":[0,1],"value":0.8}
        ]
    });
    let spec = Arc::new(PluginSpec::from_json(&json.to_string()).unwrap());
    let mut panel = PluginPanel::with_sprites(spec.clone(), SLOT, |_| None);
    let mut params = MockParams::from_spec(&spec);
    panel.tap(4, &mut params);
    assert_eq!(params.normalized(0), Some(0.8));
    params.set(2, 0.0);
    panel.tap(4, &mut params);
    assert_eq!(params.normalized(0), Some(0.2));
}

#[test]
fn image_clips_restore_the_panel_for_later_images_and_controls() {
    let dir = std::env::temp_dir().join(format!("plugin-ui-clips-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = std::fs::File::create(dir.join("pixel.png")).unwrap();
    let mut encoder = png::Encoder::new(file, 2, 2);
    encoder.set_color(png::ColorType::Rgba);
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(&[255; 16]).unwrap();
    drop(writer);
    let assets = assets();
    assets.images.register_root("spec", &dir);
    let mut json: serde_json::Value = serde_json::from_str(SPEC).unwrap();
    json["background"] = serde_json::Value::Null;
    json["elements"] = serde_json::json!([
        {"id":"toolbar","kind":"image","sprite":"pixel","x":0,"y":0,"w":400,"h":24},
        {"id":"panel","kind":"image","sprite":"pixel","x":0,"y":24,"w":400,"h":176},
        {"id":"knob","kind":"knob","sprite":"pixel","param":0,"x":10,"y":80,"w":20,"h":20}
    ]);
    let spec = Arc::new(PluginSpec::from_json(&json.to_string()).unwrap());
    let panel = PluginPanel::with_sprites(spec.clone(), SLOT, |_| Some("spec:pixel.png".into()));
    let params = MockParams::from_spec(&spec);
    let mut ui = Ui::new(&assets);
    panel.build(&mut ui, Rect::new(0.0, 0.0, 400.0, 200.0), &params);
    assert!(ui.current_clip().is_none(), "panel must leave the clip stack balanced");
    assert!(ui.hits.iter().any(|h| h.id.control == Control::PluginParam(0)), "a toolbar image must not clip controls below it");
    let mut clip = None;
    let mut image_clips = Vec::new();
    for draw in &ui.list.items {
        match draw {
            rack::Draw::Clip(rect) => clip = *rect,
            rack::Draw::Image { .. } => image_clips.push(clip),
            _ => {}
        }
    }
    assert_eq!(image_clips[1], Some(Rect::new(0.0, 24.0, 400.0, 176.0)));
    assert_eq!(image_clips[2], Some(Rect::new(0.0, 0.0, 400.0, 200.0)));
}

#[test]
fn slot_paths() {
    let slot = SlotPath::input_insert(0, 2, 1);
    assert_eq!(slot.param(7).normalized(), "/devices/0/inputs/2/effects/1/parameters/7/NormalizedValue");
    assert_eq!(SlotPath::aux_insert(0, 1, 3).param(0).string(), "/devices/0/auxs/1/effects/3/parameters/0/StringValue");
    let u = SlotPath::unison(0, 0);
    assert!(u.is_unison());
    assert_eq!(u.param(2).step(), "/devices/0/inputs/0/preamps/0/effects/0/parameters/2/StepValue");
}

// MARK: fetched ConsoleLink specs

fn library() -> Option<PluginSpecLibrary> {
    let lib = PluginSpecLibrary::locate();
    if lib.is_none() {
        eprintln!("skipped: no plug-in spec folder (set OA_PLUGIN_SPECS)");
    }
    lib
}

#[test]
fn every_spec_in_the_folder_decodes() {
    let Some(lib) = library() else { return };
    assert!(lib.failures.is_empty(), "{:?}", lib.failures);
    let has_images = lib.images_dir.is_dir();
    let mut empty = Vec::new();
    for entry in &lib.entries {
        let spec = lib.spec(entry).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(spec.schema, 1, "{}", entry.stem());
        if spec.elements.is_empty() {
            // Upstream specs whose controls were not recovered (Dreamverb, Ocean Way, RealVerb
            // Pro): the window shows the generic list for them.
            empty.push(entry.stem().to_string());
        }
        assert!(spec.size.w > 0.0 && spec.size.h > 0.0);
        for p in &spec.parameters {
            assert!(spec.parameters.iter().filter(|q| q.index == p.index).count() == 1, "{} duplicate index {}", entry.stem(), p.index);
        }
        if has_images {
            let missing: Vec<&str> = spec.sprite_names().into_iter().filter(|n| lib.sprite_key(n).is_none()).collect();
            assert!(missing.is_empty(), "{}: missing sprites {missing:?}", entry.stem());
        }
    }
    eprintln!("{} specs decoded from {} ({} without elements: {empty:?})", lib.entries.len(), lib.folder.display(), empty.len());
}

#[test]
fn every_catalog_spec_opens_and_builds_its_window() {
    let Some(lib) = library() else { return };
    let assets = assets();
    lib.attach(&assets.images);
    for entry in &lib.entries {
        let spec = lib.spec(entry).unwrap();
        let params = MockParams::from_spec(&spec);
        let win = plugin_ui::PluginWindow::open(SLOT, &entry.effect_name, "Software preview", Some(&lib), None, false);
        assert_eq!(win.content.kind(), if spec.elements.is_empty() { "generic" } else { "spec" }, "{}", entry.stem());
        let mut ui = Ui::new(&assets);
        let layout = win.build(&mut ui, (0.0, 0.0), &params);
        assert!(ui.current_clip().is_none(), "{} leaves an unbalanced clip stack", entry.stem());
        assert!(layout.editor.w > 0.0 && layout.editor.h > 0.0, "{}", entry.stem());
        assert!(!ui.list.is_empty(), "{}", entry.stem());
    }
    assert!(assets.images.missing().iter().all(|k| !k.starts_with("spec:")), "missing panel sprites: {:?}", assets.images.missing());
}

#[test]
fn fetched_1176_maps_hits() {
    let Some(lib) = library() else { return };
    let Some(spec) = lib.spec_for_effect("UAD UA 1176 Rev A") else {
        eprintln!("skipped: no 1176 Rev A spec");
        return;
    };
    assert_eq!(lib.entry_for_effect("UAD UA 1176 Rev A").map(|e| e.stem()), Some("ua_1176_rev_a"));
    let mut panel = PluginPanel::new(spec.clone(), &lib, SLOT);
    let mut params = MockParams::from_spec(&spec);
    let assets = assets();
    let mut ui = Ui::new(&assets);
    panel.build(&mut ui, Rect::new(0.0, 0.0, spec.size.w, spec.size.h), &params);
    // Every visible knob with a parameter has exactly one drag hit at its rect.
    for el in spec.elements.iter().filter(|e| e.kind == ElementKind::Knob && e.param.is_some()) {
        if panel.opacity(el, &params) <= 0.01 {
            continue;
        }
        let k = el.param.unwrap();
        assert!(ui.hits.iter().any(|h| h.id == id(Control::PluginParam(k)) && h.rect == el.rect()), "{}", el.id);
    }
    // The "20" ratio button sets Ratio to 0.4.
    let i = spec.elements.iter().position(|e| e.id == "ratio_20-button").unwrap();
    assert!(panel.apply(&Action::Tap(id(Control::PluginElement(i as u16))), &mut params));
    assert_eq!(params.value(4), Some(0.4));
}
