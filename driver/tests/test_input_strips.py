"""input_strips: the Console Mic Input Strips ua_strips.c builds on DSP 0,
rebased onto the plug-in host (task S1; port of 0e9990b's test).

Two kinds of test.  The planner tests compile the strip planner with the
transaction builder and check the command words against apollo-linux
docs/re-notes/macos-dsp-module-load.md, Track A section A6 (the first
strip on an empty DSP 0, the per-strip shifts) and A1 (the sub-plugin
defaults per rate), written out here, not taken from the C code; the
planner-equivalence test checks that the code-page uploads plus the strip
transaction carry exactly the words 0e9990b's single batch did.  The
lifecycle tests compile the whole host (ledger, transactions on the fake
FPGA of test_dsp_xact.py, catalog with a fake /lib/firmware, upload,
ownership, the strips) and run passes: each code page waits for its reply,
the addresses come from DSP 0's ledger and equal the recipe's, nothing is
waited for with ua->lock held, teardowns are published under the lock and
proven by the next pass (else DSP 0 is quarantined), and the ledger is
back to its baseline after every teardown.  Blobs are synthetic: "Bill"
headers with made-up bodies, never UA's bytes.
"""
import re
import struct

import csrc
import khost
import test_dsp_xact as fake

# Mic strip modules: (id, blob length, code pool dwords), recipe order
MODS = [(0x0eb, 716, 0x06c), (0x0da, 3044, 0x1f0), (0x0a5, 1940, 0x130),
        (0x0c2, 184, 0x016), (0x12b, 452, 0x040)]
RESERVED = [(0x0db, 728, 0x06e), (0x0c9, 352, 0x032)]

# A6.2: code page destinations and n + 2
CODE = [(0x0eb, 0xe0000, 0xb5), (0x0da, 0xe006c, 0x2fb),
        (0x0a5, 0xe025c, 0x1e7), (0x0c2, 0xe038c, 0x30),
        (0x12b, 0xe03a2, 0x73)]

# A6.3: zero-fills of the first strip, in order
ZERO = [(0x9d1b0, 8), (0x9d178, 0x38), (0xc6fee, 0x10), (0x9d168, 0x10),
        (0x8ffdff0, 0x10), (0x9d164, 4), (0x8ffdef0, 0x100),
        (0x8ffddf0, 0x100), (0x8ffdcf0, 0x100), (0x8ffdbf0, 0x100)]

# A6.4: memspec of the first strip
MEMSPEC = [0x00150013, 0x9d1b6, 0x9d178, 0x9d1ae, 0xc6fee, 0x9d1af,
           0x9d168, 0x9d193, 0x8ffdff0, 0x9d194, 0x9d164, 0x9d186,
           0x8ffdef0, 0x9d187, 0x8ffddf0, 0x9d17e, 0x8ffdcf0, 0x9d17f,
           0x8ffdbf0]

# A6.5: header parameters
HEADER = [0x00140009, 0x9d1b0, 0x24, 0x40000000, 1, 1, 0x22, 1, 0x846]

# A1: biquads per band (< 50 kHz, 50-100 kHz, > 100 kHz)
DC = [
    [0x00000000, 0x3cb55044, 0xbf7fefd0, 0x3f7ff7f0,
     0x391565e1, 0x3f7fffef, 0xbc3573eb, 0xbbd23ba2],
    [0x00000000, 0x3c76af8a, 0xbf7ff898, 0x3f7ffc44,
     0x38893a86, 0x3f7ffffc, 0xbbf6b20e, 0xbb8e6863],
    [0x00000000, 0x3c2e7056, 0xbf7ffc44, 0x3f7ffe2a,
     0x38092b1e, 0x3f7fffff, 0xbbae7117, 0xbb498cfc],
]
LOWCUT = [
    [0x00000000, 0x3e3b3f3d, 0xbf7baf04, 0x3f7dd5f1,
     0x3c20d8d2, 0x3f7ffcd7, 0xbdbc4dc9, 0xbd565d96],
    [0x00000000, 0x3e04f704, 0xbf7dd529, 0x3f7eea30,
     0x3ba0d951, 0x3f7fff36, 0xbd855748, 0xbd18e14f],
    [0x00000000, 0x3daa574a, 0xbf7f1ceb, 0x3f7f8e5c,
     0x3b20d96c, 0x3f7fffcd, 0xbd2aa2e7, 0xbcf07ac9],
]
# A1 ramp at each rate: k = SR / BS * 0.05 + 0.5, f = BS / 4, 1 / (f k);
# BS 32 below 100 kHz, 64 above (48 kHz BS 32: 75, 8.0, 0x3ada740e)
RAMP = {44100: (69, 0x41000000, 0x3aed7304, 0),
        48000: (75, 0x41000000, 0x3ada740e, 0),
        88200: (138, 0x41000000, 0x3a6d7304, 1),
        96000: (150, 0x41000000, 0x3a5a740e, 1),
        176400: (138, 0x41800000, 0x39ed7304, 2),
        192000: (150, 0x41800000, 0x39da740e, 2)}


def shift(addr, k):
    """A6.3: strip k sits lower by 0x54 (pool 1), 0x10 (pool 2), 0x410 (3)"""
    if addr >= 0x08000000:
        return addr - 0x410 * k
    if addr >= 0xc0000:
        return addr - 0x10 * k
    return addr - 0x54 * k


def bill(mid, length, sram, body_seed=0):
    """A synthetic Bill blob (header as on disk, made-up body)."""
    hdr = struct.pack('<IIIII', 0x6c6c6942, mid, 0x02000000, length - 0x14,
                      sram * 3 // 2)
    return hdr + bytes(((i * 7 + mid + body_seed) & 0xff)
                       for i in range(length - 0x14))


def code_page(dest, n2, blob):
    sent = bytearray(blob)
    sent[7] = 0x02                  # arch byte: id |= 0x02000000
    sent[8:12] = b'\0\0\0\0'        # word 2 = 0
    return struct.pack('<II', 0x00010000 | n2, dest) + bytes(sent)


def sub_params(k, rate):
    kk, f, inv, band = RAMP[rate]
    m0 = shift(0x9d178, k)
    m3 = shift(0x8ffdff0, k)
    return ([0x00140004, m0 + 0x17, 0xc0000000, 0xc0000000,
             0x0014001b, m0 + 0x1d, 0x10, kk, f, inv,
             0xffffffff, 1, 0xffffffff, 1] + DC[band] + DC[band] + [1] +
            [0x00140012, m3] + DC[band] + LOWCUT[band])


def expected(k, synch, rate, with_code):
    """(inline words, DMA stream bytes) of strip k as 0e9990b sent it."""
    hdr = shift(0x9d1b0, k)
    inl = []
    for addr, size in ZERO:
        inl += [0x00080004, shift(addr, k), 0, size]
    inl += [0x001f0004, 0x00200000 | synch, hdr, 0,
            0x001e0004, synch, 8, 0x20,
            0x001e0004, synch, 8, 0x20,
            0x001e0004, synch, 7, 0x1ffff]
    dma = b''
    if with_code:
        for (mid, length, sram), (cid, dest, n2) in zip(MODS, CODE):
            assert mid == cid and length // 4 + 2 == n2
            dma += code_page(dest, n2, bill(mid, length, sram))
    memspec = [MEMSPEC[0]] + [shift(w, k) for w in MEMSPEC[1:]]
    header = [HEADER[0], hdr] + HEADER[2:]
    dma += struct.pack('<%dI' % len(memspec), *memspec)
    dma += struct.pack('<%dI' % len(header), *header)
    sub = sub_params(k, rate)
    assert len(sub) == 49
    dma += struct.pack('<%dI' % len(sub), *sub)
    return inl, dma


def c_bytes(name, data):
    return 'static const u8 %s[] = {%s};\n' % (
        name, ','.join(str(b) for b in data) or '0')


def c_words(name, words):
    return 'static const u32 %s[] = {%s};\n' % (
        name, ','.join('0x%x' % w for w in words) or '0')


def blob_arrays():
    out = ''
    for mid, length, sram in MODS + RESERVED:
        out += c_bytes('blob_%03x' % mid, bill(mid, length, sram))
    return out


STRIPS_STUBS = r'''
void ua_preamp_phantom_resync(struct ua_device *ua, unsigned int mask) { (void)ua; (void)mask; }
#define UA_DEV_APOLLO_X4 0x1F
#define ATOMIC_INIT(v) { v }
#define atomic_inc(a) ((a)->counter++)
struct kernel_param { int unused; };
struct kernel_param_ops {
	int (*set)(const char *, const struct kernel_param *);
	int (*get)(char *, const struct kernel_param *);
};
static int sysfs_emit(char *b, const char *fmt, ...) { (void)b; (void)fmt; return 0; }
struct ua_model_desc { unsigned int preamps; };
static struct ua_model_desc x4_model = { 4 };
static const struct ua_model_desc *ua_model_lookup(u32 t, const char **n)
{
	(void)n;
	return t == 0x1F ? &x4_model : NULL;
}
static const char *ua_device_name(u32 t) { return t == 0x1F ? "Apollo x4" : "Other"; }
static bool chain_on;
static bool ua_plugin_chain_enabled(void) { return chain_on; }
struct snd_card { int unused; };
struct snd_info_entry { void *private_data; };
struct snd_info_buffer { char text[1 << 15]; size_t len; };
static void snd_iprintf(struct snd_info_buffer *b, const char *fmt, ...)
	__attribute__((format(printf, 2, 3)));
static void snd_iprintf(struct snd_info_buffer *b, const char *fmt, ...)
{
	va_list a;
	int r;

	va_start(a, fmt);
	r = vsnprintf(b->text + b->len, sizeof(b->text) - b->len, fmt, a);
	va_end(a);
	if (r > 0)
		b->len += (size_t)r;
}
'''

PLANNER_FAKE = r'''
#define UA_RING_DMA_REF 0x80000000u
#define UA_MAX_DSPS 8
struct delayed_work { int unused; };
struct device { int unused; };
struct pci_dev { struct device dev; };
struct ua_plugin_host;
struct ua_device { struct pci_dev *pdev; struct mutex lock; struct ua_plugin_host *host; };
static struct ua_device dev;
static void dma_free_coherent(struct device *d, size_t n, void *p, dma_addr_t a)
{ (void)d; (void)n; (void)p; (void)a; }
'''


def planner_source():
    src = csrc.source('ua_strips.c')
    return re.sub(r'^#include .*$', '', csrc.between(
        src, '/* UA_STRIPS_PLANNER_BEGIN', '/* UA_STRIPS_PLANNER_END'), flags=re.M)


def build_planner(main):
    xact = csrc.source('ua_dsp_xact.c')
    builder = '\n'.join(csrc.extract(xact, sig) for sig in (
        'struct ua_xact *ua_xact_alloc(', 'static void ua_xact_add(',
        'void ua_xact_inline(', '__le32 *ua_xact_cmd(', 'int ua_xact_rx(',
        'static void ua_xact_free(', 'void ua_xact_discard('))
    up = csrc.source('ua_plugin_upload.c')
    upload = '\n'.join(csrc.extract(up, sig) for sig in (
        'u32 ua_upload_header(', 'int ua_upload_build(',
        'int ua_memspec_compile('))
    cat = csrc.source('ua_plugin_catalog.c')
    code = (khost.PREFIX + PLANNER_FAKE + khost.header('ua_resource_pool.h') +
            khost.header('ua_plugin_host.h') + STRIPS_STUBS + blob_arrays() +
            csrc.extract(cat, 'int ua_bill_check(') + builder + upload +
            planner_source() + PLANNER_HELPERS + main)
    return csrc.build_and_run(code, extra=('-Wno-unused-const-variable',))


PLANNER_HELPERS = r'''
static struct ua_strip_layout lay;
static const u8 *const mic_blobs[UA_STRIP_MODS] = {
	blob_0eb, blob_0da, blob_0a5, blob_0c2, blob_12b,
};

static void a6_layout(void)
{
	static const u32 size[4] = { 0x4000, 0xa9b8, 0x1ffe, 0xffe000 };
	static const u32 base[4] = { 0xe0000, 0x92800, 0xc5000, 0x8000000 };
	static const u32 scratch[4] = { 0, 0x80, 0x80, 0x8000 };

	assert(ua_strip_layout_check(size, base, scratch, 4, &lay) == 0);
}

/* Inline entries and DMA stream of one transaction, appended */
static u8 stream[32768];
static size_t sl;
static u32 inl[256];
static size_t ni;
static char kinds[256];
static unsigned int nk;
static void take(struct ua_xact *x)
{
	for (unsigned int i = 0; i < x->n; i++) {
		const u32 *e = x->ent[i];

		if (e[0] & 0x80000000u) {
			size_t off = e[2], len = (size_t)(e[0] & 0x7fffffff) * 4;

			assert(e[1] == 0 && e[3] == 0 && len && len <= 4096);
			/* a code page is never split: one DMA_REF of n dwords */
			if ((get_unaligned_le32(x->stage + off) >> 16) == 1)
				assert(len / 4 == (get_unaligned_le32(x->stage + off) & 0xffff));
			assert(off % 4 == 0 && off / 4096 == (off + len - 1) / 4096);
			assert(off + len <= x->len);
			memcpy(stream + sl, x->stage + off, len);
			sl += len;
			kinds[nk++] = 'D';
		} else {
			memcpy(inl + ni, e, 16);
			ni += 4;
			kinds[nk++] = 'I';
		}
	}
	kinds[nk] = 0;
}
static void reset_take(void) { sl = ni = nk = 0; kinds[0] = 0; }

/* The code pages as uploads: one transaction each, one reply each */
static void take_code(void)
{
	u32 dest[UA_STRIP_MODS];

	ua_strip_code_dest(&lay, dest);
	for (unsigned int j = 0; j < UA_STRIP_MODS; j++) {
		struct ua_cat_entry e;
		struct ua_xact *x = ua_xact_alloc(0, 4 * 4096);

		memset(&e, 0, sizeof(e));
		assert(ua_bill_check(mic_blobs[j], ua_strip_modules[j].len, &e.info) == 0);
		assert(e.info.words == ua_strip_modules[j].sram);
		e.blob = (u8 *)mic_blobs[j];
		e.len = ua_strip_modules[j].len;
		assert(ua_upload_build(x, &e, dest[j], UA_BILL_STRIP) == 0);
		assert(x->n_rx == 1 && x->rx_words[0] == 4 && x->n == 1);
		take(x);
		ua_xact_discard(&dev, x);
	}
}

/* Strip k at the recipe's addresses */
static void take_strip(unsigned int k, u32 synch, u32 rate, unsigned int steps)
{
	struct ua_strip_rate r;
	u32 addr[UA_STRIP_PRIV];
	struct ua_xact *x = ua_xact_alloc(0, 3 * 4096);

	assert(ua_strip_rate_lookup(rate, &r) == 0);
	ua_strip_priv_addrs(&lay, k, addr);
	assert(ua_strip_build(x, addr, synch, &r, steps) == 0);
	take(x);
	ua_xact_discard(&dev, x);
}
'''


def test_planner_equivalence_with_0e9990b():
    """Strip 0 on input 1 at 48 kHz: the five code-page uploads followed by
    the strip's transaction carry exactly the words 0e9990b's one batch
    did: code pages, zero-fills, memspec, header, sub params, SYNCH and the
    0x1E words, as in A6."""
    inl, dma = expected(0, 0, 48000, True)
    main = c_words('want_inl', inl) + c_bytes('want_dma', dma) + r'''
int main(void)
{
	u32 dest[UA_STRIP_MODS];

	a6_layout();
	reset_take();
	take_code();
	take_strip(0, 0, 48000, 0);
	assert(ni == ARRAY_SIZE(want_inl) && !memcmp(inl, want_inl, sizeof(want_inl)));
	assert(sl == sizeof(want_dma) && !memcmp(stream, want_dma, sl));
	printf("strip 0: %s\n", kinds);
	ua_strip_code_dest(&lay, dest);
	assert(dest[0] == 0xe0000 && dest[1] == 0xe006c && dest[2] == 0xe025c &&
	       dest[3] == 0xe038c && dest[4] == 0xe03a2);
	assert(!k_objects);
	return 0;
}
'''
    out = build_planner(main)
    # five code pages (one DMA_REF each now: one per upload), ten
    # zero-fills, memspec + header + sub params, SYNCH + three 0x1E words
    assert re.search(r'^strip 0: D{5}I{10}D{3}I{4}$', out, re.M), out


def test_strips_1_to_3_shift():
    """Slots 1-3: every pool-1 address lower by 0x54 per strip, pool 2 by
    0x10, pool 3 by 0x410; no code pages (the modules are resident)."""
    parts = ''
    calls = ''
    for k in (1, 2, 3):
        inl, dma = expected(k, k, 48000, False)
        parts += c_words('inl%d' % k, inl) + c_bytes('dma%d' % k, dma)
        calls += ('\treset_take();\n\ttake_strip(%d, %d, 48000, 0);\n'
                  '\tassert(ni == ARRAY_SIZE(inl%d) && !memcmp(inl, inl%d, sizeof(inl%d)));\n'
                  '\tassert(sl == sizeof(dma%d) && !memcmp(stream, dma%d, sl));\n'
                  '\tassert(!strcmp(kinds, "IIIIIIIIIIDDDIIII"));\n'
                  % (k, k, k, k, k, k, k))
    build_planner(parts + 'int main(void)\n{\n\ta6_layout();\n' + calls +
                  '\treturn 0;\n}\n')
    assert shift(0x9d1b0, 3) == 0x9d0b4


def test_sub_params_follow_the_rate():
    """A1 defaults at every rate (ramp words, biquad band); an unknown rate
    is refused."""
    parts = ''
    calls = ''
    for i, rate in enumerate(sorted(RAMP)):
        inl, dma = expected(0, 0, rate, False)
        parts += c_words('ri%d' % i, inl) + c_bytes('rd%d' % i, dma)
        calls += ('\treset_take();\n\ttake_strip(0, 0, %d, 0);\n'
                  '\tassert(ni == ARRAY_SIZE(ri%d) && !memcmp(inl, ri%d, sizeof(ri%d)));\n'
                  '\tassert(sl == sizeof(rd%d) && !memcmp(stream, rd%d, sl));\n'
                  % (rate, i, i, i, i, i))
    build_planner(parts + r'''
int main(void)
{
	struct ua_strip_rate r;

	a6_layout();
''' + calls + r'''
	assert(ua_strip_rate_lookup(32000, &r) == -EINVAL);
	assert(ua_strip_rate_lookup(0, &r) == -EINVAL);
	return 0;
}
''')


def test_steps_bisect_the_strip():
    """input_strips_steps: 2 = zero-fills only, 3 + memspec, 4 + header,
    5 + parameters, 6 + SYNCH, 0 = all of it."""
    out = build_planner(r'''
int main(void)
{
	a6_layout();
	for (unsigned int s = 2; s <= 7; s++) {
		reset_take();
		take_strip(0, 0, 48000, s == 7 ? 0 : s);
		printf("steps %u: %s\n", s, kinds);
	}
	return 0;
}
''')
    for steps, kinds in ((2, 'I' * 10), (3, 'I' * 10 + 'D'),
                         (4, 'I' * 10 + 'DD'), (5, 'I' * 10 + 'DDD'),
                         (6, 'I' * 10 + 'DDDI'), (7, 'I' * 10 + 'DDDIIII')):
        assert 'steps %d: %s\n' % (steps, kinds) in out, out


def test_blob_validation_and_id_scope():
    """The five Mic strip modules pass; reserved 0xDB/0xC9 and every other
    id are refused; a blob in the wrong form is rejected."""
    build_planner(r'''
static u8 tmp[4096];

static int check_mod(u32 id, const u8 *b, size_t len, unsigned int byte, u8 val)
{
	memcpy(tmp, b, len);
	tmp[byte] = val;
	return ua_strip_blob_check(id, tmp, len);
}

int main(void)
{
	assert(ua_strip_blob_check(0x0eb, blob_0eb, sizeof(blob_0eb)) == 0);
	assert(ua_strip_blob_check(0x0da, blob_0da, sizeof(blob_0da)) == 0);
	assert(ua_strip_blob_check(0x0a5, blob_0a5, sizeof(blob_0a5)) == 0);
	assert(ua_strip_blob_check(0x0c2, blob_0c2, sizeof(blob_0c2)) == 0);
	assert(ua_strip_blob_check(0x12b, blob_12b, sizeof(blob_12b)) == 0);

	/* Reserved and foreign ids: refused whatever the blob looks like */
	assert(ua_strip_blob_check(0x0db, blob_0db, sizeof(blob_0db)) == -EPERM);
	assert(ua_strip_blob_check(0x0c9, blob_0c9, sizeof(blob_0c9)) == -EPERM);
	assert(ua_strip_blob_check(0x1234, blob_0eb, sizeof(blob_0eb)) == -EPERM);
	assert(ua_strip_blob_check(0, blob_0eb, sizeof(blob_0eb)) == -EPERM);
	assert(ua_strip_blob_check(0x020000eb, blob_0eb, sizeof(blob_0eb)) == -EPERM);
	assert(!ua_strip_module_find(0x0db) && !ua_strip_module_find(0x0c9) &&
	       !ua_strip_module_find(0x0a6));

	/* Wrong form */
	assert(check_mod(0x0eb, blob_0eb, sizeof(blob_0eb), 0, 'X') == -EINVAL);
	assert(check_mod(0x0eb, blob_0eb, sizeof(blob_0eb), 7, 0x02) == -EINVAL);
	assert(check_mod(0x0eb, blob_0eb, sizeof(blob_0eb), 6, 0x01) == -EINVAL);
	assert(check_mod(0x0eb, blob_0eb, sizeof(blob_0eb), 11, 0x00) == -EINVAL);
	assert(check_mod(0x0eb, blob_0eb, sizeof(blob_0eb), 12, 0x00) == -EINVAL);
	assert(check_mod(0x0eb, blob_0eb, sizeof(blob_0eb), 16, 0x00) == -EINVAL);
	assert(check_mod(0x0eb, blob_0eb, sizeof(blob_0eb), 18, 0x01) == -EINVAL);
	assert(ua_strip_blob_check(0x0eb, blob_0eb, sizeof(blob_0eb) - 4) == -EINVAL);
	assert(ua_strip_blob_check(0x0da, blob_0eb, sizeof(blob_0eb)) == -EINVAL);
	assert(ua_strip_blob_check(0x0eb, NULL, 716) == -EINVAL);
	return 0;
}
''')


def test_pool_layout_check():
    """A6.1: the registers must give the recipe's code base and pool tops
    and room for the strips above the scratch areas."""
    build_planner(r'''
static u32 size[4] = { 0x4000, 0xa9b8, 0x1ffe, 0xffe000 };
static u32 base[4] = { 0xe0000, 0x92800, 0xc5000, 0x8000000 };
static u32 scr[4] = { 0, 0x80, 0x80, 0x8000 };

static int chk(void)
{
	struct ua_strip_layout l;

	return ua_strip_layout_check(size, base, scr, 4, &l);
}

int main(void)
{
	struct ua_strip_layout l;

	assert(ua_strip_layout_check(size, base, scr, 4, &l) == 0);
	assert(l.top[1] == 0x9d1b8 && l.top[2] == 0xc6ffe && l.top[3] == 0x08ffe000);
	assert(l.start[0] == 0xe0000);
	/* An odd base rounds up, an odd size down (CResourcePool::Initialize) */
	base[1] = 0x927ff; size[1] = 0xa9b9;
	assert(chk() == 0);
	base[1] = 0x92800; size[1] = 0xa9b8;

	base[0] = 0xe000;
	assert(chk() == UA_STRIP_LAYOUT_CODE);
	base[0] = 0xe0000; size[0] = 0x100;
	assert(chk() == UA_STRIP_LAYOUT_CODE_SZ);
	size[0] = 0x4000; size[1] = 0xa9b0;
	assert(chk() == UA_STRIP_LAYOUT_TOP + 1);
	size[1] = 0xa9b8; base[3] = 0x8000100;
	assert(chk() == UA_STRIP_LAYOUT_TOP + 3);
	base[3] = 0x8000000; scr[2] = 0x1fc0;
	assert(chk() == UA_STRIP_LAYOUT_ROOM + 2);
	scr[2] = 0x80; size[2] = 0xffffffff;
	assert(chk() == UA_STRIP_LAYOUT_GONE);
	return 0;
}
''')


def test_teardown_words():
    """A6.9: SYNCH with address 0 per strip, then the release of each
    module whose last hold goes; inline entries only."""
    build_planner(r'''
int main(void)
{
	struct ua_xact *x = ua_xact_alloc(0, 0);
	struct ua_cat_entry e;

	for (unsigned int i = 0; i < 4; i++)
		ua_strip_build_stop(x, i);
	for (unsigned int j = 0; j < UA_STRIP_MODS; j++) {
		assert(ua_bill_check(mic_blobs[j], ua_strip_modules[j].len, &e.info) == 0);
		ua_release_build(x, &e.info);
	}
	assert(x->n == 9 && !x->err && !x->len);
	for (unsigned int i = 0; i < 4; i++)
		assert(x->ent[i][0] == 0x001f0004 && x->ent[i][1] == (0x00200000 | i) &&
		       x->ent[i][2] == 0 && x->ent[i][3] == 0);
	assert(x->ent[4][0] == 0x00030002 && x->ent[4][1] == 0xeb);
	assert(x->ent[8][0] == 0x00030002 && x->ent[8][1] == 0x12b);
	ua_xact_discard(&dev, x);
	return 0;
}
''' .replace('int main(void)', csrc.extract(csrc.source('ua_plugin_upload.c'),
                                           'void ua_release_build(') + '\nint main(void)'))


# --------------------------------------------------------------------------
# The whole host: passes on the fake FPGA
# --------------------------------------------------------------------------

HOST_FAKE = r'''
#define UA_RING_DMA_REF 0x80000000u
#define UA_MAX_DSPS 8
#define UA_PLUGIN_DSP_ANY 0xffffffffu
#define UA_PLUGIN_STATE_HOST (1u << 4)
#define UA_PLUGIN_STATE_STRIPS (1u << 5)
struct work_struct { int unused; };
struct delayed_work { struct work_struct work; };
static int queued __maybe_unused, cancels __maybe_unused;
static unsigned long queued_delay __maybe_unused;
static void *system_wq __maybe_unused;
static bool queue_delayed_work(void *wq, struct delayed_work *w, unsigned long d)
{ (void)wq; (void)w; queued++; queued_delay = d; return true; }
static bool cancel_delayed_work_sync(struct delayed_work *w) { (void)w; cancels++; return false; }
#define to_delayed_work(w) container_of(w, struct delayed_work, work)
#define INIT_DELAYED_WORK(w, f) ((void)(w), (void)(f))
struct device { int unused; };
struct pci_dev { struct device dev; };
struct ua_dsp_ring {
	unsigned int write_ptr, hw_pos, capacity;
	u32 reg_base, gen;
	bool live;
};
struct ua_dsp_state { struct ua_dsp_ring cmd, resp; bool rings_allocated; };
struct ua_plugin_host;
struct ua_audio { unsigned int sample_rate; bool connected; };
struct ua_device {
	struct pci_dev *pdev;
	unsigned int num_dsps;
	struct ua_dsp_state dsp[UA_MAX_DSPS];
	struct mutex lock;
	atomic_t shutdown;
	struct ua_plugin_host *host;
	bool dsps_connected;
	struct ua_audio audio;
	u32 device_type;
};
struct task_struct { int unused; };
static struct task_struct *current;
static bool fatal_signal_pending(struct task_struct *t) { (void)t; return false; }
'''

HOST_STUBS = r'''
/* DSP register windows: the boot words and DSP 0's pool map (A6.1) */
static u32 regs[4][0x200];
static unsigned int reg_reads;
static u32 ua_read(struct ua_device *ua, u32 off)
{
	(void)ua;
	reg_reads++;
	return regs[off / 0x800][(off % 0x800) / 4];
}
static void good_regs(void)
{
	memset(regs, 0, sizeof(regs));
	for (unsigned int i = 0; i < 4; i++) {
		u32 *w = regs[i];

		w[0x10 / 4] = 0x4000;
		w[0x14 / 4] = 0xa9b8;
		w[0x18 / 4] = 0x1ffe;
		w[0x1c / 4] = 0xffe000;
		w[0x184 / 4] = 0xe0000;
		w[0x188 / 4] = 0x92800;
		w[0x18c / 4] = 0xc5000;
		w[0x190 / 4] = 0x8000000;
		w[0x194 / 4] = 0x80;
		w[0x198 / 4] = 0x80;
		w[0x19c / 4] = 0x8000;
		w[0x1a4 / 4] = 3;		/* boot word: no host since power-up */
	}
}

/* /lib/firmware */
struct firmware { size_t size; const u8 *data; };
struct disk { const char *name; const u8 *data; size_t len; };
static struct disk disk[8];
static unsigned int ndisk;
static void (*on_fw_read)(void) __maybe_unused;
static int firmware_request_nowarn(const struct firmware **fw, const char *name,
				   struct device *d)
{
	(void)d;
	if (on_fw_read)
		on_fw_read();
	for (unsigned int i = 0; i < ndisk; i++) {
		if (strcmp(disk[i].name, name))
			continue;
		struct firmware *f = calloc(1, sizeof(*f));

		f->data = disk[i].data;
		f->size = disk[i].len;
		*fw = f;
		return 0;
	}
	return -ENOENT;
}
static void release_firmware(const struct firmware *fw) { free((void *)fw); }
static void sha256(const u8 *d, size_t n, u8 out[32])
{
	/* not SHA-256: a stand-in that tells the five blobs apart */
	memset(out, 0, 32);
	for (size_t i = 0; i < n; i++)
		out[i % 32] ^= d[i] + (u8)i;
}
static char *bin2hex(char *dst, const void *src, size_t n)
{
	const u8 *s = src;

	for (size_t i = 0; i < n; i++)
		dst += sprintf(dst, "%02x", s[i]);
	return dst;
}
static int snd_card_ro_proc_new(struct snd_card *c, const char *n, void *p,
				void (*fn)(struct snd_info_entry *, struct snd_info_buffer *))
{ (void)c; (void)n; (void)p; (void)fn; return 0; }



/* The route defaults on DSPs 1-3 (inline, no wait) */
static u32 raw[4096][5];
static unsigned int nraw;
static int ua_dsp_ring_send_raw(struct ua_device *ua, unsigned int dsp, u32 w0,
				u32 w1, u32 w2, u32 w3)
{
	assert(ua->lock.held && dsp >= 1 && dsp < 4);
	raw[nraw][0] = dsp;
	raw[nraw][1] = w0;
	raw[nraw][2] = w1;
	raw[nraw][3] = w2;
	raw[nraw][4] = w3;
	nraw++;
	return 0;
}
'''


STRIPS_ONLY = r'''
bool ua_plugin_host_enabled(void) { return false; }
/* No plug-in instances in these tests (test_plugin_license.py has them) */
void ua_instances_teardown(struct ua_device *ua, const char *why) { (void)ua; (void)why; }
void ua_instances_dsp_gone(struct ua_device *ua, unsigned int dsp) { (void)ua; (void)dsp; }
'''


def host_source():
    host = csrc.source('ua_plugin_host.c')
    fns = '\n'.join(csrc.extract(host, sig) for sig in (
        'void ua_plugin_connected(', 'void ua_plugin_epoch_bump(',
        'int ua_plugin_publish_now(', 'void ua_plugin_fence_set(',
        'int ua_plugin_fence_wait(', 'void ua_plugin_teardown(',
        'void ua_plugin_host_quiesce(', 'void ua_plugin_service(',
        'void ua_plugin_quarantine(', 'void ua_plugin_device_lost('))
    return fns


def build_host(main):
    xact = khost.body('ua_dsp_xact.c')
    code = ('#define UA_REAL_QUARANTINE 1\n' + khost.PREFIX + HOST_FAKE +
            khost.header('ua_resource_pool.h') +
            khost.header('ua_plugin_host.h') + STRIPS_STUBS + HOST_STUBS +
            STRIPS_ONLY + fake.FPGA + blob_arrays() +
            khost.body('ua_resource_pool.c') + xact +
            khost.body('ua_plugin_catalog.c') + khost.body('ua_plugin_upload.c') +
            khost.body('ua_plugin_place.c') + host_source() +
            khost.body('ua_strips.c') + GLUE + main)
    return csrc.build_and_run(code, extra=('-Wno-unused-const-variable',
                                           '-Wno-unused-parameter',
                                           '-fsanitize=address,undefined',
                                           '-fno-sanitize-recover=all'))


GLUE = r'''
static void all_blobs(bool old_dir)
{
	const char *d = old_dir ? "ua_apollo/dsp" : "universal-audio/dsp";
	static char names[5][64];
	static const u8 *b[5] = { blob_0eb, blob_0da, blob_0a5, blob_0c2, blob_12b };
	static const size_t l[5] = { sizeof(blob_0eb), sizeof(blob_0da),
		sizeof(blob_0a5), sizeof(blob_0c2), sizeof(blob_12b) };
	static const u32 id[5] = { 0xeb, 0xda, 0xa5, 0xc2, 0x12b };

	ndisk = 0;
	for (int i = 0; i < 5; i++) {
		snprintf(names[i], sizeof(names[i]), "%s/%03x.bill", d, id[i]);
		disk[ndisk++] = (struct disk){ names[i], b[i], l[i] };
	}
}

static void teardown_host(void)
{
	if (!host.ua)
		return;
	mutex_lock(&dev.lock);
	ua_strips_free(&dev);
	for (int i = 0; i < UA_HOST_DSPS; i++) {
		if (host.fence[i])
			ua_xact_done(&dev, host.fence[i]);
		host.fence[i] = NULL;
		if (host.dsp[i].pools)
			ua_pool_owner_destroy(host.dsp[i].pools);
		host.dsp[i].pools = NULL;
	}
	stalled = false;
	fpga_tick();
	ua_xact_reclaim(&dev);
	mutex_unlock(&dev.lock);
	ua_catalog_free_all(&dev);
}

static void setup(u32 type)
{
	teardown_host();
	reset();
	host.mutex.held = 0;
	INIT_LIST_HEAD(&host.catalog);
	INIT_LIST_HEAD(&host.instances);
	host.active = true;
	dev.device_type = type;
	dev.dsps_connected = true;
	dev.audio.sample_rate = 48000;
	for (int i = 1; i < 4; i++)
		dev.dsp[i].rings_allocated = true;
	good_regs();
	all_blobs(false);
	input_strips = true;
	input_strips_mask = 0xff;
	input_strips_steps = 0;
	input_strips_form = 0;
	input_strips_params = 0;
	input_strips_dsp = 0;
	chain_on = false;
	nraw = queued = 0;
	k_log_reset();
}

static void cold_connect(void)
{
	mutex_lock(&dev.lock);
	ua_plugin_dsp_start(&dev, true);
	ua_plugin_connected(&dev);
	mutex_unlock(&dev.lock);
}
static void connect_again(void)
{
	mutex_lock(&dev.lock);
	ua_plugin_connected(&dev);
	mutex_unlock(&dev.lock);
}
static void teardown(const char *why)
{
	mutex_lock(&dev.lock);
	ua_plugin_teardown(&dev, why);
	mutex_unlock(&dev.lock);
}
static void service(void)
{
	mutex_lock(&dev.lock);
	ua_plugin_service(&dev);
	mutex_unlock(&dev.lock);
}
static const char *proc(void)
{
	static struct snd_info_buffer b;

	b.len = 0;
	b.text[0] = 0;
	mutex_lock(&dev.lock);
	ua_strips_proc(&dev, &b);
	mutex_unlock(&dev.lock);
	return b.text;
}
static bool ledger_baseline(void)
{
	struct ua_pool_usage u[4];

	if (!host.dsp[0].pools)
		return false;
	assert(ua_pool_read_usage(host.dsp[0].pools, u) == 0);
	return ua_pool_owner_idle(host.dsp[0].pools) && !u[1].used_words &&
	       u[1].scratch_words == 0x80 && u[2].scratch_words == 0x80 &&
	       !u[0].used_words;
}
#define HAS(s) K_HAS(s)
'''


def test_cold_start_pass_waits_for_replies_and_tears_down():
    """Cold start on an x4: the five modules go out one upload at a time,
    each acknowledged; then the four strips one transaction each (taken by
    DSP 0 before the next), all waited for with ua->lock released; the
    teardown stops them and releases the modules; the ledger is back to its
    baseline; the next connect builds them again after proving the
    teardown."""
    build_host(r'''
int main(void)
{
	struct ua_strips *st;

	setup(0x1F);
	ua_strips_init(&dev);
	st = host.strips;
	assert(st && st->state == UA_STRIPS_IDLE && st->n == 4);
	assert(HAS("input strips: enabled for 4 analog input(s) (mask 0xf)"));
	cold_connect();
	assert(host.dsp[0].owned && host.dsp[0].pools);
	assert(st->state == UA_STRIPS_ARMED && queued == 1 && queued_delay == 500);
	assert(ledger_baseline());

	ua_strips_run(&dev);
	assert(!sleeps_locked && !dev.lock.held && !host.mutex.held);
	assert(st->state == UA_STRIPS_LIVE);
	assert(HAS("DSP0 pools empty, creating 4 Mic Input Strip(s) at 48000 Hz"));
	assert(replies_made == 5);
	/* not plug-ins: the strips never ask for the authorization table */
	assert(!host.board_queries && !bq_answered);
	assert(host.dsp[0].uploads == 5 && host.dsp[0].replies_ok == 5);
	assert(host.dsp[0].tx_xacts == 5 + 4);
	for (int j = 0; j < 5; j++)
		assert(ua_catalog_resident(st->mod[j], 0, host.epoch));
	assert(st->mod[0]->res[0].base == 0xe0000 && st->mod[4]->res[0].base == 0xe03a2);
	assert(st->strip[3].header == 0x9d0b4 && st->strip[0].header == 0x9d1b0);
	assert(HAS("input 4: Mic Input Strip at 0x9d0b4 on DSP 0"));
	/* route defaults on DSPs 1-3 for each input */
	assert(nraw == 12 && raw[0][0] == 1 && raw[0][1] == 0x001e0004 &&
	       raw[0][3] == 7 && raw[0][4] == 0x1ffff);
	/* DSP 0's ledger: 994 code words, four strips' private rows */
	{
		struct ua_pool_usage u[4];

		assert(ua_pool_read_usage(host.dsp[0].pools, u) == 0);
		assert(u[0].used_words == 0x6c + 0x1f0 + 0x130 + 0x16 + 0x40);
		assert(u[0].holds == 5 && u[1].used_words == 4 * 0x54 &&
		       u[2].used_words == 4 * 0x10 && u[3].used_words == 4 * 0x410);
		assert(u[1].scratch_words == 0x1c0);
	}
	assert(strstr(proc(), "strip 0: input 1 (DSP input 0), instance 0x9d1b0, live"));
	assert(strstr(proc(), "module 0x0eb: resident at 0xe0000"));

	/* live strips are not rebuilt by another connect */
	connect_again();
	assert(st->state == UA_STRIPS_LIVE && queued == 1);

	/* teardown: published under the lock, no wait; ledger at baseline */
	{
		unsigned int c = dev.dsp[0].cmd.write_ptr;

		teardown("test");
		assert(HAS("4 strip(s) stopped, modules released (test)"));
		assert(st->state == UA_STRIPS_IDLE && host.fence[0]);
		assert(slots[0][c][0] == 0x001f0004 && slots[0][c][1] == 0x00200000 &&
		       slots[0][c][2] == 0);
		assert(slots[0][(c + 4) & 1023][0] == 0x00030002 &&
		       slots[0][(c + 4) & 1023][1] == 0x0eb);
		assert(slots[0][(c + 8) & 1023][1] == 0x12b);
		assert(host.dsp[0].stops == 4 && host.dsp[0].releases == 5);
		assert(ledger_baseline());
		for (int j = 0; j < 5; j++)
			assert(!ua_catalog_resident(st->mod[j], 0, host.epoch));
	}
	/* a teardown with nothing out sends nothing */
	teardown("again");
	assert(host.dsp[0].stops == 4);

	/* the next pass proves the teardown first, uploads again */
	connect_again();
	assert(st->state == UA_STRIPS_ARMED);
	ua_strips_run(&dev);
	assert(st->state == UA_STRIPS_LIVE && !host.fence[0] && replies_made == 10);
	assert(!host.dsp[0].quarantined);

	/* an armed pass that has not started is just cancelled */
	teardown("rate change");
	connect_again();
	teardown("rate change");
	assert(st->state == UA_STRIPS_IDLE);
	ua_strips_run(&dev);
	assert(st->state == UA_STRIPS_IDLE);
	teardown_host();
	assert(!k_objects && !dma_live);
	return 0;
}
''')


def test_fifty_rebuilds_keep_the_baseline():
    """input_strips_rebuild (S2's owner check in miniature): fifty
    teardown/create cycles, the ledger back at its baseline after every
    teardown, no transaction held, no quarantine."""
    build_host(r'''
int main(void)
{
	struct ua_strips *st;

	setup(0x1F);
	ua_strips_init(&dev);
	st = host.strips;
	cold_connect();
	ua_strips_run(&dev);
	assert(st->state == UA_STRIPS_LIVE);
	for (int i = 0; i < 50; i++) {
		ua_strips_rebuild_set("1", NULL);
		service();
		assert(st->state == UA_STRIPS_ARMED);
		assert(ledger_baseline());
		ua_strips_run(&dev);
		assert(st->state == UA_STRIPS_LIVE && !host.dsp[0].quarantined);
	}
	assert(st->passes == 51 && st->teardowns == 50 && replies_made == 5 * 51);
	assert(!sleeps_locked);
	mutex_lock(&dev.lock);
	ua_xact_reclaim(&dev);
	mutex_unlock(&dev.lock);
	assert(host.xact_count <= 2);
	/* a rebuild request while nothing is live is just remembered */
	teardown("x");
	ua_strips_rebuild_set("1", NULL);
	service();
	assert(st->state == UA_STRIPS_IDLE);
	teardown_host();
	assert(!k_objects && !dma_live);
	return 0;
}
''')


def test_dsp0_not_owned_gets_nothing():
    """A DSP 0 found running at probe, one a host used since power-up (a
    Linux connect count, a macOS session), or one a raw DSP ioctl touched:
    no strip, nothing sent; until the next cold start."""
    build_host(r'''
int main(void)
{
	/* warm: another session's strips may be on DSP 0 */
	setup(0x1F);
	ua_strips_init(&dev);
	mutex_lock(&dev.lock);
	ua_plugin_dsp_start(&dev, false);
	ua_plugin_connected(&dev);
	mutex_unlock(&dev.lock);
	assert(host.strips->state == UA_STRIPS_IDLE && !queued);
	ua_strips_run(&dev);
	assert(!nputs && !replies_made && !nraw);

	/* cold path, but a host ring-connected DSP 0 since power-up */
	setup(0x1F);
	regs[0][0x1a4 / 4] = 0x0018000b;
	ua_strips_init(&dev);
	cold_connect();
	assert(host.strips->state == UA_STRIPS_IDLE && !host.dsp[0].owned);
	assert(strstr(host.dsp[0].why, "boot word 0x0018000b"));

	setup(0x1F);
	regs[0][0x1a4 / 4] = 0x00100007;	/* the x4 after macOS */
	ua_strips_init(&dev);
	cold_connect();
	assert(host.strips->state == UA_STRIPS_IDLE && !nputs);

	/* a raw DSP ioctl since the cold start */
	setup(0x1F);
	ua_strips_init(&dev);
	mutex_lock(&dev.lock);
	ua_plugin_dsp_start(&dev, true);
	mutex_unlock(&dev.lock);
	ua_plugin_taint(&dev, "a raw DSP ioctl");
	connect_again();
	assert(host.strips->state == UA_STRIPS_IDLE && !nputs);
	/* ... until the next cold start */
	cold_connect();
	ua_strips_run(&dev);
	assert(host.strips->state == UA_STRIPS_LIVE);
	teardown_host();
	return 0;
}
''')


def test_refusals_and_failures():
    """Missing or foreign modules, other models, a layout that differs, no
    ring connect, the plugin chain, a module DSP 0 refuses, a strip or
    teardown DSP 0 does not take: nothing is sent, or nothing more, and an
    outcome that cannot be proven quarantines DSP 0."""
    build_host(r'''
int main(void)
{
	struct ua_strips *st;

	/* a missing module disables the strips for the device */
	setup(0x1F);
	ndisk = 4;
	ua_strips_init(&dev);
	assert(host.strips->state == UA_STRIPS_OFF);
	assert(HAS("module 0x12b not found"));
	cold_connect();
	ua_strips_run(&dev);
	assert(!nputs && strstr(proc(), "disabled"));

	/* the older tool's directory still works */
	setup(0x1F);
	all_blobs(true);
	ua_strips_init(&dev);
	assert(host.strips->state == UA_STRIPS_IDLE);

	/* a module that is not the one this driver knows (arch byte set) */
	setup(0x1F);
	{
		static u8 bad[716];

		memcpy(bad, blob_0eb, sizeof(bad));
		bad[7] = 2;
		disk[0].data = bad;
		ua_strips_init(&dev);
	}
	assert(host.strips->state == UA_STRIPS_OFF);

	/* other models: no verified analog input map */
	setup(0x22);
	ua_strips_init(&dev);
	assert(host.strips->state == UA_STRIPS_OFF);
	assert(HAS("no verified analog input map for Other (device type 0x22)"));

	/* pool layout differs from the recipe's */
	setup(0x1F);
	ua_strips_init(&dev);
	regs[0][0x14 / 4] = 0xa9b0;
	cold_connect();
	ua_strips_run(&dev);
	assert(!nputs && HAS("not created: pool 1 does not end where the recipe expects"));
	assert(strstr(proc(), "pools: layout differs"));
	assert(ua_pool_owner_idle(host.dsp[0].pools));

	/* not ring-connected; the captured plugin chain on */
	setup(0x1F);
	ua_strips_init(&dev);
	dev.dsps_connected = false;
	cold_connect();
	ua_strips_run(&dev);
	assert(!nputs && HAS("not ring-connected"));
	dev.dsps_connected = true;
	chain_on = true;
	connect_again();
	ua_strips_run(&dev);
	assert(!nputs && HAS("captured plugin chain is on"));

	/* DSP 0 refuses a module: quarantined, the strips stop there */
	setup(0x1F);
	ua_strips_init(&dev);
	st = host.strips;
	cold_connect();
	reply_status = 0xf0010006;
	ua_strips_run(&dev);
	assert(host.dsp[0].quarantined && replies_made == 1);
	assert(HAS("a strip module was not accepted by DSP 0"));
	assert(st->state == UA_STRIPS_LIVE);	/* the upload must be released */
	reply_status = 0;
	teardown("disconnect");
	assert(st->state == UA_STRIPS_IDLE && host.dsp[0].releases == 1);
	connect_again();				/* quarantined: no pass */
	assert(st->state == UA_STRIPS_IDLE);
	cold_connect();					/* a cold start clears it */
	assert(!host.dsp[0].quarantined && st->state == UA_STRIPS_ARMED);

	/* a teardown DSP 0 does not take: the next pass quarantines DSP 0 */
	setup(0x1F);
	ua_strips_init(&dev);
	st = host.strips;
	cold_connect();
	ua_strips_run(&dev);
	assert(st->state == UA_STRIPS_LIVE);
	stalled = true;
	teardown("quiesce");
	connect_again();
	ua_strips_run(&dev);
	assert(host.dsp[0].quarantined && st->state == UA_STRIPS_IDLE);
	assert(HAS("a teardown was not taken"));
	teardown_host();

	/* the quiesce waits for the teardown with the lock released */
	setup(0x1F);
	ua_strips_init(&dev);
	cold_connect();
	ua_strips_run(&dev);
	sleeps_locked = 0;
	ua_plugin_host_quiesce(&dev, "suspend", true);
	assert(!sleeps_locked && !host.fence[0] && !host.dsp[0].quarantined);
	assert(host.strips->state == UA_STRIPS_IDLE && ledger_baseline());
	/* device gone: nothing sent, nothing owned any more */
	connect_again();
	ua_strips_run(&dev);
	{
		unsigned int n0 = nputs;

		ua_plugin_host_quiesce(&dev, "remove", false);
		assert(nputs == n0 && !host.dsp[0].owned);
	}
	teardown_host();
	assert(!k_objects && !dma_live);
	return 0;
}
''')


def test_strip_stall_quarantines_dsp0():
    """A strip whose transaction DSP 0 does not take within 1 s: the pass
    ends, DSP 0 is quarantined, and the teardown still stops what went out
    and releases the modules (proven once the ring moves again)."""
    build_host(r'''
int main(void)
{
	struct ua_strips *st;
	unsigned int c;

	setup(0x1F);
	ua_strips_init(&dev);
	st = host.strips;
	cold_connect();
	consume_limit = 5;		/* the five uploads go through, then nothing */
	ua_strips_run(&dev);
	assert(replies_made == 5 && host.dsp[0].quarantined);
	assert(HAS("DSP 0 did not take a strip in time"));
	assert(st->state == UA_STRIPS_LIVE && st->strip[0].state == UA_STRIP_QUEUED &&
	       st->strip[1].state == UA_STRIP_NONE);
	assert(!sleeps_locked);
	c = dev.dsp[0].cmd.write_ptr;
	teardown("disconnect");
	/* one stop (strip 0 went out), five releases */
	assert(slots[0][c][0] == 0x001f0004 && slots[0][c][1] == 0x00200000);
	assert(slots[0][(c + 1) & 1023][0] == 0x00030002);
	assert(host.dsp[0].stops == 1 && host.dsp[0].releases == 5);
	assert(ledger_baseline() && st->state == UA_STRIPS_IDLE);
	/* quarantined: no new pass until a cold start */
	connect_again();
	assert(st->state == UA_STRIPS_IDLE);
	consume_limit = ~0u;
	cold_connect();
	assert(st->state == UA_STRIPS_ARMED && !host.dsp[0].quarantined);
	teardown_host();
	assert(!k_objects && !dma_live);
	return 0;
}
''')


def test_device_lost_forgets_without_sending():
    """A PCIe error: the strips are forgotten without a command, every DSP
    is disowned, the fence dropped; the next cold start owns DSP 0 again
    and builds the strips afresh."""
    build_host(r'''
int main(void)
{
	struct ua_strips *st;
	unsigned int n0;

	setup(0x1F);
	ua_strips_init(&dev);
	st = host.strips;
	cold_connect();
	ua_strips_run(&dev);
	assert(st->state == UA_STRIPS_LIVE);
	teardown("x");
	connect_again();
	assert(host.fence[0] || st->state == UA_STRIPS_ARMED);
	ua_strips_run(&dev);
	assert(st->state == UA_STRIPS_LIVE);
	dev.shutdown.counter = 1;
	n0 = nputs;
	ua_plugin_device_lost(&dev, "a PCIe error");
	assert(nputs == n0 && st->state == UA_STRIPS_IDLE);
	for (int i = 0; i < 4; i++)
		assert(!host.dsp[i].owned && !host.dsp[i].pools && !host.fence[i]);
	assert(cancels >= 1 && HAS("forgotten, the device's state is gone"));
	dev.shutdown.counter = 0;
	connect_again();
	assert(st->state == UA_STRIPS_IDLE);
	cold_connect();
	ua_strips_run(&dev);
	assert(st->state == UA_STRIPS_LIVE && !host.dsp[0].quarantined);
	teardown_host();
	assert(!k_objects && !dma_live);
	return 0;
}
''')


def test_input_strips_off_changes_nothing():
    """input_strips=0: no strips state, so every hook returns at once (no
    register read, no ring entry, no log line from the strips); the proc
    block only says it is off."""
    build_host(r'''
int main(void)
{
	setup(0x1F);
	input_strips = false;
	host.active = false;
	ua_strips_init(&dev);
	assert(!host.strips);
	mutex_lock(&dev.lock);
	ua_plugin_dsp_start(&dev, true);
	ua_plugin_connected(&dev);
	ua_plugin_teardown(&dev, "x");
	ua_plugin_service(&dev);
	mutex_unlock(&dev.lock);
	ua_strips_run(&dev);
	assert(!reg_reads && !nputs && !nraw && !queued);
	assert(!strcmp(proc(), "\ninput strips\toff (input_strips=0)\n"));
	return 0;
}
''')
    src = csrc.source('ua_strips.c')
    assert 'static bool input_strips;' in src
    assert 'module_param(input_strips, bool, 0444);' in src
    assert 'module_param(input_strips_mask, uint, 0444);' in src
    init = csrc.extract(src, 'void ua_strips_init(')
    assert init.index('if (!input_strips || !h)\n\t\treturn;') < init.index('kzalloc')
    for fn in ('void ua_strips_free(', 'void ua_strips_connected(',
               'void ua_strips_run(', 'void ua_strips_teardown(',
               'void ua_strips_service(', 'void ua_strips_proc('):
        body = csrc.extract(src, fn)
        first = body.split('\n\tif (!st', 1)
        assert len(first) == 2 and 'ua_read' not in first[0] and \
            'mutex_lock' not in first[0], fn
    assert 'ua_strips.o' in csrc.source('Makefile')


def test_scope_no_user_interface():
    """Licensing boundary: no ioctl or user-space path into the strips, only
    the five Mic strip modules load, from the module store, and no
    authorization is involved (they are not plug-ins)."""
    ioctl = csrc.source('ua_ioctl.h')
    # GET_PLUGIN_CAPS reports whether strips run; nothing operates them
    assert not re.search(r'UA_IOCTL_\w*STRIP|struct ua_\w*strip', ioctl, re.I)
    src = csrc.source('ua_strips.c')
    assert 'ua_catalog_get_system(' in src
    assert 'copy_from_user' not in src and 'unlocked_ioctl' not in src
    code = re.sub(r'/\*.*?\*/', '', src, flags=re.S)
    for name in ('board_query', 'UA_PLUGIN_LICENSE', 'ua_plugin_admit', 'authoriz'):
        assert name not in code, name
    table = csrc.between(src, 'static const struct ua_strip_module ua_strip_modules[] = {',
                         '};')
    rows = re.findall(r'\{ (0x[0-9a-f]+),\s+\d+, 0x[0-9a-f]+, (true|false) \}', table)
    assert [(int(i, 16), r) for i, r in rows] == [
        (0x0eb, 'false'), (0x0da, 'false'), (0x0a5, 'false'),
        (0x0c2, 'false'), (0x12b, 'false'), (0x0db, 'true'), (0x0c9, 'true')]
    # No blob bytes in the source: no long hex byte runs
    assert not re.search(r'(0x[0-9a-f]{2},\s*){32}', src)


def test_waits_are_off_lock():
    """Review finding 5: the strips never wait for the DSP with ua->lock
    held; the teardown only publishes."""
    src = csrc.source('ua_strips.c')
    td = csrc.extract(src, 'void ua_strips_teardown(')
    assert 'ua_xact_wait' not in td and 'ua_xact_run' not in td
    assert 'sleep' not in td and 'ua_plugin_publish_now(ua, x)' in td
    assert 'msleep' not in src and 'usleep' not in src and 'mdelay' not in src
    run = csrc.extract(src, 'void ua_strips_run(')
    # every ua_xact_run / upload happens outside ua->lock
    for call in ('ua_xact_run(ua, x, UA_XACT_WAIT_MS)',):
        before = run[:run.index(call)]
        assert before.rfind('mutex_unlock(&ua->lock)') > before.rfind('mutex_lock(&ua->lock)')
    hooks = re.findall(r'ua_strips_\w+\(', csrc.source('ua_core.c') +
                       csrc.source('ua_audio.c') + csrc.source('ua_dsp.c'))
    assert set(hooks) <= {'ua_strips_init(', 'ua_strips_proc('}, hooks
