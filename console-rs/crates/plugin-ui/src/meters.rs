//! Spec meters fed from the slot's `VendorSpecific` property while a window is open: the pure
//! half of the Swift runtime's meter handling (`SpecPluginViewModel`: `pollFloats`,
//! `opcodeRange`, `ingestVendorSpecific`, `updateOpcodeMeter`, `condition`, `derived`,
//! `updateCombined`, `curve(for:)`, `calibrate`, `tickMeters`). The engine adapter does the
//! read-only `get`s and calls [`MeterFeed::ingest_floats`] / [`MeterFeed::ingest_opcode`], then
//! [`MeterFeed::tick`] each frame.
//!
//! - `get <slot>/VendorSpecific/value?floats=ID,…` → `"h1,h2,…"`: hex float bits per id, in
//!   ascending id order (`"0"` for 0.0); a leading status field is tolerated, and a single polled
//!   id takes the last field.
//! - `get <slot>/VendorSpecific/value?opcode=519&p0=ID&p1=N&p2=1` → a JSON array, sent as a
//!   string (`"[\"0\", \"0\"]"`), of N hex float bit patterns for variables ID…ID+N−1. One opcode
//!   range per plug-in ([`PluginSpec::opcode_range`]) covers the opcode meters and analyser bins.
//!
//! Per meter id the feed keeps the conditioned raw value ([`MeterFeed::raw`]: integer bits,
//! `absolute`, `ignoreZero` hold, `derive: "direction"`), the calibrated 0…1 target and the
//! displayed needle ([`MeterFeed::needle`], ballistics `smoothing`, default 0.7 per 60 Hz tick;
//! ≤ 0 or ≥ 1 follows the target immediately). Specs with an analyser keep the latest opcode
//! block by variable id ([`MeterFeed::block`]).

use std::collections::HashMap;

use crate::spec::{MeterSource, PluginSpec};

/// Seconds per Swift ballistics tick.
pub const TICK: f64 = 0.016;

#[derive(Clone, Debug, Default)]
pub struct MeterFeed {
    targets: HashMap<String, f64>,
    needles: HashMap<String, f64>,
    raw: HashMap<String, f64>,
    block: HashMap<i64, f64>,
    last_raw: HashMap<String, f64>,
    last_direction: HashMap<String, f64>,
}

impl MeterFeed {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn has_meters(spec: &PluginSpec) -> bool {
        spec.meter_sources.as_ref().is_some_and(|m| !m.is_empty()) || spec.has_spectrum()
    }

    /// `<slot>/VendorSpecific/value?floats=…`, or None when nothing is polled that way.
    pub fn floats_query(spec: &PluginSpec, slot: &str) -> Option<String> {
        let ids = spec.poll_floats();
        (!ids.is_empty()).then(|| format!("{slot}/VendorSpecific/value?floats={}", ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",")))
    }

    /// `<slot>/VendorSpecific/value?opcode=OP&p0=P0&p1=COUNT&p2=1` (p0 as an unsigned 32-bit id).
    pub fn opcode_query(spec: &PluginSpec, slot: &str) -> Option<String> {
        spec.opcode_range().map(|(op, p0, n)| format!("{slot}/VendorSpecific/value?opcode={op}&p0={}&p1={n}&p2=1", p0 as u32))
    }

    /// The fields of a `floats=` reply for `expected` ids: exactly `expected`, or one more (a
    /// leading status field), or the last field when one id is polled.
    fn fields(raw: &str, expected: usize) -> Option<Vec<String>> {
        let cleaned = raw.replace('"', "");
        let parts: Vec<String> = cleaned.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
        if parts.is_empty() || expected == 0 {
            return None;
        }
        if parts.len() == expected {
            Some(parts)
        } else if parts.len() == expected + 1 {
            Some(parts[1..].to_vec())
        } else if expected == 1 {
            Some(parts[parts.len() - 1..].to_vec())
        } else {
            None
        }
    }

    /// `"3f800000,0"` → bit patterns (None for a field that is not hex).
    pub fn parse_float_bits(raw: &str, expected: usize) -> Option<Vec<Option<u32>>> {
        Some(Self::fields(raw, expected)?.iter().map(|s| u32::from_str_radix(s, 16).ok()).collect())
    }

    /// `"3f800000,0"` → [1.0, 0.0] (unparsable fields read 0).
    pub fn parse_floats(raw: &str, expected: usize) -> Option<Vec<f64>> {
        Some(Self::parse_float_bits(raw, expected)?.into_iter().map(|b| b.map(|b| f32::from_bits(b) as f64).unwrap_or(0.0)).collect())
    }

    /// An opcode reply: a JSON array (or a string holding one) of hex strings or integer bits.
    pub fn parse_array(reply: &serde_json::Value) -> Option<Vec<f64>> {
        let inner;
        let items = match reply {
            serde_json::Value::Array(a) => a,
            serde_json::Value::String(s) => {
                inner = serde_json::from_str::<serde_json::Value>(s).ok()?;
                inner.as_array()?
            }
            _ => return None,
        };
        Some(
            items
                .iter()
                .map(|v| match v {
                    serde_json::Value::String(s) => u32::from_str_radix(s.trim(), 16).map(|b| f32::from_bits(b) as f64).unwrap_or(0.0),
                    serde_json::Value::Number(n) => f32::from_bits(n.as_i64().unwrap_or(0) as u32) as f64,
                    _ => 0.0,
                })
                .collect(),
        )
    }

    /// Feeds a `floats=` reply. `mode` gives the current value of a meter's `mode_param`.
    pub fn ingest_floats(&mut self, spec: &PluginSpec, raw: &str, mode: impl Fn(u16) -> f64) {
        let ids = spec.poll_floats();
        let Some(values) = Self::parse_float_bits(raw, ids.len()) else { return };
        let by_id: HashMap<i64, u32> = ids.into_iter().zip(values).filter_map(|(id, b)| Some((id, b?))).collect();
        for (meter, src) in spec.meter_sources.iter().flatten() {
            if spec.uses_opcode(src) {
                continue;
            }
            let Some(&bits) = src.vendor_specific_float.and_then(|id| by_id.get(&id)) else { continue };
            let raw = if src.is_integer() { bits as i32 as f64 } else { f32::from_bits(bits) as f64 };
            self.accept(meter, src, raw, &mode);
        }
        self.update_combined(spec);
    }

    /// Feeds an opcode reply whose first value is variable `p0`.
    pub fn ingest_opcode(&mut self, spec: &PluginSpec, p0: i64, values: &[f64], mode: impl Fn(u16) -> f64) {
        if spec.has_spectrum() {
            for (i, &v) in values.iter().enumerate() {
                self.block.insert(p0 + i as i64, v);
            }
        }
        for (meter, src) in spec.meter_sources.iter().flatten() {
            let Some(o) = src.opcode.filter(|_| spec.uses_opcode(src)) else { continue };
            let i = o.p0 - p0;
            if i < 0 || i as usize >= values.len() {
                continue;
            }
            let v = values[i as usize];
            // Integer variables arrive as their bit pattern in the float slot.
            let raw = if src.is_integer() { (v as f32).to_bits() as i32 as f64 } else { v };
            self.accept(meter, src, raw, &mode);
        }
        self.update_combined(spec);
    }

    fn accept(&mut self, meter: &str, src: &MeterSource, raw: f64, mode: &impl Fn(u16) -> f64) {
        let Some(c) = condition(raw, src) else { return };
        let v = self.derived(meter, c, src);
        self.raw.insert(meter.to_string(), v);
        self.targets.insert(meter.to_string(), MeterSource::calibrate(v, src.curve(src.mode_param.map(mode))));
    }

    /// `derive: "direction"`: +1 rising, −1 falling; the previous sign holds through still
    /// samples (|Δ| ≤ 1e-6) and phase wraps (|Δ| > 0.5); +1 before the first movement.
    fn derived(&mut self, meter: &str, v: f64, src: &MeterSource) -> f64 {
        if src.derive.as_deref() != Some("direction") {
            return v;
        }
        let previous = self.last_raw.insert(meter.to_string(), v);
        if let Some(previous) = previous {
            let delta = v - previous;
            if delta.abs() > 1e-6 && delta.abs() <= 0.5 {
                self.last_direction.insert(meter.to_string(), if delta > 0.0 { 1.0 } else { -1.0 });
            }
        }
        self.last_direction.get(meter).copied().unwrap_or(1.0)
    }

    /// Derived sources: the max of their (calibrated) input targets, recalibrated when they have
    /// a curve (ConsoleLink combines targets before smoothing).
    fn update_combined(&mut self, spec: &PluginSpec) {
        for (id, src) in spec.meter_sources.iter().flatten() {
            let Some(combine) = src.combine.as_ref().filter(|c| c.op == "max") else { continue };
            let present: Vec<f64> = combine.meters.iter().filter_map(|m| self.targets.get(m).copied()).collect();
            let v = if present.is_empty() { 0.0 } else { present[1..].iter().fold(present[0], |a, &b| if b > a { b } else { a }) };
            let v = if src.calibration.is_empty() { v } else { MeterSource::calibrate(v, &src.calibration) };
            self.targets.insert(id.clone(), v);
        }
    }

    /// Moves the needles toward their targets for `seconds` of Swift's 60 Hz ticks. True when a
    /// needle moved.
    pub fn tick(&mut self, spec: &PluginSpec, seconds: f64) -> bool {
        let steps = (seconds / TICK).max(0.0);
        if steps == 0.0 {
            return false;
        }
        let mut moved = false;
        for (id, &target) in &self.targets {
            let s = spec.meter_source(id).and_then(|m| m.smoothing).unwrap_or(0.7);
            let current = self.needles.get(id).copied().unwrap_or(0.0);
            let next = if s <= 0.0 || s >= 1.0 || (target - current).abs() < 0.002 {
                target
            } else {
                current + (target - current) * (1.0 - (1.0 - s).powf(steps))
            };
            if next != current {
                self.needles.insert(id.clone(), next);
                moved = true;
            }
        }
        moved
    }

    /// True while some needle is away from its target (the panel keeps redrawing).
    pub fn settling(&self) -> bool {
        self.targets.iter().any(|(id, t)| self.needles.get(id).copied().unwrap_or(0.0) != *t)
    }

    /// The displayed needle position 0…1 (0 before any reply).
    pub fn needle(&self, id: &str) -> f64 {
        self.needles.get(id).copied().unwrap_or(0.0)
    }

    /// The calibrated target 0…1.
    pub fn target(&self, id: &str) -> Option<f64> {
        self.targets.get(id).copied()
    }

    /// The conditioned raw value (text readouts, GraphValue `meter` sources).
    pub fn raw(&self, id: &str) -> Option<f64> {
        self.raw.get(id).copied()
    }

    /// The latest opcode value of variable `id` (specs with an analyser).
    pub fn block(&self, id: i64) -> Option<f64> {
        self.block.get(&id).copied()
    }

    /// Sets a needle directly (renders and tests: a meter "at" a position).
    pub fn set_needle(&mut self, id: &str, v: f64) {
        self.needles.insert(id.to_string(), v);
        self.targets.insert(id.to_string(), v);
    }

    /// Sets a conditioned raw value directly (renders and tests).
    pub fn set_raw(&mut self, id: &str, v: f64) {
        self.raw.insert(id.to_string(), v);
    }

    /// Sets analyser variables directly (renders and tests).
    pub fn set_block(&mut self, p0: i64, values: &[f64]) {
        for (i, &v) in values.iter().enumerate() {
            self.block.insert(p0 + i as i64, v);
        }
    }
}

/// A source's raw-value options; None means "keep the previous target" (`ignoreZero`).
pub fn condition(raw: f64, src: &MeterSource) -> Option<f64> {
    if src.ignore_zero == Some(true) && raw == 0.0 {
        return None;
    }
    Some(if src.absolute == Some(true) { raw.abs() } else { raw })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(meters: serde_json::Value) -> PluginSpec {
        serde_json::from_value(serde_json::json!({
            "schema": 1, "effectName": "UAD T", "namePattern": "T", "size": {"w": 10, "h": 10},
            "parameters": [{"index": 0, "name": "Meter", "steps": ["VU", "PPM"], "default": 0}],
            "elements": [], "meterSources": meters
        }))
        .unwrap()
    }

    #[test]
    fn floats_reply_shapes() {
        assert_eq!(MeterFeed::parse_floats("3f800000,0", 2), Some(vec![1.0, 0.0]));
        assert_eq!(MeterFeed::parse_floats("ok,3f800000,0", 2), Some(vec![1.0, 0.0]));
        assert_eq!(MeterFeed::parse_floats("1,2,3f800000", 1), Some(vec![1.0]));
        assert_eq!(MeterFeed::parse_floats("1,2,3,4", 2), None);
        assert_eq!(MeterFeed::parse_float_bits("zz,0", 2), Some(vec![None, Some(0)]));
        let reply = serde_json::Value::String("[\"3f800000\", \"0\"]".into());
        assert_eq!(MeterFeed::parse_array(&reply), Some(vec![1.0, 0.0]));
    }

    #[test]
    fn conditioning_integer_absolute_ignore_zero() {
        let s = spec(serde_json::json!({
            "i": {"vendorSpecificFloat": 1, "integer": true, "smoothing": 0, "calibration": [[0, 0], [4, 1]]},
            "a": {"vendorSpecificFloat": 2, "absolute": true, "ignoreZero": true, "smoothing": 1, "calibration": [[0, 0], [1, 1]]}
        }));
        let mut f = MeterFeed::new();
        // 2 as an Int32 arrives as the bit pattern 0x00000002 (a denormal float).
        f.ingest_floats(&s, "2,bf000000", |_| 0.0);
        assert_eq!(f.raw("i"), Some(2.0));
        assert_eq!(f.raw("a"), Some(0.5));
        f.tick(&s, TICK);
        assert_eq!((f.needle("i"), f.needle("a")), (0.5, 0.5), "smoothing 0 and 1 follow immediately");
        f.ingest_floats(&s, "2,0", |_| 0.0);
        assert_eq!(f.raw("a"), Some(0.5), "ignoreZero holds the previous value");
        // Negative integers.
        f.ingest_floats(&s, "fffffffe,0", |_| 0.0);
        assert_eq!(f.raw("i"), Some(-2.0));
    }

    #[test]
    fn calibration_modes_and_ballistics() {
        let s = spec(serde_json::json!({
            "vu": {"vendorSpecificFloat": 7, "calibration": [[0, 0], [1, 0.5], [2, 1]], "modeParam": 0,
                   "modeCalibrations": [{"value": 1, "calibration": [[0, 0], [2, 0.25]]}]}
        }));
        assert_eq!(MeterSource::calibrate(1.5, &s.meter_source("vu").unwrap().calibration), 0.75);
        assert_eq!(MeterSource::calibrate(-5.0, &[vec![0.0, 0.2], vec![1.0, 1.0]]), 0.2, "ends are not clamped");
        assert_eq!(MeterSource::calibrate(f64::NAN, &[vec![0.0, 0.2], vec![1.0, 1.0]]), 0.0);
        assert_eq!(MeterSource::calibrate(1.0, &[]), 0.0);
        let mut f = MeterFeed::new();
        f.ingest_floats(&s, "3fc00000", |_| 0.0); // 1.5
        assert_eq!(f.target("vu"), Some(0.75));
        f.ingest_floats(&s, "3fc00000", |_| 1.0); // PPM curve
        assert_eq!(f.target("vu"), Some(0.1875));
        f.ingest_floats(&s, "3fc00000", |_| 0.5); // no curve within 0.01: the base curve
        assert_eq!(f.target("vu"), Some(0.75));
        // Default smoothing 0.7 per 16 ms tick: 0 → 0.525 → 0.6825 → …, snapping within 0.002.
        f.tick(&s, TICK);
        assert!((f.needle("vu") - 0.525).abs() < 1e-12);
        f.tick(&s, TICK);
        assert!((f.needle("vu") - 0.6825).abs() < 1e-12);
        for _ in 0..10 {
            f.tick(&s, TICK);
        }
        assert_eq!(f.needle("vu"), 0.75);
        assert!(!f.settling());
    }

    #[test]
    fn derive_direction_holds_through_still_samples_and_wraps() {
        let s = spec(serde_json::json!({
            "dir": {"vendorSpecificFloat": 3, "derive": "direction", "smoothing": 0, "calibration": [[-1, 0], [1, 1]]}
        }));
        let mut f = MeterFeed::new();
        let feed = |f: &mut MeterFeed, v: f32| f.ingest_floats(&s, &format!("{:x}", v.to_bits()), |_| 0.0);
        feed(&mut f, 0.2);
        assert_eq!(f.raw("dir"), Some(1.0), "+1 before any movement");
        feed(&mut f, 0.1);
        assert_eq!(f.raw("dir"), Some(-1.0));
        feed(&mut f, 0.1);
        assert_eq!(f.raw("dir"), Some(-1.0), "still: held");
        feed(&mut f, 0.9);
        assert_eq!(f.raw("dir"), Some(-1.0), "a jump > 0.5 is a phase wrap: held");
        feed(&mut f, 0.95);
        assert_eq!(f.raw("dir"), Some(1.0));
        assert_eq!(f.target("dir"), Some(1.0));
    }

    #[test]
    fn combined_meters_take_the_max_target_then_recalibrate() {
        let s = spec(serde_json::json!({
            "l": {"vendorSpecificFloat": 1, "calibration": [[0, 0], [1, 1]]},
            "r": {"vendorSpecificFloat": 2, "calibration": [[0, 0], [1, 1]]},
            "clip": {"combine": {"op": "max", "meters": ["l", "r", "missing"]}, "calibration": [[0.979, 0], [0.98, 1]], "smoothing": 0}
        }));
        assert_eq!(s.poll_floats(), vec![1, 2], "a derived meter polls nothing");
        let mut f = MeterFeed::new();
        f.ingest_floats(&s, "3f000000,3f7d70a4", |_| 0.0); // 0.5, 0.99
        assert_eq!(f.target("clip"), Some(1.0));
        f.ingest_floats(&s, "3f000000,3f000000", |_| 0.0);
        assert_eq!(f.target("clip"), Some(0.0));
        assert_eq!(f.raw("clip"), None, "combined meters have no raw value");
    }

    #[test]
    fn opcode_range_merges_meters_and_spectrum_and_keeps_the_block() {
        let mut s = spec(serde_json::json!({
            "gr": {"vendorSpecificFloat": 50331658, "opcode": {"op": 519, "p0": 50331658, "p1": 1}, "integer": true, "smoothing": 0, "calibration": [[0, 0], [10, 1]]},
            "vu": {"vendorSpecificFloat": 9, "calibration": [[0, 0], [1, 1]]}
        }));
        // Without an analyser: the tagged meter alone.
        assert_eq!(s.opcode_range(), Some((519, 50331658, 1)));
        assert_eq!(s.poll_floats(), vec![9]);
        assert_eq!(MeterFeed::opcode_query(&s, "/x").as_deref(), Some("/x/VendorSpecific/value?opcode=519&p0=50331658&p1=1&p2=1"));
        let mut f = MeterFeed::new();
        f.ingest_opcode(&s, 50331658, &[f32::from_bits(5) as f64], |_| 0.0);
        assert_eq!(f.raw("gr"), Some(5.0));
        assert_eq!(f.block(50331658), None, "no analyser: no block kept");
        // An analyser block (tag 0) ties the meter group (tag 3) one opcode each; the lower tag
        // wins and the starved meter falls back to floats=.
        s.elements.push(
            serde_json::from_value(serde_json::json!({
                "id": "g", "kind": "graph", "x": 0, "y": 0, "w": 10, "h": 10,
                "graph": {"type": "eq", "spectrum": {"first": 12, "count": 4, "hz0": 0, "hzStep": 1}}
            }))
            .unwrap(),
        );
        assert_eq!(s.opcode_range(), Some((519, 12, 4)));
        assert!(!s.uses_opcode(s.meter_source("gr").unwrap()));
        assert_eq!(s.poll_floats(), vec![9, 50331658]);
        f.ingest_opcode(&s, 12, &[1.0, 2.0, 3.0, 4.0], |_| 0.0);
        assert_eq!(f.block(14), Some(3.0));
    }
}
