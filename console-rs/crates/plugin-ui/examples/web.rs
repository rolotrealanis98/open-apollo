//! Opens one of UA's HTML plug-in GUIs in a WebKitGTK webview.
//!
//!   cargo run -p plugin-ui --features webview --example web -- <component> [--snapshot out.png]
//!       [--show] [--seconds N] [--ip 127.0.0.1] [--port 4718] [--path /devices/0/inputs/0/effects/0]
//!
//! `<component>` is an id (`ssl_g_bus_compressor`), a name or an engine EffectName. Without
//! `--show` the page renders offscreen; `--snapshot` writes what it shows after `--seconds`
//! (default 4).
//!
//! By default the example runs plugin-ui's UANet bridge on an ephemeral 127.0.0.1 port and
//! answers the GUI from mock parameters (the ConsoleLink spec's defaults when one matches;
//! control ids scraped from the bundle). `--port P` instead points the GUI at an existing
//! endpoint and runs no bridge. The page loads as Console 1.3.1 loads it,
//! `ua-content://gui/index.html?host=…&port=…&path=…&plugin_id=…`; `--file` loads the 1.2.x
//! `file://…/index.html` form instead.
//! `--zero` starts every parameter at 0 (the LUNA references' state); `--compare ref.png` reports
//! the share of pixels that differ from a reference of the same size.

use std::path::PathBuf;
use std::time::Duration;

use std::collections::BTreeMap;

use plugin_ui::bridge::{BridgeEvent, BridgeServer};
use plugin_ui::uanet::{ControlMap, UaNetBridge, scrape_control_ids};
use plugin_ui::webview::{self, GuiSource, WebPanelWindow};
use plugin_ui::{GuiQuery, LunaLibrary, LunaLocator, MockParams, ParamSource, PluginSpecLibrary};

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
    let path = arg("--path").unwrap_or_else(|| "/devices/0/inputs/0/effects/0".into());

    // Mock parameters: the spec's defaults (spec index = engine ordinal) when one matches.
    let spec = PluginSpecLibrary::locate().and_then(|l| l.entries.iter().find(|e| component.matches_effect(&e.effect_name)).and_then(|e| l.spec(e).ok()));
    let mut params = spec.as_ref().map(|s| MockParams::from_spec(s)).unwrap_or_default();
    if std::env::args().any(|a| a == "--zero") {
        params.set_all(0.0);
    }
    let bundle = std::fs::read_to_string(component.html_dir.join("bundle.js")).unwrap_or_default();
    let ids = scrape_control_ids(&bundle);
    let meters: Vec<String> = spec.as_ref().and_then(|s| s.meter_sources.as_ref()).map(|m| m.keys().cloned().collect()).unwrap_or_default();
    let map = ControlMap::build(&ids, &params, &BTreeMap::new(), meters);
    let mapped: Vec<String> = map.controls.iter().map(|(id, k)| format!("{id}={}", k.map(|k| k.to_string()).unwrap_or("-".into()))).collect();
    eprintln!("spec: {}; controls: {}", spec.as_ref().map(|s| s.effect_name.as_str()).unwrap_or("none"), mapped.join(" "));

    let server = match arg("--port").and_then(|p| p.parse::<u16>().ok()) {
        Some(_) => None,
        None => Some(BridgeServer::bind(0).unwrap_or_else(|e| {
            eprintln!("web: bridge: {e}");
            std::process::exit(3);
        })),
    };
    let port = server.as_ref().map(|s| s.port()).or_else(|| arg("--port").and_then(|p| p.parse().ok())).unwrap_or(4718);
    let file = std::env::args().any(|a| a == "--file");
    // Console 1.3.1 adds plugin_id to ua-content URLs only; 1.2.x file:// URLs carry the three keys.
    let query = if file { GuiQuery::new(&ip, port, &path) } else { GuiQuery::console(&ip, port, &path, &component.id) };
    let url = if file { component.url(&query) } else { component.content_url(&query) };
    eprintln!("url: {url}");
    // The bridge's base is the `path` the GUI reads back from that URL.
    let mut uanet = UaNetBridge::for_query(&GuiQuery::parse(&url).unwrap_or_else(|| query.clone()), map);
    uanet.writes_enabled = true; // mock parameters only
    if let Some(s) = &spec {
        uanet.defaults = s.parameters.iter().map(|p| (p.index, p.default_value.unwrap_or(0.0))).collect();
    }

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
    let win = WebPanelWindow::open_with(component, &query, &format!("{} (preview)", component.name), !show, if file { GuiSource::File } else { GuiSource::UaContent }).unwrap_or_else(|e| {
        eprintln!("web: webview: {e}");
        std::process::exit(3);
    });
    let loaded = win.wait_loaded(Duration::from_secs(10));
    eprintln!("page loaded: {loaded}");
    let seconds: f64 = arg("--seconds").and_then(|s| s.parse().ok()).unwrap_or(4.0);
    let t = std::time::Instant::now();
    let (mut frames_in, mut frames_out) = (0, 0);
    while t.elapsed() < Duration::from_secs_f64(seconds) {
        webview::pump();
        if let Some(server) = &server {
            for ev in server.poll() {
                match ev {
                    BridgeEvent::Connected(id, peer) => eprintln!("bridge: GUI connected ({id}, {peer})"),
                    BridgeEvent::Closed(id) => eprintln!("bridge: GUI {id} closed"),
                    BridgeEvent::Text(id, frame) => {
                        frames_in += 1;
                        let trace = std::env::var_os("UACONSOLE_WEB_TRACE").is_some();
                        if trace {
                            eprintln!("<< {}", frame.chars().take(300).collect::<String>());
                        }
                        for reply in uanet.handle(&frame, &mut params) {
                            if trace {
                                eprintln!(">> {}", reply.chars().take(300).collect::<String>());
                            }
                            frames_out += 1;
                            server.send(id, &reply);
                        }
                        for push in uanet.pushes(&params) {
                            frames_out += 1;
                            server.broadcast(&push);
                        }
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    if server.is_some() {
        eprintln!("bridge: {frames_in} frames in, {frames_out} out; gui loaded: {}; {} parameter events", uanet.loaded, params.events.len());
        let changed: Vec<String> = params.indices().into_iter().filter_map(|k| params.events.iter().any(|e| matches!(e, plugin_ui::MockEvent::Set(x, _) if *x == k)).then(|| params.name(k).unwrap_or_default())).collect();
        if !changed.is_empty() {
            eprintln!("bridge: GUI wrote {changed:?}");
        }
    }
    if let Some(js) = arg("--eval") {
        let done = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
        let slot = done.clone();
        let _ = win.webview.evaluate_script_with_callback(&js, move |r| *slot.lock().unwrap() = Some(r));
        let t = std::time::Instant::now();
        while done.lock().unwrap().is_none() && t.elapsed() < Duration::from_secs(5) {
            webview::pump();
            std::thread::sleep(Duration::from_millis(5));
        }
        eprintln!("eval: {}", done.lock().unwrap().clone().unwrap_or_else(|| "(no result)".into()));
    }
    if let Some(out) = arg("--snapshot") {
        match win.snapshot_png(&PathBuf::from(&out), Duration::from_secs(10)) {
            Ok((w, h)) => {
                eprintln!("wrote {out} ({w}×{h})");
                if let Some(r) = arg("--compare") {
                    compare(&PathBuf::from(&out), &PathBuf::from(r));
                }
            }
            Err(e) => eprintln!("web: snapshot: {e}"),
        }
    }
    win.close();
}

/// Share of pixels whose largest channel difference exceeds 24/255 (alpha-composited on black).
fn compare(ours: &std::path::Path, reference: &std::path::Path) {
    use houston::ImageData;
    let (Some(a), Some(b)) = (ImageData::decode_png(ours), ImageData::decode_png(reference)) else {
        eprintln!("compare: could not decode {} or {}", ours.display(), reference.display());
        return;
    };
    if (a.width, a.height) != (b.width, b.height) {
        eprintln!("compare: size {}×{} vs reference {}×{}", a.width, a.height, b.width, b.height);
        return;
    }
    let px = |d: &ImageData, i: usize| {
        let p = &d.rgba[i * 4..i * 4 + 4];
        let a = p[3] as f32 / 255.0;
        [p[0] as f32 * a, p[1] as f32 * a, p[2] as f32 * a]
    };
    let n = (a.width * a.height) as usize;
    let off = (0..n).filter(|&i| px(&a, i).iter().zip(px(&b, i)).any(|(x, y)| (x - y).abs() > 24.0)).count();
    eprintln!("vs {}: {:.3}% pixels off (>24/255)", reference.display(), off as f64 * 100.0 / n as f64);
}
