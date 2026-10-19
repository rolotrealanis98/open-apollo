//! The window: winit event loop, wgpu surface, input routing and the redraw policy.
//!
//! Redraws happen on input and model changes only; while live meters (or a live panel) are on,
//! a `WaitUntil` timer at the display rate (60…120 Hz, [`meter_period`]) drives them. Otherwise
//! the loop sleeps in `ControlFlow::Wait`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use houston::Rgba;
use rack::{Action, Assets, Control, ControlId, Effect, Interaction, MixerModel, ViewState};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Fullscreen, Window, WindowId};

use crate::cli::WindowsMode;
use crate::frame::{Frame, FrameTimes, Hud, build_frame, layout_for};
use crate::popups::Popups;
use crate::input::{ClickTracker, Command, PIXELS_PER_LINE, shortcut, wheel_actions};
use crate::overlay::{OverlayStack, PointerEvent, Routed};
use crate::settings::{MixerPrefs, Settings};
use crate::source::{ModelSource, OpenedModel};
use crate::view::{Transform, View, ViewMode, zoom_in, zoom_out};

/// The hit regions of the window pointer input came from (`App::active_part`).
macro_rules! active_hits {
    ($s:ident) => {
        match $s.active_part() {
            Some(k) => &$s.parts[k].hits[..],
            None => &$s.frame.hits[..],
        }
    };
}

/// The wheel-scrollable regions of that window.
macro_rules! active_scrolls {
    ($s:ident) => {
        match $s.active_part() {
            Some(k) => &$s.parts[k].scrolls[..],
            None => &$s.frame.scrolls[..],
        }
    };
}

/// Live meters redraw at the display's rate, 60…120 Hz: Console steps its meters every
/// 16 666 µs (`Rack_Meter`, Rack.dll 0x1809c9780), and the ballistics are time-based
/// (`rack::ballistics`), so faster displays only get smoother.
pub const METER_PERIOD: Duration = Duration::from_nanos(16_666_667);

/// The live-meter redraw period for a display refreshing at `millihertz` (0: unknown).
pub fn meter_period(millihertz: Option<u32>) -> Duration {
    match millihertz {
        Some(mhz) if mhz > 0 => Duration::from_secs_f64(1000.0 / mhz.clamp(60_000, 120_000) as f64),
        _ => METER_PERIOD,
    }
}
pub const LETTERBOX: Rgba = Rgba::rgb8(18, 19, 21);
/// Pan distance per wheel line, in pixels.
const PAN_PER_LINE: f32 = 60.0;
/// Scroll distance per wheel line in the browser list and cells column, in points.
pub(crate) const SCROLL_PER_LINE: f32 = 40.0;

pub struct Options {
    pub source: ModelSource,
    pub view: View,
    pub compact: bool,
    /// Force the meter timer even when the model is not live (testing).
    pub live_meters: bool,
    /// Start with the stats overlay.
    pub stats: bool,
    pub open_plugin: Option<(String, u8)>,
    /// Settings window / panel to open at start (`--open`).
    pub open: Option<String>,
    /// `--view` spec applied to the mixer view at start.
    pub view_spec: Option<String>,
    /// `--menu` spec opened once the window is up (testing).
    pub menu: Option<String>,
    /// Console's menus, Settings and panels as separate OS windows, or inside the mixer.
    pub windows: WindowsMode,
    /// Linux: X11 (XWayland) instead of Wayland, so separate windows can be placed.
    pub x11: bool,
    pub settings: Settings,
}

struct Gpu {
    window: Arc<Window>,
    surface: gfx::SurfaceGpu,
    renderer: gfx::Renderer,
}

pub struct App {
    assets: Assets,
    source: ModelSource,
    model: Box<dyn MixerModel>,
    label: String,
    live_meters: bool,
    settings: Settings,
    view: View,
    /// The mixer's UI-only state (navigator toggles, browser, popovers, pages, compact).
    mixer: ViewState,
    gpu: Option<Gpu>,
    frame: Frame,
    dirty: bool,
    interaction: Interaction,
    clicks: ClickTracker,
    cursor: (f64, f64),
    /// Middle-button drag pans.
    panning: bool,
    mods: ModifiersState,
    overlays: OverlayStack,
    show_stats: bool,
    times: FrameTimes,
    next_tick: Option<Instant>,
    /// [`meter_period`] of the window's display.
    meter_period: Duration,
    occluded: bool,
    scale_factor: f64,
    title: String,
    pub error: Option<String>,
    /// Requested placement of Console's separate windows.
    windows: WindowsMode,
    /// Separate windows are on (requested and the platform can place windows).
    separate: bool,
    popups: Popups,
    /// The frame split per window (part 0 the mixer, part k+1 layer k) while `separate`.
    parts: Vec<rack::LayerPart>,
    /// The part each layer is drawn in (`Popups::assign`): its own, or the window it opened from.
    parts_of: Vec<u16>,
    /// The window the current pointer event came from (index into `popups.wins`; None: mixer).
    active: Option<usize>,
    /// Windows the user moved, by layer id (screen position, physical px).
    moved: HashMap<String, PhysicalPosition<i32>>,
    main_focused: bool,
    focus_check: bool,
    pending_menu: Option<String>,
    /// `--open-plugin` against a live engine: retried on rebuilds until the deadline, since
    /// the engine's state arrives after connect.
    pending_open: Option<(String, u8, Instant)>,
    /// A press on a drag source (`rack::dnd`): its tap fires on release unless it became a drag.
    deferred_tap: Option<rack::Hit>,
    /// The plug-in windows Window › Show/Hide Floating Windows hid (Some while hidden).
    hidden_floating: Option<Vec<Box<dyn crate::overlay::Panel>>>,
    /// When the changed view state is written (SAVE_DELAY after the first change; quit writes at once).
    save_at: Option<Instant>,
}

/// How long after a view-state change it is written.
const SAVE_DELAY: Duration = Duration::from_secs(2);

/// The mixer's starting view: the saved view state, then `--view` over it.
fn initial_mixer(compact: bool, settings: &Settings, view_spec: Option<&str>) -> ViewState {
    let mut mixer = ViewState {
        compact,
        // No native menu bar here: Windows Console's in-window one, in the title bar.
        menu_bar: true,
        ..ViewState::default()
    };
    settings.mixer.apply(&mut mixer);
    if let Some(spec) = view_spec {
        if let Err(e) = mixer.apply_spec(spec) {
            eprintln!("uaconsole: --view: {e}");
        }
    }
    mixer
}

/// Runs the window until it is closed. Returns an error message if it could not start.
pub fn run(assets: Assets, opts: Options) -> Result<(), String> {
    #[allow(unused_mut)]
    let mut builder = EventLoop::builder();
    #[cfg(all(unix, not(any(target_os = "macos", target_os = "ios", target_os = "android"))))]
    if opts.x11 {
        use winit::platform::x11::EventLoopBuilderExtX11;
        builder.with_x11();
    }
    let event_loop = builder.build().map_err(|e| format!("event loop: {e}"))?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = App::new(assets, opts);
    event_loop
        .run_app(&mut app)
        .map_err(|e| format!("event loop: {e}"))?;
    match app.error.take() {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

impl App {
    pub fn new(assets: Assets, opts: Options) -> Self {
        let OpenedModel {
            mut model,
            live_meters,
            label,
        } = opts.source.open().unwrap_or_else(|e| {
            eprintln!("uaconsole: {e}; showing preview data");
            crate::source::preview("preview")
        });
        let mut overlays = OverlayStack::new(Box::new(crate::plugin::PluginUiHook::new(&assets.images)));
        let mut pending_open = None;
        if let Some((strip, slot)) = &opts.open_plugin {
            if !overlays.on_open_plugin(strip, *slot, &*model) {
                if live_meters {
                    // ponytail: fixed 10 s window; use an engine "initial state complete" signal if one is added.
                    pending_open = Some((strip.clone(), *slot, Instant::now() + Duration::from_secs(10)));
                } else {
                    eprintln!("uaconsole: --open-plugin {strip}:{slot}: no plug-in loaded there");
                }
            }
        }
        overlays.sync(&mut *model);
        let area = houston::Rect::new(0.0, 0.0, 1.0, 1.0);
        let mut mixer = initial_mixer(opts.compact, &opts.settings, opts.view_spec.as_deref());
        if let Some(name) = &opts.open {
            mixer.open_named(name);
        }
        let frame = build_frame(&assets, &*model, &mixer, &mut overlays, area, None);
        Self {
            assets,
            source: opts.source,
            model,
            label,
            live_meters: live_meters || opts.live_meters,
            settings: opts.settings,
            view: opts.view,
            mixer,
            gpu: None,
            frame,
            dirty: true,
            interaction: Interaction::default(),
            clicks: ClickTracker::default(),
            deferred_tap: None,
            hidden_floating: None,
            cursor: (0.0, 0.0),
            panning: false,
            mods: ModifiersState::empty(),
            overlays,
            show_stats: opts.stats,
            times: FrameTimes::default(),
            next_tick: None,
            meter_period: METER_PERIOD,
            occluded: false,
            scale_factor: 1.0,
            title: String::new(),
            error: None,
            windows: opts.windows,
            separate: false,
            popups: Popups::default(),
            parts: Vec::new(),
            parts_of: Vec::new(),
            active: None,
            moved: HashMap::new(),
            main_focused: true,
            focus_check: false,
            pending_menu: opts.menu,
            pending_open,
            save_at: None,
        }
    }

    fn surface_size(&self) -> (u32, u32) {
        self.gpu
            .as_ref()
            .map(|g| g.surface.size())
            .unwrap_or((1, 1))
    }

    fn logical_size(&self) -> (f32, f32) {
        self.frame.layout.size()
    }

    fn transform(&self) -> Transform {
        self.view
            .transform(self.logical_size(), self.surface_size(), self.scale_factor)
    }

    fn cursor_pt(&self) -> (f32, f32) {
        match self.active.and_then(|i| self.popups.wins.get(i)) {
            Some(w) => w.to_mixer(w.cursor, self.transform().scale),
            None => self.transform().to_logical(self.cursor),
        }
    }

    /// The frame part (window) pointer input goes to: 0 the mixer, k + 1 layer k.
    fn active_part(&self) -> Option<usize> {
        if !self.separate || self.parts.is_empty() {
            return None;
        }
        Some(match self.active.and_then(|i| self.popups.wins.get(i)) {
            Some(w) => 1 + self.frame.layers.iter().position(|l| l.id == w.id)?,
            None => 0,
        })
    }

    /// Shift: fine adjust (knob / fader drags, Option+wheel).
    fn fine(&self) -> bool {
        self.mods.shift_key()
    }

    /// Console's control modifiers: Option (macOS) / Alt, and Command (macOS) / Ctrl.
    fn key_mods(&self) -> rack::KeyMods {
        let cmd = if cfg!(target_os = "macos") { self.mods.super_key() } else { self.mods.control_key() };
        rack::KeyMods { alt: self.mods.alt_key(), cmd }
    }

    fn request_redraw(&self) {
        if let Some(g) = &self.gpu {
            g.window.request_redraw();
        }
        // Separate windows are drawn with the mixer (`redraw`).
    }

    /// Console's separate windows: the screen and the moved windows' places, in mixer points,
    /// for a mixer window whose content starts at `inner` under `tf`.
    fn windowing(&self, g: &Gpu, tf: crate::view::Transform) -> Option<rack::Windowing> {
        let inner = g.window.inner_position().ok()?;
        let m = g.window.current_monitor()?;
        let (mp, ms) = (m.position(), m.size());
        let (x, y) = crate::popups::to_mixer(inner, tf, mp);
        let mut w = rack::Windowing::new(houston::Rect::new(x, y, ms.width as f32 / tf.scale, ms.height as f32 / tf.scale));
        for (id, p) in &self.moved {
            w.positions.insert(id.clone(), crate::popups::to_mixer(inner, tf, *p));
        }
        Some(w)
    }

    /// Something visible changed: rebuild before the next frame and ask for one.
    fn changed(&mut self) {
        self.dirty = true;
        self.request_redraw();
    }

    fn rebuild(&mut self) {
        let t = Instant::now();
        let surface = self.surface_size();
        // Console lays out to its window: the mixer follows the surface (in points; at 1:1
        // zoom divides them), down to the layout's minimum size.
        if self.gpu.is_some() {
            let z = if self.view.mode == ViewMode::OneToOne { self.view.zoom } else { 1.0 };
            let k = (self.scale_factor as f32 * z).max(0.01);
            self.mixer.window = Some((surface.0 as f32 / k, surface.1 as f32 / k));
        }
        let logical = layout_for(&*self.model, &self.mixer).size();
        let tf = self.view.transform(logical, surface, self.scale_factor);
        self.mixer.windowing = match (&self.gpu, self.separate) {
            (Some(g), true) if self.popups.placed => self.windowing(g, tf),
            // Native Wayland: nothing is placed on a screen, menus and callouts keep to the
            // mixer window; free windows go where the compositor puts them.
            (Some(_), true) => Some(rack::Windowing::new(houston::Rect::new(0.0, 0.0, logical.0, logical.1))),
            _ => None,
        };
        let area = tf.visible(logical, surface);
        let stats = self.show_stats.then(|| self.stats_text(tf));
        let hud = stats.as_deref().map(|text| Hud {
            text,
            px_per_pt: tf.scale,
        });
        if let Some((strip, slot, deadline)) = &self.pending_open {
            if self.overlays.on_open_plugin(strip, *slot, &*self.model) {
                self.pending_open = None;
            } else if Instant::now() > *deadline {
                eprintln!("uaconsole: --open-plugin {strip}:{slot}: no plug-in loaded there");
                self.pending_open = None;
            }
        }
        self.overlays.sync(&mut *self.model);
        self.frame = build_frame(
            &self.assets,
            &*self.model,
            &self.mixer,
            &mut self.overlays,
            area,
            hud,
        );
        self.parts = if self.mixer.windowing.is_some() {
            let from = self.active.and_then(|i| self.popups.wins.get(i)).map(|w| w.id.clone());
            self.parts_of = self.popups.assign(&self.frame.layers, from.as_deref());
            rack::layers::split_into(&self.frame.list, &self.frame.hits, &self.frame.scrolls, &self.parts_of, &self.frame.marks)
        } else {
            Vec::new()
        };
        self.times.built(t.elapsed());
        self.dirty = false;
        let title = format!(
            "UAD Console — {} — {}",
            self.label,
            self.model.session_name()
        );
        if title != self.title {
            if let Some(g) = &self.gpu {
                g.window.set_title(&title);
            }
            self.title = title;
        }
    }

    fn stats_text(&self, tf: Transform) -> String {
        let s = self
            .gpu
            .as_ref()
            .map(|g| g.renderer.stats)
            .unwrap_or_default();
        let (w, h) = self.surface_size();
        format!(
            "{} fps · build {:.2} ms (p99 {:.2}) · render {:.2} ms (p99 {:.2})\n{} quads · {} batches · {} textures ({:.1} MB) · {} glyphs\n{}×{} px · {} {:.0}% · {:.2} px/pt{}",
            self.times.fps(Instant::now()),
            self.times.build_ms,
            self.times.build_p99(),
            self.times.render_ms,
            self.times.render_p99(),
            s.quads,
            s.batches,
            s.textures,
            s.texture_bytes as f64 / 1e6,
            s.glyphs,
            w,
            h,
            self.view.mode.as_str(),
            self.view.zoom * 100.0,
            tf.scale,
            if self.live_meters {
                " · live meters"
            } else {
                ""
            },
        )
    }

    fn redraw(&mut self, event_loop: &ActiveEventLoop) {
        // Engine pushes land here once per frame; live meters rebuild every tick (≈0.2 ms).
        if self.model.pump() || self.live_meters {
            self.dirty = true;
        }
        // Console blinks an outline round CLOCK while the clock is not locked.
        if self.mixer.tick_blink(Instant::now()) && self.model.status().clock_locked == Some(false) {
            self.dirty = true;
        }
        if self.dirty || self.show_stats {
            self.rebuild();
        }
        if let Some(spec) = self.pending_menu.take() {
            match self.mixer.open_menu_spec(&self.frame.hits, &spec, &*self.model) {
                Ok(()) => self.rebuild(),
                Err(e) => eprintln!("uaconsole: --menu: {e}"),
            }
        }
        let tf = self.transform();
        if std::env::var_os("UACONSOLE_DEBUG_WINDOWS").is_some() {
            let ids: Vec<&str> = self.frame.layers.iter().map(|l| l.id.as_str()).collect();
            eprintln!("frame: menu open {}, windowing {}, layers {ids:?}, size {:?}", self.mixer.menu.is_some(), self.mixer.windowing.is_some(), self.frame.layout.size());
        }
        // Separate windows follow the frame's layers (opened, moved, closed). `active` is an
        // index into them: keep it on the same window, or none when that one closed.
        let active = self.active.and_then(|i| self.popups.wins.get(i)).map(|w| w.window.id());
        match (&self.gpu, self.mixer.windowing.is_some()) {
            (Some(g), true) => {
                // Unplaced (Wayland) there is no position; placed, follow the mixer's.
                let inner = g.window.inner_position();
                if inner.is_ok() || !self.popups.placed {
                    let overlays = &self.overlays;
                    let title = |l: &rack::Layer| overlays.title(&l.id).unwrap_or_else(|| crate::popups::title(l));
                    self.popups.sync(event_loop, &g.surface, inner.unwrap_or_default(), tf, &self.frame.layers, &self.parts_of, &title);
                }
            }
            _ => self.popups.wins.clear(),
        }
        // A plug-in window opened again: bring it forward.
        if let Some(id) = self.overlays.focus.take() {
            if let Some(w) = self.popups.wins.iter().find(|w| w.id == id) {
                w.window.focus_window();
            }
        }
        self.active = active.and_then(|id| self.popups.index_of(id));
        let Some(g) = self.gpu.as_mut() else { return };
        let size = g.surface.size();
        let Some(frame) = g.surface.acquire() else {
            return;
        };
        let t = Instant::now();
        let view = frame
            .texture
            .create_view(&gfx::wgpu::TextureViewDescriptor::default());
        let main_list = self.parts.first().map(|p| &p.list).unwrap_or(&self.frame.list);
        g.renderer.render_at(
            &g.surface.device,
            &g.surface.queue,
            main_list,
            &view,
            size,
            tf.scale,
            tf.offset,
            LETTERBOX,
        );
        g.window.pre_present_notify();
        g.surface.present(frame);
        // Each separate window: its part of the frame, its rect's origin at the window's corner.
        for w in &mut self.popups.wins {
            let Some(k) = self.frame.layers.iter().position(|l| l.id == w.id) else { continue };
            let Some(part) = self.parts.get(k + 1) else { continue };
            let Some(pf) = w.surface.acquire(&g.surface) else { continue };
            let pv = pf.texture.create_view(&gfx::wgpu::TextureViewDescriptor::default());
            let off = (-w.rect.x * tf.scale, -w.rect.y * tf.scale);
            g.renderer.render_at(&g.surface.device, &g.surface.queue, &part.list, &pv, w.surface.size(), tf.scale, off, LETTERBOX);
            w.window.pre_present_notify();
            g.surface.present(pf);
        }
        self.times.rendered(Instant::now(), t.elapsed());
    }

    /// Routes actions through the mixer UI state (navigator, browser, popovers, modifiers),
    /// then to the model; insert taps that reach the model open plug-in panels instead.
    fn apply(&mut self, actions: Vec<Action>) {
        if actions.is_empty() {
            return;
        }
        for a in actions {
            for effect in self.mixer.route(a, &*self.model) {
                match effect {
                    Effect::Model(Action::Tap(ControlId {
                        strip,
                        control: Control::Insert(slot),
                    })) => {
                        if self.overlays.on_open_plugin(&strip, slot, &*self.model) {
                            if let Some(id) = self.overlays.top_layer_id() {
                                self.popups.focus(&id);
                            }
                        }
                    }
                    Effect::Model(a) => self.model.apply(&a),
                    Effect::SetText(id, text) => self.model.set_text(&id, &text),
                }
            }
        }
        self.changed();
    }

    /// Right click / long press: insert context menu, browser assign mode.
    fn context_click(&mut self) {
        let (x, y) = self.cursor_pt();
        if self.dirty {
            self.rebuild();
        }
        if self.mixer.context_click(active_hits!(self), x, y) {
            self.changed();
        }
    }

    /// Keys for a focused text field (rename, tempo, browser search). True if consumed.
    fn text_key(&mut self, event: &KeyEvent) -> bool {
        if !self.mixer.editing() || self.mods.control_key() || self.mods.super_key() {
            return false;
        }
        match &event.logical_key {
            Key::Named(NamedKey::Enter) => self.mixer.key_enter(&mut *self.model),
            Key::Named(NamedKey::Backspace) => self.mixer.key_backspace(),
            Key::Named(NamedKey::Escape) => self.mixer.key_escape(),
            _ => match &event.text {
                Some(t) if !t.is_empty() && !t.chars().any(char::is_control) => {
                    self.mixer.key_text(t)
                }
                _ => return false,
            },
        }
        self.changed();
        true
    }

    fn route(&mut self, ev: PointerEvent) -> bool {
        if let Some(w) = self.active.and_then(|i| self.popups.wins.get(i)) {
            // Input in a plug-in window (or its menu's) is that window's panel's (the stack
            // serves the top one). Any other window's (Settings, panels, menus) is never the
            // overlays': their rects overlap a plug-in window's in mixer points, most of all
            // unplaced, where every free window is centred on the mixer.
            let Some(id) = crate::popups::plugin_layer(&w.id) else { return false };
            self.overlays.raise(&id);
        } else if self.separate && !self.popups.placed && self.active_part() == Some(0) {
            // Unplaced, plug-in windows are elsewhere: nothing in the mixer window is theirs.
            return false;
        }
        let routed = self.overlays.pointer(ev, &mut *self.model);
        let forwarded = self.overlays.take_mixer_actions();
        if !forwarded.is_empty() {
            self.apply(forwarded);
        }
        match routed {
            Routed::ToMixer => false,
            Routed::Consumed => true,
            Routed::Changed => {
                self.changed();
                true
            }
        }
    }

    fn pointer_down(&mut self) {
        let double = self
            .clicks
            .press(Instant::now(), self.cursor, 4.0 * self.scale_factor);
        let (x, y) = self.cursor_pt();
        if self.route(PointerEvent::Down { x, y, double }) {
            return;
        }
        if self.dirty {
            self.rebuild(); // hit values must match what the model holds now
        }
        // A loaded insert or a browser row: the tap waits for the release (it may be a drag);
        // a double-click still activates the row now (as the FFI host does).
        let armed = self.mixer.drag_press(active_hits!(self), x, y, &*self.model);
        self.interaction.controls_mode = rack::ControlsMode::of(&*self.model);
        self.interaction.set_default = self.mixer.modifier == Some(rack::surfaces::Modifier::SetDefault);
        self.interaction.mods = self.key_mods();
        let actions = self
            .interaction
            .pointer_down(active_hits!(self), x, y, double);
        if armed {
            if let [Action::Tap(_)] = actions.as_slice() {
                self.deferred_tap = active_hits!(self).iter().rev().find(|h| h.rect.contains(x, y)).cloned();
                return;
            }
        }
        self.apply(actions);
    }

    /// Console's copy-drag modifier: Option on macOS, Ctrl or Alt elsewhere.
    fn copy_modifier(&self) -> bool {
        if cfg!(target_os = "macos") { self.mods.alt_key() } else { self.mods.control_key() || self.mods.alt_key() }
    }

    /// Effects of a drop go straight to the model (they are model writes, not taps to route).
    fn apply_effects(&mut self, effects: Vec<Effect>) {
        for e in effects {
            match e {
                Effect::Model(a) => self.model.apply(&a),
                Effect::SetText(id, text) => self.model.set_text(&id, &text),
            }
        }
        self.changed();
    }

    fn pointer_moved(&mut self, pos: (f64, f64)) {
        if let Some(w) = self.active.and_then(|i| self.popups.wins.get_mut(i)) {
            w.cursor = pos;
            self.pointer_motion();
            return;
        }
        let last = std::mem::replace(&mut self.cursor, pos);
        if self.panning {
            self.pan_by((pos.0 - last.0) as f32, (pos.1 - last.1) as f32);
            return;
        }
        self.pointer_motion();
    }

    /// The pointer moved in the active window: overlays, drags, menu hover.
    fn pointer_motion(&mut self) {
        let (x, y) = self.cursor_pt();
        if self.route(PointerEvent::Move {
            x,
            y,
            fine: self.fine(),
        }) {
            return;
        }
        if self.mixer.drag_pending.is_some() || self.mixer.dragging() {
            let copy = self.copy_modifier();
            // Placed, a separate window covers the mixer under it: a slot hidden there is no
            // drop target (the mixer keeps the pointer during the drag).
            let covered =
                self.popups.placed && self.active.is_none() && self.popups.wins.iter().any(|w| w.rect.contains(x, y));
            let hits = if covered { &[][..] } else { active_hits!(self) };
            if self.mixer.drag_motion(hits, x, y, copy, &*self.model) {
                self.changed();
            }
        } else if self.interaction.is_dragging() {
            let actions = self.interaction.pointer_move(x, y, self.fine());
            self.apply(actions);
        } else if self.mixer.menu.is_some() && self.mixer.menu_hover(active_hits!(self), x, y) {
            self.changed();
        }
    }

    fn pointer_up(&mut self) {
        let (x, y) = self.cursor_pt();
        let tap = self.deferred_tap.take();
        match self.mixer.drag_release(&*self.model) {
            Some(effects) => {
                self.apply_effects(effects);
                return;
            }
            None => {
                if let Some(hit) = tap.filter(|h| h.rect.contains(x, y)) {
                    self.apply(vec![Action::Tap(hit.id)]);
                    return;
                }
            }
        }
        self.route(PointerEvent::Up { x, y });
        // Always end a mixer gesture, even if an overlay took the release.
        let actions = self.interaction.pointer_up();
        self.apply(actions);
    }

    fn wheel(&mut self, delta: MouseScrollDelta) {
        let (lines, px) = match delta {
            MouseScrollDelta::LineDelta(x, y) => {
                ((x as f64, y as f64), (x * PAN_PER_LINE, y * PAN_PER_LINE))
            }
            MouseScrollDelta::PixelDelta(p) => (
                (p.x / PIXELS_PER_LINE, p.y / PIXELS_PER_LINE),
                (p.x as f32, p.y as f32),
            ),
        };
        let (x, y) = self.cursor_pt();
        let fine = self.fine();
        if self.route(PointerEvent::Wheel {
            x,
            y,
            lines: lines.1 as f32,
            fine,
        }) {
            return;
        }
        if lines.1 != 0.0 && !self.interaction.is_dragging() {
            if self.dirty {
                self.rebuild();
            }
            // Option / Alt + wheel adjusts the control under the pointer; a plain wheel scrolls.
            if self.mods.alt_key() {
                if let Some(actions) = wheel_actions(active_hits!(self), x, y, lines.1, fine) {
                    self.apply(actions);
                    return;
                }
            }
            if self.mixer.wheel(
                active_scrolls!(self),
                x,
                y,
                -(lines.1 as f32) * SCROLL_PER_LINE,
            ) {
                self.changed();
                return;
            }
        }
        // Shift+wheel pans horizontally.
        let (dx, dy) = if self.mods.shift_key() && px.0 == 0.0 {
            (px.1, 0.0)
        } else {
            px
        };
        self.pan_by(dx, dy);
    }

    fn pan_by(&mut self, dx: f32, dy: f32) {
        let before = self.transform();
        self.view.pan_by(
            dx,
            dy,
            self.logical_size(),
            self.surface_size(),
            self.scale_factor,
        );
        if self.transform() != before {
            self.changed();
        }
    }

    fn set_zoom(&mut self, zoom: f32) {
        let (w, h) = self.surface_size();
        let anchor = (w as f64 / 2.0, h as f64 / 2.0);
        self.view
            .set_zoom(zoom, anchor, self.logical_size(), (w, h), self.scale_factor);
        self.changed();
    }

    fn command(&mut self, cmd: Command, repeat: bool, event_loop: &ActiveEventLoop) {
        self.run_command(cmd, repeat, event_loop);
        if matches!(
            cmd,
            Command::ZoomIn
                | Command::ZoomOut
                | Command::ZoomReset
                | Command::ToggleViewMode
                | Command::ToggleCompact
        ) {
            self.save_settings();
        }
    }

    fn run_command(&mut self, cmd: Command, repeat: bool, event_loop: &ActiveEventLoop) {
        match cmd {
            Command::ZoomIn => self.set_zoom(zoom_in(self.view.zoom)),
            Command::ZoomOut => self.set_zoom(zoom_out(self.view.zoom)),
            _ if repeat => {}
            Command::Quit => self.quit(event_loop),
            Command::ToggleFullscreen => {
                if let Some(g) = &self.gpu {
                    let full = g.window.fullscreen().is_some();
                    g.window.set_fullscreen(if full {
                        None
                    } else {
                        Some(Fullscreen::Borderless(None))
                    });
                }
            }
            Command::ZoomReset => self.set_zoom(1.0),
            Command::ToggleViewMode => {
                self.view.set_mode(if self.view.mode == ViewMode::Fit {
                    ViewMode::OneToOne
                } else {
                    ViewMode::Fit
                });
                self.changed();
            }
            Command::ToggleCompact => {
                self.mixer.compact = !self.mixer.compact;
                self.rebuild();
                self.request_redraw();
            }
            Command::ToggleStats => {
                self.show_stats = !self.show_stats;
                self.changed();
            }
            Command::Escape => {
                if self.mixer.drag_pending.is_some() || self.mixer.dragging() {
                    self.mixer.drag_cancel();
                    self.deferred_tap = None;
                    self.changed();
                } else if self.overlays.dismiss_top() {
                    self.changed();
                } else if self.mixer.menu.is_some()
                    || self.mixer.popover.is_some()
                    || self.mixer.sheet.is_some()
                    || self.mixer.modifier.is_some()
                    || self.mixer.editing()
                {
                    self.mixer.key_escape();
                    self.changed();
                }
            }
        }
    }

    /// The settings as the app currently stands.
    fn current_settings(&self) -> Settings {
        let mut next = self.settings.clone();
        next.zoom = self.view.zoom;
        next.compact = self.mixer.compact;
        next.view = self.view.mode;
        next.mixer = MixerPrefs::capture(&self.mixer);
        // A full-screen window's size is the screen's, not the user's.
        if self.gpu.as_ref().is_some_and(|g| g.window.fullscreen().is_some()) {
            next.mixer.window = self.settings.mixer.window;
        }
        next
    }

    /// Persists zoom, view mode, compact, the mixer's view state and the engine when they changed.
    fn save_settings(&mut self) {
        self.save_at = None;
        let mut next = self.current_settings();
        if let ModelSource::Engine { addr, .. } = &self.source {
            next.engine = Some(addr.clone());
        }
        if next != self.settings {
            match next.save() {
                Ok(_) => self.settings = next,
                Err(e) => eprintln!("uaconsole: could not save settings: {e}"),
            }
        }
    }

    /// Shell commands the Console menus asked for (`ViewState::take_commands`).
    fn run_menu_commands(&mut self, event_loop: &ActiveEventLoop) {
        use rack::menu::Command as C;
        for c in self.mixer.take_commands() {
            match c {
                C::Quit => self.quit(event_loop),
                C::ToggleFullScreen => self.run_command(Command::ToggleFullscreen, false, event_loop),
                C::ShowHideFloatingWindows => self.toggle_floating(),
                C::NextWindow | C::PreviousWindow => self.cycle_window(c == C::NextWindow),
                C::ShowNotifications => {
                    self.mixer.show_notifications(&*self.model);
                    self.changed();
                }
                other => eprintln!("uaconsole: {} ({}): not available yet", other.label(), other.id()),
            }
        }
        // Show In Finder, documentation, Dante pages, feedback (`ViewState::take_open_urls`).
        for url in self.mixer.take_open_urls() {
            open_target(&url);
        }
    }

    /// Window › Show/Hide Floating Windows (⇧⌘W): closes the plug-in windows and the free
    /// window (Settings, MONITOR CONTROLLER, …) and remembers them; the next one reopens them.
    /// Their OS windows (native Wayland toplevels too) follow their layers.
    fn toggle_floating(&mut self) {
        match self.hidden_floating.take() {
            Some(panels) => {
                self.overlays.restore_floating(panels);
                self.mixer.show_floating();
            }
            None => {
                let panels = self.overlays.take_floating();
                if self.mixer.hide_floating() || !panels.is_empty() {
                    self.hidden_floating = Some(panels);
                }
            }
        }
        self.changed();
    }

    /// Window › Next / Previous Window: focus moves round the mixer and the free windows
    /// (Settings, panels, plug-in windows) in stacking order. Drawn inside the mixer window, the
    /// plug-in window reached is raised.
    fn cycle_window(&mut self, forward: bool) {
        let ids: Vec<String> = self.frame.layers.iter().filter(|l| crate::popups::toplevel(l)).map(|l| l.id.clone()).collect();
        let current = match self.popups.wins.iter().find(|w| w.focused) {
            Some(w) => Some(w.id.clone()),
            // In the mixer window: the topmost plug-in window counts as current when unfocused
            // windows are drawn inside it.
            None if self.popups.wins.is_empty() => ids.iter().rev().find(|i| i.starts_with("plugin:")).cloned(),
            None => None,
        };
        match crate::popups::cycle(&ids, current.as_deref(), forward) {
            Some(id) => {
                self.overlays.raise(&id);
                // Focused after the next frame's window sync (as a reopened plug-in window).
                self.overlays.focus = Some(id);
            }
            None => {
                if let Some(g) = &self.gpu {
                    g.window.focus_window();
                }
            }
        }
        self.changed();
    }

    fn quit(&mut self, event_loop: &ActiveEventLoop) {
        self.save_settings();
        self.model.quit();
        event_loop.exit();
    }

    fn create_window(&mut self, event_loop: &ActiveEventLoop) -> Result<(), String> {
        let (lw, lh) = self.logical_size();
        let zoom = if self.view.mode == ViewMode::OneToOne {
            self.view.zoom
        } else {
            1.0
        };
        let (mut w, mut h) = (lw as f64 * zoom as f64, lh as f64 * zoom as f64);
        if let Some(m) = event_loop
            .primary_monitor()
            .or_else(|| event_loop.available_monitors().next())
        {
            let max = m.size().to_logical::<f64>(m.scale_factor());
            let k = (max.width * 0.9 / w).min(max.height * 0.9 / h).min(1.0);
            (w, h) = (w * k, h * k);
        }
        #[allow(unused_mut)]
        let mut attrs = Window::default_attributes()
            .with_title("UAD Console")
            .with_inner_size(LogicalSize::new(w.round(), h.round()))
            .with_min_inner_size(LogicalSize::new(320.0, 200.0));
        // Wayland app_id and X11 WM_CLASS, for desktop entries, docks and window rules.
        #[cfg(all(
            unix,
            not(any(target_os = "macos", target_os = "ios", target_os = "android"))
        ))]
        {
            use winit::platform::{
                wayland::WindowAttributesExtWayland, x11::WindowAttributesExtX11,
            };
            attrs = WindowAttributesExtWayland::with_name(attrs, "uaconsole", "uaconsole");
            attrs = WindowAttributesExtX11::with_name(attrs, "uaconsole", "uaconsole");
        }
        let window = Arc::new(
            event_loop
                .create_window(attrs)
                .map_err(|e| format!("create window: {e}"))?,
        );
        self.scale_factor = window.scale_factor();
        let size = window.inner_size();
        let surface = gfx::SurfaceGpu::new(
            Box::new(event_loop.owned_display_handle()),
            window.clone(),
            (size.width, size.height),
        )?;
        eprintln!(
            "gpu: {} ({:?}), surface {:?} {:?}, {}×{} px @ {:.2}x",
            surface.adapter_info.name,
            surface.adapter_info.backend,
            surface.config.format,
            surface.config.present_mode,
            size.width,
            size.height,
            self.scale_factor
        );
        let renderer = gfx::Renderer::new(
            &surface.device,
            &surface.queue,
            surface.format(),
            self.assets.images.clone(),
        );
        self.meter_period = meter_period(window.current_monitor().and_then(|m| m.refresh_rate_millihertz()));
        self.gpu = Some(Gpu {
            window,
            surface,
            renderer,
        });
        // Console's menus and panels are windows of their own. Native Wayland cannot place
        // them: there only the free windows are (`popups::toplevel`; --x11 places them all).
        self.separate = self.windows == WindowsMode::Separate;
        self.popups.placed = can_place_windows(event_loop);
        self.title.clear();
        self.changed();
        Ok(())
    }

    /// Events for one of Console's separate windows (`popups`).
    fn popup_event(&mut self, event_loop: &ActiveEventLoop, i: usize, event: WindowEvent) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.active = Some(i);
                self.pointer_moved((position.x, position.y));
            }
            WindowEvent::MouseInput { state, button, .. } => {
                self.active = Some(i);
                match (button, state) {
                    (MouseButton::Left, ElementState::Pressed) => {
                        if !self.title_bar_drag(i) {
                            self.pointer_down();
                        }
                    }
                    (MouseButton::Left, ElementState::Released) => self.pointer_up(),
                    (MouseButton::Right, ElementState::Pressed) => self.context_click(),
                    _ => {}
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                self.active = Some(i);
                self.wheel(delta);
            }
            WindowEvent::Focused(f) => {
                let Some(w) = self.popups.wins.get_mut(i) else { return };
                let lost = w.focused && !f;
                w.focused = f;
                self.focus_check |= lost && self.mixer.menu.is_some();
            }
            // Moved by its title bar (window manager): remember where, like Console.
            WindowEvent::Moved(pos) => {
                let Some(w) = self.popups.wins.get_mut(i) else { return };
                if pos != w.at && matches!(w.kind, rack::LayerKind::Window | rack::LayerKind::Dialog) {
                    w.at = pos;
                    self.moved.insert(w.id.clone(), pos);
                    self.changed();
                }
            }
            // Its window closed: close what it shows.
            WindowEvent::CloseRequested => {
                let Some(w) = self.popups.wins.get(i) else { return };
                let (id, kind) = (w.id.clone(), w.kind);
                if self.overlays.close(&id, &mut *self.model) {
                } else if id == "channels" {
                    self.mixer.show_channels = false;
                } else if kind == rack::LayerKind::Window {
                    // Settings or a panel: everything opened over it goes with it.
                    for _ in 0..16 {
                        if self.mixer.sheet.is_none() {
                            break;
                        }
                        self.mixer.key_escape();
                    }
                } else {
                    self.mixer.key_escape();
                }
                self.active = None;
                self.changed();
            }
            WindowEvent::Resized(size) => {
                if let (Some(g), Some(w)) = (&self.gpu, self.popups.wins.get_mut(i)) {
                    w.surface.resize(&g.surface, size.width, size.height);
                }
                self.request_redraw();
            }
            // Drawn with the mixer.
            WindowEvent::RedrawRequested => self.request_redraw(),
            // Keys reach the same handlers whichever window has focus.
            WindowEvent::KeyboardInput { .. } | WindowEvent::ModifiersChanged(_) => {
                if let Some(g) = &self.gpu {
                    let id = g.window.id();
                    self.handle_window_event(event_loop, id, event);
                }
            }
            _ => {}
        }
    }

    /// A press on a free window's or dialog's title bar (not on its close X or other
    /// controls) moves the window, as Console's do.
    fn title_bar_drag(&mut self, i: usize) -> bool {
        let Some(w) = self.popups.wins.get(i) else { return false };
        let (x, y) = self.cursor_pt();
        // An unplaced plug-in window (a toplevel the compositor moves) by its own title bar.
        if !self.popups.placed && self.overlays.title_bar_at(&w.id, x, y) {
            return w.window.drag_window().is_ok();
        }
        if !matches!(w.kind, rack::LayerKind::Window | rack::LayerKind::Dialog) || w.title_h <= 0.0 {
            return false;
        }
        if y - w.rect.y > w.title_h {
            return false;
        }
        let top = active_hits!(self).iter().rev().find(|h| h.rect.contains(x, y));
        if top.is_some_and(|h| h.id.control != Control::PopoverDismiss) {
            return false;
        }
        w.window.drag_window().is_ok()
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.gpu.is_some() {
            return;
        }
        if let Err(e) = self.create_window(event_loop) {
            self.error = Some(e);
            event_loop.exit();
        }
    }

    fn new_events(&mut self, _: &ActiveEventLoop, cause: StartCause) {
        if let StartCause::ResumeTimeReached {
            requested_resume, ..
        } = cause
        {
            self.next_tick = Some(requested_resume + self.meter_period);
            self.changed();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        if std::env::var_os("UACONSOLE_DEBUG_EVENTS").is_some() && !matches!(event, WindowEvent::RedrawRequested) {
            eprintln!("event {} {:?}", if self.popups.index_of(id).is_some() { "popup" } else { "mixer" }, event);
        }
        if let Some(i) = self.popups.index_of(id) {
            self.popup_event(event_loop, i, event);
        } else {
            if matches!(event, WindowEvent::CursorMoved { .. } | WindowEvent::MouseInput { .. } | WindowEvent::MouseWheel { .. }) {
                self.active = None;
            }
            self.handle_window_event(event_loop, id, event);
        }
        if !self.mixer.commands.is_empty() || !self.mixer.open_urls.is_empty() {
            self.run_menu_commands(event_loop);
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // Changed view state is written once it has been still for SAVE_DELAY.
        let now = Instant::now();
        if self.current_settings().mixer == self.settings.mixer {
            self.save_at = None;
        } else if self.save_at.is_some_and(|t| now >= t) {
            self.save_settings();
        } else {
            self.save_at.get_or_insert(now + SAVE_DELAY);
        }
        // Console closes its menus when the app loses focus (none of its windows has it).
        if std::mem::take(&mut self.focus_check)
            && !self.main_focused
            && !self.popups.any_focused()
            && self.mixer.menu.is_some()
        {
            self.mixer.menu = None;
            self.changed();
        }
        // OPTIONS › MODIFIERS TIMEOUT: an armed navigator modifier disarms on its own.
        if self.mixer.expire_modifier(self.model.as_ref(), Instant::now()) {
            self.changed();
        }
        let modifier_deadline = [self.mixer.modifier_deadline(self.model.as_ref()), self.save_at]
            .into_iter()
            .flatten()
            .min();
        let live =
            (self.live_meters || self.overlays.live()) && self.gpu.is_some() && !self.occluded;
        if !live {
            self.next_tick = None;
            event_loop.set_control_flow(match modifier_deadline {
                Some(t) => ControlFlow::WaitUntil(t),
                None => ControlFlow::Wait,
            });
            return;
        }
        let now = Instant::now();
        let next = match self.next_tick {
            Some(t) if t > now => t,
            _ => now + self.meter_period,
        };
        self.next_tick = Some(next);
        event_loop.set_control_flow(ControlFlow::WaitUntil(next));
    }
}

/// Clients can place windows (X11, macOS, Windows); native Wayland cannot.
fn can_place_windows(event_loop: &ActiveEventLoop) -> bool {
    #[cfg(all(unix, not(any(target_os = "macos", target_os = "ios", target_os = "android"))))]
    {
        use winit::platform::wayland::ActiveEventLoopExtWayland;
        !event_loop.is_wayland()
    }
    #[cfg(not(all(unix, not(any(target_os = "macos", target_os = "ios", target_os = "android")))))]
    {
        let _ = event_loop;
        true
    }
}

/// Console's keyboard shortcuts (`rack::menu::command_for_key`): Ctrl / ⌘ as "command".
fn console_key(key: &Key, mods: ModifiersState) -> Option<rack::menu::Command> {
    let name = match key {
        Key::Named(NamedKey::ArrowLeft) => "left".to_string(),
        Key::Named(NamedKey::ArrowRight) => "right".to_string(),
        Key::Character(s) if s.is_ascii() => s.to_ascii_lowercase(),
        _ => return None,
    };
    rack::menu::command_for_key(&name, mods.control_key() || mods.super_key(), mods.shift_key(), mods.alt_key())
}

/// Opens a URL in the desktop's browser.
fn open_url(url: &str) {
    let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    if let Err(e) = std::process::Command::new(opener).arg(url).spawn() {
        eprintln!("uaconsole: {opener} {url}: {e}");
    }
}

/// Opens what the console asked for: a `file://` URL is revealed like Console's Show In Finder
/// (macOS selects the file in Finder; Linux file managers open its folder), the Dante
/// Controller page launches Dante Controller when it is installed (macOS), anything else opens.
fn open_target(url: &str) {
    if let Some(path) = file_url_path(url) {
        let p = std::path::Path::new(&path);
        if cfg!(target_os = "macos") {
            let mut c = std::process::Command::new("open");
            if p.is_file() {
                c.arg("-R");
            }
            if let Err(e) = c.arg(&path).spawn() {
                eprintln!("uaconsole: open {path}: {e}");
            }
        } else {
            let dir = if p.is_dir() { p } else { p.parent().unwrap_or(p) };
            if !dir.exists() {
                eprintln!("uaconsole: {} does not exist", dir.display());
                return;
            }
            open_url(&rack::surfaces::session::file_url(&dir.to_string_lossy()));
        }
        return;
    }
    if cfg!(target_os = "macos") && url == rack::menu::DANTE_CONTROLLER_URL {
        let launched = std::process::Command::new("open").args(["-a", "Dante Controller"]).status().is_ok_and(|s| s.success());
        if launched {
            return;
        }
    }
    open_url(url);
}

/// The path of a `file://` URL (percent-decoded).
fn file_url_path(url: &str) -> Option<String> {
    let rest = url.strip_prefix("file://")?;
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&rest[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).ok()
}

impl App {
    fn handle_window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => self.quit(event_loop),
            WindowEvent::Resized(size) => {
                if let Some(g) = &mut self.gpu {
                    g.surface.resize(size.width, size.height);
                }
                self.changed();
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.scale_factor = scale_factor;
                self.changed();
            }
            WindowEvent::Occluded(occluded) => {
                if self.occluded != occluded {
                    self.model.set_background(occluded);
                }
                self.occluded = occluded;
                if !occluded {
                    self.changed();
                }
            }
            WindowEvent::Focused(true) => {
                self.main_focused = true;
            }
            WindowEvent::Focused(false) => {
                if std::env::var_os("UACONSOLE_DEBUG_WINDOWS").is_some() {
                    eprintln!("mixer window lost focus");
                }
                // Only a menu that is open when focus goes closes (JUCE); windows report an
                // initial unfocused state, which is not a change.
                self.focus_check |= self.main_focused && self.mixer.menu.is_some();
                self.main_focused = false;
                let actions = self.interaction.pointer_up();
                self.apply(actions);
                self.panning = false;
            }
            WindowEvent::ModifiersChanged(m) => self.mods = m.state(),
            WindowEvent::KeyboardInput {
                event,
                is_synthetic: false,
                ..
            } if event.state == ElementState::Pressed => {
                if self.text_key(&event) {
                    return;
                }
                if let Some(cmd) = shortcut(&event.logical_key, event.physical_key, self.mods) {
                    self.command(cmd, event.repeat, event_loop);
                } else if let Some(c) = console_key(&event.logical_key, self.mods) {
                    if self.mixer.run_command_id(c.id(), &mut *self.model) {
                        self.changed();
                    }
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.pointer_moved((position.x, position.y))
            }
            WindowEvent::MouseInput { state, button, .. } => match (button, state) {
                (MouseButton::Left, ElementState::Pressed) => self.pointer_down(),
                (MouseButton::Left, ElementState::Released) => self.pointer_up(),
                (MouseButton::Right, ElementState::Pressed) => self.context_click(),
                (MouseButton::Middle, ElementState::Pressed) => self.panning = true,
                (MouseButton::Middle, ElementState::Released) => self.panning = false,
                _ => {}
            },
            WindowEvent::MouseWheel { delta, .. } => self.wheel(delta),
            WindowEvent::RedrawRequested => self.redraw(event_loop),
            // Separate windows are placed relative to the mixer: follow it.
            WindowEvent::Moved(p) if self.separate => {
                if std::env::var_os("UACONSOLE_DEBUG_WINDOWS").is_some() {
                    eprintln!("mixer window moved to {p:?}");
                }
                self.changed()
            }
            _ => {}
        }
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meters_redraw_at_the_display_rate() {
        assert_eq!(meter_period(None), METER_PERIOD);
        assert_eq!(meter_period(Some(0)), METER_PERIOD);
        assert!((meter_period(Some(120_000)).as_secs_f64() - 1.0 / 120.0).abs() < 1e-6);
        // Below 60 Hz still steps like Console; above 120 Hz no faster.
        assert!((meter_period(Some(30_000)).as_secs_f64() - 1.0 / 60.0).abs() < 1e-6);
        assert!((meter_period(Some(240_000)).as_secs_f64() - 1.0 / 120.0).abs() < 1e-6);
    }

    #[test]
    fn saved_view_state_applies_and_view_spec_overrides() {
        let settings = Settings::parse(
            "sends = true\ninputs = false\ninsert_size = \"small\"\nfixed_slots = false\nbrowser = false\n\
             browser_tab = \"presets\"\nctrl_room = true\nwindow = \"1300x700\"\n",
        );
        let m = initial_mixer(false, &settings, None);
        assert!(m.sections.sends && !m.sections.inputs && m.sections.inserts);
        assert_eq!(m.insert_size, rack::surfaces::InsertSize::Small);
        assert!(!m.fixed_slots && !m.show_browser && m.show_ctrl_room);
        assert_eq!(m.browser.tab, rack::surfaces::BrowserTab::Presets);
        assert_eq!(m.window, Some((1300.0, 700.0)));
        // Nothing saved: the stock view. `--view` wins over what was saved.
        assert_eq!(initial_mixer(false, &Settings::default(), None).sections, ViewState::default().sections);
        assert!(!initial_mixer(false, &settings, Some("sends=0,large")).sections.sends);
        assert!(initial_mixer(true, &settings, None).compact);
    }
}
