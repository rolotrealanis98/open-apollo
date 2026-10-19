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

mod inserts;
mod settings;
mod sessions;
mod menus;
mod presets;
mod slots;
mod surfaces;
mod midi;
mod correction;

pub use slots::{SlotInfo, SlotSource};

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use rack::geometry::StripKind;
use rack::model::{
    CueOutput, EngineSettings, HeadphoneOutput, InsertLabel, InsertState, MainState, MeterFrame,
    MixerModel, MonitorOptions, MonitorState, PluginEntry, PreampState, SendState, StatusInfo,
    StripInfo,
};
use rack::ballistics::{Hold, MeterBank};
use rack::{Action, Control, ControlId};
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
    native_load_notice: Option<String>,
    /// The read-only UA Mixer Helper (4720): native inserts and helper state.
    helper: Option<ua_engine::HelperStore>,
    /// Console's client-side preferences (Settings › OPTIONS / MIDI). Saved to `prefs_path`
    /// when one is set ([`EngineMixer::set_prefs_path`]), as Console keeps its own preferences.
    prefs: rack::settings::Prefs,
    prefs_path: Option<std::path::PathBuf>,
    /// The engine's Sessions folder on this computer (Show In Finder), when the engine is local.
    local_sessions_dir: Option<String>,
    midi: midi::MidiState,
    correction: correction::CorrectionState,
    /// Engine and Console meter ballistics ([`rack::ballistics`]), per meter node.
    ballistics: RefCell<MeterBank>,
    /// Epoch of the meter clock.
    meter_epoch: Instant,
    /// The frame's time on the meter clock, set by [`EngineMixer::pump`]; every meter drawn in a
    /// frame is evaluated at it.
    frame_time: Cell<Option<f64>>,
}

impl EngineMixer {
    /// A mixer over an existing store.
    pub fn new(store: Store) -> EngineMixer {
        EngineMixer {
            store,
            live_meters: true,
            taps: Vec::new(),
            clipboard: None,
            native_load_notice: None,
            helper: None,
            prefs: rack::settings::Prefs::default(),
            prefs_path: None,
            local_sessions_dir: None,
            midi: midi::MidiState::default(),
            correction: correction::CorrectionState::default(),
            ballistics: RefCell::default(),
            meter_epoch: Instant::now(),
            frame_time: Cell::new(None),
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
        let local = is_loopback(&endpoint.host);
        let options_allow_writes = options.allow_writes;
        let mut store = Store::new();
        store.connect(endpoint, options);
        store.set_writes_enabled(writes);
        let mut mixer = EngineMixer::new(store);
        if local {
            mixer.local_sessions_dir = default_sessions_dir();
        }
        // The UA Mixer Helper on host:4720, read-only (native inserts, helper state).
        if let Some(h) = helper {
            mixer.connect_helper_writable(h, writes && options_allow_writes);
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
        self.frame_time.set(Some(self.meter_epoch.elapsed().as_secs_f64()));
        let engine = self.store.drain();
        let helper = self.helper.as_mut().is_some_and(|h| h.drain());
        let midi = self.poll_midi();
        let correction = self.poll_correction();
        engine || helper || midi || correction
    }

    /// Sets the meter clock (seconds) the next frames are drawn at, as [`EngineMixer::pump`] does
    /// with the wall clock: for tests and scripted runs that step time themselves.
    pub fn set_meter_time(&self, seconds: f64) {
        self.frame_time.set(Some(seconds));
    }

    /// Alias of [`EngineMixer::pump`].
    pub fn drain(&mut self) -> bool {
        self.pump()
    }

    /// When [`EngineMixer::pump`] next has timed work (to schedule a wake-up).
    pub fn next_deadline(&self) -> Option<Instant> {
        let helper = self.helper.as_ref().and_then(|h| h.next_deadline());
        self.store.next_deadline().into_iter().chain(helper).chain(self.midi_deadline()).chain(self.correction_deadline()).min()
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

    /// A shown input strip, or the talkback mic strip.
    fn input(&self, id: &str) -> Option<&InputStrip> {
        self.store.topologies().iter().find_map(|t| {
            t.all_inputs.iter().find(|s| s.base == id).or_else(|| t.talkback.strip.as_ref().filter(|s| s.base == id))
        })
    }

    fn aux(&self, id: &str) -> Option<&AuxStrip> {
        self.store.topologies().iter().flat_map(|t| &t.auxes).find(|a| a.base == id)
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
        let device = a.base.split('/').nth(2).and_then(|d| d.parse().ok()).unwrap_or(self.store.topology().device);
        let active: Vec<String> = self
            .store
            .topology_for_device(device).unwrap_or(self.store.topology())
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
        // Drawn with the engine's release and peak hold and Console's easing, against the frame
        // clock: the daemon publishes raw levels at its own rate.
        let now = self.frame_time.get().unwrap_or_else(|| self.meter_epoch.elapsed().as_secs_f64());
        let hold = self.str_at("/PeakHold").and_then(Hold::from_label).unwrap_or(Hold::DEFAULT_PEAK);
        let mut bank = self.ballistics.borrow_mut();
        let left = bank.update(&meters[0].base, now, l.level_db as f64, l.peak_db as f64, hold);
        let right = match meters.get(1) {
            Some(m) => bank.update(&m.base, now, rr.level_db as f64, rr.peak_db as f64, hold),
            None => left,
        };
        MeterFrame {
            left: left.level,
            right: right.level,
            peak_left: left.peak,
            peak_right: right.peak,
            clip_left: l.clip,
            clip_right: rr.clip,
        }
    }

    /// The meter of the send at `base` (`…/sends/N`): its `meters/0` and, on a stereo strip, `meters/1`.
    pub(crate) fn send_frame(&self, base: &str) -> MeterFrame {
        let legs: Vec<MeterPaths> = (0..2).map(|k| MeterPaths::new(format!("{base}/meters/{k}"))).collect();
        self.frame(&legs)
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
    fn correction_image(&self,device:usize,slot:&str)->Option<&str> {
        self.correction_svg(device,slot)
    }
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

    fn snapshots(&self) -> rack::surfaces::SnapshotInfo {
        self.snapshot_info()
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

    fn inputs_for_view(&self, show_offline: bool) -> Vec<StripInfo> {
        self.inputs().into_iter().filter(|s| {
            let device = s.id.split('/').nth(2).and_then(|d| d.parse::<usize>().ok());
            show_offline || device.and_then(|d| self.bool_at(&format!("/devices/{d}/DeviceOnline"))).unwrap_or(true)
        }).collect()
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

    fn channel_presets(&self) -> Vec<rack::PresetNode> {
        // Every strip carries the same `Preset` tree (Channel Strip presets); take the first
        // shown input's, else the first aux's.
        let t = self.store.topology();
        let path = t.inputs.first().map(|s| format!("{}/Preset", s.base)).or_else(|| t.auxes.first().map(|a| format!("{}/Preset", a.base)));
        path.map(|p| self.preset_tree_at(&p)).unwrap_or_default()
    }

    fn plugin_presets(&self, s: &StripInfo, slot: u8) -> Vec<rack::PresetNode> {
        self.surface_plugin_presets(s, slot)
    }

    fn preset_name(&self, s: &StripInfo, scope: rack::presets::PresetScope) -> Option<String> {
        self.surface_preset_name(s, scope)
    }

    fn can_paste_plugin_settings(&self, s: &StripInfo, slot: u8) -> bool {
        self.surface_can_paste_plugin_settings(s, slot)
    }

    fn plugin_footer(&self, effect_name: &str) -> Option<rack::model::PluginFooter> {
        self.surface_plugin_footer(effect_name)
    }

    fn talkback(&self) -> Option<StripInfo> {
        let s = self.store.topology().talkback.strip.as_ref()?;
        Some(StripInfo {
            id: strip_id(&s.base),
            title: "TALKBACK".into(),
            kind: StripKind::Talkback,
            stereo: false,
            preamp_layout: None,
            shows_sr_convert: false,
            console_model: None,
        })
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
        // The front Hi-Z jack (`preamps/0/HiZ`, sensed by the unit) overrides MIC / LINE: Console
        // shows HI-Z on the source button (`Input-HiZ-Big`) and a ticked, disabled Hi-Z in its
        // menu.
        let hi_z = p.has_hi_z && self.bool_at(&p.hi_z()) == Some(true);
        let source = match surfaces::source_label(&io).as_str() {
            _ if hi_z => "HI-Z",
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
                pan: 0.5,
                meter: self.send_frame(&s.base),
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

    fn send_meter(&self, node: &str) -> MeterFrame {
        self.send_frame(node)
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

    fn plugin_is_unison(&self, name: &str) -> Option<bool> {
        self.surface_plugin_is_unison(name)
    }
    fn plugin_catalog(&self) -> Vec<PluginEntry> {
        self.surface_catalog()
    }

    fn plugin_notice(&self) -> Option<&str> {
        self.native_load_notice.as_deref().or_else(|| self.store.last_error())
    }

    fn insert_refusal(&self, slot: &ControlId, name: &str) -> Option<String> {
        self.native_assignment_refusal(&self.insert_slot(slot)?, name)
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
        if self.preset_set_text(id, text) {
            return;
        }
        self.surface_set_text(id, text);
    }

    fn properties(&self) -> Option<&dyn rack::settings::PropertySource> {
        Some(self)
    }

    /// The writes the engine refused (`Store::refusals`), newest first.
    fn notifications(&self) -> Vec<String> {
        let mut n: Vec<String> = self.store.refusals().map(|r| format!("{}: {}", r.path, r.message)).collect();
        n.reverse();
        n
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

    fn devices(&self) -> Vec<rack::model::DeviceInfo> {
        self.store.topologies().iter()
            .map(|t| rack::model::DeviceInfo {
                index: t.device,
                name: t.device_name.clone().unwrap_or_default(),
                online: t.device_online,
            })
            .collect()
    }

    /// View > Show/Hide Channels: every real input of the shown device, hidden ones included
    /// (`ChannelHidden`), in mixer order; not the talkback mic.
    fn channel_list(&self) -> Vec<rack::model::ChannelEntry> {
        use ua_engine::topology::InputKind;
        self.store
            .topology()
            .all_inputs
            .iter()
            .filter(|s| s.is_active && s.enabled_by_user && s.kind != InputKind::None && s.kind != InputKind::Talkback)
            .map(|s| rack::model::ChannelEntry { id: s.base.clone(), name: s.name.clone(), hidden: s.is_hidden })
            .collect()
    }

    fn set_background(&mut self, background: bool) {
        self.store.set_sleep(background);
    }

    /// Console posts `SaveLastUsedSession` when it quits; through the session command path, so
    /// a read-only run sends nothing.
    fn quit(&mut self) {
        self.session_command(&ua_engine::SessionCommand::SaveLastUsed);
    }

    fn apply(&mut self, action: &Action) {
        // A double-click reset: the same control → path mapping as a drag, written as Console's
        // `set <prop>/value "default"` (MAP.md: CMxNtwk_Server::setObjectValue).
        if let Action::Reset(id, v) = action {
            self.store.set_default_writes(true);
            self.apply(&Action::Set(id.clone(), *v));
            self.store.set_default_writes(false);
            return;
        }
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
        if self.apply_session(action) || self.apply_snapshot(action) {
            return;
        }
        if self.apply_presets(action) {
            return;
        }
        if self.apply_surface(action) {
            return;
        }
        match action {
            Action::GestureBegin(_) => self.store.begin_gesture(),
            Action::GestureEnd(_) => self.store.end_gesture(),
            Action::Reset(..) => {} // mapped to a default-writing Set above
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

/// The engine runs on this computer (`127.0.0.1`, `localhost`, `::1`).
fn is_loopback(host: &str) -> bool {
    let h = host.trim_matches(|c| c == '[' || c == ']');
    h.eq_ignore_ascii_case("localhost") || h.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// Where UA Mixer Engine (and the open-apollo daemon) keep sessions: `~/Documents/Universal
/// Audio/Sessions` of the user. A sandboxed host's `HOME` (`…/Library/Containers/<id>/Data`) is
/// mapped back to the user's home.
pub fn default_sessions_dir() -> Option<String> {
    let home = std::env::var("HOME").ok().filter(|h| !h.is_empty())?;
    let home = match home.find("/Library/Containers/") {
        Some(i) => home[..i].to_owned(),
        None => home,
    };
    Some(format!("{}/Documents/Universal Audio/Sessions", home.trim_end_matches('/')))
}

impl EngineMixer {
    /// The engine's Sessions folder as a local path (Show In Finder); `None` when the engine's
    /// files aren't on this computer. Connecting to a loopback engine sets the default.
    pub fn set_local_sessions_dir(&mut self, dir: Option<String>) {
        self.local_sessions_dir = dir;
    }
}
