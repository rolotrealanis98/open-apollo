//! The engine side of the Settings window and secondary panels: [`rack::settings`] reads the
//! engine tree through [`PropertySource`] and its writes go out through the store's gated
//! writer (`set`), the connection (`post`, refused unless writes are allowed) or the client
//! preferences.

use rack::settings::{Choice, PropValue, PropertySource, Write};
use ua_engine::{Command, Json, Value};

use crate::EngineMixer;

/// An engine value as settings see it (`int`/`int64` numbers stay integers).
fn to_prop(v: &Value, integral: bool) -> Option<PropValue> {
    match v {
        Value::Number(n) if integral => Some(PropValue::Int(n.round() as i64)),
        Value::Number(n) => Some(PropValue::Float(*n)),
        Value::Bool(b) => Some(PropValue::Bool(*b)),
        Value::String(s) => Some(PropValue::Str(s.clone())),
        Value::Null => None,
        Value::Json(j) => Some(PropValue::Str(j.to_string())),
    }
}

fn to_value(v: &PropValue) -> Value {
    match v {
        PropValue::Bool(b) => Value::Bool(*b),
        PropValue::Int(i) => Value::Number(*i as f64),
        PropValue::Float(f) => Value::Number(*f),
        PropValue::Str(s) => Value::String(s.clone()),
    }
}

impl PropertySource for EngineMixer {
    fn prop(&self, path: &str) -> Option<PropValue> {
        if let Some(name) = path.strip_prefix("pref:") {
            return self.prefs.get(name);
        }
        let integral = self.store.meta(path).is_some_and(|m| m.is_integral());
        self.store.value(path).and_then(|v| to_prop(v, integral))
    }

    fn choices(&self, path: &str) -> Vec<Choice> {
        let Some(meta) = self.store.meta(path) else { return Vec::new() };
        let integral = meta.is_integral();
        let Some(Json::Array(list)) = &meta.values else { return Vec::new() };
        list.iter()
            .filter_map(|item| match item {
                Json::Object(o) => {
                    let value = to_prop(&Value::from_json(o.get("value")?), integral)?;
                    let label = o.get("string").and_then(Json::as_str).map(str::to_owned).unwrap_or_else(|| value.display());
                    let enabled = o.get("enabled").and_then(Json::as_bool).unwrap_or(true);
                    Some(Choice { value, label, enabled })
                }
                other => to_prop(&Value::from_json(other), integral).map(Choice::new),
            })
            .collect()
    }

    fn indices(&self, node: &str) -> Vec<usize> {
        self.store.tree().indices(node)
    }

    fn range(&self, path: &str) -> Option<(f64, f64)> {
        let m = self.store.meta(path)?;
        Some((m.min?, m.max?))
    }

    fn readonly(&self, path: &str) -> bool {
        self.store.meta(path).is_some_and(|m| m.readonly)
    }
}

impl EngineMixer {
    /// Applies settings writes: `set`s through the gated writer (local only while read-only),
    /// `post`s only with writes enabled (the connection refuses them otherwise too), client
    /// preferences locally.
    pub(crate) fn write_settings(&mut self, writes: Vec<Write>) {
        for w in writes {
            match w {
                Write::Set(path, v) => self.store.commit(&path, to_value(&v)),
                Write::Post(cmd, payload) => {
                    if !self.store.writer().writes_enabled {
                        log::info!("settings: read-only, not sending post {cmd} {payload}");
                        continue;
                    }
                    if let Some(c) = self.store.connection() {
                        if let Err(e) = c.send(Command::post(cmd.clone(), Some(payload))) {
                            log::warn!("settings: post {cmd}: {e}");
                        }
                    }
                }
                Write::Pref(name, v) => self.prefs.set(&name, v),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use rack::MixerModel;
    use rack::settings::{self, SettingOp, SettingsGroup};

    use crate::EngineMixer;

    fn mixer() -> EngineMixer {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../ua-engine/tests/fixtures/apollo-x4-12.0.0.snapshot.json");
        EngineMixer::from_snapshot(path).expect("fixture")
    }

    #[test]
    fn hardware_reads_the_engine_tree() {
        let m = mixer();
        let src = m.properties().expect("properties");
        let f = settings::fields(src, SettingsGroup::Hardware);
        let labels: Vec<&str> = f.iter().map(|f| f.label.as_str()).collect();
        assert!(labels.starts_with(&["SAMPLE RATE", "CLOCK SOURCE", "BUFFER SIZE"]), "{labels:?}");
        // The rate combo carries the engine's labels ("48 kHz").
        let sr = &f[0];
        assert!(sr.value_text().ends_with("kHz"), "{}", sr.value_text());
        assert!(!settings::devices(src).is_empty());
    }

    #[test]
    fn choices_write_through_the_store() {
        let mut m = mixer();
        let src = m.properties().unwrap();
        let rates = src.choices("/SampleRate");
        assert!(rates.len() >= 2);
        let target = rates.iter().position(|c| !c.value.same(&src.prop("/SampleRate").unwrap())).unwrap();
        let want = rates[target].value.clone();
        m.apply_setting("/SampleRate", &SettingOp::Choose(target));
        // Read-only store: the value moves locally (the writer suppresses the wire write).
        assert!(m.properties().unwrap().prop("/SampleRate").unwrap().same(&want));
        // Preferences stay in the client.
        m.apply_setting("pref:modifiers_timeout", &SettingOp::Choose(1));
        assert_eq!(m.properties().unwrap().prop("pref:modifiers_timeout").unwrap().as_str(), Some("5 SEC"));
    }
}
