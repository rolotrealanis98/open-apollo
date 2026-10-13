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
 * Nothing is routed or started: no audio reaches the instance yet (the
 * chain, SYNCH and parameters are later work).  Any failure after
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
	unsigned int n_res, n_priv;
	struct ua_cat_entry **res;	/* catalog references */
	struct ua_pool_lease *lease;	/* n_res modules, then n_priv rows */
	u8 *ident;			/* n_priv: one identity per private row */
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
	kfree(in);
}

/*
 * Take @in off its DSP (ua->lock held): the modules whose last hold it
 * had are released on the DSP, everything goes back to the ledger.  With
 * @send the release words are published as the DSP's fence (no wait);
 * without, or when the DSP is not the host's any more, nothing is sent.
 * Returns the number of releases published.
 */
static unsigned int ua_inst_unplace(struct ua_device *ua,
				    struct ua_plugin_inst *in, bool send)
{
	struct ua_plugin_host *h = ua->host;
	struct ua_host_dsp *d = &h->dsp[in->dsp];
	struct ua_xact *x = NULL;
	unsigned int i, rels = 0;

	if (!in->placed)
		return 0;
	in->placed = false;
	if (!d->pools)
		return 0;		/* the ledger went with the DSP */
	if (send && d->owned)
		x = ua_xact_alloc(in->dsp, 0);
	if (x)
		x->epoch = h->epoch;
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
	if (!x)
		return 0;
	if (!rels) {
		ua_xact_discard(ua, x);
		return 0;
	}
	if (ua_plugin_publish_now(ua, x)) {
		ua_plugin_disown(ua, in->dsp, "a plug-in teardown could not be sent");
		return 0;
	}
	d->releases += rels;
	ua_plugin_fence_set(ua, in->dsp, x);
	return rels;
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
		ua_inst_unplace(ua, in, true);
		in->lost = true;
		in->loaded = false;
		n++;
	}
	if (n)
		dev_info(&ua->pdev->dev,
			 "plug-in host: %u instance(s) taken down (%s)\n", n, why);
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
		ua_xact_inline(x, UA_CMD_ZERO, addr[i], 0, rows[i].dwords);
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
			    const struct ua_memspec_row *rows)
{
	struct ua_plugin_host *h = ua->host;
	struct ua_pool_request *rq = NULL;
	struct ua_plugin_inst *in = NULL;
	unsigned int i, n, dsp = 0;
	u64 epoch;
	int ret;

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
	n = in->n_res + in->n_priv;
	rq = kcalloc(n, sizeof(*rq), GFP_KERNEL);
	if (!in->res || !in->lease || !in->ident || !rq)
		goto fail;
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
	if (h->epoch != epoch) {
		ret = -ESTALE;
	} else {
		ret = ua_plugin_place(ua, req->dsp, rq, n, &dsp, in->lease);
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
		 "plug-in host: plug-in 0x%03x instance %u placed on DSP %u (not routed)\n",
		 in->render_id, in->handle, dsp);
	kfree(rq);
	mutex_unlock(&h->mutex);
	return 0;

takedown:
	mutex_lock(&ua->lock);
	ua_inst_unplace(ua, in, !h->dsp[in->dsp].quarantined);
	list_del(&in->node);
	h->n_instances--;
	ua_inst_free(ua, in);
	mutex_unlock(&ua->lock);
	in = NULL;
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
		sent = ua_inst_unplace(ua, in, !in->lost) > 0;
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

	if (!h || !ua_plugin_host_enabled())
		return;
	mutex_lock(&h->mutex);
	mutex_lock(&ua->lock);
	list_for_each_entry_safe(in, tmp, &h->instances, node) {
		if (in->owner != uf)
			continue;
		if (ua_inst_unplace(ua, in, !in->lost))
			fences |= BIT(in->dsp);
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
	u8 (*sha)[32] = NULL;
	unsigned int i;
	long ret;

	if (!ua_plugin_host_enabled())
		return -EOPNOTSUPP;
	if (copy_from_user(&req, (void __user *)arg, sizeof(req)))
		return -EFAULT;
	if (req.struct_size != sizeof(req) ||
	    memchr_inv(req.reserved, 0, sizeof(req.reserved)) ||
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

	if (req.handle || (req.flags & ~UA_PLUGIN_CREATE_F_CHECK) ||
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
	}

	ret = ua_plugin_create(ua, uf, &req, (const u8 (*)[32])sha, rows);
	if ((!ret || ret == -EACCES) &&
	    copy_to_user((void __user *)arg, &req, sizeof(req)))
		ret = -EFAULT;
out:
	kvfree(sha);
	kvfree(priv);
	kvfree(rows);
	return ret;
}
