//! What the mixer window shows, independent of where the data comes from.

use std::collections::HashMap;
use std::sync::Arc;

use crate::geometry::StripKind;
use crate::input::{Action, Control};
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
}

/// Reproduces console-main-1.png offline; actions edit local state.
pub struct PreviewMixer {
    pub inputs: Vec<StripInfo>,
    pub auxes: Vec<StripInfo>,
    pub mains: HashMap<Arc<str>, MainState>,
    pub preamps: HashMap<Arc<str>, PreampState>,
    pub monitor: MonitorState,
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
        Self { inputs, auxes, mains, preamps, monitor: MonitorState { level: 0.09, readout: "-52.0 dB".into(), mono: false, mute: false } }
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
    fn inserts(&self, _: &StripInfo) -> Vec<InsertState> {
        vec![InsertState::Empty; 4]
    }
    fn unison(&self, _: &StripInfo) -> InsertState {
        InsertState::Empty
    }
    fn sends(&self, s: &StripInfo) -> Vec<SendState> {
        if s.kind == StripKind::Aux {
            vec![SendState { name: "CUE 1".into(), level: 0.5, muted: false }, SendState { name: "CUE 2".into(), level: 0.5, muted: false }]
        } else {
            vec![]
        }
    }
    fn records_pre_effects(&self, s: &StripInfo) -> bool {
        s.preamp_layout.is_none()
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
    fn apply(&mut self, action: &Action) {
        match action {
            Action::Set(id, v) => {
                if id.control == Control::MonitorLevel {
                    self.monitor.level = *v;
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
                        }
                    }
                    _ => {}
                }
            }
            Action::Toggle(id) => match &id.control {
                Control::MonitorMono => self.monitor.mono ^= true,
                Control::MonitorMute => self.monitor.mute ^= true,
                c => {
                    if let Some(m) = self.mains.get_mut(&id.strip) {
                        match c {
                            Control::Solo => m.solo ^= true,
                            Control::Mute => m.mute ^= true,
                            Control::PrePost => m.pre ^= true,
                            Control::Mono => m.mono ^= true,
                            _ => {}
                        }
                    }
                    if let (Control::Preamp(c), Some(p)) = (c, self.preamps.get_mut(&id.strip)) {
                        match &**c {
                            "48v" if p.phantom_enabled => p.phantom ^= true,
                            "pad" if p.pad_enabled => p.pad ^= true,
                            "low_cut" => p.low_cut ^= true,
                            "phase" => p.phase ^= true,
                            _ => {}
                        }
                    }
                }
            },
            _ => {}
        }
    }
}
