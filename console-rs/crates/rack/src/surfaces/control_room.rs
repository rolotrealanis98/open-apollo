//! Control-room column (`Rack_ControlRoom`, 1.3.1): with SHOW CTRL ROOM, a 96-pt column between
//! the bus section and the monitor column. Its panel sits at the foot of the column: DIM AMOUNT
//! (knob, readout, DIM), SOURCE (MONITOR, CUE 1, CUE 2) and the CONTROL ROOM label
//! (`control_room_label` `(4, h − 51, 92, 47)`, `Rack_ControlRoom::resized` 0x1811e2860).
//! Positions measured on the user captures s45–s49 (1470 × 1105: panel x 1282…1374,
//! y 778…1061, separators at 908 and 1014).

use houston::{Rect, Rgba};

use crate::draw::Align;
use crate::input::{Control, ControlId, HitKind};
use crate::model::MixerModel;
use crate::surfaces::monitor::MONITOR_ID;
use crate::ui::Ui;

/// Panel height above the column's foot (778…1061 over a foot at 1065).
const PANEL_H: f32 = 283.0;
const PANEL: Rgba = Rgba::rgb8(49, 54, 56);
const PANEL_EDGE: Rgba = Rgba::rgb8(54, 58, 62);
const RULE: Rgba = Rgba::rgb8(71, 76, 80);

fn mid(control: Control) -> ControlId {
    ControlId { strip: MONITOR_ID.into(), control }
}

/// ALT TRIM section (`Rack_ControlRoom_AltTrimSection`, `alt_1_trim` / `alt_2_trim`): one
/// knob per ALT set (ALT COUNT), ±30 dB, 0 dB centre, with its dB readout. [I] Geometry is not
/// measured (no capture with ALT COUNT > 0): a panel in the column's style above the main one.
fn alt_trims(ui: &mut Ui, opts: &crate::model::MonitorOptions, panel: Rect) {
    let n = opts.alt_count.min(2) as usize;
    if n == 0 {
        return;
    }
    let h = 96.0;
    let sec = Rect::new(panel.x, panel.y - 4.0 - h, panel.w, h);
    ui.fill(sec, PANEL_EDGE);
    ui.fill(sec.inset(0.5, 0.5, 0.5, 0.5), PANEL);
    ui.font_text("CR-Label", "ALT TRIM", Rect::new(sec.x, sec.y + 6.0, sec.w, 14.0), Align::Center);
    let col = sec.w / n as f32;
    for k in 0..n {
        let x = sec.x + col * k as f32;
        let v = opts.alt_trim[k];
        ui.font_text("CR-Label", &format!("ALT {}", k + 1), Rect::new(x, sec.y + 22.0, col, 12.0), Align::Center);
        let knob = Rect::new(x + (col - 36.0) / 2.0, sec.y + 34.0, 36.0, 40.0);
        let id = mid(Control::AltTrim(k as u8));
        ui.knob("Main-Level-Small", v, knob, Some(id.clone()), None);
        let mut readout = ui.style("views", "Main-Readout");
        readout.color = Rgba::WHITE.with_alpha(0.75);
        ui.text(&format!("{:+.1} dB", v * 60.0 - 30.0), readout, Rect::new(x, sec.y + 76.0, col, 16.0), Align::Center);
        ui.hit(knob, id, HitKind::Knob { value: v, span: 120.0, reset: Some(0.5) });
    }
}

/// Draws the column into `r` (the full column, from the strip columns' top to the status bar).
pub fn draw_control_room(ui: &mut Ui, model: &dyn MixerModel, r: Rect) {
    let opts = model.monitor_options();
    let cues = model.cue_outputs();
    let b = r.h; // column-local foot
    // SOURCE offers a CUE button per active cue (CUE BUS COUNT 2…4); CUE 3 / CUE 4 take a
    // second row and the panel grows by it. [I] the 3/4-cue layout is not captured.
    let n_cues = cues.len().clamp(2, 4);
    let extra = if n_cues > 2 { 40.0 } else { 0.0 };
    ui.at(r.x, r.y, |ui| {
        let panel = Rect::new(4.0, b - 4.0 - PANEL_H - extra, r.w - 4.0, PANEL_H + extra);
        ui.fill(panel, PANEL_EDGE);
        ui.fill(panel.inset(0.5, 0.5, 0.5, 0.5), PANEL);
        let top = panel.y;
        ui.font_text("CR-Label", "DIM AMOUNT", Rect::new(panel.x, top + 6.0, panel.w, 14.0), Align::Center);
        let knob = Rect::new(30.0, top + 17.0, 40.0, 46.0);
        ui.knob("Main-Level-Small", opts.dim_level, knob, Some(mid(Control::DimLevel)), None);
        // `Main-Readout` (display_sml, the skin's 28-pt height) at 1291…1365 × 838…866 (s45).
        let ro = Rect::new(13.0, top + 60.0, 74.0, 28.0);
        ui.background("Main-Readout", ro);
        let mut readout = ui.style("views", "Main-Readout");
        readout.color = Rgba::WHITE.with_alpha(0.75);
        ui.text(&opts.dim_readout, readout, ro, Align::Center);
        let dim = opts.dim.unwrap_or(false);
        ui.toggle("Switch-Large", Some("DIM"), dim, opts.dim.is_some(), Rect::new(10.0, top + 88.0, 80.0, 40.0), Some(mid(Control::MonitorDim)), None);
        ui.fill(Rect::new(panel.x, top + 130.0, panel.w, 1.0), RULE);
        ui.font_text("CR-Label", "SOURCE", Rect::new(panel.x, top + 137.0, panel.w, 14.0), Align::Center);
        ui.toggle("Switch-Large", Some("MONITOR"), opts.cr_source == 0, true, Rect::new(10.0, top + 152.0, 80.0, 40.0), Some(mid(Control::CrSource(0))), None);
        for i in 0..n_cues as u8 {
            let label = format!("CUE\n{}", i + 1);
            let enabled = cues.len() > i as usize;
            let at = Rect::new(10.0 + 40.0 * (i % 2) as f32, top + 192.0 + 40.0 * (i / 2) as f32, 40.0, 40.0);
            ui.toggle("Switch", Some(&label), opts.cr_source == i + 1, enabled, at, Some(mid(Control::CrSource(i + 1))), None);
        }
        ui.fill(Rect::new(panel.x, top + 236.0 + extra, panel.w, 1.0), RULE);
        let mut name = ui.style("views", "Control-Room-Name");
        name.bold = true;
        ui.text("CONTROL\nROOM", name, Rect::new(4.0, b - 51.0, 92.0, 47.0), Align::Center);
        ui.hit(knob, mid(Control::DimLevel), HitKind::Knob { value: opts.dim_level, span: 120.0, reset: None });
        alt_trims(ui, &opts, panel);
    });
}
