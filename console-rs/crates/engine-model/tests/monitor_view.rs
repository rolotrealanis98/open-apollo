//! Control-room and talkback-strip commands as Console 1.3.1 sends them, against MockEngine.
//! Provenance is on the implementations (`engine-model/src/surfaces.rs`).

use std::time::{Duration, Instant};

use engine_model::EngineMixer;
use rack::model::MixerModel;
use rack::{Action, Control, ControlId, ViewState};
use ua_engine::{MockEngine, MockOptions, Snapshot, Status};

fn tree() -> ua_engine::Tree {
    let p = format!("{}/../ua-engine/tests/fixtures/apollo-x4-12.0.0.snapshot.json", env!("CARGO_MANIFEST_DIR"));
    Snapshot::load(p).unwrap().tree
}

fn live(server: &MockEngine) -> EngineMixer {
    let mut m = EngineMixer::connect(server.endpoint(), true);
    let end = Instant::now() + Duration::from_secs(10);
    while *m.store().status() != Status::Live {
        assert!(Instant::now() < end, "timed out: live");
        m.pump();
        std::thread::sleep(Duration::from_millis(5));
    }
    m
}

fn writes_after(server: &MockEngine, n: usize) -> Vec<String> {
    let end = Instant::now() + Duration::from_secs(10);
    while server.write_commands().len() < n {
        assert!(Instant::now() < end, "timed out waiting for {n} writes: {:?}", server.write_commands());
        std::thread::sleep(Duration::from_millis(5));
    }
    server.write_commands()
}

fn monitor(control: Control) -> ControlId {
    ControlId { strip: "monitor".into(), control }
}

/// Values set by a gesture, without the `/UndoRecording` brackets.
fn sets(w: &[String]) -> Vec<String> {
    w.iter().filter(|c| !c.starts_with("set /UndoRecording")).cloned().collect()
}

#[test]
fn control_room_source_sets_monitor_mix_in_source() {
    let server = MockEngine::start(tree(), MockOptions::default()).unwrap();
    let mut m = live(&server);
    let mut v = ViewState::default();
    v.dispatch(Action::Tap(monitor(Control::CrSource(2))), &mut m);
    v.dispatch(Action::Tap(monitor(Control::CrSource(0))), &mut m);
    let w = writes_after(&server, 6);
    assert_eq!(
        sets(&w),
        [r#"set /devices/0/outputs/18/MixInSource/value "cue2""#, r#"set /devices/0/outputs/18/MixInSource/value "mon""#]
    );
}

#[test]
fn talkback_to_monitor_toggles_talkback_in_physical_cr() {
    let server = MockEngine::start(tree(), MockOptions::default()).unwrap();
    let mut m = live(&server);
    assert_eq!(m.monitor_options().talkback_to_monitor, Some(false));
    let mut v = ViewState::default();
    v.dispatch(Action::Tap(monitor(Control::TalkbackToMonitor)), &mut m);
    let w = writes_after(&server, 3);
    assert_eq!(sets(&w), ["set /TalkbackInPhysicalCR/value true"]);
}
