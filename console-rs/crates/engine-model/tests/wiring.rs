//! Console 1.3.1 commands behind menus and panels, against MockEngine (the capturing engine):
//! plug-in scenes, meter CLEAR and TAP tempo. Provenance is on the implementations.

use std::time::{Duration, Instant};

use engine_model::EngineMixer;
use rack::menu::{MenuCmd, SceneOp};
use rack::model::MixerModel;
use rack::surfaces::{Popover, TextTarget};
use rack::{Action, Control, ControlId, ViewState};
use ua_engine::{MockEngine, MockOptions, Snapshot, Status};

/// The Apollo x4 fixture plus engine 12's plug-in scenes node (four of its 128 scenes, all
/// "EMPTY SCENE", none recalled) — the fixture capture has an empty `pluginscenes`.
fn tree() -> ua_engine::Tree {
    let p = format!("{}/../ua-engine/tests/fixtures/apollo-x4-12.0.0.snapshot.json", env!("CARGO_MANIFEST_DIR"));
    let mut t = Snapshot::load(p).unwrap().tree;
    let scene = |i: i64| {
        serde_json::json!({"properties": {
            "SceneIndex": {"type": "int", "min": 0, "max": 127, "value": i},
            "SceneName": {"type": "string", "value": "EMPTY SCENE"},
            "SceneTempo": {"type": "float", "min": 0, "max": 999, "value": 120},
        }})
    };
    t.insert_node(
        &serde_json::json!({
            "properties": {
                "RecalledSceneIndex": {"type": "int", "min": -1, "max": 127, "value": -1},
                "MaxPluginScenes": {"type": "int", "min": 128, "max": 128, "readonly": true, "value": 128},
                "EmptySceneName": {"type": "string", "readonly": true, "value": "EMPTY SCENE"},
            },
            "children": {"scenes": {"children": {"0": scene(0), "1": scene(1), "2": scene(2), "3": scene(3)}}},
        }),
        "/pluginscenes/0",
    );
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

fn global(control: Control) -> ControlId {
    ControlId { strip: "global".into(), control }
}

#[test]
fn scene_operations_post_console_keys() {
    let server = MockEngine::start(tree(), MockOptions::default()).unwrap();
    server.push("/pluginscenes/0/scenes/2/SceneName", "VERSE");
    let mut m = live(&server);
    pump_until(&mut m, "scene name", |m| m.plugin_scenes().get(2).map(String::as_str) == Some("VERSE"));
    let mut v = ViewState::default();
    // Save Scene and Recall Scene go straight out; the panel's SAVE / RECALL use the same.
    v.dispatch(Action::Tap(global(Control::SceneCommand(SceneOp::Save, 2))), &mut m);
    v.dispatch(Action::Tap(global(Control::SceneCommand(SceneOp::Recall, 2))), &mut m);
    // Overwrite asks first ("Overwrite Scene"); OK sends it.
    v.dispatch(Action::Tap(global(Control::Menu(MenuCmd::Scene(SceneOp::Overwrite, 2)))), &mut m);
    let Some(Popover::Confirm { title, message, ok }) = v.popover.as_ref().map(|p| p.kind.clone()) else { panic!("no confirmation: {:?}", v.popover) };
    assert_eq!(title, "Overwrite Scene");
    assert_eq!(message, "Are you sure you want to overwrite \"VERSE\"?");
    v.dispatch(Action::Tap(ok), &mut m);
    assert!(v.popover.is_none());
    // Clear: CANCEL sends nothing; OK sends ClearScene.
    v.dispatch(Action::Tap(global(Control::Menu(MenuCmd::Scene(SceneOp::Clear, 2)))), &mut m);
    v.dispatch(Action::Tap(ControlId { strip: "popover".into(), control: Control::PopoverDismiss }), &mut m);
    v.dispatch(Action::Tap(global(Control::Menu(MenuCmd::Scene(SceneOp::Clear, 2)))), &mut m);
    let Some(Popover::Confirm { ok, .. }) = v.popover.as_ref().map(|p| p.kind.clone()) else { panic!() };
    v.dispatch(Action::Tap(ok), &mut m);
    // Rename edits the row; Enter sets the upper-cased name.
    v.dispatch(Action::Tap(global(Control::Menu(MenuCmd::Scene(SceneOp::Rename, 2)))), &mut m);
    assert_eq!(v.text_edit.as_ref().map(|e| (&e.target, e.text.as_str())), Some((&TextTarget::SceneName(2), "VERSE")));
    for _ in 0..5 {
        v.key_backspace();
    }
    v.key_text("Chorus");
    v.key_enter(&mut m);
    let w = writes_after(&server, 7);
    let posts: Vec<&str> = w.iter().filter(|c| c.starts_with("post ")).map(String::as_str).collect();
    assert_eq!(
        posts,
        [
            r#"post /SceneOperation {"StoreScene":2}"#,
            r#"post /SceneOperation {"RecallScene":2}"#,
            r#"post /SceneOperation {"OverwriteScene":2}"#,
            r#"post /SceneOperation {"ClearScene":2}"#,
        ]
    );
    assert!(w.contains(&r#"set /pluginscenes/0/scenes/2/SceneName/value "CHORUS""#.to_owned()), "{w:?}");
}

#[test]
fn scene_row_menu_enables_like_console() {
    let server = MockEngine::start(tree(), MockOptions::default()).unwrap();
    server.push("/pluginscenes/0/scenes/1/SceneName", "INTRO");
    server.push("/pluginscenes/0/RecalledSceneIndex", 1);
    let mut m = live(&server);
    pump_until(&mut m, "recalled", |m| m.menu_state().recalled_scene == Some(1));
    let enabled = |m: &EngineMixer, i: u16| -> Vec<(String, bool)> {
        rack::menu::scene_row_menu(m, i).items().iter().map(|it| (it.label.clone(), it.enabled)).collect()
    };
    let e = |l: &str, on: bool| (l.to_owned(), on);
    // An empty scene: only Save Scene.
    assert_eq!(enabled(&m, 0), [e("Rename", false), e("Overwrite", false), e("Clear", false), e("Save Scene", true), e("Recall Scene", false)]);
    // The recalled scene: everything but Overwrite.
    assert_eq!(enabled(&m, 1), [e("Rename", true), e("Overwrite", false), e("Clear", true), e("Save Scene", true), e("Recall Scene", true)]);
}

#[test]
fn meter_clear_posts_clear_clips() {
    let server = MockEngine::start(tree(), MockOptions::default()).unwrap();
    let mut m = live(&server);
    let strip = m.inputs()[0].id.clone();
    let mut v = ViewState::default();
    v.dispatch(Action::Tap(ControlId { strip: strip.clone(), control: Control::Menu(MenuCmd::ClearMeter) }), &mut m);
    v.dispatch(Action::Tap(ControlId { strip, control: Control::Menu(MenuCmd::ClearAllMeters) }), &mut m);
    let w = writes_after(&server, 2);
    assert_eq!(w, [r#"post /ClearClips {"path":"/devices/0/inputs/0"}"#, "post /ClearClips {}"]);
}

#[test]
fn tap_tempo_sets_the_tempo_from_four_taps() {
    let server = MockEngine::start(tree(), MockOptions::default()).unwrap();
    let mut m = live(&server);
    let mut v = ViewState::default();
    // The status bar's TEMPO field opens the window with the current tempo in its field.
    v.dispatch(Action::Tap(global(Control::StatusTempo)), &mut m);
    assert_eq!(v.text_edit.as_ref().map(|e| e.text.as_str()), Some("120.00"));
    let tap = ControlId { strip: "popover".into(), control: Control::TempoTap };
    for _ in 0..4 {
        v.dispatch(Action::Tap(tap.clone()), &mut m);
        std::thread::sleep(Duration::from_millis(250));
    }
    let w = writes_after(&server, 3); // UndoRecording true, Tempo, UndoRecording false
    let bpm: f64 = w.iter().find_map(|c| c.strip_prefix("set /Tempo/value ")).unwrap_or_else(|| panic!("{w:?}")).parse().unwrap();
    // 250 ms apart (plus scheduling) is about 240 BPM.
    assert!((200.0..=245.0).contains(&bpm), "{bpm}");
    let shown: f64 = v.text_edit.as_ref().unwrap().text.parse().unwrap();
    assert!((shown - bpm).abs() < 0.006, "{shown} vs {bpm}");
}
