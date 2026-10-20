mod common;

use std::sync::Arc;
use std::time::Duration;

use common::x4_store;
use ua_engine::{Changes, MeterReading, MeterStore, Snapshot, Status, Store, Value};

fn push(path: &str, v: impl Into<Value>) -> (String, Value) {
    (path.to_owned(), v.into())
}

#[test]
fn store_loads_snapshot_offline() {
    let store = x4_store();
    assert_eq!(store.status(), &Status::Offline("live 127.0.0.1:4710 via lab-engine dump".into()));
    assert_eq!(store.device_name(), Some("Apollo x4"));
    assert_eq!(store.engine_version(), Some("12.0.0"));
    assert_eq!(store.topology().inputs.len(), 6);
    let fader = store.topology().inputs[0].fader();
    assert!(store.value(&fader).is_some());
    let meta = store.meta(&format!("{fader}/value")).unwrap();
    assert_eq!(meta.max, Some(1.0));
    assert_eq!(meta.default, Some(Value::Number(0.7818182110786438)));
    assert_eq!(store.tree_revision(), 1);
}

/// Change tracking: a revision per change, paths listed once, equal pushes ignored.
#[test]
fn change_tracking() {
    let mut store = x4_store();
    let strips = store.topology().inputs.clone();
    let rev = store.revision();
    assert!(store.changed_since(rev).is_empty());
    let cur = store.value(&strips[0].solo()).cloned().unwrap();
    store.apply([push(&strips[0].solo(), cur.clone())]); // equal: no change
    assert_eq!(store.revision(), rev);
    store.apply([
        push(&format!("{}/value", strips[1].solo()), true),
        push(&strips[2].fader(), 0.1),
        push("/TalkbackOn", true),
        push(&strips[2].fader(), 0.2),
    ]);
    assert_eq!(store.revision(), rev + 4);
    let Changes::Paths(p) = store.changed_since(rev) else { panic!("expected paths") };
    assert_eq!(p, [strips[2].fader().as_str(), "/TalkbackOn", strips[1].solo().as_str()]); // newest first, no repeats
    assert!(store.changed_since(rev + 3).contains(&strips[2].fader()));
    assert!(!store.changed_since(rev + 3).contains("/TalkbackOn"));
    assert_eq!(store.path_revision(&strips[2].fader()), rev + 4);
    assert_eq!(store.path_revision(&strips[0].solo()), 0);
    assert_eq!(store.value(&strips[1].solo()), Some(&Value::Bool(true)));
    // Before the tree install: everything changed.
    assert_eq!(store.changed_since(0), Changes::All);
}

/// The topology revision moves only when the strip layout changes.
#[test]
fn topology_changes_only_on_structure() {
    let mut store = x4_store();
    let r = store.topology_revision();
    store.apply([push("/devices/0/inputs/0/FaderLevelTapered", 0.2), push("/devices/0/inputs/0/Mute", false)]);
    assert_eq!(store.topology_revision(), r);
    store.apply([push("/devices/0/inputs/0/Stereo", true), push("/devices/0/inputs/1/Stereo", true)]);
    assert_eq!(store.topology_revision(), r + 1);
    assert_eq!(store.topology().inputs[0].right_index, Some(1));
    // A structural property set to the same layout does not bump it.
    store.apply([push("/devices/0/inputs/0/Name", "Lead")]);
    assert_eq!(store.topology_revision(), r + 1); // pair shows StereoName, so the layout is unchanged
    store.apply([push("/devices/0/inputs/0/StereoName", "Lead pair")]);
    assert_eq!(store.topology_revision(), r + 2);
    assert_eq!(store.topology().inputs[0].name, "Lead pair");
}

/// Meter pushes go to MeterStore and never reach the tree.
#[test]
fn meter_routing() {
    let meters = MeterStore::new();
    for (p, v) in [
        ("/devices/0/inputs/0/meters/0/MeterLevel/value", Value::Number(-12.5)),
        ("/devices/0/inputs/0/meters/0/MeterPeakLevel/value", Value::Number(-3.0)),
        ("/devices/0/inputs/0/meters/0/MeterClip/value", Value::Bool(true)),
        ("/devices/0/inputs/0/CustomIntegratedPreampInputMeter/value", Value::Number(-40.0)),
        ("/MeterPulse/value", Value::Number(1381672.0)),
    ] {
        assert!(meters.ingest(p, &v), "{p}");
    }
    assert!(!meters.ingest("/devices/0/inputs/0/Mute/value", &Value::Bool(true)));
    let r = meters.reading("/devices/0/inputs/0/meters/0");
    assert_eq!((r.level_db, r.peak_db, r.clip, r.updates), (-12.5, -3.0, true, 3));
    assert_eq!(meters.reading("/devices/0/inputs/0/CustomIntegratedPreampInputMeter").level_db, -40.0);
    assert_eq!(meters.pulse(), 1381672);
    assert_eq!(meters.reading("/devices/0/inputs/5/meters/0"), MeterReading::SILENT);
    assert!(MeterStore::is_meter_path("/devices/0/inputs/0/sends/3/meters/1/MeterClip/value"));
    assert!(!MeterStore::is_meter_path("/MeterLevel/value")); // Console's root path bug: not a meter node
    assert!(!MeterStore::is_meter_path("/devices/0/inputs/0/FaderLevel/value"));
    let stats = meters.stats();
    assert_eq!((stats.pushes, stats.pulses, stats.active_meters), (4, 1, 2));
    // A handle reads the same slot lock-free and sees later pushes.
    let h = meters.handle("/devices/0/inputs/0/meters/0");
    meters.ingest("/devices/0/inputs/0/meters/0/MeterLevel/value", &Value::Number(-6.0));
    assert_eq!(h.reading().level_db, -6.0);
}

#[test]
fn meter_store_is_thread_safe() {
    let meters = Arc::new(MeterStore::new());
    let threads: Vec<_> = (0..8)
        .map(|t| {
            let m = meters.clone();
            std::thread::spawn(move || {
                for i in 0..2000 {
                    m.ingest(
                        &format!("/devices/0/inputs/{t}/meters/0/MeterLevel/value"),
                        &Value::Number(-((i % 77) as f64)),
                    );
                    let _ = m.reading(&format!("/devices/0/inputs/{}/meters/0", (t + 1) % 8));
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    let s = meters.stats();
    assert_eq!(s.pushes, 16000);
    assert_eq!(s.active_meters, 8);
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<MeterStore>();
}

#[test]
fn meters_are_seeded_from_the_tree() {
    let store = x4_store();
    // The unit was offline at capture: meters read −77 in the tree.
    let base = &store.topology().inputs[0].meters[0].base;
    assert_eq!(store.meters().reading(base).level_db, -77.0);
    assert_eq!(store.meters().reading(base).updates, 0);
}

#[test]
fn snapshot_save_and_reload() {
    let mut store = x4_store();
    let strip = store.topology().inputs[0].clone();
    store.apply([push(&strip.fader(), 0.5), push(&strip.name_path(), "Vox")]);
    let path = std::env::temp_dir().join(format!("ua-engine-{}.json", std::process::id()));
    store.save_snapshot(&path, false, false).unwrap();
    let mut other = Store::new();
    other.load_snapshot(&path).unwrap();
    std::fs::remove_file(&path).ok();
    assert_eq!(other.value(&strip.fader()), Some(&Value::Number(0.5)));
    assert_eq!(other.topology().inputs[0].name, "Vox");
    assert_eq!(other.device_name(), Some("Apollo x4"));
    assert!(matches!(other.status(), Status::Offline(_)));
    // Trimmed snapshots drop preset lists but keep values.
    let t = store.snapshot(true);
    assert_eq!(t.tree.value(&strip.fader()), Some(&Value::Number(0.5)));
}

/// Values for paths created before a tree exists fill in on load.
#[test]
fn reads_before_load_fill_in() {
    let mut store = Store::new();
    assert_eq!(store.status(), &Status::Empty);
    assert!(store.value("/devices/0/inputs/0/Mute").is_none());
    store.load(Snapshot::load(common::manifest("tests/fixtures/apollo-x4-12.0.0.snapshot.json")).unwrap());
    assert!(store.value("/devices/0/inputs/0/Mute").is_some());
    assert_eq!(store.meta("/devices/0/inputs/0/Mute").and_then(|m| m.ty.as_deref()), Some("bool"));
}

#[test]
fn simulated_meters_move() {
    let mut store = x4_store();
    store.simulate_meters(true);
    let key = store.topology().inputs[0].meters[0].base.clone();
    common::wait_until("simulated meters", Duration::from_secs(3), || store.meters().reading(&key).updates > 2);
    store.simulate_meters(false);
}

/// A drag's echo hold: a stale push is held, the engine's echo of our own write drops it (a
/// release never pulls the control back to a value older than the write), and a push that comes
/// after that echo is the engine's newer state and wins at release.
#[test]
fn echo_hold_drops_pushes_older_than_our_write() {
    let mut store = x4_store();
    store.set_writes_enabled(true);
    let fader = store.topology().inputs[0].fader();
    store.set_echo_hold(Duration::from_millis(20));
    store.set(&fader, 0.6);
    store.apply([push(&fader, 0.3)]); // stale, from before our write
    assert_eq!(store.value(&fader), Some(&Value::Number(0.6)));
    store.apply([push(&fader, 0.6)]); // the engine's echo of our write
    std::thread::sleep(Duration::from_millis(30));
    store.drain(); // releases the hold
    assert_eq!(store.value(&fader), Some(&Value::Number(0.6)), "the stale 0.3 must not come back");

    store.set(&fader, 0.5);
    store.apply([push(&fader, 0.5)]); // echo
    store.apply([push(&fader, 0.2)]); // someone else moved it after our write
    assert_eq!(store.value(&fader), Some(&Value::Number(0.5)), "held while dragging");
    std::thread::sleep(Duration::from_millis(30));
    store.drain();
    assert_eq!(store.value(&fader), Some(&Value::Number(0.2)), "the newer engine value wins at release");
}

/// A native plug-in parameter answers slowly and quantized: a drag's stale echoes keep the
/// user's position past the echo hold, and the engine's (quantized) echo of the last write ends
/// the hold at once.
#[test]
fn plugin_parameter_holds_until_its_quantized_echo() {
    let mut store = x4_store();
    store.set_writes_enabled(true);
    let knob = "/devices/0/inputs/1/effects/0/parameters/14/NormalizedValue";
    store.set_echo_hold(Duration::from_millis(20));
    store.set(knob, 0.70423);
    store.apply([push(knob, 0.31)]); // the engine still applying an earlier value of the drag
    std::thread::sleep(Duration::from_millis(40));
    store.drain(); // past the echo hold: still the user's position
    assert_eq!(store.value(knob), Some(&Value::Number(0.70423)), "a stale echo pulled the knob back");
    store.apply([push(knob, 0.45)]);
    assert_eq!(store.value(knob), Some(&Value::Number(0.70423)));
    store.apply([push(knob, 0.7042)]); // our write, quantized
    assert_eq!(store.value(knob), Some(&Value::Number(0.7042)));
    store.apply([push(knob, 0.2)]); // after the echo: someone else's change shows at once
    assert_eq!(store.value(knob), Some(&Value::Number(0.2)));
}
