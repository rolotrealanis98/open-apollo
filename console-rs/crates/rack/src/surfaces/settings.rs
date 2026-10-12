//! The Settings window (`Rack_SettingsPopover` › `Rack_SettingsPage`): a "SETTINGS" header,
//! the `Rack_SettingsHeader` page tabs and the page of the selected tab, drawn from
//! [`crate::settings`] fields. Shared field widgets (the `Silk-Screen` label cell with its
//! combo, switch, button or text field, section headers, the combo dropdown) are used by the
//! secondary panels too (`surfaces::panels`).
//!
//! Geometry:
//! - [obs] `Rack_SettingsPage::resized` (0x180e92d40): content width `min(w − 20, 980)`,
//!   centred; the header (tabs) at y 20, `settings_header_height` (48) tall; pages below it
//!   down to 10 pt above the bottom. Dimension table (0x180855110…): `settings_header_gap` 4,
//!   `settings_header_height` 48, `settings_header_tab` 124.
//! - [obs] HARDWARE cells per `Rack_SettingsBase` (0x180e8ee90): label box `(x, y, 160, 58)`,
//!   control `(x + 5, y + 17, 150, 36)`, 170 pt apart, rows 68 pt apart starting at (10, 10).
//! - [I] The window size, the tab art and the section placement until a 1.3.1 capture of the
//!   Settings window exists.

use houston::{Rect, Rgba};

use crate::draw::Align;
use crate::input::{Control, ControlId, HitKind, ScrollTarget};
use crate::model::MixerModel;
use crate::settings::{self, FieldKind, PropertySource, SettingField, SettingsGroup, SettingsTab};
use crate::surfaces::monitor::combo_box;
use crate::surfaces::popover::{MenuItem, Placement, pid, sample_rate_label};
use crate::surfaces::view::{TextEdit, TextTarget, ViewState};
use crate::ui::Ui;
use crate::window::helvetica_width;

pub const SETTINGS_ID: &str = "settings";

/// Label cell (`Rack_SilkScreen`) and control placement (`Rack_SettingsBase`).
pub const CELL_W: f32 = 160.0;
pub const CELL_H: f32 = 58.0;
pub const CELL_PITCH_X: f32 = 170.0;
pub const CELL_PITCH_Y: f32 = 68.0;
/// `settings_header_height`, `settings_header_tab`, `settings_header_gap`.
const TAB_H: f32 = 48.0;
const TAB_W: f32 = 124.0;
const TAB_GAP: f32 = 4.0;
/// `Rack_SettingsPage`: page content width cap and top margin.
const PAGE_MAX_W: f32 = 980.0;
const PAGE_TOP: f32 = 20.0;
const HEADER_H: f32 = 30.0;
const FOOTER_H: f32 = 26.0;
pub(crate) const SECTION_HEADER_H: f32 = 30.0;

pub(crate) fn sid(control: Control) -> ControlId {
    ControlId { strip: SETTINGS_ID.into(), control }
}

/// A field drawn this frame (the dropdown looks its options up here).
pub(crate) struct Drawn {
    pub field: SettingField,
}

/// Window chrome shared by Settings and the sub-windows: shadow, frame, `Popover-Header`
/// band with the title (`Button-Popover-Header`) and the close button
/// (`Browser-Modal-Close-White`; Console 3 always shows it, feature `always_show_close_button`).
/// Swallows taps on the body. Returns the content rect below the header.
pub(crate) fn window_frame(ui: &mut Ui, r: Rect, title: &str, close: ControlId) -> Rect {
    ui.fill(r.offset(3.0, 4.0), Rgba::BLACK.with_alpha(0.4));
    ui.fill(r, Rgba::rgb8(16, 17, 18));
    ui.fill(r.inset(1.0, 1.0, 1.0, 1.0), Rgba::rgb8(44, 47, 50));
    ui.hit(r, pid(Control::PopoverDismiss), HitKind::Drag { value: 0.0, span: 1.0, reset: None });
    let band = Rect::new(r.x + 1.0, r.y + 1.0, r.w - 2.0, HEADER_H);
    ui.fill(band, Rgba::rgb8(34, 36, 39));
    if let Some(img) = ui.skin("backgrounds", "Popover-Header").and_then(|s| s.image("image")) {
        let img = img.to_string();
        ui.image_stretched(&img, band, 1.0);
    }
    let mut style = ui.style("views", "Button-Popover-Header");
    style.size = 11.0;
    ui.text(title, style, band, Align::Center);
    if let Some(x) = ui.skin("views", "Browser-Modal-Close-White").and_then(|s| s.image("image")) {
        let x = x.to_string();
        ui.image(&x, Rect::new(band.max_x() - 25.0, band.y + 7.0, 16.0, 16.0), None, None, 0.9);
    }
    ui.hit(Rect::new(band.max_x() - HEADER_H, band.y, HEADER_H, HEADER_H), close, HitKind::Tap);
    Rect::new(r.x + 1.0, band.max_y(), r.w - 2.0, r.max_y() - 1.0 - band.max_y())
}

/// Section header band (`Hardware-Setup-Device-Header`) with its title in
/// `Settings-Section-Header-Static-Text` (bevel in front: reads as white 0.7).
pub(crate) fn section_header(ui: &mut Ui, title: &str, r: Rect) {
    ui.fill(r, Rgba::rgb8(36, 38, 41));
    ui.background("Hardware-Setup-Device-Header", r);
    let mut style = ui.style("fonts", "Settings-Section-Header-Static-Text");
    style.color = Rgba::WHITE.with_alpha(0.7);
    ui.text(title, style, r.inset(10.0, 0.0, 10.0, 0.0), Align::Left);
}

/// One field: the `Silk-Screen` label box with its label on top and the control inside
/// (combo / switch / button / text / readout), at `(x, y)` in a `CELL_W × CELL_H` cell.
pub(crate) fn field_cell(ui: &mut Ui, f: &SettingField, x: f32, y: f32, edit: Option<&TextEdit>, drawn: &mut Vec<Drawn>) {
    let cell = Rect::new(x, y, CELL_W, CELL_H);
    let control = Rect::new(x + 5.0, y + 17.0, 150.0, 36.0);
    match &f.kind {
        FieldKind::Button => {
            // Buttons (`Switch-Large`) stand alone, centred in the cell.
            let b = Rect::new(x + (CELL_W - 80.0) / 2.0, y + (CELL_H - 40.0) / 2.0 + 4.0, 80.0, 40.0);
            ui.toggle("Switch-Large", Some(&f.label), false, f.enabled, b, None, None);
            if f.enabled {
                ui.hit(b, sid(Control::SettingPress(f.key.clone())), HitKind::Tap);
            }
        }
        FieldKind::Action(button) => {
            // A captioned cell holding one `Switch-Large` button (RESET, LAUNCH).
            ui.background("Silk-Screen", cell);
            silk_label(ui, &f.label, cell);
            let b = Rect::new(x + (CELL_W - 80.0) / 2.0, y + 15.0, 80.0, 40.0);
            ui.toggle("Switch-Large", Some(button), false, f.enabled, b, None, None);
            if f.enabled {
                ui.hit(b, sid(Control::SettingPress(f.key.clone())), HitKind::Tap);
            }
        }
        FieldKind::Toggle { on } => {
            ui.background("Silk-Screen", cell);
            silk_label(ui, &f.label, cell);
            let b = Rect::new(x + (CELL_W - 80.0) / 2.0, y + 15.0, 80.0, 40.0);
            ui.toggle("Switch-Large", Some(if *on { "ON" } else { "OFF" }), *on, f.enabled, b, None, None);
            if f.enabled {
                ui.hit(b, sid(Control::SettingPress(f.key.clone())), HitKind::Toggle);
            }
        }
        FieldKind::Choice { .. } => {
            ui.background("Silk-Screen", cell);
            silk_label(ui, &f.label, cell);
            choice_box(ui, f, control.inset(0.0, 4.0, 0.0, 4.0));
        }
        FieldKind::Text(text) => {
            ui.background("Silk-Screen", cell);
            silk_label(ui, &f.label, cell);
            let editing = edit.filter(|e| matches!(&e.target, TextTarget::Setting(k) if *k == f.key));
            let shown = editing.map(|e| e.text.as_str()).unwrap_or(text);
            let r = control.inset(0.0, 5.0, 0.0, 5.0);
            ui.text_field("Popover-Text-Edit-Field", shown, editing.is_some(), r);
            if f.enabled {
                ui.hit(r, sid(Control::SettingEdit(f.key.clone())), HitKind::Tap);
            }
        }
        FieldKind::Readout(text) => {
            ui.background("Silk-Screen", cell);
            silk_label(ui, &f.label, cell);
            let style = Ui::plain_style("Helvetica", 11.0, true, Rgba::WHITE.with_alpha(0.75));
            ui.text(text, style, control, Align::Center);
        }
    }
    drawn.push(Drawn { field: f.clone() });
}

/// The `Silk-Screen` caption: Helvetica Bold 9 (120, 127, 131), top-centred.
fn silk_label(ui: &mut Ui, label: &str, cell: Rect) {
    let style = ui.style("views", "Silk-Screen");
    ui.text(label, style, Rect::new(cell.x + 4.0, cell.y + 3.0, cell.w - 8.0, 14.0), Align::Center);
}

/// A settings combo (`Combo-Box`) showing the selected option; tapping opens its dropdown.
pub(crate) fn choice_box(ui: &mut Ui, f: &SettingField, r: Rect) {
    let text = f.value_text();
    let usable = f.enabled && matches!(&f.kind, FieldKind::Choice { options, .. } if !options.is_empty());
    combo_box(ui, &text, r);
    if !usable {
        ui.fill(r, Rgba::rgb8(44, 47, 50).with_alpha(0.55));
    } else {
        ui.hit(r, sid(Control::SettingMenu(f.key.clone())), HitKind::Tap);
    }
}

/// Lays `fields` out in Console's cell grid: one line per `row`, wrapping when a line would
/// pass `max_w`. Returns the height used.
pub(crate) fn field_grid(ui: &mut Ui, fields: &[SettingField], origin: (f32, f32), max_w: f32, edit: Option<&TextEdit>, drawn: &mut Vec<Drawn>) -> f32 {
    let cols = (((max_w - 10.0 - CELL_W) / CELL_PITCH_X).floor().max(0.0) as usize + 1).max(1);
    let mut y = origin.1;
    let mut rows: Vec<u8> = fields.iter().map(|f| f.row).collect();
    rows.dedup();
    for row in rows {
        let line: Vec<&SettingField> = fields.iter().filter(|f| f.row == row).collect();
        for chunk in line.chunks(cols) {
            for (k, f) in chunk.iter().enumerate() {
                field_cell(ui, f, origin.0 + k as f32 * CELL_PITCH_X, y, edit, drawn);
            }
            y += CELL_PITCH_Y;
        }
    }
    y - origin.1
}

/// The open combo's dropdown, anchored to its box (drawn last).
pub(crate) fn choice_menu(ui: &mut Ui, view: &ViewState, drawn: &[Drawn], bounds: Rect) {
    let Some(key) = &view.settings.menu else { return };
    let Some(d) = drawn.iter().find(|d| &d.field.key == key) else { return };
    let FieldKind::Choice { options, enabled, selected } = &d.field.kind else { return };
    let Some(anchor) = ui.hit_rect(&sid(Control::SettingMenu(key.clone()))) else { return };
    let items: Vec<MenuItem> = options
        .iter()
        .enumerate()
        .map(|(i, o)| MenuItem::new(o.clone(), sid(Control::SettingChoose(key.clone(), i as u16))).checked(*selected == Some(i)).enabled(enabled.get(i).copied().unwrap_or(true)))
        .collect();
    let width = options.iter().map(|o| helvetica_width(o, 11.0) + 44.0).fold(anchor.w, f32::max);
    ui.popover_catcher(bounds);
    let low = anchor.y > bounds.h * 0.6;
    ui.popover_menu(anchor, if low { Placement::Above } else { Placement::Below }, bounds, None, &items, width);
}

/// `Page-Tab`: the selected tab draws `image-on`, the others the off art for their position.
fn page_tab(ui: &mut Ui, label: &str, on: bool, first: bool, last: bool, r: Rect) {
    let Some(skin) = ui.skin("views", "Page-Tab") else {
        ui.fill(r, if on { Rgba::rgb8(60, 63, 66) } else { Rgba::rgb8(36, 38, 41) });
        return;
    };
    let img = if on {
        skin.image("image-on")
    } else if first {
        skin.image("image-off-left")
    } else if last {
        skin.image("image-off-right")
    } else {
        skin.image("image-off-center")
    };
    let color = if on { skin.color("color-text-on") } else { skin.color("color-text-off") };
    if let Some(img) = img {
        let img = img.to_string();
        ui.image_stretched(&img, r, 1.0);
    }
    let mut style = ui.style("views", "Page-Tab");
    if let Some(c) = color {
        style.color = c;
    }
    ui.text(label, style, r.inset(0.0, 5.0, 0.0, 0.0), Align::Center);
}

/// The Settings window over `bounds` (modal: the backdrop swallows taps).
pub fn draw_settings_sheet(ui: &mut Ui, model: &dyn MixerModel, view: &ViewState, bounds: Rect) {
    ui.fill(bounds, Rgba::BLACK.with_alpha(0.5));
    ui.hit(bounds, pid(Control::PopoverDismiss), HitKind::Tap);
    let w = (bounds.w - 40.0).clamp(640.0, PAGE_MAX_W + 22.0).min(bounds.w);
    let h = (bounds.h - 60.0).clamp(420.0, 760.0).min(bounds.h);
    let r = Rect::new(bounds.x + ((bounds.w - w) / 2.0).round(), bounds.y + ((bounds.h - h) / 2.5).round(), w, h);
    let content = window_frame(ui, r, "SETTINGS", sid(Control::SettingsClose));
    let body = Rect::new(content.x, content.y, content.w, content.h - FOOTER_H);
    footer(ui, model, Rect::new(content.x, body.max_y(), content.w, FOOTER_H));

    let Some(src) = model.properties() else {
        let note = Ui::plain_style("Helvetica", 11.0, false, Rgba::WHITE.with_alpha(0.5));
        ui.text("No engine properties.", note, body, Align::Center);
        return;
    };
    let tabs = settings::tabs(src);
    let tab = if tabs.contains(&view.settings.tab) { view.settings.tab } else { tabs[0] };

    // `Rack_SettingsPage`: border, then the header (tabs) and the page in a centred column.
    ui.fill(body, Rgba::rgb8(52, 55, 58));
    ui.background("Settings-Background", body);
    let cw = (body.w - 20.0).min(PAGE_MAX_W);
    let cx = body.x + ((body.w - cw) / 2.0).floor();
    let bar = Rect::new(cx, body.y + PAGE_TOP, cw, TAB_H);
    ui.background("Settings-Header", bar);
    let mut x = bar.x;
    for (i, t) in tabs.iter().enumerate() {
        let tw = TAB_W.max((helvetica_width(t.label(), 11.0) + 28.0).round());
        let tr = Rect::new(x, bar.y + (TAB_H - 30.0), tw, 30.0);
        page_tab(ui, t.label(), *t == tab, i == 0, i + 1 == tabs.len(), tr);
        ui.hit(tr, sid(Control::SettingsTab(*t)), HitKind::Tap);
        x += tw + TAB_GAP;
    }

    let page = Rect::new(cx, bar.max_y(), cw, body.max_y() - 10.0 - bar.max_y());
    ui.fill(page, Rgba::rgb8(44, 47, 50));
    let mut drawn = Vec::new();
    ui.clip(Some(page));
    let edit = view.text_edit.as_ref();
    match tab {
        SettingsTab::Hardware => hardware_page(ui, src, view, page, edit, &mut drawn),
        SettingsTab::IoMatrix => io_matrix_page(ui, src, view, page, edit, &mut drawn),
        SettingsTab::Options => {
            let mut y = page.y + 10.0;
            for (title, group) in [("DISPLAY", SettingsGroup::Display), ("EDITING", SettingsGroup::Editing)] {
                let f = settings::fields(src, group);
                if f.is_empty() {
                    continue;
                }
                section_header(ui, title, Rect::new(page.x + 10.0, y, page.w - 20.0, SECTION_HEADER_H));
                y += SECTION_HEADER_H + 10.0;
                y += field_grid(ui, &f, (page.x + 10.0, y), page.w - 10.0, edit, &mut drawn) + 6.0;
            }
        }
        SettingsTab::Midi => {
            let f = settings::fields(src, SettingsGroup::Midi);
            field_grid(ui, &f, (page.x + 10.0, page.y + 10.0), page.w - 10.0, edit, &mut drawn);
        }
        SettingsTab::PlugIns => plugins_page(ui, model, src, view, page, edit, &mut drawn),
        SettingsTab::DanteDevices => {
            let f = settings::fields(src, SettingsGroup::Dante);
            field_grid(ui, &f, (page.x + 10.0, page.y + 10.0), page.w - 10.0, edit, &mut drawn);
        }
        SettingsTab::IoSettings | SettingsTab::Controllers | SettingsTab::Network => {
            let note = Ui::plain_style("Helvetica", 13.0, false, Rgba::WHITE.with_alpha(0.5));
            ui.text("No devices", note, page, Align::Center);
        }
    }
    ui.clip(None);
    choice_menu(ui, view, &drawn, bounds);
}

/// HARDWARE: the global grid (`Rack_ApolloSettings`), then DEVICES and the selected device's
/// OPTIONS (`Rack_DeviceList`, `Rack_DeviceOptions`).
fn hardware_page(ui: &mut Ui, src: &dyn PropertySource, view: &ViewState, page: Rect, edit: Option<&TextEdit>, drawn: &mut Vec<Drawn>) {
    let f = settings::fields(src, SettingsGroup::Hardware);
    let grid_h = field_grid(ui, &f, (page.x + 10.0, page.y + 10.0), page.w - 10.0, edit, drawn);
    let top = page.y + 10.0 + grid_h + 4.0;
    let devices = settings::devices(src);
    let list = Rect::new(page.x + 10.0, top, 200.0, page.max_y() - 10.0 - top);
    section_header(ui, "DEVICES", Rect::new(list.x, list.y, list.w, SECTION_HEADER_H));
    let list_body = Rect::new(list.x, list.y + SECTION_HEADER_H, list.w, list.h - SECTION_HEADER_H);
    ui.background("Device-Settings-Background", list_body);
    let selected = devices.iter().find(|d| d.index == view.settings.device).or(devices.first()).map(|d| d.index);
    for (k, d) in devices.iter().enumerate() {
        let r = Rect::new(list_body.x + 13.0, list_body.y + 10.0 + k as f32 * 48.0, 173.0, 40.0);
        let key = format!("Core-Audio-Device-{}", d.letter());
        let on = Some(d.index) == selected;
        if let Some(skin) = ui.skin("views", &key) {
            if let Some(img) = skin.image("image-off") {
                let img = img.to_string();
                ui.image_stretched(&img, r, if d.online { 1.0 } else { 0.5 });
            }
            if on {
                if let Some(img) = skin.image("image-on") {
                    let img = img.to_string();
                    ui.image_stretched(&img, r, 1.0);
                }
            }
        } else {
            ui.fill(r, Rgba::rgb8(58, 62, 66));
        }
        if let Some((_, h)) = ui.skin("backgrounds", &d.icon_key()).and_then(|s| s.size()) {
            let (w, h) = (ui.skin("backgrounds", &d.icon_key()).and_then(|s| s.size()).map_or(39.0, |s| s.0), h);
            ui.background(&d.icon_key(), Rect::new(r.x + 8.0, r.y + ((r.h - h) / 2.0).round(), w, h));
        }
        let style = Ui::plain_style("Helvetica Neue", 12.0, false, Rgba::WHITE.with_alpha(if d.online { 0.8 } else { 0.4 }));
        ui.text(&d.name, style, Rect::new(r.x + 54.0, r.y, r.w - 60.0, r.h), Align::Left);
        ui.hit(r, sid(Control::SettingsDevice(d.index)), HitKind::Tap);
    }
    let opts = Rect::new(list.max_x() + 10.0, top, page.max_x() - 10.0 - (list.max_x() + 10.0), list.h);
    section_header(ui, "OPTIONS", Rect::new(opts.x, opts.y, opts.w, SECTION_HEADER_H));
    let body = Rect::new(opts.x, opts.y + SECTION_HEADER_H, opts.w, opts.h - SECTION_HEADER_H);
    let bg = selected.and_then(|i| devices.iter().find(|d| d.index == i)).map(|d| format!("Device-{}-Settings-Background", d.letter())).unwrap_or_else(|| "No-Device-Settings-Background".into());
    ui.background(&bg, body);
    if let Some(i) = selected {
        let f = settings::fields(src, SettingsGroup::Device(i));
        field_grid(ui, &f, (body.x + 10.0, body.y + 10.0), body.w - 10.0, edit, drawn);
    }
}

/// I/O MATRIX: MODE, I/O PRESETS (+ SAVE), # INPUTS / # OUTPUTS, then the driver channel list.
fn io_matrix_page(ui: &mut Ui, src: &dyn PropertySource, view: &ViewState, page: Rect, edit: Option<&TextEdit>, drawn: &mut Vec<Drawn>) {
    let f = settings::fields(src, SettingsGroup::IoMatrix);
    let used = field_grid(ui, &f, (page.x + 10.0, page.y + 10.0), page.w - 10.0, edit, drawn);
    let top = page.y + 10.0 + used + 4.0;
    let col_w = ((page.w - 30.0) / 2.0).floor();
    let row_h = 20.0;
    let inputs = settings::io_routes(src, false);
    let outputs = settings::io_routes(src, true);
    let n = inputs.len().max(outputs.len()) as f32;
    let list_h = (page.max_y() - 10.0 - top - SECTION_HEADER_H - 18.0).max(0.0);
    let overflow = (n * row_h - list_h).max(0.0);
    let off = overflow * view.settings.scroll;
    for (k, (title, routes)) in [("INPUTS", &inputs), ("OUTPUTS", &outputs)].into_iter().enumerate() {
        let x = page.x + 10.0 + k as f32 * (col_w + 10.0);
        section_header(ui, title, Rect::new(x, top, col_w, SECTION_HEADER_H));
        let head = Rect::new(x, top + SECTION_HEADER_H, col_w, 18.0);
        ui.fill(head, Rgba::rgb8(30, 32, 34));
        let hs = ui.style("views", "Settings-Col-Header-Text");
        ui.text("CH", hs.clone(), Rect::new(head.x, head.y, 40.0, head.h), Align::Left);
        ui.text("NAME", hs.clone(), Rect::new(head.x + 40.0, head.y, col_w - 150.0, head.h), Align::Left);
        ui.text("TYPE", hs, Rect::new(head.max_x() - 110.0, head.y, 110.0, head.h), Align::Left);
        let area = Rect::new(x, head.max_y(), col_w, list_h);
        ui.background("Device-Settings-Background", area);
        ui.clip(Some(area));
        let text = Ui::plain_style("Helvetica", 10.0, false, Rgba::WHITE.with_alpha(0.75));
        let dim = Ui::plain_style("Helvetica", 10.0, false, Rgba::WHITE.with_alpha(0.45));
        for (i, r) in routes.iter().enumerate() {
            let y = area.y + i as f32 * row_h - off;
            if y + row_h < area.y || y > area.max_y() {
                continue;
            }
            if i % 2 == 1 {
                ui.fill(Rect::new(area.x, y, area.w, row_h), Rgba::WHITE.with_alpha(0.03));
            }
            let st = if r.active { &text } else { &dim };
            ui.text(&r.channel.to_string(), st.clone(), Rect::new(area.x + 6.0, y, 34.0, row_h), Align::Left);
            ui.text(&r.name, st.clone(), Rect::new(area.x + 40.0, y, col_w - 150.0, row_h), Align::Left);
            ui.text(&r.io_type, dim.clone(), Rect::new(area.max_x() - 110.0, y, 110.0, row_h), Align::Left);
        }
        ui.clip(None);
        ui.scroll_region(area, ScrollTarget::Settings, overflow);
    }
}

/// PLUG-INS: format counts, RESCAN, SHORT NAMES and the catalogue with each plug-in's state.
fn plugins_page(ui: &mut Ui, model: &dyn MixerModel, src: &dyn PropertySource, view: &ViewState, page: Rect, edit: Option<&TextEdit>, drawn: &mut Vec<Drawn>) {
    let f = settings::fields(src, SettingsGroup::PlugIns);
    let used = field_grid(ui, &f, (page.x + 10.0, page.y + 10.0), page.w - 10.0, edit, drawn);
    let top = page.y + 10.0 + used + 4.0;
    section_header(ui, "MANAGE UAD PLUG-INS", Rect::new(page.x + 10.0, top, page.w - 20.0, SECTION_HEADER_H));
    let area = Rect::new(page.x + 10.0, top + SECTION_HEADER_H, page.w - 20.0, (page.max_y() - 10.0 - top - SECTION_HEADER_H).max(0.0));
    ui.background("Device-Settings-Background", area);
    let catalog = model.plugin_catalog();
    let row_h = 20.0;
    let overflow = (catalog.len() as f32 * row_h - area.h).max(0.0);
    let off = overflow * view.settings.scroll;
    ui.clip(Some(area));
    let name = Ui::plain_style("Helvetica", 10.0, false, Rgba::WHITE.with_alpha(0.8));
    let state = Ui::plain_style("Helvetica", 10.0, true, Rgba::WHITE.with_alpha(0.45));
    for (i, p) in catalog.iter().enumerate() {
        let y = area.y + i as f32 * row_h - off;
        if y + row_h < area.y || y > area.max_y() {
            continue;
        }
        if i % 2 == 1 {
            ui.fill(Rect::new(area.x, y, area.w, row_h), Rgba::WHITE.with_alpha(0.03));
        }
        ui.text(p.display_name(), name.clone(), Rect::new(area.x + 8.0, y, area.w - 140.0, row_h), Align::Left);
        let s = if p.enabled { "AUTHORIZED".to_string() } else { p.badge.map(|b| format!("{b:?}").to_uppercase()).unwrap_or_else(|| "INACTIVE".into()) };
        ui.text(&s, state.clone(), Rect::new(area.max_x() - 130.0, y, 122.0, row_h), Align::Right);
    }
    ui.clip(None);
    ui.scroll_region(area, ScrollTarget::Settings, overflow);
}

/// The connection strip under the pages (open-apollo's addition: Console has no write gate):
/// engine host, link state and the WRITES switch.
fn footer(ui: &mut Ui, model: &dyn MixerModel, r: Rect) {
    let st = model.engine_settings();
    ui.fill(r, Rgba::rgb8(30, 32, 34));
    ui.fill(Rect::new(r.x, r.y, r.w, 1.0), Rgba::BLACK.with_alpha(0.5));
    let note = Ui::plain_style("Helvetica", 9.5, false, Rgba::WHITE.with_alpha(0.5));
    let host = if st.host.is_empty() { "no engine".to_string() } else { st.host.clone() };
    ui.fill(Rect::new(r.x + 10.0, r.y + 9.0, 8.0, 8.0), if st.connected { Rgba::rgb8(116, 200, 92) } else { Rgba::rgb8(214, 72, 52) });
    let rate = sample_rate_label(st.sample_rate);
    ui.text(&format!("ENGINE {host}   {}   {rate}", if st.connected { "CONNECTED" } else { "OFFLINE" }), note.clone(), Rect::new(r.x + 24.0, r.y, r.w - 200.0, r.h), Align::Left);
    let sw = Rect::new(r.max_x() - 44.0, r.y + 3.5, 34.0, 19.0);
    ui.toggle("Switch-Small", Some(if st.writes_enabled { "ON" } else { "OFF" }), st.writes_enabled, true, sw, None, None);
    ui.hit(sw, sid(Control::WritesEnabled), HitKind::Toggle);
    let text = if st.writes_enabled { "WRITES  ·  edits are sent to the engine" } else { "WRITES  ·  read-only: set / post refused" };
    ui.text(text, note, Rect::new(sw.x - 250.0, r.y, 242.0, r.h), Align::Right);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_matches_rack_settings_base() {
        // `Rack_SettingsBase` (0x180e8ee90): label (x, y, 160, 58), control (x+5, y+17, 150, 36).
        assert_eq!((CELL_W, CELL_H, CELL_PITCH_X, CELL_PITCH_Y), (160.0, 58.0, 170.0, 68.0));
    }
}
