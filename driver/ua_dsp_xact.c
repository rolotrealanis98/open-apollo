// SPDX-License-Identifier: GPL-2.0-only
/*
 * Universal Audio Apollo — plug-in host transactions on the DSP rings
 *
 * Copyright (c) 2026 apollo-linux contributors
 *
 * Every command the plug-in host sends (a module upload, a zero-fill, a
 * memspec table, a parameter page, a board query, a teardown) is one
 * transaction: a batch of command-ring entries, the DMA pages they point
 * at, and the reply descriptors the DSP fills, in one coherent buffer the
 * driver allocated.  What the withdrawn overlay got wrong, and how this
 * does it (claude-review-20261011.md):
 *
 *  - Replies are queued before the command (the kext's _sendBlock order):
 *    the reply descriptors go on the response ring and are doorbelled
 *    first, then the command entries.  A transaction with replies is
 *    published only on an idle command ring and an empty response ring
 *    (a reply descriptor left over by someone else is abandoned first, as
 *    ua_dsp_send_block() does), so the DSP's next replies are ours.
 *  - ua->lock covers publication only.  Nothing here sleeps with it held:
 *    ua_xact_wait() takes it for one POSITION read at a time.  The bound is
 *    1 s (UA_XACT_WAIT_MS), not the 6 s polls the overlay held the lock
 *    across (finding 5).
 *  - The buffer outlives its caller: a transaction the rings have not
 *    provably passed when the caller is done (a timeout, an epoch change)
 *    stays on the host's own list, bounded by UA_XACT_RETAIN_MAX/_BYTES.
 *    The firmware/PCM orphan list is never used, nothing global latches
 *    (findings 4 and 15): at the cap only new transactions are refused.
 *  - A ring resync that abandons our entries settles them the way the
 *    orphans are settled (ua_xact_ring_abandon(), from ua_dsp.c).
 *  - Requests live on the heap (finding 9).
 *  - Every transaction is bound to the host epoch it was built in; one that
 *    changes while it is out makes the outcome unknown (-ESTALE).
 * An outcome that cannot be proven (no reply in time, an abandoned entry)
 * quarantines the DSP until its next cold start (ua_xact_run()).
 */

#include <linux/delay.h>
#include <linux/dma-mapping.h>
#include <linux/errno.h>
#include <linux/jiffies.h>
#include <linux/mm.h>
#include <linux/sched/signal.h>
#include <linux/slab.h>
#include <linux/string.h>

#include "ua_apollo.h"
#include "ua_plugin_host.h"

/* UA_XACT_BEGIN: the host tests build everything up to the end mark */

/**
 * ua_xact_alloc - an empty transaction for one DSP
 * @dsp: DSP index
 * @payload_cap: bytes of DMA payload it may carry (at most
 *               UA_XACT_PAYLOAD_MAX; 0 for inline commands only)
 */
struct ua_xact *ua_xact_alloc(unsigned int dsp, size_t payload_cap)
{
	struct ua_xact *x;

	if (payload_cap > UA_XACT_PAYLOAD_MAX)
		return NULL;
	x = kzalloc(sizeof(*x), GFP_KERNEL);
	if (!x)
		return NULL;
	INIT_LIST_HEAD(&x->node);
	x->dsp = dsp;
	x->cap = payload_cap;
	if (payload_cap) {
		x->stage = kvzalloc(payload_cap, GFP_KERNEL);
		if (!x->stage) {
			kfree(x);
			return NULL;
		}
	}
	return x;
}

static void ua_xact_add(struct ua_xact *x, u32 w0, u32 w1, u32 w2, u32 w3)
{
	if (x->err)
		return;
	if (x->state != UA_XACT_BUILDING) {
		x->err = -EINVAL;
		return;
	}
	if (x->n >= UA_XACT_ENTS_MAX) {
		x->err = -E2BIG;
		return;
	}
	x->ent[x->n][0] = w0;
	x->ent[x->n][1] = w1;
	x->ent[x->n][2] = w2;
	x->ent[x->n][3] = w3;
	x->n++;
}

/* An inline command (bit 31 of @w0 clear: never a DMA reference) */
void ua_xact_inline(struct ua_xact *x, u32 w0, u32 w1, u32 w2, u32 w3)
{
	if (w0 & UA_RING_DMA_REF) {
		if (!x->err)
			x->err = -EINVAL;
		return;
	}
	ua_xact_add(x, w0, w1, w2, w3);
}

/**
 * ua_xact_cmd - room for one DSP command carried by DMA pages
 * @x: transaction
 * @dwords: command length, header words included
 *
 * A command of at most 4 KiB starts on the next page rather than straddle
 * one, so it goes out as a single DMA_REF (the kext sends every code and
 * parameter page as one DMA_REF of up to 0x400 dwords; 1 KiB pieces took
 * the x4 off the bus on titan, 2026-10-11).  A longer one is split at the
 * page boundaries, never finer.  Returns where to write the command
 * (little endian), or NULL with x->err set.
 */
__le32 *ua_xact_cmd(struct ua_xact *x, unsigned int dwords)
{
	size_t bytes = (size_t)dwords * 4, off, piece, at, left;

	if (x->err)
		return NULL;
	if (x->state != UA_XACT_BUILDING || !dwords || dwords > 0xffff) {
		x->err = -EINVAL;
		return NULL;
	}
	off = x->len;
	if (bytes <= UA_XACT_PIECE &&
	    (off % UA_XACT_PIECE) + bytes > UA_XACT_PIECE)
		off += UA_XACT_PIECE - off % UA_XACT_PIECE;
	if (off + bytes > x->cap) {
		x->err = -ENOSPC;
		return NULL;
	}
	for (at = off, left = bytes; left; at += piece, left -= piece) {
		piece = min_t(size_t, left, UA_XACT_PIECE - at % UA_XACT_PIECE);
		ua_xact_add(x, UA_RING_DMA_REF | (u32)(piece / 4), 0, (u32)at, 0);
	}
	if (x->err)
		return NULL;
	x->len = off + bytes;
	return (__le32 *)(x->stage + off);
}

/**
 * ua_xact_rx - a reply descriptor of @dwords
 * @x: transaction
 * @dwords: 1..UA_XACT_RX_WORDS_MAX
 *
 * Replies fill in the order they are added.  Returns the reply's index.
 */
int ua_xact_rx(struct ua_xact *x, unsigned int dwords)
{
	if (x->err)
		return x->err;
	if (x->state != UA_XACT_BUILDING || !dwords ||
	    dwords > UA_XACT_RX_WORDS_MAX) {
		x->err = -EINVAL;
		return x->err;
	}
	if (x->n_rx >= UA_XACT_RX_MAX) {
		x->err = -E2BIG;
		return x->err;
	}
	/* Each reply in its own 16-byte-aligned slot, never across a page */
	if (x->rx_len % UA_XACT_PIECE + (size_t)dwords * 4 > UA_XACT_PIECE)
		x->rx_len += UA_XACT_PIECE - x->rx_len % UA_XACT_PIECE;
	x->rx_off[x->n_rx] = (u32)x->rx_len;
	x->rx_words[x->n_rx] = dwords;
	x->rx_len += ALIGN((size_t)dwords * 4, 16);
	return x->n_rx++;
}

/**
 * ua_xact_map - the transaction's coherent buffer
 * @ua: device (no lock needed: nothing is published)
 * @x: a transaction laid out without error
 *
 * One buffer: the payload pages, then the reply area on its own pages,
 * filled with x->rx_fill (0 unless the caller set a sentinel).
 */
int ua_xact_map(struct ua_device *ua, struct ua_xact *x)
{
	size_t pay = PAGE_ALIGN(x->len), rx = PAGE_ALIGN(x->rx_len);
	unsigned int i;
	u32 *w;

	if (x->err)
		return x->err;
	if (x->state != UA_XACT_BUILDING || (!x->n && !x->n_rx))
		return -EINVAL;
	x->size = pay + rx;
	if (x->size) {
		x->cpu = dma_alloc_coherent(&ua->pdev->dev, x->size, &x->dma,
					    GFP_KERNEL);
		if (!x->cpu)
			return -ENOMEM;
		memset(x->cpu, 0, x->size);
		if (x->len)
			memcpy(x->cpu, x->stage, x->len);
		w = (u32 *)((u8 *)x->cpu + pay);
		for (i = 0; i < rx / 4; i++)
			w[i] = cpu_to_le32(x->rx_fill);
	}
	x->rx_base = pay;
	kvfree(x->stage);
	x->stage = NULL;
	x->state = UA_XACT_MAPPED;
	return 0;
}

/* Free a transaction the device never saw */
static void ua_xact_free(struct ua_device *ua, struct ua_xact *x)
{
	if (x->cpu)
		dma_free_coherent(&ua->pdev->dev, x->size, x->cpu, x->dma);
	kvfree(x->stage);
	kfree(x);
}

/* Unpublished (building or mapped): just free it.  No lock needed. */
void ua_xact_discard(struct ua_device *ua, struct ua_xact *x)
{
	if (!x)
		return;
	if (WARN_ON_ONCE(x->state >= UA_XACT_PUBLISHED))
		return;
	ua_xact_free(ua, x);
}

/**
 * ua_xact_submit - publish a mapped transaction
 * @ua: device (ua->lock held)
 * @x: mapped transaction
 *
 * All or nothing: every entry fits or none is queued (the producers are
 * rolled back, nothing was doorbelled).  Replies first, then the command.
 * Returns 0 (published: from now on the buffer is the device's until the
 * rings pass it), -EAGAIN (no room, or a reply transaction found the
 * command ring busy: retry off-lock), -ESTALE (the epoch moved on), -EBUSY
 * (the retention list is full, or a reply is being waited for), -ENODEV,
 * -EINVAL.
 */
int ua_xact_submit(struct ua_device *ua, struct ua_xact *x)
{
	struct ua_plugin_host *h = ua->host;
	struct ua_dsp_state *ds;
	unsigned int i, cmd_start, resp_start;
	int ret;

	lockdep_assert_held(&ua->lock);
	if (x->state != UA_XACT_MAPPED)
		return -EINVAL;
	if (atomic_read(&ua->shutdown))
		return -ENODEV;
	if (x->epoch != h->epoch)
		return -ESTALE;
	if (x->dsp >= ua->num_dsps || x->dsp >= UA_HOST_DSPS)
		return -EINVAL;
	ds = &ua->dsp[x->dsp];
	if (!ds->rings_allocated || !ds->cmd.live || !ds->cmd.capacity ||
	    (x->n_rx && (!ds->resp.live || !ds->resp.capacity)))
		return -ENODEV;
	if (x->n > ds->cmd.capacity / 4 || x->n_rx > ds->resp.capacity / 4)
		return -E2BIG;

	ua_xact_reclaim(ua);
	if (h->xact_count >= UA_XACT_RETAIN_MAX ||
	    h->xact_bytes + x->size > UA_XACT_RETAIN_BYTES)
		return -EBUSY;

	ret = ua_dsp_ring_update_pos(ua, &ds->cmd);
	if (ret)
		return ret;
	if (ua_dsp_ring_free_slots(&ds->cmd) < x->n)
		return -EAGAIN;
	if (x->n_rx) {
		if (h->xact_waiting)
			return -EBUSY;
		/* Nothing queued ahead of us may answer into our replies */
		if (ua_dsp_ring_pending(&ds->cmd))
			return -EAGAIN;
		ret = ua_dsp_ring_update_pos(ua, &ds->resp);
		if (ret)
			return ret;
		if (ua_dsp_ring_pending(&ds->resp)) {
			/*
			 * A reply descriptor someone published and nobody got
			 * an answer in (its buffer is parked by its owner):
			 * abandon it, as ua_dsp_send_block() does, without a
			 * drain wait.
			 */
			ret = ua_dsp_ring_resync_now(ua, &ds->resp);
			if (ret)
				return ret;
			if (ua_dsp_ring_pending(&ds->resp))
				return -EAGAIN;
		}
		if (ua_dsp_ring_free_slots(&ds->resp) < x->n_rx)
			return -EAGAIN;
	}

	cmd_start = ds->cmd.write_ptr;
	resp_start = ds->resp.write_ptr;
	for (i = 0; i < x->n_rx; i++) {
		dma_addr_t at = x->dma + x->rx_base + x->rx_off[i];

		ret = ua_dsp_ring_put_words(ua, &ds->resp,
					    UA_RING_DMA_REF | x->rx_words[i], 0,
					    lower_32_bits(at), upper_32_bits(at));
		if (ret)
			goto unwind;
	}
	for (i = 0; i < x->n; i++) {
		u32 w2 = x->ent[i][2], w3 = x->ent[i][3];

		if (x->ent[i][0] & UA_RING_DMA_REF) {
			dma_addr_t at = x->dma + x->ent[i][2];

			w2 = lower_32_bits(at);
			w3 = upper_32_bits(at);
		}
		ret = ua_dsp_ring_put_words(ua, &ds->cmd, x->ent[i][0],
					    x->ent[i][1], w2, w3);
		if (ret)
			goto unwind;
	}

	/* The payload and the reply area are in place before any doorbell */
	dma_wmb();
	if (x->n_rx) {
		ua_dsp_ring_doorbell(ua, &ds->resp, ds->resp.reg_base);
		x->ref[1] = (struct ua_xact_ref){
			.ring = &ds->resp, .gen = ds->resp.gen,
			.end = ds->resp.write_ptr };
	}
	if (x->n) {
		ua_dsp_ring_doorbell(ua, &ds->cmd, ds->cmd.reg_base);
		x->ref[0] = (struct ua_xact_ref){
			.ring = &ds->cmd, .gen = ds->cmd.gen,
			.end = ds->cmd.write_ptr };
	}
	x->state = UA_XACT_PUBLISHED;
	x->published = jiffies;
	list_add_tail(&x->node, &h->xacts);
	h->xact_count++;
	h->xact_bytes += x->size;
	h->dsp[x->dsp].tx_xacts++;
	if (x->n_rx) {
		x->waiting = true;
		h->xact_waiting++;
	}
	return 0;

unwind:
	/* Nothing was doorbelled: the device never saw these slots */
	ds->cmd.write_ptr = cmd_start;
	ds->resp.write_ptr = resp_start;
	return ret == -ENOSPC ? -EAGAIN : ret;
}

/* Is @ref's ring past our entries?  Re-reads POSITION. */
static bool ua_xact_ref_done(struct ua_device *ua, struct ua_xact_ref *ref)
{
	struct ua_dsp_ring *ring = ref->ring;
	unsigned int mask, d_end;

	if (!ring)
		return true;
	if (!ring->capacity || ring->gen != ref->gen)
		return false;		/* held until fini */
	if (ua_dsp_ring_update_pos(ua, ring))
		return false;
	mask = ring->capacity - 1;
	if (ref->abandoned)
		return ((ring->hw_pos - ref->start) & mask) >= ref->need;
	d_end = (ref->end - ring->hw_pos) & mask;
	return !d_end || d_end > ua_dsp_ring_pending(ring);
}

static void ua_xact_settle(struct ua_device *ua, struct ua_xact *x)
{
	unsigned int i;

	for (i = 0; i < 2; i++)
		if (ua_xact_ref_done(ua, &x->ref[i]))
			x->ref[i].ring = NULL;
}

/* The reply wait is over (retired, given up or released) */
static void ua_xact_unwait(struct ua_plugin_host *h, struct ua_xact *x)
{
	if (x->waiting) {
		x->waiting = false;
		if (!WARN_ON_ONCE(!h->xact_waiting))
			h->xact_waiting--;
	}
}

/**
 * ua_xact_poll - has the device taken the transaction?
 * @ua: device (ua->lock held)
 * @x: published transaction
 *
 * Returns 1 when both rings are past it (its replies are then complete and
 * readable), 0 while it is still queued, -ESTALE when a resync abandoned
 * part of it or the epoch moved on (whether the device ran it is unknown),
 * -ENODEV when the device is gone.
 */
int ua_xact_poll(struct ua_device *ua, struct ua_xact *x)
{
	struct ua_plugin_host *h = ua->host;
	unsigned int i;

	lockdep_assert_held(&ua->lock);
	if (x->state == UA_XACT_RETIRED)
		return 1;
	if (x->state != UA_XACT_PUBLISHED)
		return -EINVAL;
	if (atomic_read(&ua->shutdown))
		return -ENODEV;
	for (i = 0; i < 2; i++) {
		struct ua_xact_ref *ref = &x->ref[i];

		if (!ref->ring)
			continue;
		if (!ref->ring->capacity)
			return -ENODEV;
		if (ref->abandoned || ref->ring->gen != ref->gen)
			return -ESTALE;
	}
	if (x->epoch != h->epoch)
		return -ESTALE;
	ua_xact_settle(ua, x);
	if (x->ref[0].ring || x->ref[1].ring)
		return 0;
	/* POSITION past the reply descriptors: their words are written */
	dma_rmb();
	ua_xact_unwait(h, x);
	x->state = UA_XACT_RETIRED;
	return 1;
}

/**
 * ua_xact_publish - ua_xact_submit(), retried off-lock for a moment
 * @ua: device (ua->lock NOT held)
 * @x: mapped transaction
 *
 * A full ring or a busy command ring is retried for UA_XACT_PUBLISH_MS
 * with the lock released between tries.
 */
int ua_xact_publish(struct ua_device *ua, struct ua_xact *x)
{
	unsigned long until = jiffies + msecs_to_jiffies(UA_XACT_PUBLISH_MS);
	int ret;

	for (;;) {
		mutex_lock(&ua->lock);
		ret = ua_xact_submit(ua, x);
		mutex_unlock(&ua->lock);
		if (ret != -EAGAIN || time_after(jiffies, until))
			return ret == -EAGAIN ? -EBUSY : ret;
		usleep_range(1000, 1500);
	}
}

/**
 * ua_xact_wait - wait for a published transaction, the lock released
 * @ua: device (ua->lock NOT held)
 * @x: published transaction
 * @timeout_ms: upper bound
 *
 * ua->lock is taken for each POSITION read only.  Returns what
 * ua_xact_poll() said last: 0 is turned into -ETIMEDOUT.
 */
int ua_xact_wait(struct ua_device *ua, struct ua_xact *x,
		 unsigned int timeout_ms)
{
	unsigned long until = jiffies + msecs_to_jiffies(timeout_ms);
	int ret;

	for (;;) {
		mutex_lock(&ua->lock);
		ret = ua_xact_poll(ua, x);
		mutex_unlock(&ua->lock);
		if (ret)
			return ret < 0 ? ret : 0;
		if (time_after(jiffies, until) || fatal_signal_pending(current))
			return -ETIMEDOUT;
		usleep_range(1000, 1500);
	}
}

/**
 * ua_xact_run - map, publish and wait; quarantine on an unknown outcome
 * @ua: device (host->mutex held, ua->lock NOT held)
 * @x: laid-out transaction
 * @timeout_ms: wait bound
 *
 * 0: retired, replies readable until ua_xact_done().  An error before
 * publication changed nothing on the device.  After publication a timeout
 * or an abandoned entry means nobody knows what the DSP did: its pools are
 * no longer this host's to hand out until the next cold start.
 * The caller still owns @x and ends with ua_xact_done().
 */
int ua_xact_run(struct ua_device *ua, struct ua_xact *x,
		unsigned int timeout_ms)
{
	int ret;

	lockdep_assert_held(&ua->host->mutex);
	ret = ua_xact_map(ua, x);
	if (!ret)
		ret = ua_xact_publish(ua, x);
	if (ret)
		return ret;
	ret = ua_xact_wait(ua, x, timeout_ms);
	if (ret && ret != -ENODEV) {
		mutex_lock(&ua->lock);
		ua->host->dsp[x->dsp].timeouts++;
		ua_plugin_quarantine(ua, x->dsp, "a transaction ended unproven",
				     ret);
		mutex_unlock(&ua->lock);
	}
	return ret;
}

/* Reply @i of a retired transaction (little endian), or NULL */
const __le32 *ua_xact_reply(const struct ua_xact *x, unsigned int i)
{
	if (x->state != UA_XACT_RETIRED || i >= x->n_rx)
		return NULL;
	return (const __le32 *)((const u8 *)x->cpu + x->rx_base + x->rx_off[i]);
}

static void ua_xact_unlink_free(struct ua_device *ua, struct ua_xact *x)
{
	struct ua_plugin_host *h = ua->host;

	list_del(&x->node);
	h->xact_count--;
	h->xact_bytes -= x->size;
	ua_xact_free(ua, x);
}

/**
 * ua_xact_done - the caller is done with @x
 * @ua: device (ua->lock held)
 * @x: any transaction (NULL is fine)
 *
 * Freed now when the device never saw it or both rings are past it;
 * otherwise it stays on the host's list until they are (ua_xact_reclaim()).
 */
void ua_xact_done(struct ua_device *ua, struct ua_xact *x)
{
	lockdep_assert_held(&ua->lock);
	if (!x)
		return;
	if (x->state < UA_XACT_PUBLISHED) {
		ua_xact_free(ua, x);
		return;
	}
	if (x->detached) {
		/* ua_xact_fini() kept its buffer for the device: struct only */
		kfree(x);
		return;
	}
	ua_xact_unwait(ua->host, x);
	x->released = true;
	ua_xact_settle(ua, x);
	if (!x->ref[0].ring && !x->ref[1].ring)
		ua_xact_unlink_free(ua, x);
}

/* Free every released transaction both rings are provably past (ua->lock) */
void ua_xact_reclaim(struct ua_device *ua)
{
	struct ua_plugin_host *h = ua->host;
	struct ua_xact *x, *tmp;

	lockdep_assert_held(&ua->lock);
	list_for_each_entry_safe(x, tmp, &h->xacts, node) {
		if (!x->released)
			continue;
		ua_xact_settle(ua, x);
		if (!x->ref[0].ring && !x->ref[1].ring)
			ua_xact_unlink_free(ua, x);
	}
}

/**
 * ua_xact_ring_abandon - @ring is resynced with entries still queued
 * @ua: device (ua->lock held)
 * @ring: the ring; hw_pos is the resync point, write_ptr the old producer
 * @next: the generation the ring starts
 *
 * Called by ua_dsp.c's orphan settlement (ua_dsp_orphans_abandon()), the
 * same rule: a reference whose entries were already consumed is done;
 * otherwise it is done once the ring has consumed, past the resync point,
 * as many entries as were abandoned up to its end.
 */
void ua_xact_ring_abandon(struct ua_device *ua, struct ua_dsp_ring *ring,
			  u32 next)
{
	struct ua_plugin_host *h = ua->host;
	unsigned int mask = ring->capacity - 1, i, d_end;
	struct ua_xact *x;

	if (!h)
		return;
	list_for_each_entry(x, &h->xacts, node) {
		for (i = 0; i < 2; i++) {
			struct ua_xact_ref *ref = &x->ref[i];

			if (ref->ring != ring || ref->gen != ring->gen)
				continue;
			if (ref->abandoned) {
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
}

/**
 * ua_xact_fini - the rings are being freed
 * @ua: device (exclusive: remove, or a failed probe)
 * @dma_quiet: the device can no longer write anywhere
 *
 * A transaction the rings never proved done may still be the target of a
 * descriptor the FPGA fetched: leaked unless the device is quiet, as the
 * orphans are.  One nobody released (a waiter that should not exist here)
 * is leaked too.
 */
void ua_xact_fini(struct ua_device *ua, bool dma_quiet)
{
	struct ua_plugin_host *h = ua->host;
	struct ua_xact *x, *tmp;
	unsigned int leaked = 0;

	if (!h)
		return;
	list_for_each_entry_safe(x, tmp, &h->xacts, node) {
		bool busy = x->ref[0].ring || x->ref[1].ring;

		if (x->released && (dma_quiet || !busy)) {
			ua_xact_unlink_free(ua, x);
			continue;
		}
		list_del_init(&x->node);
		h->xact_count--;
		h->xact_bytes -= x->size;
		if (dma_quiet || !busy) {
			/* not released yet: the buffer can go, the struct waits */
			dma_free_coherent(&ua->pdev->dev, x->size, x->cpu, x->dma);
		} else {
			leaked++;
		}
		x->cpu = NULL;
		x->detached = true;
		if (x->released)
			kfree(x);
	}
	h->xact_waiting = 0;
	h->xact_leaked += leaked;
	if (leaked)
		dev_warn(&ua->pdev->dev,
			 "plug-in host: leaking %u transaction buffer(s) a DSP ring may still use\n",
			 leaked);
}

/* UA_XACT_END */
