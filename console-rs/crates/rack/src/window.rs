//! The whole Console mixer window (`mixer-geometry.md` §0–§3).

use houston::{Rect, Rgba};

use crate::draw::Align;
use crate::geometry::{MainStripGeometry, STRIP_W, StripKind, main_strip, window as W};
use crate::input::{Control, ControlId};
use crate::model::{InsertState, MixerModel, StripInfo};
use crate::preamp::draw_preamp;
use crate::ui::Ui;

/// Column placement. `full` reproduces Console's 1920-pt window (browser + navigator,
/// aux at 1613.5, monitor at 1823.5); `compact` hides the browser/navigator and packs
/// aux and monitor after the last strip.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowLayout {
    pub compact: bool,
    pub inputs: usize,
    pub auxes: usize,
}

const GAP: f32 = 18.0;
const PANEL: Rgba = Rgba::rgb8(72, 76, 78);
const CHROME: Rgba = Rgba::rgb8(35, 37, 40);
const SEPARATOR: Rgba = Rgba::rgb8(82, 87, 89);

impl WindowLayout {
    pub fn side_x(&self) -> f32 {
        if self.compact { 0.0 } else { W::SIDE_SECTION.0 }
    }
    pub fn strips_x(&self) -> f32 {
        self.side_x() + W::SIDE_SECTION.1
    }
    pub fn aux_x(&self) -> f32 {
        let packed = self.strips_x() + self.inputs as f32 * STRIP_W + GAP;
        if self.compact { packed } else { packed.max(W::AUX_X) }
    }
    pub fn monitor_x(&self) -> f32 {
        let packed = self.aux_x() + self.auxes as f32 * STRIP_W + GAP;
        if self.compact { packed } else { packed.max(W::MONITOR_X) }
    }
    pub fn size(&self) -> (f32, f32) {
        let w = self.monitor_x() + STRIP_W + if self.compact { 4.0 } else { 0.5 };
        (if self.compact { w } else { w.max(W::SIZE.0) }, W::SIZE.1)
    }
}

fn cid(strip: &StripInfo, control: Control) -> Option<ControlId> {
    Some(ControlId { strip: strip.id.clone(), control })
}

pub fn build_window(ui: &mut Ui, model: &dyn MixerModel, compact: bool) -> WindowLayout {
    let inputs = model.inputs();
    let auxes = model.auxes();
    let l = WindowLayout { compact, inputs: inputs.len(), auxes: auxes.len() };
    let (width, height) = l.size();
    let col_h = W::COLUMN.1 - W::COLUMN.0;

    ui.fill(Rect::new(0.0, 0.0, width, height), Rgba::rgb8(41, 43, 46));
    // Title bar.
    ui.fill(Rect::new(0.0, 0.0, width, W::TITLE_BAR), CHROME);
    let title = Ui::plain_style("Helvetica Neue", 12.0, true, Rgba::WHITE.with_alpha(0.85));
    ui.text(&format!("Apollo: {}", model.session_name()), title, Rect::new(97.0, 0.0, 400.0, W::TITLE_BAR), Align::Left);
    if !compact {
        ui.fill(Rect::new(0.0, W::TITLE_BAR, W::BROWSER_W, W::STATUS_BAR.0 - W::TITLE_BAR), Rgba::rgb8(31, 33, 36));
        let hdr = Ui::plain_style("Helvetica", 11.0, true, Rgba::WHITE.with_alpha(0.7));
        ui.text("PLUG-INS", hdr.clone(), Rect::new(0.0, 32.0, 172.0, 24.0), Align::Center);
        ui.text("PRESETS", hdr, Rect::new(172.0, 32.0, 172.0, 24.0), Align::Center);
        ui.fill(Rect::new(W::NAV_COLUMN.0, W::TITLE_BAR, W::NAV_COLUMN.1, W::STATUS_BAR.0 - W::TITLE_BAR), Rgba::rgb8(44, 46, 49));
        let nav = Ui::plain_style("Helvetica", 10.0, true, Rgba::rgb8(255, 197, 74));
        for (i, t) in ["INPUTS", "INSERTS", "SENDS", "FADER"].iter().enumerate() {
            ui.text(t, nav.clone(), Rect::new(W::NAV_COLUMN.0 + 8.0, 86.0 + i as f32 * 26.0, 60.0, 14.0), Align::Left);
        }
    }
    // Strip area panel and session navigator.
    ui.fill(Rect::new(l.side_x(), W::COLUMN.0, l.aux_x() - l.side_x(), col_h), PANEL);
    ui.fill(Rect::new(l.side_x(), W::NAV_STRIP.0, l.aux_x() - l.side_x(), W::NAV_STRIP.1 - W::NAV_STRIP.0), Rgba::rgb8(28, 30, 32));

    for (i, s) in inputs.iter().enumerate() {
        strip_column(ui, model, s, Rect::new(l.strips_x() + i as f32 * STRIP_W, W::COLUMN.0, STRIP_W, col_h));
    }
    for (i, s) in auxes.iter().enumerate() {
        ui.fill(Rect::new(l.aux_x() + i as f32 * STRIP_W, W::COLUMN.0, STRIP_W, col_h), PANEL);
        strip_column(ui, model, s, Rect::new(l.aux_x() + i as f32 * STRIP_W, W::COLUMN.0, STRIP_W, col_h));
    }
    monitor_column(ui, model, Rect::new(l.monitor_x(), W::MONITOR_TOP, STRIP_W, W::STATUS_BAR.0 - W::MONITOR_TOP));
    status_bar(ui, Rect::new(0.0, W::STATUS_BAR.0, width, W::STATUS_BAR.1));
    l
}

fn strip_column(ui: &mut Ui, model: &dyn MixerModel, s: &StripInfo, r: Rect) {
    let main_y = r.h - main_strip::HEIGHT;
    ui.at(r.x, r.y, |ui| {
        ui.fill(Rect::new(1.0, 0.0, 94.0, 4.0), Rgba::rgb8(41, 86, 125)); // swatch
        ui.clip(Some(Rect::new(0.0, 5.0, STRIP_W, main_y - 5.0)));
        ui.at(0.0, 5.0, |ui| match s.kind {
            StripKind::Input => input_cells(ui, model, s),
            StripKind::Aux => aux_cells(ui, model, s),
        });
        ui.clip(None);
        ui.fill(Rect::new(0.0, 5.0, 1.0, main_y - 5.0), SEPARATOR);
        ui.fill(Rect::new(95.0, 5.0, 1.0, main_y - 5.0), SEPARATOR);
        ui.at(0.0, main_y, |ui| main_strip(ui, model, s));
    });
}

fn input_cells(ui: &mut Ui, model: &dyn MixerModel, s: &StripInfo) {
    ui.cell_header(&s.title, false, false, Rect::new(0.0, 0.0, STRIP_W, 22.0));
    if let Some(layout) = &s.preamp_layout {
        ui.background("Channel-Inserts-Background", Rect::new(4.0, 26.0, 88.0, 73.0));
        ui.font_text("Send-Label", "UNISON", Rect::new(4.0, 28.0, 88.0, 14.0), Align::Center);
        let unison = model.unison(s);
        ui.insert_slot(&unison, Rect::new(0.0, 44.0, STRIP_W, 48.0), cid(s, Control::Insert(255)));
        if let Some(state) = model.preamp(s) {
            draw_preamp(ui, layout, &state, &s.id, Rect::new(0.0, 101.0, STRIP_W, 129.0));
        }
    } else if s.shows_sr_convert {
        ui.background("Secondary-Display-Background", Rect::new(8.0, 174.0, 80.0, 48.0));
        ui.font_text("Send-Label", "SR CONVERT", Rect::new(8.0, 178.0, 80.0, 14.0), Align::Center);
        ui.toggle("Switch-Small", Some("OFF"), false, true, Rect::new(14.0, 196.0, 32.0, 19.0), None, None);
        ui.toggle("Switch-Small", Some("ON"), true, true, Rect::new(50.0, 196.0, 32.0, 19.0), None, None);
    }
    let y = W::INPUT_CELL_H;
    ui.cell_header("INSERTS", true, false, Rect::new(0.0, y, STRIP_W, 22.0));
    let slots = model.inserts(s);
    insert_list(ui, s, &slots, y + 26.0);
    let rec_y = y + 26.0 + slots.len() as f32 * 50.0 + 4.0;
    rec_mon(ui, model.records_pre_effects(s), s, rec_y);
    ui.cell_header("SENDS", false, false, Rect::new(0.0, y + W::INSERTS_CELL_H, STRIP_W, 22.0));
}

fn aux_cells(ui: &mut Ui, model: &dyn MixerModel, s: &StripInfo) {
    ui.cell_header("INSERTS", true, true, Rect::new(0.0, 0.0, STRIP_W, 22.0));
    insert_list(ui, s, &model.inserts(s), 26.0);
    let y = W::AUX_INSERTS_CELL_H;
    ui.cell_header("SENDS", false, true, Rect::new(0.0, y, STRIP_W, 22.0));
    for (k, send) in model.sends(s).iter().enumerate() {
        let sy = y + 26.0 + k as f32 * 78.0;
        ui.background("Send-Route-Background", Rect::new(4.0, sy, 88.0, 33.0));
        ui.font_text("Send-Label", &send.name, Rect::new(4.0, sy, 88.0, 33.0), Align::Center);
        ui.background("Secondary-Send-Background", Rect::new(6.0, sy + 34.0, 84.0, 38.0));
        ui.knob("Mini-Send-Level", send.level, Rect::new(7.0, sy + 31.0, 41.0, 49.0), cid(s, Control::SendLevel(k as u8)), Some(crate::scales::UNITY));
        ui.toggle("Mini-Send-Mute", Some("M"), send.muted, true, Rect::new(46.0, sy + 45.0, 32.0, 19.0), cid(s, Control::SendMute(k as u8)), None);
    }
}

fn insert_list(ui: &mut Ui, s: &StripInfo, slots: &[InsertState], y: f32) {
    ui.background("Channel-Inserts-Background", Rect::new(4.0, y, 88.0, slots.len() as f32 * 50.0 + 4.0));
    for (k, slot) in slots.iter().enumerate() {
        ui.insert_slot(slot, Rect::new(0.0, y + k as f32 * 50.0 + 3.0, STRIP_W, 48.0), cid(s, Control::Insert(k as u8)));
    }
}

fn rec_mon(ui: &mut Ui, monitor_only: bool, s: &StripInfo, y: f32) {
    let label = ui.style("fonts", "Send-Label");
    ui.text("UAD", label.clone(), Rect::new(14.0, y + 2.0, 28.0, 10.0), Align::Center);
    let (rec, mon) = if monitor_only {
        (Rgba::rgb8(90, 26, 16), Rgba::rgb8(77, 178, 242))
    } else {
        (Rgba::rgb8(237, 77, 38), Rgba::rgb8(51, 64, 77))
    };
    ui.fill(Rect::new(10.0, y + 15.0, 12.0, 12.0), rec);
    ui.fill(Rect::new(34.0, y + 15.0, 12.0, 12.0), mon);
    ui.text("REC", label.clone(), Rect::new(5.0, y + 29.0, 22.0, 10.0), Align::Center);
    ui.text("MON", label, Rect::new(28.0, y + 29.0, 24.0, 10.0), Align::Center);
    ui.hit(Rect::new(4.0, y + 10.0, 44.0, 20.0), ControlId { strip: s.id.clone(), control: Control::RecMon }, crate::input::HitKind::Toggle);
    ui.toggle("Switch", Some("INS"), false, true, Rect::new(50.0, y, 40.0, 40.0), None, None);
}

fn main_strip(ui: &mut Ui, model: &dyn MixerModel, s: &StripInfo) {
    let g = MainStripGeometry::new(s.kind);
    let st = model.main(s);
    let meters = model.meters(s);
    match s.kind {
        StripKind::Input => ui.background("Channel-Bottom", Rect::new(0.0, 0.0, STRIP_W, g.label.y)),
        StripKind::Aux => ui.fill(Rect::new(0.0, 0.0, STRIP_W, main_strip::HEIGHT), Rgba::rgb8(56, 60, 62)),
    }
    if let Some(p) = g.pans {
        let half = (p.w / 2.0).floor();
        if s.stereo {
            ui.knob("Pan-Black", st.pan_l, Rect::new(p.x, p.y, half, p.h), cid(s, Control::PanL), Some(0.0));
            ui.knob("Pan-Black", st.pan_r, Rect::new(p.x + half, p.y, half, p.h), cid(s, Control::PanR), Some(1.0));
        } else {
            ui.knob("Pan-Black", st.pan_l, Rect::new(p.x + (half / 2.0).floor(), p.y, half, p.h), cid(s, Control::PanL), Some(0.5));
        }
    }
    if let Some(r) = g.solo_mute {
        let (bw, bh) = ui.skin("views", "Switch-Small").and_then(|k| k.size()).unwrap_or((32.0, 19.0));
        let y = r.y + ((r.h - bh) / 2.0).floor();
        let mid = (r.w / 2.0).floor();
        ui.toggle("Switch-Small", Some("S"), st.solo, true, Rect::new(mid - bw - 3.0, y, bw, bh), cid(s, Control::Solo), None);
        ui.toggle("Switch-Small-Red", Some("M"), st.mute, true, Rect::new(mid + 3.0, y, bw, bh), cid(s, Control::Mute), None);
    }
    if let Some(r) = g.pre_post {
        ui.toggle("Switch", Some("PRE"), st.pre, true, Rect::new(8.0, r.y, 40.0, 40.0), cid(s, Control::PrePost), None);
        ui.toggle("Switch", Some("POST"), !st.pre, true, Rect::new(48.0, r.y, 40.0, 40.0), cid(s, Control::PrePost), None);
    }
    if let Some(r) = g.mono_mute {
        ui.toggle("Switch", Some("MONO"), st.mono, true, Rect::new(8.0, r.y, 40.0, 40.0), cid(s, Control::Mono), None);
        ui.toggle("Switch-Red", Some("MUTE"), st.mute, true, Rect::new(48.0, r.y, 40.0, 40.0), cid(s, Control::Mute), None);
    }
    let fader_key = if s.kind == StripKind::Aux { "Main-Bus-Fader" } else { "Main-Input-Fader" };
    ui.fader_and_meters(fader_key, s.stereo, st.fader, meters, &st.readout, g.fader_meters, cid(s, Control::Fader));
    ui.name_plate(if s.kind == StripKind::Aux { "Channel-Aux-Name" } else { "Channel-Input-Name" }, &st.name, g.label);
}

fn monitor_column(ui: &mut Ui, model: &dyn MixerModel, r: Rect) {
    let m = model.monitor();
    let meters = model.monitor_meters();
    let id = |c: Control| Some(ControlId { strip: "monitor".into(), control: c });
    ui.at(r.x, r.y, |ui| {
        ui.fill(Rect::new(0.0, 0.0, STRIP_W, r.h), PANEL);
        ui.bar_meter("Monitor-Meter-Left", meters.left, meters.clip_left, Rect::new(26.0, 10.0, 16.0, 495.0));
        ui.bar_meter("Monitor-Meter-Right", meters.right, meters.clip_right, Rect::new(59.0, 10.0, 16.0, 495.0));
        ui.background("Silk-Screen", Rect::new(4.0, 521.0, 88.0, 354.0));
        ui.font_text("CR-Label", "SHOW", Rect::new(4.0, 525.0, 88.0, 16.0), Align::Center);
        ui.toggle("Switch-Tight-Kerning", Some("AUX"), true, true, Rect::new(8.0, 541.0, 40.0, 40.0), None, None);
        ui.toggle("Switch-Tight-Kerning", Some("CTRL\nROOM"), false, true, Rect::new(48.0, 541.0, 40.0, 40.0), None, None);
        ui.toggle("Switch-Popover-Left", Some("CUE\nOUTPUTS"), false, true, Rect::new(8.0, 581.0, 80.0, 40.0), None, None);
        ui.toggle("Switch-Large-Tight-Kerning", Some("MONITOR\nCONTROLLER"), false, true, Rect::new(8.0, 621.0, 80.0, 40.0), None, None);
        ui.font_text("CR-Label", "MONITOR\nCORRECTION", Rect::new(0.0, 669.0, 96.0, 28.0), Align::Center);
        for (i, t) in ["HEADPHONES", "MAIN MON"].iter().enumerate() {
            let y = 699.0 + i as f32 * 54.0;
            ui.background("Secondary-Display-Background", Rect::new(6.0, y, 84.0, 50.0));
            ui.background("Display-Background", Rect::new(10.0, y + 4.0, 76.0, 28.0));
            ui.font_text("Channel-Strip-Section-Header", t, Rect::new(0.0, y + 32.0, 96.0, 16.0), Align::Center);
        }
        ui.font_text("CR-Label", "OUTPUT", Rect::new(4.0, 815.0, 88.0, 16.0), Align::Center);
        ui.toggle("Switch-Tight-Kerning", Some("MONO"), m.mono, true, Rect::new(8.0, 831.0, 40.0, 40.0), id(Control::MonitorMono), None);
        ui.toggle("Switch-Red-Tight-Kerning", Some("MUTE"), m.mute, true, Rect::new(48.0, 831.0, 40.0, 40.0), id(Control::MonitorMute), None);
        ui.background("Monitor-Gain-Session", Rect::new(4.0, 879.0, 88.0, 96.0));
        if let Some(layout) = ui.assets.layout("gain_and_session") {
            let frames = layout.frames("default", 96.0, 96.0);
            ui.at(0.0, 879.0, |ui| {
                if let Some(&f) = frames.get("label") {
                    ui.font_text("Small-Label-Font", "MONITOR", f, Align::Center);
                }
                if let Some(&f) = frames.get("gain") {
                    ui.knob("Monitor-Green", m.level, f, id(Control::MonitorLevel), None);
                }
                if let Some(&f) = frames.get("gain_value") {
                    ui.font_text("Small-Label-Font", &m.readout, f, Align::Center);
                }
            });
        }
    });
}

fn status_bar(ui: &mut Ui, r: Rect) {
    ui.fill(r, CHROME);
    let key = Ui::plain_style("Helvetica", 10.0, false, Rgba::WHITE.with_alpha(0.35));
    let val = Ui::plain_style("Helvetica", 12.0, false, Rgba::WHITE.with_alpha(0.75));
    let mut x = 14.0;
    for (k, v) in [("TEMPO", "120.00"), ("RATE", "OFFLINE"), ("CLOCK", "OFFLINE"), ("SCENE", "(NONE)")] {
        ui.text(k, key.clone(), Rect::new(x, r.y, 50.0, r.h), Align::Left);
        ui.text(v, val.clone(), Rect::new(x + 52.0, r.y, 80.0, r.h), Align::Left);
        x += 150.0;
    }
}
