"""UA_IOCTL_DMA_TEST: never under open streams, keeps pcm_prepare off
while it drops ua->lock to wait, and stops the transport on every path.

ua_audio_dma_test() is compiled as it is.  driver-v2 runs it under
ua->lock (ua_ioctl_dma_test()) with dma_test_active instead of Codex's
connect_lock wrapper.  Rewritten from the parallel Codex work
(test_dma_test_serialization).
"""
import csrc

PREFIX = r'''
#include <assert.h>
#include <errno.h>
#include <stdatomic.h>
#include <stdbool.h>
#include <stdint.h>
#include <string.h>
typedef uint32_t u32;
typedef uint32_t __u32;
#define UA_DMA_BUF_SIZE 4096
#define UA_REG_DMA_CTRL 0x2200
#define UA_REG_AX_SAMPLE_POS 0x2244
#define UA_REG_AX_CONTROL 0x2248
#define UA_REG_NOTIF_CLOCK_CFG 0xC074
#define UA_REG_AX_CONNECT 0x2260
#define UA_AX_CLOCK_DOORBELL 4
#define wmb() ((void)0)
#define atomic_read(p) atomic_load(p)
struct ua_dma_test { __u32 dma_ctrl, delay_ms, flags, sample_pos, nonzero,
		     non_zero_non_canary, data[256]; };
struct device { int dev; };
struct pci_dev { struct device dev; };
struct ua_dma_ring { int unused; };
struct ua_audio { void *rec_buf; bool connected, transport_running,
		  dma_test_active; void *play_sub, *rec_sub;
		  unsigned int clock_source, rate_index; int dma_sem;
		  struct ua_dma_ring rec_ring; };
struct ua_device { struct ua_audio audio; struct pci_dev *pdev; int lock;
		   atomic_int shutdown; };
static void log_(struct device *d, const char *fmt, ...) { (void)d; (void)fmt; }
#define dev_info log_
static int ua_locked = 1, writes, ax_zero, prep_fail, slept, gone_in_sleep;
static struct ua_device *cur;
static void mutex_lock(int *l) { assert(!*l); *l = 1; ua_locked = 1; }
static void mutex_unlock(int *l) { assert(*l); *l = 0; ua_locked = 0; }
static void down_read(int *s) { (void)s; }
static void up_read(int *s) { (void)s; }
static void ua_write(struct ua_device *ua, u32 off, u32 v)
{ (void)ua; writes++; if (off == UA_REG_AX_CONTROL && !v) ax_zero++; }
static u32 ua_read(struct ua_device *ua, u32 off) { (void)ua; (void)off; return 0; }
static void ua_dma_ring_sync(struct device *d, struct ua_dma_ring *r,
			     unsigned long off, unsigned long len, bool to)
{ (void)d; (void)r; (void)off; (void)len; (void)to; }
static int ua_audio_prepare_transport(struct ua_device *ua, unsigned int p)
{ (void)ua; (void)p; assert(ua_locked); if (prep_fail) return -EIO; return 0; }
static int ua_audio_start_transport(struct ua_device *ua)
{ ua->audio.transport_running = true; return 0; }
static void msleep(unsigned int ms)
{
	(void)ms;
	/* The wait runs without ua->lock, with pcm_prepare kept off */
	assert(!ua_locked && cur->audio.dma_test_active);
	slept++;
	if (gone_in_sleep)
		atomic_store(&cur->shutdown, 1);
}
'''

MAIN = r'''
int main(void)
{
	static unsigned char ring[UA_DMA_BUF_SIZE];
	struct pci_dev pci = { 0 };
	struct ua_device ua = { .pdev = &pci, .lock = 1 };
	struct ua_dma_test dt = { .delay_ms = 100 };

	cur = &ua;
	ua.audio.rec_buf = ring;
	ua.audio.connected = true;

	/* An open stream: refused, nothing touched */
	ua.audio.play_sub = &ua;
	assert(ua_audio_dma_test(&ua, &dt) == -EBUSY);
	assert(!writes && !ua.audio.dma_test_active);
	ua.audio.play_sub = NULL;
	ua.audio.transport_running = true;
	assert(ua_audio_dma_test(&ua, &dt) == -EBUSY && !writes);
	ua.audio.transport_running = false;

	/* Normal run: lock dropped for the wait, transport stopped after */
	assert(ua_audio_dma_test(&ua, &dt) == 0 && slept == 1);
	assert(ua_locked && !ua.audio.dma_test_active);
	assert(!ua.audio.transport_running && ax_zero == 1);

	/* Transport setup fails: still stopped and released */
	prep_fail = 1;
	assert(ua_audio_dma_test(&ua, &dt) == -EIO && slept == 1);
	assert(!ua.audio.dma_test_active && ax_zero == 2);
	prep_fail = 0;

	/* Removed or failed during the wait */
	gone_in_sleep = 1;
	assert(ua_audio_dma_test(&ua, &dt) == -ENODEV);
	assert(ua_locked && !ua.audio.dma_test_active);
	return 0;
}
'''


def test_dma_test():
    audio = csrc.source('ua_audio.c')
    code = csrc.extract(audio, 'int ua_audio_dma_test(')
    csrc.build_and_run(PREFIX + code + MAIN)


def test_prepare_refuses_while_dma_test_runs():
    audio = csrc.source('ua_audio.c')
    prep = csrc.extract(audio, 'static int ua_pcm_prepare(struct snd_pcm_substream')
    lock = prep.index('mutex_lock(&ua->lock);')
    busy = prep.index('if (audio->dma_test_active) {')
    body = prep.index('ret = ua_pcm_prepare_locked(ua, sub);')
    assert lock < busy < body
    core = csrc.source('ua_core.c')
    ioctl = csrc.extract(core, 'ua_ioctl_dma_test(struct ua_device *ua,')
    assert ioctl.index('mutex_lock(&ua->lock);') < \
        ioctl.index('ua_audio_dma_test(ua, dt);') < \
        ioctl.index('mutex_unlock(&ua->lock);')
