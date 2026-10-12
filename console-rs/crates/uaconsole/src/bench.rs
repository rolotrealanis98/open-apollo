//! `--render` and `--bench`: headless frames through the same build path as the window.

use std::path::Path;
use std::time::Instant;

use gfx::wgpu;
use houston::Rect;
use rack::{Action, Assets, Control, ControlId, MixerModel, ViewState};

use crate::app::LETTERBOX;
use crate::frame::{build_frame, layout_for, ms, summary};
use crate::overlay::{OverlayStack, PlaceholderHook};

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

fn gpu() -> Result<gfx::Headless, String> {
    let gpu = gfx::Headless::new().ok_or("no GPU adapter")?;
    eprintln!(
        "gpu: {} ({:?})",
        gpu.adapter_info.name, gpu.adapter_info.backend
    );
    Ok(gpu)
}

pub fn render_png(
    assets: &Assets,
    model: &dyn MixerModel,
    compact: bool,
    size: Option<(f32, f32)>,
    scale: f32,
    open_plugin: Option<&(String, u8)>,
    view_spec: Option<&str>,
    menu: Option<&str>,
    menu_bar: bool,
    out: &Path,
) -> Result<(), String> {
    let mut overlays = OverlayStack::new(Box::new(crate::plugin::PluginUiHook::new(&assets.images)));
    if let Some((strip, slot)) = open_plugin {
        overlays.on_open_plugin(strip, *slot, model);
    }
    // `--menu plugin:assign|more|help` opens the plug-in window's menu instead.
    let menu = match menu.and_then(|m| m.strip_prefix("plugin:")) {
        Some(which) => {
            if !overlays.open_menu(which) {
                return Err(format!("--menu plugin:{which}: no plug-in window menu by that name (open one with --open-plugin)"));
            }
            None
        }
        None => menu,
    };
    let mut view = ViewState {
        compact,
        window: size,
        menu_bar,
        ..ViewState::default()
    };
    if let Some(spec) = view_spec {
        view.apply_spec(spec)?;
    }
    // Dev: render a session surface open (`UACONSOLE_RENDER_OPEN=session-menu|save-changes|save-as`).
    if let Ok(what) = std::env::var("UACONSOLE_RENDER_OPEN") {
        use rack::surfaces::SessionOp;
        let tap = |c: Control| Action::Tap(ControlId { strip: "session".into(), control: c });
        let taps: &[Control] = match what.as_str() {
            "session-menu" => &[Control::SessionMenu],
            "save-changes" => &[Control::SessionMenu, Control::Session(SessionOp::New)],
            "save-as" => &[Control::SessionMenu, Control::Session(SessionOp::SaveAs)],
            _ => &[],
        };
        for c in taps {
            let _ = view.route(tap(c.clone()), model);
        }
    }
    let (w, h) = layout_for(model, &view).size();
    if let Some(spec) = menu {
        let first = build_frame(assets, model, &view, &mut overlays, Rect::new(0.0, 0.0, w, h), None);
        view.open_menu_spec(&first.hits, spec)?;
    }
    let t = Instant::now();
    let frame = build_frame(
        assets,
        model,
        &view,
        &mut overlays,
        Rect::new(0.0, 0.0, w, h),
        None,
    );
    let build_ms = ms(t.elapsed());
    let gpu = gpu()?;
    let mut renderer = gfx::Renderer::new(&gpu.device, &gpu.queue, FORMAT, assets.images.clone());
    let t = Instant::now();
    gpu.render_png(&mut renderer, &frame.list, frame.layout.size(), scale, out)
        .map_err(|e| format!("write {}: {e}", out.display()))?;
    let render_ms = ms(t.elapsed());
    let s = renderer.stats;
    eprintln!(
        "wrote {}: {} draws, {} hits; build {build_ms:.2} ms; first render {render_ms:.0} ms; {} quads, {} batches, {} textures ({:.1} MB), {} glyphs",
        out.display(),
        frame.list.len(),
        frame.hits.len(),
        s.quads,
        s.batches,
        s.textures,
        s.texture_bytes as f64 / 1e6,
        s.glyphs
    );
    Ok(())
}

/// Renders `frames` frames (after one warm-up frame that uploads textures) into an offscreen
/// target, moving a fader each frame so text and caps change. Render time includes waiting
/// for the GPU.
pub fn bench(
    assets: &Assets,
    model: &mut dyn MixerModel,
    compact: bool,
    scale: f32,
    frames: usize,
) -> Result<(), String> {
    let gpu = gpu()?;
    let mixer_view = ViewState {
        compact,
        ..ViewState::default()
    };
    let layout = layout_for(model, &mixer_view);
    let (lw, lh) = layout.size();
    let (w, h) = ((lw * scale).ceil() as u32, (lh * scale).ceil() as u32);
    let tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("bench"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
    let mut renderer = gfx::Renderer::new(&gpu.device, &gpu.queue, FORMAT, assets.images.clone());
    let mut overlays = OverlayStack::new(Box::new(PlaceholderHook));
    let area = Rect::new(0.0, 0.0, lw, lh);
    let fader = model.inputs().first().map(|s| ControlId {
        strip: s.id.clone(),
        control: Control::Fader,
    });
    let (mut build, mut render) = (Vec::with_capacity(frames), Vec::with_capacity(frames));
    let mut first = (0.0, 0.0);
    for i in 0..=frames {
        if let Some(id) = &fader {
            model.apply(&Action::Set(id.clone(), (i % 97) as f64 / 96.0));
        }
        let t = Instant::now();
        let frame = build_frame(assets, model, &mixer_view, &mut overlays, area, None);
        let b = ms(t.elapsed());
        let t = Instant::now();
        renderer.render(
            &gpu.device,
            &gpu.queue,
            &frame.list,
            &view,
            (w, h),
            scale,
            LETTERBOX,
        );
        gpu.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| format!("poll: {e}"))?;
        let r = ms(t.elapsed());
        if i == 0 {
            first = (b, r);
        } else {
            build.push(b);
            render.push(r);
        }
    }
    let (ba, bp, bm) = summary(&build);
    let (ra, rp, rm) = summary(&render);
    let s = renderer.stats;
    println!(
        "bench: {frames} frames, {w}×{h} px (scale {scale}, {}), {} strips",
        if compact { "compact" } else { "full" },
        layout.inputs + layout.auxes
    );
    println!("  build   avg {ba:.3} ms   p99 {bp:.3} ms   max {bm:.3} ms");
    println!(
        "  render  avg {ra:.3} ms   p99 {rp:.3} ms   max {rm:.3} ms   (encode + submit + GPU wait)"
    );
    println!(
        "  frame   avg {:.3} ms → {:.0} fps headroom",
        ba + ra,
        1000.0 / (ba + ra).max(1e-6)
    );
    println!(
        "  first frame: build {:.1} ms, render {:.1} ms (texture uploads)",
        first.0, first.1
    );
    println!(
        "  per frame: {} quads, {} batches, {} textures ({:.1} MB), {} glyphs",
        s.quads,
        s.batches,
        s.textures,
        s.texture_bytes as f64 / 1e6,
        s.glyphs
    );
    Ok(())
}
