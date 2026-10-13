//! Decoding UA's embedded images to RGBA the way the maintainers' Pillow
//! 12.3.0 pipeline did, so recipe hashes carry over.

use super::img::Rgba;
use super::jpeg;

/// How a blob's bytes become pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decode {
    /// PNG; a `tEXt` InterleaveY/InterleaveX pair (Softube MPACK art) is undone as the plug-in does.
    Png,
    /// PNG rows/columns as stored, ignoring InterleaveY/InterleaveX.
    PngRaw,
    Jpeg,
    /// A BMP file, colours as Pillow reads them (32-bit BI_RGB ignores the alpha byte).
    Bmp,
    /// A 32-bit premultiplied BMP (JUCE 1.52 art): alpha kept, colour divided by it.
    BmpPremultiplied,
    /// A BITMAPINFO without a file header (RT_BITMAP).
    Dib,
    DibPremultiplied,
    /// Softube MPK1: zlib-compressed BGRA.
    Mpk1,
    /// SVG rendered with resvg at `zoom`.
    Svg(u8),
}

impl Decode {
    pub fn parse(s: &str) -> Option<Decode> {
        Some(match s {
            "png" => Decode::Png,
            "png-raw" | "png-raw-interleaved" => Decode::PngRaw,
            "jpeg" => Decode::Jpeg,
            "bmp" => Decode::Bmp,
            "bmp-premultiplied" | "bmp-unpremul" => Decode::BmpPremultiplied,
            "dib" => Decode::Dib,
            "dib-premultiplied" | "dib-unpremul" => Decode::DibPremultiplied,
            "mpk1" => Decode::Mpk1,
            "svg" | "svg-z1" => Decode::Svg(1),
            "svg-z2" => Decode::Svg(2),
            _ => return None,
        })
    }
}

pub fn decode(b: &[u8], how: Decode) -> Result<Rgba, String> {
    match how {
        Decode::Png => {
            let (im, text) = png(b)?;
            let get = |k: &str| {
                text.iter().find(|(key, _)| key == k).and_then(|(_, v)| v.trim().parse::<u32>().ok()).unwrap_or(1)
            };
            Ok(deinterleave(&im, get("InterleaveX").max(1), get("InterleaveY").max(1)))
        }
        Decode::PngRaw => Ok(png(b)?.0),
        Decode::Jpeg => jpeg::decode(b),
        Decode::Bmp => bmp(b, false),
        Decode::BmpPremultiplied => bmp(b, true),
        Decode::Dib => bmp(&dib_to_bmp(b)?, false),
        Decode::DibPremultiplied => bmp(&dib_to_bmp(b)?, true),
        Decode::Mpk1 => mpk1(b),
        Decode::Svg(zoom) => svg(b, zoom as f32),
    }
}

fn u16_le(b: &[u8], o: usize) -> Result<u16, String> {
    b.get(o..o + 2).map(|x| u16::from_le_bytes([x[0], x[1]])).ok_or_else(|| "truncated bitmap".to_string())
}

fn u32_le(b: &[u8], o: usize) -> Result<u32, String> {
    b.get(o..o + 4).map(|x| u32::from_le_bytes([x[0], x[1], x[2], x[3]])).ok_or_else(|| "truncated bitmap".to_string())
}

/// PNG to RGBA plus its tEXt pairs. Chunk CRCs and the zlib checksum are not
/// enforced: a few UA PNGs carry a bad CRC, which Pillow also tolerates.
fn png(b: &[u8]) -> Result<(Rgba, Vec<(String, String)>), String> {
    let mut dec = png::Decoder::new(std::io::Cursor::new(b));
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    dec.ignore_checksums(true);
    let mut rd = dec.read_info().map_err(|e| format!("PNG: {e}"))?;
    let mut buf = vec![0; rd.output_buffer_size().ok_or("PNG too large")?];
    let info = rd.next_frame(&mut buf).map_err(|e| format!("PNG: {e}"))?;
    let (w, h) = (info.width, info.height);
    let n = w as usize * h as usize;
    let line = info.line_size;
    let mut px = Vec::with_capacity(n * 4);
    use png::ColorType as C;
    let ch = match info.color_type {
        C::Grayscale => 1,
        C::GrayscaleAlpha => 2,
        C::Rgb => 3,
        C::Rgba => 4,
        C::Indexed => return Err("PNG palette not expanded".into()),
    };
    for y in 0..h as usize {
        let row = &buf[y * line..y * line + w as usize * ch];
        for p in row.chunks_exact(ch) {
            px.extend_from_slice(&match ch {
                1 => [p[0], p[0], p[0], 255],
                2 => [p[0], p[0], p[0], p[1]],
                3 => [p[0], p[1], p[2], 255],
                _ => [p[0], p[1], p[2], p[3]],
            });
        }
    }
    let text = text_chunks(b);
    Ok((Rgba::from_raw(w, h, px).ok_or("PNG size")?, text))
}

/// The `tEXt` keyword/value pairs of a PNG (latin-1).
fn text_chunks(b: &[u8]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut j = 8;
    while j + 8 <= b.len() {
        let len = u32::from_be_bytes([b[j], b[j + 1], b[j + 2], b[j + 3]]) as usize;
        let ty = &b[j + 4..j + 8];
        let Some(data) = b.get(j + 8..j + 8 + len) else { break };
        if ty == b"tEXt"
            && let Some(z) = data.iter().position(|&c| c == 0)
        {
            let latin = |s: &[u8]| s.iter().map(|&c| c as char).collect::<String>();
            out.push((latin(&data[..z]), latin(&data[z + 1..])));
        }
        if ty == b"IEND" {
            break;
        }
        j += 12 + len;
    }
    out
}

/// Undo Softube's row/column interleave: output row i is input row
/// `gy[i]` where `gy` lists rows 0, ny, 2ny, …, then 1, 1+ny, …
fn deinterleave(im: &Rgba, nx: u32, ny: u32) -> Rgba {
    if nx <= 1 && ny <= 1 {
        return im.clone();
    }
    let order = |n: u32, k: u32| -> Vec<u32> { (0..k.min(n)).flat_map(|p| (p..n).step_by(k as usize)).collect() };
    let (gy, gx) = (order(im.h, ny), order(im.w, nx));
    let mut out = Rgba::new(im.w, im.h);
    for (y, &sy) in gy.iter().enumerate() {
        for (x, &sx) in gx.iter().enumerate() {
            let (s, d) = (im.idx(sx, sy), out.idx(x as u32, y as u32));
            out.px[d..d + 4].copy_from_slice(&im.px[s..s + 4]);
        }
    }
    out
}

/// Wrap an RT_BITMAP DIB in a BMP file header.
fn dib_to_bmp(b: &[u8]) -> Result<Vec<u8>, String> {
    let hdr = u32_le(b, 0)?;
    let bpp = u16_le(b, 14)?;
    let comp = u32_le(b, 16)?;
    let used = u32_le(b, 32)?;
    let ncol = if used != 0 {
        used
    } else if bpp <= 8 {
        1 << bpp
    } else {
        0
    };
    let extra = if comp == 3 && hdr == 40 { 12 } else { 0 };
    let off = 14 + hdr + extra + ncol * 4;
    let mut f = Vec::with_capacity(14 + b.len());
    f.extend_from_slice(b"BM");
    f.extend_from_slice(&(14 + b.len() as u32).to_le_bytes());
    f.extend_from_slice(&[0, 0, 0, 0]);
    f.extend_from_slice(&off.to_le_bytes());
    f.extend_from_slice(b);
    Ok(f)
}

/// BMP with Pillow's BmpImagePlugin rules for what UA ships (BITMAPINFOHEADER,
/// BI_RGB, 1/4/8/24/32 bits). `premultiplied`: agent C's rule for JUCE 1.52's
/// 32-bit art — keep the alpha byte and divide colour by it (float32, truncated);
/// an all-zero alpha band means opaque.
fn bmp(b: &[u8], premultiplied: bool) -> Result<Rgba, String> {
    if b.get(..2) != Some(b"BM") {
        return Err("not a BMP".into());
    }
    let off = u32_le(b, 10)? as usize;
    let hdr = u32_le(b, 14)? as usize;
    let w = u32_le(b, 18)? as i32;
    let h = u32_le(b, 22)? as i32;
    let bpp = u16_le(b, 28)?;
    let comp = u32_le(b, 30)?;
    if w <= 0 || h == 0 || hdr < 40 {
        return Err("unsupported BMP header".into());
    }
    let (w, ah) = (w as usize, h.unsigned_abs() as usize);
    let row = |y: usize| if h > 0 { ah - 1 - y } else { y };
    let mut out = Rgba::new(w as u32, ah as u32);
    if premultiplied {
        if bpp != 32 || !(comp == 0 || comp == 3) {
            return Err("premultiplied BMP must be 32-bit".into());
        }
        let data = b.get(off..off + w * ah * 4).ok_or("truncated BMP")?;
        let any_alpha = data.as_chunks::<4>().0.iter().any(|p| p[3] != 0);
        for y in 0..ah {
            for x in 0..w {
                let s = (row(y) * w + x) * 4;
                let p = &data[s..s + 4];
                let a = p[3];
                let d = (y * w + x) * 4;
                out.px[d..d + 4].copy_from_slice(&if !any_alpha {
                    [p[2], p[1], p[0], 255]
                } else if a == 0 {
                    [0, 0, 0, 0]
                } else {
                    let k = 255.0f32 / a as f32;
                    let f = |c: u8| (c as f32 * k).clamp(0.0, 255.0) as u8;
                    [f(p[2]), f(p[1]), f(p[0]), a]
                });
            }
        }
        return Ok(out);
    }
    if comp != 0 {
        return Err(format!("BMP compression {comp} is not supported"));
    }
    let stride = (w * bpp as usize).div_ceil(32) * 4;
    let used = u32_le(b, 46)? as usize;
    let ncol = if used != 0 { used } else { 1usize << bpp.min(8) };
    let pal_at = 14 + hdr;
    let pal = |i: usize| -> [u8; 4] {
        match b.get(pal_at + i * 4..pal_at + i * 4 + 3) {
            Some(q) if i < ncol => [q[2], q[1], q[0], 255],
            _ => [0, 0, 0, 255],
        }
    };
    for y in 0..ah {
        let r = b.get(off + row(y) * stride..off + row(y) * stride + stride).ok_or("truncated BMP")?;
        for x in 0..w {
            let px = match bpp {
                32 => [r[x * 4 + 2], r[x * 4 + 1], r[x * 4], 255],
                24 => [r[x * 3 + 2], r[x * 3 + 1], r[x * 3], 255],
                8 => pal(r[x] as usize),
                4 => pal(((r[x / 2] >> (4 - 4 * (x % 2))) & 15) as usize),
                1 => pal(((r[x / 8] >> (7 - x % 8)) & 1) as usize),
                _ => return Err(format!("{bpp}-bit BMP is not supported")),
            };
            let d = (y * w + x) * 4;
            out.px[d..d + 4].copy_from_slice(&px);
        }
    }
    Ok(out)
}

fn mpk1(b: &[u8]) -> Result<Rgba, String> {
    if b.get(..4) != Some(b"MPK1") {
        return Err("not MPK1".into());
    }
    let (w, h, n) = (u32_le(b, 4)?, u32_le(b, 8)?, u32_le(b, 16)? as usize);
    let z = b.get(20..20 + n).ok_or("truncated MPK1")?;
    let mut raw = Vec::new();
    use std::io::Read;
    flate2::read::ZlibDecoder::new(z).read_to_end(&mut raw).map_err(|e| format!("MPK1: {e}"))?;
    let need = w as usize * h as usize * 4;
    if raw.len() < need {
        return Err("MPK1 data short".into());
    }
    raw.truncate(need);
    for p in raw.as_chunks_mut::<4>().0 {
        p.swap(0, 2);
    }
    Rgba::from_raw(w, h, raw).ok_or_else(|| "MPK1 size".into())
}

/// SVG rendered by resvg (pinned in Cargo.toml) at its own size × `zoom`, no system fonts.
fn svg(b: &[u8], zoom: f32) -> Result<Rgba, String> {
    let opt = resvg::usvg::Options::default();
    let tree = resvg::usvg::Tree::from_data(b, &opt).map_err(|e| format!("SVG: {e}"))?;
    let size = tree.size();
    let (w, h) = ((size.width() * zoom).ceil() as u32, (size.height() * zoom).ceil() as u32);
    let mut pm = resvg::tiny_skia::Pixmap::new(w.max(1), h.max(1)).ok_or("SVG size")?;
    resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(zoom, zoom), &mut pm.as_mut());
    let mut px = Vec::with_capacity(w as usize * h as usize * 4);
    for p in pm.pixels() {
        let c = p.demultiply();
        px.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
    }
    Rgba::from_raw(w.max(1), h.max(1), px).ok_or_else(|| "SVG size".into())
}
