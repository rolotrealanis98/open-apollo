//! Text: cosmic-text shaping + swash rasterisation into an R8 glyph atlas (shelf packed).
//! Console's fonts map to metric-compatible Linux faces (Helvetica → Nimbus Sans,
//! Arial → Liberation Sans); Console's own bundled fonts (Oxygen …) are loaded too.

use std::collections::HashMap;
use std::path::PathBuf;

use cosmic_text::{Attrs, Buffer, CacheKey, Family, FontSystem, Metrics, Shaping, Style, SwashCache, SwashContent, Weight};
use houston::{Rect, Rgba};
use rack::{Align, TextStyle};

const ATLAS: u32 = 2048;

struct Placed {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    left: i32,
    top: i32,
}

pub struct TextSystem {
    fonts: FontSystem,
    swash: SwashCache,
    atlas: wgpu::Texture,
    bind: wgpu::BindGroup,
    glyphs: HashMap<CacheKey, Option<Placed>>,
    shelf: (u32, u32, u32), // x, y, row height
}

fn family_for(name: &str) -> &'static str {
    match name {
        "Helvetica" | "Helvetica Neue" | "Lucida Grande" | "Segoe UI" => "Nimbus Sans",
        "Arial" => "Liberation Sans",
        "Oxygen" => "Oxygen",
        _ => "Nimbus Sans",
    }
}

impl TextSystem {
    pub fn new(device: &wgpu::Device, layout: &wgpu::BindGroupLayout, sampler: &wgpu::Sampler, console_fonts: PathBuf) -> Self {
        let mut fonts = FontSystem::new();
        if console_fonts.is_dir() {
            fonts.db_mut().load_fonts_dir(console_fonts);
        }
        let atlas = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("glyphs"),
            size: wgpu::Extent3d { width: ATLAS, height: ATLAS, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = atlas.create_view(&wgpu::TextureViewDescriptor::default());
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("glyphs"),
            layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(sampler) },
            ],
        });
        Self { fonts, swash: SwashCache::new(), atlas, bind, glyphs: HashMap::new(), shelf: (0, 0, 0) }
    }

    pub fn bind_group(&self) -> &wgpu::BindGroup {
        &self.bind
    }

    fn place(&mut self, queue: &wgpu::Queue, key: CacheKey) -> Option<&Placed> {
        if !self.glyphs.contains_key(&key) {
            let placed = self.rasterize(queue, key);
            self.glyphs.insert(key, placed);
        }
        self.glyphs.get(&key)?.as_ref()
    }

    fn rasterize(&mut self, queue: &wgpu::Queue, key: CacheKey) -> Option<Placed> {
        let img = self.swash.get_image_uncached(&mut self.fonts, key)?;
        let (w, h) = (img.placement.width, img.placement.height);
        if w == 0 || h == 0 {
            return None;
        }
        let mask: Vec<u8> = match img.content {
            SwashContent::Mask => img.data,
            SwashContent::SubpixelMask => img.data.chunks_exact(4).map(|p| p[1]).collect(),
            SwashContent::Color => img.data.chunks_exact(4).map(|p| p[3]).collect(),
        };
        let (mut x, mut y, mut row) = self.shelf;
        if x + w + 1 > ATLAS {
            x = 0;
            y += row + 1;
            row = 0;
        }
        if y + h + 1 > ATLAS {
            return None; // atlas full (not expected for Console's small label set)
        }
        queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &self.atlas, mip_level: 0, origin: wgpu::Origin3d { x, y, z: 0 }, aspect: wgpu::TextureAspect::All },
            &mask,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(w), rows_per_image: Some(h) },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        self.shelf = (x + w + 1, y, row.max(h));
        Some(Placed { x, y, w, h, left: img.placement.left, top: img.placement.top })
    }

    /// Lays out `text` in `rect` (pixels), calling `emit(rect, uv, colour)` per glyph.
    /// Returns the glyph count.
    #[allow(clippy::too_many_arguments)]
    pub fn layout(
        &mut self,
        _device: &wgpu::Device,
        queue: &wgpu::Queue,
        text: &str,
        style: &TextStyle,
        rect: Rect,
        align: Align,
        scale: f32,
        emit: &mut dyn FnMut(Rect, [f32; 4], Rgba),
    ) -> usize {
        let size = style.size * scale;
        let line_h = (size * 1.15).ceil();
        let mut buffer = Buffer::new(&mut self.fonts, Metrics::new(size, line_h));
        let attrs = Attrs::new()
            .family(Family::Name(family_for(&style.family)))
            .weight(if style.bold { Weight::BOLD } else { Weight::NORMAL })
            .style(if style.italic { Style::Italic } else { Style::Normal });
        buffer.set_size(None, None);
        buffer.set_text(text, &attrs, Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut self.fonts, false);

        let runs: Vec<(f32, f32, Vec<(CacheKey, f32, f32)>)> = buffer
            .layout_runs()
            .map(|run| {
                let glyphs = run
                    .glyphs
                    .iter()
                    .map(|g| {
                        let p = g.physical((0.0, 0.0), 1.0);
                        (p.cache_key, p.x as f32, p.y as f32)
                    })
                    .collect();
                (run.line_w, run.line_y, glyphs)
            })
            .collect();
        let total_h = runs.len() as f32 * line_h;
        let y0 = rect.y + ((rect.h - total_h) / 2.0).round();
        let mut n = 0;
        for (line_w, line_y, glyphs) in runs {
            let x0 = match align {
                Align::Left => rect.x,
                Align::Center => rect.x + ((rect.w - line_w) / 2.0).round(),
                Align::Right => rect.x + rect.w - line_w,
            }
            .round();
            for (key, gx, gy) in glyphs {
                let Some(p) = self.place(queue, key) else { continue };
                let r = Rect::new(x0 + gx + p.left as f32, y0 + line_y + gy - p.top as f32, p.w as f32, p.h as f32);
                let a = ATLAS as f32;
                emit(r, [p.x as f32 / a, p.y as f32 / a, (p.x + p.w) as f32 / a, (p.y + p.h) as f32 / a], style.color);
                n += 1;
            }
        }
        n
    }
}
