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

/// Against a private open-apollo daemon (`ua_mixer_daemon.py --no-hardware --port 24710 …`):
/// `UA_SANDBOX_ENGINE=127.0.0.1:24710 cargo test -p engine-model --test sessions -- --ignored`.
/// Refuses port 4710 (a real engine or the hardware daemon).
#[test]
#[ignore]
fn sandbox_daemon_session_menu() {
    let Ok(ep) = std::env::var("UA_SANDBOX_ENGINE") else { return };
    let ep = ua_engine::Endpoint::parse(&ep);
    assert_ne!(ep.port, 4710, "never point this test at a real engine");
    let mut m = EngineMixer::connect(ep, true);
    pump_until(&mut m, "live", |m| *m.store().status() == Status::Live);
    eprintln!("sandbox session: {:?}", m.session());
    let mut v = ViewState::default();
    let tap = |v: &mut ViewState, m: &mut EngineMixer, c: Control| v.dispatch(Action::Tap(sid(c)), m);
    tap(&mut v, &mut m, Control::Session(SessionOp::SaveAs));
    while v.text_edit.as_ref().is_some_and(|t| !t.text.is_empty()) {
        v.key_backspace();
    }
    v.key_text("Sandbox Test");
    v.key_enter(&mut m);
    tap(&mut v, &mut m, Control::Session(SessionOp::Save));
    tap(&mut v, &mut m, Control::Session(SessionOp::Load(0)));
    tap(&mut v, &mut m, Control::SessionSaveChoice(SaveChoice::DontSave));
    tap(&mut v, &mut m, Control::Session(SessionOp::New));
    tap(&mut v, &mut m, Control::SessionSaveChoice(SaveChoice::DontSave));
    tap(&mut v, &mut m, Control::Session(SessionOp::Open));
    v.key_text("/tmp/none.uadmix");
    v.key_enter(&mut m);
    let end = Instant::now() + Duration::from_millis(800);
    while Instant::now() < end {
        m.pump();
        std::thread::sleep(Duration::from_millis(10));
    }
    eprintln!("sent {}, suppressed {}", m.store().writer().sent_count(), m.store().writer().suppressed_count());
    eprintln!("after: {:?}", m.session());
}

/// CURRENT SESSION folder rows: Delete → `DeleteFolder {folder, type}`, Rename →
/// `RenameSession {origName, newName}` (folders keep their parent), New Folder inside a folder;
/// Show In Finder reveals locally and sends nothing.
#[test]
fn folder_rows_and_show_in_finder() {
    use rack::surfaces::session::{PanelRowKind, REVEAL_SESSIONS_FOLDER, panel_rows};
    let server = MockEngine::start(tree(), MockOptions::default()).unwrap();
    server.set_values("/Session", serde_json::json!([
        {"value": "Session", "type": "file", "locked": false},
        {"value": "Live", "type": "folder", "locked": false, "children": [{"value": "Band", "type": "file", "locked": false}]}
    ]));
    let mut m = live(&server, true);
    pump_until(&mut m, "list", |m| m.session().files.len() == 2);
    assert!(m.session().local_dir.is_some(), "MockEngine runs on 127.0.0.1, so the Sessions folder is local");
    m.set_local_sessions_dir(Some("/Users/me/Documents/Universal Audio/Sessions".into()));
    let rows = panel_rows(&m.session());
    let folder = rows.iter().position(|r| matches!(&r.kind, PanelRowKind::Folder { path, root: false } if path == "Live")).unwrap() as u16;
    let file = rows.iter().position(|r| matches!(r.kind, PanelRowKind::File(_)) && r.label == "Band").unwrap() as u16;

    let mut v = ViewState::default();
    v.dispatch(Action::Tap(sid(Control::Session(SessionOp::FolderRename(folder)))), &mut m);
    assert_eq!(v.text_edit.as_ref().map(|t| t.text.as_str()), Some("Live"));
    for _ in 0..4 {
        v.key_backspace();
    }
    v.key_text("Gigs");
    v.key_enter(&mut m);
    v.dispatch(Action::Tap(sid(Control::Session(SessionOp::NewFolderIn(folder)))), &mut m);
    v.key_text("Rehearsal");
    v.key_enter(&mut m);
    v.dispatch(Action::Tap(sid(Control::Session(SessionOp::FolderDelete(folder)))), &mut m);
    let w = writes_after(&server, 3);
    assert!(w.contains(&r#"post /RenameSession {"newName":"Gigs","origName":"Live"}"#.to_string()), "{w:?}");
    assert!(w.contains(&r#"post /NewFolder {"folder":"Live/Rehearsal","type":"session"}"#.to_string()), "{w:?}");
    assert!(w.contains(&r#"post /DeleteFolder {"folder":"Live","type":"session"}"#.to_string()), "{w:?}");

    // Show In Finder: a file URL for the file, the folder, and the Sessions folder; no writes.
    let before = server.write_commands().len();
    for row in [file, folder, REVEAL_SESSIONS_FOLDER] {
        v.dispatch(Action::Tap(sid(Control::Session(SessionOp::Reveal(row)))), &mut m);
    }
    assert_eq!(
        v.take_open_urls(),
        [
            "file:///Users/me/Documents/Universal%20Audio/Sessions/Live/Band.uadmix",
            "file:///Users/me/Documents/Universal%20Audio/Sessions/Live",
            "file:///Users/me/Documents/Universal%20Audio/Sessions",
        ]
    );
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(server.write_commands().len(), before);
    // Without a local Sessions folder the rows are disabled and reveal nothing.
    m.set_local_sessions_dir(None);
    let menu = rack::menu::session_row_menu(&m, file);
    assert!(menu.items().iter().any(|i| i.label == rack::menu::SHOW_IN_FINDER_LABEL && !i.enabled));
    v.dispatch(Action::Tap(sid(Control::Session(SessionOp::Reveal(file)))), &mut m);
    assert!(v.take_open_urls().is_empty());
}

fn writes_after(server: &MockEngine, n: usize) -> Vec<String> {
    let end = Instant::now() + Duration::from_secs(10);
    while server.write_commands().len() < n {
        assert!(Instant::now() < end, "timed out waiting for {n} writes: {:?}", server.write_commands());
        std::thread::sleep(Duration::from_millis(5));
    }
    server.write_commands()
}

#[test]
fn quit_posts_save_last_used_session_only_with_writes_on() {
    let server = MockEngine::start(tree(), MockOptions::default()).unwrap();
    let mut m = live(&server, true);
    m.quit();
    let end = Instant::now() + Duration::from_secs(10);
    while server.write_commands().is_empty() {
        assert!(Instant::now() < end, "timed out waiting for the post");
        m.pump();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(server.write_commands(), ["post /SaveLastUsedSession {}"]);

    let server = MockEngine::start(tree(), MockOptions::default()).unwrap();
    let mut m = live(&server, false);
    m.quit();
    std::thread::sleep(Duration::from_millis(100));
    m.pump();
    assert!(server.write_commands().is_empty(), "{:?}", server.write_commands());
}
