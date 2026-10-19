//! Insert operations on the wire, against MockEngine: assign, remove, Copy / Paste
//! (`/CopyInsert`, `/PasteInsert`), Unison Copy / Paste Settings (`/PluginBarCopySettings`,
//! `/PluginBarPasteSettings`) and drag and drop (`/MoveInsert`, browser rows). Provenance is in
//! `engine-model/src/inserts.rs` and `rack/src/dnd.rs`.

use std::time::{Duration, Instant};

use engine_model::{EngineMixer, UNISON_SLOT};
use houston::Rect;
use rack::dnd::DragPayload;
use rack::input::{Hit, HitKind};
use rack::menu::{MenuCmd, PopupEntry, PopupItem, PopupMenu, insert_menu, preamp_menu};
use rack::model::{InsertState, MixerModel};
use rack::surfaces::{Effect, ViewState};
use rack::{Action, Control, ControlId};
use ua_engine::{MockEngine, MockOptions, Snapshot, Status};

/// The Apollo x4 fixture plus a two-entry plug-in catalogue: a Unison preamp and a
/// non-Unison compressor.
fn tree() -> ua_engine::Tree {
    let p = format!("{}/../ua-engine/tests/fixtures/apollo-x4-12.0.0.snapshot.json", env!("CARGO_MANIFEST_DIR"));
    let mut t = Snapshot::load(p).unwrap().tree;
    let plugin = |name: &str, unison: bool| {
        serde_json::json!({"properties": {
            "Name": {"type": "string", "value": name},
            "Unison": {"type": "bool", "value": unison},
            "Authorized": {"type": "bool", "value": true},
            "Categories": {"type": "string", "value": "Channel Strip & Preamp"},
        }})
    };
    t.insert_node(&serde_json::json!({"children": {"0": plugin("UAD Neve 1073 Preamp", true), "1": plugin("UAD 1176LN Rev E", false)}}), "/plugins");
    t
}

fn live(server: &MockEngine) -> EngineMixer {
    let mut m = EngineMixer::connect(server.endpoint(), true);
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

fn writes_after(server: &MockEngine, n: usize) -> Vec<String> {
    let end = Instant::now() + Duration::from_secs(10);
    while server.write_commands().len() < n {
        assert!(Instant::now() < end, "timed out waiting for {n} writes: {:?}", server.write_commands());
        std::thread::sleep(Duration::from_millis(5));
    }
    server.write_commands()
}

const SLOT0: &str = "/devices/0/inputs/0/effects/0";
const UNISON0: &str = "/devices/0/inputs/0/preamps/0/effects/0";

fn in0(m: &EngineMixer) -> rack::model::StripInfo {
    m.inputs()[0].clone()
}

fn items(m: &PopupMenu) -> impl Iterator<Item = &PopupItem> {
    m.entries.iter().filter_map(|e| match e {
        PopupEntry::Item(i) => Some(i),
        _ => None,
    })
}

fn id(strip: &str, control: Control) -> ControlId {
    ControlId { strip: strip.into(), control }
}

#[test]
fn assign_is_unbracketed_and_remove_is_bracketed() {
    let server = MockEngine::start(tree(), MockOptions::default()).unwrap();
    let mut m = live(&server);
    let s = in0(&m);
    m.set_text(&id(&s.id, Control::Insert(0)), "UAD 1176LN Rev E");
    m.set_text(&id(&s.id, Control::Insert(UNISON_SLOT)), "UAD Neve 1073 Preamp");
    let w = writes_after(&server, 2);
    assert_eq!(w, [format!("set {SLOT0}/EffectName/value \"UAD 1176LN Rev E\""), format!("set {UNISON0}/EffectName/value \"UAD Neve 1073 Preamp\"")]);
    pump_until(&mut m, "loaded", |m| matches!(m.inserts(&in0(m))[0], InsertState::Loaded { .. }));
    m.apply(&Action::Tap(id(&s.id, Control::InsertRemove(0))));
    let w = writes_after(&server, 5);
    assert_eq!(w[2..], ["set /UndoRecording/value true".to_owned(), format!("set {SLOT0}/EffectName/value \"\""), "set /UndoRecording/value false".to_owned()]);
}

#[test]
fn copy_and_paste_are_engine_commands() {
    let server = MockEngine::start(tree(), MockOptions::default()).unwrap();
    server.push(&format!("{SLOT0}/EffectName"), "UAD 1176LN Rev E");
    let mut m = live(&server);
    pump_until(&mut m, "loaded", |m| matches!(m.inserts(&in0(m))[0], InsertState::Loaded { .. }));
    let s = in0(&m);
    // Before a Copy, Paste is disabled.
    let menu = insert_menu(&m, &s.id, 3);
    assert!(items(&menu).any(|i| i.label == "Paste" && !i.enabled), "{menu:?}");
    let mut v = ViewState::default();
    v.dispatch(Action::Tap(id(&s.id, Control::InsertCopy(0))), &mut m);
    let w = writes_after(&server, 1);
    assert_eq!(w, [format!(r#"post /CopyInsert {{"effectPath":"{SLOT0}"}}"#)]);
    // Paste "<name>" into an insert; the Unison slot refuses a non-Unison plug-in.
    let menu = insert_menu(&m, &s.id, 3);
    assert!(items(&menu).any(|i| i.label == "Paste \"1176LN Rev E\"" && i.enabled), "{menu:?}");
    let menu = insert_menu(&m, &s.id, UNISON_SLOT);
    assert!(items(&menu).any(|i| i.label == "Paste" && !i.enabled), "{menu:?}");
    v.dispatch(Action::Tap(id(&s.id, Control::Menu(MenuCmd::InsertPaste(3)))), &mut m);
    v.dispatch(Action::Tap(id(&s.id, Control::Menu(MenuCmd::InsertPaste(UNISON_SLOT)))), &mut m);
    // The COPY modifier on an empty slot pastes too.
    v.dispatch(Action::Tap(id(&s.id, Control::InsertCopy(5))), &mut m);
    let w = writes_after(&server, 3);
    assert_eq!(
        w[1..],
        [
            r#"post /PasteInsert {"effectPaths":["/devices/0/inputs/0/effects/3"]}"#.to_owned(),
            r#"post /PasteInsert {"effectPaths":["/devices/0/inputs/0/effects/5"]}"#.to_owned(),
        ]
    );
}

#[test]
fn unison_copy_and_paste_settings() {
    let server = MockEngine::start(tree(), MockOptions::default()).unwrap();
    server.push(&format!("{UNISON0}/EffectName"), "UAD Neve 1073 Preamp");
    let mut m = live(&server);
    pump_until(&mut m, "unison", |m| matches!(m.unison(&in0(m)), InsertState::Loaded { .. }));
    let s = in0(&m);
    let menu = preamp_menu(&m, &s.id);
    assert!(items(&menu).any(|i| i.label == "Paste Settings" && !i.enabled), "CanPastePluginBarSettings is false: {menu:?}");
    server.push(&format!("{UNISON0}/CanPastePluginBarSettings"), true);
    pump_until(&mut m, "can paste", |m| m.strip_menu_state(&in0(m)).unison_paste_settings);
    let mut v = ViewState::default();
    v.dispatch(Action::Tap(id(&s.id, Control::Menu(MenuCmd::CopySettings(UNISON_SLOT)))), &mut m);
    v.dispatch(Action::Tap(id(&s.id, Control::Menu(MenuCmd::PasteSettings(UNISON_SLOT)))), &mut m);
    let w = writes_after(&server, 2);
    assert_eq!(w, [format!(r#"post /PluginBarCopySettings {{"effectPath":"{UNISON0}"}}"#), format!(r#"post /PluginBarPasteSettings {{"effectPath":"{UNISON0}"}}"#)]);
}

fn hit(strip: &str, control: Control, r: Rect) -> Hit {
    Hit { rect: r, id: id(strip, control), kind: HitKind::Tap }
}

fn apply(fx: Vec<Effect>, m: &mut EngineMixer) {
    for e in fx {
        match e {
            Effect::Model(a) => m.apply(&a),
            Effect::SetText(id, t) => m.set_text(&id, &t),
        }
    }
}

#[test]
fn dragging_an_insert_moves_or_copies_it() {
    let server = MockEngine::start(tree(), MockOptions::default()).unwrap();
    server.push(&format!("{SLOT0}/EffectName"), "UAD 1176LN Rev E");
    let mut m = live(&server);
    pump_until(&mut m, "loaded", |m| matches!(m.inserts(&in0(m))[0], InsertState::Loaded { .. }));
    let s = in0(&m);
    let in1 = m.inputs()[1].id.clone();
    let hits = [hit(&s.id, Control::Insert(0), Rect::new(0.0, 0.0, 80.0, 48.0)), hit(&s.id, Control::Insert(2), Rect::new(0.0, 100.0, 80.0, 48.0)), hit(&in1, Control::Insert(1), Rect::new(100.0, 50.0, 80.0, 48.0))];
    let mut v = ViewState::default();
    // A press that doesn't move is a tap (no drag).
    assert!(v.drag_press(&hits, 10.0, 10.0, &m));
    assert!(!v.drag_motion(&hits, 12.0, 11.0, false, &m));
    assert_eq!(v.drag_release(&m), None);
    // Press, move past the threshold, hover the target, drop: a move.
    assert!(v.drag_press(&hits, 10.0, 10.0, &m));
    assert!(v.drag_motion(&hits, 20.0, 110.0, false, &m));
    let d = v.drag.as_ref().unwrap();
    assert_eq!(d.payload, DragPayload::Insert { strip: s.id.clone(), slot: 0, name: "UAD 1176LN Rev E".into() });
    assert_eq!(d.target.as_ref().map(|t| t.id.control.clone()), Some(Control::Insert(2)));
    let fx = v.drag_release(&m).expect("a drag");
    apply(fx, &mut m);
    // With the copy modifier onto another strip: a copy.
    assert!(v.drag_press(&hits, 10.0, 10.0, &m));
    v.drag_motion(&hits, 120.0, 60.0, true, &m);
    apply(v.drag_release(&m).unwrap(), &mut m);
    // Dropping on itself does nothing.
    assert!(v.drag_press(&hits, 10.0, 10.0, &m));
    v.drag_motion(&hits, 30.0, 30.0, false, &m);
    assert_eq!(v.drag_release(&m), Some(Vec::new()));
    let w = writes_after(&server, 2);
    assert_eq!(
        w,
        [
            format!(r#"post /MoveInsert {{"copy":false,"dstPath":"/devices/0/inputs/0/effects/2","srcPath":"{SLOT0}"}}"#),
            format!(r#"post /MoveInsert {{"copy":true,"dstPath":"/devices/0/inputs/1/effects/1","srcPath":"{SLOT0}"}}"#),
        ]
    );
}

#[test]
fn dragging_a_browser_row_assigns_and_respects_unison() {
    let server = MockEngine::start(tree(), MockOptions::default()).unwrap();
    let mut m = live(&server);
    pump_until(&mut m, "catalogue", |m| m.plugin_catalog().len() == 2);
    let s = in0(&m);
    let hits = [
        hit("browser", Control::BrowserItem(1), Rect::new(0.0, 0.0, 300.0, 40.0)),
        hit(&s.id, Control::Insert(UNISON_SLOT), Rect::new(400.0, 0.0, 80.0, 48.0)),
        hit(&s.id, Control::Insert(4), Rect::new(400.0, 100.0, 80.0, 48.0)),
    ];
    let mut v = ViewState::default();
    // The 1176 is no Unison plug-in: the Unison slot is no target.
    assert!(v.drag_press(&hits, 20.0, 20.0, &m));
    assert!(v.drag_motion(&hits, 420.0, 20.0, false, &m));
    assert!(v.drag.as_ref().unwrap().target.is_none());
    assert!(v.drag_motion(&hits, 420.0, 120.0, false, &m));
    apply(v.drag_release(&m).unwrap(), &mut m);
    let w = writes_after(&server, 1);
    assert_eq!(w, [r#"set /devices/0/inputs/0/effects/4/EffectName/value "UAD 1176LN Rev E""#]);
}

#[test]
fn native_capabilities_gate_catalog_slot_rate_and_occupied_insert() {
    let mut t = tree();
    t.set_value("/NativePluginHosting", true.into());
    t.set_value("/SampleRate", 48000.into());
    let capability = serde_json::json!({"properties": {
        "NativeLoadSupported": {"type": "bool", "value": true},
        "NativeLoadSlots": {"type": "string", "value": SLOT0},
        "NativeSampleRate": {"type": "int", "value": 48000},
        "NativeLoadHint": {"type": "string", "value": "Native DSP: input 1, insert 1; mono, 48 kHz."}
    }});
    for (key, value) in capability["properties"].as_object().unwrap() {
        t.set_value(&format!("/plugins/1/{key}"), ua_engine::Value::from_json(&value["value"]));
    }
    t.set_value("/plugins/0/NativeLoadSupported", false.into());
    t.set_value("/plugins/0/NativeLoadHint", "No native DSP loader for this plug-in.".into());
    let server = MockEngine::start(t, MockOptions::default()).unwrap();
    let mut m = live(&server);
    let s = in0(&m);
    assert_eq!(m.plugin_catalog()[0].native_supported, Some(false));
    assert!(m.plugin_catalog()[0].enabled, "licence status stays separate");
    m.set_text(&id(&s.id, Control::Insert(0)), "UAD Neve 1073 Preamp");
    assert!(m.plugin_notice().unwrap().contains("No native DSP loader"));
    m.set_text(&id(&s.id, Control::Insert(1)), "UAD 1176LN Rev E");
    assert!(m.plugin_notice().unwrap().contains("input 1, insert 1"));
    m.store_mut().apply([("/SampleRate".into(), 44100.into())]);
    m.set_text(&id(&s.id, Control::Insert(0)), "UAD 1176LN Rev E");
    assert!(m.plugin_notice().unwrap().contains("Sample rate"));
    assert!(server.write_commands().is_empty(), "refusals must never reach the writer");
    m.store_mut().apply([("/SampleRate".into(), 48000.into())]);
    m.set_text(&id(&s.id, Control::Insert(0)), "UAD 1176LN Rev E");
    assert!(m.plugin_notice().is_none());
    assert_eq!(writes_after(&server, 1), [format!("set {SLOT0}/EffectName/value \"UAD 1176LN Rev E\"")]);
    m.set_text(&id(&s.id, Control::Insert(0)), "UAD 1176LN Rev E");
    assert!(m.plugin_notice().unwrap().contains("Remove the current native insert"));
    assert_eq!(server.write_commands().len(), 1);
}

fn native_tree(maximum: i32) -> ua_engine::Tree {
    let mut t = tree();
    t.set_value("/NativePluginHosting", true.into());
    t.set_value("/NativeMaxInstances", maximum.into());
    t.set_value("/NativeReplacementSupported", false.into());
    t.set_value("/SampleRate", 48000.into());
    let mono = (0..4).map(|n| format!("/devices/0/inputs/{n}/effects/0")).collect::<Vec<_>>().join(",");
    let stereo = "/devices/0/inputs/0/effects/0,/devices/0/inputs/2/effects/0";
    for p in 0..2 {
        t.set_value(&format!("/plugins/{p}/NativeLoadSupported"), true.into());
        t.set_value(&format!("/plugins/{p}/NativeLoadSlots"), mono.clone().into());
        t.set_value(&format!("/plugins/{p}/NativeMonoSlots"), mono.clone().into());
        t.set_value(&format!("/plugins/{p}/NativeStereoSlots"), stereo.into());
        t.set_value(&format!("/plugins/{p}/NativeSampleRate"), 48000.into());
    }
    for n in 0..4 {
        t.set_value(&format!("/devices/0/inputs/{n}/Stereo"), false.into());
        for effect in 0..4 {
            t.set_value(&format!("/devices/0/inputs/{n}/effects/{effect}/EffectName"), "".into());
        }
    }
    t
}

fn assign_native(m: &mut EngineMixer, input: usize, effect: u8) {
    let target = id(&format!("/devices/0/inputs/{input}"), Control::Insert(effect));
    m.set_text(&target, "UAD 1176LN Rev E");
}

#[test]
fn native_stereo_requires_the_advertised_layout_and_pair_leader() {
    let mut t = native_tree(4);
    for n in 2..4 { t.set_value(&format!("/devices/0/inputs/{n}/Stereo"), true.into()); }
    t.set_value("/plugins/1/NativeStereoSlots", "".into());
    let server = MockEngine::start(t, MockOptions::default()).unwrap();
    let mut m = live(&server);
    assign_native(&mut m, 2, 0);
    assert!(m.plugin_notice().unwrap().contains("No native stereo loader"));
    // Linked followers are absent from the assignable topology.
    assert!(m.insert_slot(&id("/devices/0/inputs/3", Control::Insert(0))).is_none());
    assign_native(&mut m, 3, 0);
    assert!(server.write_commands().is_empty());
    m.store_mut().apply([("/plugins/1/NativeStereoSlots".into(), "/devices/0/inputs/2/effects/0".into())]);
    assign_native(&mut m, 2, 0);
    assert!(m.plugin_notice().is_none());
    assert_eq!(writes_after(&server, 1), ["set /devices/0/inputs/2/effects/0/EffectName/value \"UAD 1176LN Rev E\""]);
}

#[test]
fn native_mono_does_not_use_a_stereo_only_capability() {
    let mut t = native_tree(4);
    t.set_value("/plugins/1/NativeMonoSlots", "".into());
    let server = MockEngine::start(t, MockOptions::default()).unwrap();
    let mut m = live(&server);
    assign_native(&mut m, 0, 0);
    assert!(m.plugin_notice().unwrap().contains("No native mono loader"));
    assert!(server.write_commands().is_empty());
}

#[test]
fn native_capacity_counts_a_stereo_pair_once_and_deduplicates_catalogue_slots() {
    let mut t = native_tree(2);
    for n in 2..4 { t.set_value(&format!("/devices/0/inputs/{n}/Stereo"), true.into()); }
    t.set_value("/devices/0/inputs/2/effects/0/EffectName", "UAD Neve 1073 Preamp".into());
    let server = MockEngine::start(t, MockOptions::default()).unwrap();
    let mut m = live(&server);
    // The existing stereo lease occupies two channels but consumes one instance.
    assign_native(&mut m, 0, 0);
    assert!(m.plugin_notice().is_none());
    writes_after(&server, 1);
    assign_native(&mut m, 1, 0);
    assert!(m.plugin_notice().unwrap().contains("2 simultaneous inserts"));
    assert_eq!(server.write_commands().len(), 1);
}

#[test]
fn native_occupancy_and_replacement_are_independent_of_instance_capacity() {
    let mut t = native_tree(4);
    t.set_value(&format!("{SLOT0}/EffectName"), "UAD Neve 1073 Preamp".into());
    // A second advertised slot still cannot own an already occupied channel.
    let slots = format!("{SLOT0},/devices/0/inputs/0/effects/1");
    t.set_value("/plugins/1/NativeMonoSlots", slots.into());
    let server = MockEngine::start(t, MockOptions::default()).unwrap();
    let mut m = live(&server);
    assign_native(&mut m, 0, 1);
    assert!(m.plugin_notice().unwrap().contains("already uses this input"), "{:?}", m.plugin_notice());
    assign_native(&mut m, 0, 0);
    assert!(m.plugin_notice().unwrap().contains("Remove the current native insert"));
    assert!(server.write_commands().is_empty());
    m.store_mut().apply([("/NativeReplacementSupported".into(), true.into()), ("/NativeMaxInstances".into(), 1.into())]);
    assign_native(&mut m, 0, 0);
    assert!(m.plugin_notice().is_none(), "replacement does not consume another lease");
    assert_eq!(writes_after(&server, 1), [format!("set {SLOT0}/EffectName/value \"UAD 1176LN Rev E\"")]);
}

#[test]
fn native_stereo_refuses_a_loaded_member_even_when_capacity_is_available() {
    let mut t = native_tree(4);
    for n in 2..4 { t.set_value(&format!("/devices/0/inputs/{n}/Stereo"), true.into()); }
    // A stale occupied member must not be hidden by the linked leader selection.
    t.set_value("/devices/0/inputs/3/effects/0/EffectName", "UAD Neve 1073 Preamp".into());
    let server = MockEngine::start(t, MockOptions::default()).unwrap();
    let mut m = live(&server);
    assign_native(&mut m, 2, 0);
    assert!(m.plugin_notice().unwrap().contains("already uses this input or linked pair"));
    assert!(server.write_commands().is_empty());
}

fn native_chain_tree(maximum_chains: i32, maximum_inserts: i32) -> ua_engine::Tree {
    let mut t = native_tree(maximum_chains);
    t.set_value("/NativeChainSupported", true.into());
    t.set_value("/NativeMaxInserts", maximum_inserts.into());
    // Advertise all four slots; the separate maximum must still be honored.
    let mono = (0..4).flat_map(|n| (0..4).map(move |e| format!("/devices/0/inputs/{n}/effects/{e}"))).collect::<Vec<_>>().join(",");
    let stereo = [0, 2].into_iter().flat_map(|n| (0..4).map(move |e| format!("/devices/0/inputs/{n}/effects/{e}"))).collect::<Vec<_>>().join(",");
    for p in 0..2 {
        t.set_value(&format!("/plugins/{p}/NativeLoadSlots"), mono.clone().into());
        t.set_value(&format!("/plugins/{p}/NativeMonoSlots"), mono.clone().into());
        t.set_value(&format!("/plugins/{p}/NativeStereoSlots"), stereo.clone().into());
    }
    t
}

#[test]
fn native_chain_accepts_four_logical_inserts_with_one_physical_instance() {
    let server = MockEngine::start(native_chain_tree(1, 4), MockOptions::default()).unwrap();
    let mut m = live(&server);
    for effect in 0..4 {
        assign_native(&mut m, 0, effect);
        assert!(m.plugin_notice().is_none(), "{:?}", m.plugin_notice());
        writes_after(&server, usize::from(effect) + 1);
    }
    assign_native(&mut m, 1, 0);
    assert!(m.plugin_notice().unwrap().contains("1 simultaneous input chain"));
    assert_eq!(server.write_commands().len(), 4);
}

#[test]
fn native_chain_requires_both_capabilities_and_honors_the_slot_limit() {
    let mut t = native_chain_tree(4, 4);
    t.set_value("/NativeChainSupported", false.into());
    let server = MockEngine::start(t, MockOptions::default()).unwrap();
    let mut m = live(&server);
    assign_native(&mut m, 0, 1);
    assert!(m.plugin_notice().unwrap().contains("1 insert per input chain"));
    m.store_mut().apply([("/NativeChainSupported".into(), true.into()), ("/NativeMaxInserts".into(), ua_engine::Value::Null)]);
    assign_native(&mut m, 0, 1);
    assert!(m.plugin_notice().unwrap().contains("1 insert per input chain"));
    m.store_mut().apply([("/NativeMaxInserts".into(), 2.into())]);
    assign_native(&mut m, 0, 2);
    assert!(m.plugin_notice().unwrap().contains("2 inserts per input chain"));
    assert!(server.write_commands().is_empty());
    assign_native(&mut m, 0, 1);
    assert!(m.plugin_notice().is_none(), "slot gaps remain valid");
    assert_eq!(writes_after(&server, 1), ["set /devices/0/inputs/0/effects/1/EffectName/value \"UAD 1176LN Rev E\""]);
}

#[test]
fn native_stereo_chain_counts_as_one_and_still_refuses_member_overlap() {
    let mut t = native_chain_tree(2, 4);
    for n in 2..4 { t.set_value(&format!("/devices/0/inputs/{n}/Stereo"), true.into()); }
    for effect in 0..3 { t.set_value(&format!("/devices/0/inputs/2/effects/{effect}/EffectName"), "UAD Neve 1073 Preamp".into()); }
    let server = MockEngine::start(t, MockOptions::default()).unwrap();
    let mut m = live(&server);
    assign_native(&mut m, 0, 0);
    assert!(m.plugin_notice().is_none());
    writes_after(&server, 1);
    assign_native(&mut m, 2, 3);
    assert!(m.plugin_notice().is_none(), "adding within a chain needs no additional physical instance");
    writes_after(&server, 2);
    assign_native(&mut m, 1, 0);
    assert!(m.plugin_notice().unwrap().contains("2 simultaneous input chains"));
    m.store_mut().apply([("/devices/0/inputs/3/effects/0/EffectName".into(), "UAD Neve 1073 Preamp".into()),
                         ("/NativeReplacementSupported".into(), true.into())]);
    assign_native(&mut m, 2, 3);
    assert!(m.plugin_notice().unwrap().contains("already uses this input or linked pair"));
    assert_eq!(server.write_commands().len(), 2);
}

#[test]
fn native_chain_replacement_keeps_the_explicit_replacement_gate() {
    let mut t = native_chain_tree(1, 4);
    for effect in 0..4 { t.set_value(&format!("/devices/0/inputs/0/effects/{effect}/EffectName"), "UAD Neve 1073 Preamp".into()); }
    let server = MockEngine::start(t, MockOptions::default()).unwrap();
    let mut m = live(&server);
    assign_native(&mut m, 0, 2);
    assert!(m.plugin_notice().unwrap().contains("Remove the current native insert"));
    assert!(server.write_commands().is_empty());
    m.store_mut().apply([("/NativeReplacementSupported".into(), true.into())]);
    assign_native(&mut m, 0, 2);
    assert!(m.plugin_notice().is_none());
    assert_eq!(writes_after(&server, 1), ["set /devices/0/inputs/0/effects/2/EffectName/value \"UAD 1176LN Rev E\""]);
}

#[test]
fn native_unison_and_inserts_share_one_physical_input_chain() {
    let mut t = native_chain_tree(1, 4);
    for field in ["NativeLoadSlots", "NativeMonoSlots", "NativeStereoSlots"] {
        t.set_value(&format!("/plugins/0/{field}"), UNISON0.into());
    }
    t.set_value(&format!("{UNISON0}/EffectName"), "UAD Neve 1073 Preamp".into());
    let server = MockEngine::start(t, MockOptions::default()).unwrap();
    let mut m = live(&server);
    assign_native(&mut m, 0, 0);
    assert!(m.plugin_notice().is_none(), "{:?}", m.plugin_notice());
    assert_eq!(writes_after(&server, 1), [format!("set {SLOT0}/EffectName/value \"UAD 1176LN Rev E\"")]);
    assign_native(&mut m, 1, 0);
    assert!(m.plugin_notice().unwrap().contains("1 simultaneous input chain"));
    assert_eq!(server.write_commands().len(), 1);
}

#[test]
fn native_unison_uses_input_stereo_state_and_has_no_follower_slot() {
    let mut t = native_chain_tree(1, 4);
    let follower = "/devices/0/inputs/1/preamps/0/effects/0";
    t.set_value("/plugins/0/NativeMonoSlots", "".into());
    t.set_value("/plugins/0/NativeStereoSlots", format!("{UNISON0},{follower}").into());
    for n in 0..2 { t.set_value(&format!("/devices/0/inputs/{n}/Stereo"), true.into()); }
    let server = MockEngine::start(t, MockOptions::default()).unwrap();
    let mut m = live(&server);
    m.set_text(&id("/devices/0/inputs/0", Control::Insert(UNISON_SLOT)), "UAD Neve 1073 Preamp");
    assert!(m.plugin_notice().is_none(), "{:?}", m.plugin_notice());
    assert_eq!(writes_after(&server, 1), [format!("set {UNISON0}/EffectName/value \"UAD Neve 1073 Preamp\"")]);
    m.set_text(&id("/devices/0/inputs/1", Control::Insert(UNISON_SLOT)), "UAD Neve 1073 Preamp");
    assert_eq!(server.write_commands().len(), 1);
}
