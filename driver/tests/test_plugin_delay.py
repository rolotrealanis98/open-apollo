"""Bounded retained-strip delay controls; synthetic SRAM and real DSP transactions."""
import test_plugin_license as license
import test_plugin_strip_restore as strips

MEMORY = r'''
static u32 delay_base[4], delay_words[4][12];
static unsigned int delay_writes, delay_reads;
static bool corrupt_delay_reply;
static void delay_write(u32 addr,u32 value)
{
    for(unsigned int ch=0;ch<4;ch++)
        if(addr>=delay_base[ch]+5 && addr<delay_base[ch]+17) {
            delay_words[ch][addr-delay_base[ch]-5]=value;
            delay_writes++;
        }
}
'''
READ = r'''
        } else if(e[0]==0x000c0004) {
            u32 r[14]={0x8001000e,5};
            unsigned int ch;
            assert(e[2]==12 && e[3]==5);
            for(ch=0;ch<4;ch++) if(e[1]==delay_base[ch]+5) break;
            assert(ch<4);
            memcpy(r+2,delay_words[ch],12*4);
            if(corrupt_delay_reply && delay_writes) r[13]^=1;
            reply_on(d,r,14);
            delay_reads++;
'''
HELPERS = strips.HELPERS + r'''
static void setup_delay(void)
{
    setup();
    bq_table[6]=0x81000000;
    plain=kzalloc(sizeof(*plain),GFP_KERNEL); assert(plain);
    host.strips=plain; ua_strips_dsp=1; input_strips_steps=0;
    plain->state=UA_STRIPS_LIVE; plain->epoch=host.epoch; plain->gen=9;
    plain->n=plain->n_mic=4; plain->rate.rate=48000;
    plain->rebuild_seen=atomic_read(&ua_strips_rebuilds);
    delay_writes=delay_reads=0; corrupt_delay_reply=false;
    memset(delay_words,0,sizeof(delay_words));
    for(unsigned int ch=0;ch<4;ch++) {
        struct ua_strip *s=&plain->strip[ch];
        s->synch=s->input=ch; s->state=UA_STRIP_LIVE;
        for(unsigned int j=0;j<UA_STRIP_PRIV;j++)
            assert(ua_pool_reserve(host.dsp[1].pools,ua_strip_rows[j].pool,
                   ua_strip_rows[j].dwords,&s->priv[j],false,&s->priv[j])==0);
        s->header=s->priv[0].base;
        delay_base[ch]=s->priv[1].base;
        delay_words[ch][0]=256; delay_words[ch][11]=200;
    }
}
static long delay(struct ua_plugin_delay *q)
{ return ua_plugin_ioctl_delay(&dev,(unsigned long)q); }
static struct ua_plugin_delay query(void)
{
    struct ua_plugin_delay q={0}; q.struct_size=sizeof(q);
    assert(delay(&q)==0); return q;
}
static void long_delay(struct ua_plugin_delay *q)
{
    q->flags=7;
    for(unsigned int i=0;i<4;i++) {
        q->record[i]=1000-i; q->monitor[i]=i; q->capacity[i]=2048;
    }
}
'''


def run(main):
    old = license.fake.FPGA
    license.fake.FPGA = MEMORY + old.replace(
        '\t\t} else if (e[0] == 0x00100002) {',
        READ + '\t\t} else if (e[0] == 0x00100002) {').replace(
        '\t\t\t\t\tassert(*cmd_left);',
        '\t\t\t\t\tassert(*cmd_left);\n'
        '\t\t\t\t\tif(op==0x14 && *cmd_left==4 && p[i+2]==0)\n'
        '\t\t\t\t\t\tdelay_write(p[i+1],p[i+3]);')
    try:
        license.build_license(HELPERS + main, real_strips=True)
    finally:
        license.fake.FPGA = old


def test_all_four_fields_and_overridden_strip_survive_unload():
    run(r'''
int main(void)
{
    struct ua_plugin_delay q;
    struct ua_plugin_instance insert;
    setup_delay();
    assert(sizeof(q)==72);
    q=query(); assert(q.epoch==host.epoch && q.generation==9 && q.input_mask==15);
    for(unsigned int i=0;i<4;i++) assert(q.record[i]==200 && !q.monitor[i] && q.capacity[i]==256);
    assert(start(&file_a,0x00200000,1,&insert)==0);
    long_delay(&q); assert(delay(&q)==0);
    assert(delay_writes==12);
    for(unsigned int i=0;i<4;i++) {
        assert(delay_words[i][11]==1000-i && delay_words[i][3]==i);
        assert(delay_words[i][0]==2048);
    }
    assert(destroy(&file_a,insert.handle)==0);
    q=query(); assert(q.record[0]==1000 && q.capacity[0]==2048);
    q.flags=1; q.record[0]=500;
    assert(delay(&q)==0 && q.monitor[3]==3 && q.capacity[0]==2048);
    q.flags=7;
    memset(q.record,0,sizeof(q.record)); memset(q.monitor,0,sizeof(q.monitor));
    memset(q.capacity,0,sizeof(q.capacity));
    assert(delay(&q)==0);
    q=query(); for(unsigned int i=0;i<4;i++) assert(!q.record[i] && !q.monitor[i] && !q.capacity[i]);
    cleanup_plain(); return 0;
}
''')


def test_invalid_capacity_rate_generation_and_mask_never_write():
    run(r'''
int main(void)
{
    struct ua_plugin_delay q;
    setup_delay(); q=query(); long_delay(&q);
    q.generation++; assert(delay(&q)==-ESTALE); q.generation--;
    q.input_mask=3; assert(delay(&q)==-EINVAL); q.input_mask=15;
    q.capacity[0]=2000; assert(delay(&q)==-EINVAL); q.capacity[0]=2048;
    q.record[0]=2001; assert(delay(&q)==-EINVAL); q.record[0]=1000;
    q.capacity[0]=0; assert(delay(&q)==-EINVAL); q.capacity[0]=2048;
    dev.audio.sample_rate=96000; assert(delay(&q)==-ESTALE); dev.audio.sample_rate=48000;
    plain->strip[1].priv[6].words=256; assert(delay(&q)==-ESTALE);
    plain->strip[1].priv[6].words=2048;
    assert(!delay_writes && !host.dsp[1].quarantined);
    cleanup_plain(); return 0;
}
''')


def test_unpublished_apply_preserves_every_prior_value():
    run(r'''
int main(void)
{
    struct ua_plugin_delay q;
    setup_delay(); q=query(); long_delay(&q);
    fail_put_at=nputs+12; /* four initial replies+reads, then four reply descriptors */
    assert(delay(&q)==-EBUSY);
    assert(!delay_writes && !host.dsp[1].quarantined && !plain->delay_busy);
    for(unsigned int i=0;i<4;i++) assert(delay_words[i][11]==200 && delay_words[i][0]==256);
    fail_put_at=~0u;
    assert(delay(&q)==0);
    cleanup_plain(); return 0;
}
''')


def test_bad_readback_and_published_timeout_quarantine_without_false_rollback():
    run(r'''
static unsigned int stall_after_reads;
static void stall_apply(void)
{
    if(delay_reads>=stall_after_reads) { stalled=true; on_tick=NULL; }
}
int main(void)
{
    struct ua_plugin_delay q;
    setup_delay(); q=query(); long_delay(&q);
    corrupt_delay_reply=true;
    assert(delay(&q)==-EPROTO);
    assert(host.dsp[1].quarantined && delay_writes==12);
    assert(delay_words[0][11]==1000); /* changed hardware, never claimed rollback */
    assert(delay(&q)==-ESTALE);
    cleanup_plain();
    setup_delay(); q=query(); long_delay(&q);
    stall_after_reads=delay_reads+4; on_tick=stall_apply;
    assert(delay(&q)==-ETIMEDOUT);
    assert(host.dsp[1].quarantined && !plain->delay_busy);
    stalled=false;
    cleanup_plain(); return 0;
}
''')
