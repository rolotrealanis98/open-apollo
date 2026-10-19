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

/// Console's Unison assign browser: a tap on the empty Unison slot arms assign mode for it, the
/// list holds only Unison plug-ins, and a non-Unison pick is refused with the browser notice.
#[test]
fn unison_slot_assign_lists_and_takes_only_unison_plugins() {
    let model = PreviewMixer::default();
    let cat = model.plugin_catalog();
    let strip = model.inputs()[0].id.clone();
    let mut view = ViewState::default();
    assert!(view.route(Action::Tap(id(&strip,Control::Insert(255))),&model).is_empty());
    assert_eq!(view.browser.assign_to,Some(id(&strip,Control::Insert(255))));
    assert!(view.show_browser);
    let shown = rack::surfaces::browser::filtered(&cat,&model.plugin_categories(),&view);
    let unison: Vec<usize> = (0..cat.len()).filter(|&i| cat[i].unison == Some(true)).collect();
    assert!(!unison.is_empty() && unison.len() < cat.len());
    assert_eq!(shown,unison);
    // Refused: assign mode stays armed and the browser says why.
    let comp = cat.iter().position(|p| p.unison == Some(false)).unwrap() as u16;
    for c in [Control::BrowserActivate(comp),Control::BrowserItem(comp)] {
        assert!(view.route(Action::Tap(id("browser",c)),&model).is_empty());
        assert!(view.browser.assign_to.is_some() && view.browser.notice.is_some());
    }
    let pre = unison[0] as u16;
    assert_eq!(view.route(Action::Tap(id("browser",Control::BrowserItem(pre))),&model),[Effect::SetText(id(&strip,Control::Insert(255)),cat[pre as usize].name.clone())]);
    assert!(view.browser.assign_to.is_none() && view.browser.notice.is_none());
    // A selected non-Unison row tapped onto the Unison slot is refused and stays selected.
    view.route(Action::Tap(id("browser",Control::BrowserItem(comp))),&model);
    assert!(view.route(Action::Tap(id(&strip,Control::Insert(255))),&model).is_empty());
    assert_eq!((view.browser.selected,view.browser.assign_to.is_none()),(Some(comp),true));
    // A strip insert's assign mode lists everything.
    view.browser.selected = None;
    let s = model.inputs()[0].clone();
    let free = model.inserts(&s).iter().position(|slot| matches!(slot,rack::model::InsertState::Empty)).unwrap() as u8;
    assert!(view.route(Action::Tap(id(&strip,Control::Insert(free))),&model).is_empty());
    assert_eq!(view.browser.assign_to,Some(id(&strip,Control::Insert(free))));
    assert_eq!(rack::surfaces::browser::filtered(&cat,&model.plugin_categories(),&view),(0..cat.len()).collect::<Vec<_>>());
}
