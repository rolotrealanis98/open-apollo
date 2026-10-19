//! Meter ballistics: what the UA Mixer Engine and Console do to a meter between the signal and
//! the screen, computed against the frame clock so the result does not depend on how often
//! values arrive or how often we draw.
//!
//! Engine (UA Mixer Engine 12.0.0 arm64, `UADMeterValue::Update` 0x100068130, run by
//! `CMxMeter::Update` 0x100067f98 for `MeterLevel` and `MeterPeakLevel`):
//! - a value at or above the shown one is taken at once and starts the hold;
//! - the hold is 0 for `MeterLevel` and `/PeakHold` for `MeterPeakLevel`
//!   (`CMxRoot::PeakHoldDurationMs` 0x10009c1f0, table 0x1005d4780: NONE 0, 1, 3, 5, 10 s,
//!   INFINITE −1);
//! - after the hold the value falls as `(dB + 77) · c − 77` per step, c = `MeterDecayCoefficient`
//!   0.993 (`GlobalSettings` constructor 0x1000b6720), never below the input. The step period is
//!   16 ms (`CMxAuxBus` constructor 0x10004b610) but the engine drops the remainder each call, so
//!   it is one step per 30.7 ms meter tick: the Console 1.3.1 capture (`console.pcap`, client 54264)
//!   shows ×0.99300 per push, pushes 30.7 ms apart (λ ≈ −0.229/s, about 17.6 dB/s from 0 dBFS).
//!
//! Console (Rack.dll 1.3.1, `Rack_Meter` tick 0x1809cb720): the bar eases toward the scale
//! fraction, `v += (target − v) · 0.6` per 16 666 µs step (`meter_smoothing_coefficient`
//! 0x18087d4d0, default 0.6 at 0x183bbdd58; step in the constructor 0x1809c9780). The peak line
//! and the clip LED are drawn as they are.

use std::collections::HashMap;

use crate::scales::meter_fraction;

/// The engine's silence floor, dBFS.
pub const FLOOR_DB: f64 = -77.0;
/// `MeterDecayCoefficient`: the share of (dB + 77) kept per engine meter tick.
pub const DECAY_COEFFICIENT: f64 = 0.993;
/// The engine's meter tick (`/MeterPulse` median, Console 1.3.1 capture), seconds.
pub const ENGINE_TICK: f64 = 0.030_71;
/// Console's `meter_smoothing_coefficient`: the share of the gap closed per step.
pub const SMOOTHING: f64 = 0.6;
/// Console's meter animation step, seconds.
pub const SMOOTHING_STEP: f64 = 0.016_666;

/// A hold time (`/PeakHold`, `/ClipHold`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hold {
    Seconds(f64),
    Forever,
}

impl Hold {
    pub const NONE: Hold = Hold::Seconds(0.0);
    /// The engine's default `/PeakHold` ("3 SEC").
    pub const DEFAULT_PEAK: Hold = Hold::Seconds(3.0);

    /// The engine's choice label: "NONE", "1 SEC" … "10 SEC", "INFINITE".
    pub fn from_label(label: &str) -> Option<Hold> {
        let l = label.trim().to_ascii_uppercase();
        match l.as_str() {
            "NONE" => Some(Hold::NONE),
            "INFINITE" => Some(Hold::Forever),
            _ => l.strip_suffix("SEC").and_then(|n| n.trim().parse::<f64>().ok()).map(Hold::Seconds),
        }
    }
}

/// `(db + 77) · c^(dt / tick) − 77`: the engine's fall over `dt` seconds.
pub fn decay(db: f64, dt: f64) -> f64 {
    (db - FLOOR_DB) * DECAY_COEFFICIENT.powf(dt / ENGINE_TICK) + FLOOR_DB
}

/// One `UADMeterValue`: instant rise, hold, then the engine's fall.
#[derive(Clone, Copy, Debug)]
struct Held {
    out: f64,
    /// When the hold started (the last time the input reached `out`).
    since: f64,
}

impl Held {
    fn new() -> Held {
        Held { out: FLOOR_DB, since: 0.0 }
    }

    /// Takes input `x` at `now`; `last` is the previous update's time.
    fn update(&mut self, x: f64, now: f64, last: f64, hold: Hold) -> f64 {
        if x >= self.out {
            self.out = x;
            self.since = now;
            return x;
        }
        let end = match hold {
            Hold::Seconds(s) => self.since + s,
            Hold::Forever => return self.out,
        };
        if now > end {
            self.out = decay(self.out, now - last.max(end)).max(x);
        }
        self.out
    }
}

/// One meter leg's state: the engine's level and held peak, and Console's eased bar.
#[derive(Clone, Copy, Debug)]
pub struct MeterBallistics {
    level: Held,
    peak: Held,
    /// The last `MeterPeakLevel` received, to catch peaks between level pushes.
    engine_peak: f64,
    /// The bar as drawn, 0…1.
    shown: f64,
    last: Option<f64>,
}

impl Default for MeterBallistics {
    fn default() -> Self {
        MeterBallistics { level: Held::new(), peak: Held::new(), engine_peak: FLOOR_DB, shown: 0.0, last: None }
    }
}

/// What a leg shows this frame: the bar and the peak line as 0…1 scale fractions.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Shown {
    pub level: f64,
    pub peak: f64,
}

fn clean(db: f64) -> f64 {
    if db.is_nan() { FLOOR_DB } else { db.max(FLOOR_DB) }
}

impl MeterBallistics {
    /// Takes the latest engine reading (`level_db`, `peak_db`, dBFS) at time `now` (seconds,
    /// any epoch, never decreasing) and returns what to draw.
    pub fn update(&mut self, now: f64, level_db: f64, peak_db: f64, peak_hold: Hold) -> Shown {
        let last = self.last.unwrap_or(now).min(now);
        let x = clean(level_db);
        let level = self.level.update(x, now, last, Hold::NONE);
        // A peak the level pushes missed (the engine's peak saw every sample) counts when it
        // rises; while the engine holds it flat it is not a new peak.
        let engine_peak = clean(peak_db);
        let px = if engine_peak > self.engine_peak { x.max(engine_peak) } else { x };
        self.engine_peak = engine_peak;
        let peak = self.peak.update(px, now, last, peak_hold).max(level);
        let target = meter_fraction(level);
        self.shown = match self.last {
            None => target,
            Some(_) => {
                let k = 1.0 - (1.0 - SMOOTHING).powf((now - last) / SMOOTHING_STEP);
                self.shown + (target - self.shown) * k
            }
        };
        self.last = Some(now);
        Shown { level: self.shown, peak: meter_fraction(peak) }
    }
}

/// Ballistics for every meter, keyed by meter node.
#[derive(Debug, Default)]
pub struct MeterBank {
    meters: HashMap<String, MeterBallistics>,
}

impl MeterBank {
    pub fn update(&mut self, key: &str, now: f64, level_db: f64, peak_db: f64, peak_hold: Hold) -> Shown {
        match self.meters.get_mut(key) {
            Some(m) => m.update(now, level_db, peak_db, peak_hold),
            None => self.meters.entry(key.to_owned()).or_default().update(now, level_db, peak_db, peak_hold),
        }
    }

    pub fn clear(&mut self) {
        self.meters.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feeds `input(t)` at `rate` Hz and draws at `fps`, returning the bar and peak (dB-domain
    /// where noted) sampled at `at` seconds.
    fn run(input: impl Fn(f64) -> f64, rate: f64, fps: f64, hold: Hold, at: &[f64]) -> Vec<Shown> {
        let mut m = MeterBallistics::default();
        let end = at.iter().cloned().fold(0.0, f64::max);
        let (mut t, mut next_push, mut held) = (0.0, 0.0, FLOOR_DB);
        let mut out = Vec::new();
        let mut ats = at.to_vec();
        ats.sort_by(f64::total_cmp);
        let mut ai = 0;
        while t <= end + 1e-9 {
            if t + 1e-9 >= next_push {
                held = input(t);
                next_push += 1.0 / rate;
            }
            let s = m.update(t, held, held, hold);
            while ai < ats.len() && (ats[ai] - t).abs() < 0.5 / fps {
                out.push(s);
                ai += 1;
            }
            t += 1.0 / fps;
        }
        out
    }

    fn db_of(fraction: f64) -> f64 {
        // Inverse of meter_fraction by bisection.
        let (mut lo, mut hi) = (-60.0, 0.0);
        for _ in 0..60 {
            let mid = (lo + hi) / 2.0;
            if meter_fraction(mid) < fraction { lo = mid } else { hi = mid }
        }
        (lo + hi) / 2.0
    }

    #[test]
    fn hold_labels() {
        assert_eq!(Hold::from_label("NONE"), Some(Hold::NONE));
        assert_eq!(Hold::from_label("3 SEC"), Some(Hold::Seconds(3.0)));
        assert_eq!(Hold::from_label("10 SEC"), Some(Hold::Seconds(10.0)));
        assert_eq!(Hold::from_label("INFINITE"), Some(Hold::Forever));
        assert_eq!(Hold::from_label("?"), None);
    }

    #[test]
    fn step_down_falls_at_the_engine_rate() {
        // 0 dBFS for 1 s, then silence: the bar falls as (dB + 77) · 0.993 per 30.71 ms.
        let step = |t: f64| if t < 1.0 { 0.0 } else { FLOOR_DB };
        let s = run(step, 50.0, 60.0, Hold::DEFAULT_PEAK, &[0.99, 1.5, 2.0, 3.0]);
        assert!((s[0].level - 1.0).abs() < 1e-6, "full scale before the step: {:?}", s[0]);
        for (i, t) in [(1, 0.5), (2, 1.0), (3, 2.0)] {
            let want = decay(0.0, t);
            let got = db_of(s[i].level);
            // The bar trails the engine value by Console's easing (≈ 2 frames).
            assert!((got - want).abs() < 1.5, "{t} s after the step: {got:.2} dB, engine law {want:.2} dB");
        }
        // About 1.3 s from 0 to −20 dB, the real engine's speed (not a sudden drop).
        assert!(db_of(s[1].level) > -12.0 && db_of(s[2].level) < -12.0);
    }

    #[test]
    fn peak_holds_then_decays() {
        let burst = |t: f64| if t < 0.1 { -6.0 } else { FLOOR_DB };
        let s = run(burst, 50.0, 60.0, Hold::Seconds(3.0), &[1.0, 2.9, 3.6, 4.1]);
        // Held at −6 dB for 3 s after the burst ended (≈0.1 s).
        for p in &s[..2] {
            assert!((db_of(p.peak) + 6.0).abs() < 0.05, "held: {p:?}");
        }
        assert!((db_of(s[2].peak) - decay(-6.0, 0.5)).abs() < 0.3, "decaying: {:?}", s[2]);
        assert!(db_of(s[3].peak) < db_of(s[2].peak));
        // The bar fell long before the peak line.
        assert!(s[0].level < s[0].peak);
    }

    #[test]
    fn hold_none_and_forever() {
        let burst = |t: f64| if t < 0.1 { -6.0 } else { FLOOR_DB };
        let none = run(burst, 50.0, 60.0, Hold::NONE, &[0.6]);
        assert!(db_of(none[0].peak) < -10.0, "{:?}", none[0]);
        let forever = run(burst, 50.0, 60.0, Hold::Forever, &[20.0]);
        assert!((db_of(forever[0].peak) + 6.0).abs() < 0.05, "{:?}", forever[0]);
    }

    #[test]
    fn independent_of_update_and_frame_rate() {
        let step = |t: f64| {
            if t < 0.5 {
                -3.0
            } else if t < 0.6 {
                -40.0
            } else {
                FLOOR_DB
            }
        };
        let at = [0.75, 1.0, 1.5, 2.5, 4.0];
        let base = run(step, 1000.0, 1000.0, Hold::Seconds(1.0), &at);
        for (rate, fps) in [(50.0, 60.0), (32.6, 120.0), (20.0, 30.0), (50.0, 144.0)] {
            let s = run(step, rate, fps, Hold::Seconds(1.0), &at);
            for (a, b) in base.iter().zip(&s) {
                let (da, db) = (db_of(a.level), db_of(b.level));
                assert!((da - db).abs() < 1.0, "{rate} Hz pushes / {fps} fps: bar {db:.2} dB vs {da:.2} dB");
                let (pa, pb) = (db_of(a.peak), db_of(b.peak));
                assert!((pa - pb).abs() < 1.0, "{rate} Hz pushes / {fps} fps: peak {pb:.2} dB vs {pa:.2} dB");
            }
        }
    }

    #[test]
    fn rise_eases_over_a_few_frames() {
        let mut m = MeterBallistics::default();
        m.update(0.0, FLOOR_DB, FLOOR_DB, Hold::DEFAULT_PEAK);
        let one = m.update(SMOOTHING_STEP, 0.0, 0.0, Hold::DEFAULT_PEAK);
        assert!((one.level - 0.6).abs() < 1e-6, "{one:?}");
        assert_eq!(one.peak, 1.0, "the peak line jumps");
        let three = m.update(3.0 * SMOOTHING_STEP, 0.0, 0.0, Hold::DEFAULT_PEAK);
        assert!((three.level - (1.0 - 0.4f64.powi(3))).abs() < 1e-6, "{three:?}");
    }

    #[test]
    fn engine_peak_between_pushes_counts_once() {
        let mut m = MeterBallistics::default();
        // The level push missed a −2 dB transient the engine's peak caught.
        let a = m.update(0.0, -30.0, -2.0, Hold::Seconds(1.0));
        assert!((db_of(a.peak) + 2.0).abs() < 0.05);
        // The engine keeps reporting its held −2: that does not restart our hold.
        let b = m.update(1.5, -30.0, -2.0, Hold::Seconds(1.0));
        assert!(db_of(b.peak) < -2.5, "{b:?}");
    }
}
