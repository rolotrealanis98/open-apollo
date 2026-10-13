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
        let topology = self.store.topology_for_device(s.device).unwrap_or(self.store.topology());
        let active: Vec<String> = topology.cues.iter().filter(|c| c.is_active).map(|c| c.bus()).collect();
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
                pan: ua_engine::tapers::pan::knob_from_pan(self.f64_at(&p.pan()).unwrap_or(0.0)),
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
                let topology = self.store.topology_for_device(s.device).unwrap_or(self.store.topology());
                let bus = topology.auxes.iter().find(|a| p.is_aux() && p.bus_number() == Some(a.index as u32 + 1));
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
            clock_locked: self.bool_at("/ClockLocked"),
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
            talkback_to_monitor: self.bool_at(&self.store.topology().talkback.in_physical_cr()),
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
            // `MixInSource` "mon" | "cueN".
            cr_source: self
                .store
                .value(&format!("{dev}/outputs/{}/MixInSource", m.index))
                .and_then(Value::as_str)
                .and_then(|v| v.strip_prefix("cue"))
                .and_then(|n| n.parse().ok())
                .unwrap_or(0),
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
                    self.assign_insert(&slot, text);
                }
            }
            // `MxClnt_PluginScenesData::SetSceneName` (0x1819047b0): upper-cased, then set.
            Control::SceneName(i) => {
                let name = text.trim().to_uppercase();
                if !name.is_empty() {
                    self.store.commit(&format!("/pluginscenes/0/scenes/{i}/SceneName"), name);
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
                // A mono input's send pan (full send strips): knob 0…1 → `sends/k/Pan` −1…1.
                Control::SendPan(k) => match self.input(&id.strip).and_then(|s| self.input_send_paths(s).into_iter().nth(*k as usize)) {
                    Some(p) => {
                        self.store.set(&p.pan(), ua_engine::tapers::pan::pan_from_knob(*v));
                        true
                    }
                    None => false,
                },
                // TALK released after a long press (`HitKind::Latch`): momentary, off again.
                Control::Talkback => {
                    let p = self.store.topology().talkback.on();
                    if self.bool_at(&p) != Some(*v >= 0.5) {
                        self.store.set_now(&p, *v >= 0.5);
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
                    // The COPY modifier: a loaded slot copies, an empty one pastes.
                    (Control::InsertCopy(_), Some(name)) => self.copy_insert(&slot, name),
                    (Control::InsertCopy(_), None) => self.paste_insert(&slot, k),
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
                self.store.post("/ClearSolos", Some(&ua_engine::Json::Object(Default::default())));
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
            // Console recalls through `SceneOperation` (0x181b0ffd0), not RecalledSceneIndex.
            Control::RecallScene(i) => self.scene_operation(rack::menu::SceneOp::Recall, *i as u16),
            Control::SceneCommand(op, i) => self.scene_operation(*op, *i),
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
            // TALK (`Rack_Talkback` mouse down 0x1814975b0): the opposite of the current value
            // through `UAClnt_Values<bool>::Set`, with no /UndoRecording bracket (Rack.dll RE;
            // not trace-verified). The engine's echo or push is what the button then shows.
            Control::Talkback => {
                let p = self.store.topology().talkback.on();
                let on = !self.bool_at(&p).unwrap_or(false);
                self.store.set_now(&p, on);
            }
            // `Rack_Talkback` talkback_to_mons (0x181497377) → `/TalkbackInPhysicalCR` (registered
            // by MxClnt_UAD2MixerData 0x181b187ca; root on engine 12.0.0).
            // View > Show/Hide Channels row (strip = the input's path): `ChannelHidden`.
            Control::ChannelVisible => {
                let p = format!("{}/ChannelHidden", id.strip);
                if self.store.tree().has_property(&p) {
                    self.toggle(&p, false);
                }
            }
            // TALKBACK TO MONITOR (0x181489be0): toggles on press, unbracketed like TALK.
            Control::TalkbackToMonitor => {
                let p = self.store.topology().talkback.in_physical_cr();
                let on = !self.bool_at(&p).unwrap_or(false);
                self.store.set_now(&p, on);
            }
            // Control-room SOURCE (`Rack_ControlRoom` cue_N/mon_source, 0x181b1fa7b):
            // `outputs/MON/MixInSource` "mon" | "cueN" (values on the x4: mon, cue1…cue4).
            Control::CrSource(k) => {
                if let Some(m) = monitor {
                    let path = format!("/devices/{}/outputs/{}/MixInSource", m.device, m.index);
                    let want = if *k == 0 { "mon".to_owned() } else { format!("cue{k}") };
                    let allowed = self.choices(&path);
                    if allowed.is_empty() || allowed.iter().any(|v| v.as_str() == Some(want.as_str())) {
                        self.store.commit(&path, want);
                    }
                }
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
            // Settings' WRITES switch opens both gates: launched without `--writes` the
            // connection refuses set / post too, so the writer alone would send nothing.
            Control::WritesEnabled => {
                let on = self.store.writer().writes_enabled;
                self.store.set_engine_writes(!on);
            }
            // No engine path known (clear clips is a helper command; set-default is a preset op).
            Control::ClearClips => self.store.post("/ClearClips", Some(&ua_engine::Json::Object(Default::default()))),
            Control::InsertSetDefault(_) => {}
            Control::InsertMove { .. } | Control::PresetDrop(_) => {
                self.apply_drop(id);
            }
            _ => return false,
        }
        true
    }
}
