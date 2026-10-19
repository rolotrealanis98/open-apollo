//! Console's menus (`rack::menu`): structure from the binary's menu builders, routing through
//! `ViewState`, the model commands, native-menu JSON. A minimal fixture Appearance, no UA art.

use std::path::PathBuf;

use houston::Resources;
use rack::input::{Action, Control, ControlId, Hit, Interaction};
use rack::menu::{self, Command, MenuAnchor, MenuCmd, MenuState, MenuTarget, PopupEntry, PopupMenu};
use rack::model::InsertState;
use rack::surfaces::{Popover, PopoverState};
use rack::{Assets, MixerModel, PreviewMixer, Ui, ViewState, build_window_with};

const FIXTURE: &str = r#"{
    "file_format": "appearance",
    "views": {
        "Switch": {}, "Switch-Small": {}, "Switch-Small-Red": {}, "Switch-Red": {}, "Switch-Large": {},
        "Switch-Tight-Kerning": {}, "Switch-Red-Tight-Kerning": {}, "Mini-Send-Mute": {}, "Mini-Send-Pre": {},
        "Console-Nav-Row-Button-Open": {}, "Console-Nav-Row-Button-Close": {}, "Console-Nav-Row-Button-Large": {},
        "Console-Nav-Row-Button-Small": {}, "Console-Nav-Row-Button-Fixed-Slots": {},
        "Switch-Power-Mode": {}, "Switch-Remove-Mode": {}, "Switch-Copy-Mode": {}, "Switch-Isolate-Mode": {},
        "Switch-Large-Tight-Kerning": {}, "Switch-Popover-Left": {}, "Channel-Input-Name": {}, "Channel-Aux-Name": {},
        "Popup-Menu-Item": { "font": "Helvetica", "font-size": 11, "font-size-dyn": true, "font-style": "bold" }
    },
    "inserts": { "Small-Normal": {} },
    "knobs": { "Mini-Send-Level": {}, "Pan-Black": {} }
}"#;

fn assets() -> Assets {
    static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    let dir = DIR.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("rack-menus-fixture-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("appearance")).unwrap();
        std::fs::write(dir.join("appearance/Appearance.json"), FIXTURE).unwrap();
        dir
    });
    Assets::load(Resources::at(dir).expect("fixture resources")).expect("fixture appearance")
}

fn hits(assets: &Assets, model: &dyn MixerModel, view: &ViewState) -> Vec<Hit> {
    let mut ui = Ui::new(assets);
    build_window_with(&mut ui, model, view);
    ui.hits
}

fn find<'a>(hits: &'a [Hit], strip: &str, control: &Control) -> Option<&'a Hit> {
    hits.iter().rev().find(|h| &*h.id.strip == strip && &h.id.control == control)
}

/// Topmost hit for a menu row with this label's control.
fn press(hits: &[Hit], h: &Hit, view: &mut ViewState, model: &mut PreviewMixer) {
    let (cx, cy) = h.rect.center();
    let mut i = Interaction::default();
    for a in i.pointer_down(hits, cx, cy, false) {
        view.dispatch(a, model);
    }
    for a in i.pointer_up() {
        view.dispatch(a, model);
    }
}

fn labels(m: &PopupMenu) -> Vec<String> {
    m.entries
        .iter()
        .map(|e| match e {
            PopupEntry::Item(i) => i.label.clone(),
            PopupEntry::Separator => "---".into(),
            PopupEntry::Header(h) => format!("[{h}]"),
            PopupEntry::Submenu { label, .. } => format!("{label} >"),
        })
        .collect()
}

#[test]
fn menu_bar_follows_the_console_builders() {
    let model = PreviewMixer::default();
    let view = ViewState::default();
    let bar = menu::menu_bar(&model, &view, false);
    let titles: Vec<&str> = bar.iter().map(|b| b.title).collect();
    assert_eq!(titles, ["File", "Edit", "Navigation", "View", "Window", "Help"]);
    assert_eq!(labels(&bar[0].menu), ["New", "Open...", "Save", "Save As...", "---", "Quit"]);
    assert_eq!(labels(&bar[1].menu), ["Undo", "Redo", "---", "Remove All >", "Enable All >", "Disable All >"]);
    assert_eq!(labels(&bar[3].menu), ["Section >", "Toggle Compact Monitor", "---", "Show/Hide Channels", "Show/Hide Floating Windows", "Show Offline Devices", "---", "Show Settings"]);
    let section = bar[3].menu.at_path(&[0]).unwrap();
    assert_eq!(section.find("Show Browser").map(|i| i.ticked), Some(true));
    assert_eq!(section.find("Show Auxes").map(|i| i.ticked), Some(true));
    assert_eq!(section.find("Show Control Room").map(|i| i.ticked), Some(false));
    assert_eq!(labels(&bar[5].menu), ["Documentation...", "---", "Give Feedback...", "About..."]);
    // Shortcut text: JUCE's descriptions.
    assert_eq!(bar[0].menu.find("Save As...").unwrap().shortcut.as_deref(), Some("ctrl + shift + S"));
    let mac = menu::menu_bar(&model, &view, true);
    assert_eq!(mac[0].menu.find("Save As...").unwrap().shortcut.as_deref(), Some("⇧⌘S"));
    assert!(mac[0].menu.find("Quit").is_none(), "Quit lives in the macOS application menu");
    assert!(mac[5].menu.find("About...").is_none());
    // Preview data has no undo history.
    assert!(!bar[1].menu.find("Undo").unwrap().enabled);
    // Every command id round-trips.
    for c in Command::ALL {
        assert_eq!(Command::from_id(c.id()), Some(*c));
    }
}

#[test]
fn native_menu_json_parses() {
    let model = PreviewMixer::default();
    let json = menu::menu_bar_json(&model, &ViewState::default(), true);
    let v: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
    assert_eq!(v["menus"].as_array().unwrap().len(), 6);
    assert_eq!(v["menus"][3]["items"][0]["type"], "submenu");
    assert_eq!(v["menus"][3]["items"][0]["items"][0]["command"], "show_browser");
    assert_eq!(v["menus"][0]["items"][0]["shortcut"]["key"], "n");
    assert_eq!(v["app"][0]["label"], "About UAD Console");
}

#[test]
fn context_menus_open_on_their_controls() {
    let assets = assets();
    let model = PreviewMixer::default();
    let view = ViewState::default();
    let hits = hits(&assets, &model, &view);
    let cases: [(&str, Control, fn(&MenuTarget) -> bool); 5] = [
        ("in0", Control::Insert(0), |t| matches!(t, MenuTarget::Insert { slot: 0, .. })),
        ("in0", Control::Name, |t| matches!(t, MenuTarget::StripLabel { .. })),
        ("in0", Control::Solo, |t| matches!(t, MenuTarget::Solo { .. })),
        ("in0", Control::Fader, |t| matches!(t, MenuTarget::CopyMix { .. })),
        ("in0", Control::PanL, |t| matches!(t, MenuTarget::CopyMix { .. })),
    ];
    for (strip, control, is) in cases {
        let mut v = ViewState::default();
        let r = find(&hits, strip, &control).unwrap_or_else(|| panic!("{control:?}")).rect;
        let (x, y) = r.center();
        assert!(v.context_click(&hits, x, y), "{control:?}");
        let m = v.menu.as_ref().unwrap();
        assert!(is(&m.target), "{control:?}");
        // Components anchor the menu under them; faders and pans at the click (−20, +10).
        match control {
            Control::Fader | Control::PanL => assert_eq!(m.anchor, MenuAnchor::Point(x + menu::POINTER_DX, y + menu::POINTER_DY)),
            _ => assert_eq!(m.anchor, MenuAnchor::Under(r)),
        }
        // A second right click closes it.
        assert!(v.context_click(&hits, x, y));
        assert!(v.menu.is_none());
    }
    // The meter right of the fader has no hit of its own: CLEAR / METERING.
    let f = find(&hits, "in0", &Control::Fader).unwrap().rect;
    let mut v = ViewState::default();
    assert!(v.context_click(&hits, f.max_x() + 10.0, f.y + f.h / 2.0));
    assert!(matches!(v.menu.as_ref().unwrap().target, MenuTarget::Meter { .. }));
    // Aux labels have no menu (capture lbl-aux).
    let aux = hits.iter().find(|h| h.id.control == Control::Name && hits.iter().any(|o| o.id.strip == h.id.strip && o.id.control == Control::PrePost)).expect("an aux strip label").rect;
    let mut v = ViewState::default();
    assert!(menu::context_menu_at(&hits, aux.center().0, aux.center().1).is_none());
    assert!(!v.open_context_menu(&hits, aux.center().0, aux.center().1));
}

/// A right click on the preamp plate (not on its controls) opens the preamp menu at the
/// pointer (`Rack_Overview_Preamp`'s plate button), Unison rows included.
#[test]
fn preamp_plate_opens_the_preamp_menu() {
    let assets = assets();
    let model = PreviewMixer::default();
    let hits = hits(&assets, &model, &ViewState::default());
    let plate = hits.iter().find(|h| h.id.control == Control::PreampPlate).expect("a preamp plate").clone();
    let (x, y) = (plate.rect.x + 3.0, plate.rect.y + 3.0);
    let mut v = ViewState::default();
    assert!(v.context_click(&hits, x, y));
    let m = v.menu.as_ref().unwrap();
    assert_eq!(m.target, MenuTarget::Preamp { strip: plate.id.strip.clone() });
    assert_eq!(m.anchor, MenuAnchor::Point(x + menu::POINTER_DX, y + menu::POINTER_DY));
    // A left click on the plate does nothing.
    let mut i = Interaction::default();
    let mut model = PreviewMixer::default();
    let mut v = ViewState::default();
    for a in i.pointer_down(&hits, x, y, false) {
        v.dispatch(a, &mut model);
    }
    assert!(v.menu.is_none() && v.popover.is_none());
}

#[test]
fn insert_menu_items_and_routing() {
    let assets = assets();
    let mut model = PreviewMixer::default();
    let in0: std::sync::Arc<str> = "in0".into();
    let loaded = menu::insert_menu(&model, &in0, 0);
    assert_eq!(labels(&loaded)[1..7], ["Copy", "Paste", "Assign...", "Presets...", "Remove", "Disable"]);
    assert!(!loaded.find("Paste").unwrap().enabled, "nothing copied yet");
    assert!(labels(&loaded).contains(&"[SM7B]".to_string()), "the strip's section under the channel name");
    let empty = menu::insert_menu(&model, &in0, 3);
    assert_eq!(labels(&empty)[0], "[INSERT 4 (EMPTY)]");

    // Right click, Copy, then Paste "<name>" into an empty slot.
    let mut view = ViewState::default();
    let h = hits(&assets, &model, &view);
    let (x, y) = find(&h, "in0", &Control::Insert(0)).unwrap().rect.center();
    view.context_click(&h, x, y);
    let h = hits(&assets, &model, &view);
    press(&h, find(&h, "in0", &Control::InsertCopy(0)).unwrap(), &mut view, &mut model);
    assert!(view.menu.is_none(), "a row closes the menu");
    let paste = menu::insert_menu(&model, &in0, 3);
    let item = paste.items().into_iter().find(|i| i.label.starts_with("Paste \"")).cloned().expect("Paste \"<name>\"");
    view.menu = Some(MenuState::at(MenuTarget::Insert { strip: in0.clone(), slot: 3 }, 600.0, 400.0));
    let h = hits(&assets, &model, &view);
    press(&h, h.iter().rev().find(|x| Some(&x.id) == item.id.as_ref()).unwrap(), &mut view, &mut model);
    assert!(matches!(&model.inserts["in0"][3], InsertState::Loaded { .. }));

    // Strip section: Remove All.
    view.menu = Some(MenuState::at(MenuTarget::Insert { strip: in0.clone(), slot: 0 }, 600.0, 400.0));
    let h = hits(&assets, &model, &view);
    let all = ControlId { strip: in0.clone(), control: Control::Menu(MenuCmd::StripInserts(menu::PluginOp::Remove)) };
    press(&h, find(&h, "in0", &all.control).unwrap(), &mut view, &mut model);
    assert!(model.inserts["in0"].iter().all(|s| *s == InsertState::Empty));

    // Assign... arms the browser for this slot; the next catalog row loads into it.
    view.menu = Some(MenuState::at(MenuTarget::Insert { strip: in0.clone(), slot: 1 }, 600.0, 400.0));
    let h = hits(&assets, &model, &view);
    press(&h, find(&h, "in0", &Control::Menu(MenuCmd::Assign(1))).unwrap(), &mut view, &mut model);
    assert!(view.show_browser && view.browser.assign_to.is_some());
    view.dispatch(Action::Tap(ControlId { strip: "browser".into(), control: Control::BrowserItem(2) }), &mut model);
    assert!(matches!(&model.inserts["in0"][1], InsertState::Loaded { name, .. } if name == "UAD A-Type Multiband Enhancer"));
    assert!(view.browser.assign_to.is_none());
}

#[test]
fn label_solo_and_meter_menus() {
    let assets = assets();
    let mut model = PreviewMixer::default();
    let in0: std::sync::Arc<str> = "in0".into();
    assert_eq!(labels(&menu::label_menu(&model, &in0)), ["[SM7B]", "Rename...", "Link", "Hide", "Isolate", "Channel Presets..."]);
    assert_eq!(labels(&menu::solo_menu(&model, &in0)), ["Clear Solos", "---", "Solo Safe"]);
    assert_eq!(labels(&menu::meter_menu(&model, &in0)), ["[CLEAR]", "Meter", "All Meters", "[METERING]", "Pre-Fader", "Post-Fader"]);

    let mut view = ViewState { menu: Some(MenuState::at(MenuTarget::StripLabel { strip: in0.clone() }, 500.0, 600.0)), ..ViewState::default() };
    let h = hits(&assets, &model, &view);
    press(&h, find(&h, "in0", &Control::Menu(MenuCmd::Rename)).unwrap(), &mut view, &mut model);
    assert!(matches!(view.popover, Some(PopoverState { kind: Popover::Rename { .. }, .. })));
    assert_eq!(view.text_edit.as_ref().map(|t| t.text.as_str()), Some("SM7B"));

    view = ViewState { menu: Some(MenuState::at(MenuTarget::Solo { strip: in0.clone() }, 500.0, 600.0)), ..ViewState::default() };
    let h = hits(&assets, &model, &view);
    press(&h, find(&h, "in0", &Control::Menu(MenuCmd::SoloSafe)).unwrap(), &mut view, &mut model);
    assert!(model.strip_menu_state(&model.inputs()[0]).solo_safe);
    assert!(menu::solo_menu(&model, &in0).find("Solo Safe").unwrap().ticked);

    view = ViewState { menu: Some(MenuState::at(MenuTarget::Meter { strip: in0.clone() }, 500.0, 600.0)), ..ViewState::default() };
    let h = hits(&assets, &model, &view);
    press(&h, find(&h, "in0", &Control::Menu(MenuCmd::PostFaderMetering(true))).unwrap(), &mut view, &mut model);
    assert!(model.post_fader_metering);
}

#[test]
fn submenus_hover_and_escape() {
    let assets = assets();
    let mut model = PreviewMixer::default();
    let mut view = ViewState { menu_bar: true, ..ViewState::default() };
    let h = hits(&assets, &model, &view);
    // Click View in the menu bar.
    press(&h, find(&h, "menubar", &Control::Menu(MenuCmd::BarTitle(3))).unwrap(), &mut view, &mut model);
    assert!(matches!(view.menu, Some(MenuState { target: MenuTarget::Bar(3), anchor: MenuAnchor::Bar(3), .. })));
    // Hover "Section >": its submenu opens.
    let h = hits(&assets, &model, &view);
    let sub = find(&h, "menu", &Control::Menu(MenuCmd::OpenSub { level: 0, index: 0 })).unwrap().rect;
    assert!(view.menu_hover(&h, sub.center().0, sub.center().1));
    assert_eq!(view.menu.as_ref().unwrap().open, vec![0]);
    // Its rows are live; Show Control Room toggles the control-room show (independent of AUX).
    let h = hits(&assets, &model, &view);
    let ctrl = ControlId { strip: "global".into(), control: Control::Menu(MenuCmd::App(Command::ShowControlRoom)) };
    press(&h, find(&h, "global", &ctrl.control).unwrap(), &mut view, &mut model);
    assert!(view.show_ctrl_room && view.show_aux);
    assert!(view.menu.is_none());
    // Escape closes the submenu first, then the menu.
    view.menu = Some(MenuState { open: vec![0], ..MenuState::at(MenuTarget::Bar(3), 0.0, 0.0) });
    view.key_escape();
    assert_eq!(view.menu.as_ref().map(|m| m.open.len()), Some(0));
    view.key_escape();
    assert!(view.menu.is_none());
    // Hovering another title while a bar menu is open switches to it.
    view.menu = Some(MenuState { target: MenuTarget::Bar(0), anchor: MenuAnchor::Bar(0), open: vec![], hover: None });
    let h = hits(&assets, &model, &view);
    let edit = find(&h, "menubar", &Control::Menu(MenuCmd::BarTitle(1))).unwrap().rect;
    assert!(view.menu_hover(&h, edit.center().0, edit.center().1));
    assert!(matches!(view.menu.as_ref().unwrap().target, MenuTarget::Bar(1)));
}

#[test]
fn commands_run_by_id_and_shell_commands_queue() {
    let mut model = PreviewMixer::default();
    let mut view = ViewState::default();
    assert!(view.run_command_id("show_browser", &mut model));
    assert!(!view.show_browser);
    assert!(view.run_command_id("show_mixer_nav", &mut model));
    assert!(!view.show_navigator);
    assert!(!view.run_command_id("undo", &mut model), "nothing to undo");
    assert!(!view.run_command_id("nope", &mut model));
    assert!(view.run_command_id("quit", &mut model));
    // File > Open... is the session layer's: it opens the path field instead of queueing a command.
    assert!(view.run_command_id("open_session", &mut model));
    // The preview session is dirty ("Session*"), so Console's save-changes question comes first.
    assert!(view.popover.is_some(), "Open... asks about unsaved changes, then opens the path field");
    assert_eq!(view.take_commands(), vec![Command::Quit]);
    assert!(view.take_commands().is_empty());
    // Edit > Disable All > Disable All Plug-Ins reaches the model.
    assert!(view.run_command_id("disable_all_plugins", &mut model));
    assert!(model.inserts.values().flatten().all(|s| !matches!(s, InsertState::Loaded { power: true, .. })));
    // Keyboard: Ctrl+, opens Settings, Ctrl+Shift+S is Save As.
    assert_eq!(menu::command_for_key(",", true, false, false), Some(Command::ShowSettings));
    assert_eq!(menu::command_for_key("s", true, true, false), Some(Command::SaveSessionAs));
    assert_eq!(menu::command_for_key("left", false, false, false), Some(Command::MixerScrollLeft));
}

#[test]
fn hiding_the_navigator_moves_the_strips_left() {
    let assets = assets();
    let model = PreviewMixer::default();
    let shown = hits(&assets, &model, &ViewState::default());
    let hidden = hits(&assets, &model, &ViewState { show_navigator: false, ..ViewState::default() });
    let x = |h: &[Hit]| find(h, "in0", &Control::Fader).unwrap().rect.x;
    assert_eq!(x(&shown) - x(&hidden), 96.0);
    assert!(hidden.iter().all(|h| !matches!(h.id.control, Control::SectionsOpenAll)));
}

#[test]
fn option_popovers_are_console_menus() {
    let assets = assets();
    let mut model = PreviewMixer::default();
    let anchor = ControlId { strip: "in1".into(), control: Control::Preamp("input_source".into()) };
    let mut view = ViewState { popover: Some(PopoverState { kind: Popover::InputSource { strip: "in1".into() }, anchor }), ..ViewState::default() };
    let h = hits(&assets, &model, &view);
    let line = find(&h, "in1", &Control::InputSource(1)).unwrap().clone();
    // A click between rows (the window) keeps it open; a row selects and closes it.
    let mut win = h.iter().rev().find(|x| matches!(x.id.control, Control::Menu(MenuCmd::Row { index: u16::MAX, .. }))).unwrap().clone();
    win.rect = houston::Rect::new(win.rect.x, win.rect.y, 1.5, 1.5); // the 2-pt window border
    press(&h, &win, &mut view, &mut model);
    assert!(view.popover.is_some());
    press(&h, &line, &mut view, &mut model);
    assert!(view.popover.is_none());
    assert_eq!(model.preamps["in1"].source, "LINE");
}

#[test]
fn menu_spec_opens_menus_for_renders() {
    let assets = assets();
    let model = PreviewMixer::default();
    let mut view = ViewState { menu_bar: true, ..ViewState::default() };
    let h = hits(&assets, &model, &view);
    view.open_menu_spec(&h, "bar:1>3", &model).unwrap();
    assert_eq!(view.menu.as_ref().unwrap().open, vec![3]);
    view.open_menu_spec(&h, "insert:in0:0", &model).unwrap();
    assert!(matches!(view.menu.as_ref().unwrap().target, MenuTarget::Insert { slot: 0, .. }));
    view.open_menu_spec(&h, "label:in0", &model).unwrap();
    assert!(view.open_menu_spec(&h, "insert:in0", &model).is_err());
    // The submenu is drawn: its rows are hits.
    view.open_menu_spec(&h, "bar:1>3", &model).unwrap();
    let h = hits(&assets, &model, &view);
    assert!(h.iter().any(|x| matches!(&x.id.control, Control::Menu(MenuCmd::App(Command::RemoveAllPlugins)))));
}

#[test]
fn plugin_window_insert_menu() {
    let model = PreviewMixer::default();
    let in0: std::sync::Arc<str> = "in0".into();
    let m = menu::plugin_insert_menu(&model, &in0, 0);
    let l = labels(&m);
    assert_eq!(l[0], "[INSERT]");
    assert_eq!(l[l.len() - 2..], ["[OPTIONS]".to_string(), "Assign...".to_string()]);
    assert!(m.items().iter().any(|i| i.ticked), "the window's own plug-in is ticked");
}
