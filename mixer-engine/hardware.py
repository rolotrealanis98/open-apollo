"""
Hardware register I/O via /dev/ua_apollo0 ioctls.

Provides a Python ctypes wrapper around the Linux driver's ioctl interface
for reading/writing BAR0 registers. Implements:

1. Mixer sequence handshake — 38-setting DSP register protocol
2. Preamp controls via DSP mixer settings (NOT CLI registers)
3. Bus parameter writes — fader levels, pan, send gains
4. Value encoding helpers — float↔fixed, dB↔linear, bool↔int

Register map (from hardware driver analysis):
    Mixer registers live at BAR0 + 0x3800 (NOT BAR0 + 0x0000).
    The hardware driver initializes: mixerBase = barBase + 0x3800

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

import fcntl
import glob
import logging
import math
import os
import re
import struct
import time
from typing import Any

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
# Maps to hardware driver SetMixerParam command (24 bytes).
# On Linux, the driver handles routing via DSP mixer settings:
#   ch_type=1 (preamp) → mixer setting[param_id + 7]
#   ch_type=2 (monitor) → mixer setting (TBD)
_MIXER_PARAM_SIZE = 16
UA_IOCTL_SET_MIXER_PARAM = _IOW(UA_IOCTL_MAGIC, 0x31, _MIXER_PARAM_SIZE)

# struct ua_mixer_bus_param { bus_id, sub_param, value_u32, flags } = 16 bytes
# Maps to hardware driver SetMixerBusParam command (136 bytes).
# value_u32 is IEEE 754 float packed as uint32 (linear gain).
_MIXER_BUS_PARAM_SIZE = 16
UA_IOCTL_SET_MIXER_BUS_PARAM = _IOW(UA_IOCTL_MAGIC, 0x30, _MIXER_BUS_PARAM_SIZE)

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

_FW_LOAD_SIZE = 16
UA_IOCTL_LOAD_FIRMWARE = _IOW(UA_IOCTL_MAGIC, 0x50, _FW_LOAD_SIZE)
UA_IOCTL_DSP_CONNECT = _IO(UA_IOCTL_MAGIC, 0x51)

# ── Register offsets ────────────────────────────────────────────────
# Mixer registers are at BAR0 + 0x3800, discovered from hardware driver analysis
# of the hardware driver initialization (barBase + 0x3800)

MIXER_BAR_OFFSET = 0x3800
REG_MIXER_SEQ_WR = MIXER_BAR_OFFSET + 0x08   # 0x3808
REG_MIXER_SEQ_RD = MIXER_BAR_OFFSET + 0x0C   # 0x380C
MIXER_SETTING_COUNT = 38

# ── CLI Register Interface (ARM microcontroller communication) ──────
# From ua_apollo.h and hardware driver analysis. The CLI provides a command/response
# interface to the ARM MCU that controls preamps, phantom power, etc.

REG_CLI_ENABLE   = 0xC3F4  # Write 1 to enable CLI
REG_CLI_STATUS   = 0xC3F8  # Command status / command length
REG_CLI_RESP_LEN = 0xC3FC  # Response data length (bytes)
REG_CLI_CMD_BUF  = 0xC400  # Command buffer (128 bytes, 32 uint32)
REG_CLI_RESP_BUF = 0xC480  # Response buffer (128 bytes, 32 uint32)
CLI_CMD_BUF_SIZE = 128
CLI_RESP_BUF_SIZE = 128

# ── ARM Parameter IDs ──────────────────────────────────────────────
# From hardware driver analysis of the input parameter handler, confirmed
# by hardware observation.
#
# The 36-byte SetInputParam struct layout (from hardware observation):
#   word[0]:  paramID  (0x00-0x17)
#   word[1-4]: reserved (0)
#   word[5]:  hwID     (channel hardware ID)
#   word[6-7]: reserved (0)
#   word[8]:  0x08     (data size / flags)
#
# ParamIDs route to either ARM MCU (preamp controls) or DSP.
# _isDSPInputParam() returns true for paramIDs 0x05, 0x13, 0x16.

# Preamp parameter IDs — verified via hardware observation.
# All preamp params go through DSP mixer settings (SetMixerParam ch_type=1),
# NOT through ARM CLI.  The hardware driver routes them to mixer setting[param_id + 7].
# HiZ has NO software control — hardware auto-detect only (rb_data[0] bit 24).
PREAMP_PARAM_IDS = {
    "MicLine":  0x00,   # 1=Line, 0=Mic
    "Pad":      0x01,   # on=1, off=0
    "48V":      0x03,   # on=1, off=0 (triggers ARM safety blink)
    "LowCut":   0x04,   # on=1, off=0 (NOT HiZ — verified via hardware observation)
    "Phase":    0x05,   # invert=0xbf800000 (-1.0f), normal=0x3f800000 (+1.0f)
    "GainA":    0x0A,   # Gain_A (1st write, dB-10) — verified via hardware observation
    "GainB":    0x09,   # Gain_B (2nd write, dB-10) — verified via hardware observation
    "GainC":    0x06,   # Gain_C (periodic 3rd write, dB-9) — verified via hardware observation
    "Route":    0x13,   # Channel routing mask (0x0001ffff)
    "Level":    0x16,   # Default level (0xa0 = -16dB)
}

# Legacy alias for existing code that uses ARM_PARAM_IDS
ARM_PARAM_IDS = PREAMP_PARAM_IDS

# ── Hardware IDs for Apollo x4 analog inputs ────────────────────────
# From hardware observation: each analog input has a unique hwID
# with stride 0x0800. These IDs are passed in the SetInputParam struct
# at word[5] to identify which preamp channel to control.

APOLLO_X4_HW_IDS = {
    0: 0x0000,  # Analog Input 1 (Mic/Line, HiZ available)
    1: 0x0800,  # Analog Input 2 (Mic/Line, HiZ available)
    2: 0x1000,  # Analog Input 3 (Line only)
    3: 0x1800,  # Analog Input 4 (Line only)
}
# Inputs 4+ are S/PDIF, ADAT, Virtual — no preamps, no hwID needed.
HW_ID_STRIDE = 0x0800
MAX_PREAMP_CHANNELS = 4  # Apollo x4; other models vary

# ── Bus ID Mapping (from hardware observation, Apollo x4) ──
# Bus IDs identify mixer bus channels for SetMixerBusParam (SEL130).
# VERIFIED: setting_index == bus_id for ALL 32 buses (0x00-0x1f).
# See hardware observation notes for full capture analysis.
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

# State tree input index → hardware bus ID mapping.
# The state tree uses contiguous indices (0-21 for 22 inputs on Apollo x4),
# but the hardware bus IDs have gaps (CUE/TB/AUX slots interleaved).
_INPUT_IDX_TO_BUS = (
    0x00, 0x01, 0x02, 0x03,  # Analog 1-4
    0x08, 0x09,               # S/PDIF L/R
    0x0C, 0x0D, 0x0E, 0x0F,  # ADAT 1-4
    0x14, 0x15, 0x16, 0x17,  # ADAT 5-8
    0x18, 0x19, 0x1A, 0x1B,  # Virtual 1-4
    0x1C, 0x1D, 0x1E, 0x1F,  # Virtual 5-8
    0x0A, 0x0B,               # Talkback 1/2 (inputs 22-23)
)

# Talkback bus constants (verified via hardware observation)
# Talkback faders use sub=0 ONLY (not the triplet sub=0,3,4).
# Unity gain ≈ 3.976353 (NOT 0.707 like normal inputs).
TALKBACK_BUS_START = 0x0A
TALKBACK_UNITY = 3.976353


def input_bus_id(input_idx: int) -> int:
    """Get bus ID for an input channel (state tree index → hardware bus)."""
    if 0 <= input_idx < len(_INPUT_IDX_TO_BUS):
        return _INPUT_IDX_TO_BUS[input_idx]
    return input_idx  # fallback

# Aux bus IDs: stride of 2 per aux index
def aux_bus_id(aux_idx: int) -> int:
    """Get bus ID for an aux bus. Verified: aux 0=0x0010, aux 1=0x0012."""
    return 0x0010 + aux_idx * 2

# ── SEL130 Sub-Parameter Indices ──────────────────────────────────
# From hardware observation (verified all sends + fader + pan):
#   Each input bus has 5 coefficients:
#     sub=0: main mix coefficient (fader trigger + pan)
#     sub=1: CUE1 send level
#     sub=2: CUE2 send level
#     sub=3: gain L (fader level / AUX1 send)
#     sub=4: gain R (fader level / AUX2 send)
#   Fader writes triplet: sub={0, 3, 4}
#   Pan writes single: sub=0
#   CUE sends write single: sub=1 or sub=2
#   AUX sends write single: sub=3 or sub=4
SUB_PARAM_MIX = 0           # Main mix coefficient (fader + pan)
SUB_PARAM_CUE1 = 1          # CUE 1 send level
SUB_PARAM_CUE2 = 2          # CUE 2 send level
SUB_PARAM_GAIN_L = 3        # Gain L (fader / AUX 1 send)
SUB_PARAM_GAIN_R = 4        # Gain R (fader / AUX 2 send)

# State tree send index → SEL130 sub-param mapping (verified via hardware observation)
# sends/0="AUX 1"→sub=3, sends/1="AUX 2"→sub=4,
# sends/2="CUE 1"→sub=1, sends/3="CUE 2"→sub=2
_SEND_IDX_TO_SUB = {
    0: SUB_PARAM_GAIN_L,  # AUX 1
    1: SUB_PARAM_GAIN_R,  # AUX 2
    2: SUB_PARAM_CUE1,    # CUE 1
    3: SUB_PARAM_CUE2,    # CUE 2
}

# ── Monitor Parameter IDs (SEL131, channel_type=2) ───────────────
# From hardware observation of SetMixerParam with w2=2 (monitor type).
MONITOR_PARAM = {
    "CRMonitorLevel":  0x01,   # w3=1, value = 192 + (dB × 2)
    "MonitorMute":     0x03,   # w3=0, value = 2 (muted) / 0 (unmuted)
    "MixToMono":       0x03,   # w3=0, value = 1/0 (VERIFIED: same param as MonitorMute!)
    "DimOn":           0x44,   # w3=0, value = 1/0
    # Pad and MirrorsToDigital — VERIFIED via hardware observation:
    # "Pad" on outputs/18 = OutputRef (param 0x32, ch_idx=1): 1=-10dBV, 0=+4dBu
    # "MirrorsToDigital" = DigitalMirror (param 0x1e, ch_idx=9): 1=on, 0=off
    "Pad":             0x32,   # VERIFIED: OutputRef +4dBu/-10dBV, ch_idx=1
    "MirrorsToDigital": 0x1e,  # VERIFIED: DigitalMirror, ch_idx=9
    # Cache-only controls — route through SET_MIXER_PARAM for ALSA cache sync:
    "MixInSource":     0x04,   # ch_idx=1: 0=MIX, 1=CUE1, 2=CUE2 (MonitorSrc)
    "HP1Source":       0x3f,   # ch_idx=1: 0=CUE1, 1=CUE2 (HP1 routing)
    "HP2Source":       0x40,   # ch_idx=1: 0=CUE1, 1=CUE2 (HP2 routing)
    "DimLevel":        0x43,   # ch_idx=0: 1-7 (dim attenuation step)
    "TalkbackOn":      0x46,   # ch_idx=1 ON / ch_idx=9 OFF (verified via hardware observation)
    "DigitalOutputMode": 0x21, # ch_idx=0: 0=SPDIF, 8=ADAT
    "SRConvert":       0x1f,   # ch_idx=1: sample rate converter on/off (S/PDIF inputs)
    "DSPSpanning":     0x16,   # ch_idx=0: DSP pairing/spanning mode
    # CUE mirror / output routing — all ch_idx=9 unless noted
    "MirrorA":         0x2e,   # ch_idx=9: CUE1 mirror source (see MIRROR_SOURCE_MAP)
    "MirrorB":         0x2f,   # ch_idx=9: CUE2 mirror source (see MIRROR_SOURCE_MAP)
    "MirrorEnableA":   0x3b,   # ch_idx=9: CUE1 mirror enable (1/0)
    "MirrorEnableB":   0x3c,   # ch_idx=9: CUE2 mirror enable (1/0)
    # CUE mono/mix — all ch_idx=0
    "CUE1Mono":        0x06,   # ch_idx=0: CUE1 mono (1/0)
    "CUE2Mono":        0x08,   # ch_idx=0: CUE2 mono (1/0)
    "CUE1Mix":         0x05,   # ch_idx=0: CUE1 mix (0=on, 2=off — INVERTED!)
    "CUE2Mix":         0x07,   # ch_idx=0: CUE2 mix (0=on, 2=off — INVERTED!)
    # Device identification
    "Identify":        0x1d,   # ch_idx=0: flash front panel LEDs (1=on, 0=off)
    # TalkbackConfig
    "TBConfig":        0x47,   # ch_idx=0: talkback configuration
}
MONITOR_CHANNEL_TYPE = 2
PREAMP_CHANNEL_TYPE = 1

# Headphone output bus ID (from state tree: /devices/0/outputs/19/)
# Hardware observation confirmed: HP volume on Apollo x4 uses
# ch_type=2 ch_idx=1 param=0x0001 — SAME path as main monitor.
# HP and monitor levels appear to be ganged at the hardware driver level.
HP_BUS_ID = 0x0019
HP_CHANNEL_IDX = 1  # confirmed: same as monitor on Apollo x4

# ── Driver Parameter IDs ─────────────────────────────────────────
DRIVER_PARAM_SAMPLE_RATE = 0
DRIVER_PARAM_CLOCK_SOURCE = 1
DRIVER_PARAM_TRANSPORT_RUNNING = 2  # Read-only

CLOCK_SOURCE_MAP = {
    "Internal": 0,
    "S/PDIF": 1,
    "ADAT": 2,
    "WordClock": 3,
}
VALID_SAMPLE_RATES = {44100, 48000, 88200, 96000, 176400, 192000}

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

# ── MixInSource value encoding (verified via hardware observation) ──────
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

    From hardware driver analysis, there are 3 ranges
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

    From the hardware driver write-setting function:
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

    Formula from hardware observation: value = 192 + (dB × 2)
    Verified: -10dB→172, -15dB→162, -30dB→132, -50dB→92
    """
    return max(0, min(384, int(round(192 + db * 2))))


_INV_SQRT2 = 0.7071067811865476  # 1/sqrt(2), exact -3.01 dB


def encode_input_fader(db: float) -> float:
    """Encode input fader dB to linear float (with 1/sqrt(2) scaling).

    Formula: linear = 10^(dB/20) / sqrt(2)
    This gives 0 dB fader → 1/sqrt(2) ≈ 0.707107 (-3.01 dB to DSP),
    providing headroom in the summing bus.

    Verified against hardware observation:
        0dB  → 0.707107 (0x3F3504F3)
       -10dB → 0.223607 (0x3E64F92E)
       -40dB → 0.007071 (0x3BE7B46A)
    """
    if db <= -144.0:
        return 0.0
    return 10.0 ** (db / 20.0) * _INV_SQRT2


def encode_mix_coeff(fader_db: float, pan: float) -> float:
    """Compute the sub=0 mix coefficient combining fader + pan.

    Verified formula:
        mix_coeff = 10^(fader_dB/20) × cos((pan + 1) × π/4)
    This is encode_input_fader(dB) × √2 × cos((pan+1)×π/4).

    At center pan (0.0): mix_coeff = encode_input_fader(dB) × 1.0
    At hard left (-1.0): mix_coeff = encode_input_fader(dB) × √2
    At hard right (+1.0): mix_coeff ≈ 0.0
    """
    if fader_db <= -144.0:
        return 0.0
    return 10.0 ** (fader_db / 20.0) * math.cos((pan + 1.0) * math.pi / 4.0)


def encode_aux_fader(db: float) -> float:
    """Encode aux fader dB to linear float (NO offset).

    Formula from hardware observation: linear = 10^(dB / 20)
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


def monitor_tapered_to_hw(tapered: float) -> int:
    """Convert monitor tapered 0.0-1.0 to hardware value 0-192.

    Uses a 361-entry lookup table captured from the real UA Mixer Engine.
    For arbitrary float inputs, interpolates between the two nearest
    table entries (1/360 step resolution).
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
    hw_lo = _MONITOR_TAPER_FWD[idx]
    hw_hi = _MONITOR_TAPER_FWD[idx + 1]
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
    return _MONITOR_TAPER_REV[hw_raw]


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
        self._cli_enabled = False
        self.safe_mode = safe_mode  # Block unmapped DSP writes (bus params, monitor)
        self._last_mixer_write = 0.0  # Throttle: time of last DSP setting write
        self._last_cli_write = 0.0   # Throttle: time of last ARM CLI command
        self._cli_frozen = False     # Set True on first CLI timeout — stops all CLI

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

        Protocol (reverse-engineered from the hardware driver's CLI enable/send):
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
        The command format is derived from the device request protocol.

        From hardware observation, the SetInputParam struct is 36 bytes:
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
        """Set a mixer parameter — equivalent to hardware driver SetMixerParam.

        The hardware driver routes this based on channel_type:
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
        try:
            fcntl.ioctl(self.fd, UA_IOCTL_SET_MIXER_PARAM, buf)
            self._last_mixer_write = time.monotonic()
            log.debug("MIXER_PARAM: ch_type=%d ch_idx=%d param=0x%02x "
                      "value=%d", channel_type, channel_idx, param_id, value)
            return True
        except OSError as e:
            self._last_mixer_write = time.monotonic()
            log.error("set_mixer_param(type=%d, idx=%d, param=0x%02x, "
                      "val=%d): %s", channel_type, channel_idx,
                      param_id, value, e)
            return False

    # ── SetMixerBusParam (SEL130 equivalent) ──────────────────────────

    def set_mixer_bus_param(self, bus_id: int, sub_param: int,
                            value_float: float) -> bool:
        """Set a mixer bus parameter — equivalent to hardware driver SetMixerBusParam.

        Sends fader level, pan, and send gain values. Values are IEEE 754
        float (linear gain), packed as uint32 for the ioctl.

        Each fader change sends 3 writes:
            sub_param=0: main value
            sub_param=3: companion L
            sub_param=4: companion R
        Pan sends only sub_param=0.

        Struct: {bus_id, sub_param, value_u32, flags=0x02} = 16 bytes.

        Args:
            bus_id:      Bus identifier (see INPUT_BUS_IDS, aux_bus_id())
            sub_param:   0=main, 3=companion_L, 4=companion_R
            value_float: Linear gain as float (0.0 = silence, 1.0 = unity)
        """
        value_u32 = float_to_uint32(value_float)

        # All 32 buses (0x00-0x1f) use direct mapping: setting_index = bus_id.
        # Confirmed via hardware probing on Apollo x4.
        if self.safe_mode and bus_id > 0x1f:
            log.debug("BUS_PARAM BLOCKED (safe_mode): bus=0x%04x sub=%d "
                      "value=%.6f", bus_id, sub_param, value_float)
            return True  # Pretend success so state tree still updates

        if self.fd < 0:
            log.info("HW BUS_PARAM: bus=0x%04x sub=%d value=%.6f "
                     "(0x%08x) (no device)", bus_id, sub_param,
                     value_float, value_u32)
            return False

        buf = struct.pack('<IIII', bus_id, sub_param, value_u32, 0x02)
        try:
            fcntl.ioctl(self.fd, UA_IOCTL_SET_MIXER_BUS_PARAM, buf)
            log.debug("BUS_PARAM: bus=0x%04x sub=%d value=%.6f (0x%08x)",
                      bus_id, sub_param, value_float, value_u32)
            return True
        except OSError as e:
            log.error("set_mixer_bus_param(bus=0x%04x, sub=%d, val=%.6f): %s",
                      bus_id, sub_param, value_float, e)
            return False

    def set_bus_fader(self, bus_id: int, linear_gain: float,
                     mix_coeff: float = 0.0) -> bool:
        """Send a complete fader change (3 sub-param writes).

        From hardware observation: each fader change sends 3 writes:
            sub=0: combined fader+pan mix coefficient
            sub=3: effective gain, left channel
            sub=4: effective gain, right channel
        mix_coeff = fader_linear × √2 × cos((pan + 1) × π/4)
        At center pan, mix_coeff == linear_gain (the √2 and cos(π/4) cancel).
        """
        write_fn = self.set_bus_param_direct if USE_DIRECT_MIXER_WRITES \
            else self.set_mixer_bus_param
        ok = write_fn(bus_id, SUB_PARAM_MIX, mix_coeff)
        ok &= write_fn(bus_id, SUB_PARAM_GAIN_L, linear_gain)
        ok &= write_fn(bus_id, SUB_PARAM_GAIN_R, linear_gain)
        return ok

    def set_bus_pan(self, bus_id: int, mix_coeff: float) -> bool:
        """Send a pan change (single write to sub=0).

        From hardware observation: pan writes only sub=0.
        mix_coeff = fader_linear × √2 × cos((pan + 1) × π/4)
        """
        write_fn = self.set_bus_param_direct if USE_DIRECT_MIXER_WRITES \
            else self.set_mixer_bus_param
        return write_fn(bus_id, SUB_PARAM_MIX, mix_coeff)

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

        The hardware driver reads these at ~33Hz for metering and status.
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
            log.error("get_hw_readback: %s", e)
            return None

    def get_dsp_info(self, dsp_index: int) -> dict | None:
        """Query DSP info for a single SHARC DSP.

        Returns dict with 'index', 'bank_base', 'status' (0=idle, 1=running),
        or None on error. Maps to hardware driver GetDSPInfo / GetPluginDSPLoad commands
        (GetPluginDSPLoad).

        The Apollo x4 has 4 DSPs (index 0-3).
        """
        if self.fd < 0:
            return None

        # struct ua_dsp_info { index, bank_base, status, reserved[5] } = 32 bytes
        buf = struct.pack('<III20x', dsp_index, 0, 0)
        try:
            result = fcntl.ioctl(self.fd, UA_IOCTL_GET_DSP_INFO, buf)
            idx, bank_base, status = struct.unpack_from('<III', result)
            return {"index": idx, "bank_base": bank_base, "status": status}
        except OSError as e:
            log.debug("get_dsp_info(dsp=%d): %s", dsp_index, e)
            return None

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

    # ── Driver parameter access ──────────────────────────────────────

    def set_driver_param(self, param_id: int, value: int) -> bool:
        """Set a driver parameter via UA_IOCTL_SET_DRIVER_PARAM.

        Struct: {param_id: u32, reserved: u32, value: u64} = 16 bytes.
        Used for sample rate (param 0) and clock source (param 1).
        """
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
                             value_float: float) -> bool:
        """Write a bus param directly to the correct mixer setting.

        Looks up (bus_id, sub_param) in BUS_SETTING_MAP. If found,
        writes to that mixer setting index via mixer_write_setting(),
        bypassing the driver's SET_MIXER_BUS_PARAM ioctl. Falls back
        to set_mixer_bus_param() if the mapping is unknown.
        """
        key = (bus_id, sub_param)
        setting_idx = BUS_SETTING_MAP.get(key)

        if setting_idx is None:
            log.debug("BUS_PARAM_DIRECT: no mapping for bus=0x%04x sub=%d, "
                      "falling back to ioctl", bus_id, sub_param)
            return self.set_mixer_bus_param(bus_id, sub_param, value_float)

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
# Hardware paths (from hardware driver analysis):
#
#   1. MIXER SETTINGS (38 registers at BAR0+0x3800+offset)
#      - DSP-processed: fader levels, mute, solo, pan, bus sends
#      - Written via mixer sequence handshake protocol
#      - Setting index ↔ control mapping must be determined empirically
#
#   2. ARM PARAMETERS (via CLI registers → ARM MCU)
#      - ARM microcontroller: preamp gain, 48V, pad, HiZ, lowcut, phase
#      - Verified hwIDs: 0x0000, 0x0800, 0x1000, 0x1800 (4 channels)
#      - Verified paramIDs: 0x00-0x0B, 0x17 (from hardware observation)
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
    r'IOType|RecordPreEffects|Isolate|Stereo|Active|SRConvert)/value$')

# Preamp controls: /devices/0/inputs/<ch>/preamps/0/<control>/value
_RE_PREAMP = re.compile(
    r'^/devices/0/inputs/(\d+)/preamps/0/(Gain|GainTapered|48V|Pad|HiZ|LowCut|Phase|Mute)/value$')

# Bus send controls: /devices/0/inputs/<ch>/sends/<send>/<control>/value
_RE_SEND = re.compile(
    r'^/devices/0/inputs/(\d+)/sends/(\d+)/(Gain|GainTapered|Pan|Bypass)/value$')

# Output/Monitor controls: /devices/0/outputs/<ch>/<control>/value
_RE_OUTPUT = re.compile(
    r'^/devices/0/outputs/(\d+)/(Mute|DimOn|Pad|MixToMono|MirrorsToDigital|'
    r'CRMonitorLevel|CRMonitorLevelTapered|SurroundMute|SurroundSolo|'
    r'AltMonEnabled|AltMonTrim|MixInSource|DimLevel|'
    r'DigitalOutputMode|OutputDestination|Identify)/value$')

# Aux bus controls: /devices/0/auxs/<bus>/<control>/value
_RE_AUX = re.compile(
    r'^/devices/0/auxs/(\d+)/(FaderLevel|FaderLevelTapered|Mute|MixToMono|'
    r'SendPostFader)/value$')

# Aux bus send controls: /devices/0/auxs/<bus>/sends/<send>/<control>/value
_RE_AUX_SEND = re.compile(
    r'^/devices/0/auxs/(\d+)/sends/(\d+)/(Gain|GainTapered|Pan|Bypass)/value$')

# Global device controls (directly under /devices/0/)
_RE_DEVICE_GLOBAL = re.compile(
    r'^/devices/0/(TalkbackOn|DimAttenuation|TOSLinkOutput|DSPSpanning|Identify)/value$')

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

    def __init__(self, backend: HardwareBackend, state_tree=None):
        self.backend = backend
        self.state_tree = state_tree
        # Rate-limit gain writes to prevent DSP overload.
        # Key: channel, Value: (last_db, last_time)
        self._gain_last: dict[int, tuple[int, float]] = {}
        self._sync_driver_state()

    def _sync_driver_state(self):
        """Read current rate/clock from driver and update state tree."""
        if not self.state_tree or not self.backend.connected:
            return

        rate = self.backend.get_driver_param(DRIVER_PARAM_SAMPLE_RATE)
        if rate:
            self.state_tree.set("/SampleRate/value", rate)
            log.info("Synced SampleRate from driver: %d Hz", rate)

        clock = self.backend.get_driver_param(DRIVER_PARAM_CLOCK_SOURCE)
        if clock is not None:
            for name, val in CLOCK_SOURCE_MAP.items():
                if val == clock:
                    self.state_tree.set("/ClockSource/value", name)
                    log.info("Synced ClockSource from driver: %s", name)
                    break

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

    def on_set(self, path: str, value: Any):
        """Called by state_tree when a value changes. Route to hardware."""
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
            control = m.group(1)
            self._handle_device_global(path, control, value)
            return

        m = _RE_GLOBAL.match(path)
        if m:
            control = m.group(1)
            self._handle_global(path, control, value)
            return

        # Unmatched path — software only, no hardware write needed
        log.debug("SET %s = %r (software-only)", path, value)

    # ── Solo recalculation ─────────────────────────────────────────

    def _handle_solo_recalc(self):
        """Recalculate all bus gains based on solo state.

        When any channel is soloed, only soloed channels are audible —
        all non-soloed channels get their effective gain set to 0.0.
        When no channels are soloed, all channels get their real fader gain.
        Muted channels always get 0.0 regardless of solo state.
        """
        if not self.state_tree:
            return

        any_solo = False
        channels = []
        for i in range(24):  # Apollo x4 has 24 input channels
            solo = self.state_tree.get_value(
                f"/devices/0/inputs/{i}/Solo/value")
            fader = self.state_tree.get_value(
                f"/devices/0/inputs/{i}/FaderLevel/value")
            mute = self.state_tree.get_value(
                f"/devices/0/inputs/{i}/Mute/value")
            channels.append((i, bool(solo), float(fader or 0), bool(mute)))
            if solo:
                any_solo = True

        for i, soloed, fader_db, muted in channels:
            bus = input_bus_id(i)
            if muted or (any_solo and not soloed):
                self.backend.set_bus_fader(bus, 0.0)
            else:
                linear = encode_input_fader(fader_db)
                self.backend.set_bus_fader(bus, linear)

        log.info("Solo recalc: any_solo=%s, %d channels updated",
                 any_solo, len(channels))

    # ── Preamp controls (ARM path) ────────────────────────────────

    def _handle_preamp(self, path: str, ch: int, control: str, value: Any):
        """Route preamp controls through DSP mixer settings.

        From hardware driver analysis of the preamp parameter handler:
        Preamp parameters are written to DSP mixer settings (NOT CLI).
        Each setting packs 4 channels as 8-bit values in a 32-bit word.
        The kernel driver computes: setting_index = param_id + 7,
        and packs value into the channel's byte position.

        paramID mapping (from hardware driver input parameter dispatch):
            Mute=0x00  Phase=0x01  48V=0x04  Pad=0x08
            HiZ=0x09   LowCut=0x0A  Gain=0x17
        """
        if ch >= MAX_PREAMP_CHANNELS:
            log.debug("SET %s = %r (no preamp for input %d)", path, value, ch)
            return

        # Gain: hardware driver sends triplet {0x0A, 0x09, 0x06} via DSP settings.
        # Verified: GainA(0x0A)=dB-10, GainB(0x09)=dB-10, GainC(0x06)=dB-9.
        # Rate-limited to prevent DSP overload from rapid iPad slider drags.
        if control in ("Gain", "GainTapered"):
            if control == "GainTapered":
                gain_db = preamp_tapered_to_db(float(value))
            else:
                gain_db = float(value)
            gain_db = int(round(gain_db))

            # Rate limit: skip if same dB value or < 50ms since last write
            now = time.monotonic()
            last = self._gain_last.get(ch)
            if last:
                last_db, last_t = last
                if gain_db == last_db:
                    return  # duplicate, skip entirely
                if now - last_t < 0.05:
                    return  # too fast, skip (iPad sends at 30-60Hz)
            self._gain_last[ch] = (gain_db, now)

            val_a = max(0, min(54, gain_db - 10))   # 0x0A: dB-10, max 54
            val_b = val_a                             # 0x09: same as A
            val_c = max(0, min(56, gain_db - 9))     # 0x06: dB-9, max 56

            log.info("HW PREAMP GAIN: input[%d] = %d dB → "
                     "GainA=0x0A:%d GainB=0x09:%d GainC=0x06:%d",
                     ch, gain_db, val_a, val_b, val_c)

            # Send all three as SetMixerParam — matches hardware driver behavior
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

    # ── Input channel controls (SEL130 bus param path) ───────────

    def _handle_input(self, path: str, ch: int, control: str, value: Any):
        """Route input channel controls via SetMixerBusParam (SEL130).

        From hardware observation:
        - FaderLevel → linear float with -3dB offset, 3 sub-param writes
        - Mute → fader to 0.0 (linear silence), 3 sub-param writes
        - Pan → float coefficient, single sub_param=0 write
        - Solo → recalculates ALL bus gains (batch write)

        Bus IDs: analog 0-3 = 0x0000-0x0003,
                 digital 4+ = 0x000C + (idx-4)
        """
        bus = input_bus_id(ch)

        if control in ("FaderLevel", "FaderLevelTapered"):
            if control == "FaderLevelTapered":
                db = fader_tapered_to_db(float(value))
            else:
                db = float(value)

            # Talkback bus uses different encoding (verified via hardware observation):
            # sub=0 only, unity≈3.976, sub=3/4 always 0.0
            if bus == TALKBACK_BUS_START:
                linear = 0.0 if db <= -144.0 else 10.0 ** (db / 20.0) * TALKBACK_UNITY
                log.info("HW BUS: talkback.Fader = %.1f dB → %.6f "
                         "(bus=0x%04x, sub=0 only)", db, linear, bus)
                write_fn = self.backend.set_bus_param_direct if USE_DIRECT_MIXER_WRITES \
                    else self.backend.set_mixer_bus_param
                write_fn(bus, SUB_PARAM_MIX, linear)
                return

            linear = encode_input_fader(db)
            # Read current pan to compute combined sub=0 coefficient
            pan = self._get_input_pan(ch)
            mix = encode_mix_coeff(db, pan)
            log.info("HW BUS: input[%d].Fader = %.1f dB → linear=%.6f "
                     "mix=%.6f pan=%.2f (bus=0x%04x)",
                     ch, db, linear, mix, pan, bus)
            self.backend.set_bus_fader(bus, linear, mix)
            return

        if control == "Mute":
            if value and value != "0" and value != "false":
                log.info("HW BUS: input[%d].Mute ON (fader→0.0, bus=0x%04x)",
                         ch, bus)
                self.backend.set_bus_fader(bus, 0.0, 0.0)
            else:
                # Mute OFF → recalc in case solo is active
                log.info("HW BUS: input[%d].Mute OFF (bus=0x%04x)", ch, bus)
                self._handle_solo_recalc()
            return

        if control in ("Pan", "Pan2"):
            pan_f = float(value)
            # Read current fader level to compute combined sub=0 coefficient
            fader_db = self._get_input_fader_db(ch)
            mix = encode_mix_coeff(fader_db, pan_f)
            log.info("HW BUS: input[%d].Pan = %.3f → mix=%.6f fader=%.1fdB "
                     "(bus=0x%04x)", ch, pan_f, mix, fader_db, bus)
            self.backend.set_bus_pan(bus, mix)
            return

        if control == "Solo":
            log.info("HW BUS: input[%d].Solo = %r → recalculating all bus gains",
                     ch, value)
            self._handle_solo_recalc()
            return

        if control == "IOType":
            # Mic/Line switching: verified via hardware observation, maps to
            # SEL131 ch_type=1 param=0x00, value 1=Line 0=Mic
            if ch >= MAX_PREAMP_CHANNELS:
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

        # Software-only controls (RecordPreEffects, Isolate, Stereo, Active)
        log.debug("SET %s = %r (software-only)", path, value)

    # ── Bus send controls (SEL130 bus param path) ──────────────────

    def _handle_send(self, path: str, ch: int, send_idx: int,
                     control: str, value: Any):
        """Route bus send controls via SetMixerBusParam (SEL130).

        Verified via hardware observation: sends write to the INPUT bus
        with different sub-params:
            sends/0 (AUX 1) → sub=3 (GAIN_L) on input bus
            sends/1 (AUX 2) → sub=4 (GAIN_R) on input bus
            sends/2 (CUE 1) → sub=1 (CUE1)  on input bus
            sends/3 (CUE 2) → sub=2 (CUE2)  on input bus
        CUE/AUX sends use the same -3dB offset encoding as faders.
        """
        sub = _SEND_IDX_TO_SUB.get(send_idx)
        if sub is None:
            log.debug("SET %s = %r (send_idx=%d has no sub-param mapping)",
                      path, value, send_idx)
            return

        bus = input_bus_id(ch)

        if control in ("Gain", "GainTapered"):
            if control == "GainTapered":
                db = fader_tapered_to_db(float(value))
            else:
                db = float(value)
            linear = encode_input_fader(db)
            log.info("HW BUS: input[%d].send[%d].Gain = %.1f dB → "
                     "linear=%.6f (bus=0x%04x sub=%d)",
                     ch, send_idx, db, linear, bus, sub)
            self.backend.set_mixer_bus_param(bus, sub, linear)
            return

        if control == "Pan":
            # Send pan — only meaningful for AUX sends (sub=3/4)
            log.info("HW BUS: input[%d].send[%d].Pan = %r (bus=0x%04x)",
                     ch, send_idx, value, bus)
            return

        if control == "Bypass":
            log.info("HW BUS: input[%d].send[%d].Bypass = %r "
                     "(DSP setting index unknown)", ch, send_idx, value)
            return

        log.debug("SET %s = %r (send control, unhandled)", path, value)

    # ── Output/Monitor controls (SEL131 mixer param path) ────────

    def _handle_output(self, path: str, ch: int, control: str, value: Any):
        """Route output/monitor controls via SetMixerParam (SEL131).

        From hardware observation:
        Monitor controls use SEL131 with channel_type=2:
            CRMonitorLevel: param=0x01, ch_idx=1, value = 192+(dB×2)
            MonitorMute:    param=0x03, ch_idx=0, value = 2/0 (NOT 1!)
            DimOn:          param=0x44, ch_idx=0, value = 1/0

        Output 18 = MONITOR (Apollo x4), Output 19 = HP 1.
        """
        # CUE outputs (14-15 = CUE1 L/R, 16-17 = CUE2 L/R)
        # From hardware observation: all ch_type=2, ch_idx=0
        #   CUE1 MONO=param 0x06 (1/0), CUE1 MIX=param 0x05 (0=on, 2=off)
        #   CUE2 MONO=param 0x08 (1/0), CUE2 MIX=param 0x07 (0=on, 2=off)
        if ch in (14, 15, 16, 17):
            cue_idx = 0 if ch in (14, 15) else 1  # CUE1 or CUE2

            if control == "MixToMono":
                param = 0x06 if cue_idx == 0 else 0x08
                hw_value = 1 if value and value != "0" and value != "false" else 0
                log.info("HW CUE%d: MixToMono = %r → %d (param=0x%02x, ch_idx=0)",
                         cue_idx + 1, value, hw_value, param)
                self.backend.set_mixer_param(
                    MONITOR_CHANNEL_TYPE, 0, param, hw_value)
                return

            if control == "MixInSource":
                # CUE MIX: "cue"=CUE_MIX_ON (0, mix on), "mon"=CUE_MIX_OFF (2, mix off)
                # INVERTED encoding: hw 0 = mix enabled, hw 2 = mix disabled
                param = MONITOR_PARAM["CUE1Mix"] if cue_idx == 0 else MONITOR_PARAM["CUE2Mix"]
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
                # CUE1 (outputs 14/15) → MirrorA (0x2e), CUE2 (16/17) → MirrorB (0x2f)
                param_name = "MirrorA" if cue_idx == 0 else "MirrorB"
                enable_name = "MirrorEnableA" if cue_idx == 0 else "MirrorEnableB"
                source_str = str(value)
                hw_value = MIRROR_SOURCE_MAP.get(source_str, 0xFFFFFFFF)

                if hw_value == 0xFFFFFFFF:
                    # "None" → disable mirror
                    log.info("HW CUE%d: OutputDestination = %r → mirror disabled "
                             "(param=%s=0x%02x, enable=%s=0x%02x, ch_idx=9)",
                             cue_idx + 1, value, param_name,
                             MONITOR_PARAM[param_name],
                             enable_name, MONITOR_PARAM[enable_name])
                    self.backend.set_mixer_param(
                        MONITOR_CHANNEL_TYPE, 9,
                        MONITOR_PARAM[enable_name], 0)
                    self.backend.set_mixer_param(
                        MONITOR_CHANNEL_TYPE, 9,
                        MONITOR_PARAM[param_name], hw_value & 0xFFFFFFFF)
                else:
                    # Set source, then enable mirror
                    log.info("HW CUE%d: OutputDestination = %r → %d "
                             "(param=%s=0x%02x, enable=%s=0x%02x, ch_idx=9)",
                             cue_idx + 1, value, hw_value, param_name,
                             MONITOR_PARAM[param_name],
                             enable_name, MONITOR_PARAM[enable_name])
                    self.backend.set_mixer_param(
                        MONITOR_CHANNEL_TYPE, 9,
                        MONITOR_PARAM[param_name], hw_value)
                    self.backend.set_mixer_param(
                        MONITOR_CHANNEL_TYPE, 9,
                        MONITOR_PARAM[enable_name], 1)
                return

            log.debug("SET %s = %r (CUE output, unhandled control)", path, value)
            return

        if ch == 18:
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
                # Monitor mute: 2=muted, 0=unmuted (NOT 1!)
                hw_value = 2 if value and value != "0" and value != "false" else 0
                log.info("HW MONITOR: Mute = %r → %d", value, hw_value)
                self.backend.set_mixer_param(
                    MONITOR_CHANNEL_TYPE, 0,
                    MONITOR_PARAM["MonitorMute"], hw_value)
                return

            if control == "DimOn":
                # Cold boot init uses ch_idx=7 (from power cycle observation)
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
                # MonitorMute) with value=1/0. MonitorMute uses value=2/0.
                # So param 0x03 has 3-state encoding: 0=off, 1=mono, 2=muted.
                hw_value = 1 if value and value != "0" and value != "false" else 0
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
                # Verified: ch_idx=9, param=0x1e
                hw_value = 1 if value and value != "0" and value != "false" else 0
                log.info("HW MONITOR: MirrorsToDigital = %r → %d", value, hw_value)
                self.backend.set_mixer_param(
                    MONITOR_CHANNEL_TYPE, 9,
                    MONITOR_PARAM["MirrorsToDigital"], hw_value)
                return

            if control == "MixInSource":
                # Verified: ch_idx=1, 0=MIX/1=CUE1/2=CUE2
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
                # 0=SPDIF, 8=ADAT (param=0x21, ch_idx=0)
                hw_value = int(value) if value else 0
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

        # Headphone output (ch=19 on Apollo x4)
        # Confirmed: HP uses same driver path as monitor
        # (ch_type=2, ch_idx=1). HP and monitor are ganged on Apollo x4.
        if ch == 19:
            if control in ("CRMonitorLevel", "CRMonitorLevelTapered"):
                if control == "CRMonitorLevelTapered":
                    hw_value = monitor_tapered_to_hw(float(value))
                    db = (hw_value - 192) / 2.0
                else:
                    db = float(value)
                    hw_value = encode_monitor_level(db)
                log.info("HW HP: Level = %.1f dB → %d (0x%02x) "
                         "(ganged with monitor, ch_idx=%d)",
                         db, hw_value, hw_value, HP_CHANNEL_IDX)
                self.backend.set_mixer_param(
                    MONITOR_CHANNEL_TYPE, HP_CHANNEL_IDX,
                    MONITOR_PARAM["CRMonitorLevel"], hw_value)
                return

            if control == "Mute":
                hw_value = 2 if value and value != "0" and value != "false" else 0
                log.info("HW HP: Mute = %r → %d (ganged with monitor)",
                         value, hw_value)
                self.backend.set_mixer_param(
                    MONITOR_CHANNEL_TYPE, HP_CHANNEL_IDX,
                    MONITOR_PARAM["MonitorMute"], hw_value)
                return

            if control == "DimOn":
                hw_value = 1 if value and value != "0" and value != "false" else 0
                log.info("HW HP: Dim = %r → %d (ganged with monitor)",
                         value, hw_value)
                self.backend.set_mixer_param(
                    MONITOR_CHANNEL_TYPE, HP_CHANNEL_IDX,
                    MONITOR_PARAM["DimOn"], hw_value)
                return

            if control == "MixInSource":
                # Verified: ch_idx=1, p=0x3f, CUE1=0/CUE2=1
                hw_value = HP_SOURCE_MAP.get(str(value).lower(), 0)
                log.info("HW HP1: MixInSource = %r → %d", value, hw_value)
                self.backend.set_mixer_param(
                    MONITOR_CHANNEL_TYPE, 1,
                    MONITOR_PARAM["HP1Source"], hw_value)
                return

            log.info("HW HP: %s = %r (param not yet mapped)", control, value)
            return

        # Headphone 2 output (ch=20 on Apollo x4)
        if ch == 20:
            if control == "MixInSource":
                # Verified: ch_idx=1, p=0x40, CUE1=0/CUE2=1
                hw_value = HP_SOURCE_MAP.get(str(value).lower(), 0)
                log.info("HW HP2: MixInSource = %r → %d", value, hw_value)
                self.backend.set_mixer_param(
                    MONITOR_CHANNEL_TYPE, 1,
                    MONITOR_PARAM["HP2Source"], hw_value)
                return

        # Other outputs (individual analog/digital)
        log.debug("SET %s = %r (individual output, software-only)", path, value)

    # ── Aux bus controls (SEL130 bus param path) ─────────────────

    def _handle_aux(self, path: str, bus: int, control: str, value: Any):
        """Route aux bus controls via SetMixerBusParam (SEL130).

        From hardware observation: aux faders use NO dB offset (unlike inputs).
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
                # Verified: unmute restores fader level
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

        log.debug("SET %s = %r (aux control, software-only)", path, value)

    def _handle_aux_send(self, path: str, bus: int, send_idx: int,
                         control: str, value: Any):
        """Route aux bus send controls via SetMixerBusParam (SEL130)."""
        bus_id = aux_bus_id(bus)

        if control in ("Gain", "GainTapered"):
            if control == "GainTapered":
                db = fader_tapered_to_db(float(value))
            else:
                db = float(value)
            linear = encode_aux_fader(db)
            log.info("HW BUS: aux[%d].send[%d].Gain = %.1f dB → "
                     "linear=%.6f (bus=0x%04x)", bus, send_idx, db,
                     linear, bus_id)
            self.backend.set_mixer_bus_param(bus_id, SUB_PARAM_MIX, linear)
            return

        if control == "Bypass":
            log.info("HW BUS: aux[%d].send[%d].Bypass = %r "
                     "(DSP setting index unknown)", bus, send_idx, value)
            return

        log.debug("SET %s = %r (aux send control, unhandled)", path, value)

    # ── Device-level globals (under /devices/0/) ──────────────────

    def _handle_device_global(self, path: str, control: str, value: Any):
        """Route device-level global controls via SetMixerParam (SEL131)."""
        if control == "TalkbackOn":
            # Cold boot init uses ch_idx=7 (from power cycle observation)
            # Runtime toggle uses ch_idx=0 (from hardware observation)
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

        if control == "TOSLinkOutput":
            # /devices/0/TOSLinkOutput ("S/PDIF" or "ADAT") → DigitalOutputMode
            hw_value = TOSLINK_OUTPUT_MAP.get(str(value), 0)
            log.info("HW MONITOR: TOSLinkOutput = %r → DigitalOutputMode %d "
                     "(param=0x21, ch_idx=0)", value, hw_value)
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

    # ── Global/driver controls ────────────────────────────────────

    def _handle_global(self, path: str, control: str, value: Any):
        """Route global controls (sample rate, clock source).

        These go through SetDriverParameter (sel 0xA6, 16-byte struct)
        in the hardware driver. The struct is: {paramID, reserved, value_lo, value_hi}

        WARNING: Changing sample rate or clock source while streaming
        will disrupt audio. The hardware driver warns but proceeds.
        """
        if control == "SampleRate":
            try:
                rate = int(value)
            except (ValueError, TypeError):
                log.warning("HW DRV: SampleRate invalid value: %r", value)
                return

            if rate not in VALID_SAMPLE_RATES:
                log.warning("HW DRV: SampleRate %d not in valid set %s",
                            rate, sorted(VALID_SAMPLE_RATES))
                return

            # Check transport state (warn but proceed, matches hardware driver)
            transport = self.backend.get_driver_param(DRIVER_PARAM_TRANSPORT_RUNNING)
            if transport and transport > 0:
                log.warning("HW DRV: changing SampleRate while transport "
                            "running — audio will be disrupted")

            log.info("HW DRV: SampleRate → %d Hz", rate)
            self.backend.set_driver_param(DRIVER_PARAM_SAMPLE_RATE, rate)
            return

        if control == "ClockSource":
            source_str = str(value)
            source_int = CLOCK_SOURCE_MAP.get(source_str)
            if source_int is None:
                log.warning("HW DRV: ClockSource unknown value: %r "
                            "(valid: %s)", value,
                            list(CLOCK_SOURCE_MAP.keys()))
                return

            log.info("HW DRV: ClockSource → %s (value=%d)",
                     source_str, source_int)
            self.backend.set_driver_param(DRIVER_PARAM_CLOCK_SOURCE,
                                          source_int)
            return

        if control == "PostFaderMetering":
            log.info("HW DRV: PostFaderMetering = %r (software-only)", value)
            return

        log.debug("HW DRV: %s = %r (unhandled global)", control, value)

    # ── Readback / metering ──────────────────────────────────────

    def poll_hw_readback(self) -> dict | None:
        """Poll hardware readback and decode known fields.

        Called by the meter pump at ~50Hz. Returns a dict of decoded
        values, or None if readback is unavailable (--no-hardware or
        readback not ready).

        Verified register map (docs/hw-readback-map.md):
            data[0]: Preamp flags — 4ch × 6-bit stride
                     Per ch: bit+0=Mic/Line, +1=PAD, +2=Link, +3=48V,
                             +4=LowCut, +5=Phase. bit24=HiZ active.
            data[2]: Monitor section — [7:0]=MonVol, [15:8]=HP1Vol,
                     bit16=Mute, bit17=Mono, bit31=Dim
            data[3]: Preamp gain — 4ch × 8 bits
            data[6]: bit8=Talkback active
            data[7]: HP2 volume — [7:0]=vol, [31:24]=0xa0 always

        NOT present in readback:
            data[8]-[17]: Static 0xf0000000 — NOT audio meter data.
                          Audio meters must be computed in software from
                          PCM samples (CMxMeter-equivalent logic).
        """
        rb = self.backend.get_hw_readback()
        if rb is None:
            return None

        status, data = rb
        if status != 1:
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

        # Decode HP2 volume from data[7]
        d7 = data[7]
        hp2_raw = d7 & 0xFF
        result["hp2_level_raw"] = hp2_raw
        result["hp2_level_tapered"] = monitor_hw_to_tapered(hp2_raw)

        # Decode talkback status from data[6]
        d6 = data[6]
        result["talkback_active"] = bool(d6 & (1 << 8))

        # Decode preamp flags from data[0] — 6-bit stride per channel
        d0 = data[0]
        for ch in range(4):
            base = ch * 6
            result[f"preamp_{ch}_micline"] = bool(d0 & (1 << (base + 0)))
            result[f"preamp_{ch}_pad"] = bool(d0 & (1 << (base + 1)))
            result[f"preamp_{ch}_link"] = bool(d0 & (1 << (base + 2)))
            result[f"preamp_{ch}_48v"] = bool(d0 & (1 << (base + 3)))
            result[f"preamp_{ch}_lowcut"] = bool(d0 & (1 << (base + 4)))
            result[f"preamp_{ch}_phase"] = bool(d0 & (1 << (base + 5)))

        # Decode preamp gain from data[3].
        # Register layout (shared per stereo pair):
        #   bits[7:0]   = gain for active preamp in pair 1 (preamp 1 or 2)
        #   bits[15:8]  = pair 1 metadata/flags (limited resolution)
        #   bits[23:16] = gain for active preamp in pair 2 (preamp 3 or 4)
        #   bits[31:24] = upper nybble: preamp selection (0=ch1,1=ch2,2=ch3,3=ch4)
        #                 lower nybble: pair 2 metadata
        # Gain encoding: dB = raw - 55 (verified: 0x41=10dB, 0x78=65dB, 1 step=1dB)
        d3 = data[3]
        pair1_raw = d3 & 0xFF
        pair2_raw = (d3 >> 16) & 0xFF
        selection = (d3 >> 28) & 0x0F  # 0=preamp1, 1=preamp2, 2=preamp3, 3=preamp4
        result["preamp_selection"] = selection
        result["preamp_pair1_gain_raw"] = pair1_raw
        result["preamp_pair2_gain_raw"] = pair2_raw
        result["preamp_pair1_gain"] = min(65, max(10, pair1_raw - 55))
        result["preamp_pair2_gain"] = min(65, max(10, pair2_raw - 55))

        # NOTE: rb_data[8]-[17] are NOT audio metering data.
        # Confirmed static 0xf0000000 even with live audio playing.
        # Audio meters must be computed in software from PCM samples
        # in the DMA buffers (CMxMeter-equivalent logic), not read
        # from hardware readback registers.  See docs/hw-readback-map.md
        # and docs/STATUS.md issue #32.

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
