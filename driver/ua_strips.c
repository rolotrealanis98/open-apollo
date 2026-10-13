// SPDX-License-Identifier: GPL-2.0-only
/*
 * Universal Audio Apollo — Console Mic Input Strips on DSP 0
 *
 * Copyright (c) 2026 apollo-linux contributors
 *
 * The analog inputs reach the DSP mix, the meters and the host only through
 * the Console's input strips: DSP modules the Mixer Engine has UAD2System
 * place per input (macOS: CMxInput::AllocateHeaderPlugin -> the hidden
 * "UAD Mic Input Strip Header" plugin -> CDSPResourceManager::LoadPlugin).
 * Linux never had them, so after a Linux cold start every analog record
 * channel, the monitor mix and readback meter words 8-25 stay at zero.
 * With input_strips=1 this file builds one Mic Input Strip per analog
 * preamp input on DSP 0, the way the kext's resource manager does it
 * (apollo-linux docs/re-notes/macos-dsp-module-load.md, "Native generation
 * spec" and Track A section A6), and takes them down again before every
 * reconnect, rate change, suspend and removal (A6.9).
 *
 * Scope (licensing boundary): only the built-in Console strip modules
 * every Apollo runs (0xEB, 0xDA, 0xA5, 0xC2, 0x12B; 0xDB and 0xC9 are
 * reserved for the Input and Talkback strips) are ever loaded, from files
 * the user's own UA software provides (universal-audio/dsp/<id>.bill,
 * written by oa-assets firmware; no blob bytes are in this source).  Any
 * other id is refused.  They are not plug-ins: no authorization is read
 * for them, and nothing here is reachable from user space.
 *
 * What goes to DSP 0 for analog input i (DSP input channel i on the x4) in
 * slot k of a pass:
 *   1. code pages (first strip only, modules not yet resident): one upload
 *      per module, {0x0001nnnn, dest, Bill with arch byte 2 and word 2 = 0},
 *      at 0xe0000, 0xe006c, 0xe025c, 0xe038c, 0xe03a2, each acknowledged
 *      by the DSP's {0x80070004, 0, id, x} before the next one (the kext's
 *      _transmitResource waits; 0e9990b did not);
 *   2. ten zero-fills {0x00080004, addr, 0, dwords}: the instance header
 *      and the strip's private blocks, top-down from the pool tops
 *      0x9d1b8 / 0xc6ffe / 0x08ffe000, slot k lower by 0x54 / 0x10 /
 *      0x410 dwords;
 *   3. the memspec link table, DMA_REF(19): opcode 0x15;
 *   4. the main plugin's header parameters, DMA_REF(9): opcode 0x14;
 *   5. the sub-plugin's defaults at the current rate, one DMA_REF(49);
 *   6. SYNCH {0x001F0004, 0x00200000 | i, header, 0} (starts the strip);
 *   7. {0x001E0004, i, 8, 0x20} twice and {0x001E0004, i, 7, 0x1ffff}
 *      (span: none; direct-out route: none), route 0x1ffff on DSPs 1-3.
 * Steps 2-7 are one transaction, waited for (ring consumption) before the
 * next strip.  Teardown: SYNCH with address 0 per strip, then
 * {0x00030002, id} for every module whose last reference goes.
 *
 * What changed from 0e9990b (implementation plan S1, review findings 5,
 * 17): the addresses come from DSP 0's pool ledger (ua_resource_pool.c,
 * top-down; they equal the recipe's when the layout check passes, and a
 * pass refuses if they do not); every code page waits for its reply; the
 * pass runs in the plug-in host's work and waits for the DSP with ua->lock
 * released; the teardown is published under the lock without waiting and
 * proven by the next pass (a fence) or by the quiesce, else DSP 0 is
 * quarantined; DSP 0 is used only when the plug-in host owns it
 * (ua_plugin_place.c: cold-started by this driver, boot word 3, untouched).
 */

#include <linux/errno.h>
#include <linux/jiffies.h>
#include <linux/module.h>
#include <linux/moduleparam.h>
#include <linux/slab.h>
#include <linux/string.h>
#include <linux/workqueue.h>
#include <sound/core.h>
#include <sound/info.h>

#include "ua_apollo.h"
#include "ua_plugin_host.h"

static bool input_strips;
module_param(input_strips, bool, 0444);
MODULE_PARM_DESC(input_strips,
		 "EXPERIMENTAL: create the Console's Mic Input Strip on DSP 0 for every analog preamp input after a connect (and rebuild them after each rate change), as the macOS driver does, so the analog inputs reach the record channels, the monitor mix and the meters after a Linux cold start. Needs the strip modules in /lib/firmware/universal-audio/dsp/ (oa-assets firmware, from your own UA install) and an Apollo power-cycled with this driver loaded; strips another OS left are never touched. Apollo x4 only (default: off)");

/*
 * Bring-up aid: send only the first N steps of each strip (1 code pages,
 * 2 + zero-fill, 3 + memspec, 4 + header, 5 + sub-plugin parameters,
 * 6 + SYNCH; 0 = everything, DSP 0's input words included).  A partial strip
 * does not run; its teardown still stops it and releases the modules.
 */
static unsigned int input_strips_steps;
module_param(input_strips_steps, uint, 0444);
MODULE_PARM_DESC(input_strips_steps,
		 "With input_strips=1, bring-up only: send just the first N steps of each strip (1 code, 2 +zero-fill, 3 +memspec, 4 +header, 5 +parameters, 6 +SYNCH; 0 = all, default)");
static unsigned int input_strips_mask = 0xff;
module_param(input_strips_mask, uint, 0444);
MODULE_PARM_DESC(input_strips_mask,
		 "With input_strips=1: the analog inputs that get a strip, bit 0 = input 1 (default 0xff: every preamp input)");
static unsigned int input_strips_form = 1;
module_param(input_strips_form, uint, 0444);
MODULE_PARM_DESC(input_strips_form,
		 "With input_strips=1, bring-up only: how a code page's record is sent: 0 = the recipe run on titan (architecture byte in the id word, word 2 zero; default), 1 = as UAD2System sends a registered record (word 2's upper half cleared)");

/*
 * input_strips_params=1 (bring-up): the Mic strip's parameters exactly as
 * UA's captured Solo USB stream writes them (tools/uausb/solo-usb-init.json,
 * byte-identical to the capture): m0[0..17], m0[38..53] and m3[1..15], with
 * the captured 48 kHz DC blocker.  48 kHz only.  0 = the A1 recipe.
 */
static unsigned int input_strips_inputs;
module_param(input_strips_inputs, uint, 0444);
MODULE_PARM_DESC(input_strips_inputs,
		 "With input_strips=1, bring-up only: build strips on DSP inputs 0..N-1 (N up to 8) instead of the preamp inputs; 0 = the preamps (default)");

static unsigned int input_strips_route = 0x1ffff;
module_param(input_strips_route, uint, 0444);
MODULE_PARM_DESC(input_strips_route,
		 "With input_strips=1, bring-up only: the DirectOutputRoute word sent to DSP 0 after each strip's SYNCH (default 0x1ffff, \"not on this DSP\"; Windows sends 0x0ffff for an input whose strip DSP 0 owns)");

static unsigned int input_strips_dsp = 1;
module_param(input_strips_dsp, uint, 0444);
MODULE_PARM_DESC(input_strips_dsp,
		 "With input_strips=1, bring-up only: the DSP the strips are built on (default 0)");
/* input_strips_dsp, checked against the DSP count by ua_strips_init() */
static unsigned int ua_strips_dsp;

static bool input_strips_phantom = true;
module_param(input_strips_phantom, bool, 0444);
MODULE_PARM_DESC(input_strips_phantom,
		 "With input_strips=1: after the strips go live on a cold start, write 48V off then on for every preamp the Apollo reports with 48V on, so its phantom supply comes up and the input unmutes (default: on)");

static unsigned int input_strips_params = 1;
module_param(input_strips_params, uint, 0444);
MODULE_PARM_DESC(input_strips_params,
		 "With input_strips=1, bring-up only: 1 writes the Mic strip's parameters as UA's captured Solo USB stream does (48 kHz only); 0 = the A1 recipe (default)");

/*
 * input_strips_rebuild: writing it (root) takes every live strip down and
 * builds it again on the next service pass, for the lifecycle check
 * (50 cycles, DSP 0 usage back to its baseline each time).
 */
static atomic_t ua_strips_rebuilds = ATOMIC_INIT(0);
static int ua_strips_rebuild_set(const char *val, const struct kernel_param *kp)
{
	atomic_inc(&ua_strips_rebuilds);
	return 0;
}
static int ua_strips_rebuild_get(char *buf, const struct kernel_param *kp)
{
	return sysfs_emit(buf, "%d\n", atomic_read(&ua_strips_rebuilds));
}
static const struct kernel_param_ops ua_strips_rebuild_ops = {
	.set = ua_strips_rebuild_set,
	.get = ua_strips_rebuild_get,
};
module_param_cb(input_strips_rebuild, &ua_strips_rebuild_ops, NULL, 0644);
MODULE_PARM_DESC(input_strips_rebuild,
		 "With input_strips=1, test aid: write anything to take the live strips down and build them again (counts the requests)");

bool ua_strips_wanted(void)
{
	return input_strips;
}

/* UA_STRIPS_PLANNER_BEGIN: no kernel calls in here, the host tests build it */

#define UA_STRIP_MAX		8	/* strips per pass (x4: four preamps; 8 for input_strips_inputs) */
#define UA_STRIP_MODS		5	/* modules of the Mic strip: EB DA A5 C2 12B */
#define UA_STRIP_PRIV		10	/* instance header, m0-m4, c0-c3 */

/* DSP command words (word 0 = opcode << 16 | length in dwords) */
#define UA_STRIP_OP(op, len)	(((u32)(op) << 16) | (u32)(len))
#define UA_STRIP_OP_WRITE	0x14		/* {0x0014nnnn, addr, words} */
#define UA_STRIP_CMD_INPUT	0x001E0004	/* {.., input, param, value} */
#define UA_STRIP_CMD_SYNCH	0x001F0004	/* {.., in2 << 16 | in1, hdr, 0} */
#define UA_STRIP_SYNCH_MONO	0x20		/* in2: FW_SYNCH_IN_INVALID */
#define UA_STRIP_INPUT_ROUTE	7		/* DirectOutputRoute */
#define UA_STRIP_INPUT_SPAN	8		/* SpanInfo */
#define UA_STRIP_ROUTE_NONE	0x0001ffff	/* no direct-out route */
#define UA_STRIP_SPAN_NONE	0x20		/* not linked */
#define UA_STRIP_PLUGIN_MIC	0x846		/* pluginId of the Mic Input Strip */

/* Bill blob header, as CAllocator::RegisterSharedResource checks it */
#define UA_STRIP_BILL_W2	0x02000000

/*
 * The Console strip modules (Bill resources) the driver knows.  @len is
 * the blob length on disk and @sram the code pool dwords it takes
 * (w4 * 2 / 3): the code-pool layout of the recipe is built on exactly
 * these.  Only the Mic Input Strip's five load; 0xDB (Input Strip) and
 * 0xC9 (Talkback strip) are reserved for later and refused like any other
 * id.  The first UA_STRIP_MODS entries are the Mic strip's, in its
 * resource order (UADUnison::AdjustUAD2PluginAllocInfo).
 */
struct ua_strip_module {
	u16 id;
	u16 len;
	u16 sram;
	bool reserved;
};

static const struct ua_strip_module ua_strip_modules[] = {
	{ 0x0eb,  716, 0x06c, false },
	{ 0x0da, 3044, 0x1f0, false },
	{ 0x0a5, 1940, 0x130, false },
	{ 0x0c2,  184, 0x016, false },
	{ 0x12b,  452, 0x040, false },
	{ 0x0db,  728, 0x06e, true },	/* Input Strip: reserved */
	{ 0x0c9,  352, 0x032, true },	/* Talkback strip: reserved */
};

/* The Mic strip's module @id, or NULL: not a module this driver loads */
static const struct ua_strip_module *ua_strip_module_find(u32 id)
{
	unsigned int i;

	for (i = 0; i < ARRAY_SIZE(ua_strip_modules); i++)
		if (ua_strip_modules[i].id == id)
			return ua_strip_modules[i].reserved ? NULL :
			       &ua_strip_modules[i];
	return NULL;
}

static u32 ua_strip_le32(const u8 *p)
{
	return p[0] | (u32)p[1] << 8 | (u32)p[2] << 16 | (u32)p[3] << 24;
}

/*
 * Is @data (@len bytes, universal-audio/dsp/<id>.bill) module @id's Bill
 * blob in the form the recipe needs?  "Bill" magic; the id word = @id, so
 * the arch byte is 0 as on disk; word 2 = 0x02000000; size + 0x14 = the
 * length; and the length and code size the code-pool layout was built for.
 * Returns 0, -EPERM for an id that is not a Mic strip module, -EINVAL
 * otherwise.
 */
static int ua_strip_blob_check(u32 id, const u8 *data, size_t len)
{
	const struct ua_strip_module *m = ua_strip_module_find(id);
	u32 w4;

	if (!m)
		return -EPERM;
	if (!data || len != m->len)
		return -EINVAL;
	if (ua_strip_le32(data) != UA_BILL_MAGIC ||
	    ua_strip_le32(data + 4) != id ||
	    ua_strip_le32(data + 8) != UA_STRIP_BILL_W2 ||
	    ua_strip_le32(data + 12) != len - UA_BILL_HDR)
		return -EINVAL;
	w4 = ua_strip_le32(data + 16);
	if (w4 > 0xffff || w4 * 2 / 3 != m->sram)
		return -EINVAL;
	return 0;
}

/*
 * The Mic strip's defaults that depend on the sample rate (Track A
 * section A1).  The ramp: n = 0x10, k = SR / BS * 0.05 + 0.5 blocks,
 * f = BS / 4.0f and 1.0f / (f * k), with the ramp block size BS 32 below
 * 100 kHz and 64 above (CUAD2PlugIn::Allocate: 32 for a synch plugin below
 * 100 kHz with the LowLatencySynchronous setting, which defaults to on);
 * the float bits are computed offline.  The biquads come from the SDK
 * tables for three bands (< 50 kHz, 50-100 kHz, > 100 kHz).
 */
struct ua_strip_rate {
	u32 rate;
	u32 k;
	u32 f;
	u32 inv;
	const u32 *dc;		/* DC blocker, 8 floats */
	const u32 *lowcut;	/* 75 Hz LowCut, 8 floats */
};

static const u32 ua_strip_dc[3][8] = {
	{ 0x00000000, 0x3cb55044, 0xbf7fefd0, 0x3f7ff7f0,
	  0x391565e1, 0x3f7fffef, 0xbc3573eb, 0xbbd23ba2 },
	{ 0x00000000, 0x3c76af8a, 0xbf7ff898, 0x3f7ffc44,
	  0x38893a86, 0x3f7ffffc, 0xbbf6b20e, 0xbb8e6863 },
	{ 0x00000000, 0x3c2e7056, 0xbf7ffc44, 0x3f7ffe2a,
	  0x38092b1e, 0x3f7fffff, 0xbbae7117, 0xbb498cfc },
};

static const u32 ua_strip_lowcut[3][8] = {
	{ 0x00000000, 0x3e3b3f3d, 0xbf7baf04, 0x3f7dd5f1,
	  0x3c20d8d2, 0x3f7ffcd7, 0xbdbc4dc9, 0xbd565d96 },
	{ 0x00000000, 0x3e04f704, 0xbf7dd529, 0x3f7eea30,
	  0x3ba0d951, 0x3f7fff36, 0xbd855748, 0xbd18e14f },
	{ 0x00000000, 0x3daa574a, 0xbf7f1ceb, 0x3f7f8e5c,
	  0x3b20d96c, 0x3f7fffcd, 0xbd2aa2e7, 0xbcf07ac9 },
};

static const struct {
	u32 rate, k, f, inv;
} ua_strip_ramps[] = {
	{  44100,  69, 0x41000000, 0x3aed7304 },	/* BS 32: 1/552 */
	{  48000,  75, 0x41000000, 0x3ada740e },	/* 1/600 */
	{  88200, 138, 0x41000000, 0x3a6d7304 },	/* 1/1104 */
	{  96000, 150, 0x41000000, 0x3a5a740e },	/* 1/1200 */
	{ 176400, 138, 0x41800000, 0x39ed7304 },	/* BS 64: 1/2208 */
	{ 192000, 150, 0x41800000, 0x39da740e },	/* 1/2400 */
};

/* Fill @r for @rate (Hz); -EINVAL for a rate the Apollo does not run at */
static int ua_strip_rate_lookup(u32 rate, struct ua_strip_rate *r)
{
	unsigned int i, band;

	for (i = 0; i < ARRAY_SIZE(ua_strip_ramps); i++) {
		if (ua_strip_ramps[i].rate != rate)
			continue;
		band = rate < 50000 ? 0 : rate <= 100000 ? 1 : 2;
		r->rate = rate;
		r->k = ua_strip_ramps[i].k;
		r->f = ua_strip_ramps[i].f;
		r->inv = ua_strip_ramps[i].inv;
		r->dc = ua_strip_dc[band];
		r->lowcut = ua_strip_lowcut[band];
		return 0;
	}
	return -EINVAL;
}

/*
 * DSP 0's memory pools as CDSPResourceManager::Initialize sets them up
 * from the register window (GetProperty(6)): pool n has size +0x10 + 4n,
 * base +0x184 + 4n and, for n >= 1, scratch +0x190 + 4n.
 * CResourcePool::Initialize rounds the base up to even and the size down;
 * the scratch area sits at the bottom of pools 1-3.  Pool 0 (code) is
 * filled bottom-up, pools 1-3 (private data) top-down, first fit.
 */
struct ua_strip_layout {
	u32 start[4];		/* first dword */
	u32 top[4];		/* end: start + size */
	u32 floor[4];		/* lowest dword above the scratch area */
};

/* The empty-DSP values the recipe was validated on (titan's x4, A6.1) */
#define UA_STRIP_CODE_BASE	0x000e0000
static const u32 ua_strip_tops[4] = { 0, 0x0009d1b8, 0x000c6ffe, 0x08ffe000 };
/* Scratch the Mic strip asks for (AllocInfo 0xA9C, 0xA98, 0xAA0) */
static const u32 ua_strip_scratch[4] = { 0, 0x1c0, 0x80, 0 };

/*
 * One strip's private memory in descriptor order: the main plugin's
 * 8-dword instance header (the chain head), then the Mic sub-plugin's
 * m0-m4 and the input strip commons c0-c3.  @link is the memspec link
 * (ua_memspec_compile()): m0's first word 6 of the head's row 0; the
 * others a word of m0 (row 0 of their own descriptor).
 */
static const struct ua_memspec_row ua_strip_rows[UA_STRIP_PRIV] = {
	{ 1, 0x008, 0xffffffff, 0 },	/* instance header */
	{ 1, 0x038, 0x00000006, 0 },	/* m0 -> header[6] */
	{ 2, 0x010, 0x00000036, 0 },	/* m1 -> m0[0x36] */
	{ 1, 0x010, 0x00000037, 0 },	/* m2 -> m0[0x37] */
	{ 3, 0x010, 0x0000001b, 0 },	/* m3 -> m0[0x1b] */
	{ 1, 0x004, 0x0000001c, 0 },	/* m4 -> m0[0x1c] */
	{ 3, 0x100, 0x0000000e, 0 },	/* c0 -> m0[0x0e] */
	{ 3, 0x100, 0x0000000f, 0 },	/* c1 -> m0[0x0f] */
	{ 3, 0x100, 0x00000006, 0 },	/* c2 -> m0[0x06] */
	{ 3, 0x100, 0x00000007, 0 },	/* c3 -> m0[0x07] */
};
static const unsigned int ua_strip_desc[2] = { 0, 1 };	/* head, Mic */

/* ua_strip_layout_check() results */
#define UA_STRIP_LAYOUT_GONE	1	/* a register read all-ones */
#define UA_STRIP_LAYOUT_CODE	2	/* code pool not at 0xe0000 */
#define UA_STRIP_LAYOUT_CODE_SZ	3	/* code pool too small */
#define UA_STRIP_LAYOUT_TOP	4	/* + pool: top is not the recipe's */
#define UA_STRIP_LAYOUT_ROOM	8	/* + pool: no room for the strips */

/*
 * Check the pool registers against the layout the recipe was validated on
 * (A6.1: code pool at 0xe0000, pools 1-3 ending at 0x9d1b8 / 0xc6ffe /
 * 0x08ffe000) with room for @n strips above each pool's scratch area (the
 * register's or the Mic strip's, whichever is larger).  Fills @l.
 * Returns 0 or a UA_STRIP_LAYOUT_* code.
 */
static int ua_strip_layout_check(const u32 size[4], const u32 base[4],
				 const u32 scratch[4], unsigned int n,
				 struct ua_strip_layout *l)
{
	u32 need[4] = { 0, 0, 0, 0 };
	unsigned int p, j;
	u32 sc;

	for (p = 0; p < 4; p++) {
		if (size[p] == 0xffffffff || base[p] == 0xffffffff ||
		    (p && scratch[p] == 0xffffffff))
			return UA_STRIP_LAYOUT_GONE;
		l->start[p] = base[p] + (base[p] & 1);
		l->top[p] = l->start[p] + (size[p] & ~1u);
		sc = p ? scratch[p] + (scratch[p] & 1) : 0;
		if (sc < ua_strip_scratch[p])
			sc = ua_strip_scratch[p];
		l->floor[p] = l->start[p] + sc;
		if (l->top[p] < l->start[p] || l->floor[p] < l->start[p])
			return UA_STRIP_LAYOUT_TOP + p;
	}

	for (j = 0; j < UA_STRIP_MODS; j++)
		need[0] += ua_strip_modules[j].sram;
	for (j = 0; j < UA_STRIP_PRIV; j++)
		need[ua_strip_rows[j].pool] += ua_strip_rows[j].dwords * n;

	if (l->start[0] != UA_STRIP_CODE_BASE)
		return UA_STRIP_LAYOUT_CODE;
	if (l->top[0] - l->start[0] < need[0])
		return UA_STRIP_LAYOUT_CODE_SZ;
	for (p = 1; p < 4; p++) {
		if (l->top[p] != ua_strip_tops[p])
			return UA_STRIP_LAYOUT_TOP + p;
		if (l->floor[p] > l->top[p] ||
		    l->top[p] - l->floor[p] < need[p])
			return UA_STRIP_LAYOUT_ROOM + p;
	}
	return 0;
}

/* The recipe's code pool destinations of the Mic strip's modules */
static void ua_strip_code_dest(const struct ua_strip_layout *l,
			       u32 dest[UA_STRIP_MODS])
{
	u32 cur = l->start[0];
	unsigned int j;

	for (j = 0; j < UA_STRIP_MODS; j++) {
		dest[j] = cur;
		cur += ua_strip_modules[j].sram;
	}
}

/*
 * The recipe's private block addresses of the strip in slot @k of a pass
 * that starts on empty pools: each pool hands out its blocks top-down,
 * strip after strip.
 */
static void ua_strip_priv_addrs(const struct ua_strip_layout *l,
				unsigned int k, u32 addr[UA_STRIP_PRIV])
{
	u32 cur[4];
	unsigned int p, j, s;

	for (p = 0; p < 4; p++)
		cur[p] = l->top[p];
	for (s = 0; s <= k; s++) {
		for (j = 0; j < UA_STRIP_PRIV; j++) {
			cur[ua_strip_rows[j].pool] -= ua_strip_rows[j].dwords;
			addr[j] = cur[ua_strip_rows[j].pool];
		}
	}
}

/* A DSP command @w[0..@n) carried by DMA pages */
static void ua_strip_dma(struct ua_xact *x, const u32 *w, unsigned int n)
{
	__le32 *d = ua_xact_cmd(x, n);
	unsigned int i;

	if (d)
		for (i = 0; i < n; i++)
			d[i] = cpu_to_le32(w[i]);
}

/* input_strips_params, copied in by ua_strips_init() (1: UA's captured layout) */
static unsigned int ua_strip_params_mode;
/* input_strips_route, copied in by ua_strips_init() */
static u32 ua_strip_route = 0x1ffff;

static const u32 ua_strip_usb_m0a[18] = {
	0x0f1f7fe4, 0x40000000, 0x00000001, 0x00000001, 0x00000004, 0x00000100,
	0x000000c8, 0x3a888889, 0xc0000000, 0xc0000000, 0x00000010, 0x0000004b,
	0x41000000, 0x3ada740e, 0xffffffff, 0x00000001, 0xffffffff, 0x00000001,
};
static const u32 ua_strip_usb_m0b[16] = {
	0x00000040, 0x3cae6c74, 0xbf7ff125, 0x3f7ff892, 0x3909422f, 0x3f800000,
	0xbc2e6fd3, 0xbbc96245, 0x3cae6c74, 0xbf7ff125, 0x3f7ff892, 0x3909422f,
	0x3f800000, 0xbc2e6fd3, 0xbbc96245, 0x00000001,
};
static const u32 ua_strip_usb_m3[15] = {
	0x00000040, 0x3cae6c74, 0xbf7ff125, 0x3f7ff892, 0x3909422f, 0x3f800000,
	0xbc2e6fd3, 0xbbc96245, 0x3e3b3f3d, 0xbf7baf04, 0x3f7dd5f1, 0x3c20d8d2,
	0x3f7ffcd7, 0xbdbc4dc9, 0xbd565d96,
};

/*
 * One Mic Input Strip for DSP input @synch at private addresses @addr
 * (A6 steps 3-8, DSP 0's entries after the code pages): the private blocks
 * zero-filled, the memspec table, the header and sub-plugin parameters at
 * @r's rate, SYNCH, and the input's span and route.  @steps limits it
 * (input_strips_steps; 0 = all).  Returns 0 or the first build error.
 */
static int ua_strip_build(struct ua_xact *x, const u32 addr[UA_STRIP_PRIV],
			  u32 synch, const struct ua_strip_rate *r,
			  unsigned int steps)
{
	u32 w[64];
	unsigned int j, n;
	int ret;

	if (steps && steps <= 1)
		return x->err;
	for (j = 0; j < UA_STRIP_PRIV; j++)
		ua_xact_inline(x, UA_CMD_ZERO, addr[j], 0,
			       ua_strip_rows[j].dwords);
	if (steps && steps <= 2)
		return x->err;

	/* Memspec: "store addr_j at its target row + offset" but the header */
	ret = ua_memspec_compile(ua_strip_rows, UA_STRIP_PRIV, ua_strip_desc,
				 ARRAY_SIZE(ua_strip_desc), addr, w);
	if (ret < 0)
		return ret;
	ua_strip_dma(x, w, (unsigned int)ret);
	if (steps && steps <= 3)
		return x->err;

	/* Main plugin's header: in 1, out 1, sub-plugin id */
	n = 0;
	w[n++] = UA_STRIP_OP(UA_STRIP_OP_WRITE, 9);
	w[n++] = addr[0];
	w[n++] = 0x24;
	w[n++] = 0x40000000;
	w[n++] = 1;
	w[n++] = 1;
	w[n++] = 0x22;
	w[n++] = 1;
	w[n++] = UA_STRIP_PLUGIN_MIC;
	ua_strip_dma(x, w, n);
	if (steps && steps <= 4)
		return x->err;

	if (ua_strip_params_mode == 1) {
		/*
		 * UA's captured layout: m0[0..17], m0[38..53], m3[1..15].  At
		 * 48 kHz the captured words verbatim (verified on titan's x4);
		 * at other rates the same layout with this rate's ramp (k, f,
		 * 1/(f*k)) and the SDK band's DC blocker and LowCut taps after
		 * the captured 0x40 count word (not yet verified on hardware).
		 */
		bool cap = r->rate == 48000;

		n = 0;
		w[n++] = UA_STRIP_OP(UA_STRIP_OP_WRITE, 2 + ARRAY_SIZE(ua_strip_usb_m0a));
		w[n++] = addr[1];
		for (j = 0; j < ARRAY_SIZE(ua_strip_usb_m0a); j++)
			w[n++] = ua_strip_usb_m0a[j];
		if (!cap) {
			w[n - 18 + 11] = r->k;
			w[n - 18 + 12] = r->f;
			w[n - 18 + 13] = r->inv;
		}
		w[n++] = UA_STRIP_OP(UA_STRIP_OP_WRITE, 2 + ARRAY_SIZE(ua_strip_usb_m0b));
		w[n++] = addr[1] + 38;
		for (j = 0; j < ARRAY_SIZE(ua_strip_usb_m0b); j++)
			w[n++] = cap ? ua_strip_usb_m0b[j] :
				 j == 0 || j == 15 ? ua_strip_usb_m0b[j] :
				 r->dc[j < 8 ? j : j - 7];
		w[n++] = UA_STRIP_OP(UA_STRIP_OP_WRITE, 2 + ARRAY_SIZE(ua_strip_usb_m3));
		w[n++] = addr[4] + 1;
		for (j = 0; j < ARRAY_SIZE(ua_strip_usb_m3); j++)
			w[n++] = cap ? ua_strip_usb_m3[j] :
				 j == 0 ? ua_strip_usb_m3[j] :
				 j < 8 ? r->dc[j] : r->lowcut[j - 7];
		ua_strip_dma(x, w, n);
		goto start;
	}

	/* Sub-plugin defaults (A1): m0[0x17..0x18], m0[0x1d..0x35], m3 */
	n = 0;
	w[n++] = UA_STRIP_OP(UA_STRIP_OP_WRITE, 4);
	w[n++] = addr[1] + 0x17;
	w[n++] = 0xc0000000;			/* -2.0f */
	w[n++] = 0xc0000000;
	w[n++] = UA_STRIP_OP(UA_STRIP_OP_WRITE, 0x1b);
	w[n++] = addr[1] + 0x1d;
	w[n++] = 0x10;				/* ramp n */
	w[n++] = r->k;
	w[n++] = r->f;
	w[n++] = r->inv;
	w[n++] = 0xffffffff;
	w[n++] = 1;
	w[n++] = 0xffffffff;
	w[n++] = 1;
	for (j = 0; j < 8; j++)
		w[n++] = r->dc[j];
	for (j = 0; j < 8; j++)
		w[n++] = r->dc[j];
	w[n++] = 1;
	w[n++] = UA_STRIP_OP(UA_STRIP_OP_WRITE, 0x12);
	w[n++] = addr[4];
	for (j = 0; j < 8; j++)
		w[n++] = r->dc[j];
	for (j = 0; j < 8; j++)
		w[n++] = r->lowcut[j];
	ua_strip_dma(x, w, n);
start:
	if (steps && steps <= 5)
		return x->err;

	/* Start it, then the input's span (none) and direct-out route (none) */
	ua_xact_inline(x, UA_STRIP_CMD_SYNCH, UA_STRIP_SYNCH_MONO << 16 | synch,
		       addr[0], 0);
	if (steps && steps <= 6)
		return x->err;
	ua_xact_inline(x, UA_STRIP_CMD_INPUT, synch, UA_STRIP_INPUT_SPAN,
		       UA_STRIP_SPAN_NONE);
	ua_xact_inline(x, UA_STRIP_CMD_INPUT, synch, UA_STRIP_INPUT_SPAN,
		       UA_STRIP_SPAN_NONE);
	ua_xact_inline(x, UA_STRIP_CMD_INPUT, synch, UA_STRIP_INPUT_ROUTE,
		       ua_strip_route);
	return x->err;
}

/* SYNCH with address 0: stops the strip on DSP input @synch (A6.9) */
static void ua_strip_build_stop(struct ua_xact *x, u32 synch)
{
	ua_xact_inline(x, UA_STRIP_CMD_SYNCH, UA_STRIP_SYNCH_MONO << 16 | synch,
		       0, 0);
}

/* UA_STRIPS_PLANNER_END */

/* ----------------------------------------------------------------
 * Driver side
 * ---------------------------------------------------------------- */

#define UA_STRIPS_SETTLE_MS	500	/* after the connect, before the first strip */

/*
 * Models whose analog inputs are known, with the DSP input channel of
 * analog input 1: the x4's are 0-3 (the macOS Mic strip calls for analog
 * 4..1 have synch-in 3..0).  The preamp count comes from the model data.
 */
static const struct {
	u32 device_type;
	u8 first_synch;
} ua_strips_models[] = {
	{ UA_DEV_APOLLO_X4, 0 },
};

enum ua_strips_state {
	UA_STRIPS_OFF,		/* disabled for this device (off_why) */
	UA_STRIPS_IDLE,		/* nothing of ours on DSP 0 */
	UA_STRIPS_ARMED,	/* connected: the host's work starts a pass */
	UA_STRIPS_CREATING,	/* strips going out one by one */
	UA_STRIPS_LIVE,		/* the pass is over: these strips exist */
};

enum ua_strips_pools {
	UA_POOLS_UNCHECKED,
	UA_POOLS_EMPTY,
	UA_POOLS_IN_USE,
	UA_POOLS_LAYOUT,
};

enum ua_strip_state {
	UA_STRIP_NONE,
	UA_STRIP_QUEUED,	/* published, not proven taken yet */
	UA_STRIP_LIVE,		/* DSP 0 took all of it */
};

enum ua_strip_mod_state {
	UA_MOD_NONE,
	UA_MOD_UPLOADING,	/* published, reply not seen yet */
	UA_MOD_RESIDENT,	/* reply validated */
};

struct ua_strip {
	u8 input;		/* analog input, 0-based */
	u8 synch;		/* its DSP input channel */
	u8 state;		/* UA_STRIP_* */
	u32 header;		/* instance header in DSP 0 SRAM */
	struct ua_pool_lease priv[UA_STRIP_PRIV];
	unsigned int ack_ms;
};

struct ua_strips {
	enum ua_strips_state state;
	const char *off_why;
	struct ua_cat_entry *mod[UA_STRIP_MODS];	/* catalog references */
	struct ua_pool_lease code[UA_STRIP_MODS];	/* our holds on DSP 0 */
	u8 mod_state[UA_STRIP_MODS];
	bool mod_released[UA_STRIP_MODS];	/* a teardown released it */
	unsigned int n;				/* strips per pass */
	u8 first_synch;
	struct ua_strip strip[UA_STRIP_MAX];
	u32 dest[UA_STRIP_MODS];
	struct ua_strip_layout layout;
	struct ua_strip_rate rate;
	enum ua_strip_form_e { UA_STRIP_FORM_RECIPE, UA_STRIP_FORM_SDK } form;
	enum ua_strips_pools pools;
	char why[112];				/* pools decision */
	char fail_why[112];
	u32 gen;				/* bumped by every teardown */
	u64 epoch;				/* of the pass */
	unsigned long armed;			/* jiffies */
	int rebuild_seen;
	bool scratch_set;
	unsigned int passes, created, teardowns, failures;
};

static const char *ua_strips_state_name(enum ua_strips_state s)
{
	switch (s) {
	case UA_STRIPS_OFF:		return "off";
	case UA_STRIPS_IDLE:		return "idle";
	case UA_STRIPS_ARMED:		return "armed";
	case UA_STRIPS_CREATING:	return "creating";
	case UA_STRIPS_LIVE:		return "live";
	}
	return "?";
}

static const char *ua_strips_pools_name(enum ua_strips_pools p)
{
	switch (p) {
	case UA_POOLS_UNCHECKED:	return "not checked";
	case UA_POOLS_EMPTY:		return "empty";
	case UA_POOLS_IN_USE:		return "in use";
	case UA_POOLS_LAYOUT:		return "layout differs";
	}
	return "?";
}

static void ua_strips_disable(struct ua_device *ua, struct ua_strips *st,
			      const char *why)
{
	st->state = UA_STRIPS_OFF;
	st->off_why = why;
	dev_info(&ua->pdev->dev, "input strips: disabled: %s\n", why);
}

/* Read and check one module from the store; 0 or a negative errno (logged) */
static int ua_strips_load_module(struct ua_device *ua, struct ua_strips *st,
				 unsigned int j)
{
	const struct ua_strip_module *m = &ua_strip_modules[j];
	struct ua_cat_entry *e;
	int ret;

	ret = ua_catalog_get_system(ua, m->id, &e);
	if (ret) {
		dev_info(&ua->pdev->dev,
			 "input strips: module 0x%03x not found (%d); install the strip modules from your UA software (oa-assets firmware)\n",
			 m->id, ret);
		return ret;
	}
	ret = ua_strip_blob_check(m->id, e->blob, e->len);
	if (ret) {
		dev_warn(&ua->pdev->dev,
			 "input strips: module 0x%03x is not the blob this driver knows (%zu bytes, %d)\n",
			 m->id, e->len, ret);
		mutex_lock(&ua->lock);
		ua_catalog_put(ua, e);
		mutex_unlock(&ua->lock);
		return ret;
	}
	st->mod[j] = e;
	return 0;
}

/**
 * ua_strips_init - input_strips setup at probe
 * @ua: device (probe: no lock held)
 *
 * With input_strips=0 nothing happens and host->strips stays NULL, which
 * makes every other ua_strips_* call a no-op.  Otherwise the model and
 * the modules decide: a model without a known analog input map, a mask
 * that selects no input, or a missing or unknown module disables the
 * strips for this device with one log line (shown in
 * /proc/asound/cardN/dsp_state).
 */
void ua_strips_init(struct ua_device *ua)
{
	struct ua_plugin_host *h = ua->host;
	const struct ua_model_desc *m;
	struct ua_strips *st;
	unsigned int i, j, preamps;
	u32 mask;

	if (!input_strips || !h)
		return;

	st = kzalloc(sizeof(*st), GFP_KERNEL);
	if (!st) {
		dev_warn(&ua->pdev->dev,
			 "input strips: out of memory, not used\n");
		return;
	}
	h->strips = st;
	ua_strip_params_mode = input_strips_params;
	ua_strips_dsp = input_strips_dsp < 4 ? input_strips_dsp : 0;
	ua_strip_route = input_strips_route;
	st->form = input_strips_form == 1 ? UA_STRIP_FORM_SDK :
					    UA_STRIP_FORM_RECIPE;
	st->rebuild_seen = atomic_read(&ua_strips_rebuilds);

	for (i = 0; i < ARRAY_SIZE(ua_strips_models); i++)
		if (ua_strips_models[i].device_type == ua->device_type)
			break;
	if (i == ARRAY_SIZE(ua_strips_models)) {
		dev_info(&ua->pdev->dev,
			 "input strips: no verified analog input map for %s (device type 0x%02x), skipped\n",
			 ua_device_name(ua->device_type), ua->device_type);
		st->state = UA_STRIPS_OFF;
		st->off_why = "no verified analog input map for this model";
		return;
	}
	st->first_synch = ua_strips_models[i].first_synch;

	m = ua_model_lookup(ua->device_type, NULL);
	preamps = m ? min_t(unsigned int, m->preamps, UA_STRIP_MAX) : 0;
	if (input_strips_inputs)
		preamps = min_t(unsigned int, input_strips_inputs, UA_STRIP_MAX);
	mask = input_strips_mask & (BIT(preamps) - 1);
	for (i = 0; i < preamps; i++) {
		if (!(mask & BIT(i)))
			continue;
		st->strip[st->n].input = i;
		st->strip[st->n].synch = st->first_synch + i;
		st->n++;
	}
	if (!st->n) {
		ua_strips_disable(ua, st,
				  "input_strips_mask selects no analog preamp input");
		return;
	}

	for (j = 0; j < UA_STRIP_MODS; j++) {
		if (ua_strips_load_module(ua, st, j)) {
			ua_strips_disable(ua, st,
					  "a strip module is missing or unknown");
			return;
		}
	}

	st->state = UA_STRIPS_IDLE;
	dev_info(&ua->pdev->dev,
		 "input strips: enabled for %u analog input(s) (mask 0x%x); modules 0x0eb 0x0da 0x0a5 0x0c2 0x12b read\n",
		 st->n, mask);
}

/* Host teardown (ua->lock held): drop the module references */
void ua_strips_free(struct ua_device *ua)
{
	struct ua_plugin_host *h = ua->host;
	struct ua_strips *st = h ? h->strips : NULL;
	unsigned int j;

	if (!st)
		return;
	for (j = 0; j < UA_STRIP_MODS; j++)
		ua_catalog_put(ua, st->mod[j]);
	kfree(st);
	h->strips = NULL;
}

/* Is a pass over, with strips running? (ua->lock) */
bool ua_strips_live(struct ua_device *ua)
{
	struct ua_strips *st = ua->host ? ua->host->strips : NULL;

	return st && st->state == UA_STRIPS_LIVE;
}

/**
 * ua_strips_connected - the audio connect is done: start a pass soon
 * @ua: device (ua->lock held)
 *
 * The host's work creates the strips UA_STRIPS_SETTLE_MS later, so the
 * connect and the mixer's first batch go first and no connect path waits
 * for the DSP.
 */
void ua_strips_connected(struct ua_device *ua)
{
	struct ua_plugin_host *h = ua->host;
	struct ua_strips *st = h ? h->strips : NULL;

	if (!st)
		return;
	lockdep_assert_held(&ua->lock);
	if (st->state != UA_STRIPS_IDLE || !ua_plugin_dsp_admitted(ua, ua_strips_dsp))
		return;
	st->state = UA_STRIPS_ARMED;
	st->armed = jiffies;
	queue_delayed_work(system_wq, &h->work,
			   msecs_to_jiffies(UA_STRIPS_SETTLE_MS));
}

/* Record the pools decision; log it when it differs from the last one */
static __printf(3, 4) void ua_strips_decide(struct ua_device *ua,
					    enum ua_strips_pools pools,
					    const char *fmt, ...)
{
	struct ua_strips *st = ua->host->strips;
	char why[sizeof(st->why)];
	va_list args;

	va_start(args, fmt);
	vscnprintf(why, sizeof(why), fmt, args);
	va_end(args);

	if (pools == st->pools && !strcmp(why, st->why))
		return;
	st->pools = pools;
	strscpy(st->why, why, sizeof(st->why));
	if (pools == UA_POOLS_IN_USE)
		dev_info(&ua->pdev->dev,
			 "input strips: DSP0 pools in use, leaving the existing strips (%s)\n",
			 why);
	else if (pools != UA_POOLS_EMPTY)
		dev_info(&ua->pdev->dev, "input strips: not created: %s\n",
			 why);
}

static const char *ua_strips_layout_why(int code, char *buf, size_t len)
{
	if (code == UA_STRIP_LAYOUT_GONE)
		return "a pool register reads all-ones";
	if (code == UA_STRIP_LAYOUT_CODE)
		return "the code pool does not start at 0xe0000";
	if (code == UA_STRIP_LAYOUT_CODE_SZ)
		return "the code pool is too small";
	if (code >= UA_STRIP_LAYOUT_ROOM)
		scnprintf(buf, len, "pool %d has no room for the strips",
			  code - UA_STRIP_LAYOUT_ROOM);
	else
		scnprintf(buf, len, "pool %d does not end where the recipe expects",
			  code - UA_STRIP_LAYOUT_TOP);
	return buf;
}

/* A pass ends early (ua->lock): strips already out stay tracked */
static void ua_strips_fail(struct ua_device *ua, struct ua_strips *st,
			   int err, const char *what, unsigned int input)
{
	unsigned int k;
	bool any = false;

	st->failures++;
	scnprintf(st->fail_why, sizeof(st->fail_why), "%s (input %u, %d)",
		  what, input + 1, err);
	dev_warn(&ua->pdev->dev, "input strips: %s (input %u, %d)\n", what,
		 input + 1, err);
	for (k = 0; k < st->n; k++)
		any |= st->strip[k].state != UA_STRIP_NONE;
	for (k = 0; k < UA_STRIP_MODS; k++)
		any |= st->mod_state[k] != UA_MOD_NONE;
	st->state = any ? UA_STRIPS_LIVE : UA_STRIPS_IDLE;
}

/*
 * Start a pass (ua->lock held): every gate, DSP 0's pool map against the
 * recipe's (A6.1), the scratch areas and the module holds in DSP 0's
 * ledger.  0 to go on.
 */
static int ua_strips_begin(struct ua_device *ua, struct ua_strips *st)
{
	struct ua_plugin_host *h = ua->host;
	struct ua_host_dsp *d = &h->dsp[ua_strips_dsp];
	u32 size[4], base[4], scratch[4];
	unsigned int p, j;
	char buf[64];
	int ret;

	st->state = UA_STRIPS_IDLE;
	if (!ua->dsps_connected || !ua->dsp[ua_strips_dsp].rings_allocated) {
		ua_strips_decide(ua, UA_POOLS_UNCHECKED,
				 "DSP 0 is not ring-connected (kext_connect=0, or its connect failed)");
		return -ENODEV;
	}
	if (ua_plugin_chain_enabled()) {
		ua_strips_decide(ua, UA_POOLS_UNCHECKED,
				 "the captured plugin chain is on (no_plugins=0)");
		return -EBUSY;
	}
	if (!d->owned) {
		ua_strips_decide(ua, UA_POOLS_IN_USE, "%s",
				 d->why[0] ? d->why : "DSP 0 is not this driver's");
		return -EPERM;
	}
	if (d->quarantined) {
		ua_strips_decide(ua, UA_POOLS_IN_USE,
				 "DSP 0 is quarantined until the next cold start");
		return -EPERM;
	}
	if (!d->pools || !d->geom_read) {
		ua_strips_decide(ua, UA_POOLS_UNCHECKED,
				 "DSP 0's pool registers were not read");
		return -ENODEV;
	}
	for (p = 0; p < 4; p++) {
		size[p] = d->geom[p].words;
		base[p] = d->geom[p].base;
		scratch[p] = d->geom[p].scratch;
	}
	ret = ua_strip_layout_check(size, base, scratch, st->n, &st->layout);
	if (ret) {
		ua_strips_decide(ua, UA_POOLS_LAYOUT, "%s",
				 ua_strips_layout_why(ret, buf, sizeof(buf)));
		return -EINVAL;
	}
	if (!ua_pool_owner_idle(d->pools)) {
		ua_strips_decide(ua, UA_POOLS_IN_USE,
				 "something else holds DSP 0's pools");
		return -EBUSY;
	}
	if (ua_strip_rate_lookup(ua->audio.sample_rate, &st->rate)) {
		ua_strips_decide(ua, UA_POOLS_UNCHECKED,
				 "no strip parameters for %u Hz",
				 ua->audio.sample_rate);
		return -EINVAL;
	}

	/* The strip's scratch, then the code holds bottom-up */
	for (p = 1; p < 4; p++) {
		ret = ua_pool_set_scratch(d->pools, p,
					  max_t(u32, d->geom[p].scratch,
						ua_strip_scratch[p]));
		if (ret)
			return ret;
	}
	st->scratch_set = true;
	ua_strip_code_dest(&st->layout, st->dest);
	for (j = 0; j < UA_STRIP_MODS; j++) {
		ret = ua_pool_reserve(d->pools, 0, st->mod[j]->info.words,
				      st->mod[j], true, &st->code[j]);
		if (ret)
			return ret;
		if (st->code[j].base != st->dest[j]) {
			ua_strips_decide(ua, UA_POOLS_LAYOUT,
					 "the allocator and the recipe disagree on module 0x%03x (0x%05x, 0x%05x)",
					 ua_strip_modules[j].id,
					 st->code[j].base, st->dest[j]);
			return -EINVAL;
		}
	}
	ua_strips_decide(ua, UA_POOLS_EMPTY,
			 "cold-started by this driver, code pool 0xe0000, tops 0x%05x 0x%05x 0x%08x",
			 st->layout.top[1], st->layout.top[2], st->layout.top[3]);
	return 0;
}

/* Give back what the strips hold in DSP 0's ledger (ua->lock) */
static void ua_strips_unhold(struct ua_device *ua, struct ua_strips *st,
			     bool *released)
{
	struct ua_host_dsp *d = &ua->host->dsp[ua_strips_dsp];
	unsigned int k, j, p;

	for (k = 0; k < st->n; k++) {
		if (d->pools)
			ua_plugin_unplace(ua, ua_strips_dsp, st->strip[k].priv,
					  UA_STRIP_PRIV);
		memset(st->strip[k].priv, 0, sizeof(st->strip[k].priv));
	}
	for (j = 0; j < UA_STRIP_MODS; j++) {
		bool last = false;

		if (st->code[j].cookie && d->pools)
			ua_pool_release_last(d->pools, &st->code[j], &last);
		if (released)
			released[j] = last;
		memset(&st->code[j], 0, sizeof(st->code[j]));
	}
	if (st->scratch_set && d->pools)
		for (p = 1; p < 4; p++)
			ua_pool_set_scratch(d->pools, p, 0);
	st->scratch_set = false;
}

/* Forget the strips (ua->lock): nothing of ours is on DSP 0 any more */
static void ua_strips_forget(struct ua_device *ua, struct ua_strips *st)
{
	unsigned int k, j;

	ua_strips_unhold(ua, st, NULL);
	for (k = 0; k < st->n; k++) {
		st->strip[k].state = UA_STRIP_NONE;
		st->strip[k].header = 0;
		st->strip[k].ack_ms = 0;
	}
	for (j = 0; j < UA_STRIP_MODS; j++)
		st->mod_state[j] = UA_MOD_NONE;
	if (st->state != UA_STRIPS_OFF)
		st->state = UA_STRIPS_IDLE;
}

/*
 * Strip @k's transaction (ua->lock held): its private rows from DSP 0's
 * ledger (they must be the recipe's), then steps 2-7.
 */
static struct ua_xact *ua_strips_prepare(struct ua_device *ua,
					 struct ua_strips *st, unsigned int k,
					 int *err)
{
	struct ua_host_dsp *d = &ua->host->dsp[ua_strips_dsp];
	struct ua_strip *s = &st->strip[k];
	u32 addr[UA_STRIP_PRIV], want[UA_STRIP_PRIV];
	struct ua_xact *x;
	unsigned int j;
	int ret;

	ua_strip_priv_addrs(&st->layout, k, want);
	for (j = 0; j < UA_STRIP_PRIV; j++) {
		ret = ua_pool_reserve(d->pools, ua_strip_rows[j].pool,
				      ua_strip_rows[j].dwords, &s->priv[j],
				      false, &s->priv[j]);
		if (ret)
			goto fail;
		addr[j] = s->priv[j].base;
		if (addr[j] != want[j]) {
			ret = -EINVAL;
			goto fail;
		}
	}
	x = ua_xact_alloc(ua_strips_dsp, 3 * UA_XACT_PIECE);
	if (!x) {
		ret = -ENOMEM;
		goto fail;
	}
	x->epoch = st->epoch;
	ret = ua_strip_build(x, addr, s->synch, &st->rate, input_strips_steps);
	if (ret) {
		ua_xact_discard(ua, x);
		goto fail;
	}
	s->header = addr[0];
	return x;
fail:
	ua_plugin_unplace(ua, ua_strips_dsp, s->priv, UA_STRIP_PRIV);
	memset(s->priv, 0, sizeof(s->priv));
	*err = ret;
	return NULL;
}

/* Upload the modules not resident yet (host->mutex, no ua->lock) */
static int ua_strips_upload(struct ua_device *ua, struct ua_strips *st,
			    u32 gen)
{
	enum ua_bill_form form = st->form == UA_STRIP_FORM_SDK ? UA_BILL_SDK :
						    UA_BILL_STRIP;
	unsigned int j;
	int ret;

	for (j = 0; j < UA_STRIP_MODS; j++) {
		mutex_lock(&ua->lock);
		if (st->gen != gen) {
			mutex_unlock(&ua->lock);
			return -ECANCELED;
		}
		st->mod_state[j] = UA_MOD_UPLOADING;
		mutex_unlock(&ua->lock);
		ret = ua_plugin_upload(ua, ua_strips_dsp, st->mod[j], st->dest[j], form,
				       st->epoch);
		mutex_lock(&ua->lock);
		if (st->gen != gen) {
			/* torn down meanwhile: the release went after it */
			if (st->mod_released[j])
				st->mod[j]->res[0].state = UA_RES_NONE;
			mutex_unlock(&ua->lock);
			return -ECANCELED;
		}
		/* a failed upload the DSP may have seen still needs its release */
		st->mod_state[j] = !ret ? UA_MOD_RESIDENT :
				   st->mod[j]->res[0].state != UA_RES_NONE ?
				   UA_MOD_UPLOADING : UA_MOD_NONE;
		mutex_unlock(&ua->lock);
		if (ret)
			return ret;
	}
	return 0;
}

/**
 * ua_strips_run - one strip pass (the host's work)
 * @ua: device (no lock held)
 *
 * Proves DSP 0's last teardown, checks every gate, uploads the modules
 * (each acknowledged), then one strip at a time: its transaction, waited
 * for with ua->lock released.  A teardown meanwhile ends the pass.
 */
void ua_strips_run(struct ua_device *ua)
{
	struct ua_plugin_host *h = ua->host;
	struct ua_strips *st = h ? h->strips : NULL;
	struct ua_xact *x;
	unsigned int k, dsp;
	unsigned long t0;
	u32 gen;
	bool live = false;
	int ret = 0;

	if (!st)
		return;
	mutex_lock(&h->mutex);
	ua_plugin_fence_wait(ua, ua_strips_dsp);
	mutex_lock(&ua->lock);
	if (st->state != UA_STRIPS_ARMED) {
		mutex_unlock(&ua->lock);
		goto out;
	}
	if (ua_strips_begin(ua, st)) {
		ua_strips_unhold(ua, st, NULL);
		mutex_unlock(&ua->lock);
		goto out;
	}
	st->passes++;
	memset(st->mod_released, 0, sizeof(st->mod_released));
	st->state = UA_STRIPS_CREATING;
	st->epoch = h->epoch;
	gen = st->gen;
	dev_info(&ua->pdev->dev,
		 "input strips: DSP0 pools empty, creating %u Mic Input Strip(s) at %u Hz\n",
		 st->n, st->rate.rate);
	mutex_unlock(&ua->lock);

	ret = ua_strips_upload(ua, st, gen);
	if (ret) {
		mutex_lock(&ua->lock);
		if (st->gen == gen)
			ua_strips_fail(ua, st, ret,
				       "a strip module was not accepted by DSP 0",
				       st->strip[0].input);
		mutex_unlock(&ua->lock);
		goto out;
	}

	for (k = 0; k < st->n && input_strips_steps != 1; k++) {
		mutex_lock(&ua->lock);
		if (st->gen != gen) {
			mutex_unlock(&ua->lock);
			goto out;
		}
		x = ua_strips_prepare(ua, st, k, &ret);
		if (x)
			st->strip[k].state = UA_STRIP_QUEUED;
		mutex_unlock(&ua->lock);
		if (!x) {
			mutex_lock(&ua->lock);
			ua_strips_fail(ua, st, ret, "a strip could not be laid out",
				       st->strip[k].input);
			mutex_unlock(&ua->lock);
			goto out;
		}
		t0 = jiffies;
		ret = ua_xact_run(ua, x, UA_XACT_WAIT_MS);
		mutex_lock(&ua->lock);
		if (x->state >= UA_XACT_PUBLISHED) {
			h->dsp[ua_strips_dsp].zero_fills += UA_STRIP_PRIV;
			h->dsp[ua_strips_dsp].memspecs++;
			h->dsp[ua_strips_dsp].writes += 2;
		}
		ua_xact_done(ua, x);
		if (st->gen != gen) {
			mutex_unlock(&ua->lock);
			goto out;
		}
		if (ret) {
			ua_strips_fail(ua, st, ret, ret == -ETIMEDOUT ?
				       "DSP 0 did not take a strip in time" :
				       "a strip could not be sent",
				       st->strip[k].input);
			mutex_unlock(&ua->lock);
			goto out;
		}
		st->strip[k].state = UA_STRIP_LIVE;
		st->strip[k].ack_ms = jiffies_to_msecs(jiffies - t0);
		/* The input is not routed from the DSPs that do not host it */
		for (dsp = 0; dsp < ua->num_dsps; dsp++) {
			if (dsp == ua_strips_dsp || !ua->dsp[dsp].rings_allocated)
				continue;
			ret = ua_dsp_ring_send_raw(ua, dsp, UA_STRIP_CMD_INPUT,
						   st->strip[k].synch,
						   UA_STRIP_INPUT_ROUTE,
						   UA_STRIP_ROUTE_NONE);
			if (ret)
				dev_warn(&ua->pdev->dev,
					 "input strips: route default for input %u on DSP %u not queued (%d)\n",
					 st->strip[k].input + 1, dsp, ret);
		}
		mutex_unlock(&ua->lock);
	}

	mutex_lock(&ua->lock);
	if (st->gen == gen) {
		st->state = UA_STRIPS_LIVE;
		st->created++;
		for (k = 0; k < st->n; k++)
			if (st->strip[k].state == UA_STRIP_LIVE)
				dev_info(&ua->pdev->dev,
					 "input strips: input %u: Mic Input Strip at 0x%05x on DSP %u (taken after %u ms)\n",
					 st->strip[k].input + 1,
					 st->strip[k].header, ua_strips_dsp,
					 st->strip[k].ack_ms);
	}
	/* 48V only after a cold start: a rate change keeps the supply up */
	live = st->gen == gen && h->phantom_pending;
	if (live)
		h->phantom_pending = false;
	mutex_unlock(&ua->lock);
	if (live && input_strips_phantom) {
		unsigned int mask = 0;

		for (k = 0; k < st->n; k++)
			mask |= BIT(st->strip[k].input);
		ua_preamp_phantom_resync(ua, mask);
	}
out:
	mutex_unlock(&h->mutex);
}

/**
 * ua_strips_teardown - take this driver's strips down (A6.9)
 * @ua: device (ua->lock held)
 * @why: for the log
 *
 * Before every disconnect, reconnect and rate change, and in the quiesce
 * of suspend, shutdown and remove: SYNCH with address 0 for every strip
 * that went out, then the release of each module whose last hold goes
 * (only modules this driver uploaded).  Published without waiting (the
 * lock is held); DSP 0's fence proves it later, or quarantines DSP 0.  A
 * pass that is running sees the generation change and stops.  A pass that
 * has not sent anything yet is just cancelled.
 */
void ua_strips_teardown(struct ua_device *ua, const char *why)
{
	struct ua_plugin_host *h = ua->host;
	struct ua_strips *st = h ? h->strips : NULL;
	bool released[UA_STRIP_MODS] = { false };
	unsigned int k, j, stops = 0, rels = 0;
	struct ua_xact *x;
	int ret;

	if (!st)
		return;
	lockdep_assert_held(&ua->lock);

	if (st->state == UA_STRIPS_ARMED) {
		st->state = UA_STRIPS_IDLE;
		return;
	}
	if (st->state != UA_STRIPS_CREATING && st->state != UA_STRIPS_LIVE)
		return;
	st->gen++;
	st->teardowns++;

	x = ua_xact_alloc(ua_strips_dsp, 0);
	if (x) {
		x->epoch = h->epoch;
		for (k = 0; k < st->n; k++)
			if (st->strip[k].state != UA_STRIP_NONE) {
				ua_strip_build_stop(x, st->strip[k].synch);
				stops++;
			}
	}
	ua_strips_unhold(ua, st, released);
	for (j = 0; j < UA_STRIP_MODS && x; j++) {
		st->mod_released[j] = false;
		if (st->mod_state[j] != UA_MOD_NONE && released[j]) {
			ua_release_build(x, &st->mod[j]->info);
			st->mod_released[j] = true;
			st->mod[j]->res[0].state = UA_RES_NONE;
			rels++;
		}
	}
	if (!x || (!stops && !rels)) {
		if (x)
			ua_xact_discard(ua, x);
		ua_strips_forget(ua, st);
		if (!x)
			ua_plugin_disown(ua, ua_strips_dsp, "no memory for the strips' teardown");
		return;
	}
	ret = ua_plugin_publish_now(ua, x);
	if (ret) {
		/* Nothing reaches DSP 0: what runs there is unknown */
		ua_plugin_disown(ua, ua_strips_dsp, "the strips' teardown could not be sent");
		dev_warn(&ua->pdev->dev,
			 "input strips: teardown not sent (%s, %d); DSP 0 is not used until the Apollo is power-cycled\n",
			 why, ret);
	} else {
		h->dsp[ua_strips_dsp].stops += stops;
		h->dsp[ua_strips_dsp].releases += rels;
		ua_plugin_fence_set(ua, ua_strips_dsp, x);
		dev_info(&ua->pdev->dev,
			 "input strips: %u strip(s) stopped%s (%s)\n", stops,
			 rels ? ", modules released" : "", why);
	}
	ua_strips_forget(ua, st);
}

/**
 * ua_strips_lost - the device's state is gone: forget the strips
 * @ua: device (ua->lock held)
 *
 * Nothing is sent; a pass in progress sees the generation change.
 */
void ua_strips_lost(struct ua_device *ua)
{
	struct ua_plugin_host *h = ua->host;
	struct ua_strips *st = h ? h->strips : NULL;

	if (!st)
		return;
	lockdep_assert_held(&ua->lock);
	if (st->state == UA_STRIPS_IDLE || st->state == UA_STRIPS_OFF)
		return;
	st->gen++;
	ua_strips_forget(ua, st);
	dev_info(&ua->pdev->dev,
		 "input strips: forgotten, the device's state is gone\n");
}

/**
 * ua_strips_service - the DSP service loop's share (input_strips_rebuild)
 * @ua: device (ua->lock held)
 */
void ua_strips_service(struct ua_device *ua)
{
	struct ua_plugin_host *h = ua->host;
	struct ua_strips *st = h ? h->strips : NULL;
	int want;

	if (!st)
		return;
	want = atomic_read(&ua_strips_rebuilds);
	if (want == st->rebuild_seen)
		return;
	if (st->state == UA_STRIPS_CREATING || st->state == UA_STRIPS_ARMED)
		return;		/* after the pass */
	st->rebuild_seen = want;
	if (st->state != UA_STRIPS_LIVE)
		return;
	ua_strips_teardown(ua, "rebuild requested");
	ua_strips_connected(ua);
}

/**
 * ua_strips_proc - the input strips block of /proc/asound/cardN/dsp_state
 * @ua: device (ua->lock held)
 * @buf: proc buffer
 */
void ua_strips_proc(struct ua_device *ua, struct snd_info_buffer *buf)
{
	static const char * const sname[] = { "-", "queued", "live" };
	static const char * const mname[] = { "not loaded by this driver",
					       "upload unproven", "resident" };
	struct ua_plugin_host *h = ua->host;
	struct ua_strips *st = h ? h->strips : NULL;
	struct ua_host_dsp *d;
	unsigned int k, j;

	if (!st) {
		snd_iprintf(buf, "\ninput strips\toff (input_strips=0)\n");
		return;
	}
	d = &h->dsp[ua_strips_dsp];
	snd_iprintf(buf, "\ninput strips\ton (input_strips=1, mask 0x%x, form %s), state %s\n",
		    input_strips_mask,
		    st->form == UA_STRIP_FORM_SDK ? "sdk" : "recipe",
		    ua_strips_state_name(st->state));
	if (st->state == UA_STRIPS_OFF) {
		snd_iprintf(buf, "  disabled: %s\n", st->off_why);
		return;
	}
	snd_iprintf(buf, "  pools: %s%s%s\n", ua_strips_pools_name(st->pools),
		    st->why[0] ? ": " : "", st->why);
	snd_iprintf(buf, "  DSP 0: %s%s, boot word 0x%08x at the last cold start\n",
		    d->owned ? "owned by this driver" : "not this driver's",
		    d->quarantined ? ", quarantined" : "", d->boot_word);
	if (st->fail_why[0])
		snd_iprintf(buf, "  last failure: %s\n", st->fail_why);
	snd_iprintf(buf, "  passes %u, completed %u, teardowns %u, failures %u, rate %u Hz\n",
		    st->passes, st->created, st->teardowns, st->failures,
		    st->rate.rate);
	for (k = 0; k < st->n; k++) {
		const struct ua_strip *s = &st->strip[k];

		snd_iprintf(buf, "  strip %u: input %u (DSP input %u), instance 0x%05x, %s",
			    k, s->input + 1, s->synch, s->header,
			    sname[s->state < ARRAY_SIZE(sname) ? s->state : 0]);
		if (s->state == UA_STRIP_LIVE)
			snd_iprintf(buf, " (taken after %u ms)", s->ack_ms);
		snd_iprintf(buf, "\n");
	}
	for (j = 0; j < UA_STRIP_MODS; j++) {
		if (st->mod_state[j] != UA_MOD_NONE)
			snd_iprintf(buf, "  module 0x%03x: %s at 0x%05x\n",
				    ua_strip_modules[j].id,
				    mname[st->mod_state[j]], st->dest[j]);
		else
			snd_iprintf(buf, "  module 0x%03x: %s\n",
				    ua_strip_modules[j].id, mname[0]);
	}
}
