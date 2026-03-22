#!/usr/bin/env python3
"""
WebSocket test client — test ua-mixer-daemon's WS:4720 endpoint.

Replays a UA Connect-style connection sequence over WebSocket.
Analogous to test_client.py (TCP) but uses the ws://4720 protocol.

Usage:
    python3 test_ws_client.py
    python3 test_ws_client.py --host localhost
    python3 test_ws_client.py -v --stay

Requires: pip install websockets
"""

import argparse
import asyncio
import json
import sys
import time

try:
    import websockets
except ImportError:
    print("Error: websockets package required. Install with: pip install websockets")
    sys.exit(1)


class WsTestClient:
    """Simulates a UA Connect-style WebSocket connection."""

    def __init__(self, host: str = "127.0.0.1", port: int = 4720,
                 verbose: bool = False):
        self.host = host
        self.port = port
        self.verbose = verbose
        self.ws = None
        self.recv_count = 0
        self.error_count = 0
        self.value_count = 0
        self._msg_counter = 0

    def _next_msg_id(self) -> str:
        self._msg_counter += 1
        return f"test:{self._msg_counter}"

    async def connect(self):
        """Open WebSocket connection."""
        uri = f"ws://{self.host}:{self.port}"
        self.ws = await websockets.connect(uri)
        print(f"Connected to {uri}")

    async def send(self, msg: str):
        """Send a text frame."""
        await self.ws.send(msg)
        if self.verbose:
            print(f"  -> {msg}")

    async def recv_all(self, timeout: float = 1.0) -> list[dict]:
        """Receive all pending responses within timeout."""
        responses = []
        deadline = time.monotonic() + timeout
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                break
            try:
                data = await asyncio.wait_for(self.ws.recv(), timeout=remaining)
                if isinstance(data, bytes):
                    data = data.decode("utf-8", errors="replace")
                try:
                    obj = json.loads(data)
                    responses.append(obj)
                    self.recv_count += 1
                    if "error" in obj:
                        self.error_count += 1
                    else:
                        self.value_count += 1
                    if self.verbose:
                        path = obj.get("path", "?")
                        if "error" in obj:
                            print(f"  <- ERROR: {path}: {obj['error']}")
                        else:
                            data_str = str(obj.get("data", ""))
                            if len(data_str) > 100:
                                data_str = data_str[:100] + "..."
                            print(f"  <- {path} = {data_str}")
                except json.JSONDecodeError:
                    if self.verbose:
                        print(f"  <- (non-JSON): {data[:100]}")
            except asyncio.TimeoutError:
                break
            except websockets.ConnectionClosed:
                print("  Connection closed by server")
                break
        return responses

    async def run_sequence(self, stay: bool = False):
        """Run a UA Connect-style connection sequence."""
        await self.connect()

        # Phase 1: Protocol negotiation
        print("\n[Phase 1] Protocol negotiation...")
        mid = self._next_msg_id()
        await self.send(f"post command_format?message_id={mid}&func_id=1 2")
        mid = self._next_msg_id()
        await self.send(f"post /request_challenge?message_id={mid}&func_id=2")
        responses = await self.recv_all(timeout=1.0)
        for r in responses:
            path = r.get("path", "?")
            print(f"  {path} = {r.get('data', r.get('error', '?'))}")

        # Phase 2: GET device tree
        print("[Phase 2] GET /devices (recursive)...")
        mid = self._next_msg_id()
        await self.send(f"get /devices?message_id={mid}&recursive=1")
        responses = await self.recv_all(timeout=2.0)
        if responses:
            root = responses[0]
            data = root.get("data", {})
            if isinstance(data, dict):
                children = list(data.get("children", {}).keys())
                print(f"  /devices has {len(children)} children: {children[:5]}")
            else:
                print(f"  /devices = {str(data)[:100]}")

        # Phase 3: Subscribe to input channel controls
        print("[Phase 3] Subscribe to input 0 controls...")
        input_paths = [
            "/devices/0/inputs/0/FaderLevel/value",
            "/devices/0/inputs/0/Pan/value",
            "/devices/0/inputs/0/Mute/value",
            "/devices/0/inputs/0/preamps/0/Gain/value",
            "/devices/0/inputs/0/preamps/0/48V/value",
            "/devices/0/inputs/0/preamps/0/Pad/value",
        ]
        for path in input_paths:
            mid = self._next_msg_id()
            await self.send(f"subscribe {path}?message_id={mid}")

        responses = await self.recv_all(timeout=1.0)
        print(f"  Received {len(responses)} responses "
              f"({self.value_count} values, {self.error_count} errors)")

        # Phase 4: Subscribe to monitor controls
        print("[Phase 4] Subscribe to monitor controls...")
        monitor_paths = [
            "/devices/0/outputs/18/CRMonitorLevelTapered/value",
            "/devices/0/outputs/18/Mute/value",
            "/devices/0/outputs/18/meters/0/MeterLevel/value",
        ]
        for path in monitor_paths:
            mid = self._next_msg_id()
            await self.send(f"subscribe {path}?message_id={mid}")

        responses = await self.recv_all(timeout=1.0)
        print(f"  Received {len(responses)} responses "
              f"(total: {self.value_count} values, {self.error_count} errors)")

        # Phase 5: SET a fader value and check notification
        print("[Phase 5] SET fader test...")
        mid = self._next_msg_id()
        await self.send(
            f"set /devices/0/inputs/0/FaderLevel/value?message_id={mid}&cmd_id=set_fader -6.0")
        responses = await self.recv_all(timeout=1.0)
        # We should NOT get a notification on this connection (exclude self)
        # but we might get meter updates
        notif_count = sum(1 for r in responses
                          if "FaderLevel" in r.get("path", ""))
        print(f"  FaderLevel notifications received: {notif_count} "
              f"(expect 0 — self excluded)")

        # Phase 6: GET the value back to confirm
        print("[Phase 6] GET fader value back...")
        mid = self._next_msg_id()
        await self.send(
            f"get /devices/0/inputs/0/FaderLevel/value?message_id={mid}")
        responses = await self.recv_all(timeout=1.0)
        for r in responses:
            if "FaderLevel" in r.get("path", ""):
                print(f"  FaderLevel = {r.get('data')}")

        # Phase 7: Ping
        print("[Phase 7] Ping...")
        mid = self._next_msg_id()
        await self.send(f"get /ping?message_id={mid}")
        responses = await self.recv_all(timeout=1.0)
        if responses:
            print(f"  Ping = {responses[0].get('data')}")

        # Summary
        print(f"\n{'='*50}")
        print(f"Total responses: {self.recv_count}")
        print(f"  Values: {self.value_count}")
        print(f"  Errors: {self.error_count}")
        print(f"{'='*50}")

        if self.error_count > 0:
            print(f"\nWARNING: {self.error_count} error responses received.")

        # Phase 8: Stay and watch
        if stay:
            print("\n[Staying connected — watching for updates...]")
            print("(Press Ctrl+C to disconnect)\n")
            meter_count = 0
            start = time.monotonic()
            try:
                while True:
                    responses = await self.recv_all(timeout=2.0)
                    for r in responses:
                        path = r.get("path", "")
                        if "MeterLevel" in path or "Clip" in path:
                            meter_count += 1
                        elif self.verbose:
                            print(f"  <- {path} = {r.get('data')}")
                    elapsed = time.monotonic() - start
                    if meter_count > 0 and int(elapsed) % 5 == 0:
                        rate = meter_count / elapsed if elapsed > 0 else 0
                        print(f"  Meters: {meter_count} updates "
                              f"({rate:.1f}/sec, {elapsed:.0f}s elapsed)")
            except asyncio.CancelledError:
                pass
        else:
            print("\nWaiting 3s to check for meter updates...")
            responses = await self.recv_all(timeout=3.0)
            meter_updates = sum(1 for r in responses
                                if "MeterLevel" in r.get("path", "")
                                or "Clip" in r.get("path", ""))
            print(f"  Received {len(responses)} messages "
                  f"({meter_updates} meter updates)")
            if meter_updates > 0:
                print("  Meter pump is working over WebSocket!")

        # Disconnect
        await self.ws.close()
        print("\nDisconnected.")


async def main():
    parser = argparse.ArgumentParser(
        description="WebSocket test client for ua-mixer-daemon WS:4720")
    parser.add_argument("--host", default="127.0.0.1",
                        help="Daemon host (default: 127.0.0.1)")
    parser.add_argument("--port", "-p", type=int, default=4720,
                        help="WebSocket port (default: 4720)")
    parser.add_argument("--verbose", "-v", action="store_true",
                        help="Show all responses")
    parser.add_argument("--stay", "-s", action="store_true",
                        help="Stay connected and watch updates")
    args = parser.parse_args()

    client = WsTestClient(args.host, args.port, args.verbose)
    try:
        await client.run_sequence(stay=args.stay)
    except ConnectionRefusedError:
        print(f"Connection refused at ws://{args.host}:{args.port}")
        print("Is the daemon running? Start it with:")
        print("  python3 ua_mixer_daemon.py --no-hardware -v")
        sys.exit(1)
    except KeyboardInterrupt:
        print("\nInterrupted.")


if __name__ == "__main__":
    asyncio.run(main())
