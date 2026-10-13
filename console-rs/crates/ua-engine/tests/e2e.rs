//! End to end over loopback TCP against MockEngine: real tokio connection, framing, func_id
//! correlation, multi envelopes, reconnect, the write gates.

mod common;

use std::time::{Duration, Instant};

use common::{drain_until, live_x4, wait_until};
use ua_engine::{
    ConnectionState, Endpoint, Engine, Error, MockEngine, MockOptions, Options, Status, Store,
    SubscriptionPlan, Update, Value, tapers,
};

const T: Duration = Duration::from_secs(10);

fn mock(options: MockOptions) -> MockEngine {
    MockEngine::start(live_x4(), options).expect("mock engine")
}

fn live_store(server: &MockEngine, options: Options) -> Store {
    let mut store = Store::new();
    store.connect(server.endpoint(), options);
    drain_until(&mut store, "live", T, |s| *s.status() == Status::Live);
    store
}

#[test]
fn store_goes_live_read_only() {
    let server = mock(MockOptions::default());
    let mut store = live_store(&server, Options::default());
    assert_eq!(store.device_name(), Some("Apollo x4"));
    assert_eq!(store.topology().inputs.len(), 6);
    let plan = SubscriptionPlan::new(store.topology(), store.tree());
    wait_until("batches received", T, || {
        server
            .received_commands()
            .iter()
            .filter(|c| c.starts_with("subscribe {"))
            .count()
            == plan.batches.len()
    });
    let cmds = server.received_commands();
    assert!(
        cmds[0].starts_with(r#"{"path":"networkID","data":""#),
        "{:?}",
        &cmds[..2]
    );
    assert_eq!(cmds[1], r#"{"path":"client_id","data":"uaconsole"}"#);
    // Console's hello (`Options::console_hello`), then the tree.
    assert_eq!(cmds[2], "set /Sleep false");
    assert_eq!(cmds[3], "subscribe /MixerState/value");
    assert!(cmds[4].starts_with("subscribe /errors?func_id=") && cmds[4].ends_with("&recursive=1"), "{}", cmds[4]);
    assert_eq!(cmds[5], "get /errors");
    assert!(
        cmds[6].starts_with("get /?func_id=") && cmds[6].ends_with("&recursive=1"),
        "{}",
        cmds[6]
    );
    assert!(cmds.iter().position(|c| c == "subscribe mode=multi").is_some_and(|i| i > 6));
    assert!(store.subscribed_paths().len() > 3000);
    // Read-only: no write but the per-socket sleep hint.
    assert!(
        !cmds
            .iter()
            .any(|c| (c.starts_with("set ") || c.starts_with("post ")) && c != "set /Sleep false")
    );

    // An engine-side change arrives in a multi envelope and lands in the store.
    let strip = store.topology().inputs[1].clone();
    let toggled = !store
        .value(&strip.mute())
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let rev = store.revision();
    server.push(&strip.mute(), toggled);
    drain_until(&mut store, "mute push", T, |s| {
        s.value(&strip.mute()) == Some(&Value::Bool(toggled))
    });
    assert!(store.changed_since(rev).contains(&strip.mute()));
    assert!(store.connection().unwrap().counters().multi_frames > 0);

    // Meter pushes go to MeterStore, not to the tree.
    let meter = strip.meters[0].clone();
    let before = store.value(&meter.level()).cloned();
    let rev = store.revision();
    server.push(&meter.level(), -20.0);
    drain_until(&mut store, "meter push", T, |s| {
        s.meters().reading(&meter.base).level_db == -20.0
    });
    store.drain();
    assert_eq!(store.value(&meter.level()).cloned(), before);
    assert_eq!(store.revision(), rev);

    // Read-only connection: even with the writer gate open nothing is sent.
    store.set_writes_enabled(true);
    store.commit(&strip.solo(), true);
    std::thread::sleep(Duration::from_millis(100));
    store.drain();
    assert!(server.write_commands().is_empty());
    assert_eq!(store.writer().sent_count(), 0);
    assert_eq!(store.writer().suppressed_count(), 3);
    assert_eq!(store.connection().unwrap().counters().refused_writes, 3);
    // But the local value moved.
    assert_eq!(store.value(&strip.solo()), Some(&Value::Bool(true)));
}

#[test]
fn writes_reach_engine_when_allowed() {
    let server = mock(MockOptions::default());
    let mut store = live_store(
        &server,
        Options {
            allow_writes: true,
            ..Options::default()
        },
    );
    let strip = store.topology().inputs[0].clone();
    store.set_writes_enabled(true);
    store.begin_gesture();
    for i in 0..=16 {
        store.set(&strip.fader(), 0.5 + i as f64 / 64.0);
    }
    store.end_gesture();
    wait_until("sets", T, || server.set_commands().len() == 3);
    assert_eq!(
        server.set_commands(),
        [
            "set /UndoRecording/value true".to_owned(),
            format!("set {}/value 0.75", strip.fader()),
            "set /UndoRecording/value false".to_owned()
        ]
    );
    assert_eq!(server.value(&strip.fader()), Some(Value::Number(0.75)));
    // The engine's companion dB push reaches the read-out.
    let want = tapers::fader::db_from_tapered(0.75);
    drain_until(&mut store, "FaderLevel", T, |s| {
        s.value(&strip.fader_db())
            .and_then(Value::as_f64)
            .is_some_and(|d| (d - want).abs() < 1e-6)
    });
    assert_eq!(store.value(&strip.fader()), Some(&Value::Number(0.75)));

    // Gate closed again: nothing more reaches the engine.
    store.set_writes_enabled(false);
    store.commit(&strip.fader(), 0.1);
    std::thread::sleep(Duration::from_millis(100));
    store.drain();
    assert_eq!(server.set_commands().len(), 3);
    assert_eq!(server.value(&strip.fader()), Some(Value::Number(0.75)));
    assert_eq!(store.value(&strip.fader()), Some(&Value::Number(0.1)));
}

/// Writes carry Console 1.3.1's request query (pcap `protocol/v131/console.pcap`): mixer sets
/// `?context_type=main&func_id=N`, the undo bracket and posts `?func_id=N`, one running
/// counter shared with `get`.
#[test]
fn writes_use_console_wire_format() {
    let server = mock(MockOptions::default());
    let mut store = live_store(
        &server,
        Options {
            allow_writes: true,
            ..Options::default()
        },
    );
    let strip = store.topology().inputs[1].clone();
    store.set_writes_enabled(true);
    store.commit(&strip.mute(), true);
    store.post("/SaveLastUsedSession", Some(&serde_json::json!({})));
    wait_until("writes", T, || server.raw_write_commands().len() == 4);
    let raw = server.raw_write_commands();
    let ids: Vec<u64> = raw
        .iter()
        .map(|c| {
            let q = c.split('?').nth(1).unwrap();
            let id = q.split(['&', ' ']).find_map(|kv| kv.strip_prefix("func_id=")).unwrap();
            id.parse().unwrap()
        })
        .collect();
    assert_eq!(
        raw,
        [
            format!("set /UndoRecording/value?func_id={} true", ids[0]),
            format!("set {}/value?context_type=main&func_id={} true", strip.mute(), ids[1]),
            format!("set /UndoRecording/value?func_id={} false", ids[2]),
            format!("post /SaveLastUsedSession?func_id={} {{}}", ids[3]),
        ]
    );
    assert!(ids.windows(2).all(|w| w[0] < w[1]), "func_id is a running counter: {ids:?}");
    assert_eq!(server.value(&strip.mute()), Some(Value::Bool(true)));

    // Only func_id for non-mix controls (RecordPreEffects, as in the 1.2.x traces); no query
    // for insert assignment or a reset to the default.
    store.commit(&strip.record_pre_effects(), true);
    store.commit(&strip.inserts[0].effect_name(), "UAD 1176");
    store.reset(&strip.pan(), 0.0);
    wait_until("more writes", T, || server.raw_write_commands().len() == 13);
    let raw = server.raw_write_commands();
    let strip_ids = |c: &str| {
        let (path, value) = c.split_once(' ').unwrap().1.split_once(' ').unwrap();
        let query = path.split_once('?').map(|(_, q)| q.split('&').filter(|kv| !kv.starts_with("func_id=")).collect::<Vec<_>>().join("&"));
        (path.split('?').next().unwrap().to_owned(), query, value.to_owned())
    };
    let rec = strip_ids(&raw[5]);
    assert_eq!(rec, (format!("{}/value", strip.record_pre_effects()), Some(String::new()), "true".to_owned()));
    let fx = strip_ids(&raw[8]);
    assert_eq!(fx, (format!("{}/value", strip.inserts[0].effect_name()), None, "\"UAD 1176\"".to_owned()));
    let pan = strip_ids(&raw[11]);
    assert_eq!(pan, (format!("{}/value", strip.pan()), None, "\"default\"".to_owned()));
    assert!(raw[10].starts_with("set /UndoRecording/value?func_id=") && raw[12].starts_with("set /UndoRecording/value?func_id="));
    // The UI shows the default at once.
    assert_eq!(store.value(&strip.pan()), Some(&Value::Number(0.0)));
}

/// A drag is not pulled back by echoes of its own earlier values.
#[test]
fn echoes_do_not_pull_back_a_drag() {
    let server = mock(MockOptions::default());
    let mut store = live_store(
        &server,
        Options {
            allow_writes: true,
            ..Options::default()
        },
    );
    let fader = store.topology().inputs[0].fader();
    store.set_writes_enabled(true);
    store.begin_gesture();
    store.set(&fader, 0.3);
    store.flush();
    store.set(&fader, 0.6); // local value ahead of the engine
    std::thread::sleep(Duration::from_millis(60));
    store.drain(); // the 0.3 echo arrives here and is held
    assert_eq!(store.value(&fader), Some(&Value::Number(0.6)));
    store.end_gesture();
    drain_until(&mut store, "engine settles on 0.6", T, |_| {
        server.value(&fader) == Some(Value::Number(0.6))
    });
    std::thread::sleep(Duration::from_millis(400));
    store.drain();
    assert_eq!(store.value(&fader), Some(&Value::Number(0.6)));
}

#[test]
fn reconnects_and_reloads() {
    let server = mock(MockOptions::default());
    let mut store = live_store(
        &server,
        Options {
            min_backoff: Duration::from_millis(100),
            ..Options::default()
        },
    );
    let tree_rev = store.tree_revision();
    server.drop_clients();
    drain_until(&mut store, "drop noticed", T, |s| {
        *s.status() != Status::Live
    });
    assert!(
        matches!(
            store.status(),
            Status::Reconnecting(_) | Status::Connecting(_) | Status::Loading
        ),
        "{:?}",
        store.status()
    );
    drain_until(&mut store, "live again", T, |s| *s.status() == Status::Live);
    assert_eq!(
        server
            .received_commands()
            .iter()
            .filter(|c| *c == "subscribe mode=multi")
            .count(),
        2
    );
    assert_eq!(store.connection().unwrap().counters().connects, 2);
    assert_eq!(store.tree_revision(), tree_rev + 1);
    assert!(store.subscribed_paths().len() > 3000);
}

#[test]
fn requests_correlate_by_func_id() {
    let server = mock(MockOptions::default());
    let h = Engine::connect(
        server.endpoint(),
        Options {
            load_tree: false,
            // No hello: its /MixerState subscription reply would be an update.
            console_hello: false,
            ..Options::default()
        },
    );
    wait_until("ready", T, || h.status() == ConnectionState::Ready);
    assert_eq!(
        h.get("/devices/0/DeviceName/value", false).unwrap().value(),
        Some(Value::from("Apollo x4"))
    );
    assert_eq!(
        h.get("/devices/0/inputs/0/Stereo/enabled", false)
            .unwrap()
            .value(),
        Some(Value::Bool(true))
    );
    assert_eq!(
        h.get("/devices/0/inputs/0/Name/default", false)
            .unwrap()
            .value(),
        Some(Value::from("ANALOG 1"))
    );
    match h.get("/devices/0/inputs/0/Bogus/value", false) {
        Err(Error::Engine { message, .. }) => {
            assert_eq!(message, "Unable to resolve path for get.")
        }
        other => panic!("{other:?}"),
    }
    // Many in flight at once each get their own reply.
    let paths: Vec<String> = (0..20)
        .map(|i| format!("/devices/0/inputs/{i}/Name/value"))
        .collect();
    let pending: Vec<_> = paths.iter().map(|p| h.request(p, false)).collect();
    let tree = live_x4();
    for (p, r) in paths.iter().zip(pending) {
        let m = r.wait(T).unwrap();
        assert_eq!(m.value().as_ref(), tree.value(p));
    }
    // Replies are consumed by the requester, never delivered as updates.
    assert!(
        !h.updates()
            .try_iter()
            .any(|u| matches!(u, Update::Other(_) | Update::Values(_)))
    );
    // The recursive tree reply echoes parameters.
    let root = h.get("/", true).unwrap();
    assert_eq!(
        root.parameters.as_ref().and_then(|p| p.get("recursive")),
        Some(&serde_json::json!(1))
    );
}

#[test]
fn simulated_meters_arrive_in_multi_envelopes() {
    let server = mock(MockOptions {
        simulate_meters: true,
        ..Default::default()
    });
    let mut store = live_store(&server, Options::default());
    drain_until(&mut store, "subscriptions applied", T, |_| {
        server
            .received_commands()
            .iter()
            .filter(|c| c.starts_with("subscribe {"))
            .count()
            > 40
    });
    store.meters().reset_stats();
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(600) {
        store.drain();
        std::thread::sleep(Duration::from_millis(16));
    }
    let stats = store.meters().stats();
    assert!(stats.pulses > 10, "{stats:?}");
    assert!(stats.pushes > 0);
    let base = &store.topology().inputs[0].meters[0].base;
    assert!(store.meters().reading(base).updates > 0);
    let median = {
        let mut v = stats.pulse_intervals.clone();
        v.sort_by(f64::total_cmp);
        v[v.len() / 2]
    };
    assert!((0.015..0.06).contains(&median), "{median}");
}

/// Retained (on-demand) subscriptions and read-only metadata refresh after an IOType change.
#[test]
fn retained_subscriptions_and_refresh() {
    let server = mock(MockOptions::default());
    let mut store = live_store(&server, Options::default());
    let slot = store.topology().inputs[1].insert(0).unwrap().clone();
    let params: Vec<String> = (0..3)
        .flat_map(|k| slot.parameter(k).subscribed())
        .filter(|p| store.tree().has_property(p))
        .collect();
    assert!(!params.is_empty());
    store.retain_subscriptions(&params);
    store.retain_subscriptions(&params[..1]);
    wait_until("retained batch", T, || {
        server.received_commands().iter().any(|c| {
            c.starts_with("subscribe {") && c.contains("/parameters/0/NormalizedValue/value")
        })
    });
    store.release_subscriptions(&params);
    wait_until("unsubscribe", T, || {
        server
            .received_commands()
            .iter()
            .any(|c| c.starts_with("unsubscribe ") && c.contains("/parameters/"))
    });
    assert_eq!(store.retained_subscriptions().count(), 1); // counted
    let unsubs = server
        .received_commands()
        .iter()
        .filter(|c| c.starts_with("unsubscribe "))
        .count();
    assert_eq!(unsubs, params.len() - 1);

    // An IOType change re-reads the strip with a read-only recursive get.
    let node_rev = store.node_revision();
    server.push("/devices/0/inputs/1/IOType", "Line");
    drain_until(&mut store, "refresh", T, |s| s.node_revision() > node_rev);
    assert!(
        server
            .received_commands()
            .iter()
            .any(|c| c.starts_with("get /devices/0/inputs/1?func_id=")
                && c.ends_with("&recursive=1"))
    );
    assert!(store.refresh("/devices/0/outputs/18"));
    let r = store.node_revision();
    drain_until(&mut store, "explicit refresh", T, |s| s.node_revision() > r);
    assert!(server.write_commands().is_empty());
}

#[test]
fn unreachable_engine_backs_off() {
    // Port 1 on loopback refuses connections.
    let mut store = Store::new();
    store.connect(
        Endpoint::new("127.0.0.1", 1),
        Options {
            min_backoff: Duration::from_millis(50),
            ..Options::default()
        },
    );
    drain_until(&mut store, "reconnecting", T, |s| {
        matches!(s.status(), Status::Reconnecting(_))
    });
    let mut failed = Store::new();
    failed.connect(
        Endpoint::new("127.0.0.1", 1),
        Options {
            reconnect: false,
            ..Options::default()
        },
    );
    drain_until(&mut failed, "failed", T, |s| {
        matches!(s.status(), Status::Failed(_))
    });
    assert!(failed.last_error().is_some());
}

/// The open-apollo daemon's wire style: no multi envelopes, string func_id, errors without
/// parameters. The store must work the same against it.
#[test]
fn daemon_style_engine() {
    let server = MockEngine::start(
        common::device_map("x4"),
        MockOptions {
            daemon_style: true,
            simulate_meters: true,
            ..Default::default()
        },
    )
    .unwrap();
    let mut store = live_store(
        &server,
        Options {
            allow_writes: true,
            ..Options::default()
        },
    );
    assert_eq!(store.topology().inputs.len(), 9);
    // Errors without func_id still resolve the request (matched by path).
    let started = Instant::now();
    match store
        .connection()
        .unwrap()
        .get("/devices/0/inputs/0/Bogus/value", false)
    {
        Err(Error::Engine { message, .. }) => {
            assert_eq!(message, "Unable to resolve path for get.")
        }
        other => panic!("{other:?}"),
    }
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(
        store
            .connection()
            .unwrap()
            .get("/devices/0/inputs/0/Stereo/enabled", false)
            .unwrap()
            .value(),
        Some(Value::Bool(true))
    );
    // Plain pushes reach the store and meters.
    let strip = store.topology().inputs[1].clone();
    server.push(&strip.mute(), true);
    drain_until(&mut store, "plain push", T, |s| {
        s.value(&strip.mute()) == Some(&Value::Bool(true))
    });
    drain_until(&mut store, "meters", T, |s| {
        s.meters().stats().pulses > 3 && s.meters().stats().pushes > 0
    });
    assert_eq!(store.connection().unwrap().counters().multi_frames, 0);
    // Writes and gestures.
    store.set_writes_enabled(true);
    store.commit(&strip.solo(), true);
    wait_until("sets", T, || server.set_commands().len() == 3);
    assert_eq!(server.value(&strip.solo()), Some(Value::Bool(true)));
}

#[test]
fn endpoint_parsing() {
    assert_eq!(
        Endpoint::parse("192.0.2.4"),
        Endpoint::new("192.0.2.4", 4710)
    );
    assert_eq!(
        Endpoint::parse("127.0.0.1:14710"),
        Endpoint::new("127.0.0.1", 14710)
    );
    assert_eq!(Endpoint::parse("[::1]:4710"), Endpoint::new("::1", 4710));
    assert_eq!(
        Endpoint::parse("studio.local").to_string(),
        "studio.local:4710"
    );
}

/// Console's opening (1.2.x traces): after the identity, `set /Sleep false`, `/MixerState` and
/// `/errors` subscriptions; `set /Sleep` is a per-socket hint that read-only connections send.
/// The MIDI input list is polled every second while live.
#[test]
fn console_hello_sleep_hint_and_midi_poll() {
    let server = mock(MockOptions::default());
    let mut store = live_store(&server, Options::default());
    let cmds = server.received_commands();
    let pos = |p: &str| cmds.iter().position(|c| c.starts_with(p)).unwrap_or_else(|| panic!("{p} not in {cmds:?}"));
    assert!(pos("set /Sleep false") < pos("subscribe /MixerState/value"));
    assert!(pos("subscribe /errors?func_id=") < pos("get /?func_id="));
    pos("get /errors");
    store.set_sleep(true);
    wait_until("sleep", T, || server.received_commands().iter().any(|c| c == "set /Sleep true"));
    assert!(server.write_commands().is_empty(), "no mixer writes from a read-only store: {:?}", server.write_commands());
    let polls = || server.received_commands().iter().filter(|c| c.starts_with("get /MidiInputDevice/values?func_id=")).count();
    drain_until(&mut store, "two MIDI polls", Duration::from_secs(5), |_| polls() >= 2);
}
