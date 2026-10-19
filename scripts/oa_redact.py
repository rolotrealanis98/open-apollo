#!/usr/bin/env python3
"""oa_redact.py — take the user's and the machine's identity out of text.

Shared by the install report (scripts/install.sh imports it) and the install
logs (scripts/oa-log.sh runs it as a filter).  What it removes from every
string:
  - the desktop user's home folder (-> <home>) and user name (-> <user>)
  - this computer's host name (-> <host>) and /etc/machine-id (-> <machine-id>)
  - serial numbers: "serial: 2000...", "S/N 1234AB" (-> <redacted>)
  - MAC addresses (-> <mac>)
and, in log lines, terminal colour codes and control characters.

As a filter:
  python3 oa_redact.py [--user NAME] [--append FILE] [--tee]
reads lines on stdin and appends them, redacted and time-stamped, to FILE
(stdout without --append).  --tee also copies every line unchanged to stdout,
so a script can show its own output while the log gets the redacted copy.
The file is created with mode 0600 when missing; nothing else is written.
"""

from __future__ import annotations

import argparse
import os
import pwd
import re
import socket
import sys
import time

# Serial numbers (the driver logs the Apollo serial)
SERIAL_RE = re.compile(
    r"((?:serial(?:[ _]?number)?|s/n)\s*[:=#]?\s*)[0-9A-Za-z][0-9A-Za-z-]{5,}",
    re.I)
MAC_RE = re.compile(r"(?<![0-9A-Fa-f:])(?:[0-9A-Fa-f]{2}[:_-]){5}[0-9A-Fa-f]{2}(?![0-9A-Fa-f:])")
ANSI_RE = re.compile(r"\x1b(?:\[[0-?]*[ -/]*[@-~]|[@-Z\\-_])")
CONTROL_RE = re.compile(r"[\x00-\x08\x0b-\x1f\x7f]")
MAX_LINE = 4000


def _word(text: str) -> str:
    return r"(?<![A-Za-z0-9])" + re.escape(text) + r"(?![A-Za-z0-9])"


def words_for(user: str, home: str, host: str, machine_id: str) -> list[tuple[re.Pattern, str]]:
    """The patterns for one identity, most specific first; short or
    meaningless values ("", "/", "root", "localhost") are left out."""
    words: list[tuple[str, str]] = []
    if home and len(home) >= 2 and home != "/":
        words.append((re.escape(home), "<home>"))
    if machine_id:
        words.append((re.escape(machine_id), "<machine-id>"))
    for h in sorted({host, host.split(".")[0]}, key=len, reverse=True):
        if len(h) >= 2 and h != "localhost":
            words.append((_word(h), "<host>"))
    if len(user) >= 2 and user != "root":
        words.append((_word(user), "<user>"))
    return [(re.compile(p), r) for p, r in words]


def identity_words(user: str | None = None) -> list[tuple[re.Pattern, str]]:
    """This computer's identity and `user`'s (default: whoever ran sudo,
    else whoever runs this unless that is root)."""
    if user is None:
        user = os.environ.get("SUDO_USER") or ""
        if not user and os.geteuid() != 0:
            try:
                user = pwd.getpwuid(os.geteuid()).pw_name
            except KeyError:
                user = ""
    home = ""
    if user:
        try:
            home = pwd.getpwnam(user).pw_dir
        except KeyError:
            pass
    try:
        with open("/etc/machine-id") as f:
            machine_id = f.read().strip()
    except OSError:
        machine_id = ""
    return words_for(user, home, socket.gethostname(), machine_id)


def redact(text: str, words) -> str:
    text = SERIAL_RE.sub(lambda m: m.group(1) + "<redacted>", text)
    text = MAC_RE.sub("<mac>", text)
    for pat, repl in words:
        text = pat.sub(repl, text)
    return text


def scrub(obj, words):
    """`obj` (dicts, lists, strings) with every string redacted."""
    if isinstance(obj, dict):
        return {k: scrub(v, words) for k, v in obj.items()}
    if isinstance(obj, list):
        return [scrub(v, words) for v in obj]
    if isinstance(obj, str):
        return redact(obj, words)
    return obj


def clean_line(raw: str, words) -> str:
    """One log line: no colour codes, no control characters, bounded, redacted."""
    line = ANSI_RE.sub("", raw.rstrip("\n"))
    # A progress bar redraws with \r: keep its last state
    line = line.rstrip("\r").rsplit("\r", 1)[-1]
    line = CONTROL_RE.sub("", line)
    if len(line) > MAX_LINE:
        line = line[:MAX_LINE] + " …(cut)"
    return redact(line, words)


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--user", default=None, help="the desktop user whose name and home to remove")
    ap.add_argument("--append", metavar="FILE", help="append to FILE instead of stdout")
    ap.add_argument("--tee", action="store_true", help="also copy each line, unchanged, to stdout")
    args = ap.parse_args(argv)
    words = identity_words(args.user)
    out = sys.stdout
    if args.append:
        try:
            fd = os.open(args.append, os.O_WRONLY | os.O_APPEND | os.O_CREAT | os.O_NOFOLLOW, 0o600)
            out = os.fdopen(fd, "w", encoding="utf-8", buffering=1)
        except OSError as e:
            if not args.tee:
                raise
            # The output must still get through: no log, then
            sys.stderr.write(f"oa_redact: no log ({e.strerror}): {args.append}\n")
            out = None
    raw_out = sys.stdout.buffer if args.tee else None
    for raw in sys.stdin.buffer:
        if raw_out is not None:
            try:
                raw_out.write(raw)
                raw_out.flush()
            except OSError:          # the window that showed it is gone: keep logging
                raw_out = None
        if out is not None:
            line = clean_line(raw.decode("utf-8", "replace"), words)
            out.write(time.strftime("%H:%M:%S ") + line + "\n")
            out.flush()
    return 0


if __name__ == "__main__":
    sys.exit(main())
