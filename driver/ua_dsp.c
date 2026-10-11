// SPDX-License-Identifier: GPL-2.0-only
/*
 * Universal Audio Apollo — DSP Ring Buffer & Firmware Loading
 *
 * Copyright (c) 2026 apollo-linux contributors
 *
 * Reverse-engineered from UAD2System.kext v11.8.1:
 *   CPcieRingBuffer — ring buffer management
 *   CPcieDevice::_loadFirmware — firmware DMA transfer protocol
 *   CPcieDevice::_connectDSPs — DSP handshake
 *
 * Each SHARC DSP has a 128-byte register bank split into two rings:
 *   cmd ring  at bank_base + 0x00 (host → DSP commands)
 *   resp ring at bank_base + 0x40 (DSP → host responses)
 *
 * Ring entry format (16 bytes):
 *   Inline command: { cmd_code, param, 0, 0 }
 *   DMA reference:  { size_dwords | BIT(31), 0, bus_addr_lo, bus_addr_hi }
 *
 * Firmware loading protocol:
 *   1. Allocate DMA pages for command header + firmware data + response
 *   2. Build command words (0x120000 cmd, 0x80040000 param)
 *   3. Submit DMA reference entries to cmd ring + resp ring
 *   4. Ring doorbell, poll response buffer for 0x8004xxxx
 */

#include <linux/delay.h>
#include <linux/dma-mapping.h>
#include <linux/firmware.h>
#include <linux/log2.h>
#include <linux/math64.h>
#include <linux/module.h>
#include <linux/sched/signal.h>
#include <linux/slab.h>
#include <linux/timekeeping.h>
#include <linux/workqueue.h>
#include <sound/core.h>
#include <sound/info.h>

#include "ua_dsp_modules.h"

#include "ua_apollo.h"

/* Advertise the blobs so initramfs tools and distro packaging include them. */
MODULE_FIRMWARE(UA_MIXER_FW_NAME);
MODULE_FIRMWARE(UA_PLUGIN_CHAIN_FW_NAME);

/* Defined with the DSP vector code at the end of the file */
static void ua_dsp_msg_work(struct work_struct *work);
static void ua_dsp_irq_disarm(struct ua_device *ua);
static bool ua_dsp_prop_ring_connect(const struct ua_device *ua);

/* Internal ring entry layout */
struct ua_ring_entry {
	__le32 word0;   /* cmd_code OR (size_dwords | 0x80000000) */
	__le32 word1;   /* param OR 0 (DMA ref) */
	__le32 word2;   /* 0 OR bus_addr_lo (DMA ref) */
	__le32 word3;   /* 0 OR bus_addr_hi (DMA ref) */
};

/* ----------------------------------------------------------------
 * Ring buffer allocation / deallocation
 * ---------------------------------------------------------------- */

static int ua_dsp_ring_alloc(struct ua_device *ua, struct ua_dsp_ring *ring,
			     unsigned int num_pages, u32 reg_base)
{
	unsigned int i;

	if (num_pages > UA_RING_MAX_PAGES)
		return -EINVAL;

	memset(ring, 0, sizeof(*ring));
	ring->num_pages = num_pages;
	ring->capacity = num_pages * UA_RING_ENTRIES_PER_PAGE;
	ring->reg_base = reg_base;

	/* Index arithmetic below masks with capacity - 1 */
	if (!is_power_of_2(ring->capacity))
		return -EINVAL;

	for (i = 0; i < num_pages; i++) {
		ring->pages[i] = dma_alloc_coherent(&ua->pdev->dev,
						     UA_PCIE_PAGE_SIZE,
						     &ring->page_addrs[i],
						     GFP_KERNEL);
		if (!ring->pages[i])
			goto err_free;
		memset(ring->pages[i], 0, UA_PCIE_PAGE_SIZE);
	}

	return 0;

err_free:
	while (i--) {
		dma_free_coherent(&ua->pdev->dev, UA_PCIE_PAGE_SIZE,
				  ring->pages[i], ring->page_addrs[i]);
		ring->pages[i] = NULL;
	}
	ring->num_pages = 0;
	ring->capacity = 0;
	return -ENOMEM;
}

static void ua_dsp_ring_free(struct ua_device *ua, struct ua_dsp_ring *ring)
{
	unsigned int i;

	for (i = 0; i < ring->num_pages; i++) {
		if (ring->pages[i]) {
			dma_free_coherent(&ua->pdev->dev, UA_PCIE_PAGE_SIZE,
					  ring->pages[i], ring->page_addrs[i]);
			ring->pages[i] = NULL;
		}
	}
	ring->num_pages = 0;
	ring->capacity = 0;
	ring->write_ptr = 0;
	ring->hw_pos = 0;
}

/* ----------------------------------------------------------------
 * Circular ring bookkeeping
 *
 * Mirrors CPcieRingBuffer::Service (kext 12.0.0 x86 0xad12):
 *   - POSITION (+0x28) is the FPGA's consumer index, 0..1023; a read
 *     >= 1024 is invalid and ignored (the last valid value is kept);
 *   - free slots = (POSITION - write_ptr - 1) mod 1024, so one slot
 *     always stays empty and write_ptr == POSITION means "idle";
 *   - the producer index wraps with & 0x3ff.
 * ---------------------------------------------------------------- */

static unsigned int ua_dsp_ring_free_slots(const struct ua_dsp_ring *ring)
{
	return (ring->hw_pos - ring->write_ptr - 1) & (ring->capacity - 1);
}

/* Refresh hw_pos from POSITION.  -ENODEV when BAR0 reads all-ones. */
static int ua_dsp_ring_update_pos(struct ua_device *ua,
				  struct ua_dsp_ring *ring)
{
	u32 pos = ua_read(ua, ring->reg_base + UA_RING_POSITION);

	if (pos == 0xFFFFFFFF)
		return -ENODEV;
	if (pos < ring->capacity)
		ring->hw_pos = pos;
	return 0;
}

/* Entries queued but not yet consumed by the FPGA (as of the last read) */
static unsigned int ua_dsp_ring_pending(const struct ua_dsp_ring *ring)
{
	return (ring->write_ptr - ring->hw_pos) & (ring->capacity - 1);
}

/**
 * ua_dsp_ring_wait_space - Wait until @n entries can be queued
 * @ua: device
 * @ring: target ring
 * @n: entries needed (at most capacity - 1)
 * @timeout_ms: how long to wait for the FPGA to consume entries
 *
 * Returns 0, -ENOSPC on timeout, -E2BIG if @n can never fit, -ENODEV if
 * the device is gone.
 */
static int ua_dsp_ring_wait_space(struct ua_device *ua,
				  struct ua_dsp_ring *ring,
				  unsigned int n, unsigned int timeout_ms)
{
	unsigned int waited = 0;
	int ret;

	if (n >= ring->capacity)
		return -E2BIG;

	for (;;) {
		if (ua_dsp_ring_free_slots(ring) >= n)
			return 0;
		ret = ua_dsp_ring_update_pos(ua, ring);
		if (ret)
			return ret;
		if (ua_dsp_ring_free_slots(ring) >= n)
			return 0;
		if (waited >= timeout_ms)
			return -ENOSPC;
		usleep_range(1000, 1500);
		waited++;
	}
}

/*
 * How long a command-buffer send waits for the command ring to consume it:
 * the kext's 6000 ms (parity/13 R66).  A command buffer is one DMA ref entry
 * (plus its inline header) queued on the command ring only; the wait is for
 * the ring to pass it, not for an answer.  Ordinary inline commands keep
 * their short waits.  Only a slow transfer ever waits this long.
 */
#define UA_DSP_CMDBUF_WAIT_MS	6000

/**
 * ua_dsp_ring_wait_idle - Wait until the FPGA has consumed every entry
 * @ua: device
 * @ring: ring to watch
 * @timeout_ms: upper bound (1 ms polls)
 *
 * Returns true when POSITION == write_ptr.  Replaces the old linear
 * "POSITION >= target" checks, which break once the index wraps.
 */
static bool ua_dsp_ring_wait_idle(struct ua_device *ua,
				  struct ua_dsp_ring *ring,
				  unsigned int timeout_ms)
{
	unsigned int waited;

	for (waited = 0; ; waited++) {
		if (ua_dsp_ring_update_pos(ua, ring))
			return false;
		if (!ua_dsp_ring_pending(ring))
			return true;
		if (waited >= timeout_ms)
			return false;
		usleep_range(1000, 1500);
	}
}

/* Entries consumed since the producer index was @base (debug tests) */
static unsigned int ua_dsp_ring_progress(const struct ua_dsp_ring *ring,
					 unsigned int base, u32 pos)
{
	return (pos - base) & (ring->capacity - 1);
}

/* ----------------------------------------------------------------
 * Buffers still owned by the device
 *
 * A command that times out may leave DMA_REF descriptors the FPGA has
 * not fetched yet, and a response descriptor it has not filled.  The
 * ring is circular, so those descriptors are still consumed later and
 * the buffers they point at must outlive the command.  Such a buffer is
 * parked here with, for each ring that references it, the producer index
 * just past its entries ("end") and the ring's generation.
 *
 * It is freed once every referencing ring is provably done with it:
 *  - same generation: POSITION has moved past end.  The FPGA has then
 *    consumed the entries — read a DMA_REF payload, or filled a response
 *    descriptor (CPcieRingBuffer::Service, x86 0xad12, completes
 *    descriptors from the POSITION delta for both ring directions);
 *  - abandoned by a resync (DOORBELL pulled back to POSITION while the
 *    entries were still queued, ua_dsp_ring_resync()): once the FPGA has
 *    consumed, past the resync point, as many entries as had been
 *    abandoned up to end.  An engine that had already fetched one of the
 *    abandoned descriptors has used it up by then.
 * A second abandonment before that point leaves nothing to prove it by;
 * the buffer is then held until ua_dsp_rings_fini() at remove, which
 * frees it only if the device can no longer write (see
 * ua_dsp_free_orphans()).
 *
 * The list is capped (UA_DMA_ORPHAN_MAX buffers, UA_DMA_ORPHAN_MAX_BYTES):
 * at the cap, commands that need new DMA buffers fail with -EBUSY instead
 * of pinning coherent memory without bound.  Caller holds ua->lock (or
 * has exclusive access at probe/remove).
 * ---------------------------------------------------------------- */

#define UA_DMA_ORPHAN_MAX	16
#define UA_DMA_ORPHAN_MAX_BYTES	(8U << 20)
#define UA_DMA_ORPHAN_REFS	2

struct ua_orphan_ref {
	struct ua_dsp_ring *ring;	/* NULL: this ring is done with it */
	u32 gen;			/* ring generation the indices are in */
	unsigned int end;		/* producer index just past its entries */
	unsigned int start;		/* abandoned: POSITION at the resync */
	unsigned int need;		/* abandoned: entries to consume after */
	bool abandoned;
};

struct ua_dma_orphan {
	struct list_head node;
	void *cpu;
	dma_addr_t dma;
	size_t size;
	struct ua_orphan_ref ref[UA_DMA_ORPHAN_REFS];
};

/* Is @ref's ring done with the buffer?  Re-reads POSITION. */
static bool ua_orphan_ref_done(struct ua_device *ua, struct ua_orphan_ref *ref)
{
	struct ua_dsp_ring *ring = ref->ring;
	unsigned int mask, d_end;

	if (!ring)
		return true;
	if (!ring->capacity || ring->gen != ref->gen)
		return false;			/* held until fini */
	if (ua_dsp_ring_update_pos(ua, ring))
		return false;

	mask = ring->capacity - 1;
	if (ref->abandoned)
		return ((ring->hw_pos - ref->start) & mask) >= ref->need;

	/* Consumed once end is no longer inside (hw_pos, write_ptr] */
	d_end = (ref->end - ring->hw_pos) & mask;
	return !d_end || d_end > ua_dsp_ring_pending(ring);
}

static void ua_dsp_orphan_free(struct ua_device *ua, struct ua_dma_orphan *o)
{
	list_del(&o->node);
	dma_free_coherent(&ua->pdev->dev, o->size, o->cpu, o->dma);
	ua->dma_orphan_count--;
	ua->dma_orphan_bytes -= o->size;
	kfree(o);
}

/* Free every parked buffer whose rings are provably done with it */
static void ua_dsp_reclaim_orphans(struct ua_device *ua)
{
	struct ua_dma_orphan *o, *tmp;
	unsigned int i;
	bool busy;

	list_for_each_entry_safe(o, tmp, &ua->dma_orphans, node) {
		busy = false;
		for (i = 0; i < UA_DMA_ORPHAN_REFS; i++) {
			if (ua_orphan_ref_done(ua, &o->ref[i]))
				o->ref[i].ring = NULL;
			else
				busy = true;
		}
		if (!busy) {
			dev_dbg(&ua->pdev->dev,
				"reclaimed %zu-byte DMA buffer from the rings\n",
				o->size);
			ua_dsp_orphan_free(ua, o);
		}
	}
}

/**
 * ua_dsp_orphan_dma_gen - Park a buffer the rings may still use
 * @ua: device
 * @cpu: CPU address of the coherent buffer (nothing done if NULL)
 * @dma: its bus address
 * @size: its size
 * @r0: a ring holding entries that reference it (may be NULL)
 * @g0: generation of @r0 when the buffer was first queued on it
 * @r1: a second such ring (may be NULL)
 * @g1: generation of @r1 when the buffer was first queued on it
 *
 * A ring whose generation moved since @g0/@g1 abandoned entries that may
 * have referenced the buffer, so it is then held until fini.  Everything
 * up to each ring's current producer index counts as referencing the
 * buffer.  Frees it at once if the rings are already done with it.
 */
static void ua_dsp_orphan_dma_gen(struct ua_device *ua, void *cpu,
				  dma_addr_t dma, size_t size,
				  struct ua_dsp_ring *r0, u32 g0,
				  struct ua_dsp_ring *r1, u32 g1)
{
	struct ua_dsp_ring *rings[UA_DMA_ORPHAN_REFS] = { r0, r1 };
	u32 gens[UA_DMA_ORPHAN_REFS] = { g0, g1 };
	struct ua_dma_orphan *o;
	unsigned int i;

	if (!cpu)
		return;

	o = kzalloc(sizeof(*o), GFP_KERNEL);
	if (!o) {
		/* Leak rather than free memory the device may still write */
		dev_warn(&ua->pdev->dev,
			 "leaking %zu-byte DMA buffer still referenced by the ring\n",
			 size);
		return;
	}
	o->cpu = cpu;
	o->dma = dma;
	o->size = size;
	for (i = 0; i < UA_DMA_ORPHAN_REFS; i++) {
		if (!rings[i])
			continue;
		o->ref[i].ring = rings[i];
		o->ref[i].gen = gens[i];
		o->ref[i].end = rings[i]->write_ptr;
	}
	list_add_tail(&o->node, &ua->dma_orphans);
	ua->dma_orphan_count++;
	ua->dma_orphan_bytes += size;

	ua_dsp_reclaim_orphans(ua);
}

/* Park a buffer referenced by entries queued in the current generation */
static void ua_dsp_orphan_dma(struct ua_device *ua, void *cpu,
			      dma_addr_t dma, size_t size,
			      struct ua_dsp_ring *r0, struct ua_dsp_ring *r1)
{
	ua_dsp_orphan_dma_gen(ua, cpu, dma, size,
			      r0, r0 ? r0->gen : 0, r1, r1 ? r1->gen : 0);
}

/*
 * Teardown: drop every parked buffer.  @dma_quiet: the device can no
 * longer write to any of them — it is gone, or its DMA engines were reset
 * (full_shutdown) after the rings were emptied.  Otherwise a buffer whose
 * rings never proved they were done with it may still be the target of a
 * descriptor the FPGA fetched before the quiesce (only an engine reset
 * would cancel that), and a later probe turns bus mastering back on:
 * such buffers are leaked, not freed — bounded by the orphan cap, and
 * only after DSP commands timed out.
 */
static void ua_dsp_free_orphans(struct ua_device *ua, bool dma_quiet)
{
	struct ua_dma_orphan *o, *tmp;
	unsigned int i, leaked = 0;
	size_t bytes = 0;
	bool busy;

	list_for_each_entry_safe(o, tmp, &ua->dma_orphans, node) {
		busy = false;
		for (i = 0; i < UA_DMA_ORPHAN_REFS; i++)
			busy |= o->ref[i].ring != NULL;

		if (dma_quiet || !busy) {
			ua_dsp_orphan_free(ua, o);
			continue;
		}
		leaked++;
		bytes += o->size;
		list_del(&o->node);
		kfree(o);
	}

	if (leaked)
		dev_warn(&ua->pdev->dev,
			 "leaking %u DMA buffers (%zu bytes) a DSP ring may still write; load with full_shutdown=1 to reset the DMA engines on unload instead\n",
			 leaked, bytes);
	ua->dma_orphan_count = 0;
	ua->dma_orphan_bytes = 0;
}

/*
 * Gate for commands that allocate DMA buffers: -EBUSY while the parked
 * buffers are at the cap (a ring that stopped consuming), so repeated
 * timeouts fail cleanly instead of exhausting coherent memory.
 */
static int ua_dsp_orphan_room(struct ua_device *ua)
{
	if (list_empty(&ua->dma_orphans))
		return 0;

	ua_dsp_reclaim_orphans(ua);
	if (ua->dma_orphan_count < UA_DMA_ORPHAN_MAX &&
	    ua->dma_orphan_bytes < UA_DMA_ORPHAN_MAX_BYTES)
		return 0;

	dev_warn_ratelimited(&ua->pdev->dev,
			     "%u DMA buffers (%zu bytes) still referenced by DSP rings that stopped consuming; refusing DMA commands until the device is reset\n",
			     ua->dma_orphan_count, ua->dma_orphan_bytes);
	return -EBUSY;
}

/*
 * @ring is being resynced with entries still queued: ring->hw_pos is the
 * resync point (POSITION), ring->write_ptr the old producer index.
 * Settle the parked buffers' references to it, then start a generation.
 */
static void ua_dsp_orphans_abandon(struct ua_device *ua,
				   struct ua_dsp_ring *ring)
{
	unsigned int mask = ring->capacity - 1;
	u32 next = ring->gen + 1;
	struct ua_dma_orphan *o;
	unsigned int i, d_end;

	list_for_each_entry(o, &ua->dma_orphans, node) {
		for (i = 0; i < UA_DMA_ORPHAN_REFS; i++) {
			struct ua_orphan_ref *ref = &o->ref[i];

			if (ref->ring != ring || ref->gen != ring->gen)
				continue;

			if (ref->abandoned) {
				/* Proven since the last resync, or held */
				if (((ring->hw_pos - ref->start) & mask) >=
				    ref->need)
					ref->ring = NULL;
				continue;
			}

			d_end = (ref->end - ring->hw_pos) & mask;
			if (!d_end || d_end > ua_dsp_ring_pending(ring)) {
				ref->ring = NULL;	/* consumed already */
				continue;
			}
			ref->abandoned = true;
			ref->start = ring->hw_pos;
			ref->need = d_end;
			ref->gen = next;
		}
	}
	ring->gen = next;
}

/* ----------------------------------------------------------------
 * Ring buffer hardware programming
 * ---------------------------------------------------------------- */

/* Mid-session reprogram: how long to let queued entries drain first */
#define UA_RING_DRAIN_MS	20

/*
 * Register half of CPcieRingBuffer::ProgramRegisters (kext 12.0.0 x86
 * 0xa9cc): read POSITION (an index >= 0x400 is invalid and becomes 0),
 * then DOORBELL (+0x24) = WRITE_PTR (+0x20) = POSITION.  DOORBELL is the
 * FPGA's target; left at an older, higher value it would let the FPGA
 * keep consuming slots the producer, now back at POSITION, is about to
 * overwrite.  If an entry completes under us and POSITION moves on, follow
 * it (bounded) so target and position end up equal.
 *
 * The old "never write DOORBELL here" rule came from writing DOORBELL = 0
 * with POSITION elsewhere, i.e. a target ~1000 entries of stale slots
 * ahead; DOORBELL = POSITION means "nothing queued", as in the kext.
 *
 * Returns the index, or -ENODEV when BAR0 reads all-ones.
 */
static int ua_dsp_ring_regs_resync(struct ua_device *ua, u32 ring_base)
{
	u32 pos = ua_read(ua, ring_base + UA_RING_POSITION);
	unsigned int tries;
	u32 next;

	if (pos == 0xFFFFFFFF)
		return -ENODEV;
	if (pos >= UA_RING_MAX_SIZE)
		pos = 0;

	for (tries = 0; tries < 3; tries++) {
		ua_write(ua, ring_base + UA_RING_DOORBELL, pos);
		ua_write(ua, ring_base + UA_RING_WRITE_PTR, pos);

		next = ua_read(ua, ring_base + UA_RING_POSITION);
		if (next == 0xFFFFFFFF)
			return -ENODEV;
		if (next >= UA_RING_MAX_SIZE)
			next = 0;
		if (next == pos)
			return pos;
		pos = next;
	}

	/* Still moving after three passes: settle on the latest index */
	ua_write(ua, ring_base + UA_RING_DOORBELL, pos);
	ua_write(ua, ring_base + UA_RING_WRITE_PTR, pos);
	return pos;
}

/*
 * Resync @ring's producer to the FPGA's consumer index (shared by
 * ua_dsp_ring_program() and ua_dsp_ring_reset()).  Entries still queued
 * are abandoned and the buffers they reference stay parked (see above).
 * Returns the index or -ENODEV (ring state untouched).
 */
static int ua_dsp_ring_resync(struct ua_device *ua, struct ua_dsp_ring *ring)
{
	int pos = ua_dsp_ring_regs_resync(ua, ring->reg_base);

	if (pos < 0)
		return pos;

	if (ring->live && ring->capacity) {
		ring->hw_pos = pos;
		if (ua_dsp_ring_pending(ring)) {
			dev_dbg(&ua->pdev->dev,
				"ring @0x%04x: resync abandons %u entries (pos=%d wr=%u)\n",
				ring->reg_base, ua_dsp_ring_pending(ring), pos,
				ring->write_ptr);
			ua_dsp_orphans_abandon(ua, ring);
		}
	}
	ring->write_ptr = pos;
	ring->hw_pos = pos;
	return pos;
}

/**
 * __ua_dsp_ring_program - Point a ring at its pages and resync it
 * @ua: device
 * @ring: ring buffer to program
 * @ring_base: BAR0 offset of the ring's register block (cmd or resp)
 * @drain: let queued entries drain (UA_RING_DRAIN_MS) before resyncing
 *
 * Mirrors CPcieRingBuffer::ProgramRegisters() (kext 12.0.0 x86 0xa9cc):
 *   Resync DOORBELL and WRITE_PTR to POSITION (never rewinds the producer
 *   behind the consumer; POSITION survives a page/WRITE_PTR rewrite, so
 *   the old "write_ptr = 0" made the FPGA walk round to new entries)
 *   Write page addresses to PAGE0_LO..PAGE3_HI (0x00..0x1C) and verify
 *   Zero the ring pages
 */
static void __ua_dsp_ring_program(struct ua_device *ua,
				  struct ua_dsp_ring *ring,
				  u32 ring_base, bool drain)
{
	static const u32 page_offsets[] = {
		UA_RING_PAGE0_LO, UA_RING_PAGE0_HI,
		UA_RING_PAGE1_LO, UA_RING_PAGE1_HI,
		UA_RING_PAGE2_LO, UA_RING_PAGE2_HI,
		UA_RING_PAGE3_LO, UA_RING_PAGE3_HI,
	};
	unsigned int i;
	int pos;

	if (drain && ring->live &&
	    !ua_dsp_ring_wait_idle(ua, ring, UA_RING_DRAIN_MS))
		dev_warn_ratelimited(&ua->pdev->dev,
				     "ring @0x%04x: %u entries not consumed in %u ms, abandoning them\n",
				     ring_base, ua_dsp_ring_pending(ring),
				     UA_RING_DRAIN_MS);

	pos = ua_dsp_ring_resync(ua, ring);

	dev_dbg(&ua->pdev->dev,
		"ring @0x%04x: program write_ptr=%d\n", ring_base, pos);

	/* Write page addresses (zeroed for unused pages) */
	for (i = 0; i < UA_RING_MAX_PAGES; i++) {
		u64 addr = (i < ring->num_pages) ? ring->page_addrs[i] : 0;

		ua_write(ua, ring_base + page_offsets[i * 2],
			 lower_32_bits(addr));
		ua_write(ua, ring_base + page_offsets[i * 2 + 1],
			 upper_32_bits(addr));
	}

	/* Verify page addresses read back correctly */
	for (i = 0; i < ring->num_pages; i++) {
		u32 rd_lo = ua_read(ua, ring_base + page_offsets[i * 2]);
		u32 rd_hi = ua_read(ua, ring_base + page_offsets[i * 2 + 1]);
		u32 wr_lo = lower_32_bits(ring->page_addrs[i]);
		u32 wr_hi = upper_32_bits(ring->page_addrs[i]);

		if (rd_lo != wr_lo || rd_hi != wr_hi)
			dev_warn(&ua->pdev->dev,
				 "ring @0x%04x page%u MISMATCH: "
				 "wrote %08x:%08x read %08x:%08x\n",
				 ring_base, i, wr_hi, wr_lo, rd_hi, rd_lo);
	}

	/* Zero ring pages (abandoned slots must not hold descriptors) */
	for (i = 0; i < ring->num_pages; i++)
		memset(ring->pages[i], 0, UA_PCIE_PAGE_SIZE);

	ring->live = pos >= 0;
}

/* Mid-session (re)program: drains queued entries first */
static void ua_dsp_ring_program(struct ua_device *ua,
				struct ua_dsp_ring *ring,
				u32 ring_base)
{
	__ua_dsp_ring_program(ua, ring, ring_base, true);
}

/* ----------------------------------------------------------------
 * Ring entry submission
 * ---------------------------------------------------------------- */

/**
 * ua_dsp_ring_put_entry - Append a 16-byte entry to the circular ring
 * @ua: device (POSITION is re-read when the cached view says full)
 * @ring: target ring buffer
 * @entry: 16-byte entry to copy
 *
 * Does not wait: callers queuing a batch reserve room first with
 * ua_dsp_ring_wait_space().
 *
 * Returns 0 on success, -ENOSPC if the FPGA has not freed a slot,
 * -ENODEV if the device is gone.
 */
static int ua_dsp_ring_put_entry(struct ua_device *ua,
				 struct ua_dsp_ring *ring,
				 const struct ua_ring_entry *entry)
{
	unsigned int page_idx, offset;
	struct ua_ring_entry *slot;
	int ret;

	if (!ua_dsp_ring_free_slots(ring)) {
		ret = ua_dsp_ring_update_pos(ua, ring);
		if (ret)
			return ret;
		if (!ua_dsp_ring_free_slots(ring))
			return -ENOSPC;
	}

	page_idx = ring->write_ptr / UA_RING_ENTRIES_PER_PAGE;
	offset = (ring->write_ptr % UA_RING_ENTRIES_PER_PAGE) *
		 UA_RING_ENTRY_SIZE;

	slot = (struct ua_ring_entry *)((u8 *)ring->pages[page_idx] + offset);
	*slot = *entry;

	ring->write_ptr = (ring->write_ptr + 1) & (ring->capacity - 1);
	return 0;
}

/**
 * ua_dsp_ring_doorbell - Notify DSP of new entries
 * @ua: device
 * @ring: ring that has new entries
 * @ring_base: BAR0 offset of the ring's register block
 *
 * Kext CPcieRingBuffer::Service() writes DOORBELL (0x24) FIRST,
 * then WRITE_PTR (0x20).  This order matters: DOORBELL sets the
 * target write pointer, WRITE_PTR triggers processing.  Writing
 * WRITE_PTR first (our previous code) caused the FPGA to begin
 * DMA reads immediately, and the subsequent DOORBELL write during
 * an active large DMA ref transfer reset the DMA engine, causing
 * permanent ring stalls for payloads > ~1.5 KB.
 */
static void ua_dsp_ring_doorbell(struct ua_device *ua,
				 struct ua_dsp_ring *ring,
				 u32 ring_base)
{
	/* Ensure all ring writes are visible before doorbell */
	wmb();

	/*
	 * Kext Service() order: DOORBELL (0x24) first, WRITE_PTR (0x20) second.
	 * DOORBELL tells FPGA "process up to this write_ptr" immediately.
	 * WRITE_PTR persists the value for re-reads.
	 */
	ua_write(ua, ring_base + UA_RING_DOORBELL, ring->write_ptr);
	ua_write(ua, ring_base + UA_RING_WRITE_PTR, ring->write_ptr);

	dev_dbg(&ua->pdev->dev,
		"ring @0x%04x: doorbell w_ptr=%u\n",
		ring_base, ring->write_ptr);
}

/*
 * Date word of the connect-finalize command (CUAD2Device::_connectToDsps,
 * kext 12.0.0 x86 0x13ef5):
 *   w1 = (days_since_2007 << 12) + minute_of_day
 * The day count starts at 2007-01-01 00:00 UTC, not 1970 (parity/18
 * section 6.2; the Windows driver agrees and sends 0 for a clock before
 * 2007).  The Unix-epoch count this used to send was 13514 days too large.
 */
#define UA_DSP_DATE_EPOCH	1167609600LL	/* 2007-01-01 00:00:00 UTC */

static u32 ua_dsp_connect_date(void)
{
	time64_t now = ktime_get_real_seconds() - UA_DSP_DATE_EPOCH;
	u32 secs_of_day;
	u64 days;

	if (now < 0)
		return 0;
	days = div_u64_rem((u64)now, 86400, &secs_of_day);

	return ((u32)days << 12) + secs_of_day / 60;
}

/*
 * dma_per_dsp: set only the enable bit of each DSP that exists in DMA_CTRL
 * (kext EnableDMA(idx)) instead of OR-ing 0x1FFFE, which also asserts the
 * eight reset strobes and leaves them in the cached value (issue #46).
 * Off: the long-standing x4 behaviour, byte for byte.
 */
static bool dma_per_dsp = true;
module_param(dma_per_dsp, bool, 0444);
MODULE_PARM_DESC(dma_per_dsp,
		 "Ring init sets one DMA_CTRL enable bit per DSP (macOS EnableDMA) and never leaves a reset strobe set (default: on; 0 = the old OR of 0x1FFFE, which after a cold start can leave DMA_CTRL at 0x1ffff with every reset strobe asserted and the DSP command rings unserviced)");

/* ----------------------------------------------------------------
 * Ring init / cleanup for all DSPs
 * ---------------------------------------------------------------- */

/**
 * ua_dsp_rings_init - Allocate and program rings for all DSPs
 * @ua: device
 *
 * Every command and response ring is 4 pages (1024 entries), as in the
 * kext.  Rings that are already allocated (resume) are only reprogrammed.
 *
 * Non-fatal on failure — driver still works for register access.
 */
int ua_dsp_rings_init(struct ua_device *ua)
{
	unsigned int i;
	int ret;

	if (!ua->dspx.inited) {
		INIT_WORK(&ua->dspx.msg_work, ua_dsp_msg_work);
		ua->dspx.inited = true;
	}

	for (i = 0; i < ua->num_dsps; i++) {
		struct ua_dsp_state *ds = &ua->dsp[i];
		u32 bank_base = ua_dsp_base(i);

		if (!ds->rings_allocated) {
			ret = ua_dsp_ring_alloc(ua, &ds->cmd,
						UA_RING_NUM_PAGES,
						bank_base + UA_RING_CMD_OFFSET);
			if (ret) {
				dev_warn(&ua->pdev->dev,
					 "DSP %u: cmd ring alloc failed (%d)\n",
					 i, ret);
				continue;
			}

			ret = ua_dsp_ring_alloc(ua, &ds->resp,
						UA_RING_NUM_PAGES,
						bank_base + UA_RING_RESP_OFFSET);
			if (ret) {
				dev_warn(&ua->pdev->dev,
					 "DSP %u: resp ring alloc failed (%d)\n",
					 i, ret);
				ua_dsp_ring_free(ua, &ds->cmd);
				continue;
			}
		}

		/* Program hardware registers */
		dev_dbg(&ua->pdev->dev,
			"DSP %u: programming rings at BAR0+0x%04x "
			"(cmd @+0x%02x, resp @+0x%02x)\n",
			i, bank_base,
			UA_RING_CMD_OFFSET, UA_RING_RESP_OFFSET);
		dev_dbg(&ua->pdev->dev,
			"DSP %u: cmd page0 DMA=%pad, resp page0 DMA=%pad\n",
			i, &ds->cmd.page_addrs[0], &ds->resp.page_addrs[0]);

		/* Fresh, or quiesced and neutralized: nothing to drain */
		__ua_dsp_ring_program(ua, &ds->cmd,
				      bank_base + UA_RING_CMD_OFFSET, false);
		__ua_dsp_ring_program(ua, &ds->resp,
				      bank_base + UA_RING_RESP_OFFSET, false);

		ds->rings_allocated = true;

		/* Verify ring position registers are accessible */
		{
			u32 cmd_pos = ua_read(ua, bank_base +
					      UA_RING_CMD_OFFSET +
					      UA_RING_POSITION);
			u32 resp_pos = ua_read(ua, bank_base +
					       UA_RING_RESP_OFFSET +
					       UA_RING_POSITION);
			dev_dbg(&ua->pdev->dev,
				"DSP %u: rings ready (cmd=%u resp=%u entries) "
				"pos: cmd=%u resp=%u\n",
				i, ds->cmd.capacity, ds->resp.capacity,
				cmd_pos, resp_pos);
		}
	}

	/*
	 * Enable per-channel DMA engines — required for ring buffer
	 * scatter-gather reads.  Without this, the FPGA won't process
	 * ring entries (POSITION stays at 0).
	 */
	if (ua->fw_v2 && dma_per_dsp) {
		/*
		 * CPcieDSP::ProgramRegisters -> EnableDMA(idx): one enable
		 * bit (idx + 1) per DSP that exists, never a strobe.  The
		 * kick below keeps its reset-while-enabled order but pulses
		 * only those DSPs' strobes, and the value left behind has no
		 * strobe set -- the all-bits OR left DMA_CTRL reading
		 * 0x1ffff (issue #46, x8p).
		 */
		u32 en = 0, dma;

		for (i = 0; i < ua->num_dsps; i++)
			if (ua->dsp[i].rings_allocated)
				en |= BIT(i + 1);

		dma = ua_read(ua, UA_REG_DMA_CTRL);
		dev_info(&ua->pdev->dev,
			 "DMA_CTRL before per-DSP enable: 0x%08x (enables 0x%x)\n",
			 dma, en);
		ua_write(ua, UA_REG_DMA_CTRL,
			 (dma & ~UA_DMA_RESET_V2_MASK) | en);
		ua->dma_ctrl_cache = ua_read(ua, UA_REG_DMA_CTRL) &
				     ~UA_DMA_RESET_V2_MASK;
		ua_write(ua, UA_REG_DMA_CTRL,
			 ua->dma_ctrl_cache | (en << 8));
		ua_write(ua, UA_REG_DMA_CTRL, ua->dma_ctrl_cache);
		(void)ua_read(ua, UA_REG_DMA_CTRL);	/* flush */
		dev_info(&ua->pdev->dev,
			 "DMA_CTRL after per-DSP enable: 0x%08x\n",
			 ua_read(ua, UA_REG_DMA_CTRL));

		for (i = 0; i < ua->num_dsps; i++) {
			if (!ua->dsp[i].rings_allocated)
				continue;
			ua_dsp_ring_resync(ua, &ua->dsp[i].cmd);
			ua_dsp_ring_resync(ua, &ua->dsp[i].resp);
		}
	} else if (ua->fw_v2) {
		u32 dma = ua_read(ua, UA_REG_DMA_CTRL);

		dev_dbg(&ua->pdev->dev,
			"DMA_CTRL before engine enable: 0x%08x\n", dma);
		/* macOS uses 0x1FFFE (16 engines); we had 0x1FE (9).
		 * More engines may be needed for type 0x01 capture channels. */
		ua_write(ua, UA_REG_DMA_CTRL, dma | 0x1FFFE);
		ua->dma_ctrl_cache = ua_read(ua, UA_REG_DMA_CTRL);
		dev_dbg(&ua->pdev->dev,
			"DMA_CTRL after engine enable: 0x%08x\n",
			ua->dma_ctrl_cache);

		/*
		 * CRITICAL: Pulse DMA reset with engines already enabled.
		 * Without this, the FPGA ring buffer engine stays dormant
		 * and ignores doorbells (POSITION stuck at 0).
		 * Discovered empirically 2026-03-01: reset-while-enabled
		 * kicks the ring buffer DMA engine into active state.
		 */
		ua_write(ua, UA_REG_DMA_CTRL,
			 ua->dma_ctrl_cache | UA_DMA_RESET_V2_MASK);
		ua_write(ua, UA_REG_DMA_CTRL, ua->dma_ctrl_cache);
		(void)ua_read(ua, UA_REG_DMA_CTRL);  /* flush */

		dev_dbg(&ua->pdev->dev,
			"DMA_CTRL after ring enable pulse: 0x%08x\n",
			ua_read(ua, UA_REG_DMA_CTRL));

		/*
		 * The kext reads POSITION after ResetDMAEngines (Program-
		 * Registers resets the engines first).  Here the pulse comes
		 * after the rings were programmed, so resync them again in
		 * case it moved POSITION; a no-op when it did not.
		 */
		for (i = 0; i < ua->num_dsps; i++) {
			if (!ua->dsp[i].rings_allocated)
				continue;
			ua_dsp_ring_resync(ua, &ua->dsp[i].cmd);
			ua_dsp_ring_resync(ua, &ua->dsp[i].resp);
		}
	}

	/*
	 * Dump unknown registers in the DMA control area.
	 * Some of these might control ring buffer DMA enables.
	 */
	{
		u32 r2210 = ua_read(ua, 0x2210);
		u32 r2214 = ua_read(ua, 0x2214);
		u32 r221c = ua_read(ua, 0x221C);
		u32 r2220 = ua_read(ua, 0x2220);
		u32 r2224 = ua_read(ua, 0x2224);
		u32 r2228 = ua_read(ua, 0x2228);
		u32 r222c = ua_read(ua, 0x222C);
		u32 r2230 = ua_read(ua, 0x2230);
		u32 r2234 = ua_read(ua, 0x2234);
		u32 r2238 = ua_read(ua, 0x2238);
		u32 r223c = ua_read(ua, 0x223C);

		dev_dbg(&ua->pdev->dev,
			"DMA area: 2210=%08x 2214=%08x "
			"221C=%08x 2220=%08x 2224=%08x\n",
			r2210, r2214, r221c, r2220, r2224);
		dev_dbg(&ua->pdev->dev,
			"DMA area: 2228=%08x 222C=%08x "
			"2230=%08x 2234=%08x 2238=%08x 223C=%08x\n",
			r2228, r222c, r2230, r2234, r2238, r223c);
	}

	/* dsp_msg_irq: DSP message/error vectors, after the rings exist */
	ua_dsp_irq_arm(ua);

	dev_info(&ua->pdev->dev, "DSP ring buffers initialized\n");
	return 0;
}

/**
 * ua_dsp_rings_fini - Free the ring pages and the parked buffers
 * @ua: device; remove has emptied the rings (ua_dsp_rings_quiesce()) and
 *      cleared bus mastering, or the device is gone
 * @dma_quiet: the device can no longer write anywhere (surprise removal,
 *             or DMA engines reset during the quiesce)
 *
 * The ring pages themselves are only ever read by the FPGA, and nothing
 * is queued on them any more.
 */
void ua_dsp_rings_fini(struct ua_device *ua, bool dma_quiet)
{
	unsigned int i;

	ua_dsp_irq_disarm(ua);

	for (i = 0; i < ua->num_dsps; i++) {
		struct ua_dsp_state *ds = &ua->dsp[i];

		if (!ds->rings_allocated)
			continue;

		ua_dsp_ring_free(ua, &ds->cmd);
		ua_dsp_ring_free(ua, &ds->resp);
		ds->rings_allocated = false;
	}

	ua_dsp_free_orphans(ua, dma_quiet);

	ua->fw_loaded = false;
	ua->dsps_connected = false;
}

/**
 * ua_dsp_rings_quiesce - Leave every DSP ring with nothing queued
 * @ua: device (ua->lock held or exclusive; device present)
 *
 * Part of the shutdown/suspend/remove quiesce.  Each ring gets a short
 * chance to drain, then is resynced (DOORBELL = WRITE_PTR = POSITION), so
 * when bus mastering is next enabled the FPGA has no target ahead of its
 * position, whatever the ring pages hold by then.  Entries still queued
 * are abandoned and the buffers they reference stay parked.
 */
void ua_dsp_rings_quiesce(struct ua_device *ua)
{
	unsigned int i;

	/* kext_connect: the disconnect is the date word alone (R83) */
	(void)ua_dsp_disconnect_notify(ua);
	/* dsp_msg_irq: stop counting and reading before the rings are emptied */
	ua_dsp_irq_disarm(ua);

	for (i = 0; i < ua->num_dsps; i++) {
		struct ua_dsp_state *ds = &ua->dsp[i];
		struct ua_dsp_ring *rings[] = { &ds->cmd, &ds->resp };
		unsigned int r;

		if (!ds->rings_allocated)
			continue;

		for (r = 0; r < ARRAY_SIZE(rings); r++)
			if (rings[r]->live)
				ua_dsp_ring_wait_idle(ua, rings[r], 5);
	}

	/* Free what the rings are provably done with, while POSITION can
	 * still be read */
	ua_dsp_reclaim_orphans(ua);

	for (i = 0; i < ua->num_dsps; i++) {
		struct ua_dsp_state *ds = &ua->dsp[i];

		if (!ds->rings_allocated)
			continue;

		ua_dsp_ring_resync(ua, &ds->cmd);
		ua_dsp_ring_resync(ua, &ds->resp);
		ds->cmd.live = false;
		ds->resp.live = false;
	}
}

/**
 * ua_dsp_rings_neutralize - Resync every DSP ring, by register only
 * @ua: device, bus mastering still off
 *
 * Probe and resume run this before pci_set_master().  Whatever the
 * previous owner (another OS, a crashed kernel, a removed instance of this
 * driver whose ring pages are freed) left queued, the FPGA's target is
 * pulled back to its position, so nothing in stale or freed ring pages is
 * fetched once bus mastering is on, before ua_dsp_rings_init() points the
 * rings at pages of ours.  Needs no ring memory.  On an idle ring (a live
 * mixer DSP left by macOS or by our own quiesce) DOORBELL = POSITION is
 * the value it already holds.
 */
void ua_dsp_rings_neutralize(struct ua_device *ua)
{
	unsigned int i;

	for (i = 0; i < ua->num_dsps; i++) {
		u32 base = ua_dsp_base(i);
		int cmd = ua_dsp_ring_regs_resync(ua, base + UA_RING_CMD_OFFSET);
		int resp = ua_dsp_ring_regs_resync(ua,
						   base + UA_RING_RESP_OFFSET);

		dev_dbg(&ua->pdev->dev,
			"DSP %u: rings neutralized at cmd=%d resp=%d\n",
			i, cmd, resp);
	}
}

/**
 * ua_dsp_cache_firmware - Keep the firmware files loadable across suspend
 * @ua: device
 *
 * A cold re-init after resume needs ua-apollo-mixer.bin (and the plugin
 * chain blob if it was not loaded yet).  firmware_request_cache() has the
 * firmware loader read them into its cache at suspend-prepare, while the
 * filesystem is still up, and serve request_firmware() from that cache
 * during resume.  The resume work also runs on the freezable workqueue,
 * after tasks are thawed.
 */
void ua_dsp_cache_firmware(struct ua_device *ua)
{
#if IS_ENABLED(CONFIG_FW_CACHE)
	int ret;

	ret = firmware_request_cache(&ua->pdev->dev, UA_MIXER_FW_NAME);
	if (!ret)
		ret = firmware_request_cache(&ua->pdev->dev,
					     UA_PLUGIN_CHAIN_FW_NAME);
	if (ret)
		dev_dbg(&ua->pdev->dev,
			"firmware cache request failed: %d\n", ret);
#endif
}

/**
 * ua_dsp_rings_reprogram - Re-write page addresses to FPGA after DMA reset
 * @ua: device
 *
 * The DMA reset pulse in ua_boot_transport_start() deactivates the FPGA's
 * ring buffer DMA engine even though the page address registers retain
 * their values.  Re-writing the same page addresses (via ring_program)
 * re-initializes the engine, allowing it to process DMA ref entries again.
 *
 * Must be called after ua_boot_transport_start() and before any
 * ua_dsp_send_block() calls.
 */
void ua_dsp_rings_reprogram(struct ua_device *ua)
{
	unsigned int i;

	for (i = 0; i < ua->num_dsps; i++) {
		struct ua_dsp_state *ds = &ua->dsp[i];
		u32 bank_base;

		if (!ds->rings_allocated)
			continue;

		/* Right after a DMA reset pulse: nothing left to drain */
		bank_base = ua_dsp_base(i);
		__ua_dsp_ring_program(ua, &ds->cmd,
				      bank_base + UA_RING_CMD_OFFSET, false);
		__ua_dsp_ring_program(ua, &ds->resp,
				      bank_base + UA_RING_RESP_OFFSET, false);
	}

	dev_dbg(&ua->pdev->dev,
		"DSP rings reprogrammed after transport start\n");
}

/* ----------------------------------------------------------------
 * _sendBlock result decoding (parity note 13, section 4)
 * ---------------------------------------------------------------- */

/*
 * The kext's _sendBlock error codes.  They have no readable names in the
 * kext, so these are named after the DSP status word that produces them.
 */
#define UA_KERR_NO_DSPS		(-21)
#define UA_KERR_STATUS_1	(-49)	/* 0xF0010001 */
#define UA_KERR_STATUS_2	(-48)	/* 0xF0010002 */
#define UA_KERR_BAD_RESPONSE	(-51)	/* 0xF0010003-5, other, hi mismatch */
#define UA_KERR_CLASS_F002	(-50)	/* 0xF002xxxx */
#define UA_KERR_STATUS_11	(-66)	/* 0xF0010011 */
#define UA_KERR_STATUS_6	(-68)	/* 0xF0010006 */
#define UA_KERR_STATUS_7	(-67)	/* 0xF0010007 */
#define UA_KERR_STATUS_7_ARG1	(-79)	/* 0xF0010007, response word 2 == 1 */
#define UA_KERR_STATUS_13	(-72)	/* 0xF0010013 */
#define UA_KERR_STATUS_12	(-73)	/* 0xF0010012 */
#define UA_KERR_TIMEOUT		(-96)
#define UA_KERR_NOT_READY	(-92)

static const char *ua_kext_err_name(int err)
{
	switch (err) {
	case 0:				return "ok";
	case UA_KERR_NO_DSPS:		return "no DSPs";
	case UA_KERR_STATUS_1:		return "DSP status 0xF0010001";
	case UA_KERR_STATUS_2:		return "DSP status 0xF0010002";
	case UA_KERR_BAD_RESPONSE:	return "bad response";
	case UA_KERR_CLASS_F002:	return "DSP status class 0xF002xxxx";
	case UA_KERR_STATUS_11:		return "DSP status 0xF0010011";
	case UA_KERR_STATUS_6:		return "DSP status 0xF0010006";
	case UA_KERR_STATUS_7:		return "DSP status 0xF0010007";
	case UA_KERR_STATUS_7_ARG1:	return "DSP status 0xF0010007 (arg 1)";
	case UA_KERR_STATUS_13:		return "DSP status 0xF0010013";
	case UA_KERR_STATUS_12:		return "DSP status 0xF0010012";
	case UA_KERR_TIMEOUT:		return "no response (timeout)";
	case UA_KERR_NOT_READY:		return "device not ready";
	default:			return "unknown";
	}
}

/* Response word 1 (DSP status) to the kext's result, section 4.2 */
static int ua_kext_block_status(u32 status, u32 arg)
{
	switch (status) {
	case 0:
		return 0;
	case 0xF0010001:
		return UA_KERR_STATUS_1;
	case 0xF0010002:
		return UA_KERR_STATUS_2;
	case 0xF0010006:
		return UA_KERR_STATUS_6;
	case 0xF0010007:
		return arg == 1 ? UA_KERR_STATUS_7_ARG1 : UA_KERR_STATUS_7;
	case 0xF0010011:
		return UA_KERR_STATUS_11;
	case 0xF0010012:
		return UA_KERR_STATUS_12;
	case 0xF0010013:
		return UA_KERR_STATUS_13;
	}
	if ((status & 0xFFFF0000) == 0xF0020000)
		return UA_KERR_CLASS_F002;
	return UA_KERR_BAD_RESPONSE;	/* 0xF0010003-5 and anything else */
}

/* Response area: 4 dwords (16 bytes), the kext's response descriptor.
 * The FPGA writes a 4-dword completion status to this buffer.
 */
#define SEND_BLOCK_RESP_DWORDS	4

/*
 * By default a block's outcome is only logged: any non-zero response word 0
 * counts as success, as before.  With block_strict=1 a kext error code makes
 * the send fail with -EIO.
 */
static bool block_strict;
module_param(block_strict, bool, 0444);
MODULE_PARM_DESC(block_strict,
		 "Fail a DSP block/firmware send (-EIO) when the response carries a kext error status (parity note 13 section 4.2). Default off: the error is only logged and any non-zero response counts as success.");

/*
 * Size of one data DMA ref in ua_dsp_send_block().  The kext sends up to a
 * 4KB physical page per descriptor.  An early Linux test stalled at 512+
 * dwords, so the default stays at the proven 1KB; 2048 and 4096 are accepted
 * for a hardware test.  Anything else falls back to 1024.
 */
static unsigned int sendblock_chunk = 1024;
module_param(sendblock_chunk, uint, 0444);
MODULE_PARM_DESC(sendblock_chunk,
		 "Bytes per data DMA ref in DSP block sends: 1024 (default, proven), 2048 or 4096 (the kext's 4KB pieces; untested on hardware)");

static size_t ua_sendblock_chunk(void)
{
	switch (sendblock_chunk) {
	case 2048:
	case 4096:
		return sendblock_chunk;
	default:
		return 1024;
	}
}

/* Count a block-send result (UA_KERR_*) and remember the last one */
static void ua_err_block_send(struct ua_device *ua, int kerr)
{
	atomic_set(&ua->err.last_kerr, kerr);
	ua_err_count(ua, UA_ERR_BLOCK_SEND);
}

/**
 * ua_dsp_block_result - Decode a 4-dword block response like _sendBlock
 * @ua: device
 * @dsp_idx: DSP the block went to
 * @cmd: command word sent (for the log)
 * @expect_hi: expected high half of response word 0 (0x8004 for firmware),
 *             or 0 for no check
 * @resp: the response area (little endian)
 *
 * Returns the kext's result (0 or a negative kext code) and logs it by name
 * when it is not 0.
 */
static int ua_dsp_block_result(struct ua_device *ua, unsigned int dsp_idx,
			       u32 cmd, u32 expect_hi, const __le32 *resp)
{
	u32 w0 = le32_to_cpu(READ_ONCE(resp[0]));
	u32 w1 = le32_to_cpu(READ_ONCE(resp[1]));
	u32 w2 = le32_to_cpu(READ_ONCE(resp[2]));
	int kerr;

	if (expect_hi && (w0 >> 16) != expect_hi)
		kerr = UA_KERR_BAD_RESPONSE;
	else
		kerr = ua_kext_block_status(w1, w2);

	if (kerr) {
		ua_err_block_send(ua, kerr);
		dev_warn(&ua->pdev->dev,
			 "sendBlock DSP%u cmd 0x%08x: kext result %d (%s), resp=[%08x %08x %08x %08x]\n",
			 dsp_idx, cmd, kerr, ua_kext_err_name(kerr),
			 w0, w1, w2, le32_to_cpu(READ_ONCE(resp[3])));
	}
	return kerr;
}

/* ----------------------------------------------------------------
 * Firmware loading
 * ---------------------------------------------------------------- */

/**
 * ua_dsp_load_firmware - Load a firmware image to DSP 0 via ring buffer DMA
 * @ua: device (must hold ua->lock)
 * @fw_data: kernel buffer containing the image
 * @fw_size: size of the image in bytes (multiple of 4, at most 4 MiB)
 *
 * Mirrors CUAD2Device::LoadFirmware / _sendBlock (kext 12.0.0 x86 0x134e0,
 * 0x12ed8; parity note 13 section 3.1).  DSP 0 only: _sendBlock takes no
 * DSP index.
 *   1. One contiguous DMA buffer: [header (1 or 2 dwords), image].  The
 *      header is {(dwords + 1) | 0x120000} below 0x3FFFC bytes, otherwise
 *      {0x120000 | BIT(30), dwords + 2}.
 *   2. Response entry (4 dwords) on the response ring first.
 *   3. DMA ref entries on the command ring, each below 0x10000 dwords
 *      (SetMemory asserts that), then the doorbells.
 *   4. Wait up to UA_FW_TIMEOUT_MS (150 s, the kext value) for response
 *      word 0 with high half 0x8004, then decode word 1 (the DSP status).
 *
 * Return: 0 on success, negative errno on failure.
 */
int ua_dsp_load_firmware(struct ua_device *ua, const void *fw_data,
			 size_t fw_size)
{
	struct ua_dsp_state *dsp0 = &ua->dsp[0];
	u32 bank_base = ua_dsp_base(0);
	void *cmd_buf = NULL, *resp_buf = NULL;
	dma_addr_t cmd_addr, resp_addr;
	struct ua_ring_entry entry;
	__le32 *cmd_words;
	u32 fw_dwords, hdr_dwords;
	u32 total_dwords, left, done;
	size_t cmd_buf_size;
	unsigned long deadline;
	unsigned int polls;
	bool timed_out = false;
	int kerr;
	int ret;

	if (!fw_size || fw_size > UA_FW_MAX_SIZE)
		return -EINVAL;
	if (fw_size & 3)
		return -EINVAL;

	if (!dsp0->rings_allocated) {
		dev_err(&ua->pdev->dev, "DSP 0 rings not allocated\n");
		return -ENODEV;
	}

	ret = ua_dsp_orphan_room(ua);
	if (ret)
		return ret;

	fw_dwords = fw_size / 4;
	/* The header form follows the payload size, as in _sendBlock */
	hdr_dwords = fw_size >= UA_FW_LARGE_THRESHOLD ? 2 : 1;

	/*
	 * One contiguous DMA buffer: [header..., image...].  The FPGA reads
	 * it through DMA ref entries of at most 0xFFFF dwords each.
	 */
	total_dwords = hdr_dwords + fw_dwords;
	cmd_buf_size = total_dwords * 4;

	dev_dbg(&ua->pdev->dev,
		"firmware load: allocating %zu byte contiguous DMA buffer\n",
		cmd_buf_size);

	cmd_buf = dma_alloc_coherent(&ua->pdev->dev, cmd_buf_size,
				     &cmd_addr, GFP_KERNEL);
	if (!cmd_buf) {
		dev_err(&ua->pdev->dev,
			"failed to allocate %zu byte DMA buffer for firmware\n",
			cmd_buf_size);
		return -ENOMEM;
	}

	resp_buf = dma_alloc_coherent(&ua->pdev->dev, UA_PCIE_PAGE_SIZE,
				      &resp_addr, GFP_KERNEL);
	if (!resp_buf) {
		ret = -ENOMEM;
		goto free_cmd;
	}

	/* Build the command buffer */
	cmd_words = cmd_buf;
	if (hdr_dwords == 2) {
		cmd_words[0] = cpu_to_le32(UA_FW_CMD | UA_FW_LARGE_FLAG);
		cmd_words[1] = cpu_to_le32(fw_dwords + 2);
	} else {
		cmd_words[0] = cpu_to_le32((fw_dwords + 1) | UA_FW_CMD);
	}
	memcpy(&cmd_words[hdr_dwords], fw_data, fw_size);

	/* Zero response buffer */
	memset(resp_buf, 0, UA_PCIE_PAGE_SIZE);

	/* Reset and re-program rings */
	ua_dsp_ring_program(ua, &dsp0->cmd,
			    bank_base + UA_RING_CMD_OFFSET);
	ua_dsp_ring_program(ua, &dsp0->resp,
			    bank_base + UA_RING_RESP_OFFSET);

	/* Submit response ring entry FIRST (provide buffer before sending cmd) */
	entry = (struct ua_ring_entry){
		.word0 = cpu_to_le32(UA_RING_DMA_REF | SEND_BLOCK_RESP_DWORDS),
		.word1 = 0,
		.word2 = cpu_to_le32(lower_32_bits(resp_addr)),
		.word3 = cpu_to_le32(upper_32_bits(resp_addr)),
	};
	ret = ua_dsp_ring_put_entry(ua, &dsp0->resp, &entry);
	if (ret)
		goto free_resp;
	ua_dsp_ring_doorbell(ua, &dsp0->resp,
			     bank_base + UA_RING_RESP_OFFSET);

	/* Small delay to let resp ring settle */
	usleep_range(1000, 2000);

	/*
	 * Command ring: DMA refs over the whole buffer, none above 0xFFFF
	 * dwords.  The ring was just reprogrammed, so it is empty and the
	 * few entries this needs (at most 17 for 4 MiB) always fit.
	 */
	for (done = 0, left = total_dwords; left; ) {
		u32 n = min_t(u32, left, 0xFFFF);
		dma_addr_t at = cmd_addr + (dma_addr_t)done * 4;

		entry = (struct ua_ring_entry){
			.word0 = cpu_to_le32(UA_RING_DMA_REF | n),
			.word1 = 0,
			.word2 = cpu_to_le32(lower_32_bits(at)),
			.word3 = cpu_to_le32(upper_32_bits(at)),
		};
		ret = ua_dsp_ring_put_entry(ua, &dsp0->cmd, &entry);
		if (ret) {
			/* The response descriptor is already queued: keep its page */
			ua_dsp_orphan_dma(ua, resp_buf, resp_addr,
					  UA_PCIE_PAGE_SIZE, &dsp0->resp, NULL);
			goto free_cmd;
		}
		done += n;
		left -= n;
	}

	dev_dbg(&ua->pdev->dev,
		"fw cmd buf: [%08x %08x %08x ...] total=%u dwords\n",
		le32_to_cpu(cmd_words[0]), le32_to_cpu(cmd_words[1]),
		le32_to_cpu(cmd_words[2]), total_dwords);

	ua_dsp_ring_doorbell(ua, &dsp0->cmd,
			     bank_base + UA_RING_CMD_OFFSET);

	dev_dbg(&ua->pdev->dev,
		"firmware load: %zu bytes, polling...\n", fw_size);

	/* Poll the response buffer (DMA coherent, no lock needed) */
	deadline = jiffies + msecs_to_jiffies(UA_FW_TIMEOUT_MS);
	for (polls = 0; ; polls++) {
		u32 resp_word0 = le32_to_cpu(READ_ONCE(((__le32 *)resp_buf)[0]));

		if (resp_word0)
			break;
		if (time_after(jiffies, deadline)) {
			timed_out = true;
			break;
		}
		/* Log progress every 5 seconds */
		if (polls > 0 && (polls % 500) == 0) {
			u32 cmd_pos = ua_read(ua, bank_base +
					      UA_RING_CMD_OFFSET +
					      UA_RING_POSITION);
			u32 resp_pos = ua_read(ua, bank_base +
					       UA_RING_RESP_OFFSET +
					       UA_RING_POSITION);
			dev_dbg(&ua->pdev->dev,
				"fw poll %ums: cmd_pos=%u resp_pos=%u resp[0]=0x%08x\n",
				polls * UA_FW_POLL_MS, cmd_pos, resp_pos,
				resp_word0);
		}
		msleep(UA_FW_POLL_MS);
	}

	if (timed_out) {
		u32 cmd_pos = ua_read(ua, bank_base + UA_RING_CMD_OFFSET +
				      UA_RING_POSITION);
		u32 resp_pos = ua_read(ua, bank_base + UA_RING_RESP_OFFSET +
				       UA_RING_POSITION);
		__le32 *resp_words = resp_buf;

		ua_err_block_send(ua, UA_KERR_TIMEOUT);
		dev_err(&ua->pdev->dev,
			"firmware load timeout after %u ms (kext result %d, %s)\n",
			UA_FW_TIMEOUT_MS, UA_KERR_TIMEOUT,
			ua_kext_err_name(UA_KERR_TIMEOUT));
		dev_err(&ua->pdev->dev,
			"  cmd: wrote=%u pos=%u  resp: wrote=%u pos=%u\n",
			dsp0->cmd.write_ptr, cmd_pos,
			dsp0->resp.write_ptr, resp_pos);
		dev_err(&ua->pdev->dev,
			"  resp_buf: [%08x %08x %08x %08x]\n",
			le32_to_cpu(resp_words[0]),
			le32_to_cpu(resp_words[1]),
			le32_to_cpu(resp_words[2]),
			le32_to_cpu(resp_words[3]));

		/* Both descriptors may still be used: park their buffers */
		ua_dsp_orphan_dma(ua, resp_buf, resp_addr, UA_PCIE_PAGE_SIZE,
				  &dsp0->resp, NULL);
		ua_dsp_orphan_dma(ua, cmd_buf, cmd_addr, cmd_buf_size,
				  &dsp0->cmd, NULL);
		return -ETIMEDOUT;
	}

	/* Answered: validate the high half and decode the DSP status */
	kerr = ua_dsp_block_result(ua, 0, UA_FW_CMD, UA_FW_RESP_MATCH,
				   resp_buf);
	if (kerr && ((le32_to_cpu(((__le32 *)resp_buf)[0]) >> 16) !=
		     UA_FW_RESP_MATCH || block_strict)) {
		ret = -EIO;
	} else {
		dev_info(&ua->pdev->dev,
			 "firmware loaded successfully (response=0x%08x)\n",
			 le32_to_cpu(((__le32 *)resp_buf)[0]));
		ua->fw_loaded = true;
		ua->dspx.replay_pending = true;
		ret = 0;
	}

free_resp:
	dma_free_coherent(&ua->pdev->dev, UA_PCIE_PAGE_SIZE,
			  resp_buf, resp_addr);
free_cmd:
	dma_free_coherent(&ua->pdev->dev, cmd_buf_size,
			  cmd_buf, cmd_addr);
	return ret;
}

/* ----------------------------------------------------------------
 * DMA reference entry format test
 * ---------------------------------------------------------------- */

/**
 * ua_dsp_test_dma_ref - Systematically test DMA ref entry formats
 * @ua: device (must hold ua->lock)
 *
 * Sends a known-good command ({0x00230002, 1} = DSP connect) via DMA ref
 * instead of inline, trying different entry layouts to find which one
 * the FPGA actually processes.
 *
 * First sends it inline as a baseline, then tries 6 DMA ref variations.
 * Logs results to dmesg for each format.
 */
#ifndef __clang__	/* clang has no "GCC optimize" (see UA_GCC_OPTIMIZE) */
#pragma GCC push_options
#pragma GCC optimize("O1")
#endif
int ua_dsp_test_dma_ref(struct ua_device *ua)
{
	struct ua_dsp_state *dsp0 = &ua->dsp[0];
	u32 bank_base = ua_dsp_base(0);
	u32 cmd_base = bank_base + UA_RING_CMD_OFFSET;
	void *dma_buf;
	dma_addr_t dma_addr;
	__le32 *words;
	u32 pos, cmd_gen, resp_gen;
	unsigned int base, done;
	struct ua_ring_entry entry;
	int test, ret;

	if (!dsp0->rings_allocated) {
		dev_err(&ua->pdev->dev, "DMA ref test: DSP 0 rings not allocated\n");
		return -ENODEV;
	}

	ret = ua_dsp_orphan_room(ua);
	if (ret)
		return ret;

	/*
	 * The ring is circular and ring_program() resyncs to POSITION
	 * instead of rewinding to 0, so every check below counts entries
	 * consumed since the test's starting index (base).
	 */

	/* Allocate a single DMA page for test data */
	dma_buf = dma_alloc_coherent(&ua->pdev->dev, UA_PCIE_PAGE_SIZE,
				     &dma_addr, GFP_KERNEL);
	if (!dma_buf)
		return -ENOMEM;

	dev_info(&ua->pdev->dev,
		 "=== DMA ref format test === dma_addr=%pad\n", &dma_addr);

	/*
	 * Test 0: BASELINE — inline entry (should work, proves ring is OK)
	 */
	ua_dsp_ring_program(ua, &dsp0->cmd, cmd_base);
	base = dsp0->cmd.write_ptr;

	/* dma_buf is referenced from here on; a resync that abandons
	 * entries after this point may abandon references to it */
	cmd_gen = dsp0->cmd.gen;
	resp_gen = dsp0->resp.gen;

	entry = (struct ua_ring_entry){
		.word0 = cpu_to_le32(UA_DSP_CMD_CONNECT),
		.word1 = cpu_to_le32(1),
		.word2 = 0,
		.word3 = 0,
	};
	ua_dsp_ring_put_entry(ua, &dsp0->cmd, &entry);
	ua_dsp_ring_doorbell(ua, &dsp0->cmd, cmd_base);
	usleep_range(5000, 10000);
	pos = ua_read(ua, cmd_base + UA_RING_POSITION);
	done = ua_dsp_ring_progress(&dsp0->cmd, base, pos);
	dev_info(&ua->pdev->dev,
		 "TEST 0 (inline baseline): pos=%u done=%u %s\n",
		 pos, done, done >= 1 ? "OK" : "FAIL");

	/*
	 * Write the connect command into the DMA buffer.
	 * {0x00230002, 0x00000001} = 2 dwords = 8 bytes
	 */
	memset(dma_buf, 0, UA_PCIE_PAGE_SIZE);
	words = dma_buf;
	words[0] = cpu_to_le32(UA_DSP_CMD_CONNECT);  /* 0x00230002 */
	words[1] = cpu_to_le32(1);                     /* param = 1 */

	/*
	 * Now try 6 DMA ref entry formats.
	 * Each test resets the ring, submits a DMA ref entry PLUS a
	 * follow-up inline entry (the finalize command as a canary).
	 * If pos=2, the FPGA processed the DMA ref and moved on.
	 * If pos=1, it got stuck on the DMA ref.
	 */
	for (test = 1; test <= 6; test++) {
		struct ua_ring_entry canary = {
			.word0 = cpu_to_le32(UA_DSP_CMD_FINALIZE),
			.word1 = 0,
			.word2 = 0,
			.word3 = 0,
		};

		ua_dsp_ring_program(ua, &dsp0->cmd, cmd_base);
		base = dsp0->cmd.write_ptr;

		switch (test) {
		case 1:
			/* {ndwords|BIT31, 0, addr_lo, addr_hi} — correct format */
			entry = (struct ua_ring_entry){
				.word0 = cpu_to_le32(UA_RING_DMA_REF | 2),
				.word1 = 0,
				.word2 = cpu_to_le32(lower_32_bits(dma_addr)),
				.word3 = cpu_to_le32(upper_32_bits(dma_addr)),
			};
			break;
		case 2:
			/* {nbytes|BIT31, 0, addr_lo, addr_hi} — size in bytes */
			entry = (struct ua_ring_entry){
				.word0 = cpu_to_le32(UA_RING_DMA_REF | 8),
				.word1 = 0,
				.word2 = cpu_to_le32(lower_32_bits(dma_addr)),
				.word3 = cpu_to_le32(upper_32_bits(dma_addr)),
			};
			break;
		case 3:
			/* {ndwords|BIT31, addr_lo, addr_hi, 0} — old wrong format */
			entry = (struct ua_ring_entry){
				.word0 = cpu_to_le32(UA_RING_DMA_REF | 2),
				.word1 = cpu_to_le32(lower_32_bits(dma_addr)),
				.word2 = cpu_to_le32(upper_32_bits(dma_addr)),
				.word3 = 0,
			};
			break;
		case 4:
			/* {nbytes|BIT31, addr_lo, addr_hi, 0} — bytes + old format */
			entry = (struct ua_ring_entry){
				.word0 = cpu_to_le32(UA_RING_DMA_REF | 8),
				.word1 = cpu_to_le32(lower_32_bits(dma_addr)),
				.word2 = cpu_to_le32(upper_32_bits(dma_addr)),
				.word3 = 0,
			};
			break;
		case 5:
			/* {addr_lo|BIT31, ndwords, 0, 0} — addr in w0, size in w1 */
			entry = (struct ua_ring_entry){
				.word0 = cpu_to_le32(lower_32_bits(dma_addr) | UA_RING_DMA_REF),
				.word1 = cpu_to_le32(2),
				.word2 = cpu_to_le32(upper_32_bits(dma_addr)),
				.word3 = 0,
			};
			break;
		case 6:
			/* Full page size: {page_dwords|BIT31, 0, addr_lo, addr_hi} */
			entry = (struct ua_ring_entry){
				.word0 = cpu_to_le32(UA_RING_DMA_REF | (UA_PCIE_PAGE_SIZE / 4)),
				.word1 = 0,
				.word2 = cpu_to_le32(lower_32_bits(dma_addr)),
				.word3 = cpu_to_le32(upper_32_bits(dma_addr)),
			};
			break;
		}

		ret = ua_dsp_ring_put_entry(ua, &dsp0->cmd, &entry);
		if (ret) {
			dev_err(&ua->pdev->dev,
				"TEST %d: put_entry failed (%d)\n", test, ret);
			continue;
		}

		/* Canary inline entry — should be consumed if DMA ref didn't stall */
		ua_dsp_ring_put_entry(ua, &dsp0->cmd, &canary);
		ua_dsp_ring_doorbell(ua, &dsp0->cmd, cmd_base);
		usleep_range(50000, 100000);  /* 50-100ms wait */

		pos = ua_read(ua, cmd_base + UA_RING_POSITION);
		done = ua_dsp_ring_progress(&dsp0->cmd, base, pos);
		dev_info(&ua->pdev->dev,
			 "TEST %d: entry=[%08x %08x %08x %08x] pos=%u done=%u/%u %s\n",
			 test,
			 le32_to_cpu(entry.word0), le32_to_cpu(entry.word1),
			 le32_to_cpu(entry.word2), le32_to_cpu(entry.word3),
			 pos, done, 2,
			 done >= 2 ? "DMA_OK" : (done == 1 ? "DMA_STALL" : "STUCK"));
	}

	/*
	 * Test 7: Garbage DMA ref — verify FPGA reads DMA content.
	 * If pos=2, FPGA ignores content (just skips DMA refs).
	 * If pos=1, FPGA reads content and stalls on garbage.
	 */
	memset(dma_buf, 0xFF, UA_PCIE_PAGE_SIZE);
	ua_dsp_ring_program(ua, &dsp0->cmd, cmd_base);
	base = dsp0->cmd.write_ptr;
	entry = (struct ua_ring_entry){
		.word0 = cpu_to_le32(UA_RING_DMA_REF | 2),
		.word1 = 0,
		.word2 = cpu_to_le32(lower_32_bits(dma_addr)),
		.word3 = cpu_to_le32(upper_32_bits(dma_addr)),
	};
	ua_dsp_ring_put_entry(ua, &dsp0->cmd, &entry);
	{
		struct ua_ring_entry canary = {
			.word0 = cpu_to_le32(UA_DSP_CMD_FINALIZE),
			.word1 = 0, .word2 = 0, .word3 = 0,
		};
		ua_dsp_ring_put_entry(ua, &dsp0->cmd, &canary);
	}
	ua_dsp_ring_doorbell(ua, &dsp0->cmd, cmd_base);
	usleep_range(50000, 100000);
	pos = ua_read(ua, cmd_base + UA_RING_POSITION);
	done = ua_dsp_ring_progress(&dsp0->cmd, base, pos);
	dev_info(&ua->pdev->dev,
		 "TEST 7 (garbage DMA ref): pos=%u done=%u/%u %s\n",
		 pos, done, 2,
		 done >= 2 ? "SKIP(no read)" : (done == 1 ? "STALL(reads!)" : "STUCK"));

	/* Restore valid data */
	memset(dma_buf, 0, UA_PCIE_PAGE_SIZE);
	words[0] = cpu_to_le32(UA_DSP_CMD_CONNECT);
	words[1] = cpu_to_le32(1);

	/*
	 * Test 8: Multiple DMA refs in sequence (no inline).
	 * This replicates the firmware load pattern.
	 */
	ua_dsp_ring_program(ua, &dsp0->cmd, cmd_base);
	base = dsp0->cmd.write_ptr;
	for (test = 0; test < 5; test++) {
		entry = (struct ua_ring_entry){
			.word0 = cpu_to_le32(UA_RING_DMA_REF | 2),
			.word1 = 0,
			.word2 = cpu_to_le32(lower_32_bits(dma_addr)),
			.word3 = cpu_to_le32(upper_32_bits(dma_addr)),
		};
		ua_dsp_ring_put_entry(ua, &dsp0->cmd, &entry);
	}
	ua_dsp_ring_doorbell(ua, &dsp0->cmd, cmd_base);
	usleep_range(100000, 200000);  /* 100-200ms */
	pos = ua_read(ua, cmd_base + UA_RING_POSITION);
	done = ua_dsp_ring_progress(&dsp0->cmd, base, pos);
	dev_info(&ua->pdev->dev,
		 "TEST 8 (5 DMA refs, no inline): pos=%u done=%u/%u %s\n",
		 pos, done, 5,
		 done >= 5 ? "ALL_OK" : "PARTIAL");

	/*
	 * Test 9: DMA ref + resp ring (like firmware load).
	 * Submit 1 DMA ref to cmd ring + 1 DMA ref to resp ring,
	 * ring both doorbells.
	 */
	ua_dsp_ring_program(ua, &dsp0->cmd, cmd_base);
	ua_dsp_ring_program(ua, &dsp0->resp,
			    bank_base + UA_RING_RESP_OFFSET);

	entry = (struct ua_ring_entry){
		.word0 = cpu_to_le32(UA_RING_DMA_REF | 2),
		.word1 = 0,
		.word2 = cpu_to_le32(lower_32_bits(dma_addr)),
		.word3 = cpu_to_le32(upper_32_bits(dma_addr)),
	};
	ua_dsp_ring_put_entry(ua, &dsp0->cmd, &entry);

	/* Also submit a response ring DMA ref (using same buffer as scratch) */
	{
		struct ua_ring_entry resp_entry = {
			.word0 = cpu_to_le32(UA_RING_DMA_REF | 4),
			.word1 = 0,
			.word2 = cpu_to_le32(lower_32_bits(dma_addr)),
			.word3 = cpu_to_le32(upper_32_bits(dma_addr)),
		};
		ua_dsp_ring_put_entry(ua, &dsp0->resp, &resp_entry);
	}

	ua_dsp_ring_doorbell(ua, &dsp0->cmd, cmd_base);
	ua_dsp_ring_doorbell(ua, &dsp0->resp,
			     bank_base + UA_RING_RESP_OFFSET);
	usleep_range(50000, 100000);
	{
		u32 cmd_pos = ua_read(ua, cmd_base + UA_RING_POSITION);
		u32 resp_pos = ua_read(ua, bank_base +
				       UA_RING_RESP_OFFSET + UA_RING_POSITION);
		dev_info(&ua->pdev->dev,
			 "TEST 9 (cmd+resp DMA refs): cmd_pos=%u resp_pos=%u\n",
			 cmd_pos, resp_pos);
	}

	/*
	 * Test 10: Inline recovery after all tests.
	 */
	ua_dsp_ring_program(ua, &dsp0->cmd, cmd_base);
	base = dsp0->cmd.write_ptr;
	entry = (struct ua_ring_entry){
		.word0 = cpu_to_le32(UA_DSP_CMD_CONNECT),
		.word1 = cpu_to_le32(1),
		.word2 = 0,
		.word3 = 0,
	};
	ua_dsp_ring_put_entry(ua, &dsp0->cmd, &entry);
	ua_dsp_ring_doorbell(ua, &dsp0->cmd, cmd_base);
	usleep_range(5000, 10000);
	pos = ua_read(ua, cmd_base + UA_RING_POSITION);
	done = ua_dsp_ring_progress(&dsp0->cmd, base, pos);
	dev_info(&ua->pdev->dev,
		 "TEST 10 (inline recovery): pos=%u done=%u %s\n",
		 pos, done, done >= 1 ? "OK" : "FAIL");

	dev_info(&ua->pdev->dev, "=== DMA ref test complete ===\n");

	/*
	 * Test 9 leaves a response descriptor on dma_buf that may never be
	 * filled, and a stalled DMA ref may have been abandoned by a later
	 * reprogram: park the page; it is freed right away if both rings
	 * are provably done with it.
	 */
	ua_dsp_orphan_dma_gen(ua, dma_buf, dma_addr, UA_PCIE_PAGE_SIZE,
			      &dsp0->cmd, cmd_gen, &dsp0->resp, resp_gen);
	return 0;
}
#ifndef __clang__
#pragma GCC pop_options
#endif

/* ----------------------------------------------------------------
 * _sendBlock — send a data block to a DSP via ring buffer
 * ----------------------------------------------------------------
 *
 * Kext analysis (CUAD2Device::_sendBlock, 0x1f7f8):
 *
 * _sendBlock(this, cmd, param, DMABuffer, data_offset, data_size, response_code)
 *
 * Protocol:
 *   1. Resp ring: submit 4-dword DMA ref → response buffer, doorbell
 *   2. Cmd ring:  submit header DMA ref (1 dword: (data_dwords+1)|cmd)
 *   3. Cmd ring:  submit per-page DMA refs (4KB each, one per page)
 *   4. ONE doorbell to cmd ring
 *   5. Poll response buffer for completion (resp[0] upper 16 bits == param>>16)
 *
 * Key details:
 *   - Response ring entry is 4 dwords (16 bytes), NOT a full page
 *   - param is for response validation only, NOT sent to hardware
 *   - All cmd ring entries submitted as one batch, single doorbell
 *   - Data split into pieces that never cross a 4KB bus boundary: the
 *     kext uses up to 4KB (1024 dwords) per descriptor; Linux defaults to
 *     1KB pieces, see sendblock_chunk
 *   - Header form follows the payload size: below 0x3FFFC bytes one dword
 *     {(dwords + 1) | cmd}, from 0x3FFFC bytes the large form
 *     {cmd | BIT(30), dwords + 2}
 *   - Timeout is UA_BLOCK_TIMEOUT_MS (the kext's 5 s for blocks; 150 s for
 *     firmware only in ua_dsp_load_firmware()); the wait ends at once on
 *     the response
 *   - Doorbell order: DOORBELL(0x24) first, then WRITE_PTR(0x20)
 */

int ua_dsp_send_block(struct ua_device *ua, unsigned int dsp_idx,
		      u32 cmd, u32 param, const void *data,
		      size_t data_size)
{
	struct ua_dsp_state *ds;
	u32 bank_base, cmd_base, resp_base;
	void *hdr_buf = NULL, *data_buf = NULL, *resp_buf = NULL;
	dma_addr_t hdr_dma, data_dma, resp_dma;
	struct ua_ring_entry entry;
	u32 data_dwords, num_pages;
	__le32 *resp_words;
	size_t data_alloc, offset, chunk;
	unsigned int cmd_start;
	bool resp_queued = false;
	unsigned long deadline;
	int polls, ret = 0;

	if (!ua->num_dsps) {
		ua_err_block_send(ua, UA_KERR_NO_DSPS);
		dev_warn(&ua->pdev->dev,
			 "sendBlock: kext result %d (%s)\n",
			 UA_KERR_NO_DSPS, ua_kext_err_name(UA_KERR_NO_DSPS));
		return -ENODEV;
	}
	if (dsp_idx >= ua->num_dsps)
		return -EINVAL;
	if (data_size & 3)
		return -EINVAL;
	/*
	 * The large header form (>= 0x3FFFC bytes) is reachable now; the
	 * cap is UA_FW_MAX_SIZE and the ring-room check below rejects
	 * transfers that need more than one ring's worth of entries.
	 */
	if (data_size > UA_FW_MAX_SIZE)
		return -E2BIG;

	ds = &ua->dsp[dsp_idx];
	if (!ds->rings_allocated)
		return -ENODEV;

	bank_base = ua_dsp_base(dsp_idx);
	cmd_base = bank_base + UA_RING_CMD_OFFSET;
	resp_base = bank_base + UA_RING_RESP_OFFSET;

	data_dwords = data_size / 4;
	num_pages = DIV_ROUND_UP(data_size, ua_sendblock_chunk());

	ret = ua_dsp_orphan_room(ua);
	if (ret)
		return ret;

	/* Reserve ring room: 1 header + N data chunks (circular ring) */
	ret = ua_dsp_ring_wait_space(ua, &ds->cmd, 1 + num_pages, 1000);
	if (ret)
		return ret;

	/*
	 * The DSP fills response descriptors in ring order, so one left
	 * queued by an earlier command that never got its answer (a
	 * timeout, or the unwind below) would take this command's response,
	 * and every later sendBlock on this DSP would time out.  Resync the
	 * response ring first; the abandoned descriptor's buffer stays
	 * parked (orphans) until the ring is provably past it.
	 */
	if (ds->resp.live) {
		ret = ua_dsp_ring_update_pos(ua, &ds->resp);
		if (ret)
			return ret;
		if (ua_dsp_ring_pending(&ds->resp))
			ua_dsp_ring_program(ua, &ds->resp, resp_base);
	}

	/* Allocate header DMA buffer (holds 1 command dword) */
	hdr_buf = dma_alloc_coherent(&ua->pdev->dev, UA_PCIE_PAGE_SIZE,
				     &hdr_dma, GFP_KERNEL);
	if (!hdr_buf)
		return -ENOMEM;

	/* Allocate data DMA buffer (page-aligned) */
	data_alloc = ALIGN(data_size, UA_PCIE_PAGE_SIZE);
	if (data_alloc < UA_PCIE_PAGE_SIZE)
		data_alloc = UA_PCIE_PAGE_SIZE;
	data_buf = dma_alloc_coherent(&ua->pdev->dev, data_alloc,
				      &data_dma, GFP_KERNEL);
	if (!data_buf) {
		ret = -ENOMEM;
		goto free_hdr;
	}
	memset(data_buf, 0, data_alloc);
	if (data_size > 0)
		memcpy(data_buf, data, data_size);

	/* Allocate response DMA buffer (16 bytes = 4 dwords) */
	resp_buf = dma_alloc_coherent(&ua->pdev->dev, UA_PCIE_PAGE_SIZE,
				      &resp_dma, GFP_KERNEL);
	if (!resp_buf) {
		ret = -ENOMEM;
		goto free_data;
	}
	memset(resp_buf, 0, UA_PCIE_PAGE_SIZE);
	resp_words = resp_buf;

	/*
	 * Step 1: Submit response ring entry.
	 *
	 * Kext descriptor2: 4-dword DMA ref on the response ring.
	 * The FPGA reads this entry to find where to write its response.
	 * Must be submitted BEFORE cmd ring entries.
	 */
	entry = (struct ua_ring_entry){
		.word0 = cpu_to_le32(UA_RING_DMA_REF |
				     SEND_BLOCK_RESP_DWORDS),
		.word1 = 0,
		.word2 = cpu_to_le32(lower_32_bits(resp_dma)),
		.word3 = cpu_to_le32(upper_32_bits(resp_dma)),
	};
	if (!ua_dsp_ring_put_entry(ua, &ds->resp, &entry)) {
		ua_dsp_ring_doorbell(ua, &ds->resp, resp_base);
		resp_queued = true;
		usleep_range(500, 1000);
	} else {
		dev_warn(&ua->pdev->dev,
			 "sendBlock DSP%u: response ring full, no response slot\n",
			 dsp_idx);
	}

	/* Start of this command's cmd-ring entries, for unwinding */
	cmd_start = ds->cmd.write_ptr;

	/*
	 * Step 2: Build command header.
	 *
	 * Small format (data < 0x3FFFC bytes):
	 *   header[0] = (data_dwords + 1) | cmd
	 *
	 * Large format (data >= 0x3FFFC bytes):
	 *   header[0] = cmd | BIT(30)
	 *   header[1] = data_dwords + 2
	 */
	memset(hdr_buf, 0, UA_PCIE_PAGE_SIZE);
	if (data_size < UA_FW_LARGE_THRESHOLD) {
		((__le32 *)hdr_buf)[0] = cpu_to_le32((data_dwords + 1) | cmd);

		/* Header DMA ref: 1 dword */
		entry = (struct ua_ring_entry){
			.word0 = cpu_to_le32(UA_RING_DMA_REF | 1),
			.word1 = 0,
			.word2 = cpu_to_le32(lower_32_bits(hdr_dma)),
			.word3 = cpu_to_le32(upper_32_bits(hdr_dma)),
		};
	} else {
		((__le32 *)hdr_buf)[0] = cpu_to_le32(cmd | BIT(30));
		((__le32 *)hdr_buf)[1] = cpu_to_le32(data_dwords + 2);

		/* Header DMA ref: 2 dwords */
		entry = (struct ua_ring_entry){
			.word0 = cpu_to_le32(UA_RING_DMA_REF | 2),
			.word1 = 0,
			.word2 = cpu_to_le32(lower_32_bits(hdr_dma)),
			.word3 = cpu_to_le32(upper_32_bits(hdr_dma)),
		};
	}
	ret = ua_dsp_ring_put_entry(ua, &ds->cmd, &entry);
	if (ret)
		goto unwind;

	/*
	 * Step 3: Submit data as DMA ref entries.
	 *
	 * The kext walks the payload in physical-page pieces of at most 4KB
	 * that never cross a 4KB bus boundary.  data_buf is page aligned and
	 * the piece size divides 4KB, so the same holds here.  See
	 * sendblock_chunk for why the default piece is 1KB.
	 */
	for (offset = 0; offset < data_size; offset += chunk) {
		u32 chunk_dwords;

		chunk = min_t(size_t, data_size - offset, ua_sendblock_chunk());
		chunk_dwords = chunk / 4;

		entry = (struct ua_ring_entry){
			.word0 = cpu_to_le32(UA_RING_DMA_REF | chunk_dwords),
			.word1 = 0,
			.word2 = cpu_to_le32(lower_32_bits(data_dma + offset)),
			.word3 = cpu_to_le32(upper_32_bits(data_dma + offset)),
		};
		ret = ua_dsp_ring_put_entry(ua, &ds->cmd, &entry);
		if (ret)
			goto unwind;
	}

	/*
	 * Step 4: ONE doorbell to cmd ring (all entries submitted as batch).
	 */
	ua_dsp_ring_doorbell(ua, &ds->cmd, cmd_base);

	/*
	 * Step 5: Wait for the response (the kext's _sendBlock wait).
	 *
	 * The kext checks the response word 0 high half against the
	 * expected one (param >> 16 when param carries BIT(31)) and maps
	 * word 1 to a result code (parity note 13, section 4).  Linux has
	 * always accepted any non-zero word 0; that stays, the result is
	 * decoded and logged by name (block_strict=1 makes it fatal).
	 * Timeout: UA_BLOCK_TIMEOUT_MS, ended at once by the response.
	 */
	deadline = jiffies + msecs_to_jiffies(UA_BLOCK_TIMEOUT_MS);
	for (polls = 0; !READ_ONCE(resp_words[0]); polls++) {
		if (time_after(jiffies, deadline))
			break;
		/* A block completes in well under 20 ms: poll tightly first */
		if (polls < 200)
			usleep_range(100, 200);
		else
			usleep_range(1000, 2000);
	}

	dev_dbg(&ua->pdev->dev,
		"sendBlock DSP%u: cmd=0x%08x param=0x%08x size=%zu "
		"resp=[0x%08x 0x%08x 0x%08x 0x%08x] pos=%u/%u\n",
		dsp_idx, cmd, param, data_size,
		le32_to_cpu(resp_words[0]), le32_to_cpu(resp_words[1]),
		le32_to_cpu(resp_words[2]), le32_to_cpu(resp_words[3]),
		ua_read(ua, cmd_base + UA_RING_POSITION),
		ds->cmd.write_ptr);

	if (READ_ONCE(resp_words[0]) == 0) {
		ua_err_block_send(ua, UA_KERR_TIMEOUT);
		dev_warn(&ua->pdev->dev,
			 "sendBlock: no response after %d polls (kext result %d, %s)\n",
			 polls, UA_KERR_TIMEOUT,
			 ua_kext_err_name(UA_KERR_TIMEOUT));
		ret = -ETIMEDOUT;

		/*
		 * The response descriptor stays queued until the DSP
		 * answers, and the payload is referenced until POSITION
		 * passes it: park them (freed at once if already done).
		 */
		if (resp_queued) {
			ua_dsp_orphan_dma(ua, resp_buf, resp_dma,
					  UA_PCIE_PAGE_SIZE, &ds->resp, NULL);
			resp_buf = NULL;
		}
		ua_dsp_orphan_dma(ua, data_buf, data_dma, data_alloc,
				  &ds->cmd, NULL);
		data_buf = NULL;
		ua_dsp_orphan_dma(ua, hdr_buf, hdr_dma, UA_PCIE_PAGE_SIZE,
				  &ds->cmd, NULL);
		hdr_buf = NULL;
	} else if (ua_dsp_block_result(ua, dsp_idx, cmd,
				       (param & BIT(31)) ? param >> 16 : 0,
				       resp_words) && block_strict) {
		ret = -EIO;
	}
	goto free_resp;

unwind:
	/* Nothing of this command was doorbelled on the cmd ring */
	ds->cmd.write_ptr = cmd_start;
	if (resp_queued) {
		ua_dsp_orphan_dma(ua, resp_buf, resp_dma, UA_PCIE_PAGE_SIZE,
				  &ds->resp, NULL);
		resp_buf = NULL;
	}
free_resp:
	if (resp_buf)
		dma_free_coherent(&ua->pdev->dev, UA_PCIE_PAGE_SIZE,
				  resp_buf, resp_dma);
free_data:
	if (data_buf)
		dma_free_coherent(&ua->pdev->dev, data_alloc,
				  data_buf, data_dma);
free_hdr:
	if (hdr_buf)
		dma_free_coherent(&ua->pdev->dev, UA_PCIE_PAGE_SIZE,
				  hdr_buf, hdr_dma);
	return ret;
}

/**
 * ua_dsp_send_sram_data - Send SRAM config + data as atomic pair
 * @ua: device (caller must hold ua->lock)
 * @dsp_idx: target DSP (0-3)
 * @sram_addr: SHARC DSP SRAM destination address
 * @data: data to write
 * @data_size: size in bytes (must be 4-byte aligned, max 4096)
 *
 * Sends an inline SRAM_CFG command (0x00080004) followed by a DMA ref
 * containing the data, as a single atomic pair (one doorbell for both).
 * This matches Windows driver behavior for SRAM routing table writes.
 */
int ua_dsp_send_sram_data(struct ua_device *ua, unsigned int dsp_idx,
			  u32 sram_addr, const void *data, size_t data_size)
{
	struct ua_dsp_state *ds;
	u32 bank_base, cmd_base;
	void *dma_buf;
	dma_addr_t dma_addr;
	struct ua_ring_entry entry;
	unsigned int cmd_start;
	u32 dwords;
	int ret;

	if (dsp_idx >= ua->num_dsps || !data || (data_size & 3) ||
	    data_size > UA_PCIE_PAGE_SIZE)
		return -EINVAL;

	ds = &ua->dsp[dsp_idx];
	if (!ds->rings_allocated)
		return -ENODEV;

	bank_base = ua_dsp_base(dsp_idx);
	cmd_base = bank_base + UA_RING_CMD_OFFSET;
	dwords = data_size / 4;

	ret = ua_dsp_orphan_room(ua);
	if (ret)
		return ret;

	/* Need 2 ring entries: inline + DMA ref */
	ret = ua_dsp_ring_wait_space(ua, &ds->cmd, 2, 1000);
	if (ret)
		return ret;

	dma_buf = dma_alloc_coherent(&ua->pdev->dev, UA_PCIE_PAGE_SIZE,
				     &dma_addr, GFP_KERNEL);
	if (!dma_buf)
		return -ENOMEM;

	memset(dma_buf, 0, UA_PCIE_PAGE_SIZE);
	memcpy(dma_buf, data, data_size);

	/* Entry 1: inline SRAM_CFG {0x00080004, sram_addr, 0, dwords} */
	cmd_start = ds->cmd.write_ptr;
	entry = (struct ua_ring_entry){
		.word0 = cpu_to_le32(0x00080004),
		.word1 = cpu_to_le32(sram_addr),
		.word2 = 0,
		.word3 = cpu_to_le32(dwords),
	};
	ret = ua_dsp_ring_put_entry(ua, &ds->cmd, &entry);

	/* Entry 2: DMA ref pointing to data — NO doorbell yet */
	entry = (struct ua_ring_entry){
		.word0 = cpu_to_le32(UA_RING_DMA_REF | dwords),
		.word1 = 0,
		.word2 = cpu_to_le32(lower_32_bits(dma_addr)),
		.word3 = cpu_to_le32(upper_32_bits(dma_addr)),
	};
	if (!ret)
		ret = ua_dsp_ring_put_entry(ua, &ds->cmd, &entry);
	if (ret) {
		/* Not doorbelled: drop the partial pair */
		ds->cmd.write_ptr = cmd_start;
		dma_free_coherent(&ua->pdev->dev, UA_PCIE_PAGE_SIZE,
				  dma_buf, dma_addr);
		return ret;
	}

	/* Single doorbell for both entries */
	ua_dsp_ring_doorbell(ua, &ds->cmd, cmd_base);

	/*
	 * Wait for DSP to consume both entries.  Not consumed within the
	 * kext's 6 s: -ETIMEDOUT, as its command-buffer send fails (-38);
	 * this used to report success.
	 */
	if (!ua_dsp_ring_wait_idle(ua, &ds->cmd, UA_DSP_CMDBUF_WAIT_MS)) {
		dev_warn(&ua->pdev->dev,
			 "sram_data: DSP %u timeout (pos=%u wr=%u)\n",
			 dsp_idx, ds->cmd.hw_pos, ds->cmd.write_ptr);
		/* The DMA ref may still be fetched: park its buffer */
		ua_dsp_orphan_dma(ua, dma_buf, dma_addr, UA_PCIE_PAGE_SIZE,
				  &ds->cmd, NULL);
		return -ETIMEDOUT;
	}

	dma_free_coherent(&ua->pdev->dev, UA_PCIE_PAGE_SIZE,
			  dma_buf, dma_addr);
	return 0;
}

/**
 * ua_dsp_send_raw_dma - Send raw data to DSP via DMA ref (no cmd/param header)
 * @ua: device (caller must hold ua->lock)
 * @dsp_idx: target DSP (0-3)
 * @data: raw data to send
 * @data_size: size in bytes (must be 4-byte aligned)
 *
 * Sends data as a plain DMA reference entry on the command ring.
 * No response is expected — this is for SRAM config data that follows
 * a 0x00080004 inline command.
 */
int ua_dsp_send_raw_dma(struct ua_device *ua, unsigned int dsp_idx,
			const void *data, size_t data_size)
{
	struct ua_dsp_state *ds;
	u32 bank_base, cmd_base;
	void *dma_buf;
	dma_addr_t dma_addr;
	struct ua_ring_entry entry;
	u32 dwords;
	int ret;

	if (dsp_idx >= ua->num_dsps || !data || (data_size & 3) ||
	    data_size > UA_PCIE_PAGE_SIZE)
		return -EINVAL;

	ds = &ua->dsp[dsp_idx];
	if (!ds->rings_allocated)
		return -ENODEV;

	bank_base = ua_dsp_base(dsp_idx);
	cmd_base = bank_base + UA_RING_CMD_OFFSET;
	dwords = data_size / 4;

	ret = ua_dsp_orphan_room(ua);
	if (ret)
		return ret;

	ret = ua_dsp_ring_wait_space(ua, &ds->cmd, 1, 1000);
	if (ret)
		return ret;

	dma_buf = dma_alloc_coherent(&ua->pdev->dev, UA_PCIE_PAGE_SIZE,
				     &dma_addr, GFP_KERNEL);
	if (!dma_buf)
		return -ENOMEM;

	memset(dma_buf, 0, UA_PCIE_PAGE_SIZE);
	memcpy(dma_buf, data, data_size);

	/* Send as raw DMA reference — no header, no response */
	entry = (struct ua_ring_entry){
		.word0 = cpu_to_le32(UA_RING_DMA_REF | dwords),
		.word1 = 0,
		.word2 = cpu_to_le32(lower_32_bits(dma_addr)),
		.word3 = cpu_to_le32(upper_32_bits(dma_addr)),
	};

	ret = ua_dsp_ring_put_entry(ua, &ds->cmd, &entry);
	if (ret) {
		dma_free_coherent(&ua->pdev->dev, UA_PCIE_PAGE_SIZE,
				  dma_buf, dma_addr);
		return ret;
	}
	ua_dsp_ring_doorbell(ua, &ds->cmd, cmd_base);

	/*
	 * Wait for DSP to consume the entry before freeing DMA buffer;
	 * -ETIMEDOUT when it does not (see ua_dsp_send_sram_data()).
	 */
	if (!ua_dsp_ring_wait_idle(ua, &ds->cmd, UA_DSP_CMDBUF_WAIT_MS)) {
		dev_warn(&ua->pdev->dev,
			 "raw_dma: DSP %u timeout (pos=%u wr=%u)\n",
			 dsp_idx, ds->cmd.hw_pos, ds->cmd.write_ptr);
		/* The DMA ref may still be fetched: park its buffer */
		ua_dsp_orphan_dma(ua, dma_buf, dma_addr, UA_PCIE_PAGE_SIZE,
				  &ds->cmd, NULL);
		return -ETIMEDOUT;
	}

	dma_free_coherent(&ua->pdev->dev, UA_PCIE_PAGE_SIZE,
			  dma_buf, dma_addr);
	return 0;
}

/*
 * ua_dsp_test_send_block - Test sendBlock with connect + response ring
 * @ua: device (must hold ua->lock)
 *
 * Sends the connect command via both DMA ref and inline-with-addr formats,
 * checking the response ring for DSP replies.
 */
int UA_GCC_OPTIMIZE("O1") ua_dsp_test_send_block(struct ua_device *ua)
{
	struct ua_dsp_state *dsp0 = &ua->dsp[0];
	u32 bank_base = ua_dsp_base(0);
	u32 cmd_base = bank_base + UA_RING_CMD_OFFSET;
	u32 resp_base = bank_base + UA_RING_RESP_OFFSET;
	void *cmd_buf, *resp_buf;
	dma_addr_t cmd_addr, resp_addr;
	__le32 *cmd_words, *resp_words;
	struct ua_ring_entry entry;
	u32 cmd_pos, resp_pos, cmd_gen, resp_gen;
	int i;

	if (!dsp0->rings_allocated)
		return -ENODEV;

	i = ua_dsp_orphan_room(ua);
	if (i)
		return i;

	cmd_buf = dma_alloc_coherent(&ua->pdev->dev, UA_PCIE_PAGE_SIZE,
				     &cmd_addr, GFP_KERNEL);
	resp_buf = dma_alloc_coherent(&ua->pdev->dev, UA_PCIE_PAGE_SIZE,
				      &resp_addr, GFP_KERNEL);
	if (!cmd_buf || !resp_buf) {
		if (cmd_buf)
			dma_free_coherent(&ua->pdev->dev, UA_PCIE_PAGE_SIZE,
					  cmd_buf, cmd_addr);
		if (resp_buf)
			dma_free_coherent(&ua->pdev->dev, UA_PCIE_PAGE_SIZE,
					  resp_buf, resp_addr);
		return -ENOMEM;
	}

	cmd_words = cmd_buf;
	resp_words = resp_buf;

	dev_info(&ua->pdev->dev, "=== sendBlock test ===\n");
	dev_info(&ua->pdev->dev,
		 "cmd_addr=%pad resp_addr=%pad\n", &cmd_addr, &resp_addr);

	/*
	 * Test A: DMA ref format.
	 * DMA buffer = [0x00230002, 0x00000001] (connect cmd + param)
	 * Cmd ring entry = {2 | BIT31, cmd_addr_lo, cmd_addr_hi, 0}
	 * Resp ring entry = {4 | BIT31, resp_addr_lo, resp_addr_hi, 0}
	 */
	memset(cmd_buf, 0, UA_PCIE_PAGE_SIZE);
	memset(resp_buf, 0, UA_PCIE_PAGE_SIZE);
	cmd_words[0] = cpu_to_le32(UA_DSP_CMD_CONNECT);
	cmd_words[1] = cpu_to_le32(1);

	ua_dsp_ring_program(ua, &dsp0->cmd, cmd_base);
	ua_dsp_ring_program(ua, &dsp0->resp, resp_base);

	/* Both buffers are referenced from here on (see the end) */
	cmd_gen = dsp0->cmd.gen;
	resp_gen = dsp0->resp.gen;

	entry = (struct ua_ring_entry){
		.word0 = cpu_to_le32(UA_RING_DMA_REF | 2),
		.word1 = 0,
		.word2 = cpu_to_le32(lower_32_bits(cmd_addr)),
		.word3 = cpu_to_le32(upper_32_bits(cmd_addr)),
	};
	ua_dsp_ring_put_entry(ua, &dsp0->cmd, &entry);

	entry = (struct ua_ring_entry){
		.word0 = cpu_to_le32(UA_RING_DMA_REF | 4),
		.word1 = 0,
		.word2 = cpu_to_le32(lower_32_bits(resp_addr)),
		.word3 = cpu_to_le32(upper_32_bits(resp_addr)),
	};
	ua_dsp_ring_put_entry(ua, &dsp0->resp, &entry);

	ua_dsp_ring_doorbell(ua, &dsp0->cmd, cmd_base);
	ua_dsp_ring_doorbell(ua, &dsp0->resp, resp_base);
	msleep(100);

	cmd_pos = ua_read(ua, cmd_base + UA_RING_POSITION);
	resp_pos = ua_read(ua, resp_base + UA_RING_POSITION);
	dev_info(&ua->pdev->dev,
		 "Test A (DMA ref): cmd_pos=%u resp_pos=%u "
		 "resp=[%08x %08x %08x %08x]\n",
		 cmd_pos, resp_pos,
		 le32_to_cpu(resp_words[0]), le32_to_cpu(resp_words[1]),
		 le32_to_cpu(resp_words[2]), le32_to_cpu(resp_words[3]));

	/*
	 * Test B: Inline entry with DMA address.
	 * DMA buffer = [connect data if any, but connect has no payload]
	 * Cmd ring entry = {cmd, param, dma_addr_lo, dma_addr_hi}
	 * Resp ring entry = DMA ref for response capture
	 */
	memset(cmd_buf, 0, UA_PCIE_PAGE_SIZE);
	memset(resp_buf, 0, UA_PCIE_PAGE_SIZE);

	ua_dsp_ring_program(ua, &dsp0->cmd, cmd_base);
	ua_dsp_ring_program(ua, &dsp0->resp, resp_base);

	entry = (struct ua_ring_entry){
		.word0 = cpu_to_le32(UA_DSP_CMD_CONNECT),
		.word1 = cpu_to_le32(1),
		.word2 = cpu_to_le32(lower_32_bits(cmd_addr)),
		.word3 = cpu_to_le32(upper_32_bits(cmd_addr)),
	};
	ua_dsp_ring_put_entry(ua, &dsp0->cmd, &entry);

	entry = (struct ua_ring_entry){
		.word0 = cpu_to_le32(UA_RING_DMA_REF | 4),
		.word1 = 0,
		.word2 = cpu_to_le32(lower_32_bits(resp_addr)),
		.word3 = cpu_to_le32(upper_32_bits(resp_addr)),
	};
	ua_dsp_ring_put_entry(ua, &dsp0->resp, &entry);

	ua_dsp_ring_doorbell(ua, &dsp0->cmd, cmd_base);
	ua_dsp_ring_doorbell(ua, &dsp0->resp, resp_base);
	msleep(100);

	cmd_pos = ua_read(ua, cmd_base + UA_RING_POSITION);
	resp_pos = ua_read(ua, resp_base + UA_RING_POSITION);
	dev_info(&ua->pdev->dev,
		 "Test B (inline+addr): cmd_pos=%u resp_pos=%u "
		 "resp=[%08x %08x %08x %08x]\n",
		 cmd_pos, resp_pos,
		 le32_to_cpu(resp_words[0]), le32_to_cpu(resp_words[1]),
		 le32_to_cpu(resp_words[2]), le32_to_cpu(resp_words[3]));

	/*
	 * Test C: Pure inline (no DMA), with response ring DMA ref.
	 * This is the working connect command — does the resp ring capture
	 * anything?
	 */
	memset(resp_buf, 0, UA_PCIE_PAGE_SIZE);

	ua_dsp_ring_program(ua, &dsp0->cmd, cmd_base);
	ua_dsp_ring_program(ua, &dsp0->resp, resp_base);

	entry = (struct ua_ring_entry){
		.word0 = cpu_to_le32(UA_DSP_CMD_CONNECT),
		.word1 = cpu_to_le32(1),
		.word2 = 0,
		.word3 = 0,
	};
	ua_dsp_ring_put_entry(ua, &dsp0->cmd, &entry);

	entry = (struct ua_ring_entry){
		.word0 = cpu_to_le32(UA_RING_DMA_REF | 4),
		.word1 = 0,
		.word2 = cpu_to_le32(lower_32_bits(resp_addr)),
		.word3 = cpu_to_le32(upper_32_bits(resp_addr)),
	};
	ua_dsp_ring_put_entry(ua, &dsp0->resp, &entry);

	ua_dsp_ring_doorbell(ua, &dsp0->cmd, cmd_base);
	ua_dsp_ring_doorbell(ua, &dsp0->resp, resp_base);
	msleep(100);

	cmd_pos = ua_read(ua, cmd_base + UA_RING_POSITION);
	resp_pos = ua_read(ua, resp_base + UA_RING_POSITION);
	dev_info(&ua->pdev->dev,
		 "Test C (inline+resp): cmd_pos=%u resp_pos=%u "
		 "resp=[%08x %08x %08x %08x]\n",
		 cmd_pos, resp_pos,
		 le32_to_cpu(resp_words[0]), le32_to_cpu(resp_words[1]),
		 le32_to_cpu(resp_words[2]), le32_to_cpu(resp_words[3]));

	/* Also check if the resp ring page itself has data */
	{
		__le32 *ring_page = dsp0->resp.pages[0];

		dev_info(&ua->pdev->dev,
			 "Resp ring page[0]: [%08x %08x %08x %08x]\n",
			 le32_to_cpu(ring_page[0]), le32_to_cpu(ring_page[1]),
			 le32_to_cpu(ring_page[2]), le32_to_cpu(ring_page[3]));
	}

	/*
	 * Test D: Read response ring registers directly.
	 * Maybe the DSP writes responses to the ring page directly
	 * (like inline entries in reverse direction).
	 */
	for (i = 0; i < 4; i++) {
		u32 val = ua_read(ua, resp_base + i * 4);

		dev_info(&ua->pdev->dev,
			 "Resp ring reg[0x%02x] = 0x%08x\n",
			 i * 4, val);
	}

	/*
	 * Test E: Firmware load command via single DMA ref.
	 * The kext puts [packed_cmd, param, data...] in ONE buffer.
	 * DMA ref entry points to the whole thing.
	 * Even a tiny "firmware" (1 dword) should elicit a response.
	 *
	 * Buffer: [(1+1)|0x120000, 0x80040000, 0x00000000]
	 *       = [0x00120002, 0x80040000, 0x00000000]  (3 dwords)
	 * Cmd entry: {3|BIT31, addr_lo, addr_hi, 0}
	 * Resp entry: {4|BIT31, resp_addr_lo, resp_addr_hi, 0}
	 */
	memset(cmd_buf, 0, UA_PCIE_PAGE_SIZE);
	memset(resp_buf, 0xCC, UA_PCIE_PAGE_SIZE);  /* Canary pattern */
	cmd_words[0] = cpu_to_le32((1 + 1) | UA_FW_CMD);   /* 0x00120002 */
	cmd_words[1] = cpu_to_le32(UA_FW_PARAM);            /* 0x80040000 */
	cmd_words[2] = cpu_to_le32(0);                       /* 1 dword of "firmware" */

	ua_dsp_ring_program(ua, &dsp0->resp, resp_base);
	ua_dsp_ring_program(ua, &dsp0->cmd, cmd_base);

	/* Resp ring first (provide buffer before sending command) */
	entry = (struct ua_ring_entry){
		.word0 = cpu_to_le32(UA_RING_DMA_REF | 4),
		.word1 = 0,
		.word2 = cpu_to_le32(lower_32_bits(resp_addr)),
		.word3 = cpu_to_le32(upper_32_bits(resp_addr)),
	};
	ua_dsp_ring_put_entry(ua, &dsp0->resp, &entry);
	ua_dsp_ring_doorbell(ua, &dsp0->resp, resp_base);

	/* Small delay, then cmd */
	usleep_range(1000, 2000);

	entry = (struct ua_ring_entry){
		.word0 = cpu_to_le32(UA_RING_DMA_REF | 3),
		.word1 = 0,
		.word2 = cpu_to_le32(lower_32_bits(cmd_addr)),
		.word3 = cpu_to_le32(upper_32_bits(cmd_addr)),
	};
	ua_dsp_ring_put_entry(ua, &dsp0->cmd, &entry);
	ua_dsp_ring_doorbell(ua, &dsp0->cmd, cmd_base);

	msleep(500);  /* Give DSP time to process firmware command */

	cmd_pos = ua_read(ua, cmd_base + UA_RING_POSITION);
	resp_pos = ua_read(ua, resp_base + UA_RING_POSITION);
	dev_info(&ua->pdev->dev,
		 "Test E (fw load DMA ref): cmd_pos=%u resp_pos=%u "
		 "resp=[%08x %08x %08x %08x]\n",
		 cmd_pos, resp_pos,
		 le32_to_cpu(resp_words[0]), le32_to_cpu(resp_words[1]),
		 le32_to_cpu(resp_words[2]), le32_to_cpu(resp_words[3]));

	/*
	 * Test F: Same firmware command but INLINE (no DMA ref).
	 * entry = {packed_cmd, param, 0, 0}
	 * If this produces a response but Test E didn't, the issue is DMA.
	 */
	memset(resp_buf, 0xDD, UA_PCIE_PAGE_SIZE);

	ua_dsp_ring_program(ua, &dsp0->resp, resp_base);
	ua_dsp_ring_program(ua, &dsp0->cmd, cmd_base);

	entry = (struct ua_ring_entry){
		.word0 = cpu_to_le32(UA_RING_DMA_REF | 4),
		.word1 = 0,
		.word2 = cpu_to_le32(lower_32_bits(resp_addr)),
		.word3 = cpu_to_le32(upper_32_bits(resp_addr)),
	};
	ua_dsp_ring_put_entry(ua, &dsp0->resp, &entry);
	ua_dsp_ring_doorbell(ua, &dsp0->resp, resp_base);
	usleep_range(1000, 2000);

	entry = (struct ua_ring_entry){
		.word0 = cpu_to_le32((1 + 1) | UA_FW_CMD),  /* 0x00120002 */
		.word1 = cpu_to_le32(UA_FW_PARAM),           /* 0x80040000 */
		.word2 = 0,
		.word3 = 0,
	};
	ua_dsp_ring_put_entry(ua, &dsp0->cmd, &entry);
	ua_dsp_ring_doorbell(ua, &dsp0->cmd, cmd_base);

	msleep(500);

	cmd_pos = ua_read(ua, cmd_base + UA_RING_POSITION);
	resp_pos = ua_read(ua, resp_base + UA_RING_POSITION);
	dev_info(&ua->pdev->dev,
		 "Test F (fw load inline): cmd_pos=%u resp_pos=%u "
		 "resp=[%08x %08x %08x %08x]\n",
		 cmd_pos, resp_pos,
		 le32_to_cpu(resp_words[0]), le32_to_cpu(resp_words[1]),
		 le32_to_cpu(resp_words[2]), le32_to_cpu(resp_words[3]));

	dev_info(&ua->pdev->dev, "=== sendBlock test complete ===\n");

	/*
	 * Unfilled response descriptors / unfetched DMA refs (some possibly
	 * abandoned by the reprograms between the tests) may still point at
	 * these pages: park them; freed at once if the rings are done.
	 */
	ua_dsp_orphan_dma_gen(ua, cmd_buf, cmd_addr, UA_PCIE_PAGE_SIZE,
			      &dsp0->cmd, cmd_gen, &dsp0->resp, resp_gen);
	ua_dsp_orphan_dma_gen(ua, resp_buf, resp_addr, UA_PCIE_PAGE_SIZE,
			      &dsp0->cmd, cmd_gen, &dsp0->resp, resp_gen);
	return 0;
}

/* ----------------------------------------------------------------
 * Mixer firmware blob loading (request_firmware API)
 * ---------------------------------------------------------------- */

#define UA_MIXER_FW_HDR_SIZE	16
#define UA_MIXER_FW_ENTRY_SIZE	12
#define UA_MIXER_FW_MAX_BLOCKS	64

/*
 * Check all of ua-apollo-mixer.bin before the first block goes out: the
 * header, the block count, and every block's size and range.  A block
 * that failed its check inside the send loop used to leave the blocks
 * before it already sent to DSP 0, a half-loaded program the driver then
 * did not mark loaded.  Each block must lie inside the file and after the
 * block table (the packer puts the payloads there; ranges may alias each
 * other), be a whole number of dwords, and fit what ua_dsp_send_block()
 * takes: at most UA_FW_MAX_SIZE, and few enough data descriptors to fit
 * one command ring with the header.  Returns the block count or a
 * negative errno; logs the first problem.
 */
static int ua_dsp_mixer_fw_check(struct ua_device *ua,
				 const struct firmware *fw)
{
	const u8 *data = fw->data;
	u32 magic, version, num_blocks;
	size_t table_end;
	unsigned int i;

	if (fw->size < UA_MIXER_FW_HDR_SIZE) {
		dev_err(&ua->pdev->dev, "mixer firmware too small (%zu bytes)\n",
			fw->size);
		return -EINVAL;
	}

	magic = le32_to_cpup((__le32 *)&data[0]);
	version = le32_to_cpup((__le32 *)&data[4]);
	num_blocks = le32_to_cpup((__le32 *)&data[8]);

	if (magic != UA_MIXER_FW_MAGIC) {
		dev_err(&ua->pdev->dev,
			"mixer firmware bad magic: 0x%08x (expected 0x%08x)\n",
			magic, UA_MIXER_FW_MAGIC);
		return -EINVAL;
	}

	if (version != UA_MIXER_FW_VERSION) {
		dev_err(&ua->pdev->dev,
			"mixer firmware version %u unsupported (expected %u)\n",
			version, UA_MIXER_FW_VERSION);
		return -EINVAL;
	}

	if (num_blocks == 0 || num_blocks > UA_MIXER_FW_MAX_BLOCKS) {
		dev_err(&ua->pdev->dev,
			"mixer firmware invalid block count: %u\n", num_blocks);
		return -EINVAL;
	}

	table_end = UA_MIXER_FW_HDR_SIZE +
		    (size_t)num_blocks * UA_MIXER_FW_ENTRY_SIZE;
	if (fw->size < table_end) {
		dev_err(&ua->pdev->dev,
			"mixer firmware truncated (need %zu, got %zu)\n",
			table_end, fw->size);
		return -EINVAL;
	}

	for (i = 0; i < num_blocks; i++) {
		size_t entry_off = UA_MIXER_FW_HDR_SIZE +
				   (size_t)i * UA_MIXER_FW_ENTRY_SIZE;
		u32 block_size = le32_to_cpup((__le32 *)&data[entry_off + 4]);
		u32 block_offset = le32_to_cpup((__le32 *)&data[entry_off + 8]);

		/* Inside the file, no u32 wraparound */
		if (block_size > fw->size ||
		    block_offset > fw->size - block_size) {
			dev_err(&ua->pdev->dev,
				"block %u overflows firmware (off=%u size=%u total=%zu)\n",
				i, block_offset, block_size, fw->size);
			return -EINVAL;
		}

		/* Not the header or the block table */
		if (block_size && block_offset < table_end) {
			dev_err(&ua->pdev->dev,
				"block %u starts inside the block table (off=%u, table ends at %zu)\n",
				i, block_offset, table_end);
			return -EINVAL;
		}

		if (block_size & 3) {
			dev_err(&ua->pdev->dev,
				"block %u size %u not 4-byte aligned\n",
				i, block_size);
			return -EINVAL;
		}

		/* What ua_dsp_send_block() would refuse after earlier blocks */
		if (block_size > UA_FW_MAX_SIZE ||
		    DIV_ROUND_UP(block_size, ua_sendblock_chunk()) + 1 >=
		    UA_RING_NUM_PAGES * UA_RING_ENTRIES_PER_PAGE) {
			dev_err(&ua->pdev->dev,
				"block %u too large for one send (%u bytes)\n",
				i, block_size);
			return -E2BIG;
		}
	}

	return num_blocks;
}

/**
 * ua_dsp_load_mixer_blocks - Send the ua-apollo-mixer.bin stream to DSP 0
 * @ua: device (must hold ua->lock)
 *
 * Uses the Linux request_firmware() API to load ua-apollo-mixer.bin
 * from /lib/firmware/.  The blob contains a header, block table, and
 * concatenated payloads (packed by tools/pack-mixer-firmware.py).
 *
 * What the blob is: 15 back-to-back streams of "Bill" DSP resources,
 * captured from SEL127 (RegisterSharedResourceDMA) on macOS.  It is NOT a
 * DSP boot image.  The macOS kext never uploads anything at start: the
 * DSPs boot themselves, and Bill code reaches a DSP only when the
 * resource manager places a plugin (parity note 13, findings 0.1/0.2).
 * What the 0x12 firmware transaction does with these streams on a cold
 * x4 is unproven (the Windows driver is where this behaviour comes from),
 * so the cold start keeps sending them until a hardware test (note 13,
 * experiments E1/E2) shows which part matters.
 *
 * Only DSP 0 is ever loaded.  The kext's _sendBlock hard-codes DSP 0 and
 * DSPs 1+ need nothing: the traffic seen on them in Windows captures is
 * resource-manager work for strips hosted there, not firmware.
 *
 * Each block is sent to DSP 0 via ua_dsp_send_block() with:
 *   cmd   = UA_FW_CMD (0x120000)
 *   param = UA_FW_PARAM (0x80040000, the expected response word)
 *   data  = block payload
 *
 * Returns 0 on success, negative errno on failure.
 * -ENOENT if firmware file not found (non-fatal, expected on first install).
 */
int ua_dsp_load_mixer_blocks(struct ua_device *ua)
{
	const struct firmware *fw;
	const u8 *data;
	unsigned int i, num_blocks;
	int ret;

	ret = request_firmware(&fw, UA_MIXER_FW_NAME, &ua->pdev->dev);
	if (ret) {
		if (ret == -ENOENT)
			dev_warn(&ua->pdev->dev,
				 "mixer firmware '%s' not found in /lib/firmware/\n",
				 UA_MIXER_FW_NAME);
		else
			dev_err(&ua->pdev->dev,
				"failed to load mixer firmware: %d\n", ret);
		return ret;
	}

	/* The whole table, before anything is sent */
	ret = ua_dsp_mixer_fw_check(ua, fw);
	if (ret < 0)
		goto out;
	num_blocks = ret;
	ret = 0;
	data = fw->data;

	dev_dbg(&ua->pdev->dev,
		"loading mixer firmware: %u blocks, %zu bytes\n",
		num_blocks, fw->size);

	/*
	 * Commands 0xD0000 and 0xE0000 are the kext's licensing block
	 * loaders (authorization and demo blocks).  They are not firmware
	 * phases and are never sent by this driver.
	 */

	/* Firmware transaction (0x120000) for each block */
	for (i = 0; i < num_blocks; i++) {
		size_t entry_off = UA_MIXER_FW_HDR_SIZE +
				   (size_t)i * UA_MIXER_FW_ENTRY_SIZE;
		u32 sram_addr = le32_to_cpup((__le32 *)&data[entry_off]);
		u32 block_size = le32_to_cpup((__le32 *)&data[entry_off + 4]);
		u32 block_offset = le32_to_cpup((__le32 *)&data[entry_off + 8]);

		/*
		 * The block table's sram_addr is the capture's host address
		 * of the SEL127 buffer, not a DSP address (note 13, 0.2), so
		 * it is never used as a command.  The kext's LoadFirmware is
		 * always cmd 0x120000, expected response 0x80040000.
		 */
		ret = ua_dsp_send_block(ua, 0, UA_FW_CMD, UA_FW_PARAM,
					&data[block_offset], block_size);
		if (ret) {
			dev_err(&ua->pdev->dev,
				"block %u (sram=0x%08x, %u bytes) send failed: %d\n",
				i, sram_addr, block_size, ret);
			goto out;
		}
	}

	ua->fw_loaded = true;
	ua->dspx.replay_pending = true;
	dev_info(&ua->pdev->dev,
		 "mixer firmware loaded: %u blocks sent to DSP 0\n",
		 num_blocks);

out:
	release_firmware(fw);
	return ret;
}

/* ----------------------------------------------------------------
 * DSP connect
 * ---------------------------------------------------------------- */

/**
 * ua_dsp_connect_all - Send connect commands to all DSPs
 * @ua: device (must hold ua->lock)
 *
 * Protocol (from kext CUAD2Device::_connectToDsps, 12.0.0 x86 0x13d6e):
 *   Phase 1: For each DSP i: inline {0x00230002, 1, 0, 0} + doorbell
 *   Phase 2: DSP 0 only: inline {0x00100002, date, 0, 0} + doorbell,
 *            queued behind DSP 0's connect entry (no ring reset)
 */
int ua_dsp_connect_all(struct ua_device *ua)
{
	struct ua_ring_entry entry;
	unsigned int i;
	u32 date;
	int ret;

	/* _connectToDsps: nothing to do without DSPs (kext_connect only) */
	if (ua_kext_connect && !ua->num_dsps)
		return 0;

	if (!ua->fw_loaded)
		dev_info(&ua->pdev->dev,
			 "DSP connect: firmware not loaded (testing)\n");

	/* Phase 1: Send connect command to each DSP */
	for (i = 0; i < ua->num_dsps; i++) {
		struct ua_dsp_state *ds = &ua->dsp[i];
		u32 bank_base = ua_dsp_base(i);

		if (!ds->rings_allocated)
			continue;

		/*
		 * kext_connect: only a DSP that reports property 0xC gets
		 * the connect command (_connectToDsps step 2); the date
		 * word below is sent regardless (step 3).
		 */
		if (ua_kext_connect && !ua_dsp_prop_ring_connect(ua))
			continue;

		/* Resync the cmd ring to POSITION for a fresh command */
		ua_dsp_ring_program(ua, &ds->cmd,
				    bank_base + UA_RING_CMD_OFFSET);

		entry = (struct ua_ring_entry){
			.word0 = cpu_to_le32(UA_DSP_CMD_CONNECT),
			.word1 = cpu_to_le32(1),
			.word2 = 0,
			.word3 = 0,
		};

		ret = ua_dsp_ring_put_entry(ua, &ds->cmd, &entry);
		if (ret) {
			dev_err(&ua->pdev->dev,
				"DSP %u: connect entry failed (%d)\n", i, ret);
			return ret;
		}

		ua_dsp_ring_doorbell(ua, &ds->cmd,
				     bank_base + UA_RING_CMD_OFFSET);

		usleep_range(5000, 10000);
		dev_dbg(&ua->pdev->dev,
			"DSP %u connect: pos=%u (expect %u)\n", i,
			ua_read(ua, bank_base + UA_RING_CMD_OFFSET +
				 UA_RING_POSITION),
			ds->cmd.write_ptr);
	}

	/* Brief delay between phases */
	usleep_range(1000, 2000);

	/*
	 * Phase 2: Send finalize command to DSP 0.  w1 carries the date
	 * word (Linux used to send 0).  Appended to the ring rather than
	 * after a ring reset, which could drop the connect entry above
	 * if DSP 0 had not consumed it yet.
	 */
	date = ua_dsp_connect_date();
	if (ua->dsp[0].rings_allocated) {
		u32 bank_base = ua_dsp_base(0);

		entry = (struct ua_ring_entry){
			.word0 = cpu_to_le32(UA_DSP_CMD_FINALIZE),
			.word1 = cpu_to_le32(date),
			.word2 = 0,
			.word3 = 0,
		};

		ret = ua_dsp_ring_put_entry(ua, &ua->dsp[0].cmd, &entry);
		if (ret) {
			dev_err(&ua->pdev->dev,
				"DSP 0: finalize entry failed (%d)\n", ret);
			return ret;
		}

		ua_dsp_ring_doorbell(ua, &ua->dsp[0].cmd,
				     bank_base + UA_RING_CMD_OFFSET);
	}

	usleep_range(5000, 10000);
	dev_dbg(&ua->pdev->dev,
		"DSP 0 finalize (date 0x%08x): pos=%u (expect %u)\n",
		date,
		ua_read(ua, ua_dsp_base(0) + UA_RING_CMD_OFFSET +
			 UA_RING_POSITION),
		ua->dsp[0].cmd.write_ptr);

	ua->dsps_connected = true;
	/* A reconnect after the audio side masked every vector */
	ua_dsp_irq_arm(ua);
	dev_info(&ua->pdev->dev, "all %u DSPs connected\n", ua->num_dsps);
	return 0;
}

/* ----------------------------------------------------------------
 * DSP-ready wait and connect retry (audit gap D14)
 * ---------------------------------------------------------------- */

/*
 * CPcieDSP::_waitFor469ToStart (kext 12.0.0 x86 0x8526, called from
 * CPcieDSP::ProgramRegisters 0x8258) polls the DSP window word at +0x1a4.
 * The word reads 0 or 0xa8caed0f until the DSP's boot code has run; any
 * other value means started.  Per DSP: 100 polls of 300 ms for DSP 0, 10
 * for the others.  The kext polls only when the capability word the device
 * reports in UAD2DeviceInfo+8 is 1.  CPcieDevice::GetCapabilities (0x464c)
 * fills that with ext_caps (0x2234) bits[7:4] on v2 firmware, 0 on v1.
 */
#define UA_DSP_READY_REG	0x1a4
#define UA_DSP_READY_MAGIC	0xa8caed0fu
#define UA_DSP_READY_POLL_MS	300

static int dsp_ready_wait;
module_param(dsp_ready_wait, int, 0444);
MODULE_PARM_DESC(dsp_ready_wait,
		 "EXPERIMENTAL (kext-vs-driver audit D14), 0 = off (default). On a cold start, before the DSP rings are set up, poll each DSP's ready word as the macOS driver does, and retry a failed DSP connect once. 1 = only on devices whose ext_caps bits[7:4] == 1 (the macOS driver's rule); 2 = every v2 device regardless of capability. One log line reports the outcome.");

/* DSP n's register window: n * 0x800, plus 0x2000 for DSPs 4-7 */
static inline u32 ua_dsp_window(unsigned int dsp_idx)
{
	return dsp_idx * 0x800 + (dsp_idx > 3 ? 0x2000 : 0);
}

/**
 * ua_dsp_wait_ready - Bounded poll of each DSP's boot-ready word
 * @ua: device
 *
 * No register access unless dsp_ready_wait is set.  Never fails the probe:
 * a timeout is logged and the cold start continues as before.
 *
 * Returns 0 when every polled DSP is ready (or none was polled),
 * -ETIMEDOUT when one never became ready, -ENODEV when the device went
 * away, -EINTR on a fatal signal.
 */
int ua_dsp_wait_ready(struct ua_device *ua)
{
	struct device *dev = &ua->pdev->dev;
	u32 ext_caps, cap;
	unsigned int i, polled = 0, late = 0;
	int ret = 0;

	if (!dsp_ready_wait)
		return 0;

	if (!ua->fw_v2) {
		dev_info(dev, "DSP-ready wait: v1 firmware (capability 0), skipped\n");
		return 0;
	}

	ext_caps = ua_read(ua, UA_REG_EXT_CAPS);
	cap = (ext_caps >> 4) & 0xf;
	if (dsp_ready_wait != 2 && cap != 1) {
		dev_info(dev,
			 "DSP-ready wait: ext_caps 0x%08x, capability %u != 1, skipped (as the macOS driver)\n",
			 ext_caps, cap);
		return 0;
	}

	for (i = 0; i < ua->num_dsps; i++) {
		unsigned int tries = i == 0 ? 100 : 10;
		unsigned int n;
		u32 v = 0;

		for (n = 0; n < tries; n++) {
			if (atomic_read(&ua->shutdown) ||
			    ua_read(ua, UA_REG_FPGA_REV) == 0xFFFFFFFF) {
				dev_warn(dev,
					 "DSP-ready wait: device gone, aborted at DSP %u\n",
					 i);
				return -ENODEV;
			}
			v = ua_read(ua, ua_dsp_window(i) + UA_DSP_READY_REG);
			if (v && v != UA_DSP_READY_MAGIC)
				break;
			if (fatal_signal_pending(current))
				return -EINTR;
			msleep(UA_DSP_READY_POLL_MS);
		}

		polled++;
		if (n == tries) {
			late++;
			ret = -ETIMEDOUT;
			dev_warn(dev,
				 "DSP-ready wait: DSP %u not ready after %u ms (word 0x%08x)\n",
				 i, tries * UA_DSP_READY_POLL_MS, v);
		} else if (n) {
			dev_dbg(dev, "DSP %u ready after %u polls (0x%08x)\n",
				i, n, v);
		}
	}

	dev_info(dev,
		 "DSP-ready wait: ext_caps 0x%08x, %u DSP(s) polled, %u not ready\n",
		 ext_caps, polled, late);
	return ret;
}

/**
 * ua_dsp_connect_retry - ua_dsp_connect_all() with the kext's second pass
 * @ua: device (must hold ua->lock)
 *
 * The kext's connect retries a failed connect at most once more, after
 * ResetDMAEngine(0) + CPcieDSP::ProgramRegisters(DSP 0).  With
 * kext_connect the engine reset is done as the kext does it (enable bit
 * cleared, strobe pulsed, engine re-enabled after the ring is
 * reprogrammed); with dsp_ready_wait alone only the rings are
 * re-pointed and resynced, as before.  Neither: one plain call.
 */
int ua_dsp_connect_retry(struct ua_device *ua)
{
	int ret = ua_dsp_connect_all(ua);

	if (!ret || atomic_read(&ua->shutdown) ||
	    !(dsp_ready_wait || ua_kext_connect))
		return ret;

	dev_warn(&ua->pdev->dev,
		 "DSP connect failed (%d), reprogramming rings and retrying once\n",
		 ret);
	if (ua_kext_connect)
		ua_dma_reset_engine(ua, 0);
	ua_dsp_rings_reprogram(ua);
	if (ua_kext_connect)
		ua_dma_enable_engine(ua, 0);
	ret = ua_dsp_connect_all(ua);
	dev_info(&ua->pdev->dev, "DSP connect retry: %s (%d)\n",
		 ret ? "failed" : "ok", ret);
	return ret;
}

/* ----------------------------------------------------------------
 * Plugin chain activation
 * ---------------------------------------------------------------- */

#include "ua_plugin_chain.h"

/* Dwords a DMA_REF entry makes the FPGA fetch (its word 0 below bit 31) */
static u32 ua_ring_ref_dwords(u32 word0)
{
	return word0 & ~(u32)UA_RING_DMA_REF;
}

/**
 * ua_dsp_load_plugin_payloads - Load plugin-chain DMA payloads from firmware
 * @ua: device
 *
 * The ua_plugin_chain_data[] table includes DMA_REF entries whose
 * cmd[2]/cmd[3] are Windows host physical addresses captured on the
 * original driver's machine.  Those addresses are meaningless on
 * Linux; the FPGA would fetch garbage.  This function loads the
 * captured payload bytes from /lib/firmware/ua-apollo-plugin-chain.bin
 * (packed by tools/build-plugin-chain-firmware.py) and allocates one
 * coherent DMA buffer per DMA_REF entry, keyed by ring_idx.
 *
 * ua_dsp_activate_plugin_chain() then substitutes cmd[2]/cmd[3] with
 * the Linux dma_addr of the matching buffer before handing the entry
 * to the ring.
 *
 * The whole table is checked before anything is allocated: every entry
 * names a DMA_REF row of ua_plugin_chain_data[] (each at most once),
 * lies inside the file after the table, and holds at least the dwords
 * that row makes the FPGA fetch.  tools/build-plugin-chain-firmware.py
 * writes exactly one such entry per DMA_REF row.
 *
 * -ENOENT (blob not installed) is a soft failure: activation is then
 * skipped (-ENODATA) and retried at the next connect.
 */
int ua_dsp_load_plugin_payloads(struct ua_device *ua)
{
	const struct firmware *fw;
	const u8 *data;
	u32 magic, version, entry_count, data_offset;
	unsigned int table_size, i;
	DECLARE_BITMAP(seen, UA_PLUGIN_CHAIN_COUNT);
	int ret;

	if (ua->plugin_bufs)
		return 0;   /* Already loaded */

	/* Optional blob, retried on every connect: stay quiet when absent. */
	ret = firmware_request_nowarn(&fw, UA_PLUGIN_CHAIN_FW_NAME,
				      &ua->pdev->dev);
	if (ret) {
		if (ret == -ENOENT)
			dev_warn_once(&ua->pdev->dev,
				      "plugin chain firmware '%s' not found; "
				      "plugin chain not activated (optional: "
				      "the preamp and monitor controls use the "
				      "mixer settings)\n",
				      UA_PLUGIN_CHAIN_FW_NAME);
		else
			dev_err(&ua->pdev->dev,
				"plugin chain firmware load failed: %d\n", ret);
		return ret;
	}

	data = fw->data;

	if (fw->size < 16) {
		dev_err(&ua->pdev->dev,
			"plugin chain fw too small (%zu bytes)\n", fw->size);
		ret = -EINVAL;
		goto out;
	}

	magic        = le32_to_cpup((__le32 *)&data[0]);
	version      = le32_to_cpup((__le32 *)&data[4]);
	entry_count  = le32_to_cpup((__le32 *)&data[8]);
	data_offset  = le32_to_cpup((__le32 *)&data[12]);

	if (magic != UA_PLUGIN_CHAIN_FW_MAGIC) {
		dev_err(&ua->pdev->dev,
			"plugin chain fw bad magic 0x%08x\n", magic);
		ret = -EINVAL;
		goto out;
	}
	if (version != UA_PLUGIN_CHAIN_FW_VERSION) {
		dev_err(&ua->pdev->dev,
			"plugin chain fw version %u unsupported\n", version);
		ret = -EINVAL;
		goto out;
	}
	if (entry_count == 0 || entry_count > 512) {
		dev_err(&ua->pdev->dev,
			"plugin chain fw bad entry count %u\n", entry_count);
		ret = -EINVAL;
		goto out;
	}

	table_size = entry_count * 12;
	if (fw->size < 16 + table_size || data_offset < 16 + table_size ||
	    data_offset > fw->size) {
		dev_err(&ua->pdev->dev,
			"plugin chain fw truncated (size=%zu, need >= %u)\n",
			fw->size, 16 + table_size);
		ret = -EINVAL;
		goto out;
	}

	/* Every entry, before the first allocation */
	bitmap_zero(seen, UA_PLUGIN_CHAIN_COUNT);
	for (i = 0; i < entry_count; i++) {
		unsigned int entry_off = 16 + i * 12;
		u32 ring_idx = le32_to_cpup((__le32 *)&data[entry_off]);
		u32 payload_bytes = le32_to_cpup((__le32 *)&data[entry_off + 4]);
		u32 payload_off = le32_to_cpup((__le32 *)&data[entry_off + 8]);
		u32 row0 = ring_idx < UA_PLUGIN_CHAIN_COUNT ?
			   ua_plugin_chain_data[ring_idx][0] : 0;

		if (!payload_bytes || payload_bytes > fw->size ||
		    payload_off < data_offset ||
		    payload_off > fw->size - payload_bytes) {
			dev_err(&ua->pdev->dev,
				"plugin fw entry %u bad range "
				"(off=%u bytes=%u total=%zu)\n",
				i, payload_off, payload_bytes, fw->size);
			ret = -EINVAL;
			goto out;
		}
		if (!(row0 & UA_RING_DMA_REF)) {
			dev_err(&ua->pdev->dev,
				"plugin fw entry %u: ring index %u is not a DMA_REF entry of the chain\n",
				i, ring_idx);
			ret = -EINVAL;
			goto out;
		}
		if (__test_and_set_bit(ring_idx, seen)) {
			dev_err(&ua->pdev->dev,
				"plugin fw entry %u: ring index %u given twice\n",
				i, ring_idx);
			ret = -EINVAL;
			goto out;
		}
		if ((size_t)ua_ring_ref_dwords(row0) * 4 > payload_bytes) {
			dev_err(&ua->pdev->dev,
				"plugin fw entry %u: %u bytes, ring index %u fetches %u dwords\n",
				i, payload_bytes, ring_idx,
				ua_ring_ref_dwords(row0));
			ret = -EINVAL;
			goto out;
		}
	}

	ua->plugin_bufs = kcalloc(entry_count,
				  sizeof(*ua->plugin_bufs), GFP_KERNEL);
	if (!ua->plugin_bufs) {
		ret = -ENOMEM;
		goto out;
	}
	ua->plugin_bufs_count = entry_count;

	for (i = 0; i < entry_count; i++) {
		unsigned int entry_off = 16 + i * 12;
		u32 ring_idx, payload_bytes, payload_off;
		struct ua_plugin_dma_buf *buf = &ua->plugin_bufs[i];

		ring_idx      = le32_to_cpup((__le32 *)&data[entry_off]);
		payload_bytes = le32_to_cpup((__le32 *)&data[entry_off + 4]);
		payload_off   = le32_to_cpup((__le32 *)&data[entry_off + 8]);

		/* Range and ring index checked above */
		buf->ring_idx = ring_idx;
		buf->size = payload_bytes;
		buf->cpu = dma_alloc_coherent(&ua->pdev->dev, payload_bytes,
					      &buf->dma, GFP_KERNEL);
		if (!buf->cpu) {
			dev_err(&ua->pdev->dev,
				"plugin fw entry %u: dma_alloc %u bytes failed\n",
				i, payload_bytes);
			ret = -ENOMEM;
			goto out_free;
		}
		memcpy(buf->cpu, &data[payload_off], payload_bytes);
	}

	dev_info(&ua->pdev->dev,
		 "plugin chain firmware loaded: %u DMA_REF payloads, %zu bytes total\n",
		 entry_count, fw->size);
	release_firmware(fw);
	return 0;

out_free:
	ua_dsp_free_plugin_payloads(ua);
out:
	release_firmware(fw);
	return ret;
}

/**
 * ua_dsp_free_plugin_payloads - Release plugin-chain DMA buffers
 * @ua: device
 *
 * Called on device teardown or as rollback from ua_dsp_load_plugin_payloads().
 */
void ua_dsp_free_plugin_payloads(struct ua_device *ua)
{
	unsigned int i;

	if (!ua->plugin_bufs)
		return;

	for (i = 0; i < ua->plugin_bufs_count; i++) {
		struct ua_plugin_dma_buf *buf = &ua->plugin_bufs[i];

		if (buf->cpu)
			dma_free_coherent(&ua->pdev->dev, buf->size,
					  buf->cpu, buf->dma);
	}
	kfree(ua->plugin_bufs);
	ua->plugin_bufs = NULL;
	ua->plugin_bufs_count = 0;
}

/**
 * ua_plugin_lookup_dma - Find allocated DMA buffer for a ring index
 * @ua: device
 * @ring_idx: ring entry index the buffer was allocated for
 *
 * Returns NULL if no buffer was allocated for that entry.
 */
static struct ua_plugin_dma_buf *
ua_plugin_lookup_dma(struct ua_device *ua, unsigned int ring_idx)
{
	unsigned int i;

	if (!ua->plugin_bufs)
		return NULL;

	for (i = 0; i < ua->plugin_bufs_count; i++) {
		if (ua->plugin_bufs[i].ring_idx == ring_idx)
			return &ua->plugin_bufs[i];
	}
	return NULL;
}

/**
 * ua_dsp_activate_plugin_chain - Send plugin chain init to DSP 0
 * @ua: device (must hold ua->lock)
 *
 * Replays the SRAM_CFG + ROUTING + DMA_REF + MODULE_ACTIVATE + SYNCH
 * sequence captured from a working Windows driver.  This activates the
 * DSP mixer/routing modules so that SEL131 SetMixerParam commands
 * (monitor volume, gain, talkback, dim) take effect, AND loads the
 * DSP program that forwards flag writes (PAD, 48V, MicLine) from SRAM
 * to the ARM MCU so those relays actually toggle.
 *
 * DMA_REF entries (cmd[0] & 0x80000000) carry Windows host physical
 * addresses in cmd[2]/cmd[3] that are invalid on Linux.  Each one is
 * substituted with the Linux dma_addr of a coherent buffer whose
 * contents came from ua_dsp_load_plugin_payloads().  Without the blob
 * nothing is sent (-ENODATA); a blob that lacks the payload of any
 * DMA_REF entry, or has one shorter than the entry fetches, sends
 * nothing either (-EINVAL): the chain goes out whole or not at all.
 *
 * Data extracted from tools/captures/win-dsp0-ring-complete-20260319.json
 */
int ua_dsp_activate_plugin_chain(struct ua_device *ua)
{
	struct ua_dsp_state *ds = &ua->dsp[0];
	u32 bank_base = ua_dsp_base(0);
	u32 cmd_base = bank_base + UA_RING_CMD_OFFSET;
	struct ua_ring_entry entry;
	int i, j, batch, ret;
	int sent_total = 0, skipped = 0;

	if (!ds->rings_allocated) {
		dev_warn(&ua->pdev->dev,
			 "plugin chain: DSP 0 rings not ready\n");
		return -ENODEV;
	}

	/*
	 * Attempt to load payloads if not already loaded.  This lets a
	 * caller whose first probe happened before the firmware blob
	 * was installed pick it up on a later reconnect without a
	 * module reload.  Idempotent: returns 0 immediately if already
	 * loaded.
	 */
	if (!ua->plugin_bufs)
		(void)ua_dsp_load_plugin_payloads(ua);

	/*
	 * Refuse to send the plugin chain without payloads.  Sending it
	 * with every DMA_REF skipped leaves the DSP in a partially
	 * configured state: DSP programs that depend on DMA-fetched
	 * code or tables never initialize.  (The PAD/48V/Mic-Line relays
	 * do not depend on it: they follow the one-shot mixer settings,
	 * see mixer_mailbox.)  Returning an error
	 * here keeps ua->plugins_activated = false in the caller so a
	 * subsequent reconnect retries once the firmware blob is in
	 * /lib/firmware/.
	 */
	if (!ua->plugin_bufs) {
		dev_warn_once(&ua->pdev->dev,
			      "plugin chain: payloads not loaded, activation skipped "
			      "(%s is optional)\n",
			      UA_PLUGIN_CHAIN_FW_NAME);
		return -ENODATA;
	}

	/*
	 * A payload for every DMA_REF entry, big enough for the dwords it
	 * fetches, before the first entry goes out.  The old code skipped
	 * such entries one by one and still sent the rest of the chain: a
	 * half-configured DSP with no error.  The loader already refused
	 * entries that do not match a row; this catches rows left without
	 * one.
	 */
	for (i = 0; i < UA_PLUGIN_CHAIN_COUNT; i++) {
		const u32 *cmd = ua_plugin_chain_data[i];
		struct ua_plugin_dma_buf *buf;

		if (!(cmd[0] & UA_RING_DMA_REF))
			continue;
		buf = ua_plugin_lookup_dma(ua, i);
		if (!buf ||
		    (size_t)ua_ring_ref_dwords(cmd[0]) * 4 > buf->size) {
			/* Once: every connect (each rate change) retries */
			dev_warn_once(&ua->pdev->dev,
				      "plugin chain: no %u-dword payload for entry %d in %s, not activated\n",
				      ua_ring_ref_dwords(cmd[0]), i,
				      UA_PLUGIN_CHAIN_FW_NAME);
			return -EINVAL;
		}
	}

	dev_dbg(&ua->pdev->dev,
		"plugin chain: sending %d commands to DSP 0 "
		"(skip_bus_coeff=%d)\n",
		UA_PLUGIN_CHAIN_COUNT, ua->skip_bus_coeff);

	/*
	 * Send commands in batches of 64, one doorbell per batch, pacing
	 * on the DSP's consumption.  The ring is circular: resync it to
	 * POSITION once, then reserve room for each batch instead of
	 * resetting the ring per batch — a reset rewound the producer over
	 * entries of the previous batch the DSP had not consumed yet.
	 */
	ua_dsp_ring_program(ua, &ds->cmd, cmd_base);

	for (i = 0; i < UA_PLUGIN_CHAIN_COUNT; i += batch) {
		unsigned int batch_start;
		int sent = 0;

		batch = min(64, UA_PLUGIN_CHAIN_COUNT - i);

		ret = ua_dsp_ring_wait_space(ua, &ds->cmd, batch, 1000);
		if (ret) {
			dev_err(&ua->pdev->dev,
				"plugin chain: no ring space for batch at %d (%d, pos=%u wr=%u)\n",
				i, ret, ds->cmd.hw_pos, ds->cmd.write_ptr);
			return ret;
		}
		batch_start = ds->cmd.write_ptr;

		for (j = 0; j < batch; j++) {
			const u32 *cmd = ua_plugin_chain_data[i + j];
			u32 word2 = cmd[2];
			u32 word3 = cmd[3];

			/*
			 * Optionally skip BUS_COEFF commands (0x001D0004).
			 * The 745 BUS_COEFF entries set all mix coefficients
			 * to zero, which may override firmware-initialized
			 * monitor bus routing and prevent monitor processing.
			 */
			if (ua->skip_bus_coeff && cmd[0] == 0x001d0004) {
				skipped++;
				continue;
			}

			/*
			 * DMA_REF entries: substitute Windows host physical
			 * addresses (cmd[2]/cmd[3]) with the Linux dma_addr
			 * of the matching payload buffer (present and big
			 * enough: checked above, before the first entry).
			 */
			if (cmd[0] & UA_RING_DMA_REF) {
				struct ua_plugin_dma_buf *buf =
					ua_plugin_lookup_dma(ua, i + j);

				word2 = lower_32_bits(buf->dma);
				word3 = upper_32_bits(buf->dma);
			}

			entry = (struct ua_ring_entry){
				.word0 = cpu_to_le32(cmd[0]),
				.word1 = cpu_to_le32(cmd[1]),
				.word2 = cpu_to_le32(word2),
				.word3 = cpu_to_le32(word3),
			};

			ret = ua_dsp_ring_put_entry(ua, &ds->cmd, &entry);
			if (ret) {
				dev_err(&ua->pdev->dev,
					"plugin chain: entry %d failed\n",
					i + j);
				/* Not doorbelled: drop the partial batch */
				ds->cmd.write_ptr = batch_start;
				return ret;
			}
			sent++;
		}

		if (!sent)
			continue;  /* entire batch was BUS_COEFF */

		/* Single doorbell for the batch */
		ua_dsp_ring_doorbell(ua, &ds->cmd, cmd_base);

		/* Give the DSP ~10-20 ms to consume, as before */
		if (!ua_dsp_ring_wait_idle(ua, &ds->cmd, 15))
			dev_dbg(&ua->pdev->dev,
				"plugin chain: batch %d-%d pos=%u wr=%u (%u pending)\n",
				i, i + batch - 1, ds->cmd.hw_pos,
				ds->cmd.write_ptr,
				ua_dsp_ring_pending(&ds->cmd));
		sent_total += sent;
	}

	dev_info(&ua->pdev->dev,
		 "plugin chain: activated (%d sent, %d skipped)\n",
		 sent_total, skipped);
	return 0;
}

/* ----------------------------------------------------------------
 * Bus coefficient commands (SEL130 / SetMixerBusParam equivalent)
 *
 * From kext CUAD2DeviceMixer::SetMixerBusParam disassembly:
 * Bus coefficients (fader, pan, send levels) are submitted as ring
 * buffer commands to DSP 0, NOT through the BAR0+0x38xx mixer setting
 * registers.  The command format is:
 *
 *   word0: 0x001D0004   (cmd type 0x1D, 4 dwords)
 *   word1: bus_id        (0x00-0x1F)
 *   word2: (side << 24) | param    (kext SetMixerBusParam, parity/14 s5)
 *          param: 0 CR, 1 HP1, 2 HP2, 3 Aux 1, 4 Aux 2, 5 HP3, 6 HP4
 *          side:  0 = left output weight, 1 = right output weight
 *   word3: value         (IEEE 754 float as u32)
 *
 * This is a COMPLETELY DIFFERENT hardware path from SEL131/SEL132
 * which use mask/value paired writes to mixer setting registers.
 * ---------------------------------------------------------------- */

#define UA_DSP_CMD_BUS_PARAM	0x001D0004
#define UA_DSP_CMD_INPUT_PARAM	0x001E0004
#define UA_DSP_CMD_BUS_ENABLE	0x001C0003

/**
 * ua_dsp_ring_reset - Sync software write_ptr to FPGA POSITION
 * @ua: device
 * @dsp_idx: DSP index (0-3)
 *
 * The FPGA POSITION counter is the ring's consumer index (mod 1024) and
 * is not reset when the ring is reprogrammed.  Set write_ptr = POSITION
 * so new entries start where the FPGA expects them, and write WRITE_PTR
 * so the FPGA knows our starting position.  ua_dsp_ring_program() now
 * does the same resync; this keeps the exported helper consistent.
 */
void ua_dsp_ring_reset(struct ua_device *ua, unsigned int dsp_idx)
{
	struct ua_dsp_state *ds;

	if (dsp_idx >= ua->num_dsps)
		return;

	ds = &ua->dsp[dsp_idx];
	if (!ds->rings_allocated)
		return;

	/* Same resync as ua_dsp_ring_program(): DOORBELL too, orphans kept */
	ua_dsp_ring_resync(ua, &ds->cmd);
}

/*
 * bus_solo_sign: honour UA_DSP_BUSP_SOLO_MUTE on a bus coefficient.  The Mixer
 * Engine marks a solo-muted input by sending its weights negated, with an
 * exact zero replaced by -1.0e-9 (bits 0xb089705f) so the sign survives the
 * multiplication; the DSP reads the sign bit as the solo-mute flag
 * (parity/14 B13, iokit-mixer-mapping.md "Solo Behavior").  The stock mixer
 * daemon sends 0.0 for a muted channel and has no sign to send, so with the
 * knob off (default) the flag is ignored and every value goes out as given.
 * With it on, a caller that sets the flag gets the sign forced in the driver.
 */
static bool bus_solo_sign;
module_param(bus_solo_sign, bool, 0644);
MODULE_PARM_DESC(bus_solo_sign,
		 "Bus coefficient writes that carry UA_DSP_BUSP_SOLO_MUTE are sent as the macOS Mixer Engine sends a solo-muted weight: negative, an exact zero replaced by -1.0e-9 (default: off, values are sent as given)");

/* Does the driver honour UA_DSP_BUSP_SOLO_MUTE (GET_VERSION feature bit)? */
bool ua_dsp_solo_flag_honoured(void)
{
	return READ_ONCE(bus_solo_sign);
}

#define UA_BUS_SOLO_ZERO_BITS	0xb089705fu	/* -1.0e-9f (ME 0x100062268) */

static u32 ua_bus_solo_mute_value(u32 value)
{
	if (!(value & 0x7fffffff))
		return UA_BUS_SOLO_ZERO_BITS;
	return value | 0x80000000u;
}

static unsigned int ua_bus_coef_cell(u32 bus, u32 param, u32 side)
{
	return (bus * UA_BUS_COEF_PARAMS + param) * UA_BUS_COEF_SIDES + side;
}

/* The DSP that hosts @bus (default 0); DSP 0 for anything unusable */
static unsigned int ua_dsp_bus_owner(struct ua_device *ua, u32 bus)
{
	unsigned int owner = ua->dspx.bus_owner[bus];

	if (owner >= ua->num_dsps || !ua->dsp[owner].rings_allocated)
		return 0;
	return owner;
}

/*
 * Queue one {0x001D0004, bus, (side << 24) | param, value} on @dsp.  With
 * @wait the caller may sleep until the ring has a free slot (replay sends
 * hundreds of entries back to back); otherwise a full ring fails at once.
 */
static int ua_dsp_bus_cmd_send(struct ua_device *ua, unsigned int dsp,
			       u32 bus_id, u32 param, u32 side, u32 value,
			       bool wait)
{
	struct ua_dsp_state *ds = &ua->dsp[dsp];
	struct ua_ring_entry entry;
	int ret;

	if (!ds->rings_allocated)
		return -ENODEV;

	entry = (struct ua_ring_entry){
		.word0 = cpu_to_le32(UA_DSP_CMD_BUS_PARAM),
		.word1 = cpu_to_le32(bus_id),
		.word2 = cpu_to_le32((side << 24) | param),
		.word3 = cpu_to_le32(value),
	};

	if (wait) {
		ret = ua_dsp_ring_wait_space(ua, &ds->cmd, 1, 50);
		if (ret)
			return ret;
	}

	/*
	 * Circular ring: a slot is free again once the DSP has consumed
	 * it, so this only fails if the DSP has stopped consuming (1023
	 * entries outstanding) — not after ~1024 fader moves per boot.
	 */
	ret = ua_dsp_ring_put_entry(ua, &ds->cmd, &entry);
	if (ret)
		return ret;

	ua_dsp_ring_doorbell(ua, &ds->cmd,
			     ua_dsp_base(dsp) + UA_RING_CMD_OFFSET);
	return 0;
}

/**
 * ua_dsp_set_bus_param_flags - Send one bus coefficient entry to the DSP
 * @ua: device (caller must hold ua->lock)
 * @bus_id: input bus (0x00-0x1F)
 * @param: 0 CR, 1 HP1, 2 HP2, 3 Aux 1, 4 Aux 2, 5 HP3, 6 HP4
 * @side: 0 = left, 1 = right output weight
 * @value_u32: coefficient as IEEE 754 float packed in u32
 * @flags: UA_DSP_BUSP_SOLO_MUTE (honoured only with bus_solo_sign)
 *
 * One call is one entry of the kext's SetMixerBusParam, sent as
 * {0x001D0004, bus, (side << 24) | param, value}.  The Mixer Engine sends
 * two entries per bus and param (L and R), so a caller that wants pan or a
 * stereo send calls this twice.
 *
 * Where it goes (CUAD2DeviceMixer::SetMixerBusParam 0x3031e, parity/14 B4):
 * the DSP that hosts the bus, and also DSP 0 when that is another DSP.
 * Every bus is hosted by DSP 0 on the Apollo x4 (table default 0, nothing
 * moves an input yet), so one entry goes to DSP 0 exactly as before.
 * The kext additionally follows the span links of a spanned input (an
 * origin bus's coefficient is reused, and the partner is re-synced); that
 * needs the DSP strip engine's placement and is not implemented here.
 *
 * The request is cached per (bus, param, side) for
 * ua_dsp_replay_bus_params(), whether or not the ring accepted it.
 *
 * Returns 0 on success, negative errno on failure.
 */
int ua_dsp_set_bus_param_flags(struct ua_device *ua, u32 bus_id, u32 param,
			       u32 side, u32 value_u32, u32 flags)
{
	unsigned int owner;
	int ret, ret0 = 0;

	if (!ua->dsp[0].rings_allocated)
		return -ENODEV;

	if (bus_id > 0x1f || param > 6 || side > 1)
		return -EINVAL;

	if ((flags & UA_DSP_BUSP_SOLO_MUTE) && bus_solo_sign)
		value_u32 = ua_bus_solo_mute_value(value_u32);

	ua->dspx.bus_coef[bus_id][param][side] = value_u32;
	__set_bit(ua_bus_coef_cell(bus_id, param, side),
		  ua->dspx.bus_coef_valid);

	owner = ua_dsp_bus_owner(ua, bus_id);
	ret = ua_dsp_bus_cmd_send(ua, owner, bus_id, param, side, value_u32,
				  false);
	if (owner)
		ret0 = ua_dsp_bus_cmd_send(ua, 0, bus_id, param, side,
					   value_u32, false);
	if (ret || ret0) {
		struct ua_dsp_ring *ring = &ua->dsp[ret ? owner : 0].cmd;

		dev_warn_ratelimited(&ua->pdev->dev,
				     "bus_param ring full (bus=%u param=%u side=%u pos=%u wr=%u)\n",
				     bus_id, param, side, ring->hw_pos,
				     ring->write_ptr);
		return ret ? ret : ret0;
	}

	return 0;
}

int ua_dsp_set_bus_param(struct ua_device *ua, u32 bus_id,
			 u32 param, u32 side, u32 value_u32)
{
	return ua_dsp_set_bus_param_flags(ua, bus_id, param, side, value_u32,
					  0);
}

/**
 * ua_dsp_set_bus_owner - Record which DSP hosts a mixer bus
 * @ua: device (caller must hold ua->lock)
 * @bus_id: bus 0-31
 * @dsp_idx: hosting DSP; 0 is the default every bus starts with
 *
 * The kext's per-bus hosting table (this+0x1560[bus], changed by
 * MoveInputToDSP and spanning).  Nothing moves an input on the x4, so
 * nothing calls this yet; it is the hook for the strip engine.  Only
 * coefficients sent afterwards follow the new owner; call
 * ua_dsp_replay_bus_params() to re-send the cache.
 */
int ua_dsp_set_bus_owner(struct ua_device *ua, u32 bus_id, u32 dsp_idx)
{
	if (bus_id >= UA_BUS_COEF_BUSES || dsp_idx >= ua->num_dsps)
		return -EINVAL;
	ua->dspx.bus_owner[bus_id] = dsp_idx;
	return 0;
}

/**
 * ua_dsp_replay_bus_params - Re-send the cached mixer state to a fresh DSP
 * @ua: device (caller must hold ua->lock)
 *
 * After a DSP restart or a cold re-init the DSP holds the firmware's
 * defaults: bus coefficients zero, every bus enabled.  This re-sends what
 * the host had set: first the bus-enable commands for the buses that are
 * off, then every cached (bus, param, side) coefficient in index order,
 * each to its owner DSP (and DSP 0), exactly as ua_dsp_set_bus_param() did.
 * Sleeps when a ring is full.  Nothing is replayed that was never set, so
 * on a driver that has not seen a coefficient yet it sends nothing.
 *
 * Callers use ua_dsp_replay_if_pending(): it runs after the connect that
 * follows a DSP program load (ua_audio_connect(), the non-AudioExtension
 * connect in ua_dsp_init_and_load(), UA_IOCTL_DSP_CONNECT) and after a
 * successful ua_dsp_activate_plugin_chain(), whose captured default
 * coefficients would otherwise overwrite an earlier replay.
 *
 * Returns 0, or the first error (the replay continues past a failure).
 */
int ua_dsp_replay_bus_params(struct ua_device *ua)
{
	unsigned int cell, sent = 0, failed = 0, dsp;
	int err = 0, ret;
	u32 bus;

	lockdep_assert_held(&ua->lock);

	if (!ua->dsp[0].rings_allocated)
		return -ENODEV;

	/* Nothing was ever set: nothing to replay, nothing to log */
	if (bitmap_empty(ua->dspx.bus_coef_valid, UA_BUS_COEF_CELLS) &&
	    !(ua->bus_enable_known & ua->bus_disabled_mask & 0x7f))
		return 0;

	for (bus = 0; bus < 7; bus++) {
		if (!(ua->bus_enable_known & BIT(bus)) ||
		    !(ua->bus_disabled_mask & BIT(bus)))
			continue;
		for (dsp = 0; dsp < ua->num_dsps; dsp++) {
			if (!ua->dsp[dsp].rings_allocated)
				continue;
			ret = ua_dsp_ring_wait_space(ua, &ua->dsp[dsp].cmd, 1,
						     50);
			if (!ret)
				ret = ua_dsp_ring_send_raw(ua, dsp,
							   UA_DSP_CMD_BUS_ENABLE,
							   bus, 0, 0);
			if (ret) {
				failed++;
				err = err ? err : ret;
			} else {
				sent++;
			}
		}
	}

	for_each_set_bit(cell, ua->dspx.bus_coef_valid, UA_BUS_COEF_CELLS) {
		u32 side = cell % UA_BUS_COEF_SIDES;
		u32 param = (cell / UA_BUS_COEF_SIDES) % UA_BUS_COEF_PARAMS;
		u32 b = cell / (UA_BUS_COEF_SIDES * UA_BUS_COEF_PARAMS);
		u32 val = ua->dspx.bus_coef[b][param][side];
		unsigned int owner = ua_dsp_bus_owner(ua, b);

		ret = ua_dsp_bus_cmd_send(ua, owner, b, param, side, val,
					  true);
		if (!ret && owner)
			ret = ua_dsp_bus_cmd_send(ua, 0, b, param, side, val,
						  true);
		if (ret) {
			failed++;
			err = err ? err : ret;
		} else {
			sent++;
		}
	}

	ua->dspx.bus_replays++;
	dev_info(&ua->pdev->dev,
		 "bus coefficient replay: %u command(s) sent, %u failed\n",
		 sent, failed);
	return err;
}

/**
 * ua_dsp_replay_if_pending - Replay the mixer cache once after a DSP load
 * @ua: device (caller must hold ua->lock)
 * @why: short tag for the log
 *
 * ua_dsp_load_mixer_blocks() and ua_dsp_load_firmware() set replay_pending:
 * the DSP program was (re)started and holds the firmware defaults.  The
 * first call after that, from a point where the DSP rings are usable, sends
 * the cached bus coefficients and clears the flag.  Warm paths never set it
 * (the DSP kept its state), and a driver that has cached nothing sends
 * nothing, so a warm probe is untouched.  The flag is cleared even when the
 * cache is empty.
 */
int ua_dsp_replay_if_pending(struct ua_device *ua, const char *why)
{
	lockdep_assert_held(&ua->lock);

	if (!ua->dspx.replay_pending)
		return 0;
	if (!ua->dsp[0].rings_allocated)
		return -ENODEV;
	ua->dspx.replay_pending = false;
	dev_dbg(&ua->pdev->dev, "bus replay check after DSP load (%s)\n", why);
	return ua_dsp_replay_bus_params(ua);
}

/**
 * ua_dsp_ring_send_raw - Send an arbitrary 4-word inline entry to any DSP
 * @ua: device (caller must hold ua->lock)
 * @dsp_idx: target DSP (0-3)
 * @w0: first command word
 * @w1: second command word
 * @w2: third command word
 * @w3: fourth command word
 *
 * For testing ring buffer commands from userspace (UA_IOCTL_RING_SEND),
 * and the driver's own inline commands (bus enable, input params, rate
 * ack, connect).
 *
 * Inline entries only: a DMA_REF (word 0 bit 31) carries a bus address
 * the FPGA then reads.  The DMA_REFs the driver queues point at buffers
 * it owns and keeps alive until the ring has passed them (send_block,
 * sram_data, raw_dma, the plugin chain); one taken from user space
 * would point the device at whatever that address is.
 */
int ua_dsp_ring_send_raw(struct ua_device *ua, unsigned int dsp_idx,
			  u32 w0, u32 w1, u32 w2, u32 w3)
{
	struct ua_dsp_state *ds;
	u32 ring_base;
	struct ua_ring_entry entry;
	int ret;

	if (dsp_idx >= ua->num_dsps || (w0 & UA_RING_DMA_REF))
		return -EINVAL;

	ds = &ua->dsp[dsp_idx];
	if (!ds->rings_allocated)
		return -ENODEV;

	ring_base = ua_dsp_base(dsp_idx) + UA_RING_CMD_OFFSET;

	entry = (struct ua_ring_entry){
		.word0 = cpu_to_le32(w0),
		.word1 = cpu_to_le32(w1),
		.word2 = cpu_to_le32(w2),
		.word3 = cpu_to_le32(w3),
	};

	ret = ua_dsp_ring_put_entry(ua, &ds->cmd, &entry);
	if (ret)
		return ret;

	ua_dsp_ring_doorbell(ua, &ds->cmd, ring_base);
	return 0;
}

/**
 * ua_dsp_set_bus_enable - Enable/disable a DSP output bus on all DSPs
 * @ua: device (caller must hold ua->lock)
 * @bus_idx: bus group index (0-6)
 * @enable: 1=enable, 0=disable
 *
 * Kext RE: CUAD2DeviceMixer::SetBusEnable keeps a 7-entry cache that starts
 * at 1 (all buses enabled) and, only when the requested state differs,
 * sends {0x001C0003, bus_idx, enable, 0} to EVERY DSP.  Same here:
 * ua->bus_disabled_mask holds the buses that are off (zero = all on, the
 * kext's initial state), so enabling a bus that is already on sends
 * nothing.  The cache is updated only once the command reached a ring.
 */
int ua_dsp_set_bus_enable(struct ua_device *ua, u32 bus_idx, int enable)
{
	unsigned int dsp, sent = 0;
	int ret, err = 0;
	bool on = !!enable;

	if (bus_idx > 6)
		return -EINVAL;

	/*
	 * The kext's cache starts at "all enabled" because its mixer is
	 * created right after the DSPs boot.  Here a warm probe inherits a
	 * running mixer DSP whose buses the previous driver or daemon may
	 * have switched off, so the first request for each bus is always
	 * sent; only repeats are filtered.
	 */
	if ((ua->bus_enable_known & BIT(bus_idx)) &&
	    on == !(ua->bus_disabled_mask & BIT(bus_idx)))
		return 0;

	for (dsp = 0; dsp < ua->num_dsps; dsp++) {
		if (!ua->dsp[dsp].rings_allocated)
			continue;
		ret = ua_dsp_ring_send_raw(ua, dsp, UA_DSP_CMD_BUS_ENABLE,
					   bus_idx, on, 0);
		if (ret) {
			dev_warn(&ua->pdev->dev,
				 "bus_enable ring full (dsp=%u bus=%u)\n",
				 dsp, bus_idx);
			err = ret;
		} else {
			sent++;
		}
	}

	if (!err && sent) {
		ua->bus_enable_known |= BIT(bus_idx);
		if (on)
			ua->bus_disabled_mask &= ~BIT(bus_idx);
		else
			ua->bus_disabled_mask |= BIT(bus_idx);
	}

	return err;
}

/**
 * ua_dsp_get_bus_enable_mask - Bus enable state as the kext's GetBusEnabled
 * @ua: device
 *
 * Returns the mask of enabled buses, bit n = bus n (n < 7).
 */
u32 ua_dsp_get_bus_enable_mask(struct ua_device *ua)
{
	return ~ua->bus_disabled_mask & 0x7f;
}

/**
 * ua_dsp_set_input_param - DSP-side input parameter (ring opcode 0x1E)
 * @ua: device (caller must hold ua->lock)
 * @input: input index (0-31)
 * @sub: UA_DSP_INPUT_SCALE (5), _ROUTE (7) or _SPAN (8)
 * @value: float bits for the scale, route mask, or span info
 *
 * Kext CUAD2DeviceMixer::setDSPInputParam{Scale,DirectOutputRoute,
 * SpanInfo}: {0x001E0004, input, sub, value} to the DSP that hosts the
 * input.  Every input is hosted by DSP 0 here (no MoveInputToDSP).  A route
 * also goes to every other DSP as the "not routed here" default 0x1FFFF,
 * as the kext does for the DSPs that do not host the input.  The partner
 * link table the kext keeps for spanned inputs is not modelled.
 */
int ua_dsp_set_input_param(struct ua_device *ua, u32 input, u32 sub,
			   u32 value)
{
	unsigned int dsp;
	int ret;

	if (input > 31)
		return -EINVAL;
	if (sub != UA_DSP_INPUT_SCALE && sub != UA_DSP_INPUT_ROUTE &&
	    sub != UA_DSP_INPUT_SPAN)
		return -EINVAL;
	if (!ua->dsp[0].rings_allocated)
		return -ENODEV;

	ret = ua_dsp_ring_send_raw(ua, 0, UA_DSP_CMD_INPUT_PARAM, input, sub,
				   value);
	if (ret)
		return ret;

	if (sub == UA_DSP_INPUT_ROUTE) {
		for (dsp = 1; dsp < ua->num_dsps; dsp++) {
			if (!ua->dsp[dsp].rings_allocated)
				continue;
			ret = ua_dsp_ring_send_raw(ua, dsp,
						   UA_DSP_CMD_INPUT_PARAM,
						   input, sub,
						   UA_DSP_ROUTE_NONE);
			if (ret)
				return ret;
		}
	}

	return 0;
}

/**
 * ua_dsp_send_rate_ack - Tell every DSP the new sample rate
 * @ua: device (caller must hold ua->lock)
 * @rate: sample rate in Hz
 *
 * Kext RE: after a rate change the Mixer Engine's ack makes
 * CUAD2DeviceMixer::AckSampleRateChange queue {0x001A0002, rate_Hz, 0} on
 * every DSP of the device (the captured plugin chain only ever sends
 * 44100, to DSP 0).
 */
int ua_dsp_send_rate_ack(struct ua_device *ua, u32 rate)
{
	unsigned int dsp;
	int ret, err = 0;

	for (dsp = 0; dsp < ua->num_dsps; dsp++) {
		if (!ua->dsp[dsp].rings_allocated)
			continue;
		ret = ua_dsp_ring_send_raw(ua, dsp, 0x001A0002, rate, 0, 0);
		if (ret) {
			dev_warn(&ua->pdev->dev,
				 "rate ack ring full (dsp=%u)\n", dsp);
			err = ret;
		}
	}

	return err;
}

/**
 * ua_dsp_flush_bus_params - Ensure all queued bus params are processed
 * @ua: device
 *
 * Waits briefly for the DSP to consume queued ring entries.
 */
void ua_dsp_flush_bus_params(struct ua_device *ua)
{
	struct ua_dsp_ring *ring = &ua->dsp[0].cmd;

	if (!ua->dsp[0].rings_allocated)
		return;

	if (ua_dsp_ring_wait_idle(ua, ring, 10))
		return;
	dev_warn_ratelimited(&ua->pdev->dev,
			     "bus_param flush timeout: pos=%u wr=%u (not consumed)\n",
			     ring->hw_pos, ring->write_ptr);
}

/* ----------------------------------------------------------------
 * DSP Module Configuration (multichannel capture)
 *
 * Replays the Windows DSP 0 connect sequence that configures 6 mixer
 * modules in SHARC SRAM.  Without this, only type 0x00 capture channels
 * (direct preamp, Mic 1-2) work.  Type 0x01 channels (Mic 3-4, Virtual,
 * S/PDIF, AUX) need the DSP mixer routing configured here.
 *
 * Sequence per module:
 *   1. SRAM_CFG clears (header + I/O + coefficient blocks)
 *   2. DMA write module header (9 dwords)
 *   3. DMA write I/O descriptor (11-22 dwords)
 * Then:
 *   4. EnableSynchProcessing for modules 0 and 1
 *   5. RoutingEnable commands
 *
 * Data source: tools/captures/win-dsp0-ring-complete-20260319.json
 * ---------------------------------------------------------------- */

/**
 * ua_dsp_configure_modules - Configure DSP 0 mixer modules for multichannel
 * @ua: device (caller must hold ua->lock)
 *
 * Sends the SRAM configuration sequence captured from Windows to DSP 0.
 * Must be called after ACEFACE connect and ring buffer init, before
 * transport restart.  Bus coefficients from the mixer daemon will fill
 * the coefficient blocks afterward.
 *
 * Nothing calls this at present (no connect path sends the module
 * sequence); it is kept for the multichannel-capture experiments.
 *
 * Returns 0 on success, -EBUSY when the ring has no room for the
 * sequence, -ETIMEDOUT when the DSP did not consume it (its DMA page is
 * then parked, see ua_dsp_orphan_dma()), another negative errno on
 * failure.
 */
int ua_dsp_configure_modules(struct ua_device *ua)
{
	struct ua_dsp_state *ds = &ua->dsp[0];
	u32 bank_base, cmd_base;
	void *dma_page;
	dma_addr_t dma_addr;
	struct ua_ring_entry ent;
	const struct ua_module_addrs *mod;
	unsigned int i, j;
	int ret = 0;

	if (!ds->rings_allocated) {
		dev_warn(&ua->pdev->dev, "module_cfg: DSP 0 rings not ready\n");
		return -ENODEV;
	}

	if (ua_dsp_orphan_room(ua))
		return -EBUSY;

	bank_base = ua_dsp_base(0);
	cmd_base = bank_base + UA_RING_CMD_OFFSET;

	/*
	 * Allocate a single DMA page for all module payloads.
	 * Layout: a zero slot at offset 0 (the source of the header-area
	 * clears below), then 6 modules × (header at +0x00, I/O desc at
	 * +0x40) in 256-byte slots, and the extended wiring block at the
	 * end of the page.
	 */
	dma_page = dma_alloc_coherent(&ua->pdev->dev, UA_PCIE_PAGE_SIZE,
				      &dma_addr, GFP_KERNEL);
	if (!dma_page)
		return -ENOMEM;
	memset(dma_page, 0, UA_PCIE_PAGE_SIZE);

	/*
	 * Build DMA payloads for all 6 modules.
	 * Slot layout per module (256 bytes each):
	 *   +0x00: 9-dword header (36 bytes)
	 *   +0x40: I/O descriptor (up to 88 bytes = 22 dwords)
	 *
	 * The slot was 128 bytes: module 0's 22-dword descriptor at +0x40
	 * runs to +0x98 (152), so module 1's header overwrote its last six
	 * words (the biquad coefficients) before the page was sent.  The
	 * checks below keep every template inside its slot, the slots off
	 * the extended wiring block, and the zero-source reads inside the
	 * zero slot.
	 */
#define MOD_SLOT_SIZE	256
#define MOD_HDR_OFF	0x00
#define MOD_IO_OFF	0x40
	BUILD_BUG_ON(MOD_HDR_OFF + sizeof(ua_mod_hdr_template) > MOD_IO_OFF);
	BUILD_BUG_ON(MOD_IO_OFF + sizeof(ua_mod0_io_template) > MOD_SLOT_SIZE);
	BUILD_BUG_ON(MOD_IO_OFF + sizeof(ua_mod_code_io_template) >
		     MOD_SLOT_SIZE);
	BUILD_BUG_ON(MOD_IO_OFF + sizeof(ua_mod_passthru_io_template) >
		     MOD_SLOT_SIZE);
	BUILD_BUG_ON(MOD_IO_OFF + sizeof(ua_mod5_io_template) > MOD_SLOT_SIZE);
	BUILD_BUG_ON((UA_MODULE_COUNT + 1) * MOD_SLOT_SIZE >
		     UA_PCIE_PAGE_SIZE - sizeof(ua_mod_extended_wiring));
	BUILD_BUG_ON(15 * sizeof(u32) > MOD_SLOT_SIZE);	/* zero-source reads */

	for (i = 0; i < UA_MODULE_COUNT; i++) {
		u32 *hdr, *io;

		mod = &ua_x4_modules[i];
		hdr = (u32 *)((u8 *)dma_page + (i + 1) * MOD_SLOT_SIZE + MOD_HDR_OFF);
		io  = (u32 *)((u8 *)dma_page + (i + 1) * MOD_SLOT_SIZE + MOD_IO_OFF);

		/* Build header (9 words) */
		memcpy(hdr, ua_mod_hdr_template, 9 * 4);
		hdr[1] = mod->sram_hdr;  /* self-referencing address */

		if (i == 0) {
			hdr[8] = UA_MOD_EP_MIXER;
		} else {
			hdr[8] = UA_MOD_EP_ROUTING;
		}

		/* Module 5: 2 inputs, 2 outputs */
		if (i == 5) {
			hdr[4] = 2;
			hdr[5] = 2;
		}

		/* Build I/O descriptor */
		if (i == 0) {
			memcpy(io, ua_mod0_io_template, sizeof(ua_mod0_io_template));

			io[1] = mod->sram_io;
		} else if (i <= 2) {
			memcpy(io, ua_mod_code_io_template, sizeof(ua_mod_code_io_template));

			io[1] = mod->sram_io;
		} else if (i <= 4) {
			/* Passthrough: self-addr is sram_io + 8 (Windows capture) */
			memcpy(io, ua_mod_passthru_io_template, sizeof(ua_mod_passthru_io_template));

			io[1] = mod->sram_io + 8;
		} else {
			memcpy(io, ua_mod5_io_template, sizeof(ua_mod5_io_template));

			io[1] = mod->sram_io;
		}
	}

	/*
	 * Verify ring space: extended wiring + 6 modules × ~4 entries +
	 * EnableSynch + RoutingEnable + bus coefficients.
	 */
	if (ua_dsp_ring_wait_space(ua, &ds->cmd, 120, 1000)) {
		dev_warn(&ua->pdev->dev,
			 "module_cfg: ring too full (pos=%u wr=%u cap=%u)\n",
			 ds->cmd.hw_pos, ds->cmd.write_ptr, ds->cmd.capacity);
		ret = -EBUSY;
		goto out_free;
	}

	/*
	 * Phase 0: Send extended IO wiring block (DMA[152]).
	 * Contains cross-module references, preamp ADC routing,
	 * and biquad coefficients. Must go BEFORE per-module writes.
	 */
	{
		dma_addr_t ext_dma;
		u32 *ext_buf;

		/* Place extended wiring at end of DMA page */
		ext_buf = (u32 *)((u8 *)dma_page + UA_PCIE_PAGE_SIZE -
				  UA_MOD_EXTENDED_WORDS * 4);
		memcpy(ext_buf, ua_mod_extended_wiring,
		       UA_MOD_EXTENDED_WORDS * 4);

		ext_dma = dma_addr + UA_PCIE_PAGE_SIZE -
			  UA_MOD_EXTENDED_WORDS * 4;

		ent = (struct ua_ring_entry){
			.word0 = cpu_to_le32(UA_RING_DMA_REF |
					     UA_MOD_EXTENDED_WORDS),
			.word1 = 0,
			.word2 = cpu_to_le32(lower_32_bits(ext_dma)),
			.word3 = cpu_to_le32(upper_32_bits(ext_dma)),
		};
		ua_dsp_ring_put_entry(ua, &ds->cmd, &ent);
	}

	/* Phase 1: DMA writes for each module (no SRAM clears). */
	for (i = 0; i < UA_MODULE_COUNT; i++) {
		dma_addr_t hdr_dma, io_dma;
		unsigned int io_dma_words;

		mod = &ua_x4_modules[i];

		/*
		 * General SRAM clears REMOVED — they destroy firmware's
		 * default mixer output routing on cold boot.
		 *
		 * EXCEPTION: Module 0 talkback aux SRAM at 0xC6FF6 (8dw)
		 * and extra params at 0x9D18E (4dw) must be cleared.
		 * These are at unique addresses that don't overlap capture
		 * routing. Without them, talkback DMA channels return zeros.
		 * (Windows entries 155-156)
		 */
		if (i == 0) {
			ent = (struct ua_ring_entry){
				.word0 = cpu_to_le32(0x00080004),
				.word1 = cpu_to_le32(0x000C6FF6),
				.word2 = 0,
				.word3 = cpu_to_le32(8),
			};
			ua_dsp_ring_put_entry(ua, &ds->cmd, &ent);

			ent = (struct ua_ring_entry){
				.word0 = cpu_to_le32(0x00080004),
				.word1 = cpu_to_le32(0x0009D18E),
				.word2 = 0,
				.word3 = cpu_to_le32(4),
			};
			ua_dsp_ring_put_entry(ua, &ds->cmd, &ent);
		}

		/* DMA: write zeros to header area (matches Windows entry) */
		ent = (struct ua_ring_entry){
			.word0 = cpu_to_le32(UA_RING_DMA_REF | (i == 0 ? 15 : 11)),
			.word1 = 0,
			.word2 = cpu_to_le32(lower_32_bits(dma_addr)),  /* page start = zeros */
			.word3 = cpu_to_le32(upper_32_bits(dma_addr)),
		};
		ua_dsp_ring_put_entry(ua, &ds->cmd, &ent);

		/* DMA: write module header (9 dwords) */
		hdr_dma = dma_addr + (i + 1) * MOD_SLOT_SIZE + MOD_HDR_OFF;
		ent = (struct ua_ring_entry){
			.word0 = cpu_to_le32(UA_RING_DMA_REF | 9),
			.word1 = 0,
			.word2 = cpu_to_le32(lower_32_bits(hdr_dma)),
			.word3 = cpu_to_le32(upper_32_bits(hdr_dma)),
		};
		ua_dsp_ring_put_entry(ua, &ds->cmd, &ent);

		/* DMA: write I/O descriptor */
		io_dma = dma_addr + (i + 1) * MOD_SLOT_SIZE + MOD_IO_OFF;
		io_dma_words = (i == 0) ? 22 : 11;
		ent = (struct ua_ring_entry){
			.word0 = cpu_to_le32(UA_RING_DMA_REF | io_dma_words),
			.word1 = 0,
			.word2 = cpu_to_le32(lower_32_bits(io_dma)),
			.word3 = cpu_to_le32(upper_32_bits(io_dma)),
		};
		ua_dsp_ring_put_entry(ua, &ds->cmd, &ent);
	}

	/*
	 * Phase 2: EnableSynchProcessing — activate modules 1 then 0.
	 */
	for (i = 0; i < UA_SYNCH_COUNT; i++) {
		const struct ua_synch_target *st = &ua_x4_synch[i];

		ent = (struct ua_ring_entry){
			.word0 = cpu_to_le32(UA_ENABLE_SYNCH),
			.word1 = cpu_to_le32(st->module_ref),
			.word2 = cpu_to_le32(ua_x4_modules[st->module_idx].sram_hdr),
			.word3 = 0,
		};
		ua_dsp_ring_put_entry(ua, &ds->cmd, &ent);

		/*
		 * RoutingEnable commands follow each EnableSynch.
		 * Module 1 (slot 0x1B): 3 routing cmds (indices 0-2)
		 * Module 0 (slot 0x0A): 3 routing cmds (indices 3-5)
		 */
		for (j = i * 3; j < i * 3 + 3 && j < UA_ROUTING_COUNT; j++) {
			const struct ua_routing_cmd *rc = &ua_x4_routing[j];

			ent = (struct ua_ring_entry){
				.word0 = cpu_to_le32(UA_ROUTING_ENABLE),
				.word1 = cpu_to_le32(rc->slot),
				.word2 = cpu_to_le32(rc->sub_param),
				.word3 = cpu_to_le32(rc->mask),
			};
			ua_dsp_ring_put_entry(ua, &ds->cmd, &ent);
		}
	}

	/*
	 * Phase 3: Initial bus coefficients for analog inputs.
	 *
	 * Windows sends stereo bus coefficients right after routing enables.
	 * Format: {0x001D0004, bus_id, (ch<<24)|sub_param, value_f32}
	 * ch=0 for L, ch=1 for R (0x01000000).
	 *
	 * Set unity gain (0.707 = -3dB = 0x3F3504F3) on main mix (sub=0)
	 * and gain L/R (sub=3,4) for buses 0-3 (Analog In 1-4).
	 * The daemon will override these with actual fader values.
	 */
	{
		static const u32 unity = 0x3F3504F3;  /* float 0.707 */
		unsigned int bus;

		for (bus = 0; bus < 4; bus++) {
			/* RoutingEnable for this bus first */
			ent = (struct ua_ring_entry){
				.word0 = cpu_to_le32(UA_ROUTING_ENABLE),
				.word1 = cpu_to_le32(bus),
				.word2 = cpu_to_le32(0x08),
				.word3 = cpu_to_le32(0x20),
			};
			ua_dsp_ring_put_entry(ua, &ds->cmd, &ent);

			/* Main mix L + R (sub=0) */
			ent = (struct ua_ring_entry){
				.word0 = cpu_to_le32(0x001D0004),
				.word1 = cpu_to_le32(bus),
				.word2 = cpu_to_le32(0x00000000),
				.word3 = cpu_to_le32(unity),
			};
			ua_dsp_ring_put_entry(ua, &ds->cmd, &ent);
			ent.word2 = cpu_to_le32(0x01000000);  /* ch=1 (R) */
			ua_dsp_ring_put_entry(ua, &ds->cmd, &ent);

			/* Gain L (sub=3) */
			ent.word2 = cpu_to_le32(0x00000003);
			ua_dsp_ring_put_entry(ua, &ds->cmd, &ent);
			ent.word2 = cpu_to_le32(0x01000003);
			ua_dsp_ring_put_entry(ua, &ds->cmd, &ent);

			/* Gain R (sub=4) */
			ent.word2 = cpu_to_le32(0x00000004);
			ent.word3 = 0;  /* zero for R gain on L bus */
			ua_dsp_ring_put_entry(ua, &ds->cmd, &ent);
			ent.word2 = cpu_to_le32(0x01000004);
			ua_dsp_ring_put_entry(ua, &ds->cmd, &ent);

			/* Channel mask routing (sub=7) */
			ent = (struct ua_ring_entry){
				.word0 = cpu_to_le32(UA_ROUTING_ENABLE),
				.word1 = cpu_to_le32(bus),
				.word2 = cpu_to_le32(0x07),
				.word3 = cpu_to_le32(0x0001FFFF),
			};
			ua_dsp_ring_put_entry(ua, &ds->cmd, &ent);
		}
	}

	/* Ring the doorbell once for the entire batch */
	ua_dsp_ring_doorbell(ua, &ds->cmd, cmd_base);

	dev_dbg(&ua->pdev->dev,
		"module_cfg: queued %u entries for %u modules\n",
		ds->cmd.write_ptr, UA_MODULE_COUNT);

	/* Wait for DSP to process all entries */
	if (!ua_dsp_ring_wait_idle(ua, &ds->cmd, UA_DSP_CMDBUF_WAIT_MS)) {
		dev_warn(&ua->pdev->dev,
			 "module_cfg: timeout (pos=%u target=%u)\n",
			 ds->cmd.hw_pos, ds->cmd.write_ptr);
		/* DMA refs into dma_page may still be fetched */
		ua_dsp_orphan_dma(ua, dma_page, dma_addr, UA_PCIE_PAGE_SIZE,
				  &ds->cmd, NULL);
		return -ETIMEDOUT;
	}
	dev_dbg(&ua->pdev->dev,
		"module_cfg: DSP consumed all entries (pos=%u)\n",
		ds->cmd.hw_pos);

out_free:
	dma_free_coherent(&ua->pdev->dev, UA_PCIE_PAGE_SIZE,
			  dma_page, dma_addr);
	return ret;
}

/* ----------------------------------------------------------------
 * IO Descriptor SRAM Configuration
 *
 * The IO descriptor regions at 0xC1A4 (input) and 0xC2C4 (output)
 * tell the FPGA/DSP how to map DMA channels to audio signals.
 *
 * macOS format (BAR0 dump captured 2026-03-18):
 *   6-word header + packed 16-bit channel pairs + 0x00FF00FF padding
 *
 * Linux was writing raw 32-bit channel IDs with no header — wrong
 * format.  This caused capture to return all zeros despite DMA
 * working correctly at the FPGA level.
 *
 * The firmware writes these areas; the kext only reads them
 * (_handleNotificationInterrupt) and never sends routing to the DSP:
 * SetDriverIoRoutings (SEL171) just stores the host-side channel map.
 * Writing a captured descriptor over the firmware's is a Linux-only
 * experiment kept for the legacy layout (UA_IOCTL_SEND_ROUTING; nothing
 * in userspace calls it).  With the descriptor layout (ua_io_map_enabled())
 * it is a no-op so the descriptor the driver maps by stays the firmware's.
 * ---------------------------------------------------------------- */

#include "ua_routing.h"

/**
 * ua_dsp_send_routing - Write captured IO descriptors over the firmware's
 * @ua: device (must hold ua->lock)
 *
 * Writes the macOS-captured IO descriptor data (x4, x8p) to the SRAM
 * regions at 0xC1A4 (input) and 0xC2C4 (output).  No-op with the
 * descriptor layout.
 *
 * Returns 0 on success, negative errno on failure.
 */
int ua_dsp_send_routing(struct ua_device *ua)
{
	const struct ua_routing_config *cfg;
	unsigned int i;

	if (ua_io_map_enabled(ua)) {
		dev_dbg(&ua->pdev->dev,
			"descriptor layout: not writing captured IO descriptors (the firmware owns them)\n");
		return 0;
	}

	cfg = ua_get_routing_config(ua->device_type);
	if (!cfg) {
		dev_warn(&ua->pdev->dev,
			 "no routing config for device type 0x%02x\n",
			 ua->device_type);
		return -ENODATA;
	}

	if (!cfg->io_desc_input || !cfg->io_desc_output) {
		dev_warn(&ua->pdev->dev,
			 "no IO descriptor data for device type 0x%02x\n",
			 ua->device_type);
		return -ENODATA;
	}

	/* Write input IO descriptors (72 words at 0xC1A4) */
	for (i = 0; i < UA_IO_DESC_WORDS; i++)
		ua_write(ua, UA_REG_IN_NAMES_BASE + i * 4,
			 cfg->io_desc_input[i]);

	/* Write output IO descriptors (72 words at 0xC2C4) */
	for (i = 0; i < UA_IO_DESC_WORDS; i++)
		ua_write(ua, UA_REG_OUT_NAMES_BASE + i * 4,
			 cfg->io_desc_output[i]);

	dev_dbg(&ua->pdev->dev,
		"IO descriptors written: input=%u words, output=%u words\n",
		UA_IO_DESC_WORDS, UA_IO_DESC_WORDS);

	return 0;
}

/* ----------------------------------------------------------------
 * DSP message FIFO and DSP interrupt vectors (parity/03 A07, I01-I05, I09,
 * I10; parity/13 section 5.3)
 *
 * Each DSP raises five vectors: 5n+0 command ring consumed, 5n+1 response
 * ring filled (both one-shot: the kext's filter masks them as they fire and
 * the ring service re-enables them), 5n+2 message FIFO, 5n+3 / 5n+4 error A
 * and B.  With more than four DSPs the map is 4n+j and error B does not
 * exist (I09).  Linux polls the rings through POSITION and never enabled any
 * of them; dsp_msg_irq enables the FIFO and error vectors (1) and, for
 * diagnostics, also the two ring vectors with the one-shot re-arm (2).  The
 * handler only counts and queues work; the rings are still polled exactly
 * as before and nothing here touches a ring or its response buffers.
 *
 * The FIFO is read-only for the host: the DSP publishes its write index at
 * window+0x40 (values above 0xF are stale) and 16 slots of five dwords from
 * window+0x44.  The kext keeps its own read index (CPcieDSP+0xD8), copies
 * slots from there up to the write index and writes nothing back
 * (_serviceMessageFIFO 0x93de, disassembled).  The index starts at the DSP's
 * current write index here, so messages from before the driver armed the
 * vectors are not replayed.
 * ---------------------------------------------------------------- */

#define UA_DSP_WIN_FIFO_IDX	0x40
#define UA_DSP_WIN_FIFO_MSG	0x44
#define UA_DSP_FIFO_SLOTS	16
#define UA_DSP_FIFO_WORDS	5
#define UA_DSP_STORM_LIMIT	10000	/* DSP vectors per second */

static int dsp_msg_irq;
module_param(dsp_msg_irq, int, 0444);
MODULE_PARM_DESC(dsp_msg_irq,
		 "EXPERIMENTAL (parity/03 E1), 0 = off (default, no DSP vector is enabled). 1 = enable each DSP's message-FIFO and error vectors; the handler counts them and a work item reads the FIFO (read only) and logs messages. 2 = also the command/response ring vectors, counted only, with the kext's one-shot re-arm. The rings stay polled either way. Counters are in /proc/asound/cardN/dsp_state");

/* Hardware ISR/IMR bit of DSP @dsp's vector 5n+@j, or -1 */
static int ua_dsp_vec_hwbit(const struct ua_device *ua, unsigned int dsp,
			    unsigned int j)
{
	if (dsp >= UA_MAX_DSPS || j > 4)
		return -1;
	if (ua->num_dsps > 4)
		return j == 4 ? -1 : (int)(dsp * 4 + j);
	return dsp * 5 + j;
}

/**
 * ua_dsp_irq_arm - Enable the DSP vectors selected by dsp_msg_irq
 * @ua: device (ua->lock held, or probe)
 *
 * No effect with dsp_msg_irq = 0.  Safe to repeat; call it again after
 * anything that masks every vector (ua_irq_mask_all), as the kext enables
 * these in CPcieDSP::Initialize with clearPending = 1 (R01).  Done after
 * the rings are programmed, from ua_dsp_rings_init() and
 * ua_dsp_connect_all().
 */
void ua_dsp_irq_arm(struct ua_device *ua)
{
	struct ua_dsp_ext *x = &ua->dspx;
	unsigned int dsp, j, first;
	unsigned long flags;
	u32 bits = 0;

	if (!dsp_msg_irq || !x->inited || atomic_read(&ua->shutdown))
		return;

	first = dsp_msg_irq >= 2 ? 0 : 2;
	for (dsp = 0; dsp < ua->num_dsps; dsp++) {
		u32 widx;

		if (!ua->dsp[dsp].rings_allocated)
			continue;
		widx = ua_read(ua, ua_dsp_window(dsp) + UA_DSP_WIN_FIFO_IDX);
		x->msg_rd[dsp] = widx & (UA_DSP_FIFO_SLOTS - 1);
		for (j = first; j < 5; j++) {
			int hw = ua_dsp_vec_hwbit(ua, dsp, j);

			if (hw >= 0)
				bits |= BIT(hw);
		}
	}
	if (!bits)
		return;

	spin_lock_irqsave(&ua->irq_lock, flags);
	x->vec_mask = bits;
	x->msg_armed = true;
	x->storm_tripped = false;
	x->storm_count = 0;
	x->storm_window = jiffies;
	ua->irq_mask_lo |= bits;
	ua_write(ua, UA_REG_IRQ_STATUS, bits);		/* clearPending */
	ua_write(ua, UA_REG_IRQ_ENABLE, ua->irq_mask_lo);
	spin_unlock_irqrestore(&ua->irq_lock, flags);
	(void)ua_read(ua, UA_REG_FPGA_REV);		/* posted-write flush */

	dev_info(&ua->pdev->dev,
		 "DSP vectors armed (dsp_msg_irq=%d, IMR bits 0x%08x)\n",
		 dsp_msg_irq, bits);
}

/* Mask our vectors and stop the work; the caller may sleep, not hold irq_lock */
static void ua_dsp_irq_disarm(struct ua_device *ua)
{
	struct ua_dsp_ext *x = &ua->dspx;
	unsigned long flags;

	if (!x->inited)
		return;

	spin_lock_irqsave(&ua->irq_lock, flags);
	x->msg_armed = false;
	if (x->vec_mask) {
		ua->irq_mask_lo &= ~x->vec_mask;
		ua_write(ua, UA_REG_IRQ_ENABLE, ua->irq_mask_lo);
		x->vec_mask = 0;
	}
	spin_unlock_irqrestore(&ua->irq_lock, flags);
	cancel_work_sync(&x->msg_work);
}

/**
 * ua_dsp_irq - DSP vector dispatch, called from the main ISR
 * @ua: device
 * @status_lo: ISR bits 0-31 already masked with IMR and acknowledged
 * @status_hi: ISR_HI, unused (every DSP vector is in the low word)
 *
 * Hard-IRQ context.  Counts the fired vectors, auto-masks the one-shot
 * ring vectors the way CPcieIntrManager::ServiceInterrupt does (I10),
 * and defers all register reads to ua_dsp_msg_work().  Returns at once
 * with dsp_msg_irq = 0, where none of these bits is ever unmasked.
 */
void ua_dsp_irq(struct ua_device *ua, u32 status_lo, u32 status_hi)
{
	struct ua_dsp_ext *x = &ua->dspx;
	unsigned long fifo = 0, err = 0, rearm = 0, flags;
	u32 oneshot = 0;
	unsigned int dsp, j;

	if (!dsp_msg_irq || !status_lo || !x->msg_armed)
		return;

	for (dsp = 0; dsp < ua->num_dsps; dsp++) {
		for (j = 0; j < 5; j++) {
			int hw = ua_dsp_vec_hwbit(ua, dsp, j);

			if (hw < 0 || !(status_lo & BIT(hw)))
				continue;
			atomic_inc(&x->vec_count[dsp][j]);
			switch (j) {
			case 0:
			case 1:
				oneshot |= BIT(hw);
				rearm |= BIT(dsp * 2 + j);
				break;
			case 2:
				fifo |= BIT(dsp);
				break;
			default:
				err |= BIT(dsp * 2 + (j - 3));
				break;
			}
		}
	}
	if (!fifo && !err && !rearm)
		return;

	spin_lock_irqsave(&ua->irq_lock, flags);
	if (!x->msg_armed) {
		spin_unlock_irqrestore(&ua->irq_lock, flags);
		return;
	}

	if (time_after(jiffies, x->storm_window + HZ)) {
		x->storm_window = jiffies;
		x->storm_count = 0;
	}
	if (++x->storm_count > UA_DSP_STORM_LIMIT) {
		/* Storm guard: give the vectors up until the next arm */
		ua->irq_mask_lo &= ~x->vec_mask;
		ua_write(ua, UA_REG_IRQ_ENABLE, ua->irq_mask_lo);
		x->vec_mask = 0;
		x->msg_armed = false;
		x->storm_tripped = true;
		schedule_work(&x->msg_work);
		spin_unlock_irqrestore(&ua->irq_lock, flags);
		return;
	}

	if (oneshot) {
		/* Re-enabled by the work item once the event is counted */
		ua->irq_mask_lo &= ~oneshot;
		ua_write(ua, UA_REG_IRQ_ENABLE, ua->irq_mask_lo);
	}
	x->pend_fifo |= fifo;
	x->pend_err |= err;
	x->pend_rearm |= rearm;
	schedule_work(&x->msg_work);
	spin_unlock_irqrestore(&ua->irq_lock, flags);
}

/* Read the new messages of one DSP's FIFO (CPcieDSP::_serviceMessageFIFO) */
static void ua_dsp_fifo_drain(struct ua_device *ua, unsigned int dsp)
{
	struct ua_dsp_ext *x = &ua->dspx;
	u32 win = ua_dsp_window(dsp);
	unsigned int rd = x->msg_rd[dsp], n = 0, i;
	u32 widx = ua_read(ua, win + UA_DSP_WIN_FIFO_IDX);

	if (widx == 0xFFFFFFFF)
		return;
	if (widx >= UA_DSP_FIFO_SLOTS) {
		x->msg_stale[dsp]++;
		return;
	}

	while (rd != widx && n < UA_DSP_FIFO_SLOTS) {
		u32 base = win + UA_DSP_WIN_FIFO_MSG + rd * UA_DSP_FIFO_WORDS * 4;
		u32 w[UA_DSP_FIFO_WORDS];

		for (i = 0; i < UA_DSP_FIFO_WORDS; i++) {
			w[i] = ua_read(ua, base + i * 4);
			x->msg_last[dsp][i] = w[i];
		}
		x->msg_total[dsp]++;
		if ((w[2] & 0xffff0000u) == 0xf0010000u)
			x->msg_f001[dsp]++;
		else if ((w[2] & 0xffff0000u) == 0xf0020000u)
			x->msg_f002[dsp]++;
		dev_info_ratelimited(&ua->pdev->dev,
				     "DSP %u message: %08x %08x class %08x arg %08x %08x\n",
				     dsp, w[0], w[1], w[2], w[3], w[4]);
		rd = (rd + 1) & (UA_DSP_FIFO_SLOTS - 1);
		n++;
	}
	x->msg_rd[dsp] = rd;
}

static void ua_dsp_msg_work(struct work_struct *work)
{
	struct ua_device *ua = container_of(work, struct ua_device,
					    dspx.msg_work);
	struct ua_dsp_ext *x = &ua->dspx;
	unsigned long fifo, err, rearm;
	bool tripped;
	unsigned int dsp, j;

	spin_lock_irq(&ua->irq_lock);
	fifo = x->pend_fifo;
	err = x->pend_err;
	rearm = x->pend_rearm;
	x->pend_fifo = x->pend_err = x->pend_rearm = 0;
	tripped = x->storm_tripped;
	spin_unlock_irq(&ua->irq_lock);

	if (atomic_read(&ua->shutdown))
		return;

	if (tripped)
		dev_warn(&ua->pdev->dev,
			 "DSP vectors fired over %u/s, masked until the next arm (dsp_msg_irq=%d)\n",
			 UA_DSP_STORM_LIMIT, dsp_msg_irq);

	for (dsp = 0; dsp < ua->num_dsps; dsp++) {
		if (fifo & BIT(dsp))
			ua_dsp_fifo_drain(ua, dsp);
		if (err & (BIT(dsp * 2) | BIT(dsp * 2 + 1))) {
			ua_err_count(ua, UA_ERR_DSP_VECTOR);
			dev_warn_ratelimited(&ua->pdev->dev,
					     "DSP %u error vector%s%s fired (DSPEvent 7/8)\n",
					     dsp,
					     err & BIT(dsp * 2) ? " A" : "",
					     err & BIT(dsp * 2 + 1) ? " B" : "");
		}
	}

	if (!rearm)
		return;

	/* One-shot re-arm, only while the FIFO vectors are still enabled */
	spin_lock_irq(&ua->irq_lock);
	for (dsp = 0; dsp < ua->num_dsps; dsp++) {
		int fhw = ua_dsp_vec_hwbit(ua, dsp, 2);

		if (fhw < 0 || !(ua->irq_mask_lo & BIT(fhw)))
			continue;
		for (j = 0; j < 2; j++) {
			int hw = ua_dsp_vec_hwbit(ua, dsp, j);

			if (hw >= 0 && (rearm & BIT(dsp * 2 + j)) &&
			    (x->vec_mask & BIT(hw)))
				ua->irq_mask_lo |= BIT(hw);
		}
	}
	ua_write(ua, UA_REG_IRQ_ENABLE, ua->irq_mask_lo);
	spin_unlock_irq(&ua->irq_lock);
}

/* ----------------------------------------------------------------
 * kext_connect: StartTransport notification and date-only disconnect
 * (parity/13 6.2, R83, R84, H04, H05)
 * ---------------------------------------------------------------- */

/*
 * GetProperty(0xC) of the kext's DSP: this device uses ring connect.  For
 * the types in the connect mask it is 1; the kext asks GetDeviceInfo for
 * every other type, which Linux cannot derive, so an unresolved public type
 * keeps the old behaviour (connect) and a resolved one outside the mask
 * is treated as 0.
 */
static bool ua_dsp_prop_ring_connect(const struct ua_device *ua)
{
	if (ua->public_type == UA_PUBTYPE_NONE)
		return true;
	return ua_public_ring_connect(ua->public_type);
}

/**
 * ua_dsp_start_transport_notify - CPcieDevice::StartTransport's ring part
 * @ua: device (ua->lock held)
 *
 * With kext_connect, after the DSPs are connected: {0x00230002, 0} to every
 * DSP that has property 0xC, then the date word {0x00100002, date} to DSP 0
 * (_connectToDsps(1, 0), kext 0x5bf4).  Nothing is waited for.  No effect
 * without kext_connect or before ua_dsp_connect_all() has run.  The second
 * argument's bit 0 that gates it in the kext ("first start of the bus",
 * parity/13 open question 2) is not modelled: call this once per transport
 * start, after the AX_CONTROL start write (not wired up yet; ua_audio.c).
 */
int ua_dsp_start_transport_notify(struct ua_device *ua)
{
	unsigned int dsp;
	int ret, err = 0;

	if (!ua_kext_connect || !ua->dsps_connected ||
	    !ua_dsp_prop_ring_connect(ua))
		return 0;

	for (dsp = 0; dsp < ua->num_dsps; dsp++) {
		if (!ua->dsp[dsp].rings_allocated)
			continue;
		ret = ua_dsp_ring_send_raw(ua, dsp, UA_DSP_CMD_CONNECT, 0, 0,
					   0);
		if (ret)
			err = err ? err : ret;
	}
	if (ua->dsp[0].rings_allocated) {
		ret = ua_dsp_ring_send_raw(ua, 0, UA_DSP_CMD_FINALIZE,
					   ua_dsp_connect_date(), 0, 0);
		if (ret)
			err = err ? err : ret;
	}
	dev_dbg(&ua->pdev->dev, "kext_connect: StartTransport notified (%d)\n",
		err);
	return err;
}

/**
 * ua_dsp_disconnect_notify - _connectToDsps(0, flag): the date word only
 * @ua: device (ua->lock held)
 *
 * With kext_connect, once DSPs are connected: a disconnect sends nothing but
 * {0x00100002, date} to DSP 0.  Called from ua_dsp_rings_quiesce(), i.e. at
 * shutdown, suspend and remove, before the rings are emptied.
 */
int ua_dsp_disconnect_notify(struct ua_device *ua)
{
	int ret;

	if (!ua_kext_connect || !ua->dsps_connected ||
	    !ua->dsp[0].rings_allocated)
		return 0;

	ret = ua_dsp_ring_send_raw(ua, 0, UA_DSP_CMD_FINALIZE,
				   ua_dsp_connect_date(), 0, 0);
	dev_dbg(&ua->pdev->dev, "kext_connect: date-only disconnect (%d)\n",
		ret);
	return ret;
}

/* ----------------------------------------------------------------
 * /proc/asound/cardN/dsp_state (parity/03 E6): read-only DSP window dump
 * ---------------------------------------------------------------- */

static const struct {
	u32 off;
	const char *name;
} ua_dsp_win_regs[] = {
	{ 0x000, "status (A01)" },
	{ 0x00c, "property 0 (A03)" },
	{ 0x010, "pool size 0 code (A04)" },
	{ 0x014, "pool size 2 (A04)" },
	{ 0x018, "pool size 1 (A04)" },
	{ 0x01c, "pool size 3 (A04)" },
	{ 0x184, "pool base 0 (A08)" },
	{ 0x188, "pool base 2 (A08)" },
	{ 0x18c, "pool base 1 (A08)" },
	{ 0x190, "pool base 3 (A08)" },
	{ 0x194, "scratch 2 (A08)" },
	{ 0x198, "scratch 1 (A08)" },
	{ 0x19c, "scratch 3 (A08)" },
	{ 0x1a0, "property 7 (A09)" },
	{ 0x1a4, "boot word (A10)" },
	{ 0x1a8, "reset slot (A11)" },
	{ 0x1ac, "device info (A12)" },
};

static void ua_dsp_state_proc_read(struct snd_info_entry *entry,
				   struct snd_info_buffer *buf)
{
	struct ua_device *ua = entry->private_data;
	struct ua_dsp_ext *x = &ua->dspx;
	unsigned int dsp, i, j;
	u32 ext_caps, cap, lo, hi, isr;

	if (mutex_lock_interruptible(&ua->lock)) {
		snd_iprintf(buf, "interrupted\n");
		return;
	}
	if (atomic_read(&ua->shutdown)) {
		mutex_unlock(&ua->lock);
		snd_iprintf(buf, "device removed\n");
		return;
	}

	ext_caps = ua->fw_v2 ? ua_read(ua, UA_REG_EXT_CAPS) : 0;
	cap = (ext_caps >> 4) & 0xf;
	lo = ua_read(ua, UA_REG_IRQ_ENABLE);
	hi = ua->fw_v2 ? ua_read(ua, UA_REG_EXT_IRQ_ENABLE) : 0;
	isr = ua_read(ua, UA_REG_IRQ_STATUS);

	snd_iprintf(buf, "dsps\t\t%u (connected %s, firmware %s)\n",
		    ua->num_dsps, ua->dsps_connected ? "yes" : "no",
		    ua->fw_loaded ? "loaded" : "not loaded");
	snd_iprintf(buf, "ext_caps\t0x%08x (capability nibble %u)\n",
		    ext_caps, cap);
	snd_iprintf(buf, "kext_connect\t%d\n", ua_kext_connect);
	snd_iprintf(buf, "bus_enable\tmask 0x%02x enabled, known 0x%02x\n",
		    ua_dsp_get_bus_enable_mask(ua), ua->bus_enable_known & 0x7f);
	snd_iprintf(buf, "bus_coef_cache\t%u of %u cells, %u replay(s)\n",
		    (unsigned int)bitmap_weight(x->bus_coef_valid,
						UA_BUS_COEF_CELLS),
		    UA_BUS_COEF_CELLS, x->bus_replays);
	snd_iprintf(buf, "irq\t\tIMR 0x%08x (cached 0x%08x) IMR_HI 0x%08x ISR 0x%08x\n",
		    lo, ua->irq_mask_lo, hi, isr);
	snd_iprintf(buf, "dsp_msg_irq\t%d (armed %s, storm guard %s, vector mask 0x%08x)\n",
		    dsp_msg_irq, x->msg_armed ? "yes" : "no",
		    x->storm_tripped ? "TRIPPED" : "ok", x->vec_mask);

	snd_iprintf(buf, "error counters\txrun stops %ld (+%ld for layout changes), pcie errors %ld, AX faults %ld, DSP error vectors %ld, block-send errors %ld (last kext result %d)\n",
		    atomic_long_read(&ua->err.count[UA_ERR_XRUN_STOP]),
		    atomic_long_read(&ua->err.xrun_layout),
		    atomic_long_read(&ua->err.count[UA_ERR_PCIE]),
		    atomic_long_read(&ua->err.count[UA_ERR_AX_FAULT]),
		    atomic_long_read(&ua->err.count[UA_ERR_DSP_VECTOR]),
		    atomic_long_read(&ua->err.count[UA_ERR_BLOCK_SEND]),
		    atomic_read(&ua->err.last_kerr));

	for (dsp = 0; dsp < ua->num_dsps && dsp < UA_MAX_DSPS; dsp++) {
		struct ua_dsp_state *ds = &ua->dsp[dsp];
		u32 win = ua_dsp_window(dsp);

		snd_iprintf(buf, "\ndsp %u: window BAR0+0x%04x, rings %s\n",
			    dsp, win, ds->rings_allocated ? "allocated" : "none");
		for (i = 0; i < ARRAY_SIZE(ua_dsp_win_regs); i++)
			snd_iprintf(buf, "  +0x%03x %-24s 0x%08x\n",
				    ua_dsp_win_regs[i].off,
				    ua_dsp_win_regs[i].name,
				    ua_read(ua, win + ua_dsp_win_regs[i].off));
		if (cap == 1)
			snd_iprintf(buf, "  +0x3c8 %-24s 0x%08x\n",
				    "extra info (A13)", ua_read(ua, win + 0x3c8));
		snd_iprintf(buf, "  +0x040 %-24s 0x%08x (sw read index %u)\n",
			    "message FIFO write idx",
			    ua_read(ua, win + UA_DSP_WIN_FIFO_IDX),
			    x->msg_rd[dsp]);
		if (ds->rings_allocated) {
			struct ua_dsp_ring *rings[] = { &ds->cmd, &ds->resp };
			static const char *const rn[] = { "cmd ", "resp" };

			for (i = 0; i < 2; i++) {
				u32 rb = rings[i]->reg_base;

				snd_iprintf(buf,
					    "  %s ring: sw write %u, last pos %u; hw WRITE_PTR %u DOORBELL %u POSITION %u%s\n",
					    rn[i], rings[i]->write_ptr,
					    rings[i]->hw_pos,
					    ua_read(ua, rb + UA_RING_WRITE_PTR),
					    ua_read(ua, rb + UA_RING_DOORBELL),
					    ua_read(ua, rb + UA_RING_POSITION),
					    rings[i]->live ? "" : " (not live)");
			}
		}
		snd_iprintf(buf, "  vectors fired:");
		for (j = 0; j < 5; j++)
			snd_iprintf(buf, " %u", atomic_read(&x->vec_count[dsp][j]));
		snd_iprintf(buf, " (cmd resp fifo errA errB)\n");
		snd_iprintf(buf, "  fifo: %u message(s) (F001 %u, F002 %u), %u stale index\n",
			    x->msg_total[dsp], x->msg_f001[dsp],
			    x->msg_f002[dsp], x->msg_stale[dsp]);
		if (x->msg_total[dsp])
			snd_iprintf(buf, "  last message: %08x %08x %08x %08x %08x\n",
				    x->msg_last[dsp][0], x->msg_last[dsp][1],
				    x->msg_last[dsp][2], x->msg_last[dsp][3],
				    x->msg_last[dsp][4]);
	}

	snd_iprintf(buf, "\nnotification service (notif_irq=%d):\n",
		    ua_notif_irq_enabled());
	snd_iprintf(buf, "  notification interrupts %u, kicked the work %u, storm masks %u\n",
		    ua->audio.notif_irqs, ua->audio.notif_irq_kicks,
		    ua->audio.notif_irq_storms);
	snd_iprintf(buf, "  service passes: %u interrupt-driven, %u polled (10 ms)\n",
		    ua->audio.notif_irq_passes, ua->audio.notif_poll_passes);

	mutex_unlock(&ua->lock);
}

int ua_dsp_proc_init(struct ua_device *ua, struct snd_card *card)
{
	return snd_card_ro_proc_new(card, "dsp_state", ua,
				    ua_dsp_state_proc_read);
}
