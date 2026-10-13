//! open-apollo's dark look for the wizard. Our own colors and drawing only:
//! no Universal Audio marks, art or screenshots.

use eframe::egui::{
    self, Color32, CornerRadius, FontFamily, FontId, Margin, Stroke, TextStyle, Theme, ThemePreference, Vec2, Visuals,
};

pub const BG: Color32 = Color32::from_rgb(0x15, 0x17, 0x1b);
pub const SIDEBAR: Color32 = Color32::from_rgb(0x0f, 0x11, 0x14);
pub const CARD: Color32 = Color32::from_rgb(0x1c, 0x1f, 0x25);
pub const STROKE: Color32 = Color32::from_rgb(0x2b, 0x30, 0x39);
pub const DEEP: Color32 = Color32::from_rgb(0x0d, 0x0f, 0x12);
pub const TEXT: Color32 = Color32::from_rgb(0xdc, 0xe0, 0xe6);
pub const STRONG: Color32 = Color32::from_rgb(0xf3, 0xf5, 0xf7);
pub const WEAK: Color32 = Color32::from_rgb(0x8b, 0x93, 0x9e);
pub const FAINT: Color32 = Color32::from_rgb(0x5c, 0x63, 0x6e);
pub const ACCENT: Color32 = Color32::from_rgb(0x4c, 0xc3, 0x8a);
pub const ACCENT_TEXT: Color32 = Color32::from_rgb(0x0c, 0x1f, 0x16);
pub const WARN: Color32 = Color32::from_rgb(0xe3, 0xa8, 0x46);
pub const FAIL: Color32 = Color32::from_rgb(0xe8, 0x5d, 0x55);
pub const INFO: Color32 = Color32::from_rgb(0x62, 0xa9, 0xe8);

pub fn tint(c: Color32, alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), alpha)
}

pub fn apply(ctx: &egui::Context) {
    ctx.set_theme(ThemePreference::Dark);
    let mut v = Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = CARD;
    v.window_stroke = Stroke::new(1.0, STROKE);
    v.window_corner_radius = CornerRadius::same(12);
    v.extreme_bg_color = DEEP;
    v.faint_bg_color = CARD;
    v.code_bg_color = DEEP;
    v.hyperlink_color = ACCENT;
    v.warn_fg_color = WARN;
    v.error_fg_color = FAIL;
    v.selection.bg_fill = tint(ACCENT, 90);
    v.selection.stroke = Stroke::new(1.0, ACCENT);
    v.override_text_color = None;
    let w = &mut v.widgets;
    w.noninteractive.bg_stroke = Stroke::new(1.0, STROKE);
    w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    w.noninteractive.corner_radius = CornerRadius::same(8);
    for (state, fill) in [
        (&mut w.inactive, Color32::from_rgb(0x2a, 0x2f, 0x38)),
        (&mut w.hovered, Color32::from_rgb(0x34, 0x3a, 0x45)),
        (&mut w.active, Color32::from_rgb(0x3d, 0x44, 0x50)),
        (&mut w.open, Color32::from_rgb(0x2a, 0x2f, 0x38)),
    ] {
        state.bg_fill = fill;
        state.weak_bg_fill = fill;
        state.corner_radius = CornerRadius::same(7);
        state.fg_stroke = Stroke::new(1.5, STRONG);
    }
    w.inactive.bg_stroke = Stroke::NONE;
    w.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(0x4a, 0x52, 0x5e));
    ctx.set_visuals_of(Theme::Dark, v);
    ctx.style_mut_of(Theme::Dark, |s| {
        s.spacing.item_spacing = Vec2::new(8.0, 8.0);
        s.spacing.button_padding = Vec2::new(14.0, 7.0);
        s.spacing.window_margin = Margin::same(16);
        s.spacing.interact_size.y = 30.0;
        s.text_styles = [
            (TextStyle::Heading, FontId::new(24.0, FontFamily::Proportional)),
            (TextStyle::Body, FontId::new(14.5, FontFamily::Proportional)),
            (TextStyle::Button, FontId::new(14.5, FontFamily::Proportional)),
            (TextStyle::Small, FontId::new(12.0, FontFamily::Proportional)),
            (TextStyle::Monospace, FontId::new(12.5, FontFamily::Monospace)),
        ]
        .into();
    });
}
