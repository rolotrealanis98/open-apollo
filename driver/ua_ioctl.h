/* SPDX-License-Identifier: GPL-2.0-only WITH Linux-syscall-note */
/*
 * Universal Audio Apollo — Userspace ioctl interface
 *
 * Copyright (c) 2026 apollo-linux contributors
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
	__u32 public_type;	/* kext UAD2DeviceType (x4 = 0x1A); 0xFFFFFFFF = unresolved */
	__u32 reserved[7];
};

/*
 * DSP info for a single SHARC DSP.
 *
 * UA_FEAT_DSP_LOAD (ABI 1.6): the load words of the kext's GetDSPInfo
 * (selector 112, UAD2DSPInfo), all in 1/100 % (10000 = 100 %); an older
 * driver leaves them 0.  The kext's DSP % is host bookkeeping: the plugin
 * loads it allocated plus a rate overhead.  This driver allocates nothing
 * on the DSPs, so
 *   alloc_load    = the rate overhead alone (UAD2DSPInfo+0x40): on the x4
 *                   110 at 44.1 kHz, 119 at 48, 220/239 at 88.2/96, 183/200
 *                   at 176.4/192.  UA's Mixer Engine shows the average over
 *                   the DSPs, divided by 100, as TotalDSPLoad.
 *   measured_load = the DSP's own load word (window +0x0C, UAD2DSPInfo+0x44,
 *                   the Mixer Engine's MeasuredDSPLoad x 100)
 *   pgm_load      = pool 3 used / available (the Mixer Engine's
 *                   TotalPGMLoad x 100): 0, nothing is allocated
 *   mem_load      = the largest of pools 0-2 used / available
 *                   (TotalMEMLoad x 100): 0, nothing is allocated
 *   flags         = UA_DSP_INFO_F_LOADS when the words above are filled
 */
struct ua_dsp_info {
	__u32 index;
	__u32 bank_base;	/* Register bank base offset */
	__u32 status;		/* 0 = idle, 1 = running */
	__u32 alloc_load;
	__u32 measured_load;
	__u32 pgm_load;
	__u32 mem_load;
	__u32 flags;
};

#define UA_DSP_INFO_F_LOADS	0x1

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
 *   - sub_param: the kext's bus param, 0=control room / main mix,
 *     1=HP1 (CUE 1) send, 2=HP2 (CUE 2) send, 3=Aux 1 send, 4=Aux 2 send,
 *     5=HP3 (CUE 3) send, 6=HP4 (CUE 4) send.  (3 and 4 are NOT gain L/R.)
 *   - value is IEEE 754 float packed as u32 (linear gain, NOT dB)
 *   - Input faders have -3dB offset: linear = 10^((dB-3)/20)
 *   - Aux faders have no offset: linear = 10^(dB/20)
 *   - Mute sets fader to 0.0 (linear silence)
 *
 * Sides: the kext's SetMixerBusParam takes (param, side, value) entries, and
 * the Mixer Engine sends two per bus and param: side 0 = weight into the
 * left output, side 1 = weight into the right one (pan law x fader x send).
 * One ioctl carries one value, so one call writes one entry: side 0 by
 * default, side 1 with UA_BUS_PARAM_F_SIDE_R in flags.  A pan or stereo send
 * is two calls (left, then right).  The driver never writes the other side
 * on its own.
 */
struct ua_mixer_bus_param {
	__u32 bus_id;		/* Bus identifier (see bus ID map below) */
	__u32 sub_param;	/* bus param 0-6, see above */
	__u32 value;		/* IEEE 754 float as u32 (linear gain) */
	__u32 flags;		/* 0x02 from captures; UA_BUS_PARAM_F_* */
};
/* flags: this entry is the right-output weight (side 1); else side 0 */
#define UA_BUS_PARAM_F_SIDE_R	0x100
/*
 * flags: this entry is a solo-muted weight (UA_FEAT_BUS_SOLO_FLAG).  With
 * the bus_solo_sign module parameter on, the driver sends the value as the
 * macOS Mixer Engine sends a solo-muted input: sign bit set, an exact zero
 * replaced by -1.0e-9 (0xb089705f).  With it off (default) the flag is
 * accepted and the value goes out as given.  The batch ioctl has no
 * per-entry flags and never sets it.
 */
#define UA_BUS_PARAM_F_SOLO_MUTE	0x200
/*
 * Complete Bus ID map (Apollo x4, verified Phase 3e 2026-02-19):
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
 * Sub-param: 0=CR/main mix coeff, 1=HP1 (CUE1), 2=HP2 (CUE2), 3=Aux 1,
 *            4=Aux 2, 5=HP3 (CUE3), 6=HP4 (CUE4); each has a left and a
 *            right (UA_BUS_PARAM_F_SIDE_R) entry.  A fader or pan move
 *            writes sub 0 only; unity input fader = 0.707.
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
 *   channel_type=3 (rate ack) → {0x001A0002, current rate} to every DSP, as
 *     the Mixer Engine's AckSampleRateChange; the other fields are ignored.
 *     -EBUSY while audio is paused, -ENODEV before the connect, -EINVAL on a
 *     driver that predates it (no feature bit)
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
	__u32 channel_type;	/* 0=bus enable, 1=input/preamp, 2=output/monitor, 3=rate ack */
	__u32 channel_idx;	/* Channel index within type */
	__u32 param_id;		/* Parameter ID */
	__u32 value;		/* Parameter value (integer) */
};

/*
 * Driver parameter (kext selectors 166 / 0xA6 SetDriverParameter and
 * 167 / 0xA7 GetDriverParameter; same 16-byte struct).
 *
 * Linux ids (unchanged):
 *   UA_DRIVER_PARAM_RATE        0  sample rate in Hz (set/get)
 *   UA_DRIVER_PARAM_CLOCK_SRC   1  clock source, UA_CLOCK_* (set/get)
 *   UA_DRIVER_PARAM_TRANSPORT   2  1 = transport running (get only)
 *
 * The kext's own ids 1-5 mean other things (its 1 and 2 collide with the
 * Linux ones), so they are reached as UA_DRIVER_PARAM_KEXT(id) = 0x100 + id
 * (UA_FEAT_KEXT_DRIVER_PARAMS, ABI 1.5).  CDeviceManager::Set/
 * GetDriverParameter (kext 12.0.0 x86 0x1dd38 / 0x1de68) only store them;
 * here they are stored per device and have no effect:
 *   KEXT(1) 0x101  "enable all devices" (mgr+0x5100, initial 0):
 *                  UAD2Pcie2::probe accepts an Apollo subsystem outside its
 *                  allow-list only when this reads 1.  The Linux driver binds
 *                  every subsystem anyway.  The Mixer Engine sets it from
 *                  GetEnableAllDevices in DMixer::Initialize.
 *   KEXT(2) 0x102  diagnostic flags (SetDiagnosticFlags 0x18d4c, mgr+0x5084,
 *                  initial 0): bit 0 clears KEXT(3) and is not kept; bits 1
 *                  and 2 select audio-engine debug logging (copied at engine
 *                  start, 0x38951).
 *   KEXT(3) 0x103  a counter (mgr+0x5088): Set clears it whatever the value;
 *                  nothing in kext 12.0.0 increments it, so it reads 0.
 *   KEXT(4) 0x104  a 64-bit value (mgr+0x5090, initial 0), stored, never read.
 *   KEXT(5) 0x105  "mixer connected" (mgr+0x5104, initial 1): the Mixer
 *                  Engine writes 1 in DMixer::ConnectToHardware; nothing
 *                  reads it.
 * 32-bit ids (1, 2, 3, 5) keep the low 32 bits; Get sign-extends ids 1 and
 * 5 as the kext does.  Any other id: -EINVAL.
 */
struct ua_driver_param {
	__u32 param_id;		/* Driver parameter enum */
	__u32 reserved;
	__u64 value;		/* 64-bit parameter value */
};

#define UA_DRIVER_PARAM_RATE		0
#define UA_DRIVER_PARAM_CLOCK_SRC	1
#define UA_DRIVER_PARAM_TRANSPORT	2
#define UA_DRIVER_PARAM_KEXT_BASE	0x100
#define UA_DRIVER_PARAM_KEXT(n)		(UA_DRIVER_PARAM_KEXT_BASE + (n))

/*
 * Mixer readback (kext selector 136 / 0x88, GetMixerReadback).
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
 * DSP block send (UA_IOCTL_DSP_SEND_BLOCK) — the kext's _sendBlock, used
 * for replaying captured firmware blocks.  One command + data block to a
 * DSP through its rings (ua_dsp_send_block()):
 *   - a 4-dword response descriptor on the response ring first;
 *   - a header DMA ref on the command ring: {(dwords + 1) | cmd} below
 *     0x3FFFC bytes of data, {cmd | BIT(30), dwords + 2} from there on;
 *   - the data as 1 KiB DMA refs (2 or 4 KiB with sendblock_chunk), one
 *     doorbell for the batch;
 *   - a wait of up to 5 s for the response.
 * cmd is the command code (0x120000 for the firmware transaction; the
 * captures' SRAM addresses are host buffer addresses, not commands).
 * param is never sent: with bit 31 set its high half is the expected high
 * half of response word 0 (0x80040000 for firmware).  The response's
 * status word decodes into the kext's result codes; with block_strict=1 a
 * bad one fails the call with -EIO, otherwise it is only logged.  No
 * response: -ETIMEDOUT.  data_size: a multiple of 4, at most 4 MiB, and
 * the header plus the data refs must fit one command ring (1023 entries:
 * about 1 MiB at the default 1 KiB chunk), else -E2BIG.
 * (UA_IOCTL_DSP_SRAM_DATA and UA_IOCTL_DSP_SEND_RAW reuse the struct.)
 */
struct ua_dsp_block {
	__u32 dsp_index;	/* Target DSP (0..num_dsps-1) */
	__u32 cmd;		/* Command code (SRAM address for SRAM_DATA) */
	__u32 param;		/* Expected response (see above); not sent */
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

/*
 * UA_IOCTL_SEND_ROUTING writes a macOS-captured IO descriptor over the
 * firmware's own at BAR0+0xC1A4/0xC2C4 (x4 and x8p only).  Kept for
 * experiments; nothing in userspace calls it.  The kext never does this:
 * SetDriverIoRoutings (SEL171) only stores the host-side channel ->
 * {device, channel ID, name} table, nothing is sent to the DSP, and the
 * descriptor areas are firmware-written and only read by the kext.  With
 * the descriptor layout selected (io_map=1, or the io_map=-1 default on
 * the x4) the ioctl is a no-op that returns 0.
 */
#define UA_IOCTL_SEND_ROUTING      _IO(UA_IOCTL_MAGIC, 0x61)

/*
 * IO map: the firmware IO descriptors and the ALSA channel -> DMA slot
 * map the driver derived from them (UA_IOCTL_GET_IO_MAP, read-only).
 *
 * Channel IDs are UA channel IDs, type << 8 | number (number 1-based):
 *   0x00 MIC/LINE/HIZ  0x01 MIC/LINE  0x02 AUX  0x03 LINE  0x04 ADAT
 *   0x05 S/PDIF  0x06 AES/EBU  0x07 VIRTUAL  0x08 MADI  0x09 MON
 *   0x0a CUE  0x0b TALKBACK  0x0c RESERVED  0x0d DANTE  else UNKNOWN
 * AUX, S/PDIF, AES/EBU, MON and CUE are stereo pairs: number n is pair
 * (n + 1) / 2, L when n is odd.
 *
 * In the legacy layout the map is the identity over the first
 * hw_channels slots and the names are the model's route-table names for
 * that layout; in the descriptor layout (io_map=1, or the io_map=-1
 * default on the x4) each ALSA channel is placed on the descriptor slot
 * carrying its channel ID.
 *
 * desc_* describe the last valid descriptor the firmware published for
 * the direction: a read that is not usable (all-ones, mid-republish)
 * does not replace it, it only sets desc_last_bad and last_hdr.
 */
#define UA_IO_MAP_MAX_CH	64
#define UA_IO_MAP_NAME_LEN	24
#define UA_IO_MAP_HDR_WORDS	6

struct ua_io_map_chan {
	__u16 ch_id;		/* UA channel ID, 0 = unknown */
	__u16 slot;		/* Slot in the DMA frame, 0xFFFF = none at this rate */
	char name[UA_IO_MAP_NAME_LEN];	/* NUL-terminated */
};

struct ua_io_map_dir {
	__u32 desc_valid;	/* 1 = a valid firmware descriptor was read */
	__u32 desc_count;	/* Slots listed by that descriptor */
	__u32 desc_flags;	/* Its word 5 */
	__u32 desc_last_bad;	/* 1 = the latest read was not usable */
	__u32 last_hdr[UA_IO_MAP_HDR_WORDS];	/* Header of the latest read */
	__u32 hw_channels;	/* AX_*_CH / DMA frame stride in samples */
	__u32 channels;		/* ALSA-visible channels (entries in chan[]) */
	__u32 digital_mode;	/* Route-list TOSLink mode: 0 S/PDIF, 1 ADAT */
	__u32 reserved;
	__u16 desc_ids[UA_IO_MAP_MAX_CH];	/* Channel ID per descriptor slot */
	struct ua_io_map_chan chan[UA_IO_MAP_MAX_CH];	/* ALSA order */
};

struct ua_io_map_info {
	__u32 io_map;		/* Layout selected for this device: 0 legacy,
				 * 1 descriptor (io_map=-1 resolves per model) */
	__u32 active;		/* 1 = descriptor-mapped layout in use */
	__u32 stale;		/* 1 = layout changed, streams re-prepare */
	__u32 device_type;
	__u32 sample_rate;	/* Rate the map was built for */
	__u32 reserved[3];
	struct ua_io_map_dir rec;
	struct ua_io_map_dir play;
};

#define UA_IOCTL_GET_IO_MAP        _IOR(UA_IOCTL_MAGIC, 0x62, struct ua_io_map_info)

/*
 * Raw ring buffer inline entry (4 words) to a DSP's command ring.
 * For testing ring buffer commands from userspace.  Inline entries
 * only: word0 with bit 31 set (a DMA reference) fails with -EINVAL.
 */
struct ua_ring_cmd {
	__u32 dsp_idx;	/* Target DSP (0-3) */
	__u32 word0;	/* Command word (e.g., 0x001F0004) */
	__u32 word1;
	__u32 word2;
	__u32 word3;
};

#define UA_IOCTL_RING_SEND _IOW(UA_IOCTL_MAGIC, 0x70, struct ua_ring_cmd)

/*
 * RETIRED: the number stays reserved and the ioctl returns -ENOTTY.  The
 * kext loads firmware to DSP 0 only; DSPs 1+ boot themselves.
 */
#define UA_IOCTL_LOAD_FW_DSP  _IOW(UA_IOCTL_MAGIC, 0x71, __u32)

/* Send raw DMA data to DSP (no cmd/param header). Same struct as dsp_block
 * but cmd/param fields are ignored — data is sent as-is via DMA ref.
 * -ETIMEDOUT when the ring has not consumed it within 6 s. */
#define UA_IOCTL_DSP_SEND_RAW _IOW(UA_IOCTL_MAGIC, 0x72, struct ua_dsp_block)

/* Send SRAM config + data as atomic pair (inline 0x00080004 + DMA ref).
 * Uses dsp_block struct: cmd=sram_addr, param ignored, data=payload.
 * -ETIMEDOUT when the ring has not consumed the pair within 6 s. */
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

/*
 * ABI version (UA_IOCTL_GET_VERSION).
 *
 * The ioctl numbers, struct layouts and event codes in this file are one
 * ABI with a major and a minor number:
 *   - minor is bumped for every addition that old user space can ignore:
 *     a new ioctl, a new event code, a new feature bit, a reserved field
 *     that starts to carry a value (zero stays the old meaning);
 *   - major is bumped for anything that breaks a caller written against
 *     the same major: a changed struct size or field meaning, a renumbered
 *     or removed ioctl, a changed event code or argument.
 * A caller that needs a feature checks the feature bit (or abi_minor), not
 * the driver version string.  A driver that predates GET_VERSION returns
 * -ENOTTY; treat that as "ABI 1.0 without any of the feature bits".
 *
 * The struct is fixed size: never change sizeof(struct ua_version).
 *
 * Numbers kept free (2026-10-11, apollo-linux codex-driver-overlay-review):
 *   - ioctls 0x09-0x0e and events 0x1008/0x1009 are never assigned: an
 *     out-of-tree plugin prototype used them, so a caller built against it
 *     must get -ENOTTY here, not a different operation;
 *   - ioctls 0x90-0x9f are reserved for plugin hosting, events for it start
 *     at 0x1010;
 *   - feature bit 31 is a continuation bit: reserved[0] is a second feature
 *     word (UA_FEAT2_*) and bit 31 says it is valid (an older driver leaves
 *     reserved[] zero).  From ABI 1.8 on, bit 31 is set whenever reserved[0]
 *     carries a bit, so a driver with no UA_FEAT2_* bit to report returns
 *     the same features word as before.
 */
#define UA_ABI_MAJOR		1
#define UA_ABI_MINOR		8

#define UA_FEAT_EVENTS		(1u << 0)  /* read()/poll() event channel */
#define UA_FEAT_PUBLIC_TYPE	(1u << 1)  /* ua_device_info.public_type */
#define UA_FEAT_BUS_PARAM_SIDE	(1u << 2)  /* UA_BUS_PARAM_F_SIDE_R */
#define UA_FEAT_MIXER_PARAM_T0	(1u << 3)  /* GET_MIXER_PARAM type 0 (bus mask) */
#define UA_FEAT_KIND_GATES	(1u << 4)  /* params gated by mixer kind (-EINVAL) */
#define UA_FEAT_MIXER_STATE	(1u << 5)  /* /proc/asound/cardN/mixer_state */
#define UA_FEAT_SYSTEM_INFO	(1u << 6)  /* UA_IOCTL_GET_SYSTEM_INFO */
#define UA_FEAT_BUS_PARAM_BATCH	(1u << 7)  /* UA_IOCTL_SET_MIXER_BUS_PARAMS */
#define UA_FEAT_PAUSE_INFO	(1u << 8)  /* UA_IOCTL_GET_PAUSE_INFO */
#define UA_FEAT_PAUSE_EVENTS	(1u << 9)  /* UA_EVT_PAUSED / UA_EVT_RESUMED */
#define UA_FEAT_UNIT_GROUP	(1u << 10) /* unit index/role in SYSTEM_INFO */
#define UA_FEAT_BUS_SOLO_FLAG	(1u << 11) /* UA_BUS_PARAM_F_SOLO_MUTE */
#define UA_FEAT_SYSINFO_EXT	(1u << 12) /* pcie_link_*, clock_src_mask, fw_rate_mask in SYSTEM_INFO */
#define UA_FEAT_PLUGIN_CAPS	(1u << 13) /* UA_IOCTL_GET_PLUGIN_CAPS (ABI 1.3) */
#define UA_FEAT_FW_RELEASE	(1u << 14) /* fw_release, dev_info in SYSTEM_INFO (ABI 1.4) */
#define UA_FEAT_KEXT_DRIVER_PARAMS (1u << 15) /* UA_DRIVER_PARAM_KEXT(1..5) (ABI 1.5) */
#define UA_FEAT_DSP_LOAD	(1u << 16) /* load words in ua_dsp_info (ABI 1.6) */
#define UA_FEAT_CLOCK_SOURCES	(1u << 17) /* UA_IOCTL_GET_CLOCK_SOURCES (ABI 1.7) */
#define UA_FEAT_FEATURES2	(1u << 31) /* reserved[0] = UA_FEAT2_* (ABI 1.8) */

/*
 * Second feature word: ua_version.reserved[0], valid when UA_FEAT_FEATURES2
 * is set.  UA_FEAT2_PLUGIN_HOST: the module was loaded with plugin_host=1,
 * so UA_IOCTL_BOARD_QUERY and UA_IOCTL_PLUGIN_INSTANCE are served (ABI
 * 1.8).  Which operations are accepted right now is what
 * GET_PLUGIN_CAPS.operations says.
 */
#define UA_FEAT2_PLUGIN_HOST	(1u << 0)

struct ua_version {
	__u32 abi_major;
	__u32 abi_minor;
	__u32 driver_version;	/* 0x00MMmmpp: 0.2.0 = 0x000200 */
	__u32 features;		/* UA_FEAT_* */
	char driver_name[16];	/* NUL-terminated version string, e.g. "0.2.0" */
	__u32 reserved[4];	/* [0]: UA_FEAT2_* when UA_FEAT_FEATURES2 */
};

#define UA_IOCTL_GET_VERSION	_IOR(UA_IOCTL_MAGIC, 0x04, struct ua_version)

/*
 * Device and system info (UA_IOCTL_GET_SYSTEM_INFO), the Linux side of the
 * kext's GetDeviceInfo (111/157) and GetSystemInfo (109) as far as the
 * driver knows the values.  A separate ioctl rather than an extension of
 * GET_DEVICE_INFO: _IOR encodes sizeof(struct ua_device_info) in the
 * number, so that struct cannot grow, and its 7 reserved words cannot hold
 * a serial string plus the clock and group state.  GET_DEVICE_INFO is left
 * as it was.
 *
 * Unit group (multi-unit groundwork): every probed Apollo is in group 0;
 * the first registered unit is the master and the next one takes over when
 * it is removed.  unit_index is the position in registration order; a lone
 * unit is index 0, count 1, master.  A unit that is being removed reports
 * unit_count 0 and unit_index 0xFFFFFFFF.
 */
#define UA_UNIT_ROLE_NONE	0
#define UA_UNIT_ROLE_MASTER	1
#define UA_UNIT_ROLE_FOLLOWER	2

struct ua_system_info {
	__u32 fpga_rev;
	__u32 device_type;	/* serial-table type */
	__u32 public_type;	/* kext UAD2DeviceType; 0xFFFFFFFF = unresolved */
	__u32 subsystem_id;
	__u32 pci_device;	/* PCI device ID */
	__u32 num_dsps;
	__u32 fw_version;
	__u32 fw_v2;		/* 1 = extended (v2) firmware */
	__u32 ext_caps;
	__u32 hw_uid_lo;	/* BAR0 0x30/0x34: the kext's HWID */
	__u32 hw_uid_hi;
	__u32 mixer_kind;	/* kext mixer kind; 0xFFFFFFFF = none/ungated */
	char serial[16];	/* the real serial: 16 ASCII chars from BAR0
				 * 0x20-0x2F, NUL-padded after the first NUL
				 * (not terminated when all 16 are used).  Not
				 * ua_device_info.serial, which is BAR0 0x2238
				 * and reads 0 on the x4 */
	__u32 sample_rate;	/* Hz */
	__u32 rate_index;	/* 1 = 44.1k .. 6 = 192k, 0 = unset */
	__u32 clock_source;	/* UA_CLOCK_* */
	__u32 clock_lock_valid;	/* 1 = clock_locked comes from a live readback */
	__u32 clock_locked;
	__u32 connected;	/* 1 = DSP firmware connected */
	__u32 transport_running;
	__u32 play_channels;
	__u32 rec_channels;
	__u32 unit_group;	/* group id (0) */
	__u32 unit_index;
	__u32 unit_count;	/* units in the group */
	__u32 unit_role;	/* UA_UNIT_ROLE_* */
	/*
	 * UA_FEAT_SYSINFO_EXT (ABI 1.2); an older driver returns 0 here.
	 * pcie_link_speed: the PCI_EXP_LNKSTA current-link-speed field (1 =
	 * 2.5 GT/s, 2 = 5 GT/s, 3 = 8 GT/s, 4 = 16 GT/s), 0 = unknown.
	 * pcie_link_width: the negotiated lanes (1, 2, 4 ...), 0 = unknown.
	 * clock_src_mask: 0xC08C[31:16] as read at connect (bit n = clock
	 * source n is offered), 0 = not read.
	 * fw_rate_mask: 0xC08C[7:0], bit n = rate index n offered (1 = 44.1k
	 * .. 6 = 192k), 0 = not read.
	 */
	__u32 pcie_link_speed;
	__u32 pcie_link_width;
	__u32 clock_src_mask;
	__u32 fw_rate_mask;
	/*
	 * UA_FEAT_FW_RELEASE (ABI 1.4); an older driver returns 0 here.
	 * fw_release: BAR0 0x0008, the firmware version word the macOS driver
	 * returns in UAD2DeviceInfo+0x24 (x4: 0x0b0fc9b3).  Decode it with
	 * UA_FW_MAJOR/MINOR/BUILD; UA's Mixer Engine shows it "%d.%02d.%03d"
	 * and accepts a major equal to the model's (9 or 11).  fw_version above
	 * is BAR0 0x0000, DSP 0's status word, and is not a version.
	 * dev_info: BAR0 0x01AC (UAD2DeviceInfo+0x80) for the public types the
	 * macOS driver reads it for, 0xFFFFFFFF for the others, as it reports
	 * them.  Bits [19:16] are the revision the Mixer Engine's per-device
	 * firmware checks test (x6/x8/x8p/x16/x16D).
	 */
	__u32 fw_release;
	__u32 dev_info;
	__u32 reserved[2];
};

#define UA_FW_MAJOR(v)		(((v) >> 24) & 0xff)
#define UA_FW_MINOR(v)		(((v) >> 16) & 0xff)
#define UA_FW_BUILD(v)		((v) & 0xffff)

#define UA_IOCTL_GET_SYSTEM_INFO	_IOR(UA_IOCTL_MAGIC, 0x05, struct ua_system_info)

/*
 * Pause reasons and the LockSampleRateAndClock level
 * (UA_IOCTL_GET_PAUSE_INFO), after the kext's CanAudioStream (15 section
 * 5.1) and the 0x264/0x265 notifications.  The bits are the kext's:
 *   UA_PAUSE_PHASE   0x1  PLL phase correction (multi-unit; never set yet)
 *   UA_PAUSE_RATE    0x2  a sample-rate or clock change is in progress
 *   UA_PAUSE_DEVICE  0x4  the unit is suspended, resuming or being removed
 *   UA_PAUSE_IDLE    0x8  audio is not running (derived: no transport)
 * Streaming is possible when bits 0 and 1 are clear; 0x4 and 0x8 are
 * reported but do not block, as in the kext.  UA_EVT_PAUSED / UA_EVT_RESUMED
 * carry the bit that was set / cleared (never UA_PAUSE_IDLE).
 *
 * lock_level is the kext's LockSampleRateAndClock level (0 none, 1-2 rate,
 * 3 rate and clock).  Nothing sets it on Linux yet, so it reads 0.
 */
#define UA_PAUSE_PHASE		0x1
#define UA_PAUSE_RATE		0x2
#define UA_PAUSE_DEVICE		0x4
#define UA_PAUSE_IDLE		0x8

struct ua_pause_info {
	__u32 reasons;		/* UA_PAUSE_* */
	__u32 can_stream;	/* 1 when UA_PAUSE_PHASE and _RATE are clear */
	__u32 lock_level;	/* 0..3 */
	__u32 reserved[5];
};

#define UA_IOCTL_GET_PAUSE_INFO		_IOR(UA_IOCTL_MAGIC, 0x06, struct ua_pause_info)

/*
 * Plugin capabilities (UA_IOCTL_GET_PLUGIN_CAPS, read-only, ABI 1.3,
 * UA_FEAT_PLUGIN_CAPS).
 *
 * `operations` has a UA_PLUGIN_OP_* bit for each plug-in hosting operation
 * the driver accepts right now, and only then: the module was loaded with
 * plugin_host=1 (UA_FEAT2_PLUGIN_HOST), the device is up and connected, and
 * DSP 0's rings run.  With plugin_host=0 (the default) it is 0, as it was
 * before ABI 1.8, and a client that presents plug-ins must treat every one
 * as unavailable on the hardware.  A bit that is clear means the ioctl
 * fails now (-EOPNOTSUPP, -ENODEV, ...), not that it does not exist.
 *   UA_PLUGIN_OP_BOARD_QUERY  UA_IOCTL_BOARD_QUERY
 *   UA_PLUGIN_OP_CREATE       UA_IOCTL_PLUGIN_INSTANCE, UA_PLUGIN_INST_CREATE
 *                             (the device's own authorization answer for the
 *                             plug-in is read first; see below)
 *   UA_PLUGIN_OP_DESTROY      UA_IOCTL_PLUGIN_INSTANCE, UA_PLUGIN_INST_DESTROY
 *
 * `state` reports what the driver last did, not a verified DSP state:
 *   UA_PLUGIN_STATE_FW_LOADED       a DSP firmware load completed: the
 *                                   cold-start stage (ua-apollo-mixer.bin)
 *                                   or UA_IOCTL_LOAD_FIRMWARE; cleared by
 *                                   a cold re-init and on teardown
 *   UA_PLUGIN_STATE_DSPS_CONNECTED  the DSP ring connect ran
 *                                   (ring-connect models, kext_connect)
 *   UA_PLUGIN_STATE_INIT_CHAIN      the captured DSP-0 init sequence
 *                                   (ua-apollo-plugin-chain.bin) was sent:
 *                                   fixed module setup, not a plugin load
 *   UA_PLUGIN_STATE_AUDIO_CONNECTED the audio extension is connected
 *   UA_PLUGIN_STATE_HOST            plugin_host=1 (ABI 1.8)
 *   UA_PLUGIN_STATE_STRIPS          the driver's Console input strips run
 *                                   on DSP 0 (input_strips=1, ABI 1.8)
 * dsp_rings: bit n set = DSP n has its command and response rings.
 * dma_orphans: DMA buffers a timed-out ring command may still reference;
 * the driver frees each once the ring is provably past it.
 * ABI 1.8 (0 before):
 * owned_dsps: bit n = this driver owns DSP n's memory pools: it found the
 *   DSP freshly powered (no host since power-up) at its last cold start and
 *   nothing outside the plug-in host (a raw DSP ioctl, another firmware
 *   load) has touched it since.  Only owned DSPs ever get strips or
 *   plug-ins.
 * quarantined_dsps: bit n = an operation on DSP n ended with an outcome the
 *   driver cannot prove (a reply that never came, a teardown the ring did
 *   not take); nothing more is placed there until the next cold start.
 * instances: plug-in instances that exist (all open files).
 * Other reserved words are 0.
 */
#define UA_PLUGIN_STATE_FW_LOADED	(1u << 0)
#define UA_PLUGIN_STATE_DSPS_CONNECTED	(1u << 1)
#define UA_PLUGIN_STATE_INIT_CHAIN	(1u << 2)
#define UA_PLUGIN_STATE_AUDIO_CONNECTED	(1u << 3)
#define UA_PLUGIN_STATE_HOST		(1u << 4)
#define UA_PLUGIN_STATE_STRIPS		(1u << 5)

#define UA_PLUGIN_OP_BOARD_QUERY	(1ull << 0)
#define UA_PLUGIN_OP_CREATE		(1ull << 1)
#define UA_PLUGIN_OP_DESTROY		(1ull << 2)

struct ua_plugin_caps {
	__u32 struct_size;	/* sizeof(struct ua_plugin_caps) */
	__u32 state;		/* UA_PLUGIN_STATE_* */
	__u64 operations;	/* UA_PLUGIN_OP_*: accepted right now */
	__u32 num_dsps;
	__u32 dsp_rings;	/* bit n: DSP n's rings are allocated */
	__u32 dma_orphans;	/* parked DMA buffers, see above */
	__u32 owned_dsps;	/* ABI 1.8, see above */
	__u32 quarantined_dsps;	/* ABI 1.8 */
	__u32 instances;	/* ABI 1.8 */
	__u32 reserved[6];
};

#define UA_IOCTL_GET_PLUGIN_CAPS	_IOR(UA_IOCTL_MAGIC, 0x07, struct ua_plugin_caps)

/*
 * The device's clock-source menu (UA_IOCTL_GET_CLOCK_SOURCES, read-only,
 * ABI 1.7, UA_FEAT_CLOCK_SOURCES), as the kext's GetClockSourceInfo
 * reports it: the sources the device offers (0xC08C[31:16]) with the
 * names it publishes at 0xC090 (position = source value), Internal first,
 * the peer source (0xA) left out.  `value` is what UA_DRIVER_PARAM 1
 * (clock source) takes.  UA_CLOCK_SRC_F_DEVICE_NAME: the name is the
 * device's own (an AES/EBU or MADI unit names its digital source itself);
 * without it the driver filled in a generic name.  The list is read at
 * every connect; count is 0 before the first one.
 */
#define UA_CLOCK_SOURCES_MAX		16
#define UA_CLOCK_SRC_F_DEVICE_NAME	(1u << 0)

struct ua_clock_source {
	__u32 value;		/* clock-config low byte (UAD2ClockSource) */
	__u32 flags;		/* UA_CLOCK_SRC_F_* */
	char name[32];		/* NUL-terminated */
};

struct ua_clock_sources {
	__u32 struct_size;	/* sizeof(struct ua_clock_sources) */
	__u32 count;		/* valid entries in src[] */
	__u32 mask;		/* 0xC08C[31:16] as read at connect */
	__u32 reserved;
	struct ua_clock_source src[UA_CLOCK_SOURCES_MAX];
};

#define UA_IOCTL_GET_CLOCK_SOURCES	_IOR(UA_IOCTL_MAGIC, 0x08, struct ua_clock_sources)

/*
 * Batched bus params (kext selector 130 carries a batch of
 * bus/param/side/value entries and a hardware id).  One ioctl, up to
 * UA_BUS_PARAM_BATCH_MAX entries, applied in order under one device lock
 * (no other ioctl or mixer flush interleaves).  Every entry is validated
 * first: a bad one fails the call with -EINVAL and nothing is applied.  A
 * ring error in the middle stops the batch; the entries before it stay
 * applied and `applied` says how many.  `applied` is written back on
 * success and on a ring error.
 *
 * hwid_lo/hwid_hi: the target unit's hardware id (ua_system_info.hw_uid_*);
 * 0/0 means "this node".  A non-zero id that is not this unit's fails with
 * -ENXIO.  flags must be 0.
 */
#define UA_BUS_PARAM_BATCH_MAX	64

struct ua_mixer_bus_entry {
	__u32 bus_id;		/* 0x00-0x1f */
	__u32 param;		/* 0-6, as ua_mixer_bus_param.sub_param */
	__u32 side;		/* 0 = left output weight, 1 = right */
	__u32 value;		/* IEEE 754 float as u32 */
};

struct ua_mixer_bus_params {
	__u32 count;		/* 1..UA_BUS_PARAM_BATCH_MAX */
	__u32 flags;		/* must be 0 */
	__u32 hwid_lo;
	__u32 hwid_hi;
	__u64 entries;		/* user pointer to count x ua_mixer_bus_entry */
	__u32 applied;		/* OUT: entries written to the DSP */
	__u32 reserved;
};

#define UA_IOCTL_SET_MIXER_BUS_PARAMS	_IOWR(UA_IOCTL_MAGIC, 0x3c, struct ua_mixer_bus_params)

/*
 * Event channel: read() on /dev/ua_apolloN returns struct ua_event records.
 *
 * Mirrors the kext's async notifications ({code, arg} through
 * CNotificationQueue, armed by selector 165).  poll()/select()/epoll and
 * O_NONBLOCK work; SIGIO via fcntl(F_SETFL, O_ASYNC) too.
 *
 * Each open file has its own queue of 64 events.  read() needs a buffer of
 * at least sizeof(struct ua_event) bytes and returns as many whole events
 * as fit; it blocks until one is queued (-EAGAIN with O_NONBLOCK).  Files
 * that never read lose nothing but their own oldest events.  A reader that
 * falls behind finds a UA_EVT_EVENTS_LOST event (arg = events dropped since
 * the last one delivered) in front of the next events it gets.
 *
 * Events are queued from the time the file is opened.  After the device is
 * removed (unbind, surprise removal) UA_EVT_DEVICE_OFFLINE is the last event;
 * once the queue is drained read() fails with -ENODEV and poll() reports
 * EPOLLHUP.
 *
 * time_ns is CLOCK_MONOTONIC.
 *
 * Codes 0x64-0x6f are the kext's values; arg means what it does there:
 *   UA_EVT_RATE_INFO     0x64  firmware rate/lock info changed (notification
 *                              bit 4); arg = raw notification status
 *   UA_EVT_IODESC_IN     0x65  the firmware republished the input IO
 *                              descriptor (bit 1); arg = raw status
 *   UA_EVT_IODESC_OUT    0x66  ... the output one (bit 0); arg = raw status
 *   UA_EVT_RATE_CHANGED  0x67  the clock now runs at a new rate; arg = rate
 *                              index (UA_RATE_44100..UA_RATE_192000: 1 =
 *                              44.1k, 2 = 48k, 3 = 88.2k, 4 = 96k, 5 =
 *                              176.4k, 6 = 192k).  From a committed rate
 *                              change (ALSA hw_params/prepare, ioctl, mixer
 *                              control) and from notification bits 22 / 4.
 *   UA_EVT_CLOCK_CHANGED 0x68  clock source change committed; arg = source
 *                              (UA_CLOCK_*)
 *   UA_EVT_NOTIF_STATUS  0x6e  raw notification status (BAR0 0xC030) as the
 *                              driver read and cleared it; arg = the status.
 *                              Posted before the decoded events above.
 *   UA_EVT_CLOCK_MASTER  0x6f  this unit became the group's master (the
 *                              previous master was removed); arg = 0.  Posted
 *                              to the new master's files only.
 * The kext's 0x69 (additional latency) is not produced: no equivalent
 * state here.
 *
 * Linux-only codes:
 *   UA_EVT_DEVICE_OFFLINE 0x1000  the device was removed; arg = 1 when it
 *                                 was already gone from the bus (surprise
 *                                 removal), 0 for an orderly unbind
 *   UA_EVT_CONNECTED      0x1001  the audio extension connected (probe,
 *                                 resume, reconnect); arg = 0
 *   UA_EVT_LAYOUT_CHANGED 0x1002  an io_map layout was committed; arg =
 *                                 (play_hw_channels << 16) | rec_hw_channels
 *   UA_EVT_EVENTS_LOST    0x1003  see above
 *   UA_EVT_CLOCK_LOCK     0x1004  the clock lock changed; arg = 1 locked,
 *                                 0 not locked.  Locked is the kext's
 *                                 definition, (BAR0 0xC07C & 3) == 1 (the
 *                                 Mixer Engine's GetLockedState), polled
 *                                 with the DSP service loop.  Not posted for
 *                                 the first reading after a connect, only
 *                                 for changes; the ALSA boolean "Clock
 *                                 Locked" carries the same state.
 *   UA_EVT_PAUSED         0x1005  audio on this unit must stop (the kext's
 *                                 0x264); arg = the UA_PAUSE_* bit that was
 *                                 set: UA_PAUSE_RATE around a rate/clock
 *                                 change, UA_PAUSE_DEVICE on system suspend
 *                                 and before a unit is removed
 *   UA_EVT_RESUMED        0x1006  that reason is gone (the kext's 0x265);
 *                                 arg = the bit that was cleared.  Streaming
 *                                 may restart when GET_PAUSE_INFO.can_stream
 *                                 is 1.  A removed unit gets no RESUMED
 *                                 (DEVICE_OFFLINE follows its PAUSED)
 *
 *   UA_EVT_DEVICE_ERROR   0x1007  the driver counted an error; arg = the
 *                                 UA_ERR_* class below.  At most one event
 *                                 per class per second; the counters
 *                                 themselves are in /proc/asound/cardN/
 *                                 dsp_state ("error counters")
 *
 * PAUSED / RESUMED are posted when a reason bit changes, so they come in
 * pairs per reason.
 *
 * UA_EVT_DEVICE_ERROR classes (arg):
 *   UA_ERR_XRUN_STOP   1  the driver forced a running stream into XRUN
 *                         (period watchdog, dead link, vanished transport)
 *   UA_ERR_PCIE        2  the PCIe core reported an error (AER
 *                         error_detected)
 *   UA_ERR_AX_FAULT    3  AX_CONTROL fault flag bit 7 or 8 seen
 *   UA_ERR_DSP_VECTOR  4  a DSP error vector (DSPEvent 7/8) fired
 *   UA_ERR_BLOCK_SEND  5  a DSP block/firmware send ended with a non-zero
 *                         kext result code (including timeouts)
 */
struct ua_event {
	__u32 code;
	__u32 arg;
	__u64 time_ns;
};

#define UA_EVT_RATE_INFO	0x64
#define UA_EVT_IODESC_IN	0x65
#define UA_EVT_IODESC_OUT	0x66
#define UA_EVT_RATE_CHANGED	0x67
#define UA_EVT_CLOCK_CHANGED	0x68
#define UA_EVT_NOTIF_STATUS	0x6e
#define UA_EVT_CLOCK_MASTER	0x6f

#define UA_EVT_DEVICE_OFFLINE	0x1000
#define UA_EVT_CONNECTED	0x1001
#define UA_EVT_LAYOUT_CHANGED	0x1002
#define UA_EVT_EVENTS_LOST	0x1003
#define UA_EVT_CLOCK_LOCK	0x1004
#define UA_EVT_PAUSED		0x1005
#define UA_EVT_RESUMED		0x1006
#define UA_EVT_DEVICE_ERROR	0x1007

#define UA_ERR_XRUN_STOP	1
#define UA_ERR_PCIE		2
#define UA_ERR_AX_FAULT		3
#define UA_ERR_DSP_VECTOR	4
#define UA_ERR_BLOCK_SEND	5

/*
 * Plug-in hosting (ABI 1.8, UA_FEAT2_PLUGIN_HOST).  Served only when the
 * module was loaded with plugin_host=1 (default off); otherwise both
 * ioctls fail with -EOPNOTSUPP.  Neither needs CAP_SYS_RAWIO: everything
 * that reaches the device is built by the driver (no address, command word
 * or code byte comes from user space; modules are read by the driver from
 * /lib/firmware/universal-audio/plugins/modules/<sha256>.bill).
 *
 * Numbers: 0x90 BOARD_QUERY, 0x94 PLUGIN_INSTANCE.  0x91 and 0x92 are never
 * assigned (an earlier prototype's registration and host-backing
 * transports); 0x93 is kept for a catalog ioctl.
 *
 * Licensing.  The driver hosts only plug-ins the device itself says are
 * authorized, and never anything else:
 *   - The device's answer is its board table: 768 words, the word for
 *     plug-in id R (0x800 <= R < 0xB00, the plug-in's render id) at index
 *     R - 0x800.  The driver asks for it with the date from the system
 *     clock (days since 2007-01-01 UTC << 12, plus the minute of the day);
 *     there is no way to give it another date.  The query updates the
 *     device's date as UA's own driver does on every query.
 *   - UA_IOCTL_BOARD_QUERY returns the table as the device sent it.  The
 *     driver keeps no copy: every query and every CREATE asks the device
 *     again, and a table is never reused across an epoch (a reconnect, a
 *     rate change, a cold start, a replug).
 *   - UA_PLUGIN_INST_CREATE reads a fresh table first, in the same
 *     operation and the same epoch as the upload that follows, and refuses
 *     (-EACCES, before anything is placed or sent) unless the word admits
 *     the plug-in, exactly as UA's SDK decides:
 *       0x81000000, 0x82000000   UA_PLUGIN_LICENSE_AUTHORIZED  admitted
 *       any other word but the   UA_PLUGIN_LICENSE_DEMO        admitted
 *         three below            (a demo is running)
 *       0x80000000               UA_PLUGIN_LICENSE_NOT_STARTED refused
 *       0x00000000               UA_PLUGIN_LICENSE_EXPIRED     refused
 *       0x83000000               UA_PLUGIN_LICENSE_UNAVAILABLE refused
 *     A query that fails, a reply that does not match, an id outside the
 *     table or an epoch that changed: refused too (UA_PLUGIN_LICENSE_UNKNOWN).
 *   - Nothing here starts a demo or changes what the device holds.  Demos
 *     are started in UA's own software.
 */

#define UA_BOARD_QUERY_WORDS	768
#define UA_BOARD_QUERY_BASE	0x800	/* words[i]: plug-in id 0x800 + i */

/*
 * UA_IOCTL_BOARD_QUERY: the device's board table, fresh.  Fill struct_size
 * and leave everything else 0.  On success words[] is the table exactly as
 * the device returned it, date the date word that went with the query and
 * epoch the plug-in host epoch it belongs to.  -EBUSY while another query
 * or plug-in operation runs, -ETIMEDOUT when the device did not answer in
 * 2 s, -EPROTO for an answer that is not the query's, -ESTALE when the
 * epoch changed meanwhile, -ERANGE when the system clock is before 2007.
 * Nothing is copied back on an error.
 */
struct ua_board_query {
	__u32 struct_size;	/* in: sizeof(struct ua_board_query) */
	__u32 flags;		/* in: 0 */
	__u32 date;		/* out: the date word sent with the query */
	__u32 reserved;		/* 0 */
	__u64 epoch;		/* out */
	__u32 words[UA_BOARD_QUERY_WORDS];	/* out */
};

#define UA_IOCTL_BOARD_QUERY	_IOWR(UA_IOCTL_MAGIC, 0x90, struct ua_board_query)

#define UA_PLUGIN_LICENSE_AUTHORIZED	0u
#define UA_PLUGIN_LICENSE_DEMO		1u
#define UA_PLUGIN_LICENSE_NOT_STARTED	2u
#define UA_PLUGIN_LICENSE_EXPIRED	3u
#define UA_PLUGIN_LICENSE_UNAVAILABLE	4u
#define UA_PLUGIN_LICENSE_UNKNOWN	0xffffffffu

/*
 * UA_IOCTL_PLUGIN_INSTANCE: plug-in instances, owned by the open file that
 * created them.  Closing the file destroys them; so does the device going
 * away.  A reconnect, a rate change or a cold start ends them too: their
 * handle then fails with -ESTALE and DESTROY just forgets it.
 *
 * UA_PLUGIN_INST_CREATE  in: render_id, dsp (UA_PLUGIN_DSP_ANY or an index),
 *   n_resources + resources (the plug-in's DSP modules by SHA-256),
 *   n_private + privates (its private memory rows, head row first with
 *   link 0xffffffff, every other row's link = row index << 24 | word
 *   offset), flags.  The device's authorization answer for render_id is read
 *   first (see above).  Then the modules are read from the module store and
 *   checked, placed on a DSP this driver owns (shared modules once per DSP),
 *   uploaded (each acknowledged by the DSP), and the private rows are
 *   zero-filled and linked.  Nothing is routed or started: the instance
 *   processes no audio yet.
 *   out: handle, dsp, epoch, license_state, license_word (the device's word
 *   for render_id), also on -EACCES.
 *   UA_PLUGIN_CREATE_F_CHECK: stop after the authorization answer (no
 *   instance, nothing placed or sent; handle 0).
 * UA_PLUGIN_INST_DESTROY  in: handle.  Releases what the instance placed:
 *   modules whose last user it was are released on the DSP, the memory goes
 *   back to the pools.
 *
 * Errors: -EOPNOTSUPP (plugin_host=0), -EINVAL, -EACCES (the device does not
 * authorize the plug-in), -ENOENT (a module is not in the store), -ENOSPC
 * (no owned DSP has room), -EBUSY, -ETIMEDOUT/-EPROTO (the device did not
 * answer as expected; the DSP is quarantined until the next cold start),
 * -ESTALE.
 */
#define UA_PLUGIN_INST_CREATE		1u
#define UA_PLUGIN_INST_DESTROY		2u

#define UA_PLUGIN_CREATE_F_CHECK	(1u << 0)

#define UA_PLUGIN_DSP_ANY		0xffffffffu
#define UA_PLUGIN_RES_MAX		64
#define UA_PLUGIN_PRIV_MAX		128

struct ua_plugin_resource {
	__u8 sha256[32];
};

struct ua_plugin_private {
	__u32 pool;		/* 1, 2 or 3 */
	__u32 dwords;
	__u32 link;		/* row << 24 | word offset; 0xffffffff: none */
	__u32 flags;		/* 0 or 2 */
};

/*
 * UA_PLUGIN_CREATE_F_START (bring-up, experimental): after the rows are
 * zero-filled and linked, one transaction stops whatever runs on the DSP
 * input, sends the parameter commands, starts the instance there (SYNCH
 * with the head row) and sends the input's span and route words.
 * reserved[0] = in1 | in2 << 16 (in2 0x20 = mono).
 * reserved[1] = command word count (4..UA_PLUGIN_CMD_WORDS_MAX),
 * reserved[2] / [3] = low / high half of a user pointer to the __u32
 * command words: UA's parameter-block WRITEs, as UA Mixer Engine leaves
 * them in the ParamBuffer (Mac E7 capture, UAD2System _setParamsAux):
 *   {0x0014LLLL, row << 24 | word, skip mask, L - 3 values}, L >= 4.
 * Row is a private row index.  The run starts at the word and spans
 * L - 3 + popcount(mask) words of the row; bit i of the mask leaves word
 * + i + 1 untouched (the masks UA sends skip exactly the words the memspec
 * links), so no value may land on a word a row links into.  Each goes out
 * with word 1 relocated to the row's DSP address (UAD2System
 * _fixParameterAddresses).
 * reserved[4] = pool 1 | pool 2 << 16 scratch words, reserved[5] = pool 3
 * (0: none): the chain's scratch (the largest of its descriptors'
 * AllocInfo 0xa9c / 0xa98 / 0xaa0), raised on the DSP before placement and
 * put back when the instance goes; needs an explicit dsp.
 * DESTROY and close stop it before anything is released.
 */
#define UA_PLUGIN_CREATE_F_START	(1u << 1)
#define UA_PLUGIN_CMD_WORDS_MAX		1016	/* one 4 KiB DMA page, room left */
#define UA_PLUGIN_CMD_WRITE		0x0014	/* opcode half of a parameter WRITE */

struct ua_plugin_instance {
	__u32 struct_size;	/* sizeof(struct ua_plugin_instance) */
	__u32 op;		/* UA_PLUGIN_INST_* */
	__u32 flags;		/* CREATE: UA_PLUGIN_CREATE_F_*; else 0 */
	__u32 handle;		/* CREATE: out; DESTROY: in */
	__u32 render_id;	/* CREATE: absolute plug-in id */
	__u32 dsp;		/* CREATE: in, then the DSP used */
	__u32 n_resources;	/* CREATE: 1..UA_PLUGIN_RES_MAX */
	__u32 n_private;	/* CREATE: 1..UA_PLUGIN_PRIV_MAX */
	__u64 resources;	/* CREATE: user pointer */
	__u64 privates;		/* CREATE: user pointer */
	__u64 epoch;		/* out */
	__u32 license_state;	/* out: UA_PLUGIN_LICENSE_* */
	__u32 license_word;	/* out */
	__u32 reserved[6];
};

#define UA_IOCTL_PLUGIN_INSTANCE _IOWR(UA_IOCTL_MAGIC, 0x94, struct ua_plugin_instance)

#endif /* UA_IOCTL_H */
