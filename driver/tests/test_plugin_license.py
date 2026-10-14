"""The licensing gate (tasks L1 and L2): the device's own answer, read fresh,
decided as UA's SDK decides, refusing before anything is uploaded.

L1 (ua_plugin_license.c):
  - the date word against the 1006 samples of tools-wine
    windows_snapshot_query.py's arithmetic test (PCIe 1086c/eb78), a clock
    before 2007 refused instead of encoded as 0;
  - the board query on the fake FPGA: reply descriptor first (0x80000302,
    prefilled with the 0x83000000 sentinel), then {0x00100002, date} and
    {0x00110002, echo}; the table passed through unmodified; a wrong
    header, a wrong echo, no reply, an epoch change, plugin_host=0 all
    fail, and a failed query does not quarantine DSP 0.
L2 (ua_plugin_instance.c over the whole host): CREATE asks the device
first and places nothing unless the answer admits.  Every state of plan
section 5, an id outside the table, a failed query and an epoch change
between the answer and the placement refuse with no module read, no
upload, no ledger change; admitted plug-ins are placed and acknowledged;
DESTROY and closing the file release them; CHECK reads the answer only.
Tables in these tests are synthetic: they test the parser and the
decision, nothing else.
"""
import random

import csrc
import khost
import test_dsp_xact as fake
import test_input_strips as strips

ORIGIN = 12812083200        # 2007-01-01 in Windows seconds (since 1601)
UNIX_FROM_1601 = 11644473600


def date_samples():
    """windows_snapshot_query.py's 1006 tick values and its formula."""
    rng = random.Random(1086)
    values = [0, ORIGIN * 10**7 - 1, ORIGIN * 10**7, ORIGIN * 10**7 + 599999999,
              ORIGIN * 10**7 + 600000000, (ORIGIN + 86400) * 10**7]
    values += [rng.randrange(1 << 63) for _ in range(1000)]
    out = []
    for t in values:
        secs = t // 10**7
        s = max(secs - ORIGIN, 0)
        day, within = divmod(s, 86400)
        want = ((day << 12) + within // 60) & 0xffffffff
        out.append((secs - UNIX_FROM_1601, want, secs >= ORIGIN))
    assert len(out) == 1006
    return out


def test_date_word_matches_the_original_arithmetic():
    rows = date_samples()
    unix = ','.join('%dLL' % u for u, _, _ in rows)
    want = ','.join('0x%xu' % w for _, w, _ in rows)
    ok = ','.join('1' if v else '0' for _, _, v in rows)
    lic = csrc.source('ua_plugin_license.c')
    fn = csrc.extract(lic, 'int ua_plugin_date(')
    csrc.build_and_run(khost.PREFIX + r'''
#define UA_DATE_EPOCH 1167609600LL
static u64 div_u64_rem(u64 n, u32 d, u32 *rem) { *rem = (u32)(n % d); return n / d; }
''' + fn + '''
static const s64 unix_s[] = {%s};
static const u32 want[] = {%s};
static const int valid[] = {%s};
int main(void)
{
	unsigned int n = 0;

	for (unsigned int i = 0; i < sizeof(unix_s) / sizeof(unix_s[0]); i++) {
		u32 d = 0xdeadbeef;
		int r = ua_plugin_date(unix_s[i], &d);

		if (!valid[i]) {
			/* before 2007: refused, never encoded as 0 */
			assert(r == -ERANGE && d == 0xdeadbeef);
			continue;
		}
		assert(r == 0 && d == want[i]);
		n++;
	}
	assert(n > 950);
	/* 2026-10-13 12:00 UTC: day 7225, minute 720 */
	{
		u32 d;

		assert(ua_plugin_date(1791892800LL, &d) == 0);
		assert(d == ((7225u << 12) + 720));
	}
	return 0;
}
''' % (unix, want, ok))


def test_states_are_the_sdks():
    lic = csrc.source('ua_plugin_license.c')
    code = (khost.PREFIX + '#define UA_PLUGIN_LICENSE_AUTHORIZED 0u\n'
            '#define UA_PLUGIN_LICENSE_DEMO 1u\n#define UA_PLUGIN_LICENSE_NOT_STARTED 2u\n'
            '#define UA_PLUGIN_LICENSE_EXPIRED 3u\n#define UA_PLUGIN_LICENSE_UNAVAILABLE 4u\n'
            '#define UA_PLUGIN_LICENSE_UNKNOWN 0xffffffffu\n' +
            csrc.extract(lic, 'u32 ua_plugin_license_state(') +
            csrc.extract(lic, 'bool ua_plugin_license_admits(') + r'''
int main(void)
{
	static const struct { u32 word, state; bool admit; } t[] = {
		{ 0x81000000, 0, true }, { 0x82000000, 0, true },
		{ 0x80000000, 2, false }, { 0x00000000, 3, false },
		{ 0x83000000, 4, false },
		/* any other word: a demo is running (low 31 bits: countdown) */
		{ 0x0000d000, 1, true }, { 0x00000001, 1, true },
		{ 0x84000000, 1, true }, { 0x7fffffff, 1, true },
		{ 0x80000001, 1, true }, { 0xffffffff, 1, true },
	};

	for (unsigned int i = 0; i < sizeof(t) / sizeof(t[0]); i++) {
		assert(ua_plugin_license_state(t[i].word) == t[i].state);
		assert(ua_plugin_license_admits(t[i].state) == t[i].admit);
	}
	assert(!ua_plugin_license_admits(UA_PLUGIN_LICENSE_UNKNOWN));
	for (u32 s = 2; s < 100; s++)
		assert(!ua_plugin_license_admits(s));
	return 0;
}
''')
    csrc.build_and_run(code)


LIC_STUBS = r'''
static s64 now = 1791892800LL;		/* 2026-10-13 12:00 UTC */
static s64 ktime_get_real_seconds(void) { return now; }
static u64 div_u64_rem(u64 n, u32 d, u32 *rem) { *rem = (u32)(n % d); return n / d; }
static bool host_param = true;
bool ua_plugin_host_enabled(void) { return host_param; }
/* the strips are not part of these tests */
void ua_strips_connected(struct ua_device *ua) { (void)ua; }
void ua_strips_teardown(struct ua_device *ua, const char *why) { (void)ua; (void)why; }
void ua_strips_lost(struct ua_device *ua) { (void)ua; }
void ua_strips_service(struct ua_device *ua) { (void)ua; }
bool ua_strips_live(struct ua_device *ua) { (void)ua; return false; }
int ua_strips_override_target(struct ua_device *ua, unsigned int dsp, u32 synch, u64 epoch, u32 *generation)
{ (void)ua; (void)dsp; (void)synch; (void)epoch; (void)generation; return -ESTALE; }
int ua_strips_restore(struct ua_device *ua, struct ua_xact *x, u32 synch, u32 generation)
{ (void)ua; (void)x; (void)synch; (void)generation; return -ESTALE; }
/* user memory: plain memory here */
#define u64_to_user_ptr(x) ((void *)(uintptr_t)(x))
static unsigned long copy_from_user(void *to, const void *from, unsigned long n)
{ memcpy(to, from, n); return 0; }
static unsigned long copy_to_user(void *to, const void *from, unsigned long n)
{ memcpy(to, from, n); return 0; }
static void *memchr_inv(const void *p, int c, size_t n)
{
	const u8 *b = p;

	for (size_t i = 0; i < n; i++)
		if (b[i] != (u8)c)
			return (void *)(b + i);
	return NULL;
}
static void *kvmalloc_array(size_t n, size_t s, int f) { return kvmalloc(n * s, f); }
#define plugin_host host_param
#define offsetof_ok 1
'''


def build_license(main, *, real_strips=False):
    host = csrc.source('ua_plugin_host.c')
    host_fns = '\n'.join(csrc.extract(host, sig) for sig in (
        'void ua_plugin_connected(', 'void ua_plugin_epoch_bump(',
        'int ua_plugin_publish_now(', 'void ua_plugin_fence_set(',
        'int ua_plugin_fence_wait(', 'void ua_plugin_teardown(',
        'void ua_plugin_quarantine(', 'void ua_plugin_device_lost(',
        'u64 ua_plugin_host_ops('))
    hdr_abi = csrc.source('ua_ioctl.h')
    abi = '\n'.join(line for line in hdr_abi.splitlines()
                    if line.startswith('#define UA_PLUGIN_') or
                    line.startswith('#define UA_BOARD_QUERY'))
    structs = (csrc.between(hdr_abi, 'struct ua_board_query {', '#define UA_IOCTL_BOARD_QUERY') +
               csrc.between(hdr_abi, 'struct ua_plugin_resource {', '#define UA_IOCTL_PLUGIN_INSTANCE'))
    lic_stubs = LIC_STUBS
    if real_strips:
        begin = lic_stubs.index('/* the strips are not part')
        end = lic_stubs.index('/* user memory:')
        lic_stubs = lic_stubs[:begin] + lic_stubs[end:]
    code = ('#define UA_REAL_QUARANTINE 1\n' + khost.PREFIX + strips.HOST_FAKE +
            abi + '\n' + structs +
            'struct ua_file { struct ua_device *ua; };\n' +
            khost.header('ua_resource_pool.h') + khost.header('ua_plugin_host.h') +
            strips.STRIPS_STUBS + strips.HOST_STUBS + lic_stubs + fake.FPGA +
            khost.body('ua_resource_pool.c') + khost.body('ua_dsp_xact.c') +
            khost.body('ua_plugin_catalog.c') + khost.body('ua_plugin_upload.c') +
            khost.body('ua_plugin_place.c') + host_fns +
            khost.body('ua_plugin_license.c') + khost.body('ua_plugin_instance.c') +
            (khost.body('ua_strips.c') if real_strips else '') + GLUE + main)
    code = code.replace('#define offsetof_ok 1', '')
    return csrc.build_and_run(code, extra=('-Wno-unused-const-variable',
                                           '-Wno-unused-parameter',
                                           '-fsanitize=address,undefined',
                                           '-fno-sanitize-recover=all'))


GLUE = r'''
static struct ua_file file_a __maybe_unused = { &dev }, file_b __maybe_unused = { &dev };

/* Synthetic modules: a code record and a data record (made-up bodies) */
static u8 mod_code[64], mod_data[60];
static u8 sha_code[32], sha_data[32];
static char name_code[128], name_data[128];
static void mk(u8 *b, size_t len, u32 id, u32 w2, u32 w4, u8 *sha, char *name)
{
	char hex[65];

	put_unaligned_le32(UA_BILL_MAGIC, b);
	put_unaligned_le32(id, b + 4);
	put_unaligned_le32(w2, b + 8);
	put_unaligned_le32((u32)len - 20, b + 12);
	put_unaligned_le32(w4, b + 16);
	for (size_t i = 20; i < len; i++)
		b[i] = (u8)(i * 17 + 3);
	sha256(b, len, sha);
	*bin2hex(hex, sha, 32) = 0;
	snprintf(name, 128, "universal-audio/plugins/modules/%s.bill", hex);
}

static void setup(void)
{
	reset();
	host.mutex.held = 0;
	INIT_LIST_HEAD(&host.catalog);
	INIT_LIST_HEAD(&host.instances);
	host.active = true;
	host.next_handle = 1;
	host_param = true;
	now = 1791892800LL;
	dev.dsps_connected = true;
	dev.audio.sample_rate = 48000;
	rings_all();
	good_regs();
	mk(mod_code, sizeof(mod_code), 0xF00001, 0x02000000, 3, sha_code, name_code);
	mk(mod_data, sizeof(mod_data), 0xF00002, 0x00000001, 2, sha_data, name_data);
	ndisk = 0;
	disk[ndisk++] = (struct disk){ name_code, mod_code, sizeof(mod_code) };
	disk[ndisk++] = (struct disk){ name_data, mod_data, sizeof(mod_data) };
	mutex_lock(&dev.lock);
	ua_plugin_dsp_start(&dev, true);
	for (int i = 0; i < 4; i++)
		ua_plugin_read_geometry(&dev, i);
	mutex_unlock(&dev.lock);
	k_log_reset();
}

static void cleanup(void)
{
	mutex_lock(&dev.lock);
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

static bool all_idle(void)
{
	for (int i = 0; i < 4; i++)
		if (host.dsp[i].pools && !ua_pool_owner_idle(host.dsp[i].pools))
			return false;
	return true;
}
static unsigned int uploads(void)
{
	unsigned int n = 0;

	for (int i = 0; i < 4; i++)
		n += host.dsp[i].uploads;
	return n;
}

/* CREATE through the ioctl, as the daemon would call it */
static struct ua_plugin_resource res[2];
static struct ua_plugin_private priv[2] = {
	{ 1, 8, 0xffffffff, 0 }, { 2, 4, 0x00000006, 0 } };
static long create(struct ua_file *f, u32 render, u32 flags,
		   struct ua_plugin_instance *q)
{
	memset(q, 0, sizeof(*q));
	memcpy(res[0].sha256, sha_code, 32);
	memcpy(res[1].sha256, sha_data, 32);
	q->struct_size = sizeof(*q);
	q->op = UA_PLUGIN_INST_CREATE;
	q->flags = flags;
	q->render_id = render;
	q->dsp = UA_PLUGIN_DSP_ANY;
	q->n_resources = 2;
	q->n_private = 2;
	q->resources = (u64)(uintptr_t)res;
	q->privates = (u64)(uintptr_t)priv;
	return ua_plugin_ioctl_instance(&dev, f, (unsigned long)q);
}
static long destroy(struct ua_file *f, u32 handle)
{
	struct ua_plugin_instance q;

	memset(&q, 0, sizeof(q));
	q.struct_size = sizeof(q);
	q.op = UA_PLUGIN_INST_DESTROY;
	q.handle = handle;
	return ua_plugin_ioctl_instance(&dev, f, (unsigned long)&q);
}
#define HAS(s) K_HAS(s)
'''


def test_board_query_transaction():
    build_license(r'''
int main(void)
{
	static struct ua_board_query q;
	u32 cmd0, rx0;

	setup();
	for (unsigned int i = 0; i < 768; i++)
		bq_table[i] = 0x1000 + i;		/* synthetic */
	bq_table[6] = 0x81000000;
	memset(&q, 0, sizeof(q));
	q.struct_size = sizeof(q);
	cmd0 = dev.dsp[0].cmd.write_ptr;
	rx0 = dev.dsp[0].resp.write_ptr;
	assert(ua_plugin_ioctl_board_query(&dev, (unsigned long)&q) == 0);
	/* the reply descriptor first: 770 words, prefilled with the sentinel */
	assert(bells[0] == 1 && bells[1] == 0);
	assert(slots[1][rx0][0] == 0x80000302);
	assert(slots[0][cmd0][0] == 0x00100002 && slots[0][cmd0][1] == q.date);
	assert(slots[0][(cmd0 + 1) & 1023][0] == 0x00110002);
	assert(q.date == ((7225u << 12) + 720) && bq_dates[0] == q.date);
	/* passed through unmodified */
	for (unsigned int i = 0; i < 768; i++)
		assert(q.words[i] == bq_table[i]);
	assert(q.epoch == host.epoch && host.board_queries == 1);
	assert(!sleeps_locked && !host.mutex.held);
	/* the table is never logged */
	assert(!HAS("0x00001000") && !HAS("00001000"));

	/* a reply that is not the query's, or none: refused, DSP 0 not quarantined */
	bq_mode = BQ_BAD_HEADER;
	memset(&q, 0, sizeof(q));
	q.struct_size = sizeof(q);
	assert(ua_plugin_ioctl_board_query(&dev, (unsigned long)&q) == -EPROTO);
	assert(!q.words[6] && !q.date);			/* nothing copied back */
	bq_mode = BQ_BAD_ECHO;
	q.struct_size = sizeof(q);
	assert(ua_plugin_ioctl_board_query(&dev, (unsigned long)&q) == -EPROTO);
	bq_mode = BQ_SILENT;
	assert(ua_plugin_ioctl_board_query(&dev, (unsigned long)&q) == -ETIMEDOUT);
	assert(!host.dsp[0].quarantined && host.board_query_failures == 3);
	/* the next query clears the unanswered descriptor and works */
	bq_mode = BQ_OK;
	assert(ua_plugin_ioctl_board_query(&dev, (unsigned long)&q) == 0);
	assert(q.words[6] == 0x81000000);

	/* malformed requests, the clock before 2007, plugin_host=0 */
	memset(&q, 0, sizeof(q));
	assert(ua_plugin_ioctl_board_query(&dev, (unsigned long)&q) == -EINVAL);
	q.struct_size = sizeof(q);
	q.flags = 1;
	assert(ua_plugin_ioctl_board_query(&dev, (unsigned long)&q) == -EINVAL);
	q.flags = 0;
	q.date = 5;					/* no date can be given */
	assert(ua_plugin_ioctl_board_query(&dev, (unsigned long)&q) == -EINVAL);
	q.date = 0;
	{
		unsigned int before = nputs;

		now = 1167609599LL;
		assert(ua_plugin_ioctl_board_query(&dev, (unsigned long)&q) == -ERANGE);
		assert(nputs == before);
		now = 1791892800LL;
		host_param = false;
		assert(ua_plugin_ioctl_board_query(&dev, (unsigned long)&q) == -EOPNOTSUPP);
		assert(nputs == before);
		host_param = true;
	}
	cleanup();
	assert(!k_objects && !dma_live);
	return 0;
}
''')


def test_epoch_change_during_the_query_refuses():
    """An answer that belongs to an earlier epoch is no answer: the epoch
    moving on while the query is out turns it into -ESTALE, and CREATE
    refuses with nothing placed."""
    build_license(r'''
static void bump(void) { host.epoch++; on_tick = NULL; }
int main(void)
{
	u32 words[768], date;
	u64 epoch;
	struct ua_plugin_instance q;

	setup();
	bq_table[6] = 0x81000000;
	host.mutex.held = 1;
	on_tick = bump;
	assert(ua_plugin_board_query(&dev, words, &date, &epoch) == -ESTALE);
	host.mutex.held = 0;
	on_tick = bump;
	assert(create(&file_a, 0x806, 0, &q) == -ESTALE);
	assert(!q.handle && !q.epoch);		/* nothing copied back */
	assert(!uploads() && all_idle() && !host.catalog_count);
	cleanup();
	return 0;
}
''')


def test_epoch_change_after_the_answer_places_nothing():
    """The answer and the placement are one operation in one epoch: an
    epoch change in between (here: while the modules are read) refuses
    with -ESTALE and nothing is uploaded."""
    build_license(r'''
static void bump(void) { host.epoch++; on_fw_read = NULL; }
int main(void)
{
	struct ua_plugin_instance q;

	setup();
	bq_table[6] = 0x81000000;
	on_fw_read = bump;
	assert(create(&file_a, 0x806, 0, &q) == -ESTALE);
	assert(!uploads() && all_idle() && !host.n_instances);
	cleanup();
	assert(!k_objects && !dma_live);
	return 0;
}
''')


GATE_CASES = [
    # (word, state, admitted)
    (0x81000000, 0, True), (0x82000000, 0, True), (0x0000d000, 1, True),
    (0x80000000, 2, False), (0x00000000, 3, False), (0x83000000, 4, False),
]


def test_create_refuses_before_any_upload():
    cases = ''.join('\t{ 0x%08x, %d, %d },\n' % c for c in GATE_CASES)
    build_license(r'''
static const struct { u32 word, state; int admit; } cases[] = {
''' + cases + r'''};

int main(void)
{
	struct ua_plugin_instance q;

	for (unsigned int c = 0; c < sizeof(cases) / sizeof(cases[0]); c++) {
		setup();
		bq_table[6] = cases[c].word;		/* plug-in 0x806 */
		long r = create(&file_a, 0x806, 0, &q);

		assert(q.license_state == cases[c].state);
		assert(q.license_word == cases[c].word);
		if (!cases[c].admit) {
			/* refused: the device was asked, nothing else happened */
			assert(r == -EACCES && !q.handle);
			assert(bq_answered == 1 && !uploads() && all_idle());
			assert(!host.catalog_count && !host.n_instances);
			assert(host.refused == 1 && HAS("refused"));
		} else {
			assert(r == 0 && q.handle && host.n_instances == 1);
			/* the answer came before the first upload */
			assert(bq_answered == 1 && uploads() == 2);
			assert(host.dsp[q.dsp].replies_ok == 2 && q.dsp == 1);
			assert(!all_idle() && host.admitted == 1);
			assert(destroy(&file_a, q.handle) == 0);
			assert(all_idle() && !host.n_instances);
			assert(host.dsp[1].releases == 2 && !host.fence[1]);
		}
		cleanup();
		assert(!k_objects && !dma_live);
	}
	return 0;
}
''')


def test_no_answer_refuses():
    build_license(r'''
int main(void)
{
	struct ua_plugin_instance q;

	/* ids outside the table: no query at all */
	setup();
	bq_table[6] = 0x81000000;
	assert(create(&file_a, 0x7ff, 0, &q) == -EACCES);
	assert(q.license_state == UA_PLUGIN_LICENSE_UNKNOWN && !bq_answered);
	assert(create(&file_a, 0xb00, 0, &q) == -EACCES && !bq_answered);
	assert(!uploads() && all_idle());

	/* the query fails: refused, nothing read or sent */
	bq_mode = BQ_BAD_ECHO;
	assert(create(&file_a, 0x806, 0, &q) == -EPROTO);
	bq_mode = BQ_BAD_HEADER;
	assert(create(&file_a, 0x806, 0, &q) == -EPROTO);
	bq_mode = BQ_SILENT;
	assert(create(&file_a, 0x806, 0, &q) == -ETIMEDOUT);
	bq_mode = BQ_OK;
	now = 1000;				/* clock before 2007 */
	assert(create(&file_a, 0x806, 0, &q) == -ERANGE);
	now = 1791892800LL;
	assert(!uploads() && all_idle() && !host.catalog_count);
	assert(!host.dsp[0].quarantined);

	cleanup();
	return 0;
}
''')


def test_check_flag_reads_the_answer_only():
    build_license(r'''
int main(void)
{
	struct ua_plugin_instance q;

	setup();
	bq_table[6] = 0x81000000;
	bq_table[48] = 0x80000000;		/* 0x830: demo not started */
	assert(create(&file_a, 0x806, UA_PLUGIN_CREATE_F_CHECK, &q) == 0);
	assert(q.license_state == 0 && !q.handle && q.epoch == host.epoch);
	assert(create(&file_a, 0x830, UA_PLUGIN_CREATE_F_CHECK, &q) == -EACCES);
	assert(q.license_state == 2 && q.license_word == 0x80000000);
	assert(bq_answered == 2 && !uploads() && !host.catalog_count && all_idle());
	cleanup();
	return 0;
}
''')


def test_instances_belong_to_their_file():
    build_license(r'''
int main(void)
{
	struct ua_plugin_instance q, q2, q3;

	setup();
	bq_table[6] = 0x81000000;
	assert(create(&file_a, 0x806, 0, &q) == 0);
	assert(create(&file_a, 0x806, 0, &q2) == 0);
	/* the second shares the modules: no upload, a hold more */
	assert(uploads() == 2 && q2.dsp == q.dsp && bq_answered == 2);
	assert(create(&file_b, 0x806, 0, &q3) == 0);
	assert(host.n_instances == 3);
	/* another file's handle is not this file's */
	assert(destroy(&file_b, q.handle) == -ENOENT);
	/* the first destroy releases nothing (the others still hold) */
	assert(destroy(&file_a, q.handle) == 0 && host.dsp[1].releases == 0);
	/* closing file A takes its other instance down */
	ua_plugin_file_release(&file_a);
	assert(host.n_instances == 1 && host.dsp[1].releases == 0);
	/* the last holder's close releases the modules */
	ua_plugin_file_release(&file_b);
	assert(!host.n_instances && host.dsp[1].releases == 2 && all_idle());
	assert(!sleeps_locked && !dev.lock.held && !host.mutex.held);
	cleanup();
	return 0;
}
''')


def test_teardown_and_lost_dsps_end_instances():
    build_license(r'''
int main(void)
{
	struct ua_plugin_instance q;

	setup();
	bq_table[6] = 0x81000000;
	assert(create(&file_a, 0x806, 0, &q) == 0);
	/* a rate change takes it down: published, fenced, handle stale */
	mutex_lock(&dev.lock);
	ua_plugin_teardown(&dev, "rate change");
	mutex_unlock(&dev.lock);
	assert(host.fence[1] && host.dsp[1].releases == 2 && all_idle());
	assert(destroy(&file_a, q.handle) == 0);	/* forgets it */
	assert(!host.n_instances);

	/* a raw DSP ioctl: the instance is lost with its DSP, nothing sent */
	assert(create(&file_a, 0x806, 0, &q) == 0);
	{
		unsigned int rel = host.dsp[1].releases;

		ua_plugin_taint(&dev, "a raw DSP ioctl");
		assert(host.dsp[1].releases == rel);
		assert(destroy(&file_a, q.handle) == 0 && host.dsp[1].releases == rel);
	}
	/* nothing is owned now: CREATE is refused after the answer */
	assert(create(&file_a, 0x806, 0, &q) == -ENODEV);
	assert(!host.n_instances);
	cleanup();
	return 0;
}
''')


def test_upload_failure_takes_down_what_was_placed():
    build_license(r'''
int main(void)
{
	struct ua_plugin_instance q;

	setup();
	bq_table[6] = 0x81000000;
	reply_status = 0xf0010006;		/* the DSP refuses the first module */
	assert(create(&file_a, 0x806, 0, &q) == -EPROTO);
	assert(host.dsp[1].quarantined && !host.n_instances);
	assert(all_idle());
	reply_status = 0;
	/* the next goes elsewhere (DSP 1 is quarantined) */
	assert(create(&file_a, 0x806, 0, &q) == 0 && q.dsp == 2);
	assert(destroy(&file_a, q.handle) == 0);
	cleanup();
	return 0;
}
''')


def test_another_version_of_a_module_in_use_is_refused():
    """The DSP knows a resource by its id: a module with the same id and
    architecture but other bytes than one already in use is refused before
    anything is placed."""
    build_license(r'''
int main(void)
{
	static u8 other[64];
	static u8 sha_other[32];
	static char name_other[128];
	struct ua_plugin_instance q, q2;
	unsigned int up;

	setup();
	bq_table[6] = 0x81000000;
	assert(create(&file_a, 0x806, 0, &q) == 0);
	mk(other, sizeof(other), 0xF00001, 0x02000000, 3, sha_other, name_other);
	other[40] ^= 0xff;			/* same id, other bytes */
	sha256(other, sizeof(other), sha_other);
	{
		char hex[65];

		*bin2hex(hex, sha_other, 32) = 0;
		snprintf(name_other, 128, "universal-audio/plugins/modules/%s.bill", hex);
	}
	disk[ndisk++] = (struct disk){ name_other, other, sizeof(other) };
	up = uploads();
	memset(&q2, 0, sizeof(q2));
	memcpy(res[0].sha256, sha_other, 32);
	memcpy(res[1].sha256, sha_data, 32);
	q2.struct_size = sizeof(q2);
	q2.op = UA_PLUGIN_INST_CREATE;
	q2.render_id = 0x806;
	q2.dsp = UA_PLUGIN_DSP_ANY;
	q2.n_resources = 2;
	q2.n_private = 2;
	q2.resources = (u64)(uintptr_t)res;
	q2.privates = (u64)(uintptr_t)priv;
	assert(ua_plugin_ioctl_instance(&dev, &file_a, (unsigned long)&q2) == -EEXIST);
	assert(uploads() == up && host.n_instances == 1);
	ua_plugin_file_release(&file_a);
	assert(all_idle());
	cleanup();
	assert(!k_objects && !dma_live);
	return 0;
}
''')


def test_request_validation():
    build_license(r'''
int main(void)
{
	struct ua_plugin_instance q;

	setup();
	bq_table[6] = 0x81000000;
	memset(&q, 0, sizeof(q));
	q.struct_size = sizeof(q);
	q.op = 9;
	assert(ua_plugin_ioctl_instance(&dev, &file_a, (unsigned long)&q) == -EINVAL);
	/* output fields must come in zero */
	assert(create(&file_a, 0x806, 0, &q) == 0);
	q.op = UA_PLUGIN_INST_CREATE;
	q.handle = 0;
	q.license_state = 1;
	assert(ua_plugin_ioctl_instance(&dev, &file_a, (unsigned long)&q) == -EINVAL);
	/* bad private rows are refused before the device is asked */
	{
		unsigned int before = bq_answered;

		priv[1].link = 0x00000009;		/* offset past row 0 (8 words) */
		assert(create(&file_a, 0x806, 0, &q) == -EINVAL);
		priv[1].link = 0x00000006;
		priv[1].pool = 0;
		assert(create(&file_a, 0x806, 0, &q) == -EINVAL);
		priv[1].pool = 2;
		priv[0].link = 0;
		assert(create(&file_a, 0x806, 0, &q) == -EINVAL);
		priv[0].link = 0xffffffff;
		assert(bq_answered == before);
	}
	/* a module that is not in the store: refused after the answer, nothing placed */
	ua_plugin_file_release(&file_a);
	assert(!host.n_instances && !host.catalog_count && all_idle());
	ndisk = 1;
	{
		unsigned int up = uploads();

		assert(create(&file_a, 0x806, 0, &q) == -ENOENT);
		assert(uploads() == up && !host.n_instances && all_idle());
		assert(!host.catalog_count);
	}
	host_param = false;
	assert(create(&file_a, 0x806, 0, &q) == -EOPNOTSUPP);
	host_param = true;
	ua_plugin_file_release(&file_a);
	cleanup();
	return 0;
}
''')


def test_operation_bits_only_while_accepted():
    """GET_PLUGIN_CAPS.operations (A1, review finding 7): nothing with
    plugin_host=0; DESTROY always with it; BOARD_QUERY only on a connected
    device with DSP 0's rings running; CREATE only when some DSP is the
    host's to place on (owned, not quarantined, its map read)."""
    build_license(r'''
int main(void)
{
	u64 ops;

	setup();
	dev.audio.connected = true;
	mutex_lock(&dev.lock);
	ops = ua_plugin_host_ops(&dev);
	assert(ops == (UA_PLUGIN_OP_BOARD_QUERY | UA_PLUGIN_OP_CREATE | UA_PLUGIN_OP_DESTROY | UA_PLUGIN_OP_SERVICE));
	for (int i = 0; i < 4; i++)
		host.dsp[i].quarantined = true;
	assert(ua_plugin_host_ops(&dev) == (UA_PLUGIN_OP_BOARD_QUERY | UA_PLUGIN_OP_DESTROY));
	host.dsp[2].quarantined = false;
	assert(ua_plugin_host_ops(&dev) & UA_PLUGIN_OP_CREATE);
	ua_plugin_disown(&dev, 2, "test");
	assert(!(ua_plugin_host_ops(&dev) & UA_PLUGIN_OP_CREATE));
	dev.audio.connected = false;
	assert(ua_plugin_host_ops(&dev) == UA_PLUGIN_OP_DESTROY);
	dev.audio.connected = true;
	dev.dsp[0].resp.live = false;
	assert(ua_plugin_host_ops(&dev) == UA_PLUGIN_OP_DESTROY);
	dev.dsp[0].resp.live = true;
	dev.shutdown.counter = 1;
	assert(ua_plugin_host_ops(&dev) == UA_PLUGIN_OP_DESTROY);
	dev.shutdown.counter = 0;
	host_param = false;
	assert(ua_plugin_host_ops(&dev) == 0);
	mutex_unlock(&dev.lock);
	cleanup();
	return 0;
}
'''.replace('static bool plugin_host_param_unused;', ''))


def test_the_gate_is_inside_create():
    """Static: the only path to a placement is CREATE, and CREATE asks the
    device before anything else; the strips never ask (they are not
    plug-ins); no override exists."""
    inst = csrc.source('ua_plugin_instance.c')
    create = csrc.extract(inst, 'static int ua_plugin_create(')
    i_admit = create.index('ua_plugin_admit(ua, req->render_id')
    for later in ('ua_catalog_get(', 'ua_plugin_place(', 'ua_plugin_upload(',
                  'ua_inst_privates('):
        assert create.index(later) > i_admit, later
    assert 'if (ret || (req->flags & UA_PLUGIN_CREATE_F_CHECK))\n\t\tgoto unlock;' in create
    # placement and upload only happen in CREATE (and the strips, which use
    # their own fixed modules)
    for f in ('ua_core.c', 'ua_audio.c', 'ua_event.c', 'ua_dsp.c',
              'ua_plugin_license.c', 'ua_plugin_host.c'):
        src = csrc.source(f)
        assert 'ua_plugin_place(' not in src and 'ua_plugin_upload(' not in src, f
    lic = csrc.source('ua_plugin_license.c')
    admit = csrc.extract(lic, 'int ua_plugin_admit(')
    assert 'ua_plugin_board_query(ua, words, &date, epoch)' in admit
    # fail closed: only an admitting state returns 0
    assert admit.index('if (!ua_plugin_license_admits(*state))') < admit.index('h->admitted++')
    query = csrc.extract(lic, 'int ua_plugin_board_query(')
    assert 'ktime_get_real_seconds()' in query
    assert 'cookie' in query and 'UA_BQ_REPLY' in query
