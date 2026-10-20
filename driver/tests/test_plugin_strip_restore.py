"""Retained strip restoration with synthetic allocations and the fake DSP ring."""
import test_plugin_license as license


HELPERS = r'''
static struct ua_strips *plain;
static void setup_plain(void)
{
    setup();
    bq_table[6] = 0x81000000;
    plain = kzalloc(sizeof(*plain), GFP_KERNEL);
    assert(plain);
    host.strips = plain;
    ua_strips_dsp = 1;
    input_strips_steps = 0;
    plain->state = UA_STRIPS_LIVE;
    plain->epoch = host.epoch;
    plain->gen = 9;
    plain->n = plain->n_mic = 2;
    plain->rebuild_seen = atomic_read(&ua_strips_rebuilds);
    for (unsigned int k = 0; k < 2; k++) {
        struct ua_strip *s = &plain->strip[k];
        s->synch = s->input = k;
        s->state = UA_STRIP_LIVE;
        for (unsigned int j = 0; j < UA_STRIP_PRIV; j++)
            assert(ua_pool_reserve(host.dsp[1].pools, 1, 8, &s->priv[j],
                                   false, &s->priv[j]) == 0);
        s->header = s->priv[0].base;
    }
}
static void cleanup_plain(void)
{
    ua_plugin_file_release(&file_a);
    ua_plugin_file_release(&file_b);
    kfree(plain);
    host.strips = NULL;
    cleanup();
    assert(!k_objects && !dma_live && !sleeps_locked);
}
static long start(struct ua_file *f, u32 synch, unsigned int dsp,
                  struct ua_plugin_instance *q)
{
    static u32 cmd[] = {0x00140004, 0x01000000, 0, 42};
    uintptr_t p = (uintptr_t)cmd;
    memset(q, 0, sizeof(*q));
    memcpy(res[0].sha256, sha_code, 32);
    memcpy(res[1].sha256, sha_data, 32);
    q->struct_size = sizeof(*q);
    q->op = UA_PLUGIN_INST_CREATE;
    q->flags = UA_PLUGIN_CREATE_F_START;
    q->render_id = 0x806;
    q->dsp = dsp;
    q->n_resources = q->n_private = 2;
    q->resources = (uintptr_t)res;
    q->privates = (uintptr_t)priv;
    q->reserved[0] = synch;
    q->reserved[1] = 4;
    q->reserved[2] = (u32)p;
    q->reserved[3] = (u64)p >> 32;
    return ua_plugin_ioctl_instance(&dev, f, (unsigned long)q);
}
static void check_restore(unsigned int c)
{
    unsigned int ring = 2; /* DSP 1 command ring */
    assert(slots[ring][c][0] == 0x001f0004);
    assert(slots[ring][c][1] == 0x00200000 && slots[ring][c][2] == 0);
    c = (c + 1) & 1023;
    assert(slots[ring][c][0] == 0x001f0004);
    assert(slots[ring][c][1] == 0x00200000);
    assert(slots[ring][c][2] == plain->strip[0].header);
    for (unsigned int i = 0; i < 3; i++) {
        c = (c + 1) & 1023;
        assert(slots[ring][c][0] == 0x001e0004);
        assert(slots[ring][c][1] == 0);
        assert(slots[ring][c][2] == (i < 2 ? 8u : 7u));
        assert(slots[ring][c][3] == (i < 2 ? 0x20u : ua_strip_route));
    }
}
'''


def run(main):
    license.build_license(HELPERS + main, real_strips=True)


def test_destroy_and_close_restore_only_the_retained_target():
    run(r'''
int main(void)
{
    struct ua_plugin_instance q;
    struct ua_strip before;
    unsigned int c;
    setup_plain();
    before = plain->strip[1];
    for (unsigned int i = 0; i < 2; i++) {
        assert(start(&file_a, 0x00200000, 1, &q) == 0);
        c = dev.dsp[1].cmd.write_ptr;
        if (i) ua_plugin_file_release(&file_a);
        else assert(destroy(&file_a, q.handle) == 0);
        check_restore(c);
        assert(!host.fence[1] && !host.dsp[1].quarantined);
        assert(!memcmp(&before, &plain->strip[1], sizeof(before)));
        assert(plain->gen == 9 && !plain->teardowns && !host.n_instances);
    }
    cleanup_plain();
    return 0;
}
''')


def test_shared_code_stop_only_fence_is_waited_and_timeout_propagates():
    run(r'''
int main(void)
{
    struct ua_plugin_instance idle, q;
    setup_plain();
    assert(create(&file_b, 0x806, 0, &idle) == 0);
    assert(start(&file_a, 0x00200000, 1, &q) == 0);
    stalled = true;
    assert(destroy(&file_a, q.handle) == -ETIMEDOUT);
    assert(host.dsp[1].quarantined && !host.dsp[1].releases);
    assert(!host.fence[1] && host.n_instances == 1);
    stalled = false;
    cleanup_plain();
    return 0;
}
''')


def test_start_admission_and_pending_rebuild_preserve_other_owners():
    run(r'''
int main(void)
{
    struct ua_plugin_instance q, other;
    unsigned int sent;
    setup_plain();
    sent = nputs;
    assert(start(&file_a, 0x00000000, 1, &q) == -EINVAL);
    assert(start(&file_a, 0x00200000, UA_PLUGIN_DSP_ANY, &q) == -EINVAL);
    assert(nputs == sent);
    assert(start(&file_a, 0x00200000, 2, &q) == -ESTALE);
    assert(start(&file_a, 0x00200005, 1, &q) == -ENOENT);
    assert(!uploads());
    assert(start(&file_a, 0x00200000, 1, &q) == 0);
    assert(start(&file_b, 0x00200000, 1, &other) == -EBUSY);
    assert(start(&file_b, 0x00200001, 1, &other) == 0);
    ua_strips_rebuild_set("1", NULL);
    mutex_lock(&dev.lock);
    ua_strips_service(&dev);
    mutex_unlock(&dev.lock);
    assert(plain->gen == 9 && plain->state == UA_STRIPS_LIVE);
    assert(plain->rebuild_seen != atomic_read(&ua_strips_rebuilds));
    assert(destroy(&file_a, q.handle) == 0);
    cleanup_plain();
    return 0;
}
''')


def test_stale_restore_and_publication_failure_never_report_success():
    run(r'''
int main(void)
{
    struct ua_plugin_instance q;
    unsigned int sent;
    setup_plain();
    assert(start(&file_a, 0x00200000, 1, &q) == 0);
    plain->gen++;
    sent = nputs;
    assert(destroy(&file_a, q.handle) == -ESTALE);
    assert(nputs == sent && host.dsp[1].quarantined);
    cleanup_plain();
    setup_plain();
    assert(start(&file_a, 0x00200000, 1, &q) == 0);
    fail_put_at = nputs;
    assert(destroy(&file_a, q.handle) < 0);
    assert(host.dsp[1].quarantined);
    cleanup_plain();
    return 0;
}
''')


def test_epoch_teardown_never_restarts_plain_strip():
    run(r'''
int main(void)
{
    struct ua_plugin_instance q;
    unsigned int c, count;
    setup_plain();
    assert(start(&file_a, 0x00200000, 1, &q) == 0);
    c = dev.dsp[1].cmd.write_ptr;
    mutex_lock(&dev.lock);
    ua_instances_teardown(&dev, "test epoch");
    mutex_unlock(&dev.lock);
    count = (dev.dsp[1].cmd.write_ptr - c) & 1023;
    assert(count == 3); /* one stop, two releases; no restart/span/route */
    assert(slots[2][c][2] == 0);
    assert(slots[2][(c + 1) & 1023][0] == 0x00030002);
    mutex_lock(&host.mutex);
    assert(ua_plugin_fence_wait(&dev, 1) == 0);
    mutex_unlock(&host.mutex);
    assert(destroy(&file_a, q.handle) == 0);
    cleanup_plain();
    return 0;
}
''')


def test_failed_start_mapping_restores_and_proves_cleanup_before_return():
    original = license.fake.FPGA
    license.fake.FPGA = ('static bool fail_start_map;\n' + original.replace(
        '\tp = aligned_alloc(4096, PAGE_ALIGN(n));',
        '\tif (fail_start_map && dma_allocs == 4) {\n'
        '\t\tfail_start_map = false; return NULL;\n\t}\n'
        '\tp = aligned_alloc(4096, PAGE_ALIGN(n));'))
    try:
        run(r'''
int main(void)
{
    struct ua_plugin_instance q;
    unsigned int c;
    setup_plain();
    fail_start_map = true;
    assert(start(&file_a, 0x00200000, 1, &q) == -ENOMEM);
    assert(!fail_start_map); /* the START DMA allocation was the failure */
    c = (dev.dsp[1].cmd.write_ptr - 7) & 1023;
    check_restore(c);
    assert(!host.n_instances && !host.fence[1] && !host.dsp[1].quarantined);
    assert(plain->gen == 9 && plain->state == UA_STRIPS_LIVE);
    cleanup_plain();
    return 0;
}
''')
    finally:
        license.fake.FPGA = original


def test_quarantine_and_lost_instances_never_restore():
    run(r'''
int main(void)
{
    struct ua_plugin_instance q;
    unsigned int sent;
    setup_plain();
    assert(start(&file_a, 0x00200000, 1, &q) == 0);
    host.dsp[1].quarantined = true;
    sent = nputs;
    assert(destroy(&file_a, q.handle) == -ESTALE);
    assert(nputs == sent);
    cleanup_plain();
    setup_plain();
    assert(start(&file_a, 0x00200000, 1, &q) == 0);
    mutex_lock(&dev.lock);
    ua_plugin_disown(&dev, 1, "test lost");
    mutex_unlock(&dev.lock);
    sent = nputs;
    assert(destroy(&file_a, q.handle) == 0);
    assert(nputs == sent);
    cleanup_plain();
    return 0;
}
''')


def test_rebuild_waits_for_retirement_fence_after_instance_is_removed():
    run(r'''
int main(void)
{
    struct ua_plugin_instance q;
    struct ua_plugin_inst *in;
    u32 header;
    setup_plain();
    assert(start(&file_a, 0x00200000, 1, &q) == 0);
    header = plain->strip[0].header;
    mutex_lock(&host.mutex);
    mutex_lock(&dev.lock);
    in = ua_inst_find(&host, &file_a, q.handle);
    assert(ua_inst_unplace(&dev, in, true, true) == 1);
    list_del(&in->node);
    host.n_instances--;
    ua_inst_free(&dev, in);
    assert(!ua_instances_strips_busy(&dev) && host.fence[1]);
    ua_strips_rebuild_set("1", NULL);
    ua_strips_service(&dev);
    assert(plain->gen == 9 && plain->strip[0].header == header);
    assert(plain->strip[0].priv[0].cookie);
    assert(plain->rebuild_seen != atomic_read(&ua_strips_rebuilds));
    mutex_unlock(&dev.lock);
    assert(ua_plugin_fence_wait(&dev, 1) == 0);
    mutex_unlock(&host.mutex);
    mutex_lock(&dev.lock);
    ua_strips_service(&dev);
    assert(plain->gen == 10 && !plain->strip[0].header);
    mutex_unlock(&dev.lock);
    cleanup_plain();
    return 0;
}
''')


def test_stereo_claims_both_inputs_and_restores_both_retained_strips():
    run(r'''
int main(void)
{
    struct ua_plugin_instance q, conflict;
    unsigned int c;
    setup_plain();
    assert(start(&file_a, 0x00010000, 1, &q) == 0);
    /* Two mono stops, initialization block, stereo start, routes for both. */
    c = (dev.dsp[1].cmd.write_ptr - 10) & 1023;
    assert(slots[2][c][0] == 0x001f0004 && slots[2][c][1] == 0x00200000);
    assert(slots[2][(c+1)&1023][0] == 0x001f0004);
    assert(slots[2][(c+1)&1023][1] == 0x00200001);
    assert(slots[2][(c+3)&1023][0] == 0x001f0004);
    assert(slots[2][(c+3)&1023][1] == 0x00010000);
    for (unsigned int ch=0; ch<2; ch++)
        for (unsigned int j=0; j<3; j++) {
            unsigned int pos = (c+4+ch*3+j)&1023;
            assert(slots[2][pos][0] == 0x001e0004);
            assert(slots[2][pos][1] == ch);
            assert(slots[2][pos][2] == (j<2 ? 8u : 7u));
        }
    assert(start(&file_b, 0x00200000, 1, &conflict) == -EBUSY);
    assert(start(&file_b, 0x00200001, 1, &conflict) == -EBUSY);
    assert(start(&file_b, 0x00000001, 1, &conflict) == -EBUSY);
    c = dev.dsp[1].cmd.write_ptr;
    assert(destroy(&file_a, q.handle) == 0);
    assert(slots[2][c][0] == 0x001f0004 && slots[2][c][1] == 0x00010000);
    for (unsigned int ch=0; ch<2; ch++) {
        unsigned int pos = (c+1+ch*4)&1023;
        assert(slots[2][pos][0] == 0x001f0004);
        assert(slots[2][pos][1] == (0x00200000u|ch));
        assert(slots[2][pos][2] == plain->strip[ch].header);
    }
    assert(!host.n_instances && !host.fence[1] && !host.dsp[1].quarantined);
    /* A mono owner also rejects an overlapping stereo create. */
    assert(start(&file_b, 0x00200001, 1, &q) == 0);
    assert(start(&file_a, 0x00010000, 1, &conflict) == -EBUSY);
    cleanup_plain();
    return 0;
}
''')


def test_scratch_claims_survive_arbitrary_unload_and_failed_growth():
    run(r'''
int main(void)
{
    struct ua_plugin_instance q[2];
    struct ua_pool_usage baseline[UA_POOLS], usage[UA_POOLS];
    struct ua_plugin_inst *in[2], failed;
    u32 need[UA_POOLS];
    for (unsigned int order=0; order<2; order++) {
        setup_plain();
        assert(ua_pool_read_usage(host.dsp[1].pools, baseline) == 0);
        assert(start(&file_a, 0x00200000, 1, &q[0]) == 0);
        assert(start(&file_b, 0x00200001, 1, &q[1]) == 0);
        in[0]=ua_inst_find(&host,&file_a,q[0].handle);
        in[1]=ua_inst_find(&host,&file_b,q[1].handle);
        mutex_lock(&dev.lock);
        memset(need,0,sizeof(need));
        need[1]=baseline[1].scratch_words+32;
        assert(ua_inst_scratch_raise(&host,in[0],need)==0);
        need[1]=baseline[1].scratch_words+16;
        need[2]=baseline[2].scratch_words+24;
        assert(ua_inst_scratch_raise(&host,in[1],need)==0);
        memset(&failed,0,sizeof(failed)); failed.dsp=1;
        need[1]=baseline[1].scratch_words+64;
        need[3]=0xffffffff;
        assert(ua_inst_scratch_raise(&host,&failed,need)==-EINVAL);
        assert(!failed.scratch_claim);
        mutex_unlock(&dev.lock);
        assert(ua_pool_read_usage(host.dsp[1].pools,usage)==0);
        assert(usage[1].scratch_words==baseline[1].scratch_words+32);
        assert(usage[2].scratch_words==baseline[2].scratch_words+24);
        assert(destroy(order?&file_b:&file_a,q[order].handle)==0);
        assert(ua_pool_read_usage(host.dsp[1].pools,usage)==0);
        assert(usage[1].scratch_words==baseline[1].scratch_words+(order?32:16));
        assert(usage[2].scratch_words==baseline[2].scratch_words+(order?0:24));
        assert(host.n_instances==1 && !host.dsp[1].quarantined);
        ua_plugin_file_release(order?&file_a:&file_b);
        assert(ua_pool_read_usage(host.dsp[1].pools,usage)==0);
        for (unsigned int p=1;p<UA_POOLS;p++)
            assert(usage[p].scratch_words==baseline[p].scratch_words);
        cleanup_plain();
    }
    return 0;
}
''')


def test_stereo_requires_both_retained_strips_before_any_upload():
    run(r'''
int main(void)
{
    struct ua_plugin_instance q;
    setup_plain();
    assert(start(&file_a, 0x00020000, 1, &q) == -ENOENT);
    plain->strip[1].header = 0;
    assert(start(&file_a, 0x00010000, 1, &q) == -ESTALE);
    assert(!uploads() && !host.n_instances);
    cleanup_plain();
    return 0;
}
''')


def test_disowned_dsp_drops_scratch_claims_with_its_pool_generation():
    run(r'''
int main(void)
{
    struct ua_plugin_instance q;
    struct ua_plugin_inst *in;
    u32 need[UA_POOLS] = {0, 64, 64, 0};
    setup_plain();
    assert(start(&file_a, 0x00200000, 1, &q) == 0);
    in=ua_inst_find(&host,&file_a,q.handle);
    mutex_lock(&dev.lock);
    assert(ua_inst_scratch_raise(&host,in,need)==0);
    assert(in->scratch_claim);
    ua_plugin_disown(&dev,1,"test pool generation");
    assert(!in->scratch_claim && !in->placed && in->lost);
    assert(!host.dsp[1].pools);
    mutex_unlock(&dev.lock);
    assert(destroy(&file_a,q.handle)==0);
    cleanup_plain();
    return 0;
}
''')
