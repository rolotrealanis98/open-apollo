//! Text laid out the way UAD Console's JUCE renderer does it, rasterised into an R8 glyph atlas.
//!
//! * Shaping: cosmic-text (harfrust), cached per (text, style, scale): a frame re-uses the shaped
//!   glyph runs of every label it drew before and only positions them.
//! * Vertical placement (JUCE `GlyphArrangement::justifyGlyphs`): a line's box is the font height
//!   (ascent + descent) and is centred in the rect; the baseline sits `ascent` below the box top.
//!   Ascent and descent are those of the face Console asks for (CoreText metrics of Helvetica,
//!   Helvetica Neue, Arial, Lucida Grande), also when Linux substitutes a metric-compatible face.
//!   `\n` stacks lines one font height apart and centres the block.
//! * Horizontal placement: advance widths, trailing whitespace ignored when centred.
//! * Fitting (JUCE `drawFittedText`): a single line wider than its rect is squeezed horizontally
//!   down to 0.7, then cut with "..." (`GlyphArrangement::fitLineIntoSpace`).
//! * Rasterisation (swash): unhinted outlines like CoreGraphics, 4 horizontal subpixel positions,
//!   whole-pixel baselines, synthetic bold/italic when the family has no such face, and a small
//!   stem darkening for light-on-dark text (macOS font smoothing; see [`SMOOTHING`]).
//!
//! Console's fonts map to metric-compatible Linux faces (Helvetica → Nimbus Sans, Arial →
//! Liberation Sans); Console's bundled fonts (Oxygen …) are loaded from `Resources/fonts`.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::Arc;

use cosmic_text::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping, Style, Weight};
use houston::{Rect, Rgba};
use rack::{Align, TextStyle};
use swash::scale::{Render, ScaleContext, Source, StrikeWith, image::Content};
use swash::zeno::{Angle, Format, Transform, Vector};

const ATLAS: u32 = 2048;

/// JUCE `Font::getDefaultMinimumHorizontalScaleFactor()`.
pub const MIN_HORIZONTAL_SCALE: f32 = 0.7;

/// Stem darkening (outline embolden, device px per em px) for text lighter than mid grey.
/// macOS draws Console's light-on-dark labels with font smoothing, which thickens stems; dark
/// text on light backgrounds shows no measurable difference. Calibrated on refpair p1 with
/// tools/align/text_diff.py (ink mass ours/Console, labels of equal size): light text 0.76 → 1.0
/// at 0.018; dark text 1.006 either way.
pub const SMOOTHING: f32 = 0.018;

/// Ascent and descent (fraction of the em) of the faces Console names, as CoreText reports them
/// (macOS 27, CTFontGetAscent/Descent). JUCE's font height is their sum.
fn console_metrics(family: &str, bold: bool) -> Option<(f32, f32)> {
    Some(match family {
        "Helvetica" => (0.770, 0.230),
        "Helvetica Neue" if bold => (0.975, 0.217),
        "Helvetica Neue" => (0.952, 0.213),
        "Arial" => (0.905, 0.212),
        "Lucida Grande" => (0.967, 0.211),
        _ => return None,
    })
}

#[cfg(not(target_vendor = "apple"))]
fn family_for(name: &str) -> &'static str {
    match name {
        "Helvetica" | "Helvetica Neue" | "Lucida Grande" | "Segoe UI" => "Nimbus Sans",
        "Arial" => "Liberation Sans",
        "Oxygen" => "Oxygen",
        _ => "Nimbus Sans",
    }
}

/// macOS and iOS ship the faces Console asks for.
#[cfg(target_vendor = "apple")]
fn family_for(name: &str) -> &'static str {
    match name {
        "Helvetica" => "Helvetica",
        "Arial" => "Arial",
        "Oxygen" => "Oxygen",
        "Lucida Grande" => "Lucida Grande",
        _ => "Helvetica Neue",
    }
}

struct Placed {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    left: i32,
    top: i32,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct GlyphKey {
    font: cosmic_text::fontdb::ID,
    glyph: u16,
    size_bits: u32,
    /// Quarter-pixel horizontal phase.
    x_bin: u8,
    /// Horizontal scale × 256 (fitted text).
    hscale: u16,
    /// Embolden strength × 64 px.
    embolden: u16,
    italic: bool,
}

#[derive(Clone, Copy)]
struct Glyph {
    font: cosmic_text::fontdb::ID,
    id: u16,
    /// Pen x (px from the line start) and advance.
    x: f32,
    w: f32,
    y: f32,
    space: bool,
}

struct Line {
    glyphs: Vec<Glyph>,
    /// Advance width up to the end of the last glyph.
    width: f32,
    /// Advance width without trailing whitespace.
    ink_width: f32,
}

type StyleKey = (Arc<str>, u32, bool, bool, u32);

/// A shaped label, independent of where it is drawn.
struct Shaped {
    text: Box<str>,
    style: StyleKey,
    lines: Vec<Line>,
    dots: Line,
    em: f32,
    ascent: f32,
    descent: f32,
    synth_bold: bool,
    synth_italic: bool,
    last_used: u64,
}

/// Counters since the renderer was created.
#[derive(Clone, Copy, Debug, Default)]
pub struct TextStats {
    pub shaped: u64,
    pub reused: u64,
    pub layouts: usize,
    pub glyphs_rasterised: usize,
}

pub struct TextSystem {
    fonts: FontSystem,
    scale: ScaleContext,
    atlas: wgpu::Texture,
    bind: wgpu::BindGroup,
    glyphs: HashMap<GlyphKey, Option<Placed>>,
    shelf: (u32, u32, u32), // x, y, row height
    layouts: HashMap<u64, Shaped>,
    frame: u64,
    pub stats: TextStats,
}

fn layout_key(text: &str, style: &TextStyle, scale: f32) -> (u64, StyleKey) {
    let k = (style.family.clone(), style.size.to_bits(), style.bold, style.italic, scale.to_bits());
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    k.hash(&mut h);
    (h.finish(), k)
}

impl TextSystem {
    pub fn new(device: &wgpu::Device, layout: &wgpu::BindGroupLayout, sampler: &wgpu::Sampler, console_fonts: PathBuf) -> Self {
        let mut fonts = FontSystem::new();
        // fontdb scans system fonts on macOS/Windows/Linux only; iOS keeps them here.
        #[cfg(target_os = "ios")]
        fonts.db_mut().load_fonts_dir("/System/Library/Fonts");
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
        Self {
            fonts,
            scale: ScaleContext::new(),
            atlas,
            bind,
            glyphs: HashMap::new(),
            shelf: (0, 0, 0),
            layouts: HashMap::new(),
            frame: 0,
            stats: TextStats::default(),
        }
    }

    pub fn bind_group(&self) -> &wgpu::BindGroup {
        &self.bind
    }

    /// Call once per rendered frame: drops layouts unused for a few hundred frames.
    pub fn end_frame(&mut self) {
        self.frame += 1;
        if self.frame % 256 == 0 {
            let f = self.frame;
            self.layouts.retain(|_, s| f - s.last_used < 512);
        }
        self.stats.layouts = self.layouts.len();
    }

    fn shape_lines(&mut self, text: &str, attrs: &Attrs, em: f32) -> Vec<Line> {
        let mut buffer = Buffer::new(&mut self.fonts, Metrics::new(em, em));
        buffer.set_size(None, None);
        buffer.set_text(text, attrs, Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut self.fonts, false);
        let mut lines: Vec<Line> = buffer
            .layout_runs()
            .map(|run| {
                let glyphs: Vec<Glyph> = run
                    .glyphs
                    .iter()
                    .map(|g| Glyph {
                        font: g.font_id,
                        id: g.glyph_id,
                        x: g.x + g.font_size * g.x_offset,
                        w: g.w,
                        y: -g.font_size * g.y_offset,
                        space: run.text.get(g.start..g.end).is_some_and(|s| s.chars().all(char::is_whitespace)),
                    })
                    .collect();
                let width = glyphs.iter().map(|g| g.x + g.w).fold(0.0, f32::max);
                let ink_width = glyphs.iter().filter(|g| !g.space).map(|g| g.x + g.w).fold(0.0, f32::max);
                Line { glyphs, width, ink_width }
            })
            .collect();
        if lines.is_empty() {
            lines.push(Line { glyphs: Vec::new(), width: 0.0, ink_width: 0.0 });
        }
        lines
    }

    fn shape(&mut self, text: &str, style: &TextStyle, scale: f32, key: StyleKey) -> Shaped {
        self.stats.shaped += 1;
        let family = family_for(&style.family);
        let em = style.size * scale;
        let attrs = Attrs::new()
            .family(Family::Name(family))
            .weight(if style.bold { Weight::BOLD } else { Weight::NORMAL })
            .style(if style.italic { Style::Italic } else { Style::Normal });
        let lines = self.shape_lines(text, &attrs, em);
        let dots = self.shape_lines("...", &attrs, em).swap_remove(0);
        // The face actually chosen: synthesise what it lacks.
        let face = lines.iter().flat_map(|l| l.glyphs.first()).next().map(|g| g.font);
        let (mut synth_bold, mut synth_italic, mut face_metrics) = (false, false, None);
        if let Some(id) = face {
            if let Some(info) = self.fonts.db().face(id) {
                synth_bold = style.bold && info.weight.0 < 600;
                synth_italic = style.italic && info.style == cosmic_text::fontdb::Style::Normal;
            }
            if let Some(font) = self.fonts.get_font(id, Weight::NORMAL) {
                let m = font.as_swash().metrics(&[]);
                let upm = m.units_per_em.max(1) as f32;
                face_metrics = Some((m.ascent / upm, m.descent.abs() / upm));
            }
        }
        let (ascent, descent) = console_metrics(&style.family, style.bold).or(face_metrics).unwrap_or((0.8, 0.2));
        Shaped {
            text: text.into(),
            style: key,
            lines,
            dots,
            em,
            ascent: ascent * em,
            descent: descent * em,
            synth_bold,
            synth_italic,
            last_used: self.frame,
        }
    }

    fn place(&mut self, queue: &wgpu::Queue, key: GlyphKey) -> Option<&Placed> {
        if !self.glyphs.contains_key(&key) {
            let placed = self.rasterize(queue, key);
            self.glyphs.insert(key, placed);
        }
        self.glyphs.get(&key)?.as_ref()
    }

    fn rasterize(&mut self, queue: &wgpu::Queue, key: GlyphKey) -> Option<Placed> {
        self.stats.glyphs_rasterised += 1;
        let font = self.fonts.get_font(key.font, Weight::NORMAL)?;
        let mut scaler = self.scale.builder(font.as_swash()).size(f32::from_bits(key.size_bits)).hint(false).build();
        let hs = key.hscale as f32 / 256.0;
        let mut t = if (hs - 1.0).abs() > 1e-3 { Some(Transform::scale(hs, 1.0)) } else { None };
        if key.italic {
            let skew = Transform::skew(Angle::from_degrees(12.0), Angle::from_degrees(0.0));
            t = Some(t.map_or(skew, |t| t.then(&skew)));
        }
        let img = Render::new(&[Source::ColorOutline(0), Source::ColorBitmap(StrikeWith::BestFit), Source::Outline])
            .format(Format::Alpha)
            .offset(Vector::new(key.x_bin as f32 / 4.0, 0.0))
            .transform(t)
            .embolden(key.embolden as f32 / 64.0)
            .render(&mut scaler, key.glyph)?;
        let (w, h) = (img.placement.width, img.placement.height);
        if w == 0 || h == 0 {
            return None;
        }
        let mask: Vec<u8> = match img.content {
            Content::Mask => img.data,
            Content::SubpixelMask => img.data.chunks_exact(4).map(|p| p[1]).collect(),
            Content::Color => img.data.chunks_exact(4).map(|p| p[3]).collect(),
        };
        let (mut x, mut y, mut row) = self.shelf;
        if x + w + 1 > ATLAS {
            x = 0;
            y += row + 1;
            row = 0;
        }
        if y + h + 1 > ATLAS {
            return None; // atlas full (not expected for Console's label set)
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
        let (hash, key) = layout_key(text, style, scale);
        let shaped = match self.layouts.remove(&hash) {
            Some(s) if &*s.text == text && s.style == key => {
                self.stats.reused += 1;
                s
            }
            _ => self.shape(text, style, scale, key),
        };
        let n = self.emit_shaped(queue, &shaped, style.color, rect, align, emit);
        let mut shaped = shaped;
        shaped.last_used = self.frame;
        self.layouts.insert(hash, shaped);
        n
    }

    fn emit_shaped(&mut self, queue: &wgpu::Queue, s: &Shaped, color: Rgba, rect: Rect, align: Align, emit: &mut dyn FnMut(Rect, [f32; 4], Rgba)) -> usize {
        let embolden = {
            let luma = 0.2126 * color.r + 0.7152 * color.g + 0.0722 * color.b;
            let mut e = if luma > 0.5 { SMOOTHING * s.em } else { 0.0 };
            if s.synth_bold {
                e += s.em / 48.0;
            }
            (e * 64.0).round().clamp(0.0, u16::MAX as f32) as u16
        };
        let size_bits = s.em.to_bits();
        let mut n = 0;
        for p in plan(s, rect, align) {
            let hs_key = (p.hscale * 256.0).round() as u16;
            let glyphs = s.lines[p.line].glyphs[..p.count].iter().map(|g| (g, p.x0 + g.x * p.hscale));
            let dots = p.dots.into_iter().flat_map(|dx| s.dots.glyphs.iter().map(move |g| (g, dx + g.x * p.hscale)));
            for (g, gx) in glyphs.chain(dots) {
                if g.space {
                    continue;
                }
                let mut px = gx.floor();
                let mut x_bin = ((gx - px) * 4.0).round() as u8;
                if x_bin == 4 {
                    px += 1.0;
                    x_bin = 0;
                }
                let gk = GlyphKey { font: g.font, glyph: g.id, size_bits, x_bin, hscale: hs_key, embolden, italic: s.synth_italic };
                let Some(pl) = self.place(queue, gk) else { continue };
                let r = Rect::new(px + pl.left as f32, p.baseline + g.y.round() - pl.top as f32, pl.w as f32, pl.h as f32);
                let a = ATLAS as f32;
                emit(r, [pl.x as f32 / a, pl.y as f32 / a, (pl.x + pl.w) as f32 / a, (pl.y + pl.h) as f32 / a], color);
                n += 1;
            }
        }
        n
    }
}

/// Where one line of a label goes, in pixels.
#[derive(Debug, PartialEq)]
struct LinePlan {
    line: usize,
    hscale: f32,
    x0: f32,
    baseline: f32,
    /// Glyphs kept (ellipsis cuts the rest).
    count: usize,
    /// x of an appended "...".
    dots: Option<f32>,
}

fn plan(s: &Shaped, rect: Rect, align: Align) -> Vec<LinePlan> {
    let height = s.ascent + s.descent;
    let block = height * s.lines.len() as f32;
    let top = rect.y + (rect.h - block) / 2.0;
    let single = s.lines.len() == 1;
    s.lines
        .iter()
        .enumerate()
        .map(|(i, line)| {
            let baseline = (top + i as f32 * height + s.ascent).round();
            let mut width = if align == Align::Center { line.ink_width } else { line.width };
            let mut hscale = 1.0;
            let mut count = line.glyphs.len();
            let mut dots = None;
            if single && width > rect.w && rect.w > 0.0 {
                // GlyphArrangement::fitLineIntoSpace: squeeze, then cut with "...".
                hscale = (rect.w / width).max(MIN_HORIZONTAL_SCALE);
                if width * hscale > rect.w + 0.5 {
                    let dots_w = s.dots.width * hscale;
                    while count > 0 {
                        let g = &line.glyphs[count - 1];
                        if !g.space && (g.x + g.w) * hscale + dots_w <= rect.w {
                            break;
                        }
                        count -= 1;
                    }
                    let end = if count > 0 {
                        let g = &line.glyphs[count - 1];
                        (g.x + g.w) * hscale
                    } else {
                        0.0
                    };
                    dots = Some(end);
                    width = (end + dots_w) / hscale;
                }
            }
            let w = width * hscale;
            let x0 = match align {
                Align::Left => rect.x,
                Align::Center => rect.x + (rect.w - w) / 2.0,
                Align::Right => rect.x + rect.w - w,
            };
            LinePlan { line: i, hscale, x0, baseline, count, dots: dots.map(|d| x0 + d) }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shaped(widths: &[f32], lines: usize) -> Shaped {
        let mut x = 0.0;
        let glyphs: Vec<Glyph> = widths
            .iter()
            .map(|&w| {
                let g = Glyph { font: cosmic_text::fontdb::ID::dummy(), id: 1, x, w, y: 0.0, space: false };
                x += w;
                g
            })
            .collect();
        let line = || Line { glyphs: glyphs.clone(), width: x, ink_width: x };
        Shaped {
            text: "".into(),
            style: (Arc::from(""), 0, false, false, 0),
            lines: (0..lines).map(|_| line()).collect(),
            dots: Line { glyphs: vec![], width: 6.0, ink_width: 6.0 },
            em: 20.0,
            ascent: 15.4,
            descent: 4.6,
            synth_bold: false,
            synth_italic: false,
            last_used: 0,
        }
    }

    #[test]
    fn juce_vertical_centring() {
        // Helvetica 10 pt at 2x: height 20 px, ascent 15.4: the box is centred, baseline = top + ascent.
        let s = shaped(&[10.0; 3], 1);
        let p = plan(&s, Rect::new(0.0, 100.0, 200.0, 44.0), Align::Left);
        assert_eq!(p[0].baseline, (100.0 + 12.0 + 15.4f32).round());
        // Two lines: the 40-px block is centred.
        let s = shaped(&[10.0; 3], 2);
        let p = plan(&s, Rect::new(0.0, 0.0, 200.0, 60.0), Align::Center);
        assert_eq!(p[0].baseline, (10.0 + 15.4f32).round());
        assert_eq!(p[1].baseline, (30.0 + 15.4f32).round());
        assert_eq!(p[0].x0, 85.0);
    }

    #[test]
    fn fitted_squeeze_then_ellipsis() {
        let s = shaped(&[10.0; 10], 1); // 100 px wide
        let p = plan(&s, Rect::new(0.0, 0.0, 80.0, 20.0), Align::Center);
        assert!((p[0].hscale - 0.8).abs() < 1e-6 && p[0].count == 10 && p[0].dots.is_none());
        let p = plan(&s, Rect::new(0.0, 0.0, 50.0, 20.0), Align::Left);
        assert_eq!(p[0].hscale, MIN_HORIZONTAL_SCALE);
        // At 0.7: glyphs are 7 px, dots 4.2 px: 6 glyphs (42) + dots fit in 50.
        assert_eq!(p[0].count, 6);
        assert!((p[0].dots.unwrap() - 42.0).abs() < 1e-4);
        // Multi-line text is not fitted.
        let s = shaped(&[10.0; 10], 2);
        assert_eq!(plan(&s, Rect::new(0.0, 0.0, 50.0, 40.0), Align::Left)[0].hscale, 1.0);
    }
}
