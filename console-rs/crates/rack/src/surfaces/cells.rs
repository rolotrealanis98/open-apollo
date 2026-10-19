//! Cell rows of the strip column (`mixer-geometry.md` §1): heights per section state, the
//! inserts list in LARGE/SMALL size with FIXED SLOTS, and the Sends cell (mini sends, or the
//! compact SENDS OVERVIEW form).

use houston::{Rect, Rgba};

use crate::draw::{Align, TextStyle};
use crate::geometry::STRIP_W;
use crate::input::{Control, ControlId};
use crate::model::{InsertLabel, InsertState, SendState, StripInfo};
use crate::surfaces::view::{InsertSize, ViewState};
use crate::ui::Ui;

pub const HEADER_H: f32 = 22.0;
/// Input cell with Unison + preamp (measured, §1.2).
pub const INPUT_CELL_H: f32 = 237.0;
/// SMALL shrinks the Unison slot like the insert slots (50 → 23): the input cell loses 27
/// (user captures s47/s48).
pub const INPUT_CELL_SMALL_DH: f32 = 27.0;
/// `Rack_UADRecord` row under the input inserts list (`(0, listH + 30, w, 42)`).
pub const REC_ROW_H: f32 = 42.0;
/// Send heights: medium (mini send) 75, compact 35, full (`Rack_SendChannelStrip`) 322; pitch
/// +3 (0x1814c0400).
pub const SEND_H: f32 = 75.0;
pub const SEND_H_COMPACT: f32 = 35.0;
pub const SEND_H_FULL: f32 = 322.0;
/// SENDS OVERVIEW: one birds-eye panel of mini send faders (`Rack_Overview_Sends`), 77 pt
/// (user capture s47: 583.5…660.5).
pub const OVERVIEW_H: f32 = 77.0;

/// How a Sends cell draws its sends (Console 1.3.1, user captures s45/s46): SENDS OVERVIEW
/// shows the birds-eye panel on every strip; otherwise input strips show full send strips
/// (route, pan, mute, fader and meter) and bus strips mini sends (knob and M).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendStyle {
    Overview,
    Medium,
    Full,
}

impl SendStyle {
    pub fn of(overview: bool, bus: bool) -> Self {
        if overview {
            SendStyle::Overview
        } else if bus {
            SendStyle::Medium
        } else {
            SendStyle::Full
        }
    }
    /// One send's height (stacked styles).
    pub fn send_h(self) -> f32 {
        match self {
            SendStyle::Overview => OVERVIEW_H,
            SendStyle::Medium => SEND_H,
            SendStyle::Full => SEND_H_FULL,
        }
    }
}
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

/// Sends cell: header 22 + `CalcNeededContentHeight` (0x1814c01f0: 4 + Σ(H + 3) − 3 + 5), or 87
/// without sends. p2: an aux with CUE 1–4 rows is 22 + 4 + 3·78 + 75 + 5 = 340. The overview is
/// one panel whatever the send count.
pub fn sends_cell_h(sends: usize, style: SendStyle) -> f32 {
    if sends == 0 {
        return SENDS_DEFAULT_H;
    }
    if style == SendStyle::Overview {
        // One panel; the cell is a point taller than header + 4 + 77 + 5 (s46/s47: 109).
        return HEADER_H + 4.0 + OVERVIEW_H + 5.0 + 1.0;
    }
    HEADER_H + 4.0 + sends as f32 * (style.send_h() + 3.0) - 3.0 + 5.0
}

fn cid(strip: &StripInfo, control: Control) -> Option<ControlId> {
    Some(ControlId { strip: strip.id.clone(), control })
}

/// Insert list (`Rack_Inserts`) at `y`, sized for `list_slots`: `Channel-Inserts-Background`
/// at `(4, 0, w−8, h)` and slot k at `(0, 5 + k·pitch + 1, w, pitch − 2)` (resized 0x181440960).
/// `labels[k]` is Console's label for slot k ([`crate::MixerModel::insert_labels`]).
/// `targeted`: the slot an "Assign..." is choosing a plug-in for (`image-targeted`, s08).
#[allow(clippy::too_many_arguments)]
pub fn insert_list(ui: &mut Ui, s: &StripInfo, slots: &[InsertState], labels: &[Option<InsertLabel>], list_slots: usize, size: InsertSize, targeted: Option<u8>, y: f32) {
    let pitch = size.pitch();
    ui.background("Channel-Inserts-Background", Rect::new(4.0, y, STRIP_W - 8.0, inserts_list_h(list_slots, size)));
    for (k, slot) in slots.iter().take(list_slots).enumerate() {
        let r = Rect::new(0.0, y + 6.0 + k as f32 * pitch, STRIP_W, pitch - 2.0);
        let label = labels.get(k).and_then(Option::as_ref);
        match size {
            InsertSize::Large => ui.insert_slot(slot, label, r, cid(s, Control::Insert(k as u8))),
            InsertSize::Small => small_slot(ui, slot, label, r, cid(s, Control::Insert(k as u8))),
        }
        if targeted == Some(k as u8) {
            if let Some(img) = ui.skin("inserts", "Small-Normal").and_then(|k| k.image("image-targeted")) {
                let img = img.to_string();
                ui.image_centered(&img, r, 1.0);
            }
            if let Some(plus) = ui.skin("inserts", "Small-Normal").and_then(|k| k.image("image-plus")) {
                let plus = plus.to_string();
                ui.image_centered(&plus, r, 1.0);
            }
        }
    }
}

/// SMALL insert slot: stretched slot art with the plug-in name (no icon).
pub fn small_slot(ui: &mut Ui, state: &InsertState, label: Option<&InsertLabel>, r: Rect, id: Option<ControlId>) {
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
            if let Some(plus) = ui.skin("inserts", "Small-Normal").and_then(|k| k.image("image-plus")) {
                let plus = plus.to_string();
                ui.image_centered(&plus, plate, 1.0);
            }
        }
        InsertState::Loaded { power, .. } => {
            let (text, italic) = crate::widgets::insert_text(state, label);
            let mut style = ui.style("inserts", "Small-Normal");
            style.italic = italic;
            if !*power {
                style.color.a *= 0.6;
            }
            // Same painter as LARGE: fitted into the plate less 4 pt a side.
            ui.text(&text.unwrap_or_default(), style, plate.inset(4.0, 0.0, 4.0, 0.0), Align::Center);
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
    let style = SendStyle::of(view.sends_overview, s.kind.is_bus());
    if style != SendStyle::Overview {
        ui.fill(Rect::new(4.0, y, STRIP_W - 8.0, well_h), SENDS_WELL);
    }
    match style {
        SendStyle::Overview => overview_panel(ui, s, sends, y),
        SendStyle::Medium => {
            // The talkback strip's sends are bus-style: M only, no PRE (s45).
            let talkback = s.kind == crate::geometry::StripKind::Talkback;
            for (k, send) in sends.iter().enumerate() {
                mini_send(ui, s, k, send, if talkback { None } else { pre(k) }, y + k as f32 * (SEND_H + 3.0));
            }
        }
        SendStyle::Full => {
            for (k, send) in sends.iter().enumerate() {
                full_send(ui, s, k, send, y + k as f32 * (SEND_H_FULL + 3.0));
            }
        }
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
/// * slot background `(4, 0, w−8, H)` (`Secondary-Send-Background`) and the lower plate
///   `(6, 34, w−12, h−34−2)` (`Send-Lower-State-Plate`, as the ref shows; a console model's
///   `Lower-State-Plate-*`, and its `Gain-*` knob, on a strip that has one);
/// * knob meter + knob `(7, 35−4, 41, 49)`;
/// * pre_and_mute `(46, 35+14, L, 19)`: L = mute + 10 with `Send-Mute` centred, or PRE + M
///   (`Mini-Send-Pre`, `Mini-Send-Mute`) 2 apart.
fn mini_send(ui: &mut Ui, s: &StripInfo, k: usize, send: &SendState, pre: Option<bool>, sy: f32) {
    let top = 35.0;
    // Send slot background `(4, y, w−8, H)` (Rack_Overview_Sends::resized 0x18152e670).
    ui.background("Secondary-Send-Background", Rect::new(4.0, sy, STRIP_W - 8.0, SEND_H));
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
    ui.text(&send.name, label, Rect::new(plate.x, plate.y + 0.5, plate.w, plate.h - 0.5), Align::Center);
    ui.background(s.send_plate_skin(), Rect::new(6.0, sy + top - 1.0, STRIP_W - 12.0, SEND_H - top - 1.0));
    let knob = Rect::new(7.0, sy + top - 4.0, 41.0, 49.0);
    ui.film("meters", "Knob-Meter", "image-film", 0.0, knob);
    ui.knob(s.send_knob_skin(), send.level, knob, cid(s, Control::SendLevel(k as u8)), Some(crate::scales::UNITY));
    let y = sy + top + 14.0;
    match pre {
        Some(on) => {
            ui.toggle("Mini-Send-Pre", Some("P"), on, true, Rect::new(46.0, y, 18.0, 19.0), cid(s, Control::SendPre(k as u8)), None);
            ui.toggle("Mini-Send-Mute", Some("M"), send.muted, true, Rect::new(66.0, y, 18.0, 19.0), cid(s, Control::SendMute(k as u8)), None);
        }
        None => ui.toggle("Send-Mute", Some("M"), send.muted, true, Rect::new(51.0, y, 32.0, 18.0), cid(s, Control::SendMute(k as u8)), None),
    }
}

/// Full send strip (`Rack_SendChannelStrip`, id `fullsend_<k>`, 322 pt): the route plate as on
/// a mini send, then the strip body (`Secondary-Send-Background`) with the pan knob, M and a
/// fader with its meters. Positions measured on user capture s45 (input sends, OVERVIEW off).
fn full_send(ui: &mut Ui, s: &StripInfo, k: usize, send: &SendState, sy: f32) {
    // Send pans are white on every strip (s45: `knob_pan_white`); stereo strips have none.
    let ids = FullSendIds {
        bus: false,
        pan: if s.stereo { None } else { Some((send.pan, cid(s, Control::SendPan(k as u8)))) },
        mute: cid(s, Control::SendMute(k as u8)),
        level: cid(s, Control::SendLevel(k as u8)),
    };
    let readout = crate::scales::format_db(crate::scales::fader_db(send.level));
    full_send_strip(ui, s.fader_skin(), send, &readout, sy, SEND_H_FULL, ids);
}

/// What a full send strip's controls are bound to (the mixer's strip controls, or the Sends
/// window's), and its pan: `Some((value, id))` draws the pan knob (mono input sends). `bus`:
/// a bus's or talkback's sends (Sends window s50 / s51), laid out without the pan row.
pub(crate) struct FullSendIds {
    pub bus: bool,
    pub pan: Option<(f64, Option<ControlId>)>,
    pub mute: Option<ControlId>,
    pub level: Option<ControlId>,
}

/// `Rack_SendChannelStrip` (resized 0x18148ad60) `h` pt tall at `sy`, 96 wide from x 0: the
/// route plate, the body, the pan knob (when `ids.pan`), M and the fader with its meters. A
/// taller strip (the Sends window's 326) lowers −∞ by the difference. Without a pan the M
/// button sits in the right third, `w − (w − (2·32 + 12))/3 − 32 − 6` = 52 (resized).
pub(crate) fn full_send_strip(ui: &mut Ui, fader_skin: &str, send: &SendState, readout: &str, sy: f32, h: f32, ids: FullSendIds) {
    let top = 35.0;
    // Route (sends-aux / send-cell, sRGB): the `Send-Route-Background` well (4, 0, w − 8, 35),
    // the `Flex-Route-Focus` plate (10, 6, w − 20, 23) with the `Slot-Color` bar at its foot and
    // the route name; then the `Send-Lower-State-Plate` body (6, 34, w − 12, h − 36), as on a
    // mini send.
    ui.background("Send-Route-Background", Rect::new(4.0, sy, STRIP_W - 8.0, top));
    let plate = Rect::new(10.0, sy + 6.0, STRIP_W - 20.0, 23.0);
    if let Some(img) = ui.skin("views", "Flex-Route-Focus").and_then(|k| k.image("image")) {
        let img = img.to_string();
        ui.image_stretched(&img, plate, 1.0);
    }
    ui.fill(Rect::new(plate.x + 0.5, plate.max_y() - 3.5, plate.w - 1.0, 3.0), route_bar(&send.name));
    let mut label = ui.style("fonts", "Flex-Route-Value-Font");
    if let Some(c) = ui.skin("fonts", "Flex-Route-Value-Font").and_then(|k| k.color("color-text-off")) {
        label.color = c;
    }
    ui.text(&send.name, label, Rect::new(plate.x, plate.y + 0.5, plate.w, plate.h - 0.5), Align::Center);
    ui.background("Send-Lower-State-Plate", Rect::new(6.0, sy + top - 1.0, STRIP_W - 12.0, h - top - 1.0));
    // The fader-and-meters box ends 4 pt above the strip's foot (`h − y − 4`, resized; meters,
    // their well and the readout measured on s45 and the Sends window); the fader runs from 4 pt
    // into the box to 4 pt short of the box's foot (s45: caps −∞ at 369, 0 dB at 254 for a send
    // at 118; the fader scale and −∞ cap of the Sends windows).
    if let Some((value, id)) = ids.pan {
        ui.knob("Pan-White", value, Rect::new(28.0, sy + top + 3.0, 40.0, 46.0), id, Some(0.5));
    }
    // Input sends keep the pan row (stereo strips leave it empty): M centred at top + 55, the
    // box from 109 (s45). Bus sends: M in the right third 6 pt under the route, the box 24 pt
    // under it (sends-aux).
    let (mute, fm) = if ids.bus {
        (Rect::new(52.0, sy + top + 6.0, 32.0, 18.0), Rect::new(0.0, sy + top + 24.0, STRIP_W, h - top - 24.0 - 4.0))
    } else {
        (Rect::new(32.0, sy + top + 55.0, 32.0, 18.0), Rect::new(0.0, sy + 109.0, STRIP_W, h - 109.0 - 4.0))
    };
    ui.toggle("Send-Mute", Some("M"), send.muted, true, mute, ids.mute, None);
    let fader = Rect::new(2.0, 4.0, (STRIP_W / 2.0).floor(), fm.h - 4.0);
    ui.fader_and_meters_with(fader_skin, false, send.level, send.meter.mono(), readout, fm, ids.level, Some(fader));
}

/// SENDS OVERVIEW panel (`Rack_Overview_Sends`, one per strip): `Birds-Eye-Send-Background`
/// `(6, y, w−12, 77)` holding one column per send, spread evenly (6-pt items, equal gaps;
/// user capture s47: centres 20.5/38.5/56.5/74.5 for four sends, 32.3/62.3 for two): the
/// `Signal-Present` LED (not on aux strips), the `Send-Overview-Fader`, a state square, the
/// route colour bar and the short name (A1, A2, C1, C2) in `Send-Label`.
fn overview_panel(ui: &mut Ui, s: &StripInfo, sends: &[SendState], y: f32) {
    let panel = Rect::new(6.0, y, STRIP_W - 12.0, OVERVIEW_H);
    ui.background("Birds-Eye-Send-Background", panel);
    let n = sends.len() as f32;
    let iw = 6.0;
    let gap = (panel.w - n * iw) / (n + 1.0);
    let aux = s.kind == crate::geometry::StripKind::Aux;
    for (k, send) in sends.iter().enumerate() {
        // Item x (whole points): the LED starts on it, the 7-pt cap one point left (s46/s47).
        let ix = (panel.x + gap * (k as f32 + 1.0) + iw * k as f32).round();
        let cx = ix + 2.5;
        if !aux {
            ui.film("meters", "Signal-Present", "image-film", 0.0, Rect::new(ix, y + 12.0, 5.0, 5.0));
        }
        // Caps (top) at y + 38.5 for −∞, + 23.5 at −6 dB, + 21.5 at 0 dB (s46/s47): a 22-pt
        // travel, which the skin's 5-pt margin gives in a 39-pt box from y + 12.5.
        ui.fader("Send-Overview-Fader", send.level, Rect::new(cx - 3.5, y + 12.5, 7.0, 39.0), cid(s, Control::SendLevel(k as u8)));
        let cue = send.name.starts_with("CUE");
        ui.fill(Rect::new(cx - 3.0, y + 47.5, 6.0, 6.0), if cue { Rgba::rgb8(44, 43, 41) } else { Rgba::rgb8(41, 35, 28) });
        ui.fill(Rect::new(cx - 3.5, y + 56.0, 7.0, 1.5), route_bar(&send.name));
        let mut style = ui.style("fonts", "Send-Label");
        style.bold = true;
        if cue {
            style.color = style.color.with_alpha(style.color.a * 0.5);
        }
        ui.text(&short_send_name(&send.name), style, Rect::new(cx - 12.0, y + 59.0, 24.0, 12.0), Align::Center);
    }
}

/// "AUX 1" → "A1", "CUE 2" → "C2".
fn short_send_name(name: &str) -> String {
    let mut parts = name.split_whitespace();
    match (parts.next(), parts.next()) {
        (Some(a), Some(n)) => format!("{}{n}", &a[..1]),
        _ => name.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::draw::Draw;
    use crate::model::MeterFrame;

    const METERS: &str = r#"{"file_format":"appearance","meters":{
        "Mono-Input-Meter":{"width":8,"height":196,"peak-height":1,"bounds-content":"0,14,16,372","bounds-clip":"0,4,16,10",
            "image-background":"meters/bg.png","image-content":"meters/on.png","image-clip":"meters/clip.png"}}}"#;

    /// What a full send strip draws of its meter: (lit-art draws, peak-line clips).
    fn send_meter_draws(meter: MeterFrame) -> (usize, usize) {
        let assets = crate::test_support::assets(METERS);
        let mut ui = Ui::new(&assets);
        let send = SendState { name: "AUX 1".into(), level: 0.5, muted: false, pan: 0.5, meter };
        let ids = FullSendIds { bus: false, pan: None, mute: None, level: None };
        full_send_strip(&mut ui, "Sends-Input-Fader", &send, "0.0", 0.0, SEND_H_FULL, ids);
        let lit = ui.list.items.iter().filter(|d| matches!(d, Draw::Image { key, .. } if &**key == "meters/on.png")).count();
        let peaks = ui.list.items.iter().filter(|d| matches!(d, Draw::Clip(Some(r)) if r.h == 1.0)).count();
        (lit, peaks)
    }

    #[test]
    fn a_send_with_level_draws_its_meter() {
        assert_eq!(send_meter_draws(MeterFrame::default()), (0, 0), "a silent send lights nothing");
        assert_eq!(send_meter_draws(MeterFrame { left: 0.4, ..Default::default() }), (1, 0));
        // A stereo send shows the louder leg on its one bar, with the held peak.
        assert_eq!(send_meter_draws(MeterFrame { left: 0.1, right: 0.4, peak_right: 0.7, ..Default::default() }), (2, 1));
    }

    #[test]
    fn cell_heights_match_geometry() {
        // p1: input inserts 314…602, aux inserts 77…319.
        assert_eq!(inserts_cell_h(4, InsertSize::Large, true), 288.0);
        assert_eq!(inserts_cell_h(4, InsertSize::Large, false), 242.0);
        assert_eq!(inserts_cell_h(4, InsertSize::Small, true), 22.0 + 92.0 + 11.0 + 55.0);
        // p2: the aux Sends cell (CUE 1–4 rows) runs 326 → 666.
        assert_eq!(sends_cell_h(4, SendStyle::Medium), 340.0);
        assert_eq!(sends_cell_h(0, SendStyle::Medium), SENDS_DEFAULT_H);
        assert_eq!(sends_cell_h(4, SendStyle::Overview), 109.0);
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
