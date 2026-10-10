/* SPDX-License-Identifier: GPL-2.0-only */
/*
 * Universal Audio Apollo Thunderbolt - captured IO descriptors
 *
 * Copyright (c) 2026 apollo-linux contributors
 *
 * The firmware publishes one IO descriptor per direction at BAR0+0xC1A4
 * (input) / 0xC2C4 (output); layout in ua_apollo.h (UA_IO_DESC_*).  These
 * are copies read back on running units, used only by the legacy
 * UA_IOCTL_SEND_ROUTING experiment (ua_dsp_send_routing), which writes
 * them over the firmware's own.  The kext never writes these areas.
 *
 * Per-model routing (the channel-ID order macOS exposes) is no longer
 * hand-captured here: it is generated from devices/<model>.json into
 * ua_models_gen.h, and the generated x4 (S/PDIF mode) and x8p (ADAT mode)
 * tables reproduce the SEL171 RT171 captures this file used to carry.
 */

#ifndef UA_ROUTING_H
#define UA_ROUTING_H

#include <linux/types.h>
#include "ua_apollo.h"

struct ua_routing_config {
	const u32 *io_desc_input;	/* Input IO descriptor (72 x u32) */
	const u32 *io_desc_output;	/* Output IO descriptor (72 x u32) */
};

/*
 * Apollo x4 IO Descriptor SRAM Data
 *
 * Captured from macOS warm boot BAR0 dump (2026-03-18).
 * Format: 6-word header + packed 16-bit channel pairs + 0x00FF00FF padding.
 *
 * Header layout:
 *   [0] = 1 (version)
 *   [1] = 0x46 (words that follow)
 *   [2] = 0xFFFFFFFF (sentinel)
 *   [3] = input_channel_count (input) or 0 (output)
 *   [4] = 0 (input) or output_channel_count (output)
 *   [5] = 2 (input) or 0 (output)
 *
 * Channel pairs: {ch_hi << 16 | ch_lo} — two 16-bit channel IDs per word,
 * low half first.  Lists every DMA slot, including ones no route uses
 * (LINE 5-8 0x03, ADAT 0x04 in S/PDIF mode, RESERVED 0x0C).
 */
static const u32 ua_x4_io_desc_input[UA_IO_DESC_WORDS] = {
	/* Header */
	0x00000001, 0x00000046, 0xFFFFFFFF,
	0x00000022, /* 34 channels (17 pairs) */
	0x00000000,
	0x00000002,
	/* Packed channel pairs (17 pairs) */
	0x00020001, /* Mic 1/2 */
	0x01040103, /* Mic 3/4 */
	0x03060305, /* Line 5/6 (DSP internal) */
	0x03080307, /* Line 7/8 (DSP internal) */
	0x04020401, /* type 0x04 ch 1/2 (DSP internal) */
	0x04040403, /* type 0x04 ch 3/4 */
	0x04060405, /* type 0x04 ch 5/6 */
	0x04080407, /* type 0x04 ch 7/8 */
	0x05020501, /* S/PDIF L/R */
	0x07020701, /* Virtual 1/2 */
	0x07040703, /* Virtual 3/4 */
	0x07060705, /* Virtual 5/6 */
	0x07080707, /* Virtual 7/8 */
	0x09020901, /* Mon L/R */
	0x02020201, /* AUX1 L/R */
	0x02040203, /* AUX2 L/R */
	0x0B020B01, /* Talkback 1/2 */
	/* Padding (words 23-71) */
	0x00FF00FF, 0x00FF00FF, 0x00FF00FF, 0x00FF00FF,
	0x00FF00FF, 0x00FF00FF, 0x00FF00FF, 0x00FF00FF,
	0x00FF00FF, 0x00FF00FF, 0x00FF00FF, 0x00FF00FF,
	0x00FF00FF, 0x00FF00FF, 0x00FF00FF, 0x00FF00FF,
	0x00FF00FF, 0x00FF00FF, 0x00FF00FF, 0x00FF00FF,
	0x00FF00FF, 0x00FF00FF, 0x00FF00FF, 0x00FF00FF,
	0x00FF00FF, 0x00FF00FF, 0x00FF00FF, 0x00FF00FF,
	0x00FF00FF, 0x00FF00FF, 0x00FF00FF, 0x00FF00FF,
	0x00FF00FF, 0x00FF00FF, 0x00FF00FF, 0x00FF00FF,
	0x00FF00FF, 0x00FF00FF, 0x00FF00FF, 0x00FF00FF,
	0x00FF00FF, 0x00FF00FF, 0x00FF00FF, 0x00FF00FF,
	0x00FF00FF, 0x00FF00FF, 0x00FF00FF,
	/* Words 70-71 */
	0x00000000, 0x00000000,
};

static const u32 ua_x4_io_desc_output[UA_IO_DESC_WORDS] = {
	/* Header */
	0x00000001, 0x00000046, 0xFFFFFFFF,
	0x00000000,
	0x00000030, /* 48 channels (24 pairs) */
	0x00000000,
	/* Packed channel pairs (24 pairs) */
	0x09020901, /* Mon L/R */
	0x03020301, /* Line 1/2 */
	0x03040303, /* Line 3/4 */
	0x03060305, /* Line 5/6 (DSP internal) */
	0x03080307, /* Line 7/8 (DSP internal) */
	0x04020401, /* type 0x04 ch 1/2 (DSP internal) */
	0x04040403, /* type 0x04 ch 3/4 */
	0x04060405, /* type 0x04 ch 5/6 */
	0x04080407, /* type 0x04 ch 7/8 */
	0x05020501, /* S/PDIF L/R */
	0x07020701, /* Virtual 1/2 */
	0x07040703, /* Virtual 3/4 */
	0x07060705, /* Virtual 5/6 */
	0x07080707, /* Virtual 7/8 */
	0x0A020A01, /* CUE1 L/R */
	0x0A040A03, /* CUE2 L/R */
	0x0A060A05, /* CUE3 L/R */
	0x0A080A07, /* CUE4 L/R */
	0x0C020C01, /* type 0x0C ch 1/2 (DSP internal) */
	0x0C040C03, /* type 0x0C ch 3/4 */
	0x0C060C05, /* type 0x0C ch 5/6 */
	0x0C080C07, /* type 0x0C ch 7/8 */
	0x0C0A0C09, /* type 0x0C ch 9/10 */
	0x0C0C0C0B, /* type 0x0C ch 11/12 */
	/* Padding (words 30-71) */
	0x00FF00FF, 0x00FF00FF, 0x00FF00FF, 0x00FF00FF,
	0x00FF00FF, 0x00FF00FF, 0x00FF00FF, 0x00FF00FF,
	0x00FF00FF, 0x00FF00FF, 0x00FF00FF, 0x00FF00FF,
	0x00FF00FF, 0x00FF00FF, 0x00FF00FF, 0x00FF00FF,
	0x00FF00FF, 0x00FF00FF, 0x00FF00FF, 0x00FF00FF,
	0x00FF00FF, 0x00FF00FF, 0x00FF00FF, 0x00FF00FF,
	0x00FF00FF, 0x00FF00FF, 0x00FF00FF, 0x00FF00FF,
	0x00FF00FF, 0x00FF00FF, 0x00FF00FF, 0x00FF00FF,
	0x00FF00FF, 0x00FF00FF, 0x00FF00FF, 0x00FF00FF,
	0x00FF00FF, 0x00FF00FF, 0x00FF00FF, 0x00FF00FF,
	/* Words 70-71 */
	0x00000000, 0x00000000,
};

/*
 * Apollo x8p (device type 0x22) IO descriptors, read back over the
 * driver's READ_REG ioctl from 0xC1A4 / 0xC2C4 on a running unit
 * (see tools/read-io-descriptors.py; FPGA 0xa241c5ac, 6 DSPs, FW v2).
 * 34 input and 52 output slots.
 */
static const u32 ua_x8p_io_desc_input[UA_IO_DESC_WORDS] = {
	/* Header */
	0x00000001, 0x00000046, 0xFFFFFFFF,
	0x00000022, /* 34 channels (17 pairs) */
	0x00000000,
	0x00000002,
	/* Packed channel pairs (17 pairs) */
	0x00020001, /* MIC/LINE/HIZ 1 / MIC/LINE/HIZ 2 */
	0x01040103, /* MIC/LINE 3 / MIC/LINE 4 */
	0x03060305, /* LINE 5 / LINE 6 */
	0x03080307, /* LINE 7 / LINE 8 */
	0x04020401, /* ADAT 1 / ADAT 2 */
	0x04040403, /* ADAT 3 / ADAT 4 */
	0x04060405, /* ADAT 5 / ADAT 6 */
	0x04080407, /* ADAT 7 / ADAT 8 */
	0x05020501, /* S/PDIF L / S/PDIF R */
	0x07020701, /* VIRTUAL 1 / VIRTUAL 2 */
	0x07040703, /* VIRTUAL 3 / VIRTUAL 4 */
	0x07060705, /* VIRTUAL 5 / VIRTUAL 6 */
	0x07080707, /* VIRTUAL 7 / VIRTUAL 8 */
	0x09020901, /* MON L / MON R */
	0x02020201, /* AUX1 L / AUX1 R */
	0x02040203, /* AUX2 L / AUX2 R */
	0x0B020B01, /* TALKBACK 1 / TALKBACK 2 */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	/* padding */
	0x00000000, 0x00000000,
};

static const u32 ua_x8p_io_desc_output[UA_IO_DESC_WORDS] = {
	/* Header */
	0x00000001, 0x00000046, 0xFFFFFFFF,
	0x00000000,
	0x00000034, /* 52 channels (26 pairs) */
	0x00000000,
	/* Packed channel pairs (26 pairs) */
	0x09020901, /* MON L / MON R */
	0x03020301, /* LINE 1 / LINE 2 */
	0x03040303, /* LINE 3 / LINE 4 */
	0x03060305, /* LINE 5 / LINE 6 */
	0x03080307, /* LINE 7 / LINE 8 */
	0x04020401, /* ADAT 1 / ADAT 2 */
	0x04040403, /* ADAT 3 / ADAT 4 */
	0x04060405, /* ADAT 5 / ADAT 6 */
	0x04080407, /* ADAT 7 / ADAT 8 */
	0x05020501, /* S/PDIF L / S/PDIF R */
	0x07020701, /* VIRTUAL 1 / VIRTUAL 2 */
	0x07040703, /* VIRTUAL 3 / VIRTUAL 4 */
	0x07060705, /* VIRTUAL 5 / VIRTUAL 6 */
	0x07080707, /* VIRTUAL 7 / VIRTUAL 8 */
	0x0A020A01, /* CUE1 L / CUE1 R */
	0x0A040A03, /* CUE2 L / CUE2 R */
	0x0A060A05, /* CUE3 L / CUE3 R */
	0x0A080A07, /* CUE4 L / CUE4 R */
	0x0C020C01, /* 0x0c 1 / 0x0c 2 */
	0x0C040C03, /* 0x0c 3 / 0x0c 4 */
	0x0C060C05, /* 0x0c 5 / 0x0c 6 */
	0x0C080C07, /* 0x0c 7 / 0x0c 8 */
	0x0C0A0C09, /* 0x0c 9 / 0x0c 10 */
	0x0C0C0C0B, /* 0x0c 11 / 0x0c 12 */
	0x0C0E0C0D, /* 0x0c 13 / 0x0c 14 */
	0x0C100C0F, /* 0x0c 15 / 0x0c 16 */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	0x00FF00FF, /* unused */
	/* padding */
	0x00000000, 0x00000000,
};

static const struct ua_routing_config ua_x4_routing_config = {
	.io_desc_input   = ua_x4_io_desc_input,
	.io_desc_output  = ua_x4_io_desc_output,
};

static const struct ua_routing_config ua_x8p_routing_config = {
	.io_desc_input   = ua_x8p_io_desc_input,
	.io_desc_output  = ua_x8p_io_desc_output,
};

/*
 * ua_get_routing_config - Captured IO descriptors for a device type.
 * @device_type: UA_DEV_APOLLO_* constant from ua_apollo.h
 *
 * Returns NULL when none were captured for this model.
 */
static inline const struct ua_routing_config *
ua_get_routing_config(u32 device_type)
{
	switch (device_type) {
	case UA_DEV_APOLLO_X4:
		return &ua_x4_routing_config;
	case UA_DEV_APOLLO_X8P:
		return &ua_x8p_routing_config;
	default:
		return NULL;
	}
}

#endif /* UA_ROUTING_H */
