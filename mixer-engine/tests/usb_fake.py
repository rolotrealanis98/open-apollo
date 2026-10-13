"""A fake Apollo Solo USB for the USB backend and tool tests (no pyusb).

FakeApollo answers the vendor requests of interface 0 the way the Solo USB
does in the Windows captures (tools/usb-re): protocol word 0x00010002,
versions, the status block, the DSP window (boot word, pool layout,
serial), SEQ_WR/SEQ_RD with a settable acknowledge delay, the settings
words and the 40-word readback with its check words.  Bulk OUT frames are
recorded; bulk IN serves queued replies (and code-load acks when
auto_ack is set) and otherwise times out.

FakeUsb is a stand-in for the pyusb modules (usb.core / usb.util).
"""

import struct
import sys

from conftest import ENGINE_DIR

TOOLS = ENGINE_DIR.parent / "tools"
if str(TOOLS) not in sys.path:
    sys.path.append(str(TOOLS))

from uausb import device as ud  # noqa: E402
from uausb import dspcmd as dc  # noqa: E402


class USBError(Exception):
    pass


class USBTimeoutError(USBError):
    pass


def readback_words(base=None, **set_words):
    """40 readback words with valid check words; base = the Solo capture's."""
    words = list(base) if base else [0x08000000, 0x20004182, 0x00007600,
                                     0x00000041, 0, 0, 0xC0000011,
                                     0xA0000000] + [0] * 18 + [0xFEFE] * 3
    words += [0] * (38 - len(words))
    for k, v in set_words.items():
        words[int(k[1:])] = v
    x = 0
    for w in words[:38]:
        x ^= w
    return words[:38] + [x, ~x & 0xFFFFFFFF]


class FakeApollo:
    def __init__(self, pid=0x000D, protocol=0x00010002):
        self.pid = pid
        self.bus, self.address = 3, 7
        self.idProduct = pid
        self.product = "Apollo Solo USB"
        self.protocol = protocol
        self.regs = {ud.SEQ_RD: 6, ud.SEQ_WR: 6}
        # Solo USB DSP window after the DSP started (apollo-init.pcap)
        for a, v in ((0x1069, 0x00000C03), (0x1004, 0x4000),
                     (0x1005, 0xA9B9), (0x1006, 0x1FFF), (0x1007, 0xFF51B2),
                     (0x1061, 0xE0000), (0x1062, 0x92800), (0x1063, 0xC5000),
                     (0x1064, 0x08008E4E), (0x1065, 0x80), (0x1066, 0x80),
                     (0x1067, 0x8000), (0x0C00, 0xA90EC5AC),
                     (0x0C01, 0x81410111)):
            self.regs[a] = v
        for a, raw in ((0x1008, "32303030"), (0x1009, "30303030"),
                       (0x100A, "30303030"), (0x100B, "30310000")):
            self.regs[a] = struct.unpack("<I", bytes.fromhex(raw))[0]
        self.boot_words = None          # list served for 0x1069, last repeats
        self.ack_delay = 0              # SEQ_RD reads before the DSP acks
        self._ack_left = 0
        self.readback = readback_words()
        self.status = bytes.fromhex("4a4b2b2a0200000080bb0000030000000a000000"
                                    "06000000000000000000000000000000")
        self.ctrl_log = []              # every control transfer
        self.writes = []                # (word address, [words]) register writes
        self.bulk_out = []              # bytes of every bulk OUT frame
        self.bulk_in = []               # queued bulk IN replies (bytes)
        self.auto_ack = False           # ack every LOAD_CODE seen on bulk OUT
        self.fail_requests = set()      # (request, wValue) that raise USBError
        self.gone = False
        self.released = False
        self._stream = []               # words of the OUT stream not parsed yet
        self._rseq = 0x47

    # pyusb device surface

    def ctrl_transfer(self, bm, req, wvalue, windex, data_or_len, timeout=None):
        self.ctrl_log.append((bm, req, wvalue, windex,
                              data_or_len if isinstance(data_or_len, int)
                              else bytes(data_or_len)))
        if self.gone:
            raise USBError("no such device")
        if (req, wvalue) in self.fail_requests:
            raise USBError("pipe stall")
        if bm == 0xC1:
            return bytearray(self._vendor_in(req, wvalue, data_or_len))
        if bm == 0x41:
            if req != ud.REQ_WRITE:
                raise AssertionError(f"unexpected vendor OUT {req:#x}")
            words = list(struct.unpack(f"<{len(data_or_len) // 4}I",
                                       bytes(data_or_len)))
            self.writes.append((wvalue, words))
            for i, w in enumerate(words):
                self.regs[wvalue + i] = w
            if wvalue == ud.SEQ_WR:
                self._ack_left = self.ack_delay
                if not self.ack_delay:
                    self.regs[ud.SEQ_RD] = words[0]
            return len(data_or_len)
        if bm == 0x21:                  # UAC2 SET_CUR
            self.uac2_rate = struct.unpack("<I", bytes(data_or_len))[0]
            return 4
        if bm == 0xA1:                  # UAC2 GET_CUR
            return bytearray(struct.pack("<I", getattr(self, "uac2_rate", 48000)))
        raise AssertionError(f"unexpected control {bm:#x}/{req:#x}")

    def _vendor_in(self, req, wvalue, length):
        if req == ud.REQ_PROTOCOL:
            return struct.pack("<I", self.protocol)
        if req == ud.REQ_VERSION_A:
            return struct.pack("<I", 0x01030003)
        if req == ud.REQ_VERSION_B:
            return struct.pack("<I", 0x010303EC)
        if req == ud.REQ_STATUS:
            return self.status
        if req == ud.REQ_READBACK:
            return struct.pack("<40I", *self.readback)
        if req == ud.REQ_READ:
            out = b""
            for a in range(wvalue, wvalue + length // 4):
                out += struct.pack("<I", self._reg_read(a))
            return out
        raise AssertionError(f"unexpected vendor IN {req:#x}")

    def _reg_read(self, a):
        if a == 0x1069 and self.boot_words:
            return self.boot_words.pop(0) if len(self.boot_words) > 1 \
                else self.boot_words[0]
        if a == ud.SEQ_RD and self._ack_left:
            self._ack_left -= 1
            if not self._ack_left:
                self.regs[ud.SEQ_RD] = self.regs[ud.SEQ_WR]
                return self.regs[ud.SEQ_WR]
        if ud.READBACK_DATA <= a < ud.READBACK_DATA + 40:
            return self.readback[a - ud.READBACK_DATA]
        return self.regs.get(a, 0)

    def write(self, ep, data, timeout=None):
        if self.gone:
            raise USBError("no such device")
        assert ep == 0x01
        data = bytes(data)
        self.bulk_out.append(data)
        if self.auto_ack:
            self._ack_loads(data)
        return len(data)

    def _ack_loads(self, frame):
        f = dc.Frame.from_bytes(frame)
        self._stream += f.words
        while self._stream:
            n = self._stream[0] & 0xFFFF
            if len(self._stream) < n:
                return
            cmd, self._stream = self._stream[:n], self._stream[n:]
            reply = None
            if cmd[0] >> 16 == dc.OP_LOAD_CODE:
                reply = [0x80070004, 0, cmd[3] & 0xFFFF, cmd[0]]
            elif cmd[0] >> 16 == dc.OP_QUERY_11:
                reply = [0x80030003, cmd[1], 0]      # echoes the tag
            if reply:
                self.bulk_in.append(bytes((len(reply), 0, self._rseq & 0xFF,
                                           0xDD))
                                    + struct.pack(f"<{len(reply)}I", *reply))
                self._rseq += 1

    def read(self, ep, size, timeout=None):
        if self.gone:
            raise USBError("no such device")
        if ep == 0x81 and self.bulk_in:
            return bytearray(self.bulk_in.pop(0))
        raise USBTimeoutError("timeout")

    def release(self):
        self.released = True

    # pyusb device methods the tools call
    def is_kernel_driver_active(self, intf):
        return False

    def detach_kernel_driver(self, intf):
        pass


class FakeUsb:
    """usb.core + usb.util stand-in holding one FakeApollo."""

    def __init__(self, dev):
        outer = self
        self.dev = dev
        self.claimed = []
        self.released = []
        self.halts = []

        class core:
            USBError = USBError
            USBTimeoutError = USBTimeoutError

            @staticmethod
            def find(idVendor=None, idProduct=None):
                d = outer.dev
                return d if d is not None and d.pid == idProduct else None

        class util:
            @staticmethod
            def claim_interface(d, i):
                outer.claimed.append(i)

            @staticmethod
            def release_interface(d, i):
                outer.released.append(i)

            @staticmethod
            def clear_halt(d, ep):
                outer.halts.append(ep)

        self.core = core
        self.util = util
