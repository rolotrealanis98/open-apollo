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
        let w = self.monitor_x() + STRIP_W + if self.compact { 4.0 } else { 0.0 };
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

/// Vertical offset of a cells stack: scrolled when it overflows the column; when it is shorter,
/// Console anchors it at the bottom, against the fader area (p2: input header at 119, aux at 84),
/// which is a negative offset.
pub fn cells_offset(rows: &CellRows, cells_h: f32, scroll: f32) -> f32 {
    let spare = rows.total() - cells_h;
    if spare >= 0.0 { (scroll.clamp(0.0, 1.0) * spare).round() } else { spare.floor() }
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
        let sends = strips.iter().map(|s| model.send_rows(s)).max().unwrap_or(0);
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
    let off = cells_offset(&rows, cells_h, view.cells_scroll);
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
                strip_column(ui, model, view, s, &aux_rows, cells_offset(&aux_rows, cells_h, 0.0), GroupPos { index: i, last: i + 1 == auxes.len() }, r);
            }
        }
        MonitorPage::ControlRoom => draw_control_room(ui, model, Rect::new(l.aux_x(), W::COLUMN.0, aux_w, col_h)),
    }
    draw_monitor_column(ui, model, view, monitor_rect(if l.compact { l.monitor_x() } else { width - STRIP_W }, height));
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


/// Where a strip sits in its group (inputs or auxes): the group's outer edges get a black
/// border and an inner shade, and aux strips after the first get a separator in the fader area.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GroupPos {
    pub index: usize,
    pub last: bool,
}

/// Channel colour band (`Rack_Swatch`, `(1, 0, w−2, 4)`). Console 1.3.1 defaults, measured in
/// `refpairs/p1`: inputs blue, AUX 1 amber, AUX 2 blue (`GetChannelColor` palette is [unk]).
pub fn swatch_color(s: &StripInfo, index: usize) -> Rgba {
    match s.kind {
        StripKind::Input => Rgba::rgb8(40, 87, 126),
        StripKind::Aux => [Rgba::rgb8(186, 129, 43), Rgba::rgb8(46, 107, 191)][index % 2],
    }
}

/// Black behind the swatch row and under the name plate (the column shows through).
const COLUMN_BG: Rgba = Rgba::BLACK;
/// `colors:uad2_channel_separator`, between input name plates.
const PLATE_SEPARATOR: Rgba = Rgba::rgb8(72, 72, 72);
/// Aux fader-area background and its 2-pt top line / separator (measured).
const AUX_MAIN_BG: Rgba = Rgba::rgb8(72, 76, 79);
/// Aux name plate (Console 1.3.1 draws a flat plate, not `show_fader_aux_label_bg.png`).
const AUX_PLATE: Rgba = Rgba::rgb8(50, 53, 55);

fn strip_column(ui: &mut Ui, model: &dyn MixerModel, view: &ViewState, s: &StripInfo, rows: &CellRows, off: f32, pos: GroupPos, r: Rect) {
    let main_y = r.h - rows.main;
    let cells_h = main_y - 5.0;
    // Rack lays strips out on whole points: 488 and 1614 in the 1920-pt window.
    ui.at(r.x.ceil(), r.y, |ui| {
        ui.fill(Rect::new(0.0, 0.0, STRIP_W, 5.0), COLUMN_BG);
        ui.fill(Rect::new(1.0, 0.0, STRIP_W - 2.0, 4.0), swatch_color(s, pos.index));
        ui.clip(Some(Rect::new(0.0, 5.0, STRIP_W, cells_h)));
        ui.at(0.0, 5.0 - off, |ui| strip_cells(ui, model, view, s, rows, cells_h + off));
        ui.clip(None);
        group_edges(ui, pos, Rect::new(0.0, 5.0, STRIP_W, cells_h));
        ui.at(0.0, main_y, |ui| {
            if rows.main > MAIN_HIDDEN_H {
                main_strip(ui, model, s, pos);
            } else {
                ui.fill(Rect::new(0.0, 5.0, STRIP_W, main_strip::LABEL_H), if s.kind == StripKind::Aux { AUX_MAIN_BG } else { COLUMN_BG });
                name_plate(ui, model, s, Rect::new(0.0, 5.0, STRIP_W, main_strip::LABEL_H));
            }
        });
    });
}

/// Outer edges of a strip group over the cells: a 1-pt black line and a soft inner shade.
fn group_edges(ui: &mut Ui, pos: GroupPos, r: Rect) {
    const LIGHT: [f32; 4] = [0.055, 0.044, 0.027, 0.005];
    const DARK: [f32; 3] = [0.07, 0.055, 0.028];
    if pos.index == 0 {
        ui.fill(Rect::new(r.x, r.y, 1.0, r.h), Rgba::BLACK);
        for (i, a) in LIGHT.iter().enumerate() {
            ui.fill(Rect::new(r.x + 1.0 + i as f32 * 0.5, r.y, 0.5, r.h), Rgba::WHITE.with_alpha(*a));
        }
    }
    if pos.last {
        let x = r.max_x() - 1.0;
        ui.fill(Rect::new(x, r.y, 1.0, r.h), Rgba::BLACK);
        for (i, a) in DARK.iter().enumerate() {
            ui.fill(Rect::new(x - 0.5 - i as f32 * 0.5, r.y, 0.5, r.h), Rgba::BLACK.with_alpha(*a));
        }
    }
}

/// `channel_strip_separator_line` at x = 0 and w−1 of a cell, from y+27 to the bottom − 5
/// (`Rack_ChannelStripCells::paint` 0x181402830). Input strips only; the aux section has none.
fn cell_separators(ui: &mut Ui, y: f32, h: f32) {
    if h > 32.0 {
        ui.fill(Rect::new(0.0, y + 27.0, 1.0, h - 32.0), SEPARATOR);
        ui.fill(Rect::new(STRIP_W - 1.0, y + 27.0, 1.0, h - 32.0), SEPARATOR);
    }
}

/// Input: input cell → inserts → sends; aux: inserts → sends. Collapsed rows are headers only.
/// `visible_h`: cells-local bottom of the visible column (the sends well runs down to it).
fn strip_cells(ui: &mut Ui, model: &dyn MixerModel, view: &ViewState, s: &StripInfo, rows: &CellRows, visible_h: f32) {
    let bus = s.kind == StripKind::Aux;
    let mut y = 0.0;
    if let Some(h) = rows.input {
        if h > HEADER_H {
            cell_separators(ui, y, h);
        }
        ui.cell_header(&s.title, false, false, Rect::new(0.0, 0.0, STRIP_W, HEADER_H));
        if h > HEADER_H {
            input_cell(ui, model, s);
        }
        y += h;
    }
    if rows.inserts > HEADER_H && !bus {
        cell_separators(ui, y, rows.inserts);
    }
    ui.cell_header("INSERTS", true, bus, Rect::new(0.0, y, STRIP_W, HEADER_H));
    if rows.inserts > HEADER_H {
        let slots = model.inserts(s);
        insert_list(ui, s, &slots, rows.slots, view.insert_size, y + 26.0);
        if !bus {
            rec_mon(ui, model.records_pre_effects(s), s, y + 26.0 + cells::inserts_list_h(rows.slots, view.insert_size) + 4.0);
        }
    }
    y += rows.inserts;
    if rows.sends > HEADER_H && !bus {
        cell_separators(ui, y, rows.sends);
    }
    ui.cell_header("SENDS", false, bus, Rect::new(0.0, y, STRIP_W, HEADER_H));
    if rows.sends > HEADER_H {
        let sends = if bus { model.sends(s) } else { model.input_sends(s) };
        let well_h = (visible_h - y - 26.0).max(rows.sends - 31.0);
        send_list(ui, s, &sends, &|k| model.send_pre(s, k), view, y + 26.0, well_h);
    }
}

/// Input cell (`Rack_ChannelStripCell_Input::resized` 0x1814b90a0): Unison slot from y = 25,
/// then the preamp overview at 101 (measured in p1; `Resized_InputOrInstrument` 0x1814aeb80).
fn input_cell(ui: &mut Ui, model: &dyn MixerModel, s: &StripInfo) {
    if let Some(layout) = &s.preamp_layout {
        unison(ui, &model.unison(s), s, Rect::new(0.0, 25.0, STRIP_W, 77.0));
        if let Some(state) = model.preamp(s) {
            // Rack_Overview_Preamp's secondary_background (Secondary-Channel-Inserts-Background)
            // runs 2 pt past the 129-pt plate, as under SR CONVERT.
            ui.background("Secondary-Channel-Inserts-Background", Rect::new(4.0, 101.0, STRIP_W - 8.0, 131.0));
            draw_preamp(ui, layout, &state, &s.id, Rect::new(0.0, 101.0, STRIP_W, 129.0));
        }
    } else if s.shows_sr_convert {
        sr_convert(ui, Rect::new(0.0, 167.0, STRIP_W, 65.0));
    }
}

/// S/PDIF input: the SR CONVERT branch of `Rack_Overview_Preamp::resized` (0x181525930,
/// type 3; skins from 0x1815224c0), bottom-aligned in the input cell like the preamp:
/// secondary background, label plate `(4, 0, w−8, 24)` with the Silk-Screen title `(0, 5, w, 16)`,
/// lower plate `(6, 24, w−12, 19 + 2·10)`, radio background and OFF / ON at `(10+6, 34)` / `(48, 34)`.
fn sr_convert(ui: &mut Ui, r: Rect) {
    ui.background("Secondary-Channel-Inserts-Background", Rect::new(r.x + 4.0, r.y, r.w - 8.0, r.h));
    ui.background("Channel-Inserts-Background", Rect::new(r.x + 4.0, r.y, r.w - 8.0, 24.0));
    ui.font_text("Silk-Screen", "SR CONVERT", Rect::new(r.x, r.y + 5.0, r.w, 16.0), Align::Center);
    ui.background("Send-Lower-State-Plate", Rect::new(r.x + 6.0, r.y + 24.0, r.w - 12.0, 39.0));
    ui.background("Preamp-Radio-Background", Rect::new(r.x + 16.0, r.y + 34.0, 64.0, 19.0));
    ui.toggle("SR-Convert-Off", Some("OFF"), false, true, Rect::new(r.x + 16.0, r.y + 34.0, 32.0, 19.0), None, None);
    ui.toggle("SR-Convert-On", Some("ON"), true, true, Rect::new(r.x + 48.0, r.y + 34.0, 32.0, 19.0), None, None);
}

/// `Rack_Inserts_Unison`: a one-slot `Rack_Inserts` with its header row. Background
/// `(4, 0, w−8, h)`, "UNISON" label `(0, 5, w, 16)` (Silk-Screen), slot `(0, 5+16+1, w, 48)`;
/// height = RequiredHeight(LARGE, 1, header) = 50 + 11 + 16 (0x18143b330).
fn unison(ui: &mut Ui, state: &crate::model::InsertState, s: &StripInfo, r: Rect) {
    ui.background("Channel-Inserts-Background", Rect::new(r.x + 4.0, r.y, r.w - 8.0, r.h));
    ui.font_text("Silk-Screen", "UNISON", Rect::new(r.x, r.y + 5.0, r.w, 16.0), Align::Center);
    ui.insert_slot(state, Rect::new(r.x, r.y + 22.0, r.w, 48.0), cid(s, Control::Insert(255)));
}

/// `Rack_UADRecord` (resized 0x18149a990) at `(0, y, w, 42)`: the REC/MON LED art
/// (`views:UAD-Record-LEDs`, off = MON lit, on = REC lit) at `(6, −2, 43, h)` and the INS
/// switch (`views:Switch`) at `(w/2, −2, 40, h)`.
fn rec_mon(ui: &mut Ui, monitor_only: bool, s: &StripInfo, y: f32) {
    let h = 42.0;
    ui.toggle("UAD-Record-LEDs", None, !monitor_only, true, Rect::new(6.0, y - 2.0, 43.0, h), cid(s, Control::RecMon), None);
    ui.toggle("Switch", Some("INS"), false, true, Rect::new(STRIP_W / 2.0, y - 2.0, 40.0, h), None, None);
}

fn main_strip(ui: &mut Ui, model: &dyn MixerModel, s: &StripInfo, pos: GroupPos) {
    let g = MainStripGeometry::new(s.kind);
    let st = model.main(s);
    let meters = model.meters(s);
    match s.kind {
        StripKind::Input => {
            ui.background("Channel-Bottom", Rect::new(0.0, 0.0, STRIP_W, g.label.y));
            ui.fill(Rect::new(0.0, g.label.y, STRIP_W, g.label.h), COLUMN_BG);
        }
        StripKind::Aux => {
            ui.fill(Rect::new(0.0, 0.0, STRIP_W, main_strip::HEIGHT), AUX_MAIN_BG);
            ui.fill(Rect::new(0.0, 0.0, STRIP_W, 2.0), SEPARATOR);
            if pos.index > 0 {
                ui.fill(Rect::new(0.0, 5.0, 2.0, g.label.y - 10.0), SEPARATOR);
            }
        }
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
        ui.toggle("Switch", Some("PRE"), st.pre, true, Rect::new(8.0, r.y, 40.0, r.h), cid(s, Control::PrePost), None);
        ui.toggle("Switch", Some("POST"), !st.pre, true, Rect::new(48.0, r.y, 40.0, r.h), cid(s, Control::PrePost), None);
    }
    if let Some(r) = g.mono_mute {
        ui.toggle("Switch", Some("MONO"), st.mono, true, Rect::new(8.0, r.y, 40.0, r.h), cid(s, Control::Mono), None);
        ui.toggle("Switch-Red", Some("MUTE"), st.mute, true, Rect::new(48.0, r.y, 40.0, r.h), cid(s, Control::Mute), None);
    }
    let fader_key = if s.kind == StripKind::Aux { "Main-Bus-Fader" } else { "Main-Input-Fader" };
    ui.fader_and_meters(fader_key, s.stereo, st.fader, meters, &st.readout, g.fader_meters, cid(s, Control::Fader));
    name_plate(ui, model, s, g.label);
}

/// `Rack_ChannelStripLabel` (0x18140b7c0): the name button fills `(0, 0, w, 28)`. Console 1.3.1
/// paints inputs as a flat white plate 27 tall with a `uad2_channel_separator` line at x = −1,
/// auxes as a flat dark plate inset 0.5; "[ ISOLATE ]" (`Isolated-Static-Text`) under the name.
fn name_plate(ui: &mut Ui, model: &dyn MixerModel, s: &StripInfo, r: Rect) {
    let st = model.main(s);
    let aux = s.kind == StripKind::Aux;
    if aux {
        ui.fill(Rect::new(r.x + 0.5, r.y, r.w - 1.0, r.h - 1.0), AUX_PLATE);
    } else {
        ui.fill(Rect::new(r.x, r.y, r.w, r.h - 1.0), Rgba::WHITE);
        ui.fill(Rect::new(r.x - 1.0, r.y + 2.0, 1.0, r.h - 4.0), PLATE_SEPARATOR);
    }
    ui.name_plate(if aux { "Channel-Aux-Name" } else { "Channel-Input-Name" }, &st.name, r);
    if model.isolated(s) {
        let mut style = ui.style("views", "Isolated-Static-Text");
        if aux {
            style.color = Rgba::WHITE.with_alpha(0.5);
        }
        ui.text("[ ISOLATE ]", style, Rect::new(r.x, r.y + 18.0, r.w, 9.0), Align::Center);
    }
    ui.hit(r, ControlId { strip: s.id.clone(), control: Control::Name }, HitKind::Tap);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::surfaces::view::InsertSize;

    /// p2 (1470×1105): the cells area is 77…666 (589). Inputs with Sends collapsed (237 + 288 +
    /// 22) sit on the fader area, header at 119; the aux stack (242 + 340) starts at 84.
    #[test]
    fn short_stacks_anchor_at_the_bottom() {
        let inputs = CellRows { input: Some(INPUT_CELL_H), inserts: cells::inserts_cell_h(4, InsertSize::Large, true), sends: HEADER_H, slots: 4, main: main_strip::HEIGHT };
        assert_eq!(77.0 - cells_offset(&inputs, 589.0, 0.0), 119.0);
        let aux = CellRows { input: None, inserts: cells::inserts_cell_h(4, InsertSize::Large, false), sends: cells::sends_cell_h(4, false), slots: 4, main: main_strip::HEIGHT };
        assert_eq!(77.0 - cells_offset(&aux, 589.0, 0.0), 84.0);
        // p1 (534): both overflow and scroll from the top.
        assert_eq!(cells_offset(&aux, 534.0, 0.0), 0.0);
    }
}
