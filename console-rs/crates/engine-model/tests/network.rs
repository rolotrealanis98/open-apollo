//! Settings › NETWORK › NETWORK INTERFACE: the UA Mixer Helper's adapters and choice
//! (`/dante/available_network_adapters`, `/dante/primary_network_adapter`; Console 1.3.1 captures
//! s17, s30), Console's SWITCH NETWORK INTERFACE? question, and the helper write behind YES.

use std::time::{Duration, Instant};

use engine_model::EngineMixer;
use rack::settings::{PropertySource, SettingsGroup, fields};
use rack::surfaces::Popover;
use rack::{Action, Control, ControlId, ViewState};
use ua_engine::{HelperStore, MockEngine, MockOptions, PRIMARY_NETWORK_ADAPTER, Snapshot, Value};

const ADAPTERS: &str = r#"[{"adapter_name":"en0","adapter_address":"192.168.1.4"},{"adapter_name":"en14","adapter_address":""},{"adapter_name":"en24","adapter_address":"10.0.0.5"}]"#;

fn helper_json() -> serde_json::Value {
    serde_json::json!({
        "properties": {},
        "children": [{
            "path": "dante",
            "properties": {
                "available_network_adapters": {"value": ADAPTERS},
                "primary_network_adapter": {"value": "en24"}
            },
            "children": []
        }]
    })
}

fn offline() -> EngineMixer {
    let p = format!("{}/../ua-engine/tests/fixtures/apollo-x4-12.0.0.snapshot.json", env!("CARGO_MANIFEST_DIR"));
    let mut m = EngineMixer::new(ua_engine::Store::from_snapshot(Snapshot::load(p).unwrap()));
    m.set_helper(HelperStore::from_helper_json(&helper_json()));
    m
}

#[test]
fn network_interface_lists_the_helper_adapters_and_asks_before_switching() {
    let mut m = offline();
    let fields = fields(&m, SettingsGroup::Network);
    let nic = fields.iter().find(|f| f.label == "NETWORK INTERFACE").expect("NETWORK INTERFACE");
    assert_eq!(&*nic.key, "nic:/dante/primary_network_adapter");
    match &nic.kind {
        rack::settings::FieldKind::Choice { options, selected, .. } => {
            assert_eq!(options, &["en0  (192.168.1.4)", "en14  ()", "en24  (10.0.0.5)"]);
            assert_eq!(*selected, Some(2));
        }
        k => panic!("{k:?}"),
    }
    assert_eq!(nic.display.as_deref(), Some("en24  (10.0.0.5)"));

    let mut v = ViewState::default();
    let sid = |control| ControlId { strip: "settings".into(), control };
    // Picking the current adapter asks nothing.
    v.dispatch(Action::Tap(sid(Control::SettingChoose(nic.key.clone().into(), 2))), &mut m);
    assert!(v.popover.is_none());
    // Another one: SWITCH NETWORK INTERFACE? with CANCEL / YES; nothing changes yet.
    v.dispatch(Action::Tap(sid(Control::SettingChoose(nic.key.clone().into(), 0))), &mut m);
    let Some(Popover::Confirm { title, message, ok, ok_label }) = v.popover.as_ref().map(|p| p.kind.clone()) else { panic!("{:?}", v.popover) };
    assert_eq!(title, "SWITCH NETWORK INTERFACE?");
    assert!(message.starts_with("Switching the network interface may interrupt audio"));
    assert_eq!(ok_label, "YES");
    assert_eq!(m.prop("helper:/dante/primary_network_adapter").map(|p| p.display()).as_deref(), Some("en24"));
    // YES: an offline mixer has writes off, so the helper keeps en24 (the write is suppressed).
    v.dispatch(Action::Tap(ok), &mut m);
    assert!(v.popover.is_none());
    assert_eq!(m.prop("helper:/dante/primary_network_adapter").map(|p| p.display()).as_deref(), Some("en24"));
}

#[test]
fn helper_set_is_refused_read_only_and_sent_when_writable() {
    let server = MockEngine::start(ua_engine::Tree::new(), MockOptions::default()).unwrap();
    let mut ro = HelperStore::new();
    ro.connect_writable(server.endpoint(), false);
    let mut rw = HelperStore::new();
    rw.connect_writable(server.endpoint(), true);
    let end = Instant::now() + Duration::from_secs(10);
    while *ro.status() != ua_engine::Status::Live || *rw.status() != ua_engine::Status::Live {
        assert!(Instant::now() < end, "helpers not live: {:?} {:?}", ro.status(), rw.status());
        ro.drain();
        rw.drain();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(!ro.set(PRIMARY_NETWORK_ADAPTER, &Value::String("en0".into())), "a read-only helper refuses set");
    assert!(rw.set(PRIMARY_NETWORK_ADAPTER, &Value::String("en0".into())));
    let end = Instant::now() + Duration::from_secs(10);
    while server.write_commands().is_empty() {
        assert!(Instant::now() < end, "no write");
        rw.drain();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(server.write_commands(), [r#"set /dante/primary_network_adapter/value "en0""#]);
}
