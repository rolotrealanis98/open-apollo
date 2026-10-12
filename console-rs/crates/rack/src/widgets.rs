//! Rack components, drawn from Appearance.json skins.

use houston::{Rect, Rgba};

use crate::draw::{Align, Frame, TextStyle};
use crate::geometry::{FaderAndMetersGeometry, FaderMetrics};
use crate::input::{ControlId, HitKind};
use crate::model::{InsertState, MeterFrame};
use crate::scales;
use crate::ui::{ART_SCALE, Ui};

impl Ui<'_> {
    /// `Rack_Background` with a `backgrounds` (or `views`) skin: 9-slice if declared, stretched
    /// (corner-preserving) if `stretch`, otherwise natural size at the rect origin.
    pub fn background(&mut self, key: &str, r: Rect) {
        let Some(skin) = self.skin("backgrounds", key).or_else(|| self.skin("views", key)) else { return };
        let Some(path) = skin.image("image") else { return };
        if let Some(slice) = skin.insets("inset-9-slice-background") {
            self.image(path, r, Some(slice), None, 1.0);
        } else if skin.flag("stretch") || skin.flag("resizable") {
            self.image_stretched(path, r, 1.0);
        } else if let Some((w, h)) = self.image_size(path) {
            self.image(path, Rect::new(r.x, r.y, w, h), None, None, 1.0);
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
            self.hit(r, id, HitKind::Drag { value, span: 250.0 / sens.max(0.1), reset });
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

    /// Bar meter: 9-sliced background, lit content revealed from the bottom, clip LED.
    pub fn bar_meter(&mut self, key: &str, level: f64, clip: bool, r: Rect) {
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
        if clip {
            if let Some(c) = skin.image("image-clip") {
                self.image(c, Rect::new(r.x + clip_rect.x / ART_SCALE, r.y + clip_rect.y / ART_SCALE, clip_rect.w / ART_SCALE, clip_rect.h / ART_SCALE), None, None, 1.0);
            }
        }
    }

    /// Fader, meter well, meters, scale labels and the dB readout (`Rack_FaderAndMeters`).
    pub fn fader_and_meters(&mut self, fader_key: &str, stereo: bool, fader: f64, meters: MeterFrame, readout: &str, r: Rect, id: Option<ControlId>) {
        let left_key = if stereo { "Left-Input-Meter" } else { "Mono-Input-Meter" };
        let meter_w = self.skin("meters", left_key).and_then(|s| s.size()).map(|s| s.0).unwrap_or(if stereo { 4.0 } else { 8.0 });
        let g = FaderAndMetersGeometry::new(r.w, r.h, stereo, meter_w);
        self.at(r.x, r.y, |ui| {
            ui.background("Display-Background", g.meter_well);
            ui.bar_meter(left_key, meters.left, meters.clip_left, g.left_meter);
            if let Some(right) = g.right_meter {
                ui.bar_meter("Right-Input-Meter", meters.right, meters.clip_right, right);
            }
            ui.font_text("Small-Label-Font", readout, g.value_label, Align::Center);
            // Meter scale: labels span the lit well.
            let mskin = ui.skin("meters", "Mono-Input-Meter");
            let mcolor = mskin.and_then(|s| s.color("color-text")).unwrap_or(Rgba::rgb8(93, 101, 105));
            let content = mskin.and_then(|s| s.rect("bounds-content")).unwrap_or(Rect::new(0.0, 14.0, 16.0, 372.0));
            let (top, bottom) = (g.left_meter.y + content.y / ART_SCALE, g.left_meter.max_y() - (392.0 - content.max_y()) / ART_SCALE);
            let style = Ui::plain_style("Helvetica", 9.5, false, mcolor);
            let n = scales::METER_MARKS.len() as f32 - 1.0;
            for (i, db) in scales::METER_MARKS.iter().enumerate() {
                let y = top + (bottom - top) * i as f32 / n;
                ui.text(&format!("- {}", -db as i32), style.clone(), Rect::new(g.meter_marks.x, y - 5.0, g.meter_marks.w, 10.0), Align::Left);
            }
            ui.background("Fader-Ref-Mark-Dark", g.ref_mark);
            // Fader scale, placed with the taper.
            let fskin = ui.skin("faders", fader_key);
            let fcolor = fskin.and_then(|s| s.color("color-text")).unwrap_or(Rgba::BLACK);
            let m = FaderMetrics::from_skin(fskin);
            let (t, b) = m.travel(g.fader.h);
            let fstyle = Ui::plain_style("Helvetica", 9.5, false, fcolor);
            for (label, db) in scales::FADER_MARKS {
                let y = g.fader.y + b - (b - t) * scales::fader_tapered(db) as f32;
                ui.text(&format!("- {label}"), fstyle.clone(), Rect::new(g.fader_marks.x, y - 5.0, 30.0, 10.0), Align::Left);
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

    /// Insert slot: `inserts:Small-Normal` art; "+" when empty, plug-in icon + italic name when loaded.
    pub fn insert_slot(&mut self, state: &InsertState, r: Rect, id: Option<ControlId>) {
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
                    let (fw, fh) = (66.0f32, 27.0f32);
                    let s = (fw / w).min(fh / h);
                    let (dw, dh) = (w * s, h * s);
                    self.image(&key, Rect::new(r.x + (r.w - dw) / 2.0, r.y + 4.0 + (fh - dh) / 2.0, dw, dh), None, None, 1.0);
                }
                let short = name.strip_prefix("UAD ").unwrap_or(name);
                let style = TextStyle { family: "Helvetica".into(), size: 9.0, bold: false, italic: true, color: Rgba::WHITE.with_alpha(if *power { 0.85 } else { 0.45 }) };
                let name_r = Rect::new(r.x + 13.0, r.y + 31.0, r.w - 26.0, 12.0);
                // Long names are clipped to the plate (Console elides with "…").
                let fits = short.chars().count() <= 16;
                self.clip(Some(name_r));
                self.text(short, style, if fits { name_r } else { Rect::new(name_r.x + 1.0, name_r.y, 200.0, name_r.h) }, if fits { Align::Center } else { Align::Left });
                self.clip(None);
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

/// `w × h` centred in `r`, snapped down to the 2× pixel grid (JUCE's integer placement at 2×).
pub(crate) fn snap_centered(r: Rect, w: f32, h: f32) -> Rect {
    let snap = |v: f32| (v * 2.0).floor() / 2.0;
    Rect::new(snap(r.x + (r.w - w) / 2.0), snap(r.y + (r.h - h) / 2.0), w, h)
}
