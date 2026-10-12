//! Side navigator column (x 345…441 in the full window): session box, `Rack_MixerNavigator`
//! section rows (INPUTS / INSERTS / SENDS / FADER) with OPEN / CLOSE, LARGE / SMALL,
//! FIXED SLOTS, SENDS OVERVIEW; `Rack_InsertOperations` (INSERT EFFECTS UAD REC / MON,
//! MODIFIERS) and SHOW (BROWSER, PLUG-IN SCENES, SETTINGS). Positions measured from
//! console-main-5.png (1.3.1).

use houston::{Rect, Rgba};

use crate::draw::Align;
use crate::input::{Control, ControlId, HitKind};
use crate::model::MixerModel;
use crate::surfaces::view::{InsertSize, Modifier, Section, ViewState};
use crate::ui::Ui;

pub const NAV_ID: &str = "nav";
pub const WIDTH: f32 = 96.0;
const SECTION_ON: Rgba = Rgba::rgb8(212, 146, 5);
const SECTION_OFF: Rgba = Rgba::rgb8(92, 100, 105);
const BRACKET: Rgba = Rgba::rgb8(184, 138, 40);

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
        let mut style = self.style("views", key);
        style.color = (if on { skin.color("color-text-on") } else { skin.color("color-text-off") }).unwrap_or(Rgba::WHITE);
        match overlay {
            Some(ov) => {
                let b = skin.rect("bounds-overlay").unwrap_or(Rect::new(9.0, 13.0, 12.0, 12.0));
                let alpha = skin.number("opacity-overlay").unwrap_or(1.0) as f32;
                let ov = ov.to_string();
                self.image(&ov, Rect::new(r.x + b.x, r.y + b.y, b.w, b.h), None, None, alpha.max(0.6));
                self.text(label, style, Rect::new(r.x - 4.0, r.y + 1.0, r.w + 8.0, 12.0), Align::Center);
            }
            None => self.text(label, style, r, Align::Center),
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

/// Draws the navigator column at `x` (window points), from the title bar to the status bar.
pub fn draw_navigator(ui: &mut Ui, model: &dyn MixerModel, view: &ViewState, x: f32, top: f32, bottom: f32, rows: VisibleRows) {
    ui.at(x, 0.0, |ui| {
        ui.fill(Rect::new(0.0, top, WIDTH, bottom - top), Rgba::rgb8(44, 46, 49));
        ui.background("Monitor-Nav", Rect::new(0.0, 341.0, WIDTH, bottom - 341.0));
        ui.fill(Rect::new(WIDTH - 1.0, top, 1.0, bottom - top), Rgba::rgb8(26, 27, 29));

        // Session box.
        ui.background("Display-Background", Rect::new(6.0, 31.0, 84.0, 30.0));
        ui.font_text("CR-Label", "SESSION", Rect::new(6.0, 33.0, 84.0, 12.0), Align::Center);
        let name = Ui::plain_style("Helvetica Neue", 11.0, true, Rgba::WHITE.with_alpha(0.8));
        ui.clip(Some(Rect::new(8.0, 31.0, 80.0, 30.0)));
        ui.text(&model.session_name(), name, Rect::new(6.0, 45.0, 84.0, 14.0), Align::Center);
        ui.clip(None);

        // Section rows on the Display-Nav plate.
        ui.background("Display-Nav", Rect::new(6.0, 73.0, 84.0, 268.0));
        let rows_y = [(Section::Inputs, "INPUTS", 83.0), (Section::Inserts, "INSERTS", 108.0), (Section::Sends, "SENDS", 133.0), (Section::Fader, "FADER", 162.0)];
        let mut style = ui.style("views", "Display-Select-Console");
        style.size = 11.0;
        for (s, label, y) in rows_y {
            let on = view.sections.get(s);
            let st = crate::draw::TextStyle { color: if on { SECTION_ON } else { SECTION_OFF }, ..style.clone() };
            ui.text(label, st, Rect::new(14.0, y, 56.0, 21.0), Align::Left);
            let dot = Rect::new(69.0, y + 5.5, 10.0, 10.0);
            ui.fill(dot.inset(1.0, 0.0, 1.0, 0.0), if on { SECTION_ON } else { Rgba::rgb8(150, 155, 158) });
            ui.fill(dot.inset(0.0, 1.0, 0.0, 1.0), if on { SECTION_ON } else { Rgba::rgb8(150, 155, 158) });
            ui.hit(Rect::new(8.0, y, 80.0, 23.0), nid(Control::Section(s)), HitKind::Toggle);
        }
        // Bracket around the rows visible in the cells viewport.
        let vis = [(rows.inputs, 83.0), (rows.inserts, 108.0), (rows.sends, 133.0)];
        let first = vis.iter().find(|(v, _)| *v).map(|(_, y)| *y);
        let last = vis.iter().rev().find(|(v, _)| *v).map(|(_, y)| *y + 23.0);
        if let (Some(a), Some(b)) = (first, last) {
            let (bx, bw) = (9.0, 78.0);
            for y in [a - 3.0, b + 1.0] {
                ui.fill(Rect::new(bx, y, bw, 1.0), BRACKET);
            }
            for xx in [bx, bx + bw - 1.0] {
                ui.fill(Rect::new(xx, a - 3.0, 1.0, 4.0), BRACKET);
                ui.fill(Rect::new(xx, b - 2.0, 1.0, 4.0), BRACKET);
            }
        }

        // OPEN / CLOSE, LARGE / SMALL.
        let all_open = view.sections.inputs && view.sections.inserts && view.sections.sends;
        let all_closed = !view.sections.inputs && !view.sections.inserts && !view.sections.sends;
        ui.nav_button("Console-Nav-Row-Button-Open", "OPEN", all_open, Rect::new(14.0, 196.0, 30.0, 28.0), nid(Control::SectionsOpenAll), HitKind::Tap);
        ui.nav_button("Console-Nav-Row-Button-Close", "CLOSE", all_closed, Rect::new(52.0, 196.0, 30.0, 28.0), nid(Control::SectionsCloseAll), HitKind::Tap);
        let large = view.insert_size == InsertSize::Large;
        ui.nav_button("Console-Nav-Row-Button-Large", "LARGE", large, Rect::new(14.0, 232.0, 30.0, 28.0), nid(Control::InsertSize(InsertSize::Large)), HitKind::Tap);
        ui.nav_button("Console-Nav-Row-Button-Small", "SMALL", !large, Rect::new(52.0, 232.0, 30.0, 28.0), nid(Control::InsertSize(InsertSize::Small)), HitKind::Tap);
        ui.nav_button("Console-Nav-Row-Button-Fixed-Slots", "FIXED SLOTS", view.fixed_slots, Rect::new(13.0, 269.0, 70.0, 28.0), nid(Control::FixedSlots), HitKind::Toggle);
        ui.nav_button("Console-Nav-Row-Button-Fixed-Slots", "SENDS\nOVERVIEW", view.sends_overview, Rect::new(13.0, 305.0, 70.0, 28.0), nid(Control::SendsOverview), HitKind::Toggle);

        // INSERT EFFECTS: global UAD REC / MON (lit when any input is in that mode).
        let inputs = model.inputs();
        let pre: Vec<bool> = inputs.iter().filter(|s| s.kind == crate::geometry::StripKind::Input).map(|s| model.records_pre_effects(s)).collect();
        ui.font_text("CR-Label", "INSERT EFFECTS", Rect::new(0.0, 350.0, WIDTH, 16.0), Align::Center);
        let (rec_on, mon_on) = (pre.iter().any(|p| !p), pre.iter().any(|p| *p));
        ui.toggle("Switch", Some("UAD\nREC"), rec_on, true, Rect::new(8.0, 366.0, 40.0, 40.0), None, None);
        ui.hit(Rect::new(8.0, 366.0, 40.0, 40.0), nid(Control::AllRecordPreEffects(false)), HitKind::Tap);
        ui.toggle("Switch", Some("UAD\nMON"), mon_on, true, Rect::new(48.0, 366.0, 40.0, 40.0), None, None);
        ui.hit(Rect::new(48.0, 366.0, 40.0, 40.0), nid(Control::AllRecordPreEffects(true)), HitKind::Tap);
        ui.fill(Rect::new(6.0, 410.0, 84.0, 1.0), Rgba::rgb8(64, 68, 71));

        // MODIFIERS.
        ui.font_text("CR-Label", "MODIFIERS", Rect::new(0.0, 415.0, WIDTH, 16.0), Align::Center);
        let mods = [
            (Modifier::Power, "Switch-Power-Mode", "POWER"),
            (Modifier::Remove, "Switch-Remove-Mode", "REMOVE"),
            (Modifier::Copy, "Switch-Copy-Mode", "COPY"),
            (Modifier::Isolate, "Switch-Isolate-Mode", "ISOLATE"),
            (Modifier::SetDefault, "Switch-Large-Tight-Kerning", "SET DEFAULT"),
        ];
        for (i, (m, key, label)) in mods.into_iter().enumerate() {
            let r = Rect::new(8.0, 431.0 + i as f32 * 40.0, 80.0, 40.0);
            ui.mode_switch(key, label, view.modifier == Some(m), r, nid(Control::Modifier(m)));
        }
        ui.fill(Rect::new(6.0, 636.0, 84.0, 1.0), Rgba::rgb8(64, 68, 71));

        // SHOW.
        ui.font_text("CR-Label", "SHOW", Rect::new(0.0, 640.0, WIDTH, 16.0), Align::Center);
        let scenes_open = matches!(&view.popover, Some(p) if p.anchor.control == Control::ShowPluginScenes);
        let shows = [
            ("BROWSER", view.show_browser, Control::ShowBrowser),
            ("PLUG-IN\nSCENES", scenes_open, Control::ShowPluginScenes),
            ("SETTINGS", view.sheet.is_some(), Control::ShowSettings),
        ];
        for (i, (label, on, c)) in shows.into_iter().enumerate() {
            let r = Rect::new(8.0, 656.0 + i as f32 * 40.0, 80.0, 40.0);
            ui.toggle("Switch-Large", Some(label), on, true, r, None, None);
            ui.hit(r, nid(c), HitKind::Toggle);
        }
    });
}
