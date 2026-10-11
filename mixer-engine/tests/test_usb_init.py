"""J1/J3: tools/usb-full-init.py and tools/usb-dsp-init.py on a fake Solo
USB: the generated stream (byte-identical to the replay at 48 kHz), the
old replay path, the DSP boot-word poll and the code-load ack waits with
their fixed-wait fallbacks.  No hardware, no pyusb."""

import importlib.util
import struct
import sys
import types

import pytest

from conftest import ENGINE_DIR
from usb_fake import FakeApollo, FakeUsb

TOOLS = ENGINE_DIR.parent / "tools"
CAPTURE = TOOLS / "usb-re" / "init-bulk-sequence.bin"


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


def capture_urbs():
    from uausb import dspcmd
    return dspcmd.read_urb_file(CAPTURE)


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


def test_replay_path_is_the_old_behaviour(full_init):
    dev = FakeApollo()
    rc, usb, sleeps = run(full_init, dev, ["--replay"])
    assert rc == 0
    assert dev.bulk_out == capture_urbs()
    assert sleeps == old_pacing_sleeps(capture_urbs())
    assert usb.claimed == [0] and usb.released == [0]
    assert usb.halts == [0x01, 0x81]
    assert dev.uac2_rate == 48000
    # no register access on the replay path
    assert not [c for c in dev.ctrl_log if c[0] == 0xC1]


def test_generated_stream_with_ready_poll_and_acks(full_init):
    dev = FakeApollo()
    dev.boot_words = [0, 0, 0x00000C03]
    dev.auto_ack = True
    rc, usb, sleeps = run(full_init, dev, [])
    assert rc == 0
    # at 48 kHz the generated frames are the replay's bytes
    assert dev.bulk_out == capture_urbs()
    assert sleeps == [0.3, 0.3]             # the poll only: no fixed pauses
    assert dev.bulk_in == []                # every code-load ack consumed
    polls = [c for c in dev.ctrl_log if c[:3] == (0xC1, 0x04, 0x1069)]
    assert len(polls) == 3
    assert dev.uac2_rate == 48000


def test_rate_ack_on_request_and_off(full_init):
    dev = FakeApollo()
    dev.auto_ack = True
    run(full_init, dev, ["--rate-ack"])
    assert dev.bulk_out == [RATE_ACK_48K] + capture_urbs()
    dev = FakeApollo()
    dev.auto_ack = True
    run(full_init, dev, ["--rate", "44100", "--no-rate-ack"])
    assert len(dev.bulk_out) == 38


def test_unreadable_boot_word_falls_back_to_fixed_waits(full_init):
    dev = FakeApollo()
    dev.fail_requests.add((0x04, 0x1069))
    rc, _usb, sleeps = run(full_init, dev, [])
    assert rc == 0
    assert dev.bulk_out == capture_urbs()
    assert sleeps == old_pacing_sleeps(capture_urbs())


def test_dsp_that_never_starts_falls_back(full_init):
    dev = FakeApollo()
    dev.boot_words = [0]
    rc, _usb, sleeps = run(full_init, dev, [])
    assert rc == 0
    assert sleeps[:59] == [0.3] * 59
    assert sleeps[59:] == old_pacing_sleeps(capture_urbs())


def test_missing_ack_falls_back_mid_stream(full_init):
    dev = FakeApollo()
    dev.auto_ack = False                    # the DSP never acks
    rc, _usb, sleeps = run(full_init, dev, [])
    assert rc == 0
    assert dev.bulk_out == capture_urbs()
    # the A5 load ends in body frame 11: 0.2 s, then the old pacing from
    # frame 12 on, with the 2 s pause before frame 24
    assert sleeps[0] == 0.2
    assert sleeps[1:] == old_pacing_sleeps(capture_urbs(), start=12)
    assert 2.0 in sleeps


def test_other_rate(full_init):
    dev = FakeApollo()
    dev.auto_ack = True
    run(full_init, dev, ["--rate", "96000"])
    assert dev.bulk_out[0] == bytes((2, 0, 254, 0xDC)) + struct.pack(
        "<II", 0x001A0002, 96000)
    assert len(dev.bulk_out) == 39
    assert dev.uac2_rate == 96000


def test_connect_handshake_option(full_init):
    dev = FakeApollo()
    dev.auto_ack = True
    rc, _usb, _s = run(full_init, dev, ["--connect"])
    assert rc == 0
    f0, f1 = dev.bulk_out[0], dev.bulk_out[1]
    assert struct.unpack("<II", f0[4:12]) == (0x00230002, 1)
    assert struct.unpack("<I", f0[12:16])[0] == 0x00100002
    assert struct.unpack("<I", f1[12:16])[0] == 0x00110002
    assert dev.bulk_out[2:] == capture_urbs()
    assert [b[2] for b in dev.bulk_out[:3]] == [253, 254, 255]


def test_non_solo_model_keeps_the_replay(full_init, capsys):
    dev = FakeApollo(pid=0x000F)
    rc, _usb, sleeps = run(full_init, dev, [])
    assert rc == 0
    assert dev.bulk_out == capture_urbs()
    assert "issue #69" in capsys.readouterr().out


def test_no_device(full_init):
    usb = FakeUsb(None)
    assert full_init.main([], usb=usb, sleep=lambda s: None) == 1


def test_dry_run_needs_no_usb(full_init, tmp_path, capsys):
    out = tmp_path / "gen.bin"
    assert full_init.main(["--dry-run", "--write-bin", str(out)]) == 0
    assert out.read_bytes() == CAPTURE.read_bytes()
    text = capsys.readouterr().out
    assert "128 commands" in text and "LOAD_CODE" in text


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
