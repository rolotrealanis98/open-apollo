"""ua_plugin_upload.c: resource upload, reply check, memspec, release (A5).

Framing vectors are the dsp-loader contract fixtures (console-rs/tools/
dsp-loader/check_resource_contract.py): code -> 0x10012, data -> 0x40011,
the code floor conversion -> 0x10013, a record crossing a page -> 0x10402
in two DMA pieces of at most 4 KiB; the SDK normalization clears bytes 10
and 11 only, the strip form is the 0e9990b recipe's (architecture byte in
the id, word 2 zero).  Replies: 0x80070004, status 0, the id (with the
architecture byte only for the strip form); anything else is refused.
The memspec compiler is checked against the A6.4 strip table and the
tools-wine compile_chain_memspec.py rules (head link, effect-local links,
extents).  ua_plugin_upload() runs on the fake FPGA from test_dsp_xact.py:
an accepted upload makes the module resident in the epoch; a refused or
missing reply leaves it unknown and quarantines the DSP.
"""
import struct

import csrc
import khost
import test_dsp_xact as fake
import test_plugin_catalog as cat


def c_words(name, words):
    return 'static const u32 %s[] = {%s};\n' % (
        name, ','.join('0x%x' % w for w in words))


def sdk_command(blob, dest):
    hdr = (0x10000 if (struct.unpack_from('<I', blob, 8)[0] & 0xff) == 0
           else 0x40000) | (len(blob) // 4 + 2)
    sent = blob[:10] + bytes(2) + blob[12:]
    return struct.pack('<II', hdr, dest) + sent


def strip_command(blob, dest):
    sent = bytearray(blob)
    sent[7] = sent[11]        # arch byte from word 2 into the id word
    sent[8:12] = bytes(4)
    return struct.pack('<II', 0x10000 | (len(blob) // 4 + 2), dest) + bytes(sent)


UPLOAD_GLUE = r'''
static void mk(struct ua_cat_entry *e, const u8 *data, size_t len)
{
	memset(e, 0, sizeof(*e));
	assert(ua_bill_check(data, len, &e->info) == 0);
	e->blob = (u8 *)data;
	e->len = len;
}

/* The DMA stream of transaction @x: its pieces, concatenated */
static size_t stream(struct ua_xact *x, u8 *out)
{
	size_t n = 0;

	for (unsigned int i = 0; i < x->n; i++) {
		u32 *e = x->ent[i];

		if (!(e[0] & UA_RING_DMA_REF))
			continue;
		assert((e[0] & 0xffff) <= 1024);
		assert(e[2] / 4096 == (e[2] + (e[0] & 0xffff) * 4 - 1) / 4096);
		memcpy(out + n, x->stage + e[2], (e[0] & 0xffff) * 4);
		n += (e[0] & 0xffff) * 4;
	}
	return n;
}
'''


def build_upload_test(main, extra=''):
    data = ''.join(cat.c_bytes('f_' + k, v) for k, v in cat.FIXTURES.items())
    cmds = ''.join(
        cat.c_bytes('sdk_' + k, sdk_command(v, 0x1000)) +
        cat.c_bytes('strip_' + k, strip_command(v, 0x1000))
        for k, v in cat.FIXTURES.items())
    code = (khost.PREFIX + fake.FAKE + khost.header('ua_resource_pool.h') +
            khost.header('ua_plugin_host.h') + fake.FPGA + data + cmds + extra +
            khost.body('ua_dsp_xact.c') +
            khost.body('ua_plugin_catalog.c', '/* UA_CATALOG_BEGIN', '/* The entry holding') +
            csrc.extract(csrc.source('ua_plugin_catalog.c'), 'bool ua_catalog_resident(') +
            khost.body('ua_plugin_upload.c') + UPLOAD_GLUE + main)
    return csrc.build_and_run(code, extra=('-fsanitize=address,undefined',
                                           '-fno-sanitize-recover=all',
                                           '-Wno-unused-const-variable'))


PRE = ''


def test_framing_and_normalization():
    build_upload_test(r'''
int main(void)
{
	static u8 out[16384];
	struct ua_cat_entry e;
	struct ua_xact *x;
	size_t n;

	mk(&e, f_code, sizeof(f_code));
	assert(ua_upload_header(&e.info, e.len) == 0x10012);
	x = ua_xact_alloc(0, 16384);
	assert(ua_upload_build(x, &e, 0x1000, UA_BILL_SDK) == 0);
	assert(x->n_rx == 1 && x->rx_words[0] == 4 && x->n == 1);
	n = stream(x, out);
	assert(n == sizeof(sdk_code) && !memcmp(out, sdk_code, n));
	assert(!memcmp(e.blob, f_code, e.len));		/* the original stays */
	ua_xact_discard(&dev, x);

	mk(&e, f_data, sizeof(f_data));
	assert(ua_upload_header(&e.info, e.len) == 0x40011);
	x = ua_xact_alloc(0, 16384);
	assert(ua_upload_build(x, &e, 0x1000, UA_BILL_SDK) == 0);
	n = stream(x, out);
	assert(n == sizeof(sdk_data) && !memcmp(out, sdk_data, n));
	ua_xact_discard(&dev, x);

	mk(&e, f_floor, sizeof(f_floor));
	assert(ua_upload_header(&e.info, e.len) == 0x10013);

	/* a record crossing a page: one command, two pieces, never 1 KiB */
	mk(&e, f_page, sizeof(f_page));
	assert(ua_upload_header(&e.info, e.len) == 0x10402);
	x = ua_xact_alloc(0, 16384);
	assert(ua_upload_build(x, &e, 0x1000, UA_BILL_SDK) == 0);
	assert(x->n == 2 && (x->ent[0][0] & 0xffff) == 1024 &&
	       (x->ent[1][0] & 0xffff) == 2);
	n = stream(x, out);
	assert(n == sizeof(sdk_page) && !memcmp(out, sdk_page, n));
	ua_xact_discard(&dev, x);

	/* the strips' form: the 0e9990b recipe's bytes */
	mk(&e, f_strip, sizeof(f_strip));
	x = ua_xact_alloc(0, 16384);
	assert(ua_upload_build(x, &e, 0x1000, UA_BILL_STRIP) == 0);
	n = stream(x, out);
	assert(n == sizeof(strip_strip) && !memcmp(out, strip_strip, n));
	assert(out[8 + 7] == 2 && !out[8 + 8] && !out[8 + 11]);
	ua_xact_discard(&dev, x);
	assert(!k_objects);
	return 0;
}
''', PRE)


def test_reply_rules():
    build_upload_test(r'''
int main(void)
{
	struct ua_cat_entry e;
	u32 r[4] = { 0x80070004, 0, 0xeb, 0x100b5 };
	bool echo;

	mk(&e, f_strip, sizeof(f_strip));
	assert(ua_upload_reply_check(r, &e, UA_BILL_SDK, 0x100b5, &echo) == 0 && echo);
	assert(ua_upload_reply_check(r, &e, UA_BILL_SDK, 0x100b6, &echo) == 0 && !echo);
	/* the strip form may be answered with the id as sent */
	r[2] = 0x020000eb;
	assert(ua_upload_reply_check(r, &e, UA_BILL_STRIP, 0x100b5, NULL) == 0);
	assert(ua_upload_reply_check(r, &e, UA_BILL_SDK, 0x100b5, NULL) == -EPROTO);
	r[2] = 0xeb;
	assert(ua_upload_reply_check(r, &e, UA_BILL_STRIP, 0x100b5, NULL) == 0);
	r[2] = 0xec;
	assert(ua_upload_reply_check(r, &e, UA_BILL_SDK, 0x100b5, NULL) == -EPROTO);
	r[2] = 0xeb;
	r[1] = 1;
	assert(ua_upload_reply_check(r, &e, UA_BILL_SDK, 0x100b5, NULL) == -EPROTO);
	r[1] = 0;
	r[0] = 0x80030302;
	assert(ua_upload_reply_check(r, &e, UA_BILL_SDK, 0x100b5, NULL) == -EPROTO);
	r[0] = 0;
	assert(ua_upload_reply_check(r, &e, UA_BILL_SDK, 0x100b5, NULL) == -EPROTO);
	return 0;
}
''', PRE)


# A6.4: the first strip's memspec, from the strip test's table
STRIP_MEMSPEC = [0x00150013, 0x9d1b6, 0x9d178, 0x9d1ae, 0xc6fee, 0x9d1af,
                 0x9d168, 0x9d193, 0x8ffdff0, 0x9d194, 0x9d164, 0x9d186,
                 0x8ffdef0, 0x9d187, 0x8ffddf0, 0x9d17e, 0x8ffdcf0, 0x9d17f,
                 0x8ffdbf0]


def test_memspec_compiler():
    build_upload_test(c_words('want_strip', STRIP_MEMSPEC) + r'''
int main(void)
{
	/* the Mic strip: head (instance header) + sub-plug-in rows m0-m4, c0-c3 */
	static const struct ua_memspec_row strip[10] = {
		{ 1, 0x008, 0xffffffff, 0 },
		{ 1, 0x038, 0x00000006, 0 },	/* m0 -> head row 0 + 6 */
		{ 2, 0x010, 0x00000036, 0 },	/* m1 -> m0 + 0x36 */
		{ 1, 0x010, 0x00000037, 0 },
		{ 3, 0x010, 0x0000001b, 0 },
		{ 1, 0x004, 0x0000001c, 0 },
		{ 3, 0x100, 0x0000000e, 0 },
		{ 3, 0x100, 0x0000000f, 0 },
		{ 3, 0x100, 0x00000006, 0 },
		{ 3, 0x100, 0x00000007, 0 },
	};
	static const u32 addr[10] = { 0x9d1b0, 0x9d178, 0xc6fee, 0x9d168,
		0x8ffdff0, 0x9d164, 0x8ffdef0, 0x8ffddf0, 0x8ffdcf0, 0x8ffdbf0 };
	static const unsigned int desc[2] = { 0, 1 };
	u32 w[2 * UA_MEMSPEC_ROWS_MAX];
	struct ua_memspec_row rows[10];
	u32 a[10];

	assert(ua_memspec_compile(strip, 10, desc, 2, addr, w) == 19);
	assert(!memcmp(w, want_strip, sizeof(want_strip)));

	/* compile_chain_memspec.py's cross-chain case: a one-row head, an
	 * effect whose first row links into the head (offset 6), records
	 * 14/15/6/7 that link into the effect's own first row */
	{
		struct ua_memspec_row c[6] = {
			{ 1, 8, 0xffffffff, 0 }, { 1, 0x20, 6, 0 },
			{ 3, 2, 14, 2 }, { 3, 2, 15, 2 }, { 3, 2, 6, 2 }, { 3, 2, 7, 2 } };
		u32 ad[6], out[11];

		for (int i = 0; i < 6; i++)
			ad[i] = 0x1000 + 0x100 * i;
		assert(ua_memspec_compile(c, 6, desc, 2, ad, out) == 11);
		assert(out[0] == 0x0015000b && out[1] == 0x1006 && out[2] == 0x1100);
		assert(out[3] == 0x110e && out[5] == 0x110f && out[7] == 0x1106 &&
		       out[9] == 0x1107 && out[10] == 0x1500);
		/* one descriptor: a first row links within itself */
		static const unsigned int one[1] = { 0 };
		c[1].link = 0x00000003;
		assert(ua_memspec_compile(c, 2, one, 1, ad, out) == 3 &&
		       out[1] == 0x1003 && out[2] == 0x1100);
	}

	/* refusals */
#define BAD(stmt) do { memcpy(rows, strip, sizeof(rows)); memcpy(a, addr, sizeof(a)); \
		stmt; assert(ua_memspec_compile(rows, 10, desc, 2, a, w) == -EINVAL); } while (0)
	BAD(rows[0].link = 0);				/* the head's first row has a link */
	BAD(rows[3].link = 0xffffffff);			/* any other row lacks one */
	BAD(rows[1].link = 0x01000006);			/* head has one row */
	BAD(rows[1].link = 0x00000008);			/* offset past the head row */
	BAD(rows[2].link = 0x00000038);			/* offset past m0 */
	BAD(rows[2].link = 0x09000000);			/* target past the effect */
	BAD(rows[4].pool = 0);
	BAD(rows[4].pool = 4);
	BAD(rows[4].flags = 1);
	BAD(rows[4].dwords = 0);
	BAD(a[3] = 0xffffffff);
	BAD(a[3] = 0x9d169);				/* odd */
	BAD(a[9] = 0xffffff00);				/* past 32 bits */
	{
		unsigned int d2[2] = { 1, 2 };

		memcpy(rows, strip, sizeof(rows));
		assert(ua_memspec_compile(rows, 10, d2, 2, addr, w) == -EINVAL);
		d2[0] = 0; d2[1] = 0;
		assert(ua_memspec_compile(rows, 10, d2, 2, addr, w) == -EINVAL);
		d2[1] = 10;
		assert(ua_memspec_compile(rows, 10, d2, 2, addr, w) == -EINVAL);
	}
	assert(ua_memspec_compile(strip, 0, desc, 1, addr, w) == -EINVAL);
	assert(ua_memspec_compile(strip, UA_MEMSPEC_ROWS_MAX + 1, desc, 1, addr, w) == -EINVAL);
	return 0;
}
''', PRE)


def test_release_words():
    build_upload_test(r'''
int main(void)
{
	struct ua_cat_entry e;
	struct ua_xact *x = ua_xact_alloc(0, 0);

	mk(&e, f_strip, sizeof(f_strip));
	ua_release_build(x, &e.info);
	mk(&e, f_data, sizeof(f_data));
	ua_release_build(x, &e.info);
	e.info.kind = 4;
	ua_release_build(x, &e.info);
	assert(x->n == 3);
	assert(x->ent[0][0] == 0x00030002 && x->ent[0][1] == 0xeb && !x->ent[0][2]);
	assert(x->ent[1][0] == 0x00060003 && x->ent[1][1] == 0xF00002 && x->ent[1][2] == 1);
	assert(x->ent[2][2] == 1);			/* kind 4 releases as 1 */
	ua_xact_discard(&dev, x);
	return 0;
}
''', PRE)


def test_upload_waits_for_the_reply():
    build_upload_test(r'''
int main(void)
{
	struct ua_cat_entry e;

	reset();
	host.mutex.held = 1;
	mk(&e, f_strip, sizeof(f_strip));
	/* accepted: resident in this epoch */
	assert(ua_plugin_upload(&dev, 0, &e, 0xe0000, UA_BILL_STRIP, host.epoch) == 0);
	assert(ua_catalog_resident(&e, 0, host.epoch) && e.res[0].base == 0xe0000);
	assert(host.dsp[0].uploads == 1 && host.dsp[0].replies_ok == 1);
	assert(!quarantined[0] && !sleeps_locked);

	/* refused by the DSP: unknown, quarantined */
	reply_status = 0xf0010006;
	assert(ua_plugin_upload(&dev, 0, &e, 0xe0000, UA_BILL_SDK, host.epoch) == -EPROTO);
	assert(e.res[0].state == UA_RES_UNKNOWN && quarantined[0] == 1);
	assert(host.dsp[0].replies_bad == 1);
	reply_status = 0;

	/* no reply in time: unknown, quarantined */
	stalled = true;
	assert(ua_plugin_upload(&dev, 0, &e, 0xe0000, UA_BILL_SDK, host.epoch) == -ETIMEDOUT);
	assert(e.res[0].state == UA_RES_UNKNOWN && quarantined[0] == 2);

	/* never published (stale epoch): nothing changes */
	e.res[0].state = UA_RES_NONE;
	assert(ua_plugin_upload(&dev, 0, &e, 0xe0000, UA_BILL_SDK, host.epoch - 1) == -ESTALE);
	assert(e.res[0].state == UA_RES_NONE && quarantined[0] == 2);
	stalled = false;
	fpga_tick();
	mutex_lock(&dev.lock);
	ua_xact_reclaim(&dev);
	mutex_unlock(&dev.lock);
	assert(!host.xact_count && !k_objects);
	return 0;
}
''', PRE)


def test_upload_is_wired():
    assert 'ua_plugin_upload.o' in csrc.source('Makefile')
