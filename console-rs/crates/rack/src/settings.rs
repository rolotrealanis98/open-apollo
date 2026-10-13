//! Console's Settings window and secondary panels as data: the tabs and fields UAD Console
//! 1.3.1 shows, the engine properties they bind and the writes their controls make. Drawing
//! lives in `surfaces::settings` (the window) and `surfaces::panels` (CUE OUTPUTS, MONITOR
//! CONTROLLER, HEADPHONE CORRECTION, PLUG-IN SCENES, About).
//!
//! A model exposes its properties through [`PropertySource`] (engine paths such as
//! `/SampleRate` or `/devices/0/TOSLink`, plus client preferences under `pref:`); [`fields`]
//! builds what a settings group shows and [`resolve`] turns a control operation into
//! [`Write`]s the model applies. Both models (`PreviewMixer`, the engine mixer) share this
//! code, so Console's rules live in one place.
//!
//! Evidence (catalog: `console-port/surfaces/settings.md`):
//! - [obs] Tabs: `Rack_SettingsHeader::UpdateTabs` (Win Rack.dll 0x180e90220): HARDWARE,
//!   I/O SETTINGS or I/O MATRIX, PLUG-INS, OPTIONS, MIDI, CONTROLLERS, NETWORK, DANTE DEVICES,
//!   each behind a predicate.
//! - [obs] HARDWARE (Apollo): `Rack_ApolloSettings` ctor 0x180e28140 / `resized` 0x180e2ec50:
//!   a grid of `Silk-Screen` label boxes (160 × 58) holding a `Combo-Box` (150 × 36 at +5,+17),
//!   170 pt apart, rows at y 10 / 78 / 146; DEVICES list and per-device OPTIONS below.
//! - [obs] Property vocabulary: the Rack client's node models (root 0x181b262b0, device
//!   0x181b15a00, outputs 0x181b1ef20 / 0x181b1e370) and the engine 12.0.0 tree (refpairs).
//! - [obs] Commands and argument shapes: the engine root `commands` node (`Identify {path}`,
//!   `RemoveDevice {path}`, `SaveIOMapPreset {ioPresetName}`, `UpdatePlugInStatus {}`, …).

use std::collections::BTreeMap;
use std::sync::Arc;

/// A property value as settings read and write it.
#[derive(Clone, Debug, PartialEq)]
pub enum PropValue {
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
}

impl PropValue {
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            PropValue::Bool(b) => Some(*b),
            PropValue::Int(i) => Some(*i != 0),
            PropValue::Float(f) => Some(*f != 0.0),
            PropValue::Str(_) => None,
        }
    }
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            PropValue::Bool(b) => Some(*b as i64),
            PropValue::Int(i) => Some(*i),
            PropValue::Float(f) => Some(f.round() as i64),
            PropValue::Str(s) => s.trim().parse().ok(),
        }
    }
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            PropValue::Bool(b) => Some(*b as i64 as f64),
            PropValue::Int(i) => Some(*i as f64),
            PropValue::Float(f) => Some(*f),
            PropValue::Str(s) => s.trim().parse().ok(),
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            PropValue::Str(s) => Some(s),
            _ => None,
        }
    }
    /// The value as Console's combo would label it when the engine gives no label.
    pub fn display(&self) -> String {
        match self {
            PropValue::Bool(b) => if *b { "ON" } else { "OFF" }.into(),
            PropValue::Int(i) => i.to_string(),
            PropValue::Float(f) => {
                if f.fract() == 0.0 {
                    format!("{f:.0}")
                } else {
                    format!("{f}")
                }
            }
            PropValue::Str(s) => s.clone(),
        }
    }
    /// Loose equality: numbers compare by value, so an engine `48000.0` matches `Int(48000)`.
    pub fn same(&self, other: &PropValue) -> bool {
        match (self, other) {
            (PropValue::Str(a), PropValue::Str(b)) => a == b,
            (PropValue::Str(_), _) | (_, PropValue::Str(_)) => false,
            (a, b) => match (a.as_f64(), b.as_f64()) {
                (Some(x), Some(y)) => (x - y).abs() < 1e-9,
                _ => false,
            },
        }
    }
}

/// One entry of an engine enum (`values`): the value, Console's label (`string` when the
/// engine gives one) and whether Console offers it (`enabled`).
#[derive(Clone, Debug, PartialEq)]
pub struct Choice {
    pub value: PropValue,
    pub label: String,
    pub enabled: bool,
    /// The engine gave the label (`values[].string`, e.g. "48 kHz"); Console shows other
    /// string values in upper case ("Internal" → INTERNAL).
    pub labeled: bool,
}

impl Choice {
    pub fn new(value: PropValue) -> Self {
        let label = value.display();
        Self { value, label, enabled: true, labeled: false }
    }
    pub fn labeled(value: PropValue, label: impl Into<String>) -> Self {
        Self { value, label: label.into(), enabled: true, labeled: true }
    }
}

/// Read access to a model's properties. Paths are engine property paths (`/SampleRate`,
/// `/devices/0/outputs/18/MirrorsToDigital`); `pref:<name>` are client preferences.
pub trait PropertySource {
    fn prop(&self, path: &str) -> Option<PropValue>;
    /// The property's enum (`values`), in engine order.
    fn choices(&self, _path: &str) -> Vec<Choice> {
        Vec::new()
    }
    /// Numeric child names of a node (`/devices` → [0], `/devices/0/outputs` → [0, …, 21]).
    fn indices(&self, _node: &str) -> Vec<usize> {
        Vec::new()
    }
    /// `min`/`max` of a numeric property.
    fn range(&self, _path: &str) -> Option<(f64, f64)> {
        None
    }
    fn readonly(&self, _path: &str) -> bool {
        false
    }
    /// The property's factory value (`<property>/default`: `Name/default` is the channel's
    /// default name).
    fn default_value(&self, _path: &str) -> Option<PropValue> {
        None
    }
}

/// A write a settings control makes.
#[derive(Clone, Debug, PartialEq)]
pub enum Write {
    /// `set <path>/value <v>`.
    Set(String, PropValue),
    /// `post /<command> <json payload>` (engine root commands).
    Post(String, String),
    /// A client preference (Console keeps these in its own preferences, not in the engine).
    Pref(String, PropValue),
    /// `set <path>/value <v>` on the UA Mixer Helper (4720), e.g. NETWORK INTERFACE.
    Helper(String, PropValue),
}

/// PropertySource prefix for UA Mixer Helper properties (`helper:/dante/…`).
pub const HELPER_PREFIX: &str = "helper:";
/// The helper property NETWORK INTERFACE sets, and the one listing the adapters.
pub const NIC_PATH: &str = "/dante/primary_network_adapter";
pub const NIC_LIST_PATH: &str = "/dante/available_network_adapters";
/// Console's question before switching (`Rack_NetworkSettings` 0x180e3f230).
pub const NIC_CONFIRM_TITLE: &str = "SWITCH NETWORK INTERFACE?";
pub const NIC_CONFIRM_MESSAGE: &str = "Switching the network interface may interrupt audio with Apollo E devices. Proceed?";

/// What a control did to a field.
#[derive(Clone, Debug, PartialEq)]
pub enum SettingOp {
    /// Pick option `.0` of a choice.
    Choose(usize),
    /// Press a toggle or button.
    Press,
    /// Commit edited text.
    Text(String),
    /// A continuous value 0…1 (send faders).
    Value(f64),
}

// ---- Tabs, panels and groups ----

/// Settings window tabs (`Rack_SettingsHeader`), in Console's order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SettingsTab {
    #[default]
    Hardware,
    IoMatrix,
    IoSettings,
    PlugIns,
    Options,
    Midi,
    Controllers,
    Network,
    DanteDevices,
}

impl SettingsTab {
    pub const ALL: [SettingsTab; 9] = [
        SettingsTab::Hardware,
        SettingsTab::IoMatrix,
        SettingsTab::IoSettings,
        SettingsTab::PlugIns,
        SettingsTab::Options,
        SettingsTab::Midi,
        SettingsTab::Controllers,
        SettingsTab::Network,
        SettingsTab::DanteDevices,
    ];
    pub fn label(self) -> &'static str {
        match self {
            SettingsTab::Hardware => "HARDWARE",
            SettingsTab::IoMatrix => "I/O MATRIX",
            SettingsTab::IoSettings => "I/O SETTINGS",
            SettingsTab::PlugIns => "PLUG-INS",
            SettingsTab::Options => "OPTIONS",
            SettingsTab::Midi => "MIDI",
            SettingsTab::Controllers => "CONTROLLERS",
            SettingsTab::Network => "NETWORK",
            SettingsTab::DanteDevices => "DANTE DEVICES",
        }
    }
    /// The `settings_*_tab` component id.
    pub fn component_id(self) -> &'static str {
        match self {
            SettingsTab::Hardware => "settings_hardware_tab",
            SettingsTab::IoMatrix => "settings_io_matrix_tab",
            SettingsTab::IoSettings => "settings_io_settings_tab",
            SettingsTab::PlugIns => "settings_plugin_manager_tab",
            SettingsTab::Options => "settings_options_tab",
            SettingsTab::Midi => "settings_midi_tab",
            SettingsTab::Controllers => "settings_controllers_tab",
            SettingsTab::Network => "settings_network_tab",
            SettingsTab::DanteDevices => "settings_dante_network_tab",
        }
    }
}

/// Windows Console opens from buttons besides Settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Panel {
    /// Monitor column CUE OUTPUTS: `Rack_CuesPopover` holding `Rack_CuesAssignments`.
    CueOutputs,
    /// Monitor column MONITOR CONTROLLER (and MONITOR CORRECTION › MAIN MON):
    /// `Rack_MonitorSubWindow`.
    MonitorController,
    /// MONITOR CORRECTION › HEADPHONES: the `HPCorrection` sub-window.
    HeadphoneCorrection,
    /// Navigator SHOW › PLUG-IN SCENES: `Rack_SceneSection`.
    PluginScenes,
    /// The About box (`About-Background`).
    About,
    /// `Rack_MonitorMetersWindow` "Monitor Meters" (capture s44): the tall L / R meter window.
    MeterWindow,
    /// `Rack_SendsPopover` (captures s50 / s51): a strip's sends as faders.
    Sends(SendsOf),
    /// ADD DEVICE (capture s26), from HARDWARE › DEVICES "+".
    AddDevice,
    /// I/O PRESETS (capture s38), from I/O MATRIX › I/O PRESETS.
    IoPresets,
    /// The cascading source picker of an I/O MATRIX row (capture s35): route index, outputs.
    RouteSource { route: u16, outputs: bool },
    /// TEMPO (capture s05), from the status bar.
    Tempo,
}

/// Whose sends a [`Panel::Sends`] shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SendsOf {
    /// The talkback strip (`inputs/N` with `IOType` `TalkbackMic`): AUX 1 / 2, CUE 1 / 2.
    Talkback,
    /// Aux `.0` (`auxs/N`): its cue sends.
    Aux(u8),
    /// An input strip (`inputs/N`): AUX 1 / 2, CUE 1 / 2 with their pans (capture send-cell,
    /// a right click on the strip's sends cell).
    Input(u8),
    AuxDevice(usize, u8),
    InputDevice(usize, u8),
}

impl SendsOf {
    pub fn parts(self) -> Option<(usize, &'static str, u8)> {
        match self {
            Self::Talkback => None,
            Self::Aux(k) => Some((0, "auxs", k)),
            Self::Input(k) => Some((0, "inputs", k)),
            Self::AuxDevice(d, k) => Some((d, "auxs", k)),
            Self::InputDevice(d, k) => Some((d, "inputs", k)),
        }
    }
    pub fn on_device(self, device: usize) -> Self {
        match self.parts() {
            Some((_, "auxs", k)) => if device == 0 { Self::Aux(k) } else { Self::AuxDevice(device, k) },
            Some((_, _, k)) => if device == 0 { Self::Input(k) } else { Self::InputDevice(device, k) },
            None => Self::Talkback,
        }
    }
    pub fn node(self) -> Option<String> {
        self.parts().map(|(d, kind, k)| format!("/devices/{d}/{kind}/{k}"))
    }
    pub fn is_input(self) -> bool { matches!(self, Self::Input(_) | Self::InputDevice(..)) }
    pub fn with_index(self, index: u8) -> Self {
        match self.parts() {
            Some((d, "auxs", _)) => Self::Aux(index).on_device(d),
            Some((d, _, _)) => Self::Input(index).on_device(d),
            None => Self::Talkback,
        }
    }
}

impl Panel {
    pub fn title(self) -> &'static str {
        match self {
            Panel::CueOutputs => "CUE OUTPUTS",
            Panel::MonitorController => "MONITOR CONTROLLER",
            Panel::HeadphoneCorrection => "HEADPHONE CORRECTION",
            Panel::PluginScenes => "PLUG-IN SCENES",
            Panel::About => "ABOUT",
            Panel::MeterWindow => "",
            Panel::Sends(SendsOf::Talkback) => "TALKBACK",
            Panel::Sends(SendsOf::Aux(_) | SendsOf::AuxDevice(..)) => "AUX",
            Panel::Sends(SendsOf::Input(_) | SendsOf::InputDevice(..)) => "SENDS",
            Panel::AddDevice => "ADD DEVICE",
            Panel::IoPresets => "I/O PRESETS",
            Panel::RouteSource { .. } => "INPUT",
            Panel::Tempo => "TEMPO",
        }
    }
}

/// A set of fields one Console component shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SettingsGroup {
    /// HARDWARE: global Apollo settings (`Rack_ApolloSettings`).
    Hardware,
    /// HARDWARE › OPTIONS for device `.0` (`Rack_DeviceOptions`).
    Device(u8),
    /// I/O MATRIX header (`Rack_ApolloIOMatrixSettings`).
    IoMatrix,
    /// OPTIONS › DISPLAY (`Rack_DisplaySettings`).
    Display,
    /// OPTIONS › EDITING (`Rack_EditingSettings`, Console's subset).
    Editing,
    /// MIDI (`Rack_MIDISettings_Apollo`).
    Midi,
    /// PLUG-INS (`Rack_PlugInMgrSettings`, Console's subset).
    PlugIns,
    /// CUE OUTPUTS: one `Rack_CueRoute` per active cue of device `.0` (row = cue).
    CueRoutes(u8),
    /// MONITOR CONTROLLER knob section (`Rack_MonitorKnobSection`).
    MonitorKnob(u8),
    /// MONITOR CONTROLLER › BASS MANAGEMENT (`Rack_BassManagement`).
    BassManagement(u8),
    /// MONITOR CONTROLLER › SPEAKER UTILITIES (`Rack_SpeakerUtilities`).
    SpeakerUtilities(u8),
    /// MAIN MONITOR CORRECTION (`Rack_RoomCorrectionSection`).
    RoomCorrection(u8),
    /// HEADPHONE CORRECTION, headphone `hp` (0 = HP 1).
    HeadphoneCorrection { device: u8, hp: u8 },
    /// DANTE DEVICES (read-only Dante identity of each Dante-capable device).
    Dante,
    /// NETWORK: interface, Dante controller / patents / discovery (`Rack_NetworkSettings`).
    Network,
}

/// How a field draws and edits.
#[derive(Clone, Debug, PartialEq)]
pub enum FieldKind {
    /// `Rack_ComboBox`: one of `options`.
    Choice { options: Vec<String>, enabled: Vec<bool>, selected: Option<usize> },
    /// `Switch-Large` button.
    Toggle { on: bool },
    /// A text field (DEVICE NAME).
    Text(String),
    /// An action button labelled with the field label (IDENTIFY, SAVE, RESCAN).
    Button,
    /// A captioned cell with one button (`"DON'T SHOW AGAIN" DIALOGS` › RESET).
    Action(String),
    /// A static value.
    Readout(String),
    /// A segmented slider (CHANNEL DSP PAIRING): `segments` cells, `lit` of them lit, with
    /// the numbers on both ends and their captions.
    Slider { segments: u8, lit: u8, left: String, right: String, left_label: String, right_label: String },
}

/// One labelled control of a settings group.
#[derive(Clone, Debug, PartialEq)]
pub struct SettingField {
    /// Binding key (see [`resolve`]): an engine path, or `kind:…` for derived bindings.
    pub key: Arc<str>,
    pub label: String,
    pub kind: FieldKind,
    pub enabled: bool,
    /// Grid row in Console's layout (HARDWARE rows 0/1/2; CUE OUTPUTS: the cue).
    pub row: u8,
    /// Cell width (`Rack_SilkScreen` box); 160 by default (I/O MATRIX: 90 / 210).
    pub width: f32,
    /// Explicit cell offset in the grid (MIDI's second row starts in the second column).
    pub at: Option<(f32, f32)>,
    /// A second control in the same cell, right of the first (I/O PRESETS › SAVE, TAP TEMPO
    /// EVENT › note number, DEVICE NAME › IDENTIFY).
    pub inline: Option<Box<SettingField>>,
    /// Text the control shows instead of the selected option (# INPUTS shows the live count).
    pub display: Option<String>,
}

impl SettingField {
    pub fn new(key: impl Into<Arc<str>>, label: impl Into<String>, kind: FieldKind, row: u8) -> Self {
        Self { key: key.into(), label: label.into(), kind, enabled: true, row, width: 160.0, at: None, inline: None, display: None }
    }
    pub fn disabled(mut self, off: bool) -> Self {
        self.enabled = !off;
        self
    }
    pub fn width(mut self, w: f32) -> Self {
        self.width = w;
        self
    }
    fn at(mut self, x: f32, y: f32) -> Self {
        self.at = Some((x, y));
        self
    }
    fn with_inline(mut self, f: SettingField) -> Self {
        self.inline = Some(Box::new(f));
        self
    }
    /// The selected option's label (choices), or the text / readout.
    pub fn value_text(&self) -> String {
        if let Some(d) = &self.display {
            return d.clone();
        }
        match &self.kind {
            FieldKind::Choice { options, selected, .. } => selected.and_then(|i| options.get(i)).cloned().unwrap_or_default(),
            FieldKind::Toggle { on } => if *on { "ON" } else { "OFF" }.into(),
            FieldKind::Text(t) | FieldKind::Readout(t) => t.clone(),
            FieldKind::Button => self.label.clone(),
            FieldKind::Action(b) => b.clone(),
            FieldKind::Slider { left, .. } => left.clone(),
        }
    }
}

/// A device in the HARDWARE › DEVICES list.
#[derive(Clone, Debug, PartialEq)]
pub struct SettingsDevice {
    /// Engine node (`/devices/0`).
    pub path: String,
    pub index: u8,
    pub name: String,
    /// `DeviceTypeID` ("Apollo x4").
    pub type_id: String,
    pub online: bool,
    /// Unit slot A…D (`MultiUnitOrder`; `Device-A-Settings-Background` …).
    pub slot: u8,
}

impl SettingsDevice {
    /// The unit letter Console colours the device with.
    pub fn letter(&self) -> char {
        (b'A' + self.slot.min(3)) as char
    }
    /// `Device-Icon-*` key for the type ("Apollo x4 Gen 2" → `Device-Icon-Apollo-x4-Gen-2`).
    pub fn icon_key(&self) -> String {
        format!("Device-Icon-{}", self.type_id.trim().replace(' ', "-"))
    }
}

/// A driver I/O channel in the I/O MATRIX route list (`/inputroutes/N`, `/outputroutes/N`).
#[derive(Clone, Debug, PartialEq)]
pub struct IoRoute {
    pub index: usize,
    /// `ChannelIndex` (1-based driver channel).
    pub channel: i64,
    pub name: String,
    /// The driver channel's own name (`Name/default`: "MIC/LINE/HIZ 1", "VIRTUAL 1").
    pub default_name: String,
    /// `IOType` (`MicLineHiZ`, `CtrlRoom`, …).
    pub io_type: String,
    pub active: bool,
}

// ---- Client preferences ----

/// Console's client-side preferences (not engine properties): defaults and storage.
#[derive(Clone, Debug, PartialEq)]
pub struct Prefs(pub BTreeMap<String, PropValue>);

impl Default for Prefs {
    fn default() -> Self {
        let mut m = BTreeMap::new();
        // Seconds a navigator modifier stays armed (OPTIONS › MODIFIERS TIMEOUT, a text field).
        m.insert("modifiers_timeout".into(), PropValue::Str("6".into()));
        m.insert("monitor_level_display".into(), PropValue::Str("dB".into()));
        m.insert("tap_tempo_channel".into(), PropValue::Str("1".into()));
        m.insert("tap_tempo_event".into(), PropValue::Str("NOTE".into()));
        m.insert("tap_tempo_number".into(), PropValue::Str("64".into()));
        m.insert("scenes_channel".into(), PropValue::Str("1".into()));
        m.insert("scenes_event".into(), PropValue::Str("PROGRAM CHANGE".into()));
        m.insert("network_interface".into(), PropValue::Str(String::new()));
        m.insert("short_names".into(), PropValue::Bool(false));
        Self(m)
    }
}

impl Prefs {
    pub fn get(&self, name: &str) -> Option<PropValue> {
        self.0.get(name).cloned()
    }
    pub fn set(&mut self, name: &str, v: PropValue) {
        self.0.insert(name.to_string(), v);
    }
    /// Choices for a preference with a fixed list.
    pub fn choices(name: &str) -> Vec<Choice> {
        let s = |v: &str| Choice::new(PropValue::Str(v.into()));
        match name {
            // `Rack_SpeakerUtilities` MONITOR LEVEL DISPLAY.
            "monitor_level_display" => vec![s("dB"), s("dB SPL")],
            // MIDI event types (1.3.1 captures s27 / s28).
            "tap_tempo_event" => vec![s("NOTE"), s("CONTROLLER")],
            "scenes_event" => vec![s("PROGRAM CHANGE"), s("NOTE")],
            _ => Vec::new(),
        }
    }
}

// ---- Reading helpers ----

fn device_paths(src: &dyn PropertySource) -> Vec<String> {
    src.indices("/devices").into_iter().map(|i| format!("/devices/{i}")).collect()
}

fn bool_at(src: &dyn PropertySource, path: &str) -> Option<bool> {
    src.prop(path).and_then(|v| v.as_bool())
}

fn str_at(src: &dyn PropertySource, path: &str) -> Option<String> {
    src.prop(path).and_then(|v| v.as_str().map(str::to_owned))
}

/// Outputs of `dev` whose `IOType` is `ty`.
fn outputs_of(src: &dyn PropertySource, dev: &str, ty: &str) -> Vec<String> {
    src.indices(&format!("{dev}/outputs"))
        .into_iter()
        .map(|n| format!("{dev}/outputs/{n}"))
        .filter(|o| str_at(src, &format!("{o}/IOType")).as_deref() == Some(ty))
        .collect()
}

/// The device's MONITOR output (`IOType` Monitor).
pub fn monitor_output(src: &dyn PropertySource, dev: &str) -> Option<String> {
    outputs_of(src, dev, "Monitor").into_iter().next()
}

/// A choice field from the engine enum at `path`: labels from `values[].string`, other string
/// values upper-cased as Console shows them (INTERNAL, LONG, LINEAR).
fn engine_choice(src: &dyn PropertySource, path: &str, label: &str, row: u8) -> Option<SettingField> {
    engine_choice_with(src, path, label, row, true, |_| true)
}

fn engine_choice_with(src: &dyn PropertySource, path: &str, label: &str, row: u8, upper: bool, keep: impl Fn(&Choice) -> bool) -> Option<SettingField> {
    let cur = src.prop(path)?;
    let choices: Vec<Choice> = src.choices(path).into_iter().filter(|c| keep(c)).collect();
    let selected = choices.iter().position(|c| c.value.same(&cur));
    let (options, enabled) = choices.iter().map(|c| (if upper && !c.labeled { c.label.to_uppercase() } else { c.label.clone() }, c.enabled)).unzip();
    let mut f = SettingField::new(path, label, FieldKind::Choice { options, enabled, selected }, row).disabled(src.readonly(path));
    if selected.is_none() {
        f.display = Some(if upper { cur.display().to_uppercase() } else { cur.display() });
    }
    Some(f)
}

/// A bool property shown as Console's OFF/ON combo (DIGITAL MIRROR, PT MODE).
fn bool_choice(src: &dyn PropertySource, path: &str, label: &str, labels: [&str; 2], row: u8) -> Option<SettingField> {
    let on = bool_at(src, path)?;
    let key = format!("bool:{}:{}:{path}", labels[0], labels[1]);
    Some(SettingField::new(key, label, FieldKind::Choice { options: labels.iter().map(|s| s.to_string()).collect(), enabled: vec![true, true], selected: Some(on as usize) }, row))
}

/// A bool property as a `Switch-Large` toggle (`invert`: the button shows the negation, e.g.
/// BYPASS over a `…Power` property).
fn toggle(src: &dyn PropertySource, path: &str, label: &str, invert: bool, row: u8) -> Option<SettingField> {
    let on = bool_at(src, path)? ^ invert;
    let key = if invert { format!("not:{path}") } else { format!("toggle:{path}") };
    Some(SettingField::new(key, label, FieldKind::Toggle { on }, row))
}

/// A fixed label ↔ value list over `path` (`map:<name>:<path>`).
fn mapped(src: &dyn PropertySource, name: &str, path: &str, label: &str, row: u8) -> Option<SettingField> {
    let cur = src.prop(path)?;
    let table = map_table(name);
    let selected = table.iter().position(|(_, v)| v.same(&cur));
    let options = table.iter().map(|(l, _)| l.to_string()).collect::<Vec<_>>();
    let enabled = vec![true; options.len()];
    Some(SettingField::new(format!("map:{name}:{path}"), label, FieldKind::Choice { options, enabled, selected }, row))
}

/// Fixed option tables for properties whose engine enum is not what Console shows.
fn map_table(name: &str) -> Vec<(&'static str, PropValue)> {
    match name {
        // HEADROOM (`Enable24dBMode`): Rack items "+20 dBu" (1) / "+24 dBu" (2).
        "headroom" => vec![("+20 dBu", PropValue::Bool(false)), ("+24 dBu", PropValue::Bool(true))],
        // DANTE LATENCY CONTROL (`DanteDeviceLatency`, µs 0…5000): Manual / 1 / 2 / 5 msec. [I] values.
        "dante_latency" => vec![("Manual", PropValue::Int(0)), ("1 msec", PropValue::Int(1000)), ("2 msec", PropValue::Int(2000)), ("5 msec", PropValue::Int(5000))],
        // DISPLAY › METERING (`PostFaderMetering`).
        "metering" => vec![("PRE-FADER", PropValue::Bool(false)), ("POST-FADER", PropValue::Bool(true))],
        // OUTPUT REFERENCE LEVELS (`Pad`: true = −10 dBV, false = +4 dBu, MAP.md "I/O and
        // outputs"); Rack's order (0x180e5a9d0, capture s24).
        "ref_level" => vec![("-10 dBV", PropValue::Bool(true)), ("+4 dBu", PropValue::Bool(false))],
        // MON LEVEL (monitor output `Pad`): "14 dBu" / "20 dBu" (0x180e5a9d0). [I] Pad ↔ 14 dBu.
        "mon_level" => vec![("14 dBu", PropValue::Bool(true)), ("20 dBu", PropValue::Bool(false))],
        // I/O MATRIX › MODE (`IOMapUsingDefault`; capture s39). [I] the binding.
        "io_mode" => vec![("Default", PropValue::Bool(true)), ("Custom", PropValue::Bool(false))],
        // I/O MATRIX › # INPUTS / # OUTPUTS (captures s36 / s37). [I] the binding.
        "io_count" => vec![("8", PropValue::Int(8)), ("16", PropValue::Int(16)), ("32", PropValue::Int(32)), ("64 (PT Mode)", PropValue::Int(64)), ("128", PropValue::Int(128))],
        // CUE OUTPUTS MIX (`MixInSource`): lit = the monitor mix ("mon"; capture s07 shows MIX lit
        // with "MONITOR MIX to A HP n"), else the cue's own mix.
        "cue_source" => vec![("MIX", PropValue::Str("mon".into())), ("CUE", PropValue::Str("cue".into()))],
        // MONITOR CONTROLLER MON OUTPUT GAIN (`ControlRoomPGABypass`). [I]
        "pga_bypass" => vec![("VARIABLE", PropValue::Bool(false)), ("FIXED", PropValue::Bool(true))],
        _ => Vec::new(),
    }
}

/// NETWORK INTERFACE: the helper's adapters (`en0  (192.0.2.4)`), the selected one ticked.
/// Picking one asks [`NIC_CONFIRM_TITLE`] first (the view), then writes the helper (`nic-set:`).
fn network_interface_field(src: &dyn PropertySource) -> SettingField {
    let adapters = src.choices(&format!("{HELPER_PREFIX}{NIC_LIST_PATH}"));
    let cur = src.prop(&format!("{HELPER_PREFIX}{NIC_PATH}"));
    let selected = cur.as_ref().and_then(|c| adapters.iter().position(|a| a.value.same(c)));
    let (options, enabled) = adapters.iter().map(|c| (c.label.clone(), c.enabled)).unzip();
    let mut f = SettingField::new(format!("nic:{NIC_PATH}"), "NETWORK INTERFACE", FieldKind::Choice { options, enabled, selected }, 0);
    f.display = match selected {
        Some(i) => Some(adapters[i].label.clone()),
        None => cur.map(|c| c.display()).filter(|s| !s.is_empty()),
    };
    f
}

fn pref_text(src: &dyn PropertySource, name: &str, label: &str, row: u8) -> SettingField {
    let cur = src.prop(&format!("pref:{name}")).map(|v| v.display()).unwrap_or_default();
    SettingField::new(format!("pref:{name}"), label, FieldKind::Text(cur), row)
}

fn pref_choice(src: &dyn PropertySource, name: &str, label: &str, row: u8) -> SettingField {
    let cur = src.prop(&format!("pref:{name}"));
    let choices = Prefs::choices(name);
    let selected = cur.and_then(|c| choices.iter().position(|x| x.value.same(&c)));
    let (options, enabled) = choices.iter().map(|c| (c.label.clone(), c.enabled)).unzip();
    SettingField::new(format!("pref:{name}"), label, FieldKind::Choice { options, enabled, selected }, row)
}

/// Device type with `+24 dBu` headroom (Apollo x Gen 2). [I] Rack asks a per-device capability
/// (0x180e2a9f0); the engine root carries `Enable24dBMode` for every device.
fn supports_headroom(type_id: &str) -> bool {
    type_id.contains("Gen 2") && !type_id.contains("Twin") && !type_id.contains("x4")
}

// ---- Groups ----

/// The tabs Console shows for this engine (`Rack_SettingsHeader::UpdateTabs`, Apollo type).
pub fn tabs(src: &dyn PropertySource) -> Vec<SettingsTab> {
    // Console 1.3.1 on an Apollo (captures s11…s17): HARDWARE, I/O MATRIX, OPTIONS, MIDI,
    // NETWORK. PLUG-INS, CONTROLLERS and DANTE DEVICES are LUNA's.
    let mut t = vec![SettingsTab::Hardware];
    // I/O MATRIX when the mixer supports I/O mapping (`IOMapPreset`).
    if src.prop("/IOMapPreset").is_some() || src.prop("/TotalDriverInputRoutes").is_some() {
        t.push(SettingsTab::IoMatrix);
    }
    t.push(SettingsTab::Options);
    if src.prop("/MidiInputDevice").is_some() {
        t.push(SettingsTab::Midi);
    }
    t.push(SettingsTab::Network);
    t
}

/// HARDWARE › DEVICES, in unit order (`MultiUnitOrder`, then index).
pub fn devices(src: &dyn PropertySource) -> Vec<SettingsDevice> {
    let mut out: Vec<SettingsDevice> = src
        .indices("/devices")
        .into_iter()
        .map(|i| {
            let p = format!("/devices/{i}");
            let order = src.prop(&format!("{p}/MultiUnitOrder")).and_then(|v| v.as_i64()).unwrap_or(i as i64);
            SettingsDevice {
                name: str_at(src, &format!("{p}/DeviceName")).unwrap_or_else(|| format!("Device {}", i + 1)),
                type_id: str_at(src, &format!("{p}/DeviceTypeID")).unwrap_or_default(),
                online: bool_at(src, &format!("{p}/DeviceOnline")).unwrap_or(true),
                slot: order.clamp(0, 3) as u8,
                index: i as u8,
                path: p,
            }
        })
        .collect();
    out.sort_by_key(|d| (d.slot, d.index));
    out
}

/// The I/O MATRIX route list (`/inputroutes` or `/outputroutes`).
pub fn io_routes(src: &dyn PropertySource, outputs: bool) -> Vec<IoRoute> {
    let node = if outputs { "/outputroutes" } else { "/inputroutes" };
    let mut v: Vec<IoRoute> = src
        .indices(node)
        .into_iter()
        .map(|i| {
            let p = format!("{node}/{i}");
            IoRoute {
                index: i,
                channel: src.prop(&format!("{p}/ChannelIndex")).and_then(|v| v.as_i64()).unwrap_or(i as i64 + 1),
                name: str_at(src, &format!("{p}/Name")).unwrap_or_default(),
                default_name: src.default_value(&format!("{p}/Name")).map(|v| v.display()).or_else(|| str_at(src, &format!("{p}/Name"))).unwrap_or_default(),
                io_type: str_at(src, &format!("{p}/IOType")).unwrap_or_default(),
                active: bool_at(src, &format!("{p}/Active")).unwrap_or(true),
            }
        })
        .collect();
    // Console lists the routes in engine order and numbers them 1…n (capture s12).
    v.sort_by_key(|r| r.index);
    for (i, r) in v.iter_mut().enumerate() {
        r.channel = i as i64 + 1;
    }
    v
}

/// Plug-in scene names (`/pluginscenes/0/scenes/N/SceneName`) in index order.
pub fn scene_names(src: &dyn PropertySource) -> Vec<String> {
    src.indices("/pluginscenes/0/scenes").into_iter().map(|n| str_at(src, &format!("/pluginscenes/0/scenes/{n}/SceneName")).unwrap_or_default()).collect()
}

/// The fields of `group`, in Console's order. Fields Console hides for this hardware are left
/// out; disabled ones stay with `enabled` false.
pub fn fields(src: &dyn PropertySource, group: SettingsGroup) -> Vec<SettingField> {
    let mut f: Vec<SettingField> = Vec::new();
    let devs = device_paths(src);
    let dev = |d: u8| format!("/devices/{d}");
    match group {
        SettingsGroup::Hardware => {
            let any = |prop: &str| devs.iter().any(|d| bool_at(src, &format!("{d}/{prop}")) == Some(true));
            // Windows engines carry `/BufferSize`; the Mac 1.3.1 window (capture s11) shows
            // neither BUFFER SIZE nor PT MODE. [I] PT MODE follows `/BufferSize`.
            let windows = src.prop("/BufferSize").is_some();
            // Row 0 (y 10): SAMPLE RATE, CLOCK SOURCE, BUFFER SIZE, USB SAFEGUARD, DIGITAL
            // MIRROR, DANTE LATENCY CONTROL.
            f.extend(engine_choice(src, "/SampleRate", "SAMPLE RATE", 0));
            f.extend(engine_choice(src, "/ClockSource", "CLOCK SOURCE", 0));
            if windows {
                f.extend(engine_choice(src, "/BufferSize", "BUFFER SIZE", 0));
            }
            f.extend(engine_choice(src, "/USBStreamingMode", "USB SAFEGUARD", 0));
            if let Some(mon) = devs.iter().find_map(|d| monitor_output(src, d)) {
                if bool_at(src, &format!("{mon}/DigitalMirrorAvailable")) == Some(true) {
                    f.extend(bool_choice(src, &format!("{mon}/MirrorsToDigital"), "DIGITAL MIRROR", ["OFF", "ON"], 0));
                }
            }
            if any("SupportsDante") {
                f.extend(mapped(src, "dante_latency", "/DanteDeviceLatency", "DANTE LATENCY CONTROL", 0));
            }
            // Row 1 (y 78): INPUT DELAY COMPENSATION, PT MODE, CUE BUS COUNT, ALT COUNT, FCN.
            f.extend(engine_choice(src, "/MaxDelayComp", "INPUT DELAY COMPENSATION", 1));
            if windows {
                f.extend(bool_choice(src, "/PTMode", "PT MODE", ["OFF", "ON"], 1));
            }
            // CUE BUS COUNT offers 2…4 (capture s20).
            f.extend(engine_choice_with(src, "/CueBusCount", "CUE BUS COUNT", 1, true, |c| c.value.as_i64().is_some_and(|n| n >= 2)));
            let max_alt = devs.iter().filter_map(|d| src.prop(&format!("{d}/MaxAltMons")).and_then(|v| v.as_i64())).max().unwrap_or(0);
            if max_alt > 0 {
                if let Some(d) = devs.first() {
                    let n = alt_count(src, d);
                    let options: Vec<String> = (0..=max_alt).map(|k| k.to_string()).collect();
                    let enabled = vec![true; options.len()];
                    f.push(SettingField::new(format!("alt:{d}"), "ALT COUNT", FieldKind::Choice { options, enabled, selected: Some(n.min(max_alt as usize)) }, 1));
                }
            }
            for d in &devs {
                f.extend(engine_choice(src, &format!("{d}/FuncSwitchMode"), "FCN SWITCH ASSIGN", 1));
                f.extend(engine_choice(src, &format!("{d}/FuncSwitch2Mode"), "FCN 2 SWITCH ASSIGN", 1));
            }
            // Row 2 (y 146): HEADROOM, MONITOR MODE, MADI CHANNELS.
            if devs.iter().any(|d| supports_headroom(&str_at(src, &format!("{d}/DeviceTypeID")).unwrap_or_default())) {
                f.extend(mapped(src, "headroom", "/Enable24dBMode", "HEADROOM", 2));
            }
            for d in &devs {
                if any("SupportsBassManagement") && str_at(src, &format!("{d}/DeviceTypeID")).is_some_and(|t| t.contains("x16") || t.contains("x8")) {
                    f.extend(engine_choice(src, &format!("{d}/SurroundMonitorMode"), "MONITOR MODE", 2));
                }
            }
            let madi = ["/inputroutes", "/outputroutes"].iter().any(|n| src.indices(n).into_iter().any(|i| str_at(src, &format!("{n}/{i}/IOType")).as_deref() == Some("MADI")));
            if madi {
                f.extend(engine_choice(src, "/FirstMADIChannelBlockIndex", "MADI CHANNELS", 2));
            }
        }
        SettingsGroup::Device(d) => {
            // `Rack_DeviceOptions` (layout: 0x180e5c400). Rows: 0 name, 1 digital / MON LEVEL,
            // 2 OUTPUT REFERENCE LEVELS, 3 LINE INPUT GAIN, 4 CHANNEL DSP PAIRING, 5 Dante.
            let p = dev(d);
            if src.prop(&format!("{p}/DeviceName")).is_none() {
                return f;
            }
            let name = str_at(src, &format!("{p}/DeviceName")).unwrap_or_default();
            let online = bool_at(src, &format!("{p}/DeviceOnline")).unwrap_or(true);
            // IDENTIFY for a connected unit, REMOVE for an offline one.
            let button = if online {
                SettingField::new(format!("post:Identify:{p}"), "IDENTIFY", FieldKind::Button, 0)
            } else {
                SettingField::new(format!("post:RemoveDevice:{p}"), "REMOVE", FieldKind::Button, 0)
            };
            f.push(SettingField::new(format!("{p}/DeviceName"), "DEVICE NAME", FieldKind::Text(name), 0).with_inline(button));
            f.extend(engine_choice(src, &format!("{p}/TOSLink"), "DIGITAL INPUT", 1));
            f.extend(engine_choice(src, &format!("{p}/TOSLinkOutput"), "DIGITAL OUTPUT", 1));
            if let Some(mon) = monitor_output(src, &p) {
                if src.prop(&format!("{mon}/Pad")).is_some() {
                    f.extend(mapped(src, "mon_level", &format!("{mon}/Pad"), "MON LEVEL", 1));
                }
            }
            // OUTPUT REFERENCE LEVELS, one combo per line-output pair ("1-2", "3-4").
            let lines: Vec<String> = outputs_of(src, &p, "Line").into_iter().filter(|o| src.prop(&format!("{o}/Pad")).is_some()).collect();
            for (k, pair) in lines.chunks(2).enumerate() {
                let label = format!("{}-{}", 2 * k + 1, 2 * k + 2);
                let cur = src.prop(&format!("{}/Pad", pair[0]));
                let table = map_table("ref_level");
                let selected = cur.and_then(|c| table.iter().position(|(_, v)| v.same(&c)));
                let options: Vec<String> = table.iter().map(|(l, _)| l.to_string()).collect();
                let paths: Vec<String> = pair.iter().map(|o| format!("{o}/Pad")).collect();
                f.push(SettingField::new(format!("pair:ref_level:{}", paths.join(",")), label, FieldKind::Choice { enabled: vec![true; options.len()], options, selected }, 2));
            }
            // LINE INPUT GAIN: line inputs (no preamp) with a `Pad`. [I]
            for i in src.indices(&format!("{p}/inputs")) {
                let ip = format!("{p}/inputs/{i}");
                if str_at(src, &format!("{ip}/IOType")).as_deref() == Some("Line") && src.prop(&format!("{ip}/Pad")).is_some() {
                    let label = str_at(src, &format!("{ip}/Name")).unwrap_or_default();
                    f.extend(mapped(src, "ref_level", &format!("{ip}/Pad"), &label, 3));
                }
            }
            // CHANNEL DSP PAIRING: DSP PAIRS ↔ VIRTUAL CHANNELS (`MaxSpans`, `ActiveSpans`).
            // [I] left = MaxSpans − ActiveSpans, right = ActiveSpans; one cell per position.
            if bool_at(src, &format!("{p}/DSPSpanning")).is_some() {
                let spans = src.prop(&format!("{p}/ActiveSpans")).and_then(|v| v.as_i64()).unwrap_or(0).max(0);
                let max = src.prop(&format!("{p}/MaxSpans")).and_then(|v| v.as_i64()).unwrap_or(4).max(1);
                f.push(SettingField::new(
                    format!("{p}/ActiveSpans"),
                    "CHANNEL DSP PAIRING",
                    FieldKind::Slider { segments: (max + 1) as u8, lit: spans.min(max) as u8, left: (max - spans.min(max)).to_string(), right: spans.to_string(), left_label: "DSP PAIRS".into(), right_label: "VIRTUAL CHANNELS".into() },
                    4,
                ));
            }
            if bool_at(src, &format!("{p}/SupportsDante")) == Some(true) {
                let dn = str_at(src, &format!("{p}/DanteDeviceName")).unwrap_or_default();
                f.push(SettingField::new(format!("{p}/DanteDeviceName"), "DANTE DEVICE NAME", FieldKind::Readout(dn), 5));
            }
        }
        SettingsGroup::IoMatrix => {
            // `Rack_ApolloIOMatrixSettings` (capture s12): MODE 90, I/O PRESETS 210 (+ SAVE),
            // # INPUTS 90, # OUTPUTS 90, EDIT 90 (CASCADE).
            f.extend(mapped(src, "io_mode", "/IOMapUsingDefault", "MODE", 0).map(|x| x.width(90.0)));
            if let Some(mut preset) = engine_choice_with(src, "/IOMapPreset", "I/O PRESETS", 0, false, |_| true) {
                preset.key = "iopreset:/IOMapPreset".into();
                // No preset: "Automatic" in Default mode (capture s12), "(Untitled)" in Custom
                // (drive capture set-tab-io).
                if str_at(src, "/IOMapPreset").is_none_or(|v| v.is_empty()) {
                    let default = src.prop("/IOMapUsingDefault").and_then(|v| v.as_bool()).unwrap_or(true);
                    preset.display = Some(if default { "Automatic" } else { "(Untitled)" }.into());
                }
                let name = str_at(src, "/IOMapPreset").filter(|s| !s.is_empty()).unwrap_or_else(|| "Custom".into());
                f.push(preset.width(210.0).with_inline(SettingField::new(format!("post:SaveIOMapPreset:{name}"), "SAVE", FieldKind::Button, 0)));
            }
            for (path, label) in [("/TotalDriverInputRoutes", "# INPUTS"), ("/TotalDriverOutputRoutes", "# OUTPUTS")] {
                if let Some(mut c) = mapped(src, "io_count", path, label, 0) {
                    c.display = src.prop(path).map(|v| v.display());
                    f.push(c.width(90.0));
                }
            }
            f.push(SettingField::new("launch:io_cascade", "EDIT", FieldKind::Action("CASCADE".into()), 0).width(90.0));
        }
        SettingsGroup::Display => {
            // `Rack_DisplaySettings` (capture s15).
            f.extend(mapped(src, "metering", "/PostFaderMetering", "METERING", 0));
            f.extend(engine_choice(src, "/ClipHold", "CLIP HOLD", 0));
            f.extend(engine_choice(src, "/PeakHold", "PEAK HOLD", 0));
            // RESET is live only while some dialog is suppressed (disabled in capture s15).
            let suppressed = bool_at(src, "pref:dont_show_again").unwrap_or(false);
            f.push(SettingField::new("pref-reset:dont_show_again", "\"DON'T SHOW AGAIN\" DIALOGS", FieldKind::Action("RESET".into()), 0).disabled(!suppressed));
            f.push(pref_text(src, "modifiers_timeout", "MODIFIERS TIMEOUT", 1));
        }
        SettingsGroup::Editing => {
            f.extend(engine_choice(src, "/ControlsMode", "CONTROLS MODE", 0));
        }
        SettingsGroup::Midi => {
            // `Rack_MIDISettings_Apollo` (capture s16): DEVICE; TAP TEMPO CHANNEL / EVENT (+ note
            // number); PLUG-IN SCENES CHANNEL / EVENT under them.
            let mut dev = engine_choice_with(src, "/MidiInputDevice", "DEVICE", 0, false, |_| true);
            if let Some(d) = &mut dev {
                d.key = "midi:/MidiInputDevice".into();
                if str_at(src, "/MidiInputDevice").is_none_or(|s| s.is_empty()) {
                    d.display = Some(String::new());
                }
            }
            f.extend(dev.map(|d| d.at(0.0, 0.0)));
            // Cells 164 apart, rows 60 apart (capture s16).
            f.push(pref_text(src, "tap_tempo_channel", "TAP TEMPO CHANNEL", 0).at(164.0, 0.0));
            let number = pref_text(src, "tap_tempo_number", "", 0);
            f.push(pref_choice(src, "tap_tempo_event", "TAP TEMPO EVENT", 0).at(328.0, 0.0).with_inline(number));
            f.push(pref_text(src, "scenes_channel", "PLUG-IN SCENES CHANNEL", 1).at(164.0, 60.0));
            f.push(pref_choice(src, "scenes_event", "PLUG-IN SCENES EVENT", 1).at(328.0, 60.0));
        }
        SettingsGroup::Network => {
            // `Rack_NetworkSettings` / Dante (captures s17, s30): the helper's adapters
            // (`helper:/dante/available_network_adapters`) and choice (`…/primary_network_adapter`).
            f.push(network_interface_field(src));
            f.push(SettingField::new("launch:dante_controller", "DANTE CONTROLLER", FieldKind::Action("LAUNCH".into()), 0));
            f.push(SettingField::new("launch:dante_patents", "DANTE PATENT INFORMATION", FieldKind::Action("MORE INFO".into()), 0));
            f.push(SettingField::new("launch:dante_discovery", "DANTE DISCOVERY", FieldKind::Action("REFRESH".into()), 0));
        }
        SettingsGroup::PlugIns => {
            let plugins = src.indices("/plugins");
            let authorized = plugins.iter().filter(|i| bool_at(src, &format!("/plugins/{i}/Authorized")) == Some(true)).count();
            let count = |n: usize| if n == 1 { "1 plugin".to_string() } else { format!("{n} plugins") };
            f.push(SettingField::new("plugins:uad2", "UAD-2", FieldKind::Readout(count(plugins.len())), 0));
            f.push(SettingField::new("plugins:authorized", "AUTHORIZED", FieldKind::Readout(count(authorized)), 0));
            f.push(SettingField::new("post:UpdatePlugInStatus:", "RESCAN", FieldKind::Button, 0));
            let short = bool_at(src, "pref:short_names").unwrap_or(false);
            f.push(SettingField::new("pref-toggle:short_names", "SHORT NAMES", FieldKind::Toggle { on: short }, 0));
        }
        SettingsGroup::CueRoutes(d) => {
            let p = dev(d);
            for (k, o) in outputs_of(src, &p, "Cue").into_iter().filter(|o| bool_at(src, &format!("{o}/Active")).unwrap_or(true)).enumerate() {
                let row = k as u8;
                let name = str_at(src, &format!("{o}/Name")).unwrap_or_default();
                f.push(SettingField::new(format!("{o}/Name"), name, FieldKind::Readout(String::new()), row));
                f.extend(mapped(src, "cue_source", &format!("{o}/MixInSource"), "SOURCE", row));
                f.extend(toggle(src, &format!("{o}/MixToMono"), "MONO", false, row));
                f.extend(engine_choice(src, &format!("{o}/OutputDestination"), "MIRROR TO", row));
            }
        }
        SettingsGroup::MonitorKnob(d) => {
            let p = dev(d);
            f.extend(engine_choice(src, &format!("{p}/SurroundMonitorMode"), "MONITOR MODE", 0));
            if let Some(mon) = monitor_output(src, &p) {
                let lvl = src.prop(&format!("{mon}/CRMonitorLevel")).and_then(|v| v.as_f64()).unwrap_or(-96.0);
                f.push(SettingField::new(format!("{mon}/CRMonitorLevel"), "LEVEL", FieldKind::Readout(if lvl <= -95.9 { "-INF".into() } else { format!("{lvl:.1} dB") }), 0));
            }
        }
        SettingsGroup::BassManagement(d) => {
            // `Rack_BassManagement` (capture s06): ENABLE; LFE FILTER, SATELLITE FILTER; CHANNEL,
            // SATELLITE CROSSOVER. In stereo MONITOR MODE every combo reads N/A.
            let p = dev(d);
            if bool_at(src, &format!("{p}/SupportsBassManagement")) != Some(true) {
                return f;
            }
            let mon = monitor_output(src, &p);
            let stereo = str_at(src, &format!("{p}/SurroundMonitorMode")).is_none_or(|m| m.to_lowercase().contains("stereo"));
            let na = |key: String, label: &str, row: u8| {
                let mut x = SettingField::new(key, label, FieldKind::Choice { options: Vec::new(), enabled: Vec::new(), selected: None }, row).disabled(true);
                x.display = Some("N/A".into());
                x
            };
            f.extend(toggle(src, &format!("{p}/BassMgrEnable"), "ENABLE", false, 0));
            let lfe = mon.as_ref().map(|m| format!("{m}/BassMgrLFEFilterType")).unwrap_or_default();
            let sat = format!("{p}/BassMgrXOverFilterType");
            let freq = mon.as_ref().map(|m| format!("{m}/BassMgrXOverFreq")).unwrap_or_default();
            if stereo {
                f.push(na(lfe, "LFE FILTER", 0));
                f.push(na(sat, "SATELLITE FILTER", 0));
                f.push(na(format!("{p}/BassMgrChannel"), "CHANNEL", 1));
                f.push(na(freq, "SATELLITE CROSSOVER", 1));
            } else {
                f.push(engine_choice(src, &lfe, "LFE FILTER", 0).unwrap_or_else(|| na(lfe.clone(), "LFE FILTER", 0)));
                f.push(engine_choice(src, &sat, "SATELLITE FILTER", 0).unwrap_or_else(|| na(sat.clone(), "SATELLITE FILTER", 0)));
                f.push(na(format!("{p}/BassMgrChannel"), "CHANNEL", 1));
                let hz = src.prop(&freq).and_then(|v| v.as_f64()).map(|v| format!("{v:.0} Hz")).unwrap_or_else(|| "N/A".into());
                let mut x = na(freq, "SATELLITE CROSSOVER", 1);
                x.display = Some(hz);
                f.push(x);
            }
        }
        SettingsGroup::SpeakerUtilities(d) => {
            let p = dev(d);
            for n in src.indices(&format!("{p}/outputs")) {
                let o = format!("{p}/outputs/{n}");
                if src.prop(&format!("{o}/SurroundSolo")).is_none() {
                    continue;
                }
                let name = str_at(src, &format!("{o}/Name")).unwrap_or_default();
                let row = f.len() as u8 / 2;
                f.extend(toggle(src, &format!("{o}/SurroundSolo"), &format!("SOLO {name}"), false, row));
                f.extend(toggle(src, &format!("{o}/SurroundMute"), &format!("MUTE {name}"), false, row));
            }
            let row = f.len() as u8 / 2 + 1;
            f.push(pref_choice(src, "monitor_level_display", "MONITOR LEVEL DISPLAY", row));
        }
        SettingsGroup::RoomCorrection(d) => {
            let p = dev(d);
            if bool_at(src, &format!("{p}/SupportsSpeakerCorrection")) != Some(true) {
                return f;
            }
            let name = str_at(src, &format!("{p}/SpeakerCorrectionName")).filter(|s| !s.is_empty()).unwrap_or_else(|| "<PROFILE>".into());
            f.push(SettingField::new(format!("{p}/SpeakerCorrectionName"), "CALIBRATION PROFILE", FieldKind::Readout(name), 0));
            f.extend(toggle(src, &format!("{p}/SpeakerCorrectionPower"), "BYPASS", true, 1));
            f.extend(toggle(src, &format!("{p}/UseSpeakerCorrectionSafeHeadroom"), "SAFE HEADROOM", false, 1));
            if let Some(h) = src.prop(&format!("{p}/SpeakerCorrectionSafeHeadroom")).and_then(|v| v.as_f64()) {
                f.push(SettingField::new(format!("{p}/SpeakerCorrectionSafeHeadroom"), "HEADROOM", FieldKind::Readout(format!("{h:.1} dB")), 1));
            }
            f.push(SettingField::new("post:ShowSoundIDReference:speaker", "SOUNDID REFERENCE", FieldKind::Action("OPEN".into()), 2));
        }
        SettingsGroup::HeadphoneCorrection { device, hp } => {
            let p = dev(device);
            let (supports, pre) = if hp == 0 { ("SupportsHeadphoneCorrection", "Headphone") } else { ("SupportsHeadphone2Correction", "Headphone2") };
            if bool_at(src, &format!("{p}/{supports}")) != Some(true) {
                return f;
            }
            let name = str_at(src, &format!("{p}/{pre}CorrectionName")).filter(|s| !s.is_empty()).unwrap_or_else(|| "<PROFILE>".into());
            f.push(SettingField::new(format!("{p}/{pre}CorrectionName"), "CALIBRATION PROFILE", FieldKind::Readout(name), 0));
            f.extend(toggle(src, &format!("{p}/{pre}CorrectionEnable"), "ENABLE", false, 1));
            f.extend(toggle(src, &format!("{p}/{pre}CorrectionPower"), "BYPASS", true, 1));
            f.extend(toggle(src, &format!("{p}/Use{pre}CorrectionSafeHeadroom"), "SAFE HEADROOM", false, 1));
            let ty = if hp == 0 { "headphone" } else { "headphone2" };
            f.push(SettingField::new(format!("post:ShowSoundIDReference:{ty}"), "SOUNDID REFERENCE", FieldKind::Action("OPEN".into()), 2));
        }
        SettingsGroup::Dante => {
            for (k, d) in devs.iter().enumerate() {
                if bool_at(src, &format!("{d}/SupportsDante")) != Some(true) {
                    continue;
                }
                let row = k as u8;
                let s = |prop: &str| str_at(src, &format!("{d}/{prop}")).unwrap_or_default();
                f.push(SettingField::new(format!("{d}/DanteDeviceName"), "DANTE DEVICE NAME", FieldKind::Readout(s("DanteDeviceName")), row));
                f.push(SettingField::new(format!("{d}/DantePrimaryIPAddress"), "PRIMARY IP", FieldKind::Readout(s("DantePrimaryIPAddress")), row));
                f.push(SettingField::new(format!("{d}/DanteVersion"), "VERSION", FieldKind::Readout(s("DanteVersion")), row));
                f.push(SettingField::new(format!("post:ResetDanteNetwork:{d}"), "RESET", FieldKind::Button, row));
            }
        }
    }
    f
}

/// ALT COUNT: the number of line-output pairs with `AltMonEnabled`. [I] pair k (0-based)
/// is the k-th pair of outputs that carry `AltMonEnabled`.
fn alt_capable(src: &dyn PropertySource, dev: &str) -> Vec<String> {
    src.indices(&format!("{dev}/outputs")).into_iter().map(|n| format!("{dev}/outputs/{n}")).filter(|o| src.prop(&format!("{o}/AltMonEnabled")).is_some()).collect()
}

fn alt_count(src: &dyn PropertySource, dev: &str) -> usize {
    alt_capable(src, dev).chunks(2).filter(|pair| pair.iter().any(|o| bool_at(src, &format!("{o}/AltMonEnabled")) == Some(true))).count()
}

/// The writes `op` on the field bound to `key` makes. Unknown keys and out-of-range options
/// write nothing.
pub fn resolve(src: &dyn PropertySource, key: &str, op: &SettingOp) -> Vec<Write> {
    let set = |path: &str, v: PropValue| vec![Write::Set(path.to_string(), v)];
    let (kind, rest) = match key.split_once(':') {
        Some((k, r)) if !key.starts_with('/') => (k, r),
        _ => ("", key),
    };
    match (kind, op) {
        // A plain engine path: enum choice or text.
        ("", SettingOp::Choose(i)) => src.choices(rest).get(*i).filter(|c| c.enabled).map(|c| set(rest, c.value.clone())).unwrap_or_default(),
        ("", SettingOp::Text(t)) => {
            let t = t.trim();
            if t.is_empty() { Vec::new() } else { set(rest, PropValue::Str(t.to_string())) }
        }
        ("bool", SettingOp::Choose(i)) => {
            // bool:<off label>:<on label>:<path>
            let path = rest.splitn(3, ':').nth(2).unwrap_or_default();
            if *i > 1 || path.is_empty() { Vec::new() } else { set(path, PropValue::Bool(*i == 1)) }
        }
        ("toggle", SettingOp::Press) => bool_at(src, rest).map(|on| set(rest, PropValue::Bool(!on))).unwrap_or_default(),
        ("not", SettingOp::Press) => bool_at(src, rest).map(|on| set(rest, PropValue::Bool(!on))).unwrap_or_default(),
        ("map", SettingOp::Choose(i)) => {
            let (name, path) = rest.split_once(':').unwrap_or((rest, ""));
            map_table(name).get(*i).map(|(_, v)| set(path, v.clone())).unwrap_or_default()
        }
        ("alt", SettingOp::Choose(n)) => {
            let pairs: Vec<Vec<String>> = alt_capable(src, rest).chunks(2).map(|c| c.to_vec()).collect();
            pairs.iter().enumerate().flat_map(|(k, pair)| pair.iter().map(move |o| Write::Set(format!("{o}/AltMonEnabled"), PropValue::Bool(k < *n)))).collect()
        }
        ("iopreset", SettingOp::Choose(i)) => src.choices(rest).get(*i).map(|c| set(rest, c.value.clone())).unwrap_or_default(),
        ("midi", SettingOp::Choose(i)) => {
            if *i == 0 {
                set(rest, PropValue::Str(String::new()))
            } else {
                src.choices(rest).get(i - 1).map(|c| set(rest, c.value.clone())).unwrap_or_default()
            }
        }
        ("pref", SettingOp::Choose(i)) => Prefs::choices(rest).get(*i).map(|c| vec![Write::Pref(rest.to_string(), c.value.clone())]).unwrap_or_default(),
        ("pref", SettingOp::Text(t)) => vec![Write::Pref(rest.to_string(), PropValue::Str(t.trim().to_string()))],
        ("pair", SettingOp::Choose(i)) => {
            // pair:<table>:<path>,<path>: the same value to every path.
            let (name, paths) = rest.split_once(':').unwrap_or((rest, ""));
            map_table(name).get(*i).map(|(_, v)| paths.split(',').filter(|p| !p.is_empty()).map(|p| Write::Set(p.to_string(), v.clone())).collect()).unwrap_or_default()
        }
        ("pref-toggle", SettingOp::Press) => {
            let on = bool_at(src, &format!("pref:{rest}")).unwrap_or(false);
            vec![Write::Pref(rest.to_string(), PropValue::Bool(!on))]
        }
        ("pref-reset", SettingOp::Press) => vec![Write::Pref(rest.to_string(), PropValue::Bool(false))],
        // NETWORK INTERFACE: the view asks SWITCH NETWORK INTERFACE? first; YES sends `nic-set`.
        ("nic", _) => Vec::new(),
        ("nic-set", SettingOp::Choose(i)) => src
            .choices(&format!("{HELPER_PREFIX}{NIC_LIST_PATH}"))
            .get(*i)
            .map(|c| vec![Write::Helper(rest.to_string(), c.value.clone())])
            .unwrap_or_default(),
        ("fader", SettingOp::Value(v)) => set(rest, PropValue::Float(v.clamp(0.0, 1.0))),
        ("post-text", SettingOp::Text(t)) => {
            // post-text:<Command>[:<orig>]: SAVE AS / RENAME of an I/O preset.
            let t = t.trim();
            if t.is_empty() {
                return Vec::new();
            }
            let (cmd, orig) = rest.split_once(':').unwrap_or((rest, ""));
            let payload = match cmd {
                "RenameIOMapPreset" => format!("{{\"newPreset\":{},\"origPreset\":{}}}", json_str(t), json_str(orig)),
                _ => format!("{{\"ioPresetName\":{}}}", json_str(t)),
            };
            vec![Write::Post(format!("/{cmd}"), payload)]
        }
        ("route", SettingOp::Choose(i)) => {
            // route:<node>:<IOType>: pick channel `i` of that type for the route: `IOType` +
            // `ChannelIndex` (1-based within the type; `DeviceHwID` only on multi-unit setups),
            // per UA Mixer Engine 12's route handlers (protocol/v131/MAP.md, "Routes").
            let mut it = rest.rsplitn(2, ':');
            let (ty, node) = (it.next().unwrap_or(""), it.next().unwrap_or(""));
            vec![Write::Set(format!("{node}/IOType"), PropValue::Str(ty.to_string())), Write::Set(format!("{node}/ChannelIndex"), PropValue::Int(*i as i64 + 1))]
        }
        ("post", SettingOp::Press) => {
            // post:<Command>:<arg>; payload shapes from the engine's root `commands` node.
            let (cmd, arg) = rest.split_once(':').unwrap_or((rest, ""));
            let payload = match cmd {
                "Identify" | "RemoveDevice" | "ResetDanteNetwork" => format!("{{\"path\":{}}}", json_str(arg)),
                "SaveIOMapPreset" | "DeleteIOMapPreset" => format!("{{\"ioPresetName\":{}}}", json_str(arg)),
                "AddDevice" => format!("{{\"type\":{}}}", json_str(arg)),
                "Import" => format!("{{\"type\":{}}}", json_str(arg)),
                "ShowSoundIDReference" => format!("{{\"type\":{}}}", json_str(arg)),
                _ => "{}".to_string(),
            };
            vec![Write::Post(format!("/{cmd}"), payload)]
        }
        _ => Vec::new(),
    }
}

fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Finds a field by key among every group a settings page or panel can show (the text editor
/// needs a field's current text from its key alone).
pub fn find_field(src: &dyn PropertySource, key: &str) -> Option<SettingField> {
    let mut groups = vec![SettingsGroup::Hardware, SettingsGroup::IoMatrix, SettingsGroup::Display, SettingsGroup::Editing, SettingsGroup::Midi, SettingsGroup::PlugIns, SettingsGroup::Dante, SettingsGroup::Network];
    for d in src.indices("/devices").into_iter().map(|d| d as u8) {
        groups.extend([
            SettingsGroup::Device(d),
            SettingsGroup::CueRoutes(d),
            SettingsGroup::MonitorKnob(d),
            SettingsGroup::BassManagement(d),
            SettingsGroup::SpeakerUtilities(d),
            SettingsGroup::RoomCorrection(d),
            SettingsGroup::HeadphoneCorrection { device: d, hp: 0 },
            SettingsGroup::HeadphoneCorrection { device: d, hp: 1 },
        ]);
    }
    groups.into_iter().flat_map(|g| fields(src, g)).flat_map(|f| {
        let inline = f.inline.as_deref().cloned();
        std::iter::once(f).chain(inline)
    }).find(|f| &*f.key == key)
}

// ---- In-memory properties (preview model, tests) ----

#[derive(Clone, Debug, PartialEq)]
struct MemProp {
    value: PropValue,
    choices: Vec<Choice>,
    range: Option<(f64, f64)>,
    readonly: bool,
}

/// A small property tree in memory: the preview model's settings and the tests' fixture.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MemoryProperties {
    props: BTreeMap<String, MemProp>,
    pub prefs: Prefs,
}

impl MemoryProperties {
    pub fn with(mut self, path: &str, value: PropValue) -> Self {
        self.insert(path, value, Vec::new());
        self
    }
    pub fn with_choices(mut self, path: &str, value: PropValue, choices: Vec<Choice>) -> Self {
        self.insert(path, value, choices);
        self
    }
    pub fn insert(&mut self, path: &str, value: PropValue, choices: Vec<Choice>) {
        self.props.insert(path.to_string(), MemProp { value, choices, range: None, readonly: false });
    }
    pub fn set_readonly(&mut self, path: &str) {
        if let Some(p) = self.props.get_mut(path) {
            p.readonly = true;
        }
    }
    /// Applies a write (`Post`s do nothing in memory).
    pub fn apply(&mut self, w: &Write) {
        match w {
            Write::Set(path, v) => {
                if let Some(p) = self.props.get_mut(path) {
                    p.value = v.clone();
                }
            }
            Write::Pref(name, v) => self.prefs.set(name, v.clone()),
            Write::Helper(path, v) => {
                if let Some(p) = self.props.get_mut(&format!("{HELPER_PREFIX}{path}")) {
                    p.value = v.clone();
                }
            }
            Write::Post(..) => {}
        }
    }

    /// An Apollo x4 shaped like the p2 reference engine (UA Mixer Engine 12.0.0, Console 1.3.1).
    pub fn demo_x4() -> Self {
        use PropValue::{Bool as B, Int as I, Str as S};
        let s = |v: &str| Choice::new(S(v.into()));
        let rate = |hz: i64, l: &str| Choice::labeled(I(hz), l);
        let mut m = MemoryProperties::default()
            .with_choices("/SampleRate", I(48000), vec![rate(44100, "44.1 kHz"), rate(48000, "48 kHz"), rate(88200, "88.2 kHz"), rate(96000, "96 kHz"), rate(176400, "176.4 kHz"), rate(192000, "192 kHz")])
            .with_choices("/ClockSource", S("Internal".into()), vec![s("Internal"), s("S/PDIF")])
            .with_choices("/MaxDelayComp", S("Medium".into()), ["Off", "Short", "Medium", "Medium-Long", "Long"].iter().map(|v| s(v)).collect())
            .with("/PTMode", B(false))
            .with_choices("/CueBusCount", I(2), (0..=4).map(|n| Choice::new(I(n))).collect())
            .with_choices("/ClipHold", S("3 SEC".into()), ["NONE", "1 SEC", "3 SEC", "5 SEC", "10 SEC", "INFINITE"].iter().map(|v| s(v)).collect())
            .with_choices("/PeakHold", S("3 SEC".into()), ["NONE", "1 SEC", "3 SEC", "5 SEC", "10 SEC"].iter().map(|v| s(v)).collect())
            .with("/PostFaderMetering", B(false))
            .with_choices("/ControlsMode", S("Linear".into()), ["Circular", "Relative Circular", "Linear"].iter().map(|v| s(v)).collect())
            .with("/Enable24dBMode", B(false))
            .with("/DanteDeviceLatency", I(0))
            .with("/FirstMADIChannelBlockIndex", I(0))
            .with_choices("/IOMapPreset", S(String::new()), ["Multi-A16 Legacy", "Multi-A16 PT Legacy", "Multi-Apollo PT", "Single-A16 Legacy"].iter().map(|v| s(v)).collect())
            .with("/IOMapUsingDefault", B(true))
            .with("/IOMapPresetDirty", B(true))
            .with("/TotalDriverInputRoutes", I(22))
            .with("/TotalDriverOutputRoutes", I(24))
            .with_choices("/MidiInputDevice", S(String::new()), vec![s("Network RTP MIDI Session"), s("Virtual Midi Bus 1")])
            .with("/Version", S("12.0.0".into()))
            .with("/ProtocolVersion", S("1.0.0".into()))
            .with("/SerialNumber", S("20000000000002".into()))
            .with("/devices/0/DeviceName", S("Apollo x4".into()))
            .with("/devices/0/DeviceTypeID", S("Apollo x4".into()))
            .with("/devices/0/DeviceOnline", B(true))
            .with("/devices/0/MultiUnitOrder", I(0))
            .with_choices("/devices/0/TOSLink", S("S/PDIF".into()), vec![s("S/PDIF"), s("ADAT")])
            .with_choices("/devices/0/TOSLinkOutput", S("S/PDIF".into()), vec![s("S/PDIF"), s("ADAT")])
            .with("/devices/0/MaxAltMons", I(2))
            .with("/devices/0/DSPSpanning", B(true))
            .with("/devices/0/ActiveSpans", I(0))
            .with("/devices/0/MaxSpans", I(4))
            .with("/devices/0/SupportsDante", B(false))
            .with("/devices/0/SupportsBassManagement", B(true))
            .with("/devices/0/BassMgrEnable", B(false))
            .with_choices("/devices/0/BassMgrXOverFilterType", S("LR24".into()), vec![s("LR24"), s("BW12")])
            .with("/devices/0/SupportsSpeakerCorrection", B(true))
            .with("/devices/0/SpeakerCorrectionName", S("2.0 (Stereo) studio - 2025-06-20".into()))
            .with("/devices/0/SpeakerCorrectionPower", B(true))
            .with("/devices/0/UseSpeakerCorrectionSafeHeadroom", B(true))
            .with("/devices/0/SpeakerCorrectionSafeHeadroom", PropValue::Float(-5.107))
            .with("/devices/0/SupportsHeadphoneCorrection", B(true))
            .with("/devices/0/SupportsHeadphone2Correction", B(false))
            .with("/devices/0/HeadphoneCorrectionName", S(String::new()))
            .with("/devices/0/HeadphoneCorrectionEnable", B(false))
            .with("/devices/0/HeadphoneCorrectionPower", B(true))
            .with("/devices/0/UseHeadphoneCorrectionSafeHeadroom", B(true));
        for (n, name) in ["ANALOG 1", "ANALOG 2", "ANALOG 3", "ANALOG 4"].iter().enumerate() {
            let o = format!("/devices/0/outputs/{n}");
            m.insert(&format!("{o}/Name"), S(name.to_string()), Vec::new());
            m.insert(&format!("{o}/IOType"), S("Line".into()), Vec::new());
            m.insert(&format!("{o}/Pad"), B(false), Vec::new());
            m.insert(&format!("{o}/AltMonEnabled"), B(false), Vec::new());
        }
        let dests: Vec<Choice> = ["None", "Line 1-2", "Line 3-4", "ADAT 1-2", "ADAT 3-4"].iter().enumerate().map(|(i, v)| Choice { value: S(v.to_string()), label: v.to_string(), enabled: i < 3, labeled: false }).collect();
        for (k, n) in [14, 15, 16, 17].iter().enumerate() {
            let o = format!("/devices/0/outputs/{n}");
            m.insert(&format!("{o}/Name"), S(format!("CUE {}", k + 1)), Vec::new());
            m.insert(&format!("{o}/IOType"), S("Cue".into()), Vec::new());
            m.insert(&format!("{o}/Active"), B(k < 2), Vec::new());
            m.insert(&format!("{o}/MixInSource"), S("mon".into()), vec![s("mon"), s("cue")]);
            m.insert(&format!("{o}/MixToMono"), B(false), Vec::new());
            m.insert(&format!("{o}/OutputDestination"), S("None".into()), dests.clone());
        }
        let mon = "/devices/0/outputs/18";
        for (p, v) in [
            ("Name", S("MONITOR".into())),
            ("IOType", S("Monitor".into())),
            ("Pad", B(false)),
            ("DigitalMirrorAvailable", B(true)),
            ("MirrorsToDigital", B(false)),
            ("CRMonitorLevel", PropValue::Float(-96.0)),
            ("BassMgrXOverFreq", PropValue::Float(80.0)),
            ("BassMgrHPOn", B(true)),
            ("BassMgrLPOn", B(true)),
            ("SurroundSolo", B(false)),
            ("SurroundMute", B(false)),
        ] {
            m.insert(&format!("{mon}/{p}"), v, Vec::new());
        }
        let r = "/devices/0/outputs/21";
        m.insert(&format!("{r}/Name"), S("MONITOR R FOR SURROUND".into()), Vec::new());
        m.insert(&format!("{r}/IOType"), S("Monitor".into()), Vec::new());
        m.insert(&format!("{r}/SurroundSolo"), B(false), Vec::new());
        m.insert(&format!("{r}/SurroundMute"), B(false), Vec::new());
        let routes_in = ["MIC/LINE/HIZ 1", "MIC/LINE/HIZ 2", "MIC/LINE 3", "MIC/LINE 4", "S/PDIF L", "S/PDIF R"];
        for (i, n) in routes_in.iter().enumerate() {
            m.insert(&format!("/inputroutes/{i}/ChannelIndex"), I(i as i64 + 1), Vec::new());
            m.insert(&format!("/inputroutes/{i}/Name"), S(n.to_string()), Vec::new());
            m.insert(&format!("/inputroutes/{i}/IOType"), S(if i < 2 { "MicLineHiZ" } else if i < 4 { "MicLine" } else { "SPDIF" }.into()), Vec::new());
        }
        let routes_out = ["MON L", "MON R", "LINE 1", "LINE 2", "LINE 3", "LINE 4"];
        for (i, n) in routes_out.iter().enumerate() {
            m.insert(&format!("/outputroutes/{i}/ChannelIndex"), I(i as i64 + 1), Vec::new());
            m.insert(&format!("/outputroutes/{i}/Name"), S(n.to_string()), Vec::new());
            m.insert(&format!("/outputroutes/{i}/IOType"), S(if i < 2 { "CtrlRoom" } else { "Line" }.into()), Vec::new());
        }
        // The talkback strip and the aux cue sends (p2: talkback → AUX 1 / 2 at −∞, CUE 1 / 2 at −6).
        m.insert("/devices/0/inputs/22/Name", S("TALKBACK".into()), Vec::new());
        m.insert("/devices/0/inputs/22/IOType", S("TalkbackMic".into()), Vec::new());
        for (k, (id, name, gain, tap)) in [("aux1", "AUX 1", -144.0, 0.0), ("aux2", "AUX 2", -144.0, 0.0), ("cue1", "CUE 1", -6.0, 0.6727), ("cue2", "CUE 2", -6.0, 0.6727)].iter().enumerate() {
            let p = format!("/devices/0/inputs/22/sends/{k}");
            m.insert(&format!("{p}/ID"), S(id.to_string()), Vec::new());
            m.insert(&format!("{p}/Name"), S(name.to_string()), Vec::new());
            m.insert(&format!("{p}/Gain"), PropValue::Float(*gain), Vec::new());
            m.insert(&format!("{p}/GainTapered"), PropValue::Float(*tap), Vec::new());
            m.insert(&format!("{p}/Bypass"), B(false), Vec::new());
        }
        for a in 0..2 {
            m.insert(&format!("/devices/0/auxs/{a}/Name"), S(format!("AUX {}", a + 1)), Vec::new());
            for c in 0..2 {
                let p = format!("/devices/0/auxs/{a}/sends/{c}");
                m.insert(&format!("{p}/ID"), S(format!("cue{}", c + 1)), Vec::new());
                m.insert(&format!("{p}/Name"), S(format!("CUE {}", c + 1)), Vec::new());
                m.insert(&format!("{p}/Gain"), PropValue::Float(-144.0), Vec::new());
                m.insert(&format!("{p}/GainTapered"), PropValue::Float(0.0), Vec::new());
                m.insert(&format!("{p}/Bypass"), B(false), Vec::new());
            }
        }
        for i in 0..3 {
            m.insert(&format!("/pluginscenes/0/scenes/{i}/SceneName"), S(["VERSE", "CHORUS", "EMPTY SCENE"][i].into()), Vec::new());
        }
        m.insert("/pluginscenes/0/RecalledSceneIndex", I(-1), Vec::new());
        m
    }
}

impl PropertySource for MemoryProperties {
    fn prop(&self, path: &str) -> Option<PropValue> {
        if let Some(name) = path.strip_prefix("pref:") {
            return self.prefs.get(name);
        }
        self.props.get(path).map(|p| p.value.clone())
    }
    fn choices(&self, path: &str) -> Vec<Choice> {
        self.props.get(path).map(|p| p.choices.clone()).unwrap_or_default()
    }
    fn indices(&self, node: &str) -> Vec<usize> {
        let prefix = format!("{}/", node.trim_end_matches('/'));
        let mut v: Vec<usize> = self.props.range(prefix.clone()..).take_while(|(k, _)| k.starts_with(&prefix)).filter_map(|(k, _)| k[prefix.len()..].split('/').next()?.parse().ok()).collect();
        v.sort_unstable();
        v.dedup();
        v
    }
    fn range(&self, path: &str) -> Option<(f64, f64)> {
        self.props.get(path).and_then(|p| p.range)
    }
    fn readonly(&self, path: &str) -> bool {
        self.props.get(path).is_some_and(|p| p.readonly)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(f: &[SettingField]) -> Vec<&str> {
        f.iter().map(|f| f.label.as_str()).collect()
    }

    #[test]
    fn hardware_fields_follow_console_for_an_x4() {
        let m = MemoryProperties::demo_x4();
        let f = fields(&m, SettingsGroup::Hardware);
        // Capture s11 (Mac, Apollo x4): no BUFFER SIZE / PT MODE (no `/BufferSize`), no USB
        // SAFEGUARD, Dante, HEADROOM or MADI.
        assert_eq!(labels(&f), ["SAMPLE RATE", "CLOCK SOURCE", "DIGITAL MIRROR", "INPUT DELAY COMPENSATION", "CUE BUS COUNT", "ALT COUNT"]);
        assert_eq!(f[0].value_text(), "48 kHz");
        assert_eq!(f[1].value_text(), "INTERNAL");
        assert_eq!(f[2].value_text(), "OFF");
        assert_eq!(f[3].value_text(), "MEDIUM");
        assert_eq!(f.iter().map(|f| f.row).collect::<Vec<_>>(), [0, 0, 0, 1, 1, 1]);
        // CUE BUS COUNT offers 2…4, ALT COUNT 0…2 (captures s19 / s20).
        let FieldKind::Choice { options, .. } = &f[4].kind else { panic!() };
        assert_eq!(options, &["2", "3", "4"]);
        let FieldKind::Choice { options, .. } = &f[5].kind else { panic!() };
        assert_eq!(options, &["0", "1", "2"]);
        assert_eq!(tabs(&m), [SettingsTab::Hardware, SettingsTab::IoMatrix, SettingsTab::Options, SettingsTab::Midi, SettingsTab::Network]);
    }

    #[test]
    fn writes_resolve_to_engine_paths() {
        let mut m = MemoryProperties::demo_x4();
        assert_eq!(resolve(&m, "/SampleRate", &SettingOp::Choose(3)), vec![Write::Set("/SampleRate".into(), PropValue::Int(96000))]);
        let mirror = find_field(&m, "bool:OFF:ON:/devices/0/outputs/18/MirrorsToDigital").expect("digital mirror");
        let w = resolve(&m, &mirror.key, &SettingOp::Choose(1));
        assert_eq!(w, vec![Write::Set("/devices/0/outputs/18/MirrorsToDigital".into(), PropValue::Bool(true))]);
        m.apply(&w[0]);
        assert_eq!(find_field(&m, &mirror.key).unwrap().value_text(), "ON");
        // ALT COUNT 1 enables the first pair of alt-capable outputs only.
        let w = resolve(&m, "alt:/devices/0", &SettingOp::Choose(1));
        assert_eq!(w.len(), 4);
        for x in &w {
            m.apply(x);
        }
        assert_eq!(alt_count(&m, "/devices/0"), 1);
        // Disabled engine options are refused (ADAT 1-2 on a cue output).
        assert!(resolve(&m, "/devices/0/outputs/14/OutputDestination", &SettingOp::Choose(3)).is_empty());
        // Posts carry the engine's argument shapes.
        assert_eq!(resolve(&m, "post:Identify:/devices/0", &SettingOp::Press), vec![Write::Post("/Identify".into(), "{\"path\":\"/devices/0\"}".into())]);
        // Device name edits trim and refuse empty names.
        assert!(resolve(&m, "/devices/0/DeviceName", &SettingOp::Text("  ".into())).is_empty());
        assert_eq!(resolve(&m, "/devices/0/DeviceName", &SettingOp::Text(" Studio ".into())), vec![Write::Set("/devices/0/DeviceName".into(), PropValue::Str("Studio".into()))]);
    }

    #[test]
    fn preferences_live_in_the_client() {
        let mut m = MemoryProperties::demo_x4();
        let w = resolve(&m, "pref:modifiers_timeout", &SettingOp::Text(" 10 ".into()));
        assert_eq!(w, vec![Write::Pref("modifiers_timeout".into(), PropValue::Str("10".into()))]);
        m.apply(&w[0]);
        let f = fields(&m, SettingsGroup::Display);
        assert_eq!(labels(&f), ["METERING", "CLIP HOLD", "PEAK HOLD", "\"DON'T SHOW AGAIN\" DIALOGS", "MODIFIERS TIMEOUT"]);
        assert_eq!(f[4].value_text(), "10");
        assert_eq!(f[0].value_text(), "PRE-FADER");
    }

    #[test]
    fn cue_routes_and_devices() {
        let m = MemoryProperties::demo_x4();
        let cues = fields(&m, SettingsGroup::CueRoutes(0));
        // Two active cues × (name, SOURCE, MONO, MIRROR TO).
        assert_eq!(cues.len(), 8);
        assert_eq!(cues[0].label, "CUE 1");
        assert_eq!(cues[1].value_text(), "MIX");
        assert_eq!(resolve(&m, &cues[1].key, &SettingOp::Choose(1)), vec![Write::Set("/devices/0/outputs/14/MixInSource".into(), PropValue::Str("cue".into()))]);
        let d = devices(&m);
        assert_eq!(d.len(), 1);
        assert_eq!((d[0].letter(), d[0].icon_key().as_str()), ('A', "Device-Icon-Apollo-x4"));
        assert_eq!(io_routes(&m, true)[0].name, "MON L");
        // I/O PRESETS without a preset shows "Automatic" (capture s12).
        let io = fields(&m, SettingsGroup::IoMatrix);
        let presets = io.iter().find(|f| f.label == "I/O PRESETS").unwrap();
        assert_eq!(presets.value_text(), "Automatic");
        assert_eq!(io.iter().map(|f| f.width).collect::<Vec<_>>(), [90.0, 210.0, 90.0, 90.0, 90.0]);
        assert_eq!(io[2].value_text(), "22");
        // OUTPUT REFERENCE LEVELS 1-2 writes both outputs of the pair.
        let dev = fields(&m, SettingsGroup::Device(0));
        let pair = dev.iter().find(|f| f.label == "1-2").unwrap();
        assert_eq!(resolve(&m, &pair.key, &SettingOp::Choose(0)).len(), 2);
    }
}
