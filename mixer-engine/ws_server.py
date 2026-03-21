"""
WebSocket:4720 server — UA Mixer Helper protocol.

Shares the same StateTree and HardwareRouter as the TCP:4710 daemon,
enabling cross-protocol subscriptions (SET from WS notifies TCP clients
and vice versa).

Requires: pip install websockets
"""

import asyncio
import logging
from typing import Any

try:
    import websockets
    from websockets.asyncio.server import serve, ServerConnection
    HAS_WEBSOCKETS = True
except ImportError:
    HAS_WEBSOCKETS = False

from state_tree import StateTree
from ws_protocol import (
    WsCommand, parse_ws_command,
    encode_ws_response, encode_ws_error,
)

log = logging.getLogger(__name__)


class WsClient:
    """State for a single connected WebSocket client."""

    _next_id = 0

    def __init__(self, ws, trace: bool = False):
        WsClient._next_id += 1
        self.id = f"ws-client-{WsClient._next_id}"
        self.ws = ws
        self.trace = trace
        self.closed = False
        self._send_count = 0

    async def send(self, text: str):
        """Send a text frame to this client."""
        if self.closed:
            return
        try:
            await self.ws.send(text)
            self._send_count += 1
            if self.trace:
                log.debug("WS SEND -> %s: %s", self.id, text[:200])
        except Exception:
            self.closed = True

    def __repr__(self):
        return f"<WsClient {self.id}>"


class WsServer:
    """WebSocket:4720 server sharing state with the TCP daemon."""

    def __init__(self, state: StateTree, hw_router,
                 host: str = "0.0.0.0", port: int = 4720,
                 trace: bool = False,
                 network_id: str = "", process_id: str = ""):
        self.state = state
        self.hw_router = hw_router
        self.host = host
        self.port = port
        self.trace = trace
        self.network_id = network_id
        self.process_id = process_id
        self.clients: dict[str, WsClient] = {}

    async def start(self):
        """Start the WebSocket server (runs forever)."""
        if not HAS_WEBSOCKETS:
            log.error("websockets package not installed — WS server disabled")
            return

        async with serve(self._handle_connection, self.host, self.port) as server:
            log.info("WebSocket listening on ws://%s:%d", self.host, self.port)
            await asyncio.Future()  # run forever

    async def _handle_connection(self, ws):
        """Per-client lifecycle."""
        client = WsClient(ws, trace=self.trace)
        self.clients[client.id] = client
        log.info("WS connected: %s", client)

        # Create an asyncio queue for subscription notifications
        # (callbacks come from sync context — we need to bridge to async)
        notify_queue: asyncio.Queue[str] = asyncio.Queue()
        loop = asyncio.get_event_loop()

        def on_notify(path: str, value: Any):
            """Subscription callback — queues a message for async send."""
            text = encode_ws_response(path, value)
            loop.call_soon_threadsafe(notify_queue.put_nowait, text)

        self.state.register_callback(client.id, on_notify)

        # Drain notification queue in a background task
        async def drain_notifications():
            try:
                while not client.closed:
                    try:
                        text = await asyncio.wait_for(notify_queue.get(), timeout=0.1)
                        await client.send(text)
                    except asyncio.TimeoutError:
                        continue
            except asyncio.CancelledError:
                pass

        drain_task = asyncio.create_task(drain_notifications())

        try:
            async for message in ws:
                if isinstance(message, bytes):
                    message = message.decode("utf-8", errors="replace")

                if client.trace:
                    log.debug("WS RECV <- %s: %s", client.id, message[:200])

                cmd = parse_ws_command(message)
                if cmd is None:
                    log.warning("WS: unparseable from %s: %r", client.id, message[:200])
                    continue

                log.debug("WS %s: %s %s", client.id, cmd.verb, cmd.path)

                responses = self._dispatch(client, cmd)
                for resp in responses:
                    await client.send(resp)
        except Exception as e:
            if "close" not in str(type(e).__name__).lower():
                log.debug("WS connection error for %s: %s", client.id, e)
        finally:
            log.info("WS disconnected: %s", client)
            client.closed = True
            drain_task.cancel()
            try:
                await drain_task
            except asyncio.CancelledError:
                pass
            self.state.unsubscribe_all(client.id)
            self.state.unregister_callback(client.id)
            self.clients.pop(client.id, None)

    def _dispatch(self, client: WsClient, cmd: WsCommand) -> list[str]:
        """Dispatch a parsed WS command. Returns list of response strings."""
        if cmd.verb == "get":
            return self._handle_get(client, cmd)
        elif cmd.verb == "set":
            return self._handle_set(client, cmd)
        elif cmd.verb == "subscribe":
            return self._handle_subscribe(client, cmd)
        elif cmd.verb == "unsubscribe":
            return self._handle_unsubscribe(client, cmd)
        elif cmd.verb == "post":
            return self._handle_post(client, cmd)
        else:
            return [encode_ws_error(cmd.path, f"Unknown verb: {cmd.verb}",
                                    cmd.message_id)]

    def _handle_get(self, client: WsClient, cmd: WsCommand) -> list[str]:
        """Handle GET — return control tree data."""
        # Ping
        if cmd.path in ("/ping", "ping"):
            return [encode_ws_response("/ping", 1, cmd.message_id)]

        # System identity queries
        if cmd.path == "networkID" or cmd.path == "/networkID":
            return [encode_ws_response("networkID", self.network_id, cmd.message_id)]
        if cmd.path == "processID" or cmd.path == "/processID":
            return [encode_ws_response("processID", self.process_id, cmd.message_id)]

        data = self.state.get(cmd.path, recursive=cmd.recursive,
                              propfilter=cmd.propfilter or None)
        if data is None:
            return [encode_ws_error(cmd.path,
                                    f"Unable to resolve path for get.",
                                    cmd.message_id)]

        params = {}
        if cmd.recursive:
            params["recursive"] = 1
        if cmd.propfilter:
            params["propfilter"] = ",".join(cmd.propfilter)

        return [encode_ws_response(cmd.path, data, cmd.message_id,
                                   params if params else None)]

    def _handle_set(self, client: WsClient, cmd: WsCommand) -> list[str]:
        """Handle SET — update state and write to hardware."""
        path = cmd.path
        if path.lower() == "/sleep":
            path = "/Sleep"
        self.state.set(path, cmd.value, source_client=client.id)
        return []

    def _handle_subscribe(self, client: WsClient, cmd: WsCommand) -> list[str]:
        """Handle SUBSCRIBE — register + flood current values."""
        path = cmd.path

        if not self.state.path_exists(path):
            self.state.subscribe(client.id, path)
            return []

        self.state.subscribe(client.id, path)

        # Flood current values
        values = self.state.enumerate_values(path, recursive=cmd.recursive)
        responses = []
        for vpath, value in values:
            responses.append(encode_ws_response(vpath, value, cmd.message_id))

        if values:
            log.debug("WS SUBSCRIBE %s: sent %d values to %s",
                      path, len(values), client.id)

        return responses

    def _handle_unsubscribe(self, client: WsClient, cmd: WsCommand) -> list[str]:
        """Handle UNSUBSCRIBE."""
        self.state.unsubscribe(client.id, cmd.path)
        return []

    def _handle_post(self, client: WsClient, cmd: WsCommand) -> list[str]:
        """Handle POST — auth challenge, command format, etc."""
        if cmd.path in ("command_format", "/command_format"):
            return [encode_ws_response("command_format", cmd.value or 2,
                                       cmd.message_id)]

        if cmd.path == "/request_challenge":
            return [encode_ws_response("/request_challenge", "",
                                       cmd.message_id)]

        log.debug("WS %s: POST %s (value=%r) — ignored",
                  client.id, cmd.path, cmd.value)
        return []
