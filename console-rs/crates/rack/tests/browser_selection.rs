//! Client-side selection/activation, never the engine's SelectedOnFront knob focus.
use houston::Rect;
use rack::{Action, Control, ControlId, Hit, HitKind, Interaction, MixerModel, PreviewMixer, ViewState};
use rack::surfaces::Effect;

fn id(strip: &str, control: Control) -> ControlId { ControlId { strip:strip.into(), control } }

#[test]
fn double_click_is_activation_while_single_click_only_selects() {
    let hit = Hit { id:id("browser",Control::BrowserItem(0)),rect:Rect::new(0.0,0.0,100.0,30.0),kind:HitKind::Tap };
    let mut input = Interaction::default();
    assert_eq!(input.pointer_down(&[hit.clone()],5.0,5.0,false),[Action::Tap(hit.id.clone())]);
    assert_eq!(input.pointer_down(&[hit],5.0,5.0,true),[Action::Tap(id("browser",Control::BrowserActivate(0)))]);
}

#[test]
fn activation_uses_selected_strip_first_empty_insert_and_explicit_target_wins() {
    let model = PreviewMixer::default();
    let mut view = ViewState::default();
    assert!(view.route(Action::Tap(id("browser",Control::BrowserActivate(0))),&model).is_empty(), "no guessed default channel");
    let strip = model.inputs()[0].clone();
    view.route(Action::GestureBegin(id(&strip.id,Control::Fader)),&model);
    assert_eq!(view.selected_channel,Some(strip.id.clone()));
    let free = model.inserts(&strip).iter().position(|slot| matches!(slot,rack::model::InsertState::Empty)).unwrap();
    let effects = view.route(Action::Tap(id("browser",Control::BrowserActivate(0))),&model);
    assert_eq!(effects,[Effect::SetText(id(&strip.id,Control::Insert(free as u8)),model.plugin_catalog()[0].name.clone())]);
    let target = model.inputs()[1].id.clone();
    view.begin_assign(target.clone(),2);
    let effects = view.route(Action::Tap(id("browser",Control::BrowserActivate(0))),&model);
    assert_eq!(effects,[Effect::SetText(id(&target,Control::Insert(2)),model.plugin_catalog()[0].name.clone())]);
}

#[test]
fn preset_double_click_loads_the_selected_channel_without_an_assign_header() {
    let model = PreviewMixer::default();
    let mut view = ViewState::default();
    let strip = model.inputs()[1].id.clone();
    view.route(Action::GestureBegin(id(&strip,Control::Fader)),&model);
    let tree = view.preset_tree(&model);
    let top = 0;
    let child = 0;
    let value = format!("{}/{}", tree[0].name, tree[0].children.as_ref().unwrap()[0].name);
    let effects = view.route(Action::Tap(id("browser",Control::BrowserPresetActivate(top,child))),&model);
    assert_eq!(effects,[Effect::SetText(id(&strip,Control::Preset(rack::presets::PresetScope::Channel)),value)]);
}
