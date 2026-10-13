//! Runs a recipe pipeline against the install's files.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use super::decode::{Decode, decode};
use super::img::{Rgba, sha256};
use super::pe;
use super::recipe::{Blob, Book, Layer, Op};
use super::resample::{Filter, resize, resize_nearest};

/// An input file's bytes and parsed resource directory.
pub struct FileData {
    pub bytes: Vec<u8>,
    pub sha256: String,
    leaves: Option<Vec<pe::Leaf>>,
}

impl FileData {
    pub fn new(bytes: Vec<u8>) -> FileData {
        let sha256 = sha256(&bytes);
        FileData { bytes, sha256, leaves: None }
    }

    fn leaves(&mut self) -> &[pe::Leaf] {
        if self.leaves.is_none() {
            self.leaves = Some(pe::resources(&self.bytes).unwrap_or_default());
        }
        self.leaves.as_deref().unwrap_or(&[])
    }
}

/// Per-worker state: open files (a few, most recent first), decoded blobs, built sprites.
pub struct Runner<'a> {
    pub book: &'a Book,
    /// input id → path in the install
    pub paths: &'a HashMap<String, PathBuf>,
    files: Vec<(String, FileData)>,
    blobs: HashMap<String, Arc<Rgba>>,
    built: HashMap<String, Arc<Rgba>>,
    /// input id → file SHA-256 of every input read
    pub read: HashMap<String, String>,
}

const OPEN_FILES: usize = 3;

impl<'a> Runner<'a> {
    pub fn new(book: &'a Book, paths: &'a HashMap<String, PathBuf>) -> Runner<'a> {
        Runner { book, paths, files: Vec::new(), blobs: HashMap::new(), built: HashMap::new(), read: HashMap::new() }
    }

    /// Forget decoded blobs and built sprites (keeps open files).
    pub fn clear_cache(&mut self) {
        self.blobs.clear();
        self.built.clear();
    }

    fn file(&mut self, input: &str) -> Result<&mut FileData, String> {
        if let Some(i) = self.files.iter().position(|(k, _)| k == input) {
            let f = self.files.remove(i);
            self.files.insert(0, f);
        } else {
            let path = self.paths.get(input).ok_or_else(|| format!("{input} not found in the install"))?;
            let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
            let fd = FileData::new(bytes);
            self.read.insert(input.to_string(), fd.sha256.clone());
            self.files.insert(0, (input.to_string(), fd));
            self.files.truncate(OPEN_FILES);
            // decoded blobs of a closed file may still be reused by later sprites; keep them bounded
            if self.blobs.len() > 512 {
                self.blobs.clear();
            }
        }
        Ok(&mut self.files[0].1)
    }

    fn blob(&mut self, input: &str, res: Option<&str>, carve: Option<[u64; 2]>, sha: &str) -> Result<Vec<u8>, String> {
        let f = self.file(input)?;
        let leaves = if res.is_some() { f.leaves().to_vec() } else { Vec::new() };
        let f = &*f;
        let found: Option<&[u8]> = if let Some(r) = res {
            let mut it = r.splitn(3, '/');
            let (ty, name, lang) = (it.next().unwrap_or(""), it.next().unwrap_or(""), it.next());
            let lang = lang.and_then(|l| l.parse::<u32>().ok());
            pe::find(&f.bytes, &leaves, ty, name, lang).or_else(|| pe::find(&f.bytes, &leaves, ty, name, None))
        } else if let Some([off, len]) = carve {
            f.bytes.get(off as usize..(off + len) as usize)
        } else {
            return Err("load needs res or carve".into());
        };
        if let Some(b) = found
            && sha256(b) == sha
        {
            return Ok(b.to_vec());
        }
        // The blob moved (another build of the DLL): look for it by length and hash.
        if let Some([_, len]) = carve {
            let len = len as usize;
            let magics: [&[u8]; 4] = [b"\x89PNG\r\n\x1a\n", b"\xff\xd8\xff", b"<?xml", b"<svg"];
            for m in magics {
                let mut i = 0;
                while let Some(p) = find_from(&f.bytes, m, i) {
                    if let Some(b) = f.bytes.get(p..p + len)
                        && sha256(b) == sha
                    {
                        return Ok(b.to_vec());
                    }
                    i = p + 1;
                }
            }
        }
        for l in &leaves {
            if let Some(b) = f.bytes.get(l.offset..l.offset + l.size)
                && sha256(b) == sha
            {
                return Ok(b.to_vec());
            }
        }
        let what = res.map_or_else(|| "carve".to_string(), str::to_string);
        Err(match found {
            Some(_) => format!("{input}: blob {what} differs (another UAD version?)"),
            None => format!("{input}: blob {what} not found"),
        })
    }

    /// Build sprite `id` (and anything it references).
    pub fn sprite(&mut self, id: &str, depth: usize) -> Result<Arc<Rgba>, String> {
        if let Some(im) = self.built.get(id) {
            return Ok(im.clone());
        }
        if depth > 8 {
            return Err(format!("sprite {id}: references nest too deep"));
        }
        let fi = *self.book.sprite_at.get(id).ok_or_else(|| format!("unknown sprite {id}"))?;
        let s = &self.book.files[fi].sprites[id];
        if !s.status.buildable() {
            return Err(format!("sprite {id} is {}", s.status.as_str()));
        }
        let ops = s.build.clone().ok_or_else(|| format!("sprite {id} has no build"))?;
        let im = Arc::new(self.pipeline(&ops, depth + 1, fi)?);
        self.built.insert(id.to_string(), im.clone());
        Ok(im)
    }

    /// The blob a `load` step names, from the file's `blobs` table or inline.
    fn resolve(&self, op: &Op, fi: usize) -> Result<Blob, String> {
        let Op::Load { blob, input, res, carve, sha, inflate, size, dec } = op else {
            return Err("not a load".into());
        };
        if let Some(k) = blob {
            return self.book.files[fi].blobs.get(k).cloned().ok_or_else(|| format!("unknown blob {k}"));
        }
        Ok(Blob {
            input: input.clone().ok_or("load needs in")?,
            res: res.clone(),
            carve: *carve,
            sha: sha.clone().ok_or("load needs sha")?,
            inflate: inflate.clone(),
            size: *size,
            dec: dec.clone().ok_or("load needs dec")?,
        })
    }

    pub fn pipeline(&mut self, ops: &[Op], depth: usize, fi: usize) -> Result<Rgba, String> {
        let (first, rest) = ops.split_first().ok_or("empty pipeline")?;
        let mut im: Rgba = match first {
            Op::Load { .. } => {
                let Blob { input, res, carve, sha, inflate, size, dec } = self.resolve(first, fi)?;
                let key = format!("{input}|{res:?}|{carve:?}|{inflate:?}|{dec}");
                if let Some(b) = self.blobs.get(&key) {
                    (**b).clone()
                } else {
                    let how = Decode::parse(&dec).ok_or_else(|| format!("unknown decode {dec}"))?;
                    let mut bytes = self.blob(&input, res.as_deref(), carve, &sha)?;
                    match inflate.as_deref() {
                        None => {}
                        Some("deflate-raw") => bytes = inflate_raw(&bytes, size.ok_or("inflate needs size")?)?,
                        Some(other) => return Err(format!("unknown inflate {other}")),
                    }
                    let im = Arc::new(decode(&bytes, how)?);
                    self.blobs.insert(key, im.clone());
                    (*im).clone()
                }
            }
            Op::Canvas { w, h, rgba } => Rgba::filled(*w, *h, *rgba),
            Op::Sprite { id } => (*self.sprite(id, depth)?).clone(),
            Op::Stack { parts, axis } => {
                let mut imgs = Vec::new();
                for p in parts {
                    imgs.push(self.pipeline(p, depth, fi)?);
                }
                stack(&imgs, axis.as_deref() == Some("x"))
            }
            Op::ThumbFader { track, thumb, w, h, k } => {
                let track = self.pipeline(track, depth, fi)?;
                let thumb = self.pipeline(thumb, depth, fi)?;
                thumb_fader(&track, &thumb, *w, *h, *k)?
            }
            Op::Slide { thumb, n, w, h, x, y, clip } => {
                let thumb = self.pipeline(thumb, depth, fi)?;
                slide(&thumb, *n, *w, *h, *x, *y, *clip)?
            }
            Op::Layers { n, layers } => self.layers(*n, layers, depth, fi)?,
            Op::Line { w, h, xy, width, rgba } => super::draw::line(*w, *h, *xy, *width, *rgba),
            other => return Err(format!("pipeline must start with a source, not {other:?}")),
        };
        for op in rest {
            im = self.step(im, op, depth, fi)?;
        }
        Ok(im)
    }

    fn step(&mut self, im: Rgba, op: &Op, depth: usize, fi: usize) -> Result<Rgba, String> {
        Ok(match op {
            Op::Load { .. }
            | Op::Canvas { .. }
            | Op::Sprite { .. }
            | Op::Stack { .. }
            | Op::ThumbFader { .. }
            | Op::Slide { .. }
            | Op::Layers { .. }
            | Op::Line { .. } => {
                return Err("a source step can only start a pipeline".into());
            }
            Op::Crop { bx: [x, y, w, h] } => {
                if *w < 0 || *h < 0 {
                    return Err("negative crop".into());
                }
                im.crop(*x, *y, *w as u32, *h as u32)
            }
            Op::Lanczos2x => resize(&im, im.w * 2, im.h * 2, Filter::Lanczos, None),
            Op::Resize { w, h, filter, bx } => {
                if filter == "nearest" {
                    resize_nearest(&im, *w, *h, *bx)
                } else {
                    let f = Filter::parse(filter).ok_or_else(|| format!("unknown filter {filter}"))?;
                    resize(&im, *w, *h, f, *bx)
                }
            }
            Op::Paste { src, at, blend } => {
                let s = self.pipeline(src, depth, fi)?;
                let mut out = im;
                match blend.as_deref().unwrap_or("over") {
                    "over" => out.composite(&s, at[0], at[1]),
                    "replace" => out.paste(&s, at[0], at[1]),
                    "mask" => out.paste_masked(&s, &s.alpha(), at[0], at[1]),
                    b => return Err(format!("unknown blend {b}")),
                }
                out
            }
            Op::MeterFill { n, vertical } => meter_fill(&im, *n, *vertical)?,
            Op::Flip { axis } => flip(&im, axis == "y"),
            Op::PremulTint { rgb, a } => {
                if *a > 256 {
                    return Err("premul-tint: a is 0..=256".into());
                }
                let mut out = im;
                out.premul_tint(*rgb, *a);
                out
            }
            Op::Reorder { n, order } => reorder(&im, *n, order)?,
            Op::Affine { matrix, resample } => {
                if resample != "bicubic" {
                    return Err(format!("affine: resample {resample} is not supported"));
                }
                super::geometry::affine_bicubic(&im, *matrix)
            }
            Op::Feather { px } => {
                if *px <= 0.0 {
                    return Err("feather: px must be > 0".into());
                }
                let mut out = im;
                out.feather(*px);
                out
            }
            Op::Tint { rgb } => {
                let mut out = im;
                out.tint(*rgb);
                out
            }
            Op::JucePremulRoundtrip => {
                let mut out = im;
                out.juce_premul_roundtrip();
                out
            }
        })
    }

    fn layers(&mut self, n: u32, layers: &[Layer], depth: usize, fi: usize) -> Result<Rgba, String> {
        let mut ims = Vec::new();
        for l in layers {
            ims.push((self.pipeline(&l.src, depth, fi)?, l.frames.max(1)));
        }
        let fw = ims.iter().map(|(i, _)| i.w).max().unwrap_or(0);
        let fh = ims.iter().map(|(i, fr)| i.h / fr).max().unwrap_or(0);
        let mut sheet = Rgba::new(fw, fh * n);
        for k in 0..n {
            for (im, fr) in &ims {
                if *fr == 1 {
                    sheet.composite(im, 0, (k * fh) as i64);
                } else {
                    let th = im.h / fr;
                    let tile = im.crop(0, (k * th) as i64, im.w, th);
                    sheet.composite(&tile, 0, (k * fh) as i64);
                }
            }
        }
        Ok(sheet)
    }
}

/// A raw DEFLATE stream that must end exactly where the bytes end and give `size` bytes.
pub(crate) fn inflate_raw(b: &[u8], size: usize) -> Result<Vec<u8>, String> {
    let mut d = flate2::Decompress::new(false);
    let mut out = Vec::with_capacity(size + 1);
    let st = d.decompress_vec(b, &mut out, flate2::FlushDecompress::Finish).map_err(|e| format!("inflate: {e}"))?;
    if st != flate2::Status::StreamEnd || d.total_in() as usize != b.len() || out.len() != size {
        return Err("inflate: stream does not end exactly or has the wrong size".into());
    }
    Ok(out)
}

fn find_from(h: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    h.get(from..)?.windows(needle.len()).position(|w| w == needle).map(|p| p + from)
}

fn stack(parts: &[Rgba], along_x: bool) -> Rgba {
    let (w, h) = if along_x {
        (parts.iter().map(|p| p.w).sum(), parts.iter().map(|p| p.h).max().unwrap_or(0))
    } else {
        (parts.iter().map(|p| p.w).max().unwrap_or(0), parts.iter().map(|p| p.h).sum())
    };
    let mut out = Rgba::new(w, h);
    let mut at = 0i64;
    for p in parts {
        if along_x {
            out.paste(p, at, 0);
            at += p.w as i64;
        } else {
            out.paste(p, 0, at);
            at += p.h as i64;
        }
    }
    out
}

/// Python's `round()` on a float: halves go to the even neighbour.
fn py_round(x: f64) -> i64 {
    x.round_ties_even() as i64
}

fn thumb_fader(track: &Rgba, thumb: &Rgba, w: u32, h: u32, k: u32) -> Result<Rgba, String> {
    let horiz = w >= h;
    let (tw, th) = (thumb.w as i64, thumb.h as i64);
    let (w, h) = (w as i64, h as i64);
    let travel = if horiz { w - tw } else { h - th };
    if k == 0 || travel < k as i64 {
        return Err("thumb-fader: no travel".into());
    }
    let n = travel / k as i64 + 1;
    let mut out = Rgba::new(w as u32, (h * n) as u32);
    for i in 0..n {
        let mut fr = track.clone();
        let off = py_round(i as f64 * travel as f64 / (n - 1) as f64);
        let (x, y) = if horiz { (off, (h - th).div_euclid(2)) } else { ((w - tw).div_euclid(2), travel - off) };
        fr.composite(thumb, x, y);
        out.paste(&fr, 0, i * h);
    }
    Ok(out)
}

fn slide(thumb: &Rgba, n: u32, w: u32, h: u32, x: [i64; 3], y: [i64; 3], clip: bool) -> Result<Rgba, String> {
    if n < 2 {
        return Err("slide needs at least 2 frames".into());
    }
    let mut out = Rgba::new(w, h * n);
    for i in 0..n {
        let v = i as f64 / (n - 1) as f64;
        let pos = |[base, span, dir]: [i64; 3]| -> i64 {
            match dir {
                1 => base + py_round(v * span as f64),
                -1 => base + py_round((1.0 - v) * span as f64),
                _ => base,
            }
        };
        let (px, py) = (pos(x).max(0), pos(y).max(0));
        if clip {
            let (cw, ch) = ((w as i64 - px).clamp(0, thumb.w as i64), (h as i64 - py).clamp(0, thumb.h as i64));
            out.composite(&thumb.crop(0, 0, cw as u32, ch as u32), px, (i * h) as i64 + py);
        } else {
            out.composite(thumb, px, (i * h) as i64 + py);
        }
    }
    Ok(out)
}

fn meter_fill(src: &Rgba, n: u32, vertical: bool) -> Result<Rgba, String> {
    if n < 2 {
        return Err("meter-fill needs at least 2 frames".into());
    }
    let (w2, h2) = (src.w as i64, src.h as i64);
    let mut out = Rgba::new(src.w, src.h * n);
    for k in 0..n as i64 {
        let f = k as f64 / (n - 1) as f64;
        if vertical {
            let cut = py_round(h2 as f64 * (1.0 - f));
            let part = src.crop(0, cut, w2 as u32, (h2 - cut).max(0) as u32);
            out.paste(&part, 0, k * h2 + cut);
        } else {
            let cut = py_round(w2 as f64 * f);
            let part = src.crop(0, 0, cut.max(0) as u32, h2 as u32);
            out.paste(&part, 0, k * h2);
        }
    }
    Ok(out)
}

pub(crate) fn reorder(im: &Rgba, n: u32, order: &[u32]) -> Result<Rgba, String> {
    if n == 0 || !im.h.is_multiple_of(n) {
        return Err(format!("reorder: height {} is not {n} frames", im.h));
    }
    let fh = im.h / n;
    let mut out = Rgba::new(im.w, fh * order.len() as u32);
    for (i, &j) in order.iter().enumerate() {
        if j >= n {
            return Err(format!("reorder: frame {j} out of range"));
        }
        out.paste(&im.crop(0, (j * fh) as i64, im.w, fh), 0, (i as u32 * fh) as i64);
    }
    Ok(out)
}

fn flip(im: &Rgba, vertical: bool) -> Rgba {
    let mut out = Rgba::new(im.w, im.h);
    for y in 0..im.h {
        for x in 0..im.w {
            let (sx, sy) = if vertical { (x, im.h - 1 - y) } else { (im.w - 1 - x, y) };
            let (s, d) = (im.idx(sx, sy), out.idx(x, y));
            out.px[d..d + 4].copy_from_slice(&im.px[s..s + 4]);
        }
    }
    out
}
