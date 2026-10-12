//! TCP connection to a UA Mixer Engine (4710): NUL framing, identity handshake, `func_id` request
//! correlation and reconnect with backoff, on a tokio runtime on its own thread.
//!
//! **Read-only by default.** Unless [`Options::allow_writes`] is set, `set` and `post` are refused
//! by [`EngineHandle::send`], below every other gate, so nothing can change a live mixer by
//! accident.
//!
//! Meter pushes go straight to the [`MeterStore`] on the network thread. Everything else reaches
//! the owner as [`Update`]s on a channel (drained by [`Store::drain`](crate::Store::drain) on
//! the UI thread).

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc as tmpsc;

use crate::command::Command;
use crate::error::{Error, Result};
use crate::framing::NulFramer;
use crate::message::Message;
use crate::meters::MeterStore;
use crate::path;
use crate::tree::{SnapshotInfo, Tree};
use crate::value::Value;

pub const DEFAULT_PORT: u16 = 4710;

/// Where an engine is.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
    /// Display name (a Bonjour instance name, or `host:port`).
    pub name: String,
}

impl Endpoint {
    pub fn new(host: impl Into<String>, port: u16) -> Endpoint {
        let host = host.into();
        Endpoint {
            name: format!("{host}:{port}"),
            host,
            port,
        }
    }

    /// `host`, `host:port` or `[v6]:port`; the port defaults to 4710.
    pub fn parse(text: &str) -> Endpoint {
        if let Some(rest) = text.strip_prefix('[')
            && let Some((h, tail)) = rest.split_once(']')
        {
            let port = tail
                .strip_prefix(':')
                .and_then(|p| p.parse().ok())
                .unwrap_or(DEFAULT_PORT);
            return Endpoint::new(h, port);
        }
        match text.rsplit_once(':') {
            Some((h, p)) if !h.contains(':') => match p.parse() {
                Ok(port) => Endpoint::new(h, port),
                Err(_) => Endpoint::new(text, DEFAULT_PORT),
            },
            _ => Endpoint::new(text, DEFAULT_PORT),
        }
    }
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.host, self.port)
    }
}

/// Connection options. The defaults are read-only.
#[derive(Clone, Debug)]
pub struct Options {
    /// Sent as `{"path":"client_id","data":…}` (Console sends `console`).
    pub client_id: String,
    /// 32 hex digits identifying this client machine, sent as `{"path":"networkID",…}`.
    pub network_id: String,
    /// The identity key for `network_id`: `networkID` on the engine (4710), `network_id` on the
    /// UA Mixer Helper (4720).
    pub network_id_key: String,
    /// Send `get /ping` this often (the helper's keep-alive; Console pings every 5 s). The
    /// `{"path":"/ping","data":true}` replies are consumed.
    pub ping_interval: Option<Duration>,
    /// Permit `set`/`post`. Leave false against a real engine unless the user asked for writes.
    pub allow_writes: bool,
    pub send_identity: bool,
    pub reconnect: bool,
    pub min_backoff: Duration,
    pub max_backoff: Duration,
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
    /// Fetch the whole tree (`get /?recursive=1`) on every (re)connect and deliver it as
    /// [`Update::Tree`]. The [`Store`](crate::Store) needs this.
    pub load_tree: bool,
    pub tree_timeout: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            client_id: "uaconsole".into(),
            network_id: random_network_id(),
            network_id_key: "networkID".into(),
            ping_interval: None,
            allow_writes: false,
            send_identity: true,
            reconnect: true,
            min_backoff: Duration::from_millis(500),
            max_backoff: Duration::from_secs(10),
            connect_timeout: Duration::from_secs(5),
            request_timeout: Duration::from_secs(20),
            load_tree: true,
            tree_timeout: Duration::from_secs(60),
        }
    }
}

impl Options {
    /// Read-only options for the UA Mixer Helper on 4720: `network_id` identity, a 5 s
    /// `get /ping` keep-alive, no engine-tree load (the [`HelperStore`](crate::HelperStore) reads
    /// the helper's own tree format).
    pub fn helper() -> Options {
        Options {
            network_id_key: "network_id".into(),
            ping_interval: Some(Duration::from_secs(5)),
            load_tree: false,
            request_timeout: Duration::from_secs(60),
            ..Options::default()
        }
    }
}

/// 32 random hex digits.
pub fn random_network_id() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let mut out = String::with_capacity(32);
    for salt in 0..2u64 {
        let mut h = RandomState::new().build_hasher();
        h.write_u64(salt);
        h.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        );
        h.write_u32(std::process::id());
        out.push_str(&format!("{:016x}", h.finish()));
    }
    out
}

#[derive(Clone, Debug, PartialEq)]
pub enum ConnectionState {
    Idle,
    Connecting,
    /// Connected; fetching the tree.
    Loading,
    /// Connected (and the tree delivered, when `load_tree`).
    Ready,
    /// Lost or refused; reconnecting after `delay`.
    BackingOff {
        attempt: u32,
        delay: Duration,
        reason: String,
    },
    /// Lost, and `reconnect` is off.
    Failed(String),
    /// Closed by the owner.
    Closed,
}

impl ConnectionState {
    pub fn is_connected(&self) -> bool {
        matches!(self, ConnectionState::Loading | ConnectionState::Ready)
    }
}

/// What the network task hands to the owner. Meter pushes are not here: they go to the
/// [`MeterStore`].
#[derive(Debug)]
pub enum Update {
    State(ConnectionState),
    /// The whole tree, fetched after a (re)connect. Install it and resubscribe.
    Tree {
        tree: Box<Tree>,
        info: SnapshotInfo,
    },
    /// Non-meter `…/value` pushes (property paths without `/value`), in arrival order.
    Values(Vec<(String, Value)>),
    /// An engine error for a fire-and-forget command.
    Error {
        path: String,
        message: String,
    },
    /// Any other message (replies to un-awaited `get`s and the like).
    Other(Message),
}

/// Network counters.
#[derive(Clone, Debug, Default)]
pub struct Counters {
    pub bytes_in: u64,
    /// Frames received.
    pub messages_in: u64,
    /// Frames that were `multi` envelopes.
    pub multi_frames: u64,
    /// Inner messages carried by `multi` envelopes.
    pub multi_items: u64,
    pub commands_out: u64,
    pub refused_writes: u64,
    /// Commands dropped because the connection was down when they reached the network task.
    pub dropped: u64,
    pub connects: u64,
}

#[derive(Default)]
struct AtomicCounters {
    bytes_in: AtomicU64,
    messages_in: AtomicU64,
    multi_frames: AtomicU64,
    multi_items: AtomicU64,
    commands_out: AtomicU64,
    refused_writes: AtomicU64,
    dropped: AtomicU64,
    connects: AtomicU64,
}

type Waker = Arc<dyn Fn() + Send + Sync>;

struct Shared {
    state: Mutex<ConnectionState>,
    generation: AtomicU64,
    closed: AtomicBool,
    next_func_id: AtomicU64,
    counters: AtomicCounters,
    waker: Mutex<Option<Waker>>,
}

impl Shared {
    fn state(&self) -> ConnectionState {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn wake(&self) {
        let w = self.waker.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(w) = w {
            w();
        }
    }
}

enum Request {
    Send {
        command: Command,
        generation: u64,
    },
    Get {
        id: u64,
        path: String,
        recursive: bool,
        timeout: Duration,
        reply: mpsc::Sender<Result<Message>>,
    },
    Close,
}

/// A reply to [`EngineHandle::request`] that has not been awaited yet.
#[derive(Debug)]
pub struct PendingReply {
    rx: mpsc::Receiver<Result<Message>>,
}

impl PendingReply {
    /// Blocks up to `timeout` for the reply.
    pub fn wait(&self, timeout: Duration) -> Result<Message> {
        match self.rx.recv_timeout(timeout) {
            Ok(r) => r,
            Err(mpsc::RecvTimeoutError::Timeout) => Err(Error::Timeout),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(Error::Closed),
        }
    }

    /// The reply, if it has arrived.
    pub fn try_take(&self) -> Option<Result<Message>> {
        match self.rx.try_recv() {
            Ok(r) => Some(r),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => Some(Err(Error::Closed)),
        }
    }
}

/// Anything that accepts engine commands: the live connection, or a recorder in tests.
pub trait CommandSink {
    /// Sends a command. Returns false when it was refused (write gate) or could not be sent.
    fn send_command(&self, command: Command) -> bool;
}

/// Entry point: `Engine::connect(endpoint, options)`.
pub struct Engine;

impl Engine {
    /// Connects (read-only unless `options.allow_writes`). Returns at once; progress arrives as
    /// [`Update::State`] on [`EngineHandle::updates`].
    pub fn connect(endpoint: Endpoint, options: Options) -> EngineHandle {
        Engine::connect_with_meters(endpoint, options, Arc::new(MeterStore::new()))
    }

    /// Like [`Engine::connect`], feeding an existing meter store.
    pub fn connect_with_meters(
        endpoint: Endpoint,
        options: Options,
        meters: Arc<MeterStore>,
    ) -> EngineHandle {
        let shared = Arc::new(Shared {
            state: Mutex::new(ConnectionState::Idle),
            generation: AtomicU64::new(0),
            closed: AtomicBool::new(false),
            next_func_id: AtomicU64::new(1),
            counters: AtomicCounters::default(),
            waker: Mutex::new(None),
        });
        let (tx, rx) = tmpsc::unbounded_channel();
        let (utx, urx) = mpsc::channel();
        let task = Task {
            endpoint: endpoint.clone(),
            options: options.clone(),
            shared: shared.clone(),
            updates: utx,
            meters: meters.clone(),
        };
        let thread = std::thread::Builder::new()
            .name(format!("ua-engine {endpoint}"))
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("tokio runtime");
                rt.block_on(task.run(rx));
            })
            .expect("spawn network thread");
        EngineHandle {
            endpoint,
            options,
            shared,
            tx,
            updates: urx,
            meters,
            thread: Some(thread),
        }
    }
}

/// The owner's side of a connection. Dropping it closes the connection.
pub struct EngineHandle {
    endpoint: Endpoint,
    options: Options,
    shared: Arc<Shared>,
    tx: tmpsc::UnboundedSender<Request>,
    updates: mpsc::Receiver<Update>,
    meters: Arc<MeterStore>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl fmt::Debug for EngineHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EngineHandle")
            .field("endpoint", &self.endpoint)
            .field("status", &self.status())
            .finish()
    }
}

impl EngineHandle {
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    pub fn options(&self) -> &Options {
        &self.options
    }

    pub fn status(&self) -> ConnectionState {
        self.shared.state()
    }

    pub fn counters(&self) -> Counters {
        let c = &self.shared.counters;
        Counters {
            bytes_in: c.bytes_in.load(Relaxed),
            messages_in: c.messages_in.load(Relaxed),
            multi_frames: c.multi_frames.load(Relaxed),
            multi_items: c.multi_items.load(Relaxed),
            commands_out: c.commands_out.load(Relaxed),
            refused_writes: c.refused_writes.load(Relaxed),
            dropped: c.dropped.load(Relaxed),
            connects: c.connects.load(Relaxed),
        }
    }

    pub fn meters(&self) -> &Arc<MeterStore> {
        &self.meters
    }

    /// Updates from the network task, in order.
    pub fn updates(&self) -> &mpsc::Receiver<Update> {
        &self.updates
    }

    /// Called from the network thread after each batch of updates (e.g. to wake a winit event
    /// loop through an `EventLoopProxy`).
    pub fn set_waker(&self, waker: impl Fn() + Send + Sync + 'static) {
        *self.shared.waker.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(waker));
    }

    /// Queues a command. `set`/`post` are refused unless `allow_writes`; anything is refused while
    /// disconnected (commands are never replayed into a later connection).
    pub fn send(&self, command: Command) -> Result<()> {
        if command.is_write() && !self.options.allow_writes {
            self.shared.counters.refused_writes.fetch_add(1, Relaxed);
            log::info!("refused write on read-only connection: {command}");
            return Err(Error::WriteRefused);
        }
        if self.shared.closed.load(Relaxed) {
            return Err(Error::Closed);
        }
        if !self.shared.state().is_connected() {
            return Err(Error::NotConnected);
        }
        let generation = self.shared.generation.load(Relaxed);
        self.tx
            .send(Request::Send {
                command,
                generation,
            })
            .map_err(|_| Error::Closed)
    }

    /// Sends `get <path>` with a fresh `func_id`; the reply that echoes it is delivered to the
    /// returned [`PendingReply`] (never as an [`Update`]). Engine errors are
    /// [`Error::Engine`].
    pub fn request(&self, path: &str, recursive: bool) -> PendingReply {
        self.request_with_timeout(path, recursive, self.options.request_timeout)
    }

    pub fn request_with_timeout(
        &self,
        path: &str,
        recursive: bool,
        timeout: Duration,
    ) -> PendingReply {
        let (reply, rx) = mpsc::channel();
        let id = self.shared.next_func_id.fetch_add(1, Relaxed);
        if !self.shared.state().is_connected() {
            let _ = reply.send(Err(Error::NotConnected));
        } else if self
            .tx
            .send(Request::Get {
                id,
                path: path.to_owned(),
                recursive,
                timeout,
                reply: reply.clone(),
            })
            .is_err()
        {
            let _ = reply.send(Err(Error::Closed));
        }
        PendingReply { rx }
    }

    /// Blocking `get`.
    pub fn get(&self, path: &str, recursive: bool) -> Result<Message> {
        self.request(path, recursive)
            .wait(self.options.request_timeout + Duration::from_secs(1))
    }

    /// Closes the connection for good and joins the network thread.
    pub fn close(&mut self) {
        self.shared.closed.store(true, Relaxed);
        let _ = self.tx.send(Request::Close);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for EngineHandle {
    fn drop(&mut self) {
        self.close();
    }
}

impl CommandSink for EngineHandle {
    fn send_command(&self, command: Command) -> bool {
        self.send(command).is_ok()
    }
}

// MARK: - Network task

struct Task {
    endpoint: Endpoint,
    options: Options,
    shared: Arc<Shared>,
    updates: mpsc::Sender<Update>,
    meters: Arc<MeterStore>,
}

enum End {
    Closed,
    Lost(String),
}

enum PendingKind {
    Reply(mpsc::Sender<Result<Message>>),
    Tree,
    Stub(String),
}

struct Pending {
    kind: PendingKind,
    deadline: Instant,
    /// The requested path without its query, for replies that do not echo `func_id`.
    path: String,
}

fn without_query(path: &str) -> &str {
    path.split_once('?').map_or(path, |(p, _)| p)
}

/// The oldest pending request for `path` (the open-apollo daemon answers errors without
/// `parameters`, so they can only be matched by path).
fn pending_for_path(pending: &HashMap<u64, Pending>, path: &str) -> Option<u64> {
    let path = without_query(path);
    pending
        .iter()
        .filter(|(_, p)| p.path == path)
        .map(|(id, _)| *id)
        .min()
}

/// Tree loading state within a session.
struct TreeLoad {
    tree: Option<Tree>,
    outstanding: usize,
    rounds: u32,
    /// The root `get` failed: end the session (and reconnect with backoff).
    failed: Option<String>,
}

impl Task {
    fn set_state(&self, s: ConnectionState) {
        *self.shared.state.lock().unwrap_or_else(|e| e.into_inner()) = s.clone();
        let _ = self.updates.send(Update::State(s));
        self.shared.wake();
    }

    fn closed(&self) -> bool {
        self.shared.closed.load(Relaxed)
    }

    async fn run(self, mut rx: tmpsc::UnboundedReceiver<Request>) {
        let mut attempt = 0u32;
        loop {
            if self.closed() {
                break;
            }
            self.set_state(ConnectionState::Connecting);
            let connect = tokio::time::timeout(
                self.options.connect_timeout,
                TcpStream::connect((self.endpoint.host.as_str(), self.endpoint.port)),
            );
            let reason = match connect.await {
                Ok(Ok(stream)) => {
                    attempt = 0;
                    self.shared.counters.connects.fetch_add(1, Relaxed);
                    self.shared.generation.fetch_add(1, Relaxed);
                    match self.session(stream, &mut rx).await {
                        End::Closed => break,
                        End::Lost(reason) => reason,
                    }
                }
                Ok(Err(e)) => e.to_string(),
                Err(_) => "connect timed out".to_owned(),
            };
            if self.closed() {
                break;
            }
            if !self.options.reconnect {
                log::warn!("engine {} lost ({reason})", self.endpoint);
                self.set_state(ConnectionState::Failed(reason));
                // Stay alive to answer requests with NotConnected until closed.
                while let Some(req) = rx.recv().await {
                    if !self.reject(req) {
                        return;
                    }
                }
                return;
            }
            attempt += 1;
            let delay = self.options.max_backoff.min(
                self.options
                    .min_backoff
                    .mul_f64(2f64.powi(attempt.min(30) as i32 - 1)),
            );
            log::info!(
                "engine {} lost ({reason}); retry in {:.1} s",
                self.endpoint,
                delay.as_secs_f64()
            );
            self.set_state(ConnectionState::BackingOff {
                attempt,
                delay,
                reason,
            });
            let wake_at = tokio::time::Instant::now() + delay;
            loop {
                tokio::select! {
                    _ = tokio::time::sleep_until(wake_at) => break,
                    req = rx.recv() => match req {
                        Some(req) => if !self.reject(req) { self.set_state(ConnectionState::Closed); return },
                        None => { self.set_state(ConnectionState::Closed); return }
                    },
                }
            }
        }
        self.set_state(ConnectionState::Closed);
    }

    /// Handles a request while disconnected. Returns false on `Close`.
    fn reject(&self, req: Request) -> bool {
        match req {
            Request::Close => false,
            Request::Get { reply, .. } => {
                let _ = reply.send(Err(Error::NotConnected));
                true
            }
            Request::Send { .. } => {
                self.shared.counters.dropped.fetch_add(1, Relaxed);
                true
            }
        }
    }

    fn next_func_id(&self) -> u64 {
        self.shared.next_func_id.fetch_add(1, Relaxed)
    }

    async fn session(&self, stream: TcpStream, rx: &mut tmpsc::UnboundedReceiver<Request>) -> End {
        let _ = stream.set_nodelay(true);
        {
            let sock = socket2::SockRef::from(&stream);
            let ka = socket2::TcpKeepalive::new()
                .with_time(Duration::from_secs(10))
                .with_interval(Duration::from_secs(5));
            let _ = sock.set_tcp_keepalive(&ka);
        }
        let generation = self.shared.generation.load(Relaxed);
        let (mut rd, mut wr) = stream.into_split();
        let mut out: Vec<u8> = Vec::new();
        let mut count_out = 0u64;
        if self.options.send_identity {
            for c in [
                Command::identity(&self.options.network_id_key, &self.options.network_id),
                Command::identity("client_id", &self.options.client_id),
            ] {
                out.extend(NulFramer::encode(&c.text()));
                count_out += 1;
            }
        }
        let mut pending: HashMap<u64, Pending> = HashMap::new();
        let mut load = None;
        if self.options.load_tree {
            let id = self.next_func_id();
            out.extend(NulFramer::encode(
                &Command::get_recursive(format!("/?func_id={id}")).text(),
            ));
            count_out += 1;
            pending.insert(
                id,
                Pending {
                    kind: PendingKind::Tree,
                    deadline: Instant::now() + self.options.tree_timeout,
                    path: "/".into(),
                },
            );
            load = Some(TreeLoad {
                tree: None,
                outstanding: 1,
                rounds: 0,
                failed: None,
            });
            self.set_state(ConnectionState::Loading);
        } else {
            self.set_state(ConnectionState::Ready);
        }
        if let Err(e) = wr.write_all(&out).await {
            return End::Lost(e.to_string());
        }
        self.shared
            .counters
            .commands_out
            .fetch_add(count_out, Relaxed);

        let mut framer = NulFramer::new();
        let mut buf = vec![0u8; 1 << 18];
        let mut tick = tokio::time::interval(Duration::from_millis(250));
        let ping_every = self
            .options
            .ping_interval
            .unwrap_or(Duration::from_secs(3600));
        let mut ping =
            tokio::time::interval_at(tokio::time::Instant::now() + ping_every, ping_every);
        loop {
            let mut out: Vec<u8> = Vec::new();
            tokio::select! {
                read = rd.read(&mut buf) => {
                    let n = match read {
                        Ok(0) => return self.lost(&mut pending, "closed by engine".into()),
                        Ok(n) => n,
                        Err(e) => return self.lost(&mut pending, e.to_string()),
                    };
                    self.shared.counters.bytes_in.fetch_add(n as u64, Relaxed);
                    let frames = framer.push(&buf[..n]);
                    self.ingest(frames, &mut pending, &mut load, &mut out);
                    if let Some(reason) = load.as_ref().and_then(|l| l.failed.clone()) {
                        return self.lost(&mut pending, reason);
                    }
                }
                req = rx.recv() => {
                    let Some(req) = req else { return End::Closed };
                    if !self.handle_request(req, generation, &mut pending, &mut out) {
                        return self.close_session(&mut pending);
                    }
                    // Drain whatever else is queued so a burst goes out in one write.
                    while let Ok(req) = rx.try_recv() {
                        if !self.handle_request(req, generation, &mut pending, &mut out) {
                            return self.close_session(&mut pending);
                        }
                    }
                }
                _ = ping.tick(), if self.options.ping_interval.is_some() => {
                    out.extend(NulFramer::encode(&Command::get("/ping").text()));
                    self.shared.counters.commands_out.fetch_add(1, Relaxed);
                }
                _ = tick.tick() => {
                    let now = Instant::now();
                    let expired: Vec<u64> = pending.iter().filter(|(_, p)| p.deadline <= now).map(|(k, _)| *k).collect();
                    for id in expired {
                        if let Some(p) = pending.remove(&id) {
                            match p.kind {
                                PendingKind::Reply(tx) => { let _ = tx.send(Err(Error::Timeout)); }
                                PendingKind::Tree | PendingKind::Stub(_) => return self.lost(&mut pending, "tree request timed out".into()),
                            }
                        }
                    }
                }
            }
            if !out.is_empty()
                && let Err(e) = wr.write_all(&out).await
            {
                return self.lost(&mut pending, e.to_string());
            }
        }
    }

    fn close_session(&self, pending: &mut HashMap<u64, Pending>) -> End {
        Self::fail_pending(pending, || Error::Closed);
        End::Closed
    }

    fn lost(&self, pending: &mut HashMap<u64, Pending>, reason: String) -> End {
        Self::fail_pending(pending, || Error::Disconnected);
        End::Lost(reason)
    }

    fn fail_pending(pending: &mut HashMap<u64, Pending>, err: impl Fn() -> Error) {
        for (_, p) in pending.drain() {
            if let PendingKind::Reply(tx) = p.kind {
                let _ = tx.send(Err(err()));
            }
        }
    }

    /// Returns false on `Close`.
    fn handle_request(
        &self,
        req: Request,
        generation: u64,
        pending: &mut HashMap<u64, Pending>,
        out: &mut Vec<u8>,
    ) -> bool {
        match req {
            Request::Close => return false,
            Request::Send {
                command,
                generation: g,
            } => {
                if g != generation {
                    self.shared.counters.dropped.fetch_add(1, Relaxed);
                } else if command.is_write() && !self.options.allow_writes {
                    // Defence in depth: the handle already refuses these.
                    self.shared.counters.refused_writes.fetch_add(1, Relaxed);
                } else {
                    out.extend(NulFramer::encode(&command.text()));
                    self.shared.counters.commands_out.fetch_add(1, Relaxed);
                }
            }
            Request::Get {
                id,
                path,
                recursive,
                timeout,
                reply,
            } => {
                let sep = if path.contains('?') { '&' } else { '?' };
                let cmd = Command::Get {
                    path: format!("{path}{sep}func_id={id}"),
                    recursive,
                };
                out.extend(NulFramer::encode(&cmd.text()));
                self.shared.counters.commands_out.fetch_add(1, Relaxed);
                pending.insert(
                    id,
                    Pending {
                        kind: PendingKind::Reply(reply),
                        deadline: Instant::now() + timeout,
                        path: without_query(&path).to_owned(),
                    },
                );
            }
        }
        true
    }

    fn ingest(
        &self,
        frames: Vec<Vec<u8>>,
        pending: &mut HashMap<u64, Pending>,
        load: &mut Option<TreeLoad>,
        out: &mut Vec<u8>,
    ) {
        if frames.is_empty() {
            return;
        }
        let c = &self.shared.counters;
        c.messages_in.fetch_add(frames.len() as u64, Relaxed);
        let mut values: Vec<(String, Value)> = Vec::new();
        let mut sent = false;
        for frame in frames {
            let Some(msg) = Message::decode(&frame) else {
                log::debug!(
                    "non-JSON frame: {}",
                    String::from_utf8_lossy(&frame[..frame.len().min(120)])
                );
                continue;
            };
            let id = match msg.func_id() {
                Some(id) => Some(id),
                None if msg.error.is_some() => pending_for_path(pending, &msg.path),
                None => None,
            };
            if let Some(id) = id
                && let Some(p) = pending.remove(&id)
            {
                self.resolve(p.kind, msg, pending, load, out);
                continue;
            }
            let multi = msg.is_multi();
            let inner = msg.flattened();
            if multi {
                c.multi_frames.fetch_add(1, Relaxed);
                c.multi_items.fetch_add(inner.len() as u64, Relaxed);
            }
            for m in inner {
                if m.is_value_push() {
                    let v = m.value().unwrap_or(Value::Null);
                    if self.meters.ingest(&m.path, &v) {
                        continue;
                    }
                    values.push((path::property_path(&m.path).to_owned(), v));
                } else if m.path == "/ping" && m.error.is_none() {
                    // Keep-alive reply.
                } else if let Some(err) = m.error.clone() {
                    let _ = self.updates.send(Update::Error {
                        path: m.path,
                        message: err,
                    });
                    sent = true;
                } else {
                    let _ = self.updates.send(Update::Other(m));
                    sent = true;
                }
            }
        }
        if !values.is_empty() {
            let _ = self.updates.send(Update::Values(values));
            sent = true;
        }
        if sent {
            self.shared.wake();
        }
    }

    fn resolve(
        &self,
        kind: PendingKind,
        msg: Message,
        pending: &mut HashMap<u64, Pending>,
        load: &mut Option<TreeLoad>,
        out: &mut Vec<u8>,
    ) {
        match kind {
            PendingKind::Reply(tx) => {
                let r = match &msg.error {
                    Some(e) => Err(Error::Engine {
                        path: msg.path.clone(),
                        message: e.clone(),
                    }),
                    None => Ok(msg),
                };
                let _ = tx.send(r);
            }
            PendingKind::Tree => {
                let Some(l) = load.as_mut() else { return };
                l.outstanding = l.outstanding.saturating_sub(1);
                match (&msg.error, &msg.data) {
                    (None, Some(data)) => l.tree = Some(Tree::from_node(data)),
                    (err, _) => {
                        // An engine that cannot serve its tree is not usable yet (still starting?).
                        l.failed = Some(format!("get /: {}", err.as_deref().unwrap_or("no data")));
                        return;
                    }
                }
                self.walk_stubs(pending, load, out);
            }
            PendingKind::Stub(path) => {
                let Some(l) = load.as_mut() else { return };
                l.outstanding = l.outstanding.saturating_sub(1);
                if let (Some(tree), Some(data)) = (l.tree.as_mut(), &msg.data)
                    && msg.error.is_none()
                {
                    tree.insert_node(data, &path);
                }
                self.walk_stubs(pending, load, out);
            }
        }
    }

    /// Older engines answer the root with child stubs; walk them (up to 16 rounds), then deliver.
    fn walk_stubs(
        &self,
        pending: &mut HashMap<u64, Pending>,
        load: &mut Option<TreeLoad>,
        out: &mut Vec<u8>,
    ) {
        let Some(l) = load.as_mut() else { return };
        if l.outstanding > 0 {
            return;
        }
        let stubs = l.tree.as_ref().map(Tree::stub_paths).unwrap_or_default();
        if !stubs.is_empty() && l.rounds < 16 {
            l.rounds += 1;
            for path in stubs {
                let id = self.next_func_id();
                let cmd = Command::get_recursive(format!("{path}?func_id={id}"));
                out.extend(NulFramer::encode(&cmd.text()));
                self.shared.counters.commands_out.fetch_add(1, Relaxed);
                pending.insert(
                    id,
                    Pending {
                        kind: PendingKind::Stub(path.clone()),
                        deadline: Instant::now() + self.options.tree_timeout,
                        path,
                    },
                );
                l.outstanding += 1;
            }
            return;
        }
        let tree = load.take().and_then(|l| l.tree).unwrap_or_default();
        self.meters.seed(&tree);
        let info = SnapshotInfo {
            source: Some(format!("live {}", self.endpoint)),
            captured: Some(crate::iso8601_now()),
            ..Default::default()
        };
        let _ = self.updates.send(Update::Tree {
            tree: Box::new(tree),
            info,
        });
        self.set_state(ConnectionState::Ready);
    }
}
