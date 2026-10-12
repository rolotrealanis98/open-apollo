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
