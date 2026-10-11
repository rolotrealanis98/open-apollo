"""JSON frames: the command_format 1 framing of UA's client library.

UA's client serializer (UAClnt_Client / MxClnt_UAD2Client, command copy
constructor 181e956c4, serializer 181e90e5c) writes three forms.  Mode 1
(181ebbc2c) is:

    b"JSON" + uint32_le(payload length) + JSON object + one NUL

the length counting the terminating NUL; mode 2 is the UBJS frame
(ubjson_codec).  Its receive side (181e8f23c) takes both.  Recovered by the
Codex Wine/Console work (its analysis/vendor-specific-envelope.txt).

Strict UTF-8, finite numbers, unique keys and object-only payloads are
local validation choices, not claims about UA's parser.
"""

import json
import math
import struct

MAGIC = b"JSON"
HEADER_SIZE = 8                  # magic + uint32_le length
MAX_PAYLOAD = 4 * 1024 * 1024    # a local bound per frame


class JsonFrameError(ValueError):
    pass


def _unique_keys(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise JsonFrameError("duplicate object key")
        result[key] = value
    return result


def _reject_constant(name):
    raise JsonFrameError(f"non-finite JSON constant {name}")


def _check_finite(value):
    # 1e999 parses to inf without going through parse_constant
    if isinstance(value, float) and not math.isfinite(value):
        raise JsonFrameError("non-finite JSON number")
    if isinstance(value, dict):
        for item in value.values():
            _check_finite(item)
    elif isinstance(value, list):
        for item in value:
            _check_finite(item)


def decode_payload(body: bytes) -> dict:
    """The JSON object of a frame payload (JSON text + one final NUL)."""
    if not body.endswith(b"\0") or b"\0" in body[:-1]:
        raise JsonFrameError("expected exactly one terminal NUL")
    try:
        message = json.loads(body[:-1].decode("utf-8"),
                             object_pairs_hook=_unique_keys,
                             parse_constant=_reject_constant)
        _check_finite(message)
    except JsonFrameError:
        raise
    except (ValueError, UnicodeError, RecursionError) as exc:
        raise JsonFrameError("invalid JSON payload") from exc
    if type(message) is not dict:
        raise JsonFrameError("JSON message must be an object")
    return message


def encode_frame(message: dict, *, max_payload: int = MAX_PAYLOAD) -> bytes:
    """A dict as one JSON frame."""
    if type(message) is not dict:
        raise JsonFrameError("JSON message must be an object")
    try:
        body = json.dumps(message, ensure_ascii=False, allow_nan=False,
                          separators=(",", ":")).encode("utf-8") + b"\0"
    except (TypeError, ValueError, UnicodeError, RecursionError) as exc:
        raise JsonFrameError("invalid JSON message") from exc
    # The round trip catches keys that collide after JSON's key coercion
    decode_payload(body)
    if len(body) > max_payload:
        raise JsonFrameError("payload exceeds bound")
    return MAGIC + struct.pack("<I", len(body)) + body


def decode_frame(frame: bytes, *, max_payload: int = MAX_PAYLOAD) -> dict:
    """One complete JSON frame (header included) to its dict."""
    if not isinstance(frame, (bytes, bytearray)):
        raise JsonFrameError("frame must be bytes")
    if len(frame) < HEADER_SIZE or frame[:4] != MAGIC:
        raise JsonFrameError("invalid JSON frame header")
    size = struct.unpack_from("<I", frame, 4)[0]
    if size < 3 or size > max_payload or len(frame) != size + HEADER_SIZE:
        raise JsonFrameError("invalid JSON frame length")
    return decode_payload(bytes(frame[HEADER_SIZE:]))
