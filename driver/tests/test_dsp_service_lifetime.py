"""ua_dsp_service_start()/ua_dsp_service_stop(): the stop always joins the
work, the start never runs on a removed device.

The handler clears dsp_service_running itself (device gone) and
ua_audio_disconnect_hw() clears it without waiting; a pass can still be
running or queued (the notification IRQ requeues it) when the flag already
says stopped.  The old stop returned early then.  Adapted from the parallel
Codex work (test_dsp_service_lifetime); driver-v2's starts run under
ua->lock, so there is no separate start/stop mutex to test.
"""
import csrc

PREFIX = r'''
#include <assert.h>
#include <stdatomic.h>
#include <stdbool.h>
#define READ_ONCE(x) (x)
#define WRITE_ONCE(x, v) ((x) = (v))
#define atomic_read(p) atomic_load(p)
#define dev_dbg(...) ((void)0)
#define msecs_to_jiffies(x) (x)
#define UA_DSP_SERVICE_INTERVAL_MS 10
struct pci_dev { int dev; };
struct ua_audio { bool dsp_service_running; unsigned int dsp_service_count;
		  int dsp_service_work; };
struct ua_device { struct ua_audio audio; atomic_int shutdown;
		   struct pci_dev *pdev; };
static int schedules, cancels;
static bool schedule_delayed_work(int *w, unsigned long d)
{ (void)w; (void)d; schedules++; return true; }
static bool cancel_delayed_work_sync(int *w) { (void)w; cancels++; return true; }
'''

MAIN = r'''
int main(void)
{
	struct pci_dev pci = { 0 };
	struct ua_device ua = { .pdev = &pci };

	/* Removed or failed device: no loop is started */
	atomic_store(&ua.shutdown, 1);
	ua_dsp_service_start(&ua);
	assert(!schedules && !ua.audio.dsp_service_running);

	atomic_store(&ua.shutdown, 0);
	ua_dsp_service_start(&ua);
	ua_dsp_service_start(&ua);		/* idempotent */
	assert(schedules == 1 && ua.audio.dsp_service_running);

	ua_dsp_service_stop(&ua);
	assert(cancels == 1 && !ua.audio.dsp_service_running);

	/* The flag already says stopped (handler or disconnect_hw cleared
	 * it), a pass may still run or be queued: the stop still joins */
	ua_dsp_service_stop(&ua);
	assert(cancels == 2);

	/* A start after the stop works again on a live device */
	ua_dsp_service_start(&ua);
	assert(schedules == 2 && ua.audio.dsp_service_running);
	return 0;
}
'''


def test_stop_always_joins_and_start_refuses_after_shutdown():
    audio = csrc.source('ua_audio.c')
    funcs = (csrc.extract(audio, 'void ua_dsp_service_start(') + '\n' +
             csrc.extract(audio, 'void ua_dsp_service_stop('))
    csrc.build_and_run(PREFIX + funcs + MAIN)


def test_stop_callers_do_not_hold_the_lock():
    """cancel_delayed_work_sync() under ua->lock would deadlock with the
    handler, which takes it: every caller runs without the lock."""
    audio = csrc.source('ua_audio.c')
    core = csrc.source('ua_core.c')
    callers = {
        'ua_remove': csrc.extract(core, 'static void ua_remove('),
        'ua_error_detected': csrc.extract(
            core, 'static pci_ers_result_t ua_error_detected('),
        'ua_quiesce_device': csrc.extract(
            core, 'static void ua_quiesce_device('),
        'ua_audio_suspend': csrc.extract(audio, 'void ua_audio_suspend('),
        'ua_audio_fini': csrc.extract(audio, 'void ua_audio_fini('),
        'ua_audio_disconnect': csrc.extract(
            audio, 'static void ua_audio_disconnect('),
    }
    for name, body in callers.items():
        stops = [i for i in range(len(body))
                 if body.startswith('ua_dsp_service_stop(', i)]
        assert stops, name
        for at in stops:
            before = body[:at]
            assert before.count('mutex_lock(&ua->lock)') == \
                before.count('mutex_unlock(&ua->lock)'), name
    # Nothing else calls it
    for text in (audio, core, csrc.source('ua_dsp.c')):
        for line in text.splitlines():
            if 'ua_dsp_service_stop(ua)' in line:
                assert line.strip() == 'ua_dsp_service_stop(ua);'
    assert audio.count('ua_dsp_service_stop(ua);') == 3
    assert core.count('ua_dsp_service_stop(ua);') == 4
