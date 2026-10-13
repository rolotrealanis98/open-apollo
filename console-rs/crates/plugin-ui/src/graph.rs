//! Graph elements and the GraphValue expression language, ported from the Swift runtime
//! (`PluginSpec.swift` graph math + `SpecGraphScene` in `SpecPluginView.swift`).
//!
//! Everything here is pure: [`GraphValue::resolve`] evaluates a value against a [`Lookup`]
//! (parameter values and conditioned raw meter values), [`band_db`]/[`curve_db`]/[`transfer`] are
//! the analog-prototype magnitudes, [`Axes`] maps values to element-local points, and
//! [`Scene::build`] turns a graph and the current values into fills, strokes, overlays, labels
//! and handles in element-local points. [`crate::raster`] draws a scene.
//!
//! Differences from Swift, all where Swift produces NaN or an unintended shape:
//! - `qMode: "3dB"` bells below 3 dB of gain: Swift's `max(sqrt(negative), 1)` is NaN (and the
//!   whole curve then falls to −1000 dB); here, as in the Python twin, the divisor is 1.
//! - Fills: Swift's `Path.addLines` starts a new subpath for the closing points, so a fill is the
//!   union of the curve's chord polygon and the closing polygon; here (as in the Python twin and
//!   the original plug-ins) the curve and its closing points form one polygon.

use std::collections::BTreeSet;

use houston::Rgba;

use crate::spec::{
    GradientStop, Graph, GraphBand, GraphCurve, GraphKind, GraphLaw, GraphPeakMarker, GraphPolyline, GraphSpectrum, GraphTrace, GraphValue,
    TouchState, key_distance,
};

/// Values a graph value reads: parameter values (0…1) and conditioned raw meter values.
pub trait Lookup {
    fn value(&self, k: u16) -> f64;
    /// The meter's conditioned raw value (after integer/absolute/derive), None before any reply.
    fn meter(&self, id: &str) -> Option<f64>;
}

/// A lookup from closures (tests and tools).
pub struct FnLookup<V: Fn(u16) -> f64, M: Fn(&str) -> Option<f64>>(pub V, pub M);

impl<V: Fn(u16) -> f64, M: Fn(&str) -> Option<f64>> Lookup for FnLookup<V, M> {
    fn value(&self, k: u16) -> f64 {
        (self.0)(k)
    }
    fn meter(&self, id: &str) -> Option<f64> {
        (self.1)(id)
    }
}

/// Parameter values only (every meter reads as absent, i.e. 0).
pub fn params_only(value: impl Fn(u16) -> f64) -> FnLookup<impl Fn(u16) -> f64, impl Fn(&str) -> Option<f64>> {
    FnLookup(value, |_: &str| None)
}

/// Swift's `Int(x.rounded())` clamped into `0..n`: the step index of a normalized value.
fn step_of(v: f64, n: usize) -> usize {
    if n > 1 {
        let i = (v * (n - 1) as f64).round();
        if i.is_nan() { 0 } else { (i.max(0.0) as usize).min(n - 1) }
    } else {
        0
    }
}

// MARK: - Laws

impl GraphLaw {
    /// Normalized 0…1 → real units.
    pub fn map(&self, normalized: f64) -> f64 {
        let v = normalized.clamp(0.0, 1.0);
        let v = if v.is_nan() { 0.0 } else { v };
        let (lo, hi) = (self.min.unwrap_or(0.0), self.max.unwrap_or(1.0));
        match self.kind.as_str() {
            "linear" => lo + (hi - lo) * v,
            "log" => {
                if lo > 0.0 && hi > 0.0 {
                    lo * (hi / lo).powf(v)
                } else {
                    lo + (hi - lo) * v
                }
            }
            "power" => lo + (hi - lo) * v.powf(self.power.unwrap_or(1.0)),
            "bipolar" => {
                let u = 2.0 * v - 1.0;
                let shaped = u.abs().powf(self.power.unwrap_or(1.0)) * if u < 0.0 { -1.0 } else { 1.0 };
                (lo + hi) / 2.0 + (hi - lo) / 2.0 * shaped
            }
            "steps" => match &self.values {
                Some(vals) if !vals.is_empty() => vals[step_of(v, vals.len())],
                _ => v,
            },
            "table" => {
                let pts: Vec<&Vec<f64>> = self.points.iter().flatten().filter(|p| p.len() >= 2).collect();
                let (Some(first), Some(last)) = (pts.first(), pts.last()) else { return v };
                if v <= first[0] {
                    return first[1];
                }
                if v >= last[0] {
                    return last[1];
                }
                for w in pts.windows(2) {
                    let (a, b) = (w[0], w[1]);
                    if v >= a[0] && v <= b[0] {
                        let t = if b[0] == a[0] { 0.0 } else { (v - a[0]) / (b[0] - a[0]) };
                        if self.interp.as_deref() == Some("log") && a[1] > 0.0 && b[1] > 0.0 {
                            return a[1] * (b[1] / a[1]).powf(t);
                        }
                        return a[1] + (b[1] - a[1]) * t;
                    }
                }
                last[1]
            }
            _ => v,
        }
    }

    /// Real units → normalized (handle drags): closed form for linear/log, nearest step for
    /// steps, else the nearest of 1001 samples.
    pub fn normalized_for(&self, x: f64) -> f64 {
        let (lo, hi) = (self.min.unwrap_or(0.0), self.max.unwrap_or(1.0));
        match self.kind.as_str() {
            "linear" if hi != lo => ((x - lo) / (hi - lo)).clamp(0.0, 1.0),
            "log" if lo > 0.0 && hi > 0.0 && hi != lo => ((x.max(1e-12) / lo).ln() / (hi / lo).ln()).clamp(0.0, 1.0),
            "steps" => match &self.values {
                Some(vals) if vals.len() > 1 => {
                    let mut best = 0;
                    for (i, v) in vals.iter().enumerate() {
                        if (v - x).abs() < (vals[best] - x).abs() {
                            best = i;
                        }
                    }
                    best as f64 / (vals.len() - 1) as f64
                }
                _ => 0.0,
            },
            _ => {
                let (mut best, mut distance) = (0.0, f64::INFINITY);
                for i in 0..=1000 {
                    let v = i as f64 / 1000.0;
                    let d = (self.map(v) - x).abs();
                    if d < distance {
                        best = v;
                        distance = d;
                    }
                }
                best
            }
        }
    }
}

// MARK: - Values

impl GraphValue {
    /// Evaluates the value (Swift's `resolve`, same precedence: constant, sum, min, ratio, apply,
    /// max, product, select, table, meter, param).
    pub fn resolve(&self, ctx: &dyn Lookup) -> f64 {
        if let Some(c) = self.constant {
            return c;
        }
        if let Some(sum) = &self.sum {
            return sum.iter().map(|v| v.resolve(ctx)).sum();
        }
        if let Some(list) = self.min_of.as_ref().filter(|l| !l.is_empty()) {
            return list[1..].iter().map(|v| v.resolve(ctx)).fold(list[0].resolve(ctx), swift_min);
        }
        if let Some(r) = self.ratio.as_ref().filter(|r| r.len() == 2) {
            let d = r[1].resolve(ctx);
            return if d.abs() < 1e-12 { 0.0 } else { r[0].resolve(ctx) / d };
        }
        if let Some(v) = self.apply.as_ref().and_then(|a| a.first()) {
            let x = v.resolve(ctx);
            let mapped = self.law.as_ref().map(|l| l.map(x)).unwrap_or(x);
            return mapped * self.scale.unwrap_or(1.0) + self.offset.unwrap_or(0.0);
        }
        if let Some(list) = self.max_of.as_ref().filter(|l| !l.is_empty()) {
            return list[1..].iter().map(|v| v.resolve(ctx)).fold(list[0].resolve(ctx), swift_max);
        }
        if let Some(product) = &self.product {
            return product.iter().map(|v| v.resolve(ctx)).product();
        }
        if let Some(sel) = self.select.as_ref().filter(|s| !s.cases.is_empty()) {
            let i = step_of(ctx.value(sel.param), sel.cases.len());
            return sel.cases[i].resolve(ctx);
        }
        if let (Some(params), Some(rows)) = (&self.params, self.table.as_ref().filter(|t| !t.is_empty())) {
            let key: Vec<f64> = params.iter().map(|&p| ctx.value(p)).collect();
            let mut best = &rows[0];
            for r in &rows[1..] {
                if key_distance(&r.key, &key) < key_distance(&best.key, &key) {
                    best = r;
                }
            }
            return best.value;
        }
        if let Some(m) = &self.meter {
            let raw = ctx.meter(m).unwrap_or(0.0);
            let level = self.db.map(|db| db * raw.abs().max(1e-12).log10()).unwrap_or(raw);
            return level * self.scale.unwrap_or(1.0) + self.offset.unwrap_or(0.0);
        }
        let Some(p) = self.param else { return 0.0 };
        let v = ctx.value(p);
        let mapped = self.law.as_ref().map(|l| l.map(v)).unwrap_or(v);
        mapped * self.scale.unwrap_or(1.0) + self.offset.unwrap_or(0.0)
    }

    /// The normalized parameter value that makes this value equal `x`; None unless this is a
    /// single parameter (through an optional law, scale and offset).
    pub fn normalized_for(&self, x: f64) -> Option<f64> {
        if self.constant.is_some()
            || self.table.is_some()
            || self.sum.is_some()
            || self.product.is_some()
            || self.select.is_some()
            || self.min_of.is_some()
            || self.max_of.is_some()
            || self.ratio.is_some()
            || self.apply.is_some()
            || self.meter.is_some()
            || self.param.is_none()
        {
            return None;
        }
        let scale = self.scale.unwrap_or(1.0);
        let unscaled = (x - self.offset.unwrap_or(0.0)) / if scale == 0.0 { 1.0 } else { scale };
        Some(self.law.as_ref().map(|l| l.normalized_for(unscaled)).unwrap_or_else(|| unscaled.clamp(0.0, 1.0)))
    }

    /// ≥ 0.5 counts as on; an absent value is on.
    pub fn is_on(v: Option<&GraphValue>, ctx: &dyn Lookup) -> bool {
        v.is_none_or(|v| v.resolve(ctx) >= 0.5)
    }
}

/// Swift's `Array.min()` step: a strictly smaller value replaces (so a leading NaN sticks).
fn swift_min(a: f64, b: f64) -> f64 {
    if b < a { b } else { a }
}

fn swift_max(a: f64, b: f64) -> f64 {
    if b > a { b } else { a }
}

/// Meter ids any graph value of the element reads.
pub fn graph_meters(g: &Graph) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut add = |v: &Option<GraphValue>| {
        if let Some(v) = v {
            v.meters(&mut out);
        }
    };
    for b in g.bands.iter().flatten() {
        for v in [&b.freq, &b.gain, &b.q, &b.bandwidth, &b.order, &b.slope, &b.enable, &b.shelf_order] {
            add(v);
        }
    }
    for c in g.curves.iter().flatten() {
        for v in [&c.gain, &c.fill_to_gain, &c.offset, &c.visible] {
            add(v);
        }
    }
    for h in g.handles.iter().flatten() {
        for v in [&h.x, &h.y, &h.visible] {
            add(v);
        }
    }
    for t in g.traces.iter().flatten() {
        for v in [&t.pattern, &t.pattern_scale, &t.pattern_offset, &t.rotation, &t.flip, &t.opacity, &t.threshold, &t.ratio, &t.range, &t.knee, &t.makeup, &t.enable, &t.visible, &t.amplitude, &t.arc_from, &t.arc_to] {
            add(v);
        }
    }
    for m in g.markers.iter().flatten() {
        add(&Some(m.angle.clone()));
        add(&m.flip);
        add(&m.visible);
    }
    for r in g.ranges.iter().flatten() {
        add(&Some(r.from.clone()));
        add(&Some(r.to.clone()));
        add(&r.y_value);
        add(&r.visible);
    }
    for p in g.polylines.iter().flatten() {
        for pt in &p.points {
            add(&Some(pt.y.clone()));
        }
        add(&p.visible);
    }
    if let Some(s) = &g.spectrum {
        add(&s.visible);
    }
    if let Some(pm) = &g.peak_marker {
        add(&Some(pm.from.clone()));
        add(&Some(pm.to.clone()));
        add(&pm.top);
    }
    out
}

// MARK: - Filter math (analog prototypes, RBJ cookbook s-domain forms)

#[derive(Clone, Copy, Debug, PartialEq)]
struct Complex {
    re: f64,
    im: f64,
}

impl Complex {
    const ONE: Complex = Complex { re: 1.0, im: 0.0 };
    fn new(re: f64, im: f64) -> Self {
        Self { re, im }
    }
    fn real(re: f64) -> Self {
        Self { re, im: 0.0 }
    }
    fn add(self, o: Complex) -> Complex {
        Complex::new(self.re + o.re, self.im + o.im)
    }
    fn mul(self, o: Complex) -> Complex {
        Complex::new(self.re * o.re - self.im * o.im, self.re * o.im + self.im * o.re)
    }
    fn scale(self, k: f64) -> Complex {
        Complex::new(self.re * k, self.im * k)
    }
    fn div(self, o: Complex) -> Complex {
        let d = o.re * o.re + o.im * o.im;
        if !(d > 0.0) {
            return Complex::real(1e12);
        }
        Complex::new((self.re * o.re + self.im * o.im) / d, (self.im * o.re - self.re * o.im) / d)
    }
    fn magnitude(self) -> f64 {
        (self.re * self.re + self.im * self.im).sqrt()
    }
}

/// Butterworth section Qs for an order-n filter, plus whether a first-order section is needed.
pub fn butterworth_qs(order: i64) -> (Vec<f64>, bool) {
    if order <= 1 {
        return (vec![], order == 1);
    }
    let n = order as f64;
    if order % 2 == 0 {
        return ((0..order / 2).map(|k| 1.0 / (2.0 * (std::f64::consts::PI * (2 * k + 1) as f64 / (2.0 * n)).cos())).collect(), false);
    }
    ((1..=(order - 1) / 2).map(|k| 1.0 / (2.0 * (std::f64::consts::PI * k as f64 / n).cos())).collect(), true)
}

/// The band's filter type: the stepped `filterParam` choice, else `filter`, else `bell`.
pub fn filter_kind(b: &GraphBand, ctx: &dyn Lookup) -> String {
    if let Some(sel) = b.filter_param.as_ref().filter(|s| !s.values.is_empty()) {
        return sel.values[step_of(ctx.value(sel.param), sel.values.len())].clone();
    }
    b.filter.clone().unwrap_or_else(|| "bell".into())
}

/// Analog-prototype magnitude (dB) of one band at `f` Hz.
pub fn band_db(b: &GraphBand, f: f64, gain_override: Option<f64>, ctx: &dyn Lookup) -> f64 {
    let mut kind = filter_kind(b, ctx);
    if kind.ends_with("Shelf") {
        if let Some(so) = b.shelf_order.as_ref().map(|v| v.resolve(ctx)) {
            if so < 1.5 {
                kind.push('1');
            }
        }
    }
    let mut f0 = b.freq.as_ref().map(|v| v.resolve(ctx)).unwrap_or(1000.0).max(1e-6);
    let gain = gain_override.unwrap_or_else(|| b.gain.as_ref().map(|v| v.resolve(ctx)).unwrap_or(0.0));
    if kind == "gain" {
        return gain;
    }
    let mut q = b.q.as_ref().map(|v| v.resolve(ctx));
    if q.is_none() {
        if let Some(bw) = b.bandwidth.as_ref().map(|v| v.resolve(ctx)).filter(|&bw| bw > 0.0) {
            let p = 2f64.powf(bw);
            q = Some(p.sqrt() / (p - 1.0));
        }
    }
    if let Some(sq) = b.shelf_q {
        if kind.ends_with("Shelf") {
            q = Some(sq);
        }
    }
    let mut qq = q.unwrap_or(0.7071).max(0.01);
    let mut order: i64 = 2;
    if let Some(o) = b.order.as_ref().map(|v| v.resolve(ctx)) {
        order = (o.round() as i64).max(0);
    } else if let Some(sl) = b.slope.as_ref().map(|v| v.resolve(ctx)) {
        order = ((sl / 6.0).round() as i64).max(0);
    }
    let a = 10f64.powf(gain / 40.0);
    let big = a.max(1.0 / a);
    let q_mode = b.q_mode.as_deref();
    if q_mode == Some("peak") && kind == "bell" {
        qq /= big;
    }
    if q_mode == Some("3dB") && kind == "bell" {
        // Q measured 3 dB below the peak; the cookbook Q is Q / sqrt(A² − 2/A²) from ~3 dB of gain
        // (below, the radicand is negative: RBJ Q, as the Python twin does).
        qq /= (big * big - 2.0 / (big * big)).max(0.0).sqrt().max(1.0);
    }
    if let Some(shift) = b.shelf_shift.or(if q_mode == Some("peak") { Some(0.5) } else { None }) {
        if kind == "lowShelf" {
            f0 *= big.powf(shift);
        }
        if kind == "highShelf" {
            f0 /= big.powf(shift);
        }
    }
    let mut s = Complex::new(0.0, f.max(1e-6) / f0);
    let one = Complex::ONE;
    let h = match kind.as_str() {
        "bell" => s.mul(s).add(s.scale(a / qq)).add(one).div(s.mul(s).add(s.scale(1.0 / (a * qq))).add(one)),
        "lowShelf" => {
            let sa = a.sqrt();
            s.mul(s).add(s.scale(sa / qq)).add(Complex::real(a)).div(s.mul(s).scale(a).add(s.scale(sa / qq)).add(one)).scale(a)
        }
        "highShelf" => {
            let sa = a.sqrt();
            s.mul(s).scale(a).add(s.scale(sa / qq)).add(one).div(s.mul(s).add(s.scale(sa / qq)).add(Complex::real(a))).scale(a)
        }
        "lowShelf1" => s.add(Complex::real(a)).div(s.add(Complex::real(1.0 / a))),
        "highShelf1" => s.scale(a).add(one).div(s.scale(1.0 / a).add(one)),
        "notch" => s.mul(s).add(one).div(s.mul(s).add(s.scale(1.0 / qq)).add(one)),
        "bandPass" => s.scale(1.0 / qq).div(s.mul(s).add(s.scale(1.0 / qq)).add(one)),
        "highPass" | "lowPass" => {
            if order == 0 {
                return 0.0; // "FLAT": drawn flat
            }
            if kind == "lowPass" {
                s = one.div(s);
            }
            let shape = b.shape.as_deref().unwrap_or("butterworth");
            if shape == "rbj" {
                s.mul(s).div(s.mul(s).add(s.scale(1.0 / qq)).add(one))
            } else {
                let lr = shape == "linkwitzRiley";
                let section_order = if lr { (order / 2).max(1) } else { order };
                let (qs, first_order) = butterworth_qs(section_order);
                let mut h = one;
                for sq in qs {
                    h = h.mul(s.mul(s).div(s.mul(s).add(s.scale(1.0 / sq)).add(one)));
                }
                if first_order {
                    h = h.mul(s.div(s.add(one)));
                }
                if lr { h.mul(h) } else { h }
            }
        }
        _ => return 0.0,
    };
    20.0 * h.magnitude().max(1e-12).log10()
}

/// Sum of the bands (all when `ids` is None) at `f` (dB).
pub fn bands_db(g: &Graph, ids: Option<&[String]>, f: f64, gain: Option<f64>, include_disabled: bool, ctx: &dyn Lookup) -> f64 {
    let mut total = 0.0;
    for b in g.bands.iter().flatten() {
        if ids.is_some_and(|ids| !ids.contains(&b.id)) {
            continue;
        }
        if !include_disabled && !GraphValue::is_on(b.enable.as_ref(), ctx) {
            continue;
        }
        total += band_db(b, f, gain, ctx);
    }
    total
}

/// A curve's response at `f` (dB): its bands in series, or its groups in parallel (linear
/// magnitudes summed in phase).
pub fn curve_db(g: &Graph, c: &GraphCurve, f: f64, gain: Option<f64>, ctx: &dyn Lookup) -> f64 {
    let include = c.include_disabled.unwrap_or(false);
    match c.groups.as_ref().filter(|g| !g.is_empty()) {
        None => bands_db(g, c.bands.as_deref(), f, gain, include, ctx),
        Some(groups) => {
            let sum: f64 = groups.iter().map(|grp| 10f64.powf(bands_db(g, Some(grp), f, gain, include, ctx) / 20.0)).sum();
            20.0 * sum.max(1e-12).log10()
        }
    }
}

/// Output level of one dynamics trace for input `x` dB.
pub fn transfer(t: &GraphTrace, x: f64, ctx: &dyn Lookup) -> f64 {
    let r = |v: &Option<GraphValue>| v.as_ref().map(|v| v.resolve(ctx));
    let threshold = r(&t.threshold).unwrap_or(0.0);
    let ratio = r(&t.ratio).unwrap_or(1.0);
    let range = r(&t.range);
    let knee = r(&t.knee).unwrap_or(0.0).max(0.0);
    let makeup = r(&t.makeup).unwrap_or(0.0);
    let kind = t.kind.as_deref().unwrap_or("compressor");
    let mut y = x;
    match kind {
        "compressor" | "limiter" => {
            let slope = if kind == "limiter" { 0.0 } else { 1.0 / ratio.max(1e-6) };
            let e = x - threshold + knee / 2.0;
            if knee > 0.0 && e > 0.0 && e < knee {
                y = x + (slope - 1.0) * e * e / (2.0 * knee);
            } else if x > threshold {
                y = threshold + (x - threshold) * slope;
            }
        }
        "expander" => {
            let d = threshold + knee / 2.0 - x;
            if knee > 0.0 && d > 0.0 && d < knee {
                y = x - (ratio - 1.0) * d * d / (2.0 * knee);
            } else if x < threshold {
                y = threshold + (x - threshold) * ratio;
            }
            if let Some(range) = range {
                y = y.max(x - range);
            }
        }
        "gate" => {
            if x < threshold {
                y = x - range.unwrap_or(200.0);
            }
        }
        _ => {}
    }
    y + makeup
}

// MARK: - Axes

/// Element-local mapping between values and points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Axes {
    pub x_min: f64,
    pub x_max: f64,
    pub x_log: bool,
    pub x0: f64,
    pub x1: f64,
    pub y_min: f64,
    pub y_max: f64,
    pub y_log: bool,
    pub y0: f64,
    pub y1: f64,
}

impl Axes {
    pub fn new(g: &Graph, w: f64, h: f64) -> Self {
        let (xa, ya) = (g.x, g.y);
        let x_min = xa.map(|a| a.min).unwrap_or(20.0);
        let x_max = xa.map(|a| a.max).unwrap_or(20000.0);
        let y_min = ya.map(|a| a.min).unwrap_or(-20.0);
        let y_max = ya.map(|a| a.max).unwrap_or(20.0);
        Self {
            x_min,
            x_max,
            x_log: xa.and_then(|a| a.log).unwrap_or(g.kind == GraphKind::Eq) && x_min > 0.0 && x_max > 0.0,
            x0: xa.and_then(|a| a.from).unwrap_or(0.0),
            x1: xa.and_then(|a| a.to).unwrap_or(w),
            y_min,
            y_max,
            y_log: ya.and_then(|a| a.log).unwrap_or(false) && y_min > 0.0 && y_max > 0.0,
            y0: ya.and_then(|a| a.from).unwrap_or(h),
            y1: ya.and_then(|a| a.to).unwrap_or(0.0),
        }
    }

    fn fraction(v: f64, lo: f64, hi: f64, log: bool) -> f64 {
        if log {
            return (v.max(1e-12) / lo).ln() / (hi / lo).ln();
        }
        if hi == lo { 0.0 } else { (v - lo) / (hi - lo) }
    }

    fn value(t: f64, lo: f64, hi: f64, log: bool) -> f64 {
        if log { lo * (hi / lo).powf(t) } else { lo + (hi - lo) * t }
    }

    pub fn px(&self, v: f64) -> f64 {
        self.x0 + (self.x1 - self.x0) * Self::fraction(v, self.x_min, self.x_max, self.x_log)
    }

    pub fn py(&self, v: f64) -> f64 {
        self.y0 + (self.y1 - self.y0) * Self::fraction(v, self.y_min, self.y_max, self.y_log)
    }

    pub fn x_value(&self, x: f64) -> f64 {
        Self::value(if self.x1 == self.x0 { 0.0 } else { (x - self.x0) / (self.x1 - self.x0) }, self.x_min, self.x_max, self.x_log)
    }

    pub fn y_value(&self, y: f64) -> f64 {
        Self::value(if self.y1 == self.y0 { 0.0 } else { (y - self.y0) / (self.y1 - self.y0) }, self.y_min, self.y_max, self.y_log)
    }

    pub fn clamp_y(&self, v: f64) -> f64 {
        v.max(self.y_min.min(self.y_max)).min(self.y_min.max(self.y_max))
    }
}

/// Keeps far-off-scale responses (deep filter skirts) finite: ±1000, non-finite → −1000.
pub fn limit(db: f64) -> f64 {
    if db.is_finite() { db.clamp(-1000.0, 1000.0) } else { -1000.0 }
}

/// IEEE 754 half float.
pub fn half(h: u16) -> f64 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let e = ((h >> 10) & 0x1f) as i32;
    let m = (h & 0x3ff) as f64;
    if e == 0 {
        return sign * m / 1024.0 * 2f64.powi(-14);
    }
    if e == 31 {
        return if m == 0.0 { sign * f64::INFINITY } else { f64::NAN };
    }
    sign * (1.0 + m / 1024.0) * 2f64.powi(e - 15)
}

/// The smallest positive Float (Swift's `Float.leastNonzeroMagnitude`), the spectrum's log floor.
pub const LEAST_FLOAT: f64 = 1.401298464324817e-45;

/// `#RRGGBB` / `#RRGGBBAA` (surrounding spaces allowed); None when absent or malformed.
pub fn parse_hex(hex: Option<&str>) -> Option<Rgba> {
    let h = hex?.trim().strip_prefix('#')?;
    if (h.len() != 6 && h.len() != 8) || !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let v = u32::from_str_radix(h, 16).ok()?;
    let (rgb, a) = if h.len() == 8 { (v >> 8, (v & 0xff) as f32 / 255.0) } else { (v, 1.0) };
    Some(Rgba::rgb8((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8).with_alpha(a))
}

// MARK: - Scene

/// A path segment in element-local points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Seg {
    Move(f64, f64),
    Line(f64, f64),
    /// Control points then end point.
    Cubic([f64; 6]),
    Close,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Path {
    pub segs: Vec<Seg>,
}

impl Path {
    pub fn polyline(points: &[(f64, f64)]) -> Self {
        let mut segs = Vec::with_capacity(points.len());
        for (i, &(x, y)) in points.iter().enumerate() {
            segs.push(if i == 0 { Seg::Move(x, y) } else { Seg::Line(x, y) });
        }
        Self { segs }
    }

    /// Straight segments, or a Catmull-Rom curve through the points (`smooth` with > 2 points).
    pub fn curve(points: &[(f64, f64)], smooth: bool) -> Self {
        if !smooth || points.len() <= 2 {
            return Self::polyline(points);
        }
        let n = points.len();
        let mut segs = vec![Seg::Move(points[0].0, points[0].1)];
        for i in 0..n - 1 {
            let p0 = points[i.saturating_sub(1)];
            let (p1, p2) = (points[i], points[i + 1]);
            let p3 = points[(i + 2).min(n - 1)];
            let c1 = (p1.0 + (p2.0 - p0.0) / 6.0, p1.1 + (p2.1 - p0.1) / 6.0);
            let c2 = (p2.0 - (p3.0 - p1.0) / 6.0, p2.1 - (p3.1 - p1.1) / 6.0);
            segs.push(Seg::Cubic([c1.0, c1.1, c2.0, c2.1, p2.0, p2.1]));
        }
        Self { segs }
    }

    pub fn rect(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { segs: vec![Seg::Move(x, y), Seg::Line(x + w, y), Seg::Line(x + w, y + h), Seg::Line(x, y + h), Seg::Close] }
    }

    /// An ellipse inscribed in the rect (four cubic arcs).
    pub fn ellipse(x: f64, y: f64, w: f64, h: f64) -> Self {
        let (rx, ry) = (w / 2.0, h / 2.0);
        let (cx, cy) = (x + rx, y + ry);
        let k = 0.552_284_749_830_793_6;
        let (ox, oy) = (rx * k, ry * k);
        Self {
            segs: vec![
                Seg::Move(cx + rx, cy),
                Seg::Cubic([cx + rx, cy + oy, cx + ox, cy + ry, cx, cy + ry]),
                Seg::Cubic([cx - ox, cy + ry, cx - rx, cy + oy, cx - rx, cy]),
                Seg::Cubic([cx - rx, cy - oy, cx - ox, cy - ry, cx, cy - ry]),
                Seg::Cubic([cx + ox, cy - ry, cx + rx, cy - oy, cx + rx, cy]),
                Seg::Close,
            ],
        }
    }

    /// A rounded rect (corner radius clamped to half the shorter side).
    pub fn rounded_rect(x: f64, y: f64, w: f64, h: f64, r: f64) -> Self {
        let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
        if r <= 0.0 {
            return Self::rect(x, y, w, h);
        }
        let k = r * (1.0 - 0.552_284_749_830_793_6);
        Self {
            segs: vec![
                Seg::Move(x + r, y),
                Seg::Line(x + w - r, y),
                Seg::Cubic([x + w - k, y, x + w, y + k, x + w, y + r]),
                Seg::Line(x + w, y + h - r),
                Seg::Cubic([x + w, y + h - k, x + w - k, y + h, x + w - r, y + h]),
                Seg::Line(x + r, y + h),
                Seg::Cubic([x + k, y + h, x, y + h - k, x, y + h - r]),
                Seg::Line(x, y + r),
                Seg::Cubic([x, y + k, x + k, y, x + r, y]),
                Seg::Close,
            ],
        }
    }

    pub fn is_empty(&self) -> bool {
        self.segs.is_empty()
    }

    /// Every coordinate (for change detection).
    pub fn coords(&self) -> impl Iterator<Item = f64> + '_ {
        self.segs.iter().flat_map(|s| match *s {
            Seg::Move(x, y) | Seg::Line(x, y) => vec![x, y],
            Seg::Cubic(c) => c.to_vec(),
            Seg::Close => vec![f64::NAN],
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Paint {
    Solid(Rgba),
    /// Vertical gradient over the graph's height: (location 0…1, colour), ascending.
    Vertical(Vec<(f64, Rgba)>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct FillItem {
    pub path: Path,
    pub paint: Paint,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StrokeItem {
    pub path: Path,
    pub color: Rgba,
    pub width: f64,
}

/// Text drawn above the curves: its left edge at `x`, `y` the top of the line box (SwiftUI's
/// `.offset` of a `fixedSize` text in a top-leading stack).
#[derive(Clone, Debug, PartialEq)]
pub struct Label {
    pub text: String,
    pub x: f64,
    pub y: f64,
    pub color: Rgba,
    pub size: f64,
}

/// A handle dot: drawn above everything, draggable when `draggable`.
#[derive(Clone, Debug, PartialEq)]
pub struct HandleSpot {
    pub x: f64,
    pub y: f64,
    pub radius: f64,
    pub stroke_width: f64,
    pub fill: Rgba,
    pub stroke: Option<Rgba>,
    pub square: bool,
    pub label: Option<String>,
    pub label_color: Rgba,
    pub label_size: f64,
    pub x_value: Option<GraphValue>,
    pub y_value: Option<GraphValue>,
    pub draggable: bool,
    pub touch_state: Option<TouchState>,
}

impl HandleSpot {
    /// The finger-sized hit diameter around small dots.
    pub fn hit_diameter(&self) -> f64 {
        (self.radius * 2.0).max(22.0)
    }
}

/// Paths and colours of one graph state, in element-local points.
#[derive(Clone, Debug, PartialEq)]
pub struct Scene {
    pub axes: Axes,
    /// Curves, fills and traces are clipped to this rect (x, y, w, h).
    pub clip: (f64, f64, f64, f64),
    pub fills: Vec<FillItem>,
    pub strokes: Vec<StrokeItem>,
    /// Drawn above the clipped curves (polar rim markers, ranges, the peak line).
    pub overlays: Vec<FillItem>,
    pub labels: Vec<Label>,
    pub handles: Vec<HandleSpot>,
    /// Decoded spectrum points (hz, level before the floor clamp), for the peak marker.
    pub spectrum_points: Vec<(f64, f64)>,
    pub width: f64,
    pub height: f64,
}

/// Graph values plus the analyser block (latest opcode value by variable id).
pub trait SceneInput: Lookup {
    fn block(&self, id: i64) -> Option<f64>;
}

struct NoBlock<'a>(&'a dyn Lookup);

impl Lookup for NoBlock<'_> {
    fn value(&self, k: u16) -> f64 {
        self.0.value(k)
    }
    fn meter(&self, id: &str) -> Option<f64> {
        self.0.meter(id)
    }
}

impl SceneInput for NoBlock<'_> {
    fn block(&self, _: i64) -> Option<f64> {
        None
    }
}

fn on(v: Option<&GraphValue>, ctx: &dyn Lookup) -> bool {
    GraphValue::is_on(v, ctx)
}

impl Scene {
    /// A scene without analyser data.
    pub fn build_values(g: &Graph, w: f64, h: f64, ctx: &dyn Lookup) -> Self {
        Self::build(g, w, h, &NoBlock(ctx))
    }

    pub fn build(g: &Graph, w: f64, h: f64, input: &dyn SceneInput) -> Self {
        let axes = Axes::new(g, w, h);
        let clip = g.clip.map(|c| (c.x, c.y, c.w, c.h)).unwrap_or((0.0, 0.0, w, h));
        let mut s = Scene {
            axes,
            clip,
            fills: vec![],
            strokes: vec![],
            overlays: vec![],
            labels: vec![],
            handles: vec![],
            spectrum_points: vec![],
            width: w,
            height: h,
        };
        let ctx: &dyn Lookup = &NoBlock(input);
        match g.kind {
            GraphKind::Polar => s.build_polar(g, ctx),
            GraphKind::Transfer => s.build_transfer(g, ctx),
            _ => {
                s.build_eq(g, ctx);
                if let Some(sp) = &g.spectrum {
                    if on(sp.visible.as_ref(), ctx) {
                        s.build_spectrum(sp, input);
                        if let Some(pm) = &g.peak_marker {
                            s.build_peak_marker(pm, ctx);
                        }
                    }
                }
                s.build_polylines(g.polylines.as_deref().unwrap_or(&[]), ctx);
            }
        }
        s
    }

    fn gradient(&self, stops: Option<&[GradientStop]>, opacity: f64) -> Option<Paint> {
        let stops = stops?;
        if !(self.height > 0.0) || stops.is_empty() {
            return None;
        }
        let mut sorted: Vec<&GradientStop> = stops.iter().collect();
        sorted.sort_by(|a, b| a.y.total_cmp(&b.y));
        Some(Paint::Vertical(
            sorted
                .into_iter()
                .map(|st| {
                    let mut c = parse_hex(Some(&st.color)).unwrap_or(Rgba::BLACK.with_alpha(0.0));
                    c.a *= opacity as f32;
                    ((st.y / self.height).clamp(0.0, 1.0), c)
                })
                .collect(),
        ))
    }

    /// Swift's `addCurve`: a fill (closed by `closing`) and a stroke through `points`.
    #[allow(clippy::too_many_arguments)]
    fn add_curve(&mut self, points: &[(f64, f64)], closing: Option<&[(f64, f64)]>, fill: Option<&str>, stroke: Option<&str>, width: Option<f64>, opacity: f64, gradient: Option<&[GradientStop]>, smooth: bool) {
        let grad = self.gradient(gradient, opacity);
        let fill_color = parse_hex(fill);
        if let (true, Some(closing), false) = (grad.is_some() || fill_color.is_some(), closing, points.is_empty()) {
            let mut path = Path::curve(points, smooth);
            for &(x, y) in closing {
                path.segs.push(Seg::Line(x, y));
            }
            path.segs.push(Seg::Close);
            let paint = grad.unwrap_or_else(|| {
                let mut c = fill_color.unwrap_or(Rgba::BLACK.with_alpha(0.0));
                c.a *= opacity as f32;
                Paint::Solid(c)
            });
            self.fills.push(FillItem { path, paint });
        }
        if let (Some(mut color), true) = (parse_hex(stroke), points.len() > 1) {
            color.a *= opacity as f32;
            self.strokes.push(StrokeItem { path: Path::curve(points, smooth), color, width: width.unwrap_or(1.0) });
        }
    }

    fn build_eq(&mut self, g: &Graph, ctx: &dyn Lookup) {
        let axes = self.axes;
        let w = self.width;
        let n = ((w * 2.0) as i64).max(2) as usize;
        let xs: Vec<f64> = (0..n).map(|i| i as f64 * w / (n - 1) as f64).collect();
        let freqs: Vec<f64> = xs.iter().map(|&x| axes.x_value(x)).collect();
        for c in g.curves.iter().flatten() {
            if !on(c.visible.as_ref(), ctx) {
                continue;
            }
            let include = c.include_disabled.unwrap_or(false);
            if c.require_enabled == Some(true) {
                let active = g.bands.iter().flatten().any(|b| c.bands.as_ref().is_none_or(|ids| ids.contains(&b.id)) && (include || on(b.enable.as_ref(), ctx)));
                if !active {
                    continue;
                }
            }
            let gain = c.gain.as_ref().map(|v| v.resolve(ctx));
            let offset = c.offset.as_ref().map(|v| v.resolve(ctx)).unwrap_or(0.0);
            let mut points = Vec::with_capacity(n);
            let mut dbs = Vec::with_capacity(n);
            for (&x, &f) in xs.iter().zip(&freqs) {
                let db = curve_db(g, c, f, gain, ctx);
                dbs.push(db);
                points.push((x, axes.py(limit(db + offset))));
            }
            let mut closing: Option<Vec<(f64, f64)>> = None;
            if c.fill.is_some() {
                if let Some(other) = c.fill_to_gain.as_ref().map(|v| v.resolve(ctx)) {
                    let back = xs.iter().zip(&freqs).rev().map(|(&x, &f)| (x, axes.py(limit(curve_db(g, c, f, Some(other), ctx) + offset)))).collect();
                    closing = Some(back);
                } else {
                    let base = axes.py(c.fill_to.unwrap_or(0.0));
                    closing = Some(vec![(xs[n - 1], base), (xs[0], base)]);
                }
            }
            match (c.hide_below, parse_hex(c.stroke.as_deref())) {
                (Some(lim), Some(color)) => {
                    self.add_curve(&points, closing.as_deref(), c.fill.as_deref(), None, None, 1.0, None, false);
                    let mut path = Path::default();
                    let mut drawing = false;
                    for (&(x, y), &db) in points.iter().zip(&dbs) {
                        if db.abs() > lim {
                            path.segs.push(if drawing { Seg::Line(x, y) } else { Seg::Move(x, y) });
                            drawing = true;
                        } else {
                            drawing = false;
                        }
                    }
                    if !path.is_empty() {
                        self.strokes.push(StrokeItem { path, color, width: c.width.unwrap_or(1.0) });
                    }
                }
                _ => self.add_curve(&points, closing.as_deref(), c.fill.as_deref(), c.stroke.as_deref(), c.width, 1.0, None, false),
            }
        }
        for h in g.handles.iter().flatten() {
            if !on(h.visible.as_ref(), ctx) {
                continue;
            }
            let band = g.band(h.band.as_deref());
            let Some(f) = h.x.as_ref().map(|v| v.resolve(ctx)).or_else(|| band.and_then(|b| b.freq.as_ref()).map(|v| v.resolve(ctx))) else { continue };
            let mut db = if let Some(y) = &h.y {
                y.resolve(ctx)
            } else if h.y_mode.as_deref() == Some("curve") {
                bands_db(g, None, f, None, false, ctx)
            } else {
                band.and_then(|b| b.gain.as_ref()).map(|v| v.resolve(ctx)).unwrap_or(0.0)
            };
            if h.clamp.unwrap_or(true) {
                db = axes.clamp_y(db);
            }
            let (xv, yv) = g.drag_values(h);
            self.handles.push(handle_spot(h, axes.px(f), axes.py(limit(db)), xv.cloned(), yv.cloned(), h.touch_state.clone()));
        }
        for r in g.ranges.iter().flatten() {
            if !on(r.visible.as_ref(), ctx) {
                continue;
            }
            let (a, b) = (axes.px(r.from.resolve(ctx)), axes.px(r.to.resolve(ctx)));
            let lo = a.min(b).max(axes.x0.min(axes.x1));
            let hi = a.max(b).min(axes.x0.max(axes.x1));
            if !(hi >= lo) {
                continue;
            }
            let color = parse_hex(r.color.as_deref()).unwrap_or(Rgba::WHITE);
            let w = r.width.unwrap_or(1.0);
            let y = r.y_value.as_ref().map(|v| axes.py(limit(v.resolve(ctx)))).unwrap_or(r.y.unwrap_or(0.0));
            self.overlays.push(FillItem { path: Path::rect(lo, y - w / 2.0, hi - lo, w), paint: Paint::Solid(color) });
            if r.dots == Some(true) {
                let d = r.dot_radius.unwrap_or(2.5);
                for x in [lo, hi] {
                    self.overlays.push(FillItem { path: Path::ellipse(x - d, y - d, 2.0 * d, 2.0 * d), paint: Paint::Solid(color) });
                }
            }
        }
    }

    fn build_spectrum(&mut self, sp: &GraphSpectrum, input: &dyn SceneInput) {
        let axes = self.axes;
        let floor = sp.floor.unwrap_or(-1000.0);
        let (scale, offset) = (sp.scale.unwrap_or(1.0), sp.offset.unwrap_or(0.0));
        let mut points = Vec::new();
        let mut decoded = Vec::new();
        let level = |raw: f64| sp.db.map(|db| db * raw.abs().max(LEAST_FLOAT).log10()).unwrap_or(raw) * scale + offset;
        let mut plot = |raw: f64, hz: f64, points: &mut Vec<(f64, f64)>| {
            let unclamped = level(raw);
            decoded.push((hz, unclamped));
            points.push((axes.px(hz), axes.py(limit(unclamped.max(floor)))));
        };
        let count = sp.count.max(0);
        for i in 0..count {
            let id = if sp.reverse == Some(true) { sp.first + sp.count - 1 - i } else { sp.first + i };
            let Some(raw) = input.block(id) else { continue };
            if sp.decode.as_deref() == Some("fp16x2") {
                let bits = (raw as f32).to_bits();
                plot(half(bits as u16), sp.hz0 + (2 * i) as f64 * sp.hz_step / 2.0, &mut points);
                plot(half((bits >> 16) as u16), sp.hz0 + (2 * i + 1) as f64 * sp.hz_step / 2.0, &mut points);
                continue;
            }
            let hz = match &sp.hz {
                Some(list) => list.get(i as usize).or(list.last()).copied().unwrap_or(0.0),
                None => sp.hz0 + i as f64 * sp.hz_step,
            };
            plot(raw, hz, &mut points);
        }
        self.spectrum_points = decoded;
        if points.len() > 1 {
            let (first, last) = (points[0], points[points.len() - 1]);
            let base = axes.py(limit(floor));
            let closing = [(last.0, base), (first.0, base)];
            self.add_curve(&points, Some(&closing), sp.fill.as_deref(), sp.stroke.as_deref(), sp.width, 1.0, sp.fill_gradient.as_deref(), sp.smooth == Some(true));
        }
    }

    /// The loudest decoded point in [from, to] with 3-point parabolic refinement (linear
    /// amplitude), hidden at or below `min_db`.
    pub fn peak_frequency(points: &[(f64, f64)], from: f64, to: f64, min_db: f64) -> Option<f64> {
        let (lo, hi) = (from.min(to), from.max(to));
        let mut k: Option<usize> = None;
        for (i, p) in points.iter().enumerate() {
            if p.0 >= lo && p.0 <= hi && k.is_none_or(|k| p.1 > points[k].1) {
                k = Some(i);
            }
        }
        let k = k?;
        if !(points[k].1 > min_db) {
            return None;
        }
        let mut f = points[k].0;
        if k > 0 && k < points.len() - 1 {
            let a = |i: usize| 10f64.powf(points[i].1 / 20.0);
            let (a0, a1, a2) = (a(k - 1), a(k), a(k + 1));
            let den = a0 - 2.0 * a1 + a2;
            if den < 0.0 {
                let delta = (0.5 * (a0 - a2) / den).clamp(-1.0, 1.0);
                f += delta * if delta < 0.0 { points[k].0 - points[k - 1].0 } else { points[k + 1].0 - points[k].0 };
            }
        }
        Some(f)
    }

    fn build_peak_marker(&mut self, pm: &GraphPeakMarker, ctx: &dyn Lookup) {
        let axes = self.axes;
        let Some(f) = Self::peak_frequency(&self.spectrum_points, pm.from.resolve(ctx), pm.to.resolve(ctx), pm.min_db.unwrap_or(-1000.0)) else { return };
        let (x, w) = (axes.px(f), pm.width.unwrap_or(1.0));
        let y_top = pm.top.as_ref().map(|v| axes.py(limit(v.resolve(ctx)))).unwrap_or(0.0);
        let y_bottom = axes.py(axes.y_min).max(axes.py(axes.y_max));
        if y_bottom > y_top {
            // SwiftUI's `.red` (system red).
            let color = parse_hex(pm.stroke.as_deref()).unwrap_or(Rgba::rgb8(255, 59, 48));
            self.overlays.push(FillItem { path: Path::rect(x - w / 2.0, y_top, w, y_bottom - y_top), paint: Paint::Solid(color) });
        }
        if let Some(l) = &pm.label {
            self.labels.push(Label {
                text: crate::readout::format_number(l.format.as_deref().unwrap_or("%.0f Hz"), f),
                x: x + w / 2.0 + l.dx.unwrap_or(3.0),
                y: l.y,
                // SwiftUI's `.yellow` (system yellow).
                color: parse_hex(l.color.as_deref()).unwrap_or(Rgba::rgb8(255, 204, 0)),
                size: l.size.unwrap_or(10.0),
            });
        }
    }

    fn build_polylines(&mut self, lines: &[GraphPolyline], ctx: &dyn Lookup) {
        let axes = self.axes;
        for line in lines {
            if !on(line.visible.as_ref(), ctx) {
                continue;
            }
            let points: Vec<(f64, f64)> = line.points.iter().map(|p| (axes.px(p.x), axes.py(limit(p.y.resolve(ctx))))).collect();
            if points.len() < 2 {
                continue;
            }
            let (first, last) = (points[0], points[points.len() - 1]);
            let base = line.fill_to.map(|v| axes.py(limit(v))).unwrap_or(self.height);
            let closing = [(last.0, base), (first.0, base)];
            self.add_curve(&points, Some(&closing), line.fill.as_deref(), line.stroke.as_deref(), line.width, 1.0, line.fill_gradient.as_deref(), line.smooth == Some(true));
        }
    }

    fn build_polar(&mut self, g: &Graph, ctx: &dyn Lookup) {
        let Some(pol) = g.polar.as_ref().filter(|p| p.center.len() >= 2) else { return };
        let (cx, cy) = (pol.center[0], pol.center[1]);
        let (outer, inner, range) = (pol.radius, pol.inner_radius.unwrap_or(0.0), pol.db_range.unwrap_or(36.0));
        let point = |r: f64, ang: f64| (cx + r * ang.sin(), cy - r * ang.cos());
        for t in g.traces.iter().flatten() {
            if !on(t.visible.as_ref(), ctx) {
                continue;
            }
            let r = |v: &Option<GraphValue>| v.as_ref().map(|v| v.resolve(ctx));
            let opacity = r(&t.opacity).unwrap_or(1.0).clamp(0.0, 1.0);
            if !(opacity > 0.01) {
                continue;
            }
            let a = r(&t.pattern).unwrap_or(0.5) * r(&t.pattern_scale).unwrap_or(1.0) + r(&t.pattern_offset).unwrap_or(0.0);
            let mut rotation = r(&t.rotation).unwrap_or(0.0);
            if t.flip.is_some() && on(t.flip.as_ref(), ctx) {
                rotation += 180.0;
            }
            let amp = r(&t.amplitude).unwrap_or(1.0).max(0.0);
            match t.shape.as_deref() {
                Some("arc") => {
                    let radius = inner + (outer - inner) * amp;
                    let (a0, a1) = (r(&t.arc_from).unwrap_or(-135.0), r(&t.arc_to).unwrap_or(135.0));
                    if !(a1 > a0 + 0.01) {
                        continue;
                    }
                    let n = (((a1 - a0) / 2.0) as i64).max(1);
                    let pts: Vec<(f64, f64)> = (0..=n).map(|i| point(radius, (a0 + (a1 - a0) * i as f64 / n as f64).to_radians())).collect();
                    self.add_curve(&pts, None, None, t.stroke.as_deref(), t.width, opacity, None, false);
                }
                Some("lobe") => {
                    let len = (outer - inner) * amp;
                    let w = len * t.lobe_width.unwrap_or(0.2);
                    if !(len > 0.5) {
                        continue;
                    }
                    let mut dirs = vec![rotation];
                    match t.mirror.as_deref() {
                        Some("horizontal") => dirs.push(-rotation),
                        Some("opposite") => dirs.push(rotation + 180.0),
                        _ => {}
                    }
                    for deg in dirs {
                        let ang = deg.to_radians();
                        let pts: Vec<(f64, f64)> = (0..=72)
                            .map(|i| {
                                let s = i as f64 / 72.0 * 2.0 * std::f64::consts::PI;
                                let (u, v) = (len / 2.0 * (1.0 - s.cos()), w / 2.0 * s.sin());
                                (cx + u * ang.sin() + v * ang.cos(), cy - (u * ang.cos() - v * ang.sin()))
                            })
                            .collect();
                        self.add_curve(&pts, Some(&[]), t.fill.as_deref(), t.stroke.as_deref(), t.width, opacity, None, false);
                    }
                }
                _ => {
                    let rot = rotation.to_radians();
                    let pts: Vec<(f64, f64)> = (0..=360)
                        .map(|i| {
                            let theta = (i as f64).to_radians();
                            let m = (a + (1.0 - a) * theta.cos()).abs() * amp;
                            let rr = if pol.scale.as_deref() == Some("db") {
                                inner + (outer - inner) * (20.0 * m.max(1e-9).log10() + range).max(0.0) / range
                            } else {
                                inner + (outer - inner) * m
                            };
                            point(rr, theta + rot)
                        })
                        .collect();
                    self.add_curve(&pts, Some(&[]), t.fill.as_deref(), t.stroke.as_deref(), t.width, opacity, None, false);
                }
            }
        }
        for m in g.markers.iter().flatten() {
            if !on(m.visible.as_ref(), ctx) {
                continue;
            }
            let flipped = m.flip.is_some() && on(m.flip.as_ref(), ctx);
            let angle = (m.angle.resolve(ctx) + if flipped { 180.0 } else { 0.0 }).to_radians();
            let (r, s) = (m.radius.unwrap_or(outer), m.size.unwrap_or(6.0));
            let half = s / 2.0 / r.max(1.0);
            let pts = [point(r - s, angle), point(r, angle - half), point(r, angle + half)];
            let mut path = Path::polyline(&pts);
            path.segs.push(Seg::Close);
            self.overlays.push(FillItem { path, paint: Paint::Solid(parse_hex(m.color.as_deref()).unwrap_or(Rgba::WHITE)) });
        }
    }

    fn build_transfer(&mut self, g: &Graph, ctx: &dyn Lookup) {
        let axes = self.axes;
        let n = ((self.width * 2.0) as i64).max(2) as usize;
        let traces = g.traces.as_deref().unwrap_or(&[]);
        for t in traces {
            if !on(t.visible.as_ref(), ctx) {
                continue;
            }
            if t.chain.is_none() && t.enable.is_some() && !on(t.enable.as_ref(), ctx) {
                continue;
            }
            let pts: Vec<(f64, f64)> = (0..n)
                .map(|i| {
                    let input = axes.x_min + (axes.x_max - axes.x_min) * i as f64 / (n - 1) as f64;
                    let mut output = input;
                    match &t.chain {
                        Some(chain) => {
                            for id in chain {
                                if let Some(member) = traces.iter().find(|m| m.id.as_deref() == Some(id.as_str())) {
                                    if on(member.enable.as_ref(), ctx) {
                                        output = transfer(member, output, ctx);
                                    }
                                }
                            }
                        }
                        None => output = transfer(t, input, ctx),
                    }
                    (axes.px(input), axes.py(limit(output)))
                })
                .collect();
            self.add_curve(&pts, None, None, t.stroke.as_deref(), t.width, 1.0, None, false);
        }
        for h in g.handles.iter().flatten() {
            if !on(h.visible.as_ref(), ctx) {
                continue;
            }
            // Swift compares optionals: a handle without `trace` matches the first trace without `id`.
            let (Some(x), Some(t)) = (h.x.as_ref().map(|v| v.resolve(ctx)), traces.iter().find(|t| t.id == h.trace)) else { continue };
            let mut y = h.y.as_ref().map(|v| v.resolve(ctx)).unwrap_or_else(|| transfer(t, x, ctx));
            if h.clamp.unwrap_or(true) {
                y = axes.clamp_y(y);
            }
            // Swift passes no touch state (and no y value) for transfer handles.
            self.handles.push(handle_spot(h, axes.px(x), axes.py(limit(y)), h.x.clone(), None, None));
        }
    }
}

fn handle_spot(h: &crate::spec::GraphHandle, x: f64, y: f64, xv: Option<GraphValue>, yv: Option<GraphValue>, touch: Option<TouchState>) -> HandleSpot {
    HandleSpot {
        x,
        y,
        radius: h.radius.unwrap_or(4.0),
        stroke_width: h.stroke_width.unwrap_or(0.0),
        fill: parse_hex(h.fill.as_deref()).unwrap_or(Rgba::WHITE),
        stroke: parse_hex(h.stroke.as_deref()),
        square: h.shape.as_deref() == Some("square"),
        label: h.label.clone(),
        label_color: parse_hex(h.label_color.as_deref()).unwrap_or(Rgba::BLACK),
        label_size: h.label_size.unwrap_or(9.0),
        x_value: xv,
        y_value: yv,
        draggable: h.drag == Some(true),
        touch_state: touch,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{GraphBand, GraphRow, GraphSelect};

    fn v(json: &str) -> GraphValue {
        serde_json::from_str(json).unwrap()
    }

    fn ctx(values: &'static [(u16, f64)]) -> impl Lookup {
        FnLookup(move |k: u16| values.iter().find(|(i, _)| *i == k).map(|x| x.1).unwrap_or(0.0), |m: &str| if m == "lvl" { Some(0.1) } else { None })
    }

    #[test]
    fn graph_value_decodes_like_swift() {
        assert_eq!(v("3.5").constant, Some(3.5));
        let g = v(r#"{"param": 2, "law": {"type": "log", "min": 20, "max": 20000}, "scale": 2, "offset": 1}"#);
        assert_eq!((g.param, g.scale, g.offset), (Some(2), Some(2.0), Some(1.0)));
        // `min` / `max` count only as lists (Swift's try?): a scalar is dropped, not fatal.
        let g = v(r#"{"min": 3, "param": 1}"#);
        assert!(g.min_of.is_none());
        assert_eq!(g.param, Some(1));
        let g = v(r#"{"min": [1, {"param": 0}], "max": [2]}"#);
        assert_eq!(g.min_of.as_ref().map(|l| l.len()), Some(2));
        assert_eq!(g.max_of.as_ref().map(|l| l.len()), Some(1));
        // A list holding something that is not a value is dropped too.
        assert!(v(r#"{"max": ["x"]}"#).max_of.is_none());
        assert!(serde_json::from_str::<GraphValue>(r#""str""#).is_err());
        // Round trip keeps the keys.
        let j = serde_json::to_value(v(r#"{"min": [1, 2], "ratio": [{"param": 1}, 2]}"#)).unwrap();
        assert!(j.get("min").is_some() && j.get("ratio").is_some());
    }

    #[test]
    fn graph_value_resolution_order_and_ops() {
        let c = ctx(&[(0, 0.5), (1, 1.0), (2, 0.25)]);
        assert_eq!(v("2").resolve(&c), 2.0);
        assert_eq!(v(r#"{"sum": [1, {"param": 0}]}"#).resolve(&c), 1.5);
        assert_eq!(v(r#"{"sum": []}"#).resolve(&c), 0.0);
        assert_eq!(v(r#"{"product": [3, {"param": 0}]}"#).resolve(&c), 1.5);
        assert_eq!(v(r#"{"min": [3, {"param": 0}, 7]}"#).resolve(&c), 0.5);
        assert_eq!(v(r#"{"max": [3, {"param": 0}, 7]}"#).resolve(&c), 7.0);
        assert_eq!(v(r#"{"ratio": [3, {"param": 0}]}"#).resolve(&c), 6.0);
        assert_eq!(v(r#"{"ratio": [3, 0]}"#).resolve(&c), 0.0);
        // `sum` wins over `min` when both are present (Swift's precedence).
        assert_eq!(v(r#"{"sum": [1], "min": [5]}"#).resolve(&c), 1.0);
        // apply: law on any value (clamped to 0…1 by the law), then scale / offset.
        assert_eq!(v(r#"{"apply": [0.5], "law": {"type": "linear", "min": 0, "max": 10}, "scale": 2, "offset": 1}"#).resolve(&c), 11.0);
        assert_eq!(v(r#"{"apply": [4], "scale": 2}"#).resolve(&c), 8.0);
        // select: the parameter's step index picks the case.
        let sel = GraphValue { select: Some(Box::new(GraphSelect { param: 2, cases: vec![GraphValue::constant(10.0), GraphValue::constant(20.0), GraphValue::constant(30.0)] })), ..Default::default() };
        assert_eq!(sel.resolve(&c), 20.0, "0.25 of 3 cases rounds to case 1 (0.5 rounds away from zero)");
        // table: nearest row by L1 over the params' values.
        let t = GraphValue { params: Some(vec![0, 1]), table: Some(vec![GraphRow { key: vec![0.0, 0.0], value: 1.0 }, GraphRow { key: vec![0.6, 0.9], value: 2.0 }]), ..Default::default() };
        assert_eq!(t.resolve(&c), 2.0);
        // meter: conditioned raw, optionally in dB, then scale / offset; absent meters read 0.
        assert!((v(r#"{"meter": "lvl", "db": 20, "offset": 3}"#).resolve(&c) - (-17.0)).abs() < 1e-9);
        assert_eq!(v(r#"{"meter": "none", "scale": 2, "offset": 1}"#).resolve(&c), 1.0);
        assert_eq!(v(r#"{"meter": "none", "db": 10}"#).resolve(&c), -120.0, "log floor 1e-12");
        // param through a law; no param = 0.
        assert!((v(r#"{"param": 0, "law": {"type": "log", "min": 20, "max": 20000}}"#).resolve(&c) - 632.455532).abs() < 1e-5);
        assert_eq!(v(r#"{"scale": 2}"#).resolve(&c), 0.0);
        assert!(GraphValue::is_on(None, &c));
        assert!(GraphValue::is_on(Some(&v(r#"{"param": 0}"#)), &c));
        assert!(!GraphValue::is_on(Some(&v(r#"{"param": 2}"#)), &c));
    }

    #[test]
    fn laws_map_and_invert() {
        let law = |j: &str| serde_json::from_str::<GraphLaw>(j).unwrap();
        let lin = law(r#"{"type": "linear", "min": -20, "max": 20}"#);
        assert_eq!(lin.map(0.75), 10.0);
        assert_eq!(lin.map(2.0), 20.0, "clamped");
        assert_eq!(lin.normalized_for(10.0), 0.75);
        let log = law(r#"{"type": "log", "min": 20, "max": 20000}"#);
        assert!((log.map(log.normalized_for(1000.0)) - 1000.0).abs() < 1e-9);
        assert_eq!(law(r#"{"type": "log", "min": 0, "max": 10}"#).map(0.5), 5.0, "non-positive log ends fall back to linear");
        assert!((law(r#"{"type": "power", "min": 0, "max": 100, "power": 2}"#).map(0.5) - 25.0).abs() < 1e-12);
        let bi = law(r#"{"type": "bipolar", "min": -12, "max": 12, "power": 2}"#);
        assert!((bi.map(0.25) - (-3.0)).abs() < 1e-12);
        assert!((bi.map(0.75) - 3.0).abs() < 1e-12);
        let steps = law(r#"{"type": "steps", "values": [1, 2, 4, 8]}"#);
        assert_eq!(steps.map(0.5), 4.0, "round(0.5 × 3) = 2 (half away from zero)");
        assert_eq!(steps.normalized_for(3.9), 2.0 / 3.0);
        let table = law(r#"{"type": "table", "points": [[0, 100], [0.5, 1000], [1, 10000]], "interp": "log"}"#);
        assert!((table.map(0.25) - 316.227766).abs() < 1e-5);
        assert_eq!(table.map(-1.0), 100.0);
        assert!((table.normalized_for(1000.0) - 0.5).abs() < 1e-3, "brute-force inverse");
        assert_eq!(law(r#"{"type": "mystery"}"#).map(0.3), 0.3);
        // GraphValue inverse through scale / offset.
        let gv = v(r#"{"param": 1, "law": {"type": "linear", "min": 0, "max": 10}, "scale": 2, "offset": 1}"#);
        assert_eq!(gv.normalized_for(11.0), Some(0.5));
        assert_eq!(v(r#"{"sum": [1]}"#).normalized_for(1.0), None);
    }

    fn band(json: &str) -> GraphBand {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn band_magnitudes() {
        let c = ctx(&[]);
        let bell = band(r#"{"id": "b", "filter": "bell", "freq": 1000, "gain": 6, "q": 1}"#);
        assert!((band_db(&bell, 1000.0, None, &c) - 6.0).abs() < 1e-9, "a bell reaches its gain at f0");
        assert!(band_db(&bell, 20.0, None, &c).abs() < 0.05);
        let ls = band(r#"{"id": "l", "filter": "lowShelf", "freq": 100, "gain": -10}"#);
        assert!((band_db(&ls, 5.0, None, &c) + 10.0).abs() < 0.05);
        assert!(band_db(&ls, 20000.0, None, &c).abs() < 0.05);
        let hp = band(r#"{"id": "h", "filter": "highPass", "freq": 100, "order": 2}"#);
        assert!((band_db(&hp, 100.0, None, &c) + 3.0103).abs() < 0.01, "Butterworth −3 dB at the corner");
        assert!((band_db(&hp, 10.0, None, &c) + 40.0).abs() < 0.1, "12 dB/oct");
        let lr = band(r#"{"id": "h", "filter": "lowPass", "freq": 100, "order": 4, "shape": "linkwitzRiley"}"#);
        assert!((band_db(&lr, 100.0, None, &c) + 6.0206).abs() < 0.01, "LR4 −6 dB at the corner");
        let flat = band(r#"{"id": "h", "filter": "highPass", "freq": 100, "order": 0}"#);
        assert_eq!(band_db(&flat, 10.0, None, &c), 0.0);
        let gain = band(r#"{"id": "g", "filter": "gain", "gain": 4}"#);
        assert_eq!(band_db(&gain, 10.0, Some(2.0), &c), 2.0, "curve gain overrides");
        // bandwidth → Q; peak and 3dB Q modes; first-order shelf by shelfOrder.
        let bw = band(r#"{"id": "b", "freq": 1000, "gain": 12, "bandwidth": 1}"#);
        assert!((band_db(&bw, 1000.0, None, &c) - 12.0).abs() < 1e-9);
        let peak = band(r#"{"id": "b", "freq": 1000, "gain": 12, "q": 1, "qMode": "peak"}"#);
        assert!(band_db(&peak, 2000.0, None, &c) > band_db(&bw, 2000.0, None, &c) - 20.0);
        let three = band(r#"{"id": "b", "freq": 1000, "gain": 1, "q": 1, "qMode": "3dB"}"#);
        assert!(band_db(&three, 1000.0, None, &c).is_finite(), "small gains stay finite in 3dB mode");
        let ls1 = band(r#"{"id": "l", "filter": "lowShelf", "freq": 100, "gain": 6, "shelfOrder": 1}"#);
        assert!((band_db(&ls1, 1.0, None, &c) - 6.0).abs() < 0.01);
        // filterParam picks the type from a stepped parameter.
        let sel = band(r#"{"id": "s", "filterParam": {"param": 0, "values": ["bell", "gain"]}, "gain": 3}"#);
        assert_eq!(band_db(&sel, 50.0, None, &ctx(&[(0, 1.0)])), 3.0);
        let (qs, odd) = butterworth_qs(3);
        assert!(odd && qs.len() == 1 && (qs[0] - 1.0).abs() < 1e-12);
        assert_eq!(butterworth_qs(1), (vec![], true));
    }

    #[test]
    fn transfer_curves() {
        let c = ctx(&[]);
        let t = |j: &str| serde_json::from_str::<GraphTrace>(j).unwrap();
        let comp = t(r#"{"kind": "compressor", "threshold": -20, "ratio": 4}"#);
        assert_eq!(transfer(&comp, -30.0, &c), -30.0);
        assert_eq!(transfer(&comp, 0.0, &c), -15.0);
        let lim = t(r#"{"kind": "limiter", "threshold": -10, "makeup": 2}"#);
        assert_eq!(transfer(&lim, 0.0, &c), -8.0);
        let knee = t(r#"{"kind": "compressor", "threshold": -20, "ratio": 2, "knee": 10}"#);
        assert!((transfer(&knee, -20.0, &c) - (-20.0 - 0.5 * 25.0 / 20.0)).abs() < 1e-12);
        let exp = t(r#"{"kind": "expander", "threshold": -40, "ratio": 2, "range": 10}"#);
        assert_eq!(transfer(&exp, -50.0, &c), -60.0);
        assert_eq!(transfer(&exp, -80.0, &c), -90.0, "range limits the depth");
        let gate = t(r#"{"kind": "gate", "threshold": -50}"#);
        assert_eq!(transfer(&gate, -60.0, &c), -260.0);
    }

    #[test]
    fn axes_and_half_floats() {
        let g: Graph = serde_json::from_str(r#"{"type": "eq", "x": {"min": 20, "max": 20000, "from": 10, "to": 110}, "y": {"min": -20, "max": 20}}"#).unwrap();
        let a = Axes::new(&g, 120.0, 40.0);
        assert!(a.x_log);
        assert!((a.px(632.455532) - 60.0).abs() < 1e-6);
        assert!((a.x_value(60.0) - 632.455532).abs() < 1e-4);
        assert_eq!(a.py(20.0), 0.0);
        assert_eq!(a.py(-20.0), 40.0);
        assert_eq!(a.y_value(20.0), 0.0);
        assert_eq!(a.clamp_y(50.0), 20.0);
        let t: Graph = serde_json::from_str(r#"{"type": "transfer"}"#).unwrap();
        assert!(!Axes::new(&t, 10.0, 10.0).x_log, "only eq graphs default to a log x axis");
        assert_eq!(half(0x3c00), 1.0);
        assert_eq!(half(0xc000), -2.0);
        assert_eq!(half(0x0001), 2f64.powi(-24));
        assert_eq!(half(0x7c00), f64::INFINITY);
        assert!(half(0x7e00).is_nan());
        assert_eq!(half(0x3555), 0.333251953125);
        assert_eq!(limit(f64::NAN), -1000.0);
        assert_eq!(limit(5000.0), 1000.0);
        assert_eq!(parse_hex(Some(" #FF000080 ")), Some(Rgba::rgb8(255, 0, 0).with_alpha(128.0 / 255.0)));
        assert_eq!(parse_hex(Some("FF0000")), None);
        assert_eq!(parse_hex(Some("#GG0000")), None);
    }

    #[test]
    fn catmull_rom_passes_through_points() {
        let p = Path::curve(&[(0.0, 0.0), (10.0, 10.0), (20.0, 0.0)], true);
        assert_eq!(p.segs.len(), 3);
        assert_eq!(p.segs[1], Seg::Cubic([10.0 / 6.0, 10.0 / 6.0, 10.0 - 20.0 / 6.0, 10.0, 10.0, 10.0]));
        assert!(matches!(p.segs[2], Seg::Cubic([_, _, _, _, 20.0, 0.0])));
        assert_eq!(Path::curve(&[(0.0, 0.0), (1.0, 1.0)], true).segs.len(), 2, "two points stay straight");
    }

    #[test]
    fn peak_marker_refines_parabolically() {
        // Symmetric neighbours: the peak stays on the bin.
        let pts = vec![(100.0, -20.0), (200.0, -6.0), (300.0, -20.0)];
        assert_eq!(Scene::peak_frequency(&pts, 0.0, 1000.0, -96.0), Some(200.0));
        // A louder right neighbour pulls the estimate right, by at most one bin.
        let pts = vec![(100.0, -20.0), (200.0, -6.0), (300.0, -8.0)];
        let f = Scene::peak_frequency(&pts, 0.0, 1000.0, -96.0).unwrap();
        let (a0, a1, a2) = (10f64.powf(-1.0), 10f64.powf(-0.3), 10f64.powf(-0.4));
        let delta = 0.5 * (a0 - a2) / (a0 - 2.0 * a1 + a2);
        assert!((f - (200.0 + delta * 100.0)).abs() < 1e-9 && f > 200.0);
        // Range and floor.
        assert_eq!(Scene::peak_frequency(&pts, 250.0, 400.0, -96.0), Some(300.0), "the edge bin is not refined");
        assert_eq!(Scene::peak_frequency(&pts, 0.0, 1000.0, -6.0), None, "at or below minDb is hidden");
    }

    #[test]
    fn eq_scene_fills_to_the_baseline_and_places_handles() {
        let g: Graph = serde_json::from_str(
            r##"{"type": "eq", "x": {"min": 20, "max": 20000}, "y": {"min": -20, "max": 20},
                "bands": [{"id": "lf", "filter": "lowShelf", "freq": 100, "gain": {"param": 0, "law": {"type": "linear", "min": -20, "max": 20}}}],
                "curves": [{"fill": "#FF000080", "stroke": "#FFFFFF", "width": 2}],
                "handles": [{"band": "lf", "drag": true, "radius": 5}],
                "ranges": [{"from": 100, "to": 1000, "y": 10, "dots": true}]}"##,
        )
        .unwrap();
        let s = Scene::build_values(&g, 200.0, 100.0, &ctx(&[(0, 0.75)]));
        assert_eq!(s.fills.len(), 1);
        assert_eq!(s.strokes.len(), 1);
        assert_eq!(s.strokes[0].width, 2.0);
        // One polygon: the curve, then the baseline corners, closed.
        let segs = &s.fills[0].path.segs;
        assert_eq!(segs.len(), 400 + 2 + 1);
        assert_eq!(segs[400], Seg::Line(200.0, 50.0));
        assert_eq!(segs[401], Seg::Line(0.0, 50.0));
        assert_eq!(segs[402], Seg::Close);
        // The handle sits at (f0, gain) and writes those parameters.
        let h = &s.handles[0];
        assert!((h.x - s.axes.px(100.0)).abs() < 1e-9);
        assert_eq!(h.y, 25.0);
        assert!(h.draggable && h.x_value.is_some() && h.y_value.as_ref().and_then(|v| v.param) == Some(0));
        assert_eq!(s.overlays.len(), 3, "range line and its two dots");
    }

    #[test]
    fn polar_scene_traces_lobes_arcs_and_markers() {
        let g: Graph = serde_json::from_str(
            r##"{"type": "polar", "polar": {"center": [50, 50], "radius": 40},
                "traces": [{"pattern": 0.5, "stroke": "#FFA31A"},
                           {"shape": "lobe", "amplitude": 1, "mirror": "opposite", "fill": "#FFFFFF"},
                           {"shape": "arc", "arcFrom": -90, "arcTo": 90, "amplitude": 0.5, "stroke": "#00FF00"},
                           {"shape": "arc", "arcFrom": 10, "arcTo": 10, "stroke": "#00FF00"},
                           {"pattern": 1, "opacity": 0, "stroke": "#FFFFFF"}],
                "markers": [{"angle": 90, "flip": 1}]}"##,
        )
        .unwrap();
        let s = Scene::build_values(&g, 100.0, 100.0, &ctx(&[]));
        // Cardioid: 361 points, full amplitude at the front (up).
        let first = &s.strokes[0].path.segs[0];
        assert_eq!(*first, Seg::Move(50.0, 10.0));
        assert_eq!(s.fills.len(), 2, "lobe and its opposite mirror");
        assert_eq!(s.strokes.len(), 2, "pattern stroke + the half-radius arc (empty arcs and transparent traces skipped)");
        let Seg::Move(x, y) = s.strokes[1].path.segs[0] else { panic!() };
        assert!((x - 30.0).abs() < 1e-9 && (y - 50.0).abs() < 1e-9, "arc starts at −90° on radius 20");
        // Marker at 90° + 180° (flipped): the tip points in from the left rim.
        let Seg::Move(mx, my) = s.overlays[0].path.segs[0] else { panic!() };
        assert!((mx - 16.0).abs() < 1e-9 && (my - 50.0).abs() < 1e-9);
    }

    #[test]
    fn spectrum_decodes_fp16_and_reverse_bins() {
        struct Block;
        impl Lookup for Block {
            fn value(&self, _: u16) -> f64 {
                0.0
            }
            fn meter(&self, _: &str) -> Option<f64> {
                None
            }
        }
        impl SceneInput for Block {
            fn block(&self, id: i64) -> Option<f64> {
                // Two halves: 1.0 (low) and 0.5 (high).
                (id == 12).then(|| f32::from_bits(0x3800_3c00) as f64)
            }
        }
        let g: Graph = serde_json::from_str(r##"{"type": "eq", "spectrum": {"first": 12, "count": 1, "hz0": 1000, "hzStep": 200, "db": 20, "decode": "fp16x2", "stroke": "#FFFFFF"}}"##).unwrap();
        let s = Scene::build(&g, 100.0, 50.0, &Block);
        assert_eq!(s.spectrum_points.len(), 2);
        assert_eq!(s.spectrum_points[0], (1000.0, 0.0));
        assert!((s.spectrum_points[1].0 - 1100.0).abs() < 1e-9 && (s.spectrum_points[1].1 + 6.0206).abs() < 1e-3);
        assert_eq!(s.strokes.len(), 1);
        let g: Graph = serde_json::from_str(r##"{"type": "eq", "spectrum": {"first": 10, "count": 3, "hz0": 0, "hzStep": 0, "hz": [100, 200, 300], "reverse": true}}"##).unwrap();
        let s = Scene::build(&g, 100.0, 50.0, &Block);
        assert_eq!(s.spectrum_points.len(), 1, "display bin 0 reads variable 12 when reversed");
        assert_eq!(s.spectrum_points[0].0, 100.0);
    }
}
