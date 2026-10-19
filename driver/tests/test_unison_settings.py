"""Real setting encoder, cache, journal validation and consumption fence, no device."""
import csrc
import khost


def program(main):
    core=csrc.source('ua_core.c'); hdr=csrc.source('ua_apollo.h')
    const='\n'.join(x for x in hdr.splitlines() if x.startswith('#define UA_PREAMP_PARAM_'))
    image=csrc.between(csrc.source('ua_plugin_host.h'),'#define UA_UNISON_SETTINGS','struct ua_plugin_host {')
    preamp=csrc.between(hdr,'struct ua_preamp_state {','/* Monitor section state cache')
    code=khost.PREFIX+const+'\n'+image+preamp+r'''
#define UA_MAX_PREAMP_CH 8
#define UA_MIXER_BATCH_COUNT 38
#define UA_KIND_SET_IN_15 1
#define UA_REG_MIXER_SEQ_RD 0
#define UA_RB_PREAMP_GAINC(d,c) (((d) >> ((c)*6+(((c)&2)?4:0)))&63)
struct device { int unused; }; struct pci_dev { struct device dev; };
struct ua_plugin_host { u64 epoch; bool analog_write; };
struct ua_device {
 struct pci_dev *pdev; struct mutex lock; struct ua_plugin_host *host;
 struct { unsigned int num_preamps,num_hiz; void *model;
          struct ua_preamp_state preamp[8]; } audio;
 u64 mixer_rb_generation;
 bool mixer_ready,mixer_rb_seen,mixer_dirty;
 u32 mixer_seq_wr,mixer_val[38],mixer_mask[38],mixer_rb_data[40];
};
static struct ua_device *read_device;
static u32 blocked[38], seq_read;
static bool stall, emulate_preamp, refuse_gain;
static u32 last_masks[38];
static unsigned int flushes, custom_off_flush, gain_flush, partial_gain_writes;
static unsigned int sleeps, restore_settles;
static int ua_mixer_kind(struct ua_device *ua) { (void)ua; return 14; }
static int ua_kind_max_arm_ch(int kind) { (void)kind; return 3; }
static bool ua_kind_allows(int kind,int option) { (void)kind;(void)option;return false; }
static u32 ua_read(struct ua_device *ua,int reg) { read_device=ua;(void)reg;return seq_read; }
static bool ua_instances_analog_conflict(struct ua_device *ua,unsigned int i,u32 mask)
{ return !ua->host->analog_write && (blocked[i]&mask); }
static void ua_mixer_flush_settings(struct ua_device *ua)
{
 if(!ua->mixer_dirty || seq_read!=ua->mixer_seq_wr) return;
 memcpy(last_masks,ua->mixer_mask,sizeof(last_masks));flushes++;
 if(emulate_preamp) {
  u32 lane=0x3fu<<16;
  if(last_masks[3]&lane) {
   if((last_masks[3]&lane)!=lane) partial_gain_writes++;
   else if(!refuse_gain) {
    ua->mixer_rb_data[3]=(ua->mixer_rb_data[3]&~lane)|(ua->mixer_val[3]&lane);
    gain_flush=flushes;
   }
  }
  /* Adverse firmware model: CUSTOM-off clears PGA after earlier S3 writes.
   * This exercises the observed failure without asserting its exact silicon cause. */
  if((last_masks[11]&(1u<<10)) && !(ua->mixer_val[11]&(1u<<10))) {
   ua->mixer_rb_data[3]&=~lane;custom_off_flush=flushes;
  }
 }
 memset(ua->mixer_mask,0,sizeof(ua->mixer_mask));
 ua->mixer_seq_wr++;ua->mixer_dirty=false;
 if(!stall) seq_read=ua->mixer_seq_wr;
}
static void msleep(unsigned int ms) {
 assert(read_device && !read_device->lock.held);
 if(ms==80) {restore_settles++;return;}
 assert(ms==5);sleeps++;if(!stall) read_device->mixer_rb_generation++;
}
'''
    for sig in ('int ua_mixer_write_setting_locked(', 'static int ua_preamp_setting(',
                'void ua_unison_set_owned_locked(', 'int ua_unison_capture_locked(', 'static int ua_unison_write_image_locked(', 'static void ua_unison_changed_masks(', 'int ua_unison_apply_locked(',
                'int ua_unison_patch_locked(', 'int ua_unison_drain(', 'int ua_unison_restore('):
        code+=csrc.extract(core,sig)
    csrc.build_and_run(code+main,extra=('-Wno-unused-parameter','-Wno-unused-function','-fsanitize=address,undefined'))


def test_actual_masks_restore_snapshot_cache_and_reject_whole_bad_journal():
    program(r'''
int main(void) {
 struct ua_plugin_host h={.epoch=7};
 struct ua_device d={.host=&h,.mixer_ready=true,.mixer_rb_seen=true};
 struct ua_unison_image base, cur, before;
 u32 journal[]={2,8,1, 2,6,44, 2,12,27};
 u32 bad[]={2,6,10, 0,6,30};
 u32 invalid[]={2,6,11, 2,17,1}; /* x4 input3 has no HiZ impedance */
 u32 index,mask,val,seq;
 d.audio.num_preamps=4; d.audio.num_hiz=2; d.audio.model=&d;
 d.mixer_rb_data[0]=1u<<12; d.mixer_rb_data[3]=10u<<16;
 d.mixer_val[3]=50u<<22; /* different input must survive */
 d.mixer_val[19]=0x77000000; /* another channel's LED must survive */
 assert(ua_unison_capture_locked(&d,0x00200002,&base)==0);
 assert((base.value[3]>>16&63)==10 && (base.value[0]&(1u<<12)));
 assert((base.value[16]>>16&255)==9 && (base.value[17]>>16&255)==9);
 assert(base.mask[0]&(1u<<14)); /* pair link changes are blocked too */
 d.audio.preamp[2].gain=19;
 ua_unison_set_owned_locked(&d,0x00200002,true);
 cur=base;before=cur;
 assert(ua_unison_patch_locked(&d,&cur,bad,6)==-EPERM);
 assert(!memcmp(&cur,&before,sizeof(cur)) && !d.mixer_dirty && !d.mixer_seq_wr);
 assert(ua_unison_patch_locked(&d,&cur,invalid,6)==-EINVAL);
 assert(!memcmp(&cur,&before,sizeof(cur)) && !d.mixer_seq_wr);
 assert(ua_unison_patch_locked(&d,&cur,journal,9)==0);
 assert(d.audio.preamp[2].custom && (d.mixer_val[3]>>16&63)==44);
 assert((d.mixer_val[3]>>22&63)==50 && d.mixer_val[19]==0x771b0000);
 assert(ua_unison_drain(&d,7,&seq)==0 && seq==1);
 assert(ua_unison_apply_locked(&d,&base)==0);
 ua_unison_set_owned_locked(&d,0x00200002,false);
 assert(!d.audio.preamp[2].custom && d.audio.preamp[2].gain==19);
 assert((d.mixer_val[3]>>16&63)==10 && d.mixer_val[19]==0x77000000);
 blocked[3]=base.mask[3];
 assert(ua_mixer_write_setting_locked(&d,3,0,1u<<16)==-EBUSY);
 assert(ua_mixer_write_setting_locked(&d,3,0,1u<<22)==0);
 assert(ua_preamp_setting(&d,1,2,1,&index,&mask,&val)==-EINVAL);
 assert(ua_preamp_gain_db_from_code(0)==0);
 assert(ua_preamp_gain_db_from_code(1)==10);
 assert(ua_preamp_gain_db_from_code(56)==65);
 assert(ua_preamp_gain_db_from_code(63)==65);
 return 0;
}
''')


def test_snapshot_and_fence_refuse_unknown_state():
    program(r'''
int main(void) {
 struct ua_plugin_host h={.epoch=7};
 struct ua_device d={.host=&h,.mixer_ready=true,.mixer_rb_seen=true};
 struct ua_unison_image im;
 d.audio.num_preamps=4;
 d.mixer_rb_data[38]=~0u;
 assert(ua_unison_capture_locked(&d,0x00200002,&im)==-EAGAIN);
 d.mixer_rb_data[38]=0;d.mixer_dirty=true;
 assert(ua_unison_capture_locked(&d,0x00200002,&im)==-EAGAIN);
 stall=true;
 assert(ua_unison_drain(&d,7,NULL)==-ETIMEDOUT && sleeps==100);
 assert(ua_unison_drain(&d,8,NULL)==-ESTALE);
 return 0;
}
''')


def test_pga_zero_before_custom_preserves_ordinary_gain_cache():
    program(r'''
int main(void) {
 struct ua_plugin_host h={.epoch=7};
 struct ua_device d={.host=&h,.mixer_ready=true,.mixer_rb_seen=true};
 struct ua_unison_image base,cur;
 u32 zero[]={2,6,0}, custom[]={2,8,1};
 d.audio.num_preamps=4;d.audio.preamp[2].gain=19;
 d.mixer_rb_data[3]=10u<<16;
 assert(ua_unison_capture_locked(&d,0x00200002,&base)==0);
 cur=base;ua_unison_set_owned_locked(&d,0x00200002,true);
 assert(ua_unison_patch_locked(&d,&cur,zero,3)==0);
 assert(!d.audio.preamp[2].custom && d.audio.preamp[2].unison_owned);
 assert(d.audio.preamp[2].gain==19);
 assert(ua_unison_patch_locked(&d,&cur,custom,3)==0);
 assert(d.audio.preamp[2].custom && d.audio.preamp[2].gain==19);
 assert(ua_unison_apply_locked(&d,&base)==0);
 ua_unison_set_owned_locked(&d,0x00200002,false);
 assert(!d.audio.preamp[2].custom && !d.audio.preamp[2].unison_owned);
 assert(d.audio.preamp[2].gain==19);
 return 0;
}
''')


def test_requested_field_masks_and_deferred_physical_gain_restore():
    program(r'''
int main(void) {
 struct ua_plugin_host h={.epoch=7};
 struct ua_device d={.host=&h,.mixer_ready=true,.mixer_rb_seen=true};
 struct ua_unison_image base,cur;
 u32 zero[]={2,6,0},custom[]={2,8,1},led[]={2,12,27};
 d.audio.num_preamps=4;d.audio.preamp[2].gain=19;
 d.mixer_rb_data[3]=(10u<<16)|(10u<<22);emulate_preamp=true;
 assert(ua_unison_capture_locked(&d,0x00200002,&base)==0);
 cur=base;ua_unison_set_owned_locked(&d,0x00200002,true);
 /* Cache says zero, physical says10: the requested zero must still go out. */
 assert(ua_unison_patch_locked(&d,&cur,zero,3)==0);
 assert(last_masks[3]==(0x3fu<<16) && !last_masks[11]);
 assert((d.mixer_rb_data[3]>>16&63)==0);
 assert(ua_unison_patch_locked(&d,&cur,custom,3)==0);
 assert(last_masks[11]==(1u<<10) && !last_masks[3]);
 assert(ua_unison_patch_locked(&d,&cur,led,3)==0);
 assert(last_masks[19]==(0xffu<<16) && !last_masks[11] && !last_masks[3]);
 assert(ua_unison_restore(&d,7,&base)==0);
 assert(custom_off_flush && gain_flush>custom_off_flush && !partial_gain_writes);
 assert(restore_settles==1);
 assert(last_masks[3]==(0x3fu<<16));
 assert(last_masks[16]==(0xffu<<16) && last_masks[17]==(0xffu<<16));
 assert(!last_masks[11] && (d.mixer_rb_data[3]>>16&63)==10);
 assert((d.mixer_rb_data[3]>>22&63)==10);
 ua_unison_set_owned_locked(&d,0x00200002,false);
 assert(d.audio.preamp[2].gain==19);
 return 0;
}
''')


def test_restore_rejects_consumed_but_physically_unchanged_gain():
    program(r'''
int main(void) {
 struct ua_plugin_host h={.epoch=7};
 struct ua_device d={.host=&h,.mixer_ready=true,.mixer_rb_seen=true};
 struct ua_unison_image base;
 d.audio.num_preamps=4;d.mixer_rb_data[3]=10u<<16;
 assert(ua_unison_capture_locked(&d,0x00200002,&base)==0);
 emulate_preamp=true;refuse_gain=true;
 assert(ua_unison_restore(&d,7,&base)==-EIO);
 assert((d.mixer_rb_data[3]>>16&63)==0);
 return 0;
}
''')
