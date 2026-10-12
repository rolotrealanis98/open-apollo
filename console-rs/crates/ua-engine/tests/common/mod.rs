#![allow(dead_code)]

use ua_engine::{Snapshot, Tree};

pub fn manifest(rel: &str) -> String {
    format!("{}/{rel}", env!("CARGO_MANIFEST_DIR"))
}

/// The sanitized live capture: UA Mixer Engine 12.0.0, Apollo x4 (see README).
pub fn live_x4() -> Tree {
    Snapshot::load(manifest("tests/fixtures/apollo-x4-12.0.0.snapshot.json"))
        .expect("live x4 fixture")
        .tree
}

/// open-apollo's committed device maps (engine 11.8.2 x4, 11.9.0 x8p).
pub fn device_map(model: &str) -> Tree {
    Snapshot::load(manifest(&format!(
        "../../../mixer-engine/device_maps/device_map_apollo_{model}.json"
    )))
    .expect("device map")
    .tree
}

pub fn tree(source: &str) -> Tree {
    match source {
        "live-x4" => live_x4(),
        m => device_map(m),
    }
}
