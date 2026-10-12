//! Synthetic noncontiguous units; no assets, hardware or vendor application.
use std::time::{Duration, Instant};
use engine_model::EngineMixer;
use rack::{Action, Control, ControlId, MixerModel, ViewState};
use rack::settings::{Panel, SendsOf};
use ua_engine::{MockEngine, MockOptions, Snapshot, Status, Store, SubscriptionPlan};

fn snapshot() -> Snapshot {
    let device = |id: usize, order: i64, online: bool| serde_json::json!({
        "properties": {"DeviceName":{"value":format!("Fixture {id}")}, "DeviceOnline":{"value":online}, "MultiUnitOrder":{"value":order}},
        "children":{"inputs":{"children":{"0":{
            "properties":{"Name":{"value":format!("Unit {id} Input")}, "IOType":{"value":"Line"}, "Mute":{"type":"bool","value":false}, "Pan":{"value":0.0}, "FaderLevelTapered":{"value":0.5}},
            "children":{"effects":{"children":{"0":{"properties":{"EffectName":{"value":"UAD Test Plugin"},"Power":{"value":true}}}}}}
        }}}}
    });
    Snapshot::from_json(&serde_json::json!({"properties":{"Version":{"value":"test"},"CueBusCount":{"value":2}},"children":{"devices":{"children":{"2":device(2,1,true),"7":device(7,0,false)}}}})).unwrap()
}

#[test]
fn cascade_order_sparse_ids_and_offline_projection() {
    let m = EngineMixer::new(Store::from_snapshot(snapshot()));
    assert_eq!(m.store().topologies().iter().map(|t| t.device).collect::<Vec<_>>(), [7,2]);
    assert_eq!(m.inputs().iter().map(|s| &*s.id).collect::<Vec<_>>(), ["/devices/7/inputs/0","/devices/2/inputs/0"]);
    assert_eq!(m.inputs_for_view(false).iter().map(|s| &*s.id).collect::<Vec<_>>(), ["/devices/2/inputs/0"]);
    assert_eq!(m.inputs_for_view(true).len(), 2);
    assert_eq!(m.devices().iter().map(|d| d.index).collect::<Vec<_>>(), [7,2]);
    assert_eq!(m.store().device_name(), Some("Fixture 7"));
    let off = &m.inputs()[0];
    let on = &m.inputs()[1];
    assert!(m.insert_labels(off)[0].as_ref().unwrap().italic);
    assert!(!m.insert_labels(on)[0].as_ref().unwrap().italic);
}

#[test]
fn subscriptions_cover_each_unit_and_shared_roots_once() {
    let m = EngineMixer::new(Store::from_snapshot(snapshot()));
    let plan = SubscriptionPlan::for_devices(m.store().topologies(), m.store().tree());
    let all: Vec<_> = plan.batches.iter().flat_map(|b| &b.paths).collect();
    assert!(all.iter().any(|p| p.as_str() == "/devices/2/inputs/0/Mute/value"));
    assert!(all.iter().any(|p| p.as_str() == "/devices/7/inputs/0/Mute/value"));
    assert_eq!(all.iter().filter(|p| p.as_str() == "/CueBusCount/value").count(), 1);
    assert_eq!(all.iter().collect::<std::collections::HashSet<_>>().len(), all.len());
}

#[test]
fn second_unit_writes_and_live_order_online_pushes_keep_identity() {
    let server = MockEngine::start(snapshot().tree, MockOptions::default()).unwrap();
    let mut m = EngineMixer::connect(server.endpoint(), true);
    let wait = |m: &mut EngineMixer, ok: &dyn Fn(&EngineMixer)->bool| {
        let end = Instant::now()+Duration::from_secs(10);
        while !ok(m) { assert!(Instant::now()<end); m.pump(); std::thread::sleep(Duration::from_millis(5)); }
    };
    wait(&mut m, &|m| *m.store().status()==Status::Live);
    m.apply(&Action::Toggle(ControlId { strip:"/devices/7/inputs/0".into(),control:Control::Mute }));
    let end = Instant::now()+Duration::from_secs(10);
    while server.write_commands().is_empty() { assert!(Instant::now()<end); std::thread::sleep(Duration::from_millis(5)); }
    assert!(server.write_commands().iter().any(|w| w == "set /devices/7/inputs/0/Mute/value true"));
    assert_eq!(server.value("/devices/2/inputs/0/Mute").and_then(|v| v.as_bool()), Some(false));
    let rev = m.store().topology_revision();
    server.push("/devices/7/DeviceOnline", true);
    wait(&mut m, &|m| m.inputs_for_view(false).len()==2);
    assert!(m.store().topology_revision()>rev);
    server.push("/devices/2/MultiUnitOrder", -1);
    wait(&mut m, &|m| m.store().topology().device==2);
    assert_eq!(&*m.inputs()[0].id, "/devices/2/inputs/0");
}

#[test]
fn send_panel_targets_retain_device_across_navigation_and_cli() {
    let of = SendsOf::Input(3).on_device(7);
    assert_eq!(of.node().as_deref(), Some("/devices/7/inputs/3"));
    assert_eq!(of.with_index(4).node().as_deref(), Some("/devices/7/inputs/4"));
    let mut view = ViewState::default();
    assert!(view.open_named("input-sends:7:3"));
    assert_eq!(view.sheet, Some(rack::surfaces::Sheet::Panel(Panel::Sends(of))));
}
