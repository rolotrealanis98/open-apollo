//! Optional round trip against a software-only open-apollo daemon:
//!
//! ```sh
//! python3 mixer-engine/ua_mixer_daemon.py --no-hardware --no-helper --no-ws --no-bonjour \
//!     --host 127.0.0.1 --port 14710
//! UA_ENGINE_DAEMON_PORT=14710 cargo test -p ua-engine --test daemon
//! ```
//!
//! It writes. It refuses port 4710, which may be a daemon driving hardware or UA's own engine.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{drain_until, wait_until};
use ua_engine::{
    Command, ConnectionState, Endpoint, Engine, Options, Status, Store, Update, Value, path, tapers,
};

fn daemon_port() -> Option<u16> {
    std::env::var("UA_ENGINE_DAEMON_PORT").ok()?.parse().ok()
}

#[test]
fn daemon_round_trip() {
    let Some(port) = daemon_port() else {
        eprintln!(
            "skipped: set UA_ENGINE_DAEMON_PORT to run against a software-only open-apollo daemon"
        );
        return;
    };
    assert_ne!(port, 4710, "never write to port 4710");
    let endpoint = Endpoint::new("127.0.0.1", port);
    let t = Duration::from_secs(20);
    let mut store = Store::new();
    store.connect(
        endpoint.clone(),
        Options {
            allow_writes: true,
            reconnect: false,
            ..Options::default()
        },
    );
    drain_until(&mut store, "daemon live", t, |s| {
        *s.status() == Status::Live
    });
    assert_eq!(store.device_name(), Some("Apollo x4"));
    assert_eq!(store.topology().monitor.as_ref().map(|m| m.index), Some(18));
    let strip = store.topology().inputs[0].clone();
    let original = store
        .value(&strip.fader())
        .and_then(Value::as_f64)
        .unwrap_or(tapers::fader::UNITY);

    // A second, read-only client observes what the daemon accepted.
    let observer = Engine::connect(
        endpoint,
        Options {
            load_tree: false,
            reconnect: false,
            ..Options::default()
        },
    );
    wait_until("observer", t, || {
        observer.status() == ConnectionState::Ready
    });
    observer
        .send(Command::Subscribe(path::value_path(&strip.fader())))
        .unwrap();
    let last = Arc::new(Mutex::new(f64::NAN));
    let watch = |want: f64, what: &str| {
        let deadline = std::time::Instant::now() + t;
        loop {
            for u in observer.updates().try_iter() {
                if let Update::Values(vs) = u {
                    for (p, v) in vs {
                        if p == strip.fader() {
                            *last.lock().unwrap() = v.as_f64().unwrap_or(f64::NAN);
                        }
                    }
                }
            }
            if (*last.lock().unwrap() - want).abs() < 1e-9 {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for {what} (last {})",
                last.lock().unwrap()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    watch(original, "initial value");

    store.set_writes_enabled(true);
    store.begin_gesture();
    for i in 0..=16 {
        store.set(&strip.fader(), 0.25 + i as f64 / 64.0);
        store.drain();
    }
    store.end_gesture();
    watch(0.5, "daemon pushed 0.5");
    // One gesture of 17 moves: UndoRecording true, at most a few coalesced sets, UndoRecording false.
    assert!(
        store.writer().sent_count() <= 6,
        "{}",
        store.writer().sent_count()
    );

    store.commit(&strip.fader(), original);
    watch(original, "restored");
    store.disconnect();
}
