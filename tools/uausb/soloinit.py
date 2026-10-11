"""The Apollo Solo USB DSP bring-up, built from the DSP command language (J1).

tools/usb-full-init.py used to replay the 38 bulk frames of
tools/usb-re/init-bulk-sequence.bin byte for byte.  This module builds the
same stream from a model of what the UA driver does (apollo-linux
docs/re-notes/macos-dsp-module-load.md, "native generation spec"):

  * teardown: every Console input strip is stopped (SYNCH with address 0,
    then the input's span/route params) and its code released when the
    last user goes;
  * code: Bill blobs placed bottom-up in pool 0 from its base, each taking
    w4 * 2 / 3 dwords; loaded the first time a strip needs them;
  * strips: private memory allocated top-down in pools 1-3, zero-filled,
    linked by a memspec table, then header and strip parameters written;
  * SYNCH enable of every strip with its instance address.

The pool layout comes from the device's DSP window (11 words; the Windows
driver reads them at bring-up) and the Solo USB values are below.  With
them the model reproduces all 128 commands of the captured stream, and
with the capture's frame sizes every byte of all 38 frames
(mixer-engine/tests/test_usb_dspcmd.py).

What is taken from the capture file at run time, not stored here: the five
code blobs (Bill modules A5, C2, EB, DA, 12B; UA's code) and the strip
parameter blocks (plugin defaults).  The plan below (which strips, in which
order) is the Solo USB Console's, as captured; Twin USB and Twin X USB need
their own (no capture exists).

Sample rate.  The capture is a 48 kHz session.  build_init(rate=...)
puts the rate in the rate-ack command and recomputes the Mic strip's ramp
constants (k, f, 1/(f*k), the rule in macos-dsp-module-load.md A1, which
gives the captured 75 / 8.0 / 0x3ADA740E at 48 kHz).  The strip filter
coefficients (DC blocker, 75 Hz low cut) and the input strip's 1/960 word
stay the 48 kHz values: their rule is not known.  Only 48 kHz matches a
capture.  UNTESTED ON HARDWARE.
"""

from __future__ import annotations

import struct
from dataclasses import dataclass, field
from typing import Sequence

from . import dspcmd as dc

# ── Pools ───────────────────────────────────────────────────────────────


@dataclass(frozen=True)
class MemoryMap:
    """DSP pool layout: sizes, bases and scratch (pools 0-3), in dwords and
    DSP word addresses, as CPcieDSP::GetProperty(6) returns them."""

    sizes: tuple
    bases: tuple
    scratch: tuple

    @classmethod
    def from_dict(cls, d: dict) -> "MemoryMap":
        return cls(tuple(d["sizes"]), tuple(d["bases"]), tuple(d["scratch"]))

    def tops(self) -> tuple:
        """Top of pools 1-3 (CResourcePool::Initialize: base rounded up to
        even, size rounded down to even)."""
        return tuple(_even_up(self.bases[p]) + (self.sizes[p] & ~1)
                     for p in (1, 2, 3))


# Read by the Windows driver from a Solo USB's DSP window (request 0x04 at
# 0x1004-0x1007 and 0x1061-0x1067, tools/usb-re/apollo-init.pcap).  The tops
# they give (0x9D1B8, 0xC6FFE, 0x08FFE000) are the ones every address in
# init-bulk-sequence.bin is allocated from.
SOLO_USB_MEMORY_MAP = MemoryMap(
    sizes=(0x4000, 0xA9B9, 0x1FFF, 0xFF51B2),
    bases=(0xE0000, 0x92800, 0xC5000, 0x08008E4E),
    scratch=(0, 0x80, 0x80, 0x8000))


def _even_up(v: int) -> int:
    return (v + 1) & ~1


class Pools:
    """First-fit allocation as CResourcePool::_mapResource does it on an
    empty DSP: pool 0 (code) bottom-up from base + scratch, pools 1-3
    top-down from the top.  Sizes are rounded up to even."""

    def __init__(self, mm: MemoryMap):
        self.code_next = _even_up(mm.bases[0]) + mm.scratch[0]
        self.code_end = _even_up(mm.bases[0]) + (mm.sizes[0] & ~1)
        tops = mm.tops()
        self.top = {1: tops[0], 2: tops[1], 3: tops[2]}
        self.floor = {p: _even_up(mm.bases[p]) + mm.scratch[p] for p in (1, 2, 3)}

    def code(self, dwords: int) -> int:
        need = _even_up(dwords)
        if self.code_next + need > self.code_end:
            raise MemoryError("code pool full")
        addr = self.code_next
        self.code_next += need
        return addr

    def data(self, pool: int, dwords: int) -> int:
        need = _even_up(dwords)
        addr = self.top[pool] - need
        if addr < self.floor[pool]:
            raise MemoryError(f"pool {pool} full")
        self.top[pool] = addr
        return addr


# ── Strip kinds ─────────────────────────────────────────────────────────


@dataclass(frozen=True)
class Private:
    """One private memory of a strip instance.  parent/offset: the memspec
    link (the address of this block is stored at parent's address +
    offset); the instance header has none."""

    pool: int
    dwords: int
    parent: int | None = None
    offset: int = 0


_COMMONS = (Private(3, 0x100, 1, 0x0E), Private(3, 0x100, 1, 0x0F),
            Private(3, 0x100, 1, 0x06), Private(3, 0x100, 1, 0x07))


@dataclass(frozen=True)
class StripKind:
    name: str
    plugin_id: int
    in_ch: int
    out_ch: int
    modules: tuple          # Bill module ids, resource order
    privates: tuple


# UAD2PluginAllocInfo of the Console strips (macos-dsp-module-load.md (a),
# "Mic Input Strip sub-plugin" and the input-strip common entries).  On the
# Solo USB the input strips carry A5 and C2 only, and the stereo AUX return
# strips have no pool-3 blocks.
STRIP_KINDS = {
    "input": StripKind("input", 0x83E, 1, 1, (0xA5, 0xC2),
                       (Private(1, 8), Private(1, 0x1E, 0, 6)) + _COMMONS),
    "mic": StripKind("mic", 0x846, 1, 1, (0xEB, 0xDA, 0xA5, 0xC2, 0x12B),
                     (Private(1, 8), Private(1, 0x38, 0, 6),
                      Private(2, 0x10, 1, 0x36), Private(1, 0x10, 1, 0x37),
                      Private(3, 0x10, 1, 0x1B), Private(1, 4, 1, 0x1C))
                     + _COMMONS),
    "aux": StripKind("aux", 0x83E, 2, 2, (0xA5, 0xC2),
                     (Private(1, 8), Private(1, 0x1E, 0, 6))),
}

# Header parameter block: {0x24, 0x40000000, in, out, 0x22, 1, pluginId}
HEADER_WORDS = (0x24, 0x40000000, None, None, 0x22, 1, None)

# ── The Solo USB plan (Console start, as captured) ──────────────────────

# Strips in creation order: (kind, synch-in channels)
SOLO_STRIPS = (
    ("input", (0x19,)), ("input", (0x18,)),     # VIRTUAL 2, VIRTUAL 1
    ("mic", (0x01,)), ("mic", (0x00,)),         # ANALOG 2, ANALOG 1
    ("aux", (0x12, 0x13)), ("aux", (0x10, 0x11)),   # AUX 2, AUX 1 returns
)
# What is stopped and released before, in order
SOLO_TEARDOWN = (
    ("stop", (0x00,)), ("stop", (0x01,)), ("release", (0xEB, 0xDA, 0x12B)),
    ("stop", (0x18,)), ("stop", (0x19,)), ("stop", (0x10, 0x11)),
    ("stop", (0x12, 0x13)), ("release", (0xA5,)), ("release", (0xC2,)),
)
# SYNCH enable order (by first channel)
SOLO_ENABLE_ORDER = (0x18, 0x00, 0x01, 0x19, 0x10, 0x12)

# The Mic strip block at private 1 offset 0 carries the ramp words at these
# indices (after the 8 common words, the two -2.0f and the ramp count 0x10)
MIC_RAMP_COUNT_INDEX = 10
MIC_RAMP_INDEX = 11


# ── Capture template: blobs and parameter blocks ───────────────────────


class TemplateError(ValueError):
    pass


@dataclass
class Template:
    """What the generator takes from a captured init stream."""

    blobs: dict                     # module id -> Bill blob words
    params: dict                    # kind -> [(private index, offset, words)]
    stream: dc.InitStream | None = None

    @classmethod
    def from_file(cls, path) -> "Template":
        return cls.from_stream(dc.InitStream.from_file(path))

    @classmethod
    def from_stream(cls, stream: dc.InitStream) -> "Template":
        blobs: dict = {}
        params: dict = {}
        cmds = stream.commands
        privs: list = []            # (addr, dwords) of the instance being built
        i = 0
        while i < len(cmds):
            c = cmds[i]
            a = c.args
            if c.op == dc.OP_LOAD_CODE and len(a) >= 6 and a[1] == dc.BILL_MAGIC:
                blobs.setdefault(a[2] & 0xFFFF, list(a[1:]))
                privs = []
            elif c.op == dc.OP_ZERO_FILL:
                if i == 0 or cmds[i - 1].op != dc.OP_ZERO_FILL:
                    privs = []
                privs.append((a[0], a[2]))
            elif (c.op == dc.OP_WRITE_BLOCK and len(a) == 8 and a[1] == 0x24
                  and a[2] == 0x40000000):
                kind = _kind_of(a[7], a[3], len(privs))
                blocks = []
                j = i + 1
                while j < len(cmds) and cmds[j].op == dc.OP_WRITE_BLOCK:
                    addr = cmds[j].args[0]
                    idx, off = _locate(privs, addr)
                    blocks.append((idx, off, list(cmds[j].args[1:])))
                    j += 1
                if kind is not None:
                    params.setdefault(kind, blocks)
                i = j
                continue
            i += 1
        return cls(blobs, params, stream)

    def blob(self, module: int) -> list:
        try:
            return self.blobs[module]
        except KeyError:
            raise TemplateError(f"module {module:#x} is not in the capture") from None


def _kind_of(plugin_id: int, in_ch: int, nprivs: int) -> str | None:
    for k in STRIP_KINDS.values():
        if (k.plugin_id == plugin_id and k.in_ch == in_ch
                and len(k.privates) == nprivs):
            return k.name
    return None


def _locate(privs, addr) -> tuple:
    for idx, (base, size) in enumerate(privs):
        if base <= addr < base + size:
            return idx, addr - base
    raise TemplateError(f"parameter write at {addr:#x} is in no private block")


def code_dwords(blob: Sequence[int]) -> int:
    """Pool-0 footprint of a Bill code blob: w4 * 2 / 3 (CResource)."""
    return (blob[4] * 2) // 3


def _f32(v: float) -> int:
    return struct.unpack("<I", struct.pack("<f", v))[0]


def mic_ramp_words(rate: int) -> tuple:
    """(k, f, 1/(f*k)) of the Mic strip's parameter ramp.

    Block size BS = 32 below 100 kHz, else 64 (the synch-plugin rule of
    CUAD2PlugIn+0xbcc; the 48 kHz capture's f = 8.0 means 32);
    k = int(rate / BS * 0.05 + 0.5), f = BS / 4.
    """
    bs = 32 if rate < 100000 else 64
    k = int(rate / bs * 0.05 + 0.5)
    f = bs / 4.0
    return k, _f32(f), _f32(1.0 / (f * k))


# ── The generator ───────────────────────────────────────────────────────


@dataclass
class Instance:
    kind: StripKind
    channels: tuple
    addrs: list = field(default_factory=list)

    @property
    def header(self) -> int:
        return self.addrs[0]


def _ip_triplet(ch: int) -> list:
    return [dc.input_param(ch, dc.INPUT_PARAM_SPAN, dc.SPAN_NONE),
            dc.input_param(ch, dc.INPUT_PARAM_SPAN, dc.SPAN_NONE),
            dc.input_param(ch, dc.INPUT_PARAM_ROUTE, dc.ROUTE_NONE)]


def _synch(channels, instance: int) -> dc.Command:
    in1 = channels[0]
    in2 = channels[1] if len(channels) > 1 else dc.SYNCH_IN_INVALID
    return dc.synch(in1, in2, instance, 0)


def build_transactions(template: Template, rate: int = 48000,
                       memory_map: MemoryMap = SOLO_USB_MEMORY_MAP,
                       strips=SOLO_STRIPS, teardown=SOLO_TEARDOWN,
                       enable_order=SOLO_ENABLE_ORDER) -> list:
    """The bring-up as a list of transactions (lists of commands).

    A transaction is what the UA driver hands down in one go: one strip
    stop, one release, one code load, one instance's zero-fills, its
    memspec, its parameter writes, the final SYNCH enables.
    """
    txs: list = []
    for what, args in teardown:
        if what == "stop":
            cmds = [_synch(args, 0)]
            for ch in args:
                cmds += _ip_triplet(ch)
            txs.append(cmds)
        elif what == "release":
            txs.append([dc.release(m) for m in args])
        else:
            raise ValueError(f"teardown step {what!r}")

    pools = Pools(memory_map)
    resident: dict = {}
    instances = []
    for kind_name, channels in strips:
        kind = STRIP_KINDS[kind_name]
        for m in kind.modules:
            if m in resident:
                continue
            blob = template.blob(m)
            dest = pools.code(code_dwords(blob))
            resident[m] = dest
            # Bill header as transmitted: arch byte 0, word 2 = 0 (USB)
            txs.append([dc.load_code(dest, blob)])
        inst = Instance(kind, tuple(channels))
        for p in kind.privates:
            inst.addrs.append(pools.data(p.pool, p.dwords))
        txs.append([dc.zero_fill(a, p.dwords)
                    for a, p in zip(inst.addrs, kind.privates)])
        if len(kind.privates) >= 2:
            pairs = [(inst.addrs[p.parent] + p.offset, a)
                     for a, p in zip(inst.addrs[1:], kind.privates[1:])]
            txs.append([dc.memspec(pairs)])
        header = list(HEADER_WORDS)
        header[2], header[3], header[6] = kind.in_ch, kind.out_ch, kind.plugin_id
        writes = [dc.write_block(inst.header, header)]
        for idx, off, words in _param_blocks(template, kind, rate):
            writes.append(dc.write_block(inst.addrs[idx] + off, words))
        txs.append(writes)
        instances.append(inst)

    by_first = {inst.channels[0]: inst for inst in instances}
    enable = []
    for ch in enable_order:
        inst = by_first[ch]
        enable.append(_synch(inst.channels, inst.header))
        for c in inst.channels:
            enable += _ip_triplet(c)
    txs.append(enable)
    return txs


def _param_blocks(template: Template, kind: StripKind, rate: int) -> list:
    try:
        blocks = template.params[kind.name]
    except KeyError:
        raise TemplateError(f"no {kind.name} strip parameters in the capture") \
            from None
    out = []
    for idx, off, words in blocks:
        words = list(words)
        if (kind.name == "mic" and idx == 1 and off == 0
                and len(words) > MIC_RAMP_INDEX + 2
                and words[MIC_RAMP_COUNT_INDEX] == 0x10):
            words[MIC_RAMP_INDEX:MIC_RAMP_INDEX + 3] = mic_ramp_words(rate)
        out.append((idx, off, words))
    return out


@dataclass
class InitPlan:
    """A built init: transactions plus the frames to send."""

    transactions: list
    frames: list
    preamble: int               # frames before the strip stream
    rate: int
    capture_framing: bool       # frame sizes copied from the capture

    def urbs(self) -> list:
        return [f.to_bytes() for f in self.frames]

    def commands(self) -> list:
        return [c for tx in self.transactions for c in tx]


def build_init(template: Template, rate: int = 48000, rate_ack: bool = True,
               connect_date: int | None = None, query_tag: int = 0,
               memory_map: MemoryMap = SOLO_USB_MEMORY_MAP,
               capture_framing: bool = True) -> InitPlan:
    """The frames usb-full-init.py sends.

    preamble: the Windows bring-up's connect ({0x230002, 1}, the date word
    twice, the 0x11 query; only with connect_date) and the rate ack
    ({0x1A0002, rate}, with rate_ack), each its own frame as captured.
    Then the strip stream: with capture_framing and a template stream of
    the same length, cut exactly as the capture was (byte-identical to the
    replay at 48 kHz), else one transaction per frame.  The strip stream
    keeps the capture's first sequence number (255); the preamble takes
    the numbers before it.
    """
    txs = build_transactions(template, rate, memory_map)
    words = dc.encode_commands(c for tx in txs for c in tx)
    pre: list = []
    if connect_date is not None:
        pre.append([dc.connect(1), dc.finalize(connect_date)])
        pre.append([dc.finalize(connect_date), dc.query_11(query_tag)])
    if rate_ack:
        pre.append([dc.rate_ack(rate)])

    src = template.stream
    seq0 = src.seq0 if src is not None else 0
    flags = src.flags if src is not None else 0
    use_capture = (capture_framing and src is not None
                   and sum(src.plan) == len(words))
    if use_capture:
        body = dc.frame_stream(words, seq0, src.plan, flags)
    else:
        body = dc.frame_transactions(txs, seq0, flags)
    pre_frames = []
    for k, tx in enumerate(pre):
        pre_frames.append(dc.Frame(dc.encode_commands(tx),
                                   (seq0 - len(pre) + k) & 0xFF, flags))
    return InitPlan(pre + txs, pre_frames + body, len(pre_frames), rate,
                    use_capture)
