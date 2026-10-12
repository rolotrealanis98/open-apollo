//! # engine-model
//!
//! [`EngineMixer`] implements [`rack::MixerModel`] over the UA Mixer Engine: a
//! [`ua_engine::Store`] (live, or a snapshot offline), its [`Topology`](ua_engine::Topology), the
//! lock-free [`MeterStore`](ua_engine::MeterStore) and the gated writer. A port of ConsoleLab's
//! Swift `ConsoleScreens/EngineMixer.swift`.
//!
//! ```no_run
//! use engine_model::EngineMixer;
//! use rack::MixerModel;
//!
//! let mut mixer = EngineMixer::connect(ua_engine::Endpoint::parse("127.0.0.1:4710"), false); // read-only
//! loop {
//!     let changed = mixer.pump();          // once per frame: applies queued engine updates
//!     let strips = mixer.inputs();         // rack builds the window from the model
//!     # let _ = (changed, strips);
//!     # break;
//! }
//! ```
//!
//! Strip ids are the engine node paths (`/devices/0/inputs/0`, `/devices/0/auxs/1`).
//! [`rack::Action`]s map to writes: `GestureBegin`/`GestureEnd` → `begin_gesture`/`end_gesture`
//! (one `/UndoRecording` step), `Set` → coalesced `set`, `Toggle` → `commit` of the negated
//! value. `Tap`s (insert slots) are queued for the app ([`EngineMixer::take_taps`];
//! [`EngineMixer::insert_slot`] resolves the slot, whose plug-in parameters are at
//! [`InsertPaths::parameter`](ua_engine::InsertPaths::parameter)).

mod settings;
mod sessions;
mod menus;
mod slots;
mod surfaces;

pub use slots::{SlotInfo, SlotSource};

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use rack::geometry::StripKind;
use rack::model::{
    CueOutput, EngineSettings, HeadphoneOutput, InsertLabel, InsertState, MainState, MeterFrame,
    MixerModel, MonitorOptions, MonitorState, PluginEntry, PreampState, SendState, StatusInfo,
    StripInfo,
};
use rack::{Action, Control, ControlId, scales};
use ua_engine::{
    AuxStrip, Endpoint, InputKind, InputStrip, InsertPaths, MeterPaths, MeterReading, Options,
    SendPaths, Snapshot, Store, Value, tapers,
};

/// The preamp layout name in the skin (`preamp_channel_strip_controls.json`).
pub const PREAMP_LAYOUT: &str = "preamp_channel_strip_controls";
/// Insert slots Console's mixer shows per strip.
pub const INSERT_SLOTS: usize = 4;
/// `Control::Insert` index rack uses for the Unison slot.
pub const UNISON_SLOT: u8 = 255;

/// `rack::MixerModel` backed by the engine.
pub struct EngineMixer {
    store: Store,
    live_meters: bool,
    taps: Vec<ControlId>,
    /// Plug-in name copied with the COPY modifier / insert menu, pasted into an empty slot.
    clipboard: Option<String>,
    /// The read-only UA Mixer Helper (4720): native inserts and helper state.
    helper: Option<ua_engine::HelperStore>,
    /// Console's client-side preferences (Settings › OPTIONS / MIDI), kept in memory.
    prefs: rack::settings::Prefs,
}

impl EngineMixer {
    /// A mixer over an existing store.
    pub fn new(store: Store) -> EngineMixer {
        EngineMixer {
            store,
            live_meters: true,
            taps: Vec::new(),
            clipboard: None,
            helper: None,
            prefs: rack::settings::Prefs::default(),
        }
    }

    /// Connects to an engine. With `writes` false the connection is read-only (it refuses
    /// `set`/`post`) and the writer gate stays closed: controls move locally only. With `writes`
    /// true both gates open.
    pub fn connect(endpoint: Endpoint, writes: bool) -> EngineMixer {
        EngineMixer::connect_with(
            endpoint,
            Options {
                allow_writes: writes,
                ..Options::default()
            },
            writes,
        )
    }

    /// Like [`EngineMixer::connect`] with explicit connection options; `writes` opens the writer
    /// gate (it has no effect unless `options.allow_writes` is set too).
    pub fn connect_with(endpoint: Endpoint, options: Options, writes: bool) -> EngineMixer {
        let helper = EngineMixer::helper_endpoint(&endpoint);
        let mut store = Store::new();
        store.connect(endpoint, options);
        store.set_writes_enabled(writes);
        let mut mixer = EngineMixer::new(store);
        // The UA Mixer Helper on host:4720, read-only (native inserts, helper state).
        if let Some(h) = helper {
            mixer.connect_helper(h);
        }
        mixer
    }

    /// Offline, from a snapshot file (ConsoleLab snapshot, raw engine reply or open-apollo device
    /// map). Writes move local values only.
    pub fn from_snapshot(path: impl AsRef<Path>) -> ua_engine::Result<EngineMixer> {
        Ok(EngineMixer::new(Store::from_snapshot(Snapshot::load(
            path,
        )?)))
    }

    /// Applies queued engine updates and timed work (writer flush, echo holds). Call once per
    /// frame. Returns true when values, topology or status changed (meters are separate: see
    /// [`EngineMixer::meter_generation`]).
    pub fn pump(&mut self) -> bool {
        let engine = self.store.drain();
        let helper = self.helper.as_mut().is_some_and(|h| h.drain());
        engine || helper
    }

    /// Alias of [`EngineMixer::pump`].
    pub fn drain(&mut self) -> bool {
        self.pump()
    }

    /// When [`EngineMixer::pump`] next has timed work (to schedule a wake-up).
    pub fn next_deadline(&self) -> Option<Instant> {
        let helper = self.helper.as_ref().and_then(|h| h.next_deadline());
        self.store.next_deadline().into_iter().chain(helper).min()
    }

    /// Moves on every meter push: redraw meters when it changes.
    pub fn meter_generation(&self) -> u64 {
        self.store.meters().generation()
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn store_mut(&mut self) -> &mut Store {
        &mut self.store
    }

    /// Off: meters read as silent (for deterministic one-shot renders).
    pub fn set_live_meters(&mut self, on: bool) {
        self.live_meters = on;
    }

    /// Taps since the last call (insert and Unison slots), for opening plug-in windows.
    pub fn take_taps(&mut self) -> Vec<ControlId> {
        std::mem::take(&mut self.taps)
    }

    /// The slot an `Insert(k)` control names (`Insert(255)` is the Unison slot).
    pub fn insert_slot(&self, id: &ControlId) -> Option<InsertPaths> {
        let Control::Insert(k) = id.control else {
            return None;
        };
        if k == UNISON_SLOT {
            return self
                .input(&id.strip)?
                .preamp
                .as_ref()
                .map(|p| p.unison.clone());
        }
        let inserts = match self.input(&id.strip) {
            Some(s) => &s.inserts,
            None => &self.aux(&id.strip)?.inserts,
        };
        inserts.get(k as usize).cloned()
    }

    // MARK: lookups

    fn input(&self, id: &str) -> Option<&InputStrip> {
        self.store.topology().inputs.iter().find(|s| s.base == id)
    }

    fn aux(&self, id: &str) -> Option<&AuxStrip> {
        self.store.topology().auxes.iter().find(|a| a.base == id)
    }

    fn f64_at(&self, path: &str) -> Option<f64> {
        self.store.value(path).and_then(Value::as_f64)
    }

    fn bool_at(&self, path: &str) -> Option<bool> {
        self.store.value(path).and_then(Value::as_bool)
    }

    fn str_at(&self, path: &str) -> Option<&str> {
        self.store.value(path).and_then(Value::as_str)
    }

    /// The aux's cue sends Console shows: available (within `CueBusCount`) and on an active cue.
    fn aux_sends(&self, a: &AuxStrip) -> Vec<SendPaths> {
        let active: Vec<String> = self
            .store
            .topology()
            .cues
            .iter()
            .filter(|c| c.is_active)
            .map(|c| c.bus())
            .collect();
        a.sends
            .iter()
            .filter(|s| s.is_cue() && s.is_available && active.contains(&s.id))
            .cloned()
            .collect()
    }

    fn io_type(&self, s: &InputStrip) -> String {
        self.str_at(&s.io_type_path()).unwrap_or("Mic").to_owned()
    }

    /// 48V and pad are offered only in Mic mode, and only when the engine enables them.
    fn preamp_enabled(&self, s: &InputStrip, control: &str) -> bool {
        let Some(p) = &s.preamp else { return false };
        match control {
            "48v" => self.io_type(s) == "Mic" && p.phantom_enabled,
            "pad" => self.io_type(s) == "Mic" && p.pad_enabled,
            _ => true,
        }
    }

    fn preamp_path(s: &InputStrip, control: &str) -> Option<String> {
        let p = s.preamp.as_ref()?;
        Some(match control {
            "preamp_gain" => p.gain(),
            "48v" => p.phantom(),
            "pad" => p.pad(),
            "low_cut" => p.low_cut(),
            "phase" => p.phase(),
            _ => return None,
        })
    }

    fn frame(&self, meters: &[MeterPaths]) -> MeterFrame {
        if !self.live_meters || meters.is_empty() {
            return MeterFrame::default();
        }
        let keys: Vec<&str> = meters.iter().map(|m| m.base.as_str()).collect();
        let r = self.store.meters().readings(&keys);
        let l = r.first().copied().unwrap_or(MeterReading::SILENT);
        let rr = r.get(1).copied().unwrap_or(l);
        MeterFrame {
            left: scales::meter_fraction(l.level_db as f64),
            right: scales::meter_fraction(rr.level_db as f64),
            clip_left: l.clip,
            clip_right: rr.clip,
        }
    }

    fn slot_state(&self, slot: &InsertPaths) -> InsertState {
        match self.str_at(&slot.effect_name()) {
            Some(name) if !name.is_empty() => InsertState::Loaded {
                name: name.to_owned(),
                power: self.bool_at(&slot.power()) != Some(false),
            },
            _ => InsertState::Empty,
        }
    }

    /// Commits the negation of a bool property (`default` when it has no value).
    fn toggle(&mut self, path: &str, default: bool) {
        let v = !self.bool_at(path).unwrap_or(default);
        self.store.commit(path, v);
    }
}

/// Console's fader readout ([`tapers::format_db`]): `-∞` at the bottom, the sign of the
/// unrounded value otherwise ("-0.0" just below unity, "+0.0" just above, "0.0" at it).
pub fn fader_readout(db: f64) -> String {
    tapers::format_db(db)
}

fn strip_id(base: &str) -> Arc<str> {
    Arc::from(base)
}

impl MixerModel for EngineMixer {
    fn pump(&mut self) -> bool {
        EngineMixer::pump(self)
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn session_name(&self) -> String {
        // Console's name for the `/Session` value (`- New Session -` when never saved).
        let name = match self.str_at("/Session") {
            Some(v) => ua_engine::session::display_name(v),
            None => "Session".to_owned(),
        };
        let dirty = self.bool_at("/Dirty").unwrap_or(false);
        format!("{name}{}", if dirty { "*" } else { "" })
    }

    fn session(&self) -> rack::SessionInfo {
        self.session_info()
    }

    fn inputs(&self) -> Vec<StripInfo> {
        self.store
            .topology()
            .inputs
            .iter()
            .map(|s| StripInfo {
                id: strip_id(&s.base),
                title: s.default_name.as_deref().unwrap_or(&s.name).to_uppercase(),
                kind: StripKind::Input,
                stereo: s.is_stereo(),
                preamp_layout: s.preamp.as_ref().map(|_| PREAMP_LAYOUT.to_owned()),
                shows_sr_convert: s.kind == InputKind::Spdif,
                console_model: self.input_console_model(s),
            })
            .collect()
    }

    fn auxes(&self) -> Vec<StripInfo> {
        self.store
            .topology()
            .auxes
            .iter()
            .filter(|a| a.is_active)
            .map(|a| StripInfo {
                id: strip_id(&a.base),
                title: a.name.to_uppercase(),
                kind: StripKind::Aux,
                stereo: true,
                preamp_layout: None,
                shows_sr_convert: false,
                console_model: self.aux_console_model(a),
            })
            .collect()
    }

    fn main(&self, strip: &StripInfo) -> MainState {
        if let Some(s) = self.input(&strip.id) {
            let fader = self.f64_at(&s.fader()).unwrap_or(0.0);
            let db = self
                .f64_at(&s.fader_db())
                .unwrap_or_else(|| tapers::fader::db_from_tapered(fader));
            return MainState {
                name: self
                    .str_at(&s.display_name_path())
                    .unwrap_or(&s.name)
                    .to_owned(),
                fader,
                pan_l: tapers::pan::knob_from_pan(self.f64_at(&s.pan()).unwrap_or(0.0)),
                pan_r: tapers::pan::knob_from_pan(self.f64_at(&s.pan2()).unwrap_or(0.0)),
                solo: self.bool_at(&s.solo()).unwrap_or(false),
                mute: self.bool_at(&s.mute()).unwrap_or(false),
                pre: false,
                mono: false,
                readout: fader_readout(db),
            };
        }
        if let Some(a) = self.aux(&strip.id) {
            let fader = self.f64_at(&a.fader()).unwrap_or(0.0);
            let db = self
                .f64_at(&a.fader_db())
                .unwrap_or_else(|| tapers::fader::db_from_tapered(fader));
            return MainState {
                name: self.str_at(&a.name_path()).unwrap_or(&a.name).to_owned(),
                fader,
                mute: self.bool_at(&a.mute()).unwrap_or(false),
                pre: !self.bool_at(&a.send_post_fader()).unwrap_or(true),
                mono: self.bool_at(&a.mono()).unwrap_or(false),
                readout: fader_readout(db),
                ..MainState::default()
            };
        }
        MainState {
            name: strip.title.clone(),
            ..MainState::default()
        }
    }

    fn preamp(&self, strip: &StripInfo) -> Option<PreampState> {
        let s = self.input(&strip.id)?;
        let p = s.preamp.as_ref()?;
        let io = self.io_type(s);
        let gain_text = match self.str_at(&p.custom_display_text()) {
            Some(custom) if custom.chars().any(|c| c.is_ascii_digit()) => custom.to_owned(),
            _ => format!(
                "{} dB",
                self.f64_at(&p.gain_db()).unwrap_or(10.0).round() as i64
            ),
        };
        let source = match surfaces::source_label(&io).as_str() {
            "LINE" => "LINE",
            "HI-Z" => "HI-Z",
            _ => "MIC",
        };
        let input_led = match &s.preamp_meter {
            Some(key) if self.live_meters => {
                let db = self.store.meters().reading(key).level_db;
                if db > -1.0 {
                    1.0
                } else if db > -12.0 {
                    0.67
                } else if db > -40.0 {
                    0.34
                } else {
                    0.0
                }
            }
            _ => 0.0,
        };
        Some(PreampState {
            gain: self.f64_at(&p.gain()).unwrap_or(0.0),
            gain_text,
            source: source.to_owned(),
            phantom: self.bool_at(&p.phantom()).unwrap_or(false),
            pad: self.bool_at(&p.pad()).unwrap_or(false),
            low_cut: self.bool_at(&p.low_cut()).unwrap_or(false),
            phase: self.bool_at(&p.phase()).unwrap_or(false),
            phantom_enabled: self.preamp_enabled(s, "48v"),
            pad_enabled: self.preamp_enabled(s, "pad"),
            input_led,
        })
    }

    fn inserts(&self, strip: &StripInfo) -> Vec<InsertState> {
        // UAD-2 (4710) and native (helper, 4720) merged; see `EngineMixer::slots` for labels and
        // the native flag.
        self.slots(strip).into_iter().map(|s| s.state).collect()
    }

    fn insert_labels(&self, strip: &StripInfo) -> Vec<Option<InsertLabel>> {
        self.slots(strip)
            .into_iter()
            .map(SlotInfo::insert_label)
            .collect()
    }

    fn unison_label(&self, strip: &StripInfo) -> Option<InsertLabel> {
        self.unison_slot(strip).insert_label()
    }

    fn unison(&self, strip: &StripInfo) -> InsertState {
        match self.input(&strip.id).and_then(|s| s.preamp.as_ref()) {
            Some(p) => self.slot_state(&p.unison),
            None => InsertState::Empty,
        }
    }

    fn sends(&self, strip: &StripInfo) -> Vec<SendState> {
        let Some(a) = self.aux(&strip.id) else {
            return Vec::new();
        };
        self.aux_sends(a)
            .iter()
            .map(|s| SendState {
                name: s.name.to_uppercase(),
                level: self.f64_at(&s.gain()).unwrap_or(0.0),
                muted: self.bool_at(&s.bypass()).unwrap_or(false),
            })
            .collect()
    }

    /// Console reserves a row for every cue send the engine lists on an aux (p2: CUE 1–4), shown or not.
    fn send_rows(&self, strip: &StripInfo) -> usize {
        match self.aux(&strip.id) {
            Some(a) => a.sends.iter().filter(|s| s.is_cue()).count(),
            None => self.input_sends(strip).len(),
        }
    }

    fn records_pre_effects(&self, strip: &StripInfo) -> bool {
        self.input(&strip.id)
            .and_then(|s| self.bool_at(&s.record_pre_effects()))
            .unwrap_or(false)
    }

    fn meters(&self, strip: &StripInfo) -> MeterFrame {
        match self.input(&strip.id) {
            Some(s) => self.frame(&s.meters),
            None => self
                .aux(&strip.id)
                .map(|a| self.frame(&a.meters))
                .unwrap_or_default(),
        }
    }

    fn monitor(&self) -> MonitorState {
        let Some(m) = &self.store.topology().monitor else {
            return MonitorState {
                level: 0.0,
                readout: String::new(),
                mono: false,
                mute: false,
            };
        };
        let db = self
            .f64_at(&m.level_db())
            .unwrap_or(tapers::monitor::MIN_DB);
        MonitorState {
            level: self.f64_at(&m.level()).unwrap_or(0.0),
            // Console prints -INF below −95.9 (CRMonitorLevel floor −96).
            readout: tapers::format_monitor_db(db),
            mono: self.bool_at(&m.mono()).unwrap_or(false),
            mute: self.bool_at(&m.mute()).unwrap_or(false),
        }
    }

    fn monitor_meters(&self) -> MeterFrame {
        match &self.store.topology().monitor {
            Some(m) => self.frame(&m.meters.clone()),
            None => MeterFrame::default(),
        }
    }

    fn input_sends(&self, strip: &StripInfo) -> Vec<SendState> {
        self.surface_input_sends(strip)
    }

    fn send_pre(&self, strip: &StripInfo, k: usize) -> Option<bool> {
        self.surface_send_pre(strip, k)
    }

    fn isolated(&self, strip: &StripInfo) -> bool {
        self.input(&strip.id)
            .and_then(|s| self.bool_at(&s.isolate()))
            .unwrap_or(false)
    }

    fn input_sources(&self, strip: &StripInfo) -> Vec<String> {
        self.surface_input_sources(strip)
    }

    fn plugin_catalog(&self) -> Vec<PluginEntry> {
        self.surface_catalog()
    }

    fn plugin_scenes(&self) -> Vec<String> {
        self.surface_scenes()
    }

    fn status(&self) -> StatusInfo {
        let mut s = self.surface_status();
        // The engine is authoritative; the helper fills the rate when the engine has none.
        if s.sample_rate.is_none() {
            s.sample_rate = self
                .helper_info()
                .and_then(|i| i.sample_rate)
                .map(|r| r as u32);
        }
        s
    }

    fn engine_settings(&self) -> EngineSettings {
        let mut s = self.surface_settings();
        if let Some(info) = self.helper_info() {
            if s.sample_rate.is_none() {
                s.sample_rate = info.sample_rate.map(|r| r as u32);
            }
            if s.sample_rates.is_empty() {
                s.sample_rates = info.sample_rates.iter().map(|r| *r as u32).collect();
            }
        }
        s
    }

    fn monitor_options(&self) -> MonitorOptions {
        self.surface_monitor_options()
    }

    fn cue_outputs(&self) -> Vec<CueOutput> {
        self.surface_cues()
    }

    fn headphone_outputs(&self) -> Vec<HeadphoneOutput> {
        self.surface_headphones()
    }

    fn set_text(&mut self, id: &ControlId, text: &str) {
        if let Control::SettingEdit(k) = &id.control {
            return self.apply_setting(k, &rack::settings::SettingOp::Text(text.to_string()));
        }
        if self.session_set_text(id, text) {
            return;
        }
        self.surface_set_text(id, text);
    }

    fn properties(&self) -> Option<&dyn rack::settings::PropertySource> {
        Some(self)
    }

    fn apply_setting_writes(&mut self, writes: Vec<rack::settings::Write>) {
        self.write_settings(writes);
    }

    fn strip_menu_state(&self, s: &StripInfo) -> rack::menu::StripMenuState {
        self.surface_strip_menu_state(s)
    }

    fn menu_state(&self) -> rack::menu::MenuModelState {
        self.surface_menu_state()
    }

    fn apply(&mut self, action: &Action) {
        if let Action::Set(ControlId { control: Control::SettingSet(k), .. }, v) = action {
            return self.apply_setting(k, &rack::settings::SettingOp::Value(*v));
        }
        if let Action::Tap(id) | Action::Toggle(id) = action {
            match &id.control {
                Control::SettingChoose(k, i) => return self.apply_setting(k, &rack::settings::SettingOp::Choose(*i as usize)),
                Control::SettingPress(k) => return self.apply_setting(k, &rack::settings::SettingOp::Press),
                _ => {}
            }
        }
        if self.apply_session(action) {
            return;
        }
        if self.apply_surface(action) {
            return;
        }
        match action {
            Action::GestureBegin(_) => self.store.begin_gesture(),
            Action::GestureEnd(_) => self.store.end_gesture(),
            Action::Tap(id) => self.taps.push(id.clone()),
            Action::Set(id, v) => {
                let v = *v;
                if id.control == Control::MonitorLevel {
                    if let Some(m) = self.store.topology().monitor.as_ref().map(|m| m.level()) {
                        self.store.set(&m, v);
                    }
                    return;
                }
                let path = if let Some(s) = self.input(&id.strip) {
                    match &id.control {
                        Control::Fader => Some((s.fader(), v)),
                        Control::PanL => Some((s.pan(), tapers::pan::pan_from_knob(v))),
                        Control::PanR => Some((s.pan2(), tapers::pan::pan_from_knob(v))),
                        Control::Preamp(c) if &**c == "preamp_gain" => {
                            EngineMixer::preamp_path(s, c).map(|p| (p, v))
                        }
                        _ => None,
                    }
                } else if let Some(a) = self.aux(&id.strip) {
                    match &id.control {
                        Control::Fader => Some((a.fader(), v)),
                        Control::SendLevel(k) => {
                            self.aux_sends(a).get(*k as usize).map(|s| (s.gain(), v))
                        }
                        _ => None,
                    }
                } else {
                    None
                };
                if let Some((p, v)) = path {
                    self.store.set(&p, v);
                }
            }
            Action::Toggle(id) => {
                let monitor = self.store.topology().monitor.clone();
                let target: Option<(String, bool)> = match &id.control {
                    Control::MonitorMono => monitor.map(|m| (m.mono(), false)),
                    Control::MonitorMute => monitor.map(|m| (m.mute(), false)),
                    c => {
                        if let Some(s) = self.input(&id.strip) {
                            match c {
                                Control::Solo => Some((s.solo(), false)),
                                Control::Mute => Some((s.mute(), false)),
                                Control::RecMon => Some((s.record_pre_effects(), false)),
                                Control::Preamp(name) if self.preamp_enabled(s, name) => {
                                    EngineMixer::preamp_path(s, name)
                                        .filter(|_| &**name != "preamp_gain")
                                        .map(|p| (p, false))
                                }
                                _ => None,
                            }
                        } else if let Some(a) = self.aux(&id.strip) {
                            match c {
                                Control::Mute => Some((a.mute(), false)),
                                Control::Mono => Some((a.mono(), false)),
                                Control::PrePost => Some((a.send_post_fader(), true)),
                                Control::SendMute(k) => self
                                    .aux_sends(a)
                                    .get(*k as usize)
                                    .map(|s| (s.bypass(), false)),
                                _ => None,
                            }
                        } else {
                            None
                        }
                    }
                };
                if let Some((p, default)) = target {
                    self.toggle(&p, default);
                }
            }
        }
    }
}
