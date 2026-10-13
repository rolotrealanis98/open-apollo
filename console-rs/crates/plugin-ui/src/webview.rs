//! (feature `webview`, Linux) UA's HTML plug-in GUIs in a WebKitGTK webview through wry.
//!
//! The page is loaded the way UAD Console 1.3.1 loads it on macOS: `ua-content://gui/index.html`
//! with Console's query (`host`, `port`, `path`, `plugin_id`; [`GuiQuery`]), the component's
//! `html/` folder served by a `ua-content` handler with Console's rules and headers
//! ([`crate::content`]). The Windows (1.2.x) `file://…/index.html` form stays available
//! ([`GuiSource::File`]). The GUI then opens its own WebSocket to the host's bridge (WEBBRIDGE.md).
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
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::prelude::*;
use webkit2gtk::{HardwareAccelerationPolicy, LoadEvent, SettingsExt, SnapshotOptions, SnapshotRegion, WebViewExt};
use wry::http::{Request, Response};
use wry::{WebViewBuilder, WebViewBuilderExtUnix, WebViewExtUnix};

use crate::content::{self, ContentRoots};
use crate::web::{GuiQuery, LunaComponent};

/// Custom scheme the component's `html/` folder is served on (Console 1.3.1's).
pub const SCHEME: &str = content::SCHEME;

/// How the GUI page is loaded.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GuiSource {
    /// `ua-content://gui/index.html?…`: Console 1.3.1 (macOS), through our `ua-content` handler.
    #[default]
    UaContent,
    /// `file://…/index.html?…`: Console 1.2.x (Windows layout).
    File,
}

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

/// `ua-content://gui/index.html?<query>`.
pub fn page_url(query: &GuiQuery) -> String {
    content::page_url(query)
}

pub struct WebPanelWindow {
    pub window: gtk::Window,
    pub webview: wry::WebView,
    pub size: (i32, i32),
    loaded: Rc<Cell<bool>>,
    /// The zoom that makes 1 CSS px = 1 pt; Resize Plug-In multiplies it.
    base_zoom: f64,
}

impl WebPanelWindow {
    /// Opens `component` with `query`, at the GUI's natural size, on `ua-content://`. `offscreen`
    /// renders without showing a window (snapshots, tests).
    pub fn open(component: &LunaComponent, query: &GuiQuery, title: &str, offscreen: bool) -> Result<Self, String> {
        Self::open_with(component, query, title, offscreen, GuiSource::UaContent)
    }

    /// As [`open`](Self::open), loading the page from `source`.
    pub fn open_with(component: &LunaComponent, query: &GuiQuery, title: &str, offscreen: bool, source: GuiSource) -> Result<Self, String> {
        let size = (component.window.width.round() as i32, component.window.height.round() as i32);
        let window: gtk::Window = if offscreen { gtk::OffscreenWindow::new().upcast() } else { gtk::Window::new(gtk::WindowType::Toplevel) };
        window.set_title(title);
        window.set_default_size(size.0, size.1);
        window.set_size_request(size.0, size.1);
        let file_url = source == GuiSource::File;
        let roots = ContentRoots::for_component(component);
        let webview = WebViewBuilder::new()
            .with_custom_protocol(SCHEME.into(), move |_, req| serve(&roots, &req))
            .with_url(if file_url { component.url(query) } else { component.content_url(query) })
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
        let base_zoom = dpi.map(|d| 96.0 / d).unwrap_or(1.0);
        wk.set_zoom_level(base_zoom);
        let loaded = Rc::new(Cell::new(false));
        let flag = loaded.clone();
        wk.connect_load_changed(move |_, ev| {
            if ev == LoadEvent::Finished {
                flag.set(true);
            }
        });
        window.show_all();
        Ok(Self { window, webview, size, loaded, base_zoom })
    }

    /// Resize Plug-In: the page zoomed by `scale` and the window sized to match.
    pub fn set_scale(&self, scale: f64) {
        let wk = self.webview.webview();
        wk.set_zoom_level(self.base_zoom * scale);
        let (w, h) = ((self.size.0 as f64 * scale).round() as i32, (self.size.1 as f64 * scale).round() as i32);
        self.window.set_size_request(w, h);
        self.window.resize(w, h);
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

/// Answers a `ua-content://gui/<path>` request as Console 1.3.1's `UAPWSchemeHandler` does.
fn serve(roots: &ContentRoots, req: &Request<Vec<u8>>) -> Response<Cow<'static, [u8]>> {
    let reply = roots.respond(req.uri().path());
    let mut res = Response::builder().status(reply.status);
    for (k, v) in &reply.headers {
        res = res.header(*k, v.as_str());
    }
    res.body(Cow::Owned(reply.body)).unwrap_or_else(|_| Response::new(Cow::Borrowed(&b""[..])))
}
