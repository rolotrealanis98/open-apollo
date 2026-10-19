//! Messages from the engine: `{"path":P,"data":V,"parameters":{…},"error":"…"}`.

use crate::value::{Json, Value};

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Message {
    pub path: String,
    /// `None` when the message had no `data` key (errors); `Some(Json::Null)` when it was null.
    pub data: Option<Json>,
    pub parameters: Option<serde_json::Map<String, Json>>,
    pub error: Option<String>,
    /// Human reason for `error` (the open-apollo daemon's `detail`); absent from a UA engine's replies.
    pub detail: Option<String>,
}

impl Message {
    pub fn new(path: impl Into<String>, data: Option<Json>) -> Message {
        Message {
            path: path.into(),
            data,
            parameters: None,
            error: None,
            detail: None,
        }
    }

    /// From a decoded JSON object with a `path` (or `url`) string.
    pub fn from_json(json: Json) -> Option<Message> {
        let Json::Object(mut o) = json else {
            return None;
        };
        let path = match o.remove("path") {
            Some(Json::String(p)) => p,
            _ => match o.remove("url") {
                Some(Json::String(p)) => p,
                _ => return None,
            },
        };
        let parameters = match o.remove("parameters") {
            Some(Json::Object(p)) => Some(p),
            _ => None,
        };
        let error = match o.remove("error") {
            Some(Json::String(e)) => Some(e),
            _ => None,
        };
        let detail = match o.remove("detail") {
            Some(Json::String(d)) if !d.is_empty() => Some(d),
            _ => None,
        };
        Some(Message {
            path,
            data: o.remove("data"),
            parameters,
            error,
            detail,
        })
    }

    /// The error for display: the `detail` reason with the code in parentheses, else the bare code.
    pub fn error_text(&self) -> Option<String> {
        let code = self.error.as_deref()?;
        Some(match &self.detail {
            Some(d) => format!("{d} ({code})"),
            None => code.to_owned(),
        })
    }

    /// Decodes one frame. Non-JSON frames (e.g. a legacy text `set <path> <v>` push) are `None`.
    pub fn decode(frame: &[u8]) -> Option<Message> {
        let first = frame.iter().find(|&&b| b > 0x20)?;
        if *first != b'{' {
            return None;
        }
        Message::from_json(serde_json::from_slice(frame).ok()?)
    }

    /// Correlation id echoed by the engine in `parameters.func_id` (a number, or a string on the
    /// open-apollo daemon).
    pub fn func_id(&self) -> Option<u64> {
        match self.parameters.as_ref()?.get("func_id")? {
            Json::Number(n) => n.as_u64().or_else(|| n.as_f64().map(|f| f as u64)),
            Json::String(s) => s.parse().ok(),
            _ => None,
        }
    }

    /// True for a `multi` envelope (sent after `subscribe mode=multi`).
    pub fn is_multi(&self) -> bool {
        self.path == "multi" && matches!(self.data, Some(Json::Array(_)))
    }

    /// Expands a `multi` envelope into its inner messages; any other message is returned as is.
    pub fn flattened(self) -> Vec<Message> {
        if self.path == "multi"
            && let Some(Json::Array(items)) = self.data
        {
            return items.into_iter().filter_map(Message::from_json).collect();
        }
        vec![self]
    }

    /// The pushed value, for `<property>/value` messages.
    pub fn value(&self) -> Option<Value> {
        self.data.as_ref().map(Value::from_json)
    }

    /// True for `…/value` pushes without an error.
    pub fn is_value_push(&self) -> bool {
        self.error.is_none() && self.data.is_some() && self.path.ends_with("/value")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_text_prefers_the_daemon_detail_and_keeps_the_code() {
        let frame = br#"{"path":"/devices/1/inputs/0/effects/0/EffectName/value",
            "error":"openapollo.plugin_transaction_failed",
            "detail":"This driver does not support stereo native chains"}"#;
        let m = Message::decode(frame).unwrap();
        assert_eq!(
            m.error_text().as_deref(),
            Some("This driver does not support stereo native chains (openapollo.plugin_transaction_failed)")
        );
        let bare = Message::decode(br#"{"path":"/SampleRate","error":"Unable to set value."}"#).unwrap();
        assert_eq!(bare.error_text().as_deref(), Some("Unable to set value."));
        assert_eq!(Message::decode(br#"{"path":"/x","data":1}"#).unwrap().error_text(), None);
    }
}
