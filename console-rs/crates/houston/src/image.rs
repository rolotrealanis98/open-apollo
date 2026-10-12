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
}

/// Thread-safe decode cache. Keys are `images/`-relative paths, `plugin:<EffectName>`, or
/// `<scheme>:<relative path>` for a root registered with [`ImageCache::register_root`] (for
/// example `spec:Images/UA_dbx-160_PANEL@2x.png` for plug-in panel sprites).
pub struct ImageCache {
    resources: Resources,
    map: Mutex<HashMap<String, Option<Arc<ImageData>>>>,
    roots: Mutex<HashMap<String, PathBuf>>,
}

impl ImageCache {
    /// Scheme for plug-in panel spec folders (`spec:<path relative to the spec folder>`).
    pub const SPEC_SCHEME: &'static str = "spec";

    pub fn new(resources: Resources) -> Self {
        Self { resources, map: Mutex::new(HashMap::new()), roots: Mutex::new(HashMap::new()) }
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
