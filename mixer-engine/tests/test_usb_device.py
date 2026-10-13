"""J3/J5/U36: vendor requests, the PCIe->USB register map, the DSP-ready
poll and the readback, against a fake pyusb device.  Values are the ones
of the Solo USB captures in tools/usb-re.  No hardware."""

import struct
import sys

import pytest

from conftest import ENGINE_DIR

TOOLS = ENGINE_DIR.parent / "tools"
if str(TOOLS) not in sys.path:
    sys.path.append(str(TOOLS))

from uausb import device as ud  # noqa: E402


class FakeCtrl:
    """pyusb-style ctrl_transfer recorder with scripted IN answers."""

    def __init__(self):
        self.calls = []
        self.regs = {}            # word address -> value or list (served in turn)
        self.answers = {}         # (request, wValue) -> bytes
        self.fail = set()         # (request, wValue) that raise

    def ctrl_transfer(self, bm, req, wvalue, windex, data_or_len, timeout=None):
        self.calls.append((bm, req, wvalue, windex, data_or_len))
        if (req, wvalue) in self.fail:
            raise IOError("pipe stall")
        if bm & 0x80:
            if (req, wvalue) in self.answers:
                return bytearray(self.answers[(req, wvalue)])
            if req == ud.REQ_READ:
                out = b""
                for a in range(wvalue, wvalue + data_or_len // 4):
                    v = self.regs.get(a, 0)
                    if isinstance(v, list):
                        v = v.pop(0) if len(v) > 1 else v[0]
                    out += struct.pack("<I", v)
                return bytearray(out)
            return bytearray(data_or_len)
        if req == ud.REQ_WRITE:
            n = len(data_or_len) // 4
            for i, w in enumerate(struct.unpack(f"<{n}I", data_or_len)):
                self.regs[wvalue + i] = w
        return len(data_or_len)


# Solo USB answers from tools/usb-re/apollo-init.pcap
STATUS_BOOT = bytes.fromhex("4a4b2b2a0000000000000000030000000a000000"
                            "06000000000000000000000000000000")
STATUS_48K = bytes.fromhex("4a4b2b2a0200000080bb0000030000000a000000"
                           "06000000000000000000000000000000")
CLOCK_0 = bytes.fromhex("4a4b3c3b000000000c000000496e7465726e616c00000000"
                        "532f5044494600004144415400000000436c6f63")
CLOCK_1 = bytes.fromhex("4a4b3c3b0100000000000000532f504449460000"
                        "4144415400000000436c6f636b3400000000000000000000")
CLOCK_2 = bytes.fromhex("4a4b3c3b02000000050000004144415400000000"
                        "436c6f636b34000000000000000000000000000000000000")


def test_usb_word_address_map():
    assert ud.usb_word_address(0x1A4) == 0x1069         # DSP 0 boot word
    assert ud.usb_word_address(0x0000) == 0x1000
    assert ud.usb_word_address(0x0020) == 0x1008        # serial
    assert ud.usb_word_address(0x0184) == 0x1061        # pool 0 base
    assert ud.usb_word_address(0x0800) == 0x1200        # DSP 1 window
    assert ud.usb_word_address(0x10603) == 0x0603       # raw (+0x10000)
    assert ud.usb_word_address(0x8800) == 0x0C00
    assert ud.usb_word_address(0x4000) == 0x1800
    assert ud.usb_word_address(0x3800) == 0x0E00        # not the mixer block
    assert [ud.dsp_window(i) for i in range(5)] == [0x1000, 0x1200, 0x1400,
                                                     0x1600, 0x1800]


def test_reads_and_writes_use_vendor_interface_requests():
    dev = FakeCtrl()
    dev.regs[ud.SEQ_RD] = 7
    r = ud.UsbRegs(dev)
    assert r.read1(ud.SEQ_RD) == 7
    assert dev.calls[-1] == (0xC1, 0x04, 0x0603, 0, 4)
    r.write(ud.SEQ_WR, [8])
    assert dev.calls[-1] == (0x41, 0x03, 0x0602, 0, struct.pack("<I", 8))
    r.write(0x062D, [0] * 32)
    r.write(0x064F, [0] * 32)
    r.write(0x0670, [0] * 12)


@pytest.mark.parametrize("addr,n", [(0x0900, 1), (0x08FE, 1), (0x08FF, 1),
                                    (0x064D, 1), (0x066F, 1), (0x0670, 13),
                                    (0x1069, 1), (0x0C00, 1), (0x0603, 1)])
def test_writes_outside_settings_are_refused(addr, n):
    dev = FakeCtrl()
    with pytest.raises(ud.UsbRequestRefused):
        ud.UsbRegs(dev).write(addr, [0] * n)
    assert dev.calls == []


@pytest.mark.parametrize("req", [0x05, 0x07, 0x09, 0x0C, 0x0D, 0xA0, 0x0A])
def test_unsent_requests_are_refused(req):
    dev = FakeCtrl()
    r = ud.UsbRegs(dev)
    with pytest.raises(ud.UsbRequestRefused):
        r.vendor_out(req, 0, b"")
    assert dev.calls == []


def test_info_blocks_decode():
    dev = FakeCtrl()
    dev.answers[(ud.REQ_STATUS, 0)] = STATUS_48K
    dev.answers[(ud.REQ_CLOCK_ENTRY, 0)] = CLOCK_0
    dev.answers[(ud.REQ_CLOCK_ENTRY, 1)] = CLOCK_1
    dev.answers[(ud.REQ_CLOCK_ENTRY, 2)] = CLOCK_2
    dev.answers[(ud.REQ_PROTOCOL, 0)] = bytes.fromhex("02000100")
    dev.answers[(ud.REQ_VERSION_A, 0)] = bytes.fromhex("03000301")
    dev.answers[(ud.REQ_VERSION_B, 0)] = bytes.fromhex("ec030301")
    r = ud.UsbRegs(dev)
    st = r.status_block()
    assert (st["rate_index"], st["rate"], st["clock_sources"],
            st["record_channels"], st["playback_channels"]) == (2, 48000, 3, 10, 6)
    boot = ud.decode_status_block(STATUS_BOOT)
    assert (boot["rate_index"], boot["rate"], boot["clock_sources"],
            boot["record_channels"], boot["playback_channels"]) == (0, 0, 3, 10, 6)
    assert r.clock_menu() == [(0x0C, "Internal"), (0x00, "S/PDIF"), (0x05, "ADAT")]
    assert r.versions() == {"protocol": 0x00010002, "version_a": 0x01030003,
                            "version_b": 0x010303EC}
    assert ud.decode_status_block(b"\0" * 36) is None
    assert ud.decode_clock_entry(b"\0" * 44) is None


def test_serial_and_memory_map_from_dsp_window():
    dev = FakeCtrl()
    # the DSP window of apollo-init.pcap after the DSP started (a synthetic
    # serial in place of the captured unit's)
    for a, raw in ((0x1008, "32303030"), (0x1009, "30303030"),
                   (0x100A, "30303030"), (0x100B, "30310000")):
        dev.regs[a] = struct.unpack("<I", bytes.fromhex(raw))[0]
    for a, v in ((0x1004, 0x4000), (0x1005, 0xA9B9), (0x1006, 0x1FFF),
                 (0x1007, 0xFF51B2), (0x1061, 0xE0000), (0x1062, 0x92800),
                 (0x1063, 0xC5000), (0x1064, 0x08008E4E), (0x1065, 0x80),
                 (0x1066, 0x80), (0x1067, 0x8000)):
        dev.regs[a] = v
    r = ud.UsbRegs(dev)
    assert r.serial(0) == "20000000000001"
    mm = r.memory_map(0)
    from uausb import soloinit
    assert soloinit.MemoryMap.from_dict(mm) == soloinit.SOLO_USB_MEMORY_MAP
    dev.regs.update({0x1008: 0, 0x1009: 0, 0x100A: 0, 0x100B: 0})
    assert r.serial(0) is None


def test_ready_poll_follows_windows_capture():
    """apollo-init.pcap: 0 four times, then 0x00000C03 (bit 0 set)."""
    dev = FakeCtrl()
    dev.regs[0x1069] = [0, 0, 0, 0, 0x00000C03]
    sleeps = []
    res = ud.wait_dsp_started(ud.UsbRegs(dev), 0, sleep=sleeps.append)
    assert res.started and res.ok and res.value == 0xC03 and res.tries == 5
    assert sleeps == [0.3] * 4
    assert all(c[:3] == (0xC1, 0x04, 0x1069) for c in dev.calls)


def test_ready_poll_magic_means_not_started():
    dev = FakeCtrl()
    dev.regs[0x1069] = [ud.BOOT_NOT_STARTED, ud.BOOT_NOT_STARTED, 0x3]
    res = ud.wait_dsp_started(ud.UsbRegs(dev), 0, sleep=lambda s: None)
    assert res.started and res.tries == 3


def test_ready_poll_timeout_and_bit0():
    dev = FakeCtrl()
    dev.regs[0x1069] = [0]
    sleeps = []
    res = ud.wait_dsp_started(ud.UsbRegs(dev), 0, sleep=sleeps.append)
    assert not res.started and not res.ok and res.tries == 60
    assert len(sleeps) == 59
    dev.regs[0x1069] = [0x2]                # started, bit 0 clear
    res = ud.wait_dsp_started(ud.UsbRegs(dev), 0, sleep=lambda s: None)
    assert res.started and not res.ok


def test_ready_poll_other_dsp_and_read_error():
    dev = FakeCtrl()
    dev.regs[0x1269] = [0]
    res = ud.wait_dsp_started(ud.UsbRegs(dev), 1, sleep=lambda s: None)
    assert res.tries == 10 and dev.calls[0][2] == 0x1269
    dev = FakeCtrl()
    dev.fail.add((0x04, 0x1069))
    res = ud.wait_dsp_started(ud.UsbRegs(dev), 0, sleep=lambda s: None)
    assert res.error and not res.started and res.tries == 1


def _rb_words(base):
    words = list(base) + [0] * (38 - len(base))
    x = 0
    for w in words:
        x ^= w
    return words + [x, ~x & 0xFFFFFFFF]


SOLO_RB = _rb_words([0x08000000, 0x20004182, 0x00007600, 0x00000041, 0, 0,
                     0xC0000011, 0xA0000000] + [0] * 18 + [0xFEFE] * 3)


def test_readback_direct_protocol():
    assert SOLO_RB[38] == 0x4800C92C          # the captured check words
    dev = FakeCtrl()
    dev.answers[(ud.REQ_READBACK, 0)] = struct.pack("<40I", *SOLO_RB)
    status, words = ud.UsbRegs(dev).readback(0x00010002)
    assert status == 1 and words == SOLO_RB
    assert dev.calls == [(0xC1, 0x10, 0, 0, 160)]
    bad = list(SOLO_RB)
    bad[2] ^= 1
    dev.answers[(ud.REQ_READBACK, 0)] = struct.pack("<40I", *bad)
    assert ud.UsbRegs(dev).readback()[0] == 0


def test_readback_old_protocol_reads_block_and_rearms():
    dev = FakeCtrl()
    dev.regs[ud.READBACK_STATUS] = 1
    for i, w in enumerate(SOLO_RB):
        dev.regs[ud.READBACK_DATA + i] = w
    status, words = ud.UsbRegs(dev).readback(0x00010001)
    assert status == 1 and words == SOLO_RB
    assert dev.calls[0] == (0xC1, 0x04, 0x0604, 0, 4)
    assert dev.calls[1] == (0xC1, 0x04, 0x0605, 0, 160)
    assert dev.calls[2] == (0x41, 0x03, 0x0604, 0, struct.pack("<I", 0))
    dev.calls.clear()
    assert ud.UsbRegs(dev).readback(0x00010001) == (0, [])
    assert len(dev.calls) == 1


def test_readback_check_words():
    assert ud.readback_check_ok(SOLO_RB)
    assert not ud.readback_check_ok(SOLO_RB[:39])
    assert not ud.readback_check_ok([0xFFFFFFFF] * 40)
