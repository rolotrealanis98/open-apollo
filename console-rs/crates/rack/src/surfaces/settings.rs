//! The Settings window (`Rack_SettingsPopover` › `Rack_SettingsPage`): the "SETTINGS" title
//! bar, the `Rack_SettingsHeader` tabs (HARDWARE, I/O MATRIX, OPTIONS, MIDI, NETWORK) and the
//! selected page, drawn from [`crate::settings`] fields. Shared widgets (the window chrome,
//! `Silk-Screen` label cells with their combo / text / button, section headers, the combo
//! dropdown) are used by the secondary panels too (`surfaces::panels`).
//!
//! Geometry (window points; [obs] = Rack.dll layout code, [cap] = Console 1.3.1 captures
//! s11…s17 at 2 px/pt):
//! - [cap] Window 1000 × 880: title bar 27, page frame (1 dark, 1 bright, 4.5 light) around
//!   the `Settings-Background` interior.
//! - [obs] `Rack_SettingsPage::resized` (0x180e92d40): content column `min(w − 20, 980)` at
//!   x 10; header at y 20 of the page (47 in the window), `settings_header_height` 48; pages
//!   below it. [cap] The tab band is the header inset 4 / 4 / 4 / 6 (`Display-Background`);
//!   tabs 124 apart (`settings_header_tab`), each with a 96 × 2 top rule.
//! - [obs] `Rack_SettingsBase` cells (0x180e8ee90): `Silk-Screen` box `(x, y, 160, 58)`,
//!   control `(x + 5, y + 17, 150, 36)`, 170 × 68 pitch from (10, 10).
//! - [obs] `Rack_DeviceOptions::resized` (0x180e5c400), `Rack_OptionsSettingsPanel::resized`
//!   (0x180e7c5f0), `Rack_MIDISettings_Apollo::resized` (0x180e6f6f0): offsets in the code.

use houston::{Rect, Rgba};

use crate::draw::Align;
use crate::input::{Control, ControlId, HitKind, ScrollTarget};
use crate::model::MixerModel;
use crate::settings::{self, FieldKind, PropertySource, SettingField, SettingsGroup, SettingsTab};
use crate::menu::{MenuLook, PopupItem, PopupMenu};
use crate::surfaces::popover::{anchored_menu, pid};
use crate::surfaces::view::{TextEdit, TextTarget, ViewState};
use crate::ui::Ui;

pub const SETTINGS_ID: &str = "settings";

/// `Rack_SilkScreen` cell and its control (`Rack_SettingsBase`).
pub const CELL_W: f32 = 160.0;
pub const CELL_H: f32 = 58.0;
pub const CELL_PITCH_X: f32 = 170.0;
pub const CELL_PITCH_Y: f32 = 68.0;
/// The Settings window (capture s11).
pub const WIN_W: f32 = 1000.0;
pub const WIN_H: f32 = 880.0;
pub const TITLE_H: f32 = 27.0;
/// Page frame thickness and the interior inset (dark 1, bright 1, light 4.5).
const FRAME: f32 = 6.5;
/// `settings_header_height`, `settings_header_tab`.
const HEADER_H: f32 = 48.0;
const TAB_W: f32 = 124.0;
/// Content column (`Rack_SettingsPage`): x 10, page top 95, bottom 850.
const COL_X: f32 = 10.0;
const COL_W: f32 = 980.0;
const PAGE_Y: f32 = 95.0;
const PAGE_BOTTOM: f32 = 850.0;
pub(crate) const SECTION_HEADER_H: f32 = 39.0;

const TITLE_TOP: Rgba = Rgba::rgb8(61, 66, 68);
const TITLE_BOTTOM: Rgba = Rgba::rgb8(55, 61, 63);
const EDGE_DARK: Rgba = Rgba::rgb8(105, 105, 105);
const EDGE_BRIGHT: Rgba = Rgba::rgb8(205, 215, 221);
const EDGE_LIGHT: Rgba = Rgba::rgb8(161, 169, 174);
const PAGE_BG: Rgba = Rgba::rgb8(81, 88, 91);
const TAB_ON: Rgba = Rgba::rgb8(213, 145, 0);
const TAB_OFF: Rgba = Rgba::rgb8(56, 59, 62);
const TAB_TEXT_ON: Rgba = Rgba::rgb8(212, 146, 5);
const TAB_TEXT_OFF: Rgba = Rgba::rgb8(87, 93, 99);
const DIVIDER: Rgba = Rgba::rgb8(1, 1, 1);

pub(crate) fn sid(control: Control) -> ControlId {
    ControlId { strip: SETTINGS_ID.into(), control }
}

/// A field drawn this frame (the dropdown looks its options up here).
pub(crate) struct Drawn {
    pub field: SettingField,
}

/// Console's window chrome for Settings and the sub-windows: the title bar with the title and
/// the close X at the left, then the light page frame. Swallows taps on the body. Returns the
/// page interior.
pub(crate) fn window_frame(ui: &mut Ui, r: Rect, title: &str, close: ControlId) -> Rect {
    ui.chrome_layer(r, TITLE_H);
    ui.fill(r.offset(0.0, 6.0), Rgba::BLACK.with_alpha(0.25));
    ui.fill(r, Rgba::rgb8(26, 26, 26));
    ui.hit(r, pid(Control::PopoverDismiss), HitKind::Drag { value: 0.0, span: 1.0, reset: None });
    let bar = Rect::new(r.x + 1.0, r.y + 1.0, r.w - 2.0, TITLE_H - 1.0);
    ui.fill(Rect::new(bar.x, bar.y, bar.w, bar.h / 2.0), TITLE_TOP);
    ui.fill(Rect::new(bar.x, bar.y + bar.h / 2.0, bar.w, bar.h / 2.0), TITLE_BOTTOM);
    let mut style = ui.style("views", "Button-Popover-Header");
    style.color = Rgba::WHITE.with_alpha(0.8);
    ui.text(title, style, Rect::new(r.x, r.y, r.w, TITLE_H), Align::Center);
    if let Some(x) = ui.skin("views", "Browser-Modal-Close-White").and_then(|s| s.image("image")) {
        let x = x.to_string();
        ui.image(&x, Rect::new(r.x + 11.0, r.y + 8.0, 12.0, 12.0), None, None, 0.95);
    }
    ui.hit(Rect::new(r.x, r.y, 34.0, TITLE_H), close, HitKind::Tap);
    let page = Rect::new(r.x, r.y + TITLE_H, r.w, r.h - TITLE_H);
    ui.fill(page, EDGE_DARK);
    ui.fill(page.inset(1.0, 1.0, 1.0, 1.0), EDGE_BRIGHT);
    ui.fill(page.inset(2.0, 2.0, 2.0, 2.0), EDGE_LIGHT);
    let inner = page.inset(FRAME, FRAME, FRAME, FRAME);
    ui.fill(inner, PAGE_BG);
    ui.background("Settings-Background", inner);
    inner
}

/// A section header band (`Hardware-Setup-Device-Header`) with its title centred in
/// `Settings-Section-Header-Static-Text` (the bevel in front reads as white 0.8).
pub(crate) fn section_header(ui: &mut Ui, title: &str, r: Rect) {
    ui.fill(r, Rgba::rgb8(67, 74, 75));
    ui.background("Hardware-Setup-Device-Header", r);
    let mut style = ui.style("fonts", "Settings-Section-Header-Static-Text");
    style.color = Rgba::WHITE.with_alpha(0.8);
    ui.text(title, style, r, Align::Center);
}

/// A page section title (OPTIONS › DISPLAY / EDITING): left, no band.
fn section_title(ui: &mut Ui, title: &str, r: Rect) {
    let mut style = ui.style("fonts", "Settings-Section-Header-Static-Text");
    style.color = Rgba::WHITE.with_alpha(0.8);
    ui.text(title, style, r.inset(5.0, 0.0, 0.0, 0.0), Align::Left);
}

/// `Settings-Static-Text` label (Helvetica 9, white 0.6), in a 16-pt row.
pub(crate) fn static_label(ui: &mut Ui, text: &str, r: Rect, align: Align) {
    let style = ui.style("fonts", "Settings-Static-Text");
    ui.text(text, style, r, align);
}

/// `Combo-Box`: the `display_sml` plate stretched, the value centred, the dropdown arrow
/// 6 pt in from the bottom-right (`inset-overlay`). Tapping opens the field's dropdown.
pub(crate) fn choice_box(ui: &mut Ui, f: &SettingField, r: Rect) {
    let skin = ui.skin("views", "Combo-Box");
    match skin.and_then(|s| s.image("image")) {
        Some(img) => {
            let img = img.to_string();
            ui.image_stretched(&img, r, 1.0);
        }
        None => ui.fill(r, Rgba::rgb8(24, 25, 27)),
    }
    let usable = f.enabled && matches!(&f.kind, FieldKind::Choice { options, .. } if !options.is_empty());
    let mut style = ui.style("views", "Combo-Box");
    if !usable {
        style.color = style.color.with_alpha(0.5);
    }
    ui.text(&f.value_text(), style, Rect::new(r.x + 8.0, r.y + 2.0, r.w - 16.0, r.h - 2.0), Align::Center);
    if let Some(arrow) = skin.and_then(|s| s.image("image-overlay")) {
        let arrow = arrow.to_string();
        ui.image(&arrow, Rect::new(r.max_x() - 6.0 - 8.0, r.max_y() - 6.0 - 8.0, 8.0, 8.0), None, None, 1.0);
    }
    if usable {
        // I/O PRESETS opens its window (capture s38), not a dropdown.
        let control = if &*f.key == "iopreset:/IOMapPreset" { Control::ShowPanel(crate::settings::Panel::IoPresets) } else { Control::SettingMenu(f.key.clone()) };
        ui.hit(r, sid(control), HitKind::Tap);
    }
}

/// `Text-Edit-Field` (`rename_field` art, Helvetica Neue 16, text inset 10) with the caret while
/// it has focus.
pub(crate) fn text_box(ui: &mut Ui, f: &SettingField, text: &str, edit: Option<&TextEdit>, r: Rect) {
    let editing = edit.filter(|e| matches!(&e.target, TextTarget::Setting(k) if *k == f.key));
    let shown = editing.map(|e| format!("{}|", e.text)).unwrap_or_else(|| text.to_string());
    // The `rename_field` art is a translucent dark plate (`inset-background` 2, 3, 2, 3).
    let art = r.inset(2.0, 3.0, 2.0, 3.0);
    if let Some(img) = ui.skin("views", "Text-Edit-Field").and_then(|s| s.image("image")) {
        let img = img.to_string();
        ui.image_stretched(&img, art, 1.0);
    }
    let style = ui.style("views", "Text-Edit-Field");
    ui.clip(Some(r.inset(4.0, 0.0, 4.0, 0.0)));
    ui.text(&shown, style, Rect::new(r.x + 10.0, r.y, r.w - 14.0, r.h), Align::Left);
    ui.clip(None);
    if f.enabled {
        ui.hit(r, sid(Control::SettingEdit(f.key.clone())), HitKind::Tap);
    }
}

/// A `Switch-Large` push button (IDENTIFY, SAVE, RESET, …).
pub(crate) fn push_button(ui: &mut Ui, f: &SettingField, label: &str, r: Rect) {
    ui.toggle("Switch-Large", Some(label), false, f.enabled, r, None, None);
    if f.enabled {
        ui.hit(r, sid(Control::SettingPress(f.key.clone())), HitKind::Tap);
    }
}

fn centred_in(r: Rect, w: f32, h: f32) -> Rect {
    Rect::new(r.x + ((r.w - w) / 2.0).round(), r.y + ((r.h - h) / 2.0).round(), w, h)
}

/// The control of `f` inside `r` (no caption).
fn control(ui: &mut Ui, f: &SettingField, r: Rect, edit: Option<&TextEdit>, drawn: &mut Vec<Drawn>) {
    match &f.kind {
        FieldKind::Choice { .. } => choice_box(ui, f, r),
        FieldKind::Text(t) => text_box(ui, f, t, edit, r),
        FieldKind::Button => push_button(ui, f, &f.label, centred_in(r, 80.0, 40.0)),
        FieldKind::Action(b) => push_button(ui, f, b, centred_in(r, 80.0, 40.0)),
        FieldKind::Toggle { on } => {
            let b = centred_in(r, 80.0, 40.0);
            ui.toggle("Switch-Large", Some(if *on { "ON" } else { "OFF" }), *on, f.enabled, b, None, None);
            if f.enabled {
                ui.hit(b, sid(Control::SettingPress(f.key.clone())), HitKind::Toggle);
            }
        }
        FieldKind::Readout(t) => {
            let style = Ui::plain_style("Helvetica", 11.0, true, Rgba::WHITE.with_alpha(0.75));
            ui.text(t, style, r, Align::Center);
        }
        FieldKind::Slider { .. } => slider(ui, f, r),
    }
    drawn.push(Drawn { field: f.clone() });
}

/// One `Rack_SettingsBase` cell: the `Silk-Screen` box with its caption top-left and the
/// control at (+5, +17, w − 10, 36); an inline second control sits right of the first.
pub(crate) fn field_cell(ui: &mut Ui, f: &SettingField, x: f32, y: f32, edit: Option<&TextEdit>, drawn: &mut Vec<Drawn>) {
    let cell = Rect::new(x, y, f.width, CELL_H);
    ui.background("Silk-Screen", cell);
    let style = ui.style("views", "Silk-Screen");
    ui.text(&f.label, style, Rect::new(x + 6.0, y + 3.0, f.width - 8.0, 14.0), Align::Left);
    let full = Rect::new(x + 5.0, y + 17.0, f.width - 10.0, 36.0);
    match &f.inline {
        Some(second) => {
            // I/O PRESETS › SAVE (70 wide), TAP TEMPO EVENT › note number (Rack: the combo
            // loses 44, the 40 × 38 field sits 4 pt after it).
            let text = matches!(second.kind, FieldKind::Text(_));
            let w2 = if text { 40.0 } else { 70.0 };
            let first = Rect::new(full.x, full.y, full.w - w2 - 4.0, full.h);
            control(ui, f, first, edit, drawn);
            let r2 = Rect::new(first.max_x() + 4.0, full.y, w2, if text { 38.0 } else { full.h });
            match &second.kind {
                FieldKind::Button => {
                    ui.toggle("Switch-Large", Some(&second.label), false, second.enabled, r2, None, None);
                    if second.enabled {
                        ui.hit(r2, sid(Control::SettingPress(second.key.clone())), HitKind::Tap);
                    }
                    drawn.push(Drawn { field: (**second).clone() });
                }
                _ => control(ui, second, r2, edit, drawn),
            }
        }
        None => control(ui, f, full, edit, drawn),
    }
}

/// Lays `fields` out in Console's cell grid: each row 68 pt below the last, cells left to
/// right (`width` + 10 apart), or at their explicit `at`. Returns the height used.
pub(crate) fn field_grid(ui: &mut Ui, fields: &[SettingField], origin: (f32, f32), _max_w: f32, edit: Option<&TextEdit>, drawn: &mut Vec<Drawn>) -> f32 {
    let mut rows: Vec<u8> = fields.iter().map(|f| f.row).collect();
    rows.sort_unstable();
    rows.dedup();
    let mut bottom: f32 = 0.0;
    for (ri, row) in rows.iter().enumerate() {
        let mut x = 0.0;
        for f in fields.iter().filter(|f| f.row == *row) {
            let (cx, cy) = f.at.unwrap_or((x, ri as f32 * CELL_PITCH_Y));
            field_cell(ui, f, origin.0 + cx, origin.1 + cy, edit, drawn);
            x = cx + f.width + 10.0;
            bottom = bottom.max(cy + CELL_PITCH_Y);
        }
    }
    bottom
}

/// CHANNEL DSP PAIRING slider (`DSP-Spanning-Background`, 210 × 40): the left number, the
/// segment row (one lit cell) and the right number.
pub(crate) fn slider(ui: &mut Ui, f: &SettingField, r: Rect) {
    let FieldKind::Slider { segments, lit, left, right, .. } = &f.kind else { return };
    ui.fill(r.inset(1.0, 3.0, 1.0, 3.0), Rgba::rgb8(20, 22, 24));
    ui.background("DSP-Spanning-Background", r);
    let num = Ui::plain_style("Helvetica", 12.0, false, Rgba::WHITE.with_alpha(0.8));
    ui.text(left, num.clone(), Rect::new(r.x + 6.0, r.y, 20.0, r.h), Align::Center);
    ui.text(right, num, Rect::new(r.max_x() - 26.0, r.y, 20.0, r.h), Align::Center);
    let track = Rect::new(r.x + 29.0, r.y + r.h / 2.0 - 4.0, r.w - 58.0, 8.0);
    let n = (*segments).max(1) as f32;
    let seg = (track.w / n).floor();
    for i in 0..*segments {
        let c = Rect::new(track.x + i as f32 * seg, track.y, seg - 1.0, track.h);
        ui.fill(c, if i == *lit { Rgba::rgb8(214, 166, 52) } else { Rgba::rgb8(58, 50, 34) });
    }
}

/// The open combo's dropdown (the MENUS look: `anchored_menu`, `MenuLook::Base`), under its
/// box (drawn last; captures s18…s39 show the items, the current one ticked).
pub(crate) fn choice_menu(ui: &mut Ui, view: &ViewState, drawn: &[Drawn], bounds: Rect) {
    let Some(key) = &view.settings.menu else { return };
    let Some(d) = drawn.iter().find(|d| &d.field.key == key) else { return };
    let FieldKind::Choice { options, enabled, selected } = &d.field.kind else { return };
    let Some(anchor) = ui.hit_rect(&sid(Control::SettingMenu(key.clone()))) else { return };
    let mut m = PopupMenu::new();
    for (i, o) in options.iter().enumerate() {
        m.push(PopupItem::new(o.clone(), sid(Control::SettingChoose(key.clone(), i as u16))).ticked(*selected == Some(i)).enabled(enabled.get(i).copied().unwrap_or(true)));
    }
    ui.popover_catcher(bounds);
    anchored_menu(ui, &m, MenuLook::Base, anchor, bounds);
}

/// The window rect for `size` centred in `bounds` (clamped to them).
pub(crate) fn centred(bounds: Rect, size: (f32, f32)) -> Rect {
    let (w, h) = (size.0.min(bounds.w), size.1.min(bounds.h));
    Rect::new(bounds.x + ((bounds.w - w) / 2.0).round(), bounds.y + ((bounds.h - h) / 2.0).round(), w, h)
}

/// The Settings window over `bounds` (modal: the backdrop swallows taps).
pub fn draw_settings_sheet(ui: &mut Ui, model: &dyn MixerModel, view: &ViewState, bounds: Rect) {
    // A separate window does not dim the mixer (`crate::layers`).
    if !ui.windowed() {
        ui.fill(bounds, Rgba::BLACK.with_alpha(0.5));
        ui.hit(bounds, pid(Control::PopoverDismiss), HitKind::Tap);
    }
    let r = centred(bounds, (WIN_W, WIN_H));
    window_frame(ui, r, "SETTINGS", sid(Control::SettingsClose));
    footer(ui, model, Rect::new(r.x + COL_X + 10.0, r.y + PAGE_BOTTOM + 2.0, COL_W - 20.0, 19.0));

    let Some(src) = model.properties() else { return };
    let tabs = settings::tabs(src);
    let tab = if tabs.contains(&view.settings.tab) { view.settings.tab } else { tabs[0] };

    // Header (`Rack_SettingsHeader`): the tab band and the tabs.
    let header = Rect::new(r.x + COL_X, r.y + 47.0, COL_W, HEADER_H);
    let band = header.inset(4.0, 4.0, 4.0, 6.0);
    ui.fill(band, Rgba::rgb8(34, 34, 34));
    ui.background("Display-Background", band);
    for (i, t) in tabs.iter().enumerate() {
        let tr = Rect::new(header.x + 3.0 + i as f32 * TAB_W, band.y, TAB_W, band.h);
        let on = *t == tab;
        ui.fill(Rect::new(tr.x + 14.0, tr.y + 8.0, 96.0, 2.0), if on { TAB_ON } else { TAB_OFF });
        let style = Ui::plain_style("Helvetica", 10.0, true, if on { TAB_TEXT_ON } else { TAB_TEXT_OFF });
        ui.text(t.label(), style, Rect::new(tr.x, tr.y + 15.0, tr.w, 15.0), Align::Center);
        ui.hit(tr, sid(Control::SettingsTab(*t)), HitKind::Tap);
    }

    let page = Rect::new(r.x + COL_X, r.y + PAGE_Y, COL_W, PAGE_BOTTOM - PAGE_Y);
    let mut drawn = Vec::new();
    ui.clip(Some(page));
    let edit = view.text_edit.as_ref();
    match tab {
        SettingsTab::Hardware => hardware_page(ui, src, view, page, edit, &mut drawn),
        SettingsTab::IoMatrix => io_matrix_page(ui, src, view, page, edit, &mut drawn),
        SettingsTab::Options => {
            // `Rack_OptionsSettingsPanel`: DISPLAY at (10, 20), its cells from 40 (+10);
            // EDITING at DISPLAY's height (146) + 90.
            let display = settings::fields(src, SettingsGroup::Display);
            section_title(ui, "DISPLAY", Rect::new(page.x + 10.0, page.y + 20.0, 200.0, 16.0));
            let h = field_grid(ui, &display, (page.x + 10.0, page.y + 50.0), page.w, edit, &mut drawn) + 10.0;
            let y = page.y + h.max(146.0) + 90.0;
            let editing = settings::fields(src, SettingsGroup::Editing);
            if !editing.is_empty() {
                section_title(ui, "EDITING", Rect::new(page.x + 10.0, y, 200.0, 16.0));
                field_grid(ui, &editing, (page.x + 10.0, y + 30.0), page.w, edit, &mut drawn);
            }
        }
        SettingsTab::Midi => {
            let f = settings::fields(src, SettingsGroup::Midi);
            field_grid(ui, &f, (page.x + 10.0, page.y + 10.0), page.w, edit, &mut drawn);
        }
        SettingsTab::Network => network_page(ui, src, page, edit, &mut drawn),
        SettingsTab::PlugIns | SettingsTab::DanteDevices | SettingsTab::IoSettings | SettingsTab::Controllers => {}
    }
    ui.clip(None);
    choice_menu(ui, view, &drawn, bounds);
}

/// HARDWARE: the global grid (`Rack_ApolloSettings`), then DEVICES (`Rack_DeviceList`) and the
/// selected device's OPTIONS (`Rack_DeviceOptions`), one grid row below the grid.
fn hardware_page(ui: &mut Ui, src: &dyn PropertySource, view: &ViewState, page: Rect, edit: Option<&TextEdit>, drawn: &mut Vec<Drawn>) {
    let f = settings::fields(src, SettingsGroup::Hardware);
    let grid_h = field_grid(ui, &f, (page.x + 10.0, page.y + 10.0), page.w, edit, drawn);
    let top = page.y + 10.0 + grid_h;
    let bottom = page.max_y();
    let devices = settings::devices(src);
    // DEVICES (199) | 2-pt rule | OPTIONS.
    let list = Rect::new(page.x + 10.0, top, 199.0, bottom - top);
    let opts = Rect::new(page.x + 211.0, top, page.w - 221.0, bottom - top);
    section_header(ui, "DEVICES", Rect::new(list.x, top, list.w, SECTION_HEADER_H));
    section_header(ui, "OPTIONS", Rect::new(opts.x, top, opts.w, SECTION_HEADER_H));
    ui.fill(Rect::new(list.max_x(), top, 2.0, bottom - top), DIVIDER);
    let body_y = top + SECTION_HEADER_H + 1.0;
    ui.fill(Rect::new(list.x, top + SECTION_HEADER_H, list.w, 1.0), DIVIDER);
    ui.fill(Rect::new(opts.x, top + SECTION_HEADER_H, opts.w, 1.0), DIVIDER);
    let list_body = Rect::new(list.x, body_y, list.w, bottom - body_y);
    ui.fill(list_body, Rgba::rgb8(43, 46, 48));
    ui.background("Device-Settings-Background", list_body);
    let selected = devices.iter().find(|d| d.index == view.settings.device).or(devices.first()).map(|d| d.index);
    for (k, d) in devices.iter().enumerate() {
        device_row(ui, d, Rect::new(list.x, body_y + k as f32 * 61.0, 200.0, 60.0));
    }
    // The "+" row (ADD DEVICE).
    let add = Rect::new(list.x, body_y + devices.len() as f32 * 61.0, 200.0, 60.0);
    ui.background("New-Device-Background", add);
    let plus = Ui::plain_style("Helvetica", 12.0, false, Rgba::WHITE.with_alpha(0.5));
    ui.text("+", plus, add, Align::Center);
    ui.hit(add, sid(Control::ShowPanel(crate::settings::Panel::AddDevice)), HitKind::Tap);

    let body = Rect::new(opts.x, body_y, opts.w, bottom - body_y);
    let bg = selected.and_then(|i| devices.iter().find(|d| d.index == i)).map(|d| format!("Device-{}-Settings-Background", d.letter())).unwrap_or_else(|| "No-Device-Settings-Background".into());
    ui.fill(body, Rgba::rgb8(28, 35, 42));
    ui.background(&bg, body);
    if let Some(i) = selected {
        device_options(ui, &settings::fields(src, SettingsGroup::Device(i)), (body.x, body.y), edit, drawn);
    }
}

/// A DEVICES row (`Device-A-Background` …, 200 × 60): unit letter, monitor / talkback icons,
/// name, device icon.
fn device_row(ui: &mut Ui, d: &settings::SettingsDevice, r: Rect) {
    ui.background(&format!("Device-{}-Background", d.letter()), r);
    let letter = Ui::plain_style("Helvetica", 11.0, true, Rgba::WHITE.with_alpha(if d.online { 0.85 } else { 0.4 }));
    ui.text(&d.letter().to_string(), letter, Rect::new(r.x + 6.0, r.y, 14.0, r.h), Align::Center);
    ui.background("Speaker-Icon", Rect::new(r.x + 19.0, r.y + 13.0, 12.0, 11.0));
    ui.image("backgrounds/icon_talkback_microphone@2x.png", Rect::new(r.x + 21.0, r.y + 32.0, 9.0, 15.0), None, None, 1.0);
    let name = Ui::plain_style("Helvetica", 11.0, true, Rgba::WHITE.with_alpha(if d.online { 0.9 } else { 0.45 }));
    ui.text(&d.name, name, Rect::new(r.x + 59.0, r.y, 80.0, r.h), Align::Left);
    let key = d.icon_key();
    if let Some((w, h)) = ui.skin("backgrounds", &key).and_then(|s| s.size()) {
        ui.background(&key, Rect::new(r.x + 141.0, r.y + ((r.h - h) / 2.0).round(), w, h));
    }
    ui.hit(r, sid(Control::SettingsDevice(d.index)), HitKind::Tap);
}

/// `Rack_DeviceOptions::resized` (0x180e5c400), in panel points: DEVICE NAME (10, 10) over the
/// 350 × 40 field at (10, 26) and the button at (360, 26); then rows of 100 × 40 combos 110
/// apart under 16-pt labels: y 76 (digital I/O, MON LEVEL), +66 OUTPUT REFERENCE LEVELS (a
/// caption, then labelled combos), then CHANNEL DSP PAIRING (caption, DSP PAIRS / VIRTUAL
/// CHANNELS, the 210 × 40 slider).
fn device_options(ui: &mut Ui, fields: &[SettingField], o: (f32, f32), edit: Option<&TextEdit>, drawn: &mut Vec<Drawn>) {
    let (ox, oy) = o;
    let row = |n: u8| fields.iter().filter(move |f| f.row == n);
    if let Some(name) = row(0).next() {
        static_label(ui, &name.label, Rect::new(ox + 10.0, oy + 10.0, 300.0, 16.0), Align::Left);
        if let FieldKind::Text(t) = &name.kind {
            text_box(ui, name, t, edit, Rect::new(ox + 10.0, oy + 26.0, 350.0, 40.0));
            drawn.push(Drawn { field: name.clone() });
        }
        if let Some(b) = &name.inline {
            push_button(ui, b, &b.label, Rect::new(ox + 360.0, oy + 26.0, 80.0, 40.0));
        }
    }
    let mut y = 76.0;
    let mut combos = |ui: &mut Ui, list: Vec<&SettingField>, y: f32| {
        for (k, f) in list.into_iter().enumerate() {
            let x = ox + 10.0 + k as f32 * 110.0;
            static_label(ui, &f.label, Rect::new(x, oy + y, 100.0, 16.0), Align::Left);
            control(ui, f, Rect::new(x, oy + y + 16.0, 100.0, 40.0), edit, drawn);
        }
    };
    let r1: Vec<&SettingField> = row(1).collect();
    if !r1.is_empty() {
        combos(ui, r1, y);
        y += 66.0;
    }
    for (title, n) in [("OUTPUT REFERENCE LEVELS", 2u8), ("LINE INPUT GAIN", 3u8)] {
        let list: Vec<&SettingField> = row(n).collect();
        if list.is_empty() {
            continue;
        }
        static_label(ui, title, Rect::new(ox + 10.0, oy + y, 400.0, 16.0), Align::Left);
        combos(ui, list, y + 20.0);
        y += 20.0 + 56.0 + 10.0;
    }
    let mut slider_at = None;
    if let Some(f) = row(4).next() {
        if let FieldKind::Slider { left_label, right_label, .. } = &f.kind {
            static_label(ui, &f.label, Rect::new(ox + 10.0, oy + y, 204.0, 16.0), Align::Left);
            static_label(ui, left_label, Rect::new(ox + 10.0, oy + y + 20.0, 204.0, 16.0), Align::Left);
            static_label(ui, right_label, Rect::new(ox + 10.0, oy + y + 20.0, 204.0, 16.0), Align::Right);
            slider_at = Some((f.clone(), Rect::new(ox + 10.0, oy + y + 36.0, 210.0, 40.0)));
            y += 36.0 + 40.0 + 10.0;
        }
    }
    let rest: Vec<&SettingField> = row(5).collect();
    if !rest.is_empty() {
        combos(ui, rest, y + 10.0);
    }
    if let Some((f, r)) = slider_at {
        slider(ui, &f, r);
        drawn.push(Drawn { field: f });
    }
}

/// I/O MATRIX: MODE, I/O PRESETS (+ SAVE), # INPUTS / # OUTPUTS, EDIT (CASCADE); then the
/// INPUTS and OUTPUTS tables (CH, DEVICE, INPUT / OUTPUT, CUSTOM NAME; 40-pt rows).
fn io_matrix_page(ui: &mut Ui, src: &dyn PropertySource, view: &ViewState, page: Rect, edit: Option<&TextEdit>, drawn: &mut Vec<Drawn>) {
    let f = settings::fields(src, SettingsGroup::IoMatrix);
    let used = field_grid(ui, &f, (page.x + 10.0, page.y + 10.0), page.w, edit, drawn);
    let top = page.y + 10.0 + used;
    let table = Rect::new(page.x + 10.0, top, page.w - 20.0, page.max_y() - top);
    let half = 471.0;
    let row_h = 40.0;
    let devices = settings::devices(src);
    let dev_name = devices.first().map(|d| d.name.clone()).unwrap_or_default();
    let inputs = settings::io_routes(src, false);
    let outputs = settings::io_routes(src, true);
    let n = inputs.len().max(outputs.len()) as f32;
    let list_y = top + 29.0 + 26.0;
    let list_h = (table.max_y() - list_y).max(0.0);
    let overflow = (n * row_h - list_h).max(0.0);
    let off = (overflow * view.settings.scroll).round();
    for (k, (title, kind, routes)) in [("INPUTS", "INPUT", &inputs), ("OUTPUTS", "OUTPUT", &outputs)].into_iter().enumerate() {
        let x = table.x + k as f32 * half;
        let w = if k == 0 { half } else { table.w - half };
        section_header(ui, title, Rect::new(x, top, w, 29.0));
        let head = Rect::new(x, top + 29.0, w, 26.0);
        ui.fill(head, Rgba::rgb8(48, 53, 55));
        ui.fill(Rect::new(x, head.max_y() - 1.0, w, 1.0), DIVIDER);
        let cols = [(0.0, 40.0, "CH"), (40.0, 95.0, "DEVICE"), (135.0, 77.0, kind), (212.0, w - 212.0, "CUSTOM NAME")];
        let hs = ui.style("views", "Settings-Col-Header-Text");
        for (cx, cw, label) in cols {
            ui.text(label, hs.clone(), Rect::new(x + cx, head.y, cw, head.h), Align::Center);
            if cx > 0.0 {
                ui.fill(Rect::new(x + cx, head.y, 1.0, head.h), Rgba::rgb8(30, 33, 35));
            }
        }
        let area = Rect::new(x, list_y, w, list_h);
        ui.fill(area, Rgba::rgb8(28, 35, 42));
        ui.clip(Some(area));
        let num = Ui::plain_style("Helvetica", 11.0, true, Rgba::WHITE.with_alpha(0.9));
        let text = Ui::plain_style("Helvetica", 10.0, false, Rgba::WHITE.with_alpha(0.85));
        let mut dim = Ui::plain_style("Helvetica", 10.0, false, Rgba::WHITE.with_alpha(0.3));
        dim.italic = true;
        for (i, r) in routes.iter().enumerate() {
            let y = area.y + i as f32 * row_h - off;
            if y + row_h < area.y || y > area.max_y() {
                continue;
            }
            ui.fill(Rect::new(x, y + row_h - 1.0, w, 1.0), Rgba::rgb8(36, 42, 47));
            ui.text(&r.channel.to_string(), num.clone(), Rect::new(x, y, 40.0, row_h), Align::Center);
            ui.fill(Rect::new(x + 42.0, y, 5.0, row_h), Rgba::rgb8(7, 105, 169));
            let st = if r.active { &text } else { &dim };
            ui.text(&dev_name, st.clone(), Rect::new(x + 40.0, y, 95.0, row_h), Align::Center);
            ui.text(&r.default_name, st.clone(), Rect::new(x + 135.0, y, 77.0, row_h), Align::Center);
            // The INPUT / OUTPUT cell opens the cascading source picker (capture s35).
            ui.hit(Rect::new(x + 40.0, y, 172.0, row_h), sid(Control::ShowPanel(crate::settings::Panel::RouteSource { route: i as u16, outputs: k == 1 })), HitKind::Tap);
            ui.text(&r.name, text.clone(), Rect::new(x + 222.0, y, w - 232.0, row_h), Align::Left);
        }
        ui.clip(None);
        ui.scroll_region(area, ScrollTarget::Settings, overflow);
        if k == 1 && overflow > 0.0 {
            // The shared scrollbar at the right of OUTPUTS (10 wide, 5 in from the edge).
            let track = Rect::new(x + w - 15.0, area.y + 4.0, 10.0, area.h - 8.0);
            ui.fill(track, Rgba::rgb8(20, 24, 28));
            let th = (track.h * list_h / (n * row_h)).max(24.0);
            let ty = track.y + (track.h - th) * view.settings.scroll;
            ui.fill(Rect::new(track.x + 2.0, ty, 6.0, th), Rgba::rgb8(70, 76, 80));
            ui.hit(track, sid(Control::SettingsScroll), HitKind::Drag { value: 1.0 - view.settings.scroll as f64, span: track.h, reset: None });
        }
    }
}

/// NETWORK: NETWORK INTERFACE, DANTE CONTROLLER, DANTE PATENT INFORMATION, DANTE DISCOVERY,
/// then the Dante NETWORK SETTINGS table (one row per Dante device).
fn network_page(ui: &mut Ui, src: &dyn PropertySource, page: Rect, edit: Option<&TextEdit>, drawn: &mut Vec<Drawn>) {
    let f = settings::fields(src, SettingsGroup::Network);
    let used = field_grid(ui, &f, (page.x + 10.0, page.y + 10.0), page.w, edit, drawn);
    let top = page.y + 10.0 + used;
    let t = Rect::new(page.x + 10.0, top, page.w - 20.0, 82.0);
    section_header(ui, "", Rect::new(t.x, t.y, t.w, 30.0));
    // `Dante-Logo-Small` (60 × 22) then NETWORK SETTINGS, centred together.
    let mut hs_title = ui.style("fonts", "Settings-Section-Header-Static-Text");
    hs_title.color = Rgba::WHITE.with_alpha(0.8);
    let tw = crate::window::helvetica_bold_width("NETWORK SETTINGS", hs_title.size) + 2.0;
    let x0 = t.x + ((t.w - (44.0 + tw)) / 2.0).round();
    ui.background("Dante-Logo-Small", Rect::new(x0 - 8.0, t.y + 4.0, 60.0, 22.0));
    ui.text("NETWORK SETTINGS", hs_title, Rect::new(x0 + 44.0, t.y, tw + 4.0, 30.0), Align::Left);
    let hs = ui.style("views", "Settings-Col-Header-Text");
    let groups = Rect::new(t.x, t.y + 30.0, t.w, 26.0);
    ui.fill(groups, Rgba::rgb8(58, 63, 66));
    let cols = Rect::new(t.x, groups.max_y(), t.w, 26.0);
    ui.fill(cols, Rgba::rgb8(58, 63, 66));
    let w = t.w;
    let spec: [(f32, f32, &str); 12] = [
        (0.0, 0.11, "DEVICE"),
        (0.11, 0.11, "DANTE NAME"),
        (0.22, 0.05, "IDENTIFY"),
        (0.27, 0.05, "CLOCK\nSYNC"),
        (0.32, 0.075, "IP MODE"),
        (0.395, 0.095, "IP"),
        (0.49, 0.095, "MAC"),
        (0.585, 0.075, "IP MODE"),
        (0.66, 0.095, "IP"),
        (0.755, 0.095, "MAC"),
        (0.85, 0.04, "RESET"),
        (0.89, 0.11, "REDUNDANCY"),
    ];
    for (fx, fw, label) in spec {
        ui.text(label, hs.clone(), Rect::new(t.x + fx * w, cols.y, fw * w, cols.h), Align::Center);
        ui.fill(Rect::new(t.x + fx * w, cols.y, 1.0, cols.h), Rgba::rgb8(36, 40, 42));
    }
    ui.text("PRIMARY", hs.clone(), Rect::new(t.x + 0.32 * w, groups.y, 0.265 * w, groups.h), Align::Center);
    ui.text("SECONDARY", hs, Rect::new(t.x + 0.585 * w, groups.y, 0.265 * w, groups.h), Align::Center);
    let rows = settings::fields(src, SettingsGroup::Dante);
    let text = Ui::plain_style("Helvetica", 10.0, false, Rgba::WHITE.with_alpha(0.8));
    for (i, r) in rows.chunks(4).enumerate() {
        let y = cols.max_y() + i as f32 * 30.0;
        for (k, f) in r.iter().enumerate() {
            ui.text(&f.value_text(), text.clone(), Rect::new(t.x + [0.0, 0.11, 0.395, 0.85][k] * w, y, 0.11 * w, 30.0), Align::Center);
        }
    }
}

/// open-apollo's connection strip under the page (Console has no write gate): the engine host,
/// the link state and the WRITES switch.
fn footer(ui: &mut Ui, model: &dyn MixerModel, r: Rect) {
    let st = model.engine_settings();
    let note = Ui::plain_style("Helvetica", 9.5, false, Rgba::WHITE.with_alpha(0.55));
    let host = if st.host.is_empty() { "no engine".to_string() } else { st.host.clone() };
    ui.fill(Rect::new(r.x + 2.0, r.y + 6.0, 7.0, 7.0), if st.connected { Rgba::rgb8(116, 200, 92) } else { Rgba::rgb8(214, 72, 52) });
    ui.text(&format!("ENGINE {host}   {}", if st.connected { "CONNECTED" } else { "OFFLINE" }), note.clone(), Rect::new(r.x + 14.0, r.y, r.w - 300.0, r.h), Align::Left);
    let sw = Rect::new(r.max_x() - 34.0, r.y, 34.0, 19.0);
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
        // Capture s11: cells at window (20, 105), DEVICES / OPTIONS headers at y 241.
        assert_eq!((COL_X + 10.0, PAGE_Y + 10.0, PAGE_Y + 10.0 + 2.0 * CELL_PITCH_Y), (20.0, 105.0, 241.0));
    }
}
