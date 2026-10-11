#!/usr/bin/env python3
"""daemon-protocol-check.py - check a running open-apollo mixer daemon's wire
protocol the way UA's Console and Mixer Helper use it.

Read-only by default: every check below only reads, or sends something the
daemon must refuse without writing anything.

    ping            text "get /ping" answered
    json-frames     after "post command_format 1", text and JSON-framed GETs
                    are answered in JSON frames ("JSON" + u32 LE length)
    ubjs-then-text  a UBJS frame followed by a text command in one write:
                    both answered
    split-header    a UBJS frame whose header arrives in two writes 50 ms
                    apart is answered
    oversize        "JSON" + length 0xffffffff: that connection is dropped,
                    the daemon keeps answering others
    clock-name      SET /ClockSource to a name no device has: "Unable to set
                    value.", /ClockSource unchanged
    clock-rate      SET /SampleRate 48000.5: "Unable to set value.",
                    /SampleRate unchanged
    clock-menu      /ClockSource's value list (printed, the device's menu
                    with driver ABI 1.7)

--write adds checks that change a setting and put it back:

    gain-companion  a UBJS client's GainTapered SET is answered with the
                    Gain companion in UBJS; a text client subscribed to
                    GainTapered gets exactly one push for a Gain SET
    dirty           a fader move sets /Dirty; "post /SaveLastUsedSession"
                    leaves it alone (and rewrites --state-file at once)
    clock-refused   SET /ClockSource to --refuse-source (default Dante) when
                    the menu lacks it: refused, nothing published.  If the
                    driver takes it anyway the old source is put back and
                    the check fails

--clock-switch (with --write) switches /ClockSource to another menu entry
and back, each answered with an echo.  The Apollo is unlocked while it runs
on a source with nothing plugged in.

Usage:
    tools/daemon-protocol-check.py [--host H] [--port 4710] [--write]
        [--hardware] [--clock-switch] [--refuse-source NAME]
        [--state-file PATH]

Run it against a daemon that has finished its start (about 5 s after
launch).  Exit status: 0 every check passed or was skipped, 1 a check
failed, 2 cannot connect.  It works against a "--no-hardware" daemon too;
clock-refused runs only with --hardware.
"""

import argparse
import json
import os
import socket
import sys
import time
from pathlib import Path

sys.path.append(str(Path(__file__).resolve().parent.parent / "mixer-engine"))
from json_frame_codec import encode_frame  # noqa: E402
from ubjson_codec import encode_response as ubjs_frame  # noqa: E402
from wire_command_framer import WireCommandFramer, WireError  # noqa: E402

NUL = b"\x00"
GAIN = "/devices/0/inputs/0/preamps/0/Gain/value"
TAPERED = "/devices/0/inputs/0/preamps/0/GainTapered/value"
FADER = "/devices/0/inputs/0/FaderLevel/value"
NO_SUCH_CLOCK = "NoSuchClockSource"
ENGINE_ERROR = "Unable to set value."


class Conn:
    """One daemon connection; replies of every framing come back as
    (kind, message) with kind "text", "json" or "ubjson"."""

    def __init__(self, host: str, port: int, timeout: float = 3.0):
        self.sock = socket.create_connection((host, port), timeout=timeout)
        self.framer = WireCommandFramer()
        self.pending: list[tuple[str, object]] = []
        self.eof = False

    def close(self):
        self.sock.close()

    def text(self, command: str):
        self.sock.sendall(command.encode("utf-8") + NUL)

    def raw(self, data: bytes):
        self.sock.sendall(data)

    def _pump(self, timeout: float) -> bool:
        """Read once; False on timeout or end of stream."""
        self.sock.settimeout(max(timeout, 0.01))
        try:
            data = self.sock.recv(65536)
        except socket.timeout:
            return False
        except OSError:
            self.eof = True
            return False
        if not data:
            self.eof = True
            return False
        for kind, msg in self.framer.feed(data):
            if kind == "text":
                try:
                    msg = json.loads(msg)
                except ValueError:
                    pass
            self.pending.append((kind, msg))
        return True

    def wait(self, pred, total: float = 2.0):
        """The first (kind, msg) for which pred(msg) is true, else None.
        Messages that don't match stay queued for later waits."""
        end = time.monotonic() + total
        while True:
            for i, (kind, msg) in enumerate(self.pending):
                if isinstance(msg, dict) and pred(msg):
                    return self.pending.pop(i)
            left = end - time.monotonic()
            if left <= 0 or self.eof:
                return None
            self._pump(left)

    def drain(self, idle: float = 0.3, total: float = 2.0) -> list:
        """Everything that arrives until `idle` seconds pass quietly."""
        end = time.monotonic() + total
        while time.monotonic() < end and self._pump(
                min(idle, end - time.monotonic())):
            pass
        out, self.pending = self.pending, []
        return out

    def closed_by_peer(self, total: float) -> bool:
        end = time.monotonic() + total
        while not self.eof and time.monotonic() < end:
            try:
                self._pump(end - time.monotonic())
            except WireError:
                pass
        return self.eof

    def get(self, path: str, total: float = 2.0):
        self.text(f"get {path}")
        hit = self.wait(lambda m: m.get("path") == path and "data" in m,
                        total)
        return None if hit is None else hit[1]["data"]


def for_path(path, func_id=None):
    def pred(m):
        if m.get("path") != path:
            return False
        if func_id is None:
            return True
        return str((m.get("parameters") or {}).get("func_id")) == str(func_id)
    return pred


class Checker:
    def __init__(self, args):
        self.args = args
        self.failed = 0

    def conn(self) -> Conn:
        c = Conn(self.args.host, self.args.port)
        c.drain(idle=0.2, total=1.0)    # the init pushes every client gets
        return c

    def report(self, verdict: str, name: str, detail: str = ""):
        if verdict == "FAIL":
            self.failed += 1
        print(f"{verdict:4} {name:15} {detail}", flush=True)

    def check(self, name: str, ok: bool, good: str, bad: str):
        self.report("PASS" if ok else "FAIL", name, good if ok else bad)

    # ── read-only ──────────────────────────────────────────────────

    def ping(self):
        c = self.conn()
        v = c.get("/ping")
        c.close()
        self.check("ping", v is True, "get /ping -> true", f"got {v!r}")

    def json_frames(self):
        c = self.conn()
        c.text("post command_format?func_id=71 1")
        ack = c.wait(for_path("command_format", 71))
        c.text("get /ping")
        text_reply = c.wait(for_path("/ping"))
        c.raw(encode_frame({"cmd": "get", "url": "/ping"}))
        frame_reply = c.wait(for_path("/ping"))
        c.close()
        kinds = [r and r[0] for r in (ack, text_reply, frame_reply)]
        self.check("json-frames", kinds == ["json"] * 3,
                   "ack, text GET and JSON-framed GET answered in JSON frames",
                   f"reply framings {kinds} (want json x3)")

    def ubjs_then_text(self):
        c = self.conn()
        c.raw(ubjs_frame({"cmd": "get", "url": "/ping"})
              + b"get /SampleRate/value\0")
        a = c.wait(for_path("/ping"))
        b = c.wait(for_path("/SampleRate/value"))
        c.close()
        self.check("ubjs-then-text", a is not None and b is not None,
                   f"both answered ({a[0] if a else '-'}, "
                   f"{b[0] if b else '-'})",
                   f"/ping {'ok' if a else 'missing'}, /SampleRate "
                   f"{'ok' if b else 'missing (text after a frame dropped)'}")

    def split_header(self):
        c = self.conn()
        frame = ubjs_frame({"cmd": "get", "url": "/ping"})
        c.raw(frame[:3])
        time.sleep(0.05)
        c.raw(frame[3:])
        r = c.wait(for_path("/ping"))
        c.close()
        self.check("split-header", r is not None and r[1].get("data") is True,
                   "UBJS header split across writes answered",
                   "no reply")

    def oversize(self):
        c = self.conn()
        c.raw(b"JSON\xff\xff\xff\xff")
        dropped = c.closed_by_peer(3.0)
        c.close()
        c2 = self.conn()
        alive = c2.get("/ping") is True
        c2.close()
        self.check("oversize", dropped and alive,
                   "connection dropped, daemon still answers",
                   f"dropped={dropped} daemon answers={alive}")

    def clock_name(self):
        c = self.conn()
        before = c.get("/ClockSource/value")
        c.text(f'set /ClockSource/value?func_id=72 "{NO_SUCH_CLOCK}"')
        r = c.wait(for_path("/ClockSource/value", 72))
        after = c.get("/ClockSource/value")
        c.close()
        err = r and r[1].get("error")
        self.check("clock-name", err == ENGINE_ERROR and after == before,
                   f'refused ("{err}"), still {after!r}',
                   f"reply {r and r[1]!r}, {before!r} -> {after!r}")

    def clock_rate(self):
        c = self.conn()
        before = c.get("/SampleRate/value")
        c.text("set /SampleRate/value?func_id=73 48000.5")
        r = c.wait(for_path("/SampleRate/value", 73))
        after = c.get("/SampleRate/value")
        c.close()
        err = r and r[1].get("error")
        self.check("clock-rate", err == ENGINE_ERROR and after == before,
                   f'refused ("{err}"), still {after!r}',
                   f"reply {r and r[1]!r}, {before!r} -> {after!r}")

    def clock_menu(self) -> list:
        c = self.conn()
        prop = c.get("/ClockSource")
        c.close()
        values = prop.get("values") if isinstance(prop, dict) else None
        if not values:
            self.report("FAIL", "clock-menu", f"/ClockSource has no value "
                        f"list: {prop!r}")
            return []
        self.report("INFO", "clock-menu",
                    f"{values}, current {prop.get('value')!r}")
        return values

    # ── --write ────────────────────────────────────────────────────

    def gain_companion(self):
        ub = self.conn()
        watcher = self.conn()
        orig = watcher.get(GAIN)
        if not isinstance(orig, (int, float)):
            self.report("SKIP", "gain-companion", f"no {GAIN} ({orig!r})")
            ub.close()
            watcher.close()
            return
        ub.text("post command_format?func_id=74 2")
        ub.wait(for_path("command_format", 74))
        ub.raw(ubjs_frame({"cmd": "set", "url": TAPERED, "data": 0.6}))
        comp = ub.wait(for_path(GAIN))
        gain_now = watcher.get(GAIN)
        ok1 = (comp is not None and comp[0] == "ubjson"
               and comp[1].get("data") == gain_now)
        self.check("gain-companion", ok1,
                   f"GainTapered 0.6 -> Gain companion {gain_now} in UBJS",
                   f"companion {comp!r}, Gain now {gain_now!r}")

        watcher.text(f"subscribe {TAPERED}")
        watcher.drain(idle=0.3)
        target = 32.0 if orig != 32.0 else 30.0
        ub.raw(ubjs_frame({"cmd": "set", "url": GAIN, "data": target}))
        pushes = [m for _, m in watcher.drain(idle=0.5)
                  if isinstance(m, dict) and m.get("path") == TAPERED]
        self.check("gain-companion", len(pushes) == 1,
                   f"one GainTapered push for a Gain SET ({target} dB)",
                   f"{len(pushes)} GainTapered pushes: {pushes!r}")

        ub.raw(ubjs_frame({"cmd": "set", "url": GAIN, "data": orig}))
        time.sleep(0.3)
        back = watcher.get(GAIN)
        ub.close()
        watcher.close()
        if back != orig:
            self.report("FAIL", "gain-companion",
                        f"input 1 gain not restored: {back!r} (was {orig!r})")

    def dirty(self):
        c = self.conn()
        c.text("subscribe /Dirty/value")
        c.drain(idle=0.3)
        was = c.get("/Dirty/value")
        orig = c.get(FADER)
        if not isinstance(orig, (int, float)):
            self.report("SKIP", "dirty", f"no {FADER} ({orig!r})")
            c.close()
            return
        moved = orig - 0.5 if orig > -60 else orig + 0.5
        c.text(f"set {FADER} {moved}")
        if was is False:
            hit = c.wait(lambda m: m.get("path") == "/Dirty/value"
                         and m.get("data") is True, 2.0)
            self.check("dirty", hit is not None,
                       "fader move set /Dirty true",
                       "no /Dirty true push after a fader move")
        else:
            self.report("INFO", "dirty", f"/Dirty was already {was!r}; "
                        "set test skipped")
        c.text(f"set {FADER} {orig}")
        c.drain(idle=0.3)
        mtime = None
        if self.args.state_file:
            try:
                mtime = os.stat(self.args.state_file).st_mtime
            except OSError as e:
                self.report("FAIL", "dirty", f"--state-file: {e}")
        before = c.get("/Dirty/value")
        c.text("post /SaveLastUsedSession?func_id=75 {}")
        c.wait(for_path("/SaveLastUsedSession", 75), 2.0)
        time.sleep(0.5)
        after = c.get("/Dirty/value")
        self.check("dirty", after == before,
                   f"SaveLastUsedSession left /Dirty {after!r}",
                   f"SaveLastUsedSession changed /Dirty {before!r} -> "
                   f"{after!r}")
        if mtime is not None:
            try:
                new = os.stat(self.args.state_file).st_mtime
            except OSError:
                new = mtime
            self.check("dirty", new > mtime,
                       "state file rewritten within 0.5 s",
                       "state file not rewritten within 0.5 s")
        c.close()

    def clock_refused(self, menu: list, hardware: bool):
        name = self.args.refuse_source
        if not hardware:
            self.report("SKIP", "clock-refused", "needs --hardware (without "
                        "a device the daemon validates names only)")
            return
        if name in menu:
            self.report("SKIP", "clock-refused", f"{name!r} is in the menu")
            return
        c = self.conn()
        before = c.get("/ClockSource/value")
        c.text(f'set /ClockSource/value?func_id=76 "{name}"')
        r = c.wait(for_path("/ClockSource/value", 76))
        after = c.get("/ClockSource/value")
        err = r and r[1].get("error")
        if err != ENGINE_ERROR and before is not None:
            c.text(f'set /ClockSource/value?func_id=77 "{before}"')
            c.wait(for_path("/ClockSource/value", 77))
        c.close()
        self.check("clock-refused", err == ENGINE_ERROR and after == before,
                   f'{name} refused, still {after!r}',
                   f"reply {r and r[1]!r}, {before!r} -> {after!r}")

    def clock_switch(self, menu: list):
        c = self.conn()
        before = c.get("/ClockSource/value")
        others = [n for n in menu if n != before]
        if not others:
            self.report("SKIP", "clock-switch", "one source only")
            c.close()
            return
        for i, name in enumerate((others[0], before)):
            c.text(f'set /ClockSource/value?func_id={78 + i} "{name}"')
            r = c.wait(for_path("/ClockSource/value", 78 + i), 4.0)
            now = c.get("/ClockSource/value")
            good = r is not None and r[1].get("data") == name and now == name
            self.check("clock-switch", good, f"-> {name} echoed and published",
                       f"-> {name}: reply {r and r[1]!r}, now {now!r}")
            time.sleep(1.0)
        c.close()

def main(argv=None) -> int:
    p = argparse.ArgumentParser(
        description="check a running open-apollo mixer daemon's protocol")
    p.add_argument("--host", default=os.environ.get("OA_DAEMON_HOST",
                                                    "127.0.0.1"))
    p.add_argument("--port", type=int,
                   default=int(os.environ.get("OA_DAEMON_PORT", 4710)))
    p.add_argument("--write", action="store_true",
                   help="also run the checks that change a setting and put "
                   "it back")
    p.add_argument("--clock-switch", action="store_true",
                   help="with --write: switch /ClockSource and back")
    p.add_argument("--refuse-source", default="Dante",
                   help="a source the device does not offer (default Dante)")
    p.add_argument("--state-file", help="the daemon's state file, to check "
                   "that SaveLastUsedSession rewrites it")
    p.add_argument("--hardware", action="store_true",
                   help="the daemon drives an Apollo (enables clock-refused)")
    args = p.parse_args(argv)

    ck = Checker(args)
    try:
        ck.ping()
    except OSError as e:
        print(f"cannot connect to {args.host}:{args.port}: {e}",
              file=sys.stderr)
        return 2
    for step in (ck.json_frames, ck.ubjs_then_text, ck.split_header,
                 ck.oversize, ck.clock_name, ck.clock_rate):
        step()
    menu = ck.clock_menu()
    if args.write:
        ck.dirty()              # first: the other writes set /Dirty
        ck.gain_companion()
        ck.clock_refused(menu, args.hardware)
        if args.clock_switch:
            ck.clock_switch(menu)
    print(f"{'FAILED' if ck.failed else 'OK'}: {ck.failed} failed")
    return 1 if ck.failed else 0


if __name__ == "__main__":
    sys.exit(main())
