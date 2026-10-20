//! Console's separate windows (`rack::layers`) as real OS windows: every popup menu level, the
//! Settings window, panels, dialogs and plug-in windows get a borderless window of their own at
//! `mixer window origin + layer rect`, drawn by the mixer's GPU and renderer from their share
//! of the frame (`rack::layers::split`).
//!
//! Placement needs a platform that lets clients position windows: X11 (also XWayland with
//! `--x11`), macOS and Windows. Native Wayland cannot: there only free windows (Settings,
//! panels, plug-in windows: [`toplevel`]) get windows of their own, put where the compositor
//! likes; their content keeps the rect the layout gave it, so pointer input maps back the same
//! way. Menus, callouts and dialogs stay in the window they were opened from ([`Popups::assign`]).
//!
//! X11: menus and callouts are override-redirect (placed exactly, no window manager); free
//! windows and dialogs are managed dialog windows, moved by their title bar through the window
//! manager.

use std::collections::HashMap;
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
    /// Times the size was asked for again because the compositor gave another one (capped:
    /// a compositor that never agrees must not get a resize loop).
    pub repins: u8,
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
    /// Windows can be placed (not native Wayland): every layer gets one, at its rect.
    pub placed: bool,
    /// Unplaced: the free window each other layer opened from, by layer id (None: the mixer).
    hosts: HashMap<String, Option<String>>,
}

/// A layer that is a window of its own even where windows cannot be placed: Console's free
/// windows and plug-in windows (the user moves, stacks and closes them).
pub fn toplevel(l: &Layer) -> bool {
    l.kind == LayerKind::Window || l.id.starts_with("plugin:")
}

/// The plug-in window layer whose panel takes the input of window `id`: its own, or the one
/// whose menu it is (`plugin-menu:…`). None for every other window.
pub fn plugin_layer(id: &str) -> Option<String> {
    let key = id.strip_prefix("plugin:").or_else(|| id.strip_prefix("plugin-menu:"))?;
    Some(format!("plugin:{key}"))
}

/// Window › Next / Previous Window: the window after (before) `current` among the mixer (None)
/// and the free windows `ids` (layer order), wrapping round.
pub fn cycle(ids: &[String], current: Option<&str>, forward: bool) -> Option<String> {
    let n = ids.len() + 1;
    let at = current.and_then(|c| ids.iter().position(|i| i == c)).map_or(0, |i| i + 1);
    let next = if forward { (at + 1) % n } else { (at + n - 1) % n };
    (next > 0).then(|| ids[next - 1].clone())
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

/// Where a layer's window goes (screen, physical px) and its inner size (physical px).
pub fn layer_px(inner: PhysicalPosition<i32>, tf: Transform, r: Rect) -> (PhysicalPosition<i32>, (u32, u32)) {
    let size = (((r.w * tf.scale).round() as u32).max(1), ((r.h * tf.scale).round() as u32).max(1));
    (to_screen(inner, tf, (r.x, r.y)), size)
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
    /// The frame part (window) each layer is drawn in, for `rack::layers::split_into`: placed,
    /// its own (`k + 1`); unplaced, toplevel layers their own and the rest the toplevel window
    /// they opened from (`from`, the one the pointer was last in; None: the mixer, part 0).
    pub fn assign(&mut self, layers: &[Layer], from: Option<&str>) -> Vec<u16> {
        let own = |k: usize| k as u16 + 1;
        if self.placed {
            return (0..layers.len()).map(own).collect();
        }
        self.hosts.retain(|id, _| layers.iter().any(|l| &l.id == id));
        let hosts = &mut self.hosts;
        layers
            .iter()
            .enumerate()
            .map(|(k, l)| {
                if toplevel(l) {
                    return own(k);
                }
                let host = hosts.entry(l.id.clone()).or_insert_with(|| from.map(str::to_owned));
                host.as_deref()
                    .and_then(|h| layers.iter().position(|t| t.id == h && toplevel(t)))
                    .map_or(0, own)
            })
            .collect()
    }

    /// Opens, moves, resizes and closes windows so there is exactly one per layer that has a
    /// part of its own (`parts_of`, from [`Popups::assign`]), at its rect where placed.
    #[allow(clippy::too_many_arguments)]
    pub fn sync(
        &mut self,
        event_loop: &ActiveEventLoop,
        gpu: &gfx::SurfaceGpu,
        inner: PhysicalPosition<i32>,
        tf: Transform,
        layers: &[Layer],
        parts_of: &[u16],
        title: &dyn Fn(&Layer) -> String,
    ) {
        let windowed: Vec<&Layer> =
            layers.iter().enumerate().filter(|&(k, _)| parts_of.get(k) == Some(&(k as u16 + 1))).map(|(_, l)| l).collect();
        // Close windows whose layer is gone (or changed kind).
        let debug = std::env::var_os("UACONSOLE_DEBUG_WINDOWS").is_some();
        self.wins.retain(|w| {
            let keep = windowed.iter().any(|l| l.id == w.id && l.kind == w.kind);
            if !keep && debug {
                eprintln!("window {} closed", w.id);
            }
            keep
        });
        for l in windowed {
            let (at, size) = layer_px(inner, tf, l.rect);
            if let Some(w) = self.wins.iter_mut().find(|w| w.id == l.id) {
                w.rect = l.rect;
                w.title_h = l.title_h;
                let (moved, resized) = follow(&*w.window, (&mut w.at, &mut w.size), (at, size), self.placed);
                if moved && debug {
                    eprintln!("window {} moved to {at:?} (mixer content at {inner:?})", w.id);
                }
                if resized {
                    w.repins = 0;
                    w.surface.resize(gpu, size.0, size.1);
                    if debug {
                        eprintln!("window {} resized to {}x{} px", w.id, size.0, size.1);
                    }
                }
                continue;
            }
            match open(event_loop, gpu, l, &title(l), self.placed.then_some(at), size) {
                Ok(w) => {
                    // `UACONSOLE_DEBUG_WINDOWS=1`: where each window was asked for and landed.
                    if debug {
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

/// The winit calls [`follow`] makes (a trait so tests can watch them).
pub trait Shell {
    fn place(&self, at: PhysicalPosition<i32>);
    /// Resizes to `size` and keeps it there: min = max = size. Console's separate windows
    /// have a fixed size (a plug-in window grows and shrinks by its toolbar's Resize Plug-In
    /// scale, never by dragging an edge). `with_resizable(false)` pins winit's Wayland min/max
    /// to the size at creation, and a compositor keeps a window inside those, so they move too.
    fn fix_size(&self, size: (u32, u32));
}

impl Shell for Window {
    fn place(&self, at: PhysicalPosition<i32>) {
        self.set_outer_position(at);
    }

    fn fix_size(&self, (w, h): (u32, u32)) {
        let s = PhysicalSize::new(w, h);
        self.set_min_inner_size(Some(s));
        self.set_max_inner_size(Some(s));
        let _ = self.request_inner_size(s);
    }
}

/// Makes a window that is at `cur` (screen position, inner size; updated) show a layer that
/// wants `want`: moved there where windows are placed, resized to its size everywhere.
/// Returns (moved, resized).
pub fn follow(
    shell: &dyn Shell,
    cur: (&mut PhysicalPosition<i32>, &mut (u32, u32)),
    want: (PhysicalPosition<i32>, (u32, u32)),
    placed: bool,
) -> (bool, bool) {
    let moved = placed && *cur.0 != want.0;
    if moved {
        shell.place(want.0);
        *cur.0 = want.0;
    }
    let resized = *cur.1 != want.1;
    if resized {
        shell.fix_size(want.1);
        *cur.1 = want.1;
    }
    (moved, resized)
}

/// A window for layer `l` at `at` (None: where the compositor puts it, native Wayland).
fn open(
    event_loop: &ActiveEventLoop,
    gpu: &gfx::SurfaceGpu,
    l: &Layer,
    title: &str,
    at: Option<PhysicalPosition<i32>>,
    size: (u32, u32),
) -> Result<PopupWin, String> {
    // Not resizable: a fixed-size toplevel, which tiling compositors (Hyprland) float.
    #[allow(unused_mut)]
    let mut attrs = Window::default_attributes()
        .with_title(title)
        .with_decorations(false)
        .with_resizable(false)
        .with_inner_size(PhysicalSize::new(size.0, size.1));
    if let Some(at) = at {
        attrs = attrs.with_position(at);
    }
    if l.kind == LayerKind::Menu {
        attrs = attrs.with_window_level(WindowLevel::AlwaysOnTop).with_active(false);
    }
    #[cfg(all(unix, not(any(target_os = "macos", target_os = "ios", target_os = "android"))))]
    {
        use winit::platform::wayland::WindowAttributesExtWayland;
        use winit::platform::x11::{WindowAttributesExtX11, WindowType};
        attrs = WindowAttributesExtWayland::with_name(attrs, "uaconsole", "uaconsole");
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
        at: at.unwrap_or_default(),
        size,
        repins: 0,
        cursor: (0.0, 0.0),
        focused: false,
        title_h: l.title_h,
    })
}

/// The window title of a layer the app has no better one for.
pub fn title(l: &Layer) -> String {
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
    fn next_and_previous_window_cycle_through_the_mixer() {
        let ids = ["settings".to_string(), "plugin:in0:1".to_string()];
        assert_eq!(cycle(&ids, None, true).as_deref(), Some("settings"));
        assert_eq!(cycle(&ids, Some("settings"), true).as_deref(), Some("plugin:in0:1"));
        assert_eq!(cycle(&ids, Some("plugin:in0:1"), true), None, "back to the mixer");
        assert_eq!(cycle(&ids, None, false).as_deref(), Some("plugin:in0:1"));
        assert_eq!(cycle(&[], None, true), None);
    }

    #[test]
    fn only_plug_in_windows_and_their_menus_feed_the_overlays() {
        assert_eq!(plugin_layer("plugin:in0:1").as_deref(), Some("plugin:in0:1"));
        assert_eq!(plugin_layer("plugin-menu:in0:1").as_deref(), Some("plugin:in0:1"));
        for other in ["settings", "panel:about", "menu:0", "channels", "popover"] {
            assert_eq!(plugin_layer(other), None, "{other}");
        }
    }

    #[test]
    fn screen_round_trip() {
        let tf = Transform { scale: 2.0, offset: (10.0, 0.0) };
        let inner = PhysicalPosition::new(1706, 660);
        let s = to_screen(inner, tf, (488.0, 1065.0));
        assert_eq!(s, PhysicalPosition::new(1706 + 10 + 976, 660 + 2130));
        assert_eq!(to_mixer(inner, tf, s), (488.0, 1065.0));
    }

    /// Records what [`follow`] asks of a window.
    #[derive(Default)]
    struct Watch(std::cell::RefCell<Vec<String>>);

    impl Shell for Watch {
        fn place(&self, at: PhysicalPosition<i32>) {
            self.0.borrow_mut().push(format!("place {},{}", at.x, at.y));
        }
        fn fix_size(&self, (w, h): (u32, u32)) {
            self.0.borrow_mut().push(format!("size {w}x{h}"));
        }
    }

    #[test]
    fn a_layer_that_grows_or_shrinks_resizes_its_window() {
        let tf = Transform { scale: 2.0, offset: (0.0, 0.0) };
        let inner = PhysicalPosition::new(100, 50);
        // A plug-in window at 100 %, then Resize Plug-In 150 %, then 75 % (unplaced: Wayland).
        let (mut at, mut size) = layer_px(inner, tf, Rect::new(10.0, 20.0, 400.0, 300.0));
        let watch = Watch::default();
        for (rect, want) in [
            (Rect::new(10.0, 20.0, 600.0, 450.0), "size 1200x900"),
            (Rect::new(10.0, 20.0, 300.0, 225.0), "size 600x450"),
        ] {
            let (moved, resized) = follow(&watch, (&mut at, &mut size), layer_px(inner, tf, rect), false);
            assert!(!moved && resized);
            assert_eq!(watch.0.borrow_mut().drain(..).collect::<Vec<_>>(), [want]);
        }
        // Unchanged: nothing asked. Placed (X11, macOS): moved too.
        let same = (at, size);
        assert_eq!(follow(&watch, (&mut at, &mut size), same, false), (false, false));
        let want = layer_px(inner, tf, Rect::new(0.0, 0.0, 300.0, 225.0));
        assert_eq!(follow(&watch, (&mut at, &mut size), want, true), (true, false));
        assert_eq!(watch.0.borrow().as_slice(), ["place 100,50"]);
    }

    #[test]
    fn unplaced_layers_stay_where_they_opened() {
        let layer = |id: &str, kind| Layer { id: id.into(), kind, rect: Rect::new(0.0, 0.0, 10.0, 10.0), title_h: 0.0 };
        let mut p = Popups::default();
        let settings = layer("settings", LayerKind::Window);
        let plugin = layer("plugin:in0:1", LayerKind::Popover);
        let menu = layer("menu:0", LayerKind::Menu);
        // A menu opened from the plug-in window is drawn in it, and stays there.
        assert_eq!(p.assign(&[settings.clone(), plugin.clone(), menu.clone()], Some("plugin:in0:1")), [1, 2, 2]);
        assert_eq!(p.assign(&[settings.clone(), plugin.clone(), menu.clone()], Some("settings")), [1, 2, 2]);
        // Its window gone: the mixer's.
        assert_eq!(p.assign(&[settings.clone(), menu.clone()], None), [1, 0]);
        // Closed, then opened from Settings: drawn in Settings.
        p.assign(&[settings.clone()], None);
        assert_eq!(p.assign(&[settings.clone(), menu.clone()], Some("settings")), [1, 1]);
        p.placed = true;
        assert_eq!(p.assign(&[settings, plugin, menu], None), [1, 2, 3]);
    }
}
