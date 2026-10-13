"""ua_dsp_xact.c: plug-in host transactions on a fake DSP ring (task A2).

The file is compiled whole against a fake FPGA: command and response rings
with POSITION registers that move only while the driver sleeps, DMA
references the fake reads through, and replies it writes into the next
reply descriptor for the commands that get one (a resource upload, the
board query).  Checks, per the plan's A2 list and overlay review findings
4, 5, 9 and 15:
  - replies are published before the command (doorbell order), bus
    addresses only from the transaction's own buffer;
  - ua->lock is never held while waiting (the fake's sleep asserts it);
  - a timeout quarantines the DSP and keeps the buffer on the host's own
    bounded list until the ring is provably past it (reclaimed then);
  - an epoch change refuses publication and turns an outstanding one stale;
  - a ring resync that abandons our entries settles them like the orphans;
  - the retention list is bounded and refuses only new transactions;
  - commands go out as whole 4 KiB DMA_REFs, never 1 KiB pieces;
  - a reply transaction waits for an idle command ring and clears a
    foreign reply descriptor first; a failed put rolls everything back.
"""
import khost
import csrc

FAKE = r'''
#define UA_RING_DMA_REF 0x80000000u
#define UA_MAX_DSPS 8
struct delayed_work { int unused; };
struct device { int unused; };
struct pci_dev { struct device dev; };
struct ua_dsp_ring {
	unsigned int write_ptr, hw_pos, capacity;
	u32 reg_base, gen;
	bool live;
};
struct ua_dsp_state { struct ua_dsp_ring cmd, resp; bool rings_allocated; };
struct ua_plugin_host;
struct ua_device {
	struct pci_dev *pdev;
	unsigned int num_dsps;
	struct ua_dsp_state dsp[UA_MAX_DSPS];
	struct mutex lock;
	atomic_t shutdown;
	struct ua_plugin_host *host;
};
struct task_struct { int unused; };
static struct task_struct *current;
static bool fatal_signal_pending(struct task_struct *t) { (void)t; return false; }
'''

FPGA = r'''
/* ---- the fake FPGA: DSP 0's two rings ---- */
static struct ua_device dev;
static struct ua_plugin_host host;
static struct pci_dev pci;
static u32 slots[8][1024][4];		/* [2n] DSP n's cmd, [2n + 1] its resp */
static unsigned int target[8];		/* doorbell values */
static unsigned int per_tick = 1000;	/* entries consumed per sleep */
static bool stalled;
static unsigned int bells[16], nbells;	/* 0 cmd, 1 resp, in order */
static unsigned int nputs, fail_put_at = ~0u;
static unsigned int cmd_left_[4];	/* dwords left of the current command */
static unsigned int replies_made;
static u32 reply_status;		/* word 1 of upload replies */
static unsigned int sleeps_locked;
static unsigned int dma_live, dma_allocs;
static unsigned int consumed, consume_limit = ~0u;	/* stall after N entries */
/* The board query's answer (a synthetic table: it tests the parser only) */
enum { BQ_OK, BQ_BAD_HEADER, BQ_BAD_ECHO, BQ_SILENT };
static int bq_mode __maybe_unused;
static u32 bq_table[768] __maybe_unused;
static u32 bq_dates[8] __maybe_unused;
static unsigned int bq_ndates __maybe_unused, bq_answered __maybe_unused;

static int ringi(const struct ua_dsp_ring *r)
{
	for (int n = 0; n < 4; n++) {
		if (r == &dev.dsp[n].cmd)
			return 2 * n;
		if (r == &dev.dsp[n].resp)
			return 2 * n + 1;
	}
	return -1;
}

static void *dma_alloc_coherent(struct device *d, size_t n, dma_addr_t *dma, int f)
{
	void *p;

	(void)d; (void)f;
	p = aligned_alloc(4096, PAGE_ALIGN(n));
	assert(p);
	memset(p, 0xee, PAGE_ALIGN(n));
	*dma = (dma_addr_t)(uintptr_t)p;	/* the fake reads through it */
	dma_live++;
	dma_allocs++;
	return p;
}
static void dma_free_coherent(struct device *d, size_t n, void *p, dma_addr_t dma)
{
	(void)d; (void)n;
	assert((dma_addr_t)(uintptr_t)p == dma);
	assert(dma_live);
	dma_live--;
	free(p);
}

unsigned int ua_dsp_ring_free_slots(const struct ua_dsp_ring *r)
{
	return (r->hw_pos - r->write_ptr - 1) & (r->capacity - 1);
}
unsigned int ua_dsp_ring_pending(const struct ua_dsp_ring *r)
{
	return (r->write_ptr - r->hw_pos) & (r->capacity - 1);
}
static unsigned int pos[8];
int ua_dsp_ring_update_pos(struct ua_device *ua, struct ua_dsp_ring *r)
{
	assert(ua->lock.held);
	r->hw_pos = pos[ringi(r)];
	return 0;
}
int ua_dsp_ring_put_words(struct ua_device *ua, struct ua_dsp_ring *r,
			  u32 w0, u32 w1, u32 w2, u32 w3)
{
	int i = ringi(r);

	assert(ua->lock.held && i >= 0);
	if (nputs++ == fail_put_at)
		return -ENOSPC;
	if (!ua_dsp_ring_free_slots(r))
		return -ENOSPC;
	slots[i][r->write_ptr][0] = w0;
	slots[i][r->write_ptr][1] = w1;
	slots[i][r->write_ptr][2] = w2;
	slots[i][r->write_ptr][3] = w3;
	r->write_ptr = (r->write_ptr + 1) & (r->capacity - 1);
	return 0;
}
void ua_dsp_ring_doorbell(struct ua_device *ua, struct ua_dsp_ring *r, u32 base)
{
	assert(ua->lock.held && base == r->reg_base);
	target[ringi(r)] = r->write_ptr;
	bells[nbells++ & 15] = ringi(r);
}
int ua_dsp_ring_resync_now(struct ua_device *ua, struct ua_dsp_ring *r)
{
	int i = ringi(r);

	r->hw_pos = pos[i];
	if (ua_dsp_ring_pending(r)) {
		ua_xact_ring_abandon(ua, r, r->gen + 1);
		r->gen++;
	}
	r->write_ptr = r->hw_pos;
	target[i] = r->hw_pos;
	return 0;
}

static u32 *bus(u32 lo, u32 hi) { return (u32 *)(uintptr_t)(((u64)hi << 32) | lo); }

/* DSP @d's next reply descriptor gets @n words */
static void reply_on(unsigned int d, const u32 *w, unsigned int n)
{
	unsigned int r = 2 * d + 1;
	u32 *e = slots[r][pos[r]];

	assert(pos[r] != target[r]);		/* a descriptor was waiting */
	assert(e[0] == (UA_RING_DMA_REF | n));
	memcpy(bus(e[2], e[3]), w, n * 4);
	pos[r] = (pos[r] + 1) & 1023;
	replies_made++;
}

/* DSP @d consumes command entries; commands that answer get their reply */
static void fpga_tick_dsp(unsigned int d)
{
	unsigned int n = 0, c = 2 * d;
	unsigned int *cmd_left = &cmd_left_[d];

	while (!stalled && pos[c] != target[c] && n++ < per_tick &&
	       consumed < consume_limit) {
		u32 *e = slots[c][pos[c]];

		consumed++;
		if (e[0] & UA_RING_DMA_REF) {
			unsigned int dw = e[0] & 0xffff;
			u32 *p = bus(e[2], e[3]);
			uintptr_t a = (uintptr_t)p;

			/* one piece never crosses a 4 KiB page, never 1 KiB pieces */
			assert(dw && dw <= 1024 && a / 4096 == (a + dw * 4 - 1) / 4096);
			/* the commands in it, one after the other */
			for (unsigned int i = 0, take; i < dw; i += take) {
				if (!*cmd_left) {
					unsigned int op = p[i] >> 16;

					*cmd_left = p[i] & 0xffff;
					assert(*cmd_left);
					if (op == 0x1 || op == 0x4) {
						u32 r[4] = { 0x80070004, reply_status,
							     p[i + 3], p[i] };

						reply_on(d, r, 4);
					}
				}
				take = min(*cmd_left, dw - i);
				*cmd_left -= take;
			}
		} else if (e[0] == 0x00100002) {
			bq_dates[bq_ndates++ & 7] = e[1];
		} else if (e[0] == 0x00110002 && bq_mode != BQ_SILENT) {
			static u32 table[770];

			table[0] = bq_mode == BQ_BAD_HEADER ? 0x80030303 : 0x80030302;
			table[1] = bq_mode == BQ_BAD_ECHO ? e[1] ^ 1 : e[1];
			for (unsigned int i = 2; i < 770; i++)
				table[i] = bq_table[i - 2];
			reply_on(d, table, 770);
			bq_answered++;
		}
		pos[c] = (pos[c] + 1) & 1023;
	}
}

/* Consume command entries; commands that answer get their reply */
static void (*on_tick)(void) __maybe_unused;
static void fpga_tick(void)
{
	if (on_tick)
		on_tick();
	for (unsigned int d = 0; d < 4; d++)
		fpga_tick_dsp(d);
}

static void usleep_range(unsigned long a, unsigned long b)
{
	(void)a; (void)b;
	if (dev.lock.held)
		sleeps_locked++;
	jiffies++;
	fpga_tick();
}

static int quarantined[8] __maybe_unused, quarantine_err __maybe_unused;
#ifndef UA_REAL_QUARANTINE
void ua_plugin_quarantine(struct ua_device *ua, unsigned int dsp,
			  const char *why, int err)
{
	(void)why;
	assert(ua->lock.held);
	quarantined[dsp]++;
	quarantine_err = err;
}
#endif

/* DSPs 1-3 get live rings too (placement tests) */
static void rings_all(void)
{
	for (unsigned int d = 1; d < 4; d++) {
		dev.dsp[d].rings_allocated = true;
		dev.dsp[d].cmd = (struct ua_dsp_ring){ .capacity = 1024,
			.reg_base = 0x800 * d + 0x20, .live = true };
		dev.dsp[d].resp = (struct ua_dsp_ring){ .capacity = 1024,
			.reg_base = 0x800 * d + 0x60, .live = true };
	}
}

static void reset(void)
{
	memset(&dev, 0, sizeof(dev));
	memset(&host, 0, sizeof(host));
	memset(slots, 0, sizeof(slots));
	dev.pdev = &pci;
	dev.num_dsps = 4;
	dev.host = &host;
	host.ua = &dev;
	host.epoch = 5;
	INIT_LIST_HEAD(&host.xacts);
	dev.dsp[0].rings_allocated = true;
	dev.dsp[0].cmd = (struct ua_dsp_ring){ .capacity = 1024, .reg_base = 0x20,
					       .live = true, .write_ptr = 1000, .hw_pos = 1000 };
	dev.dsp[0].resp = (struct ua_dsp_ring){ .capacity = 1024, .reg_base = 0x60,
						.live = true, .write_ptr = 7, .hw_pos = 7 };
	memset(pos, 0, sizeof(pos));
	memset(target, 0, sizeof(target));
	pos[0] = target[0] = 1000;
	pos[1] = target[1] = 7;
	stalled = false;
	per_tick = 1000;
	consumed = 0;
	consume_limit = ~0u;
	bq_mode = BQ_OK;
	on_tick = NULL;
	bq_ndates = bq_answered = 0;
	for (unsigned int i = 0; i < 768; i++)
		bq_table[i] = 0x83000000;
	nbells = nputs = replies_made = sleeps_locked = 0;
	fail_put_at = ~0u;
	memset(cmd_left_, 0, sizeof(cmd_left_));
	reply_status = 0;
	memset(quarantined, 0, sizeof(quarantined));
}

/* An upload command: [0x0001nnnn, dest, "Bill", id, ...] of @dw dwords */
static struct ua_xact *upload(unsigned int dw, u32 id)
{
	struct ua_xact *x = ua_xact_alloc(0, 64 * 1024);
	__le32 *w;

	assert(x);
	x->epoch = host.epoch;
	assert(ua_xact_rx(x, 4) == 0);
	w = ua_xact_cmd(x, dw);
	assert(w);
	w[0] = 0x00010000 | dw;
	w[1] = 0xe0000;
	w[2] = 0x6c6c6942;
	w[3] = id;
	for (unsigned int i = 4; i < dw; i++)
		w[i] = i;
	return x;
}

static void locked_done(struct ua_xact *x)
{
	mutex_lock(&dev.lock);
	ua_xact_done(&dev, x);
	mutex_unlock(&dev.lock);
}

static void locked_reclaim(void)
{
	mutex_lock(&dev.lock);
	ua_xact_reclaim(&dev);
	mutex_unlock(&dev.lock);
}
'''


def run(main):
    code = (khost.PREFIX + FAKE + khost.header('ua_resource_pool.h') +
            khost.header('ua_plugin_host.h') + FPGA +
            khost.body('ua_dsp_xact.c') + main)
    return csrc.build_and_run(code, extra=('-fsanitize=address,undefined',
                                           '-fno-sanitize-recover=all'))


def test_reply_before_command_and_wait_off_lock():
    run(r'''
int main(void)
{
	struct ua_xact *x;
	const __le32 *r;

	reset();
	x = upload(763, 0xeb);		/* 3052 bytes: one DMA_REF */
	assert(x->n == 1 && x->ent[0][0] == (UA_RING_DMA_REF | 763) &&
	       x->ent[0][2] == 0);
	host.mutex.held = 1;		/* the caller's host->mutex */
	assert(ua_xact_run(&dev, x, UA_XACT_WAIT_MS) == 0);
	assert(!sleeps_locked && !dev.lock.held);
	/* the reply descriptor was doorbelled before the command */
	assert(nbells == 2 && bells[0] == 1 && bells[1] == 0);
	/* reply descriptor: 4 dwords at the reply page, after the payload */
	assert(slots[1][7][0] == (UA_RING_DMA_REF | 4) && slots[1][7][1] == 0);
	assert(bus(slots[1][7][2], slots[1][7][3]) == (u32 *)((u8 *)x->cpu + 4096));
	/* the DMA_REF points into the transaction's own buffer */
	assert(bus(slots[0][1000][2], slots[0][1000][3]) == (u32 *)x->cpu);
	r = ua_xact_reply(x, 0);
	assert(r && r[0] == 0x80070004 && r[1] == 0 && r[2] == 0xeb &&
	       r[3] == 0x000102fb);
	assert(x->state == UA_XACT_RETIRED && !host.xact_waiting);
	assert(!quarantined[0]);
	locked_done(x);
	assert(!host.xact_count && !dma_live && !k_objects);
	return 0;
}
''')


def test_timeout_quarantines_and_retains_until_proven():
    run(r'''
int main(void)
{
	struct ua_xact *x, *y;

	reset();
	host.mutex.held = 1;
	stalled = true;
	x = upload(100, 0xda);
	assert(ua_xact_run(&dev, x, 30) == -ETIMEDOUT);
	assert(quarantined[0] == 1 && quarantine_err == -ETIMEDOUT);
	assert(!sleeps_locked);
	locked_done(x);
	/* not proven: the buffer stays with the host, never freed under the device */
	assert(host.xact_count == 1 && dma_live == 1 && !host.xact_waiting);
	locked_reclaim();
	assert(host.xact_count == 1 && dma_live == 1);
	/* the FPGA catches up later: now it can go */
	stalled = false;
	fpga_tick();
	locked_reclaim();
	assert(!host.xact_count && !dma_live && !k_objects);

	/* the late reply went to the stale descriptor, not to the next one */
	stalled = true;
	x = upload(100, 0xda);
	assert(ua_xact_run(&dev, x, 10) == -ETIMEDOUT);
	locked_done(x);
	y = upload(50, 0xa5);
	stalled = false;
	/* y waits for the idle command ring, then gets its own reply */
	assert(ua_xact_run(&dev, y, UA_XACT_WAIT_MS) == 0);
	assert(ua_xact_reply(y, 0)[2] == 0xa5);
	locked_done(y);
	locked_reclaim();
	assert(!host.xact_count && !dma_live && !k_objects);
	return 0;
}
''')


def test_epoch_change_refuses_and_goes_stale():
    run(r'''
int main(void)
{
	struct ua_xact *x;
	unsigned int before;

	reset();
	host.mutex.held = 1;
	x = upload(10, 0xc2);
	x->epoch = 4;			/* built in an earlier epoch */
	before = nputs;
	assert(ua_xact_run(&dev, x, UA_XACT_WAIT_MS) == -ESTALE);
	assert(nputs == before && !nbells && !quarantined[0]);
	assert(x->state == UA_XACT_MAPPED);
	ua_xact_discard(&dev, x);
	assert(!dma_live && !k_objects);

	/* the epoch moves on while it is out: the outcome is unknown */
	stalled = true;
	x = upload(10, 0xc2);
	assert(ua_xact_map(&dev, x) == 0 && ua_xact_publish(&dev, x) == 0);
	mutex_lock(&dev.lock);
	host.epoch++;
	assert(ua_xact_poll(&dev, x) == -ESTALE);
	ua_xact_done(&dev, x);
	mutex_unlock(&dev.lock);
	assert(host.xact_count == 1);
	stalled = false;
	fpga_tick();
	locked_reclaim();
	assert(!host.xact_count && !dma_live && !k_objects);
	return 0;
}
''')


def test_resync_abandon_is_settled_like_the_orphans():
    run(r'''
int main(void)
{
	struct ua_xact *x;

	reset();
	host.mutex.held = 1;
	stalled = true;
	x = ua_xact_alloc(0, 0);
	x->epoch = host.epoch;
	for (int i = 0; i < 6; i++)
		ua_xact_inline(x, 0x00080004, 0x9d000 + i, 0, 8);
	assert(ua_xact_map(&dev, x) == 0 && ua_xact_publish(&dev, x) == 0);
	/* the FPGA took two, then the ring is resynced under the rest */
	stalled = false;
	per_tick = 2;
	fpga_tick();
	stalled = true;
	mutex_lock(&dev.lock);
	ua_dsp_ring_resync_now(&dev, &dev.dsp[0].cmd);
	assert(dev.dsp[0].cmd.gen == 1);
	assert(ua_xact_poll(&dev, x) == -ESTALE);
	ua_xact_done(&dev, x);
	ua_xact_reclaim(&dev);
	assert(host.xact_count == 1);	/* 4 abandoned, nothing consumed since */
	mutex_unlock(&dev.lock);
	/* four more entries consumed past the resync point prove it */
	pos[0] = (pos[0] + 3) & 1023;
	locked_reclaim();
	assert(host.xact_count == 1);
	pos[0] = (pos[0] + 1) & 1023;
	locked_reclaim();
	assert(!host.xact_count && !k_objects);
	return 0;
}
''')


def test_retention_is_bounded_and_refuses_only_new_work():
    run(r'''
int main(void)
{
	struct ua_xact *x;

	reset();
	host.mutex.held = 1;
	stalled = true;
	for (int i = 0; i < UA_XACT_RETAIN_MAX; i++) {
		x = ua_xact_alloc(0, 0);
		x->epoch = host.epoch;
		ua_xact_inline(x, 0x00080004, i, 0, 8);
		assert(ua_xact_map(&dev, x) == 0 && ua_xact_publish(&dev, x) == 0);
		locked_done(x);
	}
	assert(host.xact_count == UA_XACT_RETAIN_MAX);
	x = ua_xact_alloc(0, 0);
	x->epoch = host.epoch;
	ua_xact_inline(x, 0x00080004, 99, 0, 8);
	assert(ua_xact_map(&dev, x) == 0);
	assert(ua_xact_publish(&dev, x) == -EBUSY);	/* this request only */
	assert(x->state == UA_XACT_MAPPED);
	stalled = false;
	fpga_tick();
	assert(ua_xact_publish(&dev, x) == 0);		/* room again */
	assert(host.xact_count == 1);
	locked_done(x);
	fpga_tick();
	locked_reclaim();
	assert(!host.xact_count && !dma_live && !k_objects);
	return 0;
}
''')


def test_command_pieces_and_layout():
    run(r'''
int main(void)
{
	struct ua_xact *x;
	__le32 *a, *b, *c;

	reset();
	x = ua_xact_alloc(0, 64 * 1024);
	a = ua_xact_cmd(x, 19);			/* memspec: offset 0 */
	b = ua_xact_cmd(x, 1018);		/* 4072 B: would straddle -> page 1 */
	c = ua_xact_cmd(x, 1300);		/* 5200 B: two pieces at the page edge */
	assert(a && b && c && !x->err);
	assert((u8 *)b - x->stage == 4096 && (u8 *)c - x->stage == 4096 + 4072);
	assert(x->n == 5);
	assert(x->ent[0][0] == (UA_RING_DMA_REF | 19) && x->ent[0][2] == 0);
	assert(x->ent[1][0] == (UA_RING_DMA_REF | 1018) && x->ent[1][2] == 4096);
	assert(x->ent[2][0] == (UA_RING_DMA_REF | 6) && x->ent[2][2] == 8168);
	assert(x->ent[3][0] == (UA_RING_DMA_REF | 1024) && x->ent[3][2] == 8192);
	assert(x->ent[4][0] == (UA_RING_DMA_REF | 270) && x->ent[4][2] == 12288);
	for (unsigned int i = 0; i < x->n; i++)
		assert(x->ent[i][1] == 0 && x->ent[i][3] == 0);
	/* bit 31 is never an inline command; no room is an error, not a cut */
	ua_xact_inline(x, 0x80000004, 0, 0, 0);
	assert(x->err == -EINVAL);
	ua_xact_discard(&dev, x);
	x = ua_xact_alloc(0, 4096);
	assert(ua_xact_cmd(x, 1025) == NULL && x->err == -ENOSPC);
	ua_xact_discard(&dev, x);
	x = ua_xact_alloc(0, 0);
	for (int i = 0; i < UA_XACT_ENTS_MAX + 1; i++)
		ua_xact_inline(x, 0x00080004, 0, 0, 1);
	assert(x->err == -E2BIG && x->n == UA_XACT_ENTS_MAX);
	ua_xact_discard(&dev, x);
	assert(!ua_xact_alloc(0, UA_XACT_PAYLOAD_MAX + 1));
	/* replies never cross a page */
	x = ua_xact_alloc(0, 0);
	assert(ua_xact_rx(x, 4) == 0 && ua_xact_rx(x, 770) == 1 &&
	       ua_xact_rx(x, 770) == 2);
	assert(x->rx_off[0] == 0 && x->rx_off[1] == 16 && x->rx_off[2] == 4096);
	ua_xact_discard(&dev, x);
	assert(!k_objects);
	return 0;
}
''')


def test_publication_rules_and_rollback():
    run(r'''
int main(void)
{
	struct ua_xact *x, *t;

	reset();
	host.mutex.held = 1;
	/* a TX-only transaction is still queued: the reply one waits for it */
	stalled = true;
	t = ua_xact_alloc(0, 0);
	t->epoch = host.epoch;
	ua_xact_inline(t, 0x001f0004, 0x00200000, 0x9d1b0, 0);
	assert(ua_xact_map(&dev, t) == 0 && ua_xact_publish(&dev, t) == 0);
	x = upload(10, 0x12b);
	assert(ua_xact_map(&dev, x) == 0);
	mutex_lock(&dev.lock);
	assert(ua_xact_submit(&dev, x) == -EAGAIN);
	mutex_unlock(&dev.lock);
	assert(ua_xact_publish(&dev, x) == -EBUSY);	/* 50 ms, then busy */
	stalled = false;
	assert(ua_xact_publish(&dev, x) == 0);
	assert(ua_xact_wait(&dev, x, 100) == 0);
	assert(ua_xact_reply(x, 0)[2] == 0x12b);
	locked_done(x);
	locked_done(t);
	locked_reclaim();

	/* a reply descriptor somebody left is abandoned before ours */
	slots[1][pos[1]][0] = UA_RING_DMA_REF | 4;
	dev.dsp[0].resp.write_ptr = (dev.dsp[0].resp.write_ptr + 1) & 1023;
	target[1] = dev.dsp[0].resp.write_ptr;
	x = upload(10, 0x0c2);
	assert(ua_xact_run(&dev, x, 100) == 0);
	assert(dev.dsp[0].resp.gen == 1 && ua_xact_reply(x, 0)[2] == 0x0c2);
	locked_done(x);

	/* a put that fails midway: producers back, nothing doorbelled */
	x = upload(10, 0x0eb);
	assert(ua_xact_map(&dev, x) == 0);
	{
		unsigned int c = dev.dsp[0].cmd.write_ptr, r = dev.dsp[0].resp.write_ptr;
		unsigned int nb = nbells;

		fail_put_at = nputs + 1;		/* the reply goes in, the command not */
		mutex_lock(&dev.lock);
		assert(ua_xact_submit(&dev, x) == -EAGAIN);
		mutex_unlock(&dev.lock);
		assert(dev.dsp[0].cmd.write_ptr == c && dev.dsp[0].resp.write_ptr == r);
		assert(nbells == nb && x->state == UA_XACT_MAPPED);
	}
	fail_put_at = ~0u;
	assert(ua_xact_publish(&dev, x) == 0);
	assert(ua_xact_wait(&dev, x, 100) == 0 && ua_xact_reply(x, 0)[2] == 0x0eb);
	locked_done(x);

	/* no rings, a gone device, a bad DSP */
	x = upload(10, 0x0eb);
	assert(ua_xact_map(&dev, x) == 0);
	mutex_lock(&dev.lock);
	x->dsp = 3;
	assert(ua_xact_submit(&dev, x) == -ENODEV);
	x->dsp = 9;
	assert(ua_xact_submit(&dev, x) == -EINVAL);
	x->dsp = 0;
	dev.shutdown.counter = 1;
	assert(ua_xact_submit(&dev, x) == -ENODEV);
	dev.shutdown.counter = 0;
	mutex_unlock(&dev.lock);
	ua_xact_discard(&dev, x);
	locked_reclaim();
	assert(!host.xact_count && !dma_live && !k_objects && !host.xact_waiting);
	return 0;
}
''')


def test_fini_frees_only_what_the_device_cannot_touch():
    run(r'''
int main(void)
{
	struct ua_xact *x, *y;
	void *leak;

	reset();
	host.mutex.held = 1;
	stalled = true;

	x = upload(10, 0x0eb);
	assert(ua_xact_run(&dev, x, 5) == -ETIMEDOUT);
	locked_done(x);
	leak = x->cpu;
	y = upload(10, 0x0da);		/* built, never published */
	ua_xact_discard(&dev, y);
	ua_xact_fini(&dev, false);
	assert(K_HAS("leaking 1 transaction buffer(s)"));
	assert(!host.xact_count && dma_live == 1);	/* x's buffer leaked */
	reset();
	stalled = true;
	host.mutex.held = 1;
	x = upload(10, 0x0eb);
	assert(ua_xact_run(&dev, x, 5) == -ETIMEDOUT);
	ua_xact_fini(&dev, true);	/* device quiet: the buffer goes */
	assert(!host.xact_count && x->detached && !x->cpu);
	locked_done(x);			/* the caller's struct */
	assert(dma_live == 1);		/* only the first test's leak */
	free(leak);			/* (the test's own cleanup) */
	assert(!k_objects);
	return 0;
}
''')


def test_source_rules():
    src = csrc.source('ua_dsp_xact.c')
    # never the firmware/PCM orphan list, no global latch
    assert 'dma_orphan' not in src and 'orphan_dma' not in src
    assert 'untracked' not in src
    # never a sleep with ua->lock held: every sleep is in a function that
    # dropped it first
    wait = csrc.extract(src, 'int ua_xact_wait(')
    assert wait.index('mutex_unlock(&ua->lock);') < wait.index('usleep_range')
    pub = csrc.extract(src, 'int ua_xact_publish(')
    assert pub.index('mutex_unlock(&ua->lock);') < pub.index('usleep_range')
    for fn in ('int ua_xact_submit(', 'int ua_xact_poll(',
               'void ua_xact_done(', 'void ua_xact_reclaim('):
        body = csrc.extract(src, fn)
        assert 'sleep' not in body and 'msleep' not in body, fn
    # one DMA_REF per 4 KiB at most, never the send_block chunk
    assert 'sendblock_chunk' not in src and 'ua_dsp_dma_chunk' not in src
    hook = csrc.extract(csrc.source('ua_dsp.c'), 'static void ua_dsp_orphans_abandon(')
    assert 'ua_xact_ring_abandon(ua, ring, next);' in hook
    fini = csrc.extract(csrc.source('ua_dsp.c'), 'void ua_dsp_rings_fini(')
    assert 'ua_xact_fini(ua, dma_quiet);' in fini
