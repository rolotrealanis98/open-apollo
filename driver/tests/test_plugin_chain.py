"""Every effect in the immutable original chain directory needs admission."""
import test_plugin_strip_restore as strips

HELPERS = strips.HELPERS + r'''
static struct ua_plugin_private chain_rows[] = {
    {1,12,0xffffffff,0}, {1,8,6,0}, {1,8,8,0}, {1,8,10,0}
};
static u32 chain_cmd[] = {
    0x00140004,0,0,0x40000000,
    0x00140004,5,0,3,
    0x00140004,7,0,0x846,
    0x00140004,9,0,0x806,
    0x00140004,11,0,0x816,
};
static u32 chain_synch=0x00200000;  /* in2 << 16 | in1 */
static long chain(struct ua_plugin_instance *q, unsigned int nw, u32 flags)
{
    uintptr_t p=(uintptr_t)chain_cmd;
    memset(q,0,sizeof(*q));
    memcpy(res[0].sha256,sha_code,32); memcpy(res[1].sha256,sha_data,32);
    q->struct_size=sizeof(*q); q->op=UA_PLUGIN_INST_CREATE;
    q->flags=flags; q->render_id=0x806; q->dsp=1;
    q->n_resources=2; q->n_private=4;
    q->resources=(uintptr_t)res; q->privates=(uintptr_t)chain_rows;
    q->reserved[0]=chain_synch; q->reserved[1]=nw;
    q->reserved[2]=(u32)p; q->reserved[3]=(u64)p>>32;
    return ua_plugin_ioctl_instance(&dev,&file_a,(unsigned long)q);
}
static long edit_head(struct ua_plugin_instance *in, unsigned int word)
{
    struct ua_plugin_instance q={0};
    u32 c[]={0x00140004,word,0,1};
    uintptr_t p=(uintptr_t)c;
    q.struct_size=sizeof(q); q.op=UA_PLUGIN_INST_SERVICE;
    q.handle=in->handle; q.epoch=in->epoch;
    q.reserved[1]=4; q.reserved[2]=(u32)p; q.reserved[3]=(u64)p>>32;
    return ua_plugin_ioctl_instance(&dev,&file_a,(unsigned long)&q);
}
'''


def run(main):
    strips.license.build_license(HELPERS + main, real_strips=True)


def test_every_effect_admitted_from_one_fresh_board_query():
    run(r'''
int main(void)
{
    struct ua_plugin_instance q;
    unsigned int queries;
    setup_plain();
    queries=host.board_queries;
    assert(chain(&q,20,UA_PLUGIN_CREATE_F_START|UA_PLUGIN_CREATE_F_CHAIN)==-EACCES);
    assert(host.board_queries==queries+1 && !uploads() && !host.n_instances);
    assert(q.license_state==UA_PLUGIN_LICENSE_UNAVAILABLE);
    bq_table[0x16]=0x81000000;
    queries=host.board_queries;
    assert(chain(&q,20,UA_PLUGIN_CREATE_F_START|UA_PLUGIN_CREATE_F_CHAIN)==0);
    assert(host.board_queries==queries+1 && host.n_instances==1);
    assert(destroy(&file_a,q.handle)==0);
    /* Revocation is checked again; no cached admission survives a new create. */
    bq_table[0x16]=0x80000000;
    queries=host.board_queries;
    assert(chain(&q,20,UA_PLUGIN_CREATE_F_STAGE|UA_PLUGIN_CREATE_F_CHAIN)==-EACCES);
    assert(host.board_queries==queries+1 && !host.n_instances);
    cleanup_plain();
    return 0;
}
''')


def test_incomplete_ambiguous_unbound_and_unflagged_directory_refused_before_query():
    run(r'''
int main(void)
{
    struct ua_plugin_instance q;
    unsigned int queries;
    setup_plain();
    queries=host.board_queries;
    assert(chain(&q,16,UA_PLUGIN_CREATE_F_START|UA_PLUGIN_CREATE_F_CHAIN)==-EINVAL);
    chain_cmd[17]=9; /* repeated ID write, no explicit last member */
    assert(chain(&q,20,UA_PLUGIN_CREATE_F_START|UA_PLUGIN_CREATE_F_CHAIN)==-EINVAL);
    chain_cmd[17]=11;
    chain_cmd[7]=6;
    assert(chain(&q,20,UA_PLUGIN_CREATE_F_START|UA_PLUGIN_CREATE_F_CHAIN)==-EINVAL);
    chain_cmd[7]=3;
    chain_cmd[11]=0x806; /* carrier isn't the built-in Mic strip */
    assert(chain(&q,20,UA_PLUGIN_CREATE_F_START|UA_PLUGIN_CREATE_F_CHAIN)==-EINVAL);
    chain_cmd[11]=0x846;
    chain_cmd[15]=0x816; /* primary render ID not bound to first effect */
    assert(chain(&q,20,UA_PLUGIN_CREATE_F_START|UA_PLUGIN_CREATE_F_CHAIN)==-EINVAL);
    chain_cmd[15]=0x806;
    chain_rows[3].link=0xffffffff;
    assert(chain(&q,20,UA_PLUGIN_CREATE_F_START|UA_PLUGIN_CREATE_F_CHAIN)==-EINVAL);
    chain_rows[3].link=10;
    assert(chain(&q,20,UA_PLUGIN_CREATE_F_START)==-EOPNOTSUPP);
    assert(chain(&q,20,UA_PLUGIN_CREATE_F_STAGE)==-EOPNOTSUPP);
    assert(host.board_queries==queries && !uploads() && !host.n_instances);
    cleanup_plain();
    return 0;
}
''')


def test_staged_and_active_service_cannot_rewrite_topology():
    run(r'''
int main(void)
{
    struct ua_plugin_instance q;
    unsigned int sent;
    setup_plain();
    bq_table[0x16]=0x81000000;
    assert(chain(&q,20,UA_PLUGIN_CREATE_F_STAGE|UA_PLUGIN_CREATE_F_CHAIN)==0);
    sent=nputs;
    assert(edit_head(&q,0)==-EINVAL);
    assert(edit_head(&q,5)==-EINVAL);
    assert(edit_head(&q,7)==-EINVAL);
    assert(edit_head(&q,9)==-EINVAL);
    assert(edit_head(&q,11)==-EINVAL);
    assert(nputs==sent);
    assert(destroy(&file_a,q.handle)==0);
    assert(chain(&q,20,UA_PLUGIN_CREATE_F_START|UA_PLUGIN_CREATE_F_CHAIN)==0);
    sent=nputs;
    assert(edit_head(&q,5)==-EINVAL && edit_head(&q,11)==-EINVAL);
    assert(nputs==sent);
    cleanup_plain();
    return 0;
}
''')


def test_duplicate_effects_still_require_the_shared_license():
    run(r'''
int main(void)
{
    struct ua_plugin_instance q;
    setup_plain();
    chain_cmd[19]=0x806;
    assert(chain(&q,20,UA_PLUGIN_CREATE_F_START|UA_PLUGIN_CREATE_F_CHAIN)==0);
    assert(destroy(&file_a,q.handle)==0);
    bq_table[6]=0;
    assert(chain(&q,20,UA_PLUGIN_CREATE_F_START|UA_PLUGIN_CREATE_F_CHAIN)==-EACCES);
    assert(q.license_state==UA_PLUGIN_LICENSE_EXPIRED && !host.n_instances);
    cleanup_plain();
    return 0;
}
''')


def test_input_strip_carrier_replaces_and_restores_a_virtual_input():
    run(r'''
static void add_virtual(void)
{
    struct ua_strip *s = &plain->strip[2];
    s->kind = UA_STRIP_KIND_INPUT;
    s->input = 0;
    s->synch = 0x18;
    s->state = UA_STRIP_LIVE;
    for (unsigned int j = 0; j < UA_STRIP_INPUT_PRIV; j++)
        assert(ua_pool_reserve(host.dsp[1].pools, 1, 8, &s->priv[j],
                               false, &s->priv[j]) == 0);
    s->header = s->priv[0].base;
    plain->n = 3;
}
int main(void)
{
    struct ua_plugin_instance q;
    unsigned int c, k, restored = 0;
    setup_plain();
    add_virtual();
    bq_table[0x16]=0x81000000;
    /* Mic carrier on a Virtual input, Input carrier on an analog input: refused */
    assert(chain(&q,20,UA_PLUGIN_CREATE_F_START|UA_PLUGIN_CREATE_F_CHAIN)==0);
    assert(destroy(&file_a,q.handle)==0);
    chain_synch=0x00200018;
    assert(chain(&q,20,UA_PLUGIN_CREATE_F_START|UA_PLUGIN_CREATE_F_CHAIN)==-EINVAL);
    chain_cmd[11]=0x83e;
    chain_synch=0x00200000;
    assert(chain(&q,20,UA_PLUGIN_CREATE_F_START|UA_PLUGIN_CREATE_F_CHAIN)==-EINVAL);
    assert(!host.n_instances);
    /* Input carrier on Virtual 1: started there, and its Input Strip back after */
    chain_synch=0x00200018;
    assert(chain(&q,20,UA_PLUGIN_CREATE_F_START|UA_PLUGIN_CREATE_F_CHAIN)==0);
    assert(host.n_instances==1);
    c = dev.dsp[1].cmd.write_ptr;
    assert(destroy(&file_a,q.handle)==0);
    for (k = 0; k < 8; k++, c = (c + 1) & 1023)
        if (slots[2][c][0] == 0x001f0004 && slots[2][c][1] == 0x00200018 &&
            slots[2][c][2] == plain->strip[2].header)
            restored++;
    assert(restored == 1 && !host.n_instances && !host.dsp[1].quarantined);
    chain_cmd[11]=0x846;
    chain_synch=0x00200000;
    cleanup_plain();
    return 0;
}
''')
