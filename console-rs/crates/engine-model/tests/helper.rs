//! Native inserts from the UA Mixer Helper merged with UAD-2 inserts, Console's slot labels, and
//! helper status fallbacks.

use std::time::{Duration, Instant};

use engine_model::{EngineMixer, SlotSource};
use rack::model::{InsertState, MixerModel};
use ua_engine::{
    HelperStore, Json, MockEngine, MockHelper, MockOptions, Property, Snapshot, Status, Store,
    Value,
};

fn engine_fixture() -> Snapshot {
    Snapshot::load(format!(
        "{}/../ua-engine/tests/fixtures/apollo-x4-12.0.0.snapshot.json",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

fn helper_fixture() -> Json {
    serde_json::from_slice(
        &std::fs::read(format!(
            "{}/../ua-engine/tests/fixtures/helper-1.3.1.json",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap(),
    )
    .unwrap()
}

fn devices_mut(root: &mut Json) -> &mut Json {
    root["children"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|c| c["path"] == "devices")
        .unwrap()
}

/// SYNTHETIC native track (shaped like the captured supercore bus nodes) named `name`, with
/// `plugin` in `effect_0`. No live helper has shown a populated track.
fn synthetic_track(root: &Json, name: &str, plugin: &str) -> Json {
    let devs = root["children"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["path"] == "devices")
        .unwrap();
    let buses = devs["children"][0]["children"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["path"] == "buses")
        .unwrap();
    let mut track = buses["children"][0].clone();
    track["path"] = Json::String("6b3fa9e8a6f84c06b1f1e9b6f0a1c2d3".into());
    track["properties"]["name"]["value"] = Json::String(name.into());
    let effects = track["children"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|c| c["path"] == "effects")
        .unwrap();
    let slot = effects["children"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|c| c["path"] == "effect_0")
        .unwrap();
    slot["properties"]["plugin_name"]["value"] = Json::String(plugin.into());
    slot["properties"]["plugin_power"]["value"] = Json::Bool(true);
    track
}

fn helper_with_track(name: &str, plugin: &str) -> Json {
    let mut root = helper_fixture();
    let track = synthetic_track(&root, name, plugin);
    let dev = &mut devices_mut(&mut root)["children"][0];
    let tracks = dev["children"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|c| c["path"] == "tracks")
        .unwrap();
    tracks
        .as_object_mut()
        .unwrap()
        .insert("children".into(), Json::Array(vec![track]));
    root
}

/// The engine fixture plus two catalogue entries (the capture is catalogue-trimmed).
fn engine_with_catalogue(online: bool) -> Store {
    let mut snap = engine_fixture();
    let entry = |name: &str, shorts: &[&str]| {
        let values: Vec<Json> = shorts
            .iter()
            .map(|s| serde_json::json!({ "value": s }))
            .collect();
        Property::from_json(&serde_json::json!({"type": "string", "value": name, "values": values}))
    };
    snap.tree.set_property(
        "/plugins/184/Name",
        entry("UAD SPL Vitalizer MK2-T", &["Vitalizer", "Vitalizer MK2-T"]),
    );
    snap.tree.set_property(
        "/plugins/207/Name",
        entry(
            "UAD Tube-Tech CL 1B mk II",
            &["CL 1B mkII", "TubeTech CL1B mkII"],
        ),
    );
    snap.tree
        .set_value("/devices/0/DeviceOnline", Value::Bool(online));
    Store::from_snapshot(snap)
}

/// Console labels a running UAD-2 plug-in with its catalogue short name, and an offline one
/// (Apollo offline) with its name without "UAD ", in italics.
#[test]
fn uad2_slot_labels_follow_console() {
    let online = EngineMixer::new(engine_with_catalogue(true));
    assert_eq!(
        online.short_name("UAD Tube-Tech CL 1B mk II").as_deref(),
        Some("CL 1B mkII")
    );
    let strip = online.inputs()[0].clone(); // "Vocal Mic": slot 1 holds the SPL Vitalizer MK2-T
    let slots = online.slots(&strip);
    assert_eq!(slots[1].source, SlotSource::Uad2);
    assert_eq!(
        (slots[1].label.as_str(), slots[1].italic),
        ("Vitalizer", false)
    );
    assert_eq!(
        slots[1].state,
        InsertState::Loaded {
            name: "UAD SPL Vitalizer MK2-T".into(),
            power: true
        }
    );
    assert_eq!(slots[0].source, SlotSource::Empty);
    let offline = EngineMixer::new(engine_with_catalogue(false));
    let slots = offline.slots(&strip);
    assert_eq!(
        (slots[1].label.as_str(), slots[1].italic),
        ("SPL Vitalizer MK2-T", true)
    );
}

/// A native plug-in on the helper track named like the strip fills the empty slot, flagged
/// native; the engine's UAD-2 insert keeps its slot.
#[test]
fn native_inserts_merge_with_uad2() {
    let mut m = EngineMixer::new(engine_with_catalogue(true));
    m.set_helper(HelperStore::from_helper_json(&helper_with_track(
        "Vocal Mic",
        "Tube-Tech CL 1B mkII",
    )));
    let strip = m.inputs()[0].clone();
    let slots = m.slots(&strip);
    assert!(slots[0].is_native());
    assert_eq!(slots[0].label, "Tube-Tech CL 1B mkII");
    assert_eq!(slots[0].native.as_ref().unwrap().slot, "effect_0");
    assert_eq!(slots[1].source, SlotSource::Uad2);
    let inserts = m.inserts(&strip);
    assert_eq!(
        inserts[0],
        InsertState::Loaded {
            name: "Tube-Tech CL 1B mkII".into(),
            power: true
        }
    );
    assert!(
        matches!(&inserts[1], InsertState::Loaded { name, .. } if name == "UAD SPL Vitalizer MK2-T")
    );
    // Other strips are untouched.
    assert!(m.slots(&m.inputs()[1]).iter().all(|s| !s.is_native()));
}

#[test]
fn status_and_settings_fall_back_to_the_helper() {
    let mut m = EngineMixer::new(Store::new()); // no engine tree at all
    assert_eq!(m.status().sample_rate, None);
    m.set_helper(HelperStore::from_helper_json(&helper_fixture()));
    assert_eq!(m.status().sample_rate, Some(48000));
    let settings = m.engine_settings();
    assert_eq!(settings.sample_rate, Some(48000));
    assert_eq!(settings.sample_rates.len(), 6);
    let info = m.helper_info().unwrap();
    assert_eq!(info.version.as_deref(), Some("1.3.1.1152"));
    assert_eq!(info.apollo().unwrap().num_inputs, 22);
    // The engine stays authoritative when it has a rate.
    let mut e = EngineMixer::new(engine_with_catalogue(true));
    let mut helper = helper_fixture();
    helper["properties"]["sample_rate"]["value"] = serde_json::json!(96000.0);
    e.set_helper(HelperStore::from_helper_json(&helper));
    assert_eq!(e.status().sample_rate, Some(48000));
}

/// Live: engine on a MockEngine, helper on a MockHelper (read-only); a native plug-in that
/// appears on the helper shows up in `inserts()` after the tracks poll.
#[test]
fn live_native_insert_appears() {
    let engine = MockEngine::start(engine_fixture().tree, MockOptions::default()).unwrap();
    let root = helper_fixture();
    let helper = MockHelper::start(root.clone(), 0).unwrap();
    let mut m = EngineMixer::connect(engine.endpoint(), false);
    assert!(m.helper().is_none()); // not on 4710, so no automatic helper
    m.connect_helper(helper.endpoint());
    m.helper_mut().unwrap().poll_interval = Duration::from_millis(100);
    let deadline = Instant::now() + Duration::from_secs(10);
    let live = |m: &EngineMixer| {
        *m.store().status() == Status::Live
            && m.helper().is_some_and(|h| *h.status() == Status::Live)
    };
    while !live(&m) {
        m.pump();
        assert!(Instant::now() < deadline, "not live");
        std::thread::sleep(Duration::from_millis(5));
    }
    let strip = m.inputs()[0].clone();
    assert!(m.slots(&strip).iter().all(|s| !s.is_native()));
    let devs = root["children"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["path"] == "devices")
        .unwrap();
    let dev = devs["children"][0]["path"].as_str().unwrap();
    helper.set_child(
        &format!("/devices/{dev}/tracks"),
        "6b3fa9e8a6f84c06b1f1e9b6f0a1c2d3",
        synthetic_track(&root, "Vocal Mic", "Tube-Tech CL 1B mkII"),
    );
    while !m.slots(&strip)[0].is_native() {
        m.pump();
        assert!(Instant::now() < deadline, "native insert never appeared");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        m.inserts(&strip)[0],
        InsertState::Loaded {
            name: "Tube-Tech CL 1B mkII".into(),
            power: true
        }
    );
    // Nothing but identity, get, subscribe and pings went to the helper.
    let cmds = helper.received_commands();
    assert!(
        cmds.iter()
            .all(|c| c.starts_with('{') || c.starts_with("get ") || c.starts_with("subscribe ")),
        "{cmds:?}"
    );
    assert!(engine.write_commands().is_empty());
}
