"""The period hrtimer cannot be armed after the teardown stopped it, and
every teardown path stops it.

trigger(START) checks the shutdown flag, then takes timer_lock and arms the
timer.  A START that passed the check just before remove() raised the flag
used to arm the timer after remove() had cancelled it; the timer then ran
on after the struct could be freed.  ua_pcm_trigger() and
ua_audio_stop_period_timer() are compiled as they are, with a START held
between its first check and timer_lock while the removal runs.  Adapted
from the parallel Codex work (test_period_timer_lifetime).
"""
import csrc

PREFIX = r'''
#include <assert.h>
#include <errno.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdbool.h>
#include <unistd.h>
typedef unsigned long snd_pcm_uframes_t;
#define SNDRV_PCM_STREAM_PLAYBACK 0
#define SNDRV_PCM_STREAM_CAPTURE 1
#define SNDRV_PCM_TRIGGER_STOP 0
#define SNDRV_PCM_TRIGGER_START 1
#define SNDRV_PCM_TRIGGER_SUSPEND 5
#define SNDRV_PCM_TRIGGER_RESUME 6
#define UA_DIR_REC 0
#define UA_DIR_PLAY 1
#define UA_PERIOD_WD_MS 10
#define HRTIMER_MODE_REL_SOFT 0
#define ms_to_ktime(x) (x)
#define msecs_to_jiffies(x) (x)
#define READ_ONCE(x) (x)
#define WRITE_ONCE(x, v) ((x) = (v))
#define __set_bit(n, p) (*(p) |= 1UL << (n))
#define __clear_bit(n, p) (*(p) &= ~(1UL << (n)))
#define spin_lock_irqsave(l, f) do { (f) = 0; pthread_mutex_lock(l); } while (0)
#define spin_unlock_irqrestore(l, f) do { (void)(f); pthread_mutex_unlock(l); } while (0)
static unsigned long jiffies;
static int *system_highpri_wq;
static bool pcm_live_start = true;
struct snd_pcm_status { snd_pcm_uframes_t hw_ptr; };
struct snd_pcm_runtime { snd_pcm_uframes_t buffer_size;
			 struct snd_pcm_status *status; };
struct ua_audio {
	pthread_mutex_t timer_lock;
	bool period_timer_running;
	int period_timer, period_wd;
	unsigned long running_subs, irq_subs, timer_irq_jiffies, wd_sp_jiffies;
	unsigned int transport_gen, irq_gen[2];
	snd_pcm_uframes_t last_play_period_pos, last_rec_period_pos;
};
struct ua_device { struct ua_audio audio; atomic_int shutdown; };
struct snd_pcm_substream { struct ua_device *ua; int stream;
			   struct snd_pcm_runtime *runtime; };
#define snd_pcm_substream_chip(s) ((s)->ua)

/* The first shutdown read of the racing START blocks until released */
static atomic_int reads, first_checked, permit;
static int shutdown_read(atomic_int *p)
{
	int v = atomic_load(p);

	if (atomic_fetch_add(&reads, 1) == 0) {
		atomic_store(&first_checked, 1);
		while (!atomic_load(&permit))
			usleep(1000);
	}
	return v;
}
#define atomic_read(p) shutdown_read(p)

static int arms, cancels, try_cancels;
static void hrtimer_start(int *t, int when, int mode)
{ (void)when; (void)mode; *t = 1; arms++; }
static void hrtimer_cancel(int *t) { *t = 0; cancels++; }
static void hrtimer_try_to_cancel(int *t) { *t = 0; try_cancels++; }
static bool ua_period_irq_usable(struct ua_device *ua,
				 struct snd_pcm_substream *s)
{ (void)ua; (void)s; return false; }
static void ua_period_timer_rearm(struct ua_device *ua) { (void)ua; }
static void queue_delayed_work(int *wq, int *w, unsigned long d)
{ (void)wq; (void)w; (void)d; }
'''

MAIN = r'''
static struct snd_pcm_substream play, rec;
static void *late_start(void *p)
{
	assert(ua_pcm_trigger(p, SNDRV_PCM_TRIGGER_START) == -ENODEV);
	return NULL;
}

int main(void)
{
	struct ua_device ua = { 0 };
	struct snd_pcm_status st = { .hw_ptr = 100 };
	struct snd_pcm_runtime rt = { .buffer_size = 8192, .status = &st };
	pthread_t t;

	pthread_mutex_init(&ua.audio.timer_lock, NULL);
	play = (struct snd_pcm_substream){ &ua, SNDRV_PCM_STREAM_PLAYBACK, &rt };
	rec = (struct snd_pcm_substream){ &ua, SNDRV_PCM_STREAM_CAPTURE, &rt };

	/* START reads shutdown == 0, then waits before timer_lock ... */
	pthread_create(&t, NULL, late_start, &play);
	while (!atomic_load(&first_checked))
		usleep(1000);
	/* ... while remove() raises the flag and stops the timer */
	atomic_store(&ua.shutdown, 1);
	ua_audio_stop_period_timer(&ua);
	assert(cancels == 1);
	atomic_store(&permit, 1);
	pthread_join(t, NULL);
	assert(arms == 0 && !ua.audio.period_timer &&
	       !ua.audio.period_timer_running && !ua.audio.running_subs);

	/* A normal START arms, the other direction's STOP keeps it */
	atomic_store(&ua.shutdown, 0);
	assert(ua_pcm_trigger(&play, SNDRV_PCM_TRIGGER_START) == 0);
	assert(arms == 1 && ua.audio.period_timer_running);
	assert(ua.audio.last_play_period_pos == 100);	/* pcm_live_start */
	assert(ua_pcm_trigger(&rec, SNDRV_PCM_TRIGGER_START) == 0);
	assert(ua_pcm_trigger(&play, SNDRV_PCM_TRIGGER_STOP) == 0);
	assert(ua.audio.period_timer_running && ua.audio.period_timer);
	assert(ua_pcm_trigger(&rec, SNDRV_PCM_TRIGGER_STOP) == 0);
	assert(!ua.audio.period_timer_running && !ua.audio.period_timer);

	/* The XRUN stop after a removal still clears the bits */
	assert(ua_pcm_trigger(&rec, SNDRV_PCM_TRIGGER_START) == 0);
	atomic_store(&ua.shutdown, 1);
	assert(ua_pcm_trigger(&rec, SNDRV_PCM_TRIGGER_STOP) == 0);
	assert(!ua.audio.running_subs && !ua.audio.period_timer_running);
	ua_audio_stop_period_timer(&ua);
	assert(!ua.audio.period_timer);
	return 0;
}
'''


def test_start_cannot_arm_after_the_teardown_stop():
    audio = csrc.source('ua_audio.c')
    code = (csrc.extract(audio, 'void ua_audio_stop_period_timer(') +
            '\n' + csrc.extract(audio, 'static int ua_pcm_trigger('))
    csrc.build_and_run(PREFIX + code + MAIN, threads=True)


def test_teardown_paths_stop_the_timer():
    """remove (before and after the shutdown flag), the quiesce, suspend
    and ua_audio_fini() all stop the timer, the watchdog first."""
    audio = csrc.source('ua_audio.c')
    core = csrc.source('ua_core.c')
    remove = csrc.extract(core, 'static void ua_remove(')
    assert remove.count('ua_audio_stop_period_timer(ua);') == 2
    second = remove.rindex('ua_audio_stop_period_timer(ua);')
    assert remove.index('atomic_set(&ua->shutdown, 1);') < second
    for body in (remove,
                 csrc.extract(core, 'static void ua_quiesce_device('),
                 csrc.extract(audio, 'void ua_audio_suspend('),
                 csrc.extract(audio, 'void ua_audio_fini(')):
        stop = body.index('ua_audio_stop_period_timer(ua);')
        assert body.index('ua_audio_period_wd_cancel(ua);') < stop
    fini = csrc.extract(audio, 'void ua_audio_fini(')
    # After the card is gone: no trigger can arm it again
    assert fini.index('snd_card_free(audio->card);') < \
        fini.index('ua_audio_stop_period_timer(ua);')
