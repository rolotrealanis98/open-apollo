//! open-apollo's mixer daemon over a recording fake backend (`mixer-engine/tests/fake_hw_daemon.py`),
//! started on a private loopback port for end-to-end tests: what the console sends, what the
//! daemon does with it, and which driver / mixer calls reach the (fake) hardware.
//!
//! The daemon never opens a device, ALSA or PipeWire. Tests skip themselves when Python or the
//! daemon is not available (Nix sandbox); `OA_REQUIRE_DAEMON_E2E=1` makes that a failure.

#![allow(dead_code)]

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use engine_model::EngineMixer;
use rack::MixerModel;
use serde_json::Value as Json;
use ua_engine::{Endpoint, Status};

/// Ports a real daemon uses (4710 / 4720 / 4721 and +100 per device): never connected to.
pub const FORBIDDEN: std::ops::Range<u16> = 4700..6000;

pub struct FakeDaemon {
    child: Child,
    pub port: u16,
    dir: PathBuf,
    log: PathBuf,
}

impl Drop for FakeDaemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn skip(why: &str) -> Option<FakeDaemon> {
    if std::env::var_os("OA_REQUIRE_DAEMON_E2E").is_some() {
        panic!("fake daemon unavailable: {why}");
    }
    eprintln!("skipped: fake daemon unavailable ({why})");
    None
}

impl FakeDaemon {
    /// Starts the daemon (extra args: `--refuse-rate 192000`, …). `None` (test skipped) when
    /// Python or the daemon sources are missing.
    pub fn start(extra: &[&str]) -> Option<FakeDaemon> {
        let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../mixer-engine/tests/fake_hw_daemon.py");
        if !script.exists() {
            return skip("no mixer-engine next to console-rs");
        }
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("oa-fake-daemon-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("home")).ok()?;
        let log = dir.join("calls.jsonl");
        let python = std::env::var("OA_TEST_PYTHON").unwrap_or_else(|_| "python3".into());
        let child = Command::new(&python)
            .arg(&script)
            .arg("--log")
            .arg(&log)
            .args(extra)
            .env("HOME", dir.join("home"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn();
        let mut child = match child {
            Ok(c) => c,
            Err(e) => {
                let _ = std::fs::remove_dir_all(&dir);
                return skip(&format!("{python}: {e}"));
            }
        };
        let stdout = child.stdout.take().expect("piped stdout");
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Some(p) = line.strip_prefix("LISTENING ") {
                    let _ = tx.send(p.trim().parse::<u16>().ok());
                }
            }
        });
        let port = match rx.recv_timeout(Duration::from_secs(20)) {
            Ok(Some(p)) => p,
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_dir_all(&dir);
                return skip("the daemon did not start");
            }
        };
        assert!(!FORBIDDEN.contains(&port), "port {port} belongs to a real daemon");
        Some(FakeDaemon { child, port, dir, log })
    }

    /// A console connected to it, with writes on, once the engine tree is loaded.
    pub fn console(&self) -> EngineMixer {
        self.console_with(true)
    }

    pub fn console_with(&self, writes: bool) -> EngineMixer {
        assert!(!FORBIDDEN.contains(&self.port));
        let mut m = EngineMixer::connect(Endpoint::new("127.0.0.1", self.port), writes);
        pump_until(&mut m, "engine tree", |m| *m.store().status() == Status::Live && !m.inputs().is_empty());
        settle(&mut m, 300);
        m
    }

    /// Every log entry so far.
    pub fn entries(&self) -> Vec<Json> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    /// A position in the log: entries after it are what happened since.
    pub fn mark(&self) -> usize {
        self.entries().len()
    }

    pub fn since(&self, mark: usize) -> Vec<Json> {
        self.entries().into_iter().skip(mark).collect()
    }

    /// Hardware calls since `mark`: `(method, args)`.
    pub fn hw_since(&self, mark: usize) -> Vec<(String, Vec<Json>)> {
        self.since(mark)
            .into_iter()
            .filter_map(|e| Some((e.get("hw")?.as_str()?.to_owned(), e.get("args")?.as_array()?.clone())))
            .collect()
    }

    /// Published tree changes since `mark`: `(path, value)`.
    pub fn tree_since(&self, mark: usize) -> Vec<(String, Json)> {
        self.since(mark)
            .into_iter()
            .filter_map(|e| Some((e.get("tree")?.as_str()?.to_owned(), e.get("value")?.clone())))
            .collect()
    }

    /// Pumps `m` until a hardware call since `mark` satisfies `pred` (or 5 s pass); returns the
    /// calls since `mark`.
    pub fn wait_hw(
        &self,
        m: &mut EngineMixer,
        mark: usize,
        what: &str,
        pred: impl Fn(&str, &[Json]) -> bool,
    ) -> Vec<(String, Vec<Json>)> {
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            m.pump();
            let calls = self.hw_since(mark);
            if calls.iter().any(|(n, a)| pred(n, a)) {
                return calls;
            }
            assert!(
                Instant::now() < end,
                "no hardware call for {what}; calls since: {calls:?}\ntree since: {:?}",
                self.tree_since(mark)
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Pumps `m` until the daemon published `path` = `want` since `mark`.
    pub fn wait_tree(&self, m: &mut EngineMixer, mark: usize, path: &str, want: &Json) {
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            m.pump();
            if self.tree_since(mark).iter().any(|(p, v)| p == path && v == want) {
                return;
            }
            assert!(
                Instant::now() < end,
                "daemon never published {path} = {want}; tree since: {:?}",
                self.tree_since(mark)
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

/// `set_mixer_param(type, ch, param, value)` as numbers.
pub fn mixer_param(name: &str, args: &[Json]) -> Option<(i64, i64, i64, i64)> {
    if name != "set_mixer_param" {
        return None;
    }
    let n = |i: usize| args.get(i).and_then(Json::as_i64);
    Some((n(0)?, n(1)?, n(2)?, n(3)?))
}

pub fn pump_until(m: &mut EngineMixer, what: &str, ok: impl Fn(&EngineMixer) -> bool) {
    let end = Instant::now() + Duration::from_secs(15);
    while !ok(m) {
        assert!(Instant::now() < end, "timed out: {what}");
        m.pump();
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Pumps for `ms` milliseconds (echo holds expire after 350 ms).
pub fn settle(m: &mut EngineMixer, ms: u64) {
    let end = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < end {
        m.pump();
        std::thread::sleep(Duration::from_millis(10));
    }
}
