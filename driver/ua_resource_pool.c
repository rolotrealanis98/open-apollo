// SPDX-License-Identifier: GPL-2.0-only
/*
 * Universal Audio Apollo — host ledger of one DSP's four memory pools
 *
 * Copyright (c) 2026 apollo-linux contributors
 *
 * The DSP keeps no allocator of its own: the host decides where every code
 * and data resource goes, and only the host's ledger knows what is free
 * (CResourcePool keeps it host-side; nothing in the DSP's registers changes
 * when memory is handed out).  This is that ledger for one DSP, the rules as
 * UAD2System applies them (Windows 12 System b090/c860, Mac kext
 * CResourcePool::Initialize 0x33058):
 *   - the geometry is rounded: base and scratch up to even, capacity down;
 *   - pool 0 (code) is filled from its low end, pools 1-3 (data) from their
 *     high end, first fit, every request rounded up to even;
 *   - pools 1-3 keep a scratch area at their bottom, never handed out;
 *   - an extent that fits exactly is taken whole, except the last free
 *     extent (the "terminal" one at the pool's far end), which must stay
 *     non-empty;
 *   - a shared resource (a code module several instances use) takes its
 *     extent once and a hold per user; its words go back with the last hold;
 *   - a freed extent merges with free neighbours.
 * Nothing here talks to the device.  Callers hold ua->lock; the owner's own
 * mutex nests inside it.  Ported from the plugin-native-query branch
 * (fbcf7ee, ua_resource_pool.c), which rejected odd geometry instead of
 * rounding it and had no scratch setter.
 */

#include <linux/atomic.h>
#include <linux/errno.h>
#include <linux/kernel.h>
#include <linux/mutex.h>
#include <linux/slab.h>
#include <linux/string.h>

#include "ua_resource_pool.h"

/* UA_POOL_LEDGER_BEGIN: the host tests build everything up to the end mark */

struct ua_pool_extent {
	struct ua_pool_extent *next;	/* code: ascending, data: descending */
	u32 base, words, requested, refs;
	u64 cookie;			/* 0 = free */
	const void *identity;
	bool shared, terminal;
};

struct ua_pool_hold {
	struct ua_pool_hold *next;
	struct ua_pool_extent *extent;
	u64 cookie;
};

/* Tickets are never reused: a stale lease can never match a new hold */
static atomic64_t ua_pool_hold_ids = ATOMIC64_INIT(0);

struct ua_pool_owner {
	struct mutex lock;
	struct ua_pool_extent *pool[UA_POOLS];
	struct ua_pool_hold *holds;
	u32 base[UA_POOLS];		/* rounded */
	u32 words[UA_POOLS];		/* rounded, scratch included */
	u32 scratch_min[UA_POOLS];	/* the register's, rounded */
	u32 scratch[UA_POOLS];		/* current */
	bool invalidated;
};

static u32 ua_pool_even_up(u32 v)
{
	return v + (v & 1);
}

int ua_pool_owner_create(const struct ua_pool_geometry g[UA_POOLS],
			 struct ua_pool_owner **out)
{
	struct ua_pool_owner *o;
	unsigned int i;
	int ret = -EINVAL;

	if (!out)
		return -EINVAL;
	*out = NULL;
	if (!g)
		return -EINVAL;
	o = kzalloc(sizeof(*o), GFP_KERNEL);
	if (!o)
		return -ENOMEM;
	mutex_init(&o->lock);

	for (i = 0; i < UA_POOLS; i++) {
		struct ua_pool_extent *e;
		u32 base, size, scratch;

		if (g[i].base == U32_MAX || g[i].words == U32_MAX ||
		    g[i].scratch == U32_MAX || (!i && g[i].scratch))
			goto fail;
		base = ua_pool_even_up(g[i].base);
		size = g[i].words & ~1u;
		scratch = ua_pool_even_up(g[i].scratch);
		if (base < g[i].base || !size || scratch + 2 > size ||
		    (u64)base + size > (1ULL << 32))
			goto fail;
		e = kzalloc(sizeof(*e), GFP_KERNEL);
		if (!e) {
			ret = -ENOMEM;
			goto fail;
		}
		e->base = base + scratch;
		e->words = size - scratch;
		e->terminal = true;
		o->pool[i] = e;
		o->base[i] = base;
		o->words[i] = size;
		o->scratch[i] = o->scratch_min[i] = scratch;
	}
	*out = o;
	return 0;
fail:
	for (i = 0; i < UA_POOLS; i++)
		kfree(o->pool[i]);
	kfree(o);
	return ret;
}

/* Every later operation fails with -ESTALE; destroy still frees it */
void ua_pool_owner_invalidate(struct ua_pool_owner *o)
{
	if (!o)
		return;
	mutex_lock(&o->lock);
	o->invalidated = true;
	mutex_unlock(&o->lock);
}

/* Host bookkeeping only: nothing on the DSP is freed by this */
void ua_pool_owner_destroy(struct ua_pool_owner *o)
{
	struct ua_pool_hold *h;
	unsigned int i;

	if (!o)
		return;
	while ((h = o->holds)) {
		o->holds = h->next;
		kfree(h);
	}
	for (i = 0; i < UA_POOLS; i++) {
		struct ua_pool_extent *e = o->pool[i];

		while (e) {
			struct ua_pool_extent *next = e->next;

			kfree(e);
			e = next;
		}
	}
	kfree(o);
}

int ua_pool_read_usage(struct ua_pool_owner *o, struct ua_pool_usage out[UA_POOLS])
{
	unsigned int i;
	int ret = 0;

	if (!out)
		return -EINVAL;
	memset(out, 0, sizeof(*out) * UA_POOLS);
	if (!o)
		return -EINVAL;
	mutex_lock(&o->lock);
	if (o->invalidated) {
		ret = -ESTALE;
		goto done;
	}
	for (i = 0; i < UA_POOLS; i++) {
		struct ua_pool_usage *u = &out[i];
		struct ua_pool_extent *e;

		u->base = o->base[i];
		u->total_words = o->words[i];
		u->scratch_words = o->scratch[i];
		for (e = o->pool[i]; e; e = e->next) {
			if (e->cookie) {
				u->used_words += e->words;
				u->unique_allocations++;
				u->holds += e->refs;
			} else {
				u32 fits = e->words & ~1u;

				u->free_words += e->words;
				/* the terminal extent must keep 2 words */
				if (e->terminal)
					fits = fits >= 2 ? fits - 2 : 0;
				if (fits > u->largest_request_words)
					u->largest_request_words = fits;
			}
		}
	}
done:
	mutex_unlock(&o->lock);
	return ret;
}

/* No hold left: every pool is one free extent above its scratch */
bool ua_pool_owner_idle(struct ua_pool_owner *o)
{
	bool idle;

	if (!o)
		return true;
	mutex_lock(&o->lock);
	idle = !o->holds;
	mutex_unlock(&o->lock);
	return idle;
}

static int ua_pool_reserve_locked(struct ua_pool_owner *o, unsigned int kind,
				  u32 words, const void *identity, bool shared,
				  struct ua_pool_lease *out)
{
	struct ua_pool_extent **link, *e, *n;
	struct ua_pool_hold *h = NULL;
	u32 requested = words;
	s64 id;
	int ret;

	memset(out, 0, sizeof(*out));
	if (kind >= UA_POOLS || !words || words >= U32_MAX - 1 || !identity)
		return -EINVAL;
	if (o->invalidated)
		return -ESTALE;
	h = kzalloc(sizeof(*h), GFP_KERNEL);
	if (!h)
		return -ENOMEM;
	for (;;) {
		id = atomic64_read(&ua_pool_hold_ids);
		if (id == S64_MAX) {
			ret = -EOVERFLOW;
			goto done;
		}
		if (atomic64_cmpxchg(&ua_pool_hold_ids, id, id + 1) == id)
			break;
	}
	h->cookie = (u64)id + 1;

	if (shared) {
		for (e = o->pool[kind]; e; e = e->next) {
			if (!e->cookie || e->identity != identity)
				continue;
			if (!e->shared || e->requested != requested) {
				ret = -EEXIST;
				goto done;
			}
			if (e->refs == U32_MAX) {
				ret = -EOVERFLOW;
				goto done;
			}
			e->refs++;
			goto success;
		}
	}

	words += words & 1;
	for (link = &o->pool[kind]; (e = *link); link = &e->next) {
		if (e->cookie || e->words < words ||
		    (e->terminal && e->words <= words))
			continue;
		if (e->words == words) {
			n = e;
		} else {
			n = kzalloc(sizeof(*n), GFP_KERNEL);
			if (!n) {
				ret = -ENOMEM;
				goto done;
			}
			n->words = words;
			if (!kind) {
				n->base = e->base;
				e->base += words;
			} else {
				n->base = e->base + e->words - words;
			}
			e->words -= words;
			n->next = e;
			*link = n;
		}
		n->cookie = h->cookie;
		n->identity = identity;
		n->requested = requested;
		n->refs = 1;
		n->shared = shared;
		e = n;
		goto success;
	}
	ret = -ENOSPC;
	goto done;
success:
	h->extent = e;
	h->next = o->holds;
	o->holds = h;
	*out = (struct ua_pool_lease){ o, h->cookie, kind, e->base, e->words };
	h = NULL;
	ret = 0;
done:
	kfree(h);
	return ret;
}

int ua_pool_reserve(struct ua_pool_owner *o, unsigned int kind, u32 words,
		    const void *identity, bool shared, struct ua_pool_lease *out)
{
	int ret;

	if (!out)
		return -EINVAL;
	memset(out, 0, sizeof(*out));
	if (!o)
		return -EINVAL;
	mutex_lock(&o->lock);
	ret = ua_pool_reserve_locked(o, kind, words, identity, shared, out);
	mutex_unlock(&o->lock);
	return ret;
}

static int ua_pool_release_locked(struct ua_pool_owner *o,
				  const struct ua_pool_lease *lease)
{
	struct ua_pool_extent *e, *p, *next, **pp;
	struct ua_pool_hold **hp, *h;

	if (!lease || lease->kind >= UA_POOLS || !lease->cookie ||
	    lease->owner != o)
		return -EINVAL;
	if (o->invalidated)
		return -ESTALE;
	for (hp = &o->holds; (h = *hp); hp = &h->next)
		if (h->cookie == lease->cookie)
			break;
	if (!h)
		return -ESTALE;
	p = NULL;
	for (e = o->pool[lease->kind]; e; p = e, e = e->next) {
		if (e != h->extent)
			continue;
		if (e->base != lease->base || e->words != lease->words)
			return -ESTALE;
		*hp = h->next;
		kfree(h);
		if (--e->refs)
			return 0;
		e->cookie = 0;
		e->identity = NULL;
		e->requested = 0;
		e->shared = false;
		/* List neighbours are memory neighbours: merge free ones */
		next = e->next;
		if (next && !next->cookie) {
			next->base = min(e->base, next->base);
			next->words += e->words;
			if (p)
				p->next = next;
			else
				o->pool[lease->kind] = next;
			kfree(e);
			e = next;
		}
		if (p && !p->cookie) {
			/* p is never the terminal extent: that one is last */
			e->base = min(p->base, e->base);
			e->words += p->words;
			pp = &o->pool[lease->kind];
			while (*pp != p)
				pp = &(*pp)->next;
			*pp = e;
			kfree(p);
		}
		return 0;
	}
	return -ESTALE;
}

int ua_pool_release(struct ua_pool_owner *o, const struct ua_pool_lease *lease)
{
	int ret;

	if (!o)
		return -EINVAL;
	mutex_lock(&o->lock);
	ret = ua_pool_release_locked(o, lease);
	mutex_unlock(&o->lock);
	return ret;
}

/*
 * Reserve every request or none (one owner lock): a failure releases the
 * holds this call took, in reverse order; references an earlier caller
 * holds on shared extents are untouched.
 */
int ua_pool_reserve_batch(struct ua_pool_owner *o,
			  const struct ua_pool_request *requests,
			  unsigned int count, struct ua_pool_lease *out)
{
	unsigned int i;
	int ret = 0;

	if (!out || !count || count > UA_POOL_BATCH_MAX)
		return -EINVAL;
	memset(out, 0, sizeof(*out) * count);
	if (!o || !requests)
		return -EINVAL;
	mutex_lock(&o->lock);
	for (i = 0; i < count; i++) {
		const struct ua_pool_request *r = &requests[i];

		ret = ua_pool_reserve_locked(o, r->kind, r->words, r->identity,
					     r->shared, &out[i]);
		if (ret)
			break;
	}
	if (ret) {
		while (i)
			WARN_ON_ONCE(ua_pool_release_locked(o, &out[--i]));
		memset(out, 0, sizeof(*out) * count);
	}
	mutex_unlock(&o->lock);
	return ret;
}

/*
 * The scratch area at the bottom of data pool @kind: @words (rounded up to
 * even), never below the register's.  It grows into the terminal extent
 * (which stays non-empty) and shrinks back into it; nothing allocated ever
 * moves.  -EINVAL for the code pool, -ENOSPC when it does not fit.
 */
int ua_pool_set_scratch(struct ua_pool_owner *o, unsigned int kind, u32 words)
{
	struct ua_pool_extent *t;
	u32 want;
	int ret = 0;

	if (!o || !kind || kind >= UA_POOLS || words >= U32_MAX - 1)
		return -EINVAL;
	mutex_lock(&o->lock);
	if (o->invalidated) {
		ret = -ESTALE;
		goto done;
	}
	want = ua_pool_even_up(words);
	if (want < o->scratch_min[kind])
		want = o->scratch_min[kind];
	for (t = o->pool[kind]; t->next; t = t->next)
		;
	if (WARN_ON_ONCE(!t->terminal || t->cookie ||
			 t->base != o->base[kind] + o->scratch[kind])) {
		ret = -EIO;
		goto done;
	}
	if (want > o->scratch[kind]) {
		u32 grow = want - o->scratch[kind];

		if (t->words <= grow) {
			ret = -ENOSPC;
			goto done;
		}
		t->base += grow;
		t->words -= grow;
	} else {
		u32 shrink = o->scratch[kind] - want;

		t->base -= shrink;
		t->words += shrink;
	}
	o->scratch[kind] = want;
done:
	mutex_unlock(&o->lock);
	return ret;
}

/* UA_POOL_LEDGER_END */
