//! Generic popover / menu primitive (anchored panel with an arrow, a list of items with hit
//! regions) and the popovers built on it. Drawn last in the frame so they sit on top; a
//! full-window `PopoverDismiss` hit underneath catches taps outside.


use houston::{Rect, Rgba};

use crate::draw::Align;
use crate::input::{Control, ControlId, HitKind};
use crate::menu::{MenuLook, PopupItem, PopupMenu, layout_menu};
use crate::model::MixerModel;
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
        self.chrome_layer(r, 0.0);
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
            // The same SOURCE menu as a right click (capture preamp-mic), under the button.
            anchored_menu(ui, &crate::menu::input_source_menu(model, strip), MenuLook::Context, a, bounds);
        }
        Popover::InsertMenu { strip, slot } => {
            let m = crate::menu::insert_menu(model, strip, *slot);
            anchored_menu(ui, &m, MenuLook::Context, a, bounds);
        }
        Popover::Rename { .. } => text_popover(ui, a, bounds, "CHANNEL NAME", view.text_edit.as_ref()),
        Popover::Session | Popover::SaveChanges { .. } => {
            crate::surfaces::session::draw_session_popover(ui, model, kind, a, bounds);
        }
        Popover::SessionSaveAs => text_popover(ui, a, bounds, "SAVE SESSION AS", view.text_edit.as_ref()),
        Popover::SessionOpen => text_popover(ui, a, bounds, "OPEN SESSION", view.text_edit.as_ref()),
        Popover::SessionRename => text_popover(ui, a, bounds, "RENAME SESSION", view.text_edit.as_ref()),
        Popover::SessionNewFolder => text_popover(ui, a, bounds, "NEW FOLDER", view.text_edit.as_ref()),
        Popover::Tempo => text_popover(ui, a, bounds, "TEMPO", view.text_edit.as_ref()),
        Popover::Value { target } => {
            let a = ui.hit_rect(anchor).or_else(|| ui.hit_rect(target)).unwrap_or(a);
            crate::surfaces::panels::value_popover(ui, view, a, bounds);
        }
        Popover::Confirm { title, message, ok, ok_label } => confirm_dialog(ui, title, message, ok, ok_label, bounds),
        Popover::CueOutputs => cue_outputs_popover(ui, model, a, bounds),
        Popover::Headphone { output } => {
            let hps = model.headphone_outputs();
            let Some(hp) = hps.get(*output as usize) else { return };
            let mut m = PopupMenu::new();
            for (i, o) in hp.options.iter().enumerate() {
                m.push(PopupItem::new(o.clone(), ControlId { strip: "monitor".into(), control: Control::HeadphoneSource { output: *output, option: i as u8 } }).ticked(i == hp.source));
            }
            anchored_menu(ui, &m, MenuLook::Base, a, bounds);
        }
        Popover::SampleRate | Popover::ClockSource | Popover::BufferSize => {
            status_field_open(ui, a);
            status_menu(ui, model, kind, a, bounds);
        }
        Popover::Scenes => scenes_popover(ui, model, a, bounds, low),
    }
}

/// An open status-bar field: a 1-pt amber outline 0.5 pt in from its left, 2.5 pt from its
/// right, 0.5 pt from its bottom (sb-rate / sb-clock: (175,127,36) from x 155.5 to 292.5 and
/// y 1074.5 to 1095.5 on the (155, 1074.5, 140, 21.5) field).
pub fn status_field_open(ui: &mut Ui, a: Rect) {
    let o = Rect::new(a.x + 0.5, a.y, a.w - 3.0, a.h - 0.5);
    // The open field's amber outline (sb-rate / sb-clock, in sRGB).
    let amber = Rgba::rgb8(184, 138, 40);
    ui.fill(Rect::new(o.x, o.y, o.w, 1.0), amber);
    ui.fill(Rect::new(o.x, o.max_y() - 1.0, o.w, 1.0), amber);
    ui.fill(Rect::new(o.x, o.y + 1.0, 1.0, o.h - 2.0), amber);
    ui.fill(Rect::new(o.max_x() - 1.0, o.y + 1.0, 1.0, o.h - 2.0), amber);
}

/// The status bar's RATE / CLOCK / buffer menus (`Rack_*Info`): the choices, the current one
/// ticked, under the field at its width.
fn status_menu(ui: &mut Ui, model: &dyn MixerModel, kind: &Popover, a: Rect, bounds: Rect) {
    let m = match kind {
        Popover::SampleRate => sample_rate_menu(model),
        Popover::ClockSource => clock_source_menu(model),
        Popover::BufferSize => buffer_size_menu(model),
        _ => return,
    };
    anchored_menu(ui, &m, MenuLook::Base, a, bounds);
}

/// RATE: the engine's rates, the current one ticked (sb-rate).
pub fn sample_rate_menu(model: &dyn MixerModel) -> PopupMenu {
    let st = model.engine_settings();
    let mut m = PopupMenu::new();
    if st.sample_rates.is_empty() {
        m.push(PopupItem::new("OFFLINE", gid(Control::SampleRate(0))).enabled(false));
    }
    for (i, hz) in st.sample_rates.iter().enumerate() {
        m.push(PopupItem::new(rate_label(*hz), gid(Control::SampleRate(i as u8))).ticked(Some(*hz) == st.sample_rate));
    }
    m
}

/// CLOCK (`Rack_ClockInfo::ShowMenu` 0x181546830): the sources in capitals (s03, sb-clock).
pub fn clock_source_menu(model: &dyn MixerModel) -> PopupMenu {
    let st = model.engine_settings();
    let mut m = PopupMenu::new();
    if st.clock_sources.is_empty() {
        m.push(PopupItem::new("OFFLINE", gid(Control::ClockSource(0))).enabled(false));
    }
    for (i, c) in st.clock_sources.iter().enumerate() {
        m.push(PopupItem::new(c.to_uppercase(), gid(Control::ClockSource(i as u8))).ticked(Some(c) == st.clock_source.as_ref()));
    }
    m
}

/// Buffer size (`Rack_BufferSizeInfo::BuildBufferSizeMenu` 0x18153f070; Windows adds "Open
/// ASIO Control Panel").
pub fn buffer_size_menu(model: &dyn MixerModel) -> PopupMenu {
    let st = model.engine_settings();
    let mut m = PopupMenu::new();
    if st.buffer_sizes.is_empty() {
        m.push(PopupItem::new("Not available", gid(Control::BufferSize(0))).enabled(false));
    }
    for (i, b) in st.buffer_sizes.iter().enumerate() {
        m.push(PopupItem::new(format!("{b}"), gid(Control::BufferSize(i as u8))).ticked(Some(*b) == st.buffer_size));
    }
    m
}

fn scenes_popover(ui: &mut Ui, model: &dyn MixerModel, a: Rect, bounds: Rect, low: bool) {
    let scenes = model.plugin_scenes();
    let current = model.status().scene;
    let items: Vec<MenuItem> = if scenes.is_empty() {
        vec![MenuItem::new("No scenes", gid(Control::RecallScene(0))).enabled(false)]
    } else {
        scenes.iter().enumerate().map(|(i, n)| MenuItem::new(format!("{}  {n}", i + 1), gid(Control::RecallScene(i as u8))).checked(Some(n) == current.as_ref())).collect()
    };
    ui.popover_menu(a, if low { Placement::Above } else { Placement::Right }, bounds, Some("PLUG-IN SCENES"), &items, 170.0);
}

/// A Console popup menu for a combo-like control: below `anchor` (at least its width), above it
/// when there is no room below (JUCE `showMenuAsync` with a target component).
pub fn anchored_menu(ui: &mut Ui, menu: &PopupMenu, look: MenuLook, anchor: Rect, bounds: Rect) {
    if menu.is_empty() {
        return;
    }
    let probe = layout_menu(ui, menu, look, (anchor.x, anchor.max_y()), anchor.w, bounds);
    let h = probe.frame.h;
    let y = if anchor.max_y() + h <= bounds.max_y() { anchor.max_y() } else { (anchor.y - h).max(bounds.y) };
    let f = layout_menu(ui, menu, look, (anchor.x, y), anchor.w, bounds);
    ui.begin_layer("menu:anchored", crate::layers::LayerKind::Menu, f.frame, 0.0);
    ui.popup_menu(menu, look, &f, 0, None, None);
    ui.end_layer();
}

/// Console's confirmation (`Rack_Window` 0x180a40730 → 0x180a3fa40): a `Popover-Header` title,
/// the message, CANCEL and a primed OK, centred in the window. Sized like the SAVE SESSION
/// question (no capture of this one yet).
fn confirm_dialog(ui: &mut Ui, title: &str, message: &str, ok: &ControlId, ok_label: &str, bounds: Rect) {
    let (w, h) = (360.0, 118.0);
    let (cx, cy) = bounds.center();
    let r = Rect::new((cx - w / 2.0).round(), (cy - h / 2.0).round(), w, h);
    ui.fill(r.offset(2.0, 3.0), Rgba::BLACK.with_alpha(0.35));
    ui.fill(r, Rgba::rgb8(16, 17, 18));
    ui.fill(r.inset(1.0, 1.0, 1.0, 1.0), Rgba::rgb8(47, 50, 53));
    ui.hit(r, pid(Control::PopoverDismiss), HitKind::Drag { value: 0.0, span: 1.0, reset: None });
    let band = Rect::new(r.x + 1.0, r.y + 1.0, r.w - 2.0, 24.0);
    ui.fill(band, Rgba::rgb8(36, 38, 41));
    let mut ts = ui.style("views", "Popover-Header");
    ts.size = 11.0;
    ui.text(&title.to_uppercase(), ts, band, Align::Center);
    let body = Ui::plain_style("Helvetica", 12.0, false, Rgba::WHITE.with_alpha(0.85));
    ui.text(message, body, Rect::new(r.x + 16.0, r.y + 36.0, r.w - 32.0, 30.0), Align::Center);
    let by = r.max_y() - 34.0;
    ui.flat_button("CANCEL", false, Rect::new(r.max_x() - 12.0 - 96.0 * 2.0 - 8.0, by, 96.0, 22.0), pid(Control::PopoverDismiss));
    ui.flat_button(ok_label, true, Rect::new(r.max_x() - 12.0 - 96.0, by, 96.0, 22.0), ok.clone());
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
