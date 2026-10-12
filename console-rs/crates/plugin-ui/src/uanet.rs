//! The UANet dialect UA's HTML GUIs speak over their WebSocket (WEBBRIDGE.md §3–5), answered
//! from a [`ParamSource`]. Pure logic: feed it request frames, send back what it returns.
//!
//! The GUI addresses controls by string id (`<P>/controls/<id>/normalized_value/value`); the
//! engine by ordinal (`<slot>/parameters/<k>/NormalizedValue`). [`ControlMap`] joins them.
//!
//! **Write gate.** With `writes_enabled` false, `set`s are refused with an error reply and the
//! current value is pushed back, so the GUI's optimistic move snaps back. Nothing unknown is ever
//! forwarded.

use std::collections::{BTreeMap, HashMap};

use serde_json::{Value as Json, json};

use crate::params::{ParamSource, clamp01, step_index, step_value};

/// One request frame: `<verb> <path>[?<query>][ <data>]`.
#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    pub verb: String,
    pub path: String,
    pub params: Vec<(String, String)>,
    pub data: Option<Json>,
}

impl Request {
    pub fn parse(frame: &str) -> Option<Self> {
        let frame = frame.trim_end_matches('\0').trim();
        let (verb, rest) = frame.split_once(' ').unwrap_or((frame, ""));
        let (target, data) = match rest.split_once(' ') {
            Some((t, d)) => (t, Some(d.trim())),
            None => (rest, None),
        };
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        if verb.is_empty() || path.is_empty() {
            return None;
        }
        let params = query
            .split('&')
            .filter(|kv| !kv.is_empty())
            .map(|kv| {
                let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
                (percent_decode(k), percent_decode(v))
            })
            .collect();
        // Values are JSON, or a bare "string" written without escaping.
        let data = data.filter(|d| !d.is_empty()).map(|d| {
            serde_json::from_str(d).unwrap_or_else(|_| Json::String(d.trim_matches('"').to_string()))
        });
        Some(Self { verb: verb.to_ascii_lowercase(), path: path.to_string(), params, data })
    }

    pub fn param(&self, key: &str) -> Option<&str> {
        self.params.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    fn echo(&self) -> Json {
        Json::Object(self.params.iter().map(|(k, v)| (k.clone(), Json::String(v.clone()))).collect())
    }
}

/// GUI control id ↔ parameter ordinal.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ControlMap {
    /// In GUI order; `None` = a GUI control with no engine parameter (kept local).
    pub controls: Vec<(String, Option<u16>)>,
    /// LUNA meter ids (`<P>/meters/<id>/level/value`).
    pub meters: Vec<String>,
}

impl ControlMap {
    /// Joins GUI ids to parameter names: `overrides` first, then equal names after
    /// normalisation ("SC Filter" = `sc_filter`), then a unique prefix either way
    /// ("Threshold" ~ `thresh`, "Make Up" ~ `make_up`).
    pub fn build(gui_ids: &[String], params: &dyn ParamSource, overrides: &BTreeMap<String, u16>, meters: Vec<String>) -> Self {
        let names: Vec<(u16, String)> = params.indices().into_iter().map(|k| (k, norm(&params.name(k).unwrap_or_default()))).collect();
        let controls = gui_ids
            .iter()
            .map(|id| {
                let n = norm(id);
                let k = overrides.get(id).copied().or_else(|| names.iter().find(|(_, name)| *name == n).map(|(k, _)| *k)).or_else(|| {
                    let hits: Vec<u16> = names
                        .iter()
                        .filter(|(_, name)| n.len() >= 3 && name.len() >= 3 && (name.starts_with(&n) || n.starts_with(name.as_str())))
                        .map(|(k, _)| *k)
                        .collect();
                    (hits.len() == 1).then(|| hits[0])
                });
                (id.clone(), k)
            })
            .collect();
        Self { controls, meters }
    }

    /// Every parameter as a control named by its normalized name (no GUI ids known).
    pub fn from_params(params: &dyn ParamSource) -> Self {
        let controls = params.indices().into_iter().map(|k| (slug(&params.name(k).unwrap_or_else(|| format!("p{k}"))), Some(k))).collect();
        Self { controls, meters: vec![] }
    }

    pub fn ordinal(&self, id: &str) -> Option<u16> {
        self.controls.iter().find(|(c, _)| c == id).and_then(|(_, k)| *k)
    }
}

/// GUI control ids a bundle binds (`"ua-param":"<id>"`), in first-seen order.
pub fn scrape_control_ids(bundle_js: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for marker in ["\"ua-param\":\"", "registerCallbackListener({id:\""] {
        let mut rest = bundle_js;
        while let Some(i) = rest.find(marker) {
            rest = &rest[i + marker.len()..];
            let id: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
            if !id.is_empty() && !out.contains(&id) {
                out.push(id);
            }
        }
    }
    out
}

fn norm(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_alphanumeric()).flat_map(|c| c.to_lowercase()).collect()
}

fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if !out.ends_with('_') && !out.is_empty() {
            out.push('_');
        }
    }
    out.trim_end_matches('_').to_string()
}

#[derive(Clone, Debug, Default, PartialEq)]
struct Local {
    value: f64,
}

/// Effect-level state the GUI shows (from the slot).
#[derive(Clone, Debug, PartialEq)]
pub struct EffectState {
    pub preset_name: String,
    pub power: bool,
    pub stereo: bool,
}

impl Default for EffectState {
    fn default() -> Self {
        Self { preset_name: "Default".into(), power: true, stereo: false }
    }
}

/// One GUI connection's state.
pub struct UaNetBridge {
    /// `<P>`: the slot path the page was opened with.
    pub base: String,
    pub map: ControlMap,
    pub writes_enabled: bool,
    /// Spec defaults for `set_control_default_value`.
    pub defaults: HashMap<u16, f64>,
    pub effect: EffectState,
    /// The GUI has posted `notify_gui_loaded` (show the view).
    pub loaded: bool,
    local: HashMap<String, Local>,
    pushed: HashMap<String, Json>,
    touches: u32,
}

impl UaNetBridge {
    pub fn new(base: impl Into<String>, map: ControlMap) -> Self {
        Self {
            base: base.into().trim_end_matches('/').to_string(),
            map,
            writes_enabled: false,
            defaults: HashMap::new(),
            effect: EffectState::default(),
            loaded: false,
            local: HashMap::new(),
            pushed: HashMap::new(),
            touches: 0,
        }
    }

    fn control_path(&self, id: &str, prop: &str) -> String {
        format!("{}/controls/{id}/{prop}/value", self.base)
    }

    /// `(normalized, real, string, steps)` of a control.
    fn control_values(&self, id: &str, params: &dyn ParamSource) -> (f64, Json, String, Vec<String>) {
        match self.map.ordinal(id) {
            Some(k) => {
                let v = params.normalized(k).unwrap_or(0.0);
                let steps = params.steps(k);
                let real = if steps.len() > 1 { json!(step_index(v, steps.len())) } else { json!(v) };
                let text = params.display(k).unwrap_or_else(|| format!("{:.2}", v));
                (v, real, text, steps)
            }
            None => {
                let v = self.local.get(id).map(|l| l.value).unwrap_or(0.0);
                (v, json!(v), format!("{:.2}", v), vec![])
            }
        }
    }

    fn reply(req: &Request, data: Json) -> String {
        json!({"path": req.path, "data": data, "parameters": req.echo()}).to_string()
    }

    fn error(req: &Request, msg: &str) -> String {
        json!({"path": req.path, "error": msg, "parameters": req.echo()}).to_string()
    }

    /// The `get_base_path_subtree` reply body (flat properties, `children` arrays).
    pub fn subtree(&self, params: &dyn ParamSource) -> Json {
        let controls: Vec<Json> = self
            .map
            .controls
            .iter()
            .map(|(id, k)| {
                let (v, real, text, steps) = self.control_values(id, params);
                let name = k.and_then(|k| params.name(k)).unwrap_or_else(|| id.clone());
                json!({
                    "path": id,
                    "properties": {
                        "type": "effect_control", "name": name, "normalized_value": v, "real_value": real,
                        "string_value": text, "num_steps": if steps.len() > 1 { steps.len() } else { 0 },
                        "discrete": steps.len() > 1, "real_values": (0..steps.len()).collect::<Vec<_>>(),
                        "string_values": steps, "touch_count": 0
                    },
                    "commands": ["set_touch", "set_control_default_value", "get_string_for_normalized_value"]
                })
            })
            .collect();
        let meters: Vec<Json> = self.map.meters.iter().map(|m| json!({"path": m, "properties": {"type": "meter", "level": [0.0]}})).collect();
        let stem = if self.effect.stereo { "stereo" } else { "mono" };
        json!({
            "properties": {
                "type": "effect", "preset_name": self.effect.preset_name, "preset_uid": "", "preset_uid_dirty": false,
                "plugin_power": if self.effect.power { 1 } else { 0 }, "stem_format": stem, "output_stem_format": stem
            },
            "children": [
                {"path": "commands", "properties": {}, "commands": ["notify_gui_loaded", "open_link", "set_preset"]},
                {"path": "controls", "properties": {}, "children": controls},
                {"path": "meters", "properties": {}, "children": meters}
            ]
        })
    }

    /// Handles one frame from the GUI; returns the frames to send back.
    pub fn handle(&mut self, frame: &str, params: &mut dyn ParamSource) -> Vec<String> {
        let Some(req) = Request::parse(frame) else { return vec![] };
        let rel = req.path.strip_prefix(&self.base).map(|r| r.trim_start_matches('/').to_string());
        match (req.verb.as_str(), rel.as_deref()) {
            ("get", _) if req.path == "/status" => vec![Self::reply(&req, json!({}))],
            ("get", Some("")) => {
                let data = self.subtree(params);
                self.remember(params);
                vec![Self::reply(&req, data)]
            }
            ("get", Some(r)) => match self.read(r, params) {
                Some(v) => vec![Self::reply(&req, v)],
                None => vec![Self::error(&req, "no such path")],
            },
            ("subscribe" | "unsubscribe", _) => vec![Self::reply(&req, json!(true))],
            ("set", Some(r)) => self.set(&req, r, params),
            ("post", Some(r)) => self.post(&req, r, params),
            ("post", None) => vec![Self::reply(&req, json!({}))], // /logs/send_log and friends: ack only
            _ => vec![Self::error(&req, "unsupported")],
        }
    }

    fn read(&self, rel: &str, params: &dyn ParamSource) -> Option<Json> {
        let parts: Vec<&str> = rel.split('/').collect();
        match parts.as_slice() {
            ["controls", id, prop, ..] => {
                let (v, real, text, _) = self.control_values(id, params);
                Some(match *prop {
                    "normalized_value" => json!(v),
                    "real_value" => real,
                    "string_value" => json!(text),
                    _ => return None,
                })
            }
            ["preset_name", ..] => Some(json!(self.effect.preset_name)),
            ["plugin_power", ..] => Some(json!(if self.effect.power { 1 } else { 0 })),
            _ => None,
        }
    }

    fn set(&mut self, req: &Request, rel: &str, params: &mut dyn ParamSource) -> Vec<String> {
        let parts: Vec<&str> = rel.split('/').collect();
        let ["controls", id, prop, ..] = parts.as_slice() else { return vec![Self::error(req, "unsupported")] };
        let Some(x) = req.data.as_ref().and_then(Json::as_f64) else { return vec![Self::error(req, "no value")] };
        let k = self.map.ordinal(id);
        let normalized = match (*prop, k) {
            ("normalized_value", _) => clamp01(x),
            ("real_value", Some(k)) => {
                let n = params.steps(k).len();
                if n > 1 { step_value(x.round().max(0.0) as usize, n) } else { clamp01(x) }
            }
            _ => return vec![Self::error(req, "unsupported")],
        };
        match k {
            None => {
                self.local.insert(id.to_string(), Local { value: normalized });
                vec![Self::reply(req, json!(x))]
            }
            Some(_) if !self.writes_enabled => {
                let mut out = vec![Self::error(req, "writes disabled")];
                out.extend(self.push_control(id, params, true));
                out
            }
            Some(k) => {
                if self.touches == 0 {
                    params.commit(k, normalized);
                } else {
                    params.set(k, normalized);
                }
                vec![Self::reply(req, json!(x))]
            }
        }
    }

    fn post(&mut self, req: &Request, rel: &str, params: &mut dyn ParamSource) -> Vec<String> {
        let parts: Vec<&str> = rel.split('/').collect();
        match parts.as_slice() {
            ["commands", "notify_gui_loaded"] => {
                self.loaded = true;
                vec![Self::reply(req, json!({}))]
            }
            ["controls", _, "set_touch"] => {
                let touch = req.data.as_ref().and_then(|d| d.get("touch")).and_then(Json::as_bool).unwrap_or(false);
                if self.writes_enabled {
                    if touch {
                        self.touches += 1;
                        params.begin_gesture();
                    } else if self.touches > 0 {
                        self.touches -= 1;
                        params.end_gesture();
                    }
                }
                vec![Self::reply(req, json!({}))]
            }
            ["controls", id, "set_control_default_value"] => match self.map.ordinal(id).and_then(|k| self.defaults.get(&k).map(|d| (k, *d))) {
                Some((k, d)) if self.writes_enabled => {
                    params.commit(k, d);
                    vec![Self::reply(req, json!({}))]
                }
                _ => {
                    let mut out = vec![Self::error(req, "writes disabled")];
                    out.extend(self.push_control(id, params, true));
                    out
                }
            },
            ["controls", id, cmd] if cmd.starts_with("get_string_for") => {
                let (_, _, text, _) = self.control_values(id, params);
                vec![Self::reply(req, json!(text))]
            }
            _ => vec![Self::reply(req, json!({}))],
        }
    }

    /// Records the values the subtree just sent, so later pushes carry only changes.
    fn remember(&mut self, params: &dyn ParamSource) {
        let ids: Vec<String> = self.map.controls.iter().map(|(id, _)| id.clone()).collect();
        for id in ids {
            self.push_control(&id, params, true);
        }
    }

    /// Value pushes for one control (no `cmd_id`): only changed properties unless `force`.
    fn push_control(&mut self, id: &str, params: &dyn ParamSource, force: bool) -> Vec<String> {
        let (v, real, text, _) = self.control_values(id, params);
        let mut out = Vec::new();
        for (prop, value) in [("normalized_value", json!(v)), ("real_value", real), ("string_value", json!(text))] {
            let path = self.control_path(id, prop);
            if force || self.pushed.get(&path) != Some(&value) {
                out.push(json!({"path": path, "data": value}).to_string());
                self.pushed.insert(path, value);
            }
        }
        out
    }

    /// Pushes for every control whose value changed since the last push (engine updates).
    pub fn pushes(&mut self, params: &dyn ParamSource) -> Vec<String> {
        let ids: Vec<String> = self.map.controls.iter().map(|(id, _)| id.clone()).collect();
        ids.iter().flat_map(|id| self.push_control(id, params, false)).collect()
    }

    /// A meter push (`level` is an array; the GUI uses `[0]`).
    pub fn meter_push(&self, meter: &str, level: f64) -> String {
        json!({"path": format!("{}/meters/{meter}/level/value", self.base), "data": [level]}).to_string()
    }
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => match u8::from_str_radix(&s[i + 1..i + 3], 16) {
                Ok(v) => {
                    out.push(v);
                    i += 3;
                    continue;
                }
                Err(_) => out.push(b'%'),
            },
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::{MockEvent, MockParam, MockParams};

    const P: &str = "/devices/0/inputs/2/effects/3";

    fn params() -> MockParams {
        let p = |name: &str, value: f64, steps: &[&str]| MockParam { name: name.into(), value, steps: steps.iter().map(|s| s.to_string()).collect() };
        MockParams::new(vec![p("Input", 0.5, &[]), p("Threshold", 0.2, &[]), p("Ratio", 0.1, &["None", "4:1", "8:1", "12:1", "20:1", "a", "b", "c", "d", "e", "f"]), p("SC Filter", 0.0, &["Off", "On"])])
    }

    fn bridge(params: &MockParams) -> UaNetBridge {
        let ids: Vec<String> = ["input", "thresh", "ratio", "sc_filter", "fade"].iter().map(|s| s.to_string()).collect();
        UaNetBridge::new(P, ControlMap::build(&ids, params, &BTreeMap::new(), vec!["vu".into()]))
    }

    #[test]
    fn parses_requests() {
        let r = Request::parse("set /a/controls/attack/normalized_value/value?cmd_id=set&cmd_listener=%24PATH%3Ax&message_id=U%3A5 0.42").unwrap();
        assert_eq!((r.verb.as_str(), r.path.as_str()), ("set", "/a/controls/attack/normalized_value/value"));
        assert_eq!(r.param("message_id"), Some("U:5"));
        assert_eq!(r.param("cmd_listener"), Some("$PATH:x"));
        assert_eq!(r.data, Some(json!(0.42)));
        let r = Request::parse("post /a/controls/x/set_touch?message_id=U%3A4 {\"client_uid\":\"U\",\"touch\":true}").unwrap();
        assert_eq!(r.data.unwrap()["touch"], json!(true));
        let r = Request::parse("set /a/name/value \"Hello world\"").unwrap();
        assert_eq!(r.data, Some(json!("Hello world")));
    }

    #[test]
    fn maps_gui_ids_to_ordinals() {
        let p = params();
        let b = bridge(&p);
        assert_eq!(b.map.ordinal("input"), Some(0));
        assert_eq!(b.map.ordinal("thresh"), Some(1), "prefix of Threshold");
        assert_eq!(b.map.ordinal("sc_filter"), Some(3));
        assert_eq!(b.map.ordinal("fade"), None, "kept local");
        assert_eq!(scrape_control_ids(r#"x={"ua-param":"attack",misc:1};y={"ua-param":"mix"};z={"ua-param":"attack"}"#), vec!["attack", "mix"]);
    }

    #[test]
    fn heartbeat_subtree_and_pushes() {
        let mut p = params();
        let mut b = bridge(&p);
        let hb = b.handle("get /status?cmd_id=heartbeat&properties=0&children=0&message_id=U%3A0", &mut p);
        let hb: Json = serde_json::from_str(&hb[0]).unwrap();
        assert_eq!((hb["path"].as_str(), hb["parameters"]["cmd_id"].as_str()), (Some("/status"), Some("heartbeat")));
        let sub = b.handle(&format!("get {P}?flatvalue=1&commands=1&cmd_id=get_base_path_subtree&recursive=1&message_id=U%3A1"), &mut p);
        let sub: Json = serde_json::from_str(&sub[0]).unwrap();
        assert_eq!(sub["path"], json!(P));
        assert_eq!(sub["parameters"]["cmd_id"], json!("get_base_path_subtree"));
        let controls = &sub["data"]["children"][1];
        assert_eq!(controls["path"], json!("controls"));
        let ratio = &controls["children"][2];
        assert_eq!((ratio["path"].as_str(), ratio["properties"]["real_value"].as_u64(), ratio["properties"]["num_steps"].as_u64()), (Some("ratio"), Some(1), Some(11)));
        assert_eq!(sub["data"]["children"][2]["children"][0]["properties"]["level"], json!([0.0]));
        // Nothing changed yet: no pushes. An engine change pushes three properties, no cmd_id.
        assert!(b.pushes(&p).is_empty());
        p.params[0].value = 0.75;
        let pushes = b.pushes(&p);
        assert_eq!(pushes.len(), 3);
        let first: Json = serde_json::from_str(&pushes[0]).unwrap();
        assert_eq!(first, json!({"path": format!("{P}/controls/input/normalized_value/value"), "data": 0.75}));
    }

    #[test]
    fn writes_are_gated() {
        let mut p = params();
        let mut b = bridge(&p);
        b.handle(&format!("get {P}?cmd_id=get_base_path_subtree&message_id=U%3A1"), &mut p);
        let out = b.handle(&format!("set {P}/controls/input/normalized_value/value?cmd_id=set&message_id=U%3A5 0.9"), &mut p);
        let err: Json = serde_json::from_str(&out[0]).unwrap();
        assert_eq!(err["error"], json!("writes disabled"));
        assert_eq!(out.len(), 4, "error plus the current value pushed back");
        assert_eq!(p.value(0), Some(0.5));
        assert!(p.events.is_empty());

        b.writes_enabled = true;
        b.handle(&format!("post {P}/controls/input/set_touch?message_id=U%3A6 {{\"client_uid\":\"U\",\"touch\":true}}"), &mut p);
        b.handle(&format!("set {P}/controls/input/normalized_value/value?cmd_id=set&message_id=U%3A7 0.9"), &mut p);
        b.handle(&format!("post {P}/controls/input/set_touch?message_id=U%3A8 {{\"client_uid\":\"U\",\"touch\":false}}"), &mut p);
        assert_eq!(p.events, vec![MockEvent::Begin, MockEvent::Set(0, 0.9), MockEvent::End]);
        // real_value is a step index.
        b.handle(&format!("set {P}/controls/ratio/real_value/value?cmd_id=set&message_id=U%3A9 4"), &mut p);
        assert_eq!(p.value(2), Some(0.4));
        // GUI-only controls stay local.
        let ok = b.handle(&format!("set {P}/controls/fade/normalized_value/value?cmd_id=set&message_id=U%3A10 1"), &mut p);
        assert!(ok[0].contains("\"data\":1"));
        b.handle(&format!("post {P}/commands/notify_gui_loaded?cmd_id=post&message_id=U%3A11"), &mut p);
        assert!(b.loaded);
    }
}
