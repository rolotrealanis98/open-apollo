//! (feature `webview`, Linux) UA's HTML plug-in GUIs in a WebKitGTK webview through wry.
//!
//! The component's `html/` folder is served on the custom scheme `uapw://localhost/…` (so the
//! GUI's `fetch`es of its own files are same-origin), and the page is opened with the query its
//! `@uaudio/client` expects ([`GuiQuery`]); the GUI then opens its own WebSocket to the host's
//! bridge (WEBBRIDGE.md).
//!
//! **Embedding.** This first step opens the GUI in its own GTK top-level window (or an offscreen
//! one, for snapshots); the console's winit loop calls [`pump`] to service GTK. Trade-offs:
//! - Wayland clients cannot position their windows, so a separate top-level cannot be placed
//!   under our chrome; on X11/XWayland wry's `build_as_child` could embed it in the winit window
//!   instead (not done).
//! - The likely end state on Linux is the whole plug-in window as a GTK window: our chrome
//!   (rendered by `gfx` offscreen) in drawing areas above and below the webview.
//! - Two toolkits on one main thread: GTK must be initialised and pumped on the winit thread.

use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::prelude::*;
use webkit2gtk::{HardwareAccelerationPolicy, LoadEvent, SettingsExt, SnapshotOptions, SnapshotRegion, WebViewExt};
use wry::http::{Request, Response, StatusCode, header::CONTENT_TYPE};
use wry::{WebViewBuilder, WebViewBuilderExtUnix, WebViewExtUnix};

use crate::web::{GuiQuery, LunaComponent};

/// Custom scheme the component's `html/` folder is served on.
pub const SCHEME: &str = "uapw";

/// Initialises GTK on this thread (once, before opening windows).
pub fn init() -> Result<(), String> {
    gtk::init().map_err(|e| e.to_string())
}

/// Services pending GTK events without blocking (call from the app's event loop).
pub fn pump() {
    while gtk::events_pending() {
        gtk::main_iteration_do(false);
    }
}

/// `uapw://localhost/index.html?<query>`.
pub fn page_url(query: &GuiQuery) -> String {
    format!("{SCHEME}://localhost/index.html?{}", query.to_query())
}

pub struct WebPanelWindow {
    pub window: gtk::Window,
    pub webview: wry::WebView,
    pub size: (i32, i32),
    loaded: Rc<Cell<bool>>,
}

impl WebPanelWindow {
    /// Opens `component` with `query`, at the GUI's natural size. `offscreen` renders without
    /// showing a window (snapshots, tests).
    pub fn open(component: &LunaComponent, query: &GuiQuery, title: &str, offscreen: bool) -> Result<Self, String> {
        Self::open_with(component, query, title, offscreen, false)
    }

    /// As [`open`](Self::open); `file_url` loads `file://…/index.html` instead of `uapw://`.
    pub fn open_with(component: &LunaComponent, query: &GuiQuery, title: &str, offscreen: bool, file_url: bool) -> Result<Self, String> {
        let size = (component.window.width.round() as i32, component.window.height.round() as i32);
        let window: gtk::Window = if offscreen { gtk::OffscreenWindow::new().upcast() } else { gtk::Window::new(gtk::WindowType::Toplevel) };
        window.set_title(title);
        window.set_default_size(size.0, size.1);
        window.set_size_request(size.0, size.1);
        let root = component.html_dir.clone();
        let webview = WebViewBuilder::new()
            .with_custom_protocol(SCHEME.into(), move |_, req| serve(&root, &req))
            .with_url(if file_url { component.url(query) } else { page_url(query) })
            .with_devtools(std::env::var_os("UACONSOLE_WEB_DEVTOOLS").is_some())
            .build_gtk(&window)
            .map_err(|e| e.to_string())?;
        let wk = webview.webview();
        if let Some(settings) = WebViewExt::settings(&wk) {
            settings.set_enable_developer_extras(std::env::var_os("UACONSOLE_WEB_DEVTOOLS").is_some());
            if file_url {
                // The GUI fetches its own files (hbgf_dev_features.json).
                settings.set_allow_file_access_from_file_urls(true);
            }
            if offscreen {
                // GTK's offscreen backend has no GL context.
                settings.set_hardware_acceleration_policy(HardwareAccelerationPolicy::Never);
            }
        }
        // The GUI is laid out for its manifest size. WebKitGTK scales pages by the desktop font
        // DPI (gtk-xft-dpi / 96); undo that so 1 CSS px = 1 pt.
        let dpi = gtk::Settings::default().map(|s| s.gtk_xft_dpi()).filter(|d| *d > 0).map(|d| d as f64 / 1024.0);
        wk.set_zoom_level(dpi.map(|d| 96.0 / d).unwrap_or(1.0));
        let loaded = Rc::new(Cell::new(false));
        let flag = loaded.clone();
        wk.connect_load_changed(move |_, ev| {
            if ev == LoadEvent::Finished {
                flag.set(true);
            }
        });
        window.show_all();
        Ok(Self { window, webview, size, loaded })
    }

    pub fn is_loaded(&self) -> bool {
        self.loaded.get()
    }

    /// Pumps GTK until the page has loaded or `timeout` passes.
    pub fn wait_loaded(&self, timeout: Duration) -> bool {
        let t = Instant::now();
        while !self.is_loaded() && t.elapsed() < timeout {
            pump();
            std::thread::sleep(Duration::from_millis(5));
        }
        self.is_loaded()
    }

    /// Pumps GTK for `d` (lets the page run its scripts).
    pub fn run_for(&self, d: Duration) {
        let t = Instant::now();
        while t.elapsed() < d {
            pump();
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Visible-region snapshot as straight-alpha RGBA8 `(width, height, pixels)` in device pixels.
    pub fn snapshot_rgba(&self, timeout: Duration) -> Result<(u32, u32, Vec<u8>), String> {
        let result: Rc<RefCell<Option<Result<gtk::cairo::Surface, String>>>> = Rc::new(RefCell::new(None));
        let slot = result.clone();
        self.webview.webview().snapshot(SnapshotRegion::Visible, SnapshotOptions::NONE, None::<&gtk::gio::Cancellable>, move |r| {
            *slot.borrow_mut() = Some(r.map_err(|e| e.to_string()));
        });
        let t = Instant::now();
        while result.borrow().is_none() && t.elapsed() < timeout {
            pump();
            std::thread::sleep(Duration::from_millis(5));
        }
        let surface = result.borrow_mut().take().ok_or("snapshot timed out")??;
        // Paint onto our own image surface so its pixels are ours to read, at the snapshot's
        // device scale (2 on HiDPI outputs).
        // The snapshot is in device pixels but may not carry its device scale: take the real
        // pixel size when it is an image surface.
        let (w, h) = match gtk::cairo::ImageSurface::try_from(surface.clone()) {
            Ok(i) if i.width() > 0 && i.height() > 0 => (i.width(), i.height()),
            _ => {
                let (sx, sy) = surface.device_scale();
                ((self.size.0 as f64 * sx).round() as i32, (self.size.1 as f64 * sy).round() as i32)
            }
        };
        let (sx, sy) = (w as f64 / self.size.0.max(1) as f64, h as f64 / self.size.1.max(1) as f64);
        surface.set_device_scale(1.0, 1.0);
        let mut img = gtk::cairo::ImageSurface::create(gtk::cairo::Format::ARgb32, w, h).map_err(|e| e.to_string())?;
        let _ = (sx, sy);
        {
            let cr = gtk::cairo::Context::new(&img).map_err(|e| e.to_string())?;
            cr.set_source_surface(&surface, 0.0, 0.0).map_err(|e| e.to_string())?;
            cr.paint().map_err(|e| e.to_string())?;
        }
        img.flush();
        let stride = img.stride() as usize;
        let data = img.data().map_err(|e| e.to_string())?;
        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h as usize {
            for px in data[y * stride..y * stride + w as usize * 4].chunks_exact(4) {
                // Cairo ARGB32 is native-endian premultiplied: B, G, R, A on little-endian.
                let a = px[3];
                let un = |c: u8| if a == 0 { 0 } else { ((c as u32 * 255 + a as u32 / 2) / a as u32).min(255) as u8 };
                rgba.extend_from_slice(&[un(px[2]), un(px[1]), un(px[0]), a]);
            }
        }
        Ok((w as u32, h as u32, rgba))
    }

    /// Snapshot written as a PNG.
    pub fn snapshot_png(&self, path: &Path, timeout: Duration) -> Result<(u32, u32), String> {
        let (w, h, rgba) = self.snapshot_rgba(timeout)?;
        let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
        let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().and_then(|mut wr| wr.write_image_data(&rgba)).map_err(|e| e.to_string())?;
        Ok((w, h))
    }

    /// Closes the window (the webview widget goes with it).
    pub fn close(self) {
        self.window.close();
    }
}

/// Serves a file under `root` for a `uapw://localhost/<path>` request.
fn serve(root: &Path, req: &Request<Vec<u8>>) -> Response<Cow<'static, [u8]>> {
    let rel = req.uri().path().trim_start_matches('/');
    let rel = if rel.is_empty() { "index.html" } else { rel };
    let rel = percent_decode(rel);
    let path = PathBuf::from(&rel);
    let safe = path.components().all(|c| matches!(c, Component::Normal(_)));
    let body = if safe { std::fs::read(root.join(&path)).ok() } else { None };
    match body {
        Some(bytes) => Response::builder().header(CONTENT_TYPE, mime(&rel)).body(Cow::Owned(bytes)).unwrap(),
        None => Response::builder().status(StatusCode::NOT_FOUND).body(Cow::Borrowed(&b"not found"[..])).unwrap(),
    }
}

fn mime(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("").to_ascii_lowercase().as_str() {
        "html" | "htm" => "text/html",
        "js" | "mjs" => "text/javascript",
        "json" => "application/json",
        "css" => "text/css",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "svg" => "image/svg+xml",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
