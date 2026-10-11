"""TCP input framing and command_format 1/2 (ported from the Codex daemon's
test_json_frame_codec.py and test_wire_negotiation.py).

Text commands, UBJS frames and JSON frames arrive in any order and split
anywhere; command_format 1 answers in JSON frames, 2 in UBJS frames.  An
undecodable frame or a malformed command envelope is skipped (driver-v2's
policy), an oversize length drops the connection.
"""

import json
import math
import struct

import pytest

from json_frame_codec import JsonFrameError, decode_frame, encode_frame
from ua_mixer_daemon import MixerClient, MixerDaemon
from ubjson_codec import decode_frame as decode_ubjson
from ubjson_codec import encode as ubjson_encode
from ubjson_codec import encode_response as encode_ubjson
from ubjson_codec import frame as ubjson_frame
from wire_command_framer import MAX_PAYLOAD, WireCommandFramer, WireError


class MemoryWriter:
    def __init__(self):
        self.frames = []

    def get_extra_info(self, name):
        return ("127.0.0.1", 4710)

    def write(self, data):
        self.frames.append(data)


def raw_json(body):
    return b"JSON" + struct.pack("<I", len(body)) + body


@pytest.fixture
def conn(tree):
    daemon = MixerDaemon(tree, None)
    writer = MemoryWriter()
    return daemon, MixerClient(None, writer), writer


# ── JSON frame codec ────────────────────────────────────────────────

def test_json_frame_counts_one_terminal_nul():
    frame = encode_frame({"path": "/é", "data": [1, True, None]})
    assert frame[:4] == b"JSON"
    assert struct.unpack_from("<I", frame, 4)[0] == len(frame) - 8
    assert frame[-2:] == b"}\0"
    assert decode_frame(frame)["path"] == "/é"


@pytest.mark.parametrize("body", [
    b"{}", b"{}\0\0", b"{}\0x", b"[]\0", b'{"a":1,"a":2}\0', b'{"a":NaN}\0',
    b'{"a":1e999}\0', b'{"a":"\xff"}\0'])
def test_json_frame_payload_validation(body):
    with pytest.raises(JsonFrameError):
        decode_frame(raw_json(body))


def test_json_frame_encode_validation():
    assert decode_frame(raw_json(b'{"a":"\\u0000"}\0')) == {"a": "\0"}
    for value in ([], {"a": math.nan}, {"a": object()}, {"a": "\ud800"}):
        with pytest.raises(JsonFrameError):
            encode_frame(value)
    for frame in (b"JSON", encode_frame({})[:-1], encode_frame({}) + b"x"):
        with pytest.raises(JsonFrameError):
            decode_frame(frame)


# ── Framer ──────────────────────────────────────────────────────────

MIXED = (b"post command_format 1\0" + encode_frame({"cmd": "get", "url": "/ping"})
         + encode_ubjson({"cmd": "get", "url": "/Sleep"}) + b"get /x\0")
EXPECTED = [("text", "post command_format 1"),
            ("json", {"cmd": "get", "url": "/ping"}),
            ("ubjson", {"cmd": "get", "url": "/Sleep"}),
            ("text", "get /x")]


def test_every_split_of_mixed_input():
    for split in range(len(MIXED) + 1):
        f = WireCommandFramer()
        assert f.feed(MIXED[:split]) + f.feed(MIXED[split:]) == EXPECTED
        assert f.buf == b""


def test_bytewise_and_coalesced():
    f = WireCommandFramer()
    out = []
    for b in MIXED:
        out += f.feed(bytes([b]))
    assert out == EXPECTED
    assert WireCommandFramer().feed(MIXED * 50) == EXPECTED * 50


def test_text_after_ubjson_frames_is_not_lost():
    """The old UBJSON framer skipped text that followed a UBJS frame."""
    data = encode_ubjson({"cmd": "get", "url": "/a"}) + b"set /Sleep false\0"
    assert WireCommandFramer().feed(data) == [
        ("ubjson", {"cmd": "get", "url": "/a"}), ("text", "set /Sleep false")]


def test_undecodable_frame_is_skipped(caplog):
    good = encode_ubjson({"cmd": "get", "url": "/a"})
    bad_ubjs = b"UBJS" + struct.pack("<I", 1) + b"\xff"
    bad_json = raw_json(b"[]\0")
    f = WireCommandFramer()
    assert f.feed(bad_ubjs + bad_json + good) == [("ubjson", {"cmd": "get",
                                                              "url": "/a"})]
    assert "Failed to decode" in caplog.text
    assert f.feed(b"get /b\0") == [("text", "get /b")]


def test_oversize_length_fails_the_stream():
    f = WireCommandFramer()
    with pytest.raises(WireError):
        f.feed(b"get /a\0JSON" + struct.pack("<I", MAX_PAYLOAD + 1))
    with pytest.raises(WireError):
        f.feed(b"get /b\0")             # stays failed


def test_unterminated_text_is_bounded():
    f = WireCommandFramer()
    assert f.feed(b"x" * 1000) == []
    with pytest.raises(WireError):
        f.feed(b"x" * MAX_PAYLOAD)


# ── Daemon negotiation ──────────────────────────────────────────────

def test_command_format_1_answers_in_json_frames(conn):
    daemon, client, writer = conn
    daemon._feed_client_data(client, b"post command_format?func_id=8 1\0"
                             b"get /ping\0"
                             + encode_frame({"cmd": "get", "url": "/ping"}))
    replies = [decode_frame(f) for f in writer.frames]
    assert replies[0] == {"path": "command_format", "data": {},
                          "parameters": {"func_id": 8}}
    assert replies[1:] == [{"path": "/ping", "data": True}] * 2
    assert client.json_framed and not client.ubjson


def test_command_format_2_still_ubjson(conn):
    daemon, client, writer = conn
    daemon._feed_client_data(client, b"post command_format?func_id=2 2\0")
    assert decode_ubjson(writer.frames[0])[0] == {
        "path": "command_format", "data": {}, "parameters": {"func_id": 2}}
    assert client.ubjson and not client.json_framed


def test_switch_text_json_ubjson(conn):
    daemon, client, writer = conn
    daemon._feed_client_data(client, b"get /ping\0")
    assert json.loads(writer.frames.pop().rstrip(b"\0")) == {
        "path": "/ping", "data": True}
    daemon._feed_client_data(
        client, b"post command_format 1\0"
        + encode_frame({"cmd": "post", "url": "command_format", "data": 2})
        + encode_ubjson({"cmd": "get", "url": "/ping"}))
    assert decode_frame(writer.frames[0])["path"] == "command_format"
    assert decode_ubjson(writer.frames[1])[0]["path"] == "command_format"
    assert decode_ubjson(writer.frames[2])[0] == {"path": "/ping",
                                                  "data": True}
    assert client.ubjson and not client.json_framed


def test_ubjs_frame_auto_detects_ubjson(conn):
    daemon, client, writer = conn
    daemon._feed_client_data(client, encode_ubjson({"cmd": "get",
                                                    "url": "/ping"}))
    assert client.ubjson
    assert decode_ubjson(writer.frames[0])[0] == {"path": "/ping",
                                                  "data": True}


def test_subscription_pushes_follow_the_json_framing(conn):
    daemon, client, writer = conn
    path = "/devices/0/inputs/0/FaderLevel/value"
    daemon.state.register_callback(
        client.id, lambda p, v: daemon._send_response(client, p, v))
    daemon.state.subscribe(client.id, path)
    daemon._feed_client_data(client, b"post command_format 1\0")
    writer.frames.clear()
    daemon.state.set(path, -6.0)
    assert decode_frame(writer.frames[0]) == {"path": path, "data": -6.0}


@pytest.mark.parametrize("bad", [
    {"cmd": 1, "url": "/ping"}, {"cmd": "get", "url": 1},
    {"cmd": "get", "url": "/ping", "parameters": []}, {"path": 1}, 7])
def test_malformed_envelope_is_skipped(conn, bad, caplog):
    """Codex rejected the whole batch and closed the connection; driver-v2
    keeps its policy for an unparseable command (log, skip, carry on).
    A wrong-typed cmd used to raise out of the handler."""
    daemon, client, writer = conn
    for frame in ([ubjson_frame(ubjson_encode(bad))] +
                  ([encode_frame(bad)] if isinstance(bad, dict) else [])):
        daemon._feed_client_data(client, b"get /ping\0" + frame)
    daemon._feed_client_data(client, b"get /ping\0")
    n = 3 if isinstance(bad, dict) else 2
    assert [json.loads(f.rstrip(b"\0")) for f in writer.frames] == [
        {"path": "/ping", "data": True}] * n
    assert "Unparseable" in caplog.text
    assert not client.json_framed and not client.ubjson


def test_oversize_frame_raises_for_the_handler(conn):
    daemon, client, writer = conn
    with pytest.raises(WireError):
        daemon._feed_client_data(
            client, b"get /ping\0JSON" + struct.pack("<I", 0xFFFFFFFF))
    # the valid command before the bad header is answered first
    assert [json.loads(f.rstrip(b"\0")) for f in writer.frames] == [
        {"path": "/ping", "data": True}]


def test_wire_error_carries_the_commands_parsed_before_it():
    f = WireCommandFramer()
    with pytest.raises(WireError) as ei:
        f.feed(b"get /ping\0JSON" + struct.pack("<I", MAX_PAYLOAD + 1))
    assert ei.value.messages == [("text", "get /ping")]
