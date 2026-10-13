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

/**
 * ua_plugin_host_init - allocate the host state at probe
 * @ua: device (probe, exclusive)
 *
 * Always allocated: the epoch counter is kept even when nothing is hosted.
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
	h->active = plugin_host;
	INIT_LIST_HEAD(&h->xacts);
	INIT_LIST_HEAD(&h->catalog);
	INIT_LIST_HEAD(&h->instances);
	h->next_handle = 1;
	ua->host = h;
	return 0;
}

/* ua_chardev_release(): nothing can reach the host any more */
void ua_plugin_host_free(struct ua_device *ua)
{
	struct ua_plugin_host *h = ua->host;

	if (!h)
		return;
	kfree(h);
	ua->host = NULL;
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
 * A bit is reported only for an operation that would be accepted: no
 * operation exists yet in this build, so this is 0.
 */
u64 ua_plugin_host_ops(struct ua_device *ua)
{
	lockdep_assert_held(&ua->lock);
	return 0;
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
	for (i = 0; i < ua->num_dsps && i < UA_HOST_DSPS; i++) {
		if (h->dsp[i].owned)
			*owned |= BIT(i);
		if (h->dsp[i].quarantined)
			*quarantined |= BIT(i);
	}
	*instances = h->n_instances;
}

long ua_plugin_ioctl_board_query(struct ua_device *ua, unsigned long arg)
{
	(void)ua;
	(void)arg;
	return -EOPNOTSUPP;
}

long ua_plugin_ioctl_instance(struct ua_device *ua, struct ua_file *uf,
			      unsigned long arg)
{
	(void)ua;
	(void)uf;
	(void)arg;
	return -EOPNOTSUPP;
}
