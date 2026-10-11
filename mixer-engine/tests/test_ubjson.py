"""UBJSON codec and UBJS framing (the 4720 Mixer Helper wire format)."""

import struct

import pytest

import ubjson_codec as u


@pytest.mark.parametrize("value, expected", [
    (None, b"Z"),
    (True, b"T"),
    (False, b"F"),
    (5, b"i\x05"),
    (-128, b"i\x80"),
    (200, b"U\xc8"),
    (1000, b"I" + struct.pack("<h", 1000)),      # little-endian, unlike the spec
    (100000, b"l" + struct.pack("<i", 100000)),
    (2**40, b"L" + struct.pack("<q", 2**40)),
    (1.5, b"D" + struct.pack("<d", 1.5)),
    ("hi", b"Si\x02hi"),
    ([], b"[#i\x00"),
    ({"a": 1}, b"{#i\x01i\x01ai\x01"),
])
def test_encode_exact_bytes(value, expected):
    assert u.encode(value) == expected


def test_bool_is_not_encoded_as_int():
    assert u.encode(True) == b"T" and u.encode(1) == b"i\x01"


def test_unsupported_type():
    with pytest.raises(TypeError):
        u.encode(object())


@pytest.mark.parametrize("value", [
    None, True, False, 0, 127, 128, 255, 256, -129, 70000, -70000, 2**33,
    0.25, -1e300, "", "Ünï/code", [1, "a", None], {"k": [1, 2.5, {"z": False}]},
    {"path": "/x", "data": 3, "parameters": {"func_id": 4}},
])
def test_round_trip(value):
    data = u.encode(value)
    decoded, pos = u.decode(data)
    assert decoded == value
    assert pos == len(data)


def test_long_string_uses_wider_length():
    s = "x" * 300
    assert u.encode(s)[:4] == b"SI" + struct.pack("<h", 300)
    assert u.decode(u.encode(s))[0] == s


def test_frame_header():
    assert u.frame(b"Z") == b"UBJS" + struct.pack("<I", 1) + b"Z"


def test_encode_response_and_decode_frame():
    resp = {"path": "/ping", "data": True}
    framed = u.encode_response(resp)
    assert framed[:4] == b"UBJS"
    assert struct.unpack_from("<I", framed, 4)[0] == len(framed) - 8
    value, end = u.decode_frame(framed)
    assert value == resp and end == len(framed)


def test_decode_frame_errors():
    with pytest.raises(ValueError):
        u.decode_frame(b"XXXX\x00\x00\x00\x00")
    with pytest.raises(ValueError):
        u.decode_frame(b"UBJS" + struct.pack("<I", 50) + b"Z")


class TestFramer:
    CMD = {"cmd": "get", "url": "/ping"}

    def test_single_frame(self):
        assert u.UbjsonFramer().feed(u.encode_response(self.CMD)) == [self.CMD]

    def test_split_across_feeds(self):
        data = u.encode_response(self.CMD)
        f = u.UbjsonFramer()
        assert f.feed(data[:5]) == []
        assert f.feed(data[5:-1]) == []
        assert f.feed(data[-1:]) == [self.CMD]

    def test_two_frames_in_one_feed(self):
        other = {"cmd": "set", "url": "/Sleep", "value": False}
        data = u.encode_response(self.CMD) + u.encode_response(other)
        assert u.UbjsonFramer().feed(data) == [self.CMD, other]

    def test_garbage_before_magic_is_skipped(self):
        data = b"junk" + u.encode_response(self.CMD)
        assert u.UbjsonFramer().feed(data) == [self.CMD]

    def test_non_dict_payload_wrapped(self):
        assert u.UbjsonFramer().feed(u.frame(u.encode(7))) == [{"_raw": 7}]

    def test_corrupt_payload_skipped_then_recovers(self):
        bad = u.frame(b"\xff\xff")
        out = u.UbjsonFramer().feed(bad + u.encode_response(self.CMD))
        assert out == [self.CMD]
