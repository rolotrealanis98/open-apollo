"""ua_dsp_load_mixer_blocks(): a malformed block anywhere in the table is
refused before block 0 is sent.

The loader, its table check and ua_sendblock_chunk() are compiled as they
are; request_firmware() hands them an in-memory blob and ua_dsp_send_block()
records what would reach DSP 0.  Set UA_MIXER_FIRMWARE_FIXTURE to a real
ua-apollo-mixer.bin to also check that it passes.  Adapted from the parallel
Codex work (test_mixer_firmware_preflight); the DSPs 1-3 loader it also
tested is gone from driver-v2.
"""
import os
import csrc

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
typedef uint32_t __le32;
#define UA_MIXER_FW_MAGIC 0x5541464D
#define UA_MIXER_FW_VERSION 1
#define UA_MIXER_FW_NAME "ua-apollo-mixer.bin"
#define UA_FW_CMD 0x00120000
#define UA_FW_PARAM 0x80040000
#define UA_FW_MAX_SIZE (4 * 1024 * 1024)
#define UA_RING_NUM_PAGES 4
#define UA_RING_ENTRIES_PER_PAGE 256
#define DIV_ROUND_UP(n, d) (((n) + (d) - 1) / (d))
struct firmware { size_t size; const u8 *data; };
struct pci_dev { int dev; };
struct ua_dspx { bool replay_pending; };
struct ua_device { struct pci_dev *pdev; bool fw_loaded; struct ua_dspx dspx; };
static unsigned int sendblock_chunk = 1024;
static struct firmware fixture;
static int requests, releases, sends, send_result;
static u32 le32_to_cpup(const __le32 *p) { u32 v; memcpy(&v, p, 4); return v; }
static void log_(int *d, const char *fmt, ...) { (void)d; (void)fmt; }
#define dev_err log_
#define dev_warn log_
#define dev_info log_
#define dev_dbg log_
static int request_firmware(const struct firmware **fw, const char *n, int *d)
{ (void)n; (void)d; requests++; *fw = &fixture; return 0; }
static void release_firmware(const struct firmware *fw)
{ assert(fw == &fixture); releases++; }
static int ua_dsp_send_block(struct ua_device *ua, unsigned int dsp, u32 cmd,
			     u32 param, const void *data, size_t size)
{
	(void)ua;
	assert(dsp == 0 && cmd == UA_FW_CMD && param == UA_FW_PARAM);
	assert((sends == 0 && size == 4 && !memcmp(data, "ABCD", 4)) ||
	       (sends == 1 && size == 8 && !memcmp(data, "EFGHIJKL", 8)));
	sends++;
	return send_result;
}
'''

MAIN = r'''
static void word(u8 *p, u32 v) { memcpy(p, &v, 4); }
static int load(struct ua_device *ua)
{
	sends = 0;
	ua->fw_loaded = false;
	return ua_dsp_load_mixer_blocks(ua);
}

int main(void)
{
	struct pci_dev pci = { 0 };
	struct ua_device ua = { .pdev = &pci };
	u8 blob[52], good[52];
	unsigned int bad;

#ifdef FIXTURE
	{	/* a real ua-apollo-mixer.bin passes the check */
		FILE *f = fopen(FIXTURE, "rb");
		long n;
		u8 *bytes;

		assert(f);
		fseek(f, 0, SEEK_END);
		n = ftell(f);
		rewind(f);
		bytes = malloc(n);
		assert(bytes && fread(bytes, 1, n, f) == (size_t)n);
		fclose(f);
		fixture = (struct firmware){ n, bytes };
		assert(ua_dsp_mixer_fw_check(&ua, &fixture) > 0);
		free(bytes);
	}
#endif

	/* header, 2 blocks (4 and 8 bytes at 40 and 44), payload */
	memset(blob, 0, sizeof(blob));
	word(blob, UA_MIXER_FW_MAGIC); word(blob + 4, 1); word(blob + 8, 2);
	word(blob + 16, 0x100); word(blob + 20, 4); word(blob + 24, 40);
	word(blob + 28, 0x200); word(blob + 32, 8); word(blob + 36, 44);
	memcpy(blob + 40, "ABCDEFGHIJKL", 12);
	memcpy(good, blob, sizeof(blob));
	fixture = (struct firmware){ sizeof(blob), blob };

	assert(load(&ua) == 0 && sends == 2 && ua.fw_loaded &&
	       ua.dspx.replay_pending);

	/* Every defect, in the header or the LAST block: nothing is sent */
	for (bad = 0; bad < 10; bad++) {
		memcpy(blob, good, sizeof(blob));
		fixture.size = sizeof(blob);
		switch (bad) {
		case 0: word(blob, 0x12345678); break;		/* magic */
		case 1: word(blob + 4, 2); break;		/* version */
		case 2: word(blob + 8, 0); break;		/* no blocks */
		case 3: word(blob + 8, 65); break;		/* too many */
		case 4: fixture.size = 39; break;		/* table cut */
		case 5: word(blob + 36, 0xfffffff0); break;	/* wraps */
		case 6: word(blob + 32, 7); break;		/* not dwords */
		case 7: word(blob + 36, 32); break;		/* inside table */
		case 8: word(blob + 32, 12); break;		/* past the end */
		case 9: fixture.size = 15; break;		/* no header */
		}
		assert(load(&ua) == -EINVAL && !sends && !ua.fw_loaded);
	}

	/* A late block send_block would refuse for its size: E2BIG first */
	{
		size_t big = 1024 * 1024 + 4;
		u8 *b = calloc(1, 44 + big);

		assert(b);
		memcpy(b, good, sizeof(good));
		word(b + 32, (u32)big);
		fixture = (struct firmware){ 44 + big, b };
		assert(load(&ua) == -E2BIG && !sends && !ua.fw_loaded);
		free(b);
	}

	/* A send that fails stops the load there */
	memcpy(blob, good, sizeof(blob));
	fixture = (struct firmware){ sizeof(blob), blob };
	send_result = -ETIMEDOUT;
	assert(load(&ua) == -ETIMEDOUT && sends == 1 && !ua.fw_loaded);
	assert(requests == releases);
	return 0;
}
'''


def test_whole_table_is_checked_before_the_first_block():
    dsp = csrc.source('ua_dsp.c')
    code = '\n'.join((
        csrc.extract(dsp, 'static size_t ua_sendblock_chunk('),
        csrc.between(dsp, '#define UA_MIXER_FW_HDR_SIZE',
                     'static int ua_dsp_mixer_fw_check('),
        csrc.extract(dsp, 'static int ua_dsp_mixer_fw_check('),
        csrc.extract(dsp, 'int ua_dsp_load_mixer_blocks('),
    ))
    fixture = os.environ.get('UA_MIXER_FIRMWARE_FIXTURE')
    define = '#define FIXTURE "%s"\n' % fixture if fixture else ''
    csrc.build_and_run(define + PREFIX + code + MAIN)
