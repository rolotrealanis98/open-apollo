//! Pillow 12.3.0's `Image.transform(size, AFFINE, matrix, BICUBIC)` on RGBA
//! (libImaging/Geometry.c `affine_transform` + `bicubic_filter32RGB`), with the
//! RGBA → RGBa → RGBA round trip Pillow wraps around it.

use super::img::Rgba;

/// Geometry.c's cubic (a = -1), evaluated in the same order.
fn cubic(v1: f64, v2: f64, v3: f64, v4: f64, d: f64) -> f64 {
    let p1 = v2;
    let p2 = -v1 + v3;
    let p3 = 2.0 * (v1 - v2) + v3 - v4;
    let p4 = -v1 + v2 - v3 + v4;
    p1 + d * (p2 + d * (p3 + d * p4))
}

pub fn affine_bicubic(src: &Rgba, m: [f64; 6]) -> Rgba {
    let mut pm = src.clone();
    pm.premultiply();
    let (w, h) = (src.w as i64, src.h as i64);
    let mut out = Rgba::new(src.w, src.h);
    if w == 0 || h == 0 {
        return out;
    }
    let px = |x: i64, y: i64, c: usize| pm.px[((y * w + x) * 4) as usize + c] as f64;
    for y in 0..h {
        for x in 0..w {
            let (xo, yo) = (x as f64 + 0.5, y as f64 + 0.5);
            let xin = m[0] * xo + m[1] * yo + m[2];
            let yin = m[3] * xo + m[4] * yo + m[5];
            if !(xin >= 0.0 && xin < w as f64 && yin >= 0.0 && yin < h as f64) {
                continue;
            }
            let (xin, yin) = (xin - 0.5, yin - 0.5);
            let (xf, yf) = (xin.floor(), yin.floor());
            let (dx, dy) = (xin - xf, yin - yf);
            let (x0, y0) = (xf as i64 - 1, yf as i64 - 1);
            let xc = [0, 1, 2, 3].map(|k| (x0 + k).clamp(0, w - 1));
            let o = ((y * w + x) * 4) as usize;
            for c in 0..4 {
                let row = |yy: i64| cubic(px(xc[0], yy, c), px(xc[1], yy, c), px(xc[2], yy, c), px(xc[3], yy, c), dx);
                let v1 = row(y0.clamp(0, h - 1));
                let v2 = if (0..h).contains(&(y0 + 1)) { row(y0 + 1) } else { v1 };
                let v3 = if (0..h).contains(&(y0 + 2)) { row(y0 + 2) } else { v2 };
                let v4 = if (0..h).contains(&(y0 + 3)) { row(y0 + 3) } else { v3 };
                let v = cubic(v1, v2, v3, v4, dy);
                out.px[o + c] = if v <= 0.0 {
                    0
                } else if v >= 255.0 {
                    255
                } else {
                    v as u8
                };
            }
        }
    }
    out.unpremultiply();
    out
}
