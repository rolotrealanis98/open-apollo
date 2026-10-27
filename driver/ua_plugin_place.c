// SPDX-License-Identifier: GPL-2.0-only
/*
 * Universal Audio Apollo — plug-in host: DSP ownership, geometry, placement
 *
 * Copyright (c) 2026 apollo-linux contributors
 *
 * The kext keeps every DSP's pool bookkeeping host-side (CResourcePool;
 * ChangeScratchSize sends nothing), so the registers say where the pools
 * are, never what another host put in them.  The withdrawn overlay treated
 * pool space as free on any DSP, DSP 0 (the mixer's) included (review
 * findings 1 and 16).  Here a DSP's pools are the host's only when it saw
 * the DSP freshly powered:
 *   - at a cold start (the driver (re)started the DSPs) the DSP's boot word
 *     is exactly 3: no host has connected to it since power-up (titan: a
 *     Linux connect gives 0x0b + a connect count in bits 20:19, a macOS
 *     session left 0x00100007);
 *   - a DSP found running at probe is never the host's (Windows or macOS
 *     may have left strips and plug-ins there), nor one a raw DSP ioctl
 *     touched, until the next cold start;
 *   - a warm restart of the same device instance keeps ownership only when
 *     everything placed was taken down provably (the ledger is idle).
 * The geometry (CPcieDSP::GetProperty(6): capacity, base, scratch) is read
 * at connect and becomes that DSP's ledger (ua_resource_pool.c).  DSP 0 is
 * shared with the input strips through the same ledger, so whatever the
 * strips hold is never handed out again.
 */

#include <linux/errno.h>
#include <linux/kernel.h>
#include <linux/mutex.h>
#include <sound/core.h>
#include <sound/info.h>

#include "ua_apollo.h"
#include "ua_plugin_host.h"

/* UA_PLACE_BEGIN: the host tests build everything up to the end mark */

/* DSP n's register window: n * 0x800, plus 0x2000 above DSP 3 */
u32 ua_plugin_dsp_window(unsigned int dsp)
{
	return dsp * 0x800 + (dsp > 3 ? 0x2000 : 0);
}

static void ua_plugin_why(struct ua_host_dsp *d, const char *fmt, ...)
	__printf(2, 3);
static void ua_plugin_why(struct ua_host_dsp *d, const char *fmt, ...)
{
	va_list args;

	va_start(args, fmt);
	vscnprintf(d->why, sizeof(d->why), fmt, args);
	va_end(args);
}

/**
 * ua_plugin_disown - DSP @dsp's pools are no longer the host's
 * @ua: device (ua->lock held, or exclusive)
 * @dsp: DSP index
 * @why: for the log and proc
 *
 * The ledger goes: nothing placed there can be handed out or trusted any
 * more.  Callers make sure nothing still refers to its leases (instances
 * are ended by the epoch bump that comes with every disown).
 */
void ua_plugin_disown(struct ua_device *ua, unsigned int dsp, const char *why)
{
	struct ua_host_dsp *d = &ua->host->dsp[dsp];

	if (d->owned)
		dev_info(&ua->pdev->dev,
			 "plug-in host: DSP %u no longer used: %s\n", dsp, why);
	/* its instances go with it (their leases with the ledger) */
	ua_instances_dsp_gone(ua, dsp);
	d->owned = false;
	d->geom_read = false;
	if (d->pools) {
		ua_pool_owner_invalidate(d->pools);
		ua_pool_owner_destroy(d->pools);
		d->pools = NULL;
	}
	ua_plugin_why(d, "%s", why);
}

/**
 * ua_plugin_dsp_start - the DSPs were found running (warm) or (re)started
 * @ua: device (probe, or ua->lock held on resume)
 * @cold: the cold path (re)started them
 *
 * Decides, per DSP, whether its pools are the host's (see the top of the
 * file).  Reads the boot words on a cold start only, and only when
 * something is hosted (plugin_host or input_strips): otherwise nothing is
 * read and nothing is owned.  Always starts a new epoch.
 */
void ua_plugin_dsp_start(struct ua_device *ua, bool cold)
{
	struct ua_plugin_host *h = ua->host;
	unsigned int i, owned = 0;

	if (!h)
		return;
	ua_plugin_epoch_bump(ua, cold ? "cold start" : "warm start");
	if (!h->active)
		return;
	if (cold)
		h->phantom_pending = true;
	for (i = 0; i < ua->num_dsps && i < UA_HOST_DSPS; i++) {
		struct ua_host_dsp *d = &h->dsp[i];

		if (cold) {
			ua_plugin_disown(ua, i, "cold start");
			d->quarantined = false;
			d->boot_word = ua_read(ua, ua_plugin_dsp_window(i) +
					       UA_DSP_WIN_BOOT);
			d->owned = (d->boot_word & ~UA_DSP_BOOT_MODEL_MASK) ==
				   UA_DSP_BOOT_POWERUP;
			if (d->owned)
				ua_plugin_why(d, "cold-started by this driver, no host since power-up");
			else
				ua_plugin_why(d, "a host has used it since power-up (boot word 0x%08x)",
					      d->boot_word);
		} else if (!d->seen) {
			ua_plugin_disown(ua, i,
					 "running when this driver found it: another session may have left resources there");
		} else if (d->owned && d->pools && !ua_pool_owner_idle(d->pools)) {
			ua_plugin_disown(ua, i,
					 "resources were still placed at a warm restart");
		}
		d->seen = true;
		if (d->owned)
			owned |= BIT(i);
	}
	dev_info(&ua->pdev->dev,
		 "plug-in host: %s start, DSPs this driver may use: 0x%x (DSP 0 boot word 0x%08x)\n",
		 cold ? "cold" : "warm", owned, h->dsp[0].boot_word);
}

/**
 * ua_plugin_taint - raw DSP access from user space
 * @ua: device (no lock held)
 * @why: what it was
 *
 * A raw ring command, SRAM write, firmware load or DMA reset may change
 * what the pools hold behind the host's back: no DSP is the host's until
 * the next cold start.
 */
void ua_plugin_taint(struct ua_device *ua, const char *why)
{
	struct ua_plugin_host *h = ua->host;
	unsigned int i;

	if (!h)
		return;
	mutex_lock(&ua->lock);
	for (i = 0; i < UA_HOST_DSPS; i++)
		if (h->dsp[i].owned)
			ua_plugin_disown(ua, i, why);
	ua_plugin_epoch_bump(ua, why);
	mutex_unlock(&ua->lock);
}

/**
 * ua_plugin_read_geometry - DSP @dsp's pool registers into its ledger
 * @ua: device (ua->lock held)
 * @dsp: an owned DSP
 *
 * Reads only.  A ledger is created the first time; a later read that
 * gives another map while something is placed disowns the DSP.
 */
int ua_plugin_read_geometry(struct ua_device *ua, unsigned int dsp)
{
	struct ua_host_dsp *d = &ua->host->dsp[dsp];
	struct ua_pool_geometry g[UA_POOLS];
	u32 win = ua_plugin_dsp_window(dsp);
	unsigned int p;
	int ret;

	lockdep_assert_held(&ua->lock);
	if (!d->owned)
		return -EPERM;
	for (p = 0; p < UA_POOLS; p++) {
		g[p].words = ua_read(ua, win + UA_DSP_WIN_SIZE + 4 * p);
		g[p].base = ua_read(ua, win + UA_DSP_WIN_BASE + 4 * p);
		g[p].scratch = p ? ua_read(ua, win + UA_DSP_WIN_SCRATCH + 4 * p) : 0;
		d->regs[p] = g[p].words;
		d->regs[4 + p] = g[p].base;
		if (p)
			d->regs[7 + p] = g[p].scratch;
		if (g[p].words == 0xffffffff || g[p].base == 0xffffffff ||
		    g[p].scratch == 0xffffffff)
			return -ENODEV;
	}
	if (d->pools) {
		if (!memcmp(g, d->geom, sizeof(g)))
			return 0;
		if (!ua_pool_owner_idle(d->pools)) {
			ua_plugin_disown(ua, dsp, "its pool map changed with resources placed");
			return -EIO;
		}
		ua_pool_owner_destroy(d->pools);
		d->pools = NULL;
	}
	ret = ua_pool_owner_create(g, &d->pools);
	if (ret) {
		ua_plugin_disown(ua, dsp, ret == -EINVAL ?
				 "its pool registers are not a valid map" :
				 "no memory for its pool ledger");
		return ret;
	}
	memcpy(d->geom, g, sizeof(g));
	d->geom_read = true;
	return 0;
}

/* May anything more be placed on DSP @dsp? (ua->lock) */
bool ua_plugin_dsp_admitted(struct ua_device *ua, unsigned int dsp)
{
	struct ua_host_dsp *d;

	if (dsp >= ua->num_dsps || dsp >= UA_HOST_DSPS)
		return false;
	d = &ua->host->dsp[dsp];
	return d->owned && !d->quarantined && d->pools;
}

/**
 * ua_plugin_place - reserve a resource set on one admitted DSP
 * @ua: device (ua->lock held)
 * @want: a DSP index, or UA_PLUGIN_DSP_ANY
 * @rq: the requests (shared code by catalog entry, private rows)
 * @n: how many
 * @dsp: the DSP used
 * @leases: one per request
 *
 * All or nothing on one DSP.  With UA_PLUGIN_DSP_ANY the DSPs are tried
 * from 1 up, DSP 0 (shared with the mixer and the input strips) last.
 * Returns 0, -ENODEV (no admitted DSP), -ENOSPC (none has room), -EINVAL.
 */
int ua_plugin_place(struct ua_device *ua, unsigned int want,
		    const struct ua_pool_request *rq, unsigned int n,
		    unsigned int *dsp, struct ua_pool_lease *leases)
{
	unsigned int k, i, tried = 0;
	int ret = -ENODEV;

	lockdep_assert_held(&ua->lock);
	if (!n || n > UA_POOL_BATCH_MAX)
		return -EINVAL;
	for (k = 0; k < ua->num_dsps && k < UA_HOST_DSPS; k++) {
		i = k + 1 < ua->num_dsps ? k + 1 : 0;	/* 1, 2, ..., 0 */
		if (want != UA_PLUGIN_DSP_ANY && i != want)
			continue;
		if (!ua_plugin_dsp_admitted(ua, i))
			continue;
		tried++;
		ret = ua_pool_reserve_batch(ua->host->dsp[i].pools, rq, n, leases);
		if (!ret) {
			*dsp = i;
			return 0;
		}
		if (ret != -ENOSPC)
			return ret;
	}
	return tried ? -ENOSPC : -ENODEV;
}

/* Give a placed set back to DSP @dsp's ledger, last first (ua->lock) */
int ua_plugin_unplace(struct ua_device *ua, unsigned int dsp,
		      const struct ua_pool_lease *leases, unsigned int n)
{
	struct ua_host_dsp *d = &ua->host->dsp[dsp];
	int ret = 0, r;

	lockdep_assert_held(&ua->lock);
	if (!d->pools)
		return -ESTALE;
	while (n--) {
		if (!leases[n].cookie)
			continue;
		r = ua_pool_release(d->pools, &leases[n]);
		if (r && !ret)
			ret = r;
	}
	return ret;
}

/* UA_PLACE_END */

/**
 * ua_plugin_proc - /proc/asound/cardN/plugin_host
 * @ua: device (ua->lock held)
 * @buf: proc buffer
 *
 * Ownership, geometry (raw and as the ledger rounds it), pool usage and
 * everything the host sent, per DSP; transactions, catalog, instances,
 * authorization queries.  Never the board table.
 */
void ua_plugin_proc(struct ua_device *ua, struct snd_info_buffer *buf)
{
	struct ua_plugin_host *h = ua->host;
	struct ua_cat_entry *e;
	unsigned int i, p;

	snd_iprintf(buf, "plug-in host\tplugin_host=%d, %s, epoch %llu\n",
		    ua_plugin_host_enabled(), h->active ? "active" : "inactive",
		    (unsigned long long)h->epoch);
	for (i = 0; i < ua->num_dsps && i < UA_HOST_DSPS; i++) {
		struct ua_host_dsp *d = &h->dsp[i];
		struct ua_pool_usage u[UA_POOLS];

		snd_iprintf(buf, "DSP %u: %s%s, boot word 0x%08x: %s\n", i,
			    d->owned ? "owned" : "not owned",
			    d->quarantined ? ", QUARANTINED" : "",
			    d->boot_word, d->why[0] ? d->why : "-");
		if (d->geom_read)
			snd_iprintf(buf, "  registers: size %08x %08x %08x %08x base %08x %08x %08x %08x scratch %08x %08x %08x\n",
				    d->regs[0], d->regs[1], d->regs[2], d->regs[3],
				    d->regs[4], d->regs[5], d->regs[6], d->regs[7],
				    d->regs[8], d->regs[9], d->regs[10]);
		if (d->pools && !ua_pool_read_usage(d->pools, u))
			for (p = 0; p < UA_POOLS; p++)
				snd_iprintf(buf, "  pool %u: base 0x%08x total %u scratch %u used %u free %u largest %u allocations %u holds %llu\n",
					    p, u[p].base, u[p].total_words,
					    u[p].scratch_words, u[p].used_words,
					    u[p].free_words,
					    u[p].largest_request_words,
					    u[p].unique_allocations,
					    (unsigned long long)u[p].holds);
		snd_iprintf(buf, "  sent: transactions %u, uploads %u (replies ok %u, bad %u), zero-fills %u, memspecs %u, writes %u, stops %u, releases %u, timeouts %u\n",
			    d->tx_xacts, d->uploads, d->replies_ok,
			    d->replies_bad, d->zero_fills, d->memspecs,
			    d->writes, d->stops, d->releases, d->timeouts);
	}
	snd_iprintf(buf, "transactions: %u held (%zu bytes), %u awaiting replies, %u leaked\n",
		    h->xact_count, h->xact_bytes, h->xact_waiting,
		    h->xact_leaked);
	snd_iprintf(buf, "catalog: %u module(s)\n", h->catalog_count);
	list_for_each_entry(e, &h->catalog, node) {
		snd_iprintf(buf, "  module 0x%03x arch %u kind %u, %u words, %zu bytes, sha256 %8phN.., users %u, resident:",
			    e->info.id, e->info.arch, e->info.kind,
			    e->info.words, e->len, e->sha256, e->users);
		for (i = 0; i < ua->num_dsps && i < UA_HOST_DSPS; i++)
			if (ua_catalog_resident(e, i, h->epoch))
				snd_iprintf(buf, " DSP %u @0x%05x", i,
					    e->res[i].base);
		snd_iprintf(buf, "\n");
	}
	snd_iprintf(buf, "instances: %u\n", h->n_instances);
	snd_iprintf(buf, "authorization: %u board queries (%u failed), %u admitted, %u refused\n",
		    h->board_queries, h->board_query_failures, h->admitted,
		    h->refused);
}

static void ua_plugin_proc_read(struct snd_info_entry *entry,
				struct snd_info_buffer *buf)
{
	struct ua_device *ua = entry->private_data;

	mutex_lock(&ua->lock);
	if (ua->host)
		ua_plugin_proc(ua, buf);
	mutex_unlock(&ua->lock);
}

int ua_plugin_proc_init(struct ua_device *ua, struct snd_card *card)
{
	return snd_card_ro_proc_new(card, "plugin_host", ua,
				    ua_plugin_proc_read);
}
