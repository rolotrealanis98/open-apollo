//! Plug-in browser panel (left, 345 pt; `Rack_PluginsBrowser`): PLUG-INS / PRESETS tabs,
//! search field, CATEGORIES grid with FAVORITES, and the scrollable plug-in list (icon,
//! name, DISABLED / ENABLED badge). Positions measured from console-main-5.png.

use houston::{Rect, Rgba};

use crate::draw::{Align, TextStyle};
use crate::input::{Control, ControlId, HitKind, ScrollTarget};
use crate::model::{MixerModel, PluginEntry};
use crate::surfaces::view::{BrowserTab, TextTarget, ViewState};
use crate::ui::Ui;

pub const BROWSER_ID: &str = "browser";
const BG: Rgba = Rgba::rgb8(31, 33, 36);
const PANEL: Rgba = Rgba::rgb8(46, 49, 52);
const AMBER: Rgba = Rgba::rgb8(255, 186, 0);

fn bid(control: Control) -> ControlId {
    ControlId { strip: BROWSER_ID.into(), control }
}

/// Catalog indices shown for the current filters (category, favorites, search).
pub fn filtered(catalog: &[PluginEntry], categories: &[String], view: &ViewState) -> Vec<usize> {
    let b = &view.browser;
    let cat = b.category.and_then(|i| categories.get(i as usize));
    let q = b.search.trim().to_lowercase();
    catalog
        .iter()
        .enumerate()
        .filter(|(_, p)| cat.is_none_or(|c| &p.category == c))
        .filter(|(_, p)| !b.favorites_only || p.favorite)
        .filter(|(_, p)| q.is_empty() || p.name.to_lowercase().contains(&q))
        .map(|(i, _)| i)
        .collect()
}

/// Draws the browser into `r` (window points).
pub fn draw_browser(ui: &mut Ui, model: &dyn MixerModel, view: &ViewState, r: Rect) {
    let b = &view.browser;
    ui.fill(r, BG);
    ui.background("Browser", r);
    ui.at(r.x, r.y, |ui| {
        let w = r.w;
        // Tabs.
        let tabs = [(BrowserTab::Plugins, "PLUG-INS"), (BrowserTab::Presets, "PRESETS")];
        let tw = ((w - 10.0) / 2.0).floor();
        for (i, (t, label)) in tabs.into_iter().enumerate() {
            let tr = Rect::new(5.0 + i as f32 * tw, 6.0, tw, 25.0);
            let on = b.tab == t;
            ui.fill(tr, if on { Rgba::rgb8(60, 63, 66) } else { Rgba::rgb8(40, 42, 45) });
            if on {
                ui.fill(Rect::new(tr.x, tr.y, tr.w, 1.5), Rgba::rgb8(150, 155, 158));
            }
            let style = Ui::plain_style("Helvetica", 11.0, true, if on { Rgba::rgb8(229, 245, 255) } else { Rgba::WHITE.with_alpha(0.3) });
            ui.text(label, style, tr, Align::Center);
            ui.hit(tr, bid(Control::BrowserTab(t)), HitKind::Tap);
        }
        // Search field.
        let sr = Rect::new(5.0, 37.0, w - 10.0, 27.0);
        ui.fill(sr, Rgba::rgb8(20, 21, 23));
        if let Some(icon) = ui.skin("views", "Browser-Search-Icon").and_then(|s| s.image("image")) {
            let icon = icon.to_string();
            ui.image(&icon, Rect::new(sr.x + 6.0, sr.y + 5.5, 16.0, 16.0), None, None, 0.8);
        }
        let focused = matches!(&view.text_edit, Some(t) if t.target == TextTarget::Search);
        let style = ui.style("views", "Browser-Text-Edit-Field");
        let text = if focused { format!("{}|", b.search) } else { b.search.clone() };
        if !text.is_empty() {
            ui.text(&text, style, Rect::new(sr.x + 30.0, sr.y, sr.w - 36.0, sr.h), Align::Left);
        }
        ui.hit(sr, bid(Control::BrowserSearch), HitKind::Tap);

        let mut y = 71.0;
        if b.tab == BrowserTab::Presets {
            presets_tab(ui, model, view, Rect::new(5.0, y, w - 10.0, r.h - y - 5.0));
            return;
        }
        // Categories panel.
        let cats = model.plugin_categories();
        let rows = cats.len().div_ceil(3);
        let panel_h = if b.categories_open { 29.0 + rows as f32 * 27.0 + 4.0 } else { 29.0 };
        ui.fill(Rect::new(5.0, y, w - 10.0, panel_h), PANEL);
        reveal(ui, b.categories_open, Rect::new(8.0, y + 5.0, 18.0, 18.0));
        let head = Ui::plain_style("Helvetica", 12.0, true, Rgba::rgb8(223, 223, 223));
        ui.text("CATEGORIES", head.clone(), Rect::new(26.0, y + 2.0, 120.0, 25.0), Align::Left);
        ui.hit(Rect::new(5.0, y, 160.0, 28.0), bid(Control::BrowserCategoriesReveal), HitKind::Toggle);
        let fav_on = b.favorites_only;
        ui.text("FAVORITES", TextStyle { color: if fav_on { AMBER } else { head.color }, ..head }, Rect::new(w - 150.0, y + 2.0, 118.0, 25.0), Align::Right);
        if let Some(skin) = ui.skin("views", "Favorite") {
            if let Some(img) = if fav_on { skin.image("image-on") } else { skin.image("image-off") } {
                let img = img.to_string();
                ui.image(&img, Rect::new(w - 29.0, y + 6.0, 17.0, 16.0), None, None, 1.0);
            }
        }
        ui.hit(Rect::new(w - 150.0, y, 145.0, 28.0), bid(Control::BrowserFavorites), HitKind::Toggle);
        if b.categories_open {
            let cw = ((w - 20.0 - 8.0) / 3.0).floor();
            for (i, c) in cats.iter().enumerate() {
                let (col, row) = (i % 3, i / 3);
                let cr = Rect::new(10.0 + col as f32 * (cw + 4.0), y + 29.0 + row as f32 * 27.0, cw, 23.0);
                let v = if row == 0 { "T" } else if row + 1 == rows { "B" } else { "M" };
                let h = ["L", "", "R"][col];
                let key = if v == "M" && h.is_empty() { "Browser-Category-M".to_string() } else { format!("Browser-Category-{v}{h}") };
                let on = b.category == Some(i as u8);
                category_button(ui, &key, c, on, cr);
                ui.hit(cr, bid(Control::BrowserCategory(i as u8)), HitKind::Toggle);
            }
        }
        y += panel_h + 6.0;

        // List panel.
        let list_r = Rect::new(5.0, y, w - 10.0, r.h - y - 4.0);
        ui.fill(list_r, PANEL.with_alpha(0.6));
        // View-mode toggle (two amber bars).
        let vm = Rect::new(10.0, y + 6.0, 16.0, 14.0);
        let bar = if b.large_rows { AMBER } else { Rgba::rgb8(120, 124, 128) };
        ui.fill(Rect::new(vm.x, vm.y, vm.w, 6.0), bar);
        ui.fill(Rect::new(vm.x, vm.y + 8.0, vm.w, 6.0), bar);
        ui.hit(vm.inset(-4.0, -4.0, -4.0, -4.0), bid(Control::BrowserLargeRows), HitKind::Toggle);
        y += 26.0;
        if b.hint {
            let hr = Rect::new(10.0, y + 4.0, w - 20.0, 53.0);
            ui.fill(hr, Rgba::rgb8(26, 27, 29));
            ui.fill(Rect::new(hr.x, hr.y, hr.w, 1.0), Rgba::rgb8(70, 74, 77));
            let hs = Ui::plain_style("Helvetica", 11.0, true, Rgba::rgb8(223, 223, 223));
            ui.text("DRAG TO AN INSERT TO ASSIGN", hs.clone(), Rect::new(hr.x, hr.y + 6.0, hr.w, 18.0), Align::Center);
            ui.text("RIGHT CLICK TO ENTER ASSIGN MODE", hs, Rect::new(hr.x, hr.y + 28.0, hr.w, 18.0), Align::Center);
            if let Some(x) = ui.skin("views", "Browser-Modal-Close-White").and_then(|s| s.image("image")) {
                let x = x.to_string();
                ui.image(&x, Rect::new(hr.max_x() - 18.0, hr.y + 4.0, 14.0, 14.0), None, None, 1.0);
            }
            ui.hit(Rect::new(hr.max_x() - 22.0, hr.y, 22.0, 22.0), bid(Control::BrowserHintClose), HitKind::Tap);
            y += 61.0;
        }
        plugin_list(ui, model, view, Rect::new(5.0, y, w - 10.0, list_r.max_y() - y));
    });
}

fn reveal(ui: &mut Ui, open: bool, r: Rect) {
    if let Some(skin) = ui.skin("views", "Browser-Reveal-Toggle") {
        if let Some(img) = if open { skin.image("image-on") } else { skin.image("image-off") } {
            let img = img.to_string();
            ui.image(&img, r.inset(2.0, 2.0, 2.0, 2.0), None, None, 0.9);
        }
    }
}

fn category_button(ui: &mut Ui, key: &str, label: &str, on: bool, r: Rect) {
    let skin = ui.skin("views", key);
    let img = skin.and_then(|s| if on { s.image("image-on") } else { s.image("image-off") });
    match img {
        Some(p) => {
            let p = p.to_string();
            ui.image_stretched(&p, r, 1.0);
        }
        None => ui.fill(r, Rgba::rgb8(28, 30, 32)),
    }
    let color = skin.and_then(|s| if on { s.color("color-text-on") } else { s.color("color-text-off") }).unwrap_or(Rgba::rgb8(226, 226, 227));
    let size = match label.len() {
        0..=16 => 11.0,
        17..=19 => 10.0,
        _ => 9.0,
    };
    ui.text(label, Ui::plain_style("Helvetica", size, true, color), r, Align::Center);
}

/// "Universal Audio" group header and the entries; rows scroll inside `r` with a scrollbar.
fn plugin_list(ui: &mut Ui, model: &dyn MixerModel, view: &ViewState, r: Rect) {
    let b = &view.browser;
    let catalog = model.plugin_catalog();
    let shown = filtered(&catalog, &model.plugin_categories(), view);
    let row_h = if b.large_rows { 48.0 } else { 24.0 };
    let group_h = 30.0;
    let content_h = group_h + if b.group_open { shown.len() as f32 * row_h } else { 0.0 } + 8.0;
    let max = (content_h - r.h).max(0.0);
    let off = (b.scroll.clamp(0.0, 1.0) * max).round();
    ui.scroll_region(r, ScrollTarget::Browser, max);
    ui.clip(Some(r));
    let mut y = r.y - off;
    // Group header.
    reveal(ui, b.group_open, Rect::new(r.x + 6.0, y + 6.0, 18.0, 18.0));
    let inactive = !catalog.iter().any(|p| p.enabled);
    let title = format!("Universal Audio{}  ·  {}", if inactive { " (Inactive)" } else { "" }, shown.len());
    ui.text(&title, Ui::plain_style("Helvetica", 12.0, true, Rgba::WHITE.with_alpha(0.4)), Rect::new(r.x + 30.0, y, r.w - 60.0, group_h), Align::Left);
    ui.hit(Rect::new(r.x, y, r.w - 16.0, group_h), bid(Control::BrowserGroupReveal), HitKind::Toggle);
    y += group_h;
    if b.group_open {
        if shown.is_empty() {
            ui.text("No matching plug-ins", Ui::plain_style("Helvetica", 11.0, false, Rgba::WHITE.with_alpha(0.4)), Rect::new(r.x, y, r.w, 30.0), Align::Center);
        }
        let name_style = Ui::plain_style("Helvetica", if b.large_rows { 12.0 } else { 11.0 }, true, Rgba::WHITE.with_alpha(0.88));
        for &i in &shown {
            let p = &catalog[i];
            let row = Rect::new(r.x, y, r.w - 16.0, row_h);
            y += row_h;
            if row.max_y() < r.y || row.y > r.max_y() {
                continue;
            }
            if b.selected == Some(i as u16) {
                ui.fill(row, Rgba::rgb8(70, 60, 30));
            }
            let text_x = if b.large_rows {
                let icon = Rect::new(row.x + 14.0, row.y + 5.0, 95.0, 38.0);
                let key = p.icon_key();
                match ui.image_size(&key) {
                    Some((iw, ih)) => {
                        let s = (icon.w / iw).min(icon.h / ih);
                        ui.image(&key, Rect::new(icon.x, icon.y + (icon.h - ih * s) / 2.0, iw * s, ih * s), None, None, if p.enabled { 1.0 } else { 0.85 });
                    }
                    None => ui.fill(icon, Rgba::rgb8(60, 63, 66)),
                }
                row.x + 118.0
            } else {
                row.x + 14.0
            };
            ui.clip(Some(Rect::new(text_x, row.y, r.x + 242.0 - text_x, row.h)));
            ui.text(p.display_name(), name_style.clone(), Rect::new(text_x, row.y, 300.0, row.h), Align::Left);
            ui.clip(None);
            badge(ui, p.enabled, Rect::new(r.x + 246.5, row.y + (row_h - 17.0) / 2.0, 55.0, 17.0));
            ui.hit(row, bid(Control::BrowserItem(i as u16)), HitKind::Tap);
        }
    }
    ui.clip(None);
    // Scrollbar.
    if max > 0.0 {
        let track = Rect::new(r.max_x() - 12.0, r.y + 4.0, 8.0, r.h - 8.0);
        ui.fill(track, Rgba::rgb8(24, 25, 27));
        let thumb_h = (track.h * r.h / content_h).max(24.0);
        let ty = track.y + (track.h - thumb_h) * b.scroll.clamp(0.0, 1.0);
        let thumb = Rect::new(track.x, ty, track.w, thumb_h);
        ui.fill(thumb, Rgba::rgb8(18, 19, 20));
        ui.fill(thumb.inset(1.0, 1.0, 1.0, 1.0), Rgba::rgb8(92, 96, 100));
        ui.hit(thumb.inset(-3.0, 0.0, -3.0, 0.0), bid(Control::BrowserScroll), HitKind::Drag { value: 1.0 - b.scroll as f64, span: track.h - thumb_h, reset: None });
    }
}

/// DISABLED (`Plugin-Badge-Disabled`) or a green ENABLED placeholder.
fn badge(ui: &mut Ui, enabled: bool, r: Rect) {
    if !enabled {
        if let Some(img) = ui.skin("views", "Plugin-Badge-Disabled").and_then(|s| s.image("image")) {
            let img = img.to_string();
            ui.image(&img, r, None, None, 1.0);
            return;
        }
    }
    ui.fill(r, if enabled { Rgba::rgb8(46, 125, 60) } else { Rgba::rgb8(52, 103, 170) });
    let style = Ui::plain_style("Helvetica", 9.5, true, Rgba::WHITE.with_alpha(0.92));
    ui.text(if enabled { "ENABLED" } else { "DISABLED" }, style, r, Align::Center);
}

fn presets_tab(ui: &mut Ui, model: &dyn MixerModel, view: &ViewState, r: Rect) {
    ui.fill(r, PANEL.with_alpha(0.6));
    let catalog = model.plugin_catalog();
    let sel = view.browser.selected.and_then(|i| catalog.get(i as usize));
    let head = Ui::plain_style("Helvetica", 12.0, true, Rgba::rgb8(223, 223, 223));
    ui.text(sel.map(|p| p.display_name()).unwrap_or("NO PLUG-IN SELECTED"), head, Rect::new(r.x + 10.0, r.y + 4.0, r.w - 20.0, 24.0), Align::Left);
    let note = Ui::plain_style("Helvetica", 11.0, false, Rgba::WHITE.with_alpha(0.45));
    ui.text("Presets: not yet wired (plugins/P/Preset values)", note, Rect::new(r.x + 10.0, r.y + 30.0, r.w - 20.0, 20.0), Align::Left);
}
