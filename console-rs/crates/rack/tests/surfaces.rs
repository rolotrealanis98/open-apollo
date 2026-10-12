//! UI-surface behaviour without UA assets: a minimal fixture Appearance (a few skins, no
//! images) is enough for every hit region to be registered.

use std::path::PathBuf;

use houston::Resources;
use rack::input::{Action, Control, ControlId, Hit, Interaction, ScrollTarget};
use rack::surfaces::{Modifier, MonitorPage, Popover, PopoverState, Section};
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
    // Console's monitor column has no DIM / TALK; they are on the CTRL ROOM page.
    assert!(find(&hits, "monitor", &Control::MonitorDim).is_none());
    let (cr_hits, _) = build(&assets, &model, &ViewState { monitor_page: MonitorPage::ControlRoom, ..ViewState::default() });
    assert!(find(&cr_hits, "monitor", &Control::MonitorDim).is_some());
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
