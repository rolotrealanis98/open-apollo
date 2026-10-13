//! The engine side of the Settings window and secondary panels: [`rack::settings`] reads the
//! engine tree through [`PropertySource`] and its writes go out through the store's gated
//! writer (`set` and `post`, suppressed unless writes are allowed) or the client
//! preferences.

use rack::settings::{Choice, PropValue, PropertySource, Write};
use ua_engine::{Json, Value};

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
        if let Some(hp) = path.strip_prefix(rack::settings::HELPER_PREFIX) {
            return match self.helper.as_ref()?.value(hp)? {
                Value::String(s) => Some(PropValue::Str(s.clone())),
                Value::Bool(b) => Some(PropValue::Bool(*b)),
                Value::Number(n) => Some(PropValue::Float(*n)),
                _ => None,
            };
        }
        let integral = self.store.meta(path).is_some_and(|m| m.is_integral());
        self.store.value(path).and_then(|v| to_prop(v, integral))
    }

    fn choices(&self, path: &str) -> Vec<Choice> {
        // NETWORK INTERFACE: the helper's adapter list (`en0  (192.0.2.4)` → `en0`).
        if path == format!("{}{}", rack::settings::HELPER_PREFIX, rack::settings::NIC_LIST_PATH) {
            let Some(h) = &self.helper else { return Vec::new() };
            return h
                .network_adapters()
                .0
                .into_iter()
                .map(|a| Choice { label: a.label(), value: PropValue::Str(a.name), enabled: true, labeled: true })
                .collect();
        }
        let Some(meta) = self.store.meta(path) else { return Vec::new() };
        let integral = meta.is_integral();
        let Some(Json::Array(list)) = &meta.values else { return Vec::new() };
        list.iter()
            .filter_map(|item| match item {
                Json::Object(o) => {
                    let value = to_prop(&Value::from_json(o.get("value")?), integral)?;
                    let label = o.get("string").and_then(Json::as_str).map(str::to_owned).unwrap_or_else(|| value.display());
                    let enabled = o.get("enabled").and_then(Json::as_bool).unwrap_or(true);
                    let labeled = o.get("string").is_some();
                    Some(Choice { value, label, enabled, labeled })
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

    fn default_value(&self, path: &str) -> Option<PropValue> {
        let m = self.store.meta(path)?;
        m.default.as_ref().and_then(|v| to_prop(v, m.is_integral()))
    }
}

impl EngineMixer {
    /// Applies settings writes: `set`s through the gated writer (local only while read-only),
    /// `post`s through the same gate (including suppression diagnostics), client
    /// preferences locally.
    pub(crate) fn write_settings(&mut self, writes: Vec<Write>) {
        for w in writes {
            match w {
                Write::Set(path, v) => self.store.commit(&path, to_value(&v)),
                Write::Post(cmd, payload) => match serde_json::from_str::<Json>(&payload) {
                    Ok(args) => self.store.post(&cmd, Some(&args)),
                    Err(e) => log::warn!("settings: invalid post {cmd}: {e}"),
                },
                Write::Pref(name, v) => {
                    self.prefs.set(&name, v);
                    self.save_prefs();
                }
                // Helper settings go out only while writes are on (the helper connection is
                // read-only otherwise, too).
                Write::Helper(path, v) => {
                    let allowed = self.store.writer().writes_enabled;
                    match self.helper.as_mut() {
                        Some(h) if allowed => {
                            if !h.set(&path, &to_value(&v)) {
                                log::warn!("settings: helper set {path} refused (read-only helper connection)");
                            }
                        }
                        _ => log::info!("settings: helper set {path} suppressed (writes off)"),
                    }
                }
            }
        }
    }
}

impl EngineMixer {
    /// Keeps Console's client preferences (OPTIONS, MIDI) in `path`, as Console keeps its own
    /// preferences file: loads it now (defaults for missing keys) and saves after each change.
    pub fn set_prefs_path(&mut self, path: Option<std::path::PathBuf>) {
        self.prefs_path = path;
        let Some(p) = &self.prefs_path else { return };
        let Ok(text) = std::fs::read_to_string(p) else { return };
        match serde_json::from_str::<Json>(&text) {
            Ok(Json::Object(map)) => {
                for (k, v) in map {
                    let v = match v {
                        Json::Bool(b) => PropValue::Bool(b),
                        Json::Number(n) if n.is_i64() => PropValue::Int(n.as_i64().unwrap_or_default()),
                        Json::Number(n) => PropValue::Float(n.as_f64().unwrap_or_default()),
                        Json::String(s) => PropValue::Str(s),
                        _ => continue,
                    };
                    self.prefs.set(&k, v);
                }
            }
            Ok(_) => log::warn!("prefs: {} is not a JSON object", p.display()),
            Err(e) => log::warn!("prefs: {}: {e}", p.display()),
        }
    }

    fn save_prefs(&self) {
        let Some(p) = &self.prefs_path else { return };
        let map: serde_json::Map<String, Json> = self
            .prefs
            .0
            .iter()
            .map(|(k, v)| {
                let j = match v {
                    PropValue::Bool(b) => Json::Bool(*b),
                    PropValue::Int(i) => Json::from(*i),
                    PropValue::Float(f) => Json::from(*f),
                    PropValue::Str(s) => Json::String(s.clone()),
                };
                (k.clone(), j)
            })
            .collect();
        let text = serde_json::to_string_pretty(&Json::Object(map)).unwrap_or_default();
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = std::fs::write(p, text + "\n") {
            log::warn!("prefs: could not save {}: {e}", p.display());
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
        assert!(labels.starts_with(&["SAMPLE RATE", "CLOCK SOURCE"]), "{labels:?}");
        // The Mac engine has no `/BufferSize`: no BUFFER SIZE, no PT MODE (capture s11).
        assert!(!labels.contains(&"BUFFER SIZE") && !labels.contains(&"PT MODE"));
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
        m.apply_setting("pref:modifiers_timeout", &SettingOp::Text("10".into()));
        assert_eq!(m.properties().unwrap().prop("pref:modifiers_timeout").unwrap().as_str(), Some("10"));
    }
}
