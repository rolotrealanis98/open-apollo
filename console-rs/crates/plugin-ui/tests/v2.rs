//! Schema 2 (RUNTIME-V2.md) on synthetic data: release folders (recipes, art root, texts,
//! availability and the generic fallback), withheld step labels and texts, and the code-drawn
//! element kinds (`toolbar`, `label`, `shape`, `lamp`) with their hits.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use houston::{Appearance, ImageCache, Rect, Resources};
use plugin_ui::{
    Availability, Content, MockParams, ParamSource, PluginPanel, PluginSpec, PluginSpecLibrary, PluginWindow,
    RecipeStatus, TextValue,
};
use rack::{Assets, Control, Draw, Ui};
use sha2::{Digest, Sha256};

const SLOT: &str = "/devices/0/inputs/0/effects/1";

const SPEC: &str = r##"{
  "schema": 2, "effectName": "UAD V2 Test", "namePattern": "V2 Test", "size": {"w": 300, "h": 124},
  "parameters": [
    {"index": 0, "name": "Power", "steps": ["Off", "On"], "default": 1},
    {"index": 1, "name": "Bank", "steps": ["A", "B", "C", "D"], "default": 0},
    {"index": 2, "name": "Mix", "default": 0.5},
    {"index": 3, "name": "Preset", "steps": null, "stepCount": 5, "stepsRef": "v2.presets", "default": 0},
    {"index": 4, "name": "Band", "steps": ["Off", "On"], "default": 0},
    {"index": 5, "name": "Mode", "steps": null, "stepCount": 3, "default": 0}
  ],
  "elements": [
    {"id": "panel", "kind": "image", "x": 0, "y": 24, "w": 300, "h": 100, "sprite": "v2_panel"},
    {"id": "tb", "kind": "toolbar", "x": 0, "y": 0, "w": 300, "h": 24,
     "toolbar": {"family": "brainworx", "items": [
       {"type": "switch", "x": 0, "y": 0, "w": 24, "h": 24, "param": 0, "sprite": "chrome.bx.power", "glyph": "power"},
       {"type": "icon", "x": 30, "y": 0, "w": 24, "h": 24, "sprite": "chrome.bx.undo", "glyph": "undo"},
       {"type": "bank", "x": 60, "y": 0, "w": 96, "h": 24, "param": 1, "sprite": "chrome.bx.bank"},
       {"type": "knob", "x": 200, "y": 0, "w": 20, "h": 24, "param": 2, "ring": {"cx": 10, "cy": 12, "r": 7, "width": 2}},
       {"type": "icon", "x": 230, "y": 0, "w": 40, "h": 24, "glyph": "About"}]}},
    {"id": "band-label", "kind": "label", "x": 10, "y": 40, "w": 40, "h": 14, "param": 4,
     "text": {"string": "HF", "font": "Arial", "size": 11, "align": "center"}, "colors": ["#808080", "#FFFFFF"], "fills": ["#202020", "#304050"]},
    {"id": "info", "kind": "text", "x": 10, "y": 60, "w": 200, "h": 40, "text": {"stringRef": "v2.info", "lines": 0, "size": 9}},
    {"id": "lamp", "kind": "lamp", "x": 100, "y": 40, "w": 10, "h": 10, "param": 4, "lamp": {"on": "#30FF40", "off": "#103010", "glow": 3}},
    {"id": "nut", "kind": "shape", "x": 130, "y": 40, "w": 20, "h": 20, "shape": {"type": "hexagon", "fill": "#909090", "stroke": "#202020"},
     "rotate": {"param": 2, "scale": 270}},
    {"id": "preset", "kind": "menu", "x": 160, "y": 40, "w": 60, "h": 14, "param": 3, "text": {"size": 10}}
  ]
}"##;

const RECIPES: &str = r##"{"recipesSchema": 1, "group": "v2", "inputs": {},
  "sprites": {
    "v2_panel": {"status": "verified", "output": {"w": 600, "h": 200, "sha256": "00"}},
    "chrome.bx.power": {"status": "unverified"},
    "chrome.bx.undo": {"status": "unverified"},
    "chrome.bx.bank": {"status": "fallback"}
  },
  "texts": {
    "v2.info": {"status": "verified", "kind": "dll-string", "output": {"sha256": "INFO_SHA", "bytes": 12}},
    "v2.presets": {"status": "verified", "kind": "dll-string-table", "output": {"sha256": "PRESETS_SHA", "items": 3}}
  }}"##;

fn hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

fn temp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "plugin-ui-v2-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn png(path: &Path, w: u32, h: u32, rgba: [u8; 4]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let file = std::fs::File::create(path).unwrap();
    let mut enc = png::Encoder::new(file, w, h);
    enc.set_color(png::ColorType::Rgba);
    let mut wr = enc.write_header().unwrap();
    let data: Vec<u8> = (0..w * h).flat_map(|_| rgba).collect();
    wr.write_image_data(&data).unwrap();
}

const INFO: &str = "Info text\nok";
const PRESETS: &[&str] = &["Room", "Hall", "Plate"];

/// A release folder (`spec`, `recipes/`) and an art root with everything generated.
fn release(tag: &str, recipes: &str) -> (PathBuf, PathBuf) {
    let root = temp(tag);
    let specs = root.join("specs");
    let art = root.join("art");
    std::fs::create_dir_all(specs.join("recipes")).unwrap();
    std::fs::write(specs.join("PluginSpec_v2_test.json"), SPEC).unwrap();
    let presets = serde_json::to_string(PRESETS).unwrap();
    let recipes = recipes.replace("INFO_SHA", &hex(INFO.as_bytes())).replace("PRESETS_SHA", &hex(presets.as_bytes()));
    std::fs::write(specs.join("recipes/v2.json"), recipes).unwrap();
    png(&art.join("Images/v2_panel@2x.png"), 600, 200, [40, 40, 40, 255]);
    png(&art.join("Images/chrome.bx.power@2x.png"), 48, 96, [0, 128, 255, 255]);
    png(&art.join("Images/chrome.bx.undo@2x.png"), 48, 96, [200, 200, 200, 255]);
    png(&art.join("Images/chrome.bx.bank@2x.png"), 192, 96, [200, 200, 200, 255]);
    std::fs::create_dir_all(art.join("Text")).unwrap();
    std::fs::write(art.join("Text/v2.info.txt"), INFO).unwrap();
    std::fs::write(art.join("Text/v2.presets.json"), &presets).unwrap();
    (specs, art)
}

fn assets() -> Assets {
    let resources = Resources { root: std::env::temp_dir().join("plugin-ui-v2-no-console"), console_version: None };
    Assets::from_parts(resources.clone(), Appearance::default(), Arc::new(ImageCache::new(resources)))
}

fn spec() -> Arc<PluginSpec> {
    Arc::new(PluginSpec::from_json(SPEC).unwrap())
}

fn build<'a>(panel: &PluginPanel, assets: &'a Assets, params: &dyn ParamSource) -> Ui<'a> {
    let mut ui = Ui::new(assets);
    let (w, h) = panel.size();
    panel.build(&mut ui, Rect::new(0.0, 0.0, w, h), params);
    assert!(ui.current_clip().is_none(), "unbalanced clip stack");
    ui
}

#[test]
fn schema_2_fields_decode_and_round_trip() {
    let s = spec();
    assert!(s.is_known_schema());
    let p = s.parameter(3).unwrap();
    assert_eq!((p.steps.as_ref(), p.step_count, p.steps_ref.as_deref()), (None, Some(5), Some("v2.presets")));
    assert_eq!(s.text_refs().into_iter().collect::<Vec<_>>(), vec!["v2.info", "v2.presets"]);
    let names: Vec<&str> = s.sprite_names().into_iter().collect();
    assert_eq!(
        names,
        vec!["chrome.bx.bank", "chrome.bx.power", "chrome.bx.undo", "v2_panel"],
        "toolbar item sprites are spec sprites"
    );
    // Toolbar switches, banks and knobs bind their parameters; the menu binds 3; 5 is unbound.
    assert_eq!(s.unbound_parameters(), vec![4, 5]);
    let round: serde_json::Value = serde_json::to_value(&*s).unwrap();
    assert_eq!(PluginSpec::from_json(&round.to_string()).unwrap(), *s, "encode → decode is lossless");
    assert_eq!(round["elements"][1]["toolbar"]["items"][3]["ring"]["width"], 2.0);
    assert_eq!(round["elements"][2]["colors"][1], "#FFFFFF");
    assert_eq!(round["elements"][2]["text"]["font"], "Arial");
    assert_eq!(round["elements"][4]["lamp"]["glow"], 3.0);
    assert_eq!(round["parameters"][3]["stepsRef"], "v2.presets");
    let future = SPEC.replace("\"schema\": 2", "\"schema\": 3");
    assert!(!PluginSpec::from_json(&future).unwrap().is_known_schema());
}

#[test]
fn release_folder_resolves_art_texts_and_availability() {
    let (specs, art) = release("ready", RECIPES);
    let lib = PluginSpecLibrary::open_with_art(&specs, &art).unwrap();
    assert!(!lib.is_legacy());
    assert_eq!(lib.images_dir, art.join("Images"));
    assert_eq!(lib.recipes.status("chrome.bx.bank"), Some(RecipeStatus::Fallback));
    assert_eq!(lib.sprite_key("v2_panel").as_deref(), Some("spec:Images/v2_panel@2x.png"));
    assert_eq!(lib.text("v2.info"), Some(TextValue::Text(INFO.into())));
    assert_eq!(lib.text("v2.presets"), Some(TextValue::List(PRESETS.iter().map(|s| s.to_string()).collect())));
    let entry = lib.entry_named("v2_test").unwrap().clone();
    let s = lib.spec(&entry).unwrap();
    assert_eq!(lib.availability(&s), Availability::Ready);
    let win = PluginWindow::open(SLOT, "UAD V2 Test", "In 1, Insert 1", Some(&lib), None, false);
    assert_eq!(win.content.kind(), "spec");
    assert_eq!(win.availability, Some(Availability::Ready));

    // Withheld labels come from the imported list, then placeholders up to stepCount.
    let mut params = MockParams::from_spec(&s);
    let panel = PluginPanel::new(s.clone(), &lib, SLOT);
    assert!(panel.missing_sprites().is_empty() && panel.missing_texts().is_empty());
    assert_eq!(panel.step_labels(3, &params), vec!["Room", "Hall", "Plate", "4", "5"]);
    params.params[3].value = 0.25;
    assert_eq!(panel.display(3, &MockParams::default()), "Room", "an unbound parameter reads its spec default");
    assert_eq!(panel.steps(5, &params), 3, "stepCount keeps a withheld parameter stepped");
    assert_eq!(panel.step_labels(5, &params), vec!["1", "2", "3"]);
    let info = s.elements.iter().find(|e| e.id == "info").unwrap();
    assert_eq!(panel.readout_text(info, &params), INFO);

    // A text that does not match its recipe reads as not imported.
    std::fs::write(art.join("Text/v2.info.txt"), "tampered").unwrap();
    let lib2 = PluginSpecLibrary::open_with_art(&specs, &art).unwrap();
    assert_eq!(lib2.text("v2.info"), None);
    assert_eq!(lib2.availability(&s), Availability::Pending(vec!["v2.info".into()]));
    std::fs::remove_dir_all(specs.parent().unwrap()).ok();
}

#[test]
fn pending_and_unavailable_ids_open_the_generic_list() {
    // Not generated yet: pending.
    let (specs, art) = release("pending", RECIPES);
    std::fs::remove_file(art.join("Images/chrome.bx.undo@2x.png")).unwrap();
    let lib = PluginSpecLibrary::open_with_art(&specs, &art).unwrap();
    let s = lib.spec_for_effect("UAD V2 Test").unwrap();
    assert_eq!(lib.availability(&s), Availability::Pending(vec!["chrome.bx.undo".into()]));
    let win = PluginWindow::open(SLOT, "UAD V2 Test", "t", Some(&lib), None, false);
    assert!(matches!(win.content, Content::Generic(_)), "a half-drawn panel is not opened");
    assert_eq!(win.availability, Some(Availability::Pending(vec!["chrome.bx.undo".into()])));
    std::fs::remove_dir_all(specs.parent().unwrap()).ok();

    // Deferred (the code-drawn tier-4 panels), no-recipe and ids no recipe names: unavailable,
    // even when a file happens to exist.
    let recipes = RECIPES
        .replace(r#""v2_panel": {"status": "verified""#, r#""v2_panel": {"status": "deferred""#)
        .replace(r#""chrome.bx.undo": {"status": "unverified"},"#, "");
    let (specs, art) = release("deferred", &recipes);
    let lib = PluginSpecLibrary::open_with_art(&specs, &art).unwrap();
    let s = lib.spec_for_effect("UAD V2 Test").unwrap();
    assert_eq!(lib.availability(&s), Availability::Unavailable(vec!["chrome.bx.undo".into(), "v2_panel".into()]));
    assert_eq!(lib.sprite_key("v2_panel"), None, "a deferred id never resolves");
    let win = PluginWindow::open(SLOT, "UAD V2 Test", "t", Some(&lib), None, false);
    assert!(matches!(win.content, Content::Generic(_)), "deferred panels open as the generic list");
    std::fs::remove_dir_all(specs.parent().unwrap()).ok();
}

#[test]
fn duplicate_recipe_ids_refuse_the_folder() {
    let (specs, art) = release("dup", RECIPES);
    std::fs::write(
        specs.join("recipes/z.json"),
        r#"{"recipesSchema": 1, "group": "z", "inputs": {}, "sprites": {"v2_panel": {"status": "verified"}}}"#,
    )
    .unwrap();
    let err = PluginSpecLibrary::open_with_art(&specs, &art).err().unwrap();
    assert!(err.contains("v2_panel"), "{err}");
    std::fs::remove_dir_all(specs.parent().unwrap()).ok();
}

#[test]
fn legacy_folder_falls_back_to_the_art_root() {
    let root = temp("legacy");
    let (specs, art) = (root.join("stage"), root.join("art"));
    std::fs::create_dir_all(specs.join("Images")).unwrap();
    std::fs::write(specs.join("PluginSpec_v2_test.json"), SPEC).unwrap();
    png(&specs.join("Images/v2_panel@2x.png"), 600, 200, [1, 2, 3, 255]);
    png(&art.join("Images/chrome.bx.power@2x.png"), 48, 96, [0, 0, 255, 255]);
    let lib = PluginSpecLibrary::open_with_art(&specs, &art).unwrap();
    assert!(lib.is_legacy() && lib.recipes.is_empty());
    assert_eq!(lib.sprite_key("v2_panel").as_deref(), Some("spec:Images/v2_panel@2x.png"));
    assert_eq!(lib.sprite_key("chrome.bx.power").as_deref(), Some("oa-art:Images/chrome.bx.power@2x.png"));
    let assets = assets();
    lib.attach(&assets.images);
    assert!(assets.images.get("oa-art:Images/chrome.bx.power@2x.png").is_some());
    let s = lib.spec_for_effect("UAD V2 Test").unwrap();
    match lib.availability(&s) {
        Availability::Pending(ids) => {
            assert_eq!(ids, vec!["chrome.bx.bank", "chrome.bx.undo", "v2.info", "v2.presets"])
        }
        other => panic!("{other:?}"),
    }
    let win = PluginWindow::open(SLOT, "UAD V2 Test", "t", Some(&lib), None, false);
    assert_eq!(win.content.kind(), "spec", "a development stage opens its panels as before");
    std::fs::remove_dir_all(&root).ok();
}

fn texts(ui: &Ui) -> Vec<(String, houston::Rgba)> {
    ui.list
        .items
        .iter()
        .filter_map(|d| if let Draw::Text { text, style, .. } = d { Some((text.clone(), style.color)) } else { None })
        .collect()
}

#[test]
fn toolbar_draws_its_items_and_takes_taps_and_drags() {
    let (specs, art) = release("toolbar", RECIPES);
    let lib = PluginSpecLibrary::open_with_art(&specs, &art).unwrap();
    let assets = assets();
    lib.attach(&assets.images);
    let s = lib.spec_for_effect("UAD V2 Test").unwrap();
    let mut params = MockParams::from_spec(&s);
    let mut panel = PluginPanel::new(s.clone(), &lib, SLOT);
    let ui = build(&panel, &assets, &params);
    // The bar, then the power icon's frame 1 (on), the bank's four clipped cells, the ring raster.
    assert!(ui.list.items.iter().any(|d| matches!(d, Draw::Fill { rect, color } if rect.w == 300.0 && rect.h == 24.0 && *color == houston::Rgba::BLACK)));
    let power = ui.list.items.iter().find_map(|d| {
        if let Draw::Image { key, frame, .. } = d {
            (key.as_ref() == "spec:Images/chrome.bx.power@2x.png").then_some(*frame)
        } else {
            None
        }
    });
    assert_eq!(power.flatten().map(|f| f.index), Some(1), "Power on draws the lit frame");
    let bank: Vec<u32> = ui
        .list
        .items
        .iter()
        .filter_map(|d| {
            if let Draw::Image { key, frame, .. } = d {
                (key.as_ref() == "spec:Images/chrome.bx.bank@2x.png").then(|| frame.map(|f| f.index).unwrap_or(9))
            } else {
                None
            }
        })
        .collect();
    assert_eq!(bank, vec![1, 0, 0, 0], "cell A selected");
    assert!(
        ui.list
            .items
            .iter()
            .any(|d| matches!(d, Draw::Image { key, .. } if key.starts_with("runtime:") && key.ends_with("/1/tb3"))),
        "ring knob raster"
    );
    assert!(texts(&ui).iter().any(|(t, _)| t == "About"), "a glyph that is not a vector name draws as text");
    // rack hits: power toggles to step 0, bank cells, the ring knob drags Mix.
    let ids: Vec<Control> = ui.hits.iter().map(|h| h.id.control.clone()).collect();
    assert!(
        ids.contains(&Control::PluginStep(0, 0))
            && ids.contains(&Control::PluginStep(1, 3))
            && ids.contains(&Control::PluginParam(2))
    );

    // Pointer route: tap power (off), tap bank cell C, drag the knob up.
    assert!(panel.pointer_down_at(12.0, 12.0, false, 0.0, &mut params));
    panel.pointer_up_at(12.0, 12.0, 0.05, &mut params);
    assert_eq!(params.value(0), Some(0.0));
    build(&panel, &assets, &params);
    assert!(panel.pointer_down_at(60.0 + 24.0 * 2.5, 12.0, false, 1.0, &mut params));
    panel.pointer_up_at(60.0 + 24.0 * 2.5, 12.0, 1.05, &mut params);
    assert!((params.value(1).unwrap() - 2.0 / 3.0).abs() < 1e-9);
    build(&panel, &assets, &params);
    assert!(panel.pointer_down_at(210.0, 12.0, false, 2.0, &mut params));
    panel.pointer_move_at(210.0, -28.0, 2.1, &mut params);
    panel.pointer_up_at(210.0, -28.0, 2.2, &mut params);
    assert!((params.value(2).unwrap() - 0.7).abs() < 1e-9, "40 pt up = +0.2");
    // Power off draws frame 0.
    let ui = build(&panel, &assets, &params);
    let power = ui.list.items.iter().find_map(|d| {
        if let Draw::Image { key, frame, .. } = d {
            (key.as_ref() == "spec:Images/chrome.bx.power@2x.png").then_some(*frame)
        } else {
            None
        }
    });
    assert_eq!(power.flatten().map(|f| f.index), Some(0));
    std::fs::remove_dir_all(specs.parent().unwrap()).ok();
}

#[test]
fn toolbar_without_art_draws_glyphs_and_letters() {
    let panel = PluginPanel::with_sprites(spec(), SLOT, |_| None);
    let params = MockParams::from_spec(&panel.spec);
    let assets = assets();
    let ui = build(&panel, &assets, &params);
    let t = texts(&ui);
    for letter in ["A", "B", "C", "D"] {
        assert!(t.iter().any(|(s, _)| s == letter), "bank letter {letter}");
    }
    let glyphs = ui
        .list
        .items
        .iter()
        .filter(|d| matches!(d, Draw::Image { key, .. } if key.contains("/1/tb0") || key.contains("/1/tb1")))
        .count();
    assert_eq!(glyphs, 2, "power and undo fall back to vector glyphs");
}

#[test]
fn labels_lamps_and_shapes_follow_their_frames() {
    let panel = PluginPanel::with_sprites(spec(), SLOT, |_| None);
    let mut params = MockParams::from_spec(&panel.spec);
    let assets = assets();
    let hf = |ui: &Ui| texts(ui).into_iter().find(|(t, _)| t == "HF").map(|(_, c)| c).unwrap();
    let fill_at = |ui: &Ui| {
        ui.list
            .items
            .iter()
            .find_map(|d| {
                if let Draw::Fill { rect, color } = d {
                    (rect.x == 10.0 && rect.y == 40.0).then_some(*color)
                } else {
                    None
                }
            })
            .unwrap()
    };
    let lamp_key = |ui: &Ui| {
        ui.list
            .items
            .iter()
            .find_map(|d| {
                if let Draw::Image { key, dst, .. } = d {
                    key.ends_with("/lamp").then(|| (key.clone(), *dst))
                } else {
                    None
                }
            })
            .unwrap()
    };
    let ui = build(&panel, &assets, &params);
    assert_eq!(hf(&ui), houston::Rgba::rgb8(0x80, 0x80, 0x80));
    assert_eq!(fill_at(&ui), houston::Rgba::rgb8(0x20, 0x20, 0x20));
    let (key, dst) = lamp_key(&ui);
    assert_eq!((dst.x, dst.y, dst.w, dst.h), (97.0, 37.0, 16.0, 16.0), "the glow margin surrounds the lamp");
    let dark = assets.images.get(&key).unwrap();
    let centre = |img: &houston::ImageData| img.rgba[((img.height / 2 * img.width + img.width / 2) * 4 + 1) as usize];
    let off_green = centre(&dark);
    params.params[4].value = 1.0;
    let ui = build(&panel, &assets, &params);
    assert_eq!(hf(&ui), houston::Rgba::WHITE);
    assert_eq!(fill_at(&ui), houston::Rgba::rgb8(0x30, 0x40, 0x50));
    let lit = assets.images.get(&lamp_key(&ui).0).unwrap();
    assert!(centre(&lit) > off_green + 100, "lit {} vs off {off_green}", centre(&lit));
    // The hexagon turns with Mix (rotate = Mix × 270°): rasterised, then rotated.
    assert!(ui.list.items.iter().any(|d| matches!(d, Draw::Image { key, .. } if key.ends_with("/5/rot"))));
    params.params[2].value = 0.0;
    let ui = build(&panel, &assets, &params);
    assert!(
        ui.list.items.iter().any(|d| matches!(d, Draw::Image { key, .. } if key.ends_with("/5/shape"))),
        "no rotation at 0"
    );
}

#[test]
fn release_lint_rejects_metadata_prose_and_addresses() {
    // The release lint the panels repository runs (spec-audit §3.2), on a synthetic spec.
    fn lint(text: &str) -> Vec<String> {
        let v: serde_json::Value = serde_json::from_str(text).unwrap();
        let mut out = Vec::new();
        for k in ["_images", "provenance", "notes", "todo", "meters"] {
            if v.get(k).is_some() {
                out.push(format!("internal key {k}"));
            }
        }
        for e in v["elements"].as_array().into_iter().flatten() {
            let id = e["id"].as_str().unwrap_or("");
            if id.contains("0x") {
                out.push(format!("address in id {id}"));
            }
            if let Some(s) = e["text"]["string"].as_str()
                && (s.len() > 48 || s.contains('\n'))
            {
                out.push(format!("prose in {id}"));
            }
        }
        out
    }
    assert!(lint(SPEC).is_empty());
    let bad = SPEC
        .replace(r#""string": "HF""#, r#""string": "A long help text that\nexplains the whole plug-in""#)
        .replace(r#""id": "lamp""#, r#""id": "switch-ret:0x18000dcdc""#)
        .replace("\"schema\": 2,", "\"schema\": 2, \"notes\": [],");
    assert_eq!(lint(&bad).len(), 3);
}

/// A release folder without `recipes/` (the importer embeds them) and an art root with a manifest.
fn manifest_release(tag: &str, spec_entry: &str, built: &[&str]) -> (PathBuf, PathBuf) {
    let (specs, art) = release(tag, RECIPES);
    std::fs::remove_dir_all(specs.join("recipes")).unwrap();
    let sprites: Vec<String> =
        built.iter().map(|id| format!(r#""{id}": {{"status": "verified", "rgba": "00"}}"#)).collect();
    let manifest = format!(
        r#"{{"schema": 1, "generator": "test", "recipesSchema": 2, "sprites": {{{}}}, "texts": {{"v2.info": {{"verified": true}}, "v2.presets": {{"verified": true}}}},
            "specs": {{{spec_entry}}}}}"#,
        sprites.join(", ")
    );
    std::fs::write(art.join("manifest.json"), manifest).unwrap();
    (specs, art)
}

const ALL: &[&str] = &["v2_panel", "chrome.bx.power", "chrome.bx.undo", "chrome.bx.bank"];

#[test]
fn manifest_readiness_gates_the_panel() {
    // Ready: the importer says so and every sprite the (patched) spec draws is built.
    let ready =
        r#""v2_test": {"ready": true, "tier": 2, "sprites": 4, "built": 4, "runtime": 0, "deferred": 0, "missing": 0}"#;
    let (specs, art) = manifest_release("m-ready", ready, ALL);
    let lib = PluginSpecLibrary::open_with_art(&specs, &art).unwrap();
    assert!(!lib.is_legacy() && lib.recipes.is_empty() && lib.manifest.is_some());
    let s = lib.spec_for_effect("UAD V2 Test").unwrap();
    assert_eq!(lib.availability(&s), Availability::Ready);
    assert_eq!(PluginWindow::open(SLOT, "UAD V2 Test", "t", Some(&lib), None, false).content.kind(), "spec");
    std::fs::remove_dir_all(specs.parent().unwrap()).ok();

    // The importer calls the spec ready because a sprite is runtime (ours), but the spec still
    // names it (no patch replaced it): a hole, so the generic list. A stale file without a
    // manifest entry does not count as built.
    let (specs, art) = manifest_release("m-hole", ready, &["v2_panel", "chrome.bx.power", "chrome.bx.bank"]);
    let lib = PluginSpecLibrary::open_with_art(&specs, &art).unwrap();
    let s = lib.spec_for_effect("UAD V2 Test").unwrap();
    assert_eq!(lib.availability(&s), Availability::Unavailable(vec!["chrome.bx.undo".into()]));
    assert!(matches!(
        PluginWindow::open(SLOT, "UAD V2 Test", "t", Some(&lib), None, false).content,
        Content::Generic(_)
    ));
    std::fs::remove_dir_all(specs.parent().unwrap()).ok();

    // Deferred (tier 4): never the panel, even with every file present.
    let deferred = r#""v2_test": {"ready": false, "tier": 4, "sprites": 4, "built": 3, "runtime": 0, "deferred": 1, "missing": 0}"#;
    let (specs, art) = manifest_release("m-deferred", deferred, ALL);
    let lib = PluginSpecLibrary::open_with_art(&specs, &art).unwrap();
    let s = lib.spec_for_effect("UAD V2 Test").unwrap();
    assert!(matches!(lib.availability(&s), Availability::Unavailable(_)));
    assert!(matches!(
        PluginWindow::open(SLOT, "UAD V2 Test", "t", Some(&lib), None, false).content,
        Content::Generic(_)
    ));
    std::fs::remove_dir_all(specs.parent().unwrap()).ok();

    // A spec the importer has not processed yet: pending (run it and reopen).
    let other = r#""someone_else": {"ready": true}"#;
    let (specs, art) = manifest_release("m-pending", other, &["v2_panel"]);
    std::fs::remove_file(art.join("Images/chrome.bx.undo@2x.png")).unwrap();
    let lib = PluginSpecLibrary::open_with_art(&specs, &art).unwrap();
    let s = lib.spec_for_effect("UAD V2 Test").unwrap();
    match lib.availability(&s) {
        Availability::Pending(ids) => assert_eq!(ids, vec!["chrome.bx.bank", "chrome.bx.power", "chrome.bx.undo"]),
        other => panic!("{other:?}"),
    }
    std::fs::remove_dir_all(specs.parent().unwrap()).ok();
}
