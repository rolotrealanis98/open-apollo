"""ua_resource_pool.c: the host ledger of one DSP's four memory pools.

The file is compiled whole (includes dropped) with address/undefined
sanitizers.  Ported from the plugin-native-query branch's test (fbcf7ee) and
extended with vectors from tools-wine dsp_allocation_plan.py's tests, the
rounding CResourcePool::Initialize does, the scratch setter and the idle
check.  One deliberate difference from the Python model: the last free
extent of a pool (the terminal one) is never taken whole, as the Windows
allocator (System b090/c860) has it; the model allowed an exact fit there.
"""
import khost
import csrc

SANITIZE = ('-fsanitize=address,undefined', '-fno-sanitize-recover=all')


def run(main):
    code = (khost.PREFIX + khost.header('ua_resource_pool.h') +
            khost.body('ua_resource_pool.c') + main)
    return csrc.build_and_run(code, extra=SANITIZE)


def test_ledger_split_share_merge_and_invalidate():
    run(r'''
int main(void)
{
	struct ua_pool_geometry g[4] = {
		{ 100, 100, 0 }, { 200, 101, 3 }, { 400, 100, 4 }, { 600, 100, 4 } };
	struct ua_pool_owner *o = NULL, *other = NULL;
	struct ua_pool_lease a, b, c, bad;
	struct ua_pool_usage usage[4], baseline[4], empty[4] = { 0 };
	int identity, id2;

	assert(ua_pool_owner_create(g, &o) == 0);
	/* odd capacity rounds down, odd scratch up: 204..300 */
	assert(o->pool[1]->base == 204 && o->pool[1]->words == 96);
	assert(ua_pool_read_usage(NULL, usage) == -EINVAL &&
	       !memcmp(usage, empty, sizeof(usage)));
	assert(ua_pool_read_usage(o, NULL) == -EINVAL);
	assert(ua_pool_owner_idle(o));
	assert(ua_pool_read_usage(o, baseline) == 0);
	assert(baseline[0].total_words == 100 && baseline[0].scratch_words == 0 &&
	       baseline[0].free_words == 100 && baseline[0].largest_request_words == 98);
	assert(baseline[1].base == 200 && baseline[1].total_words == 100 &&
	       baseline[1].scratch_words == 4 && baseline[1].free_words == 96 &&
	       baseline[1].largest_request_words == 94);

	/* an allocation failure changes nothing */
	k_allocs = 0; k_fail_at = 1;
	assert(ua_pool_reserve(o, 0, 4, &identity, true, &a) == -ENOMEM);
	assert(o->pool[0]->base == 100 && o->pool[0]->words == 100 && !o->holds);
	k_allocs = 0; k_fail_at = 2;
	assert(ua_pool_reserve(o, 0, 4, &identity, true, &a) == -ENOMEM);
	assert(o->pool[0]->base == 100 && o->pool[0]->words == 100 && !o->holds);
	k_fail_at = 0;

	/* code: low end, odd requests rounded up, shared once */
	assert(ua_pool_reserve(o, 0, 3, &identity, true, &a) == 0 &&
	       a.base == 100 && a.words == 4);
	assert(ua_pool_reserve(o, 0, 3, &identity, true, &b) == 0 &&
	       a.cookie != b.cookie && a.base == b.base);
	assert(!ua_pool_owner_idle(o));
	assert(ua_pool_read_usage(o, usage) == 0);
	assert(usage[0].used_words == 4 && usage[0].free_words == 96 &&
	       usage[0].unique_allocations == 1 && usage[0].holds == 2);
	/* the same identity with another size is a conflict */
	assert(ua_pool_reserve(o, 0, 4, &identity, true, &c) == -EEXIST);
	assert(ua_pool_release(o, &a) == 0);
	assert(ua_pool_release(o, &a) == -ESTALE);
	assert(o->pool[0]->refs == 1);
	assert(ua_pool_release(o, &b) == 0);
	assert(ua_pool_read_usage(o, usage) == 0 &&
	       !memcmp(usage, baseline, sizeof(usage)));
	assert(o->pool[0]->terminal && o->pool[0]->words == 100 && !o->pool[0]->next);
	assert(ua_pool_owner_idle(o));

	/* data: high end, terminal never taken whole */
	assert(ua_pool_reserve(o, 1, 96, &identity, false, &a) == -ENOSPC);
	assert(ua_pool_reserve(o, 1, 94, &identity, false, &a) == 0 && a.base == 206);
	assert(ua_pool_release(o, &a) == 0);
	assert(ua_pool_reserve(o, 1, 4, &identity, false, &a) == 0 && a.base == 296);
	assert(ua_pool_reserve(o, 1, 4, &identity, false, &b) == 0 &&
	       b.base == 292 && a.cookie != b.cookie);
	assert(ua_pool_read_usage(o, usage) == 0 && usage[1].unique_allocations == 2 &&
	       usage[1].used_words == 8 && usage[1].holds == 2);
	bad = a;
	bad.base++;
	assert(ua_pool_release(o, &bad) == -ESTALE);
	assert(ua_pool_release(o, &b) == 0);
	assert(ua_pool_release(o, &a) == 0);
	assert(o->pool[1]->base == 204 && o->pool[1]->words == 96 &&
	       o->pool[1]->terminal && !o->pool[1]->next);

	/* a free non-terminal extent is reused exactly; gaps are kept */
	assert(ua_pool_reserve(o, 2, 8, &identity, false, &a) == 0);
	assert(ua_pool_reserve(o, 2, 4, &identity, false, &b) == 0);
	assert(ua_pool_release(o, &a) == 0);
	assert(ua_pool_read_usage(o, usage) == 0 && usage[2].free_words == 92 &&
	       usage[2].largest_request_words == 82);
	assert(ua_pool_reserve(o, 2, 83, &identity, false, &c) == -ENOSPC);
	assert(ua_pool_reserve(o, 2, 8, &identity, false, &c) == 0 && c.base == a.base);
	assert(ua_pool_release(o, &b) == 0);
	assert(ua_pool_release(o, &c) == 0);
	assert(o->pool[2]->base == 404 && o->pool[2]->words == 96 && !o->pool[2]->next);

	/* a resource set is all or nothing, shared holds included */
	{
		struct ua_pool_request rq[4] = {
			{ 0, 3, &identity, true }, { 1, 4, &id2, false },
			{ 2, 8, &id2, false }, { 3, 6, &id2, false } };
		struct ua_pool_lease l[4], zero[4] = { 0 };
		struct ua_pool_usage before[4];
		int live;

		assert(ua_pool_reserve(o, 0, 3, &identity, true, &a) == 0);
		assert(ua_pool_read_usage(o, before) == 0);
		live = k_objects;
		for (int n = 1; n <= 7; n++) {
			k_allocs = 0; k_fail_at = n;
			memset(l, 0xff, sizeof(l));
			assert(ua_pool_reserve_batch(o, rq, 4, l) == -ENOMEM);
			assert(!memcmp(l, zero, sizeof(l)) && k_objects == live);
			assert(ua_pool_read_usage(o, usage) == 0 &&
			       !memcmp(usage, before, sizeof(usage)));
		}
		k_fail_at = 0;
		rq[3].kind = 4;
		assert(ua_pool_reserve_batch(o, rq, 4, l) == -EINVAL);
		assert(!memcmp(l, zero, sizeof(l)));
		rq[3].kind = 3;
		rq[3].words = 96;
		assert(ua_pool_reserve_batch(o, rq, 4, l) == -ENOSPC);
		assert(ua_pool_read_usage(o, usage) == 0 &&
		       !memcmp(usage, before, sizeof(usage)));
		rq[3].words = 6;
		assert(ua_pool_reserve_batch(o, rq, 4, l) == 0);
		assert(ua_pool_read_usage(o, usage) == 0 && usage[0].used_words == 4 &&
		       usage[0].holds == 2 && usage[1].used_words == 4 &&
		       usage[2].used_words == 8 && usage[3].used_words == 6);
		for (int n = 3; n >= 0; n--)
			assert(ua_pool_release(o, &l[n]) == 0);
		assert(ua_pool_release(o, &a) == 0);
		assert(ua_pool_read_usage(o, usage) == 0 &&
		       !memcmp(usage, baseline, sizeof(usage)));
		assert(ua_pool_reserve_batch(o, rq, 0, l) == -EINVAL);
		assert(ua_pool_reserve_batch(o, rq, UA_POOL_BATCH_MAX + 1, l) == -EINVAL);
	}

	/* tickets never wrap: exhaustion changes nothing */
	ua_pool_hold_ids.value = S64_MAX;
	assert(ua_pool_reserve(o, 0, 2, &identity, false, &c) == -EOVERFLOW);
	ua_pool_hold_ids.value = 100;

	/* another owner's lease is not this owner's */
	assert(ua_pool_owner_create(g, &other) == 0);
	assert(ua_pool_reserve(other, 1, 4, &identity, false, &b) == 0);
	assert(ua_pool_release(o, &b) == -EINVAL);
	bad = b;
	bad.cookie = 99999;
	assert(ua_pool_release(other, &bad) == -ESTALE);
	assert(ua_pool_release(other, &b) == 0);

	/* invalidated: every operation is stale, output zeroed */
	ua_pool_owner_invalidate(o);
	memset(usage, 0xff, sizeof(usage));
	assert(ua_pool_read_usage(o, usage) == -ESTALE &&
	       !memcmp(usage, empty, sizeof(usage)));
	assert(ua_pool_reserve(o, 0, 4, &identity, false, &a) == -ESTALE);
	assert(ua_pool_set_scratch(o, 1, 8) == -ESTALE);
	ua_pool_owner_destroy(o);
	ua_pool_owner_destroy(other);
	assert(k_objects == 0);

	/* geometry: all-ones, empty, scratch filling the pool, code scratch */
	g[1].words = 0xffffffff;
	assert(ua_pool_owner_create(g, &o) == -EINVAL && !o && !k_objects);
	g[1].words = 101;
	g[1].scratch = 99;
	assert(ua_pool_owner_create(g, &o) == -EINVAL && !k_objects);
	g[1].scratch = 3;
	g[0].scratch = 2;
	assert(ua_pool_owner_create(g, &o) == -EINVAL && !k_objects);
	g[0].scratch = 0;
	g[3].base = 0xfffffff0;
	assert(ua_pool_owner_create(g, &o) == -EINVAL && !k_objects);
	g[3].base = 600;
	for (int n = 1; n <= 5; n++) {
		k_allocs = 0; k_fail_at = n;
		assert(ua_pool_owner_create(g, &o) == -ENOMEM && !k_objects);
	}
	k_fail_at = 0;
	/* an odd base rounds up (CResourcePool::Initialize), it is not refused */
	g[1].base = 201;
	assert(ua_pool_owner_create(g, &o) == 0);
	assert(o->base[1] == 202 && o->pool[1]->base == 206);
	ua_pool_owner_destroy(o);
	assert(!k_objects);
	return 0;
}
''')


def test_scratch_grows_and_shrinks_at_the_bottom():
    """A data pool's scratch area moves the terminal extent's bottom only:
    allocations at the top stay where they are, it never shrinks below the
    register's value, and it cannot eat the last free words."""
    run(r'''
int main(void)
{
	struct ua_pool_geometry g[4] = {
		{ 0, 64, 0 }, { 1000, 100, 2 }, { 2000, 100, 0 }, { 3000, 100, 0 } };
	struct ua_pool_owner *o;
	struct ua_pool_lease a;
	struct ua_pool_usage u[4];
	int id;

	assert(ua_pool_owner_create(g, &o) == 0);
	assert(ua_pool_reserve(o, 1, 10, &id, false, &a) == 0 && a.base == 1090);
	assert(ua_pool_set_scratch(o, 1, 0x1f) == 0);	/* rounds to 32 */
	assert(ua_pool_read_usage(o, u) == 0 && u[1].scratch_words == 32 &&
	       u[1].free_words == 58 && u[1].used_words == 10);
	assert(o->pool[1]->next->base == 1032);
	assert(ua_pool_set_scratch(o, 1, 90) == -ENOSPC);	/* 58 free: 2 must stay */
	assert(ua_pool_set_scratch(o, 1, 88) == 0);
	assert(ua_pool_reserve(o, 1, 2, &id, false, &a) == -ENOSPC);
	assert(ua_pool_set_scratch(o, 1, 0) == 0);	/* back to the register's 2 */
	assert(ua_pool_read_usage(o, u) == 0 && u[1].scratch_words == 2 &&
	       u[1].free_words == 88);
	assert(ua_pool_set_scratch(o, 0, 4) == -EINVAL);
	assert(ua_pool_set_scratch(o, 4, 4) == -EINVAL);
	ua_pool_owner_destroy(o);
	assert(!k_objects);
	return 0;
}
''')


def test_offline_model_vectors():
    """tools-wine test_dsp_allocation_plan.py: pools at 100 * i, 20 words,
    scratch 2 in the data pools.  Private rows go top-down (116, 112) and
    give back (102, 18); code is first fit from the bottom (0, 4, 8, 12);
    shared code holds count users and returns with the last one."""
    run(r'''
int main(void)
{
	struct ua_pool_geometry g[4] = {
		{ 0, 20, 0 }, { 100, 20, 2 }, { 200, 20, 2 }, { 300, 20, 2 } };
	struct ua_pool_owner *o;
	struct ua_pool_lease p0, p1, code[5], s1, s2;
	int ids[6];

	assert(ua_pool_owner_create(g, &o) == 0);
	assert(ua_pool_reserve(o, 1, 3, &ids[0], false, &p0) == 0 &&
	       p0.base == 116 && p0.words == 4);
	assert(ua_pool_reserve(o, 1, 4, &ids[1], false, &p1) == 0 && p1.base == 112);
	assert(ua_pool_release(o, &p0) == 0);
	assert(ua_pool_release(o, &p1) == 0);
	assert(o->pool[1]->base == 102 && o->pool[1]->words == 18 && !o->pool[1]->next);

	for (int i = 0; i < 4; i++)
		assert(ua_pool_reserve(o, 0, 4, &ids[i], true, &code[i]) == 0 &&
		       code[i].base == (u32)(4 * i));
	/* the model fit a fifth at 16; the terminal guard keeps the last words */
	assert(ua_pool_reserve(o, 0, 4, &ids[4], true, &code[4]) == -ENOSPC);
	assert(ua_pool_reserve(o, 0, 2, &ids[4], true, &code[4]) == 0 &&
	       code[4].base == 16);
	assert(ua_pool_reserve(o, 0, 4, &ids[0], true, &s1) == 0 &&
	       s1.base == 0 && o->pool[0]->refs == 2);
	assert(ua_pool_release(o, &code[0]) == 0 && o->pool[0]->refs == 1);
	assert(ua_pool_release(o, &s1) == 0 && !o->pool[0]->cookie);
	/* a freed code extent in the middle is reused before the top */
	assert(ua_pool_release(o, &code[2]) == 0);
	assert(ua_pool_reserve(o, 0, 4, &ids[5], true, &s2) == 0 && s2.base == 0);
	assert(ua_pool_reserve(o, 0, 4, &ids[2], true, &code[2]) == 0 &&
	       code[2].base == 8);
	for (int i = 1; i < 5; i++)
		assert(ua_pool_release(o, &code[i]) == 0);
	assert(ua_pool_release(o, &s2) == 0);
	assert(ua_pool_owner_idle(o) && o->pool[0]->base == 0 &&
	       o->pool[0]->words == 20 && !o->pool[0]->next);
	ua_pool_owner_destroy(o);
	assert(!k_objects);
	return 0;
}
''')
