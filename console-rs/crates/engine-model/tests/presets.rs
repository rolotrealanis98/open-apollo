//! Channel-strip and plug-in presets as Console 1.3.1 sends them (UA Mixer Engine 12.0.0:
//! `CMxBase::SetChannelStripPreset`, `CMxEffect::setPreset`, `CMxSaveAsPreset`,
//! `CMxMovePreset`, `CMxDeletePreset`, `PluginBar{Copy,Paste}Settings`), against MockEngine.

use std::time::{Duration, Instant};

use engine_model::EngineMixer;
use rack::menu::MenuCmd;
use rack::model::MixerModel;
use rack::presets::{PresetCmd, PresetScope};
use rack::{Action, Control, ControlId, ViewState};
use ua_engine::{MockEngine, MockOptions, Property, Snapshot, Status};

const IN0: &str = "/devices/0/inputs/0";

/// The Apollo x4 fixture with a Channel Strip preset tree on every input (factory folder
/// "Bass", a user folder "Mine" and a user file at the top) and the UA 1176 Rev A on input 0's
/// first insert with two plug-in presets.
fn tree() -> ua_engine::Tree {
    let p = format!("{}/../ua-engine/tests/fixtures/apollo-x4-12.0.0.snapshot.json", env!("CARGO_MANIFEST_DIR"));
    let mut t = Snapshot::load(p).unwrap().tree;
    let channel = serde_json::json!({"plugin": "Channel Strip", "type": "string", "value": "", "values": [
        {"type": "folder", "value": "Bass", "locked": true, "children": [
            {"type": "file", "value": "Factory A - Bass", "locked": true},
            {"type": "file", "value": "Factory B - Bass", "locked": true}]},
        {"type": "folder", "value": "Mine", "children": [{"type": "file", "value": "Vox Chain"}]},
        {"type": "file", "value": "Loose"},
    ]});
    for k in 0..4 {
        t.set_property(&format!("/devices/0/inputs/{k}/Preset"), Property::from_json(&channel));
    }
    t.set_property(&format!("{IN0}/effects/0/EffectName"), Property::from_json(&serde_json::json!({"type": "string", "value": "UAD UA 1176 Rev A"})));
    t.set_property(
        &format!("{IN0}/effects/0/Preset"),
        Property::from_json(&serde_json::json!({"type": "string", "value": "", "values": [
            {"type": "file", "value": "Fast Attack", "locked": true},
            {"type": "folder", "value": "Vocals", "children": [{"type": "file", "value": "Smooth"}]},
        ]})),
    );
    t
}

fn live(server: &MockEngine) -> EngineMixer {
    let mut m = EngineMixer::connect(server.endpoint(), true);
    let end = Instant::now() + Duration::from_secs(10);
    while *m.store().status() != Status::Live {
        assert!(Instant::now() < end, "timed out connecting");
        m.pump();
        std::thread::sleep(Duration::from_millis(5));
    }
    m
}

/// Write frames containing `needle`, waiting until `n` have arrived.
fn writes_with(server: &MockEngine, needle: &str, n: usize) -> Vec<String> {
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        let w: Vec<String> = server.write_commands().into_iter().filter(|c| c.contains(needle)).collect();
        if w.len() >= n {
            return w;
        }
        assert!(Instant::now() < end, "timed out waiting for {n} × {needle}: {:?}", server.write_commands());
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn strip(control: Control) -> ControlId {
    ControlId { strip: IN0.into(), control }
}

#[test]
fn channel_presets_list_load_and_save() {
    let server = MockEngine::start(tree(), MockOptions::default()).unwrap();
    let mut m = live(&server);
    let list = m.channel_presets();
    assert_eq!(list.iter().map(|n| n.name.as_str()).collect::<Vec<_>>(), ["Bass", "Loose", "Mine"]);
    assert!(list[0].locked && list[0].children.as_ref().unwrap()[0].locked, "factory entries are locked");
    assert!(!list[2].locked);

    // Strip label › Channel Presets...: the PRESETS tab targets the strip.
    let mut v = ViewState::default();
    v.dispatch(Action::Tap(strip(Control::Menu(MenuCmd::Presets(PresetScope::Channel)))), &mut m);
    assert_eq!(v.browser.preset_target, Some((IN0.into(), PresetScope::Channel)));
    // A file row loads it: `set <strip>/Preset/value "Bass/Factory A - Bass"`.
    v.dispatch(Action::Tap(ControlId { strip: "browser".into(), control: Control::BrowserPresetFile(0, 0) }), &mut m);
    let w = writes_with(&server, "/inputs/0/Preset/value", 1);
    assert!(w[0].contains("\"Bass/Factory A - Bass\""), "{w:?}");
    // A top-level file: no folder.
    v.dispatch(Action::Tap(ControlId { strip: "browser".into(), control: Control::BrowserPresetFile(1, u16::MAX) }), &mut m);
    let w = writes_with(&server, "/inputs/0/Preset/value", 2);
    assert!(w[1].contains("\"Loose\""), "{w:?}");

    // SAVE AS...: a name field, then `post /SaveAsPreset`.
    v.dispatch(Action::Tap(strip(Control::PresetCmd(PresetScope::Channel, PresetCmd::SaveAs))), &mut m);
    assert!(v.editing());
    v.key_text("Mine/Lead Vox");
    v.key_enter(&mut m);
    let w = writes_with(&server, "/SaveAsPreset", 1);
    assert!(w[0].contains(r#""path":"/devices/0/inputs/0""#) && w[0].contains(r#""pluginName":"Channel Strip""#) && w[0].contains(r#""presetName":"Mine/Lead Vox""#), "{w:?}");

    // RENAME keeps the folder; DELETE names the preset; NEW FOLDER names the object's Preset.
    v.dispatch(Action::Tap(strip(Control::PresetCmd(PresetScope::Channel, PresetCmd::Rename("Mine/Vox Chain".into())))), &mut m);
    v.key_text(" 2");
    v.key_enter(&mut m);
    let w = writes_with(&server, "/MovePreset", 1);
    assert!(w[0].contains(r#""isFolder":false"#) && w[0].contains(r#""origPresetName":"Mine/Vox Chain""#) && w[0].contains(r#""newPresetName":"Mine/Vox Chain 2""#), "{w:?}");
    v.dispatch(Action::Tap(strip(Control::PresetCmd(PresetScope::Channel, PresetCmd::Delete("Loose".into())))), &mut m);
    let w = writes_with(&server, "/DeletePreset", 1);
    assert!(w[0].contains(r#""presetName":"Loose""#) && w[0].contains(r#""pluginName":"Channel Strip""#), "{w:?}");
    v.dispatch(Action::Tap(strip(Control::PresetCmd(PresetScope::Channel, PresetCmd::NewFolder))), &mut m);
    v.key_text("Live");
    v.key_enter(&mut m);
    let w = writes_with(&server, "/NewFolder", 1);
    assert!(w[0].contains(r#""folder":"Live""#) && w[0].contains(r#""path":"/devices/0/inputs/0/Preset""#), "{w:?}");
}

#[test]
fn plugin_presets_and_plugin_bar() {
    let server = MockEngine::start(tree(), MockOptions::default()).unwrap();
    let mut m = live(&server);
    let s = m.inputs()[0].clone();
    let list = m.plugin_presets(&s, 0);
    assert_eq!(list.iter().map(|n| n.name.as_str()).collect::<Vec<_>>(), ["Fast Attack", "Vocals"]);
    assert_eq!(rack::presets::flatten(&list), ["Fast Attack", "Vocals/Smooth"]);

    // An insert's Presets...: its plug-in's presets; a row loads into the effect.
    let mut v = ViewState::default();
    v.dispatch(Action::Tap(strip(Control::Menu(MenuCmd::Presets(PresetScope::Insert(0))))), &mut m);
    v.dispatch(Action::Tap(ControlId { strip: "browser".into(), control: Control::BrowserPresetFolder(1) }), &mut m);
    v.dispatch(Action::Tap(ControlId { strip: "browser".into(), control: Control::BrowserPresetFile(1, 0) }), &mut m);
    let w = writes_with(&server, "/effects/0/Preset/value", 1);
    assert!(w[0].contains("\"Vocals/Smooth\""), "{w:?}");
    assert_eq!(m.preset_name(&s, PresetScope::Insert(0)).as_deref(), Some("Vocals/Smooth"), "the local value moves at once");

    // SAVE AS on an insert names the effect and its plug-in.
    v.dispatch(Action::Tap(strip(Control::PresetCmd(PresetScope::Insert(0), PresetCmd::SaveAs))), &mut m);
    v.key_text("Mine");
    v.key_enter(&mut m);
    let w = writes_with(&server, "/SaveAsPreset", 1);
    assert!(w[0].contains(r#""path":"/devices/0/inputs/0/effects/0""#) && w[0].contains(r#""pluginName":"UAD UA 1176 Rev A""#), "{w:?}");

    // The plug-in window's COPY / PASTE.
    m.apply(&Action::Tap(strip(Control::PresetCmd(PresetScope::Insert(0), PresetCmd::CopySettings))));
    m.apply(&Action::Tap(strip(Control::PresetCmd(PresetScope::Insert(0), PresetCmd::PasteSettings))));
    let copy = writes_with(&server, "/PluginBarCopySettings", 1);
    let paste = writes_with(&server, "/PluginBarPasteSettings", 1);
    assert!(copy[0].contains(r#"{"effectPath":"/devices/0/inputs/0/effects/0"}"#), "{copy:?}");
    assert!(paste[0].contains(r#"{"effectPath":"/devices/0/inputs/0/effects/0"}"#), "{paste:?}");
}

#[test]
fn read_only_sends_nothing() {
    let server = MockEngine::start(tree(), MockOptions::default()).unwrap();
    let mut m = EngineMixer::connect(server.endpoint(), false);
    let end = Instant::now() + Duration::from_secs(10);
    while *m.store().status() != Status::Live {
        assert!(Instant::now() < end);
        m.pump();
        std::thread::sleep(Duration::from_millis(5));
    }
    m.set_text(&strip(Control::Preset(PresetScope::Channel)), "Bass/Factory A - Bass");
    m.apply(&Action::Tap(strip(Control::PresetCmd(PresetScope::Channel, PresetCmd::Delete("Loose".into())))));
    m.pump();
    std::thread::sleep(Duration::from_millis(100));
    let sent: Vec<_> = server.write_commands().into_iter().filter(|c| c.contains("Preset")).collect();
    assert!(sent.is_empty(), "read-only connection sent {sent:?}");
}

#[test]
fn a_preset_row_drags_onto_a_strip_and_loads_there() {
    use rack::dnd::DragPayload;
    use rack::{Hit, HitKind, Rect};
    let server = MockEngine::start(tree(), MockOptions::default()).unwrap();
    let mut m = live(&server);
    let mut v = ViewState::default();
    v.browser.tab = rack::surfaces::BrowserTab::Presets;
    // An untargeted PRESETS file row is a drag source carrying "[folder/]name".
    let row = Hit { rect: Rect::new(0.0, 0.0, 200.0, 20.0), id: ControlId { strip: "browser".into(), control: Control::BrowserPresetFile(2, 0) }, kind: HitKind::Tap };
    let (payload, _) = v.drag_source(&[row], 10.0, 10.0, &m).expect("preset rows drag");
    assert!(matches!(&payload, DragPayload::Preset { path } if path == "Mine/Vox Chain"), "{payload:?}");
    // The drop on input 1 loads it there: `set <strip>/Preset/value "Mine/Vox Chain"`.
    v.dispatch(Action::Tap(ControlId { strip: "/devices/0/inputs/1".into(), control: Control::PresetDrop("Mine/Vox Chain".into()) }), &mut m);
    let w = writes_with(&server, "/inputs/1/Preset/value", 1);
    assert!(w[0].contains("\"Mine/Vox Chain\""), "{w:?}");
}
