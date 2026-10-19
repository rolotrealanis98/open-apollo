//! Plug-in browser panel (left, 345 pt; `Rack_PluginsBrowser` in `Rack_PrimaryBrowserPages`):
//! PLUG-INS / PRESETS tabs, search header, CATEGORIES section with FAVORITES and the category
//! grid, the view-mode bar (`Large-Icon-Toggle`), and the list: the dismissable assign hint,
//! the "Universal Audio" / "Universal Audio (Inactive)" groups and their rows (icon, name,
//! `Plugin-Badge-*`), and the scrollbar.
//!
//! Geometry: `Rack_PluginsBrowser::resized` (0x1812f3360: 5 pt margins, view bar 27 pt, list
//! to the bottom − 5), `Rack_PluginsBrowserCategories::resized` (0x1812f4e60: cells
//! `(w − 5) / 3 − 4` wide, 23 tall, 27 apart; the last column runs to `w − 6`),
//! `Rack_PluginsBrowserCategoriesSection::resized` (header 30 pt),
//! `Rack_DismissableBrowserHelpText` (`tag_bg_on` background, close button at the top
//! right), `browser_search_header.json`; every offset checked against Console 1.3.1
//! (refpairs/p1, 2× shot). Console 1.3.1 paints the panels (search field, sections, list,
//! category cells) itself as rounded rectangles; their colours and corner coverage are taken
//! from the shot (the corner profile is the one of UA's `browser_header_bg.png`).

use houston::{Rect, Rgba};

use crate::draw::{Align, TextStyle};
use crate::input::{Control, ControlId, HitKind, ScrollTarget};
use crate::model::{MixerModel, PluginEntry};
use crate::surfaces::view::{BrowserTab, TextTarget, ViewState};
use crate::ui::Ui;

pub const BROWSER_ID: &str = "browser";

/// `Browser` background (`backgrounds/browser_bg.png`), under every panel.
const BG: Rgba = Rgba::rgb8(71, 76, 79);
/// Assign mode (s08).
const ASSIGN_BG: Rgba = Rgba::rgb8(115, 123, 128);
/// Search field and list.
const WELL: Rgba = Rgba::rgb8(29, 30, 32);
const WELL_EDGE: Rgba = Rgba::rgb8(23, 24, 25);
/// CATEGORIES section and the view-mode bar.
const SECTION: Rgba = Rgba::rgb8(49, 51, 54);
const SECTION_EDGE: Rgba = Rgba::rgb8(43, 46, 47);
/// Category cells.
const CELL: Rgba = Rgba::rgb8(34, 36, 38);
const CELL_ON: Rgba = Rgba::rgb8(64, 68, 71);
const CELL_EDGE: Rgba = Rgba::rgb8(27, 29, 30);
pub(crate) const AMBER: Rgba = Rgba::rgb8(255, 186, 0);

const MARGIN: f32 = 5.0;
const TAB_Y: f32 = 6.0;
const TAB_H: f32 = 26.0;
const SEARCH_Y: f32 = 37.0;
const SEARCH_H: f32 = 28.0;
const SECTION_HEADER_H: f32 = 30.0;
const CELL_H: f32 = 23.0;
const CELL_PITCH: f32 = 27.0;
const VIEW_BAR_H: f32 = 27.0;
const HINT_H: f32 = 54.0;
const GROUP_H: f32 = 32.0;
const ROW_H: f32 = 48.0;
const SMALL_ROW_H: f32 = 24.0;
/// `Small-Vertical-Scroll-Bar` width.
const SCROLLBAR_W: f32 = 18.0;
/// Plug-in icons (400×160 art) in large rows.
const ICON: (f32, f32) = (95.0, 38.0);

fn bid(control: Control) -> ControlId {
    ControlId { strip: BROWSER_ID.into(), control }
}

/// Helvetica bold at an em `size` in points. Console's browser fonts are `font-size-dyn` skins
/// (e.g. `Browser-Category-*`, 10 + 1 = 11; houston::fonts): measured on refpair p1, list names,
/// headers and category labels are 11, the tabs 12.
fn bold(size: f32, color: Rgba) -> TextStyle {
    Ui::plain_style("Helvetica", size, true, color)
}

/// Catalog indices shown for the current filters (category, favorites, search).
pub fn filtered(catalog: &[PluginEntry], categories: &[String], view: &ViewState) -> Vec<usize> {
    let b = &view.browser;
    let cat = b.category.and_then(|i| categories.get(i as usize));
    let q = b.search.trim().to_lowercase();
    catalog
        .iter()
        .enumerate()
        .filter(|(_, p)| cat.is_none_or(|c| p.in_category(c)))
        .filter(|(_, p)| !b.favorites_only || p.favorite)
        .filter(|(_, p)| q.is_empty() || p.name.to_lowercase().contains(&q))
        .map(|(i, _)| i)
        .collect()
}

/// Height of the CATEGORIES section for `count` categories in 3 columns.
fn section_height(open: bool, count: usize) -> f32 {
    if !open || count == 0 {
        return SECTION_HEADER_H;
    }
    let rows = count.div_ceil(3) as f32;
    SECTION_HEADER_H + (rows - 1.0) * CELL_PITCH + CELL_H + 4.0
}

/// Draws the browser into `r` (window points).
pub fn draw_browser(ui: &mut Ui, model: &dyn MixerModel, view: &ViewState, r: Rect) {
    let b = &view.browser;
    // An insert's "Assign..." (capture s08): a lighter browser titled with the slot, and a
    // CHOOSE PLUG-IN panel instead of the hint.
    let assign = b.assign_to.as_ref().map(|id| assign_title(model, id)).or_else(|| b.preset_target.as_ref().map(|(strip, scope)| preset_title(model, strip, *scope)));
    let bg = if assign.is_some() { ASSIGN_BG } else { BG };
    ui.fill(r, bg);
    if assign.is_none() {
        ui.background("Browser", r);
    }
    ui.at(r.x, r.y, |ui| {
        let (w, h) = (r.w, r.h);
        let inner = w - 2.0 * MARGIN;
        if let Some(title) = &assign {
            let band = Rect::new(0.0, 0.0, w, SEARCH_Y - 4.0);
            ui.text(title, bold(12.0, Rgba::rgb8(240, 241, 242)), band, Align::Center);
            let close = Rect::new(w - MARGIN - 24.0, (band.h - 24.0) / 2.0, 24.0, 24.0);
            if let Some(x) = ui.skin("views", "Browser-Modal-Close-White").and_then(|s| s.image("image")) {
                let x = x.to_string();
                ui.image(&x, Rect::new(close.x + 5.0, close.y + 5.0, 14.0, 14.0), None, None, 1.0);
            }
            ui.hit(close, bid(Control::Menu(crate::menu::MenuCmd::AssignCancel)), HitKind::Tap);
        }
        // Tabs (`Tab-Left` / `Tab-Right`, art stretched to the tab).
        let tw = (inner / 2.0).floor();
        let tabs = [
            (BrowserTab::Plugins, "PLUG-INS", "Tab-Left", Rect::new(MARGIN, TAB_Y, tw, TAB_H)),
            (BrowserTab::Presets, "PRESETS", "Tab-Right", Rect::new(MARGIN + tw, TAB_Y, inner - tw, TAB_H)),
        ];
        for (t, label, key, tr) in tabs {
            if assign.is_some() {
                break;
            }
            let on = b.tab == t;
            if let Some(img) = ui.skin("views", key).and_then(|s| if on { s.image("image-on") } else { s.image("image-off") }) {
                let img = img.to_string();
                ui.image(&img, tr, None, None, 1.0);
            }
            let color = if on { Rgba::rgb8(220, 221, 222) } else { Rgba::rgb8(103, 105, 107) };
            ui.text(label, bold(12.0, color), tr, Align::Center);
            ui.hit(tr, bid(Control::BrowserTab(t)), HitKind::Tap);
        }
        // Search header (`browser_search_header.json`).
        let sr = Rect::new(MARGIN, SEARCH_Y, inner, SEARCH_H);
        rounded(ui, sr, WELL, WELL_EDGE, bg, &UA_CORNER, [true; 4]);
        if let Some(icon) = ui.skin("views", "Browser-Search-Icon").and_then(|s| s.image("image")) {
            let icon = icon.to_string();
            ui.image(&icon, Rect::new(sr.x + 9.0, sr.y + 6.0, 16.0, 16.0), None, None, 1.0);
        }
        let focused = matches!(&view.text_edit, Some(t) if t.target == TextTarget::Search);
        let style = ui.style("views", "Browser-Text-Edit-Field");
        let text = if focused { format!("{}|", b.search) } else { b.search.clone() };
        if !text.is_empty() {
            ui.text(&text, style, Rect::new(sr.x + 30.0, sr.y, sr.w - 36.0, sr.h), Align::Left);
        }
        ui.hit(sr, bid(Control::BrowserSearch), HitKind::Tap);

        let mut y = sr.max_y() + MARGIN;
        if b.assign_to.is_some() {
            // s08: a 2-pt white rule, then a (90,96,99) panel 120 pt high with CHOOSE PLUG-IN
            // 8 pt down and a dashed drop box.
            let panel = Rect::new(MARGIN, y + 2.0, inner, 120.0);
            ui.fill(Rect::new(panel.x, y, panel.w, 2.0), Rgba::WHITE);
            ui.fill(panel, Rgba::rgb8(90, 96, 99));
            ui.text("CHOOSE PLUG-IN", bold(12.0, Rgba::rgb8(240, 241, 242)), Rect::new(panel.x, panel.y + 8.0, panel.w, 16.0), Align::Center);
            let dash = Rect::new(panel.x + (panel.w - 120.0) / 2.0, panel.y + 34.0, 120.0, 48.0);
            dashed(ui, dash, Rgba::rgb8(20, 22, 23));
            y = panel.max_y() + MARGIN + 3.0;
        }
        if b.tab == BrowserTab::Presets {
            presets_tab(ui, model, view, Rect::new(MARGIN, y, inner, h - y - MARGIN));
            return;
        }
        // CATEGORIES section.
        let cats = model.plugin_categories();
        let sec = Rect::new(MARGIN, y, inner, section_height(b.categories_open, cats.len()));
        rounded(ui, sec, SECTION, SECTION_EDGE, bg, &UA_CORNER, [true; 4]);
        reveal(ui, b.categories_open, Rect::new(sec.x + 1.0, sec.y + 5.0, 20.0, 20.0));
        let head = bold(11.0, Rgba::rgb8(227, 227, 227));
        ui.text("CATEGORIES", head.clone(), Rect::new(sec.x + 21.0, sec.y, 120.0, SECTION_HEADER_H), Align::Left);
        ui.hit(Rect::new(sec.x, sec.y, 160.0, SECTION_HEADER_H), bid(Control::BrowserCategoriesReveal), HitKind::Toggle);
        let fav_on = b.favorites_only;
        let star = Rect::new(sec.max_x() - 26.0, sec.y + 6.0, 17.0, 16.0);
        let fav_color = if fav_on { AMBER } else { head.color };
        ui.text("FAVORITES", TextStyle { color: fav_color, ..head }, Rect::new(star.x - 122.0, sec.y, 120.5, SECTION_HEADER_H), Align::Right);
        if let Some(img) = ui.skin("views", "Favorite").and_then(|s| if fav_on { s.image("image-on") } else { s.image("image-off") }) {
            let img = img.to_string();
            ui.image(&img, star, None, None, 1.0);
        }
        ui.hit(Rect::new(star.x - 122.0, sec.y, 150.0, SECTION_HEADER_H), bid(Control::BrowserFavorites), HitKind::Toggle);
        if b.categories_open {
            // `Rack_PluginsBrowserCategories` content, inside the section below its header.
            let cw = ((sec.w - 5.0) / 3.0 - 4.0).floor();
            for (i, c) in cats.iter().enumerate() {
                let (col, row) = (i % 3, i / 3);
                let x = 5.0 + col as f32 * (cw + 4.0);
                let cell_w = if col == 2 { sec.w - 6.0 - x } else { cw };
                let cr = Rect::new(sec.x + x, sec.y + SECTION_HEADER_H + row as f32 * CELL_PITCH, cell_w, CELL_H);
                category_cell(ui, c, b.category == Some(i as u8), cr);
                ui.hit(cr, bid(Control::BrowserCategory(i as u8)), HitKind::Toggle);
            }
        }
        y = sec.max_y() + MARGIN;

        // View-mode bar: rounded top, joined to the list below.
        let bar = Rect::new(MARGIN, y, inner, VIEW_BAR_H);
        rounded(ui, bar, SECTION, SECTION_EDGE, bg, &UA_CORNER, [true, true, false, false]);
        let toggle = Rect::new(bar.x + 6.0, bar.y + 6.0, 16.0, 15.0);
        if let Some(img) = ui.skin("views", "Large-Icon-Toggle").and_then(|s| if b.large_rows { s.image("image-on") } else { s.image("image-off") }) {
            let img = img.to_string();
            ui.image(&img, toggle, None, None, 1.0);
        }
        ui.hit(toggle.inset(-4.0, -4.0, -4.0, -4.0), bid(Control::BrowserLargeRows), HitKind::Toggle);
        y = bar.max_y();

        // List: rounded bottom, square top.
        let list = Rect::new(MARGIN, y, inner, h - y - MARGIN);
        rounded(ui, list, WELL, WELL_EDGE, bg, &UA_CORNER, [false, false, true, true]);
        y += MARGIN;
        if b.hint && assign.is_none() {
            let hr = Rect::new(list.x + MARGIN, y, list.w - 2.0 * MARGIN, HINT_H);
            ui.image_stretched("uapw/tag_bg_on.png", hr, 1.0);
            let hs = bold(11.0, Rgba::rgb8(227, 227, 228));
            ui.text("DRAG TO AN INSERT TO ASSIGN", hs.clone(), Rect::new(hr.x, hr.y + 5.75, hr.w, 20.0), Align::Center);
            ui.text("RIGHT CLICK TO ENTER ASSIGN MODE", hs, Rect::new(hr.x, hr.y + 27.75, hr.w, 20.0), Align::Center);
            // `Browser-Modal-Close-White`: 20 pt button, art in its 3,3,4,4 background insets.
            let close = Rect::new(hr.max_x() - 20.0, hr.y, 20.0, 20.0);
            if let Some(x) = ui.skin("views", "Browser-Modal-Close-White").and_then(|s| s.image("image")) {
                let x = x.to_string();
                ui.image(&x, Rect::new(close.x + 3.0, close.y + 3.0, 13.0, 13.0), None, None, 1.0);
            }
            ui.hit(close, bid(Control::BrowserHintClose), HitKind::Tap);
            y = hr.max_y() + MARGIN;
        }
        let catalog = model.plugin_catalog();
        let selected_hint = b.selected.and_then(|i| catalog.get(i as usize)).and_then(|p| p.native_hint.as_deref());
        if let Some(notice) = model.plugin_notice().or(selected_hint) {
            // Keep a visible reason even if the original assign hint was dismissed.
            let width = list.w - 2.0 * MARGIN;
            for line in wrap(notice, ((width / 5.5) as usize).max(12)).iter().take(4) {
                ui.text(line, bold(10.0, AMBER), Rect::new(list.x + MARGIN, y, width, 15.0), Align::Left);
                y += 15.0;
            }
            y += MARGIN;
        }
        plugin_list(ui, model, view, Rect::new(list.x, y, list.w, list.max_y() - MARGIN - y));
    });
}

/// `text` in lines of at most `chars` characters (a longer word gets a line of its own).
pub(crate) fn wrap(text: &str, chars: usize) -> Vec<String> {
    let mut lines = vec![String::new()];
    for word in text.split_whitespace() {
        let last = lines.last_mut().expect("one line");
        if !last.is_empty() && last.len() + word.len() + 1 > chars {
            lines.push(word.to_owned());
        } else {
            if !last.is_empty() {
                last.push(' ');
            }
            last.push_str(word);
        }
    }
    lines
}

/// The browser's title in assign mode: "<channel>: INSERT n" (s08 "AUX 1: INSERT 2") or
/// "<channel>: UNISON".
fn assign_title(model: &dyn MixerModel, id: &ControlId) -> String {
    let strip = model.inputs().into_iter().chain(model.auxes()).find(|s| s.id == id.strip);
    let name = strip.map(|s| {
        let n = model.main(&s).name;
        if n.is_empty() { s.title.clone() } else { n }
    });
    let slot = match id.control {
        Control::Insert(255) => "UNISON".to_string(),
        Control::Insert(k) => format!("INSERT {}", k as u16 + 1),
        _ => String::new(),
    };
    format!("{}: {slot}", name.unwrap_or_default().to_uppercase())
}

/// The browser's title while the PRESETS tab is targeted: "<channel>: CHANNEL PRESET" or the
/// insert's "<channel>: INSERT n" (Rack's `channel_preset_assign_browser`; not captured).
fn preset_title(model: &dyn MixerModel, strip: &std::sync::Arc<str>, scope: crate::presets::PresetScope) -> String {
    match scope {
        crate::presets::PresetScope::Channel => {
            let name = model.inputs().into_iter().chain(model.auxes()).find(|s| s.id == *strip).map(|s| {
                let n = model.main(&s).name;
                if n.is_empty() { s.title.clone() } else { n }
            });
            format!("{}: CHANNEL PRESET", name.unwrap_or_default().to_uppercase())
        }
        crate::presets::PresetScope::Insert(k) => assign_title(model, &ControlId { strip: strip.clone(), control: Control::Insert(k) }),
    }
}

/// A 1-pt dashed outline (2 pt dashes, 2 pt gaps).
fn dashed(ui: &mut Ui, r: Rect, color: Rgba) {
    let mut x = r.x;
    while x < r.max_x() {
        let w = 2.0f32.min(r.max_x() - x);
        ui.fill(Rect::new(x, r.y, w, 1.0), color);
        ui.fill(Rect::new(x, r.max_y() - 1.0, w, 1.0), color);
        x += 4.0;
    }
    let mut y = r.y;
    while y < r.max_y() {
        let h = 2.0f32.min(r.max_y() - y);
        ui.fill(Rect::new(r.x, y, 1.0, h), color);
        ui.fill(Rect::new(r.max_x() - 1.0, y, 1.0, h), color);
        y += 4.0;
    }
}

/// Top-left corner coverage of UA's 2 pt panel corner, 0.5 pt cells: `(outer, fill)`;
/// outer = the shape, fill = inside the 1 pt edge. Mirrored for the other corners.
type Corner = [[(f32, f32); 5]; 5];
const UA_CORNER: Corner = [
    [(0.0, 0.0), (0.0, 0.0), (0.30, 0.0), (0.75, 0.0), (0.96, 0.0)],
    [(0.0, 0.0), (0.52, 0.0), (1.0, 0.0), (1.0, 0.0), (1.0, 0.0)],
    [(0.30, 0.0), (1.0, 0.0), (1.0, 0.0), (1.0, 0.57), (1.0, 0.93)],
    [(0.75, 0.0), (1.0, 0.0), (1.0, 0.57), (1.0, 1.0), (1.0, 1.0)],
    [(0.96, 0.0), (1.0, 0.0), (1.0, 0.93), (1.0, 1.0), (1.0, 1.0)],
];
/// The 1 pt corner of the category cells.
const CELL_CORNER: Corner = [
    [(0.0, 0.0), (0.55, 0.0), (0.91, 0.0), (1.0, 0.0), (1.0, 0.0)],
    [(0.59, 0.0), (1.0, 0.0), (1.0, 0.0), (1.0, 0.0), (1.0, 0.0)],
    [(0.91, 0.0), (1.0, 0.0), (1.0, 0.71), (1.0, 1.0), (1.0, 1.0)],
    [(1.0, 0.0), (1.0, 0.0), (1.0, 1.0), (1.0, 1.0), (1.0, 1.0)],
    [(1.0, 0.0), (1.0, 0.0), (1.0, 1.0), (1.0, 1.0), (1.0, 1.0)],
];

/// A panel as Console 1.3.1 paints it: `fill` inside a 1 pt `edge`, the rounded corners
/// (`[tl, tr, bl, br]`) blended against the known backdrop `under`.
fn rounded(ui: &mut Ui, r: Rect, fill: Rgba, edge: Rgba, under: Rgba, corner: &Corner, round: [bool; 4]) {
    ui.fill(r, edge);
    ui.fill(r.inset(1.0, 1.0, 1.0, 1.0), fill);
    let mix = |a: Rgba, b: Rgba, t: f32| Rgba { r: a.r + (b.r - a.r) * t, g: a.g + (b.g - a.g) * t, b: a.b + (b.b - a.b) * t, a: 1.0 };
    for (k, on) in round.into_iter().enumerate() {
        if !on {
            continue;
        }
        let (right, bottom) = (k % 2 == 1, k >= 2);
        for (j, row) in corner.iter().enumerate() {
            for (i, &(outer, inner)) in row.iter().enumerate() {
                // Skip cells the square fills already painted right.
                let painted = if i >= 2 && j >= 2 { 1.0 } else { 0.0 };
                if outer >= 1.0 && inner == painted {
                    continue;
                }
                let c = mix(mix(under, edge, outer), fill, inner);
                let x = if right { r.max_x() - 0.5 * (i + 1) as f32 } else { r.x + 0.5 * i as f32 };
                let y = if bottom { r.max_y() - 0.5 * (j + 1) as f32 } else { r.y + 0.5 * j as f32 };
                ui.fill(Rect::new(x, y, 0.5, 0.5), c);
            }
        }
    }
}

/// `Browser-Reveal-Toggle`: a 20 pt button, chevron art in its 4 pt background insets.
fn reveal(ui: &mut Ui, open: bool, r: Rect) {
    if let Some(skin) = ui.skin("views", "Browser-Reveal-Toggle") {
        if let Some(img) = if open { skin.image("image-on") } else { skin.image("image-off") } {
            let img = img.to_string();
            ui.image(&img, r.inset(4.0, 4.0, 4.0, 4.0), None, None, 1.0);
        }
    }
}

fn category_cell(ui: &mut Ui, label: &str, on: bool, r: Rect) {
    rounded(ui, r, if on { CELL_ON } else { CELL }, CELL_EDGE, SECTION, &CELL_CORNER, [true; 4]);
    // Long names are squeezed to fit by the renderer (JUCE drawFittedText, down to 0.7).
    let color = if on { AMBER } else { Rgba::rgb8(225, 225, 224) };
    ui.text(label, bold(11.0, color), r.inset(5.0, 0.0, 5.0, 0.0), Align::Center);
}

/// One list line: a group header or a catalog entry.
#[derive(Clone)]
enum Line {
    Group { title: &'static str, open: bool, control: Control },
    Entry(usize),
}

/// The list's groups (`Rack_PluginsBrowserManager::_GetRowData`, refpairs/p1 + p2):
/// "Universal Audio" holds the licensed or demoing plug-ins, then those whose demo can be
/// started (DEMO badge), each in catalogue order; "Universal Audio (Inactive)" the rest
/// (BUY / ACTIVATE / DISABLED).
fn list_lines(catalog: &[PluginEntry], shown: &[usize], view: &ViewState) -> Vec<Line> {
    let b = &view.browser;
    let demo = |p: &PluginEntry| !p.enabled && p.badge == Some(crate::model::PluginBadge::Demo);
    let active: Vec<usize> = shown.iter().copied().filter(|&i| catalog[i].enabled).chain(shown.iter().copied().filter(|&i| demo(&catalog[i]))).collect();
    let inactive: Vec<usize> = shown.iter().copied().filter(|&i| !catalog[i].enabled && !demo(&catalog[i])).collect();
    let mut lines = Vec::new();
    for (members, title, open, control) in [
        (active, "Universal Audio", b.group_open, Control::BrowserGroupReveal),
        (inactive, "Universal Audio (Inactive)", b.inactive_open, Control::BrowserInactiveReveal),
    ] {
        if members.is_empty() {
            continue;
        }
        lines.push(Line::Group { title, open, control });
        if open {
            lines.extend(members.into_iter().map(Line::Entry));
        }
    }
    lines
}

/// The groups and their entries scrolling inside `r`, the scrollbar at its right edge. The
/// header of the group at the top stays pinned (pushed up by the next header).
fn plugin_list(ui: &mut Ui, model: &dyn MixerModel, view: &ViewState, r: Rect) {
    let b = &view.browser;
    let catalog = model.plugin_catalog();
    let shown = filtered(&catalog, &model.plugin_categories(), view);
    let row_h = if b.large_rows { ROW_H } else { SMALL_ROW_H };
    let lines = list_lines(&catalog, &shown, view);
    let height = |l: &Line| if matches!(l, Line::Group { .. }) { GROUP_H } else { row_h };
    let content_h: f32 = lines.iter().map(height).sum();
    let max = (content_h - r.h).max(0.0);
    let off = (b.scroll.clamp(0.0, 1.0) * max).round();
    ui.scroll_region(r, ScrollTarget::Browser, max);
    let content_w = r.w - SCROLLBAR_W;
    if lines.is_empty() {
        let msg = if catalog.is_empty() { "Plug-in catalog not loaded" } else { "No matching plug-ins" };
        ui.text(msg, Ui::plain_style("Helvetica", 11.0, false, Rgba::WHITE.with_alpha(0.4)), Rect::new(r.x, r.y, content_w, 30.0), Align::Center);
    }
    // Line tops in window points, scrolled.
    let mut tops = Vec::with_capacity(lines.len());
    let mut y = r.y - off;
    for l in &lines {
        tops.push(y);
        y += height(l);
    }
    // Pinned header: the last group whose header has scrolled to or above the top.
    let groups: Vec<usize> = (0..lines.len()).filter(|&k| matches!(lines[k], Line::Group { .. })).collect();
    let pinned = groups.iter().rposition(|&k| tops[k] <= r.y).map(|gi| {
        let k = groups[gi];
        let y = groups.get(gi + 1).map_or(r.y, |&next| r.y.min(tops[next] - GROUP_H));
        (k, y)
    });
    let rows_top = pinned.map_or(r.y, |(_, y)| (y + GROUP_H).max(r.y));
    ui.clip(Some(Rect::new(r.x, rows_top, r.w, r.max_y() - rows_top)));
    for (k, line) in lines.iter().enumerate() {
        let lh = height(line);
        let row = Rect::new(r.x, tops[k], content_w, lh);
        if row.max_y() <= rows_top || row.y >= r.max_y() || pinned.is_some_and(|(p, _)| p == k) {
            continue;
        }
        match line {
            Line::Group { title, open, control } => group_header(ui, title, *open, control, row),
            &Line::Entry(i) => entry_row(ui, &catalog[i], i, view, row),
        }
    }
    ui.clip(None);
    if let Some((k, y)) = pinned {
        if let Line::Group { title, open, control } = &lines[k] {
            ui.clip(Some(r));
            group_header(ui, title, *open, control, Rect::new(r.x, y, content_w, GROUP_H));
            ui.clip(None);
        }
    }
    // `Small-Vertical-Scroll-Bar` thumb (no track art); JUCE sizes it visible/total.
    if max > 0.0 {
        let track = Rect::new(r.max_x() - SCROLLBAR_W, r.y, SCROLLBAR_W, r.h);
        let thumb_h = (track.h * r.h / content_h).round().max(SCROLLBAR_W);
        let ty = (track.y + (track.h - thumb_h) * b.scroll.clamp(0.0, 1.0)).round();
        let thumb = Rect::new(track.x, ty, track.w, thumb_h);
        if let Some(img) = ui.skin("backgrounds", "Small-Vertical-Scroll-Bar").and_then(|s| s.image("image")) {
            let img = img.to_string();
            ui.image_stretched(&img, thumb, 1.0);
        }
        ui.hit(thumb, bid(Control::BrowserScroll), HitKind::Drag { value: 1.0 - b.scroll as f64, span: track.h - thumb_h, reset: None });
    }
}

/// Group header (32 pt): reveal toggle centred, title on the upper 30 pt.
fn group_header(ui: &mut Ui, title: &str, open: bool, control: &Control, row: Rect) {
    reveal(ui, open, Rect::new(row.x + 4.0, row.y + 6.0, 20.0, 20.0));
    ui.text(title, bold(11.0, Rgba::rgb8(132, 132, 133)), Rect::new(row.x + 34.0, row.y, row.w - 40.0, 30.0), Align::Left);
    ui.hit(row, bid(control.clone()), HitKind::Toggle);
}

/// A catalog row: icon (large rows), name, and the badge 15 pt from the content's right edge.
fn entry_row(ui: &mut Ui, p: &PluginEntry, i: usize, view: &ViewState, row: Rect) {
    let b = &view.browser;
    if b.selected == Some(i as u16) {
        ui.fill(row, Rgba::rgb8(70, 60, 30));
    }
    let text_x = if b.large_rows {
        let icon = Rect::new(row.x + 14.0, row.y + 4.5, ICON.0, ICON.1);
        let key = p.icon_key();
        match ui.image_size(&key) {
            Some((iw, ih)) => {
                let s = (icon.w / iw).min(icon.h / ih);
                ui.image(&key, Rect::new(icon.x, icon.y + (icon.h - ih * s) / 2.0, iw * s, ih * s), None, None, 1.0);
            }
            None => ui.fill(icon, Rgba::rgb8(60, 63, 66)),
        }
        row.x + 118.0
    } else {
        row.x + 14.0
    };
    let badge = p.badge.filter(|_| !p.enabled);
    let badge_r = Rect::new(row.max_x() - 15.0 - 55.0, row.y + ((row.h - 17.0) / 2.0).floor(), 55.0, 17.0);
    // Names stop 19 pt before the badge (refpairs/p2: "Valley People Dyna-mi").
    let text_right = if badge.is_some() { badge_r.x - 19.0 } else { row.max_x() - 4.0 };
    ui.clip(Some(Rect::new(text_x, row.y, text_right - text_x, row.h)));
    let unavailable = p.native_supported == Some(false);
    let color = if unavailable { Rgba::rgb8(145, 145, 145) } else { Rgba::rgb8(230, 230, 230) };
    let name_h = if b.large_rows && p.native_supported.is_some() { row.h - 14.0 } else { row.h };
    ui.text(p.display_name(), bold(11.0, color), Rect::new(text_x, row.y - 0.5, 400.0, name_h), Align::Left);
    if b.large_rows && p.native_supported.is_some() {
        ui.text(if unavailable { "DSP loader unavailable" } else { "Native DSP loader" }, bold(9.0, color), Rect::new(text_x, row.max_y() - 19.0, 400.0, 14.0), Align::Left);
    }
    ui.clip(None);
    if let Some(badge) = badge {
        if let Some(img) = ui.skin("views", badge.skin()).and_then(|s| s.image("image")) {
            let img = img.to_string();
            ui.image(&img, badge_r, None, None, 1.0);
        }
    }
    ui.hit(row, bid(Control::BrowserItem(i as u16)), HitKind::Tap);
}

/// PRESETS tab (`Rack_PresetListBox`, user captures s09/s45): the list well under the search
/// field, the dismissable "DOUBLE-CLICK OR DRAG / TO ASSIGN CHANNEL PRESET" hint, then the
/// preset tree in 32-pt rows: folders with their reveal chevron and grey name, files indented
/// under an open folder on a 1-pt tree line, white. Top-level files sit at the folder indent.
/// A row tap selects (and, with a "Channel Presets..." / "Presets..." target, loads there); the
/// target adds Console's operations row (`Rack_DeleteRenameSave`: NEW FOLDER, DELETE, RENAME,
/// SAVE AS...). The operations row's look is not captured yet; it follows the I/O PRESETS
/// panel (s38).
fn presets_tab(ui: &mut Ui, model: &dyn MixerModel, view: &ViewState, r: Rect) {
    use crate::presets::{PresetCmd, PresetRow};
    let b = &view.browser;
    rounded(ui, r, WELL, WELL_EDGE, BG, &UA_CORNER, [true; 4]);
    let mut y = r.y + MARGIN;
    if b.hint && b.preset_target.is_none() {
        let hr = Rect::new(r.x + MARGIN, y, r.w - 2.0 * MARGIN, HINT_H);
        ui.image_stretched("uapw/tag_bg_on.png", hr, 1.0);
        let hs = bold(11.0, Rgba::rgb8(227, 227, 228));
        ui.text("DOUBLE-CLICK OR DRAG", hs.clone(), Rect::new(hr.x, hr.y + 5.75, hr.w, 20.0), Align::Center);
        ui.text("TO ASSIGN CHANNEL PRESET", hs, Rect::new(hr.x, hr.y + 27.75, hr.w, 20.0), Align::Center);
        let close = Rect::new(hr.max_x() - 20.0, hr.y, 20.0, 20.0);
        if let Some(x) = ui.skin("views", "Browser-Modal-Close-White").and_then(|s| s.image("image")) {
            let x = x.to_string();
            ui.image(&x, Rect::new(close.x + 3.0, close.y + 3.0, 13.0, 13.0), None, None, 1.0);
        }
        ui.hit(close, bid(Control::BrowserHintClose), HitKind::Tap);
        y = hr.max_y() + MARGIN;
    }
    // The operations row, at the bottom of the well while a target is set.
    const OPS_H: f32 = 26.0;
    let ops = b.preset_target.as_ref().map(|_| Rect::new(r.x + MARGIN, r.max_y() - MARGIN - OPS_H, r.w - 2.0 * MARGIN, OPS_H));
    let bottom = ops.map(|o| o.y - MARGIN).unwrap_or(r.max_y() - MARGIN);
    let list = Rect::new(r.x, y, r.w, bottom - y);
    let tree = view.preset_tree(model);
    let current = b.preset_target.as_ref().and_then(|(strip, scope)| {
        let s = model.inputs().into_iter().chain(model.auxes()).find(|s| s.id == *strip)?;
        model.preset_name(&s, *scope)
    });
    // Lines: (row, node, last child of its folder).
    let mut lines: Vec<(PresetRow, &crate::model::PresetNode, bool)> = Vec::new();
    for (k, f) in tree.iter().enumerate() {
        lines.push((PresetRow { top: k as u16, child: None }, f, false));
        let open = f.children.is_some() && (b.presets_open.iter().any(|n| *n == f.name) || !b.search.trim().is_empty());
        if open {
            let kids = f.children.as_deref().unwrap_or_default();
            for (j, c) in kids.iter().enumerate() {
                lines.push((PresetRow { top: k as u16, child: Some(j as u16) }, c, j + 1 == kids.len()));
            }
        }
    }
    let content_h = lines.len() as f32 * GROUP_H;
    let max = (content_h - list.h).max(0.0);
    let off = (b.presets_scroll.clamp(0.0, 1.0) * max).round();
    let content_w = list.w - SCROLLBAR_W;
    let line_c = Rgba::rgb8(88, 90, 92);
    ui.clip(Some(list));
    for (i, &(row_id, node, last)) in lines.iter().enumerate() {
        let row = Rect::new(list.x, list.y - off + i as f32 * GROUP_H, content_w, GROUP_H);
        if row.max_y() <= list.y || row.y >= list.max_y() {
            continue;
        }
        let (top, child) = row_id.encode();
        if node.children.is_some() {
            reveal(ui, b.presets_open.iter().any(|n| *n == node.name), Rect::new(row.x + 4.0, row.y + 6.0, 20.0, 20.0));
            ui.text(&node.name, bold(11.0, Rgba::rgb8(132, 132, 133)), Rect::new(row.x + 33.0, row.y + 1.25, row.w - 38.0, 30.0), Align::Left);
            ui.hit(row, bid(Control::BrowserPresetFolder(top)), HitKind::Toggle);
            continue;
        }
        if b.preset_selected == Some((top, child)) {
            ui.fill(row.inset(2.0, 1.0, 2.0, 1.0), CELL_ON);
        }
        let value = row_id.value(&tree);
        let is_current = value.is_some() && value == current.as_ref().map(|c| c.trim_start_matches('/').to_string());
        let color = if is_current { AMBER } else { Rgba::rgb8(235, 236, 236) };
        if row_id.child.is_some() {
            // Tree line: down the folder's chevron column, a tick to the name (s09).
            let cx = row.x + 14.0;
            let end = if last { row.y + GROUP_H / 2.0 } else { row.max_y() };
            ui.fill(Rect::new(cx, row.y, 1.0, end - row.y), line_c);
            ui.fill(Rect::new(cx, row.y + GROUP_H / 2.0, 13.0, 1.0), line_c);
        }
        ui.text(&node.name, bold(11.0, color), Rect::new(row.x + 33.0, row.y + 1.25, row.w - 38.0, 30.0), Align::Left);
        ui.hit(row, bid(Control::BrowserPresetFile(top, child)), HitKind::Tap);
    }
    ui.clip(None);
    ui.scroll_region(list, ScrollTarget::Browser, max);
    if max > 0.0 {
        let track = Rect::new(list.max_x() - SCROLLBAR_W, list.y, SCROLLBAR_W, list.h);
        let thumb_h = (track.h * list.h / content_h).round().max(SCROLLBAR_W);
        let ty = (track.y + (track.h - thumb_h) * b.presets_scroll.clamp(0.0, 1.0)).round();
        if let Some(img) = ui.skin("backgrounds", "Small-Vertical-Scroll-Bar").and_then(|s| s.image("image")) {
            let img = img.to_string();
            ui.image_stretched(&img, Rect::new(track.x, ty, track.w, thumb_h), 1.0);
        }
    }
    if let (Some(o), Some((strip, scope))) = (ops, b.preset_target.as_ref()) {
        let sel = b.preset_selected.map(|(t, c)| PresetRow::decode(t, c));
        let sel_node = sel.and_then(|r| r.node(&tree));
        let user = sel_node.filter(|n| !n.locked && n.children.is_none()).and(sel).and_then(|r| r.value(&tree));
        let cmd = |c: PresetCmd| Some(ControlId { strip: strip.clone(), control: Control::PresetCmd(*scope, c) });
        let buttons: [(&str, Option<ControlId>); 4] = [
            ("NEW FOLDER", cmd(PresetCmd::NewFolder)),
            ("DELETE", user.as_ref().and_then(|v| cmd(PresetCmd::Delete(v.as_str().into())))),
            ("RENAME", user.as_ref().and_then(|v| cmd(PresetCmd::Rename(v.as_str().into())))),
            ("SAVE AS...", cmd(PresetCmd::SaveAs)),
        ];
        let bw = (o.w / 4.0).floor();
        for (k, (label, id)) in buttons.into_iter().enumerate() {
            let br = Rect::new(o.x + k as f32 * bw, o.y, bw - 1.0, o.h);
            ui.fill(br, Rgba::rgb8(88, 93, 96));
            ui.fill(Rect::new(br.x, br.max_y() - 1.0, br.w, 1.0), Rgba::BLACK.with_alpha(0.35));
            let alpha = if id.is_some() { 0.95 } else { 0.4 };
            ui.text(label, bold(10.0, Rgba::WHITE.with_alpha(alpha)), br, Align::Center);
            if let Some(id) = id {
                ui.hit(br, id, HitKind::Tap);
            }
        }
        // SAVE AS / RENAME / NEW FOLDER: the name field over the operations row.
        if let Some(e) = view.text_edit.as_ref().filter(|e| matches!(e.target, TextTarget::Preset(..))) {
            let fr = Rect::new(o.x, o.y - 30.0, o.w, 28.0);
            ui.fill(fr, Rgba::rgb8(20, 21, 22));
            ui.fill(Rect::new(fr.x, fr.max_y() - 1.0, fr.w, 1.0), AMBER);
            ui.text(&format!("{}|", e.text), bold(11.0, Rgba::WHITE), Rect::new(fr.x + 8.0, fr.y, fr.w - 16.0, fr.h), Align::Left);
        }
    }
}
