"""TCP:4710 text protocol: framing, command parsing, JSON responses."""

import json

import pytest

from protocol import (MessageFramer, encode_error_bytes, encode_response_bytes,
                      parse_command, parse_value)


class TestFraming:
    def test_splits_on_nul(self):
        f = MessageFramer()
        assert f.feed(b"get /ping\x00set /a 1\x00") == ["get /ping", "set /a 1"]

    def test_partial_message_is_buffered(self):
        f = MessageFramer()
        assert f.feed(b"get /pi") == []
        assert f.feed(b"ng\x00sub") == ["get /ping"]
        assert f.feed(b"scribe /x\x00") == ["subscribe /x"]

    def test_empty_messages_dropped(self):
        assert MessageFramer().feed(b"\x00\x00get /a\x00\x00") == ["get /a"]

    def test_invalid_utf8_is_replaced_not_raised(self):
        assert MessageFramer().feed(b"set /a \xff\x00") == ["set /a �"]


class TestParseValue:
    def test_types(self):
        assert parse_value("true") is True
        assert parse_value("false") is False
        assert parse_value("42") == 42 and isinstance(parse_value("42"), int)
        assert parse_value("-3.5") == -3.5
        assert parse_value('"Internal"') == "Internal"
        assert parse_value("S/PDIF") == "S/PDIF"


class TestParseCommand:
    def test_get_plain(self):
        c = parse_command("get /devices/0/inputs/0/preamps/0/Gain/value")
        assert (c.verb, c.path) == ("get", "/devices/0/inputs/0/preamps/0/Gain/value")
        assert not c.recursive and c.propfilter == []

    def test_get_recursive_with_params(self):
        c = parse_command("get /devices/0?recursive=1&propfilter=type,min&message_id=7")
        assert c.path == "/devices/0"
        assert c.recursive
        assert c.propfilter == ["type", "min"]
        assert c.raw_params["message_id"] == "7"

    def test_get_bare_recursive_flag(self):
        assert parse_command("get /x?recursive").recursive

    @pytest.mark.xfail(strict=True, reason=(
        "bug: protocol._parse_path_with_params sets recursive=True for "
        "'recursive=0' because its `\"recursive=\" in query_str` substring "
        "check overrides the earlier '!= \"0\"' test"))
    def test_get_recursive_zero_is_false(self):
        assert not parse_command("get /x?recursive=0").recursive

    def test_set_values(self):
        c = parse_command("set /devices/0/inputs/0/preamps/0/Gain/value 42.5")
        assert (c.verb, c.value) == ("set", 42.5)
        assert parse_command("set /TalkbackOn/value true").value is True
        assert parse_command('set /ClockSource/value "S/PDIF"').value == "S/PDIF"

    def test_set_with_params(self):
        c = parse_command("set /SampleRate/value?func_id=4 96000")
        assert c.path == "/SampleRate/value"
        assert c.value == 96000
        assert c.raw_params == {"func_id": "4"}

    def test_set_without_value(self):
        assert parse_command("set /x").value is None

    def test_set_without_path(self):
        assert parse_command("set") is None

    def test_subscribe_unsubscribe(self):
        c = parse_command("subscribe /devices/0/inputs?recursive=1&message_id=3")
        assert c.verb == "subscribe" and c.recursive
        assert c.path == "/devices/0/inputs"
        assert parse_command("unsubscribe /a").verb == "unsubscribe"
        assert parse_command("subscribe") is None

    def test_batch_subscribe_keeps_json_body(self):
        body = '{"paths":["/a","/b?x=1"]}'
        c = parse_command("subscribe " + body)
        assert c.path == body

    def test_subscribe_mode_multi(self):
        assert parse_command("subscribe mode=multi").path == "mode=multi"

    def test_post(self):
        c = parse_command("post /Something?func_id=9 12")
        assert (c.verb, c.path, c.value) == ("post", "/Something", 12)
        assert c.raw_params == {"func_id": "9"}

    def test_verb_case_insensitive(self):
        assert parse_command("GET /ping").verb == "get"

    def test_json_identification(self):
        c = parse_command('{"path":"networkID","data":"abc"}')
        assert (c.verb, c.path, c.value) == ("json", "networkID", "abc")

    def test_bad_json(self):
        assert parse_command("{not json") is None

    def test_unknown_verb_and_empty(self):
        assert parse_command("frobnicate /a") is None
        assert parse_command("   ") is None

    def test_surrounding_whitespace(self):
        assert parse_command("  get /ping \r\n").path == "/ping"


class TestResponses:
    def test_response_is_nul_terminated_compact_json(self):
        b = encode_response_bytes("/ping", True)
        assert b.endswith(b"\x00") and b.count(b"\x00") == 1
        assert b[:-1] == b'{"path":"/ping","data":true}'

    def test_response_with_params(self):
        msg = json.loads(encode_response_bytes("/a", 1, {"message_id": 5})[:-1])
        assert msg == {"path": "/a", "data": 1, "parameters": {"message_id": 5}}

    def test_response_unicode_round_trip(self):
        msg = json.loads(encode_response_bytes("/n", "Ünï")[:-1].decode())
        assert msg["data"] == "Ünï"

    def test_error_format(self):
        msg = json.loads(encode_error_bytes("/nope", "get")[:-1])
        assert msg == {"path": "/nope", "error": "Unable to resolve path for get."}

    def test_framer_round_trip(self):
        out = encode_response_bytes("/a", 1) + encode_response_bytes("/b", None)
        got = [json.loads(m) for m in MessageFramer().feed(out)]
        assert [g["path"] for g in got] == ["/a", "/b"]
