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
#include <linux/dma-mapping.h>
#include <sound/core.h>
#include <sound/pcm.h>
#include <sound/control.h>
#include <sound/tlv.h>
#include <sound/initval.h>

#include "ua_apollo.h"
#include "ua_ioctl.h"

/* Module parameters for channel count override */
static int play_ch = -1;
static int rec_ch = -1;
module_param(play_ch, int, 0444);
MODULE_PARM_DESC(play_ch, "Override playback channel count (default: auto-detect)");
module_param(rec_ch, int, 0444);
MODULE_PARM_DESC(rec_ch, "Override record channel count (default: auto-detect)");

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

/* Forward declarations */
static void ua_pcie_setup(struct ua_device *ua);

/* ----------------------------------------------------------------
 * Per-model channel counts
 *
 * These are the PCIe DMA channel counts from kext analysis, not the
 * physical I/O count.  The kext adds +1 to playback channels by
 * default (flag bit 1 clear) to carry a sync/timestamp channel.
 * ---------------------------------------------------------------- */

struct ua_model_info {
	u32 device_type;
	unsigned int play_ch;     /* DMA playback channel count */
	unsigned int rec_ch;      /* DMA record channel count */
	unsigned int num_preamps; /* Preamp channels with ALSA controls */
	unsigned int num_hiz;     /* HiZ-capable channels (first N preamps) */
};

/*
 * Channel counts from macOS IOKit IOAudioEngine properties.
 * Apollo x4: "Number of Output Channels: 24", "Number of Input Channels: 22"
 *
 * With diagnostic flags=0 (default), kext does NOT add +1 sync channel.
 * The formula: actualPlay = play + ((flags & 0x6) >= 1 ? 1 : 0)
 * With flags=0: (0 & 0x6) = 0, not >= 1, so +0.
 */
static const struct ua_model_info ua_models[] = {
	/*                             play rec preamps hiz */
	{ UA_DEV_APOLLO_SOLO,          3,  2,  1, 0 },
	{ UA_DEV_ARROW,                3,  2,  1, 0 },
	{ UA_DEV_APOLLO_TWIN_X,        8,  8,  2, 2 },
	{ UA_DEV_APOLLO_TWIN_X_GEN2,   8,  8,  2, 2 },
	{ UA_DEV_APOLLO_X4,           24, 22,  4, 2 },
	{ UA_DEV_APOLLO_X4_GEN2,     24, 22,  4, 2 },
	{ UA_DEV_APOLLO_X6,           24, 22,  4, 2 },
	{ UA_DEV_APOLLO_X6_GEN2,     24, 22,  4, 2 },
	{ UA_DEV_APOLLO_X8,           26, 26,  4, 2 },
	{ UA_DEV_APOLLO_X8_GEN2,     26, 26,  4, 2 },
	{ UA_DEV_APOLLO_X8P,          26, 26,  8, 2 },
	{ UA_DEV_APOLLO_X8P_GEN2,    26, 26,  8, 2 },
	{ UA_DEV_APOLLO_X16,          34, 34,  8, 2 },
	{ UA_DEV_APOLLO_X16_GEN2,    34, 34,  8, 2 },
	{ UA_DEV_APOLLO_X16D,        34, 34,  0, 0 },
};

static void ua_get_model_channels(struct ua_device *ua)
{
	struct ua_audio *audio = &ua->audio;
	int i;

	/* Module param overrides */
	if (play_ch > 0 && play_ch <= UA_MAX_CHANNELS) {
		audio->play_channels = play_ch;
		audio->rec_channels = (rec_ch > 0) ? rec_ch : play_ch;
		audio->num_preamps = 0;
		audio->num_hiz = 0;
		return;
	}

	/* Look up from model table */
	for (i = 0; i < ARRAY_SIZE(ua_models); i++) {
		if (ua_models[i].device_type == ua->device_type) {
			audio->play_channels = ua_models[i].play_ch;
			audio->rec_channels = ua_models[i].rec_ch;
			audio->num_preamps = ua_models[i].num_preamps;
			audio->num_hiz = ua_models[i].num_hiz;
			return;
		}
	}

	/* Fallback: safe stereo default */
	audio->play_channels = UA_DEFAULT_PLAY_CH + 1;  /* +1 for sync */
	audio->rec_channels = UA_DEFAULT_REC_CH;
	audio->num_preamps = 0;
	audio->num_hiz = 0;
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

/* ----------------------------------------------------------------
 * DMA buffer allocation and scatter-gather programming
 * ---------------------------------------------------------------- */

static int ua_audio_alloc_dma(struct ua_device *ua)
{
	struct ua_audio *audio = &ua->audio;
	struct device *dev = &ua->pdev->dev;

	audio->play_buf = dma_alloc_coherent(dev, UA_DMA_BUF_SIZE,
					     &audio->play_addr, GFP_KERNEL);
	if (!audio->play_buf)
		return -ENOMEM;

	audio->rec_buf = dma_alloc_coherent(dev, UA_DMA_BUF_SIZE,
					    &audio->rec_addr, GFP_KERNEL);
	if (!audio->rec_buf) {
		dma_free_coherent(dev, UA_DMA_BUF_SIZE,
				  audio->play_buf, audio->play_addr);
		audio->play_buf = NULL;
		return -ENOMEM;
	}

	memset(audio->play_buf, 0, UA_DMA_BUF_SIZE);
	memset(audio->rec_buf, 0, UA_DMA_BUF_SIZE);

	dev_info(dev, "DMA buffers: play=%pad rec=%pad (4 MiB each)\n",
		 &audio->play_addr, &audio->rec_addr);
	dev_info(dev, "DMA phys:    play=0x%llx rec=0x%llx\n",
		 (u64)virt_to_phys(audio->play_buf),
		 (u64)virt_to_phys(audio->rec_buf));
	return 0;
}

static void ua_audio_free_dma(struct ua_device *ua)
{
	struct ua_audio *audio = &ua->audio;
	struct device *dev = &ua->pdev->dev;

	if (audio->play_buf) {
		dma_free_coherent(dev, UA_DMA_BUF_SIZE,
				  audio->play_buf, audio->play_addr);
		audio->play_buf = NULL;
	}

	if (audio->rec_buf) {
		dma_free_coherent(dev, UA_DMA_BUF_SIZE,
				  audio->rec_buf, audio->rec_addr);
		audio->rec_buf = NULL;
	}
}

/*
 * Reset DMA engines — mirrors CPcieIntrManager::ResetDMAEngines().
 *
 * DMA_CTRL (0x2200) bits 9-16 are per-engine reset bits.
 * On power-up / connect, these are SET (0x1FE00 = all engines in reset).
 * We must toggle them to properly initialize the DMA engine state,
 * then clear all pending interrupts (kext writes 0xFFFFFFFF to ISR).
 */
static void ua_audio_reset_dma(struct ua_device *ua)
{
	u32 val, val_clean, val_reset, readback;

	/*
	 * DMA_CTRL (0x2200) bit layout:
	 *   Bit  0:      global DMA enable
	 *   Bits [8:1]:  per-channel DMA enables (v2: 8 ch, v1: 4 ch)
	 *   Bits [16:9]: per-channel reset strobes (pulse high to reset)
	 *
	 * Reset sequence:
	 *   1. val_clean = preserve bits [31:17] + force bit 0 on,
	 *      clear enables [8:1] AND strobes [16:9]
	 *   2. val_reset = val_clean | strobes (assert reset)
	 *   3. Write val_reset (reset pulse high)
	 *   4. Write val_clean (reset pulse low — engines come out of reset)
	 *   5. Readback flush
	 *
	 * BUG FIX: The old mask 0xFFFFFE01 only cleared enables [8:1]
	 * but preserved strobes [16:9].  When the device starts with
	 * strobes already asserted (0x1FE00), the "clean" write still
	 * had them high — DMA engines stayed in reset permanently.
	 * Also, bit 0 was never set, so global DMA was disabled.
	 */
	val = ua_read(ua, UA_REG_DMA_CTRL);
	dev_info(&ua->pdev->dev, "  DMA_CTRL before reset: 0x%08x\n", val);

	if (ua->fw_v2) {
		/* Clear enables [8:1] + strobes [16:9], force bit 0 on */
		val_clean = (val & ~0x1FFFE) | 0x1;
		val_reset = val_clean | 0x1FE00; /* Assert reset strobes */
	} else {
		/* Clear enables [4:1] + strobes [12:9], force bit 0 on */
		val_clean = (val & ~0x1E1E) | 0x1;
		val_reset = val_clean | 0x1E00;  /* Assert reset strobes */
	}

	ua_write(ua, UA_REG_DMA_CTRL, val_reset); /* Assert reset */
	ua_write(ua, UA_REG_DMA_CTRL, val_clean); /* Deassert reset */
	readback = ua_read(ua, UA_REG_DMA_CTRL);  /* Flush */

	dev_info(&ua->pdev->dev, "  DMA_CTRL after reset:  0x%08x (expect 0x%08x)\n",
		 readback, val_clean);

	/* Clear all pending interrupts (kext step after ResetDMAEngines) */
	ua_write(ua, UA_REG_IRQ_STATUS, UA_IRQ_CLEAR_ALL);
	if (ua->fw_v2)
		ua_write(ua, UA_REG_EXT_IRQ_STATUS, UA_IRQ_CLEAR_ALL);
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
	unsigned int i;
	dma_addr_t addr;

	dev_info(&ua->pdev->dev, "=== PROGRAM SG TABLE (%u entries, stride=0x%x) ===\n",
		 UA_DMA_SG_ENTRIES, UA_PCIE_PAGE_SIZE);
	dev_info(&ua->pdev->dev, "  play_addr=%pad rec_addr=%pad\n",
		 &audio->play_addr, &audio->rec_addr);

	for (i = 0; i < UA_DMA_SG_ENTRIES; i++) {
		addr = audio->play_addr + (dma_addr_t)i * UA_PCIE_PAGE_SIZE;
		ua_write(ua, UA_REG_PLAY_DMA_BASE + i * 8,
			 lower_32_bits(addr));
		ua_write(ua, UA_REG_PLAY_DMA_BASE + i * 8 + 4,
			 upper_32_bits(addr));

		addr = audio->rec_addr + (dma_addr_t)i * UA_PCIE_PAGE_SIZE;
		ua_write(ua, UA_REG_REC_DMA_BASE + i * 8,
			 lower_32_bits(addr));
		ua_write(ua, UA_REG_REC_DMA_BASE + i * 8 + 4,
			 upper_32_bits(addr));
	}

	/* No doorbell — kext ProgramRegisters writes SG directly without one */

	/*
	 * Enable per-channel DMA engines.
	 *
	 * reset_dma() clears DMA_CTRL bits [8:1] (per-engine enables) and
	 * only keeps bit 0 (global enable).  We must re-enable the engines
	 * after programming the SG table, before transport start.
	 */
	{
		u32 dma_ctrl = ua_read(ua, UA_REG_DMA_CTRL);

		/*
		 * Enable global DMA + per-engine enables, clear reset
		 * strobes.  On warm boot, reset_dma() is skipped so
		 * bit 0 (global enable) may not be set.
		 */
		if (ua->fw_v2)
			dma_ctrl = (dma_ctrl | 0x1FF) & ~0x1FE00;
		else
			dma_ctrl = (dma_ctrl | 0x1F) & ~0x1E00;

		ua_write(ua, UA_REG_DMA_CTRL, dma_ctrl);
		dev_info(&ua->pdev->dev, "  DMA_CTRL after enable: 0x%08x\n",
			 ua_read(ua, UA_REG_DMA_CTRL));
	}

	/* Kext ProgramRegisters enables notification vector (0x28) here */
	ua_enable_vector(ua, UA_IRQ_VEC_NOTIFICATION);

	dev_info(&ua->pdev->dev, "  SG[0]: play=0x%08x rec=0x%08x\n",
		 lower_32_bits(audio->play_addr),
		 lower_32_bits(audio->rec_addr));
	dev_info(&ua->pdev->dev, "  SG[1023]: play=0x%08x rec=0x%08x\n",
		 lower_32_bits(audio->play_addr + 1023ULL * UA_PCIE_PAGE_SIZE),
		 lower_32_bits(audio->rec_addr + 1023ULL * UA_PCIE_PAGE_SIZE));
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
	if (!audio->play_channels) {
		ua_get_model_channels(ua);
		audio->buf_frame_size =
			ua_calc_buf_frame_size(audio->play_channels,
					       audio->rec_channels);
	}

	/* Skip if DMA buffers already allocated */
	if (audio->play_buf)
		return 0;

	ret = ua_audio_alloc_dma(ua);
	if (ret)
		return ret;

	ua_audio_program_sg(ua);
	return 0;
}

/* ----------------------------------------------------------------
 * Channel name SRAM readout (for reverse engineering)
 *
 * Firmware populates channel name data at 0xC1A4 (input, 72 × u32)
 * and 0xC2C4 (output, 72 × u32) after connect.  Format unknown —
 * dump as hex for offline analysis.
 *
 * Enable with: echo 'module ua_apollo +p' > /sys/kernel/debug/dynamic_debug/control
 * ---------------------------------------------------------------- */

#define UA_CHAN_NAME_WORDS  72

static void ua_dump_channel_names(struct ua_device *ua)
{
	u32 in_buf[UA_CHAN_NAME_WORDS];
	u32 out_buf[UA_CHAN_NAME_WORDS];
	unsigned int i;

	for (i = 0; i < UA_CHAN_NAME_WORDS; i++) {
		in_buf[i] = ua_read(ua, UA_REG_IN_NAMES_BASE + i * 4);
		out_buf[i] = ua_read(ua, UA_REG_OUT_NAMES_BASE + i * 4);
	}

	dev_dbg(&ua->pdev->dev, "input channel name SRAM (0x%x, %u words):\n",
		UA_REG_IN_NAMES_BASE, UA_CHAN_NAME_WORDS);
	print_hex_dump_debug("  in:  ", DUMP_PREFIX_OFFSET, 32, 4,
			     in_buf, sizeof(in_buf), false);

	dev_dbg(&ua->pdev->dev, "output channel name SRAM (0x%x, %u words):\n",
		UA_REG_OUT_NAMES_BASE, UA_CHAN_NAME_WORDS);
	print_hex_dump_debug("  out: ", DUMP_PREFIX_OFFSET, 32, 4,
			     out_buf, sizeof(out_buf), false);
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
		ua_write(ua, UA_REG_NOTIF_STATUS, 0);

		/* Log notification events (first 10 + every 100th) */
		if (audio->dsp_service_count < 10 ||
		    (audio->dsp_service_count % 100) == 0)
			dev_info(&ua->pdev->dev,
				 "dsp_service[%u]: notif=0x%08x (cleared)\n",
				 audio->dsp_service_count, notif);
	}

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

		if (!ua->mixer_rb_seen) {
			ua->mixer_rb_seen = true;
			dev_info(&ua->pdev->dev,
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
			dev_info(&ua->pdev->dev,
				 "dsp_service[%u]: rb=1 SEQ_WR=%u SEQ_RD=%u data[0:3]=%08x %08x %08x %08x\n",
				 audio->dsp_service_count,
				 seq_wr, seq_rd,
				 data[0], data[1], data[2], data[3]);
		else
			dev_info(&ua->pdev->dev,
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

	dev_info(&ua->pdev->dev, "starting DSP service loop (%u ms)\n",
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

	dev_info(&ua->pdev->dev, "stopping DSP service loop (%u cycles)\n",
		 audio->dsp_service_count);

	audio->dsp_service_running = false;
	cancel_delayed_work_sync(&audio->dsp_service_work);
}

/* Forward declarations for connect helpers */
static void ua_audio_write_connect_config(struct ua_device *ua);
static void ua_audio_read_connect_readback(struct ua_device *ua);

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

	if (ua->aceface_done)
		return 0;

	dev_info(&ua->pdev->dev,
		 "early ACEFACE: writing 0x%08x to 0x%04x, polling 0x%04x\n",
		 UA_AX_CONNECT_MAGIC, UA_REG_NOTIF_ACEFACE,
		 UA_REG_NOTIF_STATUS);

	for (retry = 0; retry < UA_CONNECT_RETRIES; retry++) {
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
			msleep(UA_CONNECT_POLL_MS);
		}
	}

	dev_warn(&ua->pdev->dev,
		 "early ACEFACE timed out after %d retries\n",
		 UA_CONNECT_RETRIES);
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
	struct ua_audio *audio = &ua->audio;
	unsigned int block_size;
	u32 readback, dma, frame_ctr;
	int ret, retries;

	/* Set up channel counts from model table */
	ua_get_model_channels(ua);

	audio->buf_frame_size = ua_calc_buf_frame_size(audio->play_channels,
						       audio->rec_channels);

	dev_info(&ua->pdev->dev,
		 "cold boot transport: play=%u rec=%u buf=%u\n",
		 audio->play_channels, audio->rec_channels,
		 audio->buf_frame_size);

	/*
	 * Full PrepareTransport register sequence (from kext disassembly).
	 * Order matters — FPGA state machine expects this sequence.
	 */

	/* BUF_SIZE (0x2240) = frameSize - 1 */
	ua_write(ua, UA_REG_AX_BUF_SIZE, audio->buf_frame_size - 1);

	/* DMA_BLKSZ (0x226C) = ((frameSize-1) * playCh) >> 10 */
	block_size = ((audio->buf_frame_size - 1) * audio->play_channels) >> 10;
	ua_write(ua, UA_REG_AX_DMA_BLKSZ, block_size);

	/* Clear SAMPLE_POS (0x2244) */
	ua_write(ua, UA_REG_AX_SAMPLE_POS, 0);

	/* Clear AX_CONTROL (0x2248) */
	ua_write(ua, UA_REG_AX_CONTROL, 0);

	/* FRAME_CFG (0x2258) = 0x10 */
	ua_write(ua, UA_REG_AX_FRAME_CFG, 0x10);

	/* Channel counts */
	ua_write(ua, UA_REG_AX_PLAY_CH, audio->play_channels);
	ua_write(ua, UA_REG_AX_REC_CH, audio->rec_channels);

	/* P2P_ROUTE (0x224C) = 0x100 | (play_ch - 1) */
	ua_write(ua, UA_REG_AX_P2P_ROUTE,
		 0x100 | (audio->play_channels - 1));

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
	dev_info(&ua->pdev->dev,
		 "cold boot transport: FRAME_CTR=0x%x after %d polls\n",
		 frame_ctr, retries + 1);

	dev_info(&ua->pdev->dev,
		 "cold boot transport started: CTRL=0x%x STATUS=0x%x\n",
		 ua_read(ua, UA_REG_AX_CONTROL), readback);

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

	dev_info(&ua->pdev->dev,
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

	dev_info(&ua->pdev->dev, "cold boot: stopped transport\n");
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

	dev_info(&ua->pdev->dev,
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

	dev_info(&ua->pdev->dev,
		 "  readback[0..7]: %08x %08x %08x %08x %08x %08x %08x %08x\n",
		 rb[0], rb[1], rb[2], rb[3], rb[4], rb[5], rb[6], rb[7]);
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
		dev_info(&ua->pdev->dev,
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
	dev_info(&ua->pdev->dev,
		 "ACEFACE connect: writing 0x%08x to 0x%04x, polling 0x%04x\n",
		 UA_AX_CONNECT_MAGIC, UA_REG_NOTIF_ACEFACE,
		 UA_REG_NOTIF_STATUS);

	for (retry = 0; retry < UA_CONNECT_RETRIES; retry++) {
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
						dev_info(&ua->pdev->dev,
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

				/* Bit 0: Read full output IO descriptors */
				{
					u32 out_buf[UA_CHAN_NAME_WORDS];
					unsigned int i;

					for (i = 0; i < UA_CHAN_NAME_WORDS; i++)
						out_buf[i] = ua_read(ua,
							UA_REG_OUT_NAMES_BASE + i * 4);
					dev_info(&ua->pdev->dev,
						 "  out IO desc: ch_count=%u [0]=%08x [1]=%08x [2]=%08x\n",
						 out_buf[4], out_buf[0],
						 out_buf[1], out_buf[2]);
				}

				/* Bit 1: Read full input IO descriptors */
				{
					u32 in_buf[UA_CHAN_NAME_WORDS];
					unsigned int i;

					for (i = 0; i < UA_CHAN_NAME_WORDS; i++)
						in_buf[i] = ua_read(ua,
							UA_REG_IN_NAMES_BASE + i * 4);
					dev_info(&ua->pdev->dev,
						 "  in  IO desc: ch_count=%u [0]=%08x [1]=%08x [2]=%08x\n",
						 in_buf[3], in_buf[0],
						 in_buf[1], in_buf[2]);
				}

				/* Bit 22: Read rate and clock info */
				{
					u32 rate_info, clock_info;

					rate_info  = ua_read(ua, UA_REG_NOTIF_RATE_INFO);
					clock_info = ua_read(ua, UA_REG_NOTIF_CLOCK_INFO);
					dev_info(&ua->pdev->dev,
						 "  rate_info=0x%08x clock_info=0x%08x\n",
						 rate_info, clock_info);
				}

				/* Bit 4: Read transport info */
				{
					u32 xport_info;

					xport_info = ua_read(ua, UA_REG_NOTIF_XPORT_INFO);
					dev_info(&ua->pdev->dev,
						 "  xport_info=0x%08x\n",
						 xport_info);
				}

				audio->connected = true;

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

					dev_info(&ua->pdev->dev,
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

					dev_info(&ua->pdev->dev,
						 "  POST-CONNECT: DMA_CTRL=0x%03x "
						 "cmd_pos=%u resp_pos=%u "
						 "cmd_pg0=%08x:%08x "
						 "SEQ WR=%u RD=%u\n",
						 dma_ctrl, cmd_pos, resp_pos,
						 cmd_pg0_hi, cmd_pg0_lo,
						 seq_w, seq_r);
				}

				/*
				 * Plugin chain activation is deferred to
				 * pcm_prepare (!transport_running path).
				 */

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
				 * Plugin chain DISABLED — it clobbers capture.
				 *
				 * The 572 SRAM_CFG + ROUTING + MODULE_ACTIVATE
				 * ring commands overwrite DSP capture routing
				 * state established by setting[24]=0x20.
				 * Confirmed: no_plugins=1 restores capture
				 * (Ch0/1 self-noise at -102/-106 dBFS).
				 *
				 * The DSP firmware handles basic routing on
				 * cold boot without the plugin chain.  Playback,
				 * capture, monitor, and preamp all work without it.
				 *
				 * TODO: identify which specific plugin chain
				 * commands are safe vs which clobber capture,
				 * then send only the safe subset.
				 */
				if (!no_plugins)
					dev_info(&ua->pdev->dev,
						 "plugin chain skipped (clobbers capture)\n");

				/*
				 * Restart transport for DSP service.
				 */
				ua_write(ua, UA_REG_AX_CONTROL,
					 UA_AX_CTRL_DMA_EN);
				usleep_range(1000, 2000);
				ua_write(ua, UA_REG_AX_CONTROL,
					 UA_AX_CTRL_START_EXT);
				dev_info(&ua->pdev->dev,
					 "  transport restarted\n");

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
	return -ETIMEDOUT;
}

static void ua_audio_disconnect(struct ua_device *ua)
{
	struct ua_audio *audio = &ua->audio;
	u32 seq_wr, seq_rd;

	/* Stop DSP service before tearing down connection */
	ua_dsp_service_stop(ua);

	if (!audio->connected)
		return;

	/* Stop transport first — clear ALL control bits */
	ua_write(ua, UA_REG_AX_CONTROL, 0);
	audio->transport_running = false;

	/* Clear DMA engine and disable all interrupts */
	ua_write(ua, UA_REG_IRQ_ENABLE, 0);
	if (ua->fw_v2)
		ua_write(ua, UA_REG_EXT_IRQ_ENABLE, 0);

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
		dev_info(&ua->pdev->dev,
			 "audio extension soft-disconnect (preserving mixer DSP)\n");
	} else {
		ua_write(ua, UA_REG_AX_CONNECT, UA_AX_DISCONNECT);
		msleep(50);
		dev_info(&ua->pdev->dev, "audio extension disconnected\n");
	}

	audio->connected = false;
}

/**
 * ua_audio_reconnect - Reset connection state and re-do ACEFACE handshake.
 *
 * Called from ioctl after userspace firmware load.  The driver's initial
 * probe-time connect attempt fails on cold boot (DSP not ready), so
 * userspace loads firmware and then triggers this to establish the
 * ACEFACE connection with the now-ready DSP.
 *
 * Caller must NOT hold ua->lock.
 */
int ua_audio_reconnect(struct ua_device *ua)
{
	struct ua_audio *audio = &ua->audio;
	int ret;

	/* Reset connection state so ua_audio_connect() will retry */
	audio->connected = false;

	ret = ua_audio_connect(ua);
	if (ret)
		return ret;

	/* Start periodic readback drain */
	ua_dsp_service_start(ua);

	return 0;
}

/* ----------------------------------------------------------------
 * Transport control
 * ---------------------------------------------------------------- */

/*
 * Prepare the transport engine.
 * Mirrors CPcieAudioExtension::PrepareTransport().
 */
static int ua_audio_prepare_transport(struct ua_device *ua)
{
	struct ua_audio *audio = &ua->audio;
	unsigned int block_size;
	int retries;
	u32 frame_ctr, acr, readback;

	audio->buf_frame_size = ua_calc_buf_frame_size(audio->play_channels,
						       audio->rec_channels);

	dev_info(&ua->pdev->dev, "=== PREPARE TRANSPORT ===\n");
	dev_info(&ua->pdev->dev, "  channels: play=%u rec=%u buf_frames=%u\n",
		 audio->play_channels, audio->rec_channels,
		 audio->buf_frame_size);

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

	/* Step 4: Enable timer vector (TIMER_CFG set later in pcm_prepare) */
	ua_enable_vector(ua, UA_IRQ_VEC_PERIODIC);

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
		ua_write(ua, UA_REG_AX_FRAME_CFG, pos_offset);
		dev_info(&ua->pdev->dev,
			 "  POS_OFFSET=0x%02x (rate_index=%d)\n",
			 pos_offset, audio->rate_index);
	}

	/* Step 9: PLAY_CH (0x2250) = playback channel count */
	ua_write(ua, UA_REG_AX_PLAY_CH, audio->play_channels);

	/* Step 10: REC_CH (0x225C) = record channel count */
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
	 */
	ua_write(ua, UA_REG_AX_P2P_ROUTE,
		 0x100 | (audio->play_channels - 1));

	dev_info(&ua->pdev->dev,
		 "  armed: BLKSZ=%u CTRL=0x%x STATUS=0x%x\n",
		 block_size, ua_read(ua, UA_REG_AX_CONTROL), readback);

	/*
	 * Step 14: Poll FRAME_CTR (0x2254) for firmware ack.
	 * Kext checks FRAME_CTR == pos_offset (rate-dependent).
	 */
	frame_ctr = 0;
	for (retries = 0; retries < 3; retries++) {
		frame_ctr = ua_read(ua, UA_REG_AX_FRAME_CTR);
		if (frame_ctr != 0)
			break;
		if (retries > 0)
			msleep(1);
	}

	dev_info(&ua->pdev->dev, "  FRAME_CTR=0x%08x after %d polls\n",
		 frame_ctr, retries + 1);

	if (frame_ctr == 0)
		dev_warn(&ua->pdev->dev, "frame counter still zero!\n");

	/* Step 15: Enable end-of-buffer vector (0x47) — kext does this here */
	ua_enable_vector(ua, UA_IRQ_VEC_END_BUFFER);

	dev_info(&ua->pdev->dev,
		 "transport prepared: %u frames, %u/%u ch, blksz=%u\n",
		 audio->buf_frame_size, audio->play_channels,
		 audio->rec_channels, block_size);

	return 0;
}

/*
 * One-time PCIe setup: disable ASPM, increase completion timeouts.
 * Called from probe/connect, NOT from pcm_prepare.
 * Walking the Thunderbolt bridge chain takes milliseconds and must
 * not run on PipeWire's real-time audio thread.
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
			dev_info(&ua->pdev->dev,
				 "  PCIe cap at 0x%x: DevCtl=0x%04x DevSta=0x%04x LnkCtl=0x%04x LnkSta=0x%04x\n",
				 pos, devctl, devsta, lnkctl, lnksta);
			dev_info(&ua->pdev->dev,
				 "  PCIe: MPS=%u MRRS=%u Speed=Gen%u Width=x%u\n",
				 128 << ((devctl >> 5) & 7),
				 128 << ((devctl >> 12) & 7),
				 lnksta & 0xF,
				 (lnksta >> 4) & 0x3F);

			/* Disable ASPM on device */
			if (lnkctl & 3) {
				dev_info(&ua->pdev->dev,
					 "  Disabling device ASPM (was 0x%x)\n",
					 lnkctl & 3);
				lnkctl &= ~3;
				pci_write_config_word(ua->pdev,
						     pos + PCI_EXP_LNKCTL,
						     lnkctl);
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
				dev_info(&ua->pdev->dev,
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

		/* Also disable ASPM and increase completion timeout on
		 * upstream bridge and all bridges up the chain */
		bridge = ua->pdev->bus->self;
		while (bridge) {
			pos = pci_find_capability(bridge, PCI_CAP_ID_EXP);
			if (pos) {
				u16 devctl2;

				pci_read_config_word(bridge, pos + PCI_EXP_LNKCTL, &lnkctl);
				pci_read_config_word(bridge, pos + PCI_EXP_LNKSTA, &lnksta);
				pci_read_config_word(bridge, pos + PCI_EXP_DEVCTL2, &devctl2);
				dev_info(&ua->pdev->dev,
					 "  Bridge %s: LnkCtl=0x%04x LnkSta=0x%04x DevCtl2=0x%04x\n",
					 pci_name(bridge), lnkctl, lnksta, devctl2);

				/* Disable ASPM */
				if (lnkctl & 3) {
					lnkctl &= ~3;
					pci_write_config_word(bridge,
							     pos + PCI_EXP_LNKCTL,
							     lnkctl);
					dev_info(&ua->pdev->dev,
						 "  -> ASPM disabled\n");
				}

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
	u32 ctrl, readback, sample_pos, frame_ctr, status;

	if (audio->transport_running)
		return 0;

	/* Start: DMA + playback + record + interrupt */
	ctrl = UA_AX_CTRL_START;

	/*
	 * Extended mode: kext checks device type against 0xA and 0x9.
	 * For Apollo x4 (type 0x1F), try BOTH values to find which works.
	 * Previous comment said 0x20F was "REQUIRED" but device has been
	 * dying consistently with it.  Try 0x0F for v2 Thunderbolt devices.
	 */
	{
		u32 fw_ver = ua_read(ua, UA_REG_FW_VERSION);
		dev_info(&ua->pdev->dev, "=== START TRANSPORT ===\n");
		dev_info(&ua->pdev->dev, "  FW_VERSION: 0x%08x, device_type: 0x%x\n",
			 fw_ver, ua->device_type);

		/* Kext uses 0x20F for all devices including Apollo x4.
		 * Bit 9 (0x200) = extended mode, may enable capture routing. */
		ctrl = UA_AX_CTRL_START_EXT;
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
		dev_info(&ua->pdev->dev,
			 "  cleared stale IRQ: lo=0x%08x hi=0x%08x\n",
			 stale_lo, stale_hi);
	}

	dev_info(&ua->pdev->dev,
		 "  IRQ mask: lo=0x%08x hi=0x%08x\n",
		 ua->irq_mask_lo, ua->irq_mask_hi);

	/* Start transport */
	dev_info(&ua->pdev->dev, "  writing AX_CONTROL = 0x%03x\n", ctrl);
	ua_write(ua, UA_REG_AX_CONTROL, ctrl);

	/* Immediate readback to verify the write took */
	readback = ua_read(ua, UA_REG_AX_CONTROL);
	sample_pos = ua_read(ua, UA_REG_AX_SAMPLE_POS);
	frame_ctr = ua_read(ua, UA_REG_AX_FRAME_CTR);
	status = ua_read(ua, UA_REG_AX_STATUS);

	dev_info(&ua->pdev->dev,
		 "  after start: CTRL=0x%08x SAMPLE_POS=0x%08x FRAME_CTR=0x%08x STATUS=0x%08x\n",
		 readback, sample_pos, frame_ctr, status);

	/* Check if device went away (Thunderbolt disconnect) */
	if (readback == 0xFFFFFFFF) {
		dev_err(&ua->pdev->dev,
			"device unreachable after transport start!\n");
		return -ENODEV;
	}

	audio->transport_running = true;

	/* Delayed check — verify DMA is advancing */
	udelay(100);
	sample_pos = ua_read(ua, UA_REG_AX_SAMPLE_POS);
	frame_ctr = ua_read(ua, UA_REG_AX_FRAME_CTR);
	dev_info(&ua->pdev->dev,
		 "  after 100us: SAMPLE_POS=0x%08x FRAME_CTR=0x%08x\n",
		 sample_pos, frame_ctr);
	dev_info(&ua->pdev->dev,
		 "  DMA_CTRL=0x%08x IRQ_LO=0x%08x IRQ_HI=0x%08x\n",
		 ua_read(ua, UA_REG_DMA_CTRL),
		 ua->irq_mask_lo, ua->irq_mask_hi);

	return 0;
}

static void ua_audio_stop_transport(struct ua_device *ua)
{
	struct ua_audio *audio = &ua->audio;

	if (!audio->transport_running)
		return;

	dev_info(&ua->pdev->dev,
		 "=== STOP TRANSPORT === connected=%d aceface=%d\n",
		 audio->connected, ua->aceface_done);

	/* Disable end-of-buffer interrupt */
	ua_disable_vector(ua, UA_IRQ_VEC_END_BUFFER);

	/* Stop: clear all control bits */
	ua_write(ua, UA_REG_AX_CONTROL, 0);
	audio->transport_running = false;

	dev_info(&ua->pdev->dev,
		 "transport stopped: AX_CTRL=0x%08x SP=0x%08x\n",
		 ua_read(ua, UA_REG_AX_CONTROL),
		 ua_read(ua, UA_REG_AX_SAMPLE_POS));
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
	u32 *scan;
	unsigned int i;
	int ret;

	if (!audio->rec_buf || !audio->connected)
		return -ENODEV;

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

	/* Fill rec_buf with canary */
	memset(audio->rec_buf, 0xDE, UA_DMA_BUF_SIZE);
	wmb();

	/* Start transport via proper prepare+start sequence */
	ret = ua_audio_prepare_transport(ua);
	if (ret)
		return ret;
	ret = ua_audio_start_transport(ua);
	if (ret)
		return ret;

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

	/* Scan buffer */
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

	/* Stop transport */
	ua_write(ua, UA_REG_AX_CONTROL, 0);
	audio->transport_running = false;

	dev_info(&ua->pdev->dev,
		 "dma_test: SP=%u canary_gone=%u audio=%u DMA_CTRL=0x%08x\n",
		 dt->sample_pos, dt->nonzero,
		 dt->non_zero_non_canary,
		 ua_read(ua, UA_REG_DMA_CTRL));

	return 0;
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

int ua_audio_set_clock(struct ua_device *ua, unsigned int rate)
{
	struct ua_audio *audio = &ua->audio;
	int rate_idx;
	u32 clock_config;
	int timeout_ms;
	u32 status;

	rate_idx = ua_rate_to_index(rate);
	if (rate_idx < 0)
		return rate_idx;

	/* clockConfig = clockSource | (rateIndex << 8) */
	clock_config = audio->clock_source | (rate_idx << 8);

	dev_info(&ua->pdev->dev, "=== SET CLOCK ===\n");
	dev_info(&ua->pdev->dev, "  rate=%u index=%d config=0x%08x\n",
		 rate, rate_idx, clock_config);

	/*
	 * Write to bank-shifted clock config register.
	 * 0xC074 = BAR0 + 0x28 + 0xC04C (bank_shift=0x0A).
	 * Previous code wrote to 0xC04C (wrong — no bank shift).
	 */
	ua_write(ua, UA_REG_NOTIF_CLOCK_CFG, clock_config);

	/* Ring the clock doorbell */
	ua_write(ua, UA_REG_AX_CONNECT, UA_AX_CLOCK_DOORBELL);

	/*
	 * Wait for DSP acknowledgment via notification status register.
	 * Read from 0xC030 (bank-shifted), NOT 0x22C0 (transport status).
	 */
	for (timeout_ms = 0; timeout_ms < UA_CLOCK_TIMEOUT_MS; timeout_ms += 10) {
		status = ua_read(ua, UA_REG_NOTIF_STATUS);

		if (status == 0xFFFFFFFF)
			return -ENODEV;

		/* Bit 22 = sample rate change ack (expected) */
		if (status & UA_NOTIF_BIT_SRATE)
			break;

		/* Bit 4 = transport/clock change */
		if (status & UA_NOTIF_BIT_TRANSPORT)
			break;

		/* Bit 5 = event signal — FPGA uses this for clock
		 * ack after ACEFACE connect (observed on hardware) */
		if (status & BIT(5))
			break;

		msleep(10);
	}

	dev_info(&ua->pdev->dev, "  clock ack: notif_status=0x%08x after %d ms\n",
		 status, timeout_ms);

	audio->sample_rate = rate;
	audio->rate_index = rate_idx;
	return 0;
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

static int ua_pcm_open(struct snd_pcm_substream *sub)
{
	struct ua_device *ua = snd_pcm_substream_chip(sub);
	struct ua_audio *audio = &ua->audio;
	struct snd_pcm_runtime *runtime = sub->runtime;
	unsigned long flags;

	dev_info(&ua->pdev->dev,
		 "pcm_open: stream=%s connected=%d transport=%d\n",
		 sub->stream == SNDRV_PCM_STREAM_PLAYBACK ? "play" : "rec",
		 audio->connected, audio->transport_running);

	/*
	 * Reject open if device is not connected (ACEFACE not done).
	 * PipeWire/ALSA will retry later.  Without this gate, PipeWire
	 * races to start transport before DSP is ready, causing
	 * 0xFFFFFFFF reads and a dead PCIe link.
	 */
	if (!ua->aceface_done) {
		dev_info(&ua->pdev->dev,
			 "pcm_open: rejecting — ACEFACE not complete\n");
		return -ENODEV;
	}

	runtime->hw = ua_pcm_hw;

	if (sub->stream == SNDRV_PCM_STREAM_PLAYBACK) {
		runtime->hw.channels_min = audio->play_channels;
		runtime->hw.channels_max = audio->play_channels;
	} else {
		runtime->hw.channels_min = audio->rec_channels;
		runtime->hw.channels_max = audio->rec_channels;
	}

	/*
	 * Constrain ALSA buffer_size: max = buf_frame_size (HW limit).
	 * Min = 64 frames to allow smaller period sizes for lower latency.
	 * The hardware SAMPLE_POS wraps at buf_frame_size; the pointer
	 * callback does sp % buffer_size so smaller buffers work.
	 */
	snd_pcm_hw_constraint_minmax(runtime, SNDRV_PCM_HW_PARAM_BUFFER_SIZE,
				     64, audio->buf_frame_size);

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

	dev_info(&ua->pdev->dev,
		 "pcm_close: stream=%s play_sub=%p rec_sub=%p transport=%d\n",
		 sub->stream == SNDRV_PCM_STREAM_PLAYBACK ? "play" : "rec",
		 audio->play_sub, audio->rec_sub, audio->transport_running);

	spin_lock_irqsave(&audio->lock, flags);
	if (sub->stream == SNDRV_PCM_STREAM_PLAYBACK)
		audio->play_sub = NULL;
	else
		audio->rec_sub = NULL;
	spin_unlock_irqrestore(&audio->lock, flags);

	/* Stop transport when both substreams are closed */
	if (!audio->play_sub && !audio->rec_sub && audio->transport_running) {
		dev_info(&ua->pdev->dev,
			 "pcm_close: both subs closed, stopping transport\n");
		ua_audio_stop_transport(ua);
	}

	return 0;
}

static int ua_pcm_hw_params(struct snd_pcm_substream *sub,
			    struct snd_pcm_hw_params *hw)
{
	/* DMA buffers are device-lifetime, pre-allocated.
	 * Data transfer is handled by the copy callback which
	 * scatters user channels into the full-width DMA buffer. */
	return 0;
}

static int ua_pcm_hw_free(struct snd_pcm_substream *sub)
{
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
	struct snd_pcm_runtime *rt = sub->runtime;
	unsigned int user_frame_sz = frames_to_bytes(rt, 1);
	unsigned int hw_ch, hw_frame_sz;
	char *dma_buf;
	unsigned long frames, start_frame, i;

	if (sub->stream == SNDRV_PCM_STREAM_PLAYBACK) {
		hw_ch = audio->play_channels;
		dma_buf = audio->play_buf;
	} else {
		hw_ch = audio->rec_channels;
		dma_buf = audio->rec_buf;
	}
	hw_frame_sz = hw_ch * UA_SAMPLE_BYTES;

	frames = bytes / user_frame_sz;
	start_frame = hwoff / user_frame_sz;

	if (sub->stream == SNDRV_PCM_STREAM_PLAYBACK) {
		for (i = 0; i < frames; i++) {
			unsigned long f = (start_frame + i) % audio->buf_frame_size;
			unsigned long dma_off = f * hw_frame_sz;

			/* Zero unused channels to prevent stale data / noise */
			if (hw_frame_sz > user_frame_sz)
				memset(dma_buf + dma_off, 0, hw_frame_sz);

			if (copy_from_iter(dma_buf + dma_off, user_frame_sz,
					   iter) != user_frame_sz)
				return -EFAULT;
		}
	} else {
		for (i = 0; i < frames; i++) {
			unsigned long f = (start_frame + i) % audio->buf_frame_size;
			unsigned long dma_off = f * hw_frame_sz;

			/* One-shot debug: dump multiple frames' raw DMA bytes */
			if (i == 0 && !audio->rec_debug_done) {
				u32 *p = (u32 *)(dma_buf + dma_off);
				u32 sp = ua_read(ua, UA_REG_AX_SAMPLE_POS);
				unsigned int j, canary_count = 0;
				u32 *scan = (u32 *)dma_buf;

				dev_info(&ua->pdev->dev,
					 "REC DMA dump: frame=%lu off=0x%lx pos=%u "
					 "[0]=%08x [1]=%08x [2]=%08x [3]=%08x\n",
					 f, dma_off, sp,
					 p[0], p[1], p[2], p[3]);

				/* Check how much canary pattern remains */
				for (j = 0; j < UA_DMA_BUF_SIZE / 4; j++) {
					if (scan[j] == 0xDEDEDEDE)
						canary_count++;
				}
				dev_info(&ua->pdev->dev,
					 "REC canary check: %u/%u dwords still 0xDEDEDEDE "
					 "(%u%% untouched)\n",
					 canary_count,
					 UA_DMA_BUF_SIZE / 4,
					 canary_count * 100 / (UA_DMA_BUF_SIZE / 4));

				/* Dump a few samples at different offsets */
				for (j = 0; j < 8; j++) {
					unsigned long off = j * 8192;

					if (off + 16 <= UA_DMA_BUF_SIZE)
						dev_info(&ua->pdev->dev,
							 "  @0x%05lx: %08x %08x %08x %08x\n",
							 off,
							 scan[off/4], scan[off/4+1],
							 scan[off/4+2], scan[off/4+3]);
				}
				audio->rec_debug_done = true;
			}

			if (copy_to_iter(dma_buf + dma_off, user_frame_sz,
					 iter) != user_frame_sz)
				return -EFAULT;
		}
	}
	return 0;
}

/*
 * Fill silence — zero out the user channel slots in the DMA buffer.
 */
static int ua_pcm_silence(struct snd_pcm_substream *sub, int channel,
			  unsigned long hwoff, unsigned long bytes)
{
	struct ua_device *ua = snd_pcm_substream_chip(sub);
	struct ua_audio *audio = &ua->audio;
	struct snd_pcm_runtime *rt = sub->runtime;
	unsigned int user_frame_sz = frames_to_bytes(rt, 1);
	unsigned int hw_ch = (sub->stream == SNDRV_PCM_STREAM_PLAYBACK) ?
			     audio->play_channels : audio->rec_channels;
	unsigned int hw_frame_sz = hw_ch * UA_SAMPLE_BYTES;
	char *dma_buf = (sub->stream == SNDRV_PCM_STREAM_PLAYBACK) ?
			audio->play_buf : audio->rec_buf;
	unsigned long frames, start_frame, i;

	frames = bytes / user_frame_sz;
	start_frame = hwoff / user_frame_sz;

	for (i = 0; i < frames; i++) {
		unsigned long f = (start_frame + i) % audio->buf_frame_size;
		unsigned long dma_off = f * hw_frame_sz;

		memset(dma_buf + dma_off, 0, hw_frame_sz);
	}
	return 0;
}

static int __attribute__((optimize("O1"))) ua_pcm_prepare(struct snd_pcm_substream *sub)
{
	struct ua_device *ua = snd_pcm_substream_chip(sub);
	struct ua_audio *audio = &ua->audio;
	int ret;

	dev_info(&ua->pdev->dev,
		 "pcm_prepare: stream=%s rate=%u channels=%u period=%lu buffer=%lu "
		 "connected=%d transport=%d aceface=%d\n",
		 sub->stream == SNDRV_PCM_STREAM_PLAYBACK ? "play" : "rec",
		 sub->runtime->rate, sub->runtime->channels,
		 sub->runtime->period_size, sub->runtime->buffer_size,
		 audio->connected, audio->transport_running, ua->aceface_done);

	/* Connect to DSP firmware if not already connected */
	if (!audio->connected) {
		ret = ua_audio_connect(ua);
		if (ret)
			return ret;

		/* DSP connected — start periodic readback drain */
		ua_dsp_service_start(ua);
	} else if (sub->runtime->rate != audio->sample_rate) {
		/*
		 * Rate change: disconnect/reconnect cycle.
		 *
		 * DTrace RE (2026-03-20) shows macOS does a full DSP
		 * disconnect/reconnect with FW reload on every rate
		 * change.  We implement the minimum: soft-disconnect
		 * (preserves mixer DSP) then re-run connect setup to
		 * re-init mixer batch and notification state.
		 *
		 * The disconnect sets connected=false, so the existing
		 * !connected branch above handles reconnect.  Then
		 * !transport_running handles transport + clock write.
		 */
		int new_idx = ua_rate_to_index(sub->runtime->rate);

		if (new_idx < 0)
			return new_idx;

		dev_info(&ua->pdev->dev,
			 "pcm_prepare: rate change %u → %u (disconnect/reconnect)\n",
			 audio->sample_rate, sub->runtime->rate);

		/* Stop hrtimer before disconnect */
		audio->period_timer_running = false;
		hrtimer_cancel(&audio->period_timer);

		/*
		 * Soft disconnect — stops transport, preserves mixer
		 * DSP, sets connected=false.  Does NOT send
		 * UA_AX_DISCONNECT when DSP SEQ is healthy.
		 */
		ua_audio_disconnect(ua);

		/* Update rate for the reconnect */
		audio->sample_rate = sub->runtime->rate;
		audio->rate_index = new_idx;

		/* Reset service count so mixer flush happens quickly
		 * (5 cycles = 0.5s instead of 50 = 5s cold boot delay) */
		audio->dsp_service_count = 0;

		/* Fall through: !connected → ua_audio_connect() re-inits
		 * mixer batch; !transport_running → transport + clock */
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
	if (!audio->transport_running) {
		dev_info(&ua->pdev->dev,
			 "pcm_prepare: transport not running — restarting "
			 "(play_sub=%p rec_sub=%p)\n",
			 audio->play_sub, audio->rec_sub);

		ret = ua_audio_prepare_transport(ua);
		if (ret)
			return ret;

		ret = ua_audio_start_transport(ua);
		if (ret)
			return ret;

		/*
		 * Clock write AFTER transport start enables capture.
		 *
		 * The clock write (0xC074 source=0xC + doorbell 4)
		 * transitions the DSP from passthrough to active
		 * processing mode.  If done BEFORE transport, the
		 * DSP activates with no audio flowing and routes
		 * silence.  After transport, playback passthrough
		 * is already established and capture routing
		 * activates correctly.
		 * (Discovered 2026-03-18: ordering is critical)
		 */
		if (ua->aceface_done) {
			/* Force source=0xC for internal clock — required for
			 * DSP active processing. Gets reset by ALSA control
			 * init, so we force it here every time. */
			audio->clock_source = 0xC;
			ua_write(ua, UA_REG_NOTIF_CLOCK_CFG,
				 (audio->rate_index << 8) | audio->clock_source);
			ua_write(ua, UA_REG_AX_CONNECT,
				 UA_AX_CLOCK_DOORBELL);
			dev_info(&ua->pdev->dev,
				 "post-transport clock write (capture enable)\n");
		}
	} else {
		dev_info(&ua->pdev->dev,
			 "pcm_prepare: transport already running, skip restart\n");
	}

	/*
	 * Program TIMER_CFG from the ALSA period size so interrupts
	 * fire at the period boundary rather than the full buffer.
	 * Note: if both substreams are open, the last prepare wins
	 * (shared timer — one interval for both directions).
	 */
	ua_write(ua, UA_REG_AX_TIMER_CFG, sub->runtime->period_size);

	/* Reset position for this direction */
	if (sub->stream == SNDRV_PCM_STREAM_PLAYBACK) {
		audio->play_pos = 0;
	} else {
		audio->rec_pos = 0;
	}

	return 0;
}

/*
 * Period elapsed polling timer.
 *
 * The Apollo's hardware periodic timer IRQ (vector 0x46, BIT 30)
 * is unreliable — fires once then stops.  Use an hrtimer to poll
 * SAMPLE_POS and call snd_pcm_period_elapsed() at ~1ms intervals.
 * This is the same approach used by USB audio drivers.
 */
static enum hrtimer_restart ua_period_timer_callback(struct hrtimer *timer)
{
	struct ua_audio *audio = container_of(timer, struct ua_audio,
					      period_timer);
	struct ua_device *ua = container_of(audio, struct ua_device, audio);
	u32 sp;
	snd_pcm_uframes_t pos, period_size;

	if (!audio->period_timer_running || !audio->transport_running)
		return HRTIMER_NORESTART;

	sp = ua_read(ua, UA_REG_AX_SAMPLE_POS);
	if (sp == 0xFFFFFFFF) {
		audio->period_timer_running = false;
		return HRTIMER_NORESTART;
	}

	/* Check if a period boundary was crossed */
	if (audio->rec_sub) {
		period_size = audio->rec_sub->runtime->period_size;
		pos = sp % audio->rec_sub->runtime->buffer_size;
		if (pos / period_size != audio->last_period_pos / period_size) {
			snd_pcm_period_elapsed(audio->rec_sub);
		}
		audio->last_period_pos = pos;
	}

	if (audio->play_sub) {
		period_size = audio->play_sub->runtime->period_size;
		pos = sp % audio->play_sub->runtime->buffer_size;
		snd_pcm_period_elapsed(audio->play_sub);
	}

	hrtimer_forward_now(timer, ms_to_ktime(1));
	return HRTIMER_RESTART;
}

static int ua_pcm_trigger(struct snd_pcm_substream *sub, int cmd)
{
	struct ua_device *ua = snd_pcm_substream_chip(sub);
	struct ua_audio *audio = &ua->audio;

	switch (cmd) {
	case SNDRV_PCM_TRIGGER_START:
	case SNDRV_PCM_TRIGGER_RESUME:
		audio->last_period_pos = 0;
		audio->period_timer_running = true;
		hrtimer_start(&audio->period_timer,
			      ms_to_ktime(1), HRTIMER_MODE_REL);
		return 0;

	case SNDRV_PCM_TRIGGER_STOP:
	case SNDRV_PCM_TRIGGER_SUSPEND:
		audio->period_timer_running = false;
		hrtimer_cancel(&audio->period_timer);
		return 0;

	default:
		return -EINVAL;
	}
}

static snd_pcm_uframes_t ua_pcm_pointer(struct snd_pcm_substream *sub)
{
	struct ua_device *ua = snd_pcm_substream_chip(sub);
	u32 sp;

	/*
	 * Read hardware sample position.
	 *
	 * SAMPLE_POS (0x2244) returns the current frame index within
	 * the circular hardware buffer, wrapping at buf_frame_size.
	 * ALSA's buffer_size may differ, so wrap to runtime buffer_size.
	 */
	sp = ua_read(ua, UA_REG_AX_SAMPLE_POS);

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
	.copy         = ua_pcm_copy,
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
	.copy         = ua_pcm_copy,
};

/* ----------------------------------------------------------------
 * IRQ dispatch — called from main ISR in ua_core.c
 *
 * The exact mapping of interrupt vectors to ISR bits needs hardware
 * verification.  For now we check ISR_HI for any audio-related
 * activity and update ALSA position tracking.
 * ---------------------------------------------------------------- */

void ua_audio_irq(struct ua_device *ua, u32 status_lo, u32 status_hi)
{
	struct ua_audio *audio = &ua->audio;
	static unsigned int irq_count;
	u32 sp;

	if (!audio->transport_running)
		return;

	/* Only process timer (BIT 30) and end-of-buffer (BIT 31) */
	if (!(status_hi & (BIT(30) | BIT(31))))
		return;

	sp = ua_read(ua, UA_REG_AX_SAMPLE_POS);

	if (irq_count < 10)
		dev_info(&ua->pdev->dev,
			 "audio_irq[%u]: hi=0x%08x SP=0x%08x\n",
			 irq_count, status_hi, sp);
	irq_count++;

	/*
	 * Re-arm one-shot interrupt vectors.
	 *
	 * Both the periodic timer (BIT 30) and end-of-buffer (BIT 31) are
	 * ONE-SHOT: after firing, the hardware clears their enable bit.
	 * We must re-enable them or the DMA engine stalls after one cycle.
	 */
	if (status_hi & BIT(30))
		ua_enable_vector(ua, UA_IRQ_VEC_PERIODIC);
	if (status_hi & BIT(31))
		ua_enable_vector(ua, UA_IRQ_VEC_END_BUFFER);

	/*
	 * Notify ALSA of period elapsed.
	 *
	 * The hardware timer fires every buf_frame_size frames (~170ms
	 * at 48kHz with 8192 frames).  We use this as the period elapsed
	 * signal.  snd_pcm_period_elapsed() must be called outside the
	 * audio spinlock since it may call back into our ops.
	 */
	if (audio->play_sub)
		snd_pcm_period_elapsed(audio->play_sub);
	if (audio->rec_sub)
		snd_pcm_period_elapsed(audio->rec_sub);
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
 * ---------------------------------------------------------------- */

/* Pack channel index + param_id into kcontrol->private_value */
#define UA_CTL_PACK(ch, param) (((ch) << 16) | (param))
#define UA_CTL_CH(pv)          ((pv) >> 16)
#define UA_CTL_PARAM(pv)       ((pv) & 0xFFFF)

/* --- Preamp Gain (INTEGER, per-channel) --- */

static int ua_preamp_gain_info(struct snd_kcontrol *kctl,
			       struct snd_ctl_elem_info *info)
{
	info->type = SNDRV_CTL_ELEM_TYPE_INTEGER;
	info->count = 1;
	info->value.integer.min = -144;
	info->value.integer.max = 65;
	info->value.integer.step = 1;
	return 0;
}

static int ua_preamp_gain_get(struct snd_kcontrol *kctl,
			      struct snd_ctl_elem_value *val)
{
	struct ua_device *ua = snd_kcontrol_chip(kctl);
	unsigned int ch = UA_CTL_CH(kctl->private_value);

	val->value.integer.value[0] = ua->audio.preamp[ch].gain;
	return 0;
}

static int ua_preamp_gain_put(struct snd_kcontrol *kctl,
			      struct snd_ctl_elem_value *val)
{
	struct ua_device *ua = snd_kcontrol_chip(kctl);
	unsigned int ch = UA_CTL_CH(kctl->private_value);
	int newval = val->value.integer.value[0];

	if (newval < -144 || newval > 65)
		return -EINVAL;

	if (newval == ua->audio.preamp[ch].gain)
		return 0;

	/* Cache-only: daemon handles gain via ioctl (DSP settings path).
	 * CLI commands to ARM MCU freeze the hardware. */
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

	if (newval == ua_preamp_switch_cached(&ua->audio.preamp[ch], param))
		return 0;

	/* Cache-only: daemon handles preamp flags via ioctl (DSP settings).
	 * CLI commands to ARM MCU freeze the hardware. */
	ua_preamp_switch_set_cache(&ua->audio.preamp[ch], param, newval);
	return 1;
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

	ret = ua_monitor_set_param(ua, 1, UA_MON_PARAM_LEVEL, (u32)newval);
	if (ret)
		return ret;

	ua->audio.monitor.level = newval;
	return 1;
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

	ret = ua_monitor_set_param(ua, 0, UA_MON_PARAM_MUTE, (u32)hw_val);
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

	ret = ua_monitor_set_param(ua, 0, UA_MON_PARAM_DIM, newval ? 1 : 0);
	if (ret)
		return ret;

	ua->audio.monitor.dim = newval;
	return 1;
}

/* --- Preamp Input Select (ENUMERATED: Mic/Line, per-channel) --- */

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

	if (idx >= ARRAY_SIZE(ua_input_select_names))
		return -EINVAL;

	newval = !!idx;
	if (newval == ua->audio.preamp[ch].mic_line)
		return 0;

	/* Cache-only: daemon handles Mic/Line via ioctl (DSP settings).
	 * CLI commands to ARM MCU freeze the hardware. */
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

	/* VERIFIED 2026-02-20: mono uses param 0x03 value=1 (same param as mute) */
	ret = ua_monitor_set_param(ua, 0, UA_MON_PARAM_MUTE, newval ? 1 : 0);
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

	val->value.enumerated.item[0] = ua->audio.monitor.source;
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
	ret = ua_monitor_set_param(ua, 1, UA_MON_PARAM_SOURCE, idx);
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

	if (param == UA_MON_PARAM_HP1_SOURCE)
		val->value.enumerated.item[0] = ua->audio.monitor.hp1_source;
	else
		val->value.enumerated.item[0] = ua->audio.monitor.hp2_source;
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

	ret = ua_monitor_set_param(ua, 1, param, idx);
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

	ret = ua_monitor_set_param(ua, 0, UA_MON_PARAM_DIM_LEVEL, hw_val);
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
	ret = ua_monitor_set_param(ua, newval ? 1 : 9,
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
	if (hw_val == ua->audio.monitor.digital_mode)
		return 0;

	ret = ua_monitor_set_param(ua, 0, UA_MON_PARAM_DIGITAL_MODE, hw_val);
	if (ret)
		return ret;

	ua->audio.monitor.digital_mode = hw_val;
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

	val->value.enumerated.item[0] = ua->audio.monitor.output_ref;
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
	ret = ua_monitor_set_param(ua, 1, UA_MON_PARAM_OUTPUT_REF, idx);
	if (ret)
		return ret;

	ua->audio.monitor.output_ref = idx;
	return 1;
}

/* --- Clock Mode (ENUMERATED: Internal/External — cache-only) --- */

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

	/* Map HW value to enum: 0x00→0 (Internal), 0x0C→1 (External) */
	val->value.enumerated.item[0] =
		(ua->audio.monitor.clock_mode == UA_CLOCK_MODE_EXTERNAL) ? 1 : 0;
	return 0;
}

static int ua_clock_mode_put(struct snd_kcontrol *kctl,
			     struct snd_ctl_elem_value *val)
{
	struct ua_device *ua = snd_kcontrol_chip(kctl);
	unsigned int idx = val->value.enumerated.item[0];
	int hw_val;

	if (idx >= ARRAY_SIZE(ua_clock_mode_names))
		return -EINVAL;

	hw_val = idx ? UA_CLOCK_MODE_EXTERNAL : UA_CLOCK_MODE_INTERNAL;
	if (hw_val == ua->audio.monitor.clock_mode)
		return 0;

	/*
	 * Clock Mode is SEL163, NOT a SEL131 mixer param.
	 * Cache-only — the daemon handles actual HW write.
	 */
	ua->audio.monitor.clock_mode = hw_val;
	return 1;
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
	ret = ua_monitor_set_param(ua, 9, param, hw_val);
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
	ret = ua_monitor_set_param(ua, 9, param, newval ? 1 : 0);
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
	ret = ua_monitor_set_param(ua, 9, UA_MON_PARAM_DIGITAL_MIRROR,
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
	ret = ua_monitor_set_param(ua, 0, UA_MON_PARAM_IDENTIFY,
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
	ret = ua_monitor_set_param(ua, 1, UA_MON_PARAM_SR_CONVERT,
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
	ret = ua_monitor_set_param(ua, 0, UA_MON_PARAM_DSP_SPANNING,
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
	ret = ua_monitor_set_param(ua, 0, param, newval ? 1 : 0);
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
	ret = ua_monitor_set_param(ua, 0, param, (u32)hw_val);
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
	val->value.enumerated.item[0] = ua->audio.rate_index - 1;
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

static int ua_clock_source_info(struct snd_kcontrol *kctl,
				struct snd_ctl_elem_info *info)
{
	return snd_ctl_enum_info(info, 1, ARRAY_SIZE(ua_clock_source_names),
				 ua_clock_source_names);
}

static int ua_clock_source_get(struct snd_kcontrol *kctl,
			       struct snd_ctl_elem_value *val)
{
	struct ua_device *ua = snd_kcontrol_chip(kctl);

	val->value.enumerated.item[0] = ua->audio.clock_source;
	return 0;
}

static int ua_clock_source_put(struct snd_kcontrol *kctl,
			       struct snd_ctl_elem_value *val)
{
	struct ua_device *ua = snd_kcontrol_chip(kctl);
	unsigned int src = val->value.enumerated.item[0];
	int ret;

	if (src >= ARRAY_SIZE(ua_clock_source_names))
		return -EINVAL;

	if (src == ua->audio.clock_source)
		return 0;

	ua->audio.clock_source = src;
	ret = ua_audio_set_clock(ua, ua->audio.sample_rate);
	if (ret)
		return ret;

	return 1;
}

/* --- Clock control ALSA notification helper --- */

void ua_notify_clock_controls(struct ua_device *ua)
{
	struct snd_ctl_elem_id eid;

	if (!ua->audio.card)
		return;

	memset(&eid, 0, sizeof(eid));
	eid.iface = SNDRV_CTL_ELEM_IFACE_CARD;

	strscpy(eid.name, "Sample Rate", sizeof(eid.name));
	snd_ctl_notify(ua->audio.card, SNDRV_CTL_EVENT_MASK_VALUE, &eid);

	strscpy(eid.name, "Clock Source", sizeof(eid.name));
	snd_ctl_notify(ua->audio.card, SNDRV_CTL_EVENT_MASK_VALUE, &eid);
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
	struct snd_card *card = audio->card;
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
		snprintf(name, sizeof(name), "Line %u Capture Volume", ch + 1);
		ctl.name = name;
		ctl.info = ua_preamp_gain_info;
		ctl.get = ua_preamp_gain_get;
		ctl.put = ua_preamp_gain_put;
		ctl.private_value = UA_CTL_PACK(ch, UA_ARM_PARAM_GAIN);
		kctl = snd_ctl_new1(&ctl, ua);
		if (!kctl)
			return -ENOMEM;
		ret = snd_ctl_add(card, kctl);
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
		ret = snd_ctl_add(card, kctl);
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
		ret = snd_ctl_add(card, kctl);
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
		ret = snd_ctl_add(card, kctl);
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
		ret = snd_ctl_add(card, kctl);
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
		ret = snd_ctl_add(card, kctl);
		if (ret)
			return ret;
		total++;

		/* HiZ: hardware auto-detect only, no software control */
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
		ret = snd_ctl_add(card, kctl);
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
		ret = snd_ctl_add(card, kctl);
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
		ret = snd_ctl_add(card, kctl);
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
		ret = snd_ctl_add(card, kctl);
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
		ret = snd_ctl_add(card, kctl);
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
		ret = snd_ctl_add(card, kctl);
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
		ret = snd_ctl_add(card, kctl);
		if (ret)
			return ret;
		total++;
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
		ret = snd_ctl_add(card, kctl);
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
		ret = snd_ctl_add(card, kctl);
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
		ret = snd_ctl_add(card, kctl);
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
		ret = snd_ctl_add(card, kctl);
		if (ret)
			return ret;
		total++;
	}

	/* Clock Mode (cache-only — SEL163, not a mixer param) */
	{
		static const struct snd_kcontrol_new clk_mode = {
			.iface = SNDRV_CTL_ELEM_IFACE_CARD,
			.name = "Clock Mode",
			.info = ua_clock_mode_info,
			.get = ua_clock_mode_get,
			.put = ua_clock_mode_put,
		};

		kctl = snd_ctl_new1(&clk_mode, ua);
		if (!kctl)
			return -ENOMEM;
		ret = snd_ctl_add(card, kctl);
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
		ret = snd_ctl_add(card, kctl);
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
		ret = snd_ctl_add(card, kctl);
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
		ret = snd_ctl_add(card, kctl);
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
		ret = snd_ctl_add(card, kctl);
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
		ret = snd_ctl_add(card, kctl);
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
		ret = snd_ctl_add(card, kctl);
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
		ret = snd_ctl_add(card, kctl);
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
		ret = snd_ctl_add(card, kctl);
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
		ret = snd_ctl_add(card, kctl);
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
		ret = snd_ctl_add(card, kctl);
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
		ret = snd_ctl_add(card, kctl);
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
		ret = snd_ctl_add(card, kctl);
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
		ret = snd_ctl_add(card, kctl);
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
		ret = snd_ctl_add(card, kctl);
		if (ret)
			return ret;
		total++;
	}

	dev_info(&ua->pdev->dev,
		 "ALSA mixer: %u controls (%u preamp ch × 6, 23 monitor/routing, 3 clock)\n",
		 total, audio->num_preamps);

	return 0;
}

/* ----------------------------------------------------------------
 * ALSA card and PCM device registration
 * ---------------------------------------------------------------- */

__attribute__((optimize("Os"))) int ua_audio_init(struct ua_device *ua)
{
	struct ua_audio *audio = &ua->audio;
	struct snd_card *card;
	struct snd_pcm *pcm;
	int ret;

	spin_lock_init(&audio->lock);
	INIT_DELAYED_WORK(&audio->dsp_service_work, ua_dsp_service_handler);
	hrtimer_setup(&audio->period_timer, ua_period_timer_callback,
		      CLOCK_MONOTONIC, HRTIMER_MODE_REL);
	init_completion(&audio->connect_done);
	audio->sample_rate = 48000;
	audio->rate_index = UA_RATE_48000;
	/* Use source=0xC for internal clock — enables DSP processing after
	 * transport start. UA_CLOCK_INTERNAL (0) doesn't get FPGA ack. */
	audio->clock_source = 0xC;

	/* Determine channel counts for this model.
	 * ua_audio_preinit_dma() may have already set these up. */
	if (!audio->play_channels) {
		ua_get_model_channels(ua);
		audio->buf_frame_size =
			ua_calc_buf_frame_size(audio->play_channels,
					       audio->rec_channels);
	}

	dev_info(&ua->pdev->dev, "audio: %u play ch, %u rec ch, %u frame buf\n",
		 audio->play_channels, audio->rec_channels,
		 audio->buf_frame_size);

	/* Allocate DMA buffers (skip if ua_audio_preinit_dma already did it) */
	if (!audio->play_buf) {
		ret = ua_audio_alloc_dma(ua);
		if (ret)
			return ret;
	}

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
		audio->monitor.clock_mode = UA_CLOCK_MODE_INTERNAL;
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

	/*
	 * Connect to DSP firmware at init time.
	 *
	 * The macOS kext defers this to first stream open, but we need
	 * the firmware connected early so the CLI interface (ARM MCU)
	 * is responsive for userspace mixer control via ioctl.
	 * Without this handshake, CLI commands timeout (status=0x24).
	 *
	 * Non-fatal: if connect fails (e.g. FW not ready), ALSA
	 * prepare will retry when a stream is actually opened.
	 */
	ua->skip_bus_coeff = skip_bus_coeff;

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
			/* Not fatal — reset ret so probe succeeds */
			ret = 0;
		} else {
			/* DSP connected — start periodic readback drain */
			ua_dsp_service_start(ua);
		}
	}

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

	/* Free ALSA resources — disconnect first so any open fds see errors */
	if (audio->card) {
		snd_card_disconnect(audio->card);
		snd_card_free_when_closed(audio->card);
		audio->card = NULL;
	}

	/* Free DMA buffers */
	ua_audio_free_dma(ua);
}
