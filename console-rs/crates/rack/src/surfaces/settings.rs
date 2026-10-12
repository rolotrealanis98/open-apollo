//! Settings sheet (placeholder modal): engine host / connection, sample rate, clock source,
//! buffer size and the "Writes enabled" gate.

use houston::{Rect, Rgba};

use crate::draw::Align;
use crate::input::{Control, ControlId, HitKind};
use crate::model::MixerModel;
use crate::surfaces::monitor::combo_box;
use crate::surfaces::popover::{pid, sample_rate_label};
use crate::ui::Ui;

pub const SETTINGS_ID: &str = "settings";

fn sid(control: Control) -> ControlId {
    ControlId { strip: SETTINGS_ID.into(), control }
}

pub fn draw_settings_sheet(ui: &mut Ui, model: &dyn MixerModel, bounds: Rect) {
    let st = model.engine_settings();
    // Backdrop: dims the window and swallows taps (a no-op dismiss of any popover).
    ui.fill(bounds, Rgba::BLACK.with_alpha(0.5));
    ui.hit(bounds, pid(Control::PopoverDismiss), HitKind::Tap);
    let (w, h) = (460.0, 318.0);
    let r = Rect::new(bounds.x + ((bounds.w - w) / 2.0).round(), bounds.y + ((bounds.h - h) / 2.5).round(), w, h);
    ui.fill(r.offset(3.0, 4.0), Rgba::BLACK.with_alpha(0.4));
    ui.fill(r, Rgba::rgb8(16, 17, 18));
    ui.fill(r.inset(1.0, 1.0, 1.0, 1.0), Rgba::rgb8(44, 47, 50));
    ui.hit(r, pid(Control::PopoverDismiss), HitKind::Drag { value: 0.0, span: 1.0, reset: None });
    let band = Rect::new(r.x + 1.0, r.y + 1.0, r.w - 2.0, 30.0);
    ui.fill(band, Rgba::rgb8(34, 36, 39));
    if let Some(img) = ui.skin("backgrounds", "Popover-Header").and_then(|s| s.image("image")) {
        let img = img.to_string();
        ui.image_stretched(&img, band, 1.0);
    }
    ui.text("SETTINGS", Ui::plain_style("Helvetica", 13.0, true, Rgba::WHITE.with_alpha(0.75)), band, Align::Center);
    if let Some(x) = ui.skin("views", "Browser-Modal-Close-White").and_then(|s| s.image("image")) {
        let x = x.to_string();
        ui.image(&x, Rect::new(band.max_x() - 24.0, band.y + 8.0, 14.0, 14.0), None, None, 0.9);
    }
    ui.hit(Rect::new(band.max_x() - 30.0, band.y, 30.0, 30.0), sid(Control::SettingsClose), HitKind::Tap);

    let label = Ui::plain_style("Helvetica", 10.0, true, Rgba::rgb8(134, 145, 150));
    let value = Ui::plain_style("Helvetica Neue", 12.0, false, Rgba::WHITE.with_alpha(0.85));
    let note = Ui::plain_style("Helvetica", 9.5, false, Rgba::WHITE.with_alpha(0.45));
    let x0 = r.x + 24.0;
    let vx = r.x + 170.0;
    let vw = r.w - 194.0;
    let mut y = r.y + 46.0;
    let row = |ui: &mut Ui, text: &str, y: f32| ui.text(text, label.clone(), Rect::new(x0, y, 140.0, 26.0), Align::Left);

    row(ui, "ENGINE", y);
    let host = if st.host.is_empty() { "not configured".to_string() } else { st.host.clone() };
    ui.fill(Rect::new(vx, y + 9.0, 8.0, 8.0), if st.connected { Rgba::rgb8(116, 200, 92) } else { Rgba::rgb8(214, 72, 52) });
    ui.text(&host, value.clone(), Rect::new(vx + 16.0, y, vw - 16.0, 26.0), Align::Left);
    ui.text(if st.connected { "connected" } else { "offline" }, note.clone(), Rect::new(vx, y, vw, 26.0), Align::Right);
    y += 40.0;

    let combos: [(&str, String, Control); 3] = [
        ("SAMPLE RATE", sample_rate_label(st.sample_rate), Control::SettingsRate),
        ("CLOCK SOURCE", st.clock_source.clone().unwrap_or_else(|| "—".into()), Control::SettingsClock),
        ("BUFFER SIZE", st.buffer_size.map(|b| format!("{b} samples")).unwrap_or_else(|| "—".into()), Control::SettingsBuffer),
    ];
    for (l, v, c) in combos {
        row(ui, l, y);
        let cr = Rect::new(vx, y + 1.0, 170.0, 24.0);
        combo_box(ui, &v, cr);
        ui.hit(cr, sid(c), HitKind::Tap);
        y += 36.0;
    }
    ui.fill(Rect::new(x0, y + 2.0, r.w - 48.0, 1.0), Rgba::WHITE.with_alpha(0.08));
    y += 12.0;
    row(ui, "WRITES ENABLED", y + 7.0);
    let sw = Rect::new(vx, y, 40.0, 40.0);
    ui.toggle("Switch", Some(if st.writes_enabled { "ON" } else { "OFF" }), st.writes_enabled, true, sw, None, None);
    ui.hit(sw, sid(Control::WritesEnabled), HitKind::Toggle);
    let text = if st.writes_enabled { "Edits are sent to the engine." } else { "Read-only: set / post are refused." };
    ui.text(text, note, Rect::new(vx + 48.0, y, vw - 48.0, 40.0), Align::Left);
}
