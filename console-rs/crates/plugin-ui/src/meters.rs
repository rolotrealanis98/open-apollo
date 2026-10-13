//! Spec meters fed from the slot's `VendorSpecific` property while a window is open. Pure parts
//! of ConsoleLab's `PluginMeterFeed`: the queries to poll, reply decoding, calibration and needle
//! ballistics. The engine adapter does the `get`s (read-only) and calls [`MeterFeed::ingest_floats`]
//! / [`MeterFeed::ingest_opcode`], then [`MeterFeed::tick`] each frame.
//!
//! - `get <slot>/VendorSpecific/value?floats=ID,…` → `"h1,h2,…"`: hex float bits per id, in order
//!   (`"0"` for 0.0); a leading status field is tolerated.
//! - `get <slot>/VendorSpecific/value?opcode=519&p0=ID&p1=N&p2=1` → a JSON array, sent as a
//!   string (`"[\"0\", \"0\"]"`), of N hex float bit patterns for variables ID…ID+N−1.

use std::collections::HashMap;

use crate::spec::{MeterSource, PluginSpec};

pub struct MeterFeed {
    targets: HashMap<String, f64>,
}

impl Default for MeterFeed {
    fn default() -> Self {
        Self::new()
    }
}

impl MeterFeed {
    pub fn new() -> Self {
        Self { targets: HashMap::new() }
    }

    pub fn has_meters(spec: &PluginSpec) -> bool {
        spec.meter_sources.as_ref().is_some_and(|m| !m.is_empty())
    }

    /// `<slot>/VendorSpecific/value?floats=…`, or None when nothing is polled that way.
    pub fn floats_query(spec: &PluginSpec, slot: &str) -> Option<String> {
        let ids = spec.poll_floats();
        (!ids.is_empty()).then(|| format!("{slot}/VendorSpecific/value?floats={}", ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",")))
    }

    pub fn opcode_query(spec: &PluginSpec, slot: &str) -> Option<String> {
        spec.opcode_range().map(|(op, p0, n)| format!("{slot}/VendorSpecific/value?opcode={op}&p0={p0}&p1={n}&p2=1"))
    }

    /// `"3f800000,0"` → [1.0, 0.0]. Accepts a leading status field when there is one value too many.
    pub fn parse_floats(raw: &str, expected: usize) -> Option<Vec<f64>> {
        let cleaned = raw.replace('"', "");
        let parts: Vec<&str> = cleaned.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
        let values: &[&str] = if parts.len() == expected {
            &parts
        } else if parts.len() == expected + 1 {
            &parts[1..]
        } else if expected == 1 && !parts.is_empty() {
            &parts[parts.len() - 1..]
        } else {
            return None;
        };
        Some(values.iter().map(|s| hex_float(s)).collect())
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
                    serde_json::Value::String(s) => hex_float(s),
                    serde_json::Value::Number(n) => f32::from_bits(n.as_i64().unwrap_or(0) as u32) as f64,
                    _ => 0.0,
                })
                .collect(),
        )
    }

    /// Feeds a `floats=` reply. `mode` gives the current value of a meter's `mode_param`.
    pub fn ingest_floats(&mut self, spec: &PluginSpec, raw: &str, mode: impl Fn(u16) -> f64) {
        let ids = spec.poll_floats();
        let Some(values) = Self::parse_floats(raw, ids.len()) else { return };
        let by_id: HashMap<i64, f64> = ids.into_iter().zip(values).collect();
        for (meter, src) in spec.meter_sources.iter().flatten() {
            if spec.uses_opcode(src) {
                continue;
            }
            if let Some(&r) = src.vendor_specific_float.and_then(|id| by_id.get(&id)) {
                if let Some(target) = calibrated(r, src, &mode) {
                    self.targets.insert(meter.clone(), target);
                }
            }
        }
        self.update_combined(spec);
    }

    /// Feeds an opcode reply whose first value is variable `p0`.
    pub fn ingest_opcode(&mut self, spec: &PluginSpec, p0: i64, values: &[f64], mode: impl Fn(u16) -> f64) {
        for (meter, src) in spec.meter_sources.iter().flatten() {
            let Some(o) = src.opcode.filter(|_| spec.uses_opcode(src)) else { continue };
            let i = o.p0 - p0;
            if i >= 0 && (i as usize) < values.len() {
                if let Some(target) = calibrated(values[i as usize], src, &mode) {
                    self.targets.insert(meter.clone(), target);
                }
            }
        }
        self.update_combined(spec);
    }

    /// ConsoleLink combines calibrated targets, before smoothing the result.
    fn update_combined(&mut self, spec: &PluginSpec) {
        for (id, src) in spec.meter_sources.iter().flatten() {
            let Some(combine) = &src.combine else { continue };
            if combine.op != "max" { continue; }
            let value = combine.meters.iter().filter_map(|m| self.targets.get(m).copied()).fold(0.0, f64::max);
            let value = if src.calibration.is_empty() { value } else { MeterSource::calibrate(value, &src.calibration) };
            self.targets.insert(id.clone(), value);
        }
    }

    /// Moves `needles` toward their targets (ConsoleLink: factor `smoothing`, default 0.7, per
    /// 16 ms). Returns true when something moved.
    pub fn tick(&self, spec: &PluginSpec, needles: &mut HashMap<String, f64>, seconds: f64) -> bool {
        let steps = (seconds / 0.016).max(1.0);
        let mut moved = false;
        for (id, &target) in &self.targets {
            let s = spec.meter_sources.as_ref().and_then(|m| m.get(id)).and_then(|m| m.smoothing).unwrap_or(0.7);
            let k = 1.0 - (1.0 - s).powf(steps);
            let current = needles.get(id).copied().unwrap_or(0.0);
            let next = if (target - current).abs() < 0.002 { target } else { current + (target - current) * k };
            if next != current {
                needles.insert(id.clone(), next);
                moved = true;
            }
        }
        moved
    }
}

fn hex_float(s: &str) -> f64 {
    u32::from_str_radix(s.trim(), 16).map(|b| f32::from_bits(b) as f64).unwrap_or(0.0)
}

fn calibrated(raw: f64, src: &MeterSource, mode: &impl Fn(u16) -> f64) -> Option<f64> {
    let raw = if src.integer { (raw as f32).to_bits() as i32 as f64 } else { raw };
    if src.ignore_zero && raw == 0.0 { return None; }
    let raw = if src.absolute { raw.abs() } else { raw };
    Some(MeterSource::calibrate(raw, src.curve(src.mode_param.map(mode))))
}
