//! Engine data for rack's UI surfaces (`rack::surfaces`): input sends, isolate and input
//! source, the plug-in catalogue and scenes, the status bar, settings, cue / headphone outputs
//! and the monitor extras; plus the writes their controls make. Paths follow
//! `console-port/protocol/ui-to-path.md`.

use rack::model::{CueOutput, EngineSettings, HeadphoneOutput, MonitorOptions, PluginBadge, PluginEntry, SendState, StatusInfo, StripInfo};
use rack::{Action, Control, ControlId};
use ua_engine::{InputStrip, Json, SendPaths, Status, Value};

use crate::EngineMixer;

/// IOType value → the label Console draws (`MIC`, `LINE`, `HI-Z`, …).
pub fn source_label(io: &str) -> String {
    match io.to_lowercase().as_str() {
        "line" => "LINE".into(),
        "mic" => "MIC".into(),
        "hiz" | "hi-z" | "instrument" => "HI-Z".into(),
        other => other.to_uppercase(),
    }
}

/// Headphone `MixInSource` value → label (`default` follows the monitor mix).
fn hp_label(v: &str) -> String {
    match v {
        "default" => "MON".into(),
        "none" => "NONE".into(),
        cue if cue.starts_with("cue") => format!("CUE {}", &cue[3..]),
        other => other.to_uppercase(),
    }
}

fn categories(v: &Value) -> Vec<String> {
    if let Some(s) = v.as_str() {
        return s.split([',', ';', '|']).map(str::trim).filter(|c| !c.is_empty()).map(str::to_owned).collect();
    }
    if let Some(Json::Array(list)) = v.as_json() {
        return list.iter().filter_map(|c| c.as_str()).map(str::to_owned).collect();
    }
    Vec::new()
}

impl EngineMixer {
    /// Choices of an enum property, as engine values.
    fn choices(&self, path: &str) -> Vec<Value> {
        self.store.meta(path).map(|m| m.choices()).unwrap_or_default()
    }

    /// The input's sends Console shows: AUX 1/2, and the cues within `CueBusCount` that are active.
    pub(crate) fn input_send_paths(&self, s: &InputStrip) -> Vec<SendPaths> {
        let active: Vec<String> = self.store.topology().cues.iter().filter(|c| c.is_active).map(|c| c.bus()).collect();
        s.sends.iter().filter(|p| p.is_aux() || (p.is_cue() && p.is_available && active.contains(&p.id))).cloned().collect()
    }

    pub(crate) fn surface_input_sends(&self, strip: &StripInfo) -> Vec<SendState> {
        let Some(s) = self.input(&strip.id) else { return Vec::new() };
        self.input_send_paths(s)
            .iter()
            .map(|p| SendState {
                name: if p.name.is_empty() { p.id.to_uppercase() } else { p.name.to_uppercase() },
                level: self.f64_at(&p.gain()).unwrap_or(0.0),
                muted: self.bool_at(&p.bypass()).unwrap_or(false),
            })
            .collect()
    }

    /// PRE lit: `PostFaderOverride` 0, or −1 (follow the bus) on an aux whose default is pre.
    pub(crate) fn surface_send_pre(&self, strip: &StripInfo, k: usize) -> Option<bool> {
        let s = self.input(&strip.id)?;
        let p = self.input_send_paths(s).into_iter().nth(k)?;
        let ov = self.store.value(&p.post_fader_override())?.as_i64()?;
        Some(match ov {
            0 => true,
            1 => false,
            _ => {
                let bus = self.store.topology().auxes.iter().find(|a| p.is_aux() && p.bus_number() == Some(a.index as u32 + 1));
                bus.map(|a| !self.bool_at(&a.send_post_fader()).unwrap_or(true)).unwrap_or(false)
            }
        })
    }

    pub(crate) fn surface_input_sources(&self, strip: &StripInfo) -> Vec<String> {
        let Some(s) = self.input(&strip.id) else { return Vec::new() };
        if s.preamp.is_none() || !s.io_type_selectable {
            return Vec::new();
        }
        s.io_type_choices.iter().map(|c| source_label(c)).collect()
    }

    /// `/plugins/P/{Name, Categories, Authorized, Demoing, …}` in engine order (absent from
    /// trimmed snapshots). Licensed or demoing plug-ins are active; the rest are listed as
    /// "Universal Audio (Inactive)" with a badge (`Rack_PluginsBrowserManager::_GetRowData`).
    pub(crate) fn surface_catalog(&self) -> Vec<PluginEntry> {
        let mut order = self.store.tree().indices("/plugins");
        order.sort_unstable();
        order
            .into_iter()
            .filter_map(|p| {
                let base = format!("/plugins/{p}");
                let flag = |k: &str| self.bool_at(&format!("{base}/{k}"));
                let name = self.str_at(&format!("{base}/Name"))?.to_owned();
                let categories = self.store.value(&format!("{base}/Categories")).map(categories).unwrap_or_default();
                let enabled = flag("Authorized").unwrap_or(false) || flag("Demoing").unwrap_or(false);
                let badge = (!enabled).then(|| {
                    if flag("AuthUpdateRequired").unwrap_or(false) {
                        PluginBadge::Activate
                    } else if !flag("AuthHardware").unwrap_or(true) {
                        PluginBadge::Disabled
                    } else if flag("DemoAvailable").unwrap_or(false) {
                        PluginBadge::Demo
                    } else {
                        PluginBadge::Buy
                    }
                });
                let category = categories.first().cloned().unwrap_or_default();
                Some(PluginEntry { name, category, categories, enabled, favorite: false, badge })
            })
            .collect()
    }

    pub(crate) fn surface_scenes(&self) -> Vec<String> {
        self.store
            .tree()
            .indices("/pluginscenes/0/scenes")
            .into_iter()
            .map(|n| self.str_at(&format!("/pluginscenes/0/scenes/{n}/SceneName")).unwrap_or("").to_owned())
            .collect()
    }

    pub(crate) fn surface_status(&self) -> StatusInfo {
        let load = |p: &str| self.f64_at(p).unwrap_or(0.0) as f32;
        let scene = self
            .store
            .value("/pluginscenes/0/RecalledSceneIndex")
            .and_then(Value::as_i64)
            .filter(|i| *i >= 0)
            .and_then(|i| self.surface_scenes().get(i as usize).cloned())
            .filter(|s| !s.is_empty());
        StatusInfo {
            tempo: self.f64_at("/Tempo").unwrap_or(120.0),
            sample_rate: self.store.value("/SampleRate").and_then(Value::as_i64).map(|r| r as u32),
            clock_source: self.str_at("/ClockSource").map(str::to_owned),
            dsp_load: load("/TotalDSPLoad"),
            pgm_load: load("/TotalPGMLoad"),
            mem_load: load("/TotalMEMLoad"),
            // `MxClnt_UAD2Paths::ParsePluginScenesPath` (0x181a7c530) flags support when it parses
            // `/pluginscenes/0` (Type "pluginscenes").
            plugin_scenes: self.str_at("/pluginscenes/0/Type") == Some("pluginscenes"),
            scene,
        }
    }

    pub(crate) fn surface_settings(&self) -> EngineSettings {
        let ints = |path: &str| self.choices(path).iter().filter_map(Value::as_i64).map(|v| v as u32).collect::<Vec<_>>();
        EngineSettings {
            host: self.store.connection().map(|c| format!("{}:{}", c.endpoint().host, c.endpoint().port)).unwrap_or_default(),
            connected: matches!(self.store.status(), Status::Live | Status::Loading),
            sample_rate: self.store.value("/SampleRate").and_then(Value::as_i64).map(|r| r as u32),
            sample_rates: ints("/SampleRate"),
            clock_source: self.str_at("/ClockSource").map(str::to_owned),
            clock_sources: self.choices("/ClockSource").iter().filter_map(|v| v.as_str().map(str::to_owned)).collect(),
            buffer_size: self.store.value("/BufferSize").and_then(Value::as_i64).map(|b| b as u32),
            buffer_sizes: ints("/BufferSize"),
            writes_enabled: self.store.writer().writes_enabled,
        }
    }

    pub(crate) fn surface_monitor_options(&self) -> MonitorOptions {
        let Some(m) = &self.store.topology().monitor else { return MonitorOptions::default() };
        let atten = self.f64_at(&m.dim_attenuation()).unwrap_or(17.0);
        let dev = format!("/devices/{}", m.device);
        let alt_count = self.store.tree().indices(&format!("{dev}/outputs")).into_iter().filter(|n| self.bool_at(&format!("{dev}/outputs/{n}/AltMonEnabled")) == Some(true)).count().min(2) as u8;
        MonitorOptions {
            dim: self.bool_at(&m.dim()),
            dim_level: (atten / 60.0).clamp(0.0, 1.0),
            dim_readout: format!("-{atten:.0} dB"),
            talkback: self.bool_at(&self.store.topology().talkback.on()),
            alt_count,
            alt_selection: self.store.value(&m.alt_mon_selection()).and_then(Value::as_i64).unwrap_or(0).clamp(0, 2) as u8,
            // Rack's controller predicate is unresolved; Console 1.3.1 shows the button for an
            // Apollo x4 with `RemoteMonitorController` false.
            controller: true,
            hp_correction: (self.bool_at(&format!("{dev}/SupportsHeadphoneCorrection")) == Some(true))
                .then(|| self.bool_at(&format!("{dev}/HeadphoneCorrectionPower")).unwrap_or(true)),
            speaker_correction: (self.bool_at(&format!("{dev}/SupportsSpeakerCorrection")) == Some(true))
                .then(|| self.bool_at(&format!("{dev}/SpeakerCorrectionPower")).unwrap_or(true)),
            // The front-panel knob drives the monitor unless an input (preamp) or another
            // output is `SelectedOnFront` (p1: none → green dot; p2: inputs/0 → off).
            knob_focus: !["inputs", "outputs"].iter().any(|kind| {
                self.store.tree().indices(&format!("{dev}/{kind}")).into_iter().any(|n| {
                    (*kind != "outputs" || n != m.index) && self.bool_at(&format!("{dev}/{kind}/{n}/SelectedOnFront")) == Some(true)
                })
            }),
        }
    }

    pub(crate) fn surface_cues(&self) -> Vec<CueOutput> {
        self.store
            .topology()
            .cues
            .iter()
            .filter(|c| c.is_active)
            .map(|c| CueOutput {
                name: c.name.to_uppercase(),
                source_cue: self.str_at(&c.mix_in_source()) == Some("cue"),
                mono: self.bool_at(&c.mono()).unwrap_or(false),
                destination: self.str_at(&c.output_destination()).unwrap_or("None").to_uppercase(),
            })
            .collect()
    }

    pub(crate) fn surface_headphones(&self) -> Vec<HeadphoneOutput> {
        self.store
            .topology()
            .headphones
            .iter()
            .map(|h| {
                let path = h.mix_in_source();
                let raw: Vec<String> = self.choices(&path).iter().filter_map(|v| v.as_str().map(str::to_owned)).collect();
                let cur = self.str_at(&path).unwrap_or("default");
                HeadphoneOutput { name: h.name.to_uppercase(), source: raw.iter().position(|v| v == cur).unwrap_or(0), options: raw.iter().map(|v| hp_label(v)).collect() }
            })
            .collect()
    }

    /// `set_text` targets: channel name, plug-in load (`EffectName`), tempo.
    pub(crate) fn surface_set_text(&mut self, id: &ControlId, text: &str) {
        match id.control {
            Control::Name => {
                let path = match (self.input(&id.strip), self.aux(&id.strip)) {
                    (Some(s), _) => Some(s.name_path()),
                    (None, Some(a)) => Some(a.name_path()),
                    _ => None,
                };
                if let Some(p) = path {
                    self.store.commit(&p, text.to_owned());
                }
            }
            Control::Insert(_) => {
                if let Some(slot) = self.insert_slot(id) {
                    self.store.commit(&slot.effect_name(), text.to_owned());
                }
            }
            Control::Tempo => {
                if let Ok(t) = text.trim().parse::<f64>() {
                    self.store.commit("/Tempo", t.clamp(1.0, 999.0));
                }
            }
            _ => {}
        }
    }

    /// Writes for the surface controls. Returns false for controls handled elsewhere.
    pub(crate) fn apply_surface(&mut self, action: &Action) -> bool {
        let id = action.id();
        if let Action::Set(_, v) = action {
            return match &id.control {
                Control::SendLevel(k) => match self.input(&id.strip).and_then(|s| self.input_send_paths(s).into_iter().nth(*k as usize)) {
                    Some(p) => {
                        self.store.set(&p.gain(), *v);
                        true
                    }
                    None => false,
                },
                Control::DimLevel => {
                    if let Some(m) = self.store.topology().monitor.as_ref().map(|m| m.dim_attenuation()) {
                        self.store.set(&m, (v * 60.0).round() as i64);
                    }
                    true
                }
                _ => false,
            };
        }
        if !matches!(action, Action::Tap(_) | Action::Toggle(_)) {
            return false;
        }
        let monitor = self.store.topology().monitor.clone();
        match &id.control {
            Control::Menu(cmd) => self.apply_menu(&id.strip, cmd),
            Control::InsertPower(_) | Control::InsertRemove(_) | Control::InsertCopy(_) => {
                let k = match id.control {
                    Control::InsertPower(k) | Control::InsertRemove(k) | Control::InsertCopy(k) => k,
                    _ => unreachable!(),
                };
                let Some(slot) = self.insert_slot(&ControlId { strip: id.strip.clone(), control: Control::Insert(k) }) else { return true };
                let loaded = self.str_at(&slot.effect_name()).filter(|n| !n.is_empty()).map(str::to_owned);
                match (&id.control, loaded) {
                    (Control::InsertPower(_), Some(_)) => self.toggle(&slot.power(), true),
                    (Control::InsertRemove(_), Some(_)) => self.store.commit(&slot.effect_name(), String::new()),
                    (Control::InsertCopy(_), Some(name)) => self.clipboard = Some(name),
                    (Control::InsertCopy(_), None) => {
                        if let Some(name) = self.clipboard.clone() {
                            self.store.commit(&slot.effect_name(), name);
                        }
                    }
                    _ => {}
                }
            }
            Control::SendMute(k) | Control::SendPre(k) if self.input(&id.strip).is_some() => {
                let s = self.input(&id.strip).expect("checked");
                let Some(p) = self.input_send_paths(s).into_iter().nth(*k as usize) else { return true };
                if let Control::SendMute(_) = id.control {
                    self.toggle(&p.bypass(), false);
                } else {
                    let strip = StripInfo { id: id.strip.clone(), title: String::new(), kind: rack::geometry::StripKind::Input, stereo: false, preamp_layout: None, shows_sr_convert: false, console_model: None };
                    let pre = self.surface_send_pre(&strip, *k as usize).unwrap_or(false);
                    self.store.commit(&p.post_fader_override(), if pre { 1i64 } else { 0i64 });
                }
            }
            Control::Isolate => {
                if let Some(p) = self.input(&id.strip).map(|s| s.isolate()) {
                    self.toggle(&p, false);
                }
            }
            Control::InputSource(i) => {
                if let Some(s) = self.input(&id.strip) {
                    if let Some(v) = s.io_type_choices.get(*i as usize).cloned() {
                        let p = s.io_type_path();
                        self.store.commit(&p, v);
                    }
                }
            }
            Control::AllRecordPreEffects(pre) => {
                let paths: Vec<String> = self.store.topology().inputs.iter().map(InputStrip::record_pre_effects).collect();
                for p in paths {
                    if self.bool_at(&p) != Some(*pre) {
                        self.store.commit(&p, *pre);
                    }
                }
            }
            Control::ClearSolos => {
                let paths: Vec<String> = self.store.topology().inputs.iter().map(InputStrip::solo).collect();
                for p in paths {
                    if self.bool_at(&p) == Some(true) {
                        self.store.commit(&p, false);
                    }
                }
            }
            Control::SampleRate(i) | Control::BufferSize(i) => {
                let path = if matches!(id.control, Control::SampleRate(_)) { "/SampleRate" } else { "/BufferSize" };
                if let Some(v) = self.choices(path).get(*i as usize).cloned() {
                    self.store.commit(path, v);
                }
            }
            Control::ClockSource(i) => {
                if let Some(v) = self.choices("/ClockSource").get(*i as usize).cloned() {
                    self.store.commit("/ClockSource", v);
                }
            }
            Control::RecallScene(i) => self.store.commit("/pluginscenes/0/RecalledSceneIndex", *i as i64),
            Control::MonitorDim => {
                if let Some(m) = monitor {
                    self.toggle(&m.dim(), false);
                }
            }
            Control::MonitorAlt(k) => {
                if let Some(m) = monitor {
                    self.store.commit(&m.alt_mon_selection(), *k as i64);
                }
            }
            Control::Talkback => {
                let p = self.store.topology().talkback.on();
                self.toggle(&p, false);
            }
            Control::CueSource(k) | Control::CueMono(k) => {
                let cue = self.store.topology().cues.iter().filter(|c| c.is_active).nth(*k as usize).cloned();
                if let Some(c) = cue {
                    if let Control::CueMono(_) = id.control {
                        self.toggle(&c.mono(), false);
                    } else {
                        let next = if self.str_at(&c.mix_in_source()) == Some("cue") { "mon" } else { "cue" };
                        self.store.commit(&c.mix_in_source(), next);
                    }
                }
            }
            Control::HeadphoneSource { output, option } => {
                let hp = self.store.topology().headphones.get(*output as usize).cloned();
                if let Some(h) = hp {
                    if let Some(v) = self.choices(&h.mix_in_source()).get(*option as usize).cloned() {
                        self.store.commit(&h.mix_in_source(), v);
                    }
                }
            }
            Control::WritesEnabled => {
                let on = self.store.writer().writes_enabled;
                self.store.set_writes_enabled(!on);
            }
            // No engine path known (clear clips is a helper command; set-default is a preset op).
            Control::ClearClips | Control::InsertSetDefault(_) => {}
            _ => return false,
        }
        true
    }
}
