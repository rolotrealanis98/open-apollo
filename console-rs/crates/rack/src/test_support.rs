//! Unit-test fixtures: an `Assets` over a minimal `Appearance.json`, no Console install needed.

use std::hash::{Hash, Hasher};

use houston::image::ImageData;
use houston::Resources;

use crate::Assets;

/// Assets whose appearance is `json` (one fixture folder per distinct text).
pub(crate) fn assets(json: &str) -> Assets {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    json.hash(&mut h);
    let dir = std::env::temp_dir().join(format!("rack-unit-fixture-{}-{:x}", std::process::id(), h.finish()));
    std::fs::create_dir_all(dir.join("appearance")).unwrap();
    std::fs::write(dir.join("appearance/Appearance.json"), json).unwrap();
    Assets::load(Resources::at(&dir).expect("fixture resources")).expect("fixture appearance")
}

/// Registers an opaque `w`×`h` image as `runtime:<name>` (use that as a skin's image path).
pub(crate) fn raster(assets: &Assets, name: &str, w: u32, h: u32) {
    assets.images.put_raster(name, 1, || Some(ImageData { width: w, height: h, rgba: vec![255; (w * h * 4) as usize] }));
}
