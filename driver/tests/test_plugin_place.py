"""ua_plugin_place.c: DSP ownership, pool geometry, placement (task A4).

The file and the pool ledger are compiled whole against fake DSP register
windows.  Checks (review findings 1 and 16, plan A4):
  - a DSP is the host's only after a cold start that found its boot word
    exactly 3; a DSP found running at probe, one another host connected to
    (0x0018000b after Linux connects, 0x00100007 after macOS), and one a
    raw DSP ioctl touched are not, until the next cold start;
  - with nothing hosted nothing is read;
  - a warm restart keeps ownership only with an idle ledger;
  - the geometry registers (capacity +0x10, base +0x184, scratch +0x194)
    become the ledger; titan's DSP 0 map and the Mac's per-DSP capacities
    (code 16384; pools 43320 / 8062 / 16699822 after scratch 128 / 128 /
    32768) come out of it; a changed map with resources placed disowns;
  - placement tries DSPs 1, 2, 3, then 0, never a quarantined or unowned
    one, all or nothing;
  - the proc dump shows ownership, registers, usage and traffic.
"""
import re

import khost
import csrc

FAKE = r'''
#define UA_MAX_DSPS 8
#define UA_PLUGIN_DSP_ANY 0xffffffffu
struct delayed_work { int unused; };
struct device { int unused; };
struct pci_dev { struct device dev; };
struct ua_plugin_host;
struct ua_device {
	struct pci_dev *pdev;
	unsigned int num_dsps;
	struct mutex lock;
	struct ua_plugin_host *host;
};
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
static int snd_card_ro_proc_new(struct snd_card *c, const char *n, void *p,
				void (*fn)(struct snd_info_entry *, struct snd_info_buffer *))
{ (void)c; (void)n; (void)p; (void)fn; return 0; }
static u32 regs[8][0x200];
static unsigned int reads;
static u32 ua_read(struct ua_device *ua, u32 off)
{
	(void)ua;
	reads++;
	assert(off < 0x2000 || (off >= 0x4000 && off < 0x6000));
	return regs[off < 0x2000 ? off / 0x800 : (off - 0x2000) / 0x800][(off % 0x800) / 4];
}
static u64 epochs;
static bool host_param;
static bool ua_plugin_host_enabled(void) { return host_param; }
'''

HOOKS = r'''
void ua_plugin_epoch_bump(struct ua_device *ua, const char *why)
{
	(void)why;
	ua->host->epoch++;
	epochs++;
}
void ua_instances_dsp_gone(struct ua_device *ua, unsigned int dsp) { (void)ua; (void)dsp; }
bool ua_catalog_resident(const struct ua_cat_entry *e, unsigned int dsp, u64 epoch)
{
	(void)e; (void)dsp; (void)epoch;
	return false;
}
'''


def run(main):
    code = (khost.PREFIX + FAKE + khost.header('ua_resource_pool.h') +
            khost.header('ua_plugin_host.h') + HOOKS +
            khost.body('ua_resource_pool.c') + khost.body('ua_plugin_place.c') +
            GLUE + main)
    return csrc.build_and_run(code, extra=('-fsanitize=address,undefined',
                                           '-fno-sanitize-recover=all'))


GLUE = r'''
static struct ua_device dev;
static struct ua_plugin_host host;
static struct pci_dev pci;

/* titan's x4 DSP 0 (A6.1) and the Mac's empty DSPs 2/3 capacities */
static void geometry(unsigned int dsp, bool mac)
{
	u32 *w = regs[dsp];

	w[0x10 / 4] = 0x4000;
	w[0x14 / 4] = mac ? 43320 + 128 : 0xa9b8;
	w[0x18 / 4] = mac ? 8062 + 128 : 0x1ffe;
	w[0x1c / 4] = mac ? 16699822 + 32768 : 0xffe000;
	w[0x184 / 4] = 0xe0000;
	w[0x188 / 4] = 0x92800;
	w[0x18c / 4] = 0xc5000;
	w[0x190 / 4] = 0x8000000;
	w[0x194 / 4] = mac ? 128 : 0x80;
	w[0x198 / 4] = mac ? 128 : 0x80;
	w[0x19c / 4] = mac ? 32768 : 0x8000;
}

static void setup(bool active)
{
	for (unsigned int i = 0; i < UA_HOST_DSPS; i++)
		if (host.dsp[i].pools)
			ua_pool_owner_destroy(host.dsp[i].pools);
	memset(&dev, 0, sizeof(dev));
	memset(&host, 0, sizeof(host));
	memset(regs, 0, sizeof(regs));
	dev.pdev = &pci;
	dev.num_dsps = 4;
	dev.host = &host;
	host.ua = &dev;
	host.epoch = 1;
	host.active = active;
	INIT_LIST_HEAD(&host.catalog);
	for (unsigned int i = 0; i < 4; i++) {
		regs[i][0x1a4 / 4] = 3;
		geometry(i, i >= 2);
	}
	reads = 0;
	k_log_reset();
}

static void start(bool cold)
{
	mutex_lock(&dev.lock);
	ua_plugin_dsp_start(&dev, cold);
	mutex_unlock(&dev.lock);
}

static void connect_all(void)
{
	mutex_lock(&dev.lock);
	for (unsigned int i = 0; i < dev.num_dsps; i++)
		if (host.dsp[i].owned)
			ua_plugin_read_geometry(&dev, i);
	mutex_unlock(&dev.lock);
}

static const char *proc(void)
{
	static struct snd_info_buffer b;

	b.len = 0;
	b.text[0] = 0;
	mutex_lock(&dev.lock);
	ua_plugin_proc(&dev, &b);
	mutex_unlock(&dev.lock);
	return b.text;
}
'''


def test_ownership_rules():
    run(r'''
int main(void)
{
	/* nothing hosted: nothing read, nothing owned, the epoch still moves */
	setup(false);
	start(true);
	assert(!reads && !host.dsp[0].owned && host.epoch == 2);

	/* a cold start that finds every DSP freshly powered */
	setup(true);
	start(true);
	for (int i = 0; i < 4; i++)
		assert(host.dsp[i].owned && host.dsp[i].boot_word == 3);
	assert(K_HAS("DSPs this driver may use: 0xf"));
	/* the boot words are the only reads at the start */
	assert(reads == 4);

	/* a host connected since power-up: Linux's connect count, macOS */
	setup(true);
	regs[1][0x1a4 / 4] = 0x0018000b;
	regs[3][0x1a4 / 4] = 0x00100007;
	start(true);
	assert(host.dsp[0].owned && !host.dsp[1].owned && host.dsp[2].owned &&
	       !host.dsp[3].owned);
	assert(strstr(host.dsp[1].why, "boot word 0x0018000b"));

	/* found running at probe: never the host's */
	setup(true);
	start(false);
	for (int i = 0; i < 4; i++)
		assert(!host.dsp[i].owned);
	assert(!reads);
	assert(strstr(proc(), "DSP 0: not owned, boot word 0x00000000: running when this driver found it"));

	/* a warm restart of the same instance keeps an idle owner ... */
	setup(true);
	start(true);
	connect_all();
	start(false);
	assert(host.dsp[0].owned && host.dsp[0].pools);
	/* ... but not one with resources still placed */
	{
		struct ua_pool_lease l;
		int id;

		assert(!ua_pool_reserve(host.dsp[2].pools, 1, 8, &id, false, &l));
		start(false);
		assert(!host.dsp[2].owned && !host.dsp[2].pools && host.dsp[1].owned);
	}

	/* a raw DSP ioctl: nothing is owned until the next cold start */
	setup(true);
	start(true);
	connect_all();
	ua_plugin_taint(&dev, "a raw DSP ioctl");
	for (int i = 0; i < 4; i++)
		assert(!host.dsp[i].owned && !host.dsp[i].pools);
	assert(!dev.lock.held && host.epoch == 3);
	start(false);
	assert(!host.dsp[0].owned);
	start(true);
	assert(host.dsp[0].owned);
	/* a cold start clears a quarantine */
	host.dsp[1].quarantined = true;
	start(true);
	assert(!host.dsp[1].quarantined);
	setup(true);
	return 0;
}
''')


def test_geometry_becomes_the_ledger():
    run(r'''
int main(void)
{
	struct ua_pool_usage u[4];
	const char *p;

	setup(true);
	start(true);
	connect_all();
	/* titan DSP 0, as the recipe was validated on (A6.1) */
	assert(ua_pool_read_usage(host.dsp[0].pools, u) == 0);
	assert(u[0].base == 0xe0000 && u[0].total_words == 0x4000 && !u[0].scratch_words);
	assert(u[1].total_words == 0xa9b8 && u[1].scratch_words == 0x80 &&
	       u[1].base + u[1].total_words == 0x9d1b8);
	assert(u[2].base + u[2].total_words == 0xc6ffe);
	assert(u[3].base + u[3].total_words == 0x08ffe000);
	/* the Mac's empty DSPs: capacities after scratch */
	assert(ua_pool_read_usage(host.dsp[2].pools, u) == 0);
	assert(u[0].free_words == 16384 && u[1].free_words == 43320 &&
	       u[2].free_words == 8062 && u[3].free_words == 16699822);
	assert(u[1].scratch_words == 128 && u[2].scratch_words == 128 &&
	       u[3].scratch_words == 32768);
	p = proc();
	assert(strstr(p, "DSP 0: owned, boot word 0x00000003: cold-started by this driver"));
	assert(strstr(p, "  registers: size 00004000 0000a9b8 00001ffe 00ffe000 base 000e0000 00092800 000c5000 08000000 scratch 00000080 00000080 00008000\n"));
	assert(strstr(p, "  pool 1: base 0x00092800 total 43448 scratch 128 used 0 free 43320"));
	assert(strstr(p, "  sent: transactions 0, uploads 0"));
	assert(strstr(p, "authorization: 0 board queries"));

	/* the same map again: same ledger */
	{
		struct ua_pool_owner *o = host.dsp[2].pools;

		mutex_lock(&dev.lock);
		assert(ua_plugin_read_geometry(&dev, 2) == 0 && host.dsp[2].pools == o);
		/* another map with resources placed: no longer trusted */
		struct ua_pool_lease l;
		int id;

		assert(!ua_pool_reserve(o, 3, 8, &id, false, &l));
		regs[2][0x1c / 4] -= 64;
		assert(ua_plugin_read_geometry(&dev, 2) == -EIO && !host.dsp[2].owned);
		/* all-ones (device gone): nothing changes */
		regs[1][0x184 / 4] = 0xffffffff;
		assert(ua_plugin_read_geometry(&dev, 1) == -ENODEV && host.dsp[1].owned);
		/* not a map at all: disowned */
		regs[1][0x184 / 4] = 0x92800;
		regs[1][0x198 / 4] = 0x40000;
		/* idle: a new map replaces the ledger, an invalid one disowns */
		assert(ua_plugin_read_geometry(&dev, 1) == -EINVAL && !host.dsp[1].owned);
		host.dsp[3].owned = true;
		ua_pool_owner_destroy(host.dsp[3].pools);
		host.dsp[3].pools = NULL;
		regs[3][0x198 / 4] = 0x40000;	/* scratch past the pool */
		assert(ua_plugin_read_geometry(&dev, 3) == -EINVAL && !host.dsp[3].owned);
		assert(ua_plugin_read_geometry(&dev, 3) == -EPERM);
		mutex_unlock(&dev.lock);
	}
	setup(true);
	return 0;
}
''')


def test_placement_order_and_refusals():
    run(r'''
int main(void)
{
	struct ua_pool_request rq[2];
	struct ua_pool_lease l[2], m[2];
	unsigned int dsp = 99;
	int code, row;

	setup(true);
	start(true);
	connect_all();
	rq[0] = (struct ua_pool_request){ 0, 0x2d2, &code, true };
	rq[1] = (struct ua_pool_request){ 1, 0x38, &row, false };
	mutex_lock(&dev.lock);
	/* any: DSP 1 first */
	assert(ua_plugin_place(&dev, UA_PLUGIN_DSP_ANY, rq, 2, &dsp, l) == 0 && dsp == 1);
	assert(l[0].base == 0xe0000 && l[1].base == 0x9d1b8 - 0x38);
	/* quarantined and unowned DSPs are skipped: 1 is quarantined, 2 unowned */
	host.dsp[1].quarantined = true;
	ua_plugin_disown(&dev, 2, "test");
	assert(ua_plugin_place(&dev, UA_PLUGIN_DSP_ANY, rq, 2, &dsp, m) == 0 && dsp == 3);
	assert(ua_plugin_unplace(&dev, 3, m, 2) == 0);
	/* a specific DSP only */
	assert(ua_plugin_place(&dev, 1, rq, 2, &dsp, m) == -ENODEV);
	assert(ua_plugin_place(&dev, 0, rq, 2, &dsp, m) == 0 && dsp == 0);
	assert(ua_plugin_unplace(&dev, 0, m, 2) == 0);
	/* DSP 0 last */
	ua_plugin_disown(&dev, 3, "test");
	assert(ua_plugin_place(&dev, UA_PLUGIN_DSP_ANY, rq, 2, &dsp, m) == 0 && dsp == 0);
	assert(ua_plugin_unplace(&dev, 0, m, 2) == 0);
	/* no room anywhere: nothing taken */
	rq[1].words = 0x20000;
	assert(ua_plugin_place(&dev, UA_PLUGIN_DSP_ANY, rq, 2, &dsp, m) == -ENOSPC);
	assert(ua_pool_owner_idle(host.dsp[0].pools));
	/* nothing admitted */
	ua_plugin_disown(&dev, 0, "test");
	assert(ua_plugin_place(&dev, UA_PLUGIN_DSP_ANY, rq, 2, &dsp, m) == -ENODEV);
	assert(ua_plugin_place(&dev, UA_PLUGIN_DSP_ANY, rq, 0, &dsp, m) == -EINVAL);
	assert(!ua_plugin_dsp_admitted(&dev, 9));
	/* DSP 1's set comes back */
	assert(ua_plugin_unplace(&dev, 1, l, 2) == 0);
	assert(ua_pool_owner_idle(host.dsp[1].pools));
	mutex_unlock(&dev.lock);
	setup(true);
	assert(!k_objects);
	return 0;
}
''')


def test_hooks_are_wired():
    core = csrc.source('ua_core.c')
    init = csrc.extract(core, 'static int ua_dsp_init_and_load(')
    assert init.count('ua_plugin_dsp_start(ua, false);') == 1
    assert init.count('ua_plugin_dsp_start(ua, true);') == 1
    assert init.index('ua_dsp_wait_ready(ua);') < init.index('ua_plugin_dsp_start(ua, true);')
    dispatch = csrc.extract(core, 'static long ua_ioctl_dispatch(')
    assert dispatch.index('ua_plugin_taint(ua, "a raw DSP ioctl")') < dispatch.index('switch (cmd)')
    touches = csrc.extract(core, 'static bool ua_ioctl_touches_dsp_pools(')
    for name in ('LOAD_FIRMWARE', 'DSP_CONNECT', 'DSP_SEND_BLOCK', 'RING_SEND',
                 'DSP_SRAM_DATA', 'DSP_SEND_RAW', 'WRITE_REG', 'RESET_DMA',
                 'DSP_TEST_DMA', 'DSP_TEST_SEND'):
        assert 'UA_IOCTL_' + name + ':' in touches, name
    rawio = csrc.extract(core, 'static bool ua_ioctl_needs_rawio(')
    for case in re.findall(r'case (UA_IOCTL_\w+):', touches):
        assert case in rawio, case
    audio = csrc.source('ua_audio.c')
    assert 'ua_plugin_connected(ua);' in audio
    assert 'ua_plugin_proc_init(ua, card)' in csrc.source('ua_dsp.c')



