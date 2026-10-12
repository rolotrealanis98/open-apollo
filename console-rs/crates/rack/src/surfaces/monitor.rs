//! Monitor column (`Rack_MonitorSection_UAD2`, `mixer-geometry.md` §3) built bottom-up, with
//! the optional DIM / TALK and MONITOR / ALT rows, the SHOW AUX / CTRL ROOM page switch and
//! the CUE OUTPUTS popover button; plus the control-room panel that replaces the aux returns
//! on the CTRL ROOM page (cue outputs, headphone sources, talkback and dim).

use houston::{Rect, Rgba};

use crate::draw::Align;
use crate::geometry::STRIP_W;
use crate::input::{Control, ControlId, HitKind};
use crate::model::MixerModel;
use crate::surfaces::view::{MonitorPage, Popover, ViewState};
use crate::ui::Ui;

pub const MONITOR_ID: &str = "monitor";
const PANEL: Rgba = Rgba::rgb8(72, 76, 78);

fn mid(control: Control) -> ControlId {
    ControlId { strip: MONITOR_ID.into(), control }
}

/// Toggle drawn by skin with a separate hit of `kind` (Tap for selections).
fn button(ui: &mut Ui, key: &str, label: &str, on: bool, r: Rect, control: Control, kind: HitKind) {
    ui.toggle(key, Some(label), on, true, r, None, None);
    ui.hit(r, mid(control), kind);
}

/// Bottom-up stack of the monitor column (`Rack_MonitorSection_UAD2::resized` 0x1811ef010),
/// column-relative y values.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MonitorStack {
    /// gain_and_session top (G = h − 4 − 96).
    pub gain: f32,
    pub mono_mute: f32,
    pub alt: Option<f32>,
    pub monitor_button: Option<f32>,
    pub dim_talk: Option<f32>,
    pub output_label: f32,
    pub main_mon: f32,
    pub headphones: f32,
    pub correction_label: f32,
    pub controller: f32,
    pub cue_outputs: f32,
    pub show_buttons: f32,
    pub show_label: f32,
    /// `controls_bg` (Silk-Screen) top; the meters end 16 pt above it.
    pub silk: f32,
}

impl MonitorStack {
    pub fn new(h: f32, alt_count: u8, dim_or_talk: bool) -> Self {
        let gain = h - 4.0 - 96.0;
        let mono_mute = gain - 48.0;
        let mut y = mono_mute;
        let (mut alt, mut monitor_button, mut dim_talk) = (None, None, None);
        if alt_count > 0 {
            y -= 40.0;
            alt = Some(y);
            y -= 40.0;
            monitor_button = Some(y);
        }
        if dim_or_talk {
            y -= 40.0;
            dim_talk = Some(y);
        }
        let output_label = y - 16.0;
        let main_mon = y - 78.0;
        let headphones = main_mon - 54.0;
        let correction_label = headphones - 30.0;
        let controller = correction_label - 48.0;
        let cue_outputs = controller - 40.0;
        let show_buttons = cue_outputs - 40.0;
        let show_label = show_buttons - 16.0;
        Self { gain, mono_mute, alt, monitor_button, dim_talk, output_label, main_mon, headphones, correction_label, controller, cue_outputs, show_buttons, show_label, silk: show_label - 4.0 }
    }
}

/// The monitor column in `r` (window points; x = monitor column, y = 31).
pub fn draw_monitor_column(ui: &mut Ui, model: &dyn MixerModel, view: &ViewState, r: Rect) {
    let m = model.monitor();
    let opts = model.monitor_options();
    let meters = model.monitor_meters();
    let st = MonitorStack::new(r.h, opts.alt_count, opts.dim.is_some() || opts.talkback.is_some());
    ui.at(r.x, r.y, |ui| {
        ui.fill(Rect::new(0.0, 0.0, STRIP_W, r.h), PANEL);
        let meter_h = (st.silk - 16.0 - 10.0).max(60.0);
        ui.bar_meter("Monitor-Meter-Left", meters.left, meters.clip_left, Rect::new(26.0, 10.0, 16.0, meter_h));
        ui.bar_meter("Monitor-Meter-Right", meters.right, meters.clip_right, Rect::new(59.0, 10.0, 16.0, meter_h));
        ui.background("Silk-Screen", Rect::new(4.0, st.silk, 88.0, st.gain - 4.0 - st.silk));

        ui.font_text("CR-Label", "SHOW", Rect::new(4.0, st.show_label, 88.0, 16.0), Align::Center);
        let aux = view.monitor_page == MonitorPage::Aux;
        button(ui, "Switch-Tight-Kerning", "AUX", aux, Rect::new(8.0, st.show_buttons, 40.0, 40.0), Control::MonitorPage(MonitorPage::Aux), HitKind::Tap);
        button(ui, "Switch-Tight-Kerning", "CTRL\nROOM", !aux, Rect::new(48.0, st.show_buttons, 40.0, 40.0), Control::MonitorPage(MonitorPage::ControlRoom), HitKind::Tap);
        let cue_open = matches!(&view.popover, Some(p) if p.kind == Popover::CueOutputs);
        button(ui, "Switch-Popover-Left", "CUE\nOUTPUTS", cue_open, Rect::new(8.0, st.cue_outputs, 80.0, 40.0), Control::CueOutputs, HitKind::Tap);
        ui.toggle("Switch-Large-Tight-Kerning", Some("MONITOR\nCONTROLLER"), false, true, Rect::new(8.0, st.controller, 80.0, 40.0), None, None);
        ui.font_text("CR-Label", "MONITOR\nCORRECTION", Rect::new(0.0, st.correction_label, 96.0, 28.0), Align::Center);
        for (t, yy) in [("HEADPHONES", st.headphones), ("MAIN MON", st.main_mon)] {
            ui.background("Secondary-Display-Background", Rect::new(6.0, yy, 84.0, 50.0));
            ui.background("Display-Background", Rect::new(10.0, yy + 4.0, 76.0, 28.0));
            ui.font_text("Channel-Strip-Section-Header", t, Rect::new(0.0, yy + 32.0, 96.0, 16.0), Align::Center);
        }
        ui.font_text("CR-Label", "OUTPUT", Rect::new(4.0, st.output_label, 88.0, 16.0), Align::Center);
        if let Some(y) = st.dim_talk {
            if let Some(d) = opts.dim {
                ui.toggle("Switch", Some("DIM"), d, true, Rect::new(8.0, y, 40.0, 40.0), Some(mid(Control::MonitorDim)), None);
            }
            if let Some(t) = opts.talkback {
                ui.toggle("Switch", Some("TALK"), t, true, Rect::new(48.0, y, 40.0, 40.0), Some(mid(Control::Talkback)), None);
            }
        }
        if let Some(y) = st.monitor_button {
            button(ui, "Switch-Large", "MONITOR", opts.alt_selection == 0, Rect::new(8.0, y, 80.0, 40.0), Control::MonitorAlt(0), HitKind::Tap);
        }
        if let Some(y) = st.alt {
            button(ui, "Switch", "ALT\n1", opts.alt_selection == 1, Rect::new(8.0, y, 40.0, 40.0), Control::MonitorAlt(1), HitKind::Tap);
            if opts.alt_count > 1 {
                button(ui, "Switch", "ALT\n2", opts.alt_selection == 2, Rect::new(48.0, y, 40.0, 40.0), Control::MonitorAlt(2), HitKind::Tap);
            }
        }
        ui.toggle("Switch-Tight-Kerning", Some("MONO"), m.mono, true, Rect::new(8.0, st.mono_mute, 40.0, 40.0), Some(mid(Control::MonitorMono)), None);
        ui.toggle("Switch-Red-Tight-Kerning", Some("MUTE"), m.mute, true, Rect::new(48.0, st.mono_mute, 40.0, 40.0), Some(mid(Control::MonitorMute)), None);

        // Gain and session block.
        ui.background("Monitor-Gain-Session", Rect::new(4.0, st.gain, 88.0, 96.0));
        if let Some(layout) = ui.assets.layout("gain_and_session") {
            let frames = layout.frames("default", 96.0, 96.0);
            let knob = if m.mute { "Monitor-Red" } else if opts.dim == Some(true) { "Monitor-Green-Dim" } else { "Monitor-Green" };
            ui.at(0.0, st.gain, |ui| {
                if let Some(&f) = frames.get("label") {
                    ui.font_text("Small-Label-Font", "MONITOR", f, Align::Center);
                }
                if let Some(&f) = frames.get("gain") {
                    ui.knob(knob, m.level, f, Some(mid(Control::MonitorLevel)), None);
                }
                if let Some(&f) = frames.get("gain_value") {
                    ui.font_text("Small-Label-Font", &m.readout, f, Align::Center);
                }
            });
        }
    });
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
    use super::MonitorStack;

    #[test]
    fn stack_matches_console_geometry() {
        // Column 31…1010 (h 979); §3: gain 910 abs, MONO/MUTE 862, OUTPUT 846, MAIN MON 784,
        // HEADPHONES 730, correction label 700, controller 652, CUE 612, AUX 572, SHOW 556.
        let s = MonitorStack::new(979.0, 0, false);
        let abs = |y: f32| y + 31.0;
        assert_eq!(abs(s.gain), 910.0);
        assert_eq!(abs(s.mono_mute), 862.0);
        assert_eq!(abs(s.output_label), 846.0);
        assert_eq!(abs(s.main_mon), 784.0);
        assert_eq!(abs(s.headphones), 730.0);
        assert_eq!(abs(s.correction_label), 700.0);
        assert_eq!(abs(s.controller), 652.0);
        assert_eq!(abs(s.cue_outputs), 612.0);
        assert_eq!(abs(s.show_buttons), 572.0);
        assert_eq!(abs(s.show_label), 556.0);
        // ALT rows push the stack up by 80 (row G−88, MONITOR G−128).
        let alt = MonitorStack::new(979.0, 2, false);
        assert_eq!(alt.alt, Some(s.gain - 88.0));
        assert_eq!(alt.monitor_button, Some(s.gain - 128.0));
        assert_eq!(alt.show_label, s.show_label - 80.0);
    }
}
