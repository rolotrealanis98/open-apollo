#!/usr/bin/env python3
"""Tiny client for the open-apollo mixer daemon (stdlib only), for titan tests.

    oa-daemon-client.py set PATH VALUE     set a control, print the echo
    oa-daemon-client.py get PATH           print the value at PATH
    oa-daemon-client.py watch [--for SECONDS] PATH...   subscribe, print pushes

Examples:
    oa-daemon-client.py set /devices/0/inputs/0/FaderLevel/value -6
    oa-daemon-client.py set /devices/0/inputs/1/Solo/value true
    oa-daemon-client.py get /devices/0/inputs/0/Pan/value
    oa-daemon-client.py watch /devices/0/inputs/0/FaderLevel/value

The daemon speaks NUL-terminated messages: commands are text
("set <path> <value>"), replies and pushes are JSON {"path":..., "data":...}.
Host and port: --host/--port, or OA_DAEMON_HOST / OA_DAEMON_PORT; the
default port is 4711 (the daemon itself defaults to 4710 for /dev/ua_apollo0;
pass --port 4710 for a stock start).  A second Apollo is 100 higher per index.

VALUE is sent as given: true/false, a number, or a string (quote a string
that contains spaces).  set asks the daemon for an echo (func_id) so a
rejected path shows up as an error instead of silence.  Exit status: 0 ok,
1 the daemon answered with an error or nothing came back, 2 usage/connection.
"""

import argparse
import json
import os
import socket
import sys
import time

NUL = b"\x00"
DEFAULT_PORT = 4711


class Conn:
    def __init__(self, host: str, port: int, timeout: float):
        self.sock = socket.create_connection((host, port), timeout=timeout)
        self.buf = b""

    def send(self, text: str) -> None:
        self.sock.sendall(text.encode("utf-8") + NUL)

    def messages(self, timeout, total=None):
        """Yield decoded messages (dict, or the raw text when not JSON)
        until `timeout` seconds pass without data (None = no idle limit)
        or `total` seconds have passed since the call (None = no limit)."""
        end = None if total is None else time.monotonic() + total
        while True:
            if end is not None:
                left = end - time.monotonic()
                if left <= 0:
                    return
                self.sock.settimeout(left if timeout is None
                                     else min(timeout, left))
            else:
                self.sock.settimeout(timeout)
            while NUL in self.buf:
                raw, self.buf = self.buf.split(NUL, 1)
                if not raw:
                    continue
                text = raw.decode("utf-8", errors="replace")
                try:
                    yield json.loads(text)
                except ValueError:
                    yield text
            try:
                data = self.sock.recv(65536)
            except socket.timeout:
                return
            if not data:
                return
            self.buf += data


def show(msg) -> str:
    return msg if isinstance(msg, str) else json.dumps(msg, separators=(",", ":"))


def cmd_get(conn: Conn, args) -> int:
    conn.send(f"get {args.path}")
    for msg in conn.messages(args.timeout):
        if isinstance(msg, dict) and msg.get("path") == args.path:
            if "error" in msg:
                print(show(msg), file=sys.stderr)
                return 1
            print(json.dumps(msg.get("data")))
            return 0
    print(f"no reply for {args.path}", file=sys.stderr)
    return 1


def cmd_set(conn: Conn, args) -> int:
    conn.send(f"set {args.path}?func_id=1 {args.value}")
    status = 1
    for msg in conn.messages(args.timeout):
        # the daemon also pushes its init values on connect: show only ours
        if isinstance(msg, dict) and msg.get("path") == args.path:
            print(show(msg))
            status = 1 if "error" in msg else 0
    return status


def cmd_watch(conn: Conn, args) -> int:
    for path in args.paths:
        conn.send(f"subscribe {path}")
    t0 = time.monotonic()
    try:
        for msg in conn.messages(None, args.duration):
            print(f"{time.monotonic() - t0:8.3f} {show(msg)}", flush=True)
    except KeyboardInterrupt:
        pass
    return 0


def main(argv=None) -> int:
    p = argparse.ArgumentParser(
        description="open-apollo mixer daemon client (set / get / watch)")
    p.add_argument("--host", default=os.environ.get("OA_DAEMON_HOST",
                                                    "127.0.0.1"))
    p.add_argument("--port", type=int, default=int(
        os.environ.get("OA_DAEMON_PORT", DEFAULT_PORT)))
    p.add_argument("--timeout", type=float, default=1.0,
                   help="seconds to wait for a reply (default 1.0)")
    sub = p.add_subparsers(dest="verb", required=True)
    s = sub.add_parser("set", help="set PATH VALUE")
    s.add_argument("path")
    s.add_argument("value")
    g = sub.add_parser("get", help="get PATH")
    g.add_argument("path")
    w = sub.add_parser("watch", help="watch PATH... (Ctrl-C to stop)")
    w.add_argument("paths", nargs="+")
    w.add_argument("--for", dest="duration", type=float, default=None,
                   metavar="SECONDS", help="stop after this long")
    args = p.parse_args(argv)

    try:
        conn = Conn(args.host, args.port, args.timeout)
    except OSError as e:
        print(f"cannot connect to {args.host}:{args.port}: {e}",
              file=sys.stderr)
        return 2
    with conn.sock:
        return {"set": cmd_set, "get": cmd_get,
                "watch": cmd_watch}[args.verb](conn, args)


if __name__ == "__main__":
    sys.exit(main())
