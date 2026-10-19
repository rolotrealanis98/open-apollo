"""Staged initialization and reversible input handoff on a synthetic DSP ring."""
import test_plugin_license as license
import test_plugin_strip_restore as strips
import test_plugin_service as service

HELPERS = (r'''
static u32 create_flags = UA_PLUGIN_CREATE_F_START;
static u32 scratch3;
''' + strips.HELPERS.replace('q->flags = UA_PLUGIN_CREATE_F_START;',
                             'q->flags = create_flags; q->reserved[5] = scratch3;') + r'''
static long stage(struct ua_file *f, u32 pair, struct ua_plugin_instance *q)
{
    int ret;
    create_flags = UA_PLUGIN_CREATE_F_STAGE;
    ret = start(f, pair, 1, q);
    create_flags = UA_PLUGIN_CREATE_F_START;
    return ret;
}
static long swap(struct ua_file *f, const struct ua_plugin_instance *from,
                 const struct ua_plugin_instance *to)
{
    struct ua_plugin_instance q = {0};
    q.struct_size = sizeof(q);
    q.op = UA_PLUGIN_INST_SWAP;
    q.handle = from->handle;
    q.epoch = from->epoch;
    q.reserved[0] = to->handle;
    return ua_plugin_ioctl_instance(&dev, f, (unsigned long)&q);
}
static struct ua_plugin_inst *owned(struct ua_file *f, struct ua_plugin_instance *q)
{ return ua_inst_find(&host,f,q->handle); }
static void assert_no_routes(unsigned int c)
{
    while(c!=dev.dsp[1].cmd.write_ptr) {
        assert(slots[2][c][0]!=0x001f0004 && slots[2][c][0]!=0x001e0004);
        c=(c+1)&1023;
    }
}
''')


def run(main):
    old = license.fake.FPGA
    license.fake.FPGA = (
        'static u32 service_addr; static int service_mode;\n' + old.replace(
            '\t\t} else if (e[0] == 0x00100002) {',
            service.READ_FAKE + '\t\t} else if (e[0] == 0x00100002) {').replace(
                'if (nputs++ == fail_put_at)', 'if (nputs++ >= fail_put_at)'))
    try:
        license.build_license(HELPERS + main, real_strips=True)
    finally:
        license.fake.FPGA = old


def test_stage_forward_inverse_finalize_and_active_unload():
    run(r'''
int main(void)
{
    struct ua_plugin_instance old, candidate;
    unsigned int c;
    for(unsigned int stereo=0;stereo<2;stereo++) {
        u32 pair=stereo?0x00010000:0x00200000;
        setup_plain();
        assert(start(&file_a,pair,1,&old)==0);
        c=dev.dsp[1].cmd.write_ptr;
        assert(stage(&file_a,pair,&candidate)==0);
        assert_no_routes(c);
        assert(owned(&file_a,&old)->started && !owned(&file_a,&candidate)->started);
        c=dev.dsp[1].cmd.write_ptr;
        assert(swap(&file_a,&old,&candidate)==0);
        assert(!owned(&file_a,&old)->started && owned(&file_a,&candidate)->started);
        assert(((dev.dsp[1].cmd.write_ptr-c)&1023)==(stereo?8u:5u));
        assert(slots[2][c][0]==0x001f0004 && slots[2][c][1]==pair && !slots[2][c][2]);
        assert(slots[2][(c+1)&1023][0]==0x001f0004);
        assert(slots[2][(c+1)&1023][2]==owned(&file_a,&candidate)->lease[2].base);
        assert(swap(&file_a,&candidate,&old)==0);
        assert(owned(&file_a,&old)->started && !owned(&file_a,&candidate)->started);
        assert(swap(&file_a,&old,&candidate)==0);
        c=dev.dsp[1].cmd.write_ptr;
        assert(destroy(&file_a,old.handle)==0);
        assert_no_routes(c);
        assert(owned(&file_a,&candidate)->started && host.n_instances==1);
        c=dev.dsp[1].cmd.write_ptr;
        assert(destroy(&file_a,candidate.handle)==0);
        assert(slots[2][c][0]==0x001f0004 && slots[2][c][1]==pair);
        assert(slots[2][(c+1)&1023][2]==plain->strip[0].header);
        assert(!host.n_instances && !host.fence[1] && !host.dsp[1].quarantined);
        cleanup_plain();
    }
    return 0;
}
''')


def test_stage_failure_and_close_keep_old_route_and_scratch():
    run(r'''
int main(void)
{
    struct ua_plugin_instance old, candidate;
    struct ua_pool_usage before[UA_POOLS],after[UA_POOLS];
    unsigned int c;
    setup_plain();
    assert(start(&file_a,0x00200000,1,&old)==0);
    assert(ua_pool_read_usage(host.dsp[1].pools,before)==0);
    scratch3=0x7fffffff;
    c=dev.dsp[1].cmd.write_ptr;
    assert(stage(&file_b,0x00200000,&candidate)==-ENOSPC);
    scratch3=0;
    assert_no_routes(c);
    assert(owned(&file_a,&old)->started && host.n_instances==1);
    assert(ua_pool_read_usage(host.dsp[1].pools,after)==0);
    for(unsigned int p=0;p<UA_POOLS;p++) {
        assert(before[p].scratch_words==after[p].scratch_words);
        assert(before[p].used_words==after[p].used_words);
    }
    assert(stage(&file_b,0x00200000,&candidate)==0);
    c=dev.dsp[1].cmd.write_ptr;
    ua_plugin_file_release(&file_b);
    assert_no_routes(c);
    assert(owned(&file_a,&old)->started && host.n_instances==1);
    cleanup_plain();
    return 0;
}
''')


def test_swap_refuses_foreign_pair_epoch_generation_and_invalid_state():
    run(r'''
int main(void)
{
    struct ua_plugin_instance old,candidate,bad;
    unsigned int c;
    setup_plain();
    assert(start(&file_a,0x00200000,1,&old)==0);
    assert(stage(&file_b,0x00200000,&candidate)==0);
    c=dev.dsp[1].cmd.write_ptr;
    assert(swap(&file_a,&old,&candidate)==-ENOENT);
    assert(dev.dsp[1].cmd.write_ptr==c);
    assert(destroy(&file_b,candidate.handle)==0);
    assert(stage(&file_a,0x00200001,&candidate)==0);
    c=dev.dsp[1].cmd.write_ptr;
    assert(swap(&file_a,&old,&candidate)==-EINVAL);
    assert(dev.dsp[1].cmd.write_ptr==c);
    assert(destroy(&file_a,candidate.handle)==0);
    assert(stage(&file_a,0x00200000,&candidate)==0);
    c=dev.dsp[1].cmd.write_ptr;
    bad=old; bad.epoch++;
    assert(swap(&file_a,&bad,&candidate)==-ESTALE);
    assert(swap(&file_a,&candidate,&old)==-EINVAL);
    assert(swap(&file_a,&old,&old)==-EINVAL);
    owned(&file_a,&candidate)->strip_generation++;
    assert(swap(&file_a,&old,&candidate)==-ESTALE);
    assert(dev.dsp[1].cmd.write_ptr==c);
    owned(&file_a,&candidate)->strip_generation--;
    assert(swap(&file_a,&old,&candidate)==0);
    cleanup_plain();
    return 0;
}
''')


def test_unpublished_swap_failure_keeps_owner_and_timeout_quarantines():
    run(r'''
int main(void)
{
    struct ua_plugin_instance old,candidate;
    unsigned int c;
    setup_plain();
    assert(start(&file_a,0x00200000,1,&old)==0);
    assert(stage(&file_a,0x00200000,&candidate)==0);
    c=dev.dsp[1].cmd.write_ptr;
    fail_put_at=nputs+1; /* stop queued locally, start rejected before doorbell */
    assert(swap(&file_a,&old,&candidate)<0);
    assert(dev.dsp[1].cmd.write_ptr==c && !host.dsp[1].quarantined);
    assert(owned(&file_a,&old)->started && !owned(&file_a,&candidate)->started);
    fail_put_at=-1;
    assert(swap(&file_a,&old,&candidate)==0);
    stalled=true;
    assert(swap(&file_a,&candidate,&old)==-ETIMEDOUT);
    assert(host.dsp[1].quarantined && host.n_instances==2);
    assert(owned(&file_a,&candidate)->started && !owned(&file_a,&old)->started);
    assert(swap(&file_a,&candidate,&old)==-ESTALE);
    stalled=false;
    cleanup_plain();
    return 0;
}
''')


def test_staged_parameter_readback_precedes_activation():
    run(r'''
int main(void)
{
    struct ua_plugin_instance old,candidate,q={0};
    static u32 cmd[]={0x00140004,0x01000000,0,123};
    u32 output;
    uintptr_t p=(uintptr_t)cmd;
    setup_plain();
    assert(start(&file_a,0x00200000,1,&old)==0);
    assert(stage(&file_a,0x00200000,&candidate)==0);
    q.struct_size=sizeof(q); q.op=UA_PLUGIN_INST_SERVICE;
    q.handle=candidate.handle; q.epoch=candidate.epoch;
    q.reserved[0]=0x01000000; q.reserved[1]=4;
    q.reserved[2]=(u32)p; q.reserved[3]=(u64)p>>32;
    q.reserved[4]=1; q.privates=(uintptr_t)&output;
    assert(ua_plugin_ioctl_instance(&dev,&file_a,(unsigned long)&q)==0);
    assert(!owned(&file_a,&candidate)->started && owned(&file_a,&old)->started);
    assert(swap(&file_a,&old,&candidate)==0);
    cleanup_plain();
    return 0;
}
''')


def test_start_routes_a_staged_instance_with_no_anchor():
    run(r'''
static long start_flags(struct ua_file *f, const struct ua_plugin_instance *in, u32 flags)
{
    struct ua_plugin_instance q = {0};
    q.struct_size = sizeof(q);
    q.op = UA_PLUGIN_INST_START;
    q.handle = in->handle;
    q.epoch = in->epoch;
    q.flags = flags;
    return ua_plugin_ioctl_instance(&dev, f, (unsigned long)&q);
}
static long start_staged(struct ua_file *f, const struct ua_plugin_instance *in)
{ return start_flags(f, in, 0); }
int main(void)
{
    struct ua_plugin_instance staged, other;
    unsigned int c;
    for(unsigned int stereo=0;stereo<2;stereo++) {
        u32 pair=stereo?0x00010000:0x00200000;
        setup_plain();
        assert(stage(&file_a,pair,&staged)==0);
        assert(!owned(&file_a,&staged)->started);
        /* Another file's started instance on the same input: busy, nothing sent. */
        assert(start(&file_b,pair,1,&other)==0);
        c=dev.dsp[1].cmd.write_ptr;
        assert(start_staged(&file_a,&staged)==-EBUSY && c==dev.dsp[1].cmd.write_ptr);
        assert(destroy(&file_b,other.handle)==0);
        /* Malformed, foreign and started instances are refused before anything is sent. */
        c=dev.dsp[1].cmd.write_ptr;
        assert(start_flags(&file_a,&staged,1)==-EINVAL);
        assert(start_staged(&file_b,&staged)==-ENOENT);
        assert(c==dev.dsp[1].cmd.write_ptr);
        /* Routed exactly as START routes: the synch with the chain's lease, then the inputs. */
        assert(start_staged(&file_a,&staged)==0);
        assert(owned(&file_a,&staged)->started);
        assert(slots[2][c][0]==0x001f0004);
        assert(start_staged(&file_a,&staged)==-EPERM);
        assert(destroy(&file_a,staged.handle)==0);
        assert(!host.n_instances && !host.dsp[1].quarantined);
        cleanup_plain();
    }
    return 0;
}
''')
