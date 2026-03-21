"""
WebSocket:4720 protocol — message parsing and encoding.

The UA Mixer Helper (Windows/Wine) uses text-based WebSocket frames:

    <verb> <path>?<query_params> [<json_data>]

Verbs: get, set, subscribe, unsubscribe, post

Query parameters:
    message_id   — echoed back in response for request/response matching
    recursive    — "1" to recurse into children
    flatvalue    — "1" for flat value format
    propfilter   — comma-separated property names to include
    cmd_id       — command identifier

Responses are JSON text frames:
    {"path":"/devices/0/Gain/value","data":10.0,"parameters":{"message_id":"abc:1"}}
"""

import json
import logging
from dataclasses import dataclass, field
from typing import Any
from urllib.parse import parse_qs

from protocol import parse_value

log = logging.getLogger(__name__)


@dataclass
class WsCommand:
    """Parsed WebSocket command."""
    verb: str             # get, set, subscribe, unsubscribe, post
    path: str             # /devices/0/inputs/0/Gain/value
    value: Any = None
    message_id: str = ""
    recursive: bool = False
    propfilter: list[str] = field(default_factory=list)
    flatvalue: bool = False
    cmd_id: str = ""
    raw_params: dict = field(default_factory=dict)


def parse_ws_command(text: str) -> WsCommand | None:
    """Parse a WebSocket text frame into a WsCommand.

    Format: <verb> <path>?<params> [<value_or_json>]

    Examples:
        get /devices?message_id=abc:1&recursive=1
        set /devices/0/inputs/0/Gain/value?message_id=abc:2&cmd_id=set_command 10.0
        subscribe /devices/0/inputs/0/Gain/value?message_id=abc:3
        post /request_challenge?func_id=1
        post command_format?func_id=1 2
    """
    text = text.strip()
    if not text:
        return None

    # Split verb from rest
    parts = text.split(None, 1)
    if not parts:
        return None

    verb = parts[0].lower()
    rest = parts[1] if len(parts) > 1 else ""

    if verb not in ("get", "set", "subscribe", "unsubscribe", "post"):
        log.warning("WS: unknown verb %r", verb)
        return None

    # For SET: split path+params from value
    if verb == "set":
        set_parts = rest.split(None, 1)
        if not set_parts:
            return None
        path_with_params = set_parts[0]
        value_str = set_parts[1] if len(set_parts) > 1 else None
    elif verb == "post":
        post_parts = rest.split(None, 1)
        if not post_parts:
            return None
        path_with_params = post_parts[0]
        value_str = post_parts[1] if len(post_parts) > 1 else None
    else:
        # GET/SUBSCRIBE/UNSUBSCRIBE may also have trailing value/body
        other_parts = rest.split(None, 1)
        path_with_params = other_parts[0] if other_parts else ""
        value_str = other_parts[1] if len(other_parts) > 1 else None

    # Split path from query string
    if "?" in path_with_params:
        path, query_string = path_with_params.split("?", 1)
    else:
        path = path_with_params
        query_string = ""

    path = path or "/"

    # Parse query parameters
    raw_params = parse_qs(query_string)
    message_id = raw_params.get("message_id", [""])[0]
    recursive = raw_params.get("recursive", ["0"])[0] == "1"
    flatvalue = raw_params.get("flatvalue", ["0"])[0] == "1"
    cmd_id = raw_params.get("cmd_id", [""])[0]
    pf_str = raw_params.get("propfilter", [""])[0]
    propfilter = [p.strip() for p in pf_str.split(",") if p.strip()] if pf_str else []

    # Parse value
    value = None
    if value_str is not None:
        value_str = value_str.strip()
        if value_str:
            # Try JSON first (for objects/arrays)
            if value_str.startswith("{") or value_str.startswith("["):
                try:
                    value = json.loads(value_str)
                except json.JSONDecodeError:
                    value = parse_value(value_str)
            else:
                value = parse_value(value_str)

    return WsCommand(
        verb=verb,
        path=path,
        value=value,
        message_id=message_id,
        recursive=recursive,
        propfilter=propfilter,
        flatvalue=flatvalue,
        cmd_id=cmd_id,
        raw_params={k: v[0] if len(v) == 1 else v for k, v in raw_params.items()},
    )


def encode_ws_response(path: str, data: Any,
                       message_id: str = "", params: dict | None = None) -> str:
    """Encode a JSON response as a WebSocket text frame.

    Args:
        path: Response path
        data: Response data (any JSON-serializable value)
        message_id: If set, echoed back in parameters
        params: Additional parameters to include

    Returns:
        JSON string ready to send as a WS text frame
    """
    msg: dict[str, Any] = {"path": path, "data": data}
    if message_id or params:
        p = dict(params) if params else {}
        if message_id:
            p["message_id"] = message_id
        msg["parameters"] = p
    return json.dumps(msg, separators=(",", ":"))


def encode_ws_error(path: str, error_msg: str, message_id: str = "") -> str:
    """Encode an error response as a WebSocket text frame."""
    msg: dict[str, Any] = {"path": path, "error": error_msg}
    if message_id:
        msg["parameters"] = {"message_id": message_id}
    return json.dumps(msg, separators=(",", ":"))
