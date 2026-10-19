"""Licensed integrated headers use explicit admission, including the fifth member."""
import test_plugin_strip_restore as strips

HELPERS = strips.HELPERS + r'''
static struct ua_plugin_private unison_rows[] = {
    {1,16,0xffffffff,0}, {1,8,6,0}, {1,8,8,0}, {1,8,10,0},
    {1,8,12,0}, {1,8,14,0}
};
static u32 unison_cmd[28];
static long unison(struct ua_plugin_instance *q, unsigned int n, u32 flags)
{
    uintptr_t p=(uintptr_t)unison_cmd;
    unsigned int i;
    u32 ids[]={0x806,0x816,0x826,0x836,0x856};
    unison_rows[0].dwords=6+2*n;
    unison_cmd[0]=0x00140004;unison_cmd[1]=0;unison_cmd[2]=0;unison_cmd[3]=0x40000000;
    unison_cmd[4]=0x00140004;unison_cmd[5]=5;unison_cmd[6]=0;unison_cmd[7]=n;
    for(i=0;i<n;i++) {
        unison_cmd[8+4*i]=0x00140004;unison_cmd[9+4*i]=7+2*i;
        unison_cmd[10+4*i]=0;unison_cmd[11+4*i]=ids[i];
    }
    memset(q,0,sizeof(*q));
    memcpy(res[0].sha256,sha_code,32);memcpy(res[1].sha256,sha_data,32);
    q->struct_size=sizeof(*q);q->op=UA_PLUGIN_INST_CREATE;q->flags=flags;
    q->render_id=0x806;q->dsp=1;q->n_resources=2;q->n_private=n+1;
    q->resources=(uintptr_t)res;q->privates=(uintptr_t)unison_rows;
    q->reserved[0]=0x00200000;q->reserved[1]=8+4*n;
    q->reserved[2]=(u32)p;q->reserved[3]=(u64)p>>32;
    return ua_plugin_ioctl_instance(&dev,&file_a,(unsigned long)q);
}
'''

def run(body):
    strips.license.build_license(HELPERS+body,real_strips=True)


def test_unison_header_and_fifth_member_share_fresh_admission():
    run(r'''
int main(void) {
    struct ua_plugin_instance q;
    unsigned int queries;
    u32 flags=UA_PLUGIN_CREATE_F_STAGE|UA_PLUGIN_CREATE_F_CHAIN|UA_PLUGIN_CREATE_F_UNISON;
    setup_plain();
    bq_table[0x16]=bq_table[0x26]=bq_table[0x36]=0x81000000;
    queries=host.board_queries;
    assert(unison(&q,5,flags)==-EACCES);
    assert(host.board_queries==queries+1 && !uploads() && !host.n_instances);
    bq_table[0x56]=0x81000000;
    queries=host.board_queries;
    assert(unison(&q,5,flags)==0);
    assert(host.board_queries==queries+1 && host.n_instances==1);
    assert(destroy(&file_a,q.handle)==0);
    bq_table[6]=0x80000000; /* header itself must be licensed */
    queries=host.board_queries;
    assert(unison(&q,1,flags)==-EACCES);
    assert(host.board_queries==queries+1 && !host.n_instances);
    bq_table[6]=0x81000000;
    assert(unison(&q,1,flags)==0);
    assert(destroy(&file_a,q.handle)==0);
    cleanup_plain();return 0;
}
''')


def test_unison_requires_chain_and_ordinary_header_rule_is_preserved():
    run(r'''
int main(void) {
    struct ua_plugin_instance q;
    unsigned int queries;
    setup_plain();queries=host.board_queries;
    assert(unison(&q,1,UA_PLUGIN_CREATE_F_STAGE|UA_PLUGIN_CREATE_F_UNISON)==-EINVAL);
    assert(unison(&q,1,UA_PLUGIN_CREATE_F_STAGE|UA_PLUGIN_CREATE_F_CHAIN)==-EINVAL);
    assert(unison(&q,2,UA_PLUGIN_CREATE_F_STAGE|UA_PLUGIN_CREATE_F_CHAIN)==-EINVAL);
    assert(unison(&q,0,UA_PLUGIN_CREATE_F_STAGE|UA_PLUGIN_CREATE_F_CHAIN|UA_PLUGIN_CREATE_F_UNISON)==-EINVAL);
    assert(host.board_queries==queries && !host.n_instances && !uploads());
    cleanup_plain();return 0;
}
''')


LEASE_HELPERS = r'''

static long lifecycle(struct ua_file *f, struct ua_plugin_instance *in, u32 op)
{
 struct ua_plugin_instance q={.struct_size=sizeof(q),.op=op,.handle=in->handle,.epoch=in->epoch};
 long ret=ua_plugin_ioctl_instance(&dev,f,(unsigned long)&q);
 if(!ret) assert(q.reserved[5]==1 && q.reserved[4]);
 return ret;
}
static long command(struct ua_file *f, struct ua_plugin_instance *in, u32 param, u32 value)
{
 u32 w[]={0,param,value};uintptr_t p=(uintptr_t)w;
 struct ua_plugin_instance q={.struct_size=sizeof(q),.op=UA_PLUGIN_INST_UNISON_PARAMS,.handle=in->handle,.epoch=in->epoch};
 q.reserved[1]=3;q.reserved[2]=(u32)p;q.reserved[3]=(u64)p>>32;
 return ua_plugin_ioctl_instance(&dev,f,(unsigned long)&q);
}
/* Synthetic userspace coordinator: no worker journal needed for these fake rows. */
static long launched(struct ua_plugin_instance *q)
{
 long ret=unison(q,1,UA_PLUGIN_CREATE_F_CHAIN|UA_PLUGIN_CREATE_F_UNISON|UA_PLUGIN_CREATE_F_STAGE);
 if(ret) return ret;
 ret=lifecycle(&file_a,q,UA_PLUGIN_INST_BEGIN_UNISON);
 if(!ret) ret=lifecycle(&file_a,q,UA_PLUGIN_INST_ACTIVATE_UNISON);
 if(!ret) ret=command(&file_a,q,8,1);
 if(ret) destroy(&file_a,q->handle);
 return ret;
}
static long params(struct ua_file *f, struct ua_plugin_instance *in, u32 value)
{
 u32 w[]={0,6,value}; uintptr_t p=(uintptr_t)w;
 struct ua_plugin_instance q={.struct_size=sizeof(q),.op=UA_PLUGIN_INST_UNISON_PARAMS,
  .handle=in->handle,.epoch=in->epoch};
 q.reserved[1]=3;q.reserved[2]=(u32)p;q.reserved[3]=(u64)p>>32;
 return ua_plugin_ioctl_instance(&dev,f,(unsigned long)&q);
}
static long handoff(struct ua_plugin_instance *from, struct ua_plugin_instance *to)
{
 struct ua_plugin_instance q={.struct_size=sizeof(q),.op=UA_PLUGIN_INST_SWAP,
  .handle=from->handle,.epoch=from->epoch};q.reserved[0]=to->handle;
 return ua_plugin_ioctl_instance(&dev,&file_a,(unsigned long)&q);
}
'''

def test_analog_fd_epoch_swap_inverse_and_close_restore():
    run(LEASE_HELPERS + r'''
int main(void) {
 struct ua_plugin_instance a,b;
 u32 flags=UA_PLUGIN_CREATE_F_CHAIN|UA_PLUGIN_CREATE_F_UNISON;
 setup_plain();
 assert(launched(&a)==0);
 assert(analog_captures==1 && analog_physical==0);
 assert(analog_settle_sleeps==1 && analog_pga_puts < analog_custom_puts);
 assert(params(&file_b,&a,30)==-ENOENT && analog_physical==0);
 a.epoch++;assert(params(&file_a,&a,30)==-ESTALE);a.epoch--;
 assert(params(&file_a,&a,30)==0 && analog_physical==30);
 assert(unison(&b,1,flags|UA_PLUGIN_CREATE_F_STAGE)==0);
 assert(analog_captures==1 && analog_physical==30);
 assert(params(&file_a,&b,40)==-EPERM);
 assert(handoff(&a,&b)==0 && analog_physical==30);
 assert(analog_settle_sleeps==1);
 assert(params(&file_a,&b,40)==0 && analog_physical==40);
 assert(handoff(&b,&a)==0 && analog_physical==30);
 assert(destroy(&file_a,b.handle)==0 && analog_physical==30);
 ua_plugin_file_release(&file_a);
 assert(!host.n_instances && analog_physical==19);
 cleanup_plain(); return 0;
}
''')


def test_analog_unknown_completion_blocks_lane_and_quarantines():
    run(LEASE_HELPERS + r'''
int main(void) {
 struct ua_plugin_instance a;
 setup_plain();
 assert(launched(&a)==0);
 analog_drain_error=-ETIMEDOUT;
 assert(params(&file_a,&a,30)==-ETIMEDOUT);
 assert(host.dsp[1].quarantined && host.analog_block[3]==0x3f);
 mutex_lock(&dev.lock); assert(ua_instances_analog_conflict(&dev,3,1));mutex_unlock(&dev.lock);
 ua_plugin_file_release(&file_a);
 assert(!host.n_instances && host.analog_block[3]==0x3f);
 cleanup_plain();return 0;
}
''')


def test_ordinary_transition_restores_and_reacquires_baseline():
    run(LEASE_HELPERS + r'''
static long ordinary_stage(struct ua_plugin_instance *q)
{
 u32 cmd[]={0x00140004,0x01000000,0,42};uintptr_t p=(uintptr_t)cmd;
 memset(q,0,sizeof(*q));
 q->struct_size=sizeof(*q);q->op=UA_PLUGIN_INST_CREATE;q->flags=UA_PLUGIN_CREATE_F_STAGE;
 q->render_id=0x806;q->dsp=1;q->n_resources=q->n_private=2;
 q->resources=(uintptr_t)res;q->privates=(uintptr_t)priv;
 q->reserved[0]=0x00200000;q->reserved[1]=4;q->reserved[2]=(u32)p;q->reserved[3]=(u64)p>>32;
 return ua_plugin_ioctl_instance(&dev,&file_a,(unsigned long)q);
}
int main(void) {
 struct ua_plugin_instance a,b;
 setup_plain();
 assert(launched(&a)==0);
 assert(params(&file_a,&a,30)==0);
 assert(ordinary_stage(&b)==0);
 assert(handoff(&a,&b)==0 && analog_physical==19);
 analog_physical=23; /* a permitted ordinary change after ownership ended */
 assert(handoff(&b,&a)==-EPERM); /* explicit worker re-entry is required */
 assert(lifecycle(&file_a,&a,UA_PLUGIN_INST_BEGIN_UNISON)==0);
 mutex_lock(&dev.lock);
 assert(ua_inst_find(&host,&file_a,a.handle)->analog_rollback.value[3]==30);
 mutex_unlock(&dev.lock);
 assert(params(&file_a,&a,30)==0); /* caller restores worker's accepted timeline */
 assert(handoff(&b,&a)==0 && analog_physical==30);
 assert(command(&file_a,&a,8,1)==0);
 assert(analog_captures==2 && analog_settle_sleeps==2);
 assert(destroy(&file_a,b.handle)==0 && analog_physical==30);
 assert(destroy(&file_a,a.handle)==0 && analog_physical==23);
 cleanup_plain();return 0;
}
''')


def test_close_restore_timeout_is_not_reported_as_clean_release():
    run(LEASE_HELPERS + r'''
int main(void) {
 struct ua_plugin_instance a;
 setup_plain();
 assert(launched(&a)==0);
 assert(params(&file_a,&a,30)==0);
 analog_drain_error=-ETIMEDOUT;
 ua_plugin_file_release(&file_a);
 assert(!host.n_instances && host.dsp[1].quarantined);
 assert(host.analog_block[3]==0x3f);
 cleanup_plain();return 0;
}
''')


def test_device_loss_retains_block_without_attempting_analog_writes():
    run(LEASE_HELPERS + r'''
int main(void) {
 struct ua_plugin_instance a;
 unsigned int applies;
 setup_plain();
 assert(launched(&a)==0);
 assert(params(&file_a,&a,30)==0);applies=analog_applies;
 mutex_lock(&dev.lock);ua_instances_dsp_gone(&dev,1);mutex_unlock(&dev.lock);
 assert(host.analog_block[3]==0x3f && host.dsp[1].quarantined);
 ua_plugin_file_release(&file_a);
 assert(!host.n_instances && analog_applies==applies);
 cleanup_plain();return 0;
}
''')


def test_bootstrap_pga_timeout_never_enables_custom_or_reports_started():
    run(LEASE_HELPERS + r'''
int main(void) {
 struct ua_plugin_instance a;
 setup_plain();analog_drain_fail_at=2; /* initial baseline fence passes, PGA0 fence fails */
 assert(launched(&a)==-ETIMEDOUT);
 assert(!host.n_instances && host.dsp[1].quarantined && host.analog_block[3]==0x3f);
 assert(!analog_custom && !analog_custom_puts && !analog_settle_sleeps);
 cleanup_plain();return 0;
}
''')


def test_begin_and_activate_are_explicit_and_begun_close_restores():
    run(LEASE_HELPERS + r'''
int main(void) {
 struct ua_plugin_instance a;
 unsigned int before;
 setup_plain();
 assert(unison(&a,1,UA_PLUGIN_CREATE_F_CHAIN|UA_PLUGIN_CREATE_F_UNISON|UA_PLUGIN_CREATE_F_START)==-EOPNOTSUPP);
 assert(unison(&a,1,UA_PLUGIN_CREATE_F_CHAIN|UA_PLUGIN_CREATE_F_UNISON|UA_PLUGIN_CREATE_F_STAGE)==0);
 before=nputs;
 assert(lifecycle(&file_b,&a,UA_PLUGIN_INST_BEGIN_UNISON)==-ENOENT);
 assert(lifecycle(&file_a,&a,UA_PLUGIN_INST_ACTIVATE_UNISON)==-EPERM);
 assert(lifecycle(&file_a,&a,UA_PLUGIN_INST_BEGIN_UNISON)==0);
 assert(nputs==before && analog_physical==0 && !analog_custom && analog_settle_sleeps==1);
 assert(lifecycle(&file_a,&a,UA_PLUGIN_INST_BEGIN_UNISON)==-EALREADY);
 assert(params(&file_a,&a,15)==0 && analog_physical==15 && !analog_custom);
 ua_plugin_file_release(&file_a);
 assert(!host.n_instances && analog_physical==19 && !analog_custom);
 cleanup_plain();return 0;
}
''')


def test_begin_with_ordinary_route_requires_swap_and_preserves_cleanup():
    run(LEASE_HELPERS + r'''
int main(void) {
 struct ua_plugin_instance ordinary,candidate;
 setup_plain();
 assert(start(&file_a,0x00200000,1,&ordinary)==0);
 assert(unison(&candidate,1,UA_PLUGIN_CREATE_F_CHAIN|UA_PLUGIN_CREATE_F_UNISON|UA_PLUGIN_CREATE_F_STAGE)==0);
 assert(lifecycle(&file_a,&candidate,UA_PLUGIN_INST_BEGIN_UNISON)==0);
 assert(lifecycle(&file_a,&candidate,UA_PLUGIN_INST_ACTIVATE_UNISON)==-EBUSY);
 assert(params(&file_a,&candidate,12)==0);
 assert(handoff(&ordinary,&candidate)==0 && analog_physical==12 && !analog_custom);
 assert(command(&file_a,&candidate,8,1)==0);
 assert(destroy(&file_a,ordinary.handle)==0 && analog_physical==12);
 assert(destroy(&file_a,candidate.handle)==0 && analog_physical==19);
 cleanup_plain();return 0;
}
''')


def test_begin_cannot_change_a_foreign_files_routed_input():
    run(LEASE_HELPERS + r'''
int main(void) {
 struct ua_plugin_instance ordinary,candidate;
 setup_plain();
 assert(start(&file_b,0x00200000,1,&ordinary)==0);
 assert(unison(&candidate,1,UA_PLUGIN_CREATE_F_CHAIN|UA_PLUGIN_CREATE_F_UNISON|UA_PLUGIN_CREATE_F_STAGE)==0);
 assert(lifecycle(&file_a,&candidate,UA_PLUGIN_INST_BEGIN_UNISON)==-EBUSY);
 assert(analog_physical==19 && !analog_captures && !analog_settle_sleeps);
 assert(destroy(&file_a,candidate.handle)==0);
 assert(destroy(&file_b,ordinary.handle)==0);
 cleanup_plain();return 0;
}
''')
