use rack::{Action, Control, ControlId, PreviewMixer, ViewState};
use rack::settings::{fields, MemoryProperties, PropValue, SettingsGroup, SettingsTab};
use rack::surfaces::view::Sheet;

#[test]
fn native_delay_settings_follow_device_capability_and_show_warning() {
    let properties = MemoryProperties::demo_x4()
        .with("/NativePluginHosting", PropValue::Bool(true));
    let unsupported = fields(&properties, SettingsGroup::Hardware);
    assert!(!unsupported.iter().find(|f| f.key.as_ref() == "/MaxDelayComp").unwrap().enabled);
    let message = "Input 3/4 needs 124 samples. Increase Input Delay Compensation to Medium in Settings > Hardware.";
    let properties = properties.with("/NativeDelayCompSupported", PropValue::Bool(true))
        .with("/InputDelayCompWarning", PropValue::Str(message.into()));
    let supported = fields(&properties, SettingsGroup::Hardware);
    assert!(supported.iter().find(|f| f.key.as_ref() == "/MaxDelayComp").unwrap().enabled);
    assert_eq!(supported.iter().find(|f| f.key.as_ref() == "/InputDelayCompWarning").unwrap().value_text(), message);
}

#[test]
fn clicking_delay_warning_opens_hardware_settings_without_changing_budget() {
    let mut view = ViewState::default();
    let mut model = PreviewMixer::default();
    view.settings.tab = SettingsTab::Midi;
    for _ in 0..2 {
        view.dispatch(Action::Tap(ControlId { strip: "delay-comp".into(), control: Control::ShowSettings }), &mut model);
        assert_eq!(view.sheet, Some(Sheet::Settings));
        assert_eq!(view.settings.tab, SettingsTab::Hardware);
    }
}
