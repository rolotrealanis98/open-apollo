//! Decoded RGBA images from `Resources/images` and plug-in icons, cached by path.

use std::collections::HashMap;
use std::path::Path;
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

/// Thread-safe decode cache. Keys are `images/`-relative paths or `plugin:<EffectName>`.
pub struct ImageCache {
    resources: Resources,
    map: Mutex<HashMap<String, Option<Arc<ImageData>>>>,
}

impl ImageCache {
    pub fn new(resources: Resources) -> Self {
        Self { resources, map: Mutex::new(HashMap::new()) }
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
            Some(name) => self.resources.plugin_icons_dir().join(format!("{name}.png")),
            None => self.resources.images_dir().join(key),
        };
        let decoded = ImageData::decode_png(&path).map(Arc::new);
        self.map.lock().insert(key.to_string(), decoded.clone());
        decoded
    }

    /// Keys that failed to load.
    pub fn missing(&self) -> Vec<String> {
        let mut v: Vec<String> = self.map.lock().iter().filter(|(_, v)| v.is_none()).map(|(k, _)| k.clone()).collect();
        v.sort();
        v
    }
}
