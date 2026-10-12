//! Console's separate windows (`rack::layers`): menus clamped to the screen rather than the
//! mixer window, Settings at its remembered place with its sub-windows over it, and the split
//! of a frame into one draw list / hit list per window. A minimal fixture Appearance, no UA art.

use std::path::PathBuf;

use houston::{Rect, Resources};
use rack::input::Control;
use rack::layers::{self, LayerKind, Windowing};
use rack::{Assets, MixerModel, PreviewMixer, Ui, ViewState, build_window_with};

const FIXTURE: &str = r#"{
    "file_format": "appearance",
    "views": { "Popup-Menu-Item": { "font": "Helvetica", "font-size": 11, "font-size-dyn": true, "font-style": "bold" } }
}"#;

fn assets() -> Assets {
    static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    let dir = DIR.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("rack-layers-fixture-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("appearance")).unwrap();
        std::fs::write(dir.join("appearance/Appearance.json"), FIXTURE).unwrap();
        dir
    });
    Assets::load(Resources::at(dir).expect("fixture resources")).expect("fixture appearance")
}

struct Built {
    hits: Vec<rack::Hit>,
    layers: Vec<rack::Layer>,
    marks: Vec<rack::LayerMark>,
    list: rack::DrawList,
    size: (f32, f32),
}

fn build(assets: &Assets, model: &dyn MixerModel, view: &ViewState) -> Built {
    let mut ui = Ui::new(assets);
    let l = build_window_with(&mut ui, model, view);
    Built { hits: ui.hits, layers: ui.layers, marks: ui.layer_marks, list: ui.list, size: l.size() }
}

/// The drive Mac: mixer at (853, 330), visible frame (0, 30, 3200, 1770).
fn drive_screen() -> Windowing {
    Windowing::new(Rect::new(-853.0, -300.0, 3200.0, 1770.0))
}

/// The status bar RATE menu: inline it flips above the field to stay in the window; with
/// separate windows it opens under the field, past the bottom of the mixer (sb-rate).
#[test]
fn rate_menu_hangs_past_the_window_when_windowed() {
    let assets = assets();
    let model = PreviewMixer::default();
    let mut view = ViewState { window: Some((1470.0, 1105.0)), ..ViewState::default() };
    let open = |view: &mut ViewState| {
        let b = build(&assets, &model, view);
        view.open_menu_spec(&b.hits, "rate", &model).expect("rate menu");
        build(&assets, &model, view)
    };
    let inline = open(&mut view.clone());
    let menu = inline.layers.iter().find(|l| l.kind == LayerKind::Menu).expect("menu layer").rect;
    assert!(menu.max_y() <= inline.size.1, "inline menus stay in the window: {menu:?}");

    view.windowing = Some(drive_screen());
    let windowed = open(&mut view);
    let menu = windowed.layers.iter().find(|l| l.kind == LayerKind::Menu).expect("menu layer").rect;
    let field = windowed.hits.iter().rev().find(|h| &*h.id.strip == rack::surfaces::status::STATUS_ID && h.rect.x == menu.x).expect("RATE field").rect;
    assert_eq!(menu.y, field.max_y(), "under the field");
    assert!(menu.max_y() > windowed.size.1, "past the mixer window: {menu:?}");
    // The mixer keeps the dismiss catcher; the menu window has its rows.
    let parts = layers::split(&windowed.list, &windowed.hits, &[], windowed.layers.len(), &windowed.marks);
    assert!(parts[0].hits.iter().any(|h| h.id.control == Control::PopoverDismiss));
    let k = 1 + windowed.layers.iter().position(|l| l.kind == LayerKind::Menu).unwrap();
    assert!(!parts[k].hits.is_empty());
    assert!(parts[k].hits.iter().all(|h| menu.contains(h.rect.x, h.rect.y) && h.rect.max_x() <= menu.max_x() + 0.01), "rows inside the menu window");
    assert!(!parts[k].list.is_empty());
}

/// Settings opens where it was left, does not dim the mixer, and keeps ADD DEVICE / Route
/// Presets over it (set-adddev, set-d-iopresets).
#[test]
fn settings_window_and_its_dialogs() {
    let assets = assets();
    let model = PreviewMixer::default();
    let mut w = drive_screen();
    w.positions.insert("settings".into(), (-393.0, -155.0));
    let mut view = ViewState { window: Some((1470.0, 1105.0)), windowing: Some(w), ..ViewState::default() };
    assert!(view.open_named("settings:hardware+add-device"));
    let b = build(&assets, &model, &view);
    let settings = b.layers.iter().find(|l| l.id == "settings").expect("settings layer");
    assert_eq!((settings.rect.x, settings.rect.y, settings.rect.w, settings.rect.h), (-393.0, -155.0, 1000.0, 880.0));
    assert_eq!(settings.kind, LayerKind::Window);
    let add = b.layers.iter().find(|l| l.id == "panel:add-device").expect("ADD DEVICE over Settings");
    // JUCE centreWithSize on the visible frame (set-adddev: (637, 428)).
    assert_eq!((add.rect.x, add.rect.y, add.rect.w, add.rect.h), (637.0, 428.0, 220.0, 315.0));
    // No backdrop over the mixer: nothing in the mixer's part covers the whole window.
    let parts = layers::split(&b.list, &b.hits, &[], b.layers.len(), &b.marks);
    assert!(!parts[0].hits.iter().any(|h| h.id.control == Control::PopoverDismiss && h.rect.w >= b.size.0));
    // Its X closes ADD DEVICE alone; Escape then closes Settings.
    view.settings.child = None;
    assert_eq!(view.sheet, Some(rack::surfaces::Sheet::Settings));
    view.key_escape();
    assert!(view.sheet.is_none());
}

#[test]
fn canvas_covers_every_window() {
    let assets = assets();
    let model = PreviewMixer::default();
    let mut w = drive_screen();
    w.positions.insert("settings".into(), (-393.0, -155.0));
    let mut view = ViewState { window: Some((1470.0, 1105.0)), windowing: Some(w), ..ViewState::default() };
    view.open_named("settings");
    let b = build(&assets, &model, &view);
    assert_eq!(layers::canvas(b.size, &b.layers), Rect::new(-393.0, -155.0, 1863.0, 1260.0));
}
