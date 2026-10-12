//! The engine's control tree, flattened for O(1) lookup by path, and snapshot files.

use std::collections::HashMap;
use std::path::Path;

use crate::error::{Error, Result};
use crate::path::{self as p};
use crate::value::{Json, Value};

/// Property metadata a control needs: range, enum choices, default and flags.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Meta {
    /// The engine's `type`: `bool`, `int`, `int64`, `float`, `string` or `pointer`.
    pub ty: Option<String>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    /// Enum list: strings, or `{value,string?,enabled?,type?,locked?,children?}` objects.
    pub values: Option<Json>,
    /// `<property>/default` (`Name/default` is the factory label, e.g. `ANALOG 1`).
    pub default: Option<Value>,
    pub readonly: bool,
    /// `<property>/enabled`: whether Console offers the control (e.g. `48V` in Line mode).
    pub enabled: Option<bool>,
}

impl Meta {
    /// Enum choices (`values` entries' `value`, or the entries themselves).
    pub fn choices(&self) -> Vec<Value> {
        let Some(Json::Array(list)) = &self.values else {
            return Vec::new();
        };
        list.iter()
            .filter_map(|item| match item {
                Json::Object(o) => o.get("value").map(Value::from_json),
                other => Some(Value::from_json(other)),
            })
            .collect()
    }

    /// `true` for `int` and `int64` properties, whose wire values must be written without a
    /// fraction.
    pub fn is_integral(&self) -> bool {
        matches!(self.ty.as_deref(), Some("int" | "int64"))
    }

    /// `v` normalised to 0…1 against `min`/`max`.
    pub fn normalized(&self, v: f64) -> Option<f64> {
        let (lo, hi) = (self.min?, self.max?);
        (hi > lo).then(|| (v - lo) / (hi - lo))
    }
}

/// One property as the engine describes it in a `get` reply:
/// `{"type":…, "min":…, "max":…, "values":…, "default":…, "enabled":…, "readonly":…, "value":…}`.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Property {
    /// `None` when the reply had no `value` key; `Some(Value::Null)` when it was null.
    pub value: Option<Value>,
    pub meta: Meta,
    /// Keys this model does not interpret (e.g. `plugin` on `Preset`, `volumes` on `Snapshot`).
    pub extra: serde_json::Map<String, Json>,
}

const KNOWN_KEYS: [&str; 8] = [
    "type", "value", "min", "max", "values", "default", "enabled", "readonly",
];

impl Property {
    pub fn with_value(value: Value) -> Property {
        Property {
            value: Some(value),
            ..Default::default()
        }
    }

    pub fn from_json(json: &Json) -> Property {
        let empty = serde_json::Map::new();
        let o = json.as_object().unwrap_or(&empty);
        Property {
            value: o.get("value").map(Value::from_json),
            meta: Meta {
                ty: o.get("type").and_then(Json::as_str).map(str::to_owned),
                min: o.get("min").and_then(Json::as_f64),
                max: o.get("max").and_then(Json::as_f64),
                values: o.get("values").filter(|v| !v.is_null()).cloned(),
                default: o.get("default").map(Value::from_json),
                readonly: o.get("readonly").and_then(Json::as_bool).unwrap_or(false),
                enabled: o.get("enabled").and_then(Json::as_bool),
            },
            extra: o
                .iter()
                .filter(|(k, _)| !KNOWN_KEYS.contains(&k.as_str()))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        }
    }

    pub fn to_json(&self) -> Json {
        let mut o = self.extra.clone();
        let m = &self.meta;
        if let Some(t) = &m.ty {
            o.insert("type".into(), Json::String(t.clone()));
        }
        if let Some(v) = m.min {
            o.insert("min".into(), crate::value::number_json(v));
        }
        if let Some(v) = m.max {
            o.insert("max".into(), crate::value::number_json(v));
        }
        if let Some(v) = &m.values {
            o.insert("values".into(), v.clone());
        }
        if let Some(v) = &m.default {
            o.insert("default".into(), v.to_json());
        }
        if let Some(e) = m.enabled {
            o.insert("enabled".into(), Json::Bool(e));
        }
        if m.readonly {
            o.insert("readonly".into(), Json::Bool(true));
        }
        if let Some(v) = &self.value {
            o.insert("value".into(), v.to_json());
        }
        Json::Object(o)
    }
}

/// Structure of one node: its children and property names, in display order.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Node {
    /// Child names. Numeric collections (`"0"`, `"1"`, … `"10"`) are sorted numerically.
    pub children: Vec<String>,
    pub properties: Vec<String>,
    /// The node's `commands` catalogue, when the engine sent one (engine 12.0.0 sends it on `/`).
    pub commands: Option<Json>,
    /// True when the node arrived as an empty stub (`{}`) and has not been fetched.
    pub is_stub: bool,
}

/// The engine's control tree, flattened.
///
/// Properties are keyed by property path (`/devices/0/inputs/0/Mute`), nodes by node path (`/`,
/// `/devices`, `/devices/0`, …). Lookups accept paths with or without `/value`.
#[derive(Clone, Debug, PartialEq)]
pub struct Tree {
    properties: HashMap<String, Property>,
    nodes: HashMap<String, Node>,
}

impl Default for Tree {
    fn default() -> Self {
        Tree::new()
    }
}

impl Tree {
    pub fn new() -> Tree {
        let mut nodes = HashMap::new();
        nodes.insert("/".to_owned(), Node::default());
        Tree {
            properties: HashMap::new(),
            nodes,
        }
    }

    /// Builds a tree from an engine node object (`{"properties":…,"children":…,"commands":…}`),
    /// rooted at `/`.
    pub fn from_node(json: &Json) -> Tree {
        let mut t = Tree::new();
        t.insert_node(json, "/");
        t
    }

    /// Builds a tree from a flat device map (`[{path,type,min,max,values,value}]`), the format of
    /// open-apollo's `mixer-engine/device_maps/*.json`.
    pub fn from_flat_controls(controls: &[Json]) -> Tree {
        let mut t = Tree::new();
        for control in controls {
            let Some(path) = control.get("path").and_then(Json::as_str) else {
                continue;
            };
            let mut o = control.as_object().cloned().unwrap_or_default();
            o.remove("path");
            for key in ["min", "max", "values"] {
                if o.get(key).is_some_and(Json::is_null) {
                    o.remove(key);
                }
            }
            let (node, name) = p::split(path);
            t.ensure_node(node);
            if !t.properties.contains_key(path) {
                t.nodes
                    .get_mut(node)
                    .expect("ensured")
                    .properties
                    .push(name.to_owned());
            }
            t.properties
                .insert(path.to_owned(), Property::from_json(&Json::Object(o)));
        }
        t.sort_all();
        t
    }

    // MARK: Lookup

    pub fn property(&self, path: &str) -> Option<&Property> {
        self.properties.get(p::property_path(path))
    }

    pub fn value(&self, path: &str) -> Option<&Value> {
        self.property(path)?.value.as_ref()
    }

    pub fn meta(&self, path: &str) -> Option<&Meta> {
        self.property(path).map(|p| &p.meta)
    }

    pub fn node(&self, path: &str) -> Option<&Node> {
        self.nodes.get(path)
    }

    /// Child names of a node, in display order (empty for an unknown node).
    pub fn children(&self, path: &str) -> &[String] {
        self.nodes
            .get(path)
            .map(|n| n.children.as_slice())
            .unwrap_or(&[])
    }

    /// Numeric child indices of a collection node (`inputs`, `sends`, …).
    pub fn indices(&self, path: &str) -> Vec<usize> {
        self.children(path)
            .iter()
            .filter_map(|c| c.parse().ok())
            .collect()
    }

    pub fn has_property(&self, path: &str) -> bool {
        self.property(path).is_some()
    }

    pub fn has_node(&self, path: &str) -> bool {
        self.nodes.contains_key(path)
    }

    pub fn stub_paths(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .nodes
            .iter()
            .filter(|(_, n)| n.is_stub)
            .map(|(k, _)| k.clone())
            .collect();
        v.sort();
        v
    }

    pub fn property_count(&self) -> usize {
        self.properties.len()
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Every property, unordered.
    pub fn properties(&self) -> impl Iterator<Item = (&str, &Property)> {
        self.properties.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Every node, unordered.
    pub fn nodes(&self) -> impl Iterator<Item = (&str, &Node)> {
        self.nodes.iter().map(|(k, v)| (k.as_str(), v))
    }

    // MARK: Mutation

    /// Sets a property value. Returns false when the value was already equal. Unknown paths are
    /// added (pushes can name properties the tree did not list, e.g. the hidden `inputs/N/Gain`).
    pub fn set_value(&mut self, path: &str, value: Value) -> bool {
        let pp = p::property_path(path);
        if let Some(prop) = self.properties.get_mut(pp) {
            if prop.value.as_ref() == Some(&value) {
                return false;
            }
            prop.value = Some(value);
            return true;
        }
        let (node, name) = p::split(pp);
        self.ensure_node(node);
        self.nodes
            .get_mut(node)
            .expect("ensured")
            .properties
            .push(name.to_owned());
        self.properties
            .insert(pp.to_owned(), Property::with_value(value));
        true
    }

    /// Replaces a property's metadata (and value) wholesale, e.g. from a `get <property>` reply.
    pub fn set_property(&mut self, path: &str, property: Property) {
        let pp = p::property_path(path);
        if !self.properties.contains_key(pp) {
            let (node, name) = p::split(pp);
            self.ensure_node(node);
            self.nodes
                .get_mut(node)
                .expect("ensured")
                .properties
                .push(name.to_owned());
        }
        self.properties.insert(pp.to_owned(), property);
    }

    /// Replaces the subtree at `path` with an engine node object (as returned by `get <path>`).
    pub fn insert_node(&mut self, json: &Json, path: &str) {
        self.remove_subtree(path);
        self.ensure_node(path);
        self.merge(json, path);
        self.sort_all();
    }

    fn merge(&mut self, json: &Json, path: &str) {
        let empty = serde_json::Map::new();
        let o = json.as_object().unwrap_or(&empty);
        let mut node = Node {
            is_stub: o.is_empty() && path != "/",
            commands: o.get("commands").cloned(),
            ..Default::default()
        };
        if let Some(Json::Object(props)) = o.get("properties") {
            for (name, pj) in props {
                self.properties
                    .insert(p::join(path, name), Property::from_json(pj));
                node.properties.push(name.clone());
            }
        }
        if let Some(Json::Object(kids)) = o.get("children") {
            for (name, child) in kids {
                node.children.push(name.clone());
                self.merge(child, &p::join(path, name));
            }
        }
        self.nodes.insert(path.to_owned(), node);
    }

    fn ensure_node(&mut self, path: &str) {
        if self.nodes.contains_key(path) {
            return;
        }
        if path != "/" {
            let (parent, name) = p::split(path);
            self.ensure_node(parent);
            self.nodes
                .get_mut(parent)
                .expect("ensured")
                .children
                .push(name.to_owned());
        }
        self.nodes.insert(path.to_owned(), Node::default());
    }

    /// Removes a node, its properties and descendants.
    pub fn remove_subtree(&mut self, path: &str) {
        let Some(node) = self.nodes.remove(path) else {
            return;
        };
        for name in &node.properties {
            self.properties.remove(&p::join(path, name));
        }
        for child in &node.children {
            self.remove_subtree(&p::join(path, child));
        }
        if path != "/" {
            let (parent, name) = p::split(path);
            if let Some(parent) = self.nodes.get_mut(parent) {
                parent.children.retain(|c| c != name);
            }
        } else {
            self.nodes.insert("/".to_owned(), Node::default());
        }
    }

    fn sort_all(&mut self) {
        for node in self.nodes.values_mut() {
            node.children.sort_by(|a, b| child_order(a, b));
            node.properties.sort();
        }
    }

    // MARK: Serialisation

    /// The nested engine node object for `path`, the shape `get <path>?recursive=1` returns.
    pub fn node_json(&self, path: &str) -> Json {
        let Some(node) = self.nodes.get(path) else {
            return Json::Object(Default::default());
        };
        let mut o = serde_json::Map::new();
        if !node.properties.is_empty() {
            let props = node
                .properties
                .iter()
                .map(|name| {
                    let pj = self
                        .properties
                        .get(&p::join(path, name))
                        .map(Property::to_json)
                        .unwrap_or_else(|| Json::Object(Default::default()));
                    (name.clone(), pj)
                })
                .collect();
            o.insert("properties".into(), Json::Object(props));
        }
        if !node.children.is_empty() {
            let kids = node
                .children
                .iter()
                .map(|name| (name.clone(), self.node_json(&p::join(path, name))))
                .collect();
            o.insert("children".into(), Json::Object(kids));
        }
        if let Some(c) = &node.commands {
            o.insert("commands".into(), c.clone());
        }
        // `{}` reads back as an unfetched stub; a known-empty node (e.g. a trimmed catalogue)
        // is written with an empty `children` object instead.
        if o.is_empty() && !node.is_stub && path != "/" {
            o.insert("children".into(), Json::Object(Default::default()));
        }
        Json::Object(o)
    }

    /// A copy without bulky lists the mixer UI does not need: preset folder trees (`Preset/values`,
    /// ~870 KB of a 1.7 MB x4 tree), `Snapshot`'s volume list and, with `catalogs`, the plugin
    /// catalogue, plugin scenes, ASIO channels and driver routes.
    pub fn trimmed(&self, catalogs: bool) -> Tree {
        let mut t = self.clone();
        for (path, prop) in t.properties.iter_mut() {
            let name = p::split(path).1;
            if name == "Preset" {
                prop.meta.values = None;
            }
            if name == "Snapshot" {
                prop.extra.remove("volumes");
            }
        }
        if catalogs {
            for path in ["/plugins", "/pluginscenes", "/inputroutes", "/outputroutes"] {
                for child in t.children(path).to_vec() {
                    t.remove_subtree(&p::join(path, &child));
                }
            }
            for device in t.children("/devices").to_vec() {
                for coll in ["asioinputs", "asiooutputs"] {
                    let cp = format!("/devices/{device}/{coll}");
                    for child in t.children(&cp).to_vec() {
                        t.remove_subtree(&p::join(&cp, &child));
                    }
                }
            }
        }
        t
    }
}

/// Numeric names sort numerically and before non-numeric ones.
pub(crate) fn child_order(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering::*;
    match (a.parse::<i64>(), b.parse::<i64>()) {
        (Ok(x), Ok(y)) => x.cmp(&y),
        (Ok(_), Err(_)) => Less,
        (Err(_), Ok(_)) => Greater,
        _ => a.cmp(b),
    }
}

// MARK: - Snapshot files

/// Where a snapshot came from, stored alongside the tree in snapshot files.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct SnapshotInfo {
    pub source: Option<String>,
    pub captured: Option<String>,
    pub engine_version: Option<String>,
    pub device: Option<String>,
    pub note: Option<String>,
}

/// A tree plus provenance, in ConsoleLab's snapshot file format
/// (`{"format":"consolelab-engine-snapshot","version":1,"info":{…},"tree":{…}}`).
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub tree: Tree,
    pub info: SnapshotInfo,
}

impl Snapshot {
    pub const FORMAT: &'static str = "consolelab-engine-snapshot";

    /// Fills `engine_version` and `device` from the tree when the info lacks them.
    pub fn new(tree: Tree, info: SnapshotInfo) -> Snapshot {
        let mut s = Snapshot { tree, info };
        s.fill_info();
        s
    }

    fn fill_info(&mut self) {
        if self.info.engine_version.is_none() {
            self.info.engine_version = self
                .tree
                .value("/Version")
                .and_then(Value::as_str)
                .map(str::to_owned);
        }
        if self.info.device.is_none() {
            self.info.device = self
                .tree
                .value("/devices/0/DeviceName")
                .and_then(Value::as_str)
                .map(str::to_owned);
        }
    }

    /// Accepts, in order: a ConsoleLab snapshot (`{"format":…,"tree":…}`), a raw engine reply to
    /// `get /?recursive=1` (`{"path":"/","data":{…}}`), a bare node (`{"properties":…,
    /// "children":…}`), an open-apollo flat device map (`{"controls":[…]}`) or a
    /// `dump_x8p_tree.py` capture (`{"controls":{…}}`).
    pub fn from_json(json: &Json) -> Result<Snapshot> {
        let Json::Object(o) = json else {
            return Err(Error::UnrecognizedFormat);
        };
        let s = |k: &str| o.get(k).and_then(Json::as_str).map(str::to_owned);
        let is_node = |j: &Json| j.get("properties").is_some() || j.get("children").is_some();
        let (tree, info) = if o.get("format").and_then(Json::as_str) == Some(Self::FORMAT)
            && o.contains_key("tree")
        {
            let i = o.get("info").cloned().unwrap_or(Json::Null);
            let is = |k: &str| i.get(k).and_then(Json::as_str).map(str::to_owned);
            let info = SnapshotInfo {
                source: is("source"),
                captured: is("captured"),
                engine_version: is("engineVersion"),
                device: is("device"),
                note: is("note"),
            };
            (Tree::from_node(&o["tree"]), info)
        } else if o.contains_key("path") && o.get("data").is_some_and(is_node) {
            (
                Tree::from_node(&o["data"]),
                SnapshotInfo {
                    source: Some("engine reply".into()),
                    ..Default::default()
                },
            )
        } else if is_node(json) {
            (
                Tree::from_node(json),
                SnapshotInfo {
                    source: Some("engine node".into()),
                    ..Default::default()
                },
            )
        } else if let Some(Json::Array(controls)) = o.get("controls") {
            let info = SnapshotInfo {
                source: Some(
                    format!("open-apollo device map {}", s("host").unwrap_or_default())
                        .trim_end()
                        .to_owned(),
                ),
                captured: s("timestamp"),
                device: s("device_name"),
                ..Default::default()
            };
            (Tree::from_flat_controls(controls), info)
        } else if let Some(controls) = o.get("controls").filter(|c| is_node(c)) {
            let info = SnapshotInfo {
                source: Some("dump_x8p_tree capture".into()),
                device: s("device_name"),
                ..Default::default()
            };
            (Tree::from_node(controls), info)
        } else {
            return Err(Error::UnrecognizedFormat);
        };
        Ok(Snapshot::new(tree, info))
    }

    pub fn from_slice(bytes: &[u8]) -> Result<Snapshot> {
        Snapshot::from_json(&serde_json::from_slice(bytes)?)
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Snapshot> {
        Snapshot::from_slice(&std::fs::read(path)?)
    }

    pub fn to_json(&self) -> Json {
        let mut i = serde_json::Map::new();
        for (k, v) in [
            ("source", &self.info.source),
            ("captured", &self.info.captured),
            ("engineVersion", &self.info.engine_version),
            ("device", &self.info.device),
            ("note", &self.info.note),
        ] {
            if let Some(v) = v {
                i.insert(k.into(), Json::String(v.clone()));
            }
        }
        serde_json::json!({
            "format": Self::FORMAT,
            "version": 1,
            "info": Json::Object(i),
            "tree": self.tree.node_json("/"),
        })
    }

    pub fn to_vec(&self, pretty: bool) -> Vec<u8> {
        let j = self.to_json();
        if pretty {
            serde_json::to_vec_pretty(&j)
        } else {
            serde_json::to_vec(&j)
        }
        .expect("JSON values always serialise")
    }

    /// Writes atomically (temporary file, then rename).
    pub fn save(&self, path: impl AsRef<Path>, pretty: bool) -> Result<()> {
        let path = path.as_ref();
        let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
        std::fs::write(&tmp, self.to_vec(pretty))?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// A sanitized, trimmed live capture of UA Mixer Engine 12.0.0 with an Apollo x4 (see the
    /// crate README), for offline rendering.
    #[cfg(feature = "bundled-snapshot")]
    pub fn bundled_x4() -> Snapshot {
        Snapshot::from_slice(include_bytes!(
            "../tests/fixtures/apollo-x4-12.0.0.snapshot.json"
        ))
        .expect("bundled snapshot parses")
    }
}
