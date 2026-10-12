//! uaconsole — a native UAD Console for open-apollo.
//!
//!   uaconsole --render out.png [--compact] [--scale 2]     headless render (preview data)

use std::path::PathBuf;
use std::time::Instant;

use houston::Resources;
use rack::{Assets, PreviewMixer, Ui, build_window};

fn arg(name: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1).cloned())
}

fn flag(name: &str) -> bool {
    std::env::args().any(|a| a == name)
}

fn main() {
    let Some(resources) = Resources::locate() else {
        eprintln!("uaconsole: UAD Console resources not found. Looked in:");
        for c in Resources::candidates() {
            eprintln!("  {}", c.display());
        }
        std::process::exit(2);
    };
    let t = Instant::now();
    let assets = Assets::load(resources).unwrap_or_else(|e| {
        eprintln!("uaconsole: appearance: {e}");
        std::process::exit(2);
    });
    eprintln!(
        "resources: {} (Console {}), {} skins in {:.1} ms",
        assets.resources.root.display(),
        assets.resources.console_version.as_deref().unwrap_or("?"),
        assets.appearance.skins.len(),
        t.elapsed().as_secs_f64() * 1e3
    );

    if let Some(out) = arg("--render") {
        let scale: f32 = arg("--scale").and_then(|s| s.parse().ok()).unwrap_or(2.0);
        let model = PreviewMixer::default();
        let t = Instant::now();
        let mut ui = Ui::new(&assets);
        let layout = build_window(&mut ui, &model, flag("--compact"));
        let build_ms = t.elapsed().as_secs_f64() * 1e3;
        let Some(gpu) = gfx::Headless::new() else {
            eprintln!("uaconsole: no GPU adapter");
            std::process::exit(3);
        };
        eprintln!("gpu: {} ({:?})", gpu.adapter_info.name, gpu.adapter_info.backend);
        let mut renderer = gfx::Renderer::new(&gpu.device, &gpu.queue, wgpu_format(), assets.images.clone());
        let t = Instant::now();
        gpu.render_png(&mut renderer, &ui.list, layout.size(), scale, &PathBuf::from(&out)).expect("write png");
        let first_ms = t.elapsed().as_secs_f64() * 1e3;
        // Warm frame: everything uploaded, measures steady-state build + draw.
        let t = Instant::now();
        let mut ui2 = Ui::new(&assets);
        build_window(&mut ui2, &model, flag("--compact"));
        let rebuild_ms = t.elapsed().as_secs_f64() * 1e3;
        let s = renderer.stats;
        eprintln!(
            "wrote {out}: {} draws, {} hits; build {build_ms:.2} ms (warm {rebuild_ms:.2} ms); first render {first_ms:.0} ms; {} quads, {} batches, {} textures ({:.1} MB), {} glyphs",
            ui.list.len(),
            ui.hits.len(),
            s.quads,
            s.batches,
            s.textures,
            s.texture_bytes as f64 / 1e6,
            s.glyphs
        );
        let missing = assets.images.missing();
        if !missing.is_empty() {
            eprintln!("missing images: {missing:?}");
        }
        return;
    }
    eprintln!("usage: uaconsole --render out.png [--compact] [--scale N]");
}

fn wgpu_format() -> gfx::TextureFormat {
    gfx::TextureFormat::Rgba8Unorm
}
