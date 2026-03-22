/* SPDX-License-Identifier: GPL-2.0-only WITH Linux-syscall-note */
/*
 * Universal Audio Apollo — Userspace ioctl interface
 *
 * Copyright (c) 2026 open-apollo contributors
 */

#ifndef UA_IOCTL_H
#define UA_IOCTL_H

#include <linux/types.h>
#include <linux/ioctl.h>

#define UA_IOCTL_MAGIC 'U'

/*
 * Device info structure returned by UA_IOCTL_GET_DEVICE_INFO.
 * Mirrors the essential fields from UAD2DeviceInfo (0xA4 = 164 bytes in kext).
 */
struct ua_device_info {
	__u32 fpga_rev;
	__u32 device_type;
	__u32 subsystem_id;
	__u32 num_dsps;
	__u32 fw_version;
	__u32 fw_v2;		/* 1 if extended (v2) firmware */
	__u32 serial;
	__u32 ext_caps;
	__u32 reserved[8];
};

/*
 * DSP info for a single SHARC DSP.
 */
struct ua_dsp_info {
	__u32 index;
	__u32 bank_base;	/* Register bank base offset */
	__u32 status;		/* 0 = idle, 1 = running */
	__u32 reserved[5];
};

/*
 * DMA ring buffer configuration (per-channel).
 * Matches the 4-page descriptor layout from CPcieRingBuffer::ProgramRegisters.
 */
struct ua_ring_config {
	__u32 channel;		/* Ring channel index */
	__u32 num_pages;	/* Number of 4K pages (max 4) */
	__u64 page_addr[4];	/* Bus addresses for each page (4K-aligned) */
	__u32 ring_size;	/* Number of descriptors (max 1024) */
	__u32 reserved;
};

/*
 * Audio transport info — mirrors UAD2AudioTransportInfo from kext.
 * Returned by UA_IOCTL_GET_TRANSPORT_INFO.
 */
struct ua_transport_info {
	__u32 play_channels;
	__u32 play_stride;	/* Bytes per frame (all play channels) */
	__u32 rec_channels;
	__u32 rec_stride;	/* Bytes per frame (all rec channels) */
	__u32 buf_frame_size;	/* Buffer frame size (power of 2) */
	__u32 sample_rate;	/* Current sample rate in Hz */
	__u32 play_latency;
	__u32 rec_latency;
	__u32 connected;	/* 1 = DSP firmware connected */
	__u32 transport_running; /* 1 = DMA active */
	__u32 reserved[6];
};

/*
 * Register read/write for debug and development.
 * Direct MMIO access — use with caution.
 */
struct ua_reg_io {
	__u32 offset;		/* BAR0 register offset */
	__u32 value;		/* Value read/written */
};

/*
 * Mixer bus parameter — maps to macOS IOKit SEL130 (SetMixerBusParam).
 * Controls fader levels, pan, send gains for input/aux buses.
 *
 * On macOS, the IOKit struct is 136 bytes (34 × u32), but most fields
 * are internal bookkeeping. Our Linux ioctl uses a minimal 16-byte struct
 * containing only the fields needed for hardware writes.
 *
 * From DTrace IOKit capture (docs/iokit-mixer-mapping.md):
 *   - bus_id identifies the mixer bus (analog inputs 0-3, digital 0xC+,
 *     sends 0x8+, aux 0x10+)
 *   - sub_param: 0=main mix, 1=CUE1 send, 2=CUE2 send, 3=gainL, 4=gainR
 *   - value is IEEE 754 float packed as u32 (linear gain, NOT dB)
 *   - Input faders have -3dB offset: linear = 10^((dB-3)/20)
 *   - Aux faders have no offset: linear = 10^(dB/20)
 *   - Each fader change sends 3 writes (sub_param 0, 3, 4)
 *   - Pan sends single write (sub_param 0)
 *   - Mute sets fader to 0.0 (linear silence)
 */
struct ua_mixer_bus_param {
	__u32 bus_id;		/* Bus identifier (see bus ID map below) */
	__u32 sub_param;	/* 0=main mix, 1=CUE1 send, 2=CUE2, 3=gainL, 4=gainR */
	__u32 value;		/* IEEE 754 float as u32 (linear gain) */
	__u32 flags;		/* Always 0x02 from captures */
};
/*
 * Complete Bus ID map (Apollo x4, verified via DTrace):
 *   0x0000-0x0003: Analog In 1-4 (mono, mic/line)
 *   0x0004-0x0005: CUE1 L/R (output mix bus)
 *   0x0006-0x0007: CUE2 L/R (output mix bus)
 *   0x0008-0x0009: S/PDIF In L/R
 *   0x000a-0x000b: Talkback 1/2
 *   0x000c-0x000f: ADAT In 1-4
 *   0x0010:        AUX 1 Return (0x0011 reserved R slot)
 *   0x0012:        AUX 2 Return (0x0013 reserved R slot)
 *   0x0014-0x0017: ADAT In 5-8
 *   0x0018-0x001f: Virtual In 1-8 (DAW playback, 4 stereo pairs)
 *
 * Sub-param: 0=main mix coeff, 1=CUE1 send, 2=CUE2 send,
 *            3=gainL (fader), 4=gainR (fader)
 * Input fader: triplet writes {sub=0, sub=3, sub=4}, unity=0.707
 * Stereo pair: sub=0 to both L+R, sub=3/4 only to L channel
 */

/*
 * Mixer parameter — maps to macOS IOKit SEL131 (SetMixerParam).
 * Handles preamp controls AND monitor controls through a unified interface.
 *
 * On macOS, the IOKit struct is 24 bytes (6 × u32):
 *   {session_id, 0, channel_type, channel_idx, param_id, value}
 *
 * Our Linux ioctl uses a 16-byte struct (session_id not needed):
 *   {channel_type, channel_idx, param_id, value}
 *
 * The driver routes based on channel_type:
 *   channel_type=1 (preamp) → ARM CLI (gain, 48V, pad, HiZ, phase)
 *   channel_type=2 (monitor) → hardware registers (level, mute, dim)
 *
 * Preamp params (ch_type=1, ch_idx=input channel):
 *   ARM paramIDs: Mute=0x00, Phase=0x01, 48V=0x04, Pad=0x08,
 *                 HiZ=0x09, LowCut=0x0A, Gain=0x17
 *   hw_id encoding: ch0=0x0000, ch1=0x0800, ch2=0x1000, ch3=0x1800
 *
 * Monitor params (ch_type=2):
 *   CRMonitorLevel: param=0x01, ch_idx=1, value = 192 + (dB × 2)
 *   MonitorMute:    param=0x03, ch_idx=0, value = 2 (muted) / 0 (NOT 1!)
 *   DimOn:          param=0x44, ch_idx=0, value = 1/0
 */
struct ua_mixer_param {
	__u32 channel_type;	/* 1=input/preamp, 2=output/monitor */
	__u32 channel_idx;	/* Channel index within type */
	__u32 param_id;		/* Parameter ID */
	__u32 value;		/* Parameter value (integer) */
};

/*
 * Driver parameter (kext sel 68 / 0xA6).
 * Controls sample rate, clock source, etc.
 * Input size: 0x10 (16 bytes).
 */
struct ua_driver_param {
	__u32 param_id;		/* Driver parameter enum */
	__u32 reserved;
	__u64 value;		/* 64-bit parameter value */
};

/*
 * Mixer readback (kext sel 25 / 0x88).
 * Returns 41 × u32 words (0xA4 bytes) with XOR checksum.
 * word[38] = XOR of word[0..37], word[39] = ~word[38].
 */
#define UA_MIXER_READBACK_WORDS	41
#define UA_MIXER_READBACK_SIZE	(UA_MIXER_READBACK_WORDS * 4)

struct ua_mixer_readback {
	__u32 input[4];		/* 16-byte input (bus/channel selector) */
	__u32 data[UA_MIXER_READBACK_WORDS];  /* 0xA4 bytes output */
};

/*
 * CLI command interface.
 * The CLI registers at BAR0+0xC3F4..0xC480 communicate with the ARM MCU.
 * Used for preamp control (gain, 48V, pad, etc.).
 */
#define UA_CLI_CMD_BUF_SIZE	128	/* 0xC400..0xC47F */
#define UA_CLI_RESP_BUF_SIZE	128	/* 0xC480..0xC4FF */

struct ua_cli_command {
	__u32 cmd_len;		/* Command data length in bytes */
	__u32 resp_len;		/* Response length (output, filled by driver) */
	__u8  cmd_data[UA_CLI_CMD_BUF_SIZE];   /* Command payload */
	__u8  resp_data[UA_CLI_RESP_BUF_SIZE]; /* Response payload (output) */
};

/*
 * Raw mixer setting read/write.
 * Provides atomic access to individual DSP mixer settings (0-37) via
 * the mixer sequence handshake protocol.  This bypasses the bus_id →
 * setting_index mapping and lets userspace manage the mapping directly.
 *
 * For writes, the driver performs the full handshake atomically:
 *   read seq → write paired words → bump seq → poll ack.
 * This prevents races when multiple processes access the mixer.
 *
 * Paired word encoding (handled by driver):
 *   wordA = (mask[15:0]  << 16) | value[15:0]
 *   wordB = (mask[31:16] << 16) | value[31:16]
 */
struct ua_mixer_setting {
	__u32 index;		/* Setting index (0-37) */
	__u32 value;		/* 32-bit setting value */
	__u32 mask;		/* Changed-bits mask (0xFFFFFFFF = all) */
	__u32 reserved;
};

/* Ioctl commands — basic */
#define UA_IOCTL_GET_DEVICE_INFO    _IOR(UA_IOCTL_MAGIC, 0x01, struct ua_device_info)
#define UA_IOCTL_GET_DSP_INFO       _IOWR(UA_IOCTL_MAGIC, 0x02, struct ua_dsp_info)
#define UA_IOCTL_GET_TRANSPORT_INFO _IOR(UA_IOCTL_MAGIC, 0x03, struct ua_transport_info)
#define UA_IOCTL_READ_REG           _IOWR(UA_IOCTL_MAGIC, 0x10, struct ua_reg_io)
#define UA_IOCTL_WRITE_REG          _IOW(UA_IOCTL_MAGIC, 0x11, struct ua_reg_io)
#define UA_IOCTL_RESET_DMA          _IO(UA_IOCTL_MAGIC, 0x20)

/* Ioctl commands — mixer (structs updated to match IOKit SEL130/SEL131) */
#define UA_IOCTL_SET_MIXER_BUS_PARAM  _IOW(UA_IOCTL_MAGIC, 0x30, struct ua_mixer_bus_param)
#define UA_IOCTL_SET_MIXER_PARAM      _IOW(UA_IOCTL_MAGIC, 0x31, struct ua_mixer_param)
#define UA_IOCTL_GET_MIXER_PARAM      _IOWR(UA_IOCTL_MAGIC, 0x32, struct ua_mixer_param)
#define UA_IOCTL_GET_MIXER_READBACK   _IOWR(UA_IOCTL_MAGIC, 0x33, struct ua_mixer_readback)
#define UA_IOCTL_SET_DRIVER_PARAM     _IOW(UA_IOCTL_MAGIC, 0x34, struct ua_driver_param)
#define UA_IOCTL_GET_DRIVER_PARAM     _IOWR(UA_IOCTL_MAGIC, 0x35, struct ua_driver_param)

/* Ioctl commands — CLI (ARM MCU communication) */
#define UA_IOCTL_CLI_COMMAND          _IOWR(UA_IOCTL_MAGIC, 0x40, struct ua_cli_command)

/* Ioctl commands — raw mixer setting access */
#define UA_IOCTL_WRITE_MIXER_SETTING  _IOW(UA_IOCTL_MAGIC, 0x36, struct ua_mixer_setting)
#define UA_IOCTL_READ_MIXER_SETTING   _IOWR(UA_IOCTL_MAGIC, 0x37, struct ua_mixer_setting)

/*
 * Hardware readback — reads status + 40 data words from the DSP readback
 * registers (BAR0+0x3810/0x3814).  The kext reads these at ~33Hz to get
 * current monitor level, metering, etc.
 *
 * If status == 1, data is valid and the driver re-arms by writing 0 to
 * the status register.  If status != 1, data is zeroed (caller retries).
 */
struct ua_hw_readback {
	__u32 status;
	__u32 data[40];
};

#define UA_IOCTL_GET_HW_READBACK  _IOR(UA_IOCTL_MAGIC, 0x41, struct ua_hw_readback)

/* Ioctl commands — DSP firmware loading */
struct ua_fw_load {
	__u64 fw_data;		/* Userspace pointer to firmware binary */
	__u32 fw_size;		/* Firmware size in bytes */
	__u32 reserved;
};

/*
 * DSP block send — used for replaying captured firmware blocks.
 * Sends a single command+data block to a DSP via ring buffer.
 *
 * From DTrace SEL127 captures:
 *   cmd = SRAM address (e.g., 0x0e8cc000)
 *   param = always 0x00000001
 *   payload = binary firmware data
 *
 * The driver allocates a DMA buffer [cmd, param, data...] and submits
 * a DMA reference entry to the DSP's command ring.
 */
struct ua_dsp_block {
	__u32 dsp_index;	/* Target DSP (0..num_dsps-1) */
	__u32 cmd;		/* Command code / SRAM address */
	__u32 param;		/* Command parameter */
	__u32 data_size;	/* Payload size in bytes (must be 4-byte aligned) */
	__u64 data;		/* Userspace pointer to payload data */
};

#define UA_IOCTL_LOAD_FIRMWARE    _IOW(UA_IOCTL_MAGIC, 0x50, struct ua_fw_load)
#define UA_IOCTL_DSP_CONNECT      _IO(UA_IOCTL_MAGIC, 0x51)
#define UA_IOCTL_DSP_TEST_DMA     _IO(UA_IOCTL_MAGIC, 0x52)
#define UA_IOCTL_DSP_TEST_SEND    _IO(UA_IOCTL_MAGIC, 0x53)
#define UA_IOCTL_DSP_SEND_BLOCK   _IOW(UA_IOCTL_MAGIC, 0x54, struct ua_dsp_block)

/*
 * Routing table configuration.
 * Sends routing table data (RT171 format) to the DSP to configure
 * DMA channel ↔ audio signal mapping.
 *
 * If data/size are 0, uses the built-in routing table for the device type.
 * If data/size are provided, sends the given blob as the routing table.
 * Direction: 0=playback, 1=record.
 */
struct ua_routing_table {
	__u32 direction;	/* 0=playback, 1=record */
	__u32 num_channels;	/* Number of channels in table */
	__u32 data_size;	/* Routing table blob size (0 = use built-in) */
	__u32 reserved;
	__u64 data;		/* Userspace pointer to routing blob */
};

#define UA_IOCTL_SET_ROUTING_TABLE _IOW(UA_IOCTL_MAGIC, 0x60, struct ua_routing_table)
#define UA_IOCTL_SEND_ROUTING      _IO(UA_IOCTL_MAGIC, 0x61)

/*
 * Raw ring buffer inline entry (4 words) to DSP 0 command ring.
 * For testing ring buffer commands from userspace.
 */
struct ua_ring_cmd {
	__u32 dsp_idx;	/* Target DSP (0-3) */
	__u32 word0;	/* Command word (e.g., 0x001F0004) */
	__u32 word1;
	__u32 word2;
	__u32 word3;
};

#define UA_IOCTL_RING_SEND _IOW(UA_IOCTL_MAGIC, 0x70, struct ua_ring_cmd)

/* Load mixer firmware to a specific DSP (0-3). Arg = DSP index as u32. */
#define UA_IOCTL_LOAD_FW_DSP  _IOW(UA_IOCTL_MAGIC, 0x71, __u32)

/* Send raw DMA data to DSP (no cmd/param header). Same struct as dsp_block
 * but cmd/param fields are ignored — data is sent as-is via DMA ref. */
#define UA_IOCTL_DSP_SEND_RAW _IOW(UA_IOCTL_MAGIC, 0x72, struct ua_dsp_block)

/* Send SRAM config + data as atomic pair (inline 0x00080004 + DMA ref).
 * Uses dsp_block struct: cmd=sram_addr, param ignored, data=payload. */
#define UA_IOCTL_DSP_SRAM_DATA _IOW(UA_IOCTL_MAGIC, 0x73, struct ua_dsp_block)

/*
 * Raw DMA buffer capture test — bypasses ALSA entirely.
 *
 * Fills rec_buf with canary (0xDE), starts transport via proper
 * driver functions, waits delay_ms, stops transport, copies first
 * 1024 bytes of rec_buf to userspace.
 *
 * Input:  ua_dma_test { dma_ctrl_mask, delay_ms, flags, reserved }
 * Output: ua_dma_test { ..., data[256] } (first 256 u32 of rec_buf)
 */
struct ua_dma_test {
	__u32 dma_ctrl;	  /* DMA_CTRL value to set (0 = don't change) */
	__u32 delay_ms;	  /* How long to run transport (50-2000) */
	__u32 flags;	  /* bit0: do clock write after start */
	__u32 sample_pos; /* OUT: SAMPLE_POS after delay */
	__u32 nonzero;	  /* OUT: count of non-0xDEDEDEDE dwords */
	__u32 non_zero_non_canary; /* OUT: non-zero AND non-canary count */
	__u32 data[256];  /* OUT: first 1024 bytes of rec_buf */
};

#define UA_IOCTL_DMA_TEST _IOWR(UA_IOCTL_MAGIC, 0x80, struct ua_dma_test)

#endif /* UA_IOCTL_H */
