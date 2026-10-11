"""The DSP command language and the USB bulk framing around it.

One format serves the PCIe command ring, the DMA pages it points to and the
USB bulk pipe: every command starts with

    word0 = (opcode << 16) | length_in_dwords        (length counts word0)

followed by length - 1 argument words.  The opcode table below is the one
driver/ua_dsp.c and apollo-linux docs/re-notes/macos-dsp-module-load.md use
(kext 12.0.0 producers named there).

On USB (interface 0, bulk OUT 0x01 / IN 0x81) the commands travel in frames:

    byte 0  word count (dwords after the 4-byte header, at most 255)
    byte 1  flags (0 on the Solo USB; the Twin USB reports use 0x10 too,
            meaning not established)
    byte 2  sequence number, +1 per frame, wrapping at 256
    byte 3  0xDC host->device, 0xDD device->host

Bytes 0-1 read as a little-endian u16 are the word count whenever the flags
byte is 0, which is how the other notes describe the header.  A command may
continue into the next frame; the frames are one dword stream.

Checked against: tools/usb-re/init-bulk-sequence.bin (38 frames, a Windows
capture of a Solo USB) decodes completely into 128 commands and re-encodes
byte for byte (mixer-engine/tests/test_usb_dspcmd.py).  It holds no connect
or rate-ack command (those come before it in a Windows session; the
Cauldron 1.3 capture behind tools/usb-dsp-init.py has them).  The EEPROM
write of DeviceRequest sub-command 0x0F is DSP command 0x13 on PCIe and on
USB alike (UAD2Usb.sys 0x140005afc); it is decoded here, never built.  The device answers
code loads with {0x80070004, status, module id, the load's word0} and the
0x11 query with an 0x8003 reply (tools/usb-re captures, read 2026-10-11).
"""

from __future__ import annotations

import struct
from dataclasses import dataclass, field
from typing import Iterable, Sequence

FRAME_MAGIC_OUT = 0xDC
FRAME_MAGIC_IN = 0xDD
MAX_FRAME_WORDS = 255

# opcode: (name, fixed length or None, what it is)
OPCODES = {
    0x01: ("LOAD_CODE", None, "load a code resource into pool 0: {dest, Bill blob}"),
    0x03: ("RELEASE", 2, "release a shared resource in pool 0: {module id}"),
    0x04: ("LOAD_DATA", None, "load a data resource into pools 1-3: {dest, blob}"),
    0x05: ("MOVE_SHARED", 4, "move a shared resource: {id, pool, new address}"),
    0x06: ("RELEASE_POOL", 3, "release a shared resource in pools 1-3: {id, pool}"),
    0x07: ("OVERWRITE", None, "relocatable write: {address, words}"),
    0x08: ("ZERO_FILL", 4, "zero private memory ('SRAM_CFG'): {address, 0, dwords}"),
    0x09: ("MOVE_PRIVATE", 4, "move a private block: {old, new, size}"),
    0x10: ("FINALIZE", 2, "connect finalize: {date word}"),
    0x11: ("QUERY_11", 2, "query, the reply (0x8003) echoes the argument"),
    0x13: ("EEPROM_WRITE", 10, "EEPROM write (decoded only, never built here)"),
    0x14: ("WRITE_BLOCK", None, "parameter write: {address, words}"),
    0x15: ("MEMSPEC", None, "memspec link table: {(target, address) pairs}"),
    0x16: ("LED_ON", 1, "FlashLED"),
    0x17: ("LED_OFF", 1, "FlashLED"),
    0x1A: ("RATE_ACK", 2, "sample-rate ack: {rate in Hz}"),
    0x1C: ("BUS_ENABLE", 3, "mixer bus enable"),
    0x1D: ("BUS_COEFF", 4, "mixer bus coefficient: {bus, (side << 24) | sub, f32}"),
    0x1E: ("INPUT_PARAM", 4, "DSP input parameter: {input, param, value}"),
    0x1F: ("SYNCH", 4, "strip synch: {(in2 << 16) | in1, instance address, enable << 29}"),
    0x23: ("CONNECT", 2, "connect: {1} at bring-up, {0} at transport start"),
    0x27: ("QUERY_27", 1, "query, the reply is 0x800D"),
}

# Device -> host reply opcodes seen in the captures
REPLIES = {
    0x8003: "reply to QUERY_11 (word 1 echoes its argument)",
    0x8007: "code-load ack {status, module id, the load's word0}",
    0x800D: "reply to QUERY_27",
}

OP_LOAD_CODE = 0x01
OP_RELEASE = 0x03
OP_ZERO_FILL = 0x08
OP_FINALIZE = 0x10
OP_QUERY_11 = 0x11
OP_EEPROM_WRITE = 0x13
OP_WRITE_BLOCK = 0x14
OP_MEMSPEC = 0x15
OP_RATE_ACK = 0x1A
OP_BUS_COEFF = 0x1D
OP_INPUT_PARAM = 0x1E
OP_SYNCH = 0x1F
OP_CONNECT = 0x23
OP_QUERY_27 = 0x27

REPLY_CODE_LOADED = 0x8007

# DSP input parameter ids (driver/ua_apollo.h UA_DSP_INPUT_*)
INPUT_PARAM_SCALE = 5
INPUT_PARAM_ROUTE = 7
INPUT_PARAM_SPAN = 8
ROUTE_NONE = 0x0001FFFF
SPAN_NONE = 0x20
SYNCH_IN_INVALID = 0x20

BILL_MAGIC = 0x6C6C6942          # "Bill"


class DspCommandError(ValueError):
    """A word stream that is not a well-formed command stream."""


@dataclass(frozen=True)
class Command:
    """One DSP command: opcode plus the argument words after word0."""

    op: int
    args: tuple = ()

    def __post_init__(self):
        if not 0 <= self.op <= 0xFFFF:
            raise DspCommandError(f"opcode {self.op:#x} out of range")
        if len(self.args) + 1 > 0xFFFF:
            raise DspCommandError("command too long")
        fixed = OPCODES.get(self.op, (None, None))[1]
        if fixed is not None and len(self.args) + 1 != fixed:
            raise DspCommandError(
                f"{self.name} takes {fixed} dwords, got {len(self.args) + 1}")
        object.__setattr__(self, "args",
                           tuple(int(a) & 0xFFFFFFFF for a in self.args))

    @property
    def length(self) -> int:
        return len(self.args) + 1

    @property
    def word0(self) -> int:
        return (self.op << 16) | self.length

    @property
    def name(self) -> str:
        return OPCODES.get(self.op, (f"OP_{self.op:02X}",))[0]

    def words(self) -> list:
        return [self.word0, *self.args]

    def describe(self) -> str:
        """One line for logs and --dry-run listings."""
        a = self.args
        if self.op in (OP_LOAD_CODE, 0x04) and len(a) >= 6 and a[1] == BILL_MAGIC:
            return (f"{self.name:12s} dest={a[0]:#07x} module={a[2] & 0xFFFF:#x} "
                    f"size={a[4]:#x} w4={a[5]:#x} ({self.length} dwords)")
        if self.op == OP_ZERO_FILL:
            return f"{self.name:12s} addr={a[0]:#09x} dwords={a[2]:#x}"
        if self.op == OP_SYNCH:
            in1, in2 = a[0] & 0xFFFF, a[0] >> 16
            what = "stop" if a[1] == 0 else f"inst={a[1]:#07x}"
            return f"{self.name:12s} in={in1:#04x}/{in2:#04x} {what} flags={a[2]:#x}"
        if self.op == OP_INPUT_PARAM:
            return f"{self.name:12s} input={a[0]:#04x} param={a[1]} value={a[2]:#x}"
        if self.op in (OP_WRITE_BLOCK, OP_MEMSPEC, 0x07):
            head = " ".join(f"{w:08x}" for w in a[:4])
            more = " ..." if len(a) > 4 else ""
            return f"{self.name:12s} ({self.length} dwords) {head}{more}"
        return f"{self.name:12s} " + " ".join(f"{w:#010x}" for w in a)


# ── Builders for the commands the tools send ────────────────────────────

def connect(value: int = 1) -> Command:
    return Command(OP_CONNECT, (value,))


def finalize(date_word: int) -> Command:
    return Command(OP_FINALIZE, (date_word,))


def query_11(tag: int) -> Command:
    return Command(OP_QUERY_11, (tag,))


def rate_ack(rate: int) -> Command:
    return Command(OP_RATE_ACK, (rate,))


def synch(in1: int, in2: int = SYNCH_IN_INVALID, instance: int = 0,
          flags: int = 0) -> Command:
    return Command(OP_SYNCH, ((in2 << 16) | in1, instance, flags))


def input_param(input_ch: int, param: int, value: int) -> Command:
    return Command(OP_INPUT_PARAM, (input_ch, param, value))


def release(module_id: int) -> Command:
    return Command(OP_RELEASE, (module_id,))


def zero_fill(addr: int, dwords: int) -> Command:
    return Command(OP_ZERO_FILL, (addr, 0, dwords))


def write_block(addr: int, words: Sequence[int]) -> Command:
    return Command(OP_WRITE_BLOCK, (addr, *words))


def memspec(pairs: Sequence[tuple]) -> Command:
    flat = []
    for target, addr in pairs:
        flat += [target, addr]
    return Command(OP_MEMSPEC, tuple(flat))


def load_code(dest: int, blob_words: Sequence[int]) -> Command:
    return Command(OP_LOAD_CODE, (dest, *blob_words))


def bus_coeff(bus: int, sub: int, value_u32: int, side: int = 0) -> Command:
    return Command(OP_BUS_COEFF, (bus, ((side & 0xFF) << 24) | sub, value_u32))


def connect_date_word(unix_time: float) -> int:
    """w1 of FINALIZE: (days since 2007-01-01 UTC << 12) + minute of day.

    The rule of driver/ua_dsp.c ua_dsp_connect_date (kext _connectToDsps);
    0 before 2007.  The Solo USB captures carry 0x01B71448 (2026-03-27
    18:16 UTC) and 0x01B7A1FA (2026-04-05 08:26 UTC).
    """
    secs = int(unix_time) - 1167609600
    if secs < 0:
        return 0
    days, sod = divmod(secs, 86400)
    return ((days << 12) + sod // 60) & 0xFFFFFFFF


# ── Command streams ─────────────────────────────────────────────────────

def decode_commands(words: Sequence[int]) -> list:
    """Split a dword stream into commands.  Raises DspCommandError on a zero
    length or a command running past the end."""
    out = []
    i = 0
    n = len(words)
    while i < n:
        w0 = words[i]
        length = w0 & 0xFFFF
        if length == 0:
            raise DspCommandError(f"zero-length command at dword {i}")
        if i + length > n:
            raise DspCommandError(
                f"command {w0:#010x} at dword {i} runs past the end "
                f"({length} dwords, {n - i} left)")
        op = w0 >> 16
        fixed = OPCODES.get(op, (None, None))[1]
        if fixed is not None and fixed != length:
            raise DspCommandError(
                f"{OPCODES[op][0]} at dword {i} has length {length}, "
                f"expected {fixed}")
        out.append(Command(op, tuple(words[i + 1:i + length])))
        i += length
    return out


def encode_commands(cmds: Iterable[Command]) -> list:
    out = []
    for c in cmds:
        out += c.words()
    return out


@dataclass
class Frame:
    """One USB bulk frame (URB)."""

    words: list
    seq: int = 0
    flags: int = 0
    magic: int = FRAME_MAGIC_OUT

    def to_bytes(self) -> bytes:
        if len(self.words) > MAX_FRAME_WORDS:
            raise DspCommandError(f"frame of {len(self.words)} dwords "
                                  f"(max {MAX_FRAME_WORDS})")
        head = bytes((len(self.words), self.flags & 0xFF, self.seq & 0xFF,
                      self.magic))
        return head + struct.pack(f"<{len(self.words)}I", *self.words)

    @classmethod
    def from_bytes(cls, data: bytes) -> "Frame":
        if len(data) < 4:
            raise DspCommandError(f"frame of {len(data)} bytes")
        count, flags, seq, magic = data[0], data[1], data[2], data[3]
        if magic not in (FRAME_MAGIC_OUT, FRAME_MAGIC_IN):
            raise DspCommandError(f"bad frame magic {magic:#04x}")
        if len(data) < 4 + 4 * count:
            raise DspCommandError(
                f"frame says {count} dwords but carries {(len(data) - 4) // 4}")
        words = list(struct.unpack_from(f"<{count}I", data, 4))
        return cls(words, seq, flags, magic)


def frame_stream(words: Sequence[int], seq0: int = 0,
                 plan: Sequence[int] | None = None, flags: int = 0) -> list:
    """Cut a dword stream into frames.

    plan: dword count of each frame (a capture's packetization); without it
    the stream is cut at MAX_FRAME_WORDS.  The plan must cover the stream
    exactly.
    """
    words = list(words)
    sizes = list(plan) if plan is not None else []
    if plan is None:
        sizes = [min(MAX_FRAME_WORDS, len(words) - i)
                 for i in range(0, len(words), MAX_FRAME_WORDS)]
    if sum(sizes) != len(words):
        raise DspCommandError(f"frame plan covers {sum(sizes)} dwords, "
                              f"stream has {len(words)}")
    frames, pos = [], 0
    for k, size in enumerate(sizes):
        if not 0 < size <= MAX_FRAME_WORDS:
            raise DspCommandError(f"frame size {size} out of range")
        frames.append(Frame(words[pos:pos + size], (seq0 + k) & 0xFF, flags))
        pos += size
    return frames


def frame_transactions(transactions: Sequence[Sequence[Command]],
                       seq0: int = 0, flags: int = 0) -> list:
    """Frames for a list of transactions: each transaction (commands sent
    together) starts a new frame and is cut at MAX_FRAME_WORDS."""
    frames = []
    seq = seq0
    for tx in transactions:
        part = frame_stream(encode_commands(tx), seq, None, flags)
        frames += part
        seq += len(part)
    return frames


def deframe(frames: Sequence[Frame]) -> list:
    words = []
    for f in frames:
        words += f.words
    return words


def plan_of(frames: Sequence[Frame]) -> list:
    return [len(f.words) for f in frames]


# ── tools/usb-re/init-bulk-sequence.bin container ──────────────────────

def read_urb_file(path) -> list:
    """The URB list of an init-bulk-sequence.bin style file:
    u32 count, then u32 length + bytes per URB (little-endian)."""
    with open(path, "rb") as f:
        return parse_urb_container(f.read())


def parse_urb_container(data: bytes) -> list:
    if len(data) < 4:
        raise DspCommandError("URB container too short")
    count = struct.unpack_from("<I", data, 0)[0]
    pos, urbs = 4, []
    for i in range(count):
        if pos + 4 > len(data):
            raise DspCommandError(f"URB {i}: truncated length")
        n = struct.unpack_from("<I", data, pos)[0]
        pos += 4
        if pos + n > len(data):
            raise DspCommandError(f"URB {i}: truncated data")
        urbs.append(bytes(data[pos:pos + n]))
        pos += n
    if pos != len(data):
        raise DspCommandError(f"{len(data) - pos} trailing bytes")
    return urbs


def build_urb_container(urbs: Sequence[bytes]) -> bytes:
    out = bytearray(struct.pack("<I", len(urbs)))
    for u in urbs:
        out += struct.pack("<I", len(u)) + bytes(u)
    return bytes(out)


@dataclass
class InitStream:
    """A decoded bulk init stream: frames plus the commands they carry."""

    frames: list
    commands: list = field(default_factory=list)
    # frame index each command starts in
    starts: list = field(default_factory=list)

    @classmethod
    def from_urbs(cls, urbs: Sequence[bytes]) -> "InitStream":
        frames = [Frame.from_bytes(u) for u in urbs]
        for i, (f, u) in enumerate(zip(frames, urbs)):
            if len(u) != 4 + 4 * len(f.words):
                raise DspCommandError(f"URB {i}: {len(u)} bytes for "
                                      f"{len(f.words)} dwords")
        words = deframe(frames)
        commands = decode_commands(words)
        # where each command starts
        bounds, pos = [], 0
        for k, f in enumerate(frames):
            bounds.append((pos, k))
            pos += len(f.words)
        starts, pos, bi = [], 0, 0
        for c in commands:
            while bi + 1 < len(bounds) and bounds[bi + 1][0] <= pos:
                bi += 1
            starts.append(bounds[bi][1])
            pos += c.length
        return cls(frames, commands, starts)

    @classmethod
    def from_file(cls, path) -> "InitStream":
        return cls.from_urbs(read_urb_file(path))

    @property
    def plan(self) -> list:
        return plan_of(self.frames)

    @property
    def seq0(self) -> int:
        return self.frames[0].seq if self.frames else 0

    @property
    def flags(self) -> int:
        return self.frames[0].flags if self.frames else 0

    def to_urbs(self) -> list:
        """Re-encode the commands with the stream's own packetization."""
        frames = frame_stream(encode_commands(self.commands), self.seq0,
                              self.plan, self.flags)
        return [f.to_bytes() for f in frames]

    def transactions(self) -> list:
        """Commands grouped by the frame they start in."""
        groups: dict = {}
        for c, s in zip(self.commands, self.starts):
            groups.setdefault(s, []).append(c)
        return [groups[k] for k in sorted(groups)]


def parse_reply(data: bytes) -> list:
    """Commands in one device->host frame ({u8 count, flags, seq, 0xDD})."""
    f = Frame.from_bytes(data)
    if f.magic != FRAME_MAGIC_IN:
        raise DspCommandError(f"not a reply frame (magic {f.magic:#04x})")
    words = f.words
    out, i = [], 0
    while i < len(words):
        length = words[i] & 0xFFFF
        if length == 0 or i + length > len(words):
            # a long reply continues in the next frame: keep what is here
            out.append((words[i] >> 16, tuple(words[i + 1:])))
            break
        out.append((words[i] >> 16, tuple(words[i + 1:i + length])))
        i += length
    return out


def code_load_ack(reply_cmds: Sequence[tuple]) -> list:
    """(status, module id, load word0) of every code-load ack in a reply."""
    acks = []
    for op, args in reply_cmds:
        if op == REPLY_CODE_LOADED and len(args) >= 3:
            acks.append((args[0], args[1], args[2]))
    return acks
