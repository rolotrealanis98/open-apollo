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
        draw_navigator(ui, model, view, l.nav_x(), W::CONTENT_TOP, W::STATUS_BAR.0, visible_rows);
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
                strip_column(ui, model, view, s, &aux_rows, 0.0, r);
            }
        }
        MonitorPage::ControlRoom => draw_control_room(ui, model, Rect::new(l.aux_x(), W::COLUMN.0, aux_w, col_h)),
    }
    draw_monitor_column(ui, model, view, Rect::new(l.monitor_x(), W::MONITOR_TOP, STRIP_W, W::STATUS_BAR.0 - W::MONITOR_TOP));
    draw_status_bar(ui, model, Rect::new(0.0, W::STATUS_BAR.0, width, W::STATUS_BAR.1));
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
    stroked_text(ui, &title, style, stroke, Rect::new(80.0, 1.0, tab_w, h - 2.0), Align::Center);

    let style = crate::surfaces::navigator::console_style(ui, "views", "Toggle-Feedback-LP");
    let fw = helvetica_bold_width("FEEDBACK", style.size).ceil();
    let b = Rect::new(width - 6.0 - (fw + 31.0), (h - 1.0 - 22.0) / 2.0, fw + 31.0, 22.0);
    ui.text("FEEDBACK", style, Rect::new(b.x + 1.0, b.y, fw + 2.0, b.h), Align::Left);
    if let Some(img) = ui.skin("views", "Toggle-Feedback-LP").and_then(|s| s.image("image-overlay")) {
        ui.image(img, Rect::new(b.max_x() - 3.0 - 26.0, b.y, 26.0, 22.0), None, None, 1.0);
    }
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
