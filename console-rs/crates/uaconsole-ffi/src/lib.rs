//! C ABI for native Apple hosts (the iPad app, later a macOS app): houston (skins) +
//! rack (draw list, hits, input, `MixerModel`) + gfx (wgpu) behind an opaque `UacContext`.
//!
//! The C header is `apple/include/uaconsole.h`, written by hand: keep the constants and
//! `UacStats` there in sync with this file (the `abi` test checks the struct layout).
//!
//! Threading: a context is single-threaded. Create, drive and destroy it on one thread
//! (on iOS the main thread, where the CAMetalLayer and the CADisplayLink live).
//!
//! Coordinates: the host speaks *view points* (UIKit points, origin at the layer's top-left).
//! The context maps them to Console window points through a zoom and an origin it owns, so
//! the Console window (1920×1050 pt full, ≈950×1050 pt compact) can be fit, panned and pinched.

use std::cell::RefCell;
use std::ffi::{CStr, CString, c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::time::Instant;

use houston::{Resources, Rgba};
use rack::{Assets, Draw, DrawList, Hit, Interaction, MixerModel, PreviewMixer, Ui, build_window};

// MARK: constants (mirror uaconsole.h)

pub const UAC_OK: i32 = 0;
/// The frame was skipped (surface timed out, was occluded, or had to be reconfigured).
pub const UAC_SKIPPED: i32 = 1;
pub const UAC_ERR_NULL: i32 = -1;
pub const UAC_ERR_SURFACE: i32 = -2;
pub const UAC_ERR_UNSUPPORTED: i32 = -3;
pub const UAC_ERR_IO: i32 = -4;
pub const UAC_ERR_PANIC: i32 = -99;

pub const UAC_POINTER_DOWN: i32 = 0;
pub const UAC_POINTER_MOVE: i32 = 1;
pub const UAC_POINTER_UP: i32 = 2;
pub const UAC_POINTER_CANCEL: i32 = 3;

/// Double tap / double click: resets a dragged control to its default.
pub const UAC_POINTER_DOUBLE: u32 = 1 << 0;
/// Fine adjustment (Console's modifier-drag): motion divided by 10.
pub const UAC_POINTER_FINE: u32 = 1 << 1;

/// Letterbox colour around the Console window.
const CLEAR: Rgba = Rgba::rgb8(24, 25, 27);
const MAX_ZOOM: f32 = 4.0;

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

// MARK: errors

thread_local! {
    static LAST_ERROR: RefCell<CString> = RefCell::new(CString::default());
}

fn set_error(msg: impl Into<String>) {
    let s = msg.into().replace('\0', " ");
    LAST_ERROR.with(|e| *e.borrow_mut() = CString::new(s).unwrap_or_default());
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

/// Opaque to C.
pub struct UacContext {
    assets: Box<Assets>,
    model: Box<dyn MixerModel>,
    gpu: Gpu,
    renderer: gfx::Renderer,
    /// Rgba8 renderer for `uac_write_png` when the surface format differs.
    png_renderer: Option<gfx::Renderer>,
    list: DrawList,
    /// In Console points (not offset by the view).
    hits: Vec<Hit>,
    content: (f32, f32),
    interaction: Interaction,
    pan: Option<Pan>,
    view: View,
    compact: bool,
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
        let adapter_name = CString::new(format!("{} ({:?})", gpu.hl.adapter_info.name, gpu.hl.adapter_info.backend).replace('\0', " ")).unwrap_or_default();
        let mut ctx = Box::new(Self {
            assets,
            model: Box::new(PreviewMixer::default()),
            gpu,
            renderer,
            png_renderer: None,
            list: DrawList::default(),
            hits: Vec::new(),
            content: (0.0, 0.0),
            interaction: Interaction::default(),
            pan: None,
            view: View::new(width, height, scale),
            compact: false,
            dirty: true,
            stats: UacStats::default(),
            adapter_name,
        });
        ctx.build();
        Ok(ctx)
    }

    /// Rebuilds the draw list and hit regions from the model, then applies the view origin.
    fn build(&mut self) {
        let t = Instant::now();
        let mut ui = Ui::new(&self.assets);
        let layout = build_window(&mut ui, self.model.as_ref(), self.compact);
        self.content = layout.size();
        self.hits = std::mem::take(&mut ui.hits);
        self.list = std::mem::take(&mut ui.list);
        self.view.clamp(self.content);
        offset_list(&mut self.list, -self.view.origin.0, -self.view.origin.1);
        self.stats.build_ms = t.elapsed().as_secs_f64() * 1e3;
        self.stats.draws = self.list.len() as u32;
        self.stats.hits = self.hits.len() as u32;
    }

    fn frame(&mut self) -> i32 {
        let (w, h) = (self.view.width_px, self.view.height_px);
        if w == 0 || h == 0 {
            return UAC_SKIPPED;
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

    /// Returns 1 when the event lands on (or drives) a control, 0 when it pans or misses.
    fn pointer(&mut self, kind: i32, x: f32, y: f32, flags: u32) -> i32 {
        let (cx, cy) = self.view.console_point(self.content, x, y);
        let on_control = self.hits.iter().any(|h| h.rect.contains(cx, cy));
        let actions = match kind {
            UAC_POINTER_DOWN => {
                self.pan = None;
                let actions = self.interaction.pointer_down(&self.hits, cx, cy, flags & UAC_POINTER_DOUBLE != 0);
                if !on_control {
                    self.pan = Some(Pan { start: (x, y), origin: self.view.origin });
                }
                actions
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
                None => self.interaction.pointer_move(cx, cy, flags & UAC_POINTER_FINE != 0),
            },
            UAC_POINTER_UP | UAC_POINTER_CANCEL => {
                self.pan = None;
                self.interaction.pointer_up()
            }
            other => {
                set_error(format!("unknown pointer kind {other}"));
                return UAC_ERR_UNSUPPORTED;
            }
        };
        for a in &actions {
            self.model.apply(a);
        }
        if !actions.is_empty() {
            self.dirty = true;
        }
        i32::from(on_control || self.interaction.is_dragging() || !actions.is_empty())
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

/// New drawable size in pixels and pixels per view point.
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

/// True when the model, view or input changed since the last presented frame.
///
/// # Safety
/// `ctx` is null or a live context.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uac_needs_frame(ctx: *mut UacContext) -> bool {
    guard(false, || unsafe { ctx_mut(ctx) }.is_some_and(|c| c.dirty))
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

/// Builds the draw list from the model and renders one frame (presenting it on a layer).
/// Returns UAC_OK, UAC_SKIPPED, or a negative error.
///
/// # Safety
/// `ctx` is null or a live context.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uac_frame(ctx: *mut UacContext) -> i32 {
    guard(UAC_ERR_PANIC, || match unsafe { ctx_mut(ctx) } {
        Some(c) => c.frame(),
        None => UAC_ERR_NULL,
    })
}

/// A pointer event in view points. Returns 1 if it hit/drove a control, 0 if it panned or
/// missed, or a negative error.
///
/// # Safety
/// `ctx` is null or a live context.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uac_pointer(ctx: *mut UacContext, kind: i32, x_pt: f32, y_pt: f32, flags: u32) -> i32 {
    guard(UAC_ERR_PANIC, || match unsafe { ctx_mut(ctx) } {
        Some(c) => c.pointer(kind, x_pt, y_pt, flags),
        None => UAC_ERR_NULL,
    })
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
            && c.compact != compact
        {
            c.compact = compact;
            c.build();
            c.dirty = true;
        }
    })
}

/// Replaces the model with the offline preview mixer (6 inputs, 2 aux; edits stay local).
///
/// # Safety
/// `ctx` is null or a live context.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uac_set_model_preview(ctx: *mut UacContext) {
    guard((), || {
        if let Some(c) = unsafe { ctx_mut(ctx) } {
            c.model = Box::new(PreviewMixer::default());
            c.interaction = Interaction::default();
            c.pan = None;
            c.build();
            c.dirty = true;
        }
    })
}

/// TODO(engine-model): attach a `MixerModel` over `ua-engine` connected to a UA Mixer Engine
/// (open-apollo daemon or UA's engine) at `host:port`, read-only unless `flags` enables
/// writes. The hook exists so hosts can wire UI for it; it returns UAC_ERR_UNSUPPORTED until
/// the engine-model crate lands.
///
/// # Safety
/// `ctx` is null or a live context; `host` is null or a valid NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uac_set_model_engine(ctx: *mut UacContext, host: *const c_char, port: u16, flags: u32) -> i32 {
    guard(UAC_ERR_PANIC, || {
        if unsafe { ctx_mut(ctx) }.is_none() {
            return UAC_ERR_NULL;
        }
        let host = unsafe { str_arg(host) }.unwrap_or_default();
        set_error(format!("engine model not available yet ({host}:{port}, flags {flags:#x}); use uac_set_model_preview"));
        UAC_ERR_UNSUPPORTED
    })
}

/// Fills `out` with the latest stats.
///
/// # Safety
/// `ctx` is null or a live context; `out` is null or points to a `UacStats`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uac_get_stats(ctx: *mut UacContext, out: *mut UacStats) -> i32 {
    guard(UAC_ERR_PANIC, || {
        let Some(c) = (unsafe { ctx_mut(ctx) }) else { return UAC_ERR_NULL };
        if out.is_null() {
            set_error("null stats pointer");
            return UAC_ERR_NULL;
        }
        unsafe { out.write(c.fill_stats()) };
        UAC_OK
    })
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

/// Renders the current view offscreen into an RGBA PNG at `path` (debugging, tests).
///
/// # Safety
/// `ctx` is null or a live context; `path` is null or a valid NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uac_write_png(ctx: *mut UacContext, path: *const c_char) -> i32 {
    guard(UAC_ERR_PANIC, || {
        let Some(c) = (unsafe { ctx_mut(ctx) }) else { return UAC_ERR_NULL };
        let Some(path) = (unsafe { path_arg(path) }) else {
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

/// Destroys a context (NULL is ignored). The CAMetalLayer may be released afterwards.
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

    #[test]
    fn abi() {
        // Mirrors the C struct in uaconsole.h: u64, f64×2, u32×6, u64, f32×5 (+4 pad).
        assert_eq!(std::mem::size_of::<UacStats>(), 8 + 16 + 24 + 8 + 20 + 4);
        assert_eq!(std::mem::align_of::<UacStats>(), 8);
        assert_eq!(std::mem::offset_of!(UacStats, texture_bytes), 48);
        assert_eq!(std::mem::offset_of!(UacStats, content_width_pt), 56);
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
            assert!(!uac_needs_frame(std::ptr::null_mut()));
            uac_destroy(std::ptr::null_mut());
            assert!(uac_create(c"/nonexistent".as_ptr(), std::ptr::null_mut(), 10, 10, 1.0).is_null());
            let err = CStr::from_ptr(uac_last_error()).to_string_lossy().into_owned();
            assert!(err.contains("not a UAD Console Resources folder"), "{err}");
        }
    }

    /// Offscreen end-to-end: needs installed Console resources and a GPU; skipped otherwise.
    #[test]
    fn offscreen_frame_and_input() {
        if Resources::locate().is_none() {
            eprintln!("skipping: no Console resources");
            return;
        }
        unsafe {
            let ctx = uac_create(std::ptr::null(), std::ptr::null_mut(), 1900, 2100, 2.0);
            if ctx.is_null() {
                eprintln!("skipping: {}", CStr::from_ptr(uac_last_error()).to_string_lossy());
                return;
            }
            uac_set_compact(ctx, true);
            assert!(uac_needs_frame(ctx));
            assert_eq!(uac_frame(ctx), UAC_OK);
            assert!(!uac_needs_frame(ctx));
            let mut s = UacStats::default();
            assert_eq!(uac_get_stats(ctx, &mut s), UAC_OK);
            assert!(s.quads > 500 && s.hits > 50, "{s:?}");
            // Compact width from the 47-pt side section, the strips, the auxes and the monitor.
            assert!((s.content_width_pt - 951.0).abs() < 0.01, "{s:?}");

            // Tap the first strip's solo button: find it through the hit list.
            let c = &mut *ctx;
            let solo = c.hits.iter().find(|h| &*h.id.strip == "in0" && h.id.control == rack::Control::Solo).cloned().expect("solo hit");
            let z = s.zoom;
            let (vx, vy) = ((solo.rect.x + 2.0 - s.origin_x_pt) * z, (solo.rect.y + 2.0 - s.origin_y_pt) * z);
            assert_eq!(uac_pointer(ctx, UAC_POINTER_DOWN, vx, vy, 0), 1);
            assert_eq!(uac_pointer(ctx, UAC_POINTER_UP, vx, vy, 0), 1);
            assert!(c.model.main(&c.model.inputs()[0]).solo);
            assert!(uac_needs_frame(ctx));

            // Off the window (letterbox) pans nothing in fit mode and reports a miss.
            assert_eq!(uac_pointer(ctx, UAC_POINTER_DOWN, 1.0, 1.0, 0), 0);
            // (y 10 = title bar: the session navigator strip below it has hit regions.)
            assert_eq!(uac_pointer(ctx, UAC_POINTER_MOVE, 40.0, 10.0, 0), 0);
            assert_eq!(uac_pointer(ctx, UAC_POINTER_UP, 40.0, 10.0, 0), 0);

            uac_set_zoom(ctx, 2.0, 0.0, 0.0);
            assert_eq!(uac_get_stats(ctx, &mut s), UAC_OK);
            assert_eq!(s.zoom, 2.0);
            assert_eq!(uac_set_model_engine(ctx, c"127.0.0.1".as_ptr(), 4710, 0), UAC_ERR_UNSUPPORTED);
            if let Ok(dir) = std::env::var("UAC_TEST_PNG") {
                assert_eq!(uac_write_png(ctx, CString::new(dir).unwrap().as_ptr()), UAC_OK);
            }
            uac_destroy(ctx);
        }
    }
}
