//! Meter values, fed straight from the network task and read lock-free by the UI.
//!
//! Meters never go through the store's change stream: a 33 Hz push would mark every strip
//! changed. The UI polls [`MeterStore::reading`] (or, cheaper, a [`MeterHandle`] it keeps per
//! strip) when it draws. Keys are meter node paths, e.g. `/devices/0/inputs/0/meters/0`; the
//! standalone dBFS properties (`CustomIntegratedPreampInputMeter`) are keyed by their property
//! path.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

use crate::path;
use crate::tree::Tree;
use crate::value::Value;

/// Latest reading of one meter (`…/meters/K/{MeterLevel,MeterPeakLevel,MeterClip}`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MeterReading {
    /// dBFS, −77 (silence) … 0.
    pub level_db: f32,
    /// Engine-held peak (honours `/PeakHold`), dBFS.
    pub peak_db: f32,
    /// Engine-held clip flag (honours `/ClipHold`).
    pub clip: bool,
    /// Pushes received for this meter (any of its three properties).
    pub updates: u32,
}

impl MeterReading {
    pub const FLOOR: f32 = -77.0;
    pub const SILENT: MeterReading = MeterReading {
        level_db: Self::FLOOR,
        peak_db: Self::FLOOR,
        clip: false,
        updates: 0,
    };
}

impl Default for MeterReading {
    fn default() -> Self {
        Self::SILENT
    }
}

/// Which property of a meter a path names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MeterProperty {
    Level,
    Peak,
    Clip,
    /// `/MeterPulse`, the engine's meter tick counter.
    Pulse,
}

/// One meter's atomics.
#[derive(Debug)]
pub struct MeterSlot {
    level: AtomicU32,
    peak: AtomicU32,
    clip: AtomicBool,
    updates: AtomicU32,
}

impl Default for MeterSlot {
    fn default() -> Self {
        MeterSlot {
            level: AtomicU32::new(MeterReading::FLOOR.to_bits()),
            peak: AtomicU32::new(MeterReading::FLOOR.to_bits()),
            clip: AtomicBool::new(false),
            updates: AtomicU32::new(0),
        }
    }
}

impl MeterSlot {
    pub fn reading(&self) -> MeterReading {
        MeterReading {
            level_db: f32::from_bits(self.level.load(Relaxed)),
            peak_db: f32::from_bits(self.peak.load(Relaxed)),
            clip: self.clip.load(Relaxed),
            updates: self.updates.load(Relaxed),
        }
    }

    fn store(&self, r: MeterReading) {
        self.level.store(r.level_db.to_bits(), Relaxed);
        self.peak.store(r.peak_db.to_bits(), Relaxed);
        self.clip.store(r.clip, Relaxed);
        self.updates.store(r.updates, Relaxed);
    }

    fn apply(&self, prop: MeterProperty, value: &Value) {
        match prop {
            MeterProperty::Level => self.level.store(
                value.as_f32().unwrap_or(MeterReading::FLOOR).to_bits(),
                Relaxed,
            ),
            MeterProperty::Peak => self.peak.store(
                value.as_f32().unwrap_or(MeterReading::FLOOR).to_bits(),
                Relaxed,
            ),
            MeterProperty::Clip => self.clip.store(value.as_bool().unwrap_or(false), Relaxed),
            MeterProperty::Pulse => {}
        }
    }
}

/// A shared, lock-free reader for one meter. Get one per strip from [`MeterStore::handle`] when
/// the topology changes, then call [`MeterHandle::reading`] every frame.
#[derive(Clone, Debug)]
pub struct MeterHandle(Arc<MeterSlot>);

impl MeterHandle {
    pub fn reading(&self) -> MeterReading {
        self.0.reading()
    }
}

/// Push statistics, for measuring the engine's meter cadence.
#[derive(Clone, Debug, Default)]
pub struct MeterStats {
    /// Meter property pushes received.
    pub pushes: u64,
    /// `/MeterPulse` pushes received (one per engine meter tick).
    pub pulses: u64,
    /// Distinct meters that received at least one push.
    pub active_meters: u64,
    /// Seconds between consecutive `/MeterPulse` arrivals (most recent 512).
    pub pulse_intervals: Vec<f64>,
}

#[derive(Default)]
struct PulseTiming {
    last: Option<Instant>,
    intervals: VecDeque<f64>,
}

/// Thread-safe store of meter values (`Send + Sync`).
///
/// Writes (network task) take a read lock on the slot map, plus a write lock only the first time
/// a meter is seen; values are atomics. Reads take the read lock, or none through a
/// [`MeterHandle`].
#[derive(Default)]
pub struct MeterStore {
    slots: RwLock<HashMap<Box<str>, Arc<MeterSlot>>>,
    pulse: AtomicI64,
    generation: AtomicU64,
    pushes: AtomicU64,
    pulses: AtomicU64,
    active: AtomicU64,
    timing: Mutex<PulseTiming>,
}

impl std::fmt::Debug for MeterStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MeterStore")
            .field("meters", &self.len())
            .field("pulse", &self.pulse())
            .finish()
    }
}

const METER_PROPERTIES: [(&str, MeterProperty); 3] = [
    ("MeterLevel", MeterProperty::Level),
    ("MeterPeakLevel", MeterProperty::Peak),
    ("MeterClip", MeterProperty::Clip),
];

impl MeterStore {
    pub fn new() -> MeterStore {
        MeterStore::default()
    }

    /// Classifies a pushed path: the meter key it updates and which property, or `None` for a
    /// non-meter path. `/MeterPulse` is reported as (`/MeterPulse`, `Pulse`).
    pub fn meter_key(path: &str) -> Option<(&str, MeterProperty)> {
        let pp = path::property_path(path);
        let (node, name) = path::split(pp);
        if let Some(&(_, prop)) = METER_PROPERTIES.iter().find(|(n, _)| *n == name)
            && !node.ends_with("/meters")
            && node.contains("/meters/")
        {
            return Some((node, prop));
        }
        if name == "CustomIntegratedPreampInputMeter" {
            return Some((pp, MeterProperty::Level));
        }
        if pp == "/MeterPulse" {
            return Some((pp, MeterProperty::Pulse));
        }
        None
    }

    /// True when `path` is routed to the meter store rather than to the tree.
    pub fn is_meter_path(path: &str) -> bool {
        Self::meter_key(path).is_some()
    }

    fn slot(&self, key: &str) -> Arc<MeterSlot> {
        if let Some(s) = self
            .slots
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
        {
            return s.clone();
        }
        self.slots
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .entry(key.into())
            .or_default()
            .clone()
    }

    /// Applies a pushed value if `path` is a meter. Returns false (and does nothing) otherwise.
    pub fn ingest(&self, path: &str, value: &Value) -> bool {
        let Some((key, prop)) = Self::meter_key(path) else {
            return false;
        };
        self.generation.fetch_add(1, Relaxed);
        if prop == MeterProperty::Pulse {
            self.pulse.store(value.as_i64().unwrap_or(0), Relaxed);
            self.pulses.fetch_add(1, Relaxed);
            let now = Instant::now();
            let mut t = self.timing.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(last) = t.last {
                t.intervals.push_back((now - last).as_secs_f64());
                if t.intervals.len() > 512 {
                    t.intervals.pop_front();
                }
            }
            t.last = Some(now);
            return true;
        }
        let slot = self.slot(key);
        slot.apply(prop, value);
        if slot.updates.fetch_add(1, Relaxed) == 0 {
            self.active.fetch_add(1, Relaxed);
        }
        self.pushes.fetch_add(1, Relaxed);
        true
    }

    /// Seeds meters from a tree (initial values; does not count as pushes).
    pub fn seed(&self, tree: &Tree) {
        for (path, prop) in tree.properties() {
            let Some((key, mp)) = Self::meter_key(path) else {
                continue;
            };
            if mp == MeterProperty::Pulse {
                continue;
            }
            if let Some(v) = &prop.value {
                self.slot(key).apply(mp, v);
            }
        }
        self.generation.fetch_add(1, Relaxed);
    }

    /// Latest reading; [`MeterReading::SILENT`] for a meter never seen.
    pub fn reading(&self, key: &str) -> MeterReading {
        self.slots
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
            .map(|s| s.reading())
            .unwrap_or(MeterReading::SILENT)
    }

    /// Several readings under one lock acquisition (one per frame for a whole strip row).
    pub fn readings<S: AsRef<str>>(&self, keys: &[S]) -> Vec<MeterReading> {
        let slots = self.slots.read().unwrap_or_else(|e| e.into_inner());
        keys.iter()
            .map(|k| {
                slots
                    .get(k.as_ref())
                    .map(|s| s.reading())
                    .unwrap_or(MeterReading::SILENT)
            })
            .collect()
    }

    /// A lock-free reader for `key` (the slot is created if the meter has not been seen yet).
    pub fn handle(&self, key: &str) -> MeterHandle {
        MeterHandle(self.slot(key))
    }

    /// Latest `/MeterPulse` counter.
    pub fn pulse(&self) -> i64 {
        self.pulse.load(Relaxed)
    }

    /// Bumped on every meter push and pulse: redraw meters when it moves.
    pub fn generation(&self) -> u64 {
        self.generation.load(Relaxed)
    }

    /// Number of meters known.
    pub fn len(&self) -> usize {
        self.slots.read().unwrap_or_else(|e| e.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn stats(&self) -> MeterStats {
        let t = self.timing.lock().unwrap_or_else(|e| e.into_inner());
        MeterStats {
            pushes: self.pushes.load(Relaxed),
            pulses: self.pulses.load(Relaxed),
            active_meters: self.active.load(Relaxed),
            pulse_intervals: t.intervals.iter().copied().collect(),
        }
    }

    pub fn reset_stats(&self) {
        let mut t = self.timing.lock().unwrap_or_else(|e| e.into_inner());
        *t = PulseTiming::default();
        self.pushes.store(0, Relaxed);
        self.pulses.store(0, Relaxed);
        self.active.store(0, Relaxed);
        for slot in self
            .slots
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .values()
        {
            slot.updates.store(0, Relaxed);
        }
    }

    /// Test and simulation hook: sets a reading directly.
    pub fn set(&self, key: &str, reading: MeterReading) {
        self.slot(key).store(reading);
        self.generation.fetch_add(1, Relaxed);
    }

    /// Forgets every meter (handles keep their last values but stop updating).
    pub fn clear(&self) {
        self.slots
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.generation.fetch_add(1, Relaxed);
    }
}
