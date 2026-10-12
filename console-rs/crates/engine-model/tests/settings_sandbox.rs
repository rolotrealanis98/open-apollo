//! Settings writes against a live engine. Ignored by default: run it only against a private
//! software engine, never a real one, e.g. open-apollo's daemon copied to a scratch directory
//! (its persistence file lives next to the script) and started with
//! `ua_mixer_daemon.py --no-hardware --host 127.0.0.1 --port 26710 --helper-port 26720 --no-ws --no-bonjour`,
//! then `UA_SANDBOX_ENGINE=127.0.0.1:26710 cargo test -p engine-model --test settings_sandbox -- --ignored`.

use std::time::{Duration, Instant};

use engine_model::EngineMixer;
use rack::MixerModel;
use rack::settings::{self, SettingOp, SettingsGroup};

fn connect(addr: &str, writes: bool) -> EngineMixer {
    let mut m = EngineMixer::connect(ua_engine::Endpoint::parse(addr), writes);
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        m.pump();
        if !m.inputs().is_empty() && m.properties().and_then(|p| p.prop("/SampleRate")).is_some() {
            std::thread::sleep(Duration::from_millis(300));
            m.pump();
            return m;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("no engine tree from {addr}");
}

fn settle(m: &mut EngineMixer) {
    let end = Instant::now() + Duration::from_millis(600);
    while Instant::now() < end {
        m.pump();
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
#[ignore]
fn settings_round_trip_on_a_sandbox_engine() {
    let Ok(addr) = std::env::var("UA_SANDBOX_ENGINE") else {
        eprintln!("UA_SANDBOX_ENGINE not set; skipping");
        return;
    };
    let mut w = connect(&addr, true);
    let src = w.properties().unwrap();
    // Pick a different sample rate, flip DIGITAL MIRROR / PT MODE, change CLIP HOLD, rename.
    let rates = src.choices("/SampleRate");
    let cur = src.prop("/SampleRate").unwrap();
    let rate_i = rates.iter().position(|c| c.enabled && !c.value.same(&cur)).expect("another rate");
    let want_rate = rates[rate_i].value.clone();
    let hw = settings::fields(src, SettingsGroup::Hardware);
    let pt = hw.iter().find(|f| f.label == "PT MODE").map(|f| (f.key.clone(), f.value_text()));
    let clip = src.choices("/ClipHold");
    let clip_cur = src.prop("/ClipHold").unwrap();
    let clip_i = clip.iter().position(|c| !c.value.same(&clip_cur)).unwrap();
    let want_clip = clip[clip_i].value.clone();
    let old_name = src.prop("/devices/0/DeviceName").unwrap();

    w.apply_setting("/SampleRate", &SettingOp::Choose(rate_i));
    w.apply_setting("/ClipHold", &SettingOp::Choose(clip_i));
    if let Some((k, v)) = &pt {
        w.apply_setting(k, &SettingOp::Choose(if v == "ON" { 0 } else { 1 }));
    }
    w.apply_setting("/devices/0/DeviceName", &SettingOp::Text("Sandbox x4".into()));
    w.apply_setting("pref:modifiers_timeout", &SettingOp::Choose(2));
    settle(&mut w);

    // A second, read-only client sees the engine's new values.
    let r = connect(&addr, false);
    let p = r.properties().unwrap();
    assert!(p.prop("/SampleRate").unwrap().same(&want_rate), "rate {:?}", p.prop("/SampleRate"));
    assert!(p.prop("/ClipHold").unwrap().same(&want_clip));
    assert_eq!(p.prop("/devices/0/DeviceName").unwrap().as_str(), Some("Sandbox x4"));
    if let Some((k, v)) = &pt {
        let now = settings::find_field(p, k).unwrap().value_text();
        assert_ne!(&now, v, "PT MODE flipped");
    }
    // Preferences never reach the engine.
    assert!(p.prop("pref:modifiers_timeout").unwrap().as_str() == Some("NONE"));

    // Restore.
    w.apply_setting("/SampleRate", &SettingOp::Choose(rates.iter().position(|c| c.value.same(&cur)).unwrap()));
    w.apply_setting("/ClipHold", &SettingOp::Choose(clip.iter().position(|c| c.value.same(&clip_cur)).unwrap()));
    w.apply_setting("/devices/0/DeviceName", &SettingOp::Text(old_name.display()));
    if let Some((k, v)) = &pt {
        w.apply_setting(k, &SettingOp::Choose(if v == "ON" { 1 } else { 0 }));
    }
    settle(&mut w);
}

#[test]
#[ignore]
fn read_only_connections_send_nothing() {
    let Ok(addr) = std::env::var("UA_SANDBOX_ENGINE") else { return };
    let mut ro = connect(&addr, false);
    let before = ro.properties().unwrap().prop("/ClipHold").unwrap();
    let choices = ro.properties().unwrap().choices("/ClipHold");
    let i = choices.iter().position(|c| !c.value.same(&before)).unwrap();
    ro.apply_setting("/ClipHold", &SettingOp::Choose(i));
    ro.apply_setting("post:Identify:/devices/0", &SettingOp::Press);
    settle(&mut ro);
    let check = connect(&addr, false);
    assert!(check.properties().unwrap().prop("/ClipHold").unwrap().same(&before), "a read-only client changed the engine");
}
