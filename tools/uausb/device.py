"""Vendor control requests of the Apollo USB interface 0.

Every request is bmRequestType 0x41 (OUT) or 0xC1 (IN): vendor, recipient
interface, wIndex 0, wValue the argument.  The map is from the Windows
UAD2Usb.sys (call sites in apollo-linux parity/05-windows.md section 3.3,
re-read 2026-10-11) and the Solo USB captures in tools/usb-re:

  0x00 IN  4    protocol word; 0x00010002 on the Solo.  The Windows driver
                reads the 40-word readback with request 0x10 when it is
                >= 0x00010002, else through 0x0604/0x0605 (UAD2Usb.sys
                0x140007dec, 0x140007ca0, 0x140008110)
  0x03 OUT 4n   register write, wValue = FPGA word address
  0x04 IN  4n   register read, wValue = FPGA word address
  0x05 OUT 0    sample-rate index in wValue (< 10; 2 = 48 kHz seen)   [never sent here]
  0x06 IN  36   status block, magic 0x2A2B4B4A (see status_block())
  0x07 OUT 0    clock source code in wValue (< 13; 0x0C Internal)     [never sent here]
  0x08 IN  44   clock source entry wValue (see clock_entry())
  0x09 OUT 0    device reset: what DeviceRequest sub-command 0x11
                (ResetDevice of the macOS CUAD2UsbDevice) sends
                (UAD2Usb.sys 0x140005ab6)                             [never sent here]
  0x0C/0x0D OUT 0  a value pair sent together (277 then 16 on the Solo);
                the device then reports new UAC2 terminal latencies   [never sent here]
  0x0E IN  4    version word (0x01030003 on the Solo: "Cauldron 1.3 build 3")
  0x0F IN  4    version word (0x010303EC on the Solo); 0x0E, 0x00 and 0x0F
                are the three words of DeviceRequest sub-command 0x12
                (GetCyfx3Versions, UAD2Usb.sys 0x140007100)
  0x10 IN  160  the 40-word mixer readback (PCIe BAR0 0x3814 block)

FPGA word addresses.  The mixer block is the PCIe 0x3800 block one dword per
address: 0x0602 SEQ_WR (PCIe 0x3808), 0x0603 SEQ_RD (0x380C), 0x0604
readback status (0x3810), 0x0605.. readback data (0x3814), settings from
0x062D (0x38B4).  Other PCIe register offsets map through
usb_word_address() (UAD2Usb.sys 0x1400078a8): the DSP windows (BAR0
0x0000-0x1FFF) land at 0x1000 + offset/4, so DSP 0's boot word (window
+0x1A4, the kext's _waitFor469ToStart word) is 0x1069.  An offset of
0x10000 or more is a raw FPGA word address (offset - 0x10000): that is the
"+0x10000" of the macOS CUAD2UsbDevice shim.

Writes this module makes: register writes (0x03) to the mixer block and
the readback status word only.  No EEPROM, flash, FX3, rate, clock or CLI
request is built anywhere in uausb.

UNTESTED ON HARDWARE.  The request numbers, addresses and the DSP-ready
rule are read from UAD2Usb.sys and checked against the Windows captures;
nothing here has been run against a USB Apollo from Linux.
"""

from __future__ import annotations

import struct
import time
from dataclasses import dataclass

REQ_TYPE_OUT = 0x41
REQ_TYPE_IN = 0xC1

REQ_PROTOCOL = 0x00
REQ_WRITE = 0x03
REQ_READ = 0x04
REQ_SET_RATE_INDEX = 0x05       # documented, not sent
REQ_STATUS = 0x06
REQ_SET_CLOCK = 0x07            # documented, not sent
REQ_CLOCK_ENTRY = 0x08
REQ_09 = 0x09                   # documented, not sent
REQ_PAIR_A = 0x0C               # documented, not sent
REQ_PAIR_B = 0x0D               # documented, not sent
REQ_VERSION_A = 0x0E
REQ_VERSION_B = 0x0F
REQ_READBACK = 0x10

# Requests a tool may issue.  Everything else is refused by UsbRegs.
_ALLOWED_IN = {REQ_PROTOCOL, REQ_READ, REQ_STATUS, REQ_CLOCK_ENTRY,
               REQ_VERSION_A, REQ_VERSION_B, REQ_READBACK}
_ALLOWED_OUT = {REQ_WRITE}

# FPGA word addresses
MIXER_BLOCK = 0x0600
SEQ_WR = 0x0602
SEQ_RD = 0x0603
READBACK_STATUS = 0x0604
READBACK_DATA = 0x0605
READBACK_WORDS = 40
ID_WORD = 0x0C00                # compared across power cycles by D0Entry
ID_VERSION = 0x0C01
# ARM CLI mailbox, the PCIe 0xC3F4.. block at word (offset / 4) - 0x2800:
CLI_FLAG = 0x08FD               # PCIe 0xC3F4
CLI_STATUS = 0x08FE             # PCIe 0xC3F8 (command length = trigger)
CLI_RESP_LEN = 0x08FF           # PCIe 0xC3FC
CLI_CMD_BUF = 0x0900            # PCIe 0xC400, 32 words
CLI_RESP_BUF = 0x0920           # PCIe 0xC480, up to 75 words
# The 38 settings: three groups at the PCIe offsets 0xB4/0x13C/0x1C0 of
# the mixer block, 8 bytes per setting (UAD2Usb.sys 0x14000839c)
SETTINGS_GROUPS = ((0, 16, 0x062D), (16, 32, 0x064F), (32, 38, 0x0670))
# The only words UsbRegs.write() touches.  Never written by this code: the
# CLI block (it froze the ARM MCU on the x4) and everything else.
_WRITABLE = {SEQ_WR, READBACK_STATUS}
for _first, _end, _addr in SETTINGS_GROUPS:
    _WRITABLE |= set(range(_addr, _addr + 2 * (_end - _first)))
del _first, _end, _addr

# DSP window registers (word offsets inside a window, = PCIe byte offset / 4)
WIN_STATUS = 0x00
WIN_POOL_SIZE = (0x04, 0x05, 0x06, 0x07)      # pools 0-3 (+0x10..+0x1C)
WIN_SERIAL = (0x08, 0x09, 0x0A, 0x0B)         # +0x20..+0x2C, 16 ASCII bytes
WIN_POOL_BASE = (0x61, 0x62, 0x63, 0x64)      # +0x184..+0x190
WIN_POOL_SCRATCH = (None, 0x65, 0x66, 0x67)   # +0x194..+0x19C
WIN_BOOT_WORD = 0x69                          # +0x1A4

# DSP boot word: the DSP has not started while it reads 0 or this value
# (UAD2Usb.sys 0x140008fe8; the PCIe kext uses 0xA8CAED0F instead)
BOOT_NOT_STARTED = 0xF81A3C5E
BOOT_POLL_INTERVAL = 0.3        # 300 ms per try, as Windows
BOOT_TRIES_DSP0 = 60            # Windows: 60 tries for DSP 0, 10 for others
BOOT_TRIES_OTHER = 10

STATUS_MAGIC = 0x2A2B4B4A
CLOCK_ENTRY_MAGIC = 0x3B3C4B4A
PROTOCOL_DIRECT_READBACK = 0x00010002


def usb_word_address(pcie_offset: int) -> int:
    """FPGA word address of a PCIe BAR0 byte offset (UAD2Usb.sys's map).

    >= 0x10000: a raw word address (offset - 0x10000).  BAR0 0x0000-0x1FFF
    (the DSP windows): 0x1000 + offset/4.  0x8800-0x8FF8: offset/4 - 0x1600.
    0x4000-0x5FFC: offset/4 + 0x800.  Anything else: offset/4.  The mixer
    block does not go through this map (its own addresses are fixed).
    """
    if pcie_offset >= 0x10000:
        return pcie_offset - 0x10000
    w = pcie_offset >> 2
    if w < 0x800:
        return w + 0x1000
    if 0 <= w - 0x2200 <= 0x1FE:
        return w - 0x1600
    if 0 <= w - 0x1000 <= 0x7FF:
        return w + 0x800
    return w


def pcie_dsp_window(dsp: int) -> int:
    """BAR0 byte offset of DSP `dsp`'s register window (kext ProgramRegisters)."""
    return dsp * 0x800 + (0x2000 if dsp > 3 else 0)


def dsp_window(dsp: int) -> int:
    """FPGA word address of DSP `dsp`'s window (0x1000 for DSP 0)."""
    return usb_word_address(pcie_dsp_window(dsp))


class UsbRequestRefused(RuntimeError):
    """A request this module never sends."""


class UsbRegs:
    """Vendor requests on a pyusb-style device (ctrl_transfer)."""

    def __init__(self, dev, timeout_ms: int = 500):
        self.dev = dev
        self.timeout_ms = timeout_ms

    # raw requests

    def vendor_in(self, request: int, wvalue: int, length: int) -> bytes:
        if request not in _ALLOWED_IN:
            raise UsbRequestRefused(f"IN request {request:#04x} is not issued")
        data = self.dev.ctrl_transfer(REQ_TYPE_IN, request, wvalue & 0xFFFF,
                                      0, length, timeout=self.timeout_ms)
        return bytes(data)

    def vendor_out(self, request: int, wvalue: int, data: bytes) -> None:
        if request not in _ALLOWED_OUT:
            raise UsbRequestRefused(f"OUT request {request:#04x} is not issued")
        self.dev.ctrl_transfer(REQ_TYPE_OUT, request, wvalue & 0xFFFF, 0,
                               bytes(data), timeout=self.timeout_ms)

    # registers

    def read(self, addr: int, count: int = 1) -> list:
        raw = self.vendor_in(REQ_READ, addr, 4 * count)
        if len(raw) < 4 * count:
            raise IOError(f"register read {addr:#06x}: {len(raw)} of "
                          f"{4 * count} bytes")
        return list(struct.unpack_from(f"<{count}I", raw))

    def read1(self, addr: int) -> int:
        return self.read(addr, 1)[0]

    def write(self, addr: int, words) -> None:
        words = [int(w) & 0xFFFFFFFF for w in words]
        last = addr + len(words) - 1
        if not words or any(a not in _WRITABLE for a in range(addr, last + 1)):
            raise UsbRequestRefused(
                f"register write {addr:#06x}..{last:#06x} is outside the "
                "mixer settings / SEQ_WR / readback status words")
        self.vendor_out(REQ_WRITE, addr, struct.pack(f"<{len(words)}I", *words))

    # info blocks (read-only)

    def protocol_word(self) -> int:
        raw = self.vendor_in(REQ_PROTOCOL, 0, 4)
        return struct.unpack_from("<I", raw)[0]

    def versions(self) -> dict:
        out = {}
        for key, req in (("protocol", REQ_PROTOCOL), ("version_a", REQ_VERSION_A),
                         ("version_b", REQ_VERSION_B)):
            raw = self.vendor_in(req, 0, 4)
            out[key] = struct.unpack_from("<I", raw)[0]
        return out

    def status_block(self) -> dict | None:
        return decode_status_block(self.vendor_in(REQ_STATUS, 0, 36))

    def clock_entry(self, index: int) -> dict | None:
        return decode_clock_entry(self.vendor_in(REQ_CLOCK_ENTRY, index, 44))

    def clock_menu(self, count: int | None = None) -> list:
        """[(source code, name)], `count` entries (default: the status
        block's clock-source count)."""
        if count is None:
            st = self.status_block()
            count = st["clock_sources"] if st else 0
        menu = []
        for i in range(min(count, 16)):
            e = self.clock_entry(i)
            if e is None or e["index"] != i:
                break
            menu.append((e["code"], e["name"]))
        return menu

    def window_word(self, dsp: int, word: int) -> int:
        return self.read1(dsp_window(dsp) + word)

    def serial(self, dsp: int = 0) -> str | None:
        """The 16-byte ASCII serial at DSP window +0x20 (PCIe BAR0 0x20 on
        DSP 0, ua_serial_lookup); None while the DSP has not written it."""
        words = self.read(dsp_window(dsp) + WIN_SERIAL[0], 4)
        raw = struct.pack("<4I", *words)
        text = raw.split(b"\0", 1)[0].decode("ascii", "replace").strip()
        return text or None

    def memory_map(self, dsp: int = 0) -> dict:
        """The 11 pool words CPcieDSP::GetProperty(6) reads, as a dict."""
        base = dsp_window(dsp)
        return {
            "sizes": tuple(self.read1(base + w) for w in WIN_POOL_SIZE),
            "bases": tuple(self.read1(base + w) for w in WIN_POOL_BASE),
            "scratch": (0,) + tuple(self.read1(base + w)
                                    for w in WIN_POOL_SCRATCH[1:]),
        }

    # readback

    def readback(self, protocol: int | None = None) -> tuple:
        """(status, 40 words) as GET_HW_READBACK gives them on PCIe.

        status 1: a fresh snapshot whose check words hold (word 38 = XOR of
        words 0-37, word 39 = its complement: every one of the 7936
        snapshots in the Solo USB captures does).  status 0: no snapshot
        (old protocol, nothing ready) or a bad check; the words are then
        what was read, or [].
        """
        if protocol is None or protocol >= PROTOCOL_DIRECT_READBACK:
            raw = self.vendor_in(REQ_READBACK, 0, 4 * READBACK_WORDS)
            if len(raw) < 4 * READBACK_WORDS:
                return 0, []
            words = list(struct.unpack_from(f"<{READBACK_WORDS}I", raw))
        else:
            # Older protocol: status word, block read, re-arm (0x140008110)
            if self.read1(READBACK_STATUS) != 1:
                return 0, []
            words = self.read(READBACK_DATA, READBACK_WORDS)
            self.write(READBACK_STATUS, [0])
        return (1 if readback_check_ok(words) else 0), words


def readback_check_ok(words) -> bool:
    if len(words) < READBACK_WORDS:
        return False
    x = 0
    for w in words[:38]:
        x ^= w
    return words[38] == x and words[39] == (~x & 0xFFFFFFFF)


def decode_status_block(raw: bytes) -> dict | None:
    """Request 0x06: {magic, rate index, rate Hz, clock-source count,
    record channels, playback channels, 3 words 0}.

    Solo USB: {0, 0, 3, 10, 6} at bring-up, {2, 48000, 3, 10, 6} after the
    Windows driver set the rate (request 0x05 wValue 2) and the clock
    (request 0x07 wValue 0x0C).  The field names are read from those values.
    """
    if len(raw) < 24:
        return None
    w = struct.unpack_from(f"<{len(raw) // 4}I", raw)
    if w[0] != STATUS_MAGIC:
        return None
    return {"rate_index": w[1], "rate": w[2], "clock_sources": w[3],
            "record_channels": w[4], "playback_channels": w[5],
            "words": list(w)}


def decode_clock_entry(raw: bytes) -> dict | None:
    """Request 0x08: {magic, index, source code, NUL-terminated name}.

    Solo USB: 0 Internal (0x0C), 1 S/PDIF (0x00), 2 ADAT (0x05): the same
    codes as the PCIe clock sources.  The Solo USB has no digital input; the
    model's own list (devices/apollo-solo-usb.json: Internal) is the one to
    offer.
    """
    if len(raw) < 12:
        return None
    magic, index, code = struct.unpack_from("<3I", raw)
    if magic != CLOCK_ENTRY_MAGIC:
        return None
    name = raw[12:].split(b"\0", 1)[0].decode("ascii", "replace").strip()
    return {"index": index, "code": code, "name": name}


@dataclass
class ReadyResult:
    """Outcome of wait_dsp_started()."""

    started: bool               # the boot word left 0/BOOT_NOT_STARTED
    ok: bool                    # started and bit 0 set (Windows' success rule)
    value: int | None           # last boot word read
    tries: int                  # reads made
    error: str | None = None    # the read failed: fall back to a fixed wait


def wait_dsp_started(regs: UsbRegs, dsp: int = 0, tries: int | None = None,
                     interval: float = BOOT_POLL_INTERVAL,
                     sleep=None, log=None) -> ReadyResult:
    """Poll the DSP boot word until the DSP has started (USB _waitFor469ToStart).

    UAD2Usb.sys 0x140008fe8, called from the USB ProgramRegisters
    (0x140008b91): read window +0x69 (request 0x04, wValue 0x1069 for DSP
    0); while it reads 0 or 0xF81A3C5E sleep 300 ms and try again, 60 tries
    for DSP 0 and 10 for the others; then bit 0 must be set, else the
    driver fails the DSP with -21.  The Windows bring-up capture
    (tools/usb-re/apollo-init.pcap) reads 0 four times, then 0x00000C03,
    1.26 s after enumeration, and only then sends the connect command.

    A failed read returns error= set: the caller falls back to its fixed
    wait.  The read itself is the only request made.
    """
    if tries is None:
        tries = BOOT_TRIES_DSP0 if dsp == 0 else BOOT_TRIES_OTHER
    sleep = sleep or time.sleep
    addr = dsp_window(dsp) + WIN_BOOT_WORD
    value = None
    n = 0
    for n in range(1, tries + 1):
        try:
            value = regs.read1(addr)
        except Exception as e:  # noqa: BLE001 - any transport error -> fallback
            return ReadyResult(False, False, value, n, f"{type(e).__name__}: {e}")
        if value not in (0, BOOT_NOT_STARTED):
            break
        if log:
            log(f"DSP {dsp} boot word {value:#010x} (try {n}/{tries}): not started")
        if n < tries:
            sleep(interval)
    started = value not in (None, 0, BOOT_NOT_STARTED)
    return ReadyResult(started, started and bool(value & 1), value, n)
