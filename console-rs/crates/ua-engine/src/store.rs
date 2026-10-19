//! The engine state the UI binds to, owned by the UI thread.
//!
//! - The UI calls [`Store::drain`] once per frame (and when the network waker fires). It applies
//!   the network task's updates, releases echo holds, flushes the writer, refreshes metadata and
//!   recomputes the topology, and says whether anything changed.
//! - Values are read with [`Store::value`]/[`Store::property`] (paths with or without `/value`).
//!   [`Store::revision`] counts changes; [`Store::changed_since`] lists the paths that changed.
//! - Meters bypass the store entirely: they live in [`Store::meters`], fed by the network thread.
//! - [`Store::topology`] is the derived strip layout; [`Store::topology_revision`] moves only when
//!   it actually changes (a strip is linked, renamed, activated, the cue count changes…).
//! - Writes go through [`Store::set`]/[`Store::commit`]/gestures, gated off by default.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::time::{Duration, Instant};

use crate::command::Command;
use crate::connection::{
    CommandSink, ConnectionState, Endpoint, Engine, EngineHandle, Options, PendingReply, Update,
};
use crate::error::Result;
use crate::meters::{MeterReading, MeterStore};
use crate::path;
use crate::session::{self, SessionCommand, SessionState};
use crate::subscription::SubscriptionPlan;
use crate::topology::Topology;
use crate::tree::{Meta, Property, Snapshot, SnapshotInfo, Tree};
use crate::value::{Json, Value};
use crate::writer::{LocalModel, Writer};

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    /// Nothing loaded.
    Empty,
    /// Showing a snapshot (a file or a disconnected engine's last state); no engine connected.
    Offline(String),
    Connecting(String),
    /// Connected; fetching the tree.
    Loading,
    /// Tree loaded and subscriptions sent; pushes are applied as they arrive.
    Live,
    Reconnecting(String),
    Failed(String),
}

/// Paths changed since a revision (see [`Store::changed_since`]).
#[derive(Clone, Debug, PartialEq)]
pub enum Changes<'a> {
    /// The whole tree was replaced (or the change log no longer reaches that far back).
    All,
    /// Property paths, most recent first, without repeats.
    Paths(Vec<&'a str>),
}

impl Changes<'_> {
    pub fn contains(&self, path: &str) -> bool {
        match self {
            Changes::All => true,
            Changes::Paths(p) => p.contains(&path::property_path(path)),
        }
    }

    pub fn is_empty(&self) -> bool {
        matches!(self, Changes::Paths(p) if p.is_empty())
    }
}

/// Properties whose change alters the strip layout.
const STRUCTURAL: [&str; 12] = [
    "Stereo",
    "Active",
    "IOType",
    "Name",
    "StereoName",
    "ChannelHidden",
    "EnabledByUser",
    "CueBusCount",
    "DeviceName",
    "ID",
    "DeviceOnline",
    "MultiUnitOrder",
];
/// Properties whose change can change other properties' metadata (`48V/enabled`…). A new
/// `EffectName` means the slot's `parameters` subtree was replaced.
/// A change of these re-reads the node's metadata (`48V/enabled` and the like): the Hi-Z jack
/// too, which takes the preamp out of Mic.
const METADATA_TRIGGERS: [&str; 4] = ["Stereo", "IOType", "EffectName", "HiZ"];
const LOG_CAP: usize = 16_384;
const REFRESH_DELAY: Duration = Duration::from_millis(100);
/// `/UndoRedo`: an int whose `values` are `[{type:"undo"…},{type:"current"…},{type:"redo"…}]`.
pub const UNDO_REDO: &str = "/UndoRedo";
/// The daemon pulses these (true) when a device's clock-source / sample-rate lists change; the
/// lists are then re-read.
const LIST_CHANGED: [(&str, &str); 2] = [
    ("/ClockSourceListChanged", "/ClockSource"),
    ("/SampleRateListChanged", "/SampleRate"),
];
const MIDI_INPUT_DEVICE: &str = "/MidiInputDevice";
const MIDI_POLL: Duration = Duration::from_secs(1);
/// How long after a gesture the undo list is re-read (Console: 30–90 ms).
const UNDO_VALUES_DELAY: Duration = Duration::from_millis(40);

struct Hold {
    until: Instant,
    latest: Option<Value>,
}

/// Tree plus change tracking and echo holds (what the writer writes into).
struct Model {
    tree: Tree,
    revision: u64,
    log: VecDeque<(u64, String)>,
    log_floor: u64,
    path_revs: HashMap<String, u64>,
    holds: HashMap<String, Hold>,
    echo_hold: Duration,
    topology_dirty: bool,
    refresh_wanted: Vec<String>,
    /// A session identity property changed: re-read the session lists.
    values_wanted: bool,
    /// `/UndoRedo` changed (another client, the hardware): re-read its list.
    undo_values_wanted: bool,
    /// Properties whose `*ListChanged` flag pulsed: re-read their lists.
    lists_wanted: Vec<&'static str>,
    live: bool,
}

impl Model {
    fn record(&mut self, path: &str) {
        self.revision += 1;
        self.log.push_back((self.revision, path.to_owned()));
        if self.log.len() > LOG_CAP
            && let Some((r, _)) = self.log.pop_front()
        {
            self.log_floor = r;
        }
        match self.path_revs.get_mut(path) {
            Some(r) => *r = self.revision,
            None => {
                self.path_revs.insert(path.to_owned(), self.revision);
            }
        }
    }

    fn set_value(&mut self, path: &str, value: Value) -> bool {
        let pp = path::property_path(path);
        if !self.tree.set_value(pp, value) {
            return false;
        }
        self.record(pp);
        let (node, name) = path::split(pp);
        if STRUCTURAL.contains(&name) {
            self.topology_dirty = true;
        }
        if self.live && METADATA_TRIGGERS.contains(&name) {
            self.refresh_wanted.push(node.to_owned());
        }
        if self.live && session::IDENTITY_PATHS.contains(&pp) {
            self.values_wanted = true;
        }
        if self.live && pp == UNDO_REDO {
            self.undo_values_wanted = true;
        }
        if self.live
            && let Some((_, list)) = LIST_CHANGED.iter().find(|(flag, _)| *flag == pp)
            && self.tree.value(pp).is_some_and(|v| matches!(v, Value::Bool(true)))
        {
            self.lists_wanted.push(list);
        }
        true
    }
}

impl LocalModel for Model {
    fn apply_local(&mut self, path: &str, value: Value) {
        let pp = path::property_path(path).to_owned();
        self.set_value(&pp, value);
        let latest = self.holds.remove(&pp).and_then(|h| h.latest);
        self.holds.insert(
            pp,
            Hold {
                until: Instant::now() + self.echo_hold,
                latest,
            },
        );
    }

    fn is_integral(&self, path: &str) -> bool {
        self.tree.meta(path).is_some_and(Meta::is_integral)
    }

    fn has_property(&self, path: &str) -> bool {
        self.tree.has_property(path)
    }
}

struct Simulation {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Simulation {
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// A write the engine refused.
#[derive(Clone, Debug, PartialEq)]
pub struct Refusal {
    /// The property (`/SampleRate`).
    pub path: String,
    /// The engine's error text ("Unable to set value.").
    pub message: String,
    pub at: Instant,
}

type Waker = Arc<dyn Fn() + Send + Sync>;

/// The engine state the UI binds to. See the module docs.
pub struct Store {
    model: Model,
    writer: Writer,
    default_writes: bool,
    status: Status,
    topology: Topology,
    topologies: Vec<Topology>,
    topology_revision: u64,
    tree_revision: u64,
    node_revision: u64,
    info: SnapshotInfo,
    meters: Arc<MeterStore>,
    /// On-demand `<property>/value` subscriptions (plug-in windows), reference counted.
    retained: HashMap<String, u32>,
    handle: Option<EngineHandle>,
    sink: Option<Box<dyn CommandSink + Send>>,
    subscribed: HashSet<String>,
    refresh_due: HashMap<String, Instant>,
    refresh_inflight: Vec<(String, PendingReply)>,
    /// `get <property>/values` in flight (session lists).
    values_inflight: Vec<(String, PendingReply)>,
    /// `get <property>/value` in flight: re-reads after the engine refused a write, with the
    /// property's revision when the request went out.
    value_inflight: Vec<(String, u64, PendingReply)>,
    /// Writes the engine refused (`{"path", "error"}` replies), newest last, at most 16.
    refusals: VecDeque<Refusal>,
    /// When to re-read `/UndoRedo/values` after a gesture.
    undo_values_due: Option<Instant>,
    /// Next `/MidiInputDevice/values` poll.
    midi_poll_due: Option<Instant>,
    last_error: Option<String>,
    waker: Option<Waker>,
    simulation: Option<Simulation>,
}

impl Default for Store {
    fn default() -> Self {
        Store::new()
    }
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store")
            .field("status", &self.status)
            .field("revision", &self.model.revision)
            .field("device", &self.info.device)
            .finish()
    }
}

fn sink_of<'a>(
    sink: &'a Option<Box<dyn CommandSink + Send>>,
    handle: &'a Option<EngineHandle>,
) -> Option<&'a dyn CommandSink> {
    if let Some(s) = sink {
        return Some(s.as_ref());
    }
    handle.as_ref().map(|h| h as &dyn CommandSink)
}

impl Store {
    pub fn new() -> Store {
        Store::with_meters(Arc::new(MeterStore::new()))
    }

    /// A store feeding (and reading) an existing meter store.
    pub fn with_meters(meters: Arc<MeterStore>) -> Store {
        Store {
            model: Model {
                tree: Tree::new(),
                revision: 0,
                log: VecDeque::new(),
                log_floor: 0,
                path_revs: HashMap::new(),
                holds: HashMap::new(),
                echo_hold: Duration::from_millis(350),
                topology_dirty: false,
                refresh_wanted: Vec::new(),
                values_wanted: false,
                undo_values_wanted: false,
                lists_wanted: Vec::new(),
                live: false,
            },
            writer: Writer::default(),
            default_writes: false,
            status: Status::Empty,
            topology: Topology::default(),
            topologies: Vec::new(),
            topology_revision: 0,
            tree_revision: 0,
            node_revision: 0,
            info: SnapshotInfo::default(),
            meters,
            retained: HashMap::new(),
            handle: None,
            sink: None,
            subscribed: HashSet::new(),
            refresh_due: HashMap::new(),
            refresh_inflight: Vec::new(),
            values_inflight: Vec::new(),
            value_inflight: Vec::new(),
            refusals: VecDeque::new(),
            undo_values_due: None,
            midi_poll_due: None,
            last_error: None,
            waker: None,
            simulation: None,
        }
    }

    /// A store showing a snapshot offline.
    pub fn from_snapshot(snapshot: Snapshot) -> Store {
        let mut s = Store::new();
        s.load(snapshot);
        s
    }

    // MARK: Reading

    pub fn status(&self) -> &Status {
        &self.status
    }

    pub fn tree(&self) -> &Tree {
        &self.model.tree
    }

    /// Current value (`/a/Mute` or `/a/Mute/value`).
    pub fn value(&self, path: &str) -> Option<&Value> {
        self.model.tree.value(path)
    }

    pub fn property(&self, path: &str) -> Option<&Property> {
        self.model.tree.property(path)
    }

    pub fn meta(&self, path: &str) -> Option<&Meta> {
        self.model.tree.meta(path)
    }

    pub fn topology(&self) -> &Topology {
        &self.topology
    }

    /// Per-device topology in cascade order, preserving complete device paths.
    pub fn topologies(&self) -> &[Topology] {
        &self.topologies
    }

    pub fn topology_for_device(&self, device: usize) -> Option<&Topology> {
        self.topologies.iter().find(|t| t.device == device)
    }

    /// Bumped only when the topology actually changes.
    pub fn topology_revision(&self) -> u64 {
        self.topology_revision
    }

    /// Bumped each time a whole tree is installed.
    pub fn tree_revision(&self) -> u64 {
        self.tree_revision
    }

    /// Bumped each time a subtree is re-read ([`Store::refresh`], a plug-in swapped in a slot…).
    pub fn node_revision(&self) -> u64 {
        self.node_revision
    }

    /// Bumped on every value change (local or pushed) and every tree install.
    pub fn revision(&self) -> u64 {
        self.model.revision
    }

    /// The revision at which `path` last changed (0 if it has not changed since the tree was
    /// installed).
    pub fn path_revision(&self, path: &str) -> u64 {
        self.model
            .path_revs
            .get(path::property_path(path))
            .copied()
            .unwrap_or(0)
    }

    /// Paths changed after revision `rev`.
    pub fn changed_since(&self, rev: u64) -> Changes<'_> {
        if rev < self.model.log_floor {
            return Changes::All;
        }
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for (r, p) in self.model.log.iter().rev() {
            if *r <= rev {
                break;
            }
            if seen.insert(p.as_str()) {
                out.push(p.as_str());
            }
        }
        Changes::Paths(out)
    }

    pub fn info(&self) -> &SnapshotInfo {
        &self.info
    }

    pub fn device_name(&self) -> Option<&str> {
        self.info.device.as_deref()
    }

    pub fn engine_version(&self) -> Option<&str> {
        self.info.engine_version.as_deref()
    }

    pub fn meters(&self) -> &Arc<MeterStore> {
        &self.meters
    }

    /// Shortcut for `meters().reading(key)`.
    pub fn meter(&self, key: &str) -> MeterReading {
        self.meters.reading(key)
    }

    pub fn connection(&self) -> Option<&EngineHandle> {
        self.handle.as_ref()
    }

    /// Paths subscribed on the current connection.
    pub fn subscribed_paths(&self) -> &HashSet<String> {
        &self.subscribed
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// How long engine pushes for a path are held back after a local write, so a drag is not
    /// pulled back by echoes of its own earlier values (350 ms). The latest held value is applied
    /// after.
    pub fn set_echo_hold(&mut self, hold: Duration) {
        self.model.echo_hold = hold;
    }

    /// When [`Store::drain`] next has timed work (a writer flush, an echo-hold release, a
    /// metadata refresh). A UI that sleeps between events should wake by then.
    pub fn next_deadline(&self) -> Option<Instant> {
        let holds = self.model.holds.values().map(|h| h.until);
        let refresh = self.refresh_due.values().copied();
        let inflight =
            (!self.refresh_inflight.is_empty()).then(|| Instant::now() + Duration::from_millis(50));
        holds
            .chain(refresh)
            .chain(self.undo_values_due)
            .chain(self.midi_poll_due)
            .chain(self.writer.deadline())
            .chain(inflight)
            .min()
    }

    // MARK: Snapshots

    /// Shows a snapshot offline (disconnects first).
    pub fn load(&mut self, snapshot: Snapshot) {
        self.disconnect();
        let source = snapshot
            .info
            .source
            .clone()
            .unwrap_or_else(|| "snapshot".into());
        self.install(snapshot.tree, snapshot.info);
        self.status = Status::Offline(source);
    }

    pub fn load_snapshot(&mut self, path: impl AsRef<std::path::Path>) -> Result<()> {
        self.load(Snapshot::load(path)?);
        Ok(())
    }

    /// The current state as a snapshot (values include every push received so far).
    pub fn snapshot(&self, trimmed: bool) -> Snapshot {
        let mut info = self.info.clone();
        info.captured = Some(crate::iso8601_now());
        let tree = if trimmed {
            self.model.tree.trimmed(false)
        } else {
            self.model.tree.clone()
        };
        Snapshot::new(tree, info)
    }

    pub fn save_snapshot(
        &self,
        path: impl AsRef<std::path::Path>,
        trimmed: bool,
        pretty: bool,
    ) -> Result<()> {
        self.snapshot(trimmed).save(path, pretty)
    }

    // MARK: Live connection

    /// Connects (read-only unless `options.allow_writes`), loads the tree, then subscribes per
    /// strip. Reconnects with backoff and reloads on every reconnect.
    pub fn connect(&mut self, endpoint: Endpoint, mut options: Options) {
        self.disconnect();
        options.load_tree = true;
        self.status = Status::Connecting(endpoint.to_string());
        let handle = Engine::connect_with_meters(endpoint, options, self.meters.clone());
        if let Some(w) = &self.waker {
            let w = w.clone();
            handle.set_waker(move || w());
        }
        self.handle = Some(handle);
    }

    pub fn disconnect(&mut self) {
        self.handle = None;
        self.subscribed.clear();
        self.refresh_due.clear();
        self.refresh_inflight.clear();
        self.model.live = false;
        if self.status == Status::Live
            || matches!(
                self.status,
                Status::Connecting(_) | Status::Loading | Status::Reconnecting(_)
            )
        {
            self.status = Status::Offline("disconnected".into());
        }
    }

    /// Called from the network thread whenever updates are queued (e.g. to wake a winit event
    /// loop). Applies to the current and future connections.
    pub fn set_waker(&mut self, waker: impl Fn() + Send + Sync + 'static) {
        let w: Waker = Arc::new(waker);
        if let Some(h) = &self.handle {
            let w = w.clone();
            h.set_waker(move || w());
        }
        self.waker = Some(w);
    }

    /// Replaces the command sink the writer uses (normally the connection). For tests.
    pub fn set_sink(&mut self, sink: impl CommandSink + Send + 'static) {
        self.sink = Some(Box::new(sink));
    }

    /// Applies everything that is due. Returns true when anything visible changed (values,
    /// topology or status). Call once per frame.
    pub fn drain(&mut self) -> bool {
        let rev = self.model.revision;
        let topo = self.topology_revision;
        let status = self.status.clone();
        let updates: Vec<Update> = match &self.handle {
            Some(h) => h.updates().try_iter().collect(),
            None => Vec::new(),
        };
        for u in updates {
            self.handle_update(u);
        }
        let now = Instant::now();
        self.release_holds(now);
        self.writer
            .poll(now, &self.model, sink_of(&self.sink, &self.handle));
        self.poll_refresh(now);
        self.poll_values();
        self.poll_rereads();
        if self.model.topology_dirty {
            self.recompute_topology();
        }
        self.model.revision != rev || self.topology_revision != topo || self.status != status
    }

    fn handle_update(&mut self, update: Update) {
        match update {
            Update::State(s) => match s {
                ConnectionState::Connecting => {
                    let ep = self
                        .handle
                        .as_ref()
                        .map(|h| h.endpoint().to_string())
                        .unwrap_or_default();
                    self.status = Status::Connecting(ep);
                }
                ConnectionState::Loading => self.status = Status::Loading,
                ConnectionState::Ready => {}
                ConnectionState::BackingOff { delay, reason, .. } => {
                    self.subscribed.clear();
                    self.model.live = false;
                    self.status = Status::Reconnecting(format!(
                        "{reason} — retry in {:.1} s",
                        delay.as_secs_f64()
                    ));
                }
                ConnectionState::Failed(r) => {
                    self.subscribed.clear();
                    self.model.live = false;
                    self.last_error = Some(r.clone());
                    self.status = Status::Failed(r);
                }
                ConnectionState::Closed | ConnectionState::Idle => {
                    self.subscribed.clear();
                    self.model.live = false;
                }
            },
            Update::Tree { tree, info } => {
                self.install(*tree, info);
                self.status = Status::Live;
                self.model.live = true;
                self.sync_subscriptions();
            }
            Update::Values(values) => {
                let now = Instant::now();
                for (p, v) in values {
                    self.apply_push(&p, v, now);
                }
            }
            Update::Error { path, message } => self.refused(&path, message),
            Update::Other(_) => {}
        }
    }

    /// An engine error reply. For a property this client writes (a `set` the engine refused:
    /// a sample rate the driver does not take, a clock source the unit does not offer), the
    /// optimistic local value must not stay: the echo hold is dropped (a value the engine pushed
    /// meanwhile lands at once) and the property is re-read (`get <prop>/value`), so the UI
    /// shows what the engine has. Console itself only snaps back on a value push; the open-apollo
    /// daemon sends one before its error, a UA engine may not.
    fn refused(&mut self, path: &str, message: String) {
        let prop = path::property_path(path.split('?').next().unwrap_or(path)).to_owned();
        if !self.model.tree.has_property(&prop) {
            log::debug!("engine error {path}: {message}");
            return;
        }
        log::info!("engine refused {prop}: {message}");
        self.refusals.push_back(Refusal { path: prop.clone(), message, at: Instant::now() });
        while self.refusals.len() > 16 {
            self.refusals.pop_front();
        }
        let mut props = vec![prop.clone()];
        // The local dB read-out of a tapered control (FaderLevel for FaderLevelTapered).
        if let Some((companion, _)) = crate::writer::companion(&prop, 0.0) {
            props.push(companion);
        }
        for p in props {
            if let Some(Hold { latest: Some(v), .. }) = self.model.holds.remove(&p) {
                self.model.set_value(&p, v);
            }
            self.reread(&p);
        }
        if self.model.topology_dirty {
            self.recompute_topology();
        }
    }

    /// Re-reads a property's value (read-only `get <prop>/value`); the reply replaces the local
    /// value in a later [`Store::drain`] unless the property changed since (a newer local write
    /// or engine push wins). False when not live.
    pub fn reread(&mut self, property: &str) -> bool {
        let prop = path::property_path(property).to_owned();
        if self.status != Status::Live || !self.model.tree.has_property(&prop) {
            return false;
        }
        let Some(handle) = &self.handle else { return false };
        if !self.value_inflight.iter().any(|(p, _, _)| *p == prop) {
            let rev = self.path_revision(&prop);
            let reply = handle.request(&path::value_path(&prop), false);
            self.value_inflight.push((prop, rev, reply));
        }
        true
    }

    fn poll_rereads(&mut self) {
        let mut done = Vec::new();
        self.value_inflight.retain(|(prop, rev, reply)| match reply.try_take() {
            None => true,
            Some(Ok(msg)) => {
                done.push((prop.clone(), *rev, msg.value().unwrap_or(Value::Null)));
                false
            }
            Some(Err(e)) => {
                log::debug!("re-read of {prop} failed: {e}");
                false
            }
        });
        for (prop, rev, v) in done {
            // Replies are read apart from the push stream: a value written or pushed after the
            // request is newer than the reply.
            if v != Value::Null && self.path_revision(&prop) == rev {
                self.model.holds.remove(&prop);
                self.model.set_value(&prop, v);
            }
        }
        if self.model.topology_dirty {
            self.recompute_topology();
        }
    }

    /// The writes the engine refused, newest last (at most 16).
    pub fn refusals(&self) -> impl Iterator<Item = &Refusal> {
        self.refusals.iter()
    }

    /// Applies pushed values as if they came from the engine (echo holds apply), then recomputes
    /// the topology if needed. For tests and simulations; live pushes arrive through `drain`.
    pub fn apply(&mut self, values: impl IntoIterator<Item = (String, Value)>) {
        let now = Instant::now();
        for (p, v) in values {
            self.apply_push(&p, v, now);
        }
        if self.model.topology_dirty {
            self.recompute_topology();
        }
    }

    fn apply_push(&mut self, path: &str, value: Value, now: Instant) {
        let pp = path::property_path(path);
        if let Some(h) = self.model.holds.get_mut(pp)
            && h.until > now
        {
            h.latest = Some(value);
            return;
        }
        self.model.set_value(pp, value);
    }

    fn release_holds(&mut self, now: Instant) {
        let expired: Vec<String> = self
            .model
            .holds
            .iter()
            .filter(|(_, h)| h.until <= now)
            .map(|(k, _)| k.clone())
            .collect();
        for p in expired {
            if let Some(Hold {
                latest: Some(v), ..
            }) = self.model.holds.remove(&p)
            {
                self.model.set_value(&p, v);
            }
        }
    }

    fn install(&mut self, tree: Tree, info: SnapshotInfo) {
        self.meters.seed(&tree);
        let mut info = info;
        if let Some(v) = tree.value("/Version").and_then(Value::as_str) {
            info.engine_version = Some(v.to_owned());
        }
        if let Some(d) = tree.value("/devices/0/DeviceName").and_then(Value::as_str) {
            info.device = Some(d.to_owned());
        }
        self.info = info;
        let m = &mut self.model;
        m.tree = tree;
        m.revision += 1;
        m.log.clear();
        m.log_floor = m.revision;
        m.path_revs.clear();
        m.refresh_wanted.clear();
        self.tree_revision += 1;
        self.recompute_topology();
    }

    fn recompute_topology(&mut self) {
        self.model.topology_dirty = false;
        let tree = &self.model.tree;
        let mut devices = tree.indices("/devices");
        devices.sort_by_key(|d| (tree.value(&format!("/devices/{d}/MultiUnitOrder")).and_then(Value::as_i64).unwrap_or(*d as i64), *d));
        let all: Vec<Topology> = devices.iter().map(|d| Topology::new(tree, *d)).collect();
        // Keep one primary control-room/bus model; do not guess how a cascade
        // shares aux/monitor ownership. Inputs are grouped by cascade order.
        let mut t = all.first().cloned().unwrap_or_else(|| Topology::new(tree, 0));
        t.all_inputs = all.iter().flat_map(|d| d.all_inputs.iter().cloned()).collect();
        t.inputs = all.iter().flat_map(|d| d.inputs.iter().cloned()).collect();
        if t != self.topology || all != self.topologies {
            self.topology = t;
            self.topologies = all;
            self.topology_revision += 1;
        }
        self.info.device = self.topology.device_name.clone();
        self.sync_subscriptions();
    }

    /// Sends `subscribe mode=multi` and the per-strip batches not yet sent; unsubscribes paths no
    /// longer wanted (one by one: the engine ignores batch unsubscribe). Read-only commands only.
    fn sync_subscriptions(&mut self) {
        if self.status != Status::Live {
            return;
        }
        let Some(handle) = &self.handle else { return };
        let plan = SubscriptionPlan::for_devices(&self.topologies, &self.model.tree);
        if self.subscribed.is_empty() {
            let _ = handle.send(Command::SubscribeMulti);
        }
        let mut desired: HashSet<&str> = HashSet::new();
        for batch in &plan.batches {
            desired.extend(batch.paths.iter().map(String::as_str));
            let new: Vec<String> = batch
                .paths
                .iter()
                .filter(|p| !self.subscribed.contains(*p))
                .cloned()
                .collect();
            if new.is_empty() {
                continue;
            }
            if handle.send(Command::SubscribeBatch(new.clone())).is_ok() {
                self.subscribed.extend(new);
            }
        }
        let mut extra: Vec<String> = self
            .retained
            .keys()
            .filter(|p| self.model.tree.has_property(p))
            .cloned()
            .collect();
        extra.sort();
        desired.extend(extra.iter().map(String::as_str));
        let new_extra: Vec<String> = extra
            .iter()
            .filter(|p| !self.subscribed.contains(*p))
            .cloned()
            .collect();
        if !new_extra.is_empty()
            && handle
                .send(Command::SubscribeBatch(new_extra.clone()))
                .is_ok()
        {
            self.subscribed.extend(new_extra);
        }
        let stale: Vec<String> = self
            .subscribed
            .iter()
            .filter(|p| !desired.contains(p.as_str()))
            .cloned()
            .collect();
        for p in stale {
            let _ = handle.send(Command::Unsubscribe(p.clone()));
            self.subscribed.remove(&p);
        }
    }

    // MARK: On-demand subscriptions and refreshes

    /// Keeps extra properties subscribed (read-only), e.g. a plug-in's parameters while its
    /// window is open. Counted: balance each call with [`Store::release_subscriptions`]. Survives
    /// reconnects; paths the tree does not contain are subscribed once they appear.
    pub fn retain_subscriptions<S: AsRef<str>>(&mut self, paths: &[S]) {
        for p in paths {
            *self
                .retained
                .entry(path::value_path(p.as_ref()))
                .or_insert(0) += 1;
        }
        self.sync_subscriptions();
    }

    pub fn release_subscriptions<S: AsRef<str>>(&mut self, paths: &[S]) {
        for p in paths {
            let key = path::value_path(p.as_ref());
            if let Some(n) = self.retained.get_mut(&key) {
                *n -= 1;
                if *n == 0 {
                    self.retained.remove(&key);
                }
            }
        }
        self.sync_subscriptions();
    }

    /// Paths currently held by [`Store::retain_subscriptions`] (`<property>/value`).
    pub fn retained_subscriptions(&self) -> impl Iterator<Item = &str> {
        self.retained.keys().map(String::as_str)
    }

    /// Re-reads a subtree with a read-only `get <node>?recursive=1` on the next
    /// [`Store::drain`]; [`Store::node_revision`] moves when it lands. Returns false offline.
    pub fn refresh(&mut self, node: &str) -> bool {
        if self.status != Status::Live || self.handle.is_none() {
            return false;
        }
        self.refresh_due.insert(node.to_owned(), Instant::now());
        true
    }

    /// Schedules and completes read-only metadata refreshes (`get <node>?recursive=1` after an
    /// `IOType` or `Stereo` change refreshes `48V/enabled` and the like).
    fn poll_refresh(&mut self, now: Instant) {
        for node in self.model.refresh_wanted.drain(..) {
            if !self.refresh_inflight.iter().any(|(n, _)| *n == node) {
                self.refresh_due.entry(node).or_insert(now + REFRESH_DELAY);
            }
        }
        let Some(handle) = &self.handle else {
            self.refresh_due.clear();
            return;
        };
        if self.status != Status::Live {
            return;
        }
        let due: Vec<String> = self
            .refresh_due
            .iter()
            .filter(|(_, t)| **t <= now)
            .map(|(k, _)| k.clone())
            .collect();
        for node in due {
            self.refresh_due.remove(&node);
            let reply = handle.request(&node, true);
            self.refresh_inflight.push((node, reply));
        }
        let mut done = Vec::new();
        self.refresh_inflight
            .retain(|(node, reply)| match reply.try_take() {
                None => true,
                Some(Ok(msg)) => {
                    if let Some(data) = msg.data {
                        done.push((node.clone(), data));
                    }
                    false
                }
                Some(Err(e)) => {
                    log::debug!("metadata refresh of {node} failed: {e}");
                    false
                }
            });
        for (node, data) in done {
            self.apply_subtree(&node, &data);
        }
    }

    fn apply_subtree(&mut self, node: &str, data: &crate::value::Json) {
        let prefix = format!("{node}/");
        let before: HashMap<String, Property> = self
            .model
            .tree
            .properties()
            .filter(|(p, _)| p.starts_with(&prefix))
            .map(|(p, v)| (p.to_owned(), v.clone()))
            .collect();
        self.model.tree.insert_node(data, node);
        let mut changed: Vec<String> = self
            .model
            .tree
            .properties()
            .filter(|(p, v)| p.starts_with(&prefix) && before.get(*p) != Some(*v))
            .map(|(p, _)| p.to_owned())
            .collect();
        changed.extend(
            before
                .keys()
                .filter(|p| !self.model.tree.has_property(p))
                .cloned(),
        );
        for p in changed {
            self.model.record(&p);
        }
        self.node_revision += 1;
        self.recompute_topology();
    }

    // MARK: Writing

    pub fn writer(&self) -> &Writer {
        &self.writer
    }

    /// Writer configuration (`writes_enabled`, `interval`, `auto_flush`, `updates_companions`).
    pub fn writer_mut(&mut self) -> &mut Writer {
        &mut self.writer
    }

    pub fn set_writes_enabled(&mut self, enabled: bool) {
        self.writer.writes_enabled = enabled;
    }

    /// Opens or closes both write gates: the writer's and the connection's (which
    /// [`Options::allow_writes`] set at connect). The user's WRITES switch.
    pub fn set_engine_writes(&mut self, enabled: bool) {
        self.writer.writes_enabled = enabled;
        if let Some(h) = &self.handle {
            h.set_allow_writes(enabled);
        }
    }

    /// Sets a property (`/a/FaderLevelTapered` or `/a/FaderLevelTapered/value`): the local value
    /// moves now, the wire write is coalesced (latest per path, ~60 Hz).
    pub fn set(&mut self, path: &str, value: impl Into<Value>) {
        if self.default_writes {
            self.writer.reset(&mut self.model, path, value.into());
        } else {
            self.writer.set(&mut self.model, path, value.into());
        }
        if self.model.topology_dirty {
            self.recompute_topology();
        }
    }

    /// While on, [`set`](Self::set) resets instead: the value shows locally and the wire carries
    /// `"default"` (an engine model maps a UI reset through its usual control → path code).
    pub fn set_default_writes(&mut self, on: bool) {
        self.default_writes = on;
    }

    /// Resets a property to the engine's default (Console's double-click): `local` shows now,
    /// the engine gets `set <path>/value "default"` in its own undo step.
    pub fn reset(&mut self, path: &str, local: impl Into<Value>) {
        self.begin_gesture();
        self.writer.reset(&mut self.model, path, local.into());
        self.end_gesture();
    }

    /// Tells the engine this client is in the background (`set /Sleep true`: pushes every
    /// 100 ms, no metering for this socket) or back (`false`). A per-socket hint, so it goes out
    /// on read-only connections too.
    pub fn set_sleep(&mut self, asleep: bool) {
        if let Some(h) = &self.handle {
            let _ = h.send(Command::set_raw("/Sleep", if asleep { "true" } else { "false" }));
        }
    }

    /// A `set` sent at once with no undo bracket: for properties that are not edits themselves
    /// (`/UndoRedo`, which an open `/UndoRecording` group would record).
    pub fn set_now(&mut self, path: &str, value: impl Into<Value>) {
        self.set(path, value);
        self.flush();
    }

    /// A discrete edit (button, menu): its own gesture, sent at once.
    pub fn commit(&mut self, path: &str, value: impl Into<Value>) {
        self.begin_gesture();
        self.set(path, value);
        self.end_gesture();
    }

    /// Opens an undo step (`set /UndoRecording/value true`). Nested calls are counted; only the
    /// outermost pair is sent.
    pub fn begin_gesture(&mut self) {
        self.writer
            .begin_gesture(&self.model, sink_of(&self.sink, &self.handle));
    }

    pub fn end_gesture(&mut self) {
        let closing = self.writer.gesture_depth() == 1;
        self.writer
            .end_gesture(&self.model, sink_of(&self.sink, &self.handle));
        // Console re-reads the undo list after every gesture (`get /UndoRedo/values?cmdID=15`,
        // 30–90 ms later in the 1.3.1 pcap) so Edit › Undo / Redo enable correctly.
        if closing && self.writer.writes_enabled {
            self.undo_values_due = Some(Instant::now() + UNDO_VALUES_DELAY);
        }
    }

    /// `post <path> <payload>` through the writer gate (suppressed and counted while writes are
    /// off), after any pending values.
    pub fn post(&mut self, path: &str, payload: Option<&Json>) {
        let command = Command::post(path, payload.map(Json::to_string));
        self.writer
            .send_now(&self.model, sink_of(&self.sink, &self.handle), command);
    }

    /// Runs a session command (load, save, new, …) through the writer gate. Loads move the
    /// local `/Session` value at once, like any `commit`.
    pub fn session(&mut self, command: &SessionCommand) {
        if !command.is_sendable() {
            log::warn!("session command refused (empty name or path): {command:?}");
            return;
        }
        match command {
            SessionCommand::Load(name) => self.commit(session::SESSION, name.as_str()),
            SessionCommand::RestoreModified(v) => {
                self.commit(session::MODIFIED_SESSIONS, v.as_str())
            }
            other => self.writer.send_now(
                &self.model,
                sink_of(&self.sink, &self.handle),
                other.command(),
            ),
        }
    }

    /// Runs a snapshot command through the writer gate. Recall and prime move the local values
    /// at once, like any `commit`; the posts re-read `/Snapshot/values` after them (the engine
    /// pushes no list change; Console re-reads it too).
    pub fn snapshot_command(&mut self, command: &crate::session::SnapshotCommand) {
        use crate::session::SnapshotCommand as S;
        match command {
            S::Recall(n) => self.commit(session::SNAPSHOT, n.as_str()),
            S::Prime(n) => self.commit(session::PRIMED_SNAPSHOT, n.as_str()),
            other => {
                self.writer.send_now(
                    &self.model,
                    sink_of(&self.sink, &self.handle),
                    other.command(),
                );
                self.model.values_wanted = self.model.live;
            }
        }
    }

    /// The open session, the Sessions folder tree and the modified sessions.
    pub fn session_state(&self) -> SessionState {
        SessionState::from_tree(&self.model.tree)
    }

    /// Re-reads `<property>/values` (read-only `get`) and replaces the property's list when the
    /// reply arrives (in a later `drain`). False when not live.
    pub fn refresh_values(&mut self, property: &str) -> bool {
        self.refresh_values_query(property, None)
    }

    /// [`refresh_values`](Self::refresh_values) with Console's request query (`cmdID=15` for
    /// `/UndoRedo`; `func_id` is added by the connection).
    fn refresh_values_query(&mut self, property: &str, query: Option<&str>) -> bool {
        let prop = path::property_path(property).to_owned();
        if self.status != Status::Live || !self.model.tree.has_property(&prop) {
            return false;
        }
        let Some(handle) = &self.handle else {
            return false;
        };
        if self.values_inflight.iter().any(|(p, _)| *p == prop) {
            return true;
        }
        let mut target = session::values_path(&prop);
        if let Some(q) = query {
            target = format!("{target}?{q}");
        }
        let reply = handle.request(&target, false);
        self.values_inflight.push((prop, reply));
        true
    }

    fn poll_values(&mut self) {
        if std::mem::take(&mut self.model.values_wanted) {
            for p in session::LIST_PATHS {
                self.refresh_values(p);
            }
        }
        // Console polls the MIDI input list every second (1.3.1 pcap:
        // `get /MidiInputDevice/values?func_id=n` at exactly 1.000 s).
        let now = Instant::now();
        if self.status == Status::Live && self.model.tree.has_property(MIDI_INPUT_DEVICE) {
            if self.midi_poll_due.is_none_or(|t| now >= t) {
                self.midi_poll_due = Some(now + MIDI_POLL);
                self.refresh_values(MIDI_INPUT_DEVICE);
            }
        } else {
            self.midi_poll_due = None;
        }
        for list in std::mem::take(&mut self.model.lists_wanted) {
            self.refresh_values(list);
        }
        let undo_due = self.undo_values_due.is_some_and(|t| Instant::now() >= t);
        if undo_due || std::mem::take(&mut self.model.undo_values_wanted) {
            self.undo_values_due = None;
            self.refresh_values_query(UNDO_REDO, Some("cmdID=15"));
        }
        let mut done = Vec::new();
        self.values_inflight
            .retain(|(prop, reply)| match reply.try_take() {
                None => true,
                Some(Ok(msg)) => {
                    done.push((prop.clone(), msg.data.unwrap_or(Json::Null)));
                    false
                }
                Some(Err(e)) => {
                    log::debug!("values refresh of {prop} failed: {e}");
                    false
                }
            });
        for (prop, data) in done {
            let Some(mut p) = self.model.tree.property(&prop).cloned() else {
                continue;
            };
            let values = (!data.is_null()).then_some(data);
            if p.meta.values != values {
                p.meta.values = values;
                self.model.tree.set_property(&prop, p);
                self.model.record(&prop);
            }
        }
    }

    /// Sends (or counts as suppressed) every pending value now.
    pub fn flush(&mut self) {
        self.writer
            .flush(&self.model, sink_of(&self.sink, &self.handle));
    }

    // MARK: Simulated meters (offline UI work)

    /// Drives the meter store with moving values for every strip in the topology (offline use).
    pub fn simulate_meters(&mut self, on: bool) {
        self.simulation = None;
        if !on {
            return;
        }
        let t = &self.topology;
        let keys: Vec<String> = t
            .all_inputs
            .iter()
            .flat_map(|s| s.meters.iter())
            .chain(t.auxes.iter().flat_map(|a| a.meters.iter()))
            .chain(t.monitor.iter().flat_map(|m| m.meters.iter()))
            .map(|m| m.base.clone())
            .collect();
        let meters = self.meters.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::Builder::new()
            .name("ua-engine meter sim".into())
            .spawn(move || {
                let mut seed = 0x9e37_79b9_7f4a_7c15u64;
                let mut t = 0.0f64;
                while !flag.load(Relaxed) {
                    for (i, key) in keys.iter().enumerate() {
                        seed ^= seed << 13;
                        seed ^= seed >> 7;
                        seed ^= seed << 17;
                        let noise = (seed % 600) as f64 / 100.0 - 3.0;
                        let phase = t * (0.7 + (i % 7) as f64 * 0.13) + i as f64;
                        let level = (-40.0 + 30.0 * phase.sin() + noise).clamp(-77.0, 0.0) as f32;
                        let prev = meters.reading(key);
                        meters.set(
                            key,
                            MeterReading {
                                level_db: level,
                                peak_db: level.max(prev.peak_db - 0.5),
                                clip: level > -1.0,
                                updates: prev.updates + 1,
                            },
                        );
                    }
                    t += 0.033;
                    std::thread::sleep(Duration::from_millis(33));
                }
            })
            .ok();
        self.simulation = Some(Simulation { stop, thread });
    }
}
