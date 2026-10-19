"""ua_plugin_catalog.c: DSP modules read by the driver from the store (A3).

The file is compiled whole with a fake /lib/firmware and a plain C SHA-256.
Fixtures are synthetic patterned "Bill" records in the shapes the
dsp-loader contract checker uses (console-rs/tools/dsp-loader/
check_resource_contract.py: code, data, the code floor conversion, a record
crossing a 4 KiB page); none is a real module and none is ever uploaded.
Checks the IsValid rules, the name-is-the-hash rule, user references and
dedup by content, the system (strip) path with its fallback directory, the
catalog bound, and residency keyed to the epoch (review finding 14).
"""
import hashlib
import struct

import csrc
import khost


def fixture(ident, kind, arch, functional, total, flags=0):
    """A synthetic record: header as UA's tools write it, patterned body."""
    return (struct.pack('<4s4I', b'Bill', ident, kind | flags << 8 | arch << 24,
                        total - 20, functional) +
            bytes((i * 17 + 3) & 255 for i in range(total - 20)))


FIXTURES = {
    'code': fixture(0xF00001, 0, 2, 3, 64),
    'data': fixture(0xF00002, 1, 0, 2, 60),
    'floor': fixture(0xF00003, 0, 2, 4, 68),
    'page': fixture(0xF00004, 0, 2, 3, 4096),
    'strip': fixture(0x0EB, 0, 2, 9, 716),
}


def c_bytes(name, data):
    return 'static const u8 %s[] = {%s};\n' % (name, ','.join(map(str, data)))


def c_hash(name, data):
    return 'static const u8 %s[32] = {%s};\n' % (
        name, ','.join(map(str, hashlib.sha256(data).digest())))


SHA = r'''
/* A plain SHA-256 for the host (the kernel's lib/crypto in the module) */
static const u32 K256[64] = {
0x428a2f98,0x71374491,0xb5c0fbcf,0xe9b5dba5,0x3956c25b,0x59f111f1,0x923f82a4,0xab1c5ed5,
0xd807aa98,0x12835b01,0x243185be,0x550c7dc3,0x72be5d74,0x80deb1fe,0x9bdc06a7,0xc19bf174,
0xe49b69c1,0xefbe4786,0x0fc19dc6,0x240ca1cc,0x2de92c6f,0x4a7484aa,0x5cb0a9dc,0x76f988da,
0x983e5152,0xa831c66d,0xb00327c8,0xbf597fc7,0xc6e00bf3,0xd5a79147,0x06ca6351,0x14292967,
0x27b70a85,0x2e1b2138,0x4d2c6dfc,0x53380d13,0x650a7354,0x766a0abb,0x81c2c92e,0x92722c85,
0xa2bfe8a1,0xa81a664b,0xc24b8b70,0xc76c51a3,0xd192e819,0xd6990624,0xf40e3585,0x106aa070,
0x19a4c116,0x1e376c08,0x2748774c,0x34b0bcb5,0x391c0cb3,0x4ed8aa4a,0x5b9cca4f,0x682e6ff3,
0x748f82ee,0x78a5636f,0x84c87814,0x8cc70208,0x90befffa,0xa4506ceb,0xbef9a3f7,0xc67178f2};
#define ROR(x, n) (((x) >> (n)) | ((x) << (32 - (n))))
static void sha_block(u32 *h, const u8 *p)
{
	u32 w[64], a, b, c, d, e, f, g, k, t1, t2;
	int i;

	for (i = 0; i < 16; i++)
		w[i] = (u32)p[4*i] << 24 | (u32)p[4*i+1] << 16 | (u32)p[4*i+2] << 8 | p[4*i+3];
	for (; i < 64; i++)
		w[i] = (ROR(w[i-2], 17) ^ ROR(w[i-2], 19) ^ (w[i-2] >> 10)) + w[i-7] +
		       (ROR(w[i-15], 7) ^ ROR(w[i-15], 18) ^ (w[i-15] >> 3)) + w[i-16];
	a = h[0]; b = h[1]; c = h[2]; d = h[3]; e = h[4]; f = h[5]; g = h[6]; k = h[7];
	for (i = 0; i < 64; i++) {
		t1 = k + (ROR(e, 6) ^ ROR(e, 11) ^ ROR(e, 25)) + ((e & f) ^ (~e & g)) + K256[i] + w[i];
		t2 = (ROR(a, 2) ^ ROR(a, 13) ^ ROR(a, 22)) + ((a & b) ^ (a & c) ^ (b & c));
		k = g; g = f; f = e; e = d + t1; d = c; c = b; b = a; a = t1 + t2;
	}
	h[0] += a; h[1] += b; h[2] += c; h[3] += d; h[4] += e; h[5] += f; h[6] += g; h[7] += k;
}
static void sha256(const u8 *data, size_t len, u8 out[32])
{
	u32 h[8] = { 0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
		     0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19 };
	u8 tail[128] = { 0 };
	size_t i, n = len / 64 * 64, t;
	u64 bits = (u64)len * 8;

	for (i = 0; i < n; i += 64)
		sha_block(h, data + i);
	t = len - n;
	memcpy(tail, data + n, t);
	tail[t] = 0x80;
	t = t < 56 ? 64 : 128;
	for (i = 0; i < 8; i++)
		tail[t - 1 - i] = (u8)(bits >> (8 * i));
	sha_block(h, tail);
	if (t == 128)
		sha_block(h, tail + 64);
	for (i = 0; i < 32; i++)
		out[i] = (u8)(h[i / 4] >> (24 - 8 * (i % 4)));
}
static char *bin2hex(char *dst, const void *src, size_t n)
{
	const u8 *s = src;

	for (size_t i = 0; i < n; i++)
		dst += sprintf(dst, "%02x", s[i]);
	return dst;
}
'''

FAKE = r'''
#define UA_MAX_DSPS 8
struct delayed_work { int unused; };
struct device { int unused; };
struct pci_dev { struct device dev; };
struct ua_plugin_host;
struct ua_device {
	struct pci_dev *pdev;
	struct mutex lock;
	struct ua_plugin_host *host;
};
struct firmware { size_t size; const u8 *data; };
struct disk { const char *name; const u8 *data; size_t len; };
static struct disk disk[16];
static unsigned int ndisk, reads;
static char last_name[128];
static int firmware_request_nowarn(const struct firmware **fw, const char *name,
				   struct device *d)
{
	(void)d;
	reads++;
	snprintf(last_name, sizeof(last_name), "%s", name);
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
'''


def run(main, extra=''):
    data = ''.join(c_bytes('f_' + k, v) + c_hash('h_' + k, v)
                   for k, v in FIXTURES.items())
    names = ''.join('static const char n_%s[] = "universal-audio/plugins/modules/%s.bill";\n'
                    % (k, hashlib.sha256(v).hexdigest()) for k, v in FIXTURES.items())
    code = (khost.PREFIX + FAKE + khost.header('ua_resource_pool.h') +
            khost.header('ua_plugin_host.h') + SHA + data + names + extra +
            khost.body('ua_plugin_catalog.c') + main)
    return csrc.build_and_run(code, extra=('-fsanitize=address,undefined',
                                           '-fno-sanitize-recover=all',
                                           '-Wno-unused-const-variable'))


def test_bill_rules():
    run(r'''
static u8 tmp[8192];
static int with(const u8 *src, size_t len, unsigned int at, u32 word)
{
	memcpy(tmp, src, len);
	put_unaligned_le32(word, tmp + at);
	return ua_bill_check(tmp, len, NULL);
}

int main(void)
{
	struct ua_bill_info b;

	assert(ua_bill_check(f_code, sizeof(f_code), &b) == 0);
	assert(b.id == 0xF00001 && b.kind == 0 && b.arch == 2 && b.flags == 0 &&
	       b.w4 == 3 && b.words == 2 && b.pool == 0);
	assert(ua_bill_check(f_data, sizeof(f_data), &b) == 0);
	assert(b.kind == 1 && b.arch == 0 && b.words == 2 && b.pool == 1);
	/* floor(2 * 4 / 3) = 2: divisibility by three is not required */
	assert(ua_bill_check(f_floor, sizeof(f_floor), &b) == 0 && b.words == 2);
	assert(ua_bill_check(f_page, sizeof(f_page), &b) == 0 && b.words == 2);
	assert(ua_bill_check(f_strip, sizeof(f_strip), &b) == 0 &&
	       b.id == 0xeb && b.words == 6);

	/* kind 4 is data memory (pool 1), kinds 2 and 3 their own pools */
	assert(with(f_data, sizeof(f_data), 8, 4) == 0);
	memcpy(tmp, f_data, sizeof(f_data));
	put_unaligned_le32(3, tmp + 8);
	assert(ua_bill_check(tmp, sizeof(f_data), &b) == 0 && b.pool == 3);

	assert(with(f_code, 64, 0, 0x6c6c6943) == -EINVAL);	/* magic */
	assert(with(f_code, 64, 4, 0x020000eb) == -EINVAL);	/* arch in the id */
	assert(with(f_code, 64, 8, 5) == -EINVAL);		/* kind 5 */
	assert(with(f_code, 64, 8, 0x03000000) == -EINVAL);	/* arch 3 */
	assert(with(f_code, 64, 8, 0x02010000) == -EINVAL);	/* code with flags */
	assert(with(f_data, 60, 8, 0x00010001) == 0);		/* data may have flags */
	assert(with(f_code, 64, 12, 40) == -EINVAL);		/* payload length */
	assert(with(f_code, 64, 16, 0) == -EINVAL);		/* w4 0 */
	assert(with(f_code, 64, 16, 4) == -EINVAL);		/* w4 > words - 13 */
	assert(with(f_page, 4096, 16, 5) == -EINVAL);		/* odd code words */
	assert(with(f_page, 4096, 16, 1) == -EINVAL);		/* no code words */
	assert(ua_bill_check(f_code, 60, NULL) == -EINVAL);	/* truncated */
	assert(ua_bill_check(f_code, 62, NULL) == -EINVAL);
	assert(ua_bill_check(f_code, 52, NULL) == -EINVAL);	/* too short */
	assert(ua_bill_check(NULL, 64, NULL) == -EINVAL);
	assert(ua_bill_check(f_code, UA_BILL_MAX + 4, NULL) == -EINVAL);
	/* any alignment */
	memcpy(tmp + 1, f_code, 64);
	assert(ua_bill_check(tmp + 1, 64, &b) == 0 && b.id == 0xF00001);
	return 0;
}
''')


GLUE = r'''
static struct ua_device dev;
static struct ua_plugin_host host;
static struct pci_dev pci;
static void setup(void)
{
	memset(&dev, 0, sizeof(dev));
	memset(&host, 0, sizeof(host));
	dev.pdev = &pci;
	dev.host = &host;
	INIT_LIST_HEAD(&host.catalog);
	ndisk = reads = 0;
	k_log_reset();
}
static void put(struct ua_cat_entry *e)
{
	mutex_lock(&dev.lock);
	ua_catalog_put(&dev, e);
	mutex_unlock(&dev.lock);
}
#define DISK(n, d) (disk[ndisk++] = (struct disk){ (n), (d), sizeof(d) })
'''


def test_store_reads_by_hash():
    run(GLUE + r'''
int main(void)
{
	struct ua_cat_entry *a, *b, *c;
	u8 wrong[32];

	setup();
	DISK(n_code, f_code);
	DISK(n_data, f_data);
	assert(ua_catalog_get(&dev, h_code, &a) == 0);
	assert(!strcmp(last_name, n_code));
	assert(a->users == 1 && a->len == 64 && !memcmp(a->blob, f_code, 64) &&
	       !memcmp(a->sha256, h_code, 32) && a->info.id == 0xF00001);
	assert(host.catalog_count == 1 && !dev.lock.held);
	/* the second user gets the same copy without reading the store */
	reads = 0;
	assert(ua_catalog_get(&dev, h_code, &b) == 0 && b == a && a->users == 2 && !reads);
	assert(ua_catalog_get(&dev, h_data, &c) == 0 && c != a && host.catalog_count == 2);

	/* not in the store */
	assert(ua_catalog_get(&dev, h_floor, &b) == -ENOENT && !b);
	/* a file whose content is not its name */
	disk[ndisk++] = (struct disk){ n_floor, f_code, sizeof(f_code) };
	assert(ua_catalog_get(&dev, h_floor, &b) == -EBADMSG && !b);
	assert(K_HAS("its SHA-256 is not its name"));
	/* a file that is not a valid record (right name, wrong content) */
	memcpy(wrong, h_page, 32);
	{
		static u8 bad[4096];

		memcpy(bad, f_page, sizeof(bad));
		bad[8 + 2] = 1;		/* code with flags */
		sha256(bad, sizeof(bad), wrong);
		static char name[128];
		char hex[65];

		*bin2hex(hex, wrong, 32) = 0;
		snprintf(name, sizeof(name), "universal-audio/plugins/modules/%s.bill", hex);
		disk[ndisk++] = (struct disk){ name, bad, sizeof(bad) };
		assert(ua_catalog_get(&dev, wrong, &b) == -EINVAL && !b);
	}
	assert(host.catalog_count == 2);

	put(a);
	assert(host.catalog_count == 2 && a->users == 1);
	put(a);
	put(c);
	assert(host.catalog_count == 0 && !k_objects);
	return 0;
}
''')


def test_strip_modules_and_fallback():
    run(GLUE + r'''
int main(void)
{
	struct ua_cat_entry *e, *f;

	setup();
	assert(ua_catalog_get_system(&dev, 0xeb, &e) == -ENOENT && !e);
	assert(!strcmp(last_name, "ua_apollo/dsp/0eb.bill"));
	/* the older tool's directory is still read */
	disk[ndisk++] = (struct disk){ "ua_apollo/dsp/0eb.bill", f_strip, sizeof(f_strip) };
	assert(ua_catalog_get_system(&dev, 0xeb, &e) == 0 && e->info.id == 0xeb);
	/* the oa-assets directory first; same bytes, same entry */
	disk[ndisk++] = (struct disk){ "universal-audio/dsp/0eb.bill", f_strip, sizeof(f_strip) };
	assert(ua_catalog_get_system(&dev, 0xeb, &f) == 0 && f == e && e->users == 2);
	assert(!strcmp(last_name, "universal-audio/dsp/0eb.bill"));
	/* a file under another id's name is refused */
	disk[ndisk++] = (struct disk){ "universal-audio/dsp/0da.bill", f_strip, sizeof(f_strip) };
	assert(ua_catalog_get_system(&dev, 0xda, &f) == -EINVAL && !f);
	assert(ua_catalog_get_system(&dev, 0x1000, &f) == -EINVAL);
	put(e);
	put(e);
	assert(!host.catalog_count && !k_objects);
	return 0;
}
''')


def test_bound_and_epoch_keyed_residency():
    run(GLUE + r'''
int main(void)
{
	struct ua_cat_entry *e, *x;

	setup();
	DISK(n_code, f_code);
	assert(ua_catalog_get(&dev, h_code, &e) == 0);
	/* resident on DSP 2 in epoch 5 only: after an epoch change it is not */
	e->res[2] = (struct ua_cat_res){ .epoch = 5, .state = UA_RES_RESIDENT, .base = 0xe0000 };
	assert(ua_catalog_resident(e, 2, 5));
	assert(!ua_catalog_resident(e, 2, 6));
	assert(!ua_catalog_resident(e, 1, 5));
	assert(!ua_catalog_resident(e, 99, 5));
	e->res[2].state = UA_RES_UNKNOWN;
	assert(!ua_catalog_resident(e, 2, 5));

	/* the catalog is bounded */
	host.catalog_count = UA_CATALOG_MAX;
	DISK(n_data, f_data);
	assert(ua_catalog_get(&dev, h_data, &x) == -ENOSPC && !x);
	host.catalog_count = 1;
	put(e);
	assert(!host.catalog_count && !k_objects);
	return 0;
}
''')


def test_no_user_bytes_reach_the_catalog():
    src = csrc.source('ua_plugin_catalog.c')
    assert 'copy_from_user' not in src
    assert 'request_firmware' in src or 'firmware_request_nowarn' in src
    assert '"universal-audio/plugins/modules/%s.bill"' in src
    assert 'ua_plugin_catalog.o' in csrc.source('Makefile')
