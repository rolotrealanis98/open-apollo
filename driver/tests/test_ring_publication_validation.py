"""Raw ring sends: RING_SEND takes inline entries only, and SRAM_DATA /
SEND_RAW report a command the ring never consumed.

ua_dsp_ring_send_raw(), ua_dsp_send_sram_data() and ua_dsp_send_raw_dma()
are compiled as they are; the ring helpers are mocks that record entries,
doorbells and what happens to the DMA buffer.  Adapted from the parallel
Codex work (test_ring_publication_validation, test_raw_dma_retention):
driver-v2 parks a buffer the ring may still read (orphans) instead of
keeping every published buffer until teardown, and keeps using the ring.
"""
import csrc

PREFIX = r'''
#include <assert.h>
#include <errno.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
typedef uint32_t u32;
typedef uint64_t u64;
typedef uint64_t dma_addr_t;
#define GFP_KERNEL 0
#define BIT(n) (1UL << (n))
#define UA_RING_DMA_REF BIT(31)
#define UA_RING_CMD_OFFSET 0
#define UA_PCIE_PAGE_SIZE 4096
#define UA_DSP_CMDBUF_WAIT_MS 6000
#define cpu_to_le32(x) (x)
#define lower_32_bits(x) ((u32)(x))
#define upper_32_bits(x) ((u32)((u64)(x) >> 32))
struct ua_ring_entry { u32 word0, word1, word2, word3; };
struct ua_dsp_ring { unsigned int write_ptr, hw_pos; };
struct ua_dsp_state { struct ua_dsp_ring cmd; bool rings_allocated; };
struct pci_dev { int dev; };
struct ua_device { struct pci_dev *pdev; unsigned int num_dsps;
		   struct ua_dsp_state dsp[4]; };
static void log_(int *d, const char *fmt, ...) { (void)d; (void)fmt; }
#define dev_warn log_
static int puts_, bells, parked, freed, idle = 1;
static struct ua_ring_entry last;
static u32 ua_dsp_base(unsigned int i) { return i * 0x80; }
static int ua_dsp_orphan_room(struct ua_device *ua) { (void)ua; return 0; }
static int ua_dsp_ring_wait_space(struct ua_device *ua, struct ua_dsp_ring *r,
				  unsigned int n, unsigned int t)
{ (void)ua; (void)r; (void)n; (void)t; return 0; }
static int ua_dsp_ring_put_entry(struct ua_device *ua, struct ua_dsp_ring *r,
				 const struct ua_ring_entry *e)
{ (void)ua; last = *e; puts_++; r->write_ptr = (r->write_ptr + 1) & 1023; return 0; }
static void ua_dsp_ring_doorbell(struct ua_device *ua, struct ua_dsp_ring *r, u32 b)
{ (void)ua; (void)r; (void)b; bells++; }
static bool ua_dsp_ring_wait_idle(struct ua_device *ua, struct ua_dsp_ring *r,
				  unsigned int t)
{ (void)ua; (void)r; assert(t == UA_DSP_CMDBUF_WAIT_MS); return idle; }
static void *dma_alloc_coherent(int *d, size_t n, dma_addr_t *dma, int f)
{ (void)d; (void)f; *dma = 0x2000; return malloc(n); }
static void dma_free_coherent(int *d, size_t n, void *p, dma_addr_t dma)
{ (void)d; (void)n; (void)dma; free(p); freed++; }
static void ua_dsp_orphan_dma(struct ua_device *ua, void *cpu, dma_addr_t dma,
			      size_t size, struct ua_dsp_ring *r0,
			      struct ua_dsp_ring *r1)
{ (void)ua; (void)dma; (void)size; (void)r0; (void)r1; free(cpu); parked++; }
'''

MAIN = r'''
int main(void)
{
	struct pci_dev pci = { 0 };
	struct ua_device ua = { .pdev = &pci, .num_dsps = 4 };
	u32 data[4] = { 1, 2, 3, 4 };

	ua.dsp[0].rings_allocated = true;

	/* Inline entries go out as given */
	assert(ua_dsp_ring_send_raw(&ua, 0, 0x001E0004, 10, 8, 0x20) == 0);
	assert(puts_ == 1 && bells == 1 && last.word0 == 0x001E0004);
	/* A DMA reference from user space is refused before the ring */
	assert(ua_dsp_ring_send_raw(&ua, 0, 0x80000002, 0, 0x1000, 0) == -EINVAL);
	assert(ua_dsp_ring_send_raw(&ua, 0, 0xffffffff, 0, 0, 0) == -EINVAL);
	assert(ua_dsp_ring_send_raw(&ua, 4, 0x001A0002, 48000, 0, 0) == -EINVAL);
	assert(puts_ == 1 && bells == 1);

	/* Consumed: the buffer is freed, success */
	assert(ua_dsp_send_sram_data(&ua, 0, 0x9d000, data, sizeof(data)) == 0);
	assert(freed == 1 && !parked);
	assert(ua_dsp_send_raw_dma(&ua, 0, data, sizeof(data)) == 0);
	assert(freed == 2 && !parked);

	/* Not consumed in 6 s: the buffer is parked, the caller hears it */
	idle = 0;
	assert(ua_dsp_send_sram_data(&ua, 0, 0x9d000, data, sizeof(data)) ==
	       -ETIMEDOUT);
	assert(parked == 1 && freed == 2);
	assert(ua_dsp_send_raw_dma(&ua, 0, data, sizeof(data)) == -ETIMEDOUT);
	assert(parked == 2 && freed == 2);
	return 0;
}
'''


def test_raw_ring_sends():
    dsp = csrc.source('ua_dsp.c')
    code = '\n'.join(csrc.extract(dsp, sig) for sig in (
        'int ua_dsp_ring_send_raw(',
        'int ua_dsp_send_sram_data(',
        'int ua_dsp_send_raw_dma(',
    ))
    csrc.build_and_run(PREFIX + code + MAIN)


def test_internal_callers_send_inline_entries():
    """Every driver caller of ua_dsp_ring_send_raw() passes an inline
    command constant (bit 31 clear)."""
    dsp = csrc.source('ua_dsp.c')
    import re
    words = re.findall(r'ua_dsp_ring_send_raw\(ua, [^,]+,\s*([A-Z0-9_x]+)',
                       dsp)
    assert words
    consts = {
        'UA_DSP_CMD_BUS_ENABLE': 0x001C0003,
        'UA_DSP_CMD_INPUT_PARAM': 0x001E0004,
        'UA_DSP_CMD_CONNECT': 0x00230002,
        'UA_DSP_CMD_FINALIZE': 0x00100002,
    }
    for w in words:
        value = int(w, 16) if w.startswith('0x') else consts[w]
        assert not value & 0x80000000, w
