//! The window: winit event loop, wgpu surface, input routing and the redraw policy.
//!
//! Redraws happen on input and model changes only; while live meters (or a live panel) are on,
//! a 30 Hz `WaitUntil` timer drives them. Otherwise the loop sleeps in `ControlFlow::Wait`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use houston::Rgba;
use rack::{Action, Assets, Control, ControlId, Effect, Interaction, MixerModel, ViewState};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Fullscreen, Window, WindowId};

use crate::frame::{Frame, FrameTimes, Hud, build_frame, layout_for};
use crate::input::{ClickTracker, Command, PIXELS_PER_LINE, shortcut, wheel_actions};
use crate::overlay::{OverlayStack, PointerEvent, Routed};
use crate::settings::Settings;
use crate::source::{ModelSource, OpenedModel};
use crate::view::{Transform, View, ViewMode, zoom_in, zoom_out};

pub const METER_PERIOD: Duration = Duration::from_nanos(33_333_333);
pub const LETTERBOX: Rgba = Rgba::rgb8(18, 19, 21);
/// Pan distance per wheel line, in pixels.
const PAN_PER_LINE: f32 = 60.0;
/// Scroll distance per wheel line in the browser list and cells column, in points.
const SCROLL_PER_LINE: f32 = 40.0;

pub struct Options {
    pub source: ModelSource,
    pub view: View,
    pub compact: bool,
    /// Force the 30 Hz meter timer even when the model is not live (testing).
    pub live_meters: bool,
    /// Start with the stats overlay.
    pub stats: bool,
    pub open_plugin: Option<(String, u8)>,
    /// `--view` spec applied to the mixer view at start.
    pub view_spec: Option<String>,
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
    occluded: bool,
    scale_factor: f64,
    title: String,
    pub error: Option<String>,
}

/// Runs the window until it is closed. Returns an error message if it could not start.
pub fn run(assets: Assets, opts: Options) -> Result<(), String> {
    let event_loop = EventLoop::new().map_err(|e| format!("event loop: {e}"))?;
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
        if let Some((strip, slot)) = &opts.open_plugin {
            if !overlays.on_open_plugin(strip, *slot, &*model) {
                eprintln!("uaconsole: --open-plugin {strip}:{slot}: no plug-in loaded there");
            }
        }
        overlays.sync(&mut *model);
        let area = houston::Rect::new(0.0, 0.0, 1.0, 1.0);
        let mut mixer = ViewState {
            compact: opts.compact,
            ..ViewState::default()
        };
        if let Some(spec) = &opts.view_spec {
            if let Err(e) = mixer.apply_spec(spec) {
                eprintln!("uaconsole: --view: {e}");
            }
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
            cursor: (0.0, 0.0),
            panning: false,
            mods: ModifiersState::empty(),
            overlays,
            show_stats: opts.stats,
            times: FrameTimes::default(),
            next_tick: None,
            occluded: false,
            scale_factor: 1.0,
            title: String::new(),
            error: None,
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
        self.transform().to_logical(self.cursor)
    }

    fn fine(&self) -> bool {
        self.mods.shift_key() || self.mods.alt_key()
    }

    fn request_redraw(&self) {
        if let Some(g) = &self.gpu {
            g.window.request_redraw();
        }
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
        let area = tf.visible(logical, surface);
        let stats = self.show_stats.then(|| self.stats_text(tf));
        let hud = stats.as_deref().map(|text| Hud {
            text,
            px_per_pt: tf.scale,
        });
        self.overlays.sync(&mut *self.model);
        self.frame = build_frame(
            &self.assets,
            &*self.model,
            &self.mixer,
            &mut self.overlays,
            area,
            hud,
        );
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

    fn redraw(&mut self) {
        // Engine pushes land here once per frame; live meters rebuild every tick (≈0.2 ms).
        if self.model.pump() || self.live_meters {
            self.dirty = true;
        }
        if self.dirty || self.show_stats {
            self.rebuild();
        }
        let tf = self.transform();
        let Some(g) = self.gpu.as_mut() else { return };
        let size = g.surface.size();
        let Some(frame) = g.surface.acquire() else {
            return;
        };
        let t = Instant::now();
        let view = frame
            .texture
            .create_view(&gfx::wgpu::TextureViewDescriptor::default());
        g.renderer.render_at(
            &g.surface.device,
            &g.surface.queue,
            &self.frame.list,
            &view,
            size,
            tf.scale,
            tf.offset,
            LETTERBOX,
        );
        g.window.pre_present_notify();
        g.surface.present(frame);
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
                        self.overlays.on_open_plugin(&strip, slot, &*self.model);
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
        if self.mixer.context_click(&self.frame.hits, x, y) {
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
        match self.overlays.pointer(ev, &mut *self.model) {
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
        let actions = self
            .interaction
            .pointer_down(&self.frame.hits, x, y, double);
        self.apply(actions);
    }

    fn pointer_moved(&mut self, pos: (f64, f64)) {
        let last = std::mem::replace(&mut self.cursor, pos);
        if self.panning {
            self.pan_by((pos.0 - last.0) as f32, (pos.1 - last.1) as f32);
            return;
        }
        let (x, y) = self.cursor_pt();
        if self.route(PointerEvent::Move {
            x,
            y,
            fine: self.fine(),
        }) {
            return;
        }
        if self.interaction.is_dragging() {
            let actions = self.interaction.pointer_move(x, y, self.fine());
            self.apply(actions);
        }
    }

    fn pointer_up(&mut self) {
        let (x, y) = self.cursor_pt();
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
            if let Some(actions) = wheel_actions(&self.frame.hits, x, y, lines.1, fine) {
                self.apply(actions);
                return;
            }
            if self.mixer.wheel(
                &self.frame.scrolls,
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
                if self.overlays.dismiss_top() {
                    self.changed();
                } else if self.mixer.popover.is_some()
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

    /// Persists zoom, view mode, compact and the engine when they changed.
    fn save_settings(&mut self) {
        let mut next = self.settings.clone();
        next.zoom = self.view.zoom;
        next.compact = self.mixer.compact;
        next.view = self.view.mode;
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

    fn quit(&mut self, event_loop: &ActiveEventLoop) {
        self.save_settings();
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
        self.gpu = Some(Gpu {
            window,
            surface,
            renderer,
        });
        self.title.clear();
        self.changed();
        Ok(())
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
            self.next_tick = Some(requested_resume + METER_PERIOD);
            self.changed();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
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
                self.occluded = occluded;
                if !occluded {
                    self.changed();
                }
            }
            WindowEvent::Focused(false) => {
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
            WindowEvent::RedrawRequested => self.redraw(),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let live =
            (self.live_meters || self.overlays.live()) && self.gpu.is_some() && !self.occluded;
        if !live {
            self.next_tick = None;
            event_loop.set_control_flow(ControlFlow::Wait);
            return;
        }
        let now = Instant::now();
        let next = match self.next_tick {
            Some(t) if t > now => t,
            _ => now + METER_PERIOD,
        };
        self.next_tick = Some(next);
        event_loop.set_control_flow(ControlFlow::WaitUntil(next));
    }
}
