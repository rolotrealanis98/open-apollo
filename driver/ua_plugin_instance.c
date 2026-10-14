// SPDX-License-Identifier: GPL-2.0-only
/*
 * Universal Audio Apollo — plug-in host: plug-in instances
 *
 * Copyright (c) 2026 apollo-linux contributors
 *
 * UA_IOCTL_PLUGIN_INSTANCE (ua_ioctl.h).  An instance belongs to the open
 * file that created it: closing the file takes it down, and so does the
 * device going away (review finding 2: the withdrawn overlay only revoked
 * logically, nothing was reclaimed).  There is no separate PREPARE session
 * (finding 12): CREATE is one operation under host->mutex, in this order:
 *   1. the device's authorization answer, fresh (ua_plugin_admit());
 *      nothing below happens unless it admits the plug-in;
 *   2. the modules, read from the store by hash and checked (catalog);
 *   3. the last teardown on the DSPs proven (fences);
 *   4. one placement on an owned, unquarantined DSP, in the same epoch as
 *      the answer (else -ESTALE);
 *   5. each module not resident there uploaded and acknowledged;
 *   6. the private rows zero-filled and linked (memspec), taken by the DSP.
 * F_START then writes parameters and starts the chain; SERVICE updates
 * parameters and reads back private rows on that instance. Any failure after
 * placement takes down what was placed; an outcome that cannot be proven
 * quarantines the DSP.
 */

#include <linux/errno.h>
#include <linux/mm.h>
#include <linux/slab.h>
#include <linux/uaccess.h>

#include "ua_apollo.h"
#include "ua_plugin_host.h"

/* UA_INSTANCE_BEGIN: the host tests build everything up to the end mark */

/* Ring words for UA_PLUGIN_CREATE_F_START (same as the input strips') */
#define UA_INST_CMD_SYNCH	0x001F0004	/* {.., in2 << 16 | in1, head, 0} */
#define UA_INST_CMD_INPUT	0x001E0004	/* {.., input, param, value} */
#define UA_INST_INPUT_ROUTE	7
#define UA_INST_INPUT_SPAN	8
#define UA_INST_SPAN_NONE	0x20
#define UA_INST_ROUTE_NONE	0x1ffff

struct ua_plugin_inst {
	struct list_head node;		/* host->instances (ua->lock) */
	struct ua_file *owner;
	u32 handle;
	u32 render_id;
	u64 epoch;
	unsigned int dsp;
	bool placed;			/* leases taken */
	bool loaded;			/* everything taken by the DSP */
	bool lost;			/* taken down by the host, or the DSP gone */
	bool started;			/* SYNCH'd on a DSP input (F_START) */
	u32 synch;			/* in1 | in2 << 16 */
	bool strip_override;		/* reserves the retained plain strip */
	u32 strip_generation;
	u32 scratch_prev[UA_POOLS];	/* scratch before F_START raised it */
	u8 scratch_raised;		/* bit p: pool p raised by this instance */
	unsigned int n_res, n_priv;
	struct ua_cat_entry **res;	/* catalog references */
	struct ua_pool_lease *lease;	/* n_res modules, then n_priv rows */
	u8 *ident;			/* n_priv: one identity per private row */
	struct ua_memspec_row *rows;	/* immutable bounds and link ownership */
};

static void ua_inst_free(struct ua_device *ua, struct ua_plugin_inst *in)
{
	unsigned int i;

	for (i = 0; i < in->n_res; i++)
		if (in->res[i])
			ua_catalog_put(ua, in->res[i]);
	kfree(in->res);
	kfree(in->lease);
	kfree(in->ident);
	kfree(in->rows);
	kfree(in);
}

/* Put back the scratch @in raised (ua->lock held) */
static void ua_inst_scratch_restore(struct ua_pool_owner *o,
				    struct ua_plugin_inst *in)
{
	unsigned int p;

	for (p = 1; p < UA_POOLS; p++)
		if (o && (in->scratch_raised & (1u << p)))
			ua_pool_set_scratch(o, p, in->scratch_prev[p]);
	in->scratch_raised = 0;
}

/*
 * F_START: raise the DSP's scratch to what the chain needs before anything
 * is placed there, as CResourcePool::ChangeScratchSize does (nothing is
 * sent: the DSP has no scratch register).  ua->lock held.
 */
static int ua_inst_scratch_raise(struct ua_pool_owner *o,
				 struct ua_plugin_inst *in,
				 const u32 need[UA_POOLS])
{
	struct ua_pool_usage u[UA_POOLS];
	unsigned int p;
	int ret;

	if (!o)
		return -ENODEV;
	ret = ua_pool_read_usage(o, u);
	for (p = 1; !ret && p < UA_POOLS; p++) {
		if (need[p] <= u[p].scratch_words)
			continue;
		ret = ua_pool_set_scratch(o, p, need[p]);
		if (!ret) {
			in->scratch_prev[p] = u[p].scratch_words;
			in->scratch_raised |= 1u << p;
		}
	}
	if (ret)
		ua_inst_scratch_restore(o, in);
	return ret;
}

/*
 * Take @in off its DSP (ua->lock held): the modules whose last hold it
 * had are released on the DSP, everything goes back to the ledger.  With
 * @send the release words are published as the DSP's fence (no wait);
 * without, or when the DSP is not the host's any more, nothing is sent.
 * Returns 1 for a published fence, 0 for no work, or a retirement error.
 */
static int ua_inst_unplace(struct ua_device *ua,
			   struct ua_plugin_inst *in, bool send, bool restore)
{
	struct ua_plugin_host *h = ua->host;
	struct ua_host_dsp *d = &h->dsp[in->dsp];
	struct ua_xact *x = NULL;
	unsigned int i, rels = 0;
	bool stops;
	int ret = 0;

	if (!in->placed)
		return 0;
	in->placed = false;
	if (!d->pools)
		return 0;		/* the ledger went with the DSP */
	if (send && ua_plugin_dsp_admitted(ua, in->dsp)) {
		x = ua_xact_alloc(in->dsp, 0);
		if (!x)
			ret = -ENOMEM;
	} else if (restore && in->started && !in->lost) {
		ret = -ESTALE;
	}
	if (x)
		x->epoch = in->epoch;
	/* a started instance stops before anything of it is released */
	if (x && in->started && !in->lost)
		ua_xact_inline(x, UA_INST_CMD_SYNCH, in->synch, 0, 0);
	stops = x && in->started && !in->lost;
	if (stops && restore && in->strip_override)
		ret = ua_strips_restore(ua, x, in->synch, in->strip_generation);
	in->started = false;
	for (i = 0; i < in->n_res; i++) {
		struct ua_cat_entry *e = in->res[i];
		bool last = false;

		if (!in->lease[i].cookie)
			continue;
		ua_pool_release_last(d->pools, &in->lease[i], &last);
		memset(&in->lease[i], 0, sizeof(in->lease[i]));
		if (!last || e->res[in->dsp].state == UA_RES_NONE)
			continue;
		if (x && e->res[in->dsp].state == UA_RES_RESIDENT &&
		    e->res[in->dsp].epoch == h->epoch) {
			ua_release_build(x, &e->info);
			rels++;
		}
		e->res[in->dsp].state = UA_RES_NONE;
	}
	ua_plugin_unplace(ua, in->dsp, in->lease + in->n_res, in->n_priv);
	memset(in->lease + in->n_res, 0, in->n_priv * sizeof(*in->lease));
	ua_inst_scratch_restore(d->pools, in);
	if (ret) {
		if (x)
			ua_xact_discard(ua, x);
		ua_plugin_quarantine(ua, in->dsp, "plug-in retirement unproven", ret);
		return ret;
	}
	if (!x)
		return 0;
	if (!rels && !stops) {
		ua_xact_discard(ua, x);
		return 0;
	}
	ret = ua_plugin_publish_now(ua, x);
	if (ret) {
		ua_plugin_quarantine(ua, in->dsp,
				     "a plug-in teardown could not be sent", ret);
		return ret;
	}
	d->releases += rels;
	ua_plugin_fence_set(ua, in->dsp, x);
	return 1;	/* A stop-only transaction still needs its fence proved. */
}

/**
 * ua_instances_teardown - take every instance down (reconnect, rate change)
 * @ua: device (ua->lock held)
 * @why: for the log
 *
 * Their handles stay until DESTROY or close, and answer -ESTALE.
 */
void ua_instances_teardown(struct ua_device *ua, const char *why)
{
	struct ua_plugin_host *h = ua->host;
	struct ua_plugin_inst *in;
	unsigned int n = 0;

	lockdep_assert_held(&ua->lock);
	list_for_each_entry(in, &h->instances, node) {
		if (in->lost)
			continue;
		ua_inst_unplace(ua, in, true, false);
		in->lost = true;
		in->loaded = false;
		n++;
	}
	if (n)
		dev_info(&ua->pdev->dev,
			 "plug-in host: %u instance(s) taken down (%s)\n", n, why);
}

/* Rebuild must not free the plain strip a pending/running insert will restore. */
bool ua_instances_strips_busy(struct ua_device *ua)
{
	struct ua_plugin_inst *in;

	lockdep_assert_held(&ua->lock);
	list_for_each_entry(in, &ua->host->instances, node)
		if (in->placed && in->strip_override && !in->lost)
			return true;
	return false;
}

/* DSP @dsp is no longer the host's (ua->lock): its instances are gone */
void ua_instances_dsp_gone(struct ua_device *ua, unsigned int dsp)
{
	struct ua_plugin_host *h = ua->host;
	struct ua_plugin_inst *in;

	list_for_each_entry(in, &h->instances, node) {
		if (in->dsp != dsp || !in->placed)
			continue;
		/* the ledger is about to go: forget the leases, send nothing */
		memset(in->lease, 0, (in->n_res + in->n_priv) * sizeof(*in->lease));
		in->placed = false;
		in->loaded = false;
		in->lost = true;
	}
}

static struct ua_plugin_inst *ua_inst_find(struct ua_plugin_host *h,
					   struct ua_file *uf, u32 handle)
{
	struct ua_plugin_inst *in;

	list_for_each_entry(in, &h->instances, node)
		if (in->owner == uf && in->handle == handle)
			return in;
	return NULL;
}

/*
 * Are the private rows one valid descriptor (pools, sizes, flags, links
 * inside their targets)?  Checked with placeholder addresses: the real
 * ones come from the ledger.
 */
static int ua_inst_check_rows(const struct ua_memspec_row *rows,
			      unsigned int n)
{
	static const unsigned int desc[1] = { 0 };
	u32 *addr, *words;
	unsigned int i;
	int ret = -EINVAL;

	for (i = 0; i < n; i++)
		if (rows[i].dwords > 0x1000000)
			return -EINVAL;
	if (n == 1)
		return rows[0].link == 0xffffffff && rows[0].pool >= 1 &&
		       rows[0].pool <= 3 && rows[0].dwords &&
		       (rows[0].flags == 0 || rows[0].flags == 2) ? 0 : -EINVAL;
	addr = kcalloc(n, sizeof(*addr), GFP_KERNEL);
	words = kcalloc(2 * n, sizeof(*words), GFP_KERNEL);
	if (addr && words)
		ret = ua_memspec_compile(rows, n, desc, 1, addr, words) < 0 ?
		      -EINVAL : 0;
	else
		ret = -ENOMEM;
	kfree(addr);
	kfree(words);
	return ret;
}

/* Does a row link into word @w of row @r (a pointer the memspec owns)? */
static bool ua_inst_link_word(const struct ua_memspec_row *rows,
			      unsigned int n, unsigned int r, u32 w)
{
	unsigned int j;

	for (j = 1; j < n; j++)
		if (rows[j].link != 0xffffffff && rows[j].link >> 24 == r &&
		    (rows[j].link & 0xffffff) == w)
			return true;
	return false;
}

/*
 * Are the F_START parameter commands UA's WRITEs, each inside its row and
 * clear of every linked word (see ua_ioctl.h)?  Returns the number of
 * commands, or -EINVAL.
 */
static int ua_inst_check_cmds(const struct ua_memspec_row *rows,
			      unsigned int n_rows, const u32 *c,
			      unsigned int nw)
{
	unsigned int i = 0, k, cmds = 0;

	while (i < nw) {
		u32 len = c[i] & 0xffff, row, word, mask, pos;

		if (c[i] >> 16 != UA_PLUGIN_CMD_WRITE || len < 4 ||
		    len > nw - i)
			return -EINVAL;
		row = c[i + 1] >> 24;
		word = c[i + 1] & 0xffffff;
		mask = c[i + 2];
		if (row >= n_rows || word >= rows[row].dwords ||
		    len - 3 + hweight32(mask) > rows[row].dwords - word)
			return -EINVAL;
		for (k = 0, pos = word; k < len - 3; k++, pos++) {
			while (pos > word && pos - word - 1 < 32 &&
			       (mask >> (pos - word - 1)) & 1)
				pos++;
			if (ua_inst_link_word(rows, n_rows, row, pos))
				return -EINVAL;
		}
		i += len;
		cmds++;
	}
	return cmds;
}

/*
 * A module with the same id and architecture but other bytes than one the
 * host already uses (the strips' or another instance's): the DSP knows a
 * resource by its id, so the two cannot both be there.  (ua->lock)
 */
static bool ua_inst_conflicts(struct ua_plugin_host *h,
			      const struct ua_cat_entry *e)
{
	const struct ua_cat_entry *o;

	list_for_each_entry(o, &h->catalog, node)
		if (o != e && o->users && o->info.id == e->info.id &&
		    o->info.arch == e->info.arch)
			return true;
	return false;
}

/* Is the instance's set uploadable (kinds, architectures, duplicates)? */
static int ua_inst_check_set(struct ua_plugin_inst *in)
{
	unsigned int i, j;

	for (i = 0; i < in->n_res; i++) {
		const struct ua_bill_info *b = &in->res[i]->info;

		/* code and data memory only for now (kind 2/3 pools: later) */
		if (b->kind != 0 && b->kind != 1 && b->kind != 4)
			return -EOPNOTSUPP;
		/* the Apollo's DSPs take architecture 0 and 2 records */
		if (b->arch == 1)
			return -EINVAL;
		for (j = 0; j < i; j++) {
			const struct ua_bill_info *c = &in->res[j]->info;

			if (in->res[j] == in->res[i])
				return -EINVAL;
			if (c->id == b->id && c->arch == b->arch)
				return -EEXIST;
		}
	}
	return 0;
}

/*
 * The private rows' transaction: zero-fills, then the memspec (one
 * descriptor; a single row has no link to store).
 */
static int ua_inst_privates(struct ua_device *ua, struct ua_plugin_inst *in,
			    const struct ua_memspec_row *rows)
{
	struct ua_plugin_host *h = ua->host;
	static const unsigned int desc[1] = { 0 };
	struct ua_xact *x;
	u32 *addr, *words;
	unsigned int i;
	__le32 *w;
	int ret, n;

	addr = kcalloc(in->n_priv, sizeof(*addr), GFP_KERNEL);
	words = kcalloc(2 * in->n_priv, sizeof(*words), GFP_KERNEL);
	x = ua_xact_alloc(in->dsp, 4 * UA_XACT_PIECE);
	ret = -ENOMEM;
	if (!addr || !words || !x)
		goto out;
	x->epoch = in->epoch;
	for (i = 0; i < in->n_priv; i++) {
		addr[i] = in->lease[in->n_res + i].base;
		ua_xact_inline(x, UA_CMD_ZERO, addr[i], 0,
			       rows[i].dwords + (rows[i].dwords & 1));
	}
	if (in->n_priv > 1) {
		n = ua_memspec_compile(rows, in->n_priv, desc, 1, addr, words);
		if (n < 0) {
			ret = n;
			goto out;
		}
		w = ua_xact_cmd(x, (unsigned int)n);
		if (w)
			for (i = 0; i < (unsigned int)n; i++)
				w[i] = cpu_to_le32(words[i]);
	}
	ret = ua_xact_run(ua, x, UA_XACT_WAIT_MS);
	mutex_lock(&ua->lock);
	if (x->state >= UA_XACT_PUBLISHED) {
		h->dsp[in->dsp].zero_fills += in->n_priv;
		h->dsp[in->dsp].memspecs += in->n_priv > 1;
	}
	ua_xact_done(ua, x);
	mutex_unlock(&ua->lock);
	x = NULL;
out:
	ua_xact_discard(ua, x);
	kfree(addr);
	kfree(words);
	return ret;
}

/*
 * Copy checked F_START commands (ua_inst_check_cmds()) to @d with word 1
 * relocated from row << 24 | word to the row's DSP address, as UAD2System's
 * _fixParameterAddresses does.  Returns the number of commands.
 */
static unsigned int ua_inst_cmd_copy(__le32 *d, const u32 *cmd,
				     unsigned int nw,
				     const struct ua_pool_lease *rows)
{
	unsigned int i, j, len, n = 0;

	for (i = 0; i < nw; i += len, n++) {
		len = cmd[i] & 0xffff;
		for (j = 0; j < len; j++)
			d[i + j] = cpu_to_le32(cmd[i + j]);
		d[i + 1] = cpu_to_le32(rows[cmd[i + 1] >> 24].base +
				       (cmd[i + 1] & 0xffffff));
	}
	return n;
}

/*
 * UA_PLUGIN_CREATE_F_START: stop what runs on the input, send the
 * parameter WRITEs (UA's blocks, checked by ua_inst_check_cmds(), word 1
 * relocated to the row's address; one DMA page), start the instance on the
 * input and send its span and route words, as the kext does after a chain
 * is placed (EnableSynchProcessing, MoveInputToDSP).  Marked started
 * before the run, so a failure still stops it at the takedown.
 */
static int ua_inst_start(struct ua_device *ua, struct ua_plugin_inst *in,
			 const u32 *cmd, unsigned int nw, u32 synch)
{
	struct ua_plugin_host *h = ua->host;
	u32 in1 = synch & 0xffff;
	unsigned int n = 0;
	struct ua_xact *x;
	__le32 *d;
	int ret;

	x = ua_xact_alloc(in->dsp, UA_XACT_PIECE);
	if (!x)
		return -ENOMEM;
	x->epoch = in->epoch;
	ua_xact_inline(x, UA_INST_CMD_SYNCH, synch, 0, 0);
	d = ua_xact_cmd(x, nw);
	if (d)
		n = ua_inst_cmd_copy(d, cmd, nw, in->lease + in->n_res);
	ua_xact_inline(x, UA_INST_CMD_SYNCH, synch, in->lease[in->n_res].base, 0);
	ua_xact_inline(x, UA_INST_CMD_INPUT, in1, UA_INST_INPUT_SPAN, UA_INST_SPAN_NONE);
	ua_xact_inline(x, UA_INST_CMD_INPUT, in1, UA_INST_INPUT_SPAN, UA_INST_SPAN_NONE);
	ua_xact_inline(x, UA_INST_CMD_INPUT, in1, UA_INST_INPUT_ROUTE, UA_INST_ROUTE_NONE);
	mutex_lock(&ua->lock);
	in->started = true;
	in->synch = synch;
	mutex_unlock(&ua->lock);
	ret = ua_xact_run(ua, x, UA_XACT_WAIT_MS);
	mutex_lock(&ua->lock);
	if (x->state >= UA_XACT_PUBLISHED)
		h->dsp[in->dsp].writes += n;
	ua_xact_done(ua, x);
	mutex_unlock(&ua->lock);
	return ret;
}

/**
 * ua_plugin_create - UA_PLUGIN_INST_CREATE
 * @ua: device (no lock held)
 * @uf: the open file that will own it
 * @req: the request (out fields filled; see ua_ioctl.h)
 * @sha: n_resources module hashes
 * @rows: n_private rows (memspec rows: pool, dwords, link, flags)
 *
 * The authorization answer comes first and decides alone whether anything
 * else happens.
 */
static int ua_plugin_create(struct ua_device *ua, struct ua_file *uf,
			    struct ua_plugin_instance *req, const u8 (*sha)[32],
			    const struct ua_memspec_row *rows,
			    const u32 *cmd)
{
	struct ua_plugin_host *h = ua->host;
	struct ua_pool_request *rq = NULL;
	struct ua_plugin_inst *in = NULL;
	unsigned int i, n, dsp = 0;
	u64 epoch;
	int ret, retired;

	if (mutex_lock_killable(&h->mutex))
		return -EINTR;

	/* 1. The device's answer, fresh: nothing else unless it admits */
	ret = ua_plugin_admit(ua, req->render_id, &req->license_state,
			      &req->license_word, &epoch);
	req->epoch = epoch;
	if (ret || (req->flags & UA_PLUGIN_CREATE_F_CHECK))
		goto unlock;

	/* 2. The modules, from the store by hash */
	ret = -ENOMEM;
	in = kzalloc(sizeof(*in), GFP_KERNEL);
	if (!in)
		goto unlock;
	INIT_LIST_HEAD(&in->node);
	in->owner = uf;
	in->render_id = req->render_id;
	in->epoch = epoch;
	in->n_res = req->n_resources;
	in->n_priv = req->n_private;
	in->res = kcalloc(in->n_res, sizeof(*in->res), GFP_KERNEL);
	in->lease = kcalloc(in->n_res + in->n_priv, sizeof(*in->lease),
			    GFP_KERNEL);
	in->ident = kzalloc(in->n_priv, GFP_KERNEL);
	in->rows = kcalloc(in->n_priv, sizeof(*in->rows), GFP_KERNEL);
	n = in->n_res + in->n_priv;
	rq = kcalloc(n, sizeof(*rq), GFP_KERNEL);
	if (!in->res || !in->lease || !in->ident || !in->rows || !rq)
		goto fail;
	memcpy(in->rows, rows, in->n_priv * sizeof(*rows));
	for (i = 0; i < in->n_res; i++) {
		ret = ua_catalog_get(ua, sha[i], &in->res[i]);
		if (ret)
			goto fail;
	}
	ret = ua_inst_check_set(in);
	if (ret)
		goto fail;

	/* 3. Every DSP's last teardown proven before anything new goes out */
	for (i = 0; i < ua->num_dsps && i < UA_HOST_DSPS; i++)
		ua_plugin_fence_wait(ua, i);

	/* 4. One placement, in the answer's epoch */
	for (i = 0; i < in->n_res; i++)
		rq[i] = (struct ua_pool_request){ in->res[i]->info.pool,
			in->res[i]->info.words, in->res[i], true };
	for (i = 0; i < in->n_priv; i++)
		rq[in->n_res + i] = (struct ua_pool_request){ rows[i].pool,
			rows[i].dwords, &in->ident[i], false };
	mutex_lock(&ua->lock);
	for (i = 0; i < in->n_res; i++)
		if (ua_inst_conflicts(h, in->res[i]))
			ret = -EEXIST;
	if (ret == -EEXIST) {
		/* refused: another version of a module the host already uses */
	} else if (h->epoch != epoch) {
		ret = -ESTALE;
	} else {
		u32 need[UA_POOLS] = { 0, req->reserved[4] & 0xffff,
				       req->reserved[4] >> 16, req->reserved[5] };

		ret = 0;
		if (req->flags & UA_PLUGIN_CREATE_F_START) {
			struct ua_plugin_inst *other;

			ret = ua_strips_override_target(ua, req->dsp,
					 req->reserved[0], epoch, &in->strip_generation);
			/* Scratch restoration is not composable across started chains. */
			list_for_each_entry(other, &h->instances, node)
				if (other->dsp == req->dsp && other->placed &&
				    !other->lost &&
				    (other->started || other->strip_override))
					ret = -EBUSY;
			if (!ret) {
				in->strip_override = true;
				in->synch = req->reserved[0];
			}
		}
		if (!ret && (need[1] || need[2] || need[3]))
			ret = ua_inst_scratch_raise(h->dsp[req->dsp].pools, in, need);
		if (!ret)
			ret = ua_plugin_place(ua, req->dsp, rq, n, &dsp, in->lease);
		if (ret && in->scratch_raised)
			ua_inst_scratch_restore(h->dsp[req->dsp].pools, in);
		if (!ret) {
			in->dsp = dsp;
			in->placed = true;
			in->handle = h->next_handle++;
			if (!h->next_handle)
				h->next_handle = 1;
			list_add_tail(&in->node, &h->instances);
			h->n_instances++;
		}
	}
	mutex_unlock(&ua->lock);
	if (ret)
		goto fail;

	/* 5. Modules not resident there, each acknowledged */
	for (i = 0; i < in->n_res; i++) {
		struct ua_cat_entry *e = in->res[i];
		bool resident;

		mutex_lock(&ua->lock);
		resident = ua_catalog_resident(e, dsp, epoch) &&
			   e->res[dsp].base == in->lease[i].base;
		mutex_unlock(&ua->lock);
		if (resident)
			continue;
		ret = ua_plugin_upload(ua, dsp, e, in->lease[i].base,
				       UA_BILL_SDK, epoch);
		if (ret)
			goto takedown;
	}

	/* 6. Private rows zero-filled and linked */
	ret = ua_inst_privates(ua, in, rows);
	if (ret)
		goto takedown;

	/* 7. Bring-up only: parameters, then started on the DSP input */
	if (req->flags & UA_PLUGIN_CREATE_F_START) {
		ret = ua_inst_start(ua, in, cmd, req->reserved[1], req->reserved[0]);
		if (ret)
			goto takedown;
	}

	mutex_lock(&ua->lock);
	if (in->lost) {
		ret = -ESTALE;
	} else {
		in->loaded = true;
		req->handle = in->handle;
		req->dsp = dsp;
	}
	mutex_unlock(&ua->lock);
	if (ret)
		goto takedown;
	dev_info(&ua->pdev->dev,
		 "plug-in host: plug-in 0x%03x instance %u placed on DSP %u (%s)\n",
		 in->render_id, in->handle, dsp,
		 in->started ? "started on its DSP input" : "not routed");
	kfree(rq);
	mutex_unlock(&h->mutex);
	return 0;

takedown:
	mutex_lock(&ua->lock);
	retired = ua_inst_unplace(ua, in, !h->dsp[in->dsp].quarantined, true);
	list_del(&in->node);
	h->n_instances--;
	ua_inst_free(ua, in);
	mutex_unlock(&ua->lock);
	in = NULL;
	if (retired > 0) {
		int fenced = ua_plugin_fence_wait(ua, dsp);

		if (fenced)
			ret = fenced;
	} else if (retired < 0) {
		ret = retired;
	}
	goto unlock;
fail:
	mutex_lock(&ua->lock);
	if (in)
		ua_inst_free(ua, in);
	mutex_unlock(&ua->lock);
unlock:
	kfree(rq);
	mutex_unlock(&h->mutex);
	return ret;
}

/* UA_PLUGIN_INST_DESTROY: released on the DSP, proven, then forgotten */
static int ua_plugin_destroy(struct ua_device *ua, struct ua_file *uf,
			     u32 handle)
{
	struct ua_plugin_host *h = ua->host;
	struct ua_plugin_inst *in;
	unsigned int dsp = 0;
	bool sent = false;
	int ret = 0;

	if (mutex_lock_killable(&h->mutex))
		return -EINTR;
	mutex_lock(&ua->lock);
	in = ua_inst_find(h, uf, handle);
	if (!in) {
		ret = -ENOENT;
	} else {
		dsp = in->dsp;
		ret = ua_inst_unplace(ua, in, !in->lost, true);
		sent = ret > 0;
		if (sent)
			ret = 0;
		list_del(&in->node);
		h->n_instances--;
		ua_inst_free(ua, in);
	}
	mutex_unlock(&ua->lock);
	if (sent)
		ret = ua_plugin_fence_wait(ua, dsp);
	mutex_unlock(&h->mutex);
	return ret;
}

/**
 * ua_plugin_file_release - the open file is closed: its instances go
 * @uf: the file (no lock held)
 */
void ua_plugin_file_release(struct ua_file *uf)
{
	struct ua_device *ua = uf->ua;
	struct ua_plugin_host *h = ua->host;
	struct ua_plugin_inst *in, *tmp;
	u32 fences = 0;
	unsigned int i;
	int retired;

	if (!h || !ua_plugin_host_enabled())
		return;
	mutex_lock(&h->mutex);
	mutex_lock(&ua->lock);
	list_for_each_entry_safe(in, tmp, &h->instances, node) {
		if (in->owner != uf)
			continue;
		retired = ua_inst_unplace(ua, in, !in->lost, true);
		if (retired > 0)
			fences |= BIT(in->dsp);
		else if (retired < 0)
			dev_warn(&ua->pdev->dev,
				 "plug-in close retirement failed (%d)\n", retired);
		list_del(&in->node);
		h->n_instances--;
		ua_inst_free(ua, in);
	}
	mutex_unlock(&ua->lock);
	for (i = 0; i < UA_HOST_DSPS; i++)
		if (fences & BIT(i))
			ua_plugin_fence_wait(ua, i);
	mutex_unlock(&h->mutex);
}


/* SERVICE uses the CREATE layout, never caller-supplied DSP addresses. */
static int ua_plugin_instance_service(struct ua_device *ua, struct ua_file *uf,
				      const struct ua_plugin_instance *req,
				      const u32 *cmd, u32 *values)
{
	struct ua_plugin_host *h = ua->host;
	struct ua_plugin_inst *in;
	struct ua_xact *x = NULL;
	const __le32 *reply;
	__le32 *dest;
	unsigned int nw = req->reserved[1], nr = req->reserved[4];
	unsigned int row = req->reserved[0] >> 24, i, ncmd = 0;
	u32 word = req->reserved[0] & 0xffffff;
	int ret;

	if (mutex_lock_killable(&h->mutex))
		return -EINTR;
	mutex_lock(&ua->lock);
	in = ua_inst_find(h, uf, req->handle);
	if (!in) {
		ret = -ENOENT;
		goto unlock;
	}
	if (in->lost || !in->placed || in->epoch != h->epoch ||
	    req->epoch != in->epoch || !ua_plugin_dsp_admitted(ua, in->dsp)) {
		ret = -ESTALE;
		goto unlock;
	}
	if (!in->loaded || !in->started) {
		ret = -EINVAL;
		goto unlock;
	}
	if (nw) {
		ret = ua_inst_check_cmds(in->rows, in->n_priv, cmd, nw);
		if (ret < 0)
			goto unlock;
	}
	if (nr && (row >= in->n_priv || word >= in->rows[row].dwords ||
		   nr > in->rows[row].dwords - word)) {
		ret = -EINVAL;
		goto unlock;
	}
	x = ua_xact_alloc(in->dsp, nw ? UA_XACT_PIECE : 0);
	if (!x) {
		ret = -ENOMEM;
		goto unlock;
	}
	x->epoch = in->epoch;
	if (nw) {
		dest = ua_xact_cmd(x, nw);
		if (dest)
			ncmd = ua_inst_cmd_copy(dest, cmd, nw, in->lease + in->n_res);
	}
	if (nr) {
		x->rx_fill = 0xffcccccc;
		ua_xact_rx(x, nr + 2);
		ua_xact_inline(x, 0x000c0004,
			       in->lease[in->n_res + row].base + word, nr, word);
	}
	mutex_unlock(&ua->lock);
	ret = ua_xact_run(ua, x, UA_XACT_WAIT_MS);
	mutex_lock(&ua->lock);
	if (x->state >= UA_XACT_PUBLISHED)
		h->dsp[in->dsp].writes += ncmd;
	if (!ret && (in->lost || h->epoch != in->epoch))
		ret = -ESTALE;
	if (!ret && nr) {
		reply = ua_xact_reply(x, 0);
		if (!reply || le32_to_cpu(reply[0]) != (0x80010000u | (nr + 2)) ||
		    le32_to_cpu(reply[1]) != word) {
			ret = -EPROTO;
		} else {
			for (i = 0; i < nr; i++)
				values[i] = le32_to_cpu(reply[i + 2]);
		}
	}
	ua_xact_done(ua, x);
unlock:
	mutex_unlock(&ua->lock);
	mutex_unlock(&h->mutex);
	return ret;
}

static long ua_plugin_ioctl_service(struct ua_device *ua, struct ua_file *uf,
				    const struct ua_plugin_instance *req)
{
	u32 *cmd = NULL, *values = NULL;
	unsigned int nw = req->reserved[1], nr = req->reserved[4];
	long ret = -ENOMEM;

	if (!req->handle || !req->epoch || req->flags || req->render_id ||
	    req->dsp || req->n_resources || req->n_private || req->resources ||
	    req->reserved[5] || nw > UA_PLUGIN_CMD_WORDS_MAX ||
	    (nw && nw < 4) || nr > UA_PLUGIN_READ_WORDS_MAX || (!nw && !nr) ||
	    (!nw && (req->reserved[2] || req->reserved[3])) ||
	    (!nr && (req->reserved[0] || req->privates)) || (nr && !req->privates))
		return -EINVAL;
	if (nw) {
		cmd = kvmalloc_array(nw, sizeof(*cmd), GFP_KERNEL);
		if (!cmd)
			goto out;
		if (copy_from_user(cmd, u64_to_user_ptr((u64)req->reserved[3] << 32 |
						       req->reserved[2]), nw * sizeof(*cmd))) {
			ret = -EFAULT;
			goto out;
		}
	}
	if (nr) {
		values = kvmalloc_array(nr, sizeof(*values), GFP_KERNEL);
		if (!values)
			goto out;
	}
	ret = ua_plugin_instance_service(ua, uf, req, cmd, values);
	if (!ret && nr && copy_to_user(u64_to_user_ptr(req->privates), values,
				      nr * sizeof(*values)))
		ret = -EFAULT;
out:
	kvfree(cmd);
	kvfree(values);
	return ret;
}

/* UA_INSTANCE_END */

/*
 * UA_IOCTL_PLUGIN_INSTANCE.  Out fields are copied back on success and,
 * for CREATE, when the device's answer refused (-EACCES), so the caller
 * sees which state refused it.
 */
long ua_plugin_ioctl_instance(struct ua_device *ua, struct ua_file *uf,
			      unsigned long arg)
{
	struct ua_plugin_instance req;
	struct ua_plugin_private *priv = NULL;
	struct ua_memspec_row *rows = NULL;
	u32 *cmd = NULL;
	u8 (*sha)[32] = NULL;
	unsigned int i, nw = 0;
	long ret;

	if (!ua_plugin_host_enabled())
		return -EOPNOTSUPP;
	if (copy_from_user(&req, (void __user *)arg, sizeof(req)))
		return -EFAULT;
	if (req.struct_size != sizeof(req) || req.license_state || req.license_word)
		return -EINVAL;
	if (req.op == UA_PLUGIN_INST_SERVICE)
		return ua_plugin_ioctl_service(ua, uf, &req);
	if (req.op == UA_PLUGIN_INST_CREATE &&
	    (req.flags & UA_PLUGIN_CREATE_F_START)) {
		nw = req.reserved[1];
		if (req.dsp == UA_PLUGIN_DSP_ANY ||
		    (req.reserved[0] >> 16) != 0x20 ||
		    nw < 4 || nw > UA_PLUGIN_CMD_WORDS_MAX ||
		    ((req.reserved[4] || req.reserved[5]) &&
		     req.dsp == UA_PLUGIN_DSP_ANY))
			return -EINVAL;
	} else if (memchr_inv(req.reserved, 0, sizeof(req.reserved))) {
		return -EINVAL;
	}
	if (req.struct_size != sizeof(req) ||
	    req.epoch || req.license_state || req.license_word)
		return -EINVAL;

	switch (req.op) {
	case UA_PLUGIN_INST_DESTROY:
		if (req.flags || !req.handle)
			return -EINVAL;
		return ua_plugin_destroy(ua, uf, req.handle);
	case UA_PLUGIN_INST_CREATE:
		break;
	default:
		return -EINVAL;
	}

	if (req.handle ||
	    (req.flags & ~(UA_PLUGIN_CREATE_F_CHECK | UA_PLUGIN_CREATE_F_START)) ||
	    (req.dsp != UA_PLUGIN_DSP_ANY && req.dsp >= ua->num_dsps))
		return -EINVAL;
	if (!(req.flags & UA_PLUGIN_CREATE_F_CHECK) &&
	    (!req.n_resources || req.n_resources > UA_PLUGIN_RES_MAX ||
	     !req.n_private || req.n_private > UA_PLUGIN_PRIV_MAX))
		return -EINVAL;
	if (req.flags & UA_PLUGIN_CREATE_F_CHECK) {
		req.n_resources = 0;
		req.n_private = 0;
	} else {
		ret = -ENOMEM;
		sha = kvmalloc_array(req.n_resources, sizeof(*sha), GFP_KERNEL);
		priv = kvmalloc_array(req.n_private, sizeof(*priv), GFP_KERNEL);
		rows = kvmalloc_array(req.n_private, sizeof(*rows), GFP_KERNEL);
		if (!sha || !priv || !rows)
			goto out;
		ret = -EFAULT;
		if (copy_from_user(sha, u64_to_user_ptr(req.resources),
				   req.n_resources * sizeof(*sha)) ||
		    copy_from_user(priv, u64_to_user_ptr(req.privates),
				   req.n_private * sizeof(*priv)))
			goto out;
		for (i = 0; i < req.n_private; i++)
			rows[i] = (struct ua_memspec_row){ priv[i].pool,
				priv[i].dwords, priv[i].link, priv[i].flags };
		/* the rows are checked whole before the device is asked */
		ret = ua_inst_check_rows(rows, req.n_private);
		if (ret)
			goto out;
		if (nw) {
			ret = -ENOMEM;
			cmd = kvmalloc_array(nw, sizeof(*cmd), GFP_KERNEL);
			if (!cmd)
				goto out;
			ret = -EFAULT;
			if (copy_from_user(cmd, u64_to_user_ptr((u64)req.reserved[3] << 32 |
							       req.reserved[2]),
					   nw * sizeof(*cmd)))
				goto out;
			/* UA's WRITEs, inside their rows, clear of the links */
			ret = ua_inst_check_cmds(rows, req.n_private, cmd, nw);
			if (ret < 0)
				goto out;
		}
	}

	ret = ua_plugin_create(ua, uf, &req, (const u8 (*)[32])sha, rows, cmd);
	if ((!ret || ret == -EACCES) &&
	    copy_to_user((void __user *)arg, &req, sizeof(req)))
		ret = -EFAULT;
out:
	kvfree(sha);
	kvfree(priv);
	kvfree(rows);
	kvfree(cmd);
	return ret;
}
