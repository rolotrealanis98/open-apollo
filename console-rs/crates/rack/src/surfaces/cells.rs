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
/// `Rack_UADRecord` row under the input inserts list (`(0, listH + 30, w, 42)`).
pub const REC_ROW_H: f32 = 42.0;
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

/// `Rack_Inserts::RequiredHeight` (0x18143b330) without a header row: n·pitch + 11.
pub fn inserts_list_h(slots: usize, size: InsertSize) -> f32 {
    slots as f32 * size.pitch() + 11.0
}

/// Inserts cell: header 22 + list + 55 with the UAD REC row (list `(0, 26, w, h−77)`), + 9
/// without (`(0, 26, w, h−31)`) (0x1814ba3a0, `Rack_ChannelStripCell_Inserts::resized` 0x1814bcb00).
pub fn inserts_cell_h(slots: usize, size: InsertSize, rec_row: bool) -> f32 {
    HEADER_H + inserts_list_h(slots, size) + if rec_row { 55.0 } else { 9.0 }
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

/// Insert list (`Rack_Inserts`) at `y`, sized for `list_slots`: `Channel-Inserts-Background`
/// at `(4, 0, w−8, h)` and slot k at `(0, 5 + k·pitch + 1, w, pitch − 2)` (resized 0x181440960).
pub fn insert_list(ui: &mut Ui, s: &StripInfo, slots: &[InsertState], list_slots: usize, size: InsertSize, y: f32) {
    let pitch = size.pitch();
    ui.background("Channel-Inserts-Background", Rect::new(4.0, y, STRIP_W - 8.0, inserts_list_h(list_slots, size)));
    for (k, slot) in slots.iter().take(list_slots).enumerate() {
        let r = Rect::new(0.0, y + 6.0 + k as f32 * pitch, STRIP_W, pitch - 2.0);
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
/// compact overview rows over a dark well (black 30 %, `(4, y, w−8, well_h)`, measured) that runs
/// to the bottom of the column. `pre(k)` is the PRE state of send k, None hides PRE.
pub fn send_list(ui: &mut Ui, s: &StripInfo, sends: &[SendState], pre: &dyn Fn(usize) -> Option<bool>, view: &ViewState, y: f32, well_h: f32) {
    if sends.is_empty() {
        let style = ui.style("fonts", "Channel-Strip-Section-Header");
        ui.text("NO SENDS", TextStyle { color: style.color.with_alpha(0.35), ..style }, Rect::new(0.0, y + 4.0, STRIP_W, 40.0), Align::Center);
        return;
    }
    ui.fill(Rect::new(4.0, y, STRIP_W - 8.0, well_h), SENDS_WELL);
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

const SENDS_WELL: Rgba = Rgba { r: 0.0, g: 0.0, b: 0.0, a: 0.302 };
/// Route colour bar of a send with no assigned colour (cue sends in p1), measured.
const ROUTE_BAR_NONE: Rgba = Rgba::rgb8(74, 78, 80);

/// Colour bar under the route name: AUX sends use the aux colour, cues show the neutral bar.
fn route_bar(name: &str) -> Rgba {
    match name {
        "AUX 1" => Rgba::rgb8(186, 129, 43),
        "AUX 2" => Rgba::rgb8(46, 107, 191),
        _ => ROUTE_BAR_NONE,
    }
}

/// `Rack_MiniSendSection` at medium size (init 0x1814523f0, resized 0x181455290; route
/// `Rack_SendRoute::resized` 0x181492e10):
/// * route `(0, 0, w, 35)`: `Send-Route-Background` `(4, 0, w−8, 35)`, the `Flex-Route-Focus`
///   plate (`slot_bg`, inset-background 10,6,10,6) with the route name (`Flex-Route-Value-Font`)
///   and the `Slot-Color` bar at its foot;
/// * lower plate `(6, 34, w−12, h−34−2)` (`Secondary-Send-Background`);
/// * knob meter + knob `(7, 35−4, 41, 49)`;
/// * pre_and_mute `(46, 35+14, L, 19)`: L = mute + 10 with `Send-Mute` centred, or PRE + M
///   (`Mini-Send-Pre`, `Mini-Send-Mute`) 2 apart.
fn mini_send(ui: &mut Ui, s: &StripInfo, k: usize, send: &SendState, pre: Option<bool>, sy: f32) {
    let top = 35.0;
    ui.background("Send-Route-Background", Rect::new(4.0, sy, STRIP_W - 8.0, top));
    let plate = Rect::new(10.0, sy + 6.0, STRIP_W - 20.0, top - 12.0);
    if let Some(img) = ui.skin("views", "Flex-Route-Focus").and_then(|k| k.image("image")) {
        let img = img.to_string();
        ui.image_stretched(&img, plate, 1.0);
    }
    ui.fill(Rect::new(plate.x + 0.5, plate.max_y() - 3.5, plate.w - 1.0, 3.0), route_bar(&send.name));
    let mut label = ui.style("fonts", "Flex-Route-Value-Font");
    if let Some(c) = ui.skin("fonts", "Flex-Route-Value-Font").and_then(|k| k.color("color-text-off")) {
        label.color = c;
    }
    ui.text(&send.name, label, Rect::new(plate.x, plate.y, plate.w, plate.h - 4.0), Align::Center);
    ui.background("Secondary-Send-Background", Rect::new(6.0, sy + top - 1.0, STRIP_W - 12.0, SEND_H - top - 1.0));
    let knob = Rect::new(7.0, sy + top - 4.0, 41.0, 49.0);
    ui.film("meters", "Knob-Meter", "image-film", 0.0, knob);
    ui.knob("Mini-Send-Level", send.level, knob, cid(s, Control::SendLevel(k as u8)), Some(crate::scales::UNITY));
    let y = sy + top + 14.0;
    match pre {
        Some(on) => {
            ui.toggle("Mini-Send-Pre", Some("P"), on, true, Rect::new(46.0, y, 18.0, 19.0), cid(s, Control::SendPre(k as u8)), None);
            ui.toggle("Mini-Send-Mute", Some("M"), send.muted, true, Rect::new(66.0, y, 18.0, 19.0), cid(s, Control::SendMute(k as u8)), None);
        }
        None => ui.toggle("Send-Mute", Some("M"), send.muted, true, Rect::new(51.0, y, 32.0, 19.0), cid(s, Control::SendMute(k as u8)), None),
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
        // p1: input inserts 314…602, aux inserts 77…319.
        assert_eq!(inserts_cell_h(4, InsertSize::Large, true), 288.0);
        assert_eq!(inserts_cell_h(4, InsertSize::Large, false), 242.0);
        assert_eq!(inserts_cell_h(4, InsertSize::Small, true), 22.0 + 92.0 + 11.0 + 55.0);
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
