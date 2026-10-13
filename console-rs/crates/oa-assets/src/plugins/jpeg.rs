//! Baseline JPEG decoding with libjpeg-turbo's default output: the ISLOW
//! integer IDCT (jidctint.c), its range-limit table and the YCbCr → RGB
//! tables of jdcolor.c. Pillow decodes JPEGs with libjpeg-turbo, so this gives
//! the same pixels. Only what UA's DLLs use is supported: sequential Huffman,
//! 8-bit samples, 1 or 3 components without chroma subsampling.

use super::img::Rgba;

const NATURAL: [usize; 80] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14, 21,
    28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54,
    47, 55, 62, 63, // extra entries guard against a corrupt run length, as libjpeg does
    63, 63, 63, 63, 63, 63, 63, 63, 63, 63, 63, 63, 63, 63, 63, 63,
];

#[derive(Clone, Default)]
struct Huff {
    maxcode: [i32; 18],
    valoffset: [i32; 17],
    vals: Vec<u8>,
}

impl Huff {
    fn new(counts: &[u8; 16], vals: Vec<u8>) -> Huff {
        let mut h = Huff { vals, ..Default::default() };
        let (mut p, mut code) = (0i32, 0i32);
        for l in 1..=16 {
            let n = counts[l - 1] as i32;
            if n > 0 {
                h.valoffset[l] = p - code;
                code += n;
                p += n;
                h.maxcode[l] = code - 1;
            } else {
                h.maxcode[l] = -1;
            }
            code <<= 1;
        }
        h.maxcode[17] = 0x000F_FFFF;
        h
    }
}

struct Bits<'a> {
    d: &'a [u8],
    pos: usize,
    acc: u32,
    n: u32,
    marker: bool,
}

impl Bits<'_> {
    fn fill(&mut self) {
        while self.n <= 24 {
            let mut byte = 0u32;
            if !self.marker && self.pos < self.d.len() {
                let b = self.d[self.pos];
                if b == 0xFF {
                    match self.d.get(self.pos + 1) {
                        Some(0) => {
                            self.pos += 2;
                            byte = 0xFF;
                        }
                        _ => self.marker = true,
                    }
                } else {
                    self.pos += 1;
                    byte = b as u32;
                }
            }
            self.acc |= byte << (24 - self.n);
            self.n += 8;
        }
    }

    fn bits(&mut self, k: u32) -> i32 {
        if k == 0 {
            return 0;
        }
        self.fill();
        let v = self.acc >> (32 - k);
        self.acc <<= k;
        self.n -= k;
        v as i32
    }

    fn decode(&mut self, h: &Huff) -> Result<u8, String> {
        let mut code = self.bits(1);
        let mut l = 1;
        while code > h.maxcode[l] {
            code = (code << 1) | self.bits(1);
            l += 1;
            if l > 16 {
                return Err("corrupt Huffman code".into());
            }
        }
        h.vals.get((code + h.valoffset[l]) as usize).copied().ok_or_else(|| "corrupt Huffman table".to_string())
    }

    /// Skip to the RSTn marker after a restart interval.
    fn restart(&mut self) {
        self.acc = 0;
        self.n = 0;
        self.marker = false;
        while self.pos + 1 < self.d.len() {
            if self.d[self.pos] == 0xFF && (0xD0..=0xD7).contains(&self.d[self.pos + 1]) {
                self.pos += 2;
                return;
            }
            self.pos += 1;
        }
    }
}

fn extend(v: i32, s: u32) -> i32 {
    if s == 0 {
        0
    } else if v < (1 << (s - 1)) {
        v - (1 << s) + 1
    } else {
        v
    }
}

/// libjpeg's post-IDCT range limit: `range_limit[x & 1023]` for x centred on 0.
fn idct_limit(x: i32) -> u8 {
    let i = (x & 1023) as usize;
    match i {
        0..=127 => (i + 128) as u8,
        128..=511 => 255,
        512..=895 => 0,
        _ => (i - 896) as u8,
    }
}

const CONST_BITS: i32 = 13;
const PASS1_BITS: i32 = 2;

fn descale(x: i64, n: i32) -> i64 {
    (x + (1i64 << (n - 1))) >> n
}

/// jidctint.c `jpeg_idct_islow`.
fn idct_islow(coef: &[i32; 64], q: &[u16; 64], out: &mut [u8], stride: usize) {
    const F0_298: i64 = 2446;
    const F0_390: i64 = 3196;
    const F0_541: i64 = 4433;
    const F0_765: i64 = 6270;
    const F0_899: i64 = 7373;
    const F1_175: i64 = 9633;
    const F1_501: i64 = 12299;
    const F1_847: i64 = 15137;
    const F1_961: i64 = 16069;
    const F2_053: i64 = 16819;
    const F2_562: i64 = 20995;
    const F3_072: i64 = 25172;
    let mut ws = [0i64; 64];
    for c in 0..8 {
        let dq = |r: usize| (coef[r * 8 + c] as i64) * (q[r * 8 + c] as i64);
        if (1..8).all(|r| coef[r * 8 + c] == 0) {
            let dc = dq(0) << PASS1_BITS;
            for r in 0..8 {
                ws[r * 8 + c] = dc;
            }
            continue;
        }
        let (z2, z3) = (dq(2), dq(6));
        let z1 = (z2 + z3) * F0_541;
        let tmp2 = z1 + z3 * -F1_847;
        let tmp3 = z1 + z2 * F0_765;
        let (z2, z3) = (dq(0), dq(4));
        let tmp0 = (z2 + z3) << CONST_BITS;
        let tmp1 = (z2 - z3) << CONST_BITS;
        let (t10, t13, t11, t12) = (tmp0 + tmp3, tmp0 - tmp3, tmp1 + tmp2, tmp1 - tmp2);
        let (mut o0, mut o1, mut o2, mut o3) = (dq(7), dq(5), dq(3), dq(1));
        let z1 = o0 + o3;
        let z2 = o1 + o2;
        let z3 = o0 + o2;
        let z4 = o1 + o3;
        let z5 = (z3 + z4) * F1_175;
        o0 *= F0_298;
        o1 *= F2_053;
        o2 *= F3_072;
        o3 *= F1_501;
        let z1 = z1 * -F0_899;
        let z2 = z2 * -F2_562;
        let z3 = z3 * -F1_961 + z5;
        let z4 = z4 * -F0_390 + z5;
        o0 += z1 + z3;
        o1 += z2 + z4;
        o2 += z2 + z3;
        o3 += z1 + z4;
        let s = CONST_BITS - PASS1_BITS;
        ws[c] = descale(t10 + o3, s);
        ws[56 + c] = descale(t10 - o3, s);
        ws[8 + c] = descale(t11 + o2, s);
        ws[48 + c] = descale(t11 - o2, s);
        ws[16 + c] = descale(t12 + o1, s);
        ws[40 + c] = descale(t12 - o1, s);
        ws[24 + c] = descale(t13 + o0, s);
        ws[32 + c] = descale(t13 - o0, s);
    }
    for r in 0..8 {
        let w = &ws[r * 8..r * 8 + 8];
        let o = &mut out[r * stride..r * stride + 8];
        if w[1..].iter().all(|&v| v == 0) {
            let v = idct_limit(descale(w[0], PASS1_BITS + 3) as i32);
            o.fill(v);
            continue;
        }
        let (z2, z3) = (w[2], w[6]);
        let z1 = (z2 + z3) * F0_541;
        let tmp2 = z1 + z3 * -F1_847;
        let tmp3 = z1 + z2 * F0_765;
        let tmp0 = (w[0] + w[4]) << CONST_BITS;
        let tmp1 = (w[0] - w[4]) << CONST_BITS;
        let (t10, t13, t11, t12) = (tmp0 + tmp3, tmp0 - tmp3, tmp1 + tmp2, tmp1 - tmp2);
        let (mut o0, mut o1, mut o2, mut o3) = (w[7], w[5], w[3], w[1]);
        let z1 = o0 + o3;
        let z2 = o1 + o2;
        let z3 = o0 + o2;
        let z4 = o1 + o3;
        let z5 = (z3 + z4) * F1_175;
        o0 *= F0_298;
        o1 *= F2_053;
        o2 *= F3_072;
        o3 *= F1_501;
        let z1 = z1 * -F0_899;
        let z2 = z2 * -F2_562;
        let z3 = z3 * -F1_961 + z5;
        let z4 = z4 * -F0_390 + z5;
        o0 += z1 + z3;
        o1 += z2 + z4;
        o2 += z2 + z3;
        o3 += z1 + z4;
        let s = CONST_BITS + PASS1_BITS + 3;
        let lim = |x: i64| idct_limit(descale(x, s) as i32);
        o[0] = lim(t10 + o3);
        o[7] = lim(t10 - o3);
        o[1] = lim(t11 + o2);
        o[6] = lim(t11 - o2);
        o[2] = lim(t12 + o1);
        o[5] = lim(t12 - o1);
        o[3] = lim(t13 + o0);
        o[4] = lim(t13 - o0);
    }
}

struct Comp {
    id: u8,
    tq: usize,
    plane: Vec<u8>,
}

fn be16(d: &[u8], o: usize) -> Result<usize, String> {
    d.get(o..o + 2).map(|b| u16::from_be_bytes([b[0], b[1]]) as usize).ok_or_else(|| "truncated JPEG".to_string())
}

pub fn decode(d: &[u8]) -> Result<Rgba, String> {
    if d.get(..2) != Some(&[0xFF, 0xD8]) {
        return Err("not a JPEG".into());
    }
    let mut q: [Option<[u16; 64]>; 4] = [None; 4];
    let mut dc: [Option<Huff>; 4] = Default::default();
    let mut ac: [Option<Huff>; 4] = Default::default();
    let (mut w, mut h) = (0usize, 0usize);
    let mut comps: Vec<Comp> = Vec::new();
    let (mut jfif, mut adobe) = (false, None::<u8>);
    let mut restart = 0usize;
    let mut pos = 2;
    let (mut bw, mut bh) = (0usize, 0usize);
    loop {
        while d.get(pos) == Some(&0xFF) && d.get(pos + 1) == Some(&0xFF) {
            pos += 1;
        }
        if d.get(pos) != Some(&0xFF) {
            return Err("JPEG marker expected".into());
        }
        let m = *d.get(pos + 1).ok_or("truncated JPEG")?;
        pos += 2;
        match m {
            0xD9 => break,
            0xD0..=0xD7 | 0x01 => continue,
            _ => {}
        }
        let len = be16(d, pos)?;
        let seg = d.get(pos + 2..pos + len).ok_or("truncated JPEG segment")?;
        match m {
            0xDB => {
                let mut i = 0;
                while i < seg.len() {
                    let (pq, tq) = (seg[i] >> 4, (seg[i] & 15) as usize);
                    i += 1;
                    let mut t = [0u16; 64];
                    for k in 0..64 {
                        t[NATURAL[k]] = if pq == 0 {
                            *seg.get(i + k).ok_or("bad DQT")? as u16
                        } else {
                            be16(seg, i + 2 * k)? as u16
                        };
                    }
                    i += if pq == 0 { 64 } else { 128 };
                    *q.get_mut(tq).ok_or("bad DQT")? = Some(t);
                }
            }
            0xC4 => {
                let mut i = 0;
                while i < seg.len() {
                    let (tc, th) = (seg[i] >> 4, (seg[i] & 15) as usize);
                    let counts: [u8; 16] = seg.get(i + 1..i + 17).ok_or("bad DHT")?.try_into().unwrap();
                    let n: usize = counts.iter().map(|&c| c as usize).sum();
                    let vals = seg.get(i + 17..i + 17 + n).ok_or("bad DHT")?.to_vec();
                    i += 17 + n;
                    let t = Some(Huff::new(&counts, vals));
                    match (tc, th) {
                        (0, 0..=3) => dc[th] = t,
                        (1, 0..=3) => ac[th] = t,
                        _ => return Err("bad DHT".into()),
                    }
                }
            }
            0xC0 | 0xC1 => {
                if seg[0] != 8 {
                    return Err("only 8-bit JPEG is supported".into());
                }
                h = be16(seg, 1)?;
                w = be16(seg, 3)?;
                let n = seg[5] as usize;
                for k in 0..n {
                    let c = seg.get(6 + 3 * k..9 + 3 * k).ok_or("bad SOF")?;
                    if c[1] != 0x11 {
                        return Err("subsampled JPEG is not supported".into());
                    }
                    comps.push(Comp { id: c[0], tq: (c[2] & 3) as usize, plane: Vec::new() });
                }
                if !(n == 1 || n == 3) || w == 0 || h == 0 {
                    return Err("unsupported JPEG layout".into());
                }
                bw = w.div_ceil(8);
                bh = h.div_ceil(8);
                for c in &mut comps {
                    c.plane = vec![0; bw * 8 * bh * 8];
                }
            }
            0xC2 | 0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF => {
                return Err("progressive/lossless/arithmetic JPEG is not supported".into());
            }
            0xDD => restart = be16(seg, 0)?,
            0xE0 => jfif |= seg.starts_with(b"JFIF\0"),
            0xEE if seg.starts_with(b"Adobe") && seg.len() >= 12 => adobe = Some(seg[11]),
            0xDA => {
                let ns = seg[0] as usize;
                let mut sc = Vec::new();
                for k in 0..ns {
                    let (cid, t) = (seg[1 + 2 * k], seg[2 + 2 * k]);
                    let ci = comps.iter().position(|c| c.id == cid).ok_or("scan names an unknown component")?;
                    sc.push((ci, (t >> 4) as usize, (t & 15) as usize));
                }
                let mut bits = Bits { d, pos: pos + len, acc: 0, n: 0, marker: false };
                let mut pred = vec![0i32; comps.len()];
                let stride = bw * 8;
                let mut coef = [0i32; 64];
                let total = bw * bh;
                for mcu in 0..total {
                    if restart > 0 && mcu > 0 && mcu % restart == 0 {
                        bits.restart();
                        pred.iter_mut().for_each(|p| *p = 0);
                    }
                    let (bx, by) = (mcu % bw, mcu / bw);
                    for &(ci, td, ta) in &sc {
                        let hd = dc[td].as_ref().ok_or("missing DC table")?;
                        let ha = ac[ta].as_ref().ok_or("missing AC table")?;
                        coef.fill(0);
                        let s = bits.decode(hd)? as u32;
                        let diff = extend(bits.bits(s), s);
                        pred[ci] += diff;
                        coef[0] = pred[ci] as i16 as i32;
                        let mut k = 1;
                        while k < 64 {
                            let rs = bits.decode(ha)?;
                            let (r, s) = ((rs >> 4) as usize, (rs & 15) as u32);
                            if s != 0 {
                                k += r;
                                coef[NATURAL[k]] = extend(bits.bits(s), s) as i16 as i32;
                                k += 1;
                            } else if r == 15 {
                                k += 16;
                            } else {
                                break;
                            }
                        }
                        let qt = q[comps[ci].tq].as_ref().ok_or("missing quantization table")?;
                        let o = by * 8 * stride + bx * 8;
                        idct_islow(&coef, qt, &mut comps[ci].plane[o..], stride);
                    }
                }
                // continue after the entropy-coded data: find the next marker
                let mut p = bits.pos;
                while p + 1 < d.len() && !(d[p] == 0xFF && d[p + 1] != 0 && !(0xD0..=0xD7).contains(&d[p + 1])) {
                    p += 1;
                }
                pos = p;
                continue;
            }
            _ => {}
        }
        pos += len;
    }
    if comps.is_empty() {
        return Err("JPEG without a frame".into());
    }
    let stride = bw * 8;
    let mut out = Rgba::new(w as u32, h as u32);
    let ycc = comps.len() == 3
        && if jfif {
            true
        } else if let Some(t) = adobe {
            t != 0
        } else {
            !(comps[0].id == b'R' && comps[1].id == b'G' && comps[2].id == b'B')
        };
    let (rr, bb, rg, bg) = ycc_tables();
    for y in 0..h {
        for x in 0..w {
            let i = y * stride + x;
            let o = (y * w + x) * 4;
            let px = if comps.len() == 1 {
                let l = comps[0].plane[i];
                [l, l, l, 255]
            } else if ycc {
                let (yy, cb, cr) = (comps[0].plane[i] as i32, comps[1].plane[i] as usize, comps[2].plane[i] as usize);
                let lim = |v: i32| v.clamp(0, 255) as u8;
                [lim(yy + rr[cr]), lim(yy + ((bg[cb] + rg[cr]) >> 16) as i32), lim(yy + bb[cb]), 255]
            } else {
                [comps[0].plane[i], comps[1].plane[i], comps[2].plane[i], 255]
            };
            out.px[o..o + 4].copy_from_slice(&px);
        }
    }
    Ok(out)
}

/// jdcolor.c `build_ycc_rgb_table`.
fn ycc_tables() -> ([i32; 256], [i32; 256], [i64; 256], [i64; 256]) {
    const ONE_HALF: i64 = 1 << 15;
    let fix = |x: f64| (x * 65536.0 + 0.5) as i64;
    let (mut rr, mut bb, mut rg, mut bg) = ([0i32; 256], [0i32; 256], [0i64; 256], [0i64; 256]);
    for i in 0..256 {
        let x = i as i64 - 128;
        rr[i] = ((fix(1.40200) * x + ONE_HALF) >> 16) as i32;
        bb[i] = ((fix(1.77200) * x + ONE_HALF) >> 16) as i32;
        rg[i] = -fix(0.71414) * x;
        bg[i] = -fix(0.34414) * x + ONE_HALF;
    }
    (rr, bb, rg, bg)
}
