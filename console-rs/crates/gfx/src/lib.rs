//! wgpu renderer for a `rack::DrawList`: premultiplied textured quads batched by texture and
//! clip, 9-slice expansion on the CPU, and text rasterised by cosmic-text into a glyph atlas
//! so text keeps its place in the draw order. Renders to a surface or offscreen to PNG.

pub mod draws;
pub mod surface;
mod text;

use std::collections::HashMap;
use std::sync::Arc;

use houston::{ImageCache, Insets, Rect, Rgba};
use rack::{Draw, DrawList, Frame};
use wgpu::util::DeviceExt;

pub use surface::SurfaceGpu;
pub use text::{TextStats, TextSystem};
pub use wgpu;
pub use wgpu::TextureFormat;

const ART_SCALE: f32 = 2.0;

/// Vertex `mode`: premultiplied image texel × tint, glyph-atlas coverage × colour, or flat colour.
const MODE_IMAGE: f32 = 0.0;
const MODE_GLYPH: f32 = 1.0;
const MODE_FILL: f32 = 2.0;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    pos: [f32; 2],
    uv: [f32; 2],
    color: [f32; 4],
    mode: f32,
}

/// The image texture a batch binds. Fills and text bind nothing of their own (the glyph atlas
/// sits next to every image texture), so they join whatever batch is open.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum TexId {
    /// Not chosen yet: the batch holds only fills and text so far.
    Any,
    Image(u32),
    /// A slot in `Renderer::resampled`.
    Resampled(u32),
}

/// An image (or filmstrip frame) area-resampled to the device-pixel size it is drawn at.
#[derive(Clone, PartialEq, Eq, Hash)]
struct ResampleKey {
    key: Arc<str>,
    /// Source rows (y, h) in image px.
    rows: (u32, u32),
    size: (u32, u32),
}

struct Batch {
    tex: TexId,
    scissor: Option<[u32; 4]>,
    range: std::ops::Range<u32>,
}

struct GpuTexture {
    bind: wgpu::BindGroup,
    size: (u32, u32),
    /// For a whole filmstrip uploaded as one texture: None. For a single uploaded frame: its rows.
    frame_rows: Option<(u32, u32)>,
}

/// Stats from the last frame.
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameStats {
    pub quads: usize,
    pub batches: usize,
    pub textures: usize,
    pub texture_bytes: usize,
    pub glyphs: usize,
}

pub struct Renderer {
    pipeline: wgpu::RenderPipeline,
    globals: wgpu::Buffer,
    globals_bind: wgpu::BindGroup,
    tex_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    white: wgpu::BindGroup,
    vbuf: Option<wgpu::Buffer>,
    images: Arc<ImageCache>,
    textures: HashMap<(Arc<str>, Option<Frame>), Option<u32>>,
    gpu_textures: Vec<GpuTexture>,
    resampled: HashMap<ResampleKey, (u32, u64)>,
    resampled_slots: Vec<Option<GpuTexture>>,
    frame_no: u64,
    max_dim: u32,
    pub text: TextSystem,
    pub stats: FrameStats,
}

impl Renderer {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, format: wgpu::TextureFormat, images: Arc<ImageCache>) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("rack"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });
        let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("globals"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                count: None,
            }],
        });
        let tex_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("tex"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("rack"),
            bind_group_layouts: &[Some(&globals_layout), Some(&tex_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("rack"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x4, 3 => Float32],
                })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("globals"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let globals_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("globals"),
            layout: &globals_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: globals.as_entire_binding() }],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("linear"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        let text = TextSystem::new(device, images.resources().fonts_dir());
        let white_tex = upload_rgba(device, queue, 1, 1, &[255, 255, 255, 255]);
        let white = bind_texture(device, &tex_layout, &white_tex, &sampler, text.atlas_view());
        Self {
            pipeline,
            globals,
            globals_bind,
            tex_layout,
            sampler,
            white,
            vbuf: None,
            images,
            textures: HashMap::new(),
            gpu_textures: Vec::new(),
            resampled: HashMap::new(),
            resampled_slots: Vec::new(),
            frame_no: 0,
            max_dim: device.limits().max_texture_dimension_2d,
            text,
            stats: FrameStats::default(),
        }
    }

    /// Texture for an image (or one filmstrip frame when the strip exceeds the GPU limit).
    fn texture(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, key: &Arc<str>, frame: Option<Frame>) -> Option<u32> {
        let whole = (key.clone(), None);
        if let Some(Some(id)) = self.textures.get(&whole) {
            return Some(*id);
        }
        if let Some(f) = frame {
            if let Some(Some(id)) = self.textures.get(&(key.clone(), Some(f))) {
                return Some(*id);
            }
        }
        if self.textures.get(&whole) == Some(&None) && frame.is_none() {
            return None;
        }
        let img = self.images.get(key)?;
        let (data, w, h, rows, cache_key) = if img.height <= self.max_dim {
            (premultiply(&img.rgba), img.width, img.height, None, whole)
        } else {
            let f = frame?;
            let start = (f.index * f.height) as usize * img.width as usize * 4;
            let end = start + (f.height as usize * img.width as usize * 4);
            let slice = img.rgba.get(start..end)?;
            (premultiply(slice), img.width, f.height, Some((f.index * f.height, f.height)), (key.clone(), Some(f)))
        };
        let tex = upload_rgba(device, queue, w, h, &data);
        let bind = bind_texture(device, &self.tex_layout, &tex, &self.sampler, self.text.atlas_view());
        self.stats.texture_bytes += data.len();
        self.gpu_textures.push(GpuTexture { bind, size: (w, h), frame_rows: rows });
        let id = (self.gpu_textures.len() - 1) as u32;
        self.textures.insert(cache_key, Some(id));
        Some(id)
    }

    /// Texture slot holding `rows` of image `key` box-filtered to `size` px (see
    /// [`houston::ImageData::resample_area_premul`]): drawn 1:1, a minified image looks like
    /// CoreGraphics' high-quality downscale instead of aliased bilinear taps.
    fn resampled(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, key: &Arc<str>, rows: (u32, u32), size: (u32, u32)) -> Option<u32> {
        let rk = ResampleKey { key: key.clone(), rows, size };
        if let Some((slot, used)) = self.resampled.get_mut(&rk) {
            *used = self.frame_no;
            return Some(*slot);
        }
        let img = self.images.get(key)?;
        let data = img.resample_area_premul((0, rows.0, img.width, rows.1), size.0, size.1);
        let tex = upload_rgba(device, queue, size.0, size.1, &data);
        let bind = bind_texture(device, &self.tex_layout, &tex, &self.sampler, self.text.atlas_view());
        let gt = GpuTexture { bind, size, frame_rows: None };
        let slot = match self.resampled_slots.iter().position(Option::is_none) {
            Some(i) => {
                self.resampled_slots[i] = Some(gt);
                i
            }
            None => {
                self.resampled_slots.push(Some(gt));
                self.resampled_slots.len() - 1
            }
        } as u32;
        self.resampled.insert(rk, (slot, self.frame_no));
        Some(slot)
    }

    /// Drops resampled textures not drawn for a while (zooming leaves many sizes behind).
    fn evict_resampled(&mut self) {
        self.frame_no += 1;
        if self.frame_no % 128 != 0 {
            return;
        }
        let now = self.frame_no;
        let slots = &mut self.resampled_slots;
        self.resampled.retain(|_, (slot, used)| {
            let keep = now - *used < 600;
            if !keep {
                slots[*slot as usize] = None;
            }
            keep
        });
    }

    /// Renders `list` (window points) into `target` of `size_px`, at `scale` px per point.
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        list: &DrawList,
        target: &wgpu::TextureView,
        size_px: (u32, u32),
        scale: f32,
        clear: Rgba,
    ) {
        self.render_at(device, queue, list, target, size_px, scale, (0.0, 0.0), clear);
    }

    /// [`Renderer::render`] with the list's origin placed at `offset` pixels in the target
    /// (letterboxing, panning). Areas the list does not cover are cleared to `clear`. Whole-pixel
    /// offsets keep art and text crisp.
    #[allow(clippy::too_many_arguments)]
    pub fn render_at(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        list: &DrawList,
        target: &wgpu::TextureView,
        size_px: (u32, u32),
        scale: f32,
        offset: (f32, f32),
        clear: Rgba,
    ) {
        let scaled = |r: Rect, s: f32| Rect::new(r.x * s + offset.0, r.y * s + offset.1, r.w * s, r.h * s);
        let mut verts: Vec<Vertex> = Vec::with_capacity(list.len() * 6);
        let mut batches: Vec<Batch> = Vec::new();
        let mut scissor: Option<[u32; 4]> = None;
        let mut quads = 0usize;
        let mut glyphs = 0usize;

        let push_batch = |batches: &mut Vec<Batch>, tex: TexId, scissor: Option<[u32; 4]>, start: u32, end: u32| {
            if end == start {
                return;
            }
            if let Some(last) = batches.last_mut() {
                if last.scissor == scissor && last.range.end == start {
                    match (last.tex, tex) {
                        (_, TexId::Any) => {}
                        (TexId::Any, t) => last.tex = t,
                        (a, b) if a == b => {}
                        _ => {
                            batches.push(Batch { tex, scissor, range: start..end });
                            return;
                        }
                    }
                    last.range.end = end;
                    return;
                }
            }
            batches.push(Batch { tex, scissor, range: start..end });
        };

        for item in &list.items {
            match item {
                Draw::Clip(r) => {
                    scissor = r.map(|r| {
                        let r = scaled(r, scale);
                        let x0 = r.x.max(0.0).floor() as u32;
                        let y0 = r.y.max(0.0).floor() as u32;
                        let x1 = (r.max_x().max(0.0).ceil() as u32).min(size_px.0);
                        let y1 = (r.max_y().max(0.0).ceil() as u32).min(size_px.1);
                        [x0.min(size_px.0), y0.min(size_px.1), x1.saturating_sub(x0), y1.saturating_sub(y0)]
                    });
                }
                Draw::Fill { rect, color } => {
                    let start = verts.len() as u32;
                    quad(&mut verts, snap_edges(scaled(*rect, scale)), [0.0, 0.0, 1.0, 1.0], *color, MODE_FILL);
                    quads += 1;
                    push_batch(&mut batches, TexId::Any, scissor, start, verts.len() as u32);
                }
                Draw::Image { key, frame, dst, slice, tint } => {
                    let Some(id) = self.texture(device, queue, key, *frame) else { continue };
                    let t = &self.gpu_textures[id as usize];
                    let (tw, th) = (t.size.0 as f32, t.size.1 as f32);
                    // Source rect in texture pixels.
                    let src = match (frame, t.frame_rows) {
                        (Some(f), None) => Rect::new(0.0, (f.index * f.height) as f32, tw, f.height as f32),
                        _ => Rect::new(0.0, 0.0, tw, th),
                    };
                    // Source rows in image px (frames of split strips live in their own texture).
                    let rows = match (frame, t.frame_rows) {
                        (_, Some(r)) => r,
                        (Some(f), None) => (f.index * f.height, f.height),
                        (None, None) => (0, t.size.1),
                    };
                    let start = verts.len() as u32;
                    let dst = if slice.is_some() { snap_out(scaled(*dst, scale)) } else { snap(scaled(*dst, scale)) };
                    if dst.w < 0.5 || dst.h < 0.5 {
                        continue;
                    }
                    let mut tex = TexId::Image(id);
                    match slice {
                        Some(ins) => {
                            // Corners keep inset × scale/2 px; below 1:1 the whole source is
                            // box-filtered by that factor first.
                            let f = scale / ART_SCALE;
                            let size = ((src.w * f).round().max(1.0) as u32, (src.h * f).round().max(1.0) as u32);
                            let res = if f < 0.98 { self.resampled(device, queue, key, rows, size) } else { None };
                            match res {
                                Some(slot) => {
                                    tex = TexId::Resampled(slot);
                                    let (rw, rh) = (size.0 as f32, size.1 as f32);
                                    let k = (rw / src.w, rh / src.h);
                                    let ins = Insets { l: ins.l * k.0, r: ins.r * k.0, t: ins.t * k.1, b: ins.b * k.1 };
                                    quads += nine_slice(&mut verts, dst, Rect::new(0.0, 0.0, rw, rh), (rw, rh), ins, 1.0, *tint);
                                }
                                None => quads += nine_slice(&mut verts, dst, src, (tw, th), *ins, f, *tint),
                            }
                        }
                        None => {
                            let size = (dst.w.round() as u32, dst.h.round() as u32);
                            let minified = (size.0 as f32) < src.w - 0.5 || (size.1 as f32) < src.h - 0.5;
                            match if minified { self.resampled(device, queue, key, rows, size) } else { None } {
                                Some(slot) => {
                                    tex = TexId::Resampled(slot);
                                    quad(&mut verts, dst, [0.0, 0.0, 1.0, 1.0], *tint, MODE_IMAGE);
                                }
                                None => quad(&mut verts, dst, [src.x / tw, src.y / th, src.max_x() / tw, src.max_y() / th], *tint, 0.0),
                            }
                            quads += 1;
                        }
                    }
                    push_batch(&mut batches, tex, scissor, start, verts.len() as u32);
                }
                Draw::Text { text, style, rect, align } => {
                    let start = verts.len() as u32;
                    glyphs += self.text.layout(device, queue, text, style, scaled(*rect, scale), *align, scale, &mut |r, uv, color| {
                        quad(&mut verts, r, uv, color, MODE_GLYPH);
                    });
                    push_batch(&mut batches, TexId::Any, scissor, start, verts.len() as u32);
                }
            }
        }

        queue.write_buffer(&self.globals, 0, bytemuck::cast_slice(&[size_px.0 as f32, size_px.1 as f32, 0.0, 0.0]));
        // One vertex buffer, grown by doubling and rewritten each frame.
        let bytes: &[u8] = if verts.is_empty() { &[0u8; 64] } else { bytemuck::cast_slice(&verts) };
        if self.vbuf.as_ref().is_none_or(|b| b.size() < bytes.len() as u64) {
            let size = (bytes.len() as u64).next_power_of_two().max(1 << 16);
            self.vbuf = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("quads"),
                size,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        let vbuf = self.vbuf.clone().expect("vertex buffer");
        queue.write_buffer(&vbuf, 0, bytes);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("rack") });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("rack"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color { r: clear.r as f64, g: clear.g as f64, b: clear.b as f64, a: clear.a as f64 }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.globals_bind, &[]);
            pass.set_vertex_buffer(0, vbuf.slice(..bytes.len() as u64));
            for b in &batches {
                match b.scissor {
                    Some([x, y, w, h]) if w > 0 && h > 0 => pass.set_scissor_rect(x, y, w, h),
                    Some(_) => continue,
                    None => pass.set_scissor_rect(0, 0, size_px.0, size_px.1),
                }
                let bind = match b.tex {
                    TexId::Any => &self.white,
                    TexId::Image(id) => &self.gpu_textures[id as usize].bind,
                    TexId::Resampled(slot) => match &self.resampled_slots[slot as usize] {
                        Some(t) => &t.bind,
                        None => continue,
                    },
                };
                pass.set_bind_group(1, bind, &[]);
                pass.draw(b.range.clone(), 0..1);
            }
        }
        queue.submit([encoder.finish()]);
        self.text.end_frame();
        self.evict_resampled();
        let textures = self.gpu_textures.len() + self.resampled.len();
        self.stats = FrameStats { quads, batches: batches.len(), textures, texture_bytes: self.stats.texture_bytes, glyphs };
    }
}

/// Device-pixel snapping for plain images, after Rack.dll's `GUI_Images` DrawImage
/// (0x181c1de70): the image covers `ceil(size × scale)` device pixels, drawn 1:1 from a cached
/// resample when that differs from the source. The origin rounds to the nearest pixel.
fn snap(r: Rect) -> Rect {
    let (x, y) = (r.x.round(), r.y.round());
    Rect::new(x, y, (r.w - 0.01).ceil().max(0.0), (r.h - 0.01).ceil().max(0.0))
}

/// 9-slice destinations (Rack.dll 0x181c237c0): `(int)(x·s)` to `ceilf((x+w)·s)`.
fn snap_out(r: Rect) -> Rect {
    let (x0, y0) = ((r.x + 0.01).floor(), (r.y + 0.01).floor());
    let (x1, y1) = ((r.max_x() - 0.01).ceil(), (r.max_y() - 0.01).ceil());
    Rect::new(x0, y0, (x1 - x0).max(0.0), (y1 - y0).max(0.0))
}

/// Fills: both edges round to pixels, so adjacent fills meet without seams or overlap.
fn snap_edges(r: Rect) -> Rect {
    let (x0, y0, x1, y1) = (r.x.round(), r.y.round(), r.max_x().round(), r.max_y().round());
    Rect::new(x0, y0, x1 - x0, y1 - y0)
}

fn quad(v: &mut Vec<Vertex>, r: Rect, uv: [f32; 4], c: Rgba, mode: f32) {
    let color = [c.r, c.g, c.b, c.a];
    let (x0, y0, x1, y1) = (r.x, r.y, r.max_x(), r.max_y());
    let (u0, v0, u1, v1) = (uv[0], uv[1], uv[2], uv[3]);
    let p = |x, y, u, vv| Vertex { pos: [x, y], uv: [u, vv], color, mode };
    v.extend_from_slice(&[p(x0, y0, u0, v0), p(x1, y0, u1, v0), p(x0, y1, u0, v1), p(x0, y1, u0, v1), p(x1, y0, u1, v0), p(x1, y1, u1, v1)]);
}

/// 9-slice: corners keep `inset × px_per_art_px` size, edges and centre stretch.
fn nine_slice(v: &mut Vec<Vertex>, dst: Rect, src: Rect, tex: (f32, f32), ins: Insets, px_per_art_px: f32, tint: Rgba) -> usize {
    let mut dl = ins.l * px_per_art_px;
    let mut dr = ins.r * px_per_art_px;
    let mut dt = ins.t * px_per_art_px;
    let mut db = ins.b * px_per_art_px;
    // Shrink corners proportionally when the destination is smaller than them.
    if dl + dr > dst.w && dl + dr > 0.0 {
        let k = dst.w / (dl + dr);
        dl *= k;
        dr *= k;
    }
    if dt + db > dst.h && dt + db > 0.0 {
        let k = dst.h / (dt + db);
        dt *= k;
        db *= k;
    }
    let xs = [dst.x, dst.x + dl, dst.max_x() - dr, dst.max_x()];
    let ys = [dst.y, dst.y + dt, dst.max_y() - db, dst.max_y()];
    let us = [src.x, src.x + ins.l, src.max_x() - ins.r, src.max_x()].map(|u| u / tex.0);
    let vs = [src.y, src.y + ins.t, src.max_y() - ins.b, src.max_y()].map(|vv| vv / tex.1);
    let mut n = 0;
    for j in 0..3 {
        for i in 0..3 {
            let r = Rect::new(xs[i], ys[j], xs[i + 1] - xs[i], ys[j + 1] - ys[j]);
            if r.w <= 0.0 || r.h <= 0.0 {
                continue;
            }
            quad(v, r, [us[i], vs[j], us[i + 1], vs[j + 1]], tint, MODE_IMAGE);
            n += 1;
        }
    }
    n
}

fn premultiply(rgba: &[u8]) -> Vec<u8> {
    let mut out = rgba.to_vec();
    for p in out.chunks_exact_mut(4) {
        let a = p[3] as u16;
        p[0] = ((p[0] as u16 * a + 127) / 255) as u8;
        p[1] = ((p[1] as u16 * a + 127) / 255) as u8;
        p[2] = ((p[2] as u16 * a + 127) / 255) as u8;
    }
    out
}

fn upload_rgba(device: &wgpu::Device, queue: &wgpu::Queue, w: u32, h: u32, data: &[u8]) -> wgpu::Texture {
    device.create_texture_with_data(
        queue,
        &wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        },
        wgpu::util::TextureDataOrder::LayerMajor,
        data,
    )
}

fn bind_texture(device: &wgpu::Device, layout: &wgpu::BindGroupLayout, tex: &wgpu::Texture, sampler: &wgpu::Sampler, atlas: &wgpu::TextureView) -> wgpu::BindGroup {
    let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(sampler) },
            wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(atlas) },
        ],
    })
}

/// Headless GPU for offscreen renders (`uaconsole --render`, tests).
pub struct Headless {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub adapter_info: wgpu::AdapterInfo,
}

impl Headless {
    pub fn new() -> Option<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..Default::default()
        }))
        .ok()?;
        let adapter_info = adapter.get_info();
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("uaconsole"),
            required_limits: adapter.limits(),
            ..Default::default()
        }))
        .ok()?;
        Some(Self { device, queue, adapter_info })
    }

    /// Renders `list` at `scale` into an RGBA8 PNG, plus `<name>.draws.json` (see [`draws`]).
    pub fn render_png(&self, renderer: &mut Renderer, list: &DrawList, size_pt: (f32, f32), scale: f32, path: &std::path::Path) -> std::io::Result<()> {
        let (w, h) = ((size_pt.0 * scale).ceil() as u32, (size_pt.1 * scale).ceil() as u32);
        let tex = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("offscreen"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        renderer.render(&self.device, &self.queue, list, &view, (w, h), scale, Rgba::BLACK);
        let bytes_per_row = (w * 4).div_ceil(256) * 256;
        let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: (bytes_per_row * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo { texture: &tex, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyBufferInfo { buffer: &buf, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(bytes_per_row), rows_per_image: Some(h) } },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        self.queue.submit([enc.finish()]);
        let slice = buf.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        self.device.poll(wgpu::PollType::wait_indefinitely()).map_err(std::io::Error::other)?;
        let data = slice.get_mapped_range().map_err(|e| std::io::Error::other(format!("{e:?}")))?;
        let mut pixels = Vec::with_capacity((w * h * 4) as usize);
        for row in 0..h {
            let start = (row * bytes_per_row) as usize;
            pixels.extend_from_slice(&data[start..start + (w * 4) as usize]);
        }
        drop(data);
        buf.unmap();
        let file = std::fs::File::create(path)?;
        let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().map_err(std::io::Error::other)?.write_image_data(&pixels).map_err(std::io::Error::other)?;
        // What was drawn where, for console-rs/tools/align (image paths and labels only).
        std::fs::write(draws::sidecar_path(path), draws::draws_json(list, size_pt, scale))?;
        Ok(())
    }
}

#[cfg(test)]
mod snap_tests {
    use super::*;

    #[test]
    fn image_snapping() {
        // 2x: half-point art lands on whole pixels unchanged.
        assert_eq!(snap(Rect::new(975.0, 1242.0, 96.0, 152.0)), Rect::new(975.0, 1242.0, 96.0, 152.0));
        // Fractional (fader cap at 0.25 pt): origin rounds, size stays whole.
        assert_eq!(snap(Rect::new(10.5, 20.4, 96.0, 152.0)), Rect::new(11.0, 20.0, 96.0, 152.0));
        // Non-integer zoom: size is ceil(w·s).
        assert_eq!(snap(Rect::new(3.3, 0.0, 65.76, 26.3)), Rect::new(3.0, 0.0, 66.0, 27.0));
        // 9-slice: floor/ceil of the edges.
        assert_eq!(snap_out(Rect::new(3.3, 1.7, 10.0, 10.0)), Rect::new(3.0, 1.0, 11.0, 11.0));
        assert_eq!(snap_out(Rect::new(4.0, 2.0, 10.0, 10.0)), Rect::new(4.0, 2.0, 10.0, 10.0));
        assert_eq!(snap_edges(Rect::new(0.4, 0.6, 10.2, 10.0)), Rect::new(0.0, 1.0, 11.0, 10.0));
    }
}
