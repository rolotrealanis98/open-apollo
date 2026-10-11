// SPDX-License-Identifier: GPL-2.0-only
/*
 * Universal Audio Apollo — ALSA Audio Subsystem
 *
 * Copyright (c) 2026 apollo-linux contributors
 *
 * Implements:
 *  - DMA buffer allocation and scatter-gather table programming
 *  - DSP firmware connection handshake (0xACEFACE protocol)
 *  - Audio transport prepare/start/stop
 *  - Sample clock control
 *  - ALSA PCM device with playback and capture
 *
 * Protocol reference: docs/pcie-protocol/audio-transport.md
 */

#include <linux/module.h>
#include <linux/delay.h>
#include <linux/jiffies.h>
#include <linux/dma-mapping.h>
#include <linux/slab.h>
#include <linux/uio.h>
#include <linux/vmalloc.h>
#include <linux/version.h>
#include <linux/ktime.h>
#include <linux/pm_qos.h>
#include <sound/core.h>
#include <sound/pcm.h>
#include <sound/pcm_params.h>
#include <sound/control.h>
#include <sound/info.h>
#include <sound/tlv.h>
#include <sound/initval.h>

#include "ua_apollo.h"
#include "ua_ioctl.h"
#include "ua_models_gen.h"

/* Module parameters for channel count override (legacy layout only) */
static int play_ch = -1;
static int rec_ch = -1;
module_param(play_ch, int, 0444);
MODULE_PARM_DESC(play_ch, "Override playback channel count (legacy io_map=0 only; applies to EVERY Apollo, so debugging with one device only)");
module_param(rec_ch, int, 0444);
MODULE_PARM_DESC(rec_ch, "Override record channel count (legacy io_map=0 only; applies to EVERY Apollo)");

static int io_map = -1;
module_param(io_map, int, 0444);
MODULE_PARM_DESC(io_map,
		 "Channel layout (applies to every Apollo): -1 = auto (default: firmware IO descriptor on models verified with it, the Apollo x4; legacy elsewhere), 0 = legacy (route-table DMA counts, ALSA ch N = DMA slot N), 1 = firmware IO descriptor on every model, channels mapped by ID like the macOS kext");

static bool no_connect;
module_param(no_connect, bool, 0444);
MODULE_PARM_DESC(no_connect, "Skip ACEFACE connect at probe (for crash debugging)");

static bool warm_boot;
module_param(warm_boot, bool, 0444);
MODULE_PARM_DESC(warm_boot, "Skip FW load + ACEFACE, mark connected (warm boot from Windows/macOS)");

static bool skip_bus_coeff = true;
module_param(skip_bus_coeff, bool, 0444);
MODULE_PARM_DESC(skip_bus_coeff, "Skip BUS_COEFF (0x1D) in plugin chain (default: true)");

static bool no_plugins;
module_param(no_plugins, bool, 0444);
MODULE_PARM_DESC(no_plugins, "Skip plugin chain entirely (keep ACEFACE)");

/*
 * The macOS driver never writes PCI config space (UAD2System.kext only
 * reads it), and the April Thunderbolt link drops followed these writes
 * by ~3 ms.  Off by default; kept for anyone who depended on them.
 */
static bool pcie_tuning;
module_param(pcie_tuning, bool, 0444);
MODULE_PARM_DESC(pcie_tuning,
		 "Legacy PCIe tweaks: disable ASPM on the Apollo endpoint (via the ASPM core) and raise the completion timeout to 65-210 ms on the Apollo and every upstream bridge (default: off)");

/*
 * 64-bit audio DMA experiment (parity/18 s6.2 rank 6).  The probe keeps the
 * device at DMA_BIT_MASK(32) after an old note (a buffer at 0x19f800000
 * failed); Windows and macOS never constrain the range, and the SG table
 * already carries the high dword.  Off: byte-identical to before.
 */
static bool dma64_audio;
module_param(dma64_audio, bool, 0444);
MODULE_PARM_DESC(dma64_audio,
		 "Experiment: allocate the two 4 MiB audio rings with a 64-bit DMA mask (they may land above 4 GiB); DSP rings and everything else stay below 4 GiB (default: off)");

/*
 * Rate-change experiments (titan A/B, 2026-10): the kext changes the rate
 * with the transport stopped and never writes the clock on a plain
 * transport start (apollo-linux kext-rate-meters-hiz-2026-10-10.md).
 */
static int rate_change;
module_param(rate_change, int, 0644);
MODULE_PARM_DESC(rate_change,
		 "Rate change: 0 = soft reconnect, clock written after the transport starts, 48 kHz session before a Hi-band start from a Hi-band clock (default); 1 = as the kext: clock change with the transport stopped, wait for the firmware rate, rate ack to every DSP, 1.5 s hold");
static bool post_start_clock = true;
module_param(post_start_clock, bool, 0644);
MODULE_PARM_DESC(post_start_clock,
		 "Write the clock after every transport start (default: on; the kext only writes it on a rate or source change)");
static bool ax_control_ext;
module_param(ax_control_ext, bool, 0644);
MODULE_PARM_DESC(ax_control_ext,
		 "Start the transport with AX_CONTROL 0x20F (on) or 0xF (default: off, the kext's value for a unit on its own clock; 0x20F is for a unit clocked from a peer)");

/*
 * Clock and transport parity knobs (apollo-linux parity/12, parity/18;
 * titan A/B).  Every one defaults to the behaviour the driver had before
 * it existed; the reads, logs and counters next to them are always on.
 */
static int frame_ctr_gate = 2;
module_param(frame_ctr_gate, int, 0644);
MODULE_PARM_DESC(frame_ctr_gate,
		 "PrepareTransport FRAME_CTR gate: wait up to N ms (1 ms sleeps) for FRAME_CTR == FRAME_CFG as the kext does (default 2 = kext-exact, 3 reads; 20 = bounded); 0 = accept the first non-zero value, the old behaviour, which starts a Hi-band transport at FRAME_CTR 4 of 0x20. A mismatch is logged, not an error");
static bool notif_kext_reads;
module_param(notif_kext_reads, bool, 0644);
MODULE_PARM_DESC(notif_kext_reads,
		 "On notification bits 4/22 read 0xC084, 0xC07C, 0xC080 (and log them) before writing 0 to 0xC030, as the kext's handler does (default: off)");
static bool adopt_fw_rate;
module_param(adopt_fw_rate, bool, 0644);
MODULE_PARM_DESC(adopt_fw_rate,
		 "Adopt the rate the firmware reports (0xC07C bits 15:8) at the first connect after a load or resume, instead of assuming 48 kHz (default: off; the kext does this and never forces the clock at start)");
static bool rate_ack_real;
module_param(rate_ack_real, bool, 0644);
MODULE_PARM_DESC(rate_ack_real,
		 "Send {0x001A0002, rate} to every DSP at connect (the firmware's rate) and after every clock change, as the macOS Mixer Engine does (default: off; the plugin chain's WAKE entry always says 44100)");
static bool clock_ack_bit5;
module_param(clock_ack_bit5, bool, 0644);
MODULE_PARM_DESC(clock_ack_bit5,
		 "Accept only notification bit 5 as the clock-change acknowledgement, as the kext and Windows do (default: off, bits 5, 22 and 4)");
static bool frame_cfg_win;
module_param(frame_cfg_win, bool, 0644);
MODULE_PARM_DESC(frame_cfg_win,
		 "FRAME_CFG per rate band: Windows values 0x0B/0x16/0x2C (on) or the kext's 8/0x10/0x20 (default: off)");
static bool clock_src_from_device;
module_param(clock_src_from_device, bool, 0644);
MODULE_PARM_DESC(clock_src_from_device,
		 "Build the Clock Source enum from the device's own list (mask 0xC08C[31:16], names at 0xC090, source 0xA hidden) instead of the fixed Internal/S-PDIF/ADAT/Word Clock list (default: off)");
static int stall_qos_us;
module_param(stall_qos_us, int, 0644);
MODULE_PARM_DESC(stall_qos_us,
		 "While the transport runs, hold a CPU latency QoS request of this many microseconds (the closest Linux has to the kext's requireMaxBusStall(10 us)); 0 = none (default). Bus read latency is counted either way, see /proc/asound/cardN/clock");

/*
 * Period tracking (kext-vs-driver audit D2, "Period tracking" below).
 * Default: a 1 ms soft hrtimer polls SAMPLE_POS.  period_irq=1: the
 * period-timer interrupt wakes a threaded IRQ handler instead, so BAR0 is
 * read once per period in process context rather than 1000 times a second
 * from a timer.  period_irq is read when the transport starts (whether to
 * program the timer) and when a stream starts (its owner), so a change
 * applies once all streams are closed.
 */
static bool period_irq;
module_param(period_irq, bool, 0644);
MODULE_PARM_DESC(period_irq,
		 "Period wakeups from the hardware period-timer interrupt (threaded IRQ handler) instead of the 1 ms SAMPLE_POS poll (default: off; off leaves the timer unprogrammed and its vector masked, as the macOS driver; a change applies once all streams are closed and the transport restarts)");
static int period_irq_cfg_adj = -1;
module_param(period_irq_cfg_adj, int, 0644);
MODULE_PARM_DESC(period_irq_cfg_adj,
		 "period_irq=1 only: TIMER_CFG = timer interval in frames + this (-1 = the macOS driver's value, default; 0 or 1 to test the register's counting); read at transport start");

/* Forward declarations */
static void ua_pcie_setup(struct ua_device *ua);
static void ua_ctl_notify_info_name(struct ua_device *ua, const char *name);

/* ----------------------------------------------------------------
 * Per-model data
 *
 * Model identity, preamp/HiZ counts and route tables come from the
 * generated ua_models_gen.h (devices/<model>.json, tools/gen-driver-models.py).
 *
 * Legacy layout (io_map=0): ALSA channel N is DMA slot N, and the DMA
 * channel and preamp counts come from ua_legacy_dma[] below for every
 * type the old table knew.  The kext instead sizes
 * the DMA frame from the firmware IO descriptor and maps channels by ID;
 * that is io_map=1, see "IO descriptor and channel map" below.
 * ---------------------------------------------------------------- */

/*
 * io_map=0 DMA channel and preamp counts, frozen per device type NUMBER
 * so the default layout keeps the frame size and the ALSA controls every
 * existing install has today.
 *
 * These are the values the old hand-written ua_models[] gave each type
 * value, even where that table had the wrong model on the type (0x23 was
 * called a Twin X, 0x20 an x8p, ...): a user whose unit resolves to a
 * given type keeps the stride it streams with and every control it has.
 * Only the x4 (22/24, macOS IOAudioEngine) and x8p (32/34, hardware-
 * tested) are known-good.  The one change is the x8p itself: it was
 * pinned to 0x20 by subsystem 0x0014 and resolves to 0x22 now, so 0x22
 * carries its 34/32 and 8 preamps.  Types not listed (the v12 serials
 * 2090-2110 master did not know) fall back to 3/2 and take the
 * generated preamp counts.  io_map=1 uses the firmware descriptor and
 * the generated counts throughout.
 */
struct ua_legacy_dma {
	u32 device_type;
	unsigned int play;
	unsigned int rec;
	unsigned int preamps;
	unsigned int hiz;
};

static const struct ua_legacy_dma ua_legacy_dma[] = {
	/* type                     play rec pre hiz    old ua_models[] row */
	{ UA_DEV_APOLLO_SOLO,         3,  2,  1, 0 },  /* 0x27 Solo */
	{ UA_DEV_ARROW,               3,  2,  1, 0 },  /* 0x28 Arrow */
	{ UA_DEV_APOLLO_X16,          8,  8,  2, 2 },  /* 0x23 "Twin X" */
	{ UA_DEV_APOLLO_X16_GEN2,     8,  8,  2, 2 },  /* 0x3A "Twin X Gen 2" */
	{ UA_DEV_APOLLO_X4,          24, 22,  4, 2 },  /* 0x1F x4 */
	{ UA_DEV_APOLLO_X4_GEN2,     24, 22,  4, 2 },  /* 0x36 x4 Gen 2 */
	{ UA_DEV_SERIAL_2016,        24, 22,  4, 2 },  /* 0x1E "x6" */
	{ UA_DEV_APOLLO_X16D,        24, 22,  4, 2 },  /* 0x35 "x6 Gen 2" */
	{ UA_DEV_APOLLO_X8P,         34, 32,  8, 2 },  /* 0x22 x8p (was the 0x20 pin) */
	{ UA_DEV_APOLLO_X6_GEN2,     26, 26,  4, 2 },  /* 0x37 "x8 Gen 2" */
	{ UA_DEV_APOLLO_8P,          26, 26,  8, 2 },  /* 0x0A 8p */
	{ UA_DEV_APOLLO_X6,          34, 32,  8, 2 },  /* 0x20 "x8p" */
	{ UA_DEV_APOLLO_X8_GEN2,     26, 26,  8, 2 },  /* 0x38 "x8p Gen 2" */
	{ UA_DEV_APOLLO_X8,          34, 34,  8, 2 },  /* 0x21 "x16" */
	{ UA_DEV_APOLLO_X8P_GEN2,    34, 34,  8, 2 },  /* 0x39 "x16 Gen 2" */
	{ UA_DEV_SERIAL_2032,        34, 34,  0, 0 },  /* 0x2A "x16D" */
};

static const struct ua_legacy_dma *ua_legacy_row(u32 device_type)
{
	unsigned int i;

	for (i = 0; i < ARRAY_SIZE(ua_legacy_dma); i++)
		if (ua_legacy_dma[i].device_type == device_type)
			return &ua_legacy_dma[i];
	return NULL;
}

const struct ua_model_desc *ua_model_lookup(u32 device_type,
					    const char **type_name)
{
	unsigned int i;

	for (i = 0; i < ARRAY_SIZE(ua_gen_model_types); i++) {
		const struct ua_model_type *t = &ua_gen_model_types[i];

		if (t->device_type != device_type)
			continue;
		if (type_name)
			*type_name = t->name ? t->name : t->model->name;
		return t->model;
	}

	if (type_name)
		*type_name = NULL;
	return NULL;
}

/*
 * io_map=-1 (the default) picks the firmware descriptor layout on the
 * models it has been verified on with hardware, the legacy layout on the
 * rest: only the Apollo x4 so far (titan, 2026-10-09/10: every route
 * channel on its labelled ALSA channel at all six rates, where the legacy
 * layout mislabels everything past MIC 4).  io_map=0/1 force one layout
 * on every model.
 */
bool ua_io_map_enabled(const struct ua_device *ua)
{
	if (io_map >= 0)
		return io_map == 1;
	return ua->device_type == UA_DEV_APOLLO_X4;
}

/*
 * Legacy DMA channel counts: module overrides, else ua_legacy_dma[], else
 * the stereo fallback.  rec_ch is bounded like play_ch (the channel map
 * holds at most UA_MAX_CHANNELS).
 */
static void ua_legacy_counts(struct ua_device *ua, unsigned int *play,
			     unsigned int *rec)
{
	const struct ua_legacy_dma *row;

	if (play_ch > 0 && play_ch <= UA_MAX_CHANNELS) {
		*play = play_ch;
		*rec = (rec_ch > 0 && rec_ch <= UA_MAX_CHANNELS) ?
			rec_ch : play_ch;
		return;
	}

	row = ua_legacy_row(ua->device_type);
	if (row) {
		*play = row->play;
		*rec = row->rec;
		return;
	}

	/* Fallback: safe stereo default */
	*play = UA_DEFAULT_PLAY_CH + 1;  /* +1 for sync */
	*rec = UA_DEFAULT_REC_CH;
}

/*
 * Legacy map: ALSA channel N is DMA slot N.  When the legacy count equals
 * the model's route count (x4, x8p) the channels are labelled with the
 * route-table IDs that layout is meant to carry (labels only; whether the
 * firmware puts them there is what io_map=1 settles).  Otherwise the
 * legacy count is not the model's and the channels stay unlabelled.
 */
static void ua_chan_map_legacy(const struct ua_model_desc *m,
			       unsigned int dir, unsigned int count,
			       struct ua_chan_map *map)
{
	const struct ua_route_list *r = NULL;
	unsigned int i;

	if (m)
		r = &m->routes[UA_RATE_BAND_LOMID][m->legacy_mode][dir];
	if (r && r->count != count)
		r = NULL;

	memset(map, 0, sizeof(*map));
	map->channels = min_t(unsigned int, count, UA_MAX_CHANNELS);
	for (i = 0; i < map->channels; i++) {
		map->slot[i] = i;
		map->ch_id[i] = (r && i < r->count) ? r->ids[i] : 0;
	}
}

/* ----------------------------------------------------------------
 * Buffer frame size calculation
 *
 * From kext CPcieAudioExtension::PrepareTransport():
 *   maxCh = max(playCh, recCh)
 *   frameSize = roundDownPow2(0x400000 / (maxCh * 4) / 2)
 *   if (frameSize > 8192) frameSize = 8192
 * ---------------------------------------------------------------- */

static unsigned int ua_calc_buf_frame_size(unsigned int play_ch,
					   unsigned int rec_ch)
{
	unsigned int max_ch = max(play_ch, rec_ch);
	unsigned int frames;

	if (max_ch == 0)
		max_ch = 1;

	frames = UA_DMA_BUF_SIZE / (max_ch * UA_SAMPLE_BYTES) / 2;

	/* Round down to power of 2 */
	frames = rounddown_pow_of_two(frames);

	if (frames > UA_MAX_BUF_FRAMES)
		frames = UA_MAX_BUF_FRAMES;

	return frames;
}

/*
 * The DMA frame geometry: hardware channel counts and the ring size in
 * frames derived from them.  The only place play/rec_channels and
 * buf_frame_size are set: once at probe (ua_get_model_channels(), before
 * anything reads them) and in ua_io_map_commit() under dma_sem.
 */
static void ua_audio_set_hw_layout(struct ua_audio *audio, unsigned int play,
				   unsigned int rec)
{
	audio->play_channels = play;
	audio->rec_channels = rec;
	audio->buf_frame_size = ua_calc_buf_frame_size(play, rec);
}

static void ua_get_model_channels(struct ua_device *ua)
{
	struct ua_audio *audio = &ua->audio;
	const struct ua_model_desc *m;
	const struct ua_legacy_dma *row;
	unsigned int play, rec;

	m = ua_model_lookup(ua->device_type, NULL);
	audio->model = m;

	/*
	 * Preamp/HiZ counts decide which ALSA controls exist.  The legacy
	 * layout never drops a control a type has today, so types the old
	 * table knew keep its counts; io_map=1 uses the generated ones.
	 */
	row = ua_legacy_row(ua->device_type);
	if (!ua_io_map_enabled(ua) && row) {
		audio->num_preamps = row->preamps;
		audio->num_hiz = row->hiz;
	} else {
		audio->num_preamps = m ? m->preamps : 0;
		audio->num_hiz = m ? m->hiz : 0;
	}
	audio->num_preamps = min_t(unsigned int, audio->num_preamps,
				   UA_MAX_PREAMP_CH);

	/* Module param overrides keep their historical no-preamp behaviour */
	if (play_ch > 0 && play_ch <= UA_MAX_CHANNELS) {
		audio->num_preamps = 0;
		audio->num_hiz = 0;
	}

	/*
	 * The legacy layout.  Runs once at probe (callers check
	 * play_channels), before the ALSA card and the ioctls exist, so
	 * nothing reads it concurrently; from then on only
	 * ua_io_map_commit() changes it (io_map=1).
	 */
	ua_legacy_counts(ua, &play, &rec);
	ua_audio_set_hw_layout(audio, play, rec);
	ua_chan_map_legacy(m, UA_DIR_REC, rec, &audio->chan_map[UA_DIR_REC]);
	ua_chan_map_legacy(m, UA_DIR_PLAY, play, &audio->chan_map[UA_DIR_PLAY]);
	audio->io_map_active = false;
}

/* ----------------------------------------------------------------
 * Audio DMA rings
 *
 * Each 4 MiB ring is UA_DMA_SG_ENTRIES separate pages from
 * dma_alloc_pages() rather than one contiguous dma_alloc_coherent()
 * block.  A single 4 MiB coherent allocation needs a physically
 * contiguous order-10 buddy block below 4 GiB (DMA_BIT_MASK(32)); on
 * a dma-direct (no IOMMU) host with CmaTotal=0, low memory fragments
 * within hours of uptime and probe fails with -ENOMEM on every replug
 * (2026-08-29: three consecutive -12 failures after the first
 * boot-time connect).  The FPGA walks a 1024-entry SG table, so the
 * device never needed contiguity.
 *
 * dma_alloc_pages() rather than per-page dma_alloc_coherent(): it
 * returns the struct page, which is what vmap() needs to build the
 * flat CPU view.  dma_alloc_coherent()'s CPU pointer is not a
 * linear-map address on every backend — behind an IOMMU it is a
 * vmalloc remap, and virt_to_page() on it is garbage.
 *
 * dma_alloc_pages() memory is non-coherent by contract: a range the
 * CPU wrote is handed to the device with dma_sync_single_for_device()
 * and taken back before the CPU reads it with
 * dma_sync_single_for_cpu().  On cache-coherent x86 both are no-ops.
 * Both rings are DMA_BIDIRECTIONAL because the record ring is also
 * CPU-written (the dma_test canary).
 * ---------------------------------------------------------------- */

#define UA_DMA_RING_DIR		DMA_BIDIRECTIONAL

static void ua_dma_ring_free(struct device *dev, struct ua_dma_ring *ring)
{
	unsigned int i;

	for (i = 0; i < ring->nr_pages; i++)
		dma_free_pages(dev, PAGE_SIZE, ring->pages[i], ring->dmas[i],
			       UA_DMA_RING_DIR);

	kvfree(ring->pages);
	kvfree(ring->dmas);
	ring->pages = NULL;
	ring->dmas = NULL;
	ring->nr_pages = 0;
}

/*
 * Allocate one ring and map it flat into *vaddr.  On failure everything
 * allocated so far is released and *vaddr is left untouched.
 */
static int ua_dma_ring_alloc(struct device *dev, struct ua_dma_ring *ring,
			     void **vaddr)
{
	unsigned int i;
	void *va;

	/* The flat view is one kernel page per SG entry */
	BUILD_BUG_ON(PAGE_SIZE != UA_PCIE_PAGE_SIZE);
	BUILD_BUG_ON(UA_DMA_SG_ENTRIES * UA_PCIE_PAGE_SIZE != UA_DMA_BUF_SIZE);

	ring->pages = kvcalloc(UA_DMA_SG_ENTRIES, sizeof(*ring->pages),
			       GFP_KERNEL);
	ring->dmas = kvcalloc(UA_DMA_SG_ENTRIES, sizeof(*ring->dmas),
			      GFP_KERNEL);
	if (!ring->pages || !ring->dmas)
		goto err;

	for (i = 0; i < UA_DMA_SG_ENTRIES; i++) {
		ring->pages[i] = dma_alloc_pages(dev, PAGE_SIZE, &ring->dmas[i],
						 UA_DMA_RING_DIR, GFP_KERNEL);
		if (!ring->pages[i])
			goto err;
		ring->nr_pages = i + 1;
	}

	/* dma_alloc_pages() zero-fills, so no memset through the vmap */
	va = vmap(ring->pages, UA_DMA_SG_ENTRIES, VM_MAP, PAGE_KERNEL);
	if (!va)
		goto err;

	*vaddr = va;
	return 0;

err:
	ua_dma_ring_free(dev, ring);
	return -ENOMEM;
}

/*
 * Transfer ownership of [off, off + len) of a ring: to the device after
 * the CPU wrote it, or back to the CPU before the CPU reads it.
 */
static void ua_dma_ring_sync(struct device *dev, struct ua_dma_ring *ring,
			     unsigned long off, unsigned long len,
			     bool to_device)
{
	unsigned long first, last;

	if (!len || !ring->nr_pages)
		return;

	first = off / PAGE_SIZE;
	last = (off + len - 1) / PAGE_SIZE;
	if (last >= ring->nr_pages)
		last = ring->nr_pages - 1;

	for (; first <= last; first++) {
		if (to_device)
			dma_sync_single_for_device(dev, ring->dmas[first],
						   PAGE_SIZE, UA_DMA_RING_DIR);
		else
			dma_sync_single_for_cpu(dev, ring->dmas[first],
						PAGE_SIZE, UA_DMA_RING_DIR);
	}
}

/*
 * Same, for a run of hardware frames that may wrap at the end of the
 * ring exactly like the PCM copy path does.
 */
static void ua_dma_ring_sync_frames(struct device *dev,
				    struct ua_dma_ring *ring,
				    unsigned long start_frame,
				    unsigned long frames,
				    unsigned int hw_frame_sz,
				    unsigned int buf_frames,
				    bool to_device)
{
	unsigned long ring_bytes = (unsigned long)buf_frames * hw_frame_sz;
	unsigned long off, len;

	if (!frames || !buf_frames)
		return;
	if (frames > buf_frames)
		frames = buf_frames;

	off = (start_frame % buf_frames) * (unsigned long)hw_frame_sz;
	len = frames * (unsigned long)hw_frame_sz;

	if (off + len <= ring_bytes) {
		ua_dma_ring_sync(dev, ring, off, len, to_device);
	} else {
		ua_dma_ring_sync(dev, ring, off, ring_bytes - off, to_device);
		ua_dma_ring_sync(dev, ring, 0, off + len - ring_bytes,
				 to_device);
	}
}

/* Idempotent; also the probe-failure path's (ua_probe_unwind_dsp()) */
void ua_audio_free_dma(struct ua_device *ua)
{
	struct ua_audio *audio = &ua->audio;
	struct device *dev = &ua->pdev->dev;

	/* vunmap first — the flat views alias the ring pages */
	if (audio->play_buf) {
		vunmap(audio->play_buf);
		audio->play_buf = NULL;
	}
	if (audio->rec_buf) {
		vunmap(audio->rec_buf);
		audio->rec_buf = NULL;
	}

	ua_dma_ring_free(dev, &audio->play_ring);
	ua_dma_ring_free(dev, &audio->rec_ring);
}

/* Span of a ring's bus addresses and how many pages sit above 4 GiB */
static void ua_dma_ring_log64(struct device *dev, const char *name,
			      const struct ua_dma_ring *ring)
{
	dma_addr_t lo = ~(dma_addr_t)0, hi = 0;
	phys_addr_t first = page_to_phys(ring->pages[0]);
	unsigned int i, above_bus = 0, above_phys = 0;

	for (i = 0; i < ring->nr_pages; i++) {
		if (ring->dmas[i] < lo)
			lo = ring->dmas[i];
		if (ring->dmas[i] > hi)
			hi = ring->dmas[i];
		if (upper_32_bits(ring->dmas[i]))
			above_bus++;
		if (upper_32_bits(page_to_phys(ring->pages[i])))
			above_phys++;
	}
	dev_info(dev,
		 "dma64_audio: %s ring bus %pad..%pad (page 0 phys %pa), %u/%u pages above 4 GiB (bus), %u above (phys)%s\n",
		 name, &lo, &hi, &first, above_bus, ring->nr_pages,
		 above_phys,
		 above_bus ? "" : " - buffer stayed below 4 GiB");
}

static int ua_audio_alloc_dma(struct ua_device *ua)
{
	struct ua_audio *audio = &ua->audio;
	struct device *dev = &ua->pdev->dev;
	bool wide = false;
	int ret;

	/*
	 * dma64_audio: raise the mask only around the two ring allocations
	 * and drop it to 32 bits again straight after, so the DSP rings,
	 * command buffers and every later dma_alloc_coherent() stay below
	 * 4 GiB.  Runs from probe/resume (connect path), before any ioctl
	 * can allocate; the already mapped ring pages keep their addresses
	 * (sync and free do not look at the mask).
	 */
	if (dma64_audio) {
		ret = dma_set_mask_and_coherent(dev, DMA_BIT_MASK(64));
		if (ret)
			dev_warn(dev, "dma64_audio: 64-bit mask refused (%d), rings stay below 4 GiB\n",
				 ret);
		else
			wide = true;
	}

	ret = ua_dma_ring_alloc(dev, &audio->play_ring, &audio->play_buf);
	if (!ret) {
		ret = ua_dma_ring_alloc(dev, &audio->rec_ring, &audio->rec_buf);
		if (ret)
			ua_audio_free_dma(ua);
	}

	if (wide) {
		int mret = dma_set_mask_and_coherent(dev, DMA_BIT_MASK(32));

		if (mret) {
			dev_err(dev, "dma64_audio: cannot restore the 32-bit DMA mask (%d)\n",
				mret);
			if (!ret) {
				ua_audio_free_dma(ua);
				ret = mret;
			}
		}
	}
	if (ret)
		return ret;

	if (dma64_audio) {
		ua_dma_ring_log64(dev, "play", &audio->play_ring);
		ua_dma_ring_log64(dev, "rec", &audio->rec_ring);
	}

	dev_info(dev, "DMA buffers: %u pages per direction, vmapped flat (4 MiB each)\n",
		 UA_DMA_SG_ENTRIES);
	return 0;
}

/*
 * Program the scatter-gather table in hardware.
 *
 * The SG SRAM at 0x8000 appears to be 8 KiB (0x8000-0x9FFF) for playback
 * and 0xA000-0xBFFF for record, supporting up to 1024 entries at 8 bytes
 * each.  1024 entries × 4K stride = 4 MiB, covering the full DMA buffer.
 *
 * The region is write-only (reads return 0x00000000).
 *
 *   Playback: 0x8000 + i*8 (i=0..1023)
 *   Record:   0xA000 + i*8 (i=0..1023)
 */
static void ua_audio_program_sg(struct ua_device *ua)
{
	struct ua_audio *audio = &ua->audio;
	const dma_addr_t *play = audio->play_ring.dmas;
	const dma_addr_t *rec = audio->rec_ring.dmas;
	unsigned int i;
	u32 ax;

	dev_dbg(&ua->pdev->dev, "=== PROGRAM SG TABLE (%u entries, stride=0x%x) ===\n",
		UA_DMA_SG_ENTRIES, UA_PCIE_PAGE_SIZE);

	/*
	 * Never rewrite the table under a running transport.  Kext
	 * CPcieAudioExtension::ProgramRegisters (12.0.0 x86 0x33f4a)
	 * writes AX_CONTROL = 0 and reads it back as a flush immediately
	 * before the 1024 SG pairs.  A no-op on the normal probe paths,
	 * which have already stopped the transport.
	 */
	ax = ua_read(ua, UA_REG_AX_CONTROL);
	if (ax && ax != 0xFFFFFFFF)
		dev_info(&ua->pdev->dev,
			 "  transport still armed (AX_CONTROL=0x%x), stopping before SG rewrite\n",
			 ax);
	ua_write(ua, UA_REG_AX_CONTROL, 0);
	(void)ua_read(ua, UA_REG_AX_CONTROL);
	audio->transport_running = false;

	for (i = 0; i < UA_DMA_SG_ENTRIES; i++) {
		ua_write(ua, UA_REG_PLAY_DMA_BASE + i * 8,
			 lower_32_bits(play[i]));
		ua_write(ua, UA_REG_PLAY_DMA_BASE + i * 8 + 4,
			 upper_32_bits(play[i]));

		ua_write(ua, UA_REG_REC_DMA_BASE + i * 8,
			 lower_32_bits(rec[i]));
		ua_write(ua, UA_REG_REC_DMA_BASE + i * 8 + 4,
			 upper_32_bits(rec[i]));
	}

	/* No doorbell — kext ProgramRegisters writes SG directly without one */

	/*
	 * Enable per-channel DMA engines.
	 *
	 * ResetDMAEngines (ua_reset_dma_engines()) clears DMA_CTRL bits [8:1]
	 * (per-engine enables) and only keeps bit 0 (global enable).  We must
	 * re-enable the engines after programming the SG table, before
	 * transport start.
	 */
	{
		u32 dma_ctrl = ua_read(ua, UA_REG_DMA_CTRL);

		/*
		 * Enable global DMA + per-engine enables, clear reset
		 * strobes.  On warm boot the engine reset is skipped so
		 * bit 0 (global enable) may not be set.
		 */
		if (ua->fw_v2)
			dma_ctrl = (dma_ctrl | 0x1FF) & ~0x1FE00;
		else
			dma_ctrl = (dma_ctrl | 0x1F) & ~0x1E00;

		ua_write(ua, UA_REG_DMA_CTRL, dma_ctrl);
		dma_ctrl = ua_read(ua, UA_REG_DMA_CTRL);
		dev_dbg(&ua->pdev->dev, "  DMA_CTRL after enable: 0x%08x\n",
			dma_ctrl);
	}

	/* Kext ProgramRegisters: EnableVector(0x28, clearPending=1) */
	ua_enable_vector(ua, UA_IRQ_VEC_NOTIFICATION, true);

	dev_dbg(&ua->pdev->dev, "  SG[0]: play=0x%08x rec=0x%08x\n",
		lower_32_bits(play[0]), lower_32_bits(rec[0]));
	dev_dbg(&ua->pdev->dev, "  SG[%u]: play=0x%08x rec=0x%08x\n",
		UA_DMA_SG_ENTRIES - 1,
		lower_32_bits(play[UA_DMA_SG_ENTRIES - 1]),
		lower_32_bits(rec[UA_DMA_SG_ENTRIES - 1]));
}

/*
 * ua_audio_preinit_dma - allocate PCM DMA buffers and program SG table.
 *
 * Must be called BEFORE ua_boot_transport_start().  When transport starts,
 * the FPGA immediately begins audio DMA using the SG table stored in BAR0
 * SRAM (0x8000 play / 0xA000 rec).  If those entries are zero (cold boot
 * default), the FPGA issues DMA reads to physical address 0x0.  On systems
 * with IOMMU enforced (Fedora 43 / Intel VT-d), this causes an IOMMU fault
 * → PCIe Uncorrectable Fatal error → kernel panic or hard lockup.
 *
 * Pre-allocating DMA buffers and programming the SG table ensures every SG
 * entry points to a valid IOMMU-mapped page before the first transport start.
 *
 * Safe to call multiple times — skips allocation if already done.
 */
int ua_audio_preinit_dma(struct ua_device *ua)
{
	struct ua_audio *audio = &ua->audio;
	int ret;

	/* Channel counts may already be set (ua_audio_init calls this too) */
	if (!audio->play_channels)
		ua_get_model_channels(ua);

	/* Skip if DMA buffers already allocated */
	if (audio->play_buf)
		return 0;

	ret = ua_audio_alloc_dma(ua);
	if (ret)
		return ret;

	ua_audio_program_sg(ua);
	return 0;
}

/*
 * ua_audio_reprogram_dma - point the FPGA back at the existing audio rings.
 *
 * Resume path: the SG SRAM may have lost its contents (or still hold the
 * previous session's entries) and ua_audio_preinit_dma() deliberately does
 * nothing once the rings exist.  Allocates them first if an earlier preinit
 * failed.
 */
int ua_audio_reprogram_dma(struct ua_device *ua)
{
	if (!ua->audio.play_buf)
		return ua_audio_preinit_dma(ua);

	ua_audio_program_sg(ua);
	return 0;
}

/* ----------------------------------------------------------------
 * IO descriptor and channel map
 *
 * The firmware publishes one IO descriptor per direction at 0xC1A4
 * (input) / 0xC2C4 (output): a slot count and the channel ID carried by
 * each slot of the DMA frame (layout: UA_IO_DESC_* in ua_apollo.h).  The
 * kext reads both after connect and on notification bits 0/1, never
 * writes them, programs AX_PLAY_CH/AX_REC_CH from the counts and sizes
 * the DMA frame from them.  CoreAudio channel j then reads or writes the
 * slot FindIndexForChannelID() returns for the j-th route-table channel
 * ID (first match in slot order; a channel with no slot reads silence).
 *
 * Here the descriptor is read and logged in both modes.  With io_map=1
 * the driver also does what the kext does: AX_*_CH and the frame stride
 * follow the descriptor, ALSA channels follow the model's route table
 * for the current rate band and TOSLink mode(s), and the PCM copy path
 * moves each ALSA channel to and from its slot.  A route channel that is
 * missing from the descriptor (or hidden at this rate) keeps its ALSA
 * channel without a slot, so the channel count does not change with the
 * rate; a model without a route table exposes every descriptor slot.
 *
 * A read that is not usable (all-ones on a dropped link, a header caught
 * mid-republish, an area not repopulated yet after a reconnect) never
 * replaces the last valid descriptor; the legacy layout is used only
 * until a valid one has been seen.
 *
 * Who changes the layout: ua_io_map_update_into(), with ua->lock and
 * io_lock held (connect, notification, TOSLink mode or rate change,
 * pcm_open, prepare).  With the transport stopped it commits the new
 * layout at once; ua->lock keeps that from landing between the AX_*_CH
 * writes and the transport start in prepare.  With the transport running
 * it marks the layout stale and pushes the open streams into XRUN
 * (ua_io_map_force_reprepare()), so userspace prepares again and
 * ua_pcm_prepare() restarts the transport on the new layout.
 *
 * Per-slot names: dynamic debug ("module ua_apollo +p"), and the whole
 * map: /proc/asound/cardN/io_map and UA_IOCTL_GET_IO_MAP.
 * ---------------------------------------------------------------- */

#define UA_IO_DESC_SETTLE_MS    100     /* wait for a post-clock republish */
#define UA_IO_DESC_POLL_MS      10

/* devNames table (kext _parseIONames), indexed by channel ID >> 8 */
static const char * const ua_chan_type_names[] = {
	"MIC/LINE/HIZ", "MIC/LINE", "AUX", "LINE", "ADAT", "S/PDIF",
	"AES/EBU", "VIRTUAL", "MADI", "MON", "CUE", "TALKBACK", "RESERVED",
	"DANTE", "UNKNOWN",
};

#define UA_CHAN_TYPE_ADAT       0x04
#define UA_CHAN_TYPE_UNKNOWN    0x0e
#define UA_CHAN_STEREO_TYPES    0x0664  /* AUX, S/PDIF, AES/EBU, MON, CUE */
#define UA_CHAN_NUMBERED_TYPES  0x7d9f  /* all but S/PDIF, AES/EBU, MON */

/**
 * ua_chan_name - Name a UA channel ID the way the kext does
 * @ch_id: type << 8 | number (1-based)
 * @buf: output buffer
 * @len: size of @buf
 *
 * Follows _parseIONames: IDs above 0x0dff are UNKNOWN, stereo-pair types
 * (mask 0x664) are named pair (n + 1) / 2 plus L (odd n) or R, numbered
 * types (mask 0x7d9f) carry the number, and AUX/CUE run the pair number
 * into the type ("AUX1 L").  The kext writes two spaces before L/R for
 * S/PDIF, AES/EBU and MON; this uses one, the route-table spelling.
 *
 * Returns the length written.
 */
int ua_chan_name(u16 ch_id, char *buf, size_t len)
{
	unsigned int type = ch_id >> 8;
	unsigned int num = ch_id & 0xff;
	const char *lr = (num & 1) ? "L" : "R";
	bool stereo, numbered;

	if (ch_id > 0x0dff)
		type = UA_CHAN_TYPE_UNKNOWN;
	stereo = UA_CHAN_STEREO_TYPES & BIT(type);
	numbered = UA_CHAN_NUMBERED_TYPES & BIT(type);
	if (stereo)
		num = (num + 1) / 2;

	if (numbered && stereo)
		return scnprintf(buf, len, "%s%u %s",
				 ua_chan_type_names[type], num, lr);
	if (numbered)
		return scnprintf(buf, len, "%s %u",
				 ua_chan_type_names[type], num);
	if (stereo)
		return scnprintf(buf, len, "%s %s",
				 ua_chan_type_names[type], lr);
	return scnprintf(buf, len, "%s", ua_chan_type_names[type]);
}

/* Label of ALSA channel @i of @map: its channel name, else "CH n" */
static void ua_chan_map_label(const struct ua_chan_map *map, unsigned int i,
			      char *buf, size_t len)
{
	if (map->ch_id[i])
		ua_chan_name(map->ch_id[i], buf, len);
	else
		scnprintf(buf, len, "CH %u", i + 1);
}

static const char *ua_dir_name(unsigned int dir)
{
	return dir == UA_DIR_REC ? "record" : "playback";
}

static const char *ua_dig_name(unsigned int mode)
{
	return mode == UA_DIG_ADAT ? "ADAT" : "S/PDIF";
}

/* Slot count of @dir from a header, or 0 when the header is not usable */
static unsigned int ua_io_desc_hdr_count(const u32 *hdr, unsigned int dir)
{
	unsigned int count = hdr[dir == UA_DIR_REC ? UA_IO_DESC_IN_COUNT :
						     UA_IO_DESC_OUT_COUNT];

	if (hdr[0] != UA_IO_DESC_VERSION || hdr[1] != UA_IO_DESC_LENGTH)
		return 0;
	if (!count || count > UA_MAX_CHANNELS)
		return 0;
	return count;
}

/*
 * Read one direction's descriptor: the six header words first, then only
 * the DIV_ROUND_UP(count, 2) ID words the count calls for (the rest of
 * the 72-word area is padding), then the version, length and count words
 * once more.  @d->valid stays false when the header is not usable or the
 * second look differs from the first (the link dropped mid-read and reads
 * all-ones, or the firmware republished meanwhile); @d->hdr always holds
 * the first header read.
 */
static void ua_io_desc_fetch(struct ua_device *ua, unsigned int dir,
			     struct ua_io_desc *d)
{
	u32 base = (dir == UA_DIR_REC) ? UA_REG_IN_NAMES_BASE :
					 UA_REG_OUT_NAMES_BASE;
	unsigned int cw = dir == UA_DIR_REC ? UA_IO_DESC_IN_COUNT :
					      UA_IO_DESC_OUT_COUNT;
	unsigned int count, i;

	BUILD_BUG_ON(UA_IO_DESC_HDR_WORDS + DIV_ROUND_UP(UA_MAX_CHANNELS, 2) >
		     UA_IO_DESC_WORDS);
	BUILD_BUG_ON(UA_IO_MAP_HDR_WORDS != UA_IO_DESC_HDR_WORDS);

	memset(d, 0, sizeof(*d));
	for (i = 0; i < UA_IO_DESC_HDR_WORDS; i++)
		d->hdr[i] = ua_read(ua, base + i * 4);

	count = ua_io_desc_hdr_count(d->hdr, dir);
	if (!count)
		return;

	for (i = 0; i < DIV_ROUND_UP(count, 2); i++) {
		u32 word = ua_read(ua, base + (UA_IO_DESC_HDR_WORDS + i) * 4);

		d->ids[2 * i] = word & 0xffff;
		if (2 * i + 1 < count)
			d->ids[2 * i + 1] = word >> 16;
	}

	if (ua_read(ua, base) != d->hdr[0] ||
	    ua_read(ua, base + 4) != d->hdr[1] ||
	    ua_read(ua, base + cw * 4) != d->hdr[cw])
		return;

	d->count = count;
	d->valid = true;
}

static bool ua_io_desc_same(const struct ua_io_desc *a,
			    const struct ua_io_desc *b)
{
	if (a->valid != b->valid)
		return false;
	if (!a->valid)
		return true;
	return a->count == b->count &&
	       !memcmp(a->ids, b->ids, a->count * sizeof(a->ids[0]));
}

/*
 * Take one read into audio->io_desc.  A usable read replaces the stored
 * descriptor; one that is not usable only records its header (the last
 * valid descriptor stays).  Returns true when the stored slots changed.
 * Caller holds io_lock.
 */
static bool ua_io_desc_store(struct ua_audio *audio, unsigned int dir,
			     const struct ua_io_desc *d)
{
	bool changed;

	if (!d->valid) {
		audio->io_desc_bad[dir] = true;
		memcpy(audio->io_desc_bad_hdr[dir], d->hdr,
		       sizeof(audio->io_desc_bad_hdr[dir]));
		return false;
	}

	changed = !ua_io_desc_same(d, &audio->io_desc[dir]);
	audio->io_desc[dir] = *d;
	audio->io_desc_bad[dir] = false;
	return changed;
}

/* One-line summary per direction (dev_info); slot IDs and names (dev_dbg) */
static void ua_io_desc_log(struct ua_device *ua, unsigned int dir,
			   const char *why)
{
	struct ua_audio *audio = &ua->audio;
	const struct ua_io_desc *d = &audio->io_desc[dir];
	struct device *dev = &ua->pdev->dev;
	char ids[UA_MAX_CHANNELS * 5 + 1];
	unsigned int i, pos = 0;

	if (audio->io_desc_bad[dir]) {
		const u32 *h = audio->io_desc_bad_hdr[dir];

		if (d->valid)
			dev_info_ratelimited(dev,
					     "IO desc %s (%s): read not usable (header %08x %08x %08x %08x %08x %08x), keeping the last valid %u-slot descriptor\n",
					     ua_dir_name(dir), why, h[0], h[1],
					     h[2], h[3], h[4], h[5], d->count);
		else
			dev_info_ratelimited(dev,
					     "IO desc %s (%s): not usable, header %08x %08x %08x %08x %08x %08x\n",
					     ua_dir_name(dir), why, h[0], h[1],
					     h[2], h[3], h[4], h[5]);
		return;
	}
	if (!d->valid)
		return;		/* never read */

	ids[0] = '\0';
	for (i = 0; i < d->count; i++)
		pos += scnprintf(ids + pos, sizeof(ids) - pos, " %04x",
				 d->ids[i]);
	dev_info(dev, "IO desc %s (%s): %u slots, word5 0x%x\n",
		 ua_dir_name(dir), why, d->count, d->hdr[UA_IO_DESC_FLAGS]);
	dev_dbg(dev, "IO desc %s slot IDs:%s\n", ua_dir_name(dir), ids);

	for (i = 0; i < d->count; i++) {
		char name[UA_IO_MAP_NAME_LEN];

		ua_chan_name(d->ids[i], name, sizeof(name));
		dev_dbg(dev, "  %s slot %2u: 0x%04x %s\n",
			ua_dir_name(dir), i, d->ids[i], name);
	}
}

/*
 * Read both descriptors (ua_io_desc_store() semantics).  Logs a direction
 * when @always_log is set, when it changed, or when a read turned bad.
 * Returns true if either stored descriptor changed.  Caller holds io_lock.
 */
static bool ua_io_desc_read_locked(struct ua_device *ua, const char *why,
				   bool always_log)
{
	struct ua_audio *audio = &ua->audio;
	bool changed = false;
	unsigned int dir;

	lockdep_assert_held(&audio->io_lock);

	if (atomic_read(&ua->shutdown))
		return false;

	for (dir = 0; dir < UA_DIRS; dir++) {
		struct ua_io_desc d;
		bool was_bad = audio->io_desc_bad[dir];
		bool diff;

		ua_io_desc_fetch(ua, dir, &d);
		diff = ua_io_desc_store(audio, dir, &d);
		changed |= diff;
		if (always_log || diff || (audio->io_desc_bad[dir] && !was_bad))
			ua_io_desc_log(ua, dir, why);
	}
	return changed;
}

/* kext FindIndexForChannelID: first slot carrying @ch_id, or -1 */
static int ua_io_desc_find(const struct ua_io_desc *d, u16 ch_id)
{
	unsigned int i;

	for (i = 0; i < d->count; i++)
		if (d->ids[i] == ch_id)
			return i;
	return -1;
}

/* kext GetCurrentRouteTableIndex: Hi table when (rate >> 7) >= 0x55f */
static unsigned int ua_rate_band(unsigned int rate)
{
	return (rate >> 7) >= 0x55f ? UA_RATE_BAND_HI : UA_RATE_BAND_LOMID;
}

/*
 * TOSLink mode that picks @dir's route list (model->toslink, from the
 * class's IsChannelShownForIORoute()).
 *
 * The output type is the "Digital Output Mode" the driver writes to the
 * device (param 0x21, 0 = S/PDIF, 8 = ADAT).  The input type is host-side
 * only: the Mixer Engine keeps it in GlobalSettings
 * (CMxMixer::SetTOSLINKInputType stores it and refreshes the routes; no
 * device parameter for it is known), and the firmware descriptor lists
 * the S/PDIF and the ADAT input slots in either mode.  So dig_in_mode
 * ("Digital Input Mode", default the class's DefaultTOSLinkInputType)
 * only selects which input channels are exposed.
 */
static unsigned int ua_route_mode(const struct ua_audio *audio,
				  unsigned int dir)
{
	unsigned int out = audio->monitor.digital_mode == 8 ? UA_DIG_ADAT :
							      UA_DIG_SPDIF;

	if (!audio->model)
		return out;

	switch (audio->model->toslink) {
	case UA_TOSLINK_PER_DIR:
		return dir == UA_DIR_REC ? audio->dig_in_mode : out;
	case UA_TOSLINK_INPUT:
		return audio->dig_in_mode;
	default:
		return out;	/* both lists are identical */
	}
}

/*
 * MixerTypeFreud/Stretch/Armstrong::IsChannelShownForIORoute hide ADAT
 * channels from 5 up while 50001 <= rate <= 150000 (88.2/96 kHz S/MUX).
 */
static bool ua_route_hidden_at_rate(const struct ua_model_desc *m, u16 id,
				    unsigned int rate)
{
	return m->adat_mid_rate_max &&
	       (id >> 8) == UA_CHAN_TYPE_ADAT &&
	       rate - 50001 < 99999 &&
	       (id & 0xff) > m->adat_mid_rate_max;
}

/*
 * Build one direction's ALSA map from the route table and the firmware
 * descriptor.  @note receives the IDs that get no slot because the
 * descriptor lacks them or the rate hides them (empty if none).
 */
static void ua_chan_map_build(struct ua_device *ua, unsigned int dir,
			      unsigned int rate, unsigned int mode,
			      struct ua_chan_map *map, char *note,
			      size_t note_len)
{
	const struct ua_io_desc *d = &ua->audio.io_desc[dir];
	const struct ua_model_desc *m = ua->audio.model;
	const struct ua_route_list *r = NULL;
	unsigned int i, pos = 0, matched = 0;

	memset(map, 0, sizeof(*map));
	note[0] = '\0';

	if (m)
		r = &m->routes[ua_rate_band(rate)][mode][dir];

	if (r && r->ids && r->count) {
		for (i = 0; i < r->count && map->channels < UA_MAX_CHANNELS;
		     i++) {
			u16 id = r->ids[i];
			int slot = -1;

			if (!ua_route_hidden_at_rate(m, id, rate))
				slot = ua_io_desc_find(d, id);
			if (slot < 0) {
				pos += scnprintf(note + pos, note_len - pos,
						 " %04x", id);
				map->slot[map->channels] = UA_CHAN_NO_SLOT;
			} else {
				map->slot[map->channels] = slot;
				matched++;
			}
			map->ch_id[map->channels] = id;
			map->channels++;
		}
		if (matched)
			return;
		memset(map, 0, sizeof(*map));
		scnprintf(note, note_len,
			  " (no route channel matched, exposing all slots)");
	}

	/* No route table, or nothing matched: every descriptor slot */
	for (i = 0; i < d->count; i++) {
		map->slot[i] = i;
		map->ch_id[i] = d->ids[i];
	}
	map->channels = d->count;
}

/* A complete io_map=1 layout, computed before it is committed */
struct ua_io_layout {
	struct ua_chan_map map[UA_DIRS];
	char note[UA_DIRS][96];
	unsigned int play_hw;
	unsigned int rec_hw;
	unsigned int rate;
	unsigned int mode[UA_DIRS];
	bool active;
};

/*
 * Compute the layout the current descriptors, rate and TOSLink modes
 * call for.  Legacy layout until both directions have had a valid
 * descriptor.  Caller holds io_lock.
 */
static void ua_io_map_compute(struct ua_device *ua, struct ua_io_layout *l)
{
	struct ua_audio *audio = &ua->audio;
	unsigned int dir;

	memset(l, 0, sizeof(*l));
	l->rate = audio->sample_rate;
	for (dir = 0; dir < UA_DIRS; dir++)
		l->mode[dir] = ua_route_mode(audio, dir);

	l->active = audio->io_desc[UA_DIR_REC].valid &&
		    audio->io_desc[UA_DIR_PLAY].valid;
	if (l->active) {
		for (dir = 0; dir < UA_DIRS; dir++)
			ua_chan_map_build(ua, dir, l->rate, l->mode[dir],
					  &l->map[dir], l->note[dir],
					  sizeof(l->note[dir]));
		/* kext PrepareTransport: +1 flag channel per diag flags */
		l->rec_hw = audio->io_desc[UA_DIR_REC].count +
			    ((UA_AX_DIAG_FLAGS >> 2) & 1);
		l->play_hw = audio->io_desc[UA_DIR_PLAY].count +
			     ((UA_AX_DIAG_FLAGS & 6) ? 1 : 0);
	} else {
		ua_legacy_counts(ua, &l->play_hw, &l->rec_hw);
		ua_chan_map_legacy(audio->model, UA_DIR_REC, l->rec_hw,
				   &l->map[UA_DIR_REC]);
		ua_chan_map_legacy(audio->model, UA_DIR_PLAY, l->play_hw,
				   &l->map[UA_DIR_PLAY]);
	}
}

/* Does @l differ from the layout the transport and copy path use? */
static bool ua_io_layout_same(const struct ua_audio *audio,
			      const struct ua_io_layout *l)
{
	return l->active == audio->io_map_active &&
	       l->play_hw == audio->play_channels &&
	       l->rec_hw == audio->rec_channels &&
	       !memcmp(l->map, audio->chan_map, sizeof(l->map));
}

static void ua_io_map_log(struct ua_device *ua, const char *why,
			  const struct ua_io_layout *l)
{
	struct ua_audio *audio = &ua->audio;
	struct device *dev = &ua->pdev->dev;
	unsigned int dir;

	dev_info(dev, "io_map (%s): %s layout, %s, %u Hz (%s), record %s / playback %s mode, DMA frame %u play / %u rec\n",
		 why, audio->io_map_active ? "descriptor" : "legacy",
		 audio->model ? audio->model->name : "no model data",
		 audio->io_map_rate,
		 ua_rate_band(audio->io_map_rate) == UA_RATE_BAND_HI ?
			"Hi" : "LoMid",
		 ua_dig_name(audio->io_map_mode[UA_DIR_REC]),
		 ua_dig_name(audio->io_map_mode[UA_DIR_PLAY]),
		 audio->play_channels, audio->rec_channels);

	for (dir = 0; dir < UA_DIRS; dir++) {
		const struct ua_chan_map *map = &audio->chan_map[dir];
		char slots[UA_MAX_CHANNELS * 3 + 1];
		unsigned int i, pos = 0;

		slots[0] = '\0';
		for (i = 0; i < map->channels; i++)
			pos += map->slot[i] == UA_CHAN_NO_SLOT ?
			       scnprintf(slots + pos, sizeof(slots) - pos, " -") :
			       scnprintf(slots + pos, sizeof(slots) - pos,
					 " %u", map->slot[i]);
		dev_dbg(dev, "io_map %s: %u ALSA ch -> slots%s\n",
			ua_dir_name(dir), map->channels, slots);
		if (l->note[dir][0])
			dev_dbg(dev,
				"io_map %s: not carried by the firmware at this rate, kept silent:%s\n",
				ua_dir_name(dir), l->note[dir]);
	}
}

/*
 * Make @l the layout the transport and the copy path use.  Caller holds
 * ua->lock (the transport registers are only written under it, so
 * nothing can arm the transport between this and the next prepare) and
 * io_lock; the transport is stopped.  The copy path is held off by
 * dma_sem.
 */
static void ua_io_map_commit(struct ua_device *ua,
			     const struct ua_io_layout *l, const char *why)
{
	struct ua_audio *audio = &ua->audio;
	bool stride_changed;

	lockdep_assert_held(&ua->lock);
	lockdep_assert_held(&audio->io_lock);

	stride_changed = l->play_hw != audio->play_channels ||
			 l->rec_hw != audio->rec_channels;

	down_write(&audio->dma_sem);
	memcpy(audio->chan_map, l->map, sizeof(audio->chan_map));
	ua_audio_set_hw_layout(audio, l->play_hw, l->rec_hw);
	audio->io_map_active = l->active;
	/* Data laid out at the old stride would play back as noise */
	if (stride_changed && audio->play_buf) {
		memset(audio->play_buf, 0, UA_DMA_BUF_SIZE);
		ua_dma_ring_sync(&ua->pdev->dev, &audio->play_ring, 0,
				 UA_DMA_BUF_SIZE, true);
	}
	up_write(&audio->dma_sem);

	audio->io_map_rate = l->rate;
	memcpy(audio->io_map_mode, l->mode, sizeof(audio->io_map_mode));
	audio->io_layout_stale = false;

	ua_event_post(ua, UA_EVT_LAYOUT_CHANGED,
		      (l->play_hw << 16) | (l->rec_hw & 0xffff));
	ua_io_map_log(ua, why, l);
}

/*
 * Push the open substreams that are running, paused or prepared into
 * XRUN, so userspace prepares again and ua_pcm_prepare() restarts the
 * transport on the new layout under ua->lock.  For a running stream this
 * is what snd_pcm_stop_xrun() does; a paused or prepared one would
 * otherwise go on (or start) on the old layout.  Process context;
 * audio->lock keeps the substreams alive (pcm_close clears them under
 * it) and is taken before the stream lock, as in the period timer.
 */
static void ua_io_map_force_reprepare(struct ua_device *ua)
{
	struct ua_audio *audio = &ua->audio;
	struct snd_pcm_substream *subs[UA_DIRS];
	unsigned long flags, sflags;
	unsigned int i;

	spin_lock_irqsave(&audio->lock, flags);
	subs[0] = audio->play_sub;
	subs[1] = audio->rec_sub;
	for (i = 0; i < ARRAY_SIZE(subs); i++) {
		struct snd_pcm_substream *sub = subs[i];
		snd_pcm_state_t state;

		if (!sub || !sub->runtime)
			continue;
		snd_pcm_stream_lock_irqsave(sub, sflags);
		state = sub->runtime->status->state;
		if (state == SNDRV_PCM_STATE_RUNNING ||
		    state == SNDRV_PCM_STATE_PAUSED ||
		    state == SNDRV_PCM_STATE_PREPARED)
			snd_pcm_stop(sub, SNDRV_PCM_STATE_XRUN);
		snd_pcm_stream_unlock_irqrestore(sub, sflags);
	}
	spin_unlock_irqrestore(&audio->lock, flags);
}

/*
 * io_map=1: bring the layout in line with the current descriptors, rate
 * and TOSLink modes.  Transport stopped: commit now.  Running: if the
 * layout would change, mark it stale and force the streams to re-prepare
 * (the restart then happens under ua->lock in prepare).  @l receives the
 * layout the next transport start uses.  Caller holds ua->lock and
 * io_lock.
 */
static void ua_io_map_update_into(struct ua_device *ua, const char *why,
				  struct ua_io_layout *l)
{
	struct ua_audio *audio = &ua->audio;

	lockdep_assert_held(&ua->lock);
	lockdep_assert_held(&audio->io_lock);

	ua_io_map_compute(ua, l);

	if (ua_io_layout_same(audio, l)) {
		/* Same slots: only the build parameters to record */
		audio->io_map_rate = l->rate;
		memcpy(audio->io_map_mode, l->mode, sizeof(audio->io_map_mode));
		audio->io_layout_stale = false;
	} else if (audio->transport_running) {
		if (!audio->io_layout_stale)
			dev_info(&ua->pdev->dev,
				 "io_map (%s): IO layout changed while streaming, streams re-prepare\n",
				 why);
		audio->io_layout_stale = true;
		ua_io_map_force_reprepare(ua);
	} else {
		ua_io_map_commit(ua, l, why);
	}
}

/* ua_io_map_update_into() without a caller-side layout (io_map=1 only) */
static void ua_io_map_update_locked(struct ua_device *ua, const char *why)
{
	struct ua_io_layout *l;

	if (!ua_io_map_enabled(ua))
		return;

	l = kzalloc(sizeof(*l), GFP_KERNEL);
	if (!l)
		return;
	ua_io_map_update_into(ua, why, l);
	kfree(l);
}

/*
 * After connect: the kext synthesises notification bits 0 + 1 and reads
 * both areas.  Caller holds ua->lock (probe, resume, prepare, reconnect).
 */
static void ua_io_map_connected(struct ua_device *ua)
{
	struct ua_audio *audio = &ua->audio;

	if (!audio->io_ready)
		return;

	mutex_lock(&audio->io_lock);
	ua_io_desc_read_locked(ua, "connect", true);
	ua_io_map_update_locked(ua, "connect");
	mutex_unlock(&audio->io_lock);
}

/* Notification bits 0/1 (dsp service loop, ua->lock held) */
static void ua_io_desc_notify(struct ua_device *ua)
{
	struct ua_audio *audio = &ua->audio;

	if (!audio->io_ready)
		return;

	mutex_lock(&audio->io_lock);
	if (ua_io_desc_read_locked(ua, "notification", false))
		ua_io_map_update_locked(ua, "notification");
	mutex_unlock(&audio->io_lock);
}

/*
 * Re-read and re-map after a TOSLink mode change: ALSA "Digital Output
 * Mode" / "Digital Input Mode", or SET_MIXER_PARAM 0x21.  Caller holds
 * ua->lock.
 */
void ua_io_map_digital_mode_changed(struct ua_device *ua)
{
	struct ua_audio *audio = &ua->audio;

	if (!audio->io_ready)
		return;

	mutex_lock(&audio->io_lock);
	ua_io_desc_read_locked(ua, "digital mode", true);
	ua_io_map_update_locked(ua, "digital mode");
	mutex_unlock(&audio->io_lock);
}

/*
 * io_map=1, transport about to be prepared (stopped): refresh the
 * descriptors and commit the layout AX_*_CH is about to be written
 * from.  Caller holds ua->lock.
 */
static void ua_io_map_prepare(struct ua_device *ua)
{
	struct ua_audio *audio = &ua->audio;

	if (!ua_io_map_enabled(ua) || !audio->io_ready)
		return;

	mutex_lock(&audio->io_lock);
	ua_io_desc_read_locked(ua, "prepare", false);
	ua_io_map_update_locked(ua, "prepare");
	mutex_unlock(&audio->io_lock);
}

/*
 * pcm_prepare: a stream opened under one layout and prepared under
 * another (descriptor or mode changed in between) keeps its channel
 * count; the copy path pads or drops the difference.  Say so.
 */
static void ua_io_map_check_stream(struct ua_device *ua,
				   struct snd_pcm_substream *sub)
{
	struct ua_audio *audio = &ua->audio;
	unsigned int dir = sub->stream == SNDRV_PCM_STREAM_PLAYBACK ?
			   UA_DIR_PLAY : UA_DIR_REC;
	unsigned int ch;

	if (!ua_io_map_enabled(ua) || !audio->io_ready)
		return;

	mutex_lock(&audio->io_lock);
	ch = audio->chan_map[dir].channels;
	mutex_unlock(&audio->io_lock);

	if (sub->runtime->channels != ch)
		dev_warn(&ua->pdev->dev,
			 "io_map %s: stream has %u channels but the layout now has %u; reopen the stream\n",
			 ua_dir_name(dir), sub->runtime->channels, ch);
}

/* io_map=1: a layout change seen while streaming needs a restart */
static bool ua_io_map_needs_restart(struct ua_device *ua)
{
	struct ua_audio *audio = &ua->audio;
	bool stale;

	if (!ua_io_map_enabled(ua) || !audio->io_ready)
		return false;

	mutex_lock(&audio->io_lock);
	stale = audio->io_layout_stale && audio->transport_running;
	mutex_unlock(&audio->io_lock);
	return stale;
}

/*
 * pcm_open: ALSA channel count for @dir and the ring size in frames.
 * With io_map=1 the layout is brought up to date first, under ua->lock
 * like every other layout change (warm_boot=1 never connects, so the
 * descriptors may not have been read yet), and the count is the one of
 * the layout the next transport start uses: the committed one, or the
 * pending one when it changed while streaming.  Not while a resume
 * re-init holds the lock for seconds: prepare refreshes the layout then.
 */
static unsigned int ua_io_map_open_channels(struct ua_device *ua,
					    unsigned int dir,
					    unsigned int *frames)
{
	struct ua_audio *audio = &ua->audio;
	const struct ua_chan_map *map;
	struct ua_io_layout *l = NULL;
	unsigned int ch, hw;

	/*
	 * Up to UA_MAX_CHANNELS slots the ring is always UA_MAX_BUF_FRAMES
	 * frames (ua_calc_buf_frame_size()), so a layout change never
	 * changes the buffer size an open stream was constrained to.
	 */
	BUILD_BUG_ON(UA_DMA_BUF_SIZE / (UA_MAX_CHANNELS * UA_SAMPLE_BYTES) / 2 <
		     UA_MAX_BUF_FRAMES);

	if (!audio->io_ready) {
		*frames = audio->buf_frame_size;
		return dir == UA_DIR_PLAY ? audio->play_channels :
					    audio->rec_channels;
	}

	if (ua_io_map_enabled(ua) && !READ_ONCE(ua->resume_pending))
		l = kzalloc(sizeof(*l), GFP_KERNEL);
	if (l)
		mutex_lock(&ua->lock);
	mutex_lock(&audio->io_lock);

	map = &audio->chan_map[dir];
	hw = dir == UA_DIR_PLAY ? audio->play_channels : audio->rec_channels;
	if (l && !atomic_read(&ua->shutdown)) {
		if (!audio->io_desc[UA_DIR_REC].valid ||
		    !audio->io_desc[UA_DIR_PLAY].valid)
			ua_io_desc_read_locked(ua, "open", false);
		ua_io_map_update_into(ua, "open", l);
		map = &l->map[dir];
		hw = dir == UA_DIR_PLAY ? l->play_hw : l->rec_hw;
	}

	ch = map->channels ? map->channels : hw;
	*frames = audio->buf_frame_size;

	mutex_unlock(&audio->io_lock);
	if (l) {
		mutex_unlock(&ua->lock);
		kfree(l);
	}
	return ch;
}

/*
 * After a clock change (the post-start clock write of a rate change in
 * prepare, or ua_audio_set_clock()).  io_map=0: one read-only pass that
 * logs the descriptors.  io_map=1: the firmware may republish its
 * descriptors for the new rate, so poll them for UA_IO_DESC_SETTLE_MS;
 * returns true when a valid layout that read back the same twice in a
 * row differs from the stored one, and stores it.  Reads that are not
 * usable never count as settled.  Caller holds ua->lock.
 */
static bool ua_io_map_settle(struct ua_device *ua)
{
	struct ua_audio *audio = &ua->audio;
	struct ua_io_desc *buf, *applied, *prev, *cur;
	unsigned int t, dir;
	bool changed = false;

	if (!audio->io_ready)
		return false;

	if (!ua_io_map_enabled(ua)) {
		mutex_lock(&audio->io_lock);
		ua_io_desc_read_locked(ua, "rate change", true);
		mutex_unlock(&audio->io_lock);
		return false;
	}

	buf = kcalloc(3 * UA_DIRS, sizeof(*buf), GFP_KERNEL);
	if (!buf)
		return false;
	applied = buf;
	prev = buf + UA_DIRS;
	cur = buf + 2 * UA_DIRS;

	mutex_lock(&audio->io_lock);
	memcpy(applied, audio->io_desc, UA_DIRS * sizeof(*applied));
	mutex_unlock(&audio->io_lock);
	memcpy(prev, applied, UA_DIRS * sizeof(*prev));

	for (t = 0; t < UA_IO_DESC_SETTLE_MS && !changed;
	     t += UA_IO_DESC_POLL_MS) {
		bool stable = true, differs = false;

		msleep(UA_IO_DESC_POLL_MS);
		if (atomic_read(&ua->shutdown))
			break;
		for (dir = 0; dir < UA_DIRS; dir++) {
			ua_io_desc_fetch(ua, dir, &cur[dir]);
			stable &= cur[dir].valid &&
				  ua_io_desc_same(&cur[dir], &prev[dir]);
			differs |= !ua_io_desc_same(&cur[dir], &applied[dir]);
		}
		memcpy(prev, cur, UA_DIRS * sizeof(*prev));
		changed = stable && differs;
	}

	/*
	 * Store a layout that settled.  Otherwise keep the stored one: the
	 * last read only records a bad header, or confirms what is stored
	 * (a valid read that differs but never repeated is left alone —
	 * the notification bits report a republish that comes later).
	 */
	mutex_lock(&audio->io_lock);
	for (dir = 0; dir < UA_DIRS; dir++) {
		if (changed || !prev[dir].valid ||
		    ua_io_desc_same(&prev[dir], &applied[dir]))
			ua_io_desc_store(audio, dir, &prev[dir]);
		ua_io_desc_log(ua, dir, "rate change");
	}
	mutex_unlock(&audio->io_lock);

	kfree(buf);
	return changed;
}

/*
 * io_map=1, after a sample-rate change that did not go through prepare
 * (ua_audio_set_clock(), possibly while streaming): the rate band and
 * the ADAT S/MUX rule depend on the rate, and the firmware may republish
 * its descriptors.  A changed layout is handled like any other (commit,
 * or stale + forced re-prepare).  Caller holds ua->lock.
 */
static void ua_io_map_rate_changed(struct ua_device *ua)
{
	struct ua_audio *audio = &ua->audio;

	if (!ua_io_map_enabled(ua) || !audio->io_ready)
		return;

	ua_io_map_settle(ua);

	mutex_lock(&audio->io_lock);
	ua_io_map_update_locked(ua, "rate change");
	mutex_unlock(&audio->io_lock);
}

/*
 * Snapshot of the map for UA_IOCTL_GET_IO_MAP and /proc (one walk, one
 * naming rule, one rate/mode source: the parameters the applied layout
 * was built for, or the legacy layout's).
 */
void ua_io_map_get_info(struct ua_device *ua, struct ua_io_map_info *info)
{
	struct ua_audio *audio = &ua->audio;
	unsigned int dir, i;

	memset(info, 0, sizeof(*info));
	info->io_map = ua_io_map_enabled(ua) ? 1 : 0;
	info->device_type = ua->device_type;

	if (!audio->io_ready)
		return;

	BUILD_BUG_ON(UA_IO_MAP_MAX_CH < UA_MAX_CHANNELS);
	BUILD_BUG_ON(UA_IO_MAP_NAME_LEN < 17);

	mutex_lock(&audio->io_lock);
	info->active = audio->io_map_active;
	info->stale = audio->io_layout_stale;
	info->sample_rate = audio->io_map_active ? audio->io_map_rate :
						    audio->sample_rate;

	for (dir = 0; dir < UA_DIRS; dir++) {
		struct ua_io_map_dir *o = dir == UA_DIR_REC ? &info->rec :
							      &info->play;
		const struct ua_io_desc *d = &audio->io_desc[dir];
		const struct ua_chan_map *map = &audio->chan_map[dir];

		o->desc_valid = d->valid;
		o->desc_count = d->count;
		o->desc_flags = d->hdr[UA_IO_DESC_FLAGS];
		o->desc_last_bad = audio->io_desc_bad[dir];
		memcpy(o->last_hdr, audio->io_desc_bad[dir] ?
				    audio->io_desc_bad_hdr[dir] : d->hdr,
		       sizeof(o->last_hdr));
		o->hw_channels = dir == UA_DIR_REC ? audio->rec_channels :
						     audio->play_channels;
		o->channels = map->channels;
		if (audio->io_map_active)
			o->digital_mode = audio->io_map_mode[dir];
		else
			o->digital_mode = audio->model ?
					  audio->model->legacy_mode :
					  UA_DIG_SPDIF;
		for (i = 0; i < d->count; i++)
			o->desc_ids[i] = d->ids[i];
		for (i = 0; i < map->channels; i++) {
			o->chan[i].ch_id = map->ch_id[i];
			o->chan[i].slot = map->slot[i] == UA_CHAN_NO_SLOT ?
					  0xFFFF : map->slot[i];
			ua_chan_map_label(map, i, o->chan[i].name,
					  sizeof(o->chan[i].name));
		}
	}
	mutex_unlock(&audio->io_lock);
}

/* /proc/asound/cardN/io_map: prints ua_io_map_get_info() */
static void ua_io_map_proc_read(struct snd_info_entry *entry,
				struct snd_info_buffer *buf)
{
	struct ua_device *ua = entry->private_data;
	struct ua_audio *audio = &ua->audio;
	struct ua_io_map_info *info;
	unsigned int dir, i;

	info = kzalloc(sizeof(*info), GFP_KERNEL);
	if (!info)
		return;
	ua_io_map_get_info(ua, info);

	snd_iprintf(buf, "io_map\t\t%u (%s layout%s)%s\n", info->io_map,
		    info->active ? "descriptor" : "legacy",
		    io_map < 0 ? ", auto" : "",
		    info->stale ? ", stale: streams re-prepare" : "");
	snd_iprintf(buf, "model\t\t%s (device type 0x%02x)\n",
		    audio->model ? audio->model->name : "no model data",
		    info->device_type);
	snd_iprintf(buf, "public_type\t0x%02x\n", ua->public_type);
	snd_iprintf(buf, "rate\t\t%u Hz\n", info->sample_rate);

	for (dir = 0; dir < UA_DIRS; dir++) {
		const struct ua_io_map_dir *o = dir == UA_DIR_REC ?
						&info->rec : &info->play;
		const u32 *h = o->last_hdr;

		snd_iprintf(buf, "\n%s: %u ALSA channels, DMA frame %u slots, %s mode\n",
			    ua_dir_name(dir), o->channels, o->hw_channels,
			    ua_dig_name(o->digital_mode));
		for (i = 0; i < o->channels; i++)
			snd_iprintf(buf, "  ch %2u  slot %2u  0x%04x  %s\n",
				    i, o->chan[i].slot, o->chan[i].ch_id,
				    o->chan[i].name);

		if (o->desc_last_bad)
			snd_iprintf(buf, "%s descriptor: last read not usable (%08x %08x %08x %08x %08x %08x)%s\n",
				    ua_dir_name(dir), h[0], h[1], h[2], h[3],
				    h[4], h[5],
				    o->desc_valid ? ", last valid one below" :
						    "");
		if (!o->desc_valid)
			continue;
		snd_iprintf(buf, "%s descriptor: %u slots, word5 0x%x\n",
			    ua_dir_name(dir), o->desc_count, o->desc_flags);
		for (i = 0; i < o->desc_count; i++) {
			char name[UA_IO_MAP_NAME_LEN];

			ua_chan_name(o->desc_ids[i], name, sizeof(name));
			snd_iprintf(buf, "  slot %2u  0x%04x  %s\n", i,
				    o->desc_ids[i], name);
		}
	}

	kfree(info);
}

/* ----------------------------------------------------------------
 * Clock parity: lock state, AX_CONTROL fault flags, bus read latency,
 * the notification ack, the device's clock-source list
 *
 * Reads and counters only unless a module parameter above says otherwise.
 * Everything here runs with ua->lock held.
 * ---------------------------------------------------------------- */

/* Rate band for the per-band tables: 0 = 44.1/48, 1 = 88.2/96, 2 = 176.4/192 kHz */
static unsigned int ua_rate_idx_band(unsigned int idx)
{
	switch (idx) {
	case UA_RATE_88200:
	case UA_RATE_96000:
		return 1;
	case UA_RATE_176400:
	case UA_RATE_192000:
		return 2;
	default:
		return 0;
	}
}

/* Hz for a rate index 1..6, 0 otherwise */
static unsigned int ua_rate_hz(unsigned int idx)
{
	static const unsigned int hz[] = {
		0, 44100, 48000, 88200, 96000, 176400, 192000,
	};

	return idx < ARRAY_SIZE(hz) ? hz[idx] : 0;
}

/* The rate index (1..6) the firmware reports in a 0xC07C word, else 0 */
static unsigned int ua_fw_rate_index(u32 rate_info)
{
	unsigned int idx = (rate_info >> 8) & 0xff;

	return idx >= UA_RATE_44100 && idx <= UA_RATE_192000 ? idx : 0;
}

/*
 * Clock Locked = (0xC07C & 3) == 1, the Mixer Engine's GetLockedState
 * (parity/12 C06).  The first reading only seeds the state; later changes
 * notify the ALSA control and post UA_EVT_CLOCK_LOCK.
 */
static void ua_clock_lock_update(struct ua_device *ua, u32 rate_info)
{
	struct ua_audio *audio = &ua->audio;
	bool locked;

	if (rate_info == 0xFFFFFFFF)
		return;
	audio->last_rate_info = rate_info;
	locked = (rate_info & 3) == 1;

	if (!audio->clock_lock_valid) {
		WRITE_ONCE(audio->clock_locked, locked);
		audio->clock_lock_valid = true;
		dev_dbg(&ua->pdev->dev, "clock lock: %s (rate info 0x%08x)\n",
			locked ? "locked" : "not locked", rate_info);
		return;
	}
	if (locked == audio->clock_locked)
		return;

	WRITE_ONCE(audio->clock_locked, locked);
	audio->clock_lock_changes++;
	dev_info(&ua->pdev->dev, "clock %s (rate info 0x%08x)\n",
		 locked ? "locked" : "lost lock", rate_info);
	ua_ctl_notify_name(ua, "Clock Locked");
	ua_event_post(ua, UA_EVT_CLOCK_LOCK, locked ? 1 : 0);
}

/*
 * AX_CONTROL bits 7 and 8 are fault flags the kext and Windows latch and
 * log once at error level (TransportPosition, parity/12 T12).  Count every
 * sample with a bit set; latch and log the first until the next prepare
 * (the kext clears its diag status in PrepareTransport).  0x22C0 is read
 * with the bit 7 log, as the kext does.
 */
static void ua_audio_check_ax_faults(struct ua_device *ua, u32 ctrl,
				     const char *where)
{
	struct ua_audio *audio = &ua->audio;

	if (ctrl == 0xFFFFFFFF)
		return;

	if (ctrl & BIT(7)) {
		audio->ax_fault_count[0]++;
		if (!(audio->ax_fault_latch & BIT(0))) {
			audio->ax_fault_latch |= BIT(0);
			audio->ax_fault_last_ctrl = ctrl;
			audio->ax_fault_status = ua_read(ua, UA_REG_AX_STATUS);
			dev_err_ratelimited(&ua->pdev->dev,
					    "AX_CONTROL fault flag bit 7 set (%s): AX_CONTROL=0x%03x AX_STATUS=0x%08x\n",
					    where, ctrl, audio->ax_fault_status);
		}
	}
	if (ctrl & BIT(8)) {
		audio->ax_fault_count[1]++;
		if (!(audio->ax_fault_latch & BIT(1))) {
			audio->ax_fault_latch |= BIT(1);
			audio->ax_fault_last_ctrl = ctrl;
			dev_err_ratelimited(&ua->pdev->dev,
					    "AX_CONTROL fault flag bit 8 set (%s): AX_CONTROL=0x%03x\n",
					    where, ctrl);
		}
	}
}

/*
 * One read of AX_CONTROL while the transport runs: the fault flags, and
 * how long the BAR0 read took (counted in buckets; reads of 1 ms or more
 * are logged).  The kext asks the platform to cap bus stalls at 10 us for
 * as long as the engine runs (requireMaxBusStall, parity/12 T16); this is
 * the count-only side of it.  stall_qos_us adds the QoS request itself.
 */
static void ua_audio_poll_transport(struct ua_device *ua)
{
	struct ua_audio *audio = &ua->audio;
	u64 t0, dt;
	u32 ctrl;

	if (!audio->transport_running)
		return;

	t0 = ktime_get_ns();
	ctrl = ua_read(ua, UA_REG_AX_CONTROL);
	dt = ktime_get_ns() - t0;

	audio->stall_samples++;
	if (dt > audio->stall_max_ns)
		audio->stall_max_ns = dt;
	if (dt < 10000)
		audio->stall_hist[0]++;
	else if (dt < 50000)
		audio->stall_hist[1]++;
	else if (dt < 100000)
		audio->stall_hist[2]++;
	else if (dt < 1000000)
		audio->stall_hist[3]++;
	else {
		audio->stall_hist[4]++;
		dev_warn_ratelimited(&ua->pdev->dev,
				     "bus stall: AX_CONTROL read took %llu us\n",
				     div_u64(dt, 1000));
	}

	ua_audio_check_ax_faults(ua, ctrl, "service poll");
}

/* stall_qos_us: hold / release the CPU latency request around the transport */
static void ua_audio_stall_qos(struct ua_device *ua, bool on)
{
	struct ua_audio *audio = &ua->audio;

	if (on) {
		if (stall_qos_us <= 0 || audio->stall_qos)
			return;
		audio->stall_qos = kzalloc(sizeof(*audio->stall_qos), GFP_KERNEL);
		if (!audio->stall_qos)
			return;
		cpu_latency_qos_add_request(audio->stall_qos, stall_qos_us);
		dev_info(&ua->pdev->dev,
			 "CPU latency QoS %d us held while streaming\n",
			 stall_qos_us);
	} else if (audio->stall_qos) {
		cpu_latency_qos_remove_request(audio->stall_qos);
		kfree(audio->stall_qos);
		audio->stall_qos = NULL;
	}
}

/*
 * Acknowledge the notification status (0xC030 = 0).  The kext's handler
 * reads 0xC084, 0xC07C and 0xC080 first when bit 4 or 22 is set
 * (parity/12 C08); notif_kext_reads does the same.  Without it this is the
 * single write the driver always did.
 */
static void ua_audio_notif_ack(struct ua_device *ua, u32 notif)
{
	struct ua_audio *audio = &ua->audio;

	if (notif_kext_reads &&
	    (notif & (UA_NOTIF_BIT_SRATE | UA_NOTIF_BIT_TRANSPORT))) {
		audio->kr_clock_info = ua_read(ua, UA_REG_NOTIF_CLOCK_INFO);
		audio->kr_rate_info = ua_read(ua, UA_REG_NOTIF_RATE_INFO);
		audio->kr_xport_info = ua_read(ua, UA_REG_NOTIF_XPORT_INFO);
		audio->kr_count++;
		dev_info(&ua->pdev->dev,
			 "notification 0x%08x: clock_info(0xC084)=0x%08x rate_info(0xC07C)=0x%08x xport_info(0xC080)=0x%08x\n",
			 notif, audio->kr_clock_info, audio->kr_rate_info,
			 audio->kr_xport_info);
		ua_clock_lock_update(ua, audio->kr_rate_info);
	}
	ua_write(ua, UA_REG_NOTIF_STATUS, 0);
}

/*
 * The firmware's own latency words (0xC198 and 0xC19C, the kext's
 * this+0x1bc / +0x1c0; output / input is inferred, parity/12 L03), kept
 * per rate index for the proc file.
 */
static void ua_audio_read_fw_latency(struct ua_device *ua, const char *why)
{
	struct ua_audio *audio = &ua->audio;
	unsigned int idx;
	u32 out, in;

	out = ua_read(ua, UA_REG_NOTIF_LAT_OUT);
	in = ua_read(ua, UA_REG_NOTIF_LAT_IN);
	if (out == 0xFFFFFFFF && in == 0xFFFFFFFF)
		return;

	idx = ua_fw_rate_index(ua_read(ua, UA_REG_NOTIF_RATE_INFO));
	if (!idx)
		idx = audio->rate_index;
	if (idx >= UA_RATE_44100 && idx <= UA_RATE_192000) {
		audio->fw_lat_out[idx] = out;
		audio->fw_lat_in[idx] = in;
		audio->fw_lat_seen[idx] = true;
	}
	dev_info(&ua->pdev->dev,
		 "firmware latency words (%s, rate index %u): out(0xC198)=%u in(0xC19C)=%u\n",
		 why, idx, out, in);
}

/* ---- the device's clock-source list (kext _parseNames, parity/12 C07) ---- */

/* clock_src_from_device and a list has been read */
static bool ua_clock_list_active(const struct ua_audio *audio)
{
	return clock_src_from_device && READ_ONCE(audio->clock_list_n);
}


static const char *ua_clock_fallback_name(unsigned int src, char *buf,
					  size_t len)
{
	switch (src) {
	case UA_CLOCK_SPDIF:
		return "S/PDIF";
	case UA_CLOCK_ADAT:
		return "ADAT";
	case UA_CLOCK_WORDCLOCK:
		return "Word Clock";
	case UA_CLOCK_INTERNAL:
		return "Internal";
	default:
		scnprintf(buf, len, "Source %u", src);
		return buf;
	}
}

/*
 * Names are backslash separated and the position is the source value:
 * token n is the name of source n, an empty token leaves it unnamed.  A NUL
 * ends the text.  What the kext does with a doubled backslash is not
 * certain (parity/12 vs the titan dump), so an empty token is only a gap
 * here; the raw bytes are in /proc/asound/cardN/clock to settle it.
 */
static void ua_clock_parse_names(const u8 *raw,
				 char names[UA_CLOCK_LIST_MAX][UA_CLOCK_NAME_LEN])
{
	unsigned int i, pos = 0, len = 0;
	char cur[UA_CLOCK_NAME_LEN];

	memset(names, 0, UA_CLOCK_LIST_MAX * UA_CLOCK_NAME_LEN);
	for (i = 0; i <= UA_CLOCK_NAMES_BYTES && pos < UA_CLOCK_LIST_MAX; i++) {
		u8 c = i < UA_CLOCK_NAMES_BYTES ? raw[i] : 0;

		if (c == '\\' || c == 0) {
			cur[len] = 0;
			if (len)
				strscpy(names[pos], cur, UA_CLOCK_NAME_LEN);
			pos++;
			len = 0;
			if (c == 0)
				break;
		} else if (c >= 0x20 && c < 0x7f && len < sizeof(cur) - 1) {
			cur[len++] = c;
		}
	}
}

/*
 * Read 0xC090.. and build the enum list from the offered mask: Internal
 * first, then the other sources in ascending order, source 0xA (peer) and
 * anything above 0xC left out, as the kext's menu does.  Called from the
 * connect with ua->lock held.  Always run (reads and a log); the Clock
 * Source control only uses the result with clock_src_from_device.
 */
static void ua_clock_list_build(struct ua_device *ua, u16 mask)
{
	struct ua_audio *audio = &ua->audio;
	/* Scratch off the stack: three name tables are over the frame limit */
	struct {
		char names[UA_CLOCK_LIST_MAX][UA_CLOCK_NAME_LEN];
		char out[UA_CLOCK_LIST_MAX][UA_CLOCK_NAME_LEN];
		u8 raw[UA_CLOCK_NAMES_BYTES];
		u8 val[UA_CLOCK_LIST_MAX];
	} *t;
	unsigned int i, n = 0, named = 0;
	bool changed;
	char tmp[16];

	t = kzalloc(sizeof(*t), GFP_KERNEL);
	if (!t)
		return;

	for (i = 0; i < UA_CLOCK_NAMES_BYTES / 4; i++) {
		u32 w = ua_read(ua, UA_REG_NOTIF_CLOCK_NAMES + i * 4);

		t->raw[i * 4 + 0] = w;
		t->raw[i * 4 + 1] = w >> 8;
		t->raw[i * 4 + 2] = w >> 16;
		t->raw[i * 4 + 3] = w >> 24;
	}
	ua_clock_parse_names(t->raw, t->names);

	memset(t->out, 0, sizeof(t->out));
	if (mask & BIT(UA_CLOCK_INTERNAL))
		t->val[n++] = UA_CLOCK_INTERNAL;
	for (i = 0; i < UA_CLOCK_INTERNAL; i++) {
		if (i == UA_CLOCK_PEER)
			continue;
		if (mask & BIT(i))
			t->val[n++] = i;
	}
	for (i = 0; i < n; i++) {
		if (t->names[t->val[i]][0]) {
			strscpy(t->out[i], t->names[t->val[i]], UA_CLOCK_NAME_LEN);
			named++;
		} else {
			strscpy(t->out[i], ua_clock_fallback_name(t->val[i], tmp,
							       sizeof(tmp)),
				UA_CLOCK_NAME_LEN);
		}
	}

	spin_lock(&audio->clock_list_lock);
	changed = audio->clock_list_n != n ||
		  memcmp(audio->clock_list_val, t->val, n) ||
		  memcmp(audio->clock_list_name, t->out, n * UA_CLOCK_NAME_LEN);
	memcpy(audio->clock_names_raw, t->raw, sizeof(t->raw));
	memcpy(audio->clock_list_val, t->val, n);
	memcpy(audio->clock_list_name, t->out, n * UA_CLOCK_NAME_LEN);
	audio->clock_names_ok = n && named == n;
	WRITE_ONCE(audio->clock_list_n, n);
	spin_unlock(&audio->clock_list_lock);

	dev_info(&ua->pdev->dev,
		 "clock sources offered 0x%04x: %u in the menu, %u named by the device%s\n",
		 mask, n, named, changed ? "" : " (unchanged)");
	for (i = 0; i < n; i++)
		dev_dbg(&ua->pdev->dev, "  clock source 0x%02x \"%s\"\n",
			t->val[i], t->out[i]);

	kfree(t);

	if (changed && clock_src_from_device)
		ua_ctl_notify_info_name(ua, "Clock Source");
}

/* ----------------------------------------------------------------
 * DSP service loop — periodic readback drain
 *
 * The Apollo DSP firmware needs periodic "servicing" from the host to
 * stay alive.  On macOS, the kext calls CDSPResourceManager::Service()
 * ~103/sec via ProcessPlugin (IOKit SEL119).  Without this, the DSP
 * halts: front panel freezes, mixer settings are written but never
 * processed (SEQ_RD never advances).
 *
 * Hypothesis: the readback drain cycle — read status at 0x3810, read
 * 40 data words at 0x3814+, write 0 to 0x3810 to re-arm — acts as the
 * host liveness signal.  We run this at 10 Hz via delayed_work.
 * ---------------------------------------------------------------- */

static void ua_dsp_service_handler(struct work_struct *work)
{
	struct ua_audio *audio = container_of(to_delayed_work(work),
					      struct ua_audio,
					      dsp_service_work);
	struct ua_device *ua = container_of(audio, struct ua_device, audio);
	u32 rb_status, seq_wr, seq_rd, notif;
	u32 data[UA_MIXER_RB_WORDS];
	unsigned int i;

	/* Device removed — stop servicing */
	if (atomic_read(&ua->shutdown)) {
		audio->dsp_service_running = false;
		return;
	}

	mutex_lock(&ua->lock);

	/*
	 * Poll and clear notification status register.
	 *
	 * The kext's interrupt handler continuously reads and clears
	 * notification events from this register.  If the DSP fires
	 * notifications (transport change, IO descriptors, etc.) and
	 * the host never clears them, the DSP may stall.
	 *
	 * IMPORTANT: This register is NOT write-1-to-clear!
	 * Write 0 to clear all bits (verified experimentally).
	 */
	notif = ua_read(ua, UA_REG_NOTIF_STATUS);

	/* Device removed (Thunderbolt hot-unplug) — stop servicing */
	if (notif == 0xFFFFFFFF) {
		mutex_unlock(&ua->lock);
		dev_warn(&ua->pdev->dev,
			 "dsp_service: device gone (0xFFFFFFFF), stopping\n");
		audio->dsp_service_running = false;
		return;
	}

	if (notif) {
		ua_audio_notif_ack(ua, notif);

		/* Log notification events (first 10 + every 100th) */
		if (audio->dsp_service_count < 10 ||
		    (audio->dsp_service_count % 100) == 0)
			dev_dbg(&ua->pdev->dev,
				"dsp_service[%u]: notif=0x%08x (cleared)\n",
				audio->dsp_service_count, notif);

		/* Bits 0/1: the firmware republished an IO descriptor */
		if (notif & (UA_NOTIF_BIT_OUT_IO | UA_NOTIF_BIT_IN_IO))
			ua_io_desc_notify(ua);
		ua_event_post_notif(ua, notif);
	}

	/* Clock lock, AX_CONTROL fault flags and bus read latency (reads) */
	ua_clock_lock_update(ua, ua_read(ua, UA_REG_NOTIF_RATE_INFO));
	ua_audio_poll_transport(ua);

	rb_status = ua_read(ua, UA_REG_MIXER_RB_STATUS);

	if (rb_status == 1) {
		/* Readback ready — drain 40 words into cache, then re-arm */
		for (i = 0; i < UA_MIXER_RB_WORDS; i++) {
			data[i] = ua_read(ua,
					  UA_REG_MIXER_RB_DATA + i * 4);
			ua->mixer_rb_data[i] = data[i];
		}
		ua->mixer_rb_status = 1;
		ua_write(ua, UA_REG_MIXER_RB_STATUS, 0);

		/* Front-panel changes → ALSA controls (no write back) */
		ua_mixer_sync_readback(ua);

		if (!ua->mixer_rb_seen) {
			ua->mixer_rb_seen = true;
			dev_dbg(&ua->pdev->dev,
				"dsp_service: first readback seen\n");
		}
	}

	/* Flush pending mixer settings (batch protocol).
	 * Defer flush 5 cycles (~0.5s) to let DSP stabilize after
	 * connect.  After rate change, service count resets to 0
	 * so mixer re-init flushes promptly. */
	if (audio->dsp_service_count >= 5)
		ua_mixer_flush_settings(ua);

	seq_wr = ua_read(ua, UA_REG_MIXER_SEQ_WR);
	seq_rd = ua_read(ua, UA_REG_MIXER_SEQ_RD);

	mutex_unlock(&ua->lock);

	/* Log first 3, then every 100th up to 500, then go silent */
	if (audio->dsp_service_count < 3 ||
	    (audio->dsp_service_count < 500 &&
	     (audio->dsp_service_count % 100) == 0)) {
		if (rb_status == 1)
			dev_dbg(&ua->pdev->dev,
				"dsp_service[%u]: rb=1 SEQ_WR=%u SEQ_RD=%u data[0:3]=%08x %08x %08x %08x\n",
				audio->dsp_service_count,
				seq_wr, seq_rd,
				data[0], data[1], data[2], data[3]);
		else
			dev_dbg(&ua->pdev->dev,
				"dsp_service[%u]: rb=%u SEQ_WR=%u SEQ_RD=%u\n",
				audio->dsp_service_count,
				rb_status, seq_wr, seq_rd);
	}

	audio->dsp_service_count++;

	/* Re-schedule if still running */
	if (audio->dsp_service_running)
		schedule_delayed_work(&audio->dsp_service_work,
				      msecs_to_jiffies(UA_DSP_SERVICE_INTERVAL_MS));
}

void ua_dsp_service_start(struct ua_device *ua)
{
	struct ua_audio *audio = &ua->audio;

	if (audio->dsp_service_running)
		return;

	dev_dbg(&ua->pdev->dev, "starting DSP service loop (%u ms)\n",
		UA_DSP_SERVICE_INTERVAL_MS);

	audio->dsp_service_running = true;
	audio->dsp_service_count = 0;
	schedule_delayed_work(&audio->dsp_service_work,
			      msecs_to_jiffies(UA_DSP_SERVICE_INTERVAL_MS));
}

void ua_dsp_service_stop(struct ua_device *ua)
{
	struct ua_audio *audio = &ua->audio;

	if (!audio->dsp_service_running)
		return;

	dev_dbg(&ua->pdev->dev, "stopping DSP service loop (%u cycles)\n",
		audio->dsp_service_count);

	audio->dsp_service_running = false;
	cancel_delayed_work_sync(&audio->dsp_service_work);
}

/* Forward declarations for connect helpers */
static void ua_audio_write_connect_config(struct ua_device *ua);
static void ua_audio_read_connect_readback(struct ua_device *ua);

/*
 * A host that went away without disconnecting (rmmod, a reboot, a crash, or
 * a hand-off from macOS/Windows) leaves the audio extension connected, and
 * it ignores a new ACEFACE until it has seen the disconnect doorbell.  On an
 * x4 (2026-10-09) no connect ack ever came after a quiesce without the
 * doorbell; writing 0x10 to AX_CONNECT, then ACEFACE + connect, was acked at
 * once and left the mixer DSP's SEQ counters untouched.
 */
static void ua_aceface_clear_stale(struct ua_device *ua)
{
	dev_info(&ua->pdev->dev,
		 "no ACEFACE ack yet: sending the disconnect doorbell to clear a stale host connection\n");
	ua_write(ua, UA_REG_AX_CONNECT, UA_AX_DISCONNECT);
	(void)ua_read(ua, UA_REG_FPGA_REV);
	msleep(50);
}

/* ----------------------------------------------------------------
 * Early ACEFACE handshake — called from ua_core.c probe path
 * BEFORE firmware loading on cold boot.
 *
 * The FPGA ring buffer engine appears to be gated behind the
 * ACEFACE notification handshake.  Without this, ring POSITION
 * stays at 0 and firmware blocks are never consumed.
 *
 * This does only the handshake + config write + readback.
 * The full ua_audio_connect() handles channel counts, routing,
 * SEQ sync, and service handler start.
 * ---------------------------------------------------------------- */
int ua_aceface_handshake(struct ua_device *ua)
{
	int retry, poll;
	u32 status;

	/*
	 * Quick ACEFACE probe: 2 retries x 10 polls x 50ms = ~1s max.
	 *
	 * The full 20s timeout (UA_CONNECT_RETRIES) blocks the PCI probe
	 * path and freezes the system on warm reboot when the Apollo
	 * firmware doesn't respond.  If it doesn't connect quickly here,
	 * set aceface_failed and let hot-replug (power cycle) handle it.
	 */
#define EARLY_ACEFACE_RETRIES   2
#define EARLY_ACEFACE_POLL_MS   50

	if (ua->aceface_done)
		return 0;

	dev_dbg(&ua->pdev->dev,
		"early ACEFACE: writing 0x%08x to 0x%04x, polling 0x%04x\n",
		UA_AX_CONNECT_MAGIC, UA_REG_NOTIF_ACEFACE,
		UA_REG_NOTIF_STATUS);

	for (retry = 0; retry < EARLY_ACEFACE_RETRIES; retry++) {
		if (retry == 1)
			ua_aceface_clear_stale(ua);

		ua_write(ua, UA_REG_NOTIF_ACEFACE, UA_AX_CONNECT_MAGIC);
		ua_write(ua, UA_REG_AX_CONNECT, UA_AX_CONNECT);

		for (poll = 0; poll < UA_CONNECT_POLLS; poll++) {
			status = ua_read(ua, UA_REG_NOTIF_STATUS);

			if (status == 0xFFFFFFFF) {
				dev_err(&ua->pdev->dev,
					"early ACEFACE: device gone!\n");
				return -ENODEV;
			}

			if (status & UA_NOTIF_BIT_CONNECT) {
				dev_info(&ua->pdev->dev,
					 "early ACEFACE connected (status=0x%08x, retry=%d poll=%d)\n",
					 status, retry, poll);

				ua_audio_write_connect_config(ua);
				ua_audio_read_connect_readback(ua);

				/* Clear notification status (write 0, not W1C) */
				ua_write(ua, UA_REG_NOTIF_STATUS, 0);

				ua->aceface_done = true;
				ua_pcie_setup(ua);
				return 0;
			}
			msleep(EARLY_ACEFACE_POLL_MS);
		}
	}

	dev_warn(&ua->pdev->dev,
		 "early ACEFACE timed out — power-cycle Apollo to connect\n");
	ua->aceface_failed = true;
	return -ETIMEDOUT;
}

/* ----------------------------------------------------------------
 * Cold boot transport start/stop
 *
 * The FPGA ring buffer engine requires the FULL PrepareTransport
 * register sequence before it processes ring entries.  Discovered
 * 2026-03-01: just writing AX_CONTROL=0x20F is insufficient.
 *
 * This encapsulates:
 *   1. Model channel count lookup
 *   2. Full PrepareTransport register sequence
 *   3. Transport start (AX_CONTROL = 0x20F)
 *   4. ACEFACE notification handshake
 *   5. DMA reset pulse to activate ring buffers
 *
 * Called from ua_core.c cold boot path before firmware loading.
 * ua_boot_transport_stop() must be called after FW load completes.
 * ---------------------------------------------------------------- */

int ua_boot_transport_start(struct ua_device *ua)
{
	unsigned int block_size, play, rec, frames;
	u32 readback, dma, frame_ctr, ctrl;
	int ret, retries;

	/*
	 * Legacy model counts, in locals.  This pre-firmware transport only
	 * has to run for the ring engine; it writes the counts it always
	 * wrote, and it leaves the layout the PCM copy path uses alone —
	 * on the resume cold path (ua_resume_work()) that layout belongs
	 * to ua_io_map_commit() and may be the io_map=1 one.  The next
	 * connect and prepare re-read the descriptors and re-apply it.
	 */
	ua_legacy_counts(ua, &play, &rec);
	frames = ua_calc_buf_frame_size(play, rec);

	dev_dbg(&ua->pdev->dev,
		"cold boot transport: play=%u rec=%u buf=%u\n",
		play, rec, frames);

	/*
	 * Full PrepareTransport register sequence (from kext disassembly).
	 * Order matters — FPGA state machine expects this sequence.
	 */

	/* BUF_SIZE (0x2240) = frameSize - 1 */
	ua_write(ua, UA_REG_AX_BUF_SIZE, frames - 1);

	/* DMA_BLKSZ (0x226C) = ((frameSize-1) * playCh) >> 10 */
	block_size = ((frames - 1) * play) >> 10;
	ua_write(ua, UA_REG_AX_DMA_BLKSZ, block_size);

	/* Clear SAMPLE_POS (0x2244) */
	ua_write(ua, UA_REG_AX_SAMPLE_POS, 0);

	/* Clear AX_CONTROL (0x2248) */
	ua_write(ua, UA_REG_AX_CONTROL, 0);

	/* FRAME_CFG (0x2258) = 0x10 */
	ua_write(ua, UA_REG_AX_FRAME_CFG, 0x10);

	/* Channel counts */
	ua_write(ua, UA_REG_AX_PLAY_CH, play);
	ua_write(ua, UA_REG_AX_REC_CH, rec);

	/* P2P_ROUTE (0x224C) = 0x100 | (play_ch - 1) */
	ua_write(ua, UA_REG_AX_P2P_ROUTE, 0x100 | (play - 1));

	/* ARM — AX_CONTROL = 1 (DMA enable) */
	ua_write(ua, UA_REG_AX_CONTROL, UA_AX_CTRL_DMA_EN);

	/* Read fence */
	readback = ua_read(ua, UA_REG_AX_STATUS);

	/*
	 * CRITICAL: The FPGA state machine needs time between the
	 * ARM write and the START write.  Without this delay, the
	 * transport start is silently ignored (FRAME_CTR stays 0).
	 * The kext has substantial overhead between these writes
	 * (FRAME_CTR polling + vector enable), and the Python ioctl
	 * path has syscall overhead between each write.  Back-to-back
	 * iowrite32 in the kernel is too fast for the FPGA.
	 */
	usleep_range(1000, 2000);

	/* START + extended mode */
	ua_write(ua, UA_REG_AX_CONTROL, UA_AX_CTRL_START_EXT);

	/* Wait for FPGA to acknowledge (FRAME_CTR becomes non-zero) */
	frame_ctr = 0;
	for (retries = 0; retries < 10; retries++) {
		frame_ctr = ua_read(ua, UA_REG_AX_FRAME_CTR);
		if (frame_ctr != 0)
			break;
		usleep_range(1000, 2000);
	}
	dev_dbg(&ua->pdev->dev,
		"cold boot transport: FRAME_CTR=0x%x after %d polls\n",
		frame_ctr, retries + 1);

	ctrl = ua_read(ua, UA_REG_AX_CONTROL);
	dev_dbg(&ua->pdev->dev,
		"cold boot transport started: CTRL=0x%x STATUS=0x%x\n",
		ctrl, readback);

	/* ACEFACE handshake */
	ret = ua_aceface_handshake(ua);
	if (ret)
		dev_warn(&ua->pdev->dev,
			 "early ACEFACE failed (%d) — FW load may fail\n",
			 ret);

	/* DMA reset pulse to activate ring buffer processing */
	dma = ua_read(ua, UA_REG_DMA_CTRL);
	ua_write(ua, UA_REG_DMA_CTRL, dma | UA_DMA_RESET_V2_MASK);
	ua_write(ua, UA_REG_DMA_CTRL, dma);
	(void)ua_read(ua, UA_REG_DMA_CTRL);

	/*
	 * Give DMA engines time to stabilize after reset pulse.
	 * Verified 2026-03-01: without this delay, FPGA ignores
	 * ring entries. 50ms is empirically sufficient.
	 */
	msleep(50);

	dev_dbg(&ua->pdev->dev,
		"cold boot transport: DMA pulse done (DMA_CTRL=0x%x)\n",
		dma);

	return ret;
}

void ua_boot_transport_stop(struct ua_device *ua)
{
	ua_write(ua, UA_REG_AX_CONTROL, 0);

	/*
	 * Keep aceface_done = true.  The early ACEFACE handshake already
	 * established DSP communication; ua_audio_connect() just needs to
	 * do post-handshake work (IO descriptors, mixer init, service start).
	 * Clearing aceface_done forces a re-ACEFACE that can't succeed
	 * because transport is stopped.
	 */
	ua->audio.connected = false;

	dev_dbg(&ua->pdev->dev, "cold boot: stopped transport\n");
}

/* ----------------------------------------------------------------
 * DSP firmware connection handshake
 *
 * From kext CPcieAudioExtension::Connect() disassembly (2026-02-28):
 *
 *   1. Write 0xACEFACE to BAR0 + bank*4 + 0xC004  (= 0xC02C for x4)
 *   2. Write 1 (doorbell) to BAR0 + 0x2260
 *   3. Poll BAR0 + bank*4 + 0xC008 (= 0xC030) for bit 21 (connect response)
 *   4. On bit 21: write 10 config dwords to BAR0 + 0xC000 (NO bank shift)
 *   5. Read 95-dword readback from BAR0 + bank*4 + 0xC000 (= 0xC028)
 *   6. Synthetically inject bits 0+1+22 (force IO descriptor + rate handlers)
 *   7. Signal connect completion
 *
 * CRITICAL: Previous code wrote ACEFACE to 0xC004 and polled 0x22C0.
 * The kext uses bank-shifted addresses: 0xC02C and 0xC030.
 * 0x22C0 is a transport status register, NOT the notification status!
 * ---------------------------------------------------------------- */

static void ua_audio_write_connect_config(struct ua_device *ua)
{
	int i;

	/*
	 * After connect response (bit 21), write 10 config dwords to
	 * BAR0+0xC000 (NO bank shift).  From kext _handleNotificationInterrupt.
	 *
	 * config[0] = bank_shift (0x0A for Apollo x4)
	 * config[1] = readback_count (0x17C = 380)
	 * config[2..9] = 0
	 */
	ua_write(ua, UA_REG_NOTIF_CONFIG + 0, UA_NOTIF_BANK_SHIFT);
	ua_write(ua, UA_REG_NOTIF_CONFIG + 4, UA_NOTIF_READBACK_COUNT);
	for (i = 2; i < UA_NOTIF_CONFIG_WORDS; i++)
		ua_write(ua, UA_REG_NOTIF_CONFIG + i * 4, 0);

	dev_dbg(&ua->pdev->dev,
		"  wrote %d config dwords (bank=0x%x, rb_count=0x%x)\n",
		UA_NOTIF_CONFIG_WORDS, UA_NOTIF_BANK_SHIFT,
		UA_NOTIF_READBACK_COUNT);
}

static void ua_audio_read_connect_readback(struct ua_device *ua)
{
	u32 rb[UA_NOTIF_READBACK_WORDS];
	int i;

	/*
	 * Read 95-dword readback from BAR0 + bank*4 + 0xC000 (= 0xC028).
	 * Contains device info, channel bitmask, channel names.
	 * Log first 8 words for diagnostics.
	 */
	for (i = 0; i < UA_NOTIF_READBACK_WORDS; i++)
		rb[i] = ua_read(ua, UA_REG_NOTIF_READBACK + i * 4);

	dev_dbg(&ua->pdev->dev,
		"  readback[0..7]: %08x %08x %08x %08x %08x %08x %08x %08x\n",
		rb[0], rb[1], rb[2], rb[3], rb[4], rb[5], rb[6], rb[7]);
}

/*
 * {0x001A0002, rate_Hz} to every DSP: what CUAD2DeviceMixer::
 * AckSampleRateChange queues on every clock event (parity/12 C10).
 * Caller holds ua->lock.
 */
static void ua_audio_rate_ack_send(struct ua_device *ua, unsigned int rate,
				   const char *why)
{
	struct ua_audio *audio = &ua->audio;
	int ret = ua_dsp_send_rate_ack(ua, rate);

	audio->rate_ack_sent++;
	audio->rate_acked = rate;
	dev_info(&ua->pdev->dev, "rate ack %u Hz to the DSPs (%s)%s\n", rate,
		 why, ret ? ": a ring was full" : "");
}

/* The same, only with rate_ack_real */
static void ua_audio_rate_ack_real(struct ua_device *ua, unsigned int rate,
				   const char *why)
{
	if (rate_ack_real && rate)
		ua_audio_rate_ack_send(ua, rate, why);
}

/*
 * adopt_fw_rate: take the rate the firmware runs at (0xC07C bits 15:8) as
 * the driver's rate at the first connect after a load or resume, the way
 * the kext's Connect does (parity/12 C04), instead of assuming 48 kHz and
 * forcing it with the post-start clock write.  Only while no stream holds a
 * rate; called before the IO map is built so it is built for that rate.
 * Caller holds ua->lock.
 */
static void ua_audio_adopt_fw_rate(struct ua_device *ua)
{
	struct ua_audio *audio = &ua->audio;
	u32 rate_info;
	unsigned int idx;

	if (!audio->adopt_pending)
		return;
	audio->adopt_pending = false;
	if (!adopt_fw_rate)
		return;
	if (audio->stream_rate[UA_DIR_PLAY] || audio->stream_rate[UA_DIR_REC] ||
	    audio->transport_running)
		return;

	rate_info = ua_read(ua, UA_REG_NOTIF_RATE_INFO);
	idx = ua_fw_rate_index(rate_info);
	if (!idx) {
		dev_info(&ua->pdev->dev,
			 "adopt_fw_rate: firmware reports no usable rate (0x%08x), keeping %u Hz\n",
			 rate_info, audio->sample_rate);
		return;
	}
	if (idx == audio->rate_index) {
		dev_info(&ua->pdev->dev,
			 "adopt_fw_rate: firmware runs at %u Hz, as assumed\n",
			 ua_rate_hz(idx));
		return;
	}
	dev_info(&ua->pdev->dev,
		 "adopt_fw_rate: firmware runs at %u Hz (rate info 0x%08x), was assuming %u Hz\n",
		 ua_rate_hz(idx), rate_info, audio->sample_rate);
	audio->sample_rate = ua_rate_hz(idx);
	audio->rate_index = idx;
	ua_event_post(ua, UA_EVT_RATE_CHANGED, idx);
	ua_ctl_notify_name(ua, "Sample Rate");
}

static int ua_audio_connect(struct ua_device *ua)
{
	struct ua_audio *audio = &ua->audio;
	int retry, poll;
	u32 status;

	if (audio->connected)
		return 0;

	/*
	 * If the early ACEFACE handshake already ran (from cold boot
	 * probe path), skip the handshake loop and go straight to
	 * channel count reads and routing setup.
	 */
	if (ua->aceface_done) {
		dev_dbg(&ua->pdev->dev,
			"ACEFACE already done (early handshake), completing connect\n");
		goto post_handshake;
	}

	/*
	 * ACEFACE handshake from kext CPcieAudioExtension::Connect():
	 *   Each outer retry re-sends ACEFACE magic + CONNECT doorbell,
	 *   then polls notification status for bit 21 (connect response).
	 *
	 * The kext uses up to 20 retries × 9 polls × 100ms = ~18 seconds.
	 *
	 * CRITICAL FIX (2026-02-28): Use bank-shifted registers.
	 *   ACEFACE target: 0xC02C (was 0xC004)
	 *   Status poll:    0xC030 (was 0x22C0)
	 */
	dev_dbg(&ua->pdev->dev,
		"ACEFACE connect: writing 0x%08x to 0x%04x, polling 0x%04x\n",
		UA_AX_CONNECT_MAGIC, UA_REG_NOTIF_ACEFACE,
		UA_REG_NOTIF_STATUS);

	for (retry = 0; retry < UA_CONNECT_RETRIES; retry++) {
		if (retry == 1)
			ua_aceface_clear_stale(ua);

		/* Write handshake magic to bank-shifted ACEFACE register */
		ua_write(ua, UA_REG_NOTIF_ACEFACE, UA_AX_CONNECT_MAGIC);

		/* Send connect doorbell */
		ua_write(ua, UA_REG_AX_CONNECT, UA_AX_CONNECT);

		for (poll = 0; poll < UA_CONNECT_POLLS; poll++) {
			/*
			 * Read notification status from bank-shifted register.
			 * 0xC030 = BAR0 + 0x28 + 0xC008.
			 * (Previously read 0x22C0 which is a transport status
			 * register — completely wrong!)
			 */
			status = ua_read(ua, UA_REG_NOTIF_STATUS);

			/* Check for device gone (Thunderbolt disconnect) */
			if (status == 0xFFFFFFFF)
				return -ENODEV;

			/* Bit 21 = connect response from firmware */
			if (status & UA_NOTIF_BIT_CONNECT) {
				dev_info(&ua->pdev->dev,
					 "audio extension connected (notif_status=0x%08x)\n",
					 status);

				ua_audio_write_connect_config(ua);
				ua_audio_read_connect_readback(ua);
				ua->aceface_done = true;
				ua_pcie_setup(ua);

post_handshake:
				/*
				 * Clear notification status register.
				 *
				 * This register is NOT write-1-to-clear!
				 * Write 0 to clear all bits (verified
				 * experimentally: W1C just re-sets bits).
				 */
				{
					u32 stale = ua_read(ua, UA_REG_NOTIF_STATUS);

					if (stale) {
						ua_write(ua, UA_REG_NOTIF_STATUS, 0);
						dev_dbg(&ua->pdev->dev,
							"  cleared notif status: 0x%08x\n",
							stale);
					}
				}

				/*
				 * Synthetic notification handling.
				 *
				 * The kext's _handleNotificationInterrupt, after
				 * processing bit 21 (connect), synthetically
				 * injects bits 0 + 1 + 22 (OR 0x00400003) and
				 * falls through to handle them in-line.  This
				 * forces the driver to:
				 *   bit 0: read 72 dwords from output IO (0xC2C4)
				 *   bit 1: read 72 dwords from input IO (0xC1A4)
				 *   bit 22: read rate info (0xC07C) + clock (0xC084)
				 *   bit 4: read transport info (0xC080)
				 *
				 * The firmware state machine likely expects the
				 * host to read these registers as acknowledgment
				 * after connect.  Without these reads, the DSP
				 * may not advance to active audio routing.
				 */

				/* adopt_fw_rate: before the map is built for a rate */
				ua_audio_adopt_fw_rate(ua);

				/*
				 * Bits 0 + 1: read both IO descriptors
				 * (read-only), log them, and with io_map=1
				 * rebuild the channel map from them.
				 */
				ua_io_map_connected(ua);

				/* Bit 22: Read rate and clock info */
				{
					u32 rate_info, clock_info;

					rate_info  = ua_read(ua, UA_REG_NOTIF_RATE_INFO);
					clock_info = ua_read(ua, UA_REG_NOTIF_CLOCK_INFO);
					dev_dbg(&ua->pdev->dev,
						"  rate_info=0x%08x clock_info=0x%08x\n",
						rate_info, clock_info);
					ua_clock_lock_update(ua, rate_info);
				}

				/* Clock sources the device offers (kext:
				 * bit 21 handler, mask at 0xC08E); the low
				 * byte is the rate mask [M] */
				{
					u32 srcs = ua_read(ua, UA_REG_NOTIF_CLOCK_SRCS);

					audio->clock_src_mask = srcs >> 16;
					audio->fw_rate_mask = srcs & 0xff;
				}
				dev_dbg(&ua->pdev->dev,
					"  clock sources offered: 0x%04x\n",
					audio->clock_src_mask);
				dev_info(&ua->pdev->dev,
					 "firmware rate mask 0x%02x (0xC08C[7:0], rate indexes offered)\n",
					 audio->fw_rate_mask);
				ua_clock_list_build(ua, audio->clock_src_mask);
				ua_audio_read_fw_latency(ua, "connect");

				/* Bit 4: Read transport info */
				{
					u32 xport_info;

					xport_info = ua_read(ua, UA_REG_NOTIF_XPORT_INFO);
					dev_dbg(&ua->pdev->dev,
						"  xport_info=0x%08x\n",
						xport_info);
				}

				audio->connected = true;
				ua_event_post(ua, UA_EVT_CONNECTED, 0);

				/* kext_connect: CPcieDevice::Connect's ring
				 * connect follows the ACEFACE handshake */
				ua_kext_ring_connect(ua);

				/*
				 * Activate plugin chain AFTER ACEFACE connect
				 * succeeds and BEFORE mixer batch protocol
				 * begins.  This loads the DSP programs that
				 * forward preamp flag writes (PAD/48V/MicLine)
				 * from SRAM to the ARM MCU; without it, those
				 * relays don't toggle.  cmd[2]/cmd[3] of each
				 * DMA_REF entry is substituted with the Linux
				 * dma_addr of the matching payload buffer
				 * loaded earlier via request_firmware().
				 *
				 * Guarded by plugins_activated so replug /
				 * reconnect doesn't re-run, and skipped with
				 * no_plugins=1 (the chain's SRAM_CFG / ROUTING /
				 * MODULE_ACTIVATE commands can overwrite the
				 * capture routing that setting[24]=0x20 sets
				 * up).  Soft-fails if payloads weren't loaded
				 * (DMA_REFs skipped).
				 */
				if (!no_plugins && !ua->plugins_activated) {
					int pc_ret =
						ua_dsp_activate_plugin_chain(ua);
					if (pc_ret == -ENODATA)	/* optional blob absent */
						dev_dbg(&ua->pdev->dev,
							"plugin chain not loaded (no payloads)\n");
					else if (pc_ret)
						dev_warn(&ua->pdev->dev,
							 "plugin chain activate failed: %d\n",
							 pc_ret);
					else
						ua->plugins_activated = true;
				}

				/* rate_ack_real: the firmware's rate to every DSP */
				{
					unsigned int fw_idx =
						ua_fw_rate_index(audio->last_rate_info);

					ua_audio_rate_ack_real(ua,
						fw_idx ? ua_rate_hz(fw_idx) :
							 audio->sample_rate,
						"connect");
				}

				/*
				 * Initialize mixer batch protocol.
				 *
				 * The DSP reads ALL 38 settings atomically
				 * on each SEQ_WR advance.  We must write all
				 * 38 settings before each SEQ_WR bump.
				 *
				 * Sync to current DSP sequence, zero cache.
				 * Do NOT mark dirty — let DSP initialize
				 * before any settings writes.  First flush
				 * happens only when user changes a setting.
				 */
				{
					u32 seq_rd = ua_read(ua, UA_REG_MIXER_SEQ_RD);

					ua->mixer_seq_wr = seq_rd;
					ua_write(ua, UA_REG_MIXER_SEQ_WR, seq_rd);

					memset(ua->mixer_val, 0,
					       sizeof(ua->mixer_val));
					memset(ua->mixer_mask, 0,
					       sizeof(ua->mixer_mask));

					ua->mixer_rb_seen = false;
					ua->mixer_ready = true;

					/*
					 * Activate DSP mixer capture routing.
					 * Setting[24] = 0x20 enables type 0x01
					 * capture (Mic 3-4, Virtual, etc).
					 * Full batch: all 38 with mask=0xFF.
					 *
					 * Multiple flushes needed — DSP processes
					 * setting[24] across 2+ batch cycles.
					 */
					/*
					 * macOS SEL132 sends val=1 mask=0 to
					 * settings 0-11 — but only AFTER ~7s
					 * of DSP processing.  mask=0 means
					 * "preserve DSP firmware defaults."
					 *
					 * Previously we set mask=1 here which
					 * CORRUPTED setting[2] (monitor core:
					 * volume/mute/source/dim) by writing
					 * val=1 before the daemon could set
					 * correct values.  This broke standalone
					 * monitor operation.
					 *
					 * Fix: leave all settings at {0,0} so
					 * the flush skips them (mask==0 check).
					 * The daemon's 50-param monitor init
					 * writes correct values via ioctl later.
					 *
					 * Exception: capture routing requires
					 * settings 1-37 = 0x20/0xFF (Windows
					 * BAR0 capture) plus setting[35] = 0x05
					 * (channel config count).
					 *
					 * SKIP setting[2] — monitor core (volume,
					 * mute, source, dim).  Writing 0x20 to it
					 * corrupts monitor standalone operation.
					 * The daemon's 57-param monitor init sets
					 * setting[2] correctly via ioctl later.
					 */
					{
						int s;
						for (s = 1; s < UA_MIXER_BATCH_COUNT; s++) {
							if (s == 2)
								continue; /* skip monitor */
							ua->mixer_val[s] = 0x20;
							ua->mixer_mask[s] = 0xFF;
						}
						ua->mixer_val[35] = 0x05;
						ua->mixer_mask[35] = 0x1F;
						ua->mixer_dirty = true;
					}

					/*
					 * Kext CPcieDevice::Connect ends with
					 * SetOutputMute(0): undo the mute the
					 * shutdown/suspend quiesce applied.
					 * Cache only — flushed with the batch
					 * above.  No-op except on the device
					 * types that have an output mute.
					 */
					ua_set_output_mute_locked(ua, false, false);

					dev_dbg(&ua->pdev->dev,
						"  mixer init (SEQ=%u, s24=0x20)\n",
						seq_rd);
				}

				/* Post-connect diagnostics */
				{
					u32 dma_ctrl, cmd_pos, resp_pos;
					u32 cmd_pg0_lo, cmd_pg0_hi;
					u32 seq_w, seq_r;
					u32 dsp0 = UA_DSP_BANK_LOW;

					dma_ctrl = ua_read(ua, UA_REG_DMA_CTRL);
					cmd_pos  = ua_read(ua, dsp0 + UA_RING_CMD_OFFSET + UA_RING_POSITION);
					resp_pos = ua_read(ua, dsp0 + UA_RING_RESP_OFFSET + UA_RING_POSITION);
					cmd_pg0_lo = ua_read(ua, dsp0 + UA_RING_CMD_OFFSET + UA_RING_PAGE0_LO);
					cmd_pg0_hi = ua_read(ua, dsp0 + UA_RING_CMD_OFFSET + UA_RING_PAGE0_HI);
					seq_w = ua_read(ua, UA_REG_MIXER_SEQ_WR);
					seq_r = ua_read(ua, UA_REG_MIXER_SEQ_RD);

					dev_dbg(&ua->pdev->dev,
						"  POST-CONNECT: DMA_CTRL=0x%03x "
						"cmd_pos=%u resp_pos=%u "
						"cmd_pg0=%08x:%08x "
						"SEQ WR=%u RD=%u\n",
						dma_ctrl, cmd_pos, resp_pos,
						cmd_pg0_hi, cmd_pg0_lo,
						seq_w, seq_r);
				}

				/*
				 * Bus coefficients and clock set removed.
				 *
				 * Bus coefficients: tried ring buffer (cmd
				 * 0x001D0004, not consumed) and mixer settings
				 * (settings 0-3 with float + sub masks, caused
				 * SEQ deadlock on 12th write).  The correct
				 * encoding for SEL130 is unknown — need macOS
				 * DTrace BAR0 write capture.
				 *
				 * Clock set: writes 0xC074 + doorbell(4) but
				 * bit 22 ack never arrives (bit 21 reappears).
				 * Adds 2s timeout delay for no benefit.
				 * The firmware uses its default rate (48kHz).
				 */

				/*
				 * Transport start deferred to pcm_prepare.
				 *
				 * Starting transport here (during probe/connect)
				 * triggers an immediate PCIe AER error on some
				 * Thunderbolt controllers (kernel 6.18+, Maple
				 * Ridge), killing the link within ~50ms.
				 * The DSP service will start once pcm_prepare
				 * brings up transport on first audio open.
				 */
				dev_dbg(&ua->pdev->dev,
					"  transport deferred to pcm_prepare\n");

				return 0;
			}

			/*
			 * Other notification bits may be set — log them.
			 * The kext processes bits 0, 1, 4, 5, 22 as well.
			 */
			if (status != 0)
				dev_dbg(&ua->pdev->dev,
					"  connect poll: notif_status=0x%08x (no bit 21 yet)\n",
					status);

			msleep(UA_CONNECT_POLL_MS);
		}
	}

	dev_err(&ua->pdev->dev, "audio extension connect timeout\n");
	ua->aceface_failed = true;
	return -ETIMEDOUT;
}

/*
 * Hardware part of the soft disconnect.  Safe under ua->lock: the DSP
 * service handler takes ua->lock, so instead of cancel_delayed_work_sync()
 * this only marks the loop stopped and cancels a pending run.  A handler
 * already blocked on the lock runs once more and does not reschedule.
 */
static void ua_audio_disconnect_hw(struct ua_device *ua)
{
	struct ua_audio *audio = &ua->audio;
	u32 seq_wr, seq_rd;

	if (audio->dsp_service_running) {
		audio->dsp_service_running = false;
		cancel_delayed_work(&audio->dsp_service_work);
	}

	if (!audio->connected)
		return;

	/* Stop transport first — clear ALL control bits */
	ua_write(ua, UA_REG_AX_CONTROL, 0);
	audio->transport_running = false;

	/*
	 * Disable all interrupts.  Through the shared helper so the cached
	 * masks follow: the old direct IMR writes left them stale, and the
	 * next ua_enable_vector() silently re-enabled every cached vector.
	 */
	ua_irq_mask_all(ua, false);

	/* Give firmware time to stop DMA operations */
	msleep(50);

	/*
	 * If the mixer DSP is alive, skip the disconnect command.
	 * Sending UA_AX_DISCONNECT kills the mixer DSP readback,
	 * and reconnect alone can't restart it without a full
	 * firmware reload.  Transport and DMA are already stopped.
	 */
	seq_wr = ua_read(ua, UA_REG_MIXER_SEQ_WR);
	seq_rd = ua_read(ua, UA_REG_MIXER_SEQ_RD);
	if (seq_rd > 0 && seq_rd == seq_wr) {
		dev_dbg(&ua->pdev->dev,
			"audio extension soft-disconnect (preserving mixer DSP)\n");
	} else {
		ua_write(ua, UA_REG_AX_CONNECT, UA_AX_DISCONNECT);
		msleep(50);
		dev_dbg(&ua->pdev->dev, "audio extension disconnected\n");
	}

	audio->connected = false;
}

/* Caller must NOT hold ua->lock (waits for the DSP service handler) */
static void ua_audio_disconnect(struct ua_device *ua)
{
	/* Stop DSP service before tearing down connection */
	ua_dsp_service_stop(ua);
	ua_audio_disconnect_hw(ua);
}

/**
 * ua_audio_reconnect - Reset connection state and re-do ACEFACE handshake.
 * @ua: device
 *
 * Called from ioctl after userspace firmware load.  The driver's initial
 * probe-time connect attempt fails on cold boot (DSP not ready), so
 * userspace loads firmware and then triggers this to establish the
 * ACEFACE connection with the now-ready DSP.
 *
 * Caller must NOT hold ua->lock.  The connect itself runs under it, as
 * it does from pcm_prepare.
 */
int ua_audio_reconnect(struct ua_device *ua)
{
	struct ua_audio *audio = &ua->audio;
	int ret;

	mutex_lock(&ua->lock);

	/* Reset connection state so ua_audio_connect() will retry */
	audio->connected = false;
	audio->adopt_pending = true;

	ret = ua_audio_connect(ua);
	if (!ret)
		ua_dsp_service_start(ua);	/* periodic readback drain */

	mutex_unlock(&ua->lock);
	return ret;
}

/* ----------------------------------------------------------------
 * Transport control
 * ---------------------------------------------------------------- */

/*
 * The period timer of a transport about to start with a period of @period
 * frames: TIMER_CFG (0x2270) and timer_frames.  Caller holds ua->lock with
 * the transport stopped (ua_audio_prepare_transport()).
 *
 * period_irq=0 programs no timer (timer_frames = 0, the vector stays
 * masked).  That is the macOS state: PrepareTransport writes TIMER_CFG and
 * enables vector 0x46 only for a non-zero interval, and the CoreAudio
 * engine never sets one; it clocks off the end-of-buffer vector, as the
 * Windows driver does.  The driver used to write the period here and arm
 * the vector on every start although nothing took wakeups from it.
 *
 * period_irq=1 writes what the kext's one timer user writes, the interval
 * minus one (CUAD2AudioMixer::StartTransport → vtable +0xf0 =
 * SetPeriodicTimerIntervalInFrames(block - 1, 0), 12.0.0 x86 0xf958;
 * BUF_SIZE is frames - 1 too; see period_irq_cfg_adj), with an interval of
 * at least 1 ms: a shorter period gets a multiple of itself and its stream
 * stays on the hrtimer (ua_period_irq_usable()), so the interrupt rate
 * never exceeds the hrtimer's 1 kHz.
 */
static void ua_audio_set_timer(struct ua_device *ua, unsigned int period)
{
	struct ua_audio *audio = &ua->audio;
	unsigned int min = DIV_ROUND_UP(audio->sample_rate, 1000);
	unsigned int frames = period;
	int adj = READ_ONCE(period_irq_cfg_adj);
	u32 cfg;

	if (!READ_ONCE(period_irq)) {
		audio->timer_frames = 0;
		return;
	}

	/* Only -1, 0, +1 (clamp() trips old kernels' minmax checks) */
	adj = adj < 0 ? -1 : adj > 0 ? 1 : 0;
	if (frames < min)
		frames = roundup(min, period);
	cfg = (u32)((int)frames + adj);
	ua_write(ua, UA_REG_AX_TIMER_CFG, cfg);
	audio->timer_frames = frames;
	audio->timer_cfg = cfg;
}

/*
 * Prepare the transport engine.
 * Mirrors CPcieAudioExtension::PrepareTransport().
 *
 * @timer_frames: the ALSA period for the period timer (ua_audio_set_timer()
 * decides whether there is one), or 0 to keep the current setting.
 * The kext writes TIMER_CFG only here, i.e. only while the transport is
 * stopped (PrepareTransport, 12.0.0 x86 0x34ee8); rewriting it under a
 * running transport on every pcm_prepare is a suspect in issue #42.
 * Caller holds ua->lock.
 */
static int ua_audio_prepare_transport(struct ua_device *ua,
				      unsigned int timer_frames)
{
	struct ua_audio *audio = &ua->audio;
	unsigned int block_size;
	int retries;
	u32 frame_ctr, first_ctr, readback, ctrl;
	unsigned int gate_ms;

	/*
	 * io_map=1: re-read the firmware IO descriptors and commit the
	 * layout (play/rec_channels and buf_frame_size, written to AX_*_CH
	 * and BUF_SIZE below) from them.  Every layout change happens under
	 * ua->lock, held from here to the transport start.
	 */
	ua_io_map_prepare(ua);

	/* The kext clears its fault latches here (PrepareTransport) */
	audio->ax_fault_latch = 0;

	dev_dbg(&ua->pdev->dev, "=== PREPARE TRANSPORT ===\n");
	dev_dbg(&ua->pdev->dev, "  channels: play=%u rec=%u buf_frames=%u\n",
		audio->play_channels, audio->rec_channels,
		audio->buf_frame_size);

	/*
	 * Clear stale DMA reset strobes before configuring transport.
	 *
	 * On x8p (issue #46) DMA_CTRL reads 0x1ffff at stream time — the
	 * per-engine reset strobes [16:9] are re-asserted some time after
	 * the SG-programming enable write (observed post FW replay),
	 * holding the DMA engines in reset: transport arms and acks
	 * (CTRL readback 0x28f) but SAMPLE_POS/FRAME_CTR never advance.
	 * Re-apply the enable/clear mask here; no-op on devices where the
	 * strobes are already clear (x4 reads 0x1FF at this point).
	 */
	{
		u32 dma_ctrl = ua_read(ua, UA_REG_DMA_CTRL);
		u32 clean = ua->fw_v2 ?
			((dma_ctrl | 0x1FF) & ~0x1FE00) :
			((dma_ctrl | 0x1F) & ~0x1E00);

		if (dma_ctrl != clean) {
			ua_write(ua, UA_REG_DMA_CTRL, clean);
			dev_info_ratelimited(&ua->pdev->dev,
					     "DMA_CTRL stale 0x%08x -> 0x%08x (reset strobes cleared)\n",
					     dma_ctrl, ua_read(ua, UA_REG_DMA_CTRL));
		}
	}

	/*
	 * Exact register sequence from kext PrepareTransport() disassembly.
	 * Order matters — firmware state machine expects this sequence.
	 */

	/* Step 1: BUF_SIZE (0x2240) = frameSize - 1 */
	ua_write(ua, UA_REG_AX_BUF_SIZE, audio->buf_frame_size - 1);

	/* Step 2: DMA_BLKSZ (0x226C) = ((frameSize-1) * playCh) >> 10 */
	block_size = ((audio->buf_frame_size - 1) * audio->play_channels) >> 10;
	ua_write(ua, UA_REG_AX_DMA_BLKSZ, block_size);

	/* Step 3: Clear SAMPLE_POS (0x2244) = 0 */
	ua_write(ua, UA_REG_AX_SAMPLE_POS, 0);

	/*
	 * Steps 3b/4: TIMER_CFG (0x2270) and EnableVector(0x46,
	 * clearPending=1), as the kext: only with a timer interval, i.e.
	 * period_irq=1 (ua_audio_set_timer()).  A restart (timer_frames 0)
	 * keeps the session's choice.  Without a timer, mask a vector an
	 * earlier period_irq=1 session left armed.
	 */
	if (timer_frames)
		ua_audio_set_timer(ua, timer_frames);
	if (audio->timer_frames)
		ua_enable_vector(ua, UA_IRQ_VEC_PERIODIC, true);
	else if (ua->fw_v2 &&
		 (READ_ONCE(ua->irq_mask_hi) &
		  BIT(ua_vector_to_hwbit(UA_IRQ_VEC_PERIODIC) - 32)))
		ua_disable_vector(ua, UA_IRQ_VEC_PERIODIC);

	/* Step 5: Clear SAMPLE_POS again (kext does this twice) */
	ua_write(ua, UA_REG_AX_SAMPLE_POS, 0);

	/* Step 6: Fence read of SAMPLE_POS */
	readback = ua_read(ua, UA_REG_AX_SAMPLE_POS);

	/* Step 7: Clear AX_CONTROL (0x2248) = 0 */
	ua_write(ua, UA_REG_AX_CONTROL, 0);

	/*
	 * Step 8: POS_OFFSET (0x2258) — rate-dependent value.
	 * From kext PrepareTransport(): stored at this+0x2880,
	 * indexed by rate enum.  The value doubles per speed tier:
	 *   44.1/48 kHz   → 0x08
	 *   88.2/96 kHz   → 0x10
	 *   176.4/192 kHz → 0x20
	 * Previously hardcoded to 0x10 (wrong for 48kHz!).
	 */
	{
		u32 pos_offset;
		static const u32 win_cfg[3] = { 0x0B, 0x16, 0x2C };

		switch (audio->rate_index) {
		case UA_RATE_44100:
		case UA_RATE_48000:
			pos_offset = 0x08;
			break;
		case UA_RATE_88200:
		case UA_RATE_96000:
			pos_offset = 0x10;
			break;
		case UA_RATE_176400:
		case UA_RATE_192000:
			pos_offset = 0x20;
			break;
		default:
			pos_offset = 0x08;
			break;
		}
		/* Windows caches 0x0B/0x16/0x2C per band (parity/18 C01) */
		if (frame_cfg_win)
			pos_offset = win_cfg[ua_rate_idx_band(audio->rate_index)];
		audio->frame_cfg_last = pos_offset;
		ua_write(ua, UA_REG_AX_FRAME_CFG, pos_offset);
		dev_dbg(&ua->pdev->dev,
			"  POS_OFFSET=0x%02x (rate_index=%d)\n",
			pos_offset, audio->rate_index);
	}

	/*
	 * Step 9: PLAY_CH (0x2250) = playback channel count.
	 * Legacy: route-table count.  io_map=1: output descriptor count,
	 * +1 only when (UA_AX_DIAG_FLAGS & 6) (kext rule).
	 */
	ua_write(ua, UA_REG_AX_PLAY_CH, audio->play_channels);

	/*
	 * Step 10: REC_CH (0x225C) = record channel count.
	 * io_map=1: input descriptor count, +1 only when
	 * (UA_AX_DIAG_FLAGS & 4).
	 */
	ua_write(ua, UA_REG_AX_REC_CH, audio->rec_channels);

	/* Step 11: ARM — AX_CONTROL (0x2248) = 1 */
	ua_write(ua, UA_REG_AX_CONTROL, UA_AX_CTRL_DMA_EN);

	/* Step 12: Read fence — AX_STATUS (0x22C0) */
	readback = ua_read(ua, UA_REG_AX_STATUS);

	/*
	 * Step 13: P2P_ROUTE (0x224C) = 0x100 | (play_ch - 1)
	 * From kext: written AFTER DMA enable + status fence.
	 * Tells FPGA how to route audio between DSP and DMA.
	 * Previously written before DMA enable (wrong order).
	 *
	 * The kext only writes it when diagnostic flags bit 1 is set
	 * (PrepareTransport tests +0x28b8 & 2), i.e. never in a normal
	 * session.  io_map=1 follows the kext; the legacy layout keeps
	 * the write it has always done.
	 */
	if (!audio->io_map_active || (UA_AX_DIAG_FLAGS & 2))
		ua_write(ua, UA_REG_AX_P2P_ROUTE,
			 0x100 | (audio->play_channels - 1));

	ctrl = ua_read(ua, UA_REG_AX_CONTROL);
	dev_dbg(&ua->pdev->dev, "  armed: BLKSZ=%u CTRL=0x%x STATUS=0x%x\n",
		block_size, ctrl, readback);

	/*
	 * Step 14: Poll FRAME_CTR (0x2254) for firmware ack.
	 * Kext checks FRAME_CTR == pos_offset (rate-dependent): up to 3 reads
	 * with a 1 ms sleep before the 2nd and 3rd, a mismatch is not an
	 * error (parity/12 T06).  Linux has always taken the first non-zero
	 * value; frame_ctr_gate > 0 waits for the equality instead.
	 */
	ua_audio_check_ax_faults(ua, ctrl, "arm");
	frame_ctr = 0;
	first_ctr = 0;
	gate_ms = 0;
	if (frame_ctr_gate > 0) {
		frame_ctr = first_ctr = ua_read(ua, UA_REG_AX_FRAME_CTR);
		for (retries = 1; frame_ctr != audio->frame_cfg_last &&
		     retries <= frame_ctr_gate; retries++) {
			usleep_range(1000, 1500);
			frame_ctr = ua_read(ua, UA_REG_AX_FRAME_CTR);
			gate_ms++;
		}
	} else {
		for (retries = 0; retries < 3; retries++) {
			frame_ctr = ua_read(ua, UA_REG_AX_FRAME_CTR);
			if (!retries)
				first_ctr = frame_ctr;
			if (frame_ctr != 0)
				break;
			if (retries > 0)
				msleep(1);
		}
	}

	dev_dbg(&ua->pdev->dev, "  FRAME_CTR=0x%08x after %d polls\n",
		frame_ctr, retries + 1);

	{
		unsigned int band = ua_rate_idx_band(audio->rate_index);

		audio->frame_ctr_first[band] = first_ctr;
		audio->frame_ctr_final[band] = frame_ctr;
		audio->frame_ctr_ms[band] = gate_ms;
		audio->frame_ctr_seen[band] = true;
		dev_info_ratelimited(&ua->pdev->dev,
				     "FRAME_CFG 0x%02x armed (rate index %u): FRAME_CTR right after the arm 0x%x, final 0x%x%s (%u ms gate)\n",
				     audio->frame_cfg_last, audio->rate_index,
				     first_ctr, frame_ctr,
				     frame_ctr == audio->frame_cfg_last ?
				     " = FRAME_CFG" : " != FRAME_CFG", gate_ms);
	}

	if (frame_ctr == 0)
		dev_warn_ratelimited(&ua->pdev->dev,
				     "frame counter still zero!\n");

	/* Step 15: Enable end-of-buffer vector — EnableVector(0x47, 1) */
	ua_enable_vector(ua, UA_IRQ_VEC_END_BUFFER, true);

	dev_dbg(&ua->pdev->dev,
		"transport prepared: %u frames, %u/%u ch, blksz=%u\n",
		audio->buf_frame_size, audio->play_channels,
		audio->rec_channels, block_size);

	return 0;
}

/*
 * One-time PCIe setup.  Called from probe/connect, NOT from pcm_prepare.
 * Walking the Thunderbolt bridge chain takes milliseconds and must
 * not run on PipeWire's real-time audio thread.
 *
 * By default this only logs the link state: the kext never writes config
 * space.  With pcie_tuning=1 it disables ASPM on the Apollo's link through
 * the ASPM core (pci_disable_link_state(), endpoint only — no raw LNKCTL
 * writes behind the ASPM driver's back, no bridge LNKCTL writes), clears
 * DevSta errors and raises the completion timeout on the Apollo and every
 * upstream bridge, as the driver always did before.
 */
static void ua_pcie_setup(struct ua_device *ua)
{
	if (ua->pcie_setup_done)
		return;
	{
		int pos;
		u16 devctl, devsta, lnkctl, lnksta;
		struct pci_dev *bridge;

		pos = pci_find_capability(ua->pdev, PCI_CAP_ID_EXP);
		if (pos) {
			pci_read_config_word(ua->pdev, pos + PCI_EXP_DEVCTL, &devctl);
			pci_read_config_word(ua->pdev, pos + PCI_EXP_DEVSTA, &devsta);
			pci_read_config_word(ua->pdev, pos + PCI_EXP_LNKCTL, &lnkctl);
			pci_read_config_word(ua->pdev, pos + PCI_EXP_LNKSTA, &lnksta);
			dev_dbg(&ua->pdev->dev,
				"  PCIe cap at 0x%x: DevCtl=0x%04x DevSta=0x%04x LnkCtl=0x%04x LnkSta=0x%04x\n",
				pos, devctl, devsta, lnkctl, lnksta);
			dev_dbg(&ua->pdev->dev,
				"  PCIe: MPS=%u MRRS=%u Speed=Gen%u Width=x%u\n",
				128 << ((devctl >> 5) & 7),
				128 << ((devctl >> 12) & 7),
				lnksta & 0xF,
				(lnksta >> 4) & 0x3F);
		}

		/*
		 * L0s on the Apollo's own link.  The FPGA endpoint advertises
		 * an unlimited L0s exit latency, and with L0s enabled (the
		 * kernel's default ASPM policy) the Thunderbolt bridge above it
		 * logs a steady stream of correctable Replay Timer Timeouts
		 * while audio streams (x4 on kernel 7.2, 2026-10-09: ~15 in
		 * 4 minutes with L0s on, none with it off).  Turn L0s off
		 * through the ASPM core; this is not a raw config write.
		 */
		if (pos && (lnkctl & PCI_EXP_LNKCTL_ASPM_L0S)) {
			int err = pci_disable_link_state(ua->pdev,
							 PCIE_LINK_STATE_L0S);

			dev_info(&ua->pdev->dev,
				 "  ASPM L0s disabled on the Apollo link: %d\n",
				 err);
		}

		if (!pcie_tuning) {
			ua->pcie_setup_done = true;
			dev_info(&ua->pdev->dev,
				 "PCIe config otherwise left as enumerated (pcie_tuning=0)\n");
			return;
		}

		if (pos) {
			/* Disable ASPM on the Apollo's link via the ASPM core */
			if (lnkctl & PCI_EXP_LNKCTL_ASPMC) {
				int err = pci_disable_link_state(ua->pdev,
							PCIE_LINK_STATE_L0S |
							PCIE_LINK_STATE_L1);

				dev_info(&ua->pdev->dev,
					 "  Disabling device ASPM (was 0x%x): %d\n",
					 lnkctl & PCI_EXP_LNKCTL_ASPMC, err);
			}

			/* Clear any pending error status */
			if (devsta & 0xF) {
				dev_info(&ua->pdev->dev,
					 "  Clearing DevSta errors: 0x%x\n",
					 devsta & 0xF);
				pci_write_config_word(ua->pdev,
						     pos + PCI_EXP_DEVSTA,
						     devsta);
			}

			/* Increase device completion timeout to range D (65-210ms) */
			{
				u16 devctl2;

				pci_read_config_word(ua->pdev,
						     pos + PCI_EXP_DEVCTL2,
						     &devctl2);
				dev_dbg(&ua->pdev->dev,
					"  Device DevCtl2=0x%04x\n", devctl2);
				if ((devctl2 & 0xF) != 0x6) {
					devctl2 = (devctl2 & ~0xF) | 0x6;
					pci_write_config_word(ua->pdev,
							     pos + PCI_EXP_DEVCTL2,
							     devctl2);
					dev_info(&ua->pdev->dev,
						 "  -> Device completion timeout set to range D (65-210ms)\n");
				}
			}
		}

		/* Also increase completion timeout on the upstream bridge
		 * and all bridges up the chain (ASPM on the Apollo's link
		 * was handled by the ASPM core above) */
		bridge = ua->pdev->bus->self;
		while (bridge) {
			pos = pci_find_capability(bridge, PCI_CAP_ID_EXP);
			if (pos) {
				u16 devctl2;

				pci_read_config_word(bridge, pos + PCI_EXP_LNKCTL, &lnkctl);
				pci_read_config_word(bridge, pos + PCI_EXP_LNKSTA, &lnksta);
				pci_read_config_word(bridge, pos + PCI_EXP_DEVCTL2, &devctl2);
				dev_dbg(&ua->pdev->dev,
					"  Bridge %s: LnkCtl=0x%04x LnkSta=0x%04x DevCtl2=0x%04x\n",
					pci_name(bridge), lnkctl, lnksta, devctl2);

				/* Set completion timeout to range D
				 * (65-210ms) on upstream bridges */
				if ((devctl2 & 0xF) != 0x6) {
					devctl2 = (devctl2 & ~0xF) | 0x6;
					pci_write_config_word(bridge,
							     pos + PCI_EXP_DEVCTL2,
							     devctl2);
					dev_info(&ua->pdev->dev,
						 "  -> Completion timeout set to range D (65-210ms)\n");
				}
			}
			if (!bridge->bus || !bridge->bus->self)
				break;
			bridge = bridge->bus->self;
		}
	}
	ua->pcie_setup_done = true;
	dev_info(&ua->pdev->dev, "PCIe ASPM/timeout setup complete\n");
}

static int ua_audio_start_transport(struct ua_device *ua)
{
	struct ua_audio *audio = &ua->audio;
	u32 ctrl, readback, sample_pos, frame_ctr, status, dma_ctrl;

	if (audio->transport_running)
		return 0;

	/*
	 * AX_CONTROL start value: 0xF (DMA + playback + record + interrupt),
	 * or 0x20F with the extended-mode bit (a unit clocked from a peer).
	 * The kext's StartTransport writes 0x20F only for clock source 0x0A
	 * (and 0x09 without a notify callback) and 0xF otherwise, so 0xF is
	 * the default (titan, 2026-10-11: 28 of 28 ratecheck runs through
	 * every rate change; 0x20F, the old value, also passed 14 of 14).
	 * ax_control_ext=1 brings 0x20F back.
	 */
	{
		u32 fw_ver = ua_read(ua, UA_REG_FW_VERSION);
		dev_dbg(&ua->pdev->dev, "=== START TRANSPORT ===\n");
		dev_dbg(&ua->pdev->dev, "  FW_VERSION: 0x%08x, device_type: 0x%x\n",
			fw_ver, ua->device_type);

		ctrl = ax_control_ext ? UA_AX_CTRL_START_EXT : UA_AX_CTRL_START;
	}

	/*
	 * Clear stale pending interrupts right before transport start.
	 * Vectors are already enabled from ProgramRegisters (notification
	 * BIT(0)) and PrepareTransport (timer BIT(30), end-of-buffer BIT(31)).
	 */
	{
		u32 stale_lo = ua_read(ua, UA_REG_IRQ_STATUS);
		u32 stale_hi = 0;

		if (stale_lo)
			ua_write(ua, UA_REG_IRQ_STATUS, stale_lo);
		if (ua->fw_v2) {
			stale_hi = ua_read(ua, UA_REG_EXT_IRQ_STATUS);
			if (stale_hi)
				ua_write(ua, UA_REG_EXT_IRQ_STATUS, stale_hi);
		}
		dev_dbg(&ua->pdev->dev,
			"  cleared stale IRQ: lo=0x%08x hi=0x%08x\n",
			stale_lo, stale_hi);
	}

	dev_dbg(&ua->pdev->dev,
		"  IRQ mask: lo=0x%08x hi=0x%08x\n",
		ua->irq_mask_lo, ua->irq_mask_hi);

	/* Start transport */
	dev_dbg(&ua->pdev->dev, "  writing AX_CONTROL = 0x%03x\n", ctrl);
	ua_write(ua, UA_REG_AX_CONTROL, ctrl);

	/* Immediate readback to verify the write took */
	readback = ua_read(ua, UA_REG_AX_CONTROL);
	sample_pos = ua_read(ua, UA_REG_AX_SAMPLE_POS);
	frame_ctr = ua_read(ua, UA_REG_AX_FRAME_CTR);
	status = ua_read(ua, UA_REG_AX_STATUS);

	dev_dbg(&ua->pdev->dev,
		"  after start: CTRL=0x%08x SAMPLE_POS=0x%08x FRAME_CTR=0x%08x STATUS=0x%08x\n",
		readback, sample_pos, frame_ctr, status);

	/* Check if device went away (Thunderbolt disconnect) */
	if (readback == 0xFFFFFFFF) {
		dev_err(&ua->pdev->dev,
			"device unreachable after transport start!\n");
		audio->connected = false;
		audio->transport_running = false;
		return -ENODEV;
	}

	ua_audio_check_ax_faults(ua, readback, "start");

	/*
	 * A new transport session: streams the IRQ path served on the
	 * previous one are stale (ua_period_irq_stale()), and the period
	 * timer gets another chance after a fallback.
	 */
	WRITE_ONCE(audio->transport_gen, audio->transport_gen + 1);
	audio->irq_dead = false;
	audio->pstats.phase_sum = 0;
	audio->pstats.phase_n = 0;
	audio->pstats.phase_max = 0;
	audio->transport_running = true;
	ua_audio_stall_qos(ua, true);

	/* Delayed check — verify DMA is advancing */
	udelay(100);
	sample_pos = ua_read(ua, UA_REG_AX_SAMPLE_POS);
	frame_ctr = ua_read(ua, UA_REG_AX_FRAME_CTR);
	dev_dbg(&ua->pdev->dev,
		"  after 100us: SAMPLE_POS=0x%08x FRAME_CTR=0x%08x\n",
		sample_pos, frame_ctr);
	dma_ctrl = ua_read(ua, UA_REG_DMA_CTRL);
	dev_dbg(&ua->pdev->dev,
		"  DMA_CTRL=0x%08x IRQ_LO=0x%08x IRQ_HI=0x%08x\n",
		dma_ctrl, ua->irq_mask_lo, ua->irq_mask_hi);

	return 0;
}

static void ua_audio_stop_transport(struct ua_device *ua)
{
	struct ua_audio *audio = &ua->audio;
	u32 ctrl, sample_pos;

	ua_audio_stall_qos(ua, false);
	if (!audio->transport_running)
		return;

	dev_dbg(&ua->pdev->dev,
		"=== STOP TRANSPORT === connected=%d aceface=%d\n",
		audio->connected, ua->aceface_done);

	/* Disable end-of-buffer interrupt */
	ua_disable_vector(ua, UA_IRQ_VEC_END_BUFFER);

	/* Stop: clear all control bits */
	ua_write(ua, UA_REG_AX_CONTROL, 0);
	audio->transport_running = false;

	ctrl = ua_read(ua, UA_REG_AX_CONTROL);
	sample_pos = ua_read(ua, UA_REG_AX_SAMPLE_POS);
	dev_dbg(&ua->pdev->dev, "transport stopped: AX_CTRL=0x%08x SP=0x%08x\n",
		ctrl, sample_pos);
}

/**
 * ua_audio_dma_test - Raw DMA buffer capture test (bypasses ALSA)
 * @ua: device (caller must hold ua->lock)
 * @dt: test parameters and results
 *
 * Fills rec_buf with canary, starts transport properly, waits,
 * scans buffer for non-canary/non-zero data, returns results.
 */
int ua_audio_dma_test(struct ua_device *ua, struct ua_dma_test *dt)
{
	struct ua_audio *audio = &ua->audio;
	struct device *dev = &ua->pdev->dev;
	u32 *scan;
	unsigned int i;
	int ret;

	if (!audio->rec_buf || !audio->connected)
		return -ENODEV;

	/*
	 * It reprograms and finally stops the transport and overwrites the
	 * record ring: never under open streams.  ua->lock is dropped during
	 * the wait below, so dma_test_active keeps pcm_prepare off until it
	 * is done.
	 */
	if (audio->play_sub || audio->rec_sub || audio->transport_running)
		return -EBUSY;
	audio->dma_test_active = true;

	if (dt->delay_ms < 50)
		dt->delay_ms = 50;
	if (dt->delay_ms > 2000)
		dt->delay_ms = 2000;

	/* Set DMA_CTRL if requested */
	if (dt->dma_ctrl) {
		ua_write(ua, UA_REG_DMA_CTRL, dt->dma_ctrl);
		dev_info(&ua->pdev->dev,
			 "dma_test: set DMA_CTRL=0x%08x (readback=0x%08x)\n",
			 dt->dma_ctrl,
			 ua_read(ua, UA_REG_DMA_CTRL));
	}

	/*
	 * Fill rec_buf with canary and hand it to the device.  dma_sem is
	 * held only around the buffer accesses, never across the sleep
	 * below, so a concurrent ua_audio_fini() is delayed by at most one
	 * pass over the ring.
	 */
	down_read(&audio->dma_sem);
	if (!audio->rec_buf) {
		up_read(&audio->dma_sem);
		ret = -ENODEV;
		goto out;
	}
	memset(audio->rec_buf, 0xDE, UA_DMA_BUF_SIZE);
	ua_dma_ring_sync(dev, &audio->rec_ring, 0, UA_DMA_BUF_SIZE, true);
	up_read(&audio->dma_sem);
	wmb();

	/* Start transport via proper prepare+start sequence */
	ret = ua_audio_prepare_transport(ua, 0);
	if (ret)
		goto out;
	ret = ua_audio_start_transport(ua);
	if (ret)
		goto out;

	/* Clock write if requested */
	if (dt->flags & 1) {
		audio->clock_source = 0xC;
		ua_write(ua, UA_REG_NOTIF_CLOCK_CFG,
			 (audio->rate_index << 8) | audio->clock_source);
		ua_write(ua, UA_REG_AX_CONNECT, UA_AX_CLOCK_DOORBELL);
	}

	/* Release lock during sleep so DSP service can run */
	mutex_unlock(&ua->lock);
	msleep(dt->delay_ms);
	mutex_lock(&ua->lock);

	/* Read results */
	dt->sample_pos = ua_read(ua, UA_REG_AX_SAMPLE_POS);

	/* Take the ring back from the device and scan it */
	down_read(&audio->dma_sem);
	if (atomic_read(&ua->shutdown) || !audio->rec_buf) {
		up_read(&audio->dma_sem);
		ret = -ENODEV;
		goto out;
	}
	ua_dma_ring_sync(dev, &audio->rec_ring, 0, UA_DMA_BUF_SIZE, false);

	scan = (u32 *)audio->rec_buf;
	dt->nonzero = 0;
	dt->non_zero_non_canary = 0;
	for (i = 0; i < UA_DMA_BUF_SIZE / 4; i++) {
		if (scan[i] != 0xDEDEDEDE)
			dt->nonzero++;
		if (scan[i] != 0 && scan[i] != 0xDEDEDEDE)
			dt->non_zero_non_canary++;
	}

	/* Copy first 1024 bytes */
	memcpy(dt->data, audio->rec_buf, sizeof(dt->data));
	up_read(&audio->dma_sem);

	dev_info(&ua->pdev->dev,
		 "dma_test: SP=%u canary_gone=%u audio=%u DMA_CTRL=0x%08x\n",
		 dt->sample_pos, dt->nonzero,
		 dt->non_zero_non_canary,
		 ua_read(ua, UA_REG_DMA_CTRL));
	ret = 0;
out:
	/* Stop the transport it started, on every path */
	ua_write(ua, UA_REG_AX_CONTROL, 0);
	audio->transport_running = false;
	audio->dma_test_active = false;
	return ret;
}

/* ----------------------------------------------------------------
 * Clock control
 *
 * From kext CPcieAudioExtension::_setSampleClock() disassembly:
 *   clockConfig = clockSource | (rateIndex << 8)
 *   Write to BAR0 + bank*4 + 0xC04C (= 0xC074 for Apollo x4)
 *   Doorbell: write 4 to BAR0 + 0x2260
 *   Wait up to 2000ms for response
 *
 * CRITICAL FIX (2026-02-28): Previous code wrote to 0xC04C (no bank
 * shift) and polled 0x22C0 (transport status, NOT notification status).
 * ---------------------------------------------------------------- */

static int ua_rate_to_index(unsigned int rate)
{
	switch (rate) {
	case 44100:  return UA_RATE_44100;
	case 48000:  return UA_RATE_48000;
	case 88200:  return UA_RATE_88200;
	case 96000:  return UA_RATE_96000;
	case 176400: return UA_RATE_176400;
	case 192000: return UA_RATE_192000;
	default:     return -EINVAL;
	}
}

/*
 * Notification bits taken as a clock-change acknowledgement.  The kext's
 * ClockChangeEvent is signalled by bit 5 alone (_handleNotificationInterrupt);
 * Linux has always also accepted bit 22 (rate-change notification) and
 * bit 4 (transport/clock change), and hardware has been seen acking with
 * those, so all three count.
 */
#define UA_CLOCK_ACK_BITS \
	(UA_NOTIF_BIT_EVENT | UA_NOTIF_BIT_SRATE | UA_NOTIF_BIT_TRANSPORT)

/* The bits that count as the ack: all three, or bit 5 only (clock_ack_bit5) */
static u32 ua_clock_ack_mask(void)
{
	return clock_ack_bit5 ? UA_NOTIF_BIT_EVENT : UA_CLOCK_ACK_BITS;
}

/*
 * One clock-change attempt, as CPcieAudioExtension::_setSampleClock
 * (kext 12.0.0 x86 0x35e3e): write the config, reset the clock-change
 * event, ring doorbell 4, wait up to 2 s.  "Reset the event" clears only
 * the ack bits in 0xC030 (a plain read/write register, not W1C), so a
 * stale ack cannot satisfy the wait while IO-descriptor bits 0/1 stay for
 * the service loop.  Caller holds ua->lock, which keeps the service loop
 * from clearing the ack before we see it.
 */
static int ua_audio_clock_change_locked(struct ua_device *ua,
					u32 clock_config)
{
	unsigned int waited;
	u32 status;

	/*
	 * Write to bank-shifted clock config register.
	 * 0xC074 = BAR0 + 0x28 + 0xC04C (bank_shift=0x0A).
	 */
	ua_write(ua, UA_REG_NOTIF_CLOCK_CFG, clock_config);
	status = ua_read(ua, UA_REG_NOTIF_STATUS);
	if (status == 0xFFFFFFFF)
		return -ENODEV;
	if (status & ua_clock_ack_mask())
		ua_write(ua, UA_REG_NOTIF_STATUS, status & ~ua_clock_ack_mask());

	/* Ring the clock doorbell */
	ua_write(ua, UA_REG_AX_CONNECT, UA_AX_CLOCK_DOORBELL);

	/* 0xC030 (bank-shifted), NOT 0x22C0 (transport status) */
	for (waited = 0; waited < UA_CLOCK_TIMEOUT_MS; waited += 10) {
		status = ua_read(ua, UA_REG_NOTIF_STATUS);
		if (status == 0xFFFFFFFF)
			return -ENODEV;
		if (status & ua_clock_ack_mask()) {
			struct ua_audio *audio = &ua->audio;

			/* Which bit acked: 5 is the kext's, 22 and 4 are Linux's extras */
			if (status & UA_NOTIF_BIT_EVENT)
				audio->clock_ack_count[0]++;
			if (status & UA_NOTIF_BIT_SRATE)
				audio->clock_ack_count[1]++;
			if (status & UA_NOTIF_BIT_TRANSPORT)
				audio->clock_ack_count[2]++;
			audio->clock_ack_last = status;
			dev_info(&ua->pdev->dev,
				 "  clock ack: notif_status=0x%08x (bit5=%u bit22=%u bit4=%u) after %u ms\n",
				 status,
				 !!(status & UA_NOTIF_BIT_EVENT),
				 !!(status & UA_NOTIF_BIT_SRATE),
				 !!(status & UA_NOTIF_BIT_TRANSPORT), waited);
			return 0;
		}
		msleep(10);
	}

	/* Kext: FPGA_REV == ~0 means offline, anything else a timeout */
	if (ua_read(ua, UA_REG_FPGA_REV) == 0xFFFFFFFF)
		return -ENODEV;

	dev_warn(&ua->pdev->dev,
		 "  clock change not acknowledged in %u ms (notif_status=0x%08x)\n",
		 UA_CLOCK_TIMEOUT_MS, status);
	return -ETIMEDOUT;
}

/* A clock source the device can run on (values: UA_CLOCK_*) */
bool ua_clock_source_valid(const struct ua_device *ua, unsigned int source)
{
	const struct ua_audio *audio = &ua->audio;

	/* clock_src_from_device: exactly the sources in the device's menu */
	if (ua_clock_list_active(audio)) {
		unsigned int i;
		bool found = false;

		for (i = 0; i < READ_ONCE(audio->clock_list_n); i++)
			if (audio->clock_list_val[i] == source)
				found = true;
		return found;
	}
	if (source != UA_CLOCK_INTERNAL && source != UA_CLOCK_SPDIF &&
	    source != UA_CLOCK_ADAT && source != UA_CLOCK_WORDCLOCK)
		return false;
	/* No list read yet (not connected): let the device decide */
	return !audio->clock_src_mask || (audio->clock_src_mask & BIT(source));
}

/*
 * Playback and capture share one clock.  A rate other than the one a
 * stream's hw_params fixed would leave that stream running at the wrong
 * speed (and, with io_map, on a layout it wasn't opened with).
 */
static bool ua_rate_busy_locked(const struct ua_audio *audio,
				unsigned int rate)
{
	unsigned int dir;

	for (dir = 0; dir < ARRAY_SIZE(audio->stream_rate); dir++)
		if (audio->stream_rate[dir] && audio->stream_rate[dir] != rate)
			return true;
	return false;
}

/*
 * Mirrors CPcieAudioExtension::SetSampleClock (x86 0x35cfc) where it
 * matters: retry the change once on timeout and report the error.
 * rate 0 keeps the current rate, source < 0 the current source.
 *
 * Not connected yet (no ACEFACE handshake, so the notification block is
 * not configured and nothing could ack): store rate and source and
 * return 0; the connect/prepare path applies them — prepare_transport()
 * programs the rate-dependent POS_OFFSET and the post-start clock write
 * sends rate_index/source.
 *
 * Connected: commit rate and source only when the device acknowledged
 * (any of UA_CLOCK_ACK_BITS, the bits the old code accepted).  A change
 * that got no acknowledgement in two 2 s attempts returns -ETIMEDOUT and
 * keeps the last acknowledged values.  A rate an open stream doesn't
 * run at returns -EBUSY.
 *
 * Caller must NOT hold ua->lock.
 */
static int ua_audio_set_clock_cfg(struct ua_device *ua, unsigned int rate,
				  int source)
{
	struct ua_audio *audio = &ua->audio;
	int rate_idx;
	u32 clock_config;
	int ret;

	mutex_lock(&ua->lock);

	if (!rate)
		rate = audio->sample_rate;
	if (source < 0)
		source = audio->clock_source;
	rate_idx = ua_rate_to_index(rate);
	if (rate_idx < 0) {
		ret = rate_idx;
		goto out;
	}
	if (atomic_read(&ua->shutdown)) {
		ret = -ENODEV;
		goto out;
	}
	if (ua_rate_busy_locked(audio, rate)) {
		dev_info_ratelimited(&ua->pdev->dev,
				     "clock: %u Hz refused, a stream is open at %u Hz\n",
				     rate, audio->sample_rate);
		ret = -EBUSY;
		goto out;
	}
	if (!ua->aceface_done) {
		dev_dbg(&ua->pdev->dev,
			"clock: %u Hz (source 0x%x) stored, applied at connect\n",
			rate, source);
		audio->sample_rate = rate;
		audio->rate_index = rate_idx;
		audio->clock_source = source;
		ret = 0;
		goto out;
	}

	/* clockConfig = clockSource | (rateIndex << 8) */
	clock_config = source | (rate_idx << 8);

	dev_info(&ua->pdev->dev, "clock: %u Hz, index %d, config 0x%08x\n",
		 rate, rate_idx, clock_config);

	ua_pause_set(ua, UA_PAUSE_RATE);
	ret = ua_audio_clock_change_locked(ua, clock_config);
	if (ret == -ETIMEDOUT) {
		dev_info(&ua->pdev->dev, "  retrying clock change once\n");
		ret = ua_audio_clock_change_locked(ua, clock_config);
	}

	if (!ret) {
		bool rate_new = audio->rate_index != (unsigned int)rate_idx;
		bool src_new = audio->clock_source != source;

		audio->sample_rate = rate;
		audio->rate_index = rate_idx;
		audio->clock_source = source;
		if (rate_new)
			ua_event_post(ua, UA_EVT_RATE_CHANGED, rate_idx);
		if (src_new)
			ua_event_post(ua, UA_EVT_CLOCK_CHANGED, source);
		/*
		 * io_map=1: the rate band and the firmware's descriptors may
		 * have changed with the rate (re-read, then commit or force
		 * a re-prepare).
		 */
		ua_io_map_rate_changed(ua);
		if (rate_new)
			ua_audio_rate_ack_real(ua, rate, "clock change");
		ua_audio_read_fw_latency(ua, "clock change");
	} else {
		dev_err(&ua->pdev->dev,
			"clock change to %u Hz (config 0x%08x) failed: %d\n",
			rate, clock_config, ret);
	}
out:
	ua_pause_clear(ua, UA_PAUSE_RATE);
	mutex_unlock(&ua->lock);
	return ret;
}

int ua_audio_set_clock(struct ua_device *ua, unsigned int rate)
{
	if (ua_rate_to_index(rate) < 0)
		return -EINVAL;
	return ua_audio_set_clock_cfg(ua, rate, -1);
}

int ua_audio_set_clock_source(struct ua_device *ua, unsigned int source)
{
	if (!ua_clock_source_valid(ua, source))
		return -EINVAL;
	return ua_audio_set_clock_cfg(ua, 0, source);
}

/* ----------------------------------------------------------------
 * ALSA PCM implementation
 * ---------------------------------------------------------------- */

static const struct snd_pcm_hardware ua_pcm_hw = {
	.info = SNDRV_PCM_INFO_INTERLEAVED |
		SNDRV_PCM_INFO_BLOCK_TRANSFER,
	.formats = SNDRV_PCM_FMTBIT_S32_LE,
	.rates = SNDRV_PCM_RATE_44100 | SNDRV_PCM_RATE_48000 |
		 SNDRV_PCM_RATE_88200 | SNDRV_PCM_RATE_96000 |
		 SNDRV_PCM_RATE_176400 | SNDRV_PCM_RATE_192000,
	.rate_min = 44100,
	.rate_max = 192000,
	.channels_min = 2,
	.channels_max = UA_MAX_CHANNELS,
	.buffer_bytes_max = UA_DMA_BUF_SIZE,
	.period_bytes_min = 256,
	.period_bytes_max = UA_DMA_BUF_SIZE / 2,
	.periods_min = 2,
	.periods_max = 64,
};

/* Offer only the rate the other direction's hw_params fixed (one clock) */
static int ua_pcm_rate_rule(struct snd_pcm_hw_params *params,
			    struct snd_pcm_hw_rule *rule)
{
	struct snd_pcm_substream *sub = rule->private;
	struct ua_device *ua = snd_pcm_substream_chip(sub);
	unsigned int other = sub->stream == SNDRV_PCM_STREAM_PLAYBACK ?
			     UA_DIR_REC : UA_DIR_PLAY;
	unsigned int rate = READ_ONCE(ua->audio.stream_rate[other]);
	struct snd_interval fixed = { .min = rate, .max = rate, .integer = 1 };

	if (!rate)
		return 0;
	return snd_interval_refine(hw_param_interval(params,
						     SNDRV_PCM_HW_PARAM_RATE),
				   &fixed);
}

static int ua_pcm_open(struct snd_pcm_substream *sub)
{
	struct ua_device *ua = snd_pcm_substream_chip(sub);
	struct ua_audio *audio = &ua->audio;
	struct snd_pcm_runtime *runtime = sub->runtime;
	unsigned long flags;
	unsigned int ch, frames;

	if (atomic_read(&ua->shutdown))
		return -ENODEV;

	dev_dbg(&ua->pdev->dev,
		"pcm_open: stream=%s connected=%d transport=%d\n",
		sub->stream == SNDRV_PCM_STREAM_PLAYBACK ? "play" : "rec",
		audio->connected, audio->transport_running);

	runtime->hw = ua_pcm_hw;

	/*
	 * ALSA channels come from the channel map: the DMA channel count in
	 * the legacy layout, the route channels found in the firmware IO
	 * descriptor with io_map=1.
	 */
	ch = ua_io_map_open_channels(ua,
				     sub->stream == SNDRV_PCM_STREAM_PLAYBACK ?
				     UA_DIR_PLAY : UA_DIR_REC, &frames);
	runtime->hw.channels_min = ch;
	runtime->hw.channels_max = ch;

	/*
	 * Fix ALSA buffer_size to the hardware ring size (buf_frame_size,
	 * typically 8192). The hardware DMA reads from a fixed-size ring
	 * buffer — the copy callback writes directly into it, and the
	 * pointer callback reports SAMPLE_POS within it. If ALSA's
	 * buffer_size is smaller, the copy callback only fills a portion
	 * of the ring and the hardware plays stale data from the rest.
	 *
	 * Period size remains configurable (e.g. 512 for low latency).
	 * With buffer=8192 and period=512, PipeWire gets 16 periods of
	 * headroom at the same 10.7ms latency.
	 */
	snd_pcm_hw_constraint_minmax(runtime, SNDRV_PCM_HW_PARAM_BUFFER_SIZE,
				     frames, frames);

	/*
	 * Playback and capture share one clock: once one direction has a
	 * rate, the other is offered only that one, so the two can't
	 * re-prepare each other's rate in a loop (PipeWire with the nodes
	 * ungrouped did that ~5 times a second, and the DSP ended up
	 * recording silence until a power cycle).
	 */
	snd_pcm_hw_rule_add(runtime, 0, SNDRV_PCM_HW_PARAM_RATE,
			    ua_pcm_rate_rule, sub, SNDRV_PCM_HW_PARAM_RATE, -1);

	spin_lock_irqsave(&audio->lock, flags);
	if (sub->stream == SNDRV_PCM_STREAM_PLAYBACK)
		audio->play_sub = sub;
	else
		audio->rec_sub = sub;
	spin_unlock_irqrestore(&audio->lock, flags);

	return 0;
}

static int ua_pcm_close(struct snd_pcm_substream *sub)
{
	struct ua_device *ua = snd_pcm_substream_chip(sub);
	struct ua_audio *audio = &ua->audio;
	unsigned long flags;
	bool both_closed;

	dev_dbg(&ua->pdev->dev,
		"pcm_close: stream=%s play_sub=%p rec_sub=%p transport=%d\n",
		sub->stream == SNDRV_PCM_STREAM_PLAYBACK ? "play" : "rec",
		audio->play_sub, audio->rec_sub, audio->transport_running);

	/*
	 * Clear substream pointer under lock — the hrtimer and audio
	 * IRQ handler snapshot these under the same lock before use.
	 */
	spin_lock_irqsave(&audio->lock, flags);
	if (sub->stream == SNDRV_PCM_STREAM_PLAYBACK)
		audio->play_sub = NULL;
	else
		audio->rec_sub = NULL;
	both_closed = !audio->play_sub && !audio->rec_sub;
	spin_unlock_irqrestore(&audio->lock, flags);

	/*
	 * Stop transport when both substreams are closed.  Under ua->lock
	 * so it cannot interleave with a pcm_prepare of a substream opened
	 * meanwhile; re-check under the lock for that case.
	 */
	if (both_closed && audio->transport_running) {
		mutex_lock(&ua->lock);
		spin_lock_irqsave(&audio->lock, flags);
		both_closed = !audio->play_sub && !audio->rec_sub;
		spin_unlock_irqrestore(&audio->lock, flags);
		if (both_closed) {
			dev_dbg(&ua->pdev->dev,
				"pcm_close: both subs closed, stopping transport\n");
			ua_audio_stop_transport(ua);
		}
		mutex_unlock(&ua->lock);
	}

	return 0;
}

static int ua_pcm_hw_params(struct snd_pcm_substream *sub,
			    struct snd_pcm_hw_params *hw)
{
	struct ua_device *ua = snd_pcm_substream_chip(sub);
	struct ua_audio *audio = &ua->audio;
	unsigned int dir = sub->stream == SNDRV_PCM_STREAM_PLAYBACK ?
			   UA_DIR_PLAY : UA_DIR_REC;
	unsigned int other = dir == UA_DIR_PLAY ? UA_DIR_REC : UA_DIR_PLAY;
	unsigned int rate = params_rate(hw);
	int ret = 0;

	/* DMA buffers are device-lifetime, pre-allocated.
	 * Data transfer is handled by the copy callback which
	 * scatters user channels into the full-width DMA buffer.
	 *
	 * Record the rate (the other direction may have set one since
	 * the rate rule ran). */
	mutex_lock(&ua->lock);
	if (audio->stream_rate[other] && audio->stream_rate[other] != rate)
		ret = -EBUSY;
	else
		audio->stream_rate[dir] = rate;
	mutex_unlock(&ua->lock);
	return ret;
}

static int ua_pcm_hw_free(struct snd_pcm_substream *sub)
{
	struct ua_device *ua = snd_pcm_substream_chip(sub);

	mutex_lock(&ua->lock);
	ua->audio.stream_rate[sub->stream == SNDRV_PCM_STREAM_PLAYBACK ?
			      UA_DIR_PLAY : UA_DIR_REC] = 0;
	mutex_unlock(&ua->lock);
	return 0;
}

/*
 * io_map=1 copy: move ALSA channel c to/from DMA frame slot map->slot[c]
 * (kext ReadAudioInputData / WriteAudioOutputData).  Playback zeroes the
 * whole hardware frame first so slots without an ALSA channel stay
 * silent; capture returns silence for ALSA channels beyond the map
 * (a stream opened before a layout change).  Caller holds dma_sem (read).
 */
static int ua_pcm_copy_mapped(struct ua_audio *audio,
			      struct snd_pcm_runtime *rt, bool playback,
			      char *dma_buf, unsigned int hw_frame_sz,
			      unsigned long start_frame, unsigned long frames,
			      struct iov_iter *iter)
{
	const struct ua_chan_map *map =
		&audio->chan_map[playback ? UA_DIR_PLAY : UA_DIR_REC];
	unsigned int hw_ch = hw_frame_sz / UA_SAMPLE_BYTES;
	unsigned int user_ch = rt->channels;
	size_t user_frame_sz = (size_t)user_ch * UA_SAMPLE_BYTES;
	unsigned int n = min(user_ch, map->channels);
	u32 tmp[UA_MAX_CHANNELS];
	unsigned long i;
	unsigned int c;

	if (user_ch > UA_MAX_CHANNELS || hw_ch > UA_MAX_CHANNELS)
		return -EINVAL;

	/* Every slot must lie inside the hardware frame */
	for (c = 0; c < n; c++)
		if (map->slot[c] != UA_CHAN_NO_SLOT && map->slot[c] >= hw_ch)
			return -EINVAL;

	for (i = 0; i < frames; i++) {
		unsigned long f = (start_frame + i) % audio->buf_frame_size;
		u32 *hw = (u32 *)(dma_buf + f * hw_frame_sz);

		if (playback) {
			if (copy_from_iter(tmp, user_frame_sz, iter) !=
			    user_frame_sz)
				return -EFAULT;
			memset(hw, 0, hw_frame_sz);
			for (c = 0; c < n; c++)
				if (map->slot[c] != UA_CHAN_NO_SLOT)
					hw[map->slot[c]] = tmp[c];
		} else {
			for (c = 0; c < n; c++)
				tmp[c] = map->slot[c] == UA_CHAN_NO_SLOT ?
					 0 : hw[map->slot[c]];
			for (; c < user_ch; c++)
				tmp[c] = 0;
			if (copy_to_iter(tmp, user_frame_sz, iter) !=
			    user_frame_sz)
				return -EFAULT;
		}
	}
	return 0;
}

/*
 * Copy callback — scatter user's N-channel data into the hardware's
 * full-width (24ch play / 22ch rec) interleaved DMA buffer.
 *
 * ALSA calls this instead of memcpy when MMAP is not advertised.
 * hwoff is a byte offset in ALSA's view (user_channels * 4 * frame).
 * We map each ALSA frame to the corresponding hardware frame in DMA,
 * writing only the first N channels.
 */
static int ua_pcm_copy(struct snd_pcm_substream *sub, int channel,
		       unsigned long hwoff, struct iov_iter *iter,
		       unsigned long bytes)
{
	struct ua_device *ua = snd_pcm_substream_chip(sub);
	struct ua_audio *audio = &ua->audio;
	struct device *dev = &ua->pdev->dev;
	struct snd_pcm_runtime *rt = sub->runtime;
	unsigned int user_frame_sz = frames_to_bytes(rt, 1);
	bool playback = sub->stream == SNDRV_PCM_STREAM_PLAYBACK;
	struct ua_dma_ring *ring;
	unsigned int hw_ch, hw_frame_sz, copy_sz;
	char *dma_buf;
	unsigned long frames, start_frame, i;
	int ret = 0;

	if (atomic_read(&ua->shutdown))
		return -ENODEV;

	/*
	 * Keep the flat views mapped for the whole transfer.
	 * snd_card_disconnect() does not drain a write()/read() that is
	 * already inside this callback, so ua_audio_fini() takes the write
	 * side of dma_sem before it vunmaps the rings.
	 */
	down_read(&audio->dma_sem);

	if (playback) {
		hw_ch = audio->play_channels;
		dma_buf = audio->play_buf;
		ring = &audio->play_ring;
	} else {
		hw_ch = audio->rec_channels;
		dma_buf = audio->rec_buf;
		ring = &audio->rec_ring;
	}
	if (!dma_buf) {
		ret = -ENODEV;
		goto out;
	}
	hw_frame_sz = hw_ch * UA_SAMPLE_BYTES;

	/*
	 * Legacy linear copy: ALSA channel N is slot N.  A stream opened
	 * under another layout (io_map=1 before the first valid descriptor)
	 * can be wider or narrower than the hardware frame: move the
	 * channels both have, drop the extra playback channels and return
	 * silence for the extra capture ones, never touching memory past a
	 * hardware frame.
	 */
	copy_sz = min(user_frame_sz, hw_frame_sz);

	frames = bytes / user_frame_sz;
	start_frame = hwoff / user_frame_sz;

	if (playback) {
		if (audio->io_map_active) {
			ret = ua_pcm_copy_mapped(audio, rt, true, dma_buf,
						 hw_frame_sz, start_frame,
						 frames, iter);
			if (ret)
				goto out;
		} else {
			for (i = 0; i < frames; i++) {
				unsigned long f = (start_frame + i) %
						  audio->buf_frame_size;
				unsigned long dma_off = f * hw_frame_sz;

				/* Zero unused channels (stale data/noise) */
				if (hw_frame_sz > copy_sz)
					memset(dma_buf + dma_off, 0,
					       hw_frame_sz);

				if (copy_from_iter(dma_buf + dma_off,
						   copy_sz, iter) != copy_sz) {
					ret = -EFAULT;
					goto out;
				}
				if (user_frame_sz > copy_sz)
					iov_iter_advance(iter,
							 user_frame_sz - copy_sz);
			}
		}

		/* The CPU wrote these frames — hand them to the device */
		ua_dma_ring_sync_frames(dev, ring, start_frame, frames,
					hw_frame_sz, audio->buf_frame_size,
					true);
	} else {
		/* The device wrote these frames — take them back first */
		ua_dma_ring_sync_frames(dev, ring, start_frame, frames,
					hw_frame_sz, audio->buf_frame_size,
					false);

		if (audio->io_map_active) {
			ret = ua_pcm_copy_mapped(audio, rt, false, dma_buf,
						 hw_frame_sz, start_frame,
						 frames, iter);
			goto out;
		}

		for (i = 0; i < frames; i++) {
			unsigned long f = (start_frame + i) % audio->buf_frame_size;
			unsigned long dma_off = f * hw_frame_sz;

			/* One-shot debug: dump multiple frames' raw DMA bytes */
			if (i == 0 && !audio->rec_debug_done) {
				u32 *p = (u32 *)(dma_buf + dma_off);
				u32 sp = ua_read(ua, UA_REG_AX_SAMPLE_POS);
				unsigned int j, canary_count = 0;
				u32 *scan = (u32 *)dma_buf;

				dev_dbg(&ua->pdev->dev,
					"REC DMA dump: frame=%lu off=0x%lx pos=%u "
					"[0]=%08x [1]=%08x [2]=%08x [3]=%08x\n",
					f, dma_off, sp,
					p[0], p[1], p[2], p[3]);

				/* Whole-ring scan — take all of it back first */
				ua_dma_ring_sync(dev, ring, 0, UA_DMA_BUF_SIZE,
						 false);

				/* Check how much canary pattern remains */
				for (j = 0; j < UA_DMA_BUF_SIZE / 4; j++) {
					if (scan[j] == 0xDEDEDEDE)
						canary_count++;
				}
				dev_dbg(&ua->pdev->dev,
					"REC canary check: %u/%u dwords still 0xDEDEDEDE "
					"(%u%% untouched)\n",
					canary_count,
					UA_DMA_BUF_SIZE / 4,
					canary_count * 100 / (UA_DMA_BUF_SIZE / 4));

				/* Dump a few samples at different offsets */
				for (j = 0; j < 8; j++) {
					unsigned long off = j * 8192;

					if (off + 16 <= UA_DMA_BUF_SIZE)
						dev_dbg(&ua->pdev->dev,
							"  @0x%05lx: %08x %08x %08x %08x\n",
							off,
							scan[off/4], scan[off/4+1],
							scan[off/4+2], scan[off/4+3]);
				}
				audio->rec_debug_done = true;
			}

			if (copy_to_iter(dma_buf + dma_off, copy_sz, iter) !=
			    copy_sz) {
				ret = -EFAULT;
				goto out;
			}
			if (user_frame_sz > copy_sz &&
			    iov_iter_zero(user_frame_sz - copy_sz, iter) !=
			    user_frame_sz - copy_sz) {
				ret = -EFAULT;
				goto out;
			}
		}
	}

out:
	up_read(&audio->dma_sem);
	return ret;
}

#if LINUX_VERSION_CODE < KERNEL_VERSION(6, 5, 0)
/*
 * Before 6.5 the PCM core has separate copy_user/copy_kernel callbacks
 * (same byte offset/count, plain buffer pointers) instead of the single
 * iov_iter-based .copy.  Wrap the buffer in an iov_iter and reuse
 * ua_pcm_copy().  Playback reads from the buffer (WRITE = data source),
 * capture fills it (READ = destination).
 */
static int ua_pcm_copy_user(struct snd_pcm_substream *sub, int channel,
			    unsigned long hwoff, void __user *buf,
			    unsigned long bytes)
{
	struct iovec iov = { .iov_base = buf, .iov_len = bytes };
	struct iov_iter iter;

	iov_iter_init(&iter,
		      sub->stream == SNDRV_PCM_STREAM_PLAYBACK ? WRITE : READ,
		      &iov, 1, bytes);
	return ua_pcm_copy(sub, channel, hwoff, &iter, bytes);
}

static int ua_pcm_copy_kernel(struct snd_pcm_substream *sub, int channel,
			      unsigned long hwoff, void *buf,
			      unsigned long bytes)
{
	struct kvec kv = { .iov_base = buf, .iov_len = bytes };
	struct iov_iter iter;

	iov_iter_kvec(&iter,
		      sub->stream == SNDRV_PCM_STREAM_PLAYBACK ? WRITE : READ,
		      &kv, 1, bytes);
	return ua_pcm_copy(sub, channel, hwoff, &iter, bytes);
}
#endif

/*
 * Fill silence — zero out the user channel slots in the DMA buffer.
 */
static int ua_pcm_silence(struct snd_pcm_substream *sub, int channel,
			  unsigned long hwoff, unsigned long bytes)
{
	struct ua_device *ua = snd_pcm_substream_chip(sub);
	struct ua_audio *audio = &ua->audio;
	struct device *dev = &ua->pdev->dev;
	struct snd_pcm_runtime *rt = sub->runtime;
	unsigned int user_frame_sz = frames_to_bytes(rt, 1);
	bool playback = sub->stream == SNDRV_PCM_STREAM_PLAYBACK;
	struct ua_dma_ring *ring = playback ? &audio->play_ring :
					     &audio->rec_ring;
	unsigned int hw_ch, hw_frame_sz;
	unsigned long frames, start_frame, i;
	char *dma_buf;

	/*
	 * Runs under the PCM stream lock, so it must not sleep.  There is
	 * nothing to do once the device is gone or ua_audio_fini() is
	 * tearing the rings down; report success rather than an error
	 * because the ALSA core WARNs on a failed silence fill.
	 */
	if (atomic_read(&ua->shutdown))
		return 0;
	if (!down_read_trylock(&audio->dma_sem))
		return 0;

	dma_buf = playback ? audio->play_buf : audio->rec_buf;
	if (!dma_buf) {
		up_read(&audio->dma_sem);
		return 0;
	}

	/* io_map=1 may change the frame width; read it under dma_sem */
	hw_ch = playback ? audio->play_channels : audio->rec_channels;
	hw_frame_sz = hw_ch * UA_SAMPLE_BYTES;

	frames = bytes / user_frame_sz;
	start_frame = hwoff / user_frame_sz;

	for (i = 0; i < frames; i++) {
		unsigned long f = (start_frame + i) % audio->buf_frame_size;
		unsigned long dma_off = f * hw_frame_sz;

		memset(dma_buf + dma_off, 0, hw_frame_sz);
	}

	ua_dma_ring_sync_frames(dev, ring, start_frame, frames, hw_frame_sz,
				audio->buf_frame_size, true);

	up_read(&audio->dma_sem);
	return 0;
}

/*
 * Clock write AFTER transport start enables capture.
 *
 * The clock write (0xC074 source=0xC + doorbell 4) transitions the DSP
 * from passthrough to active processing mode.  If done BEFORE transport,
 * the DSP activates with no audio flowing and routes silence.  After
 * transport, playback passthrough is already established and capture
 * routing activates correctly.
 * (Discovered 2026-03-18: ordering is critical)
 *
 * It sends the current source: internal (0x0C) unless one was chosen
 * (source 0 is S/PDIF, which never acknowledges without a signal; the
 * Clock Source control used to write it for "Internal").  Caller holds
 * ua->lock.
 */
static void ua_audio_post_start_clock(struct ua_device *ua)
{
	struct ua_audio *audio = &ua->audio;

	if (!ua->aceface_done || !post_start_clock)
		return;

	ua_write(ua, UA_REG_NOTIF_CLOCK_CFG,
		 (audio->rate_index << 8) | audio->clock_source);
	ua_write(ua, UA_REG_AX_CONNECT, UA_AX_CLOCK_DOORBELL);
	dev_dbg(&ua->pdev->dev,
		"post-transport clock write (capture enable)\n");

	/* A clock event for the DSPs when the rate is not the one they have */
	if (rate_ack_real && audio->rate_acked != ua_rate_hz(audio->rate_index))
		ua_audio_rate_ack_send(ua, ua_rate_hz(audio->rate_index),
				       "clock write after start");
}

/*
 * io_map=1, in pcm_prepare: the firmware republished its descriptors
 * after the post-start clock write of a rate change.  Restart the
 * transport the way pcm_prepare starts it (prepare_transport() commits
 * the new layout) so AX_*_CH, the frame stride and the channel map agree
 * again.  Caller holds ua->lock.
 */
static int ua_io_map_restart_transport(struct ua_device *ua)
{
	int ret;

	dev_info(&ua->pdev->dev,
		 "io_map: IO layout changed, restarting transport\n");

	ua_audio_stop_transport(ua);

	ret = ua_audio_prepare_transport(ua, 0);	/* keep TIMER_CFG */
	if (ret)
		return ret;

	ret = ua_audio_start_transport(ua);
	if (ret)
		return ret;

	ua_audio_post_start_clock(ua);
	return 0;
}

/*
 * A Hi-band start from a Hi-band clock needs a 48 kHz session first (x4,
 * 2026-10-10 on titan).
 *
 * Started while the hardware clock is already at 176.4 or 192 kHz, a
 * 192 kHz stream does not record: either the record DMA never writes the
 * ring, or it records silence on the monitor channels, while the
 * transport runs at the right rate and playback timing is fine.  176.4
 * kHz fails the same way, but only now and then: about 1 start in 6
 * (192 -> 176.4 and 176.4 -> 176.4).  Started from 44.1, 48, 88.2 or
 * 96 kHz, both work.  Running the transport at 48 kHz before the start
 * fixes it, provided the session is long enough.  Before a 192 kHz
 * start: from 192 kHz, 300 ms was enough (12 of 12); from 176.4 kHz it
 * took 800 ms (600 ms worked 2 of 4, 450 ms and shorter never did).
 * With the holds below, 176.4 kHz restarts worked 8 of 8 and 192 ->
 * 176.4 kHz 6 of 6.
 *
 * So run the transport at 48 kHz the way a 48 kHz stream would
 * (rate-change reconnect, start, clock write, restart on the republished
 * 48 kHz layout) for the per-rate hold, then let the regular rate-change
 * path take it from 48 kHz to the Hi-band rate.  That costs 0.5 s (from
 * 192 kHz) or 1.3 s (from 176.4 kHz) on such a start.
 *
 * The hardware rate comes from the notification block (rate index << 8),
 * not from sample_rate, which says 48 kHz after a reload whatever the
 * clock runs at.  Caller holds ua->lock (the service loop cannot clear the
 * notification bits meanwhile); the transport is stopped.
 */
#define UA_LOMID_FROM_192K_MS		400	/* 300 worked 12 of 12 */
#define UA_LOMID_FROM_176K4_MS		1200	/* 600 worked 2 of 4, 800 6 of 6 */

static void ua_audio_hi_from_hi(struct ua_device *ua, unsigned int rate)
{
	struct ua_audio *audio = &ua->audio;
	unsigned int hw_idx, hold_ms, waited;
	u32 status = 0;

	if ((rate != 176400 && rate != 192000) || !audio->connected ||
	    !ua->aceface_done)
		return;

	hw_idx = (ua_read(ua, UA_REG_NOTIF_RATE_INFO) >> 8) & 0xf;
	if (hw_idx == UA_RATE_192000)
		hold_ms = UA_LOMID_FROM_192K_MS;
	else if (hw_idx == UA_RATE_176400)
		hold_ms = UA_LOMID_FROM_176K4_MS;
	else
		return;

	dev_info(&ua->pdev->dev,
		 "%u Hz start with the clock at rate index %u: running 48 kHz first\n",
		 rate, hw_idx);

	/* The rate-change path of ua_pcm_prepare_locked(), to 48 kHz */
	audio->period_timer_running = false;
	hrtimer_cancel(&audio->period_timer);
	ua_audio_disconnect_hw(ua);
	audio->sample_rate = 48000;
	audio->rate_index = UA_RATE_48000;
	if (ua_audio_connect(ua))
		return;
	ua_enable_vector(ua, UA_IRQ_VEC_NOTIFICATION, true);
	ua_dsp_irq_arm(ua);	/* disconnect_hw masked every vector */

	if (ua_audio_prepare_transport(ua, 0) || ua_audio_start_transport(ua))
		return;
	ua_audio_post_start_clock(ua);

	/* io_map=1: restart on the 48 kHz layout once it is republished */
	if (ua_io_map_settle(ua) && ua_io_map_restart_transport(ua))
		return;

	/*
	 * Keep it running for hold_ms, doing what the DSP service loop does
	 * (it is stopped and blocked on ua->lock): clear the notification
	 * block (the firmware posts nothing new while earlier bits stay
	 * set), drain the mixer readback, flush the mixer batch.
	 */
	for (waited = 0; waited < hold_ms; waited += 10) {
		u32 notif = ua_read(ua, UA_REG_NOTIF_STATUS);

		if (notif == 0xFFFFFFFF)
			return;
		if (notif) {
			ua_audio_notif_ack(ua, notif);
			ua_event_post_notif(ua, notif);
			status |= notif;
		}
		ua_audio_poll_transport(ua);
		if (ua_read(ua, UA_REG_MIXER_RB_STATUS) == 1) {
			unsigned int i;

			for (i = 0; i < UA_MIXER_RB_WORDS; i++)
				ua->mixer_rb_data[i] =
					ua_read(ua, UA_REG_MIXER_RB_DATA + i * 4);
			ua->mixer_rb_status = 1;
			ua_write(ua, UA_REG_MIXER_RB_STATUS, 0);
		}
		/* The connect-time mixer batch, after the loop's 5-cycle delay */
		if (waited >= 50)
			ua_mixer_flush_settings(ua);
		msleep(10);
	}
	ua_audio_stop_transport(ua);

	dev_info(&ua->pdev->dev,
		 "48 kHz session done after %u ms (notifications 0x%08x), going on to %u Hz\n",
		 waited, status, rate);
}

/*
 * rate_change=1: change the rate the way the kext does
 * (UAD2AudioEngine::performFormatChange → CDeviceManager::SetAudioSampleRate,
 * the Mixer Engine's ack, _finishAckSrWaitCallback): the transport stays
 * stopped for the whole change, the clock write waits for its ack, then
 * for the firmware to report the new rate (25 ms polls, up to 4 s; a
 * timeout is only logged, as in the kext), then every DSP gets the rate
 * ack and the transport stays stopped another 1.5 s.  No disconnect and
 * no reconnect.  The caller starts the transport.  Caller holds ua->lock.
 */
#define UA_RATE_POLL_MS		25
#define UA_RATE_POLL_MAX_MS	4000
#define UA_RATE_ACK_HOLD_MS	1500

/*
 * One DSP service step while ua->lock is held through a long wait (the
 * service loop takes the lock): acknowledge notifications as the kext's
 * handler does (it writes 0 to the status after acting; the firmware
 * publishes the new rate only after that), drain the readback, flush the
 * mixer batch.  Returns the notification bits, ~0 when offline.
 */
static u32 ua_audio_service_step(struct ua_device *ua)
{
	u32 notif = ua_read(ua, UA_REG_NOTIF_STATUS);

	if (notif == 0xFFFFFFFF)
		return notif;
	if (notif) {
		ua_audio_notif_ack(ua, notif);
		ua_event_post_notif(ua, notif);
	}
	if (ua_read(ua, UA_REG_MIXER_RB_STATUS) == 1) {
		unsigned int i;

		for (i = 0; i < UA_MIXER_RB_WORDS; i++)
			ua->mixer_rb_data[i] =
				ua_read(ua, UA_REG_MIXER_RB_DATA + i * 4);
		ua->mixer_rb_status = 1;
		ua_write(ua, UA_REG_MIXER_RB_STATUS, 0);
	}
	ua_mixer_flush_settings(ua);
	return notif;
}

static int ua_audio_rate_change_kext(struct ua_device *ua, unsigned int rate)
{
	struct ua_audio *audio = &ua->audio;
	int idx = ua_rate_to_index(rate);
	unsigned int waited;
	u32 cfg;
	int ret;

	if (idx < 0)
		return idx;

	if (!audio->connected || !ua->aceface_done) {
		/* No notification block yet: connect applies the rate */
		audio->sample_rate = rate;
		audio->rate_index = idx;
		return 0;
	}

	dev_info(&ua->pdev->dev, "rate change %u -> %u Hz (transport stopped)\n",
		 audio->sample_rate, rate);

	if (audio->transport_running) {
		/* Callback never takes ua->lock: safe to wait for it here */
		audio->period_timer_running = false;
		hrtimer_cancel(&audio->period_timer);
		ua_audio_stop_transport(ua);
	}

	cfg = audio->clock_source | (idx << 8);
	ret = ua_audio_clock_change_locked(ua, cfg);
	if (ret == -ETIMEDOUT) {
		dev_info(&ua->pdev->dev, "  retrying clock change once\n");
		ret = ua_audio_clock_change_locked(ua, cfg);
	}
	if (ret) {
		dev_err(&ua->pdev->dev,
			"clock change to %u Hz (config 0x%08x) failed: %d\n",
			rate, cfg, ret);
		return ret;
	}

	for (waited = 0; waited < UA_RATE_POLL_MAX_MS;
	     waited += UA_RATE_POLL_MS) {
		if (ua_audio_service_step(ua) == 0xFFFFFFFF)
			return -ENODEV;
		if (((ua_read(ua, UA_REG_NOTIF_RATE_INFO) >> 8) & 0xff) == idx)
			break;
		msleep(UA_RATE_POLL_MS);
	}
	if (waited >= UA_RATE_POLL_MAX_MS)
		dev_warn(&ua->pdev->dev,
			 "  firmware rate still 0x%08x after %u ms (want index %d)\n",
			 ua_read(ua, UA_REG_NOTIF_RATE_INFO), waited, idx);
	else
		dev_dbg(&ua->pdev->dev, "  firmware rate after %u ms\n",
			waited);

	audio->sample_rate = rate;
	audio->rate_index = idx;
	ua_event_post(ua, UA_EVT_RATE_CHANGED, idx);

	ua_audio_rate_ack_send(ua, rate, "rate_change=1");
	for (waited = 0; waited < UA_RATE_ACK_HOLD_MS; waited += 10) {
		if (ua_audio_service_step(ua) == 0xFFFFFFFF)
			return -ENODEV;
		msleep(10);
	}

	/* io_map=1: the descriptors for the new rate */
	ua_io_map_rate_changed(ua);
	ua_audio_read_fw_latency(ua, "rate_change=1");
	return 0;
}

/* Body of ua_pcm_prepare(); caller holds ua->lock */
static int UA_GCC_OPTIMIZE("O1")
ua_pcm_prepare_locked(struct ua_device *ua, struct snd_pcm_substream *sub)
{
	struct ua_audio *audio = &ua->audio;
	bool rate_changed = false;
	int ret;

	if (atomic_read(&ua->shutdown))
		return -ENODEV;

	/*
	 * Prepare-churn guard (issue #42).
	 *
	 * When the protective WirePlumber rule is missing, the session
	 * manager auto-opens the raw multichannel PCM and churns
	 * open/prepare cycles against a transport whose SAMPLE_POS never
	 * advances (e.g. x8p, issue #46).  The resulting register traffic
	 * eventually wedges the PCIe link — hard system freeze, no oops.
	 *
	 * A healthy transport advances SAMPLE_POS between prepares (48
	 * frames/ms at minimum rate), so a frozen SAMPLE_POS across many
	 * rapid prepares can only be stall churn.  Refuse with -EIO and a
	 * 30s cooldown so userspace gets a clean error instead of the
	 * machine locking up.
	 */
	{
		u32 sp = ua_read(ua, UA_REG_AX_SAMPLE_POS);

		if (time_before(jiffies, audio->churn_block_until)) {
			dev_err_ratelimited(&ua->pdev->dev,
				"pcm_prepare: transport stalled, backing off — deploy WirePlumber rule 51-ua-apollo (configs/deploy.sh)\n");
			return -EIO;
		}

		if (sp == audio->churn_last_sample_pos &&
		    time_before(jiffies, audio->churn_last_jiffies + 2 * HZ)) {
			if (++audio->churn_count >= 8) {
				audio->churn_count = 0;
				audio->churn_block_until = jiffies + 30 * HZ;
				dev_err(&ua->pdev->dev,
					"pcm_prepare: SAMPLE_POS frozen across 8 rapid prepares — refusing opens for 30s to protect the PCIe link\n");
				return -EIO;
			}
		} else {
			audio->churn_count = 0;
		}
		audio->churn_last_jiffies = jiffies;
		audio->churn_last_sample_pos = sp;
	}

	dev_dbg(&ua->pdev->dev,
		"pcm_prepare: stream=%s rate=%u channels=%u period=%lu buffer=%lu "
		"connected=%d transport=%d aceface=%d\n",
		sub->stream == SNDRV_PCM_STREAM_PLAYBACK ? "play" : "rec",
		sub->runtime->rate, sub->runtime->channels,
		sub->runtime->period_size, sub->runtime->buffer_size,
		audio->connected, audio->transport_running, ua->aceface_done);

	if (rate_change == 1 && sub->runtime->rate != audio->sample_rate &&
	    audio->connected) {
		ua_pause_set(ua, UA_PAUSE_RATE);
		ret = ua_audio_rate_change_kext(ua, sub->runtime->rate);
		if (ret)
			return ret;
		rate_changed = true;
	}

	if (rate_change != 1 && !audio->transport_running)
		ua_audio_hi_from_hi(ua, sub->runtime->rate);

	if (sub->runtime->rate != audio->sample_rate) {
		/*
		 * Rate change (rate_change=0): soft disconnect/reconnect cycle.
		 *
		 * The kext does not do this: it changes the rate with the
		 * transport stopped and no disconnect, reconnect or firmware
		 * load (rate_change=1, ua_audio_rate_change_kext()).  Here the
		 * soft disconnect (preserves the mixer DSP) is followed by a
		 * connect that re-inits the mixer batch and notification
		 * state.
		 *
		 * The disconnect sets connected=false, so the connect
		 * block below reconnects and restarts the DSP service
		 * loop in this same prepare (it used to be an else-if
		 * that never reached the connect, leaving the device
		 * disconnected with the service loop — and so mixer
		 * flushes — stopped until the next prepare).  Then
		 * !transport_running handles transport + clock write.
		 */
		int new_idx = ua_rate_to_index(sub->runtime->rate);

		if (new_idx < 0)
			return new_idx;
		ua_pause_set(ua, UA_PAUSE_RATE);

		if (audio->connected) {
			dev_info(&ua->pdev->dev,
				 "pcm_prepare: rate change %u → %u (disconnect/reconnect)\n",
				 audio->sample_rate, sub->runtime->rate);

			/* Stop hrtimer before disconnect (callback never
			 * takes ua->lock, so waiting for it here is safe) */
			audio->period_timer_running = false;
			hrtimer_cancel(&audio->period_timer);

			/*
			 * Soft disconnect — stops transport, preserves
			 * mixer DSP, sets connected=false.  Does NOT send
			 * UA_AX_DISCONNECT when DSP SEQ is healthy.  The
			 * _hw variant stops the DSP service loop without
			 * waiting for it (its handler takes ua->lock); the
			 * reconnect below restarts it.
			 */
			ua_audio_disconnect_hw(ua);
		}

		/*
		 * Update rate for the (re)connect: prepare_transport()
		 * and the post-start clock write below use rate_index.
		 * Also applies when not connected, so the transport is
		 * never started at the previous rate.
		 */
		audio->sample_rate = sub->runtime->rate;
		audio->rate_index = new_idx;
		rate_changed = true;
	}

	/* Connect to DSP firmware if not (or no longer) connected */
	if (!audio->connected) {
		/*
		 * If ACEFACE already timed out, don't retry from pcm_prepare.
		 * PipeWire retries pcm_prepare immediately on failure, which
		 * creates an infinite 20s-blocking loop that freezes the system.
		 * A power cycle (hot-replug) clears aceface_failed via probe.
		 *
		 * Only when the handshake would actually run: with
		 * aceface_done set, ua_audio_connect() skips straight to
		 * post-handshake setup (no loop).  aceface_failed is never
		 * cleared by a later successful handshake — on a cold boot
		 * the pre-firmware attempt sets it and the post-firmware
		 * retry succeeds — so without this a rate change on such a
		 * device would leave it disconnected for good.
		 */
		if (ua->aceface_failed && !ua->aceface_done) {
			dev_dbg(&ua->pdev->dev,
				"pcm_prepare: ACEFACE previously failed, "
				"power-cycle Apollo to recover\n");
			return -ENODEV;
		}

		ret = ua_audio_connect(ua);
		if (ret)
			return ret;

		/*
		 * The soft disconnect masked every vector; re-arm the
		 * notification vector the kext keeps enabled for the whole
		 * connected session (EnableVector(0x28, 1) in Program-
		 * Registers, disabled only by Shutdown).  The transport
		 * vectors follow in prepare_transport() below.
		 */
		if (rate_changed) {
			ua_enable_vector(ua, UA_IRQ_VEC_NOTIFICATION, true);
			ua_dsp_irq_arm(ua);	/* masked by disconnect_hw */
		}

		/*
		 * DSP connected — start periodic readback drain.  This
		 * also resets dsp_service_count, so after a rate change
		 * the mixer flush happens after 5 cycles (0.5 s) rather
		 * than a cold-boot delay.
		 */
		ua_dsp_service_start(ua);
	}

	/*
	 * Prepare transport if not running.
	 *
	 * Order follows kext CPcieDevice::ProgramRegisters():
	 *   1. ResetDMAEngines (toggle 0x2200 reset bits)
	 *   2. Clear interrupts (0xFFFFFFFF → ISR)
	 *   3. Program SG tables (ring buffer descriptors)
	 *   4. PrepareTransport (configure params, ARM with CTRL=1)
	 *   5. StartTransport (CTRL=0x20F)
	 *
	 * Previously SG was programmed AFTER ARM, which is wrong.
	 * Also, writing 1024 SG entries (instead of 4) corrupted
	 * ring buffer control registers and the record SG table.
	 */
	if (ua_io_map_needs_restart(ua)) {
		/* io_map=1: apply a layout change seen while streaming */
		dev_info(&ua->pdev->dev,
			 "pcm_prepare: IO layout is stale, restarting transport\n");
		ua_audio_stop_transport(ua);
	}

	if (!audio->transport_running) {
		dev_dbg(&ua->pdev->dev,
			"pcm_prepare: transport not running — restarting "
			"(play_sub=%p rec_sub=%p)\n",
			audio->play_sub, audio->rec_sub);

		/*
		 * The period timer (period_irq=1) from the ALSA period size,
		 * so its interrupts fire at the period boundary.  Only
		 * written here, while the transport is stopped: the first
		 * substream to prepare sets the shared interval.
		 */
		ret = ua_audio_prepare_transport(ua, sub->runtime->period_size);
		if (ret)
			return ret;

		ret = ua_audio_start_transport(ua);
		if (ret)
			return ret;

		/* Clock write AFTER transport start enables capture */
		ua_audio_post_start_clock(ua);

		/*
		 * The firmware may republish its IO descriptors for the
		 * new rate; they are logged either way, and with io_map=1
		 * a changed layout restarts the transport once more.
		 */
		if (rate_changed && ua_io_map_settle(ua)) {
			ret = ua_io_map_restart_transport(ua);
			if (ret)
				return ret;
		}
	} else {
		/* No TIMER_CFG rewrite under a running transport (#42) */
		dev_dbg(&ua->pdev->dev,
			"pcm_prepare: transport already running, skip restart\n");
	}

	ua_io_map_check_stream(ua, sub);

	/* Reset position for this direction */
	if (sub->stream == SNDRV_PCM_STREAM_PLAYBACK) {
		audio->play_pos = 0;
	} else {
		audio->rec_pos = 0;
	}

	return 0;
}

/*
 * pcm_prepare is serialized with the DSP service loop, the ioctls and the
 * other substream's prepare by ua->lock (it used to touch the transport
 * registers unlocked).
 *
 * Nothing is stopped before the lock is taken: whether a rate change
 * happens is only known under it (the churn guard may refuse first, or
 * ua_audio_set_clock() may have committed the rate meanwhile).  The rate
 * change stops the service loop with the non-waiting
 * ua_audio_disconnect_hw() — a handler blocked on ua->lock runs once more
 * against the new state, the single delayed_work cannot run twice — and
 * the period timer with hrtimer_cancel(), whose callback never takes
 * ua->lock.  On the way out the two invariants are restored whatever
 * path the body took: a connected device has its service loop, and a
 * transport that runs again keeps the shared period timer another
 * running substream relies on.
 */
static int ua_pcm_prepare(struct snd_pcm_substream *sub)
{
	struct ua_device *ua = snd_pcm_substream_chip(sub);
	struct ua_audio *audio = &ua->audio;
	bool timer_was_running;
	int ret;

	if (atomic_read(&ua->shutdown))
		return -ENODEV;

	/* System resume still re-initialising (ua_resume_work()) */
	if (READ_ONCE(ua->resume_pending)) {
		dev_info_ratelimited(&ua->pdev->dev,
				     "pcm_prepare: device re-initialising after resume, try again shortly\n");
		return -EBUSY;
	}

	/* Verify device is reachable before attempting any hardware access */
	if (ua_read(ua, UA_REG_FPGA_REV) == 0xFFFFFFFF) {
		dev_err(&ua->pdev->dev,
			"pcm_prepare: device unreachable\n");
		return -ENODEV;
	}

	mutex_lock(&ua->lock);

	/* UA_IOCTL_DMA_TEST owns the transport (it drops the lock to wait) */
	if (audio->dma_test_active) {
		mutex_unlock(&ua->lock);
		return -EBUSY;
	}

	timer_was_running = audio->period_timer_running;

	ret = ua_pcm_prepare_locked(ua, sub);
	ua_pause_clear(ua, UA_PAUSE_RATE);

	if (audio->connected && !audio->dsp_service_running &&
	    !atomic_read(&ua->shutdown))
		ua_dsp_service_start(ua);

	/*
	 * The prepared substream is never running (ALSA refuses prepare
	 * then), so a timer that was running served the other one.
	 */
	if (timer_was_running && !audio->period_timer_running &&
	    audio->transport_running) {
		audio->period_timer_running = true;
		hrtimer_start(&audio->period_timer, ms_to_ktime(1),
			      HRTIMER_MODE_REL_SOFT);
	}
	mutex_unlock(&ua->lock);

	return ret;
}

/* ----------------------------------------------------------------
 * Period tracking
 *
 * Every running substream gets its period wakeups from exactly one of:
 *
 *  - the hrtimer (default): a 1 ms soft hrtimer reads SAMPLE_POS and calls
 *    snd_pcm_period_elapsed() when a period boundary was crossed;
 *  - the period-timer interrupt (period_irq=1): prepare programs TIMER_CFG
 *    from the period, the hard IRQ (ISR/ISR_HI read and ack, vector
 *    re-arm, nothing else on BAR0) wakes ua_audio_irq_thread(), which
 *    reads SAMPLE_POS once in process context and makes the same boundary
 *    test.  The reference drivers take their audio clock from interrupts
 *    too, never from a BAR0 poll, but from the end-of-buffer vector (kext:
 *    UAD2AudioEngine::_bufferCallback → takeTimeStamp, once per 8192-frame
 *    wrap; Windows: the same fast callback).  The kext's only timer user is
 *    its client audio mixer (CUAD2AudioMixer: the block size, capped at
 *    256/512/1024 frames for the 48/96/192 kHz bands).
 *    With period_irq=0 the timer is not programmed and its vector stays
 *    masked, the macOS state.
 *
 * The owner is chosen per substream at trigger(START) and kept until
 * STOP (irq_subs): the interrupt when period_irq=1, the firmware has the
 * 64-vector interrupt block (fw_v2), and the period is a whole multiple of
 * the timer interval the transport runs with and at most a quarter of the
 * ring (ua_period_irq_usable() has the details).  There is one clock and one
 * TIMER_CFG, written only while the transport is stopped (#42), so a
 * second substream with a shorter or unaligned period goes to the
 * hrtimer.  Each path tests its own substream's bit under that substream's
 * PCM stream lock, which the trigger holds while it flips the bit: no
 * substream is ever served by both.  The hard IRQ itself never calls
 * snd_pcm_period_elapsed() (it used to, on top of the hrtimer: a second
 * wakeup per period and a SAMPLE_POS read in hard-IRQ context through the
 * pointer callback).
 *
 * The end-of-buffer interrupt (one per ring wrap) also wakes the thread:
 * one more boundary test, and a check that SAMPLE_POS is near the ring
 * start then (pstats.eob_pos_odd).
 *
 * The watchdog (ua_period_wd_handler(), process context, every
 * UA_PERIOD_WD_MS while a substream is IRQ-served) covers what no
 * interrupt reports: the transport stopping or restarting under the
 * stream and the device leaving the bus (both push the stream into XRUN,
 * as the hrtimer does), and a timer interrupt that falls silent while
 * SAMPLE_POS keeps moving (the stream moves to the hrtimer for the rest of
 * the transport session).  It reads BAR0 only after a few silent timer
 * intervals.
 * ---------------------------------------------------------------- */

#define UA_PIRQ_EV_TIMER		0	/* irq_events: period timer fired */
#define UA_PIRQ_EV_EOB			1	/* irq_events: end of buffer */
#define UA_PERIOD_WD_MS			10	/* watchdog interval */
#define UA_PERIOD_IRQ_SILENT_MS		20	/* least silence it acts on */

/*
 * The transport an IRQ-served stream started on is gone: stopped, or
 * restarted (SAMPLE_POS back at 0 under the stream, new TIMER_CFG phase).
 */
static bool ua_period_irq_stale(struct ua_audio *audio, unsigned int dir)
{
	return !READ_ONCE(audio->transport_running) ||
	       READ_ONCE(audio->irq_gen[dir]) != READ_ONCE(audio->transport_gen);
}

/*
 * The period boundary test of one substream for the path @irq (true: the
 * IRQ thread, false: the hrtimer), with SAMPLE_POS @sp.  buffer_size ==
 * buf_frame_size (pcm_open), so sp % buffer_size is the position.
 *
 * Caller holds audio->lock (pcm_close clears the substream pointers under
 * it before ALSA frees the runtime) with interrupts off.  The stream lock
 * taken here pins the owner bit and last_*_period_pos against the trigger.
 * Lock order audio->lock -> PCM stream lock -> timer_lock (the trigger,
 * also reached from an XRUN inside snd_pcm_period_elapsed()).
 */
static void ua_period_check(struct ua_audio *audio,
			    struct snd_pcm_substream *sub, unsigned int dir,
			    u32 sp, bool irq)
{
	struct snd_pcm_runtime *rt = sub->runtime;
	snd_pcm_uframes_t *last = dir == UA_DIR_PLAY ?
				  &audio->last_play_period_pos :
				  &audio->last_rec_period_pos;
	snd_pcm_uframes_t pos;
	bool owned, crossed;

	snd_pcm_stream_lock(sub);
	owned = test_bit(dir, &audio->irq_subs);

	/* Not this path's, or open but not configured yet (sizes 0) */
	if (owned != irq || !rt->period_size || !rt->buffer_size)
		goto out;

	if (irq && ua_period_irq_stale(audio, dir)) {
		if (snd_pcm_running(sub)) {
			audio->pstats.stops++;
			snd_pcm_stop(sub, SNDRV_PCM_STATE_XRUN);
		}
		goto out;
	}

	pos = sp % rt->buffer_size;
	crossed = pos / rt->period_size != *last / rt->period_size;
	*last = pos;
	if (crossed) {
		audio->pstats.elapsed[irq][dir]++;
		snd_pcm_period_elapsed_under_stream_lock(sub);
	}
out:
	snd_pcm_stream_unlock(sub);
}

/*
 * Period elapsed polling timer.
 *
 * The original driver found the period-timer interrupt (vector 0x46,
 * ISR_HI bit 30) firing once and then stopping; it is re-armed after
 * every interrupt now (ua_audio_irq()), and period_irq=1 takes the period
 * wakeups from it, but by default an hrtimer polls SAMPLE_POS and calls
 * snd_pcm_period_elapsed() at ~1ms intervals, as USB audio drivers do.
 *
 * The timer is a soft hrtimer (HRTIMER_MODE_REL_SOFT): the callback runs
 * from the HRTIMER_SOFTIRQ right after the timer interrupt instead of
 * inside it, so the 1 ms cadence is kept but the BAR0 read no longer
 * happens in hard-IRQ context with interrupts off.  A read that stalls on
 * a wedged link then holds up softirq processing on one CPU instead of
 * a CPU with interrupts disabled.  A workqueue would cost jiffy
 * granularity (4 ms at HZ=250), longer than a 256-frame period at 96 kHz.
 */
static enum hrtimer_restart ua_period_timer_callback(struct hrtimer *timer)
{
	struct ua_audio *audio = container_of(timer, struct ua_audio,
					      period_timer);
	struct ua_device *ua = container_of(audio, struct ua_device, audio);
	struct snd_pcm_substream *rec, *play;
	unsigned long flags;
	u32 sp;

	if (!audio->period_timer_running)
		return HRTIMER_NORESTART;

	/*
	 * Device removed or failed (AER sets the shutdown flag and clears
	 * transport_running), or the transport stopped under running
	 * streams: their pointer would freeze with no wakeups, so push them
	 * into XRUN as on a dead link below; pcm_prepare then fails or
	 * restarts the transport.
	 */
	sp = 0xFFFFFFFF;
	if (!atomic_read(&ua->shutdown) && audio->transport_running)
		sp = ua_read(ua, UA_REG_AX_SAMPLE_POS);
	if (sp == 0xFFFFFFFF) {
		/*
		 * Link down (Thunderbolt drop, power-off, AER): every
		 * further read would stall a CPU for a full completion
		 * timeout.  Stop polling and push the streams into XRUN
		 * so userspace re-prepares, where pcm_prepare sees
		 * FPGA_REV == ~0 and fails cleanly instead of hanging.
		 */
		audio->period_timer_running = false;
		if (!atomic_read(&ua->shutdown) && audio->transport_running)
			dev_err_ratelimited(&ua->pdev->dev,
					    "period timer: BAR0 reads all-ones (link down), stopping streams\n");
		spin_lock_irqsave(&audio->lock, flags);
		if (audio->play_sub)
			snd_pcm_stop_xrun(audio->play_sub);
		if (audio->rec_sub)
			snd_pcm_stop_xrun(audio->rec_sub);
		spin_unlock_irqrestore(&audio->lock, flags);
		return HRTIMER_NORESTART;
	}

	/*
	 * Hold audio->lock while the substreams are used — pcm_close
	 * clears these pointers under the same lock before ALSA frees the
	 * runtime, so holding it (not just snapshotting) keeps the runtime
	 * valid for the whole update.  Lock order audio->lock -> PCM
	 * stream lock; nothing takes them the other way round.
	 */
	spin_lock_irqsave(&audio->lock, flags);
	rec = audio->rec_sub;
	play = audio->play_sub;

	/* Period boundary crossed?  Only substreams the IRQ path does not serve */
	if (play)
		ua_period_check(audio, play, UA_DIR_PLAY, sp, false);
	if (rec)
		ua_period_check(audio, rec, UA_DIR_REC, sp, false);
	spin_unlock_irqrestore(&audio->lock, flags);

	hrtimer_forward_now(timer, ms_to_ktime(1));
	return HRTIMER_RESTART;
}

/*
 * Push IRQ-served substreams into XRUN: all of them when @dead (the device
 * left the bus), else those whose transport is gone (ua_period_irq_stale()).
 * Nothing else would notice without the 1 ms poll: their pointer would
 * freeze or jump.  pcm_prepare then fails or restarts the transport, as
 * after the hrtimer's XRUN.  Takes audio->lock, then each stream lock.
 */
static void ua_period_irq_stop(struct ua_device *ua, bool dead,
			       const char *why)
{
	struct ua_audio *audio = &ua->audio;
	struct snd_pcm_substream *subs[UA_DIRS];
	unsigned int dir, stopped = 0;
	unsigned long flags;

	spin_lock_irqsave(&audio->lock, flags);
	subs[UA_DIR_REC] = audio->rec_sub;
	subs[UA_DIR_PLAY] = audio->play_sub;
	for (dir = 0; dir < UA_DIRS; dir++) {
		struct snd_pcm_substream *sub = subs[dir];

		if (!test_bit(dir, &audio->irq_subs))
			continue;
		if (sub)
			snd_pcm_stream_lock(sub);
		if (sub && snd_pcm_running(sub)) {
			if (dead || ua_period_irq_stale(audio, dir)) {
				audio->pstats.stops++;
				snd_pcm_stop(sub, SNDRV_PCM_STATE_XRUN);
				stopped++;
			}
		} else {
			/*
			 * Not running, or closed: a bit trigger(STOP) never
			 * cleared.  Drop it, or the watchdog runs forever.
			 */
			spin_lock(&audio->timer_lock);
			__clear_bit(dir, &audio->irq_subs);
			spin_unlock(&audio->timer_lock);
		}
		if (sub)
			snd_pcm_stream_unlock(sub);
	}
	spin_unlock_irqrestore(&audio->lock, flags);

	if (stopped)
		dev_warn_ratelimited(&ua->pdev->dev,
				     "period_irq: %s, stopping %u stream(s)\n",
				     why, stopped);
}

/*
 * The period timer fell silent while SAMPLE_POS moves on: hand the
 * IRQ-served substreams to the hrtimer.  irq_dead keeps later starts on
 * the hrtimer too, until the next transport start.
 */
static void ua_period_irq_fallback(struct ua_device *ua, unsigned long silent)
{
	struct ua_audio *audio = &ua->audio;
	struct snd_pcm_substream *subs[UA_DIRS];
	unsigned int dir, moved = 0;
	unsigned long flags;

	spin_lock_irqsave(&audio->lock, flags);
	subs[UA_DIR_REC] = audio->rec_sub;
	subs[UA_DIR_PLAY] = audio->play_sub;
	for (dir = 0; dir < UA_DIRS; dir++) {
		struct snd_pcm_substream *sub = subs[dir];

		if (!sub)
			continue;
		/* The trigger's locks: the owner changes as in trigger(START) */
		snd_pcm_stream_lock(sub);
		spin_lock(&audio->timer_lock);
		if (test_bit(dir, &audio->irq_subs)) {
			__clear_bit(dir, &audio->irq_subs);
			audio->irq_dead = true;
			audio->period_timer_running = true;
			hrtimer_start(&audio->period_timer, ms_to_ktime(1),
				      HRTIMER_MODE_REL_SOFT);
			moved++;
		}
		spin_unlock(&audio->timer_lock);
		snd_pcm_stream_unlock(sub);
	}
	spin_unlock_irqrestore(&audio->lock, flags);

	if (moved) {
		audio->pstats.fallbacks++;
		dev_warn(&ua->pdev->dev,
			 "period_irq: no period-timer interrupt for %u ms while SAMPLE_POS advances (TIMER_CFG 0x%x, %u frames); %u stream(s) moved to the 1 ms poll for this transport session\n",
			 jiffies_to_msecs(silent), audio->timer_cfg,
			 audio->timer_frames, moved);
	}
}

/* How long the period timer may stay silent: three intervals, >= 20 ms */
static unsigned long ua_period_irq_silent_limit(struct ua_audio *audio)
{
	unsigned int rate = READ_ONCE(audio->sample_rate);
	unsigned int ms = 0;

	if (rate)
		ms = DIV_ROUND_UP(3 * READ_ONCE(audio->timer_frames) * 1000,
				  rate);
	return msecs_to_jiffies(max_t(unsigned int, ms,
				      UA_PERIOD_IRQ_SILENT_MS));
}

/*
 * IRQ-path watchdog, every UA_PERIOD_WD_MS while a substream is
 * IRQ-served (queued by trigger(START), stops itself when none is).
 * Process context, never takes ua->lock.  BAR0 is read only when the
 * period timer has been silent for ua_period_irq_silent_limit(): all-ones
 * means the link is down (XRUN, as the hrtimer does); a SAMPLE_POS that
 * moved since the previous silent read means the transport runs but the
 * interrupt does not (fall back to the hrtimer); a frozen one is a stalled
 * transport, where the hrtimer would not report a period either.
 */
static void ua_period_wd_handler(struct work_struct *work)
{
	struct ua_audio *audio = container_of(to_delayed_work(work),
					      struct ua_audio, period_wd);
	struct ua_device *ua = container_of(audio, struct ua_device, audio);
	unsigned long last, silent;
	unsigned int dir;
	u32 sp;

	if (!READ_ONCE(audio->irq_subs))
		return;

	if (atomic_read(&ua->shutdown)) {
		/* Removed or an AER error: no BAR0 access */
		ua_period_irq_stop(ua, true, "device gone");
		goto rearm;
	}

	for (dir = 0; dir < UA_DIRS; dir++) {
		if (test_bit(dir, &audio->irq_subs) &&
		    ua_period_irq_stale(audio, dir)) {
			ua_period_irq_stop(ua, false,
					   "transport stopped under the stream");
			break;
		}
	}

	last = READ_ONCE(audio->timer_irq_jiffies);
	silent = jiffies - last;
	if (READ_ONCE(audio->transport_running) &&
	    silent > ua_period_irq_silent_limit(audio)) {
		sp = ua_read(ua, UA_REG_AX_SAMPLE_POS);
		if (sp == 0xFFFFFFFF)
			ua_period_irq_stop(ua, true,
					   "BAR0 reads all-ones (link down)");
		else if (time_after(audio->wd_sp_jiffies, last) &&
			 sp != audio->wd_last_sp)
			ua_period_irq_fallback(ua, silent);
		audio->wd_last_sp = sp;
		audio->wd_sp_jiffies = jiffies;
	}

rearm:
	if (READ_ONCE(audio->irq_subs))
		queue_delayed_work(system_highpri_wq, &audio->period_wd,
				   msecs_to_jiffies(UA_PERIOD_WD_MS));
}

/* Remove, shutdown, suspend: no watchdog pass after this returns */
void ua_audio_period_wd_cancel(struct ua_device *ua)
{
	cancel_delayed_work_sync(&ua->audio.period_wd);
}

/*
 * May the period-timer interrupt serve @sub (trigger(START), under
 * timer_lock)?  It needs period_irq=1; the 64-vector interrupt block (the
 * timer is ISR_HI bit 30, which ua_enable_vector() only arms with fw_v2);
 * a running transport whose timer has not fallen silent; and a timer
 * interval of at least 1 ms that divides both the ring (8192 frames) and
 * the period, so every period boundary in SAMPLE_POS falls on a timer
 * interrupt and the boundary test catches it at the first one after it.
 * And at least four periods in the ring: should the interrupt land just
 * before the boundary instead of after it (TIMER_CFG counting, see
 * period_irq_cfg_adj), a wakeup comes one period late, which then still
 * leaves headroom.
 */
static bool ua_period_irq_usable(struct ua_device *ua,
				 struct snd_pcm_substream *sub)
{
	struct ua_audio *audio = &ua->audio;
	unsigned int timer = READ_ONCE(audio->timer_frames);
	snd_pcm_uframes_t period = sub->runtime->period_size;

	return READ_ONCE(period_irq) && ua->fw_v2 &&
	       audio->transport_running && !audio->irq_dead &&
	       timer && timer * 1000ULL >= audio->sample_rate &&
	       audio->buf_frame_size % timer == 0 &&
	       period >= timer && period % timer == 0 &&
	       period * 4 <= audio->buf_frame_size;
}

/* IRQ-thread diagnostics for /proc/asound/cardN/period_irq */
static void ua_period_irq_stats(struct ua_audio *audio, unsigned long events,
				u32 sp)
{
	struct ua_period_stats *st = &audio->pstats;
	unsigned int buf = audio->buf_frame_size;
	unsigned int timer = READ_ONCE(audio->timer_frames);

	if (!buf)
		return;
	sp %= buf;
	if ((events & BIT(UA_PIRQ_EV_TIMER)) && timer) {
		unsigned int phase = sp % timer;

		st->phase_sum += phase;
		st->phase_n++;
		if (phase > st->phase_max)
			st->phase_max = phase;
	}
	if (events & BIT(UA_PIRQ_EV_EOB)) {
		/* The ring just wrapped: SAMPLE_POS should be near 0 */
		st->eob_last_sp = sp;
		if (sp > buf / 8)
			st->eob_pos_odd++;
	}
}

/*
 * Threaded half of the interrupt (process context, the IRQ's kthread),
 * woken by ua_audio_irq() for a period-timer or end-of-buffer interrupt
 * while a substream is IRQ-served: one SAMPLE_POS read, then each
 * IRQ-served substream's boundary test.
 */
void ua_audio_irq_thread(struct ua_device *ua)
{
	struct ua_audio *audio = &ua->audio;
	struct snd_pcm_substream *rec, *play;
	unsigned long events, flags;
	u32 sp = 0;

	events = xchg(&audio->irq_events, 0);
	if (!events || !READ_ONCE(audio->irq_subs) ||
	    atomic_read(&ua->shutdown))
		return;

	/* Transport stopped: no read, ua_period_check() stops the streams */
	if (READ_ONCE(audio->transport_running)) {
		sp = ua_read(ua, UA_REG_AX_SAMPLE_POS);
		if (sp == 0xFFFFFFFF) {
			ua_period_irq_stop(ua, true,
					   "BAR0 reads all-ones (link down)");
			return;
		}
		ua_period_irq_stats(audio, events, sp);
	}

	spin_lock_irqsave(&audio->lock, flags);
	rec = audio->rec_sub;
	play = audio->play_sub;
	if (play)
		ua_period_check(audio, play, UA_DIR_PLAY, sp, true);
	if (rec)
		ua_period_check(audio, rec, UA_DIR_REC, sp, true);
	audio->pstats.thread_runs++;
	spin_unlock_irqrestore(&audio->lock, flags);
}

/*
 * /proc/asound/cardN/period_irq: one "key value" per line, for
 * tools/period-irq-test.sh.  Counters are cumulative since load except
 * phase_*, which restart with each transport start.
 */
static void ua_period_proc_read(struct snd_info_entry *entry,
				struct snd_info_buffer *buf)
{
	struct ua_device *ua = entry->private_data;
	struct ua_audio *audio = &ua->audio;
	struct ua_period_stats *st = &audio->pstats;
	unsigned long run = READ_ONCE(audio->running_subs);
	unsigned long irq = READ_ONCE(audio->irq_subs);
	static const char * const name[UA_DIRS] = {
		[UA_DIR_REC] = "rec", [UA_DIR_PLAY] = "play",
	};
	unsigned int dir;

	snd_iprintf(buf, "period_irq %d\n", READ_ONCE(period_irq));
	snd_iprintf(buf, "cfg_adj %d\n", READ_ONCE(period_irq_cfg_adj));
	snd_iprintf(buf, "fw_v2 %d\n", ua->fw_v2);
	snd_iprintf(buf, "transport %d\n", audio->transport_running);
	snd_iprintf(buf, "transport_gen %u\n", audio->transport_gen);
	snd_iprintf(buf, "rate %u\n", audio->sample_rate);
	snd_iprintf(buf, "buffer_frames %u\n", audio->buf_frame_size);
	snd_iprintf(buf, "timer_frames %u\n", audio->timer_frames);
	snd_iprintf(buf, "timer_cfg %u\n", audio->timer_cfg);
	snd_iprintf(buf, "irq_dead %d\n", audio->irq_dead);
	for (dir = 0; dir < UA_DIRS; dir++)
		snd_iprintf(buf, "owner_%s %s\n", name[dir],
			    !test_bit(dir, &run) ? "idle" :
			    test_bit(dir, &irq) ? "irq" : "hrtimer");
	snd_iprintf(buf, "timer_irqs %lu\n", st->timer_irqs);
	snd_iprintf(buf, "eob_irqs %lu\n", st->eob_irqs);
	snd_iprintf(buf, "thread_runs %lu\n", st->thread_runs);
	for (dir = 0; dir < UA_DIRS; dir++) {
		snd_iprintf(buf, "elapsed_irq_%s %lu\n", name[dir],
			    st->elapsed[UA_PERIOD_PATH_IRQ][dir]);
		snd_iprintf(buf, "elapsed_hrtimer_%s %lu\n", name[dir],
			    st->elapsed[UA_PERIOD_PATH_HRTIMER][dir]);
	}
	snd_iprintf(buf, "phase_n %lu\n", st->phase_n);
	snd_iprintf(buf, "phase_mean %lu\n",
		    st->phase_n ? st->phase_sum / st->phase_n : 0);
	snd_iprintf(buf, "phase_max %u\n", st->phase_max);
	snd_iprintf(buf, "eob_last_sp %u\n", st->eob_last_sp);
	snd_iprintf(buf, "eob_pos_odd %lu\n", st->eob_pos_odd);
	snd_iprintf(buf, "fallbacks %lu\n", st->fallbacks);
	snd_iprintf(buf, "stops %lu\n", st->stops);
}

static int ua_pcm_trigger(struct snd_pcm_substream *sub, int cmd)
{
	struct ua_device *ua = snd_pcm_substream_chip(sub);
	struct ua_audio *audio = &ua->audio;
	unsigned long flags;
	unsigned int dir;

	dir = sub->stream == SNDRV_PCM_STREAM_PLAYBACK ? UA_DIR_PLAY :
							  UA_DIR_REC;

	switch (cmd) {
	case SNDRV_PCM_TRIGGER_START:
	case SNDRV_PCM_TRIGGER_RESUME:
		if (atomic_read(&ua->shutdown))
			return -ENODEV;

		spin_lock_irqsave(&audio->timer_lock, flags);
		/* Only this direction's period tracking starts over */
		if (dir == UA_DIR_PLAY)
			audio->last_play_period_pos = 0;
		else
			audio->last_rec_period_pos = 0;
		__set_bit(dir, &audio->running_subs);
		if (ua_period_irq_usable(ua, sub)) {
			/* Period-timer interrupt ("Period tracking") */
			__set_bit(dir, &audio->irq_subs);
			WRITE_ONCE(audio->irq_gen[dir],
				   READ_ONCE(audio->transport_gen));
			WRITE_ONCE(audio->timer_irq_jiffies, jiffies);
			audio->wd_sp_jiffies = jiffies - 1;
			queue_delayed_work(system_highpri_wq, &audio->period_wd,
					   msecs_to_jiffies(UA_PERIOD_WD_MS));
		} else {
			audio->period_timer_running = true;
			hrtimer_start(&audio->period_timer,
				      ms_to_ktime(1), HRTIMER_MODE_REL_SOFT);
		}
		spin_unlock_irqrestore(&audio->timer_lock, flags);
		return 0;

	case SNDRV_PCM_TRIGGER_STOP:
	case SNDRV_PCM_TRIGGER_SUSPEND:
		/*
		 * The timer is shared: one substream stopping (a short aplay
		 * ending while arecord runs) must not take the other's period
		 * wakeups away, so it stops only when no running substream
		 * is left on it (IRQ-served ones do not use it).
		 *
		 * Use try_to_cancel — NOT hrtimer_cancel.
		 * trigger(STOP) can be called from snd_pcm_period_elapsed()
		 * inside the hrtimer callback itself (xrun path).
		 * hrtimer_cancel() would deadlock waiting for the callback
		 * that is calling us.
		 *
		 * Also with the shutdown flag set (no register access here):
		 * the XRUN stop after a removal or an AER error must clear
		 * the bits, or they outlive the stream.
		 */
		spin_lock_irqsave(&audio->timer_lock, flags);
		__clear_bit(dir, &audio->running_subs);
		__clear_bit(dir, &audio->irq_subs);
		if (!(audio->running_subs & ~audio->irq_subs)) {
			audio->period_timer_running = false;
			hrtimer_try_to_cancel(&audio->period_timer);
		}
		spin_unlock_irqrestore(&audio->timer_lock, flags);
		return 0;

	default:
		return -EINVAL;
	}
}

static snd_pcm_uframes_t ua_pcm_pointer(struct snd_pcm_substream *sub)
{
	struct ua_device *ua = snd_pcm_substream_chip(sub);
	struct ua_audio *audio = &ua->audio;
	unsigned int dir = sub->stream == SNDRV_PCM_STREAM_PLAYBACK ?
			   UA_DIR_PLAY : UA_DIR_REC;
	u32 sp;

	if (atomic_read(&ua->shutdown))
		return SNDRV_PCM_POS_XRUN;

	/*
	 * Read hardware sample position.
	 *
	 * SAMPLE_POS (0x2244) returns the current frame index within
	 * the hardware ring buffer. buffer_size is constrained to
	 * buf_frame_size, so sp % buffer_size == sp % buf_frame_size
	 * and always produces correct positions.
	 */
	sp = ua_read(ua, UA_REG_AX_SAMPLE_POS);

	/*
	 * An IRQ-served stream has no 1 ms poll to notice a dead link or a
	 * transport stopped or restarted under it: report the XRUN here.
	 * Called under the stream lock, so the owner bit is stable.
	 */
	if (test_bit(dir, &audio->irq_subs) &&
	    (sp == 0xFFFFFFFF || ua_period_irq_stale(audio, dir)))
		return SNDRV_PCM_POS_XRUN;

	return sp % sub->runtime->buffer_size;
}

static const struct snd_pcm_ops ua_pcm_playback_ops = {
	.open         = ua_pcm_open,
	.close        = ua_pcm_close,
	.hw_params    = ua_pcm_hw_params,
	.hw_free      = ua_pcm_hw_free,
	.prepare      = ua_pcm_prepare,
	.trigger      = ua_pcm_trigger,
	.pointer      = ua_pcm_pointer,
#if LINUX_VERSION_CODE >= KERNEL_VERSION(6, 5, 0)
	.copy         = ua_pcm_copy,
#else
	.copy_user    = ua_pcm_copy_user,
	.copy_kernel  = ua_pcm_copy_kernel,
#endif
	.fill_silence = ua_pcm_silence,
};

static const struct snd_pcm_ops ua_pcm_capture_ops = {
	.open         = ua_pcm_open,
	.close        = ua_pcm_close,
	.hw_params    = ua_pcm_hw_params,
	.hw_free      = ua_pcm_hw_free,
	.prepare      = ua_pcm_prepare,
	.trigger      = ua_pcm_trigger,
	.pointer      = ua_pcm_pointer,
#if LINUX_VERSION_CODE >= KERNEL_VERSION(6, 5, 0)
	.copy         = ua_pcm_copy,
#else
	.copy_user    = ua_pcm_copy_user,
	.copy_kernel  = ua_pcm_copy_kernel,
#endif
};

/* ----------------------------------------------------------------
 * IRQ dispatch — called from main ISR in ua_core.c (hard IRQ)
 *
 * ISR_HI bit 30 is the period timer (vector 0x46), bit 31 the end of
 * buffer (vector 0x47).  Hard-IRQ work is the vector re-arm and the
 * bookkeeping below; the period wakeups run in ua_audio_irq_thread().
 * Returns true to wake that thread.
 * ---------------------------------------------------------------- */

bool ua_audio_irq(struct ua_device *ua, u32 status_lo, u32 status_hi)
{
	struct ua_audio *audio = &ua->audio;
	static unsigned int irq_count;

	if (atomic_read(&ua->shutdown))
		return false;

	if (!audio->transport_running)
		return false;

	/* Only process timer (BIT 30) and end-of-buffer (BIT 31) */
	if (!(status_hi & (BIT(30) | BIT(31))))
		return false;

	/* No BAR0 read here beyond the ISR's status registers */
	if (irq_count < 10)
		dev_dbg(&ua->pdev->dev, "audio_irq[%u]: hi=0x%08x\n",
			irq_count, status_hi);
	irq_count++;

	/*
	 * Re-arm the vectors that fired.  The original driver found them
	 * one-shot (enable bit cleared on fire).  The kext does not support
	 * that for the end of buffer: 0x47 is a fast vector its filter never
	 * masks, so this re-arm is a no-op write there.  Its one timer user
	 * registers 0x46 non-fast, so the filter masks it and the owner
	 * re-arms it after processing with EnableVector(0x46, 1)
	 * (ResetPeriodicTimerInterrupt).  Here the ISR has just acked both;
	 * no pending clear, which could drop an event that arrived since.
	 * One posted IMR_HI write each, no BAR0 read.
	 */
	if (status_hi & BIT(30))
		ua_enable_vector(ua, UA_IRQ_VEC_PERIODIC, false);
	if (status_hi & BIT(31))
		ua_enable_vector(ua, UA_IRQ_VEC_END_BUFFER, false);

	/* Counters and the watchdog's liveness stamp (one writer: here) */
	if (status_hi & BIT(30)) {
		audio->pstats.timer_irqs++;
		WRITE_ONCE(audio->timer_irq_jiffies, jiffies);
	}
	if (status_hi & BIT(31))
		audio->pstats.eob_irqs++;

	/*
	 * No snd_pcm_period_elapsed() here.  Streams on the hrtimer get
	 * their wakeups from it alone; this used to wake them a second time
	 * and read SAMPLE_POS in hard-IRQ context (pointer callback).  For
	 * IRQ-served streams (period_irq=1) hand the event to the thread.
	 */
	if (!READ_ONCE(audio->irq_subs))
		return false;
	if (status_hi & BIT(30))
		set_bit(UA_PIRQ_EV_TIMER, &audio->irq_events);
	if (status_hi & BIT(31))
		set_bit(UA_PIRQ_EV_EOB, &audio->irq_events);
	return true;
}

/* ----------------------------------------------------------------
 * ALSA Mixer Controls
 *
 * Exposes preamp (gain, 48V, pad, HiZ, phase, lowcut) and monitor
 * (volume, mute, dim) as standard ALSA kcontrols so that alsamixer,
 * amixer, and PulseAudio/PipeWire can see and adjust them.
 *
 * Cache strategy: write-through.  .put writes to HW first and only
 * updates the cache on success.  .get returns cached values (no HW read).
 * The cache follows the hardware too: ua_mixer_sync_readback() compares
 * each mixer readback with the previous one and, for what the front panel
 * changed, updates the cache and notifies the control (no write back).
 *
 * Cache-only, by design (nothing reaches the device): Digital Input Mode
 * (host-side route selection) and DSP Spanning Switch (a NOP param in the
 * kext).  Clock Mode is a read-only view of Clock Source.  Everything else
 * writes the mixer settings.
 * ---------------------------------------------------------------- */

/* Pack channel index + param_id into kcontrol->private_value */
#define UA_CTL_PACK(ch, param) (((ch) << 16) | (param))
#define UA_CTL_CH(pv)          ((pv) >> 16)
#define UA_CTL_PARAM(pv)       ((pv) & 0xFFFF)

static int ua_ctl_preamp_write(struct ua_device *ua, unsigned int ch,
			       u32 param_id, u32 value);

/* --- Preamp Gain (INTEGER dB, per-channel; settings: GainA/B/C triplet) --- */

/* 1 dB steps, 10..65 dB (GainA/GainB = dB-10 up to 54, GainC = dB-9 up to 56) */
static const DECLARE_TLV_DB_SCALE(preamp_gain_tlv,
				  UA_PREAMP_GAIN_MIN_DB * 100, 100, 0);

static int ua_preamp_gain_info(struct snd_kcontrol *kctl,
			       struct snd_ctl_elem_info *info)
{
	info->type = SNDRV_CTL_ELEM_TYPE_INTEGER;
	info->count = 1;
	info->value.integer.min = UA_PREAMP_GAIN_MIN_DB;
	info->value.integer.max = UA_PREAMP_GAIN_MAX_DB;
	info->value.integer.step = 1;
	return 0;
}

static int ua_preamp_gain_get(struct snd_kcontrol *kctl,
			      struct snd_ctl_elem_value *val)
{
	struct ua_device *ua = snd_kcontrol_chip(kctl);
	unsigned int ch = UA_CTL_CH(kctl->private_value);

	val->value.integer.value[0] =
		min_t(int, max_t(int, ua->audio.preamp[ch].gain,
				 UA_PREAMP_GAIN_MIN_DB), UA_PREAMP_GAIN_MAX_DB);
	return 0;
}

static int ua_preamp_gain_put(struct snd_kcontrol *kctl,
			      struct snd_ctl_elem_value *val)
{
	struct ua_device *ua = snd_kcontrol_chip(kctl);
	unsigned int ch = UA_CTL_CH(kctl->private_value);
	int newval = val->value.integer.value[0];
	unsigned int a, c;
	int ret;

	if (newval < UA_PREAMP_GAIN_MIN_DB || newval > UA_PREAMP_GAIN_MAX_DB)
		return -EINVAL;

	if (newval == ua->audio.preamp[ch].gain)
		return 0;

	/*
	 * The Mixer Engine's triplet: GainA and GainB are dB-10, GainC (the
	 * field the readback reports) dB-9.  One lock hold, so the three go
	 * out in the same mailbox commit.
	 */
	a = newval - 10;
	c = newval - 9;
	mutex_lock(&ua->lock);
	ret = ua_preamp_write_locked(ua, ch, UA_PREAMP_PARAM_GAIN_A, a);
	if (!ret)
		ret = ua_preamp_write_locked(ua, ch, UA_PREAMP_PARAM_GAIN_B, a);
	if (!ret)
		ret = ua_preamp_write_locked(ua, ch, UA_PREAMP_PARAM_GAIN_C, c);
	mutex_unlock(&ua->lock);
	if (ret)
		return ret;

	ua->audio.preamp[ch].gain = newval;
	return 1;  /* value changed */
}

/* --- Preamp Switch (BOOLEAN, per-channel, shared info) --- */

static int ua_preamp_switch_info(struct snd_kcontrol *kctl,
				 struct snd_ctl_elem_info *info)
{
	info->type = SNDRV_CTL_ELEM_TYPE_BOOLEAN;
	info->count = 1;
	info->value.integer.min = 0;
	info->value.integer.max = 1;
	return 0;
}

/* Helper: read the correct cache field based on param_id */
static bool ua_preamp_switch_cached(struct ua_preamp_state *st, u32 param)
{
	switch (param) {
	case UA_ARM_PARAM_48V:    return st->phantom;
	case UA_ARM_PARAM_PAD:    return st->pad;
	case UA_ARM_PARAM_PHASE:  return st->phase;
	case UA_ARM_PARAM_LOWCUT: return st->lowcut;
	default:                  return false;
	}
}

/* Helper: update the correct cache field */
static void ua_preamp_switch_set_cache(struct ua_preamp_state *st,
				       u32 param, bool on)
{
	switch (param) {
	case UA_ARM_PARAM_48V:    st->phantom = on; break;
	case UA_ARM_PARAM_PAD:    st->pad = on;     break;
	case UA_ARM_PARAM_PHASE:  st->phase = on;   break;
	case UA_ARM_PARAM_LOWCUT: st->lowcut = on;  break;
	}
}

static int ua_preamp_switch_get(struct snd_kcontrol *kctl,
				struct snd_ctl_elem_value *val)
{
	struct ua_device *ua = snd_kcontrol_chip(kctl);
	unsigned int ch = UA_CTL_CH(kctl->private_value);
	u32 param = UA_CTL_PARAM(kctl->private_value);

	val->value.integer.value[0] =
		ua_preamp_switch_cached(&ua->audio.preamp[ch], param) ? 1 : 0;
	return 0;
}

static int ua_preamp_switch_put(struct snd_kcontrol *kctl,
				struct snd_ctl_elem_value *val)
{
	struct ua_device *ua = snd_kcontrol_chip(kctl);
	unsigned int ch = UA_CTL_CH(kctl->private_value);
	u32 param = UA_CTL_PARAM(kctl->private_value);
	bool newval = !!val->value.integer.value[0];
	int ret;

	if (newval == ua_preamp_switch_cached(&ua->audio.preamp[ch], param))
		return 0;

	ret = ua_ctl_preamp_write(ua, ch, param, newval);
	if (ret)
		return ret;

	ua_preamp_switch_set_cache(&ua->audio.preamp[ch], param, newval);
	return 1;
}

/* Preamp flag write (settings 0/12), under ua->lock like the monitor ones */
static int ua_ctl_preamp_write(struct ua_device *ua, unsigned int ch,
			       u32 param_id, u32 value)
{
	int ret;

	mutex_lock(&ua->lock);
	ret = ua_preamp_write_locked(ua, ch, param_id, value);
	mutex_unlock(&ua->lock);
	return ret;
}

/*
 * ALSA .put callbacks run from the control ioctl without ua->lock, but
 * ua_monitor_set_param() merges into the cached mixer settings that the
 * DSP service loop flushes under ua->lock.  Take it here.
 */
static int ua_ctl_monitor_set_param(struct ua_device *ua, unsigned int ch_idx,
				    u32 param_id, u32 value)
{
	int ret;

	mutex_lock(&ua->lock);
	ret = ua_monitor_set_param(ua, ch_idx, param_id, value);
	mutex_unlock(&ua->lock);
	return ret;
}

/* --- Monitor Volume (INTEGER with dB scale) --- */

/* raw = 192 + (dB × 2): 0 = -96 dB (mute), 192 = 0 dB (max) */
static const DECLARE_TLV_DB_SCALE(monitor_vol_tlv, -9600, 50, 1);

static int ua_monitor_vol_info(struct snd_kcontrol *kctl,
			       struct snd_ctl_elem_info *info)
{
	info->type = SNDRV_CTL_ELEM_TYPE_INTEGER;
	info->count = 1;
	info->value.integer.min = 0;
	info->value.integer.max = 192;  /* 0xC0 = 0 dB */
	info->value.integer.step = 1;
	return 0;
}

static int ua_monitor_vol_get(struct snd_kcontrol *kctl,
			      struct snd_ctl_elem_value *val)
{
	struct ua_device *ua = snd_kcontrol_chip(kctl);

	val->value.integer.value[0] = ua->audio.monitor.level;
	return 0;
}

static int ua_monitor_vol_put(struct snd_kcontrol *kctl,
			      struct snd_ctl_elem_value *val)
{
	struct ua_device *ua = snd_kcontrol_chip(kctl);
	int newval = val->value.integer.value[0];
	int ret;

	if (newval < 0 || newval > 192)
		return -EINVAL;

	if (newval == ua->audio.monitor.level)
		return 0;

	ret = ua_ctl_monitor_set_param(ua, 1, UA_MON_PARAM_LEVEL, (u32)newval);
	if (ret)
		return ret;

	ua->audio.monitor.level = newval;
	return 1;
}

/*
 * --- Headphone 1/2 Volume (INTEGER, same raw scale as the monitor) ---
 *
 * HP1 is setting[2] bits 15:8 (param 0x02, readback word 2 15:8).  HP2 is
 * readback only (word 7 7:0): its write param is not confirmed on the
 * hardware, so the control is read-only.  The knobs apply themselves.
 */

static int ua_hp_vol_get(struct snd_kcontrol *kctl,
			 struct snd_ctl_elem_value *val)
{
	struct ua_device *ua = snd_kcontrol_chip(kctl);

	val->value.integer.value[0] =
		(UA_CTL_PARAM(kctl->private_value) == UA_MON_PARAM_HP1_VOL) ?
		ua->audio.monitor.hp1_vol : ua->audio.monitor.hp2_vol;
	return 0;
}

static int ua_hp_vol_put(struct snd_kcontrol *kctl,
			 struct snd_ctl_elem_value *val)
{
	struct ua_device *ua = snd_kcontrol_chip(kctl);
	int newval = val->value.integer.value[0];
	int ret;

	if (newval < 0 || newval > 192)
		return -EINVAL;

	if (newval == ua->audio.monitor.hp1_vol)
		return 0;

	ret = ua_ctl_monitor_set_param(ua, 1, UA_MON_PARAM_HP1_VOL,
				       (u32)newval);
	if (ret)
		return ret;

	ua->audio.monitor.hp1_vol = newval;
	return 1;
}

/* --- HiZ detect (BOOLEAN, read-only: readback word 0 bit 24 + input) --- */

static int ua_hiz_get(struct snd_kcontrol *kctl,
		      struct snd_ctl_elem_value *val)
{
	struct ua_device *ua = snd_kcontrol_chip(kctl);
	unsigned int ch = UA_CTL_CH(kctl->private_value);

	val->value.integer.value[0] = ua->audio.preamp[ch].hiz ? 1 : 0;
	return 0;
}

/* --- Monitor Mute (BOOLEAN, inverted: ALSA 1=unmuted → HW 0) --- */

static int ua_monitor_mute_get(struct snd_kcontrol *kctl,
			       struct snd_ctl_elem_value *val)
{
	struct ua_device *ua = snd_kcontrol_chip(kctl);

	/* ALSA convention: Switch 1 = unmuted, 0 = muted */
	val->value.integer.value[0] =
		(ua->audio.monitor.mute == UA_MON_MUTE_OFF) ? 1 : 0;
	return 0;
}

static int ua_monitor_mute_put(struct snd_kcontrol *kctl,
			       struct snd_ctl_elem_value *val)
{
	struct ua_device *ua = snd_kcontrol_chip(kctl);
	bool unmuted = !!val->value.integer.value[0];
	int hw_val = unmuted ? UA_MON_MUTE_OFF : UA_MON_MUTE_ON;
	int ret;

	if (hw_val == ua->audio.monitor.mute)
		return 0;

	/* Param 0x03 carries mute and mono: keep the mono bit */
	ret = ua_ctl_monitor_set_param(ua, 0, UA_MON_PARAM_MUTE,
				       hw_val | (ua->audio.monitor.mono ?
						 UA_MON_MONO : 0));
	if (ret)
		return ret;

	ua->audio.monitor.mute = hw_val;
	return 1;
}

/* --- Monitor Dim (BOOLEAN) --- */

static int ua_monitor_dim_get(struct snd_kcontrol *kctl,
			      struct snd_ctl_elem_value *val)
{
	struct ua_device *ua = snd_kcontrol_chip(kctl);

	val->value.integer.value[0] = ua->audio.monitor.dim ? 1 : 0;
	return 0;
}

static int ua_monitor_dim_put(struct snd_kcontrol *kctl,
			      struct snd_ctl_elem_value *val)
{
	struct ua_device *ua = snd_kcontrol_chip(kctl);
	bool newval = !!val->value.integer.value[0];
	int ret;

	if (newval == ua->audio.monitor.dim)
		return 0;

	ret = ua_ctl_monitor_set_param(ua, 0, UA_MON_PARAM_DIM, newval ? 1 : 0);
	if (ret)
		return ret;

	ua->audio.monitor.dim = newval;
	return 1;
}

/* --- Preamp Input Select (ENUMERATED: Mic/Line, per-channel; readback +0 = Line) --- */

static const char * const ua_input_select_names[] = { "Mic", "Line" };

static int ua_input_select_info(struct snd_kcontrol *kctl,
				struct snd_ctl_elem_info *info)
{
	return snd_ctl_enum_info(info, 1, ARRAY_SIZE(ua_input_select_names),
				 ua_input_select_names);
}

static int ua_input_select_get(struct snd_kcontrol *kctl,
			       struct snd_ctl_elem_value *val)
{
	struct ua_device *ua = snd_kcontrol_chip(kctl);
	unsigned int ch = UA_CTL_CH(kctl->private_value);

	val->value.enumerated.item[0] = ua->audio.preamp[ch].mic_line ? 1 : 0;
	return 0;
}

static int ua_input_select_put(struct snd_kcontrol *kctl,
			       struct snd_ctl_elem_value *val)
{
	struct ua_device *ua = snd_kcontrol_chip(kctl);
	unsigned int ch = UA_CTL_CH(kctl->private_value);
	unsigned int idx = val->value.enumerated.item[0];
	bool newval;
	int ret;

	if (idx >= ARRAY_SIZE(ua_input_select_names))
		return -EINVAL;

	newval = !!idx;
	if (newval == ua->audio.preamp[ch].mic_line)
		return 0;

	ret = ua_ctl_preamp_write(ua, ch, UA_PREAMP_PARAM_MIC_LINE, newval);
	if (ret)
		return ret;

	ua->audio.preamp[ch].mic_line = newval;
	return 1;
}

/* --- Monitor Mono Switch (BOOLEAN, known routing: setting[2] bit 17) --- */

static int ua_monitor_mono_get(struct snd_kcontrol *kctl,
			       struct snd_ctl_elem_value *val)
{
	struct ua_device *ua = snd_kcontrol_chip(kctl);

	val->value.integer.value[0] = ua->audio.monitor.mono ? 1 : 0;
	return 0;
}

static int ua_monitor_mono_put(struct snd_kcontrol *kctl,
			       struct snd_ctl_elem_value *val)
{
	struct ua_device *ua = snd_kcontrol_chip(kctl);
	bool newval = !!val->value.integer.value[0];
	int ret;

	if (newval == ua->audio.monitor.mono)
		return 0;

	/*
	 * VERIFIED 2026-02-20: mono is param 0x03 (the mute's param), value
	 * bit 0.  Keep the mute bit: they are independent on the hardware.
	 */
	ret = ua_ctl_monitor_set_param(ua, 0, UA_MON_PARAM_MUTE,
				       (newval ? UA_MON_MONO : 0) |
				       (ua->audio.monitor.mute == UA_MON_MUTE_ON ?
					UA_MON_MUTE_ON : 0));
	if (ret)
		return ret;

	ua->audio.monitor.mono = newval;
	return 1;
}

/* --- Monitor Source (ENUMERATED: MIX/CUE 1/CUE 2) --- */

static const char * const ua_monitor_source_names[] = {
	"MIX", "CUE 1", "CUE 2",
};

static int ua_monitor_source_info(struct snd_kcontrol *kctl,
				  struct snd_ctl_elem_info *info)
{
	return snd_ctl_enum_info(info, 1, ARRAY_SIZE(ua_monitor_source_names),
				 ua_monitor_source_names);
}

static int ua_monitor_source_get(struct snd_kcontrol *kctl,
				 struct snd_ctl_elem_value *val)
{
	struct ua_device *ua = snd_kcontrol_chip(kctl);

	/* The cache can hold any 3-bit value the daemon sent */
	val->value.enumerated.item[0] =
		(unsigned int)ua->audio.monitor.source <
		ARRAY_SIZE(ua_monitor_source_names) ?
		ua->audio.monitor.source : 0;
	return 0;
}

static int ua_monitor_source_put(struct snd_kcontrol *kctl,
				 struct snd_ctl_elem_value *val)
{
	struct ua_device *ua = snd_kcontrol_chip(kctl);
	unsigned int idx = val->value.enumerated.item[0];
	int ret;

	if (idx >= ARRAY_SIZE(ua_monitor_source_names))
		return -EINVAL;

	if ((int)idx == ua->audio.monitor.source)
		return 0;

	/* DTrace-verified 2026-02-22: Monitor Source uses ch_idx=1 */
	ret = ua_ctl_monitor_set_param(ua, 1, UA_MON_PARAM_SOURCE, idx);
	if (ret)
		return ret;

	ua->audio.monitor.source = idx;
	return 1;
}

/* --- Headphone Source (ENUMERATED: CUE 1/CUE 2) --- */

static const char * const ua_hp_source_names[] = { "CUE 1", "CUE 2" };

static int ua_hp_source_info(struct snd_kcontrol *kctl,
			     struct snd_ctl_elem_info *info)
{
	return snd_ctl_enum_info(info, 1, ARRAY_SIZE(ua_hp_source_names),
				 ua_hp_source_names);
}

static int ua_hp_source_get(struct snd_kcontrol *kctl,
			    struct snd_ctl_elem_value *val)
{
	struct ua_device *ua = snd_kcontrol_chip(kctl);
	u32 param = UA_CTL_PARAM(kctl->private_value);

	int src = (param == UA_MON_PARAM_HP1_SOURCE) ?
		  ua->audio.monitor.hp1_source : ua->audio.monitor.hp2_source;

	val->value.enumerated.item[0] =
		(unsigned int)src < ARRAY_SIZE(ua_hp_source_names) ? src : 0;
	return 0;
}

static int ua_hp_source_put(struct snd_kcontrol *kctl,
			    struct snd_ctl_elem_value *val)
{
	struct ua_device *ua = snd_kcontrol_chip(kctl);
	u32 param = UA_CTL_PARAM(kctl->private_value);
	unsigned int idx = val->value.enumerated.item[0];
	int *cache;
	int ret;

	if (idx >= ARRAY_SIZE(ua_hp_source_names))
		return -EINVAL;

	cache = (param == UA_MON_PARAM_HP1_SOURCE) ?
		&ua->audio.monitor.hp1_source :
		&ua->audio.monitor.hp2_source;

	if ((int)idx == *cache)
		return 0;

	ret = ua_ctl_monitor_set_param(ua, 1, param, idx);
	if (ret)
		return ret;

	*cache = idx;
	return 1;
}

/* --- Monitor Dim Level (ENUMERATED: 7 stepped levels) --- */

static const char * const ua_dim_level_names[] = {
	"-9 dB", "-17 dB", "-26 dB", "-34 dB", "-43 dB", "-51 dB", "-60 dB",
};

static int ua_dim_level_info(struct snd_kcontrol *kctl,
			     struct snd_ctl_elem_info *info)
{
	return snd_ctl_enum_info(info, 1, ARRAY_SIZE(ua_dim_level_names),
				 ua_dim_level_names);
}

static int ua_dim_level_get(struct snd_kcontrol *kctl,
			    struct snd_ctl_elem_value *val)
{
	struct ua_device *ua = snd_kcontrol_chip(kctl);
	int lvl = ua->audio.monitor.dim_level;

	/* Hardware uses 1-7, ALSA enum uses 0-6 */
	val->value.enumerated.item[0] = (lvl >= 1 && lvl <= 7) ? lvl - 1 : 0;
	return 0;
}

static int ua_dim_level_put(struct snd_kcontrol *kctl,
			    struct snd_ctl_elem_value *val)
{
	struct ua_device *ua = snd_kcontrol_chip(kctl);
	unsigned int idx = val->value.enumerated.item[0];
	int hw_val;
	int ret;

	if (idx >= ARRAY_SIZE(ua_dim_level_names))
		return -EINVAL;

	hw_val = idx + 1;  /* ALSA 0-6 → HW 1-7 */
	if (hw_val == ua->audio.monitor.dim_level)
		return 0;

	ret = ua_ctl_monitor_set_param(ua, 0, UA_MON_PARAM_DIM_LEVEL, hw_val);
	if (ret)
		return ret;

	ua->audio.monitor.dim_level = hw_val;
	return 1;
}

/* --- Monitor Talkback Switch (BOOLEAN) --- */

static int ua_monitor_talkback_get(struct snd_kcontrol *kctl,
				   struct snd_ctl_elem_value *val)
{
	struct ua_device *ua = snd_kcontrol_chip(kctl);

	val->value.integer.value[0] = ua->audio.monitor.talkback ? 1 : 0;
	return 0;
}

static int ua_monitor_talkback_put(struct snd_kcontrol *kctl,
				   struct snd_ctl_elem_value *val)
{
	struct ua_device *ua = snd_kcontrol_chip(kctl);
	bool newval = !!val->value.integer.value[0];
	int ret;

	if (newval == ua->audio.monitor.talkback)
		return 0;

	/* DTrace-verified 2026-02-22: ON uses ch_idx=1, OFF uses ch_idx=9 */
	ret = ua_ctl_monitor_set_param(ua, newval ? 1 : 9,
				       UA_MON_PARAM_TALKBACK,
				       newval ? 1 : 0);
	if (ret)
		return ret;

	ua->audio.monitor.talkback = newval;
	return 1;
}

/* --- Digital Output Mode (ENUMERATED: S/PDIF / ADAT) --- */

static const char * const ua_digital_mode_names[] = { "S/PDIF", "ADAT" };

static int ua_digital_mode_info(struct snd_kcontrol *kctl,
				struct snd_ctl_elem_info *info)
{
	return snd_ctl_enum_info(info, 1, ARRAY_SIZE(ua_digital_mode_names),
				 ua_digital_mode_names);
}

static int ua_digital_mode_get(struct snd_kcontrol *kctl,
			       struct snd_ctl_elem_value *val)
{
	struct ua_device *ua = snd_kcontrol_chip(kctl);

	/* HW: 0=S/PDIF, 8=ADAT → ALSA enum: 0=S/PDIF, 1=ADAT */
	val->value.enumerated.item[0] =
		(ua->audio.monitor.digital_mode == 8) ? 1 : 0;
	return 0;
}

static int ua_digital_mode_put(struct snd_kcontrol *kctl,
			       struct snd_ctl_elem_value *val)
{
	struct ua_device *ua = snd_kcontrol_chip(kctl);
	unsigned int idx = val->value.enumerated.item[0];
	int hw_val;
	int ret;

	if (idx >= ARRAY_SIZE(ua_digital_mode_names))
		return -EINVAL;

	hw_val = idx ? 8 : 0;  /* ALSA 0→HW 0 (S/PDIF), ALSA 1→HW 8 (ADAT) */

	/*
	 * Under ua->lock: ua_monitor_set_param() requires it, and the IO
	 * map update below may change the layout (io_map=1).
	 */
	mutex_lock(&ua->lock);
	if (hw_val == ua->audio.monitor.digital_mode) {
		mutex_unlock(&ua->lock);
		return 0;
	}

	ret = ua_monitor_set_param(ua, 0, UA_MON_PARAM_DIGITAL_MODE, hw_val);
	if (!ret) {
		ua->audio.monitor.digital_mode = hw_val;
		/* The TOSLink output type picks playback route channels */
		ua_io_map_digital_mode_changed(ua);
	}
	mutex_unlock(&ua->lock);

	return ret ? ret : 1;
}

/*
 * --- Digital Input Mode (ENUMERATED: S/PDIF / ADAT; io_map=1) ---
 *
 * The TOSLink input type: it picks the record route channels on the x4,
 * x8p and 8p, and both directions' on the Twin family (ua_route_mode()).
 * Host side only, like the Mixer Engine's GlobalSettings value — no
 * device parameter for it is known, so nothing is written to the
 * hardware; the firmware descriptor lists the S/PDIF and the ADAT input
 * slots in either mode.  Registered with io_map=1 on models whose route
 * lists depend on it.
 */
static int ua_digital_in_mode_get(struct snd_kcontrol *kctl,
				  struct snd_ctl_elem_value *val)
{
	struct ua_device *ua = snd_kcontrol_chip(kctl);

	val->value.enumerated.item[0] =
		ua->audio.dig_in_mode == UA_DIG_ADAT ? 1 : 0;
	return 0;
}

static int ua_digital_in_mode_put(struct snd_kcontrol *kctl,
				  struct snd_ctl_elem_value *val)
{
	struct ua_device *ua = snd_kcontrol_chip(kctl);
	unsigned int idx = val->value.enumerated.item[0];
	unsigned int mode;

	if (idx >= ARRAY_SIZE(ua_digital_mode_names))
		return -EINVAL;
	mode = idx ? UA_DIG_ADAT : UA_DIG_SPDIF;

	mutex_lock(&ua->lock);
	if (mode == ua->audio.dig_in_mode) {
		mutex_unlock(&ua->lock);
		return 0;
	}
	ua->audio.dig_in_mode = mode;
	ua_io_map_digital_mode_changed(ua);
	mutex_unlock(&ua->lock);

	return 1;
}

/* --- Output Reference Level (ENUMERATED: +4 dBu / -10 dBV) --- */

static const char * const ua_output_ref_names[] = { "+4 dBu", "-10 dBV" };

static int ua_output_ref_info(struct snd_kcontrol *kctl,
			      struct snd_ctl_elem_info *info)
{
	return snd_ctl_enum_info(info, 1, ARRAY_SIZE(ua_output_ref_names),
				 ua_output_ref_names);
}

static int ua_output_ref_get(struct snd_kcontrol *kctl,
			     struct snd_ctl_elem_value *val)
{
	struct ua_device *ua = snd_kcontrol_chip(kctl);

	val->value.enumerated.item[0] = ua->audio.monitor.output_ref ? 1 : 0;
	return 0;
}

static int ua_output_ref_put(struct snd_kcontrol *kctl,
			     struct snd_ctl_elem_value *val)
{
	struct ua_device *ua = snd_kcontrol_chip(kctl);
	unsigned int idx = val->value.enumerated.item[0];
	int ret;

	if (idx >= ARRAY_SIZE(ua_output_ref_names))
		return -EINVAL;

	if ((int)idx == ua->audio.monitor.output_ref)
		return 0;

	/* VERIFIED 2026-02-20: OutputRef uses ch_idx=1 (was 0) */
	ret = ua_ctl_monitor_set_param(ua, 1, UA_MON_PARAM_OUTPUT_REF, idx);
	if (ret)
		return ret;

	ua->audio.monitor.output_ref = idx;
	return 1;
}

/*
 * --- Clock Mode (ENUMERATED: Internal/External, read-only) ---
 *
 * This used to be a cache-only control that wrote nothing: whether the
 * clock is the internal one or an external one is just the Clock Source.
 * This is that, read-only; set Clock Source.
 */

static const char * const ua_clock_mode_names[] = { "Internal", "External" };

static int ua_clock_mode_info(struct snd_kcontrol *kctl,
			      struct snd_ctl_elem_info *info)
{
	return snd_ctl_enum_info(info, 1, ARRAY_SIZE(ua_clock_mode_names),
				 ua_clock_mode_names);
}

static int ua_clock_mode_get(struct snd_kcontrol *kctl,
			     struct snd_ctl_elem_value *val)
{
	struct ua_device *ua = snd_kcontrol_chip(kctl);

	val->value.enumerated.item[0] =
		READ_ONCE(ua->audio.clock_source) != UA_CLOCK_INTERNAL;
	return 0;
}

/* --- CUE Mirror Source (ENUMERATED, per-CUE) --- */

static const char * const ua_mirror_source_names[] = {
	"None", "S/PDIF", "Line 1-2", "Line 3-4",
	"ADAT 1-2", "ADAT 3-4", "ADAT 5-6", "ADAT 7-8",
};

/*
 * Map ALSA enum index → hardware value.
 * NONE=0xFFFFFFFF, SPDIF=6, LINE1-2=8, LINE3-4=10,
 * ADAT1-2=16, ADAT3-4=18, ADAT5-6=20, ADAT7-8=22
 */
static const u32 ua_mirror_hw_values[] = {
	0xFFFFFFFF, 6, 8, 10, 16, 18, 20, 22,
};

static int ua_mirror_source_info(struct snd_kcontrol *kctl,
				 struct snd_ctl_elem_info *info)
{
	return snd_ctl_enum_info(info, 1, ARRAY_SIZE(ua_mirror_source_names),
				 ua_mirror_source_names);
}

/* Convert HW value back to enum index */
static unsigned int ua_mirror_hw_to_idx(int hw_val)
{
	unsigned int i;

	for (i = 0; i < ARRAY_SIZE(ua_mirror_hw_values); i++) {
		if ((u32)hw_val == ua_mirror_hw_values[i])
			return i;
	}
	return 0; /* default to None */
}

static int ua_mirror_source_get(struct snd_kcontrol *kctl,
				struct snd_ctl_elem_value *val)
{
	struct ua_device *ua = snd_kcontrol_chip(kctl);
	u32 param = UA_CTL_PARAM(kctl->private_value);
	int hw_val;

	hw_val = (param == UA_MON_PARAM_CUE1_MIRROR) ?
		 ua->audio.monitor.cue1_mirror :
		 ua->audio.monitor.cue2_mirror;

	val->value.enumerated.item[0] = ua_mirror_hw_to_idx(hw_val);
	return 0;
}

static int ua_mirror_source_put(struct snd_kcontrol *kctl,
				struct snd_ctl_elem_value *val)
{
	struct ua_device *ua = snd_kcontrol_chip(kctl);
	u32 param = UA_CTL_PARAM(kctl->private_value);
	unsigned int idx = val->value.enumerated.item[0];
	int *cache;
	u32 hw_val;
	int ret;

	if (idx >= ARRAY_SIZE(ua_mirror_source_names))
		return -EINVAL;

	hw_val = ua_mirror_hw_values[idx];
	cache = (param == UA_MON_PARAM_CUE1_MIRROR) ?
		&ua->audio.monitor.cue1_mirror :
		&ua->audio.monitor.cue2_mirror;

	if ((u32)*cache == hw_val)
		return 0;

	/* ch_idx=9 for mirror params */
	ret = ua_ctl_monitor_set_param(ua, 9, param, hw_val);
	if (ret)
		return ret;

	*cache = (int)hw_val;
	return 1;
}

/* --- CUE Mirror Enable (BOOLEAN, per-CUE) --- */

static int ua_mirror_enable_get(struct snd_kcontrol *kctl,
				struct snd_ctl_elem_value *val)
{
	struct ua_device *ua = snd_kcontrol_chip(kctl);
	u32 param = UA_CTL_PARAM(kctl->private_value);

	val->value.integer.value[0] =
		(param == UA_MON_PARAM_MIRROR_ENABLE_A) ?
		(ua->audio.monitor.cue1_mirror_en ? 1 : 0) :
		(ua->audio.monitor.cue2_mirror_en ? 1 : 0);
	return 0;
}

static int ua_mirror_enable_put(struct snd_kcontrol *kctl,
				struct snd_ctl_elem_value *val)
{
	struct ua_device *ua = snd_kcontrol_chip(kctl);
	u32 param = UA_CTL_PARAM(kctl->private_value);
	bool newval = !!val->value.integer.value[0];
	bool *cache;
	int ret;

	cache = (param == UA_MON_PARAM_MIRROR_ENABLE_A) ?
		&ua->audio.monitor.cue1_mirror_en :
		&ua->audio.monitor.cue2_mirror_en;

	if (newval == *cache)
		return 0;

	/* ch_idx=9 for mirror params */
	ret = ua_ctl_monitor_set_param(ua, 9, param, newval ? 1 : 0);
	if (ret)
		return ret;

	*cache = newval;
	return 1;
}

/* --- Digital Mirror / MirrorsToDigital (BOOLEAN) --- */

static int ua_digital_mirror_get(struct snd_kcontrol *kctl,
				 struct snd_ctl_elem_value *val)
{
	struct ua_device *ua = snd_kcontrol_chip(kctl);

	val->value.integer.value[0] = ua->audio.monitor.digital_mirror ? 1 : 0;
	return 0;
}

static int ua_digital_mirror_put(struct snd_kcontrol *kctl,
				 struct snd_ctl_elem_value *val)
{
	struct ua_device *ua = snd_kcontrol_chip(kctl);
	bool newval = !!val->value.integer.value[0];
	int ret;

	if (newval == ua->audio.monitor.digital_mirror)
		return 0;

	/* ch_idx=9 for digital mirror */
	ret = ua_ctl_monitor_set_param(ua, 9, UA_MON_PARAM_DIGITAL_MIRROR,
				       newval ? 1 : 0);
	if (ret)
		return ret;

	ua->audio.monitor.digital_mirror = newval;
	return 1;
}

/* --- Identify Switch (BOOLEAN) --- */

static int ua_identify_get(struct snd_kcontrol *kctl,
			   struct snd_ctl_elem_value *val)
{
	struct ua_device *ua = snd_kcontrol_chip(kctl);

	val->value.integer.value[0] = ua->audio.monitor.identify ? 1 : 0;
	return 0;
}

static int ua_identify_put(struct snd_kcontrol *kctl,
			   struct snd_ctl_elem_value *val)
{
	struct ua_device *ua = snd_kcontrol_chip(kctl);
	bool newval = !!val->value.integer.value[0];
	int ret;

	if (newval == ua->audio.monitor.identify)
		return 0;

	/* ch_idx=0 for identify */
	ret = ua_ctl_monitor_set_param(ua, 0, UA_MON_PARAM_IDENTIFY,
				       newval ? 1 : 0);
	if (ret)
		return ret;

	ua->audio.monitor.identify = newval;
	return 1;
}

/* --- S/PDIF SRC Switch (BOOLEAN) --- */

static int ua_sr_convert_get(struct snd_kcontrol *kctl,
			     struct snd_ctl_elem_value *val)
{
	struct ua_device *ua = snd_kcontrol_chip(kctl);

	val->value.integer.value[0] = ua->audio.monitor.sr_convert ? 1 : 0;
	return 0;
}

static int ua_sr_convert_put(struct snd_kcontrol *kctl,
			     struct snd_ctl_elem_value *val)
{
	struct ua_device *ua = snd_kcontrol_chip(kctl);
	bool newval = !!val->value.integer.value[0];
	int ret;

	if (newval == ua->audio.monitor.sr_convert)
		return 0;

	/* ch_idx=1 for SR convert */
	ret = ua_ctl_monitor_set_param(ua, 1, UA_MON_PARAM_SR_CONVERT,
				       newval ? 1 : 0);
	if (ret)
		return ret;

	ua->audio.monitor.sr_convert = newval;
	return 1;
}

/* --- DSP Spanning Switch (BOOLEAN) --- */

static int ua_dsp_spanning_get(struct snd_kcontrol *kctl,
			       struct snd_ctl_elem_value *val)
{
	struct ua_device *ua = snd_kcontrol_chip(kctl);

	val->value.integer.value[0] = ua->audio.monitor.dsp_spanning ? 1 : 0;
	return 0;
}

static int ua_dsp_spanning_put(struct snd_kcontrol *kctl,
			       struct snd_ctl_elem_value *val)
{
	struct ua_device *ua = snd_kcontrol_chip(kctl);
	bool newval = !!val->value.integer.value[0];
	int ret;

	if (newval == ua->audio.monitor.dsp_spanning)
		return 0;

	/* ch_idx=0 for DSP spanning */
	ret = ua_ctl_monitor_set_param(ua, 0, UA_MON_PARAM_DSP_SPANNING,
				       newval ? 1 : 0);
	if (ret)
		return ret;

	ua->audio.monitor.dsp_spanning = newval;
	return 1;
}

/* --- CUE Mono Switch (BOOLEAN, per-CUE) --- */

static int ua_cue_mono_get(struct snd_kcontrol *kctl,
			   struct snd_ctl_elem_value *val)
{
	struct ua_device *ua = snd_kcontrol_chip(kctl);
	u32 param = UA_CTL_PARAM(kctl->private_value);

	val->value.integer.value[0] =
		(param == UA_MON_PARAM_CUE1_MONO) ?
		(ua->audio.monitor.cue1_mono ? 1 : 0) :
		(ua->audio.monitor.cue2_mono ? 1 : 0);
	return 0;
}

static int ua_cue_mono_put(struct snd_kcontrol *kctl,
			   struct snd_ctl_elem_value *val)
{
	struct ua_device *ua = snd_kcontrol_chip(kctl);
	u32 param = UA_CTL_PARAM(kctl->private_value);
	bool newval = !!val->value.integer.value[0];
	bool *cache;
	int ret;

	cache = (param == UA_MON_PARAM_CUE1_MONO) ?
		&ua->audio.monitor.cue1_mono :
		&ua->audio.monitor.cue2_mono;

	if (newval == *cache)
		return 0;

	/* ch_idx=0 for CUE mono */
	ret = ua_ctl_monitor_set_param(ua, 0, param, newval ? 1 : 0);
	if (ret)
		return ret;

	*cache = newval;
	return 1;
}

/* --- CUE Mix Switch (BOOLEAN, per-CUE, INVERTED: hw 0=on, 2=off) --- */

static int ua_cue_mix_get(struct snd_kcontrol *kctl,
			  struct snd_ctl_elem_value *val)
{
	struct ua_device *ua = snd_kcontrol_chip(kctl);
	u32 param = UA_CTL_PARAM(kctl->private_value);
	int hw_val;

	hw_val = (param == UA_MON_PARAM_CUE1_MIX) ?
		 ua->audio.monitor.cue1_mix :
		 ua->audio.monitor.cue2_mix;

	/* Inverted: hw 0=on → ALSA 1, hw 2=off → ALSA 0 */
	val->value.integer.value[0] = (hw_val == 0) ? 1 : 0;
	return 0;
}

static int ua_cue_mix_put(struct snd_kcontrol *kctl,
			  struct snd_ctl_elem_value *val)
{
	struct ua_device *ua = snd_kcontrol_chip(kctl);
	u32 param = UA_CTL_PARAM(kctl->private_value);
	bool alsa_on = !!val->value.integer.value[0];
	int hw_val = alsa_on ? 0 : 2;  /* ALSA 1→hw 0 (on), ALSA 0→hw 2 (off) */
	int *cache;
	int ret;

	cache = (param == UA_MON_PARAM_CUE1_MIX) ?
		&ua->audio.monitor.cue1_mix :
		&ua->audio.monitor.cue2_mix;

	if (hw_val == *cache)
		return 0;

	/* ch_idx=0 for CUE mix */
	ret = ua_ctl_monitor_set_param(ua, 0, param, (u32)hw_val);
	if (ret)
		return ret;

	*cache = hw_val;
	return 1;
}

/* --- Sample Rate (ENUMERATED) --- */

static const char * const ua_rate_names[] = {
	"44100", "48000", "88200", "96000", "176400", "192000",
};

static const unsigned int ua_rate_values[] = {
	44100, 48000, 88200, 96000, 176400, 192000,
};

static int ua_sample_rate_info(struct snd_kcontrol *kctl,
			       struct snd_ctl_elem_info *info)
{
	return snd_ctl_enum_info(info, 1, ARRAY_SIZE(ua_rate_names),
				 ua_rate_names);
}

static int ua_sample_rate_get(struct snd_kcontrol *kctl,
			      struct snd_ctl_elem_value *val)
{
	struct ua_device *ua = snd_kcontrol_chip(kctl);

	/* rate_index is 1-based (UA_RATE_44100=1), enum is 0-based */
	val->value.enumerated.item[0] =
		(ua->audio.rate_index >= 1 &&
		 ua->audio.rate_index <= ARRAY_SIZE(ua_rate_values)) ?
		ua->audio.rate_index - 1 : 0;
	return 0;
}

static int ua_sample_rate_put(struct snd_kcontrol *kctl,
			      struct snd_ctl_elem_value *val)
{
	struct ua_device *ua = snd_kcontrol_chip(kctl);
	unsigned int idx = val->value.enumerated.item[0];
	unsigned int rate;
	int ret;

	if (idx >= ARRAY_SIZE(ua_rate_values))
		return -EINVAL;

	rate = ua_rate_values[idx];
	if (rate == ua->audio.sample_rate)
		return 0;

	ret = ua_audio_set_clock(ua, rate);
	if (ret)
		return ret;

	return 1;
}

/* --- Clock Source (ENUMERATED) --- */

static const char * const ua_clock_source_names[] = {
	"Internal", "S/PDIF", "ADAT", "Word Clock",
};

/* clock_config source value for each ua_clock_source_names entry */
static const u8 ua_clock_source_values[] = {
	UA_CLOCK_INTERNAL, UA_CLOCK_SPDIF, UA_CLOCK_ADAT, UA_CLOCK_WORDCLOCK,
};

/*
 * clock_src_from_device: the enum is the device's list (ua_clock_list_build:
 * Internal first, then ascending, source 0xA hidden) once a connect has
 * read it; before that, or with the parameter off, the fixed list above.
 * Entry i of the device list has source clock_list_val[i].
 */
static int ua_clock_source_info(struct snd_kcontrol *kctl,
				struct snd_ctl_elem_info *info)
{
	struct ua_device *ua = snd_kcontrol_chip(kctl);
	struct ua_audio *audio = &ua->audio;
	char name[UA_CLOCK_NAME_LEN];
	unsigned int n, item;

	if (!ua_clock_list_active(audio))
		return snd_ctl_enum_info(info, 1,
					 ARRAY_SIZE(ua_clock_source_names),
					 ua_clock_source_names);

	spin_lock(&audio->clock_list_lock);
	n = audio->clock_list_n;
	item = min(info->value.enumerated.item, n ? n - 1 : 0);
	strscpy(name, audio->clock_list_name[item], sizeof(name));
	spin_unlock(&audio->clock_list_lock);

	info->type = SNDRV_CTL_ELEM_TYPE_ENUMERATED;
	info->count = 1;
	info->value.enumerated.items = n;
	info->value.enumerated.item = item;
	strscpy(info->value.enumerated.name, name,
		sizeof(info->value.enumerated.name));
	return 0;
}

static int ua_clock_source_get(struct snd_kcontrol *kctl,
			       struct snd_ctl_elem_value *val)
{
	struct ua_device *ua = snd_kcontrol_chip(kctl);
	struct ua_audio *audio = &ua->audio;
	unsigned int i;

	val->value.enumerated.item[0] = 0;
	if (ua_clock_list_active(audio)) {
		spin_lock(&audio->clock_list_lock);
		for (i = 0; i < audio->clock_list_n; i++)
			if (audio->clock_list_val[i] == audio->clock_source)
				val->value.enumerated.item[0] = i;
		spin_unlock(&audio->clock_list_lock);
		return 0;
	}
	for (i = 0; i < ARRAY_SIZE(ua_clock_source_values); i++)
		if (ua_clock_source_values[i] == ua->audio.clock_source)
			val->value.enumerated.item[0] = i;
	return 0;
}

static int ua_clock_source_put(struct snd_kcontrol *kctl,
			       struct snd_ctl_elem_value *val)
{
	struct ua_device *ua = snd_kcontrol_chip(kctl);
	unsigned int idx = val->value.enumerated.item[0];
	unsigned int src;
	int ret;

	if (ua_clock_list_active(&ua->audio)) {
		spin_lock(&ua->audio.clock_list_lock);
		if (idx >= ua->audio.clock_list_n) {
			spin_unlock(&ua->audio.clock_list_lock);
			return -EINVAL;
		}
		src = ua->audio.clock_list_val[idx];
		spin_unlock(&ua->audio.clock_list_lock);
	} else {
		if (idx >= ARRAY_SIZE(ua_clock_source_values))
			return -EINVAL;
		src = ua_clock_source_values[idx];
	}
	if (src == READ_ONCE(ua->audio.clock_source))
		return 0;

	/* Commits the source under ua->lock once the device acknowledged */
	ret = ua_audio_set_clock_source(ua, src);
	if (ret)
		return ret;

	return 1;
}

/* --- Clock Locked (BOOLEAN, read-only: (0xC07C & 3) == 1) --- */

static int ua_clock_locked_get(struct snd_kcontrol *kctl,
			       struct snd_ctl_elem_value *val)
{
	struct ua_device *ua = snd_kcontrol_chip(kctl);

	val->value.integer.value[0] = READ_ONCE(ua->audio.clock_locked) ? 1 : 0;
	return 0;
}

/* --- Clock control ALSA notification helper --- */

void ua_notify_clock_controls(struct ua_device *ua)
{
	ua_ctl_notify_name(ua, "Sample Rate");
	ua_ctl_notify_name(ua, "Clock Source");
	ua_ctl_notify_name(ua, "Clock Mode");
	ua_ctl_notify_name(ua, "Clock Locked");
}

/*
 * Every control goes through here so a notification can carry the numid
 * (alsa-lib's hctl matches events by numid, an id with only a name is
 * dropped) and the readback sync can find it by name.
 */
static int ua_ctl_register(struct ua_device *ua, struct snd_kcontrol *kctl)
{
	struct ua_audio *audio = &ua->audio;
	int ret = snd_ctl_add(audio->card, kctl);

	if (!ret && audio->num_kctls < UA_MAX_KCTLS)
		audio->kctls[audio->num_kctls++] = kctl;
	return ret;
}

/* Value-changed event for the control called @name, if there is one */
void ua_ctl_notify_name(struct ua_device *ua, const char *name)
{
	struct ua_audio *audio = &ua->audio;
	unsigned int i;

	if (!audio->card)
		return;
	for (i = 0; i < audio->num_kctls; i++) {
		if (!strcmp(audio->kctls[i]->id.name, name)) {
			snd_ctl_notify(audio->card, SNDRV_CTL_EVENT_MASK_VALUE,
				       &audio->kctls[i]->id);
			return;
		}
	}
}

/* Info-changed event: the control's item list changed */
static void ua_ctl_notify_info_name(struct ua_device *ua, const char *name)
{
	struct ua_audio *audio = &ua->audio;
	unsigned int i;

	if (!audio->card)
		return;
	for (i = 0; i < audio->num_kctls; i++) {
		if (!strcmp(audio->kctls[i]->id.name, name)) {
			snd_ctl_notify(audio->card, SNDRV_CTL_EVENT_MASK_INFO,
				       &audio->kctls[i]->id);
			return;
		}
	}
}

static void ua_sync_bool(struct ua_device *ua, bool *cache, bool val,
			 const char *name)
{
	if (*cache == val)
		return;
	*cache = val;
	ua_ctl_notify_name(ua, name);
}

static void ua_sync_int(struct ua_device *ua, int *cache, int val,
			const char *name)
{
	if (*cache == val)
		return;
	*cache = val;
	ua_ctl_notify_name(ua, name);
}

/**
 * ua_mixer_sync_readback - follow the hardware in the ALSA control cache
 * @ua: device (caller holds ua->lock; ua->mixer_rb_data is fresh)
 *
 * The front panel (and the monitor knob, which applies itself; the host
 * only mirrors it) changes the device, and the device reports it in the
 * readback.  For each field that differs from the readback this ran on
 * last time, set the cached value and notify the control.  Nothing is
 * written back to the hardware.  Comparing with the previous readback
 * rather than with the cache keeps a stale readback right after a put (the
 * cache is already new, the readback not yet) from undoing it; the first
 * readback seeds everything.
 *
 * Readback layout (hardware-verified on the x4, 2026-10-10):
 *   word 0  6 bits per input (+0 Line, +1 PAD, +2 Link, +3 48V, +4 LowCut,
 *           +5 Phase); bit 24/25 HiZ detect of input 1/2
 *   word 2  monitor volume 7:0 (192 + 2*dB), HP1 volume 15:8, mute 16,
 *           mono 17, dim 31
 *   word 3  GainC fields (UA_RB_PREAMP_GAINC), dB = GainC + 9
 *   word 6  bit 8 talkback
 *   word 7  HP2 volume 7:0
 * The clock lock (word 6 bit 31, valid when bit 30) is not a control here.
 */
void ua_mixer_sync_readback(struct ua_device *ua)
{
	struct ua_audio *audio = &ua->audio;
	const u32 *rb = ua->mixer_rb_data;
	u32 *old = audio->rb_synced;
	u32 chg[UA_MIXER_RB_WORDS];
	unsigned int ch, n, i;
	char name[44];

	/*
	 * Words 38/39 guard the snapshot: the Mixer Engine (CMxMixer::
	 * TimerCallback) skips one where either reads 0xFFFFFFFF, so the
	 * ALSA cache does not follow it either (parity/14 section 7).
	 */
	BUILD_BUG_ON(UA_MIXER_RB_WORDS < 40);
	if (rb[38] == 0xFFFFFFFF || rb[39] == 0xFFFFFFFF)
		return;

	for (i = 0; i < UA_MIXER_RB_WORDS; i++)
		chg[i] = audio->rb_synced_valid ? rb[i] ^ old[i] : ~0u;
	memcpy(old, rb, sizeof(audio->rb_synced));
	audio->rb_synced_valid = true;

	/* Word 0, 3: the four inputs the words describe */
	n = min_t(unsigned int, audio->num_preamps, 4);
	for (ch = 0; ch < n; ch++) {
		struct ua_preamp_state *st = &audio->preamp[ch];
		u32 flags = (chg[0] >> (ch * 6)) & 0x3f;

		if (flags & BIT(0)) {
			snprintf(name, sizeof(name), "Line %u Input Select",
				 ch + 1);
			ua_sync_bool(ua, &st->mic_line,
				     UA_RB_PREAMP_MICLINE(rb[0], ch), name);
		}
		if (flags & BIT(1)) {
			snprintf(name, sizeof(name), "Line %u Pad Switch",
				 ch + 1);
			ua_sync_bool(ua, &st->pad, UA_RB_PREAMP_PAD(rb[0], ch),
				     name);
		}
		if (flags & BIT(3)) {
			snprintf(name, sizeof(name),
				 "Mic %u 48V Phantom Power Switch", ch + 1);
			ua_sync_bool(ua, &st->phantom,
				     UA_RB_PREAMP_48V(rb[0], ch), name);
		}
		if (flags & BIT(4)) {
			snprintf(name, sizeof(name), "Line %u LowCut Switch",
				 ch + 1);
			ua_sync_bool(ua, &st->lowcut,
				     UA_RB_PREAMP_LOWCUT(rb[0], ch), name);
		}
		if (flags & BIT(5)) {
			snprintf(name, sizeof(name),
				 "Line %u Phase Invert Switch", ch + 1);
			ua_sync_bool(ua, &st->phase,
				     UA_RB_PREAMP_PHASE(rb[0], ch), name);
		}
		if (ch < 2 && ch < audio->num_hiz && (chg[0] & BIT(24 + ch))) {
			snprintf(name, sizeof(name),
				 "Line %u HiZ Detect Switch", ch + 1);
			ua_sync_bool(ua, &st->hiz, UA_RB_HIZ_DETECT(rb[0], ch),
				     name);
		}
		if (UA_RB_PREAMP_GAINC(chg[3], ch)) {
			snprintf(name, sizeof(name), "Line %u Capture Volume",
				 ch + 1);
			ua_sync_int(ua, &st->gain,
				    min_t(int, max_t(int,
					    UA_RB_PREAMP_GAIN_DB(rb[3], ch),
					    UA_PREAMP_GAIN_MIN_DB),
					  UA_PREAMP_GAIN_MAX_DB), name);
		}
	}

	/* Word 2: monitor section */
	if (chg[2] & 0xff)
		ua_sync_int(ua, &audio->monitor.level,
			    min_t(int, UA_RB_MON_VOLUME(rb[2]), 192),
			    "Monitor Playback Volume");
	if (chg[2] & 0xff00)
		ua_sync_int(ua, &audio->monitor.hp1_vol,
			    min_t(int, UA_RB_HP1_VOLUME(rb[2]), 192),
			    "Headphone 1 Playback Volume");
	if (chg[2] & BIT(16))
		ua_sync_int(ua, &audio->monitor.mute,
			    UA_RB_MON_MUTE(rb[2]) ? UA_MON_MUTE_ON :
						    UA_MON_MUTE_OFF,
			    "Monitor Playback Switch");
	if (chg[2] & BIT(17))
		ua_sync_bool(ua, &audio->monitor.mono, UA_RB_MON_MONO(rb[2]),
			     "Monitor Mono Switch");
	if (chg[2] & BIT(31))
		ua_sync_bool(ua, &audio->monitor.dim, UA_RB_MON_DIM(rb[2]),
			     "Monitor Dim Switch");

	/* Word 6: talkback button; word 7: HP2 knob */
	if (chg[6] & BIT(8))
		ua_sync_bool(ua, &audio->monitor.talkback,
			     UA_RB_TALKBACK(rb[6]), "Monitor Talkback Switch");
	if (chg[7] & 0xff)
		ua_sync_int(ua, &audio->monitor.hp2_vol,
			    min_t(int, UA_RB_HP2_VOLUME(rb[7]), 192),
			    "Headphone 2 Playback Volume");
}

/*
 * /proc/asound/cardN/mixer_state: the readback fields that have no control
 * (read-only state).  parity/14-mixer-params.md section 7.
 */
static void ua_mixer_state_proc_read(struct snd_info_entry *entry,
				     struct snd_info_buffer *buf)
{
	struct ua_device *ua = entry->private_data;
	u32 rb[UA_MIXER_RB_WORDS];
	bool seen;

	mutex_lock(&ua->lock);
	memcpy(rb, ua->mixer_rb_data, sizeof(rb));
	seen = ua->mixer_rb_seen;
	mutex_unlock(&ua->lock);

	if (!seen) {
		snd_iprintf(buf, "no mixer readback yet\n");
		return;
	}
	snd_iprintf(buf, "headphone_focus\t%s\n",
		    !UA_RB_HP_FOCUS_VALID(rb[0]) ? "unset" :
		    UA_RB_HP_FOCUS_HP(rb[0]) ? "headphone" : "monitor");
	snd_iprintf(buf, "selected_input\t%u\n", UA_RB_SEL_INPUT(rb[3]));
	snd_iprintf(buf, "altmon_select\t%u\n", UA_RB_ALTMON_SEL(rb[6]));
	snd_iprintf(buf, "low_noise\t%u\n", UA_RB_LOW_NOISE(rb[2]));
	snd_iprintf(buf, "temperature_raw\t%u\n", UA_RB_TEMPERATURE(rb[17]));
	snd_iprintf(buf, "analog_board_id\t%u\n", UA_RB_BOARD_ID(rb[33]));
}

/*
 * ua_mixer_init - Register ALSA mixer (kcontrol) elements
 *
 * Dynamically creates controls based on detected model:
 *   Per preamp ch: Gain + 48V + Pad + Phase + LowCut (+ HiZ if capable)
 *   Monitor: Volume + Switch (mute) + Dim
 *   Card-level: Sample Rate + Clock Source
 */
static int ua_mixer_init(struct ua_device *ua)
{
	struct ua_audio *audio = &ua->audio;
	struct snd_kcontrol_new ctl;
	struct snd_kcontrol *kctl;
	unsigned int ch, total = 0;
	int ret;

	/* Per-preamp-channel controls */
	for (ch = 0; ch < audio->num_preamps; ch++) {
		char name[44];

		/* Gain */
		ctl = (struct snd_kcontrol_new){};
		ctl.iface = SNDRV_CTL_ELEM_IFACE_MIXER;
		ctl.access = SNDRV_CTL_ELEM_ACCESS_READWRITE |
			     SNDRV_CTL_ELEM_ACCESS_TLV_READ;
		ctl.tlv.p = preamp_gain_tlv;
		snprintf(name, sizeof(name), "Line %u Capture Volume", ch + 1);
		ctl.name = name;
		ctl.info = ua_preamp_gain_info;
		ctl.get = ua_preamp_gain_get;
		ctl.put = ua_preamp_gain_put;
		ctl.private_value = UA_CTL_PACK(ch, UA_PREAMP_PARAM_GAIN_C);
		kctl = snd_ctl_new1(&ctl, ua);
		if (!kctl)
			return -ENOMEM;
		ret = ua_ctl_register(ua, kctl);
		if (ret)
			return ret;
		total++;

		/* 48V Phantom */
		ctl = (struct snd_kcontrol_new){};
		ctl.iface = SNDRV_CTL_ELEM_IFACE_MIXER;
		snprintf(name, sizeof(name),
			 "Mic %u 48V Phantom Power Switch", ch + 1);
		ctl.name = name;
		ctl.info = ua_preamp_switch_info;
		ctl.get = ua_preamp_switch_get;
		ctl.put = ua_preamp_switch_put;
		ctl.private_value = UA_CTL_PACK(ch, UA_ARM_PARAM_48V);
		kctl = snd_ctl_new1(&ctl, ua);
		if (!kctl)
			return -ENOMEM;
		ret = ua_ctl_register(ua, kctl);
		if (ret)
			return ret;
		total++;

		/* Pad */
		ctl = (struct snd_kcontrol_new){};
		ctl.iface = SNDRV_CTL_ELEM_IFACE_MIXER;
		snprintf(name, sizeof(name), "Line %u Pad Switch", ch + 1);
		ctl.name = name;
		ctl.info = ua_preamp_switch_info;
		ctl.get = ua_preamp_switch_get;
		ctl.put = ua_preamp_switch_put;
		ctl.private_value = UA_CTL_PACK(ch, UA_ARM_PARAM_PAD);
		kctl = snd_ctl_new1(&ctl, ua);
		if (!kctl)
			return -ENOMEM;
		ret = ua_ctl_register(ua, kctl);
		if (ret)
			return ret;
		total++;

		/* Phase Invert */
		ctl = (struct snd_kcontrol_new){};
		ctl.iface = SNDRV_CTL_ELEM_IFACE_MIXER;
		snprintf(name, sizeof(name),
			 "Line %u Phase Invert Switch", ch + 1);
		ctl.name = name;
		ctl.info = ua_preamp_switch_info;
		ctl.get = ua_preamp_switch_get;
		ctl.put = ua_preamp_switch_put;
		ctl.private_value = UA_CTL_PACK(ch, UA_ARM_PARAM_PHASE);
		kctl = snd_ctl_new1(&ctl, ua);
		if (!kctl)
			return -ENOMEM;
		ret = ua_ctl_register(ua, kctl);
		if (ret)
			return ret;
		total++;

		/* LowCut */
		ctl = (struct snd_kcontrol_new){};
		ctl.iface = SNDRV_CTL_ELEM_IFACE_MIXER;
		snprintf(name, sizeof(name),
			 "Line %u LowCut Switch", ch + 1);
		ctl.name = name;
		ctl.info = ua_preamp_switch_info;
		ctl.get = ua_preamp_switch_get;
		ctl.put = ua_preamp_switch_put;
		ctl.private_value = UA_CTL_PACK(ch, UA_ARM_PARAM_LOWCUT);
		kctl = snd_ctl_new1(&ctl, ua);
		if (!kctl)
			return -ENOMEM;
		ret = ua_ctl_register(ua, kctl);
		if (ret)
			return ret;
		total++;

		/* Input Select (Mic/Line) */
		ctl = (struct snd_kcontrol_new){};
		ctl.iface = SNDRV_CTL_ELEM_IFACE_MIXER;
		snprintf(name, sizeof(name),
			 "Line %u Input Select", ch + 1);
		ctl.name = name;
		ctl.info = ua_input_select_info;
		ctl.get = ua_input_select_get;
		ctl.put = ua_input_select_put;
		ctl.private_value = UA_CTL_PACK(ch, UA_PREAMP_PARAM_MIC_LINE);
		kctl = snd_ctl_new1(&ctl, ua);
		if (!kctl)
			return -ENOMEM;
		ret = ua_ctl_register(ua, kctl);
		if (ret)
			return ret;
		total++;

		/* HiZ: hardware auto-detect only, so a read-only indicator */
		if (ch < audio->num_hiz && ch < 2) {
			ctl = (struct snd_kcontrol_new){};
			ctl.iface = SNDRV_CTL_ELEM_IFACE_MIXER;
			ctl.access = SNDRV_CTL_ELEM_ACCESS_READ;
			snprintf(name, sizeof(name),
				 "Line %u HiZ Detect Switch", ch + 1);
			ctl.name = name;
			ctl.info = ua_preamp_switch_info;
			ctl.get = ua_hiz_get;
			ctl.private_value = UA_CTL_PACK(ch, 0);
			kctl = snd_ctl_new1(&ctl, ua);
			if (!kctl)
				return -ENOMEM;
			ret = ua_ctl_register(ua, kctl);
			if (ret)
				return ret;
			total++;
		}
	}

	/* Monitor Volume */
	{
		static const struct snd_kcontrol_new mon_vol = {
			.iface = SNDRV_CTL_ELEM_IFACE_MIXER,
			.access = SNDRV_CTL_ELEM_ACCESS_READWRITE |
				  SNDRV_CTL_ELEM_ACCESS_TLV_READ,
			.name = "Monitor Playback Volume",
			.info = ua_monitor_vol_info,
			.get = ua_monitor_vol_get,
			.put = ua_monitor_vol_put,
			.tlv.p = monitor_vol_tlv,
		};

		kctl = snd_ctl_new1(&mon_vol, ua);
		if (!kctl)
			return -ENOMEM;
		ret = ua_ctl_register(ua, kctl);
		if (ret)
			return ret;
		total++;
	}

	/* Monitor Switch (mute) */
	{
		static const struct snd_kcontrol_new mon_sw = {
			.iface = SNDRV_CTL_ELEM_IFACE_MIXER,
			.name = "Monitor Playback Switch",
			.info = ua_preamp_switch_info, /* reuse boolean info */
			.get = ua_monitor_mute_get,
			.put = ua_monitor_mute_put,
		};

		kctl = snd_ctl_new1(&mon_sw, ua);
		if (!kctl)
			return -ENOMEM;
		ret = ua_ctl_register(ua, kctl);
		if (ret)
			return ret;
		total++;
	}

	/* Monitor Dim */
	{
		static const struct snd_kcontrol_new mon_dim = {
			.iface = SNDRV_CTL_ELEM_IFACE_MIXER,
			.name = "Monitor Dim Switch",
			.info = ua_preamp_switch_info,
			.get = ua_monitor_dim_get,
			.put = ua_monitor_dim_put,
		};

		kctl = snd_ctl_new1(&mon_dim, ua);
		if (!kctl)
			return -ENOMEM;
		ret = ua_ctl_register(ua, kctl);
		if (ret)
			return ret;
		total++;
	}

	/* Monitor Mono */
	{
		static const struct snd_kcontrol_new mon_mono = {
			.iface = SNDRV_CTL_ELEM_IFACE_MIXER,
			.name = "Monitor Mono Switch",
			.info = ua_preamp_switch_info,
			.get = ua_monitor_mono_get,
			.put = ua_monitor_mono_put,
		};

		kctl = snd_ctl_new1(&mon_mono, ua);
		if (!kctl)
			return -ENOMEM;
		ret = ua_ctl_register(ua, kctl);
		if (ret)
			return ret;
		total++;
	}

	/* Monitor Source */
	{
		static const struct snd_kcontrol_new mon_src = {
			.iface = SNDRV_CTL_ELEM_IFACE_MIXER,
			.name = "Monitor Source",
			.info = ua_monitor_source_info,
			.get = ua_monitor_source_get,
			.put = ua_monitor_source_put,
		};

		kctl = snd_ctl_new1(&mon_src, ua);
		if (!kctl)
			return -ENOMEM;
		ret = ua_ctl_register(ua, kctl);
		if (ret)
			return ret;
		total++;
	}

	/* Headphone 1 Source */
	{
		ctl = (struct snd_kcontrol_new){};
		ctl.iface = SNDRV_CTL_ELEM_IFACE_MIXER;
		ctl.name = "Headphone 1 Source";
		ctl.info = ua_hp_source_info;
		ctl.get = ua_hp_source_get;
		ctl.put = ua_hp_source_put;
		ctl.private_value = UA_CTL_PACK(0, UA_MON_PARAM_HP1_SOURCE);
		kctl = snd_ctl_new1(&ctl, ua);
		if (!kctl)
			return -ENOMEM;
		ret = ua_ctl_register(ua, kctl);
		if (ret)
			return ret;
		total++;
	}

	/* Headphone 2 Source */
	{
		ctl = (struct snd_kcontrol_new){};
		ctl.iface = SNDRV_CTL_ELEM_IFACE_MIXER;
		ctl.name = "Headphone 2 Source";
		ctl.info = ua_hp_source_info;
		ctl.get = ua_hp_source_get;
		ctl.put = ua_hp_source_put;
		ctl.private_value = UA_CTL_PACK(0, UA_MON_PARAM_HP2_SOURCE);
		kctl = snd_ctl_new1(&ctl, ua);
		if (!kctl)
			return -ENOMEM;
		ret = ua_ctl_register(ua, kctl);
		if (ret)
			return ret;
		total++;
	}

	/* Headphone 1 Volume, Headphone 2 Volume (read-only mirror) */
	{
		unsigned int hp;

		for (hp = 1; hp <= 2; hp++) {
			char name[32];

			ctl = (struct snd_kcontrol_new){};
			ctl.iface = SNDRV_CTL_ELEM_IFACE_MIXER;
			ctl.access = SNDRV_CTL_ELEM_ACCESS_TLV_READ |
				     (hp == 1 ? SNDRV_CTL_ELEM_ACCESS_READWRITE :
						SNDRV_CTL_ELEM_ACCESS_READ);
			snprintf(name, sizeof(name),
				 "Headphone %u Playback Volume", hp);
			ctl.name = name;
			ctl.info = ua_monitor_vol_info;
			ctl.get = ua_hp_vol_get;
			ctl.put = ua_hp_vol_put;
			ctl.tlv.p = monitor_vol_tlv;
			ctl.private_value = UA_CTL_PACK(0, hp == 1 ?
				UA_MON_PARAM_HP1_VOL : 0);
			kctl = snd_ctl_new1(&ctl, ua);
			if (!kctl)
				return -ENOMEM;
			ret = ua_ctl_register(ua, kctl);
			if (ret)
				return ret;
			total++;
		}
	}

	/* Monitor Dim Level */
	{
		static const struct snd_kcontrol_new dim_lvl = {
			.iface = SNDRV_CTL_ELEM_IFACE_MIXER,
			.name = "Monitor Dim Level",
			.info = ua_dim_level_info,
			.get = ua_dim_level_get,
			.put = ua_dim_level_put,
		};

		kctl = snd_ctl_new1(&dim_lvl, ua);
		if (!kctl)
			return -ENOMEM;
		ret = ua_ctl_register(ua, kctl);
		if (ret)
			return ret;
		total++;
	}

	/* Monitor Talkback */
	{
		static const struct snd_kcontrol_new mon_tb = {
			.iface = SNDRV_CTL_ELEM_IFACE_MIXER,
			.name = "Monitor Talkback Switch",
			.info = ua_preamp_switch_info,
			.get = ua_monitor_talkback_get,
			.put = ua_monitor_talkback_put,
		};

		kctl = snd_ctl_new1(&mon_tb, ua);
		if (!kctl)
			return -ENOMEM;
		ret = ua_ctl_register(ua, kctl);
		if (ret)
			return ret;
		total++;
	}

	/* Digital Output Mode */
	{
		static const struct snd_kcontrol_new dig_mode = {
			.iface = SNDRV_CTL_ELEM_IFACE_MIXER,
			.name = "Digital Output Mode",
			.info = ua_digital_mode_info,
			.get = ua_digital_mode_get,
			.put = ua_digital_mode_put,
		};

		kctl = snd_ctl_new1(&dig_mode, ua);
		if (!kctl)
			return -ENOMEM;
		ret = ua_ctl_register(ua, kctl);
		if (ret)
			return ret;
		total++;
	}

	/* Digital Input Mode: io_map=1 route selection (see its put) */
	if (ua_io_map_enabled(ua) && audio->model &&
	    audio->model->toslink != UA_TOSLINK_NONE) {
		static const struct snd_kcontrol_new dig_in_mode = {
			.iface = SNDRV_CTL_ELEM_IFACE_MIXER,
			.name = "Digital Input Mode",
			.info = ua_digital_mode_info,
			.get = ua_digital_in_mode_get,
			.put = ua_digital_in_mode_put,
		};

		kctl = snd_ctl_new1(&dig_in_mode, ua);
		if (!kctl)
			return -ENOMEM;
		ret = ua_ctl_register(ua, kctl);
		if (ret)
			return ret;
		total++;
	}

	/* Output Reference Level */
	{
		static const struct snd_kcontrol_new out_ref = {
			.iface = SNDRV_CTL_ELEM_IFACE_MIXER,
			.name = "Output Reference Level",
			.info = ua_output_ref_info,
			.get = ua_output_ref_get,
			.put = ua_output_ref_put,
		};

		kctl = snd_ctl_new1(&out_ref, ua);
		if (!kctl)
			return -ENOMEM;
		ret = ua_ctl_register(ua, kctl);
		if (ret)
			return ret;
		total++;
	}

	/* Clock Mode (read-only view of Clock Source) */
	{
		static const struct snd_kcontrol_new clk_mode = {
			.iface = SNDRV_CTL_ELEM_IFACE_CARD,
			.access = SNDRV_CTL_ELEM_ACCESS_READ,
			.name = "Clock Mode",
			.info = ua_clock_mode_info,
			.get = ua_clock_mode_get,
		};

		kctl = snd_ctl_new1(&clk_mode, ua);
		if (!kctl)
			return -ENOMEM;
		ret = ua_ctl_register(ua, kctl);
		if (ret)
			return ret;
		total++;
	}

	/* CUE 1 Mirror Source */
	{
		ctl = (struct snd_kcontrol_new){};
		ctl.iface = SNDRV_CTL_ELEM_IFACE_MIXER;
		ctl.name = "CUE 1 Mirror Source";
		ctl.info = ua_mirror_source_info;
		ctl.get = ua_mirror_source_get;
		ctl.put = ua_mirror_source_put;
		ctl.private_value = UA_CTL_PACK(0, UA_MON_PARAM_CUE1_MIRROR);
		kctl = snd_ctl_new1(&ctl, ua);
		if (!kctl)
			return -ENOMEM;
		ret = ua_ctl_register(ua, kctl);
		if (ret)
			return ret;
		total++;
	}

	/* CUE 2 Mirror Source */
	{
		ctl = (struct snd_kcontrol_new){};
		ctl.iface = SNDRV_CTL_ELEM_IFACE_MIXER;
		ctl.name = "CUE 2 Mirror Source";
		ctl.info = ua_mirror_source_info;
		ctl.get = ua_mirror_source_get;
		ctl.put = ua_mirror_source_put;
		ctl.private_value = UA_CTL_PACK(0, UA_MON_PARAM_CUE2_MIRROR);
		kctl = snd_ctl_new1(&ctl, ua);
		if (!kctl)
			return -ENOMEM;
		ret = ua_ctl_register(ua, kctl);
		if (ret)
			return ret;
		total++;
	}

	/* CUE 1 Mirror Switch */
	{
		ctl = (struct snd_kcontrol_new){};
		ctl.iface = SNDRV_CTL_ELEM_IFACE_MIXER;
		ctl.name = "CUE 1 Mirror Switch";
		ctl.info = ua_preamp_switch_info;
		ctl.get = ua_mirror_enable_get;
		ctl.put = ua_mirror_enable_put;
		ctl.private_value = UA_CTL_PACK(0, UA_MON_PARAM_MIRROR_ENABLE_A);
		kctl = snd_ctl_new1(&ctl, ua);
		if (!kctl)
			return -ENOMEM;
		ret = ua_ctl_register(ua, kctl);
		if (ret)
			return ret;
		total++;
	}

	/* CUE 2 Mirror Switch */
	{
		ctl = (struct snd_kcontrol_new){};
		ctl.iface = SNDRV_CTL_ELEM_IFACE_MIXER;
		ctl.name = "CUE 2 Mirror Switch";
		ctl.info = ua_preamp_switch_info;
		ctl.get = ua_mirror_enable_get;
		ctl.put = ua_mirror_enable_put;
		ctl.private_value = UA_CTL_PACK(0, UA_MON_PARAM_MIRROR_ENABLE_B);
		kctl = snd_ctl_new1(&ctl, ua);
		if (!kctl)
			return -ENOMEM;
		ret = ua_ctl_register(ua, kctl);
		if (ret)
			return ret;
		total++;
	}

	/* Digital Mirror Switch */
	{
		static const struct snd_kcontrol_new dig_mirr = {
			.iface = SNDRV_CTL_ELEM_IFACE_MIXER,
			.name = "Digital Mirror Switch",
			.info = ua_preamp_switch_info,
			.get = ua_digital_mirror_get,
			.put = ua_digital_mirror_put,
		};

		kctl = snd_ctl_new1(&dig_mirr, ua);
		if (!kctl)
			return -ENOMEM;
		ret = ua_ctl_register(ua, kctl);
		if (ret)
			return ret;
		total++;
	}

	/* Identify Switch */
	{
		static const struct snd_kcontrol_new ident = {
			.iface = SNDRV_CTL_ELEM_IFACE_MIXER,
			.name = "Identify Switch",
			.info = ua_preamp_switch_info,
			.get = ua_identify_get,
			.put = ua_identify_put,
		};

		kctl = snd_ctl_new1(&ident, ua);
		if (!kctl)
			return -ENOMEM;
		ret = ua_ctl_register(ua, kctl);
		if (ret)
			return ret;
		total++;
	}

	/* S/PDIF SRC Switch */
	{
		static const struct snd_kcontrol_new src_sw = {
			.iface = SNDRV_CTL_ELEM_IFACE_MIXER,
			.name = "S/PDIF SRC Switch",
			.info = ua_preamp_switch_info,
			.get = ua_sr_convert_get,
			.put = ua_sr_convert_put,
		};

		kctl = snd_ctl_new1(&src_sw, ua);
		if (!kctl)
			return -ENOMEM;
		ret = ua_ctl_register(ua, kctl);
		if (ret)
			return ret;
		total++;
	}

	/* DSP Spanning Switch */
	{
		static const struct snd_kcontrol_new dsp_span = {
			.iface = SNDRV_CTL_ELEM_IFACE_MIXER,
			.name = "DSP Spanning Switch",
			.info = ua_preamp_switch_info,
			.get = ua_dsp_spanning_get,
			.put = ua_dsp_spanning_put,
		};

		kctl = snd_ctl_new1(&dsp_span, ua);
		if (!kctl)
			return -ENOMEM;
		ret = ua_ctl_register(ua, kctl);
		if (ret)
			return ret;
		total++;
	}

	/* CUE 1 Mono Switch */
	{
		ctl = (struct snd_kcontrol_new){};
		ctl.iface = SNDRV_CTL_ELEM_IFACE_MIXER;
		ctl.name = "CUE 1 Mono Switch";
		ctl.info = ua_preamp_switch_info;
		ctl.get = ua_cue_mono_get;
		ctl.put = ua_cue_mono_put;
		ctl.private_value = UA_CTL_PACK(0, UA_MON_PARAM_CUE1_MONO);
		kctl = snd_ctl_new1(&ctl, ua);
		if (!kctl)
			return -ENOMEM;
		ret = ua_ctl_register(ua, kctl);
		if (ret)
			return ret;
		total++;
	}

	/* CUE 2 Mono Switch */
	{
		ctl = (struct snd_kcontrol_new){};
		ctl.iface = SNDRV_CTL_ELEM_IFACE_MIXER;
		ctl.name = "CUE 2 Mono Switch";
		ctl.info = ua_preamp_switch_info;
		ctl.get = ua_cue_mono_get;
		ctl.put = ua_cue_mono_put;
		ctl.private_value = UA_CTL_PACK(0, UA_MON_PARAM_CUE2_MONO);
		kctl = snd_ctl_new1(&ctl, ua);
		if (!kctl)
			return -ENOMEM;
		ret = ua_ctl_register(ua, kctl);
		if (ret)
			return ret;
		total++;
	}

	/* CUE 1 Mix Switch (INVERTED: ALSA 1=hw 0=on, ALSA 0=hw 2=off) */
	{
		ctl = (struct snd_kcontrol_new){};
		ctl.iface = SNDRV_CTL_ELEM_IFACE_MIXER;
		ctl.name = "CUE 1 Mix Switch";
		ctl.info = ua_preamp_switch_info;
		ctl.get = ua_cue_mix_get;
		ctl.put = ua_cue_mix_put;
		ctl.private_value = UA_CTL_PACK(0, UA_MON_PARAM_CUE1_MIX);
		kctl = snd_ctl_new1(&ctl, ua);
		if (!kctl)
			return -ENOMEM;
		ret = ua_ctl_register(ua, kctl);
		if (ret)
			return ret;
		total++;
	}

	/* CUE 2 Mix Switch (INVERTED: ALSA 1=hw 0=on, ALSA 0=hw 2=off) */
	{
		ctl = (struct snd_kcontrol_new){};
		ctl.iface = SNDRV_CTL_ELEM_IFACE_MIXER;
		ctl.name = "CUE 2 Mix Switch";
		ctl.info = ua_preamp_switch_info;
		ctl.get = ua_cue_mix_get;
		ctl.put = ua_cue_mix_put;
		ctl.private_value = UA_CTL_PACK(0, UA_MON_PARAM_CUE2_MIX);
		kctl = snd_ctl_new1(&ctl, ua);
		if (!kctl)
			return -ENOMEM;
		ret = ua_ctl_register(ua, kctl);
		if (ret)
			return ret;
		total++;
	}

	/* Sample Rate */
	{
		static const struct snd_kcontrol_new rate_ctl = {
			.iface = SNDRV_CTL_ELEM_IFACE_CARD,
			.name = "Sample Rate",
			.info = ua_sample_rate_info,
			.get = ua_sample_rate_get,
			.put = ua_sample_rate_put,
		};

		kctl = snd_ctl_new1(&rate_ctl, ua);
		if (!kctl)
			return -ENOMEM;
		ret = ua_ctl_register(ua, kctl);
		if (ret)
			return ret;
		total++;
	}

	/* Clock Source */
	{
		static const struct snd_kcontrol_new clk_ctl = {
			.iface = SNDRV_CTL_ELEM_IFACE_CARD,
			.name = "Clock Source",
			.info = ua_clock_source_info,
			.get = ua_clock_source_get,
			.put = ua_clock_source_put,
		};

		kctl = snd_ctl_new1(&clk_ctl, ua);
		if (!kctl)
			return -ENOMEM;
		ret = ua_ctl_register(ua, kctl);
		if (ret)
			return ret;
		total++;
	}

	/* Clock Locked (read-only) */
	{
		static const struct snd_kcontrol_new lock_ctl = {
			.iface = SNDRV_CTL_ELEM_IFACE_CARD,
			.access = SNDRV_CTL_ELEM_ACCESS_READ,
			.name = "Clock Locked",
			.info = snd_ctl_boolean_mono_info,
			.get = ua_clock_locked_get,
		};

		kctl = snd_ctl_new1(&lock_ctl, ua);
		if (!kctl)
			return -ENOMEM;
		ret = ua_ctl_register(ua, kctl);
		if (ret)
			return ret;
		total++;
	}

	dev_info(&ua->pdev->dev,
		 "ALSA mixer: %u controls (%u preamp ch × 6 + HiZ, monitor/routing, clock)\n",
		 total, audio->num_preamps);

	return 0;
}

/* ----------------------------------------------------------------
 * ALSA card and PCM device registration
 * ---------------------------------------------------------------- */

/*
 * Connect to DSP firmware at init (and resume) time.
 *
 * The macOS kext defers this to first stream open, but we need
 * the firmware connected early so the CLI interface (ARM MCU)
 * is responsive for userspace mixer control via ioctl.
 * Without this handshake, CLI commands timeout (status=0x24).
 *
 * Non-fatal: if connect fails (e.g. FW not ready), ALSA
 * prepare will retry when a stream is actually opened.
 * Returns the connect result for logging only.  Caller holds ua->lock.
 */
static int ua_audio_initial_connect(struct ua_device *ua)
{
	struct ua_audio *audio = &ua->audio;
	int ret = 0;

	if (warm_boot) {
		/*
		 * Warm boot from Windows/macOS: DSP already fully
		 * initialized with per-channel firmware.  Skip ACEFACE
		 * (which conflicts with existing state) and mark
		 * connected so pcm_prepare goes straight to transport.
		 */
		u32 seq_rd = ua_read(ua, UA_REG_MIXER_SEQ_RD);

		audio->connected = true;
		ua->aceface_done = true;
		ua->mixer_seq_wr = seq_rd;
		ua_write(ua, UA_REG_MIXER_SEQ_WR, seq_rd);
		ua->mixer_rb_seen = false;
		ua->mixer_ready = true;
		dev_info(&ua->pdev->dev,
			 "warm_boot=1: skipping ACEFACE, marked connected (SEQ=%u)\n",
			 seq_rd);

		/*
		 * Kext Connect() ends with SetOutputMute(0); the regular
		 * connect does it in post-handshake, this path skips that,
		 * so undo a shutdown/suspend mute here too.  Flushed by the
		 * service loop; a no-op except on the output-mute types.
		 */
		ua_set_output_mute_locked(ua, false, false);

		/*
		 * The connect path reads the IO descriptors (read-only)
		 * after the handshake; this one skips it, so read them here
		 * and apply the io_map=1 layout before the first open.
		 */
		ua_io_map_connected(ua);

		ua_dsp_service_start(ua);
	} else if (no_connect) {
		dev_info(&ua->pdev->dev,
			 "no_connect=1: skipping ACEFACE connect (debug mode)\n");
	} else {
		ret = ua_audio_connect(ua);
		if (ret) {
			dev_warn(&ua->pdev->dev,
				 "early firmware connect failed (%d), will retry on stream open\n",
				 ret);
		} else {
			/* DSP connected — start periodic readback drain */
			ua_dsp_service_start(ua);
		}
	}

	return ret;
}

/*
 * ua_audio_suspend - software half of the kext's _stopPowerGated.
 *
 * Blocks the ALSA controls (the PCM core has already suspended running
 * streams: the PCM devices are children of this PCI device, so they
 * suspend first and trigger(SUSPEND) has stopped the period timer) and
 * stops everything that could touch BAR0 behind the hardware quiesce:
 * the DSP service loop and the period timer.  Caller must NOT hold
 * ua->lock (the service handler takes it).
 */
void ua_audio_suspend(struct ua_device *ua)
{
	struct ua_audio *audio = &ua->audio;

	if (audio->card)
		snd_power_change_state(audio->card, SNDRV_CTL_POWER_D3hot);

	ua_dsp_service_stop(ua);
	/* The watchdog first: its fallback can start the hrtimer */
	ua_audio_period_wd_cancel(ua);
	audio->period_timer_running = false;
	hrtimer_cancel(&audio->period_timer);
}

/*
 * ua_audio_resume - connect half of the kext's _startPowerGated
 * (ProgramRegisters has already run, from ua_resume()).
 * @how: UA_RESUME_CONTROLS: no connect;
 *       UA_RESUME_CONNECT_FAST: the same connect as probe, but only if the
 *       ACEFACE handshake answers within the early probe's ~1 s
 *       (ua_aceface_handshake()); -EAGAIN when the full connect loop is
 *       still needed (ua_resume() then defers it to a worker);
 *       UA_RESUME_CONNECT: the same connect as probe (up to ~20 s).
 * Reopens the ALSA controls in every case.  Streams come back through a
 * fresh prepare (no SNDRV_PCM_INFO_RESUME), which restarts the
 * transport.  Caller must NOT hold ua->lock.
 */
int ua_audio_resume(struct ua_device *ua, enum ua_resume_how how)
{
	struct ua_audio *audio = &ua->audio;
	int ret = 0;

	mutex_lock(&ua->lock);
	audio->connected = false;
	audio->transport_running = false;
	audio->adopt_pending = true;
	ua_audio_stall_qos(ua, false);

	if (how == UA_RESUME_CONNECT_FAST && !warm_boot && !no_connect &&
	    !ua->aceface_done && ua_aceface_handshake(ua)) {
		/* Not answering yet: the worker runs the full loop */
		ua->aceface_failed = false;
		ret = -EAGAIN;
	} else if (how != UA_RESUME_CONTROLS) {
		ret = ua_audio_initial_connect(ua);
	}
	mutex_unlock(&ua->lock);

	if (audio->card)
		snd_power_change_state(audio->card, SNDRV_CTL_POWER_D0);

	return ret;
}

/*
 * /proc/asound/cardN/clock: clock and transport state for the titan A/B
 * runs (parity/12, parity/18).  Cached values only, no register access.
 */
static void ua_clock_proc_read(struct snd_info_entry *entry,
			       struct snd_info_buffer *buf)
{
	struct ua_device *ua = entry->private_data;
	struct ua_audio *audio = &ua->audio;
	static const char * const bands[3] = {
		"44.1/48 kHz", "88.2/96 kHz", "176.4/192 kHz",
	};
	unsigned int i, n;
	u8 raw[UA_CLOCK_NAMES_BYTES];
	char names[UA_CLOCK_LIST_MAX][UA_CLOCK_NAME_LEN];
	u8 vals[UA_CLOCK_LIST_MAX];
	bool names_ok;

	snd_iprintf(buf, "rate\t\t\t%u Hz (index %u)\n", audio->sample_rate,
		    audio->rate_index);
	snd_iprintf(buf, "clock source\t\t0x%02x\n", audio->clock_source);
	snd_iprintf(buf, "firmware rate info\t0x%08x (rate index %u)\n",
		    audio->last_rate_info,
		    ua_fw_rate_index(audio->last_rate_info));
	snd_iprintf(buf, "clock locked\t\t%s (%u changes)\n",
		    audio->clock_lock_valid ?
		    (audio->clock_locked ? "yes" : "no") : "unknown",
		    audio->clock_lock_changes);
	snd_iprintf(buf, "rate mask 0xC08C[7:0]\t0x%02x\n", audio->fw_rate_mask);
	snd_iprintf(buf, "source mask 0xC08C\t0x%04x\n", audio->clock_src_mask);

	spin_lock(&audio->clock_list_lock);
	n = audio->clock_list_n;
	memcpy(vals, audio->clock_list_val, sizeof(vals));
	memcpy(names, audio->clock_list_name, sizeof(names));
	memcpy(raw, audio->clock_names_raw, sizeof(raw));
	names_ok = audio->clock_names_ok;
	spin_unlock(&audio->clock_list_lock);

	snd_iprintf(buf, "\nclock source menu from the device (%s, clock_src_from_device=%d):\n",
		    names_ok ? "all named by the device" : "some names fall back",
		    clock_src_from_device);
	for (i = 0; i < n; i++)
		snd_iprintf(buf, "  item %u  source 0x%02x  %s\n", i, vals[i],
			    names[i]);
	snd_iprintf(buf, "names 0xC090 raw (non-printable bytes as '.'):\n  ");
	for (i = 0; i < UA_CLOCK_NAMES_BYTES; i++)
		snd_iprintf(buf, "%c", raw[i] >= 0x20 && raw[i] < 0x7f ?
			    raw[i] : '.');
	snd_iprintf(buf, "\n");

	snd_iprintf(buf, "\nfirmware latency words, out 0xC198 / in 0xC19C (frames [M]):\n");
	for (i = UA_RATE_44100; i <= UA_RATE_192000; i++)
		if (audio->fw_lat_seen[i])
			snd_iprintf(buf, "  %6u Hz  out %u  in %u\n",
				    ua_rate_hz(i), audio->fw_lat_out[i],
				    audio->fw_lat_in[i]);

	snd_iprintf(buf, "\nFRAME_CFG written last 0x%02x (frame_cfg_win=%d, frame_ctr_gate=%d)\n",
		    audio->frame_cfg_last, frame_cfg_win, frame_ctr_gate);
	for (i = 0; i < 3; i++)
		if (audio->frame_ctr_seen[i])
			snd_iprintf(buf, "  %-14s FRAME_CTR after arm 0x%x, final 0x%x, gate %u ms\n",
				    bands[i], audio->frame_ctr_first[i],
				    audio->frame_ctr_final[i],
				    audio->frame_ctr_ms[i]);

	snd_iprintf(buf, "\nclock acks (clock_ack_bit5=%d): bit5 %u  bit22 %u  bit4 %u  last status 0x%08x\n",
		    clock_ack_bit5, audio->clock_ack_count[0],
		    audio->clock_ack_count[1], audio->clock_ack_count[2],
		    audio->clock_ack_last);
	snd_iprintf(buf, "notification reads before the ack (notif_kext_reads=%d): %u, last 0xC084=0x%08x 0xC07C=0x%08x 0xC080=0x%08x\n",
		    notif_kext_reads, audio->kr_count, audio->kr_clock_info,
		    audio->kr_rate_info, audio->kr_xport_info);
	snd_iprintf(buf, "rate acks to the DSPs (rate_ack_real=%d): %u, last %u Hz\n",
		    rate_ack_real, audio->rate_ack_sent, audio->rate_acked);
	snd_iprintf(buf, "adopt_fw_rate=%d\n", adopt_fw_rate);

	snd_iprintf(buf, "\nAX_CONTROL fault flags: bit 7 seen %u times, bit 8 seen %u times (latched now: 0x%x)\n",
		    audio->ax_fault_count[0], audio->ax_fault_count[1],
		    audio->ax_fault_latch);
	if (audio->ax_fault_count[0] || audio->ax_fault_count[1])
		snd_iprintf(buf, "  last AX_CONTROL 0x%03x, AX_STATUS at bit 7 0x%08x\n",
			    audio->ax_fault_last_ctrl, audio->ax_fault_status);

	snd_iprintf(buf, "\nbus read latency while streaming (AX_CONTROL read, stall_qos_us=%d%s): %u samples, max %llu us\n",
		    stall_qos_us, audio->stall_qos ? ", QoS held" : "",
		    audio->stall_samples, div_u64(audio->stall_max_ns, 1000));
	snd_iprintf(buf, "  <10us %u  <50us %u  <100us %u  <1ms %u  >=1ms %u\n",
		    audio->stall_hist[0], audio->stall_hist[1],
		    audio->stall_hist[2], audio->stall_hist[3],
		    audio->stall_hist[4]);
}

int ua_audio_init(struct ua_device *ua)
{
	struct ua_audio *audio = &ua->audio;
	struct snd_card *card;
	struct snd_pcm *pcm;
	int ret;

	spin_lock_init(&audio->lock);
	spin_lock_init(&audio->timer_lock);
	audio->running_subs = 0;
	init_rwsem(&audio->dma_sem);
	mutex_init(&audio->io_lock);
	INIT_DELAYED_WORK(&audio->dsp_service_work, ua_dsp_service_handler);
	INIT_DELAYED_WORK(&audio->period_wd, ua_period_wd_handler);
	/* Soft hrtimer: callback in softirq, not hard IRQ (see callback) */
#if LINUX_VERSION_CODE >= KERNEL_VERSION(6, 13, 0)
	hrtimer_setup(&audio->period_timer, ua_period_timer_callback,
		      CLOCK_MONOTONIC, HRTIMER_MODE_REL_SOFT);
#else
	hrtimer_init(&audio->period_timer, CLOCK_MONOTONIC,
		     HRTIMER_MODE_REL_SOFT);
	audio->period_timer.function = ua_period_timer_callback;
#endif
	init_completion(&audio->connect_done);
	spin_lock_init(&audio->clock_list_lock);
	audio->adopt_pending = true;
	audio->sample_rate = 48000;
	audio->rate_index = UA_RATE_48000;
	/* Use source=0xC for internal clock — enables DSP processing after
	 * transport start. UA_CLOCK_INTERNAL (0) doesn't get FPGA ack. */
	audio->clock_source = 0xC;

	/* Determine channel counts for this model.
	 * ua_audio_preinit_dma() may have already set these up. */
	if (!audio->play_channels)
		ua_get_model_channels(ua);

	dev_info(&ua->pdev->dev, "audio: %u play ch, %u rec ch, %u frame buf\n",
		 audio->play_channels, audio->rec_channels,
		 audio->buf_frame_size);

	/*
	 * TOSLink input type for the io_map=1 route lists: the Mixer
	 * Engine's DefaultTOSLinkInputType for the class ("Digital Input
	 * Mode" changes it; host side only).
	 */
	audio->dig_in_mode = audio->model ? audio->model->default_in_mode :
					    UA_DIG_SPDIF;

	/* The IO map may be used from here on (connect, ioctl, PCM) */
	audio->io_ready = true;
	dev_info(&ua->pdev->dev, "io_map=%d: %s\n", io_map,
		 ua_io_map_enabled(ua) ?
		 "DMA layout follows the firmware IO descriptor" :
		 "legacy DMA layout (ALSA channel N = DMA slot N)");

	/*
	 * Allocate DMA buffers and program the SG table.  A no-op when
	 * ua_audio_preinit_dma() already ran from the connect path; when a
	 * warm-boot preinit failed this is the retry, and it has to program
	 * the SG SRAM too — a bare ua_audio_alloc_dma() here left the FPGA
	 * on the previous OS's entries (see the warm-boot comment in
	 * ua_core.c).
	 */
	ret = ua_audio_preinit_dma(ua);
	if (ret)
		return ret;

	/* Create ALSA card */
	ret = snd_card_new(&ua->pdev->dev, -1, NULL, THIS_MODULE, 0, &card);
	if (ret) {
		dev_err(&ua->pdev->dev, "snd_card_new failed: %d\n", ret);
		goto err_dma;
	}

	audio->card = card;
	strscpy(card->driver, "ua_apollo", sizeof(card->driver));
	strscpy(card->shortname, ua_device_name(ua->device_type),
		sizeof(card->shortname));
	snprintf(card->longname, sizeof(card->longname),
		 "%s at %s", ua_device_name(ua->device_type),
		 pci_name(ua->pdev));

	/* Create PCM device: 1 playback substream, 1 capture substream */
	ret = snd_pcm_new(card, "UA Apollo", 0, 1, 1, &pcm);
	if (ret) {
		dev_err(&ua->pdev->dev, "snd_pcm_new failed: %d\n", ret);
		goto err_card;
	}

	audio->pcm = pcm;
	pcm->private_data = ua;
	strscpy(pcm->name, "UA Apollo PCM", sizeof(pcm->name));

	snd_pcm_set_ops(pcm, SNDRV_PCM_STREAM_PLAYBACK, &ua_pcm_playback_ops);
	snd_pcm_set_ops(pcm, SNDRV_PCM_STREAM_CAPTURE, &ua_pcm_capture_ops);

	/* /proc/asound/cardN/io_map: channel names, slots, descriptors */
	ret = snd_card_ro_proc_new(card, "io_map", ua, ua_io_map_proc_read);
	if (ret) {
		dev_warn(&ua->pdev->dev, "io_map proc entry failed: %d\n", ret);
		ret = 0;
	}

	/* /proc/asound/cardN/clock: clock and transport diagnostics */
	ret = snd_card_ro_proc_new(card, "clock", ua, ua_clock_proc_read);
	if (ret) {
		dev_warn(&ua->pdev->dev, "clock proc entry failed: %d\n", ret);
		ret = 0;
	}

	/* /proc/asound/cardN/mixer_state: readback fields without a control */
	ret = snd_card_ro_proc_new(card, "mixer_state", ua,
				   ua_mixer_state_proc_read);
	if (ret) {
		dev_warn(&ua->pdev->dev, "mixer_state proc entry failed: %d\n",
			 ret);
		ret = 0;
	}

	/* /proc/asound/cardN/dsp_state: DSP window and ring registers (ua_dsp.c) */
	if (ua_dsp_proc_init(ua, card))
		dev_warn(&ua->pdev->dev, "dsp_state proc entry failed\n");

	/* /proc/asound/cardN/period_irq: period tracking state, counters */
	ret = snd_card_ro_proc_new(card, "period_irq", ua, ua_period_proc_read);
	if (ret) {
		dev_warn(&ua->pdev->dev, "period_irq proc entry failed: %d\n",
			 ret);
		ret = 0;
	}

	/* Initialize preamp/monitor cache to safe defaults */
	{
		unsigned int i;

		for (i = 0; i < UA_MAX_PREAMP_CH; i++) {
			audio->preamp[i].gain = 0;
			audio->preamp[i].phantom = false;
			audio->preamp[i].pad = false;
			audio->preamp[i].hiz = false;
			audio->preamp[i].phase = false;
			audio->preamp[i].lowcut = false;
			audio->preamp[i].mic_line = false; /* default Mic */
		}
		audio->monitor.level = 192;  /* 0 dB: 192 + 0×2 */
		audio->monitor.mute = UA_MON_MUTE_OFF;
		audio->monitor.dim = false;
		audio->monitor.mono = false;
		audio->monitor.source = 0;       /* MIX */
		audio->monitor.hp1_source = 0;   /* CUE 1 */
		audio->monitor.hp2_source = 0;   /* CUE 1 */
		audio->monitor.dim_level = 1;    /* -9 dB */
		audio->monitor.talkback = false;
		audio->monitor.digital_mode = 0; /* S/PDIF */
		audio->monitor.output_ref = 0;   /* +4 dBu */
		audio->monitor.hp1_vol = 192;    /* the knob sets it; mirrored */
		audio->monitor.hp2_vol = 192;
		audio->monitor.cue1_mirror = (int)0xFFFFFFFF; /* NONE */
		audio->monitor.cue2_mirror = (int)0xFFFFFFFF; /* NONE */
		audio->monitor.cue1_mirror_en = false;
		audio->monitor.cue2_mirror_en = false;
		audio->monitor.digital_mirror = false;
		audio->monitor.identify = false;
		audio->monitor.sr_convert = 0;
		audio->monitor.dsp_spanning = 0;
		audio->monitor.cue1_mono = false;
		audio->monitor.cue2_mono = false;
		audio->monitor.cue1_mix = 0;  /* on */
		audio->monitor.cue2_mix = 0;  /* on */
	}

	/* Register ALSA mixer controls */
	ret = ua_mixer_init(ua);
	if (ret) {
		dev_err(&ua->pdev->dev, "mixer init failed: %d\n", ret);
		goto err_card;
	}

	/* Register the card */
	ret = snd_card_register(card);
	if (ret) {
		dev_err(&ua->pdev->dev, "snd_card_register failed: %d\n", ret);
		goto err_card;
	}

	dev_info(&ua->pdev->dev, "ALSA card registered: %s\n", card->longname);

	/* Connect to DSP firmware at init time (non-fatal, see helper) */
	ua->skip_bus_coeff = skip_bus_coeff;

	/*
	 * Under ua->lock like every other connect (resume, pcm_prepare,
	 * reconnect): the card is registered, so a PCM open, an ALSA control
	 * or an ioctl can run concurrently, and the connect may change the
	 * io_map=1 layout.
	 */
	mutex_lock(&ua->lock);
	ua_audio_initial_connect(ua);
	mutex_unlock(&ua->lock);

	return 0;

err_card:
	snd_card_free(card);
	audio->card = NULL;
err_dma:
	ua_audio_free_dma(ua);
	return ret;
}

void ua_audio_fini(struct ua_device *ua)
{
	struct ua_audio *audio = &ua->audio;

	/* Stop DSP service loop first (blocks until handler returns) */
	ua_dsp_service_stop(ua);

	/* Stop transport and disconnect */
	ua_audio_stop_transport(ua);
	ua_audio_stall_qos(ua, false);
	ua_audio_disconnect(ua);

	/*
	 * The firmware may continue DMA for a brief period after disconnect.
	 * Wait for the DMA engine to drain before freeing buffers, otherwise
	 * the IOMMU revokes the mapping while the device is still writing,
	 * causing a DMA fault that kills the Thunderbolt bridge.
	 */
	ua_write(ua, UA_REG_AX_CONTROL, 0);
	ua_write(ua, UA_REG_TRANSPORT, 0);
	msleep(100);

	/*
	 * Free the ALSA card: disconnect (open files get errors), then wait
	 * until every PCM and control file is closed.  The PCM close and
	 * the controls use this struct and the DMA rings, so neither may go
	 * away before; snd_card_free_when_closed() returned at once and let
	 * a later close run on freed memory.
	 */
	if (audio->card) {
		snd_card_free(audio->card);
		audio->card = NULL;
	}

	/*
	 * No trigger can run any more: a START that raced the removal may
	 * have queued the period_irq watchdog after remove cancelled it.
	 */
	ua_audio_period_wd_cancel(ua);

	/*
	 * Free DMA buffers.  Take the write side of dma_sem so a PCM
	 * copy/silence callback that was already inside the ops when the
	 * card was disconnected finishes before the flat views go away;
	 * anything that comes later sees NULL buffers and returns -ENODEV.
	 */
	down_write(&audio->dma_sem);
	ua_audio_free_dma(ua);
	up_write(&audio->dma_sem);
}
