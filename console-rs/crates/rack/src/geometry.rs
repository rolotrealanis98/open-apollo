//! Geometry recovered from Rack.dll (UAD Console 1.2.x) and verified against Console 1.3.1
//! screenshots (`console-port/rack/mixer-geometry.md`). All values in points.

use houston::Rect;

pub const STRIP_W: f32 = 96.0;

/// Window chrome and columns of the full 1920×1050 Console window (§0).
pub mod window {
    pub const SIZE: (f32, f32) = (1920.0, 1050.0);
    pub const TITLE_BAR: f32 = 26.0;
    /// Top of the columns below the title bar (1.3.1: the bar is 27 pt including its
    /// bottom black line, which also covers the browser's first point).
    pub const CONTENT_TOP: f32 = 27.0;
    pub const NAV_STRIP: (f32, f32) = (27.0, 71.0);
    pub const BROWSER_W: f32 = 345.0;
    pub const NAV_COLUMN: (f32, f32) = (345.0, 96.0);
    /// `Rack_ChannelStripsSideSection`: 47 pt, so the strips start on the whole point 488 and its
    /// Channel-Bottom's black right edge lands on 487 (p1).
    pub const SIDE_SECTION: (f32, f32) = (441.0, 47.0);
    pub const AUX_X: f32 = 1613.5;
    /// Console 1.3.1 draws the monitor column flush right (1920 − 96): every UA image in it
    /// matches at x0 = 1824 (p1 refpair), half a point right of the 18-pt gap after the auxes.
    pub const MONITOR_X: f32 = 1824.0;
    pub const COLUMN: (f32, f32) = (72.0, 1010.0);
    pub const STATUS_BAR: (f32, f32) = (1010.0, 40.0);
    pub const INPUT_CELL_H: f32 = 237.0;
    pub const INSERTS_CELL_H: f32 = 271.0;
    pub const AUX_INSERTS_CELL_H: f32 = 235.0;
    /// `Rack_MonitorSection_UAD2` top: the section spans 27…1010 (h 983, G = h − 100 → 910);
    /// its monitor_meters background starts 4 pt lower, at 31.
    pub const MONITOR_TOP: f32 = 27.0;
}

/// `Rack_MainChannelStrip::resized` (0x18144cef0), branch used by UAD Console (§2).
pub mod main_strip {
    pub const FADER_AREA: f32 = 371.0;
    pub const LABEL_H: f32 = 28.0;
    pub const HEIGHT: f32 = FADER_AREA + LABEL_H;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StripKind {
    Input,
    Aux,
    /// The talkback mic strip (`Rack_TalkbackChannelStrip`, T = 7): shown in the bus section
    /// after the auxes while SHOW CTRL ROOM is on.
    Talkback,
}

impl StripKind {
    /// Aux and talkback strips: the bus section's strips.
    pub fn is_bus(self) -> bool {
        matches!(self, StripKind::Aux | StripKind::Talkback)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MainStripGeometry {
    pub pans: Option<Rect>,
    pub solo_mute: Option<Rect>,
    pub pre_post: Option<Rect>,
    pub mono_mute: Option<Rect>,
    /// Talkback strip: TALK, then TALKBACK TO MONITOR (`Rack_Talkback`, two 80 × 40 rows).
    pub talkback: Option<(Rect, Rect)>,
    pub fader_meters: Rect,
    pub label: Rect,
}

impl MainStripGeometry {
    pub fn new(kind: StripKind) -> Self {
        let (w, h) = (STRIP_W, main_strip::HEIGHT);
        let f = h - main_strip::LABEL_H;
        let k = 26.0; // console offset
        let y = 8.0;
        let (pans, solo_mute, pre_post, mono_mute) = match kind {
            StripKind::Input => (
                Some(Rect::new(((w - 84.0) / 2.0).floor(), y - 10.0, 84.0, 70.0)),
                Some(Rect::new(0.0, y + 54.0, w, 19.0)),
                None,
                None,
            ),
            StripKind::Aux => (None, None, Some(Rect::new(0.0, y, w, 40.0)), Some(Rect::new(0.0, y + 40.0, w, 40.0))),
            StripKind::Talkback => (None, None, None, None),
        };
        let talkback = (kind == StripKind::Talkback).then(|| (Rect::new(8.0, y, 80.0, 40.0), Rect::new(8.0, y + 40.0, 80.0, 40.0)));
        Self {
            pans,
            solo_mute,
            pre_post,
            mono_mute,
            talkback,
            // Talkback: `(0, 83, w, F − 87)` (mixer-geometry §2.3), a point below the aux's.
            fader_meters: if kind == StripKind::Talkback { Rect::new(0.0, 83.0, w, f - 87.0) } else { Rect::new(0.0, 108.0 - k, w, f - 112.0 + k) },
            label: Rect::new(0.0, h - main_strip::LABEL_H, w, main_strip::LABEL_H),
        }
    }
}

/// `Rack_FaderAndMeters::resized` (0x18141ea20), relative to the F&M rect (§2.5).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FaderAndMetersGeometry {
    pub fader: Rect,
    pub ref_mark: Rect,
    pub meter_well: Rect,
    pub value_label: Rect,
    pub left_meter: Rect,
    pub right_meter: Option<Rect>,
    pub meter_marks: Rect,
    pub fader_marks: Rect,
}

impl FaderAndMetersGeometry {
    pub fn new(w: f32, h: f32, stereo: bool, meter_w: f32) -> Self {
        let (w, h) = (w.floor(), h.floor());
        let half = (w / 2.0).floor();
        let fx = 2.0;
        let fader = Rect::new(fx, 4.0, half, h - 4.0);
        let meter_well = Rect::new(half + 2.0, 6.0, half - 11.0, h - 13.0);
        let meter_h = (h - 13.0) - 31.0;
        let left_meter = Rect::new(half + 9.0, 11.0, meter_w, meter_h);
        let right_meter = stereo.then(|| Rect::new(left_meter.max_x(), 11.0, meter_w, meter_h));
        let last = right_meter.unwrap_or(left_meter);
        let marks_w = 18.0;
        Self {
            fader,
            ref_mark: Rect::new((half / 2.0).floor() - 1.0 + fx, fader.max_y() - 27.0, 24.0, 11.0),
            meter_well,
            value_label: Rect::new(meter_well.x, meter_well.y + meter_well.h - 20.0, meter_well.w, 16.0),
            left_meter,
            right_meter,
            meter_marks: Rect::new(last.max_x(), last.y - 20.0, (w / 2.0 - 11.0) - last.w, last.h + 40.0),
            fader_marks: Rect::new(half - marks_w + 2.0, fader.y - 20.0, marks_w, fader.h + 40.0),
        }
    }
}

/// Fader cap travel from the `faders` skin (Rack_Fader 0x1809ad2f0 / 0x1809acc00).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FaderMetrics {
    pub cap: (f32, f32),
    pub slot_w: f32,
    pub overlap: f32,
    pub margin: f32,
}

impl FaderMetrics {
    pub fn from_skin(skin: Option<&houston::Skin>) -> Self {
        let n = |p: &str, d: f64| skin.and_then(|s| s.number(p)).unwrap_or(d) as f32;
        Self { cap: (n("width-cap", 48.0), n("height-cap", 76.0)), slot_w: n("width-slot", 6.0), overlap: n("overlap-cap", 14.0), margin: n("height-margin", 5.0) }
    }
    /// (top, bottom) cap-centre y within a fader of height `h`.
    pub fn travel(&self, h: f32) -> (f32, f32) {
        let half = self.cap.1 / 2.0 - self.overlap;
        (self.margin + half, h - self.margin - half)
    }
    pub fn cap_center_y(&self, value: f64, h: f32) -> f32 {
        let (t, b) = self.travel(h);
        b - (b - t) * value.clamp(0.0, 1.0) as f32
    }
}
