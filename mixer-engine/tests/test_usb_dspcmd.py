"""J1: the DSP command language and the Solo USB init generator.  No hardware.

The Windows capture of a Solo USB's DSP bring-up held UA's DSP modules and
is not in the repository.  The generator takes the modules from the user's
own UAD install and everything else from tools/uausb/solo-usb-init.json
(the facts).  These tests run it on made-up modules of the same sizes
(usb_solo_modules.py): every address, command, frame size and sequence
number is checked, and the stream with the code payloads zeroed must hash
to the facts' code_blanked_sha256, i.e. equal the capture byte for byte
outside UA's code.  When the extracted modules are installed
($OA_DSP_DIR or /lib/firmware/universal-audio/dsp) the whole stream must
hash to the captured stream's SHA-256.
"""

import hashlib
import json
import struct
from datetime import datetime, timezone

import pytest

from usb_solo_modules import (FACTS, MODULE_IDS, REAL_DSP_DIR,
                              container_sha256, fake_record,
                              real_modules_present, write_modules)
from usb_solo_modules import dc, si

PINS = (si.FACTS_PATH.parents[2] / "console-rs" / "crates" / "oa-assets"
        / "data" / "firmware-pins.json")
SEED = 5            # made-up payloads that are not all zero


@pytest.fixture(scope="module")
def fakes(tmp_path_factory):
    """(dsp dir, facts naming the stand-ins, {id: record}), patterned payloads."""
    d = tmp_path_factory.mktemp("dsp")
    facts, records = write_modules(d, seed=SEED)
    return d, facts, records


@pytest.fixture(scope="module")
def template(fakes):
    d, facts, _ = fakes
    return si.Template.from_firmware(d, facts)


@pytest.fixture(scope="module")
def plan(template):
    """The 48 kHz strip stream as captured (no rate ack)."""
    return si.build_init(template, rate=48000, rate_ack=False)


@pytest.fixture(scope="module")
def stream(plan):
    """The generated frames decoded again, as a capture would be."""
    return dc.InitStream.from_urbs(plan.urbs())


# ── the shipped facts ───────────────────────────────────────────────────

def test_facts_describe_the_capture_without_its_code():
    s = FACTS["stream"]
    assert FACTS["format"] == si.FACTS_FORMAT and FACTS["rate"] == 48000
    assert (s["frames"], s["commands"], s["seq0"], s["flags"]) == (38, 128, 255, 0)
    assert len(s["frame_words"]) == 38
    assert all(0 < n <= dc.MAX_FRAME_WORDS for n in s["frame_words"])
    assert s["bytes"] == 4 + sum(4 + 4 + 4 * n for n in s["frame_words"])
    assert MODULE_IDS == [0xA5, 0xC2, 0xEB, 0xDA, 0x12B]     # load order
    assert {k: [(i, o, len(w)) for i, o, w in v]
            for k, v in si.params_from_json(FACTS["params"]).items()} == {
        "input": [(1, 0, 9)],
        "mic": [(1, 0, 18), (1, 0x26, 16), (4, 1, 15)],
        "aux": [(1, 0, 5)],
    }
    # parameters and hashes only: nothing that looks like a code module
    text = si.FACTS_PATH.read_text()
    assert len(text) < 8192
    assert set(FACTS) == {"format", "about", "rate", "modules", "params", "stream"}


def test_facts_modules_are_the_pinned_versions():
    """oa-assets firmware extracts exactly the versions the init expects."""
    pins = {int(m["id"], 16): m for m in json.loads(PINS.read_text())["dsp_modules"]}
    for m in FACTS["modules"]:
        pin = pins[int(m["id"], 16)]
        assert pin["required"]
        assert {"length": m["length"], "w4": m["w4"], "sha256": m["sha256"]} in [
            {k: known[k] for k in ("length", "w4", "sha256")} for known in pin["known"]]


def test_params_json_roundtrip():
    params = si.params_from_json(FACTS["params"])
    assert si.params_from_json(si.params_to_json(params)) == params
    assert si.params_to_json(params) == FACTS["params"]


# ── modules: on disk and on USB ─────────────────────────────────────────

def test_usb_blob_clears_word2_and_keeps_the_rest(fakes):
    _d, _facts, records = fakes
    for module, rec in records.items():
        blob = si.usb_blob(rec, module)
        assert blob[0] == dc.BILL_MAGIC and blob[1] == module
        assert blob[2] == 0                         # 0x02000000 on disk
        assert blob[3] == len(rec) - 20 and blob[4] == struct.unpack_from("<I", rec, 16)[0]
        assert struct.pack(f"<{len(blob) - 5}I", *blob[5:]) == rec[20:]
        assert si.disk_record(blob) == rec


def test_usb_blob_clears_the_arch_byte():
    rec = fake_record(0xC2, 184, 33, seed=1, w2=0)
    rec = rec[:7] + bytes([2]) + rec[8:]         # PCIe form: arch byte 2
    blob = si.usb_blob(rec, 0xC2)
    assert blob[1] == 0xC2 and blob[2] == 0


@pytest.mark.parametrize("bad, why", [
    (lambda r: r[:-4], "says"),                     # truncated payload
    (lambda r: r[:10], "not a Bill record"),
    (lambda r: b"Xi" + r[2:], "magic"),
    (lambda r: r + b"\0", "not a Bill record"),     # not whole words
    (lambda r: r[:16] + struct.pack("<I", 0) + r[20:], "w4"),
])
def test_usb_blob_refuses_a_broken_record(bad, why):
    rec = fake_record(0xC2, 184, 33, seed=1)
    with pytest.raises(si.TemplateError, match=why):
        si.usb_blob(bad(rec), 0xC2)
    with pytest.raises(si.TemplateError, match="module 0xc2, expected 0xeb"):
        si.usb_blob(rec, 0xEB)


# ── Template.from_firmware ─────────────────────────────────────────────

def test_template_takes_code_from_the_modules_and_the_rest_from_facts(fakes, template):
    _d, facts, records = fakes
    assert list(template.blobs) == MODULE_IDS
    for module, rec in records.items():
        assert template.blob(module) == si.usb_blob(rec, module)
    assert template.params == si.params_from_json(FACTS["params"])
    assert template.plan == FACTS["stream"]["frame_words"]
    assert (template.seq0, template.flags) == (255, 0)
    assert template.capture_sha256 == FACTS["stream"]["sha256"]
    assert template.stream is None


def test_missing_module_names_the_file_and_how_to_extract(fakes, tmp_path):
    d, facts, _ = fakes
    for m in MODULE_IDS:
        if m != 0xDA:
            (tmp_path / si.module_file(m)).write_bytes((d / si.module_file(m)).read_bytes())
    with pytest.raises(si.FirmwareError) as e:
        si.Template.from_firmware(tmp_path, facts)
    msg = str(e.value)
    assert str(tmp_path / "0da.bill") in msg and "missing" in msg
    assert "open-apollo-setup" in msg and "oa-assets firmware" in msg
    assert "/lib/firmware/universal-audio" in msg


def test_other_module_version_is_refused_unless_unverified(fakes, tmp_path):
    d, facts, _ = fakes
    for m in MODULE_IDS:
        (tmp_path / si.module_file(m)).write_bytes((d / si.module_file(m)).read_bytes())
    other = fake_record(0x12B, 452, 96, seed=SEED + 1)
    (tmp_path / "12b.bill").write_bytes(other)
    with pytest.raises(si.FirmwareError) as e:
        si.Template.from_firmware(tmp_path, facts)
    msg = str(e.value)
    assert str(tmp_path / "12b.bill") in msg and "not the module version" in msg
    assert hashlib.sha256(other).hexdigest()[:16] in msg
    assert "oa-assets firmware" in msg
    t = si.Template.from_firmware(tmp_path, facts, verify=False)
    assert t.blob(0x12B) == si.usb_blob(other, 0x12B)


def test_broken_module_file_is_a_firmware_error(fakes, tmp_path):
    d, facts, _ = fakes
    for m in MODULE_IDS:
        (tmp_path / si.module_file(m)).write_bytes((d / si.module_file(m)).read_bytes())
    (tmp_path / "0c2.bill").write_bytes(fake_record(0xEB, 184, 33, seed=1))
    with pytest.raises(si.FirmwareError, match="0c2.bill: the record is module 0xeb"):
        si.Template.from_firmware(tmp_path, facts, verify=False)


def test_facts_file_errors(tmp_path):
    with pytest.raises(si.TemplateError, match="cannot read the init facts"):
        si.load_facts(tmp_path / "none.json")
    bad = tmp_path / "bad.json"
    bad.write_text("{")
    with pytest.raises(si.TemplateError, match="not JSON"):
        si.load_facts(bad)
    bad.write_text(json.dumps({"format": "something/9"}))
    with pytest.raises(si.TemplateError, match="format"):
        si.load_facts(bad)
    with pytest.raises(si.TemplateError, match="format"):
        si.Template.from_firmware(tmp_path, bad)
    assert si.load_facts() == FACTS


# ── the generated stream ────────────────────────────────────────────────

def test_stream_has_the_captured_commands(stream):
    assert len(stream.frames) == 38
    assert stream.seq0 == 255 and stream.flags == 0
    names = {}
    for c in stream.commands:
        names[c.name] = names.get(c.name, 0) + 1
    assert names == {"SYNCH": 12, "INPUT_PARAM": 48, "RELEASE": 5,
                     "LOAD_CODE": 5, "ZERO_FILL": 36, "MEMSPEC": 6,
                     "WRITE_BLOCK": 16}
    assert len(stream.commands) == 128
    assert [f.seq for f in stream.frames] == [(255 + i) & 0xFF for i in range(38)]
    assert all(f.magic == dc.FRAME_MAGIC_OUT for f in stream.frames)
    assert stream.plan == FACTS["stream"]["frame_words"]


def test_stream_command_details(stream, fakes):
    _d, _facts, records = fakes
    c = stream.commands
    assert c[0] == dc.synch(0x00, 0x20, 0, 0)
    assert c[1] == dc.input_param(0, 8, 0x20)
    assert c[3] == dc.input_param(0, 7, 0x1FFFF)
    loads = [x for x in c if x.op == dc.OP_LOAD_CODE]
    assert [(x.args[0], x.args[2] & 0xFFFF, x.length) for x in loads] == [
        (0xE0000, 0xA5, 487), (0xE0130, 0xC2, 48), (0xE0146, 0xEB, 181),
        (0xE01B2, 0xDA, 763), (0xE03A2, 0x12B, 115)]
    # Bill header as transmitted: arch byte 0, word 2 = 0; payload as on disk
    for x in loads:
        assert x.args[1] == dc.BILL_MAGIC
        assert x.args[2] >> 24 == 0 and x.args[3] == 0
        assert x.length == 2 + (x.args[4] + 0x14) // 4
        rec = records[x.args[2] & 0xFFFF]
        assert struct.pack(f"<{x.length - 7}I", *x.args[6:]) == rec[20:]
    # code loads that span frames: A5 (2 frames) and DA (3 frames)
    starts = {x.args[2] & 0xFFFF: s for x, s in zip(c, stream.starts)
              if x.op == dc.OP_LOAD_CODE}
    assert starts == {0xA5: 10, 0xC2: 12, 0xEB: 20, 0xDA: 21, 0x12B: 24}


def test_stream_is_the_capture_outside_the_code(plan):
    """Every byte but UA's code payloads, against the facts' hash."""
    assert plan.capture_framing and plan.preamble == 0
    assert plan.stream_sha256(blank=True) == FACTS["stream"]["code_blanked_sha256"]
    # the patterned payloads are not the captured code
    assert not plan.byte_identical


def test_zero_payload_modules_rebuild_the_blanked_capture(tmp_path):
    facts, _ = write_modules(tmp_path)
    plan = si.build_init(si.Template.from_firmware(tmp_path, facts), rate_ack=False)
    assert container_sha256(plan.urbs()) == FACTS["stream"]["code_blanked_sha256"]
    assert len(dc.build_urb_container(plan.urbs())) == FACTS["stream"]["bytes"]
    assert plan.byte_identical            # these facts name that stream


def test_stream_reencodes_byte_identically(stream, plan):
    urbs = plan.urbs()
    assert stream.to_urbs() == urbs
    for u in urbs:
        assert dc.Frame.from_bytes(u).to_bytes() == u
    assert dc.parse_urb_container(dc.build_urb_container(urbs)) == urbs


def test_capture_template_gives_back_the_blobs_and_params(stream, template):
    """Template.from_stream (a maintainer's own capture) reads back what
    from_firmware put in."""
    back = si.Template.from_stream(stream)
    assert back.blobs == template.blobs
    assert back.params == template.params
    assert (back.plan, back.seq0, back.flags) == (template.plan, 255, 0)
    assert si.build_init(back, rate_ack=False).byte_identical


def test_capture_file_template(plan, template, tmp_path):
    f = tmp_path / "capture.bin"
    f.write_bytes(dc.build_urb_container(plan.urbs()))
    back = si.Template.from_file(f)
    assert back.blobs == template.blobs
    assert back.capture_sha256 == hashlib.sha256(f.read_bytes()).hexdigest()


def test_make_facts_rebuilds_the_shipped_facts(stream, fakes):
    """tools/usb-re/make-solo-init-facts.py's core, on the stand-in stream:
    the same parameters and framing, the stand-ins' hashes."""
    _d, facts, records = fakes
    got = si.make_facts(stream)
    assert got["params"] == FACTS["params"]
    assert got["modules"] == facts["modules"]
    for m in got["modules"]:
        assert m["sha256"] == hashlib.sha256(records[int(m["id"], 16)]).hexdigest()
    want = dict(FACTS["stream"], sha256=got["stream"]["sha256"])
    assert got["stream"] == want
    assert got["stream"]["sha256"] == container_sha256(stream.to_urbs())
    assert {k: got[k] for k in ("format", "about", "rate")} == {
        k: FACTS[k] for k in ("format", "about", "rate")}


def test_facts_script_writes_the_shipped_format(plan, fakes, tmp_path, capsys):
    """tools/usb-re/make-solo-init-facts.py on a stand-in capture file."""
    import importlib.util
    script = si.FACTS_PATH.parents[1] / "usb-re" / "make-solo-init-facts.py"
    spec = importlib.util.spec_from_file_location("make_solo_init_facts", script)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    cap = tmp_path / "capture.bin"
    cap.write_bytes(dc.build_urb_container(plan.urbs()))
    out = tmp_path / "facts.json"
    assert mod.main([str(cap), str(out)]) == 0
    got = json.loads(out.read_text())
    assert got == si.make_facts(dc.InitStream.from_file(cap))
    assert got["modules"] == fakes[1]["modules"]
    # the stand-ins are not the pinned modules, and the script says so
    assert "not in firmware-pins.json" in capsys.readouterr().out
    # the shipped file is the script's own output format
    assert mod.dumps(FACTS) + "\n" == si.FACTS_PATH.read_text()
    assert mod.main([str(tmp_path / "none.bin"), str(out)]) == 1


def test_make_facts_refuses_a_stream_it_cannot_rebuild(stream):
    frames = list(stream.frames)
    words = list(frames[0].words)
    words[3] ^= 1                           # an input param the model doesn't make
    frames[0] = dc.Frame(words, frames[0].seq, frames[0].flags)
    odd = dc.InitStream.from_urbs([f.to_bytes() for f in frames])
    with pytest.raises(si.TemplateError, match="byte for byte"):
        si.make_facts(odd)


def test_memory_map_tops_match_capture():
    assert si.SOLO_USB_MEMORY_MAP.tops() == (0x9D1B8, 0xC6FFE, 0x08FFE000)


def test_rate_ack_frame_comes_first(template, plan):
    full = si.build_init(template)          # rate_ack default on, 48 kHz
    out = full.urbs()
    assert out[0] == bytes((2, 0, 254, 0xDC)) + struct.pack("<II", 0x001A0002, 48000)
    assert out[1:] == plan.urbs()
    assert full.preamble == 1 and full.body() == plan.frames[0:]


def test_natural_framing_carries_same_stream(template, stream):
    nat = si.build_init(template, rate_ack=False, capture_framing=False)
    assert not nat.capture_framing
    assert all(len(f.words) <= 255 for f in nat.frames)
    assert dc.decode_commands(dc.deframe(nat.frames)) == stream.commands
    # one transaction per frame start: 9 teardown groups first
    assert [len(f.words) for f in nat.frames[:3]] == [16, 16, 6]
    assert nat.stream_sha256(blank=True) != FACTS["stream"]["code_blanked_sha256"]


def test_connect_preamble(template, plan):
    full = si.build_init(template, connect_date=0x01B71448, query_tag=0x0AE52420)
    out = full.urbs()
    # the two connect frames of the Windows bring-up (apollo-init.pcap)
    assert out[0][4:] == bytes.fromhex("0200230001000000020010004814b701")
    assert out[1][4:] == bytes.fromhex("02001000" "4814b701" "02001100" "2024e50a")
    assert [u[2] for u in out[:3]] == [252, 253, 254]
    assert out[3:] == plan.urbs()


def test_mic_ramp_rule_gives_captured_words():
    assert si.mic_ramp_words(48000) == (75, 0x41000000, 0x3ADA740E)
    k, f, inv = si.mic_ramp_words(44100)
    assert k == 69 and f == 0x41000000
    k, f, _ = si.mic_ramp_words(192000)
    assert k == 150 and f == 0x41800000     # block size 64 above 100 kHz
    # the facts carry the 48 kHz words the rule gives
    mic = si.params_from_json(FACTS["params"])["mic"][0][2]
    assert tuple(mic[si.MIC_RAMP_INDEX:si.MIC_RAMP_INDEX + 3]) == si.mic_ramp_words(48000)


def test_other_rate_changes_rate_ack_and_mic_ramp_only(template, stream):
    other = si.build_init(template, rate=96000)
    gen = other.commands()
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
    assert other.capture_framing and not other.byte_identical


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


def test_template_without_framing_cuts_per_transaction(template):
    t = si.Template(dict(template.blobs), dict(template.params))
    p = si.build_init(t, rate_ack=False)
    assert not p.capture_framing and p.capture_sha256 is None
    assert not p.byte_identical
    assert p.frames[0].seq == 0


# ── the real modules (skipped unless they are installed) ────────────────

@pytest.mark.skipif(not real_modules_present(),
                    reason=f"no extracted DSP modules in {REAL_DSP_DIR} "
                           "(setup wizard / oa-assets firmware; $OA_DSP_DIR)")
def test_real_modules_rebuild_the_capture_byte_for_byte():
    t = si.Template.from_firmware(REAL_DSP_DIR)
    full = si.build_init(t, rate=48000, rate_ack=False)
    data = dc.build_urb_container(full.urbs())
    assert len(data) == FACTS["stream"]["bytes"]
    assert hashlib.sha256(data).hexdigest() == FACTS["stream"]["sha256"]
    assert full.byte_identical
    assert full.stream_sha256(blank=True) == FACTS["stream"]["code_blanked_sha256"]
    # with the rate ack in front the strip stream is still the capture's
    assert si.build_init(t).byte_identical


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
    p = si.build_init(template, rate_ack=False, parts={"teardown", "code"})
    assert not p.capture_framing
    assert p.commands() == tear + code
