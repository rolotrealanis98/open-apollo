//! Console's secondary windows, drawn from [`crate::settings`] fields and engine properties,
//! laid out after the Console 1.3.1 captures (2 px/pt; [cap] sNN):
//! - CUE OUTPUTS (`Rack_CuesPopover` › `Rack_CuesAssignments`, [cap] s07): DEVICES / CUE n
//!   headers, per cue MONO / MIX and MIRROR TO, the device row with its headphone outputs and
//!   the "MONITOR MIX to A HP n" routing tiles.
//! - MONITOR CONTROLLER (`Rack_MonitorSubWindow`, [cap] s06): MONITOR MODE, the monitor knob,
//!   MONO / MUTE, BASS MANAGEMENT / SPEAKER UTILITIES, MAIN MONITOR CORRECTION.
//! - HEADPHONE CORRECTION (`HPCorrection`): HP 1 / HP 2 correction sections.
//! - PLUG-IN SCENES (`Rack_SceneSection`, [cap] s10): over the browser column.
//! - Monitor Meters (`Rack_MonitorMetersWindow`, [cap] s44); sends (`Rack_SendsPopover`,
//!   [cap] s50 TALKBACK / s51 AUX n); ADD DEVICE ([cap] s26); I/O PRESETS ([cap] s38); the
//!   route source picker ([cap] s35); TEMPO ([cap] s05); About.
//!
//! [obs] Ids, labels and skins: Win Rack.dll constructors 0x180d26bf0 (cues), 0x180d1f610 (cue
//! route), 0x181145940 (monitor sub-window), 0x181122330 (HP correction), 0x1811f5820 (scenes),
//! 0x181171db0 (sends popover), 0x180d9e250 (meters window), 0x180a23b90 (About).

use std::sync::Arc;

use houston::{Rect, Rgba};

use crate::draw::Align;
use crate::input::{Control, ControlId, HitKind, ScrollTarget};
use crate::model::{MeterFrame, MixerModel};
use crate::settings::{self, FieldKind, Panel, PropertySource, SendsOf, SettingField, SettingsGroup};
use crate::surfaces::popover::pid;
use crate::surfaces::settings::{CELL_PITCH_X, Drawn, centred, choice_box, choice_menu, field_grid, section_header, section_header_text, sid, static_label, text_box, window_frame};
use crate::surfaces::view::{TextTarget, ViewState};
use crate::ui::Ui;

const BLUE: Rgba = Rgba::rgb8(14, 96, 160);
const BUTTON_GREY: Rgba = Rgba::rgb8(66, 72, 76);
const LIST_BG: Rgba = Rgba::rgb8(40, 44, 47);
const LIST_SEL: Rgba = Rgba::rgb8(72, 78, 82);
const DARK: Rgba = Rgba::rgb8(28, 35, 42);

/// Draws the open panel. `browser` is the plug-in browser column, which PLUG-IN SCENES covers
/// when it is shown.
pub fn draw_panel(ui: &mut Ui, model: &dyn MixerModel, view: &ViewState, panel: Panel, bounds: Rect, browser: Option<Rect>) {
    let mut drawn = Vec::new();
    match panel {
        Panel::CueOutputs => cue_outputs(ui, model, bounds, &mut drawn),
        Panel::MonitorController => monitor_controller(ui, model, view, bounds, &mut drawn),
        Panel::HeadphoneCorrection => headphone_correction(ui, model, view, bounds, &mut drawn),
        Panel::PluginScenes => plugin_scenes(ui, model, view, bounds, browser, Control::PanelClose),
        Panel::About => about(ui, model, bounds),
        Panel::MeterWindow => meter_window(ui, model, bounds),
        Panel::Sends(of) => sends(ui, model, of, bounds),
        Panel::AddDevice => add_device(ui, view, bounds),
        Panel::IoPresets => io_presets(ui, model, view, bounds),
        Panel::RouteSource { route, outputs } => route_source(ui, model, view, route, outputs, bounds),
        Panel::Tempo => tempo(ui, model, view, bounds),
    }
    choice_menu(ui, view, &drawn, bounds);
}

fn device_of(model: &dyn MixerModel, view: &ViewState) -> u8 {
    let Some(src) = model.properties() else { return 0 };
    let devs = settings::devices(src);
    devs.iter().find(|d| d.index == view.settings.device).or(devs.first()).map(|d| d.index).unwrap_or(0)
}

/// A centred, non-modal sub-window (`Rack_SubWindow`); returns its page interior.
fn sub_window(ui: &mut Ui, bounds: Rect, size: (f32, f32), title: &str) -> Rect {
    window_frame(ui, centred(bounds, size), title, sid(Control::PanelClose))
}

/// The small-window chrome of Console's dialogs (captures s26 / s38 / s44 / s50): a 27-pt
/// dark title bar (close X at the left, title centred) over a light (`light`) or dark body.
/// Swallows taps on the body; returns the body.
fn dialog_frame(ui: &mut Ui, r: Rect, title: &str, light: bool) -> Rect {
    ui.fill(r.offset(0.0, 6.0), Rgba::BLACK.with_alpha(0.25));
    ui.fill(r, Rgba::rgb8(26, 26, 26));
    ui.hit(r, pid(Control::PopoverDismiss), HitKind::Drag { value: 0.0, span: 1.0, reset: None });
    let bar = Rect::new(r.x + 1.0, r.y + 1.0, r.w - 2.0, 26.0);
    ui.fill(bar, Rgba::rgb8(41, 41, 41));
    if !title.is_empty() {
        let mut style = ui.style("views", "Button-Popover-Header");
        style.color = Rgba::WHITE.with_alpha(0.8);
        ui.text(title, style, Rect::new(r.x, r.y, r.w, 27.0), Align::Center);
    }
    if let Some(x) = ui.skin("views", "Browser-Modal-Close-White").and_then(|s| s.image("image")) {
        let x = x.to_string();
        ui.image(&x, Rect::new(r.x + 9.0, r.y + 8.0, 11.0, 11.0), None, None, 0.95);
    }
    ui.hit(Rect::new(r.x, r.y, 30.0, 27.0), sid(Control::PanelClose), HitKind::Tap);
    let body = Rect::new(r.x + 1.0, r.y + 27.0, r.w - 2.0, r.h - 28.0);
    ui.fill(body, if light { Rgba::rgb8(192, 195, 197) } else { Rgba::rgb8(41, 44, 46) });
    body
}

/// `Rack_Popover::paint` 0x181164400 / `::resized` 0x1811647e0: `Popover-Header` over
/// (0, 0, w, 27) and `Popover-Content` below it; the close X (`Popover-Close-Old`, 12 × 12)
/// centred in (8, 1, 20, 27); the title (`Rack_CalloutBoxHeader`, `Button-Popover-Header`)
/// centred in (0, 0, w, 26). Swallows taps on the body.
fn popover_frame(ui: &mut Ui, r: Rect, title: &str) {
    popover_frame_with(ui, r, title, true);
}

/// [`popover_frame`], with or without the close X (dialogs such as ADD DEVICE have none).
fn popover_frame_with(ui: &mut Ui, r: Rect, title: &str, close: bool) {
    ui.hit(r, pid(Control::PopoverDismiss), HitKind::Drag { value: 0.0, span: 1.0, reset: None });
    ui.background("Popover-Header", Rect::new(r.x, r.y, r.w, 27.0));
    ui.background("Popover-Content", Rect::new(r.x, r.y + 27.0, r.w, r.h - 27.0));
    if !title.is_empty() {
        let style = ui.style("views", "Button-Popover-Header");
        ui.text(title, style, Rect::new(r.x, r.y, r.w, 26.0), Align::Center);
    }
    if !close {
        return;
    }
    if let Some(x) = ui.skin("views", "Popover-Close-Old").and_then(|s| s.image("image")) {
        let x = x.to_string();
        ui.image(&x, Rect::new(r.x + 12.0, r.y + 8.5, 12.0, 12.0), None, None, 1.0);
    }
    ui.hit(Rect::new(r.x + 8.0, r.y + 1.0, 20.0, 27.0), sid(Control::PanelClose), HitKind::Tap);
}

/// A flat dialog button (CANCEL grey, ADD / OK blue); without an id it draws disabled.
fn dialog_button(ui: &mut Ui, label: &str, primary: bool, r: Rect, id: Option<ControlId>) {
    ui.fill(r, if primary { BLUE } else { BUTTON_GREY });
    ui.fill(Rect::new(r.x, r.max_y() - 1.0, r.w, 1.0), Rgba::BLACK.with_alpha(0.35));
    let alpha = if id.is_some() { 0.95 } else { 0.4 };
    let style = Ui::plain_style("Helvetica", 11.0, true, Rgba::WHITE.with_alpha(alpha));
    ui.text(label, style, r, Align::Center);
    if let Some(id) = id {
        ui.hit(r, id, HitKind::Tap);
    }
}

/// Section tabs (`Tab-Section-Left` / `-Right`, or `-Single`); returns the content top.
fn section_tabs(ui: &mut Ui, labels: &[&str], selected: u8, x: f32, y: f32, w: f32) -> f32 {
    let h = 25.0;
    let tw = (w / labels.len() as f32).floor();
    for (i, label) in labels.iter().enumerate() {
        let r = Rect::new(x + i as f32 * tw, y, tw, h);
        let on = selected as usize == i;
        ui.fill(r, if on { Rgba::rgb8(70, 76, 80) } else { Rgba::rgb8(48, 52, 55) });
        if !on {
            ui.fill(Rect::new(r.x, r.max_y() - 1.0, r.w, 1.0), Rgba::BLACK.with_alpha(0.5));
        }
        let style = Ui::plain_style("Helvetica", 10.0, true, Rgba::WHITE.with_alpha(if on { 0.85 } else { 0.35 }));
        ui.text(label, style, r, Align::Center);
        if labels.len() > 1 {
            ui.hit(r, sid(Control::PanelTab(i as u8)), HitKind::Tap);
        }
    }
    y + h
}

/// A `Switch` with its label, bound to a settings toggle key.
fn switch(ui: &mut Ui, label: &str, on: bool, key: &str, r: Rect) {
    ui.toggle("Switch", Some(label), on, true, r, None, None);
    ui.hit(r, sid(Control::SettingPress(key.into())), HitKind::Toggle);
}

fn bool_at(src: &dyn PropertySource, path: &str) -> Option<bool> {
    src.prop(path).and_then(|v| v.as_bool())
}

fn str_at(src: &dyn PropertySource, path: &str) -> Option<String> {
    src.prop(path).and_then(|v| v.as_str().map(str::to_owned))
}

/// A send readout: "-∞" at the floor, one decimal, "+" above 0 dB.
fn db_readout(db: f64) -> String {
    if db <= -143.9 {
        "-∞".into()
    } else if db > 0.0 {
        format!("+{db:.1}")
    } else {
        format!("{db:.1}")
    }
}

// ---- CUE OUTPUTS (drive capture panel-cue "Cues Assignments", 348 × 238) ----

/// `Rack_CuesAssignments` (resized 0x180d29880; ids/skins: cues_assignments, `Rack_CueRoute`):
/// a 27-pt title, the DEVICES header (3, 27, 161, 32) and one 90-pt cue column per cue from
/// x 164 (header, MONO / MIX, MIRROR TO), the device list from y 59 (an empty
/// `No-Device-Background` slot 90 tall, then the device rows), and per headphone output a
/// `Switch-Cue-A` tile (89 × 41) in the column of the cue it plays, rows 43 pt apart from 150.
fn cue_outputs(ui: &mut Ui, model: &dyn MixerModel, bounds: Rect, drawn: &mut Vec<Drawn>) {
    let Some(src) = model.properties() else { return };
    let devices = settings::devices(src);
    let Some(dev) = devices.first() else { return };
    let cues = settings::fields(src, SettingsGroup::CueRoutes(dev.index));
    let n = cues.iter().map(|f| f.row as usize + 1).max().unwrap_or(0).max(1);
    let hp: Vec<String> = src.indices(&format!("{}/outputs", dev.path)).into_iter().map(|o| format!("{}/outputs/{o}", dev.path)).filter(|o| str_at(src, &format!("{o}/IOType")).as_deref() == Some("Headphone")).collect();
    let rows = hp.len().max(2) as f32;
    let col_w = 90.0;
    let size = (164.0 + n as f32 * col_w + 4.0, 149.0 + rows * 44.0 + 1.0);
    // Opens beside its button; Console's window may leave the main window, ours stays inside.
    let anchor = ui.hit_rect(&ControlId { strip: "monitor".into(), control: Control::CueOutputs });
    let r = match anchor {
        Some(a) => Rect::new((a.x - 178.0).min(bounds.max_x() - size.0).max(bounds.x), (a.y - 24.0).clamp(bounds.y, (bounds.max_y() - size.1).max(bounds.y)), size.0, size.1),
        None => centred(bounds, size),
    };
    ui.popover_catcher(bounds);
    sub_window_frame(ui, r, "CUE OUTPUTS");
    let (x0, y0) = (r.x, r.y);
    // The body (`Device-Settings-Background`) under the slots; the device list: the empty top
    // slot, then the device's row (blue bar, icons, name, HPs).
    ui.background("Device-Settings-Background", Rect::new(x0 + 3.0, y0 + 59.0, size.0 - 6.0, size.1 - 62.0));
    ui.background("No-Device-Background", Rect::new(x0 + 3.0, y0 + 59.0, 161.0, 90.0));
    let row = Rect::new(x0 + 3.0, y0 + 149.0, 161.0, rows * 44.0);
    ui.background(&format!("Device-{}-Background", dev.letter()), row);
    let letter = Ui::plain_style("Helvetica", 11.0, true, Rgba::WHITE.with_alpha(0.85));
    ui.text(&dev.letter().to_string(), letter, Rect::new(row.x + 6.0, row.y, 10.0, row.h), Align::Center);
    ui.background("Speaker-Icon", Rect::new(row.x + 20.0, row.y + 20.0, 12.0, 11.0));
    ui.image("backgrounds/icon_talkback_microphone@2x.png", Rect::new(row.x + 22.0, row.y + 40.5, 9.0, 15.0), None, None, 1.0);
    let name = Ui::plain_style("Helvetica", 10.0, true, Rgba::WHITE.with_alpha(0.9));
    ui.text(&dev.name, name, Rect::new(row.x + 45.0, row.y + 1.0, 86.0, 14.0), Align::Center);
    let key = dev.icon_key();
    if let Some((w, h)) = ui.skin("backgrounds", &key).and_then(|s| s.size()) {
        ui.background(&key, Rect::new(row.x + 72.0, row.y + 39.0, w * 0.85, h * 0.85));
    }
    for h in 0..hp.len().min(2) {
        ui.background(if h == 0 { "Headphone1-Icon" } else { "Headphone2-Icon" }, Rect::new(row.x + 135.0, row.y + 14.0 + h as f32 * 43.0, 18.0, 15.0));
    }
    // One header band across, DEVICES and each cue titled in its column, `Vertical-Divider`s
    // (2 pt) at the column edges; each cue column's slots start 1 pt right of the divider.
    section_header(ui, "", Rect::new(x0 + 3.0, y0 + 27.0, size.0 - 6.0, 32.0));
    section_header_text(ui, "DEVICES", Rect::new(x0 + 3.0, y0 + 27.0, 161.0, 32.0));
    for k in 0..n {
        let cx = x0 + 164.0 + k as f32 * col_w;
        let fields: Vec<&SettingField> = cues.iter().filter(|f| f.row as usize == k).collect();
        let name = fields.first().map(|f| f.label.clone()).unwrap_or_default();
        ui.background("No-Device-Settings-Background", Rect::new(cx + 1.0, y0 + 59.0, col_w - 1.0, 89.0));
        ui.background(&format!("Device-{}-Settings-Background", dev.letter()), Rect::new(cx + 1.0, row.y + 1.0, col_w - 1.0, row.h - 1.0));
        ui.background("Vertical-Divider", Rect::new(cx, y0 + 27.0, 2.0, 31.0));
        section_header_text(ui, &name, Rect::new(cx, y0 + 27.0, col_w, 32.0));
        for f in &fields {
            match (&f.kind, f.label.as_str()) {
                (FieldKind::Toggle { on }, _) => switch(ui, "MONO", *on, &f.key, Rect::new(cx + 7.0, y0 + 65.0, 36.0, 36.0)),
                (FieldKind::Choice { selected, .. }, "SOURCE") => {
                    // MIX lit while the cue plays its own mix; tapping flips the source.
                    let mix = *selected == Some(0);
                    let b = Rect::new(cx + 48.0, y0 + 65.0, 36.0, 36.0);
                    ui.toggle("Switch", Some("MIX"), mix, true, b, None, None);
                    ui.hit(b, sid(Control::SettingChoose(f.key.clone(), if mix { 1 } else { 0 })), HitKind::Tap);
                }
                (FieldKind::Choice { .. }, _) => {
                    // MIRROR TO (`Mirror-Background-Popup`): caption over the destination.
                    let box_r = Rect::new(cx + 3.0, y0 + 106.0, 86.0, 36.0);
                    ui.background("Mirror-Background-Popup", box_r);
                    if let Some(icon) = ui.skin("backgrounds", "Mirror-Background-Popup").and_then(|s| s.image("image-overlay")) {
                        let icon = icon.to_string();
                        ui.image(&icon, Rect::new(box_r.x + 69.0, box_r.y + 23.0, 7.0, 7.0), None, None, 1.0);
                    }
                    let cap = ui.style("fonts", "Silk-Screen");
                    ui.text("MIRROR TO", cap, Rect::new(box_r.x, box_r.y + 6.5, box_r.w, 12.0), Align::Center);
                    let value = Ui::plain_style("Helvetica", 10.0, true, Rgba::WHITE.with_alpha(0.7));
                    ui.text(&f.value_text(), value, Rect::new(box_r.x, box_r.y + 18.0, box_r.w, 13.0), Align::Center);
                    ui.hit(box_r, sid(Control::SettingMenu(f.key.clone())), HitKind::Tap);
                    drawn.push(Drawn { field: (*f).clone() });
                }
                _ => {}
            }
        }
        // The headphone tile: which HP output plays this cue.
        for (h, o) in hp.iter().enumerate() {
            if str_at(src, &format!("{o}/MixInSource")).unwrap_or_default() == format!("cue{}", k + 1) {
                let mix = fields.iter().find(|f| f.label == "SOURCE").map(|f| f.value_text()).unwrap_or_default();
                let what = if mix == "MIX" { "MONITOR MIX".to_string() } else { format!("CUE {} MIX", k + 1) };
                let tile = Rect::new(cx + 1.0, row.y + 1.0 + h as f32 * 43.0, 89.0, 41.0);
                // The tile art fills the 89 × 41 tile (`Switch-NoDedicatedCue-A`, stretched).
                ui.toggle("Switch-NoDedicatedCue-A", Some(&format!("{what}\nto\n{} HP {}", dev.letter(), h + 1)), true, true, tile, None, None);
            }
        }
    }
}

/// `Rack_SubWindow` chrome (the cue / monitor sub-windows; capture panel-cue): a 1-px dark
/// outline, the grey frame (lighter under the title), the title centred in the 27-pt bar and
/// the close X at the left. Swallows taps.
fn sub_window_frame(ui: &mut Ui, r: Rect, title: &str) {
    ui.hit(r, pid(Control::PopoverDismiss), HitKind::Drag { value: 0.0, span: 1.0, reset: None });
    ui.fill(r, Rgba::rgb8(26, 26, 26));
    let inner = r.inset(0.5, 0.5, 1.0, 0.5);
    ui.fill(inner, Rgba::rgb8(60, 64, 66));
    ui.fill(Rect::new(inner.x, inner.y, inner.w, 12.5), Rgba::rgb8(63, 67, 69));
    ui.fill(Rect::new(inner.x, inner.y + 12.5, inner.w, 12.0), Rgba::rgb8(61, 65, 67));
    ui.fill(Rect::new(inner.x, inner.y + 24.5, inner.w, 2.0), Rgba::rgb8(55, 59, 61));
    ui.fill(Rect::new(inner.x, inner.y, inner.w, 0.5), Rgba::rgb8(68, 72, 74));
    let style = ui.style("views", "Button-Popover-Header");
    ui.text(title, style, Rect::new(r.x, r.y, r.w, 27.0), Align::Center);
    if let Some(x) = ui.skin("views", "Window-Button-Close").and_then(|s| s.image("image")) {
        let x = x.to_string();
        ui.image(&x, Rect::new(r.x + 10.5, r.y + 7.5, 12.0, 12.0), None, None, 1.0);
    }
    ui.hit(Rect::new(r.x, r.y, 34.0, 27.0), sid(Control::PanelClose), HitKind::Tap);
}

// ---- MONITOR CONTROLLER ([cap] s06, 821 × 532) ----

fn monitor_controller(ui: &mut Ui, model: &dyn MixerModel, view: &ViewState, bounds: Rect, drawn: &mut Vec<Drawn>) {
    let c = sub_window(ui, bounds, (821.0, 532.0), "MONITOR CONTROLLER");
    let Some(src) = model.properties() else { return };
    let d = device_of(model, view);
    let dev = format!("/devices/{d}");
    let left = Rect::new(c.x + 3.0, c.y + 1.0, 268.0, c.h - 2.0);
    // Monitor knob section (`Monitor-Knob-Section-Background`, 264 × 98).
    let knob = Rect::new(left.x + 2.0, left.y + 2.0, 264.0, 98.0);
    ui.fill(knob, Rgba::rgb8(50, 54, 57));
    ui.background("Monitor-Knob-Section-Background", knob);
    let mode = settings::fields(src, SettingsGroup::MonitorKnob(d));
    static_label(ui, "MONITOR MODE", Rect::new(knob.x + 6.0, knob.y + 28.0, 80.0, 12.0), Align::Left);
    let mode_text = mode.iter().find(|f| f.label == "MONITOR MODE").map(|f| f.value_text()).unwrap_or_else(|| "STEREO".into());
    let mbox = Rect::new(knob.x + 6.0, knob.y + 42.0, 68.0, 22.0);
    ui.fill(mbox, Rgba::rgb8(22, 24, 26));
    ui.text(&mode_text, Ui::plain_style("Helvetica", 10.0, true, Rgba::WHITE.with_alpha(0.8)), mbox, Align::Center);
    let m = model.monitor();
    ui.knob("Monitor-Green", m.level, Rect::new(knob.x + 88.0, knob.y + 2.0, 88.0, 86.0), Some(ControlId { strip: "monitor".into(), control: Control::MonitorLevel }), None);
    let lvl = mode.iter().find(|f| f.label == "LEVEL").map(|f| f.value_text()).unwrap_or_default();
    ui.text(&lvl, Ui::plain_style("Helvetica", 9.0, true, Rgba::WHITE.with_alpha(0.75)), Rect::new(knob.x + 100.0, knob.y + 78.0, 64.0, 12.0), Align::Center);
    ui.toggle("Switch", Some("MONO"), m.mono, true, Rect::new(knob.x + 184.0, knob.y + 30.0, 36.0, 36.0), Some(ControlId { strip: "monitor".into(), control: Control::MonitorMono }), None);
    ui.toggle("Switch-Red", Some("MUTE"), m.mute, true, Rect::new(knob.x + 222.0, knob.y + 30.0, 36.0, 36.0), Some(ControlId { strip: "monitor".into(), control: Control::MonitorMute }), None);
    // BASS MANAGEMENT / SPEAKER UTILITIES.
    let top = section_tabs(ui, &["BASS MANAGEMENT", "SPEAKER UTILITIES"], view.settings.panel_tab.min(1), left.x + 2.0, knob.max_y() + 8.0, 264.0);
    let area = Rect::new(left.x + 2.0, top, 264.0, left.max_y() - top);
    ui.fill(area, Rgba::rgb8(52, 55, 58));
    let group = if view.settings.panel_tab == 0 { SettingsGroup::BassManagement(d) } else { SettingsGroup::SpeakerUtilities(d) };
    let f = settings::fields(src, group);
    let mut y = area.y + 8.0;
    if f.is_empty() {
        ui.text("N/A", Ui::plain_style("Helvetica", 10.0, false, Rgba::WHITE.with_alpha(0.5)), Rect::new(area.x, y, area.w, 40.0), Align::Center);
    }
    // `Rack_BassManagement` layout (capture s06): ENABLE (66 × 28) then LFE FILTER / SATELLITE
    // FILTER (78 wide) on the first row; CHANNEL (66) and SATELLITE CROSSOVER (168) below.
    let slots = [(6.0, 0.0, 66.0), (85.0, 0.0, 78.0), (175.0, 0.0, 78.0), (6.0, 36.0, 66.0), (85.0, 36.0, 168.0)];
    for (fl, (dx, dy, w)) in f.iter().zip(slots).filter(|_| view.settings.panel_tab == 0) {
        let (x, yy) = (area.x + dx, y + dy);
        match &fl.kind {
            FieldKind::Toggle { on } => {
                let r = Rect::new(x, yy + 4.0, w, 28.0);
                ui.toggle("Switch-Large", Some(&fl.label), *on, true, r, None, None);
                ui.hit(r, sid(Control::SettingPress(fl.key.clone())), HitKind::Toggle);
            }
            _ => {
                static_label(ui, &fl.label, Rect::new(x, yy, w, 12.0), Align::Left);
                choice_box(ui, fl, Rect::new(x, yy + 12.0, w, 20.0));
                drawn.push(Drawn { field: fl.clone() });
            }
        }
    }
    if view.settings.panel_tab == 1 {
        for (k, fl) in f.iter().enumerate() {
            let x = area.x + 6.0 + (k % 3) as f32 * 86.0;
            let yy = y + (k / 3) as f32 * 40.0;
            static_label(ui, &fl.label, Rect::new(x, yy, 84.0, 12.0), Align::Left);
            let r = Rect::new(x, yy + 12.0, 80.0, 24.0);
            match &fl.kind {
                FieldKind::Toggle { on } => {
                    ui.toggle("Switch-Large", Some(if *on { "ON" } else { "OFF" }), *on, true, r, None, None);
                    ui.hit(r, sid(Control::SettingPress(fl.key.clone())), HitKind::Toggle);
                }
                FieldKind::Choice { .. } => {
                    choice_box(ui, fl, r);
                    drawn.push(Drawn { field: fl.clone() });
                }
                _ => ui.text(&fl.value_text(), Ui::plain_style("Helvetica", 10.0, true, Rgba::WHITE.with_alpha(0.75)), r, Align::Center),
            }
        }
    }
    y += if view.settings.panel_tab == 0 { 76.0 } else { f.len().div_ceil(3) as f32 * 40.0 + 8.0 };
    // The speaker display (`Monitor-Surround-Speakers-Background`).
    let disp = Rect::new(area.x + 6.0, y, area.w - 12.0, (area.max_y() - y - 6.0).max(0.0));
    ui.fill(disp, Rgba::rgb8(20, 22, 24));
    ui.background("Monitor-Surround-Speakers-Background", disp);
    let spk = Ui::plain_style("Helvetica", 9.0, true, Rgba::WHITE.with_alpha(0.6));
    for k in 0..2 {
        let x = if k == 0 { disp.x + 10.0 } else { disp.max_x() - 40.0 };
        ui.background(if k == 0 { "Monitor-Selected-Left" } else { "Monitor-Selected-Right" }, Rect::new(x + 5.0, disp.y + 14.0, 20.0, 20.0));
        ui.text("N/A", spk.clone(), Rect::new(x, disp.y + 38.0, 30.0, 12.0), Align::Center);
    }
    // MAIN MONITOR CORRECTION.
    let right = Rect::new(left.max_x() + 6.0, c.y + 1.0, c.max_x() - left.max_x() - 9.0, c.h - 2.0);
    ui.fill(right, Rgba::rgb8(52, 55, 58));
    ui.text("MAIN MONITOR CORRECTION", Ui::plain_style("Helvetica", 10.0, true, Rgba::WHITE.with_alpha(0.45)), Rect::new(right.x, right.y + 6.0, right.w, 16.0), Align::Center);
    let rc = settings::fields(src, SettingsGroup::RoomCorrection(d));
    let profile = rc.iter().find(|f| f.label == "CALIBRATION PROFILE").map(|f| f.value_text()).unwrap_or_else(|| "<PROFILE>".into());
    static_label(ui, "CALIBRATION PROFILE", Rect::new(right.x + 10.0, right.y + 62.0, 200.0, 14.0), Align::Left);
    let pbox = Rect::new(right.x + 8.0, right.y + 80.0, right.w - 18.0, 22.0);
    ui.fill(pbox, Rgba::rgb8(20, 22, 24));
    ui.text(&profile, Ui::plain_style("Helvetica", 10.0, false, Rgba::WHITE.with_alpha(0.85)), pbox.inset(8.0, 0.0, 100.0, 0.0), Align::Left);
    ui.text("OPEN SoundID", Ui::plain_style("Helvetica", 10.0, true, Rgba::WHITE.with_alpha(0.9)), Rect::new(pbox.max_x() - 90.0, pbox.y, 84.0, pbox.h), Align::Right);
    ui.hit(Rect::new(pbox.max_x() - 100.0, pbox.y, 100.0, pbox.h), sid(Control::SettingPress("post:ShowSoundIDReference:speaker".into())), HitKind::Tap);
    // The correction curve: grid, dB and frequency labels (the curve itself comes from
    // `GetCorrectionImage`, which the tree does not carry).
    let graph = Rect::new(right.x, pbox.max_y() + 8.0, right.w, 298.0);
    ui.fill(graph, Rgba::rgb8(12, 13, 14));
    let grid = Rgba::WHITE.with_alpha(0.08);
    let label = Ui::plain_style("Helvetica", 9.0, true, Rgba::WHITE.with_alpha(0.7));
    for (i, db) in ["+18 dB", "+12 dB", "+6 dB", "0 dB", "-6 dB", "-12 dB", "-18 dB"].iter().enumerate() {
        let y = graph.y + 46.0 + i as f32 * 36.0;
        ui.fill(Rect::new(graph.x, y, graph.w - 50.0, 1.0), grid);
        ui.text(db, label.clone(), Rect::new(graph.max_x() - 50.0, y - 6.0, 46.0, 12.0), Align::Right);
    }
    for (i, fq) in ["100 Hz", "1 kHz", "10 kHz"].iter().enumerate() {
        let x = graph.x + 140.0 + i as f32 * 165.0;
        ui.fill(Rect::new(x, graph.y, 1.0, graph.h - 18.0), grid);
        ui.text(fq, label.clone(), Rect::new(x - 30.0, graph.max_y() - 16.0, 60.0, 12.0), Align::Center);
    }
    ui.fill(Rect::new(graph.x + 8.0, graph.y + 46.0 + 3.0 * 36.0, graph.w - 60.0, 1.0), Rgba::rgb8(120, 200, 110).with_alpha(0.6));
    // SAFE HEADROOM, BYPASS; MAIN MONITOR CORRECTION ENABLE.
    let by = graph.max_y() + 10.0;
    let head = rc.iter().find(|f| f.label == "HEADROOM").map(|f| f.value_text()).unwrap_or_default();
    for (label, x) in [("SAFE HEADROOM", 14.0), ("BYPASS", 95.0)] {
        if let Some(fl) = rc.iter().find(|f| f.label == label) {
            if let FieldKind::Toggle { on } = fl.kind {
                let r = Rect::new(right.x + x, by, 70.0, 30.0);
                let text = if label == "BYPASS" { "BYPASS".to_string() } else { format!("({head} SAFE\nHEADROOM)") };
                ui.toggle("Switch-Large", Some(&text), on, true, r, None, None);
                ui.hit(r, sid(Control::SettingPress(fl.key.clone())), HitKind::Toggle);
            }
        }
    }
    let power = bool_at(src, &format!("{dev}/SpeakerCorrectionPower")).unwrap_or(false);
    ui.text("MAIN MONITOR\nCORRECTION", Ui::plain_style("Helvetica", 9.0, true, Rgba::WHITE.with_alpha(0.45)), Rect::new(right.max_x() - 180.0, by, 90.0, 30.0), Align::Right);
    let er = Rect::new(right.max_x() - 80.0, by, 70.0, 30.0);
    ui.toggle("Switch-Large", Some("ENABLE"), power, true, er, None, None);
    ui.hit(er, sid(Control::SettingPress(format!("toggle:{dev}/SpeakerCorrectionPower").into())), HitKind::Toggle);
}

// ---- HEADPHONE CORRECTION ----

fn headphone_correction(ui: &mut Ui, model: &dyn MixerModel, view: &ViewState, bounds: Rect, drawn: &mut Vec<Drawn>) {
    let c = sub_window(ui, bounds, (4.0 * CELL_PITCH_X + 30.0, 320.0), "HEADPHONE CORRECTION");
    let Some(src) = model.properties() else { return };
    let d = device_of(model, view);
    let two = !settings::fields(src, SettingsGroup::HeadphoneCorrection { device: d, hp: 1 }).is_empty();
    let labels: &[&str] = if two { &["HP 1", "HP 2"] } else { &["HP 1"] };
    let hp = if two { view.settings.panel_tab.min(1) } else { 0 };
    let top = section_tabs(ui, labels, hp, c.x + 10.0, c.y + 10.0, if two { 262.0 } else { 131.0 });
    let area = Rect::new(c.x + 10.0, top, c.w - 20.0, c.max_y() - 10.0 - top);
    ui.background("Device-Settings-Background", area);
    let f = settings::fields(src, SettingsGroup::HeadphoneCorrection { device: d, hp });
    if f.is_empty() {
        ui.text("Headphone correction is not available", Ui::plain_style("Helvetica", 11.0, false, Rgba::WHITE.with_alpha(0.5)), area, Align::Center);
    } else {
        field_grid(ui, &f, (area.x + 10.0, area.y + 10.0), area.w - 10.0, view.text_edit.as_ref(), drawn);
    }
}

// ---- PLUG-IN SCENES ([cap] s10) ----

const SCENE_ROW_H: f32 = 31.75;

/// `Rack_SceneSection` (capture s10, at 1470 × 1105): a 290-pt column under the title bar —
/// header 30, the CURRENT / PRIMED inset (10…280, y 39…186) with RECALL / SAVE / ▲ ▼, then
/// the scene list from y 195, 31.75 pt rows. [I] Console inserts the column left of the
/// browser; here it covers the browser's left 290 pt.
/// PLUG-IN SCENES contents in `browser` (the window's scenes column, or centred in `bounds`);
/// `close` is what the header's X sends.
pub fn plugin_scenes(ui: &mut Ui, model: &dyn MixerModel, view: &ViewState, bounds: Rect, browser: Option<Rect>, close: Control) {
    let full = browser.unwrap_or_else(|| centred(bounds, (290.0, 560.0)));
    let r = Rect::new(full.x, full.y, 290.0f32.min(full.w), full.h);
    // `Scenes-Main-Background` (72, 76, 78); swallows taps.
    ui.fill(r, Rgba::rgb8(44, 47, 50));
    ui.hit(r, pid(Control::PopoverDismiss), HitKind::Drag { value: 0.0, span: 1.0, reset: None });
    let header = Rect::new(r.x, r.y, r.w, 30.0);
    ui.fill(header, Rgba::rgb8(52, 56, 59));
    let hs = ui.style("fonts", "Scenes-Header-Font");
    let tw = crate::window::helvetica_bold_width("PLUG-IN SCENES", hs.size);
    let tx = header.x + ((header.w - tw - 16.0) / 2.0).round();
    ui.fill(Rect::new(tx, header.y + 10.0, 11.0, 11.0), Rgba::rgb8(224, 32, 208));
    ui.text("PLUG-IN SCENES", hs, Rect::new(tx + 17.0, header.y, tw + 4.0, header.h), Align::Left);
    if let Some(x) = ui.skin("views", "Browser-Modal-Close-White").and_then(|s| s.image("image")) {
        let x = x.to_string();
        ui.image(&x, Rect::new(header.x + 262.0, header.y + 9.0, 12.0, 12.0), None, None, 0.9);
    }
    ui.hit(Rect::new(header.x + 254.0, header.y, 36.0, 30.0), sid(close), HitKind::Tap);

    let scenes = model.plugin_scenes();
    let current = model.status().scene;
    let primed = view.settings.scene.and_then(|i| scenes.get(i as usize).cloned());
    // CURRENT / PRIMED (`Scenes-Inset-Background`).
    let inset = Rect::new(r.x + 10.0, r.y + 39.0, 270.0, 147.0);
    ui.fill(inset, Rgba::rgb8(72, 76, 78));
    let ls = ui.style("fonts", "Scenes-Current-Primed-Label-Text");
    let vs = Ui::plain_style("Helvetica", 9.0, true, Rgba::WHITE.with_alpha(0.85));
    for (k, (label, value)) in [("CURRENT", current.clone()), ("PRIMED", primed)].into_iter().enumerate() {
        let y = inset.y + 3.0 + k as f32 * 58.0;
        ui.text(label, ls.clone(), Rect::new(inset.x, y, inset.w, 16.0), Align::Center);
        let v = Rect::new(r.x + 18.0, y + 22.0, 252.0, 28.0);
        ui.fill(v, Rgba::rgb8(36, 38, 40));
        ui.text(value.as_deref().unwrap_or("(NONE)"), vs.clone(), v, Align::Center);
    }
    // RECALL, SAVE, ▲ ▼.
    let by = r.y + 161.0;
    let recall = Rect::new(r.x + 18.0, by, 94.0, 20.0);
    let can_recall = view.settings.scene.is_some();
    ui.fill(recall, Rgba::rgb8(86, 90, 93));
    let bs = Ui::plain_style("Helvetica", 9.0, true, Rgba::WHITE.with_alpha(if can_recall { 0.85 } else { 0.3 }));
    ui.text("RECALL", bs.clone(), recall, Align::Center);
    if let Some(i) = view.settings.scene {
        ui.hit(recall, ControlId { strip: "global".into(), control: Control::RecallScene(i.min(255) as u8) }, HitKind::Tap);
    }
    let save = Rect::new(r.x + 120.0, by, 96.0, 20.0);
    ui.fill(save, Rgba::rgb8(86, 90, 93));
    // SAVE: `SceneOperation {}` has no argument shape in the root `commands` node; disabled.
    ui.text("SAVE", Ui::plain_style("Helvetica", 9.0, true, Rgba::WHITE.with_alpha(0.3)), save, Align::Center);
    for (i, up) in [true, false].into_iter().enumerate() {
        let b = Rect::new(r.x + 224.0 + i as f32 * 26.0, by, 18.0, 20.0);
        ui.fill(b, Rgba::rgb8(86, 90, 93));
        ui.text(if up { "▲" } else { "▼" }, bs.clone(), b, Align::Center);
        ui.hit(b, sid(Control::SceneStep(up)), HitKind::Tap);
    }
    // The scene list (`Rack_ScenesListBox`).
    let list = Rect::new(r.x, r.y + 195.0, r.w, (r.max_y() - r.y - 195.0).max(0.0));
    ui.fill(list, Rgba::rgb8(29, 31, 33));
    let overflow = (scenes.len() as f32 * SCENE_ROW_H - list.h).max(0.0);
    let off = overflow * view.settings.scroll;
    ui.clip(Some(list));
    let mut st = ui.style("views", "Scenes-List-Box-Number");
    st.color = Rgba::WHITE.with_alpha(0.9);
    for (i, n) in scenes.iter().enumerate() {
        let y = list.y + i as f32 * SCENE_ROW_H - off;
        if y + SCENE_ROW_H < list.y || y > list.max_y() {
            continue;
        }
        let row = Rect::new(list.x, y, list.w - 14.0, SCENE_ROW_H);
        if view.settings.scene == Some(i as u16) {
            ui.fill(row, Rgba::rgb8(64, 58, 40));
        } else if Some(n) == current.as_ref() {
            ui.fill(row, Rgba::WHITE.with_alpha(0.06));
        }
        ui.text(&format!("{}", i + 1), st.clone(), Rect::new(row.x + 14.0, y, 28.0, SCENE_ROW_H), Align::Center);
        ui.text(n, st.clone(), Rect::new(row.x + 59.0, y, row.w - 62.0, SCENE_ROW_H), Align::Left);
        ui.hit(row, sid(Control::SceneSelect(i as u16)), HitKind::Tap);
    }
    ui.clip(None);
    ui.scroll_region(list, ScrollTarget::Settings, overflow);
}

// ---- Monitor Meters ([cap] s44, 164 × 645) ----

fn meter_window(ui: &mut Ui, model: &dyn MixerModel, bounds: Rect) {
    // 118 × 599 (s44): meters at x 29.5 / 71.5 (18 wide), y 83…585; scales left, centre, right.
    let r = centred(bounds, (118.0, 599.0));
    let body = dialog_frame(ui, r, "", false);
    ui.fill(body, Rgba::rgb8(45, 45, 45));
    ui.background("UA-Logo", Rect::new(r.x + 44.0, r.y + 37.0, 30.0, 24.0));
    let m: MeterFrame = model.monitor_meters();
    let (top, h) = (r.y + 83.0, 502.0);
    for (k, (lvl, clip)) in [(m.left, m.clip_left), (m.right, m.clip_right)].into_iter().enumerate() {
        let x = r.x + 29.5 + k as f32 * 42.0;
        ui.fill(Rect::new(x - 2.5, top - 3.0, 23.0, h + 6.0), Rgba::rgb8(0, 0, 0));
        ui.bar_meter("Monitor-Meter", lvl, clip, Rect::new(x, top, 18.0, h));
    }
    let st = Ui::plain_style("Helvetica", 8.0, false, Rgba::rgb8(99, 104, 108));
    for (i, db) in [0, 3, 6, 9, 12, 15, 18, 21, 27, 36, 46, 60].iter().enumerate() {
        let y = top + 4.0 + i as f32 * (h - 8.0) / 11.0;
        for (x, align) in [(r.x + 3.0, Align::Right), (r.x + 50.0, Align::Center), (r.x + 94.0, Align::Left)] {
            ui.text(&format!("{db}"), st.clone(), Rect::new(x, y - 5.0, 20.0, 10.0), align);
        }
    }
    let lr = Ui::plain_style("Helvetica", 9.0, true, Rgba::WHITE.with_alpha(0.6));
    ui.text("L", lr.clone(), Rect::new(r.x + 29.5, top + h + 6.0, 18.0, 12.0), Align::Center);
    ui.text("R", lr, Rect::new(r.x + 71.5, top + h + 6.0, 18.0, 12.0), Align::Center);
}

// ---- Sends ([cap] s50 TALKBACK, s51 AUX n; 97-pt columns, 408 tall) ----

/// The window title and send nodes of `of`: (`…/sends/N`, name), cues within `CueBusCount`.
fn send_nodes(src: &dyn PropertySource, of: SendsOf) -> (String, Vec<(String, String)>) {
    let dev = "/devices/0";
    let node = match of {
        SendsOf::Talkback => src.indices(&format!("{dev}/inputs")).into_iter().map(|i| format!("{dev}/inputs/{i}")).find(|p| str_at(src, &format!("{p}/IOType")).as_deref() == Some("TalkbackMic")),
        SendsOf::Aux(k) => Some(format!("{dev}/auxs/{k}")),
        SendsOf::Input(k) => Some(format!("{dev}/inputs/{k}")),
    };
    let Some(node) = node else { return (String::new(), Vec::new()) };
    let active_cues = src.prop("/CueBusCount").and_then(|v| v.as_i64()).unwrap_or(2).max(0) as usize;
    let sends = src
        .indices(&format!("{node}/sends"))
        .into_iter()
        .map(|s| format!("{node}/sends/{s}"))
        .filter(|s| {
            let id = str_at(src, &format!("{s}/ID")).unwrap_or_default();
            !id.starts_with("cue") || id[3..].parse::<usize>().is_ok_and(|n| n <= active_cues)
        })
        .map(|s| {
            let name = str_at(src, &format!("{s}/Name")).unwrap_or_default();
            (s, name)
        })
        .collect();
    let title = match of {
        SendsOf::Talkback => "TALKBACK".to_string(),
        SendsOf::Aux(_) | SendsOf::Input(_) => str_at(src, &format!("{node}/Name")).unwrap_or_else(|| "SENDS".into()),
    };
    (title, sends)
}

fn sends(ui: &mut Ui, model: &dyn MixerModel, of: SendsOf, bounds: Rect) {
    let Some(src) = model.properties() else { return };
    let (title, sends) = send_nodes(src, of);
    // 96-pt columns under a 27-pt title (s50: 386 × 362 for four, s51: 194 for two). Beside
    // the strip whose sends they are, at its sends cell (send-cell: (584, 583) for the strip at
    // 488; sends-aux: (1085, 583) for the aux at 989): the strip's right edge, 12.5 pt above
    // its first mini send fader.
    let col = 96.0;
    let size = (2.0 + sends.len().max(1) as f32 * col, 362.0);
    let strip: Option<Arc<str>> = match of {
        SendsOf::Aux(k) => Some(format!("/devices/0/auxs/{k}").into()),
        SendsOf::Input(k) => Some(format!("/devices/0/inputs/{k}").into()),
        SendsOf::Talkback => ui.hits.iter().find(|h| h.id.control == Control::ShowPanel(Panel::Sends(SendsOf::Talkback))).map(|h| h.id.strip.clone()),
    };
    let anchor = strip.and_then(|st| {
        let name = ui.hit_rect(&ControlId { strip: st.clone(), control: Control::Name })?;
        let send = ui.hit_rect(&ControlId { strip: st, control: Control::SendLevel(0) })?;
        Some((name.max_x(), send.y - 12.5))
    });
    let r = match anchor {
        Some((x, y)) => {
            let x = if x + size.0 <= bounds.max_x() { x } else { (bounds.max_x() - size.0).max(bounds.x) };
            Rect::new(x, y.min(bounds.max_y() - size.1).max(bounds.y), size.0, size.1)
        }
        None => centred(bounds, size),
    };
    popover_frame(ui, r, &title);
    // Each channel (`Rack_SendChannelStrip`) at (1 + 96k, 31, 96, h − 36) over its
    // `Sends-Popover-Channel` background (the first `-Left`, the last `-Right`) at
    // (x, 27, 96, h − 28) (`Rack_SendsPopover::resized` 0x181173220, `::paint` 0x181173030).
    let n = sends.len();
    for (k, (node, name)) in sends.iter().enumerate() {
        let x = r.x + 1.0 + k as f32 * col;
        let key = if k == 0 { "Sends-Popover-Channel-Left" } else if k + 1 == n { "Sends-Popover-Channel-Right" } else { "Sends-Popover-Channel" };
        ui.background(key, Rect::new(x, r.y + 27.0, col, r.h - 28.0));
        let muted = bool_at(src, &format!("{node}/Bypass")).unwrap_or(false);
        let level = src.prop(&format!("{node}/GainTapered")).and_then(|v| v.as_f64()).unwrap_or(0.0);
        let db = src.prop(&format!("{node}/Gain")).and_then(|v| v.as_f64()).unwrap_or(-144.0);
        // Mono input sends carry their pan (send-cell); bus and talkback sends do not (s50/s51).
        let mono_input = matches!(of, SendsOf::Input(_)) && src.prop(&format!("{node}/Pan")).is_some();
        let pan = mono_input.then(|| ((src.prop(&format!("{node}/Pan")).and_then(|v| v.as_f64()).unwrap_or(0.0) + 1.0) / 2.0, None));
        let ids = crate::surfaces::cells::FullSendIds {
            bus: !matches!(of, SendsOf::Input(_)),
            pan,
            mute: Some(sid(Control::SettingPress(format!("toggle:{node}/Bypass").into()))),
            level: Some(sid(Control::SettingSet(format!("fader:{node}/GainTapered").into()))),
        };
        let send = crate::model::SendState { name: name.clone(), level, muted };
        ui.at(x, r.y + 31.0, |ui| crate::surfaces::cells::full_send_strip(ui, "Sends-Input-Fader", &send, &db_readout(db), 0.0, r.h - 36.0, ids));
    }
    // ‹ › step through the auxes, or through the input strips: `Popover-Left` / `-Right`
    // (12 × 12 chevrons) centred in (w − 58, 1, 20, 27) and (w − 28, 1, 20, 27), dimmed at
    // the ends.
    let step: Option<(u8, &str, fn(u8) -> SendsOf)> = match of {
        SendsOf::Aux(k) => Some((k, "auxs", SendsOf::Aux)),
        SendsOf::Input(k) => Some((k, "inputs", SendsOf::Input)),
        SendsOf::Talkback => None,
    };
    if let Some((k, kind, make)) = step {
        let exists = |t: u8| src.prop(&format!("/devices/0/{kind}/{t}/Name")).is_some();
        let prev = (0..k).rev().find(|t| exists(*t));
        let next = (k.saturating_add(1)..=u8::MAX).take(64).find(|t| exists(*t));
        for (i, (key, to)) in [("Popover-Left", prev), ("Popover-Right", next)].into_iter().enumerate() {
            let b = Rect::new(r.max_x() - 58.0 + i as f32 * 30.0, r.y + 1.0, 20.0, 27.0);
            if let Some(img) = ui.skin("views", key).and_then(|s| s.image("image")) {
                let img = img.to_string();
                ui.image(&img, Rect::new(b.x + 4.0, b.y + 7.5, 12.0, 12.0), None, None, if to.is_some() { 1.0 } else { 0.3 });
            }
            if let Some(to) = to {
                ui.hit(b, sid(Control::ShowPanel(Panel::Sends(make(to)))), HitKind::Tap);
            }
        }
    }
}

// ---- ADD DEVICE ([cap] s26, 266 × 361) ----

/// The device types the engine's `AddDevice` command offers.
const DEVICE_TYPES: [&str; 14] = ["Apollo", "Apollo 16", "Apollo 8", "Apollo 8p", "Apollo 16 MkII", "Apollo x6", "Apollo x8", "Apollo x8p", "Apollo x16", "Apollo x16D", "Apollo x6 Gen 2", "Apollo x8 Gen 2", "Apollo x8p Gen 2", "Apollo x16 Gen 2"];

fn add_device(ui: &mut Ui, view: &ViewState, bounds: Rect) {
    // `Rack_AddDeviceDialog::resized` 0x180a27e30 (capture set-adddev, 220 × 315): the list
    // (12, 27 + 12, w − 24, h − 27 − 60), CANCEL (12, h − 36, (w − 36)/2, 24) and ADD beside it;
    // a popover without a close X.
    let r = centred(bounds, (220.0, 315.0));
    popover_frame_with(ui, r, "ADD DEVICE", false);
    let list = Rect::new(r.x + 12.0, r.y + 39.0, r.w - 24.0, r.h - 87.0);
    let items: Vec<(Option<String>, String)> = DEVICE_TYPES.iter().map(|t| (Some(format!("Device-Icon-{}", t.replace(' ', "-"))), t.to_string())).collect();
    popover_list(ui, view, list, &items, 50.0, 11.0);
    let bw = ((r.w - 36.0) / 2.0).floor();
    let by = r.max_y() - 36.0;
    popover_button(ui, "CANCEL", false, Rect::new(r.x + 12.0, by, bw, 24.0), Some(sid(Control::PanelClose)));
    let add = view.settings.list.and_then(|i| DEVICE_TYPES.get(i as usize)).map(|t| sid(Control::SettingPress(format!("post:AddDevice:{t}").into())));
    popover_button(ui, "ADD", true, Rect::new(r.x + 24.0 + bw, by, bw, 24.0), add);
}

/// A popover list box (ADD DEVICE, I/O PRESETS; captures set-adddev / set-d-iopresets): a
/// 1-pt dark frame round the (50, 55, 57) well, 40-pt rows, the selected row (67, 80, 86)
/// inset 3 pt (21 on the right, by the scroll bar), the `Small-Vertical-Scroll-Bar` thumb in
/// the right 18 pt. Rows are an optional `backgrounds` icon (fitted 32 wide, centred on
/// x + 21.75) and a label `text_x` from the list's left edge (Helvetica `text_size`, white 0.85). The first row is
/// selected until the user picks one, as in Console.
fn popover_list(ui: &mut Ui, view: &ViewState, list: Rect, items: &[(Option<String>, String)], text_x: f32, text_size: f32) {
    ui.fill(list, Rgba::rgb8(22, 25, 26));
    let well = list.inset(1.0, 1.0, 1.0, 1.0);
    ui.fill(well, Rgba::rgb8(50, 55, 57));
    let row_h = 40.0;
    let content = items.len() as f32 * row_h;
    let overflow = (content - list.h).max(0.0);
    let off = (overflow * view.settings.scroll).round();
    let selected = view.settings.list.map(|i| i as usize).unwrap_or(0);
    ui.clip(Some(well));
    let ns = Ui::plain_style("Helvetica", text_size, false, Rgba::WHITE.with_alpha(0.85));
    for (i, (icon, label)) in items.iter().enumerate() {
        let y = list.y + i as f32 * row_h - off;
        if y > list.max_y() || y + row_h < list.y {
            continue;
        }
        let row = Rect::new(list.x, y, list.w - 18.0, row_h);
        if i == selected {
            ui.fill(Rect::new(list.x + 3.0, y + 3.0, list.w - 24.0, row_h - 6.0), Rgba::rgb8(67, 80, 86));
        }
        if let Some(key) = icon {
            let art = ui.skin("backgrounds", key).and_then(|s| Some((s.size()?, s.image("image")?.to_string())));
            if let Some(((w, h), path)) = art {
                let f = 32.0 / w;
                ui.image(&path, Rect::new(list.x + 21.75 - 16.0, y + (row_h - h * f) / 2.0, 32.0, h * f), None, None, 1.0);
            }
        }
        // The selected row's label is brighter than the rest (set-d-iopresets).
        let mut st = ns.clone();
        st.color = Rgba::WHITE.with_alpha(if i == selected { 0.9 } else { 0.72 });
        ui.text(label, st, Rect::new(list.x + text_x, y, row.w - text_x - 4.0, row_h), Align::Left);
        ui.hit(row, sid(Control::ListSelect(i as u16)), HitKind::Tap);
    }
    ui.clip(None);
    if overflow > 0.0 {
        let track = Rect::new(list.max_x() - 18.0, list.y + 1.0, 18.0, list.h - 2.0);
        let thumb_h = (track.h * list.h / content).round().max(18.0);
        let ty = (track.y + (track.h - thumb_h) * view.settings.scroll.clamp(0.0, 1.0)).round();
        if let Some(img) = ui.skin("backgrounds", "Small-Vertical-Scroll-Bar").and_then(|s| s.image("image")) {
            let img = img.to_string();
            ui.image_stretched(&img, Rect::new(track.x, ty, track.w, thumb_h), 1.0);
        }
    }
    ui.scroll_region(list, ScrollTarget::Settings, overflow);
}

/// `Popover-Button` (CANCEL, OPEN, …; `Popover-Button-Primed` for the default, ADD): the
/// stretched plate with its Oxygen bold label; without an id it draws disabled.
/// Console draws these lit whether or not they can act (set-d-iopresets).
fn popover_button(ui: &mut Ui, label: &str, primed: bool, r: Rect, id: Option<ControlId>) {
    let art = ui.skin("views", if primed { "Popover-Button-Primed" } else { "Popover-Button" }).and_then(|s| s.image(if primed { "image-overlay" } else { "image-off" }).map(str::to_string));
    if let Some(path) = art {
        ui.image_stretched(&path, r, 1.0);
    }
    let mut style = ui.style("views", "Popover-Button");
    style.color = Rgba::WHITE;
    ui.text(label, style, r, Align::Center);
    if let Some(id) = id {
        ui.hit(r, id, HitKind::Tap);
    }
}

// ---- I/O PRESETS (`Rack_RoutePresetsPopover`; capture set-d-iopresets, 205 × 360) ----

fn io_presets(ui: &mut Ui, model: &dyn MixerModel, view: &ViewState, bounds: Rect) {
    // `Rack_RoutePresetsPopover::resized` 0x18116f5d0: the list (1, 28, w − 2, h − 80), then
    // OPEN / SAVE / SAVE AS... at y = h − 51 and LOAD / RENAME / DELETE 26 below, each
    // (w − 4)/3 wide, 1 pt apart.
    let r = centred(bounds, (205.0, 360.0));
    popover_frame(ui, r, "I/O PRESETS");
    let Some(src) = model.properties() else { return };
    let presets: Vec<String> = src.choices("/IOMapPreset").into_iter().map(|c| c.label).collect();
    let list = Rect::new(r.x + 1.0, r.y + 28.0, r.w - 2.0, r.h - 80.0);
    let items: Vec<(Option<String>, String)> = presets.iter().map(|p| (None, p.clone())).collect();
    popover_list(ui, view, list, &items, 10.5, 11.0);
    let sel_i = view.settings.list.unwrap_or(0);
    let sel = presets.get(sel_i as usize).cloned();
    let current = str_at(src, "/IOMapPreset").filter(|s| !s.is_empty());
    let bw = ((r.w - 4.0) / 3.0).floor();
    // OPEN / SAVE / SAVE AS... / LOAD / RENAME / DELETE: the engine's IOMapPreset commands
    // (`SaveIOMapPreset`, `RenameIOMapPreset`, `DeleteIOMapPreset`, `Import`).
    let buttons: [(&str, Option<Control>); 6] = [
        ("OPEN", sel.as_ref().map(|_| Control::SettingChoose("iopreset:/IOMapPreset".into(), sel_i))),
        ("SAVE", current.as_ref().map(|n| Control::SettingPress(format!("post:SaveIOMapPreset:{n}").into()))),
        ("SAVE AS...", Some(Control::SettingEdit("post-text:SaveIOMapPreset".into()))),
        ("LOAD", Some(Control::SettingPress("post:Import:IOMapPreset".into()))),
        ("RENAME", sel.as_ref().map(|n| Control::SettingEdit(format!("post-text:RenameIOMapPreset:{n}").into()))),
        ("DELETE", sel.as_ref().map(|n| Control::SettingPress(format!("post:DeleteIOMapPreset:{n}").into()))),
    ];
    for (k, (label, control)) in buttons.into_iter().enumerate() {
        let b = Rect::new(r.x + 1.0 + (k % 3) as f32 * (bw + 1.0), r.max_y() - 51.0 + (k / 3) as f32 * 26.0, bw, 24.0);
        popover_button(ui, label, false, b, control.map(sid));
    }
    // SAVE AS / RENAME edit the name in a field over the buttons.
    if let Some(e) = view.text_edit.as_ref() {
        if let TextTarget::Setting(k) = &e.target {
            if k.starts_with("post-text:") {
                let f = SettingField::new(k.clone(), "", FieldKind::Text(String::new()), 0);
                text_box(ui, &f, "", Some(e), Rect::new(r.x + 4.0, r.max_y() - 98.0, r.w - 8.0, 40.0));
            }
        }
    }
}

// ---- Route source ([cap] s35, 520 × 360) ----

fn route_source(ui: &mut Ui, model: &dyn MixerModel, view: &ViewState, route: u16, outputs: bool, bounds: Rect) {
    // `Rack_IOMatrixPopover` (resized 0x181128920; capture set-d-insrc, 522 × 360): a legacy
    // popover; the device list (4, 31, 200, h − 35), the categories (204, 31, 160, …) and the
    // channels (365, 31, w − 369, …) with 2-pt `Vertical-Divider`s between; 40-pt rows, the
    // selected one (67, 80, 86) inset 3 pt.
    let Some(src) = model.properties() else { return };
    let routes = settings::io_routes(src, outputs);
    let Some(rt) = routes.get(route as usize) else { return };
    let title = format!("{} {}", if outputs { "OUTPUT" } else { "INPUT" }, route + 1);
    let r = centred(bounds, (522.0, 360.0));
    legacy_popover_frame(ui, r, &title, 403.0);
    // ^ v step through the routes; < > drawn as Console does (their target is unknown).
    let n = routes.len() as u16;
    for (k, (key, to)) in [("Popover-Up", route.checked_sub(1)), ("Popover-Down", (route + 1 < n).then_some(route + 1)), ("Popover-Left", route.checked_sub(1)), ("Popover-Right", (route + 1 < n).then_some(route + 1))].into_iter().enumerate() {
        let b = Rect::new(r.x + 403.0 + k as f32 * 30.0, r.y + 1.0, 20.0, 27.0);
        if let Some(img) = ui.skin("views", key).and_then(|s| s.image("image")) {
            let img = img.to_string();
            ui.image(&img, Rect::new(b.x + 4.0, b.y + 7.5, 12.0, 12.0), None, None, if to.is_some() { 1.0 } else { 0.3 });
        }
        if let (Some(to), true) = (to, k < 2) {
            ui.hit(b, sid(Control::ShowPanel(Panel::RouteSource { route: to, outputs })), HitKind::Tap);
        }
    }
    let node = format!("{}/{}", if outputs { "/outputroutes" } else { "/inputroutes" }, rt.index);
    let (top, h) = (r.y + 31.0, r.h - 35.0);
    // Devices: the NONE slot, then each unit's row (as in Settings).
    let devs = settings::devices(src);
    ui.background("Device-Settings-Background", Rect::new(r.x + 4.0, top, 200.0, h));
    let none = Rect::new(r.x + 4.0, top, 200.0, 60.0);
    ui.background("No-Device-Settings-Background", Rect::new(none.x + 2.0, none.y + 1.0, 196.0, 58.0));
    ui.text("NONE", Ui::plain_style("Helvetica", 11.0, false, Rgba::WHITE.with_alpha(0.4)), none, Align::Center);
    let letter = devs.first().map(|d| d.letter()).unwrap_or('A');
    for (k, d) in devs.iter().enumerate() {
        crate::surfaces::settings::device_row(ui, d, Rect::new(none.x, none.max_y() + k as f32 * 60.0, 200.0, 60.0));
    }
    // Categories and their channels.
    let cats: [(&str, &[&str]); 6] = [
        ("ANALOG", &["MicLineHiZ", "MicLine", "Line"]),
        ("ADAT", &["ADAT"]),
        ("DIGITAL", &["SPDIF", "AESEBU"]),
        ("VIRTUAL", &["Virtual"]),
        ("MONITOR", &["CtrlRoom", "Cue", "Aux"]),
        ("TALKBACK", &["Talkback"]),
    ];
    let sel = view.settings.list.map(|i| i as usize).unwrap_or_else(|| cats.iter().position(|(_, t)| t.contains(&rt.io_type.as_str())).unwrap_or(0)).min(cats.len() - 1);
    let ccol = Rect::new(r.x + 204.0, top, 160.0, h);
    let xcol = Rect::new(r.x + 365.0, top, r.w - 369.0, h);
    for col in [ccol, xcol] {
        ui.background(&format!("Device-{letter}-Settings-Background"), col);
        ui.background("Vertical-Divider", Rect::new(col.x - 2.0 + if col.x == ccol.x { 1.0 } else { 0.0 }, top, 2.0, h));
    }
    let row_at = |col: Rect, i: usize| Rect::new(col.x, col.y + i as f32 * 40.0, col.w, 40.0);
    let item = Ui::plain_style("Helvetica", 11.0, false, Rgba::WHITE.with_alpha(0.85));
    for (i, (name, _)) in cats.iter().enumerate() {
        let row = row_at(ccol, i);
        if i == sel {
            ui.fill(Rect::new(row.x + 3.0, row.y + 3.0, row.w - 6.0, 34.0), Rgba::rgb8(67, 80, 86));
        }
        ui.text(name, item.clone(), row.inset(10.0, 0.0, 24.0, 0.0), Align::Left);
        ui.text("▶", Ui::plain_style("Helvetica", 9.0, false, Rgba::WHITE.with_alpha(0.85)), Rect::new(row.max_x() - 20.5, row.y, 10.0, row.h), Align::Center);
        ui.hit(row, sid(Control::ListSelect(i as u16)), HitKind::Tap);
    }
    let all = settings::io_routes(src, false);
    let chans: Vec<&settings::IoRoute> = all.iter().filter(|x| cats[sel].1.contains(&x.io_type.as_str())).collect();
    let ty = cats[sel].1.first().copied().unwrap_or("");
    for (i, ch) in chans.iter().enumerate() {
        let row = row_at(xcol, i);
        if ch.default_name == rt.default_name {
            ui.fill(Rect::new(row.x + 3.0, row.y + 3.0, row.w - 6.0, 34.0), Rgba::rgb8(67, 80, 86));
        }
        ui.text(&ch.default_name, item.clone(), row.inset(10.0, 0.0, 4.0, 0.0), Align::Left);
        ui.hit(row, sid(Control::SettingChoose(format!("route:{node}:{ty}").into(), i as u16)), HitKind::Tap);
    }
}

/// A legacy popover (`Legacy-Popover-Header` / `-Content`, the I/O matrix source picker): the
/// grey header, the close X centred in (8, 1, 20, 27), the title centred between the X and
/// `title_right`, a `Horizontal-Divider` under the header. Swallows taps.
fn legacy_popover_frame(ui: &mut Ui, r: Rect, title: &str, title_right: f32) {
    ui.hit(r, pid(Control::PopoverDismiss), HitKind::Drag { value: 0.0, span: 1.0, reset: None });
    ui.fill(r, Rgba::rgb8(26, 26, 26));
    let inner = r.inset(0.5, 0.5, 0.5, 0.5);
    ui.background("Legacy-Popover-Header", Rect::new(inner.x, inner.y, inner.w, 26.5));
    ui.background("Legacy-Popover-Content", Rect::new(inner.x, r.y + 27.0, inner.w, inner.max_y() - r.y - 27.0));
    ui.background("Horizontal-Divider", Rect::new(r.x, r.y + 27.0, r.w, 2.0));
    let style = ui.style("views", "Button-Popover-Header");
    ui.text(title, style, Rect::new(r.x + 28.0, r.y, title_right - 28.0, 27.0), Align::Center);
    if let Some(x) = ui.skin("views", "Popover-Close-Old").and_then(|s| s.image("image")) {
        let x = x.to_string();
        ui.image(&x, Rect::new(r.x + 12.0, r.y + 8.5, 12.0, 12.0), None, None, 1.0);
    }
    ui.hit(Rect::new(r.x + 8.0, r.y + 1.0, 20.0, 27.0), sid(Control::PanelClose), HitKind::Tap);
}

// ---- TEMPO ([cap] s05; drive capture sb-tempo: 196 × 111 at (9, 963)) ----

/// The TEMPO window opens above the status bar's TEMPO field at the field's x, its bottom
/// 0.5 pt above the field (sb-tempo: (9, 963) over the (9, 1074.5) field); centred when the
/// field is not on screen.
fn tempo(ui: &mut Ui, model: &dyn MixerModel, view: &ViewState, bounds: Rect) {
    let size = (196.0, 111.0);
    let field = ui.hit_rect(&ControlId { strip: crate::surfaces::status::STATUS_ID.into(), control: Control::StatusTempo });
    let r = match field {
        Some(f) => Rect::new(f.x, (f.y - size.1 - 0.5).max(bounds.y), size.0, size.1),
        None => centred(bounds, size),
    };
    // `Rack_TempoPopover` (sb-tempo): Rack_Popover's chrome (`Popover-Header` / `-Content`,
    // the close X, the title).
    popover_frame(ui, r, "TEMPO");
    // The tempo field (80 × 24 at (12, 39)): the value selected when the window opens.
    // (Opened without the status bar, `--open tempo`, it shows the session tempo the same way.)
    let text = view.text_edit.as_ref().filter(|e| matches!(e.target, TextTarget::Tempo)).map(|e| e.text.clone()).unwrap_or_else(|| format!("{:.2}", model.status().tempo));
    let fr = Rect::new(r.x + 12.0, r.y + 39.0, 80.0, 24.0);
    ui.fill(fr, Rgba::rgb8(78, 83, 88));
    let ts = Ui::plain_style("Helvetica", 11.0, false, Rgba::rgb8(191, 202, 214));
    if !text.is_empty() {
        let w = crate::window::helvetica_width(&text, ts.size).ceil();
        ui.fill(Rect::new(fr.x + 8.0, fr.y + 6.5, w + 2.0, 11.0), Rgba::rgb8(43, 79, 119));
    }
    ui.text(&text, ts, Rect::new(fr.x + 8.5, fr.y + 2.0, fr.w - 10.0, fr.h), Align::Left);
    // TAP (Switch-Large, 70 × 30 at (109.5, 36.5)): tap tempo needs event timing the UI does
    // not keep yet, so it is drawn without a hit.
    ui.toggle("Switch-Large", Some("TAP"), false, true, Rect::new(r.x + 108.5, r.y + 35.5, 71.0, 31.0), None, None);
    popover_button(ui, "CANCEL", false, Rect::new(r.x + 12.0, r.y + 75.0, 80.0, 24.0), Some(pid(Control::TextCancel)));
    popover_button(ui, "OK", true, Rect::new(r.x + 104.0, r.y + 75.0, 80.0, 24.0), Some(pid(Control::TextCommit)));
}

// ---- About ----

fn about(ui: &mut Ui, model: &dyn MixerModel, bounds: Rect) {
    ui.fill(bounds, Rgba::BLACK.with_alpha(0.35));
    ui.hit(bounds, sid(Control::PanelClose), HitKind::Tap);
    let (w, h) = (596.0, 428.0);
    let r = centred(bounds, (w, h));
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
