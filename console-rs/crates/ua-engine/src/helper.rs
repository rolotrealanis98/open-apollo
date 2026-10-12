//! The UA Mixer Helper on TCP 4720 (`mxhlpr`, `_uamixerhelper._tcp`), read-only.
//!
//! Verified against the Mac's helper 1.3.1.1152 (2026-10-12):
//!
//! - **Framing.** Plain NUL-terminated JSON text, like the engine on 4710. The helper answers
//!   text clients in text; `post command_format 2` (UBJSON) is not needed and is never sent.
//! - **No challenge needed.** A client that sends only identity (`network_id`, `client_id`),
//!   `get` and `subscribe` is served, and stays connected (tested for 45 s without pings and
//!   40 s with them). `post /request_challenge`/`/response` are never sent.
//! - **Tree format.** Node `children` are *arrays* of `{"path": name, …}`; properties are
//!   `{"value", "read_only", "values"?}` with snake_case names; nodes carry `commands` lists.
//!   [`engine_node_from_helper`] converts it to the engine's object form so it loads into a
//!   [`Tree`].
//! - **Replies.** `get` echoes `func_id`/`recursive` in `parameters`. Errors are
//!   `{"error":"uasrv.invalid_object"}` (the open-apollo daemon's replay answers engine-style
//!   errors without `func_id`). `subscribe <property>` is acknowledged with the current value; a
//!   recursive node subscribe is acknowledged with `null`. `get /ping` answers
//!   `{"path":"/ping","data":true}`.
//!
//! Content: engine and device settings (`sample_rate`, buffer sizes, `latency`,
//! `driver_status`, initialisation progress, peak/clip hold, controls mode), `status` (helper
//! name and version), `drivers` (CoreAudio devices with I/O names and rates), `auto_gain`,
//! `dante`, `preferences`, `workspace`, `presets`, and `devices`: the native "supercore" engine's
//! device with `buses` (CUE 1–4, MAIN), `tracks`, `meters` (render, renderio, mem), MIDI and
//! banks. Native plug-ins would sit in `devices/{uid}/{tracks,buses}/{uid}/effects/{slot}` with
//! `plugin_name`, `plugin_id`, `plugin_power`, `enabled`. On the Mac every such slot was empty
//! and `tracks` had no children (`active_audio_engine.enabled` false): the Apollo mixer and its
//! UAD-2 inserts live on 4710.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use crate::command::Command;
use crate::connection::{
    ConnectionState, Endpoint, Engine, EngineHandle, Options, PendingReply, Update,
};
use crate::path;
use crate::store::Status;
use crate::tree::Tree;
use crate::value::{Json, Value};

/// The helper's TCP port.
pub const HELPER_PORT: u16 = 4720;

/// Converts a helper node (`children` as an array of `{"path": name, …}`, properties with
/// `read_only`) into the engine's node form (`children` keyed by name, `readonly`).
/// Children that arrived as bare `{"path": name}` become empty (stub) nodes.
pub fn engine_node_from_helper(json: &Json) -> Json {
    let mut o = serde_json::Map::new();
    if let Some(Json::Object(props)) = json.get("properties") {
        let props = props
            .iter()
            .map(|(k, v)| {
                let p = match v {
                    Json::Object(m) => {
                        let mut m = m.clone();
                        if let Some(ro) = m.remove("read_only") {
                            m.insert("readonly".into(), ro);
                        }
                        Json::Object(m)
                    }
                    other => serde_json::json!({ "value": other }),
                };
                (k.clone(), p)
            })
            .collect();
        o.insert("properties".into(), Json::Object(props));
    }
    let kids: Option<serde_json::Map<String, Json>> = match json.get("children") {
        Some(Json::Array(list)) => Some(
            list.iter()
                .filter_map(|c| {
                    Some((
                        c.get("path")?.as_str()?.to_owned(),
                        engine_node_from_helper(c),
                    ))
                })
                .collect(),
        ),
        Some(Json::Object(map)) => Some(
            map.iter()
                .map(|(k, c)| (k.clone(), engine_node_from_helper(c)))
                .collect(),
        ),
        _ => None,
    };
    if let Some(kids) = kids {
        o.insert("children".into(), Json::Object(kids));
    }
    if let Some(c) = json.get("commands") {
        o.insert("commands".into(), c.clone());
    }
    Json::Object(o)
}

/// A [`Tree`] from a helper root node (or a `{"path":"/","data":{…}}` reply to `get /?recursive=1`).
pub fn tree_from_helper(json: &Json) -> Tree {
    let node = match (json.get("path"), json.get("data")) {
        (Some(_), Some(data)) => data,
        _ => json,
    };
    Tree::from_node(&engine_node_from_helper(node))
}

/// A CoreAudio device the helper reports in `/drivers/connected`.
#[derive(Clone, Debug, PartialEq)]
pub struct DriverInfo {
    pub id: String,
    pub name: String,
    /// `apollo_thunderbolt`, `built_in`, `other`, …
    pub kind: String,
    /// `apollo` for UA devices.
    pub device_type: Option<String>,
    pub transport: String,
    pub num_inputs: u32,
    pub num_outputs: u32,
    pub input_names: Vec<String>,
    pub output_names: Vec<String>,
}

impl DriverInfo {
    pub fn is_apollo(&self) -> bool {
        self.device_type.as_deref() == Some("apollo") || self.kind.starts_with("apollo")
    }
}

/// The helper's settings and state, typed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HelperInfo {
    /// `/status/name` (`mxhlpr`).
    pub name: Option<String>,
    /// `/status/version` (`1.3.1.1152`).
    pub version: Option<String>,
    pub sample_rate: Option<f64>,
    pub sample_rates: Vec<f64>,
    /// `initializing`, …
    pub driver_status: Option<String>,
    pub initialized: Option<bool>,
    pub initialized_percent: Option<f64>,
    /// Native engine latency in samples.
    pub latency: Option<i64>,
    pub io_buffer_size: Option<i64>,
    pub render_buffer_size: Option<i64>,
    /// `/active_audio_engine.enabled`: whether the native (supercore) engine is running.
    pub native_engine_active: bool,
    pub peak_hold: Option<String>,
    pub clip_hold: Option<String>,
    pub controls_mode: Option<String>,
    pub post_fader_meters: Option<bool>,
    /// `/auto_gain/status` (`stopped`, …).
    pub auto_gain_status: Option<String>,
    pub drivers: Vec<DriverInfo>,
}

impl HelperInfo {
    pub fn from_tree(tree: &Tree) -> HelperInfo {
        let s = |p: &str| tree.value(p).and_then(Value::as_str).map(str::to_owned);
        let f = |p: &str| tree.value(p).and_then(Value::as_f64);
        let i = |p: &str| tree.value(p).and_then(Value::as_i64);
        let b = |p: &str| tree.value(p).and_then(Value::as_bool);
        let drivers = tree
            .value("/drivers/connected")
            .and_then(Value::as_json)
            .and_then(Json::as_array)
            .map(|list| list.iter().map(driver_info).collect())
            .unwrap_or_default();
        HelperInfo {
            name: s("/status/name"),
            version: s("/status/version"),
            sample_rate: f("/sample_rate"),
            sample_rates: tree
                .meta("/sample_rate")
                .map(|m| m.choices().iter().filter_map(Value::as_f64).collect())
                .unwrap_or_default(),
            driver_status: s("/driver_status"),
            initialized: b("/initialized"),
            initialized_percent: f("/initialized_percent"),
            latency: i("/latency"),
            io_buffer_size: i("/io_buffer_size"),
            render_buffer_size: i("/render_buffer_size"),
            native_engine_active: tree
                .value("/active_audio_engine")
                .and_then(Value::as_json)
                .and_then(|j| j.get("enabled"))
                .and_then(Json::as_bool)
                .unwrap_or(false),
            peak_hold: s("/peak_hold"),
            clip_hold: s("/clip_hold"),
            controls_mode: s("/controls_mode"),
            post_fader_meters: b("/post_fader_meters"),
            auto_gain_status: s("/auto_gain/status"),
            drivers,
        }
    }

    /// The connected Apollo, if the helper sees one.
    pub fn apollo(&self) -> Option<&DriverInfo> {
        self.drivers.iter().find(|d| d.is_apollo())
    }
}

fn names(j: Option<&Json>) -> Vec<String> {
    match j {
        Some(Json::Array(a)) => a
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect(),
        Some(Json::String(s)) => vec![s.clone()],
        _ => Vec::new(),
    }
}

fn driver_info(j: &Json) -> DriverInfo {
    let s = |k: &str| {
        j.get(k)
            .and_then(Json::as_str)
            .unwrap_or_default()
            .trim()
            .to_owned()
    };
    let n = |k: &str| j.get(k).and_then(Json::as_u64).unwrap_or(0) as u32;
    DriverInfo {
        id: s("id"),
        name: s("name"),
        kind: s("type"),
        device_type: j
            .get("device_type")
            .and_then(Json::as_str)
            .map(str::to_owned),
        transport: s("transport"),
        num_inputs: n("num_inputs"),
        num_outputs: n("num_outputs"),
        input_names: names(j.get("input_names")),
        output_names: names(j.get("output_names")),
    }
}

/// Who owns a native effect slot in the supercore device.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NativeOwner {
    Track,
    Bus,
}

/// A native plug-in loaded in the helper's supercore engine.
#[derive(Clone, Debug, PartialEq)]
pub struct NativeInsert {
    /// Supercore device uid.
    pub device: String,
    pub owner: NativeOwner,
    pub owner_uid: String,
    /// The track's or bus's `name` (`CUE 1`, `MAIN`, a channel name…).
    pub owner_name: Option<String>,
    /// The effect node's name: `effect_0` … `effect_7`, `bus_compressor`, …
    pub slot: String,
    /// `N` of `effect_N`.
    pub index: Option<usize>,
    pub insert_type: Option<String>,
    pub plugin_id: Option<String>,
    pub plugin_name: String,
    pub power: bool,
    pub enabled: bool,
    /// The effect node path.
    pub path: String,
}

impl NativeInsert {
    /// Every slot of the supercore device(s) with a `plugin_name`, tracks first.
    pub fn list(tree: &Tree) -> Vec<NativeInsert> {
        let mut out = Vec::new();
        for device in tree.children("/devices") {
            for (coll, owner) in [("tracks", NativeOwner::Track), ("buses", NativeOwner::Bus)] {
                let cp = format!("/devices/{device}/{coll}");
                for uid in tree.children(&cp) {
                    let base = format!("{cp}/{uid}");
                    let owner_name = tree
                        .value(&format!("{base}/name"))
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    let ep = format!("{base}/effects");
                    for slot in tree.children(&ep) {
                        let sp = format!("{ep}/{slot}");
                        let Some(plugin_name) = tree
                            .value(&format!("{sp}/plugin_name"))
                            .and_then(Value::as_str)
                            .filter(|n| !n.is_empty())
                        else {
                            continue;
                        };
                        let plugin_id = tree
                            .value(&format!("{sp}/plugin_id"))
                            .filter(|v| !v.is_null())
                            .map(|v| match v {
                                Value::String(s) => s.clone(),
                                other => other.to_string(),
                            });
                        out.push(NativeInsert {
                            device: device.clone(),
                            owner,
                            owner_uid: uid.clone(),
                            owner_name: owner_name.clone(),
                            slot: slot.clone(),
                            index: slot.strip_prefix("effect_").and_then(|k| k.parse().ok()),
                            insert_type: tree
                                .value(&format!("{sp}/insert_type"))
                                .and_then(Value::as_str)
                                .map(str::to_owned),
                            plugin_id,
                            plugin_name: plugin_name.to_owned(),
                            power: tree
                                .value(&format!("{sp}/plugin_power"))
                                .and_then(Value::as_bool)
                                .unwrap_or(true),
                            enabled: tree
                                .value(&format!("{sp}/enabled"))
                                .and_then(Value::as_bool)
                                .unwrap_or(true),
                            path: sp,
                        });
                    }
                }
            }
        }
        out
    }
}

/// Status properties kept subscribed (read-only `subscribe <p>/value`).
const STATUS_PATHS: [&str; 17] = [
    "/sample_rate",
    "/driver_status",
    "/initialized",
    "/initialized_percent",
    "/initialized_status",
    "/latency",
    "/io_buffer_size",
    "/render_buffer_size",
    "/active_audio_engine",
    "/peak_hold",
    "/clip_hold",
    "/controls_mode",
    "/post_fader_meters",
    "/drivers/connected",
    "/auto_gain/status",
    // Settings › NETWORK › NETWORK INTERFACE (Console 1.3.1 capture s30): the adapters (a JSON
    // text `[{adapter_name, adapter_address}]`) and the chosen one.
    "/dante/available_network_adapters",
    "/dante/primary_network_adapter",
];

/// The helper property NETWORK INTERFACE writes (`set …/value "<adapter_name>"`).
pub const PRIMARY_NETWORK_ADAPTER: &str = "/dante/primary_network_adapter";
/// The helper property listing the host's network adapters.
pub const AVAILABLE_NETWORK_ADAPTERS: &str = "/dante/available_network_adapters";

/// One host network adapter as the helper lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetworkAdapter {
    pub name: String,
    /// IPv4 address, empty when the adapter has none.
    pub address: String,
}

impl NetworkAdapter {
    /// Parses `/dante/available_network_adapters` (a JSON text: `[{"adapter_name", "adapter_address"}]`).
    pub fn list(text: &str) -> Vec<NetworkAdapter> {
        let Ok(Json::Array(items)) = serde_json::from_str::<Json>(text) else { return Vec::new() };
        items
            .iter()
            .filter_map(|a| {
                let name = a.get("adapter_name")?.as_str()?.to_owned();
                let address = a.get("adapter_address").and_then(Json::as_str).unwrap_or("").to_owned();
                Some(NetworkAdapter { name, address })
            })
            .collect()
    }

    /// How Console lists it: `en0  (192.168.1.4)`, `en14  ()` (capture s30).
    pub fn label(&self) -> String {
        format!("{}  ({})", self.name, self.address)
    }
}
/// Effect-slot properties kept subscribed on every supercore bus and track.
const SLOT_PROPS: [&str; 3] = ["plugin_name", "plugin_power", "enabled"];

/// The helper's state, owned by the UI thread (like [`Store`](crate::Store), read-only).
///
/// On connect it reads the whole tree (`get /?recursive=1`, about 0.8 MB on the Mac), then
/// subscribes the status properties and every supercore effect slot's `plugin_name`,
/// `plugin_power` and `enabled`, and re-reads each device's `tracks` subtree every
/// [`HelperStore::poll_interval`] (structural changes are not pushed to value subscribers).
pub struct HelperStore {
    tree: Tree,
    status: Status,
    handle: Option<EngineHandle>,
    tree_request: Option<PendingReply>,
    polls: Vec<(String, PendingReply)>,
    last_poll: Option<Instant>,
    /// How often `tracks` subtrees are re-read (10 s).
    pub poll_interval: Duration,
    subscribed: HashSet<String>,
    revision: u64,
    tree_revision: u64,
}

impl Default for HelperStore {
    fn default() -> Self {
        HelperStore::new()
    }
}

impl std::fmt::Debug for HelperStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HelperStore")
            .field("status", &self.status)
            .field("revision", &self.revision)
            .finish()
    }
}

impl HelperStore {
    pub fn new() -> HelperStore {
        HelperStore {
            tree: Tree::new(),
            status: Status::Empty,
            handle: None,
            tree_request: None,
            polls: Vec::new(),
            last_poll: None,
            poll_interval: Duration::from_secs(10),
            subscribed: HashSet::new(),
            revision: 0,
            tree_revision: 0,
        }
    }

    /// Offline, from a helper tree (root node or `get /?recursive=1` reply, helper format).
    pub fn from_helper_json(json: &Json) -> HelperStore {
        let mut s = HelperStore::new();
        s.install(tree_from_helper(json));
        s.status = Status::Offline("helper snapshot".into());
        s
    }

    /// Connects read-only (identity, `get`, `subscribe`, `get /ping`; never `set`/`post`).
    pub fn connect(&mut self, endpoint: Endpoint) {
        self.connect_with(endpoint, Options::helper());
    }

    /// Connects like [`HelperStore::connect`]; with `writes` the connection also accepts `set`
    /// (Console writes helper settings such as NETWORK INTERFACE). Read-only otherwise.
    pub fn connect_writable(&mut self, endpoint: Endpoint, writes: bool) {
        self.disconnect();
        let mut options = Options::helper();
        options.allow_writes = writes;
        options.load_tree = false;
        self.status = Status::Connecting(endpoint.to_string());
        self.handle = Some(Engine::connect(endpoint, options));
    }

    /// `set <path>/value <value>` on the helper. Refused (false) unless connected with
    /// [`HelperStore::connect_writable`]`(…, true)`. The local tree follows at once.
    pub fn set(&mut self, path: &str, value: &Value) -> bool {
        let Some(h) = &self.handle else { return false };
        if h.send(Command::set(path, value, false)).is_err() {
            return false;
        }
        if self.tree.set_value(path, value.clone()) {
            self.revision += 1;
        }
        true
    }

    /// The host's network adapters and the selected one (`/dante/…`), when the helper has them.
    pub fn network_adapters(&self) -> (Vec<NetworkAdapter>, Option<String>) {
        let list = match self.tree.value(AVAILABLE_NETWORK_ADAPTERS) {
            Some(Value::String(s)) => NetworkAdapter::list(s),
            _ => Vec::new(),
        };
        let cur = match self.tree.value(PRIMARY_NETWORK_ADAPTER) {
            Some(Value::String(s)) => Some(s.clone()),
            _ => None,
        };
        (list, cur)
    }

    /// Like [`HelperStore::connect`]; `allow_writes` is forced off.
    pub fn connect_with(&mut self, endpoint: Endpoint, mut options: Options) {
        self.disconnect();
        options.allow_writes = false;
        options.load_tree = false;
        self.status = Status::Connecting(endpoint.to_string());
        self.handle = Some(Engine::connect(endpoint, options));
    }

    pub fn disconnect(&mut self) {
        self.handle = None;
        self.tree_request = None;
        self.polls.clear();
        self.subscribed.clear();
        if !matches!(self.status, Status::Empty | Status::Offline(_)) {
            self.status = Status::Offline("disconnected".into());
        }
    }

    pub fn status(&self) -> &Status {
        &self.status
    }

    pub fn tree(&self) -> &Tree {
        &self.tree
    }

    pub fn value(&self, path: &str) -> Option<&Value> {
        self.tree.value(path)
    }

    pub fn connection(&self) -> Option<&EngineHandle> {
        self.handle.as_ref()
    }

    /// Bumped on every change (value pushes, polled subtrees, tree installs).
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn tree_revision(&self) -> u64 {
        self.tree_revision
    }

    pub fn info(&self) -> HelperInfo {
        HelperInfo::from_tree(&self.tree)
    }

    pub fn native_inserts(&self) -> Vec<NativeInsert> {
        NativeInsert::list(&self.tree)
    }

    fn install(&mut self, tree: Tree) {
        self.tree = tree;
        self.revision += 1;
        self.tree_revision += 1;
    }

    /// Applies queued updates and timed work. Returns true when anything changed.
    pub fn drain(&mut self) -> bool {
        let rev = self.revision;
        let status = self.status.clone();
        let updates: Vec<Update> = match &self.handle {
            Some(h) => h.updates().try_iter().collect(),
            None => return false,
        };
        for u in updates {
            match u {
                Update::State(ConnectionState::Connecting) => {
                    let ep = self
                        .handle
                        .as_ref()
                        .map(|h| h.endpoint().to_string())
                        .unwrap_or_default();
                    self.status = Status::Connecting(ep);
                }
                Update::State(ConnectionState::Ready) => {
                    self.subscribed.clear();
                    self.polls.clear();
                    if let Some(h) = &self.handle {
                        self.tree_request =
                            Some(h.request_with_timeout("/", true, Duration::from_secs(60)));
                        self.status = Status::Loading;
                    }
                }
                Update::State(ConnectionState::BackingOff { delay, reason, .. }) => {
                    self.status = Status::Reconnecting(format!(
                        "{reason} — retry in {:.1} s",
                        delay.as_secs_f64()
                    ));
                }
                Update::State(ConnectionState::Failed(r)) => self.status = Status::Failed(r),
                Update::State(_) => {}
                Update::Values(values) => {
                    for (p, v) in values {
                        if self.tree.set_value(&p, v) {
                            self.revision += 1;
                        }
                    }
                }
                Update::Tree { .. } | Update::Error { .. } | Update::Other(_) => {}
            }
        }
        if let Some(reply) = &self.tree_request {
            match reply.try_take() {
                None => {}
                Some(Ok(msg)) => {
                    self.tree_request = None;
                    if let Some(data) = msg.data {
                        self.install(tree_from_helper(&data));
                        self.status = Status::Live;
                        self.last_poll = Some(Instant::now());
                        self.subscribe();
                    }
                }
                Some(Err(e)) => {
                    self.tree_request = None;
                    self.status = Status::Failed(format!("get /: {e}"));
                }
            }
        }
        self.poll();
        self.revision != rev || self.status != status
    }

    /// Subscribes (read-only) the status properties and the effect-slot properties present.
    fn subscribe(&mut self) {
        let Some(h) = &self.handle else { return };
        let mut want: Vec<String> = STATUS_PATHS
            .iter()
            .filter(|p| self.tree.has_property(p))
            .map(|p| p.to_string())
            .collect();
        for device in self.tree.children("/devices") {
            for coll in ["tracks", "buses"] {
                let cp = format!("/devices/{device}/{coll}");
                for uid in self.tree.children(&cp) {
                    let ep = format!("{cp}/{uid}/effects");
                    for slot in self.tree.children(&ep) {
                        want.extend(
                            SLOT_PROPS
                                .iter()
                                .map(|k| format!("{ep}/{slot}/{k}"))
                                .filter(|p| self.tree.has_property(p)),
                        );
                    }
                }
            }
        }
        for p in want {
            if self.subscribed.insert(p.clone()) {
                let _ = h.send(Command::Subscribe(path::value_path(&p)));
            }
        }
    }

    fn poll(&mut self) {
        if self.status != Status::Live || self.handle.is_none() {
            return;
        }
        let mut done = Vec::new();
        self.polls.retain(|(node, reply)| match reply.try_take() {
            None => true,
            Some(Ok(msg)) => {
                if let Some(data) = msg.data {
                    done.push((node.clone(), data));
                }
                false
            }
            Some(Err(_)) => false,
        });
        let mut changed = false;
        for (node, data) in done {
            let before = self.tree.node_json(&node);
            let converted = engine_node_from_helper(&data);
            self.tree.insert_node(&converted, &node);
            if self.tree.node_json(&node) != before {
                changed = true;
            }
        }
        if changed {
            self.revision += 1;
            self.subscribe();
        }
        let due = self
            .last_poll
            .is_none_or(|t| t.elapsed() >= self.poll_interval);
        let Some(h) = &self.handle else { return };
        if due && self.polls.is_empty() {
            self.last_poll = Some(Instant::now());
            for device in self.tree.children("/devices") {
                let node = format!("/devices/{device}/tracks");
                let reply = h.request(&node, true);
                self.polls.push((node, reply));
            }
        }
    }

    /// When [`HelperStore::drain`] next has timed work.
    pub fn next_deadline(&self) -> Option<Instant> {
        if self.status != Status::Live {
            return None;
        }
        self.last_poll.map(|t| t + self.poll_interval)
    }
}
