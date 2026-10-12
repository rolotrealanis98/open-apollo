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
use crate::surfaces::settings::{CELL_PITCH_X, Drawn, centred, choice_box, choice_menu, field_grid, section_header, sid, static_label, text_box, window_frame};
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
        Panel::Tempo => tempo(ui, view, bounds),
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
    ui.chrome_layer(r, 27.0);
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

// ---- CUE OUTPUTS ([cap] s07, 360 × 242) ----

fn cue_outputs(ui: &mut Ui, model: &dyn MixerModel, bounds: Rect, drawn: &mut Vec<Drawn>) {
    let Some(src) = model.properties() else { return };
    let devices = settings::devices(src);
    let Some(dev) = devices.first() else { return };
    let cues = settings::fields(src, SettingsGroup::CueRoutes(dev.index));
    let n = cues.iter().map(|f| f.row as usize + 1).max().unwrap_or(0).max(1);
    let col_w = 90.0;
    let size = (180.0 + n as f32 * col_w, 242.0);
    // Opens left of its button, like Console's popover.
    let anchor = ui.hit_rect(&ControlId { strip: "monitor".into(), control: Control::CueOutputs });
    let r = match anchor {
        Some(a) => Rect::new((a.x - 8.0 - size.0).max(bounds.x), (a.max_y() - size.1).clamp(bounds.y, (bounds.max_y() - size.1).max(bounds.y)), size.0, size.1),
        None => centred(bounds, size),
    };
    ui.popover_catcher(bounds);
    let c = window_frame(ui, r, "CUE OUTPUTS", sid(Control::PanelClose));
    let x0 = c.x;
    section_header(ui, "DEVICES", Rect::new(x0, c.y, 160.0, 28.0));
    ui.fill(Rect::new(x0, c.y + 28.0, c.w, c.h - 28.0), DARK);
    let hp: Vec<String> = src.indices(&format!("{}/outputs", dev.path)).into_iter().map(|o| format!("{}/outputs/{o}", dev.path)).filter(|o| str_at(src, &format!("{o}/IOType")).as_deref() == Some("Headphone")).collect();
    let dev_y = c.y + 28.0 + 88.0;
    for k in 0..n {
        let cx = x0 + 162.0 + k as f32 * col_w;
        let row: Vec<&SettingField> = cues.iter().filter(|f| f.row as usize == k).collect();
        let name = row.first().map(|f| f.label.clone()).unwrap_or_default();
        section_header(ui, &name, Rect::new(cx, c.y, col_w - 2.0, 28.0));
        for f in &row {
            match (&f.kind, f.label.as_str()) {
                (FieldKind::Toggle { on }, _) => switch(ui, "MONO", *on, &f.key, Rect::new(cx + 6.0, c.y + 34.0, 36.0, 36.0)),
                (FieldKind::Choice { selected, .. }, "SOURCE") => {
                    // MIX lit while the cue plays its own mix; tapping flips the source.
                    let mix = *selected == Some(0);
                    let b = Rect::new(cx + 46.0, c.y + 34.0, 36.0, 36.0);
                    ui.toggle("Switch", Some("MIX"), mix, true, b, None, None);
                    ui.hit(b, sid(Control::SettingChoose(f.key.clone(), if mix { 1 } else { 0 })), HitKind::Tap);
                }
                (FieldKind::Choice { .. }, _) => {
                    // MIRROR TO: the caption over the destination combo.
                    let box_r = Rect::new(cx + 4.0, c.y + 76.0, col_w - 10.0, 34.0);
                    choice_box(ui, f, box_r);
                    let st = ui.style("views", "Silk-Screen");
                    ui.text("MIRROR TO", st, Rect::new(box_r.x, box_r.y + 3.0, box_r.w, 12.0), Align::Center);
                    drawn.push(Drawn { field: (*f).clone() });
                }
                _ => {}
            }
        }
        // The headphone tile: which HP output plays this cue.
        for (h, o) in hp.iter().enumerate() {
            if str_at(src, &format!("{o}/MixInSource")).unwrap_or_default() == format!("cue{}", k + 1) {
                let mix = row.iter().find(|f| f.label == "SOURCE").map(|f| f.value_text()).unwrap_or_default();
                let what = if mix == "MIX" { "MONITOR MIX".to_string() } else { format!("CUE {} MIX", k + 1) };
                let tile = Rect::new(cx + 1.0, dev_y + 2.0 + h as f32 * 42.0, col_w - 4.0, 39.0);
                ui.fill(tile, BLUE);
                let st = Ui::plain_style("Helvetica", 9.0, true, Rgba::WHITE.with_alpha(0.9));
                ui.text(&format!("{what}\nto\n{} HP {}", dev.letter(), h + 1), st, tile, Align::Center);
            }
        }
    }
    // The device row: unit letter, icons, name, device icon, HP outputs.
    let row = Rect::new(x0, dev_y, 160.0, 84.0);
    ui.fill(Rect::new(x0, dev_y - 1.0, c.w, 1.0), Rgba::BLACK.with_alpha(0.6));
    ui.fill(row, Rgba::rgb8(29, 32, 33));
    ui.fill(Rect::new(row.x + 31.0, row.y, 6.0, row.h), Rgba::rgb8(7, 105, 169));
    let letter = Ui::plain_style("Helvetica", 11.0, true, Rgba::WHITE.with_alpha(0.85));
    ui.text(&dev.letter().to_string(), letter, Rect::new(row.x + 2.0, row.y + 30.0, 14.0, 14.0), Align::Center);
    ui.background("Speaker-Icon", Rect::new(row.x + 16.0, row.y + 16.0, 12.0, 11.0));
    ui.image("backgrounds/icon_talkback_microphone@2x.png", Rect::new(row.x + 18.0, row.y + 36.0, 9.0, 15.0), None, None, 1.0);
    let name = Ui::plain_style("Helvetica", 10.0, true, Rgba::WHITE.with_alpha(0.9));
    ui.text(&dev.name, name, Rect::new(row.x + 40.0, row.y + 2.0, 80.0, 14.0), Align::Center);
    let key = dev.icon_key();
    if let Some((w, h)) = ui.skin("backgrounds", &key).and_then(|s| s.size()) {
        ui.background(&key, Rect::new(row.x + 63.0, row.y + 38.0, w * 0.9, h * 0.9));
    }
    let hs = Ui::plain_style("Helvetica", 9.0, true, Rgba::WHITE.with_alpha(0.7));
    for h in 0..hp.len().min(2) {
        ui.text(&format!("HP{}", h + 1), hs.clone(), Rect::new(row.x + 124.0, row.y + 12.0 + h as f32 * 42.0, 30.0, 16.0), Align::Center);
    }
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
    ui.chrome_layer(r, 30.0);
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
    // RECALL / SAVE (`Rack_SceneSection::buttonClicked` 0x1811f7e80): RecallScene / StoreScene of
    // the selected (primed) row.
    let scene_cmd = |op, i: u16| ControlId { strip: "global".into(), control: Control::SceneCommand(op, i) };
    if let Some(i) = view.settings.scene {
        ui.hit(recall, scene_cmd(crate::menu::SceneOp::Recall, i), HitKind::Tap);
    }
    let save = Rect::new(r.x + 120.0, by, 96.0, 20.0);
    ui.fill(save, Rgba::rgb8(86, 90, 93));
    ui.text("SAVE", bs.clone(), save, Align::Center);
    if let Some(i) = view.settings.scene {
        ui.hit(save, scene_cmd(crate::menu::SceneOp::Save, i), HitKind::Tap);
    }
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
        // Rename edits the row in place (`Rack_ScenesListBoxRow::EditSceneName`).
        let editing = view.text_edit.as_ref().filter(|e| e.target == TextTarget::SceneName(i as u16));
        if editing.is_some() {
            ui.fill(Rect::new(row.x + 56.0, y + 6.0, row.w - 60.0, SCENE_ROW_H - 12.0), Rgba::rgb8(78, 83, 88));
        }
        ui.text(editing.map_or(n.as_str(), |e| e.text.as_str()), st.clone(), Rect::new(row.x + 59.0, y, row.w - 62.0, SCENE_ROW_H), Align::Left);
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
    let c = dialog_frame(ui, r, &title, false);
    // ‹ › step through the auxes, or through the input strips (send-cell shows them on an
    // input's window too).
    let step: Option<(u8, &str, fn(u8) -> SendsOf)> = match of {
        SendsOf::Aux(k) => Some((k, "auxs", SendsOf::Aux)),
        SendsOf::Input(k) => Some((k, "inputs", SendsOf::Input)),
        SendsOf::Talkback => None,
    };
    if let Some((k, kind, make)) = step {
        let st = Ui::plain_style("Helvetica", 12.0, true, Rgba::WHITE.with_alpha(0.5));
        let exists = |t: u8| src.prop(&format!("/devices/0/{kind}/{t}/Name")).is_some();
        let prev = (0..k).rev().find(|t| exists(*t));
        let next = (k.saturating_add(1)..=u8::MAX).take(64).find(|t| exists(*t));
        for (i, (label, to)) in [("‹", prev), ("›", next)].into_iter().enumerate() {
            let b = Rect::new(r.max_x() - 50.0 + i as f32 * 22.0, r.y + 3.0, 20.0, 20.0);
            ui.text(label, st.clone(), b, Align::Center);
            if let Some(to) = to {
                ui.hit(b, sid(Control::ShowPanel(Panel::Sends(make(to)))), HitKind::Tap);
            }
        }
    }
    for (k, (node, name)) in sends.iter().enumerate() {
        let x = c.x + k as f32 * col;
        ui.fill(Rect::new(x, c.y, col - 1.0, c.h), Rgba::rgb8(66, 70, 73));
        ui.fill(Rect::new(x + col - 1.0, c.y, 1.0, c.h), Rgba::rgb8(20, 22, 24));
        // Name tab with the destination's colour rule (AUX 1 orange, AUX 2 blue).
        let tab = Rect::new(x + 6.0, c.y + 6.0, col - 13.0, 26.0);
        ui.fill(tab, Rgba::rgb8(30, 32, 34));
        ui.text(name, Ui::plain_style("Helvetica", 10.0, false, Rgba::WHITE.with_alpha(0.8)), tab, Align::Center);
        let rule = match name.as_str() {
            "AUX 1" => Some(Rgba::rgb8(224, 128, 32)),
            "AUX 2" => Some(Rgba::rgb8(48, 120, 220)),
            _ => None,
        };
        if let Some(rc) = rule {
            ui.fill(Rect::new(tab.x + 6.0, tab.max_y() - 4.0, tab.w - 12.0, 2.0), rc);
        }
        // Input sends carry their pan (send-cell: a knob with L / R over the M button).
        let pan_h = if matches!(of, SendsOf::Input(_)) {
            let pan = src.prop(&format!("{node}/Pan")).and_then(|v| v.as_f64()).unwrap_or(0.0);
            // The send pan knob of the strips' sends cells (`Pan-White`, 40 × 46).
            ui.knob("Pan-White", (pan + 1.0) / 2.0, Rect::new(x + 28.0, c.y + 35.0, 40.0, 46.0), None, Some(0.5));
            50.0
        } else {
            0.0
        };
        let muted = bool_at(src, &format!("{node}/Bypass")).unwrap_or(false);
        let mr = Rect::new(x + (col - 26.0) / 2.0, c.y + 42.0 + pan_h, 24.0, 18.0);
        ui.toggle("Switch-Small", Some("M"), muted, true, mr, None, None);
        ui.hit(mr, sid(Control::SettingPress(format!("toggle:{node}/Bypass").into())), HitKind::Toggle);
        let v = src.prop(&format!("{node}/GainTapered")).and_then(|v| v.as_f64()).unwrap_or(0.0);
        let db = src.prop(&format!("{node}/Gain")).and_then(|v| v.as_f64()).unwrap_or(-144.0);
        let fm = Rect::new(x + 6.0, c.y + 64.0 + pan_h, col - 13.0, c.h - 70.0 - pan_h);
        ui.fader_and_meters("Main-Bus-Fader", false, v, MeterFrame::default(), &db_readout(db), fm, Some(sid(Control::SettingSet(format!("fader:{node}/GainTapered").into()))));
    }
}

// ---- ADD DEVICE ([cap] s26, 266 × 361) ----

/// The device types the engine's `AddDevice` command offers.
const DEVICE_TYPES: [&str; 14] = ["Apollo", "Apollo 16", "Apollo 8", "Apollo 8p", "Apollo 16 MkII", "Apollo x6", "Apollo x8", "Apollo x8p", "Apollo x16", "Apollo x16D", "Apollo x6 Gen 2", "Apollo x8 Gen 2", "Apollo x8p Gen 2", "Apollo x16 Gen 2"];

fn add_device(ui: &mut Ui, view: &ViewState, bounds: Rect) {
    // 220 × 315 (s26): a dark list inset 11 pt in the light body, CANCEL / ADD under it.
    let r = centred(bounds, (220.0, 315.0));
    let c = dialog_frame(ui, r, "ADD DEVICE", true);
    let list = Rect::new(c.x + 11.0, c.y + 11.0, c.w - 22.0, c.h - 58.0);
    ui.fill(list, LIST_BG);
    let row_h = 40.0;
    let overflow = (DEVICE_TYPES.len() as f32 * row_h - list.h).max(0.0);
    let off = (overflow * view.settings.scroll).round();
    ui.clip(Some(list));
    let ns = Ui::plain_style("Helvetica", 11.0, false, Rgba::WHITE.with_alpha(0.85));
    for (i, t) in DEVICE_TYPES.iter().enumerate() {
        let y = list.y + i as f32 * row_h - off;
        let row = Rect::new(list.x, y, list.w - 14.0, row_h);
        if view.settings.list == Some(i as u16) {
            ui.fill(row.inset(2.0, 2.0, 2.0, 2.0), LIST_SEL);
        }
        ui.background(&format!("Device-Icon-{}", t.replace(' ', "-")), Rect::new(row.x + 8.0, y + 6.0, 39.0, 28.0));
        ui.text(t, ns.clone(), Rect::new(row.x + 52.0, y, row.w - 56.0, row_h), Align::Left);
        ui.hit(row, sid(Control::ListSelect(i as u16)), HitKind::Tap);
    }
    ui.clip(None);
    ui.scroll_region(list, ScrollTarget::Settings, overflow);
    let by = c.max_y() - 37.0;
    let bw = ((c.w - 33.0) / 2.0).floor();
    dialog_button(ui, "CANCEL", false, Rect::new(c.x + 11.0, by, bw, 24.0), Some(sid(Control::PanelClose)));
    let add = view.settings.list.and_then(|i| DEVICE_TYPES.get(i as usize)).map(|t| sid(Control::SettingPress(format!("post:AddDevice:{t}").into())));
    dialog_button(ui, "ADD", true, Rect::new(c.max_x() - 11.0 - bw, by, bw, 24.0), add);
}

// ---- I/O PRESETS ([cap] s38, 251 × 406) ----

fn io_presets(ui: &mut Ui, model: &dyn MixerModel, view: &ViewState, bounds: Rect) {
    // 205 × 360 (s38): the list, then OPEN / SAVE / SAVE AS... and LOAD / RENAME / DELETE.
    let r = centred(bounds, (205.0, 360.0));
    let c = dialog_frame(ui, r, "I/O PRESETS", true);
    let Some(src) = model.properties() else { return };
    let presets: Vec<String> = src.choices("/IOMapPreset").into_iter().map(|c| c.label).collect();
    let list = Rect::new(c.x, c.y, c.w, c.h - 56.0);
    ui.fill(list, LIST_BG);
    let row_h = 40.0;
    let overflow = (presets.len() as f32 * row_h - list.h).max(0.0);
    let off = (overflow * view.settings.scroll).round();
    ui.clip(Some(list));
    let ns = Ui::plain_style("Helvetica", 11.0, false, Rgba::WHITE.with_alpha(0.85));
    for (i, p) in presets.iter().enumerate() {
        let y = list.y + i as f32 * row_h - off;
        let row = Rect::new(list.x, y, list.w - 14.0, row_h);
        if view.settings.list == Some(i as u16) {
            ui.fill(row.inset(2.0, 2.0, 2.0, 2.0), LIST_SEL);
        }
        ui.text(p, ns.clone(), Rect::new(row.x + 8.0, y, row.w - 12.0, row_h), Align::Left);
        ui.hit(row, sid(Control::ListSelect(i as u16)), HitKind::Tap);
    }
    ui.clip(None);
    ui.scroll_region(list, ScrollTarget::Settings, overflow);
    let sel = view.settings.list.and_then(|i| presets.get(i as usize)).cloned();
    let current = str_at(src, "/IOMapPreset").filter(|s| !s.is_empty());
    let bw = (c.w / 3.0).floor();
    // OPEN / SAVE / SAVE AS... / LOAD / RENAME / DELETE: the engine's IOMapPreset commands
    // (`SaveIOMapPreset`, `RenameIOMapPreset`, `DeleteIOMapPreset`, `Import`).
    let buttons: [(&str, Option<Control>); 6] = [
        ("OPEN", view.settings.list.filter(|_| sel.is_some()).map(|i| Control::SettingChoose("iopreset:/IOMapPreset".into(), i))),
        ("SAVE", current.as_ref().map(|n| Control::SettingPress(format!("post:SaveIOMapPreset:{n}").into()))),
        ("SAVE AS...", Some(Control::SettingEdit("post-text:SaveIOMapPreset".into()))),
        ("LOAD", Some(Control::SettingPress("post:Import:IOMapPreset".into()))),
        ("RENAME", sel.as_ref().map(|n| Control::SettingEdit(format!("post-text:RenameIOMapPreset:{n}").into()))),
        ("DELETE", sel.as_ref().map(|n| Control::SettingPress(format!("post:DeleteIOMapPreset:{n}").into()))),
    ];
    for (k, (label, control)) in buttons.into_iter().enumerate() {
        let r = Rect::new(c.x + (k % 3) as f32 * bw, c.max_y() - 54.0 + (k / 3) as f32 * 26.0, bw - 1.0, 25.0);
        dialog_button(ui, label, false, r, control.map(sid));
    }
    // SAVE AS / RENAME edit the name in a field over the buttons.
    if let Some(e) = view.text_edit.as_ref() {
        if let TextTarget::Setting(k) = &e.target {
            if k.starts_with("post-text:") {
                let f = SettingField::new(k.clone(), "", FieldKind::Text(String::new()), 0);
                text_box(ui, &f, "", Some(e), Rect::new(c.x + 4.0, c.max_y() - 98.0, c.w - 8.0, 40.0));
            }
        }
    }
}

// ---- Route source ([cap] s35, 520 × 360) ----

fn route_source(ui: &mut Ui, model: &dyn MixerModel, view: &ViewState, route: u16, outputs: bool, bounds: Rect) {
    let Some(src) = model.properties() else { return };
    let routes = settings::io_routes(src, outputs);
    let Some(r) = routes.get(route as usize) else { return };
    let title = format!("{} {}", if outputs { "OUTPUT" } else { "INPUT" }, route + 1);
    let c = sub_window(ui, bounds, (522.0, 360.0), &title);
    let node = format!("{}/{}", if outputs { "/outputroutes" } else { "/inputroutes" }, r.index);
    ui.fill(c, DARK);
    // Devices: NONE, then each unit.
    let devs = settings::devices(src);
    let dcol = Rect::new(c.x, c.y, 198.0, c.h);
    ui.fill(dcol, Rgba::rgb8(43, 46, 48));
    let none = Rect::new(dcol.x, dcol.y + 2.0, dcol.w, 58.0);
    ui.fill(none, Rgba::rgb8(29, 32, 33));
    ui.text("NONE", Ui::plain_style("Helvetica", 11.0, false, Rgba::WHITE.with_alpha(0.5)), none, Align::Center);
    for (k, d) in devs.iter().enumerate() {
        let row = Rect::new(dcol.x, none.max_y() + 1.0 + k as f32 * 61.0, dcol.w, 58.0);
        ui.background(&format!("Device-{}-Background", d.letter()), row);
        ui.text(&d.name, Ui::plain_style("Helvetica", 11.0, true, Rgba::WHITE.with_alpha(0.9)), Rect::new(row.x + 57.0, row.y, 80.0, row.h), Align::Left);
        ui.text(&d.letter().to_string(), Ui::plain_style("Helvetica", 11.0, true, Rgba::WHITE.with_alpha(0.85)), Rect::new(row.x + 4.0, row.y, 14.0, row.h), Align::Center);
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
    let sel = view.settings.list.map(|i| i as usize).unwrap_or_else(|| cats.iter().position(|(_, t)| t.contains(&r.io_type.as_str())).unwrap_or(0)).min(cats.len() - 1);
    let ccol = Rect::new(dcol.max_x() + 2.0, c.y, 158.0, c.h);
    let item = Ui::plain_style("Helvetica", 11.0, false, Rgba::WHITE.with_alpha(0.8));
    for (i, (name, _)) in cats.iter().enumerate() {
        let row = Rect::new(ccol.x, ccol.y + 2.0 + i as f32 * 40.0, ccol.w, 38.0);
        if i == sel {
            ui.fill(row, LIST_SEL);
        }
        ui.text(name, item.clone(), row.inset(10.0, 0.0, 24.0, 0.0), Align::Left);
        ui.text("▸", item.clone(), Rect::new(row.max_x() - 22.0, row.y, 14.0, row.h), Align::Center);
        ui.hit(row, sid(Control::ListSelect(i as u16)), HitKind::Tap);
    }
    let all = settings::io_routes(src, false);
    let chans: Vec<&settings::IoRoute> = all.iter().filter(|x| cats[sel].1.contains(&x.io_type.as_str())).collect();
    let xcol = Rect::new(ccol.max_x() + 2.0, c.y, c.max_x() - ccol.max_x() - 2.0, c.h);
    let ty = cats[sel].1.first().copied().unwrap_or("");
    for (i, ch) in chans.iter().enumerate() {
        let row = Rect::new(xcol.x, xcol.y + 2.0 + i as f32 * 40.0, xcol.w, 38.0);
        if ch.default_name == r.default_name {
            ui.fill(row, LIST_SEL);
        }
        ui.text(&ch.default_name, item.clone(), row.inset(10.0, 0.0, 4.0, 0.0), Align::Left);
        ui.hit(row, sid(Control::SettingChoose(format!("route:{node}:{ty}").into(), i as u16)), HitKind::Tap);
    }
}

// ---- TEMPO ([cap] s05; drive capture sb-tempo: 196 × 111 at (9, 963)) ----

/// The TEMPO window opens above the status bar's TEMPO field at the field's x, its bottom
/// 0.5 pt above the field (sb-tempo: (9, 963) over the (9, 1074.5) field); centred when the
/// field is not on screen.
fn tempo(ui: &mut Ui, view: &ViewState, bounds: Rect) {
    let size = (196.0, 111.0);
    let field = ui.hit_rect(&ControlId { strip: crate::surfaces::status::STATUS_ID.into(), control: Control::StatusTempo });
    let r = match field {
        Some(f) => Rect::new(f.x, (f.y - size.1 - 0.5).max(bounds.y), size.0, size.1),
        None => centred(bounds, size),
    };
    ui.chrome_layer(r, 27.0);
    // sb-tempo (2 px per pt): a 0.5-pt black outline, a 1-pt (147,153,157) border, a 26-pt dark
    // title bar (close X at the left, TEMPO centred), a light body.
    ui.fill(r.inset(-0.5, -0.5, -0.5, -0.5), Rgba::rgb8(3, 4, 4));
    ui.fill(r, Rgba::rgb8(147, 153, 157));
    let bar = Rect::new(r.x + 1.0, r.y + 1.0, r.w - 2.0, 26.0);
    ui.fill(bar, Rgba::rgb8(31, 34, 35));
    ui.fill(Rect::new(r.x + 1.0, bar.max_y(), r.w - 2.0, r.h - 28.0), Rgba::rgb8(176, 186, 192));
    ui.hit(r, pid(Control::PopoverDismiss), HitKind::Drag { value: 0.0, span: 1.0, reset: None });
    ui.text("TEMPO", Ui::plain_style("Helvetica", 11.5, true, Rgba::rgb8(187, 188, 188)), bar.offset(0.0, 1.5), Align::Center);
    if let Some(x) = ui.skin("views", "Browser-Modal-Close-White").and_then(|s| s.image("image")) {
        let x = x.to_string();
        ui.image(&x, Rect::new(r.x + 12.5, r.y + 8.5, 10.0, 10.5), None, None, 1.0);
    }
    ui.hit(Rect::new(r.x + 1.0, r.y + 1.0, 30.0, 26.0), sid(Control::PanelClose), HitKind::Tap);
    // The tempo field (80 × 24 at (12, 39)): the value selected when the window opens.
    let text = view.text_edit.as_ref().filter(|e| matches!(e.target, TextTarget::Tempo)).map(|e| e.text.clone()).unwrap_or_default();
    let fr = Rect::new(r.x + 12.0, r.y + 39.0, 80.0, 24.0);
    ui.fill(fr, Rgba::rgb8(78, 83, 88));
    let ts = Ui::plain_style("Helvetica", 11.0, false, Rgba::rgb8(191, 202, 214));
    if !text.is_empty() {
        let w = crate::window::helvetica_width(&text, ts.size).ceil();
        ui.fill(Rect::new(fr.x + 8.0, fr.y + 6.5, w + 2.0, 11.0), Rgba::rgb8(43, 79, 119));
    }
    ui.text(&text, ts, Rect::new(fr.x + 8.5, fr.y + 2.0, fr.w - 10.0, fr.h), Align::Left);
    // TAP (Switch-Large, 70 × 30 at (109.5, 36.5)): Console's tap tempo (`TapTempo`).
    ui.toggle("Switch-Large", Some("TAP"), false, true, Rect::new(r.x + 108.5, r.y + 35.5, 71.0, 31.0), Some(pid(Control::TempoTap)), None);
    let button = |ui: &mut Ui, label: &str, fill: Rgba, b: Rect, id: ControlId| {
        ui.fill(b, fill);
        ui.text(label, Ui::plain_style("Helvetica", 10.5, true, Rgba::WHITE), b.offset(0.0, 1.25), Align::Center);
        ui.hit(b, id, HitKind::Tap);
    };
    button(ui, "CANCEL", Rgba::rgb8(54, 70, 82), Rect::new(r.x + 12.0, r.y + 75.0, 80.0, 24.0), pid(Control::TextCancel));
    button(ui, "OK", Rgba::rgb8(0, 84, 140), Rect::new(r.x + 104.0, r.y + 75.0, 80.0, 24.0), pid(Control::TextCommit));
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
