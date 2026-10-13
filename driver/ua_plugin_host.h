/* SPDX-License-Identifier: GPL-2.0-only */
/*
 * Universal Audio Apollo — plug-in host: shared definitions
 *
 * Copyright (c) 2026 apollo-linux contributors
 *
 * The plug-in host is everything that places resources on the DSPs: the
 * Console input strips on DSP 0 (input_strips=1) and plug-in instances
 * (plugin_host=1).  Both are off by default; with both off the host only
 * keeps an epoch counter and answers caps with nothing.
 *
 * Locking: host->mutex serialises the long operations (a strip pass, a
 * plug-in CREATE or DESTROY, a board query); it is taken before ua->lock,
 * never while holding it.  ua->lock covers ring publication and every
 * field marked "ua->lock" below, and is never held across a wait for the
 * device: a transaction is published under ua->lock and waited for with
 * the lock released (ua_xact_wait()).
 */
#ifndef UA_PLUGIN_HOST_H
#define UA_PLUGIN_HOST_H

#include <linux/list.h>
#include <linux/mutex.h>
#include <linux/types.h>
#include <linux/workqueue.h>

#include "ua_resource_pool.h"

struct ua_device;
struct ua_file;
struct snd_info_buffer;

#define UA_HOST_DSPS		8	/* UA_MAX_DSPS */

/* ---- module parameters (ua_plugin_host.c) ---- */
bool ua_plugin_host_enabled(void);	/* plugin_host=1 */

/* ---- per-DSP ownership ---- */
struct ua_host_dsp {
	bool owned;		/* ua->lock: see ua_plugin_dsp_start() */
	bool quarantined;	/* ua->lock: an outcome that cannot be proven */
	bool geom_read;		/* the pool registers were read this epoch */
	u32 boot_word;		/* at the last cold start */
	char why[96];		/* the last ownership decision */
	u32 regs[11];		/* pool registers: size 0-3, base 0-3, scratch 1-3 */
	struct ua_pool_geometry geom[UA_POOLS];
	struct ua_pool_owner *pools;	/* owned and geometry read */
	/* traffic since probe (proc): what this host sent to the DSP */
	u32 tx_xacts, uploads, replies_ok, replies_bad, zero_fills;
	u32 memspecs, writes, releases, stops, timeouts;
};

struct ua_plugin_host {
	struct ua_device *ua;
	struct mutex mutex;		/* long operations; before ua->lock */
	u64 epoch;			/* ua->lock; never 0 */
	bool active;			/* plugin_host or input_strips */
	struct ua_host_dsp dsp[UA_HOST_DSPS];

	/* ua_dsp_xact.c: published transactions not yet retired (ua->lock) */
	struct list_head xacts;
	unsigned int xact_count;
	size_t xact_bytes;
	unsigned int xact_waiting;	/* published with replies, being waited */
	u32 xact_leaked;

	/* ua_plugin_catalog.c: module copies, keyed (id, arch, sha256) */
	struct list_head catalog;	/* ua->lock */
	unsigned int catalog_count;

	/* ua_plugin_instance.c: every instance of every open file (ua->lock) */
	struct list_head instances;
	unsigned int n_instances;
	u32 next_handle;

	/* ua_plugin_license.c: queries and decisions since probe */
	u32 board_queries, board_query_failures, admitted, refused;

	struct delayed_work work;	/* strip passes */
	bool work_ready;
};

/* ---- ua_plugin_host.c ---- */
int ua_plugin_host_init(struct ua_device *ua);		/* probe */
void ua_plugin_host_free(struct ua_device *ua);		/* chardev release */
u64 ua_plugin_host_ops(struct ua_device *ua);		/* ua->lock */
void ua_plugin_host_caps(struct ua_device *ua, u32 *state, u32 *owned,
			 u32 *quarantined, u32 *instances);	/* ua->lock */
long ua_plugin_ioctl_board_query(struct ua_device *ua, unsigned long arg);
long ua_plugin_ioctl_instance(struct ua_device *ua, struct ua_file *uf,
			      unsigned long arg);

#endif /* UA_PLUGIN_HOST_H */
