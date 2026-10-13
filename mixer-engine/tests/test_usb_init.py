"""J1/J3: tools/usb-full-init.py and tools/usb-dsp-init.py on a fake Solo
USB: the generated stream (the captured bytes at 48 kHz), the replay
timing, the DSP boot-word poll and the code-load ack waits with their
fixed-wait fallbacks, and what happens without the extracted DSP modules.
The modules are made-up stand-ins with zero payloads (usb_solo_modules.py),
so the stream sent is the capture with its code zeroed: the facts'
code_blanked_sha256.  No hardware, no pyusb."""

import importlib.util
import json
import struct
import sys
import types
from types import SimpleNamespace

import pytest

from conftest import ENGINE_DIR
from usb_fake import FakeApollo, FakeUsb
from usb_solo_modules import (FACTS, container_sha256, fake_record,
                              write_modules)
from usb_solo_modules import dc, si

TOOLS = ENGINE_DIR.parent / "tools"
BLANKED = FACTS["stream"]["code_blanked_sha256"]


def load_tool(name, modname, fake_usb=None):
    if fake_usb is not None:
        pkg = types.ModuleType("usb")
        pkg.core, pkg.util = fake_usb.core, fake_usb.util
        sys.modules["usb"] = pkg
        sys.modules["usb.core"] = fake_usb.core
        sys.modules["usb.util"] = fake_usb.util
    try:
        spec = importlib.util.spec_from_file_location(modname, TOOLS / name)
        mod = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(mod)
    finally:
        for k in ("usb", "usb.core", "usb.util"):
            if fake_usb is not None:
                sys.modules.pop(k, None)
    return mod


@pytest.fixture
def full_init():
    mod = load_tool("usb-full-init.py", "usb_full_init")
    mod.ACK_TIMEOUT_MS = 20
    return mod


@pytest.fixture
def fw(tmp_path):
    """Stand-in modules in a DSP folder and facts naming them.  urbs: the
    48 kHz strip stream they make, the capture with its code zeroed."""
    d = tmp_path / "dsp"
    facts, _ = write_modules(d)
    path = tmp_path / "facts.json"
    path.write_text(json.dumps(facts))
    template = si.Template.from_firmware(d, facts)
    urbs = si.build_init(template, rate_ack=False).urbs()
    assert len(urbs) == 38 and container_sha256(urbs) == BLANKED
    return SimpleNamespace(dir=d, facts=facts, path=path, template=template,
                           urbs=urbs,
                           args=["--dsp-dir", str(d), "--facts", str(path)])


def old_pacing_sleeps(urbs, start=0):
    """The sleeps the old replay makes between packets (from packet start)."""
    out = []
    for i, u in enumerate(urbs):
        if i < start:
            continue
        if i == 24:
            out.append(2.0)
        if i > 0:
            out.append(0.2 if len(u) > 512 else 0.05)
    return out


RATE_ACK_48K = bytes((2, 0, 254, 0xDC)) + struct.pack("<II", 0x001A0002, 48000)


def run(mod, dev, argv):
    usb = FakeUsb(dev)
    sleeps = []
    rc = mod.main(argv, usb=usb, sleep=sleeps.append)
    return rc, usb, sleeps


def test_default_paths(full_init):
    assert full_init.DSP_DIR == "/lib/firmware/universal-audio/dsp"
    assert full_init.DSP_DIR == si.DEFAULT_DSP_DIR
    # the facts travel with the uausb package (install-usb.sh copies both)
    assert si.FACTS_PATH == TOOLS / "uausb" / "solo-usb-init.json"
    assert si.FACTS_PATH.is_file()
    install = (ENGINE_DIR.parent / "scripts" / "install-usb.sh").read_text()
    assert "tools/uausb/solo-usb-init.json" in install
    assert "--check --dsp-dir" in install


def test_replay_path_is_the_old_behaviour(full_init, fw, capsys):
    dev = FakeApollo()
    rc, usb, sleeps = run(full_init, dev, ["--replay"] + fw.args)
    assert rc == 0
    assert dev.bulk_out == fw.urbs
    assert sleeps == old_pacing_sleeps(fw.urbs)
    assert usb.claimed == [0] and usb.released == [0]
    assert usb.halts == [0x01, 0x81]
    assert dev.uac2_rate == 48000
    # no register access on the replay path
    assert not [c for c in dev.ctrl_log if c[0] == 0xC1]
    assert "byte-identical to the captured stream" in capsys.readouterr().out


def test_replay_of_your_own_capture(full_init, fw, tmp_path):
    # a capture cut differently from the facts' framing: sent as it is
    nat = si.build_init(fw.template, rate_ack=False, capture_framing=False)
    cap = tmp_path / "mine.bin"
    cap.write_bytes(dc.build_urb_container(nat.urbs()))
    dev = FakeApollo()
    rc, _usb, sleeps = run(full_init, dev, ["--replay", "--capture", str(cap),
                                            "--dsp-dir", str(tmp_path / "none")])
    assert rc == 0
    assert dev.bulk_out == nat.urbs()
    assert sleeps == old_pacing_sleeps(nat.urbs())


def test_generated_from_your_own_capture(full_init, fw, tmp_path):
    cap = tmp_path / "mine.bin"
    cap.write_bytes(dc.build_urb_container(fw.urbs))
    dev = FakeApollo()
    dev.auto_ack = True
    rc, _usb, _s = run(full_init, dev, ["--capture", str(cap),
                                        "--dsp-dir", str(tmp_path / "none")])
    assert rc == 0
    assert dev.bulk_out == fw.urbs
    dev = FakeApollo()
    rc, _usb, _s = run(full_init, dev, ["--capture", str(tmp_path / "no.bin")])
    assert rc == 1 and dev.bulk_out == []


def test_generated_stream_with_ready_poll_and_acks(full_init, fw, capsys):
    dev = FakeApollo()
    dev.boot_words = [0, 0, 0x00000C03]
    dev.auto_ack = True
    rc, usb, sleeps = run(full_init, dev, fw.args)
    assert rc == 0
    # at 48 kHz the generated frames are the capture's bytes
    assert dev.bulk_out == fw.urbs
    assert sleeps == [0.3, 0.3]             # the poll only: no fixed pauses
    assert dev.bulk_in == []                # every code-load ack consumed
    polls = [c for c in dev.ctrl_log if c[:3] == (0xC1, 0x04, 0x1069)]
    assert len(polls) == 3
    assert dev.uac2_rate == 48000
    assert "byte-identical to the captured stream" in capsys.readouterr().out


def test_rate_ack_on_request_and_off(full_init, fw):
    dev = FakeApollo()
    dev.auto_ack = True
    run(full_init, dev, ["--rate-ack"] + fw.args)
    assert dev.bulk_out == [RATE_ACK_48K] + fw.urbs
    dev = FakeApollo()
    dev.auto_ack = True
    run(full_init, dev, ["--rate", "44100", "--no-rate-ack"] + fw.args)
    assert len(dev.bulk_out) == 38


def test_unreadable_boot_word_falls_back_to_fixed_waits(full_init, fw):
    dev = FakeApollo()
    dev.fail_requests.add((0x04, 0x1069))
    rc, _usb, sleeps = run(full_init, dev, fw.args)
    assert rc == 0
    assert dev.bulk_out == fw.urbs
    assert sleeps == old_pacing_sleeps(fw.urbs)


def test_dsp_that_never_starts_falls_back(full_init, fw):
    dev = FakeApollo()
    dev.boot_words = [0]
    rc, _usb, sleeps = run(full_init, dev, fw.args)
    assert rc == 0
    assert sleeps[:59] == [0.3] * 59
    assert sleeps[59:] == old_pacing_sleeps(fw.urbs)


def test_missing_ack_falls_back_mid_stream(full_init, fw):
    dev = FakeApollo()
    dev.auto_ack = False                    # the DSP never acks
    rc, _usb, sleeps = run(full_init, dev, fw.args)
    assert rc == 0
    assert dev.bulk_out == fw.urbs
    # the A5 load ends in body frame 11: 0.2 s, then the old pacing from
    # frame 12 on, with the 2 s pause before frame 24
    assert sleeps[0] == 0.2
    assert sleeps[1:] == old_pacing_sleeps(fw.urbs, start=12)
    assert 2.0 in sleeps


def test_other_rate(full_init, fw, capsys):
    dev = FakeApollo()
    dev.auto_ack = True
    run(full_init, dev, ["--rate", "96000"] + fw.args)
    assert dev.bulk_out[0] == bytes((2, 0, 254, 0xDC)) + struct.pack(
        "<II", 0x001A0002, 96000)
    assert len(dev.bulk_out) == 39
    assert dev.uac2_rate == 96000
    out = capsys.readouterr().out
    assert "differs from the captured stream (rate 96000)" in out


def test_connect_handshake_option(full_init, fw):
    dev = FakeApollo()
    dev.auto_ack = True
    rc, _usb, _s = run(full_init, dev, ["--connect"] + fw.args)
    assert rc == 0
    f0, f1 = dev.bulk_out[0], dev.bulk_out[1]
    assert struct.unpack("<II", f0[4:12]) == (0x00230002, 1)
    assert struct.unpack("<I", f0[12:16])[0] == 0x00100002
    assert struct.unpack("<I", f1[12:16])[0] == 0x00110002
    assert dev.bulk_out[2:] == fw.urbs
    assert [b[2] for b in dev.bulk_out[:3]] == [253, 254, 255]


def test_non_solo_model_keeps_the_replay(full_init, fw, capsys):
    dev = FakeApollo(pid=0x000F)
    rc, _usb, sleeps = run(full_init, dev, fw.args)
    assert rc == 0
    assert dev.bulk_out == fw.urbs
    assert sleeps == old_pacing_sleeps(fw.urbs)
    assert "issue #69" in capsys.readouterr().out


def test_no_device(full_init, fw):
    usb = FakeUsb(None)
    assert full_init.main(fw.args, usb=usb, sleep=lambda s: None) == 1


def test_missing_modules_stop_before_the_device_is_touched(full_init, fw, capsys):
    (fw.dir / "0da.bill").unlink()
    for argv in ([], ["--replay"]):
        dev = FakeApollo()
        rc, usb, sleeps = run(full_init, dev, argv + fw.args)
        assert rc == 1
        assert usb.claimed == [] and usb.halts == []
        assert dev.bulk_out == [] and sleeps == []
        out = capsys.readouterr().out
        assert str(fw.dir / "0da.bill") in out
        assert "oa-assets firmware" in out and "open-apollo-setup" in out


def test_unknown_module_version_needs_the_flag(full_init, fw, capsys):
    (fw.dir / "12b.bill").write_bytes(fake_record(0x12B, 452, 96, seed=9))
    dev = FakeApollo()
    rc, usb, _s = run(full_init, dev, fw.args)
    assert rc == 1 and usb.claimed == [] and dev.bulk_out == []
    assert "not the module version" in capsys.readouterr().out
    dev = FakeApollo()
    dev.auto_ack = True
    rc, _usb, _s = run(full_init, dev, ["--accept-unknown-modules"] + fw.args)
    assert rc == 0 and len(dev.bulk_out) == 38
    assert dev.bulk_out != fw.urbs
    assert "differs from the captured stream" in capsys.readouterr().out


def test_check_option_needs_no_usb(full_init, fw, capsys):
    assert full_init.main(["--check"] + fw.args) == 0
    out = capsys.readouterr().out
    assert f"DSP modules OK in {fw.dir}: 0a5 0c2 0eb 0da 12b" in out
    assert "38 frames, 128 commands, byte-identical to the captured stream" in out
    (fw.dir / "0c2.bill").unlink()
    assert full_init.main(["--check"] + fw.args) == 1
    out = capsys.readouterr().out
    assert str(fw.dir / "0c2.bill") in out and "missing" in out
    assert full_init.main(["--check", "--facts", str(fw.dir / "none.json"),
                           "--dsp-dir", str(fw.dir)]) == 1
    assert "cannot read the init facts" in capsys.readouterr().out


def test_dry_run_needs_no_usb(full_init, fw, tmp_path, capsys):
    out = tmp_path / "gen.bin"
    assert full_init.main(["--dry-run", "--write-bin", str(out)] + fw.args) == 0
    assert container_sha256(dc.read_urb_file(out)) == BLANKED
    text = capsys.readouterr().out
    assert "128 commands" in text and "LOAD_CODE" in text
    assert "byte-identical to the captured stream" in text


def test_usb_dsp_init_polls_then_sends_the_captured_connect():
    dev = FakeApollo()
    dev.boot_words = [0, 0x00000C03]
    fake = FakeUsb(dev)
    mod = load_tool("usb-dsp-init.py", "usb_dsp_init", fake)
    import time as _t
    orig = _t.sleep
    try:
        _t.sleep = lambda s: None
        assert mod.wait_dsp_ready(dev) is True
    finally:
        _t.sleep = orig
    mod.dsp_init(dev)
    # unchanged bytes (Cauldron 1.3 capture frames 0-1)
    assert dev.bulk_out[0] == bytes.fromhex(
        "060000dc" "02002300" "01000000" "02001000" "faa1b701" "02001000"
        "faa1b701")
    assert dev.bulk_out[1] == bytes.fromhex("020001dc" "02001100" "00766082")
    assert len(dev.bulk_out) == 15
    dev2 = FakeApollo()
    dev2.fail_requests.add((0x04, 0x1069))
    assert mod.wait_dsp_ready(dev2) is False


def test_usb_mixer_test_tool_uses_the_mailbox(monkeypatch):
    mod = load_tool("usb-mixer-test.py", "usb_mixer_test")
    monkeypatch.setattr(mod.time, "sleep", lambda s: None)
    dev = FakeApollo()
    mx = mod.Mixer(dev)
    assert mx.mb.seq_wr == 6 and mx.protocol == 0x00010002
    mod.test_preamp_gain(mx)
    mod.test_mute(mx)
    seqs = [w[0] for a, w in dev.writes if a == 0x0602]
    assert seqs == list(range(7, 7 + 8 + 2))   # one SEQ bump per step
    # the 30 dB step: GainC 21 in setting 3, GainB/GainA 20 in 16/17
    g = [w for a, w in dev.writes if a == 0x064F]
    assert any(w[0] == 0x00FF0014 and w[2] == 0x00FF0014 for w in g)
    mute = [w for a, w in dev.writes if a == 0x062D][-2]
    assert mute[5] == 0x00030001             # setting 2 word B: mute bit 16


def test_part_option_sends_a_phase_subset(full_init, fw):
    dev = FakeApollo()
    dev.auto_ack = True
    rc, _usb, _s = run(full_init, dev, ["--part", "teardown,code"] + fw.args)
    assert rc == 0
    sent = dc.decode_commands(dc.deframe(
        [dc.Frame.from_bytes(b) for b in dev.bulk_out]))
    assert len(sent) == 40
    assert [c.op for c in sent].count(dc.OP_LOAD_CODE) == 5
    assert dev.bulk_in == []                # every load acked and read
    with pytest.raises(SystemExit):
        full_init.main(["--part", "bogus", "--dry-run"] + fw.args)
