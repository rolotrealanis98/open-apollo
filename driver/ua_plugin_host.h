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

/* ---- ring primitives exported by ua_dsp.c for ua_dsp_xact.c ---- */
struct ua_dsp_ring;
unsigned int ua_dsp_ring_free_slots(const struct ua_dsp_ring *ring);
int ua_dsp_ring_update_pos(struct ua_device *ua, struct ua_dsp_ring *ring);
unsigned int ua_dsp_ring_pending(const struct ua_dsp_ring *ring);
int ua_dsp_ring_put_words(struct ua_device *ua, struct ua_dsp_ring *ring,
			  u32 w0, u32 w1, u32 w2, u32 w3);
void ua_dsp_ring_doorbell(struct ua_device *ua, struct ua_dsp_ring *ring,
			  u32 ring_base);
void ua_dsp_ring_program(struct ua_device *ua, struct ua_dsp_ring *ring,
			 u32 ring_base);
int ua_dsp_ring_resync_now(struct ua_device *ua, struct ua_dsp_ring *ring);

/* ---- transactions (ua_dsp_xact.c) ----
 *
 * One request to one DSP: inline commands and DSP commands carried by DMA
 * pages on its command ring, and optionally reply descriptors on its
 * response ring, published together (replies first) under ua->lock with
 * one doorbell per ring.  The coherent buffer behind them belongs to the
 * transaction until both rings are provably past it, whatever happens to
 * the caller: a transaction that is not retired when its caller is done
 * stays on the host's own bounded list (never the firmware/PCM orphans).
 */
#define UA_XACT_ENTS_MAX	128	/* command-ring entries, a quarter ring at most */
#define UA_XACT_RX_MAX		8	/* reply descriptors */
#define UA_XACT_RX_WORDS_MAX	1024	/* dwords of one reply descriptor */
#define UA_XACT_PAYLOAD_MAX	(256u << 10)
#define UA_XACT_PIECE		4096	/* one DMA_REF never crosses a 4 KiB page */
#define UA_XACT_RETAIN_MAX	32	/* unretired transactions kept, at most */
#define UA_XACT_RETAIN_BYTES	(8u << 20)
#define UA_XACT_WAIT_MS		1000	/* the plan's 1 s bound */
#define UA_XACT_PUBLISH_MS	50	/* room / an idle command ring, off-lock */

enum ua_xact_state {
	UA_XACT_BUILDING,	/* entries and payload being laid out */
	UA_XACT_MAPPED,		/* coherent buffer filled, nothing published */
	UA_XACT_PUBLISHED,	/* doorbelled: the device may read/write it */
	UA_XACT_RETIRED,	/* both rings past it: replies valid */
};

struct ua_xact_ref {
	struct ua_dsp_ring *ring;	/* NULL: this ring is done with it */
	u32 gen;
	unsigned int end;		/* producer index just past our entries */
	unsigned int start, need;	/* abandoned by a resync */
	bool abandoned;
};

struct ua_xact {
	struct list_head node;		/* host->xacts while published */
	unsigned int dsp;
	u64 epoch;			/* host epoch it was built in */
	int err;			/* sticky builder error */
	enum ua_xact_state state;
	bool released;			/* the caller is done with it */
	bool waiting;			/* counted in host->xact_waiting */
	bool detached;			/* ua_xact_fini() took it off the list */
	/* layout */
	unsigned int n;
	u32 ent[UA_XACT_ENTS_MAX][4];	/* DMA_REF entries: word 2 = offset */
	u8 *stage;			/* payload staging (kvmalloc) */
	size_t cap, len;
	unsigned int n_rx;
	u32 rx_words[UA_XACT_RX_MAX];
	u32 rx_off[UA_XACT_RX_MAX];	/* bytes into the reply area */
	size_t rx_len;
	u32 rx_fill;			/* the reply area's initial word */
	/* mapping */
	void *cpu;
	dma_addr_t dma;
	size_t size, rx_base;
	/* publication */
	struct ua_xact_ref ref[2];	/* [0] command ring, [1] response ring */
	unsigned long published;	/* jiffies */
};

struct ua_xact *ua_xact_alloc(unsigned int dsp, size_t payload_cap);
void ua_xact_inline(struct ua_xact *x, u32 w0, u32 w1, u32 w2, u32 w3);
__le32 *ua_xact_cmd(struct ua_xact *x, unsigned int dwords);
int ua_xact_rx(struct ua_xact *x, unsigned int dwords);
int ua_xact_map(struct ua_device *ua, struct ua_xact *x);	/* no lock */
int ua_xact_submit(struct ua_device *ua, struct ua_xact *x);	/* ua->lock */
int ua_xact_poll(struct ua_device *ua, struct ua_xact *x);	/* ua->lock */
int ua_xact_publish(struct ua_device *ua, struct ua_xact *x);	/* no lock */
int ua_xact_wait(struct ua_device *ua, struct ua_xact *x,
		 unsigned int timeout_ms);			/* no lock */
int ua_xact_run(struct ua_device *ua, struct ua_xact *x,
		unsigned int timeout_ms);	/* host->mutex, no ua->lock */
const __le32 *ua_xact_reply(const struct ua_xact *x, unsigned int i);
void ua_xact_done(struct ua_device *ua, struct ua_xact *x);	/* ua->lock */
void ua_xact_discard(struct ua_device *ua, struct ua_xact *x);	/* unpublished */
void ua_xact_reclaim(struct ua_device *ua);			/* ua->lock */
void ua_xact_ring_abandon(struct ua_device *ua, struct ua_dsp_ring *ring,
			  u32 next);					/* ua->lock */
void ua_xact_fini(struct ua_device *ua, bool dma_quiet);	/* rings fini */

/* ---- epoch and quarantine (ua_plugin_host.c) ---- */
void ua_plugin_epoch_bump(struct ua_device *ua, const char *why); /* ua->lock */
void ua_plugin_quarantine(struct ua_device *ua, unsigned int dsp,
			  const char *why, int err);		/* ua->lock */

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
