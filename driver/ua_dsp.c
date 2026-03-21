// SPDX-License-Identifier: GPL-2.0-only
/*
 * Universal Audio Apollo — DSP Ring Buffer & Firmware Loading
 *
 * Copyright (c) 2026 Open Apollo contributors
 *
 * Reverse-engineered from vendor driver analysis:
 *   Ring buffer management protocol
 *   Firmware DMA transfer protocol
 *   DSP handshake sequence
 *
 * Each SHARC DSP has a 128-byte register bank split into two rings:
 *   cmd ring  at bank_base + 0x00 (host → DSP commands)
 *   resp ring at bank_base + 0x40 (DSP → host responses)
 *
 * Ring entry format (16 bytes):
 *   Inline command: { cmd_code, param, 0, 0 }
 *   DMA reference:  { size_dwords | BIT(31), 0, bus_addr_lo, bus_addr_hi }
 *
 * Firmware loading protocol:
 *   1. Allocate DMA pages for command header + firmware data + response
 *   2. Build command words (0x120000 cmd, 0x80040000 param)
 *   3. Submit DMA reference entries to cmd ring + resp ring
 *   4. Ring doorbell, poll response buffer for 0x8004xxxx
 */

#include <linux/delay.h>
#include <linux/dma-mapping.h>
#include <linux/firmware.h>
#include <linux/slab.h>

#include "ua_dsp_programs.h"
#include "ua_dsp_modules.h"

#include "ua_apollo.h"

/* Internal ring entry layout */
struct ua_ring_entry {
	__le32 word0;   /* cmd_code OR (size_dwords | 0x80000000) */
	__le32 word1;   /* param OR 0 (DMA ref) */
	__le32 word2;   /* 0 OR bus_addr_lo (DMA ref) */
	__le32 word3;   /* 0 OR bus_addr_hi (DMA ref) */
};

/* ----------------------------------------------------------------
 * Ring buffer allocation / deallocation
 * ---------------------------------------------------------------- */

static int ua_dsp_ring_alloc(struct ua_device *ua, struct ua_dsp_ring *ring,
			     unsigned int num_pages)
{
	unsigned int i;

	if (num_pages > UA_RING_MAX_PAGES)
		return -EINVAL;

	memset(ring, 0, sizeof(*ring));
	ring->num_pages = num_pages;
	ring->capacity = num_pages * UA_RING_ENTRIES_PER_PAGE;

	for (i = 0; i < num_pages; i++) {
		ring->pages[i] = dma_alloc_coherent(&ua->pdev->dev,
						     UA_PCIE_PAGE_SIZE,
						     &ring->page_addrs[i],
						     GFP_KERNEL);
		if (!ring->pages[i])
			goto err_free;
		memset(ring->pages[i], 0, UA_PCIE_PAGE_SIZE);
	}

	return 0;

err_free:
	while (i--) {
		dma_free_coherent(&ua->pdev->dev, UA_PCIE_PAGE_SIZE,
				  ring->pages[i], ring->page_addrs[i]);
		ring->pages[i] = NULL;
	}
	ring->num_pages = 0;
	ring->capacity = 0;
	return -ENOMEM;
}

static void ua_dsp_ring_free(struct ua_device *ua, struct ua_dsp_ring *ring)
{
	unsigned int i;

	for (i = 0; i < ring->num_pages; i++) {
		if (ring->pages[i]) {
			dma_free_coherent(&ua->pdev->dev, UA_PCIE_PAGE_SIZE,
					  ring->pages[i], ring->page_addrs[i]);
			ring->pages[i] = NULL;
		}
	}
	ring->num_pages = 0;
	ring->capacity = 0;
	ring->write_ptr = 0;
}

/* ----------------------------------------------------------------
 * Ring buffer hardware programming
 * ---------------------------------------------------------------- */

/**
 * ua_dsp_ring_program - Write page addresses to hardware registers
 * @ua: device
 * @ring: ring buffer to program
 * @ring_base: BAR0 offset of the ring's register block (cmd or resp)
 *
 * Mirrors hardware driver ring buffer register programming:
 *   Write page addresses to PAGE0_LO..PAGE3_HI (0x00..0x1C)
 *   Write 0 to WRITE_PTR (0x20)
 *   Reset software write pointer
 */
static void ua_dsp_ring_program(struct ua_device *ua,
				struct ua_dsp_ring *ring,
				u32 ring_base)
{
	static const u32 page_offsets[] = {
		UA_RING_PAGE0_LO, UA_RING_PAGE0_HI,
		UA_RING_PAGE1_LO, UA_RING_PAGE1_HI,
		UA_RING_PAGE2_LO, UA_RING_PAGE2_HI,
		UA_RING_PAGE3_LO, UA_RING_PAGE3_HI,
	};
	unsigned int i;

	/* Write page addresses (zeroed for unused pages) */
	for (i = 0; i < UA_RING_MAX_PAGES; i++) {
		u64 addr = (i < ring->num_pages) ? ring->page_addrs[i] : 0;

		ua_write(ua, ring_base + page_offsets[i * 2],
			 lower_32_bits(addr));
		ua_write(ua, ring_base + page_offsets[i * 2 + 1],
			 upper_32_bits(addr));
	}

	/*
	 * Reset write pointer.
	 *
	 * CRITICAL: Only write WRITE_PTR (0x20) here — do NOT write
	 * DOORBELL (0x24).  Writing DOORBELL=0 during init puts the
	 * FPGA in a state where it ignores future entries.  The hardware driver's
	 * ProgramRegisters writes both, but that's called in a different
	 * hardware state (after full transport+ACEFACE init).
	 */
	ring->write_ptr = 0;
	ua_write(ua, ring_base + UA_RING_WRITE_PTR, 0);

	dev_dbg(&ua->pdev->dev,
		"ring @0x%04x: program write_ptr=0\n", ring_base);

	/* Verify page addresses read back correctly */
	for (i = 0; i < ring->num_pages; i++) {
		u32 rd_lo = ua_read(ua, ring_base + page_offsets[i * 2]);
		u32 rd_hi = ua_read(ua, ring_base + page_offsets[i * 2 + 1]);
		u32 wr_lo = lower_32_bits(ring->page_addrs[i]);
		u32 wr_hi = upper_32_bits(ring->page_addrs[i]);

		if (rd_lo != wr_lo || rd_hi != wr_hi)
			dev_warn(&ua->pdev->dev,
				 "ring @0x%04x page%u MISMATCH: "
				 "wrote %08x:%08x read %08x:%08x\n",
				 ring_base, i, wr_hi, wr_lo, rd_hi, rd_lo);
	}

	/* Zero ring pages */
	for (i = 0; i < ring->num_pages; i++)
		memset(ring->pages[i], 0, UA_PCIE_PAGE_SIZE);
}

/* ----------------------------------------------------------------
 * Ring entry submission
 * ---------------------------------------------------------------- */

/**
 * ua_dsp_ring_put_entry - Append a 16-byte entry to the ring
 * @ring: target ring buffer
 * @entry: 16-byte entry to copy
 *
 * Returns 0 on success, -ENOSPC if ring is full.
 */
static int ua_dsp_ring_put_entry(struct ua_dsp_ring *ring,
				 const struct ua_ring_entry *entry)
{
	unsigned int page_idx, offset;
	struct ua_ring_entry *slot;

	if (ring->write_ptr >= ring->capacity)
		return -ENOSPC;

	page_idx = ring->write_ptr / UA_RING_ENTRIES_PER_PAGE;
	offset = (ring->write_ptr % UA_RING_ENTRIES_PER_PAGE) *
		 UA_RING_ENTRY_SIZE;

	slot = (struct ua_ring_entry *)((u8 *)ring->pages[page_idx] + offset);
	*slot = *entry;

	ring->write_ptr++;
	return 0;
}

/**
 * ua_dsp_ring_doorbell - Notify DSP of new entries
 * @ua: device
 * @ring: ring that has new entries
 * @ring_base: BAR0 offset of the ring's register block
 *
 * Hardware driver ring buffer service writes DOORBELL (0x24) FIRST,
 * then WRITE_PTR (0x20).  This order matters: DOORBELL sets the
 * target write pointer, WRITE_PTR triggers processing.  Writing
 * WRITE_PTR first (our previous code) caused the FPGA to begin
 * DMA reads immediately, and the subsequent DOORBELL write during
 * an active large DMA ref transfer reset the DMA engine, causing
 * permanent ring stalls for payloads > ~1.5 KB.
 */
static void ua_dsp_ring_doorbell(struct ua_device *ua,
				 struct ua_dsp_ring *ring,
				 u32 ring_base)
{
	/* Ensure all ring writes are visible before doorbell */
	wmb();

	/*
	 * Kext Service() order: DOORBELL (0x24) first, WRITE_PTR (0x20) second.
	 * DOORBELL tells FPGA "process up to this write_ptr" immediately.
	 * WRITE_PTR persists the value for re-reads.
	 */
	ua_write(ua, ring_base + UA_RING_DOORBELL, ring->write_ptr);
	ua_write(ua, ring_base + UA_RING_WRITE_PTR, ring->write_ptr);

	dev_dbg(&ua->pdev->dev,
		"ring @0x%04x: doorbell w_ptr=%u\n",
		ring_base, ring->write_ptr);
}

/* ----------------------------------------------------------------
 * Ring init / cleanup for all DSPs
 * ---------------------------------------------------------------- */

/**
 * ua_dsp_rings_init - Allocate and program rings for all DSPs
 * @ua: device
 *
 * DSP 0 gets a larger cmd ring (2 pages = 512 entries) for firmware
 * loading.  Other DSPs get 1-page rings.  Response rings are always
 * 1 page.
 *
 * Non-fatal on failure — driver still works for register access.
 */
int ua_dsp_rings_init(struct ua_device *ua)
{
	unsigned int i;
	int ret;

	for (i = 0; i < ua->num_dsps; i++) {
		struct ua_dsp_state *ds = &ua->dsp[i];
		u32 bank_base = ua_dsp_base(i);
		unsigned int cmd_pages;

		/* DSP 0 needs more room for firmware DMA refs */
		cmd_pages = (i == 0) ? UA_FW_RING_NUM_PAGES :
					UA_RING_NUM_PAGES;

		ret = ua_dsp_ring_alloc(ua, &ds->cmd, cmd_pages);
		if (ret) {
			dev_warn(&ua->pdev->dev,
				 "DSP %u: cmd ring alloc failed (%d)\n",
				 i, ret);
			continue;
		}

		ret = ua_dsp_ring_alloc(ua, &ds->resp, UA_RING_NUM_PAGES);
		if (ret) {
			dev_warn(&ua->pdev->dev,
				 "DSP %u: resp ring alloc failed (%d)\n",
				 i, ret);
			ua_dsp_ring_free(ua, &ds->cmd);
			continue;
		}

		/* Program hardware registers */
		dev_info(&ua->pdev->dev,
			 "DSP %u: programming rings at BAR0+0x%04x "
			 "(cmd @+0x%02x, resp @+0x%02x)\n",
			 i, bank_base,
			 UA_RING_CMD_OFFSET, UA_RING_RESP_OFFSET);
		dev_info(&ua->pdev->dev,
			 "DSP %u: cmd page0 DMA=%pad, resp page0 DMA=%pad\n",
			 i, &ds->cmd.page_addrs[0], &ds->resp.page_addrs[0]);

		ua_dsp_ring_program(ua, &ds->cmd,
				    bank_base + UA_RING_CMD_OFFSET);
		ua_dsp_ring_program(ua, &ds->resp,
				    bank_base + UA_RING_RESP_OFFSET);

		ds->rings_allocated = true;

		/* Verify ring position registers are accessible */
		{
			u32 cmd_pos = ua_read(ua, bank_base +
					      UA_RING_CMD_OFFSET +
					      UA_RING_POSITION);
			u32 resp_pos = ua_read(ua, bank_base +
					       UA_RING_RESP_OFFSET +
					       UA_RING_POSITION);
			dev_info(&ua->pdev->dev,
				 "DSP %u: rings ready (cmd=%u resp=%u entries) "
				 "pos: cmd=%u resp=%u\n",
				 i, ds->cmd.capacity, ds->resp.capacity,
				 cmd_pos, resp_pos);
		}
	}

	/*
	 * Enable per-channel DMA engines — required for ring buffer
	 * scatter-gather reads.  Without this, the FPGA won't process
	 * ring entries (POSITION stays at 0).
	 */
	if (ua->fw_v2) {
		u32 dma = ua_read(ua, UA_REG_DMA_CTRL);

		dev_info(&ua->pdev->dev,
			 "DMA_CTRL before engine enable: 0x%08x\n", dma);
		/* hardware driver uses 0x1FFFE (16 engines); we had 0x1FE (9).
		 * More engines may be needed for type 0x01 capture channels. */
		ua_write(ua, UA_REG_DMA_CTRL, dma | 0x1FFFE);
		ua->dma_ctrl_cache = ua_read(ua, UA_REG_DMA_CTRL);
		dev_info(&ua->pdev->dev,
			 "DMA_CTRL after engine enable: 0x%08x\n",
			 ua->dma_ctrl_cache);

		/*
		 * CRITICAL: Pulse DMA reset with engines already enabled.
		 * Without this, the FPGA ring buffer engine stays dormant
		 * and ignores doorbells (POSITION stuck at 0).
		 * Discovered empirically: reset-while-enabled
		 * kicks the ring buffer DMA engine into active state.
		 */
		ua_write(ua, UA_REG_DMA_CTRL,
			 ua->dma_ctrl_cache | UA_DMA_RESET_V2_MASK);
		ua_write(ua, UA_REG_DMA_CTRL, ua->dma_ctrl_cache);
		(void)ua_read(ua, UA_REG_DMA_CTRL);  /* flush */

		dev_info(&ua->pdev->dev,
			 "DMA_CTRL after ring enable pulse: 0x%08x\n",
			 ua_read(ua, UA_REG_DMA_CTRL));
	}

	/*
	 * Dump unknown registers in the DMA control area.
	 * Some of these might control ring buffer DMA enables.
	 */
	{
		u32 r2210 = ua_read(ua, 0x2210);
		u32 r2214 = ua_read(ua, 0x2214);
		u32 r221c = ua_read(ua, 0x221C);
		u32 r2220 = ua_read(ua, 0x2220);
		u32 r2224 = ua_read(ua, 0x2224);
		u32 r2228 = ua_read(ua, 0x2228);
		u32 r222c = ua_read(ua, 0x222C);
		u32 r2230 = ua_read(ua, 0x2230);
		u32 r2234 = ua_read(ua, 0x2234);
		u32 r2238 = ua_read(ua, 0x2238);
		u32 r223c = ua_read(ua, 0x223C);

		dev_info(&ua->pdev->dev,
			 "DMA area: 2210=%08x 2214=%08x "
			 "221C=%08x 2220=%08x 2224=%08x\n",
			 r2210, r2214, r221c, r2220, r2224);
		dev_info(&ua->pdev->dev,
			 "DMA area: 2228=%08x 222C=%08x "
			 "2230=%08x 2234=%08x 2238=%08x 223C=%08x\n",
			 r2228, r222c, r2230, r2234, r2238, r223c);
	}

	dev_info(&ua->pdev->dev, "DSP ring buffers initialized\n");
	return 0;
}

void ua_dsp_rings_fini(struct ua_device *ua)
{
	unsigned int i;

	for (i = 0; i < ua->num_dsps; i++) {
		struct ua_dsp_state *ds = &ua->dsp[i];

		if (!ds->rings_allocated)
			continue;

		ua_dsp_ring_free(ua, &ds->cmd);
		ua_dsp_ring_free(ua, &ds->resp);
		ds->rings_allocated = false;
	}

	ua->fw_loaded = false;
	ua->dsps_connected = false;
}

/**
 * ua_dsp_rings_reprogram - Re-write page addresses to FPGA after DMA reset
 * @ua: device
 *
 * The DMA reset pulse in ua_boot_transport_start() deactivates the FPGA's
 * ring buffer DMA engine even though the page address registers retain
 * their values.  Re-writing the same page addresses (via ring_program)
 * re-initializes the engine, allowing it to process DMA ref entries again.
 *
 * Must be called after ua_boot_transport_start() and before any
 * ua_dsp_send_block() calls.
 */
void ua_dsp_rings_reprogram(struct ua_device *ua)
{
	unsigned int i;

	for (i = 0; i < ua->num_dsps; i++) {
		struct ua_dsp_state *ds = &ua->dsp[i];
		u32 bank_base;

		if (!ds->rings_allocated)
			continue;

		bank_base = ua_dsp_base(i);
		ua_dsp_ring_program(ua, &ds->cmd,
				    bank_base + UA_RING_CMD_OFFSET);
		ua_dsp_ring_program(ua, &ds->resp,
				    bank_base + UA_RING_RESP_OFFSET);
	}

	dev_info(&ua->pdev->dev,
		 "DSP rings reprogrammed after transport start\n");
}

/* ----------------------------------------------------------------
 * Firmware loading
 * ---------------------------------------------------------------- */

/**
 * ua_dsp_load_firmware - Load firmware binary to DSP 0 via ring buffer DMA
 * @ua: device (must hold ua->lock)
 * @fw_data: kernel buffer containing firmware binary
 * @fw_size: size of firmware in bytes
 *
 * Protocol (from hardware driver firmware load analysis):
 *   1. Allocate single contiguous DMA buffer for [cmd, count, param, fw_data]
 *   2. Allocate response DMA buffer
 *   3. Submit ONE DMA ref entry to cmd ring (entire payload)
 *   4. Submit ONE DMA ref entry to resp ring
 *   5. Ring doorbells (resp first, then cmd)
 *   6. Poll response buffer for 0x8004xxxx
 *   7. Free DMA buffers
 *
 * The hardware driver uses a single large DMA buffer per sendBlock call.
 * The FPGA processes one ring entry per logical command — chained
 * entries do NOT work for firmware loading.
 */
int ua_dsp_load_firmware(struct ua_device *ua, const void *fw_data,
			 size_t fw_size)
{
	return ua_dsp_load_firmware_to(ua, 0, fw_data, fw_size);
}

/**
 * ua_dsp_load_firmware_to - Load firmware to a specific DSP
 */
int ua_dsp_load_firmware_to(struct ua_device *ua, unsigned int dsp_idx,
			     const void *fw_data, size_t fw_size)
{
	struct ua_dsp_state *dsp0 = &ua->dsp[dsp_idx];
	u32 bank_base = ua_dsp_base(dsp_idx);
	void *cmd_buf = NULL, *resp_buf = NULL;
	dma_addr_t cmd_addr, resp_addr;
	struct ua_ring_entry entry;
	u32 *cmd_words;
	u32 fw_dwords;
	u32 total_dwords;
	size_t cmd_buf_size;
	unsigned int polls;
	int ret;

	if (!fw_size || fw_size > UA_FW_MAX_SIZE)
		return -EINVAL;
	if (fw_size & 3)
		return -EINVAL;

	if (!dsp0->rings_allocated) {
		dev_err(&ua->pdev->dev, "DSP 0 rings not allocated\n");
		return -ENODEV;
	}

	fw_dwords = fw_size / 4;

	/*
	 * Allocate single contiguous DMA buffer for:
	 *   [cmd_word, count_word, param_word, firmware_data...]
	 * The FPGA reads this as one DMA ref entry.
	 */
	cmd_buf_size = (3 + fw_dwords) * 4;
	total_dwords = 3 + fw_dwords;

	dev_info(&ua->pdev->dev,
		 "firmware load: allocating %zu byte contiguous DMA buffer\n",
		 cmd_buf_size);

	cmd_buf = dma_alloc_coherent(&ua->pdev->dev, cmd_buf_size,
				     &cmd_addr, GFP_KERNEL);
	if (!cmd_buf) {
		dev_err(&ua->pdev->dev,
			"failed to allocate %zu byte DMA buffer for firmware\n",
			cmd_buf_size);
		return -ENOMEM;
	}

	resp_buf = dma_alloc_coherent(&ua->pdev->dev, UA_PCIE_PAGE_SIZE,
				      &resp_addr, GFP_KERNEL);
	if (!resp_buf) {
		ret = -ENOMEM;
		goto free_cmd;
	}

	/* Build command buffer: [cmd, count, param, fw_data...] */
	cmd_words = cmd_buf;
	cmd_words[0] = cpu_to_le32(UA_FW_CMD | UA_FW_LARGE_FLAG);
	cmd_words[1] = cpu_to_le32(fw_dwords + 2);
	cmd_words[2] = cpu_to_le32(UA_FW_PARAM);
	memcpy(&cmd_words[3], fw_data, fw_size);

	/* Zero response buffer */
	memset(resp_buf, 0, UA_PCIE_PAGE_SIZE);

	/* Reset and re-program rings */
	ua_dsp_ring_program(ua, &dsp0->cmd,
			    bank_base + UA_RING_CMD_OFFSET);
	ua_dsp_ring_program(ua, &dsp0->resp,
			    bank_base + UA_RING_RESP_OFFSET);

	/* Submit response ring entry FIRST (provide buffer before sending cmd) */
	entry = (struct ua_ring_entry){
		.word0 = cpu_to_le32(UA_RING_DMA_REF | (UA_PCIE_PAGE_SIZE / 4)),
		.word1 = 0,
		.word2 = cpu_to_le32(lower_32_bits(resp_addr)),
		.word3 = cpu_to_le32(upper_32_bits(resp_addr)),
	};
	ret = ua_dsp_ring_put_entry(&dsp0->resp, &entry);
	if (ret)
		goto free_resp;
	ua_dsp_ring_doorbell(ua, &dsp0->resp,
			     bank_base + UA_RING_RESP_OFFSET);

	/* Small delay to let resp ring settle */
	usleep_range(1000, 2000);

	/* Submit command ring entry: single DMA ref for entire payload */
	entry = (struct ua_ring_entry){
		.word0 = cpu_to_le32(UA_RING_DMA_REF | total_dwords),
		.word1 = 0,
		.word2 = cpu_to_le32(lower_32_bits(cmd_addr)),
		.word3 = cpu_to_le32(upper_32_bits(cmd_addr)),
	};
	ret = ua_dsp_ring_put_entry(&dsp0->cmd, &entry);
	if (ret)
		goto free_resp;

	dev_info(&ua->pdev->dev,
		 "fw cmd entry: [%08x %08x %08x %08x] cmd_addr=%pad\n",
		 le32_to_cpu(entry.word0), le32_to_cpu(entry.word1),
		 le32_to_cpu(entry.word2), le32_to_cpu(entry.word3),
		 &cmd_addr);
	dev_info(&ua->pdev->dev,
		 "fw cmd buf: [%08x %08x %08x %08x ...] total=%u dwords\n",
		 le32_to_cpu(cmd_words[0]), le32_to_cpu(cmd_words[1]),
		 le32_to_cpu(cmd_words[2]), le32_to_cpu(cmd_words[3]),
		 total_dwords);

	ua_dsp_ring_doorbell(ua, &dsp0->cmd,
			     bank_base + UA_RING_CMD_OFFSET);

	dev_info(&ua->pdev->dev,
		 "firmware load: %zu bytes, polling...\n", fw_size);

	/* Poll response buffer (DMA coherent — no lock needed) */
	ret = -ETIMEDOUT;
	for (polls = 0; polls < UA_FW_TIMEOUT_MS / UA_FW_POLL_MS; polls++) {
		u32 resp_word0 = le32_to_cpu(((u32 *)resp_buf)[0]);

		if ((resp_word0 >> 16) == UA_FW_RESP_MATCH) {
			dev_info(&ua->pdev->dev,
				 "firmware loaded successfully (response=0x%08x)\n",
				 resp_word0);
			ua->fw_loaded = true;
			ret = 0;
			break;
		}
		/* Log progress every 5 seconds */
		if (polls > 0 && (polls % 500) == 0) {
			u32 cmd_pos = ua_read(ua, bank_base +
					      UA_RING_CMD_OFFSET +
					      UA_RING_POSITION);
			u32 resp_pos = ua_read(ua, bank_base +
					       UA_RING_RESP_OFFSET +
					       UA_RING_POSITION);
			dev_info(&ua->pdev->dev,
				 "fw poll %us: cmd_pos=%u resp_pos=%u resp[0]=0x%08x\n",
				 polls / 100, cmd_pos, resp_pos, resp_word0);
		}
		msleep(UA_FW_POLL_MS);
	}

	if (ret) {
		u32 cmd_pos = ua_read(ua, bank_base + UA_RING_CMD_OFFSET +
				      UA_RING_POSITION);
		u32 resp_pos = ua_read(ua, bank_base + UA_RING_RESP_OFFSET +
				       UA_RING_POSITION);
		u32 *resp_words = resp_buf;

		dev_err(&ua->pdev->dev,
			"firmware load timeout after %u ms\n",
			polls * UA_FW_POLL_MS);
		dev_err(&ua->pdev->dev,
			"  cmd: wrote=%u pos=%u  resp: wrote=%u pos=%u\n",
			dsp0->cmd.write_ptr, cmd_pos,
			dsp0->resp.write_ptr, resp_pos);
		dev_err(&ua->pdev->dev,
			"  resp_buf: [%08x %08x %08x %08x]\n",
			le32_to_cpu(resp_words[0]),
			le32_to_cpu(resp_words[1]),
			le32_to_cpu(resp_words[2]),
			le32_to_cpu(resp_words[3]));
	}

free_resp:
	dma_free_coherent(&ua->pdev->dev, UA_PCIE_PAGE_SIZE,
			  resp_buf, resp_addr);
free_cmd:
	dma_free_coherent(&ua->pdev->dev, cmd_buf_size,
			  cmd_buf, cmd_addr);
	return ret;
}

/* ----------------------------------------------------------------
 * DMA reference entry format test
 * ---------------------------------------------------------------- */

/**
 * ua_dsp_test_dma_ref - Systematically test DMA ref entry formats
 * @ua: device (must hold ua->lock)
 *
 * Sends a known-good command ({0x00230002, 1} = DSP connect) via DMA ref
 * instead of inline, trying different entry layouts to find which one
 * the FPGA actually processes.
 *
 * First sends it inline as a baseline, then tries 6 DMA ref variations.
 * Logs results to dmesg for each format.
 */
#pragma GCC push_options
#pragma GCC optimize("O1")
int ua_dsp_test_dma_ref(struct ua_device *ua)
{
	struct ua_dsp_state *dsp0 = &ua->dsp[0];
	u32 bank_base = ua_dsp_base(0);
	u32 cmd_base = bank_base + UA_RING_CMD_OFFSET;
	void *dma_buf;
	dma_addr_t dma_addr;
	u32 *words;
	u32 pos;
	struct ua_ring_entry entry;
	int test, ret;

	if (!dsp0->rings_allocated) {
		dev_err(&ua->pdev->dev, "DMA ref test: DSP 0 rings not allocated\n");
		return -ENODEV;
	}

	/* Allocate a single DMA page for test data */
	dma_buf = dma_alloc_coherent(&ua->pdev->dev, UA_PCIE_PAGE_SIZE,
				     &dma_addr, GFP_KERNEL);
	if (!dma_buf)
		return -ENOMEM;

	dev_info(&ua->pdev->dev,
		 "=== DMA ref format test === dma_addr=%pad\n", &dma_addr);

	/*
	 * Test 0: BASELINE — inline entry (should work, proves ring is OK)
	 */
	ua_dsp_ring_program(ua, &dsp0->cmd, cmd_base);

	entry = (struct ua_ring_entry){
		.word0 = cpu_to_le32(UA_DSP_CMD_CONNECT),
		.word1 = cpu_to_le32(1),
		.word2 = 0,
		.word3 = 0,
	};
	ua_dsp_ring_put_entry(&dsp0->cmd, &entry);
	ua_dsp_ring_doorbell(ua, &dsp0->cmd, cmd_base);
	usleep_range(5000, 10000);
	pos = ua_read(ua, cmd_base + UA_RING_POSITION);
	dev_info(&ua->pdev->dev,
		 "TEST 0 (inline baseline): pos=%u %s\n",
		 pos, pos >= 1 ? "OK" : "FAIL");

	/*
	 * Write the connect command into the DMA buffer.
	 * {0x00230002, 0x00000001} = 2 dwords = 8 bytes
	 */
	memset(dma_buf, 0, UA_PCIE_PAGE_SIZE);
	words = dma_buf;
	words[0] = cpu_to_le32(UA_DSP_CMD_CONNECT);  /* 0x00230002 */
	words[1] = cpu_to_le32(1);                     /* param = 1 */

	/*
	 * Now try 6 DMA ref entry formats.
	 * Each test resets the ring, submits a DMA ref entry PLUS a
	 * follow-up inline entry (the finalize command as a canary).
	 * If pos=2, the FPGA processed the DMA ref and moved on.
	 * If pos=1, it got stuck on the DMA ref.
	 */
	for (test = 1; test <= 6; test++) {
		struct ua_ring_entry canary = {
			.word0 = cpu_to_le32(UA_DSP_CMD_FINALIZE),
			.word1 = 0,
			.word2 = 0,
			.word3 = 0,
		};

		ua_dsp_ring_program(ua, &dsp0->cmd, cmd_base);

		switch (test) {
		case 1:
			/* {ndwords|BIT31, 0, addr_lo, addr_hi} — correct format */
			entry = (struct ua_ring_entry){
				.word0 = cpu_to_le32(UA_RING_DMA_REF | 2),
				.word1 = 0,
				.word2 = cpu_to_le32(lower_32_bits(dma_addr)),
				.word3 = cpu_to_le32(upper_32_bits(dma_addr)),
			};
			break;
		case 2:
			/* {nbytes|BIT31, 0, addr_lo, addr_hi} — size in bytes */
			entry = (struct ua_ring_entry){
				.word0 = cpu_to_le32(UA_RING_DMA_REF | 8),
				.word1 = 0,
				.word2 = cpu_to_le32(lower_32_bits(dma_addr)),
				.word3 = cpu_to_le32(upper_32_bits(dma_addr)),
			};
			break;
		case 3:
			/* {ndwords|BIT31, addr_lo, addr_hi, 0} — old wrong format */
			entry = (struct ua_ring_entry){
				.word0 = cpu_to_le32(UA_RING_DMA_REF | 2),
				.word1 = cpu_to_le32(lower_32_bits(dma_addr)),
				.word2 = cpu_to_le32(upper_32_bits(dma_addr)),
				.word3 = 0,
			};
			break;
		case 4:
			/* {nbytes|BIT31, addr_lo, addr_hi, 0} — bytes + old format */
			entry = (struct ua_ring_entry){
				.word0 = cpu_to_le32(UA_RING_DMA_REF | 8),
				.word1 = cpu_to_le32(lower_32_bits(dma_addr)),
				.word2 = cpu_to_le32(upper_32_bits(dma_addr)),
				.word3 = 0,
			};
			break;
		case 5:
			/* {addr_lo|BIT31, ndwords, 0, 0} — addr in w0, size in w1 */
			entry = (struct ua_ring_entry){
				.word0 = cpu_to_le32(lower_32_bits(dma_addr) | UA_RING_DMA_REF),
				.word1 = cpu_to_le32(2),
				.word2 = cpu_to_le32(upper_32_bits(dma_addr)),
				.word3 = 0,
			};
			break;
		case 6:
			/* Full page size: {page_dwords|BIT31, 0, addr_lo, addr_hi} */
			entry = (struct ua_ring_entry){
				.word0 = cpu_to_le32(UA_RING_DMA_REF | (UA_PCIE_PAGE_SIZE / 4)),
				.word1 = 0,
				.word2 = cpu_to_le32(lower_32_bits(dma_addr)),
				.word3 = cpu_to_le32(upper_32_bits(dma_addr)),
			};
			break;
		}

		ret = ua_dsp_ring_put_entry(&dsp0->cmd, &entry);
		if (ret) {
			dev_err(&ua->pdev->dev,
				"TEST %d: put_entry failed (%d)\n", test, ret);
			continue;
		}

		/* Canary inline entry — should be consumed if DMA ref didn't stall */
		ua_dsp_ring_put_entry(&dsp0->cmd, &canary);
		ua_dsp_ring_doorbell(ua, &dsp0->cmd, cmd_base);
		usleep_range(50000, 100000);  /* 50-100ms wait */

		pos = ua_read(ua, cmd_base + UA_RING_POSITION);
		dev_info(&ua->pdev->dev,
			 "TEST %d: entry=[%08x %08x %08x %08x] pos=%u/%u %s\n",
			 test,
			 le32_to_cpu(entry.word0), le32_to_cpu(entry.word1),
			 le32_to_cpu(entry.word2), le32_to_cpu(entry.word3),
			 pos, 2,
			 pos >= 2 ? "DMA_OK" : (pos == 1 ? "DMA_STALL" : "STUCK"));
	}

	/*
	 * Test 7: Garbage DMA ref — verify FPGA reads DMA content.
	 * If pos=2, FPGA ignores content (just skips DMA refs).
	 * If pos=1, FPGA reads content and stalls on garbage.
	 */
	memset(dma_buf, 0xFF, UA_PCIE_PAGE_SIZE);
	ua_dsp_ring_program(ua, &dsp0->cmd, cmd_base);
	entry = (struct ua_ring_entry){
		.word0 = cpu_to_le32(UA_RING_DMA_REF | 2),
		.word1 = 0,
		.word2 = cpu_to_le32(lower_32_bits(dma_addr)),
		.word3 = cpu_to_le32(upper_32_bits(dma_addr)),
	};
	ua_dsp_ring_put_entry(&dsp0->cmd, &entry);
	{
		struct ua_ring_entry canary = {
			.word0 = cpu_to_le32(UA_DSP_CMD_FINALIZE),
			.word1 = 0, .word2 = 0, .word3 = 0,
		};
		ua_dsp_ring_put_entry(&dsp0->cmd, &canary);
	}
	ua_dsp_ring_doorbell(ua, &dsp0->cmd, cmd_base);
	usleep_range(50000, 100000);
	pos = ua_read(ua, cmd_base + UA_RING_POSITION);
	dev_info(&ua->pdev->dev,
		 "TEST 7 (garbage DMA ref): pos=%u/%u %s\n",
		 pos, 2,
		 pos >= 2 ? "SKIP(no read)" : (pos == 1 ? "STALL(reads!)" : "STUCK"));

	/* Restore valid data */
	memset(dma_buf, 0, UA_PCIE_PAGE_SIZE);
	words[0] = cpu_to_le32(UA_DSP_CMD_CONNECT);
	words[1] = cpu_to_le32(1);

	/*
	 * Test 8: Multiple DMA refs in sequence (no inline).
	 * This replicates the firmware load pattern.
	 */
	ua_dsp_ring_program(ua, &dsp0->cmd, cmd_base);
	for (test = 0; test < 5; test++) {
		entry = (struct ua_ring_entry){
			.word0 = cpu_to_le32(UA_RING_DMA_REF | 2),
			.word1 = 0,
			.word2 = cpu_to_le32(lower_32_bits(dma_addr)),
			.word3 = cpu_to_le32(upper_32_bits(dma_addr)),
		};
		ua_dsp_ring_put_entry(&dsp0->cmd, &entry);
	}
	ua_dsp_ring_doorbell(ua, &dsp0->cmd, cmd_base);
	usleep_range(100000, 200000);  /* 100-200ms */
	pos = ua_read(ua, cmd_base + UA_RING_POSITION);
	dev_info(&ua->pdev->dev,
		 "TEST 8 (5 DMA refs, no inline): pos=%u/%u %s\n",
		 pos, 5,
		 pos >= 5 ? "ALL_OK" : "PARTIAL");

	/*
	 * Test 9: DMA ref + resp ring (like firmware load).
	 * Submit 1 DMA ref to cmd ring + 1 DMA ref to resp ring,
	 * ring both doorbells.
	 */
	ua_dsp_ring_program(ua, &dsp0->cmd, cmd_base);
	ua_dsp_ring_program(ua, &dsp0->resp,
			    bank_base + UA_RING_RESP_OFFSET);

	entry = (struct ua_ring_entry){
		.word0 = cpu_to_le32(UA_RING_DMA_REF | 2),
		.word1 = 0,
		.word2 = cpu_to_le32(lower_32_bits(dma_addr)),
		.word3 = cpu_to_le32(upper_32_bits(dma_addr)),
	};
	ua_dsp_ring_put_entry(&dsp0->cmd, &entry);

	/* Also submit a response ring DMA ref (using same buffer as scratch) */
	{
		struct ua_ring_entry resp_entry = {
			.word0 = cpu_to_le32(UA_RING_DMA_REF | 4),
			.word1 = 0,
			.word2 = cpu_to_le32(lower_32_bits(dma_addr)),
			.word3 = cpu_to_le32(upper_32_bits(dma_addr)),
		};
		ua_dsp_ring_put_entry(&dsp0->resp, &resp_entry);
	}

	ua_dsp_ring_doorbell(ua, &dsp0->cmd, cmd_base);
	ua_dsp_ring_doorbell(ua, &dsp0->resp,
			     bank_base + UA_RING_RESP_OFFSET);
	usleep_range(50000, 100000);
	{
		u32 cmd_pos = ua_read(ua, cmd_base + UA_RING_POSITION);
		u32 resp_pos = ua_read(ua, bank_base +
				       UA_RING_RESP_OFFSET + UA_RING_POSITION);
		dev_info(&ua->pdev->dev,
			 "TEST 9 (cmd+resp DMA refs): cmd_pos=%u resp_pos=%u\n",
			 cmd_pos, resp_pos);
	}

	/*
	 * Test 10: Inline recovery after all tests.
	 */
	ua_dsp_ring_program(ua, &dsp0->cmd, cmd_base);
	entry = (struct ua_ring_entry){
		.word0 = cpu_to_le32(UA_DSP_CMD_CONNECT),
		.word1 = cpu_to_le32(1),
		.word2 = 0,
		.word3 = 0,
	};
	ua_dsp_ring_put_entry(&dsp0->cmd, &entry);
	ua_dsp_ring_doorbell(ua, &dsp0->cmd, cmd_base);
	usleep_range(5000, 10000);
	pos = ua_read(ua, cmd_base + UA_RING_POSITION);
	dev_info(&ua->pdev->dev,
		 "TEST 10 (inline recovery): pos=%u %s\n",
		 pos, pos >= 1 ? "OK" : "FAIL");

	dev_info(&ua->pdev->dev, "=== DMA ref test complete ===\n");

	dma_free_coherent(&ua->pdev->dev, UA_PCIE_PAGE_SIZE,
			  dma_buf, dma_addr);
	return 0;
}
#pragma GCC pop_options

/* ----------------------------------------------------------------
 * _sendBlock — send a data block to a DSP via ring buffer
 * ----------------------------------------------------------------
 *
 * Hardware driver analysis (sendBlock at offset 0x1f7f8):
 *
 * _sendBlock(this, cmd, param, DMABuffer, data_offset, data_size, response_code)
 *
 * Protocol:
 *   1. Resp ring: submit 4-dword DMA ref → response buffer, doorbell
 *   2. Cmd ring:  submit header DMA ref (1 dword: (data_dwords+1)|cmd)
 *   3. Cmd ring:  submit per-page DMA refs (4KB each, one per page)
 *   4. ONE doorbell to cmd ring
 *   5. Poll response buffer for completion (resp[0] upper 16 bits == param>>16)
 *
 * Key details:
 *   - Response ring entry is 4 dwords (16 bytes), NOT a full page
 *   - param is for response validation only, NOT sent to hardware
 *   - All cmd ring entries submitted as one batch, single doorbell
 *   - Data split at 4KB page boundaries (DMA page alignment)
 *   - Doorbell order: DOORBELL(0x24) first, then WRITE_PTR(0x20)
 */

/* Response buffer size: 4 dwords (16 bytes), matching hardware driver response descriptor.
 * The FPGA writes a 4-dword completion status to this buffer.
 */
#define SEND_BLOCK_RESP_DWORDS	4

int ua_dsp_send_block(struct ua_device *ua, unsigned int dsp_idx,
		      u32 cmd, u32 param, const void *data,
		      size_t data_size)
{
	struct ua_dsp_state *ds;
	u32 bank_base, cmd_base, resp_base;
	void *hdr_buf = NULL, *data_buf = NULL, *resp_buf = NULL;
	dma_addr_t hdr_dma, data_dma, resp_dma;
	struct ua_ring_entry entry;
	u32 data_dwords, num_pages;
	u32 *resp_words;
	size_t data_alloc, offset, chunk;
	int polls, ret = 0;

	if (dsp_idx >= ua->num_dsps)
		return -EINVAL;
	if (data_size & 3)
		return -EINVAL;
	if (data_size > 256 * 1024)
		return -E2BIG;

	ds = &ua->dsp[dsp_idx];
	if (!ds->rings_allocated)
		return -ENODEV;

	bank_base = ua_dsp_base(dsp_idx);
	cmd_base = bank_base + UA_RING_CMD_OFFSET;
	resp_base = bank_base + UA_RING_RESP_OFFSET;

	data_dwords = data_size / 4;
	num_pages = DIV_ROUND_UP(data_size, 1024);  /* 1KB chunks */

	/* Check ring capacity: 1 header + N data chunks */
	if (ds->cmd.write_ptr + 1 + num_pages > ds->cmd.capacity)
		return -ENOSPC;

	/* Allocate header DMA buffer (holds 1 command dword) */
	hdr_buf = dma_alloc_coherent(&ua->pdev->dev, UA_PCIE_PAGE_SIZE,
				     &hdr_dma, GFP_KERNEL);
	if (!hdr_buf)
		return -ENOMEM;

	/* Allocate data DMA buffer (page-aligned) */
	data_alloc = ALIGN(data_size, UA_PCIE_PAGE_SIZE);
	if (data_alloc < UA_PCIE_PAGE_SIZE)
		data_alloc = UA_PCIE_PAGE_SIZE;
	data_buf = dma_alloc_coherent(&ua->pdev->dev, data_alloc,
				      &data_dma, GFP_KERNEL);
	if (!data_buf) {
		ret = -ENOMEM;
		goto free_hdr;
	}
	memset(data_buf, 0, data_alloc);
	if (data_size > 0)
		memcpy(data_buf, data, data_size);

	/* Allocate response DMA buffer (16 bytes = 4 dwords) */
	resp_buf = dma_alloc_coherent(&ua->pdev->dev, UA_PCIE_PAGE_SIZE,
				      &resp_dma, GFP_KERNEL);
	if (!resp_buf) {
		ret = -ENOMEM;
		goto free_data;
	}
	memset(resp_buf, 0, UA_PCIE_PAGE_SIZE);
	resp_words = resp_buf;

	/*
	 * Step 1: Submit response ring entry.
	 *
	 * Kext descriptor2: 4-dword DMA ref on the response ring.
	 * The FPGA reads this entry to find where to write its response.
	 * Must be submitted BEFORE cmd ring entries.
	 */
	if (ds->resp.write_ptr < ds->resp.capacity) {
		entry = (struct ua_ring_entry){
			.word0 = cpu_to_le32(UA_RING_DMA_REF |
					     SEND_BLOCK_RESP_DWORDS),
			.word1 = 0,
			.word2 = cpu_to_le32(lower_32_bits(resp_dma)),
			.word3 = cpu_to_le32(upper_32_bits(resp_dma)),
		};
		ua_dsp_ring_put_entry(&ds->resp, &entry);
		ua_dsp_ring_doorbell(ua, &ds->resp, resp_base);
		usleep_range(500, 1000);
	}

	/*
	 * Step 2: Build command header.
	 *
	 * Small format (data < 262140 bytes):
	 *   header[0] = (data_dwords + 1) | cmd
	 *
	 * Large format (data >= 262140 bytes):
	 *   header[0] = cmd | BIT(30)
	 *   header[1] = data_dwords + 2
	 */
	memset(hdr_buf, 0, UA_PCIE_PAGE_SIZE);
	if (data_size < 262140) {
		((u32 *)hdr_buf)[0] = cpu_to_le32((data_dwords + 1) | cmd);

		/* Header DMA ref: 1 dword */
		entry = (struct ua_ring_entry){
			.word0 = cpu_to_le32(UA_RING_DMA_REF | 1),
			.word1 = 0,
			.word2 = cpu_to_le32(lower_32_bits(hdr_dma)),
			.word3 = cpu_to_le32(upper_32_bits(hdr_dma)),
		};
	} else {
		((u32 *)hdr_buf)[0] = cpu_to_le32(cmd | BIT(30));
		((u32 *)hdr_buf)[1] = cpu_to_le32(data_dwords + 2);

		/* Header DMA ref: 2 dwords */
		entry = (struct ua_ring_entry){
			.word0 = cpu_to_le32(UA_RING_DMA_REF | 2),
			.word1 = 0,
			.word2 = cpu_to_le32(lower_32_bits(hdr_dma)),
			.word3 = cpu_to_le32(upper_32_bits(hdr_dma)),
		};
	}
	ret = ua_dsp_ring_put_entry(&ds->cmd, &entry);
	if (ret)
		goto free_resp;

	/*
	 * Step 3: Submit data as DMA ref entries.
	 *
	 * Kext splits data at 4KB page boundaries.  However, our DMA ref
	 * size limit tests showed stalls at 512+ dwords (2KB+).  Use 1KB
	 * chunks (256 dwords) which are verified working.  Try 4KB later
	 * once basic flow works.
	 */
#define SEND_BLOCK_CHUNK_SIZE	1024
	for (offset = 0; offset < data_size; offset += chunk) {
		u32 chunk_dwords;

		chunk = min_t(size_t, data_size - offset, SEND_BLOCK_CHUNK_SIZE);
		chunk_dwords = chunk / 4;

		entry = (struct ua_ring_entry){
			.word0 = cpu_to_le32(UA_RING_DMA_REF | chunk_dwords),
			.word1 = 0,
			.word2 = cpu_to_le32(lower_32_bits(data_dma + offset)),
			.word3 = cpu_to_le32(upper_32_bits(data_dma + offset)),
		};
		ret = ua_dsp_ring_put_entry(&ds->cmd, &entry);
		if (ret)
			goto free_resp;
	}

	/*
	 * Step 4: ONE doorbell to cmd ring (all entries submitted as batch).
	 */
	ua_dsp_ring_doorbell(ua, &ds->cmd, cmd_base);

	/*
	 * Step 5: Poll response buffer for completion.
	 *
	 * Kext checks: (resp[0] >> 16) == (param >> 16)
	 * For FW blocks with param=1: accept any non-zero response.
	 * Timeout: ~150ms (1500 polls × 100us).
	 */
	for (polls = 0; polls < 1500; polls++) {
		usleep_range(100, 200);
		/* Check if FPGA wrote anything to response buffer */
		if (resp_words[0] != 0)
			break;
	}

	dev_info(&ua->pdev->dev,
		 "sendBlock DSP%u: cmd=0x%08x param=0x%08x size=%zu "
		 "resp=[0x%08x 0x%08x 0x%08x 0x%08x] pos=%u/%u\n",
		 dsp_idx, cmd, param, data_size,
		 le32_to_cpu(resp_words[0]), le32_to_cpu(resp_words[1]),
		 le32_to_cpu(resp_words[2]), le32_to_cpu(resp_words[3]),
		 ua_read(ua, cmd_base + UA_RING_POSITION),
		 ds->cmd.write_ptr);

	if (resp_words[0] == 0) {
		dev_warn(&ua->pdev->dev,
			 "sendBlock: no response after %d polls\n", polls);
		ret = -ETIMEDOUT;
	}

free_resp:
	dma_free_coherent(&ua->pdev->dev, UA_PCIE_PAGE_SIZE,
			  resp_buf, resp_dma);
free_data:
	dma_free_coherent(&ua->pdev->dev, data_alloc,
			  data_buf, data_dma);
free_hdr:
	dma_free_coherent(&ua->pdev->dev, UA_PCIE_PAGE_SIZE,
			  hdr_buf, hdr_dma);
	return ret;
}

/**
 * ua_dsp_send_sram_data - Send SRAM config + data as atomic pair
 * @ua: device (caller must hold ua->lock)
 * @dsp_idx: target DSP (0-3)
 * @sram_addr: SHARC DSP SRAM destination address
 * @data: data to write
 * @data_size: size in bytes (must be 4-byte aligned, max 4096)
 *
 * Sends an inline SRAM_CFG command (0x00080004) followed by a DMA ref
 * containing the data, as a single atomic pair (one doorbell for both).
 * This matches Windows driver behavior for SRAM routing table writes.
 */
int ua_dsp_send_sram_data(struct ua_device *ua, unsigned int dsp_idx,
			  u32 sram_addr, const void *data, size_t data_size)
{
	struct ua_dsp_state *ds;
	u32 bank_base, cmd_base;
	void *dma_buf;
	dma_addr_t dma_addr;
	struct ua_ring_entry entry;
	u32 dwords, target, pos;
	int polls;

	if (dsp_idx >= ua->num_dsps || !data || (data_size & 3) ||
	    data_size > UA_PCIE_PAGE_SIZE)
		return -EINVAL;

	ds = &ua->dsp[dsp_idx];
	if (!ds->rings_allocated)
		return -ENODEV;

	bank_base = ua_dsp_base(dsp_idx);
	cmd_base = bank_base + UA_RING_CMD_OFFSET;
	dwords = data_size / 4;

	/* Need 2 ring entries: inline + DMA ref */
	if (ds->cmd.write_ptr + 2 > ds->cmd.capacity)
		return -ENOSPC;

	dma_buf = dma_alloc_coherent(&ua->pdev->dev, UA_PCIE_PAGE_SIZE,
				     &dma_addr, GFP_KERNEL);
	if (!dma_buf)
		return -ENOMEM;

	memset(dma_buf, 0, UA_PCIE_PAGE_SIZE);
	memcpy(dma_buf, data, data_size);

	/* Entry 1: inline SRAM_CFG {0x00080004, sram_addr, 0, dwords} */
	entry = (struct ua_ring_entry){
		.word0 = cpu_to_le32(0x00080004),
		.word1 = cpu_to_le32(sram_addr),
		.word2 = 0,
		.word3 = cpu_to_le32(dwords),
	};
	ua_dsp_ring_put_entry(&ds->cmd, &entry);

	/* Entry 2: DMA ref pointing to data — NO doorbell yet */
	entry = (struct ua_ring_entry){
		.word0 = cpu_to_le32(UA_RING_DMA_REF | dwords),
		.word1 = 0,
		.word2 = cpu_to_le32(lower_32_bits(dma_addr)),
		.word3 = cpu_to_le32(upper_32_bits(dma_addr)),
	};
	ua_dsp_ring_put_entry(&ds->cmd, &entry);

	/* Single doorbell for both entries */
	ua_dsp_ring_doorbell(ua, &ds->cmd, cmd_base);

	/* Wait for DSP to consume both entries */
	target = ds->cmd.write_ptr;
	for (polls = 0; polls < 200; polls++) {
		pos = ua_read(ua, cmd_base + UA_RING_POSITION);
		if (pos >= target)
			break;
		usleep_range(500, 1000);
	}
	if (polls >= 200)
		dev_warn(&ua->pdev->dev,
			 "sram_data: DSP %u timeout (pos=%u target=%u)\n",
			 dsp_idx, pos, target);

	dma_free_coherent(&ua->pdev->dev, UA_PCIE_PAGE_SIZE,
			  dma_buf, dma_addr);
	return 0;
}

/**
 * ua_dsp_send_raw_dma - Send raw data to DSP via DMA ref (no cmd/param header)
 * @ua: device (caller must hold ua->lock)
 * @dsp_idx: target DSP (0-3)
 * @data: raw data to send
 * @data_size: size in bytes (must be 4-byte aligned)
 *
 * Sends data as a plain DMA reference entry on the command ring.
 * No response is expected — this is for SRAM config data that follows
 * a 0x00080004 inline command.
 */
int ua_dsp_send_raw_dma(struct ua_device *ua, unsigned int dsp_idx,
			const void *data, size_t data_size)
{
	struct ua_dsp_state *ds;
	u32 bank_base, cmd_base;
	void *dma_buf;
	dma_addr_t dma_addr;
	struct ua_ring_entry entry;
	u32 dwords;

	if (dsp_idx >= ua->num_dsps || !data || (data_size & 3))
		return -EINVAL;

	ds = &ua->dsp[dsp_idx];
	if (!ds->rings_allocated)
		return -ENODEV;

	bank_base = ua_dsp_base(dsp_idx);
	cmd_base = bank_base + UA_RING_CMD_OFFSET;
	dwords = data_size / 4;

	dma_buf = dma_alloc_coherent(&ua->pdev->dev, UA_PCIE_PAGE_SIZE,
				     &dma_addr, GFP_KERNEL);
	if (!dma_buf)
		return -ENOMEM;

	memset(dma_buf, 0, UA_PCIE_PAGE_SIZE);
	memcpy(dma_buf, data, data_size);

	/* Send as raw DMA reference — no header, no response */
	entry = (struct ua_ring_entry){
		.word0 = cpu_to_le32(UA_RING_DMA_REF | dwords),
		.word1 = 0,
		.word2 = cpu_to_le32(lower_32_bits(dma_addr)),
		.word3 = cpu_to_le32(upper_32_bits(dma_addr)),
	};

	ua_dsp_ring_put_entry(&ds->cmd, &entry);
	ua_dsp_ring_doorbell(ua, &ds->cmd, cmd_base);

	/* Wait for DSP to consume the entry before freeing DMA buffer.
	 * Poll POSITION register — must advance past our entry. */
	{
		u32 target = ds->cmd.write_ptr;
		u32 pos;
		int polls;

		for (polls = 0; polls < 200; polls++) {
			pos = ua_read(ua, cmd_base + UA_RING_POSITION);
			if (pos >= target)
				break;
			usleep_range(500, 1000);
		}
		if (polls >= 200) {
			dev_warn(&ua->pdev->dev,
				 "raw_dma: DSP %u timeout (pos=%u target=%u)\n",
				 dsp_idx, pos, target);
		}
	}

	dma_free_coherent(&ua->pdev->dev, UA_PCIE_PAGE_SIZE,
			  dma_buf, dma_addr);
	return 0;
}

/**
 * ua_dsp_test_send_block - Test sendBlock with connect + response ring
 * @ua: device (must hold ua->lock)
 *
 * Sends the connect command via both DMA ref and inline-with-addr formats,
 * checking the response ring for DSP replies.
 */
int __attribute__((optimize("O1"))) ua_dsp_test_send_block(struct ua_device *ua)
{
	struct ua_dsp_state *dsp0 = &ua->dsp[0];
	u32 bank_base = ua_dsp_base(0);
	u32 cmd_base = bank_base + UA_RING_CMD_OFFSET;
	u32 resp_base = bank_base + UA_RING_RESP_OFFSET;
	void *cmd_buf, *resp_buf;
	dma_addr_t cmd_addr, resp_addr;
	u32 *cmd_words, *resp_words;
	struct ua_ring_entry entry;
	u32 cmd_pos, resp_pos;
	int i;

	if (!dsp0->rings_allocated)
		return -ENODEV;

	cmd_buf = dma_alloc_coherent(&ua->pdev->dev, UA_PCIE_PAGE_SIZE,
				     &cmd_addr, GFP_KERNEL);
	resp_buf = dma_alloc_coherent(&ua->pdev->dev, UA_PCIE_PAGE_SIZE,
				      &resp_addr, GFP_KERNEL);
	if (!cmd_buf || !resp_buf) {
		if (cmd_buf)
			dma_free_coherent(&ua->pdev->dev, UA_PCIE_PAGE_SIZE,
					  cmd_buf, cmd_addr);
		if (resp_buf)
			dma_free_coherent(&ua->pdev->dev, UA_PCIE_PAGE_SIZE,
					  resp_buf, resp_addr);
		return -ENOMEM;
	}

	cmd_words = cmd_buf;
	resp_words = resp_buf;

	dev_info(&ua->pdev->dev, "=== sendBlock test ===\n");
	dev_info(&ua->pdev->dev,
		 "cmd_addr=%pad resp_addr=%pad\n", &cmd_addr, &resp_addr);

	/*
	 * Test A: DMA ref format.
	 * DMA buffer = [0x00230002, 0x00000001] (connect cmd + param)
	 * Cmd ring entry = {2 | BIT31, cmd_addr_lo, cmd_addr_hi, 0}
	 * Resp ring entry = {4 | BIT31, resp_addr_lo, resp_addr_hi, 0}
	 */
	memset(cmd_buf, 0, UA_PCIE_PAGE_SIZE);
	memset(resp_buf, 0, UA_PCIE_PAGE_SIZE);
	cmd_words[0] = cpu_to_le32(UA_DSP_CMD_CONNECT);
	cmd_words[1] = cpu_to_le32(1);

	ua_dsp_ring_program(ua, &dsp0->cmd, cmd_base);
	ua_dsp_ring_program(ua, &dsp0->resp, resp_base);

	entry = (struct ua_ring_entry){
		.word0 = cpu_to_le32(UA_RING_DMA_REF | 2),
		.word1 = 0,
		.word2 = cpu_to_le32(lower_32_bits(cmd_addr)),
		.word3 = cpu_to_le32(upper_32_bits(cmd_addr)),
	};
	ua_dsp_ring_put_entry(&dsp0->cmd, &entry);

	entry = (struct ua_ring_entry){
		.word0 = cpu_to_le32(UA_RING_DMA_REF | 4),
		.word1 = 0,
		.word2 = cpu_to_le32(lower_32_bits(resp_addr)),
		.word3 = cpu_to_le32(upper_32_bits(resp_addr)),
	};
	ua_dsp_ring_put_entry(&dsp0->resp, &entry);

	ua_dsp_ring_doorbell(ua, &dsp0->cmd, cmd_base);
	ua_dsp_ring_doorbell(ua, &dsp0->resp, resp_base);
	msleep(100);

	cmd_pos = ua_read(ua, cmd_base + UA_RING_POSITION);
	resp_pos = ua_read(ua, resp_base + UA_RING_POSITION);
	dev_info(&ua->pdev->dev,
		 "Test A (DMA ref): cmd_pos=%u resp_pos=%u "
		 "resp=[%08x %08x %08x %08x]\n",
		 cmd_pos, resp_pos,
		 le32_to_cpu(resp_words[0]), le32_to_cpu(resp_words[1]),
		 le32_to_cpu(resp_words[2]), le32_to_cpu(resp_words[3]));

	/*
	 * Test B: Inline entry with DMA address.
	 * DMA buffer = [connect data if any, but connect has no payload]
	 * Cmd ring entry = {cmd, param, dma_addr_lo, dma_addr_hi}
	 * Resp ring entry = DMA ref for response capture
	 */
	memset(cmd_buf, 0, UA_PCIE_PAGE_SIZE);
	memset(resp_buf, 0, UA_PCIE_PAGE_SIZE);

	ua_dsp_ring_program(ua, &dsp0->cmd, cmd_base);
	ua_dsp_ring_program(ua, &dsp0->resp, resp_base);

	entry = (struct ua_ring_entry){
		.word0 = cpu_to_le32(UA_DSP_CMD_CONNECT),
		.word1 = cpu_to_le32(1),
		.word2 = cpu_to_le32(lower_32_bits(cmd_addr)),
		.word3 = cpu_to_le32(upper_32_bits(cmd_addr)),
	};
	ua_dsp_ring_put_entry(&dsp0->cmd, &entry);

	entry = (struct ua_ring_entry){
		.word0 = cpu_to_le32(UA_RING_DMA_REF | 4),
		.word1 = 0,
		.word2 = cpu_to_le32(lower_32_bits(resp_addr)),
		.word3 = cpu_to_le32(upper_32_bits(resp_addr)),
	};
	ua_dsp_ring_put_entry(&dsp0->resp, &entry);

	ua_dsp_ring_doorbell(ua, &dsp0->cmd, cmd_base);
	ua_dsp_ring_doorbell(ua, &dsp0->resp, resp_base);
	msleep(100);

	cmd_pos = ua_read(ua, cmd_base + UA_RING_POSITION);
	resp_pos = ua_read(ua, resp_base + UA_RING_POSITION);
	dev_info(&ua->pdev->dev,
		 "Test B (inline+addr): cmd_pos=%u resp_pos=%u "
		 "resp=[%08x %08x %08x %08x]\n",
		 cmd_pos, resp_pos,
		 le32_to_cpu(resp_words[0]), le32_to_cpu(resp_words[1]),
		 le32_to_cpu(resp_words[2]), le32_to_cpu(resp_words[3]));

	/*
	 * Test C: Pure inline (no DMA), with response ring DMA ref.
	 * This is the working connect command — does the resp ring capture
	 * anything?
	 */
	memset(resp_buf, 0, UA_PCIE_PAGE_SIZE);

	ua_dsp_ring_program(ua, &dsp0->cmd, cmd_base);
	ua_dsp_ring_program(ua, &dsp0->resp, resp_base);

	entry = (struct ua_ring_entry){
		.word0 = cpu_to_le32(UA_DSP_CMD_CONNECT),
		.word1 = cpu_to_le32(1),
		.word2 = 0,
		.word3 = 0,
	};
	ua_dsp_ring_put_entry(&dsp0->cmd, &entry);

	entry = (struct ua_ring_entry){
		.word0 = cpu_to_le32(UA_RING_DMA_REF | 4),
		.word1 = 0,
		.word2 = cpu_to_le32(lower_32_bits(resp_addr)),
		.word3 = cpu_to_le32(upper_32_bits(resp_addr)),
	};
	ua_dsp_ring_put_entry(&dsp0->resp, &entry);

	ua_dsp_ring_doorbell(ua, &dsp0->cmd, cmd_base);
	ua_dsp_ring_doorbell(ua, &dsp0->resp, resp_base);
	msleep(100);

	cmd_pos = ua_read(ua, cmd_base + UA_RING_POSITION);
	resp_pos = ua_read(ua, resp_base + UA_RING_POSITION);
	dev_info(&ua->pdev->dev,
		 "Test C (inline+resp): cmd_pos=%u resp_pos=%u "
		 "resp=[%08x %08x %08x %08x]\n",
		 cmd_pos, resp_pos,
		 le32_to_cpu(resp_words[0]), le32_to_cpu(resp_words[1]),
		 le32_to_cpu(resp_words[2]), le32_to_cpu(resp_words[3]));

	/* Also check if the resp ring page itself has data */
	{
		u32 *ring_page = dsp0->resp.pages[0];

		dev_info(&ua->pdev->dev,
			 "Resp ring page[0]: [%08x %08x %08x %08x]\n",
			 le32_to_cpu(ring_page[0]), le32_to_cpu(ring_page[1]),
			 le32_to_cpu(ring_page[2]), le32_to_cpu(ring_page[3]));
	}

	/*
	 * Test D: Read response ring registers directly.
	 * Maybe the DSP writes responses to the ring page directly
	 * (like inline entries in reverse direction).
	 */
	for (i = 0; i < 4; i++) {
		u32 val = ua_read(ua, resp_base + i * 4);

		dev_info(&ua->pdev->dev,
			 "Resp ring reg[0x%02x] = 0x%08x\n",
			 i * 4, val);
	}

	/*
	 * Test E: Firmware load command via single DMA ref.
	 * The hardware driver puts [packed_cmd, param, data...] in ONE buffer.
	 * DMA ref entry points to the whole thing.
	 * Even a tiny "firmware" (1 dword) should elicit a response.
	 *
	 * Buffer: [(1+1)|0x120000, 0x80040000, 0x00000000]
	 *       = [0x00120002, 0x80040000, 0x00000000]  (3 dwords)
	 * Cmd entry: {3|BIT31, addr_lo, addr_hi, 0}
	 * Resp entry: {4|BIT31, resp_addr_lo, resp_addr_hi, 0}
	 */
	memset(cmd_buf, 0, UA_PCIE_PAGE_SIZE);
	memset(resp_buf, 0xCC, UA_PCIE_PAGE_SIZE);  /* Canary pattern */
	cmd_words[0] = cpu_to_le32((1 + 1) | UA_FW_CMD);   /* 0x00120002 */
	cmd_words[1] = cpu_to_le32(UA_FW_PARAM);            /* 0x80040000 */
	cmd_words[2] = cpu_to_le32(0);                       /* 1 dword of "firmware" */

	ua_dsp_ring_program(ua, &dsp0->resp, resp_base);
	ua_dsp_ring_program(ua, &dsp0->cmd, cmd_base);

	/* Resp ring first (provide buffer before sending command) */
	entry = (struct ua_ring_entry){
		.word0 = cpu_to_le32(UA_RING_DMA_REF | 4),
		.word1 = 0,
		.word2 = cpu_to_le32(lower_32_bits(resp_addr)),
		.word3 = cpu_to_le32(upper_32_bits(resp_addr)),
	};
	ua_dsp_ring_put_entry(&dsp0->resp, &entry);
	ua_dsp_ring_doorbell(ua, &dsp0->resp, resp_base);

	/* Small delay, then cmd */
	usleep_range(1000, 2000);

	entry = (struct ua_ring_entry){
		.word0 = cpu_to_le32(UA_RING_DMA_REF | 3),
		.word1 = 0,
		.word2 = cpu_to_le32(lower_32_bits(cmd_addr)),
		.word3 = cpu_to_le32(upper_32_bits(cmd_addr)),
	};
	ua_dsp_ring_put_entry(&dsp0->cmd, &entry);
	ua_dsp_ring_doorbell(ua, &dsp0->cmd, cmd_base);

	msleep(500);  /* Give DSP time to process firmware command */

	cmd_pos = ua_read(ua, cmd_base + UA_RING_POSITION);
	resp_pos = ua_read(ua, resp_base + UA_RING_POSITION);
	dev_info(&ua->pdev->dev,
		 "Test E (fw load DMA ref): cmd_pos=%u resp_pos=%u "
		 "resp=[%08x %08x %08x %08x]\n",
		 cmd_pos, resp_pos,
		 le32_to_cpu(resp_words[0]), le32_to_cpu(resp_words[1]),
		 le32_to_cpu(resp_words[2]), le32_to_cpu(resp_words[3]));

	/*
	 * Test F: Same firmware command but INLINE (no DMA ref).
	 * entry = {packed_cmd, param, 0, 0}
	 * If this produces a response but Test E didn't, the issue is DMA.
	 */
	memset(resp_buf, 0xDD, UA_PCIE_PAGE_SIZE);

	ua_dsp_ring_program(ua, &dsp0->resp, resp_base);
	ua_dsp_ring_program(ua, &dsp0->cmd, cmd_base);

	entry = (struct ua_ring_entry){
		.word0 = cpu_to_le32(UA_RING_DMA_REF | 4),
		.word1 = 0,
		.word2 = cpu_to_le32(lower_32_bits(resp_addr)),
		.word3 = cpu_to_le32(upper_32_bits(resp_addr)),
	};
	ua_dsp_ring_put_entry(&dsp0->resp, &entry);
	ua_dsp_ring_doorbell(ua, &dsp0->resp, resp_base);
	usleep_range(1000, 2000);

	entry = (struct ua_ring_entry){
		.word0 = cpu_to_le32((1 + 1) | UA_FW_CMD),  /* 0x00120002 */
		.word1 = cpu_to_le32(UA_FW_PARAM),           /* 0x80040000 */
		.word2 = 0,
		.word3 = 0,
	};
	ua_dsp_ring_put_entry(&dsp0->cmd, &entry);
	ua_dsp_ring_doorbell(ua, &dsp0->cmd, cmd_base);

	msleep(500);

	cmd_pos = ua_read(ua, cmd_base + UA_RING_POSITION);
	resp_pos = ua_read(ua, resp_base + UA_RING_POSITION);
	dev_info(&ua->pdev->dev,
		 "Test F (fw load inline): cmd_pos=%u resp_pos=%u "
		 "resp=[%08x %08x %08x %08x]\n",
		 cmd_pos, resp_pos,
		 le32_to_cpu(resp_words[0]), le32_to_cpu(resp_words[1]),
		 le32_to_cpu(resp_words[2]), le32_to_cpu(resp_words[3]));

	dev_info(&ua->pdev->dev, "=== sendBlock test complete ===\n");

	dma_free_coherent(&ua->pdev->dev, UA_PCIE_PAGE_SIZE,
			  cmd_buf, cmd_addr);
	dma_free_coherent(&ua->pdev->dev, UA_PCIE_PAGE_SIZE,
			  resp_buf, resp_addr);
	return 0;
}

/* ----------------------------------------------------------------
 * Ring buffer engine diagnostic
 * ---------------------------------------------------------------- */

/**
 * ua_dsp_ring_diagnostic - Test if FPGA ring buffer engine processes entries
 * @ua: device
 *
 * Runs after ACEFACE + DMA pulse, before firmware loading.
 * Submits inline entries (no DMA ref) to determine if the ring buffer
 * engine is active.  This helps distinguish "engine not started" from
 * "DMA ref format issue".
 */
void ua_dsp_ring_diagnostic(struct ua_device *ua)
{
	struct ua_dsp_state *dsp0 = &ua->dsp[0];
	u32 cb = ua_dsp_base(0) + UA_RING_CMD_OFFSET;
	u32 rb = ua_dsp_base(0) + UA_RING_RESP_OFFSET;
	struct ua_ring_entry ent;
	u32 pos_before, pos_after, pos_resp;
	u32 reg2c, reg30, reg34, reg38, reg3c;

	if (!dsp0->rings_allocated)
		return;

	/* Read unknown ring registers beyond POSITION for clues */
	reg2c = ua_read(ua, cb + 0x2C);
	reg30 = ua_read(ua, cb + 0x30);
	reg34 = ua_read(ua, cb + 0x34);
	reg38 = ua_read(ua, cb + 0x38);
	reg3c = ua_read(ua, cb + 0x3C);
	dev_info(&ua->pdev->dev,
		 "ring diag: cmd extra regs "
		 "+2C=%08x +30=%08x +34=%08x +38=%08x +3C=%08x\n",
		 reg2c, reg30, reg34, reg38, reg3c);

	pos_before = ua_read(ua, cb + UA_RING_POSITION);

	/* Test 1: Single inline connect entry (no DMA ref) */
	ua_dsp_ring_program(ua, &dsp0->cmd, cb);
	ent = (struct ua_ring_entry){
		.word0 = cpu_to_le32(UA_DSP_CMD_CONNECT),
		.word1 = cpu_to_le32(1),
		.word2 = 0,
		.word3 = 0,
	};
	ua_dsp_ring_put_entry(&dsp0->cmd, &ent);
	ua_dsp_ring_doorbell(ua, &dsp0->cmd, cb);
	msleep(200);

	pos_after = ua_read(ua, cb + UA_RING_POSITION);
	dev_info(&ua->pdev->dev,
		 "ring diag test1 (inline connect): "
		 "pos %u → %u (WPTR=1 DBELL=1)\n",
		 pos_before, pos_after);

	/* Check if POSITION register is writable (try reset to 0) */
	ua_write(ua, cb + UA_RING_POSITION, 0);
	dev_info(&ua->pdev->dev,
		 "ring diag: wrote 0 to POSITION, readback=%u\n",
		 ua_read(ua, cb + UA_RING_POSITION));

	/* Test 2: Both cmd+resp doorbelled simultaneously */
	ua_dsp_ring_program(ua, &dsp0->cmd, cb);
	ua_dsp_ring_program(ua, &dsp0->resp, rb);

	ent = (struct ua_ring_entry){
		.word0 = cpu_to_le32(UA_DSP_CMD_CONNECT),
		.word1 = cpu_to_le32(1),
		.word2 = 0,
		.word3 = 0,
	};
	ua_dsp_ring_put_entry(&dsp0->cmd, &ent);

	ent = (struct ua_ring_entry){
		.word0 = cpu_to_le32(UA_RING_DMA_REF | 4),
		.word1 = 0,
		.word2 = cpu_to_le32(lower_32_bits(dsp0->resp.page_addrs[0])),
		.word3 = cpu_to_le32(upper_32_bits(dsp0->resp.page_addrs[0])),
	};
	ua_dsp_ring_put_entry(&dsp0->resp, &ent);

	ua_dsp_ring_doorbell(ua, &dsp0->cmd, cb);
	ua_dsp_ring_doorbell(ua, &dsp0->resp, rb);
	msleep(200);

	pos_after = ua_read(ua, cb + UA_RING_POSITION);
	pos_resp = ua_read(ua, rb + UA_RING_POSITION);
	dev_info(&ua->pdev->dev,
		 "ring diag test2 (both doorbell): "
		 "cmd_pos=%u resp_pos=%u\n",
		 pos_after, pos_resp);

	/* Verify ring page content is what we wrote */
	if (dsp0->cmd.pages[0]) {
		u32 *p = dsp0->cmd.pages[0];

		dev_info(&ua->pdev->dev,
			 "ring diag: cmd page[0] slot0="
			 "%08x %08x %08x %08x\n",
			 le32_to_cpu(p[0]), le32_to_cpu(p[1]),
			 le32_to_cpu(p[2]), le32_to_cpu(p[3]));
	}

	/* Test 3: Try writing DOORBELL value=0 then value=1 (some FPGAs
	 * use edge-triggered doorbells, not level-triggered)
	 */
	ua_dsp_ring_program(ua, &dsp0->cmd, cb);
	ent = (struct ua_ring_entry){
		.word0 = cpu_to_le32(UA_DSP_CMD_CONNECT),
		.word1 = cpu_to_le32(1),
		.word2 = 0,
		.word3 = 0,
	};
	ua_dsp_ring_put_entry(&dsp0->cmd, &ent);

	/* Write WRITE_PTR first, then do doorbell with explicit 0→1 */
	ua_write(ua, cb + UA_RING_WRITE_PTR, 1);
	ua_write(ua, cb + UA_RING_DOORBELL, 0);  /* reset doorbell */
	wmb();
	ua_write(ua, cb + UA_RING_DOORBELL, 1);  /* trigger */
	msleep(200);

	pos_after = ua_read(ua, cb + UA_RING_POSITION);
	dev_info(&ua->pdev->dev,
		 "ring diag test3 (edge doorbell): cmd_pos=%u\n",
		 pos_after);

	/* Clean up: reset rings for FW loading */
	ua_dsp_ring_program(ua, &dsp0->cmd, cb);
	ua_dsp_ring_program(ua, &dsp0->resp, rb);
}

/**
 * ua_dsp_test_dma_locality - Determine if FPGA can DMA-read host memory
 * @ua: device
 *
 * THE critical diagnostic: uses CUMULATIVE ring entries (no ring_program
 * between tests) to avoid stale POSITION.  Each test adds one entry,
 * doorbells, and checks if POSITION advances to the expected value.
 *
 * Tests DMA ref with:
 *   A: Small payload (8 bytes) → ring page 1 (registered address)
 *   B: Small payload (8 bytes) → external DMA buffer
 *   C: Large payload (~12KB) → external DMA buffer (FW-sized)
 *   D: Inline baseline (sanity check)
 */
void ua_dsp_test_dma_locality(struct ua_device *ua)
{
	struct ua_dsp_state *dsp0 = &ua->dsp[0];
	u32 cb = ua_dsp_base(0) + UA_RING_CMD_OFFSET;
	struct ua_ring_entry ent;
	u32 *page1_words;
	void *ext_buf = NULL;
	dma_addr_t ext_dma = 0;
	void *big_buf = NULL;
	dma_addr_t big_dma = 0;
	u32 pos_before, pos_after;
	size_t big_size = ALIGN(12040 + 8, UA_PCIE_PAGE_SIZE);

	if (!dsp0->rings_allocated || dsp0->cmd.num_pages < 2)
		return;

	/* Allocate external buffers upfront */
	ext_buf = dma_alloc_coherent(&ua->pdev->dev, UA_PCIE_PAGE_SIZE,
				     &ext_dma, GFP_KERNEL);
	big_buf = dma_alloc_coherent(&ua->pdev->dev, big_size,
				     &big_dma, GFP_KERNEL);

	dev_info(&ua->pdev->dev, "=== DMA locality test (cumulative) ===\n");
	dev_info(&ua->pdev->dev,
		 "  cmd page0=%pad  page1=%pad  ext=%pad  big=%pad\n",
		 &dsp0->cmd.page_addrs[0], &dsp0->cmd.page_addrs[1],
		 &ext_dma, &big_dma);

	/* Program ring ONCE — all tests use cumulative write_ptr */
	ua_dsp_ring_program(ua, &dsp0->cmd, cb);

	pos_before = ua_read(ua, cb + UA_RING_POSITION);
	dev_info(&ua->pdev->dev,
		 "  initial POSITION=%u WRITE_PTR=%u\n",
		 pos_before, dsp0->cmd.write_ptr);

	/*
	 * Test A: DMA ref → ring page 1 (small, 2 dwords, registered addr)
	 */
	page1_words = dsp0->cmd.pages[1];
	page1_words[0] = cpu_to_le32(UA_DSP_CMD_CONNECT);
	page1_words[1] = cpu_to_le32(1);

	ent = (struct ua_ring_entry){
		.word0 = cpu_to_le32(UA_RING_DMA_REF | 2),
		.word1 = 0,
		.word2 = cpu_to_le32(lower_32_bits(dsp0->cmd.page_addrs[1])),
		.word3 = cpu_to_le32(upper_32_bits(dsp0->cmd.page_addrs[1])),
	};
	ua_dsp_ring_put_entry(&dsp0->cmd, &ent);
	ua_dsp_ring_doorbell(ua, &dsp0->cmd, cb);
	msleep(200);
	pos_after = ua_read(ua, cb + UA_RING_POSITION);
	dev_info(&ua->pdev->dev,
		 "  Test A (DMA ref→page1, 8B): pos %u→%u %s\n",
		 pos_before, pos_after,
		 pos_after > pos_before ? "WORKS" : "FAIL");
	pos_before = pos_after;

	/*
	 * Test B: DMA ref → external buffer (small, 2 dwords)
	 */
	if (ext_buf) {
		memset(ext_buf, 0, UA_PCIE_PAGE_SIZE);
		((u32 *)ext_buf)[0] = cpu_to_le32(UA_DSP_CMD_CONNECT);
		((u32 *)ext_buf)[1] = cpu_to_le32(1);

		ent = (struct ua_ring_entry){
			.word0 = cpu_to_le32(UA_RING_DMA_REF | 2),
			.word1 = 0,
			.word2 = cpu_to_le32(lower_32_bits(ext_dma)),
			.word3 = cpu_to_le32(upper_32_bits(ext_dma)),
		};
		ua_dsp_ring_put_entry(&dsp0->cmd, &ent);
		ua_dsp_ring_doorbell(ua, &dsp0->cmd, cb);
		msleep(200);
		pos_after = ua_read(ua, cb + UA_RING_POSITION);
		dev_info(&ua->pdev->dev,
			 "  Test B (DMA ref→ext, 8B): pos %u→%u %s\n",
			 pos_before, pos_after,
			 pos_after > pos_before ? "WORKS" : "FAIL");
		pos_before = pos_after;
	}

	/*
	 * Tests C-H: DMA ref size sweep.
	 * Find the maximum DMA ref size the FPGA will process.
	 * Uses the same external buffer, varying the size field.
	 * Sizes: 4, 16, 64, 256, 1024 (=4KB), 3012 (~12KB) dwords.
	 * All contain a connect command at the start.
	 */
	if (big_buf) {
		static const u32 test_sizes[] = {
			4, 256, 384, 512, 640, 768, 1024
		};
		static const char * const test_labels[] = {
			"16B", "1KB", "1.5KB", "2KB", "2.5KB", "3KB", "4KB"
		};
		u32 *bw = big_buf;
		int t;

		memset(big_buf, 0, big_size);
		bw[0] = cpu_to_le32(UA_DSP_CMD_CONNECT);
		bw[1] = cpu_to_le32(1);

		for (t = 0; t < 7; t++) {
			ent = (struct ua_ring_entry){
				.word0 = cpu_to_le32(UA_RING_DMA_REF |
						     test_sizes[t]),
				.word1 = 0,
				.word2 = cpu_to_le32(lower_32_bits(big_dma)),
				.word3 = cpu_to_le32(upper_32_bits(big_dma)),
			};
			ua_dsp_ring_put_entry(&dsp0->cmd, &ent);
			ua_dsp_ring_doorbell(ua, &dsp0->cmd, cb);
			msleep(300);
			pos_after = ua_read(ua, cb + UA_RING_POSITION);
			dev_info(&ua->pdev->dev,
				 "  Test %c (DMA ref %s, %u dw): "
				 "pos %u→%u %s\n",
				 'C' + t, test_labels[t], test_sizes[t],
				 pos_before, pos_after,
				 pos_after > pos_before ? "WORKS" : "FAIL");

			if (pos_after <= pos_before) {
				/* Ring stalled — try to recover with
				 * ring_program for remaining tests */
				dev_info(&ua->pdev->dev,
					 "  STALL at %u dwords (%s) — "
					 "max working size was %s\n",
					 test_sizes[t], test_labels[t],
					 t > 0 ? test_labels[t - 1] : "< 4dw");
				break;
			}
			pos_before = pos_after;
		}
	}

	/*
	 * Test I: Inline after sweep (check if ring recovered or stalled)
	 */
	ent = (struct ua_ring_entry){
		.word0 = cpu_to_le32(UA_DSP_CMD_CONNECT),
		.word1 = cpu_to_le32(1),
		.word2 = 0,
		.word3 = 0,
	};
	ua_dsp_ring_put_entry(&dsp0->cmd, &ent);
	ua_dsp_ring_doorbell(ua, &dsp0->cmd, cb);
	msleep(200);
	pos_after = ua_read(ua, cb + UA_RING_POSITION);
	dev_info(&ua->pdev->dev,
		 "  Test I (inline post-sweep): pos %u→%u %s\n",
		 pos_before, pos_after,
		 pos_after > pos_before ? "WORKS" : "STALLED");

	dev_info(&ua->pdev->dev,
		 "  final: POSITION=%u WRITE_PTR=%u\n",
		 pos_after, dsp0->cmd.write_ptr);
	dev_info(&ua->pdev->dev, "=== DMA locality test done ===\n");

	/* Free buffers */
	if (ext_buf)
		dma_free_coherent(&ua->pdev->dev, UA_PCIE_PAGE_SIZE,
				  ext_buf, ext_dma);
	if (big_buf)
		dma_free_coherent(&ua->pdev->dev, big_size,
				  big_buf, big_dma);

	/*
	 * CRITICAL: Sync write_ptr for FW loading.
	 * Don't ring_program (would desync POSITION vs WRITE_PTR).
	 * FW loading will continue accumulating from current write_ptr.
	 */
}

/* ----------------------------------------------------------------
 * Mixer firmware blob loading (request_firmware API)
 * ---------------------------------------------------------------- */

/**
 * ua_dsp_load_mixer_blocks - Load mixer firmware from filesystem blob
 * @ua: device (must hold ua->lock)
 *
 * Uses the Linux request_firmware() API to load ua-apollo-mixer.bin
 * from /lib/firmware/.  The blob contains a header, block table, and
 * concatenated firmware payloads (packed by tools/pack-mixer-firmware.py).
 *
 * Each block is sent to DSP 0 via ua_dsp_send_block() with:
 *   cmd   = SRAM address (from block table)
 *   param = 1 (constant, matches hardware driver capture)
 *   data  = block payload
 *
 * Returns 0 on success, negative errno on failure.
 * -ENOENT if firmware file not found (non-fatal, expected on first install).
 */
int ua_dsp_load_mixer_blocks(struct ua_device *ua)
{
	const struct firmware *fw;
	const u8 *data;
	u32 magic, version, num_blocks;
	unsigned int header_size, table_size;
	unsigned int i;
	int ret;

	ret = request_firmware(&fw, UA_MIXER_FW_NAME, &ua->pdev->dev);
	if (ret) {
		if (ret == -ENOENT)
			dev_warn(&ua->pdev->dev,
				 "mixer firmware '%s' not found in /lib/firmware/\n",
				 UA_MIXER_FW_NAME);
		else
			dev_err(&ua->pdev->dev,
				"failed to load mixer firmware: %d\n", ret);
		return ret;
	}

	data = fw->data;

	/* Validate header (16 bytes minimum) */
	if (fw->size < 16) {
		dev_err(&ua->pdev->dev, "mixer firmware too small (%zu bytes)\n",
			fw->size);
		ret = -EINVAL;
		goto out;
	}

	magic = le32_to_cpup((__le32 *)&data[0]);
	version = le32_to_cpup((__le32 *)&data[4]);
	num_blocks = le32_to_cpup((__le32 *)&data[8]);

	if (magic != UA_MIXER_FW_MAGIC) {
		dev_err(&ua->pdev->dev,
			"mixer firmware bad magic: 0x%08x (expected 0x%08x)\n",
			magic, UA_MIXER_FW_MAGIC);
		ret = -EINVAL;
		goto out;
	}

	if (version != UA_MIXER_FW_VERSION) {
		dev_err(&ua->pdev->dev,
			"mixer firmware version %u unsupported (expected %u)\n",
			version, UA_MIXER_FW_VERSION);
		ret = -EINVAL;
		goto out;
	}

	if (num_blocks == 0 || num_blocks > 64) {
		dev_err(&ua->pdev->dev,
			"mixer firmware invalid block count: %u\n", num_blocks);
		ret = -EINVAL;
		goto out;
	}

	header_size = 16;
	table_size = num_blocks * 12;

	if (fw->size < header_size + table_size) {
		dev_err(&ua->pdev->dev,
			"mixer firmware truncated (need %u, got %zu)\n",
			header_size + table_size, fw->size);
		ret = -EINVAL;
		goto out;
	}

	dev_info(&ua->pdev->dev,
		 "loading mixer firmware: %u blocks, %zu bytes\n",
		 num_blocks, fw->size);

	/*
	 * 3-phase FW load (from Windows driver analysis):
	 *   0xD0000 (pre-load) → 0x120000 (upload) → 0xE0000 (post-load)
	 *
	 * Pre/post phases DISABLED: sending 0xD0000 before main FW
	 * consumes ring entries and desyncs the ring when DSP can't
	 * respond (bootloader only handles 0x120000). The pre/post
	 * phases may only apply to SUBSEQUENT loads (rate changes),
	 * not the initial cold boot load.
	 *
	 * TODO: Try pre/post after initial load succeeds, or for
	 * DSPs 1-3 after DSP 0 is fully initialized.
	 */

	/* Phase 2: Main firmware upload (0x120000) — iterate blocks */
	for (i = 0; i < num_blocks; i++) {
		u32 sram_addr, block_size, block_offset;
		unsigned int entry_off = header_size + i * 12;

		sram_addr = le32_to_cpup((__le32 *)&data[entry_off]);
		block_size = le32_to_cpup((__le32 *)&data[entry_off + 4]);
		block_offset = le32_to_cpup((__le32 *)&data[entry_off + 8]);

		/* Validate block bounds */
		if (block_offset + block_size > fw->size) {
			dev_err(&ua->pdev->dev,
				"block %u overflows firmware (off=%u size=%u total=%zu)\n",
				i, block_offset, block_size, fw->size);
			ret = -EINVAL;
			goto out;
		}

		if (block_size & 3) {
			dev_err(&ua->pdev->dev,
				"block %u size %u not 4-byte aligned\n",
				i, block_size);
			ret = -EINVAL;
			goto out;
		}

		/*
		 * Try cmd=0x120000 (hardware driver LoadFirmware address) with param
		 * 0x80040000 first.  The hardware driver sends the entire firmware as
		 * one _sendBlock with these values.  Our per-block SRAM
		 * addresses (0x0e8cc000 etc.) may not be valid ring commands.
		 *
		 * If 0x120000 fails, fall back to per-block SRAM addresses.
		 */
		ret = ua_dsp_send_block(ua, 0, 0x120000, 0x80040000,
					&data[block_offset], block_size);
		if (ret) {
			dev_info(&ua->pdev->dev,
				 "block %u: cmd=0x120000 failed (%d), trying sram=0x%08x\n",
				 i, ret, sram_addr);
			ret = ua_dsp_send_block(ua, 0, sram_addr, 1,
						&data[block_offset], block_size);
		}
		if (ret) {
			dev_err(&ua->pdev->dev,
				"block %u (sram=0x%08x, %u bytes) send failed: %d\n",
				i, sram_addr, block_size, ret);
			goto out;
		}
	}

	ua->fw_loaded = true;
	dev_info(&ua->pdev->dev,
		 "mixer firmware loaded: %u blocks sent to DSP 0\n",
		 num_blocks);

out:
	release_firmware(fw);
	return ret;
}

/**
 * ua_dsp_load_mixer_blocks_to - Load mixer firmware to a specific DSP
 * @ua: device
 * @dsp_idx: target DSP (1-3)
 *
 * Same as ua_dsp_load_mixer_blocks but sends blocks to dsp_idx
 * instead of DSP 0. Uses the same firmware file.
 */
int ua_dsp_load_mixer_blocks_to(struct ua_device *ua, unsigned int dsp_idx)
{
	const struct firmware *fw;
	const u8 *data;
	u32 num_blocks;
	unsigned int header_size, i;
	int ret;

	if (dsp_idx >= ua->num_dsps || !ua->dsp[dsp_idx].rings_allocated)
		return -ENODEV;

	ret = request_firmware(&fw, UA_MIXER_FW_NAME, &ua->pdev->dev);
	if (ret)
		return ret;

	data = fw->data;
	if (fw->size < 16 ||
	    le32_to_cpup((__le32 *)&data[0]) != UA_MIXER_FW_MAGIC) {
		release_firmware(fw);
		return -EINVAL;
	}

	num_blocks = le32_to_cpup((__le32 *)&data[8]);
	header_size = 16;

	for (i = 0; i < num_blocks; i++) {
		u32 sram_addr, block_size, block_offset;
		unsigned int entry_off = header_size + i * 12;

		sram_addr = le32_to_cpup((__le32 *)&data[entry_off]);
		block_size = le32_to_cpup((__le32 *)&data[entry_off + 4]);
		block_offset = le32_to_cpup((__le32 *)&data[entry_off + 8]);

		if (block_offset + block_size > fw->size || (block_size & 3))
			break;

		ret = ua_dsp_send_block(ua, dsp_idx, 0x120000, 0x80040000,
					&data[block_offset], block_size);
		if (ret) {
			dev_warn(&ua->pdev->dev,
				 "DSP %u block %u failed: %d\n",
				 dsp_idx, i, ret);
			break;
		}
	}

	dev_info(&ua->pdev->dev,
		 "DSP %u: %u/%u firmware blocks loaded\n",
		 dsp_idx, i, num_blocks);

	release_firmware(fw);
	return (i == num_blocks) ? 0 : ret;
}

/* ----------------------------------------------------------------
 * DSP connect
 * ---------------------------------------------------------------- */

/**
 * ua_dsp_connect_all - Send connect commands to all DSPs
 * @ua: device (must hold ua->lock)
 *
 * Protocol (from hardware driver DSP connect):
 *   Phase 1: For each DSP i: inline {0x00230002, 1, 0, 0} + doorbell
 *   Phase 2: DSP 0 only: inline {0x00100002, 0, 0, 0} + doorbell
 */
int ua_dsp_connect_all(struct ua_device *ua)
{
	struct ua_ring_entry entry;
	unsigned int i;
	int ret;

	if (!ua->fw_loaded)
		dev_info(&ua->pdev->dev,
			 "DSP connect: firmware not loaded (testing)\n");

	/* Phase 1: Send connect command to each DSP */
	for (i = 0; i < ua->num_dsps; i++) {
		struct ua_dsp_state *ds = &ua->dsp[i];
		u32 bank_base = ua_dsp_base(i);

		if (!ds->rings_allocated)
			continue;

		/* Reset cmd ring for a fresh command */
		ua_dsp_ring_program(ua, &ds->cmd,
				    bank_base + UA_RING_CMD_OFFSET);

		entry = (struct ua_ring_entry){
			.word0 = cpu_to_le32(UA_DSP_CMD_CONNECT),
			.word1 = cpu_to_le32(1),
			.word2 = 0,
			.word3 = 0,
		};

		ret = ua_dsp_ring_put_entry(&ds->cmd, &entry);
		if (ret) {
			dev_err(&ua->pdev->dev,
				"DSP %u: connect entry failed (%d)\n", i, ret);
			return ret;
		}

		ua_dsp_ring_doorbell(ua, &ds->cmd,
				     bank_base + UA_RING_CMD_OFFSET);

		usleep_range(5000, 10000);
		dev_info(&ua->pdev->dev,
			 "DSP %u connect: pos=%u (expect 1)\n", i,
			 ua_read(ua, bank_base + UA_RING_CMD_OFFSET +
				 UA_RING_POSITION));
	}

	/* Brief delay between phases */
	usleep_range(1000, 2000);

	/* Phase 2: Send finalize command to DSP 0 */
	if (ua->dsp[0].rings_allocated) {
		u32 bank_base = ua_dsp_base(0);

		ua_dsp_ring_program(ua, &ua->dsp[0].cmd,
				    bank_base + UA_RING_CMD_OFFSET);

		entry = (struct ua_ring_entry){
			.word0 = cpu_to_le32(UA_DSP_CMD_FINALIZE),
			.word1 = 0,
			.word2 = 0,
			.word3 = 0,
		};

		ret = ua_dsp_ring_put_entry(&ua->dsp[0].cmd, &entry);
		if (ret) {
			dev_err(&ua->pdev->dev,
				"DSP 0: finalize entry failed (%d)\n", ret);
			return ret;
		}

		ua_dsp_ring_doorbell(ua, &ua->dsp[0].cmd,
				     bank_base + UA_RING_CMD_OFFSET);
	}

	usleep_range(5000, 10000);
	dev_info(&ua->pdev->dev,
		 "DSP 0 finalize: pos=%u (expect 1)\n",
		 ua_read(ua, ua_dsp_base(0) + UA_RING_CMD_OFFSET +
			 UA_RING_POSITION));

	ua->dsps_connected = true;
	dev_info(&ua->pdev->dev, "all %u DSPs connected\n", ua->num_dsps);
	return 0;
}

/* ----------------------------------------------------------------
 * Plugin chain activation
 * ---------------------------------------------------------------- */

#include "ua_plugin_chain.h"

/**
 * ua_dsp_activate_plugin_chain - Send plugin chain init to DSP 0
 * @ua: device (must hold ua->lock)
 *
 * Replays the SRAM_CFG + ROUTING + DMA_REF + MODULE_ACTIVATE + SYNCH
 * sequence captured from a working Windows driver.  This activates the
 * DSP mixer/routing modules so that SEL131 SetMixerParam commands
 * (monitor volume, gain, talkback, dim) take effect.
 *
 * Without this, the DSP kernel runs (ring buffer commands like bus
 * coefficients work) but the mixer task never starts, so SRAM-based
 * parameter writes are ignored.
 *
 * Data extracted from tools/captures/win-dsp0-ring-complete-20260319.json
 */
int ua_dsp_activate_plugin_chain(struct ua_device *ua)
{
	struct ua_dsp_state *ds = &ua->dsp[0];
	u32 bank_base = ua_dsp_base(0);
	u32 cmd_base = bank_base + UA_RING_CMD_OFFSET;
	struct ua_ring_entry entry;
	int i, batch, ret;
	u32 pos;

	if (!ds->rings_allocated) {
		dev_warn(&ua->pdev->dev,
			 "plugin chain: DSP 0 rings not ready\n");
		return -ENODEV;
	}

	dev_info(&ua->pdev->dev,
		 "plugin chain: sending %d commands to DSP 0\n",
		 UA_PLUGIN_CHAIN_COUNT);

	/*
	 * Send commands in batches to avoid overflowing the ring buffer.
	 * The ring has 256 entries; send 64 at a time, then wait for
	 * the DSP to consume them before sending more.
	 */
	for (i = 0; i < UA_PLUGIN_CHAIN_COUNT; i += batch) {
		batch = min(64, UA_PLUGIN_CHAIN_COUNT - i);

		/* Reset ring for this batch */
		ua_dsp_ring_program(ua, &ds->cmd, cmd_base);

		for (int j = 0; j < batch; j++) {
			const u32 *cmd = ua_plugin_chain_data[i + j];

			entry = (struct ua_ring_entry){
				.word0 = cpu_to_le32(cmd[0]),
				.word1 = cpu_to_le32(cmd[1]),
				.word2 = cpu_to_le32(cmd[2]),
				.word3 = cpu_to_le32(cmd[3]),
			};

			ret = ua_dsp_ring_put_entry(&ds->cmd, &entry);
			if (ret) {
				dev_err(&ua->pdev->dev,
					"plugin chain: entry %d failed\n",
					i + j);
				return ret;
			}
		}

		/* Single doorbell for the batch */
		ua_dsp_ring_doorbell(ua, &ds->cmd, cmd_base);

		/* Wait for DSP to consume */
		usleep_range(10000, 20000);
		pos = ua_read(ua, cmd_base + UA_RING_POSITION);
		if (pos < (u32)batch)
			dev_dbg(&ua->pdev->dev,
				"plugin chain: batch %d-%d pos=%u/%d\n",
				i, i + batch - 1, pos, batch);
	}

	dev_info(&ua->pdev->dev,
		 "plugin chain: activated (%d commands sent)\n",
		 UA_PLUGIN_CHAIN_COUNT);
	return 0;
}

/* ----------------------------------------------------------------
 * Bus coefficient commands (SEL130 / SetMixerBusParam equivalent)
 *
 * From hardware driver analysis (SetMixerBusParam):
 * Bus coefficients (fader, pan, send levels) are submitted as ring
 * buffer commands to DSP 0, NOT through the BAR0+0x38xx mixer setting
 * registers.  The command format is:
 *
 *   word0: 0x001D0004   (cmd type 0x1D, 4 dwords)
 *   word1: bus_id        (0x00-0x1F)
 *   word2: sub_param     (0=main, 1=CUE1, 2=CUE2, 3=gainL, 4=gainR)
 *   word3: value         (IEEE 754 float as u32)
 *
 * This is a COMPLETELY DIFFERENT hardware path from SEL131/SEL132
 * which use mask/value paired writes to mixer setting registers.
 * ---------------------------------------------------------------- */

#define UA_DSP_CMD_BUS_PARAM	0x001D0004

/**
 * ua_dsp_ring_reset - Sync software write_ptr to FPGA POSITION
 * @ua: device
 * @dsp_idx: DSP index (0-3)
 *
 * The FPGA POSITION counter is monotonic — it never resets when the
 * ring is reprogrammed.  After FW loading, POSITION may be at ~275
 * while write_ptr was reset to 0 by ring_program().  New entries at
 * 0..N are invisible to the FPGA because POSITION > N.
 *
 * Fix: set write_ptr = POSITION so new entries start where the FPGA
 * expects them.  Also write WRITE_PTR register so the FPGA knows
 * our starting position.
 */
void ua_dsp_ring_reset(struct ua_device *ua, unsigned int dsp_idx)
{
	struct ua_dsp_state *ds = &ua->dsp[dsp_idx];
	u32 bank_base = ua_dsp_base(dsp_idx);
	u32 ring_base = bank_base + UA_RING_CMD_OFFSET;
	u32 pos;

	if (!ds->rings_allocated)
		return;

	pos = ua_read(ua, ring_base + UA_RING_POSITION);
	ds->cmd.write_ptr = pos;
	ua_write(ua, ring_base + UA_RING_WRITE_PTR, pos);
}

/**
 * ua_dsp_set_bus_param - Send a bus coefficient to DSP via ring buffer
 * @ua: device (caller must hold ua->lock)
 * @bus_id: bus identifier (0x00-0x1F)
 * @sub_param: sub-parameter (0=main, 1=CUE1, 2=CUE2, 3=gainL, 4=gainR)
 * @value_u32: coefficient as IEEE 754 float packed in u32
 *
 * Returns 0 on success, negative errno on failure.
 */
int ua_dsp_set_bus_param(struct ua_device *ua, u32 bus_id,
			 u32 sub_param, u32 value_u32)
{
	struct ua_dsp_state *dsp0 = &ua->dsp[0];
	u32 bank_base = ua_dsp_base(0);
	u32 ring_base = bank_base + UA_RING_CMD_OFFSET;
	struct ua_ring_entry entry;
	int ret;

	if (!dsp0->rings_allocated)
		return -ENODEV;

	if (bus_id > 0x1f || sub_param > 6)
		return -EINVAL;

	entry = (struct ua_ring_entry){
		.word0 = cpu_to_le32(UA_DSP_CMD_BUS_PARAM),
		.word1 = cpu_to_le32(bus_id),
		.word2 = cpu_to_le32(sub_param),
		.word3 = cpu_to_le32(value_u32),
	};

	ret = ua_dsp_ring_put_entry(&dsp0->cmd, &entry);
	if (ret) {
		dev_warn(&ua->pdev->dev,
			 "bus_param ring full (bus=%u sub=%u)\n",
			 bus_id, sub_param);
		return ret;
	}

	ua_dsp_ring_doorbell(ua, &dsp0->cmd, ring_base);

	return 0;
}

/**
 * ua_dsp_ring_send_raw - Send an arbitrary 4-word inline entry to any DSP
 * @ua: device (caller must hold ua->lock)
 * @dsp_idx: target DSP (0-3)
 * @w0..w3: the four command words
 *
 * For testing ring buffer commands from userspace.
 */
int ua_dsp_ring_send_raw(struct ua_device *ua, unsigned int dsp_idx,
			  u32 w0, u32 w1, u32 w2, u32 w3)
{
	struct ua_dsp_state *ds;
	u32 ring_base;
	struct ua_ring_entry entry;
	int ret;

	if (dsp_idx >= ua->num_dsps)
		return -EINVAL;

	ds = &ua->dsp[dsp_idx];
	if (!ds->rings_allocated)
		return -ENODEV;

	ring_base = ua_dsp_base(dsp_idx) + UA_RING_CMD_OFFSET;

	entry = (struct ua_ring_entry){
		.word0 = cpu_to_le32(w0),
		.word1 = cpu_to_le32(w1),
		.word2 = cpu_to_le32(w2),
		.word3 = cpu_to_le32(w3),
	};

	ret = ua_dsp_ring_put_entry(&ds->cmd, &entry);
	if (ret)
		return ret;

	ua_dsp_ring_doorbell(ua, &ds->cmd, ring_base);
	return 0;
}

/**
 * ua_dsp_flush_bus_params - Ensure all queued bus params are processed
 * @ua: device
 *
 * Waits briefly for the DSP to consume queued ring entries.
 */
void ua_dsp_flush_bus_params(struct ua_device *ua)
{
	u32 bank_base = ua_dsp_base(0);
	u32 ring_base = bank_base + UA_RING_CMD_OFFSET;
	u32 pos;
	int i;

	for (i = 0; i < 10; i++) {
		pos = ua_read(ua, ring_base + UA_RING_POSITION);
		if (pos >= ua->dsp[0].cmd.write_ptr)
			return;
		usleep_range(1000, 2000);
	}
	dev_info(&ua->pdev->dev,
		 "bus_param flush timeout: pos=%u wr=%u (not consumed)\n",
		 pos, ua->dsp[0].cmd.write_ptr);
}

/* ----------------------------------------------------------------
 * DSP Module Configuration (multichannel capture)
 *
 * Replays the Windows DSP 0 connect sequence that configures 6 mixer
 * modules in SHARC SRAM.  Without this, only type 0x00 capture channels
 * (direct preamp, Mic 1-2) work.  Type 0x01 channels (Mic 3-4, Virtual,
 * S/PDIF, AUX) need the DSP mixer routing configured here.
 *
 * Sequence per module:
 *   1. SRAM_CFG clears (header + I/O + coefficient blocks)
 *   2. DMA write module header (9 dwords)
 *   3. DMA write I/O descriptor (11-22 dwords)
 * Then:
 *   4. EnableSynchProcessing for modules 0 and 1
 *   5. RoutingEnable commands
 *
 * Data source: tools/captures/win-dsp0-ring-complete-20260319.json
 * ---------------------------------------------------------------- */

/**
 * ua_dsp_configure_modules - Configure DSP 0 mixer modules for multichannel
 * @ua: device (caller must hold ua->lock)
 *
 * Sends the SRAM configuration sequence captured from Windows to DSP 0.
 * Must be called after ACEFACE connect and ring buffer init, before
 * transport restart.  Bus coefficients from the mixer daemon will fill
 * the coefficient blocks afterward.
 *
 * Returns 0 on success, negative errno on failure.
 */
int ua_dsp_configure_modules(struct ua_device *ua)
{
	struct ua_dsp_state *ds = &ua->dsp[0];
	u32 bank_base, cmd_base;
	void *dma_page;
	dma_addr_t dma_addr;
	struct ua_ring_entry ent;
	const struct ua_module_addrs *mod;
	unsigned int i, j;
	int polls;
	u32 pos;

	if (!ds->rings_allocated) {
		dev_warn(&ua->pdev->dev, "module_cfg: DSP 0 rings not ready\n");
		return -ENODEV;
	}

	bank_base = ua_dsp_base(0);
	cmd_base = bank_base + UA_RING_CMD_OFFSET;

	/*
	 * Allocate a single DMA page for all module payloads.
	 * Layout: 6 modules × (header at +0x00, I/O desc at +0x40)
	 * each in a 128-byte slot = 768 bytes total + zeros at offset 0.
	 */
	dma_page = dma_alloc_coherent(&ua->pdev->dev, UA_PCIE_PAGE_SIZE,
				      &dma_addr, GFP_KERNEL);
	if (!dma_page)
		return -ENOMEM;
	memset(dma_page, 0, UA_PCIE_PAGE_SIZE);

	/*
	 * Build DMA payloads for all 6 modules.
	 * Slot layout per module (128 bytes each):
	 *   +0x00: 9-dword header (36 bytes)
	 *   +0x40: I/O descriptor (up to 88 bytes = 22 dwords)
	 */
#define MOD_SLOT_SIZE	128
#define MOD_HDR_OFF	0x00
#define MOD_IO_OFF	0x40

	for (i = 0; i < UA_MODULE_COUNT; i++) {
		u32 *hdr, *io;

		mod = &ua_x4_modules[i];
		hdr = (u32 *)((u8 *)dma_page + (i + 1) * MOD_SLOT_SIZE + MOD_HDR_OFF);
		io  = (u32 *)((u8 *)dma_page + (i + 1) * MOD_SLOT_SIZE + MOD_IO_OFF);

		/* Build header (9 words) */
		memcpy(hdr, ua_mod_hdr_template, 9 * 4);
		hdr[1] = mod->sram_hdr;  /* self-referencing address */

		if (i == 0) {
			hdr[8] = UA_MOD_EP_MIXER;
		} else {
			hdr[8] = UA_MOD_EP_ROUTING;
		}

		/* Module 5: 2 inputs, 2 outputs */
		if (i == 5) {
			hdr[4] = 2;
			hdr[5] = 2;
		}

		/* Build I/O descriptor */
		if (i == 0) {
			memcpy(io, ua_mod0_io_template, sizeof(ua_mod0_io_template));

			io[1] = mod->sram_io;
		} else if (i <= 2) {
			memcpy(io, ua_mod_code_io_template, sizeof(ua_mod_code_io_template));

			io[1] = mod->sram_io;
		} else if (i <= 4) {
			/* Passthrough: self-addr is sram_io + 8 (Windows capture) */
			memcpy(io, ua_mod_passthru_io_template, sizeof(ua_mod_passthru_io_template));

			io[1] = mod->sram_io + 8;
		} else {
			memcpy(io, ua_mod5_io_template, sizeof(ua_mod5_io_template));

			io[1] = mod->sram_io;
		}
	}

	/*
	 * Verify ring space: extended wiring + 6 modules × ~4 entries +
	 * EnableSynch + RoutingEnable + bus coefficients.
	 */
	if (ds->cmd.write_ptr + 120 > ds->cmd.capacity) {
		dev_warn(&ua->pdev->dev,
			 "module_cfg: ring too full (wr=%u cap=%u)\n",
			 ds->cmd.write_ptr, ds->cmd.capacity);
		goto out_free;
	}

	/*
	 * Phase 0: Send extended IO wiring block (DMA[152]).
	 * Contains cross-module references, preamp ADC routing,
	 * and biquad coefficients. Must go BEFORE per-module writes.
	 */
	{
		dma_addr_t ext_dma;
		u32 *ext_buf;

		/* Place extended wiring at end of DMA page */
		ext_buf = (u32 *)((u8 *)dma_page + UA_PCIE_PAGE_SIZE -
				  UA_MOD_EXTENDED_WORDS * 4);
		memcpy(ext_buf, ua_mod_extended_wiring,
		       UA_MOD_EXTENDED_WORDS * 4);

		ext_dma = dma_addr + UA_PCIE_PAGE_SIZE -
			  UA_MOD_EXTENDED_WORDS * 4;

		ent = (struct ua_ring_entry){
			.word0 = cpu_to_le32(UA_RING_DMA_REF |
					     UA_MOD_EXTENDED_WORDS),
			.word1 = 0,
			.word2 = cpu_to_le32(lower_32_bits(ext_dma)),
			.word3 = cpu_to_le32(upper_32_bits(ext_dma)),
		};
		ua_dsp_ring_put_entry(&ds->cmd, &ent);
	}

	/* Phase 1: DMA writes for each module (no SRAM clears). */
	for (i = 0; i < UA_MODULE_COUNT; i++) {
		dma_addr_t hdr_dma, io_dma;
		unsigned int io_dma_words;

		mod = &ua_x4_modules[i];

		/*
		 * SRAM clears REMOVED — they destroy firmware's
		 * default mixer output routing on cold boot.
		 * The SRAM is empty after FW load anyway.
		 */

		/* DMA: write zeros to header area (matches Windows entry) */
		ent = (struct ua_ring_entry){
			.word0 = cpu_to_le32(UA_RING_DMA_REF | (i == 0 ? 15 : 11)),
			.word1 = 0,
			.word2 = cpu_to_le32(lower_32_bits(dma_addr)),  /* page start = zeros */
			.word3 = cpu_to_le32(upper_32_bits(dma_addr)),
		};
		ua_dsp_ring_put_entry(&ds->cmd, &ent);

		/* DMA: write module header (9 dwords) */
		hdr_dma = dma_addr + (i + 1) * MOD_SLOT_SIZE + MOD_HDR_OFF;
		ent = (struct ua_ring_entry){
			.word0 = cpu_to_le32(UA_RING_DMA_REF | 9),
			.word1 = 0,
			.word2 = cpu_to_le32(lower_32_bits(hdr_dma)),
			.word3 = cpu_to_le32(upper_32_bits(hdr_dma)),
		};
		ua_dsp_ring_put_entry(&ds->cmd, &ent);

		/* DMA: write I/O descriptor */
		io_dma = dma_addr + (i + 1) * MOD_SLOT_SIZE + MOD_IO_OFF;
		io_dma_words = (i == 0) ? 22 : 11;
		ent = (struct ua_ring_entry){
			.word0 = cpu_to_le32(UA_RING_DMA_REF | io_dma_words),
			.word1 = 0,
			.word2 = cpu_to_le32(lower_32_bits(io_dma)),
			.word3 = cpu_to_le32(upper_32_bits(io_dma)),
		};
		ua_dsp_ring_put_entry(&ds->cmd, &ent);
	}

	/*
	 * Phase 2: EnableSynchProcessing — activate modules 1 then 0.
	 */
	for (i = 0; i < UA_SYNCH_COUNT; i++) {
		const struct ua_synch_target *st = &ua_x4_synch[i];

		ent = (struct ua_ring_entry){
			.word0 = cpu_to_le32(UA_ENABLE_SYNCH),
			.word1 = cpu_to_le32(st->module_ref),
			.word2 = cpu_to_le32(ua_x4_modules[st->module_idx].sram_hdr),
			.word3 = 0,
		};
		ua_dsp_ring_put_entry(&ds->cmd, &ent);

		/*
		 * RoutingEnable commands follow each EnableSynch.
		 * Module 1 (slot 0x1B): 3 routing cmds (indices 0-2)
		 * Module 0 (slot 0x0A): 3 routing cmds (indices 3-5)
		 */
		for (j = i * 3; j < i * 3 + 3 && j < UA_ROUTING_COUNT; j++) {
			const struct ua_routing_cmd *rc = &ua_x4_routing[j];

			ent = (struct ua_ring_entry){
				.word0 = cpu_to_le32(UA_ROUTING_ENABLE),
				.word1 = cpu_to_le32(rc->slot),
				.word2 = cpu_to_le32(rc->sub_param),
				.word3 = cpu_to_le32(rc->mask),
			};
			ua_dsp_ring_put_entry(&ds->cmd, &ent);
		}
	}

	/*
	 * Phase 3: Initial bus coefficients for analog inputs.
	 *
	 * Windows sends stereo bus coefficients right after routing enables.
	 * Format: {0x001D0004, bus_id, (ch<<24)|sub_param, value_f32}
	 * ch=0 for L, ch=1 for R (0x01000000).
	 *
	 * Set unity gain (0.707 = -3dB = 0x3F3504F3) on main mix (sub=0)
	 * and gain L/R (sub=3,4) for buses 0-3 (Analog In 1-4).
	 * The daemon will override these with actual fader values.
	 */
	{
		static const u32 unity = 0x3F3504F3;  /* float 0.707 */
		unsigned int bus;

		for (bus = 0; bus < 4; bus++) {
			/* RoutingEnable for this bus first */
			ent = (struct ua_ring_entry){
				.word0 = cpu_to_le32(UA_ROUTING_ENABLE),
				.word1 = cpu_to_le32(bus),
				.word2 = cpu_to_le32(0x08),
				.word3 = cpu_to_le32(0x20),
			};
			ua_dsp_ring_put_entry(&ds->cmd, &ent);

			/* Main mix L + R (sub=0) */
			ent = (struct ua_ring_entry){
				.word0 = cpu_to_le32(0x001D0004),
				.word1 = cpu_to_le32(bus),
				.word2 = cpu_to_le32(0x00000000),
				.word3 = cpu_to_le32(unity),
			};
			ua_dsp_ring_put_entry(&ds->cmd, &ent);
			ent.word2 = cpu_to_le32(0x01000000);  /* ch=1 (R) */
			ua_dsp_ring_put_entry(&ds->cmd, &ent);

			/* Gain L (sub=3) */
			ent.word2 = cpu_to_le32(0x00000003);
			ua_dsp_ring_put_entry(&ds->cmd, &ent);
			ent.word2 = cpu_to_le32(0x01000003);
			ua_dsp_ring_put_entry(&ds->cmd, &ent);

			/* Gain R (sub=4) */
			ent.word2 = cpu_to_le32(0x00000004);
			ent.word3 = 0;  /* zero for R gain on L bus */
			ua_dsp_ring_put_entry(&ds->cmd, &ent);
			ent.word2 = cpu_to_le32(0x01000004);
			ua_dsp_ring_put_entry(&ds->cmd, &ent);

			/* Channel mask routing (sub=7) */
			ent = (struct ua_ring_entry){
				.word0 = cpu_to_le32(UA_ROUTING_ENABLE),
				.word1 = cpu_to_le32(bus),
				.word2 = cpu_to_le32(0x07),
				.word3 = cpu_to_le32(0x0001FFFF),
			};
			ua_dsp_ring_put_entry(&ds->cmd, &ent);
		}
	}

	/* Ring the doorbell once for the entire batch */
	ua_dsp_ring_doorbell(ua, &ds->cmd, cmd_base);

	dev_info(&ua->pdev->dev,
		 "module_cfg: queued %u entries for %u modules\n",
		 ds->cmd.write_ptr, UA_MODULE_COUNT);

	/* Wait for DSP to process all entries */
	for (polls = 0; polls < 500; polls++) {
		pos = ua_read(ua, cmd_base + UA_RING_POSITION);
		if (pos >= ds->cmd.write_ptr)
			break;
		usleep_range(1000, 2000);
	}

	if (polls >= 500) {
		dev_warn(&ua->pdev->dev,
			 "module_cfg: timeout (pos=%u target=%u)\n",
			 pos, ds->cmd.write_ptr);
	} else {
		dev_info(&ua->pdev->dev,
			 "module_cfg: DSP consumed all entries (pos=%u) in %ums\n",
			 pos, polls);
	}

out_free:
	dma_free_coherent(&ua->pdev->dev, UA_PCIE_PAGE_SIZE,
			  dma_page, dma_addr);
	return 0;
}

/* ----------------------------------------------------------------
 * IO Descriptor SRAM Configuration
 *
 * The IO descriptor regions at 0xC1A4 (input) and 0xC2C4 (output)
 * tell the FPGA/DSP how to map DMA channels to audio signals.
 *
 * Hardware format (from BAR0 dump):
 *   6-word header + packed 16-bit channel pairs + 0x00FF00FF padding
 *
 * Linux was writing raw 32-bit channel IDs with no header — wrong
 * format.  This caused capture to return all zeros despite DMA
 * working correctly at the FPGA level.
 * ---------------------------------------------------------------- */

#include "ua_routing.h"

/**
 * ua_dsp_send_routing - Write IO descriptors to SRAM for audio routing
 * @ua: device (must hold ua->lock)
 *
 * Writes the hardware-format IO descriptor data to the SRAM regions at
 * 0xC1A4 (input) and 0xC2C4 (output).  These descriptors map DMA
 * channels to audio signals and must match what the DSP firmware expects.
 *
 * Must be called after ACEFACE connect and before transport start.
 *
 * Returns 0 on success, negative errno on failure.
 */
int ua_dsp_send_routing(struct ua_device *ua)
{
	const struct ua_routing_config *cfg;
	unsigned int i;

	cfg = ua_get_routing_config(ua->device_type);
	if (!cfg) {
		dev_warn(&ua->pdev->dev,
			 "no routing config for device type 0x%02x\n",
			 ua->device_type);
		return -ENODATA;
	}

	if (!cfg->io_desc_input || !cfg->io_desc_output) {
		dev_warn(&ua->pdev->dev,
			 "no IO descriptor data for device type 0x%02x\n",
			 ua->device_type);
		return -ENODATA;
	}

	/* Write input IO descriptors (72 words at 0xC1A4) */
	for (i = 0; i < UA_IO_DESC_WORDS; i++)
		ua_write(ua, UA_REG_IN_NAMES_BASE + i * 4,
			 cfg->io_desc_input[i]);

	/* Write output IO descriptors (72 words at 0xC2C4) */
	for (i = 0; i < UA_IO_DESC_WORDS; i++)
		ua_write(ua, UA_REG_OUT_NAMES_BASE + i * 4,
			 cfg->io_desc_output[i]);

	dev_info(&ua->pdev->dev,
		 "IO descriptors written: input=%u words, output=%u words\n",
		 UA_IO_DESC_WORDS, UA_IO_DESC_WORDS);

	return 0;
}

/**
 * ua_dsp_load_programs - Send DSP audio routing programs to DSP 0
 *
 * Loads the mixer core, output routing, and capture routing programs
 * captured from hardware observation. These programs
 * tell the DSP how to route audio between preamp inputs, mix buses,
 * and DMA record/playback channels.
 *
 * Must be called after ACEFACE connect completes.
 * Uses ring buffer sendBlock with program command (0x0027012A),
 * NOT firmware load command (0x00120000).
 *
 * Command word from hardware driver analysis at offset 0x39158:
 *   MOVZ W1, #0x12A; MOVK W1, #0x27, LSL#16 → cmd = 0x0027012A
 *   W2 = 0x1 (param)
 */
/* Program load command from hardware driver command table at 0x8aade.
 * Different from firmware load (0x00120000) — this is for
 * DSP audio routing programs ("Bill" blocks). */
#define UA_DSP_PROG_CMD   0x00360000
#define UA_DSP_PROG_PARAM 0x80040000

int ua_dsp_load_programs(struct ua_device *ua)
{
	int dsp, i, ret;

	if (ua->device_type != UA_DEV_APOLLO_X4) {
		dev_info(&ua->pdev->dev,
			 "DSP programs: only x4 supported, skipping\n");
		return 0;
	}

	/*
	 * Send programs to all 4 DSPs matching hardware driver sequence:
	 *   DSP 0: A5 (mixer) + C2 (routing) + DB (capture) = 3 blocks
	 *   DSP 1-3: A5 (mixer) + C2 (routing) = 2 blocks each
	 */
	for (dsp = 0; dsp < ua->num_dsps && dsp < 4; dsp++) {
		/* All DSPs get A5 + C2 (first 2 programs) */
		int num = (dsp == 0) ? UA_X4_DSP0_NUM_PROGRAMS : 2;

		dev_info(&ua->pdev->dev,
			 "loading %d programs for DSP %d\n", num, dsp);

		for (i = 0; i < num; i++) {
			const struct ua_dsp_program *prog =
				&ua_x4_dsp0_programs[i];

			ret = ua_dsp_send_block(ua, dsp, UA_FW_CMD,
						UA_FW_PARAM,
						prog->data, prog->size);
		if (ret) {
			dev_err(&ua->pdev->dev,
				"DSP program %s failed: %d\n",
				prog->name, ret);
			return ret;
		}
		dev_info(&ua->pdev->dev,
			 "  DSP%d: loaded %s (%u bytes)\n",
			 dsp, prog->name, prog->size);
		}
	}

	dev_info(&ua->pdev->dev, "DSP audio programs loaded (4 DSPs)\n");
	return 0;
}
