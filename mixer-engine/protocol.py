"""
TCP:4710 protocol framing and command parsing.

The UA Mixer Engine protocol uses null-terminated (\0) UTF-8 messages
over TCP. Commands are text-based with a REST-like path structure:

    set <path> <value>\0
    get <path>?<query>\0
    subscribe <path>\0
    unsubscribe <path>\0

Responses are JSON objects, also null-terminated:

    {"path": "<path>", "data": <value>}\0
"""

import json
import logging
from dataclasses import dataclass, field
from typing import Any
from urllib.parse import parse_qs, urlparse

log = logging.getLogger(__name__)

NULL = b"\x00"


# ── Message Framing ─────────────────────────────────────────────────

class MessageFramer:
    """Accumulates TCP data and yields complete null-terminated messages."""

    def __init__(self):
        self.buf = b""

    def feed(self, data: bytes) -> list[str]:
        """Feed raw TCP bytes, return list of complete decoded messages."""
        self.buf += data
        messages = []
        while NULL in self.buf:
            raw, self.buf = self.buf.split(NULL, 1)
            if raw:
                messages.append(raw.decode("utf-8", errors="replace"))
        return messages


def encode_response_bytes(path: str, data: Any, params: dict | None = None) -> bytes:
    """Encode a JSON response as null-terminated bytes."""
    msg = {"path": path, "data": data}
    if params:
        msg["parameters"] = params
    return (json.dumps(msg, separators=(",", ":")) + "\0").encode("utf-8")


def encode_error_bytes(path: str, verb: str) -> bytes:
    """Encode an error response matching the real mixer engine format."""
    msg = {"path": path, "error": f"Unable to resolve path for {verb}."}
    return (json.dumps(msg, separators=(",", ":")) + "\0").encode("utf-8")


# ── Command Parsing ─────────────────────────────────────────────────

@dataclass
class Command:
    """Parsed TCP:4710 command."""
    verb: str           # "get", "set", "subscribe", "unsubscribe", "post", "json"
    path: str           # e.g. "/devices/0/inputs/0/preamps/0/Gain/value"
    value: Any = None   # For SET: the value to set
    recursive: bool = False
    propfilter: list[str] = field(default_factory=list)
    json_data: dict = field(default_factory=dict)  # For raw JSON messages
    raw_params: dict = field(default_factory=dict)  # All query params (for echo)


def parse_value(raw: str) -> Any:
    """Parse a value string into the appropriate Python type."""
    if raw == "true":
        return True
    if raw == "false":
        return False
    # Try int
    try:
        return int(raw)
    except ValueError:
        pass
    # Try float
    try:
        return float(raw)
    except ValueError:
        pass
    # Strip quotes for strings
    if len(raw) >= 2 and raw[0] == '"' and raw[-1] == '"':
        return raw[1:-1]
    return raw


def _parse_path_with_params(path_raw: str) -> tuple[str, bool, list[str], dict]:
    """Parse a path that may contain query params.

    Returns (path, recursive, propfilter, raw_params).
    raw_params has all query params as single-value strings for echoing in responses.
    """
    recursive = False
    propfilter = []
    raw_params = {}
    if "?" in path_raw:
        path, query_str = path_raw.split("?", 1)
        params = parse_qs(query_str, keep_blank_values=True)
        # Handle both ?recursive (blank value) and ?recursive=1
        rec_val = params.get("recursive", [None])
        recursive = rec_val[0] is not None and rec_val[0] != "0" if rec_val != [None] else False
        if "recursive" in query_str.split("&") or "recursive=" in query_str:
            recursive = True
        pf = params.get("propfilter", [""])[0]
        if pf:
            propfilter = [p.strip() for p in pf.split(",") if p.strip()]
        # Flatten all params to single values for echo
        raw_params = {k: v[0] if len(v) == 1 else v for k, v in params.items()}
    else:
        path = path_raw
    return path or "/", recursive, propfilter, raw_params


def parse_command(text: str) -> Command | None:
    """Parse a raw text message into a Command, or None if unparseable.

    Handles:
        get <path>?<query>
        set <path>?<query> <value>
        subscribe <path>?<query>
        unsubscribe <path>?<query>
        post <path>?<query> <value>
        {"path": "...", "data": ...}     (JSON identification messages)
    """
    text = text.strip()
    if not text:
        return None

    # Check for raw JSON messages (client identification)
    # e.g. {"path":"networkID","data":"abc123"}
    if text.startswith("{"):
        try:
            obj = json.loads(text)
            path = obj.get("path", "")
            data = obj.get("data")
            return Command(verb="json", path=path, value=data, json_data=obj)
        except json.JSONDecodeError:
            return None

    parts = text.split(None, 1)
    if not parts:
        return None

    verb = parts[0].lower()
    rest = parts[1] if len(parts) > 1 else ""

    if verb == "set":
        # "set <path>?<params> <value>"
        set_parts = rest.split(None, 1)
        if not set_parts:
            return None
        path, recursive, _, raw_params = _parse_path_with_params(set_parts[0])
        value = parse_value(set_parts[1]) if len(set_parts) > 1 else None
        return Command(verb="set", path=path, value=value, raw_params=raw_params)

    elif verb == "get":
        path, recursive, propfilter, raw_params = _parse_path_with_params(rest.strip())
        return Command(verb="get", path=path, recursive=recursive,
                       propfilter=propfilter, raw_params=raw_params)

    elif verb in ("subscribe", "unsubscribe"):
        path_raw = rest.strip()
        if not path_raw:
            return None
        # Batch subscribe: "subscribe {"paths":[...]}" — preserve JSON body as-is
        if path_raw.startswith("{"):
            return Command(verb=verb, path=path_raw)
        path, recursive, _, raw_params = _parse_path_with_params(path_raw)
        return Command(verb=verb, path=path, recursive=recursive, raw_params=raw_params)

    elif verb == "post":
        # "post <path>?<params> <value>"
        post_parts = rest.split(None, 1)
        if not post_parts:
            return None
        path, _, _, raw_params = _parse_path_with_params(post_parts[0])
        value = parse_value(post_parts[1]) if len(post_parts) > 1 else None
        return Command(verb="post", path=path, value=value, raw_params=raw_params)

    else:
        log.warning("Unknown command verb: %r", verb)
        return None
