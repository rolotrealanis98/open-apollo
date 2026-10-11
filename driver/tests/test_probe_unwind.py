"""A failed probe stops the device the way remove() does before it frees
anything the device may DMA into.

ua_probe_unwind_dsp() is compiled as it is with mocks that record the
order and the device state at every step: nothing is freed while bus
mastering is on, the shutdown flag is up before ua->lock is dropped, and
the service loop and timers are stopped first when the audio side exists.
ua_audio_init() leaves the DMA rings to this unwind.  From the parallel
Codex work (OVERLAP-REPORT section 4, test_remove_dma_containment), adapted
to driver-v2's unwind.
"""
import csrc

PREFIX = r'''
#include <assert.h>
#include <stdatomic.h>
#include <stdbool.h>
#include <string.h>
typedef unsigned int u32;
#define UA_REG_TRANSPORT 0x2220
struct pci_dev { bool master, present; };
struct ua_device { struct pci_dev *pdev; atomic_int shutdown; bool locked;
		   bool audio_stopped; };
static char log_[256];
static void note(const char *s) { strcat(log_, s); strcat(log_, " "); }
#define atomic_set(p, v) atomic_store(p, v)
static void mutex_lock(bool *l) { assert(!*l); *l = true; }
static void mutex_unlock(bool *l) { assert(*l); *l = false; }
#define lock locked
static bool full_shutdown, want_quiet;
static bool pci_device_is_present(struct pci_dev *p) { return p->present; }
static int pci_irq_vector(struct pci_dev *p, int n) { (void)p; return n; }
static struct ua_device *cur;
static void synchronize_irq(int irq)
{ (void)irq; assert(!cur->locked && atomic_load(&cur->shutdown)); note("syncirq"); }
static void msleep(unsigned int ms) { (void)ms; note("sleep"); }
static void pci_clear_master(struct pci_dev *p) { p->master = false; note("nomaster"); }
static void ua_write(struct ua_device *ua, u32 off, u32 v)
{ assert(ua->locked && off == UA_REG_TRANSPORT && !v); note("transport0"); }
static void ua_hw_quiesce_locked(struct ua_device *ua)
{ assert(ua->locked && ua->pdev->master); note("quiesce"); }
static void ua_dsp_service_stop(struct ua_device *ua)
{ assert(!ua->locked); ua->audio_stopped = true; note("svcstop"); }
static void ua_audio_period_wd_cancel(struct ua_device *ua)
{ assert(!ua->locked); note("wdcancel"); }
static void ua_audio_stop_period_timer(struct ua_device *ua)
{ assert(!ua->locked); note("timerstop"); }
static void ua_audio_fini(struct ua_device *ua)
{ assert(!ua->pdev->master && atomic_load(&ua->shutdown)); note("fini"); }
static void ua_dsp_rings_fini(struct ua_device *ua, bool quiet)
{ assert(quiet == want_quiet && !ua->pdev->master); note("rings"); }
static void ua_dsp_free_plugin_payloads(struct ua_device *ua)
{ assert(!ua->pdev->master); note("payloads"); }
static void ua_audio_free_dma(struct ua_device *ua)
{ assert(!ua->pdev->master); note("dma"); }
'''

MAIN = r'''
int main(void)
{
	struct pci_dev pci = { .master = true, .present = true };
	struct ua_device ua = { .pdev = &pci };

	cur = &ua;
	/* Device present, engines not reset: parked buffers are not freed */
	want_quiet = false;
	ua_probe_unwind_dsp(&ua, true);
	assert(!strcmp(log_, "svcstop wdcancel timerstop quiesce transport0 "
			     "syncirq sleep nomaster fini rings payloads dma "));
	assert(atomic_load(&ua.shutdown) && !ua.locked);

	/* Before ua_audio_init(): no audio side to stop or tear down; the
	 * device is gone, so parked buffers can go */
	log_[0] = 0;
	pci.master = true;
	pci.present = false;
	want_quiet = true;
	atomic_store(&ua.shutdown, 0);
	ua.audio_stopped = false;
	ua_probe_unwind_dsp(&ua, false);
	assert(!strcmp(log_, "quiesce transport0 syncirq sleep nomaster rings "
			     "payloads dma "));
	assert(!ua.audio_stopped && atomic_load(&ua.shutdown));

	/* full_shutdown reset the DMA engines in the quiesce: quiet too */
	pci.master = true;
	pci.present = true;
	full_shutdown = true;
	ua_probe_unwind_dsp(&ua, false);
	return 0;
}
'''


def test_unwind_quiesces_before_anything_is_freed():
    core = csrc.source('ua_core.c')
    unwind = csrc.extract(core, 'static void ua_probe_unwind_dsp(')
    csrc.build_and_run(PREFIX + unwind + MAIN)


def test_probe_error_labels_and_audio_init():
    core = csrc.source('ua_core.c')
    probe = csrc.extract(core, 'static int ua_probe(')
    tail = probe[probe.index('err_ida:'):]
    assert tail.index('err_audio:') < tail.index(
        'ua_probe_unwind_dsp(ua, true);') < tail.index('err_dsp:') < \
        tail.index('ua_probe_unwind_dsp(ua, false);') < \
        tail.index('free_irq(')
    assert 'ua_audio_fini(ua);' not in tail
    # ua_audio_init() failing takes the audio-side unwind (its work items
    # and timer exist) and leaves the rings to it (bus mastering is on)
    head = probe[:probe.index('err_ida:')]
    init_call = head.index('ret = ua_audio_init(ua);')
    assert head.index('goto err_audio;', init_call) < \
        head.index('ua_check_dsps(ua, true)', init_call)
    init = csrc.extract(csrc.source('ua_audio.c'), 'int ua_audio_init(')
    assert 'ua_audio_free_dma' not in init
    # everything ua_audio_fini() relies on is set up before the first
    # failure point
    first_fail = init.index('ret = ua_audio_preinit_dma(ua);')
    for setup in ('INIT_DELAYED_WORK(&audio->dsp_service_work',
                  'INIT_DELAYED_WORK(&audio->period_wd',
                  'spin_lock_init(&audio->timer_lock);',
                  'init_rwsem(&audio->dma_sem);', 'hrtimer_'):
        assert init.index(setup) < first_fail, setup
