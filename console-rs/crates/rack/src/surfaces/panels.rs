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
use crate::surfaces::settings::{CELL_PITCH_X, Drawn, centred, choice_box, choice_menu, field_grid, section_header, section_header_text, sid, text_box, window_frame};
use crate::surfaces::view::{TextTarget, ViewState};
use crate::ui::Ui;


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

/// `Rack_Popover::paint` 0x181164400 / `::resized` 0x1811647e0: `Popover-Header` over
/// (0, 0, w, 27) and `Popover-Content` below it; the close X (`Popover-Close-Old`, 12 × 12)
/// centred in (8, 1, 20, 27); the title (`Rack_CalloutBoxHeader`, `Button-Popover-Header`)
/// centred in (0, 0, w, 26). Swallows taps on the body.
fn popover_frame(ui: &mut Ui, r: Rect, title: &str) {
    popover_frame_with(ui, r, title, true);
}

/// [`popover_frame`], with or without the close X (dialogs such as ADD DEVICE have none).
fn popover_frame_with(ui: &mut Ui, r: Rect, title: &str, close: bool) {
    ui.chrome_layer(r, 27.0);
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
    let hp = model.headphone_outputs();
    let rows = hp.len().max(2) as f32;
    let col_w = 90.0;
    let size = (164.0 + n as f32 * col_w + 4.0, 149.0 + rows * 44.0 + 1.0);
    // Opens beside its button; Console's window may leave the main window, ours stays inside.
    let anchor = ui.hit_rect(&ControlId { strip: "monitor".into(), control: Control::CueOutputs });
    let r = match anchor {
        Some(a) => Rect::new((a.x - 172.0).min(bounds.max_x() - size.0).max(bounds.x), (a.y - 19.0).clamp(bounds.y, (bounds.max_y() - size.1).max(bounds.y)), size.0, size.1),
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
        // The headphone tile: which HP output plays this cue. The lit tile opens the HP's
        // source menu (`MixInSource`: MON, CUE 1…4, NONE); an empty cell assigns this cue.
        let cue = format!("CUE {}", k + 1);
        for (h, o) in hp.iter().enumerate() {
            let tile = Rect::new(cx + 1.0, row.y + 1.0 + h as f32 * 43.0, 89.0, 41.0);
            if o.options.get(o.source) == Some(&cue) {
                let mix = fields.iter().find(|f| f.label == "SOURCE").map(|f| f.value_text()).unwrap_or_default();
                let what = if mix == "MIX" { "MONITOR MIX".to_string() } else { format!("CUE {} MIX", k + 1) };
                // The tile art fills the 89 × 41 tile (`Switch-NoDedicatedCue-A`, stretched).
                ui.toggle("Switch-NoDedicatedCue-A", Some(&format!("{what}\nto\n{} HP {}", dev.letter(), h + 1)), true, true, tile, None, None);
                ui.hit(tile, ControlId { strip: "monitor".into(), control: Control::HeadphoneMenu(h as u8) }, HitKind::Tap);
            } else if let Some(i) = o.options.iter().position(|l| *l == cue) {
                ui.hit(tile, ControlId { strip: "monitor".into(), control: Control::HeadphoneSource { output: h as u8, option: i as u8 } }, HitKind::Tap);
            }
        }
    }
}

/// `Rack_SubWindow` chrome (the cue / monitor sub-windows; capture panel-cue): a 1-px dark
/// outline, the grey frame (lighter under the title), the title centred in the 27-pt bar and
/// the close X at the left. Swallows taps.
fn sub_window_frame(ui: &mut Ui, r: Rect, title: &str) {
    ui.chrome_layer(r, 27.0);
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

// ---- MONITOR CONTROLLER (drive capture panel-monctl "Monitor", 812 × 527) ----

/// `Rack_MonitorSubWindow` (resized 0x181148550): the knob section (5, 32, 264, 98), the
/// BASS MANAGEMENT / SPEAKER UTILITIES tabs (5, h − 389, 132, 25 each) over the 264 × 359
/// section at (5, 163), and right of them the MAIN MONITOR CORRECTION label (279, 37, w − 284,
/// 25) over the room correction section (279, 62, w − 284, h − 67).
fn monitor_controller(ui: &mut Ui, model: &dyn MixerModel, view: &ViewState, bounds: Rect, drawn: &mut Vec<Drawn>) {
    let r = centred(bounds, (812.0, 527.0));
    sub_window_frame(ui, r, "MONITOR CONTROLLER");
    ui.fill(Rect::new(r.x + 0.5, r.y + 27.0, r.w - 1.5, r.h - 27.5), Rgba::rgb8(81, 88, 91));
    let Some(src) = model.properties() else { return };
    let d = device_of(model, view);
    let dev = format!("/devices/{d}");
    let (x0, y0) = (r.x, r.y);
    // Monitor knob section (`Monitor-Knob-Section-Background`, 264 × 98): MONITOR MODE and its
    // display, the monitor knob with its readout, MONO / MUTE.
    let knob = Rect::new(x0 + 5.0, y0 + 32.0, 264.0, 98.0);
    ui.background("Monitor-Knob-Section-Background", knob);
    let mode = settings::fields(src, SettingsGroup::MonitorKnob(d));
    let cr = Ui::plain_style("Helvetica", 9.0, false, Rgba::rgb8(116, 122, 126));
    ui.text("MONITOR MODE", cr, Rect::new(knob.x + 8.0, knob.y + 32.0, 80.0, 12.0), Align::Left);
    let mode_text = mode.iter().find(|f| f.label == "MONITOR MODE").map(|f| f.value_text()).unwrap_or_else(|| "STEREO".into());
    let mbox = Rect::new(knob.x + 8.0, knob.y + 44.0, 67.0, 19.0);
    ui.background("Combo-Box", mbox);
    ui.text(&mode_text, Ui::plain_style("Helvetica", 10.0, false, Rgba::WHITE.with_alpha(0.85)), mbox.offset(0.0, 0.5), Align::Center);
    let m = model.monitor();
    ui.knob("Monitor-Green", m.level, Rect::new(knob.x + 93.0, knob.y + 6.0, 77.0, 87.0), Some(ControlId { strip: "monitor".into(), control: Control::MonitorLevel }), None);
    let lvl = mode.iter().find(|f| f.label == "LEVEL").map(|f| f.value_text()).unwrap_or_else(|| m.readout.clone());
    ui.text(&lvl, Ui::plain_style("Helvetica", 10.0, true, Rgba::WHITE.with_alpha(0.75)), Rect::new(knob.x + 99.0, knob.y + 76.0, 65.0, 12.0), Align::Center);
    ui.toggle("Switch", Some("MONO"), m.mono, true, Rect::new(knob.x + 186.0, knob.y + 32.0, 36.0, 36.0), Some(ControlId { strip: "monitor".into(), control: Control::MonitorMono }), None);
    ui.toggle("Switch-Red", Some("MUTE"), m.mute, true, Rect::new(knob.x + 223.0, knob.y + 32.0, 36.0, 36.0), Some(ControlId { strip: "monitor".into(), control: Control::MonitorMute }), None);
    // BASS MANAGEMENT / SPEAKER UTILITIES (`Tab-Section-Left` / `-Right`).
    let tab = view.settings.panel_tab.min(1);
    for (k, label) in ["BASS MANAGEMENT", "SPEAKER UTILITIES"].into_iter().enumerate() {
        let b = Rect::new(x0 + 5.0 + k as f32 * 132.0, y0 + 138.0, 132.0, 25.0);
        ui.toggle(if k == 0 { "Tab-Section-Left" } else { "Tab-Section-Right" }, Some(label), tab as usize == k, true, b, None, None);
        ui.hit(b, sid(Control::PanelTab(k as u8)), HitKind::Tap);
    }
    let area = Rect::new(x0 + 5.0, y0 + 163.0, 264.0, 359.0);
    ui.background("Bass-Management-Background", area);
    let group = if tab == 0 { SettingsGroup::BassManagement(d) } else { SettingsGroup::SpeakerUtilities(d) };
    let f = settings::fields(src, group);
    // `Rack_BassManagement` (capture): ENABLE (11, 11, 65, 29); LFE FILTER / SATELLITE FILTER
    // labels at y 11 over 78 × 18 combos at (87, 22) / (177, 22); CHANNEL (11, 59, 65) and
    // SATELLITE CROSSOVER (87, 59, 168) under labels at y 47.
    let label_at = |ui: &mut Ui, t: &str, x: f32, y: f32, w: f32| {
        let st = Ui::plain_style("Helvetica", 9.0, false, Rgba::rgb8(116, 122, 126));
        ui.text(t, st, Rect::new(area.x + x + 2.0, area.y + y, w, 9.0), Align::Left);
    };
    if tab == 0 {
        let slots = [(11.0, 11.0, 65.0), (87.0, 11.0, 78.0), (177.0, 11.0, 78.0), (11.0, 47.0, 65.0), (87.0, 47.0, 168.0)];
        for (fl, (dx, dy, w)) in f.iter().zip(slots) {
            match &fl.kind {
                FieldKind::Toggle { on } => {
                    let b = Rect::new(area.x + dx, area.y + dy, w, 29.0);
                    ui.toggle("Switch-Large-Stretch", Some(&fl.label), *on, true, b, None, None);
                    ui.hit(b, sid(Control::SettingPress(fl.key.clone())), HitKind::Toggle);
                }
                _ => {
                    label_at(ui, &fl.label, dx, dy, w + 20.0);
                    choice_box(ui, fl, Rect::new(area.x + dx, area.y + dy + 11.0, w, 18.0));
                    drawn.push(Drawn { field: fl.clone() });
                }
            }
        }
    } else {
        for (k, fl) in f.iter().enumerate() {
            let (dx, dy) = (11.0 + (k % 3) as f32 * 83.0, 11.0 + (k / 3) as f32 * 36.0);
            label_at(ui, &fl.label, dx, dy, 80.0);
            let b = Rect::new(area.x + dx, area.y + dy + 11.0, 78.0, 18.0);
            match &fl.kind {
                FieldKind::Toggle { on } => {
                    ui.toggle("Switch-Large-Stretch", Some(if *on { "ON" } else { "OFF" }), *on, true, b, None, None);
                    ui.hit(b, sid(Control::SettingPress(fl.key.clone())), HitKind::Toggle);
                }
                FieldKind::Choice { .. } => {
                    choice_box(ui, fl, b);
                    drawn.push(Drawn { field: fl.clone() });
                }
                _ => ui.text(&fl.value_text(), Ui::plain_style("Helvetica", 10.0, true, Rgba::WHITE.with_alpha(0.75)), b, Align::Center),
            }
        }
    }
    // The speaker display (`Monitor-Surround-Speakers-Background`, 254 × 270) with the L / R
    // speaker icons and their N/A trims.
    let disp = Rect::new(area.x + 5.0, area.y + 85.0, 254.0, 270.0);
    ui.background("Monitor-Surround-Speakers-Background", disp);
    let spk = Ui::plain_style("Helvetica", 9.0, false, Rgba::WHITE.with_alpha(0.7));
    for (k, (key, x)) in [("Monitor-Selected-Left", 17.0), ("Monitor-Selected-Right", 215.0)].into_iter().enumerate() {
        ui.toggle(key, None, false, true, Rect::new(disp.x + x, disp.y + 19.0, 20.0, 20.0), None, None);
        ui.text("N/A", spk.clone(), Rect::new(disp.x + x - 10.0 + if k == 0 { -2.5 } else { 2.5 }, disp.y + 45.0, 40.0, 12.0), Align::Center);
        let dots = Rect::new(disp.x + if k == 0 { 4.0 } else { 202.0 }, disp.y + 57.0, 38.0, 1.0);
        for i in 0..19 {
            ui.fill(Rect::new(dots.x + 2.0 * i as f32, dots.y, 1.0, 0.5), Rgba::WHITE.with_alpha(0.35));
        }
    }
    // MAIN MONITOR CORRECTION (`Rack_RoomCorrection`).
    let right = Rect::new(x0 + 279.0, y0 + 37.0, r.w - 284.0, r.h - 42.0);
    ui.fill(right, Rgba::rgb8(65, 71, 74));
    ui.text("MAIN MONITOR CORRECTION", Ui::plain_style("Helvetica", 10.0, true, Rgba::WHITE.with_alpha(0.35)), Rect::new(right.x, right.y + 6.0, right.w, 14.0), Align::Center);
    let rc = settings::fields(src, SettingsGroup::RoomCorrection(d));
    let profile = rc.iter().find(|f| f.label == "CALIBRATION PROFILE").map(|f| f.value_text()).unwrap_or_else(|| "<PROFILE>".into());
    ui.text("CALIBRATION PROFILE", Ui::plain_style("Helvetica", 10.0, true, Rgba::WHITE.with_alpha(0.35)), Rect::new(right.x + 11.0, right.y + 67.0, 200.0, 12.0), Align::Left);
    let pbox = Rect::new(right.x + 10.0, right.y + 83.0, right.w - 20.0, 20.0);
    ui.background("Combo-Box", pbox);
    ui.text(&profile, Ui::plain_style("Helvetica", 11.0, false, Rgba::WHITE.with_alpha(0.85)), pbox.inset(8.0, 0.0, 120.0, 0.0), Align::Left);
    ui.fill(Rect::new(pbox.max_x() - 98.0, pbox.y + 6.0, 9.0, 8.0), Rgba::WHITE.with_alpha(0.85));
    ui.fill(Rect::new(pbox.max_x() - 97.0, pbox.y + 8.0, 7.0, 5.0), Rgba::rgb8(24, 25, 27));
    ui.text("OPEN SoundID", Ui::plain_style("Helvetica", 11.0, true, Rgba::WHITE.with_alpha(0.95)), Rect::new(pbox.max_x() - 84.0, pbox.y, 77.0, pbox.h), Align::Right);
    ui.hit(Rect::new(pbox.max_x() - 100.0, pbox.y, 100.0, pbox.h), sid(Control::SettingPress("post:ShowSoundIDReference:speaker".into())), HitKind::Tap);
    let graph=Rect::new(right.x, right.y + 113.0, right.w, 296.0);
    let image=model.correction_image(d as usize,"speaker").and_then(|svg|
        ui.assets.images.correction_svg("speaker",svg,((graph.w*2.0).round() as u32,592)));
    if let Some(key)=image {
        ui.fill(graph,Rgba::BLACK);
        ui.image(&key,graph,None,None,1.0);
    } else {
        correction_graph(ui,graph,None);
        ui.text("NO PROFILE IMAGE AVAILABLE",Ui::plain_style("Helvetica",10.0,true,Rgba::WHITE.with_alpha(0.6)),graph,Align::Center);
    }
    // SAFE HEADROOM, BYPASS; MAIN MONITOR CORRECTION ENABLE (70 × 30 at y 455).
    let by = y0 + 455.0;
    let head = rc.iter().find(|f| f.label == "HEADROOM").map(|f| f.value_text()).unwrap_or_default();
    for (label, x) in [("SAFE HEADROOM", 293.0), ("BYPASS", 374.0)] {
        if let Some(fl) = rc.iter().find(|f| f.label == label) {
            if let FieldKind::Toggle { on } = fl.kind {
                let b = Rect::new(x0 + x, by, 71.0, 30.0);
                let text = if label == "BYPASS" { "BYPASS".to_string() } else { format!("({head} SAFE\nHEADROOM)") };
                ui.toggle("Switch-Large-Stretch", Some(&text), on, true, b, None, None);
                ui.hit(b, sid(Control::SettingPress(fl.key.clone())), HitKind::Toggle);
            }
        }
    }
    let power = bool_at(src, &format!("{dev}/SpeakerCorrectionPower")).unwrap_or(false);
    ui.text("MAIN MONITOR\nCORRECTION", Ui::plain_style("Helvetica", 10.0, true, Rgba::WHITE.with_alpha(0.35)), Rect::new(x0 + 618.0, by, 100.0, 30.0), Align::Right);
    let er = Rect::new(x0 + 727.0, by, 71.0, 30.0);
    ui.toggle("Switch-Large-Stretch", Some("ENABLE"), power, true, er, None, None);
    ui.hit(er, sid(Control::SettingPress(format!("toggle:{dev}/SpeakerCorrectionPower").into())), HitKind::Toggle);
}

/// The correction display (black, `r` = 528 × 296 in the capture panel-monctl): log-frequency
/// grid lines 20 Hz…20 kHz (161.5 pt a decade, 100 Hz at x + 120), dB lines every 3 dB 18 pt
/// apart with 0 dB at y + 150.5 (the 6-dB ones run under the labels), the ±6/12/18 labels
/// right-aligned at x + 516 just under their lines, the frequency labels 5.5 pt right of the
/// 100 Hz / 1 kHz / 10 kHz lines. `curve` (dB at evenly spaced log frequencies, 20 Hz…20 kHz) is
/// Local placeholder samples only; GetCorrectionImage supplies an SVG and is rendered
/// separately above. Without a local curve this draws the reference grid/baseline.
pub(crate) fn correction_graph(ui: &mut Ui, r: Rect, curve: Option<&[f32]>) {
    ui.fill(r, Rgba::BLACK);
    let x_of = |hz: f32| r.x + 120.0 + 161.5 * (hz / 100.0).log10();
    let y_of = |db: f32| r.y + 150.5 - db * 6.0;
    let (gx0, gx1) = (x_of(20.0).round(), x_of(20000.0).round());
    let line = Rgba::WHITE.with_alpha(0.118);
    for k in -7i32..=7 {
        let y = y_of(k as f32 * 3.0) - 0.5;
        let long = k % 2 == 0 && k != 0 && k.abs() < 7;
        ui.fill(Rect::new(gx0, y, if long { r.x + 520.0 - gx0 } else { gx1 - gx0 }, 1.0), line);
    }
    for decade in [10.0f32, 100.0, 1000.0, 10000.0] {
        for m in 1..10 {
            let hz = decade * m as f32;
            if !(20.0..=20000.0).contains(&hz) {
                continue;
            }
            let x = x_of(hz).round() - 0.5;
            ui.fill(Rect::new(x, r.y + 7.0, 1.0, 274.0), line);
        }
    }
    let label = Ui::plain_style("Helvetica", 10.0, true, Rgba::WHITE.with_alpha(0.7));
    for k in [18, 12, 6, 0, -6, -12, -18] {
        let t = if k > 0 { format!("+{k} dB") } else { format!("{k} dB") };
        ui.text(&t, label.clone(), Rect::new(r.x + 446.0, y_of(k as f32) - 0.5, 70.0, 14.0), Align::Right);
    }
    for (hz, t) in [(100.0, "100 Hz"), (1000.0, "1 kHz"), (10000.0, "10 kHz")] {
        ui.text(t, label.clone(), Rect::new(x_of(hz).round() + 5.0, r.y + 283.0, 60.0, 14.0), Align::Left);
    }
    let green = Rgba::rgb8(120, 200, 110);
    match curve {
        Some(c) if c.len() > 1 => {
            let step = (gx1 - gx0) / (c.len() - 1) as f32;
            for (i, db) in c.iter().enumerate() {
                let x = gx0 + step * i as f32;
                let y = y_of(*db);
                ui.fill(Rect::new(x, y.min(y_of(0.0)), step.max(0.5), (y - y_of(0.0)).abs().max(0.5)), green.with_alpha(0.12));
                ui.fill(Rect::new(x, y - 0.5, step.max(0.5), 1.0), green);
            }
        }
        _ => ui.fill(Rect::new(gx0, y_of(0.0) - 0.5, gx1 - gx0, 1.0), green.with_alpha(0.6)),
    }
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

const SCENE_ROW_H: f32 = 32.0;

/// `Rack_SceneSection` (capture s10, at 1470 × 1105): a 290-pt column under the title bar —
/// header 30, the CURRENT / PRIMED inset (10…280, y 39…186) with RECALL / SAVE / ▲ ▼, then
/// the scene list from y 195, 31.75 pt rows. [I] Console inserts the column left of the
/// browser; here it covers the browser's left 290 pt.
/// PLUG-IN SCENES contents in `browser` (the window's scenes column, or centred in `bounds`);
/// `close` is what the header's X sends.
pub fn plugin_scenes(ui: &mut Ui, model: &dyn MixerModel, view: &ViewState, bounds: Rect, browser: Option<Rect>, close: Control) {
    let full = browser.unwrap_or_else(|| centred(bounds, (290.0, 560.0)));
    let r = Rect::new(full.x, full.y, 290.0f32.min(full.w), full.h);
    ui.chrome_layer(r, 33.0);
    // `Rack_SceneSection::resized` 0x1811f8630 (capture sb-scene): `Scenes-Main-Background`
    // (72, 76, 78) 288 wide; the close X in (260, 8, 20, 20); the `Scenes-Inset-Background`
    // (28, 29, 32) at (5, 33, w − 10, h − 38) holding the status panel (10, 38, w − 20, 150):
    // CURRENT / PRIMED captions at y 7 / 65 and their 253 × 30 boxes at y 26 / 85 (panel
    // coordinates), RECALL / SAVE (96 × 20) and ▲ ▼ (20 × 20) at y 123; the scene list from 193.
    let sw = 288.0f32.min(r.w);
    ui.fill(r, Rgba::rgb8(72, 76, 78));
    ui.hit(r, pid(Control::PopoverDismiss), HitKind::Drag { value: 0.0, span: 1.0, reset: None });
    let hs = ui.style("fonts", "Scenes-Header-Font");
    let tw = crate::window::helvetica_bold_width("PLUG-IN SCENES", hs.size);
    let tx = r.x + ((sw - tw - 18.0) / 2.0).round();
    ui.fill(Rect::new(tx, r.y + 12.0, 12.0, 12.0), Rgba::BLACK);
    ui.fill(Rect::new(tx + 1.0, r.y + 13.0, 10.0, 10.0), Rgba::rgb8(255, 0, 255));
    ui.text("PLUG-IN SCENES", hs, Rect::new(tx + 18.0, r.y + 3.5, tw + 4.0, 33.0), Align::Left);
    if let Some(x) = ui.skin("views", "Browser-Modal-Close-White").and_then(|s| s.image("image")) {
        let x = x.to_string();
        ui.image(&x, Rect::new(r.x + 264.0, r.y + 12.0, 12.0, 12.0), None, None, 0.95);
    }
    ui.hit(Rect::new(r.x + 254.0, r.y, 36.0, 33.0), sid(close), HitKind::Tap);

    let scenes = model.plugin_scenes();
    let current = model.status().scene;
    let primed = view.settings.scene.and_then(|i| scenes.get(i as usize).cloned());
    ui.fill(Rect::new(r.x + 5.0, r.y + 33.0, sw - 10.0, r.h - 38.0), Rgba::rgb8(28, 29, 32));
    let panel = Rect::new(r.x + 10.0, r.y + 38.0, sw - 20.0, 150.0);
    let framed = |ui: &mut Ui, b: Rect, edge: Rgba, fill: Rgba| {
        ui.fill(b, edge);
        ui.fill(b.inset(1.0, 1.0, 1.0, 1.0), fill);
    };
    framed(ui, Rect::new(panel.x, panel.y + 1.0, panel.w, panel.h), Rgba::rgb8(83, 86, 88), Rgba::rgb8(67, 70, 72));
    let ls = ui.style("fonts", "Scenes-Current-Primed-Label-Text");
    let vs = Ui::plain_style("Helvetica", 10.0, true, Rgba::WHITE.with_alpha(0.85));
    for (k, (label, value)) in [("CURRENT", current.clone()), ("PRIMED", primed)].into_iter().enumerate() {
        let y = panel.y + 7.0 + k as f32 * 58.0;
        ui.text(label, ls.clone(), Rect::new(panel.x + 7.0, y + 0.5, 253.0, 13.0), Align::Center);
        let v = Rect::new(panel.x + 7.0, y + 20.0, 253.0, 30.0);
        framed(ui, v, Rgba::rgb8(83, 86, 88), Rgba::rgb8(46, 49, 52));
        ui.text(value.as_deref().unwrap_or("(NONE)"), vs.clone(), v.offset(0.0, 1.5), Align::Center);
    }
    // RECALL, SAVE, ▲ ▼ (flat (89, 91, 94) plates with a 1-pt (109) rim).
    let by = panel.y + 124.0;
    let recall = Rect::new(panel.x + 7.0, by, 96.0, 20.0);
    let can_recall = view.settings.scene.is_some();
    let plate = |ui: &mut Ui, b: Rect| {
        ui.fill(b, Rgba::rgb8(109, 111, 113));
        ui.fill(b.inset(0.0, 1.0, 0.0, 1.0), Rgba::rgb8(89, 91, 94));
    };
    plate(ui, recall);
    let bs = Ui::plain_style("Helvetica", 10.0, true, Rgba::WHITE.with_alpha(if can_recall { 0.85 } else { 0.3 }));
    ui.text("RECALL", bs.clone(), recall, Align::Center);
    // RECALL / SAVE (`Rack_SceneSection::buttonClicked` 0x1811f7e80): RecallScene / StoreScene of
    // the selected (primed) row.
    let scene_cmd = |op, i: u16| ControlId { strip: "global".into(), control: Control::SceneCommand(op, i) };
    if let Some(i) = view.settings.scene {
        ui.hit(recall, scene_cmd(crate::menu::SceneOp::Recall, i), HitKind::Tap);
    }
    let save = Rect::new(panel.x + 110.0, by, 96.0, 20.0);
    plate(ui, save);
    ui.text("SAVE", bs.clone(), save, Align::Center);
    if let Some(i) = view.settings.scene {
        ui.hit(save, scene_cmd(crate::menu::SceneOp::Save, i), HitKind::Tap);
    }
    for (i, up) in [true, false].into_iter().enumerate() {
        let b = Rect::new(panel.x + 213.0 + i as f32 * 27.0, by, 20.0, 20.0);
        plate(ui, b);
        if let Some(img) = ui.skin("views", if up { "Popover-Up" } else { "Popover-Down" }).and_then(|s| s.image("image")) {
            let img = img.to_string();
            ui.image(&img, Rect::new(b.x + 4.0, b.y + 4.0, 12.0, 12.0), None, None, 0.6);
        }
        ui.hit(b, sid(Control::SceneStep(up)), HitKind::Tap);
    }
    // The scene list (`Rack_ScenesListBox`, (7, 193, w − 14, h − 200)).
    let list = Rect::new(r.x + 7.0, r.y + 193.0, sw - 14.0, (r.h - 200.0).max(0.0));
    ui.fill(list, Rgba::rgb8(28, 29, 32));
    let overflow = (scenes.len() as f32 * SCENE_ROW_H - list.h).max(0.0);
    let off = overflow * view.settings.scroll;
    // Clip from the column's left edge so the rows' hits keep x = 0 (the row menus anchor on it).
    ui.clip(Some(Rect::new(r.x, list.y, list.max_x() - r.x, list.h)));
    let mut st = ui.style("views", "Scenes-List-Box-Number");
    st.color = Rgba::WHITE.with_alpha(0.9);
    for (i, n) in scenes.iter().enumerate() {
        let y = list.y + i as f32 * SCENE_ROW_H - off;
        if y + SCENE_ROW_H < list.y || y > list.max_y() {
            continue;
        }
        let row = Rect::new(list.x - 7.0, y, list.w - 7.0, SCENE_ROW_H);
        if view.settings.scene == Some(i as u16) {
            ui.fill(row, Rgba::rgb8(64, 58, 40));
        } else if Some(n) == current.as_ref() {
            ui.fill(row, Rgba::WHITE.with_alpha(0.06));
        }
        ui.text(&format!("{}", i + 1), st.clone(), Rect::new(row.x + 14.0, y + 1.0, 28.0, SCENE_ROW_H), Align::Center);
        // Rename edits the row in place (`Rack_ScenesListBoxRow::EditSceneName`).
        let editing = view.text_edit.as_ref().filter(|e| e.target == TextTarget::SceneName(i as u16));
        if editing.is_some() {
            ui.fill(Rect::new(row.x + 56.0, y + 6.0, row.w - 60.0, SCENE_ROW_H - 12.0), Rgba::rgb8(78, 83, 88));
        }
        ui.text(editing.map_or(n.as_str(), |e| e.text.as_str()), st.clone(), Rect::new(row.x + 59.0, y + 1.0, row.w - 62.0, SCENE_ROW_H), Align::Left);
        ui.hit(row, sid(Control::SceneSelect(i as u16)), HitKind::Tap);
    }
    ui.clip(None);
    // `Small-Vertical-Scroll-Bar` thumb in the list's right 18 pt (sb-scene).
    if overflow > 0.0 {
        let track = Rect::new(list.max_x() - 18.0, list.y + 4.0, 18.0, list.h - 8.0);
        let thumb_h = (track.h * list.h / (scenes.len() as f32 * SCENE_ROW_H)).round().max(18.0);
        let ty = (track.y + (track.h - thumb_h) * view.settings.scroll.clamp(0.0, 1.0)).round();
        if let Some(img) = ui.skin("backgrounds", "Small-Vertical-Scroll-Bar").and_then(|s| s.image("image")) {
            let img = img.to_string();
            ui.image_stretched(&img, Rect::new(track.x, ty, track.w, thumb_h), 1.0);
        }
    }
    ui.scroll_region(list, ScrollTarget::Settings, overflow);
}

// ---- Monitor Meters ([cap] s44, 164 × 645) ----

fn meter_window(ui: &mut Ui, model: &dyn MixerModel, bounds: Rect) {
    // `Rack_MonitorMetersWindow` (resized 0x180d9f0c0; capture s44, 118 × 599): the sub-window
    // header, then the monitor meters on (43, 46, 48): the UA logo, two `Monitor-Meter` bars
    // 23.5 wide at x 27 / 69 from y 74 to 565, scale rows from y 80 to 563 (12 marks) with 3-pt
    // ticks outside the bars, 1-pt dots between, 2-pt dots between the bars, the numbers centred
    // at x 14.4 / 59.4 / 104.4, and L / R under the bars.
    let r = centred(bounds, (118.0, 599.0));
    sub_window_frame(ui, r, "");
    ui.fill(Rect::new(r.x + 0.5, r.y + 27.0, r.w - 1.5, r.h - 27.5), Rgba::rgb8(43, 46, 48));
    ui.background("UA-Logo", Rect::new(r.x + 44.0, r.y + 37.0, 30.0, 24.0));
    let m: MeterFrame = model.monitor_meters();
    let left = Rect::new(r.x + 26.75, r.y + 74.0, 23.5, 491.0);
    let right = Rect::new(r.x + 68.75, r.y + 74.0, 23.5, 491.0);
    ui.bar_meter("Monitor-Meter", m.left, m.peak_left, m.clip_left, left);
    ui.bar_meter("Monitor-Meter", m.right, m.peak_right, m.clip_right, right);
    let skin = ui.skin("meters", "Monitor-Meter");
    let tick = skin.and_then(|s| s.color("color-text")).unwrap_or(Rgba::rgb8(93, 101, 105));
    let mut numbers = ui.style("meters", "Monitor-Meter");
    numbers.color = tick;
    let n = crate::scales::METER_MARKS.len();
    let rows: Vec<f32> = (0..n).map(|i| (r.y + 80.0 + (563.0 - 80.0) * i as f32 / (n - 1) as f32 + 0.5).floor()).collect();
    for (i, &y) in rows.iter().enumerate() {
        ui.fill(Rect::new(r.x + 21.75, y, 3.0, 1.0), tick);
        ui.fill(Rect::new(r.x + 94.0, y, 3.0, 1.0), tick);
        if i > 0 {
            let ym = ((rows[i - 1] + y) / 2.0 + 0.5).floor();
            ui.fill(Rect::new(r.x + 23.75, ym, 1.0, 1.0), tick);
            ui.fill(Rect::new(r.x + 94.0, ym, 1.0, 1.0), tick);
            ui.fill(Rect::new(r.x + 58.25, ym, 2.0, 1.0), tick);
        }
        let db = -crate::scales::METER_MARKS[i] as i32;
        for cx in [14.4, 59.4, 104.4] {
            ui.text(&db.to_string(), numbers.clone(), Rect::new(r.x + cx - 10.0, y - 4.0, 20.0, 9.0), Align::Center);
        }
    }
    for (label, meter) in [("L", left), ("R", right)] {
        ui.font_text("Silk-Screen-Surround-Mode", label, Rect::new(meter.x, meter.max_y() + 9.5, meter.w, 16.0), Align::Center);
    }
}

// ---- Sends ([cap] s50 TALKBACK, s51 AUX n; 97-pt columns, 408 tall) ----

/// The window title and send nodes of `of`: (`…/sends/N`, name), cues within `CueBusCount`.
fn send_nodes(src: &dyn PropertySource, of: SendsOf) -> (String, Vec<(String, String)>) {
    let device = of.parts().map(|p| p.0).unwrap_or(0);
    let dev = format!("/devices/{device}");
    let node = match of {
        SendsOf::Talkback => src.indices(&format!("{dev}/inputs")).into_iter().map(|i| format!("{dev}/inputs/{i}")).find(|p| str_at(src, &format!("{p}/IOType")).as_deref() == Some("TalkbackMic")),
        _ => of.node(),
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
        _ => str_at(src, &format!("{node}/Name")).unwrap_or_else(|| "SENDS".into()),
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
        SendsOf::Talkback => ui.hits.iter().find(|h| h.id.control == Control::ShowPanel(Panel::Sends(SendsOf::Talkback))).map(|h| h.id.strip.clone()),
        _ => of.node().map(Into::into),
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
        let mono_input = of.is_input() && src.prop(&format!("{node}/Pan")).is_some();
        let pan_knob = ((src.prop(&format!("{node}/Pan")).and_then(|v| v.as_f64()).unwrap_or(0.0) + 1.0) / 2.0).clamp(0.0, 1.0);
        let pan = mono_input.then(|| (pan_knob, Some(sid(Control::SettingSet(format!("pan:{node}/Pan").into())))));
        let ids = crate::surfaces::cells::FullSendIds {
            bus: !of.is_input(),
            pan,
            mute: Some(sid(Control::SettingPress(format!("toggle:{node}/Bypass").into()))),
            level: Some(sid(Control::SettingSet(format!("fader:{node}/GainTapered").into()))),
        };
        let send = crate::model::SendState { name: name.clone(), level, muted, pan: pan_knob };
        ui.at(x, r.y + 31.0, |ui| crate::surfaces::cells::full_send_strip(ui, "Sends-Input-Fader", &send, &db_readout(db), 0.0, r.h - 36.0, ids));
    }
    // ‹ › step through the auxes, or through the input strips: `Popover-Left` / `-Right`
    // (12 × 12 chevrons) centred in (w − 58, 1, 20, 27) and (w − 28, 1, 20, 27), dimmed at
    // the ends.
    if let Some((device, kind, k)) = of.parts() {
        let exists = |t: u8| src.prop(&format!("/devices/{device}/{kind}/{t}/Name")).is_some();
        let prev = (0..k).rev().find(|t| exists(*t));
        let next = (k.saturating_add(1)..=u8::MAX).take(64).find(|t| exists(*t));
        for (i, (key, to)) in [("Popover-Left", prev), ("Popover-Right", next)].into_iter().enumerate() {
            let b = Rect::new(r.max_x() - 58.0 + i as f32 * 30.0, r.y + 1.0, 20.0, 27.0);
            if let Some(img) = ui.skin("views", key).and_then(|s| s.image("image")) {
                let img = img.to_string();
                ui.image(&img, Rect::new(b.x + 4.0, b.y + 7.5, 12.0, 12.0), None, None, if to.is_some() { 1.0 } else { 0.3 });
            }
            if let Some(to) = to {
                ui.hit(b, sid(Control::ShowPanel(Panel::Sends(of.with_index(to)))), HitKind::Tap);
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
    ui.chrome_layer(r, 27.0);
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
    // TAP (Switch-Large, 70 × 30 at (109.5, 36.5)): Console's tap tempo (`TapTempo`).
    ui.toggle("Switch-Large", Some("TAP"), false, true, Rect::new(r.x + 108.5, r.y + 35.5, 71.0, 31.0), Some(pid(Control::TempoTap)), None);
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
