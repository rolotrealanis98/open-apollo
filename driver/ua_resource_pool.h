/* SPDX-License-Identifier: GPL-2.0-only */
/*
 * Universal Audio Apollo — host ledger of one DSP's four memory pools
 *
 * Copyright (c) 2026 apollo-linux contributors
 */
#ifndef UA_RESOURCE_POOL_H
#define UA_RESOURCE_POOL_H

#include <linux/types.h>

#define UA_POOLS		4	/* 0 code, 1-3 data */
#define UA_POOL_BATCH_MAX	192	/* CREATE: 64 resources + 128 private rows */

/*
 * One pool as the DSP's register window gives it (CPcieDSP::GetProperty(6):
 * capacity +0x10 + 4n, base +0x184 + 4n, scratch +0x190 + 4n for n >= 1).
 * Raw values; ua_pool_owner_create() rounds them as CResourcePool::
 * Initialize does (base and scratch up to even, capacity down to even).
 */
struct ua_pool_geometry {
	u32 base, words, scratch;
};

struct ua_pool_owner;

/* What a reserve gave: [base, base + words) in pool @kind */
struct ua_pool_lease {
	const struct ua_pool_owner *owner;
	u64 cookie;		/* 0 = no lease */
	u32 kind, base, words;
};

struct ua_pool_usage {
	u32 base;		/* rounded base */
	u32 total_words;	/* rounded capacity, scratch included */
	u32 scratch_words;	/* current scratch (bottom of pools 1-3) */
	u32 used_words, free_words;
	u32 largest_request_words;	/* largest even request that fits now */
	u32 unique_allocations;
	u64 holds;
};

struct ua_pool_request {
	u32 kind, words;
	const void *identity;	/* the resource object; shared extents match on it */
	bool shared;
};

int ua_pool_owner_create(const struct ua_pool_geometry geometry[UA_POOLS],
			 struct ua_pool_owner **out);
void ua_pool_owner_invalidate(struct ua_pool_owner *owner);
void ua_pool_owner_destroy(struct ua_pool_owner *owner);
int ua_pool_reserve(struct ua_pool_owner *owner, unsigned int kind, u32 words,
		    const void *identity, bool shared,
		    struct ua_pool_lease *out);
int ua_pool_reserve_batch(struct ua_pool_owner *owner,
			  const struct ua_pool_request *requests,
			  unsigned int count, struct ua_pool_lease *out);
int ua_pool_release(struct ua_pool_owner *owner,
		    const struct ua_pool_lease *lease);
int ua_pool_release_last(struct ua_pool_owner *owner,
			 const struct ua_pool_lease *lease, bool *last);
int ua_pool_set_scratch(struct ua_pool_owner *owner, unsigned int kind,
			u32 words);
int ua_pool_read_usage(struct ua_pool_owner *owner,
		       struct ua_pool_usage out[UA_POOLS]);
bool ua_pool_owner_idle(struct ua_pool_owner *owner);

#endif /* UA_RESOURCE_POOL_H */
