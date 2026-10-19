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
        "Switch-Tight-Kerning": {}, "Switch-Red-Tight-Kerning": {}, "Mini-Send-Mute": {}, "Mini-Send-Pre": {}, "Send-Mute": {},
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
    let (cr_hits, _) = build(&assets, &model, &ViewState { show_ctrl_room: true, ..ViewState::default() });
    assert!(find(&cr_hits, "monitor", &Control::MonitorDim).is_some());
    assert!(find(&cr_hits, "tb", &Control::Fader).is_some(), "CTRL ROOM adds the talkback strip");
    assert!(find(&hits, "in0", &Control::Name).is_some());
    assert!(scrolls.iter().any(|s| s.target == ScrollTarget::Browser && s.max > 0.0));
    assert!(scrolls.iter().any(|s| s.target == ScrollTarget::Cells && s.max > 0.0), "sends overflow the cells column");
}

/// The INS switch beside the UAD REC/MON LEDs flips the input's `RecordPreEffects`, as the
/// LEDs do (`Rack_UADRecord` has one state; INS is its press target).
#[test]
fn ins_switch_toggles_record_pre_effects() {
    let assets = assets();
    let mut model = PreviewMixer::default();
    let mut view = ViewState::default();
    let (hits, _) = build(&assets, &model, &view);
    // INS is the 40-pt `Switch` (the fixture has no LED art, so the LEDs add no hit).
    let ins = hits.iter().find(|h| &*h.id.strip == "in0" && h.id.control == Control::RecMon && h.rect.w == 40.0).expect("INS hit");
    let s = model.inputs().into_iter().find(|s| &*s.id == "in0").unwrap();
    let before = model.records_pre_effects(&s);
    tap(&hits, ins, &mut view, &mut model);
    assert_eq!(model.records_pre_effects(&s), !before);
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
    // Input sends are reachable now: full send strips (route, pan, M, fader; 1.3.1 s45).
    assert!(find(&hits, "in0", &Control::SendMute(0)).is_some());
    assert!(find(&hits, "in0", &Control::SendPan(0)).is_some());
    assert!(find(&hits, "in0", &Control::SendLevel(0)).is_some());
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
    assert!(view.show_aux && view.show_ctrl_room, "SHOW AUX and CTRL ROOM are independent");
    let (hits, _) = build(&assets, &model, &view);
    assert!(find(&hits, "monitor", &Control::CrSource(0)).is_some());
    assert!(find(&hits, "aux0", &Control::Fader).is_some(), "the aux strips stay");
    assert!(find(&hits, "tb", &Control::Fader).is_some());
    let aux = find(&hits, "monitor", &Control::MonitorPage(rack::surfaces::MonitorPage::Aux)).unwrap().clone();
    tap(&hits, &aux, &mut view, &mut model);
    let (hits, _) = build(&assets, &model, &view);
    assert!(find(&hits, "aux0", &Control::Fader).is_none(), "SHOW AUX off hides the aux strips");

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
    assert!(matches!(view.menu.as_ref().map(|m| &m.target), Some(rack::menu::MenuTarget::Setting(k)) if &**k == "/SampleRate"));
    let (hits, _) = build(&assets, &model, &view);
    press(&mut view, &mut model, &hits, "settings", Control::SettingChoose("/SampleRate".into(), 3));
    assert!(view.menu.is_none());
    assert_eq!(model.props.prop("/SampleRate").and_then(|v| v.as_i64()), Some(96000));
    assert_eq!(model.status.sample_rate, Some(96000));
    // A tap outside an open dropdown closes it and keeps the window.
    press(&mut view, &mut model, &hits, "settings", Control::SettingMenu("/ClockSource".into()));
    let (hits, _) = build(&assets, &model, &view);
    let top = hits.iter().rev().find(|h| h.rect.contains(2.0, 1040.0)).unwrap().clone();
    assert_eq!(top.id.control, Control::PopoverDismiss);
    tap(&hits, &top, &mut view, &mut model);
    assert!(view.menu.is_none());
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

/// CUE OUTPUTS: an empty cell in a cue's column puts that HP on the cue; the lit tile opens the
/// HP's source menu (MON, CUE 1…4, NONE), which closes on a pick and keeps the panel.
#[test]
fn cue_outputs_headphone_tiles_pick_the_source() {
    use rack::settings::Panel;
    use rack::surfaces::Sheet;
    let assets = assets();
    let mut model = PreviewMixer::default();
    let mut view = ViewState::default();
    let (hits, _) = build(&assets, &model, &view);
    press(&mut view, &mut model, &hits, "monitor", Control::CueOutputs);
    let (hits, _) = build(&assets, &model, &view);
    // HP 1 plays CUE 1 (lit tile → menu); its CUE 2 cell assigns CUE 2 (option 2).
    assert!(find(&hits, "monitor", &Control::HeadphoneMenu(0)).is_some());
    assert!(find(&hits, "monitor", &Control::HeadphoneSource { output: 1, option: 2 }).is_none(), "HP 2's lit cell is a menu");
    press(&mut view, &mut model, &hits, "monitor", Control::HeadphoneSource { output: 0, option: 2 });
    assert_eq!(model.headphones[0].source, 2);
    let (hits, _) = build(&assets, &model, &view);
    assert!(find(&hits, "monitor", &Control::HeadphoneSource { output: 0, option: 1 }).is_some());
    press(&mut view, &mut model, &hits, "monitor", Control::HeadphoneMenu(0));
    assert!(matches!(view.popover, Some(PopoverState { kind: Popover::Headphone { output: 0 }, .. })));
    let (hits, _) = build(&assets, &model, &view);
    press(&mut view, &mut model, &hits, "monitor", Control::HeadphoneSource { output: 0, option: 5 });
    assert_eq!(model.headphones[0].source, 5, "NONE");
    assert!(view.popover.is_none());
    assert_eq!(view.sheet, Some(Sheet::Panel(Panel::CueOutputs)));
    // A tap outside an open HP menu closes the menu only.
    let (hits, _) = build(&assets, &model, &view);
    press(&mut view, &mut model, &hits, "monitor", Control::HeadphoneSource { output: 0, option: 1 });
    let (hits, _) = build(&assets, &model, &view);
    press(&mut view, &mut model, &hits, "monitor", Control::HeadphoneMenu(0));
    let (hits, _) = build(&assets, &model, &view);
    let outside = hits.iter().rev().find(|h| h.rect.contains(2.0, 1040.0)).unwrap().clone();
    tap(&hits, &outside, &mut view, &mut model);
    assert!(view.popover.is_none());
    assert_eq!(view.sheet, Some(Sheet::Panel(Panel::CueOutputs)));
}

/// ALT COUNT 1: one ALT button and one ALT TRIM knob in the control room; 2: two of each.
#[test]
fn alt_trim_knobs_follow_the_alt_count() {
    let assets = assets();
    let mut model = PreviewMixer::default();
    let mut view = ViewState::default();
    view.show_ctrl_room = true;
    let (hits, _) = build(&assets, &model, &view);
    assert!(find(&hits, "monitor", &Control::AltTrim(0)).is_none(), "ALT COUNT 0");
    model.monitor_options.alt_count = 1;
    let (hits, _) = build(&assets, &model, &view);
    assert!(find(&hits, "monitor", &Control::MonitorAlt(1)).is_some());
    assert!(find(&hits, "monitor", &Control::MonitorAlt(2)).is_none());
    assert!(find(&hits, "monitor", &Control::AltTrim(1)).is_none());
    let h = find(&hits, "monitor", &Control::AltTrim(0)).expect("ALT 1 TRIM").clone();
    assert!(matches!(h.kind, rack::input::HitKind::Knob { reset: Some(r), .. } if r == 0.5), "0 dB default");
    // Drag up 30 pt over a 120-pt span: +0.25 → +15 dB.
    let (cx, cy) = h.rect.center();
    let mut i = Interaction::default();
    for a in i.pointer_down(&hits, cx, cy, false) {
        view.dispatch(a, &mut model);
    }
    for a in i.pointer_move(cx, cy - 30.0, false) {
        view.dispatch(a, &mut model);
    }
    for a in i.pointer_up() {
        view.dispatch(a, &mut model);
    }
    assert!((model.monitor_options.alt_trim[0] - 0.75).abs() < 1e-9);
    model.monitor_options.alt_count = 2;
    let (hits, _) = build(&assets, &model, &view);
    assert!(find(&hits, "monitor", &Control::AltTrim(1)).is_some());
}

/// CTRL ROOM SOURCE: CUE 1 / CUE 2 with two cues; CUE 3 / CUE 4 appear with CUE BUS COUNT 4.
#[test]
fn control_room_source_follows_the_cue_bus_count() {
    let assets = assets();
    let mut model = PreviewMixer::default();
    let mut view = ViewState::default();
    view.show_ctrl_room = true;
    let (hits, _) = build(&assets, &model, &view);
    assert!(find(&hits, "monitor", &Control::CrSource(2)).is_some());
    assert!(find(&hits, "monitor", &Control::CrSource(3)).is_none());
    for k in [3, 4] {
        model.cues.push(rack::CueOutput { name: format!("CUE {k}"), source_cue: false, mono: false, destination: "NONE".into() });
    }
    let (hits, _) = build(&assets, &model, &view);
    let c3 = find(&hits, "monitor", &Control::CrSource(3)).expect("CUE 3").rect;
    let c4 = find(&hits, "monitor", &Control::CrSource(4)).expect("CUE 4").rect;
    let c1 = find(&hits, "monitor", &Control::CrSource(1)).unwrap().rect;
    assert!(c3.y > c1.y && c3.x == c1.x && c4.y == c3.y, "a second row under CUE 1 / CUE 2");
    press(&mut view, &mut model, &hits, "monitor", Control::CrSource(4));
    assert_eq!(model.monitor_options.cr_source, 4);
}

/// Settings › CONTROLS MODE: Linear drags knobs vertically; Circular jumps a knob to the
/// pointer's angle; Relative Circular turns it by the angle swept. Faders stay linear.
#[test]
fn controls_mode_drives_knob_drags() {
    use rack::ControlsMode;
    use rack::settings::{PropValue, Write};
    let assets = assets();
    let mut model = PreviewMixer::default();
    let view = ViewState::default();
    assert_eq!(ControlsMode::of(&model), ControlsMode::Linear);
    let (hits, _) = build(&assets, &model, &view);
    let pan = find(&hits, "in0", &Control::PanL).expect("pan knob").clone();
    let fader = find(&hits, "in0", &Control::Fader).expect("fader").clone();
    let rack::input::HitKind::Knob { value: v0, span, .. } = pan.kind else { panic!("{:?}", pan.kind) };
    let (cx, cy) = pan.rect.center();
    let r = pan.rect.w.min(pan.rect.h) / 2.0 - 1.0;
    let set = |acts: Vec<Action>| acts.into_iter().filter_map(|a| if let Action::Set(_, v) = a { Some(v) } else { None }).last();
    // Linear: up by span/4 → +0.25.
    let mut i = Interaction::default();
    i.pointer_down(&hits, cx, cy, false);
    assert!((set(i.pointer_move(cx, cy - span / 4.0, false)).unwrap() - (v0 + 0.25).min(1.0)).abs() < 1e-6);
    i.pointer_up();
    // Circular: pressing at 3 o'clock jumps to 90° of the 270° sweep.
    model.apply_setting_writes(vec![Write::Set("/ControlsMode".into(), PropValue::Str("Circular".into()))]);
    let mut i = Interaction::default();
    i.controls_mode = ControlsMode::of(&model);
    assert_eq!(i.controls_mode, ControlsMode::Circular);
    let v = set(i.pointer_down(&hits, cx + r, cy, false)).expect("jump on press");
    assert!((v - (90.0 / 270.0 + 0.5)).abs() < 1e-6, "{v}");
    // … and 9 o'clock.
    assert!((set(i.pointer_move(cx - r, cy, false)).unwrap() - (0.5 - 90.0 / 270.0)).abs() < 1e-6);
    i.pointer_up();
    // A fader in Circular mode is still a linear drag.
    let (fx, fy) = fader.rect.center();
    let rack::input::HitKind::Drag { value: f0, span: fspan, .. } = fader.kind else { panic!() };
    assert!(set(i.pointer_down(&hits, fx, fy, false)).is_none());
    assert!((set(i.pointer_move(fx, fy - fspan / 10.0, false)).unwrap() - (f0 + 0.1).min(1.0)).abs() < 1e-6);
    i.pointer_up();
    // Relative Circular: no jump; a quarter turn (12 → 3 o'clock, crossing nothing) adds 90/270.
    model.apply_setting_writes(vec![Write::Set("/ControlsMode".into(), PropValue::Str("Relative Circular".into()))]);
    let mut i = Interaction::default();
    i.controls_mode = ControlsMode::of(&model);
    assert!(set(i.pointer_down(&hits, cx, cy - r, false)).is_none());
    i.pointer_move(cx + r * 0.7071, cy - r * 0.7071, false);
    let v = set(i.pointer_move(cx + r, cy, false)).unwrap();
    assert!((v - (v0 + 90.0 / 270.0).min(1.0)).abs() < 1e-4, "{v}");
    // Turning back past the start goes below it.
    i.pointer_move(cx, cy - r, false);
    let v = set(i.pointer_move(cx - r, cy, false)).unwrap();
    assert!((v - (v0 - 90.0 / 270.0).max(0.0)).abs() < 1e-4, "{v}");
}

#[test]
fn plugin_scenes_panel_primes_and_recalls() {
    let assets = assets();
    let mut model = PreviewMixer::default();
    let mut view = ViewState::default();
    let (hits, _) = build(&assets, &model, &view);
    press(&mut view, &mut model, &hits, "nav", Control::ShowPluginScenes);
    assert!(view.show_scenes, "PLUG-IN SCENES is the window's column (s10), not a sheet");
    assert!(view.sheet.is_none());
    let (hits, _) = build(&assets, &model, &view);
    assert!(find(&hits, "global", &Control::SceneCommand(rack::menu::SceneOp::Recall, 0)).is_none(), "RECALL needs a primed scene");
    press(&mut view, &mut model, &hits, "settings", Control::SceneSelect(1));
    assert_eq!(view.settings.scene, Some(1));
    let (hits, _) = build(&assets, &model, &view);
    press(&mut view, &mut model, &hits, "settings", Control::SceneStep(false));
    assert_eq!(view.settings.scene, Some(2));
    let (hits, _) = build(&assets, &model, &view);
    press(&mut view, &mut model, &hits, "global", Control::SceneCommand(rack::menu::SceneOp::Recall, 2));
    assert_eq!(model.status.scene.as_deref(), Some(model.scenes[2].as_str()));
    // The navigator button closes it again.
    let (hits, _) = build(&assets, &model, &view);
    press(&mut view, &mut model, &hits, "nav", Control::ShowPluginScenes);
    assert!(!view.show_scenes);
}

#[test]
fn view_specs_and_1_3_1_view_modes() {
    let assets = assets();
    let model = PreviewMixer::default();
    let mut view = ViewState::default();
    view.apply_spec("ctrl=1,sends=1,overview=1,small,presets-open=Bass|Guitar,tab=presets").unwrap();
    assert!(view.show_ctrl_room && view.show_aux && view.sends_overview && view.sections.sends);
    assert_eq!(view.browser.presets_open, vec!["Bass".to_string(), "Guitar".to_string()]);
    assert!(view.apply_spec("nope=1").is_err());
    let (hits, _) = build(&assets, &model, &view);
    // SMALL shrinks the Unison slot with the insert slots (s47/s48).
    assert_eq!(find(&hits, "in0", &Control::Insert(255)).unwrap().rect.h, 21.0);
    // CTRL ROOM: the talkback strip and the control-room column's SOURCE buttons.
    assert!(find(&hits, "tb", &Control::Fader).is_some());
    assert!(find(&hits, "monitor", &Control::CrSource(1)).is_some());
    // PRESETS: folders toggle open.
    let folder = find(&hits, "browser", &Control::BrowserPresetFolder(1)).unwrap().clone();
    let mut model = model;
    tap(&hits, &folder, &mut view, &mut model);
    assert!(view.browser.presets_open.iter().any(|n| n == "Drums"));
    // Assign mode targets a slot; the next catalog tap loads into it.
    view.begin_assign("aux0".into(), 1);
    assert!(view.show_browser && view.browser.assign_to.is_some());
}
