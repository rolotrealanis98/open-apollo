//! Decoded RGBA images from `Resources/images` and plug-in icons, cached by path.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;

use crate::resources::Resources;

/// Straight-alpha RGBA8 pixels.
#[derive(Debug)]
pub struct ImageData {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl ImageData {
    /// Decodes a PNG (any bit depth / colour type) to RGBA8. Non-PNG files (the two PSDs
    /// shipped as `.png`, SVGs) return None.
    pub fn decode_png(path: &Path) -> Option<Self> {
        let file = std::fs::File::open(path).ok()?;
        let mut decoder = png::Decoder::new(std::io::BufReader::new(file));
        decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        let mut reader = decoder.read_info().ok()?;
        let mut buf = vec![0; reader.output_buffer_size()?];
        let info = reader.next_frame(&mut buf).ok()?;
        buf.truncate(info.buffer_size());
        let (w, h) = (info.width, info.height);
        let rgba = match info.color_type {
            png::ColorType::Rgba => buf,
            png::ColorType::Rgb => buf.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
            png::ColorType::GrayscaleAlpha => buf.chunks_exact(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect(),
            png::ColorType::Grayscale => buf.iter().flat_map(|&g| [g, g, g, 255]).collect(),
            png::ColorType::Indexed => return None, // EXPAND converts palettes; not reached
        };
        Some(Self { width: w, height: h, rgba })
    }

    pub fn bytes(&self) -> usize {
        self.rgba.len()
    }

    /// Premultiplied RGBA8 of the source rect `(x, y, w, h)` (image px), area-averaged (box
    /// filter with fractional pixel coverage) to `out_w`×`out_h`. This is what CoreGraphics'
    /// high-quality downscale gives UAD Console's plug-in icons (RMS error 2.7/255 on refpair p1,
    /// against 8.5 for bilinear and 7.8 for trilinear mipmaps).
    pub fn resample_area_premul(&self, src: (u32, u32, u32, u32), out_w: u32, out_h: u32) -> Vec<u8> {
        let (sx, sy, sw, sh) = src;
        let (sw, sh) = (sw.min(self.width.saturating_sub(sx)), sh.min(self.height.saturating_sub(sy)));
        let (ow, oh) = (out_w.max(1) as usize, out_h.max(1) as usize);
        if sw == 0 || sh == 0 {
            return vec![0; ow * oh * 4];
        }
        let xw = area_weights(sw as usize, ow);
        let yw = area_weights(sh as usize, oh);
        // Horizontal pass into f32 premultiplied rows.
        let stride = self.width as usize * 4;
        let mut tmp = vec![0f32; sh as usize * ow * 4];
        for r in 0..sh as usize {
            let row = &self.rgba[(sy as usize + r) * stride + sx as usize * 4..];
            for (o, taps) in xw.iter().enumerate() {
                let mut acc = [0f32; 4];
                for &(i, w) in taps {
                    let p = &row[i * 4..i * 4 + 4];
                    let a = p[3] as f32 / 255.0;
                    acc[0] += p[0] as f32 * a * w;
                    acc[1] += p[1] as f32 * a * w;
                    acc[2] += p[2] as f32 * a * w;
                    acc[3] += p[3] as f32 * w;
                }
                tmp[(r * ow + o) * 4..(r * ow + o) * 4 + 4].copy_from_slice(&acc);
            }
        }
        let mut out = vec![0u8; ow * oh * 4];
        for (o, taps) in yw.iter().enumerate() {
            for x in 0..ow {
                let mut acc = [0f32; 4];
                for &(i, w) in taps {
                    let p = &tmp[(i * ow + x) * 4..(i * ow + x) * 4 + 4];
                    for c in 0..4 {
                        acc[c] += p[c] * w;
                    }
                }
                for c in 0..4 {
                    out[(o * ow + x) * 4 + c] = (acc[c] + 0.5).clamp(0.0, 255.0) as u8;
                }
            }
        }
        out
    }
}

/// Box-filter taps for resizing `n_in` samples to `n_out`: per output sample, the input samples
/// it covers and their coverage weights (summing to 1).
fn area_weights(n_in: usize, n_out: usize) -> Vec<Vec<(usize, f32)>> {
    let scale = n_in as f64 / n_out as f64;
    (0..n_out)
        .map(|o| {
            let (start, end) = (o as f64 * scale, (o + 1) as f64 * scale);
            let mut taps = Vec::new();
            let mut i = start.floor() as usize;
            while (i as f64) < end && i < n_in {
                let w = (end.min(i as f64 + 1.0) - start.max(i as f64)) / scale;
                if w > 1e-9 {
                    taps.push((i, w as f32));
                }
                i += 1;
            }
            taps
        })
        .collect()
}

#[cfg(test)]
mod resample_tests {
    use super::*;

    #[test]
    fn area_weights_cover_input() {
        let w = area_weights(400, 190);
        assert_eq!(w.len(), 190);
        for taps in &w {
            let s: f32 = taps.iter().map(|t| t.1).sum();
            assert!((s - 1.0).abs() < 1e-5);
        }
        assert_eq!(w[0][0], (0, 190.0 / 400.0));
        assert_eq!(area_weights(4, 2), vec![vec![(0, 0.5), (1, 0.5)], vec![(2, 0.5), (3, 0.5)]]);
    }

    #[test]
    fn resample_box_average_premultiplied() {
        // 2x2 → 1x1: opaque red, opaque blue, two transparent pixels.
        let img = ImageData { width: 2, height: 2, rgba: vec![255, 0, 0, 255, 0, 0, 255, 255, 9, 9, 9, 0, 9, 9, 9, 0] };
        assert_eq!(img.resample_area_premul((0, 0, 2, 2), 1, 1), vec![64, 0, 64, 128]);
        // Source sub-rect (a filmstrip frame): the bottom row only.
        assert_eq!(img.resample_area_premul((0, 1, 2, 1), 1, 1), vec![0, 0, 0, 0]);
    }
}

/// Thread-safe decode cache. Keys are `images/`-relative paths, `plugin:<EffectName>`, or
/// `<scheme>:<relative path>` for a root registered with [`ImageCache::register_root`] (for
/// example `spec:Images/UA_dbx-160_PANEL@2x.png` for plug-in panel sprites).
pub struct ImageCache {
    resources: Resources,
    map: Mutex<HashMap<String, Option<Arc<ImageData>>>>,
    roots: Mutex<HashMap<String, PathBuf>>,
    runtime: Mutex<HashMap<String, (String, (u32, u32), u64)>>,
}

impl ImageCache {
    /// Scheme for plug-in panel spec folders (`spec:<path relative to the spec folder>`).
    pub const SPEC_SCHEME: &'static str = "spec";

    pub fn new(resources: Resources) -> Self {
        Self { resources, map: Mutex::new(HashMap::new()), roots: Mutex::new(HashMap::new()), runtime: Mutex::new(HashMap::new()) }
    }

    /// Bounded in-memory correction SVG; nothing is written to the asset directory.
    /// Only three stable keys are allowed, and replaced rather than accumulated.
    pub fn correction_svg(&self, slot: &str, svg: &str, size: (u32,u32)) -> Option<String> {
        if !matches!(slot,"speaker"|"headphone"|"headphone2") || svg.len()>512*1024
            || size.0==0 || size.1==0 || size.0>2048 || size.1>2048 { return None; }
        let key=format!("runtime-correction:{slot}");
        let mut runtime=self.runtime.lock();
        if runtime.get(&key).is_some_and(|(s,z,_)| s==svg && *z==size) {
            return self.map.lock().get(&key).and_then(|i| i.as_ref()).map(|_|key);
        }
        let revision=runtime.get(&key).map_or(1,|(_,_,n)| n.wrapping_add(1));
        let image=render_correction_svg(svg,size).map(Arc::new);
        let good=image.is_some();
        self.map.lock().insert(key.clone(),image);
        runtime.insert(key.clone(),(svg.to_owned(),size,revision));
        good.then_some(key)
    }

    pub fn runtime_revision(&self,key:&str)->u64 {
        if !key.starts_with("runtime-correction:") {return 0}
        self.runtime.lock().get(key).map_or(0,|(_,_,n)| *n)
    }

    /// A cache with one extra root registered (see [`ImageCache::register_root`]).
    pub fn with_root(resources: Resources, scheme: &str, dir: impl Into<PathBuf>) -> Self {
        let cache = Self::new(resources);
        cache.register_root(scheme, dir);
        cache
    }

    /// Resolves keys `<scheme>:<relative path>` against `dir` (e.g. `spec` → a plug-in spec
    /// folder). Registering a new root drops what was cached under the scheme. `plugin` is reserved for
    /// plug-in icons.
    pub fn register_root(&self, scheme: &str, dir: impl Into<PathBuf>) {
        let dir = dir.into();
        let prev = self.roots.lock().insert(scheme.to_string(), dir.clone());
        if prev.as_ref() != Some(&dir) {
            let prefix = format!("{scheme}:");
            self.map.lock().retain(|k, _| !k.starts_with(&prefix));
        }
    }

    /// The folder registered for `scheme`, if any.
    pub fn root(&self, scheme: &str) -> Option<PathBuf> {
        self.roots.lock().get(scheme).cloned()
    }

    pub fn resources(&self) -> &Resources {
        &self.resources
    }

    /// Image by key: `images/`-relative path, or `plugin:<EffectName>` for plug-in icons.
    pub fn get(&self, key: &str) -> Option<Arc<ImageData>> {
        if let Some(hit) = self.map.lock().get(key) {
            return hit.clone();
        }
        let path = match key.strip_prefix("plugin:") {
            Some(name) => plugin_icon_path(&self.resources.plugin_icons_dir(), name),
            None => match self.rooted(key) {
                Some(path) => path,
                None => self.resources.images_dir().join(key),
            },
        };
        let decoded = ImageData::decode_png(&path).map(Arc::new);
        self.map.lock().insert(key.to_string(), decoded.clone());
        decoded
    }

    /// `<scheme>:<rel>` under a registered root. `..` components are refused.
    fn rooted(&self, key: &str) -> Option<PathBuf> {
        let (scheme, rel) = key.split_once(':')?;
        let root = self.roots.lock().get(scheme).cloned()?;
        let rel = Path::new(rel);
        if rel.is_absolute() || rel.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
            return Some(PathBuf::new());
        }
        Some(root.join(rel))
    }

    /// Keys that failed to load.
    pub fn missing(&self) -> Vec<String> {
        let mut v: Vec<String> = self.map.lock().iter().filter(|(_, v)| v.is_none()).map(|(k, _)| k.clone()).collect();
        v.sort();
        v
    }
}

fn render_correction_svg(svg:&str,size:(u32,u32))->Option<ImageData> {
    // Native wrapper supplies these CSS variables (Rack 0x18116b9e0).
    let mut svg=svg.to_owned();
    // The native wrapper assigns the same theme curveColor to all these families.
    for family in ["after","before","before-accuracy","calibration","phase","limits","target","left","right"] {
        for side in ["","-left","-right"] {
            svg=svg.replace(&format!("var(--color-curve-{family}{side})"),"#78c86e");
        }
    }
    svg=svg.replace("var(--font-size-tickmark)","10px");
    let mut options=resvg::usvg::Options::default();
    // Never follow image hrefs into local files or network resources.
    options.image_href_resolver.resolve_string=Box::new(|_,_|None);
    options.image_href_resolver.resolve_data=Box::new(|_,_,_|None);
    options.fontdb_mut().load_system_fonts();
    let tree=resvg::usvg::Tree::from_str(&svg,&options).ok()?;
    if tree.size().width()>4096.0 || tree.size().height()>4096.0 { return None; }
    let mut pixmap=resvg::tiny_skia::Pixmap::new(size.0,size.1)?;
    let transform=resvg::tiny_skia::Transform::from_scale(size.0 as f32/tree.size().width(),size.1 as f32/tree.size().height());
    resvg::render(&tree,transform,&mut pixmap.as_mut());
    // tiny-skia is premultiplied; ImageData's contract is straight RGBA.
    let mut rgba=pixmap.take();
    for p in rgba.chunks_exact_mut(4) {
        if p[3]>0 { for k in 0..3 { p[k]=((p[k] as u32*255+p[3] as u32/2)/p[3] as u32).min(255) as u8; } }
    }
    Some(ImageData{width:size.0,height:size.1,rgba})
}

#[cfg(test)]
mod correction_tests {
    use super::*;
    #[test]
    fn svg_pixels_alpha_cache_replacement_and_bounds() {
        let cache=ImageCache::new(Resources{root:PathBuf::from("/nonexistent"),console_version:None});
        let svg=r#"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="4"><rect width="4" height="4" fill="red" opacity="0.5"/></svg>"#;
        let key=cache.correction_svg("speaker",svg,(4,4)).unwrap();
        let image=cache.get(&key).unwrap();
        assert_eq!(image.rgba[0],255);
        assert!((126..=129).contains(&image.rgba[3]));
        let rev=cache.runtime_revision(&key);
        assert_eq!(cache.correction_svg("speaker",svg,(4,4)).unwrap(),key);
        assert_eq!(cache.runtime_revision(&key),rev);
        let green=svg.replace("red","var(--color-curve-after)");
        cache.correction_svg("speaker",&green,(4,4)).unwrap();
        assert_eq!(cache.runtime_revision(&key),rev+1);
        assert!(cache.get(&key).unwrap().rgba[1]>150);
        assert_eq!(cache.runtime.lock().len(),1);
        assert!(cache.correction_svg("invalid",svg,(4,4)).is_none());
        assert!(cache.correction_svg("speaker",svg,(4096,4096)).is_none());
        assert!(cache.correction_svg("speaker","bad",(4,4)).is_none());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_png(path: &Path, w: u32, h: u32) {
        let file = std::fs::File::create(path).unwrap();
        let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().unwrap().write_image_data(&vec![255; (w * h * 4) as usize]).unwrap();
    }

    #[test]
    fn registered_root_resolves_scheme_keys() {
        let dir = std::env::temp_dir().join(format!("houston-root-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("Images")).unwrap();
        write_png(&dir.join("Images/knob@2x.png"), 4, 8);
        let resources = Resources { root: dir.join("no-console"), console_version: None };
        let cache = ImageCache::new(resources);
        assert!(cache.get("spec:Images/knob@2x.png").is_none(), "unregistered scheme");
        cache.register_root(ImageCache::SPEC_SCHEME, &dir);
        let img = cache.get("spec:Images/knob@2x.png").expect("rooted image");
        assert_eq!((img.width, img.height), (4, 8));
        assert!(cache.get("spec:../etc/passwd").is_none());
        std::fs::remove_dir_all(&dir).ok();
    }
}

/// `<dir>/<name>.png`, matched case-insensitively when the exact file is absent (UA's icon
/// names differ in case from some catalogue names, e.g. "UAD AMS dmx 15-80 S"; macOS
/// volumes are case-insensitive, Linux ones are not).
fn plugin_icon_path(dir: &Path, name: &str) -> PathBuf {
    let file = format!("{name}.png");
    let exact = dir.join(&file);
    if exact.is_file() {
        return exact;
    }
    std::fs::read_dir(dir)
        .ok()
        .and_then(|entries| {
            entries.flatten().map(|e| e.path()).find(|p| p.file_name().and_then(|f| f.to_str()).is_some_and(|f| f.eq_ignore_ascii_case(&file)))
        })
        .unwrap_or(exact)
}
