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
