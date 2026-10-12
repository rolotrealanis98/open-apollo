//! The SESSION menu over the engine: lists, names and the commands the menu sends.

use std::time::{Duration, Instant};

use engine_model::EngineMixer;
use rack::model::MixerModel;
use rack::surfaces::{SaveChoice, SessionOp};
use rack::{Action, Control, ControlId, ViewState};
use ua_engine::{MockEngine, MockOptions, Snapshot, Status};

fn tree() -> ua_engine::Tree {
    let p = format!("{}/../ua-engine/tests/fixtures/apollo-x4-12.0.0.snapshot.json", env!("CARGO_MANIFEST_DIR"));
    Snapshot::load(p).unwrap().tree
}

fn live(server: &MockEngine, writes: bool) -> EngineMixer {
    let mut m = EngineMixer::connect(server.endpoint(), writes);
    pump_until(&mut m, "live", |m| *m.store().status() == Status::Live);
    m
}

fn pump_until(m: &mut EngineMixer, what: &str, ok: impl Fn(&EngineMixer) -> bool) {
    let end = Instant::now() + Duration::from_secs(10);
    while !ok(m) {
        assert!(Instant::now() < end, "timed out: {what}");
        m.pump();
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn sid(control: Control) -> ControlId {
    ControlId { strip: "session".into(), control }
}

#[test]
fn offline_snapshot_shows_the_session() {
    let m = EngineMixer::new(ua_engine::Store::from_snapshot(Snapshot::load(format!(
        "{}/../ua-engine/tests/fixtures/apollo-x4-12.0.0.snapshot.json",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()));
    let s = m.session();
    assert_eq!((s.name.as_str(), s.dirty, s.has_file), ("Session", true, true));
    assert_eq!(s.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), ["Session"]);
    assert_eq!(m.session_name(), "Session*");
}

#[test]
fn menu_requests_reach_the_engine() {
    let server = MockEngine::start(tree(), MockOptions::default()).unwrap();
    server.set_values("/Session", serde_json::json!([
        {"value": "Session", "type": "file", "locked": false},
        {"value": "Live", "type": "folder", "locked": false, "children": [{"value": "Band", "type": "file", "locked": false}]}
    ]));
    let mut m = live(&server, true);
    let mut v = ViewState::default();
    // The fixture's session is dirty: loading asks first; Save, then load Live/Band.
    v.dispatch(Action::Tap(sid(Control::SessionMenu)), &mut m);
    v.dispatch(Action::Tap(sid(Control::Session(SessionOp::Load(1)))), &mut m);
    v.dispatch(Action::Tap(sid(Control::SessionSaveChoice(SaveChoice::Save))), &mut m);
    pump_until(&mut m, "loaded", |m| m.session().current == "Live/Band" && !m.session().dirty);
    // Save As from the name field; the engine adds it to the list (mock) and the store re-reads it.
    v.dispatch(Action::Tap(sid(Control::Session(SessionOp::SaveAs))), &mut m);
    assert_eq!(v.text_edit.as_ref().map(|t| t.text.as_str()), Some("Band"));
    for _ in 0..4 {
        v.key_backspace();
    }
    v.key_text("Band 2");
    v.key_enter(&mut m);
    pump_until(&mut m, "saved as", |m| m.session().files.iter().any(|f| f.path == "Band 2") && m.session().current == "Band 2");
    let w = server.write_commands();
    let pos = |c: &str| w.iter().position(|x| x == c).unwrap_or_else(|| panic!("{c} not in {w:?}"));
    assert!(pos("post /SaveSession {}") < pos(r#"set /Session/value "Live/Band""#));
    pos(r#"post /SaveSessionAs {"name":"Band 2.uadmix"}"#);
}

#[test]
fn read_only_menu_sends_nothing() {
    let server = MockEngine::start(tree(), MockOptions::default()).unwrap();
    let mut m = live(&server, false);
    let mut v = ViewState::default();
    v.dispatch(Action::Tap(sid(Control::Session(SessionOp::New))), &mut m);
    v.dispatch(Action::Tap(sid(Control::SessionSaveChoice(SaveChoice::DontSave))), &mut m);
    v.dispatch(Action::Tap(sid(Control::Session(SessionOp::Save))), &mut m);
    std::thread::sleep(Duration::from_millis(100));
    m.pump();
    assert!(server.write_commands().is_empty(), "{:?}", server.write_commands());
    let suppressed: Vec<String> = m.store().writer().recent_suppressed().map(str::to_owned).collect();
    assert_eq!(suppressed, ["post /NewSession {}", "post /SaveSession {}"]);
}
