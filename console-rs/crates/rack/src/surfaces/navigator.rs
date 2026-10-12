//! Side navigator column (x 345…441 in the full window): session box, `Rack_MixerNavigator`
//! section rows (INPUTS / INSERTS / SENDS / FADER) with OPEN / CLOSE, LARGE / SMALL,
//! FIXED SLOTS, SENDS OVERVIEW; `Rack_InsertOperations` (INSERT EFFECTS UAD REC / MON,
//! MODIFIERS) and SHOW (BROWSER, PLUG-IN SCENES, SETTINGS). Geometry from
//! `Rack_MixerNavigator::resized` (0x180daf400), checked against Console 1.3.1 (refpair p1).

use houston::{Rect, Rgba};

use crate::draw::{Align, TextStyle};
use crate::input::{Control, ControlId, HitKind};
use crate::model::MixerModel;
use crate::surfaces::view::{InsertSize, Modifier, Section, ViewState};
use crate::ui::Ui;

pub const NAV_ID: &str = "nav";
pub const WIDTH: f32 = 96.0;
const SECTION_ON: Rgba = Rgba::rgb8(212, 146, 5);
const SECTION_OFF: Rgba = Rgba::rgb8(92, 100, 105);
/// Viewport bracket (measured, Console 1.3.1).
const BRACKET: Rgba = Rgba::rgb8(242, 169, 59);
/// `lower_background` fill (`mixer_scrollbar_hidden`).
const LOWER_PANEL: Rgba = Rgba::rgb8(49, 53, 55);
/// 1 pt highlight between the INSERT EFFECTS / MODIFIERS / SHOW groups.
const DIVIDER: Rgba = Rgba::rgb8(72, 76, 79);

fn nid(control: Control) -> ControlId {
    ControlId { strip: NAV_ID.into(), control }
}

/// Rows whose cells are (partly) visible in the cells viewport, for the amber bracket.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct VisibleRows {
    pub inputs: bool,
    pub inserts: bool,
    pub sends: bool,
}

/// JUCE text size on macOS for a skin font: `font-size-dyn` fonts come out 10 % larger, and
/// JUCE's font height maps to 0.84 of the point size for Helvetica Neue (ascent + descent).
/// Measured on Console 1.3.1 (refpair p1); Helvetica fonts without `font-size-dyn` are exact.
pub(crate) fn console_style(ui: &Ui, section: &str, key: &str) -> TextStyle {
    let mut st = ui.style(section, key);
    if let Some(skin) = ui.skin(section, key) {
        if skin.flag("font-size-dyn") {
            st.size *= 1.1;
        }
    }
    if &*st.family == "Helvetica Neue" {
        st.size *= 0.84;
    }
    st
}

/// JUCE `drawFittedText`: shrink `style` so `text` fits `width` (Helvetica Bold widths;
/// JUCE squeezes horizontally instead, which the renderer cannot do).
pub(crate) fn fitted(mut style: TextStyle, text: &str, width: f32) -> TextStyle {
    let w = text.lines().map(|l| crate::window::helvetica_bold_width(l, style.size)).fold(0.0, f32::max);
    if w > width {
        style.size *= width / w;
    }
    style
}

impl Ui<'_> {
    /// `Console-Nav-Row-Button-*`: stretched `btn_display` art, label in the top part, overlay
    /// icon at `bounds-overlay` (points).
    pub fn nav_button(&mut self, key: &str, label: &str, on: bool, r: Rect, id: ControlId, kind: HitKind) {
        let Some(skin) = self.skin("views", key) else { return };
        let img = if on { skin.image("image-on") } else { skin.image("image-off") };
        if let Some(p) = img {
            self.image_stretched(p, r, 1.0);
        }
        let overlay = skin.image("image-overlay");
        let mut style = fitted(console_style(self, "views", key), label, r.w - 8.0);
        style.color = (if on { skin.color("color-text-on") } else { skin.color("color-text-off") }).unwrap_or(Rgba::WHITE);
        match overlay {
            Some(ov) => {
                let b = skin.rect("bounds-overlay").unwrap_or(Rect::new(9.0, 13.0, 12.0, 12.0));
                let alpha = skin.number("opacity-overlay").unwrap_or(1.0) as f32;
                let ov = ov.to_string();
                self.image(&ov, Rect::new(r.x + b.x, r.y + b.y, b.w, b.h), None, None, alpha);
                self.text(label, style, Rect::new(r.x - 4.0, r.y + 1.0, r.w + 8.0, 12.0), Align::Center);
            }
            None => {
                let i = skin.insets("inset-text").unwrap_or_default();
                self.text(label, style, r.inset(i.l, i.t, i.r, i.b), Align::Center)
            }
        }
        self.hit(r, id, kind);
    }

    /// `Switch-*-Mode` modifier button: overlay icon at `bounds-overlay`, label in `bounds-content`.
    pub fn mode_switch(&mut self, key: &str, label: &str, on: bool, r: Rect, id: ControlId) {
        let Some(skin) = self.skin("views", key) else { return };
        let img = if on { skin.image("image-on") } else { skin.image("image-off") };
        if let Some(p) = img {
            self.image_centered(p, r, 1.0);
        }
        let mut style = self.style("views", key);
        style.color = skin.color("color-text").unwrap_or(Rgba::BLACK);
        match (skin.image("image-overlay"), skin.rect("bounds-overlay"), skin.rect("bounds-content")) {
            (Some(ov), Some(b), Some(c)) => {
                let ov = ov.to_string();
                self.image(&ov, Rect::new(r.x + b.x, r.y + b.y, b.w, b.h), None, None, 0.85);
                self.text(label, style, Rect::new(r.x + c.x, r.y + c.y, c.w, c.h), Align::Center);
            }
            _ => self.text(label, style, r, Align::Center),
        }
        self.hit(r, id, HitKind::Toggle);
    }
}

/// Navigator geometry, relative to the column origin (`Rack_MixerNavigator::resized`
/// 0x180daf400, checked against Console 1.3.1 at 2×): session box (4, 4, 88, 36); Display-Nav
/// plate at y 44 holding the section rows (8, 54 + 25·i, 80, 25), FADER at y 133 (the 0x400
/// row is placed 4 pt lower), OPEN/CLOSE at y 170, LARGE/SMALL at 206, FIXED SLOTS 242,
/// SENDS OVERVIEW 278 (buttons 30 × 28, 8 apart, x 14 and 52); the plate ends at 316. The
/// lower panel (`lower_background`) starts at 320 and holds `Rack_InsertOperations` and SHOW.
pub mod layout {
    pub const SESSION: (f32, f32, f32, f32) = (4.0, 4.0, 88.0, 36.0);
    pub const PLATE: (f32, f32) = (44.0, 272.0);
    pub const ROWS_Y: f32 = 54.0;
    pub const ROW_PITCH: f32 = 25.0;
    pub const FADER_Y: f32 = 133.0;
    pub const OPEN_Y: f32 = 170.0;
    pub const SIZE_Y: f32 = 206.0;
    pub const FIXED_Y: f32 = 242.0;
    pub const OVERVIEW_Y: f32 = 278.0;
    pub const LOWER: (f32, f32) = (320.0, 432.0);
    pub const OPS_Y: f32 = 320.0;
    pub const SHOW_Y: f32 = 612.0;
}

/// A `stretch` background drawn with the largest corners that fit: every image row/column is
/// kept 1:1 except the middle one. Console 1.3.1 draws the session navigator's Display-Nav
/// well (174 × 354 px art at 2338 × 74 px) this way; 12 px corners smear its 20 px bevel.
pub(crate) fn background_max_corners(ui: &mut Ui, key: &str, r: Rect) {
    let Some(path) = ui.skin("backgrounds", key).and_then(|s| s.image("image")) else { return };
    let Some(img) = ui.assets.images.get(path) else { return };
    let ix = (img.width as f32 / 2.0 - 1.0).min(r.w * crate::ui::ART_SCALE / 2.0).max(0.0).floor();
    let iy = (img.height as f32 / 2.0 - 1.0).min(r.h * crate::ui::ART_SCALE / 2.0).max(0.0).floor();
    ui.image(path, r, Some(houston::Insets { l: ix, t: iy, r: ix, b: iy }), None, 1.0);
}

/// Rounded rectangle fill with anti-aliased corners at 0.5 pt (one 2× pixel) resolution.
pub(crate) fn fill_rounded(ui: &mut Ui, r: Rect, radius: f32, color: Rgba) {
    let steps = (radius * 2.0).ceil() as usize;
    let inset = |k: usize| {
        let dy = radius - (k as f32 + 0.5) * 0.5;
        radius - (radius * radius - dy * dy).max(0.0).sqrt()
    };
    ui.fill(Rect::new(r.x, r.y + radius, r.w, r.h - 2.0 * radius), color);
    for k in 0..steps {
        let i = inset(k);
        let whole = (i * 2.0).ceil() / 2.0;
        let frac = 1.0 - (whole - i) * 2.0;
        for y in [r.y + k as f32 * 0.5, r.max_y() - (k as f32 + 1.0) * 0.5] {
            ui.fill(Rect::new(r.x + whole, y, r.w - 2.0 * whole, 0.5), color);
            if whole > 0.0 && frac < 1.0 {
                let c = color.with_alpha(color.a * (1.0 - frac));
                ui.fill(Rect::new(r.x + whole - 0.5, y, 0.5, 0.5), c);
                ui.fill(Rect::new(r.max_x() - whole, y, 0.5, 0.5), c);
            }
        }
    }
}

/// A `CR-Label` group title, fitted to the lower panel.
fn label(ui: &mut Ui, text: &str, r: Rect, align: Align) {
    let style = fitted(console_style(ui, "fonts", "CR-Label"), text, r.w - 8.0);
    ui.text(text, style, r, align);
}

/// Draws the navigator column at `x` (window points), from `top` (below the title bar) to
/// `bottom` (the status bar, which overlaps the column's last point).
pub fn draw_navigator(ui: &mut Ui, model: &dyn MixerModel, view: &ViewState, x: f32, top: f32, bottom: f32, rows: VisibleRows) {
    use layout as L;
    ui.at(x, top, |ui| {
        ui.background("Monitor-Nav", Rect::new(0.0, 0.0, WIDTH, bottom - top + 1.0));

        // Session box: "SESSION" over the session name.
        let (sx, sy, sw, sh) = L::SESSION;
        ui.background("Display-Background", Rect::new(sx, sy, sw, sh));
        ui.font_text("Info-Bridge-Label", "SESSION", Rect::new(sx, sy + 7.0, sw, 12.0), Align::Center);
        let name = Ui::plain_style("Helvetica", 10.0, true, Rgba::WHITE.with_alpha(0.5));
        ui.clip(Some(Rect::new(sx + 4.0, sy, sw - 8.0, sh)));
        ui.text(&model.session_name(), name, Rect::new(sx, sy + 18.5, sw, 13.0), Align::Center);
        ui.clip(None);

        // Section rows on the Display-Nav plate. Text and dot are lit (Icon-Show-Selected) for
        // the rows the cells viewport shows; the dot is filled while the row is open.
        ui.background("Display-Nav", Rect::new(0.0, L::PLATE.0, WIDTH, L::PLATE.1));
        let row_y = |i: f32| L::ROWS_Y + i * L::ROW_PITCH;
        let sections = [
            (Section::Inputs, "INPUTS", row_y(0.0), rows.inputs),
            (Section::Inserts, "INSERTS", row_y(1.0), rows.inserts),
            (Section::Sends, "SENDS", row_y(2.0), rows.sends),
            (Section::Fader, "FADER", L::FADER_Y, false),
        ];
        let select = ui.skin("views", "Display-Select-Console");
        let base = console_style(ui, "views", "Display-Select-Console");
        let inset = select.and_then(|s| s.insets("inset-text")).unwrap_or_default();
        for (s, label, y, lit) in sections {
            let row = Rect::new(8.0, y, 80.0, L::ROW_PITCH);
            let color = select.and_then(|k| k.color(if lit { "color-text-on" } else { "color-text-off" }));
            let st = TextStyle { color: color.unwrap_or(if lit { SECTION_ON } else { SECTION_OFF }), ..base.clone() };
            ui.text(label, st, Rect::new(row.x + inset.l + 4.0, row.y - 1.0, 60.0, row.h), Align::Left);
            let icon = if lit { "Icon-Show-Selected" } else { "Icon-Show" };
            if let Some(img) = ui.skin("views", icon).and_then(|k| k.image(if view.sections.get(s) { "image-on" } else { "image-off" })) {
                ui.image(img, Rect::new(row.max_x() - 11.0 - 8.0, y + 7.0, 11.0, 11.0), None, None, 1.0);
            }
            ui.hit(row, nid(Control::Section(s)), HitKind::Toggle);
        }
        // Bracket around the rows visible in the cells viewport.
        let vis = [(rows.inputs, row_y(0.0)), (rows.inserts, row_y(1.0)), (rows.sends, row_y(2.0))];
        let first = vis.iter().find(|(v, _)| *v).map(|(_, y)| *y);
        let last = vis.iter().rev().find(|(v, _)| *v).map(|(_, y)| *y + L::ROW_PITCH);
        if let (Some(a), Some(b)) = (first, last) {
            let (bx, bw) = (12.5, 70.5);
            ui.fill(Rect::new(bx, a - 0.5, bw, 1.0), BRACKET);
            ui.fill(Rect::new(bx, b - 0.5, bw, 1.0), BRACKET);
            for xx in [bx, bx + bw - 1.0] {
                ui.fill(Rect::new(xx, a - 0.5, 1.0, 3.5), BRACKET);
                ui.fill(Rect::new(xx, b - 3.0, 1.0, 3.5), BRACKET);
            }
        }

        // OPEN / CLOSE and LARGE / SMALL are momentary; FIXED SLOTS and SENDS OVERVIEW latch.
        let b = |x: f32, y: f32| Rect::new(x, y, 30.0, 28.0);
        ui.nav_button("Console-Nav-Row-Button-Open", "OPEN", false, b(14.0, L::OPEN_Y), nid(Control::SectionsOpenAll), HitKind::Tap);
        ui.nav_button("Console-Nav-Row-Button-Close", "CLOSE", false, b(52.0, L::OPEN_Y), nid(Control::SectionsCloseAll), HitKind::Tap);
        ui.nav_button("Console-Nav-Row-Button-Large", "LARGE", false, b(14.0, L::SIZE_Y), nid(Control::InsertSize(InsertSize::Large)), HitKind::Tap);
        ui.nav_button("Console-Nav-Row-Button-Small", "SMALL", false, b(52.0, L::SIZE_Y), nid(Control::InsertSize(InsertSize::Small)), HitKind::Tap);
        ui.nav_button("Console-Nav-Row-Button-Fixed-Slots", "FIXED SLOTS", view.fixed_slots, Rect::new(14.0, L::FIXED_Y, 68.0, 28.0), nid(Control::FixedSlots), HitKind::Toggle);
        ui.nav_button("Console-Nav-Row-Button-Fixed-Slots", "SENDS\nOVERVIEW", view.sends_overview, Rect::new(14.0, L::OVERVIEW_Y, 68.0, 28.0), nid(Control::SendsOverview), HitKind::Toggle);

        // Lower panel: INSERT EFFECTS, MODIFIERS (`Rack_InsertOperations`), SHOW.
        fill_rounded(ui, Rect::new(4.0, L::LOWER.0, WIDTH - 8.0, L::LOWER.1), 3.0, LOWER_PANEL);
        let ops = L::OPS_Y;
        // INSERT EFFECTS: global UAD REC (Switch-Red) / MON (Switch-Blue), lit when any input is in that mode.
        let inputs = model.inputs();
        let pre: Vec<bool> = inputs.iter().filter(|s| s.kind == crate::geometry::StripKind::Input).map(|s| model.records_pre_effects(s)).collect();
        label(ui, "INSERT EFFECTS", Rect::new(4.0, ops + 4.0, WIDTH - 8.0, 16.0), Align::Center);
        let (rec_on, mon_on) = (pre.iter().any(|p| !p), pre.iter().any(|p| *p));
        let (rec, mon) = (Rect::new(8.0, ops + 20.0, 40.0, 40.0), Rect::new(48.0, ops + 20.0, 40.0, 40.0));
        ui.toggle("Switch-Red", Some("UAD\nREC"), rec_on, true, rec, None, None);
        ui.hit(rec, nid(Control::AllRecordPreEffects(false)), HitKind::Tap);
        ui.toggle("Switch-Blue", Some("UAD\nMON"), mon_on, true, mon, None, None);
        ui.hit(mon, nid(Control::AllRecordPreEffects(true)), HitKind::Tap);
        ui.fill(Rect::new(7.0, ops + 62.0, WIDTH - 14.0, 1.0), DIVIDER);

        label(ui, "MODIFIERS", Rect::new(4.0, ops + 67.0, WIDTH - 8.0, 16.0), Align::Center);
        let mods = [
            (Modifier::Power, "Switch-Power-Mode", "POWER"),
            (Modifier::Remove, "Switch-Remove-Mode", "REMOVE"),
            (Modifier::Copy, "Switch-Copy-Mode", "COPY"),
            (Modifier::Isolate, "Switch-Isolate-Mode", "ISOLATE"),
            (Modifier::SetDefault, "Switch-Large-Tight-Kerning", "SET DEFAULT"),
        ];
        for (i, (m, key, label)) in mods.into_iter().enumerate() {
            let r = Rect::new(8.0, ops + 84.0 + i as f32 * 40.0, 80.0, 40.0);
            ui.mode_switch(key, label, view.modifier == Some(m), r, nid(Control::Modifier(m)));
        }
        ui.fill(Rect::new(7.0, ops + 286.0, WIDTH - 14.0, 1.0), DIVIDER);

        // SHOW.
        label(ui, "SHOW", Rect::new(4.0, L::SHOW_Y, WIDTH - 8.0, 16.0), Align::Center);
        let scenes_open = matches!(&view.popover, Some(p) if p.anchor.control == Control::ShowPluginScenes);
        let shows = [
            ("BROWSER", view.show_browser, Control::ShowBrowser),
            ("PLUG-IN\nSCENES", scenes_open, Control::ShowPluginScenes),
            ("SETTINGS", view.sheet.is_some(), Control::ShowSettings),
        ];
        for (i, (label, on, c)) in shows.into_iter().enumerate() {
            let r = Rect::new(8.0, L::SHOW_Y + 16.0 + i as f32 * 40.0, 80.0, 40.0);
            ui.toggle("Switch-Large", Some(label), on, true, r, None, None);
            ui.hit(r, nid(c), HitKind::Toggle);
        }
    });
}
