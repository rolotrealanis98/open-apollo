"""Mixer settings for the USB Apollos: the one-shot mailbox and the
SetMixerParam mapping.

Mailbox.  The 38 DSP mixer settings are an edge-triggered mailbox, on USB
exactly as on PCIe (driver/ua_core.c mixer_mailbox, kext _writeSetting).
The USB form is read from UAD2Usb.sys (WriteSetting 0x140007fb0, flush
0x140008330/0x14000839c) and checked against every settings write in the
Windows capture tools/usb-re/mixer-knobs.pcap:

  * a change merges {value, mask} into the setting's cache (value bits
    stay, the mask accumulates);
  * a flush happens only when SEQ_RD (0x0603) equals the SEQ_WR the host
    wrote last; it writes each of the three setting groups (settings 0-15
    at 0x062D, 16-31 at 0x064F, 32-37 at 0x0670, 8 bytes per setting) that
    holds a pending change or a pending zero pass, as one block: a changed
    setting as A = (mask[15:0] << 16) | value[15:0],
    B = (mask[31:16] << 16) | value[31:16], whose mask is then cleared; a
    setting written last time and unchanged since as zeros (once); every
    other setting of the block as zeros;
  * then one SEQ_WR bump (0x0602 = SEQ_WR + 1);
  * a flush is made only for a pending change; zero passes ride along with
    the next one (no flush of their own).

Mapping.  map_param() is a port of the PCIe driver's
ua_preamp_write_locked() and ua_monitor_set_param() (driver/ua_core.c on
driver-v2), so a (channel type, channel, param, value) from the daemon lands
in the same setting bits on USB as on PCIe.  The Solo USB capture agrees for
every control it exercises: preamp gain (settings 3, 16, 17), 48V and
Mic/Line (setting 0), monitor level and the mute/mono field (setting 2),
the delay-comp field (setting 35).  The USB mixer type has no kext gate
table, so nothing is gated by kind (the driver's UA_MIXER_KIND_NONE).

UNTESTED ON HARDWARE.
"""

from __future__ import annotations

import struct
from dataclasses import dataclass

from .device import SEQ_RD, SEQ_WR, SETTINGS_GROUPS

NUM_SETTINGS = 38

PREAMP_CHANNEL_TYPE = 1
MONITOR_CHANNEL_TYPE = 2

# preamp params (driver/ua_apollo.h UA_PREAMP_PARAM_*)
P_MIC_LINE, P_PAD, P_LINK, P_48V, P_LOWCUT, P_PHASE, P_GAIN_C = 0, 1, 2, 3, 4, 5, 6
P_MIC_IMP, P_CUSTOM, P_GAIN_B, P_GAIN_A = 7, 8, 9, 0x0A
P_KNOB_STEPS, P_HIZ_IMP, P_SEL_PARAM, P_ROUTE = 0x10, 0x11, 0x12, 0x13
P_HIZ, P_PGA_LINE, P_LEVEL, P_RAW_REC = 0x14, 0x15, 0x16, 0x17

DSP_INPUT_SCALE, DSP_INPUT_ROUTE, DSP_INPUT_SPAN = 5, 7, 8

MON_MUTE_ON = 2
MON_MONO = 1


class MixerParamError(ValueError):
    """The PCIe driver returns -EINVAL for this param (the kext's -8)."""


@dataclass(frozen=True)
class SettingWrite:
    setting: int
    value: int
    mask: int


@dataclass(frozen=True)
class DspInputParam:
    """A param that is a DSP command {0x001E0004, input, param, value}."""

    input: int
    param: int
    value: int


def _bit(n: int) -> int:
    return 1 << n


def _swap2(value: int, lo: int) -> int:
    """The kext's 2-bit swap field: value bit 1 -> field bit lo, bit 0 -> lo+1."""
    return (_bit(lo) if value & 2 else 0) | (_bit(lo + 1) if value & 1 else 0)


def map_preamp(ch: int, param: int, value: int, num_preamps: int = 2,
               num_hiz: int = 1):
    """ua_preamp_write_locked() for a model with `num_preamps` preamps.

    Returns SettingWrite, DspInputParam, or raises MixerParamError.
    """
    value &= 0xFFFFFFFF
    if param > P_RAW_REC:
        raise MixerParamError(f"preamp param {param:#x}")
    if param in (P_ROUTE, P_LEVEL):
        if ch > 31:
            raise MixerParamError(f"input {ch}")
        return DspInputParam(ch, DSP_INPUT_ROUTE if param == P_ROUTE
                             else DSP_INPUT_SPAN, value)
    if param == P_PHASE and ch >= num_preamps:
        if ch > 31:
            raise MixerParamError(f"input {ch}")
        if value == 0:
            value = 0x3F800000
        elif value == 1:
            value = 0xBF800000
        return DspInputParam(ch, DSP_INPUT_SCALE, value)
    if ch >= num_preamps or ch >= 8:
        raise MixerParamError(f"preamp channel {ch} (model has {num_preamps})")

    hi = ch > 3
    hiz_inputs = min(num_hiz, 2)
    if param in (P_MIC_LINE, P_PAD, P_48V, P_LOWCUT, P_PHASE):
        setting = 12 if hi else 0
        mask = (1 << param) << ((ch & 3) * 6)
        if param == P_PHASE:
            on = bool(value & 0x80000000) or value == 1
        else:
            on = bool(value)
        return SettingWrite(setting, mask if on else 0, mask)
    if param == P_LINK:
        if ch & 1:
            return None             # the kext links a pair from its even channel
        mask = 0x104 << (12 * ((ch & 3) >> 1))
        return SettingWrite(12 if hi else 0, mask if value else 0, mask)
    if param == P_GAIN_C:
        shift = (ch & 3) * 6 + (4 if ch & 2 else 0)
        return SettingWrite(14 if hi else 3, (value & 0x3F) << shift,
                            0x3F << shift)
    if param == P_MIC_IMP:
        shift = (9, 16, 20, 24)[ch & 3]
        return SettingWrite(13 if hi else 1, (value & 0xF) << shift,
                            0xF << shift)
    if param == P_CUSTOM:
        mask = _bit(8 + ch)
        return SettingWrite(11, mask if value else 0, mask)
    if 0x09 <= param <= 0x0F:
        shift = (ch & 3) * 8
        return SettingWrite(param + 7 + (8 if hi else 0),
                            (value & 0xFF) << shift, 0xFF << shift)
    if param == P_KNOB_STEPS:
        shift = 4 * (ch & 3)
        return SettingWrite(31 if hi else 23, (value & 0xF) << shift,
                            0xF << shift)
    if param == P_HIZ_IMP:
        if ch >= hiz_inputs:
            raise MixerParamError(f"HiZ impedance on input {ch}")
        shift = 4 * ch
        return SettingWrite(13, (value & 0xF) << shift, 0xF << shift)
    if param == P_SEL_PARAM:
        shift = 16 + 2 * (ch & 3)
        return SettingWrite(31 if hi else 23, (value & 3) << shift, 3 << shift)
    if param == P_HIZ:
        if hi:
            raise MixerParamError("HiZ write on channels 4-7")
        mask = _bit(24 + ch)
        return SettingWrite(0, mask if value else 0, mask)
    if param == P_PGA_LINE:
        # kind gate UA_KIND_SET_IN_15; no kind on USB, as the driver's NONE
        mask = _bit(24 + ch)
        return SettingWrite(15, mask if value else 0, mask)
    if param == P_RAW_REC:
        if hi:
            raise MixerParamError("raw record on channels 4-7")
        mask = _bit(16 + ch)
        return SettingWrite(5, mask if value else 0, mask)
    raise MixerParamError(f"preamp param {param:#x}")


# SetOtherParam plain field runs: (first, last, setting, shift, stride, width)
_OTHER_FIELDS = (
    (0x4D, 0x4E, 33, 16, 8, 8), (0x4F, 0x52, 34, 0, 8, 8),
    (0x55, 0x5A, 32, 26, 1, 1), (0x5D, 0x62, 32, 18, 1, 1),
    (0x65, 0x65, 31, 24, 0, 2), (0x69, 0x69, 5, 5, 0, 1),
    (0x6B, 0x6E, 36, 0, 8, 8), (0x6F, 0x72, 37, 0, 8, 8),
    (0x73, 0x7A, 35, 16, 1, 1), (0x7B, 0x82, 35, 8, 1, 1),
    (0x88, 0x88, 5, 24, 0, 8), (0x89, 0x8A, 5, 10, 1, 1),
)

# (setting, shift, width) of the single-field monitor params; width 1 = bool
_MON_FIELDS = {
    0x19: (0, 28, 1), 0x1A: (0, 29, 1), 0x1B: (0, 30, 1), 0x1C: (0, 31, 1),
    0x13: (1, 28, 3), 0x14: (1, 13, 3), 0x1F: (1, 31, 1),
    0x01: (2, 0, 8), 0x02: (2, 8, 8), 0x05: (2, 24, 2), 0x07: (2, 26, 2),
    0x15: (2, 28, 1), 0x44: (2, 31, 1),
    0x1E: (6, 10, 1), 0x2E: (6, 0, 8),
    0x20: (7, 10, 1), 0x2F: (7, 0, 8), 0x45: (7, 11, 1), 0x47: (7, 12, 1),
    0x48: (7, 13, 1), 0x63: (7, 14, 1),
    0x21: (8, 0, 8), 0x22: (8, 28, 2), 0x24: (8, 30, 2),
    0x2A: (11, 0, 1), 0x2B: (11, 1, 1), 0x2C: (11, 2, 1), 0x2D: (11, 3, 1),
    0x30: (11, 16, 8), 0x31: (11, 24, 8), 0x32: (11, 4, 1), 0x33: (11, 5, 1),
    0x34: (11, 6, 1), 0x35: (11, 7, 1),
    0x3F: (12, 24, 3), 0x40: (12, 27, 3),
    0x3A: (13, 28, 2), 0x42: (13, 13, 3), 0x46: (13, 8, 1),
    0x43: (14, 28, 3), 0x49: (14, 31, 1), 0x66: (14, 0, 8),
    0x38: (15, 0, 7), 0x39: (15, 16, 7), 0x3B: (15, 8, 1), 0x3C: (15, 12, 1),
    0x3E: (15, 9, 1), 0x41: (15, 7, 1),
    0x67: (5, 3, 1), 0x68: (5, 4, 1), 0x87: (5, 9, 1),
    0x4A: (32, 0, 8), 0x64: (32, 8, 8), 0x5B: (32, 16, 1), 0x5C: (32, 17, 1),
    0x53: (32, 24, 1), 0x54: (32, 25, 1),
    0x4B: (33, 0, 8), 0x4C: (33, 8, 8),
    0x6A: (35, 0, 5),
}
# The kext's swapped 2-bit fields: param -> (setting, lower field bit)
_MON_SWAP = {0x06: (2, 20), 0x08: (2, 22), 0x0A: (6, 8), 0x0F: (7, 8),
             0x23: (8, 24), 0x25: (8, 26)}
# Stored by the kext without a setting write; accepted, nothing written
_MON_NOP = {0x16, 0x0B, 0x0C, 0x0D, 0x10, 0x11, 0x12, 0x17, 0x18, 0x26,
            0x27, 0x28, 0x29}
# The kext answers -18 for these
_MON_ERROR = {0x09, 0x0E, 0x3D}


def map_monitor(param: int, value: int):
    """ua_monitor_set_param() with no kind gate.  SettingWrite, None (no
    write) or MixerParamError."""
    value &= 0xFFFFFFFF
    if param > 0x8A or param in _MON_ERROR:
        raise MixerParamError(f"monitor param {param:#x}")
    if param in _MON_NOP:
        return None
    if param == 0x03:
        # bit 1 of the value is the mute (bit 16), bit 0 the mono (bit 17)
        v = (_bit(16) if value & MON_MUTE_ON else 0) | \
            (_bit(17) if value & MON_MONO else 0)
        return SettingWrite(2, v, 0x00030000)
    if param == 0x04:
        v = ((value & 3) << 18) | (((value >> 2) & 1) << 29)
        return SettingWrite(2, v, 0x200C0000)
    if param == 0x1D:
        return SettingWrite(6, (value & 7) << 11, 0x0000F800)
    if param == 0x36:
        return SettingWrite(4, value, 0xFFFFFFFF)
    if param in _MON_SWAP:
        setting, lo = _MON_SWAP[param]
        return SettingWrite(setting, _swap2(value, lo), 3 << lo)
    if param in _MON_FIELDS:
        setting, shift, width = _MON_FIELDS[param]
        vmask = (1 << width) - 1
        v = (1 if value else 0) if width == 1 else (value & vmask)
        return SettingWrite(setting, v << shift, vmask << shift)
    for first, last, setting, shift0, stride, width in _OTHER_FIELDS:
        if first <= param <= last:
            shift = shift0 + (param - first) * stride
            vmask = (1 << width) - 1
            v = (1 if value else 0) if width == 1 else (value & vmask)
            return SettingWrite(setting, v << shift, vmask << shift)
    return None                     # unknown: the driver logs and writes nothing


def map_param(channel_type: int, ch: int, param: int, value: int,
              num_preamps: int = 2, num_hiz: int = 1):
    if channel_type == PREAMP_CHANNEL_TYPE:
        return map_preamp(ch, param, value, num_preamps, num_hiz)
    if channel_type == MONITOR_CHANNEL_TYPE:
        return map_monitor(param, value)
    raise MixerParamError(f"channel type {channel_type} has no USB mapping")


def setting_address(index: int) -> int:
    """FPGA word address of setting `index` (word A; word B follows)."""
    for first, end, addr in SETTINGS_GROUPS:
        if first <= index < end:
            return addr + 2 * (index - first)
    raise IndexError(index)


def encode_pair(value: int, mask: int) -> tuple:
    return (((mask & 0xFFFF) << 16) | (value & 0xFFFF),
            (mask & 0xFFFF0000) | ((value >> 16) & 0xFFFF))


class SettingsMailbox:
    """The host side of the 38 settings: cache, masks, zero passes, SEQ."""

    def __init__(self):
        self.value = [0] * NUM_SETTINGS
        self.mask = [0] * NUM_SETTINGS
        # The registers hold zeros: no zero pass owed (the Windows driver
        # starts every setting this way, 0x140007e42)
        self.zeroed = [True] * NUM_SETTINGS
        self.seq_wr = None          # unknown until adopt_seq()

    def merge(self, index: int, value: int, mask: int) -> None:
        if not 0 <= index < NUM_SETTINGS:
            raise IndexError(index)
        mask &= 0xFFFFFFFF
        self.value[index] = (self.value[index] & ~mask) | (value & mask)
        self.mask[index] |= mask

    @property
    def pending(self) -> bool:
        """A change waits.  A zero pass alone does not count: it goes out
        with the next change, as in the Windows capture (the driver flushes
        from its readback timer only when a write is pending)."""
        return any(self.mask)

    def build_flush(self) -> list:
        """[(word address, bytes)] of the group blocks to write; updates
        the masks and zero-pass state as the flush does."""
        writes = []
        for first, end, addr in SETTINGS_GROUPS:
            words = []
            needed = False
            for i in range(first, end):
                if self.mask[i]:
                    a, b = encode_pair(self.value[i], self.mask[i])
                    self.mask[i] = 0
                    self.zeroed[i] = False
                    needed = True
                else:
                    a = b = 0
                    if not self.zeroed[i]:
                        self.zeroed[i] = True
                        needed = True
                words += [a, b]
            if needed:
                writes.append((addr, struct.pack(f"<{len(words)}I", *words)))
        return writes

    def adopt_seq(self, regs) -> int:
        """Start from the device's SEQ_RD (the Windows mixer init does)."""
        self.seq_wr = regs.read1(SEQ_RD)
        return self.seq_wr

    def flush(self, regs) -> bool | None:
        """One flush attempt.  True: written; False: the DSP has not
        acknowledged the last batch yet (try later); None: nothing to do."""
        if not self.pending:
            return None
        if self.seq_wr is None:
            self.adopt_seq(regs)
        if regs.read1(SEQ_RD) != self.seq_wr:
            return False
        for addr, data in self.build_flush():
            regs.write(addr, struct.unpack(f"<{len(data) // 4}I", data))
        self.seq_wr = (self.seq_wr + 1) & 0xFFFFFFFF
        regs.write(SEQ_WR, [self.seq_wr])
        return True
