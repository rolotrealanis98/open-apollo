"""ua_read()/ua_write(): no BAR0 access once the shutdown flag is up.

driver-v2 keeps the lock-free flag test and joins every BAR0 user before
remove() returns (ioctl_sem, the service and timer stops, free_irq,
snd_card_free); BAR0 stays mapped (pcim) until then.  Codex's version put
every access under a new io_lock spinlock to drain accesses at the flag
change; that lock is not taken here (a stalled read on a dead link would
spin other CPUs with interrupts off), so this checks the flag behaviour
only.  Rewritten from the parallel Codex work (test_mmio_shutdown_gate).
"""
import csrc

PREFIX = r'''
#include <assert.h>
#include <stdatomic.h>
#include <stdint.h>
typedef uint32_t u32;
#define unlikely(x) (x)
#define atomic_read(p) atomic_load(p)
struct ua_device { atomic_int shutdown; unsigned char *regs; };
static int reads, writes;
static u32 ioread32(void *p) { (void)p; reads++; return 0x1234; }
static void iowrite32(u32 v, void *p) { (void)v; (void)p; writes++; }
'''

MAIN = r'''
int main(void)
{
	unsigned char bar[0x100];
	struct ua_device ua = { .regs = bar };

	assert(ua_read(&ua, 0x10) == 0x1234 && reads == 1);
	ua_write(&ua, 0x10, 1);
	assert(writes == 1);

	atomic_store(&ua.shutdown, 1);
	assert(ua_read(&ua, 0x10) == 0xFFFFFFFF && reads == 1);
	ua_write(&ua, 0x10, 1);
	assert(writes == 1);
	return 0;
}
'''


def test_flag_blocks_bar0():
    hdr = csrc.source('ua_apollo.h')
    helpers = (csrc.extract(hdr, 'static inline u32 ua_read(') + '\n' +
               csrc.extract(hdr, 'static inline void ua_write('))
    csrc.build_and_run(PREFIX + helpers + MAIN)
