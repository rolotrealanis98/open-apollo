//! Small drawing helpers shared by the pages.

use eframe::egui::{
    self, Align, Button, Color32, CornerRadius, Frame, Layout, Margin, Rect, Response, RichText, ScrollArea, Sense,
    Shape, Spinner, Stroke, Ui, Vec2, pos2, vec2,
};

use super::theme::*;
use crate::installlog::{Line, SubStep};
use crate::steps::Status;
use crate::syscheck::Level;

/// A round status mark: pending ring, spinner, check, "!", cross, or dash.
pub fn status_icon(ui: &mut Ui, status: Status, size: f32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    paint_status(ui, rect, status);
    resp
}

pub fn paint_status(ui: &mut Ui, rect: Rect, status: Status) {
    let p = ui.painter();
    let c = rect.center();
    let r = rect.width() / 2.0 - 1.0;
    let mark = Stroke::new((r * 0.22).max(1.6), DEEP);
    match status {
        Status::Pending => {
            p.circle_stroke(c, r - 0.5, Stroke::new(1.5, FAINT));
        }
        Status::Running => {
            ui.put(rect, Spinner::new().size(rect.width()).color(INFO));
        }
        Status::Done => {
            p.circle_filled(c, r, ACCENT);
            let pts = [pos2(-0.45, 0.02), pos2(-0.12, 0.34), pos2(0.46, -0.3)];
            p.add(Shape::line(pts.iter().map(|q| c + q.to_vec2() * r).collect(), mark));
        }
        Status::Warning => {
            p.circle_filled(c, r, WARN);
            p.line_segment([c + vec2(0.0, -0.48) * r, c + vec2(0.0, 0.12) * r], mark);
            p.circle_filled(c + vec2(0.0, 0.42) * r, mark.width * 0.62, DEEP);
        }
        Status::Failed => {
            p.circle_filled(c, r, FAIL);
            let d = 0.36 * r;
            p.line_segment([c + vec2(-d, -d), c + vec2(d, d)], mark);
            p.line_segment([c + vec2(-d, d), c + vec2(d, -d)], mark);
        }
        Status::Skipped => {
            p.circle_stroke(c, r - 0.5, Stroke::new(1.5, FAINT));
            p.line_segment([c + vec2(-0.4 * r, 0.0), c + vec2(0.4 * r, 0.0)], Stroke::new(1.8, WEAK));
        }
    }
}

pub fn level_status(level: Level) -> Status {
    match level {
        Level::Ok => Status::Done,
        Level::Info => Status::Skipped,
        Level::Warn => Status::Warning,
    }
}

pub fn status_color(status: Status) -> Color32 {
    match status {
        Status::Done => ACCENT,
        Status::Warning => WARN,
        Status::Failed => FAIL,
        Status::Running => INFO,
        Status::Pending | Status::Skipped => WEAK,
    }
}

/// open-apollo's mark: three faders, as in packaging/uaconsole.svg.
pub fn logo(ui: &mut Ui, size: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    let p = ui.painter();
    let s = size / 64.0;
    let at = |x: f32, y: f32| rect.min + vec2(x * s, y * s);
    p.rect_filled(
        Rect::from_min_max(at(4.0, 4.0), at(60.0, 60.0)),
        CornerRadius::same((10.0 * s) as u8),
        Color32::from_rgb(0x26, 0x29, 0x2e),
    );
    for x in [18.0, 32.0, 46.0] {
        p.line_segment([at(x, 14.0), at(x, 50.0)], Stroke::new(3.0 * s, Color32::from_rgb(0x55, 0x5c, 0x66)));
    }
    for (x, y) in [(12.0, 34.0), (26.0, 20.0), (40.0, 28.0)] {
        p.rect_filled(
            Rect::from_min_size(at(x, y), vec2(12.0 * s, 7.0 * s)),
            CornerRadius::same((2.0 * s) as u8),
            Color32::from_rgb(0xd9, 0xdd, 0xe3),
        );
        p.rect_filled(Rect::from_min_size(at(x + 4.5, y + 2.5), vec2(3.0 * s, 2.0 * s)), CornerRadius::same(1), ACCENT);
    }
}

/// A rounded panel; `accent` outlines it (the selected install).
pub fn card<R>(ui: &mut Ui, accent: bool, add: impl FnOnce(&mut Ui) -> R) -> R {
    Frame::new()
        .fill(CARD)
        .stroke(Stroke::new(if accent { 1.5 } else { 1.0 }, if accent { ACCENT } else { STROKE }))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// A tinted note: warning, error or info, with a status icon.
pub fn callout(ui: &mut Ui, status: Status, title: &str, body: &str) {
    let color = status_color(status);
    Frame::new()
        .fill(tint(color, 22))
        .stroke(Stroke::new(1.0, tint(color, 110)))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_top(|ui| {
                status_icon(ui, status, 18.0);
                ui.vertical(|ui| {
                    ui.label(RichText::new(title).strong().color(STRONG));
                    if !body.is_empty() {
                        ui.label(RichText::new(body).color(TEXT));
                    }
                });
            });
        });
}

pub fn primary(ui: &mut Ui, text: &str, enabled: bool) -> Response {
    let b = Button::new(RichText::new(text).strong().color(if enabled { ACCENT_TEXT } else { WEAK }))
        .fill(if enabled { ACCENT } else { Color32::from_rgb(0x2a, 0x2f, 0x38) })
        .min_size(vec2(0.0, 34.0));
    ui.add_enabled(enabled, b)
}

pub fn secondary(ui: &mut Ui, text: &str, enabled: bool) -> Response {
    ui.add_enabled(enabled, Button::new(RichText::new(text).color(TEXT)).min_size(vec2(0.0, 34.0)))
}

pub fn badge(ui: &mut Ui, text: &str, color: Color32) {
    Frame::new()
        .fill(tint(color, 34))
        .corner_radius(CornerRadius::same(9))
        .inner_margin(Margin::symmetric(8, 2))
        .show(ui, |ui| ui.label(RichText::new(text).small().strong().color(color)));
}

/// A command in a monospace box with a Copy button.
pub fn command(ui: &mut Ui, cmd: &str) {
    ui.horizontal(|ui| {
        Frame::new().fill(DEEP).corner_radius(CornerRadius::same(6)).inner_margin(Margin::symmetric(10, 6)).show(
            ui,
            |ui| {
                ui.add(egui::Label::new(RichText::new(cmd).monospace().color(STRONG)).wrap());
            },
        );
        if ui.small_button("Copy").on_hover_text("Copy to the clipboard").clicked() {
            ui.ctx().copy_text(cmd.to_string());
        }
    });
}

/// Two-column facts: label on the left, value on the right.
/// Values wrap; the label column is as wide as the widest label.
pub fn facts(ui: &mut Ui, rows: &[(&str, String)]) {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let key_width = rows
        .iter()
        .map(|(k, _)| ui.fonts_mut(|f| f.layout_no_wrap(k.to_string(), font.clone(), WEAK).size().x))
        .fold(0.0, f32::max)
        + 18.0;
    ui.spacing_mut().item_spacing.y = 5.0;
    for (k, v) in rows {
        ui.horizontal_top(|ui| {
            fixed_label(ui, key_width, RichText::new(*k).color(WEAK));
            ui.add(egui::Label::new(RichText::new(v).color(TEXT)).wrap());
        });
    }
}

/// The sub-steps of a script run, one row each.
pub fn substeps(ui: &mut Ui, steps: &[SubStep]) {
    for s in steps {
        ui.horizontal(|ui| {
            status_icon(ui, s.status, 16.0);
            let color = match s.status {
                Status::Pending | Status::Skipped => WEAK,
                _ => TEXT,
            };
            ui.label(RichText::new(&s.label).color(color));
            if s.status == Status::Skipped {
                ui.label(RichText::new("skipped").small().color(FAINT));
            }
        });
        if let Some(note) = s.note.as_ref().filter(|_| matches!(s.status, Status::Warning | Status::Failed)) {
            ui.horizontal(|ui| {
                ui.add_space(24.0);
                ui.add(egui::Label::new(RichText::new(note).small().color(status_color(s.status))).wrap());
            });
        }
    }
}

/// The scrolling, colored log of a script run.
pub fn log_view(ui: &mut Ui, id: &str, lines: &[Line], height: f32) {
    Frame::new().fill(DEEP).corner_radius(CornerRadius::same(8)).inner_margin(Margin::same(10)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ScrollArea::vertical()
            .id_salt(id)
            .max_height(height)
            .min_scrolled_height(height)
            .stick_to_bottom(true)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                // Not justified, even inside ui.columns (which justifies).
                ui.with_layout(Layout::top_down(Align::Min), |ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    for l in lines {
                        let (text, color) = match l {
                            Line::Header(h) => (format!("── {h} ──"), STRONG),
                            Line::Ok(m) => (format!("[ OK ]  {m}"), ACCENT),
                            Line::Info(m) => (format!("[INFO]  {m}"), INFO),
                            Line::Warn(m) => (format!("[WARN]  {m}"), WARN),
                            Line::Fail(m) => (format!("[FAIL]  {m}"), FAIL),
                            Line::Command(c) => (format!("        {c}"), STRONG),
                            Line::Summary { key, status, detail } => {
                                let tag = match status {
                                    Status::Done => "[OK]  ",
                                    Status::Failed => "[FAIL]",
                                    _ => "[SKIP]",
                                };
                                let d = detail.as_ref().map(|d| format!(" — {d}")).unwrap_or_default();
                                (format!("  {tag}  {key}{d}"), status_color(*status))
                            }
                            Line::Plain(t) => (t.clone(), WEAK),
                        };
                        ui.add(egui::Label::new(RichText::new(text).monospace().color(color)).wrap());
                    }
                });
            });
    });
}

/// Paint a thin horizontal rule.
pub fn rule(ui: &mut Ui) {
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(w, 1.0), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, STROKE);
}

/// A full-width row that responds to clicks, for the sidebar.
pub fn sidebar_row(ui: &mut Ui, selected: bool, enabled: bool, add: impl FnOnce(&mut Ui)) -> Response {
    let height = 38.0;
    let (rect, resp) = ui
        .allocate_exact_size(vec2(ui.available_width(), height), if enabled { Sense::click() } else { Sense::hover() });
    if selected {
        ui.painter().rect_filled(rect, CornerRadius::same(8), Color32::from_rgb(0x1f, 0x23, 0x2a));
        let bar = Rect::from_min_size(rect.min + vec2(0.0, 9.0), vec2(3.0, height - 18.0));
        ui.painter().rect_filled(bar, CornerRadius::same(2), ACCENT);
    } else if enabled && resp.hovered() {
        ui.painter().rect_filled(rect, CornerRadius::same(8), Color32::from_rgb(0x18, 0x1b, 0x20));
    }
    let inner = rect.shrink2(vec2(12.0, 0.0));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner).layout(Layout::left_to_right(Align::Center)));
    add(&mut child);
    if enabled { resp.on_hover_cursor(egui::CursorIcon::PointingHand) } else { resp }
}

/// A left-aligned label in a fixed-width column.
pub fn fixed_label(ui: &mut Ui, width: f32, text: RichText) {
    ui.allocate_ui_with_layout(vec2(width, 0.0), Layout::left_to_right(Align::Center), |ui| {
        ui.set_min_width(width);
        ui.label(text);
    });
}

/// A row of small tiles: label above, value below.
pub fn stats(ui: &mut Ui, items: &[(&str, String)]) {
    ui.horizontal_wrapped(|ui| {
        for (k, v) in items {
            Frame::new().fill(DEEP).corner_radius(CornerRadius::same(8)).inner_margin(Margin::symmetric(12, 7)).show(
                ui,
                |ui| {
                    ui.set_min_width(118.0);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 1.0;
                        ui.label(RichText::new(*k).small().color(WEAK));
                        ui.label(RichText::new(v).strong().color(STRONG));
                    });
                },
            );
        }
    });
}
