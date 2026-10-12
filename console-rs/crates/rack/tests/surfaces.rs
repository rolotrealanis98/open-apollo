//! UI-surface behaviour without UA assets: a minimal fixture Appearance (a few skins, no
//! images) is enough for every hit region to be registered.

use std::path::PathBuf;

use houston::Resources;
use rack::input::{Action, Control, ControlId, Hit, Interaction, ScrollTarget};
use rack::surfaces::{Modifier, Popover, PopoverState, Section};
use rack::{Assets, MixerModel, PreviewMixer, Ui, ViewState, build_window_with};

const FIXTURE: &str = r#"{
    "file_format": "appearance",
    "views": {
        "Switch": {}, "Switch-Small": {}, "Switch-Small-Red": {}, "Switch-Red": {}, "Switch-Large": {},
        "Switch-Tight-Kerning": {}, "Switch-Red-Tight-Kerning": {}, "Mini-Send-Mute": {}, "Mini-Send-Pre": {},
        "Console-Nav-Row-Button-Open": {}, "Console-Nav-Row-Button-Close": {}, "Console-Nav-Row-Button-Large": {},
        "Console-Nav-Row-Button-Small": {}, "Console-Nav-Row-Button-Fixed-Slots": {},
        "Switch-Power-Mode": {}, "Switch-Remove-Mode": {}, "Switch-Copy-Mode": {}, "Switch-Isolate-Mode": {},
        "Switch-Large-Tight-Kerning": {}, "Switch-Popover-Left": {}, "Channel-Input-Name": {}, "Channel-Aux-Name": {}
    },
    "inserts": { "Small-Normal": {} },
    "knobs": { "Mini-Send-Level": {}, "Pan-Black": {} }
}"#;

fn assets() -> Assets {
    let dir = std::env::temp_dir().join(format!("rack-surfaces-fixture-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("appearance")).unwrap();
    std::fs::write(dir.join("appearance/Appearance.json"), FIXTURE).unwrap();
    let res = Resources::at(&PathBuf::from(&dir)).expect("fixture resources");
    Assets::load(res).expect("fixture appearance")
}

fn build(assets: &Assets, model: &dyn MixerModel, view: &ViewState) -> (Vec<Hit>, Vec<rack::ScrollRegion>) {
    let mut ui = Ui::new(assets);
    build_window_with(&mut ui, model, view);
    (ui.hits, ui.scrolls)
}

fn find<'a>(hits: &'a [Hit], strip: &str, control: &Control) -> Option<&'a Hit> {
    hits.iter().rev().find(|h| &*h.id.strip == strip && &h.id.control == control)
}

fn tap(hits: &[Hit], h: &Hit, view: &mut ViewState, model: &mut PreviewMixer) {
    let (cx, cy) = h.rect.center();
    let mut i = Interaction::default();
    for a in i.pointer_down(hits, cx, cy, false) {
        view.dispatch(a, model);
    }
    for a in i.pointer_up() {
        view.dispatch(a, model);
    }
}

#[test]
fn every_surface_registers_its_controls() {
    let assets = assets();
    let model = PreviewMixer::default();
    let (hits, scrolls) = build(&assets, &model, &ViewState::default());
    for s in [Section::Inputs, Section::Inserts, Section::Sends, Section::Fader] {
        assert!(find(&hits, "nav", &Control::Section(s)).is_some(), "{s:?}");
    }
    for m in [Modifier::Power, Modifier::Remove, Modifier::Copy, Modifier::Isolate, Modifier::SetDefault] {
        assert!(find(&hits, "nav", &Control::Modifier(m)).is_some(), "{m:?}");
    }
    for c in [Control::ShowBrowser, Control::ShowPluginScenes, Control::ShowSettings, Control::AllRecordPreEffects(true), Control::SectionsOpenAll, Control::FixedSlots] {
        assert!(find(&hits, "nav", &c).is_some(), "{c:?}");
    }
    assert!(find(&hits, "browser", &Control::BrowserItem(0)).is_some());
    assert!(find(&hits, "browser", &Control::BrowserCategory(11)).is_some());
    assert!(find(&hits, "status", &Control::StatusRate).is_some());
    assert!(find(&hits, "session", &Control::SessionNavigate(0)).is_some());
    assert!(find(&hits, "monitor", &Control::CueOutputs).is_some());
    assert!(find(&hits, "monitor", &Control::MonitorDim).is_some());
    assert!(find(&hits, "in0", &Control::Name).is_some());
    assert!(scrolls.iter().any(|s| s.target == ScrollTarget::Browser && s.max > 0.0));
    assert!(scrolls.iter().any(|s| s.target == ScrollTarget::Cells && s.max > 0.0), "sends overflow the cells column");
}

#[test]
fn section_toggle_collapses_the_input_row() {
    let assets = assets();
    let mut model = PreviewMixer::default();
    let mut view = ViewState::default();
    let (hits, _) = build(&assets, &model, &view);
    let slot = find(&hits, "in0", &Control::Insert(0)).expect("insert slot").rect;
    // Inputs row 237 + header 26 + 3 under the column top (72) + swatch (5).
    assert_eq!(slot.y, 72.0 + 5.0 + 237.0 + 26.0 + 3.0);
    let toggle = find(&hits, "nav", &Control::Section(Section::Inputs)).unwrap().clone();
    tap(&hits, &toggle, &mut view, &mut model);
    assert!(!view.sections.inputs);
    let (hits, _) = build(&assets, &model, &view);
    let slot = find(&hits, "in0", &Control::Insert(0)).unwrap().rect;
    assert_eq!(slot.y, 72.0 + 5.0 + 22.0 + 26.0 + 3.0, "collapsed = header only");
    // Input sends are reachable now (mini sends of the expanded Sends row).
    assert!(find(&hits, "in0", &Control::SendMute(0)).is_some());
    assert!(find(&hits, "in0", &Control::SendPre(0)).is_some());
}

#[test]
fn popover_items_sit_on_top_of_a_dismiss_catcher() {
    let assets = assets();
    let mut model = PreviewMixer::default();
    let mut view = ViewState {
        popover: Some(PopoverState { kind: Popover::InsertMenu { strip: "in2".into(), slot: 1 }, anchor: ControlId { strip: "in2".into(), control: Control::Insert(1) } }),
        ..ViewState::default()
    };
    let (hits, _) = build(&assets, &model, &view);
    // Far away from the popover: the catcher wins over the strip controls below.
    let top = hits.iter().rev().find(|h| h.rect.contains(1700.0, 900.0)).unwrap();
    assert_eq!(top.id.control, Control::PopoverDismiss);
    let remove = find(&hits, "in2", &Control::InsertRemove(1)).unwrap().clone();
    tap(&hits, &remove, &mut view, &mut model);
    assert!(view.popover.is_none());
    assert_eq!(model.inserts["in2"][1], rack::model::InsertState::Empty);
}

#[test]
fn monitor_page_and_settings_sheet() {
    let assets = assets();
    let mut model = PreviewMixer::default();
    let mut view = ViewState::default();
    let (hits, _) = build(&assets, &model, &view);
    let ctrl = find(&hits, "monitor", &Control::MonitorPage(rack::surfaces::MonitorPage::ControlRoom)).unwrap().clone();
    tap(&hits, &ctrl, &mut view, &mut model);
    let (hits, _) = build(&assets, &model, &view);
    assert!(find(&hits, "monitor", &Control::HeadphoneMenu(1)).is_some());
    assert!(find(&hits, "aux0", &Control::Fader).is_none(), "aux strips replaced by the control room");

    view.dispatch(Action::Toggle(ControlId { strip: "nav".into(), control: Control::ShowSettings }), &mut model);
    let (hits, _) = build(&assets, &model, &view);
    let writes = find(&hits, "settings", &Control::WritesEnabled).unwrap().clone();
    tap(&hits, &writes, &mut view, &mut model);
    assert!(model.settings.writes_enabled);
    let close = find(&hits, "settings", &Control::SettingsClose).unwrap().clone();
    tap(&hits, &close, &mut view, &mut model);
    assert!(view.sheet.is_none());
}

#[test]
fn session_navigator_scrolls_many_strips() {
    let assets = assets();
    let mut model = PreviewMixer::default();
    for i in 0..10 {
        let s = rack::model::StripInfo {
            id: format!("x{i}").as_str().into(),
            title: format!("X {i}"),
            kind: rack::geometry::StripKind::Input,
            stereo: false,
            preamp_layout: None,
            shows_sr_convert: false,
        };
        model.inputs.push(s);
    }
    let mut view = ViewState::default();
    let (hits, _) = build(&assets, &model, &view);
    assert!(find(&hits, "x9", &Control::Fader).is_none() && find(&hits, "x9", &Control::Name).is_none());
    let last = hits.iter().filter(|h| h.id.strip.as_ref() == "session").last().unwrap().clone();
    tap(&hits, &last, &mut view, &mut model);
    assert!(view.first_strip > 0);
    let (hits, _) = build(&assets, &model, &view);
    assert!(find(&hits, "x9", &Control::Name).is_some());
    assert!(find(&hits, "in0", &Control::Name).is_none());
}

/// Browser geometry of Console 1.3.1 (Rack_PluginsBrowser and friends; refpairs/p1): the
/// browser starts below the 26 pt title bar.
#[test]
fn browser_matches_console_geometry() {
    let assets = assets();
    let model = PreviewMixer::default();
    let view = ViewState::default();
    let (hits, scrolls) = build(&assets, &model, &view);
    let rect = |c: Control| {
        let r = find(&hits, "browser", &c).unwrap_or_else(|| panic!("{c:?}")).rect;
        (r.x, r.y, r.w, r.h)
    };
    use rack::surfaces::BrowserTab;
    assert_eq!(rect(Control::BrowserTab(BrowserTab::Plugins)), (5.0, 32.0, 167.0, 26.0));
    assert_eq!(rect(Control::BrowserTab(BrowserTab::Presets)), (172.0, 32.0, 168.0, 26.0));
    assert_eq!(rect(Control::BrowserSearch), (5.0, 63.0, 335.0, 28.0));
    // Category cells: (w − 5) / 3 − 4 wide, the last column to w − 6; 23 tall, 27 apart.
    assert_eq!(rect(Control::BrowserCategory(0)), (10.0, 126.0, 106.0, 23.0));
    assert_eq!(rect(Control::BrowserCategory(5)), (230.0, 153.0, 104.0, 23.0));
    assert_eq!(rect(Control::BrowserCategory(9)), (10.0, 207.0, 106.0, 23.0));
    assert_eq!(rect(Control::BrowserLargeRows), (7.0, 241.0, 24.0, 23.0));
    assert_eq!(rect(Control::BrowserHintClose), (315.0, 271.0, 20.0, 20.0));
    // List viewport below the hint, 5 pt inside the well; groups and 48 pt rows.
    let list = scrolls.iter().find(|s| s.target == ScrollTarget::Browser).expect("list");
    assert_eq!((list.rect.x, list.rect.y, list.rect.w, list.rect.h), (5.0, 330.0, 335.0, 670.0));
    assert_eq!(rect(Control::BrowserGroupReveal), (5.0, 330.0, 317.0, 30.0));
    let first = model.plugin_catalog().iter().position(|p| p.enabled).expect("an active plug-in");
    assert_eq!(rect(Control::BrowserItem(first as u16)), (5.0, 360.0, 317.0, 48.0));
    assert!(find(&hits, "browser", &Control::BrowserInactiveReveal).is_some());
    let thumb = rect(Control::BrowserScroll);
    assert_eq!((thumb.0, thumb.1, thumb.2), (322.0, 330.0, 18.0));
}
