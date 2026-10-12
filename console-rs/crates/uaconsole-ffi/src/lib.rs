//! C ABI for native Apple hosts (the iPad app, ConsoleLink's Console 1:1 screen, a macOS app):
//! houston (skins) + rack (draw list, hits, input, `ViewState`, `MixerModel`) + engine-model (the
//! UA Mixer Engine) + gfx (wgpu) behind an opaque `UacContext`.
//!
//! The C header is `apple/include/uaconsole.h`, written by hand: keep the constants, `UacStats`
//! and `UacPluginRequest` there in sync with this file (the `abi` test checks the layouts).
//!
//! Threading: a context is single-threaded. Create, drive and destroy it on one thread
//! (on iOS the main thread, where the CAMetalLayer and the CADisplayLink live). An engine
//! connection runs on its own threads; its updates are applied on the context's thread by
//! `uac_needs_frame` / `uac_frame` (`MixerModel::pump`).
//!
//! Coordinates: the host speaks *view points* (UIKit/AppKit points, origin at the layer's
//! top-left). The context maps them to Console window points through a zoom and an origin it
//! owns, so the Console window can be fit, panned and pinched.
//!
//! Layout: like Console, the full layout follows its window. By default the Console window is
//! the view's size in points (`uac_resize`), so a 1470 × 1105 pt view lays out exactly like a
//! 1470 × 1105 Console window at zoom 1; `uac_set_window_size` pins another size. Compact
//! (browser and navigator hidden) takes its width from the strips and its height from the window.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::ffi::{CStr, CString, c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use engine_model::EngineMixer;
use houston::{Resources, Rgba};
use rack::model::{InsertState, StripInfo};
use rack::{Action, Assets, Control, ControlId, Draw, DrawList, Effect, Hit, Interaction, MixerModel, PreviewMixer, ScrollRegion, Ui, ViewState, build_window_with};
use ua_engine::{Endpoint, Status};

// MARK: constants (mirror uaconsole.h)

pub const UAC_OK: i32 = 0;
/// The frame was skipped (surface timed out, was occluded, or had to be reconfigured).
pub const UAC_SKIPPED: i32 = 1;
pub const UAC_ERR_NULL: i32 = -1;
pub const UAC_ERR_SURFACE: i32 = -2;
pub const UAC_ERR_UNSUPPORTED: i32 = -3;
pub const UAC_ERR_IO: i32 = -4;
pub const UAC_ERR_ARG: i32 = -5;
pub const UAC_ERR_PANIC: i32 = -99;

pub const UAC_POINTER_DOWN: i32 = 0;
pub const UAC_POINTER_MOVE: i32 = 1;
pub const UAC_POINTER_UP: i32 = 2;
pub const UAC_POINTER_CANCEL: i32 = 3;
/// Secondary click / long press at a point: the insert context menu, browser assign mode.
pub const UAC_POINTER_SECONDARY: i32 = 4;
/// Pointer motion with no button down (mouse hover): highlights rows of an open menu.
pub const UAC_POINTER_HOVER: i32 = 5;

/// Double tap / double click: resets a dragged control to its default.
pub const UAC_POINTER_DOUBLE: u32 = 1 << 0;
/// Fine adjustment (Console's modifier-drag): motion divided by 10.
pub const UAC_POINTER_FINE: u32 = 1 << 1;

/// `uac_set_model_engine` flag: open the write gates (both the connection and the writer).
pub const UAC_ENGINE_WRITES: u32 = 1 << 0;

/// `uac_model_state` values.
pub const UAC_MODEL_PREVIEW: i32 = 0;
pub const UAC_MODEL_SNAPSHOT: i32 = 1;
pub const UAC_MODEL_CONNECTING: i32 = 2;
pub const UAC_MODEL_LOADING: i32 = 3;
pub const UAC_MODEL_LIVE: i32 = 4;
pub const UAC_MODEL_RECONNECTING: i32 = 5;
pub const UAC_MODEL_FAILED: i32 = 6;
pub const UAC_MODEL_OFFLINE: i32 = 7;

/// `uac_key` keys (for a focused text field: rename, tempo, browser search).
pub const UAC_KEY_ENTER: i32 = 1;
pub const UAC_KEY_BACKSPACE: i32 = 2;
pub const UAC_KEY_ESCAPE: i32 = 3;

/// `Control::Insert` index rack uses for the Unison slot.
pub const UAC_UNISON_SLOT: i32 = 255;

/// Letterbox colour around the Console window.
const CLEAR: Rgba = Rgba::rgb8(24, 25, 27);
const MAX_ZOOM: f32 = 4.0;
/// Live meters redraw at most this often (Console's meter rate).
const METER_PERIOD: Duration = Duration::from_millis(33);
/// Plug-in requests kept for a host that never takes them.
const MAX_PLUGIN_REQUESTS: usize = 8;

/// Per-frame numbers for a HUD or logs. All fields are filled by `uac_get_stats`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct UacStats {
    /// Frames presented since creation.
    pub frames: u64,
    /// Last frame: rebuilding the draw list and hit regions (CPU).
    pub build_ms: f64,
    /// Last frame: encoding, uploading and submitting (CPU side; excludes GPU time).
    pub render_ms: f64,
    pub draws: u32,
    pub hits: u32,
    pub quads: u32,
    pub batches: u32,
    pub textures: u32,
    pub glyphs: u32,
    pub texture_bytes: u64,
    /// Console window size in Console points (depends on the layout and the model's strips).
    pub content_width_pt: f32,
    pub content_height_pt: f32,
    /// View points per Console point.
    pub zoom: f32,
    /// Console point shown at the view's top-left (negative when letterboxed).
    pub origin_x_pt: f32,
    pub origin_y_pt: f32,
}

/// A loaded insert (or Unison) slot the user opened: the host shows its plug-in window.
/// Strings are owned by the context and valid until the next `uac_take_plugin_request` or
/// `uac_destroy`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct UacPluginRequest {
    /// The strip id (an engine node path such as `/devices/0/inputs/1`, or `in1` in the preview).
    pub strip_id: *const c_char,
    /// The strip title as the mixer shows it ("ANALOG 1").
    pub strip_title: *const c_char,
    /// The slot's engine node (`…/inserts/2`, `…/preamps/0/unison`); "" without an engine model.
    pub slot_path: *const c_char,
    /// The loaded plug-in's name (its `EffectName`).
    pub effect_name: *const c_char,
    /// 0-based insert slot, or `UAC_UNISON_SLOT`.
    pub slot: i32,
}

// MARK: errors

thread_local! {
    static LAST_ERROR: RefCell<CString> = RefCell::new(CString::default());
}

fn set_error(msg: impl Into<String>) {
    let s = msg.into().replace('\0', " ");
    LAST_ERROR.with(|e| *e.borrow_mut() = CString::new(s).unwrap_or_default());
}

fn cstring(s: impl Into<String>) -> CString {
    CString::new(s.into().replace('\0', " ")).unwrap_or_default()
}

/// Runs `f`, turning a panic into `fallback` plus a last-error message (never unwind into C).
fn guard<T>(fallback: T, f: impl FnOnce() -> T) -> T {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(v) => v,
        Err(p) => {
            let msg = p
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| p.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown".into());
            set_error(format!("panic: {msg}"));
            fallback
        }
    }
}

/// A non-empty UTF-8 C string.
///
/// # Safety
/// `p` is null or a valid NUL-terminated string.
unsafe fn str_arg(p: *const c_char) -> Option<String> {
    if p.is_null() {
        return None;
    }
    let s = unsafe { CStr::from_ptr(p) }.to_str().ok()?;
    (!s.is_empty()).then(|| s.to_string())
}

/// # Safety
/// As [`str_arg`].
unsafe fn path_arg(p: *const c_char) -> Option<PathBuf> {
    unsafe { str_arg(p) }.map(PathBuf::from)
}

// MARK: view transform

#[derive(Clone, Copy, Debug, PartialEq)]
struct View {
    width_px: u32,
    height_px: u32,
    /// Host pixels per view point (UIScreen / backing scale).
    screen_scale: f32,
    /// View points per Console point; None fits the whole window and tracks resizes.
    zoom: Option<f32>,
    /// Console point at the view's top-left.
    origin: (f32, f32),
}

impl View {
    fn new(width_px: u32, height_px: u32, screen_scale: f32) -> Self {
        let screen_scale = if screen_scale.is_finite() && screen_scale > 0.0 { screen_scale } else { 1.0 };
        Self { width_px, height_px, screen_scale, zoom: None, origin: (0.0, 0.0) }
    }

    fn size_pt(&self) -> (f32, f32) {
        (self.width_px as f32 / self.screen_scale, self.height_px as f32 / self.screen_scale)
    }

    fn fit(&self, content: (f32, f32)) -> f32 {
        let (w, h) = self.size_pt();
        if content.0 <= 0.0 || content.1 <= 0.0 || w <= 0.0 || h <= 0.0 {
            return 1.0;
        }
        (w / content.0).min(h / content.1)
    }

    fn zoom(&self, content: (f32, f32)) -> f32 {
        self.zoom.unwrap_or_else(|| self.fit(content))
    }

    fn console_point(&self, content: (f32, f32), x: f32, y: f32) -> (f32, f32) {
        let z = self.zoom(content);
        (x / z + self.origin.0, y / z + self.origin.1)
    }

    /// Centres an axis that fits; otherwise keeps the window edge-to-edge.
    fn clamp(&mut self, content: (f32, f32)) {
        let z = self.zoom(content);
        let (w, h) = self.size_pt();
        let axis = |o: f32, visible: f32, total: f32| if total <= visible { -(visible - total) / 2.0 } else { o.clamp(0.0, total - visible) };
        self.origin = (axis(self.origin.0, w / z, content.0), axis(self.origin.1, h / z, content.1));
    }

    /// Pixels per Console point.
    fn px_per_pt(&self, content: (f32, f32)) -> f32 {
        self.zoom(content) * self.screen_scale
    }
}

fn offset_list(list: &mut DrawList, dx: f32, dy: f32) {
    if dx == 0.0 && dy == 0.0 {
        return;
    }
    for item in &mut list.items {
        match item {
            Draw::Image { dst, .. } => *dst = dst.offset(dx, dy),
            Draw::Fill { rect, .. } | Draw::Text { rect, .. } => *rect = rect.offset(dx, dy),
            Draw::Clip(Some(r)) => *r = r.offset(dx, dy),
            Draw::Clip(None) => {}
        }
    }
}

// MARK: GPU

enum Target {
    /// A CAMetalLayer owned by the host view.
    Surface { surface: wgpu::Surface<'static>, config: wgpu::SurfaceConfiguration },
    /// No layer: render into a texture (tests, headless hosts).
    Offscreen { view: wgpu::TextureView },
}

struct Gpu {
    _instance: wgpu::Instance,
    hl: gfx::Headless,
    target: Target,
    format: wgpu::TextureFormat,
}

const OFFSCREEN_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

impl Gpu {
    /// # Safety
    /// `layer` is null or a `CAMetalLayer *` that outlives the context.
    unsafe fn new(layer: *mut c_void, width: u32, height: u32) -> Result<Self, String> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let surface = if layer.is_null() { None } else { Some(unsafe { layer_surface(&instance, layer) }?) };
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: surface.as_ref(),
            ..Default::default()
        }))
        .map_err(|e| format!("no GPU adapter: {e}"))?;
        let adapter_info = adapter.get_info();
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("uaconsole-ffi"),
            required_limits: adapter.limits(),
            ..Default::default()
        }))
        .map_err(|e| format!("request_device: {e}"))?;
        let (target, format) = match surface {
            Some(surface) => {
                let caps = surface.get_capabilities(&adapter);
                // Non-sRGB like the offscreen path, so the layer shows what --render writes.
                let format = caps
                    .formats
                    .iter()
                    .copied()
                    .find(|f| *f == wgpu::TextureFormat::Bgra8Unorm)
                    .or_else(|| caps.formats.iter().copied().find(|f| !f.is_srgb()))
                    .or_else(|| caps.formats.first().copied())
                    .ok_or("surface reports no formats")?;
                let alpha_mode = if caps.alpha_modes.contains(&wgpu::CompositeAlphaMode::Opaque) {
                    wgpu::CompositeAlphaMode::Opaque
                } else {
                    caps.alpha_modes.first().copied().unwrap_or(wgpu::CompositeAlphaMode::Auto)
                };
                let config = wgpu::SurfaceConfiguration {
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                    format,
                    color_space: wgpu::SurfaceColorSpace::Auto,
                    width: width.max(1),
                    height: height.max(1),
                    present_mode: wgpu::PresentMode::Fifo,
                    desired_maximum_frame_latency: 2,
                    alpha_mode,
                    view_formats: vec![],
                };
                surface.configure(&device, &config);
                (Target::Surface { surface, config }, format)
            }
            None => (Target::Offscreen { view: offscreen_view(&device, width, height) }, OFFSCREEN_FORMAT),
        };
        Ok(Self { _instance: instance, hl: gfx::Headless { device, queue, adapter_info }, target, format })
    }

    fn resize(&mut self, width: u32, height: u32) {
        match &mut self.target {
            Target::Surface { surface, config } => {
                if width > 0 && height > 0 && (config.width, config.height) != (width, height) {
                    config.width = width;
                    config.height = height;
                    surface.configure(&self.hl.device, config);
                }
            }
            Target::Offscreen { view } => *view = offscreen_view(&self.hl.device, width, height),
        }
    }
}

fn offscreen_view(device: &wgpu::Device, width: u32, height: u32) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("uac offscreen"),
            size: wgpu::Extent3d { width: width.max(1), height: height.max(1), depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: OFFSCREEN_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        })
        .create_view(&wgpu::TextureViewDescriptor::default())
}

/// # Safety
/// `layer` is a live `CAMetalLayer *`.
#[cfg(target_vendor = "apple")]
unsafe fn layer_surface(instance: &wgpu::Instance, layer: *mut c_void) -> Result<wgpu::Surface<'static>, String> {
    unsafe { instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::CoreAnimationLayer(layer)) }.map_err(|e| format!("CAMetalLayer surface: {e}"))
}

#[cfg(not(target_vendor = "apple"))]
unsafe fn layer_surface(_: &wgpu::Instance, _: *mut c_void) -> Result<wgpu::Surface<'static>, String> {
    Err("a CAMetalLayer needs an Apple target; pass NULL for offscreen rendering".into())
}


// MARK: context

#[derive(Clone, Copy)]
struct Pan {
    start: (f32, f32),
    origin: (f32, f32),
}

/// Where the model came from (for `uac_model_state`).
#[derive(Clone, Debug, PartialEq)]
enum Source {
    Preview,
    /// File name of the snapshot.
    Snapshot(String),
    Engine { addr: String, writes: bool },
}

struct PluginRequest {
    strip_id: CString,
    strip_title: CString,
    slot_path: CString,
    effect_name: CString,
    slot: i32,
}

impl PluginRequest {
    fn to_c(&self) -> UacPluginRequest {
        UacPluginRequest {
            strip_id: self.strip_id.as_ptr(),
            strip_title: self.strip_title.as_ptr(),
            slot_path: self.slot_path.as_ptr(),
            effect_name: self.effect_name.as_ptr(),
            slot: self.slot,
        }
    }
}

/// Opaque to C.
pub struct UacContext {
    assets: Box<Assets>,
    model: Box<dyn MixerModel>,
    source: Source,
    gpu: Gpu,
    renderer: gfx::Renderer,
    /// Rgba8 renderer for `uac_write_png` when the surface format differs.
    png_renderer: Option<gfx::Renderer>,
    list: DrawList,
    /// In Console points (not offset by the view).
    hits: Vec<Hit>,
    scrolls: Vec<ScrollRegion>,
    content: (f32, f32),
    interaction: Interaction,
    pan: Option<Pan>,
    /// A tap-kind control pressed by the current pointer: it fires on release inside it (touch
    /// convention), so a long press can still become a secondary click without opening it.
    pending_tap: Option<Hit>,
    view: View,
    /// The mixer's UI-only state: navigator toggles, browser, popovers, pages, compact, window.
    mixer: ViewState,
    /// Console window size pinned by the host (`uac_set_window_size`); None follows the view.
    window: Option<(f32, f32)>,
    /// Engine meter generation drawn by the last frame.
    meter_generation: u64,
    last_present: Option<Instant>,
    plugin_requests: VecDeque<PluginRequest>,
    /// Keeps the strings of the request last handed to C alive.
    taken_request: Option<PluginRequest>,
    status: CString,
    /// Keeps `uac_menu_bar_json` / `uac_take_command` strings alive.
    menu_json: CString,
    taken_command: CString,
    dirty: bool,
    stats: UacStats,
    adapter_name: CString,
}

impl UacContext {
    fn new(resources: Option<PathBuf>, layer: *mut c_void, width: u32, height: u32, scale: f32) -> Result<Box<Self>, String> {
        let resources = match resources {
            Some(p) => Resources::at(&p).ok_or_else(|| format!("{}: not a UAD Console Resources folder (no appearance/Appearance.json)", p.display()))?,
            None => Resources::locate().ok_or("UAD Console Resources not found; pass a path")?,
        };
        let assets = Box::new(Assets::load(resources).map_err(|e| format!("appearance: {e}"))?);
        // SAFETY: forwarded from uac_create's contract.
        let gpu = unsafe { Gpu::new(layer, width, height) }?;
        let renderer = gfx::Renderer::new(&gpu.hl.device, &gpu.hl.queue, gpu.format, assets.images.clone());
        let adapter_name = cstring(format!("{} ({:?})", gpu.hl.adapter_info.name, gpu.hl.adapter_info.backend));
        let mut ctx = Box::new(Self {
            assets,
            model: Box::new(PreviewMixer::default()),
            source: Source::Preview,
            gpu,
            renderer,
            png_renderer: None,
            list: DrawList::default(),
            hits: Vec::new(),
            scrolls: Vec::new(),
            content: (0.0, 0.0),
            interaction: Interaction::default(),
            pan: None,
            pending_tap: None,
            view: View::new(width, height, scale),
            mixer: ViewState::default(),
            window: None,
            meter_generation: 0,
            last_present: None,
            plugin_requests: VecDeque::new(),
            taken_request: None,
            status: CString::default(),
            menu_json: CString::default(),
            taken_command: CString::default(),
            dirty: true,
            stats: UacStats::default(),
            adapter_name,
        });
        ctx.build();
        Ok(ctx)
    }

    fn engine(&self) -> Option<&EngineMixer> {
        self.model.as_any()?.downcast_ref()
    }

    /// The Console window the layout follows: the pinned size, else the view in points.
    fn console_window(&self) -> Option<(f32, f32)> {
        self.window.or_else(|| {
            let (w, h) = self.view.size_pt();
            (w >= 1.0 && h >= 1.0).then_some((w, h))
        })
    }

    /// Rebuilds the draw list and hit regions from the model, then applies the view origin.
    fn build(&mut self) {
        let t = Instant::now();
        self.mixer.window = self.console_window();
        let mut ui = Ui::new(&self.assets);
        let layout = build_window_with(&mut ui, self.model.as_ref(), &self.mixer);
        self.content = layout.size();
        self.hits = std::mem::take(&mut ui.hits);
        self.scrolls = std::mem::take(&mut ui.scrolls);
        self.list = std::mem::take(&mut ui.list);
        self.view.clamp(self.content);
        offset_list(&mut self.list, -self.view.origin.0, -self.view.origin.1);
        self.stats.build_ms = t.elapsed().as_secs_f64() * 1e3;
        self.stats.draws = self.list.len() as u32;
        self.stats.hits = self.hits.len() as u32;
    }

    /// Applies queued engine updates (`MixerModel::pump`); true when a frame is due: the model,
    /// view or input changed, or live meters moved (at most every `METER_PERIOD`).
    fn poll(&mut self) -> bool {
        if self.model.pump() {
            self.dirty = true;
        }
        if !self.dirty
            && let Some(generation) = self.engine().map(EngineMixer::meter_generation)
            && generation != self.meter_generation
            && self.last_present.is_none_or(|t| t.elapsed() >= METER_PERIOD)
        {
            self.dirty = true;
        }
        self.dirty
    }

    fn frame(&mut self) -> i32 {
        let (w, h) = (self.view.width_px, self.view.height_px);
        if w == 0 || h == 0 {
            return UAC_SKIPPED;
        }
        self.model.pump();
        if let Some(generation) = self.engine().map(EngineMixer::meter_generation) {
            self.meter_generation = generation;
        }
        self.build();
        let scale = self.view.px_per_pt(self.content);
        let t = Instant::now();
        let Gpu { hl, target, .. } = &mut self.gpu;
        match target {
            Target::Surface { surface, config } => {
                let frame = match surface.get_current_texture() {
                    wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
                    wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => return UAC_SKIPPED,
                    wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                        surface.configure(&hl.device, config);
                        return UAC_SKIPPED;
                    }
                    wgpu::CurrentSurfaceTexture::Validation => {
                        set_error("surface texture: validation error");
                        return UAC_ERR_SURFACE;
                    }
                };
                let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
                let size = (frame.texture.width(), frame.texture.height());
                self.renderer.render(&hl.device, &hl.queue, &self.list, &view, size, scale, CLEAR);
                hl.queue.present(frame);
            }
            Target::Offscreen { view } => {
                self.renderer.render(&hl.device, &hl.queue, &self.list, view, (w, h), scale, CLEAR);
            }
        }
        let s = self.renderer.stats;
        self.stats.render_ms = t.elapsed().as_secs_f64() * 1e3;
        self.stats.frames += 1;
        self.stats.quads = s.quads as u32;
        self.stats.batches = s.batches as u32;
        self.stats.textures = s.textures as u32;
        self.stats.glyphs = s.glyphs as u32;
        self.stats.texture_bytes = s.texture_bytes as u64;
        self.last_present = Some(Instant::now());
        self.dirty = false;
        UAC_OK
    }

    fn resize(&mut self, width: u32, height: u32, scale: f32) {
        let zoom = self.view.zoom;
        let origin = self.view.origin;
        self.view = View::new(width, height, scale);
        self.view.zoom = zoom;
        self.view.origin = origin;
        self.view.clamp(self.content);
        self.gpu.resize(width, height);
        self.dirty = true;
    }

    // MARK: model

    fn set_model(&mut self, model: Box<dyn MixerModel>, source: Source) {
        self.model = model;
        self.source = source;
        self.interaction = Interaction::default();
        self.pan = None;
        self.pending_tap = None;
        // Layout preferences stay; state that points into the old model goes.
        self.mixer.popover = None;
        self.mixer.text_edit = None;
        self.mixer.sheet = None;
        self.mixer.first_strip = 0;
        self.mixer.browser.selected = None;
        self.plugin_requests.clear();
        self.meter_generation = 0;
        self.build();
        self.dirty = true;
    }

    fn connect_engine(&mut self, host: &str, port: u16, writes: bool) {
        let endpoint = Endpoint::new(host, if port == 0 { ua_engine::DEFAULT_PORT } else { port });
        let addr = endpoint.to_string();
        // Read-only unless `writes`: the connection refuses set/post and the writer gate stays
        // closed, so controls move locally only.
        let model = EngineMixer::connect(endpoint, writes);
        self.set_model(Box::new(model), Source::Engine { addr, writes });
    }

    fn use_snapshot(&mut self, path: &Path) -> Result<(), String> {
        let model = EngineMixer::from_snapshot(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string());
        self.set_model(Box::new(model), Source::Snapshot(name));
        Ok(())
    }

    fn model_state(&self) -> (i32, String) {
        let Some(engine) = self.engine() else { return (UAC_MODEL_PREVIEW, "Preview".into()) };
        let device = engine.store().device_name().filter(|s| !s.is_empty()).unwrap_or("Engine").to_owned();
        match &self.source {
            Source::Preview => (UAC_MODEL_PREVIEW, "Preview".into()),
            Source::Snapshot(name) => (UAC_MODEL_SNAPSHOT, format!("{device} · snapshot ({name})")),
            Source::Engine { addr, writes } => {
                let w = if *writes { " · writes ON" } else { " · read-only" };
                match engine.store().status() {
                    Status::Live => (UAC_MODEL_LIVE, format!("{device} · live{w}")),
                    Status::Loading => (UAC_MODEL_LOADING, format!("Loading {addr}…")),
                    Status::Empty | Status::Connecting(_) => (UAC_MODEL_CONNECTING, format!("Connecting {addr}…")),
                    Status::Reconnecting(s) => (UAC_MODEL_RECONNECTING, format!("Reconnecting {addr} ({s})…")),
                    Status::Failed(s) => (UAC_MODEL_FAILED, format!("Failed {addr}: {s}")),
                    Status::Offline(s) => (UAC_MODEL_OFFLINE, format!("Offline ({s})")),
                }
            }
        }
    }

    // MARK: input

    /// Routes actions through the mixer's UI state (navigator, browser, popovers, modifiers),
    /// then to the model; insert taps that reach the model become plug-in requests.
    fn apply(&mut self, actions: Vec<Action>) {
        if actions.is_empty() {
            return;
        }
        for a in actions {
            for effect in self.mixer.route(a, self.model.as_ref()) {
                match effect {
                    Effect::Model(Action::Tap(ControlId { strip, control: Control::Insert(slot) })) => self.request_plugin(&strip, slot),
                    Effect::Model(a) => self.model.apply(&a),
                    Effect::SetText(id, text) => self.model.set_text(&id, &text),
                }
            }
        }
        self.dirty = true;
    }

    fn find_strip(&self, id: &str) -> Option<StripInfo> {
        self.model.inputs().into_iter().chain(self.model.auxes()).find(|s| &*s.id == id)
    }

    /// Queues a plug-in window request for a loaded slot (empty slots have no window).
    fn request_plugin(&mut self, strip_id: &Arc<str>, slot: u8) {
        let Some(strip) = self.find_strip(strip_id) else { return };
        let state = if i32::from(slot) == UAC_UNISON_SLOT {
            self.model.unison(&strip)
        } else {
            match self.model.inserts(&strip).get(usize::from(slot)) {
                Some(s) => s.clone(),
                None => return,
            }
        };
        let InsertState::Loaded { name, .. } = state else { return };
        let id = ControlId { strip: strip.id.clone(), control: Control::Insert(slot) };
        let slot_path = self.engine().and_then(|e| e.insert_slot(&id)).map(|p| p.base).unwrap_or_default();
        if self.plugin_requests.len() >= MAX_PLUGIN_REQUESTS {
            self.plugin_requests.pop_front();
        }
        self.plugin_requests.push_back(PluginRequest {
            strip_id: cstring(&*strip.id),
            strip_title: cstring(strip.title.clone()),
            slot_path: cstring(slot_path),
            effect_name: cstring(name),
            slot: i32::from(slot),
        });
    }

    /// Returns 1 when the event lands on (or drives) a control, 0 when it pans or misses.
    fn pointer(&mut self, kind: i32, x: f32, y: f32, flags: u32) -> i32 {
        if matches!(kind, UAC_POINTER_DOWN | UAC_POINTER_SECONDARY) && self.dirty {
            self.build(); // hit values must match what the model holds now
        }
        let (cx, cy) = self.view.console_point(self.content, x, y);
        let on_control = self.hits.iter().any(|h| h.rect.contains(cx, cy));
        let actions = match kind {
            UAC_POINTER_DOWN => {
                self.pan = None;
                self.pending_tap = None;
                let actions = self.interaction.pointer_down(&self.hits, cx, cy, flags & UAC_POINTER_DOUBLE != 0);
                if let [Action::Tap(_)] = actions.as_slice() {
                    // Taps (inserts, menu items, browser rows) fire on release.
                    self.pending_tap = self.hits.iter().rev().find(|h| h.rect.contains(cx, cy)).cloned();
                    Vec::new()
                } else {
                    if !on_control {
                        self.pan = Some(Pan { start: (x, y), origin: self.view.origin });
                    }
                    actions
                }
            }
            UAC_POINTER_MOVE => match self.pan {
                Some(p) => {
                    let z = self.view.zoom(self.content);
                    let before = self.view.origin;
                    self.view.origin = (p.origin.0 - (x - p.start.0) / z, p.origin.1 - (y - p.start.1) / z);
                    self.view.clamp(self.content);
                    self.dirty |= self.view.origin != before;
                    return 0;
                }
                None => {
                    if !self.interaction.is_dragging() && self.mixer.menu.is_some() {
                        self.dirty |= self.mixer.menu_hover(&self.hits, cx, cy);
                    }
                    self.interaction.pointer_move(cx, cy, flags & UAC_POINTER_FINE != 0)
                }
            },
            UAC_POINTER_HOVER => {
                let changed = self.mixer.menu.is_some() && self.mixer.menu_hover(&self.hits, cx, cy);
                self.dirty |= changed;
                return i32::from(changed);
            }
            UAC_POINTER_UP => {
                self.pan = None;
                let mut actions = self.interaction.pointer_up();
                if let Some(tap) = self.pending_tap.take()
                    && tap.rect.contains(cx, cy)
                {
                    actions.push(Action::Tap(tap.id));
                }
                actions
            }
            UAC_POINTER_CANCEL => {
                self.pan = None;
                self.pending_tap = None;
                self.interaction.pointer_up()
            }
            UAC_POINTER_SECONDARY => {
                self.pan = None;
                self.pending_tap = None;
                let ended = self.interaction.pointer_up();
                self.apply(ended);
                let handled = self.mixer.context_click(&self.hits, cx, cy);
                self.dirty |= handled;
                return i32::from(handled);
            }
            other => {
                set_error(format!("unknown pointer kind {other}"));
                return UAC_ERR_ARG;
            }
        };
        let acted = !actions.is_empty();
        self.apply(actions);
        i32::from(on_control || self.interaction.is_dragging() || self.pending_tap.is_some() || acted)
    }

    /// Wheel / trackpad scroll in view points (AppKit's `scrollingDelta`: positive reveals
    /// content above / to the left). Scrolls the browser list or the cells column under the
    /// point (returns 1), else pans a zoomed view (returns 0).
    fn scroll(&mut self, x: f32, y: f32, dx: f32, dy: f32) -> i32 {
        if self.dirty {
            self.build();
        }
        let (cx, cy) = self.view.console_point(self.content, x, y);
        let z = self.view.zoom(self.content);
        if dy != 0.0 && !self.interaction.is_dragging() && self.mixer.wheel(&self.scrolls, cx, cy, -dy / z) {
            self.dirty = true;
            return 1;
        }
        let before = self.view.origin;
        self.view.origin = (before.0 - dx / z, before.1 - dy / z);
        self.view.clamp(self.content);
        self.dirty |= self.view.origin != before;
        0
    }

    /// Returns 1 if the key was consumed.
    fn key(&mut self, key: i32) -> i32 {
        let consumed = match key {
            UAC_KEY_ESCAPE => {
                let open = self.mixer.menu.is_some() || self.mixer.popover.is_some() || self.mixer.sheet.is_some() || self.mixer.modifier.is_some() || self.mixer.editing();
                if open {
                    self.mixer.key_escape();
                }
                open
            }
            UAC_KEY_ENTER if self.mixer.editing() => {
                self.mixer.key_enter(self.model.as_mut());
                true
            }
            UAC_KEY_BACKSPACE if self.mixer.editing() => {
                self.mixer.key_backspace();
                true
            }
            UAC_KEY_ENTER | UAC_KEY_BACKSPACE => false,
            other => {
                set_error(format!("unknown key {other}"));
                return UAC_ERR_ARG;
            }
        };
        self.dirty |= consumed;
        i32::from(consumed)
    }

    fn key_text(&mut self, text: &str) -> i32 {
        if !self.mixer.editing() || text.chars().any(char::is_control) {
            return 0;
        }
        self.mixer.key_text(text);
        self.dirty = true;
        1
    }

    fn set_zoom(&mut self, zoom: f32, ax: f32, ay: f32) {
        let anchor = self.view.console_point(self.content, ax, ay);
        let fit = self.view.fit(self.content);
        self.view.zoom = if !zoom.is_finite() || zoom <= 0.0 {
            None
        } else {
            let z = zoom.clamp(fit, MAX_ZOOM.max(fit));
            ((z - fit).abs() > fit * 1e-3).then_some(z)
        };
        let z = self.view.zoom(self.content);
        self.view.origin = (anchor.0 - ax / z, anchor.1 - ay / z);
        self.view.clamp(self.content);
        self.dirty = true;
    }

    fn write_png(&mut self, path: &Path) -> Result<(), String> {
        self.build();
        let scale = self.view.px_per_pt(self.content);
        let size_pt = (self.view.width_px as f32 / scale, self.view.height_px as f32 / scale);
        let Self { gpu, renderer, png_renderer, assets, list, .. } = self;
        let r = if gpu.format == OFFSCREEN_FORMAT {
            renderer
        } else {
            png_renderer.get_or_insert_with(|| gfx::Renderer::new(&gpu.hl.device, &gpu.hl.queue, OFFSCREEN_FORMAT, assets.images.clone()))
        };
        gpu.hl.render_png(r, list, size_pt, scale, path).map_err(|e| format!("{}: {e}", path.display()))
    }

    fn fill_stats(&self) -> UacStats {
        UacStats {
            content_width_pt: self.content.0,
            content_height_pt: self.content.1,
            zoom: self.view.zoom(self.content),
            origin_x_pt: self.view.origin.0,
            origin_y_pt: self.view.origin.1,
            ..self.stats
        }
    }
}

/// # Safety
/// `ctx` is null or a live pointer from `uac_create`.
unsafe fn ctx_mut<'a>(ctx: *mut UacContext) -> Option<&'a mut UacContext> {
    if ctx.is_null() {
        set_error("null context");
        return None;
    }
    Some(unsafe { &mut *ctx })
}

/// Runs `f` on a live context; `UAC_ERR_NULL` for NULL, `UAC_ERR_PANIC` if it panics.
///
/// # Safety
/// `ctx` is null or a live context.
unsafe fn with_ctx(ctx: *mut UacContext, f: impl FnOnce(&mut UacContext) -> i32) -> i32 {
    guard(UAC_ERR_PANIC, || match unsafe { ctx_mut(ctx) } {
        Some(c) => f(c),
        None => UAC_ERR_NULL,
    })
}

// MARK: C API

/// Library version, static NUL-terminated string.
#[unsafe(no_mangle)]
pub extern "C" fn uac_version() -> *const c_char {
    concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr().cast()
}

/// Last error on this thread ("" if none). Valid until the next failing call on this thread.
#[unsafe(no_mangle)]
pub extern "C" fn uac_last_error() -> *const c_char {
    LAST_ERROR.with(|e| e.borrow().as_ptr())
}

/// True when `path` is a UAD Console `Resources` folder (has appearance/Appearance.json).
///
/// # Safety
/// `path` is null or a valid NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uac_resources_valid(path: *const c_char) -> bool {
    guard(false, || unsafe { path_arg(path) }.is_some_and(|p| Resources::at(&p).is_some()))
}

/// Creates a context rendering into `metal_layer` (a `CAMetalLayer *`), or offscreen when it
/// is NULL. `resources_path` NULL searches the default locations (macOS app bundle, …).
/// `width_px`/`height_px` are the drawable size, `scale` pixels per view point.
/// Returns NULL on failure (see `uac_last_error`).
///
/// # Safety
/// `resources_path` is null or a valid NUL-terminated string. `metal_layer` is null or a
/// `CAMetalLayer *` that stays alive until `uac_destroy`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uac_create(resources_path: *const c_char, metal_layer: *mut c_void, width_px: u32, height_px: u32, scale: f32) -> *mut UacContext {
    guard(std::ptr::null_mut(), || {
        let path = unsafe { path_arg(resources_path) };
        match UacContext::new(path, metal_layer, width_px, height_px, scale) {
            Ok(ctx) => Box::into_raw(ctx),
            Err(e) => {
                set_error(e);
                std::ptr::null_mut()
            }
        }
    })
}

/// New drawable size in pixels and pixels per view point. Unless a window size is pinned
/// (`uac_set_window_size`), the Console window follows the view: the full layout reflows.
///
/// # Safety
/// `ctx` is null or a live context.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uac_resize(ctx: *mut UacContext, width_px: u32, height_px: u32, scale: f32) {
    guard((), || {
        if let Some(c) = unsafe { ctx_mut(ctx) } {
            c.resize(width_px, height_px, scale);
        }
    })
}

/// Pins the Console window size in Console points (e.g. 1920 × 1050, Console's default);
/// `width_pt` or `height_pt` <= 0 returns to following the view (the default). The layout is
/// never smaller than its minimum size. The view fits the window unless zoomed.
///
/// # Safety
/// `ctx` is null or a live context.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uac_set_window_size(ctx: *mut UacContext, width_pt: f32, height_pt: f32) {
    guard((), || {
        if let Some(c) = unsafe { ctx_mut(ctx) } {
            let window = (width_pt.is_finite() && height_pt.is_finite() && width_pt > 0.0 && height_pt > 0.0).then_some((width_pt, height_pt));
            if c.window != window {
                c.window = window;
                c.build();
                c.dirty = true;
            }
        }
    })
}

/// Applies queued engine updates (call it every display refresh) and returns true when a frame
/// is due: the model, view or input changed, or live meters moved (redrawn at ≤ 30 Hz).
///
/// # Safety
/// `ctx` is null or a live context.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uac_needs_frame(ctx: *mut UacContext) -> bool {
    guard(false, || unsafe { ctx_mut(ctx) }.is_some_and(UacContext::poll))
}

/// Forces the next `uac_needs_frame` to return true (layer contents lost, benchmarks).
///
/// # Safety
/// `ctx` is null or a live context.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uac_invalidate(ctx: *mut UacContext) {
    guard((), || {
        if let Some(c) = unsafe { ctx_mut(ctx) } {
            c.dirty = true;
        }
    })
}

/// Applies queued engine updates, builds the draw list from the model and renders one frame
/// (presenting it on a layer). Returns UAC_OK, UAC_SKIPPED, or a negative error.
///
/// # Safety
/// `ctx` is null or a live context.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uac_frame(ctx: *mut UacContext) -> i32 {
    unsafe { with_ctx(ctx, UacContext::frame) }
}

/// A pointer event in view points: DOWN/MOVE/UP/CANCEL drive rack's `Interaction` (drags,
/// toggles, taps) through the mixer's `ViewState` (navigator, browser, popovers, modifiers);
/// SECONDARY opens context menus. Drags off controls pan a zoomed view. Returns 1 if it hit or
/// drove a control, 0 if it panned or missed, or a negative error.
///
/// # Safety
/// `ctx` is null or a live context.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uac_pointer(ctx: *mut UacContext, kind: i32, x_pt: f32, y_pt: f32, flags: u32) -> i32 {
    unsafe { with_ctx(ctx, |c| c.pointer(kind, x_pt, y_pt, flags)) }
}

/// True while a pointer drag is driving a control (fader, knob, pan): hosts use it to keep a
/// long-press recognizer from turning a held drag into a context click.
///
/// # Safety
/// `ctx` is null or a live context.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uac_dragging(ctx: *mut UacContext) -> bool {
    guard(false, || unsafe { ctx_mut(ctx) }.is_some_and(|c| c.interaction.is_dragging()))
}

/// Wheel / trackpad scroll at a point, deltas in view points (AppKit `scrollingDelta`
/// convention: positive reveals content above / to the left). Scrolls the plug-in browser list
/// or the cells column under the point (returns 1), else pans a zoomed view (returns 0).
///
/// # Safety
/// `ctx` is null or a live context.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uac_scroll(ctx: *mut UacContext, x_pt: f32, y_pt: f32, dx_pt: f32, dy_pt: f32) -> i32 {
    unsafe { with_ctx(ctx, |c| c.scroll(x_pt, y_pt, dx_pt, dy_pt)) }
}

/// True while a text field (channel rename, tempo, browser search) has keyboard focus.
///
/// # Safety
/// `ctx` is null or a live context.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uac_text_editing(ctx: *mut UacContext) -> bool {
    guard(false, || unsafe { ctx_mut(ctx) }.is_some_and(|c| c.mixer.editing()))
}

/// Typed text (UTF-8) for the focused field. Returns 1 if consumed, 0 if no field has focus.
///
/// # Safety
/// `ctx` is null or a live context; `utf8` is null or a valid NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uac_key_text(ctx: *mut UacContext, utf8: *const c_char) -> i32 {
    let text = unsafe { str_arg(utf8) }.unwrap_or_default();
    unsafe { with_ctx(ctx, |c| c.key_text(&text)) }
}

/// UAC_KEY_ENTER / BACKSPACE (focused field) or ESCAPE (closes the field, popover, sheet or
/// modifier). Returns 1 if consumed.
///
/// # Safety
/// `ctx` is null or a live context.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uac_key(ctx: *mut UacContext, key: i32) -> i32 {
    unsafe { with_ctx(ctx, |c| c.key(key)) }
}

/// Zoom in view points per Console point, keeping the Console point under the anchor (view
/// points) fixed. `zoom <= 0` returns to fit-to-view. Clamped to [fit, 4].
///
/// # Safety
/// `ctx` is null or a live context.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uac_set_zoom(ctx: *mut UacContext, zoom: f32, anchor_x_pt: f32, anchor_y_pt: f32) {
    guard((), || {
        if let Some(c) = unsafe { ctx_mut(ctx) } {
            c.set_zoom(zoom, anchor_x_pt, anchor_y_pt);
        }
    })
}

/// `compact` hides the plug-in browser and navigator and packs aux/monitor after the strips.
///
/// # Safety
/// `ctx` is null or a live context.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uac_set_compact(ctx: *mut UacContext, compact: bool) {
    guard((), || {
        if let Some(c) = unsafe { ctx_mut(ctx) }
            && c.mixer.compact != compact
        {
            c.mixer.compact = compact;
            c.build();
            c.dirty = true;
        }
    })
}

/// Replaces the model with the offline preview mixer (6 inputs, 2 aux; edits stay local).
/// Closes an engine connection.
///
/// # Safety
/// `ctx` is null or a live context.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uac_set_model_preview(ctx: *mut UacContext) {
    guard((), || {
        if let Some(c) = unsafe { ctx_mut(ctx) } {
            c.set_model(Box::new(PreviewMixer::default()), Source::Preview);
        }
    })
}

/// Connects to a UA Mixer Engine (UA's engine or the open-apollo daemon) at `host:port`
/// (`port` 0 = 4710) and shows it live: `engine_model::EngineMixer` over `ua-engine`, plus the
/// read-only UA Mixer Helper on `host:4720` when `port` is 4710. Replaces the current model
/// (closing an earlier connection). The connection runs in the background; updates are applied
/// by `uac_needs_frame`/`uac_frame`, and `uac_model_state` reports progress.
///
/// Read-only unless `writes`: the connection refuses `set`/`post` and the writer gate stays
/// closed, so controls move locally and nothing is sent. Pass `writes` only after the user
/// explicitly asked for it.
///
/// # Safety
/// `ctx` is null or a live context; `host` is null or a valid NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uac_connect_engine(ctx: *mut UacContext, host: *const c_char, port: u16, writes: bool) -> i32 {
    let host = unsafe { str_arg(host) };
    unsafe {
        with_ctx(ctx, |c| {
            let Some(host) = host.as_deref().map(str::trim).filter(|h| !h.is_empty()) else {
                set_error("uac_connect_engine: empty host");
                return UAC_ERR_ARG;
            };
            c.connect_engine(host, port, writes);
            UAC_OK
        })
    }
}

/// `uac_connect_engine` with flags (`UAC_ENGINE_WRITES`); kept for earlier hosts.
///
/// # Safety
/// As `uac_connect_engine`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uac_set_model_engine(ctx: *mut UacContext, host: *const c_char, port: u16, flags: u32) -> i32 {
    unsafe { uac_connect_engine(ctx, host, port, flags & UAC_ENGINE_WRITES != 0) }
}

/// Shows an engine snapshot offline (a ConsoleLab / ua-engine snapshot JSON, a raw
/// `get /?recursive=1` reply or an open-apollo device map). Edits stay local. Closes an engine
/// connection.
///
/// # Safety
/// `ctx` is null or a live context; `path` is null or a valid NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uac_use_snapshot(ctx: *mut UacContext, path: *const c_char) -> i32 {
    let path = unsafe { path_arg(path) };
    unsafe {
        with_ctx(ctx, |c| {
            let Some(path) = path else {
                set_error("uac_use_snapshot: null path");
                return UAC_ERR_ARG;
            };
            match c.use_snapshot(&path) {
                Ok(()) => UAC_OK,
                Err(e) => {
                    set_error(e);
                    UAC_ERR_IO
                }
            }
        })
    }
}

/// The model's state: UAC_MODEL_PREVIEW, SNAPSHOT, or an engine connection's CONNECTING,
/// LOADING, LIVE, RECONNECTING, FAILED or OFFLINE. Negative on error.
///
/// # Safety
/// `ctx` is null or a live context.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uac_model_state(ctx: *mut UacContext) -> i32 {
    unsafe { with_ctx(ctx, |c| c.model_state().0) }
}

/// One line for a status bar, e.g. "Apollo x4 · live · read-only". Owned by the context, valid
/// until the next call on it.
///
/// # Safety
/// `ctx` is null or a live context.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uac_model_status(ctx: *mut UacContext) -> *const c_char {
    guard(c"".as_ptr(), || match unsafe { ctx_mut(ctx) } {
        Some(c) => {
            c.status = cstring(c.model_state().1);
            c.status.as_ptr()
        }
        None => c"".as_ptr(),
    })
}

/// Takes the oldest pending plug-in window request (a tap on a loaded insert or Unison slot,
/// or "Open" in its context menu) into `out`. Returns true if there was one. The strings stay
/// valid until the next call or `uac_destroy`. The renderer has no plug-in windows of its own:
/// the host shows one (e.g. from the slot's engine path).
///
/// # Safety
/// `ctx` is null or a live context; `out` is null or points to a `UacPluginRequest`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uac_take_plugin_request(ctx: *mut UacContext, out: *mut UacPluginRequest) -> bool {
    guard(false, || {
        let Some(c) = (unsafe { ctx_mut(ctx) }) else { return false };
        if out.is_null() {
            set_error("null plug-in request pointer");
            return false;
        }
        let Some(req) = c.plugin_requests.pop_front() else { return false };
        unsafe { out.write(req.to_c()) };
        c.taken_request = Some(req);
        true
    })
}

/// Console's menu bar as JSON for native menus (`rack::menu::menu_bar_json`): File, Edit,
/// Navigation, View, Window, Help with live enable / tick states, plus the macOS application
/// menu's extra items under "app" when `mac`. Valid until the next call or `uac_destroy`;
/// rebuild native menus when they open (`menuNeedsUpdate`). NULL on error.
///
/// # Safety
/// `ctx` is null or a live context.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uac_menu_bar_json(ctx: *mut UacContext, mac: bool) -> *const c_char {
    guard(std::ptr::null(), || {
        let Some(c) = (unsafe { ctx_mut(ctx) }) else { return std::ptr::null() };
        c.menu_json = cstring(rack::menu::menu_bar_json(c.model.as_ref(), &c.mixer, mac));
        c.menu_json.as_ptr()
    })
}

/// Runs a Console menu command by id (a menu-bar item's "command", e.g. `show_browser`,
/// `undo`). Returns 1 when it ran, 0 when it is unknown or disabled now. Shell-level commands
/// (`quit`, `toggle_full_screen`, `open_session`, …) come back through `uac_take_command`.
///
/// # Safety
/// `ctx` is null or a live context; `id` is null or a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uac_run_command(ctx: *mut UacContext, id: *const c_char) -> i32 {
    let id = unsafe { str_arg(id) };
    unsafe {
        with_ctx(ctx, |c| {
            let Some(id) = id else {
                set_error("null or empty command id");
                return UAC_ERR_ARG;
            };
            let ran = c.mixer.run_command_id(&id, c.model.as_mut());
            c.dirty |= ran;
            i32::from(ran)
        })
    }
}

/// Takes the oldest command the in-renderer menus or `uac_run_command` left for the host
/// (`quit`, `toggle_full_screen`, `open_documentation_url_app`, the session commands
/// `select_new_session_panel` / `open_session` / `save_bookmark` / `save_as`, …). NULL when
/// there is none; the string stays valid until the next call or `uac_destroy`.
///
/// # Safety
/// `ctx` is null or a live context.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uac_take_command(ctx: *mut UacContext) -> *const c_char {
    guard(std::ptr::null(), || {
        let Some(c) = (unsafe { ctx_mut(ctx) }) else { return std::ptr::null() };
        if c.mixer.commands.is_empty() {
            return std::ptr::null();
        }
        let cmd = c.mixer.commands.remove(0);
        c.taken_command = cstring(cmd.id());
        c.taken_command.as_ptr()
    })
}

/// Draws Windows Console's menu bar in the renderer's title bar (hosts without a native menu
/// bar: iPad without a keyboard menu, Linux). Off by default.
///
/// # Safety
/// `ctx` is null or a live context.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uac_set_menu_bar(ctx: *mut UacContext, on: bool) {
    unsafe {
        with_ctx(ctx, |c| {
            c.mixer.menu_bar = on;
            c.dirty = true;
            UAC_OK
        });
    }
}

/// Fills `out` with the latest stats.
///
/// # Safety
/// `ctx` is null or a live context; `out` is null or points to a `UacStats`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uac_get_stats(ctx: *mut UacContext, out: *mut UacStats) -> i32 {
    unsafe {
        with_ctx(ctx, |c| {
            if out.is_null() {
                set_error("null stats pointer");
                return UAC_ERR_NULL;
            }
            out.write(c.fill_stats());
            UAC_OK
        })
    }
}

/// GPU adapter name and backend, e.g. "Apple M2 (Metal)". Owned by the context.
///
/// # Safety
/// `ctx` is null or a live context.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uac_adapter_name(ctx: *mut UacContext) -> *const c_char {
    guard(c"".as_ptr(), || match unsafe { ctx_mut(ctx) } {
        Some(c) => c.adapter_name.as_ptr(),
        None => c"".as_ptr(),
    })
}

/// Renders the current view offscreen into an RGBA PNG at `path`, plus `<name>.draws.json`
/// (what was drawn where, for the alignment tools).
///
/// # Safety
/// `ctx` is null or a live context; `path` is null or a valid NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uac_write_png(ctx: *mut UacContext, path: *const c_char) -> i32 {
    let path = unsafe { path_arg(path) };
    unsafe {
        with_ctx(ctx, |c| {
            let Some(path) = path else {
                set_error("null path");
                return UAC_ERR_NULL;
            };
            match c.write_png(&path) {
                Ok(()) => UAC_OK,
                Err(e) => {
                    set_error(e);
                    UAC_ERR_IO
                }
            }
        })
    }
}

/// Destroys a context (NULL is ignored), closing its engine connection. The CAMetalLayer may
/// be released afterwards.
///
/// # Safety
/// `ctx` is null or a live context, not used again afterwards.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uac_destroy(ctx: *mut UacContext) {
    if !ctx.is_null() {
        guard((), || drop(unsafe { Box::from_raw(ctx) }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ua_engine::{MockEngine, MockOptions, Snapshot};

    #[test]
    fn abi() {
        // Mirrors the C struct in uaconsole.h: u64, f64×2, u32×6, u64, f32×5 (+4 pad).
        assert_eq!(std::mem::size_of::<UacStats>(), 8 + 16 + 24 + 8 + 20 + 4);
        assert_eq!(std::mem::align_of::<UacStats>(), 8);
        assert_eq!(std::mem::offset_of!(UacStats, texture_bytes), 48);
        assert_eq!(std::mem::offset_of!(UacStats, content_width_pt), 56);
        // Four C strings, then int32_t slot (+4 pad on 64-bit).
        assert_eq!(std::mem::offset_of!(UacPluginRequest, effect_name), 3 * std::mem::size_of::<usize>());
        assert_eq!(std::mem::offset_of!(UacPluginRequest, slot), 4 * std::mem::size_of::<usize>());
    }

    #[test]
    fn view_fit_and_clamp() {
        let content = (1920.0, 1050.0);
        // iPad Pro 13" landscape at 2×.
        let mut v = View::new(2752, 2064, 2.0);
        let fit = v.fit(content);
        assert!((fit - 1376.0 / 1920.0).abs() < 1e-5);
        v.clamp(content);
        assert!(v.origin.0.abs() < 0.01, "{:?}", v.origin);
        assert!(v.origin.1 < 0.0, "letterboxed vertically: {:?}", v.origin);
        // Point mapping round-trips through the view.
        let (cx, cy) = v.console_point(content, 688.0, 516.0);
        assert!((cx - 960.0).abs() < 0.01 && (cy - 525.0).abs() < 0.01, "{cx},{cy}");
        // Zoomed in, origin stays inside the window.
        v.zoom = Some(2.0);
        v.origin = (5000.0, -50.0);
        v.clamp(content);
        assert_eq!(v.origin, (1920.0 - 688.0, 0.0));
    }

    #[test]
    fn null_safety() {
        unsafe {
            assert_eq!(uac_frame(std::ptr::null_mut()), UAC_ERR_NULL);
            assert_eq!(uac_pointer(std::ptr::null_mut(), UAC_POINTER_DOWN, 0.0, 0.0, 0), UAC_ERR_NULL);
            assert_eq!(uac_connect_engine(std::ptr::null_mut(), c"127.0.0.1".as_ptr(), 4710, false), UAC_ERR_NULL);
            assert_eq!(uac_model_state(std::ptr::null_mut()), UAC_ERR_NULL);
            assert!(!uac_needs_frame(std::ptr::null_mut()));
            assert!(!uac_take_plugin_request(std::ptr::null_mut(), std::ptr::null_mut()));
            uac_destroy(std::ptr::null_mut());
            assert!(uac_create(c"/nonexistent".as_ptr(), std::ptr::null_mut(), 10, 10, 1.0).is_null());
            let err = CStr::from_ptr(uac_last_error()).to_string_lossy().into_owned();
            assert!(err.contains("not a UAD Console Resources folder"), "{err}");
        }
    }

    /// GPU tests run one at a time: concurrent wgpu instances crash some Vulkan drivers.
    static GPU: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// An offscreen context (holding the GPU test lock), or None (skipped) without installed
    /// Console resources or a GPU.
    unsafe fn offscreen(width_px: u32, height_px: u32, scale: f32) -> Option<(std::sync::MutexGuard<'static, ()>, *mut UacContext)> {
        let lock = GPU.lock().unwrap_or_else(|e| e.into_inner());
        if Resources::locate().is_none() {
            eprintln!("skipping: no Console resources");
            return None;
        }
        let ctx = unsafe { uac_create(std::ptr::null(), std::ptr::null_mut(), width_px, height_px, scale) };
        if ctx.is_null() {
            eprintln!("skipping: {}", unsafe { CStr::from_ptr(uac_last_error()) }.to_string_lossy());
            return None;
        }
        Some((lock, ctx))
    }

    fn stats(ctx: *mut UacContext) -> UacStats {
        let mut s = UacStats::default();
        assert_eq!(unsafe { uac_get_stats(ctx, &mut s) }, UAC_OK);
        s
    }

    /// View points of a Console point (through the context's zoom and origin).
    fn to_view(s: &UacStats, x: f32, y: f32) -> (f32, f32) {
        ((x - s.origin_x_pt) * s.zoom, (y - s.origin_y_pt) * s.zoom)
    }

    fn fixture() -> String {
        format!("{}/../ua-engine/tests/fixtures/apollo-x4-12.0.0.snapshot.json", env!("CARGO_MANIFEST_DIR"))
    }

    /// Offscreen end-to-end: needs installed Console resources and a GPU; skipped otherwise.
    #[test]
    fn offscreen_frame_and_input() {
        unsafe {
            let Some((_gpu, ctx)) = offscreen(1900, 2100, 2.0) else { return };
            uac_set_compact(ctx, true);
            assert!(uac_needs_frame(ctx));
            assert_eq!(uac_frame(ctx), UAC_OK);
            assert!(!uac_needs_frame(ctx));
            let mut s = stats(ctx);
            assert!(s.quads > 500 && s.hits > 50, "{s:?}");
            // Compact width from the 47-pt side section, the strips, the auxes and the monitor.
            assert!((s.content_width_pt - 951.0).abs() < 0.01, "{s:?}");
            // Compact takes its height from the window, which follows the view (950 × 1050 pt).
            assert_eq!(s.content_height_pt, 1050.0, "{s:?}");

            // Tap the first strip's solo button: find it through the hit list.
            let c = &mut *ctx;
            let solo = c.hits.iter().find(|h| &*h.id.strip == "in0" && h.id.control == rack::Control::Solo).cloned().expect("solo hit");
            let (vx, vy) = to_view(&s, solo.rect.x + 2.0, solo.rect.y + 2.0);
            assert_eq!(uac_pointer(ctx, UAC_POINTER_DOWN, vx, vy, 0), 1);
            assert_eq!(uac_pointer(ctx, UAC_POINTER_UP, vx, vy, 0), 1);
            assert!(c.model.main(&c.model.inputs()[0]).solo);
            assert!(uac_needs_frame(ctx));

            // Off the window (letterbox) pans nothing in fit mode and reports a miss.
            assert_eq!(uac_pointer(ctx, UAC_POINTER_DOWN, 1.0, 1.0, 0), 0);
            assert_eq!(uac_pointer(ctx, UAC_POINTER_MOVE, 40.0, 10.0, 0), 0);
            assert_eq!(uac_pointer(ctx, UAC_POINTER_UP, 40.0, 10.0, 0), 0);
            assert_eq!(uac_pointer(ctx, 42, 0.0, 0.0, 0), UAC_ERR_ARG);

            uac_set_zoom(ctx, 2.0, 0.0, 0.0);
            s = stats(ctx);
            assert_eq!(s.zoom, 2.0);
            if let Ok(dir) = std::env::var("UAC_TEST_PNG") {
                assert_eq!(uac_write_png(ctx, CString::new(dir).unwrap().as_ptr()), UAC_OK);
            }
            uac_destroy(ctx);
        }
    }

    /// The full layout follows the view (Console's responsive window) unless a size is pinned.
    #[test]
    fn window_follows_the_view() {
        unsafe {
            let Some((_gpu, ctx)) = offscreen(2940, 2210, 2.0) else { return };
            assert_eq!(uac_frame(ctx), UAC_OK);
            let s = stats(ctx);
            assert_eq!((s.content_width_pt, s.content_height_pt, s.zoom), (1470.0, 1105.0, 1.0), "{s:?}");
            uac_resize(ctx, 3840, 2100, 2.0);
            assert_eq!(uac_frame(ctx), UAC_OK);
            let s = stats(ctx);
            assert_eq!((s.content_width_pt, s.content_height_pt), (1920.0, 1050.0), "{s:?}");
            // Pinned: Console's default window, fit into a smaller view.
            uac_resize(ctx, 2940, 2210, 2.0);
            uac_set_window_size(ctx, 1920.0, 1050.0);
            assert_eq!(uac_frame(ctx), UAC_OK);
            let s = stats(ctx);
            assert_eq!((s.content_width_pt, s.content_height_pt), (1920.0, 1050.0), "{s:?}");
            assert!((s.zoom - 1470.0 / 1920.0).abs() < 1e-4, "{s:?}");
            uac_set_window_size(ctx, 0.0, 0.0);
            assert_eq!(uac_frame(ctx), UAC_OK);
            assert_eq!(stats(ctx).content_width_pt, 1470.0);
            uac_destroy(ctx);
        }
    }

    /// Pointer actions go through `ViewState`: navigator toggles, rename popover + keys, and
    /// insert taps become plug-in requests.
    #[test]
    fn view_state_routing_and_plugin_requests() {
        unsafe {
            let Some((_gpu, ctx)) = offscreen(3840, 2100, 2.0) else { return };
            assert_eq!(uac_frame(ctx), UAC_OK);
            let s = stats(ctx);
            let c = &mut *ctx;
            let tap = |c: &mut UacContext, pick: &dyn Fn(&Hit) -> bool| {
                let hit = c.hits.iter().find(|h| pick(h)).cloned().expect("hit");
                let (x, y) = to_view(&s, hit.rect.x + hit.rect.w / 2.0, hit.rect.y + hit.rect.h / 2.0);
                assert_eq!(c.pointer(UAC_POINTER_DOWN, x, y, 0), 1, "{hit:?}");
                c.pointer(UAC_POINTER_UP, x, y, 0);
                c.frame();
            };

            // Navigator: collapse the INSERTS row (view state, never the model).
            assert!(c.mixer.sections.inserts);
            tap(c, &|h| matches!(h.id.control, Control::Section(rack::surfaces::Section::Inserts)));
            assert!(!c.mixer.sections.inserts);
            tap(c, &|h| matches!(h.id.control, Control::Section(rack::surfaces::Section::Inserts)));
            assert!(c.mixer.sections.inserts);

            // Rename through the popover and the key API.
            tap(c, &|h| &*h.id.strip == "in0" && h.id.control == Control::Name);
            assert!(uac_text_editing(ctx));
            assert_eq!(uac_key(ctx, UAC_KEY_BACKSPACE), 1);
            assert_eq!(uac_key_text(ctx, c"8".as_ptr()), 1);
            assert_eq!(uac_key(ctx, UAC_KEY_ENTER), 1);
            assert!(!uac_text_editing(ctx));
            assert_eq!(c.model.main(&c.model.inputs()[0]).name, "SM78");
            assert_eq!(uac_key(ctx, UAC_KEY_ESCAPE), 0);

            // A loaded insert opens a plug-in request; an empty one does not.
            let mut req = std::mem::zeroed::<UacPluginRequest>();
            assert!(!uac_take_plugin_request(ctx, &mut req));
            c.frame();
            tap(c, &|h| &*h.id.strip == "in0" && h.id.control == Control::Insert(0));
            assert!(uac_take_plugin_request(ctx, &mut req));
            assert_eq!(CStr::from_ptr(req.strip_id).to_str().unwrap(), "in0");
            assert_eq!(req.slot, 0);
            assert!(!CStr::from_ptr(req.effect_name).to_bytes().is_empty());
            assert_eq!(CStr::from_ptr(req.slot_path).to_bytes(), b"");
            assert!(!uac_take_plugin_request(ctx, &mut req));

            // A tap released outside its control does nothing.
            let s = stats(ctx);
            let hit = c.hits.iter().find(|h| &*h.id.strip == "in1" && h.id.control == Control::Insert(0)).cloned().unwrap();
            let (x, y) = to_view(&s, hit.rect.x + 2.0, hit.rect.y + 2.0);
            assert_eq!(uac_pointer(ctx, UAC_POINTER_DOWN, x, y, 0), 1);
            assert!(uac_pointer(ctx, UAC_POINTER_UP, x, y - 200.0, 0) >= 0);
            assert!(!uac_take_plugin_request(ctx, &mut req));

            // A long press (DOWN, the recognizer's CANCEL, SECONDARY) on an insert opens its
            // context menu (a popover) without opening the plug-in; Escape closes it.
            assert_eq!(uac_pointer(ctx, UAC_POINTER_DOWN, x, y, 0), 1);
            assert_eq!(uac_pointer(ctx, UAC_POINTER_CANCEL, x, y, 0), 1);
            assert_eq!(uac_pointer(ctx, UAC_POINTER_SECONDARY, x, y, 0), 1);
            assert!(!uac_take_plugin_request(ctx, &mut req));
            assert!(c.mixer.menu.is_some(), "Console's insert context menu");
            assert_eq!(uac_key(ctx, UAC_KEY_ESCAPE), 1);
            assert!(c.mixer.menu.is_none());
            uac_destroy(ctx);
        }
    }

    #[test]
    fn snapshot_model() {
        unsafe {
            let Some((_gpu, ctx)) = offscreen(2940, 2210, 2.0) else { return };
            assert_eq!(uac_model_state(ctx), UAC_MODEL_PREVIEW);
            assert_eq!(uac_use_snapshot(ctx, c"/nonexistent.json".as_ptr()), UAC_ERR_IO);
            let path = CString::new(fixture()).unwrap();
            assert_eq!(uac_use_snapshot(ctx, path.as_ptr()), UAC_OK);
            assert_eq!(uac_model_state(ctx), UAC_MODEL_SNAPSHOT);
            let status = CStr::from_ptr(uac_model_status(ctx)).to_string_lossy().into_owned();
            assert!(status.contains("snapshot"), "{status}");
            assert!(uac_needs_frame(ctx));
            assert_eq!(uac_frame(ctx), UAC_OK);
            assert!((*ctx).model.inputs().len() >= 4);
            uac_set_model_preview(ctx);
            assert_eq!(uac_model_state(ctx), UAC_MODEL_PREVIEW);
            uac_destroy(ctx);
        }
    }

    /// A live (mock) engine, read-only: the tree arrives through `uac_needs_frame`'s pump,
    /// controls move locally, and nothing is written.
    #[test]
    fn engine_read_only() {
        unsafe {
            let Some((_gpu, ctx)) = offscreen(2940, 2210, 2.0) else { return };
            let tree = Snapshot::load(fixture()).unwrap().tree;
            let server = MockEngine::start(tree, MockOptions::default()).unwrap();
            assert_eq!(uac_connect_engine(ctx, c"".as_ptr(), server.port(), false), UAC_ERR_ARG);
            assert_eq!(uac_connect_engine(ctx, c"127.0.0.1".as_ptr(), server.port(), false), UAC_OK);
            let deadline = Instant::now() + Duration::from_secs(10);
            while uac_model_state(ctx) != UAC_MODEL_LIVE {
                uac_needs_frame(ctx);
                assert!(Instant::now() < deadline, "not live: {}", CStr::from_ptr(uac_model_status(ctx)).to_string_lossy());
                std::thread::sleep(Duration::from_millis(5));
            }
            let status = CStr::from_ptr(uac_model_status(ctx)).to_string_lossy().into_owned();
            assert!(status.contains("live") && status.contains("read-only"), "{status}");
            assert!(uac_needs_frame(ctx));
            assert_eq!(uac_frame(ctx), UAC_OK);
            let s = stats(ctx);
            let c = &mut *ctx;
            assert!(c.model.inputs().len() >= 4);
            assert_eq!((s.content_width_pt, s.content_height_pt), (1470.0, 1105.0));

            // Drag the first input's fader: it moves locally, nothing reaches the engine.
            let fader = c.hits.iter().find(|h| h.id.control == Control::Fader).cloned().expect("fader hit");
            let strip = c.find_strip(&fader.id.strip).expect("fader strip");
            let before = c.model.main(&strip).fader;
            let (x, y) = to_view(&s, fader.rect.x + fader.rect.w / 2.0, fader.rect.y + fader.rect.h / 2.0);
            assert_eq!(uac_pointer(ctx, UAC_POINTER_DOWN, x, y, 0), 1);
            assert!(uac_dragging(ctx));
            assert_eq!(uac_pointer(ctx, UAC_POINTER_MOVE, x, y - 40.0, 0), 1);
            assert_eq!(uac_pointer(ctx, UAC_POINTER_UP, x, y - 40.0, 0), 1);
            assert!(!uac_dragging(ctx));
            uac_needs_frame(ctx);
            assert_ne!(c.model.main(&strip).fader, before);
            std::thread::sleep(Duration::from_millis(100));
            uac_needs_frame(ctx);
            assert!(server.write_commands().is_empty(), "{:?}", server.write_commands());
            uac_destroy(ctx);
        }
    }
}
