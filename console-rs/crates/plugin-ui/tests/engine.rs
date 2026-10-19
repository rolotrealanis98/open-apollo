//! The engine-backed ParamSource against the recorded Apollo x4 snapshot (offline, no engine).

use std::path::PathBuf;

use plugin_ui::{EngineSlot, ParamSnapshot, ParamSource, PluginPanel, PluginSpecLibrary};
use ua_engine::{InsertPaths, Snapshot, Store};

fn store() -> Store {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../ua-engine/tests/fixtures/apollo-x4-12.0.0.snapshot.json");
    Store::from_snapshot(Snapshot::load(path).expect("fixture"))
}

#[test]
fn reads_and_gated_writes() {
    let mut store = store();
    let mut slot = EngineSlot::open(&mut store, InsertPaths::from_slot("/devices/0/inputs/0/effects/1"));
    assert_eq!(slot.effect_name(&store).as_deref(), Some("UAD SPL Vitalizer MK2-T"));
    let view = slot.view(&store);
    assert_eq!(view.count(), 11);
    assert_eq!(view.name(2).as_deref(), Some("Drive"));
    assert_eq!(view.steps(0), vec!["A", "B", "C", "D"]);
    let drive = view.normalized(2).expect("drive value");
    assert!((0.0..=1.0).contains(&drive));
    assert!(view.display(2).is_some());
    assert!(view.normalized(40).is_none(), "unpublished ordinal");
    let snap = ParamSnapshot::capture(&view);
    assert_eq!(snap.count(), 11);
    // NormalizedValue + StringValue for every parameter, StepValue for the two stepped ones.
    assert_eq!(slot.retained().len(), 11 * 2 + 2);

    // Writes are gated (read-only by default): the local value moves, nothing is sent.
    assert!(!store.writer().writes_enabled);
    {
        let mut w = slot.view_mut(&mut store);
        w.begin_gesture();
        w.set(2, 0.25);
        w.end_gesture();
    }
    assert_eq!(slot.view(&store).normalized(2), Some(0.25));
    assert_eq!(store.writer().sent_count(), 0);
    slot.close(&mut store);
    assert!(slot.retained().is_empty());
}

/// Spec index = engine index: the Vitalizer spec's parameter names match the snapshot's.
#[test]
fn spec_matches_engine_parameters() {
    let Some(lib) = PluginSpecLibrary::locate() else { return };
    let Some(spec) = lib.spec_for_effect("UAD SPL Vitalizer MK2-T") else { return };
    let mut store = store();
    let slot = EngineSlot::open(&mut store, InsertPaths::from_slot("/devices/0/inputs/0/effects/1"));
    let view = slot.view(&store);
    for p in &spec.parameters {
        assert_eq!(view.name(p.index).as_deref(), Some(p.name.as_str()), "parameter {}", p.index);
    }
    // A panel over the live view reads engine values.
    let panel = PluginPanel::new(spec.clone(), &lib, slot.slot.base.as_str());
    assert_eq!(panel.value(2, &view), view.normalized(2).unwrap());
}

/// Live meters: the panel's VendorSpecific query goes to the engine as a read-only `get` and the
/// reply moves the meter (MockEngine over loopback; no hardware).
#[test]
fn meters_poll_the_engine_read_only() {
    use std::time::{Duration, Instant};
    use ua_engine::{MockEngine, MockOptions, Options, Status};

    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../ua-engine/tests/fixtures/apollo-x4-12.0.0.snapshot.json");
    let server = MockEngine::start(Snapshot::load(path).expect("fixture").tree, MockOptions::default()).expect("mock engine");
    let mut store = Store::new();
    store.connect(server.endpoint(), Options::default());
    let deadline = Instant::now() + Duration::from_secs(10);
    while *store.status() != Status::Live {
        assert!(Instant::now() < deadline, "mock engine never went live");
        store.drain();
        std::thread::sleep(Duration::from_millis(5));
    }
    let slot = "/devices/0/inputs/0/effects/1";
    let spec = std::sync::Arc::new(
        plugin_ui::PluginSpec::from_json(
            r#"{"schema": 1, "effectName": "UAD SPL Vitalizer MK2-T", "namePattern": "Vitalizer", "size": {"w": 10, "h": 10},
                "parameters": [], "elements": [],
                "meterSources": {"vu": {"vendorSpecificFloat": 7, "smoothing": 0, "calibration": [[0, 0], [2, 1]]}}}"#,
        )
        .unwrap(),
    );
    let mut panel = PluginPanel::with_sprites(spec, slot, |_| None);
    let params = plugin_ui::MockParams::default();
    server.push(&format!("{slot}/VendorSpecific"), "3f800000");
    let mut poller = plugin_ui::MeterPoller::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    while panel.meters.raw("vu") != Some(1.0) {
        assert!(Instant::now() < deadline, "no meter reply");
        store.drain();
        for (q, data) in poller.poll(&store, &panel.meter_queries()) {
            assert!(panel.ingest_reply(&q, &data, &params));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panel.meters.tick(&panel.spec.clone(), 0.016);
    assert_eq!(panel.meters.needle("vu"), 0.5);
    let gets: Vec<String> = server.received_commands().into_iter().filter(|c| c.contains("VendorSpecific")).collect();
    assert!(gets.iter().all(|c| c.starts_with(&format!("get {slot}/VendorSpecific/value?floats=7"))), "{gets:?}");
    assert!(server.write_commands().is_empty(), "meter polling never writes");
}

/// A slot whose parameters the engine publishes after the insert (no push for them): the slot
/// re-reads itself while empty and lists them once they are there (MockEngine, no hardware).
#[test]
fn late_parameters_are_reread() {
    use std::time::{Duration, Instant};
    use ua_engine::{MockEngine, MockOptions, Options, Status};

    let slot = "/devices/0/inputs/0/effects/1";
    let params = format!("{slot}/parameters");
    let mut tree = store().tree().clone();
    let published = tree.node_json(&params);
    tree.remove_subtree(&params);
    let server = MockEngine::start(tree, MockOptions::default()).expect("mock engine");
    let mut store = Store::new();
    store.connect(server.endpoint(), Options::default());
    let deadline = Instant::now() + Duration::from_secs(10);
    while *store.status() != Status::Live {
        assert!(Instant::now() < deadline, "mock engine never went live");
        store.drain();
        std::thread::sleep(Duration::from_millis(5));
    }
    let mut engine = EngineSlot::open(&mut store, InsertPaths::from_slot(slot));
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        store.drain();
        engine.sync(&mut store);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(engine.view(&store).count(), 0);
    server.set_node(&params, &published);
    let deadline = Instant::now() + Duration::from_secs(5);
    while engine.view(&store).count() == 0 {
        assert!(Instant::now() < deadline, "late parameters never listed");
        store.drain();
        engine.sync(&mut store);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(engine.view(&store).count(), 11);
    assert!(server.write_commands().is_empty(), "re-reads never write");
    engine.close(&mut store);
}
