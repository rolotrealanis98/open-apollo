mod common;

use ua_engine::{Json, Message, Property, Snapshot, Tree, Value};

fn message(text: &str) -> Option<Message> {
    Message::decode(text.as_bytes())
}

fn json(text: &str) -> Json {
    serde_json::from_str(text).unwrap()
}

#[test]
fn decodes_value_kinds() {
    // The real engine writes `"key": value` with a space.
    assert_eq!(
        message(r#"{"path": "/devices/0/inputs/0/Mute/value", "data": true}"#)
            .unwrap()
            .value(),
        Some(Value::Bool(true))
    );
    assert_eq!(
        message(r#"{"path": "/x/FaderLevel/value", "data": -0.009999999776482582}"#)
            .unwrap()
            .value(),
        Some(Value::Number(-0.009999999776482582))
    );
    assert_eq!(
        message(r#"{"path": "/x/DimAttenuation/value", "data": 17}"#)
            .unwrap()
            .value(),
        Some(Value::Number(17.0))
    );
    assert_eq!(
        message(r#"{"path": "/x/Name/value", "data": "SM7B"}"#)
            .unwrap()
            .value(),
        Some(Value::String("SM7B".into()))
    );
    assert_eq!(
        message(r#"{"path": "/x/IOType/value", "data": null}"#)
            .unwrap()
            .value(),
        Some(Value::Null)
    );
    assert_eq!(
        message(r#"{"path": "/x/values", "data": ["Line", "Mic"]}"#)
            .unwrap()
            .value(),
        Some(Value::Json(json(r#"["Line","Mic"]"#)))
    );
    // Booleans are not numbers and numbers are not booleans.
    assert_eq!(
        message(r#"{"path": "/a/value", "data": 1}"#)
            .unwrap()
            .value(),
        Some(Value::Number(1.0))
    );
    assert_ne!(
        message(r#"{"path": "/a/value", "data": 0}"#)
            .unwrap()
            .value(),
        Some(Value::Bool(false))
    );
}

#[test]
fn decodes_envelope_fields() {
    let m = message(r#"{"path": "/devices/0/inputs/0/FaderLevel/value", "parameters": {"func_id": 9}, "data": -0.01}"#).unwrap();
    assert_eq!(m.func_id(), Some(9));
    // The open-apollo daemon echoes query parameters as strings.
    assert_eq!(
        message(r#"{"path": "/", "parameters": {"func_id": "12"}, "data": {}}"#)
            .unwrap()
            .func_id(),
        Some(12)
    );
    let err = message(r#"{"path": "/devices/0/inputs/0/Bogus/value", "error": "Unable to resolve path for get."}"#).unwrap();
    assert_eq!(
        err.error.as_deref(),
        Some("Unable to resolve path for get.")
    );
    assert!(err.data.is_none());
    assert!(!err.is_value_push());
    assert!(message("set /a/value 1").is_none()); // non-JSON frames are skipped
    assert!(message("  {\"url\": \"/x\", \"data\": 1}").is_some()); // `url` is accepted too
}

/// After `subscribe mode=multi` the engine wraps pushes (verified on engine 12.0.0).
#[test]
fn flattens_multi_envelopes() {
    let text = r#"{"path": "multi", "data": [{"path": "/devices/0/inputs/1/Mute/value", "data": true}, {"path": "/MeterPulse/value", "data": 1381672}]}"#;
    let m = message(text).unwrap();
    assert!(m.is_multi());
    let inner = m.flattened();
    assert_eq!(inner.len(), 2);
    assert_eq!(inner[0].path, "/devices/0/inputs/1/Mute/value");
    assert_eq!(inner[0].value(), Some(Value::Bool(true)));
    assert_eq!(inner[1].value(), Some(Value::Number(1381672.0)));
    let single = message(r#"{"path": "/a/value", "data": 1}"#).unwrap();
    assert_eq!(single.clone().flattened(), vec![single]);
}

#[test]
fn value_accessors() {
    assert_eq!(Value::Number(0.5).as_f64(), Some(0.5));
    assert_eq!(Value::Number(2.0).as_i64(), Some(2));
    assert_eq!(Value::Bool(true).as_f64(), Some(1.0));
    assert_eq!(Value::Number(0.0).as_bool(), Some(false));
    assert_eq!(Value::String("x".into()).as_f64(), None);
    assert_eq!(Value::Number(f64::NAN).as_i64(), None);
    assert_eq!(Value::Number(0.7818182).wire_text(false), "0.7818182");
    assert_eq!(Value::Number(-144.0).wire_text(false), "-144");
    assert_eq!(Value::Bool(false).wire_text(false), "false");
    assert_eq!(Value::String("cue1".into()).wire_text(false), "\"cue1\"");
    assert_eq!(Value::Number(48000.0).to_json(), json("48000"));
    assert_eq!(Value::from(json("[1, 2]")), Value::Json(json("[1,2]")));
}

#[test]
fn parses_property_metadata() {
    let p = Property::from_json(&json(
        r#"{"type": "float", "min": -144.0, "max": 12.0, "default": 0.0, "value": -0.01}"#,
    ));
    assert_eq!(p.meta.ty.as_deref(), Some("float"));
    assert_eq!((p.meta.min, p.meta.max), (Some(-144.0), Some(12.0)));
    assert_eq!(p.meta.default, Some(Value::Number(0.0)));
    assert_eq!(p.value, Some(Value::Number(-0.01)));
    assert!(!p.meta.readonly);
    assert_eq!(p.meta.normalized(-66.0), Some(0.5));
    let io = Property::from_json(&json(
        r#"{"type": "string", "values": ["Line", "Mic", "Dante"], "enabled": true, "value": "Line"}"#,
    ));
    assert_eq!(
        io.meta.choices(),
        vec![
            Value::from("Line"),
            Value::from("Mic"),
            Value::from("Dante")
        ]
    );
    assert_eq!(io.meta.enabled, Some(true));
    let sr = Property::from_json(&json(
        r#"{"type": "int", "values": [{"value": 44100, "string": "44.1 kHz", "enabled": true}], "value": 48000}"#,
    ));
    assert_eq!(sr.meta.choices(), vec![Value::Number(44100.0)]);
    assert!(sr.meta.is_integral());
    let ro = Property::from_json(&json(
        r#"{"type": "pointer", "readonly": true, "value": 500608159232}"#,
    ));
    assert!(ro.meta.readonly);
    // Unknown keys survive a round trip.
    let preset = Property::from_json(&json(
        r#"{"plugin": "Channel Strip", "type": "string", "value": ""}"#,
    ));
    assert_eq!(
        preset.extra.get("plugin"),
        Some(&json(r#""Channel Strip""#))
    );
    assert_eq!(Property::from_json(&preset.to_json()), preset);
}

#[test]
fn tree_from_engine_node() {
    let j = json(
        r#"{"properties": {"Type": {"type": "string", "value": "root"}, "TalkbackOn": {"type": "bool", "value": false}},
         "commands": {"ClearSolos": {}},
         "children": {"devices": {"properties": {"Type": {"type": "string", "value": "container"}},
            "children": {"0": {"properties": {"DeviceName": {"type": "string", "value": "Apollo x4"}},
               "children": {"inputs": {"children": {"10": {}, "2": {"properties": {"Mute": {"type": "bool", "value": true}}}}}}}}}}}"#,
    );
    let mut tree = Tree::from_node(&j);
    assert_eq!(tree.value("/TalkbackOn"), Some(&Value::Bool(false)));
    assert_eq!(
        tree.value("/devices/0/DeviceName/value"),
        Some(&Value::from("Apollo x4"))
    );
    assert_eq!(tree.children("/devices/0/inputs"), ["2", "10"]); // numeric order
    assert_eq!(tree.stub_paths(), vec!["/devices/0/inputs/10"]);
    assert!(
        tree.node("/")
            .unwrap()
            .commands
            .as_ref()
            .unwrap()
            .get("ClearSolos")
            .is_some()
    );
    assert!(tree.set_value("/devices/0/inputs/2/Mute/value", Value::Bool(false)));
    assert!(!tree.set_value("/devices/0/inputs/2/Mute", Value::Bool(false)));
    // Unknown paths are added (hidden `inputs/N/Gain` pushes).
    assert!(tree.set_value("/devices/0/inputs/2/Gain", Value::Number(10.0)));
    assert!(
        tree.node("/devices/0/inputs/2")
            .unwrap()
            .properties
            .contains(&"Gain".to_owned())
    );
    // Filling a stub (level walk).
    tree.insert_node(
        &json(r#"{"properties": {"Mute": {"type": "bool", "value": false}}}"#),
        "/devices/0/inputs/10",
    );
    assert!(tree.stub_paths().is_empty());
    assert_eq!(
        tree.value("/devices/0/inputs/10/Mute"),
        Some(&Value::Bool(false))
    );
    assert_eq!(tree.children("/devices/0/inputs"), ["2", "10"]);
    // Round trip through the nested form.
    let again = Tree::from_node(&tree.node_json("/"));
    assert_eq!(again, tree);
}

#[test]
fn tree_from_flat_device_map() {
    let j = json(
        r#"{"device_name": "Apollo x4", "host": "127.0.0.1", "controls": [
          {"path": "/Type", "type": "string", "min": null, "max": null, "values": null, "value": "root"},
          {"path": "/devices/0/inputs/0/Mute", "type": "bool", "min": null, "max": null, "values": null, "value": false},
          {"path": "/devices/0/inputs/0/FaderLevel", "type": "float", "min": -144.0, "max": 12.0, "values": null, "value": 0.0}]}"#,
    );
    let snap = Snapshot::from_json(&j).unwrap();
    assert_eq!(
        snap.tree.value("/devices/0/inputs/0/Mute"),
        Some(&Value::Bool(false))
    );
    assert_eq!(
        snap.tree
            .meta("/devices/0/inputs/0/FaderLevel")
            .unwrap()
            .min,
        Some(-144.0)
    );
    assert_eq!(
        snap.tree
            .meta("/devices/0/inputs/0/FaderLevel")
            .unwrap()
            .values,
        None
    );
    assert_eq!(snap.tree.children("/devices/0"), ["inputs"]);
    assert_eq!(snap.info.device.as_deref(), Some("Apollo x4"));
    assert_eq!(
        snap.info.source.as_deref(),
        Some("open-apollo device map 127.0.0.1")
    );
}

/// The formats a snapshot can be loaded from.
#[test]
fn snapshot_formats() {
    let node = r#"{"properties": {"Version": {"type": "string", "value": "12.0.0"}}}"#;
    let reply = format!(
        r#"{{"path": "/", "parameters": {{"func_id": 1, "recursive": 1}}, "data": {node}}}"#
    );
    let s = Snapshot::from_json(&json(&reply)).unwrap();
    assert_eq!(s.info.source.as_deref(), Some("engine reply"));
    assert_eq!(s.info.engine_version.as_deref(), Some("12.0.0"));
    assert_eq!(
        Snapshot::from_json(&json(node))
            .unwrap()
            .info
            .source
            .as_deref(),
        Some("engine node")
    );
    let capture = format!(r#"{{"device_name": "Apollo x8p", "controls": {node}}}"#);
    assert_eq!(
        Snapshot::from_json(&json(&capture))
            .unwrap()
            .info
            .source
            .as_deref(),
        Some("dump_x8p_tree capture")
    );
    assert!(Snapshot::from_json(&json(r#"{"nothing": 1}"#)).is_err());
}

#[test]
fn snapshot_round_trip() {
    let snap = Snapshot::load(common::manifest(
        "tests/fixtures/apollo-x4-12.0.0.snapshot.json",
    ))
    .unwrap();
    assert_eq!(snap.info.device.as_deref(), Some("Apollo x4"));
    assert_eq!(snap.info.engine_version.as_deref(), Some("12.0.0"));
    assert!(snap.tree.property_count() > 7000); // catalogue-trimmed (12,238 untrimmed)
    let again = Snapshot::from_slice(&snap.to_vec(false)).unwrap();
    assert_eq!(again.tree, snap.tree);
    assert_eq!(again.info, snap.info);
    // The 12.0.0 root carries the commands catalogue.
    assert!(
        snap.tree
            .node("/")
            .unwrap()
            .commands
            .as_ref()
            .unwrap()
            .get("ClearClips")
            .is_some()
    );
}

#[test]
fn trimming_drops_catalogues() {
    let x4 = common::device_map("x4");
    assert!(!x4.children("/pluginscenes").is_empty());
    assert!(!x4.children("/inputroutes").is_empty());
    let t = x4.trimmed(true);
    assert!(t.children("/pluginscenes").is_empty());
    assert!(t.children("/inputroutes").is_empty());
    assert!(t.children("/devices/0/asioinputs").is_empty());
    assert!(t.property_count() < x4.property_count());
    assert_eq!(
        t.value("/devices/0/inputs/0/Mute"),
        x4.value("/devices/0/inputs/0/Mute")
    );
}

/// A trimmed (known-empty) node survives a round trip without turning into an unfetched stub.
#[test]
fn empty_nodes_are_not_stubs_after_a_round_trip() {
    let t = common::device_map("x4").trimmed(true);
    assert!(t.stub_paths().is_empty());
    let again = Tree::from_node(&t.node_json("/"));
    assert!(again.stub_paths().is_empty(), "{:?}", again.stub_paths());
    assert!(again.has_node("/pluginscenes") && again.has_node("/inputroutes"));
}
