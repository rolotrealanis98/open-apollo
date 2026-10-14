//! Console's separate windows (`rack::layers`) as real OS windows: every popup menu level, the
//! Settings window, panels, dialogs and plug-in windows get a borderless window of their own at
//! `mixer window origin + layer rect`, drawn by the mixer's GPU and renderer from their share
//! of the frame (`rack::layers::split`).
//!
//! Placement needs a platform that lets clients position windows: X11 (also XWayland with
//! `--x11`), macOS and Windows. Native Wayland cannot, so there the app keeps drawing them
//! inside the mixer window (`--windows inline`).
//!
//! X11: menus and callouts are override-redirect (placed exactly, no window manager); free
//! windows and dialogs are managed dialog windows, moved by their title bar through the window
//! manager.

use std::sync::Arc;

use houston::Rect;
use rack::{Layer, LayerKind};
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event_loop::ActiveEventLoop;
use winit::window::{Window, WindowId, WindowLevel};

use crate::view::Transform;

/// One separate window.
pub struct PopupWin {
    pub id: String,
    pub kind: LayerKind,
    pub window: Arc<Window>,
    pub surface: gfx::WindowSurface,
    /// The layer rect it shows (mixer points).
    pub rect: Rect,
    /// Where it was put (physical pixels, screen).
    pub at: PhysicalPosition<i32>,
    pub size: (u32, u32),
    /// Pointer position in the window (physical pixels).
    pub cursor: (f64, f64),
    pub focused: bool,
    /// The layer's title bar height (points): a press there moves the window.
    pub title_h: f32,
}

impl PopupWin {
    /// A pointer position in this window, in mixer points.
    pub fn to_mixer(&self, px: (f64, f64), scale: f32) -> (f32, f32) {
        (self.rect.x + px.0 as f32 / scale, self.rect.y + px.1 as f32 / scale)
    }
}

/// The open separate windows, matched to the frame's layers by id.
#[derive(Default)]
pub struct Popups {
    pub wins: Vec<PopupWin>,
}

/// Screen position (physical px) of mixer point `p` for a mixer window whose content starts
/// at `inner` (physical px) under transform `tf`.
pub fn to_screen(inner: PhysicalPosition<i32>, tf: Transform, p: (f32, f32)) -> PhysicalPosition<i32> {
    PhysicalPosition::new(
        inner.x + (tf.offset.0 + p.0 * tf.scale).round() as i32,
        inner.y + (tf.offset.1 + p.1 * tf.scale).round() as i32,
    )
}

/// Mixer point of screen position `s` (physical px); the inverse of [`to_screen`].
pub fn to_mixer(inner: PhysicalPosition<i32>, tf: Transform, s: PhysicalPosition<i32>) -> (f32, f32) {
    (
        ((s.x - inner.x) as f32 - tf.offset.0) / tf.scale,
        ((s.y - inner.y) as f32 - tf.offset.1) / tf.scale,
    )
}

impl Popups {
    pub fn index_of(&self, id: WindowId) -> Option<usize> {
        self.wins.iter().position(|w| w.window.id() == id)
    }

    pub fn any_focused(&self) -> bool {
        self.wins.iter().any(|w| w.focused)
    }

    /// Reopening an existing panel must raise its OS window, not just its drawn layer.
    pub fn focus(&self, layer_id: &str) {
        if let Some(w) = self.wins.iter().find(|w| w.id == layer_id) {
            w.window.set_minimized(false);
            w.window.focus_window();
        }
    }

    /// Opens, moves, resizes and closes windows so there is exactly one per layer, at its rect.
    pub fn sync(
        &mut self,
        event_loop: &ActiveEventLoop,
        gpu: &gfx::SurfaceGpu,
        inner: PhysicalPosition<i32>,
        tf: Transform,
        layers: &[Layer],
    ) {
        // Close windows whose layer is gone (or changed kind).
        let debug = std::env::var_os("UACONSOLE_DEBUG_WINDOWS").is_some();
        self.wins.retain(|w| {
            let keep = layers.iter().any(|l| l.id == w.id && l.kind == w.kind);
            if !keep && debug {
                eprintln!("window {} closed", w.id);
            }
            keep
        });
        for l in layers {
            let at = to_screen(inner, tf, (l.rect.x, l.rect.y));
            let size = (
                ((l.rect.w * tf.scale).round() as u32).max(1),
                ((l.rect.h * tf.scale).round() as u32).max(1),
            );
            if let Some(w) = self.wins.iter_mut().find(|w| w.id == l.id) {
                w.rect = l.rect;
                w.title_h = l.title_h;
                if w.at != at {
                    w.window.set_outer_position(at);
                    w.at = at;
                    if std::env::var_os("UACONSOLE_DEBUG_WINDOWS").is_some() {
                        eprintln!("window {} moved to {at:?} (mixer content at {inner:?})", w.id);
                    }
                }
                if w.size != size {
                    let _ = w.window.request_inner_size(PhysicalSize::new(size.0, size.1));
                    w.surface.resize(gpu, size.0, size.1);
                    w.size = size;
                }
                continue;
            }
            match open(event_loop, gpu, l, at, size) {
                Ok(w) => {
                    // `UACONSOLE_DEBUG_WINDOWS=1`: where each window was asked for and landed.
                    if std::env::var_os("UACONSOLE_DEBUG_WINDOWS").is_some() {
                        let got = w.window.outer_position().ok();
                        eprintln!(
                            "window {} {:?}: rect {:?} -> at {:?} {}x{} px (outer {:?})",
                            w.id, w.kind, l.rect, at, size.0, size.1, got
                        );
                    }
                    self.wins.push(w)
                }
                Err(e) => eprintln!("uaconsole: {} window: {e}", l.id),
            }
        }
        // Stack in layer order (later layers above): new windows are created on top.
    }
}

fn open(
    event_loop: &ActiveEventLoop,
    gpu: &gfx::SurfaceGpu,
    l: &Layer,
    at: PhysicalPosition<i32>,
    size: (u32, u32),
) -> Result<PopupWin, String> {
    #[allow(unused_mut)]
    let mut attrs = Window::default_attributes()
        .with_title(title(l))
        .with_decorations(false)
        .with_resizable(false)
        .with_position(at)
        .with_inner_size(PhysicalSize::new(size.0, size.1));
    if l.kind == LayerKind::Menu {
        attrs = attrs.with_window_level(WindowLevel::AlwaysOnTop).with_active(false);
    }
    #[cfg(all(unix, not(any(target_os = "macos", target_os = "ios", target_os = "android"))))]
    {
        use winit::platform::x11::{WindowAttributesExtX11, WindowType};
        attrs = WindowAttributesExtX11::with_name(attrs, "uaconsole", "uaconsole");
        attrs = match l.kind {
            // Placed exactly, above everything, never managed (like X11 menus).
            LayerKind::Menu => attrs.with_override_redirect(true).with_x11_window_type(vec![WindowType::PopupMenu]),
            LayerKind::Popover => attrs.with_override_redirect(true).with_x11_window_type(vec![WindowType::Utility]),
            LayerKind::Window | LayerKind::Dialog => attrs.with_x11_window_type(vec![WindowType::Dialog]),
        };
    }
    let window = Arc::new(event_loop.create_window(attrs).map_err(|e| e.to_string())?);
    let inner = window.inner_size();
    let surface = gfx::WindowSurface::new(gpu, window.clone(), (inner.width, inner.height))?;
    Ok(PopupWin {
        id: l.id.clone(),
        kind: l.kind,
        window,
        surface,
        rect: l.rect,
        at,
        size,
        cursor: (0.0, 0.0),
        focused: false,
        title_h: l.title_h,
    })
}

fn title(l: &Layer) -> String {
    match l.id.as_str() {
        "settings" => "Settings".into(),
        id if id.starts_with("menu") => "menu".into(),
        id => id.trim_start_matches("panel:").replace('-', " "),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screen_round_trip() {
        let tf = Transform { scale: 2.0, offset: (10.0, 0.0) };
        let inner = PhysicalPosition::new(1706, 660);
        let s = to_screen(inner, tf, (488.0, 1065.0));
        assert_eq!(s, PhysicalPosition::new(1706 + 10 + 976, 660 + 2130));
        assert_eq!(to_mixer(inner, tf, s), (488.0, 1065.0));
    }
}
