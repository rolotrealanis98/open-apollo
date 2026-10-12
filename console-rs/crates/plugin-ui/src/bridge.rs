//! (feature `bridge`) The local WebSocket endpoint a hosted UA GUI connects to.
//!
//! UA's GUIs (`@uaudio/client`) open `ws://<ip>:<port>` from their page's query string and speak
//! the protocol described in WEBBRIDGE.md. This module is the transport only: it listens on
//! 127.0.0.1, runs one reader thread per connection, and hands text frames to the main thread
//! through [`BridgeServer::poll`], where the app (owner of the engine connection and its write
//! gate) answers them. Nothing here talks to the engine.

use std::collections::HashMap;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

use parking_lot::Mutex;
use tungstenite::{Message, WebSocket};

pub type ConnId = u64;

/// What the main thread sees.
#[derive(Clone, Debug, PartialEq)]
pub enum BridgeEvent {
    Connected(ConnId, SocketAddr),
    Text(ConnId, String),
    Closed(ConnId),
}

type Socket = Arc<Mutex<WebSocket<TcpStream>>>;

pub struct BridgeServer {
    addr: SocketAddr,
    events: Receiver<BridgeEvent>,
    sockets: Arc<Mutex<HashMap<ConnId, Socket>>>,
    stop: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
}

impl BridgeServer {
    /// Listens on `127.0.0.1:<port>` (0 picks a free port; see [`port`](Self::port)).
    pub fn bind(port: u16) -> std::io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", port))?;
        listener.set_nonblocking(true)?;
        let addr = listener.local_addr()?;
        let (tx, events) = mpsc::channel();
        let sockets: Arc<Mutex<HashMap<ConnId, Socket>>> = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        let accept = {
            let (sockets, stop) = (sockets.clone(), stop.clone());
            std::thread::Builder::new().name("plugin-ui-bridge".into()).spawn(move || accept_loop(listener, tx, sockets, stop))?
        };
        Ok(Self { addr, events, sockets, stop, accept: Some(accept) })
    }

    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    pub fn connections(&self) -> Vec<ConnId> {
        let mut v: Vec<ConnId> = self.sockets.lock().keys().copied().collect();
        v.sort();
        v
    }

    /// Pending events, without blocking.
    pub fn poll(&self) -> Vec<BridgeEvent> {
        self.events.try_iter().collect()
    }

    /// Waits up to `timeout` for the next event.
    pub fn next(&self, timeout: Duration) -> Option<BridgeEvent> {
        self.events.recv_timeout(timeout).ok()
    }

    /// Sends a text frame; false when the connection is gone.
    pub fn send(&self, conn: ConnId, text: &str) -> bool {
        let Some(sock) = self.sockets.lock().get(&conn).cloned() else { return false };
        let ok = sock.lock().send(Message::text(text)).is_ok();
        if !ok {
            self.sockets.lock().remove(&conn);
        }
        ok
    }

    /// Sends to every connection.
    pub fn broadcast(&self, text: &str) {
        for c in self.connections() {
            self.send(c, text);
        }
    }
}

impl Drop for BridgeServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for (_, s) in self.sockets.lock().drain() {
            let _ = s.lock().close(None);
        }
        if let Some(h) = self.accept.take() {
            let _ = h.join();
        }
    }
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn accept_loop(listener: TcpListener, tx: Sender<BridgeEvent>, sockets: Arc<Mutex<HashMap<ConnId, Socket>>>, stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, peer)) => {
                let _ = stream.set_nonblocking(false);
                // Reads time out so the reader releases the socket lock for writers.
                let _ = stream.set_read_timeout(Some(Duration::from_millis(20)));
                let _ = stream.set_nodelay(true);
                let Ok(ws) = tungstenite::accept(stream) else { continue };
                let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
                let sock: Socket = Arc::new(Mutex::new(ws));
                sockets.lock().insert(id, sock.clone());
                let _ = tx.send(BridgeEvent::Connected(id, peer));
                let (tx, sockets, stop) = (tx.clone(), sockets.clone(), stop.clone());
                let _ = std::thread::Builder::new().name(format!("plugin-ui-bridge-{id}")).spawn(move || read_loop(id, sock, tx, sockets, stop));
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

fn read_loop(id: ConnId, sock: Socket, tx: Sender<BridgeEvent>, sockets: Arc<Mutex<HashMap<ConnId, Socket>>>, stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::Relaxed) {
        let msg = sock.lock().read();
        match msg {
            Ok(Message::Text(t)) => {
                if tx.send(BridgeEvent::Text(id, t.to_string())).is_err() {
                    break;
                }
            }
            Ok(Message::Binary(b)) => {
                let _ = tx.send(BridgeEvent::Text(id, String::from_utf8_lossy(&b).into_owned()));
            }
            Ok(Message::Close(_)) => break,
            Ok(_) => {}
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                std::thread::yield_now();
            }
            Err(_) => break,
        }
    }
    sockets.lock().remove(&id);
    let _ = tx.send(BridgeEvent::Closed(id));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn echo_round_trip() {
        let server = BridgeServer::bind(0).unwrap();
        let url = format!("ws://127.0.0.1:{}", server.port());
        let client = std::thread::spawn(move || {
            let (mut ws, _) = tungstenite::connect(url).unwrap();
            ws.send(Message::text("{\"hello\":1}")).unwrap();
            let reply = ws.read().unwrap();
            ws.close(None).ok();
            reply.into_text().unwrap().to_string()
        });
        let Some(BridgeEvent::Connected(id, _)) = server.next(Duration::from_secs(5)) else { panic!("no connection") };
        let Some(BridgeEvent::Text(from, text)) = server.next(Duration::from_secs(5)) else { panic!("no text") };
        assert_eq!((from, text.as_str()), (id, "{\"hello\":1}"));
        assert!(server.send(id, "{\"ok\":true}"));
        assert_eq!(client.join().unwrap(), "{\"ok\":true}");
    }
}
