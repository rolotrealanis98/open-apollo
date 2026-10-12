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

impl std::fmt::Display for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text())
    }
}
