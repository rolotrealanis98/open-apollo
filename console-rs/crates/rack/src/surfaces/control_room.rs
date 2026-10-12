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

/// Draws the column into `r` (the full column, from the strip columns' top to the status bar).
pub fn draw_control_room(ui: &mut Ui, model: &dyn MixerModel, r: Rect) {
    let opts = model.monitor_options();
    let cues = model.cue_outputs();
    let b = r.h; // column-local foot
    ui.at(r.x, r.y, |ui| {
        let panel = Rect::new(4.0, b - 4.0 - PANEL_H, r.w - 4.0, PANEL_H);
        ui.fill(panel, PANEL_EDGE);
        ui.fill(panel.inset(0.5, 0.5, 0.5, 0.5), PANEL);
        let top = panel.y;
        ui.font_text("CR-Label", "DIM AMOUNT", Rect::new(panel.x, top + 6.0, panel.w, 14.0), Align::Center);
        let knob = Rect::new(30.0, top + 15.0, 40.0, 46.0);
        ui.knob("Main-Level-Small", opts.dim_level, knob, Some(mid(Control::DimLevel)), None);
        ui.background("Main-Readout", Rect::new(15.0, top + 63.0, 70.0, 22.0));
        let mut readout = ui.style("views", "Main-Readout");
        readout.color = Rgba::WHITE.with_alpha(0.75);
        ui.text(&opts.dim_readout, readout, Rect::new(15.0, top + 63.0, 70.0, 22.0), Align::Center);
        let dim = opts.dim.unwrap_or(false);
        ui.toggle("Switch-Large", Some("DIM"), dim, opts.dim.is_some(), Rect::new(10.0, top + 87.5, 80.0, 40.0), Some(mid(Control::MonitorDim)), None);
        ui.fill(Rect::new(panel.x, top + 130.0, panel.w, 1.0), RULE);
        ui.font_text("CR-Label", "SOURCE", Rect::new(panel.x, top + 137.0, panel.w, 14.0), Align::Center);
        ui.toggle("Switch-Large", Some("MONITOR"), opts.cr_source == 0, true, Rect::new(10.0, top + 151.5, 80.0, 40.0), Some(mid(Control::CrSource(0))), None);
        for i in 0..2u8 {
            let label = format!("CUE\n{}", i + 1);
            let enabled = cues.len() > i as usize;
            ui.toggle("Switch", Some(&label), opts.cr_source == i + 1, enabled, Rect::new(10.0 + 40.0 * i as f32, top + 191.5, 40.0, 40.0), Some(mid(Control::CrSource(i + 1))), None);
        }
        ui.fill(Rect::new(panel.x, top + 236.0, panel.w, 1.0), RULE);
        let mut name = ui.style("views", "Control-Room-Name");
        name.bold = true;
        ui.text("CONTROL\nROOM", name, Rect::new(4.0, b - 51.0, 92.0, 47.0), Align::Center);
        ui.hit(knob, mid(Control::DimLevel), HitKind::Drag { value: opts.dim_level, span: 120.0, reset: None });
    });
}
