//! Code-drawn schema-2 widgets (RUNTIME-V2.md): the plug-in shell toolbar's ring knobs and
//! glyphs, `shape` primitives and `lamp` LEDs, rasterised with tiny-skia into `runtime:` images
//! (like [`crate::raster`]'s graph scenes). Everything here is our own drawing from facts
//! (positions, colours, sizes); UA art only enters through recipe-id sprites the panel draws.

use std::hash::{Hash, Hasher};

use houston::{ImageData, Rgba};
use tiny_skia::{
    Color, FillRule, GradientStop, LineCap, LineJoin, LinearGradient, Paint, PathBuilder, Pixmap, Point,
    RadialGradient, Shader, SpreadMode, Stroke, StrokeDash, Transform,
};

use crate::graph::parse_hex;
use crate::spec::{Fill, Lamp, Ring, Shape, ShapeKind, ToolbarFamily};

/// Largest raster side in pixels.
const MAX_SIDE: u32 = 4096;

/// A family's default look: bar fill, ring track and ring value colours, glyph ink.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FamilyLook {
    pub bar: Rgba,
    pub track: Rgba,
    pub accent: Rgba,
    pub ink: Rgba,
}

/// Defaults measured on the live editors' toolbars (agent F's references): Brainworx / Plugin
/// Alliance draw a black strip with light-grey ink and a blue accent; Softube a #444444 strip.
pub fn family_look(family: ToolbarFamily) -> FamilyLook {
    match family {
        ToolbarFamily::Softube => FamilyLook {
            bar: Rgba::rgb8(0x44, 0x44, 0x44),
            track: Rgba::rgb8(0x9a, 0x9a, 0x9a),
            accent: Rgba::rgb8(0x00, 0xc0, 0x8c),
            ink: Rgba::rgb8(0xf0, 0xf0, 0xf0),
        },
        _ => FamilyLook {
            bar: Rgba::BLACK,
            track: Rgba::rgb8(0xb4, 0xb4, 0xb4),
            accent: Rgba::rgb8(0x0d, 0x75, 0x9f),
            ink: Rgba::rgb8(0xd2, 0xd2, 0xd2),
        },
    }
}

fn color(c: Rgba) -> Color {
    Color::from_rgba(c.r.clamp(0.0, 1.0), c.g.clamp(0.0, 1.0), c.b.clamp(0.0, 1.0), c.a.clamp(0.0, 1.0))
        .unwrap_or(Color::TRANSPARENT)
}

/// Linear blend `a` → `b` by `t` (stored values, like SwiftUI's).
pub fn mix(a: Rgba, b: Rgba, t: f32) -> Rgba {
    let t = t.clamp(0.0, 1.0);
    Rgba { r: a.r + (b.r - a.r) * t, g: a.g + (b.g - a.g) * t, b: a.b + (b.b - a.b) * t, a: a.a + (b.a - a.a) * t }
}

/// A pixmap for a `w × h`-point region at `px` pixels per point, and the point → pixel transform.
fn canvas(w: f64, h: f64, px: f64) -> Option<(Pixmap, Transform)> {
    let (pw, ph) = ((w * px).ceil().max(1.0) as u32, (h * px).ceil().max(1.0) as u32);
    if pw > MAX_SIDE || ph > MAX_SIDE {
        return None;
    }
    Some((Pixmap::new(pw, ph)?, Transform::from_scale(px as f32, px as f32)))
}

/// tiny-skia pixmaps are premultiplied; [`ImageData`] is straight RGBA.
fn image(pm: Pixmap) -> ImageData {
    let (width, height) = (pm.width(), pm.height());
    let mut rgba = pm.take();
    for p in rgba.as_chunks_mut::<4>().0 {
        let a = p[3] as u32;
        if a > 0 && a < 255 {
            for c in &mut p[..3] {
                *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
    ImageData { width, height, rgba }
}

/// A point at `deg` clockwise from up on a circle.
fn polar(cx: f64, cy: f64, r: f64, deg: f64) -> (f32, f32) {
    let a = deg.to_radians();
    ((cx + r * a.sin()) as f32, (cy - r * a.cos()) as f32)
}

/// An arc from `from` to `to` degrees (clockwise from up) as a polyline (1° steps).
fn arc_path(cx: f64, cy: f64, r: f64, from: f64, to: f64) -> Option<tiny_skia::Path> {
    if to <= from || r <= 0.0 {
        return None;
    }
    let mut b = PathBuilder::new();
    let n = ((to - from).ceil() as usize).max(2);
    for i in 0..=n {
        let (x, y) = polar(cx, cy, r, from + (to - from) * i as f64 / n as f64);
        if i == 0 { b.move_to(x, y) } else { b.line_to(x, y) }
    }
    b.finish()
}

fn stroke_with(pm: &mut Pixmap, path: &tiny_skia::Path, c: Rgba, width: f64, cap: LineCap, t: Transform) {
    let paint = Paint { shader: Shader::SolidColor(color(c)), anti_alias: true, ..Default::default() };
    let stroke =
        Stroke { width: width.max(0.0) as f32, line_cap: cap, line_join: LineJoin::Round, ..Default::default() };
    pm.stroke_path(path, &paint, &stroke, t, None);
}

fn fill_with(pm: &mut Pixmap, path: &tiny_skia::Path, shader: Shader, t: Transform) {
    let paint = Paint { shader, anti_alias: true, ..Default::default() };
    pm.fill_path(path, &paint, FillRule::Winding, t, None);
}

/// The ring's two arcs, `start` and the full sweep in degrees clockwise from up.
pub fn ring_sweep(ring: &Ring) -> (f64, f64) {
    let gap = ring.gap.unwrap_or(70.0).clamp(0.0, 359.0) as f64;
    (180.0 + gap / 2.0, 360.0 - gap)
}

/// A toolbar knob in its `w × h` item box: the track over the whole sweep, then the value arc
/// from the start clockwise (butt ends, as the shell draws them).
pub fn ring_image(ring: &Ring, value: f64, w: f64, h: f64, track: Rgba, accent: Rgba, px: f64) -> Option<ImageData> {
    let (mut pm, t) = canvas(w, h, px)?;
    let (start, sweep) = ring_sweep(ring);
    let (cx, cy, r) = (ring.cx as f64, ring.cy as f64, ring.r as f64);
    if let Some(p) = arc_path(cx, cy, r, start, start + sweep) {
        stroke_with(&mut pm, &p, track, ring.width as f64, LineCap::Butt, t);
    }
    let v = value.clamp(0.0, 1.0);
    if v > 0.0
        && let Some(p) = arc_path(cx, cy, r, start, start + sweep * v)
    {
        stroke_with(&mut pm, &p, accent, ring.width as f64, LineCap::Butt, t);
    }
    Some(image(pm))
}

/// Fallback glyphs for toolbar icons whose art is not available, and the Softube gear:
/// `power`, `undo`, `redo`, `gear`. None for an unknown name (the caller draws it as text).
pub fn glyph_image(name: &str, w: f64, h: f64, ink: Rgba, px: f64) -> Option<ImageData> {
    let (mut pm, t) = canvas(w, h, px)?;
    let (cx, cy) = (w / 2.0, h / 2.0);
    let s = w.min(h);
    let line = (s * 0.09).max(1.0);
    match name {
        "power" => {
            let r = s * 0.28;
            if let Some(p) = arc_path(cx, cy, r, 35.0, 325.0) {
                stroke_with(&mut pm, &p, ink, line, LineCap::Round, t);
            }
            let mut b = PathBuilder::new();
            b.move_to(cx as f32, (cy - r * 1.15) as f32);
            b.line_to(cx as f32, (cy - r * 0.2) as f32);
            if let Some(p) = b.finish() {
                stroke_with(&mut pm, &p, ink, line, LineCap::Round, t);
            }
        }
        "undo" | "redo" => {
            // A hook: a horizontal stroke turning back, with an arrow head at its free end.
            let dir = if name == "undo" { -1.0 } else { 1.0 };
            let (hw, hh) = (s * 0.26, s * 0.14);
            let mut b = PathBuilder::new();
            b.move_to((cx - dir * hw) as f32, (cy - hh) as f32);
            b.line_to((cx + dir * hw * 0.5) as f32, (cy - hh) as f32);
            b.quad_to((cx + dir * hw * 1.25) as f32, (cy - hh) as f32, (cx + dir * hw * 1.25) as f32, cy as f32);
            b.quad_to((cx + dir * hw * 1.25) as f32, (cy + hh) as f32, (cx + dir * hw * 0.5) as f32, (cy + hh) as f32);
            b.line_to((cx - dir * hw * 0.3) as f32, (cy + hh) as f32);
            if let Some(p) = b.finish() {
                stroke_with(&mut pm, &p, ink, line, LineCap::Butt, t);
            }
            let mut a = PathBuilder::new();
            let tip = (cx - dir * hw * 1.2, cy - hh);
            a.move_to(tip.0 as f32, tip.1 as f32);
            a.line_to((tip.0 + dir * s * 0.16) as f32, (tip.1 - s * 0.13) as f32);
            a.line_to((tip.0 + dir * s * 0.16) as f32, (tip.1 + s * 0.13) as f32);
            a.close();
            if let Some(p) = a.finish() {
                fill_with(&mut pm, &p, Shader::SolidColor(color(ink)), t);
            }
        }
        "gear" => {
            let outer = s * 0.5;
            // The Softube shell's settings gear: 8 tapered teeth turned a quarter pitch, a wide hole.
            if let Some(p) = gear_path(cx, cy, outer, 8, 0.74, 11.25) {
                fill_with(&mut pm, &p, Shader::SolidColor(color(ink)), t);
            }
            let hole = PathBuilder::from_circle(cx as f32, cy as f32, (outer * 0.42) as f32);
            if let Some(p) = hole {
                let paint = Paint {
                    shader: Shader::SolidColor(Color::TRANSPARENT),
                    anti_alias: true,
                    blend_mode: tiny_skia::BlendMode::Clear,
                    ..Default::default()
                };
                pm.fill_path(&p, &paint, FillRule::Winding, t, None);
            }
        }
        _ => return None,
    }
    Some(image(pm))
}

/// A gear outline: `teeth` tapered teeth (tops 30 % of the pitch, roots 50 %) between `outer` and
/// `outer × inner`, the first centred `phase` degrees clockwise from up.
fn gear_path(cx: f64, cy: f64, outer: f64, teeth: u32, inner: f64, phase: f64) -> Option<tiny_skia::Path> {
    let n = teeth.max(3);
    let root = outer * inner.clamp(0.2, 0.98);
    let step = 360.0 / n as f64;
    let mut b = PathBuilder::new();
    for i in 0..n {
        let a = phase + i as f64 * step;
        let pts =
            [(root, a - step * 0.25), (outer, a - step * 0.15), (outer, a + step * 0.15), (root, a + step * 0.25)];
        for (k, (r, d)) in pts.iter().enumerate() {
            let (x, y) = polar(cx, cy, *r, *d);
            if i == 0 && k == 0 { b.move_to(x, y) } else { b.line_to(x, y) }
        }
    }
    b.close();
    b.finish()
}

/// A [`Fill`] as a tiny-skia shader (element-local points).
pub fn shader_for(fill: &Fill, opacity: f32) -> Option<Shader<'static>> {
    match fill {
        Fill::Color(c) => parse_hex(Some(c)).map(|mut c| {
            c.a *= opacity;
            Shader::SolidColor(color(c))
        }),
        Fill::Gradient(g) => {
            let parsed: Vec<(f32, Rgba)> = g
                .stops
                .iter()
                .filter_map(|(t, c)| {
                    parse_hex(Some(c)).map(|mut c| {
                        c.a *= opacity;
                        (*t as f32, c)
                    })
                })
                .collect();
            let first = parsed.first()?.1;
            let stops: Vec<GradientStop> = parsed.iter().map(|(t, c)| GradientStop::new(*t, color(*c))).collect();
            if let Some([x0, y0, x1, y1]) = g.linear {
                LinearGradient::new(
                    Point::from_xy(x0 as f32, y0 as f32),
                    Point::from_xy(x1 as f32, y1 as f32),
                    stops,
                    SpreadMode::Pad,
                    Transform::identity(),
                )
            } else if let Some([cx, cy, r]) = g.radial {
                let c = Point::from_xy(cx as f32, cy as f32);
                RadialGradient::new(c, 0.0, c, r.max(1e-3) as f32, stops, SpreadMode::Pad, Transform::identity())
            } else {
                Some(Shader::SolidColor(color(first)))
            }
        }
    }
}

/// A shape's resolved inputs: frame-picked fill and stroke colour, arc angles.
#[derive(Clone, Debug, PartialEq)]
pub struct ShapeInput<'a> {
    pub shape: &'a Shape,
    pub fill: Option<&'a Fill>,
    pub stroke: Option<Rgba>,
    pub from: f64,
    pub to: f64,
}

fn shape_path(s: &Shape, w: f64, h: f64, from: f64, to: f64) -> Option<tiny_skia::Path> {
    let inset = s.inset.unwrap_or(0.0);
    let (x, y, w, h) = (inset, inset, (w - 2.0 * inset).max(0.0), (h - 2.0 * inset).max(0.0));
    let rect = tiny_skia::Rect::from_xywh(x as f32, y as f32, w as f32, h as f32);
    let (cx, cy) = (x + w / 2.0, y + h / 2.0);
    match s.kind {
        ShapeKind::Rect => {
            let r = s.radius.unwrap_or(0.0).clamp(0.0, w.min(h) / 2.0);
            if r <= 0.0 {
                return rect.map(PathBuilder::from_rect);
            }
            let (x0, y0, x1, y1) = (x as f32, y as f32, (x + w) as f32, (y + h) as f32);
            let r = r as f32;
            let k = r * 0.552_284_8;
            let mut b = PathBuilder::new();
            b.move_to(x0 + r, y0);
            b.line_to(x1 - r, y0);
            b.cubic_to(x1 - r + k, y0, x1, y0 + r - k, x1, y0 + r);
            b.line_to(x1, y1 - r);
            b.cubic_to(x1, y1 - r + k, x1 - r + k, y1, x1 - r, y1);
            b.line_to(x0 + r, y1);
            b.cubic_to(x0 + r - k, y1, x0, y1 - r + k, x0, y1 - r);
            b.line_to(x0, y0 + r);
            b.cubic_to(x0, y0 + r - k, x0 + r - k, y0, x0 + r, y0);
            b.close();
            b.finish()
        }
        ShapeKind::Ellipse => rect.and_then(PathBuilder::from_oval),
        ShapeKind::Line | ShapeKind::Polyline | ShapeKind::Polygon => {
            let pts = s.points.as_ref()?;
            let mut b = PathBuilder::new();
            for (i, p) in pts.iter().enumerate() {
                if i == 0 { b.move_to(p[0] as f32, p[1] as f32) } else { b.line_to(p[0] as f32, p[1] as f32) }
            }
            if s.kind == ShapeKind::Polygon {
                b.close();
            }
            b.finish()
        }
        ShapeKind::Arc => arc_path(cx, cy, w.min(h) / 2.0, from, to),
        ShapeKind::Gear => gear_path(cx, cy, w.min(h) / 2.0, s.teeth.unwrap_or(8), s.inner.unwrap_or(0.72), 0.0),
        ShapeKind::Hexagon => {
            let r = w.min(h) / 2.0;
            let mut b = PathBuilder::new();
            for i in 0..6 {
                let (px, py) = polar(cx, cy, r, 30.0 + 60.0 * i as f64);
                if i == 0 { b.move_to(px, py) } else { b.line_to(px, py) }
            }
            b.close();
            b.finish()
        }
        ShapeKind::Other => None,
    }
}

/// A `shape` element in its `w × h` frame.
pub fn shape_image(input: &ShapeInput, w: f64, h: f64, opacity: f32, px: f64) -> Option<ImageData> {
    let (mut pm, t) = canvas(w, h, px)?;
    let path = shape_path(input.shape, w, h, input.from, input.to)?;
    let open = matches!(input.shape.kind, ShapeKind::Line | ShapeKind::Polyline | ShapeKind::Arc);
    if let (Some(f), false) = (input.fill, open)
        && let Some(sh) = shader_for(f, opacity)
    {
        fill_with(&mut pm, &path, sh, t);
    }
    if let Some(mut c) = input.stroke {
        c.a *= opacity;
        let paint = Paint { shader: Shader::SolidColor(color(c)), anti_alias: true, ..Default::default() };
        // Never thinner than one device pixel: a hairline still shows (and dashes keep their gaps).
        let mut stroke = Stroke {
            width: input.shape.width.unwrap_or(1.0).max(1.0 / px) as f32,
            line_cap: LineCap::Butt,
            line_join: LineJoin::Miter,
            ..Default::default()
        };
        if let Some(d) = input.shape.dash.as_ref().filter(|d| d.len() >= 2) {
            stroke.dash = StrokeDash::new(d.iter().map(|v| *v as f32).collect(), 0.0);
        }
        pm.stroke_path(&path, &paint, &stroke, t, None);
    }
    Some(image(pm))
}

/// Points a lamp's raster extends beyond its frame (the glow).
pub fn lamp_margin(lamp: &Lamp) -> f64 {
    lamp.glow.unwrap_or(0.0).max(0.0).ceil()
}

/// A `lamp` at `level` (0 off … 1 lit) in its `w × h` frame plus [`lamp_margin`] on every side.
pub fn lamp_image(lamp: &Lamp, level: f64, w: f64, h: f64, px: f64) -> Option<ImageData> {
    let m = lamp_margin(lamp);
    let (mut pm, t) = canvas(w + 2.0 * m, h + 2.0 * m, px)?;
    let on = parse_hex(Some(&lamp.on)).unwrap_or(Rgba::rgb8(255, 64, 32));
    let off = parse_hex(lamp.off.as_deref()).unwrap_or(mix(Rgba::BLACK, on, 0.18));
    let level = level.clamp(0.0, 1.0) as f32;
    let body = mix(off, on, level);
    let (cx, cy) = (m + w / 2.0, m + h / 2.0);
    let circle = lamp.shape.as_deref() != Some("rect");
    let path = if circle {
        PathBuilder::from_oval(tiny_skia::Rect::from_xywh(m as f32, m as f32, w as f32, h as f32)?)
    } else {
        let s = Shape {
            kind: ShapeKind::Rect,
            radius: lamp.radius,
            points: None,
            fill: None,
            stroke: None,
            width: None,
            dash: None,
            from: None,
            to: None,
            teeth: None,
            inner: None,
            inset: None,
        };
        shape_path(&s, w, h, 0.0, 0.0).and_then(|p| p.transform(Transform::from_translate(m as f32, m as f32)))
    }?;
    if let (Some(g), true) = (lamp.glow.filter(|g| *g > 0.0), level > 0.0) {
        let r = (w.max(h) / 2.0 + g) as f32;
        let inner = (w.min(h) / 2.0) as f32 / r;
        let stops = vec![
            GradientStop::new(0.0, color(on.with_alpha(0.55 * level))),
            GradientStop::new(inner.clamp(0.0, 1.0), color(on.with_alpha(0.45 * level))),
            GradientStop::new(1.0, color(on.with_alpha(0.0))),
        ];
        let centre = Point::from_xy(cx as f32, cy as f32);
        if let Some(sh) = RadialGradient::new(centre, 0.0, centre, r, stops, SpreadMode::Pad, Transform::identity())
            && let Some(full) = tiny_skia::Rect::from_xywh(0.0, 0.0, (w + 2.0 * m) as f32, (h + 2.0 * m) as f32)
        {
            fill_with(&mut pm, &PathBuilder::from_rect(full), sh, t);
        }
    }
    // Body: a radial falloff from the centre (brighter core while lit).
    let core = mix(body, Rgba::WHITE, 0.35 * level);
    let r = (w.max(h) / 2.0) as f32;
    let stops = vec![GradientStop::new(0.0, color(core)), GradientStop::new(1.0, color(body))];
    let sh = RadialGradient::new(
        Point::from_xy(cx as f32, cy as f32),
        0.0,
        Point::from_xy(cx as f32, cy as f32),
        r.max(1e-3),
        stops,
        SpreadMode::Pad,
        Transform::identity(),
    )
    .unwrap_or(Shader::SolidColor(color(body)));
    fill_with(&mut pm, &path, sh, t);
    if lamp.highlight == Some(true) {
        let (hx, hy, hr) = (cx - w * 0.18, cy - h * 0.22, (w.min(h) * 0.3) as f32);
        let stops = vec![
            GradientStop::new(0.0, color(Rgba::WHITE.with_alpha(0.55))),
            GradientStop::new(1.0, color(Rgba::WHITE.with_alpha(0.0))),
        ];
        if let Some(sh) = RadialGradient::new(
            Point::from_xy(hx as f32, hy as f32),
            0.0,
            Point::from_xy(hx as f32, hy as f32),
            hr.max(1e-3),
            stops,
            SpreadMode::Pad,
            Transform::identity(),
        ) {
            fill_with(&mut pm, &path, sh, t);
        }
    }
    if let Some(b) = parse_hex(lamp.bezel.as_deref()) {
        stroke_with(&mut pm, &path, b, lamp.bezel_width.unwrap_or(1.0), LineCap::Butt, t);
    }
    Some(image(pm))
}

/// A fingerprint of anything hashable plus floats (raster cache keys).
pub fn fingerprint(parts: &impl Hash, floats: &[f64]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    parts.hash(&mut h);
    for f in floats {
        f.to_bits().hash(&mut h);
    }
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(img: &ImageData, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * img.width + x) * 4) as usize;
        [img.rgba[i], img.rgba[i + 1], img.rgba[i + 2], img.rgba[i + 3]]
    }

    #[test]
    fn ring_fills_clockwise_from_the_gap() {
        let ring = Ring { cx: 10.0, cy: 10.0, r: 7.0, width: 2.0, gap: Some(90.0) };
        let (track, accent) = (Rgba::rgb8(200, 200, 200), Rgba::rgb8(0, 0, 255));
        let half = ring_image(&ring, 0.5, 20.0, 20.0, track, accent, 2.0).unwrap();
        assert_eq!((half.width, half.height), (40, 40));
        // Left of centre (270°) lies in the first half of the sweep (225° … 495°), the right in
        // the second; the bottom is the gap.
        assert_eq!(px(&half, 6, 20)[..3], [0, 0, 255]);
        assert_eq!(px(&half, 34, 20)[..3], [200, 200, 200]);
        assert_eq!(px(&half, 20, 34)[3], 0);
        let empty = ring_image(&ring, 0.0, 20.0, 20.0, track, accent, 2.0).unwrap();
        assert_eq!(px(&empty, 6, 20)[..3], [200, 200, 200]);
    }

    #[test]
    fn shapes_fill_stroke_and_gradients() {
        let mut s: Shape = serde_json::from_str(r##"{"type": "rect", "radius": 3, "fill": "#FF0000"}"##).unwrap();
        let input = ShapeInput { shape: &s, fill: s.fill.as_ref(), stroke: None, from: 0.0, to: 0.0 };
        let img = shape_image(&input, 10.0, 10.0, 1.0, 2.0).unwrap();
        assert_eq!(px(&img, 10, 10), [255, 0, 0, 255]);
        assert_eq!(px(&img, 0, 0)[3], 0, "rounded corner");
        s.fill = Some(
            serde_json::from_str(r##"{"linear": [0, 0, 10, 0], "stops": [[0, "#000000"], [1, "#FFFFFF"]]}"##).unwrap(),
        );
        let input = ShapeInput { shape: &s, fill: s.fill.as_ref(), stroke: None, from: 0.0, to: 0.0 };
        let img = shape_image(&input, 10.0, 10.0, 1.0, 2.0).unwrap();
        assert!(px(&img, 3, 10)[0] < 60 && px(&img, 17, 10)[0] > 190, "left dark, right light");
        let line: Shape =
            serde_json::from_str(r##"{"type": "line", "points": [[0, 5], [10, 5]], "width": 2}"##).unwrap();
        let input = ShapeInput { shape: &line, fill: None, stroke: Some(Rgba::WHITE), from: 0.0, to: 0.0 };
        let img = shape_image(&input, 10.0, 10.0, 1.0, 2.0).unwrap();
        assert_eq!(px(&img, 10, 10), [255, 255, 255, 255]);
        assert_eq!(px(&img, 10, 2)[3], 0);
        let hex: Shape = serde_json::from_str(r##"{"type": "hexagon", "fill": "#00FF00"}"##).unwrap();
        let input = ShapeInput { shape: &hex, fill: hex.fill.as_ref(), stroke: None, from: 0.0, to: 0.0 };
        let img = shape_image(&input, 10.0, 10.0, 1.0, 2.0).unwrap();
        assert_eq!(px(&img, 10, 10), [0, 255, 0, 255]);
    }

    #[test]
    fn lamps_light_with_their_level() {
        let lamp: Lamp = serde_json::from_str(r##"{"on": "#FF0000", "off": "#200000", "glow": 2}"##).unwrap();
        let off = lamp_image(&lamp, 0.0, 10.0, 10.0, 2.0).unwrap();
        let on = lamp_image(&lamp, 1.0, 10.0, 10.0, 2.0).unwrap();
        assert_eq!((on.width, on.height), (28, 28), "frame plus the glow margin");
        assert!(px(&on, 14, 14)[0] > 200 && px(&off, 14, 14)[0] < 60);
        assert!(px(&on, 2, 14)[3] > 0, "glow outside the body while lit");
        assert_eq!(px(&off, 2, 14)[3], 0, "no glow while off");
    }

    #[test]
    fn glyphs_draw_known_names_only() {
        for g in ["power", "undo", "redo", "gear"] {
            let img = glyph_image(g, 24.0, 24.0, Rgba::WHITE, 2.0).unwrap();
            assert!(img.rgba.chunks(4).any(|p| p[3] > 0), "{g} draws ink");
        }
        assert!(glyph_image("Copy", 24.0, 24.0, Rgba::WHITE, 2.0).is_none());
    }
}
