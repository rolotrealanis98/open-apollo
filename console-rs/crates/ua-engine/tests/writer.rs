mod common;

use std::time::Duration;

use common::{RecordingSink, x4_store};
use ua_engine::{Command, Endpoint, Engine, Error, InputStrip, Options, Store, Value, tapers};

fn make_writer() -> (Store, RecordingSink, InputStrip) {
    let mut store = x4_store();
    let sink = RecordingSink::new();
    store.set_sink(sink.clone());
    store.writer_mut().auto_flush = false;
    let strip = store.topology().inputs[0].clone();
    (store, sink, strip)
}

#[test]
fn writes_are_off_by_default() {
    let (mut store, sink, strip) = make_writer();
    assert!(!store.writer().writes_enabled);
    store.set(&strip.fader(), 0.5);
    store.flush();
    assert!(sink.texts().is_empty());
    assert_eq!(store.writer().suppressed_count(), 1);
    assert_eq!(
        store.writer().recent_suppressed().collect::<Vec<_>>(),
        [format!("set {}/value 0.5", strip.fader())]
    );
    // The local value still moves, and so does its dB read-out.
    assert_eq!(store.value(&strip.fader()), Some(&Value::Number(0.5)));
    let db = store
        .value(&strip.fader_db())
        .and_then(Value::as_f64)
        .unwrap();
    assert!((db - tapers::fader::db_from_tapered(0.5)).abs() < 1e-9);
}

#[test]
fn coalesces_latest_value_per_path() {
    let (mut store, sink, strip) = make_writer();
    store.set_writes_enabled(true);
    for v in [0.1, 0.2, 0.3] {
        store.set(&strip.fader(), v);
    }
    store.set(&strip.mute(), true);
    store.set(&format!("{}/value", strip.fader()), 0.4);
    store.set(&strip.pan(), -0.5);
    assert_eq!(store.writer().pending_count(), 3);
    store.flush();
    assert_eq!(
        sink.texts(),
        [
            format!("set {}/value 0.4", strip.fader()),
            format!("set {}/value true", strip.mute()),
            format!("set {}/value -0.5", strip.pan())
        ]
    );
    store.flush();
    assert_eq!(sink.texts().len(), 3);
    assert_eq!(store.writer().sent_count(), 3);
}

#[test]
fn gesture_brackets_with_undo_recording() {
    let (mut store, sink, strip) = make_writer();
    store.set_writes_enabled(true);
    store.set(&strip.mute(), true); // pending before the gesture: flushed ahead of the bracket
    store.begin_gesture();
    for i in 0..=16 {
        store.set(&strip.fader(), 0.5 + i as f64 / 64.0);
    }
    store.begin_gesture(); // nested: no second bracket
    store.set(&strip.pan(), 0.25);
    store.end_gesture();
    assert_eq!(store.writer().gesture_depth(), 1);
    store.end_gesture();
    assert_eq!(
        sink.texts(),
        [
            format!("set {}/value true", strip.mute()),
            "set /UndoRecording/value true".to_owned(),
            format!("set {}/value 0.75", strip.fader()),
            format!("set {}/value 0.25", strip.pan()),
            "set /UndoRecording/value false".to_owned(),
        ]
    );
    store.end_gesture(); // unbalanced end is ignored
    assert_eq!(store.writer().gesture_depth(), 0);
    assert_eq!(sink.texts().len(), 5);
}

#[test]
fn commit_is_a_one_step_gesture() {
    let (mut store, sink, strip) = make_writer();
    store.set_writes_enabled(true);
    store.commit(&strip.solo(), true);
    assert_eq!(
        sink.texts(),
        [
            "set /UndoRecording/value true".to_owned(),
            format!("set {}/value true", strip.solo()),
            "set /UndoRecording/value false".to_owned()
        ]
    );
}

#[test]
fn gated_gesture_is_suppressed_whole() {
    let (mut store, sink, strip) = make_writer();
    store.begin_gesture();
    store.set(&strip.fader(), 0.6);
    store.end_gesture();
    assert!(sink.texts().is_empty());
    assert_eq!(store.writer().suppressed_count(), 3);
}

#[test]
fn refused_send_counts_as_suppressed() {
    let (mut store, sink, strip) = make_writer();
    store.set_writes_enabled(true);
    *sink.accept.lock().unwrap() = false;
    store.set(&strip.mute(), true);
    store.flush();
    assert_eq!(store.writer().sent_count(), 0);
    assert_eq!(store.writer().suppressed_count(), 1);
}

#[test]
fn integer_properties_are_written_without_fraction() {
    let (mut store, sink, _) = make_writer();
    store.set_writes_enabled(true);
    let dim = store.topology().monitor.as_ref().unwrap().dim_attenuation();
    store.set(&dim, 26.0);
    store.set("/devices/0/inputs/0/sends/0/PostFaderOverride", 1.0);
    store.flush();
    assert_eq!(
        sink.texts(),
        [
            format!("set {dim}/value 26"),
            "set /devices/0/inputs/0/sends/0/PostFaderOverride/value 1".to_owned()
        ]
    );
}

#[test]
fn companions_follow_locally() {
    let (mut store, _, strip) = make_writer();
    let monitor = store.topology().monitor.clone().unwrap();
    store.set(&monitor.level(), 0.4361);
    assert_eq!(
        store.value(&monitor.level_db()),
        Some(&Value::Number(-15.0))
    );
    let preamp = strip.preamp.clone().unwrap();
    store.set(&preamp.gain(), 1.0);
    assert_eq!(store.value(&preamp.gain_db()), Some(&Value::Number(65.0)));
    let send = strip.sends[0].clone();
    store.set(&send.gain(), tapers::fader::UNITY);
    assert!(
        store
            .value(&send.gain_db())
            .and_then(Value::as_f64)
            .unwrap()
            .abs()
            < 1e-9
    );
}

/// `drain()` flushes the coalesced writes about 16 ms after the first pending change.
#[test]
fn auto_flush_runs_at_frame_rate() {
    let (mut store, sink, strip) = make_writer();
    store.writer_mut().auto_flush = true;
    store.set_writes_enabled(true);
    store.set(&strip.fader(), 0.3);
    store.set(&strip.fader(), 0.35);
    store.drain();
    assert!(sink.texts().is_empty(), "flushed before the interval");
    assert!(store.next_deadline().is_some());
    common::drain_until(&mut store, "auto flush", Duration::from_secs(2), |_| {
        !sink.texts().is_empty()
    });
    assert_eq!(sink.texts(), [format!("set {}/value 0.35", strip.fader())]);
}

/// Local writes hold back engine echoes for 350 ms, then apply the latest held value.
#[test]
fn echo_hold() {
    let (mut store, _, strip) = make_writer();
    store.set_echo_hold(Duration::from_millis(120));
    store.set(&strip.fader(), 0.6);
    store.apply([
        (strip.fader(), Value::Number(0.2)),
        (strip.fader(), Value::Number(0.55)),
    ]); // stale echoes
    assert_eq!(store.value(&strip.fader()), Some(&Value::Number(0.6)));
    common::drain_until(&mut store, "hold release", Duration::from_secs(2), |s| {
        s.value(&strip.fader()) == Some(&Value::Number(0.55))
    });
    // Other paths are not held.
    store.apply([(strip.mute(), Value::Bool(true))]);
    assert_eq!(store.value(&strip.mute()), Some(&Value::Bool(true)));
}

/// The connection itself refuses writes unless built with `allow_writes`.
#[test]
fn connection_is_read_only_by_default() {
    let h = Engine::connect(
        Endpoint::new("127.0.0.1", 1),
        Options {
            reconnect: false,
            ..Options::default()
        },
    );
    assert!(!h.options().allow_writes);
    assert!(matches!(
        h.send(Command::set_raw("/devices/0/inputs/0/Mute/value", "true")),
        Err(Error::WriteRefused)
    ));
    assert!(matches!(
        h.send(Command::post("/ClearSolos", None)),
        Err(Error::WriteRefused)
    ));
    assert_eq!(h.counters().refused_writes, 2);
    // Reads are not refused, just not sent while disconnected.
    assert!(matches!(
        h.send(Command::get("/")),
        Err(Error::NotConnected)
    ));
    assert_eq!(h.counters().refused_writes, 2);
    assert_eq!(h.options().network_id.len(), 32);
    assert!(
        h.options()
            .network_id
            .chars()
            .all(|c| c.is_ascii_hexdigit())
    );
    assert_ne!(h.options().network_id, Options::default().network_id);
    assert_eq!(h.options().client_id, "uaconsole");
}
