//! The Snapshots section over the engine: each button's exact wire form, the lists from
//! `/Snapshot/values`, and a refused prime snapping back.

use std::time::{Duration, Instant};

use engine_model::EngineMixer;
use rack::model::MixerModel;
use rack::surfaces::snapshots::{SnapshotOp, SnapshotRequest, request_id};
use rack::{Action, Control, ControlId, ViewState};
use ua_engine::{MockEngine, MockOptions, Snapshot, Status};

fn tree() -> ua_engine::Tree {
    let p = format!("{}/../ua-engine/tests/fixtures/apollo-x4-12.0.0.snapshot.json", env!("CARGO_MANIFEST_DIR"));
    let mut t = Snapshot::load(p).unwrap().tree;
    let mut snap = t.property("/Snapshot").cloned().unwrap();
    snap.meta.values = Some(serde_json::json!([
        {"value": "000 - Live", "type": "folder", "locked": false, "children": [
            {"value": "001 - A", "type": "file", "locked": false}
        ]},
        {"value": "001 - Show", "type": "folder", "locked": false, "children": [
            {"value": "001 - Intro", "type": "file", "locked": false}
        ]}
    ]));
    snap.extra.insert(
        "volumes".into(),
        serde_json::json!([{"volume": "Macintosh HD", "user": true, "removable": false, "locked": false}]),
    );
    t.set_property("/Snapshot", snap);
    t
}

fn pump_until(m: &mut EngineMixer, what: &str, ok: impl Fn(&EngineMixer) -> bool) {
    let end = Instant::now() + Duration::from_secs(10);
    while !ok(m) {
        assert!(Instant::now() < end, "timed out: {what}");
        m.pump();
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn tap(v: &mut ViewState, m: &mut EngineMixer, op: SnapshotOp) {
    v.dispatch(Action::Tap(ControlId { strip: "snapshots".into(), control: Control::Snapshots(op) }), m);
}

fn type_text(v: &mut ViewState, m: &mut EngineMixer, text: &str) {
    v.text_edit.as_mut().expect("a name field").text = text.into();
    v.key_enter(m);
}

#[test]
fn every_button_sends_consoles_wire_form() {
    let server = MockEngine::start(tree(), MockOptions::default()).unwrap();
    let mut m = EngineMixer::connect(server.endpoint(), true);
    pump_until(&mut m, "live", |m| *m.store().status() == Status::Live);
    let s = m.snapshots();
    assert_eq!(s.folders.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), ["000 - Live", "001 - Show"]);
    assert_eq!(s.folders[0].snapshots, ["001 - A"]);
    assert_eq!(s.volumes, ["Macintosh HD"]);

    let mut v = ViewState { show_snapshots: true, ..Default::default() };
    tap(&mut v, &mut m, SnapshotOp::Snapshot(0)); // prime
    tap(&mut v, &mut m, SnapshotOp::Recall);
    pump_until(&mut m, "recalled", |m| m.snapshots().current == "000 - Live/001 - A");
    tap(&mut v, &mut m, SnapshotOp::Save);
    tap(&mut v, &mut m, SnapshotOp::SaveAs);
    type_text(&mut v, &mut m, "002 - B");
    tap(&mut v, &mut m, SnapshotOp::New);
    tap(&mut v, &mut m, SnapshotOp::Snapshot(0));
    tap(&mut v, &mut m, SnapshotOp::Rename);
    type_text(&mut v, &mut m, "001 - Z");
    tap(&mut v, &mut m, SnapshotOp::Folder(1));
    tap(&mut v, &mut m, SnapshotOp::Move);
    type_text(&mut v, &mut m, "002 - Show");
    tap(&mut v, &mut m, SnapshotOp::Delete);
    assert!(v.popover.is_some(), "DELETE asks first");
    v.dispatch(Action::Tap(request_id(SnapshotRequest::Delete("001 - Show".into()))), &mut m);
    tap(&mut v, &mut m, SnapshotOp::ImportAll);
    type_text(&mut v, &mut m, "Backup");
    tap(&mut v, &mut m, SnapshotOp::ExportAll);
    type_text(&mut v, &mut m, "Backup");

    let want = [
        r#"set /PrimedSnapshot/value "000 - Live/001 - A""#,
        r#"set /Snapshot/value "000 - Live/001 - A""#,
        r#"post /SaveAsSnapshot {"snapshotName":"000 - Live/001 - A"}"#,
        r#"post /SaveAsSnapshot {"snapshotName":"000 - Live/002 - B"}"#,
        r#"post /SaveAsSnapshot {"isNew":true,"snapshotName":"000 - Live/002 - Untitled"}"#,
        r#"set /Snapshot/value "000 - Live/002 - Untitled""#,
        r#"post /MoveSnapshot {"newName":"000 - Live/001 - Z","origName":"000 - Live/001 - A","shuffle":false}"#,
        r#"set /PrimedSnapshot/value "000 - Live/001 - Z""#,
        r#"set /PrimedSnapshot/value """#,
        r#"post /MoveSnapshot {"newName":"002 - Show","origName":"001 - Show","shuffle":false}"#,
        r#"post /DeleteSnapshot {"snapshotName":"001 - Show"}"#,
        r#"post /ImportSnapshots {"append":true,"folder":"Backup","volume":"Macintosh HD"}"#,
        r#"post /ExportSnapshots {"folder":"Backup","volume":"Macintosh HD"}"#,
    ];
    let end = Instant::now() + Duration::from_secs(5);
    let got = loop {
        m.pump();
        let w = server.write_commands();
        if want.iter().all(|c| w.iter().any(|x| x == c)) || Instant::now() > end {
            break w;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let pos: Vec<usize> =
        want.iter().map(|c| got.iter().position(|x| x == c).unwrap_or_else(|| panic!("{c} not in {got:#?}"))).collect();
    assert!(pos.windows(2).all(|p| p[0] < p[1]), "out of order: {got:#?}");
}

#[test]
fn a_refused_prime_snaps_back() {
    let server = MockEngine::start(tree(), MockOptions::default()).unwrap();
    server.refuse("/PrimedSnapshot", "001 - Show/001 - Intro");
    let mut m = EngineMixer::connect(server.endpoint(), true);
    pump_until(&mut m, "live", |m| *m.store().status() == Status::Live);
    let mut v = ViewState { show_snapshots: true, ..Default::default() };
    tap(&mut v, &mut m, SnapshotOp::Snapshot(0));
    pump_until(&mut m, "primed", |m| m.snapshots().primed == "000 - Live/001 - A");
    tap(&mut v, &mut m, SnapshotOp::Folder(1));
    tap(&mut v, &mut m, SnapshotOp::Snapshot(0));
    assert_eq!(m.snapshots().primed, "001 - Show/001 - Intro", "shown at once");
    pump_until(&mut m, "snapped back", |m| m.snapshots().primed.is_empty());
}

#[test]
fn the_preference_offers_view_show_snapshots() {
    let server = MockEngine::start(tree(), MockOptions::default()).unwrap();
    let mut m = EngineMixer::connect(server.endpoint(), false);
    pump_until(&mut m, "live", |m| *m.store().status() == Status::Live);
    let has = |m: &EngineMixer| rack::menu::menu_bar_json(m, &ViewState::default(), true).contains("show_snapshots");
    assert!(!has(&m), "hidden by default (Console 1.3.1 hides it)");
    let dir = std::env::temp_dir().join(format!("snap-prefs-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("console-prefs.json");
    std::fs::write(&file, r#"{"show_snapshots": true}"#).unwrap();
    m.set_prefs_path(Some(file));
    assert!(has(&m));
    let _ = std::fs::remove_dir_all(dir);
}
