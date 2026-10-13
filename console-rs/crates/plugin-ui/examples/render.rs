//! Headless render of a plug-in panel.
//!
//!   cargo run -p plugin-ui --example render -- <spec name> out.png [--scale 2] [--generic]
//!       [--window [--title "GTR, Insert 4"] [--status "NO UAD-2 DEVICES FOUND"] [--web]]
//!
//! `--window` draws Console's plug-in window chrome around the content (`--web` picks UA's HTML
//! GUI when a lunacomponent matches; a headless render shows a placeholder where the webview goes).
//! Writes `out.png` (parameters at their spec defaults) and `out.zero.png` (all parameters 0, the
//! state the spec pipeline's LUNA references show), and compares the latter with a LUNA reference
//! (`$PLUGIN_UI_REFS` or `~/wine-ua-work/plugin-specs/render/uaudio_<stem>.ref.png`) when one exists.
//! Both are also compared with the live AU snapshots of the spec pipeline (agent F:
//! `$PLUGIN_UI_LIVE_REFS` or `~/wine-ua-work/plugin-specs/agents/F/reference[_zero]/<stem>.png`).
//! `<spec name>` is a file stem (`ua_1176_rev_a`), an effect name or a name pattern.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use houston::{Appearance, ImageCache, ImageData, Rect, Resources};
use plugin_ui::{GenericPanel, LunaLibrary, MockParams, PluginPanel, PluginSpecLibrary, PluginWindow, SpecLocator};
use rack::{Assets, Ui};

fn arg(name: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1).cloned())
}

fn flag(name: &str) -> bool {
    std::env::args().any(|a| a == name)
}

fn main() {
    let positional: Vec<String> = {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let mut out = Vec::new();
        let mut i = 0;
        while i < args.len() {
            if ["--scale", "--title", "--status"].contains(&args[i].as_str()) {
                i += 2;
                continue;
            }
            if !args[i].starts_with("--") {
                out.push(args[i].clone());
            }
            i += 1;
        }
        out
    };
    let [name, out] = positional.as_slice() else {
        eprintln!("usage: render <spec name> out.png [--scale N] [--generic]");
        std::process::exit(64);
    };
    let scale: f32 = arg("--scale").and_then(|s| s.parse().ok()).unwrap_or(2.0);
    let Some(lib) = PluginSpecLibrary::locate() else {
        eprintln!("render: no plug-in spec folder. Looked in:");
        for c in SpecLocator::candidates() {
            eprintln!("  {}", c.display());
        }
        std::process::exit(2);
    };
    let Some(entry) = lib.entry_named(name) else {
        eprintln!("render: no spec named {name:?} in {} ({} specs)", lib.folder.display(), lib.entries.len());
        std::process::exit(2);
    };
    let spec = lib.spec(entry).unwrap_or_else(|e| {
        eprintln!("render: {e}");
        std::process::exit(2);
    });
    eprintln!("spec: {} ({}), {}×{} pt, {} elements, {} parameters", spec.effect_name, entry.stem(), spec.size.w, spec.size.h, spec.elements.len(), spec.parameters.len());

    // UA Console resources give fonts (and, for the window chrome, skins); a panel renders without.
    let assets = match Resources::locate() {
        Some(r) => Assets::load(r).unwrap_or_else(|e| {
            eprintln!("render: appearance: {e}");
            std::process::exit(2);
        }),
        None => {
            let r = Resources { root: PathBuf::from("/nonexistent"), console_version: None };
            Assets::from_parts(r.clone(), Appearance::default(), Arc::new(ImageCache::new(r)))
        }
    };
    lib.attach(&assets.images);

    let Some(gpu) = gfx::Headless::new() else {
        eprintln!("render: no GPU adapter");
        std::process::exit(3);
    };
    let mut renderer = gfx::Renderer::new(&gpu.device, &gpu.queue, gfx::TextureFormat::Rgba8Unorm, assets.images.clone());
    let slot = "/devices/0/inputs/0/effects/0";
    let mut panel = PluginPanel::new(spec.clone(), &lib, slot);
    // The live snapshots' state: AU parameter values and display strings (agent F's dumps) and
    // meters running in silence (every polled variable reads 0).
    let live = live_state(entry.stem());
    silence(&mut panel, &MockParams::from_spec(&spec));
    if !panel.missing_sprites().is_empty() {
        eprintln!("missing sprites: {:?}", panel.missing_sprites());
    }

    let zero_path = sibling(Path::new(out), "zero");
    for (path, zero) in [(PathBuf::from(out), false), (zero_path.clone(), true)] {
        let mut mock = MockParams::from_spec(&spec);
        let mut strings = std::collections::HashMap::new();
        if zero {
            mock.set_all(0.0);
        } else if let Some(live) = &live {
            for (k, v, display) in live {
                if let Some(p) = mock.params.get_mut(*k as usize) {
                    p.value = *v;
                }
                strings.insert(*k, display.clone());
            }
        }
        let params = Strings { inner: mock, strings };
        let mut ui = Ui::new(&assets);
        let size = if flag("--window") {
            let luna = LunaLibrary::locate();
            let mut win = PluginWindow::open(slot, &spec.effect_name, &arg("--title").unwrap_or_else(|| "GTR, Insert 4".into()), Some(&lib), Some(&luna), flag("--web"));
            if flag("--generic") {
                win.content = plugin_ui::Content::Generic(GenericPanel::new(slot));
            }
            win.chrome.status = arg("--status");
            let size = win.size(&params);
            let layout = win.build(&mut ui, (0.0, 0.0), &params);
            eprintln!("window: {}×{} pt, content {} at {:?}", size.0, size.1, win.content.kind(), layout.editor);
            size
        } else if flag("--generic") {
            let g = GenericPanel::new(slot);
            let size = g.size(&params);
            g.build(&mut ui, Rect::new(0.0, 0.0, size.0, size.1), &params);
            size
        } else {
            let size = panel.size();
            panel.build(&mut ui, Rect::new(0.0, 0.0, size.0, size.1), &params);
            size
        };
        gpu.render_png(&mut renderer, &ui.list, size, scale, &path).expect("write png");
        eprintln!("wrote {} ({} draws, {} hits)", path.display(), ui.list.len(), ui.hits.len());
    }

    if !flag("--generic") && !flag("--window") && scale == 2.0 {
        match reference(entry.stem()) {
            Some(r) => compare(&zero_path, &r),
            None => eprintln!("no LUNA reference for {}", entry.stem()),
        }
        for (ours, sub) in [(PathBuf::from(out), "reference"), (zero_path.clone(), "reference_zero")] {
            let dir = std::env::var("PLUGIN_UI_LIVE_REFS")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join("wine-ua-work/plugin-specs/agents/F"));
            let r = dir.join(sub).join(format!("{}.png", entry.stem()));
            if r.is_file() {
                compare(&ours, &r);
            }
        }
    }
    let missing = assets.images.missing();
    if !missing.is_empty() {
        eprintln!("images that failed to load: {missing:?}");
    }
}

/// `(index, normalized value, display)` per parameter from agent F's AU dump
/// (`$PLUGIN_UI_LIVE_REFS` or `~/wine-ua-work/plugin-specs/agents/F` + `params/<stem>.json`).
fn live_state(stem: &str) -> Option<Vec<(u16, f64, String)>> {
    let dir = std::env::var("PLUGIN_UI_LIVE_REFS").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join("wine-ua-work/plugin-specs/agents/F"));
    let text = std::fs::read_to_string(dir.join("params").join(format!("{stem}.json"))).ok()?;
    let list: Vec<serde_json::Value> = serde_json::from_str(&text).ok()?;
    Some(
        list.iter()
            .filter_map(|p| {
                let (id, v) = (p["id"].as_u64()? as u16, p["value"].as_f64()?);
                let (lo, hi) = (p["min"].as_f64().unwrap_or(0.0), p["max"].as_f64().unwrap_or(1.0));
                let n = if hi > lo { (v - lo) / (hi - lo) } else { v };
                Some((id, n, p["display"].as_str().unwrap_or_default().to_string()))
            })
            .collect(),
    )
}

/// Every meter source reads 0 (a running plug-in in silence), settled.
fn silence(panel: &mut PluginPanel, params: &MockParams) {
    let spec = panel.spec.clone();
    let floats = spec.poll_floats();
    if !floats.is_empty() {
        panel.ingest_floats(&vec!["0"; floats.len()].join(","), params);
    }
    if let Some((_, p0, n)) = spec.opcode_range() {
        panel.ingest_opcode(p0, &vec![0.0; n.max(0) as usize], params);
    }
    panel.meters.tick(&spec, 10.0);
}

/// Mock parameters with Console display strings.
struct Strings {
    inner: MockParams,
    strings: std::collections::HashMap<u16, String>,
}

impl plugin_ui::ParamSource for Strings {
    fn count(&self) -> usize {
        self.inner.count()
    }
    fn name(&self, k: u16) -> Option<String> {
        self.inner.name(k)
    }
    fn normalized(&self, k: u16) -> Option<f64> {
        self.inner.normalized(k)
    }
    fn display(&self, k: u16) -> Option<String> {
        self.strings.get(&k).cloned().or_else(|| self.inner.display(k).filter(|_| self.strings.is_empty()))
    }
    fn steps(&self, k: u16) -> Vec<String> {
        self.inner.steps(k)
    }
    fn begin_gesture(&mut self) {}
    fn set(&mut self, k: u16, v: f64) {
        self.inner.set(k, v)
    }
    fn end_gesture(&mut self) {}
}

/// `out.png` → `out.<tag>.png`.
fn sibling(path: &Path, tag: &str) -> PathBuf {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("out");
    path.with_file_name(format!("{stem}.{tag}.png"))
}

/// `uaudio_<stem>.ref.png`, matching `-` and `_` loosely (`teletronix_la-2a_silver`).
fn reference(stem: &str) -> Option<PathBuf> {
    let dir = std::env::var("PLUGIN_UI_REFS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join("wine-ua-work/plugin-specs/render"));
    let norm = |s: &str| s.replace('-', "_").to_lowercase();
    std::fs::read_dir(&dir).ok()?.flatten().map(|e| e.path()).find(|p| {
        p.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_suffix(".ref.png")).is_some_and(|n| norm(n.strip_prefix("uaudio_").unwrap_or(n)) == norm(stem))
    })
}

/// Share of pixels whose largest channel difference exceeds 24/255 (alpha-composited on black).
fn compare(ours: &Path, reference: &Path) {
    let (Some(a), Some(b)) = (ImageData::decode_png(ours), ImageData::decode_png(reference)) else {
        eprintln!("compare: could not decode {} or {}", ours.display(), reference.display());
        return;
    };
    if (a.width, a.height) != (b.width, b.height) {
        // Compare the common top-left area (live snapshots may carry a vendor toolbar or the
        // UA wrapper's extra rows).
        eprintln!("compare: size {}×{} vs reference {}×{} ({}); comparing the common area", a.width, a.height, b.width, b.height, reference.display());
    }
    let (w, h) = (a.width.min(b.width), a.height.min(b.height));
    let px = |d: &ImageData, x: u32, y: u32| {
        let i = (y * d.width + x) as usize;
        let p = &d.rgba[i * 4..i * 4 + 4];
        let a = p[3] as f32 / 255.0;
        [p[0] as f32 * a, p[1] as f32 * a, p[2] as f32 * a]
    };
    let n = (w * h) as usize;
    let off = (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).filter(|&(x, y)| px(&a, x, y).iter().zip(px(&b, x, y)).any(|(p, q)| (p - q).abs() > 24.0)).count();
    eprintln!("vs {}: {:.3}% pixels off (>24/255)", reference.display(), off as f64 * 100.0 / n as f64);
}
