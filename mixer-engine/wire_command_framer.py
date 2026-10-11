"""Input framing of a TCP:4710/4720 connection.

A client sends NUL-terminated text commands ("get /ping"), UBJS frames
(command_format 2) and JSON frames (command_format 1), and may mix them: a
few trailing text commands follow command_format before the first frame,
and a frame's 8-byte header can be split across TCP reads.  One framer
yields them in order as (kind, message) pairs:

    ("text", str)       a text command, decoded UTF-8 (errors replaced)
    ("ubjson", value)   a UBJS frame's decoded payload (a dict when valid)
    ("json", dict)      a JSON frame's object

A frame whose payload does not decode is skipped with a warning, as the
daemon's UBJSON framer always did: its length header says where the next
message starts.  A length past MAX_PAYLOAD, or that much text without a
NUL, cannot be resynchronised: feed() raises WireError, the framer stays
failed and the daemon drops the connection.  (Ported from the Codex
daemon's wire_command_framer.py, keeping driver-v2's skip policy.)
"""

import logging
import struct

from json_frame_codec import (HEADER_SIZE, JsonFrameError,
                              decode_payload as decode_json_payload)
from ubjson_codec import decode as decode_ubjson

log = logging.getLogger(__name__)

MAX_PAYLOAD = 4 * 1024 * 1024    # per message: a local resource bound
_MAGICS = (b"JSON", b"UBJS")


class WireError(ValueError):
    """The stream cannot be framed any further."""


class WireCommandFramer:
    def __init__(self):
        self.buf = b""
        self.failed = False

    def feed(self, data: bytes) -> list[tuple[str, object]]:
        if self.failed:
            raise WireError("wire stream already failed")
        self.buf += data
        messages: list[tuple[str, object]] = []
        try:
            while self.buf:
                if len(self.buf) < 4 and any(m.startswith(self.buf)
                                             for m in _MAGICS):
                    break               # maybe the start of a frame header
                magic = self.buf[:4]
                if magic in _MAGICS:
                    if len(self.buf) < HEADER_SIZE:
                        break
                    size = struct.unpack_from("<I", self.buf, 4)[0]
                    if size > MAX_PAYLOAD:
                        raise WireError(f"{magic.decode()} frame of {size} "
                                        "bytes exceeds the local bound")
                    end = HEADER_SIZE + size
                    if len(self.buf) < end:
                        break
                    payload, self.buf = self.buf[HEADER_SIZE:end], self.buf[end:]
                    msg = self._decode(magic, payload)
                    if msg is not None:
                        messages.append(msg)
                    continue
                end = self.buf.find(b"\0")
                if end < 0:
                    if len(self.buf) > MAX_PAYLOAD:
                        raise WireError("text command exceeds the local bound")
                    break
                raw, self.buf = self.buf[:end], self.buf[end + 1:]
                if raw:
                    messages.append(("text", raw.decode("utf-8",
                                                        errors="replace")))
        except WireError:
            self.failed = True
            self.buf = b""
            raise
        return messages

    @staticmethod
    def _decode(magic: bytes, payload: bytes):
        try:
            if magic == b"JSON":
                return "json", decode_json_payload(payload)
            value, _ = decode_ubjson(payload)
            return "ubjson", value
        except (JsonFrameError, ValueError, IndexError, struct.error,
                RecursionError, UnicodeError) as e:
            log.warning("Failed to decode %s frame (%d bytes): %s",
                        magic.decode(), len(payload), e)
            return None
