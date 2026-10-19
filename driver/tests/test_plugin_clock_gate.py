"""Strip construction requires an actual rate acknowledgement and settled clock."""
import csrc
import khost


def test_rate_gate_matches_clock_ack_and_avoids_reads_when_disconnected():
    code = khost.PREFIX + r'''
#define UA_REG_NOTIF_RATE_INFO 0xc07c
struct ua_audio { bool connected; u32 sample_rate, rate_acked, rate_index; };
struct ua_device { struct mutex lock; atomic_t shutdown; struct ua_audio audio; };
static u32 rate_info;
static unsigned int reads;
static u32 ua_read(struct ua_device *ua,u32 reg)
{ (void)ua; assert(reg==UA_REG_NOTIF_RATE_INFO); reads++; return rate_info; }
''' + csrc.extract(csrc.source('ua_audio.c'), 'bool ua_audio_plugin_rate_ready(') + r'''
int main(void)
{
    struct ua_device ua={0};
    ua.audio.sample_rate=48000; ua.audio.rate_index=2;
    ua.audio.rate_acked=96000; rate_info=0x401;
    mutex_lock(&ua.lock);
    assert(!ua_audio_plugin_rate_ready(&ua) && !reads);
    ua.audio.connected=true;
    assert(!ua_audio_plugin_rate_ready(&ua) && !reads);
    ua.audio.rate_acked=48000;
    assert(!ua_audio_plugin_rate_ready(&ua));
    rate_info=0x200; assert(!ua_audio_plugin_rate_ready(&ua));
    rate_info=0xffffffff; assert(!ua_audio_plugin_rate_ready(&ua));
    rate_info=0x201; assert(ua_audio_plugin_rate_ready(&ua));
    unsigned int before=reads;
    ua.shutdown.counter=1;
    assert(!ua_audio_plugin_rate_ready(&ua) && reads==before);
    mutex_unlock(&ua.lock);
    return 0;
}
'''
    csrc.build_and_run(code)
