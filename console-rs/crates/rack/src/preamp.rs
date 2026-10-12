//! The preamp block: `layouts/preamp_channel_strip_controls*.json` positioned by houston,
//! with component → skin bindings recovered from Rack.dll (0x181478600, 0x1814796e0,
//! 0x18147d100, 0x18147f5a0).

use std::sync::Arc;

use houston::{Rect, Rgba};

use crate::draw::Align;
use crate::input::{Control, ControlId};
use crate::model::PreampState;
use crate::ui::Ui;

/// LINE amber, MIC green (Rack_PreampChannelStripTextButton legend colours).
fn source_color(s: &str) -> Rgba {
    match s {
        "LINE" => Rgba::rgb8(255, 197, 74),
        "MIC" => Rgba::rgb8(142, 214, 0),
        _ => Rgba::WHITE,
    }
}

pub fn draw_preamp(ui: &mut Ui, layout_name: &str, state: &PreampState, strip: &Arc<str>, origin: Rect) {
    let Some(layout) = ui.assets.layout(layout_name) else { return };
    let frames = layout.frames("default", origin.w, origin.h);
    let id = |c: &str| Some(ControlId { strip: strip.clone(), control: Control::Preamp(c.into()) });
    ui.at(origin.x, origin.y, |ui| {
        for child in &layout.children {
            let Some(&r) = frames.get(&child.id) else { continue };
            match child.id.as_str() {
                "lower_state_plate" => ui.background("Send-Lower-State-Plate", r),
                "preamp_input_meter" => ui.film("meters", "Preamp-Input-Meter", "image-film", state.input_led, r),
                "preamp_gain" => ui.knob("Small-Green-Input", state.gain, r, id("preamp_gain"), None),
                "input_source_backround" | "preamp_db_gain_backround" => ui.background("Display-Background", r),
                "input_source" => {
                    let key = match state.source.as_str() {
                        "LINE" => "Input-Line-Big",
                        "HI-Z" => "Input-HiZ-Big",
                        _ => "Input-Mic-Big",
                    };
                    ui.toggle(key, Some(&state.source), false, true, r, None, Some(source_color(&state.source)));
                    // Tap opens the MIC / LINE / HI-Z picker (`ViewState`, Popover::InputSource).
                    if let Some(id) = id("input_source") {
                        ui.hit(r, id, crate::input::HitKind::Tap);
                    }
                }
                "input_source_button" => {
                    ui.toggle("Switch-Small", Some("IN"), false, true, r, None, None);
                    if let Some(id) = id("input_source") {
                        ui.hit(r, id, crate::input::HitKind::Tap);
                    }
                }
                "preamp_db_gain" => ui.font_text("Small-Label-Font", &state.gain_text, r, Align::Center),
                "48v" => {
                    let label = state.phantom_enabled.then_some("48V");
                    ui.toggle("Input-48v", label, state.phantom, state.phantom_enabled, r, id("48v"), None)
                }
                "pad" => {
                    let label = state.pad_enabled.then_some("PAD");
                    ui.toggle("Input-Pad", label, state.pad, state.pad_enabled, r, id("pad"), None)
                }
                "low_cut" => ui.toggle("Input-HPF", None, state.low_cut, true, r, id("low_cut"), None),
                "phase" => ui.toggle("Input-Phase", None, state.phase, true, r, id("phase"), None),
                "auto_gain" => ui.toggle("Toggle-Yellow-Button", Some("AUTO-GAIN"), false, true, r, None, None),
                // knob_focus, preamp_gain_background/bypass, secondary/tertiary plates, disabler:
                // not drawn in the default state.
                _ => {}
            }
        }
    });
}
