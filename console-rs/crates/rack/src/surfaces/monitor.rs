//! Monitor column (`Rack_MonitorSection_UAD2`, `mixer-geometry.md` §3) built bottom-up: the
//! monitor meters with the UA logo and scale, SHOW AUX / CTRL ROOM, CUE OUTPUTS, MONITOR
//! CONTROLLER, the MONITOR CORRECTION buttons, OUTPUT (optional MONITOR / ALT rows, MONO, MUTE)
//! and the MONITOR knob block; plus the control-room panel that replaces the aux returns on the
//! CTRL ROOM page (cue outputs, headphone sources, talkback and dim).
//!
//! Sources: Rack.dll 1.2.x (`resized` 0x1811ef010, init 0x1811ebab0, paint 0x1811eee40,
//! `Rack_MonitorMeters` resized 0x1809dbbe0 / stereo layout 0x1809d9980, `Rack_MeterMarks` styles
//! 0x1809d2030 / 0x1809d19b0 / 0x1809d2620, `Rack_RoomCorrectionButtonPainter` 0x1811f52b0 over
//! `Rack_InsertButtonPainter::PaintInsertContent` 0x18142c290), placed against the Console
//! 1.3.1 p1 refpair to the half point.

use houston::{Insets, Rect, Rgba};

use crate::draw::Align;
use crate::geometry::STRIP_W;
use crate::input::{Control, ControlId, HitKind};
use crate::model::{MeterFrame, MixerModel};
use crate::scales;
use crate::surfaces::view::{MonitorPage, Popover, ViewState};
use crate::ui::{ART_SCALE, Ui};

pub const MONITOR_ID: &str = "monitor";
/// Control-room panel fill.
const PANEL: Rgba = Rgba::rgb8(72, 76, 78);
/// `Rack_MonitorSection_UAD2::paint` fill (0x47, 0x4c, 0x4f); also the section separators.
const SECTION_BG: Rgba = Rgba::rgb8(71, 76, 79);

fn mid(control: Control) -> ControlId {
    ControlId { strip: MONITOR_ID.into(), control }
}

/// Toggle drawn by skin with a separate hit of `kind` (Tap for selections).
fn button(ui: &mut Ui, key: &str, label: &str, on: bool, r: Rect, control: Control, kind: HitKind) {
    ui.toggle(key, Some(label), on, true, r, None, None);
    ui.hit(r, mid(control), kind);
}

/// Bottom-up stack of the monitor column (`Rack_MonitorSection_UAD2::resized` 0x1811ef010),
/// column-relative y values. Optional rows are `None` when hidden.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MonitorStack {
    /// gain_and_session top (G = h − 4 − 96).
    pub gain: f32,
    pub mono_mute: f32,
    pub alt: Option<f32>,
    pub monitor_button: Option<f32>,
    pub output_label: f32,
    /// `rm_corr_btn` (MAIN MON) and `hp_corr_btn` (HEADPHONES), 50 tall.
    pub main_mon: Option<f32>,
    pub headphones: Option<f32>,
    /// `mon_corr_lbl` (28 tall), shown with either correction button.
    pub correction_label: Option<f32>,
    pub controller: Option<f32>,
    pub cue_outputs: f32,
    pub show_buttons: f32,
    pub show_label: f32,
    /// `controls_bg` (Silk-Screen) top; `monitor_meters` ends 4 pt above it.
    pub silk: f32,
}

impl MonitorStack {
    pub fn new(h: f32, alt_count: u8, controller: bool, headphones: bool, main_mon: bool) -> Self {
        let gain = h - 4.0 - 96.0;
        let mono_mute = gain - 48.0;
        let mut y = mono_mute;
        let (mut alt, mut monitor_button) = (None, None);
        if alt_count > 0 {
            alt = Some(gain - 88.0);
            monitor_button = Some(gain - 128.0);
            y = gain - 128.0;
        }
        let output_label = y - 16.0;
        let mut c = output_label;
        let (mut mm, mut hp, mut label) = (None, None, None);
        if headphones || main_mon {
            c = y - 28.0;
            if main_mon {
                c = y - 78.0;
                mm = Some(c);
            }
            if headphones {
                if main_mon {
                    c -= 4.0;
                }
                c -= 50.0;
                hp = Some(c);
            }
            c -= 30.0;
            label = Some(c);
        }
        let mut y = c - 8.0;
        let mut ctl = None;
        if controller {
            y = c - 48.0;
            ctl = Some(y);
        }
        let cue_outputs = y - 40.0;
        let show_buttons = cue_outputs - 40.0;
        let show_label = cue_outputs - 56.0;
        Self {
            gain,
            mono_mute,
            alt,
            monitor_button,
            output_label,
            main_mon: mm,
            headphones: hp,
            correction_label: label,
            controller: ctl,
            cue_outputs,
            show_buttons,
            show_label,
            silk: cue_outputs - 60.0,
        }
    }
}

/// `Rack_MonitorMeters` (stereo), column-relative: Display-Nav panel, UA logo, two
/// `Monitor-Meter` bars, the `Rack_MeterMarks` scale and the L / R labels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MonitorMetersGeometry {
    pub background: Rect,
    pub logo: Rect,
    pub left: Rect,
    pub right: Rect,
    /// Lit content span of the bars (the scale's 0 dB and −60 dB rows).
    pub content: (f32, f32),
}

impl MonitorMetersGeometry {
    /// `bottom` = monitor_meters height (silk − 4); `meter_w` = `meters:Monitor-Meter` width.
    pub fn new(bottom: f32, meter_w: f32, logo_h: f32) -> Self {
        let background = Rect::new(0.0, 4.0, STRIP_W, bottom - 4.0);
        // Meter top = logo height + 4 (0x1809d75a0), measured from the panel top in 1.3.1;
        // height = panel − top − 12 − 16.
        let top = logo_h + 4.0;
        let h = background.h - top - 12.0 - 16.0;
        let half = (STRIP_W / 2.0).floor();
        let y = background.y + top;
        let left = Rect::new(half - meter_w - 8.0, y, meter_w, h);
        let right = Rect::new(half + 8.0, y, meter_w, h);
        Self { background, logo: Rect::new(0.0, 7.0, STRIP_W, logo_h), left, right, content: (y + 7.0, y + h - 2.0) }
    }

    /// Scale rows for `scales::METER_MARKS`, rounded to whole points like Rack_MeterMarks.
    pub fn mark_rows(&self) -> Vec<f32> {
        let (t, b) = self.content;
        let n = scales::METER_MARKS.len() as f32 - 1.0;
        (0..scales::METER_MARKS.len()).map(|i| (t + (b - t) * i as f32 / n + 0.5).floor()).collect()
    }
}

/// The monitor column in `r` (window points; x = monitor column, y = section top).
pub fn draw_monitor_column(ui: &mut Ui, model: &dyn MixerModel, view: &ViewState, r: Rect) {
    let m = model.monitor();
    let opts = model.monitor_options();
    let st = MonitorStack::new(r.h, opts.alt_count, opts.controller, opts.hp_correction.is_some(), opts.speaker_correction.is_some());
    ui.at(r.x, r.y, |ui| {
        ui.fill(Rect::new(0.0, 0.0, STRIP_W, r.h), SECTION_BG);
        monitor_meters(ui, model.monitor_meters(), st.silk - 4.0);
        ui.background("Silk-Screen", Rect::new(4.0, st.silk, STRIP_W - 8.0, st.gain - 4.0 - st.silk));

        ui.font_text("CR-Label", "SHOW", Rect::new(4.0, st.show_label, 88.0, 16.0), Align::Center);
        let aux = view.monitor_page == MonitorPage::Aux;
        button(ui, "Switch", "AUX", aux, Rect::new(8.0, st.show_buttons, 40.0, 40.0), Control::MonitorPage(MonitorPage::Aux), HitKind::Tap);
        button(ui, "Switch-Tight-Kerning", "CTRL\nROOM", !aux, Rect::new(48.0, st.show_buttons, 40.0, 40.0), Control::MonitorPage(MonitorPage::ControlRoom), HitKind::Tap);
        let cue_open = matches!(&view.popover, Some(p) if p.kind == Popover::CueOutputs);
        button(ui, "Switch-Popover-Left", "CUE\nOUTPUTS", cue_open, Rect::new(8.0, st.cue_outputs, 80.0, 40.0), Control::CueOutputs, HitKind::Tap);
        if let Some(y) = st.controller {
            ui.toggle("Switch-Large-Tight-Kerning", Some("MONITOR\nCONTROLLER"), false, true, Rect::new(8.0, y, 80.0, 40.0), None, None);
        }
        if let Some(y) = st.correction_label {
            separator(ui, y);
            ui.font_text("CR-Label", "MONITOR\nCORRECTION", Rect::new(0.0, y, STRIP_W, 28.0), Align::Center);
        }
        if let (Some(y), Some(power)) = (st.headphones, opts.hp_correction) {
            correction_button(ui, true, power, "HEADPHONES", y);
        }
        if let (Some(y), Some(power)) = (st.main_mon, opts.speaker_correction) {
            correction_button(ui, false, power, "MAIN MON", y);
        }
        separator(ui, st.output_label);
        ui.font_text("CR-Label", "OUTPUT", Rect::new(4.0, st.output_label, 88.0, 16.0), Align::Center);
        if let Some(y) = st.monitor_button {
            button(ui, "Switch-Large", "MONITOR", opts.alt_selection == 0, Rect::new(8.0, y, 80.0, 40.0), Control::MonitorAlt(0), HitKind::Tap);
        }
        if let Some(y) = st.alt {
            if opts.alt_count > 1 {
                button(ui, "Switch", "ALT\n1", opts.alt_selection == 1, Rect::new(8.0, y, 40.0, 40.0), Control::MonitorAlt(1), HitKind::Tap);
                button(ui, "Switch", "ALT\n2", opts.alt_selection == 2, Rect::new(48.0, y, 40.0, 40.0), Control::MonitorAlt(2), HitKind::Tap);
            } else {
                button(ui, "Switch-Large", "ALT 1", opts.alt_selection == 1, Rect::new(8.0, y, 80.0, 40.0), Control::MonitorAlt(1), HitKind::Tap);
            }
        }
        ui.toggle("Switch-Tight-Kerning", Some("MONO"), m.mono, true, Rect::new(8.0, st.mono_mute, 40.0, 40.0), Some(mid(Control::MonitorMono)), None);
        ui.toggle("Switch-Red-Tight-Kerning", Some("MUTE"), m.mute, true, Rect::new(48.0, st.mono_mute, 40.0, 40.0), Some(mid(Control::MonitorMute)), None);

        gain_and_session(ui, &m, &opts, st.gain);
    });
}

/// Section rule above MONITOR CORRECTION and OUTPUT (Console 1.3.1): a 1-pt line of the
/// section colour across the silk-screen, 6 pt above the label, inset 3 pt from the panel.
fn separator(ui: &mut Ui, label_y: f32) {
    ui.fill(Rect::new(7.0, label_y - 6.0, STRIP_W - 14.0, 1.0), SECTION_BG);
}

/// `Rack_MonitorMeters` stereo layout (0x1809d9980) in a `bottom`-tall box at the column top.
fn monitor_meters(ui: &mut Ui, meters: MeterFrame, bottom: f32) {
    let meter_w = ui.skin("meters", "Monitor-Meter").and_then(|s| s.size()).map(|s| s.0).unwrap_or(17.0);
    let logo_h = ui.skin("backgrounds", "UA-Logo").and_then(|s| s.size()).map(|s| s.1).unwrap_or(38.0);
    let g = MonitorMetersGeometry::new(bottom, meter_w, logo_h);
    // Display-Nav is a `stretch` panel whose caps are half the image: only the centre row and
    // column of the art stretch (its top shading and edges stay 1:1).
    if let Some(path) = ui.skin("backgrounds", "Display-Nav").and_then(|s| s.image("image")) {
        let path = path.to_string();
        if let Some(img) = ui.assets.images.get(&path) {
            let (l, t) = (((img.width - 1) / 2) as f32, ((img.height - 1) / 2) as f32);
            ui.image(&path, g.background, Some(Insets { l, t, r: l, b: t }), None, 1.0);
        }
    }
    if let Some(path) = ui.skin("backgrounds", "UA-Logo").and_then(|s| s.image("image")) {
        let path = path.to_string();
        if let Some((w, h)) = ui.image_size(&path) {
            ui.image(&path, Rect::new(((g.logo.w - w) / 2.0).floor(), g.logo.y, w, h), None, None, 1.0);
        }
    }
    ui.bar_meter("Monitor-Meter", meters.left, meters.clip_left, g.left);
    ui.bar_meter("Monitor-Meter", meters.right, meters.clip_right, g.right);

    // Rack_MeterMarks: style 1 left of L (3-pt major / 1-pt minor ticks on its right edge),
    // style 3 between the bars (numbers, 2-pt centred minor ticks), style 2 right of R.
    let skin = ui.skin("meters", "Monitor-Meter");
    let tick = skin.and_then(|s| s.color("color-text")).unwrap_or(Rgba::rgb8(93, 101, 105));
    let mut numbers = ui.style("meters", "Monitor-Meter");
    numbers.color = tick;
    let rows = g.mark_rows();
    let (lx, rx) = (g.left.x, g.right.max_x());
    let (cx, cw) = (g.left.max_x(), g.right.x - g.left.max_x());
    let cmid = cx + (cw / 2.0).round();
    for (i, &y) in rows.iter().enumerate() {
        ui.fill(Rect::new(lx - 3.0, y, 3.0, 1.0), tick);
        ui.fill(Rect::new(rx, y, 3.0, 1.0), tick);
        if i > 0 {
            let ym = ((rows[i - 1] + y) / 2.0 + 0.5).floor();
            ui.fill(Rect::new(lx - 1.0, ym, 1.0, 1.0), tick);
            ui.fill(Rect::new(rx, ym, 1.0, 1.0), tick);
            ui.fill(Rect::new(cmid - 1.0, ym, 2.0, 1.0), tick);
        }
        let db = -scales::METER_MARKS[i] as i32;
        ui.text(&db.to_string(), numbers.clone(), Rect::new(cx, y - 4.0, cw, 9.0), Align::Center);
    }
    for (label, meter) in [("L", g.left), ("R", g.right)] {
        ui.font_text("Silk-Screen-Surround-Mode", label, Rect::new(meter.x, meter.max_y() + 5.0, meter.w, 16.0), Align::Center);
    }
}

/// `Rack_RoomCorrectionButton` (0, y, 96, 50): an insert slot (`inserts:Medium-Normal`, 10 pt
/// margins) whose plug-in image is the correction EQ curve, fitted centred in
/// (14, 0, 68, h − 15), with the output name under it.
fn correction_button(ui: &mut Ui, headphones: bool, power: bool, label: &str, y: f32) {
    let r = Rect::new(0.0, y, STRIP_W, 50.0);
    if let Some(skin) = ui.skin("inserts", "Medium-Normal") {
        let slot = if power { skin.image("image-active") } else { skin.image("image-power-off") };
        if let Some(slot) = slot {
            let slot = slot.to_string();
            ui.image_stretched(&slot, r.inset(10.0, 0.0, 10.0, 0.0), 1.0);
        }
    }
    let key = if headphones { "correction_hp" } else { "correction" };
    if let Some(path) = ui.assets.appearance.image_paths.get(key).cloned() {
        if let Some((iw, ih)) = ui.image_size(&path) {
            let area = Rect::new(r.x + 14.0, r.y, r.w - 28.0, r.h - 15.0);
            let s = (area.w / iw).min(area.h / ih);
            ui.image(&path, area.centered(iw * s, ih * s), None, None, if power { 1.0 } else { 0.5 });
        }
    }
    let mut style = ui.style("inserts", "Medium-Normal");
    if let Some(c) = ui.skin("inserts", "Medium-Normal").and_then(|s| s.color("color-text")) {
        style.color = c;
    }
    ui.text(label, style, Rect::new(r.x + 4.0, r.y + 30.0, r.w - 8.0, 17.0), Align::Center);
}

/// `gain_and_session` (0, G, 96, 96): `Monitor-Gain-Session` art, the `layouts/gain_and_session`
/// label / knob / value, and the `knob_focus` dot at (w − 16, G + 6, 6, 6).
fn gain_and_session(ui: &mut Ui, m: &crate::model::MonitorState, opts: &crate::model::MonitorOptions, g: f32) {
    // Natural-size art centred in the column, snapped to device pixels (88.5 pt → x 3.5).
    if let Some(path) = ui.skin("backgrounds", "Monitor-Gain-Session").and_then(|s| s.image("image")) {
        let path = path.to_string();
        if let Some((w, h)) = ui.image_size(&path) {
            let x = ((STRIP_W - w) / 2.0 * ART_SCALE).floor() / ART_SCALE;
            ui.image(&path, Rect::new(x, g, w, h), None, None, 1.0);
        }
    }
    let dim = opts.dim == Some(true);
    let knob = match (m.mute, dim) {
        (true, true) => "Monitor-Red-Dim",
        (true, false) => "Monitor-Red",
        (false, true) => "Monitor-Green-Dim",
        (false, false) => "Monitor-Green",
    };
    if let Some(layout) = ui.assets.layout("gain_and_session") {
        let frames = layout.frames("default", STRIP_W, 96.0);
        ui.at(0.0, g, |ui| {
            if let Some(&f) = frames.get("label") {
                ui.font_text("Silk-Screen", "MONITOR", f, Align::Center);
            }
            if let Some(&f) = frames.get("gain") {
                // Rack_Knob centres its film frame rounding half-pixels up: (−10, −1, 116, 100)
                // puts the 77 × 87 frame at (10, 6).
                let (kw, kh) = ui.skin("knobs", knob).and_then(|s| s.size()).unwrap_or((77.0, 87.0));
                let k = Rect::new(f.x + ((f.w - kw) / 2.0).ceil(), f.y + ((f.h - kh) / 2.0).ceil(), kw, kh);
                ui.knob(knob, m.level, k, Some(mid(Control::MonitorLevel)), None);
            }
            if let Some(&f) = frames.get("gain_value") {
                ui.font_text("Small-Label-Font", &m.readout, f, Align::Center);
            }
        });
    }
    let focus = if opts.knob_focus { "Knob-Focus-Green-Old" } else { "Knob-Focus-Off-Old" };
    ui.background(focus, Rect::new(STRIP_W - 16.0, g + 6.0, 6.0, 6.0));
}

/// CTRL ROOM page: cue outputs, headphone sources, talkback and dim, in the aux area `r`.
pub fn draw_control_room(ui: &mut Ui, model: &dyn MixerModel, r: Rect) {
    ui.fill(r, PANEL);
    let opts = model.monitor_options();
    let cues = model.cue_outputs();
    let hps = model.headphone_outputs();
    let name = Ui::plain_style("Helvetica Neue", 11.0, true, Rgba::rgb8(226, 226, 227));
    let small = Ui::plain_style("Helvetica", 9.0, false, Rgba::WHITE.with_alpha(0.5));
    ui.at(r.x, r.y, |ui| {
        let w = r.w;
        ui.cell_header("CONTROL ROOM", false, true, Rect::new(0.0, 5.0, w, 22.0));
        let mut y = 33.0;
        ui.font_text("CR-Label", "CUE OUTPUTS", Rect::new(0.0, y, w, 14.0), Align::Center);
        y += 16.0;
        if cues.is_empty() {
            ui.text("none", small.clone(), Rect::new(0.0, y, w, 20.0), Align::Center);
            y += 24.0;
        }
        for (i, c) in cues.iter().enumerate() {
            let k = i as u8;
            ui.background("Send-Route-Background", Rect::new(4.0, y, w - 8.0, 64.0));
            ui.fill(Rect::new(10.0, y + 22.0, w - 20.0, 2.0), crate::surfaces::cells::send_color(&c.name, 2 + i).with_alpha(0.85));
            ui.text(&c.name, name.clone(), Rect::new(10.0, y + 3.0, 70.0, 18.0), Align::Left);
            ui.text(&format!("→ {}", c.destination), small.clone(), Rect::new(80.0, y + 3.0, w - 90.0, 18.0), Align::Right);
            let cue_id = |on: bool| if on { None } else { Some(mid(Control::CueSource(k))) };
            ui.toggle("Switch-Small", Some("CUE"), c.source_cue, true, Rect::new(10.0, y + 32.0, 40.0, 19.0), cue_id(c.source_cue), None);
            ui.toggle("Switch-Small", Some("MON"), !c.source_cue, true, Rect::new(52.0, y + 32.0, 40.0, 19.0), cue_id(!c.source_cue), None);
            ui.toggle("Switch-Small", Some("MONO"), c.mono, true, Rect::new(w - 58.0, y + 32.0, 48.0, 19.0), Some(mid(Control::CueMono(k))), None);
            y += 70.0;
        }
        y += 6.0;
        ui.font_text("CR-Label", "HEADPHONES", Rect::new(0.0, y, w, 14.0), Align::Center);
        y += 16.0;
        if hps.is_empty() {
            ui.text("none", small.clone(), Rect::new(0.0, y, w, 20.0), Align::Center);
            y += 24.0;
        }
        for (i, h) in hps.iter().enumerate() {
            ui.text(&h.name, name.clone(), Rect::new(10.0, y, 50.0, 26.0), Align::Left);
            let combo = Rect::new(60.0, y + 1.0, w - 70.0, 24.0);
            combo_box(ui, h.options.get(h.source).map(String::as_str).unwrap_or("—"), combo);
            ui.hit(combo, mid(Control::HeadphoneMenu(i as u8)), HitKind::Tap);
            y += 30.0;
        }
        y += 6.0;
        if opts.talkback.is_some() || opts.dim.is_some() {
            ui.font_text("CR-Label", "TALKBACK / DIM", Rect::new(0.0, y, w, 14.0), Align::Center);
            y += 16.0;
            if let Some(t) = opts.talkback {
                ui.toggle("Switch-Large", Some("TALK"), t, true, Rect::new(8.0, y, 80.0, 40.0), Some(mid(Control::Talkback)), None);
            }
            if let Some(d) = opts.dim {
                ui.toggle("Switch", Some("DIM"), d, true, Rect::new(96.0, y, 40.0, 40.0), Some(mid(Control::MonitorDim)), None);
                ui.knob("Mini-Send-Level", opts.dim_level, Rect::new(138.0, y - 5.0, 41.0, 49.0), Some(mid(Control::DimLevel)), None);
                ui.text(&opts.dim_readout, small, Rect::new(130.0, y + 42.0, 56.0, 12.0), Align::Center);
            }
        }
    });
}

/// `Combo-Box`: stretched `display_sml` plate, value text and the dropdown arrow.
pub fn combo_box(ui: &mut Ui, text: &str, r: Rect) {
    let skin = ui.skin("views", "Combo-Box");
    match skin.and_then(|s| s.image("image")) {
        Some(img) => {
            let img = img.to_string();
            ui.image_stretched(&img, r, 1.0);
        }
        None => ui.fill(r, Rgba::rgb8(24, 25, 27)),
    }
    let mut style = ui.style("views", "Combo-Box");
    style.size = 10.0;
    ui.text(text, style, r.inset(8.0, 0.0, 18.0, 0.0), Align::Left);
    if let Some(arrow) = skin.and_then(|s| s.image("image-overlay")) {
        let arrow = arrow.to_string();
        ui.image(&arrow, Rect::new(r.max_x() - 14.0, r.y + (r.h - 8.0) / 2.0, 8.0, 8.0), None, None, 0.9);
    }
}

#[cfg(test)]
mod tests {
    use super::{MonitorMetersGeometry, MonitorStack};

    #[test]
    fn stack_matches_console_geometry() {
        // Section 27…1010 (h 983). p1 refpair (Console 1.3.1, Apollo x4): gain 910, MONO/MUTE 862,
        // OUTPUT 846, MAIN MON 784, HEADPHONES 730, correction label 700, controller 652, CUE 612,
        // AUX 572, SHOW 556, silk 552.
        let s = MonitorStack::new(983.0, 0, true, true, true);
        let abs = |y: f32| y + 27.0;
        assert_eq!(abs(s.gain), 910.0);
        assert_eq!(abs(s.mono_mute), 862.0);
        assert_eq!(abs(s.output_label), 846.0);
        assert_eq!(s.main_mon.map(abs), Some(784.0));
        assert_eq!(s.headphones.map(abs), Some(730.0));
        assert_eq!(s.correction_label.map(abs), Some(700.0));
        assert_eq!(s.controller.map(abs), Some(652.0));
        assert_eq!(abs(s.cue_outputs), 612.0);
        assert_eq!(abs(s.show_buttons), 572.0);
        assert_eq!(abs(s.show_label), 556.0);
        assert_eq!(abs(s.silk), 552.0);
        // ALT rows push the stack up by 80 (row G−88, MONITOR G−128).
        let alt = MonitorStack::new(983.0, 2, true, true, true);
        assert_eq!(alt.alt, Some(s.gain - 88.0));
        assert_eq!(alt.monitor_button, Some(s.gain - 128.0));
        assert_eq!(alt.show_label, s.show_label - 80.0);
        // No correction and no controller: OUTPUT label − 8 is the controller slot's floor.
        let bare = MonitorStack::new(983.0, 0, false, false, false);
        assert_eq!(bare.correction_label, None);
        assert_eq!(abs(bare.cue_outputs), 846.0 - 8.0 - 40.0);
    }

    #[test]
    fn meters_match_console_geometry() {
        // monitor_meters 27…548: panel 31…548, logo 34, bars 73…520 at x 23 / 56, scale rows
        // 80, 120, 160, 199 … 518 (p1 refpair).
        let g = MonitorMetersGeometry::new(521.0, 17.0, 38.0);
        let abs = |y: f32| y + 27.0;
        assert_eq!((abs(g.background.y), abs(g.background.max_y())), (31.0, 548.0));
        assert_eq!(abs(g.logo.y), 34.0);
        assert_eq!((g.left.x, g.right.x), (23.0, 56.0));
        assert_eq!((abs(g.left.y), abs(g.left.max_y())), (73.0, 520.0));
        let rows: Vec<f32> = g.mark_rows().into_iter().map(abs).collect();
        assert_eq!(rows, vec![80.0, 120.0, 160.0, 199.0, 239.0, 279.0, 319.0, 359.0, 399.0, 438.0, 478.0, 518.0]);
    }
}
