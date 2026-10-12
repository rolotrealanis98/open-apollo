//! Generic popover / menu primitive (anchored panel with an arrow, a list of items with hit
//! regions) and the popovers built on it. Drawn last in the frame so they sit on top; a
//! full-window `PopoverDismiss` hit underneath catches taps outside.

use std::sync::Arc;

use houston::{Rect, Rgba};

use crate::draw::Align;
use crate::input::{Control, ControlId, HitKind};
use crate::model::{InsertState, MixerModel};
use crate::surfaces::view::{Popover, PopoverState, TextEdit, ViewState};
use crate::ui::Ui;

pub const POPOVER_ID: &str = "popover";
const ARROW: f32 = 7.0;
const TITLE_H: f32 = 24.0;
const ROW_H: f32 = 22.0;
const BODY: Rgba = Rgba::rgb8(47, 50, 53);
const EDGE: Rgba = Rgba::rgb8(16, 17, 18);
const AMBER: Rgba = Rgba::rgb8(255, 197, 74);

/// Which side of the anchor the popover opens on (flipped when it does not fit).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    Below,
    Above,
    Left,
    Right,
}

/// One menu row. Items without an id (or disabled) draw but have no hit region.
#[derive(Clone, Debug, PartialEq)]
pub struct MenuItem {
    pub label: String,
    pub checked: bool,
    pub enabled: bool,
    pub id: Option<ControlId>,
    /// Draw a divider above this row.
    pub separator: bool,
}

impl MenuItem {
    pub fn new(label: impl Into<String>, id: ControlId) -> Self {
        Self { label: label.into(), checked: false, enabled: true, id: Some(id), separator: false }
    }
    pub fn checked(mut self, on: bool) -> Self {
        self.checked = on;
        self
    }
    pub fn enabled(mut self, on: bool) -> Self {
        self.enabled = on;
        self
    }
    pub fn separated(mut self) -> Self {
        self.separator = true;
        self
    }
}

pub(crate) fn pid(control: Control) -> ControlId {
    ControlId { strip: POPOVER_ID.into(), control }
}

/// Where a popover of `size` goes against `anchor`, kept inside `bounds`.
pub fn place(anchor: Rect, size: (f32, f32), placement: Placement, bounds: Rect) -> (Rect, Placement) {
    let (w, h) = size;
    let fits = |r: Rect| r.x >= bounds.x && r.y >= bounds.y && r.max_x() <= bounds.max_x() && r.max_y() <= bounds.max_y();
    let (cx, cy) = anchor.center();
    let at = |p: Placement| match p {
        Placement::Below => Rect::new(cx - w / 2.0, anchor.max_y() + ARROW, w, h),
        Placement::Above => Rect::new(cx - w / 2.0, anchor.y - ARROW - h, w, h),
        Placement::Left => Rect::new(anchor.x - ARROW - w, cy - h / 2.0, w, h),
        Placement::Right => Rect::new(anchor.max_x() + ARROW, cy - h / 2.0, w, h),
    };
    let flipped = match placement {
        Placement::Below => Placement::Above,
        Placement::Above => Placement::Below,
        Placement::Left => Placement::Right,
        Placement::Right => Placement::Left,
    };
    let mut p = placement;
    let mut r = at(p);
    if !fits(r) && fits(at(flipped)) {
        p = flipped;
        r = at(p);
    }
    let x = r.x.clamp(bounds.x + 2.0, (bounds.max_x() - w - 2.0).max(bounds.x + 2.0));
    let y = r.y.clamp(bounds.y + 2.0, (bounds.max_y() - h - 2.0).max(bounds.y + 2.0));
    (Rect::new(x, y, w, h), p)
}

impl Ui<'_> {
    /// Full-`bounds` hit that dismisses the open popover; register before the popover body.
    pub fn popover_catcher(&mut self, bounds: Rect) {
        self.hit(bounds, pid(Control::PopoverDismiss), HitKind::Tap);
    }

    /// Popover chrome (shadow, body, arrow, optional title band). Returns the content rect.
    pub fn popover_frame(&mut self, anchor: Rect, size: (f32, f32), placement: Placement, bounds: Rect, title: Option<&str>) -> Rect {
        let h = size.1 + if title.is_some() { TITLE_H } else { 0.0 };
        let (r, p) = place(anchor, (size.0, h), placement, bounds);
        self.fill(r.offset(2.0, 3.0), Rgba::BLACK.with_alpha(0.35));
        self.fill(r, EDGE);
        self.fill(r.inset(1.0, 1.0, 1.0, 1.0), BODY);
        // Arrow: stacked 1-pt bars pointing at the anchor.
        let (ax, ay) = anchor.center();
        for i in 0..ARROW as i32 {
            let half = (ARROW - i as f32 - 1.0).max(0.5);
            let d = i as f32;
            let bar = match p {
                Placement::Below => Rect::new(ax.clamp(r.x + 8.0, r.max_x() - 8.0) - half, r.y - d - 1.0, half * 2.0, 1.0),
                Placement::Above => Rect::new(ax.clamp(r.x + 8.0, r.max_x() - 8.0) - half, r.max_y() + d, half * 2.0, 1.0),
                Placement::Left => Rect::new(r.max_x() + d, ay.clamp(r.y + 8.0, r.max_y() - 8.0) - half, 1.0, half * 2.0),
                Placement::Right => Rect::new(r.x - d - 1.0, ay.clamp(r.y + 8.0, r.max_y() - 8.0) - half, 1.0, half * 2.0),
            };
            self.fill(bar, if i == 0 { BODY } else { EDGE.with_alpha(0.9) });
        }
        // Swallow taps on the body so they do not dismiss.
        self.hit(r, pid(Control::PopoverDismiss), HitKind::Drag { value: 0.0, span: 1.0, reset: None });
        let mut content = r.inset(1.0, 1.0, 1.0, 1.0);
        if let Some(t) = title {
            let band = Rect::new(content.x, content.y, content.w, TITLE_H);
            self.fill(band, Rgba::rgb8(36, 38, 41));
            if let Some(img) = self.skin("backgrounds", "Popover-Header").and_then(|s| s.image("image")) {
                let img = img.to_string();
                self.image_stretched(&img, band, 1.0);
            }
            let mut style = self.style("views", "Popover-Header");
            style.size = 11.0;
            self.text(t, style, band, Align::Center);
            content = Rect::new(content.x, content.y + TITLE_H, content.w, content.h - TITLE_H);
        }
        content
    }

    /// A popover menu: one 22-pt row per item, amber marker on checked rows. Returns the frame.
    pub fn popover_menu(&mut self, anchor: Rect, placement: Placement, bounds: Rect, title: Option<&str>, items: &[MenuItem], width: f32) -> Rect {
        let content = self.popover_frame(anchor, (width, items.len() as f32 * ROW_H + 6.0), placement, bounds, title);
        let base = Ui::plain_style("Helvetica", 11.0, true, Rgba::WHITE.with_alpha(0.85));
        for (i, item) in items.iter().enumerate() {
            let row = Rect::new(content.x, content.y + 3.0 + i as f32 * ROW_H, content.w, ROW_H);
            if item.separator {
                self.fill(Rect::new(row.x + 6.0, row.y, row.w - 12.0, 1.0), Rgba::WHITE.with_alpha(0.1));
            }
            if item.checked {
                self.fill(row.inset(3.0, 1.0, 3.0, 1.0), Rgba::rgb8(64, 58, 40));
                self.fill(Rect::new(row.x + 9.0, row.y + 8.0, 6.0, 6.0), AMBER);
            }
            let mut style = base.clone();
            if item.checked {
                style.color = AMBER;
            }
            if !item.enabled {
                style.color = style.color.with_alpha(0.3);
            }
            self.text(&item.label, style, Rect::new(row.x + 22.0, row.y, row.w - 28.0, row.h), Align::Left);
            if let (true, Some(id)) = (item.enabled, &item.id) {
                self.hit(row, id.clone(), HitKind::Tap);
            }
        }
        content
    }

    /// A one-line text field with a caret (`Text-Edit-Field`).
    pub fn text_field(&mut self, key: &str, text: &str, focused: bool, r: Rect) {
        self.fill(r, Rgba::rgb8(20, 21, 22));
        if let Some(img) = self.skin("views", key).and_then(|s| s.image("image")) {
            let img = img.to_string();
            self.image_stretched(&img, r, 1.0);
        }
        let style = Ui::plain_style("Helvetica Neue", 12.0, false, Rgba::WHITE.with_alpha(0.85));
        let shown = if focused { format!("{text}|") } else { text.to_string() };
        self.clip(Some(r.inset(6.0, 0.0, 6.0, 0.0)));
        self.text(&shown, style, r.inset(8.0, 0.0, 8.0, 0.0), Align::Left);
        self.clip(None);
    }

    /// Flat text button for popovers and sheets.
    pub fn flat_button(&mut self, label: &str, primary: bool, r: Rect, id: ControlId) {
        self.fill(r, EDGE);
        self.fill(r.inset(1.0, 1.0, 1.0, 1.0), if primary { Rgba::rgb8(150, 108, 22) } else { Rgba::rgb8(72, 76, 79) });
        let style = Ui::plain_style("Helvetica", 10.0, true, Rgba::WHITE.with_alpha(0.9));
        self.text(label, style, r, Align::Center);
        self.hit(r, id, HitKind::Tap);
    }
}

fn sid(strip: &Arc<str>, control: Control) -> ControlId {
    ControlId { strip: strip.clone(), control }
}

fn gid(control: Control) -> ControlId {
    ControlId { strip: "global".into(), control }
}

fn rate_label(hz: u32) -> String {
    if hz % 1000 == 0 { format!("{} kHz", hz / 1000) } else { format!("{:.1} kHz", hz as f64 / 1000.0) }
}

pub(crate) fn sample_rate_label(hz: Option<u32>) -> String {
    hz.map(rate_label).unwrap_or_else(|| "OFFLINE".into())
}

/// Draws the open popover (if any) on top of the frame, anchored to this frame's hit rect of
/// its anchor control (or the window centre when the anchor is not visible).
pub fn draw_popover(ui: &mut Ui, model: &dyn MixerModel, view: &ViewState, bounds: Rect) {
    let Some(PopoverState { kind, anchor }) = &view.popover else { return };
    let a = ui.hit_rect(anchor).unwrap_or_else(|| {
        let (cx, cy) = bounds.center();
        Rect::new(cx - 1.0, cy - 1.0, 2.0, 2.0)
    });
    ui.popover_catcher(bounds);
    let low = a.y > bounds.h * 0.75;
    match kind {
        Popover::InputSource { strip } => {
            let s = model.inputs().into_iter().find(|s| &s.id == strip);
            let current = s.as_ref().and_then(|s| model.preamp(s)).map(|p| p.source).unwrap_or_default();
            let sources = s.as_ref().map(|s| model.input_sources(s)).unwrap_or_default();
            let items: Vec<MenuItem> =
                sources.iter().enumerate().map(|(i, src)| MenuItem::new(src.clone(), sid(strip, Control::InputSource(i as u8))).checked(*src == current)).collect();
            ui.popover_menu(a, Placement::Below, bounds, Some("INPUT"), &items, 120.0);
        }
        Popover::InsertMenu { strip, slot } => {
            let s = model.inputs().into_iter().chain(model.auxes()).find(|s| &s.id == strip);
            let state = s.as_ref().map(|s| if *slot == 255 { model.unison(s) } else { model.inserts(s).get(*slot as usize).cloned().unwrap_or(InsertState::Empty) });
            let (title, loaded, power) = match &state {
                Some(InsertState::Loaded { name, power }) => (name.strip_prefix("UAD ").unwrap_or(name).to_string(), true, *power),
                _ => ("EMPTY SLOT".to_string(), false, false),
            };
            let k = *slot;
            let items = vec![
                MenuItem::new("Open", sid(strip, Control::Insert(k))).enabled(loaded),
                MenuItem::new("Power", sid(strip, Control::InsertPower(k))).enabled(loaded).checked(power).separated(),
                MenuItem::new("Remove", sid(strip, Control::InsertRemove(k))).enabled(loaded),
                MenuItem::new(if loaded { "Copy" } else { "Paste" }, sid(strip, Control::InsertCopy(k))),
            ];
            ui.popover_menu(a, Placement::Right, bounds, Some(&title), &items, 150.0);
        }
        Popover::Rename { .. } => text_popover(ui, a, bounds, "CHANNEL NAME", view.text_edit.as_ref()),
        Popover::Session | Popover::SaveChanges { .. } => {
            crate::surfaces::session::draw_session_popover(ui, model, kind, a, bounds);
        }
        Popover::SessionSaveAs => text_popover(ui, a, bounds, "SAVE SESSION AS", view.text_edit.as_ref()),
        Popover::SessionOpen => text_popover(ui, a, bounds, "OPEN SESSION", view.text_edit.as_ref()),
        Popover::Tempo => text_popover(ui, a, bounds, "TEMPO", view.text_edit.as_ref()),
        Popover::CueOutputs => cue_outputs_popover(ui, model, a, bounds),
        Popover::Headphone { output } => {
            let hps = model.headphone_outputs();
            let Some(hp) = hps.get(*output as usize) else { return };
            let items: Vec<MenuItem> = hp
                .options
                .iter()
                .enumerate()
                .map(|(i, o)| {
                    MenuItem::new(o.clone(), ControlId { strip: "monitor".into(), control: Control::HeadphoneSource { output: *output, option: i as u8 } }).checked(i == hp.source)
                })
                .collect();
            ui.popover_menu(a, Placement::Left, bounds, Some(&hp.name), &items, 120.0);
        }
        Popover::SampleRate => {
            let st = model.engine_settings();
            let items: Vec<MenuItem> = if st.sample_rates.is_empty() {
                vec![MenuItem::new("Not connected", gid(Control::SampleRate(0))).enabled(false)]
            } else {
                st.sample_rates.iter().enumerate().map(|(i, hz)| MenuItem::new(rate_label(*hz), gid(Control::SampleRate(i as u8))).checked(Some(*hz) == st.sample_rate)).collect()
            };
            ui.popover_menu(a, if low { Placement::Above } else { Placement::Below }, bounds, Some("SAMPLE RATE"), &items, 140.0);
        }
        Popover::ClockSource => {
            let st = model.engine_settings();
            let items: Vec<MenuItem> = if st.clock_sources.is_empty() {
                vec![MenuItem::new("Not connected", gid(Control::ClockSource(0))).enabled(false)]
            } else {
                st.clock_sources.iter().enumerate().map(|(i, c)| MenuItem::new(c.clone(), gid(Control::ClockSource(i as u8))).checked(Some(c) == st.clock_source.as_ref())).collect()
            };
            ui.popover_menu(a, if low { Placement::Above } else { Placement::Below }, bounds, Some("CLOCK SOURCE"), &items, 150.0);
        }
        Popover::BufferSize => {
            let st = model.engine_settings();
            let items: Vec<MenuItem> = if st.buffer_sizes.is_empty() {
                vec![MenuItem::new("Not available", gid(Control::BufferSize(0))).enabled(false)]
            } else {
                st.buffer_sizes.iter().enumerate().map(|(i, b)| MenuItem::new(format!("{b} samples"), gid(Control::BufferSize(i as u8))).checked(Some(*b) == st.buffer_size)).collect()
            };
            ui.popover_menu(a, Placement::Below, bounds, Some("BUFFER SIZE"), &items, 140.0);
        }
        Popover::Scenes => {
            let scenes = model.plugin_scenes();
            let current = model.status().scene;
            let items: Vec<MenuItem> = if scenes.is_empty() {
                vec![MenuItem::new("No scenes", gid(Control::RecallScene(0))).enabled(false)]
            } else {
                scenes.iter().enumerate().map(|(i, n)| MenuItem::new(format!("{}  {n}", i + 1), gid(Control::RecallScene(i as u8))).checked(Some(n) == current.as_ref())).collect()
            };
            ui.popover_menu(a, if low { Placement::Above } else { Placement::Right }, bounds, Some("PLUG-IN SCENES"), &items, 170.0);
        }
    }
}

fn text_popover(ui: &mut Ui, anchor: Rect, bounds: Rect, title: &str, edit: Option<&TextEdit>) {
    let low = anchor.y > bounds.h * 0.75;
    let c = ui.popover_frame(anchor, (200.0, 72.0), if low { Placement::Above } else { Placement::Below }, bounds, Some(title));
    ui.text_field("Text-Edit-Field", edit.map(|e| e.text.as_str()).unwrap_or(""), true, Rect::new(c.x + 8.0, c.y + 8.0, c.w - 16.0, 26.0));
    ui.flat_button("CANCEL", false, Rect::new(c.x + c.w - 136.0, c.y + 42.0, 62.0, 22.0), pid(Control::TextCancel));
    ui.flat_button("OK", true, Rect::new(c.x + c.w - 70.0, c.y + 42.0, 62.0, 22.0), pid(Control::TextCommit));
}

fn cue_outputs_popover(ui: &mut Ui, model: &dyn MixerModel, anchor: Rect, bounds: Rect) {
    let cues = model.cue_outputs();
    let row_h = 34.0;
    let h = (cues.len().max(1) as f32) * row_h + 8.0;
    let c = ui.popover_frame(anchor, (268.0, h), Placement::Left, bounds, Some("CUE OUTPUTS"));
    if cues.is_empty() {
        let style = Ui::plain_style("Helvetica", 11.0, false, Rgba::WHITE.with_alpha(0.5));
        ui.text("No cue outputs", style, Rect::new(c.x, c.y + 4.0, c.w, row_h), Align::Center);
        return;
    }
    let name = Ui::plain_style("Helvetica", 11.0, true, Rgba::WHITE.with_alpha(0.85));
    let dest = Ui::plain_style("Helvetica", 9.0, false, Rgba::WHITE.with_alpha(0.45));
    let mid = |c: Control| Some(ControlId { strip: "monitor".into(), control: c });
    for (i, cue) in cues.iter().enumerate() {
        let y = c.y + 4.0 + i as f32 * row_h;
        if i > 0 {
            ui.fill(Rect::new(c.x + 6.0, y, c.w - 12.0, 1.0), Rgba::WHITE.with_alpha(0.08));
        }
        ui.text(&cue.name, name.clone(), Rect::new(c.x + 10.0, y + 4.0, 80.0, 14.0), Align::Left);
        ui.text(&cue.destination, dest.clone(), Rect::new(c.x + 10.0, y + 18.0, 80.0, 12.0), Align::Left);
        let k = i as u8;
        // Source pair: only the unselected side is tappable (it switches the source).
        ui.toggle("Switch-Small", Some("CUE"), cue.source_cue, true, Rect::new(c.x + 96.0, y + 7.0, 40.0, 19.0), if cue.source_cue { None } else { mid(Control::CueSource(k)) }, None);
        ui.toggle("Switch-Small", Some("MON"), !cue.source_cue, true, Rect::new(c.x + 138.0, y + 7.0, 40.0, 19.0), if cue.source_cue { mid(Control::CueSource(k)) } else { None }, None);
        ui.toggle("Switch-Small", Some("MONO"), cue.mono, true, Rect::new(c.x + 196.0, y + 7.0, 48.0, 19.0), mid(Control::CueMono(k)), None);
    }
}
