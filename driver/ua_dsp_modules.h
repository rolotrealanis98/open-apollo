/* SPDX-License-Identifier: GPL-2.0-only */
/*
 * DSP module SRAM configuration data for Apollo x4
 *
 * Captured from Windows UAD driver ring buffer (DSP 0).
 * Each module gets: SRAM clears → DMA header → DMA I/O descriptor.
 * After all modules: EnableSynchProcessing + RoutingEnable commands.
 *
 * Reference: tools/captures/win-dsp0-ring-complete.json
 */

#ifndef UA_DSP_MODULES_H
#define UA_DSP_MODULES_H

#include <linux/types.h>

/* Number of modules configured in the DSP 0 connect sequence */
#define UA_MODULE_COUNT		6

/* Common constants across all modules */
#define UA_MOD_TOTAL_CH		0x22	/* 34 channels */
#define UA_MOD_BLOCK_SIZE	0x100	/* 256 samples */
#define UA_MOD_LATENCY		0xC8	/* 200 samples */
#define UA_MOD_RATE_RECIP	0x3A924925  /* float 1/(44100*stride) */
#define UA_MOD_FILTER_PARAM	0x3B83126F  /* float 0.004 */

/* Module entry points */
#define UA_MOD_EP_MIXER		0x8A5	/* Mixer core (module 0) */
#define UA_MOD_EP_ROUTING	0x83E	/* Routing/passthrough (modules 1-5) */

/* Code offsets in SHARC firmware memory */
#define UA_MOD_CODE_DSP0	0x005F7FE4  /* DSP 0 mixer code */
#define UA_MOD_CODE_OTHER	0x00DF7FE4  /* DSP 1-3 routing code */

/* Module flags */
#define UA_MOD_FLAG_CODE	0x40000000  /* Has executable code */
#define UA_MOD_FLAG_PASSTHRU	0x00000006  /* Passthrough (no code) */

/*
 * Extended IO wiring block (DMA[152] from Windows capture).
 * Contains cross-module references, preamp ADC routing, and
 * biquad coefficient sets. Sent BEFORE per-module SRAM writes.
 * Includes IO type 0x1E (preamp ADC input) wiring at word[20-21].
 */
static const u32 ua_mod_extended_wiring[] = {
	0x00000000, 0x0009D0D8, 0x00000000, 0x00000006,
	0x00000000, 0x00000001, 0x00000004, 0x00000100,
	0x000000C8, 0x3A924925, 0xC0000000, 0xC0000000,
	0x00000010, 0x00000045, 0x41000000, 0x3AED7304,
	0xFFFFFFFF, 0x00000001, 0xFFFFFFFF, 0x00000001,
	0x00140012, 0x0009D0F6, 0x00000040, 0x3CB5F897,
	0xBF7FEFD4, 0x3F7FF7EA, 0x391565D3, 0x3F800000,
	0xBC35FC6C, 0xBBD21841, 0x3CB5F897, 0xBF7FEFD4,
	0x3F7FF7EA, 0x391565D3, 0x3F800000, 0xBC35FC6C,
	0xBBD21841, 0x00000001, 0x00140011, 0x08FFD7D1,
	0x00000040, 0x3CB5F897, 0xBF7FEFD4, 0x3F7FF7EA,
	0x391565D3, 0x3F800000, 0xBC35FC6C, 0xBBD21841,
	0x3E433499, 0xBF7B4E32, 0x3F7DA53E, 0x3C2F122D,
	0x3F7FFC42, 0xBDC46777, 0xBD5F4E1C,
};
#define UA_MOD_EXTENDED_WORDS	ARRAY_SIZE(ua_mod_extended_wiring)

/* Module 0 auxiliary data */
#define UA_MOD0_AUX_SRAM	0x000C6FF6
#define UA_MOD0_AUX_DESC	0x0014000A  /* packed: count=20, start=10 */

/* Biquad filter coefficients (7 words, shared across code modules) */
static const u32 ua_mod_biquad[7] = {
	0x3CB5F897,  /* 0.0222133 */
	0xBF7FEFD4,  /* -0.999753 */
	0x3F7FF7EA,  /* 0.999877 */
	0x391565D3,  /* 0.000142477 */
	0x3F800000,  /* 1.0 */
	0xBC35FC6C,  /* -0.0111075 */
	0xBBD21841,  /* -0.00641158 */
};

/*
 * Per-module SRAM addresses (allocated downward from base 0x9D2FE).
 * Module 0 has extra aux SRAM between sram_io and sram_hdr.
 */
struct ua_module_addrs {
	u32 sram_hdr;	/* Module header SRAM address (9 dwords) */
	u32 sram_io;	/* I/O descriptor SRAM address (30 dwords) */
	u32 coeff_base;	/* Coefficient blocks base (4 × 256 dwords) */
};

static const struct ua_module_addrs ua_x4_modules[UA_MODULE_COUNT] = {
	{ 0x0009D1B0, 0x0009D192, 0x08FFDC00 },  /* 0: mixer (A5+C2+DB) */
	{ 0x0009D186, 0x0009D168, 0x08FFD800 },  /* 1: routing */
	{ 0x0009D160, 0x0009D142, 0x08FFD400 },  /* 2: routing */
	{ 0x0009D13A, 0x0009D11C, 0x08FFD000 },  /* 3: passthrough */
	{ 0x0009D114, 0x0009D0F6, 0x08FFCC00 },  /* 4: passthrough */
	{ 0x0009D0EE, 0x0009D0D0, 0x08FFC800 },  /* 5: routing (2in/2out) */
};

/*
 * Module header template (9 dwords).
 * Word [1] = sram_hdr (self-referencing, filled per-module).
 * Word [8] = entry_point (filled per-module).
 */
static const u32 ua_mod_hdr_template[9] = {
	0x00000000,  /* [0] reserved */
	0x00000000,  /* [1] self_sram_addr (FILL) */
	0x00000024,  /* [2] size (36 = 9 * 4) */
	0x40000000,  /* [3] flags (has_code) */
	0x00000001,  /* [4] input_count (FILL for module 5: 2) */
	0x00000001,  /* [5] output_count (FILL for module 5: 2) */
	0x00000022,  /* [6] total_channel_count (34) */
	0x00000001,  /* [7] module_type */
	0x00000000,  /* [8] entry_point (FILL) */
};

/*
 * Module 0 I/O descriptor (22 dwords) — mixer core with aux + biquads.
 * Words [1], [12], [13] are self-referencing (filled at runtime).
 */
static const u32 ua_mod0_io_template[22] = {
	0x00000000,  /* [0]  reserved */
	0x00000000,  /* [1]  self_sram_addr (FILL: sram_io) */
	0x005F7FE4,  /* [2]  code_offset */
	0x40000000,  /* [3]  flags (CODE) */
	0x00000001,  /* [4]  input_count */
	0x00000001,  /* [5]  output_count */
	0x00000004,  /* [6]  channels_per_io */
	0x00000100,  /* [7]  block_size (256) */
	0x000000C8,  /* [8]  latency (200) */
	0x3A924925,  /* [9]  sample_rate_reciprocal */
	0x3B83126F,  /* [10] filter_parameter */
	0x00000001,  /* [11] has_aux_data */
	0x0014000A,  /* [12] aux_descriptor (count=20, start=10) */
	0x000C6FF6,  /* [13] aux_sram_addr */
	0x00000000,  /* [14] reserved */
	/* [15-21] biquad filter coefficients */
	0x3CB5F897, 0xBF7FEFD4, 0x3F7FF7EA,
	0x391565D3, 0x3F800000, 0xBC35FC6C, 0xBBD21841,
};

/*
 * Modules 1-2 I/O descriptor (11 dwords) — routing with code.
 * Word [1] = self_sram_addr (filled per-module).
 */
static const u32 ua_mod_code_io_template[11] = {
	0x00000000,  /* [0]  reserved */
	0x00000000,  /* [1]  self_sram_addr (FILL) */
	0x00DF7FE4,  /* [2]  code_offset */
	0x40000000,  /* [3]  flags (CODE) */
	0x00000001,  /* [4]  input_count */
	0x00000001,  /* [5]  output_count */
	0x00000004,  /* [6]  channels_per_io */
	0x00000100,  /* [7]  block_size */
	0x000000C8,  /* [8]  latency */
	0x3A924925,  /* [9]  sample_rate_reciprocal */
	0x3B83126F,  /* [10] filter_parameter */
};

/*
 * Modules 3-4 I/O descriptor (11 dwords) — passthrough (no code).
 */
static const u32 ua_mod_passthru_io_template[11] = {
	0x00000000,  /* [0]  reserved */
	0x00000000,  /* [1]  self_sram_addr (FILL) */
	0x00000000,  /* [2]  code_offset (none) */
	0x00000006,  /* [3]  flags (PASSTHROUGH) */
	0x00000000,  /* [4]  input_count (0 for passthru) */
	0x00000001,  /* [5]  output_count */
	0x00000004,  /* [6]  channels_per_io */
	0x00000100,  /* [7]  block_size */
	0x000000C8,  /* [8]  latency */
	0x3A924925,  /* [9]  sample_rate_reciprocal */
	0x3B83126F,  /* [10] filter_parameter */
};

/*
 * Module 5 I/O descriptor (11 dwords) — routing with 2 in/out.
 */
static const u32 ua_mod5_io_template[11] = {
	0x00000000,  /* [0]  reserved */
	0x00000000,  /* [1]  self_sram_addr (FILL) */
	0x00DF7FE4,  /* [2]  code_offset */
	0x40000000,  /* [3]  flags (CODE) */
	0x00000002,  /* [4]  input_count */
	0x00000002,  /* [5]  output_count */
	0x00000004,  /* [6]  channels_per_io */
	0x00000100,  /* [7]  block_size */
	0x000000C8,  /* [8]  latency */
	0x3A924925,  /* [9]  sample_rate_reciprocal */
	0x3B83126F,  /* [10] filter_parameter */
};

/*
 * EnableSynchProcessing targets (from Windows capture).
 * cmd: {0x001F0004, module_ref, sram_hdr, 0}
 * Only modules 0 and 1 are activated in the basic connect.
 */
#define UA_ENABLE_SYNCH		0x001F0004
#define UA_ROUTING_ENABLE	0x001E0004

struct ua_synch_target {
	u32 module_ref;	/* (slot_hi << 16) | slot_lo */
	int module_idx;	/* Index into ua_x4_modules[] for sram_hdr */
};

static const struct ua_synch_target ua_x4_synch[] = {
	{ 0x0020001B, 1 },  /* Module 1 first */
	{ 0x0020000A, 0 },  /* Module 0 second */
};

#define UA_SYNCH_COUNT	ARRAY_SIZE(ua_x4_synch)

/*
 * RoutingEnable commands sent after EnableSynchProcessing.
 * Format: {0x001E0004, slot, sub_param, mask}
 */
struct ua_routing_cmd {
	u32 slot;
	u32 sub_param;
	u32 mask;
};

static const struct ua_routing_cmd ua_x4_routing[] = {
	/* After EnableSynch module 1 (slot 0x1B) */
	{ 0x0000001B, 0x00000008, 0x00000020 },
	{ 0x0000001B, 0x00000008, 0x00000020 },
	{ 0x0000001B, 0x00000007, 0x0001FFFF },
	/* After EnableSynch module 0 (slot 0x0A) */
	{ 0x0000000A, 0x00000008, 0x00000020 },
	{ 0x0000000A, 0x00000008, 0x00000020 },
	{ 0x0000000A, 0x00000007, 0x0000FFFF },
};

#define UA_ROUTING_COUNT	ARRAY_SIZE(ua_x4_routing)

#endif /* UA_DSP_MODULES_H */
