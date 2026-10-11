"""ua_dsp_send_block(): queue order, response decoding, and what happens
to the three DMA buffers on success, timeout and a failed queueing.

Compiled as it is with its response decoder (ua_dsp_block_result(),
ua_kext_block_status()); the ring calls are mocks and a fake DSP fills the
response buffer when the command ring is doorbelled.  driver-v2's contract:
a buffer the rings may still reference is parked (orphans) and freed once
the ring is past it; a response decodes into the kext's codes, fatal only
with block_strict=1.  Codex's version kept every published buffer until
teardown and poisoned the ring on any failure; that is not tested here.
Rewritten from the parallel Codex work (test_send_block_staging).
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
typedef uint32_t __le32;
typedef uint64_t dma_addr_t;
#define GFP_KERNEL 0
#define BIT(n) (1UL << (n))
#define UA_RING_DMA_REF BIT(31)
#define UA_RING_CMD_OFFSET 0
#define UA_RING_RESP_OFFSET 0x40
#define UA_RING_POSITION 0x28
#define UA_PCIE_PAGE_SIZE 4096
#define UA_FW_LARGE_THRESHOLD 0x3FFFC
#define UA_FW_MAX_SIZE (4 * 1024 * 1024)
#define UA_BLOCK_TIMEOUT_MS 5000
#define SEND_BLOCK_RESP_DWORDS 4
#define ALIGN(x, a) (((x) + (a) - 1) & ~((size_t)(a) - 1))
#define DIV_ROUND_UP(n, d) (((n) + (d) - 1) / (d))
#define min_t(t, a, b) ((t)(a) < (t)(b) ? (t)(a) : (t)(b))
#define cpu_to_le32(x) (x)
#define le32_to_cpu(x) (x)
#define READ_ONCE(x) (*(volatile typeof(x) *)&(x))
#define lower_32_bits(x) ((u32)(x))
#define upper_32_bits(x) ((u32)((u64)(x) >> 32))
#define msecs_to_jiffies(x) (x)
#define time_after(a, b) ((long)((b) - (a)) < 0)
static unsigned long jiffies;
static bool block_strict;
static unsigned int sendblock_chunk = 1024;
static void log_(int *d, const char *fmt, ...) { (void)d; (void)fmt; }
#define dev_warn log_
#define dev_dbg log_
struct ua_ring_entry { u32 word0, word1, word2, word3; };
struct ua_dsp_ring { unsigned int write_ptr, hw_pos, capacity; bool live;
		     int id; };
struct ua_dsp_state { struct ua_dsp_ring cmd, resp; bool rings_allocated; };
struct pci_dev { int dev; };
struct ua_device { struct pci_dev *pdev; unsigned int num_dsps;
		   struct ua_dsp_state dsp[4]; };

/* DMA: the bus address is the CPU pointer, so the fake DSP can write */
static int allocs, frees, parked, kerrs;
static void *dma_alloc_coherent(int *d, size_t n, dma_addr_t *dma, int f)
{ void *p = calloc(1, n); (void)d; (void)f; *dma = (dma_addr_t)(uintptr_t)p; allocs++; return p; }
static void dma_free_coherent(int *d, size_t n, void *p, dma_addr_t dma)
{ (void)d; (void)n; (void)dma; free(p); frees++; }
static void ua_dsp_orphan_dma(struct ua_device *ua, void *cpu, dma_addr_t dma,
			      size_t size, struct ua_dsp_ring *r0,
			      struct ua_dsp_ring *r1)
{ (void)ua; (void)dma; (void)size; (void)r0; (void)r1; if (cpu) { free(cpu); parked++; } }
static void ua_err_block_send(struct ua_device *ua, int kerr)
{ (void)ua; (void)kerr; kerrs++; }
static int ua_dsp_orphan_room(struct ua_device *ua) { (void)ua; return 0; }
static int ua_dsp_ring_wait_space(struct ua_device *ua, struct ua_dsp_ring *r,
				  unsigned int n, unsigned int t)
{ (void)ua; (void)r; (void)t; return n >= 1024 ? -E2BIG : 0; }
static int ua_dsp_ring_update_pos(struct ua_device *ua, struct ua_dsp_ring *r)
{ (void)ua; (void)r; return 0; }
static unsigned int ua_dsp_ring_pending(const struct ua_dsp_ring *r)
{ return (r->write_ptr - r->hw_pos) & 1023; }
static void ua_dsp_ring_program(struct ua_device *ua, struct ua_dsp_ring *r, u32 b)
{ (void)ua; (void)b; r->hw_pos = r->write_ptr; }
static u32 ua_dsp_base(unsigned int i) { (void)i; return 0; }
static u32 ua_read(struct ua_device *ua, u32 off) { (void)ua; (void)off; return 0; }

/* Ring model: entries logged, the DSP answers on the cmd doorbell */
static char order[64];
static struct ua_ring_entry cmd_log[64];
static int ncmd, fail_put_at = -1, answer;
static u32 reply[4], hdr_word;
static __le32 *resp_buf;
static int ua_dsp_ring_put_entry(struct ua_device *ua, struct ua_dsp_ring *r,
				 const struct ua_ring_entry *e)
{
	(void)ua;
	if (r->id == 0) {
		if (ncmd == fail_put_at)
			return -ENOSPC;
		if (!ncmd)	/* the header page, while it is still ours */
			hdr_word = *(u32 *)(uintptr_t)((u64)e->word3 << 32 |
						       e->word2);
		cmd_log[ncmd++] = *e;
		strcat(order, "c");
	} else {
		resp_buf = (__le32 *)(uintptr_t)((u64)e->word3 << 32 | e->word2);
		strcat(order, "r");
	}
	r->write_ptr = (r->write_ptr + 1) & 1023;
	return 0;
}
static void ua_dsp_ring_doorbell(struct ua_device *ua, struct ua_dsp_ring *r, u32 b)
{
	(void)ua; (void)b;
	strcat(order, r->id ? "R" : "C");
	if (r->id == 0 && answer)
		memcpy(resp_buf, reply, sizeof(reply));
}
static void usleep_range(unsigned long a, unsigned long b)
{ (void)a; (void)b; jiffies++; }
'''

MAIN = r'''
static struct pci_dev pci;
static struct ua_device ua;
static u32 payload[1024];

static void reset(void)
{
	memset(&ua, 0, sizeof(ua));
	ua.pdev = &pci;
	ua.num_dsps = 1;
	ua.dsp[0].rings_allocated = true;
	ua.dsp[0].cmd.id = 0;
	ua.dsp[0].resp.id = 1;
	order[0] = 0; ncmd = 0; fail_put_at = -1;
	allocs = frees = parked = kerrs = 0;
}

int main(void)
{
	unsigned int i;

	for (i = 0; i < 1024; i++)
		payload[i] = i;

	/* 3 KiB block, answered: response slot first, header + 3 chunks,
	 * one doorbell per ring, everything freed */
	reset();
	answer = 1;
	reply[0] = 0x80040001; reply[1] = 0; reply[2] = 0; reply[3] = 0;
	assert(ua_dsp_send_block(&ua, 0, 0x120000, 0x80040000, payload, 3072) == 0);
	assert(!strcmp(order, "rRccccC"));
	assert(cmd_log[0].word0 == (UA_RING_DMA_REF | 1));
	assert(hdr_word == ((3072 / 4 + 1) | 0x120000));
	for (i = 1; i < 4; i++)
		assert(cmd_log[i].word0 == (UA_RING_DMA_REF | 256));
	assert(allocs == 3 && frees == 3 && !parked && !kerrs);

	/* No answer within 5 s: -ETIMEDOUT, all three buffers parked */
	reset();
	answer = 0;
	assert(ua_dsp_send_block(&ua, 0, 0x120000, 0x80040000, payload, 64) ==
	       -ETIMEDOUT);
	assert(parked == 3 && !frees && kerrs == 1);

	/* A DSP error status: logged; fatal only with block_strict */
	reset();
	answer = 1;
	reply[0] = 0x80040001; reply[1] = 0xF0010002;
	assert(ua_dsp_send_block(&ua, 0, 0x120000, 0x80040000, payload, 64) == 0);
	assert(kerrs == 1 && frees == 3);
	block_strict = true;
	reset();
	assert(ua_dsp_send_block(&ua, 0, 0x120000, 0x80040000, payload, 64) == -EIO);
	/* The wrong response prefix for this param is a bad response */
	reset();
	reply[0] = 0x12340001; reply[1] = 0;
	assert(ua_dsp_send_block(&ua, 0, 0x120000, 0x80040000, payload, 64) == -EIO);
	block_strict = false;

	/* Queueing fails after the header: the cmd entries are dropped
	 * (never doorbelled), the queued response slot's page parked */
	reset();
	answer = 1;
	reply[0] = 0x80040001; reply[1] = 0;
	ua.dsp[0].cmd.write_ptr = 10;
	fail_put_at = 2;
	assert(ua_dsp_send_block(&ua, 0, 0x120000, 0x80040000, payload, 3072) ==
	       -ENOSPC);
	assert(ua.dsp[0].cmd.write_ptr == 10 && !strchr(order, 'C'));
	assert(parked == 1 && frees == 2);

	/* Argument checks before anything is allocated */
	reset();
	assert(ua_dsp_send_block(&ua, 1, 0x120000, 0x80040000, payload, 64) == -EINVAL);
	assert(ua_dsp_send_block(&ua, 0, 0x120000, 0x80040000, payload, 6) == -EINVAL);
	assert(ua_dsp_send_block(&ua, 0, 0x120000, 0x80040000, payload,
				 UA_FW_MAX_SIZE + 4) == -E2BIG);
	assert(!allocs);
	return 0;
}
'''


def test_send_block():
    dsp = csrc.source('ua_dsp.c')
    code = '\n'.join((
        csrc.between(dsp, '#define UA_KERR_NO_DSPS',
                     'static const char *ua_kext_err_name('),
        csrc.extract(dsp, 'static const char *ua_kext_err_name('),
        csrc.extract(dsp, 'static int ua_kext_block_status('),
        csrc.extract(dsp, 'static size_t ua_sendblock_chunk('),
        csrc.extract(dsp, 'static int ua_dsp_block_result('),
        csrc.extract(dsp, 'int ua_dsp_send_block('),
    ))
    csrc.build_and_run(PREFIX + code + MAIN)
