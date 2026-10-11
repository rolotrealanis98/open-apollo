"""
Hardware register I/O via /dev/ua_apollo0 ioctls.

Provides a Python ctypes wrapper around the Linux driver's ioctl interface
for reading/writing BAR0 registers. Implements:

1. Mixer sequence handshake — 38-setting DSP register protocol
2. Preamp controls via DSP mixer settings (NOT CLI registers)
3. Bus parameter writes — fader levels, pan, send gains
4. Value encoding helpers — float↔fixed, dB↔linear, bool↔int

Register map (from CPcieDeviceMixer kext disassembly):
    Mixer registers live at BAR0 + 0x3800 (NOT BAR0 + 0x0000).
    CPcieDeviceMixer::Initialize does: this->mixerBase = barBase + 0x3800

    0x3808  MIXER_SEQ_WR    Mixer sequence counter (host → DSP)
    0x380C  MIXER_SEQ_RD    Mixer sequence counter (DSP readback)
    0x38B4+ MIXER settings  38 settings, 3 offset ranges (see setting_offset())

    Setting value encoding (paired 32-bit writes):
        wordA = (changed_mask[15:0]  << 16) | value[15:0]
        wordB = (changed_mask[31:16] << 16) | value[31:16]
    The upper 16 bits of each word are the changed-bits mask.
    If mask is 0, the DSP ignores the write (no ack).

CLI register interface (BAR0 + 0xC3F4):
    0xC3F4  CLI_ENABLE      Enable CLI interface
    0xC3F8  CLI_STATUS      Command status / length
    0xC3FC  CLI_RESP_LEN    Response length
    0xC400  CLI_CMD_BUF     Command data (128 bytes / 32 uint32)
    0xC480  CLI_RESP_BUF    Response data (128 bytes / 32 uint32)

Ioctl interface (from driver/ua_ioctl.h):
    UA_IOCTL_READ_REG            _IOWR('U', 0x10, struct ua_reg_io)
    UA_IOCTL_WRITE_REG           _IOW('U', 0x11, struct ua_reg_io)
    UA_IOCTL_SET_MIXER_BUS_PARAM _IOW('U', 0x30, struct ua_mixer_bus_param)
    UA_IOCTL_SET_MIXER_PARAM     _IOW('U', 0x31, struct ua_mixer_param)
    UA_IOCTL_SET_DRIVER_PARAM    _IOW('U', 0x34, struct ua_driver_param)
    UA_IOCTL_GET_DRIVER_PARAM    _IOWR('U', 0x35, struct ua_driver_param)
    UA_IOCTL_WRITE_MIXER_SETTING _IOW('U', 0x36, struct ua_mixer_setting)
    UA_IOCTL_READ_MIXER_SETTING  _IOWR('U', 0x37, struct ua_mixer_setting)
    UA_IOCTL_CLI_COMMAND         _IOWR('U', 0x40, struct ua_cli_command)
"""

import errno
import fcntl
import glob
import logging
import math
import os
import re
import struct
import subprocess
import time
from dataclasses import dataclass
from typing import Any

import model_features as mf

log = logging.getLogger(__name__)

# ── Ioctl constants ─────────────────────────────────────────────────

UA_IOCTL_MAGIC = ord('U')

# Linux ioctl direction bits
_IOC_NONE = 0
_IOC_WRITE = 1
_IOC_READ = 2

_IOC_NRBITS = 8
_IOC_TYPEBITS = 8
_IOC_SIZEBITS = 14
_IOC_DIRBITS = 2

_IOC_NRSHIFT = 0
_IOC_TYPESHIFT = _IOC_NRSHIFT + _IOC_NRBITS
_IOC_SIZESHIFT = _IOC_TYPESHIFT + _IOC_TYPEBITS
_IOC_DIRSHIFT = _IOC_SIZESHIFT + _IOC_SIZEBITS


def _IOC(direction, type_, nr, size):
    return (direction << _IOC_DIRSHIFT) | (type_ << _IOC_TYPESHIFT) | \
           (nr << _IOC_NRSHIFT) | (size << _IOC_SIZESHIFT)


def _IOWR(type_, nr, size):
    return _IOC(_IOC_READ | _IOC_WRITE, type_, nr, size)


def _IOW(type_, nr, size):
    return _IOC(_IOC_WRITE, type_, nr, size)


# struct ua_reg_io { uint32_t offset; uint32_t value; } = 8 bytes
_REG_IO_SIZE = 8

UA_IOCTL_READ_REG = _IOWR(UA_IOCTL_MAGIC, 0x10, _REG_IO_SIZE)
UA_IOCTL_WRITE_REG = _IOW(UA_IOCTL_MAGIC, 0x11, _REG_IO_SIZE)

# struct ua_mixer_param { ch_type, ch_idx, param_id, value } = 16 bytes
# Maps to macOS IOKit SEL131 (SetMixerParam, 24 bytes over IOKit).
# On Linux, the driver handles routing via DSP mixer settings:
#   ch_type=1 (preamp) → mixer setting[param_id + 7]
#   ch_type=2 (monitor) → mixer setting (TBD)
_MIXER_PARAM_SIZE = 16
UA_IOCTL_SET_MIXER_PARAM = _IOW(UA_IOCTL_MAGIC, 0x31, _MIXER_PARAM_SIZE)

# struct ua_mixer_bus_param { bus_id, sub_param, value_u32, flags } = 16 bytes
# Maps to macOS IOKit SEL130 (SetMixerBusParam, 136 bytes over IOKit).
# value_u32 is IEEE 754 float packed as uint32 (linear gain).
_MIXER_BUS_PARAM_SIZE = 16
UA_IOCTL_SET_MIXER_BUS_PARAM = _IOW(UA_IOCTL_MAGIC, 0x30, _MIXER_BUS_PARAM_SIZE)
# flags of the bus-param ioctl (0x02 from the captures): this entry is the
# right-output weight (side 1).  One call is one (param, side) entry; the
# kext's API takes both sides, so a pan or stereo send is two calls.
UA_BUS_PARAM_FLAGS = 0x02
UA_BUS_PARAM_F_SIDE_R = 0x100
# This input is muted by a solo elsewhere: the driver applies the kext's
# negated pair / -1.0e-9 rule to the entry (the value stays the normal
# coefficient, so unsolo only has to write it again without the flag).
# Needs UA_FEAT_BUS_SOLO_FLAG; an older driver would ignore the flag and
# write the live coefficient, so it must not be sent there.
UA_BUS_PARAM_F_SOLO_MUTE = 0x200

# struct ua_mixer_setting { index, value, mask, reserved } = 16 bytes
# Atomic mixer setting read/write via sequence handshake in kernel.
# Replaces the racy userspace reg_read/reg_write sequence.
_MIXER_SETTING_SIZE = 16
UA_IOCTL_WRITE_MIXER_SETTING = _IOW(UA_IOCTL_MAGIC, 0x36, _MIXER_SETTING_SIZE)
UA_IOCTL_READ_MIXER_SETTING = _IOWR(UA_IOCTL_MAGIC, 0x37, _MIXER_SETTING_SIZE)

# struct ua_cli_command { cmd_len, resp_len, cmd_data[128], resp_data[128] } = 264 bytes
# Full CLI command/response via kernel (avoids userspace CLI register access).
_CLI_COMMAND_SIZE = 264
UA_IOCTL_CLI_COMMAND = _IOWR(UA_IOCTL_MAGIC, 0x40, _CLI_COMMAND_SIZE)

# struct ua_mixer_readback { input[4], data[41] } = 180 bytes
# Returns all 38 mixer settings atomically with XOR checksum.
# data[38] = XOR of data[0..37], data[39] = ~data[38], data[40] = 0.
_MIXER_READBACK_SIZE = (4 + 41) * 4  # 180 bytes
UA_IOCTL_GET_MIXER_READBACK = _IOWR(UA_IOCTL_MAGIC, 0x33, _MIXER_READBACK_SIZE)

# struct ua_hw_readback { status, data[40] } = 164 bytes
# Reads DSP readback registers (BAR0+0x3810/0x3814) — status + 40 data words.
# status=1 means data valid, driver re-arms automatically.
def _IOR(type_, nr, size):
    return _IOC(_IOC_READ, type_, nr, size)

_HW_READBACK_SIZE = (1 + 40) * 4  # 164 bytes
UA_IOCTL_GET_HW_READBACK = _IOR(UA_IOCTL_MAGIC, 0x41, _HW_READBACK_SIZE)

# struct ua_driver_param { param_id, reserved, value(u64) } = 16 bytes
_DRIVER_PARAM_SIZE = 16
UA_IOCTL_SET_DRIVER_PARAM = _IOW(UA_IOCTL_MAGIC, 0x34, _DRIVER_PARAM_SIZE)
UA_IOCTL_GET_DRIVER_PARAM = _IOWR(UA_IOCTL_MAGIC, 0x35, _DRIVER_PARAM_SIZE)

# struct ua_fw_load { fw_data(u64), fw_size(u32), reserved(u32) } = 16 bytes
# DSP firmware loading and connect ioctls.
def _IO(type_, nr):
    return _IOC(_IOC_NONE, type_, nr, 0)

# struct ua_dsp_info { index, bank_base, status, reserved[5] } = 32 bytes
# Returns per-DSP status (0=idle, 1=running). Polls register at DSP bank base.
_DSP_INFO_SIZE = 32
UA_IOCTL_GET_DSP_INFO = _IOWR(UA_IOCTL_MAGIC, 0x02, _DSP_INFO_SIZE)

# struct ua_device_info {
#     fpga_rev, device_type, subsystem_id, num_dsps,
#     fw_version, fw_v2, serial, ext_caps, reserved[8]
# } = 16 * u32 = 64 bytes.  Identifies which Apollo model is attached so the
# daemon can select the matching device map (x4 vs x8p vs ...).
_DEVICE_INFO_SIZE = 64
# errnos that mean the PCI device went away under an open fd (the driver
# returns -ENODEV for every ioctl after remove)
_DEVICE_LOST_ERRNOS = (errno.ENODEV, errno.EIO, errno.ENXIO, errno.EBADF)
UA_IOCTL_GET_DEVICE_INFO = _IOR(UA_IOCTL_MAGIC, 0x01, _DEVICE_INFO_SIZE)

# struct ua_version { u32 abi_major, abi_minor, driver_version, features;
#                     char driver_name[16]; u32 reserved[4]; } = 48 bytes.
# ABI rule (driver/ua_ioctl.h): minor bumps for additions, major for breaks.
_VERSION_SIZE = 48
UA_IOCTL_GET_VERSION = _IOR(UA_IOCTL_MAGIC, 0x04, _VERSION_SIZE)
UA_ABI_MAJOR = 1
UA_FEAT_EVENTS = 1 << 0
UA_FEAT_PUBLIC_TYPE = 1 << 1
UA_FEAT_BUS_PARAM_SIDE = 1 << 2
UA_FEAT_MIXER_PARAM_T0 = 1 << 3
UA_FEAT_KIND_GATES = 1 << 4
UA_FEAT_MIXER_STATE = 1 << 5
UA_FEAT_SYSTEM_INFO = 1 << 6
UA_FEAT_BUS_PARAM_BATCH = 1 << 7
UA_FEAT_PAUSE_INFO = 1 << 8
UA_FEAT_PAUSE_EVENTS = 1 << 9
UA_FEAT_UNIT_GROUP = 1 << 10
UA_FEAT_BUS_SOLO_FLAG = 1 << 11
# ABI 1.2: the first four reserved words of struct ua_system_info carry
# pcie_link_speed, pcie_link_width, clock_src_mask and fw_rate_mask
UA_FEAT_SYSINFO_EXT = 1 << 12

# struct ua_system_info (ABI 1.x, 148 bytes): the first 12 u32, the 16-byte
# serial, 13 more u32 and reserved[8].  Field names in order.
_SYSTEM_INFO_SIZE = 148
UA_IOCTL_GET_SYSTEM_INFO = _IOR(UA_IOCTL_MAGIC, 0x05, _SYSTEM_INFO_SIZE)
_SYSTEM_INFO_HEAD = ("fpga_rev", "device_type", "public_type", "subsystem_id",
                     "pci_device", "num_dsps", "fw_version", "fw_v2",
                     "ext_caps", "hw_uid_lo", "hw_uid_hi", "mixer_kind")
_SYSTEM_INFO_TAIL = ("sample_rate", "rate_index", "clock_source",
                     "clock_lock_valid", "clock_locked", "connected",
                     "transport_running", "play_channels", "rec_channels",
                     "unit_group", "unit_index", "unit_count", "unit_role")

# ABI 1.2 (UA_FEAT_SYSINFO_EXT): four more u32 right after unit_role, then
# reserved[4]; the size stays 148.
#   pcie_link_speed  PCI_EXP_LNKSTA current link speed code (1 = 2.5 GT/s
#                    Gen1, 2 = 5 GT/s Gen2, 3 = 8 GT/s Gen3, ...), 0 unknown
#   pcie_link_width  negotiated lanes (1, 2, 4), 0 unknown
#   clock_src_mask   0xC08C[31:16]: bit n set = clock source value n offered
#   fw_rate_mask     0xC08C[7:0]: bit n set = rate index n offered
_SYSTEM_INFO_EXT = ("pcie_link_speed", "pcie_link_width", "clock_src_mask",
                    "fw_rate_mask")
_SYSTEM_INFO_EXT_OFF = 64 + 4 * len(_SYSTEM_INFO_TAIL)

# ABI 1.4 (UA_FEAT_FW_RELEASE): two more u32 after fw_rate_mask, then
# reserved[2]; the size stays 148.
#   fw_release  BAR0 0x0008, the firmware version word UA's driver reports
#               (major [31:24], minor [23:16], build [15:0]; x4 0x0b0fc9b3
#               = 11.15.51635).  fw_version is DSP 0's status word.
#   dev_info    BAR0 0x01AC for the types UA's driver reads it for, else
#               0xFFFFFFFF; [19:16] feed the per-device firmware checks
UA_FEAT_FW_RELEASE = 1 << 14
# ABI 1.6: the load words of struct ua_dsp_info (see get_dsp_info)
UA_FEAT_DSP_LOAD = 1 << 16
UA_DSP_INFO_F_LOADS = 0x1
_SYSTEM_INFO_FW = ("fw_release", "dev_info")
_SYSTEM_INFO_FW_OFF = _SYSTEM_INFO_EXT_OFF + 4 * len(_SYSTEM_INFO_EXT)

# ABI 1.7 (UA_FEAT_CLOCK_SOURCES): the device's clock-source menu,
# struct ua_clock_sources { u32 struct_size, count, mask, reserved;
# { u32 value, flags; char name[32]; } src[16] } = 656 bytes
UA_FEAT_CLOCK_SOURCES = 1 << 17
_CLOCK_SOURCES_SIZE = 16 + 16 * 40
UA_IOCTL_GET_CLOCK_SOURCES = _IOR(UA_IOCTL_MAGIC, 0x08, _CLOCK_SOURCES_SIZE)
UA_CLOCK_SRC_F_DEVICE_NAME = 0x1

_FW_LOAD_SIZE = 16
UA_IOCTL_LOAD_FIRMWARE = _IOW(UA_IOCTL_MAGIC, 0x50, _FW_LOAD_SIZE)
UA_IOCTL_DSP_CONNECT = _IO(UA_IOCTL_MAGIC, 0x51)

# ── Register offsets ────────────────────────────────────────────────
# Mixer registers are at BAR0 + 0x3800, discovered from kext disassembly
# of CPcieDeviceMixer::Initialize (barBase + 0x3800)

MIXER_BAR_OFFSET = 0x3800
REG_MIXER_SEQ_WR = MIXER_BAR_OFFSET + 0x08   # 0x3808
REG_MIXER_SEQ_RD = MIXER_BAR_OFFSET + 0x0C   # 0x380C
MIXER_SETTING_COUNT = 38

# ── CLI Register Interface (ARM microcontroller communication) ──────
# From ua_apollo.h and kext disassembly. The CLI provides a command/response
# interface to the ARM MCU that controls preamps, phantom power, etc.

REG_CLI_ENABLE   = 0xC3F4  # Write 1 to enable CLI
REG_CLI_STATUS   = 0xC3F8  # Command status / command length
REG_CLI_RESP_LEN = 0xC3FC  # Response data length (bytes)
REG_CLI_CMD_BUF  = 0xC400  # Command buffer (128 bytes, 32 uint32)
REG_CLI_RESP_BUF = 0xC480  # Response buffer (128 bytes, 32 uint32)
CLI_CMD_BUF_SIZE = 128
CLI_RESP_BUF_SIZE = 128

# ── ARM Parameter IDs ──────────────────────────────────────────────
# From kext disassembly of CUAD2DeviceMixer::SetInputParam, confirmed
# by IoKit capture (tools/captures/mixer/iokit-baseline.log).
#
# The 36-byte SetInputParam struct layout (from IoKit capture):
#   word[0]:  paramID  (0x00-0x17)
#   word[1-4]: reserved (0)
#   word[5]:  hwID     (channel hardware ID)
#   word[6-7]: reserved (0)
#   word[8]:  0x08     (data size / flags)
#
# ParamIDs route to either ARM MCU (preamp controls) or DSP.
# _isDSPInputParam() returns true for paramIDs 0x05, 0x13, 0x16.

# Preamp parameter IDs — VERIFIED via DTrace SEL131 capture.
# All preamp params go through DSP mixer settings (SetMixerParam ch_type=1),
# NOT through ARM CLI.  The kext routes them to mixer setting[param_id + 7].
# HiZ has NO software control — hardware auto-detect only (rb_data[0] bit 24).
PREAMP_PARAM_IDS = {
    "MicLine":  0x00,   # 1=Line, 0=Mic
    "Pad":      0x01,   # on=1, off=0
    "48V":      0x03,   # on=1, off=0 (triggers ARM safety blink)
    "LowCut":   0x04,   # on=1, off=0 (NOT HiZ — verified via DTrace)
    "Phase":    0x05,   # invert=0xbf800000 (-1.0f), normal=0x3f800000 (+1.0f)
    "GainA":    0x0A,   # Gain_A (1st write, dB-10) — verified DTrace
    "GainB":    0x09,   # Gain_B (2nd write, dB-10) — verified DTrace
    "GainC":    0x06,   # Gain_C (periodic 3rd write, dB-9) — verified DTrace
    "Route":    0x13,   # Channel routing mask (0x0001ffff)
    "Level":    0x16,   # Default level (0xa0 = -16dB)
}

# Legacy alias for existing code that uses ARM_PARAM_IDS
ARM_PARAM_IDS = PREAMP_PARAM_IDS

# ── Hardware IDs for Apollo x4 analog inputs ────────────────────────
# From IoKit capture analysis: each analog input has a unique hwID
# with stride 0x0800. These IDs are passed in the SetInputParam struct
# at word[5] to identify which preamp channel to control.

MAX_PREAMP_CHANNELS = 8  # the driver's UA_MAX_PREAMP_CH; models have 0-8
HW_ID_STRIDE = 0x0800
# hwID of preamp input N.  Inputs 1-4 are verified on the x4; 5-8 (Apollo 8p,
# x8p) continue the stride, which is assumed (not captured).
APOLLO_PREAMP_HW_IDS = {
    n: n * HW_ID_STRIDE for n in range(MAX_PREAMP_CHANNELS)
}
APOLLO_X4_HW_IDS = {n: APOLLO_PREAMP_HW_IDS[n] for n in range(4)}
# Inputs past the preamps are S/PDIF, ADAT, Virtual — no hwID needed.
# How many of a model's inputs are preamps comes from its device map
# (HardwareRouter.preamp_inputs), capped at MAX_PREAMP_CHANNELS.
_X4_PREAMP_COUNT = 4   # used only when the router has no state tree

# ── Bus ID Mapping (from IOKit SEL130 capture + hw probe, Apollo x4) ──
# Bus IDs identify mixer bus channels for SetMixerBusParam (SEL130).
# VERIFIED: setting_index == bus_id for ALL 32 buses (0x00-0x1f).
# See docs/iokit-mixer-mapping.md for full capture analysis.
#
# Complete bus map (Apollo x4):
#   0x0000-0x0003: Analog In 1-4 (mono, mic/line)
#   0x0004-0x0007: CUE1 L/R + CUE2 L/R (output mix buses)
#   0x0008-0x0009: S/PDIF In L/R
#   0x000a-0x000b: Talkback 1/2
#   0x000c-0x000f: ADAT In 1-4
#   0x0010, 0x0012: AUX 1/2 Return (0x0011/0x0013 reserved R slots)
#   0x0014-0x0017: ADAT In 5-8
#   0x0018-0x001f: Virtual In 1-8 (DAW playback, 4 stereo pairs)

# Talkback bus constants (DTrace-verified)
# Talkback faders write sub=0 only.
# Unity gain ≈ 3.976353 (NOT 0.707 like normal inputs).
TALKBACK_BUS_START = 0x0A
TALKBACK_UNITY = 3.976353

# Hardware bus IDs of the ADAT inputs: 1-4 sit at 0x0C-0x0F, 5-8 after the
# AUX return slots at 0x14-0x17.  The same slots carry the second block of
# 8 channels of the 16-channel models (Apollo 16 / x16 analog 9-16, x16D
# Dante 9-16, Rack MADI 9-16): devices/*.json synch_in agrees for all 25.
_ADAT_BUS_IDS = (0x0C, 0x0D, 0x0E, 0x0F, 0x14, 0x15, 0x16, 0x17)
_ANALOG_BUS_COUNT = 8        # analog inputs 1-8 at bus 0x00-0x07
_SPDIF_BUS_IDS = (0x08, 0x09)   # S/PDIF L/R, or AES/EBU 1/2
_VIRTUAL_BUS_BASE = 0x18     # Virtual 1-8 at 0x18-0x1F
_VIRTUAL_BUS_COUNT = 8

_INPUT_NAME_RE = re.compile(
    r'^(ANALOG|ADAT|VIRTUAL|DANTE|MADI)\s+(\d+)$')
_SPDIF_NAME_RE = re.compile(r'^S/?PDIF\s*([LR])$')
_AES_NAME_RE = re.compile(r'^AES(?:/EBU)?\s*([12])$')


def bus_id_for_input_name(name) -> int | None:
    """Hardware bus ID for a tree input, from its device-map name.

    The state tree numbers inputs contiguously, in the order of the device
    map (x4: ANALOG 1-4, ADAT 1-8, S/PDIF L/R, VIRTUAL 1-8, TALKBACK), while
    the hardware bus IDs have gaps (CUE, talkback and AUX slots in between).
    Deriving the ID from the name keeps the table right for any map that
    names its inputs this way: analog 1-8 at 0x00-0x07 (x4: 1-4), a second
    block of eight (ANALOG / DANTE / MADI 9-16) in the ADAT slots, AES/EBU
    1/2 where the other models have S/PDIF L/R.  Returns None for a name
    with no bus (a placeholder like "N/A 1", or a channel this table has no
    bus for).
    """
    if not isinstance(name, str):
        return None
    n = name.strip().upper()
    if n == "TALKBACK":
        return TALKBACK_BUS_START
    m = _SPDIF_NAME_RE.match(n)
    if m:
        return _SPDIF_BUS_IDS[0 if m.group(1) == "L" else 1]
    m = _AES_NAME_RE.match(n)
    if m:
        return _SPDIF_BUS_IDS[int(m.group(1)) - 1]
    m = _INPUT_NAME_RE.match(n)
    if not m:
        return None
    kind, num = m.group(1), int(m.group(2))
    if kind in ("ANALOG", "DANTE", "MADI"):
        if 1 <= num <= _ANALOG_BUS_COUNT:
            return num - 1
        if _ANALOG_BUS_COUNT < num <= _ANALOG_BUS_COUNT + len(_ADAT_BUS_IDS):
            return _ADAT_BUS_IDS[num - _ANALOG_BUS_COUNT - 1]
        return None
    if kind == "ADAT" and 1 <= num <= len(_ADAT_BUS_IDS):
        return _ADAT_BUS_IDS[num - 1]
    if kind == "VIRTUAL" and 1 <= num <= _VIRTUAL_BUS_COUNT:
        return _VIRTUAL_BUS_BASE + num - 1
    return None


def build_input_bus_table(names) -> tuple:
    """State tree input index -> bus ID (None = no bus) for a list of names."""
    return tuple(bus_id_for_input_name(n) for n in names)


# Apollo x4 device map input order (device_maps/device_map_apollo_x4.json).
_X4_INPUT_NAMES = (
    "ANALOG 1", "ANALOG 2", "ANALOG 3", "ANALOG 4",
    "ADAT 1", "ADAT 2", "ADAT 3", "ADAT 4",
    "ADAT 5", "ADAT 6", "ADAT 7", "ADAT 8",
    "S/PDIF L", "S/PDIF R",
    "VIRTUAL 1", "VIRTUAL 2", "VIRTUAL 3", "VIRTUAL 4",
    "VIRTUAL 5", "VIRTUAL 6", "VIRTUAL 7", "VIRTUAL 8",
    "TALKBACK", "N/A 1",
)
_INPUT_IDX_TO_BUS = build_input_bus_table(_X4_INPUT_NAMES)


def input_bus_id(input_idx: int) -> int | None:
    """Bus ID for an input of the Apollo x4 map (state tree index -> bus).

    HardwareRouter derives its own table from the loaded device map; this
    is the x4 default.  None when the index has no bus.
    """
    if 0 <= input_idx < len(_INPUT_IDX_TO_BUS):
        return _INPUT_IDX_TO_BUS[input_idx]
    return None

# Aux bus IDs: stride of 2 per aux index
def aux_bus_id(aux_idx: int) -> int:
    """Get bus ID for an aux bus. Verified: aux 0=0x0010, aux 1=0x0012."""
    return 0x0010 + aux_idx * 2

# ── SEL130 Sub-Parameter Indices ──────────────────────────────────
# From the Mixer Engine's CMxInput::_UpdateDSPBusParameters jump table
# (apollo-linux/docs/re-notes/parity/04-engine-surface.md §0.1).  Each
# input bus carries these coefficients:
#     sub=0: main mix coefficient (fader x pan)
#     sub=1: CUE 1 send level
#     sub=2: CUE 2 send level
#     sub=3: AUX 1 send level
#     sub=4: AUX 2 send level
#     sub=5: CUE 3 send level
#     sub=6: CUE 4 send level
# A fader or pan move writes sub 0 only.  (The DTrace "triplet" 0/3/4
# wrote the fader value into the two AUX send slots; the engine never
# does that.)  Aux buses use the same layout for their CUE sends.
SUB_PARAM_MIX = 0           # Main mix coefficient (fader + pan)
SUB_PARAM_CUE1 = 1          # CUE 1 send level
SUB_PARAM_CUE2 = 2          # CUE 2 send level
SUB_PARAM_AUX1 = 3          # AUX 1 send level
SUB_PARAM_AUX2 = 4          # AUX 2 send level
SUB_PARAM_CUE3 = 5          # CUE 3 send level
SUB_PARAM_CUE4 = 6          # CUE 4 send level
# Legacy names (the old DTrace reading of the same slots)
SUB_PARAM_GAIN_L = SUB_PARAM_AUX1
SUB_PARAM_GAIN_R = SUB_PARAM_AUX2
SUB_PARAM_MUTE_L = SUB_PARAM_CUE3
SUB_PARAM_MUTE_R = SUB_PARAM_CUE4

# Send ID (the "ID" property of a send in the device map) -> sub-param.
_SEND_ID_TO_SUB = {
    "aux1": SUB_PARAM_AUX1, "aux2": SUB_PARAM_AUX2,
    "cue1": SUB_PARAM_CUE1, "cue2": SUB_PARAM_CUE2,
    "cue3": SUB_PARAM_CUE3, "cue4": SUB_PARAM_CUE4,
}

# Input send index -> sub-param, used when the map has no send IDs.
# x4 map: sends/0-1 = AUX 1-2, sends/2-5 = CUE 1-4.
_SEND_IDX_TO_SUB = {
    0: SUB_PARAM_AUX1,
    1: SUB_PARAM_AUX2,
    2: SUB_PARAM_CUE1,
    3: SUB_PARAM_CUE2,
    4: SUB_PARAM_CUE3,
    5: SUB_PARAM_CUE4,
}

# Aux bus send index -> sub-param (x4 map: auxs/N/sends/0-3 = CUE 1-4).
_AUX_SEND_IDX_TO_SUB = {
    0: SUB_PARAM_CUE1,
    1: SUB_PARAM_CUE2,
    2: SUB_PARAM_CUE3,
    3: SUB_PARAM_CUE4,
}

IN_BASE = "/devices/0/inputs"

# ── Monitor Parameter IDs (SEL131, channel_type=2) ───────────────
# From IOKit capture of SetMixerParam with w2=2 (monitor type).
MONITOR_PARAM = {
    "CRMonitorLevel":  0x01,   # w3=1, value = 192 + (dB × 2)
    "MonitorMute":     0x03,   # w3=0, value = 2 (muted) / 0 (unmuted)
    "MixToMono":       0x03,   # w3=0, value = 1/0 (VERIFIED: same param as MonitorMute!)
    "DimOn":           0x44,   # w3=0, value = 1/0
    # Pad and MirrorsToDigital — VERIFIED via DTrace:
    # "Pad" on outputs/18 = OutputRef (param 0x32, ch_idx=1): 1=-10dBV, 0=+4dBu
    # "MirrorsToDigital" = DigitalMirror (param 0x1e, ch_idx=10): 1=on, 0=off
    "Pad":             0x32,   # VERIFIED: OutputRef +4dBu/-10dBV, ch_idx=1
    "MirrorsToDigital": 0x1e,  # VERIFIED: DigitalMirror, ch_idx=9
    # Cache-only controls — route through SET_MIXER_PARAM for ALSA cache sync:
    "MixInSource":     0x04,   # ch_idx=1: 0=MIX, 1=CUE1, 2=CUE2 (MonitorSrc)
    "HP1Source":       0x3f,   # ch_idx=1: 0=CUE1, 1=CUE2 (HP1 routing)
    "HP2Source":       0x40,   # ch_idx=1: 0=CUE1, 1=CUE2 (HP2 routing)
    "DimLevel":        0x43,   # ch_idx=0: 1-7 (dim attenuation step)
    "TalkbackOn":      0x46,   # ch_idx=1 ON / ch_idx=9 OFF (DTrace-verified)
    "DigitalOutputMode": 0x21, # ch_idx=0: 0=SPDIF, 8=ADAT
    "SRConvert":       0x1f,   # ch_idx=1: sample rate converter on/off (S/PDIF inputs)
    "DSPSpanning":     0x16,   # ch_idx=0: DSP pairing/spanning mode
    # CUE mirror / output routing — all ch_idx=9 unless noted
    "MirrorA":         0x2e,   # ch_idx=9: CUE1 mirror source (see MIRROR_SOURCE_MAP)
    "MirrorB":         0x2f,   # ch_idx=9: CUE2 mirror source (see MIRROR_SOURCE_MAP)
    # 0x3b / 0x3c are the kext's AltMonEnabled (kPropAltMonEnabled 0xf6,
    # setting[15] bits 8 / 12; parity/14 s4, medium confidence), not a
    # "mirror enable".  MirrorEnableA/B stay as the names this daemon uses.
    "MirrorEnableA":   0x3b,   # ch_idx=9: = AltMon1Enabled (1/0)
    "MirrorEnableB":   0x3c,   # ch_idx=9: = AltMon2Enabled (1/0)
    "AltMon1Enabled":  0x3b,   # kPropAltMonEnabled, alt monitor 1
    "AltMon2Enabled":  0x3c,   # kPropAltMonEnabled, alt monitor 2
    "AltMon1Trim":     0x38,   # kPropAltMonTrim: clamp(int(trim*2 + 60.5), 0, 0x78)
    "AltMon2Trim":     0x39,
    "AltMonSelection": 0x3a,   # kPropAltMonSelection, 2 bits (setting[13])
    # CUE mono/mix — all ch_idx=0
    "CUE1Mono":        0x06,   # ch_idx=0: CUE1 mono (1/0)
    "CUE2Mono":        0x08,   # ch_idx=0: CUE2 mono (1/0)
    "CUE1Mix":         0x05,   # ch_idx=0: CUE1 mix (0=on, 2=off — INVERTED!)
    "CUE2Mix":         0x07,   # ch_idx=0: CUE2 mix (0=on, 2=off — INVERTED!)
    # Headphone level: the engine sends 0x02 for the first headphone output
    # and 0x66 for the next (CMxOutput::UpdateHardwareState @0x10007efb8).
    # HP1 = setting[2] bits 15:8 (readback word 2).
    "HP1Level":        0x02,   # ch_idx=1: raw 192+dB*2
    "HP2Level":        0x66,   # ch_idx=1: raw 192+dB*2 (setting[14] bits 7:0)
    # Device identification
    "Identify":        0x1d,   # ch_idx=0: flash front panel LEDs (1=on, 0=off)
    # TalkbackConfig
    "TBConfig":        0x47,   # ch_idx=0: talkback configuration
    # Other params the Mixer Engine sends from properties (parity/14 s4/s8)
    "OutputsAttenuate": 0x20,  # kPropOutputsAttenuate, setting[7] bit 10
    "CRPGABypass":     0x3e,   # kPropCRPGABypass, setting[15] bit 9 (value inverted)
    "RemoteMonitor":   0x41,   # kPropRemoteMonitorController, setting[15] bit 7
    "JackSensingDisabled": 0x45,  # kPropInputJackSensingDisabled, setting[7] bit 11
    "TalkbackMaster":  0x48,   # kPropTalkbackMaster, setting[7] bit 13
    "Enable24dB":      0x49,   # kPropEnable24dBMode; the x4 (kind 14) refuses it
    "OutPadA":         0x19,   # kPropPad of a line-out pair: setting[0] bit 28 ..
    "OutPadB":         0x1a,
    "OutPadC":         0x1b,
    "OutPadD":         0x1c,   # .. bit 31
    # CUE 3 / CUE 4 outputs (synch 0x18 / 0x1a; parity/14 s4, M for the
    # output they belong to)
    "CUE3Mix":         0x22,
    "CUE3Mono":        0x23,
    "CUE4Mix":         0x24,
    "CUE4Mono":        0x25,
    "MirrorC":         0x30,   # CUE3 mirror source
    "MirrorD":         0x31,   # CUE4 mirror source
}
# Per CUE output (0-based): mono / mix / mirror-source params, and the
# mirror enable (AltMon enable bits 0x3b/0x3c; none known for CUE 3/4)
CUE_MONO_PARAMS = (0x06, 0x08, 0x23, 0x25)
CUE_MIX_PARAMS = (0x05, 0x07, 0x22, 0x24)
CUE_MIRROR_PARAMS = (0x2E, 0x2F, 0x30, 0x31)
CUE_MIRROR_ENABLE_PARAMS = (0x3B, 0x3C, None, None)

# Alt monitors: output pair n of the line outputs -> alt monitor n+1
ALTMON_TRIM_PARAMS = (0x38, 0x39)
ALTMON_ENABLE_PARAMS = (0x3B, 0x3C)
OUT_PAD_PARAMS = (0x19, 0x1A, 0x1B, 0x1C)
# DSP bus-enable ids of CUE 1..4 (SetBusEnable; CueBusCount decides)
CUE_BUS_ENABLE_BUSES = (1, 2, 5, 6)
AUX_MONO_PARAMS = {0: 0x0A, 1: 0x0F}     # CMxAuxBus MixToMono, aux 1 / 2


def encode_altmon_trim(trim_db: float) -> int:
    """kPropAltMonTrim -> param 0x38/0x39: clamp(int(trim*2 + 60.5), 0, 0x78)
    (60 = 0 dB, 0.5 dB steps, -30..+30 dB)."""
    return max(0, min(0x78, int(float(trim_db) * 2 + 60.5)))


# Input param 0x11 (kPropHiZImpedance, inputs 1 and 2 only): the code is the
# index into this table (ConvertHiZImpedanceToHWCode, parity/14 s3)
HIZ_IMPEDANCES_OHM = (2_200_000, 1_000_000, 453_000, 232_000,
                      100_000, 70_000, 47_000, 21_000)
HIZ_IMPEDANCE_PARAM = 0x11
LINK_PARAM = 0x02            # kPropStereo: pair leader (even channel) only
# More input params of CMxInput::UpdateHardwareState (parity/04 s0.1, 14 s3)
LINE_REF_PARAM = 0x01        # kPropPad on a line input = its reference level
KNOB_STEPS_PARAM = 0x10      # kPropPreampInputKnobSteps
SELECTED_PARAM_PARAM = 0x12  # kPropPreampSelectedParam
DIRECT_OUT_PARAM = 0x13      # route word (DSP ring command in the driver)
HIZ_OVERRIDE_PARAM = 0x14    # kPropHiZ, sent only with jack sensing off
PGA_DISABLE_PARAM = 0x15     # kPropPGADisabledInLineMode
# Other params (SetOtherParam) the engine sends from root/mixer/output props
CLIP_HOLD_PARAM = 0x13       # /ClipHold index (setting[1] [30:28])
PEAK_HOLD_PARAM = 0x14       # /PeakHold index (setting[1] [15:13])
FUNC_SWITCH_PARAM = 0x42     # kPropFuncSwitchMode (setting[13] [15:13])
FUNC_SWITCH2_PARAM = 0x83    # kPropFuncSwitch2Mode (x16D)
SURROUND_MODE_PARAM = 0x4a   # kPropSurroundMonitorMode (setting[32] [7:0])
SURROUND_FOLD_PARAM = 0x63   # kPropSurroundStereoFoldDown (setting[7] bit 14)
TB_MIC_SELECT_PARAM = 0x65   # kPropTalkbackMicSelect (setting[31] [25:24])
ADC_DELAY_COMP_PARAM = 0x6a  # CMxMixer::UpdateADCDelayComp (setting[35])
STEREO_SUB_PARAM = 0x87      # kPropStereoSubOutputEnabled (setting[5] bit 9)


def hiz_impedance_code(value) -> int | None:
    """HiZ impedance property value -> hardware code 0-7, None if unknown.

    Accepts the code itself (int 0-7), the resistance in ohms (a number
    >= 1000 or a string such as "1M", "453k", "2.2 MOhm", "100 kΩ"), or a
    label in HIZ_IMPEDANCES_OHM order.
    """
    if isinstance(value, bool):
        return None
    if isinstance(value, (int, float)):
        if float(value) == int(value) and 0 <= int(value) < len(HIZ_IMPEDANCES_OHM):
            return int(value)
        ohms = float(value)
    else:
        m = re.match(r'^\s*([0-9]*\.?[0-9]+)\s*([kKmM]?)\s*(?:ohms?|\u03a9|\u2126)?\s*$',
                     str(value).replace("\u00a0", " "), re.I)
        if not m:
            return None
        ohms = float(m.group(1))
        unit = m.group(2).lower()
        if unit == "k":
            ohms *= 1_000
        elif unit == "m":
            ohms *= 1_000_000
        elif ohms == int(ohms) and 0 <= int(ohms) < len(HIZ_IMPEDANCES_OHM) \
                and "." not in str(value):
            return int(ohms)       # a bare "3" is the code, as the int is
    for code, nominal in enumerate(HIZ_IMPEDANCES_OHM):
        if abs(ohms - nominal) <= nominal * 0.02:
            return code
    return None
MONITOR_CHANNEL_TYPE = 2
PREAMP_CHANNEL_TYPE = 1

# Headphone output level goes to its own monitor param (HP1Level /
# HP2Level above), not the monitor's 0x01.  ch_idx is 1 like the monitor.
HP_BUS_ID = 0x0019
HP_CHANNEL_IDX = 1
# Headphone output (n-th of its IOType) -> monitor param for its level
HP_LEVEL_PARAMS = {0: MONITOR_PARAM["HP1Level"], 1: MONITOR_PARAM["HP2Level"]}

# ── Driver Parameter IDs ─────────────────────────────────────────
# ua_audio.c toslink_in_bit: the driver keeps device-mode bit 0x10 clear
# unless this is on
TOSLINK_IN_BIT_PARAM = "/sys/module/ua_apollo/parameters/toslink_in_bit"
DRIVER_PARAM_SAMPLE_RATE = 0
DRIVER_PARAM_CLOCK_SOURCE = 1
DRIVER_PARAM_TRANSPORT_RUNNING = 2  # Read-only

# /ClockSource name → driver clock source (clock_config low byte, the
# kext's UAD2ClockSource; the device names its sources by the same numbers)
CLOCK_SOURCE_MAP = {
    "Internal": 0x0C,
    "S/PDIF": 0x00,
    "ADAT": 0x05,
    "Word Clock": 0x07,
    "Dante": 0x09,
    # "AES/EBU" and "MADI" are left out on purpose: the device maps of the
    # A16, x16 and Rack offer them, but no UAD2ClockSource value for either
    # is known (kext-rate-meters-hiz-2026-10-10.md Q4 lists only the five
    # above), and writing a guess would select the wrong sync source.
    # A client selecting them gets the "unknown value" warning and nothing
    # reaches the driver.  The driver also rejects values the device does
    # not advertise (ua_clock_source_valid).
}


# The clock-source values UA's Mixer Engine knows (_s_KnownClockSourceList):
# CMxRoot::UpdateClockNames shows the device's own name for each source the
# device offers whose value is one of these, and drops the others.
KNOWN_CLOCK_VALUES = frozenset(CLOCK_SOURCE_MAP.values())


def device_clock_menu(sources: list | None) -> dict[str, int]:
    """{name: value} of the device's clock-source menu (get_clock_sources),
    as the Mixer Engine shows it: the device's own names, only for the
    values it knows, in the device's order.  {} without a usable menu."""
    menu = {}
    if not isinstance(sources, (list, tuple)):
        return menu
    for entry in sources:
        if not isinstance(entry, (list, tuple)) or len(entry) < 2:
            continue
        value, name = entry[0], entry[1]
        if isinstance(value, int) and isinstance(name, str) and \
                value in KNOWN_CLOCK_VALUES and name not in menu:
            menu[name] = value
    return menu if len(menu) > 1 else {}


def clock_source_names(mask: int | None,
                       model_names: list[str] | None) -> list[str] | None:
    """The /ClockSource value list from the device's source mask.

    Bit n of `mask` (0xC08C[31:16]) set means clock source value n is on
    the device's menu; the names are CLOCK_SOURCE_MAP's.  The result keeps
    the model's order and only names the model also offers (devices/*.json
    clock_sources).  Internal is always kept: a unit can always run on its
    own clock, whatever bit 12 says.  None (no mask, an empty mask, or no
    name left but Internal) means "use the static list".
    """
    if not mask or not model_names:
        return None
    names = [n for n in model_names
             if n == "Internal"
             or (n in CLOCK_SOURCE_MAP and CLOCK_SOURCE_MAP[n] < 16
                 and mask & (1 << CLOCK_SOURCE_MAP[n]))]
    return names if len(names) > 1 else None


# Per-lane payload of a PCIe link, MB/s, by LNKSTA speed code
_PCIE_LANE_MBPS = {1: 250, 2: 500, 3: 985, 4: 1969, 5: 3938}


def pcie_link_below_minimum(speed: int | None, width: int | None) -> bool:
    """True when the link is known and slower than x1 Gen1 (250 MB/s).

    The Apollo's own endpoint trains at x1 Gen1 (titan's x4 behind
    Thunderbolt reads LNKSTA speed 1 width 1), which carries every channel
    at 192 kHz with room to spare, so only a link below that is reported.
    An unknown link (0 or None) is not reported."""
    if not speed or not width:
        return False
    lane = _PCIE_LANE_MBPS.get(speed, 985 if speed > 3 else 0)
    return lane * width < _PCIE_LANE_MBPS[1]


def pcie_link_text(speed: int | None, width: int | None) -> str:
    """'x1 Gen2', or '' while unknown."""
    if not speed or not width:
        return ""
    return f"x{width} Gen{speed}"


VALID_SAMPLE_RATES = {44100, 48000, 88200, 96000, 176400, 192000}

# The clock controls whose SET is a transaction: written to the driver
# first, published only when the driver took it
CLOCK_CONTROL_PATHS = ("/SampleRate/value", "/ClockSource/value")


@dataclass(frozen=True)
class GlobalWriteResult:
    """Outcome of a /SampleRate or /ClockSource write (the daemon's own,
    not a UA status code).  `value` is the canonical value to publish."""
    accepted: bool
    value: Any = None
    reason: str = ""


def normalize_sample_rate(value: Any) -> int | None:
    """A rate as an int, or None: an int, an integral finite float or a
    short digit string, and one of VALID_SAMPLE_RATES.  48000.5, True and
    "48000.5" are refused (int() used to truncate the float)."""
    rate = None
    if isinstance(value, int) and not isinstance(value, bool):
        rate = value
    elif isinstance(value, float) and math.isfinite(value) and \
            value.is_integer():
        rate = int(value)
    elif isinstance(value, str) and re.fullmatch(r"[0-9]{1,6}", value.strip()):
        rate = int(value.strip())
    return rate if rate in VALID_SAMPLE_RATES else None


def normalize_global(path: str, value: Any,
                     clock_names=None) -> GlobalWriteResult | None:
    """Validate a clock control before any write or publication.  None for
    a path that is not one (CLOCK_CONTROL_PATHS).  `clock_names` are the
    names a /ClockSource SET may use (the device's menu plus
    CLOCK_SOURCE_MAP; CLOCK_SOURCE_MAP when not given)."""
    if path == "/SampleRate/value":
        rate = normalize_sample_rate(value)
        if rate is None:
            return GlobalWriteResult(False, reason="unsupported sample rate")
        return GlobalWriteResult(True, rate)
    if path == "/ClockSource/value":
        names = CLOCK_SOURCE_MAP if clock_names is None else clock_names
        if isinstance(value, str) and value in names:
            return GlobalWriteResult(True, value)
        return GlobalWriteResult(False, reason="unknown clock source")
    return None


def pipewire_force_rate(rate: int, device: str | None = None) -> bool:
    """Ask the PipeWire that holds the Apollo's streams to run at `rate`.

    Sets clock.force-rate, or clears it (0) when `rate` is PipeWire's own
    default rate.  It is global: PipeWire's other devices follow too.
    Returns False when PipeWire doesn't hold the Apollo's streams.
    """
    from metering import apollo_stream_owner, find_apollo_card, pipewire_env

    card = find_apollo_card(device)
    owner = apollo_stream_owner(card) if card is not None else None
    if not owner or owner[1] != "pipewire":
        return False
    env = pipewire_env(owner[0])
    if env is None:
        return False
    cmd = ["pw-metadata", "-n", "settings", "0"]
    try:
        out = subprocess.run(cmd + ["clock.rate"], capture_output=True,
                             text=True, timeout=3, env=env).stdout
        m = re.search(r"key:'clock\.rate' value:'(\d+)'", out)
        force = 0 if m and int(m.group(1)) == rate else rate
        r = subprocess.run(cmd + ["clock.force-rate", str(force)],
                           capture_output=True, timeout=3, env=env)
    except (OSError, subprocess.TimeoutExpired):
        return False
    return r.returncode == 0

# ── Mirror Source value encoding (CUE OutputDestination) ─────────
# CUE Mirror Source: param 0x2e (MirrorA) / 0x2f (MirrorB), ch_idx=9.
# The state tree stores string names (OutputDestination on CUE outputs).
# Hardware values verified from MEMORY.md mirror documentation.
MIRROR_SOURCE_MAP = {
    "None": 0xFFFFFFFF, "none": 0xFFFFFFFF,
    "S/PDIF": 6, "SPDIF": 6, "spdif": 6,
    "Line 1-2": 8, "line12": 8,
    "Line 3-4": 10, "line34": 10,
    "ADAT 1-2": 16, "adat12": 16,
    "ADAT 3-4": 18, "adat34": 18,
    "ADAT 5-6": 20, "adat56": 20,
    "ADAT 7-8": 22, "adat78": 22,
}

# Reverse map: hardware int -> display string (for readback decoding)
MIRROR_SOURCE_NAMES = {
    0xFFFFFFFF: "None", 6: "S/PDIF", 8: "Line 1-2", 10: "Line 3-4",
    16: "ADAT 1-2", 18: "ADAT 3-4", 20: "ADAT 5-6", 22: "ADAT 7-8",
}

# ── CUE Mix inverted encoding ────────────────────────────────────
# CUE1 Mix (param 0x05) and CUE2 Mix (param 0x07):
# Hardware value 0 = mix ON (use CUE's own mix), 2 = mix OFF (mirror monitor).
# State tree MixInSource on CUE outputs: "cue" = independent mix, "mon" = follow monitor.
CUE_MIX_ON = 0   # "cue" / "on" / "1" → mix enabled
CUE_MIX_OFF = 2  # "mon" / "off" / "0" → mix disabled (follow monitor)

# ── MixInSource value encoding (DTrace-verified) ──────
# Monitor source: string → int. Tree values are "mon", "cue1", "cue2"
MONITOR_SOURCE_MAP = {"mon": 0, "mix": 0, "0": 0, "cue1": 1, "1": 1, "cue2": 2, "2": 2}
# HP source: string → int. Tree values are "cue1", "cue2"
HP_SOURCE_MAP = {"cue1": 0, "0": 0, "cue2": 1, "1": 1}

# ── DimAttenuation → DimLevel step encoding ──────────────────────
# DimAttenuation (state tree) is 0-60 dB.  Hardware DimLevel (param 0x43)
# is a 1-7 step index: {1:-9, 2:-17, 3:-26, 4:-34, 5:-43, 6:-51, 7:-60 dB}.
# Convert dB attenuation to nearest step.
_DIM_STEPS_DB = [9, 17, 26, 34, 43, 51, 60]  # step 1..7

def dim_attenuation_to_step(db: int) -> int:
    """Convert DimAttenuation (0-60 dB) to DimLevel step (1-7)."""
    db = max(0, min(60, int(db)))
    best_step = 1
    best_dist = abs(db - _DIM_STEPS_DB[0])
    for i, step_db in enumerate(_DIM_STEPS_DB[1:], start=2):
        dist = abs(db - step_db)
        if dist < best_dist:
            best_dist = dist
            best_step = i
    return best_step

# ── TOSLinkOutput string → DigitalOutputMode int ────────────────
TOSLINK_OUTPUT_MAP = {"S/PDIF": 0, "SPDIF": 0, "ADAT": 8}

# ── Bus ID → Mixer Setting Index Mapping ─────────────────────────
# Populated from hw-probe-settings.py Phase D results.
# Key: (bus_id, sub_param) → value: mixer setting index (0-37)
BUS_SETTING_MAP: dict[tuple[int, int], int] = {}
USE_DIRECT_MIXER_WRITES = len(BUS_SETTING_MAP) > 0


def mixer_setting_offset(index: int) -> int:
    """Calculate the BAR0-absolute register offset for a mixer setting.

    From CPcieDeviceMixer::_writeSetting disassembly, there are 3 ranges
    with different base offsets (all relative to the 0x3800 mixer window):
        Settings 0-15:  0xB4 + index * 8
        Settings 16-31: 0xBC + index * 8
        Settings 32-37: 0xC0 + index * 8

    Each setting occupies 8 bytes (word A at offset, word B at offset+4).
    """
    if index < 0 or index >= MIXER_SETTING_COUNT:
        raise ValueError(f"Setting index {index} out of range 0-{MIXER_SETTING_COUNT-1}")
    if index <= 15:
        return MIXER_BAR_OFFSET + 0xB4 + index * 8
    elif index <= 31:
        return MIXER_BAR_OFFSET + 0xBC + index * 8
    else:
        return MIXER_BAR_OFFSET + 0xC0 + index * 8


def encode_mixer_pair(value: int, mask: int = 0xFFFFFFFF) -> tuple[int, int]:
    """Encode a 32-bit value + changed-bits mask into the paired word format.

    From CPcieDeviceMixer::_writeSetting:
        wordA = (mask[15:0]  << 16) | value[15:0]
        wordB = (mask[31:16] << 16) | value[31:16]

    The upper 16 bits of each written word are the changed-bits mask.
    If mask is 0, the DSP ignores the write entirely (no ack).

    Args:
        value: The 32-bit setting value
        mask:  Which bits changed (default 0xFFFFFFFF = all bits)

    Returns:
        (wordA, wordB) ready for register write
    """
    value &= 0xFFFFFFFF
    mask &= 0xFFFFFFFF
    val_lo = value & 0xFFFF
    val_hi = (value >> 16) & 0xFFFF
    mask_lo = mask & 0xFFFF
    mask_hi = (mask >> 16) & 0xFFFF
    word_a = (mask_lo << 16) | val_lo
    word_b = (mask_hi << 16) | val_hi
    return word_a, word_b


def decode_mixer_pair(word_a: int, word_b: int) -> tuple[int, int]:
    """Decode paired register words back to (value, mask).

    Inverse of encode_mixer_pair.
    """
    val_lo = word_a & 0xFFFF
    val_hi = word_b & 0xFFFF
    mask_lo = (word_a >> 16) & 0xFFFF
    mask_hi = (word_b >> 16) & 0xFFFF
    value = (val_hi << 16) | val_lo
    mask = (mask_hi << 16) | mask_lo
    return value, mask


# ── Value encoding helpers ─────────────────────────────────────────

def float_to_fixed16(value: float) -> int:
    """Convert float to Q16.16 fixed-point integer."""
    return int(value * 65536) & 0xFFFFFFFF


def fixed16_to_float(value: int) -> float:
    """Convert Q16.16 fixed-point integer to float."""
    if value & 0x80000000:
        value -= 0x100000000
    return value / 65536.0


def db_to_linear(db: float) -> float:
    """Convert dB to linear scale (0.0 to 1.0)."""
    if db <= -144.0:
        return 0.0
    return 10.0 ** (db / 20.0)


def encode_gain_value(gain_db: float) -> int:
    """Encode preamp gain (dB) to ARM register value.

    From capture analysis: gain values are integer dB, range -144 to 65.
    The ARM MCU expects the raw integer dB value.
    """
    return max(-144, min(65, int(round(gain_db)))) & 0xFFFFFFFF


def encode_monitor_level(db: float) -> int:
    """Encode monitor level dB to SEL131 integer value.

    Formula from IOKit capture: value = 192 + (dB × 2)
    Verified: -10dB→172, -15dB→162, -30dB→132, -50dB→92
    """
    return max(0, min(384, int(round(192 + db * 2))))


_INV_SQRT2 = 0.7071067811865476  # 1/sqrt(2), exact -3.01 dB


def encode_input_fader(db: float) -> float:
    """Encode input fader dB to linear float (with 1/sqrt(2) scaling).

    Formula: linear = 10^(dB/20) / sqrt(2)
    This gives 0 dB fader → 1/sqrt(2) ≈ 0.707107 (-3.01 dB to DSP),
    providing headroom in the summing bus.

    Verified against IOKit captures:
        0dB  → 0.707107 (0x3F3504F3)
       -10dB → 0.223607 (0x3E64F92E)
       -40dB → 0.007071 (0x3BE7B46A)
    """
    if db <= -144.0:
        return 0.0
    return 10.0 ** (db / 20.0) * _INV_SQRT2


def encode_mix_coeff(fader_db: float, pan: float) -> float:
    """Compute the sub=0 mix coefficient combining fader + pan.

    DTrace-verified formula:
        mix_coeff = 10^(fader_dB/20) × cos((pan + 1) × π/4)
    This is encode_input_fader(dB) × √2 × cos((pan+1)×π/4).

    At center pan (0.0): mix_coeff = encode_input_fader(dB) × 1.0
    At hard left (-1.0): mix_coeff = encode_input_fader(dB) × √2
    At hard right (+1.0): mix_coeff ≈ 0.0
    """
    if fader_db <= -144.0:
        return 0.0
    return 10.0 ** (fader_db / 20.0) * math.cos((pan + 1.0) * math.pi / 4.0)


def encode_mix_coeff_r(fader_db: float, pan: float) -> float:
    """Right-output weight of the sub=0 mix coefficient (side 1 entry).

    The Mixer Engine sends each bus param as a left and a right entry:
        left  = 10^(fader_dB/20) × cos((pan + 1) × π/4)   (encode_mix_coeff)
        right = 10^(fader_dB/20) × sin((pan + 1) × π/4)
    At center pan both are 0.707 × the fader; at hard left the right
    weight is 0.
    """
    if fader_db <= -144.0:
        return 0.0
    return 10.0 ** (fader_db / 20.0) * math.sin((pan + 1.0) * math.pi / 4.0)


def encode_aux_fader(db: float) -> float:
    """Encode aux fader dB to linear float (NO offset).

    Formula from IOKit capture: linear = 10^(dB / 20)
    Verified: -10dB→0.316228, -15dB→0.177828, -20dB→0.100000
    """
    if db <= -144.0:
        return 0.0
    return 10.0 ** (db / 20.0)


def taper_to_db(tapered: float, min_db: float = -96.0, max_db: float = 0.0) -> float:
    """Convert tapered 0.0-1.0 to dB using quadratic audio curve.

    DEPRECATED: Use fader_tapered_to_db() for input/send/aux faders,
    or preamp_tapered_to_db() for preamp gain. This old quadratic
    approximation is kept for backwards compatibility only.
    """
    if tapered <= 0.0:
        return -144.0  # silence
    if tapered >= 1.0:
        return max_db
    # Quadratic taper: more resolution near 0 dB
    return min_db + (max_db - min_db) * (tapered ** 2)


def db_to_taper(db: float, min_db: float = -96.0, max_db: float = 0.0) -> float:
    """Convert dB to tapered 0.0-1.0 using quadratic audio curve.

    DEPRECATED: Inverse of taper_to_db.
    """
    if db <= min_db:
        return 0.0
    if db >= max_db:
        return 1.0
    return ((db - min_db) / (max_db - min_db)) ** 0.5


# ── Fader taper lookup table (input/send/aux) ──────────────────────────
# Captured from real UA Mixer Engine on Apollo x4.
# 361 entries: index i → dB for tapered = i/360.
# Swept every 1/360 step via TCP:4710, recording the server's dB response.
# Input, send, and aux faders all use the EXACT SAME curve (-144 to +12 dB).
# fmt: off
_FADER_TAPER_FWD = (
    -144.0, -140.8, -137.6, -134.4, -131.1, -127.9, -124.7, -121.5,
    -118.3, -115.1, -111.8, -108.6, -105.4, -102.2,  -99.0,  -95.8,
     -92.5,  -89.3,  -86.1,  -85.2,  -84.4,  -83.6,  -82.7,  -81.9,
     -81.1,  -80.3,  -79.5,  -78.7,  -77.8,  -77.0,  -76.2,  -75.4,
     -74.6,  -73.7,  -72.9,  -72.1,  -71.3,  -70.5,  -69.6,  -68.8,
     -68.0,  -67.2,  -66.4,  -65.5,  -64.7,  -63.9,  -63.1,  -62.3,
     -61.4,  -60.6,  -59.8,  -59.0,  -58.2,  -57.3,  -56.5,  -55.8,
     -55.4,  -55.0,  -54.6,  -54.1,  -53.7,  -53.3,  -52.8,  -52.4,
     -52.0,  -51.5,  -51.1,  -50.7,  -50.2,  -49.8,  -49.4,  -49.0,
     -48.5,  -48.1,  -47.7,  -47.2,  -46.8,  -46.4,  -45.9,  -45.5,
     -45.1,  -44.7,  -44.2,  -43.8,  -43.4,  -42.9,  -42.5,  -42.1,
     -41.6,  -41.2,  -40.8,  -40.3,  -39.9,  -39.5,  -39.1,  -38.6,
     -38.2,  -37.8,  -37.3,  -36.9,  -36.5,  -36.0,  -35.6,  -35.2,
     -34.7,  -34.3,  -33.9,  -33.5,  -33.0,  -32.6,  -32.2,  -31.9,
     -31.6,  -31.4,  -31.1,  -30.9,  -30.7,  -30.4,  -30.2,  -30.0,
     -29.7,  -29.5,  -29.3,  -29.0,  -28.8,  -28.5,  -28.3,  -28.1,
     -27.8,  -27.6,  -27.4,  -27.1,  -26.9,  -26.7,  -26.4,  -26.2,
     -26.0,  -25.7,  -25.5,  -25.2,  -25.0,  -24.8,  -24.5,  -24.3,
     -24.1,  -23.8,  -23.6,  -23.4,  -23.1,  -22.9,  -22.6,  -22.4,
     -22.2,  -21.9,  -21.7,  -21.5,  -21.2,  -21.0,  -20.8,  -20.5,
     -20.3,  -20.0,  -19.8,  -19.6,  -19.3,  -19.1,  -18.9,  -18.6,
     -18.4,  -18.2,  -17.9,  -17.8,  -17.6,  -17.4,  -17.2,  -17.0,
     -16.9,  -16.7,  -16.5,  -16.3,  -16.1,  -16.0,  -15.8,  -15.6,
     -15.4,  -15.2,  -15.1,  -14.9,  -14.7,  -14.5,  -14.3,  -14.2,
     -14.0,  -13.8,  -13.6,  -13.4,  -13.2,  -13.1,  -12.9,  -12.7,
     -12.5,  -12.3,  -12.2,  -12.0,  -11.8,  -11.7,  -11.5,  -11.4,
     -11.2,  -11.1,  -10.9,  -10.8,  -10.6,  -10.5,  -10.3,  -10.2,
     -10.0,   -9.8,   -9.7,   -9.5,   -9.4,   -9.2,   -9.1,   -8.9,
      -8.8,   -8.6,   -8.5,   -8.3,   -8.2,   -8.0,   -7.9,   -7.7,
      -7.6,   -7.4,   -7.3,   -7.1,   -6.9,   -6.8,   -6.6,   -6.5,
      -6.3,   -6.2,   -6.0,   -5.9,   -5.7,   -5.6,   -5.4,   -5.3,
      -5.1,   -5.0,   -4.8,   -4.7,   -4.5,   -4.3,   -4.2,   -4.0,
      -3.9,   -3.7,   -3.6,   -3.4,   -3.3,   -3.1,   -3.0,   -2.8,
      -2.7,   -2.5,   -2.4,   -2.2,   -2.1,   -1.9,   -1.8,   -1.6,
      -1.4,   -1.3,   -1.1,   -1.0,   -0.8,   -0.7,   -0.5,   -0.4,
      -0.2,   -0.1,    0.1,    0.2,    0.4,    0.5,    0.7,    0.8,
       1.0,    1.2,    1.3,    1.5,    1.6,    1.8,    1.9,    2.1,
       2.2,    2.4,    2.5,    2.7,    2.8,    3.0,    3.1,    3.3,
       3.4,    3.6,    3.8,    3.9,    4.1,    4.2,    4.4,    4.5,
       4.7,    4.8,    5.0,    5.1,    5.3,    5.4,    5.6,    5.7,
       5.9,    6.0,    6.2,    6.3,    6.5,    6.7,    6.8,    7.0,
       7.1,    7.3,    7.4,    7.6,    7.7,    7.9,    8.0,    8.2,
       8.3,    8.5,    8.6,    8.8,    8.9,    9.1,    9.2,    9.4,
       9.6,    9.7,    9.9,   10.0,   10.2,   10.3,   10.5,   10.6,
      10.8,   10.9,   11.1,   11.2,   11.4,   11.5,   11.7,   11.8,
      12.0,
)
# fmt: on


def fader_tapered_to_db(tapered: float) -> float:
    """Convert fader tapered 0.0-1.0 to dB using captured LUT.

    Uses a 361-entry lookup table captured from the real UA Mixer Engine.
    Input, send, and aux faders all use this same curve (-144 to +12 dB,
    6-segment piecewise linear). Interpolates between table entries for
    arbitrary float inputs.
    """
    if tapered <= 0.0:
        return -144.0
    if tapered >= 1.0:
        return 12.0
    pos = tapered * 360.0
    idx = int(pos)
    if idx >= 360:
        return 12.0
    frac = pos - idx
    db_lo = _FADER_TAPER_FWD[idx]
    db_hi = _FADER_TAPER_FWD[idx + 1]
    return db_lo + frac * (db_hi - db_lo)


def fader_db_to_tapered(db: float) -> float:
    """Convert dB to fader tapered 0.0-1.0 using captured LUT.

    Inverse of fader_tapered_to_db. Uses binary search on the
    monotonically increasing LUT.
    """
    if db <= -144.0:
        return 0.0
    if db >= 12.0:
        return 1.0
    # Binary search for the interval containing db
    lo, hi = 0, 360
    while lo < hi - 1:
        mid = (lo + hi) // 2
        if _FADER_TAPER_FWD[mid] <= db:
            lo = mid
        else:
            hi = mid
    # Interpolate within the interval
    db_lo = _FADER_TAPER_FWD[lo]
    db_hi = _FADER_TAPER_FWD[hi] if hi <= 360 else 12.0
    if db_hi == db_lo:
        return lo / 360.0
    frac = (db - db_lo) / (db_hi - db_lo)
    return (lo + frac) / 360.0


def preamp_tapered_to_db(tapered: float) -> float:
    """Convert preamp gain tapered 0.0-1.0 to integer dB.

    Preamp gain is linear: 10 dB to 65 dB (55 dB range), quantized
    to integer dB. Captured from real UA Mixer Engine — confirmed
    linear with max 1 dB rounding error vs round(10 + tapered * 55).
    """
    return float(max(10, min(65, round(10 + max(0.0, min(1.0, tapered)) * 55))))


def preamp_db_to_tapered(db: float) -> float:
    """Convert preamp gain dB (10-65) to tapered 0.0-1.0."""
    return max(0.0, min(1.0, (db - 10) / 55.0))


# ── Monitor taper lookup table ───────────────────────────────────────
# Captured from real UA Mixer Engine on Apollo x4.
# 361 entries: index i → hw value for tapered = i/360.
# Swept every 1/360 step via TCP:4710, recording the server's dB response.
# fmt: off
_MONITOR_TAPER_FWD = (
      0,   0,   0,   0,   1,   1,   1,   1,   1,   1,  42,  42,
     42,  42,  42,  42,  42,  72,  72,  72,  72,  72,  72,  72,
     82,  82,  82,  82,  82,  82,  88,  88,  88,  88,  88,  88,
     88,  94,  94,  94,  94,  94,  94,  94, 100, 100, 100, 100,
    100, 100, 106, 106, 106, 106, 106, 106, 106, 112, 112, 112,
    112, 112, 112, 112, 118, 118, 118, 118, 118, 118, 121, 121,
    121, 121, 121, 121, 121, 126, 126, 126, 126, 126, 126, 126,
    131, 131, 131, 131, 131, 131, 136, 136, 136, 136, 136, 136,
    136, 140, 140, 140, 140, 140, 140, 140, 144, 144, 144, 144,
    144, 144, 148, 148, 148, 148, 148, 148, 148, 152, 152, 152,
    152, 152, 152, 152, 156, 156, 156, 156, 156, 156, 158, 158,
    158, 158, 158, 158, 158, 159, 159, 159, 159, 159, 159, 159,
    160, 160, 160, 160, 160, 160, 161, 161, 161, 161, 161, 161,
    161, 162, 162, 162, 162, 162, 162, 162, 163, 163, 163, 163,
    163, 163, 164, 164, 164, 164, 164, 164, 164, 165, 165, 165,
    165, 165, 165, 165, 166, 166, 166, 166, 166, 166, 167, 167,
    167, 167, 167, 167, 167, 168, 168, 168, 168, 168, 168, 168,
    169, 169, 169, 169, 169, 169, 169, 170, 170, 170, 170, 170,
    170, 171, 171, 171, 171, 171, 171, 171, 172, 172, 172, 172,
    172, 172, 173, 173, 173, 173, 173, 173, 173, 174, 174, 174,
    174, 174, 174, 174, 175, 175, 175, 175, 175, 175, 176, 176,
    176, 176, 176, 176, 176, 177, 177, 177, 177, 177, 177, 177,
    178, 178, 178, 178, 178, 178, 179, 179, 179, 179, 179, 179,
    179, 180, 180, 180, 180, 180, 180, 180, 181, 181, 181, 181,
    181, 181, 182, 182, 182, 182, 182, 182, 182, 183, 183, 183,
    183, 183, 183, 183, 184, 184, 184, 184, 184, 184, 185, 185,
    185, 185, 185, 185, 185, 186, 186, 186, 186, 186, 186, 186,
    187, 187, 187, 187, 187, 187, 188, 188, 188, 188, 188, 188,
    188, 189, 189, 189, 189, 189, 189, 189, 190, 190, 190, 190,
    190, 190, 191, 191, 191, 191, 191, 191, 191, 192, 192, 192,
    192,
)
# Reverse table: hw (0-192) → tapered midpoint.
# For hw values that appear in the sweep, uses the midpoint of matching steps.
# Gaps (hw values skipped by quantization) are linearly interpolated.
_MONITOR_TAPER_REV = (
    0.004167, 0.018056, 0.018496, 0.018936, 0.019377, 0.019817, 0.020257,
    0.020698, 0.021138, 0.021579, 0.022019, 0.022459, 0.022900, 0.023340,
    0.023780, 0.024221, 0.024661, 0.025102, 0.025542, 0.025982, 0.026423,
    0.026863, 0.027304, 0.027744, 0.028184, 0.028625, 0.029065, 0.029505,
    0.029946, 0.030386, 0.030827, 0.031267, 0.031707, 0.032148, 0.032588,
    0.033028, 0.033469, 0.033909, 0.034350, 0.034790, 0.035230, 0.035671,
    0.036111, 0.036759, 0.037407, 0.038056, 0.038704, 0.039352, 0.040000,
    0.040648, 0.041296, 0.041944, 0.042593, 0.043241, 0.043889, 0.044537,
    0.045185, 0.045833, 0.046481, 0.047130, 0.047778, 0.048426, 0.049074,
    0.049722, 0.050370, 0.051019, 0.051667, 0.052315, 0.052963, 0.053611,
    0.054259, 0.054907, 0.055556, 0.057361, 0.059167, 0.060972, 0.062778,
    0.064583, 0.066389, 0.068194, 0.070000, 0.071806, 0.073611, 0.076620,
    0.079630, 0.082639, 0.085648, 0.088657, 0.091667, 0.094907, 0.098148,
    0.101389, 0.104630, 0.107870, 0.111111, 0.114120, 0.117130, 0.120139,
    0.123148, 0.126157, 0.129167, 0.132176, 0.135185, 0.138194, 0.141204,
    0.144213, 0.147222, 0.150463, 0.153704, 0.156944, 0.160185, 0.163426,
    0.166667, 0.169676, 0.172685, 0.175694, 0.178704, 0.181713, 0.184722,
    0.190741, 0.196759, 0.202778, 0.206667, 0.210556, 0.214444, 0.218333,
    0.222222, 0.225833, 0.229444, 0.233056, 0.236667, 0.240278, 0.243889,
    0.247500, 0.251111, 0.254722, 0.258333, 0.263194, 0.268056, 0.272917,
    0.277778, 0.282292, 0.286806, 0.291319, 0.295833, 0.300347, 0.304861,
    0.309375, 0.313889, 0.318750, 0.323611, 0.328472, 0.333333, 0.337847,
    0.342361, 0.346875, 0.351389, 0.360417, 0.369444, 0.388889, 0.406944,
    0.425000, 0.444444, 0.462500, 0.480556, 0.500000, 0.518056, 0.536111,
    0.555556, 0.575000, 0.593056, 0.611111, 0.629167, 0.647222, 0.666667,
    0.684722, 0.702778, 0.722222, 0.740278, 0.758333, 0.777778, 0.795833,
    0.813889, 0.833333, 0.851389, 0.869444, 0.888889, 0.906944, 0.925000,
    0.944444, 0.962500, 0.980556, 0.995833,
)
# fmt: on

# The tables above are the x4's 54-step taper (MixerTypeFreud::
# GetCRPGATaperTodBTable).  The Apollo/8/8p/A16/x6/x8/x8p/x16 classes use a
# 48-step table instead (MixerTypeBase, CRPGATaperTodBTableApolloA16); the
# daemon installs the attached model's LUTs with set_monitor_taper()
# (model_features.taper_luts regenerates these two tables exactly from the
# 54-step dB table).
_monitor_lut = {"fwd": _MONITOR_TAPER_FWD, "rev": _MONITOR_TAPER_REV,
                "steps": 54}


def set_monitor_taper(db_table: list | None) -> int:
    """Use the monitor taper of a model (its devices/*.json monitor_taper
    dB table, steps + 1 values).  None or an unusable table restores the
    x4's 54-step taper.  Returns the step count now in use."""
    if db_table and len(db_table) >= 2:
        fwd, rev = mf.taper_luts(list(db_table))
        _monitor_lut.update(fwd=fwd, rev=rev, steps=len(db_table) - 1)
    else:
        _monitor_lut.update(fwd=_MONITOR_TAPER_FWD, rev=_MONITOR_TAPER_REV,
                            steps=54)
    return _monitor_lut["steps"]


def monitor_taper_steps() -> int:
    """Steps of the monitor taper in use (54 on the x4, 48 on HEXA)."""
    return _monitor_lut["steps"]


def monitor_tapered_to_hw(tapered: float) -> int:
    """Convert monitor tapered 0.0-1.0 to hardware value 0-192.

    Uses the 361-entry lookup table of the model's taper (the x4's was
    captured from the real UA Mixer Engine).  For arbitrary float inputs,
    interpolates between the two nearest table entries (1/360 step
    resolution).
    """
    if tapered <= 0.0:
        return 0
    if tapered >= 1.0:
        return 192
    pos = tapered * 360.0
    idx = int(pos)
    if idx >= 360:
        return 192
    frac = pos - idx
    fwd = _monitor_lut["fwd"]
    hw_lo = fwd[idx]
    hw_hi = fwd[idx + 1]
    return max(0, min(192, round(hw_lo + frac * (hw_hi - hw_lo))))


def monitor_hw_to_tapered(hw_raw: int) -> float:
    """Convert hardware value 0-192 to monitor tapered 0.0-1.0.

    Uses a 193-entry reverse lookup table. Each entry is the midpoint
    tapered value for that hw level.
    """
    if hw_raw <= 0:
        return 0.0
    if hw_raw >= 192:
        return 1.0
    return _monitor_lut["rev"][hw_raw]


def float_to_uint32(f: float) -> int:
    """Pack an IEEE 754 float as a uint32 (for register writes)."""
    return struct.unpack('<I', struct.pack('<f', f))[0]


def uint32_to_float(u: int) -> float:
    """Unpack a uint32 as an IEEE 754 float."""
    return struct.unpack('<f', struct.pack('<I', u & 0xFFFFFFFF))[0]


# ── Hardware Backend ────────────────────────────────────────────────

class HardwareBackend:
    """Low-level register I/O via /dev/ua_apollo* ioctl.

    Provides:
    - Raw register read/write via ioctl
    - Mixer sequence handshake protocol (38 DSP settings)
    - CLI register interface (ARM microcontroller commands)
    - Diagnostic register dumping
    """

    def __init__(self, device_path: str | None = None, safe_mode: bool = False):
        self.fd = -1
        self.device_path = device_path
        self.connected = False
        self.lost = False            # True after mark_lost() until reopened
        self._cli_enabled = False
        self.safe_mode = safe_mode  # Block unmapped DSP writes (bus params, monitor)
        self._last_mixer_write = 0.0  # Throttle: time of last DSP setting write
        self._last_cli_write = 0.0   # Throttle: time of last ARM CLI command
        self._cli_frozen = False     # Set True on first CLI timeout — stops all CLI
        self.last_errno = 0          # errno of the last failed set_mixer_param
        self._features: int | None = None   # GET_VERSION feature bits, cached
        self._toslink_in_bit: bool | None = None   # module parameter, cached
        # True while the driver says the device is paused for system
        # suspend (UA_PAUSE_DEVICE): every write is refused, see
        # HardwareRouter.set_paused
        self.paused = False

    def open(self, device_path: str | None = None) -> bool:
        """Open the device node. Auto-discovers /dev/ua_apollo* if no path given."""
        path = device_path or self.device_path
        if not path:
            path = self._find_device()
        if not path:
            log.warning("No UA Apollo device found")
            return False

        try:
            self.fd = os.open(path, os.O_RDWR)
            self.device_path = path
            self.connected = True
            self.lost = False
            # A new device session starts with a working ARM CLI
            self._cli_enabled = False
            self._cli_frozen = False
            self._features = None
            self._toslink_in_bit = None
            log.info("Opened hardware device: %s", path)
            return True
        except OSError as e:
            log.error("Failed to open %s: %s", path, e)
            return False

    def close(self):
        if self.fd >= 0:
            os.close(self.fd)
            self.fd = -1
            self.connected = False
            self._cli_enabled = False
            self._features = None
            self._toslink_in_bit = None

    def toslink_in_bit_enabled(self) -> bool:
        """The driver's toslink_in_bit module parameter (device-mode word
        bit 0x10 = TOSLink in is ADAT).  Off unless sysfs says Y/1; a
        missing file or any other content reads as off.  Cached until the
        device is reopened."""
        if self._toslink_in_bit is None:
            try:
                with open(TOSLINK_IN_BIT_PARAM) as f:
                    self._toslink_in_bit = f.read().strip() in ("Y", "y", "1")
            except OSError:
                self._toslink_in_bit = False
        return self._toslink_in_bit

    # ── Hot-plug: loss detection and reopen ──────────────────────────

    def probe(self) -> bool:
        """True while the open device node still answers.

        False when the node is gone or was replaced (unplug, module
        reload: the new node is a different inode), or when the driver
        answers ENODEV/EIO because the PCI device was removed under the
        open fd.  An old driver without GET_DEVICE_INFO (ENOTTY) counts
        as alive.
        """
        if self.fd < 0:
            return False
        try:
            st_fd = os.fstat(self.fd)
            st_path = os.stat(self.device_path)
        except (OSError, TypeError):
            return False
        if (st_fd.st_rdev, st_fd.st_ino) != (st_path.st_rdev, st_path.st_ino):
            return False
        try:
            fcntl.ioctl(self.fd, UA_IOCTL_GET_DEVICE_INFO,
                        bytes(_DEVICE_INFO_SIZE))
        except OSError as e:
            return e.errno not in _DEVICE_LOST_ERRNOS
        return True

    def mark_lost(self, reason: str = ""):
        """Drop the dead fd and go offline; reopen() brings it back."""
        if self.fd >= 0:
            try:
                os.close(self.fd)
            except OSError:
                pass
        self.fd = -1
        self.connected = False
        self.lost = True
        self._cli_enabled = False
        self._features = None
        log.warning("Hardware device %s lost%s", self.device_path,
                    f" ({reason})" if reason else "")

    def reopen(self) -> bool:
        """Try to open the device again after it was lost (or never seen).

        Quiet while the node is absent, so a backoff loop does not flood
        the log.  The same path is reused: after a replug udev recreates
        /dev/ua_apolloN with the same name.
        """
        path = self.device_path or self._find_device()
        if not path or not os.path.exists(path):
            return False
        try:
            fd = os.open(path, os.O_RDWR)
        except OSError as e:
            log.debug("reopen %s: %s", path, e)
            return False
        try:
            fcntl.ioctl(fd, UA_IOCTL_GET_DEVICE_INFO, bytes(_DEVICE_INFO_SIZE))
        except OSError as e:
            if e.errno in _DEVICE_LOST_ERRNOS:
                os.close(fd)
                return False
        self.fd = fd
        self.device_path = path
        self.connected = True
        self.lost = False
        self._cli_enabled = False
        self._cli_frozen = False
        self._features = None
        log.info("Reopened hardware device: %s", path)
        return True

    def _find_device(self) -> str | None:
        """Auto-discover /dev/ua_apollo*."""
        matches = sorted(glob.glob("/dev/ua_apollo*"))
        return matches[0] if matches else None

    # ── Raw register access ──────────────────────────────────────────

    def reg_read(self, offset: int) -> int | None:
        """Read a BAR0 register. Returns value or None on error."""
        if self.fd < 0:
            return None
        buf = struct.pack("II", offset, 0)
        try:
            result = fcntl.ioctl(self.fd, UA_IOCTL_READ_REG, buf)
            _, value = struct.unpack("II", result)
            return value
        except OSError as e:
            log.error("reg_read(0x%04x): %s", offset, e)
            return None

    def reg_write(self, offset: int, value: int) -> bool:
        """Write a BAR0 register. Returns True on success."""
        if self.paused:
            return False
        if self.fd < 0:
            return False
        buf = struct.pack("II", offset, value & 0xFFFFFFFF)
        try:
            fcntl.ioctl(self.fd, UA_IOCTL_WRITE_REG, buf)
            return True
        except OSError as e:
            log.error("reg_write(0x%04x, 0x%08x): %s", offset, value, e)
            return False

    # ── Mixer sequence protocol ──────────────────────────────────────

    def mixer_write_setting(self, index: int, value: int,
                            mask: int = 0xFFFFFFFF) -> bool:
        """Write a mixer setting atomically via the kernel ioctl.

        The kernel performs the full sequence handshake atomically under
        its device mutex, preventing races between concurrent writers:
            1. Read MIXER_SEQ_RD
            2. Encode and write paired (value, mask) words
            3. Increment and write MIXER_SEQ_WR
            4. Poll MIXER_SEQ_RD for DSP ack

        Falls back to the userspace register-by-register path if the
        atomic ioctl is not available (e.g. older driver without 0x36).

        Args:
            index: Setting index 0-37
            value: 32-bit setting value
            mask:  Changed-bits mask (default all bits = 0xFFFFFFFF)
        """
        if self.paused:
            return False
        if index < 0 or index >= MIXER_SETTING_COUNT:
            log.error("mixer_write_setting: index %d out of range (0-%d)",
                      index, MIXER_SETTING_COUNT - 1)
            return False

        # Try atomic kernel ioctl first
        if self.fd >= 0:
            buf = struct.pack('<IIII', index, value & 0xFFFFFFFF,
                              mask & 0xFFFFFFFF, 0)
            try:
                fcntl.ioctl(self.fd, UA_IOCTL_WRITE_MIXER_SETTING, buf)
                log.debug("mixer[%d]: atomic write val=0x%08x mask=0x%08x",
                          index, value, mask)
                return True
            except OSError as e:
                if e.errno == 25:  # ENOTTY — ioctl not supported
                    log.debug("WRITE_MIXER_SETTING not supported, "
                              "falling back to register path")
                else:
                    log.error("mixer_write_setting[%d]: ioctl error: %s",
                              index, e)
                    return False

        # Fallback: userspace register-by-register (racy without locking)
        seq_rd = self.reg_read(REG_MIXER_SEQ_RD)
        if seq_rd is None:
            return False

        reg = mixer_setting_offset(index)
        word_a, word_b = encode_mixer_pair(value, mask)
        if not self.reg_write(reg, word_a):
            return False
        if not self.reg_write(reg + 4, word_b):
            return False
        log.debug("mixer[%d] @ 0x%04x: A=0x%08x B=0x%08x (val=0x%08x mask=0x%08x)",
                  index, reg, word_a, word_b, value, mask)

        new_seq = (seq_rd + 1) & 0xFFFFFFFF
        if not self.reg_write(REG_MIXER_SEQ_WR, new_seq):
            return False

        for _ in range(100):
            ack = self.reg_read(REG_MIXER_SEQ_RD)
            if ack is None:
                return False
            if ack == new_seq:
                return True
            time.sleep(0.001)

        log.error("mixer_write_setting[%d]: DSP ack timeout (wrote seq=%d, "
                  "read=%d)", index, new_seq, self.reg_read(REG_MIXER_SEQ_RD) or -1)
        return False

    def mixer_read_setting(self, index: int) -> tuple[int, int] | None:
        """Read a mixer setting, returning (value, mask).

        Uses the atomic kernel ioctl (READ_MIXER_SETTING) when available,
        which reads the paired registers under the device mutex. Falls back
        to direct register reads if the ioctl is not supported.

        Returns (value, mask) tuple or None on error.
        """
        if index < 0 or index >= MIXER_SETTING_COUNT:
            return None

        # Try atomic kernel ioctl first
        if self.fd >= 0:
            buf = struct.pack('<IIII', index, 0, 0, 0)
            try:
                result = fcntl.ioctl(self.fd, UA_IOCTL_READ_MIXER_SETTING, buf)
                _, value, mask, _ = struct.unpack('<IIII', result)
                return value, mask
            except OSError as e:
                if e.errno != 25:  # ENOTTY — not supported
                    log.error("mixer_read_setting[%d]: ioctl error: %s",
                              index, e)
                    return None
                # Fall through to register path

        # Fallback: direct register reads (returns raw wordA, wordB)
        reg = mixer_setting_offset(index)
        a = self.reg_read(reg)
        b = self.reg_read(reg + 4)
        if a is None or b is None:
            return None
        return decode_mixer_pair(a, b)

    # ── CLI register interface (ARM MCU communication) ───────────────

    def cli_enable(self) -> bool:
        """Enable the CLI interface for ARM MCU communication.

        Must be called before cli_send_command(). The CLI interface is
        a register-mapped command/response buffer at BAR0+0xC3F4.
        """
        if self._cli_enabled:
            return True
        if not self.reg_write(REG_CLI_ENABLE, 1):
            return False
        # Brief delay for CLI to initialize
        time.sleep(0.01)
        status = self.reg_read(REG_CLI_STATUS)
        if status is None:
            log.error("CLI enable: failed to read status")
            return False
        self._cli_enabled = True
        log.debug("CLI enabled (status=0x%08x)", status)
        return True

    def cli_send_command(self, cmd_data: bytes, timeout_ms: int = 100) -> bytes | None:
        """Send a command via CLI registers and return response data.

        Protocol (reverse-engineered from kext EnableCLI/SendCLICommand):
            1. Ensure CLI is enabled
            2. Write command data to CLI_CMD_BUF (0xC400), 4 bytes at a time
            3. Write command length to CLI_STATUS (0xC3F8) to trigger
            4. Poll CLI_STATUS for completion (bit 31 or length change)
            5. Read response length from CLI_RESP_LEN (0xC3FC)
            6. Read response data from CLI_RESP_BUF (0xC480)

        Args:
            cmd_data: Command bytes (max 128 bytes)
            timeout_ms: Timeout for response in milliseconds

        Returns:
            Response bytes, or None on timeout/error
        """
        if not self._cli_enabled:
            if not self.cli_enable():
                return None

        if len(cmd_data) > CLI_CMD_BUF_SIZE:
            log.error("CLI command too large: %d bytes (max %d)",
                      len(cmd_data), CLI_CMD_BUF_SIZE)
            return None

        # Pad to 4-byte boundary
        remainder = len(cmd_data) % 4
        padded = cmd_data + b'\x00' * (4 - remainder) if remainder else cmd_data

        # Write command data to CLI_CMD_BUF
        for i in range(0, len(padded), 4):
            word = struct.unpack_from('<I', padded, i)[0]
            if not self.reg_write(REG_CLI_CMD_BUF + i, word):
                log.error("CLI: failed to write cmd buf at offset %d", i)
                return None

        # Trigger command by writing length to CLI_STATUS
        if not self.reg_write(REG_CLI_STATUS, len(cmd_data)):
            return None

        log.debug("CLI: sent %d bytes, waiting for response...", len(cmd_data))

        # Poll for response
        polls = timeout_ms
        for _ in range(polls):
            status = self.reg_read(REG_CLI_STATUS)
            if status is None:
                return None
            # Check for completion — status should change from command length
            # to 0 or a response indicator
            if status == 0 or (status & 0x80000000):
                break
            time.sleep(0.001)
        else:
            log.error("CLI: timeout waiting for response (status=0x%08x)", status or 0)
            return None

        # Read response length
        resp_len = self.reg_read(REG_CLI_RESP_LEN)
        if resp_len is None or resp_len == 0:
            # No response data (command may have been void)
            return b''

        resp_len = min(resp_len, CLI_RESP_BUF_SIZE)

        # Read response data
        resp = bytearray()
        for i in range(0, resp_len + (4 - resp_len % 4) if resp_len % 4 else resp_len, 4):
            word = self.reg_read(REG_CLI_RESP_BUF + i)
            if word is None:
                break
            resp.extend(struct.pack('<I', word))

        result = bytes(resp[:resp_len])
        log.debug("CLI: received %d bytes response", len(result))
        return result

    # ── ARM parameter access ─────────────────────────────────────────

    def arm_set_param(self, hw_id: int, param_id: int, value: int) -> bool:
        """Set an ARM microcontroller parameter (preamp, 48V, pad, etc.).

        Uses the CLI register interface to send commands to the ARM MCU.
        The command format is derived from the kext's DeviceRequest protocol.

        From IoKit capture analysis, the SetInputParam struct is 36 bytes:
            word[0]:   paramID (0x00-0x17)
            word[1-4]: reserved (0)
            word[5]:   hwID (channel identifier)
            word[6-7]: reserved (0)
            word[8]:   0x08 (data size / flags)

        For the CLI path, we encode this as a register write sequence:
        the ARM MCU reads paramID, hwID, and value from known locations.

        Args:
            hw_id:    Channel hardware ID (0x0000, 0x0800, 0x1000, 0x1800)
            param_id: Parameter ID (see ARM_PARAM_IDS)
            value:    Parameter value (integer)

        Returns:
            True on success
        """
        if self.paused:
            return False
        # Build the 36-byte CLI command matching the kernel driver's
        # ua_preamp_set_param(): {param_id, value, 0, 0, 0, hw_id, 0, 0, 8}
        cli_payload = struct.pack('<9I',
            param_id,          # word[0]: paramID
            value,             # word[1]: value
            0, 0, 0,           # word[2-4]: reserved
            hw_id,             # word[5]: hardware channel ID
            0, 0,              # word[6-7]: reserved
            8,                 # word[8]: data size
        )

        if self._cli_frozen:
            log.debug("ARM CLI frozen — skipping hwID=0x%04x paramID=0x%02x",
                      hw_id, param_id)
            return False

        log.info("HW ARM: hwID=0x%04x paramID=0x%02x value=%d", hw_id, param_id, value)

        # Throttle: ARM MCU needs 500ms+ between commands.
        # Without this, rapid CLI writes freeze the Apollo permanently.
        now = time.monotonic()
        elapsed = now - self._last_cli_write
        if elapsed < 0.500:
            time.sleep(0.500 - elapsed)

        # Use UA_IOCTL_CLI_COMMAND to go through the kernel's CLI
        # handshake (mutex + enable + send + poll + read response).
        # struct: {cmd_len(u32), resp_len(u32), cmd_data[128], resp_data[128]}
        buf = bytearray(264)
        struct.pack_into('<I', buf, 0, len(cli_payload))  # cmd_len
        # resp_len at offset 4 is output, leave as 0
        buf[8:8+len(cli_payload)] = cli_payload  # cmd_data at offset 8
        try:
            result = fcntl.ioctl(self.fd, UA_IOCTL_CLI_COMMAND, bytes(buf))
            self._last_cli_write = time.monotonic()
            resp_len = struct.unpack_from('<I', result, 4)[0]
            if resp_len > 0:
                resp_data = result[136:136+min(resp_len, 128)]
                resp_code = struct.unpack_from('<I', resp_data, 0)[0] if resp_len >= 4 else 0
                log.debug("ARM param OK (resp_len=%d code=0x%04x)", resp_len, resp_code)
            return True
        except OSError as e:
            self._last_cli_write = time.monotonic()
            if e.errno == 110:  # ETIMEDOUT — ARM MCU is frozen
                self._cli_frozen = True
                log.error("ARM CLI FROZEN — stopping all CLI commands. "
                          "Apollo needs power cycle. "
                          "(hwID=0x%04x paramID=0x%02x value=%d)",
                          hw_id, param_id, value)
            else:
                log.error("ARM set param failed: %s "
                          "(hwID=0x%04x paramID=0x%02x value=%d)",
                          e, hw_id, param_id, value)
            return False

    def arm_get_param(self, hw_id: int, param_id: int) -> int | None:
        """Read an ARM parameter value.

        Sends a query command and returns the current value, or None on error.
        """
        # Query struct: same layout but value=0 indicates read
        cmd = struct.pack('<9I',
            param_id, 0, 0, 0, 0, hw_id, 0, 0, 8)

        resp = self.cli_send_command(cmd)
        if resp is None or len(resp) < 8:
            return None

        # Response should contain the current value
        value = struct.unpack_from('<I', resp, 4)[0]
        return value

    # ── SetMixerParam (SEL131 equivalent) ────────────────────────────

    def set_mixer_param(self, channel_type: int, channel_idx: int,
                        param_id: int, value: int) -> bool:
        """Set a mixer parameter — equivalent to macOS IOKit SEL131.

        On macOS, the kext routes this based on channel_type:
            channel_type=1 (preamp) → DSP mixer settings
            channel_type=2 (monitor) → DSP mixer settings

        Both paths write through DSP mixer settings (38 settings at
        BAR0+0x3800) using the sequence-counter handshake protocol.
        The kernel driver handles the routing and encoding.

        Struct: {channel_type, channel_idx, param_id, value} = 16 bytes.

        Args:
            channel_type: 1=input/preamp, 2=output/monitor
            channel_idx:  Channel index within type (0-3 for preamp)
            param_id:     Parameter ID (preamp or monitor param)
            value:        Integer parameter value
        """
        if self.paused:
            log.debug("MIXER_PARAM refused (device paused): ch_type=%d "
                      "ch_idx=%d param=0x%02x", channel_type, channel_idx,
                      param_id)
            return False

        # Safe mode: allow preamp (1) and monitor (2) — kernel handles routing
        if self.safe_mode and channel_type not in (PREAMP_CHANNEL_TYPE, MONITOR_CHANNEL_TYPE):
            log.debug("MIXER_PARAM BLOCKED (safe_mode): ch_type=%d ch_idx=%d "
                      "param=0x%02x value=%d", channel_type, channel_idx,
                      param_id, value)
            return True  # Pretend success so state tree still updates

        if self.fd < 0:
            log.info("HW MIXER_PARAM: ch_type=%d ch_idx=%d param=0x%02x "
                     "value=%d (no device)", channel_type, channel_idx,
                     param_id, value)
            return False

        # Throttle: ensure minimum 15ms between DSP setting writes.
        # The DSP needs time to process each write (especially preamp
        # settings that trigger ARM MCU operations).  Without this,
        # rapid-fire writes from iPad knob movements stall the DSP.
        now = time.monotonic()
        elapsed = now - self._last_mixer_write
        if elapsed < 0.015:
            time.sleep(0.015 - elapsed)

        buf = struct.pack('<IIII', channel_type, channel_idx,
                          param_id, value & 0xFFFFFFFF)
        self.last_errno = 0
        try:
            fcntl.ioctl(self.fd, UA_IOCTL_SET_MIXER_PARAM, buf)
            self._last_mixer_write = time.monotonic()
            log.debug("MIXER_PARAM: ch_type=%d ch_idx=%d param=0x%02x "
                      "value=%d", channel_type, channel_idx, param_id, value)
            return True
        except OSError as e:
            self._last_mixer_write = time.monotonic()
            self.last_errno = e.errno
            log.error("set_mixer_param(type=%d, idx=%d, param=0x%02x, "
                      "val=%d): %s", channel_type, channel_idx,
                      param_id, value, e)
            return False

    # ── SetMixerBusParam (SEL130 equivalent) ──────────────────────────

    def set_mixer_bus_param(self, bus_id: int, sub_param: int,
                            value_float: float, side: int = 0,
                            solo_mute: bool = False) -> bool:
        """Set a mixer bus parameter — equivalent to macOS IOKit SEL130.

        Sends fader level, pan, and send gain values. Values are IEEE 754
        float (linear gain), packed as uint32 for the ioctl.

        A fader or pan change writes sub_param 0 only; sub_param 1-6 are
        the CUE 1/2, AUX 1/2 and CUE 3/4 send levels (see SUB_PARAM_*).

        Each (bus, sub_param) has a left (side 0) and a right (side 1)
        entry; the Mixer Engine writes both.  One call writes one entry.

        Struct: {bus_id, sub_param, value_u32, flags} = 16 bytes; flags is
        0x02, plus UA_BUS_PARAM_F_SIDE_R for the right entry.

        Args:
            bus_id:      Bus identifier (see INPUT_BUS_IDS, aux_bus_id())
            sub_param:   0=main mix, 1/2=CUE 1/2, 3/4=AUX 1/2, 5/6=CUE 3/4
            value_float: Linear gain as float (0.0 = silence, 1.0 = unity)
            side:        0 = left output weight, 1 = right output weight
            solo_mute:   the input is muted by a solo: send the normal
                         coefficient with UA_BUS_PARAM_F_SOLO_MUTE and the
                         driver applies the kext's negated pair rule.  A
                         driver without UA_FEAT_BUS_SOLO_FLAG gets 0.0
                         instead (the flag would be ignored and the live
                         coefficient would stay audible).

        Feature gating (GET_VERSION): a side-1 entry is only sent when the
        driver reports UA_FEAT_BUS_PARAM_SIDE.  An older driver ignores the
        flag and would write the right weight into the LEFT entry, which
        corrupts pan; the entry is dropped (and counted as written).
        """
        if getattr(self, "paused", False):
            return False
        value_u32 = float_to_uint32(value_float)

        # All 32 buses (0x00-0x1f) use direct mapping: setting_index = bus_id.
        # Confirmed via hw-probe-bus-settings.py on Apollo x4.
        if self.safe_mode and bus_id > 0x1f:
            log.debug("BUS_PARAM BLOCKED (safe_mode): bus=0x%04x sub=%d "
                      "value=%.6f", bus_id, sub_param, value_float)
            return True  # Pretend success so state tree still updates

        if self.fd < 0:
            log.info("HW BUS_PARAM: bus=0x%04x sub=%d value=%.6f "
                     "(0x%08x) (no device)", bus_id, sub_param,
                     value_float, value_u32)
            return False

        if side and not self.has_feature(UA_FEAT_BUS_PARAM_SIDE):
            log.debug("BUS_PARAM side-1 dropped (driver has no "
                      "UA_FEAT_BUS_PARAM_SIDE): bus=0x%04x sub=%d "
                      "value=%.6f", bus_id, sub_param, value_float)
            return True
        flags = UA_BUS_PARAM_FLAGS | (UA_BUS_PARAM_F_SIDE_R if side else 0)
        if solo_mute:
            if self.has_feature(UA_FEAT_BUS_SOLO_FLAG):
                flags |= UA_BUS_PARAM_F_SOLO_MUTE
            else:
                log.debug("BUS_PARAM solo-mute flag not supported: bus=0x%04x "
                          "sub=%d side=%d written as 0.0", bus_id, sub_param,
                          side)
                value_u32 = float_to_uint32(0.0)
        buf = struct.pack('<IIII', bus_id, sub_param, value_u32, flags)
        try:
            fcntl.ioctl(self.fd, UA_IOCTL_SET_MIXER_BUS_PARAM, buf)
            log.debug("BUS_PARAM: bus=0x%04x sub=%d side=%d value=%.6f "
                      "(0x%08x) flags=0x%x", bus_id, sub_param, side,
                      value_float, value_u32, flags)
            return True
        except OSError as e:
            log.error("set_mixer_bus_param(bus=0x%04x, sub=%d, side=%d, "
                      "val=%.6f): %s", bus_id, sub_param, side, value_float, e)
            return False

    def set_bus_fader(self, bus_id: int, linear_gain: float,
                     mix_coeff: float = 0.0,
                     mix_coeff_r: float | None = None,
                     solo_mute: bool = False) -> bool:
        """Send a fader change: sub 0 (fader x pan mix coefficient), L and R.

        The Mixer Engine moves a fader through sub 0 only.  Sub 3 and 4 are
        the AUX 1 / AUX 2 send levels (parity/04-engine-surface.md §0.1),
        so a fader must never write them: an earlier version copied the
        fader value into both, overwriting the AUX sends.

        mix_coeff = fader_linear × √2 × cos((pan + 1) × π/4)
        At center pan, mix_coeff == linear_gain (the √2 and cos(π/4) cancel).
        `linear_gain` is accepted for call compatibility and not written.

        mix_coeff_r, when given, is the right-output weight, written as the
        side-1 entry of sub 0 (fader_linear × √2 × sin((pan + 1) × π/4));
        without it only the left entry is written, as before.

        solo_mute marks both entries as muted by a solo (see
        set_mixer_bus_param); the coefficients stay the live ones.
        """
        write_fn = self.set_bus_param_direct if USE_DIRECT_MIXER_WRITES \
            else self.set_mixer_bus_param
        kw = {"solo_mute": True} if solo_mute else {}
        ok = write_fn(bus_id, SUB_PARAM_MIX, mix_coeff, **kw)
        if mix_coeff_r is not None:
            ok = write_fn(bus_id, SUB_PARAM_MIX, mix_coeff_r, side=1,
                          **kw) and ok
        return ok

    def set_bus_pan(self, bus_id: int, mix_coeff: float,
                    mix_coeff_r: float | None = None) -> bool:
        """Send a pan change (sub=0, left entry and, if given, right entry).

        From DTrace capture: pan writes only sub=0.
        mix_coeff = fader_linear × √2 × cos((pan + 1) × π/4)
        mix_coeff_r = fader_linear × √2 × sin((pan + 1) × π/4)
        """
        write_fn = self.set_bus_param_direct if USE_DIRECT_MIXER_WRITES \
            else self.set_mixer_bus_param
        ok = write_fn(bus_id, SUB_PARAM_MIX, mix_coeff)
        if mix_coeff_r is not None:
            ok = write_fn(bus_id, SUB_PARAM_MIX, mix_coeff_r, side=1) and ok
        return ok

    # ── Diagnostic / Probing ──────────────────────────────────────────

    def dump_mixer_settings(self) -> list[dict]:
        """Read and return all 38 mixer settings for diagnostics."""
        results = []
        for i in range(MIXER_SETTING_COUNT):
            pair = self.mixer_read_setting(i)
            if pair is None:
                results.append({"index": i, "error": True})
                continue
            value, mask = pair
            results.append({
                "index": i,
                "reg": mixer_setting_offset(i),
                "value": value,
                "mask": mask,
            })
        return results

    def dump_cli_registers(self) -> dict:
        """Read CLI register state for diagnostics."""
        return {
            "enable": self.reg_read(REG_CLI_ENABLE),
            "status": self.reg_read(REG_CLI_STATUS),
            "resp_len": self.reg_read(REG_CLI_RESP_LEN),
        }

    def dump_mixer_seq(self) -> dict:
        """Read mixer sequence counters."""
        return {
            "seq_wr": self.reg_read(REG_MIXER_SEQ_WR),
            "seq_rd": self.reg_read(REG_MIXER_SEQ_RD),
        }

    def get_mixer_readback(self) -> list[int] | None:
        """Read all 38 mixer settings atomically via GET_MIXER_READBACK ioctl.

        The kernel reads all 38 settings under the device mutex and computes
        XOR checksums (data[38] = XOR of [0..37], data[39] = ~data[38]).

        Returns list of 38 u32 values, or None on error.
        """
        if self.fd < 0:
            return None

        # Pack: 4 input words (zeroed) + 41 data words (zeroed)
        buf = struct.pack('<' + 'I' * 45, *([0] * 45))
        try:
            result = fcntl.ioctl(self.fd, UA_IOCTL_GET_MIXER_READBACK, buf)
            words = struct.unpack('<' + 'I' * 45, result)
            data = list(words[4:])  # data[0..40]

            # Verify checksum
            checksum = 0
            for i in range(38):
                checksum ^= data[i]
            if data[38] != checksum:
                log.warning("mixer readback checksum mismatch: "
                            "computed=0x%08x, got=0x%08x", checksum, data[38])

            return data[:38]  # Return just the 38 settings
        except OSError as e:
            log.error("get_mixer_readback: %s", e)
            return None

    def get_hw_readback(self) -> tuple[int, list[int]] | None:
        """Read 40-word hardware readback from DSP (BAR0+0x3810/0x3814).

        The kext reads these at ~33Hz for metering and status.
        Returns (status, [40 words]) or None on error.
        status=1 means data is valid; the driver re-arms automatically.
        """
        if self.fd < 0:
            return None

        buf = bytearray(164)  # 4 + 40*4
        try:
            fcntl.ioctl(self.fd, UA_IOCTL_GET_HW_READBACK, buf)
            status = struct.unpack_from('<I', buf, 0)[0]
            data = list(struct.unpack_from('<40I', buf, 4))
            return (status, data)
        except OSError as e:
            if e.errno in _DEVICE_LOST_ERRNOS:
                # Polled at 50 Hz: go offline at once instead of logging
                # an error per poll until the daemon's watchdog notices
                self.mark_lost(f"get_hw_readback: {e}")
                return None
            log.error("get_hw_readback: %s", e)
            return None

    def get_device_info(self) -> dict | None:
        """Query which Apollo model is attached via UA_IOCTL_GET_DEVICE_INFO.

        Returns a dict with 'fpga_rev', 'device_type', 'subsystem_id',
        'num_dsps', 'fw_version', 'fw_v2', 'serial', 'ext_caps', or None if the
        device node isn't open or the driver is too old to support the ioctl.

        The daemon uses 'device_type' to pick the right device map
        (e.g. 0x1F -> Apollo x4, 0x22 -> Apollo x8p).
        """
        if self.fd < 0:
            return None

        # struct ua_device_info: 8 u32 fields + reserved[8] = 64 bytes.
        # The buffer must be immutable: given a bytearray, fcntl.ioctl() writes
        # the reply in place and returns the ioctl's int retcode instead of the
        # bytes we unpack below (same pattern as get_dsp_info()).
        buf = bytes(_DEVICE_INFO_SIZE)
        try:
            result = fcntl.ioctl(self.fd, UA_IOCTL_GET_DEVICE_INFO, buf)
        except OSError as e:
            # Older drivers (pre-GET_DEVICE_INFO) return ENOTTY — not fatal.
            log.debug("get_device_info: %s", e)
            return None

        (fpga_rev, device_type, subsystem_id, num_dsps,
         fw_version, fw_v2, serial, ext_caps) = struct.unpack_from('<8I', result)
        return {
            "fpga_rev": fpga_rev,
            "device_type": device_type,
            "subsystem_id": subsystem_id,
            "num_dsps": num_dsps,
            "fw_version": fw_version,
            "fw_v2": bool(fw_v2),
            "serial": serial,
            "ext_caps": ext_caps,
        }

    def get_dsp_info(self, dsp_index: int) -> dict | None:
        """Query DSP info for a single SHARC DSP.

        Returns dict with 'index', 'bank_base', 'status' (0=idle, 1=running),
        or None on error. The kext's DSP and session load figures come
        from selectors 110 and 112 (SEL170 is GetMixerParam).

        The Apollo x4 has 4 DSPs (index 0-3).
        """
        if self.fd < 0:
            return None

        # struct ua_dsp_info { index, bank_base, status, alloc_load,
        #   measured_load, pgm_load, mem_load, flags } = 32 bytes; the load
        # words (1/100 %) are filled with UA_FEAT_DSP_LOAD (ABI 1.6)
        buf = struct.pack('<III20x', dsp_index, 0, 0)
        try:
            result = fcntl.ioctl(self.fd, UA_IOCTL_GET_DSP_INFO, buf)
            vals = struct.unpack_from('<8I', result)
        except OSError as e:
            log.debug("get_dsp_info(dsp=%d): %s", dsp_index, e)
            return None
        info = dict(zip(("index", "bank_base", "status"), vals[:3]))
        if self.has_feature(UA_FEAT_DSP_LOAD) and vals[7] & UA_DSP_INFO_F_LOADS:
            info.update(zip(("alloc_load", "measured_load", "pgm_load",
                             "mem_load"), vals[3:7]))
        return info

    def get_dsp_loads(self) -> tuple[float, float, float] | None:
        """(TotalDSPLoad, TotalPGMLoad, TotalMEMLoad) in percent, as UA's
        Mixer Engine computes them (CUAD2Info::GetAllocatedLoads, arm64
        0x1002b490c): the average over the DSPs of the allocated load, of
        pool 3 use and of the busiest of pools 0-2.  None without
        UA_FEAT_DSP_LOAD or a device.  With no plugins the driver's
        allocated load is the kext's rate overhead (x4: 1.19 % at 48 kHz)
        and the pools are unused."""
        if self.fd < 0 or not self.has_feature(UA_FEAT_DSP_LOAD):
            return None
        rows = []
        for i in range(8):          # the driver refuses an index past the last DSP
            info = self.get_dsp_info(i)
            if info is None:
                break
            if "alloc_load" in info:
                rows.append(info)
        if not rows:
            return None
        n = float(len(rows))
        return tuple(sum(r[k] for r in rows) / 100.0 / n
                     for k in ("alloc_load", "pgm_load", "mem_load"))

    def get_all_dsp_status(self) -> list[dict]:
        """Query all 4 DSPs and return their status."""
        results = []
        for i in range(4):
            info = self.get_dsp_info(i)
            if info:
                results.append(info)
        return results

    def probe_register(self, offset: int) -> int | None:
        """Read a single register (for empirical probing)."""
        return self.reg_read(offset)

    def get_version(self) -> dict | None:
        """Driver ABI version via UA_IOCTL_GET_VERSION.

        Returns {abi_major, abi_minor, driver_version, features,
        driver_name}, or None when the driver predates the ioctl (ENOTTY),
        there is no device, or the call fails.  Informational only.
        """
        if self.fd < 0:
            return None
        try:
            raw = fcntl.ioctl(self.fd, UA_IOCTL_GET_VERSION,
                              bytes(_VERSION_SIZE))
        except OSError as e:
            if e.errno != errno.ENOTTY:
                log.debug("get_version: %s", e)
            return None
        major, minor, drv, feats, name = struct.unpack_from('<IIII16s', raw)
        return {
            "abi_major": major,
            "abi_minor": minor,
            "driver_version": drv,
            "features": feats,
            "driver_name": name.split(b'\0', 1)[0].decode('ascii', 'replace'),
        }

    def features(self) -> int:
        """UA_FEAT_* bits of the open driver (0 for a driver without
        GET_VERSION).  Read once per device session; 0 and uncached while
        there is no device, so it is asked again after the open."""
        if self.fd < 0:
            return 0
        if self._features is None:
            ver = self.get_version()
            self._features = int(ver["features"]) if ver else 0
            log.info("Driver features: 0x%x%s", self._features,
                     "" if ver else " (no GET_VERSION: pre-ABI driver)")
        return self._features

    def has_feature(self, bit: int) -> bool:
        return bool(self.features() & bit)

    def get_system_info(self) -> dict | None:
        """UA_IOCTL_GET_SYSTEM_INFO as a dict (every field of
        struct ua_system_info, `serial` as the real 16-character string).

        None without UA_FEAT_SYSTEM_INFO, without a device, or when the
        call fails.  Unlike get_device_info()['serial'] (BAR0 0x2238, 0 on
        the x4) the serial here is the one printed on the unit.
        """
        if self.fd < 0 or not self.has_feature(UA_FEAT_SYSTEM_INFO):
            return None
        try:
            raw = fcntl.ioctl(self.fd, UA_IOCTL_GET_SYSTEM_INFO,
                              bytes(_SYSTEM_INFO_SIZE))
        except OSError as e:
            log.debug("get_system_info: %s", e)
            return None
        if len(raw) < _SYSTEM_INFO_SIZE:
            return None
        head = struct.unpack_from('<12I', raw, 0)
        tail = struct.unpack_from('<13I', raw, 64)
        info = dict(zip(_SYSTEM_INFO_HEAD, head))
        info.update(zip(_SYSTEM_INFO_TAIL, tail))
        if self.has_feature(UA_FEAT_SYSINFO_EXT):
            info.update(zip(_SYSTEM_INFO_EXT, struct.unpack_from(
                '<4I', raw, _SYSTEM_INFO_EXT_OFF)))
        if self.has_feature(UA_FEAT_FW_RELEASE):
            info.update(zip(_SYSTEM_INFO_FW, struct.unpack_from(
                '<2I', raw, _SYSTEM_INFO_FW_OFF)))
        # NUL-padded after the first NUL, unterminated when all 16 are used
        info["serial"] = raw[48:64].split(b'\0', 1)[0].decode(
            'ascii', 'replace').strip()
        return info

    def get_clock_sources(self) -> list | None:
        """The device's clock-source menu (UA_IOCTL_GET_CLOCK_SOURCES) as
        [(value, name, named_by_device), ...], Internal first.  None
        without UA_FEAT_CLOCK_SOURCES, without a device, before the first
        connect (empty list from the driver) or when the call fails."""
        if self.fd < 0 or not self.has_feature(UA_FEAT_CLOCK_SOURCES):
            return None
        try:
            raw = fcntl.ioctl(self.fd, UA_IOCTL_GET_CLOCK_SOURCES,
                              bytes(_CLOCK_SOURCES_SIZE))
        except OSError as e:
            log.debug("get_clock_sources: %s", e)
            return None
        _size, count, _mask, _res = struct.unpack_from('<4I', raw, 0)
        out = []
        for i in range(min(count, 16)):
            value, flags, name = struct.unpack_from('<II32s', raw, 16 + 40 * i)
            text = name.split(b'\0', 1)[0].decode('ascii', 'replace').strip()
            if text:
                out.append((value, text,
                            bool(flags & UA_CLOCK_SRC_F_DEVICE_NAME)))
        return out or None

    # ── Driver parameter access ──────────────────────────────────────

    def set_driver_param(self, param_id: int, value: int) -> bool:
        """Set a driver parameter via UA_IOCTL_SET_DRIVER_PARAM.

        Struct: {param_id: u32, reserved: u32, value: u64} = 16 bytes.
        Used for sample rate (param 0) and clock source (param 1).
        """
        if self.paused:
            return False
        if self.fd < 0:
            log.info("HW DRV: set_driver_param(param=%d, val=%d) (no device)",
                     param_id, value)
            return False

        buf = struct.pack('<IIQ', param_id, 0, value)
        try:
            fcntl.ioctl(self.fd, UA_IOCTL_SET_DRIVER_PARAM, buf)
            log.debug("DRV_PARAM: set param=%d value=%d", param_id, value)
            return True
        except OSError as e:
            log.error("set_driver_param(param=%d, val=%d): %s",
                      param_id, value, e)
            return False

    def get_driver_param(self, param_id: int) -> int | None:
        """Get a driver parameter via UA_IOCTL_GET_DRIVER_PARAM.

        Returns the parameter value, or None on error.
        """
        if self.fd < 0:
            log.info("HW DRV: get_driver_param(param=%d) (no device)",
                     param_id)
            return None

        buf = struct.pack('<IIQ', param_id, 0, 0)
        try:
            result = fcntl.ioctl(self.fd, UA_IOCTL_GET_DRIVER_PARAM, buf)
            _, _, value = struct.unpack('<IIQ', result)
            return int(value)
        except OSError as e:
            log.error("get_driver_param(param=%d): %s", param_id, e)
            return None

    # ── Direct mixer setting writes for bus params ────────────────────

    def set_bus_param_direct(self, bus_id: int, sub_param: int,
                             value_float: float, side: int = 0,
                             solo_mute: bool = False) -> bool:
        """Write a bus param directly to the correct mixer setting.

        Looks up (bus_id, sub_param) in BUS_SETTING_MAP. If found,
        writes to that mixer setting index via mixer_write_setting(),
        bypassing the driver's SET_MIXER_BUS_PARAM ioctl. Falls back
        to set_mixer_bus_param() if the mapping is unknown.
        """
        key = (bus_id, sub_param)
        setting_idx = BUS_SETTING_MAP.get(key) \
            if not (side or solo_mute) else None

        if setting_idx is None:
            log.debug("BUS_PARAM_DIRECT: no mapping for bus=0x%04x sub=%d "
                      "side=%d, falling back to ioctl", bus_id, sub_param,
                      side)
            return self.set_mixer_bus_param(bus_id, sub_param, value_float,
                                            side, solo_mute=solo_mute)

        value_u32 = float_to_uint32(value_float)

        if self.safe_mode:
            log.debug("BUS_PARAM_DIRECT BLOCKED (safe_mode): bus=0x%04x "
                      "sub=%d → setting[%d] value=%.6f",
                      bus_id, sub_param, setting_idx, value_float)
            return True

        log.debug("BUS_PARAM_DIRECT: bus=0x%04x sub=%d → setting[%d] "
                  "value=%.6f (0x%08x)",
                  bus_id, sub_param, setting_idx, value_float, value_u32)
        return self.mixer_write_setting(setting_idx, value_u32)


# ── Control-to-Hardware Routing ─────────────────────────────────────
#
# Maps TCP:4710 control paths to hardware write actions.
#
# Hardware paths from kext disassembly (UAD2System.kext):
#
#   1. MIXER SETTINGS (38 registers at BAR0+0x3800+offset)
#      - DSP-processed: fader levels, mute, solo, pan, bus sends
#      - Written via mixer sequence handshake protocol
#      - Setting index ↔ control mapping must be determined empirically
#
#   2. ARM PARAMETERS (via CLI registers → ARM MCU)
#      - ARM microcontroller: preamp gain, 48V, pad, HiZ, lowcut, phase
#      - Verified hwIDs: 0x0000, 0x0800, 0x1000, 0x1800 (4 channels)
#      - Verified paramIDs: 0x00-0x0B, 0x17 (from IoKit capture)
#
#   3. BUS PARAMETERS (via SetMixerBusParam, 24-byte struct)
#      - Monitor level, bus enable/mute, headphone level
#      - struct: {hwID, busID, paramID, value, flags, reserved}
#
#   4. DRIVER PARAMETERS (via SetDriverParameter, 16-byte struct)
#      - Sample rate, clock source
#      - struct: {paramID, reserved, value_lo, value_hi}

# ── Path patterns for routing ──────────────────────────────────────
# Compiled regexes to match TCP:4710 control paths and extract indices

# Input channel controls: /devices/0/inputs/<ch>/<control>/value
_RE_INPUT = re.compile(
    r'^/devices/0/inputs/(\d+)/(Mute|Solo|Pan|Pan2|FaderLevel|FaderLevelTapered|'
    r'IOType|RecordPreEffects|Isolate|SoloSafe|Stereo|Active|SRConvert|'
    r'HiZImpedance|PGADisabledInLineMode|OutputDestination|'
    r'OutputMonitorMixEnable|Pad)/value$')

# RemoteSolo: a remote surface holds the solo engaged (device-wide flag)
_RE_REMOTE_SOLO = re.compile(r'^/RemoteSolo/value$')

# Preamp controls: /devices/0/inputs/<ch>/preamps/0/<control>/value
_RE_PREAMP = re.compile(
    r'^/devices/0/inputs/(\d+)/preamps/0/'
    r'(Gain|GainTapered|48V|Pad|HiZ|HiZImpedance|LowCut|Phase|Mute|'
    r'PreampInputKnobSteps|PreampSelectedParam|'
    r'PreampGainCustomDisplayText)/value$')

# Bus send controls: /devices/0/inputs/<ch>/sends/<send>/<control>/value
_RE_SEND = re.compile(
    r'^/devices/0/inputs/(\d+)/sends/(\d+)/(Gain|GainTapered|Pan|Bypass)/value$')

# Output/Monitor controls: /devices/0/outputs/<ch>/<control>/value
_RE_OUTPUT = re.compile(
    r'^/devices/0/outputs/(\d+)/(Mute|DimOn|Pad|MixToMono|MirrorsToDigital|'
    r'CRMonitorLevel|CRMonitorLevelTapered|SurroundMute|SurroundSolo|'
    r'AltMonEnabled|AltMonTrim|CRPGABypass|MixInSource|DimLevel|'
    r'DigitalOutputMode|OutputDestination|Identify|SurroundStereoFoldDown|'
    r'StereoSubOutputEnabled|SurroundOutputEnabled|SurroundTrimLevel)/value$')

# Aux bus controls: /devices/0/auxs/<bus>/<control>/value
_RE_AUX = re.compile(
    r'^/devices/0/auxs/(\d+)/(FaderLevel|FaderLevelTapered|Mute|MixToMono|'
    r'SendPostFader)/value$')

# Aux bus send controls: /devices/0/auxs/<bus>/sends/<send>/<control>/value
_RE_AUX_SEND = re.compile(
    r'^/devices/0/auxs/(\d+)/sends/(\d+)/(Gain|GainTapered|Pan|Bypass)/value$')

# Global device controls.  The device maps put TalkbackOn and a few more at
# the tree root (/TalkbackOn); /devices/0/<same> is also accepted.  The rest
# lives under /devices/0/.
_RE_DEVICE_GLOBAL = re.compile(
    r'^(?:/devices/0)?/(TalkbackOn|TalkbackInPhysicalCR|TalkbackMaster|'
    r'Enable24dBMode|ClipHold|PeakHold|TalkbackMicSelect)/value$|'
    r'^/devices/0/(DimAttenuation|TOSLinkOutput|TOSLink|ConverterMode|'
    r'MultiUnitOrder|DSPSpanning|Identify|'
    r'AltMonSelection|RemoteMonitorController|InputJackSensingDisabled|'
    r'OutputsAttenuate|CueBusCount|FuncSwitchMode|FuncSwitch2Mode|'
    r'SurroundMonitorMode)/value$')

# Global driver controls
_RE_GLOBAL = re.compile(
    r'^/(SampleRate|ClockSource|PostFaderMetering)/value$')


class HardwareRouter:
    """Routes control path changes to appropriate hardware writes.

    Uses pattern matching on TCP:4710 paths to classify controls into
    hardware categories. Each category has a different write path to
    the Apollo hardware.

    Write paths:
    - Preamp:        set_mixer_param(ch_type=1) → kernel → DSP mixer setting
    - Monitor:       set_mixer_param(ch_type=2) → kernel → DSP mixer setting
    - Bus params:    set_mixer_bus_param() → kernel → DSP mixer setting
    - Driver params: Direct register writes
    """

    def __init__(self, backend: HardwareBackend, state_tree=None,
                 input_buses: dict | None = None,
                 stereo_sends: bool = False, model=None):
        """`input_buses` maps an input name of the device map to its bus ID
        (the model's devices/<model>.json mixer_inputs synch_in); it wins
        over the name rule of bus_id_for_input_name().  Only needed where a
        model deviates from the rule (the Twin MkII's talkback input).

        `stereo_sends` also writes the right (side-1) entry of every send
        and of the talkback bus (parity/14 s5: the Mixer Engine always sends
        L and R).  Off by default until a listening test; the entry is only
        sent when the driver reports UA_FEAT_BUS_PARAM_SIDE."""
        self.backend = backend
        self.stereo_sends = stereo_sends
        self.state_tree = state_tree
        # The device's clock-source menu, {name: value} (device_clock_menu):
        # the daemon sets it from UA_IOCTL_GET_CLOCK_SOURCES; it names an
        # AES/EBU or MADI source as the device does.  CLOCK_SOURCE_MAP
        # covers the names it does not list.
        self.clock_sources: dict[str, int] = {}
        # model_info.ModelInfo of the attached Apollo (its devices/*.json);
        # the Apollo x4 when nothing says otherwise
        from model_info import ModelInfo
        self.model = model or ModelInfo.x4()
        if input_buses is None and model is not None:
            input_buses = dict(model.inputs)
        self._input_bus_overrides = {
            str(k).strip().upper(): v for k, v in (input_buses or {}).items()}
        self._output_roles_cache: dict | None = None
        self._preamp_inputs_cache: frozenset | None = None
        # Rate-limit gain writes to prevent DSP overload.
        self._gain_last: dict[int, tuple[int, float]] = {}  # ch → (db, time)
        self._gain_pending: dict[int, int] = {}  # ch → deferred db value
        self._input_bus_table: tuple | None = None
        # SetOtherParam ids the driver refused with EINVAL (kind gates, e.g.
        # 24 dB mode on the x4): not retried
        self._refused_other: set[int] = set()
        self._line_outputs_cache: list | None = None
        # Mute-until-settle: set while the outputs are attenuated after a
        # rate change (see begin_settle_mute).  Only models whose Mixer
        # Engine type supports OutputsAttenuate get it (the daemon sets
        # this from the detected model: the x4 does).
        self._settle: dict | None = None
        self.supports_outputs_attenuate = False
        # Device paused for system suspend: no hardware writes (set_paused)
        self.paused = False
        # devices/<model>.json of the attached model ({} = not known; the
        # handlers then fall back to what the device map has)
        self.model_data: dict = {}
        # Direct-output signals in use: input tree index -> signal index
        # (CMxMixer::AllocateDirectOutputSignal)
        self._direct_out_signals: dict[int, int] = {}
        self._sync_driver_state()

    def set_model_data(self, desc: dict | None):
        """Use the attached model's devices/*.json facts: its monitor
        taper (48 or 54 steps) and the engine feature flags the handlers
        gate on.  None or {} keeps the x4 behaviour."""
        self.model_data = dict(desc or {})
        steps = set_monitor_taper(mf.monitor_taper_table(self.model_data))
        log.info("Model data: %s (monitor taper %d steps)",
                 self.model_data.get("model", "unknown model"), steps)

    def _model_flag(self, key, default=None):
        return self.model_data.get(key, default) if self.model_data else default

    def _sync_driver_state(self):
        """Read current rate/clock from driver and update state tree.

        A readback: hardware=False, so it is never written back to the
        driver (or handed to PipeWire) even when the tree's on_set is
        already wired."""
        if not self.state_tree or not self.backend.connected:
            return

        rate = self.backend.get_driver_param(DRIVER_PARAM_SAMPLE_RATE)
        if rate:
            self.state_tree.set("/SampleRate/value", rate, hardware=False)
            log.info("Synced SampleRate from driver: %d Hz", rate)

        clock = self.backend.get_driver_param(DRIVER_PARAM_CLOCK_SOURCE)
        if clock is not None:
            menu = getattr(self, "clock_sources", None) or {}
            for name, val in (list(menu.items()) +
                              list(CLOCK_SOURCE_MAP.items())):
                if val == clock:
                    self.state_tree.set("/ClockSource/value", name,
                                        hardware=False)
                    log.info("Synced ClockSource from driver: %s", name)
                    break

    # ── Device-map lookups (names, not hard-coded indices) ──────────

    def _tree_value(self, path: str):
        if not self.state_tree:
            return None
        return self.state_tree.get_value(path)

    def _input_buses(self) -> tuple:
        """State tree input index -> bus ID, derived from the input names.

        Built once from the loaded device map's /inputs/N/Name values; the
        x4 default table when the tree has no named inputs.
        """
        table = self._input_bus_table
        if table is None:
            names = []
            while True:
                name = self._tree_value(
                    f"/devices/0/inputs/{len(names)}/Name/value")
                if name is None:
                    break
                names.append(name)
            table = tuple(
                self._input_bus_overrides.get(
                    str(n).strip().upper(), bus_id_for_input_name(n))
                for n in names) if names else _INPUT_IDX_TO_BUS
            if names:
                self._input_bus_table = table
        return table

    def _input_bus(self, ch: int) -> int | None:
        """Bus ID of tree input `ch`, None when it has no bus."""
        table = self._input_buses()
        return table[ch] if 0 <= ch < len(table) else None

    # Output roles by IOType, then by name, then by who carries the monitor
    # level.  n counts outputs of a kind in map order: x4 outputs 14-17 are
    # ('cue', 0..3), 18 is ('monitor', 0), 19-20 are ('hp', 0..1); the x8p's
    # are 18-21, 22 and 23-24, a generated map's wherever its rows land.
    _OUTPUT_KINDS = {"cue": "cue", "headphone": "hp", "monitor": "monitor"}
    _OUTPUT_NAME_KINDS = (
        (re.compile(r"^CUE\s*\d+$", re.I), "cue"),
        (re.compile(r"^(HP|HEADPHONES?)\s*\d*$", re.I), "hp"),
        (re.compile(r"^MONITOR$", re.I), "monitor"),
    )
    # The x4 layout, used only when the router has no state tree at all.
    _X4_OUTPUT_ROLES = {14: ("cue", 0), 15: ("cue", 1), 16: ("cue", 2),
                        17: ("cue", 3), 18: ("monitor", 0),
                        19: ("hp", 0), 20: ("hp", 1)}

    def _output_roles(self) -> dict:
        """Tree output index -> (kind, n) for every cue, headphone and
        monitor output of the loaded device map.

        kind comes from the output's IOType ("Cue", "Headphone",
        "Monitor"), else from its Name; n counts outputs of that kind in map
        order.  A "Monitor" output counts only if it carries
        CRMonitorLevel (the "Monitor R for Surround" output has a different
        IOType and none); an output without IOType that carries
        CRMonitorLevel and is not named as something else is the monitor.
        """
        cache = self._output_roles_cache
        if cache is not None:
            return cache
        roles: dict = {}
        counts: dict = {}
        found = False
        i = 0
        while True:
            base = f"/devices/0/outputs/{i}"
            io_type = self._tree_value(f"{base}/IOType/value")
            name = self._tree_value(f"{base}/Name/value")
            if io_type is None and name is None:
                break
            found = True
            kind = self._OUTPUT_KINDS.get(str(io_type or "").strip().lower())
            if kind is None and io_type is None:
                for rx, k in self._OUTPUT_NAME_KINDS:
                    if rx.match(str(name or "").strip()):
                        kind = k
                        break
            has_level = self._tree_value(
                f"{base}/CRMonitorLevel/value") is not None
            if kind is None and io_type is None and has_level:
                kind = "monitor"
            if kind == "monitor" and not has_level:
                kind = None
            if kind is not None:
                roles[i] = (kind, counts.get(kind, 0))
                counts[kind] = counts.get(kind, 0) + 1
            i += 1
        if not found:
            if self.state_tree is None:
                return dict(self._X4_OUTPUT_ROLES)
            if self._tree_value("/devices/0/inputs/0/Name/value") is None:
                return roles    # no map loaded yet: not cached
            # a map without outputs (the Realtime Rack): no roles
        self._output_roles_cache = roles
        return roles

    def _output_role(self, ch: int) -> tuple[str, int] | None:
        """('cue' | 'hp' | 'monitor', n) for tree output `ch`, or None."""
        return self._output_roles().get(ch)

    def output_index(self, kind: str, n: int = 0) -> int | None:
        """Tree output index of the n-th 'monitor' / 'hp' / 'cue' output."""
        for ch, role in self._output_roles().items():
            if role == (kind, n):
                return ch
        return None

    def talkback_bus(self) -> int:
        """Hardware bus of the talkback input: the model's talkback
        synch-in (Twin MkII 2), else 0x0A."""
        return self._input_bus_overrides.get("TALKBACK", TALKBACK_BUS_START)

    def mixer_kind(self) -> int | None:
        """The driver's mixer kind (GET_SYSTEM_INFO), None when unknown."""
        get = getattr(self.backend, "get_system_info", None)
        info = get() if callable(get) else None
        kind = info.get("mixer_kind") if isinstance(info, dict) else None
        return kind if isinstance(kind, int) and kind >= 0 else None

    def gate_unknown_kind(self) -> bool:
        """True when the backend has no mixer kind to report (USB), so the
        kind-gated monitor params stay unsent rather than going to a
        mixer that may refuse or misapply them."""
        return getattr(self.backend, "gate_unknown_kind", False) is True

    def aux_count(self) -> int:
        """AUX buses in the loaded map (x4: 2); 2 without a state tree."""
        if self.state_tree is None:
            return 2
        n = 0
        while self._tree_value(f"/devices/0/auxs/{n}/FaderLevel/value") \
                is not None:
            n += 1
        return n

    def init_send_buses(self) -> list:
        """Hardware buses the connect sequence zeroes the CUE 3/4 sends of:
        the map's input buses (Virtual 1-4 only, as macOS) and its AUX
        returns, in bus order (x4: 0-3, 8-0x0A, 0x0C-0x10, 0x12, 0x14-0x1B)."""
        buses = {b for b in self._input_buses()
                 if b is not None and b < _VIRTUAL_BUS_BASE + 4}
        buses.update(aux_bus_id(i) for i in range(self.aux_count()))
        return sorted(buses)

    def monitor_output_index(self) -> int | None:
        """Tree output index of the monitor (x4 18, x8p 22)."""
        return self.output_index("monitor", 0)

    def preamp_inputs(self) -> frozenset:
        """Tree input indexes that have a preamp, capped at the driver's
        MAX_PREAMP_CHANNELS: those with an inputs/N/preamps/0/Gain control
        (x4 0-3, x8p 0-7, Solo 0-1, Apollo 16: none)."""
        cache = self._preamp_inputs_cache
        if cache is not None:
            return cache
        if self.state_tree is None:
            return frozenset(range(_X4_PREAMP_COUNT))
        found = set()
        i = 0
        while self._tree_value(f"/devices/0/inputs/{i}/Name/value") \
                is not None:
            if self._tree_value(
                    f"/devices/0/inputs/{i}/preamps/0/Gain/value") is not None \
                    and i < MAX_PREAMP_CHANNELS:
                found.add(i)
            i += 1
        if i == 0:
            return frozenset(range(_X4_PREAMP_COUNT))   # no inputs loaded yet
        self._preamp_inputs_cache = frozenset(found)
        return self._preamp_inputs_cache

    def input_count(self) -> int:
        """Number of mixer inputs in the loaded device map (x4: 24)."""
        n = 0
        while self._tree_value(f"/devices/0/inputs/{n}/Name/value") \
                is not None:
            n += 1
        return n or len(_INPUT_IDX_TO_BUS)

    def _send_sub(self, base: str, send_idx: int, by_idx: dict) -> int | None:
        """SEL130 sub-param of a send: from its map ID, else its index."""
        send_id = self._tree_value(f"{base}/sends/{send_idx}/ID/value")
        if isinstance(send_id, str) and send_id.lower() in _SEND_ID_TO_SUB:
            return _SEND_ID_TO_SUB[send_id.lower()]
        return by_idx.get(send_idx)

    def _send_is_post_fader(self, base: str, send_idx: int) -> bool:
        """Pre/post-fader for input send `send_idx` (same rule the host
        meters use, ua_mixer_daemon._meter_gain_db): the send's own
        PostFaderOverride (0/1) if set, else auxs/N/SendPostFader for AUX N,
        else /HPnSendPostFader for CUE n."""
        sbase = f"{base}/sends/{send_idx}"
        override = self._tree_value(f"{sbase}/PostFaderOverride/value")
        if override in (0, 1):
            return override == 1
        send_id = str(self._tree_value(f"{sbase}/ID/value") or "")
        num = send_id[3:]
        if send_id.startswith("aux") and num.isdigit():
            return bool(self._tree_value(
                f"/devices/0/auxs/{int(num) - 1}/SendPostFader/value"))
        if send_id.startswith("cue") and num.isdigit():
            return bool(self._tree_value(
                f"/devices/0/HP{num}SendPostFader/value"))
        return False

    def _input_send_linear(self, ch: int, send_idx: int,
                           fader_db: float | None = None,
                           muted: bool | None = None,
                           gain_db: float | None = None) -> float:
        """SEL130 coefficient of an input send.

        Pre-fader: encode_input_fader(send dB).  Post-fader: the fader is
        added in dB (as the engine does), so with the send at 0 dB it equals
        the fader's own encode_input_fader() value.  Bypassed sends and,
        when post-fader, muted or -inf faders give 0.0.  Pass fader_db /
        muted / gain_db for a value that is being set right now and may not
        be in the tree yet.
        """
        base = f"/devices/0/inputs/{ch}"
        sbase = f"{base}/sends/{send_idx}"
        if self._truthy(self._tree_value(f"{sbase}/Bypass/value")):
            return 0.0
        if gain_db is None:
            gain_db = self._float(self._tree_value(f"{sbase}/Gain/value"),
                                  -144.0)
        if gain_db <= -144.0:
            return 0.0
        if not self._send_is_post_fader(base, send_idx):
            return encode_input_fader(gain_db)
        if fader_db is None:
            fader_db = self._float(
                self._tree_value(f"{base}/FaderLevel/value"), 0.0)
        if muted is None:
            muted = self._truthy(self._tree_value(f"{base}/Mute/value"))
        if muted or fader_db <= -144.0:
            return 0.0
        return encode_input_fader(gain_db + fader_db)

    @staticmethod
    def _float(v, default: float) -> float:
        try:
            return float(v)
        except (TypeError, ValueError):
            return default

    def _has_feature(self, bit: int) -> bool:
        fn = getattr(self.backend, "has_feature", None)
        return bool(fn(bit)) if callable(fn) else False

    def _send_pan(self, send_base: str) -> float | None:
        """The send's own Pan (-1..1), None when its map has none."""
        v = self._tree_value(f"{send_base}/Pan/value")
        try:
            return max(-1.0, min(1.0, float(v)))
        except (TypeError, ValueError):
            return None

    def _write_send(self, bus: int, sub: int, linear: float,
                    pan: float | None = None):
        """Write one send coefficient: the left entry, and with
        --stereo-sends the right entry too.

        With a pan the pair follows the sub-0 law (encode_mix_coeff_r):
        left = linear x sqrt2 x cos((pan+1) pi/4), right = ... sin; at
        centre both equal `linear`.  Without one the right entry equals the
        left.  Without --stereo-sends only the left entry is written, with
        `linear` as before (the pan is not applied).
        """
        if not self.stereo_sends:
            self.backend.set_mixer_bus_param(bus, sub, linear)
            return
        left = right = linear
        if pan is not None and linear:
            theta = (pan + 1.0) * math.pi / 4.0
            left = linear * math.sqrt(2.0) * math.cos(theta)
            right = linear * math.sqrt(2.0) * math.sin(theta)
        self.backend.set_mixer_bus_param(bus, sub, left)
        self.backend.set_mixer_bus_param(bus, sub, right, side=1)

    def _write_post_fader_sends(self, ch: int, bus: int, fader_db: float,
                                muted: bool):
        """Re-write the input's post-fader sends after a fader/mute change.

        The engine recomputes a post-fader send's coefficient whenever the
        fader moves; with the sends at unity and AUX post-fader this is the
        sub 0/3/4 triplet the macOS capture shows.
        """
        base = f"/devices/0/inputs/{ch}"
        for send_idx in range(len(_SEND_IDX_TO_SUB)):
            sub = self._send_sub(base, send_idx, _SEND_IDX_TO_SUB)
            if sub is None or not self._send_is_post_fader(base, send_idx):
                continue
            self._write_send(
                bus, sub,
                self._input_send_linear(
                    ch, send_idx, fader_db=fader_db, muted=muted),
                self._send_pan(f"{base}/sends/{send_idx}"))

    def _get_input_pan(self, ch: int) -> float:
        """Read current Pan value for an input from the state tree."""
        if self.state_tree:
            val = self.state_tree.get(f"/devices/0/inputs/{ch}/Pan/value")
            if val is not None:
                return float(val)
        return 0.0  # default center

    def _get_input_fader_db(self, ch: int) -> float:
        """Read current FaderLevel (dB) for an input from the state tree."""
        if self.state_tree:
            val = self.state_tree.get(f"/devices/0/inputs/{ch}/FaderLevel/value")
            if val is not None:
                return float(val)
        return -144.0  # default silence

    def set_paused(self, paused: bool):
        """Stop (True) or allow (False) every hardware write: the device is
        paused for system suspend (UA_EVT_PAUSED, reason UA_PAUSE_DEVICE).
        The state tree keeps taking changes; the resume push writes them."""
        self.paused = bool(paused)
        self.backend.paused = self.paused

    def on_set(self, path: str, value: Any):
        """Called by state_tree when a value changes. Route to hardware."""
        if self.paused:
            log.debug("SET %s = %r (device paused, tree only)", path, value)
            return
        # Try each pattern in priority order
        m = _RE_PREAMP.match(path)
        if m:
            ch, control = int(m.group(1)), m.group(2)
            self._handle_preamp(path, ch, control, value)
            return

        m = _RE_INPUT.match(path)
        if m:
            ch, control = int(m.group(1)), m.group(2)
            self._handle_input(path, ch, control, value)
            return

        if _RE_REMOTE_SOLO.match(path):
            self._handle_solo_recalc()
            return

        m = _RE_SEND.match(path)
        if m:
            ch, send_idx, control = int(m.group(1)), int(m.group(2)), m.group(3)
            self._handle_send(path, ch, send_idx, control, value)
            return

        m = _RE_OUTPUT.match(path)
        if m:
            ch, control = int(m.group(1)), m.group(2)
            self._handle_output(path, ch, control, value)
            return

        m = _RE_AUX.match(path)
        if m:
            bus, control = int(m.group(1)), m.group(2)
            self._handle_aux(path, bus, control, value)
            return

        m = _RE_AUX_SEND.match(path)
        if m:
            bus, send_idx, control = int(m.group(1)), int(m.group(2)), m.group(3)
            self._handle_aux_send(path, bus, send_idx, control, value)
            return

        m = _RE_DEVICE_GLOBAL.match(path)
        if m:
            control = m.group(1) or m.group(2)
            self._handle_device_global(path, control, value)
            return

        m = _RE_GLOBAL.match(path)
        if m:
            control = m.group(1)
            self._handle_global(path, control, value)
            return

        # Unmatched path — software only, no hardware write needed
        log.debug("SET %s = %r (software-only)", path, value)

    # ── SetOtherParam helpers ────────────────────────────────────────

    def _set_other(self, param: int, value: int, ch_idx: int = 0,
                   what: str = "") -> bool:
        """SetMixerParam type 2 (SetOtherParam; the kext ignores ch_idx).

        A param the driver refuses with EINVAL (a kind gate: the Mixer
        Engine does not send it to this model either) is remembered and
        skipped from then on, so a client that sets the property does not
        flood the log.
        """
        if param in self._refused_other:
            log.debug("HW %s: param 0x%02x refused by this device, skipped",
                      what or "OTHER", param)
            return False
        ok = self.backend.set_mixer_param(MONITOR_CHANNEL_TYPE, ch_idx,
                                          param, value)
        if not ok and getattr(self.backend, "last_errno", 0) == errno.EINVAL:
            self._refused_other.add(param)
            log.info("HW %s: param 0x%02x is refused by this device (kind "
                     "gate), not sending it again", what or "OTHER", param)
        return ok

    def _line_outputs(self) -> list:
        """Tree indexes of the outputs of IOType Line, in map order (the
        x4's analog outputs 1-4).  Alt monitors and the output pads are
        per pair of these."""
        cache = self._line_outputs_cache
        if cache is not None:
            return cache
        out = []
        i = 0
        while True:
            base = f"/devices/0/outputs/{i}"
            io_type = self._tree_value(f"{base}/IOType/value")
            if io_type is None and self._tree_value(
                    f"{base}/Name/value") is None:
                break
            if str(io_type or "").strip().lower() == "line":
                out.append(i)
            i += 1
        if i:
            self._line_outputs_cache = out
        return out

    def _line_pair(self, ch: int) -> int | None:
        """Pair number (0-based) of line output `ch`, None if it is not one."""
        lines = self._line_outputs()
        return lines.index(ch) // 2 if ch in lines else None

    # ── Solo recalculation ─────────────────────────────────────────

    @staticmethod
    def _talkback_coeff(db: float) -> float:
        return 0.0 if db <= -144.0 else 10.0 ** (db / 20.0) * TALKBACK_UNITY

    def _write_talkback(self, bus: int, coeff: float):
        """Talkback sub 0: the left entry, plus R = L with --stereo-sends."""
        self.backend.set_bus_pan(
            bus, coeff, coeff if self.stereo_sends else None)

    def _solo_engaged(self) -> bool:
        tree = self.state_tree
        if not tree:
            return False
        if self._truthy(tree.get_value("/RemoteSolo/value")):
            return True
        return any(self._truthy(tree.get_value(
            f"/devices/0/inputs/{i}/Solo/value"))
            for i in range(len(self._input_buses())))

    def _handle_solo_recalc(self):
        """Recalculate all bus gains based on solo state.

        When any channel is soloed, only soloed channels are audible —
        all non-soloed channels get their effective gain set to 0.0, or,
        with UA_FEAT_BUS_SOLO_FLAG, their normal coefficients plus the
        solo-mute flag on sub 0 (the driver applies the kext's negated
        pair / -1.0e-9 rule).  Their sends are still written as 0.0.
        When no channels are soloed, all channels get their real fader gain.
        Muted channels always get 0.0 regardless of solo state.

        Host rules of the engine's UpdateSoloState (parity M5): a channel
        with SoloSafe or Isolate set is not muted by another channel's
        solo, and /RemoteSolo (a remote surface holding the solo) engages
        the solo rule like a soloed channel does, without exempting any
        channel itself.  Mute still wins over every exemption.
        """
        if not self.state_tree:
            return

        any_solo = False
        channels = []
        for i in range(len(self._input_buses())):
            bus = self._input_bus(i)
            if bus is None:
                continue
            solo = self.state_tree.get_value(
                f"/devices/0/inputs/{i}/Solo/value")
            fader = self.state_tree.get_value(
                f"/devices/0/inputs/{i}/FaderLevel/value")
            mute = self.state_tree.get_value(
                f"/devices/0/inputs/{i}/Mute/value")
            exempt = any(self._truthy(self.state_tree.get_value(
                f"/devices/0/inputs/{i}/{n}/value"))
                for n in ("SoloSafe", "Isolate"))
            # an exempt channel is audible during a solo like a soloed one
            channels.append((i, bus, bool(solo) or exempt,
                             float(fader or 0), bool(mute)))
            if solo:
                any_solo = True
        if self._truthy(self.state_tree.get_value("/RemoteSolo/value")):
            any_solo = True

        solo_flag = any_solo and self._has_feature(UA_FEAT_BUS_SOLO_FLAG)
        for i, bus, soloed, fader_db, muted in channels:
            if self._pair_follower(i):
                # A linked pair's right half: its main-mix bus is written
                # with the leader (Pan2), only its own sends here
                self._write_post_fader_sends(
                    i, bus, fader_db, muted or (any_solo and not soloed))
                continue
            if bus == self.talkback_bus():
                # Talkback has its own encoding (sub 0 only, unity 3.976)
                db = -144.0 if (muted or (any_solo and not soloed)) \
                    else fader_db
                self._write_talkback(bus, self._talkback_coeff(db))
            elif not muted and any_solo and not soloed and solo_flag:
                # Muted by a solo (not by its own Mute): the driver applies
                # the kext's negated pair rule to the live coefficients, so
                # these are the unmuted values plus the flag.
                pan = self._get_input_pan(i)
                self.backend.set_bus_fader(
                    bus, encode_input_fader(fader_db),
                    encode_mix_coeff(fader_db, pan),
                    encode_mix_coeff_r(fader_db, pan), solo_mute=True)
                self._write_partner_bus(i, fader_db, False, solo_mute=True)
                self._write_post_fader_sends(i, bus, fader_db, True)
            elif muted or (any_solo and not soloed):
                self.backend.set_bus_fader(bus, 0.0, 0.0, 0.0)
                self._write_partner_bus(i, fader_db, True)
                self._write_post_fader_sends(i, bus, fader_db, True)
            else:
                # sub 0 is fader x pan; it must carry the pan too, or the
                # recalculation would drop a panned channel to centre.
                pan = self._get_input_pan(i)
                mix = encode_mix_coeff(fader_db, pan)
                mix_r = encode_mix_coeff_r(fader_db, pan)
                self.backend.set_bus_fader(bus, encode_input_fader(fader_db),
                                           mix, mix_r)
                self._write_partner_bus(i, fader_db, False)
                self._write_post_fader_sends(i, bus, fader_db, False)

        log.info("Solo recalc: any_solo=%s, %d channels updated",
                 any_solo, len(channels))

    # ── Preamp controls (ARM path) ────────────────────────────────

    def _handle_preamp(self, path: str, ch: int, control: str, value: Any):
        """Route preamp controls through DSP mixer settings.

        From kext disassembly of CUAD2DeviceMixer::setInputParamARM():
        Preamp parameters are written to DSP mixer settings (NOT CLI).
        Each setting packs 4 channels as 8-bit values in a 32-bit word.
        The kernel driver computes: setting_index = param_id + 7,
        and packs value into the channel's byte position.

        paramID mapping (from kext SetInputParam dispatch):
            Mute=0x00  Phase=0x01  48V=0x04  Pad=0x08
            HiZ=0x09   LowCut=0x0A  Gain=0x17
        """
        if ch not in self.preamp_inputs():
            log.debug("SET %s = %r (no preamp for input %d)", path, value, ch)
            return

        # Gain: macOS sends SEL131 triplet {0x0A, 0x09, 0x06} via DSP settings.
        # No rate limit — macOS doesn't rate-limit either. The driver's
        # batch flush only sends when DSP is idle, so we can't overflow.
        if control in ("Gain", "GainTapered"):
            if control == "GainTapered":
                gain_db = preamp_tapered_to_db(float(value))
            else:
                gain_db = float(value)
            gain_db = int(round(gain_db))

            # Skip exact duplicates only
            last = self._gain_last.get(ch)
            if last and last[0] == gain_db:
                return
            self._gain_last[ch] = (gain_db, time.monotonic())

            val_a = max(0, min(54, gain_db - 10))   # 0x0A: dB-10, max 54
            val_b = val_a                             # 0x09: same as A
            val_c = max(0, min(56, gain_db - 9))     # 0x06: dB-9, max 56

            log.info("HW PREAMP GAIN: input[%d] = %d dB → "
                     "GainA=0x0A:%d GainB=0x09:%d GainC=0x06:%d",
                     ch, gain_db, val_a, val_b, val_c)

            # Send all three as SEL131 (SetMixerParam) — same as macOS
            ok = True
            ok &= self.backend.set_mixer_param(PREAMP_CHANNEL_TYPE, ch,
                                               0x0A, val_a)
            ok &= self.backend.set_mixer_param(PREAMP_CHANNEL_TYPE, ch,
                                               0x09, val_b)
            ok &= self.backend.set_mixer_param(PREAMP_CHANNEL_TYPE, ch,
                                               0x06, val_c)
            if not ok:
                log.error("HW PREAMP GAIN: failed for input[%d]", ch)
            return

        if control == "HiZImpedance":
            self._write_hiz_impedance(path, ch, value)
            return

        if control == "HiZ":
            self._write_hiz_override(path, ch, value)
            return

        if control == "PreampInputKnobSteps":
            # kPropPreampInputKnobSteps (0xbc) -> input param 0x10, 4 bits
            # (CMxInput::UpdateHardwareState 0x100063a64: value & 0xf;
            # kext lane setting[23] nibble 4*ch)
            try:
                hw_value = int(value) & 0xF
            except (TypeError, ValueError):
                return
            log.info("HW PREAMP: input[%d].PreampInputKnobSteps = %r → %d "
                     "(param=0x10)", ch, value, hw_value)
            self.backend.set_mixer_param(PREAMP_CHANNEL_TYPE, ch,
                                         KNOB_STEPS_PARAM, hw_value)
            return

        if control == "PreampSelectedParam":
            # kPropPreampSelectedParam (0xbe) -> input param 0x12 (the engine
            # sends the stored int; the kext keeps 2 bits, setting[23]
            # [17+2ch:16+2ch])
            try:
                hw_value = int(value) & 0x3
            except (TypeError, ValueError):
                return
            log.info("HW PREAMP: input[%d].PreampSelectedParam = %r → %d "
                     "(param=0x12)", ch, value, hw_value)
            self.backend.set_mixer_param(PREAMP_CHANNEL_TYPE, ch,
                                         SELECTED_PARAM_PARAM, hw_value)
            return

        if control == "PreampGainCustomDisplayText":
            # A display string for clients; the engine sends nothing for it
            log.debug("SET %s = %r (display text, software-only)", path,
                      value)
            return

        param_id = ARM_PARAM_IDS.get(control)
        if param_id is None:
            log.debug("SET %s = %r (unknown preamp param '%s')", path, value, control)
            return

        # Encode value for DSP mixer setting (8-bit per channel)
        if control in ("48V", "Pad", "HiZ", "LowCut", "Phase", "Mute"):
            hw_value = 1 if value else 0
        else:
            try:
                hw_value = int(value) & 0xFFFFFFFF
            except (ValueError, TypeError):
                hw_value = 0

        log.info("HW PREAMP: input[%d].preamp.%s = %r → ch_idx=%d "
                 "paramID=0x%02x hw_value=%d (→ mixer setting[%d])",
                 ch, control, value, ch, param_id, hw_value,
                 param_id + 7)

        if not self.backend.set_mixer_param(PREAMP_CHANNEL_TYPE, ch,
                                            param_id, hw_value):
            log.error("HW PREAMP: failed to set input[%d].preamp.%s", ch, control)

        # A switch writes its own parameter only.  The gain used to be
        # re-sent after every switch against a batch flush that re-wrote
        # stale cached fields; the driver's one-shot mailbox writes a
        # setting once with its mask, so nothing stale is flushed, and the
        # re-send could put back a gain the front-panel knob had just
        # changed (the router's cache follows the readback only every
        # 20 ms, and not for 2 s after a client gain SET).

    # ── Input channel controls (SEL130 bus param path) ───────────

    def _handle_input(self, path: str, ch: int, control: str, value: Any):
        """Route input channel controls via SetMixerBusParam (SEL130).

        From IOKit capture (docs/iokit-mixer-mapping.md):
        - FaderLevel → linear float with -3dB offset, 3 sub-param writes
        - Mute → fader to 0.0 (linear silence), 3 sub-param writes
        - Pan → float coefficient, single sub_param=0 write
        - Solo → recalculates ALL bus gains (batch write)

        Bus IDs come from the input's name in the device map (_input_bus):
        analog 0x00-0x03, S/PDIF 0x08/0x09, ADAT 0x0C-0x0F / 0x14-0x17,
        virtual 0x18-0x1F, talkback 0x0A.
        """
        bus = self._input_bus(ch)
        if bus is None and control in ("FaderLevel", "FaderLevelTapered",
                                       "Mute", "Pan", "Pan2", "Solo"):
            if control == "Solo":
                self._handle_solo_recalc()
            else:
                log.debug("SET %s = %r (input %d has no mixer bus)",
                          path, value, ch)
            return

        if control in ("FaderLevel", "FaderLevelTapered"):
            if control == "FaderLevelTapered":
                db = fader_tapered_to_db(float(value))
            else:
                db = float(value)

            # Talkback bus uses different encoding (DTrace-verified):
            # sub=0 only, unity≈3.976, sub=3/4 always 0.0
            if bus == self.talkback_bus():
                linear = self._talkback_coeff(db)
                log.info("HW BUS: talkback.Fader = %.1f dB → %.6f "
                         "(bus=0x%04x, sub=0 only)", db, linear, bus)
                self._write_talkback(bus, linear)
                return

            muted = self._truthy(self._tree_value(f"{IN_BASE}/{ch}/Mute/value"))
            if self._pair_follower(ch):
                # The pair's leader carries this bus (its Pan2 position)
                log.debug("SET %s = %r (right half of a linked pair: the "
                          "leader drives bus 0x%04x)", path, value, bus)
                self._write_post_fader_sends(ch, bus, db, muted)
                return
            linear = encode_input_fader(db)
            # Read current pan to compute combined sub=0 coefficient
            pan = self._get_input_pan(ch)
            mix = encode_mix_coeff(db, pan)
            mix_r = encode_mix_coeff_r(db, pan)
            log.info("HW BUS: input[%d].Fader = %.1f dB → linear=%.6f "
                     "mix=%.6f/%.6f (L/R) pan=%.2f (bus=0x%04x)",
                     ch, db, linear, mix, mix_r, pan, bus)
            self.backend.set_bus_fader(bus, linear, mix, mix_r)
            self._write_partner_bus(ch, db, False)
            self._write_post_fader_sends(ch, bus, db, muted)
            return

        if control == "Mute":
            if value and value != "0" and value != "false":
                if self._pair_follower(ch):
                    log.debug("SET %s = %r (right half of a linked pair: "
                              "the leader's mute silences bus 0x%04x)",
                              path, value, bus)
                    self._write_post_fader_sends(
                        ch, bus, self._get_input_fader_db(ch), True)
                    return
                log.info("HW BUS: input[%d].Mute ON (fader→0.0, bus=0x%04x)",
                         ch, bus)
                self.backend.set_bus_fader(bus, 0.0, 0.0, 0.0)
                self._write_partner_bus(ch, 0.0, True)
                self._write_post_fader_sends(
                    ch, bus, self._get_input_fader_db(ch), True)
            else:
                # Mute OFF → recalc in case solo is active
                log.info("HW BUS: input[%d].Mute OFF (bus=0x%04x)", ch, bus)
                self._handle_solo_recalc()
            return

        if control in ("Pan", "Pan2"):
            try:
                pan_f = float(value)
            except (TypeError, ValueError):
                return
            partner = self._pair_partner(ch)
            if control == "Pan2":
                # kPropCRPanPosition2: only a linked pair's leader uses it,
                # for its second bus (synch-in + 1); a mono input has none
                if partner is None:
                    log.debug("SET %s = %r (Pan2 only moves a linked "
                              "pair's second bus)", path, value)
                    return
                bus = self._input_bus(partner)
            elif self._pair_follower(ch):
                log.debug("SET %s = %r (right half of a linked pair: the "
                          "leader's Pan2 places bus 0x%04x)", path, value, bus)
                return
            # Read current fader level to compute combined sub=0 coefficient
            fader_db = self._get_input_fader_db(ch)
            mix = encode_mix_coeff(fader_db, pan_f)
            mix_r = encode_mix_coeff_r(fader_db, pan_f)
            log.info("HW BUS: input[%d].%s = %.3f → mix=%.6f/%.6f (L/R) "
                     "fader=%.1fdB (bus=0x%04x)",
                     ch, control, pan_f, mix, mix_r, fader_db, bus)
            self.backend.set_bus_pan(bus, mix, mix_r)
            return

        if control == "Solo":
            log.info("HW BUS: input[%d].Solo = %r → recalculating all bus gains",
                     ch, value)
            self._handle_solo_recalc()
            return

        if control == "IOType":
            # Mic/Line switching: DTrace-verified, maps to
            # SEL131 ch_type=1 param=0x00, value 1=Line 0=Mic
            if ch not in self.preamp_inputs():
                log.debug("SET %s = %r (no preamp for input %d)", path, value, ch)
                return
            hw_value = 1 if str(value).lower() in ("line", "1", "true") else 0
            log.info("HW PREAMP: input[%d].IOType = %r → MicLine=%d",
                     ch, value, hw_value)
            self.backend.set_mixer_param(PREAMP_CHANNEL_TYPE, ch, 0x00, hw_value)
            return

        if control == "SRConvert":
            # Sample rate converter on S/PDIF inputs (inputs 12-13 on x4)
            # SEL131 ch_type=2, ch_idx=1, param=0x1f
            hw_value = 1 if value and value != "0" and value != "false" else 0
            log.info("HW MONITOR: input[%d].SRConvert = %r → %d "
                     "(param=0x1f, ch_idx=1)", ch, value, hw_value)
            self.backend.set_mixer_param(
                MONITOR_CHANNEL_TYPE, 1,
                MONITOR_PARAM["SRConvert"], hw_value)
            return

        if control == "HiZImpedance":
            if ch in self.preamp_inputs():
                self._write_hiz_impedance(path, ch, value)
            return

        if control == "Stereo":
            self._write_link(path, ch, value)
            # Linking hands the second bus to the leader (and unlinking
            # gives it back): rewrite every bus's coefficients
            self._handle_solo_recalc()
            return

        if control == "PGADisabledInLineMode":
            self._write_pga_disable(path, ch, value)
            return

        if control in ("OutputDestination", "OutputMonitorMixEnable"):
            self._write_direct_out(path, ch)
            return

        if control == "Pad":
            self._write_line_ref_level(path, ch, value)
            return

        if control in ("SoloSafe", "Isolate"):
            # Changes who a running solo mutes; nothing to do without one
            if self._solo_engaged():
                self._handle_solo_recalc()
            return

        # Software-only controls (RecordPreEffects, Active)
        log.debug("SET %s = %r (software-only)", path, value)

    def _write_hiz_impedance(self, path: str, ch: int, value: Any):
        """kPropHiZImpedance -> input param 0x11 = index into
        HIZ_IMPEDANCES_OHM.  Only the HiZ inputs (preamp channels 0 and 1);
        any other channel is refused by the kext.  The old "HiZ flag"
        write (param 0x11 = 1) selected 1 Mohm by accident."""
        if ch not in (0, 1) or ch not in self.preamp_inputs():
            log.debug("SET %s = %r (input %d has no HiZ impedance)",
                      path, value, ch)
            return
        code = hiz_impedance_code(value)
        if code is None:
            log.warning("HW PREAMP: input[%d] HiZImpedance %r is not one of "
                        "%s ohm", ch, value, list(HIZ_IMPEDANCES_OHM))
            return
        log.info("HW PREAMP: input[%d].HiZImpedance = %r → code %d "
                 "(%d ohm, param=0x11)", ch, value, code,
                 HIZ_IMPEDANCES_OHM[code])
        self.backend.set_mixer_param(PREAMP_CHANNEL_TYPE, ch,
                                     HIZ_IMPEDANCE_PARAM, code)

    def _write_link(self, path: str, ch: int, value: Any):
        """kPropStereo -> input param 0x02, pair leader (even channel)
        only: the kext writes both link bits of the pair for the leader and
        nothing for the odd channel, so the daemon does not send it."""
        if ch not in self.preamp_inputs():
            log.debug("SET %s = %r (no preamp for input %d)", path, value, ch)
            return
        if ch % 2:
            log.debug("SET %s = %r (odd channel: the pair leader carries "
                      "the link)", path, value)
            return
        hw_value = 1 if self._truthy(value) else 0
        log.info("HW PREAMP: input[%d].Stereo = %r → Link %d (param=0x02, "
                 "pair %d/%d)", ch, value, hw_value, ch + 1, ch + 2)
        self.backend.set_mixer_param(PREAMP_CHANNEL_TYPE, ch, LINK_PARAM,
                                     hw_value)

    # ── HiZ client override (N27) ───────────────────────────────────

    def _jack_sensing_disabled(self) -> bool:
        return self._truthy(self._tree_value(
            "/devices/0/InputJackSensingDisabled/value"))

    def _hiz_inputs(self) -> list:
        """Preamp inputs with a HiZ control in the device map (the x4: 0, 1;
        the Twin/Solo/Arrow: 0)."""
        return [ch for ch in sorted(self.preamp_inputs())
                if self._tree_value(
                    f"/devices/0/inputs/{ch}/preamps/0/HiZ/value") is not None]

    def _write_hiz_override(self, path: str, ch: int, value: Any) -> bool:
        """kPropHiZ -> input param 0x14 (setting[0] bit 24+ch), sent by
        CMxInput::UpdateHardwareState (0x1000638ac) only when the mixer's
        InputJackSensingDisabled is set and the input supports HiZ: with
        jack sensing on, the jack decides and the readback reports it."""
        if ch not in self._hiz_inputs():
            log.debug("SET %s = %r (input %d has no HiZ)", path, value, ch)
            return False
        if not self._jack_sensing_disabled():
            log.debug("SET %s = %r (jack sensing on: the HiZ jack decides)",
                      path, value)
            return False
        hw_value = 1 if self._truthy(value) else 0
        log.info("HW PREAMP: input[%d].HiZ = %r → %d (param=0x14, jack "
                 "sensing off)", ch, value, hw_value)
        self.backend.set_mixer_param(PREAMP_CHANNEL_TYPE, ch,
                                     HIZ_OVERRIDE_PARAM, hw_value)
        return True

    def _push_hiz_overrides(self):
        """Jack sensing was just disabled: the host now owns the HiZ
        switch, so send each HiZ input's current HiZ value."""
        for ch in self._hiz_inputs():
            self._write_hiz_override(
                f"/devices/0/inputs/{ch}/preamps/0/HiZ/value", ch,
                self._tree_value(f"/devices/0/inputs/{ch}/preamps/0/HiZ/value"))

    # ── Line inputs: reference level and PGA disable (N30) ──────────

    def _write_line_ref_level(self, path: str, ch: int, value: Any):
        """kPropPad on a Line input = its reference level (+4 dBu / -10
        dBV): CMxInput::SupportsReferenceLevel (IOType Line and the class's
        InputSupportsLineReferenceLevel, the model's line_ref_level_inputs
        synch-ins) gates input param 0x01 on the input's synch-in (both
        halves of a stereo input)."""
        bus = self._input_bus(ch)
        refs = self._model_flag("line_ref_level_inputs") or []
        if bus is None or bus not in refs:
            log.debug("SET %s = %r (no line reference level on input %d)",
                      path, value, ch)
            return
        hw_value = 1 if self._truthy(value) else 0
        targets = [bus]
        if self._truthy(self._tree_value(f"{IN_BASE}/{ch}/Stereo/value")) \
                and bus % 2 == 0 and bus + 1 in refs:
            targets.append(bus + 1)
        for b in targets:
            log.info("HW LINE IN: synch-in %d reference level = %r → %d "
                     "(param=0x01)", b, value, hw_value)
            self.backend.set_mixer_param(PREAMP_CHANNEL_TYPE, b,
                                         LINE_REF_PARAM, hw_value)

    def _write_pga_disable(self, path: str, ch: int, value: Any):
        """kPropPGADisabledInLineMode -> input param 0x15 (the input's fixed
        line mode, CMxInput::GetFixedLineMode; 0x100063058).  The engine
        sends it for preamp inputs of the classes with SupportsLinePGADisable
        (Apollo 8/8p, x6/x8/x8p and their Gen 2, x4 Gen 2, Twin X Gen 2);
        the driver refuses it on other kinds (the x4)."""
        if ch not in self.preamp_inputs() or \
                self._model_flag("line_pga_disable") is False:
            log.debug("SET %s = %r (no line PGA disable on input %d)", path,
                      value, ch)
            return
        hw_value = 1 if self._truthy(value) else 0
        log.info("HW PREAMP: input[%d].PGADisabledInLineMode = %r → %d "
                 "(param=0x15)", ch, value, hw_value)
        self.backend.set_mixer_param(PREAMP_CHANNEL_TYPE, ch,
                                     PGA_DISABLE_PARAM, hw_value)

    # ── Direct outputs / flex routing (N30, N18) ────────────────────

    def _max_direct_outputs(self) -> int:
        """GetMaxDirectOutputs of the model (8 on the Apollo/8/8p/16, 16 on
        HEXA, 0 elsewhere).  Without model data: 16 when the map has a
        per-input OutputDestination (only flex-routing maps carry it)."""
        n = self._model_flag("direct_outputs")
        if isinstance(n, int):
            return n
        return 16 if self._tree_value(
            f"{IN_BASE}/0/OutputDestination/value") is not None else 0

    def _write_direct_out(self, path: str, ch: int):
        """Input OutputDestination / OutputMonitorMixEnable -> input param
        0x13, the DSP route word (CMxInput::UpdateHardwareState 0x100062928):
        a destination other than Monitor takes a direct-output signal
        (AllocateDirectOutputSignal: lowest free of GetMaxDirectOutputs),
        Monitor frees it; bit 16 keeps the input in the monitor mix."""
        bus = self._input_bus(ch)
        if bus is None:
            log.debug("SET %s (input %d has no synch-in)", path, ch)
            return
        if self._max_direct_outputs() <= 0:
            # CMxMixer::SupportsOutputDestination is false on this class
            log.debug("SET %s (model has no direct outputs)", path)
            return
        base = f"{IN_BASE}/{ch}"
        dest = mf.output_destination_synch(
            self._tree_value(f"{base}/OutputDestination/value") or "Monitor")
        if dest is None:
            log.warning("HW INPUT: input[%d] OutputDestination %r unknown",
                        ch, self._tree_value(f"{base}/OutputDestination/value"))
            return
        mix = self._tree_value(f"{base}/OutputMonitorMixEnable/value")
        monitor_mix = True if mix is None else self._truthy(mix)
        signal = self._direct_out_signals.get(ch)
        if dest < 0:
            self._direct_out_signals.pop(ch, None)
            signal = mf.DIRECT_OUT_NONE
        elif signal is None:
            used = set(self._direct_out_signals.values())
            free = [s for s in range(self._max_direct_outputs())
                    if s not in used]
            if not free:
                log.warning("HW INPUT: input[%d] direct output to synch-out "
                            "%d: no free direct-output signal (max %d)", ch,
                            dest, self._max_direct_outputs())
                return
            signal = free[0]
            self._direct_out_signals[ch] = signal
        leader = self._truthy(self._tree_value(f"{base}/Stereo/value")) and \
            bus % 2 == 0
        route = mf.direct_out_route(signal, dest, monitor_mix, leader)
        log.info("HW INPUT: input[%d] direct out → synch-out %d signal %d "
                 "monitor mix %s → route 0x%05x (param=0x13, synch-in %d)",
                 ch, dest, signal, monitor_mix, route, bus)
        self.backend.set_mixer_param(PREAMP_CHANNEL_TYPE, bus,
                                     DIRECT_OUT_PARAM, route)

    # ── Stereo-linked inputs: one pan pair, two buses (N27) ─────────

    def _linked(self, ch: int) -> bool:
        return self._truthy(self._tree_value(f"{IN_BASE}/{ch}/Stereo/value"))

    def _pair_partner(self, ch: int) -> int | None:
        """Tree index of the right half when `ch` leads a linked stereo pair
        (Stereo set and an even synch-in); None otherwise.  As
        CMxInput::UpdateHardwareState, the leader drives both buses: its
        own with the Pan pair, synch-in + 1 with the Pan2 pair."""
        bus = self._input_bus(ch)
        if bus is None or bus % 2 or not self._linked(ch):
            return None
        for cand in (ch + 1, ch - 1):
            if self._input_bus(cand) == bus + 1:
                return cand
        return None

    def _pair_follower(self, ch: int) -> bool:
        """True for the right half (odd synch-in) of a linked pair: the
        engine sends no main-mix coefficients for it."""
        bus = self._input_bus(ch)
        if bus is None or bus % 2 == 0 or not self._linked(ch):
            return False
        return any(self._input_bus(c) == bus - 1 and self._linked(c)
                   for c in (ch - 1, ch + 1))

    def _write_partner_bus(self, ch: int, fader_db: float, silent: bool,
                           solo_mute: bool = False):
        """Main-mix coefficients of a linked pair's second bus from the
        leader `ch`: the leader's level with its Pan2 position."""
        partner = self._pair_partner(ch)
        if partner is None:
            return
        pbus = self._input_bus(partner)
        pan2 = self._float(self._tree_value(f"{IN_BASE}/{ch}/Pan2/value"),
                           0.0)
        if silent:
            self.backend.set_bus_fader(pbus, 0.0, 0.0, 0.0)
            return
        kw = {"solo_mute": True} if solo_mute else {}
        self.backend.set_bus_fader(pbus, encode_input_fader(fader_db),
                                   encode_mix_coeff(fader_db, pan2),
                                   encode_mix_coeff_r(fader_db, pan2), **kw)

    # ── Bus send controls (SEL130 bus param path) ──────────────────

    def _handle_send(self, path: str, ch: int, send_idx: int,
                     control: str, value: Any):
        """Route bus send controls via SetMixerBusParam (SEL130).

        A send writes ONLY its own sub-param on the INPUT bus (the send's
        map ID picks it; see SUB_PARAM_* for the numbering):
            AUX 1 -> sub 3, AUX 2 -> sub 4, CUE 1 -> sub 1, CUE 2 -> sub 2,
            CUE 3 -> sub 5, CUE 4 -> sub 6
        Sends use the same -3 dB offset encoding as faders.  Bypass forces
        the send to 0.0; clearing it restores the send's Gain.
        """
        sub = self._send_sub(f"/devices/0/inputs/{ch}", send_idx,
                             _SEND_IDX_TO_SUB)
        if sub is None:
            log.debug("SET %s = %r (send_idx=%d has no sub-param mapping)",
                      path, value, send_idx)
            return

        bus = self._input_bus(ch)
        if bus is None:
            log.debug("SET %s = %r (input %d has no mixer bus)",
                      path, value, ch)
            return
        base = f"/devices/0/inputs/{ch}/sends/{send_idx}"

        if control in ("Gain", "GainTapered"):
            if self._send_bypassed(base):
                log.debug("SET %s = %r (send bypassed, hardware stays 0.0)",
                          path, value)
                return
            if control == "GainTapered":
                db = fader_tapered_to_db(float(value))
            else:
                db = float(value)
            linear = self._input_send_linear(ch, send_idx, gain_db=db)
            log.info("HW BUS: input[%d].send[%d].Gain = %.1f dB → "
                     "linear=%.6f (bus=0x%04x sub=%d)",
                     ch, send_idx, db, linear, bus, sub)
            self._write_send(bus, sub, linear, self._send_pan(base))
            return

        if control == "Pan":
            # Send pan — only meaningful for AUX sends (sub=3/4).  It moves
            # the left/right pair only with --stereo-sends; otherwise there
            # is only a left entry and nothing to pan.
            log.info("HW BUS: input[%d].send[%d].Pan = %r (bus=0x%04x)",
                     ch, send_idx, value, bus)
            if self.stereo_sends and not self._send_bypassed(base):
                try:
                    pan = max(-1.0, min(1.0, float(value)))
                except (TypeError, ValueError):
                    return
                self._write_send(bus, sub,
                                 self._input_send_linear(ch, send_idx), pan)
            return

        if control == "Bypass":
            if self._truthy(value):
                linear = 0.0
            else:
                linear = self._input_send_linear(ch, send_idx)
            log.info("HW BUS: input[%d].send[%d].Bypass = %r → %.6f "
                     "(bus=0x%04x sub=%d)", ch, send_idx, value, linear,
                     bus, sub)
            self._write_send(bus, sub, linear, self._send_pan(base))
            return

        log.debug("SET %s = %r (send control, unhandled)", path, value)

    @staticmethod
    def _truthy(value) -> bool:
        return bool(value) and str(value).lower() not in ("0", "false")

    def _send_bypassed(self, base: str) -> bool:
        return self._truthy(self._tree_value(f"{base}/Bypass/value"))

    # ── Output/Monitor controls (SEL131 mixer param path) ────────

    def _toslink_in_bit_on(self) -> bool:
        """Bit 0x10 goes out only when the driver's toslink_in_bit is on:
        with it off the driver passes the daemon's byte through, and the
        x8p (ADAT default) would otherwise send 0x18 instead of 0x08."""
        probe = getattr(self.backend, "toslink_in_bit_enabled", None)
        try:
            return bool(probe()) if callable(probe) else False
        except Exception:
            return False

    def _device_mode_word(self, out_adat: bool | None = None,
                          in_adat: bool | None = None, converter=None,
                          order=None) -> int:
        """Other param 0x21 as CMxMixer::PushDeviceMode composes it
        (feature-mechanisms.md section 5): [2:0] multi-unit role (0 for a
        single unit; `multi_unit_role` once units are chained), 0x08
        TOSLink out = ADAT, 0x10 TOSLink in = ADAT, 0x20/0x28/0x30
        ConverterMode 1/2/3, [7:6] multi-unit order.  A field not given
        comes from the tree.  The driver writes the word's low byte."""
        def tree(path):
            return self._tree_value(f"/devices/0/{path}/value")
        if out_adat is None:
            out_adat = TOSLINK_OUTPUT_MAP.get(str(tree("TOSLinkOutput")),
                                              0) == 8
        if in_adat is None:
            in_adat = TOSLINK_OUTPUT_MAP.get(str(tree("TOSLink")), 0) == 8
        if converter is None:
            converter = tree("ConverterMode")
        if order is None:
            order = tree("MultiUnitOrder")
        try:
            conv = int(converter or 0)
        except (TypeError, ValueError):
            conv = 0
        try:
            order = max(0, min(3, int(order or 0)))
        except (TypeError, ValueError):
            order = 0
        word = getattr(self, "multi_unit_role", 0) & 0x7
        if out_adat:
            word |= 0x08
        if in_adat and self._toslink_in_bit_on():
            word |= 0x10
        if conv in (1, 2, 3):
            word |= {1: 0x20, 2: 0x28, 3: 0x30}[conv]
        return word | (order << 6)

    def _monitor_mute_mono(self, ch: int, mute: bool | None = None,
                           mono: bool | None = None) -> int:
        """Monitor param 0x03 value: bit 1 mute, bit 0 mono.  The switch
        not being set keeps its state from the tree (output `ch`)."""
        def tree(control):
            if self.state_tree is None:
                return False
            try:
                return self._truthy(self.state_tree.get_value(
                    f"/devices/0/outputs/{ch}/{control}/value"))
            except Exception:   # a map without the control
                return False
        if mute is None:
            mute = tree("Mute")
        if mono is None:
            mono = tree("MixToMono")
        return (2 if mute else 0) | (1 if mono else 0)

    def _handle_output(self, path: str, ch: int, control: str, value: Any):
        """Route output/monitor controls via SetMixerParam (SEL131).

        From IOKit capture (docs/iokit-mixer-mapping.md):
        Monitor controls use SEL131 with channel_type=2:
            CRMonitorLevel: param=0x01, ch_idx=1, value = 192+(dB×2)
            MonitorMute:    param=0x03, ch_idx=0, value = 2/0 (NOT 1!)
            DimOn:          param=0x44, ch_idx=0, value = 1/0

        Output 18 = MONITOR (Apollo x4), Output 19 = HP 1.
        """
        # CUE outputs: one tree output per cue (x4 map: 14 = CUE 1,
        # 15 = CUE 2, 16/17 = CUE 3/4, inactive), found by IOType.
        # From IOKit mapping: all ch_type=2, ch_idx=0
        #   CUE1 MONO=param 0x06 (1/0), CUE1 MIX=param 0x05 (0=on, 2=off)
        #   CUE2 MONO=param 0x08 (1/0), CUE2 MIX=param 0x07 (0=on, 2=off)
        # CUE 3/4 (4-cue models): mono 0x23 / 0x25, mix 0x22 / 0x24, mirror
        # source 0x30 / 0x31 (parity/14 s4: the kext's outputs of synch
        # 0x18 / 0x1a; which tree output carries them is M confidence).
        if control.startswith("Surround") or control == \
                "StereoSubOutputEnabled":
            self._handle_surround_output(path, ch, control, value)
            return

        role = self._output_role(ch)
        if role and role[0] == "cue":
            cue_idx = role[1]
            if cue_idx > 3:
                log.debug("SET %s = %r (CUE %d: no such cue bus)",
                          path, value, cue_idx + 1)
                return

            if control == "MixToMono":
                param = CUE_MONO_PARAMS[cue_idx]
                hw_value = 1 if value and value != "0" and value != "false" else 0
                log.info("HW CUE%d: MixToMono = %r → %d (param=0x%02x, ch_idx=0)",
                         cue_idx + 1, value, hw_value, param)
                self.backend.set_mixer_param(
                    MONITOR_CHANNEL_TYPE, 0, param, hw_value)
                return

            if control == "MixInSource":
                # CUE MIX: "cue"=CUE_MIX_ON (0, mix on), "mon"=CUE_MIX_OFF (2, mix off)
                # INVERTED encoding: hw 0 = mix enabled, hw 2 = mix disabled
                param = CUE_MIX_PARAMS[cue_idx]
                val_str = str(value).lower()
                hw_value = CUE_MIX_ON if val_str in ("cue", "on", "1") else CUE_MIX_OFF
                log.info("HW CUE%d: MixInSource = %r → %d (param=0x%02x, ch_idx=0, "
                         "inverted: 0=on 2=off)",
                         cue_idx + 1, value, hw_value, param)
                self.backend.set_mixer_param(
                    MONITOR_CHANNEL_TYPE, 0, param, hw_value)
                return

            if control == "OutputDestination":
                # CUE mirror source: string → hardware int
                # CUE1 → MirrorA (0x2e) + enable 0x3b, CUE2 → MirrorB (0x2f)
                # + enable 0x3c; CUE3/4 → 0x30 / 0x31 (no enable bit known)
                param = CUE_MIRROR_PARAMS[cue_idx]
                enable = CUE_MIRROR_ENABLE_PARAMS[cue_idx]
                source_str = str(value)
                hw_value = MIRROR_SOURCE_MAP.get(source_str, 0xFFFFFFFF)

                if hw_value == 0xFFFFFFFF:
                    # "None" → disable mirror
                    log.info("HW CUE%d: OutputDestination = %r → mirror disabled "
                             "(param=0x%02x, enable=%s, ch_idx=10)",
                             cue_idx + 1, value, param,
                             f"0x{enable:02x}" if enable else "-")
                    if enable is not None:
                        self.backend.set_mixer_param(
                            MONITOR_CHANNEL_TYPE, 10, enable, 0)
                    self.backend.set_mixer_param(
                        MONITOR_CHANNEL_TYPE, 10, param, hw_value & 0xFFFFFFFF)
                else:
                    # Set source, then enable mirror
                    log.info("HW CUE%d: OutputDestination = %r → %d "
                             "(param=0x%02x, enable=%s, ch_idx=10)",
                             cue_idx + 1, value, hw_value, param,
                             f"0x{enable:02x}" if enable else "-")
                    self.backend.set_mixer_param(
                        MONITOR_CHANNEL_TYPE, 10, param, hw_value)
                    if enable is not None:
                        self.backend.set_mixer_param(
                            MONITOR_CHANNEL_TYPE, 10, enable, 1)
                return

            log.debug("SET %s = %r (CUE output, unhandled control)", path, value)
            return

        if role == ("monitor", 0):
            if control in ("CRMonitorLevel", "CRMonitorLevelTapered"):
                if control == "CRMonitorLevelTapered":
                    hw_value = monitor_tapered_to_hw(float(value))
                    db = (hw_value - 192) / 2.0
                else:
                    db = float(value)
                    hw_value = encode_monitor_level(db)
                log.info("HW MONITOR: Level raw=%r → %.1f dB → %d (0x%02x)",
                         value, db, hw_value, hw_value)
                self.backend.set_mixer_param(
                    MONITOR_CHANNEL_TYPE, 1,
                    MONITOR_PARAM["CRMonitorLevel"], hw_value)
                return

            if control == "Mute":
                # Param 0x03 carries both monitor switches: bit 1 mute,
                # bit 0 mono (MixToMono), and the driver writes both bits
                # every time, so the other switch's state goes along
                hw_value = self._monitor_mute_mono(
                    ch, mute=self._truthy(value))
                log.info("HW MONITOR: Mute = %r → %d", value, hw_value)
                self.backend.set_mixer_param(
                    MONITOR_CHANNEL_TYPE, 0,
                    MONITOR_PARAM["MonitorMute"], hw_value)
                return

            if control == "DimOn":
                # Cold boot init uses ch_idx=7 (DTrace clock-power-cycle)
                hw_value = 1 if value and value != "0" and value != "false" else 0
                log.info("HW MONITOR: Dim = %r → %d", value, hw_value)
                self.backend.set_mixer_param(
                    MONITOR_CHANNEL_TYPE, 7,
                    MONITOR_PARAM["DimOn"], hw_value)
                self.backend.set_mixer_param(
                    MONITOR_CHANNEL_TYPE, 0,
                    MONITOR_PARAM["DimOn"], hw_value)
                return

            if control == "MixToMono":
                # VERIFIED: MixToMono uses param 0x03 (same as
                # MonitorMute): bit 0 mono, bit 1 mute (the Mixer Engine
                # sends 0, 1 and 2; the readback has both bits, so 3 is
                # both).  The mute state goes along (see Mute above).
                hw_value = self._monitor_mute_mono(
                    ch, mono=self._truthy(value))
                log.info("HW MONITOR: MixToMono = %r → %d (param=0x03)",
                         value, hw_value)
                self.backend.set_mixer_param(
                    MONITOR_CHANNEL_TYPE, 0,
                    MONITOR_PARAM["MixToMono"], hw_value)
                return

            if control == "Pad":
                # VERIFIED: "Pad" on outputs/18 = OutputRef
                # param=0x32, ch_idx=1, value: 1=-10dBV, 0=+4dBu
                hw_value = 1 if value and value != "0" and value != "false" else 0
                log.info("HW MONITOR: Pad (OutputRef) = %r → %d", value, hw_value)
                self.backend.set_mixer_param(
                    MONITOR_CHANNEL_TYPE, 1,
                    MONITOR_PARAM["Pad"], hw_value)
                return

            if control == "MirrorsToDigital":
                # DTrace-verified: ch_idx=10, param=0x1e
                hw_value = 1 if value and value != "0" and value != "false" else 0
                log.info("HW MONITOR: MirrorsToDigital = %r → %d", value, hw_value)
                self.backend.set_mixer_param(
                    MONITOR_CHANNEL_TYPE, 10,
                    MONITOR_PARAM["MirrorsToDigital"], hw_value)
                return

            if control == "MixInSource":
                # DTrace-verified: ch_idx=1, 0=MIX/1=CUE1/2=CUE2
                hw_value = MONITOR_SOURCE_MAP.get(str(value).lower(), 0)
                log.info("HW MONITOR: MixInSource = %r → %d", value, hw_value)
                self.backend.set_mixer_param(
                    MONITOR_CHANNEL_TYPE, 1,
                    MONITOR_PARAM["MixInSource"], hw_value)
                return

            if control == "DimLevel":
                # Dim attenuation: 1-7 (param=0x43, ch_idx=0)
                hw_value = max(1, min(7, int(value))) if value else 1
                log.info("HW MONITOR: DimLevel = %r → %d", value, hw_value)
                self.backend.set_mixer_param(
                    MONITOR_CHANNEL_TYPE, 0,
                    MONITOR_PARAM["DimLevel"], hw_value)
                return

            if control == "DigitalOutputMode":
                # 0=SPDIF, 8=ADAT (param=0x21, ch_idx=0): the TOSLink output
                # bit of the device-mode word, the other bits go along
                try:
                    out_adat = bool(int(value) & 0x08) if value else False
                except (TypeError, ValueError):
                    out_adat = False
                hw_value = self._device_mode_word(out_adat=out_adat)
                log.info("HW MONITOR: DigitalOutputMode = %r → %d",
                         value, hw_value)
                self.backend.set_mixer_param(
                    MONITOR_CHANNEL_TYPE, 0,
                    MONITOR_PARAM["DigitalOutputMode"], hw_value)
                return

            if control == "Identify":
                # Flash front panel LEDs (param=0x1d, ch_idx=0)
                hw_value = 1 if value and value != "0" and value != "false" else 0
                log.info("HW MONITOR: Identify = %r → %d (param=0x1d, ch_idx=0)",
                         value, hw_value)
                self.backend.set_mixer_param(
                    MONITOR_CHANNEL_TYPE, 0,
                    MONITOR_PARAM["Identify"], hw_value)
                return

            if control == "CRPGABypass":
                # kPropCRPGABypass (0xfc) -> param 0x3e, the engine sends the
                # inverted value (init: 1 = bypass property off)
                hw_value = 0 if self._truthy(value) else 1
                log.info("HW MONITOR: CRPGABypass = %r → %d (param=0x3e, "
                         "inverted)", value, hw_value)
                self._set_other(MONITOR_PARAM["CRPGABypass"], hw_value,
                                what="CRPGABypass")
                return

        # Headphone outputs (x4: outputs 19/20 = HP 1/2, IOType Headphone).
        # The level has its own monitor param: 0x02 for the first headphone
        # output (setting[2] bits 15:8, readback word 2) and 0x66 for the
        # next, as the Mixer Engine sends it.  It used to go to the
        # monitor's param 0x01 ("ganged"), which moved the monitor level.
        if role and role[0] == "hp":
            hp_idx = role[1]
            if control in ("CRMonitorLevel", "CRMonitorLevelTapered"):
                param = HP_LEVEL_PARAMS.get(hp_idx)
                if param is None:
                    log.debug("SET %s = %r (HP %d: no level param)",
                              path, value, hp_idx + 1)
                    return
                if control == "CRMonitorLevelTapered":
                    hw_value = monitor_tapered_to_hw(float(value))
                    db = (hw_value - 192) / 2.0
                else:
                    db = float(value)
                    hw_value = encode_monitor_level(db)
                log.info("HW HP%d: Level = %.1f dB → %d (0x%02x) "
                         "(param=0x%02x, ch_idx=%d)",
                         hp_idx + 1, db, hw_value, hw_value, param,
                         HP_CHANNEL_IDX)
                self.backend.set_mixer_param(
                    MONITOR_CHANNEL_TYPE, HP_CHANNEL_IDX, param, hw_value)
                return

            if control == "MixInSource":
                # DTrace-verified: ch_idx=1, p=0x3f (HP1) / 0x40 (HP2),
                # CUE1=0 / CUE2=1
                src_param = {0: "HP1Source", 1: "HP2Source"}.get(hp_idx)
                if src_param is not None:
                    hw_value = HP_SOURCE_MAP.get(str(value).lower(), 0)
                    log.info("HW HP%d: MixInSource = %r → %d",
                             hp_idx + 1, value, hw_value)
                    self.backend.set_mixer_param(
                        MONITOR_CHANNEL_TYPE, 1,
                        MONITOR_PARAM[src_param], hw_value)
                    return

            if hp_idx == 0 and control == "Mute":
                hw_value = 2 if value and value != "0" and value != "false" else 0
                log.info("HW HP: Mute = %r → %d (ganged with monitor)",
                         value, hw_value)
                self.backend.set_mixer_param(
                    MONITOR_CHANNEL_TYPE, HP_CHANNEL_IDX,
                    MONITOR_PARAM["MonitorMute"], hw_value)
                return

            if hp_idx == 0 and control == "DimOn":
                hw_value = 1 if value and value != "0" and value != "false" else 0
                log.info("HW HP: Dim = %r → %d (ganged with monitor)",
                         value, hw_value)
                self.backend.set_mixer_param(
                    MONITOR_CHANNEL_TYPE, HP_CHANNEL_IDX,
                    MONITOR_PARAM["DimOn"], hw_value)
                return

            log.info("HW HP: %s = %r (param not yet mapped)", control, value)
            return

        # Line outputs (x4 outputs 0-3): pad per pair, alt monitor per pair
        pair = self._line_pair(ch)
        if pair is not None:
            if control == "Pad" and pair < len(OUT_PAD_PARAMS):
                # kPropPad on a line output -> OutPadA..D (0x19..0x1c), one
                # bit per pair of outputs: the last of the two to be set wins
                param = OUT_PAD_PARAMS[pair]
                hw_value = 1 if self._truthy(value) else 0
                log.info("HW OUT: output[%d].Pad = %r → %d (param=0x%02x, "
                         "pair %d)", ch, value, hw_value, param, pair + 1)
                self._set_other(param, hw_value, what="OutPad")
                return
            if control == "AltMonTrim" and pair < len(ALTMON_TRIM_PARAMS):
                raw = encode_altmon_trim(self._float(value, 0.0))
                param = ALTMON_TRIM_PARAMS[pair]
                log.info("HW OUT: output[%d].AltMonTrim = %r dB → %d "
                         "(param=0x%02x, alt monitor %d)", ch, value, raw,
                         param, pair + 1)
                self._set_other(param, raw, what="AltMonTrim")
                return
            if control == "AltMonEnabled" and pair < len(ALTMON_ENABLE_PARAMS):
                param = ALTMON_ENABLE_PARAMS[pair]
                hw_value = 1 if self._truthy(value) else 0
                log.info("HW OUT: output[%d].AltMonEnabled = %r → %d "
                         "(param=0x%02x, alt monitor %d)", ch, value,
                         hw_value, param, pair + 1)
                self._set_other(param, hw_value, what="AltMonEnabled")
                return

        # Other outputs (individual analog/digital)
        log.debug("SET %s = %r (individual output, software-only)", path, value)

    def _handle_surround_output(self, path: str, ch: int, control: str,
                                value: Any):
        """Per-output surround properties (CMxOutput::UpdateHardwareState).

        SurroundStereoFoldDown -> other param 0x63 (the engine sends its
        global fold-down flag for an output that supports fold-down);
        StereoSubOutputEnabled -> other param 0x87.  SurroundTrimLevel /
        Solo / Mute go to params 0x4b+i / 0x53+i / 0x5b+i (0x6b/0x73/0x7b+i
        for channels 8-15, 0x88-0x8a for the sub) where i is the output's
        surround channel index (MixerType::GetSurroundChannelIndex, per
        surround mode); that index and the trim encoding are not decoded,
        so those stay in the tree.  SurroundOutputEnabled has no hardware
        write in the engine."""
        if control in ("SurroundStereoFoldDown", "StereoSubOutputEnabled"):
            if control == "SurroundStereoFoldDown" and self.model_data and \
                    self.model_data.get("surround_max_mode") is None:
                log.debug("SET %s = %r (model has no surround)", path, value)
                return
            param = SURROUND_FOLD_PARAM if control == \
                "SurroundStereoFoldDown" else STEREO_SUB_PARAM
            hw_value = 1 if self._truthy(value) else 0
            log.info("HW OUT: output[%d].%s = %r → %d (param=0x%02x)", ch,
                     control, value, hw_value, param)
            self._set_other(param, hw_value, what=control)
            return
        log.debug("SET %s = %r (surround channel index not decoded: "
                  "software-only)", path, value)

    # ── Aux bus controls (SEL130 bus param path) ─────────────────

    def _handle_aux(self, path: str, bus: int, control: str, value: Any):
        """Route aux bus controls via SetMixerBusParam (SEL130).

        From IOKit capture: aux faders use NO dB offset (unlike inputs).
        Bus IDs: aux 0=0x0010, aux 1=0x0012 (stride of 2).
        Aux faders send only sub_param=0 (single write, not 3).
        """
        bus_id = aux_bus_id(bus)

        if control in ("FaderLevel", "FaderLevelTapered"):
            if control == "FaderLevelTapered":
                db = fader_tapered_to_db(float(value))
            else:
                db = float(value)
            linear = encode_aux_fader(db)
            log.info("HW BUS: aux[%d].Fader = %.1f dB → linear=%.6f "
                     "(bus=0x%04x)", bus, db, linear, bus_id)
            # Aux faders: single sub_param=0 write (no pan, no L/R split)
            self.backend.set_mixer_bus_param(bus_id, SUB_PARAM_MIX, linear)
            return

        if control == "Mute":
            if value and value != "0" and value != "false":
                log.info("HW BUS: aux[%d].Mute ON (fader→0.0, bus=0x%04x)",
                         bus, bus_id)
                self.backend.set_mixer_bus_param(bus_id, SUB_PARAM_MIX, 0.0)
            else:
                # DTrace-verified: unmute restores fader level
                saved_db = -144.0
                if self.state_tree:
                    val = self.state_tree.get(
                        f"/devices/0/auxs/{bus}/FaderLevel/value")
                    if val is not None:
                        saved_db = float(val)
                linear = encode_aux_fader(saved_db)
                log.info("HW BUS: aux[%d].Mute OFF → restore %.1f dB = %.6f "
                         "(bus=0x%04x)", bus, saved_db, linear, bus_id)
                self.backend.set_mixer_bus_param(bus_id, SUB_PARAM_MIX, linear)
            return

        if control == "MixToMono":
            # CMxAuxBus MixToMono: aux 1 -> param 0x0a, aux 2 -> 0x0f
            # (the driver swaps the two value bits)
            param = AUX_MONO_PARAMS.get(bus)
            if param is None:
                log.debug("SET %s = %r (aux %d has no mono param)",
                          path, value, bus + 1)
                return
            hw_value = 1 if self._truthy(value) else 0
            log.info("HW AUX: aux[%d].MixToMono = %r → %d (param=0x%02x)",
                     bus, value, hw_value, param)
            self.backend.set_mixer_param(MONITOR_CHANNEL_TYPE, 0, param,
                                         hw_value)
            return

        log.debug("SET %s = %r (aux control, software-only)", path, value)

    def _handle_aux_send(self, path: str, bus: int, send_idx: int,
                         control: str, value: Any):
        """Route aux bus -> CUE send controls via SetMixerBusParam (SEL130).

        An aux->CUE send writes the CUE's sub-param (CUE 1/2/3/4 = sub
        1/2/5/6) on the AUX bus.  Sub 0 is the aux return fader and is
        never touched by a send (it used to be: every aux send move
        rewrote the aux return level).  The engine also sets pre/post-fader
        flags with OP 0x0c/0x11/...; their encoding is not decoded, so
        those are not sent.
        """
        bus_id = aux_bus_id(bus)
        base = f"/devices/0/auxs/{bus}"
        sub = self._send_sub(base, send_idx, _AUX_SEND_IDX_TO_SUB)
        if sub is None or sub == SUB_PARAM_MIX:
            log.debug("SET %s = %r (aux send_idx=%d has no sub-param)",
                      path, value, send_idx)
            return
        send_base = f"{base}/sends/{send_idx}"

        if control in ("Gain", "GainTapered"):
            if self._send_bypassed(send_base):
                log.debug("SET %s = %r (send bypassed, hardware stays 0.0)",
                          path, value)
                return
            if control == "GainTapered":
                db = fader_tapered_to_db(float(value))
            else:
                db = float(value)
            linear = encode_aux_fader(db)
            log.info("HW BUS: aux[%d].send[%d].Gain = %.1f dB → "
                     "linear=%.6f (bus=0x%04x sub=%d)", bus, send_idx, db,
                     linear, bus_id, sub)
            self._write_send(bus_id, sub, linear, self._send_pan(send_base))
            return

        if control == "Bypass":
            if self._truthy(value):
                linear = 0.0
            else:
                gain = self._tree_value(f"{send_base}/Gain/value")
                linear = encode_aux_fader(float(gain)) \
                    if gain is not None else 0.0
            log.info("HW BUS: aux[%d].send[%d].Bypass = %r → %.6f "
                     "(bus=0x%04x sub=%d)", bus, send_idx, value, linear,
                     bus_id, sub)
            self._write_send(bus_id, sub, linear, self._send_pan(send_base))
            return

        log.debug("SET %s = %r (aux send control, unhandled)", path, value)

    # ── Device-level globals (under /devices/0/) ──────────────────

    def _handle_device_global(self, path: str, control: str, value: Any):
        """Route device-level global controls via SetMixerParam (SEL131)."""
        if control == "TalkbackOn":
            # Cold boot init uses ch_idx=7 (DTrace clock-power-cycle capture)
            # Runtime toggle uses ch_idx=0 (DTrace talkback capture)
            # Send to BOTH to cover init and runtime scenarios
            hw_value = 1 if value and value != "0" and value != "false" else 0
            log.info("HW MONITOR: TalkbackOn = %r → %d",
                     value, hw_value)
            self.backend.set_mixer_param(
                MONITOR_CHANNEL_TYPE, 7,
                MONITOR_PARAM["TalkbackOn"], hw_value)
            self.backend.set_mixer_param(
                MONITOR_CHANNEL_TYPE, 0,
                MONITOR_PARAM["TalkbackOn"], hw_value)
            return

        if control == "DimAttenuation":
            # /devices/0/DimAttenuation (int 0-60 dB) → DimLevel step (1-7)
            step = dim_attenuation_to_step(int(value) if value else 0)
            log.info("HW MONITOR: DimAttenuation = %r dB → DimLevel step %d "
                     "(param=0x43, ch_idx=0)", value, step)
            self.backend.set_mixer_param(
                MONITOR_CHANNEL_TYPE, 0,
                MONITOR_PARAM["DimLevel"], step)
            return

        if control in ("TOSLinkOutput", "TOSLink", "ConverterMode",
                       "MultiUnitOrder"):
            # One device-mode word (other param 0x21) carries all four, as
            # CMxMixer::PushDeviceMode composes it: the new value plus the
            # tree's other fields
            over = {"TOSLinkOutput": "out_adat", "TOSLink": "in_adat",
                    "ConverterMode": "converter",
                    "MultiUnitOrder": "order"}[control]
            arg = value
            if over in ("out_adat", "in_adat"):
                arg = TOSLINK_OUTPUT_MAP.get(str(value), 0) == 8
            hw_value = self._device_mode_word(**{over: arg})
            log.info("HW MONITOR: %s = %r → device mode 0x%02x "
                     "(param=0x21, ch_idx=0)", control, value, hw_value)
            self.backend.set_mixer_param(
                MONITOR_CHANNEL_TYPE, 0,
                MONITOR_PARAM["DigitalOutputMode"], hw_value)
            return

        if control == "DSPSpanning":
            # /devices/0/DSPSpanning (bool) → DSP pairing mode
            hw_value = 1 if value and value != "0" and value != "false" else 0
            log.info("HW MONITOR: DSPSpanning = %r → %d (param=0x16, ch_idx=0)",
                     value, hw_value)
            self.backend.set_mixer_param(
                MONITOR_CHANNEL_TYPE, 0,
                MONITOR_PARAM["DSPSpanning"], hw_value)
            return

        if control == "Identify":
            # /devices/0/Identify (bool) → flash front panel LEDs
            hw_value = 1 if value and value != "0" and value != "false" else 0
            log.info("HW MONITOR: Identify = %r → %d (param=0x1d, ch_idx=0)",
                     value, hw_value)
            self.backend.set_mixer_param(
                MONITOR_CHANNEL_TYPE, 0,
                MONITOR_PARAM["Identify"], hw_value)
            return

        # Mixer-level properties that map to one SetOtherParam each
        # (parity/14 s4 and s8); a bool is v != 0
        bool_params = {
            "TalkbackInPhysicalCR": MONITOR_PARAM["TBConfig"],
            "OutputsAttenuate": MONITOR_PARAM["OutputsAttenuate"],
            "InputJackSensingDisabled": MONITOR_PARAM["JackSensingDisabled"],
            "RemoteMonitorController": MONITOR_PARAM["RemoteMonitor"],
            "Enable24dBMode": MONITOR_PARAM["Enable24dB"],
        }
        if control in bool_params:
            param = bool_params[control]
            hw_value = 1 if self._truthy(value) else 0
            if control == "OutputsAttenuate" and self._settle is not None:
                # attenuated until the new rate has settled: the user's
                # value is applied when the hold ends
                self._settle["user"] = hw_value
                log.info("HW MONITOR: OutputsAttenuate = %r held until the "
                         "rate change has settled", value)
                return
            log.info("HW MONITOR: %s = %r → %d (param=0x%02x)",
                     control, value, hw_value, param)
            self._set_other(param, hw_value, what=control)
            if control == "InputJackSensingDisabled" and hw_value:
                # the host owns the HiZ switches from now on
                self._push_hiz_overrides()
            return

        if control in ("ClipHold", "PeakHold"):
            # Root /ClipHold, /PeakHold: the value's index goes to every
            # mixer as other param 0x13 / 0x14 (CMxRoot::UpdateHardwareState)
            names = mf.HOLD_NAMES if control == "ClipHold" \
                else mf.HOLD_NAMES[:-1]
            idx = self._enum_index(value, names)
            if idx is None:
                log.warning("HW MONITOR: %s %r is not one of %s", control,
                            value, list(names))
                return
            param = CLIP_HOLD_PARAM if control == "ClipHold" \
                else PEAK_HOLD_PARAM
            log.info("HW MONITOR: %s = %r → %d (param=0x%02x, %d ms)",
                     control, value, idx, param, mf.HOLD_MS[idx])
            self._set_other(param, idx, what=control)
            return

        if control == "TalkbackMicSelect":
            # Root kPropTalkbackMicSelect -> other param 0x65 on the mixers
            # whose type supports talkback (CMxRoot::UpdateHardwareState
            # 0x100095834); the kext keeps 2 bits
            if self.model_data and not self.model_data.get("talkback"):
                log.debug("SET %s = %r (model has no talkback)", path, value)
                return
            try:
                hw_value = int(value) & 0x3
            except (TypeError, ValueError):
                log.warning("HW MONITOR: TalkbackMicSelect %r invalid", value)
                return
            log.info("HW MONITOR: TalkbackMicSelect = %r → %d (param=0x65)",
                     value, hw_value)
            self._set_other(TB_MIC_SELECT_PARAM, hw_value,
                            what="TalkbackMicSelect")
            return

        if control in ("FuncSwitchMode", "FuncSwitch2Mode"):
            self._write_func_switch(path, control, value)
            return

        if control == "SurroundMonitorMode":
            self._write_surround_mode(path, value)
            return

        if control == "TalkbackMaster":
            # kPropTalkbackMaster is the order of the unit that owns the
            # talkback; the hardware bit says "this unit is it"
            order = self._tree_value("/devices/0/MultiUnitOrder/value")
            try:
                is_master = int(value) == int(order or 0)
            except (TypeError, ValueError):
                is_master = False
            log.info("HW MONITOR: TalkbackMaster = %r (this unit %r) → %d "
                     "(param=0x48)", value, order or 0, int(is_master))
            self._set_other(MONITOR_PARAM["TalkbackMaster"], int(is_master),
                            what="TalkbackMaster")
            return

        if control == "AltMonSelection":
            # kPropAltMonSelection: 2 bits (setting[13] [29:28])
            try:
                hw_value = int(value) & 3
            except (TypeError, ValueError):
                log.warning("HW MONITOR: AltMonSelection %r invalid", value)
                return
            log.info("HW MONITOR: AltMonSelection = %r → %d (param=0x3a)",
                     value, hw_value)
            self._set_other(MONITOR_PARAM["AltMonSelection"], hw_value,
                            what="AltMonSelection")
            return

        if control == "CueBusCount":
            # The engine enables the DSP buses {1, 2, 5, 6} for CUE 1-4 as
            # index < CueBusCount: a 4-cue model turns 5 and 6 (CUE 3/4) on
            try:
                count = int(value)
            except (TypeError, ValueError):
                return
            for idx, bus in enumerate(CUE_BUS_ENABLE_BUSES):
                if idx >= 2 and count > idx:
                    log.info("HW MONITOR: CueBusCount = %d → enable bus %d "
                             "(CUE %d)", count, bus, idx + 1)
                    self.backend.set_mixer_param(0, bus, 0x01, 1)
            return

    @staticmethod
    def _enum_index(value, names) -> int | None:
        """Index of a string value in `names` (case-insensitive), or an int
        index in range; None otherwise."""
        if isinstance(value, bool):
            return None
        if isinstance(value, (int, float)) and float(value) == int(value):
            return int(value) if 0 <= int(value) < len(names) else None
        s = str(value).strip().upper()
        for i, n in enumerate(names):
            if s == n.upper():
                return i
        return None

    def _write_func_switch(self, path: str, control: str, value: Any):
        """kPropFuncSwitchMode / FuncSwitch2Mode -> other param 0x42 / 0x83
        with the DescartesFuncSwitchMode value (none 0, alt 2 1, mono 2, dim
        3, talkback 4, alt 1 5; CMxMixer::UpdateHardwareState 0x100071df8).
        Only the modes the class supports (Apollo 8/8p: none..dim; HEXA up to
        talkback; x16D all six and the second switch)."""
        if isinstance(value, str):
            v = mf.FUNC_SWITCH_VALUES.get(value.strip().lower())
            name = value.strip().lower()
        else:
            try:
                v = int(value)
            except (TypeError, ValueError):
                v = None
            name = next((n for n, x in mf.FUNC_SWITCH_VALUES.items()
                         if x == v), None)
        if v is None or name is None:
            log.warning("HW MONITOR: %s %r is not a function switch mode",
                        control, value)
            return
        fs = self._model_flag("func_switch", None)
        if self.model_data and not fs:
            log.debug("SET %s = %r (model has no function switch)", path,
                      value)
            return
        if fs:
            if control == "FuncSwitch2Mode" and not fs.get("second"):
                log.debug("SET %s = %r (no second function switch)", path,
                          value)
                return
            if name not in (fs.get("modes") or []):
                log.warning("HW MONITOR: %s %r not offered by this model "
                            "(%s)", control, value, fs.get("modes"))
                return
        param = FUNC_SWITCH_PARAM if control == "FuncSwitchMode" \
            else FUNC_SWITCH2_PARAM
        log.info("HW MONITOR: %s = %r → %d (param=0x%02x)", control, value,
                 v, param)
        self._set_other(param, v, what=control)

    def _write_surround_mode(self, path: str, value: Any):
        """kPropSurroundMonitorMode -> other param 0x4a = the mode index
        (CMxMixer::UpdateHardwareState 0x100071c60), for the modes the class
        supports (x6 up to 5.1, x8/x8p up to 7.1, x16/x16D the Atmos modes
        8-10).  The engine also needs its hidden EnableSurround setting;
        the daemon follows the model data only."""
        idx = self._enum_index(value, mf.SURROUND_MODES)
        top = self._model_flag("surround_max_mode", None)
        if idx is None and isinstance(value, int) and top and 0 <= value <= top:
            idx = value
        if idx is None:
            log.warning("HW MONITOR: SurroundMonitorMode %r unknown", value)
            return
        if self.model_data and (top is None or idx > top):
            log.debug("SET %s = %r (surround mode not offered by this model)",
                      path, value)
            return
        log.info("HW MONITOR: SurroundMonitorMode = %r → %d (param=0x4a)",
                 value, idx)
        self._set_other(SURROUND_MODE_PARAM, idx, what="SurroundMonitorMode")

    # ── Global/driver controls ────────────────────────────────────

    def clock_source_names(self) -> set:
        """The names a /ClockSource SET may use: the device's menu
        (clock_sources) and CLOCK_SOURCE_MAP."""
        menu = getattr(self, "clock_sources", None) or {}
        return set(menu) | set(CLOCK_SOURCE_MAP)

    def set_global(self, path: str, value: Any) -> GlobalWriteResult | None:
        """Validate and write a clock control (SetDriverParameter, sel 0xA6
        in the kext: {paramID, reserved, value_lo, value_hi}).

        Returns None for a path that is not a clock control, else whether
        the driver took it.  The daemon publishes the value only when
        accepted; a refused or failed ioctl leaves the tree as it was (the
        hardware may still have changed: that is not a rollback claim).
        A sample rate PipeWire takes over (clock.force-rate) counts as
        accepted, as before.

        WARNING: Changing sample rate or clock source while streaming
        will disrupt audio. The macOS kext warns but proceeds.
        """
        if path not in CLOCK_CONTROL_PATHS:
            return None
        result = normalize_global(path, value, self.clock_source_names())
        if not result.accepted:
            if path == "/ClockSource/value":
                log.warning("HW DRV: ClockSource unknown value: %r "
                            "(valid: %s)", value,
                            sorted(self.clock_source_names()))
            else:
                log.warning("HW DRV: SampleRate invalid value: %r (valid: %s)",
                            value, sorted(VALID_SAMPLE_RATES))
            return result
        try:
            if path == "/SampleRate/value":
                rate = result.value
                # While streams are open, the app that owns them re-prepares
                # at its own rate and undoes a clock change made under it.
                # When that's PipeWire, ask it to run its graph at this rate
                # instead, as Console changes the system rate on macOS.
                if pipewire_force_rate(
                        rate, device=getattr(self.backend, "device_path", None)):
                    log.info("HW DRV: SampleRate → %d Hz (PipeWire "
                             "clock.force-rate)", rate)
                    return result
                transport = self.backend.get_driver_param(
                    DRIVER_PARAM_TRANSPORT_RUNNING)
                if transport and transport > 0:
                    log.warning("HW DRV: changing SampleRate while transport "
                                "running — audio will be disrupted")
                log.info("HW DRV: SampleRate → %d Hz", rate)
                ok = self.backend.set_driver_param(DRIVER_PARAM_SAMPLE_RATE,
                                                   rate)
            else:
                menu = getattr(self, "clock_sources", None) or {}
                source = menu.get(result.value,
                                  CLOCK_SOURCE_MAP.get(result.value))
                log.info("HW DRV: ClockSource → %s (value=%d)",
                         result.value, source)
                ok = self.backend.set_driver_param(DRIVER_PARAM_CLOCK_SOURCE,
                                                   source)
        except Exception:
            log.exception("HW DRV: %s write failed", path)
            ok = False
        if not ok:
            return GlobalWriteResult(False, reason="not acknowledged by the "
                                     "driver; hardware outcome unknown")
        return result

    def _handle_global(self, path: str, control: str, value: Any):
        """Route global controls (sample rate, clock source) that arrive
        through the tree's on_set: the WS server, a session or snapshot
        load.  The TCP SET handlers call set_global() before publishing."""
        if control in ("SampleRate", "ClockSource"):
            self.set_global(path, value)
            return

        if control == "PostFaderMetering":
            log.info("HW DRV: PostFaderMetering = %r (software-only)", value)
            return

        log.debug("HW DRV: %s = %r (unhandled global)", control, value)

    # ── State re-apply after a daemon restart (A6) ──────────────────

    # Controls the hardware reports back (preamp gain and switches, link,
    # monitor level/mute/dim/mono, headphone levels, talkback button) are
    # never in this list: after a restart the Apollo's own values win.  What
    # cannot be read back is pushed once from the persisted state, in the
    # order the Mixer Engine's PushEntireMixerTreeToHardware uses:
    # device-level settings, aux buses, input sends and solo, outputs.
    _REAPPLY_DEVICE = (
        "CueBusCount", "OutputsAttenuate", "InputJackSensingDisabled",
        "DimAttenuation", "TOSLinkOutput", "AltMonSelection",
        "RemoteMonitorController", "DSPSpanning",
        "TalkbackInPhysicalCR", "TalkbackMaster", "Enable24dBMode",
        "ClipHold", "PeakHold", "TalkbackMicSelect", "FuncSwitchMode",
        "FuncSwitch2Mode", "SurroundMonitorMode")
    # Per-input settings the hardware does not report back
    _REAPPLY_INPUT = ("SRConvert", "HiZImpedance", "PGADisabledInLineMode",
                      "OutputDestination", "Pad")
    _RE_REAPPLY = re.compile(
        r'^/devices/0/(?P<kind>auxs|inputs|outputs)/(?P<n>\d+)/'
        r'(?:(?P<sub>sends|preamps)/(?P<m>\d+)/)?(?P<ctl>\w+)/value$')
    _REAPPLY_AUX = ("FaderLevel", "FaderLevelTapered", "Mute", "MixToMono")
    _REAPPLY_SEND = ("Gain", "GainTapered", "Bypass", "Pan",
                     "PostFaderOverride")

    def reapply_saved_state(self, saved) -> int:
        """Push the persisted state that is not read back from the hardware.

        `saved` is the set of paths (or a mapping keyed by path) the user
        changed (StateTree._dirty); values are read from the tree.  Returns
        the number of controls written.  Input faders, pans and mutes are
        pushed by the init path itself; this adds device-level settings,
        aux buses, input and aux sends (both of the CUE 3/4 sub-params too),
        solo, the CUE/headphone-source/line-out/alt-monitor outputs and the
        talkback configuration.
        """
        if not self.state_tree:
            return 0
        paths = [p for p in saved if isinstance(p, str)
                 and p.endswith("/value")]
        have = set(paths)
        count = 0

        def push(path):
            nonlocal count
            value = self._tree_value(path)
            if value is None:
                return
            self.on_set(path, value)
            count += 1

        # 1. device-level settings (the engine's mixer/root properties)
        for name in self._REAPPLY_DEVICE:
            for path in (f"/devices/0/{name}/value", f"/{name}/value"):
                if path in have:
                    push(path)

        # Group the rest by object
        aux_ctl: dict[int, set] = {}
        aux_sends: dict[tuple, set] = {}
        in_sends: dict[tuple, set] = {}
        in_misc: list[tuple] = []
        out_ctl: dict[int, set] = {}
        solo = False
        for path in paths:
            m = self._RE_REAPPLY.match(path)
            if not m:
                continue
            kind, n, sub, mm, ctl = (m.group("kind"), int(m.group("n")),
                                     m.group("sub"), m.group("m"),
                                     m.group("ctl"))
            if kind == "auxs":
                if sub == "sends" and ctl in self._REAPPLY_SEND:
                    aux_sends.setdefault((n, int(mm)), set()).add(ctl)
                elif sub is None and ctl in self._REAPPLY_AUX:
                    aux_ctl.setdefault(n, set()).add(ctl)
            elif kind == "inputs":
                if sub == "sends" and ctl in self._REAPPLY_SEND:
                    in_sends.setdefault((n, int(mm)), set()).add(ctl)
                elif ctl == "Solo" and sub is None:
                    solo = solo or self._truthy(self._tree_value(path))
                elif ctl in self._REAPPLY_INPUT and sub is None:
                    in_misc.append((n, ctl, path))
                elif ctl == "OutputMonitorMixEnable" and sub is None:
                    # written with OutputDestination (one route word)
                    in_misc.append((n, ctl, path))
                elif ctl == "HiZImpedance":
                    in_misc.append((n, ctl, path))
                elif sub == "preamps" and ctl in ("PreampInputKnobSteps",
                                                  "PreampSelectedParam"):
                    in_misc.append((n, ctl, path))
            elif kind == "outputs" and sub is None:
                out_ctl.setdefault(n, set()).add(ctl)

        # 2. aux buses: return fader, mute, mono, then their CUE sends
        for n in sorted(aux_ctl):
            base = f"/devices/0/auxs/{n}"
            ctls = aux_ctl[n]
            fader = next((c for c in ("FaderLevel", "FaderLevelTapered")
                          if c in ctls), None)
            if fader:
                push(f"{base}/{fader}/value")
            if "Mute" in ctls:
                push(f"{base}/Mute/value")
            if "MixToMono" in ctls:
                push(f"{base}/MixToMono/value")
        for (n, k) in sorted(aux_sends):
            base = f"/devices/0/auxs/{n}/sends/{k}"
            ctl = "Bypass" if self._truthy(
                self._tree_value(f"{base}/Bypass/value")) else "Gain"
            push(f"{base}/{ctl}/value")

        # 3. input sends (sub 1-6, CUE 3/4 included), per-input settings
        for (n, k) in sorted(in_sends):
            base = f"/devices/0/inputs/{n}/sends/{k}"
            ctl = "Bypass" if self._truthy(
                self._tree_value(f"{base}/Bypass/value")) else "Gain"
            push(f"{base}/{ctl}/value")
        for n, ctl, path in sorted(in_misc):
            push(path)
        if solo:
            self._handle_solo_recalc()
            count += 1

        # 4. outputs: not read back, so the saved value is the truth
        roles = self._output_roles()
        for n in sorted(out_ctl):
            base = f"/devices/0/outputs/{n}"
            ctls = out_ctl[n]
            role = roles.get(n)
            if role and role[0] == "cue":
                wanted = ("MixToMono", "MixInSource", "OutputDestination")
            elif role and role[0] == "hp":
                wanted = ("MixInSource",)
            elif role and role[0] == "monitor":
                wanted = ("MixInSource", "Pad", "MirrorsToDigital",
                          "CRPGABypass", "DigitalOutputMode")
            elif n in self._line_outputs():
                wanted = ("Pad", "AltMonTrim", "AltMonEnabled",
                          "SurroundStereoFoldDown", "StereoSubOutputEnabled")
            else:
                wanted = ("SurroundStereoFoldDown", "StereoSubOutputEnabled")
            for ctl in wanted:
                if ctl in ctls:
                    push(f"{base}/{ctl}/value")
        return count

    # ── Mute-until-settle (04 H5) ───────────────────────────────────

    def begin_settle_mute(self) -> bool:
        """Attenuate the outputs while a rate change settles.

        The Mixer Engine's CMxRoot::MuteUntilSettleDown sets
        kPropOutputsAttenuate (other param 0x20) to 1 on each mixer and its
        timer (CheckMuteUntilSettleDown) lifts it; UpdateHardwareState sends
        it only for mixer types that support OutputsAttenuate (the x4's
        MixerTypeFreud does, the base type does not).  The monitor mute bit
        is never touched, so clients see no Mute toggle.  Returns False,
        writing nothing, when the model has no OutputsAttenuate.  A second
        call while held just keeps the hold.
        """
        if self._settle is not None:
            return True
        if not self.supports_outputs_attenuate:
            return False
        user = 1 if self._truthy(self._tree_value(
            "/devices/0/OutputsAttenuate/value")) else 0
        self._settle = {"user": user}
        log.info("HW MONITOR: outputs attenuated while the new rate settles")
        self._set_other(MONITOR_PARAM["OutputsAttenuate"], 1,
                        what="OutputsAttenuate (settle)")
        return True

    def end_settle_mute(self) -> bool:
        """Lift the settle attenuation: back to the user's OutputsAttenuate
        (0 unless a client set it).  False if none was active."""
        st = self._settle
        if st is None:
            return False
        self._settle = None
        log.info("HW MONITOR: settle attenuation over → %d", st["user"])
        self._set_other(MONITOR_PARAM["OutputsAttenuate"], st["user"],
                        what="OutputsAttenuate (settle)")
        return True

    def cancel_settle_mute(self):
        """Forget a settle mute without writing (the device went away: the
        init path writes the whole state again)."""
        self._settle = None

    def flush_pending_gains(self):
        """Send any deferred gain values whose rate-limit window has passed."""
        now = time.monotonic()
        for ch in list(self._gain_pending):
            last = self._gain_last.get(ch)
            if last and now - last[1] >= 0.03:
                gain_db = self._gain_pending.pop(ch)
                self._gain_last[ch] = (gain_db, now)
                val_a = max(0, min(54, gain_db - 10))
                val_b = val_a
                val_c = max(0, min(56, gain_db - 9))
                log.debug("HW PREAMP GAIN (deferred): ch%d = %d dB", ch, gain_db)
                self.backend.set_mixer_param(PREAMP_CHANNEL_TYPE, ch, 0x0A, val_a)
                self.backend.set_mixer_param(PREAMP_CHANNEL_TYPE, ch, 0x09, val_b)
                self.backend.set_mixer_param(PREAMP_CHANNEL_TYPE, ch, 0x06, val_c)

    # ── Readback / metering ──────────────────────────────────────

    # True when the last poll_hw_readback() dropped a snapshot on its check
    # words (a valid one follows shortly), False otherwise
    readback_skipped = False

    def poll_hw_readback(self) -> dict | None:
        """Poll hardware readback and decode known fields.

        Called by the meter pump at ~50Hz. Returns a dict of decoded
        values, or None if readback is unavailable (--no-hardware or
        readback not ready) or the snapshot is one the Mixer Engine skips
        (word 38 or 39 all ones).

        Register map (hardware-verified on the x4 where noted; the rest
        from the Mixer Engine's decoders, apollo-linux parity/14 section 7):
            data[0]: preamp flags of preamps 1-4, 6 bits each:
                     +0 Mic/Line, +1 PAD, +2 Link, +3 48V, +4 LowCut,
                     +5 Phase (verified).  Bit 24/25: HiZ jack in input
                     1/2.  Bit 27: the front panel is on an output, bit 26
                     set = on the headphones (CUE 1), clear = on the monitor
            data[2]: [7:0] monitor level, [15:8] HP1 level, bit 16 mute,
                     bit 17 mono, bit 31 dim (verified); bit 30
                     LowNoisePreamps
            data[3]: GainC of preamps 1-4, 6 bits at shifts 0/6/16/22
                     (verified); [30:28] the preamp selected on the front
                     panel
            data[5], data[7]: the same as words 0 and 3 for preamps 5-8
                     (models with more than four); on the x4 data[7] [7:0]
                     is the HP2 level instead (verified)
            data[6]: bit 8 talkback, bit 31 clock locked when bit 30 is set
                     (verified); [29:28] alt-monitor selection
            data[8]-[25]: level meters the DSP writes (metering.py
                     X4_READBACK_METERS); returned raw in "words".
            data[17]: [15:8] temperature byte (reads 0 on the x4)
            data[33]: [28:26] analog board id
        """
        self.readback_skipped = False
        rb = self.backend.get_hw_readback()
        if rb is None:
            return None

        status, data = rb
        if status != 1:
            return None
        # The Mixer Engine drops the whole snapshot when either check word
        # is all ones (CMxMixer::TimerCallback)
        if len(data) < 40 or 0xFFFFFFFF in (data[38], data[39]):
            self.readback_skipped = True
            return None

        result = {}

        # Decode monitor section from data[2]
        d2 = data[2]
        mon_raw = d2 & 0xFF
        hp1_raw = (d2 >> 8) & 0xFF
        result["monitor_level_raw"] = mon_raw
        result["monitor_level_db"] = (mon_raw - 192) / 2.0
        result["monitor_level_tapered"] = monitor_hw_to_tapered(mon_raw)
        result["hp1_level_raw"] = hp1_raw
        result["hp1_level_tapered"] = monitor_hw_to_tapered(hp1_raw)
        result["monitor_mute"] = bool(d2 & (1 << 16))
        result["monitor_mono"] = bool(d2 & (1 << 17))
        result["monitor_dim"] = bool(d2 & (1 << 31))
        result["low_noise_preamps"] = bool(d2 & (1 << 30))

        # Decode HP2 volume from data[7]
        d7 = data[7]
        hp2_raw = d7 & 0xFF
        result["hp2_level_raw"] = hp2_raw
        result["hp2_level_tapered"] = monitor_hw_to_tapered(hp2_raw)

        # Decode talkback status from data[6]
        d6 = data[6]
        result["talkback_active"] = bool(d6 & (1 << 8))
        # Clock lock: bit 31, valid when bit 30 is set (kext)
        if d6 & (1 << 30):
            result["clock_locked"] = bool(d6 & (1 << 31))
        result["altmon_selection"] = (d6 >> 28) & 3

        # Preamp flags and gains, by preamp index (the input's position
        # among the preamps; FWSynchIn in the Mixer Engine).  Preamps 1-4
        # are in words 0 and 3, 5-8 in words 5 and 7; models with four or
        # fewer get keys for 0-3.
        n = max(4, min(MAX_PREAMP_CHANNELS, len(self.preamp_inputs())))
        for ch in range(n):
            flags = (data[0] if ch < 4 else data[5]) >> ((ch & 3) * 6)
            result[f"preamp_{ch}_micline"] = bool(flags & (1 << 0))
            result[f"preamp_{ch}_pad"] = bool(flags & (1 << 1))
            result[f"preamp_{ch}_link"] = bool(flags & (1 << 2))
            result[f"preamp_{ch}_48v"] = bool(flags & (1 << 3))
            result[f"preamp_{ch}_lowcut"] = bool(flags & (1 << 4))
            result[f"preamp_{ch}_phase"] = bool(flags & (1 << 5))

            # GainC: 6 bits at shift 0/6/16/22 (verified via a macOS DTrace
            # 1 dB sweep).  dB = GainC + 9; GainC 0 reads as 0 dB and
            # anything from 0x39 up as 65 dB, as the Mixer Engine does
            # (applyInputGainSettings)
            shift = (ch & 1) * 6 + (ch & 2) * 8
            gain_c = ((data[3] if ch < 4 else data[7]) >> shift) & 0x3F
            result[f"preamp_{ch}_gain_raw"] = gain_c
            result[f"preamp_{ch}_gain"] = (
                0 if gain_c == 0 else 65 if gain_c >= 0x39 else gain_c + 9)

        # HiZ jack detect: word 0 bit 24 = input 1, bit 25 = input 2
        d0 = data[0]
        result["preamp_0_hiz"] = bool(d0 & (1 << 24))
        result["preamp_1_hiz"] = bool(d0 & (1 << 25))

        # Front panel: on an output (bit 27; bit 26 = the headphones, else
        # the monitor) or on the preamp in word 3 [30:28]
        result["hp_focus_valid"] = bool(d0 & (1 << 27))
        result["hp_focus_headphone"] = bool(d0 & (1 << 26))
        result["front_panel_input"] = (data[3] >> 28) & 7

        # Temperature byte (kPropTemperature; the Mixer Engine passes the
        # raw byte on as a float) and analog board id (kPropAnalogBoardID)
        result["temperature_raw"] = (data[17] >> 8) & 0xFF
        result["analog_board_id"] = (data[33] >> 26) & 7

        # The raw words, for the readback level meters (words 8-25, see
        # metering.ReadbackMeter); the Mixer Engine reads its meters from
        # there, the old "static 0xf0000000" captures predate an active DSP
        result["words"] = list(data)

        # Log at debug level
        log.debug("HW readback: status=%d data[0:8]=%s",
                  status, [f"0x{d:08x}" for d in data[:8]])

        return result

    # ── Value encoding helpers ────────────────────────────────────

    @staticmethod
    def encode_mixer_value(value: Any, encoding: str = "raw") -> int:
        """Encode a control value to a mixer register uint32."""
        if encoding == "bool":
            return 1 if value else 0
        elif encoding == "float_to_fixed":
            return float_to_fixed16(float(value))
        elif encoding == "db":
            return encode_gain_value(float(value))
        else:
            try:
                return int(value) & 0xFFFFFFFF
            except (ValueError, TypeError):
                return 0
