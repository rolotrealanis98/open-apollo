//! Tapers and scale laws recovered from Rack.dll.

/// Console fader taper (UATaper table 3, 0x181f71c50): dB ↔ FaderLevelTapered.
pub const FADER_TAPER: [(f64, f64); 13] = [
    (12.0, 1.0), (6.0, 0.8909), (0.0, 0.7818), (-6.0, 0.6727), (-9.0, 0.6182), (-12.0, 0.5636), (-18.0, 0.4713),
    (-28.0, 0.3537), (-32.0, 0.3066), (-40.0, 0.2550), (-56.0, 0.1518), (-86.0, 0.0501), (-144.0, 0.0),
];

pub const UNITY: f64 = 0.781_818_2;

pub fn fader_tapered(db: f64) -> f64 {
    if db >= FADER_TAPER[0].0 {
        return 1.0;
    }
    if db <= FADER_TAPER[12].0 {
        return 0.0;
    }
    for w in FADER_TAPER.windows(2) {
        let ((d0, t0), (d1, t1)) = (w[0], w[1]);
        if db <= d0 && db >= d1 {
            return t1 + (db - d1) / (d0 - d1) * (t0 - t1);
        }
    }
    0.0
}

pub fn fader_db(t: f64) -> f64 {
    if t <= 0.0 {
        return f64::NEG_INFINITY;
    }
    if t >= 1.0 {
        return FADER_TAPER[0].0;
    }
    for w in FADER_TAPER.windows(2) {
        let ((d0, t0), (d1, t1)) = (w[0], w[1]);
        if t <= t0 && t >= t1 {
            return d1 + (t - t1) / (t0 - t1) * (d0 - d1);
        }
    }
    f64::NEG_INFINITY
}

/// Console's fader readout (Rack.dll 0x181bb1294; aux faders and mini sends too): `-∞` below
/// −143.9 dB, otherwise the 32-bit value to one decimal with `+` when it is above 0. The sign is
/// the unrounded value's: −0.01 prints "-0.0", +0.047 "+0.0", only exactly 0 prints "0.0".
pub fn format_db(db: f64) -> String {
    let v = db as f32;
    if !(v as f64 >= -143.9) {
        return "-∞".into();
    }
    format!("{}{:.1}", if v > 0.0 { "+" } else { "" }, v as f64)
}

/// Channel meter law (table 0x184c38508): evenly spaced labels, piecewise linear between them.
pub const METER_MARKS: [f64; 12] = [0.0, -3.0, -6.0, -9.0, -12.0, -15.0, -18.0, -21.0, -27.0, -36.0, -46.0, -60.0];

pub fn meter_fraction(db: f64) -> f64 {
    if !db.is_finite() || db <= METER_MARKS[11] {
        return 0.0;
    }
    if db >= 0.0 {
        return 1.0;
    }
    let step = 1.0 / (METER_MARKS.len() - 1) as f64;
    for i in 0..METER_MARKS.len() - 1 {
        let (a, b) = (METER_MARKS[i], METER_MARKS[i + 1]);
        if db <= a && db >= b {
            return 1.0 - (i as f64 + (a - db) / (a - b)) * step;
        }
    }
    0.0
}

/// Fader mark labels (dense set, 0x1809b1c60).
pub const FADER_MARKS: [(&str, f64); 9] =
    [("12", 12.0), ("6", 6.0), ("0", 0.0), ("6", -6.0), ("12", -12.0), ("20", -20.0), ("32", -32.0), ("56", -56.0), ("∞", -144.0)];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn taper_round_trip() {
        assert!((fader_tapered(0.0) - 0.7818).abs() < 1e-4);
        assert!((fader_db(0.6727) + 6.0).abs() < 1e-3);
        assert_eq!(format_db(fader_db(0.0)), "-∞");
        assert_eq!(format_db(-9.37), "-9.4");
        assert_eq!(format_db(-0.01), "-0.0");
        assert_eq!(format_db(0.047), "+0.0");
        assert_eq!(format_db(0.0), "0.0");
        assert_eq!(format_db(6.0), "+6.0");
    }

    #[test]
    fn meter_law() {
        assert_eq!(meter_fraction(0.0), 1.0);
        assert!((meter_fraction(-12.0) - 0.6364).abs() < 1e-3);
        assert_eq!(meter_fraction(-77.0), 0.0);
    }
}
