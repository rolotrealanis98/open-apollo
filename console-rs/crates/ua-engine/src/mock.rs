//! A loopback stand-in for UA Mixer Engine, serving a [`Tree`] with the wire behaviour verified
//! against engine 12.0.0:
//!
//! - `get` answers nodes (recursive, or with child stubs), properties, `/value`, `/values`,
//!   `/enabled`, `/default`, echoing `func_id`/`recursive` in `parameters`; unknown paths get
//!   `{"error":"Unable to resolve path for get."}`.
//! - `subscribe mode=multi` switches pushes to `{"path":"multi","data":[…]}` envelopes; a batch
//!   subscribe is answered with one `multi` message of current values; a single subscribe with
//!   one value message. Batch unsubscribe is ignored (as on the real engine).
//! - `set` clamps to min/max, pushes the value and its dB companion to every subscriber
//!   (including the sender), and is recorded.
//! - Optional meter simulation: `/MeterPulse` and delta-gated meter pushes every 30 ms.
//!
//! With [`MockOptions::daemon_style`] it speaks like the open-apollo daemon instead (verified
//! read-only against it): `subscribe mode=multi` is ignored, so every push and every batch
//! subscribe reply is a plain `{"path":…,"data":…}` message; `func_id` is echoed as a string; and
//! errors carry no `parameters`.
//!
//! It runs on its own thread and tokio runtime, so tests stay synchronous.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio::task::AbortHandle;

use crate::connection::Endpoint;
use crate::framing::NulFramer;
use crate::path;
use crate::tree::Tree;
use crate::value::{Json, Value};
use crate::writer::companion;

#[derive(Clone, Debug)]
pub struct MockOptions {
    pub simulate_meters: bool,
    pub meter_tick: Duration,
    /// 0 picks a free port.
    pub port: u16,
    /// Speak like the open-apollo daemon (see the module docs).
    pub daemon_style: bool,
    /// Apply a client's `set` without echoing it to anyone (no push, no reply): a slow or
    /// lossy engine, for tests of what a client does when its own echo never comes.
    pub withhold_echoes: bool,
}

impl Default for MockOptions {
    fn default() -> Self {
        MockOptions {
            simulate_meters: false,
            meter_tick: Duration::from_millis(30),
            port: 0,
            daemon_style: false,
            withhold_echoes: false,
        }
    }
}

struct Client {
    tx: mpsc::UnboundedSender<Vec<u8>>,
    multi: bool,
    subs: HashSet<String>,
    aborts: Vec<AbortHandle>,
}

#[derive(Default)]
struct State {
    tree: Tree,
    clients: HashMap<u64, Client>,
    next_client: u64,
    received: Vec<String>,
    pulse: i64,
    meter_state: HashMap<String, f64>,
    daemon_style: bool,
    withhold_echoes: bool,
    read_rpc_replies: HashMap<String, Json>,
    /// `set`s answered with `{"error":"Unable to set value."}` and no change: (property, value).
    refused: Vec<(String, Value)>,
}

type Shared = Arc<Mutex<State>>;

fn lock(s: &Shared) -> std::sync::MutexGuard<'_, State> {
    s.lock().unwrap_or_else(|e| e.into_inner())
}

pub struct MockEngine {
    port: u16,
    state: Shared,
    shutdown: Option<oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl MockEngine {
    /// Synthetic replies for the audited read RPCs; no hardware/vendor execution.
    pub fn set_read_rpc_reply(&self, path: &str, data: Json) {
        assert!(matches!(path, "/GetMidiBuffers" | "/GetCorrectionImage"));
        lock(&self.state).read_rpc_replies.insert(path.to_owned(), data);
    }
    /// Starts listening on 127.0.0.1.
    pub fn start(tree: Tree, options: MockOptions) -> std::io::Result<MockEngine> {
        let std_listener = std::net::TcpListener::bind(("127.0.0.1", options.port))?;
        std_listener.set_nonblocking(true)?;
        let port = std_listener.local_addr()?.port();
        let state: Shared = Arc::new(Mutex::new(State {
            tree,
            daemon_style: options.daemon_style,
            withhold_echoes: options.withhold_echoes,
            ..Default::default()
        }));
        let (stx, srx) = oneshot::channel();
        let st = state.clone();
        let thread = std::thread::Builder::new().name(format!("mock-engine :{port}")).spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("tokio runtime");
            rt.block_on(async move {
                let listener = TcpListener::from_std(std_listener).expect("listener");
                if options.simulate_meters {
                    tokio::spawn(meter_loop(st.clone(), options.meter_tick));
                }
                tokio::select! {
                    _ = accept_loop(listener, st) => {}
                    _ = srx => {}
                }
            });
        })?;
        Ok(MockEngine { port, state, shutdown: Some(stx), thread: Some(thread) })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn endpoint(&self) -> Endpoint {
        Endpoint::new("127.0.0.1", self.port)
    }

    /// Every text frame received, in order.
    pub fn received_commands(&self) -> Vec<String> {
        lock(&self.state).received.clone()
    }

    /// Received `set` commands only (not the per-socket `set /Sleep` hint, which
    /// [`received_commands`](Self::received_commands) keeps), without Console's per-request query (`func_id`,
    /// `context_type`; [`Command::strip_request_query`](crate::Command::strip_request_query)).
    pub fn set_commands(&self) -> Vec<String> {
        lock(&self.state)
            .received
            .iter()
            .filter(|c| c.starts_with("set ") && !is_sleep_hint(c))
            .map(|c| crate::Command::strip_request_query(c))
            .collect()
    }

    /// Mutating `set`/`post` commands, without Console's per-request query.
    /// Audited read RPCs stay available in `received_commands`.
    pub fn write_commands(&self) -> Vec<String> {
        lock(&self.state)
            .received
            .iter()
            .filter(|c| (c.starts_with("set ") || c.starts_with("post ")) && !is_sleep_hint(c) && !is_read_rpc(c))
            .map(|c| crate::Command::strip_request_query(c))
            .collect()
    }

    /// Received `set` and `post` commands exactly as sent (with `func_id` / `context_type`), but
    /// not the `set /Sleep` socket hint or audited read RPCs.
    pub fn raw_write_commands(&self) -> Vec<String> {
        lock(&self.state)
            .received
            .iter()
            .filter(|c| (c.starts_with("set ") || c.starts_with("post ")) && !is_sleep_hint(c) && !is_read_rpc(c))
            .cloned()
            .collect()
    }

    pub fn current_tree(&self) -> Tree {
        lock(&self.state).tree.clone()
    }

    pub fn value(&self, path: &str) -> Option<Value> {
        lock(&self.state).tree.value(path).cloned()
    }

    pub fn client_count(&self) -> usize {
        lock(&self.state).clients.len()
    }

    /// Simulates an engine-side change (another client, the hardware): updates and pushes.
    pub fn push(&self, path: &str, value: impl Into<Value>) {
        let mut s = lock(&self.state);
        change(&mut s, path::property_path(path), value.into());
    }

    /// Refuses `set <property>/value <value>` as UA Mixer Engine does when the hardware does not
    /// take it (a sample rate the driver refuses): `{"path", "parameters", "error":"Unable to set
    /// value."}`, nothing changes, nothing is pushed.
    pub fn refuse(&self, property: &str, value: impl Into<Value>) {
        lock(&self.state).refused.push((path::property_path(property).to_owned(), value.into()));
    }

    /// Replaces the subtree at `path` (no push: clients see it on their next `get`), e.g. an
    /// insert's `parameters` published after its metadata.
    pub fn set_node(&self, path: &str, node: &Json) {
        lock(&self.state).tree.insert_node(node, path);
    }

    /// Replaces a property's `values` list (no push: clients re-read lists with `get …/values`).
    pub fn set_values(&self, prop: &str, values: Json) {
        set_values(&mut lock(&self.state), path::property_path(prop), values);
    }

    /// Drops every client connection (to exercise reconnects).
    pub fn drop_clients(&self) {
        let mut s = lock(&self.state);
        for (_, c) in s.clients.drain() {
            for a in c.aborts {
                a.abort();
            }
        }
    }

    pub fn stop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        lock(&self.state).clients.clear();
    }
}

impl Drop for MockEngine {
    fn drop(&mut self) {
        self.stop();
    }
}

async fn accept_loop(listener: TcpListener, state: Shared) {
    while let Ok((stream, _)) = listener.accept().await {
        let _ = stream.set_nodelay(true);
        let (tx, rx) = mpsc::unbounded_channel();
        let id = {
            let mut s = lock(&state);
            s.next_client += 1;
            let id = s.next_client;
            s.clients.insert(id, Client { tx, multi: false, subs: HashSet::new(), aborts: Vec::new() });
            id
        };
        let (rd, wr) = stream.into_split();
        let w = tokio::spawn(write_loop(wr, rx));
        let r = tokio::spawn(read_loop(rd, id, state.clone()));
        if let Some(c) = lock(&state).clients.get_mut(&id) {
            c.aborts = vec![w.abort_handle(), r.abort_handle()];
        }
    }
}

async fn write_loop(mut wr: tokio::net::tcp::OwnedWriteHalf, mut rx: mpsc::UnboundedReceiver<Vec<u8>>) {
    while let Some(mut buf) = rx.recv().await {
        while let Ok(more) = rx.try_recv() {
            buf.extend(more);
        }
        if wr.write_all(&buf).await.is_err() {
            break;
        }
    }
}

async fn read_loop(mut rd: tokio::net::tcp::OwnedReadHalf, id: u64, state: Shared) {
    let mut framer = NulFramer::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = match rd.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        let frames = framer.push_text(&buf[..n]);
        let mut s = lock(&state);
        for f in frames {
            handle(&mut s, id, f);
        }
    }
    if let Some(c) = lock(&state).clients.remove(&id) {
        for a in c.aborts {
            a.abort();
        }
    }
}

async fn meter_loop(state: Shared, tick: Duration) {
    let mut iv = tokio::time::interval(tick);
    iv.tick().await;
    loop {
        iv.tick().await;
        let mut s = lock(&state);
        s.pulse += 1;
        let t = s.pulse as f64 * tick.as_secs_f64();
        let pulse = s.pulse;
        let ids: Vec<u64> = s.clients.keys().copied().collect();
        for id in ids {
            let subs: Vec<String> = s.clients[&id].subs.iter().cloned().collect();
            let mut items: Vec<(String, Value)> = Vec::new();
            if subs.iter().any(|p| p == "/MeterPulse") {
                items.push(("/MeterPulse".into(), Value::Number(pulse as f64)));
            }
            for p in subs.iter().filter(|p| p.ends_with("/MeterLevel") || p.ends_with("/MeterPeakLevel")) {
                let seed = (fnv(p) % 97) as f64;
                let level = ((-30.0 + 20.0 * (t * 1.3 + seed).sin()).clamp(-77.0, 0.0) * 10.0).round() / 10.0;
                if s.meter_state.get(p) != Some(&level) {
                    s.meter_state.insert(p.clone(), level);
                    items.push((p.clone(), Value::Number(level)));
                }
            }
            if let Some(c) = s.clients.get(&id) {
                deliver(c, &items);
            }
        }
    }
}

fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3))
}

fn send_json(c: &Client, json: &Json) {
    let _ = c.tx.send(NulFramer::encode(&json.to_string()));
}

fn reply(c: &Client, path: &str, data: Option<Json>, params: &serde_json::Map<String, Json>, error: Option<&str>) {
    let mut o = serde_json::Map::new();
    o.insert("path".into(), Json::String(path.into()));
    if !params.is_empty() {
        o.insert("parameters".into(), Json::Object(params.clone()));
    }
    if let Some(d) = data {
        o.insert("data".into(), d);
    }
    if let Some(e) = error {
        o.insert("error".into(), Json::String(e.into()));
    }
    send_json(c, &Json::Object(o));
}

fn deliver(c: &Client, items: &[(String, Value)]) {
    if items.is_empty() {
        return;
    }
    if c.multi {
        let data: Vec<Json> =
            items.iter().map(|(p, v)| serde_json::json!({"path": format!("{p}/value"), "data": v.to_json()})).collect();
        send_json(c, &serde_json::json!({"path": "multi", "data": data}));
    } else {
        for (p, v) in items {
            send_json(c, &serde_json::json!({"path": format!("{p}/value"), "data": v.to_json()}));
        }
    }
}

fn split_first(s: &str) -> (&str, &str) {
    s.split_once(' ').unwrap_or((s, ""))
}

fn split_query(s: &str) -> (&str, Vec<(String, String)>) {
    let Some((p, q)) = s.split_once('?') else {
        return (s, Vec::new());
    };
    let query = q
        .split('&')
        .filter(|kv| !kv.is_empty())
        .map(|kv| match kv.split_once('=') {
            Some((k, v)) => (k.to_owned(), v.to_owned()),
            None => (kv.to_owned(), String::new()),
        })
        .collect();
    (p, query)
}

fn echo_params(query: &[(String, String)], daemon_style: bool) -> serde_json::Map<String, Json> {
    if daemon_style {
        return query.iter().map(|(k, v)| (k.clone(), Json::String(v.clone()))).collect();
    }
    query
        .iter()
        .map(|(k, v)| {
            let j = if let Ok(i) = v.parse::<i64>() {
                Json::from(i)
            } else {
                v.parse::<f64>()
                    .ok()
                    .and_then(serde_json::Number::from_f64)
                    .map(Json::Number)
                    .unwrap_or_else(|| Json::String(v.clone()))
            };
            (k.clone(), j)
        })
        .collect()
}

fn handle(s: &mut State, id: u64, text: String) {
    s.received.push(text.clone());
    if text.starts_with('{') {
        return; // identity
    }
    let (verb, rest) = split_first(&text);
    match verb {
        "get" => {
            let (p, query) = split_query(rest);
            get(s, id, p, &query);
        }
        "set" => {
            let (target, value_text) = split_first(rest);
            let (p, query) = split_query(target);
            set(s, id, p, value_text, &query);
        }
        "subscribe" => {
            if rest == "mode=multi" {
                if let Some(c) = s.clients.get_mut(&id)
                    && !s.daemon_style
                {
                    c.multi = true;
                }
            } else if rest.starts_with('{') {
                let paths: Vec<String> = serde_json::from_str::<Json>(rest)
                    .ok()
                    .and_then(|j| j.get("paths").and_then(Json::as_array).cloned())
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|p| p.as_str().map(str::to_owned))
                    .collect();
                let mut items = Vec::new();
                for p in &paths {
                    let prop = path::property_path(p).to_owned();
                    if let Some(v) = s.tree.value(&prop) {
                        items.push(serde_json::json!({"path": p, "data": v.to_json()}));
                    }
                    if let Some(c) = s.clients.get_mut(&id) {
                        c.subs.insert(prop);
                    }
                }
                if let Some(c) = s.clients.get(&id) {
                    if s.daemon_style {
                        for item in &items {
                            send_json(c, item);
                        }
                    } else {
                        send_json(c, &serde_json::json!({"path": "multi", "data": items}));
                    }
                }
            } else {
                let prop = path::property_path(split_query(rest).0).to_owned();
                let v = s.tree.value(&prop).cloned();
                if let Some(c) = s.clients.get_mut(&id) {
                    c.subs.insert(prop.clone());
                    if let Some(v) = v {
                        reply(c, &path::value_path(&prop), Some(v.to_json()), &Default::default(), None);
                    }
                }
            }
        }
        "unsubscribe" => {
            if rest.starts_with('{') {
                return; // the engine ignores batch unsubscribe
            }
            let prop = path::property_path(split_query(rest).0).to_owned();
            if let Some(c) = s.clients.get_mut(&id) {
                c.subs.remove(&prop);
            }
        }
        "post" => {
            let (target, body) = split_first(rest);
            let (path, query) = split_query(target);
            if let Some(data) = s.read_rpc_replies.get(path) {
                if let Some(c) = s.clients.get(&id) {
                    reply(c, path, Some(data.clone()), &echo_params(&query, s.daemon_style), None);
                }
            } else {
                session_post(s, path, body);
            }
        }
        _ => {} // unknown verbs: recorded only
    }
}

/// A stand-in for the engine's session handling, so the session UI can be driven offline. The
/// command shapes are Console's; the effects are assumed (no capture of the real engine saving
/// or loading exists): Save clears `/Dirty`, Save As adds the file to `/Session/values` and makes
/// it current, New empties `/Session`, Delete/Rename edit the list. Other posts are recorded only.
fn session_post(s: &mut State, command: &str, body: &str) {
    use crate::session;
    let args: Json = serde_json::from_str(body).unwrap_or(Json::Null);
    let arg = |k: &str| args.get(k).and_then(Json::as_str).unwrap_or_default().to_owned();
    let mut files: Vec<String> = s
        .tree
        .meta(session::SESSION)
        .and_then(|m| m.values.clone())
        .map(|v| session::parse_entries(&v))
        .unwrap_or_default()
        .iter()
        .flat_map(|e| e.files().into_iter().map(|f| f.path().to_owned()).collect::<Vec<_>>())
        .collect();
    let strip_ext = |n: &str| {
        let n = n.rsplit('/').next().unwrap_or(n);
        n.strip_suffix(session::EXTENSION).unwrap_or(n).to_owned()
    };
    let before = files.clone();
    match command {
        "/SaveSession" => change(s, session::DIRTY, Value::Bool(false)),
        "/SaveSessionAs" => {
            let name = strip_ext(&arg("name"));
            if !files.contains(&name) {
                files.push(name.clone());
            }
            change(s, session::SESSION, Value::String(name));
            change(s, session::DIRTY, Value::Bool(false));
        }
        "/NewSession" => {
            change(s, session::SESSION, Value::String(String::new()));
            change(s, session::DIRTY, Value::Bool(false));
        }
        "/OpenSession" => {
            change(s, session::SESSION, Value::String(strip_ext(&arg("path"))));
            change(s, session::DIRTY, Value::Bool(false));
        }
        "/DeleteSession" => files.retain(|f| *f != strip_ext(&arg("name"))),
        "/RenameSession" => {
            let (from, to) = (strip_ext(&arg("origName")), strip_ext(&arg("newName")));
            for f in files.iter_mut().filter(|f| **f == from) {
                *f = to.clone();
            }
        }
        _ => {}
    }
    if files != before {
        let list: Vec<Json> =
            files.iter().map(|f| serde_json::json!({"value": f, "type": "file", "locked": false})).collect();
        set_values(s, session::SESSION, Json::Array(list));
    }
}

fn set_values(s: &mut State, prop: &str, values: Json) {
    if let Some(mut p) = s.tree.property(prop).cloned() {
        p.meta.values = Some(values);
        s.tree.set_property(prop, p);
    }
}

const GET_ERROR: &str = "Unable to resolve path for get.";

fn get(s: &State, id: u64, raw: &str, query: &[(String, String)]) {
    let Some(c) = s.clients.get(&id) else { return };
    let params = echo_params(query, s.daemon_style);
    // The daemon's errors carry no parameters (so no func_id).
    let err_params = if s.daemon_style { serde_json::Map::new() } else { params.clone() };
    let recursive = query.iter().any(|(k, v)| k == "recursive" && v != "0");
    let p = if raw.len() > 1 && raw.ends_with('/') { &raw[..raw.len() - 1] } else { raw };
    if s.tree.has_node(p) {
        let mut json = s.tree.node_json(p);
        if !recursive && let Some(Json::Object(kids)) = json.get_mut("children") {
            for v in kids.values_mut() {
                *v = Json::Object(Default::default());
            }
        }
        reply(c, raw, Some(json), &params, None);
        return;
    }
    let (node, selector) = path::split(p);
    if let Some(prop) = s.tree.property(p).filter(|_| selector != "value") {
        reply(c, raw, Some(prop.to_json()), &params, None);
    } else if let Some(prop) = s.tree.property(node) {
        let data = match selector {
            "value" => Some(prop.value.as_ref().map(Value::to_json).unwrap_or(Json::Null)),
            "values" => Some(prop.meta.values.clone().unwrap_or(Json::Array(Vec::new()))),
            "enabled" => Some(Json::Bool(prop.meta.enabled.unwrap_or(true))),
            "default" => {
                Some(prop.meta.default.as_ref().or(prop.value.as_ref()).map(Value::to_json).unwrap_or(Json::Null))
            }
            _ => None,
        };
        match data {
            Some(d) => reply(c, raw, Some(d), &params, None),
            None => reply(c, raw, None, &err_params, Some(GET_ERROR)),
        }
    } else {
        reply(c, raw, None, &err_params, Some(GET_ERROR));
    }
}

fn set(s: &mut State, id: u64, raw: &str, value_text: &str, query: &[(String, String)]) {
    let prop = path::property_path(raw).to_owned();
    let Some(meta) = s.tree.meta(&prop).cloned() else {
        if let Some(c) = s.clients.get(&id) {
            let params = if s.daemon_style { Default::default() } else { echo_params(query, false) };
            reply(c, raw, None, &params, Some("Unable to resolve path for set."));
        }
        return;
    };
    let parsed = serde_json::from_str::<Json>(value_text).unwrap_or_else(|_| Json::String(value_text.to_owned()));
    let mut v = Value::from_json_owned(parsed);
    match meta.ty.as_deref() {
        Some("bool") => v = Value::Bool(v.as_bool().unwrap_or(value_text == "true")),
        Some("float" | "int" | "int64") => {
            if let Some(mut d) = v.as_f64() {
                if let Some(lo) = meta.min {
                    d = d.max(lo);
                }
                if let Some(hi) = meta.max {
                    d = d.min(hi);
                }
                v = Value::Number(if meta.is_integral() { d.round() } else { d });
            }
        }
        _ => {}
    }
    if s.refused.iter().any(|(p, r)| *p == prop && *r == v) {
        if let Some(c) = s.clients.get(&id) {
            let params = if s.daemon_style { Default::default() } else { echo_params(query, false) };
            reply(c, raw, None, &params, Some("Unable to set value."));
        }
        return;
    }
    if s.withhold_echoes {
        s.tree.set_value(&prop, v);
        return;
    }
    change(s, &prop, v.clone());
    if prop == crate::session::SESSION || prop == crate::session::MODIFIED_SESSIONS {
        // Loading a session (assumed): the loaded session is clean.
        change(s, crate::session::DIRTY, Value::Bool(false));
    }
    if query.iter().any(|(k, _)| k == "func_id")
        && let Some(c) = s.clients.get(&id)
    {
        reply(c, raw, Some(v.to_json()), &echo_params(query, s.daemon_style), None);
    }
}

/// Applies a value and pushes it (plus its dB companion) to subscribers.
fn change(s: &mut State, prop: &str, v: Value) {
    let mut updates = vec![(prop.to_owned(), v.clone())];
    if let Some((cpath, db)) = v.as_f64().and_then(|t| companion(prop, t))
        && s.tree.has_property(&cpath)
    {
        updates.push((cpath, Value::Number(db)));
    }
    for (p, value) in &updates {
        s.tree.set_value(p, value.clone());
    }
    for c in s.clients.values() {
        let items: Vec<(String, Value)> = updates.iter().filter(|(p, _)| c.subs.contains(p)).cloned().collect();
        deliver(c, &items);
    }
}

// MARK: - Mock UA Mixer Helper (4720)

/// A loopback stand-in for the UA Mixer Helper, serving a helper-format tree (`children` as
/// arrays of `{"path": name, …}`) with the behaviour verified against helper 1.3.1.1152: `get`
/// (nodes, recursive or with `{"path"}` stubs; properties, `/value`, `/values`), `subscribe`
/// acknowledged with the current value (`null` for nodes), `get /ping`, errors
/// `uasrv.invalid_object` with the echoed `parameters`. Value pushes after [`MockHelper::push`]
/// use `{"path":"<property>/value","data":…}` (an assumption: no change was observed live).
/// `set`/`post` are recorded and answered with `uasrv.read_only`; nothing changes.
pub struct MockHelper {
    port: u16,
    state: Arc<Mutex<HelperState>>,
    shutdown: Option<oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

struct HelperClient {
    tx: mpsc::UnboundedSender<Vec<u8>>,
    subs: HashSet<String>,
    aborts: Vec<AbortHandle>,
}

#[derive(Default)]
struct HelperState {
    root: Json,
    clients: HashMap<u64, HelperClient>,
    next_client: u64,
    received: Vec<String>,
}

fn hlock(s: &Arc<Mutex<HelperState>>) -> std::sync::MutexGuard<'_, HelperState> {
    s.lock().unwrap_or_else(|e| e.into_inner())
}

impl MockHelper {
    /// Serves `root` (a helper root node) on 127.0.0.1 (`port` 0 picks a free port).
    pub fn start(root: Json, port: u16) -> std::io::Result<MockHelper> {
        let std_listener = std::net::TcpListener::bind(("127.0.0.1", port))?;
        std_listener.set_nonblocking(true)?;
        let port = std_listener.local_addr()?.port();
        let state = Arc::new(Mutex::new(HelperState { root, ..Default::default() }));
        let (stx, srx) = oneshot::channel();
        let st = state.clone();
        let thread = std::thread::Builder::new().name(format!("mock-helper :{port}")).spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("tokio runtime");
            rt.block_on(async move {
                let listener = TcpListener::from_std(std_listener).expect("listener");
                tokio::select! {
                    _ = helper_accept(listener, st) => {}
                    _ = srx => {}
                }
            });
        })?;
        Ok(MockHelper { port, state, shutdown: Some(stx), thread: Some(thread) })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn endpoint(&self) -> Endpoint {
        Endpoint::new("127.0.0.1", self.port)
    }

    pub fn received_commands(&self) -> Vec<String> {
        hlock(&self.state).received.clone()
    }

    /// Changes a property and pushes it to its subscribers.
    pub fn push(&self, path: &str, value: Json) {
        let mut s = hlock(&self.state);
        let prop = path::property_path(path).to_owned();
        let (node, name) = path::split(&prop);
        if let Some(n) = helper_node_mut(&mut s.root, node) {
            let props = n
                .as_object_mut()
                .expect("node object")
                .entry("properties")
                .or_insert_with(|| Json::Object(Default::default()));
            if let Some(p) = props.as_object_mut() {
                let entry = p.entry(name.to_owned()).or_insert_with(|| serde_json::json!({}));
                if let Some(e) = entry.as_object_mut() {
                    e.insert("value".into(), value.clone());
                }
            }
        }
        let msg = serde_json::json!({"path": format!("{prop}/value"), "data": value});
        for c in s.clients.values().filter(|c| c.subs.contains(&prop)) {
            let _ = c.tx.send(NulFramer::encode(&msg.to_string()));
        }
    }

    /// Replaces (or adds) the child node `name` of `parent` (structural change; nothing is
    /// pushed, as on the real helper's value subscriptions).
    pub fn set_child(&self, parent: &str, name: &str, mut node: Json) {
        let mut s = hlock(&self.state);
        if let Some(o) = node.as_object_mut() {
            o.insert("path".into(), Json::String(name.to_owned()));
        }
        if let Some(n) = helper_node_mut(&mut s.root, parent) {
            let kids =
                n.as_object_mut().expect("node object").entry("children").or_insert_with(|| Json::Array(Vec::new()));
            if let Some(list) = kids.as_array_mut() {
                list.retain(|c| c.get("path").and_then(Json::as_str) != Some(name));
                list.push(node);
            }
        }
    }

    pub fn stop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        hlock(&self.state).clients.clear();
    }
}

impl Drop for MockHelper {
    fn drop(&mut self) {
        self.stop();
    }
}

fn helper_child<'a>(node: &'a Json, name: &str) -> Option<&'a Json> {
    node.get("children")?.as_array()?.iter().find(|c| c.get("path").and_then(Json::as_str) == Some(name))
}

/// The helper node at `path` (`/` is the root).
fn helper_node<'a>(root: &'a Json, path: &str) -> Option<&'a Json> {
    path.split('/').filter(|c| !c.is_empty()).try_fold(root, helper_child)
}

fn helper_node_mut<'a>(root: &'a mut Json, path: &str) -> Option<&'a mut Json> {
    let mut n = root;
    for comp in path.split('/').filter(|c| !c.is_empty()) {
        n = n
            .get_mut("children")?
            .as_array_mut()?
            .iter_mut()
            .find(|c| c.get("path").and_then(Json::as_str) == Some(comp))?;
    }
    Some(n)
}

fn helper_property<'a>(root: &'a Json, path: &str) -> Option<&'a Json> {
    let (node, name) = path::split(path);
    helper_node(root, node)?.get("properties")?.get(name)
}

/// A node as a non-recursive `get` returns it: children reduced to `{"path"}`.
fn helper_shallow(node: &Json) -> Json {
    let mut n = node.clone();
    if let Some(Json::Array(kids)) = n.get_mut("children") {
        for k in kids.iter_mut() {
            let name = k.get("path").cloned().unwrap_or(Json::Null);
            *k = serde_json::json!({ "path": name });
        }
    }
    if let Some(o) = n.as_object_mut() {
        o.remove("path");
    }
    n
}

async fn helper_accept(listener: TcpListener, state: Arc<Mutex<HelperState>>) {
    while let Ok((stream, _)) = listener.accept().await {
        let _ = stream.set_nodelay(true);
        let (tx, rx) = mpsc::unbounded_channel();
        let id = {
            let mut s = hlock(&state);
            s.next_client += 1;
            let id = s.next_client;
            s.clients.insert(id, HelperClient { tx, subs: HashSet::new(), aborts: Vec::new() });
            id
        };
        let (mut rd, wr) = stream.into_split();
        let w = tokio::spawn(write_loop(wr, rx));
        let st = state.clone();
        let r = tokio::spawn(async move {
            let mut framer = NulFramer::new();
            let mut buf = vec![0u8; 1 << 16];
            loop {
                let n = match rd.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                let frames = framer.push_text(&buf[..n]);
                let mut s = hlock(&st);
                for f in frames {
                    helper_handle(&mut s, id, f);
                }
            }
            hlock(&st).clients.remove(&id);
        });
        if let Some(c) = hlock(&state).clients.get_mut(&id) {
            c.aborts = vec![w.abort_handle(), r.abort_handle()];
        }
    }
}

fn helper_reply(s: &HelperState, id: u64, json: Json) {
    if let Some(c) = s.clients.get(&id) {
        let _ = c.tx.send(NulFramer::encode(&json.to_string()));
    }
}

fn helper_handle(s: &mut HelperState, id: u64, text: String) {
    s.received.push(text.clone());
    if text.starts_with('{') {
        return; // identity
    }
    let (verb, rest) = split_first(&text);
    let (target, _) = split_first(rest);
    let (raw, query) = split_query(target);
    let params = Json::Object(echo_params(&query, false));
    let with = |mut o: serde_json::Map<String, Json>| {
        if params.as_object().is_some_and(|p| !p.is_empty()) {
            o.insert("parameters".into(), params.clone());
        }
        Json::Object(o)
    };
    let mut out = serde_json::Map::new();
    out.insert("path".into(), Json::String(raw.to_owned()));
    match verb {
        "get" if raw == "/ping" || raw == "ping" => {
            out.insert("data".into(), Json::Bool(true));
        }
        "get" | "subscribe" => {
            let recursive = query.iter().any(|(k, v)| k == "recursive" && v != "0");
            let p = if raw.len() > 1 && raw.ends_with('/') { &raw[..raw.len() - 1] } else { raw };
            let prop_path = path::property_path(p).to_owned();
            let (node_part, selector) = path::split(p);
            let data = if let Some(node) = helper_node(&s.root, p) {
                if verb == "subscribe" {
                    Some(Json::Null)
                } else if recursive {
                    let mut n = node.clone();
                    if let Some(o) = n.as_object_mut() {
                        o.remove("path");
                    }
                    Some(n)
                } else {
                    Some(helper_shallow(node))
                }
            } else if selector == "values" {
                helper_property(&s.root, node_part).map(|p| p.get("values").cloned().unwrap_or(Json::Array(Vec::new())))
            } else {
                helper_property(&s.root, &prop_path).map(|p| p.get("value").cloned().unwrap_or(Json::Null))
            };
            match data {
                Some(d) => {
                    if verb == "subscribe" {
                        let key = if helper_node(&s.root, p).is_some() { p.to_owned() } else { prop_path };
                        if let Some(c) = s.clients.get_mut(&id) {
                            c.subs.insert(key);
                        }
                    }
                    out.insert("data".into(), d);
                }
                None => {
                    out.insert("error".into(), Json::String("uasrv.invalid_object".into()));
                }
            }
        }
        "unsubscribe" => {
            let prop = path::property_path(raw).to_owned();
            if let Some(c) = s.clients.get_mut(&id) {
                c.subs.remove(&prop);
            }
            return;
        }
        _ => {
            out.insert("error".into(), Json::String("uasrv.read_only".into()));
        }
    }
    let reply = with(out);
    helper_reply(s, id, reply);
}

/// `set /Sleep …` / `set /sleep …`: a per-socket hint, not a mixer write.
fn is_sleep_hint(c: &str) -> bool {
    c.starts_with("set /Sleep ")
        || c.starts_with("set /sleep ")
        || c.starts_with("set /Sleep?")
        || c.starts_with("set /sleep?")
}

fn is_read_rpc(c: &str) -> bool {
    let Some(rest) = c.strip_prefix("post ") else { return false };
    let (target, body) = split_first(rest);
    let Ok(Json::Object(args)) = serde_json::from_str::<Json>(body) else { return false };
    match split_query(target).0 {
        "/GetMidiBuffers" => args.is_empty(),
        "/GetCorrectionImage" => {
            args.len() == 1
                && args
                    .get("type")
                    .and_then(Json::as_str)
                    .is_some_and(|s| matches!(s, "speaker" | "headphone" | "headphone2"))
        }
        _ => false,
    }
}
