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
    let panel = PluginPanel::new(spec.clone(), &lib, slot);
    if !panel.missing_sprites().is_empty() {
        eprintln!("missing sprites: {:?}", panel.missing_sprites());
    }

    let zero_path = sibling(Path::new(out), "zero");
    for (path, zero) in [(PathBuf::from(out), false), (zero_path.clone(), true)] {
        let mut params = MockParams::from_spec(&spec);
        if zero {
            params.set_all(0.0);
        }
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
    }
    let missing = assets.images.missing();
    if !missing.is_empty() {
        eprintln!("images that failed to load: {missing:?}");
    }
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
