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
    // Tests run in parallel threads: write the fixture once, then only read it.
    static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    let dir = DIR.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("rack-surfaces-fixture-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("appearance")).unwrap();
        std::fs::write(dir.join("appearance/Appearance.json"), FIXTURE).unwrap();
        dir
    });
    let res = Resources::at(dir).expect("fixture resources");
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
    // Sends open (Console's default is collapsed), so the cells overflow and scroll from the top.
    let mut view = ViewState::default();
    view.sections.sends = true;
    let (hits, _) = build(&assets, &model, &view);
    let slot = find(&hits, "in0", &Control::Insert(0)).expect("insert slot").rect;
    // Inputs row 237 + list top 26 + 5 + 1 under the column top (72) + swatch (5).
    assert_eq!(slot.y, 72.0 + 5.0 + 237.0 + 26.0 + 6.0);
    let toggle = find(&hits, "nav", &Control::Section(Section::Inputs)).unwrap().clone();
    tap(&hits, &toggle, &mut view, &mut model);
    assert!(!view.sections.inputs);
    let (hits, _) = build(&assets, &model, &view);
    let slot = find(&hits, "in0", &Control::Insert(0)).unwrap().rect;
    // Collapsed = header only; with Sends open the stack still overflows and scrolls from the top.
    assert_eq!(slot.y, 72.0 + 5.0 + 22.0 + 26.0 + 6.0, "collapsed = header only");
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
            console_model: None,
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
    assert_eq!(rect(Control::BrowserGroupReveal), (5.0, 330.0, 317.0, 32.0));
    let first = model.plugin_catalog().iter().position(|p| p.enabled).expect("an active plug-in");
    assert_eq!(rect(Control::BrowserItem(first as u16)), (5.0, 362.0, 317.0, 48.0));
    // Demo-able plug-ins (DEMO badge) close the "Universal Audio" group after the licensed
    // ones (refpairs/p2); only the rest go to "Universal Audio (Inactive)".
    let catalog = model.plugin_catalog();
    let licensed = catalog.iter().filter(|p| p.enabled).count();
    let first_demo = catalog.iter().position(|p| !p.enabled && p.badge == Some(rack::PluginBadge::Demo)).expect("a demo-able plug-in");
    assert_eq!(rect(Control::BrowserItem(first_demo as u16)).1, 362.0 + 48.0 * licensed as f32);
    assert!(catalog.iter().all(|p| p.enabled || p.badge == Some(rack::PluginBadge::Demo)));
    assert!(find(&hits, "browser", &Control::BrowserInactiveReveal).is_none());
    let thumb = rect(Control::BrowserScroll);
    assert_eq!((thumb.0, thumb.1, thumb.2), (322.0, 330.0, 18.0));
}

/// Status bar as `Rack_DSPInfo` / `Rack_InfoSection` draw it: loads rounded half up from the raw
/// percent (p2: 10.70 / 11.24 → 11% / 11%; p2 user shot: 7.21 / 4.55 → 7% / 5%), and SCENE only
/// with plug-in scenes and room for it.
#[test]
fn status_bar_loads_and_scene() {
    let assets = assets();
    let texts = |model: &PreviewMixer, view: &ViewState| {
        let mut ui = Ui::new(&assets);
        build_window_with(&mut ui, model, view);
        let texts: Vec<String> = ui
            .list
            .items
            .iter()
            .filter_map(|d| match d {
                rack::Draw::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect();
        (texts, ui.hits)
    };
    let mut model = PreviewMixer::default();
    let view = ViewState { window: Some((1470.0, 1105.0)), ..ViewState::default() };
    (model.status.dsp_load, model.status.pgm_load, model.status.mem_load) = (10.7025, 11.2427, 0.0369);
    let (t, hits) = texts(&model, &view);
    assert_eq!(t.iter().filter(|s| *s == "11%").count(), 2);
    assert!(t.iter().any(|s| s == "0%") && t.iter().any(|s| s == "SCENE"));
    assert!(find(&hits, "status", &Control::StatusScene).is_some());
    (model.status.dsp_load, model.status.pgm_load) = (7.2075, 4.5502);
    let (t, _) = texts(&model, &view);
    assert!(t.iter().any(|s| s == "7%") && t.iter().any(|s| s == "5%"));
    model.status.plugin_scenes = false;
    let (t, hits) = texts(&model, &view);
    assert!(!t.iter().any(|s| s == "SCENE" || s == "(NONE)"));
    assert!(find(&hits, "status", &Control::StatusScene).is_none());
}

fn press(view: &mut ViewState, model: &mut PreviewMixer, hits: &[Hit], strip: &str, control: Control) {
    let h = find(hits, strip, &control).unwrap_or_else(|| panic!("no hit for {strip} {control:?}")).clone();
    tap(hits, &h, view, model);
}

#[test]
fn settings_tabs_combos_and_text_fields() {
    use rack::settings::{PropertySource, SettingsTab};
    let assets = assets();
    let mut model = PreviewMixer::default();
    let mut view = ViewState::default();
    assert!(view.open_named("settings"));
    let (hits, _) = build(&assets, &model, &view);
    for t in [SettingsTab::Hardware, SettingsTab::IoMatrix, SettingsTab::Options, SettingsTab::Midi, SettingsTab::Network] {
        assert!(find(&hits, "settings", &Control::SettingsTab(t)).is_some(), "tab {t:?}");
    }
    // SAMPLE RATE: open the dropdown, pick 96 kHz; the dropdown closes and the model follows.
    press(&mut view, &mut model, &hits, "settings", Control::SettingMenu("/SampleRate".into()));
    assert_eq!(view.settings.menu.as_deref(), Some("/SampleRate"));
    let (hits, _) = build(&assets, &model, &view);
    press(&mut view, &mut model, &hits, "settings", Control::SettingChoose("/SampleRate".into(), 3));
    assert!(view.settings.menu.is_none());
    assert_eq!(model.props.prop("/SampleRate").and_then(|v| v.as_i64()), Some(96000));
    assert_eq!(model.status.sample_rate, Some(96000));
    // A tap outside an open dropdown closes it and keeps the window.
    press(&mut view, &mut model, &hits, "settings", Control::SettingMenu("/ClockSource".into()));
    let (hits, _) = build(&assets, &model, &view);
    let top = hits.iter().rev().find(|h| h.rect.contains(2.0, 1040.0)).unwrap().clone();
    assert_eq!(top.id.control, Control::PopoverDismiss);
    tap(&hits, &top, &mut view, &mut model);
    assert!(view.settings.menu.is_none());
    assert_eq!(view.sheet, Some(rack::surfaces::Sheet::Settings));
    // DEVICE NAME: focus, type, Enter.
    let (hits, _) = build(&assets, &model, &view);
    press(&mut view, &mut model, &hits, "settings", Control::SettingEdit("/devices/0/DeviceName".into()));
    assert_eq!(view.text_edit.as_ref().map(|t| t.text.as_str()), Some("Apollo x4"));
    for _ in 0..2 {
        view.key_backspace();
    }
    view.key_text("8p");
    view.key_enter(&mut model);
    assert_eq!(model.props.prop("/devices/0/DeviceName").and_then(|v| v.as_str().map(str::to_owned)).as_deref(), Some("Apollo 8p"));
    // OPTIONS page: METERING → POST-FADER.
    let (hits, _) = build(&assets, &model, &view);
    press(&mut view, &mut model, &hits, "settings", Control::SettingsTab(SettingsTab::Options));
    let (hits, _) = build(&assets, &model, &view);
    press(&mut view, &mut model, &hits, "settings", Control::SettingMenu("map:metering:/PostFaderMetering".into()));
    let (hits, _) = build(&assets, &model, &view);
    press(&mut view, &mut model, &hits, "settings", Control::SettingChoose("map:metering:/PostFaderMetering".into(), 1));
    assert_eq!(model.props.prop("/PostFaderMetering").and_then(|v| v.as_bool()), Some(true));
    // Escape closes the window.
    view.key_escape();
    assert!(view.sheet.is_none());
}

#[test]
fn monitor_buttons_open_their_panels() {
    use rack::settings::{Panel, PropertySource};
    use rack::surfaces::Sheet;
    let assets = assets();
    let mut model = PreviewMixer::default();
    let mut view = ViewState::default();
    let (hits, _) = build(&assets, &model, &view);
    // CUE OUTPUTS opens the anchored panel; MIX on CUE 2 sets its MixInSource to the cue mix.
    press(&mut view, &mut model, &hits, "monitor", Control::CueOutputs);
    assert_eq!(view.sheet, Some(Sheet::Panel(Panel::CueOutputs)));
    let (hits, _) = build(&assets, &model, &view);
    press(&mut view, &mut model, &hits, "settings", Control::SettingChoose("map:cue_source:/devices/0/outputs/15/MixInSource".into(), 1));
    assert_eq!(model.props.prop("/devices/0/outputs/15/MixInSource").and_then(|v| v.as_str().map(str::to_owned)).as_deref(), Some("cue"));
    // A tap outside closes it.
    let (hits, _) = build(&assets, &model, &view);
    let outside = hits.iter().rev().find(|h| h.rect.contains(2.0, 1040.0)).unwrap().clone();
    tap(&hits, &outside, &mut view, &mut model);
    assert!(view.sheet.is_none());
    // MONITOR CONTROLLER and its tabs; BASS MANAGEMENT ENABLE toggles the device property.
    let (hits, _) = build(&assets, &model, &view);
    press(&mut view, &mut model, &hits, "monitor", Control::ShowPanel(Panel::MonitorController));
    assert_eq!(view.sheet, Some(Sheet::Panel(Panel::MonitorController)));
    let (hits, _) = build(&assets, &model, &view);
    press(&mut view, &mut model, &hits, "settings", Control::SettingPress("toggle:/devices/0/BassMgrEnable".into()));
    assert_eq!(model.props.prop("/devices/0/BassMgrEnable").and_then(|v| v.as_bool()), Some(true));
    let (hits, _) = build(&assets, &model, &view);
    press(&mut view, &mut model, &hits, "settings", Control::PanelTab(1));
    assert_eq!(view.settings.panel_tab, 1);
    let (hits, _) = build(&assets, &model, &view);
    press(&mut view, &mut model, &hits, "settings", Control::PanelClose);
    assert!(view.sheet.is_none());
    // HEADPHONES (MONITOR CORRECTION) opens HEADPHONE CORRECTION.
    let (hits, _) = build(&assets, &model, &view);
    press(&mut view, &mut model, &hits, "monitor", Control::ShowPanel(Panel::HeadphoneCorrection));
    assert_eq!(view.sheet, Some(Sheet::Panel(Panel::HeadphoneCorrection)));
}

#[test]
fn plugin_scenes_panel_primes_and_recalls() {
    use rack::settings::Panel;
    use rack::surfaces::Sheet;
    let assets = assets();
    let mut model = PreviewMixer::default();
    let mut view = ViewState::default();
    let (hits, _) = build(&assets, &model, &view);
    press(&mut view, &mut model, &hits, "nav", Control::ShowPluginScenes);
    assert_eq!(view.sheet, Some(Sheet::Panel(Panel::PluginScenes)));
    let (hits, _) = build(&assets, &model, &view);
    assert!(find(&hits, "global", &Control::RecallScene(0)).is_none(), "RECALL needs a primed scene");
    press(&mut view, &mut model, &hits, "settings", Control::SceneSelect(1));
    assert_eq!(view.settings.scene, Some(1));
    let (hits, _) = build(&assets, &model, &view);
    press(&mut view, &mut model, &hits, "settings", Control::SceneStep(false));
    assert_eq!(view.settings.scene, Some(2));
    let (hits, _) = build(&assets, &model, &view);
    press(&mut view, &mut model, &hits, "global", Control::RecallScene(2));
    assert_eq!(model.status.scene.as_deref(), Some(model.scenes[2].as_str()));
    // The navigator button closes it again.
    let (hits, _) = build(&assets, &model, &view);
    press(&mut view, &mut model, &hits, "nav", Control::ShowPluginScenes);
    assert!(view.sheet.is_none());
}
