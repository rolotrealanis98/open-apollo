"""The Hi-band dwell (ua_audio_clock_written(), ua_audio_hi_dwell()).

Leaving 176.4/192 kHz less than about 3 s after the clock was written
there wedged the x4's firmware rate (titan 2026-10-11), so a clock write
that leaves a Hi-band rate waits until the clock has been there for
hi_dwell_ms.  Both functions are compiled as they are, with jiffies in
milliseconds and msleep() advancing it.
"""
import csrc

PREFIX = r'''
#include <assert.h>
#include <stdbool.h>
#include <stdint.h>
typedef uint32_t u32;
#define UA_RATE_44100 1
#define UA_RATE_48000 2
#define UA_RATE_88200 3
#define UA_RATE_96000 4
#define UA_RATE_176400 5
#define UA_RATE_192000 6
static unsigned long jiffies = 100000;
#define msecs_to_jiffies(ms) ((unsigned long)(ms))
#define jiffies_to_msecs(j) ((unsigned int)(j))
#define time_before(a, b) ((long)((a) - (b)) < 0)
static unsigned int hi_dwell_ms = 3000;
static int infos, steps, offline_after = -1;
static void log_(int *d, const char *fmt, ...) { (void)d; (void)fmt; infos++; }
#define dev_info log_
struct pci_dev { int dev; };
struct ua_audio { unsigned int hi_clock_idx; unsigned long hi_clock_since; };
struct ua_device { struct ua_audio audio; struct pci_dev *pdev; };
static unsigned int ua_rate_hz(unsigned int idx) { return idx * 1000; }
static void msleep(unsigned int ms) { jiffies += ms; }
static u32 ua_audio_service_step(struct ua_device *ua)
{
	(void)ua;
	return ++steps == offline_after ? 0xFFFFFFFF : 0;
}
'''

MAIN = r'''
int main(void)
{
	struct pci_dev pci = { 0 };
	struct ua_device ua = { .pdev = &pci };
	unsigned long t0;

	/* Lo/Mid rates never wait */
	ua_audio_clock_written(&ua, UA_RATE_48000);
	t0 = jiffies;
	ua_audio_hi_dwell(&ua, UA_RATE_96000);
	assert(jiffies == t0 && infos == 0);

	/* 192 kHz written, left 1 s later: waits the other 2 s */
	ua_audio_clock_written(&ua, UA_RATE_192000);
	t0 = jiffies;
	jiffies += 1000;
	ua_audio_hi_dwell(&ua, UA_RATE_48000);
	assert(jiffies - t0 >= 3000 && jiffies - t0 < 3025 && infos == 1);

	/* Rewriting the same Hi-band rate keeps the first time */
	ua_audio_clock_written(&ua, UA_RATE_48000);
	ua_audio_clock_written(&ua, UA_RATE_192000);
	t0 = jiffies;
	ua_audio_clock_written(&ua, UA_RATE_192000);
	jiffies += 2000;
	ua_audio_clock_written(&ua, UA_RATE_192000);
	ua_audio_hi_dwell(&ua, UA_RATE_44100);
	assert(jiffies - t0 >= 3000 && jiffies - t0 < 3025);

	/* Same rate again (a restart, a source change): no wait */
	ua.audio.hi_clock_since = jiffies;
	t0 = jiffies;
	ua_audio_hi_dwell(&ua, UA_RATE_192000);
	assert(jiffies == t0);

	/* 192 -> 176.4 kHz waits too; after the dwell nothing waits */
	t0 = jiffies;
	ua_audio_hi_dwell(&ua, UA_RATE_176400);
	assert(jiffies - t0 >= 3000);
	t0 = jiffies;
	ua_audio_hi_dwell(&ua, UA_RATE_48000);
	assert(jiffies == t0);

	/* Leaving the Hi band clears the state */
	ua_audio_clock_written(&ua, UA_RATE_48000);
	assert(ua.audio.hi_clock_idx == 0);

	/* hi_dwell_ms=0 turns it off */
	ua_audio_clock_written(&ua, UA_RATE_176400);
	hi_dwell_ms = 0;
	t0 = jiffies;
	ua_audio_hi_dwell(&ua, UA_RATE_48000);
	assert(jiffies == t0);
	hi_dwell_ms = 3000;

	/* The device goes away mid-wait: stop at once */
	ua.audio.hi_clock_since = jiffies;
	steps = 0;
	offline_after = 3;
	ua_audio_hi_dwell(&ua, UA_RATE_48000);
	assert(steps == 3);
	return 0;
}
'''


def test_hi_dwell():
    audio = csrc.source('ua_audio.c')
    code = '\n'.join((
        csrc.extract(audio, 'static void ua_audio_clock_written('),
        csrc.extract(audio, 'static void ua_audio_hi_dwell('),
    ))
    csrc.build_and_run(PREFIX + code + MAIN)


def test_every_clock_write_is_tracked():
    """Each write of the clock config goes through ua_audio_clock_written()
    (the DMA_TEST ioctl rewrites the current rate and does not count)."""
    audio = csrc.source('ua_audio.c')
    change = csrc.extract(audio, 'static int ua_audio_clock_change_locked(')
    assert change.index('ua_audio_hi_dwell(') < change.index(
        'UA_REG_NOTIF_CLOCK_CFG') < change.index('ua_audio_clock_written(')
    post = csrc.extract(audio, 'static void ua_audio_post_start_clock(')
    assert 'ua_audio_clock_written(ua, audio->rate_index)' in post
    prepare = csrc.extract(audio, 'static void ua_audio_hi_from_hi(')
    assert 'ua_audio_hi_dwell(ua, UA_RATE_48000)' in prepare
