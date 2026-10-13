//! Session commands and lists over loopback TCP against MockEngine.

mod common;

use std::time::{Duration, Instant};

use common::live_x4;
use ua_engine::{MockEngine, MockOptions, Options, SessionCommand, SessionEntry, Status, Store};

const T: Duration = Duration::from_secs(5);

fn drain_until(store: &mut Store, what: &str, mut ok: impl FnMut(&Store) -> bool) {
    let end = Instant::now() + T;
    while Instant::now() < end {
        store.drain();
        if ok(store) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("timed out waiting for {what}");
}

fn live(server: &MockEngine, writes: bool) -> Store {
    let mut store = Store::new();
    store.connect(server.endpoint(), Options { allow_writes: writes, ..Options::default() });
    store.set_writes_enabled(writes);
    drain_until(&mut store, "live", |s| *s.status() == Status::Live);
    store
}

fn files(store: &Store) -> Vec<String> {
    store.session_state().entries.iter().flat_map(SessionEntry::files).map(|f| f.path().to_owned()).collect()
}

#[test]
fn reads_the_open_session_and_lists() {
    let server = MockEngine::start(live_x4(), MockOptions::default()).unwrap();
    let store = live(&server, false);
    let st = store.session_state();
    assert_eq!(st.current, "Session");
    assert_eq!(st.name(), "Session");
    assert!(st.dirty);
    assert_eq!(files(&store), ["Session"]);
}

#[test]
fn read_only_sessions_send_nothing() {
    let server = MockEngine::start(live_x4(), MockOptions::default()).unwrap();
    let mut store = live(&server, false);
    for c in [SessionCommand::Save, SessionCommand::save_as("X"), SessionCommand::New, SessionCommand::Load("Session".into())] {
        store.session(&c);
    }
    std::thread::sleep(Duration::from_millis(100));
    store.drain();
    assert!(server.write_commands().is_empty(), "{:?}", server.write_commands());
    assert_eq!(store.writer().suppressed_count(), 6, "{:?}", store.writer().recent_suppressed().collect::<Vec<_>>());
}

#[test]
fn save_as_then_load_round_trip() {
    let server = MockEngine::start(live_x4(), MockOptions::default()).unwrap();
    let mut store = live(&server, true);
    store.session(&SessionCommand::save_as("Tracking"));
    // The engine pushes /Session and /Dirty; the store re-reads /Session/values.
    drain_until(&mut store, "saved list", |s| files(s).contains(&"Tracking".to_owned()) && !s.session_state().dirty);
    assert_eq!(store.session_state().current, "Tracking");
    store.session(&SessionCommand::Load("Session".into()));
    drain_until(&mut store, "loaded", |s| {
        s.session_state().current == "Session" && server.write_commands().iter().any(|c| c.starts_with("set /Session/value"))
    });
    let w = server.write_commands();
    assert!(w.contains(&r#"post /SaveSessionAs {"name":"Tracking.uadmix"}"#.to_owned()), "{w:?}");
    assert!(w.iter().any(|c| c == r#"set /Session/value "Session""#), "{w:?}");
    // Loads are a gesture of their own, like every discrete edit.
    assert!(w.iter().any(|c| c == "set /UndoRecording/value true"), "{w:?}");
}

#[test]
fn engine_side_list_changes_are_picked_up() {
    let server = MockEngine::start(live_x4(), MockOptions::default()).unwrap();
    let mut store = live(&server, false);
    server.set_values("/Session", serde_json::json!([
        {"value": "Session", "type": "file", "locked": false},
        {"value": "Live", "type": "folder", "locked": false, "children": [{"value": "Band", "type": "file", "locked": false}]}
    ]));
    server.set_values("/ModifiedSessions", serde_json::json!([{"value": "newSession", "time": "Oct 12, 2026 09:00:00 AM"}]));
    // Another client saved: /Dirty goes false and the store re-reads both lists.
    server.push("/Dirty", false);
    drain_until(&mut store, "lists", |s| files(s) == ["Session", "Live/Band"] && s.session_state().modified.len() == 1);
    assert_eq!(store.session_state().modified[0].label(), "[Oct 12, 2026 09:00:00 AM] - - New Session -");
    let reads = server.received_commands();
    assert!(reads.iter().any(|c| c.starts_with("get /Session/values")), "{reads:?}");
}

/// Session folders take `{"folder","type":"session"}` (UA Mixer Engine 12 `NewFolder` /
/// `DeleteFolder` handlers), and an empty Save As name or Open path never leaves: the engine
/// would answer with a native file dialog on its host.
#[test]
fn folder_shapes_and_empty_name_guard() {
    let cmd = |c: SessionCommand| c.command().text();
    assert_eq!(cmd(SessionCommand::NewFolder { folder: "Live".into() }), r#"post /NewFolder {"folder":"Live","type":"session"}"#);
    assert_eq!(cmd(SessionCommand::DeleteFolder { folder: "Live".into() }), r#"post /DeleteFolder {"folder":"Live","type":"session"}"#);
    assert_eq!(cmd(SessionCommand::Delete { name: "Band".into() }), r#"post /DeleteSession {"name":"Band"}"#);
    assert_eq!(cmd(SessionCommand::Rename { from: "Band".into(), to: "Band 2".into() }), r#"post /RenameSession {"newName":"Band 2","origName":"Band"}"#);
    for bad in [SessionCommand::save_as(""), SessionCommand::save_as("  .uadmix"), SessionCommand::Open { path: " ".into() }, SessionCommand::NewFolder { folder: String::new() }] {
        assert!(!bad.is_sendable(), "{bad:?}");
    }
    let server = MockEngine::start(live_x4(), MockOptions::default()).unwrap();
    let mut store = live(&server, true);
    store.session(&SessionCommand::save_as(""));
    store.session(&SessionCommand::Open { path: String::new() });
    store.session(&SessionCommand::Save);
    let end = Instant::now() + Duration::from_secs(10);
    while server.write_commands().is_empty() {
        assert!(Instant::now() < end, "no writes");
        std::thread::sleep(Duration::from_millis(5));
    }
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(server.write_commands(), ["post /SaveSession {}"]);
}
