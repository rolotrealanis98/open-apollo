//! Bottom status bar (`Rack_InfoSection::CConfigStatus`, 40 pt): TEMPO, RATE, CLOCK,
//! DSP / PGM / MEM loads, SCENE. Field positions measured from console-main-5.png.

use houston::{Rect, Rgba};

use crate::draw::Align;
use crate::input::{Control, ControlId, HitKind};
use crate::model::MixerModel;
use crate::surfaces::popover::sample_rate_label;
use crate::ui::Ui;

pub const STATUS_ID: &str = "status";
const CHROME: Rgba = Rgba::rgb8(35, 37, 40);

fn sid(control: Control) -> ControlId {
    ControlId { strip: STATUS_ID.into(), control }
}

impl Ui<'_> {
    /// The small dropdown triangle in the bottom-right corner of a status field.
    fn corner_arrow(&mut self, x: f32, y: f32) {
        for i in 0..5 {
            let i = i as f32;
            self.fill(Rect::new(x + 4.0 - i, y + i, i + 1.0, 1.0), Rgba::WHITE.with_alpha(0.35));
        }
    }
}

pub fn draw_status_bar(ui: &mut Ui, model: &dyn MixerModel, r: Rect) {
    let st = model.status();
    ui.fill(r, CHROME);
    ui.fill(Rect::new(r.x, r.y, r.w, 1.0), Rgba::rgb8(20, 21, 23));
    ui.background("Info-Bridge-Panel", Rect::new(r.x + 4.0, r.y + 4.0, 697.0, r.h - 8.0));
    let key = Ui::plain_style("Helvetica", 10.0, false, Rgba::WHITE.with_alpha(0.35));
    let val = Ui::plain_style("Helvetica", 12.0, false, Rgba::WHITE.with_alpha(0.78));
    let dim = Ui::plain_style("Helvetica", 12.0, false, Rgba::WHITE.with_alpha(0.4));
    let (y, h) = (r.y, r.h);
    let sep = |ui: &mut Ui, x: f32| ui.fill(Rect::new(x, y + 8.0, 1.0, h - 16.0), Rgba::rgb8(70, 74, 77));
    let offline = st.sample_rate.is_none();

    // label x, value x, value text, arrow x, separator x, control
    let rate = sample_rate_label(st.sample_rate);
    let clock = st.clock_source.clone().unwrap_or_else(|| "OFFLINE".into());
    let scene = st.scene.clone().unwrap_or_else(|| "(NONE)".into());
    let tempo = format!("{:.2}", st.tempo);
    let fields: [(&str, f32, f32, &str, f32, f32, Control); 3] = [
        ("TEMPO", 15.0, 82.0, &tempo, 141.0, 151.0, Control::StatusTempo),
        ("RATE", 160.0, 219.0, &rate, 288.0, 297.0, Control::StatusRate),
        ("CLOCK", 307.0, 365.0, &clock, 432.0, 441.0, Control::StatusClock),
    ];
    for (k, kx, vx, v, ax, sx, c) in fields {
        let k_style = if offline && k != "TEMPO" { Ui::plain_style("Helvetica", 10.0, false, Rgba::WHITE.with_alpha(0.22)) } else { key.clone() };
        ui.text(k, k_style, Rect::new(kx, y, vx - kx, h), Align::Left);
        let style = if v == "OFFLINE" { dim.clone() } else { val.clone() };
        ui.text(v, style, Rect::new(vx, y, ax - vx - 4.0, h), Align::Left);
        ui.corner_arrow(ax, y + h - 14.0);
        sep(ui, sx);
        ui.hit(Rect::new(kx - 4.0, y + 4.0, sx - kx, h - 8.0), sid(c), HitKind::Tap);
    }
    // DSP / PGM / MEM.
    let tiny = Ui::plain_style("Helvetica", 6.5, false, Rgba::WHITE.with_alpha(0.4));
    for (i, (label, load)) in [("DSP", st.dsp_load), ("PGM", st.pgm_load), ("MEM", st.mem_load)].into_iter().enumerate() {
        let ly = y + 9.0 + i as f32 * 7.5;
        ui.text(label, tiny.clone(), Rect::new(453.0, ly - 1.0, 18.0, 8.0), Align::Left);
        let bar = Rect::new(471.0, ly + 1.0, 78.0, 5.0);
        ui.fill(bar, Rgba::rgb8(16, 17, 18));
        let c = if load > 0.85 { Rgba::rgb8(237, 77, 38) } else { Rgba::rgb8(116, 168, 92) };
        ui.fill(Rect::new(bar.x, bar.y, bar.w * load.clamp(0.0, 1.0), bar.h), c);
        ui.text(&format!("{:.0}%", load * 100.0), tiny.clone(), Rect::new(549.0, ly - 1.0, 22.0, 8.0), Align::Right);
    }
    sep(ui, 574.0);
    ui.text("SCENE", key, Rect::new(587.0, y, 44.0, h), Align::Left);
    ui.text(&scene, val, Rect::new(631.0, y, 52.0, h), Align::Left);
    ui.corner_arrow(687.0, y + h - 14.0);
    sep(ui, 697.0);
    ui.hit(Rect::new(583.0, y + 4.0, 112.0, h - 8.0), sid(Control::StatusScene), HitKind::Tap);
}
