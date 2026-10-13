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
    // Toggled on press, unbracketed (`Rack_Talkback` 0x181489be0, UAClnt_Values::Set).
    let w = writes_after(&server, 1);
    assert_eq!(w, ["set /TalkbackInPhysicalCR/value true"]);
}

/// TALK: a press writes the opposite of `/TalkbackOn` at once, with no /UndoRecording bracket;
/// a release after a long press (`HitKind::Latch` → `Set(Talkback, 0)`) writes false.
#[test]
fn talk_latches_on_press_and_releases_after_a_long_press() {
    let server = MockEngine::start(tree(), MockOptions::default()).unwrap();
    let mut m = live(&server);
    assert_eq!(m.monitor_options().talkback, Some(false));
    let mut v = ViewState::default();
    v.dispatch(Action::Toggle(monitor(Control::Talkback)), &mut m);
    assert_eq!(m.monitor_options().talkback, Some(true), "the button lights at once");
    assert_eq!(writes_after(&server, 1), ["set /TalkbackOn/value true"]);
    v.dispatch(Action::Set(monitor(Control::Talkback), 0.0), &mut m);
    assert_eq!(writes_after(&server, 2), ["set /TalkbackOn/value true", "set /TalkbackOn/value false"]);
    assert_eq!(m.monitor_options().talkback, Some(false));
    // A release when TALK is already off writes nothing more.
    v.dispatch(Action::Set(monitor(Control::Talkback), 0.0), &mut m);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(server.write_commands().len(), 2);
}

#[test]
fn show_hide_channels_lists_inputs_and_toggles_channel_hidden() {
    let server = MockEngine::start(tree(), MockOptions::default()).unwrap();
    let mut m = live(&server);
    let list = m.channel_list();
    assert!(!list.is_empty() && list.iter().all(|c| !c.hidden), "{list:?}");
    assert!(list.iter().all(|c| !c.name.is_empty() && c.id.starts_with("/devices/0/inputs/")));
    let first = list[0].id.clone();
    let mut v = ViewState::default();
    assert!(v.run_command_id("show_hide_channels", &mut m));
    assert!(v.show_channels);
    v.dispatch(Action::Tap(ControlId { strip: first.clone().into(), control: Control::ChannelVisible }), &mut m);
    let w = writes_after(&server, 3);
    assert_eq!(sets(&w), [format!("set {first}/ChannelHidden/value true")]);
    v.dispatch(Action::Tap(ControlId { strip: "channels".into(), control: Control::ChannelsClose }), &mut m);
    assert!(!v.show_channels);
}

#[test]
fn view_menu_monitor_sections_toggle_the_layout() {
    let server = MockEngine::start(tree(), MockOptions::default()).unwrap();
    let mut m = live(&server);
    let mut v = ViewState::default();
    let full = rack::window::WindowLayout::for_model(&m, &v);
    assert!(full.monitor && full.monitor_w() > 0.0);
    assert!(v.run_command_id("show_monitor", &mut m));
    let hidden = rack::window::WindowLayout::for_model(&m, &v);
    assert_eq!(hidden.monitor_w(), 0.0);
    assert_eq!(hidden.monitor_x(), hidden.width);
    assert!(v.run_command_id("show_monitor_correction", &mut m));
    assert!(!v.show_monitor_correction);
    // Compact Monitor Faders is for the HB/Volt monitor fader; Apollo's monitor has a knob.
    assert!(!v.run_command_id("toggle_compact_monitor_fader", &mut m));
    // The fixture's one unit was captured offline (`DeviceOnline` false), so Show Offline
    // Devices is enabled and ticks.
    assert_eq!(m.devices(), [rack::model::DeviceInfo { index: 0, name: "Apollo x4".into(), online: false }]);
    assert!(v.run_command_id("show_offline_devices", &mut m));
    assert!(v.show_offline_devices);
}
