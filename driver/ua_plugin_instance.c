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

#include <linux/delay.h>
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
	bool route_pending;		/* START reserves its inputs before publication */
	u32 synch;			/* in1 | in2 << 16 */
	bool unison, analog_owned, unison_begun;
	struct ua_unison_image analog_base, analog_current, analog_rollback;
	bool strip_override;		/* reserves the retained plain strip */
	u32 strip_generation;
	u32 scratch_base[UA_POOLS];	/* baseline shared by this DSP's claims */
	u32 scratch_need[UA_POOLS];	/* this chain's minimum scratch */
	bool scratch_claim;
	unsigned int n_res, n_priv;
	struct ua_cat_entry **res;	/* catalog references */
	struct ua_pool_lease *lease;	/* n_res modules, then n_priv rows */
	u8 *ident;			/* n_priv: one identity per private row */
	struct ua_memspec_row *rows;	/* immutable bounds and link ownership */
};

/* ua->lock held; generic ALSA/ioctl/raw writes cannot cross an owned lane. */
bool ua_instances_analog_conflict(struct ua_device *ua, unsigned int index, u32 mask)
{
	struct ua_plugin_host *h = ua->host;
	struct ua_plugin_inst *in;
	if (!h || h->analog_write || index >= UA_UNISON_SETTINGS)
		return false;
	if (h->analog_block[index] & mask)
		return true;
	list_for_each_entry (in, &h->instances, node)
		if (in->analog_owned && (in->analog_base.mask[index] & mask))
			return true;
	return false;
}

static void ua_inst_analog_failed(struct ua_device *ua, struct ua_plugin_inst *in, int ret)
{
	unsigned int i;
	for (i = 0; i < UA_UNISON_SETTINGS; i++)
		ua->host->analog_block[i] |= in->analog_base.mask[i];
	in->analog_owned = false;
	ua_plugin_quarantine(ua, in->dsp, "Unison analog completion unproven", ret);
}

/* Original pre-insert PGA0 and 80ms. CUSTOM remains an explicit later journal
 * after userspace has resumed the original worker and activated the graph. */
static int ua_inst_analog_bootstrap(struct ua_device *ua, struct ua_plugin_inst *in, u32 *seq)
{
	u32 words[6], input[2] = {in->synch & 0xffff, in->synch >> 16};
	unsigned int k, nw = 0;
	int ret;
	for (k = 0; k < 2; k++) {
		if (input[k] == UA_INST_SPAN_NONE)
			continue;
		words[nw++] = input[k];
		words[nw++] = 6u; /* GainC */
		words[nw++] = 0u;
	}
	mutex_lock(&ua->lock);
	ret = (in->lost || !in->analog_owned || ua->host->epoch != in->epoch ||
	       !ua_plugin_dsp_admitted(ua, in->dsp))
		  ? -ESTALE
		  : 0;
	if (!ret)
		ret = ua_unison_patch_locked(ua, &in->analog_current, words, nw);
	mutex_unlock(&ua->lock);
	if (!ret)
		ret = ua_unison_drain(ua, in->epoch, seq);
	if (!ret)
		msleep(80);
	mutex_lock(&ua->lock);
	if (!ret &&
	    (in->lost || ua->host->epoch != in->epoch || !ua_plugin_dsp_admitted(ua, in->dsp)))
		ret = -ESTALE;
	if (ret)
		ua_inst_analog_failed(ua, in, ret);
	mutex_unlock(&ua->lock);
	return ret;
}

/* Route retirement must be acknowledged before ordinary analog restoration. */
static int ua_inst_analog_restore(struct ua_device *ua, struct ua_plugin_inst *in, int ret)
{
	bool owns;
	mutex_lock(&ua->lock);
	owns = in->analog_owned;
	if (owns && !ret && in->lost)
		ret = -ESTALE;
	mutex_unlock(&ua->lock);
	if (owns && !ret)
		ret = ua_unison_restore(ua, in->epoch, &in->analog_base);
	mutex_lock(&ua->lock);
	if (owns && ret)
		ua_inst_analog_failed(ua, in, ret);
	else if (owns) {
		ua_unison_set_owned_locked(ua, in->synch, false);
		in->analog_owned = false;
	}
	mutex_unlock(&ua->lock);
	return ret;
}

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

/*
 * Scratch is shared by sequential chains on a DSP. Every claimant retains
 * the same pre-chain baseline; releasing any claimant recomputes the maximum
 * over the survivors. A previous-value stack is unsafe for out-of-order close.
 * ua->lock held, and host->mutex serializes placement and retirement.
 */
static void ua_inst_scratch_restore(struct ua_plugin_host *h,
                                    struct ua_plugin_inst *in)
{
	struct ua_pool_owner *o = h->dsp[in->dsp].pools;
	struct ua_plugin_inst *other;
	u32 need[UA_POOLS];
	unsigned int p;

	if (!in->scratch_claim)
		return;
	in->scratch_claim = false;
	if (!o)
		return;
	memcpy(need, in->scratch_base, sizeof(need));
	list_for_each_entry(other, &h->instances, node)
		if (other->dsp == in->dsp && other->scratch_claim)
			for (p = 1; p < UA_POOLS; p++)
				need[p] = max_t(u32, need[p], other->scratch_need[p]);
	/* These are only reductions from already-reserved scratch. */
	for (p = 1; p < UA_POOLS; p++)
		ua_pool_set_scratch(o, p, need[p]);
}

/* Claim the maximum scratch before placing any rows; nothing is sent to DSP. */
static int ua_inst_scratch_raise(struct ua_plugin_host *h,
                                 struct ua_plugin_inst *in,
                                 const u32 need[UA_POOLS])
{
	struct ua_pool_owner *o = h->dsp[in->dsp].pools;
	struct ua_plugin_inst *other;
	struct ua_pool_usage u[UA_POOLS];
	unsigned int p;
	int ret;

	if (!o)
		return -ENODEV;
	ret = ua_pool_read_usage(o, u);
	if (ret)
		return ret;
	for (p = 1; p < UA_POOLS; p++)
		in->scratch_base[p] = u[p].scratch_words;
	list_for_each_entry(other, &h->instances, node)
		if (other->dsp == in->dsp && other->scratch_claim) {
			memcpy(in->scratch_base, other->scratch_base,
			       sizeof(in->scratch_base));
			break;
		}
	memcpy(in->scratch_need, need, sizeof(in->scratch_need));
	in->scratch_claim = true;
	for (p = 1; p < UA_POOLS; p++) {
		if (need[p] <= u[p].scratch_words)
			continue;
		ret = ua_pool_set_scratch(o, p, need[p]);
		if (ret) {
			ua_inst_scratch_restore(h, in);
			return ret;
		}
	}
	return 0;
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
	ua_inst_scratch_restore(h, in);
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
		if (in->analog_owned)
			ua_inst_analog_failed(ua, in, -ESTALE);
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
		if (in->analog_owned)
			ua_inst_analog_failed(ua, in, -ESTALE);
		/* the ledger is about to go: forget the leases, send nothing */
		memset(in->lease, 0, (in->n_res + in->n_priv) * sizeof(*in->lease));
		in->placed = false;
		in->scratch_claim = false;
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

/* Recover the original head directory from checked initialization WRITEs.
 * No caller-provided license list can omit a directory member. Head word 5
 * is the sub count; each pointer/ID pair begins at word 6. The first sub is
 * the built-in Mic strip (an analog input) or Input Strip (a Virtual input),
 * followed by at most four licensed effects, or (UNISON) a licensed
 * integrated header followed by zero to four effects.  @carrier: the strip
 * the instance replaces must run (0x846 Mic, also for UNISON and a plain
 * START; 0x83E Input), checked against the inputs at CREATE.
 */
static int ua_inst_chain_ids(const struct ua_memspec_row *rows, unsigned int nr,
			     const u32 *cmd, unsigned int nw, bool chain, bool unison,
			     u32 primary, u32 ids[5], unsigned int *nids, u32 *carrier)
{
	u32 head[16] = {0};
	u8 seen[16] = {0};
	unsigned int i, k, pos, len, n, j, links, first = unison ? 0 : 1;

	*nids = 0;
	*carrier = 0x846;
	for (i = 0; i < nw; i += len) {
		len = cmd[i] & 0xffff;
		if (cmd[i + 1] >> 24)
			continue;
		pos = cmd[i + 1] & 0xffffff;
		for (k = 0; k < len - 3; k++, pos++) {
			while (pos > (cmd[i + 1] & 0xffffff) &&
			       pos - (cmd[i + 1] & 0xffffff) - 1 < 32 &&
			       (cmd[i + 2] >> (pos - (cmd[i + 1] & 0xffffff) - 1)) & 1)
				pos++;
			if (pos < ARRAY_SIZE(head)) {
				if (seen[pos] && (pos == 0 || pos == 5 ||
						 (pos >= 7 && (pos & 1))))
					return -EINVAL;
				seen[pos] = 1;
				head[pos] = cmd[i + 3 + k];
			}
		}
	}
	if (!chain)
		return seen[5] && head[5] > 2 ? -EOPNOTSUPP : 0;
	n = head[5];
	if (!seen[0] || head[0] != 0x40000000 || !seen[5] ||
	    n < (unison ? 1 : 2) || n > 5 ||
	    rows[0].pool != 1 || rows[0].dwords != 6 + 2 * n ||
	    !seen[7] || (!unison && head[7] != 0x846 && head[7] != 0x83e))
		return -EINVAL;
	if (!unison)
		*carrier = head[7];
	for (i = 0; i < n; i++) {
		if (!seen[7 + 2 * i])
			return -EINVAL;
		links = 0;
		for (j = 1; j < nr; j++)
			if (rows[j].link == 6 + 2 * i)
				links++;
		if (links != 1)
			return -EINVAL;
		if (i >= first) {
			if (head[7 + 2 * i] < UA_BOARD_QUERY_BASE ||
			    head[7 + 2 * i] >= UA_BOARD_QUERY_BASE + UA_BOARD_QUERY_WORDS)
				return -EACCES;
			ids[i - first] = head[7 + 2 * i];
		}
	}
	if (ids[0] != primary)
		return -EINVAL;
	*nids = n - first;
	return 0;
}

/* Topology changes require a freshly admitted allocation, never SERVICE. */
static bool ua_inst_writes_directory(const u32 *cmd, unsigned int nw)
{
	unsigned int i, k, pos, word, len;

	for (i = 0; i < nw; i += len) {
		len = cmd[i] & 0xffff;
		if (cmd[i + 1] >> 24)
			continue;
		word = cmd[i + 1] & 0xffffff;
		for (k = 0, pos = word; k < len - 3; k++, pos++) {
			while (pos > word && pos - word - 1 < 32 &&
			       (cmd[i + 2] >> (pos - word - 1)) & 1)
				pos++;
			if (pos == 0 || pos == 5 || (pos >= 7 && (pos & 1)))
				return true;
		}
	}
	return false;
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

		/* SDK LoadPlugin maps kinds 0..3 directly to their pools;
		 * kind 4 maps to pool 1. Nonzero pools use the DATA path.
		 * Kind 3 is qualified for ordinary, non-overwritable records.
		 */
		if (b->kind > 4 || (b->kind == 3 && b->flags))
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
static int ua_inst_start(struct ua_device *ua, struct ua_plugin_inst *in, const u32 *cmd,
			 unsigned int nw, u32 synch, bool start)
{
	struct ua_plugin_host *h = ua->host;
	u32 input[2] = {synch & 0xffff, synch >> 16};
	unsigned int k;
	unsigned int n = 0;
	struct ua_xact *x;
	__le32 *d;
	int ret;

	if (start && in->unison && (!in->analog_owned || !in->unison_begun))
		return -EPERM;
	x = ua_xact_alloc(in->dsp, UA_XACT_PIECE);
	if (!x)
		return -ENOMEM;
	x->epoch = in->epoch;
	for (k = 0; start && k < 2; k++)
		if (input[k] != UA_INST_SPAN_NONE)
			ua_xact_inline(x, UA_INST_CMD_SYNCH, UA_INST_SPAN_NONE << 16 | input[k], 0,
				       0);
	if (nw) {
		d = ua_xact_cmd(x, nw);
		if (d)
			n = ua_inst_cmd_copy(d, cmd, nw, in->lease + in->n_res);
	}
	if (start)
		ua_xact_inline(x, UA_INST_CMD_SYNCH, synch, in->lease[in->n_res].base, 0);
	for (k = 0; start && k < 2; k++) {
		if (input[k] == UA_INST_SPAN_NONE)
			continue;
		ua_xact_inline(x, UA_INST_CMD_INPUT, input[k], UA_INST_INPUT_SPAN,
			       UA_INST_SPAN_NONE);
		ua_xact_inline(x, UA_INST_CMD_INPUT, input[k], UA_INST_INPUT_SPAN,
			       UA_INST_SPAN_NONE);
		ua_xact_inline(x, UA_INST_CMD_INPUT, input[k], UA_INST_INPUT_ROUTE,
			       UA_INST_ROUTE_NONE);
	}
	mutex_lock(&ua->lock);
	in->started = start;
	in->route_pending = false;
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

/* Both actual SYNCH inputs are owned; 0x20 is the mono sentinel. */
static bool ua_inst_inputs_overlap(u32 a, u32 b)
{
	u32 a1 = a & 0xffff, a2 = a >> 16;
	u32 b1 = b & 0xffff, b2 = b >> 16;

	return a1 == b1 || a1 == b2 || a2 == b1 ||
	       (a2 != UA_INST_SPAN_NONE && a2 == b2);
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
			    const struct ua_memspec_row *rows, const u32 *cmd, const u32 *ids,
			    unsigned int nids, u32 carrier)
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
	if (nids)
		ret = ua_plugin_admit_chain(ua, ids, nids, &req->license_state, &req->license_word,
					    &epoch);
	else
		ret = ua_plugin_admit(ua, req->render_id, &req->license_state, &req->license_word,
				      &epoch);
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
	in->unison = !!(req->flags & UA_PLUGIN_CREATE_F_UNISON);
	in->render_id = req->render_id;
	in->epoch = epoch;
	in->n_res = req->n_resources;
	in->n_priv = req->n_private;
	in->res = kcalloc(in->n_res, sizeof(*in->res), GFP_KERNEL);
	in->lease = kcalloc(in->n_res + in->n_priv, sizeof(*in->lease), GFP_KERNEL);
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
		rq[i] = (struct ua_pool_request){in->res[i]->info.pool, in->res[i]->info.words,
						 in->res[i], true};
	for (i = 0; i < in->n_priv; i++)
		rq[in->n_res + i] =
		    (struct ua_pool_request){rows[i].pool, rows[i].dwords, &in->ident[i], false};
	mutex_lock(&ua->lock);
	for (i = 0; i < in->n_res; i++)
		if (ua_inst_conflicts(h, in->res[i]))
			ret = -EEXIST;
	if (ret == -EEXIST) {
		/* refused: another version of a module the host already uses */
	} else if (h->epoch != epoch) {
		ret = -ESTALE;
	} else {
		u32 need[UA_POOLS] = {0, req->reserved[4] & 0xffff, req->reserved[4] >> 16,
				      req->reserved[5]};

		ret = 0;
		if (req->flags & (UA_PLUGIN_CREATE_F_START | UA_PLUGIN_CREATE_F_STAGE)) {
			struct ua_plugin_inst *other;

			ret = ua_strips_override_target(ua, req->dsp, req->reserved[0], epoch,
							&in->strip_generation, carrier);
			/* Each input has exactly one routed owner, regardless of file. */
			list_for_each_entry (other, &h->instances, node)
				if ((req->flags & UA_PLUGIN_CREATE_F_START) &&
				    other->dsp == req->dsp && other->placed &&
				    ua_inst_inputs_overlap(other->synch, req->reserved[0]) &&
				    !other->lost && (other->started || other->route_pending))
					ret = -EBUSY;
			if (!ret) {
				in->strip_override = true;
				in->route_pending = !!(req->flags & UA_PLUGIN_CREATE_F_START);
				in->synch = req->reserved[0];
			}
		}
		in->dsp = req->dsp;
		if (!ret && (need[1] || need[2] || need[3]))
			ret = ua_inst_scratch_raise(h, in, need);
		if (!ret)
			ret = ua_plugin_place(ua, req->dsp, rq, n, &dsp, in->lease);
		if (ret && in->scratch_claim)
			ua_inst_scratch_restore(h, in);
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
		resident =
		    ua_catalog_resident(e, dsp, epoch) && e->res[dsp].base == in->lease[i].base;
		mutex_unlock(&ua->lock);
		if (resident)
			continue;
		ret = ua_plugin_upload(ua, dsp, e, in->lease[i].base, UA_BILL_SDK, epoch);
		if (ret)
			goto takedown;
	}

	/* 6. Private rows zero-filled and linked */
	ret = ua_inst_privates(ua, in, rows);
	if (ret)
		goto takedown;

	/* 7. Bring-up only: parameters, then started on the DSP input */
	if (req->flags & (UA_PLUGIN_CREATE_F_START | UA_PLUGIN_CREATE_F_STAGE)) {
		ret = ua_inst_start(ua, in, cmd, req->reserved[1], req->reserved[0],
				    !!(req->flags & UA_PLUGIN_CREATE_F_START));
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
	dev_info(&ua->pdev->dev, "plug-in host: plug-in 0x%03x instance %u placed on DSP %u (%s)\n",
		 in->render_id, in->handle, dsp,
		 in->started ? "started on its DSP input" : "not routed");
	kfree(rq);
	mutex_unlock(&h->mutex);
	return 0;

takedown:
	mutex_lock(&ua->lock);
	retired = ua_inst_unplace(ua, in, !h->dsp[in->dsp].quarantined, true);
	mutex_unlock(&ua->lock);
	if (retired > 0) {
		int fenced = ua_plugin_fence_wait(ua, dsp);

		if (fenced) {
			ret = fenced;
			retired = fenced;
		}
	} else if (retired < 0) {
		ret = retired;
	}
	retired = ua_inst_analog_restore(ua, in, retired < 0 ? retired : 0);
	if (retired)
		ret = retired;
	mutex_lock(&ua->lock);
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

/* host->mutex held; retain lane ownership until both fences complete. */
static int ua_inst_retire(struct ua_device *ua, struct ua_plugin_inst *in)
{
	int ret;
	mutex_lock(&ua->lock);
	ret = ua_inst_unplace(ua, in, !in->lost, true);
	mutex_unlock(&ua->lock);
	if (ret > 0)
		ret = ua_plugin_fence_wait(ua, in->dsp);
	ret = ua_inst_analog_restore(ua, in, ret);
	mutex_lock(&ua->lock);
	list_del(&in->node);
	ua->host->n_instances--;
	ua_inst_free(ua, in);
	mutex_unlock(&ua->lock);
	return ret;
}

static int ua_plugin_destroy(struct ua_device *ua, struct ua_file *uf, u32 handle)
{
	struct ua_plugin_host *h = ua->host;
	struct ua_plugin_inst *in;
	int ret;
	if (mutex_lock_killable(&h->mutex))
		return -EINTR;
	mutex_lock(&ua->lock);
	in = ua_inst_find(h, uf, handle);
	mutex_unlock(&ua->lock);
	ret = in ? ua_inst_retire(ua, in) : -ENOENT;
	mutex_unlock(&h->mutex);
	return ret;
}

void ua_plugin_file_release(struct ua_file *uf)
{
	struct ua_device *ua = uf->ua;
	struct ua_plugin_host *h = ua->host;
	struct ua_plugin_inst *in, *tmp;
	if (!h || !ua_plugin_host_enabled())
		return;
	mutex_lock(&h->mutex);
	list_for_each_entry_safe (in, tmp, &h->instances, node) {
		if (in->owner == uf)
			ua_inst_retire(ua, in);
	}
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
	u32 word = req->reserved[0] & 0xffffff, probe[4];
	bool resync = false;
	unsigned int dsp = 0;
	u64 epoch = 0;
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
	if (!in->loaded || (!in->started && !in->strip_override)) {
		ret = -EINVAL;
		goto unlock;
	}
	if (nw) {
		ret = ua_inst_check_cmds(in->rows, in->n_priv, cmd, nw);
		if (ret < 0)
			goto unlock;
		if (in->strip_override && ua_inst_writes_directory(cmd, nw)) {
			ret = -EINVAL;
			goto unlock;
		}
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
			dev_warn(&ua->pdev->dev,
				 "plug-in host: DSP %u SERVICE read of %u words at row %u word %u (DSP address 0x%x) got a reply that is not its own\n",
				 in->dsp, nr, row, word,
				 in->lease[in->n_res + row].base + word);
			ua_xact_report_reply(ua, x, 0, "SERVICE read");
			/* A different length than the refused read's: its late
			 * reply cannot pass for the probe's.
			 */
			probe[0] = 0x000c0004;
			probe[1] = in->lease[in->n_res + row].base + word;
			probe[2] = nr == 1 && in->rows[row].dwords - word >= 2 ? 2 : 1;
			probe[3] = word;
			dsp = in->dsp;
			epoch = in->epoch;
			resync = true;
		} else {
			for (i = 0; i < nr; i++)
				values[i] = le32_to_cpu(reply[i + 2]);
		}
	}
	ua_xact_done(ua, x);
unlock:
	mutex_unlock(&ua->lock);
	if (resync)
		ua_xact_resync_replies(ua, dsp, epoch, probe,
				       0x80010000u | (probe[2] + 2));
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

/* A reversible route handoff. Neither allocation is retired by SWAP. */
static int ua_plugin_swap(struct ua_device *ua, struct ua_file *uf,
			  const struct ua_plugin_instance *req)
{
	struct ua_plugin_host *h = ua->host;
	struct ua_plugin_inst *from, *to;
	struct ua_xact *x = NULL;
	u32 generation, input[2];
	unsigned int k;
	int ret;

	if (!req->handle || !req->epoch || !req->reserved[0] || req->handle == req->reserved[0] ||
	    req->flags || req->render_id || req->dsp || req->n_resources || req->n_private ||
	    req->resources || req->privates ||
	    memchr_inv(req->reserved + 1, 0, sizeof(req->reserved) - sizeof(u32)))
		return -EINVAL;
	if (mutex_lock_killable(&h->mutex))
		return -EINTR;
	mutex_lock(&ua->lock);
	from = ua_inst_find(h, uf, req->handle);
	to = ua_inst_find(h, uf, req->reserved[0]);
	if (!from || !to) {
		ret = -ENOENT;
		goto unlock;
	}
	if (from->lost || to->lost || !from->placed || !to->placed || from->epoch != req->epoch ||
	    to->epoch != req->epoch || h->epoch != req->epoch ||
	    !ua_plugin_dsp_admitted(ua, from->dsp)) {
		ret = -ESTALE;
		goto unlock;
	}
	if (!from->loaded || !to->loaded || !from->started || to->started ||
	    !from->strip_override || !to->strip_override || from->dsp != to->dsp ||
	    from->synch != to->synch) {
		ret = -EINVAL;
		goto unlock;
	}
	ret = ua_strips_override_target(ua, from->dsp, from->synch, req->epoch, &generation, 0);
	if (ret)
		goto unlock;
	if (generation != from->strip_generation || generation != to->strip_generation) {
		ret = -ESTALE;
		goto unlock;
	}
	if (to->unison) {
		if (from->analog_owned) {
			to->analog_base = from->analog_base;
			if (!to->analog_current.valid)
				to->analog_current = from->analog_current;
		} else if (!to->analog_owned || !to->unison_begun) {
			ret = -EPERM;
			goto unlock;
		}
	}
	x = ua_xact_alloc(from->dsp, 0);
	if (!x) {
		ret = -ENOMEM;
		goto unlock;
	}
	x->epoch = req->epoch;
	ua_xact_inline(x, UA_INST_CMD_SYNCH, from->synch, 0, 0);
	ua_xact_inline(x, UA_INST_CMD_SYNCH, to->synch, to->lease[to->n_res].base, 0);
	input[0] = to->synch & 0xffff;
	input[1] = to->synch >> 16;
	for (k = 0; k < 2; k++) {
		if (input[k] == UA_INST_SPAN_NONE)
			continue;
		ua_xact_inline(x, UA_INST_CMD_INPUT, input[k], UA_INST_INPUT_SPAN,
			       UA_INST_SPAN_NONE);
		ua_xact_inline(x, UA_INST_CMD_INPUT, input[k], UA_INST_INPUT_SPAN,
			       UA_INST_SPAN_NONE);
		ua_xact_inline(x, UA_INST_CMD_INPUT, input[k], UA_INST_INPUT_ROUTE,
			       UA_INST_ROUTE_NONE);
	}
	mutex_unlock(&ua->lock);
	ret = ua_xact_run(ua, x, UA_XACT_WAIT_MS);
	mutex_lock(&ua->lock);
	if (!ret && (from->lost || to->lost || h->epoch != req->epoch))
		ret = -ESTALE;
	if (!ret) {
		/* Ownership is transferred only after the route transaction is proven. */
		if (to->unison) {
			if (from->analog_owned)
				to->analog_base = from->analog_base;
			ret = ua_unison_apply_locked(ua, &to->analog_current);
		} else if (from->analog_owned) {
			mutex_unlock(&ua->lock);
			ret = ua_unison_restore(ua, req->epoch, &from->analog_base);
			mutex_lock(&ua->lock);
		}
		if (!ret && to->unison) {
			mutex_unlock(&ua->lock);
			ret = ua_unison_drain(ua, req->epoch, NULL);
			mutex_lock(&ua->lock);
		}
		if (!ret) {
			if (from->analog_owned && !to->unison)
				ua_unison_set_owned_locked(ua, from->synch, false);
			from->analog_owned = false;
			to->analog_owned = to->unison;
			from->started = false;
			to->started = true;
			to->unison_begun = false;
			to->route_pending = false;
		}
	}
	if (ret && (from->analog_owned || to->unison)) {
		ua_inst_analog_failed(ua, from, ret);
		ua_inst_analog_failed(ua, to, ret);
	}
	ua_xact_done(ua, x);
unlock:
	mutex_unlock(&ua->lock);
	mutex_unlock(&h->mutex);
	return ret;
}

/*
 * UA_PLUGIN_INST_START: route an initialized STAGE instance on its inputs, as
 * CREATE with START would have, when no instance owns them. A chain whose
 * startup is over one command page is created STAGE and initialized with
 * SERVICE writes; without an anchor to SWAP from it would never be routed.
 */
static int ua_plugin_start_staged(struct ua_device *ua, struct ua_file *uf,
				  const struct ua_plugin_instance *req)
{
	struct ua_plugin_host *h = ua->host;
	struct ua_plugin_inst *in, *other;
	u32 generation;
	int ret;

	if (!req->handle || !req->epoch || req->flags || req->render_id || req->dsp ||
	    req->n_resources || req->n_private || req->resources || req->privates ||
	    memchr_inv(req->reserved, 0, sizeof(req->reserved)))
		return -EINVAL;
	if (mutex_lock_killable(&h->mutex))
		return -EINTR;
	mutex_lock(&ua->lock);
	in = ua_inst_find(h, uf, req->handle);
	ret = !in ? -ENOENT : 0;
	if (!ret && (in->unison || !in->loaded || in->started || in->route_pending ||
		     !in->strip_override))
		ret = -EPERM;
	if (!ret && (in->lost || !in->placed || in->epoch != req->epoch ||
		     h->epoch != req->epoch || !ua_plugin_dsp_admitted(ua, in->dsp)))
		ret = -ESTALE;
	if (!ret) {
		list_for_each_entry (other, &h->instances, node)
			if (other != in && other->placed && !other->lost &&
			    (other->started || other->route_pending) &&
			    ua_inst_inputs_overlap(in->synch, other->synch)) {
				ret = -EBUSY;
				break;
			}
	}
	if (!ret)
		ret = ua_strips_override_target(ua, in->dsp, in->synch, req->epoch, &generation, 0);
	if (!ret && generation != in->strip_generation)
		ret = -ESTALE;
	if (ret)
		goto out;
	mutex_unlock(&ua->lock);
	ret = ua_inst_start(ua, in, NULL, 0, in->synch, true);
	mutex_lock(&ua->lock);
	if (!ret && (in->lost || h->epoch != req->epoch))
		ret = -ESTALE;
	if (!ret)
		dev_info(&ua->pdev->dev, "plug-in host: instance %u started on its DSP input\n",
			 in->handle);
out:
	mutex_unlock(&ua->lock);
	mutex_unlock(&h->mutex);
	return ret;
}

static long ua_plugin_unison_lifecycle(struct ua_device *ua, struct ua_file *uf,
				       struct ua_plugin_instance *req)
{
	struct ua_plugin_host *h = ua->host;
	struct ua_plugin_inst *in, *other;
	u32 generation, seq = 0;
	int ret;
	bool begin = req->op == UA_PLUGIN_INST_BEGIN_UNISON;
	if (!req->handle || !req->epoch || req->flags || req->render_id || req->dsp ||
	    req->n_resources || req->n_private || req->resources || req->privates ||
	    memchr_inv(req->reserved, 0, sizeof(req->reserved)))
		return -EINVAL;
	if (mutex_lock_killable(&h->mutex))
		return -EINTR;
	mutex_lock(&ua->lock);
	in = ua_inst_find(h, uf, req->handle);
	ret = !in ? -ENOENT : 0;
	if (!ret && (in->lost || !in->placed || in->epoch != req->epoch || h->epoch != req->epoch ||
		     !ua_plugin_dsp_admitted(ua, in->dsp)))
		ret = -ESTALE;
	if (!ret && (!in->unison || !in->loaded || in->started))
		ret = -EPERM;
	if (ret)
		goto out;
	ret = ua_strips_override_target(ua, in->dsp, in->synch, req->epoch, &generation, 0);
	if (ret)
		goto out;
	if (generation != in->strip_generation) {
		ret = -ESTALE;
		goto out;
	}
	if (begin) {
		list_for_each_entry (other, &h->instances, node) {
			if (other != in && other->owner != uf && other->placed && !other->lost &&
			    (other->started || other->route_pending) &&
			    ua_inst_inputs_overlap(in->synch, other->synch)) {
				ret = -EBUSY;
				goto out;
			}
		}
		if (in->analog_owned || in->unison_begun) {
			ret = -EALREADY;
			goto out;
		}
		mutex_unlock(&ua->lock);
		ret = ua_unison_drain(ua, in->epoch, NULL);
		mutex_lock(&ua->lock);
		if (!ret && (in->lost || h->epoch != req->epoch))
			ret = -ESTALE;
		if (!ret)
			ret = ua_unison_capture_locked(ua, in->synch, &in->analog_base);
		if (!ret) {
			in->analog_rollback = in->analog_current;
			in->analog_current = in->analog_base;
			in->analog_owned = true;
			in->unison_begun = true;
			in->route_pending = true;
			ua_unison_set_owned_locked(ua, in->synch, true);
		}
		mutex_unlock(&ua->lock);
		if (!ret)
			ret = ua_inst_analog_bootstrap(ua, in, &seq);
		mutex_lock(&ua->lock);
	} else {
		if (!in->analog_owned || !in->unison_begun) {
			ret = -EPERM;
			goto out;
		}
		list_for_each_entry (other, &h->instances, node)
			if (other != in && other->placed && !other->lost &&
			    (other->started || other->route_pending) &&
			    ua_inst_inputs_overlap(in->synch, other->synch)) {
				ret = -EBUSY;
				goto out;
			}
		mutex_unlock(&ua->lock);
		ret = ua_inst_start(ua, in, NULL, 0, in->synch, true);
		if (!ret)
			ret = ua_unison_drain(ua, in->epoch, &seq);
		mutex_lock(&ua->lock);
		if (!ret && (in->lost || h->epoch != req->epoch))
			ret = -ESTALE;
		if (ret)
			ua_inst_analog_failed(ua, in, ret);
		else
			in->unison_begun = false;
	}
out:
	if (!ret) {
		req->reserved[4] = seq;
		req->reserved[5] = 1;
	}
	mutex_unlock(&ua->lock);
	mutex_unlock(&h->mutex);
	return ret;
}

static long ua_plugin_unison_params(struct ua_device *ua, struct ua_file *uf,
				    struct ua_plugin_instance *req)
{
	struct ua_plugin_host *h = ua->host;
	struct ua_plugin_inst *in;
	u32 *words = NULL, nw = req->reserved[1], seq = 0;
	u64 ptr = (u64)req->reserved[3] << 32 | req->reserved[2];
	int ret;
	if (!req->handle || !req->epoch || req->flags || req->render_id || req->dsp ||
	    req->n_resources || req->n_private || req->resources || req->privates ||
	    req->reserved[0] || req->reserved[4] || req->reserved[5] || nw % 3 ||
	    nw > UA_PLUGIN_UNISON_WORDS_MAX || (!nw && ptr) || (nw && !ptr))
		return -EINVAL;
	if (nw) {
		words = kvmalloc_array(nw, sizeof(*words), GFP_KERNEL);
		if (!words)
			return -ENOMEM;
		if (copy_from_user(words, u64_to_user_ptr(ptr), nw * sizeof(*words))) {
			kvfree(words);
			return -EFAULT;
		}
	}
	if (mutex_lock_killable(&h->mutex)) {
		kvfree(words);
		return -EINTR;
	}
	mutex_lock(&ua->lock);
	in = ua_inst_find(h, uf, req->handle);
	ret = !in ? -ENOENT : 0;
	if (!ret && (in->lost || in->epoch != req->epoch || h->epoch != req->epoch ||
		     !ua_plugin_dsp_admitted(ua, in->dsp)))
		ret = -ESTALE;
	if (!ret && (!in->unison || (!in->started && !in->unison_begun) || !in->loaded ||
		     !in->analog_owned))
		ret = -EPERM;
	if (!ret && nw)
		ret = ua_unison_patch_locked(ua, &in->analog_current, words, nw);
	mutex_unlock(&ua->lock);
	if (!ret) {
		ret = ua_unison_drain(ua, req->epoch, &seq);
		if (ret) {
			mutex_lock(&ua->lock);
			ua_inst_analog_failed(ua, in, ret);
			mutex_unlock(&ua->lock);
		}
	}
	if (!ret) {
		req->reserved[4] = seq;
		req->reserved[5] = 1;
	}
	mutex_unlock(&h->mutex);
	kvfree(words);
	return ret;
}

long ua_plugin_ioctl_delay(struct ua_device *ua, unsigned long arg)
{
	struct ua_plugin_delay q;
	u32 values[3][4];
	int ret;

	if (!ua_plugin_host_enabled())
		return -EOPNOTSUPP;
	if (copy_from_user(&q, (void __user *)arg, sizeof(q)))
		return -EFAULT;
	if (q.struct_size != sizeof(q) || q.flags & ~7u ||
	    (q.flags && (!q.epoch || q.input_mask != 0xf)) ||
	    (!q.flags && (q.epoch || q.generation || q.input_mask ||
			 memchr_inv(q.record, 0, sizeof(q.record)) ||
			 memchr_inv(q.monitor, 0, sizeof(q.monitor)) ||
			 memchr_inv(q.capacity, 0, sizeof(q.capacity)))))
		return -EINVAL;
	memcpy(values[0], q.record, sizeof(q.record));
	memcpy(values[1], q.monitor, sizeof(q.monitor));
	memcpy(values[2], q.capacity, sizeof(q.capacity));
	ret = ua_strips_delay(ua, &q.epoch, &q.generation, q.flags, values);
	if (ret)
		return ret;
	memcpy(q.record, values[0], sizeof(q.record));
	memcpy(q.monitor, values[1], sizeof(q.monitor));
	memcpy(q.capacity, values[2], sizeof(q.capacity));
	q.input_mask = 0xf;
	return copy_to_user((void __user *)arg, &q, sizeof(q)) ? -EFAULT : 0;
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
	unsigned int i, nw = 0, nids = 0;
	u32 ids[5], carrier = 0x846;	/* the strip a START/STAGE replaces */
	long ret;

	if (!ua_plugin_host_enabled())
		return -EOPNOTSUPP;
	if (copy_from_user(&req, (void __user *)arg, sizeof(req)))
		return -EFAULT;
	if (req.struct_size != sizeof(req) || req.license_state || req.license_word)
		return -EINVAL;
	if (req.op == UA_PLUGIN_INST_BEGIN_UNISON || req.op == UA_PLUGIN_INST_ACTIVATE_UNISON) {
		ret = ua_plugin_unison_lifecycle(ua, uf, &req);
		if (!ret && copy_to_user((void __user *)arg, &req, sizeof(req)))
			ret = -EFAULT;
		return ret;
	}
	if (req.op == UA_PLUGIN_INST_UNISON_PARAMS) {
		ret = ua_plugin_unison_params(ua, uf, &req);
		if (!ret && copy_to_user((void __user *)arg, &req, sizeof(req)))
			ret = -EFAULT;
		return ret;
	}
	if (req.op == UA_PLUGIN_INST_SERVICE)
		return ua_plugin_ioctl_service(ua, uf, &req);
	if (req.op == UA_PLUGIN_INST_SWAP)
		return ua_plugin_swap(ua, uf, &req);
	if (req.op == UA_PLUGIN_INST_START)
		return ua_plugin_start_staged(ua, uf, &req);
	if (req.op == UA_PLUGIN_INST_CREATE &&
	    (req.flags & (UA_PLUGIN_CREATE_F_START | UA_PLUGIN_CREATE_F_STAGE))) {
		nw = req.reserved[1];
		if ((req.flags & (UA_PLUGIN_CREATE_F_START | UA_PLUGIN_CREATE_F_STAGE)) ==
		    (UA_PLUGIN_CREATE_F_START | UA_PLUGIN_CREATE_F_STAGE) ||
		    req.dsp == UA_PLUGIN_DSP_ANY ||
		    (req.reserved[0] & 0xffff) >= UA_INST_SPAN_NONE ||
		    (req.reserved[0] >> 16) > UA_INST_SPAN_NONE ||
		    (req.reserved[0] & 0xffff) == (req.reserved[0] >> 16) ||
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

 if ((req.flags & (UA_PLUGIN_CREATE_F_UNISON | UA_PLUGIN_CREATE_F_START)) ==
     (UA_PLUGIN_CREATE_F_UNISON | UA_PLUGIN_CREATE_F_START))
  return -EOPNOTSUPP;
	if ((req.flags & UA_PLUGIN_CREATE_F_UNISON) &&
	    !(req.flags & UA_PLUGIN_CREATE_F_CHAIN))
		return -EINVAL;
	if ((req.flags & UA_PLUGIN_CREATE_F_CHAIN) &&
	    (!(req.flags & (UA_PLUGIN_CREATE_F_START | UA_PLUGIN_CREATE_F_STAGE)) ||
	     (req.flags & UA_PLUGIN_CREATE_F_CHECK)))
		return -EINVAL;
	if (req.handle ||
	    (req.flags & ~(UA_PLUGIN_CREATE_F_CHECK | UA_PLUGIN_CREATE_F_START |
		       UA_PLUGIN_CREATE_F_STAGE | UA_PLUGIN_CREATE_F_CHAIN |
		       UA_PLUGIN_CREATE_F_UNISON)) ||
	    ((req.flags & UA_PLUGIN_CREATE_F_CHECK) &&
	     (req.flags & UA_PLUGIN_CREATE_F_STAGE)) ||
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
		if (nw) {
			ret = ua_inst_chain_ids(rows, req.n_private, cmd, nw,
				!!(req.flags & UA_PLUGIN_CREATE_F_CHAIN),
				!!(req.flags & UA_PLUGIN_CREATE_F_UNISON), req.render_id, ids, &nids,
				&carrier);
			if (ret)
				goto out;
		}
	}

	ret = ua_plugin_create(ua, uf, &req, (const u8 (*)[32])sha, rows, cmd, ids, nids,
			       carrier);
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
