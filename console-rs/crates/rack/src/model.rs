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

#[derive(Clone, Debug, PartialEq)]
pub struct SendState {
    pub name: String,
    pub level: f64,
    pub muted: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MonitorState {
    pub level: f64,
    pub readout: String,
    pub mono: bool,
    pub mute: bool,
}

/// Meter levels as 0…1 scale fractions plus clip LEDs.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MeterFrame {
    pub left: f64,
    pub right: f64,
    pub clip_left: bool,
    pub clip_right: bool,
}

/// One entry of the plug-in browser (`/plugins/P/{Name, Categories, Authorized, …}`).
#[derive(Clone, Debug, PartialEq)]
pub struct PluginEntry {
    /// Catalogue name, as written to an insert's `EffectName` ("UAD 1176LN Rev E").
    pub name: String,
    /// One of `MixerModel::plugin_categories`.
    pub category: String,
    /// Authorized/usable (false → DISABLED badge).
    pub enabled: bool,
    pub favorite: bool,
}

impl PluginEntry {
    pub fn new(name: &str, category: &str, enabled: bool) -> Self {
        Self { name: name.into(), category: category.into(), enabled, favorite: false }
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
    /// Loads 0…1.
    pub dsp_load: f32,
    pub pgm_load: f32,
    pub mem_load: f32,
    /// Recalled plug-in scene; None → "(NONE)".
    pub scene: Option<String>,
}

impl Default for StatusInfo {
    fn default() -> Self {
        Self { tempo: 120.0, sample_rate: None, clock_source: None, dsp_load: 0.0, pgm_load: 0.0, mem_load: 0.0, scene: None }
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
    fn inputs(&self) -> Vec<StripInfo>;
    fn auxes(&self) -> Vec<StripInfo>;
    fn main(&self, s: &StripInfo) -> MainState;
    fn preamp(&self, s: &StripInfo) -> Option<PreampState>;
    fn inserts(&self, s: &StripInfo) -> Vec<InsertState>;
    fn unison(&self, s: &StripInfo) -> InsertState;
    fn sends(&self, s: &StripInfo) -> Vec<SendState>;
    /// UAD REC (false) vs MON (true): `RecordPreEffects`.
    fn records_pre_effects(&self, s: &StripInfo) -> bool;
    fn meters(&self, s: &StripInfo) -> MeterFrame;
    fn monitor(&self) -> MonitorState;
    fn monitor_meters(&self) -> MeterFrame;
    fn apply(&mut self, action: &Action);
    /// Applies pending external updates (engine pushes); true if anything changed.
    /// Called once per frame by the app.
    fn pump(&mut self) -> bool {
        false
    }

    // ---- UI surfaces; every method has a default so existing models keep compiling. ----

    /// Input strip sends: AUX 1, AUX 2, CUE 1…n (`inputs/N/sends/S`), drawn in the Sends cell.
    fn input_sends(&self, _s: &StripInfo) -> Vec<SendState> {
        Vec::new()
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
    pub status: StatusInfo,
    pub settings: EngineSettings,
    pub monitor_options: MonitorOptions,
    pub cues: Vec<CueOutput>,
    pub headphones: Vec<HeadphoneOutput>,
    /// Insert copied with COPY, pasted by the next COPY on an empty slot.
    pub clipboard: Option<InsertState>,
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
        };
        let aux = |id: &str, title: &str| StripInfo { id: id.into(), title: title.into(), kind: StripKind::Aux, stereo: true, preamp_layout: None, shows_sr_convert: false };
        let inputs = vec![
            input("in0", "ANALOG 1", false, true, false),
            input("in1", "ANALOG 2", false, true, false),
            input("in2", "ANALOG 3/4", true, true, false),
            input("in4", "S/PDIF", true, false, true),
            input("v0", "VIRTUAL 1/2", true, false, false),
            input("v2", "VIRTUAL 3/4", true, false, false),
        ];
        let auxes = vec![aux("aux0", "AUX 1"), aux("aux1", "AUX 2")];
        let names = ["SM7B", "GTR", "Dante AVIO", "S/PDIF", "Macbook", "Macbook 3/4"];
        let mut mains = HashMap::new();
        for (s, n) in inputs.iter().zip(names) {
            let st = if s.stereo { MainState { pan_l: 0.0, pan_r: 1.0, ..Default::default() } } else { MainState::default() };
            mains.insert(s.id.clone(), MainState { name: n.into(), mute: true, ..st });
        }
        mains.insert("aux0".into(), MainState { name: "AUX 1".into(), fader: scales::fader_tapered(-9.4), readout: "-9.4".into(), ..Default::default() });
        mains.insert("aux1".into(), MainState { name: "AUX 2".into(), ..Default::default() });
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
        let send = |name: &str, level: f64, muted: bool| SendState { name: name.into(), level, muted };
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
        let record_pre = inputs.iter().map(|s| (s.id.clone(), s.preamp_layout.is_none())).collect();
        Self {
            inputs,
            auxes,
            mains,
            preamps,
            monitor: MonitorState { level: 0.09, readout: "-52.0 dB".into(), mono: false, mute: false },
            inserts,
            sends,
            record_pre,
            isolated: HashSet::from([Arc::from("in2")]),
            catalog: demo_catalog(),
            scenes: vec!["Tracking".into(), "Overdubs".into(), "Mix Prep".into()],
            status: StatusInfo { tempo: 120.0, sample_rate: None, clock_source: None, dsp_load: 0.42, pgm_load: 0.31, mem_load: 0.18, scene: None },
            settings: EngineSettings {
                host: "127.0.0.1:4710".into(),
                connected: false,
                sample_rate: Some(48_000),
                sample_rates: vec![44_100, 48_000, 88_200, 96_000, 176_400, 192_000],
                clock_source: Some("Internal".into()),
                clock_sources: vec!["Internal".into(), "S/PDIF".into(), "Word Clock".into()],
                buffer_size: Some(256),
                buffer_sizes: vec![32, 64, 128, 256, 512, 1024],
                writes_enabled: false,
            },
            monitor_options: MonitorOptions { dim: Some(false), dim_level: 0.3, dim_readout: "-17 dB".into(), talkback: Some(false), alt_count: 0, alt_selection: 0 },
            cues: vec![
                CueOutput { name: "CUE 1".into(), source_cue: true, mono: false, destination: "LINE 3/4".into() },
                CueOutput { name: "CUE 2".into(), source_cue: false, mono: true, destination: "HP 2".into() },
            ],
            headphones: vec![
                HeadphoneOutput { name: "HP 1".into(), source: 0, options: vec!["MON".into(), "CUE 1".into(), "CUE 2".into()] },
                HeadphoneOutput { name: "HP 2".into(), source: 2, options: vec!["MON".into(), "CUE 1".into(), "CUE 2".into()] },
            ],
            clipboard: None,
        }
    }
}

impl PreviewMixer {
    fn strip(&self, id: &str) -> Option<&StripInfo> {
        self.inputs.iter().chain(&self.auxes).find(|s| &*s.id == id)
    }
}

impl MixerModel for PreviewMixer {
    fn session_name(&self) -> String {
        "Session*".into()
    }
    fn inputs(&self) -> Vec<StripInfo> {
        self.inputs.clone()
    }
    fn auxes(&self) -> Vec<StripInfo> {
        self.auxes.clone()
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
        if s.kind == StripKind::Input { self.sends.get(&s.id).map(|v| v.iter().map(|(s, _)| s.clone()).collect()).unwrap_or_default() } else { vec![] }
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
    fn set_text(&mut self, id: &ControlId, text: &str) {
        match id.control {
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
            _ => {}
        }
    }
    fn apply(&mut self, action: &Action) {
        let press = matches!(action, Action::Tap(_) | Action::Toggle(_));
        let id = action.id();
        // Controls that are not per-strip state.
        if press {
            match &id.control {
                Control::MonitorMono => return self.monitor.mono ^= true,
                Control::MonitorMute => return self.monitor.mute ^= true,
                Control::MonitorDim => return self.monitor_options.dim = self.monitor_options.dim.map(|d| !d),
                Control::Talkback => return self.monitor_options.talkback = self.monitor_options.talkback.map(|t| !t),
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
                Control::WritesEnabled => return self.settings.writes_enabled ^= true,
                _ => {}
            }
        }
        match action {
            Action::Set(id, v) => {
                if id.control == Control::MonitorLevel {
                    self.monitor.level = *v;
                    return;
                }
                if id.control == Control::DimLevel {
                    self.monitor_options.dim_level = *v;
                    self.monitor_options.dim_readout = format!("-{:.0} dB", v * 60.0);
                    return;
                }
                if let Control::SendLevel(k) = id.control {
                    if let Some((s, _)) = self.sends.get_mut(&id.strip).and_then(|v| v.get_mut(k as usize)) {
                        s.level = *v;
                    }
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
