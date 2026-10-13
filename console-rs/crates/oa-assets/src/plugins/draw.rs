//! Pillow 12.3.0's `ImageDraw.line` on a transparent RGBA layer (libImaging/Draw.c:
//! `line32` for width 1, `ImagingDrawWideLine` + `polygon_generic` for wider lines),
//! reproduced with the same int / float32 / float64 arithmetic.

use super::img::Rgba;

struct Mask {
    w: i32,
    h: i32,
    on: Vec<bool>,
}

impl Mask {
    fn point(&mut self, x: i32, y: i32) {
        if (0..self.w).contains(&x) && (0..self.h).contains(&y) {
            self.on[(y * self.w + x) as usize] = true;
        }
    }

    /// `hline32`: inclusive, clipped.
    fn hline(&mut self, mut x0: i32, y: i32, mut x1: i32) {
        if !(0..self.h).contains(&y) {
            return;
        }
        if x0 < 0 {
            x0 = 0;
        } else if x0 >= self.w {
            return;
        }
        if x1 < 0 {
            return;
        } else if x1 >= self.w {
            x1 = self.w - 1;
        }
        for x in x0..=x1 {
            self.on[(y * self.w + x) as usize] = true;
        }
    }
}

/// `line32` (the end point is not drawn).
fn bresenham(m: &mut Mask, mut x0: i32, mut y0: i32, x1: i32, y1: i32) {
    let (mut dx, mut dy) = (x1 - x0, y1 - y0);
    let xs = if dx < 0 { -1 } else { 1 };
    let ys = if dy < 0 { -1 } else { 1 };
    dx = dx.abs();
    dy = dy.abs();
    if dx == 0 {
        for _ in 0..dy {
            m.point(x0, y0);
            y0 += ys;
        }
    } else if dy == 0 {
        for _ in 0..dx {
            m.point(x0, y0);
            x0 += xs;
        }
    } else if dx > dy {
        let n = dx;
        dy += dy;
        let mut e = dy - dx;
        dx += dx;
        for _ in 0..n {
            m.point(x0, y0);
            if e >= 0 {
                y0 += ys;
                e -= dx;
            }
            e += dy;
            x0 += xs;
        }
    } else {
        let n = dy;
        dx += dx;
        let mut e = dx - dy;
        dy += dy;
        for _ in 0..n {
            m.point(x0, y0);
            if e >= 0 {
                x0 += xs;
                e -= dy;
            }
            e += dx;
            y0 += ys;
        }
    }
}

/// Draw.c `ROUND_UP` / `ROUND_DOWN` on a double.
fn round_up(f: f64) -> i32 {
    if f >= 0.0 { (f + 0.5).floor() as i32 } else { -((f.abs() + 0.5).floor() as i32) }
}

fn round_down(f: f64) -> i32 {
    if f >= 0.0 { (f - 0.5).ceil() as i32 } else { -((f.abs() - 0.5).ceil() as i32) }
}

/// The same macros on a C float (`f + 0.5F` is a float addition).
fn round_up_f(f: f32) -> i32 {
    if f >= 0.0 { (f + 0.5).floor() as i32 } else { -((f.abs() + 0.5).floor() as i32) }
}

fn round_down_f(f: f32) -> i32 {
    if f >= 0.0 { (f - 0.5).ceil() as i32 } else { -((f.abs() - 0.5).ceil() as i32) }
}

#[derive(Clone, Copy)]
struct Edge {
    x0: i32,
    y0: i32,
    xmin: i32,
    xmax: i32,
    ymin: i32,
    ymax: i32,
    dx: f32,
}

fn edge(x0: i32, y0: i32, x1: i32, y1: i32) -> Edge {
    let dx = if y0 == y1 { 0.0 } else { (x1 - x0) as f32 / (y1 - y0) as f32 };
    Edge { x0, y0, xmin: x0.min(x1), xmax: x0.max(x1), ymin: y0.min(y1), ymax: y0.max(y1), dx }
}

fn edge_x(e: &Edge, y: i32) -> f32 {
    (y - e.y0) as f32 * e.dx + e.x0 as f32
}

/// `polygon_generic` (non-even-odd fill, no alpha coverage) with `hline32`.
fn polygon(m: &mut Mask, edges: &[Edge]) {
    let (mut ymin, mut ymax) = (m.h - 1, 0);
    let mut table = Vec::new();
    for e in edges {
        ymin = ymin.min(e.ymin);
        ymax = ymax.max(e.ymax);
        if e.ymin == e.ymax {
            m.hline(e.xmin, e.ymin, e.xmax);
            continue;
        }
        table.push(*e);
    }
    ymin = ymin.max(0);
    ymax = ymax.min(m.h);
    let mut xx: Vec<f32> = Vec::new();
    for y in ymin..=ymax {
        xx.clear();
        for (i, cur) in table.iter().enumerate() {
            if !(cur.ymin <= y && y <= cur.ymax) {
                continue;
            }
            xx.push(edge_x(cur, y));
            let last = xx[xx.len() - 1];
            if y == cur.ymax && y < ymax {
                xx.push(last);
            } else if cur.dx != 0.0 && xx.len() % 2 == 1 && last.round() == last {
                // "connect discontiguous corners" (C roundf: halves away from zero, as f32::round)
                for k in 0..i {
                    let other = &table[k];
                    if (cur.dx > 0.0 && other.dx <= 0.0) || (cur.dx < 0.0 && other.dx >= 0.0) {
                        continue;
                    }
                    if last == edge_x(other, y) {
                        let off = if y == ymax { -1 } else { 1 };
                        let a1 = edge_x(cur, y + off);
                        let a2 = edge_x(other, y + off);
                        let v = if y == cur.ymax {
                            if cur.dx > 0.0 { a1.max(a2) + 1.0 } else { a1.min(a2) - 1.0 }
                        } else if cur.dx > 0.0 {
                            a1.min(a2)
                        } else {
                            a1.max(a2) + 1.0
                        };
                        if k < xx.len() {
                            xx[k] = v;
                        }
                        break;
                    }
                }
            }
        }
        xx.sort_by(|a, b| a.total_cmp(b));
        for i in (1..xx.len()).step_by(2) {
            m.hline(round_up_f(xx[i - 1]), y, round_down_f(xx[i]));
        }
    }
}

/// `ImagingDrawWideLine`.
fn wide_line(m: &mut Mask, x0: i32, y0: i32, x1: i32, y1: i32, width: u32) {
    let (dx, dy) = (x1 - x0, y1 - y0);
    if dx == 0 && dy == 0 {
        m.point(x0, y0);
        return;
    }
    let big = ((dx as i64 * dx as i64 + dy as i64 * dy as i64) as f64).sqrt();
    let small = (width as f64 - 1.0) / 2.0;
    let ratio_max = round_up(small) as f64 / big;
    let ratio_min = round_down(small) as f64 / big;
    let dxmin = round_down(ratio_min * dy as f64);
    let dxmax = round_down(ratio_max * dy as f64);
    let dymin = round_down(ratio_min * dx as f64);
    let dymax = round_down(ratio_max * dx as f64);
    let v = [(x0 - dxmin, y0 + dymax), (x1 - dxmin, y1 + dymax), (x1 + dxmax, y1 - dymin), (x0 + dxmax, y0 - dymin)];
    let edges: Vec<Edge> = (0..4).map(|i| edge(v[i].0, v[i].1, v[(i + 1) % 4].0, v[(i + 1) % 4].1)).collect();
    polygon(m, &edges);
}

/// `Image.new("RGBA", (w, h))` + `ImageDraw.Draw(im).line([(x0, y0), (x1, y1)], fill=rgba, width=width)`:
/// pixels on the line are set to `rgba` (no blending).
pub fn line(w: u32, h: u32, xy: [i32; 4], width: u32, rgba: [u8; 4]) -> Rgba {
    let mut m = Mask { w: w as i32, h: h as i32, on: vec![false; w as usize * h as usize] };
    let [x0, y0, x1, y1] = xy;
    match width {
        0 => {}
        1 => {
            bresenham(&mut m, x0, y0, x1, y1);
            m.point(x1, y1);
        }
        _ => wide_line(&mut m, x0, y0, x1, y1, width),
    }
    let mut out = Rgba::new(w, h);
    for (i, &on) in m.on.iter().enumerate() {
        if on {
            out.px[i * 4..i * 4 + 4].copy_from_slice(&rgba);
        }
    }
    out
}
