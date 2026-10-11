"""The plugin-chain payload loader checks its whole table before it
allocates, and activation sends the chain whole or not at all.

ua_dsp_load_plugin_payloads(), ua_plugin_lookup_dma(),
ua_dsp_free_plugin_payloads() and ua_dsp_activate_plugin_chain() are
compiled as they are against the real ua_plugin_chain.h, with blobs built
the way tools/build-plugin-chain-firmware.py builds them and mocked DMA and
ring calls.  UA_PLUGIN_CHAIN_FIXTURE can name a real
ua-apollo-plugin-chain.bin to also check that it loads.  Adapted from the
parallel Codex work (test_plugin_payload_loading); driver-v2 keeps its
circular-ring batching, so the ring side here only records entries.
"""
import os
import re
import struct
import csrc

FLAG = 0x80000000


def chain_rows():
    text = csrc.source('ua_plugin_chain.h')
    return [[int(v, 16) for v in m] for m in re.findall(
        r'\{\s*(0x[0-9a-f]+),\s*(0x[0-9a-f]+),\s*(0x[0-9a-f]+),'
        r'\s*(0x[0-9a-f]+)\s*\}', text)]


def blob(entries):
    """entries: [(ring_idx, payload_bytes)] in the builder's layout."""
    data_offset = 16 + 12 * len(entries)
    table, payload = b'', b''
    for idx, nbytes in entries:
        table += struct.pack('<III', idx, nbytes, data_offset + len(payload))
        payload += bytes((idx + k) & 0xff for k in range(nbytes))
    return struct.pack('<IIII', 0x43504155, 1, len(entries),
                       data_offset) + table + payload


def c_array(name, data):
    return 'static const unsigned char %s[] = {%s};\nstatic const size_t %s_len = %d;\n' % (
        name, ','.join(str(b) for b in data) or '0', name, len(data))


PREFIX = r'''
#include <assert.h>
#include <errno.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
typedef uint8_t u8;
typedef uint32_t u32;
typedef uint64_t u64;
typedef uint32_t __le32;
typedef uint64_t dma_addr_t;
#define GFP_KERNEL 0
#define BIT(n) (1UL << (n))
#define UA_RING_DMA_REF BIT(31)
#define UA_RING_CMD_OFFSET 0
#define UA_PLUGIN_CHAIN_FW_MAGIC 0x43504155
#define UA_PLUGIN_CHAIN_FW_VERSION 1
#define UA_PLUGIN_CHAIN_FW_NAME "ua-apollo-plugin-chain.bin"
#define BITS_PER_LONG (8 * sizeof(long))
#define DECLARE_BITMAP(n, bits) unsigned long n[((bits) + BITS_PER_LONG - 1) / BITS_PER_LONG]
#define bitmap_zero(b, n) memset(b, 0, sizeof(unsigned long) * (((n) + BITS_PER_LONG - 1) / BITS_PER_LONG))
static bool __test_and_set_bit(unsigned int n, unsigned long *b)
{
	bool old = b[n / BITS_PER_LONG] >> (n % BITS_PER_LONG) & 1;

	b[n / BITS_PER_LONG] |= 1UL << (n % BITS_PER_LONG);
	return old;
}
#define min(a, b) ((a) < (b) ? (a) : (b))
#define cpu_to_le32(x) (x)
#define lower_32_bits(x) ((u32)(x))
#define upper_32_bits(x) ((u32)((u64)(x) >> 32))
static u32 le32_to_cpup(const __le32 *p) { u32 v; memcpy(&v, p, 4); return v; }
struct firmware { size_t size; const u8 *data; };
struct ua_ring_entry { u32 word0, word1, word2, word3; };
struct ua_dsp_ring { unsigned int write_ptr, hw_pos; };
struct ua_dsp_state { struct ua_dsp_ring cmd; bool rings_allocated; };
struct ua_plugin_dma_buf { unsigned int ring_idx; size_t size; dma_addr_t dma;
			   void *cpu; };
struct pci_dev { int dev; };
struct ua_device { struct pci_dev *pdev; struct ua_dsp_state dsp[1];
		   struct ua_plugin_dma_buf *plugin_bufs;
		   unsigned int plugin_bufs_count; bool skip_bus_coeff; };
static void log_(int *d, const char *fmt, ...) { (void)d; (void)fmt; }
#define dev_err log_
#define dev_warn log_
#define dev_warn_once log_
#define dev_info log_
#define dev_dbg log_
static struct firmware fw_now;
static int fw_ret, allocs, frees, nput;
static struct ua_ring_entry sent[2048];
static int firmware_request_nowarn(const struct firmware **fw,
				   const char *n, int *d)
{ (void)n; (void)d; if (fw_ret) return fw_ret; *fw = &fw_now; return 0; }
static void release_firmware(const struct firmware *fw) { (void)fw; }
static void *kcalloc(size_t n, size_t sz, int f) { (void)f; return calloc(n, sz); }
static void kfree(void *p) { free(p); }
static void *dma_alloc_coherent(int *d, size_t n, dma_addr_t *dma, int f)
{ void *p = malloc(n); (void)d; (void)f; *dma = 0x1000000 + (dma_addr_t)allocs * 0x10000; allocs++; return p; }
static void dma_free_coherent(int *d, size_t n, void *p, dma_addr_t dma)
{ (void)d; (void)n; (void)dma; free(p); frees++; }
static u32 ua_dsp_base(unsigned int i) { (void)i; return 0; }
static void ua_dsp_ring_program(struct ua_device *ua, struct ua_dsp_ring *r, u32 b)
{ (void)ua; (void)r; (void)b; }
static int ua_dsp_ring_wait_space(struct ua_device *ua, struct ua_dsp_ring *r,
				  unsigned int n, unsigned int t)
{ (void)ua; (void)r; (void)n; (void)t; return 0; }
static int ua_dsp_ring_put_entry(struct ua_device *ua, struct ua_dsp_ring *r,
				 const struct ua_ring_entry *e)
{ (void)ua; sent[nput++] = *e; r->write_ptr = (r->write_ptr + 1) & 1023; return 0; }
static void ua_dsp_ring_doorbell(struct ua_device *ua, struct ua_dsp_ring *r, u32 b)
{ (void)ua; (void)r; (void)b; }
static bool ua_dsp_ring_wait_idle(struct ua_device *ua, struct ua_dsp_ring *r,
				  unsigned int t)
{ (void)ua; (void)r; (void)t; return true; }
static unsigned int ua_dsp_ring_pending(const struct ua_dsp_ring *r)
{ (void)r; return 0; }
'''

MAIN = r'''
static struct pci_dev pci;
static struct ua_device ua_new(void)
{
	struct ua_device ua = { .pdev = &pci };

	ua.dsp[0].rings_allocated = true;
	return ua;
}

static void use(const unsigned char *b, size_t n)
{
	fw_now = (struct firmware){ n, b };
	nput = 0;
}

static int dma_refs(void)
{
	int i, n = 0;

	for (i = 0; i < UA_PLUGIN_CHAIN_COUNT; i++)
		n += !!(ua_plugin_chain_data[i][0] & UA_RING_DMA_REF);
	return n;
}

int main(void)
{
	struct ua_device ua;
	int i, k;

	/* The builder's blob: loads, and the whole chain goes out */
	ua = ua_new();
	use(good, good_len);
	assert(ua_dsp_load_plugin_payloads(&ua) == 0);
	assert((int)ua.plugin_bufs_count == dma_refs());
	assert(ua_dsp_activate_plugin_chain(&ua) == 0 &&
	       nput == UA_PLUGIN_CHAIN_COUNT);
	for (i = 0; i < UA_PLUGIN_CHAIN_COUNT; i++) {
		const u32 *c = ua_plugin_chain_data[i];

		assert(sent[i].word0 == c[0] && sent[i].word1 == c[1]);
		if (c[0] & UA_RING_DMA_REF) {
			struct ua_plugin_dma_buf *b = ua_plugin_lookup_dma(&ua, i);

			assert(b && sent[i].word2 == lower_32_bits(b->dma) &&
			       sent[i].word3 == upper_32_bits(b->dma));
		} else {
			assert(sent[i].word2 == c[2] && sent[i].word3 == c[3]);
		}
	}
	ua_dsp_free_plugin_payloads(&ua);
	assert(allocs == frees);

	/* A row left without its payload: loads, but nothing is sent */
	ua = ua_new();
	use(missing, missing_len);
	assert(ua_dsp_load_plugin_payloads(&ua) == 0);
	assert(ua_dsp_activate_plugin_chain(&ua) == -EINVAL && nput == 0);
	ua_dsp_free_plugin_payloads(&ua);

	/* Tables that do not match the chain: refused before any allocation */
	{
		const unsigned char *bad[] = { not_dmaref, dup, small, past,
					       outside, before_table };
		const size_t len[] = { not_dmaref_len, dup_len, small_len,
				       past_len, outside_len,
				       before_table_len };

		for (k = 0; k < (int)(sizeof(bad) / sizeof(bad[0])); k++) {
			int before = allocs;

			ua = ua_new();
			use(bad[k], len[k]);
			assert(ua_dsp_load_plugin_payloads(&ua) == -EINVAL);
			assert(allocs == before && !ua.plugin_bufs);
			/* activation retries the load, then refuses */
			assert(ua_dsp_activate_plugin_chain(&ua) == -ENODATA &&
			       nput == 0);
		}
	}

	/* No blob installed: nothing sent, -ENODATA (retried later) */
	ua = ua_new();
	fw_ret = -ENOENT;
	assert(ua_dsp_activate_plugin_chain(&ua) == -ENODATA && nput == 0);
	fw_ret = 0;

#ifdef REAL_BLOB
	ua = ua_new();
	use(real, real_len);
	assert(ua_dsp_load_plugin_payloads(&ua) == 0);
	assert(ua_dsp_activate_plugin_chain(&ua) == 0 &&
	       nput == UA_PLUGIN_CHAIN_COUNT);
	ua_dsp_free_plugin_payloads(&ua);
#endif
	assert(allocs == frees);
	return 0;
}
'''


def test_loader_and_activation():
    rows = chain_rows()
    assert len(rows) == 1317
    refs = [i for i, r in enumerate(rows) if r[0] & FLAG]
    assert len(refs) == 99
    size = {i: 4 * (rows[i][0] & ~FLAG) for i in refs}
    good = [(i, size[i] + (4 if i == refs[0] else 0)) for i in refs]
    non_ref = next(i for i, r in enumerate(rows) if not r[0] & FLAG)

    blobs = {
        'good': blob(good),
        'missing': blob(good[:-1]),
        'not_dmaref': blob(good[:-1] + [(non_ref, 16)]),
        'dup': blob(good[:-1] + [good[0]]),
        'small': blob(good[:-1] + [(good[-1][0], good[-1][1] - 4)]),
        'past': blob(good[:-1] + [(1317, 16)]),
    }
    out = bytearray(blob(good))
    struct.pack_into('<I', out, 16 + 12 * 5 + 4, 0x7ffffff0)  # entry 5 size
    blobs['outside'] = bytes(out)
    out = bytearray(blob(good))
    struct.pack_into('<I', out, 16 + 12 * 7 + 8, 16)  # entry 7 in the table
    blobs['before_table'] = bytes(out)

    dsp = csrc.source('ua_dsp.c')
    code = '\n'.join((
        csrc.source('ua_plugin_chain.h'),
        csrc.extract(dsp, 'static u32 ua_ring_ref_dwords('),
        csrc.extract(dsp, 'void ua_dsp_free_plugin_payloads('),
        csrc.extract(dsp, 'int ua_dsp_load_plugin_payloads('),
        csrc.extract(dsp, 'static struct ua_plugin_dma_buf *\n'
                          'ua_plugin_lookup_dma('),
        csrc.extract(dsp, 'int ua_dsp_activate_plugin_chain('),
    ))
    arrays = ''.join(c_array(k, v) for k, v in blobs.items())
    real = os.environ.get('UA_PLUGIN_CHAIN_FIXTURE')
    if real:
        with open(real, 'rb') as f:
            arrays += '#define REAL_BLOB\n' + c_array('real', f.read())
    csrc.build_and_run(PREFIX + arrays + code + MAIN)
