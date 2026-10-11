"""N33/U39: USB mixer settings.  The one-shot mailbox reproduces every
settings write of the Windows capture (fixtures/usb_solo_mixer_knobs.json,
extracted from tools/usb-re/mixer-knobs.pcap) byte for byte, and the
SetMixerParam mapping matches the PCIe driver's (driver/ua_core.c).  No
hardware."""

import json
import re
import sys
from pathlib import Path

import pytest

from conftest import ENGINE_DIR

TOOLS = ENGINE_DIR.parent / "tools"
if str(TOOLS) not in sys.path:
    sys.path.insert(0, str(TOOLS))

from uausb import mixer as um  # noqa: E402

FIXTURE = Path(__file__).parent / "fixtures" / "usb_solo_mixer_knobs.json"
DRIVER = ENGINE_DIR.parent / "driver"
GROUP_FIRST = {0x062D: 0, 0x064F: 16, 0x0670: 32}


class Regs:
    """UsbRegs stand-in: SEQ_RD answers from a script, writes recorded."""

    def __init__(self, seq_reads=()):
        self.seq_reads = list(seq_reads)
        self.writes = []

    def read1(self, addr):
        assert addr == 0x0603
        return self.seq_reads.pop(0)

    def write(self, addr, words):
        self.writes.append((addr, list(words)))


def load_flushes():
    events = json.loads(FIXTURE.read_text())["events"]
    flushes, cur = [], None
    for e in events:
        if e["op"] == "seq_rd":
            cur = cur or {"blocks": []}
            cur["seq_rd"] = e["value"]
        elif e["op"] == "block":
            words = [0] * e["words"]
            for k, v in e["nonzero"].items():
                words[int(k)] = int(v, 16)
            cur["blocks"].append((int(e["addr"], 16), words))
        elif e["op"] == "seq_wr":
            cur["seq_wr"] = e["value"]
            flushes.append(cur)
            cur = None
    return flushes


def changes_of(flush):
    """(setting, value, mask) of every nonzero setting the flush wrote."""
    out = []
    for addr, words in flush["blocks"]:
        first = GROUP_FIRST[addr]
        for s in range(len(words) // 2):
            a, b = words[2 * s], words[2 * s + 1]
            if a or b:
                out.append((first + s, (a & 0xFFFF) | ((b & 0xFFFF) << 16),
                            ((a >> 16) & 0xFFFF) | (b & 0xFFFF0000)))
    return out


def test_fixture_is_the_capture_handshake():
    flushes = load_flushes()
    assert len(flushes) == 69
    assert flushes[0]["seq_rd"] == 6 and flushes[-1]["seq_wr"] == 0x4B
    # every flush waited for SEQ_RD == the last SEQ_WR and bumped it once
    for a, b in zip(flushes, flushes[1:]):
        assert b["seq_rd"] == a["seq_wr"] == b["seq_wr"] - 1


def test_mailbox_reproduces_every_captured_flush():
    """Feed the captured changes in; every block write and SEQ_WR comes out
    byte for byte, zero passes and untouched groups included."""
    flushes = load_flushes()
    mb = um.SettingsMailbox()
    # State from before the capture window: the value bits outside each
    # setting's first captured mask, and the zero pass the first flush
    # owes group 0 (it writes 0x062D as all zeros)
    first_seen = set()
    mb.zeroed[2] = False
    seq_reads = [f["seq_rd"] for f in flushes]
    regs = Regs(seq_reads)
    mb.seq_wr = flushes[0]["seq_rd"]
    for n, fl in enumerate(flushes):
        for idx, value, mask in changes_of(fl):
            if idx not in first_seen:
                first_seen.add(idx)
                mb.value[idx] = value & ~mask
            mb.merge(idx, value & mask, mask)
            assert mb.value[idx] == value, (n, idx)
        regs.writes.clear()
        assert mb.flush(regs) is True, n
        expected = [(addr, words) for addr, words in fl["blocks"]]
        expected.append((0x0602, [fl["seq_wr"]]))
        assert regs.writes == expected, f"flush {n}"
    assert not mb.pending


def test_flush_waits_for_seq_rd():
    mb = um.SettingsMailbox()
    mb.seq_wr = 5
    mb.merge(2, 0x2A, 0xFF)
    regs = Regs([4])
    assert mb.flush(regs) is False
    assert regs.writes == [] and mb.pending
    regs.seq_reads = [5]
    assert mb.flush(regs) is True
    assert regs.writes[0][0] == 0x062D
    assert regs.writes[0][1][4:6] == [0x00FF002A, 0]
    assert regs.writes[-1] == (0x0602, [6])
    assert mb.flush(Regs([6])) is None      # nothing pending: no flush


def test_zero_pass_rides_with_next_change():
    mb = um.SettingsMailbox()
    mb.seq_wr = 0
    mb.merge(35, 5, 0x1F)
    regs = Regs([0, 1])
    mb.flush(regs)
    assert [a for a, _ in regs.writes] == [0x0670, 0x0602]
    assert not mb.pending                   # the zero pass alone: no flush
    mb.merge(2, 0x10000, 0x30000)
    regs.writes.clear()
    mb.flush(regs)
    # group 0 with the change, group 32-37 as the zero pass of setting 35
    assert [a for a, _ in regs.writes] == [0x062D, 0x0670, 0x0602]
    assert regs.writes[1][1] == [0] * 12
    assert regs.writes[0][1][5] == 0x00030001


def test_adopt_seq_reads_seq_rd():
    mb = um.SettingsMailbox()
    assert mb.adopt_seq(Regs([0x41])) == 0x41 and mb.seq_wr == 0x41


def test_setting_address_layout():
    assert um.setting_address(0) == 0x062D
    assert um.setting_address(2) == 0x0631
    assert um.setting_address(15) == 0x064B
    assert um.setting_address(16) == 0x064F
    assert um.setting_address(31) == 0x066D
    assert um.setting_address(32) == 0x0670
    assert um.setting_address(37) == 0x067A
    with pytest.raises(IndexError):
        um.setting_address(38)


# ── mapping: the controls the capture exercises ─────────────────────────

W = um.SettingWrite


@pytest.mark.parametrize("args,expected", [
    # gain triplet ch0 at 11 dB: GainA/B = dB-10 in settings 17/16, GainC
    # = dB-9 in setting 3 bits 5:0 (capture 70.689: 0x00FF0001 x2, 0x003F0042
    # with ch1's field from before)
    ((1, 0, 0x0A, 1), W(17, 0x01, 0xFF)),
    ((1, 0, 0x09, 1), W(16, 0x01, 0xFF)),
    ((1, 0, 0x06, 2), W(3, 0x02, 0x3F)),
    ((1, 1, 0x0A, 1), W(17, 0x0100, 0xFF00)),
    ((1, 1, 0x06, 2), W(3, 0x80, 0xFC0)),
    ((1, 0, 0x03, 1), W(0, 0x08, 0x08)),          # 48V on (capture 75.385)
    ((1, 0, 0x03, 0), W(0, 0x00, 0x08)),          # 48V off (78.209)
    ((1, 0, 0x00, 1), W(0, 0x01, 0x01)),          # Line (97.877)
    ((1, 1, 0x01, 1), W(0, 0x80, 0x80)),          # PAD ch1
    ((1, 1, 0x04, 1), W(0, 0x400, 0x400)),        # LowCut ch1
    ((1, 0, 0x05, 0xBF800000), W(0, 0x20, 0x20)),  # Phase invert (float)
    ((1, 0, 0x05, 0x3F800000), W(0, 0x00, 0x20)),
    ((1, 0, 0x05, 1), W(0, 0x20, 0x20)),          # daemon's int form
    ((1, 0, 0x02, 1), W(0, 0x104, 0x104)),        # link from the even channel
    ((1, 0, 0x11, 3), W(13, 0x3, 0xF)),           # HiZ impedance input 1
    ((2, 1, 0x01, 0x2A), W(2, 0x2A, 0xFF)),       # monitor level (82.264)
    ((2, 0, 0x03, 2), W(2, 0x10000, 0x30000)),    # mute
    ((2, 0, 0x03, 1), W(2, 0x20000, 0x30000)),    # MixToMono
    ((2, 0, 0x44, 1), W(2, 0x80000000, 0x80000000)),   # dim
    ((2, 1, 0x02, 0x76), W(2, 0x7600, 0xFF00)),   # HP level field
    ((2, 0, 0x6A, 5), W(35, 5, 0x1F)),            # delay comp (capture 0.088)
    ((2, 0, 0x06, 1), W(2, 0x200000, 0x300000)),  # CUE1 mono, swapped field
    ((2, 0, 0x4E, 0x12), W(33, 0x12000000, 0xFF000000)),   # field run
])
def test_mapping(args, expected):
    assert um.map_param(*args) == expected


def test_mapping_nops_dsp_params_and_errors():
    assert um.map_param(1, 1, 0x02, 1) is None      # odd channel link: no-op
    assert um.map_param(2, 0, 0x16, 1) is None      # DSP spanning: cache only
    assert um.map_param(1, 0, 0x13, 0x1FFFF) == um.DspInputParam(0, 7, 0x1FFFF)
    assert um.map_param(1, 3, 0x16, 0x20) == um.DspInputParam(3, 8, 0x20)
    assert um.map_param(1, 2, 0x05, 1) == um.DspInputParam(2, 5, 0xBF800000)
    for bad in [(1, 2, 0x03, 1), (1, 0, 0x18, 0), (1, 1, 0x11, 1),
                (2, 0, 0x09, 0), (2, 0, 0x8B, 0), (3, 0, 0, 0)]:
        with pytest.raises(um.MixerParamError):
            um.map_param(*bad)
    assert um.map_param(1, 2, 0x03, 1, num_preamps=4) == um.SettingWrite(
        0, 0x8 << 12, 0x8 << 12)


def _c_defines():
    out = {}
    for line in (DRIVER / "ua_apollo.h").read_text().splitlines():
        m = re.match(r"#define\s+(UA_MON_PARAM_\w+)\s+(\w+)", line)
        if m:
            out[m.group(1)] = m.group(2)

    def resolve(name):
        seen = 0
        while not re.fullmatch(r"0x[0-9a-fA-F]+|\d+", name):
            name = out[name]
            seen += 1
            assert seen < 5
        return int(name, 0)
    return resolve


def _c_mask(expr):
    expr = expr.strip()
    m = re.fullmatch(r"BIT\((\d+)\)", expr)
    if m:
        return 1 << int(m.group(1))
    return int(expr, 0)


def test_monitor_mapping_matches_driver_source():
    """Every explicit case of ua_monitor_set_param: same setting and mask."""
    src = (DRIVER / "ua_core.c").read_text()
    body = src[src.index("ua_monitor_set_param(struct ua_device *ua"):]
    body = body[:body.index("\n}\n")]
    resolve = _c_defines()
    checked = 0
    for block in re.split(r"\n\tcase ", body)[1:]:
        label = block.split(":", 1)[0].strip()
        sm = re.search(r"setting_idx = (\d+);", block)
        mm = re.search(r"hw_mask\s*= ([^;]+);", block)
        if not (sm and mm):
            continue
        param = int(label, 0) if label.startswith("0x") else resolve(label)
        res = um.map_monitor(param, 0xFFFFFFFF)
        assert res is not None, hex(param)
        assert (res.setting, res.mask) == (int(sm.group(1)), _c_mask(mm.group(1))), \
            hex(param)
        checked += 1
    assert checked >= 70


def test_monitor_field_runs_match_driver_table():
    src = (DRIVER / "ua_core.c").read_text()
    table = src[src.index("ua_other_fields[] = {"):]
    table = table[:table.index("};")]
    resolve = _c_defines()
    rows = re.findall(r"\{\s*(\w+),\s*(0x[0-9a-f]+),\s*(\d+),\s*(\d+),\s*(\d+),\s*(\d+)\s*\}",
                      table)
    assert len(rows) == 12
    for first, last, setting, shift, stride, width in rows:
        first = resolve(first)
        for p in range(first, int(last, 0) + 1):
            sh = int(shift) + (p - first) * int(stride)
            mask = ((1 << int(width)) - 1) << sh
            assert um.map_monitor(p, 0xFFFFFFFF) == um.SettingWrite(
                int(setting), mask if int(width) > 1 else 1 << sh, mask)
