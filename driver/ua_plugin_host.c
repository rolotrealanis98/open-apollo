// SPDX-License-Identifier: GPL-2.0-only
/*
 * Universal Audio Apollo — plug-in host: parameters, caps, ioctl entry
 *
 * Copyright (c) 2026 apollo-linux contributors
 *
 * plugin_host=1 (default off) turns on UA_IOCTL_BOARD_QUERY and
 * UA_IOCTL_PLUGIN_INSTANCE (ua_ioctl.h, "Plug-in hosting").  With it off
 * both fail with -EOPNOTSUPP, GET_PLUGIN_CAPS reports no operation and
 * GET_VERSION's features word is what it was before ABI 1.8.
 */

#include <linux/errno.h>
#include <linux/module.h>
#include <linux/slab.h>
#include <linux/workqueue.h>

#include "ua_apollo.h"
#include "ua_plugin_host.h"

static bool plugin_host;
module_param(plugin_host, bool, 0444);
MODULE_PARM_DESC(plugin_host,
		 "EXPERIMENTAL: host UAD plug-ins on the Apollo's DSPs (UA_IOCTL_BOARD_QUERY, UA_IOCTL_PLUGIN_INSTANCE). A plug-in is placed only when the Apollo itself reports it authorized or in a running demo (its board table, read fresh for every instance), and only on a DSP this driver cold-started. Default: off");

bool ua_plugin_host_enabled(void)
{
	return plugin_host;
}

/* The host's work: strip passes (they wait for the DSP, off-lock) */
static void ua_plugin_work(struct work_struct *work)
{
	struct ua_plugin_host *h = container_of(to_delayed_work(work),
						struct ua_plugin_host, work);

	ua_strips_run(h->ua);
}

/**
 * ua_plugin_host_init - allocate the host state at probe
 * @ua: device (probe, exclusive)
 *
 * Always allocated: the epoch counter is kept even when nothing is hosted.
 * The input strips (input_strips=1) read their modules here.
 */
int ua_plugin_host_init(struct ua_device *ua)
{
	struct ua_plugin_host *h;

	h = kzalloc(sizeof(*h), GFP_KERNEL);
	if (!h)
		return -ENOMEM;
	h->ua = ua;
	mutex_init(&h->mutex);
	h->epoch = 1;
	h->active = plugin_host || ua_strips_wanted();
	INIT_LIST_HEAD(&h->xacts);
	INIT_LIST_HEAD(&h->catalog);
	INIT_LIST_HEAD(&h->instances);
	INIT_DELAYED_WORK(&h->work, ua_plugin_work);
	h->next_handle = 1;
	ua->host = h;
	return 0;
}

/* ua_chardev_release(): nothing can reach the host any more */
void ua_plugin_host_free(struct ua_device *ua)
{
	struct ua_plugin_host *h = ua->host;
	unsigned int i;

	if (!h)
		return;
	cancel_delayed_work_sync(&h->work);
	mutex_lock(&ua->lock);
	ua_strips_free(ua);
	for (i = 0; i < UA_HOST_DSPS; i++) {
		if (h->fence[i])
			ua_xact_done(ua, h->fence[i]);
		h->fence[i] = NULL;
		if (h->dsp[i].pools)
			ua_pool_owner_destroy(h->dsp[i].pools);
		h->dsp[i].pools = NULL;
	}
	mutex_unlock(&ua->lock);
	ua_catalog_free_all(ua);
	kfree(h);
	ua->host = NULL;
}

/**
 * ua_plugin_publish_now - publish a transaction built under ua->lock
 * @ua: device (ua->lock held)
 * @x: an inline-only transaction (no DMA payload, no replies)
 *
 * For teardowns, which run where the lock is already held and must not
 * wait.  The ring being full is not retried: -EBUSY.  On an error the
 * transaction is freed; on success the caller keeps it (a fence).
 */
int ua_plugin_publish_now(struct ua_device *ua, struct ua_xact *x)
{
	int ret;

	lockdep_assert_held(&ua->lock);
	if (x->len || x->n_rx) {
		ua_xact_discard(ua, x);
		return -EINVAL;
	}
	ret = ua_xact_map(ua, x);
	if (!ret)
		ret = ua_xact_submit(ua, x);
	if (ret) {
		ua_xact_discard(ua, x);
		return ret == -EAGAIN ? -EBUSY : ret;
	}
	return 0;
}

/**
 * ua_plugin_fence_set - @x is the latest teardown published on DSP @dsp
 * @ua: device (ua->lock held)
 * @dsp: DSP index
 * @x: published transaction (the host keeps it until it is proven)
 */
void ua_plugin_fence_set(struct ua_device *ua, unsigned int dsp,
			 struct ua_xact *x)
{
	struct ua_plugin_host *h = ua->host;

	lockdep_assert_held(&ua->lock);
	if (h->fence[dsp])
		ua_xact_done(ua, h->fence[dsp]);
	h->fence[dsp] = x;
}

/**
 * ua_plugin_fence_wait - prove DSP @dsp took its last teardown
 * @ua: device (host->mutex held, ua->lock NOT held)
 * @dsp: DSP index
 *
 * Waits up to UA_XACT_WAIT_MS with the lock released.  A teardown the
 * ring did not take, or that a resync abandoned, quarantines the DSP:
 * what it holds is no longer known.
 */
int ua_plugin_fence_wait(struct ua_device *ua, unsigned int dsp)
{
	struct ua_plugin_host *h = ua->host;
	struct ua_xact *x;
	int ret;

	lockdep_assert_held(&h->mutex);
	mutex_lock(&ua->lock);
	x = h->fence[dsp];
	mutex_unlock(&ua->lock);
	if (!x)
		return 0;
	ret = ua_xact_wait(ua, x, UA_XACT_WAIT_MS);
	mutex_lock(&ua->lock);
	if (ret && ret != -ENODEV)
		ua_plugin_quarantine(ua, dsp, "a teardown was not taken", ret);
	if (h->fence[dsp] == x) {
		h->fence[dsp] = NULL;
		ua_xact_done(ua, x);
	}
	mutex_unlock(&ua->lock);
	return ret;
}

/**
 * ua_plugin_teardown - take down everything the host placed, now
 * @ua: device (ua->lock held)
 * @why: for the log
 *
 * Before every disconnect, reconnect and rate change, and in the quiesce
 * of suspend, shutdown and remove.  Publishes the teardown (no wait: the
 * lock is held); the next operation on that DSP proves it first, or
 * quarantines the DSP (ua_plugin_fence_wait()).
 */
void ua_plugin_teardown(struct ua_device *ua, const char *why)
{
	lockdep_assert_held(&ua->lock);
	if (!ua->host || !ua->host->active)
		return;
	ua_instances_teardown(ua, why);
	ua_strips_teardown(ua, why);
}

/**
 * ua_plugin_host_quiesce - teardown before the device stops, and wait for it
 * @ua: device (no lock held)
 * @why: for the log
 * @present: the device can still take commands
 *
 * Stops the host's work (a strip pass in progress ends first), publishes
 * the teardown and waits for each DSP to take it with the lock released.
 * With the device gone nothing is sent and nothing is owned any more.
 */
void ua_plugin_host_quiesce(struct ua_device *ua, const char *why,
			    bool present)
{
	struct ua_plugin_host *h = ua->host;
	unsigned int i;

	if (!h)
		return;
	cancel_delayed_work_sync(&h->work);
	if (!h->active)
		return;
	mutex_lock(&h->mutex);
	mutex_lock(&ua->lock);
	if (present && !atomic_read(&ua->shutdown)) {
		ua_plugin_teardown(ua, why);
	} else {
		for (i = 0; i < UA_HOST_DSPS; i++)
			if (h->dsp[i].owned)
				ua_plugin_disown(ua, i, "the device is gone");
		ua_plugin_epoch_bump(ua, why);
	}
	mutex_unlock(&ua->lock);
	for (i = 0; i < ua->num_dsps && i < UA_HOST_DSPS; i++)
		(void)ua_plugin_fence_wait(ua, i);
	mutex_unlock(&h->mutex);
}

/**
 * ua_plugin_device_lost - the device's state is gone (a PCIe error)
 * @ua: device (no lock held; the shutdown flag is already set)
 * @why: for the log
 *
 * Nothing can be sent and nothing on the DSPs can be trusted: the strips
 * and instances are forgotten without a command, every DSP is disowned
 * until the next cold start, held fences are dropped (their buffers stay
 * with the transactions list until the rings prove them, or fini).
 */
void ua_plugin_device_lost(struct ua_device *ua, const char *why)
{
	struct ua_plugin_host *h = ua->host;
	unsigned int i;

	if (!h)
		return;
	cancel_delayed_work_sync(&h->work);
	if (!h->active)
		return;
	mutex_lock(&ua->lock);
	ua_strips_lost(ua);
	for (i = 0; i < UA_HOST_DSPS; i++) {
		if (h->fence[i])
			ua_xact_done(ua, h->fence[i]);
		h->fence[i] = NULL;
		if (h->dsp[i].owned)
			ua_plugin_disown(ua, i, why);
	}
	ua_plugin_epoch_bump(ua, why);
	mutex_unlock(&ua->lock);
}

/* The DSP service loop's share (ua->lock held, every 10 ms) */
void ua_plugin_service(struct ua_device *ua)
{
	if (ua->host && ua->host->active)
		ua_strips_service(ua);
}

/**
 * ua_plugin_connected - the audio connect is done
 * @ua: device (ua->lock held)
 *
 * Reads the pool registers of every DSP the host owns into its ledger
 * (reads only).
 */
void ua_plugin_connected(struct ua_device *ua)
{
	struct ua_plugin_host *h = ua->host;
	unsigned int i;

	lockdep_assert_held(&ua->lock);
	if (!h || !h->active)
		return;
	for (i = 0; i < ua->num_dsps && i < UA_HOST_DSPS; i++)
		if (h->dsp[i].owned)
			(void)ua_plugin_read_geometry(ua, i);
	ua_strips_connected(ua);
}

/**
 * ua_plugin_epoch_bump - what the host knows about the DSPs is over
 * @ua: device (ua->lock held, or exclusive)
 * @why: for the log
 *
 * Every transaction built before fails to publish (-ESTALE) and one still
 * out reports -ESTALE.  The epoch is never 0.
 */
void ua_plugin_epoch_bump(struct ua_device *ua, const char *why)
{
	struct ua_plugin_host *h = ua->host;

	if (!h)
		return;
	if (!++h->epoch)
		h->epoch = 1;
	if (h->active)
		dev_dbg(&ua->pdev->dev, "plug-in host: epoch %llu (%s)\n",
			(unsigned long long)h->epoch, why);
}

/**
 * ua_plugin_quarantine - DSP @dsp did something nobody can prove
 * @ua: device (ua->lock held)
 * @dsp: DSP index
 * @why: for the log
 * @err: the error that ended it
 *
 * Nothing more is placed on that DSP until its next cold start (no reply
 * in time, an entry a resync abandoned, a teardown the ring did not take).
 */
void ua_plugin_quarantine(struct ua_device *ua, unsigned int dsp,
			  const char *why, int err)
{
	struct ua_plugin_host *h = ua->host;

	lockdep_assert_held(&ua->lock);
	if (!h || dsp >= UA_HOST_DSPS)
		return;
	if (!h->dsp[dsp].quarantined)
		dev_warn(&ua->pdev->dev,
			 "plug-in host: DSP %u quarantined until the next cold start: %s (%d)\n",
			 dsp, why, err);
	h->dsp[dsp].quarantined = true;
}

/**
 * ua_plugin_host_ops - UA_PLUGIN_OP_* accepted right now
 * @ua: device (ua->lock held)
 *
 * A bit only for an operation that would be accepted now: none with
 * plugin_host=0; BOARD_QUERY while DSP 0's rings run on a connected
 * device; CREATE when, on top of that, a DSP is the host's to place on;
 * DESTROY always (it never needs the device).
 */
u64 ua_plugin_host_ops(struct ua_device *ua)
{
	struct ua_dsp_state *ds = &ua->dsp[0];
	u64 ops;
	unsigned int i;

	lockdep_assert_held(&ua->lock);
	if (!plugin_host || !ua->host)
		return 0;
	ops = UA_PLUGIN_OP_DESTROY;
	if (atomic_read(&ua->shutdown) || !ua->num_dsps || !ua->audio.connected ||
	    !ds->rings_allocated || !ds->cmd.live || !ds->resp.live)
		return ops;
	ops |= UA_PLUGIN_OP_BOARD_QUERY;
	for (i = 0; i < ua->num_dsps && i < UA_HOST_DSPS; i++)
		if (ua_plugin_dsp_admitted(ua, i))
			return ops | UA_PLUGIN_OP_CREATE | UA_PLUGIN_OP_SERVICE |
			       UA_PLUGIN_OP_SWAP | UA_PLUGIN_OP_UNISON_PARAMS |
                       UA_PLUGIN_OP_BEGIN_UNISON | UA_PLUGIN_OP_ACTIVATE_UNISON |
			       UA_PLUGIN_OP_START;
	return ops;
}

/* GET_PLUGIN_CAPS' host fields (ua->lock held) */
void ua_plugin_host_caps(struct ua_device *ua, u32 *state, u32 *owned,
			 u32 *quarantined, u32 *instances)
{
	struct ua_plugin_host *h = ua->host;
	unsigned int i;

	lockdep_assert_held(&ua->lock);
	*owned = *quarantined = *instances = 0;
	if (plugin_host)
		*state |= UA_PLUGIN_STATE_HOST;
	if (!h)
		return;
	if (ua_strips_live(ua))
		*state |= UA_PLUGIN_STATE_STRIPS;
	for (i = 0; i < ua->num_dsps && i < UA_HOST_DSPS; i++) {
		if (h->dsp[i].owned)
			*owned |= BIT(i);
		if (h->dsp[i].quarantined)
			*quarantined |= BIT(i);
	}
	*instances = h->n_instances;
}
