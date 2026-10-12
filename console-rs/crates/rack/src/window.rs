//! The whole Console mixer window (`mixer-geometry.md` §0–§3) and its surfaces.

use houston::{Rect, Rgba};

use crate::draw::Align;
use crate::geometry::{MainStripGeometry, STRIP_W, StripKind, main_strip, window as W};
use crate::input::{Control, ControlId, HitKind, ScrollTarget};
use crate::model::{MixerModel, StripInfo};
use crate::preamp::draw_preamp;
use crate::surfaces::browser::draw_browser;
use crate::surfaces::cells::{self, HEADER_H, INPUT_CELL_H, insert_list, send_list, visible_slots};
use crate::surfaces::monitor::{draw_control_room, draw_monitor_column};
use crate::surfaces::navigator::{self, VisibleRows, draw_navigator};
use crate::surfaces::popover::draw_popover;
use crate::surfaces::session_nav::draw_session_navigator;
use crate::surfaces::settings::draw_settings_sheet;
use crate::surfaces::status::draw_status_bar;
use crate::surfaces::{MonitorPage, Section, Sheet, ViewState};
use crate::ui::Ui;

/// Column placement. `full` reproduces Console's 1920-pt window (browser + navigator,
/// aux at 1613.5, monitor at 1823.5; strips beyond the visible range scroll with the session
/// navigator); `compact` hides the browser/navigator and packs aux and monitor after the last
/// strip. `browser` = the plug-in browser is shown (full layout only).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowLayout {
    pub compact: bool,
    pub inputs: usize,
    pub auxes: usize,
    pub browser: bool,
}

const GAP: f32 = 18.0;
const PANEL: Rgba = Rgba::rgb8(72, 76, 78);
const CHROME: Rgba = Rgba::rgb8(35, 37, 40);
const SEPARATOR: Rgba = Rgba::rgb8(82, 87, 89);
/// Main strip height with the fader hidden (F = 5, label 28).
const MAIN_HIDDEN_H: f32 = 5.0 + main_strip::LABEL_H;

impl WindowLayout {
    pub fn new(view: &ViewState, inputs: usize, auxes: usize) -> Self {
        Self { compact: view.compact, inputs, auxes, browser: view.show_browser && !view.compact }
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
        if self.compact { self.strips_x() + self.inputs as f32 * STRIP_W + GAP } else { W::AUX_X }
    }
    pub fn monitor_x(&self) -> f32 {
        let packed = self.aux_x() + self.auxes.max(2) as f32 * STRIP_W + GAP;
        if self.compact { packed } else { packed.max(W::MONITOR_X) }
    }
    pub fn size(&self) -> (f32, f32) {
        let w = self.monitor_x() + STRIP_W + if self.compact { 4.0 } else { 0.5 };
        (if self.compact { w } else { w.max(W::SIZE.0) }, W::SIZE.1)
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
    let l = WindowLayout::new(view, inputs.len(), auxes.len());
    let (width, height) = l.size();
    let bounds = Rect::new(0.0, 0.0, width, height);
    let col_h = W::COLUMN.1 - W::COLUMN.0;

    ui.fill(bounds, Rgba::rgb8(41, 43, 46));
    ui.fill(Rect::new(0.0, 0.0, width, W::TITLE_BAR), CHROME);
    let title = Ui::plain_style("Helvetica Neue", 12.0, true, Rgba::WHITE.with_alpha(0.85));
    ui.text(&format!("Apollo: {}", model.session_name()), title, Rect::new(97.0, 0.0, 400.0, W::TITLE_BAR), Align::Left);

    // Input cell rows and scrolling.
    let rows = CellRows::new(model, view, &inputs, false);
    let cells_h = col_h - rows.main - 5.0;
    let overflow = (rows.total() - cells_h).max(0.0);
    let off = (view.cells_scroll.clamp(0.0, 1.0) * overflow).round();
    let visible_rows = {
        let vis = |s: Section| rows.rows().iter().any(|&(r, y, h)| r == s && y - off + h > 0.0 && y - off < cells_h);
        VisibleRows { inputs: vis(Section::Inputs), inserts: vis(Section::Inserts), sends: vis(Section::Sends) }
    };

    if !l.compact {
        if l.browser {
            draw_browser(ui, model, view, Rect::new(0.0, W::TITLE_BAR, W::BROWSER_W, W::STATUS_BAR.0 - W::TITLE_BAR));
        }
        draw_navigator(ui, model, view, l.nav_x(), W::TITLE_BAR, W::STATUS_BAR.0, visible_rows);
    }

    // Strip area panel and session navigator.
    let visible = l.visible_inputs();
    let first = view.first_strip.min(inputs.len().saturating_sub(visible));
    ui.fill(Rect::new(l.side_x(), W::COLUMN.0, l.aux_x() - l.side_x(), col_h), PANEL);
    draw_session_navigator(ui, model, Rect::new(l.side_x(), W::NAV_STRIP.0, l.aux_x() - l.side_x(), W::NAV_STRIP.1 - W::NAV_STRIP.0), first, visible);
    side_section(ui, &rows, off, Rect::new(l.side_x(), W::COLUMN.0, W::SIDE_SECTION.1, col_h));

    for (i, s) in inputs.iter().enumerate().skip(first).take(visible) {
        let x = l.strips_x() + (i - first) as f32 * STRIP_W;
        let pos = GroupPos { index: i - first, last: i - first + 1 == visible.min(inputs.len() - first) };
        strip_column(ui, model, view, s, &rows, off, pos, Rect::new(x, W::COLUMN.0, STRIP_W, col_h));
    }
    // Cells scrolling: wheel region and scrollbar in the gap before the aux section.
    let cells_area = Rect::new(l.side_x(), W::COLUMN.0 + 5.0, l.aux_x() - l.side_x(), cells_h);
    ui.scroll_region(cells_area, ScrollTarget::Cells, overflow);
    scrollbar(ui, Rect::new(l.aux_x() - 13.0, cells_area.y, 8.0, cells_area.h), rows.total(), view.cells_scroll, ControlId { strip: "view".into(), control: Control::CellsScroll });

    let aux_w = auxes.len().max(2) as f32 * STRIP_W;
    match view.monitor_page {
        MonitorPage::Aux => {
            let aux_rows = CellRows::new(model, view, &auxes, true);
            for (i, s) in auxes.iter().enumerate() {
                let r = Rect::new(l.aux_x() + i as f32 * STRIP_W, W::COLUMN.0, STRIP_W, col_h);
                ui.fill(r, PANEL);
                strip_column(ui, model, view, s, &aux_rows, 0.0, GroupPos { index: i, last: i + 1 == auxes.len() }, r);
            }
        }
        MonitorPage::ControlRoom => draw_control_room(ui, model, Rect::new(l.aux_x(), W::COLUMN.0, aux_w, col_h)),
    }
    draw_monitor_column(ui, model, view, Rect::new(l.monitor_x(), W::MONITOR_TOP, STRIP_W, W::STATUS_BAR.0 - W::MONITOR_TOP));
    draw_status_bar(ui, model, Rect::new(0.0, W::STATUS_BAR.0, width, W::STATUS_BAR.1));

    // Overlays.
    if view.sheet == Some(Sheet::Settings) {
        draw_settings_sheet(ui, model, bounds);
    }
    draw_popover(ui, model, view, bounds);
    l
}

/// Vertical scrollbar with a draggable thumb; nothing when the content fits.
fn scrollbar(ui: &mut Ui, track: Rect, content: f32, scroll: f32, id: ControlId) {
    if content <= track.h {
        return;
    }
    ui.fill(track, Rgba::rgb8(36, 38, 40));
    let thumb_h = (track.h * track.h / content).max(30.0);
    let thumb = Rect::new(track.x, track.y + (track.h - thumb_h) * scroll.clamp(0.0, 1.0), track.w, thumb_h);
    ui.fill(thumb, Rgba::rgb8(20, 21, 22));
    ui.fill(thumb.inset(1.0, 1.0, 1.0, 1.0), Rgba::rgb8(104, 108, 112));
    ui.hit(thumb.inset(-3.0, 0.0, -3.0, 0.0), id, HitKind::Drag { value: 1.0 - scroll as f64, span: track.h - thumb_h, reset: None });
}

/// `Rack_ChannelStripsSideSection` (46.5 pt): per-row collapse chevrons aligned with the
/// cell headers, and S (clear solos) / OL (clear clips) by the faders.
fn side_section(ui: &mut Ui, rows: &CellRows, off: f32, r: Rect) {
    let main_y = r.h - rows.main;
    let nav = |c: Control| ControlId { strip: navigator::NAV_ID.into(), control: c };
    ui.at(r.x, r.y, |ui| {
        ui.clip(Some(Rect::new(0.0, 5.0, r.w, main_y - 5.0)));
        ui.at(0.0, 5.0 - off, |ui| {
            for (sec, y, _) in rows.rows() {
                ui.background("Mixer-Row-Header", Rect::new(0.0, y, r.w, HEADER_H));
                let open = rows_open(rows, sec);
                if let Some(skin) = ui.skin("views", "Browser-Reveal-Toggle") {
                    if let Some(img) = if open { skin.image("image-on") } else { skin.image("image-off") } {
                        let img = img.to_string();
                        ui.image(&img, Rect::new((r.w - 14.0) / 2.0, y + 4.0, 14.0, 14.0), None, None, 0.7);
                    }
                }
                ui.hit(Rect::new(0.0, y, r.w, HEADER_H), nav(Control::Section(sec)), HitKind::Toggle);
            }
        });
        ui.clip(None);
        if rows.main > MAIN_HIDDEN_H {
            for (i, (label, c)) in [("S", Control::ClearSolos), ("OL", Control::ClearClips)].into_iter().enumerate() {
                let b = Rect::new(7.0, main_y + 62.0 + i as f32 * 25.0, 32.0, 19.0);
                ui.toggle("Switch-Small", Some(label), false, true, b, None, None);
                ui.hit(b, ControlId { strip: "global".into(), control: c }, HitKind::Tap);
            }
        }
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

