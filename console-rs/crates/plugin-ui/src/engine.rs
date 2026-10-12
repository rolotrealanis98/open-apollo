//! [`ParamSource`] over ua-engine's [`Store`]: one slot's `<slot>/parameters/<k>/…`.
//!
//! Reads come from the store's tree (kept current by subscriptions the slot retains while its
//! window is open); writes go through the store's writer, so they obey its write gate (read-only
//! by default: values move locally and nothing is sent). See the README for the path mapping.

use ua_engine::{InsertPaths, PluginParameter, Store};

use crate::params::{ParamSource, clamp01};

/// A plug-in slot whose window is open: its parameter list and retained subscriptions.
#[derive(Debug)]
pub struct EngineSlot {
    pub slot: InsertPaths,
    pub params: Vec<PluginParameter>,
    retained: Vec<String>,
    seen: Option<(u64, u64)>,
}

impl EngineSlot {
    /// Refreshes the slot (read-only `get <slot>?recursive=1` when live) and subscribes its
    /// parameters. Balance with [`close`](Self::close).
    pub fn open(store: &mut Store, slot: InsertPaths) -> Self {
        store.refresh(&slot.base);
        let mut s = Self { slot, params: Vec::new(), retained: Vec::new(), seen: None };
        s.sync(store);
        s
    }

    /// Lists the slot's parameters without refreshing or subscribing (a one-off read, e.g. a
    /// headless render). Nothing to close.
    pub fn peek(store: &Store, slot: InsertPaths) -> Self {
        let params = PluginParameter::list(store.tree(), &slot);
        Self { slot, params, retained: Vec::new(), seen: None }
    }

    /// Re-lists the parameters when the tree changed (refresh landed, plug-in swapped) and keeps
    /// `NormalizedValue`, `StringValue` and `StepValue` of each subscribed. True when the list
    /// changed.
    pub fn sync(&mut self, store: &mut Store) -> bool {
        let rev = (store.tree_revision(), store.node_revision());
        if self.seen == Some(rev) {
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
        self.params = list;
        changed
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
