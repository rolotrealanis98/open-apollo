#![allow(dead_code)]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ua_engine::{Command, CommandSink, Snapshot, Store, Tree};

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

/// A store showing the live x4 capture offline.
pub fn x4_store() -> Store {
    Store::from_snapshot(
        Snapshot::load(manifest("tests/fixtures/apollo-x4-12.0.0.snapshot.json")).unwrap(),
    )
}

/// Records what the writer sends.
#[derive(Clone, Default)]
pub struct RecordingSink {
    pub sent: Arc<Mutex<Vec<String>>>,
    pub accept: Arc<Mutex<bool>>,
}

impl RecordingSink {
    pub fn new() -> RecordingSink {
        RecordingSink {
            sent: Default::default(),
            accept: Arc::new(Mutex::new(true)),
        }
    }
    pub fn texts(&self) -> Vec<String> {
        self.sent.lock().unwrap().clone()
    }
}

impl CommandSink for RecordingSink {
    fn send_command(&self, command: Command) -> bool {
        if !*self.accept.lock().unwrap() {
            return false;
        }
        self.sent.lock().unwrap().push(command.text());
        true
    }
}

/// Polls `f` (draining nothing) until it is true or `timeout` passes.
pub fn wait_until(what: &str, timeout: Duration, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !f() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Drains the store until `f` holds.
pub fn drain_until(
    store: &mut Store,
    what: &str,
    timeout: Duration,
    mut f: impl FnMut(&Store) -> bool,
) {
    let deadline = Instant::now() + timeout;
    loop {
        store.drain();
        if f(store) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what} (status {:?})",
            store.status()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
