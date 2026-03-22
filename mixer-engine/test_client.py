#!/usr/bin/env python3
"""
ConsoleLink simulator — test client for ua-mixer-daemon.

Replays the exact command sequence that ConsoleLink (iPhone) sends when
connecting to the UA Mixer Engine. Useful for testing the daemon without
the actual iOS app.

Usage:
    # Test against local daemon
    python3 test_client.py

    # Test against remote host
    python3 test_client.py --host localhost

    # Verbose mode (show all responses)
    python3 test_client.py -v

    # Stay connected and watch for meter updates
    python3 test_client.py --stay
"""

import argparse
import asyncio
import json
import sys
import time


NULL = b"\x00"


class TestClient:
    """Simulates ConsoleLink's connection to the mixer daemon."""

    def __init__(self, host: str = "127.0.0.1", port: int = 4710,
                 verbose: bool = False):
        self.host = host
        self.port = port
        self.verbose = verbose
        self.reader = None
        self.writer = None
        self.recv_count = 0
        self.error_count = 0
        self.value_count = 0
        self.buf = b""

    async def connect(self):
        """Open TCP connection."""
        self.reader, self.writer = await asyncio.open_connection(
            self.host, self.port)
        print(f"Connected to {self.host}:{self.port}")

    def send(self, msg: str):
        """Send a null-terminated message."""
        self.writer.write((msg + "\0").encode("utf-8"))
        if self.verbose:
            print(f"  → {msg}")

    async def recv_all(self, timeout: float = 1.0) -> list[dict]:
        """Receive all pending responses within timeout."""
        responses = []
        deadline = time.monotonic() + timeout
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                break
            try:
                data = await asyncio.wait_for(
                    self.reader.read(65536),
                    timeout=remaining)
                if not data:
                    break
                self.buf += data
                while NULL in self.buf:
                    raw, self.buf = self.buf.split(NULL, 1)
                    if raw:
                        try:
                            obj = json.loads(raw.decode("utf-8", errors="replace"))
                            responses.append(obj)
                            self.recv_count += 1
                            if "error" in obj:
                                self.error_count += 1
                            else:
                                self.value_count += 1
                            if self.verbose:
                                path = obj.get("path", "?")
                                if "error" in obj:
                                    print(f"  ← ERROR: {path}: {obj['error']}")
                                else:
                                    data_str = str(obj.get("data", ""))
                                    if len(data_str) > 100:
                                        data_str = data_str[:100] + "..."
                                    print(f"  ← {path} = {data_str}")
                        except json.JSONDecodeError:
                            text = raw.decode("utf-8", errors="replace")
                            if self.verbose:
                                print(f"  ← (non-JSON): {text[:100]}")
            except asyncio.TimeoutError:
                break
        return responses

    async def run_consolelink_sequence(self, stay: bool = False):
        """Replay the exact ConsoleLink connection sequence."""
        await self.connect()

        # Phase 1: Initial keepalive (ConsoleLink sends /Sleep twice)
        print("\n[Phase 1] Keepalive...")
        self.send("set /Sleep false")
        self.send("set /Sleep false")

        # Phase 2: Subscribe to critical monitor controls
        print("[Phase 2] Monitor subscriptions...")
        monitor_subs = [
            "/SampleRate", "/ClockSource",
            "/devices/0/outputs/18/CRMonitorLevelTapered/value",
            "/devices/0/outputs/18/Mute/value",
            "/devices/0/outputs/18/MixToMono/value",
            "/devices/0/outputs/18/meters/0/MeterLevel/value",
            "/devices/0/outputs/18/meters/1/MeterLevel/value",
        ]
        for path in monitor_subs:
            self.send(f"subscribe {path}")

        responses = await self.recv_all(timeout=1.0)
        print(f"  Received {len(responses)} responses "
              f"({self.value_count} values, {self.error_count} errors)")

        # Phase 3: GET root tree (ConsoleLink does this to learn the structure)
        print("[Phase 3] GET / (tree structure)...")
        self.send("get /")
        responses = await self.recv_all(timeout=1.0)
        if responses:
            root = responses[0]
            data = root.get("data", {})
            props = list(data.get("properties", {}).keys())
            children = list(data.get("children", {}).keys())
            print(f"  Root: {len(props)} properties, {len(children)} children")
            # Check for critical properties
            has_init = "initialized" in props
            has_sleep = "Sleep" in props
            print(f"  /initialized: {'YES' if has_init else 'MISSING!'}")
            print(f"  /Sleep: {'YES' if has_sleep else 'MISSING!'}")

        # Phase 4: Subscribe to channel strip controls (input 0)
        print("[Phase 4] Channel strip subscriptions (input 0)...")
        channel_subs = [
            "/devices/0/inputs/0/FaderLevel/value",
            "/devices/0/inputs/0/FaderLevelTapered/value",
            "/devices/0/inputs/0/Pan/value",
            "/devices/0/inputs/0/Mute/value",
            "/devices/0/inputs/0/Solo/value",
            "/devices/0/inputs/0/Active/value",
            "/devices/0/inputs/0/Stereo/value",
            "/devices/0/inputs/0/UserDefinedName/value",
            "/devices/0/inputs/0/meters/0/MeterLevel/value",
            "/devices/0/inputs/0/meters/1/MeterLevel/value",
            "/devices/0/inputs/0/meters/0/Clip/value",
            "/devices/0/inputs/0/preamps/0/Gain/value",
            "/devices/0/inputs/0/preamps/0/GainTapered/value",
            "/devices/0/inputs/0/preamps/0/48V/value",
            "/devices/0/inputs/0/preamps/0/Pad/value",
            "/devices/0/inputs/0/preamps/0/Phase/value",
            "/devices/0/inputs/0/preamps/0/LowCut/value",
        ]
        for path in channel_subs:
            self.send(f"subscribe {path}")

        responses = await self.recv_all(timeout=1.0)
        print(f"  Received {len(responses)} responses "
              f"(total: {self.value_count} values, {self.error_count} errors)")

        # Phase 5: Subscribe to initialization state
        print("[Phase 5] Initialization state...")
        init_subs = [
            "/initialized/value",
            "/initialized_percent/value",
            "/initialized_status/value",
        ]
        for path in init_subs:
            self.send(f"subscribe {path}")

        responses = await self.recv_all(timeout=1.0)
        for r in responses:
            path = r.get("path", "")
            if "initialized" in path:
                print(f"  {path} = {r.get('data')}")

        # Phase 6: Test ping
        print("[Phase 6] Ping test...")
        self.send("get /ping")
        responses = await self.recv_all(timeout=1.0)
        if responses:
            ping_resp = responses[0]
            if "error" in ping_resp:
                print(f"  PING FAILED: {ping_resp['error']}")
            else:
                print(f"  PING OK: {ping_resp.get('data')}")

        # Summary
        print(f"\n{'='*50}")
        print(f"Total responses: {self.recv_count}")
        print(f"  Values: {self.value_count}")
        print(f"  Errors: {self.error_count}")
        print(f"{'='*50}")

        if self.error_count > 0:
            print(f"\nWARNING: {self.error_count} error responses received.")
            print("ConsoleLink may disconnect due to these errors.")

        # Phase 7: Stay connected and watch for meter updates
        if stay:
            print("\n[Staying connected — watching for meter updates...]")
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
                            print(f"  ← {path} = {r.get('data')}")
                    elapsed = time.monotonic() - start
                    if meter_count > 0 and int(elapsed) % 5 == 0:
                        rate = meter_count / elapsed if elapsed > 0 else 0
                        print(f"  Meters: {meter_count} updates "
                              f"({rate:.1f}/sec, {elapsed:.0f}s elapsed)")
            except asyncio.CancelledError:
                pass
        else:
            # Wait a moment to see if connection drops
            print("\nWaiting 5s to check connection stability...")
            responses = await self.recv_all(timeout=5.0)
            meter_updates = sum(1 for r in responses
                                if "MeterLevel" in r.get("path", "")
                                or "Clip" in r.get("path", ""))
            print(f"  Received {len(responses)} messages ({meter_updates} meter updates)")
            if meter_updates > 0:
                print("  Meter pump is working!")
            else:
                print("  WARNING: No meter updates received.")

        # Disconnect
        self.writer.close()
        try:
            await self.writer.wait_closed()
        except Exception:
            pass
        print("\nDisconnected.")


async def main():
    parser = argparse.ArgumentParser(
        description="ConsoleLink simulator for ua-mixer-daemon testing")
    parser.add_argument("--host", default="127.0.0.1",
                        help="Daemon host (default: 127.0.0.1)")
    parser.add_argument("--port", "-p", type=int, default=4710,
                        help="Daemon port (default: 4710)")
    parser.add_argument("--verbose", "-v", action="store_true",
                        help="Show all responses")
    parser.add_argument("--stay", "-s", action="store_true",
                        help="Stay connected and watch meter updates")
    args = parser.parse_args()

    client = TestClient(args.host, args.port, args.verbose)
    try:
        await client.run_consolelink_sequence(stay=args.stay)
    except ConnectionRefusedError:
        print(f"Connection refused at {args.host}:{args.port}")
        print("Is the daemon running? Start it with:")
        print("  python3 ua_mixer_daemon.py --no-hardware -v")
        sys.exit(1)
    except KeyboardInterrupt:
        print("\nInterrupted.")


if __name__ == "__main__":
    asyncio.run(main())
