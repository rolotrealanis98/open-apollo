//! UA Mixer Helper (4720): format conversion, typed info, native inserts, and a read-only
//! end-to-end run against MockHelper.

mod common;

use std::time::Duration;

use common::wait_until;
use ua_engine::helper::{engine_node_from_helper, tree_from_helper};
use ua_engine::{
    HelperInfo, HelperStore, Json, MockHelper, NativeInsert, NativeOwner, Options, Status, Value,
};

const T: Duration = Duration::from_secs(10);

/// Sanitized `get /?recursive=1` reply of the Mac's helper 1.3.1.1152 (see README).
fn fixture() -> Json {
    serde_json::from_slice(
        &std::fs::read(common::manifest("tests/fixtures/helper-1.3.1.json")).unwrap(),
    )
    .unwrap()
}

/// The supercore device uid in the fixture.
fn device(root: &Json) -> String {
    let devs = root["children"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["path"] == "devices")
        .unwrap();
    devs["children"][0]["path"].as_str().unwrap().to_owned()
}

/// A native track shaped like the captured bus nodes, with the CL 1B mkII in `effect_0`.
/// SYNTHETIC: no live helper has shown a populated track (see README).
fn synthetic_track(root: &Json, name: &str, plugin: &str) -> Json {
    let devs = root["children"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["path"] == "devices")
        .unwrap();
    let buses = devs["children"][0]["children"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["path"] == "buses")
        .unwrap();
    let mut track = buses["children"][0].clone();
    track["properties"]["name"]["value"] = Json::String(name.into());
    let effects = track["children"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|c| c["path"] == "effects")
        .unwrap();
    let slot = effects["children"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|c| c["path"] == "effect_0")
        .unwrap();
    slot["properties"]["plugin_name"]["value"] = Json::String(plugin.into());
    slot["properties"]["plugin_id"]["value"] = Json::String("cl1b-mk2".into());
    slot["properties"]["plugin_power"]["value"] = Json::Bool(true);
    track
}

#[test]
fn converts_the_helper_tree_format() {
    let small: Json = serde_json::from_str(
        r#"{"properties": {"latency": {"read_only": true, "value": 1024}, "peak_hold": {"values": ["NONE", "3 SEC"], "value": "3 SEC"}},
            "commands": [{"path": "clear_clips"}],
            "children": [{"path": "status", "properties": {"version": {"read_only": true, "value": "1.3.1.1152"}}}, {"path": "dante"}]}"#,
    )
    .unwrap();
    let engine = engine_node_from_helper(&small);
    assert_eq!(
        engine["properties"]["latency"]["readonly"],
        Json::Bool(true)
    );
    assert!(engine["children"]["status"]["properties"]["version"].is_object());
    let tree = tree_from_helper(&small);
    assert_eq!(tree.value("/latency"), Some(&Value::Number(1024.0)));
    assert!(tree.meta("/latency").unwrap().readonly);
    assert_eq!(tree.meta("/peak_hold").unwrap().choices().len(), 2);
    assert_eq!(
        tree.value("/status/version/value"),
        Some(&Value::from("1.3.1.1152"))
    );
    assert_eq!(tree.stub_paths(), vec!["/dante"]); // a non-recursive child
    assert!(tree.node("/").unwrap().commands.is_some());
    // A reply envelope works too.
    let reply = serde_json::json!({"path": "/", "parameters": {"func_id": 1}, "data": small});
    assert_eq!(tree_from_helper(&reply), tree);
}

#[test]
fn helper_info_from_the_mac_capture() {
    let tree = tree_from_helper(&fixture());
    let info = HelperInfo::from_tree(&tree);
    assert_eq!(info.name.as_deref(), Some("mxhlpr"));
    assert_eq!(info.version.as_deref(), Some("1.3.1.1152"));
    assert_eq!(info.sample_rate, Some(48000.0));
    assert_eq!(info.sample_rates.len(), 6);
    assert_eq!(info.io_buffer_size, Some(128));
    assert_eq!(info.render_buffer_size, Some(512));
    assert_eq!(info.latency, Some(1024));
    assert!(!info.native_engine_active);
    assert_eq!(info.auto_gain_status.as_deref(), Some("stopped"));
    let apollo = info.apollo().expect("apollo driver");
    assert_eq!((apollo.num_inputs, apollo.num_outputs), (22, 24));
    assert_eq!(apollo.input_names[0], "MIC/LINE/HIZ 1");
    assert_eq!(info.drivers.len(), 1); // the fixture keeps only the Apollo
    // The supercore device: 5 buses (CUE 1-4, MAIN), no tracks, no native plug-ins.
    let dev = device(&fixture());
    let buses: Vec<String> = tree
        .children(&format!("/devices/{dev}/buses"))
        .iter()
        .filter_map(|b| {
            tree.value(&format!("/devices/{dev}/buses/{b}/name"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .collect();
    assert_eq!(buses.len(), 5);
    assert!(buses.contains(&"MAIN".to_owned()) && buses.contains(&"CUE 4".to_owned()));
    assert!(tree.children(&format!("/devices/{dev}/tracks")).is_empty());
    assert!(NativeInsert::list(&tree).is_empty());
}

#[test]
fn native_inserts_are_found_on_tracks() {
    let mut root = fixture();
    let dev = device(&root);
    let track = synthetic_track(&root, "SM7B", "Tube-Tech CL 1B mkII");
    {
        let devs = root["children"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|c| c["path"] == "devices")
            .unwrap();
        let tracks = devs["children"][0]["children"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|c| c["path"] == "tracks")
            .unwrap();
        let mut t = track;
        t["path"] = Json::String("6b3fa9e8a6f84c06b1f1e9b6f0a1c2d3".into());
        tracks
            .as_object_mut()
            .unwrap()
            .insert("children".into(), Json::Array(vec![t]));
    }
    let list = NativeInsert::list(&tree_from_helper(&root));
    assert_eq!(list.len(), 1);
    let n = &list[0];
    assert_eq!(n.owner, NativeOwner::Track);
    assert_eq!(n.owner_name.as_deref(), Some("SM7B"));
    assert_eq!((n.slot.as_str(), n.index), ("effect_0", Some(0)));
    assert_eq!(n.plugin_name, "Tube-Tech CL 1B mkII");
    assert_eq!(n.plugin_id.as_deref(), Some("cl1b-mk2"));
    assert!(n.power && n.enabled);
    assert_eq!(n.device, dev);
}

#[test]
fn helper_store_reads_a_mock_helper_read_only() {
    let root = fixture();
    let dev = device(&root);
    let server = MockHelper::start(root.clone(), 0).unwrap();
    let mut store = HelperStore::new();
    store.poll_interval = Duration::from_millis(100);
    // Even if a caller asks for writes, the helper connection stays read-only.
    store.connect_with(
        server.endpoint(),
        Options {
            allow_writes: true,
            ping_interval: Some(Duration::from_millis(200)),
            ..Options::helper()
        },
    );
    assert!(!store.connection().unwrap().options().allow_writes);
    let drain_until = |store: &mut HelperStore, what: &str, f: &dyn Fn(&HelperStore) -> bool| {
        let deadline = std::time::Instant::now() + T;
        loop {
            store.drain();
            if f(store) {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for {what} ({:?})",
                store.status()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    drain_until(&mut store, "live", &|s| *s.status() == Status::Live);
    assert_eq!(store.info().version.as_deref(), Some("1.3.1.1152"));
    wait_until("subscriptions", T, || {
        server
            .received_commands()
            .iter()
            .any(|c| c == "subscribe /sample_rate/value")
    });
    // A value push lands in the tree.
    let rev = store.revision();
    server.push("/sample_rate", serde_json::json!(96000.0));
    drain_until(&mut store, "push", &|s| {
        s.info().sample_rate == Some(96000.0)
    });
    assert!(store.revision() > rev);
    // A native plug-in appears on a new track: found by the tracks poll.
    server.set_child(
        &format!("/devices/{dev}/tracks"),
        "6b3fa9e8a6f84c06b1f1e9b6f0a1c2d3",
        synthetic_track(&root, "SM7B", "Tube-Tech CL 1B mkII"),
    );
    drain_until(&mut store, "native insert", &|s| {
        !s.native_inserts().is_empty()
    });
    assert_eq!(
        store.native_inserts()[0].plugin_name,
        "Tube-Tech CL 1B mkII"
    );
    // Keep-alive pings, and only read-only verbs on the wire.
    std::thread::sleep(Duration::from_millis(300));
    let cmds = server.received_commands();
    assert!(
        cmds[0].starts_with(r#"{"path":"network_id","data":""#),
        "{}",
        cmds[0]
    );
    assert_eq!(cmds[1], r#"{"path":"client_id","data":"uaconsole"}"#);
    assert!(
        cmds[2].starts_with("get /?func_id=") && cmds[2].ends_with("&recursive=1"),
        "{}",
        cmds[2]
    );
    assert!(cmds.iter().any(|c| c == "get /ping"));
    assert!(
        cmds.iter()
            .all(|c| c.starts_with('{') || c.starts_with("get ") || c.starts_with("subscribe ")),
        "{cmds:?}"
    );
    // Errors (uasrv.*) correlate by func_id.
    match store.connection().unwrap().get("/nonexistent", false) {
        Err(ua_engine::Error::Engine { message, .. }) => {
            assert_eq!(message, "uasrv.invalid_object")
        }
        other => panic!("{other:?}"),
    }
}
