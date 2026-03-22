/* SPDX-License-Identifier: GPL-2.0-only */
/*
 * Universal Audio Apollo Thunderbolt - Routing Table Definitions
 *
 * Copyright (c) 2026 open-apollo contributors
 *
 * DMA channel routing tables for Apollo Thunderbolt interfaces.
 * DTrace-captured from macOS UAD2System.kext (SEL171 RT data).
 * 100% coverage of record and playback channel maps.
 *
 * Binary format (RT171 entry encoding):
 *   Header:    4 x u32  {unused, direction, unused, num_channels}
 *   Per-entry: 12 x u32 {session_id, 0, channel_id, type, name[8]}
 *
 * Names are ASCII strings stored as little-endian u32 words,
 * null-padded to 32 bytes (8 words).  session_id is captured as
 * 0x2f4240dd but is replaceable at runtime.
 */

#ifndef UA_ROUTING_H
#define UA_ROUTING_H

#include <linux/types.h>
#include "ua_apollo.h"

/* Routing table binary format constants */
#define UA_ROUTING_HEADER_WORDS		4
#define UA_ROUTING_ENTRY_WORDS		12
#define UA_ROUTING_ENTRY_SIZE		48	/* 12 x 4 bytes */
#define UA_ROUTING_TYPE			3	/* entry type field value */

/*
 * Routing configuration descriptor.
 * Points to the static const routing tables for a given device model.
 */
/* IO descriptor SRAM region is always 72 u32 words */
#define UA_IO_DESC_WORDS		72

struct ua_routing_config {
	const u32 *rec_table;		/* Record routing table (raw u32 array) */
	unsigned int rec_table_size;	/* Record table size in bytes */
	unsigned int rec_channels;	/* Number of record DMA channels */
	const u32 *play_table;		/* Playback routing table (raw u32 array) */
	unsigned int play_table_size;	/* Playback table size in bytes */
	unsigned int play_channels;	/* Number of playback DMA channels */
	const char * const *rec_names;	/* Short channel names for ALSA (record) */
	const char * const *play_names;	/* Short channel names for ALSA (playback) */
	const u32 *io_desc_input;	/* Input IO descriptor (72 x u32, macOS format) */
	const u32 *io_desc_output;	/* Output IO descriptor (72 x u32, macOS format) */
};

/*
 * Routing entry structure for parsing.
 * Maps directly onto the 12-word binary format used in RT171 data.
 */
struct ua_routing_entry {
	u32 session_id;		/* Session identifier (runtime-replaceable) */
	u32 zero;		/* Reserved, always 0 */
	u32 channel_id;		/* Channel type/index (high byte=type, low byte=index) */
	u32 type;		/* Entry type, always UA_ROUTING_TYPE (3) */
	u32 name[8];		/* ASCII name as LE u32 words, null-padded to 32 bytes */
};

/*
 * Apollo x4 Record Routing Table
 *
 * Direction: 1 (record / device-to-host)
 * Channels: 22
 * Size: 268 words = 1072 bytes
 *
 * DMA  ch_id   Name
 * ---  ------  --------------
 *  0   0x0001  MIC/LINE/HIZ 1
 *  1   0x0002  MIC/LINE/HIZ 2
 *  2   0x0103  MIC/LINE 3
 *  3   0x0104  MIC/LINE 4
 *  4   0x0501  S/PDIF L
 *  5   0x0502  S/PDIF R
 *  6   0x0701  VIRTUAL 1
 *  7   0x0702  VIRTUAL 2
 *  8   0x0703  VIRTUAL 3
 *  9   0x0704  VIRTUAL 4
 * 10   0x0705  VIRTUAL 5
 * 11   0x0706  VIRTUAL 6
 * 12   0x0707  VIRTUAL 7
 * 13   0x0708  VIRTUAL 8
 * 14   0x0901  MON L
 * 15   0x0902  MON R
 * 16   0x0201  AUX1 L
 * 17   0x0202  AUX1 R
 * 18   0x0203  AUX2 L
 * 19   0x0204  AUX2 R
 * 20   0x0b01  TALKBACK 1
 * 21   0x0b02  TALKBACK 2
 */
static const u32 ua_x4_rec_routing[] = {
	/* Header: unused, direction=1(record), unused, num_channels=22 */
	0x00000000, 0x00000001, 0x00000000, 0x00000016,
	/* DMA 0: ch_id=0x0001 "MIC/LINE/HIZ 1" */
	0x2f4240dd, 0x00000000, 0x00000001, 0x00000003,
	0x2f43494d, 0x454e494c, 0x5a49482f, 0x00003120,
	0x00000000, 0x00000000, 0x00000000, 0x00000000,
	/* DMA 1: ch_id=0x0002 "MIC/LINE/HIZ 2" */
	0x2f4240dd, 0x00000000, 0x00000002, 0x00000003,
	0x2f43494d, 0x454e494c, 0x5a49482f, 0x00003220,
	0x00000000, 0x00000000, 0x00000000, 0x00000000,
	/* DMA 2: ch_id=0x0103 "MIC/LINE 3" */
	0x2f4240dd, 0x00000000, 0x00000103, 0x00000003,
	0x2f43494d, 0x454e494c, 0x00003320, 0x00000000,
	0x00000000, 0x00000000, 0x00000000, 0x00000000,
	/* DMA 3: ch_id=0x0104 "MIC/LINE 4" */
	0x2f4240dd, 0x00000000, 0x00000104, 0x00000003,
	0x2f43494d, 0x454e494c, 0x00003420, 0x00000000,
	0x00000000, 0x00000000, 0x00000000, 0x00000000,
	/* DMA 4: ch_id=0x0501 "S/PDIF L" */
	0x2f4240dd, 0x00000000, 0x00000501, 0x00000003,
	0x44502f53, 0x4c204649, 0x00000000, 0x00000000,
	0x00000000, 0x00000000, 0x00000000, 0x00000000,
	/* DMA 5: ch_id=0x0502 "S/PDIF R" */
	0x2f4240dd, 0x00000000, 0x00000502, 0x00000003,
	0x44502f53, 0x52204649, 0x00000000, 0x00000000,
	0x00000000, 0x00000000, 0x00000000, 0x00000000,
	/* DMA 6: ch_id=0x0701 "VIRTUAL 1" */
	0x2f4240dd, 0x00000000, 0x00000701, 0x00000003,
	0x54524956, 0x204c4155, 0x00000031, 0x00000000,
	0x00000000, 0x00000000, 0x00000000, 0x00000000,
	/* DMA 7: ch_id=0x0702 "VIRTUAL 2" */
	0x2f4240dd, 0x00000000, 0x00000702, 0x00000003,
	0x54524956, 0x204c4155, 0x00000032, 0x00000000,
	0x00000000, 0x00000000, 0x00000000, 0x00000000,
	/* DMA 8: ch_id=0x0703 "VIRTUAL 3" */
	0x2f4240dd, 0x00000000, 0x00000703, 0x00000003,
	0x54524956, 0x204c4155, 0x00000033, 0x00000000,
	0x00000000, 0x00000000, 0x00000000, 0x00000000,
	/* DMA 9: ch_id=0x0704 "VIRTUAL 4" */
	0x2f4240dd, 0x00000000, 0x00000704, 0x00000003,
	0x54524956, 0x204c4155, 0x00000034, 0x00000000,
	0x00000000, 0x00000000, 0x00000000, 0x00000000,
	/* DMA 10: ch_id=0x0705 "VIRTUAL 5" */
	0x2f4240dd, 0x00000000, 0x00000705, 0x00000003,
	0x54524956, 0x204c4155, 0x00000035, 0x00000000,
	0x00000000, 0x00000000, 0x00000000, 0x00000000,
	/* DMA 11: ch_id=0x0706 "VIRTUAL 6" */
	0x2f4240dd, 0x00000000, 0x00000706, 0x00000003,
	0x54524956, 0x204c4155, 0x00000036, 0x00000000,
	0x00000000, 0x00000000, 0x00000000, 0x00000000,
	/* DMA 12: ch_id=0x0707 "VIRTUAL 7" */
	0x2f4240dd, 0x00000000, 0x00000707, 0x00000003,
	0x54524956, 0x204c4155, 0x00000037, 0x00000000,
	0x00000000, 0x00000000, 0x00000000, 0x00000000,
	/* DMA 13: ch_id=0x0708 "VIRTUAL 8" */
	0x2f4240dd, 0x00000000, 0x00000708, 0x00000003,
	0x54524956, 0x204c4155, 0x00000038, 0x00000000,
	0x00000000, 0x00000000, 0x00000000, 0x00000000,
	/* DMA 14: ch_id=0x0901 "MON L" */
	0x2f4240dd, 0x00000000, 0x00000901, 0x00000003,
	0x204e4f4d, 0x0000004c, 0x00000000, 0x00000000,
	0x00000000, 0x00000000, 0x00000000, 0x00000000,
	/* DMA 15: ch_id=0x0902 "MON R" */
	0x2f4240dd, 0x00000000, 0x00000902, 0x00000003,
	0x204e4f4d, 0x00000052, 0x00000000, 0x00000000,
	0x00000000, 0x00000000, 0x00000000, 0x00000000,
	/* DMA 16: ch_id=0x0201 "AUX1 L" */
	0x2f4240dd, 0x00000000, 0x00000201, 0x00000003,
	0x31585541, 0x00004c20, 0x00000000, 0x00000000,
	0x00000000, 0x00000000, 0x00000000, 0x00000000,
	/* DMA 17: ch_id=0x0202 "AUX1 R" */
	0x2f4240dd, 0x00000000, 0x00000202, 0x00000003,
	0x31585541, 0x00005220, 0x00000000, 0x00000000,
	0x00000000, 0x00000000, 0x00000000, 0x00000000,
	/* DMA 18: ch_id=0x0203 "AUX2 L" */
	0x2f4240dd, 0x00000000, 0x00000203, 0x00000003,
	0x32585541, 0x00004c20, 0x00000000, 0x00000000,
	0x00000000, 0x00000000, 0x00000000, 0x00000000,
	/* DMA 19: ch_id=0x0204 "AUX2 R" */
	0x2f4240dd, 0x00000000, 0x00000204, 0x00000003,
	0x32585541, 0x00005220, 0x00000000, 0x00000000,
	0x00000000, 0x00000000, 0x00000000, 0x00000000,
	/* DMA 20: ch_id=0x0b01 "TALKBACK 1" */
	0x2f4240dd, 0x00000000, 0x00000b01, 0x00000003,
	0x4b4c4154, 0x4b434142, 0x00003120, 0x00000000,
	0x00000000, 0x00000000, 0x00000000, 0x00000000,
	/* DMA 21: ch_id=0x0b02 "TALKBACK 2" */
	0x2f4240dd, 0x00000000, 0x00000b02, 0x00000003,
	0x4b4c4154, 0x4b434142, 0x00003220, 0x00000000,
	0x00000000, 0x00000000, 0x00000000, 0x00000000,
};

/*
 * Apollo x4 Playback Routing Table
 *
 * Direction: 0 (playback / host-to-device)
 * Channels: 24
 * Size: 292 words = 1168 bytes
 *
 * DMA  ch_id   Name
 * ---  ------  ----------
 *  0   0x0901  MON L
 *  1   0x0902  MON R
 *  2   0x0301  LINE 1
 *  3   0x0302  LINE 2
 *  4   0x0303  LINE 3
 *  5   0x0304  LINE 4
 *  6   0x0501  S/PDIF L
 *  7   0x0502  S/PDIF R
 *  8   0x0701  VIRTUAL 1
 *  9   0x0702  VIRTUAL 2
 * 10   0x0703  VIRTUAL 3
 * 11   0x0704  VIRTUAL 4
 * 12   0x0705  VIRTUAL 5
 * 13   0x0706  VIRTUAL 6
 * 14   0x0707  VIRTUAL 7
 * 15   0x0708  VIRTUAL 8
 * 16   0x0a01  CUE1 L
 * 17   0x0a02  CUE1 R
 * 18   0x0a03  CUE2 L
 * 19   0x0a04  CUE2 R
 * 20   0x0a05  CUE3 L
 * 21   0x0a06  CUE3 R
 * 22   0x0a07  CUE4 L
 * 23   0x0a08  CUE4 R
 */
static const u32 ua_x4_play_routing[] = {
	/* Header: unused, direction=0(playback), unused, num_channels=24 */
	0x00000000, 0x00000000, 0x00000000, 0x00000018,
	/* DMA 0: ch_id=0x0901 "MON L" */
	0x2f4240dd, 0x00000000, 0x00000901, 0x00000003,
	0x204e4f4d, 0x0000004c, 0x00000000, 0x00000000,
	0x00000000, 0x00000000, 0x00000000, 0x00000000,
	/* DMA 1: ch_id=0x0902 "MON R" */
	0x2f4240dd, 0x00000000, 0x00000902, 0x00000003,
	0x204e4f4d, 0x00000052, 0x00000000, 0x00000000,
	0x00000000, 0x00000000, 0x00000000, 0x00000000,
	/* DMA 2: ch_id=0x0301 "LINE 1" */
	0x2f4240dd, 0x00000000, 0x00000301, 0x00000003,
	0x454e494c, 0x00003120, 0x00000000, 0x00000000,
	0x00000000, 0x00000000, 0x00000000, 0x00000000,
	/* DMA 3: ch_id=0x0302 "LINE 2" */
	0x2f4240dd, 0x00000000, 0x00000302, 0x00000003,
	0x454e494c, 0x00003220, 0x00000000, 0x00000000,
	0x00000000, 0x00000000, 0x00000000, 0x00000000,
	/* DMA 4: ch_id=0x0303 "LINE 3" */
	0x2f4240dd, 0x00000000, 0x00000303, 0x00000003,
	0x454e494c, 0x00003320, 0x00000000, 0x00000000,
	0x00000000, 0x00000000, 0x00000000, 0x00000000,
	/* DMA 5: ch_id=0x0304 "LINE 4" */
	0x2f4240dd, 0x00000000, 0x00000304, 0x00000003,
	0x454e494c, 0x00003420, 0x00000000, 0x00000000,
	0x00000000, 0x00000000, 0x00000000, 0x00000000,
	/* DMA 6: ch_id=0x0501 "S/PDIF L" */
	0x2f4240dd, 0x00000000, 0x00000501, 0x00000003,
	0x44502f53, 0x4c204649, 0x00000000, 0x00000000,
	0x00000000, 0x00000000, 0x00000000, 0x00000000,
	/* DMA 7: ch_id=0x0502 "S/PDIF R" */
	0x2f4240dd, 0x00000000, 0x00000502, 0x00000003,
	0x44502f53, 0x52204649, 0x00000000, 0x00000000,
	0x00000000, 0x00000000, 0x00000000, 0x00000000,
	/* DMA 8: ch_id=0x0701 "VIRTUAL 1" */
	0x2f4240dd, 0x00000000, 0x00000701, 0x00000003,
	0x54524956, 0x204c4155, 0x00000031, 0x00000000,
	0x00000000, 0x00000000, 0x00000000, 0x00000000,
	/* DMA 9: ch_id=0x0702 "VIRTUAL 2" */
	0x2f4240dd, 0x00000000, 0x00000702, 0x00000003,
	0x54524956, 0x204c4155, 0x00000032, 0x00000000,
	0x00000000, 0x00000000, 0x00000000, 0x00000000,
	/* DMA 10: ch_id=0x0703 "VIRTUAL 3" */
	0x2f4240dd, 0x00000000, 0x00000703, 0x00000003,
	0x54524956, 0x204c4155, 0x00000033, 0x00000000,
	0x00000000, 0x00000000, 0x00000000, 0x00000000,
	/* DMA 11: ch_id=0x0704 "VIRTUAL 4" */
	0x2f4240dd, 0x00000000, 0x00000704, 0x00000003,
	0x54524956, 0x204c4155, 0x00000034, 0x00000000,
	0x00000000, 0x00000000, 0x00000000, 0x00000000,
	/* DMA 12: ch_id=0x0705 "VIRTUAL 5" */
	0x2f4240dd, 0x00000000, 0x00000705, 0x00000003,
	0x54524956, 0x204c4155, 0x00000035, 0x00000000,
	0x00000000, 0x00000000, 0x00000000, 0x00000000,
	/* DMA 13: ch_id=0x0706 "VIRTUAL 6" */
	0x2f4240dd, 0x00000000, 0x00000706, 0x00000003,
	0x54524956, 0x204c4155, 0x00000036, 0x00000000,
	0x00000000, 0x00000000, 0x00000000, 0x00000000,
	/* DMA 14: ch_id=0x0707 "VIRTUAL 7" */
	0x2f4240dd, 0x00000000, 0x00000707, 0x00000003,
	0x54524956, 0x204c4155, 0x00000037, 0x00000000,
	0x00000000, 0x00000000, 0x00000000, 0x00000000,
	/* DMA 15: ch_id=0x0708 "VIRTUAL 8" */
	0x2f4240dd, 0x00000000, 0x00000708, 0x00000003,
	0x54524956, 0x204c4155, 0x00000038, 0x00000000,
	0x00000000, 0x00000000, 0x00000000, 0x00000000,
	/* DMA 16: ch_id=0x0a01 "CUE1 L" */
	0x2f4240dd, 0x00000000, 0x00000a01, 0x00000003,
	0x31455543, 0x00004c20, 0x00000000, 0x00000000,
	0x00000000, 0x00000000, 0x00000000, 0x00000000,
	/* DMA 17: ch_id=0x0a02 "CUE1 R" */
	0x2f4240dd, 0x00000000, 0x00000a02, 0x00000003,
	0x31455543, 0x00005220, 0x00000000, 0x00000000,
	0x00000000, 0x00000000, 0x00000000, 0x00000000,
	/* DMA 18: ch_id=0x0a03 "CUE2 L" */
	0x2f4240dd, 0x00000000, 0x00000a03, 0x00000003,
	0x32455543, 0x00004c20, 0x00000000, 0x00000000,
	0x00000000, 0x00000000, 0x00000000, 0x00000000,
	/* DMA 19: ch_id=0x0a04 "CUE2 R" */
	0x2f4240dd, 0x00000000, 0x00000a04, 0x00000003,
	0x32455543, 0x00005220, 0x00000000, 0x00000000,
	0x00000000, 0x00000000, 0x00000000, 0x00000000,
	/* DMA 20: ch_id=0x0a05 "CUE3 L" */
	0x2f4240dd, 0x00000000, 0x00000a05, 0x00000003,
	0x33455543, 0x00004c20, 0x00000000, 0x00000000,
	0x00000000, 0x00000000, 0x00000000, 0x00000000,
	/* DMA 21: ch_id=0x0a06 "CUE3 R" */
	0x2f4240dd, 0x00000000, 0x00000a06, 0x00000003,
	0x33455543, 0x00005220, 0x00000000, 0x00000000,
	0x00000000, 0x00000000, 0x00000000, 0x00000000,
	/* DMA 22: ch_id=0x0a07 "CUE4 L" */
	0x2f4240dd, 0x00000000, 0x00000a07, 0x00000003,
	0x34455543, 0x00004c20, 0x00000000, 0x00000000,
	0x00000000, 0x00000000, 0x00000000, 0x00000000,
	/* DMA 23: ch_id=0x0a08 "CUE4 R" */
	0x2f4240dd, 0x00000000, 0x00000a08, 0x00000003,
	0x34455543, 0x00005220, 0x00000000, 0x00000000,
	0x00000000, 0x00000000, 0x00000000, 0x00000000,
};

/*
 * Short channel names for ALSA channel map / mixer display.
 * Indexed by DMA channel number within record or playback direction.
 */
static const char * const ua_x4_rec_names[22] = {
	"Mic 1",	/* DMA 0:  MIC/LINE/HIZ 1 */
	"Mic 2",	/* DMA 1:  MIC/LINE/HIZ 2 */
	"Line 3",	/* DMA 2:  MIC/LINE 3 */
	"Line 4",	/* DMA 3:  MIC/LINE 4 */
	"SPDIF L",	/* DMA 4:  S/PDIF L */
	"SPDIF R",	/* DMA 5:  S/PDIF R */
	"Virt 1",	/* DMA 6:  VIRTUAL 1 */
	"Virt 2",	/* DMA 7:  VIRTUAL 2 */
	"Virt 3",	/* DMA 8:  VIRTUAL 3 */
	"Virt 4",	/* DMA 9:  VIRTUAL 4 */
	"Virt 5",	/* DMA 10: VIRTUAL 5 */
	"Virt 6",	/* DMA 11: VIRTUAL 6 */
	"Virt 7",	/* DMA 12: VIRTUAL 7 */
	"Virt 8",	/* DMA 13: VIRTUAL 8 */
	"Mon L",	/* DMA 14: MON L */
	"Mon R",	/* DMA 15: MON R */
	"Aux1 L",	/* DMA 16: AUX1 L */
	"Aux1 R",	/* DMA 17: AUX1 R */
	"Aux2 L",	/* DMA 18: AUX2 L */
	"Aux2 R",	/* DMA 19: AUX2 R */
	"TB 1",		/* DMA 20: TALKBACK 1 */
	"TB 2",		/* DMA 21: TALKBACK 2 */
};

static const char * const ua_x4_play_names[24] = {
	"Mon L",	/* DMA 0:  MON L */
	"Mon R",	/* DMA 1:  MON R */
	"Line 1",	/* DMA 2:  LINE 1 */
	"Line 2",	/* DMA 3:  LINE 2 */
	"Line 3",	/* DMA 4:  LINE 3 */
	"Line 4",	/* DMA 5:  LINE 4 */
	"SPDIF L",	/* DMA 6:  S/PDIF L */
	"SPDIF R",	/* DMA 7:  S/PDIF R */
	"Virt 1",	/* DMA 8:  VIRTUAL 1 */
	"Virt 2",	/* DMA 9:  VIRTUAL 2 */
	"Virt 3",	/* DMA 10: VIRTUAL 3 */
	"Virt 4",	/* DMA 11: VIRTUAL 4 */
	"Virt 5",	/* DMA 12: VIRTUAL 5 */
	"Virt 6",	/* DMA 13: VIRTUAL 6 */
	"Virt 7",	/* DMA 14: VIRTUAL 7 */
	"Virt 8",	/* DMA 15: VIRTUAL 8 */
	"Cue1 L",	/* DMA 16: CUE1 L */
	"Cue1 R",	/* DMA 17: CUE1 R */
	"Cue2 L",	/* DMA 18: CUE2 L */
	"Cue2 R",	/* DMA 19: CUE2 R */
	"Cue3 L",	/* DMA 20: CUE3 L */
	"Cue3 R",	/* DMA 21: CUE3 R */
	"Cue4 L",	/* DMA 22: CUE4 L */
	"Cue4 R",	/* DMA 23: CUE4 R */
};

/*
 * Apollo x4 IO Descriptor SRAM Data
 *
 * Captured from macOS warm boot BAR0 dump.
 * Format: 6-word header + packed 16-bit channel pairs + 0x00FF00FF padding.
 *
 * Header layout:
 *   [0] = 1 (version)
 *   [1] = 0x46 (flags)
 *   [2] = 0xFFFFFFFF (sentinel)
 *   [3] = input_channel_count (input) or 0 (output)
 *   [4] = 0 (input) or output_channel_count (output)
 *   [5] = 2 (input) or 0 (output)
 *
 * Channel pairs: {ch_hi << 16 | ch_lo} — two 16-bit channel IDs per word.
 * Includes DSP-internal channels (types 0x03, 0x04, 0x0C) beyond DMA channels.
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

/* Apollo x4 routing configuration */
static const struct ua_routing_config ua_x4_routing_config = {
	.rec_table      = ua_x4_rec_routing,
	.rec_table_size = sizeof(ua_x4_rec_routing),
	.rec_channels   = 22,
	.play_table      = ua_x4_play_routing,
	.play_table_size = sizeof(ua_x4_play_routing),
	.play_channels   = 24,
	.rec_names       = ua_x4_rec_names,
	.play_names      = ua_x4_play_names,
	.io_desc_input   = ua_x4_io_desc_input,
	.io_desc_output  = ua_x4_io_desc_output,
};

/*
 * ua_get_routing_config - Return routing config for a given device type.
 * @device_type: UA_DEV_APOLLO_* constant from ua_apollo.h
 *
 * Returns pointer to static routing config, or NULL if the device type
 * has no routing table defined yet.
 */
static inline const struct ua_routing_config *
ua_get_routing_config(u32 device_type)
{
	switch (device_type) {
	case UA_DEV_APOLLO_X4:
		return &ua_x4_routing_config;
	default:
		return NULL;
	}
}

#endif /* UA_ROUTING_H */
