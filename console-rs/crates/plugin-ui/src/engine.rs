//! [`ParamSource`] over ua-engine's [`Store`]: one slot's `<slot>/parameters/<k>/…`.
//!
//! Reads come from the store's tree (kept current by subscriptions the slot retains while its
//! window is open); writes go through the store's writer, so they obey its write gate (read-only
//! by default: values move locally and nothing is sent). See the README for the path mapping.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use ua_engine::connection::PendingReply;
use ua_engine::store::Status;
use ua_engine::value::Json;
use ua_engine::{InsertPaths, PluginParameter, Store};

use crate::params::{ParamSource, clamp01};

/// A plug-in slot whose window is open: its parameter list and retained subscriptions.
#[derive(Debug)]
pub struct EngineSlot {
    pub slot: InsertPaths,
    pub params: Vec<PluginParameter>,
    retained: Vec<String>,
    seen: Option<(u64, u64)>,
    /// While the list is empty: when the slot was last re-read and how many times.
    reread: (Instant, u32),
    /// A plug-in load into the slot is in flight ([`Store::insert_loading`]).
    loading: bool,
}

/// An empty slot is re-read this often (`get <slot>?recursive=1`) and at most [`REREADS`] times:
/// the daemon can publish `parameters/N/*` after the metadata refresh, and the store drops pushes
/// for paths its tree does not have yet.
const REREAD: Duration = Duration::from_millis(500);
const REREADS: u32 = 20;

impl EngineSlot {
    /// Refreshes the slot (read-only `get <slot>?recursive=1` when live) and subscribes its
    /// parameters. Balance with [`close`](Self::close).
    pub fn open(store: &mut Store, slot: InsertPaths) -> Self {
        store.refresh(&slot.base);
        let mut s = Self::new(slot, Vec::new());
        s.sync(store);
        s
    }

    /// Lists the slot's parameters without refreshing or subscribing (a one-off read, e.g. a
    /// headless render). Nothing to close.
    pub fn peek(store: &Store, slot: InsertPaths) -> Self {
        let params = PluginParameter::list(store.tree(), &slot);
        Self::new(slot, params)
    }

    fn new(slot: InsertPaths, params: Vec<PluginParameter>) -> Self {
        Self { slot, params, retained: Vec::new(), seen: None, reread: (Instant::now(), 0), loading: false }
    }

    /// Re-lists the parameters when the tree changed (refresh landed, plug-in swapped) and keeps
    /// `NormalizedValue`, `StringValue` and `StepValue` of each subscribed. An empty list is
    /// re-read now and then (see [`REREAD`]). True when the list changed.
    pub fn sync(&mut self, store: &mut Store) -> bool {
        // A load that commits publishes its parameters in the daemon's tree, and its name push
        // repeats the console's own optimistic one: re-read the slot once it is live.
        let loading = store.insert_loading(&self.slot);
        if self.loading && !loading {
            store.refresh(&self.slot.base);
            self.reread = (Instant::now(), 0);
        }
        self.loading = loading;
        let rev = (store.tree_revision(), store.node_revision());
        if self.seen == Some(rev) {
            if !loading && self.params.is_empty() && self.reread.1 < REREADS && self.reread.0.elapsed() >= REREAD {
                store.refresh(&self.slot.base);
                self.reread = (Instant::now(), self.reread.1 + 1);
            }
            return false;
        }
        self.seen = Some(rev);
        let list = PluginParameter::list(store.tree(), &self.slot);
        let paths: Vec<String> = list.iter().flat_map(|p| p.paths.subscribed()).filter(|p| store.tree().has_property(p)).collect();
        if paths != self.retained {
            store.retain_subscriptions(&paths);
            store.release_subscriptions(&self.retained);
            self.retained = paths;
        }
        let changed = list != self.params;
        if !list.is_empty() {
            self.reread.1 = 0;
        }
        self.params = list;
        changed
    }

    /// Whether [`sync`](Self::sync) still waits for the plug-in: a load in flight, or an empty
    /// slot it re-reads (the window keeps ticking).
    pub fn rereading(&self) -> bool {
        self.loading || (self.params.is_empty() && self.reread.1 < REREADS)
    }

    /// A plug-in load into the slot is in flight: its window shows the panel locked.
    pub fn loading(&self) -> bool {
        self.loading
    }

    /// The daemon's own word for a load in flight (`Status`, e.g. "Preparing plug-in…"), when it
    /// says more than "Loading".
    pub fn loading_text(&self, store: &Store) -> Option<String> {
        let s = store.value(&self.slot.status())?.as_str()?;
        (self.loading && !s.is_empty() && s != "Loading").then(|| s.to_owned())
    }

    /// Releases the subscriptions taken by [`open`](Self::open)/[`sync`](Self::sync).
    pub fn close(&mut self, store: &mut Store) {
        store.release_subscriptions(&self.retained);
        self.retained.clear();
    }

    pub fn retained(&self) -> &[String] {
        &self.retained
    }

    pub fn effect_name(&self, store: &Store) -> Option<String> {
        store.value(&self.slot.effect_name()).and_then(|v| v.as_str()).map(str::to_owned)
    }

    pub fn power(&self, store: &Store) -> Option<bool> {
        store.value(&self.slot.power()).and_then(|v| v.as_bool())
    }

    pub fn preset(&self, store: &Store) -> Option<String> {
        store.value(&self.slot.preset()).and_then(|v| v.as_str()).map(str::to_owned).filter(|s| !s.is_empty())
    }

    /// Slot bypass: `<slot>/Power` (one undo step, gated like every write).
    pub fn set_power(&self, store: &mut Store, on: bool) {
        store.commit(&self.slot.power(), on);
    }

    /// Read-only view (drawing).
    pub fn view<'a>(&'a self, store: &'a Store) -> StoreParams<'a> {
        StoreParams { slot: self, store }
    }

    /// Read-write view (input).
    pub fn view_mut<'a>(&'a self, store: &'a mut Store) -> StoreParamsMut<'a> {
        StoreParamsMut { slot: self, store }
    }

    fn param(&self, k: u16) -> Option<&PluginParameter> {
        self.params.iter().find(|p| p.index() == k as usize)
    }

    fn read_value(&self, store: &Store, k: u16) -> Option<f64> {
        self.param(k)?;
        store.value(&self.slot.parameter(k as usize).normalized()).and_then(|v| v.as_f64())
    }

    fn read_display(&self, store: &Store, k: u16) -> Option<String> {
        let p = self.param(k)?;
        let text = store.value(&p.paths.string()).and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(str::to_owned);
        text.or_else(|| {
            let v = self.read_value(store, k)?;
            p.is_stepped().then(|| p.steps[p.step_of(v)].clone())
        })
    }
}

macro_rules! read_impl {
    () => {
        fn count(&self) -> usize {
            self.slot.params.len()
        }
        fn indices(&self) -> Vec<u16> {
            self.slot.params.iter().map(|p| p.index() as u16).collect()
        }
        fn name(&self, k: u16) -> Option<String> {
            self.slot.param(k).map(|p| p.name.clone())
        }
        fn normalized(&self, k: u16) -> Option<f64> {
            self.slot.read_value(self.store, k)
        }
        fn display(&self, k: u16) -> Option<String> {
            self.slot.read_display(self.store, k)
        }
        fn steps(&self, k: u16) -> Vec<String> {
            self.slot.param(k).map(|p| p.steps.clone()).unwrap_or_default()
        }
    };
}

/// Read-only: writes are ignored.
pub struct StoreParams<'a> {
    slot: &'a EngineSlot,
    store: &'a Store,
}

impl ParamSource for StoreParams<'_> {
    read_impl!();
    fn begin_gesture(&mut self) {}
    fn set(&mut self, _: u16, _: f64) {}
    fn end_gesture(&mut self) {}
}

/// Writes `set <slot>/parameters/<k>/NormalizedValue/value v` through the store's gated writer.
pub struct StoreParamsMut<'a> {
    slot: &'a EngineSlot,
    store: &'a mut Store,
}

impl ParamSource for StoreParamsMut<'_> {
    read_impl!();
    fn begin_gesture(&mut self) {
        self.store.begin_gesture();
    }
    fn set(&mut self, k: u16, v: f64) {
        if self.slot.param(k).is_some() {
            self.store.set(&self.slot.slot.parameter(k as usize).normalized(), clamp01(v));
        }
    }
    fn end_gesture(&mut self) {
        self.store.end_gesture();
    }
}

/// Polls a spec panel's meter queries ([`crate::PluginPanel::meter_queries`]: read-only
/// `get <slot>/VendorSpecific/value?floats=…` / `?opcode=…`) over the store's live connection
/// while the window is open, as ConsoleLink does: one request per query in flight, every
/// `interval`, a second's back-off after an error. Nothing is sent offline.
#[derive(Debug)]
pub struct MeterPoller {
    inflight: Vec<(String, PendingReply)>,
    next: HashMap<String, Instant>,
    /// Time between requests of one query (default 1/30 s).
    pub interval: Duration,
}

impl Default for MeterPoller {
    fn default() -> Self {
        Self { inflight: Vec::new(), next: HashMap::new(), interval: Duration::from_millis(33) }
    }
}

impl MeterPoller {
    pub fn new() -> Self {
        Self::default()
    }

    /// Collects the replies that landed (query, data) and sends the queries that are due.
    pub fn poll(&mut self, store: &Store, queries: &[String]) -> Vec<(String, Json)> {
        let now = Instant::now();
        let mut out = Vec::new();
        let mut pending = Vec::new();
        for (q, reply) in self.inflight.drain(..) {
            match reply.try_take() {
                None => pending.push((q, reply)),
                Some(Ok(msg)) => {
                    if let Some(data) = msg.data {
                        out.push((q, data));
                    }
                }
                Some(Err(_)) => {
                    self.next.insert(q, now + Duration::from_secs(1));
                }
            }
        }
        self.inflight = pending;
        if *store.status() != Status::Live {
            return out;
        }
        let Some(handle) = store.connection() else { return out };
        for q in queries {
            if self.inflight.iter().any(|(p, _)| p == q) || self.next.get(q).is_some_and(|t| *t > now) {
                continue;
            }
            self.next.insert(q.clone(), now + self.interval);
            self.inflight.push((q.clone(), handle.request(q, false)));
        }
        out
    }

    /// Requests still waiting for a reply.
    pub fn in_flight(&self) -> usize {
        self.inflight.len()
    }
}
