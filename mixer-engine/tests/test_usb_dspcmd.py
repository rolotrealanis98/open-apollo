"""J1: the DSP command language and the Solo USB init generator, checked
against tools/usb-re/init-bulk-sequence.bin (a Windows capture of an Apollo
Solo USB) byte for byte.  No hardware."""

import struct
import sys
from datetime import datetime, timezone

import pytest

from conftest import ENGINE_DIR

TOOLS = ENGINE_DIR.parent / "tools"
if str(TOOLS) not in sys.path:
    sys.path.append(str(TOOLS))

from uausb import dspcmd as dc  # noqa: E402
from uausb import soloinit as si  # noqa: E402

CAPTURE = TOOLS / "usb-re" / "init-bulk-sequence.bin"


@pytest.fixture(scope="module")
def urbs():
    return dc.read_urb_file(CAPTURE)


@pytest.fixture(scope="module")
def stream(urbs):
    return dc.InitStream.from_urbs(urbs)


@pytest.fixture(scope="module")
def template():
    return si.Template.from_file(CAPTURE)


# ── decode / re-encode ──────────────────────────────────────────────────

def test_capture_decodes_into_commands(stream):
    assert len(stream.frames) == 38
    assert stream.seq0 == 255 and stream.flags == 0
    names = {}
    for c in stream.commands:
        names[c.name] = names.get(c.name, 0) + 1
    assert names == {"SYNCH": 12, "INPUT_PARAM": 48, "RELEASE": 5,
                     "LOAD_CODE": 5, "ZERO_FILL": 36, "MEMSPEC": 6,
                     "WRITE_BLOCK": 16}
    assert len(stream.commands) == 128
    # every frame is a 0xDC frame with consecutive sequence numbers
    assert [f.seq for f in stream.frames] == [(255 + i) & 0xFF for i in range(38)]
    assert all(f.magic == dc.FRAME_MAGIC_OUT for f in stream.frames)


def test_capture_command_details(stream):
    c = stream.commands
    assert c[0] == dc.synch(0x00, 0x20, 0, 0)
    assert c[1] == dc.input_param(0, 8, 0x20)
    assert c[3] == dc.input_param(0, 7, 0x1FFFF)
    loads = [x for x in c if x.op == dc.OP_LOAD_CODE]
    assert [(x.args[0], x.args[2] & 0xFFFF, x.length) for x in loads] == [
        (0xE0000, 0xA5, 487), (0xE0130, 0xC2, 48), (0xE0146, 0xEB, 181),
        (0xE01B2, 0xDA, 763), (0xE03A2, 0x12B, 115)]
    # Bill header as transmitted: arch byte 0, word 2 = 0
    for x in loads:
        assert x.args[1] == dc.BILL_MAGIC
        assert x.args[2] >> 24 == 0 and x.args[3] == 0
        assert x.length == 2 + (x.args[4] + 0x14) // 4
    # code loads that span frames: A5 (2 frames) and DA (3 frames)
    starts = {x.args[2] & 0xFFFF: s for x, s in zip(c, stream.starts)
              if x.op == dc.OP_LOAD_CODE}
    assert starts == {0xA5: 10, 0xC2: 12, 0xEB: 20, 0xDA: 21, 0x12B: 24}


def test_capture_reencodes_byte_identically(stream, urbs):
    assert stream.to_urbs() == urbs
    assert dc.build_urb_container(stream.to_urbs()) == CAPTURE.read_bytes()


def test_frames_and_container_roundtrip(urbs):
    for u in urbs:
        assert dc.Frame.from_bytes(u).to_bytes() == u
    assert dc.parse_urb_container(dc.build_urb_container(urbs)) == urbs


# ── the generator ───────────────────────────────────────────────────────

def test_template_takes_blobs_and_params_from_capture(template):
    assert sorted(template.blobs) == [0xA5, 0xC2, 0xDA, 0xEB, 0x12B]
    assert {k: [(i, o, len(w)) for i, o, w in v]
            for k, v in template.params.items()} == {
        "input": [(1, 0, 9)],
        "mic": [(1, 0, 18), (1, 0x26, 16), (4, 1, 15)],
        "aux": [(1, 0, 5)],
    }


def test_memory_map_tops_match_capture():
    assert si.SOLO_USB_MEMORY_MAP.tops() == (0x9D1B8, 0xC6FFE, 0x08FFE000)


def test_generator_reproduces_every_command(template, stream):
    txs = si.build_transactions(template)
    gen = [c for tx in txs for c in tx]
    assert gen == stream.commands


def test_generator_frames_byte_identical_to_capture(template, urbs):
    plan = si.build_init(template, rate=48000, rate_ack=False)
    assert plan.capture_framing and plan.preamble == 0
    assert plan.urbs() == urbs
    assert dc.build_urb_container(plan.urbs()) == CAPTURE.read_bytes()


def test_rate_ack_frame_comes_first(template, urbs):
    plan = si.build_init(template)          # rate_ack default on, 48 kHz
    out = plan.urbs()
    assert out[0] == bytes((2, 0, 254, 0xDC)) + struct.pack("<II", 0x001A0002, 48000)
    assert out[1:] == urbs


def test_natural_framing_carries_same_stream(template, stream):
    plan = si.build_init(template, rate_ack=False, capture_framing=False)
    assert not plan.capture_framing
    assert all(len(f.words) <= 255 for f in plan.frames)
    assert dc.decode_commands(dc.deframe(plan.frames)) == stream.commands
    # one transaction per frame start: 9 teardown groups first
    assert [len(f.words) for f in plan.frames[:3]] == [16, 16, 6]


def test_connect_preamble(template, urbs):
    plan = si.build_init(template, connect_date=0x01B71448, query_tag=0x0AE52420)
    out = plan.urbs()
    # the two connect frames of the Windows bring-up (apollo-init.pcap)
    assert out[0][4:] == bytes.fromhex("0200230001000000020010004814b701")
    assert out[1][4:] == bytes.fromhex("02001000" "4814b701" "02001100" "2024e50a")
    assert [u[2] for u in out[:3]] == [252, 253, 254]
    assert out[3:] == urbs


def test_mic_ramp_rule_gives_captured_words():
    assert si.mic_ramp_words(48000) == (75, 0x41000000, 0x3ADA740E)
    k, f, inv = si.mic_ramp_words(44100)
    assert k == 69 and f == 0x41000000
    k, f, _ = si.mic_ramp_words(192000)
    assert k == 150 and f == 0x41800000     # block size 64 above 100 kHz


def test_other_rate_changes_rate_ack_and_mic_ramp_only(template, stream):
    plan = si.build_init(template, rate=96000)
    gen = plan.commands()
    assert gen[0] == dc.rate_ack(96000)
    body = gen[1:]
    assert len(body) == len(stream.commands)
    diffs = [(i, a, b) for i, (a, b) in enumerate(zip(body, stream.commands))
             if a != b]
    assert len(diffs) == 2                  # the two Mic strips' first block
    k, f, inv = si.mic_ramp_words(96000)
    for _i, a, b in diffs:
        assert a.op == dc.OP_WRITE_BLOCK
        assert a.args[0] == b.args[0]
        changed = [j for j, (x, y) in enumerate(zip(a.args, b.args)) if x != y]
        assert changed and set(changed) <= {12, 13, 14}
        assert a.args[12:15] == (k, f, inv)
    # same length: the capture's framing still applies
    assert plan.capture_framing


def test_code_placement_follows_pool_rule(template):
    pools = si.Pools(si.SOLO_USB_MEMORY_MAP)
    dests = [pools.code(si.code_dwords(template.blob(m)))
             for m in (0xA5, 0xC2, 0xEB, 0xDA, 0x12B)]
    assert dests == [0xE0000, 0xE0130, 0xE0146, 0xE01B2, 0xE03A2]


def test_missing_module_or_params_is_an_error(template):
    t = si.Template(dict(template.blobs), dict(template.params))
    del t.blobs[0xDA]
    with pytest.raises(si.TemplateError):
        si.build_transactions(t)
    t = si.Template(dict(template.blobs), {"input": template.params["input"]})
    with pytest.raises(si.TemplateError):
        si.build_transactions(t)


# ── framing and command edge cases ──────────────────────────────────────

def test_frame_stream_splits_and_wraps_seq():
    words = list(range(600))
    frames = dc.frame_stream(words, seq0=254)
    assert [len(f.words) for f in frames] == [255, 255, 90]
    assert [f.seq for f in frames] == [254, 255, 0]
    assert dc.deframe(frames) == words


def test_frame_flags_byte_roundtrip():
    f = dc.Frame([1, 2], seq=7, flags=0x10)
    raw = f.to_bytes()
    assert raw[:4] == bytes((2, 0x10, 7, 0xDC))
    g = dc.Frame.from_bytes(raw)
    assert (g.words, g.seq, g.flags) == ([1, 2], 7, 0x10)


def test_frame_errors():
    with pytest.raises(dc.DspCommandError):
        dc.Frame(list(range(256))).to_bytes()
    with pytest.raises(dc.DspCommandError):
        dc.Frame.from_bytes(bytes((2, 0, 0, 0xAB)) + bytes(8))
    with pytest.raises(dc.DspCommandError):
        dc.Frame.from_bytes(bytes((3, 0, 0, 0xDC)) + bytes(8))
    with pytest.raises(dc.DspCommandError):
        dc.frame_stream([0] * 10, plan=[4, 4])


def test_decode_errors():
    with pytest.raises(dc.DspCommandError):
        dc.decode_commands([0x00010000])            # zero length
    with pytest.raises(dc.DspCommandError):
        dc.decode_commands([0x00140005, 1, 2])      # runs past the end
    with pytest.raises(dc.DspCommandError):
        dc.decode_commands([0x00080003, 1, 2])      # ZERO_FILL is 4 dwords
    with pytest.raises(dc.DspCommandError):
        dc.Command(dc.OP_SYNCH, (1, 2))


def test_connect_date_word_matches_captures_and_driver_rule():
    def ts(*a):
        return datetime(*a, tzinfo=timezone.utc).timestamp()
    assert dc.connect_date_word(ts(2026, 3, 27, 18, 16)) == 0x01B71448
    assert dc.connect_date_word(ts(2026, 4, 5, 8, 26, 59)) == 0x01B7A1FA
    assert dc.connect_date_word(ts(2006, 12, 31)) == 0
    assert dc.connect_date_word(ts(2007, 1, 1)) == 0


def test_code_load_ack_reply():
    # bulk IN reply to the A5 load, tools/usb-re/mixer-knobs.pcap
    raw = bytes.fromhex("040047dd" "04000780" "00000000" "a5000000" "e7010100")
    replies = dc.parse_reply(raw)
    assert replies == [(0x8007, (0, 0xA5, 0x000101E7))]
    assert dc.code_load_ack(replies) == [(0, 0xA5, 0x000101E7)]


def test_long_reply_is_kept_partial():
    # first frame of the 770-word 0x8003 reply (apollo-init.pcap): echoes
    # the 0x11 query's argument
    raw = bytes.fromhex("040000dd" "02030380" "2024e50a" "00000081" "00000081")
    assert dc.parse_reply(raw) == [(0x8003, (0x0AE52420, 0x81000000, 0x81000000))]
    with pytest.raises(dc.DspCommandError):
        dc.parse_reply(bytes.fromhex("01000adc" "01002700"))


def test_eeprom_write_is_decodable_but_not_built():
    assert dc.OPCODES[dc.OP_EEPROM_WRITE][0] == "EEPROM_WRITE"
    builders = [n for n in dir(dc) if "eeprom" in n.lower() and callable(getattr(dc, n))]
    assert builders == []


def test_parts_split_the_stream_for_the_u38_bisection(template, stream):
    cap = stream.commands
    tear = si.build_transactions(template, parts={"teardown"})
    tear = [c for tx in tear for c in tx]
    assert tear == cap[:35]                 # capture frames 0-9
    assert all(c.op in (dc.OP_SYNCH, dc.OP_INPUT_PARAM, dc.OP_RELEASE)
               for c in tear)
    code = [c for tx in si.build_transactions(template, parts={"code"})
            for c in tx]
    assert code == [c for c in cap if c.op == dc.OP_LOAD_CODE]
    no_enable = [c for tx in si.build_transactions(
        template, parts={"teardown", "code", "strips"}) for c in tx]
    assert no_enable == cap[:-30]           # all but the last frame's enables
    plan = si.build_init(template, rate_ack=False, parts={"teardown", "code"})
    assert not plan.capture_framing
    assert plan.commands() == tear + code
