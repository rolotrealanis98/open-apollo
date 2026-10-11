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
 * Driver parameter (kext selector 166 / 0xA6, SetDriverParameter).
 * Controls sample rate, clock source, etc.
 * Input size: 0x10 (16 bytes).
 */
struct ua_driver_param {
	__u32 param_id;		/* Driver parameter enum */
	__u32 reserved;
	__u64 value;		/* 64-bit parameter value */
};

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

/*
 * RETIRED: the number stays reserved and the ioctl returns -ENOTTY.  The
 * kext loads firmware to DSP 0 only; DSPs 1+ boot themselves.
 */
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
 * The kext's 0x69 (additional latency) and 0x6f (clock master) are not
 * produced: no equivalent state here.
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

#define UA_EVT_DEVICE_OFFLINE	0x1000
#define UA_EVT_CONNECTED	0x1001
#define UA_EVT_LAYOUT_CHANGED	0x1002
#define UA_EVT_EVENTS_LOST	0x1003

#endif /* UA_IOCTL_H */
