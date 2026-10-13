//! A straight-alpha RGBA8 image and the handful of Pillow operations the
//! recipes use, reproduced bit for bit (Pillow 12.3.0 integer formulas).

use sha2::{Digest, Sha256};

#[derive(Clone, PartialEq, Eq)]
pub struct Rgba {
    pub w: u32,
    pub h: u32,
    /// Row-major RGBA, straight (not premultiplied) alpha.
    pub px: Vec<u8>,
}

impl std::fmt::Debug for Rgba {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Rgba({}x{})", self.w, self.h)
    }
}

/// Pillow's `MULDIV255`: `round(a * b / 255)` in integers.
#[inline]
pub fn muldiv255(a: u32, b: u32) -> u32 {
    let t = a * b + 128;
    ((t >> 8) + t) >> 8
}

impl Rgba {
    /// A transparent black image, like `Image.new("RGBA", (w, h))`.
    pub fn new(w: u32, h: u32) -> Rgba {
        Rgba { w, h, px: vec![0; w as usize * h as usize * 4] }
    }

    pub fn filled(w: u32, h: u32, c: [u8; 4]) -> Rgba {
        let mut px = Vec::with_capacity(w as usize * h as usize * 4);
        for _ in 0..w as usize * h as usize {
            px.extend_from_slice(&c);
        }
        Rgba { w, h, px }
    }

    pub fn from_raw(w: u32, h: u32, px: Vec<u8>) -> Option<Rgba> {
        (px.len() == w as usize * h as usize * 4).then_some(Rgba { w, h, px })
    }

    #[inline]
    pub fn idx(&self, x: u32, y: u32) -> usize {
        (y as usize * self.w as usize + x as usize) * 4
    }

    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let i = self.idx(x, y);
        [self.px[i], self.px[i + 1], self.px[i + 2], self.px[i + 3]]
    }

    /// `Image.crop((x, y, x + w, y + h))`: area outside the image is transparent black.
    pub fn crop(&self, x: i64, y: i64, w: u32, h: u32) -> Rgba {
        let mut out = Rgba::new(w, h);
        out.paste(self, -x, -y);
        out
    }

    /// The overlap of `src` placed at (`x`, `y`) with `self`, as
    /// (dst x, dst y, src x, src y, width, height).
    fn overlap(&self, src: &Rgba, x: i64, y: i64) -> Option<(usize, usize, usize, usize, usize, usize)> {
        let x0 = x.max(0);
        let y0 = y.max(0);
        let x1 = (x + src.w as i64).min(self.w as i64);
        let y1 = (y + src.h as i64).min(self.h as i64);
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        Some((x0 as usize, y0 as usize, (x0 - x) as usize, (y0 - y) as usize, (x1 - x0) as usize, (y1 - y0) as usize))
    }

    /// `Image.paste(src, (x, y))`: replaces pixels, clipped to `self`.
    pub fn paste(&mut self, src: &Rgba, x: i64, y: i64) {
        let Some((dx, dy, sx, sy, w, h)) = self.overlap(src, x, y) else { return };
        let (dw, sw) = (self.w as usize, src.w as usize);
        for r in 0..h {
            let d = ((dy + r) * dw + dx) * 4;
            let s = ((sy + r) * sw + sx) * 4;
            self.px[d..d + w * 4].copy_from_slice(&src.px[s..s + w * 4]);
        }
    }

    /// `Image.paste(src, (x, y), mask)` with an RGBA/L mask image of src's size:
    /// every band becomes `(src * m + dst * (255 - m)) / 255`, rounded.
    pub fn paste_masked(&mut self, src: &Rgba, mask: &[u8], x: i64, y: i64) {
        let Some((dx, dy, sx, sy, w, h)) = self.overlap(src, x, y) else { return };
        let (dw, sw) = (self.w as usize, src.w as usize);
        for r in 0..h {
            for c in 0..w {
                let d = ((dy + r) * dw + dx + c) * 4;
                let si = (sy + r) * sw + sx + c;
                let m = mask[si] as u32;
                for k in 0..4 {
                    let t = src.px[si * 4 + k] as u32 * m + self.px[d + k] as u32 * (255 - m) + 128;
                    self.px[d + k] = (((t >> 8) + t) >> 8) as u8;
                }
            }
        }
    }

    /// The alpha band, e.g. for [`Rgba::paste_masked`] with the source as its own mask.
    pub fn alpha(&self) -> Vec<u8> {
        self.px.as_chunks::<4>().0.iter().map(|p| p[3]).collect()
    }

    /// `Image.alpha_composite(src, dest=(x, y))`: Porter-Duff "over", clipped to `self`.
    pub fn composite(&mut self, src: &Rgba, x: i64, y: i64) {
        let Some((dx, dy, sx, sy, w, h)) = self.overlap(src, x, y) else { return };
        let (dw, sw) = (self.w as usize, src.w as usize);
        for r in 0..h {
            for c in 0..w {
                let d = ((dy + r) * dw + dx + c) * 4;
                let s = ((sy + r) * sw + sx + c) * 4;
                let px = over(&src.px[s..s + 4], &self.px[d..d + 4]);
                self.px[d..d + 4].copy_from_slice(&px);
            }
        }
    }

    /// SHA-256 over `u32le w, u32le h, RGBA` with RGB zeroed where alpha is 0,
    /// so the hash doesn't depend on invisible colour or the PNG encoder.
    pub fn hash(&self) -> String {
        let mut h = Sha256::new();
        h.update(self.w.to_le_bytes());
        h.update(self.h.to_le_bytes());
        let mut row = Vec::with_capacity(self.w as usize * 4);
        for line in self.px.chunks(self.w.max(1) as usize * 4) {
            row.clear();
            for p in line.as_chunks::<4>().0 {
                if p[3] == 0 {
                    row.extend_from_slice(&[0, 0, 0, 0]);
                } else {
                    row.extend_from_slice(p);
                }
            }
            h.update(&row);
        }
        hex(&h.finalize())
    }

    /// Premultiply in place (`convert("RGBa")`).
    pub fn premultiply(&mut self) {
        for p in self.px.as_chunks_mut::<4>().0 {
            let a = p[3] as u32;
            for c in &mut p[..3] {
                *c = muldiv255(*c as u32, a) as u8;
            }
        }
    }

    /// Undo [`Rgba::premultiply`] (`convert("RGBA")` from "RGBa"): alpha 0 and 255 keep their colour.
    pub fn unpremultiply(&mut self) {
        for p in self.px.as_chunks_mut::<4>().0 {
            let a = p[3] as u32;
            if a == 0 || a == 255 {
                continue;
            }
            for c in &mut p[..3] {
                *c = ((255 * *c as u32) / a).min(255) as u8;
            }
        }
    }
}

impl Rgba {
    /// Brainworx's disabled-control tint in integer form: for alpha `al` > 0 and
    /// target `t`, `c = min(255, round_half_even((c*al*(256-a) + t*a*256) / 65536))`.
    /// Equal to agent Q's float64 formula, whose terms are all exact.
    pub fn premul_tint(&mut self, rgb: [u8; 3], a: u32) {
        for p in self.px.as_chunks_mut::<4>().0 {
            let al = p[3] as u32;
            if al == 0 {
                continue;
            }
            for k in 0..3 {
                let num = p[k] as u32 * al * (256 - a) + rgb[k] as u32 * a * 256;
                let (mut q, r) = (num >> 16, num & 0xFFFF);
                if r > 0x8000 || (r == 0x8000 && q & 1 == 1) {
                    q += 1;
                }
                p[k] = q.min(255) as u8;
            }
        }
    }

    /// JUCE premultiplies 8-bit art on load (`(c * a + 127) >> 8`, not at alpha
    /// 255); dumping it back divides by alpha (integer floor). Alpha 0 clears colour.
    pub fn juce_premul_roundtrip(&mut self) {
        for p in self.px.as_chunks_mut::<4>().0 {
            let al = p[3] as u32;
            for c in &mut p[..3] {
                *c = match al {
                    255 => *c,
                    0 => 0,
                    _ => (((*c as u32 * al + 127) >> 8) * 255 / al) as u8,
                };
            }
        }
    }
}

/// Pillow's `ImagingAlphaComposite` for one pixel (`src` over `dst`).
#[inline]
pub fn over(src: &[u8], dst: &[u8]) -> [u8; 4] {
    const PB: u32 = 7;
    let sa = src[3] as u32;
    if sa == 0 {
        return [dst[0], dst[1], dst[2], dst[3]];
    }
    let blend = dst[3] as u32 * (255 - sa);
    let outa255 = sa * 255 + blend;
    let coef1 = sa * 255 * 255 * (1 << PB) / outa255;
    let coef2 = 255 * (1 << PB) - coef1;
    let sh = |a: u32| ((a >> 8) + a) >> 8;
    let mut out = [0u8; 4];
    for k in 0..3 {
        let t = src[k] as u32 * coef1 + dst[k] as u32 * coef2;
        out[k] = (sh(t + (0x80 << PB)) >> PB) as u8;
    }
    out[3] = sh(outa255 + 0x80) as u8;
    out
}

pub fn hex(b: &[u8]) -> String {
    use std::fmt::Write;
    b.iter().fold(String::with_capacity(b.len() * 2), |mut s, x| {
        let _ = write!(s, "{x:02x}");
        s
    })
}

pub fn sha256(b: &[u8]) -> String {
    hex(&Sha256::digest(b))
}
