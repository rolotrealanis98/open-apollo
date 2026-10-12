//! Requests a client sends on 4710 (text mode), as observed in Console traces and verified
//! against UA Mixer Engine 12.0.0.

use crate::path;
use crate::value::{Json, Value};

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    /// `get <path>`; `path` may already carry a `?query`.
    Get { path: String, recursive: bool },
    /// `set <path> <text>`. `path` names the value leaf, e.g.
    /// `/devices/0/inputs/0/FaderLevelTapered/value`.
    Set { path: String, value: String },
    /// `subscribe <path>`.
    Subscribe(String),
    /// `unsubscribe <path>`. Single path only: engine 12.0.0 ignores `unsubscribe {"paths":[…]}`.
    Unsubscribe(String),
    /// `subscribe mode=multi`. After it the engine wraps pushes in `{"path":"multi","data":[…]}`.
    SubscribeMulti,
    /// `subscribe {"paths":[…]}`. The engine answers with one `multi` message carrying the current
    /// values.
    SubscribeBatch(Vec<String>),
    /// `post <command> [<payload>]`.
    Post {
        path: String,
        payload: Option<String>,
    },
    /// A bare identity object, e.g. `{"path":"networkID","data":"<32 hex>"}`.
    Identity { key: String, value: String },
}

impl Command {
    pub fn get(path: impl Into<String>) -> Command {
        Command::Get {
            path: path.into(),
            recursive: false,
        }
    }

    pub fn get_recursive(path: impl Into<String>) -> Command {
        Command::Get {
            path: path.into(),
            recursive: true,
        }
    }

    /// `set <path> <text>` with pre-formatted text.
    pub fn set_raw(path: impl Into<String>, text: impl Into<String>) -> Command {
        Command::Set {
            path: path.into(),
            value: text.into(),
        }
    }

    /// `set <property>/value <v>` with the value formatted for the wire. `integral` writes numbers
    /// without a fraction (for `int`/`int64` properties).
    pub fn set(property: &str, value: &Value, integral: bool) -> Command {
        Command::Set {
            path: path::value_path(property),
            value: value.wire_text(integral),
        }
    }

    pub fn post(path: impl Into<String>, payload: Option<String>) -> Command {
        Command::Post {
            path: path.into(),
            payload,
        }
    }

    pub fn identity(key: impl Into<String>, value: impl Into<String>) -> Command {
        Command::Identity {
            key: key.into(),
            value: value.into(),
        }
    }

    /// The write as Console 1.3.1 puts it on the wire (pcap of Console ↔ UA Mixer Engine 12,
    /// `protocol/v131/console.pcap`; the Windows Console traces agree):
    ///
    /// - `set <prop>/value?context_type=main&func_id=N <v>` for mixer properties;
    /// - `set /UndoRecording/value?func_id=N true|false` (and `/status/timeout_ms`): no context;
    /// - `post /<Command>?func_id=N <payload>`;
    /// - `set /Sleep …` and `post /response …` carry no query.
    ///
    /// `func_id` is the connection's running request counter (shared with `get`). Paths that
    /// already carry a query are left alone, and so is everything that is not a write.
    pub fn with_func_id(self, id: u64) -> Command {
        match self {
            Command::Set { path, value } if !path.contains('?') => {
                let prop = path::property_path(&path);
                let leaf = prop.rsplit('/').next().unwrap_or_default();
                let query = if prop == "/Sleep" || prop == "/sleep" || leaf == "EffectName" || value == "\"default\"" {
                    // No query: per-socket sleep, insert assignment, a reset to the default.
                    return Command::Set { path, value };
                } else if mix_control(prop, leaf) {
                    format!("context_type=main&func_id={id}")
                } else {
                    format!("func_id={id}")
                };
                Command::Set { path: format!("{path}?{query}"), value }
            }
            Command::Post { path, payload } if !path.contains('?') && path != "/response" => {
                Command::Post { path: format!("{path}?func_id={id}"), payload }
            }
            other => other,
        }
    }

    /// The text without Console's per-request query (`func_id`, `context_type`): what a test
    /// compares. Other query parameters (`recursive`, `cmdID`, …) stay.
    pub fn strip_request_query(text: &str) -> String {
        let (verb, rest) = text.split_once(' ').unwrap_or((text, ""));
        let (target, tail) = match rest.split_once(' ') {
            Some((t, v)) => (t, Some(v)),
            None => (rest, None),
        };
        let target = match target.split_once('?') {
            Some((p, q)) => {
                let kept: Vec<&str> = q
                    .split('&')
                    .filter(|kv| !kv.starts_with("func_id=") && !kv.starts_with("context_type="))
                    .collect();
                if kept.is_empty() { p.to_owned() } else { format!("{p}?{}", kept.join("&")) }
            }
            None => target.to_owned(),
        };
        match tail {
            Some(v) => format!("{verb} {target} {v}"),
            None if rest.is_empty() => verb.to_owned(),
            None => format!("{verb} {target}"),
        }
    }

    /// `set /Sleep <bool>`: a per-socket client hint, not mixer state (`UASrv_Server::SetSleepMode`:
    /// `true` slows that socket's pushes to 100 ms and stops counting it as a meter consumer;
    /// default `false`). Read-only connections may send it.
    pub fn is_socket_hint(&self) -> bool {
        matches!(self, Command::Set { path, .. } if path::property_path(path.split('?').next().unwrap_or(path)) == "/Sleep")
    }

    /// `set` and `post` change engine state; everything else is read-only.
    pub fn is_write(&self) -> bool {
        matches!(self, Command::Set { .. } | Command::Post { .. })
    }

    /// The text frame (without its NUL terminator).
    pub fn text(&self) -> String {
        match self {
            Command::Get {
                path,
                recursive: false,
            } => format!("get {path}"),
            Command::Get {
                path,
                recursive: true,
            } => {
                let sep = if path.contains('?') { '&' } else { '?' };
                format!("get {path}{sep}recursive=1")
            }
            Command::Set { path, value } => format!("set {path} {value}"),
            Command::Subscribe(path) => format!("subscribe {path}"),
            Command::Unsubscribe(path) => format!("unsubscribe {path}"),
            Command::SubscribeMulti => "subscribe mode=multi".to_owned(),
            Command::SubscribeBatch(paths) => {
                let paths: Vec<Json> = paths.iter().map(|p| Json::String(p.clone())).collect();
                format!("subscribe {}", serde_json::json!({ "paths": paths }))
            }
            Command::Post {
                path,
                payload: Some(p),
            } => format!("post {path} {p}"),
            Command::Post {
                path,
                payload: None,
            } => format!("post {path}"),
            // Key order as Console writes it: path first.
            Command::Identity { key, value } => {
                format!(
                    "{{\"path\":{},\"data\":{}}}",
                    Json::String(key.clone()),
                    Json::String(value.clone())
                )
            }
        }
    }
}

/// Mixer controls Console writes with `context_type=main`: observed for input `Mute` and `Pan`
/// (1.3.1 pcap), input/aux `FaderLevelTapered` and `CRMonitorLevelTapered` (1.2.x traces). Its
/// other writes (`/UndoRecording`, `RecordPreEffects`, preamp `Phase`) carry only `func_id`.
/// The rest of this list — `Solo`, `Pan2`, `FaderLevel`, `CRMonitorLevel` and the sends'
/// `Gain`/`Pan`/`Bypass` — is a guess by kind (mix controls), not observed.
fn mix_control(prop: &str, leaf: &str) -> bool {
    match leaf {
        "Mute" | "Solo" | "Pan" | "Pan2" | "FaderLevel" | "FaderLevelTapered" | "CRMonitorLevel"
        | "CRMonitorLevelTapered" => !prop.contains("/preamps/"),
        "Gain" | "GainTapered" | "Bypass" => prop.contains("/sends/"),
        _ => false,
    }
}

impl std::fmt::Display for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text())
    }
}
