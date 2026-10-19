//! What the mixer window shows, independent of where the data comes from.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::geometry::StripKind;
use crate::input::{Action, Control, ControlId};
use crate::scales;

#[derive(Clone, Debug, PartialEq)]
pub struct StripInfo {
    pub id: Arc<str>,
    /// Cell header ("ANALOG 1", "AUX 1").
    pub title: String,
    pub kind: StripKind,
    pub stereo: bool,
    /// Preamp layout (`preamp_channel_strip_controls[...]`), None for digital/virtual inputs.
    pub preamp_layout: Option<String>,
    pub shows_sr_convert: bool,
    /// The console model in the strip's console slot (API Summing, Neve 8028, …), which swaps
    /// the strip's skins ([`ConsoleModel`]). None on Apollo (4710) strips: the engine has no
    /// console slot.
    pub console_model: Option<ConsoleModel>,
}

/// A console model ("console extension") assigned to a strip's console slot. Console swaps
/// fader, bottom, send and pan skins by it through `Rack_ExtensionCustomization_*`.
///
/// Evidence (Console 1.2.9 `Rack.dll` and 1.3.1 Mac `Rack`; WEBBRIDGE-style tags):
/// - [obs] The slot is the strip's console insert: Rack asks for the plug-in id of insert kind 3,
///   index 0 (`… + 0xc1b8` vfunc +0x10 with `(strip, 3, 0)`; Mac `UpdateFaderInfoID` 0x1611d10,
///   `Rack_Pans` 0x16935f0). The helper's (4720) supercore buses carry it as
///   `effects/console` with `insert_type` `"console"` and a `plugin_id`; the engine (4710) has
///   no such node.
/// - [obs] Model numbers and plug-in ids (Win 0x181fa6c00): 1 none, 2 `uaudio_api_summing`,
///   3 `uaudio_neve_console`, 4 `UAD API Preamp`, 5 `uaudio_ampex_atr_102_ext`,
///   6 `uaudio_api_vision_ext`, 7 `uaudio_api_2500_ext`, 8 `uaudio_tube_channel_strip_ext`,
///   9 `uaudio_rhea` (flagged), 10 `uaudio_hardware_insert`.
/// - [obs] `Rack_LookAndFeelBase::GetConsoleModelCustomization` (Win 0x180ca89b0) looks the id up
///   in a table filled at Win 0x180ca9040: model 3 → `Neve8028`, 2 → `APISumming`,
///   4 → `APIPreamp`, 6 → `APIVision`, 8 → `CenturyChannelStrip`, 7 → `API2500`. Models 5, 9
///   and 10 have no customization. `APIPreamp` returns `""` for every strip slot (it skins the
///   preamp), so it is not a strip model here.
/// - [obs] The vtables of `APIVision`, `API2500` and `CenturyChannelStrip` share
///   `APISumming`'s strip slots, so they draw the `-API-Summing` skins.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ConsoleModel {
    /// API Summing, `uaudio_api_summing` (model 2).
    ApiSumming,
    /// Neve Summing (8028 skins), `uaudio_neve_console` (model 3).
    Neve8028,
    /// API Vision channel strip extension, `uaudio_api_vision_ext` (model 6).
    ApiVision,
    /// API 2500 bus compressor extension, `uaudio_api_2500_ext` (model 7).
    Api2500,
    /// Century tube channel strip extension, `uaudio_tube_channel_strip_ext` (model 8).
    CenturyTubeChannelStrip,
}

impl ConsoleModel {
    pub const ALL: [ConsoleModel; 5] = [ConsoleModel::ApiSumming, ConsoleModel::Neve8028, ConsoleModel::ApiVision, ConsoleModel::Api2500, ConsoleModel::CenturyTubeChannelStrip];

    /// The model whose plug-in id this is; None for no model or one without strip skins.
    pub fn from_plugin_id(id: &str) -> Option<ConsoleModel> {
        ConsoleModel::ALL.into_iter().find(|m| m.plugin_id() == id.trim())
    }

    pub fn plugin_id(self) -> &'static str {
        match self {
            ConsoleModel::ApiSumming => "uaudio_api_summing",
            ConsoleModel::Neve8028 => "uaudio_neve_console",
            ConsoleModel::ApiVision => "uaudio_api_vision_ext",
            ConsoleModel::Api2500 => "uaudio_api_2500_ext",
            ConsoleModel::CenturyTubeChannelStrip => "uaudio_tube_channel_strip_ext",
        }
    }

    /// Rack's console-model number (2…10).
    pub fn number(self) -> u8 {
        match self {
            ConsoleModel::ApiSumming => 2,
            ConsoleModel::Neve8028 => 3,
            ConsoleModel::ApiVision => 6,
            ConsoleModel::Api2500 => 7,
            ConsoleModel::CenturyTubeChannelStrip => 8,
        }
    }

    fn neve(self) -> bool {
        self == ConsoleModel::Neve8028
    }

    /// The skin-name suffix: `API-Summing` or `Neve-8028`.
    pub fn skin_suffix(self) -> &'static str {
        if self.neve() { "Neve-8028" } else { "API-Summing" }
    }

    /// Fader (`faders`; customization slot 6, Mac `UpdateFaderInfoID` vfunc +0x38).
    pub fn fader_skin(self) -> &'static str {
        if self.neve() { "Main-Input-Fader-Neve-8028" } else { "Main-Input-Fader-API-Summing" }
    }

    /// Main-strip bottom of a non-input strip (`backgrounds`; slot 5, Win 0x18144c570).
    pub fn aux_channel_bottom_skin(self) -> &'static str {
        if self.neve() { "Aux-Channel-Bottom-Neve-8028" } else { "Aux-Channel-Bottom-API-Summing" }
    }

    /// Send level knob (`knobs`, parent `Mini-Send-Level`; slot 8, Win 0x181454690).
    pub fn gain_skin(self) -> &'static str {
        if self.neve() { "Gain-Neve-8028" } else { "Gain-API-Summing" }
    }

    /// Pan knob (`knobs`; slot 11, Win 0x18146db00): `Pan-*` (parent `Pan-White`) when the flag
    /// is set, `Input-Pan-*` (parent `Pan-Black`) when not. `Rack_Pans` passes its section flag
    /// (+0x33c, nonzero outside the input section).
    pub fn pan_skin(self, bus: bool) -> &'static str {
        match (self.neve(), bus) {
            (true, true) => "Pan-Neve-8028",
            (true, false) => "Input-Pan-Neve-8028",
            (false, true) => "Pan-API-Summing",
            (false, false) => "Input-Pan-API-Summing",
        }
    }

    /// Lower plate of a send (`backgrounds`, parent `Send-Lower-State-Plate`; slot 12,
    /// `Rack_SendRoute` Win 0x18148ff70).
    pub fn lower_state_plate_skin(self) -> &'static str {
        if self.neve() { "Lower-State-Plate-Neve-8028" } else { "Lower-State-Plate-API-Summing" }
    }
}

/// A channel preset folder or file (a strip's `Preset` values tree; the PRESETS tab).
#[derive(Clone, Debug, PartialEq)]
pub struct PresetNode {
    pub name: String,
    /// Some for a folder (its entries, sorted by name as Console lists them).
    pub children: Option<Vec<PresetNode>>,
    /// A factory entry (`locked` in the engine's values): cannot be renamed or deleted.
    pub locked: bool,
}

impl PresetNode {
    pub fn file(name: &str) -> Self {
        Self { name: name.into(), children: None, locked: false }
    }
    pub fn folder(name: &str, children: Vec<PresetNode>) -> Self {
        Self { name: name.into(), children: Some(children), locked: false }
    }
    pub fn locked(mut self, locked: bool) -> Self {
        self.locked = locked;
        self
    }
}

/// The plug-in window footer (`Rack_PlugInFooter`) for an unlicensed plug-in: the state plate
/// and its action button (Rack.dll strings: "DEMO NOT STARTED", "DEMO ACTIVE - ", "DEMO
/// EXPIRED", "START DEMO"; `DoStateClicked` → StartDemo).
#[derive(Clone, Debug, PartialEq)]
pub struct PluginFooter {
    pub status: String,
    /// Plate background: 2 = error (red), 3 = blue, else neutral.
    pub state: u8,
    /// "START DEMO" / "BUY".
    pub action: Option<String>,
    /// The catalogue entry (`/plugins/N`), for `post /PluginStartDemo {"pluginPath"}`.
    pub plugin_path: Option<String>,
}

/// The skins a strip draws with, by kind and console model (the default rendering is the
/// `console_model == None` case).
impl StripInfo {
    /// `Rack_FaderAndMeters::UpdateFaderInfoID` (Mac 0x1611d10): a console model's fader for any
    /// strip with one; otherwise `Main-Bus-Fader` on buses and `Main-Input-Fader` on inputs.
    pub fn fader_skin(&self) -> &'static str {
        match (self.console_model, self.kind) {
            (Some(m), _) => m.fader_skin(),
            (None, StripKind::Aux | StripKind::Talkback) => "Main-Bus-Fader",
            (None, StripKind::Input) => "Main-Input-Fader",
        }
    }

    /// `Rack_Pans` (Mac 0x16935f0, Win 0x18146db00): inputs in the mixer always draw `Pan-Black`;
    /// other strips draw the model's `Pan-*`, or `Pan-White`.
    pub fn pan_skin(&self) -> &'static str {
        match self.kind {
            StripKind::Input => "Pan-Black",
            StripKind::Aux | StripKind::Talkback => self.console_model.map_or("Pan-White", |m| m.pan_skin(true)),
        }
    }

    /// Image behind a bus's main strip when it has a console model (`Aux-Channel-Bottom-*`,
    /// Mac 0x165b73f); None keeps the blank aux bottom.
    pub fn aux_bottom_skin(&self) -> Option<&'static str> {
        match self.kind {
            StripKind::Aux => self.console_model.map(ConsoleModel::aux_channel_bottom_skin),
            StripKind::Input | StripKind::Talkback => None,
        }
    }

    /// Send row lower plate (`Rack_SendRoute`, Mac 0x16d8a5b).
    pub fn send_plate_skin(&self) -> &'static str {
        self.console_model.map_or("Send-Lower-State-Plate", ConsoleModel::lower_state_plate_skin)
    }

    /// Send level knob (Mac 0x166a2a5).
    pub fn send_knob_skin(&self) -> &'static str {
        self.console_model.map_or("Mini-Send-Level", ConsoleModel::gain_skin)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct MainState {
    pub name: String,
    pub fader: f64,
    pub pan_l: f64,
    pub pan_r: f64,
    pub solo: bool,
    pub mute: bool,
    pub pre: bool,
    pub mono: bool,
    pub readout: String,
}

impl Default for MainState {
    fn default() -> Self {
        Self { name: String::new(), fader: scales::UNITY, pan_l: 0.5, pan_r: 0.5, solo: false, mute: false, pre: false, mono: false, readout: "0.0".into() }
    }
}

/// Values for the preamp layout's component ids.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PreampState {
    pub gain: f64,
    pub gain_text: String,
    pub source: String,
    pub phantom: bool,
    pub pad: bool,
    pub low_cut: bool,
    pub phase: bool,
    pub phantom_enabled: bool,
    pub pad_enabled: bool,
    /// 0…1 for the 4-frame input LED.
    pub input_led: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum InsertState {
    Empty,
    Loaded { name: String, power: bool },
}

/// The text Console draws in a loaded insert slot and its face (`Rack_InsertButtonPainter::
/// GetEffectName` 0x181429980): a running UAD-2 plug-in's catalogue short name upright, an
/// offline one's long name in italics, a native plug-in's name.
#[derive(Clone, Debug, PartialEq)]
pub struct InsertLabel {
    pub text: String,
    pub italic: bool,
}

/// Width (pt) Console fits slot labels into: the plate (76 pt) less 4 pt a side
/// (`PaintInsertContent` 0x18142c290 draws the name at `(x + 4, w − 8)`).
pub const INSERT_LABEL_W: f32 = 68.0;

/// Em (pt) of `inserts:Small-Normal`, the slot label font: Helvetica bold 9, `font-size-dyn`.
pub const INSERT_LABEL_SIZE: f32 = 9.0 + houston::fonts::FONT_SIZE_ADJUST;

/// Console's label for a running UAD-2 plug-in: [`effect_name_for_font`] with the slot font
/// (JUCE's rounded `getStringWidth`) and [`INSERT_LABEL_W`].
pub fn insert_label_for(name: &str, short_names: &[String]) -> String {
    let width = |t: &str| crate::window::helvetica_bold_width(t, INSERT_LABEL_SIZE).round();
    effect_name_for_font(name, short_names, &width, INSERT_LABEL_W)
}

/// `Rack_PlugInHeader::GetEffectNameForFont` (0x180dfda90 → 0x180dfd680): starting from the
/// catalogue name, walk the catalogue's short names, taking a longer one that still fits `max_w`
/// or a shorter one while the current one does not fit; `width` is JUCE's integer
/// `Font::getStringWidth`. `GetEffectName` then drops "UAD ". "UAD UA 1176 Rev A" with
/// ["1176 A", "1176 Rev A"] in Helvetica bold 10 → "1176 Rev A".
pub fn effect_name_for_font(name: &str, short_names: &[String], width: &dyn Fn(&str) -> f32, max_w: f32) -> String {
    let mut cur = name.to_owned();
    for c in short_names {
        let (wc, wn) = (width(&cur), width(c));
        if (wc < wn && wn <= max_w) || (wc > wn && max_w < wc) {
            cur = c.clone();
        }
    }
    cur.replace("UAD ", "")
}

#[derive(Clone, Debug, PartialEq)]
pub struct SendState {
    pub name: String,
    pub level: f64,
    pub muted: bool,
    /// The send's pan knob, 0…1 (0.5 = centre; `sends/k/Pan` −1…1). Sends without a pan
    /// (aux → cue, stereo strips) read 0.5.
    pub pan: f64,
    /// The send's own meter (`sends/k/meters/*`, post-fader): the full send strip's bar, the
    /// mini send's knob ring and the overview's signal LED.
    pub meter: MeterFrame,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MonitorState {
    pub level: f64,
    pub readout: String,
    pub mono: bool,
    pub mute: bool,
}

/// Meter levels as 0…1 scale fractions, the engine-held peaks (`MeterPeakLevel`, same scale)
/// and clip LEDs.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MeterFrame {
    pub left: f64,
    pub right: f64,
    pub peak_left: f64,
    pub peak_right: f64,
    pub clip_left: bool,
    pub clip_right: bool,
}

impl MeterFrame {
    /// The louder leg, 0…1: what a single-valued indicator (knob ring, signal LED) shows.
    pub fn level(&self) -> f64 {
        self.left.max(self.right)
    }

    /// Both legs folded into the left (a mono bar showing a stereo send).
    pub fn mono(&self) -> MeterFrame {
        MeterFrame {
            left: self.level(),
            peak_left: self.peak_left.max(self.peak_right),
            clip_left: self.clip_left || self.clip_right,
            ..Default::default()
        }
    }
}

/// One entry of the plug-in browser (`/plugins/P/{Name, Categories, Authorized, …}`).
#[derive(Clone, Debug, PartialEq)]
pub struct PluginEntry {
    /// Catalogue name, as written to an insert's `EffectName` ("UAD 1176LN Rev E").
    pub name: String,
    /// First of `categories` (one of `MixerModel::plugin_categories`).
    pub category: String,
    /// Every category the plug-in is listed under (`Categories`, comma separated).
    pub categories: Vec<String>,
    /// Licensed or demoing: listed under "Universal Audio"; otherwise under
    /// "Universal Audio (Inactive)" with `badge`.
    pub enabled: bool,
    pub favorite: bool,
    /// Badge of an inactive plug-in (`Plugin-Badge-*`).
    pub badge: Option<PluginBadge>,
    /// Optional native host support, independent of the device licence.
    pub native_supported: Option<bool>,
    pub native_hint: Option<String>,
}

/// `Rack_PluginsTreeViewBadgedItem` badges (`Rack_PluginsBrowserManager::_GetRowData`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginBadge {
    Demo,
    Buy,
    Activate,
    Disabled,
}

impl PluginBadge {
    /// `views` skin of the badge (55×17 pt).
    pub fn skin(self) -> &'static str {
        match self {
            PluginBadge::Demo => "Plugin-Badge-Demo",
            PluginBadge::Buy => "Plugin-Badge-Buy",
            PluginBadge::Activate => "Plugin-Badge-Activate",
            PluginBadge::Disabled => "Plugin-Badge-Disabled",
        }
    }
}

impl PluginEntry {
    pub fn new(name: &str, category: &str, enabled: bool) -> Self {
        Self {
            name: name.into(),
            category: category.into(),
            categories: vec![category.into()],
            enabled,
            favorite: false,
            badge: (!enabled).then_some(PluginBadge::Demo),
            native_supported: None,
            native_hint: None,
        }
    }
    /// Listed under `category` (any of its categories).
    pub fn in_category(&self, category: &str) -> bool {
        self.categories.iter().any(|c| c == category) || self.category == category
    }
    /// Browser label: the name without the "UAD " prefix.
    pub fn display_name(&self) -> &str {
        self.name.strip_prefix("UAD ").unwrap_or(&self.name)
    }
    /// `houston` image key of the plug-in icon.
    pub fn icon_key(&self) -> String {
        format!("plugin:{}", self.name)
    }
}

/// The category grid of Console 1.3.1's plug-in browser, in display order (3 columns).
pub const PLUGIN_CATEGORIES: [&str; 12] = [
    "Channel Strip & Preamp",
    "Compressors & Limiters",
    "Delay",
    "Equalization",
    "Guitar & Bass",
    "Mastering",
    "Microphone",
    "Modulation",
    "Pitch Correction",
    "Reverb & Room",
    "Special Processing",
    "Tape & Saturation",
];

/// Bottom status bar (`/Tempo`, `/SampleRate`, `/ClockSource`, `/Total{DSP,PGM,MEM}Load`, scene).
#[derive(Clone, Debug, PartialEq)]
pub struct StatusInfo {
    pub tempo: f64,
    /// None → "OFFLINE".
    pub sample_rate: Option<u32>,
    /// None → "OFFLINE".
    pub clock_source: Option<String>,
    /// Loads in percent (0…100), the engine's latest values as they are (Console shows them raw:
    /// no smoothing or peak hold).
    pub dsp_load: f32,
    pub pgm_load: f32,
    pub mem_load: f32,
    /// The engine has plug-in scenes (`/pluginscenes/0`): Console shows the SCENE field only then.
    pub plugin_scenes: bool,
    /// Recalled plug-in scene; None → "(NONE)".
    pub scene: Option<String>,
    /// `/ClockLocked`: false blinks an outline round CLOCK (`Rack_ClockInfo`); None = unknown.
    pub clock_locked: Option<bool>,
}

impl Default for StatusInfo {
    fn default() -> Self {
        Self { tempo: 120.0, sample_rate: None, clock_source: None, dsp_load: 0.0, pgm_load: 0.0, mem_load: 0.0, plugin_scenes: true, scene: None, clock_locked: None }
    }
}

/// Settings sheet: connection and global engine settings with their choices.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EngineSettings {
    /// "127.0.0.1:4710".
    pub host: String,
    pub connected: bool,
    pub sample_rate: Option<u32>,
    pub sample_rates: Vec<u32>,
    /// The engine's per-rate `enabled` flag, parallel to `sample_rates`; missing = enabled.
    pub sample_rate_enabled: Vec<bool>,
    pub clock_source: Option<String>,
    pub clock_sources: Vec<String>,
    /// `/BufferSize` (Windows engine only).
    pub buffer_size: Option<u32>,
    pub buffer_sizes: Vec<u32>,
    /// The connection's write gate (read-only by default).
    pub writes_enabled: bool,
}

/// Monitor-column extras that only exist on some devices / configurations.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MonitorOptions {
    /// `outputs/MON/DimOn`; None hides DIM.
    pub dim: Option<bool>,
    /// Dim attenuation knob 0…1 (`/devices/D/DimAttenuation` 0…60 dB) and its readout.
    pub dim_level: f64,
    pub dim_readout: String,
    /// `/TalkbackOn`; None hides TALK.
    pub talkback: Option<bool>,
    /// Number of ALT speaker sets (0 hides the MONITOR/ALT rows) and the selection (0 = main).
    pub alt_count: u8,
    pub alt_selection: u8,
    /// ALT 1 / ALT 2 TRIM knobs 0…1 (`outputs/N/AltMonTrim` −30…+30 dB, 0.5 = 0 dB).
    pub alt_trim: [f64; 2],
    /// MONITOR CONTROLLER button shown (`Rack_MonitorSection_UAD2::resized`: hidden when the
    /// mixer reports no controller support; the column then closes up by 40 pt).
    pub controller: bool,
    /// MONITOR CORRECTION buttons: `Some(power)` when the device supports headphone
    /// (`SupportsHeadphoneCorrection`, `HeadphoneCorrectionPower`) / speaker
    /// (`SupportsSpeakerCorrection`, `SpeakerCorrectionPower`) correction.
    pub hp_correction: Option<bool>,
    pub speaker_correction: Option<bool>,
    /// The hardware monitor knob controls this mixer's monitor level (green `knob_focus` dot).
    pub knob_focus: bool,
    /// Control-room SOURCE (`outputs/MON/MixInSource`): 0 = MONITOR, n = CUE n.
    pub cr_source: u8,
    /// TALKBACK TO MONITOR (`Rack_Talkback` `talkback_to_mons`, 0x181497377):
    /// `/TalkbackInPhysicalCR`; None hides the button's state (off).
    pub talkback_to_monitor: Option<bool>,
}

/// A unit on the engine (`/devices/N`): Console's device tabs and offline-device list.
#[derive(Clone, Debug, PartialEq)]
pub struct DeviceInfo {
    pub index: usize,
    /// `DeviceName` ("Apollo x4").
    pub name: String,
    /// `DeviceOnline`.
    pub online: bool,
}

/// One channel in View > Show/Hide Channels: the input strip's path, its name and whether it
/// is hidden (`ChannelHidden`).
#[derive(Clone, Debug, PartialEq)]
pub struct ChannelEntry {
    pub id: String,
    pub name: String,
    pub hidden: bool,
}

/// A cue output (`outputs/14..17`): what it plays and how.
#[derive(Clone, Debug, PartialEq)]
pub struct CueOutput {
    pub name: String,
    /// `MixInSource`: true = its cue mix, false = the monitor mix.
    pub source_cue: bool,
    /// `MixToMono`.
    pub mono: bool,
    /// `OutputDestination` label ("LINE 3/4").
    pub destination: String,
}

/// A headphone output and its source choices (`outputs/19,20/MixInSource`).
#[derive(Clone, Debug, PartialEq)]
pub struct HeadphoneOutput {
    pub name: String,
    pub source: usize,
    pub options: Vec<String>,
}

pub trait MixerModel {
    fn session_name(&self) -> String;
    /// The open session and the SESSION menu's lists. Default: the name (and `*`) from
    /// `session_name`, no lists.
    fn session(&self) -> crate::surfaces::session::SessionInfo {
        crate::surfaces::session::SessionInfo::from_name(&self.session_name())
    }
    fn inputs(&self) -> Vec<StripInfo>;
    /// The Snapshots section's lists and CURRENT / PRIMED. Default: none.
    fn snapshots(&self) -> crate::surfaces::SnapshotInfo {
        crate::surfaces::SnapshotInfo::default()
    }
    /// The visible projection; complete lookups keep offline strips addressable.
    fn inputs_for_view(&self, _show_offline: bool) -> Vec<StripInfo> { self.inputs() }
    fn auxes(&self) -> Vec<StripInfo>;
    /// Channel presets (the PRESETS tab): a strip's `Preset` values tree.
    fn channel_presets(&self) -> Vec<PresetNode> {
        Vec::new()
    }
    /// A loaded insert's plug-in presets (its `Preset` values: `.fxp` files and folders).
    fn plugin_presets(&self, _s: &StripInfo, _slot: u8) -> Vec<PresetNode> {
        Vec::new()
    }
    /// The preset a strip (Channel) or insert shows (`Preset`, `[folder/]name`), if any.
    fn preset_name(&self, _s: &StripInfo, _scope: crate::presets::PresetScope) -> Option<String> {
        None
    }
    /// Whether the plug-in bar's PASTE is enabled (`CanPastePluginBarSettings`).
    fn can_paste_plugin_settings(&self, _s: &StripInfo, _slot: u8) -> bool {
        false
    }
    /// The plug-in window footer for `effect_name` (None: licensed, no footer).
    fn plugin_footer(&self, _effect_name: &str) -> Option<PluginFooter> {
        None
    }
    /// The talkback mic strip (`IOType` `TalkbackMic`), shown with SHOW CTRL ROOM. Its sends
    /// (AUX 1, AUX 2, CUE 1…) come from `input_sends`.
    fn talkback(&self) -> Option<StripInfo> {
        None
    }
    fn main(&self, s: &StripInfo) -> MainState;
    fn preamp(&self, s: &StripInfo) -> Option<PreampState>;
    fn inserts(&self, s: &StripInfo) -> Vec<InsertState>;
    fn unison(&self, s: &StripInfo) -> InsertState;
    /// Console's label for each slot of [`MixerModel::inserts`]. A slot without one (None, or past
    /// the end of the list) shows the plug-in name without "UAD ", upright.
    fn insert_labels(&self, _s: &StripInfo) -> Vec<Option<InsertLabel>> {
        Vec::new()
    }
    /// The catalogue's `/plugins/P/Unison` for plug-in `name`: whether it can sit in a Unison
    /// slot (drag and drop, paste). None: unknown (allowed).
    fn plugin_is_unison(&self, _name: &str) -> Option<bool> {
        None
    }
    /// Console's label for the Unison slot (None: from [`MixerModel::unison`]'s name).
    fn unison_label(&self, _s: &StripInfo) -> Option<InsertLabel> {
        None
    }
    fn sends(&self, s: &StripInfo) -> Vec<SendState>;
    /// UAD REC (false) vs MON (true): `RecordPreEffects`.
    fn records_pre_effects(&self, s: &StripInfo) -> bool;
    fn meters(&self, s: &StripInfo) -> MeterFrame;
    fn monitor(&self) -> MonitorState;
    fn monitor_meters(&self) -> MeterFrame;
    /// Meter of the send at property node `node` (`…/sends/N`, as the Sends window addresses it).
    fn send_meter(&self, _node: &str) -> MeterFrame {
        MeterFrame::default()
    }
    fn apply(&mut self, action: &Action);
    /// Applies pending external updates (engine pushes); true if anything changed.
    /// Called once per frame by the app.
    fn pump(&mut self) -> bool {
        false
    }
    /// The app went to the background (window hidden, iPad app inactive) or came back. An
    /// engine model tells the engine (`set /Sleep true|false`); default: nothing.
    fn set_background(&mut self, _background: bool) {}
    /// The user quit the app. An engine model tells the engine to save its last-used session
    /// (`post /SaveLastUsedSession {}`, as Console does on its way out); default: nothing.
    fn quit(&mut self) {}

    // ---- UI surfaces; every method has a default so existing models keep compiling. ----

    /// Input strip sends: AUX 1, AUX 2, CUE 1…n (`inputs/N/sends/S`), drawn in the Sends cell.
    fn input_sends(&self, _s: &StripInfo) -> Vec<SendState> {
        Vec::new()
    }
    /// Send rows Console reserves in the strip's Sends cell. Console sizes the cell for every
    /// send the engine lists (an Apollo aux has CUE 1–4) but draws only the active ones, so the
    /// cell is taller than the visible sends (`GetChannelStripCellHeight_Sends` 0x18145a6e0).
    fn send_rows(&self, s: &StripInfo) -> usize {
        if s.kind == crate::geometry::StripKind::Aux { self.sends(s).len() } else { self.input_sends(s).len() }
    }
    /// Pre state of send `k` (input or aux strip); None hides the PRE button.
    fn send_pre(&self, _s: &StripInfo, _k: usize) -> Option<bool> {
        None
    }
    /// `Isolate` ("[ ISOLATE ]" under the name).
    fn isolated(&self, _s: &StripInfo) -> bool {
        false
    }
    /// Input-source choices (`IOType/values`), labels as drawn ("MIC", "LINE", "HI-Z").
    fn input_sources(&self, s: &StripInfo) -> Vec<String> {
        if s.preamp_layout.is_some() { vec!["MIC".into(), "LINE".into()] } else { Vec::new() }
    }
    fn plugin_catalog(&self) -> Vec<PluginEntry> {
        Vec::new()
    }
    /// Latest assignment refusal or engine error, displayed in the browser.
    fn plugin_notice(&self) -> Option<&str> {
        None
    }
    fn plugin_categories(&self) -> Vec<String> {
        PLUGIN_CATEGORIES.iter().map(|c| c.to_string()).collect()
    }
    /// Plug-in scene names (`/pluginscenes/0/scenes/N/SceneName`).
    fn plugin_scenes(&self) -> Vec<String> {
        Vec::new()
    }
    fn status(&self) -> StatusInfo {
        StatusInfo::default()
    }
    fn engine_settings(&self) -> EngineSettings {
        EngineSettings::default()
    }
    fn monitor_options(&self) -> MonitorOptions {
        MonitorOptions::default()
    }
    fn cue_outputs(&self) -> Vec<CueOutput> {
        Vec::new()
    }
    fn headphone_outputs(&self) -> Vec<HeadphoneOutput> {
        Vec::new()
    }
    /// Text edits committed by the UI: `Name` (channel name), `Insert(k)` (load plug-in by
    /// catalogue name), `Tempo`.
    fn set_text(&mut self, _id: &ControlId, _text: &str) {}
    /// Window › Notifications: what went wrong lately (engine refusals), newest first.
    fn notifications(&self) -> Vec<String> {
        Vec::new()
    }
    /// Properties behind the Settings window and secondary panels (engine paths and `pref:`
    /// client preferences). None leaves those pages empty.
    fn properties(&self) -> Option<&dyn crate::settings::PropertySource> {
        None
    }
    /// Native correction SVG, kept in memory; slot is speaker/headphone/headphone2.
    fn correction_image(&self,_device:usize,_slot:&str)->Option<&str> {None}
    /// Applies the writes of a settings control (`settings::resolve`): engine `set`s and
    /// `post`s, client preferences. Ignored by default.
    fn apply_setting_writes(&mut self, _writes: Vec<crate::settings::Write>) {}
    /// A settings control operation on the field bound to `key`.
    fn apply_setting(&mut self, key: &str, op: &crate::settings::SettingOp) {
        let Some(src) = self.properties() else { return };
        let writes = crate::settings::resolve(src, key, op);
        if !writes.is_empty() {
            self.apply_setting_writes(writes);
        }
    }
    /// Ticks and enable states of the strip menus (label, solo, preamp).
    fn strip_menu_state(&self, _s: &StripInfo) -> crate::menu::StripMenuState {
        crate::menu::StripMenuState::default()
    }
    /// Global menu state: undo / redo, metering, the plug-in clipboard.
    fn menu_state(&self) -> crate::menu::MenuModelState {
        crate::menu::MenuModelState::default()
    }
    /// The engine's units (`/devices/N`). Default: none known.
    fn devices(&self) -> Vec<DeviceInfo> {
        Vec::new()
    }
    /// Every input channel that can be hidden or shown (View > Show/Hide Channels), hidden ones
    /// included. Default: the visible inputs, none hidden.
    fn channel_list(&self) -> Vec<ChannelEntry> {
        self.inputs().into_iter().map(|s| ChannelEntry { id: s.id.to_string(), name: s.title.to_string(), hidden: false }).collect()
    }
    /// The concrete model, for hosts that need more than this trait (plug-in windows reach the
    /// engine store through it). None by default.
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        None
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        None
    }
}

/// Offline demo data modelled on console-main-5.png (UAD Console 1.3.1); actions edit local state.
pub struct PreviewMixer {
    pub inputs: Vec<StripInfo>,
    pub auxes: Vec<StripInfo>,
    pub talkback: Option<StripInfo>,
    pub mains: HashMap<Arc<str>, MainState>,
    pub preamps: HashMap<Arc<str>, PreampState>,
    pub monitor: MonitorState,
    pub inserts: HashMap<Arc<str>, Vec<InsertState>>,
    /// Sends per strip (inputs: AUX 1, AUX 2, CUE 1, CUE 2; auxes: CUE 1, CUE 2) and their pre state.
    pub sends: HashMap<Arc<str>, Vec<(SendState, Option<bool>)>>,
    pub record_pre: HashMap<Arc<str>, bool>,
    pub isolated: HashSet<Arc<str>>,
    pub catalog: Vec<PluginEntry>,
    pub scenes: Vec<String>,
    pub snapshots: crate::surfaces::SnapshotInfo,
    pub status: StatusInfo,
    pub settings: EngineSettings,
    pub monitor_options: MonitorOptions,
    pub cues: Vec<CueOutput>,
    pub headphones: Vec<HeadphoneOutput>,
    /// Insert copied with COPY, pasted by the next COPY on an empty slot.
    pub clipboard: Option<InsertState>,
    /// Settings window and panel properties (an Apollo x4 like the p2 reference engine).
    pub props: crate::settings::MemoryProperties,
    /// Strip menu toggles (solo safe, link, hide, SR convert) set through the menus.
    pub strip_flags: HashMap<Arc<str>, crate::menu::StripMenuState>,
    /// `/PostFaderMetering`.
    pub post_fader_metering: bool,
}

fn demo_catalog() -> Vec<PluginEntry> {
    let entries: [(&str, &str); 48] = [
        ("UAD 4K Buss Compressor", "Compressors & Limiters"),
        ("UAD 4K Channel Strip", "Channel Strip & Preamp"),
        ("UAD A-Type Multiband Enhancer", "Special Processing"),
        ("UAD ADA Flanger", "Modulation"),
        ("UAD ADA STD-1 Stereo Tapped Delay", "Delay"),
        ("UAD AKG BX 20", "Reverb & Room"),
        ("UAD Ampeg B15N", "Guitar & Bass"),
        ("UAD Ampeg SVT3Pro", "Guitar & Bass"),
        ("UAD Ampeg SVTVR", "Guitar & Bass"),
        ("UAD Ampeg SVTVR Classic", "Guitar & Bass"),
        ("UAD Ampex ATR-102", "Tape & Saturation"),
        ("UAD AMS dmx 15-80 S", "Delay"),
        ("UAD AMS Neve DFC Channel Strip", "Channel Strip & Preamp"),
        ("UAD AMS RMX16", "Reverb & Room"),
        ("UAD Antares Auto-Tune Realtime", "Pitch Correction"),
        ("UAD API 2500", "Compressors & Limiters"),
        ("UAD API 550A", "Equalization"),
        ("UAD API Vision Channel Strip", "Channel Strip & Preamp"),
        ("UAD Auto-Tune Realtime X", "Pitch Correction"),
        ("UAD Avalon VT-737sp", "Channel Strip & Preamp"),
        ("UAD Brigade Chorus", "Modulation"),
        ("UAD Cambridge", "Equalization"),
        ("UAD Capitol Chambers", "Reverb & Room"),
        ("UAD Chandler Limited Curve Bender", "Equalization"),
        ("UAD dbx 160", "Compressors & Limiters"),
        ("UAD Eden WT800", "Guitar & Bass"),
        ("UAD EMT 140", "Reverb & Room"),
        ("UAD Fairchild 670", "Compressors & Limiters"),
        ("UAD Galaxy Tape Echo", "Delay"),
        ("UAD Gallien Krueger 800RB", "Guitar & Bass"),
        ("UAD Lexicon 224", "Reverb & Room"),
        ("UAD Maag EQ4 MS", "Equalization"),
        ("UAD Manley Massive Passive MST", "Mastering"),
        ("UAD Millennia NSEQ-2", "Equalization"),
        ("UAD Neve 1073", "Channel Strip & Preamp"),
        ("UAD Neve 33609SE", "Compressors & Limiters"),
        ("UAD Ocean Way Mic Collection", "Microphone"),
        ("UAD Oxford Inflator", "Mastering"),
        ("UAD Oxide Tape", "Tape & Saturation"),
        ("UAD Precision Buss Compressor", "Compressors & Limiters"),
        ("UAD Pultec EQP-1A", "Equalization"),
        ("UAD Roland RE-201", "Delay"),
        ("UAD Sphere Mic Collection", "Microphone"),
        ("UAD SPL Vitalizer MK2-T", "Special Processing"),
        ("UAD SSL E Channel Strip", "Channel Strip & Preamp"),
        ("UAD SSL G Bus Compressor", "Compressors & Limiters"),
        ("UAD Studer A800", "Tape & Saturation"),
        ("UAD Teletronix LA-2A Gray", "Compressors & Limiters"),
    ];
    let enabled = ["UAD Neve 1073", "UAD Teletronix LA-2A Gray", "UAD Pultec EQP-1A", "UAD Oxide Tape", "UAD Galaxy Tape Echo"];
    let favorites = ["UAD Neve 1073", "UAD Teletronix LA-2A Gray", "UAD Lexicon 224", "UAD Fairchild 670"];
    entries
        .iter()
        .map(|(n, c)| PluginEntry { favorite: favorites.contains(n), ..PluginEntry::new(n, c, enabled.contains(n)) })
        .collect()
}

impl Default for PreviewMixer {
    fn default() -> Self {
        let input = |id: &str, title: &str, stereo, preamp: bool, sr| StripInfo {
            id: id.into(),
            title: title.into(),
            kind: StripKind::Input,
            stereo,
            preamp_layout: preamp.then(|| "preamp_channel_strip_controls".into()),
            shows_sr_convert: sr,
            console_model: None,
        };
        let aux = |id: &str, title: &str| StripInfo { id: id.into(), title: title.into(), kind: StripKind::Aux, stereo: true, preamp_layout: None, shows_sr_convert: false, console_model: None };
        let inputs = vec![
            input("in0", "ANALOG 1", false, true, false),
            input("in1", "ANALOG 2", false, true, false),
            input("in2", "ANALOG 3/4", true, true, false),
            input("in4", "S/PDIF", true, false, true),
            input("v0", "VIRTUAL 1/2", true, false, false),
            input("v2", "VIRTUAL 3/4", true, false, false),
        ];
        let auxes = vec![aux("aux0", "AUX 1"), aux("aux1", "AUX 2")];
        let talkback = StripInfo { id: "tb".into(), title: "TALKBACK".into(), kind: StripKind::Talkback, stereo: false, preamp_layout: None, shows_sr_convert: false, console_model: None };
        let names = ["SM7B", "GTR", "Dante AVIO", "S/PDIF", "Macbook", "Macbook 3/4"];
        let mut mains = HashMap::new();
        for (s, n) in inputs.iter().zip(names) {
            let st = if s.stereo { MainState { pan_l: 0.0, pan_r: 1.0, ..Default::default() } } else { MainState::default() };
            mains.insert(s.id.clone(), MainState { name: n.into(), mute: true, ..st });
        }
        mains.insert("aux0".into(), MainState { name: "AUX 1".into(), fader: scales::fader_tapered(-9.4), readout: "-9.4".into(), ..Default::default() });
        mains.insert("aux1".into(), MainState { name: "AUX 2".into(), ..Default::default() });
        mains.insert("tb".into(), MainState { name: "TALKBACK".into(), fader: scales::fader_tapered(-6.0), readout: "-6.0".into(), ..Default::default() });
        let mut preamps = HashMap::new();
        preamps.insert("in0".into(), PreampState { gain: 0.3, gain_text: "27 dB".into(), source: "LINE".into(), ..Default::default() });
        for id in ["in1", "in2"] {
            preamps.insert(id.into(), PreampState { gain: 0.0, gain_text: "10 dB".into(), source: "MIC".into(), phantom_enabled: true, pad_enabled: true, ..Default::default() });
        }
        let loaded = |names: &[&str]| -> Vec<InsertState> {
            let mut v: Vec<InsertState> = names.iter().map(|n| InsertState::Loaded { name: format!("UAD {n}"), power: true }).collect();
            v.resize(4, InsertState::Empty);
            v
        };
        let mut inserts = HashMap::new();
        inserts.insert("in0".into(), loaded(&["Millennia NSEQ-2", "SPL Vitalizer MK2-T"]));
        inserts.insert("in1".into(), loaded(&["Chandler Limited Curve Bender", "Gallien Krueger 800RB", "Tube-Tech PE 1C", "SSL G Bus Compressor"]));
        inserts.insert("in2".into(), loaded(&["Summit Audio TLA-100A", "dbx 160", "Eden WT800", "Precision Buss Compressor"]));
        inserts.insert("in4".into(), loaded(&["Neve 33609SE", "Maag EQ4 MS", "UA 1176SE Legacy"]));
        inserts.insert("v0".into(), loaded(&["Auto-Tune Realtime X", "Auto-Tune Realtime X", "SSL E Channel Strip"]));
        inserts.insert("v2".into(), loaded(&["Cambridge", "Oxford Inflator"]));
        if let Some(InsertState::Loaded { power, .. }) = inserts.get_mut("in1").and_then(|v: &mut Vec<InsertState>| v.get_mut(3)) {
            *power = false;
        }
        let send = |name: &str, level: f64, muted: bool| SendState { name: name.into(), level, muted, pan: 0.5, meter: MeterFrame::default() };
        let mut sends = HashMap::new();
        for (i, s) in inputs.iter().enumerate() {
            let lvl = 0.25 + 0.08 * i as f64;
            sends.insert(
                s.id.clone(),
                vec![
                    (send("AUX 1", lvl, false), Some(false)),
                    (send("AUX 2", 0.0, i % 2 == 1), Some(i == 0)),
                    (send("CUE 1", scales::UNITY, false), Some(true)),
                    (send("CUE 2", 0.45, false), Some(true)),
                ],
            );
        }
        for a in &auxes {
            sends.insert(a.id.clone(), vec![(send("CUE 1", 0.5, false), None), (send("CUE 2", 0.5, false), None)]);
        }
        sends.insert(
            talkback.id.clone(),
            vec![(send("AUX 1", 0.5, false), None), (send("AUX 2", 0.5, false), None), (send("CUE 1", 0.5, false), None), (send("CUE 2", 0.5, false), None)],
        );
        let record_pre = inputs.iter().map(|s| (s.id.clone(), s.preamp_layout.is_none())).collect();
        Self {
            inputs,
            auxes,
            talkback: Some(talkback),
            mains,
            preamps,
            monitor: MonitorState { level: 0.09, readout: "-52.0 dB".into(), mono: false, mute: false },
            inserts,
            sends,
            record_pre,
            isolated: HashSet::from([Arc::from("in2")]),
            catalog: demo_catalog(),
            scenes: vec!["Tracking".into(), "Overdubs".into(), "Mix Prep".into()],
            snapshots: {
                use crate::surfaces::SnapshotFolder as F;
                let f = |name: &str, s: &[&str]| F { name: name.into(), snapshots: s.iter().map(|n| n.to_string()).collect() };
                crate::surfaces::SnapshotInfo {
                    folders: vec![f("000 - Untitled", &["001 - Intro", "002 - Verse"]), f("001 - Show", &["001 - Intro", "002 - Chorus"])],
                    volumes: vec!["Macintosh HD".into()],
                    ..Default::default()
                }
            },
            status: StatusInfo { tempo: 120.0, sample_rate: None, clock_source: None, dsp_load: 42.0, pgm_load: 31.0, mem_load: 18.0, plugin_scenes: true, scene: None, clock_locked: None },
            settings: EngineSettings {
                host: "127.0.0.1:4710".into(),
                connected: false,
                sample_rate: Some(48_000),
                sample_rates: vec![44_100, 48_000, 88_200, 96_000, 176_400, 192_000],
                sample_rate_enabled: Vec::new(),
                clock_source: Some("Internal".into()),
                clock_sources: vec!["Internal".into(), "S/PDIF".into(), "Word Clock".into()],
                buffer_size: Some(256),
                buffer_sizes: vec![32, 64, 128, 256, 512, 1024],
                writes_enabled: false,
            },
            monitor_options: MonitorOptions {
                dim: Some(false),
                dim_level: 0.3,
                dim_readout: "-17 dB".into(),
                talkback: Some(false),
                alt_count: 0,
                alt_selection: 0,
                alt_trim: [0.5; 2],
                controller: true,
                hp_correction: Some(true),
                speaker_correction: Some(true),
                knob_focus: true,
                cr_source: 0,
                talkback_to_monitor: Some(false),
            },
            cues: vec![
                CueOutput { name: "CUE 1".into(), source_cue: true, mono: false, destination: "LINE 3/4".into() },
                CueOutput { name: "CUE 2".into(), source_cue: false, mono: true, destination: "HP 2".into() },
            ],
            headphones: vec![
                // The engine's `outputs/19,20/MixInSource` list: default, cue1…cue4, none.
                HeadphoneOutput { name: "HP 1".into(), source: 1, options: ["MON", "CUE 1", "CUE 2", "CUE 3", "CUE 4", "NONE"].map(String::from).to_vec() },
                HeadphoneOutput { name: "HP 2".into(), source: 2, options: ["MON", "CUE 1", "CUE 2", "CUE 3", "CUE 4", "NONE"].map(String::from).to_vec() },
            ],
            clipboard: None,
            props: crate::settings::MemoryProperties::demo_x4(),
            strip_flags: HashMap::new(),
            post_fader_metering: false,
        }
    }
}

impl PreviewMixer {
    fn strip(&self, id: &str) -> Option<&StripInfo> {
        self.inputs.iter().chain(&self.auxes).chain(&self.talkback).find(|s| &*s.id == id)
    }
}

impl MixerModel for PreviewMixer {
    fn session_name(&self) -> String {
        "Session*".into()
    }
    fn snapshots(&self) -> crate::surfaces::SnapshotInfo {
        self.snapshots.clone()
    }
    fn session(&self) -> crate::surfaces::session::SessionInfo {
        use crate::surfaces::session::{SessionFile, SessionInfo};
        let file = |p: &str| SessionFile { path: p.into(), label: p.into() };
        SessionInfo {
            files: vec![file("Session"), file("Tracking"), file("Mix/Rough Mix"), file("Mix/Vocal Comp")],
            modified: vec!["[Sep 18, 2026 05:14:34 AM] - - New Session -".into()],
            ..SessionInfo::from_name(&self.session_name())
        }
    }
    fn inputs(&self) -> Vec<StripInfo> {
        self.inputs.clone()
    }
    fn auxes(&self) -> Vec<StripInfo> {
        self.auxes.clone()
    }
    fn talkback(&self) -> Option<StripInfo> {
        self.talkback.clone()
    }
    fn channel_presets(&self) -> Vec<PresetNode> {
        let f = |names: &[&str]| names.iter().map(|n| PresetNode::file(n)).collect::<Vec<_>>();
        vec![
            PresetNode::folder("Bass", f(&["Andrew Scheps - Bass", "Darrell Thorp - Bass"])),
            PresetNode::folder("Drums", f(&["Andrew Scheps - Kick", "Andrew Scheps - Snare"])),
            PresetNode::folder("Guitar", f(&["Darrell Thorp - Electric Guitar", "Jeff Balding - Acoustic"])),
            PresetNode::folder("Vocals", f(&["Darrell Thorp - Vocals", "Dave Isaac - Lead Vox"])),
        ]
    }
    fn main(&self, s: &StripInfo) -> MainState {
        self.mains.get(&s.id).cloned().unwrap_or_default()
    }
    fn preamp(&self, s: &StripInfo) -> Option<PreampState> {
        self.preamps.get(&s.id).cloned()
    }
    fn inserts(&self, s: &StripInfo) -> Vec<InsertState> {
        self.inserts.get(&s.id).cloned().unwrap_or_else(|| vec![InsertState::Empty; 4])
    }
    fn unison(&self, _: &StripInfo) -> InsertState {
        InsertState::Empty
    }
    fn sends(&self, s: &StripInfo) -> Vec<SendState> {
        if s.kind == StripKind::Aux { self.sends.get(&s.id).map(|v| v.iter().map(|(s, _)| s.clone()).collect()).unwrap_or_default() } else { vec![] }
    }
    fn input_sends(&self, s: &StripInfo) -> Vec<SendState> {
        if matches!(s.kind, StripKind::Input | StripKind::Talkback) { self.sends.get(&s.id).map(|v| v.iter().map(|(s, _)| s.clone()).collect()).unwrap_or_default() } else { vec![] }
    }
    fn send_pre(&self, s: &StripInfo, k: usize) -> Option<bool> {
        self.sends.get(&s.id).and_then(|v| v.get(k)).and_then(|(_, p)| *p)
    }
    fn isolated(&self, s: &StripInfo) -> bool {
        self.isolated.contains(&s.id)
    }
    fn input_sources(&self, s: &StripInfo) -> Vec<String> {
        match &*s.id {
            "in0" | "in1" => vec!["MIC".into(), "LINE".into(), "HI-Z".into()],
            _ if s.preamp_layout.is_some() => vec!["MIC".into(), "LINE".into()],
            _ => vec![],
        }
    }
    fn records_pre_effects(&self, s: &StripInfo) -> bool {
        self.record_pre.get(&s.id).copied().unwrap_or(s.preamp_layout.is_none())
    }
    fn meters(&self, _: &StripInfo) -> MeterFrame {
        MeterFrame::default()
    }
    fn monitor(&self) -> MonitorState {
        self.monitor.clone()
    }
    fn monitor_meters(&self) -> MeterFrame {
        MeterFrame::default()
    }
    fn plugin_catalog(&self) -> Vec<PluginEntry> {
        self.catalog.clone()
    }
    fn plugin_scenes(&self) -> Vec<String> {
        self.scenes.clone()
    }
    fn status(&self) -> StatusInfo {
        self.status.clone()
    }
    fn engine_settings(&self) -> EngineSettings {
        self.settings.clone()
    }
    fn monitor_options(&self) -> MonitorOptions {
        self.monitor_options.clone()
    }
    fn cue_outputs(&self) -> Vec<CueOutput> {
        self.cues.clone()
    }
    fn headphone_outputs(&self) -> Vec<HeadphoneOutput> {
        self.headphones.clone()
    }
    fn strip_menu_state(&self, s: &StripInfo) -> crate::menu::StripMenuState {
        let mut st = self.strip_flags.get(&s.id).cloned().unwrap_or_default();
        if s.kind == StripKind::Input && st.linked.is_none() && s.preamp_layout.is_some() {
            st.linked = Some(s.stereo);
        }
        if s.shows_sr_convert && st.sr_convert.is_none() {
            st.sr_convert = Some(false);
        }
        st
    }
    fn menu_state(&self) -> crate::menu::MenuModelState {
        let clipboard = match &self.clipboard {
            Some(InsertState::Loaded { name, .. }) => Some(name.clone()),
            _ => None,
        };
        crate::menu::MenuModelState { can_undo: false, can_redo: false, post_fader_metering: Some(self.post_fader_metering), clipboard, recalled_scene: self.status.scene.as_ref().and_then(|n| self.scenes.iter().position(|s| s == n)).map(|i| i as u16) }
    }
    fn set_text(&mut self, id: &ControlId, text: &str) {
        match id.control {
            Control::SceneName(i) => {
                if let Some(n) = self.scenes.get_mut(i as usize) {
                    *n = text.trim().to_uppercase();
                }
            }
            Control::Name => {
                if let Some(m) = self.mains.get_mut(&id.strip) {
                    m.name = text.to_string();
                }
            }
            Control::Insert(k) => {
                if let Some(slot) = self.inserts.get_mut(&id.strip).and_then(|v| v.get_mut(k as usize)) {
                    *slot = InsertState::Loaded { name: text.to_string(), power: true };
                }
            }
            Control::Tempo => {
                if let Ok(t) = text.trim().parse::<f64>() {
                    self.status.tempo = t.clamp(1.0, 999.0);
                }
            }
            Control::SettingEdit(ref k) => self.apply_setting(k, &crate::settings::SettingOp::Text(text.to_string())),
            // The typed-value popover: dB through the fader taper (sends share it).
            Control::Fader | Control::SendLevel(_) => {
                let Ok(db) = text.trim().parse::<f64>() else { return };
                let v = scales::fader_tapered(db);
                match id.control {
                    Control::Fader => self.mains.get_mut(&id.strip).into_iter().for_each(|m| m.fader = v),
                    Control::SendLevel(k) => self.sends.get_mut(&id.strip).and_then(|s| s.get_mut(k as usize)).into_iter().for_each(|s| s.0.level = v),
                    _ => {}
                }
            }
            _ => {}
        }
    }
    fn properties(&self) -> Option<&dyn crate::settings::PropertySource> {
        Some(&self.props)
    }
    fn apply_setting_writes(&mut self, writes: Vec<crate::settings::Write>) {
        for w in &writes {
            self.props.apply(w);
            // Keep the status bar and the legacy settings sheet in step.
            if let crate::settings::Write::Set(path, v) = w {
                match path.as_str() {
                    "/SampleRate" => {
                        let hz = v.as_i64().map(|h| h as u32);
                        self.settings.sample_rate = hz;
                        self.status.sample_rate = hz;
                    }
                    "/ClockSource" => {
                        let c = v.as_str().map(str::to_owned);
                        self.settings.clock_source = c.clone();
                        self.status.clock_source = c;
                    }
                    _ => {}
                }
            }
        }
    }
    fn apply(&mut self, action: &Action) {
        if let Action::Reset(id, v) = action {
            return self.apply(&Action::Set(id.clone(), *v));
        }
        if let Action::Set(ControlId { control: Control::SettingSet(k), .. }, v) = action {
            return self.apply_setting(k, &crate::settings::SettingOp::Value(*v));
        }
        let press = matches!(action, Action::Tap(_) | Action::Toggle(_));
        let id = action.id();
        // Controls that are not per-strip state.
        if press {
            match &id.control {
                Control::SettingChoose(k, i) => return self.apply_setting(k, &crate::settings::SettingOp::Choose(*i as usize)),
                Control::SettingPress(k) => return self.apply_setting(k, &crate::settings::SettingOp::Press),
                Control::MonitorMono => return self.monitor.mono ^= true,
                Control::MonitorMute => return self.monitor.mute ^= true,
                Control::MonitorDim => return self.monitor_options.dim = self.monitor_options.dim.map(|d| !d),
                Control::Talkback => return self.monitor_options.talkback = self.monitor_options.talkback.map(|t| !t),
                Control::TalkbackToMonitor => {
                    return self.monitor_options.talkback_to_monitor = self.monitor_options.talkback_to_monitor.map(|t| !t);
                }
                Control::CrSource(k) => return self.monitor_options.cr_source = *k,
                Control::MonitorAlt(k) => return self.monitor_options.alt_selection = *k,
                Control::CueSource(k) => return if let Some(c) = self.cues.get_mut(*k as usize) { c.source_cue ^= true },
                Control::CueMono(k) => return if let Some(c) = self.cues.get_mut(*k as usize) { c.mono ^= true },
                Control::HeadphoneSource { output, option } => return if let Some(h) = self.headphones.get_mut(*output as usize) { h.source = *option as usize },
                Control::AllRecordPreEffects(pre) => return self.record_pre.values_mut().for_each(|v| *v = *pre),
                Control::ClearSolos => return self.mains.values_mut().for_each(|m| m.solo = false),
                Control::ClearClips => return,
                Control::SampleRate(i) => {
                    self.settings.sample_rate = self.settings.sample_rates.get(*i as usize).copied();
                    return self.status.sample_rate = self.settings.connected.then_some(self.settings.sample_rate).flatten();
                }
                Control::ClockSource(i) => return self.settings.clock_source = self.settings.clock_sources.get(*i as usize).cloned(),
                Control::BufferSize(i) => return self.settings.buffer_size = self.settings.buffer_sizes.get(*i as usize).copied(),
                Control::RecallScene(i) => return self.status.scene = self.scenes.get(*i as usize).cloned(),
                Control::SnapshotRequest(r) => return self.snapshots.apply(r),
                Control::SceneCommand(op, i) => {
                    use crate::menu::SceneOp;
                    let i = *i as usize;
                    if self.scenes.len() <= i {
                        self.scenes.resize(i + 1, "EMPTY SCENE".into());
                    }
                    match op {
                        SceneOp::Recall => self.status.scene = self.scenes.get(i).cloned(),
                        SceneOp::Clear => self.scenes[i] = "EMPTY SCENE".into(),
                        SceneOp::Save if crate::menu::scene_is_empty(&self.scenes[i]) => self.scenes[i] = format!("SCENE {}", i + 1),
                        _ => {}
                    }
                    return;
                }
                Control::WritesEnabled => return self.settings.writes_enabled ^= true,
                Control::Menu(cmd) => return self.apply_menu(&id.strip, cmd),
                _ => {}
            }
        }
        match action {
            Action::Set(id, v) => {
                if id.control == Control::MonitorLevel {
                    self.monitor.level = *v;
                    return;
                }
                if let Control::AltTrim(k) = id.control {
                    if let Some(t) = self.monitor_options.alt_trim.get_mut(k as usize) {
                        *t = *v;
                    }
                    return;
                }
                if id.control == Control::DimLevel {
                    self.monitor_options.dim_level = *v;
                    self.monitor_options.dim_readout = format!("-{:.0} dB", v * 60.0);
                    return;
                }
                if let Control::SendLevel(k) | Control::SendPan(k) = id.control {
                    if let Some((s, _)) = self.sends.get_mut(&id.strip).and_then(|v| v.get_mut(k as usize)) {
                        if let Control::SendPan(_) = id.control {
                            s.pan = *v;
                        } else {
                            s.level = *v;
                        }
                    }
                    return;
                }
                // TALK released after a long press: momentary.
                if id.control == Control::Talkback {
                    self.monitor_options.talkback = self.monitor_options.talkback.map(|_| *v >= 0.5);
                    return;
                }
                let Some(m) = self.mains.get_mut(&id.strip) else { return };
                match &id.control {
                    Control::Fader => {
                        m.fader = *v;
                        m.readout = scales::format_db(scales::fader_db(*v));
                    }
                    Control::PanL => m.pan_l = *v,
                    Control::PanR => m.pan_r = *v,
                    Control::Preamp(c) if &**c == "preamp_gain" => {
                        if let Some(p) = self.preamps.get_mut(&id.strip) {
                            p.gain = *v;
                            p.gain_text = format!("{:.0} dB", 10.0 + 55.0 * v);
                        }
                    }
                    _ => {}
                }
            }
            Action::Toggle(id) | Action::Tap(id) => {
                let strip = id.strip.clone();
                match &id.control {
                    Control::Solo | Control::Mute | Control::PrePost | Control::Mono => {
                        if let Some(m) = self.mains.get_mut(&strip) {
                            match id.control {
                                Control::Solo => m.solo ^= true,
                                Control::Mute => m.mute ^= true,
                                Control::PrePost => m.pre ^= true,
                                _ => m.mono ^= true,
                            }
                        }
                    }
                    Control::RecMon => {
                        let cur = self.strip(&strip).map(|s| self.records_pre_effects(s)).unwrap_or(false);
                        self.record_pre.insert(strip, !cur);
                    }
                    Control::Isolate => {
                        if !self.isolated.remove(&strip) {
                            self.isolated.insert(strip);
                        }
                    }
                    Control::SendMute(k) | Control::SendPre(k) => {
                        if let Some((s, pre)) = self.sends.get_mut(&strip).and_then(|v| v.get_mut(*k as usize)) {
                            match id.control {
                                Control::SendMute(_) => s.muted ^= true,
                                _ => *pre = pre.map(|p| !p),
                            }
                        }
                    }
                    Control::InputSource(i) => {
                        let sources = self.strip(&strip).map(|s| self.input_sources(s)).unwrap_or_default();
                        if let (Some(src), Some(p)) = (sources.get(*i as usize), self.preamps.get_mut(&strip)) {
                            p.source = src.clone();
                        }
                    }
                    Control::InsertPower(k) | Control::InsertRemove(k) | Control::InsertCopy(k) => {
                        let Some(slot) = self.inserts.get_mut(&strip).and_then(|v| v.get_mut(*k as usize)) else { return };
                        match (&id.control, &mut *slot) {
                            (Control::InsertPower(_), InsertState::Loaded { power, .. }) => *power ^= true,
                            (Control::InsertRemove(_), _) => *slot = InsertState::Empty,
                            (Control::InsertCopy(_), InsertState::Loaded { .. }) => self.clipboard = Some(slot.clone()),
                            (Control::InsertCopy(_), InsertState::Empty) => {
                                if let Some(c) = self.clipboard.take() {
                                    *slot = c;
                                }
                            }
                            _ => {}
                        }
                    }
                    // Drag and drop (`crate::dnd`): the engine's `MoveInsert`, locally.
                    Control::InsertMove { slot, from, from_slot, copy } => {
                        let src = self.inserts.get(from).and_then(|v| v.get(*from_slot as usize)).cloned();
                        let Some(src @ InsertState::Loaded { .. }) = src else { return };
                        if let Some(dst) = self.inserts.get_mut(&strip).and_then(|v| v.get_mut(*slot as usize)) {
                            *dst = src;
                        }
                        if !*copy && (*from != strip || from_slot != slot) {
                            if let Some(old) = self.inserts.get_mut(from).and_then(|v| v.get_mut(*from_slot as usize)) {
                                *old = InsertState::Empty;
                            }
                        }
                    }
                    Control::Preamp(c) => {
                        if let Some(p) = self.preamps.get_mut(&strip) {
                            match &**c {
                                "48v" if p.phantom_enabled => p.phantom ^= true,
                                "pad" if p.pad_enabled => p.pad ^= true,
                                "low_cut" => p.low_cut ^= true,
                                "phase" => p.phase ^= true,
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

impl PreviewMixer {
    /// Model-scope menu commands (`menu::MenuCmd`).
    fn apply_menu(&mut self, strip: &Arc<str>, cmd: &crate::menu::MenuCmd) {
        use crate::menu::{MenuCmd as M, PluginScope};
        let default = self.strip(strip).map(|s| self.strip_menu_state(s)).unwrap_or_default();
        let flags = self.strip_flags.entry(strip.clone()).or_insert(default);
        match cmd {
            M::SoloSafe => flags.solo_safe ^= true,
            M::Link => flags.linked = flags.linked.map(|l| !l),
            M::Hide => flags.hidden ^= true,
            M::SrConvert(on) => flags.sr_convert = Some(*on),
            M::PostFaderMetering(post) => self.post_fader_metering = *post,
            M::InsertPaste(k) => {
                let Some(c) = self.clipboard.clone() else { return };
                if let Some(slot) = self.inserts.get_mut(strip).and_then(|v| v.get_mut(*k as usize)) {
                    *slot = c;
                }
            }
            M::StripInserts(op) => {
                if let Some(v) = self.inserts.get_mut(strip) {
                    v.iter_mut().for_each(|slot| plugin_op(slot, *op));
                }
            }
            M::App(c) => {
                let Some((op, scope)) = c.plugin_op() else { return };
                let auxes: HashSet<Arc<str>> = self.auxes.iter().map(|a| a.id.clone()).collect();
                for (id, v) in self.inserts.iter_mut() {
                    let aux = auxes.contains(id);
                    let hit = match scope {
                        PluginScope::All => true,
                        PluginScope::Inserts => !aux,
                        PluginScope::Aux => aux,
                        PluginScope::Unison => false,
                    };
                    if hit {
                        v.iter_mut().for_each(|slot| plugin_op(slot, op));
                    }
                }
            }
            _ => {}
        }
    }
}

fn plugin_op(slot: &mut InsertState, op: crate::menu::PluginOp) {
    use crate::menu::PluginOp;
    match (op, &mut *slot) {
        (PluginOp::Remove, _) => *slot = InsertState::Empty,
        (PluginOp::Enable, InsertState::Loaded { power, .. }) => *power = true,
        (PluginOp::Disable, InsertState::Loaded { power, .. }) => *power = false,
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn insert_labels_pick_like_console() {
        // p2: the 1176 Rev A on input 0 reads "1176 Rev A" (catalogue values sorted, shortest first).
        assert_eq!(insert_label_for("UAD UA 1176 Rev A", &names(&["1176 A", "1176 Rev A"])), "1176 Rev A");
        // p1: CL 1B mkII, not the 90-pt "TubeTech CL1B mkII".
        assert_eq!(insert_label_for("UAD Tube-Tech CL 1B mk II", &names(&["CL 1B mkII", "TubeTech CL1B mkII"])), "CL 1B mkII");
        // A name that fits stays; without short names only "UAD " goes.
        assert_eq!(insert_label_for("UAD dbx 160", &names(&["dbx 160"])), "dbx 160");
        assert_eq!(insert_label_for("UAD Pultec-Pro Legacy", &[]), "Pultec-Pro Legacy");
        assert_eq!(insert_label_for("UAD Ampex ATR-102", &names(&["ATR", "ATR-102", "Ampex ATR-102"])), "ATR-102");
    }
}
