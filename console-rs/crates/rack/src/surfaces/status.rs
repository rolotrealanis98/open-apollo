//! Bottom status bar (`Rack_InfoSection::CConfigStatus`, 40 pt): TEMPO, RATE, CLOCK,
//! DSP / PGM / MEM loads, SCENE. `Info-Bridge-Background` across the window, the
//! `Info-Bridge-Panel` well 4 pt in, `Info-Bridge-Separator`s and `Info-Bridge-Popup` corner
//! arrows; positions measured on Console 1.3.1 (refpair p1, 2×).

use houston::{Rect, Rgba};

use crate::draw::Align;
use crate::input::{Control, ControlId, HitKind};
use crate::model::MixerModel;
use crate::surfaces::popover::sample_rate_label;
use crate::ui::Ui;

pub const STATUS_ID: &str = "status";
/// DSP / PGM / MEM bar well and fill (measured).
const LOAD_WELL: Rgba = Rgba::rgb8(13, 13, 13);
const LOAD_FILL: Rgba = Rgba::rgb8(46, 97, 142);
const LOAD_HOT: Rgba = Rgba::rgb8(237, 77, 38);

fn sid(control: Control) -> ControlId {
    ControlId { strip: STATUS_ID.into(), control }
}

/// One field: label x, value x, popup-arrow x, separator x (window points, relative to the bar).
struct Field {
    label: &'static str,
    label_x: f32,
    value_x: f32,
    arrow_x: f32,
    sep_x: f32,
    control: Control,
}

pub fn draw_status_bar(ui: &mut Ui, model: &dyn MixerModel, r: Rect) {
    let st = model.status();
    // The labels render 10 % above their skin size here (as `font-size-dyn` fonts do).
    let mut key = ui.style("fonts", "Info-Bridge-Label");
    key.size *= 1.1;
    let val = ui.style("fonts", "Info-Bridge-Value");
    let mut gas = ui.style("fonts", "Info-Bridge-Gas-Label");
    gas.size *= 1.1;
    let offline = st.sample_rate.is_none();
    ui.at(r.x, r.y, |ui| {
        ui.background("Info-Bridge-Background", Rect::new(0.0, 0.0, r.w, r.h));
        ui.background("Info-Bridge-Panel", Rect::new(4.0, 4.0, r.w - 8.0, r.h - 8.0));
        let sep = |ui: &mut Ui, x: f32| ui.background("Info-Bridge-Separator", Rect::new(x - 1.0, 0.0, 3.0, r.h));
        let arrow = |ui: &mut Ui, x: f32| ui.background("Info-Bridge-Popup", Rect::new(x, 22.0, 8.0, 7.0));
        let (ty, th) = (4.0, r.h - 8.0);

        let tempo = format!("{:.2}", st.tempo);
        let rate = sample_rate_label(st.sample_rate);
        let clock = st.clock_source.clone().unwrap_or_else(|| "OFFLINE".into()).to_uppercase();
        let fields = [
            (Field { label: "TEMPO", label_x: 15.0, value_x: 80.5, arrow_x: 137.0, sep_x: 150.0, control: Control::StatusTempo }, tempo),
            (Field { label: "RATE", label_x: 161.0, value_x: 226.0, arrow_x: 283.0, sep_x: 296.0, control: Control::StatusRate }, rate),
            (Field { label: "CLOCK", label_x: 307.5, value_x: 362.0, arrow_x: 429.0, sep_x: 442.0, control: Control::StatusClock }, clock),
        ];
        for (f, v) in fields {
            let mut k = key.clone();
            if offline && f.label != "TEMPO" {
                k.color = k.color.with_alpha(k.color.a * 0.6);
            }
            ui.text(f.label, k, Rect::new(f.label_x, ty - 0.75, f.value_x - f.label_x, th), Align::Left);
            let mut v_style = val.clone();
            if v == "OFFLINE" {
                v_style.color = v_style.color.with_alpha(0.4);
            }
            ui.text(&v, v_style, Rect::new(f.value_x, ty, f.arrow_x - f.value_x, th), Align::Left);
            arrow(ui, f.arrow_x);
            sep(ui, f.sep_x);
            ui.hit(Rect::new(f.label_x - 8.0, 4.0, f.sep_x - f.label_x + 7.0, r.h - 8.0), sid(f.control), HitKind::Tap);
        }

        // DSP / PGM / MEM: label right-aligned to the 81 pt bar, value after it, 7 pt pitch.
        for (i, (label, load)) in [("DSP", st.dsp_load), ("PGM", st.pgm_load), ("MEM", st.mem_load)].into_iter().enumerate() {
            let bar = Rect::new(470.0, 10.0 + i as f32 * 7.0, 81.0, 6.0);
            ui.text(label, gas.clone(), Rect::new(bar.x - 30.0, bar.y + 0.5, 30.0, bar.h), Align::Right);
            ui.fill(bar, LOAD_WELL);
            let c = if load > 0.85 { LOAD_HOT } else { LOAD_FILL };
            ui.fill(Rect::new(bar.x, bar.y, (bar.w * load.clamp(0.0, 1.0) * 2.0).round() / 2.0, bar.h), c);
            ui.text(&format!("{:.0}%", load * 100.0), gas.clone(), Rect::new(bar.max_x() + 1.0, bar.y + 0.5, 24.0, bar.h), Align::Left);
        }
        sep(ui, 576.0);

        let scene = st.scene.clone().unwrap_or_else(|| "(NONE)".into());
        ui.text("SCENE", key.clone(), Rect::new(587.5, ty - 0.75, 44.0, th), Align::Left);
        ui.text(&scene, val.clone(), Rect::new(631.5, ty, 52.0, th), Align::Left);
        arrow(ui, 684.0);
        sep(ui, 697.0);
        ui.hit(Rect::new(580.0, 4.0, 116.0, r.h - 8.0), sid(Control::StatusScene), HitKind::Tap);
    });
}
