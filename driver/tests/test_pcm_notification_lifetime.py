"""Period notifications never use a substream after pcm_close() and the
transport stops only when both directions are closed.

driver-v2 holds audio->lock for the whole period check (the hrtimer and
the IRQ thread), and pcm_close() clears the substream pointer under the
same lock before ALSA frees the runtime, so a callback either finished
with the substream or never sees it.  Codex's version counted readers and
drained them (ua_audio_notify_get/put/drain, .sync_stop); that machinery
is not needed here.  This checks driver-v2's structure and compiles
ua_pcm_close() as it is.  Rewritten from the parallel Codex work
(test_pcm_notification_lifetime, test_pcm_transaction_serialization).
"""
import csrc

PREFIX = r'''
#include <assert.h>
#include <stdbool.h>
#include <stddef.h>
#define SNDRV_PCM_STREAM_PLAYBACK 0
#define SNDRV_PCM_STREAM_CAPTURE 1
#define dev_dbg(...) ((void)0)
static int audio_locked, ua_locked, stops, stop_under_lock;
#define spin_lock_irqsave(l, f) do { (f) = 0; assert(!audio_locked); audio_locked = 1; } while (0)
#define spin_unlock_irqrestore(l, f) do { (void)(f); audio_locked = 0; } while (0)
struct snd_pcm_substream;
struct ua_audio { int lock; struct snd_pcm_substream *play_sub, *rec_sub;
		  bool transport_running; };
struct pci_dev { int dev; };
struct ua_device { struct ua_audio audio; int lock; struct pci_dev *pdev; };
struct snd_pcm_substream { struct ua_device *ua; int stream; };
#define snd_pcm_substream_chip(s) ((s)->ua)
static void mutex_lock(int *l) { assert(!*l); *l = 1; ua_locked = 1; }
static void mutex_unlock(int *l) { assert(*l); *l = 0; ua_locked = 0; }
static void ua_audio_stop_transport(struct ua_device *ua)
{ stops++; stop_under_lock = ua_locked; ua->audio.transport_running = false; }
'''

MAIN = r'''
int main(void)
{
	struct ua_device ua = { 0 };
	struct snd_pcm_substream play = { &ua, SNDRV_PCM_STREAM_PLAYBACK };
	struct snd_pcm_substream rec = { &ua, SNDRV_PCM_STREAM_CAPTURE };

	ua.audio.play_sub = &play;
	ua.audio.rec_sub = &rec;
	ua.audio.transport_running = true;

	/* One direction closes: its pointer goes, the transport stays */
	assert(ua_pcm_close(&play) == 0);
	assert(!ua.audio.play_sub && ua.audio.rec_sub == &rec);
	assert(!stops && ua.audio.transport_running);

	/* The last one closes: transport stopped, under ua->lock */
	assert(ua_pcm_close(&rec) == 0);
	assert(!ua.audio.rec_sub && stops == 1 && stop_under_lock);
	assert(!audio_locked && !ua_locked);
	return 0;
}
'''


def test_close():
    audio = csrc.source('ua_audio.c')
    code = csrc.extract(audio, 'static int ua_pcm_close(')
    csrc.build_and_run(PREFIX + code + MAIN)


def _held_across(body, first, last):
    lock = body.index('spin_lock_irqsave(&audio->lock, flags);')
    unlock = body.index('spin_unlock_irqrestore(&audio->lock, flags);', lock)
    return lock < body.index(first) < body.index(last) < unlock


def test_callbacks_hold_audio_lock_across_the_check():
    audio = csrc.source('ua_audio.c')
    timer = csrc.extract(audio, 'static enum hrtimer_restart '
                                'ua_period_timer_callback(')
    # (the link-down path above takes it for its own XRUN stop first)
    tail = timer[timer.index('rec = audio->rec_sub;') - 200:]
    assert _held_across(tail, 'ua_period_check(audio, play',
                        'ua_period_check(audio, rec')
    thread = csrc.extract(audio, 'void ua_audio_irq_thread(')
    assert _held_across(thread, 'ua_period_check(audio, play',
                        'ua_period_check(audio, rec')
    close = csrc.extract(audio, 'static int ua_pcm_close(')
    assert _held_across(close, 'audio->play_sub = NULL;',
                        'audio->rec_sub = NULL;')
    # The check pins the runtime with the stream lock and skips a
    # substream that is open but not configured
    check = csrc.extract(audio, 'static void ua_period_check(')
    assert 'snd_pcm_stream_lock(sub);' in check
    assert '!rt->period_size || !rt->buffer_size' in check
    # The hard IRQ never wakes a stream itself
    hard = csrc.extract(audio, 'bool ua_audio_irq(')
    assert 'snd_pcm_period_elapsed' not in hard.replace(
        'No snd_pcm_period_elapsed() here', '')


def test_rate_change_reconnects_in_the_same_prepare():
    """The rate-change disconnect is followed by the connect block in the
    same prepare (Codex's test_pcm_rate_reconnect; driver-v2 fixed the
    else-if that skipped it)."""
    audio = csrc.source('ua_audio.c')
    body = csrc.extract(audio, 'ua_pcm_prepare_locked(struct ua_device *ua,')
    rate = body.index('if (sub->runtime->rate != audio->sample_rate) {')
    disc = body.index('ua_audio_disconnect_hw(ua);', rate)
    conn = body.index('if (!audio->connected) {', disc)
    assert 'else if (!audio->connected)' not in body
    assert body.index('ret = ua_audio_connect(ua);', conn) > conn
    assert body.index('ua_dsp_service_start(ua);', conn) > conn
