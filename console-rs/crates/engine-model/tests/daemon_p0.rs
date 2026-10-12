//! P0 acceptance against an OWNED --no-hardware daemon, never a real engine.
//! UA_SANDBOX_ENGINE=127.0.0.1:34713 UA_SANDBOX_HELPER=127.0.0.1:34723
//! cargo test -p engine-model --test daemon_p0 -- --ignored --test-threads=1
//! The insert test requires a synthetic parameter at input 0 / effect 0 / parameter 0.

use std::time::{Duration, Instant};
use engine_model::EngineMixer;
use rack::{Action, Control, ControlId, MixerModel};
use rack::menu::MenuCmd;
use rack::presets::{PresetCmd, PresetScope};
use ua_engine::{Endpoint, Status};

fn connect() -> Option<EngineMixer> {
    let addr = std::env::var("UA_SANDBOX_ENGINE").ok()?;
    let ep = Endpoint::parse(&addr);
    assert_eq!(ep.host, "127.0.0.1");
    assert!(![4710, 4720].contains(&ep.port), "never test writes on production ports");
    let mut m = EngineMixer::connect(ep, true);
    until(&mut m, |m| *m.store().status() == Status::Live);
    assert_eq!(m.store().value("/openapollo/PluginBackend").and_then(|v| v.as_str()), Some("software-model"), "refuse a daemon without explicit software-model marker");
    Some(m)
}

#[track_caller]
fn until(m: &mut EngineMixer, ok: impl Fn(&EngineMixer) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ok(m) {
        assert!(Instant::now() < deadline, "daemon state did not settle: {:?}", m.store().last_error());
        m.pump();
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn id(strip: &str, control: Control) -> ControlId {
    ControlId { strip: strip.into(), control }
}

#[test]
#[ignore = "requires an owned software daemon with synthetic insert parameters"]
fn insert_clipboards_presets_and_moves_round_trip() {
    let Some(mut m) = connect() else { return };
    let src = "/devices/0/inputs/0";
    let dst = "/devices/0/inputs/1";
    let effect = format!("{src}/effects/0");
    m.store_mut().commit(&format!("{dst}/ChannelHidden"), false);
    until(&mut m, |m| m.inputs().iter().any(|s| &*s.id == dst));
    let name = m.store().value(&format!("{effect}/EffectName")).and_then(|v| v.as_str()).expect("seeded source slot").to_owned();
    let value = m.store().value(&format!("{effect}/parameters/0/NormalizedValue")).and_then(|v| v.as_f64()).expect("synthetic parameter");
    let mut observer = connect().unwrap();
    observer.store_mut().retain_subscriptions(&[
        format!("{effect}/parameters/0/NormalizedValue"),
        format!("{dst}/effects/0/parameters/0/NormalizedValue"),
        format!("{effect}/CanPastePluginBarSettings"),
    ]);
    m.apply(&Action::Tap(id(src, Control::InsertCopy(0))));
    m.apply(&Action::Tap(id(dst, Control::Menu(MenuCmd::InsertPaste(0)))));
    // A fresh observer cannot see the writer's optimistic value or the clipboard.
    until(&mut observer, |o| o.store().value(&format!("{dst}/effects/0/parameters/0/NormalizedValue")).and_then(|v| v.as_f64()) == Some(value));
    assert_eq!(observer.store().value(&format!("{dst}/effects/0/EffectName")).and_then(|v| v.as_str()), Some(name.as_str()));
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let preset = format!("P0 Roundtrip {stamp}");
    m.set_text(&id(src, Control::PresetCmd(PresetScope::Insert(0), PresetCmd::SaveAs)), &preset);
    // Wait for save before changing the parameter.
    until(&mut observer, |o| o.store().value(&format!("{effect}/Preset")).and_then(|v| v.as_str()) == Some(preset.as_str()));
    let changed = if value == 0.8 { 0.1 } else { 0.8 };
    m.store_mut().commit(&format!("{effect}/parameters/0/NormalizedValue"), changed);
    until(&mut observer, |o| o.store().value(&format!("{effect}/parameters/0/NormalizedValue")).and_then(|v| v.as_f64()) == Some(changed));
    m.set_text(&id(src, Control::Preset(PresetScope::Insert(0))), &preset);
    until(&mut observer, |o| o.store().value(&format!("{effect}/parameters/0/NormalizedValue")).and_then(|v| v.as_f64()) == Some(value));
    m.apply(&Action::Tap(id(src, Control::Menu(MenuCmd::CopySettings(0)))));
    until(&mut observer, |o| o.store().value(&format!("{effect}/CanPastePluginBarSettings")).and_then(|v| v.as_bool()) == Some(true));
    m.apply(&Action::Tap(id(dst, Control::InsertMove { slot: 1, from: src.into(), from_slot: 0, copy: false })));
    until(&mut observer, |o| o.store().value(&format!("{effect}/EffectName")).and_then(|v| v.as_str()) == Some(""));
    until(&mut observer, |o| o.store().value(&format!("{dst}/effects/1/EffectName")).and_then(|v| v.as_str()) == Some(name.as_str()));
    // Restore the seeded source so repeated serial runs remain independent.
    m.apply(&Action::Tap(id(src, Control::InsertMove { slot: 0, from: dst.into(), from_slot: 1, copy: true })));
    until(&mut observer, |o| o.store().value(&format!("{effect}/EffectName")).and_then(|v| v.as_str()) == Some(name.as_str()));
}

#[test]
#[ignore = "requires an owned software-only daemon"]
fn monitor_hidden_channels_demo_and_session_folders_round_trip() {
    let Some(mut m) = connect() else { return };
    let mut observer = connect().unwrap();
    let talk = !m.store().value("/TalkbackOn").and_then(|v| v.as_bool()).unwrap_or(false);
    m.apply(&Action::Tap(id("global", Control::Talkback)));
    until(&mut observer, |o| o.store().value("/TalkbackOn").and_then(|v| v.as_bool()) == Some(talk));
    let to_monitor = !m.store().value("/TalkbackInPhysicalCR").and_then(|v| v.as_bool()).unwrap_or(false);
    m.apply(&Action::Tap(id("global", Control::TalkbackToMonitor)));
    until(&mut observer, |o| o.store().value("/TalkbackInPhysicalCR").and_then(|v| v.as_bool()) == Some(to_monitor));
    m.store_mut().commit("/devices/0/inputs/1/ChannelHidden", true);
    m.store_mut().commit("/devices/0/outputs/18/MixInSource", "cue4");
    until(&mut observer, |o| o.store().value("/devices/0/outputs/18/MixInSource").and_then(|v| v.as_str()) == Some("cue4"));
    until(&mut observer, |o| o.store().value("/devices/0/inputs/1/ChannelHidden").and_then(|v| v.as_bool()) == Some(true));
    if m.store().value("/plugins/0/DemoAvailable").and_then(|v| v.as_bool()) == Some(true) {
        m.start_plugin_demo("/plugins/0");
        until(&mut observer, |o| o.store().value("/plugins/0/Demoing").and_then(|v| v.as_bool()) == Some(true));
    }
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let folder = format!("P0 {stamp}");
    let renamed = format!("P0 renamed {stamp}");
    let session = format!("{folder}/Session");
    m.store_mut().session(&ua_engine::SessionCommand::NewFolder { folder: folder.clone() });
    m.store_mut().session(&ua_engine::SessionCommand::SaveAs { name: session.clone() });
    until(&mut observer, |o| o.store().value("/Session").and_then(|v| v.as_str()) == Some(session.as_str()));
    m.store_mut().session(&ua_engine::SessionCommand::Rename { from: folder, to: renamed.clone() });
    let session = format!("{renamed}/Session");
    until(&mut observer, |o| o.store().value("/Session").and_then(|v| v.as_str()) == Some(session.as_str()));
}

#[test]
#[ignore = "requires the owned daemon's private helper port"]
fn helper_interface_selection_round_trip() {
    let Some(mut m) = connect() else { return };
    let Ok(addr) = std::env::var("UA_SANDBOX_HELPER") else { return };
    let endpoint = Endpoint::parse(&addr);
    assert_eq!(endpoint.host, "127.0.0.1");
    assert!(![4710, 4720].contains(&endpoint.port));
    m.connect_helper_writable(endpoint.clone(), true);
    until(&mut m, |m| m.helper().is_some_and(|h| *h.status() == Status::Live));
    let encoded = m.helper().unwrap().value("/dante/available_network_adapters").and_then(|v| v.as_str()).expect("adapter metadata");
    let entries: serde_json::Value = serde_json::from_str(encoded).unwrap();
    let name = entries[0]["adapter_name"].as_str().unwrap().to_owned();
    assert!(m.helper_mut().unwrap().set("/dante/primary_network_adapter", &ua_engine::Value::String(name.clone())));
    let mut observer = connect().unwrap();
    observer.connect_helper(endpoint);
    until(&mut observer, |m| m.helper().and_then(|h| h.value("/dante/primary_network_adapter")).and_then(|v| v.as_str()) == Some(name.as_str()));
}
