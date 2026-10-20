/* SPDX-License-Identifier: GPL-2.0-only */
/*
 * Universal Audio Apollo — plug-in host: shared definitions
 *
 * Copyright (c) 2026 apollo-linux contributors
 *
 * The plug-in host is everything that places resources on the DSPs: the
 * Console input strips on DSP 0 (input_strips=1) and plug-in instances
 * (plugin_host=1).  Both are off by default; with both off the host only
 * keeps an epoch counter and answers caps with nothing.
 *
 * Locking: host->mutex serialises the long operations (a strip pass, a
 * plug-in CREATE or DESTROY, a board query); it is taken before ua->lock,
 * never while holding it.  ua->lock covers ring publication and every
 * field marked "ua->lock" below, and is never held across a wait for the
 * device: a transaction is published under ua->lock and waited for with
 * the lock released (ua_xact_wait()).
 */
#ifndef UA_PLUGIN_HOST_H
#define UA_PLUGIN_HOST_H

#include <linux/list.h>
#include <linux/mutex.h>
#include <linux/types.h>
#include <linux/workqueue.h>

#include "ua_resource_pool.h"

struct ua_device;
struct ua_file;
struct snd_info_buffer;
struct ua_strips;

#define UA_HOST_DSPS		8	/* UA_MAX_DSPS */

/* ---- module parameters (ua_plugin_host.c) ---- */
bool ua_plugin_host_enabled(void);	/* plugin_host=1 */

/* ---- per-DSP ownership ---- */
struct ua_host_dsp {
	bool owned;		/* ua->lock: see ua_plugin_dsp_start() */
	bool quarantined;	/* ua->lock: an outcome that cannot be proven */
	bool geom_read;		/* the pool registers were read */
	bool seen;		/* a start was seen for this device instance */
	u32 boot_word;		/* at the last cold start */
	char why[96];		/* the last ownership decision */
	u32 regs[11];		/* pool registers: size 0-3, base 0-3, scratch 1-3 */
	struct ua_pool_geometry geom[UA_POOLS];
	struct ua_pool_owner *pools;	/* owned and geometry read */
	/* traffic since probe (proc): what this host sent to the DSP */
	u32 tx_xacts, uploads, replies_ok, replies_bad, zero_fills;
	u32 memspecs, writes, releases, stops, timeouts;
};

/* Accepted setting images. Physical readback covers flags and GainC only. */
#define UA_UNISON_SETTINGS 38
struct ua_unison_image {
	u32 value[UA_UNISON_SETTINGS], mask[UA_UNISON_SETTINGS];
	u32 synch;
	bool valid;
};

struct ua_plugin_host {
	struct ua_device *ua;
	struct mutex mutex;		/* long operations; before ua->lock */
	u64 epoch;			/* ua->lock; never 0 */
	bool active;			/* plugin_host or input_strips */
	bool phantom_pending;		/* cold start: 48V not re-applied yet (ua->lock) */
	struct ua_host_dsp dsp[UA_HOST_DSPS];

	/* ua_dsp_xact.c: published transactions not yet retired (ua->lock) */
	struct list_head xacts;
	unsigned int xact_count;
	size_t xact_bytes;
	unsigned int xact_waiting;	/* published with replies, being waited */
	u32 xact_leaked;

	/* ua_plugin_catalog.c: module copies, keyed (id, arch, sha256) */
	struct list_head catalog;	/* ua->lock */
	unsigned int catalog_count;

	/* ua_plugin_instance.c: every instance of every open file (ua->lock) */
	struct list_head instances;
	unsigned int n_instances;
	u32 next_handle;
	bool analog_write; /* ua->lock: narrowly scoped internal lease write */
	u32 analog_block[UA_UNISON_SETTINGS]; /* unknown completion: until reprobe */

	/* ua_plugin_license.c: queries and decisions since probe */
	u32 board_queries, board_query_failures, admitted, refused;

	/*
	 * The last teardown published on each DSP and not yet proven taken
	 * (ua->lock).  Nothing new is placed on a DSP before its fence is
	 * proven (ua_plugin_fence_wait()); one that is not, quarantines it.
	 */
	struct ua_xact *fence[UA_HOST_DSPS];

	struct ua_strips *strips;	/* ua_strips.c, input_strips=1 only */
	struct delayed_work work;	/* strip passes */
};

/* ---- ring primitives exported by ua_dsp.c for ua_dsp_xact.c ---- */
struct ua_dsp_ring;
unsigned int ua_dsp_ring_free_slots(const struct ua_dsp_ring *ring);
int ua_dsp_ring_update_pos(struct ua_device *ua, struct ua_dsp_ring *ring);
unsigned int ua_dsp_ring_pending(const struct ua_dsp_ring *ring);
int ua_dsp_ring_put_words(struct ua_device *ua, struct ua_dsp_ring *ring,
			  u32 w0, u32 w1, u32 w2, u32 w3);
void ua_dsp_ring_doorbell(struct ua_device *ua, struct ua_dsp_ring *ring,
			  u32 ring_base);
void ua_dsp_ring_program(struct ua_device *ua, struct ua_dsp_ring *ring,
			 u32 ring_base);
int ua_dsp_ring_resync_now(struct ua_device *ua, struct ua_dsp_ring *ring);

/* ---- transactions (ua_dsp_xact.c) ----
 *
 * One request to one DSP: inline commands and DSP commands carried by DMA
 * pages on its command ring, and optionally reply descriptors on its
 * response ring, published together (replies first) under ua->lock with
 * one doorbell per ring.  The coherent buffer behind them belongs to the
 * transaction until both rings are provably past it, whatever happens to
 * the caller: a transaction that is not retired when its caller is done
 * stays on the host's own bounded list (never the firmware/PCM orphans).
 */
#define UA_XACT_ENTS_MAX	128	/* command-ring entries, a quarter ring at most */
#define UA_XACT_RX_MAX		8	/* reply descriptors */
#define UA_XACT_RX_WORDS_MAX	1024	/* dwords of one reply descriptor */
#define UA_XACT_PAYLOAD_MAX	(256u << 10)
#define UA_XACT_PIECE		4096	/* one DMA_REF never crosses a 4 KiB page */
#define UA_XACT_RETAIN_MAX	32	/* unretired transactions kept, at most */
#define UA_XACT_RETAIN_BYTES	(8u << 20)
#define UA_XACT_WAIT_MS		1000	/* the plan's 1 s bound */
#define UA_XACT_PUBLISH_MS	50	/* room / an idle command ring, off-lock */
#define UA_XACT_RESYNC_RX	4	/* a resync probe's reply descriptors */

enum ua_xact_state {
	UA_XACT_BUILDING,	/* entries and payload being laid out */
	UA_XACT_MAPPED,		/* coherent buffer filled, nothing published */
	UA_XACT_PUBLISHED,	/* doorbelled: the device may read/write it */
	UA_XACT_RETIRED,	/* both rings past it: replies valid */
};

struct ua_xact_ref {
	struct ua_dsp_ring *ring;	/* NULL: this ring is done with it */
	u32 gen;
	unsigned int end;		/* producer index just past our entries */
	unsigned int start, need;	/* abandoned by a resync */
	bool abandoned;
};

struct ua_xact {
	struct list_head node;		/* host->xacts while published */
	unsigned int dsp;
	u64 epoch;			/* host epoch it was built in */
	int err;			/* sticky builder error */
	enum ua_xact_state state;
	bool released;			/* the caller is done with it */
	bool waiting;			/* counted in host->xact_waiting */
	bool detached;			/* ua_xact_fini() took it off the list */
	/* layout */
	unsigned int n;
	u32 ent[UA_XACT_ENTS_MAX][4];	/* DMA_REF entries: word 2 = offset */
	u8 *stage;			/* payload staging (kvmalloc) */
	size_t cap, len;
	unsigned int n_rx;
	u32 rx_words[UA_XACT_RX_MAX];
	u32 rx_off[UA_XACT_RX_MAX];	/* bytes into the reply area */
	size_t rx_len;
	u32 rx_fill;			/* the reply area's initial word */
	/* mapping */
	void *cpu;
	dma_addr_t dma;
	size_t size, rx_base;
	/* publication */
	struct ua_xact_ref ref[2];	/* [0] command ring, [1] response ring */
	unsigned long published;	/* jiffies */
};

struct ua_xact *ua_xact_alloc(unsigned int dsp, size_t payload_cap);
void ua_xact_inline(struct ua_xact *x, u32 w0, u32 w1, u32 w2, u32 w3);
__le32 *ua_xact_cmd(struct ua_xact *x, unsigned int dwords);
int ua_xact_rx(struct ua_xact *x, unsigned int dwords);
int ua_xact_map(struct ua_device *ua, struct ua_xact *x);	/* no lock */
int ua_xact_submit(struct ua_device *ua, struct ua_xact *x);	/* ua->lock */
int ua_xact_poll(struct ua_device *ua, struct ua_xact *x);	/* ua->lock */
int ua_xact_publish(struct ua_device *ua, struct ua_xact *x);	/* no lock */
int ua_xact_wait(struct ua_device *ua, struct ua_xact *x,
		 unsigned int timeout_ms);			/* no lock */
int ua_xact_run(struct ua_device *ua, struct ua_xact *x,
		unsigned int timeout_ms);	/* host->mutex, no ua->lock */
const __le32 *ua_xact_reply(const struct ua_xact *x, unsigned int i);
void ua_xact_report_reply(struct ua_device *ua, const struct ua_xact *x,
			  unsigned int i, const char *what);	/* ua->lock */
int ua_xact_resync_replies(struct ua_device *ua, unsigned int dsp, u64 epoch,
			   const u32 cmd[4], u32 hdr);	/* host->mutex, no ua->lock */
void ua_xact_done(struct ua_device *ua, struct ua_xact *x);	/* ua->lock */
void ua_xact_discard(struct ua_device *ua, struct ua_xact *x);	/* unpublished */
void ua_xact_reclaim(struct ua_device *ua);			/* ua->lock */
void ua_xact_ring_abandon(struct ua_device *ua, struct ua_dsp_ring *ring,
			  u32 next);					/* ua->lock */
void ua_xact_fini(struct ua_device *ua, bool dma_quiet);	/* rings fini */

/* ---- the module catalog (ua_plugin_catalog.c) ----
 *
 * DSP modules ("Bill" records) as the user's own UA install carries them,
 * read by the driver itself from the module store (never from user space):
 *   universal-audio/plugins/modules/<sha256>.bill   any plug-in's module
 *   universal-audio/dsp/<id>.bill                   the Console strip modules
 * (/lib/firmware; `oa-assets dsp` and `oa-assets firmware` write them).
 * Each copy is checked as UAD2System's IsValid does before anything else
 * looks at it, and a store file whose SHA-256 is not its name is refused.
 * Entries are keyed (id, arch, sha256); where an entry is resident is kept
 * per DSP together with the epoch it was uploaded in, so after an epoch
 * change it is uploaded again (overlay review finding 14).
 */
#define UA_BILL_HDR		20	/* "Bill", id, w2, payload bytes, w4 */
#define UA_BILL_MAGIC		0x6c6c6942
#define UA_BILL_MAX		((256u << 10) - 8)	/* one upload transaction: PAGE_ALIGN(len + 8) <= UA_XACT_PAYLOAD_MAX */
#define UA_CATALOG_MAX		256
#define UA_CATALOG_BYTES	(32u << 20)

struct ua_bill_info {
	u32 id;			/* the record's id word: 24 bits on disk */
	u32 w2;			/* kind (7:0), flags (23:8), architecture (31:24) */
	u32 kind, flags, arch;
	u32 w4;			/* functional words */
	u32 words;		/* pool words: floor(2 * w4 / 3) for code, else w4 */
	u32 pool;		/* 0 code, 1 data (kinds 1 and 4), 2, 3 */
};

enum ua_res_state {
	UA_RES_NONE,		/* not on that DSP (or from an earlier epoch) */
	UA_RES_RESIDENT,	/* uploaded, the DSP's reply validated */
	UA_RES_UNKNOWN,		/* an upload or release ended unproven */
};

struct ua_cat_res {
	u64 epoch;		/* the epoch the state belongs to */
	u8 state;		/* enum ua_res_state */
	u32 base;		/* where it is */
};

struct ua_cat_entry {
	struct list_head node;	/* host->catalog */
	u8 sha256[32];
	struct ua_bill_info info;
	size_t len;
	u8 *blob;		/* the record as stored on disk */
	unsigned int users;	/* strips and instances holding it (ua->lock) */
	struct ua_cat_res res[UA_HOST_DSPS];	/* ua->lock */
};

int ua_bill_check(const u8 *data, size_t len, struct ua_bill_info *out);
int ua_catalog_get(struct ua_device *ua, const u8 sha256[32],
		   struct ua_cat_entry **out);			/* no lock */
int ua_catalog_get_system(struct ua_device *ua, u32 id,
			  struct ua_cat_entry **out);		/* no lock */
void ua_catalog_put(struct ua_device *ua, struct ua_cat_entry *e); /* ua->lock */
bool ua_catalog_resident(const struct ua_cat_entry *e, unsigned int dsp,
			 u64 epoch);
void ua_catalog_free_all(struct ua_device *ua);			/* exclusive */

/* ---- DSP ownership, geometry, placement (ua_plugin_place.c) ----
 *
 * Only the host's own ledger knows what is free on a DSP (the pools live
 * host-side; the registers only give the static map), so a DSP is this
 * host's to use only when it found the DSP freshly powered: at its last
 * cold start the DSP's boot word was exactly 3 (no host since power-up),
 * and nothing outside the host (a raw DSP ioctl, a firmware load) has
 * touched it since.  DSP 0 is shared with the input strips through the
 * same ledger; the mixer itself is not in the pools.
 */
#define UA_DSP_WIN_BOOT		0x1a4	/* boot/ready word */
#define UA_DSP_BOOT_POWERUP	0x00000003
#define UA_DSP_WIN_SIZE		0x010	/* pool n capacity: +0x10 + 4n */
#define UA_DSP_WIN_BASE		0x184	/* pool n base: +0x184 + 4n */
#define UA_DSP_WIN_SCRATCH	0x190	/* pool n scratch: +0x190 + 4n, n >= 1 */

u32 ua_plugin_dsp_window(unsigned int dsp);
void ua_plugin_dsp_start(struct ua_device *ua, bool cold);	/* probe/ua->lock */
void ua_plugin_taint(struct ua_device *ua, const char *why);	/* no lock */
void ua_plugin_disown(struct ua_device *ua, unsigned int dsp,
		      const char *why);				/* ua->lock */
int ua_plugin_read_geometry(struct ua_device *ua, unsigned int dsp); /* ua->lock */
bool ua_plugin_dsp_admitted(struct ua_device *ua, unsigned int dsp); /* ua->lock */
int ua_plugin_place(struct ua_device *ua, unsigned int want,
		    const struct ua_pool_request *rq, unsigned int n,
		    unsigned int *dsp, struct ua_pool_lease *leases); /* ua->lock */
int ua_plugin_unplace(struct ua_device *ua, unsigned int dsp,
		      const struct ua_pool_lease *leases, unsigned int n);
void ua_plugin_proc(struct ua_device *ua, struct snd_info_buffer *buf);
struct snd_card;
int ua_plugin_proc_init(struct ua_device *ua, struct snd_card *card);

/* ---- upload, zero-fill, memspec, release (ua_plugin_upload.c) ----
 *
 * Resource upload (CResourcePool::_transmitResource, Mac kext 0x33da0;
 * framing at 0x34154-0x34174):
 *   word 0  (pool 0 ? 0x00010000 : 0x00040000) | (record bytes / 4 + 2)
 *   word 1  the DSP address the ledger gave
 *   then    the whole record, normalized
 * and the DSP's reply (accepted at 0x34708-0x34780, Windows 0xd4ff..):
 *   {0x80070004, 0, resource id, word 3 not interpreted}
 * which every upload waits for (review finding 17: the strips did not).
 * Zero-fill {0x00080004, addr, 0, words}, memspec {0x0015nnnn, pairs...},
 * releases {0x00030002, id} (code) and {0x00060003, id, kind, 0} get no
 * reply: the command ring passing them is all there is.
 */
#define UA_CMD_UPLOAD_CODE	0x00010000
#define UA_CMD_UPLOAD_DATA	0x00040000
#define UA_REPLY_UPLOAD		0x80070004
#define UA_CMD_ZERO		0x00080004
#define UA_CMD_MEMSPEC		0x00150000
#define UA_CMD_WRITE		0x00140000
#define UA_CMD_RELEASE_CODE	0x00030002
#define UA_CMD_RELEASE_DATA	0x00060003
#define UA_MEMSPEC_ROWS_MAX	128

/*
 * How the record is normalized before it is sent.  UA_BILL_SDK is what
 * UAD2System sends for a registered record (the kext's copy clears bytes
 * 10 and 11: word 2's flag/architecture half); the genuine 1176 uploads
 * on titan were answered with the plain id (wave34).  UA_BILL_STRIP is
 * the input strips' recipe as built and run on titan (0e9990b): the
 * architecture byte set in the id word (what Plugin Support does in
 * memory on the Mac) and word 2 cleared.
 */
enum ua_bill_form {
	UA_BILL_SDK,
	UA_BILL_STRIP,
};

struct ua_memspec_row {
	u32 pool, dwords, link, flags;
};

u32 ua_upload_header(const struct ua_bill_info *b, size_t len);
int ua_upload_build(struct ua_xact *x, const struct ua_cat_entry *e,
		    u32 dest, enum ua_bill_form form);
int ua_upload_reply_check(const __le32 *r, const struct ua_cat_entry *e,
			  enum ua_bill_form form, u32 header, bool *echo);
int ua_memspec_compile(const struct ua_memspec_row *rows, unsigned int n,
		       const unsigned int *desc, unsigned int n_desc,
		       const u32 *addr, u32 *words);
void ua_release_build(struct ua_xact *x, const struct ua_bill_info *b);
int ua_plugin_upload(struct ua_device *ua, unsigned int dsp,
		     struct ua_cat_entry *e, u32 dest, enum ua_bill_form form,
		     u64 epoch);			/* host->mutex, no ua->lock */

/* ---- the device's authorization answer (ua_plugin_license.c) ----
 *
 * The board query, as UA's PCIe driver makes it (refresh 108f8): TX
 * {0x00100002, date} and {0x00110002, low32 of the owned request}, RX one
 * 0xc08-byte reply descriptor (0x80000302) prefilled with the unfilled-table
 * sentinel 0x83000000, accepted only after the response ring retired it and
 * only when word 0 is 0x80030302 and word 1 echoes the request; words 2..769
 * are the 768-word table, index render id - 0x800.  The decision is the
 * SDK's (CUADPlug topology 1951b0 over PCIe's records f640).  Nothing is
 * cached: every query and every CREATE asks the device again.
 */
#define UA_BQ_CMD_DATE		0x00100002
#define UA_BQ_CMD_QUERY		0x00110002
#define UA_BQ_REPLY		0x80030302
#define UA_BQ_WORDS		768
#define UA_BQ_RX_WORDS		(UA_BQ_WORDS + 2)
#define UA_BQ_SENTINEL		0x83000000
#define UA_BQ_BASE		0x800
#define UA_BQ_WAIT_MS		2000	/* the original: 20 x 100 ms */
#define UA_DATE_EPOCH		1167609600LL	/* 2007-01-01 00:00:00 UTC */

int ua_plugin_date(s64 unix_seconds, u32 *date);
u32 ua_plugin_license_state(u32 word);
bool ua_plugin_license_admits(u32 state);
int ua_plugin_board_query(struct ua_device *ua, u32 *words, u32 *date,
			  u64 *epoch);		/* host->mutex, no ua->lock */
int ua_plugin_admit(struct ua_device *ua, u32 render_id, u32 *state,
		    u32 *word, u64 *epoch);	/* host->mutex, no ua->lock */

int ua_plugin_admit_chain(struct ua_device *ua, const u32 *ids, unsigned int n,
			  u32 *state, u32 *word, u64 *epoch);

/* ---- plug-in instances (ua_plugin_instance.c) ---- */
void ua_instances_teardown(struct ua_device *ua, const char *why); /* ua->lock */
bool ua_instances_strips_busy(struct ua_device *ua); /* ua->lock */
void ua_instances_dsp_gone(struct ua_device *ua, unsigned int dsp); /* ua->lock */
void ua_plugin_file_release(struct ua_file *uf);	/* no lock */

bool ua_instances_analog_conflict(struct ua_device *, unsigned int, u32);
void ua_unison_set_owned_locked(struct ua_device *, u32, bool);
int ua_unison_capture_locked(struct ua_device *, u32, struct ua_unison_image *);
int ua_unison_apply_locked(struct ua_device *, const struct ua_unison_image *);
int ua_unison_patch_locked(struct ua_device *, struct ua_unison_image *, const u32 *, unsigned int);
int ua_unison_drain(struct ua_device *, u64, u32 *);
int ua_unison_restore(struct ua_device *, u64, const struct ua_unison_image *);

/* ---- Console input strips on DSP 0 (ua_strips.c, input_strips=1) ---- */
bool ua_strips_wanted(void);
void ua_strips_init(struct ua_device *ua);		/* probe */
bool ua_strips_model_known(u32 device_type);
void ua_strips_free(struct ua_device *ua);		/* host free */
void ua_strips_connected(struct ua_device *ua);	/* ua->lock */
void ua_strips_run(struct ua_device *ua);		/* host work */
void ua_strips_teardown(struct ua_device *ua, const char *why); /* ua->lock */
void ua_strips_lost(struct ua_device *ua);		/* ua->lock */
void ua_strips_service(struct ua_device *ua);		/* ua->lock */
int ua_strips_override_target(struct ua_device *ua, unsigned int dsp,
			     u32 synch, u64 epoch, u32 *generation,
			     u32 carrier); /* ua->lock */
int ua_strips_restore(struct ua_device *ua, struct ua_xact *x,
		      u32 synch, u32 generation); /* ua->lock */
bool ua_strips_live(struct ua_device *ua);		/* ua->lock */
void ua_strips_proc(struct ua_device *ua, struct snd_info_buffer *buf);

/* ---- epoch and quarantine (ua_plugin_host.c) ---- */
void ua_plugin_epoch_bump(struct ua_device *ua, const char *why); /* ua->lock */
void ua_plugin_quarantine(struct ua_device *ua, unsigned int dsp,
			  const char *why, int err);		/* ua->lock */

/* ---- ua_plugin_host.c ---- */
int ua_plugin_host_init(struct ua_device *ua);		/* probe */
void ua_plugin_connected(struct ua_device *ua);		/* ua->lock */
void ua_plugin_teardown(struct ua_device *ua, const char *why); /* ua->lock */
void ua_plugin_host_quiesce(struct ua_device *ua, const char *why,
			    bool present);			/* no lock */
void ua_plugin_service(struct ua_device *ua);		/* ua->lock, 10 ms */
void ua_plugin_device_lost(struct ua_device *ua, const char *why); /* no lock */
int ua_plugin_publish_now(struct ua_device *ua, struct ua_xact *x); /* ua->lock */
void ua_plugin_fence_set(struct ua_device *ua, unsigned int dsp,
			 struct ua_xact *x);			/* ua->lock */
int ua_plugin_fence_wait(struct ua_device *ua, unsigned int dsp); /* host->mutex */
void ua_plugin_host_free(struct ua_device *ua);		/* chardev release */
u64 ua_plugin_host_ops(struct ua_device *ua);		/* ua->lock */
void ua_plugin_host_caps(struct ua_device *ua, u32 *state, u32 *owned,
			 u32 *quarantined, u32 *instances);	/* ua->lock */
long ua_plugin_ioctl_board_query(struct ua_device *ua, unsigned long arg);
long ua_plugin_ioctl_instance(struct ua_device *ua, struct ua_file *uf,
			      unsigned long arg);

/* ua_core.c: re-apply 48V (off, then on) on the preamps in @mask the Apollo reports with 48V on */
void ua_preamp_phantom_resync(struct ua_device *ua, unsigned int mask);

long ua_plugin_ioctl_delay(struct ua_device *ua, unsigned long arg);
int ua_strips_delay(struct ua_device *ua, u64 *epoch, u32 *generation,
		    unsigned int fields, u32 values[3][4]);

#endif /* UA_PLUGIN_HOST_H */
