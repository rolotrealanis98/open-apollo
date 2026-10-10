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
#include <linux/slab.h>
#include <linux/timekeeping.h>

#include "ua_dsp_programs.h"
#include "ua_dsp_modules.h"

#include "ua_apollo.h"

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
 * Date word of the connect-finalize command, built exactly as
 * CUAD2Device::_connectToDsps does (kext 12.0.0 x86 0x13ef5) from
 * CUAOS::GetCurrentDate() (seconds since the epoch):
 *   w1 = (days_since_1970 << 12) + minute_of_day
 */
static u32 ua_dsp_connect_date(void)
{
	time64_t now = ktime_get_real_seconds();
	u32 secs_of_day;
	u64 days;

	if (now < 0)
		now = 0;
	days = div_u64_rem((u64)now, 86400, &secs_of_day);

	return ((u32)days << 12) + secs_of_day / 60;
}

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
		dev_info(&ua->pdev->dev,
			 "DSP %u: programming rings at BAR0+0x%04x "
			 "(cmd @+0x%02x, resp @+0x%02x)\n",
			 i, bank_base,
			 UA_RING_CMD_OFFSET, UA_RING_RESP_OFFSET);
		dev_info(&ua->pdev->dev,
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
			dev_info(&ua->pdev->dev,
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
	if (ua->fw_v2) {
		u32 dma = ua_read(ua, UA_REG_DMA_CTRL);

		dev_info(&ua->pdev->dev,
			 "DMA_CTRL before engine enable: 0x%08x\n", dma);
		/* macOS uses 0x1FFFE (16 engines); we had 0x1FE (9).
		 * More engines may be needed for type 0x01 capture channels. */
		ua_write(ua, UA_REG_DMA_CTRL, dma | 0x1FFFE);
		ua->dma_ctrl_cache = ua_read(ua, UA_REG_DMA_CTRL);
		dev_info(&ua->pdev->dev,
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

		dev_info(&ua->pdev->dev,
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

		dev_info(&ua->pdev->dev,
			 "DMA area: 2210=%08x 2214=%08x "
			 "221C=%08x 2220=%08x 2224=%08x\n",
			 r2210, r2214, r221c, r2220, r2224);
		dev_info(&ua->pdev->dev,
			 "DMA area: 2228=%08x 222C=%08x "
			 "2230=%08x 2234=%08x 2238=%08x 223C=%08x\n",
			 r2228, r222c, r2230, r2234, r2238, r223c);
	}

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

	dev_info(&ua->pdev->dev,
		 "DSP rings reprogrammed after transport start\n");
}

/* ----------------------------------------------------------------
 * Firmware loading
 * ---------------------------------------------------------------- */

/**
 * ua_dsp_load_firmware - Load firmware binary to DSP 0 via ring buffer DMA
 * @ua: device (must hold ua->lock)
 * @fw_data: kernel buffer containing firmware binary
 * @fw_size: size of firmware in bytes
 *
 * Protocol (from kext _loadFirmware / _sendBlock):
 *   1. Allocate single contiguous DMA buffer for [cmd, count, param, fw_data]
 *   2. Allocate response DMA buffer
 *   3. Submit ONE DMA ref entry to cmd ring (entire payload)
 *   4. Submit ONE DMA ref entry to resp ring
 *   5. Ring doorbells (resp first, then cmd)
 *   6. Poll response buffer for 0x8004xxxx
 *   7. Free DMA buffers
 *
 * The kext uses a single large DMA buffer per sendBlock call.
 * The FPGA processes one ring entry per logical command — chained
 * entries do NOT work for firmware loading.
 */
int ua_dsp_load_firmware(struct ua_device *ua, const void *fw_data,
			 size_t fw_size)
{
	return ua_dsp_load_firmware_to(ua, 0, fw_data, fw_size);
}

/**
 * ua_dsp_load_firmware_to - Load firmware to a specific DSP
 */
int ua_dsp_load_firmware_to(struct ua_device *ua, unsigned int dsp_idx,
			     const void *fw_data, size_t fw_size)
{
	struct ua_dsp_state *dsp0 = &ua->dsp[dsp_idx];
	u32 bank_base = ua_dsp_base(dsp_idx);
	void *cmd_buf = NULL, *resp_buf = NULL;
	dma_addr_t cmd_addr, resp_addr;
	struct ua_ring_entry entry;
	u32 *cmd_words;
	u32 fw_dwords;
	u32 total_dwords;
	size_t cmd_buf_size;
	unsigned int polls;
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

	/*
	 * Allocate single contiguous DMA buffer for:
	 *   [cmd_word, count_word, param_word, firmware_data...]
	 * The FPGA reads this as one DMA ref entry.
	 */
	cmd_buf_size = (3 + fw_dwords) * 4;
	total_dwords = 3 + fw_dwords;

	dev_info(&ua->pdev->dev,
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

	/* Build command buffer: [cmd, count, param, fw_data...] */
	cmd_words = cmd_buf;
	cmd_words[0] = cpu_to_le32(UA_FW_CMD | UA_FW_LARGE_FLAG);
	cmd_words[1] = cpu_to_le32(fw_dwords + 2);
	cmd_words[2] = cpu_to_le32(UA_FW_PARAM);
	memcpy(&cmd_words[3], fw_data, fw_size);

	/* Zero response buffer */
	memset(resp_buf, 0, UA_PCIE_PAGE_SIZE);

	/* Reset and re-program rings */
	ua_dsp_ring_program(ua, &dsp0->cmd,
			    bank_base + UA_RING_CMD_OFFSET);
	ua_dsp_ring_program(ua, &dsp0->resp,
			    bank_base + UA_RING_RESP_OFFSET);

	/* Submit response ring entry FIRST (provide buffer before sending cmd) */
	entry = (struct ua_ring_entry){
		.word0 = cpu_to_le32(UA_RING_DMA_REF | (UA_PCIE_PAGE_SIZE / 4)),
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

	/* Submit command ring entry: single DMA ref for entire payload */
	entry = (struct ua_ring_entry){
		.word0 = cpu_to_le32(UA_RING_DMA_REF | total_dwords),
		.word1 = 0,
		.word2 = cpu_to_le32(lower_32_bits(cmd_addr)),
		.word3 = cpu_to_le32(upper_32_bits(cmd_addr)),
	};
	ret = ua_dsp_ring_put_entry(ua, &dsp0->cmd, &entry);
	if (ret) {
		/* The response descriptor is already queued: keep its page */
		ua_dsp_orphan_dma(ua, resp_buf, resp_addr, UA_PCIE_PAGE_SIZE,
				  &dsp0->resp, NULL);
		goto free_cmd;
	}

	dev_info(&ua->pdev->dev,
		 "fw cmd entry: [%08x %08x %08x %08x] cmd_addr=%pad\n",
		 le32_to_cpu(entry.word0), le32_to_cpu(entry.word1),
		 le32_to_cpu(entry.word2), le32_to_cpu(entry.word3),
		 &cmd_addr);
	dev_info(&ua->pdev->dev,
		 "fw cmd buf: [%08x %08x %08x %08x ...] total=%u dwords\n",
		 le32_to_cpu(cmd_words[0]), le32_to_cpu(cmd_words[1]),
		 le32_to_cpu(cmd_words[2]), le32_to_cpu(cmd_words[3]),
		 total_dwords);

	ua_dsp_ring_doorbell(ua, &dsp0->cmd,
			     bank_base + UA_RING_CMD_OFFSET);

	dev_info(&ua->pdev->dev,
		 "firmware load: %zu bytes, polling...\n", fw_size);

	/* Poll response buffer (DMA coherent — no lock needed) */
	ret = -ETIMEDOUT;
	for (polls = 0; polls < UA_FW_TIMEOUT_MS / UA_FW_POLL_MS; polls++) {
		u32 resp_word0 = le32_to_cpu(((u32 *)resp_buf)[0]);

		if ((resp_word0 >> 16) == UA_FW_RESP_MATCH) {
			dev_info(&ua->pdev->dev,
				 "firmware loaded successfully (response=0x%08x)\n",
				 resp_word0);
			ua->fw_loaded = true;
			ret = 0;
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
			dev_info(&ua->pdev->dev,
				 "fw poll %us: cmd_pos=%u resp_pos=%u resp[0]=0x%08x\n",
				 polls / 100, cmd_pos, resp_pos, resp_word0);
		}
		msleep(UA_FW_POLL_MS);
	}

	if (ret) {
		u32 cmd_pos = ua_read(ua, bank_base + UA_RING_CMD_OFFSET +
				      UA_RING_POSITION);
		u32 resp_pos = ua_read(ua, bank_base + UA_RING_RESP_OFFSET +
				       UA_RING_POSITION);
		u32 *resp_words = resp_buf;

		dev_err(&ua->pdev->dev,
			"firmware load timeout after %u ms\n",
			polls * UA_FW_POLL_MS);
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
		return ret;
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
#pragma GCC push_options
#pragma GCC optimize("O1")
int ua_dsp_test_dma_ref(struct ua_device *ua)
{
	struct ua_dsp_state *dsp0 = &ua->dsp[0];
	u32 bank_base = ua_dsp_base(0);
	u32 cmd_base = bank_base + UA_RING_CMD_OFFSET;
	void *dma_buf;
	dma_addr_t dma_addr;
	u32 *words;
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
#pragma GCC pop_options

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
 *   - Data split at 4KB page boundaries (DMA page alignment)
 *   - Doorbell order: DOORBELL(0x24) first, then WRITE_PTR(0x20)
 */

/* Response buffer size: 4 dwords (16 bytes), matching kext descriptor2.
 * The FPGA writes a 4-dword completion status to this buffer.
 */
#define SEND_BLOCK_RESP_DWORDS	4

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
	u32 *resp_words;
	size_t data_alloc, offset, chunk;
	unsigned int cmd_start;
	bool resp_queued = false;
	int polls, ret = 0;

	if (dsp_idx >= ua->num_dsps)
		return -EINVAL;
	if (data_size & 3)
		return -EINVAL;
	if (data_size > 256 * 1024)
		return -E2BIG;

	ds = &ua->dsp[dsp_idx];
	if (!ds->rings_allocated)
		return -ENODEV;

	bank_base = ua_dsp_base(dsp_idx);
	cmd_base = bank_base + UA_RING_CMD_OFFSET;
	resp_base = bank_base + UA_RING_RESP_OFFSET;

	data_dwords = data_size / 4;
	num_pages = DIV_ROUND_UP(data_size, 1024);  /* 1KB chunks */

	ret = ua_dsp_orphan_room(ua);
	if (ret)
		return ret;

	/* Reserve ring room: 1 header + N data chunks (circular ring) */
	ret = ua_dsp_ring_wait_space(ua, &ds->cmd, 1 + num_pages, 1000);
	if (ret)
		return ret;

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
	 * Small format (data < 262140 bytes):
	 *   header[0] = (data_dwords + 1) | cmd
	 *
	 * Large format (data >= 262140 bytes):
	 *   header[0] = cmd | BIT(30)
	 *   header[1] = data_dwords + 2
	 */
	memset(hdr_buf, 0, UA_PCIE_PAGE_SIZE);
	if (data_size < 262140) {
		((u32 *)hdr_buf)[0] = cpu_to_le32((data_dwords + 1) | cmd);

		/* Header DMA ref: 1 dword */
		entry = (struct ua_ring_entry){
			.word0 = cpu_to_le32(UA_RING_DMA_REF | 1),
			.word1 = 0,
			.word2 = cpu_to_le32(lower_32_bits(hdr_dma)),
			.word3 = cpu_to_le32(upper_32_bits(hdr_dma)),
		};
	} else {
		((u32 *)hdr_buf)[0] = cpu_to_le32(cmd | BIT(30));
		((u32 *)hdr_buf)[1] = cpu_to_le32(data_dwords + 2);

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
	 * Kext splits data at 4KB page boundaries.  However, our DMA ref
	 * size limit tests showed stalls at 512+ dwords (2KB+).  Use 1KB
	 * chunks (256 dwords) which are verified working.  Try 4KB later
	 * once basic flow works.
	 */
#define SEND_BLOCK_CHUNK_SIZE	1024
	for (offset = 0; offset < data_size; offset += chunk) {
		u32 chunk_dwords;

		chunk = min_t(size_t, data_size - offset, SEND_BLOCK_CHUNK_SIZE);
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
	 * Step 5: Poll response buffer for completion.
	 *
	 * Kext checks: (resp[0] >> 16) == (param >> 16)
	 * For FW blocks with param=1: accept any non-zero response.
	 * Timeout: ~150ms (1500 polls × 100us).
	 */
	for (polls = 0; polls < 1500; polls++) {
		usleep_range(100, 200);
		/* Check if FPGA wrote anything to response buffer */
		if (resp_words[0] != 0)
			break;
	}

	dev_info(&ua->pdev->dev,
		 "sendBlock DSP%u: cmd=0x%08x param=0x%08x size=%zu "
		 "resp=[0x%08x 0x%08x 0x%08x 0x%08x] pos=%u/%u\n",
		 dsp_idx, cmd, param, data_size,
		 le32_to_cpu(resp_words[0]), le32_to_cpu(resp_words[1]),
		 le32_to_cpu(resp_words[2]), le32_to_cpu(resp_words[3]),
		 ua_read(ua, cmd_base + UA_RING_POSITION),
		 ds->cmd.write_ptr);

	if (resp_words[0] == 0) {
		dev_warn(&ua->pdev->dev,
			 "sendBlock: no response after %d polls\n", polls);
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

	/* Wait for DSP to consume both entries */
	if (!ua_dsp_ring_wait_idle(ua, &ds->cmd, 150)) {
		dev_warn(&ua->pdev->dev,
			 "sram_data: DSP %u timeout (pos=%u wr=%u)\n",
			 dsp_idx, ds->cmd.hw_pos, ds->cmd.write_ptr);
		/* The DMA ref may still be fetched: park its buffer */
		ua_dsp_orphan_dma(ua, dma_buf, dma_addr, UA_PCIE_PAGE_SIZE,
				  &ds->cmd, NULL);
		return 0;
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

	/* Wait for DSP to consume the entry before freeing DMA buffer */
	if (!ua_dsp_ring_wait_idle(ua, &ds->cmd, 150)) {
		dev_warn(&ua->pdev->dev,
			 "raw_dma: DSP %u timeout (pos=%u wr=%u)\n",
			 dsp_idx, ds->cmd.hw_pos, ds->cmd.write_ptr);
		/* The DMA ref may still be fetched: park its buffer */
		ua_dsp_orphan_dma(ua, dma_buf, dma_addr, UA_PCIE_PAGE_SIZE,
				  &ds->cmd, NULL);
		return 0;
	}

	dma_free_coherent(&ua->pdev->dev, UA_PCIE_PAGE_SIZE,
			  dma_buf, dma_addr);
	return 0;
}

/**
 * ua_dsp_test_send_block - Test sendBlock with connect + response ring
 * @ua: device (must hold ua->lock)
 *
 * Sends the connect command via both DMA ref and inline-with-addr formats,
 * checking the response ring for DSP replies.
 */
int __attribute__((optimize("O1"))) ua_dsp_test_send_block(struct ua_device *ua)
{
	struct ua_dsp_state *dsp0 = &ua->dsp[0];
	u32 bank_base = ua_dsp_base(0);
	u32 cmd_base = bank_base + UA_RING_CMD_OFFSET;
	u32 resp_base = bank_base + UA_RING_RESP_OFFSET;
	void *cmd_buf, *resp_buf;
	dma_addr_t cmd_addr, resp_addr;
	u32 *cmd_words, *resp_words;
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
		u32 *ring_page = dsp0->resp.pages[0];

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
 * Ring buffer engine diagnostic
 * ---------------------------------------------------------------- */

/**
 * ua_dsp_ring_diagnostic - Test if FPGA ring buffer engine processes entries
 * @ua: device
 *
 * Runs after ACEFACE + DMA pulse, before firmware loading.
 * Submits inline entries (no DMA ref) to determine if the ring buffer
 * engine is active.  This helps distinguish "engine not started" from
 * "DMA ref format issue".
 */
void ua_dsp_ring_diagnostic(struct ua_device *ua)
{
	struct ua_dsp_state *dsp0 = &ua->dsp[0];
	u32 cb = ua_dsp_base(0) + UA_RING_CMD_OFFSET;
	u32 rb = ua_dsp_base(0) + UA_RING_RESP_OFFSET;
	struct ua_ring_entry ent;
	u32 pos_before, pos_after, pos_resp;
	u32 reg2c, reg30, reg34, reg38, reg3c;

	if (!dsp0->rings_allocated)
		return;

	/* Read unknown ring registers beyond POSITION for clues */
	reg2c = ua_read(ua, cb + 0x2C);
	reg30 = ua_read(ua, cb + 0x30);
	reg34 = ua_read(ua, cb + 0x34);
	reg38 = ua_read(ua, cb + 0x38);
	reg3c = ua_read(ua, cb + 0x3C);
	dev_info(&ua->pdev->dev,
		 "ring diag: cmd extra regs "
		 "+2C=%08x +30=%08x +34=%08x +38=%08x +3C=%08x\n",
		 reg2c, reg30, reg34, reg38, reg3c);

	pos_before = ua_read(ua, cb + UA_RING_POSITION);

	/* Test 1: Single inline connect entry (no DMA ref) */
	ua_dsp_ring_program(ua, &dsp0->cmd, cb);
	ent = (struct ua_ring_entry){
		.word0 = cpu_to_le32(UA_DSP_CMD_CONNECT),
		.word1 = cpu_to_le32(1),
		.word2 = 0,
		.word3 = 0,
	};
	ua_dsp_ring_put_entry(ua, &dsp0->cmd, &ent);
	ua_dsp_ring_doorbell(ua, &dsp0->cmd, cb);
	msleep(200);

	pos_after = ua_read(ua, cb + UA_RING_POSITION);
	dev_info(&ua->pdev->dev,
		 "ring diag test1 (inline connect): "
		 "pos %u → %u (WPTR=1 DBELL=1)\n",
		 pos_before, pos_after);

	/* Check if POSITION register is writable (try reset to 0) */
	ua_write(ua, cb + UA_RING_POSITION, 0);
	dev_info(&ua->pdev->dev,
		 "ring diag: wrote 0 to POSITION, readback=%u\n",
		 ua_read(ua, cb + UA_RING_POSITION));

	/* Test 2: Both cmd+resp doorbelled simultaneously */
	ua_dsp_ring_program(ua, &dsp0->cmd, cb);
	ua_dsp_ring_program(ua, &dsp0->resp, rb);

	ent = (struct ua_ring_entry){
		.word0 = cpu_to_le32(UA_DSP_CMD_CONNECT),
		.word1 = cpu_to_le32(1),
		.word2 = 0,
		.word3 = 0,
	};
	ua_dsp_ring_put_entry(ua, &dsp0->cmd, &ent);

	ent = (struct ua_ring_entry){
		.word0 = cpu_to_le32(UA_RING_DMA_REF | 4),
		.word1 = 0,
		.word2 = cpu_to_le32(lower_32_bits(dsp0->resp.page_addrs[0])),
		.word3 = cpu_to_le32(upper_32_bits(dsp0->resp.page_addrs[0])),
	};
	ua_dsp_ring_put_entry(ua, &dsp0->resp, &ent);

	ua_dsp_ring_doorbell(ua, &dsp0->cmd, cb);
	ua_dsp_ring_doorbell(ua, &dsp0->resp, rb);
	msleep(200);

	pos_after = ua_read(ua, cb + UA_RING_POSITION);
	pos_resp = ua_read(ua, rb + UA_RING_POSITION);
	dev_info(&ua->pdev->dev,
		 "ring diag test2 (both doorbell): "
		 "cmd_pos=%u resp_pos=%u\n",
		 pos_after, pos_resp);

	/* Verify ring page content is what we wrote */
	if (dsp0->cmd.pages[0]) {
		u32 *p = dsp0->cmd.pages[0];

		dev_info(&ua->pdev->dev,
			 "ring diag: cmd page[0] slot0="
			 "%08x %08x %08x %08x\n",
			 le32_to_cpu(p[0]), le32_to_cpu(p[1]),
			 le32_to_cpu(p[2]), le32_to_cpu(p[3]));
	}

	/* Test 3: Try writing DOORBELL value=0 then value=1 (some FPGAs
	 * use edge-triggered doorbells, not level-triggered)
	 */
	ua_dsp_ring_program(ua, &dsp0->cmd, cb);
	ent = (struct ua_ring_entry){
		.word0 = cpu_to_le32(UA_DSP_CMD_CONNECT),
		.word1 = cpu_to_le32(1),
		.word2 = 0,
		.word3 = 0,
	};
	ua_dsp_ring_put_entry(ua, &dsp0->cmd, &ent);

	/* Write WRITE_PTR first, then do doorbell with explicit 0→1 */
	ua_write(ua, cb + UA_RING_WRITE_PTR, 1);
	ua_write(ua, cb + UA_RING_DOORBELL, 0);  /* reset doorbell */
	wmb();
	ua_write(ua, cb + UA_RING_DOORBELL, 1);  /* trigger */
	msleep(200);

	pos_after = ua_read(ua, cb + UA_RING_POSITION);
	dev_info(&ua->pdev->dev,
		 "ring diag test3 (edge doorbell): cmd_pos=%u\n",
		 pos_after);

	/* Clean up: reset rings for FW loading */
	ua_dsp_ring_program(ua, &dsp0->cmd, cb);
	ua_dsp_ring_program(ua, &dsp0->resp, rb);
}

/**
 * ua_dsp_test_dma_locality - Determine if FPGA can DMA-read host memory
 * @ua: device
 *
 * THE critical diagnostic: uses CUMULATIVE ring entries (no ring_program
 * between tests) to avoid stale POSITION.  Each test adds one entry,
 * doorbells, and checks if POSITION advances to the expected value.
 *
 * Tests DMA ref with:
 *   A: Small payload (8 bytes) → ring page 1 (registered address)
 *   B: Small payload (8 bytes) → external DMA buffer
 *   C: Large payload (~12KB) → external DMA buffer (FW-sized)
 *   D: Inline baseline (sanity check)
 */
void ua_dsp_test_dma_locality(struct ua_device *ua)
{
	struct ua_dsp_state *dsp0 = &ua->dsp[0];
	u32 cb = ua_dsp_base(0) + UA_RING_CMD_OFFSET;
	struct ua_ring_entry ent;
	u32 *page1_words;
	void *ext_buf = NULL;
	dma_addr_t ext_dma = 0;
	void *big_buf = NULL;
	dma_addr_t big_dma = 0;
	u32 pos_before, pos_after;
	size_t big_size = ALIGN(12040 + 8, UA_PCIE_PAGE_SIZE);

	if (!dsp0->rings_allocated || dsp0->cmd.num_pages < 2)
		return;

	/* Allocate external buffers upfront */
	ext_buf = dma_alloc_coherent(&ua->pdev->dev, UA_PCIE_PAGE_SIZE,
				     &ext_dma, GFP_KERNEL);
	big_buf = dma_alloc_coherent(&ua->pdev->dev, big_size,
				     &big_dma, GFP_KERNEL);

	dev_info(&ua->pdev->dev, "=== DMA locality test (cumulative) ===\n");
	dev_info(&ua->pdev->dev,
		 "  cmd page0=%pad  page1=%pad  ext=%pad  big=%pad\n",
		 &dsp0->cmd.page_addrs[0], &dsp0->cmd.page_addrs[1],
		 &ext_dma, &big_dma);

	/* Program ring ONCE — all tests use cumulative write_ptr */
	ua_dsp_ring_program(ua, &dsp0->cmd, cb);

	pos_before = ua_read(ua, cb + UA_RING_POSITION);
	dev_info(&ua->pdev->dev,
		 "  initial POSITION=%u WRITE_PTR=%u\n",
		 pos_before, dsp0->cmd.write_ptr);

	/*
	 * Test A: DMA ref → ring page 1 (small, 2 dwords, registered addr)
	 */
	page1_words = dsp0->cmd.pages[1];
	page1_words[0] = cpu_to_le32(UA_DSP_CMD_CONNECT);
	page1_words[1] = cpu_to_le32(1);

	ent = (struct ua_ring_entry){
		.word0 = cpu_to_le32(UA_RING_DMA_REF | 2),
		.word1 = 0,
		.word2 = cpu_to_le32(lower_32_bits(dsp0->cmd.page_addrs[1])),
		.word3 = cpu_to_le32(upper_32_bits(dsp0->cmd.page_addrs[1])),
	};
	ua_dsp_ring_put_entry(ua, &dsp0->cmd, &ent);
	ua_dsp_ring_doorbell(ua, &dsp0->cmd, cb);
	msleep(200);
	pos_after = ua_read(ua, cb + UA_RING_POSITION);
	dev_info(&ua->pdev->dev,
		 "  Test A (DMA ref→page1, 8B): pos %u→%u %s\n",
		 pos_before, pos_after,
		 pos_after > pos_before ? "WORKS" : "FAIL");
	pos_before = pos_after;

	/*
	 * Test B: DMA ref → external buffer (small, 2 dwords)
	 */
	if (ext_buf) {
		memset(ext_buf, 0, UA_PCIE_PAGE_SIZE);
		((u32 *)ext_buf)[0] = cpu_to_le32(UA_DSP_CMD_CONNECT);
		((u32 *)ext_buf)[1] = cpu_to_le32(1);

		ent = (struct ua_ring_entry){
			.word0 = cpu_to_le32(UA_RING_DMA_REF | 2),
			.word1 = 0,
			.word2 = cpu_to_le32(lower_32_bits(ext_dma)),
			.word3 = cpu_to_le32(upper_32_bits(ext_dma)),
		};
		ua_dsp_ring_put_entry(ua, &dsp0->cmd, &ent);
		ua_dsp_ring_doorbell(ua, &dsp0->cmd, cb);
		msleep(200);
		pos_after = ua_read(ua, cb + UA_RING_POSITION);
		dev_info(&ua->pdev->dev,
			 "  Test B (DMA ref→ext, 8B): pos %u→%u %s\n",
			 pos_before, pos_after,
			 pos_after > pos_before ? "WORKS" : "FAIL");
		pos_before = pos_after;
	}

	/*
	 * Tests C-H: DMA ref size sweep.
	 * Find the maximum DMA ref size the FPGA will process.
	 * Uses the same external buffer, varying the size field.
	 * Sizes: 4, 16, 64, 256, 1024 (=4KB), 3012 (~12KB) dwords.
	 * All contain a connect command at the start.
	 */
	if (big_buf) {
		static const u32 test_sizes[] = {
			4, 256, 384, 512, 640, 768, 1024
		};
		static const char * const test_labels[] = {
			"16B", "1KB", "1.5KB", "2KB", "2.5KB", "3KB", "4KB"
		};
		u32 *bw = big_buf;
		int t;

		memset(big_buf, 0, big_size);
		bw[0] = cpu_to_le32(UA_DSP_CMD_CONNECT);
		bw[1] = cpu_to_le32(1);

		for (t = 0; t < 7; t++) {
			ent = (struct ua_ring_entry){
				.word0 = cpu_to_le32(UA_RING_DMA_REF |
						     test_sizes[t]),
				.word1 = 0,
				.word2 = cpu_to_le32(lower_32_bits(big_dma)),
				.word3 = cpu_to_le32(upper_32_bits(big_dma)),
			};
			ua_dsp_ring_put_entry(ua, &dsp0->cmd, &ent);
			ua_dsp_ring_doorbell(ua, &dsp0->cmd, cb);
			msleep(300);
			pos_after = ua_read(ua, cb + UA_RING_POSITION);
			dev_info(&ua->pdev->dev,
				 "  Test %c (DMA ref %s, %u dw): "
				 "pos %u→%u %s\n",
				 'C' + t, test_labels[t], test_sizes[t],
				 pos_before, pos_after,
				 pos_after > pos_before ? "WORKS" : "FAIL");

			if (pos_after <= pos_before) {
				/* Ring stalled — try to recover with
				 * ring_program for remaining tests */
				dev_info(&ua->pdev->dev,
					 "  STALL at %u dwords (%s) — "
					 "max working size was %s\n",
					 test_sizes[t], test_labels[t],
					 t > 0 ? test_labels[t - 1] : "< 4dw");
				break;
			}
			pos_before = pos_after;
		}
	}

	/*
	 * Test I: Inline after sweep (check if ring recovered or stalled)
	 */
	ent = (struct ua_ring_entry){
		.word0 = cpu_to_le32(UA_DSP_CMD_CONNECT),
		.word1 = cpu_to_le32(1),
		.word2 = 0,
		.word3 = 0,
	};
	ua_dsp_ring_put_entry(ua, &dsp0->cmd, &ent);
	ua_dsp_ring_doorbell(ua, &dsp0->cmd, cb);
	msleep(200);
	pos_after = ua_read(ua, cb + UA_RING_POSITION);
	dev_info(&ua->pdev->dev,
		 "  Test I (inline post-sweep): pos %u→%u %s\n",
		 pos_before, pos_after,
		 pos_after > pos_before ? "WORKS" : "STALLED");

	dev_info(&ua->pdev->dev,
		 "  final: POSITION=%u WRITE_PTR=%u\n",
		 pos_after, dsp0->cmd.write_ptr);
	dev_info(&ua->pdev->dev, "=== DMA locality test done ===\n");

	/* A stalled DMA ref may still read them: park, freed once consumed */
	ua_dsp_orphan_dma(ua, ext_buf, ext_dma, UA_PCIE_PAGE_SIZE,
			  &dsp0->cmd, NULL);
	ua_dsp_orphan_dma(ua, big_buf, big_dma, big_size, &dsp0->cmd, NULL);

	/*
	 * CRITICAL: Sync write_ptr for FW loading.
	 * Don't ring_program (would desync POSITION vs WRITE_PTR).
	 * FW loading will continue accumulating from current write_ptr.
	 */
}

/* ----------------------------------------------------------------
 * Mixer firmware blob loading (request_firmware API)
 * ---------------------------------------------------------------- */

/**
 * ua_dsp_load_mixer_blocks - Load mixer firmware from filesystem blob
 * @ua: device (must hold ua->lock)
 *
 * Uses the Linux request_firmware() API to load ua-apollo-mixer.bin
 * from /lib/firmware/.  The blob contains a header, block table, and
 * concatenated firmware payloads (packed by tools/pack-mixer-firmware.py).
 *
 * Each block is sent to DSP 0 via ua_dsp_send_block() with:
 *   cmd   = SRAM address (from block table)
 *   param = 1 (constant, matches macOS capture)
 *   data  = block payload
 *
 * Returns 0 on success, negative errno on failure.
 * -ENOENT if firmware file not found (non-fatal, expected on first install).
 */
int ua_dsp_load_mixer_blocks(struct ua_device *ua)
{
	const struct firmware *fw;
	const u8 *data;
	u32 magic, version, num_blocks;
	unsigned int header_size, table_size;
	unsigned int i;
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

	data = fw->data;

	/* Validate header (16 bytes minimum) */
	if (fw->size < 16) {
		dev_err(&ua->pdev->dev, "mixer firmware too small (%zu bytes)\n",
			fw->size);
		ret = -EINVAL;
		goto out;
	}

	magic = le32_to_cpup((__le32 *)&data[0]);
	version = le32_to_cpup((__le32 *)&data[4]);
	num_blocks = le32_to_cpup((__le32 *)&data[8]);

	if (magic != UA_MIXER_FW_MAGIC) {
		dev_err(&ua->pdev->dev,
			"mixer firmware bad magic: 0x%08x (expected 0x%08x)\n",
			magic, UA_MIXER_FW_MAGIC);
		ret = -EINVAL;
		goto out;
	}

	if (version != UA_MIXER_FW_VERSION) {
		dev_err(&ua->pdev->dev,
			"mixer firmware version %u unsupported (expected %u)\n",
			version, UA_MIXER_FW_VERSION);
		ret = -EINVAL;
		goto out;
	}

	if (num_blocks == 0 || num_blocks > 64) {
		dev_err(&ua->pdev->dev,
			"mixer firmware invalid block count: %u\n", num_blocks);
		ret = -EINVAL;
		goto out;
	}

	header_size = 16;
	table_size = num_blocks * 12;

	if (fw->size < header_size + table_size) {
		dev_err(&ua->pdev->dev,
			"mixer firmware truncated (need %u, got %zu)\n",
			header_size + table_size, fw->size);
		ret = -EINVAL;
		goto out;
	}

	dev_info(&ua->pdev->dev,
		 "loading mixer firmware: %u blocks, %zu bytes\n",
		 num_blocks, fw->size);

	/*
	 * 3-phase FW load (from Windows UAD2Pcie.sys Ghidra):
	 *   0xD0000 (pre-load) → 0x120000 (upload) → 0xE0000 (post-load)
	 *
	 * Pre/post phases DISABLED: sending 0xD0000 before main FW
	 * consumes ring entries and desyncs the ring when DSP can't
	 * respond (bootloader only handles 0x120000). The pre/post
	 * phases may only apply to SUBSEQUENT loads (rate changes),
	 * not the initial cold boot load.
	 *
	 * TODO: Try pre/post after initial load succeeds, or for
	 * DSPs 1-3 after DSP 0 is fully initialized.
	 */

	/* Phase 2: Main firmware upload (0x120000) — iterate blocks */
	for (i = 0; i < num_blocks; i++) {
		u32 sram_addr, block_size, block_offset;
		unsigned int entry_off = header_size + i * 12;

		sram_addr = le32_to_cpup((__le32 *)&data[entry_off]);
		block_size = le32_to_cpup((__le32 *)&data[entry_off + 4]);
		block_offset = le32_to_cpup((__le32 *)&data[entry_off + 8]);

		/* Validate block bounds */
		if (block_offset + block_size > fw->size) {
			dev_err(&ua->pdev->dev,
				"block %u overflows firmware (off=%u size=%u total=%zu)\n",
				i, block_offset, block_size, fw->size);
			ret = -EINVAL;
			goto out;
		}

		if (block_size & 3) {
			dev_err(&ua->pdev->dev,
				"block %u size %u not 4-byte aligned\n",
				i, block_size);
			ret = -EINVAL;
			goto out;
		}

		/*
		 * Try cmd=0x120000 (kext LoadFirmware address) with param
		 * 0x80040000 first.  The kext sends the entire firmware as
		 * one _sendBlock with these values.  Our per-block SRAM
		 * addresses (0x0e8cc000 etc.) may not be valid ring commands.
		 *
		 * If 0x120000 fails, fall back to per-block SRAM addresses.
		 */
		ret = ua_dsp_send_block(ua, 0, 0x120000, 0x80040000,
					&data[block_offset], block_size);
		if (ret) {
			dev_info(&ua->pdev->dev,
				 "block %u: cmd=0x120000 failed (%d), trying sram=0x%08x\n",
				 i, ret, sram_addr);
			ret = ua_dsp_send_block(ua, 0, sram_addr, 1,
						&data[block_offset], block_size);
		}
		if (ret) {
			dev_err(&ua->pdev->dev,
				"block %u (sram=0x%08x, %u bytes) send failed: %d\n",
				i, sram_addr, block_size, ret);
			goto out;
		}
	}

	ua->fw_loaded = true;
	dev_info(&ua->pdev->dev,
		 "mixer firmware loaded: %u blocks sent to DSP 0\n",
		 num_blocks);

out:
	release_firmware(fw);
	return ret;
}

/**
 * ua_dsp_load_mixer_blocks_to - Load mixer firmware to a specific DSP
 * @ua: device
 * @dsp_idx: target DSP (1-3)
 *
 * Same as ua_dsp_load_mixer_blocks but sends blocks to dsp_idx
 * instead of DSP 0. Uses the same firmware file.
 */
int ua_dsp_load_mixer_blocks_to(struct ua_device *ua, unsigned int dsp_idx)
{
	const struct firmware *fw;
	const u8 *data;
	u32 num_blocks;
	unsigned int header_size, i;
	int ret;

	if (dsp_idx >= ua->num_dsps || !ua->dsp[dsp_idx].rings_allocated)
		return -ENODEV;

	ret = request_firmware(&fw, UA_MIXER_FW_NAME, &ua->pdev->dev);
	if (ret)
		return ret;

	data = fw->data;
	if (fw->size < 16 ||
	    le32_to_cpup((__le32 *)&data[0]) != UA_MIXER_FW_MAGIC) {
		release_firmware(fw);
		return -EINVAL;
	}

	num_blocks = le32_to_cpup((__le32 *)&data[8]);
	header_size = 16;

	for (i = 0; i < num_blocks; i++) {
		u32 sram_addr, block_size, block_offset;
		unsigned int entry_off = header_size + i * 12;

		sram_addr = le32_to_cpup((__le32 *)&data[entry_off]);
		block_size = le32_to_cpup((__le32 *)&data[entry_off + 4]);
		block_offset = le32_to_cpup((__le32 *)&data[entry_off + 8]);

		if (block_offset + block_size > fw->size || (block_size & 3))
			break;

		ret = ua_dsp_send_block(ua, dsp_idx, 0x120000, 0x80040000,
					&data[block_offset], block_size);
		if (ret) {
			dev_warn(&ua->pdev->dev,
				 "DSP %u block %u failed: %d\n",
				 dsp_idx, i, ret);
			break;
		}
	}

	dev_info(&ua->pdev->dev,
		 "DSP %u: %u/%u firmware blocks loaded\n",
		 dsp_idx, i, num_blocks);

	release_firmware(fw);
	return (i == num_blocks) ? 0 : ret;
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

	if (!ua->fw_loaded)
		dev_info(&ua->pdev->dev,
			 "DSP connect: firmware not loaded (testing)\n");

	/* Phase 1: Send connect command to each DSP */
	for (i = 0; i < ua->num_dsps; i++) {
		struct ua_dsp_state *ds = &ua->dsp[i];
		u32 bank_base = ua_dsp_base(i);

		if (!ds->rings_allocated)
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
		dev_info(&ua->pdev->dev,
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
	dev_info(&ua->pdev->dev,
		 "DSP 0 finalize (date 0x%08x): pos=%u (expect %u)\n",
		 date,
		 ua_read(ua, ua_dsp_base(0) + UA_RING_CMD_OFFSET +
			 UA_RING_POSITION),
		 ua->dsp[0].cmd.write_ptr);

	ua->dsps_connected = true;
	dev_info(&ua->pdev->dev, "all %u DSPs connected\n", ua->num_dsps);
	return 0;
}

/* ----------------------------------------------------------------
 * Plugin chain activation
 * ---------------------------------------------------------------- */

#include "ua_plugin_chain.h"

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
 * -ENOENT is a soft failure: plugin-chain activation will still run
 * but DMA_REF entries will be skipped, so DSP programs that depend on
 * plugin-loaded code (including the ARM flag-forwarder driving
 * PAD/48V/MicLine relays) will not initialize.
 */
int ua_dsp_load_plugin_payloads(struct ua_device *ua)
{
	const struct firmware *fw;
	const u8 *data;
	u32 magic, version, entry_count, data_offset;
	unsigned int table_size, i;
	int ret;

	if (ua->plugin_bufs)
		return 0;   /* Already loaded */

	ret = request_firmware(&fw, UA_PLUGIN_CHAIN_FW_NAME, &ua->pdev->dev);
	if (ret) {
		if (ret == -ENOENT)
			dev_warn(&ua->pdev->dev,
				 "plugin chain firmware '%s' not found; "
				 "DMA_REF entries will be skipped (preamp "
				 "PAD/48V/MicLine relays will not work)\n",
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
	if (fw->size < 16 + table_size || data_offset < 16 + table_size) {
		dev_err(&ua->pdev->dev,
			"plugin chain fw truncated (size=%zu, need >= %u)\n",
			fw->size, 16 + table_size);
		ret = -EINVAL;
		goto out;
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

		if (!payload_bytes ||
		    payload_off < data_offset ||
		    payload_off + payload_bytes > fw->size) {
			dev_err(&ua->pdev->dev,
				"plugin fw entry %u bad range "
				"(off=%u bytes=%u total=%zu)\n",
				i, payload_off, payload_bytes, fw->size);
			ret = -EINVAL;
			goto out_free;
		}

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
 * contents came from ua_dsp_load_plugin_payloads().  If no payload
 * was loaded (firmware blob missing), the DMA_REF entry is skipped.
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
	 * code or tables (including the ARM flag-forwarder driving
	 * PAD/48V/MicLine relays) never initialize.  Returning an error
	 * here keeps ua->plugins_activated = false in the caller so a
	 * subsequent reconnect retries once the firmware blob is in
	 * /lib/firmware/.
	 */
	if (!ua->plugin_bufs) {
		dev_warn(&ua->pdev->dev,
			 "plugin chain: payloads not loaded, activation skipped "
			 "(install %s to enable preamp relays)\n",
			 UA_PLUGIN_CHAIN_FW_NAME);
		return -ENODATA;
	}

	dev_info(&ua->pdev->dev,
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
			 * of the matching payload buffer.  Skip the entry if
			 * no payload is available — the FPGA would otherwise
			 * DMA garbage from an invalid address.
			 */
			if (cmd[0] & 0x80000000) {
				struct ua_plugin_dma_buf *buf =
					ua_plugin_lookup_dma(ua, i + j);

				if (!buf) {
					skipped++;
					continue;
				}
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
 *   word2: sub_param     (0=main, 1=CUE1, 2=CUE2, 3=gainL, 4=gainR)
 *   word3: value         (IEEE 754 float as u32)
 *
 * This is a COMPLETELY DIFFERENT hardware path from SEL131/SEL132
 * which use mask/value paired writes to mixer setting registers.
 * ---------------------------------------------------------------- */

#define UA_DSP_CMD_BUS_PARAM	0x001D0004

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

/**
 * ua_dsp_set_bus_param - Send a bus coefficient to DSP via ring buffer
 * @ua: device (caller must hold ua->lock)
 * @bus_id: bus identifier (0x00-0x1F)
 * @sub_param: sub-parameter (0=main, 1=CUE1, 2=CUE2, 3=gainL, 4=gainR)
 * @value_u32: coefficient as IEEE 754 float packed in u32
 *
 * Returns 0 on success, negative errno on failure.
 */
int ua_dsp_set_bus_param(struct ua_device *ua, u32 bus_id,
			 u32 sub_param, u32 value_u32)
{
	struct ua_dsp_state *dsp0 = &ua->dsp[0];
	u32 bank_base = ua_dsp_base(0);
	u32 ring_base = bank_base + UA_RING_CMD_OFFSET;
	struct ua_ring_entry entry;
	int ret;

	if (!dsp0->rings_allocated)
		return -ENODEV;

	if (bus_id > 0x1f || sub_param > 6)
		return -EINVAL;

	entry = (struct ua_ring_entry){
		.word0 = cpu_to_le32(UA_DSP_CMD_BUS_PARAM),
		.word1 = cpu_to_le32(bus_id),
		.word2 = cpu_to_le32(sub_param),
		.word3 = cpu_to_le32(value_u32),
	};

	/*
	 * Circular ring: a slot is free again once the DSP has consumed
	 * it, so this only fails if the DSP has stopped consuming (1023
	 * entries outstanding) — not after ~1024 fader moves per boot.
	 */
	ret = ua_dsp_ring_put_entry(ua, &dsp0->cmd, &entry);
	if (ret) {
		dev_warn_ratelimited(&ua->pdev->dev,
				     "bus_param ring full (bus=%u sub=%u pos=%u wr=%u)\n",
				     bus_id, sub_param, dsp0->cmd.hw_pos,
				     dsp0->cmd.write_ptr);
		return ret;
	}

	ua_dsp_ring_doorbell(ua, &dsp0->cmd, ring_base);

	return 0;
}

/**
 * ua_dsp_ring_send_raw - Send an arbitrary 4-word inline entry to any DSP
 * @ua: device (caller must hold ua->lock)
 * @dsp_idx: target DSP (0-3)
 * @w0..w3: the four command words
 *
 * For testing ring buffer commands from userspace.
 */
int ua_dsp_ring_send_raw(struct ua_device *ua, unsigned int dsp_idx,
			  u32 w0, u32 w1, u32 w2, u32 w3)
{
	struct ua_dsp_state *ds;
	u32 ring_base;
	struct ua_ring_entry entry;
	int ret;

	if (dsp_idx >= ua->num_dsps)
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
 * Kext RE: CUAD2DeviceMixer::SetBusEnable sends ring buffer command
 * {0x001C0003, bus_idx, enable, 0} to EVERY DSP (not just DSP 0).
 * This enables audio routing paths through the DSP fabric.
 */
int ua_dsp_set_bus_enable(struct ua_device *ua, u32 bus_idx, int enable)
{
	unsigned int dsp;
	int ret, err = 0;

	if (bus_idx > 6)
		return -EINVAL;

	for (dsp = 0; dsp < ua->num_dsps; dsp++) {
		if (!ua->dsp[dsp].rings_allocated)
			continue;
		ret = ua_dsp_ring_send_raw(ua, dsp,
					   0x001C0003, bus_idx, enable, 0);
		if (ret) {
			dev_warn(&ua->pdev->dev,
				 "bus_enable ring full (dsp=%u bus=%u)\n",
				 dsp, bus_idx);
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
	dev_info(&ua->pdev->dev,
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
 * Returns 0 on success, negative errno on failure.
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
	 * Layout: 6 modules × (header at +0x00, I/O desc at +0x40)
	 * each in a 128-byte slot = 768 bytes total + zeros at offset 0.
	 */
	dma_page = dma_alloc_coherent(&ua->pdev->dev, UA_PCIE_PAGE_SIZE,
				      &dma_addr, GFP_KERNEL);
	if (!dma_page)
		return -ENOMEM;
	memset(dma_page, 0, UA_PCIE_PAGE_SIZE);

	/*
	 * Build DMA payloads for all 6 modules.
	 * Slot layout per module (128 bytes each):
	 *   +0x00: 9-dword header (36 bytes)
	 *   +0x40: I/O descriptor (up to 88 bytes = 22 dwords)
	 */
#define MOD_SLOT_SIZE	128
#define MOD_HDR_OFF	0x00
#define MOD_IO_OFF	0x40

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

	dev_info(&ua->pdev->dev,
		 "module_cfg: queued %u entries for %u modules\n",
		 ds->cmd.write_ptr, UA_MODULE_COUNT);

	/* Wait for DSP to process all entries */
	if (!ua_dsp_ring_wait_idle(ua, &ds->cmd, 500)) {
		dev_warn(&ua->pdev->dev,
			 "module_cfg: timeout (pos=%u target=%u)\n",
			 ds->cmd.hw_pos, ds->cmd.write_ptr);
		/* DMA refs into dma_page may still be fetched */
		ua_dsp_orphan_dma(ua, dma_page, dma_addr, UA_PCIE_PAGE_SIZE,
				  &ds->cmd, NULL);
		return 0;
	}
	dev_info(&ua->pdev->dev,
		 "module_cfg: DSP consumed all entries (pos=%u)\n",
		 ds->cmd.hw_pos);

out_free:
	dma_free_coherent(&ua->pdev->dev, UA_PCIE_PAGE_SIZE,
			  dma_page, dma_addr);
	return 0;
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
 * in userspace calls it).  With io_map=1 it is a no-op so the descriptor
 * the driver maps by stays the firmware's.
 * ---------------------------------------------------------------- */

#include "ua_routing.h"

/**
 * ua_dsp_send_routing - Write captured IO descriptors over the firmware's
 * @ua: device (must hold ua->lock)
 *
 * Writes the macOS-captured IO descriptor data (x4, x8p) to the SRAM
 * regions at 0xC1A4 (input) and 0xC2C4 (output).  No-op with io_map=1.
 *
 * Returns 0 on success, negative errno on failure.
 */
int ua_dsp_send_routing(struct ua_device *ua)
{
	const struct ua_routing_config *cfg;
	unsigned int i;

	if (ua_io_map_enabled()) {
		dev_info(&ua->pdev->dev,
			 "io_map=1: not writing captured IO descriptors (the firmware owns them)\n");
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

	dev_info(&ua->pdev->dev,
		 "IO descriptors written: input=%u words, output=%u words\n",
		 UA_IO_DESC_WORDS, UA_IO_DESC_WORDS);

	return 0;
}

/**
 * ua_dsp_load_programs - Send DSP audio routing programs to DSP 0
 *
 * Loads the mixer core, output routing, and capture routing programs
 * captured from macOS kext via DTrace (2026-03-18). These programs
 * tell the DSP how to route audio between preamp inputs, mix buses,
 * and DMA record/playback channels.
 *
 * Must be called after ACEFACE connect completes.
 * Uses ring buffer sendBlock with program command (0x0027012A),
 * NOT firmware load command (0x00120000).
 *
 * Command word from kext disassembly of SEL127 handler at 0x39158:
 *   MOVZ W1, #0x12A; MOVK W1, #0x27, LSL#16 → cmd = 0x0027012A
 *   W2 = 0x1 (param)
 */
/* Program load command from kext command table at 0x8aade.
 * Different from firmware load (0x00120000) — this is for
 * DSP audio routing programs ("Bill" blocks). */
#define UA_DSP_PROG_CMD   0x00360000
#define UA_DSP_PROG_PARAM 0x80040000

int ua_dsp_load_programs(struct ua_device *ua)
{
	unsigned int i;
	int ret;

	if (ua->device_type != UA_DEV_APOLLO_X4) {
		dev_info(&ua->pdev->dev,
			 "DSP programs: only x4 supported, skipping\n");
		return 0;
	}

	/*
	 * DSP 0 only.  macOS loads the same 5 "Bill" programs to all
	 * 4 SHARC DSPs, but Linux currently only firmware-loads DSP 0
	 * (ua_dsp_load_mixer_blocks / ua-apollo-mixer.bin). Submitting
	 * program blocks to DSPs 1-3 whose bootloaders are not yet
	 * firmware-loaded leaves ring state of unclear correctness —
	 * ship-safe behavior is to stay on the FW-ready DSP 0 until
	 * the DSP 1-3 FW load path exists.  Extend to all DSPs once
	 * that lands.
	 */
	dev_info(&ua->pdev->dev,
		 "loading %u programs for DSP 0\n", UA_X4_DSP0_NUM_PROGRAMS);

	for (i = 0; i < UA_X4_DSP0_NUM_PROGRAMS; i++) {
		const struct ua_dsp_program *prog = &ua_x4_dsp0_programs[i];

		ret = ua_dsp_send_block(ua, 0,
					UA_DSP_PROG_CMD, UA_DSP_PROG_PARAM,
					prog->data, prog->size);
		if (ret) {
			dev_err(&ua->pdev->dev,
				"DSP program %s failed: %d\n",
				prog->name, ret);
			return ret;
		}
		dev_info(&ua->pdev->dev,
			 "  DSP0: loaded %s (%u bytes)\n",
			 prog->name, prog->size);
	}

	dev_info(&ua->pdev->dev, "DSP audio programs loaded (DSP 0)\n");
	return 0;
}
