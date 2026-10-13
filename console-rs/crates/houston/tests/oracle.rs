//! Compares the Appearance parse with the Python oracle (`swift-manifest.json`, UA data, kept
//! outside the repo). Set HOUSTON_ORACLE and CONSOLE_RESOURCES; skipped otherwise.

use std::collections::HashSet;

use houston::{Appearance, Resources};

#[test]
fn matches_python_oracle() {
    let (Ok(oracle_path), Some(resources)) = (std::env::var("HOUSTON_ORACLE"), Resources::locate()) else {
        eprintln!("skipped: set HOUSTON_ORACLE and CONSOLE_RESOURCES");
        return;
    };
    let oracle: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(oracle_path).unwrap()).unwrap();
    let skins = oracle["skins"].as_object().unwrap();
    let a = Appearance::load(&resources).unwrap();

    let expected: HashSet<&str> =
        skins.keys().map(String::as_str).filter(|k| !k.rsplit('/').next().unwrap().starts_with("__")).collect();
    let actual: HashSet<&str> = a.skins.keys().map(String::as_str).collect();
    assert_eq!(expected.len(), actual.len(), "skin count");
    assert!(expected == actual, "key sets differ: {:?}", expected.symmetric_difference(&actual).take(10).collect::<Vec<_>>());

    let mut image_mismatch = Vec::new();
    let mut film_mismatch = Vec::new();
    for (id, entry) in skins {
        let Some(skin) = a.skins.get(id) else { continue };
        let want: HashSet<String> = entry["img"]
            .as_object()
            .map(|m| {
                m.values()
                    .filter_map(|v| v.as_array()?.first()?.as_str().map(|s| s.replace(" (missing)", "")))
                    .collect()
            })
            .unwrap_or_default();
        let got: HashSet<String> = skin.images().map(|(_, p)| p.to_string()).collect();
        if want != got {
            image_mismatch.push(id.clone());
        }
        if let Some(fh) = entry["film"]["fh"].as_u64() {
            if skin.frame_height() != Some(fh as u32) {
                film_mismatch.push(id.clone());
            }
        }
    }
    assert!(image_mismatch.is_empty(), "{} image mismatches: {:?}", image_mismatch.len(), &image_mismatch[..image_mismatch.len().min(10)]);
    assert!(film_mismatch.is_empty(), "{film_mismatch:?}");
    assert_eq!(a.colors.len(), oracle["colors"].as_object().unwrap().len());
    assert_eq!(a.image_paths.len(), oracle["image_paths"].as_object().unwrap().len());
}
