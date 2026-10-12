//! Secondary windows Console opens from the mixer, drawn from [`crate::settings`] fields:
//! - CUE OUTPUTS (`Rack_CuesPopover` › `Rack_CuesAssignments`): per device, one
//!   `Rack_CueRoute` per active cue — name, SOURCE (MIX / MON), MONO, MIRROR TO.
//! - MONITOR CONTROLLER (`Rack_MonitorSubWindow`): the monitor knob section, BASS MANAGEMENT /
//!   SPEAKER UTILITIES tabs and MAIN MONITOR CORRECTION (`Rack_RoomCorrectionSection`).
//! - HEADPHONE CORRECTION (`HPCorrection` sub-window): HP 1 / HP 2 correction sections.
//! - PLUG-IN SCENES (`Rack_SceneSection`): CURRENT / PRIMED, the scene list, SAVE / RECALL.
//! - About (`About-Background`, `About-Product_Logo`, `About-UA-Logo`, `About-Box-Version`).
//!
//! [obs] Component ids, labels and skins: the constructors at Win Rack.dll 0x180d26bf0
//! (cues), 0x180d1f610 (cue route), 0x181145940 (monitor sub-window), 0x181122330 (headphone
//! correction), 0x1811f5820 (scene section), 0x180a23b90 / 0x180a24830 (About). [I] Sizes and
//! placement until 1.3.1 captures of each window exist.

use houston::{Rect, Rgba};

use crate::draw::Align;
use crate::input::{Control, ControlId, HitKind, ScrollTarget};
use crate::model::MixerModel;
use crate::settings::{self, FieldKind, Panel, SettingField, SettingsGroup};
use crate::surfaces::popover::Placement;
use crate::surfaces::settings::{CELL_PITCH_X, Drawn, choice_box, choice_menu, field_grid, section_header, sid, window_frame};
use crate::surfaces::view::ViewState;
use crate::ui::Ui;

/// Draws the open panel. `browser` is the plug-in browser column, which PLUG-IN SCENES covers
/// when it is shown.
pub fn draw_panel(ui: &mut Ui, model: &dyn MixerModel, view: &ViewState, panel: Panel, bounds: Rect, browser: Option<Rect>) {
    let mut drawn = Vec::new();
    match panel {
        Panel::CueOutputs => cue_outputs(ui, model, view, bounds, &mut drawn),
        Panel::MonitorController => monitor_controller(ui, model, view, bounds, &mut drawn),
        Panel::HeadphoneCorrection => headphone_correction(ui, model, view, bounds, &mut drawn),
        Panel::PluginScenes => plugin_scenes(ui, model, view, bounds, browser),
        Panel::About => about(ui, model, bounds),
    }
    choice_menu(ui, view, &drawn, bounds);
}

fn device_of(model: &dyn MixerModel, view: &ViewState) -> u8 {
    let Some(src) = model.properties() else { return 0 };
    let devs = settings::devices(src);
    devs.iter().find(|d| d.index == view.settings.device).or(devs.first()).map(|d| d.index).unwrap_or(0)
}

/// A centred, non-modal sub-window (`Rack_SubWindow`) of `size`; returns its content rect.
fn sub_window(ui: &mut Ui, bounds: Rect, size: (f32, f32), title: &str) -> Rect {
    let (w, h) = (size.0.min(bounds.w - 20.0), size.1.min(bounds.h - 20.0));
    let r = Rect::new(bounds.x + ((bounds.w - w) / 2.0).round(), bounds.y + ((bounds.h - h) / 2.5).round(), w, h);
    window_frame(ui, r, title, sid(Control::PanelClose))
}

/// `Tab-Section-Left` / `-Right` (or `-Single`) pair; returns the content top.
fn section_tabs(ui: &mut Ui, labels: &[&str], selected: u8, x: f32, y: f32) -> f32 {
    let (w, h) = (131.0, 20.0);
    for (i, label) in labels.iter().enumerate() {
        let key = match (labels.len(), i) {
            (1, _) => "Tab-Section-Single",
            (_, 0) => "Tab-Section-Left",
            _ => "Tab-Section-Right",
        };
        let r = Rect::new(x + i as f32 * w, y, w, h);
        let on = selected as usize == i;
        let skin = ui.skin("views", key);
        let img = skin.and_then(|s| if on { s.image("image-on").or(s.image("image")) } else { s.image("image-off").or(s.image("image")) }).map(str::to_owned);
        let color = skin.and_then(|s| if labels.len() == 1 { s.color("color-text") } else if on { s.color("color-text-on") } else { s.color("color-text-off") });
        match img {
            Some(img) => ui.image_stretched(&img, r, 1.0),
            None => ui.fill(r, if on { Rgba::rgb8(70, 74, 77) } else { Rgba::rgb8(40, 42, 45) }),
        }
        let mut style = ui.style("views", "Silk-Screen");
        style.color = color.unwrap_or(Rgba::WHITE.with_alpha(if on { 0.8 } else { 0.25 }));
        ui.text(label, style, r, Align::Center);
        if labels.len() > 1 {
            ui.hit(r, sid(Control::PanelTab(i as u8)), HitKind::Tap);
        }
    }
    y + h
}

// ---- CUE OUTPUTS ----

const CUE_ROW_H: f32 = 46.0;

fn cue_outputs(ui: &mut Ui, model: &dyn MixerModel, _view: &ViewState, bounds: Rect, drawn: &mut Vec<Drawn>) {
    let Some(src) = model.properties() else { return };
    let devices = settings::devices(src);
    let groups: Vec<(String, Vec<SettingField>)> = devices.iter().map(|d| (d.name.to_uppercase(), settings::fields(src, SettingsGroup::CueRoutes(d.index)))).collect();
    let rows: usize = groups.iter().map(|(_, f)| f.iter().map(|f| f.row).max().map_or(0, |m| m as usize + 1)).sum();
    let h = groups.len() as f32 * (26.0 + 16.0) + rows.max(1) as f32 * CUE_ROW_H + 8.0;
    let anchor = ui.hit_rect(&ControlId { strip: "monitor".into(), control: Control::CueOutputs }).unwrap_or_else(|| {
        let (cx, cy) = bounds.center();
        Rect::new(cx, cy, 1.0, 1.0)
    });
    ui.popover_catcher(bounds);
    let c = ui.popover_frame(anchor, (440.0, h), Placement::Left, bounds, Some("CUE OUTPUTS"));
    if rows == 0 {
        let note = Ui::plain_style("Helvetica", 11.0, false, Rgba::WHITE.with_alpha(0.5));
        ui.text("No cue outputs", note, c, Align::Center);
        return;
    }
    let mut y = c.y + 4.0;
    let label_style = ui.style("views", "Label-Button");
    for (name, fields) in groups {
        section_header(ui, &name, Rect::new(c.x + 4.0, y, c.w - 8.0, 26.0));
        y += 26.0;
        // Column heads (`Label-Button`): SOURCE, MIX, OUTPUT.
        ui.text("SOURCE", label_style.clone(), Rect::new(c.x + 96.0, y, 84.0, 16.0), Align::Center);
        ui.text("MIX", label_style.clone(), Rect::new(c.x + 186.0, y, 48.0, 16.0), Align::Center);
        ui.text("OUTPUT", label_style.clone(), Rect::new(c.x + 244.0, y, c.w - 252.0, 16.0), Align::Center);
        y += 16.0;
        let last = fields.iter().map(|f| f.row).max().unwrap_or(0);
        for row in 0..=last {
            let line: Vec<&SettingField> = fields.iter().filter(|f| f.row == row).collect();
            let ry = y + row as f32 * CUE_ROW_H;
            if row > 0 {
                ui.fill(Rect::new(c.x + 8.0, ry, c.w - 16.0, 1.0), Rgba::WHITE.with_alpha(0.06));
            }
            for f in line {
                match (&f.kind, f.label.as_str()) {
                    (FieldKind::Readout(_), name) => {
                        let mut st = ui.style("fonts", "Settings-Section-Header-Static-Text");
                        st.color = Rgba::WHITE.with_alpha(0.7);
                        ui.text(name, st, Rect::new(c.x + 12.0, ry, 80.0, CUE_ROW_H), Align::Left);
                    }
                    (FieldKind::Choice { selected, .. }, "SOURCE") => {
                        // Two `Switch` buttons: MIX (the cue's own mix), MON (the monitor mix).
                        for (i, label) in ["MIX", "MON"].iter().enumerate() {
                            let b = Rect::new(c.x + 96.0 + i as f32 * 42.0, ry + 3.0, 40.0, 40.0);
                            let on = *selected == Some(i);
                            ui.toggle("Switch", Some(label), on, true, b, None, None);
                            if !on {
                                ui.hit(b, sid(Control::SettingChoose(f.key.clone(), i as u16)), HitKind::Tap);
                            }
                        }
                    }
                    (FieldKind::Toggle { on }, _) => {
                        let b = Rect::new(c.x + 190.0, ry + 3.0, 40.0, 40.0);
                        ui.toggle("Switch", Some("MONO"), *on, true, b, None, None);
                        ui.hit(b, sid(Control::SettingPress(f.key.clone())), HitKind::Toggle);
                    }
                    (FieldKind::Choice { .. }, _) => {
                        // MIRROR TO: `Silk-Screen` caption over the destination combo.
                        let st = ui.style("views", "Silk-Screen");
                        ui.text("MIRROR TO", st, Rect::new(c.x + 244.0, ry + 2.0, c.w - 252.0, 12.0), Align::Center);
                        choice_box(ui, f, Rect::new(c.x + 248.0, ry + 16.0, c.w - 260.0, 26.0));
                        drawn.push(Drawn { field: f.clone() });
                    }
                    _ => {}
                }
            }
        }
        y += (last as f32 + 1.0) * CUE_ROW_H;
    }
}

// ---- MONITOR CONTROLLER ----

fn monitor_controller(ui: &mut Ui, model: &dyn MixerModel, view: &ViewState, bounds: Rect, drawn: &mut Vec<Drawn>) {
    let c = sub_window(ui, bounds, (4.0 * CELL_PITCH_X + 30.0, 560.0), "MONITOR CONTROLLER");
    ui.fill(c, Rgba::rgb8(52, 55, 58));
    let Some(src) = model.properties() else { return };
    let d = device_of(model, view);
    let edit = view.text_edit.as_ref();
    ui.clip(Some(c));
    // Monitor knob section (`Monitor-Knob-Section-Background`, 264 × 98).
    let knob = Rect::new(c.x + 10.0, c.y + 10.0, 264.0, 98.0);
    ui.background("Monitor-Knob-Section-Background", knob);
    let kf = settings::fields(src, SettingsGroup::MonitorKnob(d));
    let mut y = c.y + 10.0;
    if !kf.is_empty() {
        field_grid(ui, &kf, (knob.max_x() + 10.0, y), c.max_x() - knob.max_x() - 10.0, edit, drawn);
    }
    y = knob.max_y() + 12.0;
    let tabs = ["BASS MANAGEMENT", "SPEAKER UTILITIES"];
    let top = section_tabs(ui, &tabs, view.settings.panel_tab.min(1), c.x + 10.0, y);
    let group = if view.settings.panel_tab == 0 { SettingsGroup::BassManagement(d) } else { SettingsGroup::SpeakerUtilities(d) };
    let f = settings::fields(src, group);
    let area_h = 2.0 * 68.0 + 20.0;
    let area = Rect::new(c.x + 10.0, top, c.w - 20.0, area_h);
    ui.background("Device-Settings-Background", area);
    if f.is_empty() {
        let note = Ui::plain_style("Helvetica", 11.0, false, Rgba::WHITE.with_alpha(0.5));
        ui.text("Not available on this device", note, area, Align::Center);
    } else {
        field_grid(ui, &f, (area.x + 10.0, area.y + 10.0), area.w - 10.0, edit, drawn);
    }
    // MAIN MONITOR CORRECTION.
    let y = area.max_y() + 12.0;
    let top = section_tabs(ui, &["MAIN MONITOR CORRECTION"], 0, c.x + 10.0, y);
    let rc = settings::fields(src, SettingsGroup::RoomCorrection(d));
    let area = Rect::new(c.x + 10.0, top, c.w - 20.0, c.max_y() - 10.0 - top);
    ui.background("Device-Settings-Background", area);
    if rc.is_empty() {
        let note = Ui::plain_style("Helvetica", 11.0, false, Rgba::WHITE.with_alpha(0.5));
        ui.text("Monitor correction is not available", note, area, Align::Center);
    } else {
        field_grid(ui, &rc, (area.x + 10.0, area.y + 10.0), area.w - 10.0, edit, drawn);
    }
    ui.clip(None);
}

// ---- HEADPHONE CORRECTION ----

fn headphone_correction(ui: &mut Ui, model: &dyn MixerModel, view: &ViewState, bounds: Rect, drawn: &mut Vec<Drawn>) {
    let c = sub_window(ui, bounds, (4.0 * CELL_PITCH_X + 30.0, 320.0), "HEADPHONE CORRECTION");
    ui.fill(c, Rgba::rgb8(52, 55, 58));
    let Some(src) = model.properties() else { return };
    let d = device_of(model, view);
    let two = !settings::fields(src, SettingsGroup::HeadphoneCorrection { device: d, hp: 1 }).is_empty();
    let labels: &[&str] = if two { &["HP 1", "HP 2"] } else { &["HP 1"] };
    let hp = if two { view.settings.panel_tab.min(1) } else { 0 };
    let top = section_tabs(ui, labels, hp, c.x + 10.0, c.y + 10.0);
    let area = Rect::new(c.x + 10.0, top, c.w - 20.0, c.max_y() - 10.0 - top);
    ui.background("Device-Settings-Background", area);
    let f = settings::fields(src, SettingsGroup::HeadphoneCorrection { device: d, hp });
    if f.is_empty() {
        let note = Ui::plain_style("Helvetica", 11.0, false, Rgba::WHITE.with_alpha(0.5));
        ui.text("Headphone correction is not available", note, area, Align::Center);
    } else {
        field_grid(ui, &f, (area.x + 10.0, area.y + 10.0), area.w - 10.0, view.text_edit.as_ref(), drawn);
    }
}

// ---- PLUG-IN SCENES ----

const SCENE_ROW_H: f32 = 24.0;

fn plugin_scenes(ui: &mut Ui, model: &dyn MixerModel, view: &ViewState, bounds: Rect, browser: Option<Rect>) {
    let r = browser.unwrap_or_else(|| {
        let (w, h) = (320.0f32.min(bounds.w - 20.0), 560.0f32.min(bounds.h - 20.0));
        Rect::new(bounds.x + ((bounds.w - w) / 2.0).round(), bounds.y + ((bounds.h - h) / 2.5).round(), w, h)
    });
    // `Scenes-Main-Background` (colour overlay 72, 76, 78); swallows taps.
    ui.fill(r, Rgba::rgb8(72, 76, 78));
    ui.hit(r, crate::surfaces::popover::pid(Control::PopoverDismiss), HitKind::Drag { value: 0.0, span: 1.0, reset: None });
    let header = Rect::new(r.x, r.y, r.w, 32.0);
    let hs = ui.style("fonts", "Scenes-Header-Font");
    ui.text("PLUG-IN SCENES", hs, header.inset(12.0, 0.0, 30.0, 0.0), Align::Left);
    if let Some(x) = ui.skin("views", "Browser-Modal-Close-White").and_then(|s| s.image("image")) {
        let x = x.to_string();
        ui.image(&x, Rect::new(header.max_x() - 26.0, header.y + 8.0, 16.0, 16.0), None, None, 0.9);
    }
    ui.hit(Rect::new(header.max_x() - 32.0, header.y, 32.0, 32.0), sid(Control::PanelClose), HitKind::Tap);

    let scenes = model.plugin_scenes();
    let current = model.status().scene;
    let primed = view.settings.scene.and_then(|i| scenes.get(i as usize).cloned());
    // CURRENT / PRIMED (`Scenes-Inset-Background`, colour 27, 29, 32).
    let status = Rect::new(r.x + 8.0, header.max_y(), r.w - 16.0, 48.0);
    ui.fill(status, Rgba::rgb8(27, 29, 32));
    let ls = ui.style("fonts", "Scenes-Current-Primed-Label-Text");
    let vs = Ui::plain_style("Helvetica Neue", 12.0, false, Rgba::WHITE.with_alpha(0.8));
    for (k, (label, value)) in [("CURRENT", current.clone()), ("PRIMED", primed)].into_iter().enumerate() {
        let y = status.y + 4.0 + k as f32 * 20.0;
        ui.text(label, ls.clone(), Rect::new(status.x + 8.0, y, 64.0, 20.0), Align::Left);
        ui.text(value.as_deref().unwrap_or("(NONE)"), vs.clone(), Rect::new(status.x + 74.0, y, status.w - 80.0, 20.0), Align::Left);
    }
    // Buttons: up / down, SAVE, RECALL (`Popover-Button`, `Popover-Button-Primed`).
    let foot = Rect::new(r.x + 8.0, r.max_y() - 40.0, r.w - 16.0, 32.0);
    let bw = ((foot.w - 2.0 * 30.0 - 12.0) / 2.0).floor();
    for (i, up) in [true, false].into_iter().enumerate() {
        let b = Rect::new(foot.x + i as f32 * 30.0, foot.y + 4.0, 28.0, 24.0);
        ui.fill(b, Rgba::rgb8(40, 42, 45));
        let st = Ui::plain_style("Helvetica", 11.0, true, Rgba::WHITE.with_alpha(0.7));
        ui.text(if up { "▲" } else { "▼" }, st, b, Align::Center);
        ui.hit(b, sid(Control::SceneStep(up)), HitKind::Tap);
    }
    let save = Rect::new(foot.x + 66.0, foot.y + 4.0, bw, 24.0);
    // SAVE has no known engine command shape yet (`SceneOperation {}` takes no arguments in the
    // root `commands` node): drawn disabled.
    ui.toggle("Popover-Button", Some("SAVE"), false, false, save, None, None);
    let recall = Rect::new(save.max_x() + 6.0, foot.y + 4.0, bw, 24.0);
    ui.toggle("Popover-Button-Primed", Some("RECALL"), false, view.settings.scene.is_some(), recall, None, None);
    if let Some(i) = view.settings.scene {
        ui.hit(recall, ControlId { strip: "global".into(), control: Control::RecallScene(i.min(255) as u8) }, HitKind::Tap);
    }
    // The scene list (`Rack_ScenesListBox`).
    let list = Rect::new(r.x + 8.0, status.max_y() + 8.0, r.w - 16.0, (foot.y - 8.0 - (status.max_y() + 8.0)).max(0.0));
    ui.fill(list, Rgba::rgb8(27, 29, 32));
    let overflow = (scenes.len() as f32 * SCENE_ROW_H - list.h).max(0.0);
    let off = overflow * view.settings.scroll;
    ui.clip(Some(list));
    let ns = ui.style("views", "Scenes-List-Box-Number");
    let names = ui.style("views", "Scenes-List-Box-Name");
    for (i, n) in scenes.iter().enumerate() {
        let y = list.y + i as f32 * SCENE_ROW_H - off;
        if y + SCENE_ROW_H < list.y || y > list.max_y() {
            continue;
        }
        let row = Rect::new(list.x, y, list.w, SCENE_ROW_H);
        if view.settings.scene == Some(i as u16) {
            ui.fill(row, Rgba::rgb8(64, 58, 40));
        } else if Some(n) == current.as_ref() {
            ui.fill(row, Rgba::WHITE.with_alpha(0.06));
        }
        ui.text(&format!("{}", i + 1), ns.clone(), Rect::new(row.x + 6.0, y, 30.0, SCENE_ROW_H), Align::Right);
        let mut st = names.clone();
        st.color = Rgba::WHITE.with_alpha(if n == "EMPTY SCENE" { 0.35 } else { 0.8 });
        ui.text(n, st, Rect::new(row.x + 44.0, y, row.w - 50.0, SCENE_ROW_H), Align::Left);
        ui.hit(row, sid(Control::SceneSelect(i as u16)), HitKind::Tap);
    }
    ui.clip(None);
    ui.scroll_region(list, ScrollTarget::Settings, overflow);
}

// ---- About ----

fn about(ui: &mut Ui, model: &dyn MixerModel, bounds: Rect) {
    ui.fill(bounds, Rgba::BLACK.with_alpha(0.35));
    ui.hit(bounds, sid(Control::PanelClose), HitKind::Tap);
    let (w, h) = (596.0, 428.0);
    let r = Rect::new(bounds.x + ((bounds.w - w) / 2.0).round(), bounds.y + ((bounds.h - h) / 2.5).round(), w, h);
    ui.fill(r, Rgba::rgb8(28, 30, 32));
    ui.background("About-Background", r);
    if let Some((lw, lh)) = ui.skin("backgrounds", "About-Product_Logo").and_then(|s| s.size()) {
        ui.background("About-Product_Logo", Rect::new(r.x + ((w - lw) / 2.0).round(), r.y + 70.0, lw, lh));
    }
    let src = model.properties();
    let engine = src.and_then(|s| s.prop("/Version")).map(|v| v.display()).unwrap_or_else(|| "—".into());
    let mut vs = ui.style("fonts", "About-Box-Version");
    vs.size = vs.size.min(28.0);
    ui.text(&format!("Version {}", env!("CARGO_PKG_VERSION")), vs, Rect::new(r.x, r.y + 200.0, w, 36.0), Align::Center);
    let bs = ui.style("fonts", "About-Box-Branch");
    ui.text(&format!("console-rs for open-apollo · UA Mixer Engine {engine}"), bs, Rect::new(r.x, r.y + 240.0, w, 26.0), Align::Center);
    if let Some((lw, lh)) = ui.skin("backgrounds", "About-UA-Logo").and_then(|s| s.size()) {
        ui.background("About-UA-Logo", Rect::new(r.x + ((w - lw) / 2.0).round(), r.max_y() - lh - 30.0, lw, lh));
    }
    let fs = ui.style("fonts", "Feature-Key-Static-Text");
    let serial = src.and_then(|s| s.prop("/SerialNumber")).map(|v| v.display()).unwrap_or_default();
    if !serial.is_empty() {
        ui.text(&format!("Serial {serial}"), fs, Rect::new(r.x, r.max_y() - 26.0, w, 16.0), Align::Center);
    }
}
