//! Session navigator strip (y 27…71 above the strips): one miniature block per input strip
//! with its fader position, mute/solo tint, and the viewport rectangle of the visible strips.
//! Tapping a block scrolls the strips so that block is centred.

use houston::{Rect, Rgba};

use crate::draw::Align;
use crate::input::{Control, ControlId, HitKind};
use crate::model::MixerModel;
use crate::ui::Ui;

pub const SESSION_ID: &str = "session";

/// `first` / `visible`: the strip range shown in the mixer.
pub fn draw_session_navigator(ui: &mut Ui, model: &dyn MixerModel, r: Rect, first: usize, visible: usize) {
    ui.fill(r, Rgba::rgb8(28, 30, 32));
    let frame = r.inset(6.0, 6.0, 6.0, 6.0);
    ui.fill(frame, Rgba::rgb8(58, 61, 64));
    ui.fill(frame.inset(1.0, 1.0, 1.0, 1.0), Rgba::rgb8(36, 38, 40));
    let inputs = model.inputs();
    let n = inputs.len();
    if n == 0 {
        return;
    }
    let inner = frame.inset(3.0, 3.0, 3.0, 3.0);
    let bw = inner.w / n as f32;
    let tiny = Ui::plain_style("Helvetica", 7.0, true, Rgba::WHITE.with_alpha(0.28));
    for (i, s) in inputs.iter().enumerate() {
        let st = model.main(s);
        let b = Rect::new(inner.x + i as f32 * bw, inner.y, bw, inner.h);
        if i > 0 {
            ui.fill(Rect::new(b.x, b.y + 2.0, 1.0, b.h - 4.0), Rgba::WHITE.with_alpha(0.05));
        }
        if st.mute {
            ui.fill(Rect::new(b.x + 2.0, b.max_y() - 4.0, b.w - 4.0, 2.0), Rgba::rgb8(200, 50, 30).with_alpha(0.55));
        }
        if st.solo {
            ui.fill(b.inset(1.0, 1.0, 1.0, 1.0), Rgba::rgb8(255, 197, 74).with_alpha(0.15));
        }
        if bw > 40.0 {
            ui.text(&s.title, tiny.clone(), Rect::new(b.x + 3.0, b.y, b.w * 0.4, 9.0), Align::Left);
        }
        // Miniature fader cap: vertical position follows the fader.
        let travel = b.h - 14.0;
        let cy = b.y + 2.0 + travel * (1.0 - st.fader.clamp(0.0, 1.0) as f32);
        ui.fill(Rect::new(b.x + b.w / 2.0 - 5.0, cy, 10.0, 10.0), Rgba::rgb8(18, 19, 20));
        ui.fill(Rect::new(b.x + b.w / 2.0 - 4.0, cy + 4.5, 8.0, 1.0), Rgba::WHITE.with_alpha(0.25));
        let target = i.saturating_sub(visible / 2).min(n.saturating_sub(visible));
        ui.hit(b, ControlId { strip: SESSION_ID.into(), control: Control::SessionNavigate(target as u16) }, HitKind::Tap);
    }
    // Viewport.
    let vis = visible.min(n);
    let v = Rect::new(inner.x + first as f32 * bw, inner.y - 2.0, vis as f32 * bw, inner.h + 4.0);
    let c = Rgba::rgb8(210, 212, 214).with_alpha(0.7);
    ui.fill(Rect::new(v.x, v.y, v.w, 1.5), c);
    ui.fill(Rect::new(v.x, v.max_y() - 1.5, v.w, 1.5), c);
    ui.fill(Rect::new(v.x, v.y, 1.5, v.h), c);
    ui.fill(Rect::new(v.max_x() - 1.5, v.y, 1.5, v.h), c);
}
