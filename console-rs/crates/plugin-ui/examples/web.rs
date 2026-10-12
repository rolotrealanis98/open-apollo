//! Opens one of UA's HTML plug-in GUIs in a WebKitGTK webview.
//!
//!   cargo run -p plugin-ui --features webview --example web -- <component> [--snapshot out.png]
//!       [--show] [--seconds N] [--ip 127.0.0.1] [--port 4718] [--path /devices/0/inputs/0/effects/0]
//!
//! `<component>` is an id (`ssl_g_bus_compressor`), a name or an engine EffectName. Without
//! `--show` the page renders offscreen; `--snapshot` writes what it shows after `--seconds`
//! (default 4). The GUI connects its own WebSocket to `--ip:--port`; with nothing listening
//! there it stays on its loading/disconnected state. Never point it at a real engine unless
//! writes are meant to happen: the GUI writes parameters directly.

use std::path::PathBuf;
use std::time::Duration;

use plugin_ui::webview::{self, WebPanelWindow};
use plugin_ui::{GuiQuery, LunaLibrary, LunaLocator};

fn arg(name: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1).cloned())
}

fn main() {
    let Some(name) = std::env::args().nth(1).filter(|a| !a.starts_with("--")) else {
        eprintln!("usage: web <component> [--snapshot out.png] [--show] [--seconds N] [--ip IP] [--port P] [--path SLOT]");
        std::process::exit(64);
    };
    let lib = LunaLibrary::locate();
    if lib.components.is_empty() {
        eprintln!("web: no lunacomponents found. Looked in:");
        for c in LunaLocator::candidates() {
            eprintln!("  {}", c.display());
        }
        std::process::exit(2);
    }
    let Some(component) = lib.named(&name) else {
        eprintln!("web: no component {name:?} ({} found in {:?})", lib.components.len(), lib.folders);
        std::process::exit(2);
    };
    eprintln!(
        "component: {} ({}), {}×{} pt, images: {}, client interface {}",
        component.name,
        component.id,
        component.window.width,
        component.window.height,
        component.has_images,
        component.client_interface_version.as_deref().unwrap_or("?")
    );
    let ip = arg("--ip").unwrap_or_else(|| "127.0.0.1".into());
    let port: u16 = arg("--port").and_then(|p| p.parse().ok()).unwrap_or(4718);
    let path = arg("--path").unwrap_or_else(|| "/devices/0/inputs/0/effects/0".into());
    let query = GuiQuery::new(&ip, port, &path);
    eprintln!("url: {}", webview::page_url(&query));

    let show = std::env::args().any(|a| a == "--show");
    if !show {
        // SAFETY: single-threaded here, before GTK or WebKit start. GTK's offscreen backend has
        // no GL context, and WebKitGTK's DMA-BUF renderer needs one in the UI process.
        unsafe { std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1") };
    }
    webview::init().unwrap_or_else(|e| {
        eprintln!("web: gtk: {e}");
        std::process::exit(3);
    });
    let win = WebPanelWindow::open(component, &query, &format!("{} (preview)", component.name), !show).unwrap_or_else(|e| {
        eprintln!("web: webview: {e}");
        std::process::exit(3);
    });
    let loaded = win.wait_loaded(Duration::from_secs(10));
    eprintln!("page loaded: {loaded}");
    let seconds: f64 = arg("--seconds").and_then(|s| s.parse().ok()).unwrap_or(4.0);
    win.run_for(Duration::from_secs_f64(seconds));
    if let Some(out) = arg("--snapshot") {
        match win.snapshot_png(&PathBuf::from(&out), Duration::from_secs(10)) {
            Ok((w, h)) => eprintln!("wrote {out} ({w}×{h})"),
            Err(e) => eprintln!("web: snapshot: {e}"),
        }
    }
    win.close();
}
