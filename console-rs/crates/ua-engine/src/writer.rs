//! Writes control changes to the engine (used through [`Store`](crate::Store)).
//!
//! - **Gate.** `writes_enabled` is false by default. While false, writes update the local tree (so
//!   the UI moves) and are counted as suppressed; nothing reaches the engine. The connection has
//!   its own read-only gate underneath ([`Options::allow_writes`](crate::Options::allow_writes)).
//! - **Coalescing.** `set` keeps only the latest value per path and flushes `interval` (16 ms,
//!   ~60 Hz) after the first pending change, in first-touched order.
//! - **Gestures.** `begin_gesture`/`end_gesture` bracket an edit with
//!   `set /UndoRecording/value true` … `false`, as Console does, so it is one undo step. Pending
//!   values are flushed before the bracket opens and before it closes. Nesting is counted.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use crate::command::Command;
use crate::connection::CommandSink;
use crate::path;
use crate::tapers;
use crate::value::Value;

pub const UNDO_RECORDING_PATH: &str = "/UndoRecording";

/// What the writer needs from the model it writes into.
pub(crate) trait LocalModel {
    /// Optimistic local write: applies now and holds back engine echoes.
    fn apply_local(&mut self, path: &str, value: Value);
    fn is_integral(&self, path: &str) -> bool;
    fn has_property(&self, path: &str) -> bool;
}

#[derive(Debug)]
pub struct Writer {
    /// Off by default. Turn on only when the user has asked to control the engine.
    pub writes_enabled: bool,
    /// Coalescing interval (16 ms).
    pub interval: Duration,
    /// When false, nothing is sent until [`Store::flush`](crate::Store::flush) (tests drive
    /// time by hand).
    pub auto_flush: bool,
    /// Update companion read-outs locally (FaderLevel from FaderLevelTapered, …) so offline and
    /// suppressed edits show plausible dB. The engine's own pushes replace them when live.
    pub updates_companions: bool,
    sent: u64,
    suppressed: u64,
    depth: u32,
    undo_open: bool,
    recent_suppressed: VecDeque<String>,
    order: Vec<String>,
    pending: HashMap<String, Value>,
    pending_since: Option<Instant>,
}

impl Default for Writer {
    fn default() -> Self {
        Writer {
            writes_enabled: false,
            interval: Duration::from_millis(16),
            auto_flush: true,
            updates_companions: true,
            sent: 0,
            suppressed: 0,
            depth: 0,
            undo_open: false,
            recent_suppressed: VecDeque::new(),
            order: Vec::new(),
            pending: HashMap::new(),
            pending_since: None,
        }
    }
}

impl Writer {
    pub fn sent_count(&self) -> u64 {
        self.sent
    }

    pub fn suppressed_count(&self) -> u64 {
        self.suppressed
    }

    pub fn gesture_depth(&self) -> u32 {
        self.depth
    }

    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// The most recent suppressed commands (newest last, at most 64), for a dev panel.
    pub fn recent_suppressed(&self) -> impl Iterator<Item = &str> {
        self.recent_suppressed.iter().map(String::as_str)
    }

    /// When the pending values are due to be flushed.
    pub fn deadline(&self) -> Option<Instant> {
        if !self.auto_flush {
            return None;
        }
        self.pending_since.map(|t| t + self.interval)
    }

    pub(crate) fn set(&mut self, model: &mut impl LocalModel, path: &str, value: Value) {
        let p = path::property_path(path).to_owned();
        model.apply_local(&p, value.clone());
        if self.updates_companions {
            apply_companion(model, &p, &value);
        }
        if self.pending.insert(p.clone(), value).is_none() {
            self.order.push(p);
        }
        self.pending_since.get_or_insert_with(Instant::now);
    }

    pub(crate) fn begin_gesture(
        &mut self,
        model: &impl LocalModel,
        sink: Option<&dyn CommandSink>,
    ) {
        self.depth += 1;
        if self.depth != 1 {
            return;
        }
        self.flush(model, sink);
        self.emit(
            sink,
            Command::set(UNDO_RECORDING_PATH, &Value::Bool(true), false),
        );
        self.undo_open = true;
    }

    pub(crate) fn end_gesture(&mut self, model: &impl LocalModel, sink: Option<&dyn CommandSink>) {
        if self.depth == 0 {
            return;
        }
        self.depth -= 1;
        if self.depth != 0 {
            return;
        }
        self.flush(model, sink);
        if self.undo_open {
            self.emit(
                sink,
                Command::set(UNDO_RECORDING_PATH, &Value::Bool(false), false),
            );
            self.undo_open = false;
        }
    }

    /// Sends (or counts as suppressed) every pending value now.
    pub(crate) fn flush(&mut self, model: &impl LocalModel, sink: Option<&dyn CommandSink>) {
        self.pending_since = None;
        let order = std::mem::take(&mut self.order);
        let mut values = std::mem::take(&mut self.pending);
        for p in order {
            let Some(v) = values.remove(&p) else { continue };
            let integral = model.is_integral(&p);
            self.emit(sink, Command::set(&p, &v, integral));
        }
    }

    /// Flushes if the coalescing interval has elapsed.
    pub(crate) fn poll(
        &mut self,
        now: Instant,
        model: &impl LocalModel,
        sink: Option<&dyn CommandSink>,
    ) {
        if let Some(due) = self.deadline()
            && now >= due
        {
            self.flush(model, sink);
        }
    }

    fn emit(&mut self, sink: Option<&dyn CommandSink>, command: Command) {
        match sink {
            Some(sink) if self.writes_enabled => {
                let text = command.text();
                if sink.send_command(command) {
                    self.sent += 1;
                } else {
                    self.suppress(text, "refused by connection");
                }
            }
            _ => {
                let reason = if self.writes_enabled {
                    "no connection"
                } else {
                    "writes disabled"
                };
                self.suppress(command.text(), reason);
            }
        }
    }

    fn suppress(&mut self, text: String, reason: &str) {
        self.suppressed += 1;
        log::debug!("suppressed ({reason}): {text}");
        self.recent_suppressed.push_back(text);
        while self.recent_suppressed.len() > 64 {
            self.recent_suppressed.pop_front();
        }
    }
}

/// The dB companion of a tapered control and its value, if `path` is one.
pub fn companion(path: &str, tapered: f64) -> Option<(String, f64)> {
    let (node, name) = path::split(path);
    let (cname, db) = match name {
        "FaderLevelTapered" => ("FaderLevel", tapers::fader::db_from_tapered(tapered)),
        "GainTapered" if node.ends_with("/preamps/0") => {
            ("Gain", tapers::preamp::db_from_tapered(tapered))
        }
        "GainTapered" if node.contains("/sends/") => {
            ("Gain", tapers::fader::db_from_tapered(tapered))
        }
        "CRMonitorLevelTapered" => ("CRMonitorLevel", tapers::monitor::db_from_tapered(tapered)),
        _ => return None,
    };
    Some((path::join(node, cname), db))
}

/// Local read-out for the dB companion of a tapered control (never sent).
fn apply_companion(model: &mut impl LocalModel, path: &str, value: &Value) {
    let Some(t) = value.as_f64() else { return };
    let Some((cpath, db)) = companion(path, t) else {
        return;
    };
    if model.has_property(&cpath) {
        model.apply_local(&cpath, Value::Number(db));
    }
}
