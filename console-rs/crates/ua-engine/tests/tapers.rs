//! Numbers from console-port/protocol/ui-to-path.md and the live Apollo x4 tree.

use std::collections::BTreeSet;

use ua_engine::tapers::{self, fader, monitor, pan, preamp};

fn close(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() < tol
}

#[test]
fn fader_unity_and_ends() {
    assert!(close(fader::UNITY, 0.7818182, 1e-7));
    assert!(close(fader::db_from_tapered(0.7818182), 0.0, 1e-4));
    assert_eq!(fader::db_from_tapered(1.0), 12.0);
    assert_eq!(fader::db_from_tapered(0.0), -144.0);
    assert!(close(fader::tapered_from_db(0.0), 43.0 / 55.0, 1e-12));
    assert_eq!(fader::tapered_from_db(12.0), 1.0);
    assert_eq!(fader::tapered_from_db(-200.0), 0.0);
}

#[test]
fn fader_lower_table_anchors() {
    for (t, db) in [
        (0.05, -86.1),
        (0.10, -71.3),
        (0.15, -56.5),
        (0.20, -48.5),
        (0.25, -40.8),
        (0.30, -33.0),
        (0.35, -28.3),
        (0.40, -24.1),
        (0.45, -19.8),
        (0.50, -16.1),
    ] {
        assert!(
            close(fader::db_from_tapered(t), db, 0.051),
            "{t} → {}",
            fader::db_from_tapered(t)
        );
        assert!(
            close(fader::tapered_from_db(db), t, 0.002),
            "{db} → {}",
            fader::tapered_from_db(db)
        );
    }
}

#[test]
fn fader_linear_section() {
    // dB = 55t − 43 above t = 0.5528.
    assert!(close(fader::db_from_tapered(0.6), -10.0, 1e-9));
    assert!(close(fader::tapered_from_db(-6.0), 37.0 / 55.0, 1e-12));
    // Live engine pairs (tapered, dB) read from the Apollo x4 tree.
    for (t, db) in [
        (0.7816363573074341, -0.009999999776482582),
        (0.782672643661499, 0.04699379950761795),
        (0.6114145517349243, -9.372200012207031),
        (0.6727272868156433, -6.0),
    ] {
        assert!(close(fader::db_from_tapered(t), db, 1e-4));
    }
}

#[test]
fn fader_is_monotonic_and_round_trips() {
    let mut last = f64::NEG_INFINITY;
    for i in 0..=10_000 {
        let db = fader::db_from_tapered(i as f64 / 10_000.0);
        assert!(db >= last);
        last = db;
    }
    for i in 0..=224 {
        let db = -100.0 + i as f64 * 0.5;
        assert!(
            close(fader::db_from_tapered(fader::tapered_from_db(db)), db, 0.15),
            "{db}"
        );
    }
}

#[test]
fn preamp_taper() {
    assert_eq!(preamp::db_from_tapered(0.0), 10.0);
    assert_eq!(preamp::db_from_tapered(1.0), 65.0);
    assert_eq!(preamp::db_from_tapered(0.5), 38.0); // 37.5 rounds up
    assert_eq!(preamp::db_from_tapered(0.3148148059844971), 27.0); // live input 0
    assert_eq!(preamp::db_from_tapered(0.5636363636363636), 41.0); // open-apollo state file
    assert!(close(preamp::tapered_from_db(41.0), 31.0 / 55.0, 1e-12));
    assert_eq!(preamp::tapered_from_db(0.0), 0.0);
    for db in 10..=65 {
        assert_eq!(
            preamp::db_from_tapered(preamp::tapered_from_db(db as f64)),
            db as f64
        );
    }
}

#[test]
fn monitor_anchors() {
    for (t, db) in [
        (0.0, -96.0),
        (0.0111, -95.5),
        (0.0278, -75.0),
        (0.0472, -60.0),
        (0.0667, -55.0),
        (0.1583, -40.0),
        (0.325, -20.0),
        (0.4361, -15.0),
        (0.6222, -10.0),
        (0.7694, -6.0),
        (0.8806, -3.0),
        (0.992, 0.0),
        (1.0, 0.0),
    ] {
        assert_eq!(monitor::db_from_tapered(t), db, "t = {t}");
    }
}

#[test]
fn monitor_defaults_and_round_trip() {
    assert_eq!(monitor::db_from_tapered(0.0740741), -55.0); // tree default
    assert_eq!(monitor::db_from_tapered(0.09259258955717087), -52.0); // live
    // Every dB the knob can produce round-trips exactly. From −17 dB up that is every 0.5 dB
    // step; below, the knob jumps (−95.5 → −75 → −60 → −55 → −52 …).
    for code in monitor::codes().iter().collect::<BTreeSet<_>>() {
        let db = (*code as f64 - 192.0) / 2.0;
        assert_eq!(monitor::db_from_tapered(monitor::tapered_from_db(db)), db);
    }
    let mut db = -17.0;
    while db <= 0.0 {
        assert_eq!(monitor::db_from_tapered(monitor::tapered_from_db(db)), db);
        db += 0.5;
    }
    for i in 0..=1000 {
        let v = monitor::db_from_tapered(i as f64 / 1000.0);
        assert_eq!(v % 0.5, 0.0);
    }
}

#[test]
fn pan_labels() {
    assert_eq!(pan::label(0.0), "C");
    assert_eq!(pan::label(-1.0), "L100");
    assert_eq!(pan::label(1.0), "R100");
    assert_eq!(pan::label(0.5), "R50");
    assert_eq!(pan::label(-0.254), "L25");
    assert_eq!(pan::label(0.004), "C");
    assert_eq!(pan::knob_from_pan(0.0), 0.5);
    assert_eq!(pan::pan_from_knob(0.0), -1.0);
    assert!(close(
        pan::pan_from_knob(pan::knob_from_pan(0.3)),
        0.3,
        1e-12
    ));
}

#[test]
fn db_readout() {
    // Rack.dll 0x181bb1294: the sign of the unrounded value, `-∞` below −143.9.
    assert_eq!(tapers::format_db(-144.0), "-\u{221e}");
    assert_eq!(tapers::format_db(-143.95), "-\u{221e}");
    assert_eq!(tapers::format_db(-143.8), "-143.8");
    assert_eq!(tapers::format_db(0.0), "0.0");
    assert_eq!(tapers::format_db(-0.009_999_999_776_482_582), "-0.0");
    assert_eq!(tapers::format_db(0.046_993_799_507_617_95), "+0.0");
    assert_eq!(tapers::format_db(-3.337_860_107_421_875e-6), "-0.0");
    assert_eq!(tapers::format_db(3.04), "+3.0");
    assert_eq!(tapers::format_db(-9.372_200_012_207_031), "-9.4");
    assert_eq!(tapers::format_db(-12.66), "-12.7");
    // Rack.dll 0x1811e8800.
    assert_eq!(tapers::format_monitor_db(-96.0), "-INF");
    // The engine's float −95.9 is just below the double −95.9.
    assert_eq!(tapers::format_monitor_db(-95.9), "-INF");
    assert_eq!(tapers::format_monitor_db(-95.8), "-95.8 dB");
    assert_eq!(tapers::format_monitor_db(-52.0), "-52.0 dB");
    assert_eq!(tapers::format_monitor_db(-0.01), "-0.0 dB");
}
