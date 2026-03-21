"""
UBJSON encoder/decoder for the UA Mixer Helper protocol.

The real UA Mixer Helper on port 4720 uses UBJSON (Universal Binary JSON)
with "UBJS" framing for TCP responses. This module provides byte-identical
encoding to match the real protocol.

UBJSON spec: https://ubjson.org/

Wire format:
    b"UBJS" + uint32_le(payload_len) + payload + b"\\x00\\x00"

Type markers:
    Z = null, T = true, F = false
    i = int8, U = uint8, I = int16, l = int32, L = int64
    D = float64 (big-endian IEEE 754)
    S = string (length-prefixed)
    { = object start, [ = array start
    # = count marker (optimized containers)

Container format (counted/optimized only):
    Object: {#<count> <key1><value1> <key2><value2> ...
    Array:  [#<count> <value1> <value2> ...

Keys: <length><string_bytes> (NO 'S' type prefix on keys)
Length/count: uses standard UBJSON integer encoding (i/U/I/l/L + value)
"""

import struct
from typing import Any


# ---------------------------------------------------------------------------
# Encoding: Python -> UBJSON bytes
# ---------------------------------------------------------------------------

def _encode_int_value(n: int) -> bytes:
    """Encode an integer using the smallest UBJSON integer type.

    Picks the tightest fit:
        i  int8    -128 .. 127
        U  uint8   0 .. 255
        I  int16   -32768 .. 32767
        l  int32   -2147483648 .. 2147483647
        L  int64   full range

    Note: UA uses LITTLE-endian for multi-byte integers (not big-endian
    like the UBJSON spec). Confirmed from frame headers (uint32_LE) and
    timeout_ms values (0xA861 = 25000 LE, not -22431 BE).
    """
    if -128 <= n <= 127:
        return b"i" + struct.pack("b", n)
    if 0 <= n <= 255:
        return b"U" + struct.pack("B", n)
    if -32768 <= n <= 32767:
        return b"I" + struct.pack("<h", n)
    if -2147483648 <= n <= 2147483647:
        return b"l" + struct.pack("<i", n)
    return b"L" + struct.pack("<q", n)


def _encode_length(n: int) -> bytes:
    """Encode a length or count value (used for string lengths and container counts).

    Same encoding as _encode_int_value -- lengths use the full UBJSON integer
    type selection.
    """
    return _encode_int_value(n)


def _encode_string_value(s: str) -> bytes:
    """Encode a string value: S + length + utf8 bytes."""
    raw = s.encode("utf-8")
    return b"S" + _encode_length(len(raw)) + raw


def _encode_key(s: str) -> bytes:
    """Encode an object key: length + utf8 bytes (no 'S' prefix).

    UBJSON object keys are NOT type-prefixed strings. They are just
    a UBJSON-encoded length followed by the raw string bytes.
    """
    raw = s.encode("utf-8")
    return _encode_length(len(raw)) + raw


def encode(value: Any) -> bytes:
    """Encode a Python value to UBJSON bytes.

    Supported types:
        None        -> Z (null)
        bool        -> T (true) / F (false)
        int         -> i/U/I/l/L (smallest fit)
        float       -> D (float64, big-endian)
        str         -> S + length + utf8
        list        -> [#<count> <values...>
        dict        -> {#<count> <key-value pairs...>

    Dict key ordering matches Python dict insertion order, which is
    critical for producing byte-identical output to the real UA Mixer
    Helper.
    """
    if value is None:
        return b"Z"

    if isinstance(value, bool):
        # Must check bool before int because bool is a subclass of int
        return b"T" if value else b"F"

    if isinstance(value, int):
        return _encode_int_value(value)

    if isinstance(value, float):
        return b"D" + struct.pack("<d", value)

    if isinstance(value, str):
        return _encode_string_value(value)

    if isinstance(value, list):
        parts = [b"[#", _encode_length(len(value))]
        for item in value:
            parts.append(encode(item))
        return b"".join(parts)

    if isinstance(value, dict):
        parts = [b"{#", _encode_length(len(value))]
        for k, v in value.items():
            parts.append(_encode_key(k))
            parts.append(encode(v))
        return b"".join(parts)

    raise TypeError(f"Cannot encode {type(value).__name__} to UBJSON")


# ---------------------------------------------------------------------------
# UBJS Framing
# ---------------------------------------------------------------------------

def frame(ubjson_bytes: bytes) -> bytes:
    """Wrap UBJSON payload in UBJS frame header.

    Wire format: b"UBJS" + uint32_le(payload_len) + payload

    The length is little-endian (confirmed from captures: 0x25000000 = 37
    in LE for the command_format response).
    """
    return b"UBJS" + struct.pack("<I", len(ubjson_bytes)) + ubjson_bytes


def encode_response(response_dict: dict) -> bytes:
    """Encode a response dict and wrap it in UBJS framing.

    This is the complete wire format for a single TCP response message
    from the UA Mixer Helper.

    Args:
        response_dict: The response to encode (e.g. {"path": "...", "data": ...})

    Returns:
        Complete UBJS-framed UBJSON bytes ready to send over TCP.
    """
    payload = encode(response_dict)
    return frame(payload)


# ---------------------------------------------------------------------------
# Decoding: UBJSON bytes -> Python
# ---------------------------------------------------------------------------

def _decode_int(data: bytes, pos: int, marker: int) -> tuple[Any, int]:
    """Decode a UBJSON integer given its type marker byte.

    Uses little-endian for multi-byte types (UA proprietary variant).
    """
    if marker == ord("i"):
        return struct.unpack_from("b", data, pos)[0], pos + 1
    if marker == ord("U"):
        return struct.unpack_from("B", data, pos)[0], pos + 1
    if marker == ord("I"):
        return struct.unpack_from("<h", data, pos)[0], pos + 2
    if marker == ord("l"):
        return struct.unpack_from("<i", data, pos)[0], pos + 4
    if marker == ord("L"):
        return struct.unpack_from("<q", data, pos)[0], pos + 8
    raise ValueError(f"Not an integer marker: {chr(marker)!r} (0x{marker:02x})")


def _is_int_marker(marker: int) -> bool:
    """Check if a byte is a UBJSON integer type marker."""
    return marker in (ord("i"), ord("U"), ord("I"), ord("l"), ord("L"))


def _decode_length(data: bytes, pos: int) -> tuple[int, int]:
    """Decode a UBJSON length/count value (integer without leading type check).

    Reads the type marker at pos and then the integer value.
    Returns (length, new_pos).
    """
    marker = data[pos]
    if not _is_int_marker(marker):
        raise ValueError(
            f"Expected integer marker for length at pos {pos}, "
            f"got {chr(marker)!r} (0x{marker:02x})"
        )
    return _decode_int(data, pos + 1, marker)


def _decode_typed_value(data: bytes, pos: int, type_marker: int) -> tuple[Any, int]:
    """Decode a value whose type marker is known (from strongly-typed containers).

    In [$<type>#<count>] containers, the type marker is specified once and
    omitted per-element. This function decodes a value given its pre-known type.
    """
    if type_marker == ord("Z"):
        return None, pos
    if type_marker == ord("T"):
        return True, pos
    if type_marker == ord("F"):
        return False, pos
    if _is_int_marker(type_marker):
        return _decode_int(data, pos, type_marker)
    if type_marker == ord("D"):
        return struct.unpack_from("<d", data, pos)[0], pos + 8
    if type_marker == ord("d"):
        return struct.unpack_from("<f", data, pos)[0], pos + 4
    if type_marker == ord("S"):
        length, pos = _decode_length(data, pos)
        s = data[pos:pos + length].decode("utf-8")
        return s, pos + length
    if type_marker == ord("{"):
        return _decode_object(data, pos)
    if type_marker == ord("["):
        return _decode_array(data, pos)
    raise ValueError(f"Unknown type marker in typed container: {chr(type_marker)!r} (0x{type_marker:02x})")


def decode(data: bytes, pos: int = 0) -> tuple[Any, int]:
    """Decode a UBJSON value starting at position pos.

    Args:
        data: Raw UBJSON bytes
        pos: Starting byte offset (default 0)

    Returns:
        Tuple of (decoded_value, new_position) where new_position is
        the byte offset immediately after the decoded value.

    Raises:
        ValueError: If the data contains invalid UBJSON
        IndexError: If the data is truncated
    """
    if pos >= len(data):
        raise ValueError(f"Unexpected end of data at pos {pos}")

    marker = data[pos]
    pos += 1

    # Null
    if marker == ord("Z"):
        return None, pos

    # Boolean
    if marker == ord("T"):
        return True, pos
    if marker == ord("F"):
        return False, pos

    # Integers
    if _is_int_marker(marker):
        return _decode_int(data, pos, marker)

    # Float64
    if marker == ord("D"):
        val = struct.unpack_from("<d", data, pos)[0]
        return val, pos + 8

    # Float32
    if marker == ord("d"):
        val = struct.unpack_from("<f", data, pos)[0]
        return val, pos + 4

    # String
    if marker == ord("S"):
        length, pos = _decode_length(data, pos)
        s = data[pos:pos + length].decode("utf-8")
        return s, pos + length

    # Object
    if marker == ord("{"):
        return _decode_object(data, pos)

    # Array
    if marker == ord("["):
        return _decode_array(data, pos)

    raise ValueError(f"Unknown UBJSON marker {chr(marker)!r} (0x{marker:02x}) at pos {pos - 1}")


def _decode_object(data: bytes, pos: int) -> tuple[dict, int]:
    """Decode a UBJSON object starting after the '{' marker.

    Supports formats:
        {#<n> key val ...           — counted
        {$<type>#<n> key val ...    — strongly-typed (all values same type)
        {... }                      — uncounted (read until '}')
    """
    if pos < len(data) and data[pos] == ord("$"):
        # Strongly-typed format: {$<type>#<count> key1 val1 ...
        pos += 1  # skip '$'
        value_type = data[pos]
        pos += 1  # skip type marker
        if pos < len(data) and data[pos] == ord("#"):
            pos += 1  # skip '#'
            count, pos = _decode_length(data, pos)
            result = {}
            for _ in range(count):
                key_len, pos = _decode_length(data, pos)
                key = data[pos:pos + key_len].decode("utf-8")
                pos += key_len
                val, pos = _decode_typed_value(data, pos, value_type)
                result[key] = val
            return result, pos
        raise ValueError(f"Expected '#' after '$<type>' in object at pos {pos}")

    if pos < len(data) and data[pos] == ord("#"):
        # Counted (optimized) format: {#<count> key1 val1 key2 val2 ...
        pos += 1  # skip '#'
        count, pos = _decode_length(data, pos)
        result = {}
        for _ in range(count):
            # Key: length + string bytes (no 'S' prefix)
            key_len, pos = _decode_length(data, pos)
            key = data[pos:pos + key_len].decode("utf-8")
            pos += key_len
            # Value: normal UBJSON value
            val, pos = decode(data, pos)
            result[key] = val
        return result, pos
    else:
        # Uncounted format: read until '}'
        result = {}
        while pos < len(data) and data[pos] != ord("}"):
            # Key: length + string bytes
            key_len, pos = _decode_length(data, pos)
            key = data[pos:pos + key_len].decode("utf-8")
            pos += key_len
            # Value
            val, pos = decode(data, pos)
            result[key] = val
        if pos < len(data) and data[pos] == ord("}"):
            pos += 1  # skip '}'
        return result, pos


def _decode_array(data: bytes, pos: int) -> tuple[list, int]:
    """Decode a UBJSON array starting after the '[' marker.

    Supports formats:
        [#<n> val val ...           — counted
        [$<type>#<n> val val ...    — strongly-typed (all elements same type)
        [... ]                      — uncounted (read until ']')
    """
    if pos < len(data) and data[pos] == ord("$"):
        # Strongly-typed format: [$<type>#<count> val1 val2 ...
        pos += 1  # skip '$'
        elem_type = data[pos]
        pos += 1  # skip type marker
        if pos < len(data) and data[pos] == ord("#"):
            pos += 1  # skip '#'
            count, pos = _decode_length(data, pos)
            result = []
            for _ in range(count):
                val, pos = _decode_typed_value(data, pos, elem_type)
                result.append(val)
            return result, pos
        raise ValueError(f"Expected '#' after '$<type>' in array at pos {pos}")

    if pos < len(data) and data[pos] == ord("#"):
        # Counted (optimized) format: [#<count> val1 val2 ...
        pos += 1  # skip '#'
        count, pos = _decode_length(data, pos)
        result = []
        for _ in range(count):
            val, pos = decode(data, pos)
            result.append(val)
        return result, pos
    else:
        # Uncounted format: read until ']'
        result = []
        while pos < len(data) and data[pos] != ord("]"):
            val, pos = decode(data, pos)
            result.append(val)
        if pos < len(data) and data[pos] == ord("]"):
            pos += 1  # skip ']'
        return result, pos


def decode_frame(data: bytes, pos: int = 0) -> tuple[Any, int]:
    """Decode a UBJS-framed message.

    Reads the "UBJS" magic + uint32_le length header, then decodes the
    UBJSON payload within.

    Args:
        data: Raw bytes starting with "UBJS" header
        pos: Starting offset

    Returns:
        Tuple of (decoded_value, new_position past the frame)

    Raises:
        ValueError: If the magic bytes are wrong or data is truncated
    """
    if data[pos:pos + 4] != b"UBJS":
        raise ValueError(f"Bad UBJS magic at pos {pos}: {data[pos:pos+4]!r}")
    payload_len = struct.unpack_from("<I", data, pos + 4)[0]
    payload_start = pos + 8
    payload_end = payload_start + payload_len
    if payload_end > len(data):
        raise ValueError(
            f"UBJS frame truncated: header says {payload_len} bytes "
            f"but only {len(data) - payload_start} available"
        )
    value, inner_pos = decode(data, payload_start)
    return value, payload_end


# ---------------------------------------------------------------------------
# UBJS Frame Accumulator (for receiving UBJSON commands from clients)
# ---------------------------------------------------------------------------

class UbjsonFramer:
    """Accumulates TCP data and yields complete UBJS-framed messages.

    After command_format 2, clients send UBJSON commands in UBJS frames:
        b"UBJS" + uint32_le(size) + UBJSON_payload

    The UBJSON payload is typically a dict like:
        {"cmd": "get", "url": "/ping"}
        {"cmd": "subscribe", "url": "/initialized", "parameters": {"func_id": 4}}
        {"cmd": "set", "url": "/Sleep", "value": false}
    """

    HEADER_SIZE = 8  # 4 bytes magic + 4 bytes length

    def __init__(self):
        self.buf = b""

    def feed(self, data: bytes) -> list[dict]:
        """Feed raw TCP bytes, return list of complete decoded UBJSON dicts."""
        self.buf += data
        messages = []
        while len(self.buf) >= self.HEADER_SIZE:
            # Check for UBJS magic
            if self.buf[:4] != b"UBJS":
                # Skip non-UBJS data (shouldn't happen, but be safe)
                # Look for next UBJS magic
                idx = self.buf.find(b"UBJS", 1)
                if idx == -1:
                    self.buf = b""  # discard all
                    break
                self.buf = self.buf[idx:]
                continue

            # Read payload length (uint32 LE)
            payload_len = struct.unpack_from("<I", self.buf, 4)[0]
            total_len = self.HEADER_SIZE + payload_len

            if len(self.buf) < total_len:
                break  # incomplete frame, wait for more data

            # Extract and decode the payload
            payload = self.buf[self.HEADER_SIZE:total_len]
            self.buf = self.buf[total_len:]

            try:
                value, _ = decode(payload)
                if isinstance(value, dict):
                    messages.append(value)
                else:
                    messages.append({"_raw": value})
            except (ValueError, IndexError, struct.error) as e:
                # Corrupted frame — skip it
                import logging
                logging.getLogger(__name__).warning(
                    "Failed to decode UBJSON frame (%d bytes): %s", payload_len, e)

        return messages


# ---------------------------------------------------------------------------
# Tests
# ---------------------------------------------------------------------------

if __name__ == "__main__":
    import sys

    passed = 0
    failed = 0

    def check(name: str, got: bytes, expected: bytes):
        global passed, failed
        if got == expected:
            print(f"  PASS: {name}")
            passed += 1
        else:
            print(f"  FAIL: {name}")
            print(f"    expected: {expected.hex(' ')}")
            print(f"    got:      {got.hex(' ')}")
            # Find first difference
            for i in range(min(len(got), len(expected))):
                if got[i] != expected[i]:
                    print(f"    first diff at byte {i}: "
                          f"expected 0x{expected[i]:02x}, got 0x{got[i]:02x}")
                    break
            if len(got) != len(expected):
                print(f"    length: expected {len(expected)}, got {len(got)}")
            failed += 1

    # --- Test 1: command_format response ---
    print("Test 1: command_format response encoding")
    cmd_fmt = {"path": "command_format", "data": {}}
    cmd_fmt_expected = bytes.fromhex(
        "7b 23 69 02 69 04 70 61 74 68 53 69 0e 63 6f 6d"
        "6d 61 6e 64 5f 66 6f 72 6d 61 74 69 04 64 61 74"
        "61 7b 23 69 00".replace(" ", "")
    )
    cmd_fmt_encoded = encode(cmd_fmt)
    check("encode", cmd_fmt_encoded, cmd_fmt_expected)

    # --- Test 2: initialized response ---
    print("Test 2: initialized response encoding")
    init_resp = {"path": "/initialized", "parameters": {"message_id": 1}, "data": True}
    init_expected = bytes.fromhex(
        "7b 23 69 03 69 04 70 61 74 68 53 69 0c 2f 69 6e"
        "69 74 69 61 6c 69 7a 65 64 69 0a 70 61 72 61 6d"
        "65 74 65 72 73 7b 23 69 01 69 0a 6d 65 73 73 61"
        "67 65 5f 69 64 69 01 69 04 64 61 74 61 54".replace(" ", "")
    )
    init_encoded = encode(init_resp)
    check("encode", init_encoded, init_expected)

    # --- Test 3: round-trip encode -> decode ---
    print("Test 3: round-trip encode -> decode")
    test_values = [
        None,
        True,
        False,
        0,
        1,
        -1,
        127,
        128,      # crosses into uint8
        255,
        256,      # crosses into int16
        -129,     # crosses into int16
        32767,
        32768,    # crosses into int32
        1.5,
        -0.0,
        "",
        "hello",
        "command_format",
        [],
        [1, 2, 3],
        {},
        {"a": 1},
        {"path": "/test", "data": {"nested": [1, "two", True, None, 3.14]}},
        {"path": "command_format", "data": {}},
        {"path": "/initialized", "parameters": {"message_id": 1}, "data": True},
    ]

    all_rt_ok = True
    for val in test_values:
        encoded = encode(val)
        decoded, end_pos = decode(encoded)
        if decoded != val:
            # Special case: -0.0 == 0.0 in Python but they are different floats
            if isinstance(val, float) and val == decoded:
                pass
            else:
                print(f"  FAIL round-trip: {val!r} -> {encoded.hex(' ')} -> {decoded!r}")
                all_rt_ok = False
        if end_pos != len(encoded):
            print(f"  FAIL round-trip pos: {val!r}: consumed {end_pos}/{len(encoded)} bytes")
            all_rt_ok = False

    if all_rt_ok:
        print("  PASS: all round-trips")
        passed += 1
    else:
        failed += 1

    # --- Test 4: UBJS framing ---
    print("Test 4: UBJS framing")
    framed = frame(cmd_fmt_encoded)
    # Payload is 37 bytes (0x25), LE -> 25 00 00 00
    expected_header = b"UBJS" + struct.pack("<I", 37)
    check("frame header", framed[:8], expected_header)
    check("frame payload", framed[8:], cmd_fmt_encoded)

    # Also verify the full framed output matches capture
    # Capture shows: 55 42 4a 53 25 00 (only 6 bytes shown due to capture truncation)
    # Full header is 8 bytes: UBJS + 25 00 00 00
    check("frame magic+len", framed[:6], bytes.fromhex("55 42 4a 53 25 00".replace(" ", "")))

    # --- Test 5: encode_response convenience ---
    print("Test 5: encode_response convenience function")
    resp_bytes = encode_response(cmd_fmt)
    check("encode_response", resp_bytes, framed)

    # --- Test 6: decode_frame ---
    print("Test 6: decode_frame")
    decoded_from_frame, frame_end = decode_frame(framed)
    if decoded_from_frame == cmd_fmt and frame_end == len(framed):
        print("  PASS: decode_frame round-trip")
        passed += 1
    else:
        print(f"  FAIL: decode_frame: {decoded_from_frame!r}, end={frame_end}")
        failed += 1

    # --- Test 7: integer type selection (little-endian for multi-byte) ---
    print("Test 7: integer type selection")
    check("int8(0)",    _encode_int_value(0),      b"i\x00")
    check("int8(127)",  _encode_int_value(127),    b"i\x7f")
    check("int8(-128)", _encode_int_value(-128),   b"i\x80")
    check("uint8(128)", _encode_int_value(128),    b"U\x80")
    check("uint8(255)", _encode_int_value(255),    b"U\xff")
    check("int16(256)", _encode_int_value(256),    b"I\x00\x01")       # LE
    check("int16(-129)",_encode_int_value(-129),   b"I\x7f\xff")       # LE
    check("int32(32768)", _encode_int_value(32768), b"l\x00\x80\x00\x00")  # LE
    check("int16(25000)", _encode_int_value(25000), b"I\xa8\x61")      # LE (timeout_ms)

    # --- Test 8: decode the captured initialized response ---
    print("Test 8: decode captured initialized response")
    captured_init = bytes.fromhex(
        "7b 23 69 03 69 04 70 61 74 68 53 69 0c 2f 69 6e"
        "69 74 69 61 6c 69 7a 65 64 69 0a 70 61 72 61 6d"
        "65 74 65 72 73 7b 23 69 01 69 0a 6d 65 73 73 61"
        "67 65 5f 69 64 69 01 69 04 64 61 74 61 54".replace(" ", "")
    )
    decoded_init, _ = decode(captured_init)
    expected_init = {"path": "/initialized", "parameters": {"message_id": 1}, "data": True}
    if decoded_init == expected_init:
        print("  PASS: decoded captured bytes match expected dict")
        passed += 1
    else:
        print(f"  FAIL: {decoded_init!r} != {expected_init!r}")
        failed += 1

    # --- Summary ---
    print(f"\n{'='*60}")
    print(f"Results: {passed} passed, {failed} failed")
    if failed:
        sys.exit(1)
    else:
        print("All tests passed.")
