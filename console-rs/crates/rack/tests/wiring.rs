//! Console controls whose engine wiring needed the UI's help: the CLOCK outline for an unlocked
//! clock, TALK's latch / momentary press, send pans, CUE BUS COUNT's choice. A minimal fixture
//! Appearance, no UA assets.

use std::path::PathBuf;

use houston::Resources;
use rack::input::{Action, Control, ControlId, Hit, HitKind, Interaction};
use rack::settings::{self, MemoryProperties, PropValue, SettingOp, SettingsGroup, Write};
use rack::{Assets, Draw, MixerModel, PreviewMixer, Rgba, Ui, ViewState, build_window_with};

const FIXTURE: &str = r#"{
    "file_format": "appearance",
    "views": { "Switch-Large": {}, "Switch-Small": {}, "Switch-Small-Red": {} },
    "backgrounds": {
        "Keyboard-Focus": { "color-outline": "184,138,40,1.0", "outline-corner": 4, "outline-thickness": 1 },
        "Info-Focus": { "parent": "Keyboard-Focus", "outline-corner": 2, "inset-outline": "1,1,3,1" }
    },
    "knobs": { "Pan-White": {} }
}"#;

const AMBER: Rgba = Rgba::rgb8(184, 138, 40);

fn assets() -> Assets {
    static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    let dir = DIR.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("rack-wiring-fixture-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("appearance")).unwrap();
        std::fs::write(dir.join("appearance/Appearance.json"), FIXTURE).unwrap();
        dir
    });
    Assets::load(Resources::at(dir).expect("fixture resources")).expect("fixture appearance")
}

fn amber_fills(assets: &Assets, model: &dyn MixerModel, view: &ViewState) -> Vec<rack::Rect> {
    let mut ui = Ui::new(assets);
    build_window_with(&mut ui, model, view);
    ui.list
        .items
        .iter()
        .filter_map(|d| match d {
            Draw::Fill { rect, color } if *color == AMBER => Some(*rect),
            _ => None,
        })
        .collect()
}

fn clocked(locked: Option<bool>) -> PreviewMixer {
    let mut m = PreviewMixer::default();
    m.status.sample_rate = Some(48_000);
    m.status.clock_source = Some("S/PDIF".into());
    m.status.clock_locked = locked;
    m
}

/// `Rack_ClockInfo::paint` (0x181547d00): `Info-Focus` over the CLOCK item while the clock is not
/// locked and the blink flag is on; nothing otherwise.
#[test]
fn unlocked_clock_blinks_the_info_focus_outline() {
    let assets = assets();
    let on = ViewState { blink: true, ..ViewState::default() };
    let off = ViewState::default();
    let ring = amber_fills(&assets, &clocked(Some(false)), &on);
    assert_eq!(ring.len(), 4, "a 1-pt outline: {ring:?}");
    // The CLOCK item (x 301 in the bar, 140 wide) inset 1,1,3,1.
    let left = ring.iter().map(|r| r.x).fold(f32::MAX, f32::min);
    let right = ring.iter().map(|r| r.max_x()).fold(f32::MIN, f32::max);
    assert_eq!((left, right), (302.0, 438.0));
    assert!(amber_fills(&assets, &clocked(Some(false)), &off).is_empty(), "the off phase");
    assert!(amber_fills(&assets, &clocked(Some(true)), &on).is_empty(), "locked");
    assert!(amber_fills(&assets, &clocked(None), &on).is_empty(), "unknown");
}

/// The blink flag: 500 ms on, 500 ms off from the first tick; `tick_blink` says when it flips.
#[test]
fn blink_phase_is_half_a_second() {
    let mut v = ViewState::default();
    let t0 = std::time::Instant::now();
    assert!(v.tick_blink(t0) && v.blink);
    assert!(!v.tick_blink(t0 + std::time::Duration::from_millis(499)));
    assert!(v.tick_blink(t0 + std::time::Duration::from_millis(500)) && !v.blink);
    assert!(v.tick_blink(t0 + std::time::Duration::from_millis(1000)) && v.blink);
}

fn hits(assets: &Assets, model: &dyn MixerModel, view: &ViewState) -> Vec<Hit> {
    let mut ui = Ui::new(assets);
    build_window_with(&mut ui, model, view);
    ui.hits
}

/// TALK is a latch hit: a click toggles on the press; a long press turns it off on release.
#[test]
fn talk_button_latches_and_is_momentary_when_held() {
    let assets = assets();
    let mut model = PreviewMixer::default();
    let mut view = ViewState { show_ctrl_room: true, ..ViewState::default() };
    let hits = hits(&assets, &model, &view);
    let talk = ControlId { strip: "monitor".into(), control: Control::Talkback };
    let hit = hits.iter().rev().find(|h| h.id == talk).expect("TALK").clone();
    assert_eq!(hit.kind, HitKind::Latch);
    assert_eq!(hits.iter().filter(|h| h.id == talk).count(), 1, "one hit, not a toggle and a latch");
    let (x, y) = hit.rect.center();
    let mut i = Interaction::default();
    for a in i.pointer_down(&hits, x, y, false) {
        view.dispatch(a, &mut model);
    }
    assert_eq!(model.monitor_options().talkback, Some(true), "on at the press");
    for a in i.pointer_up_at(std::time::Instant::now() + rack::input::LATCH_HOLD) {
        view.dispatch(a, &mut model);
    }
    assert_eq!(model.monitor_options().talkback, Some(false), "momentary: off at the release");
    for a in i.pointer_down(&hits, x, y, false).into_iter().chain(i.pointer_up()) {
        view.dispatch(a, &mut model);
    }
    assert_eq!(model.monitor_options().talkback, Some(true), "a click latches");
}

/// A full send strip's pan knob shows the send's pan and drags it.
#[test]
fn send_pan_knob_shows_and_sets_the_send_pan() {
    let assets = assets();
    let mut model = PreviewMixer::default();
    let in0: std::sync::Arc<str> = "in0".into();
    model.sends.get_mut(&in0).expect("in0 sends")[0].0.pan = 0.25;
    let mut view = ViewState::default();
    view.sections.sends = true;
    view.sections.inputs = false;
    let hits = hits(&assets, &model, &view);
    let pan = ControlId { strip: in0.clone(), control: Control::SendPan(0) };
    let hit = hits.iter().rev().find(|h| h.id == pan).expect("send pan");
    assert!(
        matches!(hit.kind, HitKind::Drag { value, reset: Some(r), .. } if value == 0.25 && r == 0.5),
        "{:?}",
        hit.kind
    );
    view.dispatch(Action::Set(pan, 0.75), &mut model);
    assert_eq!(model.sends[&in0][0].0.pan, 0.75);
}

/// The Sends window's pan knob writes the send's `Pan` (−1…1).
#[test]
fn sends_window_pan_writes_the_send_pan() {
    let src = MemoryProperties::demo_x4().with("/devices/0/inputs/0/sends/0/Pan", PropValue::Float(0.0));
    let w = settings::resolve(&src, "pan:/devices/0/inputs/0/sends/0/Pan", &SettingOp::Value(0.25));
    assert_eq!(w, [Write::Set("/devices/0/inputs/0/sends/0/Pan".into(), PropValue::Float(-0.5))]);
}

/// CUE BUS COUNT offers 2…4 of the engine's 0…4: picking the third option writes 4, not 2.
#[test]
fn cue_bus_count_writes_the_count_it_shows() {
    let src = MemoryProperties::demo_x4();
    let f = settings::fields(&src, SettingsGroup::Hardware);
    let cue = f.iter().find(|f| f.label == "CUE BUS COUNT").expect("CUE BUS COUNT");
    let settings::FieldKind::Choice { options, .. } = &cue.kind else { panic!("{:?}", cue.kind) };
    assert_eq!(options, &["2", "3", "4"]);
    for (i, want) in [(0, 2), (1, 3), (2, 4)] {
        assert_eq!(
            settings::resolve(&src, &cue.key, &SettingOp::Choose(i)),
            [Write::Set("/CueBusCount".into(), PropValue::Int(want))]
        );
    }
    assert!(settings::resolve(&src, &cue.key, &SettingOp::Choose(3)).is_empty());
}
