"""/dev/ua_apolloN lifetime: an ioctl in flight holds off remove(), a later
one gets -ENODEV, raw ioctls need CAP_SYS_RAWIO, and remove() tears down
in the order that keeps the device from DMAing into freed memory.

driver-v2's design (not Codex's ua_chardev/gate): the chardev owns struct
ua_device and frees it on the last reference (ua_chardev_release), ioctls
run under ioctl_sem (read), remove() takes it for writing and sets
`removed`.  ua_ioctl() is compiled as it is with a pthread rwlock.
Rewritten from the parallel Codex work (test_chardev_lifetime,
test_remove_dma_containment) for driver-v2's code.
"""
import csrc

PREFIX = r'''
#include <assert.h>
#include <errno.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdbool.h>
#include <unistd.h>
#define CAP_SYS_RAWIO 17
#define atomic_read(p) atomic_load(p)
#define down_read(s) pthread_rwlock_rdlock(s)
#define up_read(s) pthread_rwlock_unlock(s)
struct ua_device { pthread_rwlock_t ioctl_sem; bool removed;
		   atomic_int shutdown; };
struct ua_file { struct ua_device *ua; };
struct file { void *private_data; };
static bool cap;
static atomic_int entered, release_, dispatched;
static bool capable(int c) { assert(c == CAP_SYS_RAWIO); return cap; }
static bool ua_ioctl_needs_rawio(unsigned int cmd) { return cmd == 2; }
static long ua_ioctl_dispatch(struct ua_device *ua, struct ua_file *uf,
			      unsigned int cmd, unsigned long arg)
{
	(void)ua; (void)cmd; (void)arg;
	assert(uf && uf->ua == ua);	/* the open file reaches the dispatch */
	atomic_fetch_add(&dispatched, 1);
	atomic_store(&entered, 1);
	while (!atomic_load(&release_))
		usleep(1000);
	return 42;
}
'''

MAIN = r'''
static struct ua_device ua;
static struct ua_file uf = { &ua };
static struct file f = { &uf };
static atomic_int removed_done;

static void *in_flight(void *p) { (void)p; assert(ua_ioctl(&f, 1, 0) == 42); return NULL; }
static void *remove_step0(void *p)
{
	(void)p;
	pthread_rwlock_wrlock(&ua.ioctl_sem);	/* remove() step 0 */
	ua.removed = true;
	pthread_rwlock_unlock(&ua.ioctl_sem);
	atomic_store(&removed_done, 1);
	return NULL;
}

int main(void)
{
	pthread_t a, b;

	pthread_rwlock_init(&ua.ioctl_sem, NULL);

	/* Raw ioctl without the capability: refused before anything */
	assert(ua_ioctl(&f, 2, 0) == -EPERM && !atomic_load(&dispatched));

	/* remove() waits for the ioctl in flight ... */
	pthread_create(&a, NULL, in_flight, NULL);
	while (!atomic_load(&entered))
		usleep(1000);
	pthread_create(&b, NULL, remove_step0, NULL);
	usleep(20000);
	assert(!atomic_load(&removed_done));
	atomic_store(&release_, 1);
	pthread_join(a, NULL);
	pthread_join(b, NULL);
	assert(atomic_load(&removed_done));

	/* ... and none runs after it */
	assert(ua_ioctl(&f, 1, 0) == -ENODEV && atomic_load(&dispatched) == 1);
	cap = true;
	assert(ua_ioctl(&f, 2, 0) == -ENODEV && atomic_load(&dispatched) == 1);

	/* The shutdown flag alone (AER) refuses too */
	ua.removed = false;
	atomic_store(&ua.shutdown, 1);
	assert(ua_ioctl(&f, 1, 0) == -ENODEV && atomic_load(&dispatched) == 1);
	return 0;
}
'''


def test_ioctl_against_remove():
    core = csrc.source('ua_core.c')
    code = csrc.extract(core, 'static long ua_ioctl(struct file *filp')
    csrc.build_and_run(PREFIX + code + MAIN, threads=True)


def test_remove_order():
    """remove(): ioctls refused, software stopped, kext quiesce and the
    shutdown flag under ua->lock, bus mastering off, IRQ freed, then the
    frees, the chardev last (an open fd keeps the struct)."""
    core = csrc.source('ua_core.c')
    rm = csrc.extract(core, 'static void ua_remove(')
    steps = [
        'down_write(&ua->ioctl_sem);',
        'ua->removed = true;',
        'up_write(&ua->ioctl_sem);',
        'cancel_work_sync(&ua->resume_work);',
        'ua_dsp_service_stop(ua);',
        'ua_audio_stop_period_timer(ua);',
        'mutex_lock(&ua->lock);',
        'ua_hw_quiesce_locked(ua);',
        'atomic_set(&ua->shutdown, 1);',
        'mutex_unlock(&ua->lock);',
        'pci_clear_master(pdev);',
        'free_irq(',
        'ua_audio_fini(ua);',
        'ua_dsp_rings_fini(ua,',
        'ua_dsp_free_plugin_payloads(ua);',
        'cdev_device_del(',
        'put_device(&ua->chardev);',
    ]
    at = [rm.index(s) for s in steps]
    assert at == sorted(at), [s for s, i in zip(steps, at)]


def test_struct_owned_by_the_chardev():
    core = csrc.source('ua_core.c')
    probe = csrc.extract(core, 'static int ua_probe(')
    assert 'ua = kzalloc(sizeof(*ua), GFP_KERNEL);' in probe
    assert 'devm_kzalloc' not in probe
    assert 'ua->chardev.release = ua_chardev_release;' in probe
    # The node appears last, after the audio side exists
    assert probe.index('ua_audio_init(ua)') < \
        probe.index('cdev_device_add(&ua->cdev, &ua->chardev)')
    release = csrc.extract(core, 'static void ua_chardev_release(')
    assert 'kfree(ua);' in release
    # The ALSA card is freed synchronously: PCM and control files closed
    fini = csrc.extract(csrc.source('ua_audio.c'), 'void ua_audio_fini(')
    assert 'snd_card_free(audio->card);' in fini
    assert 'snd_card_free_when_closed(' not in \
        fini.replace('snd_card_free_when_closed() returned', '')
    assert fini.index('snd_card_free(audio->card);') < \
        fini.index('ua_audio_free_dma(ua);')
