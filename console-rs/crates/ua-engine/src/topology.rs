//! Path builders for every control Console's mixer shows, derived from the engine tree.
//!
//! Names are the verified ones from the real tree (engine 11.8.2 and 12.0.0): sends mute with
//! `Bypass` (there is no `sends/S/Mute`), meters clip with `MeterClip` (not `Clip`), the talkback
//! switch is `/TalkbackOn` at the root, and output indices are discovered by property and
//! `IOType` (x4 monitor = outputs/18, x8p = outputs/22), never hard-coded.

use crate::tree::Tree;
use crate::value::Value;

fn s(tree: &Tree, path: &str) -> Option<String> {
    tree.value(path).and_then(Value::as_str).map(str::to_owned)
}

fn b(tree: &Tree, path: &str) -> Option<bool> {
    tree.value(path).and_then(Value::as_bool)
}

/// One meter node, `…/meters/K`. `base` is the [`MeterStore`](crate::MeterStore) key.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct MeterPaths {
    pub base: String,
}

impl MeterPaths {
    pub fn new(base: impl Into<String>) -> MeterPaths {
        MeterPaths { base: base.into() }
    }
    pub fn level(&self) -> String {
        format!("{}/MeterLevel", self.base)
    }
    pub fn peak(&self) -> String {
        format!("{}/MeterPeakLevel", self.base)
    }
    pub fn clip(&self) -> String {
        format!("{}/MeterClip", self.base)
    }
    pub fn all(&self) -> [String; 3] {
        [self.level(), self.peak(), self.clip()]
    }
}

/// A send from a strip to a bus: `…/sends/S`.
#[derive(Clone, Debug, PartialEq)]
pub struct SendPaths {
    pub index: usize,
    /// The send's `ID`: `aux1`, `aux2`, `cue1` … `cue4`.
    pub id: String,
    /// The bus label from the send's `Name` (e.g. `AUX 1`, `CUE 1`).
    pub name: String,
    pub base: String,
    /// False for cue sends beyond `/CueBusCount` (Console hides them).
    pub is_available: bool,
    pub meters: Vec<MeterPaths>,
}

impl SendPaths {
    pub fn is_aux(&self) -> bool {
        self.id.starts_with("aux")
    }
    pub fn is_cue(&self) -> bool {
        self.id.starts_with("cue")
    }
    /// 1-based bus number parsed from `id`.
    pub fn bus_number(&self) -> Option<u32> {
        self.id
            .trim_start_matches(|c: char| !c.is_ascii_digit())
            .parse()
            .ok()
    }
    /// Send level, tapered 0…1 (fader taper). Write this.
    pub fn gain(&self) -> String {
        format!("{}/GainTapered", self.base)
    }
    /// Send level in dB (−144…+12). Read for the readout.
    pub fn gain_db(&self) -> String {
        format!("{}/Gain", self.base)
    }
    /// Send on/off. `true` = send muted.
    pub fn bypass(&self) -> String {
        format!("{}/Bypass", self.base)
    }
    /// Send pan −1…1 (null on aux→cue sends).
    pub fn pan(&self) -> String {
        format!("{}/Pan", self.base)
    }
    /// −1 follow bus default, 0 pre, 1 post.
    pub fn post_fader_override(&self) -> String {
        format!("{}/PostFaderOverride", self.base)
    }
    pub fn name_path(&self) -> String {
        format!("{}/Name", self.base)
    }
    pub fn id_path(&self) -> String {
        format!("{}/ID", self.base)
    }
}

/// An insert slot: `…/effects/K` (also the Unison slot `preamps/0/effects/0`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct InsertPaths {
    pub index: usize,
    pub base: String,
}

impl InsertPaths {
    /// The 11 properties Console subscribes per slot, in its order.
    pub const SUBSCRIBED: [&'static str; 11] = [
        "EffectName",
        "Power",
        "EffectInstance",
        "Status",
        "CanPastePluginBarSettings",
        "Preset",
        "Dirty",
        "ScenePluginUuid",
        "PluginExistsInCurrentScene",
        "PluginExistsInAnyScene",
        "ChangeCount",
    ];

    pub fn effect_name(&self) -> String {
        format!("{}/EffectName", self.base)
    }
    pub fn power(&self) -> String {
        format!("{}/Power", self.base)
    }
    pub fn preset(&self) -> String {
        format!("{}/Preset", self.base)
    }
    pub fn status(&self) -> String {
        format!("{}/Status", self.base)
    }
    pub fn effect_instance(&self) -> String {
        format!("{}/EffectInstance", self.base)
    }
    pub fn dirty(&self) -> String {
        format!("{}/Dirty", self.base)
    }
    pub fn change_count(&self) -> String {
        format!("{}/ChangeCount", self.base)
    }
    pub fn latency(&self) -> String {
        format!("{}/Latency", self.base)
    }
    pub fn vendor_specific(&self) -> String {
        format!("{}/VendorSpecific", self.base)
    }

    /// A slot from its node path; the index is the last component (`…/effects/K`).
    pub fn from_slot(base: impl Into<String>) -> InsertPaths {
        let base = base.into();
        let index = base
            .rsplit('/')
            .next()
            .and_then(|k| k.parse().ok())
            .unwrap_or(0);
        InsertPaths { index, base }
    }

    /// `<slot>/parameters`, present only while a plug-in is loaded.
    pub fn parameters(&self) -> String {
        format!("{}/parameters", self.base)
    }

    /// Paths of plug-in parameter `k` in this slot.
    pub fn parameter(&self, k: usize) -> PluginParamPaths {
        PluginParamPaths {
            slot: self.base.clone(),
            index: k,
        }
    }

    /// True for the Unison slot (`…/preamps/0/effects/0`).
    pub fn is_unison(&self) -> bool {
        self.base.contains("/preamps/")
    }
}

/// Paths of plug-in parameter `index` in a slot: `<slot>/parameters/<index>/…`.
///
/// The engine publishes a `parameters` node under a slot only while a plug-in is loaded there
/// (and replaces it when the plug-in is swapped). Each parameter is a node with `Name`,
/// `NormalizedValue` (float 0…1), `StringValue` (display text) and, for stepped parameters,
/// `StepValue` (string with a `values` list). The index is the plug-in's own parameter ordinal
/// (its VST index), the same index ConsoleLink's specs use.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PluginParamPaths {
    /// Slot node, e.g. `/devices/0/inputs/0/effects/1` or `…/preamps/0/effects/0` (Unison).
    pub slot: String,
    pub index: usize,
}

impl PluginParamPaths {
    pub fn base(&self) -> String {
        format!("{}/parameters/{}", self.slot, self.index)
    }
    /// 0…1. Console's plug-in host reads and writes this one (`set …/NormalizedValue/value v`).
    pub fn normalized(&self) -> String {
        format!("{}/NormalizedValue", self.base())
    }
    /// The plug-in's display text for the current value ("2.80", "Off"). Pushed by the engine.
    pub fn string(&self) -> String {
        format!("{}/StringValue", self.base())
    }
    /// Step label with a `values` list; only on stepped parameters.
    pub fn step(&self) -> String {
        format!("{}/StepValue", self.base())
    }
    pub fn name(&self) -> String {
        format!("{}/Name", self.base())
    }
    /// What Console subscribes per parameter (NormalizedValue, StringValue, StepValue).
    pub fn subscribed(&self) -> [String; 3] {
        [self.normalized(), self.string(), self.step()]
    }
}

/// One plug-in parameter as the engine describes it.
#[derive(Clone, Debug, PartialEq)]
pub struct PluginParameter {
    pub paths: PluginParamPaths,
    pub name: String,
    /// `StepValue/values` labels, in order; empty for continuous parameters.
    pub steps: Vec<String>,
}

impl PluginParameter {
    pub fn index(&self) -> usize {
        self.paths.index
    }

    pub fn is_stepped(&self) -> bool {
        self.steps.len() > 1
    }

    /// Normalized value of step `i`: steps are spread evenly over 0…1 (step = round(v·(n−1))).
    pub fn normalized_of_step(&self, i: usize) -> f64 {
        if self.steps.len() < 2 {
            return 0.0;
        }
        i.min(self.steps.len() - 1) as f64 / (self.steps.len() - 1) as f64
    }

    pub fn step_of(&self, normalized: f64) -> usize {
        if self.steps.len() < 2 {
            return 0;
        }
        let n = self.steps.len() - 1;
        ((normalized * n as f64).round().max(0.0) as usize).min(n)
    }

    /// The parameters of the plug-in loaded in `slot`, by index (empty when the slot is empty).
    pub fn list(tree: &Tree, slot: &InsertPaths) -> Vec<PluginParameter> {
        let mut ks = tree.indices(&slot.parameters());
        ks.sort_unstable();
        ks.into_iter()
            .map(|k| {
                let paths = slot.parameter(k);
                let name = s(tree, &paths.name()).unwrap_or_else(|| format!("Parameter {k}"));
                let steps = tree
                    .meta(&paths.step())
                    .map(|m| {
                        m.choices()
                            .iter()
                            .filter_map(|v| v.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default();
                PluginParameter { paths, name, steps }
            })
            .collect()
    }
}

/// Preamp controls: `/devices/D/inputs/N/preamps/0`.
#[derive(Clone, Debug, PartialEq)]
pub struct PreampPaths {
    pub base: String,
    /// Present only on front-jack inputs (inputs 0-1 on x4/x8p).
    pub has_hi_z: bool,
    /// `48V/enabled`: whether Console shows the button (false in Line mode).
    pub phantom_enabled: bool,
    /// `Pad/enabled`.
    pub pad_enabled: bool,
    pub unison: InsertPaths,
}

impl PreampPaths {
    /// Preamp gain, tapered 0…1 ([`tapers::preamp`](crate::tapers::preamp)). Write this.
    pub fn gain(&self) -> String {
        format!("{}/GainTapered", self.base)
    }
    /// Preamp gain in dB (10…65 for Mic). Read for the readout.
    pub fn gain_db(&self) -> String {
        format!("{}/Gain", self.base)
    }
    /// +48V.
    pub fn phantom(&self) -> String {
        format!("{}/48V", self.base)
    }
    pub fn pad(&self) -> String {
        format!("{}/Pad", self.base)
    }
    pub fn low_cut(&self) -> String {
        format!("{}/LowCut", self.base)
    }
    pub fn phase(&self) -> String {
        format!("{}/Phase", self.base)
    }
    /// Read-only front-jack sense.
    pub fn hi_z(&self) -> String {
        format!("{}/HiZ", self.base)
    }
    pub fn knob_steps(&self) -> String {
        format!("{}/PreampInputKnobSteps", self.base)
    }
    pub fn custom_display_text(&self) -> String {
        format!("{}/PreampGainCustomDisplayText", self.base)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InputKind {
    Analog,
    Adat,
    Spdif,
    Aes,
    Madi,
    Dante,
    Virtual,
    Talkback,
    None,
    Other,
}

impl InputKind {
    pub fn classify(io: Option<&str>, has_preamp: bool) -> InputKind {
        match io {
            None | Some("None") => InputKind::None,
            Some("TalkbackMic") => InputKind::Talkback,
            Some("ADAT") => InputKind::Adat,
            Some("S/PDIF" | "SPDIF") => InputKind::Spdif,
            Some("AES/EBU" | "AESEBU") => InputKind::Aes,
            Some("MADI") => InputKind::Madi,
            Some("Virtual") => InputKind::Virtual,
            Some("Dante") => {
                if has_preamp {
                    InputKind::Analog
                } else {
                    InputKind::Dante
                }
            }
            Some("Mic" | "Line" | "MicLineHiZ" | "HiZ") => InputKind::Analog,
            Some(_) => {
                if has_preamp {
                    InputKind::Analog
                } else {
                    InputKind::Other
                }
            }
        }
    }
}

/// An input channel strip, with a stereo pair folded into its left (even) input.
#[derive(Clone, Debug, PartialEq)]
pub struct InputStrip {
    pub device: usize,
    /// The left (or mono) input index.
    pub index: usize,
    /// The right input of a stereo pair.
    pub right_index: Option<usize>,
    /// `/devices/D/inputs/N` of the left (or mono) input. All strip controls live here, including
    /// `Pan2` and `meters/1` for the right leg.
    pub base: String,
    /// Display name: `StereoName` for a pair, else `Name`.
    pub name: String,
    /// Factory label (`Name/default`), e.g. `ANALOG 1`.
    pub default_name: Option<String>,
    pub io_type: Option<String>,
    /// `IOType/values` (e.g. Line, Mic, Dante) when the input type is switchable.
    pub io_type_choices: Vec<String>,
    /// `IOType/enabled`.
    pub io_type_selectable: bool,
    /// `Stereo/enabled`: whether the link button is offered.
    pub stereo_linkable: bool,
    pub kind: InputKind,
    pub is_active: bool,
    pub is_hidden: bool,
    pub enabled_by_user: bool,
    pub preamp: Option<PreampPaths>,
    pub sends: Vec<SendPaths>,
    pub inserts: Vec<InsertPaths>,
    /// `[L]` for mono, `[L, R]` for a stereo pair.
    pub meters: Vec<MeterPaths>,
    /// `CustomIntegratedPreampInputMeter` (dBFS), present on every input in the tree.
    pub preamp_meter: Option<String>,
}

impl InputStrip {
    pub fn is_stereo(&self) -> bool {
        self.right_index.is_some()
    }
    /// Shown in Console's mixer: active, enabled, not hidden, a real input and not the talkback mic.
    pub fn is_visible(&self) -> bool {
        self.is_active
            && self.enabled_by_user
            && !self.is_hidden
            && self.kind != InputKind::None
            && self.kind != InputKind::Talkback
    }
    fn p(&self, name: &str) -> String {
        format!("{}/{name}", self.base)
    }
    pub fn fader(&self) -> String {
        self.p("FaderLevelTapered")
    }
    pub fn fader_db(&self) -> String {
        self.p("FaderLevel")
    }
    pub fn pan(&self) -> String {
        self.p("Pan")
    }
    /// Right-leg pan of a stereo pair.
    pub fn pan2(&self) -> String {
        self.p("Pan2")
    }
    pub fn solo(&self) -> String {
        self.p("Solo")
    }
    pub fn solo_safe(&self) -> String {
        self.p("SoloSafe")
    }
    pub fn mute(&self) -> String {
        self.p("Mute")
    }
    pub fn isolate(&self) -> String {
        self.p("Isolate")
    }
    pub fn stereo(&self) -> String {
        self.p("Stereo")
    }
    pub fn io_type_path(&self) -> String {
        self.p("IOType")
    }
    pub fn name_path(&self) -> String {
        self.p("Name")
    }
    pub fn stereo_name_path(&self) -> String {
        self.p("StereoName")
    }
    /// The name property the strip label shows (`StereoName` for a pair).
    pub fn display_name_path(&self) -> String {
        if self.is_stereo() {
            self.stereo_name_path()
        } else {
            self.name_path()
        }
    }
    pub fn record_pre_effects(&self) -> String {
        self.p("RecordPreEffects")
    }
    pub fn channel_hidden(&self) -> String {
        self.p("ChannelHidden")
    }
    pub fn talkback_enabled(&self) -> String {
        self.p("TalkbackEnabled")
    }
    pub fn delay_comp_group(&self) -> String {
        self.p("DelayCompGroup")
    }
    pub fn preset(&self) -> String {
        self.p("Preset")
    }
    pub fn active(&self) -> String {
        self.p("Active")
    }
    /// `SRConvert` (S/PDIF inputs only).
    pub fn sr_convert(&self) -> String {
        self.p("SRConvert")
    }
    /// The send whose `ID` is `id` (`aux1`, `cue2`, …).
    pub fn send(&self, id: &str) -> Option<&SendPaths> {
        self.sends.iter().find(|s| s.id == id)
    }
    pub fn insert(&self, k: usize) -> Option<&InsertPaths> {
        self.inserts.get(k)
    }
}

/// An aux return: `/devices/D/auxs/A`.
#[derive(Clone, Debug, PartialEq)]
pub struct AuxStrip {
    pub index: usize,
    pub base: String,
    pub name: String,
    pub is_active: bool,
    /// Aux → cue sends (`sends/0..3` = cue1..cue4).
    pub sends: Vec<SendPaths>,
    pub inserts: Vec<InsertPaths>,
    /// `[L, R]`.
    pub meters: Vec<MeterPaths>,
}

impl AuxStrip {
    fn p(&self, name: &str) -> String {
        format!("{}/{name}", self.base)
    }
    pub fn fader(&self) -> String {
        self.p("FaderLevelTapered")
    }
    pub fn fader_db(&self) -> String {
        self.p("FaderLevel")
    }
    pub fn mute(&self) -> String {
        self.p("Mute")
    }
    pub fn mono(&self) -> String {
        self.p("MixToMono")
    }
    pub fn send_post_fader(&self) -> String {
        self.p("SendPostFader")
    }
    pub fn name_path(&self) -> String {
        self.p("Name")
    }
    pub fn preset(&self) -> String {
        self.p("Preset")
    }
    pub fn send(&self, id: &str) -> Option<&SendPaths> {
        self.sends.iter().find(|s| s.id == id)
    }
}

/// A cue output (`IOType` `Cue`): its mix source, mono and destination.
#[derive(Clone, Debug, PartialEq)]
pub struct CueOutput {
    /// Output index (x4: 14-17, x8p: 18-21).
    pub index: usize,
    /// 1-based cue bus number.
    pub number: usize,
    pub base: String,
    pub name: String,
    /// False beyond `/CueBusCount`.
    pub is_active: bool,
    pub meters: Vec<MeterPaths>,
}

impl CueOutput {
    /// Bus id used by sends: `cue1` …
    pub fn bus(&self) -> String {
        format!("cue{}", self.number)
    }
    pub fn mix_in_source(&self) -> String {
        format!("{}/MixInSource", self.base)
    }
    pub fn mono(&self) -> String {
        format!("{}/MixToMono", self.base)
    }
    pub fn output_destination(&self) -> String {
        format!("{}/OutputDestination", self.base)
    }
    pub fn selected_on_front(&self) -> String {
        format!("{}/SelectedOnFront", self.base)
    }
}

/// A headphone output (`IOType` `Headphone`). No software level on x4/x8p (hardware knob).
#[derive(Clone, Debug, PartialEq)]
pub struct HeadphoneOutput {
    pub index: usize,
    pub number: usize,
    pub base: String,
    pub name: String,
    /// `CRMonitorLevelTapered` when this model has a software HP level (none captured yet).
    pub level: Option<String>,
    pub meters: Vec<MeterPaths>,
}

impl HeadphoneOutput {
    /// `default`, `cue1` … `cue4`, `none`.
    pub fn mix_in_source(&self) -> String {
        format!("{}/MixInSource", self.base)
    }
}

/// The monitor output, found by its `CRMonitorLevelTapered` property (x4: 18, x8p: 22).
#[derive(Clone, Debug, PartialEq)]
pub struct MonitorOutput {
    pub device: usize,
    pub index: usize,
    pub base: String,
    pub name: String,
    /// `[L, R]`.
    pub meters: Vec<MeterPaths>,
}

impl MonitorOutput {
    fn p(&self, name: &str) -> String {
        format!("{}/{name}", self.base)
    }
    /// Monitor level, tapered 0…1 ([`tapers::monitor`](crate::tapers::monitor)). Write this.
    pub fn level(&self) -> String {
        self.p("CRMonitorLevelTapered")
    }
    /// Monitor level in dB (−96…0, 0.5 dB steps). Read for the readout.
    pub fn level_db(&self) -> String {
        self.p("CRMonitorLevel")
    }
    pub fn mute(&self) -> String {
        self.p("Mute")
    }
    pub fn dim(&self) -> String {
        self.p("DimOn")
    }
    /// Device-level dim depth in dB (0…60; hardware snaps to 7 steps).
    pub fn dim_attenuation(&self) -> String {
        format!("/devices/{}/DimAttenuation", self.device)
    }
    pub fn mono(&self) -> String {
        self.p("MixToMono")
    }
    /// `mon`, `cue1` … `cue4`.
    pub fn source(&self) -> String {
        self.p("MixInSource")
    }
    pub fn mirrors_to_digital(&self) -> String {
        self.p("MirrorsToDigital")
    }
    pub fn digital_mirror_available(&self) -> String {
        self.p("DigitalMirrorAvailable")
    }
    pub fn alt_mon_selection(&self) -> String {
        format!("/devices/{}/AltMonSelection", self.device)
    }
}

/// Talkback lives at the root (`/TalkbackOn`), not on the monitor output.
#[derive(Clone, Debug, PartialEq)]
pub struct TalkbackPaths {
    pub device: usize,
    /// The talkback mic input (`IOType` `TalkbackMic`; x4 input 22, x8p input 26).
    pub strip: Option<InputStrip>,
}

impl TalkbackPaths {
    pub fn on(&self) -> String {
        "/TalkbackOn".to_owned()
    }
    pub fn in_physical_cr(&self) -> String {
        "/TalkbackInPhysicalCR".to_owned()
    }
    /// Root owner index (−1 = none).
    pub fn master(&self) -> String {
        "/TalkbackMaster".to_owned()
    }
    pub fn mic_select(&self) -> String {
        "/TalkbackMicSelect".to_owned()
    }
    pub fn device_master(&self) -> String {
        format!("/devices/{}/TalkbackMaster", self.device)
    }
}

/// The strips and outputs Console's mixer shows, derived from the tree.
#[derive(Clone, Debug, PartialEq)]
pub struct Topology {
    pub device: usize,
    pub device_name: Option<String>,
    /// Every input strip (pairs folded), visible or not, excluding the talkback mic.
    pub all_inputs: Vec<InputStrip>,
    /// The input strips Console shows, in order (the visible subset of `all_inputs`).
    pub inputs: Vec<InputStrip>,
    pub auxes: Vec<AuxStrip>,
    pub cues: Vec<CueOutput>,
    pub headphones: Vec<HeadphoneOutput>,
    pub monitor: Option<MonitorOutput>,
    pub talkback: TalkbackPaths,
    pub cue_bus_count: usize,
}

impl Default for Topology {
    fn default() -> Self {
        Topology::new(&Tree::new(), 0)
    }
}

impl Topology {
    /// The topology of device 0.
    pub fn from_tree(tree: &Tree) -> Topology {
        Topology::new(tree, 0)
    }

    pub fn new(tree: &Tree, device: usize) -> Topology {
        let dev = format!("/devices/{device}");
        let cue_count = tree
            .value("/CueBusCount")
            .and_then(Value::as_i64)
            .or_else(|| {
                tree.value(&format!("{dev}/CueBusCount"))
                    .and_then(Value::as_i64)
            })
            .unwrap_or(4)
            .max(0) as usize;
        let bld = Builder {
            tree,
            cue_bus_count: cue_count,
        };

        // Inputs, folding stereo pairs into the left (even) input.
        let mut inputs = Vec::new();
        let mut talkback_strip = None;
        let indices = tree.indices(&format!("{dev}/inputs"));
        let has = |n: usize| indices.contains(&n);
        for &n in &indices {
            let base = format!("{dev}/inputs/{n}");
            let stereo = b(tree, &format!("{base}/Stereo")).unwrap_or(false);
            if stereo
                && n % 2 == 1
                && has(n - 1)
                && b(tree, &format!("{dev}/inputs/{}/Stereo", n - 1)).unwrap_or(false)
            {
                continue; // right leg of a pair
            }
            let right = (stereo && n % 2 == 0 && has(n + 1)).then_some(n + 1);
            let strip = bld.input(device, n, base, right);
            if strip.kind == InputKind::Talkback {
                talkback_strip = Some(strip);
            } else {
                inputs.push(strip);
            }
        }

        let auxes = tree
            .indices(&format!("{dev}/auxs"))
            .into_iter()
            .map(|a| {
                let base = format!("{dev}/auxs/{a}");
                AuxStrip {
                    index: a,
                    name: s(tree, &format!("{base}/Name"))
                        .unwrap_or_else(|| format!("AUX {}", a + 1)),
                    is_active: b(tree, &format!("{base}/Active")).unwrap_or(true),
                    sends: bld.sends(&base),
                    inserts: bld.inserts(&base),
                    meters: bld.meters(&base, true),
                    base,
                }
            })
            .collect();

        // Outputs: classify by property and IOType, never by index.
        let mut cues: Vec<CueOutput> = Vec::new();
        let mut hps: Vec<HeadphoneOutput> = Vec::new();
        let mut monitor = None;
        for n in tree.indices(&format!("{dev}/outputs")) {
            let base = format!("{dev}/outputs/{n}");
            let io = s(tree, &format!("{base}/IOType")).unwrap_or_default();
            let name = s(tree, &format!("{base}/Name")).unwrap_or_else(|| format!("OUT {}", n + 1));
            let level_path = format!("{base}/CRMonitorLevelTapered");
            let has_level = tree.has_property(&level_path);
            if io == "Headphone" || (io.is_empty() && name.starts_with("HP")) {
                hps.push(HeadphoneOutput {
                    index: n,
                    number: hps.len() + 1,
                    name,
                    level: has_level.then_some(level_path),
                    meters: bld.meters(&base, true),
                    base,
                });
            } else if has_level && monitor.is_none() && io != "Monitor R for Surround" {
                monitor = Some(MonitorOutput {
                    device,
                    index: n,
                    name,
                    meters: bld.meters(&base, true),
                    base,
                });
            } else if io == "Cue" || (io.is_empty() && name.starts_with("CUE")) {
                let is_active =
                    b(tree, &format!("{base}/Active")).unwrap_or(cues.len() < cue_count);
                cues.push(CueOutput {
                    index: n,
                    number: cues.len() + 1,
                    name,
                    is_active,
                    meters: bld.meters(&base, true),
                    base,
                });
            }
        }

        let visible = inputs
            .iter()
            .filter(|s: &&InputStrip| s.is_visible())
            .cloned()
            .collect();
        Topology {
            device,
            device_name: s(tree, &format!("{dev}/DeviceName")),
            all_inputs: inputs,
            inputs: visible,
            auxes,
            cues,
            headphones: hps,
            monitor,
            talkback: TalkbackPaths {
                device,
                strip: talkback_strip,
            },
            cue_bus_count: cue_count,
        }
    }

    /// Every loaded plug-in slot (Unison and inserts on every input, then aux inserts).
    pub fn loaded_slots(&self, tree: &Tree) -> Vec<InsertPaths> {
        let mut slots = Vec::new();
        for strip in &self.all_inputs {
            if let Some(p) = &strip.preamp {
                slots.push(p.unison.clone());
            }
            slots.extend(strip.inserts.iter().cloned());
        }
        for a in &self.auxes {
            slots.extend(a.inserts.iter().cloned());
        }
        slots.retain(|slot| {
            tree.value(&slot.effect_name())
                .and_then(Value::as_str)
                .is_some_and(|n| !n.is_empty())
        });
        slots
    }

    /// The strip (visible or not) whose left input is `index`, or the talkback strip.
    pub fn input(&self, index: usize) -> Option<&InputStrip> {
        self.all_inputs
            .iter()
            .find(|s| s.index == index)
            .or_else(|| self.talkback.strip.as_ref().filter(|s| s.index == index))
    }
}

/// Reads the tree for path construction.
struct Builder<'a> {
    tree: &'a Tree,
    cue_bus_count: usize,
}

impl Builder<'_> {
    fn input(&self, device: usize, index: usize, base: String, right: Option<usize>) -> InputStrip {
        let tree = self.tree;
        let io = s(tree, &format!("{base}/IOType"));
        let name = s(tree, &format!("{base}/Name")).unwrap_or_else(|| format!("IN {}", index + 1));
        let stereo_name = s(tree, &format!("{base}/StereoName"));
        let io_meta = tree.meta(&format!("{base}/IOType"));
        let preamp_base = format!("{base}/preamps/0");
        let preamp = tree.has_node(&preamp_base).then(|| PreampPaths {
            has_hi_z: tree.has_property(&format!("{preamp_base}/HiZ")),
            phantom_enabled: tree
                .meta(&format!("{preamp_base}/48V"))
                .and_then(|m| m.enabled)
                .unwrap_or(true),
            pad_enabled: tree
                .meta(&format!("{preamp_base}/Pad"))
                .and_then(|m| m.enabled)
                .unwrap_or(true),
            unison: InsertPaths {
                index: 0,
                base: format!("{preamp_base}/effects/0"),
            },
            base: preamp_base,
        });
        let choices: Vec<String> = io_meta
            .map(|m| {
                m.choices()
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let choice_count = io_meta.map(|m| m.choices().len()).unwrap_or(0);
        let preamp_meter = format!("{base}/CustomIntegratedPreampInputMeter");
        InputStrip {
            device,
            index,
            right_index: right,
            name: if right.is_some() {
                stereo_name.unwrap_or(name)
            } else {
                name
            },
            default_name: tree
                .meta(&format!("{base}/Name"))
                .and_then(|m| m.default.as_ref())
                .and_then(Value::as_str)
                .map(str::to_owned),
            kind: InputKind::classify(io.as_deref(), preamp.is_some()),
            io_type: io,
            io_type_choices: choices,
            io_type_selectable: io_meta.and_then(|m| m.enabled).unwrap_or(choice_count > 1),
            stereo_linkable: tree
                .meta(&format!("{base}/Stereo"))
                .and_then(|m| m.enabled)
                .unwrap_or(true),
            is_active: b(tree, &format!("{base}/Active")).unwrap_or(true),
            is_hidden: b(tree, &format!("{base}/ChannelHidden")).unwrap_or(false),
            enabled_by_user: b(tree, &format!("{base}/EnabledByUser")).unwrap_or(true),
            preamp,
            sends: self.sends(&base),
            inserts: self.inserts(&base),
            meters: self.meters(&base, right.is_some()),
            preamp_meter: tree.has_property(&preamp_meter).then_some(preamp_meter),
            base,
        }
    }

    fn sends(&self, base: &str) -> Vec<SendPaths> {
        let tree = self.tree;
        tree.indices(&format!("{base}/sends"))
            .into_iter()
            .map(|i| {
                let sb = format!("{base}/sends/{i}");
                let id = s(tree, &format!("{sb}/ID")).unwrap_or_else(|| format!("send{i}"));
                let is_available =
                    match id.strip_prefix("cue").and_then(|k| k.parse::<usize>().ok()) {
                        Some(k) => k <= self.cue_bus_count,
                        None => true,
                    };
                let meters = tree
                    .children(&format!("{sb}/meters"))
                    .iter()
                    .map(|k| MeterPaths::new(format!("{sb}/meters/{k}")))
                    .collect();
                SendPaths {
                    index: i,
                    name: s(tree, &format!("{sb}/Name")).unwrap_or_else(|| id.to_uppercase()),
                    id,
                    base: sb,
                    is_available,
                    meters,
                }
            })
            .collect()
    }

    fn inserts(&self, base: &str) -> Vec<InsertPaths> {
        self.tree
            .indices(&format!("{base}/effects"))
            .into_iter()
            .map(|k| InsertPaths {
                index: k,
                base: format!("{base}/effects/{k}"),
            })
            .collect()
    }

    fn meters(&self, base: &str, stereo: bool) -> Vec<MeterPaths> {
        let all = self.tree.children(&format!("{base}/meters"));
        all.iter()
            .take(if stereo { 2 } else { 1 })
            .map(|k| MeterPaths::new(format!("{base}/meters/{k}")))
            .collect()
    }
}
