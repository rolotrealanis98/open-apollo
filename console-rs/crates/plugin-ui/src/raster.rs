//! Code-drawn images for spec panels (tiny-skia): graph scenes (curves, fills, gradients,
//! overlays, handles), sprites rotated by `rotate`/`spin`, and the "more" button. The panel stores
//! them in the image cache under `runtime:` keys ([`houston::ImageCache::put_raster`]), so `gfx`
//! draws them as ordinary images and re-uploads only when their fingerprint changes.
//!
//! Paint follows SwiftUI: non-zero fills, round caps and joins, vertical gradients over the
//! graph's height, anti-aliasing, blending in the stored (sRGB-encoded) values.

use std::hash::{Hash, Hasher};

use houston::{ImageData, Rgba};
use tiny_skia::{Color, FillRule, LineCap, LineJoin, Mask, Paint, PathBuilder, Pixmap, PixmapPaint, Shader, Stroke, Transform};

use crate::graph::{FillItem, Paint as ScenePaint, Path, Scene, Seg};

/// A region in element-local points rasterized at `px` pixels per point.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Region {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub px: f64,
}

impl Region {
    pub fn size_px(&self) -> (u32, u32) {
        (((self.w * self.px).ceil().max(1.0)) as u32, ((self.h * self.px).ceil().max(1.0)) as u32)
    }

    fn transform(&self) -> Transform {
        let k = self.px as f32;
        Transform::from_row(k, 0.0, 0.0, k, (-self.x * self.px) as f32, (-self.y * self.px) as f32)
    }
}

/// Largest raster side in pixels (keeps a runaway region from allocating gigabytes).
const MAX_SIDE: u32 = 4096;

fn color(c: Rgba) -> Color {
    Color::from_rgba(c.r.clamp(0.0, 1.0), c.g.clamp(0.0, 1.0), c.b.clamp(0.0, 1.0), c.a.clamp(0.0, 1.0)).unwrap_or(Color::TRANSPARENT)
}

fn sk_path(p: &Path) -> Option<tiny_skia::Path> {
    let mut b = PathBuilder::new();
    let f = |v: f64| if v.is_finite() { v.clamp(-1e6, 1e6) as f32 } else { 0.0 };
    for s in &p.segs {
        match *s {
            Seg::Move(x, y) => b.move_to(f(x), f(y)),
            Seg::Line(x, y) => b.line_to(f(x), f(y)),
            Seg::Cubic(c) => b.cubic_to(f(c[0]), f(c[1]), f(c[2]), f(c[3]), f(c[4]), f(c[5])),
            Seg::Close => b.close(),
        }
    }
    b.finish()
}

fn shader(paint: &ScenePaint, height: f64) -> Option<Shader<'static>> {
    match paint {
        ScenePaint::Solid(c) => Some(Shader::SolidColor(color(*c))),
        ScenePaint::Vertical(stops) => {
            let stops: Vec<tiny_skia::GradientStop> = stops.iter().map(|(t, c)| tiny_skia::GradientStop::new(*t as f32, color(*c))).collect();
            tiny_skia::LinearGradient::new(tiny_skia::Point::from_xy(0.0, 0.0), tiny_skia::Point::from_xy(0.0, height.max(1e-3) as f32), stops, tiny_skia::SpreadMode::Pad, Transform::identity())
        }
    }
}

fn fill(pm: &mut Pixmap, item: &FillItem, height: f64, t: Transform, mask: Option<&Mask>) {
    let (Some(path), Some(shader)) = (sk_path(&item.path), shader(&item.paint, height)) else { return };
    let paint = Paint { shader, anti_alias: true, ..Default::default() };
    pm.fill_path(&path, &paint, FillRule::Winding, t, mask);
}

fn new_pixmap(region: &Region) -> Option<Pixmap> {
    let (w, h) = region.size_px();
    if w > MAX_SIDE || h > MAX_SIDE {
        return None;
    }
    Pixmap::new(w, h)
}

/// tiny-skia pixmaps are premultiplied; [`ImageData`] is straight RGBA.
fn image(pm: Pixmap) -> ImageData {
    let (width, height) = (pm.width(), pm.height());
    let mut rgba = pm.take();
    for p in rgba.chunks_exact_mut(4) {
        let a = p[3] as u32;
        if a > 0 && a < 255 {
            for c in &mut p[..3] {
                *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
    ImageData { width, height, rgba }
}

/// The scene's clipped fills and strokes, then its overlays (Swift draws them unclipped).
pub fn scene_curves(scene: &Scene, region: &Region) -> Option<ImageData> {
    let mut pm = new_pixmap(region)?;
    let t = region.transform();
    let (cx, cy, cw, ch) = scene.clip;
    let mut mask = Mask::new(pm.width(), pm.height())?;
    if let Some(clip) = sk_path(&Path::rect(cx, cy, cw.max(0.0), ch.max(0.0))) {
        mask.fill_path(&clip, FillRule::Winding, true, t);
    }
    for f in &scene.fills {
        fill(&mut pm, f, scene.height, t, Some(&mask));
    }
    for s in &scene.strokes {
        let Some(path) = sk_path(&s.path) else { continue };
        let paint = Paint { shader: Shader::SolidColor(color(s.color)), anti_alias: true, ..Default::default() };
        let stroke = Stroke { width: s.width.max(0.0) as f32, line_cap: LineCap::Round, line_join: LineJoin::Round, ..Default::default() };
        pm.stroke_path(&path, &paint, &stroke, t, Some(&mask));
    }
    for o in &scene.overlays {
        fill(&mut pm, o, scene.height, t, None);
    }
    Some(image(pm))
}

/// The scene's handle dots (labels are drawn as text above them by the panel).
pub fn scene_handles(scene: &Scene, region: &Region) -> Option<ImageData> {
    let mut pm = new_pixmap(region)?;
    let t = region.transform();
    for h in &scene.handles {
        let d = h.radius * 2.0;
        let ring = h.stroke.is_some() && h.stroke_width > 0.0;
        let inner = if ring { (h.radius - h.stroke_width).max(0.0) * 2.0 } else { d };
        let shape = |size: f64| {
            let r = if h.square { 1.0 } else { size / 2.0 };
            Path::rounded_rect(h.x - size / 2.0, h.y - size / 2.0, size, size, r)
        };
        if let (Some(stroke), true) = (h.stroke, ring) {
            fill(&mut pm, &FillItem { path: shape(d), paint: ScenePaint::Solid(stroke) }, scene.height, t, None);
        }
        fill(&mut pm, &FillItem { path: shape(inner), paint: ScenePaint::Solid(h.fill) }, scene.height, t, None);
    }
    Some(image(pm))
}

/// A content fingerprint of a scene's drawn parts (`handles` selects the handle layer).
pub fn scene_fingerprint(scene: &Scene, region: &Region, handles: bool) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let mut f = |v: f64| v.to_bits().hash(&mut h);
    for v in [region.x, region.y, region.w, region.h, region.px] {
        f(v);
    }
    let color = |c: Rgba, f: &mut dyn FnMut(f64)| {
        for v in [c.r, c.g, c.b, c.a] {
            f(v as f64);
        }
    };
    let paint = |p: &ScenePaint, f: &mut dyn FnMut(f64)| match p {
        ScenePaint::Solid(c) => color(*c, f),
        ScenePaint::Vertical(stops) => {
            for (t, c) in stops {
                f(*t);
                color(*c, f);
            }
        }
    };
    if handles {
        for s in &scene.handles {
            for v in [s.x, s.y, s.radius, s.stroke_width, s.square as u8 as f64] {
                f(v);
            }
            paint(&ScenePaint::Solid(s.fill), &mut f);
            if let Some(c) = s.stroke {
                paint(&ScenePaint::Solid(c), &mut f);
            }
        }
    } else {
        let (cx, cy, cw, ch) = scene.clip;
        for v in [cx, cy, cw, ch, scene.height] {
            f(v);
        }
        for item in scene.fills.iter().chain([&FillItem { path: Path::default(), paint: ScenePaint::Solid(Rgba::default()) }]).chain(&scene.overlays) {
            item.path.coords().for_each(&mut f);
            paint(&item.paint, &mut f);
        }
        for s in &scene.strokes {
            s.path.coords().for_each(&mut f);
            f(s.width);
            paint(&ScenePaint::Solid(s.color), &mut f);
        }
    }
    h.finish()
}

/// One frame (`rows` = (y, h) in image px) of a sprite drawn at `w`×`h` points, rotated by
/// `degrees` clockwise about its centre, into an image covering the rotated bounds (returned
/// with the bounds' size in points).
pub fn rotated_sprite(img: &ImageData, rows: (u32, u32), w: f64, h: f64, degrees: f64, px: f64) -> Option<(ImageData, f64, f64)> {
    let (y0, rh) = (rows.0.min(img.height), rows.1.min(img.height.saturating_sub(rows.0)));
    if img.width == 0 || rh == 0 || !(w > 0.0 && h > 0.0) {
        return None;
    }
    // The frame as a premultiplied pixmap.
    let start = (y0 * img.width * 4) as usize;
    let mut data = img.rgba[start..start + (img.width * rh * 4) as usize].to_vec();
    for p in data.chunks_exact_mut(4) {
        let a = p[3] as u32;
        for c in &mut p[..3] {
            *c = ((*c as u32 * a + 127) / 255) as u8;
        }
    }
    let src = Pixmap::from_vec(data, tiny_skia::IntSize::from_wh(img.width, rh)?)?;
    let rad = degrees.to_radians();
    let (bw, bh) = (w * rad.cos().abs() + h * rad.sin().abs(), w * rad.sin().abs() + h * rad.cos().abs());
    let region = Region { x: 0.0, y: 0.0, w: bw, h: bh, px };
    let mut pm = new_pixmap(&region)?;
    let k = px as f32;
    // src px → dst pt (w×h), centred at the origin, rotated, moved to the bounds' centre, → px.
    let t = Transform::from_translate(-(img.width as f32) / 2.0, -(rh as f32) / 2.0)
        .post_scale(w as f32 / img.width as f32, h as f32 / rh as f32)
        .post_rotate(degrees as f32)
        .post_translate(bw as f32 / 2.0, bh as f32 / 2.0)
        .post_scale(k, k);
    let paint = PixmapPaint { quality: tiny_skia::FilterQuality::Bicubic, ..Default::default() };
    pm.draw_pixmap(0, 0, src.as_ref(), &paint, t, None);
    Some((image(pm), bw, bh))
}

/// The "more" button: a 55 % black disc with three slider bars (SF `slider.horizontal.3`).
pub fn more_icon(diameter: f64, px: f64) -> Option<ImageData> {
    let region = Region { x: 0.0, y: 0.0, w: diameter, h: diameter, px };
    let mut pm = new_pixmap(&region)?;
    let t = region.transform();
    let disc = FillItem { path: Path::ellipse(0.0, 0.0, diameter, diameter), paint: ScenePaint::Solid(Rgba::BLACK.with_alpha(0.55)) };
    fill(&mut pm, &disc, diameter, t, None);
    let white = Rgba::WHITE;
    let (x0, x1) = (diameter * 0.27, diameter * 0.73);
    for (i, knob) in [0.62, 0.36, 0.55].into_iter().enumerate() {
        let y = diameter * (0.33 + 0.17 * i as f64);
        let bar = FillItem { path: Path::rounded_rect(x0, y - 0.6, x1 - x0, 1.2, 0.6), paint: ScenePaint::Solid(white) };
        fill(&mut pm, &bar, diameter, t, None);
        let kx = x0 + (x1 - x0) * knob;
        let dot = FillItem { path: Path::ellipse(kx - 1.6, y - 1.6, 3.2, 3.2), paint: ScenePaint::Solid(white) };
        fill(&mut pm, &dot, diameter, t, None);
    }
    Some(image(pm))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Axes, HandleSpot, StrokeItem};

    fn scene() -> Scene {
        let g: crate::spec::Graph = serde_json::from_str(r#"{"type": "eq"}"#).unwrap();
        Scene {
            axes: Axes::new(&g, 20.0, 10.0),
            clip: (0.0, 0.0, 10.0, 10.0),
            fills: vec![FillItem { path: Path::rect(0.0, 0.0, 20.0, 10.0), paint: ScenePaint::Solid(Rgba::rgb8(255, 0, 0)) }],
            strokes: vec![StrokeItem { path: Path::polyline(&[(0.0, 5.0), (20.0, 5.0)]), color: Rgba::WHITE, width: 2.0 }],
            overlays: vec![FillItem { path: Path::rect(15.0, 0.0, 5.0, 2.0), paint: ScenePaint::Solid(Rgba::rgb8(0, 0, 255)) }],
            labels: vec![],
            handles: vec![HandleSpot {
                x: 10.0,
                y: 5.0,
                radius: 3.0,
                stroke_width: 1.0,
                fill: Rgba::rgb8(0, 255, 0),
                stroke: Some(Rgba::BLACK),
                square: false,
                label: None,
                label_color: Rgba::BLACK,
                label_size: 9.0,
                x_value: None,
                y_value: None,
                draggable: false,
                touch_state: None,
            }],
            spectrum_points: vec![],
            width: 20.0,
            height: 10.0,
        }
    }

    fn px(img: &ImageData, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * img.width + x) * 4) as usize;
        [img.rgba[i], img.rgba[i + 1], img.rgba[i + 2], img.rgba[i + 3]]
    }

    #[test]
    fn curves_are_clipped_and_overlays_are_not() {
        let s = scene();
        let region = Region { x: 0.0, y: 0.0, w: 20.0, h: 10.0, px: 2.0 };
        let img = scene_curves(&s, &region).unwrap();
        assert_eq!((img.width, img.height), (40, 20));
        assert_eq!(px(&img, 4, 4), [255, 0, 0, 255], "fill inside the clip");
        assert_eq!(px(&img, 4, 10), [255, 255, 255, 255], "stroke over the fill");
        assert_eq!(px(&img, 26, 14)[3], 0, "fill outside the clip is cut");
        assert_eq!(px(&img, 34, 1), [0, 0, 255, 255], "overlays ignore the clip");
        let h = scene_handles(&s, &region).unwrap();
        assert_eq!(px(&h, 20, 10), [0, 255, 0, 255], "handle fill");
        assert_eq!(px(&h, 20, 5)[..3], [0, 0, 0], "ring");
        assert_ne!(scene_fingerprint(&s, &region, false), scene_fingerprint(&s, &region, true));
        let mut moved = s.clone();
        moved.strokes[0].width = 3.0;
        assert_ne!(scene_fingerprint(&s, &region, false), scene_fingerprint(&moved, &region, false));
        assert_eq!(scene_fingerprint(&s, &region, true), scene_fingerprint(&moved, &region, true));
    }

    #[test]
    fn gradients_run_over_the_graph_height() {
        let mut s = scene();
        s.strokes.clear();
        s.overlays.clear();
        s.clip = (0.0, 0.0, 20.0, 10.0);
        s.fills[0].paint = ScenePaint::Vertical(vec![(0.0, Rgba::rgb8(255, 0, 0)), (1.0, Rgba::rgb8(0, 0, 255))]);
        let img = scene_curves(&s, &Region { x: 0.0, y: 0.0, w: 20.0, h: 10.0, px: 1.0 }).unwrap();
        assert!(px(&img, 5, 0)[0] > 240 && px(&img, 5, 9)[2] > 220);
    }

    #[test]
    fn rotation_covers_the_rotated_bounds() {
        // A 2×4 px frame: left column red, right column green; drawn 10×20 pt, rotated 90°.
        let img = ImageData { width: 2, height: 4, rgba: [[255, 0, 0, 255], [0, 255, 0, 255]].repeat(4).concat() };
        let (out, bw, bh) = rotated_sprite(&img, (0, 4), 10.0, 20.0, 90.0, 1.0).unwrap();
        assert!((bw - 20.0).abs() < 1e-9 && (bh - 10.0).abs() < 1e-9);
        // Clockwise: the left (red) column ends up on top.
        assert!(px(&out, 10, 1)[0] > 200 && px(&out, 10, 8)[1] > 200);
        assert!(more_icon(21.0, 2.0).is_some());
    }
}
