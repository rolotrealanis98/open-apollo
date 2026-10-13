//! Session navigator strip (above the strips, y 27…72): a `Monitor-Nav` panel holding a
//! `Display-Nav` well, the viewport frame of the visible strips and one `Main-Meter-Bridge`
//! mini meter per input strip (stereo strips: left/right halves), centred in equal slots.
//! Tapping a slot scrolls the strips so it is centred. Positions measured on Console 1.3.1
//! (refpair p1, 2×): panel ends at 72, well (0, 31 − top, w − 4, 37), frame (9, 36) to
//! (w − 9, 63), meters 12 × 21 at y 38.

use houston::{Rect, Rgba};

use crate::input::{Control, ControlId, HitKind};
use crate::model::MixerModel;
use crate::ui::Ui;

pub const SESSION_ID: &str = "session";
/// Viewport frame outline and fill (measured).
const FRAME_LINE: Rgba = Rgba::rgb8(98, 98, 98);
const FRAME_FILL: Rgba = Rgba::rgb8(53, 53, 53);

/// `r`: the panel (x, top, width, height to the strips); `first` / `visible`: the strip range
/// shown in the mixer.
pub fn draw_session_navigator(ui: &mut Ui, model: &dyn MixerModel, show_offline: bool, r: Rect, first: usize, visible: usize) {
    // The panel's top edge sits under the title bar; only its body and bottom edge show.
    ui.background("Monitor-Nav", Rect::new(r.x, r.y - 6.0, r.w, r.h + 6.0));
    let well_y = 31.0;
    crate::surfaces::navigator::background_max_corners(ui, "Display-Nav", Rect::new(r.x, well_y, r.w - 4.0, 37.0));
    let inner = Rect::new(r.x + 9.0, well_y + 5.0, r.w - 18.0 - 0.0, 27.0);
    let inputs = model.inputs_for_view(show_offline);
    let n = inputs.len();
    if n == 0 {
        return;
    }
    let slot = inner.w / n as f32;
    // Viewport frame (0.5 pt outline) over the visible strips.
    let vis = visible.min(n);
    let f = Rect::new(inner.x + first as f32 * slot, inner.y, vis as f32 * slot, inner.h);
    ui.fill(f, FRAME_LINE);
    ui.fill(f.inset(0.5, 0.5, 0.5, 0.5), FRAME_FILL);
    for (i, s) in inputs.iter().enumerate() {
        let b = Rect::new(inner.x + i as f32 * slot, inner.y, slot, inner.h);
        let m = model.meters(s);
        let x = (b.x + (slot - 12.0) / 2.0).floor();
        let mr = Rect::new(x, inner.y + 2.0, 12.0, 21.0);
        if s.stereo {
            ui.bar_meter("Left-Main-Meter-Bridge", m.left, m.clip_left, Rect::new(mr.x, mr.y, 6.0, mr.h));
            ui.bar_meter("Right-Main-Meter-Bridge", m.right, m.clip_right, Rect::new(mr.x + 6.0, mr.y, 6.0, mr.h));
        } else {
            ui.bar_meter("Main-Meter-Bridge", m.left, m.clip_left, mr);
        }
        let target = i.saturating_sub(visible / 2).min(n.saturating_sub(visible));
        ui.hit(b, ControlId { strip: SESSION_ID.into(), control: Control::SessionNavigate(target as u16) }, HitKind::Tap);
    }
}
