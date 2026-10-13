//! A port of Pillow 12.3.0's `ImagingResample` (libImaging/Resample.c): the
//! separable convolution with fixed-point coefficients that `Image.resize`
//! runs. Every LANCZOS recipe hashes the result, so this has to match Pillow
//! bit for bit, including its RGBA → RGBa premultiply round trip.

use super::img::Rgba;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter {
    Box,
    Bilinear,
    Hamming,
    Bicubic,
    Lanczos,
}

impl Filter {
    pub fn parse(s: &str) -> Option<Filter> {
        Some(match s {
            "box" => Filter::Box,
            "bilinear" => Filter::Bilinear,
            "hamming" => Filter::Hamming,
            "bicubic" => Filter::Bicubic,
            "lanczos" => Filter::Lanczos,
            _ => return None,
        })
    }

    fn support(self) -> f64 {
        match self {
            Filter::Box => 0.5,
            Filter::Bilinear | Filter::Hamming => 1.0,
            Filter::Bicubic => 2.0,
            Filter::Lanczos => 3.0,
        }
    }

    fn eval(self, x: f64) -> f64 {
        match self {
            Filter::Box => {
                if x > -0.5 && x <= 0.5 {
                    1.0
                } else {
                    0.0
                }
            }
            Filter::Bilinear => {
                let x = x.abs();
                if x < 1.0 { 1.0 - x } else { 0.0 }
            }
            Filter::Hamming => {
                let x = x.abs();
                if x == 0.0 {
                    1.0
                } else if x >= 1.0 {
                    0.0
                } else {
                    let x = x * std::f64::consts::PI;
                    x.sin() / x * (0.54 + 0.46 * x.cos())
                }
            }
            Filter::Bicubic => {
                const A: f64 = -0.5;
                let x = x.abs();
                if x < 1.0 {
                    ((A + 2.0) * x - (A + 3.0)) * x * x + 1.0
                } else if x < 2.0 {
                    (((x - 5.0) * x + 8.0) * x - 4.0) * A
                } else {
                    0.0
                }
            }
            Filter::Lanczos => {
                if (-3.0..3.0).contains(&x) {
                    sinc(x) * sinc(x / 3.0)
                } else {
                    0.0
                }
            }
        }
    }
}

fn sinc(x: f64) -> f64 {
    if x == 0.0 {
        return 1.0;
    }
    let x = x * std::f64::consts::PI;
    x.sin() / x
}

const PRECISION_BITS: u32 = 32 - 8 - 2;

/// `precompute_coeffs` + `normalize_coeffs_8bpc`: per output pixel, the first
/// input index, the tap count and `ksize` fixed-point weights.
struct Coeffs {
    ksize: usize,
    bounds: Vec<(usize, usize)>,
    kk: Vec<i32>,
}

fn coeffs(in_size: u32, in0: f32, in1: f32, out_size: u32, f: Filter) -> Coeffs {
    let scale = (in1 - in0) as f64 / out_size as f64;
    let filterscale = if scale < 1.0 { 1.0 } else { scale };
    let support = f.support() * filterscale;
    let ksize = support.ceil() as usize * 2 + 1;
    let mut bounds = Vec::with_capacity(out_size as usize);
    let mut kk = vec![0i32; out_size as usize * ksize];
    let mut k = vec![0f64; ksize];
    for xx in 0..out_size as usize {
        let center = in0 as f64 + (xx as f64 + 0.5) * scale;
        let ss = 1.0 / filterscale;
        let xmin = ((center - support + 0.5) as i64).max(0);
        let xmax = ((center + support + 0.5) as i64).min(in_size as i64) - xmin;
        let xmax = xmax.max(0) as usize;
        let mut ww = 0.0;
        for (x, kx) in k.iter_mut().enumerate().take(xmax) {
            let w = f.eval(((x as i64 + xmin) as f64 - center + 0.5) * ss);
            *kx = w;
            ww += w;
        }
        for (x, kx) in k.iter_mut().enumerate() {
            let v = if x < xmax { if ww != 0.0 { *kx / ww } else { *kx } } else { 0.0 };
            let one = (1i64 << PRECISION_BITS) as f64;
            kk[xx * ksize + x] = if v < 0.0 { (-0.5 + v * one) as i32 } else { (0.5 + v * one) as i32 };
        }
        bounds.push((xmin as usize, xmax));
    }
    Coeffs { ksize, bounds, kk }
}

#[inline]
fn clip8(v: i32) -> u8 {
    (v >> PRECISION_BITS).clamp(0, 255) as u8
}

/// Pillow's `ImagingResampleInner` on a 4-band 8-bit image (no premultiply here).
/// Index loops on purpose: they mirror the C code line for line.
#[allow(clippy::needless_range_loop)]
fn resample_bands(src: &Rgba, w: u32, h: u32, f: Filter, b: [f32; 4]) -> Rgba {
    let need_h = w != src.w || b[0] != 0.0 || b[2] != w as f32;
    let need_v = h != src.h || b[1] != 0.0 || b[3] != h as f32;
    let ch = coeffs(src.w, b[0], b[2], w, f);
    let mut cv = coeffs(src.h, b[1], b[3], h, f);
    let ybox_first = cv.bounds[0].0;
    let last = cv.bounds[h as usize - 1];
    let ybox_last = last.0 + last.1;
    let mut cur = std::borrow::Cow::Borrowed(src);
    if need_h {
        for bd in &mut cv.bounds {
            bd.0 -= ybox_first;
        }
        let th = (ybox_last - ybox_first) as u32;
        let mut tmp = Rgba::new(w, th);
        let sw = src.w as usize;
        for yy in 0..th as usize {
            let row = &src.px[(yy + ybox_first) * sw * 4..(yy + ybox_first + 1) * sw * 4];
            for xx in 0..w as usize {
                let (xmin, xmax) = ch.bounds[xx];
                let k = &ch.kk[xx * ch.ksize..xx * ch.ksize + ch.ksize];
                let mut ss = [1i32 << (PRECISION_BITS - 1); 4];
                for x in 0..xmax {
                    let p = &row[(x + xmin) * 4..(x + xmin) * 4 + 4];
                    for c in 0..4 {
                        ss[c] += p[c] as i32 * k[x];
                    }
                }
                let o = (yy * w as usize + xx) * 4;
                for c in 0..4 {
                    tmp.px[o + c] = clip8(ss[c]);
                }
            }
        }
        cur = std::borrow::Cow::Owned(tmp);
    }
    if need_v {
        let inp = cur.as_ref();
        let iw = inp.w as usize;
        let mut out = Rgba::new(inp.w, h);
        for yy in 0..h as usize {
            let (ymin, ymax) = cv.bounds[yy];
            let k = &cv.kk[yy * cv.ksize..yy * cv.ksize + cv.ksize];
            for xx in 0..iw {
                let mut ss = [1i32 << (PRECISION_BITS - 1); 4];
                for y in 0..ymax {
                    let o = ((y + ymin) * iw + xx) * 4;
                    for c in 0..4 {
                        ss[c] += inp.px[o + c] as i32 * k[y];
                    }
                }
                let o = (yy * iw + xx) * 4;
                for c in 0..4 {
                    out.px[o + c] = clip8(ss[c]);
                }
            }
        }
        return out;
    }
    cur.into_owned()
}

/// `Image.resize((w, h), filter, box)` on an RGBA image, premultiplying around
/// the convolution as Pillow does. `box` defaults to the whole image.
pub fn resize(src: &Rgba, w: u32, h: u32, f: Filter, b: Option<[f32; 4]>) -> Rgba {
    let full = [0.0, 0.0, src.w as f32, src.h as f32];
    let b = b.unwrap_or(full);
    if (w, h) == (src.w, src.h) && b == full {
        return src.clone();
    }
    if w == 0 || h == 0 || src.w == 0 || src.h == 0 {
        return Rgba::new(w, h);
    }
    let mut pm = src.clone();
    pm.premultiply();
    let mut out = resample_bands(&pm, w, h, f, b);
    out.unpremultiply();
    out
}

/// `Image.resize(size, NEAREST, box)`: Pillow maps output pixel centres
/// through the affine box transform and truncates.
pub fn resize_nearest(src: &Rgba, w: u32, h: u32, b: Option<[f32; 4]>) -> Rgba {
    let full = [0.0, 0.0, src.w as f32, src.h as f32];
    let b = b.unwrap_or(full);
    if (w, h) == (src.w, src.h) && b == full {
        return src.clone();
    }
    let mut out = Rgba::new(w, h);
    let sx = (b[2] - b[0]) as f64 / w as f64;
    let sy = (b[3] - b[1]) as f64 / h as f64;
    for y in 0..h {
        let yin = (b[1] as f64 + (y as f64 + 0.5) * sy).floor();
        if yin < 0.0 || yin >= src.h as f64 {
            continue;
        }
        for x in 0..w {
            let xin = (b[0] as f64 + (x as f64 + 0.5) * sx).floor();
            if xin < 0.0 || xin >= src.w as f64 {
                continue;
            }
            let s = src.idx(xin as u32, yin as u32);
            let d = out.idx(x, y);
            out.px[d..d + 4].copy_from_slice(&src.px[s..s + 4]);
        }
    }
    out
}
