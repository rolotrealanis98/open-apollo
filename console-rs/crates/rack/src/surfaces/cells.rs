//! Cell rows of the strip column (`mixer-geometry.md` §1): heights per section state, the
//! inserts list in LARGE/SMALL size with FIXED SLOTS, and the Sends cell (mini sends, or the
//! compact SENDS OVERVIEW form).

use houston::{Rect, Rgba};

use crate::draw::{Align, TextStyle};
use crate::geometry::STRIP_W;
use crate::input::{Control, ControlId};
use crate::model::{InsertState, SendState, StripInfo};
use crate::surfaces::view::{InsertSize, ViewState};
use crate::ui::Ui;

pub const HEADER_H: f32 = 22.0;
/// Input cell with Unison + preamp (measured, §1.2).
pub const INPUT_CELL_H: f32 = 237.0;
/// UAD REC/MON + INS row under the input inserts list.
pub const REC_ROW_H: f32 = 41.0;
/// Send heights: medium (mini send) 75, compact (overview) 35; pitch +3 (0x1814c0400).
pub const SEND_H: f32 = 75.0;
pub const SEND_H_COMPACT: f32 = 35.0;
/// Sends cell height when there are no sends (0x1814c01f0).
pub const SENDS_DEFAULT_H: f32 = 87.0;

/// Send / cue swatch colours (0x181496ab0): AUX 1, AUX 2, CUE 1…4 [inf order].
pub const SEND_COLORS: [Rgba; 6] =
    [Rgba::rgb8(210, 134, 28), Rgba::rgb8(0, 127, 211), Rgba::rgb8(213, 45, 171), Rgba::rgb8(213, 95, 0), Rgba::rgb8(142, 214, 0), Rgba::rgb8(0, 209, 212)];

pub fn send_color(name: &str, k: usize) -> Rgba {
    let idx = if let Some(n) = name.strip_prefix("AUX ") {
        n.trim().parse::<usize>().ok().map(|n| n.saturating_sub(1).min(1))
    } else if let Some(n) = name.strip_prefix("CUE ") {
        n.trim().parse::<usize>().ok().map(|n| 2 + n.saturating_sub(1).min(3))
    } else {
        None
    };
    SEND_COLORS[idx.unwrap_or(k % SEND_COLORS.len())]
}

/// Slots shown: all with FIXED SLOTS, else up to the last loaded one plus one empty.
pub fn visible_slots(slots: &[InsertState], fixed: bool) -> usize {
    if fixed {
        return slots.len();
    }
    let last = slots.iter().rposition(|s| matches!(s, InsertState::Loaded { .. }));
    last.map(|i| i + 2).unwrap_or(1).min(slots.len())
}

/// Inserts cell: header + list (+ UAD REC row on inputs).
pub fn inserts_cell_h(slots: usize, size: InsertSize, rec_row: bool) -> f32 {
    26.0 + slots as f32 * size.pitch() + if rec_row { 4.0 + REC_ROW_H } else { 9.0 }
}

/// Sends cell: header + Σ(H + 3) + 5, or 87 without sends.
pub fn sends_cell_h(sends: usize, overview: bool) -> f32 {
    if sends == 0 {
        return SENDS_DEFAULT_H;
    }
    let h = if overview { SEND_H_COMPACT } else { SEND_H };
    26.0 + sends as f32 * (h + 3.0) + 5.0
}

fn cid(strip: &StripInfo, control: Control) -> Option<ControlId> {
    Some(ControlId { strip: strip.id.clone(), control })
}

/// Insert list at `y`: `Channel-Inserts-Background` plate and one slot per visible insert.
pub fn insert_list(ui: &mut Ui, s: &StripInfo, slots: &[InsertState], shown: usize, size: InsertSize, y: f32) {
    let pitch = size.pitch();
    ui.background("Channel-Inserts-Background", Rect::new(4.0, y, 88.0, shown as f32 * pitch + 4.0));
    for (k, slot) in slots.iter().take(shown).enumerate() {
        let r = Rect::new(0.0, y + k as f32 * pitch + 3.0, STRIP_W, pitch - 2.0);
        match size {
            InsertSize::Large => ui.insert_slot(slot, r, cid(s, Control::Insert(k as u8))),
            InsertSize::Small => small_slot(ui, slot, r, cid(s, Control::Insert(k as u8))),
        }
    }
}

/// SMALL insert slot: stretched slot art with the plug-in name (no icon).
fn small_slot(ui: &mut Ui, state: &InsertState, r: Rect, id: Option<ControlId>) {
    let skin = ui.skin("inserts", "Small-Normal");
    let img = skin.and_then(|k| match state {
        InsertState::Empty => k.image("image-empty"),
        InsertState::Loaded { power: true, .. } => k.image("image-active"),
        InsertState::Loaded { power: false, .. } => k.image("image-power-off"),
    });
    let plate = Rect::new(r.x + 10.0, r.y, r.w - 20.0, r.h);
    if let Some(img) = img {
        let img = img.to_string();
        ui.image_stretched(&img, plate, 1.0);
    } else {
        ui.fill(plate, Rgba::rgb8(30, 32, 34));
    }
    match state {
        InsertState::Empty => {
            let style = TextStyle { family: "Helvetica".into(), size: 12.0, bold: true, italic: false, color: Rgba::WHITE.with_alpha(0.3) };
            ui.text("+", style, plate, Align::Center);
        }
        InsertState::Loaded { name, power } => {
            let short = name.strip_prefix("UAD ").unwrap_or(name);
            let style = TextStyle { family: "Helvetica".into(), size: 8.5, bold: false, italic: true, color: Rgba::WHITE.with_alpha(if *power { 0.85 } else { 0.4 }) };
            let inner = plate.inset(3.0, 0.0, 3.0, 0.0);
            let fits = short.chars().count() <= 15;
            ui.clip(Some(inner));
            ui.text(short, style, if fits { inner } else { Rect::new(inner.x, inner.y, 200.0, inner.h) }, if fits { Align::Center } else { Align::Left });
            ui.clip(None);
        }
    }
    if let Some(id) = id {
        ui.hit(r, id, crate::input::HitKind::Tap);
    }
}

/// Sends cell body starting at `y` (below the header): mini sends (knob, PRE, M) or the
/// compact overview rows. `pre(k)` is the PRE state of send k, None hides PRE.
pub fn send_list(ui: &mut Ui, s: &StripInfo, sends: &[SendState], pre: &dyn Fn(usize) -> Option<bool>, view: &ViewState, y: f32) {
    if sends.is_empty() {
        let style = ui.style("fonts", "Channel-Strip-Section-Header");
        ui.text("NO SENDS", TextStyle { color: style.color.with_alpha(0.35), ..style }, Rect::new(0.0, y + 4.0, STRIP_W, 40.0), Align::Center);
        return;
    }
    if view.sends_overview {
        for (k, send) in sends.iter().enumerate() {
            overview_send(ui, s, k, send, y + k as f32 * (SEND_H_COMPACT + 3.0));
        }
        return;
    }
    for (k, send) in sends.iter().enumerate() {
        mini_send(ui, s, k, send, pre(k), y + k as f32 * (SEND_H + 3.0));
    }
}

/// `Rack_MiniSendSection` at medium size (§1.6): route header 35, plate, knob, PRE / M.
fn mini_send(ui: &mut Ui, s: &StripInfo, k: usize, send: &SendState, pre: Option<bool>, sy: f32) {
    let top = 35.0;
    ui.background("Send-Route-Background", Rect::new(4.0, sy, 88.0, top - 2.0));
    ui.fill(Rect::new(10.0, sy + top - 7.0, 76.0, 2.0), send_color(&send.name, k).with_alpha(0.85));
    ui.font_text("Send-Label", &send.name, Rect::new(4.0, sy, 88.0, top - 4.0), Align::Center);
    ui.background("Secondary-Send-Background", Rect::new(6.0, sy + top - 1.0, 84.0, SEND_H - top + 3.0));
    ui.knob("Mini-Send-Level", send.level, Rect::new(7.0, sy + top - 4.0, 41.0, 49.0), cid(s, Control::SendLevel(k as u8)), Some(crate::scales::UNITY));
    match pre {
        Some(on) => {
            ui.toggle("Mini-Send-Pre", Some("P"), on, true, Rect::new(48.0, sy + top + 12.0, 18.0, 18.0), cid(s, Control::SendPre(k as u8)), None);
            ui.toggle("Mini-Send-Mute", Some("M"), send.muted, true, Rect::new(69.0, sy + top + 12.0, 18.0, 18.0), cid(s, Control::SendMute(k as u8)), None);
        }
        None => ui.toggle("Mini-Send-Mute", Some("M"), send.muted, true, Rect::new(46.0, sy + top + 10.0, 32.0, 19.0), cid(s, Control::SendMute(k as u8)), None),
    }
}

/// SENDS OVERVIEW row (35 pt): name, level bar, M.
fn overview_send(ui: &mut Ui, s: &StripInfo, k: usize, send: &SendState, sy: f32) {
    ui.background("Send-Route-Background", Rect::new(4.0, sy, 88.0, SEND_H_COMPACT - 2.0));
    let label = ui.style("fonts", "Send-Label");
    ui.text(&send.name, label.clone(), Rect::new(8.0, sy + 2.0, 50.0, 14.0), Align::Left);
    let db = crate::scales::format_db(crate::scales::fader_db(send.level));
    ui.text(&db, label, Rect::new(40.0, sy + 2.0, 46.0, 14.0), Align::Right);
    let bar = Rect::new(9.0, sy + 20.0, 54.0, 6.0);
    ui.fill(bar, Rgba::rgb8(18, 19, 20));
    let c = send_color(&send.name, k);
    ui.fill(Rect::new(bar.x, bar.y, bar.w * send.level.clamp(0.0, 1.0) as f32, bar.h), if send.muted { c.with_alpha(0.3) } else { c });
    ui.hit(bar.inset(0.0, -6.0, 0.0, -6.0), ControlId { strip: s.id.clone(), control: Control::SendLevel(k as u8) }, crate::input::HitKind::Drag { value: send.level, span: 120.0, reset: Some(crate::scales::UNITY) });
    ui.toggle("Mini-Send-Mute", Some("M"), send.muted, true, Rect::new(68.0, sy + 13.0, 18.0, 18.0), cid(s, Control::SendMute(k as u8)), None);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_heights_match_geometry() {
        assert_eq!(inserts_cell_h(4, InsertSize::Large, true), 271.0);
        assert_eq!(inserts_cell_h(4, InsertSize::Large, false), 235.0);
        assert_eq!(inserts_cell_h(4, InsertSize::Small, true), 26.0 + 92.0 + 45.0);
        // Aux sends: 22 + 26 + 2 × 78 (+ 5 bottom margin).
        assert_eq!(sends_cell_h(2, false), 26.0 + 156.0 + 5.0);
        assert_eq!(sends_cell_h(0, false), SENDS_DEFAULT_H);
    }

    #[test]
    fn dynamic_slots() {
        let loaded = InsertState::Loaded { name: "UAD dbx 160".into(), power: true };
        let slots = vec![loaded.clone(), InsertState::Empty, InsertState::Empty, InsertState::Empty];
        assert_eq!(visible_slots(&slots, true), 4);
        assert_eq!(visible_slots(&slots, false), 2);
        assert_eq!(visible_slots(&vec![InsertState::Empty; 4], false), 1);
        assert_eq!(visible_slots(&vec![loaded; 4], false), 4);
    }
}
