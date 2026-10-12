//! The whole Console mixer window (`mixer-geometry.md` §0–§3) and its surfaces.

use houston::{Rect, Rgba};

use crate::draw::Align;
use crate::geometry::{MainStripGeometry, STRIP_W, StripKind, main_strip, window as W};
use crate::input::{Control, ControlId, HitKind, ScrollTarget};
use crate::model::{MixerModel, StripInfo};
use crate::preamp::draw_preamp;
use crate::surfaces::browser::draw_browser;
use crate::surfaces::cells::{self, HEADER_H, INPUT_CELL_H, insert_list, send_list, visible_slots};
use crate::surfaces::monitor::{draw_control_room, draw_monitor_column, monitor_rect};
use crate::surfaces::navigator::{self, VisibleRows, draw_navigator};
use crate::surfaces::popover::draw_popover;
use crate::surfaces::session_nav::draw_session_navigator;
use crate::surfaces::settings::draw_settings_sheet;
use crate::surfaces::status::draw_status_bar;
use crate::surfaces::{MonitorPage, Section, Sheet, ViewState};
use crate::ui::Ui;

/// Column placement (`Rack_WindowLayouts::resized` 0x18167bb40, checked on Console 1.3.1 at
/// 1920 × 1050 and 1470 × 1105). The full layout follows the window size: browser (345) and
/// navigator (96) keep their widths from the left, the monitor column (96) and the bus section
/// pack against the right edge (`Resized_BusSection` 0x181674da0: two aux strips, plus their
/// 17-pt scrollbar while the aux cells overflow, 1.5 pt before the monitor), and the mixer
/// page takes what is left; columns run from y 72 down to the status bar, 40 pt above the
/// bottom. `compact` hides the browser/navigator and packs aux and monitor after the last
/// strip. `browser` = the plug-in browser is shown (full layout only).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowLayout {
    pub compact: bool,
    pub inputs: usize,
    pub auxes: usize,
    pub browser: bool,
    /// Window size in points (the full layout; compact derives its width from the strips).
    pub width: f32,
    pub height: f32,
    /// The aux cells overflow, so the bus section carries its scrollbar.
    pub aux_scroll: bool,
}

const GAP: f32 = 18.0;
/// Vertical scrollbar column of the mixer page and of the bus section.
pub const SCROLLBAR_W: f32 = 17.0;
/// Between the bus section and the monitor column (1.3.1).
const BUS_MONITOR_GAP: f32 = 1.5;
const PANEL: Rgba = Rgba::rgb8(72, 76, 78);
const SEPARATOR: Rgba = Rgba::rgb8(82, 87, 89);
/// Main strip height with the fader hidden (F = 5, label 28).
const MAIN_HIDDEN_H: f32 = 5.0 + main_strip::LABEL_H;

impl WindowLayout {
    pub fn new(view: &ViewState, inputs: usize, auxes: usize) -> Self {
        let (w, h) = view.window.unwrap_or(W::SIZE);
        let mut l = Self { compact: view.compact, inputs, auxes, browser: view.show_browser && !view.compact, width: w, height: h, aux_scroll: true };
        let (min_w, min_h) = l.min_size();
        l.width = l.width.max(min_w);
        l.height = l.height.max(min_h);
        l
    }
    /// With the aux scrollbar shown or not (it depends on the aux cells' height).
    pub fn with_aux_scroll(mut self, aux_scroll: bool) -> Self {
        self.aux_scroll = aux_scroll;
        self
    }
    /// Smallest window: one input strip, the bus section and the monitor; the main strip and a
    /// cell header above it.
    pub fn min_size(&self) -> (f32, f32) {
        let w = self.strips_x() + STRIP_W + SCROLLBAR_W + self.bus_w() + BUS_MONITOR_GAP + STRIP_W;
        (w, W::COLUMN.0 + main_strip::HEIGHT + 5.0 + 22.0 + W::STATUS_BAR.1)
    }
    fn aux_w(&self) -> f32 {
        self.auxes.max(2) as f32 * STRIP_W
    }
    /// Aux strips plus the scrollbar column when it shows.
    pub fn bus_w(&self) -> f32 {
        self.aux_w() + if self.aux_scroll { SCROLLBAR_W } else { 0.0 }
    }
    /// Navigator column x (full layout).
    pub fn nav_x(&self) -> f32 {
        if self.browser { W::BROWSER_W } else { 0.0 }
    }
    pub fn side_x(&self) -> f32 {
        if self.compact { 0.0 } else { self.nav_x() + W::NAV_COLUMN.1 }
    }
    pub fn strips_x(&self) -> f32 {
        self.side_x() + W::SIDE_SECTION.1
    }
    pub fn aux_x(&self) -> f32 {
        if self.compact { self.strips_x() + self.inputs as f32 * STRIP_W + GAP } else { self.monitor_x() - BUS_MONITOR_GAP - self.bus_w() }
    }
    pub fn monitor_x(&self) -> f32 {
        if self.compact { self.strips_x() + self.inputs as f32 * STRIP_W + GAP + self.aux_w() + GAP } else { self.width - STRIP_W }
    }
    pub fn size(&self) -> (f32, f32) {
        if self.compact { (self.monitor_x() + STRIP_W + 4.0, self.height) } else { (self.width, self.height) }
    }
    /// Top of the status bar (and bottom of the columns).
    pub fn status_y(&self) -> f32 {
        self.height - W::STATUS_BAR.1
    }
    /// Input strips that fit between the side section and the aux section.
    pub fn visible_inputs(&self) -> usize {
        if self.compact {
            return self.inputs;
        }
        (((self.aux_x() - GAP - self.strips_x()) / STRIP_W).floor().max(1.0) as usize).min(self.inputs)
    }
}

fn cid(strip: &StripInfo, control: Control) -> Option<ControlId> {
    Some(ControlId { strip: strip.id.clone(), control })
}

/// Row heights of one strip group (inputs or auxes) for the current view state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CellRows {
    /// Input cell (None for aux strips).
    pub input: Option<f32>,
    pub inserts: f32,
    pub sends: f32,
    /// Insert slots shown in every strip of the group.
    pub slots: usize,
    /// Main strip (fader area + label) height.
    pub main: f32,
}

impl CellRows {
    pub fn new(model: &dyn MixerModel, view: &ViewState, strips: &[StripInfo], aux: bool) -> Self {
        let sec = view.sections;
        let slots = strips.iter().map(|s| visible_slots(&model.inserts(s), view.fixed_slots)).max().unwrap_or(4);
        let sends = strips.iter().map(|s| if aux { model.sends(s).len() } else { model.input_sends(s).len() }).max().unwrap_or(0);
        Self {
            input: (!aux).then_some(if sec.inputs { INPUT_CELL_H } else { HEADER_H }),
            inserts: if sec.inserts { cells::inserts_cell_h(slots, view.insert_size, !aux) } else { HEADER_H },
            sends: if sec.sends { cells::sends_cell_h(sends, view.sends_overview) } else { HEADER_H },
            slots,
            main: if sec.fader { main_strip::HEIGHT } else { MAIN_HIDDEN_H },
        }
    }
    pub fn total(&self) -> f32 {
        self.input.unwrap_or(0.0) + self.inserts + self.sends
    }
    /// (section, top y, height) per row, cells-local.
    pub fn rows(&self) -> Vec<(Section, f32, f32)> {
        let mut out = Vec::new();
        let mut y = 0.0;
        if let Some(h) = self.input {
            out.push((Section::Inputs, y, h));
            y += h;
        }
        out.push((Section::Inserts, y, self.inserts));
        y += self.inserts;
        out.push((Section::Sends, y, self.sends));
        out
    }
}

/// Builds the window with default view state (all rows expanded, browser shown).
pub fn build_window(ui: &mut Ui, model: &dyn MixerModel, compact: bool) -> WindowLayout {
    build_window_with(ui, model, &ViewState { compact, ..ViewState::default() })
}

/// Builds the window for `view` (navigator toggles, browser, popovers, sheet, pages).
pub fn build_window_with(ui: &mut Ui, model: &dyn MixerModel, view: &ViewState) -> WindowLayout {
    let inputs = model.inputs();
    let auxes = model.auxes();
    let base = WindowLayout::new(view, inputs.len(), auxes.len());
    let col_h = base.status_y() - W::COLUMN.0;

    // Cell rows. Content that fits is bottom-aligned on the main strips (1.3.1 at 1470 × 1105);
    // content that overflows scrolls, from the top.
    let rows = CellRows::new(model, view, &inputs, false);
    let mut aux_rows = CellRows::new(model, view, &auxes, true);
    // The aux sends cell keeps room for as many sends as an input strip has (1.3.1: two cues
    // shown, four reserved; the p1 aux scrollbar thumb gives 583 pt of aux cells).
    if view.sections.sends {
        let in_sends = inputs.iter().map(|s| model.input_sends(s).len()).max().unwrap_or(0);
        aux_rows.sends = aux_rows.sends.max(cells::sends_cell_h(in_sends, view.sends_overview));
    }
    let cells_h = col_h - rows.main - 5.0;
    let aux_cells_h = col_h - aux_rows.main - 5.0;
    let aux_scroll = view.monitor_page == MonitorPage::Aux && aux_rows.total() > aux_cells_h;
    let l = base.with_aux_scroll(aux_scroll);
    let (width, height) = l.size();
    let bounds = Rect::new(0.0, 0.0, width, height);
    let status_y = l.status_y();

    ui.fill(bounds, Rgba::rgb8(41, 43, 46));

    let overflow = (rows.total() - cells_h).max(0.0);
    let off = if overflow > 0.0 { (view.cells_scroll.clamp(0.0, 1.0) * overflow).round() } else { rows.total() - cells_h };
    let visible_rows = {
        let vis = |s: Section| rows.rows().iter().any(|&(r, y, h)| r == s && y - off + h > 0.0 && y - off < cells_h);
        VisibleRows { inputs: vis(Section::Inputs), inserts: vis(Section::Inserts), sends: vis(Section::Sends) }
    };

    if !l.compact {
        if l.browser {
            draw_browser(ui, model, view, Rect::new(0.0, W::TITLE_BAR, W::BROWSER_W, status_y - W::TITLE_BAR));
        }
        draw_navigator(ui, model, view, l.nav_x(), W::CONTENT_TOP, status_y, visible_rows);
    }

    // Strip area panel and session navigator.
    let visible = l.visible_inputs();
    let first = view.first_strip.min(inputs.len().saturating_sub(visible));
    ui.fill(Rect::new(l.side_x(), W::COLUMN.0, l.aux_x() - l.side_x(), col_h), PANEL);
    draw_session_navigator(ui, model, Rect::new(l.side_x(), W::CONTENT_TOP, l.aux_x() + 0.5 - l.side_x(), W::COLUMN.0 - W::CONTENT_TOP), first, visible);
    side_section(ui, &rows, off, Rect::new(l.side_x(), W::COLUMN.0, W::SIDE_SECTION.1, col_h));

    for (i, s) in inputs.iter().enumerate().skip(first).take(visible) {
        let x = l.strips_x() + (i - first) as f32 * STRIP_W;
        strip_column(ui, model, view, s, &rows, off, Rect::new(x, W::COLUMN.0, STRIP_W, col_h));
    }
    // Empty columns after the last strip, then the cells scrollbar column while they overflow.
    let main_y = W::COLUMN.0 + col_h - rows.main;
    let scroll_x = l.aux_x() - SCROLLBAR_W - 0.5;
    let strips_end = l.strips_x() + inputs.len().saturating_sub(first).min(visible) as f32 * STRIP_W;
    empty_columns(ui, strips_end, if overflow > 0.0 { scroll_x } else { l.aux_x() }, main_y, status_y);
    let cells_area = Rect::new(l.side_x(), W::COLUMN.0 + 5.0, l.aux_x() - l.side_x(), cells_h);
    ui.scroll_region(cells_area, ScrollTarget::Cells, overflow);
    if overflow > 0.0 {
        scrollbar_column(ui, scroll_x, main_y, status_y, cells_h, rows.total(), view.cells_scroll, Some(ControlId { strip: "view".into(), control: Control::CellsScroll }), false);
    }

    let aux_w = auxes.len().max(2) as f32 * STRIP_W;
    match view.monitor_page {
        MonitorPage::Aux => {
            let aux_off = (aux_rows.total() - aux_cells_h).min(0.0);
            if l.aux_scroll {
                let aux_main_y = W::COLUMN.0 + col_h - aux_rows.main;
                scrollbar_column(ui, l.aux_x() + aux_w + 0.5, aux_main_y, status_y, aux_cells_h, aux_rows.total(), 0.0, None, true);
            }
            for (i, s) in auxes.iter().enumerate() {
                let r = Rect::new(l.aux_x() + i as f32 * STRIP_W, W::COLUMN.0, STRIP_W, col_h);
                ui.fill(r, PANEL);
                strip_column(ui, model, view, s, &aux_rows, aux_off, r);
            }
        }
        MonitorPage::ControlRoom => draw_control_room(ui, model, Rect::new(l.aux_x(), W::COLUMN.0, aux_w, col_h)),
    }
    draw_monitor_column(ui, model, view, monitor_rect(l.monitor_x(), height));
    draw_status_bar(ui, model, Rect::new(0.0, status_y, width, W::STATUS_BAR.1));
    // Last of the chrome: its bottom line covers the first point of the columns below.
    title_bar(ui, model, width);

    // Overlays.
    if view.sheet == Some(Sheet::Settings) {
        draw_settings_sheet(ui, model, bounds);
    }
    draw_popover(ui, model, view, bounds);
    l
}

/// Approximate advance width of `text` in Helvetica Bold at `size` pt (Adobe AFM widths),
/// standing in for JUCE's `Font::getStringWidth` where Console sizes a component to its label.
pub fn helvetica_bold_width(text: &str, size: f32) -> f32 {
    const W: [u16; 95] = [
        278, 333, 474, 556, 556, 889, 722, 238, 333, 333, 389, 584, 278, 333, 278, 278, // ' '…/
        556, 556, 556, 556, 556, 556, 556, 556, 556, 556, 333, 333, 584, 584, 584, 611, // 0…?
        975, 722, 722, 722, 722, 667, 611, 778, 722, 278, 556, 722, 611, 833, 722, 778, // @…O
        667, 778, 722, 667, 611, 722, 667, 944, 667, 667, 611, 333, 278, 333, 584, 556, // P…_
        333, 556, 611, 556, 611, 556, 333, 611, 611, 278, 278, 556, 278, 889, 611, 611, // `…o
        611, 611, 389, 556, 333, 611, 556, 778, 556, 556, 500, 389, 280, 389, 584, // p…~
    ];
    text.chars().map(|c| W.get((c as usize).wrapping_sub(32)).copied().unwrap_or(611) as f32).sum::<f32>() * size / 1000.0
}

/// Text with Console's `text-stroke` (a dark outline), approximated by offset copies.
fn stroked_text(ui: &mut Ui, text: &str, style: crate::draw::TextStyle, stroke: Rgba, r: Rect, align: Align) {
    let shadow = crate::draw::TextStyle { color: stroke, ..style.clone() };
    for (dx, dy) in [(-0.5, 0.0), (0.5, 0.0), (0.0, -0.5), (0.0, 0.5), (0.5, 0.5), (-0.5, 0.5)] {
        ui.text(text, shadow.clone(), r.offset(dx, dy), align);
    }
    ui.text(text, style, r, align);
}

/// macOS Console 1.3.1 title bar (`Rack_TitleBar` with `Main-Title-Bar-LP`): a flat 27 pt
/// bar between 1 pt black lines; the window buttons (close, minimise, zoom) dimmed at the
/// left, the session tab (`Titlebar-Tab-On` + divider, `Session-Tab-Single` text) and
/// FEEDBACK (`Toggle-Feedback-LP`: label, icon right-aligned 3 pt in, 6 pt from the edge).
fn title_bar(ui: &mut Ui, model: &dyn MixerModel, width: f32) {
    let h = W::CONTENT_TOP;
    let bg = ui.skin("backgrounds", "Main-Title-Bar-LP").and_then(|s| s.color("color-overlay")).unwrap_or(Rgba::rgb8(75, 79, 83));
    ui.fill(Rect::new(0.0, 0.0, width, h), bg);
    ui.fill(Rect::new(0.0, 0.0, width, 1.0), Rgba::BLACK);
    ui.fill(Rect::new(0.0, h - 1.0, width, 1.0), Rgba::BLACK);
    for (i, key) in ["Window-Button-Close", "Window-Button-Min", "Window-Button-Max-Console"].into_iter().enumerate() {
        if let Some(img) = ui.skin("views", key).and_then(|s| s.image("image")) {
            ui.image(img, Rect::new(10.0 + i as f32 * 25.0, 7.0, 12.0, 12.0), None, None, 0.2);
        }
    }
    let title = format!("Apollo: {}", model.session_name());
    let style = ui.style("views", "Session-Tab-Single");
    let tab_w = helvetica_bold_width(&title, style.size).ceil() + 26.0;
    ui.background("Titlebar-Tab-On", Rect::new(80.0, 0.0, tab_w, h));
    ui.background("Titlebar-Tab-Divider", Rect::new(80.0 + tab_w - 3.0, 0.0, 3.0, h));
    let stroke = ui.skin("views", "Session-Tab").and_then(|s| s.color("color-text-stroke")).unwrap_or(Rgba::BLACK.with_alpha(0.4));
    stroked_text(ui, &title, style, stroke, Rect::new(80.0, 2.0, tab_w, h - 2.0), Align::Center);

    let style = ui.style("views", "Toggle-Feedback-LP");
    let fw = helvetica_bold_width("FEEDBACK", style.size).ceil();
    let b = Rect::new(width - 6.0 - (fw + 31.0), (h - 1.0 - 22.0) / 2.0, fw + 31.0, 22.0);
    ui.text("FEEDBACK", style, Rect::new(b.x + 1.0, b.y + 1.0, fw + 2.0, b.h), Align::Left);
    if let Some(img) = ui.skin("views", "Toggle-Feedback-LP").and_then(|s| s.image("image-overlay")) {
        ui.image(img, Rect::new(b.max_x() - 3.0 - 26.0, b.y, 26.0, 22.0), None, None, 1.0);
    }
}

/// Empty strip columns from `x0` (the edge after the last strip) to `x1`, at the strip pitch
/// (1.3.1): swatch row, black rule, cells well with light column rules, the main area with
/// dark rules and the label band. Colours measured on refpair p1.
fn empty_columns(ui: &mut Ui, x0: f32, x1: f32, main_y: f32, status_y: f32) {
    const CELLS: Rgba = Rgba::rgb8(58, 61, 64);
    const MAIN: Rgba = Rgba::rgb8(80, 84, 87);
    const LABEL: Rgba = Rgba::rgb8(88, 93, 97);
    let w = x1 - x0;
    if w <= 0.0 {
        return;
    }
    let top = W::COLUMN.0;
    let label_y = status_y - main_strip::LABEL_H;
    ui.fill(Rect::new(x0, top + 4.0, w, 1.0), Rgba::BLACK);
    ui.fill(Rect::new(x0, top + 5.0, w, main_y - top - 5.0), CELLS);
    ui.fill(Rect::new(x0, main_y, w, label_y - 1.0 - main_y), MAIN);
    ui.fill(Rect::new(x0, main_y + 4.0, w, 1.0), CELLS);
    ui.fill(Rect::new(x0, label_y - 1.0, w, 1.0), CELLS);
    ui.fill(Rect::new(x0, label_y, w, status_y - label_y), LABEL);
    let mut x = x0;
    while x < x1 {
        ui.fill(Rect::new(x - 0.5, top, 1.0, 4.0), Rgba::BLACK);
        if x > x0 {
            ui.fill(Rect::new(x - 0.5, top + 10.0, 1.0, main_y - top - 15.0), MAIN);
            ui.fill(Rect::new(x - 0.5, main_y, 1.0, status_y - main_y), CELLS);
        }
        x += STRIP_W;
    }
}

/// Scrollbar column (`uad2_scrollbar_background`, 18 pt): white swatch, the
/// `mixer_scrollbar_background` track over the cells with the `Small-Vertical-Scroll-Bar`
/// thumb (9-slice; height track × visible / content), then the strip bottom and label plate.
/// Measured on refpair p1 (inputs at 1596, auxes at 1806).
#[allow(clippy::too_many_arguments)]
fn scrollbar_column(ui: &mut Ui, x: f32, main_y: f32, status_y: f32, visible: f32, content: f32, scroll: f32, id: Option<ControlId>, bus: bool) {
    let top = W::COLUMN.0;
    let label_y = status_y - main_strip::LABEL_H;
    ui.fill(Rect::new(x + 1.0, top, 17.0, 4.0), Rgba::WHITE);
    let track = Rect::new(x, top + 4.0, 18.0, main_y - top - 4.0);
    let bg = ui.assets.appearance.colors.get("mixer_scrollbar_background").copied().unwrap_or(Rgba::rgb8(35, 37, 40));
    ui.fill(track, bg);
    let thumb_h = (track.h * visible / content.max(1.0)).clamp(30.0, track.h);
    let thumb = Rect::new(x, track.y + (track.h - thumb_h) * scroll.clamp(0.0, 1.0), 18.0, thumb_h);
    if let Some(img) = ui.skin("backgrounds", "Small-Vertical-Scroll-Bar").and_then(|k| k.image("image")) {
        ui.image(img, thumb, Some(houston::Insets::uniform(12.0)), None, 1.0);
    }
    if let Some(id) = id {
        ui.hit(thumb, id, HitKind::Drag { value: 1.0 - scroll as f64, span: track.h - thumb_h, reset: None });
    }
    if bus {
        // Under the bus scrollbar: the aux strips' dark main area and name band.
        ui.fill(Rect::new(x, main_y, 18.0, 2.0), Rgba::rgb8(82, 88, 89));
        ui.fill(Rect::new(x, main_y + 2.0, 18.0, label_y - 1.0 - main_y - 2.0), Rgba::rgb8(51, 53, 56));
        ui.fill(Rect::new(x, label_y - 1.0, 18.0, 1.0), Rgba::rgb8(73, 76, 79));
        ui.fill(Rect::new(x, label_y, 18.0, main_strip::LABEL_H - 1.0), Rgba::rgb8(50, 52, 55));
        ui.fill(Rect::new(x, status_y - 1.0, 18.0, 1.0), Rgba::rgb8(72, 76, 78));
    } else {
        ui.background("Channel-Bottom", Rect::new(x, main_y, 18.0, label_y - main_y));
        ui.fill(Rect::new(x, label_y - 1.0, 18.0, 1.0), Rgba::BLACK);
        ui.fill(Rect::new(x, label_y, 18.0, main_strip::LABEL_H - 1.0), Rgba::WHITE);
    }
}

/// `Rack_ChannelStripsSideSection` (46.5 pt) and the reveal column (`console_reveal.json`):
/// white swatch, per-row Mixer-Row-Header with the reveal chevron at (17, 5) and, on open
/// rows, the yellow row icon at (15, 35); the main area's `fader_and_meters_bg` with
/// S (clear solos) / OL (clear clips) at main + 63 / + 87, and the white label plate.
/// Measured on Console 1.3.1 (p1 1920 × 1050, p2 1470 × 1105).
fn side_section(ui: &mut Ui, rows: &CellRows, off: f32, r: Rect) {
    const ROW_ICON: Rgba = Rgba::rgb8(246, 201, 91);
    const ROW_ICON_EDGE: Rgba = Rgba::rgb8(44, 46, 48);
    let main_y = r.h - rows.main;
    let nav = |c: Control| ControlId { strip: navigator::NAV_ID.into(), control: c };
    ui.at(r.x, r.y, |ui| {
        ui.fill(Rect::new(0.5, 0.0, r.w - 1.0, 4.0), Rgba::WHITE);
        ui.clip(Some(Rect::new(0.0, 5.0, r.w, main_y - 5.0)));
        ui.at(0.0, 5.0 - off, |ui| {
            for (sec, y, h) in rows.rows() {
                ui.background("Mixer-Row-Header", Rect::new(0.0, y, r.w, HEADER_H));
                let open = rows_open(rows, sec);
                if let Some(img) = ui.skin("views", "Browser-Reveal-Toggle").and_then(|k| k.image(if open { "image-on" } else { "image-off" })) {
                    ui.image(img, Rect::new(17.0, y + 5.0, 12.0, 12.0), None, None, 1.0);
                }
                if open && h >= 50.0 {
                    let icon = Rect::new(15.0, y + 35.0, 16.0, 15.0);
                    ui.fill(icon, ROW_ICON_EDGE);
                    ui.fill(Rect::new(icon.x + 1.0, icon.y + 1.0, 14.0, 6.0), ROW_ICON);
                    ui.fill(Rect::new(icon.x + 1.0, icon.y + 8.0, 14.0, 6.0), ROW_ICON);
                }
                ui.hit(Rect::new(0.0, y, r.w, HEADER_H), nav(Control::Section(sec)), HitKind::Toggle);
            }
        });
        ui.clip(None);
        let label_y = r.h - main_strip::LABEL_H;
        if rows.main > MAIN_HIDDEN_H {
            ui.background("Channel-Bottom", Rect::new(0.0, main_y, r.w, label_y - main_y));
            for (i, (label, c)) in [("S", Control::ClearSolos), ("OL", Control::ClearClips)].into_iter().enumerate() {
                let b = Rect::new(7.0, main_y + 63.0 + i as f32 * 24.0, 32.0, 19.0);
                ui.toggle("Switch-Small", Some(label), false, true, b, None, None);
                ui.hit(b, ControlId { strip: "global".into(), control: c }, HitKind::Tap);
            }
        }
        ui.fill(Rect::new(0.0, label_y - 1.0, r.w, 1.0), Rgba::BLACK);
        ui.fill(Rect::new(0.0, label_y, r.w, main_strip::LABEL_H - 1.0), Rgba::WHITE);
    });
}

fn rows_open(rows: &CellRows, sec: Section) -> bool {
    match sec {
        Section::Inputs => rows.input.is_some_and(|h| h > HEADER_H),
        Section::Inserts => rows.inserts > HEADER_H,
        Section::Sends => rows.sends > HEADER_H,
        Section::Fader => rows.main > MAIN_HIDDEN_H,
    }
}

fn strip_column(ui: &mut Ui, model: &dyn MixerModel, view: &ViewState, s: &StripInfo, rows: &CellRows, off: f32, r: Rect) {
    let main_y = r.h - rows.main;
    ui.at(r.x, r.y, |ui| {
        ui.fill(Rect::new(1.0, 0.0, 94.0, 4.0), Rgba::rgb8(41, 86, 125)); // swatch
        ui.clip(Some(Rect::new(0.0, 5.0, STRIP_W, main_y - 5.0)));
        ui.at(0.0, 5.0 - off, |ui| strip_cells(ui, model, view, s, rows));
        ui.clip(None);
        ui.fill(Rect::new(0.0, 5.0, 1.0, main_y - 5.0), SEPARATOR);
        ui.fill(Rect::new(95.0, 5.0, 1.0, main_y - 5.0), SEPARATOR);
        ui.at(0.0, main_y, |ui| {
            if rows.main > MAIN_HIDDEN_H {
                main_strip(ui, model, s);
            } else {
                name_plate(ui, model, s, Rect::new(0.0, 5.0, STRIP_W, main_strip::LABEL_H));
            }
        });
    });
}

/// Input: input cell → inserts → sends; aux: inserts → sends. Collapsed rows are headers only.
fn strip_cells(ui: &mut Ui, model: &dyn MixerModel, view: &ViewState, s: &StripInfo, rows: &CellRows) {
    let bus = s.kind == StripKind::Aux;
    let mut y = 0.0;
    if let Some(h) = rows.input {
        ui.cell_header(&s.title, false, false, Rect::new(0.0, 0.0, STRIP_W, HEADER_H));
        if h > HEADER_H {
            input_cell(ui, model, s);
        }
        y += h;
    }
    ui.cell_header("INSERTS", true, bus, Rect::new(0.0, y, STRIP_W, HEADER_H));
    if rows.inserts > HEADER_H {
        let slots = model.inserts(s);
        insert_list(ui, s, &slots, rows.slots.min(slots.len()), view.insert_size, y + 26.0);
        if !bus {
            rec_mon(ui, model.records_pre_effects(s), s, y + 26.0 + rows.slots as f32 * view.insert_size.pitch() + 4.0);
        }
    }
    y += rows.inserts;
    ui.cell_header("SENDS", false, bus, Rect::new(0.0, y, STRIP_W, HEADER_H));
    if rows.sends > HEADER_H {
        let sends = if bus { model.sends(s) } else { model.input_sends(s) };
        send_list(ui, s, &sends, &|k| model.send_pre(s, k), view, y + 26.0);
    }
}

fn input_cell(ui: &mut Ui, model: &dyn MixerModel, s: &StripInfo) {
    if let Some(layout) = &s.preamp_layout {
        ui.background("Channel-Inserts-Background", Rect::new(4.0, 26.0, 88.0, 73.0));
        ui.font_text("Send-Label", "UNISON", Rect::new(4.0, 28.0, 88.0, 14.0), Align::Center);
        let unison = model.unison(s);
        ui.insert_slot(&unison, Rect::new(0.0, 44.0, STRIP_W, 48.0), cid(s, Control::Insert(255)));
        if let Some(state) = model.preamp(s) {
            draw_preamp(ui, layout, &state, &s.id, Rect::new(0.0, 101.0, STRIP_W, 129.0));
        }
    } else if s.shows_sr_convert {
        ui.background("Secondary-Display-Background", Rect::new(8.0, 174.0, 80.0, 48.0));
        ui.font_text("Send-Label", "SR CONVERT", Rect::new(8.0, 178.0, 80.0, 14.0), Align::Center);
        ui.toggle("Switch-Small", Some("OFF"), false, true, Rect::new(14.0, 196.0, 32.0, 19.0), None, None);
        ui.toggle("Switch-Small", Some("ON"), true, true, Rect::new(50.0, 196.0, 32.0, 19.0), None, None);
    }
}

fn rec_mon(ui: &mut Ui, monitor_only: bool, s: &StripInfo, y: f32) {
    let label = ui.style("fonts", "Send-Label");
    ui.text("UAD", label.clone(), Rect::new(14.0, y + 2.0, 28.0, 10.0), Align::Center);
    let (rec, mon) = if monitor_only {
        (Rgba::rgb8(90, 26, 16), Rgba::rgb8(77, 178, 242))
    } else {
        (Rgba::rgb8(237, 77, 38), Rgba::rgb8(51, 64, 77))
    };
    ui.fill(Rect::new(10.0, y + 15.0, 12.0, 12.0), rec);
    ui.fill(Rect::new(34.0, y + 15.0, 12.0, 12.0), mon);
    ui.text("REC", label.clone(), Rect::new(5.0, y + 29.0, 22.0, 10.0), Align::Center);
    ui.text("MON", label, Rect::new(28.0, y + 29.0, 24.0, 10.0), Align::Center);
    ui.hit(Rect::new(4.0, y + 10.0, 44.0, 20.0), ControlId { strip: s.id.clone(), control: Control::RecMon }, HitKind::Toggle);
    ui.toggle("Switch", Some("INS"), false, true, Rect::new(50.0, y, 40.0, 40.0), None, None);
}

fn main_strip(ui: &mut Ui, model: &dyn MixerModel, s: &StripInfo) {
    let g = MainStripGeometry::new(s.kind);
    let st = model.main(s);
    let meters = model.meters(s);
    match s.kind {
        StripKind::Input => ui.background("Channel-Bottom", Rect::new(0.0, 0.0, STRIP_W, g.label.y)),
        StripKind::Aux => ui.fill(Rect::new(0.0, 0.0, STRIP_W, main_strip::HEIGHT), Rgba::rgb8(56, 60, 62)),
    }
    if let Some(p) = g.pans {
        let half = (p.w / 2.0).floor();
        if s.stereo {
            ui.knob("Pan-Black", st.pan_l, Rect::new(p.x, p.y, half, p.h), cid(s, Control::PanL), Some(0.0));
            ui.knob("Pan-Black", st.pan_r, Rect::new(p.x + half, p.y, half, p.h), cid(s, Control::PanR), Some(1.0));
        } else {
            ui.knob("Pan-Black", st.pan_l, Rect::new(p.x + (half / 2.0).floor(), p.y, half, p.h), cid(s, Control::PanL), Some(0.5));
        }
    }
    if let Some(r) = g.solo_mute {
        let (bw, bh) = ui.skin("views", "Switch-Small").and_then(|k| k.size()).unwrap_or((32.0, 19.0));
        let y = r.y + ((r.h - bh) / 2.0).floor();
        let mid = (r.w / 2.0).floor();
        ui.toggle("Switch-Small", Some("S"), st.solo, true, Rect::new(mid - bw - 3.0, y, bw, bh), cid(s, Control::Solo), None);
        ui.toggle("Switch-Small-Red", Some("M"), st.mute, true, Rect::new(mid + 3.0, y, bw, bh), cid(s, Control::Mute), None);
    }
    if let Some(r) = g.pre_post {
        ui.toggle("Switch", Some("PRE"), st.pre, true, Rect::new(8.0, r.y, 40.0, 40.0), cid(s, Control::PrePost), None);
        ui.toggle("Switch", Some("POST"), !st.pre, true, Rect::new(48.0, r.y, 40.0, 40.0), cid(s, Control::PrePost), None);
    }
    if let Some(r) = g.mono_mute {
        ui.toggle("Switch", Some("MONO"), st.mono, true, Rect::new(8.0, r.y, 40.0, 40.0), cid(s, Control::Mono), None);
        ui.toggle("Switch-Red", Some("MUTE"), st.mute, true, Rect::new(48.0, r.y, 40.0, 40.0), cid(s, Control::Mute), None);
    }
    let fader_key = if s.kind == StripKind::Aux { "Main-Bus-Fader" } else { "Main-Input-Fader" };
    ui.fader_and_meters(fader_key, s.stereo, st.fader, meters, &st.readout, g.fader_meters, cid(s, Control::Fader));
    name_plate(ui, model, s, g.label);
}

/// Name plate (tap: rename / isolate) with the "[ ISOLATE ]" tag.
fn name_plate(ui: &mut Ui, model: &dyn MixerModel, s: &StripInfo, r: Rect) {
    let st = model.main(s);
    ui.name_plate(if s.kind == StripKind::Aux { "Channel-Aux-Name" } else { "Channel-Input-Name" }, &st.name, r);
    if model.isolated(s) {
        let color = if s.kind == StripKind::Aux { Rgba::WHITE.with_alpha(0.5) } else { Rgba::rgb8(40, 40, 40) };
        ui.text("[ ISOLATE ]", Ui::plain_style("Helvetica", 6.5, false, color), Rect::new(r.x, r.y + 17.0, r.w, 9.0), Align::Center);
    }
    ui.hit(r, ControlId { strip: s.id.clone(), control: Control::Name }, HitKind::Tap);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(window: (f32, f32), aux_scroll: bool) -> WindowLayout {
        let view = ViewState { window: Some(window), ..ViewState::default() };
        WindowLayout::new(&view, 7, 2).with_aux_scroll(aux_scroll)
    }

    #[test]
    fn columns_follow_the_window_size() {
        // p1 refpair, 1920 × 1050: the aux cells overflow, so the bus section has its scrollbar.
        let l = layout((1920.0, 1050.0), true);
        assert_eq!((l.nav_x(), l.side_x(), l.strips_x()), (345.0, 441.0, 487.5));
        assert_eq!((l.aux_x(), l.monitor_x(), l.status_y()), (1613.5, 1824.0, 1010.0));
        assert_eq!(l.size(), (1920.0, 1050.0));
        // p2 refpair, 1470 × 1105: aux and monitor pack right after the strips.
        let l = layout((1470.0, 1105.0), false);
        assert_eq!((l.aux_x(), l.monitor_x(), l.status_y()), (1180.5, 1374.0, 1065.0));
        assert_eq!(l.visible_inputs(), 7);
        // Too small a window clamps to one strip and a cell header.
        let l = layout((400.0, 300.0), true);
        assert!(l.visible_inputs() >= 1 && l.size().0 > 400.0 && l.size().1 > 300.0);
    }

    #[test]
    fn helvetica_bold_widths() {
        // The title tab is the title's width + 26 (refpair p1: 122 pt for "Apollo: Session*").
        assert_eq!(helvetica_bold_width("Apollo: Session*", 12.0).ceil() + 26.0, 122.0);
    }
}
