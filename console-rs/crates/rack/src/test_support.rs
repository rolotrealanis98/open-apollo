//! Unit-test fixtures: an `Assets` over a minimal `Appearance.json`, no Console install needed.

use std::hash::{Hash, Hasher};

use houston::image::ImageData;
use houston::Resources;

use crate::Assets;

/// Assets whose appearance is `json` (a fresh fixture folder per call: parallel tests with the
/// same text must not read each other's half-written file).
pub(crate) fn assets(json: &str) -> Assets {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let mut h = std::collections::hash_map::DefaultHasher::new();
    json.hash(&mut h);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rack-unit-fixture-{}-{:x}-{n}", std::process::id(), h.finish()));
    std::fs::create_dir_all(dir.join("appearance")).unwrap();
    std::fs::write(dir.join("appearance/Appearance.json"), json).unwrap();
    Assets::load(Resources::at(&dir).expect("fixture resources")).expect("fixture appearance")
}

/// Registers an opaque `w`×`h` image as `runtime:<name>` (use that as a skin's image path).
pub(crate) fn raster(assets: &Assets, name: &str, w: u32, h: u32) {
    assets.images.put_raster(name, 1, || Some(ImageData { width: w, height: h, rgba: vec![255; (w * h * 4) as usize] }));
}
