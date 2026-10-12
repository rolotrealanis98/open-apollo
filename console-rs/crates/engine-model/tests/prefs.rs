//! Console's client preferences (Settings › OPTIONS / MIDI): kept in a JSON file like Console's
//! own preferences, and OPTIONS › MODIFIERS TIMEOUT disarming the navigator modifiers.

use std::time::{Duration, Instant};

use engine_model::EngineMixer;
use rack::model::MixerModel;
use rack::surfaces::view::Modifier;
use rack::settings::{PropertySource, SettingOp};
use rack::{Action, Control, ControlId, ViewState};
use ua_engine::Snapshot;

fn offline() -> EngineMixer {
    let p = format!("{}/../ua-engine/tests/fixtures/apollo-x4-12.0.0.snapshot.json", env!("CARGO_MANIFEST_DIR"));
    EngineMixer::new(ua_engine::Store::from_snapshot(Snapshot::load(p).unwrap()))
}

#[test]
fn preferences_survive_a_restart() {
    let dir = std::env::temp_dir().join(format!("uaconsole-prefs-{}", std::process::id()));
    let file = dir.join("console-prefs.json");
    let _ = std::fs::remove_dir_all(&dir);
    let mut a = offline();
    a.set_prefs_path(Some(file.clone()));
    a.apply_setting("pref:modifiers_timeout", &SettingOp::Text("10".into()));
    a.apply_setting("pref:tap_tempo_channel", &SettingOp::Text("3".into()));
    assert!(file.exists(), "saved on change");
    let mut b = offline();
    assert_eq!(b.prop("pref:modifiers_timeout").map(|v| v.display()).as_deref(), Some("6"), "defaults before loading");
    b.set_prefs_path(Some(file.clone()));
    assert_eq!(b.prop("pref:modifiers_timeout").map(|v| v.display()).as_deref(), Some("10"));
    assert_eq!(b.prop("pref:tap_tempo_channel").map(|v| v.display()).as_deref(), Some("3"));
    assert_eq!(b.prop("pref:scenes_event").map(|v| v.display()).as_deref(), Some("PROGRAM CHANGE"), "missing keys keep defaults");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn modifiers_disarm_after_the_timeout() {
    let mut m = offline();
    let mut v = ViewState::default();
    let nav = |control| ControlId { strip: "nav".into(), control };
    v.dispatch(Action::Toggle(nav(Control::Modifier(Modifier::Power))), &mut m);
    assert_eq!(v.modifier, Some(Modifier::Power));
    let armed = v.modifier_armed.unwrap();
    // Default 6 s.
    assert!(!v.expire_modifier(&m, armed + Duration::from_secs(5)));
    assert_eq!(v.modifier_deadline(&m), Some(armed + Duration::from_secs(6)));
    assert!(v.expire_modifier(&m, armed + Duration::from_secs(6)));
    assert_eq!(v.modifier, None);
    // MODIFIERS TIMEOUT 0: stays armed.
    m.apply_setting("pref:modifiers_timeout", &SettingOp::Text("0".into()));
    v.dispatch(Action::Toggle(nav(Control::Modifier(Modifier::Remove))), &mut m);
    assert!(!v.expire_modifier(&m, Instant::now() + Duration::from_secs(3600)));
    assert_eq!(v.modifier, Some(Modifier::Remove));
}
