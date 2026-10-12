//! Property values as the engine sends them.

use std::fmt;

/// A JSON document (the engine's wire format). Objects keep sorted keys.
pub type Json = serde_json::Value;

/// The value of one engine property (`<property>/value`).
///
/// Engine numbers are all JSON numbers (int, int64, float and pointer properties alike), so they
/// decode to [`Value::Number`]. int64 values beyond 2^53 (e.g. `DeviceHwID`) lose precision; none
/// of them are user-facing. Arrays and objects (rare: some string properties carry JSON) are
/// [`Value::Json`].
#[derive(Clone, Debug, PartialEq, Default)]
pub enum Value {
    Number(f64),
    Bool(bool),
    String(String),
    #[default]
    Null,
    Json(Json),
}

impl Value {
    /// Converts a JSON value (arrays and objects become [`Value::Json`]).
    pub fn from_json(json: &Json) -> Value {
        match json {
            Json::Number(n) => Value::Number(n.as_f64().unwrap_or(0.0)),
            Json::Bool(b) => Value::Bool(*b),
            Json::String(s) => Value::String(s.clone()),
            Json::Null => Value::Null,
            Json::Array(_) | Json::Object(_) => Value::Json(json.clone()),
        }
    }

    /// Like [`Value::from_json`], without copying.
    pub fn from_json_owned(json: Json) -> Value {
        match json {
            Json::Number(n) => Value::Number(n.as_f64().unwrap_or(0.0)),
            Json::Bool(b) => Value::Bool(b),
            Json::String(s) => Value::String(s),
            Json::Null => Value::Null,
            j @ (Json::Array(_) | Json::Object(_)) => Value::Json(j),
        }
    }

    /// The JSON form. Integral numbers below 9e15 are written as integers so ints stay ints on disk.
    pub fn to_json(&self) -> Json {
        match self {
            Value::Number(n) => number_json(*n),
            Value::Bool(b) => Json::Bool(*b),
            Value::String(s) => Json::String(s.clone()),
            Value::Null => Json::Null,
            Value::Json(j) => j.clone(),
        }
    }

    /// Numbers, and booleans as 1/0.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Number(n) => Some(*n),
            Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            _ => None,
        }
    }

    pub fn as_f32(&self) -> Option<f32> {
        self.as_f64().map(|d| d as f32)
    }

    /// Rounded to the nearest integer; `None` for non-finite or out-of-range numbers.
    pub fn as_i64(&self) -> Option<i64> {
        let d = self.as_f64()?;
        (d.is_finite() && d.abs() < 9.0e18).then(|| d.round() as i64)
    }

    /// Booleans, and numbers as `!= 0`.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            Value::Number(n) => Some(*n != 0.0),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_json(&self) -> Option<&Json> {
        match self {
            Value::Json(j) => Some(j),
            _ => None,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// Text for `set <path>/value <text>`: strings are JSON-quoted, bools are `true`/`false`,
    /// numbers are bare. `integral` forces integer formatting (for `int`/`int64` properties).
    pub fn wire_text(&self, integral: bool) -> String {
        match self {
            Value::Number(n) => format_number(*n, integral),
            Value::Bool(b) => if *b { "true" } else { "false" }.to_owned(),
            Value::String(s) => Json::String(s.clone()).to_string(),
            Value::Null => "null".to_owned(),
            Value::Json(j) => j.to_string(),
        }
    }
}

/// Bare number text: integers without a fraction, otherwise the shortest round-trip form.
pub(crate) fn format_number(n: f64, integral: bool) -> String {
    if !n.is_finite() {
        return "0".to_owned();
    }
    if integral || (n.round() == n && n.abs() < 1e15) {
        return format!("{}", n.round() as i64);
    }
    format!("{n}")
}

pub(crate) fn number_json(n: f64) -> Json {
    if n.round() == n && n.abs() < 9.0e15 {
        Json::from(n as i64)
    } else {
        serde_json::Number::from_f64(n)
            .map(Json::Number)
            .unwrap_or(Json::Null)
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Number(n) => f.write_str(&format_number(*n, false)),
            Value::Bool(b) => write!(f, "{b}"),
            Value::String(s) => write!(f, "\"{s}\""),
            Value::Null => f.write_str("null"),
            Value::Json(j) => write!(f, "{j}"),
        }
    }
}

impl From<f64> for Value {
    fn from(v: f64) -> Self {
        Value::Number(v)
    }
}
impl From<f32> for Value {
    fn from(v: f32) -> Self {
        Value::Number(v as f64)
    }
}
impl From<i32> for Value {
    fn from(v: i32) -> Self {
        Value::Number(v as f64)
    }
}
impl From<i64> for Value {
    fn from(v: i64) -> Self {
        Value::Number(v as f64)
    }
}
impl From<u32> for Value {
    fn from(v: u32) -> Self {
        Value::Number(v as f64)
    }
}
impl From<bool> for Value {
    fn from(v: bool) -> Self {
        Value::Bool(v)
    }
}
impl From<&str> for Value {
    fn from(v: &str) -> Self {
        Value::String(v.to_owned())
    }
}
impl From<String> for Value {
    fn from(v: String) -> Self {
        Value::String(v)
    }
}
impl From<Json> for Value {
    fn from(v: Json) -> Self {
        Value::from_json_owned(v)
    }
}
