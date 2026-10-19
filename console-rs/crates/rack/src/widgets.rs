//! Rack components, drawn from Appearance.json skins.

use houston::{Insets, Rect, Rgba};

use crate::draw::{Align, Frame};
use crate::geometry::{FaderAndMetersGeometry, FaderMetrics};
use crate::input::{Control, ControlId, HitKind};
use crate::model::{INSERT_LABEL_W, InsertLabel, InsertState, MeterFrame};
use crate::scales;
use crate::ui::{ART_SCALE, Ui};

impl Ui<'_> {
    /// `Rack_Background` with a `backgrounds` (or `views`) skin: 9-slice if declared, stretched
    /// (corner-preserving) if `stretch`, otherwise natural size at the rect origin.
    /// `Rack_Background::paint` (Rack.dll 0x1809903d0). A skin with a JSON size that is not
    /// `stretch` draws its image at that size minus `inset-background`, centred with integer
    /// halves (or, when `resizable`, fitted uniformly); otherwise the image is a 9-slice over the
    /// rect minus `inset-background`, with `inset-9-slice-background` or third-of-image corners.
    pub fn background(&mut self, key: &str, r: Rect) {
        let Some(skin) = self.skin("backgrounds", key).or_else(|| self.skin("views", key)) else { return };
        let Some(path) = skin.image("image") else { return };
        let path = path.to_string();
        let ib = skin.insets("inset-background").unwrap_or_default();
        let size = skin.size().filter(|(w, h)| *w > 0.0 && *h > 0.0);
        match size {
            Some((w, h)) if !skin.flag("stretch") => {
                if skin.flag("resizable") {
                    let f = (r.w / w).min(r.h / h);
                    self.image(&path, r.centered(w * f, h * f), None, None, 1.0);
                } else {
                    let x = r.x + ib.l + ((r.w / 2.0).trunc() - (w / 2.0).trunc());
                    let y = r.y + ib.t + ((r.h / 2.0).trunc() - (h / 2.0).trunc());
                    self.image(&path, Rect::new(x, y, w - ib.l - ib.r, h - ib.t - ib.b), None, None, 1.0);
                }
            }
            _ => {
                let dst = r.inset(ib.l, ib.t, ib.r, ib.b);
                // Without inset-9-slice-background the corners are a third of the image or of
                // the destination, whichever is smaller, per axis (0x181c237c0: `min(dst px,
                // image px) / 3`). The 47-pt side section's Channel-Bottom (194 px art) has
                // 31-px corners, the 23-pt preamp gain display (88 px display_bg1) 15-px ones.
                let slice = match skin.insets("inset-9-slice-background") {
                    Some(s) => Some(clamp_corners(s, dst)),
                    None => self.assets.images.get(&path).map(|img| {
                        let px = |pt: f32, img: u32| ((pt * ART_SCALE).round().max(0.0) as u32).min(img);
                        Insets::thirds(px(dst.w, img.width), px(dst.h, img.height))
                    }),
                };
                self.image(&path, dst, slice, None, 1.0);
            }
        }
    }

    /// Filmstrip frame for `value` 0…1, natural size, centred in `r`.
    pub fn film(&mut self, section: &str, key: &str, prop: &str, value: f64, r: Rect) {
        let Some(skin) = self.skin(section, key) else { return };
        let (Some(path), Some(fh)) = (skin.image(prop), skin.frame_height()) else { return };
        let Some(img) = self.assets.images.get(path) else { return };
        let count = (img.height / fh).max(1);
        let index = ((value.clamp(0.0, 1.0) * (count - 1) as f64).round()) as u32;
        let (w, h) = (img.width as f32 / ART_SCALE, fh as f32 / ART_SCALE);
        self.image(path, r.centered(w, h), None, Some(Frame { height: fh, index }), 1.0);
    }

    /// Filmstrip frame for `value` drawn at `size` (pt), centred in `r`.
    pub fn film_sized(&mut self, section: &str, key: &str, prop: &str, value: f64, r: Rect, size: (f32, f32)) {
        let Some(skin) = self.skin(section, key) else { return };
        let (Some(path), Some(fh)) = (skin.image(prop), skin.frame_height()) else { return };
        let Some(img) = self.assets.images.get(path) else { return };
        let count = (img.height / fh).max(1);
        let index = ((value.clamp(0.0, 1.0) * (count - 1) as f64).round()) as u32;
        self.image(path, snap_centered(r, size.0, size.1), None, Some(Frame { height: fh, index }), 1.0);
    }

    /// `Rack_Knob`: optional background image plus the filmstrip, drawn at the skin's
    /// width × height (e.g. `Small-Green-Input` scales its 41×46 art to 36×40); vertical drag.
    pub fn knob(&mut self, key: &str, value: f64, r: Rect, id: Option<ControlId>, reset: Option<f64>) {
        if let Some(bg) = self.skin("knobs", key).and_then(|s| s.image("image-background")) {
            let bg = bg.to_string();
            self.image_centered(&bg, r, 1.0);
        }
        match self.skin("knobs", key).and_then(|s| s.size()) {
            Some(size) => self.film_sized("knobs", key, "image-film", value, r, size),
            None => self.film("knobs", key, "image-film", value, r),
        }
        if let Some(id) = id {
            let sens = self.skin("knobs", key).and_then(|s| s.number("sensitivity")).unwrap_or(1.0) as f32;
            self.hit(r, id, HitKind::Knob { value, span: 250.0 / sens.max(0.1), reset });
        }
    }

    /// `Rack_Button`/`Rack_TextButton`: on/off image (stretched if the skin says so), icon
    /// overlay and/or label.
    pub fn toggle(&mut self, key: &str, label: Option<&str>, on: bool, enabled: bool, r: Rect, id: Option<ControlId>, color: Option<Rgba>) {
        let Some(skin) = self.skin("views", key) else { return };
        let img = if on { skin.image("image-on").or(skin.image("image")) } else { skin.image("image-off").or(skin.image("image")) };
        let overlay = if on { skin.image("image-overlay-on").or(skin.image("image-overlay")) } else { skin.image("image-overlay-off").or(skin.image("image-overlay")) };
        let opacity = if enabled { 1.0 } else { skin.number("opacity-disabled").unwrap_or(0.5) as f32 };
        if let Some(path) = img {
            if skin.flag("stretch") {
                self.image_stretched(path, r, opacity);
            } else {
                self.image_centered(path, r, opacity);
            }
        }
        let label = label.filter(|l| !l.is_empty());
        if let Some(ov) = overlay {
            if label.is_some() {
                if let Some((w, h)) = self.image_size(ov) {
                    self.image(ov, Rect::new(r.x + 6.0, r.max_y() - 6.0 - h, w, h), None, None, opacity);
                }
            } else if let Some((iw, ih)) = self.image_size(ov) {
                // The overlay box is width-overlay × height-overlay (else the art size), centred
                // on whole points, and the art is stretched into it.
                let (w, h) = (skin.number("width-overlay").map_or(iw, |v| v as f32), skin.number("height-overlay").map_or(ih, |v| v as f32));
                let b = Rect::new(r.x + ((r.w - w) / 2.0).floor(), r.y + ((r.h - h) / 2.0).floor(), w, h);
                self.image(ov, b, None, None, opacity);
            }
        }
        if let Some(label) = label {
            let mut style = self.style("views", key);
            let c = if on { skin.color("color-text-on") } else { skin.color("color-text-off") };
            style.color = color.or(c).or(skin.color("color-text")).unwrap_or(Rgba::BLACK);
            style.color.a *= opacity;
            let inset = skin.insets("inset-text").unwrap_or_default();
            self.text(label, style, r.inset(inset.l, inset.t, inset.r, inset.b), Align::Center);
        }
        if let Some(id) = id {
            self.hit(r, id, HitKind::Toggle);
        }
    }

    pub fn font_text(&mut self, font_key: &str, text: &str, r: Rect, align: Align) {
        let style = if self.skin("fonts", font_key).is_some() { self.style("fonts", font_key) } else { self.style("views", font_key) };
        self.text(text, style, r, align);
    }

    /// `Rack_Fader`: slot (2 pt past the travel at each end) and cap.
    pub fn fader(&mut self, key: &str, value: f64, r: Rect, id: Option<ControlId>) {
        let skin = self.skin("faders", key);
        let m = FaderMetrics::from_skin(skin);
        let (top, bottom) = m.travel(r.h);
        if let Some(slot) = skin.and_then(|s| s.image("image-slot")) {
            let slot = slot.to_string();
            let x = r.x + (r.w - m.slot_w) / 2.0;
            self.image_stretched(&slot, Rect::new(x, r.y + top - 2.0, m.slot_w, bottom - top + 4.0), 1.0);
        }
        if let Some(cap) = skin.and_then(|s| s.image("image-cap")) {
            let cap = cap.to_string();
            // Rack_Fader positions the cap on whole points (p1: 0 dB−0.01 → top 736, not 736.7).
            let cy = r.y + m.cap_center_y(value, r.h).floor();
            self.image(&cap, Rect::new(r.x + (r.w - m.cap.0) / 2.0, cy - m.cap.1 / 2.0, m.cap.0, m.cap.1), None, None, 1.0);
        }
        if let Some(id) = id {
            self.hit(r, id, HitKind::Drag { value, span: bottom - top, reset: Some(scales::UNITY) });
        }
    }

    /// Bar meter: 9-sliced background, lit content revealed from the bottom, the peak-hold line
    /// (`peak-height` pt of the lit art at `peak`, 0…1; skins without `peak-height` have none),
    /// clip LED.
    pub fn bar_meter(&mut self, key: &str, level: f64, peak: f64, clip: bool, r: Rect) {
        let Some(skin) = self.skin("meters", key) else { return };
        let bg = skin.image("image-background");
        let bg_h = bg.and_then(|p| self.assets.images.get(p)).map(|i| i.height as f32).unwrap_or(392.0);
        let content = skin.rect("bounds-content").unwrap_or(Rect::new(0.0, 14.0, 16.0, 372.0));
        let clip_rect = skin.rect("bounds-clip").unwrap_or(Rect::new(0.0, 4.0, 16.0, 10.0));
        let top = content.y / ART_SCALE;
        let bottom = (bg_h - content.max_y()) / ART_SCALE;
        let well_h = (r.h - top - bottom).max(0.0);
        if let Some(bg) = bg {
            match skin.insets("inset-9-slice-background") {
                Some(s) => self.image(bg, r, Some(s), None, 1.0),
                None => self.image_stretched(bg, r, 1.0),
            }
        }
        if let Some(lit) = skin.image("image-content") {
            let lit_h = well_h * level.clamp(0.0, 1.0) as f32;
            if lit_h > 0.0 {
                let well = Rect::new(r.x + content.x / ART_SCALE, r.y + top, content.w / ART_SCALE, well_h);
                let slice = skin.insets("inset-9-slice-content");
                self.clip(Some(Rect::new(well.x, well.max_y() - lit_h, well.w, lit_h)));
                self.image(lit, well, slice, None, 1.0);
                self.clip(None);
            }
        }
        let peak_h = skin.number("peak-height").unwrap_or(0.0) as f32;
        if let (Some(lit), true) = (skin.image("image-content"), peak_h > 0.0 && peak > 0.0) {
            // The line rides the well's travel: its foot at the peak, kept inside the well.
            let y = (r.y + top + well_h * (1.0 - peak.clamp(0.0, 1.0) as f32) - peak_h).clamp(r.y + top, r.y + top + (well_h - peak_h).max(0.0));
            let well = Rect::new(r.x + content.x / ART_SCALE, r.y + top, content.w / ART_SCALE, well_h);
            let slice = skin.insets("inset-9-slice-content");
            self.clip(Some(Rect::new(well.x, y, well.w, peak_h)));
            self.image(lit, well, slice, None, 1.0);
            self.clip(None);
        }
        if clip {
            if let Some(c) = skin.image("image-clip") {
                self.image(c, Rect::new(r.x + clip_rect.x / ART_SCALE, r.y + clip_rect.y / ART_SCALE, clip_rect.w / ART_SCALE, clip_rect.h / ART_SCALE), None, None, 1.0);
            }
        }
    }

    /// Fader, meter well, meters, scale labels and the dB readout (`Rack_FaderAndMeters`).
    pub fn fader_and_meters(&mut self, fader_key: &str, stereo: bool, fader: f64, meters: MeterFrame, readout: &str, r: Rect, id: Option<ControlId>) {
        self.fader_and_meters_with(fader_key, stereo, fader, meters, readout, r, id, None);
    }

    /// As [`Self::fader_and_meters`], with the fader (and its scale) in `fader_rect` (relative
    /// to `r`) instead of the strip geometry's: a full send strip's fader runs past its meters.
    #[allow(clippy::too_many_arguments)]
    pub fn fader_and_meters_with(&mut self, fader_key: &str, stereo: bool, fader: f64, meters: MeterFrame, readout: &str, r: Rect, id: Option<ControlId>, fader_rect: Option<Rect>) {
        let left_key = if stereo { "Left-Input-Meter" } else { "Mono-Input-Meter" };
        let meter_w = self.skin("meters", left_key).and_then(|s| s.size()).map(|s| s.0).unwrap_or(if stereo { 4.0 } else { 8.0 });
        let mut g = FaderAndMetersGeometry::new(r.w, r.h, stereo, meter_w);
        if let Some(f) = fader_rect {
            g.fader = f;
            g.fader_marks = Rect::new(g.fader_marks.x, f.y - 20.0, g.fader_marks.w, f.h + 40.0);
            g.ref_mark.y = f.max_y() - 27.0;
        }
        self.at(r.x, r.y, |ui| {
            // secondary_meter_well then meter_well, same rect (0x18141ea20).
            ui.background("Secondary-Display-Background", g.meter_well);
            ui.background("Display-Background", g.meter_well);
            ui.bar_meter(left_key, meters.left, meters.peak_left, meters.clip_left, g.left_meter);
            if let Some(right) = g.right_meter {
                ui.bar_meter("Right-Input-Meter", meters.right, meters.peak_right, meters.clip_right, right);
            }
            ui.font_text("Small-Label-Font", readout, g.value_label, Align::Center);
            // Double-click the readout: the typed-value popover (`Rack_FaderAndMeters::buttonClicked`).
            // A Sends window fader (`SettingSet("fader:…/GainTapered")`) too.
            if let Some(id) = id.as_ref().filter(|id| matches!(&id.control, Control::Fader | Control::SendLevel(_)) || matches!(&id.control, Control::SettingSet(k) if k.starts_with("fader:"))) {
                ui.hit(g.value_label, ControlId { strip: id.strip.clone(), control: Control::ValueEntry(Box::new(id.control.clone())) }, HitKind::Tap);
            }
            // Meter scale (`Rack_MeterMarks`, labels 0x1809d1470): a 3×1 tick at the scale's left
            // edge per label, a 1×1 dot halfway, the number 7 pt right of the tick (measured).
            let mskin = ui.skin("meters", "Mono-Input-Meter");
            let mcolor = mskin.and_then(|s| s.color("color-text")).unwrap_or(Rgba::rgb8(93, 101, 105));
            let content = mskin.and_then(|s| s.rect("bounds-content")).unwrap_or(Rect::new(0.0, 14.0, 16.0, 372.0));
            let (top, bottom) = (g.left_meter.y + content.y / ART_SCALE, g.left_meter.max_y() - (392.0 - content.max_y()) / ART_SCALE);
            let mut style = ui.style("meters", "Mono-Input-Meter");
            style.color = mcolor;
            let n = scales::METER_MARKS.len() as f32 - 1.0;
            let step = (bottom - top) / n;
            for (i, db) in scales::METER_MARKS.iter().enumerate() {
                let y = (top + step * i as f32).floor();
                ui.fill(Rect::new(g.meter_marks.x, y, 3.0, 1.0), mcolor);
                if i + 1 < scales::METER_MARKS.len() {
                    ui.fill(Rect::new(g.meter_marks.x, y + (step / 2.0).ceil(), 1.0, 1.0), mcolor);
                }
                // Numbers centred 10.25 pt right of the tick (p2 / Sends windows: "3" and "12"
                // share a centre).
                ui.text(&format!("{}", -db as i32), style.clone(), Rect::new(g.meter_marks.x + 0.25, y - 4.75, 20.0, 10.0), Align::Center);
            }
            ui.background("Fader-Ref-Mark-Dark", g.ref_mark);
            // Fader scale (`Rack_FaderMarks`, 0x1809b1c60), placed with the taper: a 4×1 tick at the
            // scale's left edge and the number centred 11 pt right of it ("6" and "12" share a
            // centre, p2).
            let fskin = ui.skin("faders", fader_key);
            let fcolor = fskin.and_then(|s| s.color("color-text")).unwrap_or(Rgba::BLACK);
            let m = FaderMetrics::from_skin(fskin);
            let (t, b) = m.travel(g.fader.h);
            let mut fstyle = ui.style("faders", fader_key);
            fstyle.color = fcolor;
            for (label, db) in scales::FADER_MARKS {
                // Ticks sit on whole points; the numbers follow the unrounded taper position.
                let exact = g.fader.y + b - (b - t) * scales::fader_tapered(db) as f32;
                ui.fill(Rect::new(g.fader_marks.x, exact.floor(), 4.0, 1.0), fcolor);
                ui.text(label, fstyle.clone(), Rect::new(g.fader_marks.x + 1.0, exact - 4.25, 20.0, 10.0), Align::Center);
            }
            // Gain button separator (0x18141ea20, no polarity toggle): a dotted 1-pt line at
            // (value.x + 6, value.y − 3), value = (well.x, well.bottom − 20); white 0.15 dots every 2 pt.
            let dots = Rect::new(g.meter_well.x + 6.0, g.meter_well.max_y() - 23.0, g.meter_well.w - 12.0, 1.0);
            for k in 0..((dots.w as i32 + 3) / 2) {
                ui.fill(Rect::new(dots.x + 2.0 * k as f32, dots.y, 1.0, 1.0), Rgba::WHITE.with_alpha(0.15));
            }
            ui.fader(fader_key, fader, g.fader, id);
        });
    }

    /// Channel name text (`views:Channel-Input-Name` / `Channel-Aux-Name`): bounds-text inset
    /// by inset-text, 3 pt above the 40-pt art's frame. The plate itself is painted by the caller.
    pub fn name_plate(&mut self, key: &str, name: &str, r: Rect) {
        let Some(skin) = self.skin("views", key) else { return };
        let text = skin.rect("bounds-text").unwrap_or(Rect::new(0.0, 3.0, 97.0, 36.0));
        let inset = skin.insets("inset-text").unwrap_or_default();
        let mut style = self.style("views", key);
        if key == "Channel-Aux-Name" {
            // 1.3.1 draws the aux name brighter than the skin's 0.4 (measured 0.5 on the flat plate).
            style.color = Rgba::WHITE.with_alpha(0.5);
        }
        let rect = Rect::new(r.x + text.x + inset.l, r.y + text.y + inset.t - 3.0, text.w - inset.l - inset.r, text.h - inset.t - inset.b);
        self.text(name, style, rect, Align::Center);
    }

    /// Insert slot (`Rack_InsertsButton`, LARGE): `inserts:Small-Normal` art; "+" when empty;
    /// when loaded, the plug-in icon and Console's label (`label`, else the name without "UAD ").
    pub fn insert_slot(&mut self, state: &InsertState, label: Option<&InsertLabel>, r: Rect, id: Option<ControlId>) {
        let (text, italic) = insert_text(state, label);
        self.insert_slot_labeled(state, text.as_deref(), italic, r, id);
    }

    /// Insert slot with an explicit label (`Rack_InsertButtonPainter::PaintInsertContent`
    /// 0x18142c290, measured on p1's "CL 1B mkII", a native UADx insert): icon fitted in
    /// `(16, 4, 64, 25)` (icons are 400×160 px → 62.5 × 25), label in the `inserts:Small-Normal` font
    /// (Helvetica bold 9 dyn, white 0.7) centred on y + 38. Native and running UAD-2 plug-ins use
    /// the upright face; `italic` is Console's offline (Apollo not running) form.
    pub fn insert_slot_labeled(&mut self, state: &InsertState, label: Option<&str>, italic: bool, r: Rect, id: Option<ControlId>) {
        let Some(skin) = self.skin("inserts", "Small-Normal") else { return };
        let img = match state {
            InsertState::Empty => skin.image("image-empty"),
            InsertState::Loaded { power: true, .. } => skin.image("image-active"),
            InsertState::Loaded { power: false, .. } => skin.image("image-power-off"),
        };
        if let Some(p) = img {
            self.image_centered(p, r, 1.0);
        }
        match state {
            InsertState::Empty => {
                if let Some(plus) = skin.image("image-plus") {
                    self.image_centered(plus, r, 1.0);
                }
            }
            InsertState::Loaded { name, power } => {
                let key = format!("plugin:{name}");
                if let Some((w, h)) = self.image_size(&key) {
                    let b = Rect::new(r.x + 16.0, r.y + 4.0, 64.0, 25.0);
                    let s = (b.w / w).min(b.h / h);
                    self.image(&key, snap_centered(b, w * s, h * s), None, None, if *power { 1.0 } else { 0.5 });
                }
                if let Some(label) = label {
                    let mut style = self.style("inserts", "Small-Normal");
                    style.italic = italic;
                    if !*power {
                        style.color.a *= 0.6;
                    }
                    // drawFittedText in the plate (10, 76) less 4 a side: long names are
                    // squeezed to 0.7, then cut with "..." (the renderer fits single lines).
                    let name_r = Rect::new(r.x + 14.0, r.y + 31.0, INSERT_LABEL_W, 14.0);
                    self.text(label, style, name_r, Align::Center);
                }
            }
        }
        if let Some(id) = id {
            self.hit(r, id, HitKind::Tap);
        }
    }

    /// `Rack_ChannelStripCell` header (`(0, 0, w, 22)`, paint 0x18149f850): `Mixer-Row-Header`
    /// (bus cells: `-Bus`) with the title centred on the whole header. `plus` adds the inserts
    /// header's "+" (a 2-pt cross, 8 × 8, at x 8 / y 7; white 0.6, measured). Bus titles draw at
    /// 0.62 of the header colour (measured on the dark header).
    pub fn cell_header(&mut self, title: &str, plus: bool, bus: bool, r: Rect) {
        self.background(if bus { "Mixer-Row-Header-Bus" } else { "Mixer-Row-Header" }, r);
        let mut style = self.style("fonts", "Channel-Strip-Section-Header");
        if bus {
            style.color.a *= 0.62;
        }
        if plus {
            let c = Rgba::WHITE.with_alpha(0.59);
            self.fill(Rect::new(r.x + 8.0, r.y + 10.0, 8.0, 2.0), c);
            self.fill(Rect::new(r.x + 11.0, r.y + 7.0, 2.0, 3.0), c);
            self.fill(Rect::new(r.x + 11.0, r.y + 12.0, 2.0, 3.0), c);
        }
        self.text(title, style, r, Align::Center);
    }
}

/// A loaded slot's text and face: the model's label, else the name without "UAD ", upright.
pub(crate) fn insert_text(state: &InsertState, label: Option<&InsertLabel>) -> (Option<String>, bool) {
    match (state, label) {
        (InsertState::Empty, _) => (None, false),
        (InsertState::Loaded { .. }, Some(l)) => (Some(l.text.clone()), l.italic),
        (InsertState::Loaded { name, .. }, None) => (Some(name.replace("UAD ", "")), false),
    }
}

/// `w × h` centred in `r`, snapped down to the 2× pixel grid (JUCE's integer placement at 2×).
pub(crate) fn snap_centered(r: Rect, w: f32, h: f32) -> Rect {
    let snap = |v: f32| (v * 2.0).floor() / 2.0;
    Rect::new(snap(r.x + (r.w - w) / 2.0), snap(r.y + (r.h - h) / 2.0), w, h)
}

/// 9-slice insets (image px) limited to half the destination (pt × ART_SCALE) per axis.
fn clamp_corners(s: Insets, dst: Rect) -> Insets {
    let (hw, hh) = (dst.w * ART_SCALE / 2.0, dst.h * ART_SCALE / 2.0);
    let fit = |a: f32, b: f32, half: f32| if a + b > 2.0 * half { (a.min(half), b.min(half)) } else { (a, b) };
    let (l, r) = fit(s.l, s.r, hw);
    let (t, b) = fit(s.t, s.b, hh);
    Insets { l, t, r, b }
}
