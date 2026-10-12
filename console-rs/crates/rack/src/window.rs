//! The whole Console mixer window (`mixer-geometry.md` §0–§3) and its surfaces.

use houston::{Rect, Rgba};

use crate::draw::Align;
use crate::geometry::{MainStripGeometry, STRIP_W, StripKind, main_strip, window as W};
use crate::input::{Control, ControlId, HitKind, ScrollTarget};
use crate::model::{MixerModel, StripInfo};
use crate::preamp::draw_preamp;
use crate::surfaces::browser::draw_browser;
use crate::surfaces::cells::{self, HEADER_H, INPUT_CELL_H, insert_list, send_list, visible_slots};
use crate::surfaces::monitor::{draw_monitor_column, monitor_rect};
use crate::surfaces::control_room::draw_control_room;
use crate::surfaces::navigator::{self, VisibleRows, draw_navigator};
use crate::surfaces::popover::draw_popover;
use crate::surfaces::session_nav::draw_session_navigator;
use crate::surfaces::panels::draw_panel;
use crate::surfaces::settings::draw_settings_sheet;
use crate::surfaces::status::draw_status_bar_with;
use crate::surfaces::{InsertSize, Section, Sheet, ViewState};
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
    /// Strips in the bus section: the auxes (SHOW AUX) and the talkback strip (SHOW CTRL ROOM).
    pub auxes: usize,
    pub browser: bool,
    /// The PLUG-IN SCENES panel column, left of the browser.
    pub scenes: bool,
    /// The control-room column before the monitor column (SHOW CTRL ROOM).
    pub ctrl_room: bool,
    /// The navigator column is shown (View > Section > Show Mixer Navigation; full layout).
    pub navigator: bool,
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
/// Between the bus section and the control-room column (user captures s45–s49: the bus ends
/// on 1277, the column starts on 1278 in the 1470-pt window).
const BUS_CTRL_ROOM_GAP: f32 = 1.0;
/// `Rack_PluginScenes` panel column (s10: the browser moves right by 288).
pub const SCENES_W: f32 = 288.0;
/// Horizontal scrollbar under the input strips while they do not all fit (s10): the strip
/// columns, the bus section included, end 18 pt higher.
pub const HSCROLL_H: f32 = 18.0;
const PANEL: Rgba = Rgba::rgb8(72, 76, 78);
const SEPARATOR: Rgba = Rgba::rgb8(82, 87, 89);
/// Main strip height with the fader hidden (F = 5, label 28).
const MAIN_HIDDEN_H: f32 = 5.0 + main_strip::LABEL_H;

impl WindowLayout {
    pub fn new(view: &ViewState, inputs: usize, auxes: usize) -> Self {
        let (w, h) = view.window.unwrap_or(W::SIZE);
        let mut l = Self {
            compact: view.compact,
            inputs,
            auxes,
            // The SESSION box's CURRENT SESSION panel takes the browser column (sess-panel).
            browser: (view.show_browser || view.session_panel.open) && !view.compact,
            navigator: view.show_navigator,
            scenes: view.show_scenes && !view.compact,
            ctrl_room: view.show_ctrl_room,
            width: w,
            height: h,
            aux_scroll: true,
        };
        let (min_w, min_h) = l.min_size();
        l.width = l.width.max(min_w);
        l.height = l.height.max(min_h);
        l
    }
    /// The layout for `model`'s strips under `view` (bus strips per the SHOW toggles).
    pub fn for_model(model: &dyn MixerModel, view: &ViewState) -> Self {
        Self::new(view, model.inputs().len(), bus_strips(model, view).len())
    }
    /// With the aux scrollbar shown or not (it depends on the aux cells' height).
    pub fn with_aux_scroll(mut self, aux_scroll: bool) -> Self {
        self.aux_scroll = aux_scroll;
        self
    }
    /// Smallest window: one input strip, the bus section and the monitor; the main strip and a
    /// cell header above it.
    pub fn min_size(&self) -> (f32, f32) {
        let w = self.strips_x() + STRIP_W + SCROLLBAR_W + self.bus_w() + BUS_MONITOR_GAP + self.ctrl_room_w() + STRIP_W;
        (w, W::COLUMN.0 + main_strip::HEIGHT + 5.0 + 22.0 + W::STATUS_BAR.1)
    }
    fn aux_w(&self) -> f32 {
        self.auxes as f32 * STRIP_W
    }
    fn ctrl_room_w(&self) -> f32 {
        if self.ctrl_room { STRIP_W } else { 0.0 }
    }
    /// Aux strips plus the scrollbar column when it shows.
    pub fn bus_w(&self) -> f32 {
        self.aux_w() + if self.aux_scroll && self.auxes > 0 { SCROLLBAR_W } else { 0.0 }
    }
    /// Scenes panel column x (full layout).
    pub fn scenes_x(&self) -> f32 {
        0.0
    }
    /// Browser column x (full layout).
    pub fn browser_x(&self) -> f32 {
        if self.scenes { SCENES_W } else { 0.0 }
    }
    /// Navigator column x (full layout).
    pub fn nav_x(&self) -> f32 {
        self.browser_x() + if self.browser { W::BROWSER_W } else { 0.0 }
    }
    pub fn side_x(&self) -> f32 {
        if self.compact { 0.0 } else { self.nav_x() + if self.navigator { W::NAV_COLUMN.1 } else { 0.0 } }
    }
    pub fn strips_x(&self) -> f32 {
        self.side_x() + W::SIDE_SECTION.1
    }
    pub fn aux_x(&self) -> f32 {
        if self.compact {
            self.strips_x() + self.inputs as f32 * STRIP_W + GAP
        } else if self.ctrl_room {
            self.ctrl_room_x() - BUS_CTRL_ROOM_GAP - self.bus_w()
        } else {
            self.monitor_x() - BUS_MONITOR_GAP - self.bus_w()
        }
    }
    /// Control-room column x (with SHOW CTRL ROOM): flush against the monitor column.
    pub fn ctrl_room_x(&self) -> f32 {
        self.monitor_x() - STRIP_W
    }
    pub fn monitor_x(&self) -> f32 {
        if self.compact {
            self.strips_x() + self.inputs as f32 * STRIP_W + GAP + self.aux_w() + GAP + self.ctrl_room_w()
        } else {
            self.width - STRIP_W
        }
    }
    pub fn size(&self) -> (f32, f32) {
        if self.compact { (self.monitor_x() + STRIP_W + 4.0, self.height) } else { (self.width, self.height) }
    }
    /// Top of the status bar (and bottom of the monitor column).
    pub fn status_y(&self) -> f32 {
        self.height - W::STATUS_BAR.1
    }
    /// Input strips that fit between the side section and the aux section.
    pub fn visible_inputs(&self) -> usize {
        if self.compact {
            return self.inputs;
        }
        // Whole strips between the side section and the bus section (p2: 7 in 488…1163.5; s10
        // with the scenes panel and CTRL ROOM: 2 in 776…972, the rest behind the hscroll).
        (((self.aux_x() + 0.5 - self.strips_x()) / STRIP_W).floor().max(1.0) as usize).min(self.inputs)
    }
    /// The input strips overflow sideways: a horizontal scrollbar runs under them.
    pub fn hscroll(&self) -> bool {
        !self.compact && self.visible_inputs() < self.inputs
    }
    /// Bottom of the strip columns (inputs and bus section).
    pub fn strips_bottom(&self) -> f32 {
        self.status_y() - if self.hscroll() { HSCROLL_H } else { 0.0 }
    }
}

/// The bus section's strips: the auxes while SHOW AUX is on, then the talkback strip while
/// SHOW CTRL ROOM is on.
pub fn bus_strips(model: &dyn MixerModel, view: &ViewState) -> Vec<StripInfo> {
    let mut v = if view.show_aux { model.auxes() } else { Vec::new() };
    if view.show_ctrl_room {
        v.extend(model.talkback());
    }
    v
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
    /// Insert slot size of the group.
    pub size: InsertSize,
    /// The inserts cell has the UAD REC row (inputs, and a bus section with the talkback strip).
    pub rec_row: bool,
    /// Main strip (fader area + label) height.
    pub main: f32,
}

impl CellRows {
    pub fn new(model: &dyn MixerModel, view: &ViewState, strips: &[StripInfo], aux: bool) -> Self {
        let sec = view.sections;
        let slots = strips.iter().map(|s| visible_slots(&model.inserts(s), view.fixed_slots)).max().unwrap_or(4);
        let sends = strips.iter().map(|s| model.send_rows(s)).max().unwrap_or(0);
        // The bus section keeps its Sends row open (Console's default collapses only the inputs').
        let open = |r: Section| sec.get(r) || (aux && r == Section::Sends);
        // LARGE / SMALL resize the input inserts only (s47: SMALL, the bus keeps 50-pt slots); the
        // talkback strip has the UAD REC row, so the bus inserts row makes room for it.
        let size = if aux { InsertSize::Large } else { view.insert_size };
        let rec_row = !aux || strips.iter().any(|s| s.kind == StripKind::Talkback);
        let style = cells::SendStyle::of(view.sends_overview, aux);
        Self {
            input: (!aux).then_some(if !open(Section::Inputs) {
                HEADER_H
            } else if size == InsertSize::Small {
                INPUT_CELL_H - cells::INPUT_CELL_SMALL_DH
            } else {
                INPUT_CELL_H
            }),
            inserts: if open(Section::Inserts) { cells::inserts_cell_h(slots, size, rec_row) } else { HEADER_H },
            sends: if open(Section::Sends) { cells::sends_cell_h(sends, style) } else { HEADER_H },
            slots,
            size,
            rec_row,
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
    ui.windowing = view.windowing.clone();
    let inputs = model.inputs();
    let bus = bus_strips(model, view);
    let base = WindowLayout::new(view, inputs.len(), bus.len());
    let bottom = base.strips_bottom();
    let col_h = bottom - W::COLUMN.0;

    // Cell rows. Content that fits is bottom-aligned on the main strips (1.3.1 at 1470 × 1105);
    // content that overflows scrolls, from the top.
    let rows = CellRows::new(model, view, &inputs, false);
    // The aux Sends cell keeps a row for every cue send the engine lists (1.3.1: two cues shown,
    // four reserved, `MixerModel::send_rows`): 242 + 340 = 582 pt (p2: aux header at 84), open
    // whatever the input Sends row does (p1 and p2 show that one collapsed).
    let aux_rows = CellRows::new(model, view, &bus, true);
    let cells_h = col_h - rows.main - 5.0;
    let aux_cells_h = col_h - aux_rows.main - 5.0;
    let aux_overflow = (aux_rows.total() - aux_cells_h).max(0.0);
    let l = base.with_aux_scroll(!bus.is_empty() && aux_overflow > 0.0);
    let (width, height) = l.size();
    let bounds = Rect::new(0.0, 0.0, width, height);
    let status_y = l.status_y();

    ui.fill(bounds, Rgba::rgb8(41, 43, 46));

    let overflow = (rows.total() - cells_h).max(0.0);
    let off = cells_offset(&rows, cells_h, view.cells_scroll);
    let visible_rows = {
        let vis = |s: Section| rows.rows().iter().any(|&(r, y, h)| r == s && y - off + h > 0.0 && y - off < cells_h);
        VisibleRows { inputs: vis(Section::Inputs), inserts: vis(Section::Inserts), sends: vis(Section::Sends) }
    };

    if !l.compact {
        if l.scenes {
            draw_scenes_column(ui, model, view, Rect::new(l.scenes_x(), W::TITLE_BAR, SCENES_W, status_y - W::TITLE_BAR));
        }
        if l.browser {
            let col = Rect::new(l.browser_x(), W::TITLE_BAR, W::BROWSER_W, status_y - W::TITLE_BAR);
            if view.session_panel.open {
                crate::surfaces::session::draw_session_panel(ui, model, view, col);
            } else {
                draw_browser(ui, model, view, col);
            }
        }
        if l.navigator {
            draw_navigator(ui, model, view, l.nav_x(), W::CONTENT_TOP, status_y, visible_rows);
        }
    }

    // Strip area panel and session navigator.
    let visible = l.visible_inputs();
    let first = view.first_strip.min(inputs.len().saturating_sub(visible));
    ui.fill(Rect::new(l.side_x(), W::COLUMN.0, l.aux_x() - l.side_x(), col_h), PANEL);
    // The navigator ends on the bus section's first whole point (p2 1181, s08 972).
    draw_session_navigator(ui, model, Rect::new(l.side_x(), W::CONTENT_TOP, l.aux_x().ceil() - l.side_x(), W::COLUMN.0 - W::CONTENT_TOP), first, visible);
    side_section(ui, &rows, off, Rect::new(l.side_x(), W::COLUMN.0, W::SIDE_SECTION.1, col_h));

    for (i, s) in inputs.iter().enumerate().skip(first).take(visible) {
        let x = l.strips_x() + (i - first) as f32 * STRIP_W;
        let pos = GroupPos { index: i - first, last: i - first + 1 == visible.min(inputs.len() - first) };
        strip_column(ui, model, view, s, &rows, off, pos, Rect::new(x, W::COLUMN.0, STRIP_W, col_h));
    }
    // Empty columns after the last strip, then the cells scrollbar column while they overflow.
    let main_y = W::COLUMN.0 + col_h - rows.main;
    // On a whole point: p1 1596 (bus at 1613.5), s46 971 (bus at 989).
    let scroll_x = (l.aux_x() - SCROLLBAR_W - 0.5).floor();
    let strips_end = l.strips_x() + inputs.len().saturating_sub(first).min(visible) as f32 * STRIP_W;
    empty_columns(ui, strips_end, if overflow > 0.0 { scroll_x } else { l.aux_x() }, main_y, bottom);
    let cells_area = Rect::new(l.side_x(), W::COLUMN.0 + 5.0, l.aux_x() - l.side_x(), cells_h);
    ui.scroll_region(cells_area, ScrollTarget::Cells, overflow);
    if overflow > 0.0 {
        scrollbar_column(ui, scroll_x, main_y, bottom, cells_h, rows.total(), view.cells_scroll, Some(ControlId { strip: "view".into(), control: Control::CellsScroll }), false);
    }
    if l.hscroll() {
        strips_hscroll(ui, &l, first, visible, Rect::new(l.side_x(), bottom, l.aux_x() - l.side_x(), status_y - bottom));
    }

    // Bus section: aux strips, then the talkback strip, then their scrollbar column.
    let aux_off = cells_offset(&aux_rows, aux_cells_h, view.aux_scroll);
    let aux_w = bus.len() as f32 * STRIP_W;
    if l.aux_scroll {
        let aux_main_y = W::COLUMN.0 + col_h - aux_rows.main;
        let id = ControlId { strip: "view".into(), control: Control::AuxCellsScroll };
        // Half a point after the strips before the monitor column (p1/p2); flush against the
        // control-room column, which it meets (s08: 1260…1278).
        let sx = l.aux_x() + aux_w + if l.ctrl_room { 0.0 } else { 0.5 };
        scrollbar_column(ui, sx, aux_main_y, bottom, aux_cells_h, aux_rows.total(), view.aux_scroll, Some(id), true);
        ui.scroll_region(Rect::new(l.aux_x(), W::COLUMN.0 + 5.0, aux_w + SCROLLBAR_W, aux_cells_h), ScrollTarget::AuxCells, aux_overflow);
    }
    for (i, s) in bus.iter().enumerate() {
        let r = Rect::new(l.aux_x() + i as f32 * STRIP_W, W::COLUMN.0, STRIP_W, col_h);
        ui.fill(r, PANEL);
        strip_column(ui, model, view, s, &aux_rows, aux_off, GroupPos { index: i, last: i + 1 == bus.len() }, r);
    }
    if l.hscroll() && !bus.is_empty() {
        // Under the bus section the strips end with the inputs (s10).
        ui.fill(Rect::new(l.aux_x(), bottom, l.bus_w() + 0.5, status_y - bottom), Rgba::rgb8(49, 54, 56));
    }
    if l.ctrl_room {
        draw_control_room(ui, model, Rect::new(l.ctrl_room_x(), W::COLUMN.0, STRIP_W, status_y - W::COLUMN.0));
    }
    draw_monitor_column(ui, model, view, monitor_rect(l.monitor_x(), height));
    draw_status_bar_with(ui, model, view.scene_info, Rect::new(0.0, status_y, width, W::STATUS_BAR.1));
    // Last of the chrome: its bottom line covers the first point of the columns below.
    title_bar(ui, model, view, width);

    // Overlays: Console's separate windows (`crate::layers`). Windowed, they are placed and
    // clamped on the screen rather than inside the mixer window.
    let pb = ui.screen_or(bounds);
    match view.sheet {
        Some(Sheet::Settings) => {
            ui.layer_hint("settings", crate::layers::LayerKind::Window);
            draw_settings_sheet(ui, model, view, pb);
            ui.end_layers();
            if let Some(child) = view.settings.child {
                // Route Presets opens under its field (set-d-iopresets), the I/O Matrix source
                // picker over its row's cell (set-d-insrc), ADD DEVICE centred (set-adddev).
                use crate::settings::Panel as P;
                let field = ui.hit_rect(&ControlId { strip: "settings".into(), control: Control::ShowPanel(child) });
                let at = match child {
                    P::IoPresets => field.map(|f| (f.x, f.max_y())),
                    P::RouteSource { .. } => field.map(|f| (f.x, f.y)),
                    _ => None,
                };
                ui.layer_hint_at(crate::layers::panel_id(child), crate::layers::panel_kind(child), at);
                draw_panel(ui, model, view, child, pb, None);
            }
        }
        Some(Sheet::Panel(p)) => {
            let browser = (l.browser && !l.compact).then(|| Rect::new(0.0, W::TITLE_BAR, W::BROWSER_W, status_y - W::TITLE_BAR));
            ui.layer_hint(crate::layers::panel_id(p), crate::layers::panel_kind(p));
            draw_panel(ui, model, view, p, pb, browser);
        }
        None => {}
    }
    ui.end_layers();
    ui.layer_hint("popover", crate::layers::LayerKind::Popover);
    draw_popover(ui, model, view, pb);
    ui.end_layers();
    ui.clear_layer_hint();
    crate::menu::draw_menu(ui, model, view, pb);
    l
}

/// PLUG-IN SCENES panel column (`SCENES_W`, left of the browser; s10): the window makes the
/// room, `surfaces::panels::plugin_scenes` draws the contents (CURRENT / PRIMED, RECALL / SAVE,
/// the scene list); its X hides the column.
fn draw_scenes_column(ui: &mut Ui, model: &dyn MixerModel, view: &ViewState, r: Rect) {
    ui.fill(Rect::new(r.max_x(), r.y, 1.0, r.h), Rgba::BLACK);
    crate::surfaces::panels::plugin_scenes(ui, model, view, r, Some(r), Control::ShowPluginScenes);
}

/// Horizontal scrollbar under the input strips (s10: track 1047…1065 after a black rule at 1046,
/// thumb over the visible share), dragged or tapped to page the session navigator.
fn strips_hscroll(ui: &mut Ui, l: &WindowLayout, first: usize, visible: usize, r: Rect) {
    let bg = ui.assets.appearance.colors.get("mixer_scrollbar_background").copied().unwrap_or(Rgba::rgb8(35, 36, 40));
    ui.fill(Rect::new(r.x, r.y - 1.0, r.w, 1.0), Rgba::BLACK);
    ui.fill(r, bg);
    // The track starts a point into the strips (s10: thumb at 777 for strips at 776).
    let track = Rect::new(l.strips_x() + 1.0, r.y, (l.aux_x() - l.strips_x() - 1.0).max(1.0), r.h);
    let n = l.inputs.max(1) as f32;
    let thumb = Rect::new(track.x + track.w * first as f32 / n, track.y, (track.w * visible as f32 / n).max(30.0), track.h);
    if let Some(img) = ui.skin("backgrounds", "Small-Vertical-Scroll-Bar").and_then(|k| k.image("image")) {
        ui.image(img, thumb, Some(houston::Insets::uniform(12.0)), None, 1.0);
    }
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

/// Approximate advance width of `text` in Helvetica (regular) at `size` pt (Adobe AFM widths).
pub fn helvetica_width(text: &str, size: f32) -> f32 {
    const W: [u16; 95] = [
        278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278, 278, // ' '…/
        556, 556, 556, 556, 556, 556, 556, 556, 556, 556, 278, 278, 584, 584, 584, 556, // 0…?
        1015, 667, 667, 722, 722, 667, 611, 778, 722, 278, 500, 667, 556, 833, 722, 778, // @…O
        667, 778, 722, 667, 611, 722, 667, 944, 667, 667, 611, 278, 278, 278, 469, 556, // P…_
        333, 556, 556, 500, 556, 556, 278, 556, 556, 222, 222, 500, 222, 833, 556, 556, // `…o
        556, 556, 333, 500, 278, 556, 500, 722, 500, 500, 500, 334, 260, 334, 584, // p…~
    ];
    text.chars().map(|c| W.get((c as usize).wrapping_sub(32)).copied().unwrap_or(556) as f32).sum::<f32>() * size / 1000.0
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
///
/// With `ViewState::menu_bar` (desktop shells without a native menu bar) Windows Console's menu
/// bar takes the window buttons' place and the session tab follows it.
fn title_bar(ui: &mut Ui, model: &dyn MixerModel, view: &ViewState, width: f32) {
    let h = W::CONTENT_TOP;
    let bg = ui.skin("backgrounds", "Main-Title-Bar-LP").and_then(|s| s.color("color-overlay")).unwrap_or(Rgba::rgb8(75, 79, 83));
    ui.fill(Rect::new(0.0, 0.0, width, h), bg);
    ui.fill(Rect::new(0.0, 0.0, width, 1.0), Rgba::BLACK);
    ui.fill(Rect::new(0.0, h - 1.0, width, 1.0), Rgba::BLACK);
    let tab_x = if view.menu_bar {
        (crate::menu::draw_menu_bar(ui, model, view, crate::menu::BAR_X, h) + 12.0).max(80.0)
    } else {
        for (i, key) in ["Window-Button-Close", "Window-Button-Min", "Window-Button-Max-Console"].into_iter().enumerate() {
            if let Some(img) = ui.skin("views", key).and_then(|s| s.image("image")) {
                ui.image(img, Rect::new(10.0 + i as f32 * 25.0, 7.0, 12.0, 12.0), None, None, 0.2);
            }
        }
        80.0
    };
    let title = format!("Apollo: {}", model.session_name());
    let style = ui.style("views", "Session-Tab-Single");
    let tab_w = helvetica_bold_width(&title, style.size).ceil() + 26.0;
    ui.background("Titlebar-Tab-On", Rect::new(tab_x, 0.0, tab_w, h));
    ui.background("Titlebar-Tab-Divider", Rect::new(tab_x + tab_w - 3.0, 0.0, 3.0, h));
    let stroke = ui.skin("views", "Session-Tab").and_then(|s| s.color("color-text-stroke")).unwrap_or(Rgba::BLACK.with_alpha(0.4));
    stroked_text(ui, &title, style, stroke, Rect::new(tab_x, 2.0, tab_w, h - 2.0), Align::Center);

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
    // Column edges: the last point of each 96-pt column (p1: 1159, 1255, … after strips ending at 1160).
    let mut x = x0;
    while x < x1 {
        ui.fill(Rect::new(x - 1.0, top, 1.0, 4.0), Rgba::BLACK);
        if x > x0 {
            ui.fill(Rect::new(x - 1.0, top + 10.0, 1.0, main_y - top - 15.0), MAIN);
            ui.fill(Rect::new(x - 1.0, main_y, 1.0, status_y - main_y), CELLS);
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
    let thumb_h = (track.h * visible / content.max(1.0)).clamp(30.0f32.min(track.h.max(0.0)), track.h.max(0.0));
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

/// `Rack_ChannelStripsSideSection` (47 pt) and the reveal column (`console_reveal.json`):
/// white swatch, per-row Mixer-Row-Header with the reveal chevron at (17, 5) and, on open
/// rows, the yellow row icon at (15, 35); the main area's `fader_and_meters_bg` with
/// S (clear solos) / OL (clear clips) at main + 63 / + 87, and the white label plate.
/// Measured on Console 1.3.1 (p1 1920 × 1050, p2 1470 × 1105).
fn side_section(ui: &mut Ui, rows: &CellRows, off: f32, r: Rect) {
    const ROW_ICON: Rgba = Rgba::rgb8(246, 201, 91);
    const ROW_ICON_EDGE: Rgba = Rgba::rgb8(44, 46, 48);
    const ROW_ICON_SMALL: Rgba = Rgba::rgb8(144, 155, 161);
    let main_y = r.h - rows.main;
    // Measured on p1 and the 1470-pt user shot: the cells column is (76, 80, 83) between a 1-pt
    // channel_strip_separator_line and a black right edge (x 487, against the first strip's own
    // black edge) with a half-point shade before it.
    const SIDE_BG: Rgba = Rgba::rgb8(76, 80, 83);
    ui.at(r.x, r.y, |ui| {
        // Swatch row as on a strip: black, white swatch 1 pt in.
        ui.fill(Rect::new(0.0, 0.0, r.w, 5.0), Rgba::BLACK);
        ui.fill(Rect::new(1.0, 0.0, r.w - 2.0, 4.0), Rgba::WHITE);
        ui.fill(Rect::new(0.0, 5.0, r.w, main_y - 5.0), SIDE_BG);
        ui.fill(Rect::new(0.0, 5.0, 1.0, main_y - 5.0), SEPARATOR);
        ui.fill(Rect::new(r.w - 1.5, 5.0, 0.5, main_y - 5.0), Rgba::rgb8(71, 75, 79));
        ui.fill(Rect::new(r.w - 1.0, 5.0, 1.0, main_y - 5.0), Rgba::BLACK);
        ui.clip(Some(Rect::new(0.0, 5.0, r.w, main_y - 5.0)));
        ui.at(0.0, 5.0 - off, |ui| {
            for (sec, y, h) in rows.rows() {
                ui.background("Mixer-Row-Header", Rect::new(0.0, y, r.w, HEADER_H));
                let open = rows_open(rows, sec);
                if let Some(img) = ui.skin("views", "Browser-Reveal-Toggle").and_then(|k| k.image(if open { "image-on" } else { "image-off" })) {
                    ui.image(img, Rect::new(17.0, y + 5.0, 12.0, 12.0), None, None, 1.0);
                }
                if open && h >= 50.0 {
                    // Yellow in LARGE, blue-grey in SMALL (s47/s48: (144, 155, 161)).
                    let c = if rows.size == InsertSize::Small { ROW_ICON_SMALL } else { ROW_ICON };
                    let icon = Rect::new(15.0, y + 35.0, 16.0, 15.0);
                    ui.fill(icon, ROW_ICON_EDGE);
                    ui.fill(Rect::new(icon.x + 1.0, icon.y + 1.0, 14.0, 6.0), c);
                    ui.fill(Rect::new(icon.x + 1.0, icon.y + 8.0, 14.0, 6.0), c);
                }
                ui.hit(Rect::new(0.0, y, r.w, HEADER_H), ControlId { strip: navigator::NAV_ID.into(), control: Control::Section(sec) }, HitKind::Toggle);
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
        // Label plate 1 pt in from a black left edge, black below (the first strip's plate
        // separator falls on its last point); measured on p1.
        let plate_h = main_strip::LABEL_H - 1.0;
        ui.fill(Rect::new(0.0, label_y, 1.0, plate_h + 1.0), Rgba::BLACK);
        ui.fill(Rect::new(1.0, label_y, r.w - 1.0, plate_h), Rgba::WHITE);
        ui.fill(Rect::new(0.0, label_y + plate_h, r.w, 1.0), Rgba::BLACK);
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
        // s46 (clipboard capture, (189, 158, 0) there next to AUX 1 at (196, 126, 0)).
        StripKind::Talkback => Rgba::rgb8(181, 157, 45),
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
                ui.fill(Rect::new(0.0, 5.0, STRIP_W, main_strip::LABEL_H), if s.kind.is_bus() { AUX_MAIN_BG } else { COLUMN_BG });
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
    let bus = s.kind.is_bus();
    let mut y = 0.0;
    if let Some(h) = rows.input {
        if h > HEADER_H {
            cell_separators(ui, y, h);
        }
        ui.cell_header(&s.title, false, false, Rect::new(0.0, 0.0, STRIP_W, HEADER_H));
        if h > HEADER_H {
            input_cell(ui, model, s, rows.size);
        }
        y += h;
    }
    if rows.inserts > HEADER_H && !bus {
        cell_separators(ui, y, rows.inserts);
    }
    ui.cell_header("INSERTS", true, bus, Rect::new(0.0, y, STRIP_W, HEADER_H));
    if rows.inserts > HEADER_H {
        let slots = model.inserts(s);
        let targeted = match &view.browser.assign_to {
            Some(ControlId { strip, control: Control::Insert(k) }) if *strip == s.id => Some(*k),
            _ => None,
        };
        insert_list(ui, s, &slots, &model.insert_labels(s), rows.slots, rows.size, targeted, y + 26.0);
        if s.kind != StripKind::Aux && rows.rec_row {
            rec_mon(ui, model.records_pre_effects(s), s, y + 26.0 + cells::inserts_list_h(rows.slots, rows.size) + 4.0);
        }
    }
    y += rows.inserts;
    if rows.sends > HEADER_H && !bus {
        cell_separators(ui, y, rows.sends);
    }
    ui.cell_header("SENDS", false, bus, Rect::new(0.0, y, STRIP_W, HEADER_H));
    if bus {
        // The aux's SENDS header opens its sends window (`Rack_SendsPopover`, capture s51).
        let k = s.id.trim_end_matches(|c: char| !c.is_ascii_digit()).chars().rev().take_while(|c| c.is_ascii_digit()).collect::<String>().chars().rev().collect::<String>().parse::<u8>().unwrap_or(0);
        let of = if s.kind == StripKind::Talkback { crate::settings::SendsOf::Talkback } else { crate::settings::SendsOf::Aux(k) };
        ui.hit(Rect::new(0.0, y, STRIP_W, HEADER_H), ControlId { strip: s.id.clone(), control: Control::ShowPanel(crate::settings::Panel::Sends(of)) }, HitKind::Tap);
    }
    if rows.sends > HEADER_H {
        let sends = if s.kind == StripKind::Aux { model.sends(s) } else { model.input_sends(s) };
        let well_h = (visible_h - y - 26.0).max(rows.sends - 31.0);
        send_list(ui, s, &sends, &|k| model.send_pre(s, k), view, y + 26.0, well_h);
    }
}

/// Input cell (`Rack_ChannelStripCell_Input::resized` 0x1814b90a0): Unison slot from y = 25,
/// then the preamp overview at 101 (measured in p1; `Resized_InputOrInstrument` 0x1814aeb80).
fn input_cell(ui: &mut Ui, model: &dyn MixerModel, s: &StripInfo, size: InsertSize) {
    // SMALL: the Unison block is 27 shorter (a 23-pt slot), the preamp moves up with it.
    let dy = if size == InsertSize::Small { cells::INPUT_CELL_SMALL_DH } else { 0.0 };
    if let Some(layout) = &s.preamp_layout {
        unison(ui, &model.unison(s), model.unison_label(s).as_ref(), s, size, Rect::new(0.0, 25.0, STRIP_W, 77.0 - dy));
        if let Some(state) = model.preamp(s) {
            // Rack_Overview_Preamp's secondary_background (Secondary-Channel-Inserts-Background)
            // runs 2 pt past the 129-pt plate, as under SR CONVERT.
            ui.background("Secondary-Channel-Inserts-Background", Rect::new(4.0, 101.0 - dy, STRIP_W - 8.0, 131.0));
            draw_preamp(ui, layout, &state, &s.id, Rect::new(0.0, 101.0 - dy, STRIP_W, 129.0));
        }
    } else if s.shows_sr_convert {
        sr_convert(ui, Rect::new(0.0, 167.0 - dy, STRIP_W, 65.0));
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
fn unison(ui: &mut Ui, state: &crate::model::InsertState, label: Option<&crate::model::InsertLabel>, s: &StripInfo, size: InsertSize, r: Rect) {
    ui.background("Channel-Inserts-Background", Rect::new(r.x + 4.0, r.y, r.w - 8.0, r.h));
    ui.font_text("Silk-Screen", "UNISON", Rect::new(r.x, r.y + 5.0, r.w, 16.0), Align::Center);
    let slot = Rect::new(r.x, r.y + 22.0, r.w, size.pitch() - 2.0);
    match size {
        InsertSize::Large => ui.insert_slot(state, label, slot, cid(s, Control::Insert(255))),
        InsertSize::Small => cells::small_slot(ui, state, label, slot, cid(s, Control::Insert(255))),
    }
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
        StripKind::Aux | StripKind::Talkback => {
            ui.fill(Rect::new(0.0, 0.0, STRIP_W, main_strip::HEIGHT), AUX_MAIN_BG);
            if let Some(bottom) = s.aux_bottom_skin() {
                ui.background(bottom, Rect::new(0.0, 0.0, STRIP_W, g.label.y));
            }
            ui.fill(Rect::new(0.0, 0.0, STRIP_W, 2.0), SEPARATOR);
            if pos.index > 0 {
                ui.fill(Rect::new(0.0, 5.0, 2.0, g.label.y - 10.0), SEPARATOR);
            }
        }
    }
    if let Some(p) = g.pans {
        let half = (p.w / 2.0).floor();
        let pan = s.pan_skin();
        if s.stereo {
            ui.knob(pan, st.pan_l, Rect::new(p.x, p.y, half, p.h), cid(s, Control::PanL), Some(0.0));
            ui.knob(pan, st.pan_r, Rect::new(p.x + half, p.y, half, p.h), cid(s, Control::PanR), Some(1.0));
        } else {
            ui.knob(pan, st.pan_l, Rect::new(p.x + (half / 2.0).floor(), p.y, half, p.h), cid(s, Control::PanL), Some(0.5));
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
    if let Some((talk, to_mons)) = g.talkback {
        let on = model.monitor_options().talkback.unwrap_or(false);
        ui.toggle("Switch-Large", Some("TALK"), on, true, talk, Some(ControlId { strip: "monitor".into(), control: Control::Talkback }), None);
        ui.toggle("Switch-Large", Some("TALKBACK\nTO MONITOR"), false, true, to_mons, None, None);
    }
    if let Some(r) = g.mono_mute {
        ui.toggle("Switch", Some("MONO"), st.mono, true, Rect::new(8.0, r.y, 40.0, r.h), cid(s, Control::Mono), None);
        ui.toggle("Switch-Red", Some("MUTE"), st.mute, true, Rect::new(48.0, r.y, 40.0, r.h), cid(s, Control::Mute), None);
    }
    ui.fader_and_meters(s.fader_skin(), s.stereo, st.fader, meters, &st.readout, g.fader_meters, cid(s, Control::Fader));
    name_plate(ui, model, s, g.label);
}

/// `Rack_ChannelStripLabel` (0x18140b7c0): the name button fills `(0, 0, w, 28)`. Console 1.3.1
/// paints inputs as a flat white plate 27 tall with a `uad2_channel_separator` line at x = −1,
/// auxes as a flat dark plate inset 0.5; "[ ISOLATE ]" (`Isolated-Static-Text`) under the name.
fn name_plate(ui: &mut Ui, model: &dyn MixerModel, s: &StripInfo, r: Rect) {
    let st = model.main(s);
    let aux = s.kind.is_bus();
    if aux {
        ui.fill(Rect::new(r.x + 0.5, r.y, r.w - 1.0, r.h - 1.0), AUX_PLATE);
    } else {
        ui.fill(Rect::new(r.x, r.y, r.w, r.h - 1.0), Rgba::WHITE);
        ui.fill(Rect::new(r.x - 1.0, r.y + 2.0, 1.0, r.h - 4.0), PLATE_SEPARATOR);
    }
    let key = match s.kind {
        StripKind::Input => "Channel-Input-Name",
        StripKind::Aux => "Channel-Aux-Name",
        StripKind::Talkback => "Channel-Talkback-Name",
    };
    ui.name_plate(key, &st.name, r);
    // The talkback strip carries `Isolate` in the engine but shows no ISOLATE line (s45).
    if s.kind != StripKind::Talkback && model.isolated(s) {
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
        let inputs = CellRows { input: Some(INPUT_CELL_H), inserts: cells::inserts_cell_h(4, InsertSize::Large, true), size: InsertSize::Large, rec_row: true, sends: HEADER_H, slots: 4, main: main_strip::HEIGHT };
        assert_eq!(77.0 - cells_offset(&inputs, 589.0, 0.0), 119.0);
        let aux = CellRows { input: None, inserts: cells::inserts_cell_h(4, InsertSize::Large, false), size: InsertSize::Large, rec_row: false, sends: cells::sends_cell_h(4, cells::SendStyle::Medium), slots: 4, main: main_strip::HEIGHT };
        assert_eq!(77.0 - cells_offset(&aux, 589.0, 0.0), 84.0);
        // p1 (534): both overflow and scroll from the top.
        assert_eq!(cells_offset(&aux, 534.0, 0.0), 0.0);
    }

    fn layout(window: (f32, f32), aux_scroll: bool) -> WindowLayout {
        let view = ViewState { window: Some(window), ..ViewState::default() };
        WindowLayout::new(&view, 7, 2).with_aux_scroll(aux_scroll)
    }

    #[test]
    fn columns_follow_the_window_size() {
        // p1 refpair, 1920 × 1050: the aux cells overflow, so the bus section has its scrollbar.
        let l = layout((1920.0, 1050.0), true);
        assert_eq!((l.nav_x(), l.side_x(), l.strips_x()), (345.0, 441.0, 488.0));
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
