/**
 * WebSocket client for the Apollo mixer daemon (WS:4721).
 * Connection, exponential-backoff reconnect, command send, subscription mgmt.
 */

let ws = null;
let msgId = 0;
let connected = false;
let onMessage = null;
let onConnect = null;
let onDisconnect = null;
let onFatal = null;          // called after RETRY_CAP consecutive failures
let reconnectTimer = null;
let url = null;
let retryCount = 0;
let manualClose = false;

const BACKOFF_BASE_MS = 500;
const BACKOFF_MAX_MS = 10000;
const RETRY_CAP = 60;

function _backoffDelay(n) {
  // 500, 1000, 2000, 4000, 8000, 10000 cap
  return Math.min(BACKOFF_BASE_MS * 2 ** n, BACKOFF_MAX_MS);
}

export function connect(wsUrl, callbacks = {}) {
  url = wsUrl;
  onMessage = callbacks.onMessage || null;
  onConnect = callbacks.onConnect || null;
  onDisconnect = callbacks.onDisconnect || null;
  onFatal = callbacks.onFatal || null;
  manualClose = false;
  retryCount = 0;
  _open();
}

function _open() {
  console.log("[ws] _open() → connecting", url, "manualClose=", manualClose, "retry#", retryCount);
  // Clean up any prior instance — detach handlers first so its late onclose
  // doesn't spawn a parallel retry chain.
  if (ws) {
    ws.onopen = ws.onmessage = ws.onclose = ws.onerror = null;
    try { ws.close(); } catch {}
  }

  ws = new WebSocket(url);
  const self = ws;

  self.onopen = () => {
    if (self !== ws) return;
    connected = true;
    retryCount = 0;
    console.log("[ws] connected to", url);
    onConnect?.();
  };

  self.onmessage = (evt) => {
    if (self !== ws) return;
    try {
      const msg = JSON.parse(evt.data);
      onMessage?.(msg.path, msg.data, msg.parameters || {});
    } catch (e) {
      console.warn("[ws] parse error:", e, evt.data);
    }
  };

  self.onclose = (evt) => {
    if (self !== ws) {
      console.log("[ws] onclose from stale instance — ignored");
      return;
    }
    connected = false;
    console.log(`[ws] onclose code=${evt.code} clean=${evt.wasClean} reason=${evt.reason || "(none)"}`);
    onDisconnect?.();
    if (manualClose) {
      console.log("[ws] manualClose — not retrying");
      return;
    }
    if (retryCount >= RETRY_CAP) {
      console.warn("[ws] retry cap reached, giving up");
      onFatal?.();
      return;
    }
    const delay = _backoffDelay(retryCount);
    retryCount += 1;
    console.log(`[ws] scheduling retry ${retryCount}/${RETRY_CAP} in ${delay}ms`);
    clearTimeout(reconnectTimer);
    reconnectTimer = setTimeout(_open, delay);
  };

  self.onerror = (e) => {
    console.warn("[ws] onerror:", e);
  };
}

/** Force a reconnect attempt now, resetting the retry counter. */
export function retryNow() {
  retryCount = 0;
  clearTimeout(reconnectTimer);
  _open();
}

export function send(cmd) {
  if (ws?.readyState === WebSocket.OPEN) ws.send(cmd);
}

// Daemon query parser splits on first `?` only — use `&` to append when the
// path already contains one.
function _q(path, extra) {
  const sep = path.includes("?") ? "&" : "?";
  return `${path}${sep}${extra}`;
}

export function get(path, messageId) {
  const id = messageId || `get:${++msgId}`;
  send(`get ${_q(path, `message_id=${id}`)}`);
  return id;
}

export function set(path, value) {
  const id = `cmd:${++msgId}`;
  send(`set ${_q(path, `cmd_id=${id}`)} ${value}`);
  return id;
}

export function subscribe(path) {
  const id = `sub:${++msgId}`;
  send(`subscribe ${_q(path, `message_id=${id}`)}`);
  return id;
}

export function getDeviceTree() {
  return get("/devices?recursive=1", "tree:init");
}

export function isConnected() { return connected; }

export function disconnect() {
  manualClose = true;
  clearTimeout(reconnectTimer);
  if (ws) {
    ws.onclose = null;
    ws.close();
    ws = null;
  }
  connected = false;
}
