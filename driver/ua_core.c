// SPDX-License-Identifier: GPL-2.0-only
/*
 * Universal Audio Apollo Thunderbolt — Linux PCIe Driver
 *
 * Copyright (c) 2026 apollo-linux contributors
 *
 * Reverse engineered from UAD2System.kext v11.8.1 (com.uaudio.driver.UAD2System)
 * and UAD-2 SDK Support.framework.
 *
 * Core PCIe driver:
 *  - Probes UA devices on the PCIe bus (vendor 0x1A00)
 *  - Maps BAR0 for MMIO register access
 *  - Reads FPGA revision, detects device type from serial prefix
 *  - Enumerates SHARC DSPs
 *  - Sets up MSI interrupts with audio dispatch
 *  - Exposes a /dev/ua_apolloN chardev for userspace control
 *  - Initializes ALSA audio subsystem (ua_audio.c)
 *
 * Hardware topology (Thunderbolt):
 *   Mac/PC → TB controller → Intel JHL8440 switch → PCIe bridge → Apollo
 *   The device appears as a standard PCIe endpoint to the OS.
 */

#include <linux/module.h>
#include <linux/pci.h>
#include <linux/interrupt.h>
#include <linux/cdev.h>
#include <linux/fs.h>
#include <linux/uaccess.h>
#include <linux/idr.h>
#include <linux/slab.h>
#include <linux/version.h>
#include <linux/delay.h>

#include <sound/control.h>

#include "ua_apollo.h"
#include "ua_ioctl.h"

#define DRIVER_NAME	"ua_apollo"
#define DRIVER_DESC	"Universal Audio Apollo Thunderbolt PCIe driver"
#define DRIVER_VERSION	"0.2.0"

/*
 * Kernel version compat: class_create() lost the owner param in 6.4.
 * Before 6.4 class_create() is itself a macro, so redefining it (as this
 * used to) breaks the build there; use a wrapper name instead.
 */
#if LINUX_VERSION_CODE < KERNEL_VERSION(6, 4, 0)
#define ua_class_create(name)	class_create(THIS_MODULE, name)
#else
#define ua_class_create(name)	class_create(name)
#endif

/* PCI_IRQ_LEGACY renamed to PCI_IRQ_INTX in 6.8+ */
#ifndef PCI_IRQ_LEGACY
#define PCI_IRQ_LEGACY PCI_IRQ_INTX
#endif

/* Up to 8 devices */
#define UA_MAX_DEVICES	8

static dev_t ua_devno;
static struct class *ua_class;
static DEFINE_IDA(ua_ida);

/*
 * Lifecycle module parameters (kext-vs-driver audit D1/D4/D5/D6).
 * All default to the conservative behaviour; the "on" settings follow
 * UAD2System.kext 12.0.0 more closely but need hardware validation.
 */
static bool full_shutdown;
module_param(full_shutdown, bool, 0444);
MODULE_PARM_DESC(full_shutdown,
		 "On remove/shutdown/suspend also run ResetDMAEngines, clear the ACEFACE magic and ring the disconnect doorbell, as the macOS driver does; the next probe then cold-boots the DSP (default: off, keep a live mixer DSP)");

static bool warm_dma_reset;
module_param(warm_dma_reset, bool, 0444);
MODULE_PARM_DESC(warm_dma_reset,
		 "With a live mixer DSP (warm probe/resume), also write TRANSPORT=0 and reset the DMA engines with the macOS driver's 4/8-engine masks before reprogramming the SG table (default: off; an earlier 8-engine reset killed a running mixer DSP)");

static bool v12_connect;
module_param(v12_connect, bool, 0444);
MODULE_PARM_DESC(v12_connect,
		 "EXPERIMENTAL, needs Solo/Arrow/x8p testers: use the v12 ring-connect device set (adds Solo TB, Arrow, type 0x29) and do the ACEFACE handshake before the ring connect (default: off)");

/* ----------------------------------------------------------------
 * PCI device table
 * ---------------------------------------------------------------- */

static const struct pci_device_id ua_pci_ids[] = {
	{ PCI_DEVICE(PCI_VENDOR_ID_UA, PCI_DEVICE_ID_UA_UAD2) },
	{ PCI_DEVICE(PCI_VENDOR_ID_UA, PCI_DEVICE_ID_UA_APOLLO) },
	{ 0 }
};
MODULE_DEVICE_TABLE(pci, ua_pci_ids);

/* ----------------------------------------------------------------
 * Interrupt handler
 * ---------------------------------------------------------------- */

static unsigned int ua_irq_count;

static irqreturn_t ua_irq_handler(int irq, void *data)
{
	struct ua_device *ua = data;
	u32 status_lo, status_hi = 0;
	unsigned long flags;

	/* Device removed — don't touch BAR0 */
	if (atomic_read(&ua->shutdown))
		return IRQ_NONE;

	spin_lock_irqsave(&ua->irq_lock, flags);

	/*
	 * Read ISR and mask with IMR — only process enabled vectors.
	 *
	 * The kext does: pending = ReadReg(ISR) & enableMask
	 * It never touches unmasked bits.  Acking unmasked DSP noise
	 * bits (0x18c63) causes them to immediately re-set, generating
	 * an MSI storm that overwhelms the Thunderbolt link.
	 *
	 * Only ack the specific bits we're handling.
	 */
	status_lo = ua_read(ua, UA_REG_IRQ_STATUS) & ua->irq_mask_lo;

	if (ua->fw_v2)
		status_hi = ua_read(ua, UA_REG_EXT_IRQ_STATUS) &
			    ua->irq_mask_hi;

	if (!status_lo && !status_hi) {
		spin_unlock_irqrestore(&ua->irq_lock, flags);
		return IRQ_NONE;
	}

	/* Ack ONLY the bits we're handling — one at a time per kext pattern */
	if (status_lo)
		ua_write(ua, UA_REG_IRQ_STATUS, status_lo);
	if (status_hi)
		ua_write(ua, UA_REG_EXT_IRQ_STATUS, status_hi);

	spin_unlock_irqrestore(&ua->irq_lock, flags);

	/* Log first 20 IRQs for debugging */
	if (ua_irq_count < 20) {
		dev_info(&ua->pdev->dev,
			 "IRQ #%u: lo=0x%08x hi=0x%08x\n",
			 ua_irq_count, status_lo, status_hi);
	}
	ua_irq_count++;

	/* Dispatch audio-related interrupts */
	ua_audio_irq(ua, status_lo, status_hi);

	return IRQ_HANDLED;
}

/* Forward declarations */
static void ua_read_serial_type(struct ua_device *ua);
static void ua_resume_work(struct work_struct *work);

/* ----------------------------------------------------------------
 * Hardware initialisation helpers
 * ---------------------------------------------------------------- */

/*
 * Pulse the per-engine DMA reset strobes, as CPcieIntrManager::
 * ResetDMAEngines() (kext 12.0.0 x86 0x99f8) does: clear the engine
 * enables, write enables-cleared | strobes (assert), then the cleared
 * value (deassert), read back as a flush.
 *   @eight:  8-engine masks (enables [8:1], strobes [16:9]) rather than
 *            the 4-engine ones (enables [4:1], strobes [12:9]);
 *   @global: force bit 0 (global DMA enable) on in the result.
 * Starts from the live register: the cache is stale at probe/resume.
 */
static void ua_dma_reset_pulse(struct ua_device *ua, bool eight, bool global)
{
	u32 strobes = eight ? UA_DMA_RESET_V2_MASK : UA_DMA_RESET_V1_MASK;
	u32 clean = ua_read(ua, UA_REG_DMA_CTRL) &
		    ~(eight ? UA_DMA_RESET_V2_CLR : UA_DMA_RESET_V1_CLR);

	if (global)
		clean |= 0x1;

	ua_write(ua, UA_REG_DMA_CTRL, clean | strobes);	/* Assert reset */
	ua_write(ua, UA_REG_DMA_CTRL, clean);		/* Deassert reset */
	(void)ua_read(ua, UA_REG_DMA_CTRL);		/* Flush */
	ua->dma_ctrl_cache = clean;
}

/*
 * Reset DMA engines (the driver's long-standing variant, cold path and
 * UA_IOCTL_RESET_DMA): 8-engine masks on every v2 device, global enable
 * forced on, then all pending interrupts cleared.
 */
void ua_reset_dma(struct ua_device *ua)
{
	ua_dma_reset_pulse(ua, ua->fw_v2, true);

	/* Clear all pending interrupts */
	ua_write(ua, UA_REG_IRQ_STATUS, UA_IRQ_CLEAR_ALL);

	if (ua->fw_v2)
		ua_write(ua, UA_REG_EXT_IRQ_STATUS, UA_IRQ_CLEAR_ALL);
}

/*
 * Detect firmware version, device type, and DSP count.
 *
 * Mirrors CPcieDevice::Name() and GetCapabilities():
 *   - FPGA rev (0x2218) bit 31 clear → v1 firmware, type from bits[31:28]-1
 *   - FPGA rev bit 31 set → v2 firmware, read ext_caps (0x2234):
 *       bits[25:20] - 1 = device family index
 *       bits[15:8]      = DSP count
 *   - Some v2 devices need serial prefix (0x20-0x2C) to get exact type
 */
static void ua_detect_capabilities(struct ua_device *ua)
{
	u32 fpga = ua->fpga_rev;
	u32 ext_caps;

	if ((s32)fpga >= 0) {
		/* v1 firmware: type from FPGA rev bits[31:28] - 1 */
		ua->fw_v2 = false;
		ua->device_type = (fpga >> 28) - 1;

		/* v1 DSP count from subsystem ID */
		switch (ua->subsystem_id) {
		case UA_SUBSYS_APOLLO_X4_QUAD:
			ua->num_dsps = 4;
			break;
		default:
			ua->num_dsps = 1;
			break;
		}
	} else {
		/* v2 firmware */
		ua->fw_v2 = true;
		ext_caps = ua_read(ua, UA_REG_EXT_CAPS);
		ua->num_dsps = (ext_caps >> 8) & 0xFF;

		/*
		 * Device type detection for v2 firmware:
		 * 1. PCI subsystem ID for the models the serial table cannot
		 *    resolve: the Apollo 8p (an ext_caps family, no serial
		 *    entry) and the Solo (serial "2252 2032 ..." would read
		 *    as 2032 -> 0x2A).
		 * 2. Otherwise the serial prefix table, as the kext does.
		 *
		 * The x8p used to be pinned by subsystem 0x0014 because the
		 * table here named 0x22 an x8.  The kext names 0x22 the x8p,
		 * so the observed unit ("2008 2017 ...") now resolves by
		 * serial; 0x0014 is only a fallback when the serial lookup
		 * fails, since nothing shows the subsystem ID is unique to
		 * the x8p among the HEXA units.
		 */
		switch (ua->subsystem_id) {
		case UA_SUBSYS_APOLLO_8P:
			ua->device_type = UA_DEV_APOLLO_8P;
			break;
		case UA_SUBSYS_APOLLO_SOLO:
			ua->device_type = UA_DEV_APOLLO_SOLO;
			break;
		default:
			ua_read_serial_type(ua);
			if (!ua->device_type &&
			    ua->subsystem_id == UA_SUBSYS_APOLLO_X8P) {
				dev_info(&ua->pdev->dev,
					 "serial unresolved, subsystem 0x%04x -> Apollo x8p\n",
					 ua->subsystem_id);
				ua->device_type = UA_DEV_APOLLO_X8P;
			}
			break;
		}
	}

	if (ua->num_dsps > UA_MAX_DSPS)
		ua->num_dsps = UA_MAX_DSPS;
}

/*
 * Serial prefix → device type lookup table.
 *
 * UAD2System.kext 12.0.0 _deviceTypeFromSerialNumber(): 22 entries of
 * {char[4] prefix, u32 pad, u32 type} at __TEXT,__const 0x3f800 (x86_64
 * slice), kept in the kext's order.  The type numbers match the older
 * v11 table; v12 adds 2107-2110.  Model names: ua_device_name().
 */
struct ua_serial_entry {
	char prefix[5]; /* 4-char ASCII serial prefix + NUL */
	u32 device_type;
};

static const struct ua_serial_entry ua_serial_table[] = {
	{ "2005", UA_DEV_APOLLO_X4 },
	{ "2019", UA_DEV_APOLLO_X6 },
	{ "2018", UA_DEV_APOLLO_X8 },
	{ "2017", UA_DEV_APOLLO_X8P },
	{ "2020", UA_DEV_APOLLO_X16 },
	{ "2024", UA_DEV_APOLLO_SOLO },
	{ "2025", UA_DEV_ARROW },
	{ "2073", UA_DEV_APOLLO_X16D },
	{ "2082", UA_DEV_APOLLO_X16D },
	{ "2016", UA_DEV_SERIAL_2016 },
	{ "2032", UA_DEV_SERIAL_2032 },
	{ "2092", UA_DEV_APOLLO_X4_GEN2 },
	{ "2086", UA_DEV_APOLLO_X6_GEN2 },
	{ "2087", UA_DEV_APOLLO_X8_GEN2 },
	{ "2088", UA_DEV_APOLLO_X8P_GEN2 },
	{ "2089", UA_DEV_APOLLO_X16_GEN2 },
	{ "2090", UA_DEV_APOLLO_TWIN_X_GEN2 },
	{ "2091", UA_DEV_APOLLO_TWIN_X_GEN2_B },
	{ "2107", UA_DEV_APOLLO_TWIN_X_GEN2_TB4 },
	{ "2108", UA_DEV_APOLLO_TWIN_X_GEN2_TB4_B },
	{ "2109", UA_DEV_APOLLO_X4_GEN2_TB4 },
	{ "2110", UA_DEV_SERIAL_2110 },
};

/*
 * Read serial number from hardware and look up device type.
 * The kext reads BAR0 + 0x20 through 0x2C (4 dwords = 16 bytes) and matches
 * the 4-byte ASCII model prefix at offset +4 (digits 5-8) against the table —
 * not the leading four digits.
 */
static void ua_read_serial_type(struct ua_device *ua)
{
	char serial[UA_REG_SERIAL_LEN + 1];
	u32 regs[4];
	int i;

	for (i = 0; i < 4; i++)
		regs[i] = ua_read(ua, UA_REG_SERIAL_BASE + i * 4);

	memcpy(serial, regs, UA_REG_SERIAL_LEN);
	serial[UA_REG_SERIAL_LEN] = '\0';

	dev_info(&ua->pdev->dev, "serial: %.16s\n", serial);

	/* Match the 4-char model prefix at digits 5-8 against the lookup table */
	for (i = 0; i < ARRAY_SIZE(ua_serial_table); i++) {
		if (!strncmp(serial + 4, ua_serial_table[i].prefix, 4)) {
			ua->device_type = ua_serial_table[i].device_type;
			return;
		}
	}

	dev_warn(&ua->pdev->dev, "unknown serial prefix, type unresolved\n");
	ua->device_type = 0;
}

/*
 * Model name for a device type, following CPcieDevice::Name() (kext
 * 12.0.0).  Names of the models with generated data come from
 * devices/<model>.json via ua_models_gen.h.
 */
const char *ua_device_name(u32 device_type)
{
	const char *name;

	if (ua_model_lookup(device_type, &name))
		return name;

	switch (device_type) {
	case UA_DEV_SERIAL_2016:
	case UA_DEV_SERIAL_2032:
	case UA_DEV_SERIAL_2110:
		/*
		 * In the HEXA family Name() calls these an x16D only when
		 * _deviceTypeFromFPGAVersion() returns 0x35; that check is
		 * not ported, so they stay unidentified.
		 */
		return "Apollo (unidentified HEXA)";
	default:
		return "Unknown UA Device";
	}
}

/*
 * Verify DSP banks are accessible by reading their base registers.
 * Mirrors CPcieDevice::_checkDSPs().
 */
static int ua_check_dsps(struct ua_device *ua)
{
	unsigned int i;

	for (i = 0; i < ua->num_dsps; i++) {
		u32 base = ua_dsp_base(i);
		u32 val = ua_read(ua, base);

		dev_dbg(&ua->pdev->dev, "DSP %u: bank base 0x%04x, reg[0] = 0x%08x\n",
			i, base, val);
	}

	return 0;
}

/*
 * Program registers — mirrors CPcieDevice::ProgramRegisters():
 *   1. Verify FPGA revision matches cached value
 *   2. Reset DMA engines
 *   3. Clear interrupts
 */
static int ua_program_registers(struct ua_device *ua)
{
	u32 fpga_rev;

	fpga_rev = ua_read(ua, UA_REG_FPGA_REV);
	if (fpga_rev != ua->fpga_rev) {
		dev_err(&ua->pdev->dev,
			"FPGA revision mismatch: cached 0x%08x, read 0x%08x\n",
			ua->fpga_rev, fpga_rev);
		return -EIO;
	}

	/* Stop transport */
	ua_write(ua, UA_REG_TRANSPORT, 0);

	/* Reset DMA and clear interrupts */
	ua_reset_dma(ua);

	/* Verify DSP accessibility */
	ua_check_dsps(ua);

	return 0;
}

/*
 * ua_reset_dma_engines - CPcieIntrManager::ResetDMAEngines() as the kext
 * does it (12.0.0 x86 0x99f8).  Differs from ua_reset_dma():
 *   - 8-engine masks (enables [8:1], strobes [16:9]) only when the DSP
 *     count is > 4 (MapHardware 0x97da: ext_caps[15:8] >= 5 on v2
 *     firmware); otherwise the 4-engine masks (enables [4:1], strobes
 *     [12:9]) — the x4 is a 4-engine part to the kext;
 *   - bit 0 is not forced on, and no ISR write (CPcieDevice::
 *     ProgramRegisters writes ISR = ~0 itself afterwards).
 * Used only with full_shutdown=1 / warm_dma_reset=1.
 */
static void ua_reset_dma_engines(struct ua_device *ua)
{
	ua_dma_reset_pulse(ua, ua->fw_v2 && ua->num_dsps > 4, false);
}

/*
 * ua_hw_stop_xport_irqs - the DSP-safe front half of the kext's
 * ProgramRegisters: stop the audio transport and mask + clear every
 * interrupt, before bus mastering is (re)enabled and before the SG table
 * is rewritten.
 *
 *   CPcieAudioExtension::ProgramRegisters (x86 0x33f4a): AX_CONTROL = 0,
 *     read it back as a flush (logs if the previous owner left it set);
 *   CPcieIntrManager::ProgramRegisters (0x996a): IMR = 0, IMR_HI = 0;
 *   CPcieDevice::ProgramRegisters (0x5d66): ISR = 0xFFFFFFFF.
 *
 * Safe with a live mixer DSP: AX_CONTROL = 0 is what pcm_close does on
 * every stream stop.  TRANSPORT (0x2220) = 0 and ResetDMAEngines are left
 * to ua_program_registers() (cold) and warm_dma_reset=1 (warm).
 */
static void ua_hw_stop_xport_irqs(struct ua_device *ua)
{
	u32 ax;

	ax = ua_read(ua, UA_REG_AX_CONTROL);
	if (ax && ax != 0xFFFFFFFF)
		dev_info(&ua->pdev->dev,
			 "previous session left the transport running (AX_CONTROL=0x%x), stopping it\n",
			 ax);
	ua_write(ua, UA_REG_AX_CONTROL, 0);
	(void)ua_read(ua, UA_REG_AX_CONTROL);
	ua->audio.transport_running = false;

	ua_irq_mask_all(ua, true);
}

/* ----------------------------------------------------------------
 * Character device file operations
 * ---------------------------------------------------------------- */

static int ua_open(struct inode *inode, struct file *filp)
{
	struct ua_device *ua = container_of(inode->i_cdev, struct ua_device, cdev);

	filp->private_data = ua;
	return 0;
}

static int ua_release(struct inode *inode, struct file *filp)
{
	return 0;
}

static long ua_ioctl_get_device_info(struct ua_device *ua, unsigned long arg)
{
	struct ua_device_info info = {};

	info.fpga_rev = ua->fpga_rev;
	info.device_type = ua->device_type;
	info.subsystem_id = ua->subsystem_id;
	info.num_dsps = ua->num_dsps;
	info.fw_version = ua_read(ua, UA_REG_FW_VERSION);
	info.fw_v2 = ua->fw_v2 ? 1 : 0;
	info.serial = ua_read(ua, UA_REG_SERIAL);
	info.ext_caps = ua_read(ua, UA_REG_EXT_CAPS);

	if (copy_to_user((void __user *)arg, &info, sizeof(info)))
		return -EFAULT;

	return 0;
}

static long ua_ioctl_get_dsp_info(struct ua_device *ua, unsigned long arg)
{
	struct ua_dsp_info info;

	if (copy_from_user(&info, (void __user *)arg, sizeof(info)))
		return -EFAULT;

	if (info.index >= ua->num_dsps)
		return -EINVAL;

	info.bank_base = ua_dsp_base(info.index);
	info.status = ua_read(ua, info.bank_base) ? 1 : 0;

	if (copy_to_user((void __user *)arg, &info, sizeof(info)))
		return -EFAULT;

	return 0;
}

static long ua_ioctl_read_reg(struct ua_device *ua, unsigned long arg)
{
	struct ua_reg_io rio;

	if (copy_from_user(&rio, (void __user *)arg, sizeof(rio)))
		return -EFAULT;

	/* Bounds check: must be within mapped BAR0 */
	if (rio.offset >= ua->regs_size)
		return -EINVAL;

	/* Must be 4-byte aligned */
	if (rio.offset & 3)
		return -EINVAL;

	rio.value = ua_read(ua, rio.offset);

	if (copy_to_user((void __user *)arg, &rio, sizeof(rio)))
		return -EFAULT;

	return 0;
}

static long ua_ioctl_get_transport_info(struct ua_device *ua, unsigned long arg)
{
	struct ua_transport_info info = {};
	struct ua_audio *audio = &ua->audio;

	info.play_channels = audio->play_channels;
	info.play_stride = audio->play_channels * UA_SAMPLE_BYTES;
	info.rec_channels = audio->rec_channels;
	info.rec_stride = audio->rec_channels * UA_SAMPLE_BYTES;
	info.buf_frame_size = audio->buf_frame_size;
	info.sample_rate = audio->sample_rate;
	info.play_latency = audio->buf_frame_size;
	info.rec_latency = audio->buf_frame_size;
	info.connected = audio->connected ? 1 : 0;
	info.transport_running = audio->transport_running ? 1 : 0;

	if (copy_to_user((void __user *)arg, &info, sizeof(info)))
		return -EFAULT;

	return 0;
}

static long ua_ioctl_write_reg(struct ua_device *ua, unsigned long arg)
{
	struct ua_reg_io rio;

	if (copy_from_user(&rio, (void __user *)arg, sizeof(rio)))
		return -EFAULT;

	if (rio.offset >= ua->regs_size)
		return -EINVAL;

	if (rio.offset & 3)
		return -EINVAL;

	/*
	 * Safety: don't allow writes to the interrupt/DMA control registers
	 * from userspace without the device lock held.
	 */
	mutex_lock(&ua->lock);
	ua_write(ua, rio.offset, rio.value);
	mutex_unlock(&ua->lock);

	return 0;
}

/* ----------------------------------------------------------------
 * CLI register interface helpers
 *
 * The CLI provides a command/response interface to the ARM
 * microcontroller that manages preamp gain, 48V phantom, pad, etc.
 * Registers at BAR0 + 0xC3F4..0xC4FF.
 * ---------------------------------------------------------------- */

static int ua_cli_enable_locked(struct ua_device *ua)
{
	if (ua->cli_enabled)
		return 0;

	ua_write(ua, UA_REG_CLI_ENABLE, 1);
	usleep_range(900, 1100);
	ua->cli_enabled = true;

	dev_dbg(&ua->pdev->dev, "CLI interface enabled\n");
	return 0;
}

/**
 * ua_cli_send_locked - Send CLI command (kernel-internal helper)
 *
 * Must be called with ua->lock held.
 */
static int ua_cli_send_locked(struct ua_device *ua, const u8 *cmd_data,
			      u32 cmd_len, u8 *resp_data, u32 *resp_len)
{
	u32 status, word;
	unsigned int i, words, polls;

	/*
	 * CLI commands freeze the ARM MCU on Apollo x4.
	 * All preamp control goes through DSP mixer settings (ioctl path).
	 * Block ALL CLI access unconditionally.
	 */
	dev_dbg(&ua->pdev->dev, "CLI BLOCKED (would freeze ARM MCU)\n");
	return -ENOSYS;

	if (cmd_len == 0 || cmd_len > UA_CLI_CMD_BUF_SIZE)
		return -EINVAL;

	/* Write command data to CLI_CMD_BUF (32-bit MMIO writes) */
	words = (cmd_len + 3) / 4;
	for (i = 0; i < words; i++) {
		word = 0;
		memcpy(&word, &cmd_data[i * 4],
		       min_t(u32, 4, cmd_len - i * 4));
		ua_write(ua, UA_REG_CLI_CMD_BUF + i * 4, word);
	}

	/* Trigger command by writing length to CLI_STATUS */
	ua_write(ua, UA_REG_CLI_STATUS, cmd_len);

	/* Poll for completion: status == 0 or bit 31 set */
	for (polls = 0; polls < UA_CLI_TIMEOUT_POLLS; polls++) {
		status = ua_read(ua, UA_REG_CLI_STATUS);
		if (status == 0 || (status & BIT(31)))
			goto read_response;
		usleep_range(900, 1100);
	}

	dev_err(&ua->pdev->dev,
		"CLI command timeout after %u polls (status=0x%08x)\n",
		polls, status);
	return -ETIMEDOUT;

read_response:
	if (resp_data && resp_len) {
		*resp_len = ua_read(ua, UA_REG_CLI_RESP_LEN);
		if (*resp_len > UA_CLI_RESP_BUF_SIZE)
			*resp_len = UA_CLI_RESP_BUF_SIZE;

		words = (*resp_len + 3) / 4;
		for (i = 0; i < words; i++) {
			word = ua_read(ua, UA_REG_CLI_RESP_BUF + i * 4);
			memcpy(&resp_data[i * 4], &word, sizeof(word));
		}
	}

	return 0;
}

/* ----------------------------------------------------------------
 * Mixer batch write protocol
 *
 * The DSP reads ALL 38 settings atomically on each SEQ_WR advance.
 * If we bump SEQ_WR after writing only 1 setting, the DSP reads 37
 * uninitialized registers and crashes.
 *
 * Protocol (from CPcieDeviceMixer::_flushCachedSettings disassembly):
 *   1. Maintain cached val/mask arrays for all 38 settings
 *   2. On setting change: merge into cache, mark dirty
 *   3. On service tick: if dirty AND SEQ_RD == cached SEQ_WR,
 *      write ALL 38 settings to SRAM, then bump SEQ_WR once
 *   4. DSP reads all 38 atomically per SEQ_WR advance
 *
 * The mask merge is critical — setting[2] has monitor volume ([7:0]),
 * mute ([16]), mono ([17]), and dim ([31]) sharing one register.
 * Each caller provides a partial mask; the cache accumulates all
 * active fields.
 * ---------------------------------------------------------------- */

/**
 * ua_dump_mixer_sram - Log all 38 settings from BAR0 SRAM
 * @ua: device
 * @label: context label for the log message
 *
 * Reads both words of each setting directly from SRAM and logs them.
 * Used to compare firmware-initialized state vs post-plugin-chain state.
 */
void ua_dump_mixer_sram(struct ua_device *ua, const char *label)
{
	u32 reg, wa, wb, seq_w, seq_r;
	int i;

	seq_w = ua_read(ua, UA_REG_MIXER_SEQ_WR);
	seq_r = ua_read(ua, UA_REG_MIXER_SEQ_RD);
	dev_info(&ua->pdev->dev,
		 "SRAM dump [%s]: SEQ WR=%u RD=%u\n", label, seq_w, seq_r);

	for (i = 0; i < UA_MIXER_NUM_SETTINGS; i++) {
		reg = ua_mixer_setting_reg(i);
		wa = ua_read(ua, reg);
		wb = ua_read(ua, reg + 4);
		if (wa || wb)
			dev_info(&ua->pdev->dev,
				 "  setting[%2d] @ 0x%04x: A=0x%08x B=0x%08x\n",
				 i, reg, wa, wb);
	}
}

int ua_mixer_write_setting_locked(struct ua_device *ua,
				  unsigned int index,
				  u32 value, u32 mask)
{
	if (index >= UA_MIXER_BATCH_COUNT) {
		dev_warn(&ua->pdev->dev,
			 "mixer_write: index %u >= %d\n",
			 index, UA_MIXER_BATCH_COUNT);
		return -EINVAL;
	}
	if (!ua->mixer_ready) {
		dev_warn(&ua->pdev->dev,
			 "mixer_write: not ready (index=%u)\n", index);
		return -EAGAIN;
	}

	/* Merge into cache (read-modify-write with mask) */
	ua->mixer_val[index] = (ua->mixer_val[index] & ~mask) | (value & mask);
	ua->mixer_mask[index] |= mask;
	ua->mixer_dirty = true;
	dev_dbg(&ua->pdev->dev,
		"mixer_write: setting[%u] val=0x%08x mask=0x%08x\n",
		index, ua->mixer_val[index], ua->mixer_mask[index]);
	return 0;
}

/**
 * ua_mixer_flush_settings - Write all 38 settings + single SEQ bump
 * @ua: device (caller must hold ua->lock)
 *
 * Called from the DSP service loop at 10 Hz.  Matches the kext's
 * _flushCachedSettings which is called from GetReadback at ~33Hz.
 *
 * Windows BAR0 capture (2026-03-20) proves: val AND mask persist
 * across flushes. The DSP reads val/mask on every SEQ bump.
 * Do NOT clear mask after writing — gain requires persistent values.
 */
void ua_mixer_flush_settings(struct ua_device *ua)
{
	u32 seq_rd, word_a, word_b, reg;
	int i;

	if (!ua->mixer_ready || !ua->mixer_dirty)
		return;

	/* Check DSP idle: SEQ_RD must match our cached SEQ_WR */
	seq_rd = ua_read(ua, UA_REG_MIXER_SEQ_RD);
	if (seq_rd != ua->mixer_seq_wr)
		return;  /* DSP still processing previous batch */

	/*
	 * Write ALL 38 settings to SRAM registers.
	 * Word A = (mask[15:0] << 16) | val[15:0]
	 * Word B = (mask[31:16] << 16) | val[31:16]
	 *
	 * Windows BAR0 capture (2026-03-20) proves: val AND mask persist
	 * across flushes. The DSP reads val/mask on every SEQ bump.
	 * Do NOT clear mask after writing — gain requires persistent values.
	 *
	 * Word order tested (2026-03-21): swapping words for settings 0-31
	 * does NOT fix monitor. Single-setting[2] write with SEQ bump also
	 * fails. Issue is DSP module activation, not word encoding.
	 */
	for (i = 0; i < UA_MIXER_BATCH_COUNT; i++) {
		/*
		 * Skip settings with no mask — preserves firmware defaults.
		 * Kext _writeSetting checks dirty_mask and skips clean
		 * settings (cbz at 0x4a63c). Firmware initializes settings
		 * with default val/mask on boot; writing zeros here would
		 * clear those defaults, breaking DSP module configuration
		 * (e.g., monitor processing depends on preset defaults).
		 */
		if (!ua->mixer_mask[i])
			continue;
		reg = ua_mixer_setting_reg(i);
		word_a = ((ua->mixer_mask[i] & 0xFFFF) << 16) |
			 (ua->mixer_val[i] & 0xFFFF);
		word_b = (((ua->mixer_mask[i] >> 16) & 0xFFFF) << 16) |
			 ((ua->mixer_val[i] >> 16) & 0xFFFF);
		ua_write(ua, reg, word_a);
		ua_write(ua, reg + 4, word_b);
	}

	/* Single SEQ_WR bump for entire batch */
	ua->mixer_seq_wr++;
	ua_write(ua, UA_REG_MIXER_SEQ_WR, ua->mixer_seq_wr);
	ua->mixer_dirty = false;

	dev_dbg(&ua->pdev->dev,
		"mixer_flush: SEQ_WR=%u (SEQ_RD was %u)\n",
		ua->mixer_seq_wr, seq_rd);
}

static int ua_mixer_read_setting_locked(struct ua_device *ua,
					unsigned int index,
					u32 *value, u32 *mask)
{
	u32 reg, word_a, word_b;

	if (index >= UA_MIXER_NUM_SETTINGS)
		return -EINVAL;

	reg = ua_mixer_setting_reg(index);
	word_a = ua_read(ua, reg);
	word_b = ua_read(ua, reg + 4);

	/* Settings 0-31 store data in word B, settings 32+ in word A */
	{
		u32 packed = (index < 32) ? word_b : word_a;
		*value = packed & 0xFFFF;
		*mask = (packed >> 16) & 0xFFFF;
	}
	return 0;
}

/*
 * Device types with an output mute: CPcieDevice::SetOutputMute (kext
 * 12.0.0 x86 0x6140) acts only for type <= 0x27 with a bit set in
 * 0x8010080000, i.e. 0x13, 0x1C and 0x27 (Apollo Solo TB).  A no-op on
 * every other model, the x4 included.
 */
#define UA_OUTPUT_MUTE_TYPES	0x8010080000ULL
#define UA_OUTPUT_MUTE_MAX_TYPE	0x27

/**
 * ua_set_output_mute_locked - SetOutputMute(mute) as the kext does it
 * @ua: device (caller holds ua->lock)
 * @mute: true = mute (shutdown/suspend), false = unmute (connect)
 * @flush_now: push the batch now instead of on the next service tick
 *
 * The kext calls CUAD2DeviceMixer::SetOtherParam(0x20, mute) — setting[7]
 * bit 10 here — and sleeps 50 ms after muting so the outputs settle
 * before the transport stops.
 */
void ua_set_output_mute_locked(struct ua_device *ua, bool mute,
			       bool flush_now)
{
	unsigned int polls;
	u32 seq_rd;

	if (ua->device_type > UA_OUTPUT_MUTE_MAX_TYPE ||
	    !(UA_OUTPUT_MUTE_TYPES & BIT_ULL(ua->device_type)))
		return;
	if (!ua->mixer_ready)
		return;
	if (ua_monitor_set_param(ua, 0, UA_MON_PARAM_OUTPUT_MUTE, mute))
		return;
	if (!flush_now)
		return;

	/* The batch only goes out when the DSP has taken the last one */
	for (polls = 0; polls < UA_MIXER_SEQ_TIMEOUT; polls++) {
		seq_rd = ua_read(ua, UA_REG_MIXER_SEQ_RD);
		if (seq_rd == ua->mixer_seq_wr || seq_rd == 0xFFFFFFFF)
			break;
		usleep_range(1000, 1200);
	}
	ua_mixer_flush_settings(ua);

	dev_info(&ua->pdev->dev, "output %s\n", mute ? "muted" : "unmuted");
	if (mute)
		msleep(50);
}

/**
 * ua_hw_quiesce_locked - stop all device DMA and interrupts
 * @ua: device (caller holds ua->lock and has stopped the DSP service loop
 *      and the period timer; the device must be present)
 *
 * CPcieDevice::Shutdown (kext 12.0.0 x86 0x62b6) followed by
 * CPcieIntrManager::DisableInterrupts, which stop() and _stopPowerGated()
 * both run:
 *   1. connected = 0
 *   2. SetOutputMute(1)
 *   3. StopTransport: DisableVector(0x47), AX_CONTROL = 0
 *   3b. DSP rings: drain briefly, then DOORBELL = WRITE_PTR = POSITION
 *       (ua_dsp_rings_quiesce(); the kext gets there by ResetDMAEngines
 *       and HardResetDSPs, which gentle init cannot use)
 *   4. ResetDMAEngines                                    [full_shutdown]
 *   5. AudioExtension::Shutdown (0x34cb8): AX_CONTROL = 0,
 *      NOTIF_ACEFACE = 0 [full_shutdown], read AX_CONTROL (flush),
 *      0x22C4 (P2P enable) = 0, DisableVector(0x28)
 *   6. _disconnect (0x34dac): AX_CONNECT = 0x10, read FPGA_REV
 *   7. DisableInterrupts: IMR = 0, IMR_HI = 0 (and W1C anything pending)
 *
 * Without full_shutdown the steps that can kill a running mixer DSP (per
 * the 2026-02-16 finding) or force a cold boot are skipped,
 * so the next probe/resume can take the gentle warm path.  The caller
 * clears bus mastering afterwards; with the transport stopped, nothing
 * queued on any ring and BME off, the device cannot DMA into buffers
 * that are about to be freed, nor right after the next probe/resume
 * turns BME back on.
 */
static void ua_hw_quiesce_locked(struct ua_device *ua)
{
	struct ua_audio *audio = &ua->audio;

	lockdep_assert_held(&ua->lock);

	dev_info(&ua->pdev->dev, "quiescing device (full_shutdown=%d)\n",
		 full_shutdown);

	/* 2. SetOutputMute(1) — needs the DSP connection, so goes first */
	ua_set_output_mute_locked(ua, true, true);

	/* 3. StopTransport */
	ua_disable_vector(ua, UA_IRQ_VEC_END_BUFFER);
	ua_write(ua, UA_REG_AX_CONTROL, 0);
	audio->transport_running = false;

	/* 3b. Nothing left queued on the DSP rings */
	ua_dsp_rings_quiesce(ua);

	/* 4. ResetDMAEngines */
	if (full_shutdown)
		ua_reset_dma_engines(ua);

	/* 5. AudioExtension::Shutdown */
	ua_write(ua, UA_REG_AX_CONTROL, 0);
	if (full_shutdown)
		ua_write(ua, UA_REG_NOTIF_ACEFACE, 0);
	(void)ua_read(ua, UA_REG_AX_CONTROL);
	ua_write(ua, UA_REG_AX_P2P_ENABLE, 0);
	ua_disable_vector(ua, UA_IRQ_VEC_NOTIFICATION);

	/*
	 * 6. _disconnect.  Always: without the doorbell the audio extension
	 * stays connected to the host that just left and never acks the next
	 * ACEFACE (x4, 2026-10-09: rmmod + insmod could not connect).  The
	 * doorbell alone left the mixer DSP's SEQ counters untouched, so it
	 * does not cost the gentle warm path.
	 */
	ua_write(ua, UA_REG_AX_CONNECT, UA_AX_DISCONNECT);
	(void)ua_read(ua, UA_REG_FPGA_REV);

	/* 7. DisableInterrupts */
	ua_irq_mask_all(ua, true);

	/* 1. connected = 0 (state only, kept last so step 2 could flush) */
	audio->connected = false;
	ua->mixer_ready = false;
}

/* ----------------------------------------------------------------
 * Preamp / monitor hardware wrappers for ALSA mixer controls
 *
 * These are called from ua_audio.c ALSA kcontrol .put callbacks.
 * They take ua->lock internally.
 * ---------------------------------------------------------------- */

/**
 * ua_preamp_set_param - Send a preamp parameter to the ARM MCU via CLI
 * @ua: device
 * @ch: preamp channel index (0-based)
 * @param_id: ARM parameter ID (UA_ARM_PARAM_*)
 * @value: parameter value (0/1 for booleans, dB-related for gain)
 *
 * Builds the 36-byte CLI command with hw_id = ch * 0x800 and sends it.
 */
int ua_preamp_set_param(struct ua_device *ua, unsigned int ch,
			u32 param_id, u32 value)
{
	u32 cli_cmd[9];
	int ret;

	if (ch >= UA_MAX_PREAMP_CH)
		return -EINVAL;

	mutex_lock(&ua->lock);

	ret = ua_cli_enable_locked(ua);
	if (ret)
		goto out;

	memset(cli_cmd, 0, sizeof(cli_cmd));
	cli_cmd[0] = param_id;
	cli_cmd[1] = value;
	cli_cmd[5] = UA_ARM_HW_ID(ch);
	cli_cmd[8] = 8;

	ret = ua_cli_send_locked(ua, (u8 *)cli_cmd, 36, NULL, NULL);

out:
	mutex_unlock(&ua->lock);
	return ret;
}

/**
 * ua_monitor_set_param - Set a monitor parameter via DSP mixer settings
 * @ua: device
 * @ch_idx: monitor channel index (from IOKit captures)
 * @param_id: monitor parameter ID (UA_MON_PARAM_*)
 * @value: parameter value (raw IOKit encoding)
 *
 * Routes monitor params to the correct DSP mixer setting index with
 * per-param bitmasks.  Multiple params share single 32-bit setting words
 * at different bit positions -- each write uses a mask so it doesn't
 * clobber the other fields.
 *
 * Setting index and bit layout from kext SetOtherParam disassembly
 * (UAD2System.kext v11.8.1, arm64e).  See the complete mapping in
 * plans/reports/hardware-re-260321-1301-setotherparam-setting-map.md
 *
 * Must be called with ua->lock held.
 */
__attribute__((optimize("Os"))) int
ua_monitor_set_param(struct ua_device *ua, unsigned int ch_idx,
		     u32 param_id, u32 value)
{
	unsigned int setting_idx;
	u32 hw_val, hw_mask;

	switch (param_id) {

	/* ---- Setting[0]: Output pad flags ---- */

	case UA_MON_PARAM_OUT_PAD_A:	/* 0x19 -> bit28 */
		setting_idx = 0;
		hw_val  = value ? BIT(28) : 0;
		hw_mask = BIT(28);
		break;
	case UA_MON_PARAM_OUT_PAD_B:	/* 0x1a -> bit29 */
		setting_idx = 0;
		hw_val  = value ? BIT(29) : 0;
		hw_mask = BIT(29);
		break;
	case UA_MON_PARAM_OUT_PAD_C:	/* 0x1b -> bit30 */
		setting_idx = 0;
		hw_val  = value ? BIT(30) : 0;
		hw_mask = BIT(30);
		break;
	case UA_MON_PARAM_OUT_PAD_D:	/* 0x1c -> bit31 */
		setting_idx = 0;
		hw_val  = value ? BIT(31) : 0;
		hw_mask = BIT(31);
		break;

	/* ---- Setting[1]: SR convert + misc 3-bit fields ---- */

	case UA_MON_PARAM_UNKNOWN_13:	/* 0x13 -> bits[30:28] */
		setting_idx = 1;
		hw_val  = (value & 7) << 28;
		hw_mask = 0x70000000;
		break;
	case UA_MON_PARAM_UNKNOWN_14:	/* 0x14 -> bits[15:13] */
		setting_idx = 1;
		hw_val  = (value & 7) << 13;
		hw_mask = 0x0000E000;
		break;
	case UA_MON_PARAM_SR_CONVERT:	/* 0x1f -> bit31 */
		setting_idx = 1;
		hw_val  = value ? BIT(31) : 0;
		hw_mask = BIT(31);
		break;

	/* ---- Setting[2]: Monitor core ---- */

	case UA_MON_PARAM_LEVEL:	/* 0x01 -> bits[7:0] */
		setting_idx = 2;
		hw_val  = value & 0xFF;
		hw_mask = 0x000000FF;
		break;
	case UA_MON_PARAM_HP1_VOL:	/* 0x02 -> bits[15:8] */
		setting_idx = 2;
		hw_val  = (value & 0xFF) << 8;
		hw_mask = 0x0000FF00;
		break;
	case UA_MON_PARAM_MUTE:		/* 0x03 -> bits[17:16] */
		/*
		 * 3-state (VERIFIED 2026-02-20):
		 *   value=0: unmuted, stereo -> bits[17:16] = 00
		 *   value=1: mono (MixToMono) -> bit 17 set
		 *   value=2: muted            -> bit 16 set
		 */
		setting_idx = 2;
		if (value == UA_MON_MUTE_ON)       /* value == 2 */
			hw_val = 0x00010000;       /* bit 16 = mute */
		else if (value == 1)               /* MixToMono */
			hw_val = 0x00020000;       /* bit 17 = mono */
		else
			hw_val = 0;               /* unmuted, stereo */
		hw_mask = 0x00030000;
		break;
	case UA_MON_PARAM_SOURCE:	/* 0x04 -> bit29 + bits[19:18] */
		/*
		 * MonitorSrc encoding: bit29 = (v >> 2) & 1,
		 * bits[19:18] = v & 3.  For values 0-2 (MIX/CUE1/CUE2)
		 * bit29 is always 0.
		 */
		setting_idx = 2;
		hw_val  = ((value & 3) << 18) |
			  (((value >> 2) & 1) << 29);
		hw_mask = 0x200C0000;
		break;
	case UA_MON_PARAM_CUE1_MIX:	/* 0x05 -> bits[25:24] */
		setting_idx = 2;
		hw_val  = (value & 3) << 24;
		hw_mask = 0x03000000;
		break;
	case UA_MON_PARAM_CUE1_MONO:	/* 0x06 -> bits[21:20] */
		setting_idx = 2;
		hw_val  = (value & 3) << 20;
		hw_mask = 0x00300000;
		break;
	case UA_MON_PARAM_CUE2_MIX:	/* 0x07 -> bits[27:26] */
		setting_idx = 2;
		hw_val  = (value & 3) << 26;
		hw_mask = 0x0C000000;
		break;
	case UA_MON_PARAM_CUE2_MONO:	/* 0x08 -> bits[23:22] */
		setting_idx = 2;
		hw_val  = (value & 3) << 22;
		hw_mask = 0x00C00000;
		break;
	case UA_MON_PARAM_UNKNOWN_15:	/* 0x15 -> bit28 */
		setting_idx = 2;
		hw_val  = value ? BIT(28) : 0;
		hw_mask = BIT(28);
		break;
	case UA_MON_PARAM_DIM:		/* 0x44 -> bit31 */
		setting_idx = 2;
		hw_val  = value ? BIT(31) : 0;
		hw_mask = BIT(31);
		break;
	case UA_MON_PARAM_MONO:		/* 0x36 -> setting[4] full word */
		/*
		 * Legacy kext property ID 0x36.  Only works for
		 * dev_type==3 (original Apollo).  On x4, mono is
		 * handled via param 0x03 value=1 (MixToMono).
		 * Keep for backward compat -- routes to setting[4].
		 */
		setting_idx = 4;
		hw_val  = value;
		hw_mask = 0xFFFFFFFF;
		break;

	/* ---- Setting[6]: Mirror A / Identify / MirrorsToDigital ---- */

	case UA_MON_PARAM_UNKNOWN_0A:	/* 0x0a -> bits[9:8] */
		setting_idx = 6;
		hw_val  = (value & 3) << 8;
		hw_mask = 0x00000300;
		break;
	case UA_MON_PARAM_IDENTIFY:	/* 0x1d -> bits[15:11] */
		setting_idx = 6;
		hw_val  = (value & 0x1F) << 11;
		hw_mask = 0x0000F800;
		break;
	case UA_MON_PARAM_DIGITAL_MIRROR: /* 0x1e -> bit10 */
		setting_idx = 6;
		hw_val  = value ? BIT(10) : 0;
		hw_mask = BIT(10);
		break;
	case UA_MON_PARAM_CUE1_MIRROR:	/* 0x2e -> bits[7:0] */
		setting_idx = 6;
		hw_val  = value & 0xFF;
		hw_mask = 0x000000FF;
		break;

	/* ---- Setting[7]: Mirror B / TBConfig / misc ---- */

	case UA_MON_PARAM_UNKNOWN_0F:	/* 0x0f -> bits[9:8] */
		setting_idx = 7;
		hw_val  = (value & 3) << 8;
		hw_mask = 0x00000300;
		break;
	case UA_MON_PARAM_UNKNOWN_20:	/* 0x20 -> bit10 */
		setting_idx = 7;
		hw_val  = value ? BIT(10) : 0;
		hw_mask = BIT(10);
		break;
	case UA_MON_PARAM_CUE2_MIRROR:	/* 0x2f -> bits[7:0] */
		setting_idx = 7;
		hw_val  = value & 0xFF;
		hw_mask = 0x000000FF;
		break;
	case UA_MON_PARAM_UNKNOWN_45:	/* 0x45 -> bit11 */
		setting_idx = 7;
		hw_val  = value ? BIT(11) : 0;
		hw_mask = BIT(11);
		break;
	case UA_MON_PARAM_TB_CONFIG:	/* 0x47 -> bit12 */
		setting_idx = 7;
		hw_val  = value ? BIT(12) : 0;
		hw_mask = BIT(12);
		break;
	case UA_MON_PARAM_UNKNOWN_48:	/* 0x48 -> bit13 */
		setting_idx = 7;
		hw_val  = value ? BIT(13) : 0;
		hw_mask = BIT(13);
		break;
	case UA_MON_PARAM_UNKNOWN_63:	/* 0x63 -> bit14 */
		setting_idx = 7;
		hw_val  = value ? BIT(14) : 0;
		hw_mask = BIT(14);
		break;

	/* ---- Setting[8]: Digital out mode / CUE alt ---- */

	case UA_MON_PARAM_DIGITAL_MODE:	/* 0x21 -> bits[7:0] */
		setting_idx = 8;
		hw_val  = value & 0xFF;
		hw_mask = 0x000000FF;
		break;
	case UA_MON_PARAM_UNKNOWN_22:	/* 0x22 -> bits[29:28] */
		setting_idx = 8;
		hw_val  = (value & 3) << 28;
		hw_mask = 0x30000000;
		break;
	case UA_MON_PARAM_CUE1_MIX_ALT:/* 0x23 -> bits[25:24] */
		setting_idx = 8;
		hw_val  = (value & 3) << 24;
		hw_mask = 0x03000000;
		break;
	case UA_MON_PARAM_UNKNOWN_24:	/* 0x24 -> bits[31:30] */
		setting_idx = 8;
		hw_val  = (value & 3) << 30;
		hw_mask = 0xC0000000;
		break;
	case UA_MON_PARAM_CUE2_MIX_ALT:/* 0x25 -> bits[27:26] */
		setting_idx = 8;
		hw_val  = (value & 3) << 26;
		hw_mask = 0x0C000000;
		break;

	/* ---- Setting[11]: Mirror config / OutputRef ---- */

	case UA_MON_PARAM_MIRROR_CFG_A:	/* 0x2a -> bit0 */
		setting_idx = 11;
		hw_val  = value ? BIT(0) : 0;
		hw_mask = BIT(0);
		break;
	case UA_MON_PARAM_MIRROR_CFG_B:	/* 0x2b -> bit1 */
		setting_idx = 11;
		hw_val  = value ? BIT(1) : 0;
		hw_mask = BIT(1);
		break;
	case UA_MON_PARAM_MIRROR_CFG_C:	/* 0x2c -> bit2 */
		setting_idx = 11;
		hw_val  = value ? BIT(2) : 0;
		hw_mask = BIT(2);
		break;
	case UA_MON_PARAM_MIRROR_CFG_D:	/* 0x2d -> bit3 */
		setting_idx = 11;
		hw_val  = value ? BIT(3) : 0;
		hw_mask = BIT(3);
		break;
	case UA_MON_PARAM_UNKNOWN_30:	/* 0x30 -> bits[23:16] */
		setting_idx = 11;
		hw_val  = (value & 0xFF) << 16;
		hw_mask = 0x00FF0000;
		break;
	case UA_MON_PARAM_UNKNOWN_31:	/* 0x31 -> bits[31:24] */
		setting_idx = 11;
		hw_val  = (value & 0xFF) << 24;
		hw_mask = 0xFF000000;
		break;
	case UA_MON_PARAM_OUTPUT_REF:	/* 0x32 -> bit4 */
		setting_idx = 11;
		hw_val  = value ? BIT(4) : 0;
		hw_mask = BIT(4);
		break;
	case UA_MON_PARAM_UNKNOWN_33:	/* 0x33 -> bit5 */
		setting_idx = 11;
		hw_val  = value ? BIT(5) : 0;
		hw_mask = BIT(5);
		break;
	case UA_MON_PARAM_UNKNOWN_34:	/* 0x34 -> bit6 */
		setting_idx = 11;
		hw_val  = value ? BIT(6) : 0;
		hw_mask = BIT(6);
		break;
	case UA_MON_PARAM_UNKNOWN_35:	/* 0x35 -> bit7 */
		setting_idx = 11;
		hw_val  = value ? BIT(7) : 0;
		hw_mask = BIT(7);
		break;

	/* ---- Setting[12]: HP cue source ---- */

	case UA_MON_PARAM_HP1_SOURCE:	/* 0x3f -> bits[26:24] */
		setting_idx = 12;
		hw_val  = (value & 7) << 24;
		hw_mask = 0x07000000;
		break;
	case UA_MON_PARAM_HP2_SOURCE:	/* 0x40 -> bits[29:27] */
		setting_idx = 12;
		hw_val  = (value & 7) << 27;
		hw_mask = 0x38000000;
		break;

	/* ---- Setting[13]: Talkback / misc ---- */

	case UA_MON_PARAM_UNKNOWN_3A:	/* 0x3a -> bits[29:28] */
		setting_idx = 13;
		hw_val  = (value & 3) << 28;
		hw_mask = 0x30000000;
		break;
	case UA_MON_PARAM_UNKNOWN_42:	/* 0x42 -> bits[15:13] */
		setting_idx = 13;
		hw_val  = (value & 7) << 13;
		hw_mask = 0x0000E000;
		break;
	case UA_MON_PARAM_TALKBACK:	/* 0x46 -> bit8 */
		setting_idx = 13;
		hw_val  = value ? BIT(8) : 0;
		hw_mask = BIT(8);
		break;

	/* ---- Setting[14]: Dim level / misc ---- */

	case UA_MON_PARAM_DIM_LEVEL:	/* 0x43 -> bits[30:28] */
		setting_idx = 14;
		hw_val  = (value & 7) << 28;
		hw_mask = 0x70000000;
		break;
	case UA_MON_PARAM_UNKNOWN_49:	/* 0x49 -> bit31 */
		setting_idx = 14;
		hw_val  = value ? BIT(31) : 0;
		hw_mask = BIT(31);
		break;
	case UA_MON_PARAM_UNKNOWN_66:	/* 0x66 -> bits[7:0] */
		setting_idx = 14;
		hw_val  = value & 0xFF;
		hw_mask = 0x000000FF;
		break;

	/* ---- Setting[15]: Mirror enables / clock-HP routing ---- */

	case UA_MON_PARAM_UNKNOWN_38:	/* 0x38 -> bits[6:0] */
		setting_idx = 15;
		hw_val  = value & 0x7F;
		hw_mask = 0x0000007F;
		break;
	case UA_MON_PARAM_UNKNOWN_39:	/* 0x39 -> bits[22:16] */
		setting_idx = 15;
		hw_val  = (value & 0x7F) << 16;
		hw_mask = 0x007F0000;
		break;
	case UA_MON_PARAM_MIRROR_ENABLE_A: /* 0x3b -> bit8 */
		setting_idx = 15;
		hw_val  = value ? BIT(8) : 0;
		hw_mask = BIT(8);
		break;
	case UA_MON_PARAM_MIRROR_ENABLE_B: /* 0x3c -> bit12 */
		setting_idx = 15;
		hw_val  = value ? BIT(12) : 0;
		hw_mask = BIT(12);
		break;
	case UA_MON_PARAM_UNKNOWN_3E:	/* 0x3e -> bit9 */
		setting_idx = 15;
		hw_val  = value ? BIT(9) : 0;
		hw_mask = BIT(9);
		break;
	case UA_MON_PARAM_UNKNOWN_41:	/* 0x41 -> bit7 */
		setting_idx = 15;
		hw_val  = value ? BIT(7) : 0;
		hw_mask = BIT(7);
		break;

	/* ---- Setting[5]: Sound/config flags (type >= 0xa) ---- */

	case UA_MON_PARAM_UNKNOWN_67:	/* 0x67 -> bit3 */
		setting_idx = 5;
		hw_val  = value ? BIT(3) : 0;
		hw_mask = BIT(3);
		break;
	case UA_MON_PARAM_UNKNOWN_68:	/* 0x68 -> bit4 */
		setting_idx = 5;
		hw_val  = value ? BIT(4) : 0;
		hw_mask = BIT(4);
		break;
	case UA_MON_PARAM_UNKNOWN_87:	/* 0x87 -> bit9 */
		setting_idx = 5;
		hw_val  = value ? BIT(9) : 0;
		hw_mask = BIT(9);
		break;

	/* ---- Setting[32] (0x20): HP/output config flags ---- */

	case 0x4a:			/* 0x4a -> bits[7:0] */
		setting_idx = 32;
		hw_val  = value & 0xFF;
		hw_mask = 0x000000FF;
		break;
	case 0x64:			/* 0x64 -> bits[15:8] */
		setting_idx = 32;
		hw_val  = (value & 0xFF) << 8;
		hw_mask = 0x0000FF00;
		break;
	case 0x5b:			/* 0x5b -> bit16 */
		setting_idx = 32;
		hw_val  = value ? BIT(16) : 0;
		hw_mask = BIT(16);
		break;
	case 0x5c:			/* 0x5c -> bit17 */
		setting_idx = 32;
		hw_val  = value ? BIT(17) : 0;
		hw_mask = BIT(17);
		break;
	case 0x53:			/* 0x53 -> bit24 */
		setting_idx = 32;
		hw_val  = value ? BIT(24) : 0;
		hw_mask = BIT(24);
		break;
	case 0x54:			/* 0x54 -> bit25 */
		setting_idx = 32;
		hw_val  = value ? BIT(25) : 0;
		hw_mask = BIT(25);
		break;

	/* ---- Setting[33] (0x21): HP output byte fields ---- */

	case 0x4b:			/* 0x4b -> bits[7:0] */
		setting_idx = 33;
		hw_val  = value & 0xFF;
		hw_mask = 0x000000FF;
		break;
	case 0x4c:			/* 0x4c -> bits[15:8] */
		setting_idx = 33;
		hw_val  = (value & 0xFF) << 8;
		hw_mask = 0x0000FF00;
		break;

	/* ---- Setting[35] (0x23): Channel config count ---- */

	case UA_MON_PARAM_CHAN_CFG:	/* 0x6a -> bits[4:0] */
		setting_idx = 35;
		hw_val  = value & 0x1F;
		hw_mask = 0x0000001F;
		break;

	/*
	 * NOP params: stored in kext object but no WriteSetting call.
	 * Accept for ALSA cache notifications but don't write hardware.
	 */
	case UA_MON_PARAM_DSP_SPANNING:	/* 0x16 */
	case 0x0b:
	case 0x0c:
	case 0x0d:
	case 0x10:
	case 0x11:
	case 0x12:
	case 0x17:
	case 0x18:
	case 0x26:
	case 0x27:
	case 0x28:
	case 0x29:
		dev_dbg(&ua->pdev->dev,
			"monitor param 0x%x: NOP/cache-only (ch=%u val=0x%x)\n",
			param_id, ch_idx, value);
		return 0;

	/*
	 * Error params: kext returns -18 (ENODEV).  Reject them.
	 */
	case 0x09:
	case 0x0e:
	case 0x3d:
		dev_dbg(&ua->pdev->dev,
			"monitor param 0x%x: rejected (error param)\n",
			param_id);
		return -EINVAL;

	default:
		dev_dbg(&ua->pdev->dev,
			"monitor_set_param: unknown param 0x%x (ch=%u val=0x%x)\n",
			param_id, ch_idx, value);
		return 0;
	}

	dev_dbg(&ua->pdev->dev,
		"monitor->mixer: setting[%u] val=0x%08x mask=0x%08x "
		"(ch=%u param=0x%x raw=0x%x)\n",
		setting_idx, hw_val, hw_mask,
		ch_idx, param_id, value);

	return ua_mixer_write_setting_locked(ua, setting_idx,
					     hw_val, hw_mask);
}

/* ----------------------------------------------------------------
 * Mixer / CLI ioctl handlers
 * ---------------------------------------------------------------- */

#pragma GCC push_options
#pragma GCC optimize("O1")
static noinline long ua_ioctl_cli_command(struct ua_device *ua,
					  unsigned long arg)
{
	struct ua_cli_command cmd;
	u32 word;
	unsigned int i, words;
	int ret;

	if (copy_from_user(&cmd, (void __user *)arg, sizeof(cmd)))
		return -EFAULT;

	if (cmd.cmd_len == 0 || cmd.cmd_len > UA_CLI_CMD_BUF_SIZE)
		return -EINVAL;

	mutex_lock(&ua->lock);

	ret = ua_cli_enable_locked(ua);
	if (ret)
		goto out_unlock;

	/* Write command buffer */
	words = (cmd.cmd_len + 3) / 4;
	for (i = 0; i < words; i++) {
		word = 0;
		memcpy(&word, &cmd.cmd_data[i * 4],
		       min_t(u32, 4, cmd.cmd_len - i * 4));
		ua_write(ua, UA_REG_CLI_CMD_BUF + i * 4, word);
	}

	/* Trigger command */
	ua_write(ua, UA_REG_CLI_STATUS, cmd.cmd_len);

	/* Poll for completion */
	{
		u32 status;
		unsigned int polls;

		ret = -ETIMEDOUT;
		for (polls = 0; polls < UA_CLI_TIMEOUT_POLLS; polls++) {
			status = ua_read(ua, UA_REG_CLI_STATUS);
			if (status == 0 || (status & BIT(31))) {
				ret = 0;
				break;
			}
			usleep_range(900, 1100);
		}
	}

	if (ret) {
		dev_err(&ua->pdev->dev, "CLI ioctl timeout\n");
		goto out_unlock;
	}

	/* Read response */
	cmd.resp_len = ua_read(ua, UA_REG_CLI_RESP_LEN);
	if (cmd.resp_len > UA_CLI_RESP_BUF_SIZE)
		cmd.resp_len = UA_CLI_RESP_BUF_SIZE;

	memset(cmd.resp_data, 0, sizeof(cmd.resp_data));
	words = (cmd.resp_len + 3) / 4;
	for (i = 0; i < words; i++) {
		word = ua_read(ua, UA_REG_CLI_RESP_BUF + i * 4);
		memcpy(&cmd.resp_data[i * 4], &word, sizeof(word));
	}

	mutex_unlock(&ua->lock);

	if (copy_to_user((void __user *)arg, &cmd, sizeof(cmd)))
		return -EFAULT;
	return 0;

out_unlock:
	mutex_unlock(&ua->lock);
	return ret;
}
#pragma GCC pop_options

/* GCC 13.3 ICE in try_forward_edges on this large switch — reduce optimization */
__attribute__((optimize("O1")))
static noinline __attribute__((optimize("O0"))) long
ua_ioctl_set_mixer_param(struct ua_device *ua, unsigned long arg)
{
	struct ua_mixer_param param;
	int ret = 0;	/* the preamp (type 1) path never set it */
	bool notify_alsa = false;
	bool digital_mode_changed = false;
	const char *ctl_name = NULL;
	unsigned int ch = 0;

	if (copy_from_user(&param, (void __user *)arg, sizeof(param)))
		return -EFAULT;

	dev_dbg(&ua->pdev->dev,
		"set_mixer_param: type=%u idx=%u param=0x%x val=0x%x\n",
		param.channel_type, param.channel_idx,
		param.param_id, param.value);

	mutex_lock(&ua->lock);

	switch (param.channel_type) {
	case 1:  /* Preamp → DSP mixer settings (no CLI) */
		/*
		 * Preamp controls go through DSP mixer settings only.
		 * Kext disassembly confirms: setInputParamARM routes ALL
		 * preamp params to CPcieDeviceMixer::WriteSetting.
		 * CLI binary commands FREEZE the ARM MCU — never use them.
		 *
		 * FLAG params (0x00-0x05): setting 0 (ch0-3) or 12 (ch4-7),
		 *   6-bit stride, bit position = param_id.
		 * GAIN params (0x06+): setting param_id+7 (ch0-3) or +15,
		 *   8-bit byte packing.
		 *
		 * Route (0x13) and Level (0x16) are sent to ALL input
		 * channels (0-31), not just physical preamps (0-3).
		 */
		if (param.param_id == 0x13 || param.param_id == 0x16) {
			if (param.channel_idx > 31) {
				ret = -EINVAL;
				goto out_unlock;
			}
		} else if (param.channel_idx >= ua->audio.num_preamps ||
			   param.channel_idx >= UA_MAX_PREAMP_CH) {
			ret = -EINVAL;
			goto out_unlock;
		}

		/* Write to DSP mixer settings (kext-verified path) */
		if (param.param_id <= 0x05) {
			/* Flag params: 6-bit stride, shared setting */
			unsigned int setting_idx;
			unsigned int shift;
			u32 hw_val, hw_mask;
			bool flag_set;

			setting_idx = (param.channel_idx > 3) ? 12 : 0;
			shift = (param.channel_idx & 3) * 6;
			hw_mask = (1u << param.param_id) << shift;

			/*
			 * Phase: kext uses float (+1.0f=normal, -1.0f=invert)
			 * but daemon sends int (0=normal, 1=invert).
			 * Handle both: bit 31 set (negative float) OR non-zero int.
			 */
			if (param.param_id == UA_PREAMP_PARAM_PHASE)
				flag_set = !!(param.value & BIT(31)) || param.value == 1;
			else
				flag_set = !!param.value;

			hw_val = flag_set ? hw_mask : 0;
			ua_mixer_write_setting_locked(ua, setting_idx,
						      hw_val, hw_mask);
		} else if (param.param_id == 0x06) {
			/*
			 * GainC: 6-bit field at setting[3] (ch0-3) or [14] (ch4-7).
			 * Kext disasm (sub_48734): mask=0x3F with a +4 bit gap
			 * between channel pairs:
			 *   ch0=shift 0, ch1=shift 6, ch2=shift 16, ch3=shift 22
			 * Bits [15:12] and [31:28] are reserved gaps.
			 */
			unsigned int setting_idx;
			unsigned int base_shift, shift;
			u32 value, mask;

			setting_idx = (param.channel_idx > 3) ? 14 : 3;
			base_shift = (param.channel_idx & 3) * 6;
			shift = (param.channel_idx & 2) ?
				(base_shift + 4) : base_shift;
			mask = 0x3Fu << shift;
			value = (param.value & 0x3F) << shift;
			ua_mixer_write_setting_locked(ua, setting_idx,
						      value, mask);
		} else if (param.param_id <= 0x17) {
			/* GainA(0x0A), GainB(0x09), other: 8-bit byte packing */
			unsigned int setting_idx;
			unsigned int shift;
			u32 value, mask;

			if (param.channel_idx <= 3)
				setting_idx = param.param_id + 7;
			else
				setting_idx = param.param_id + 15;

			if (setting_idx < UA_MIXER_BATCH_COUNT) {
				shift = (param.channel_idx & 3) * 8;
				value = (param.value & 0xFF) << shift;
				mask = 0xFF << shift;
				ua_mixer_write_setting_locked(ua, setting_idx,
							      value, mask);
			}
		}

		/* Update ALSA cache and prepare notification */
		ch = param.channel_idx;
		switch (param.param_id) {
		case UA_PREAMP_PARAM_MIC_LINE:
			ua->audio.preamp[ch].mic_line = !!param.value;
			ctl_name = "Line %u Input Select";
			break;
		case UA_PREAMP_PARAM_PAD:
			ua->audio.preamp[ch].pad = !!param.value;
			ctl_name = "Line %u Pad Switch";
			break;
		case UA_PREAMP_PARAM_48V:
			ua->audio.preamp[ch].phantom = !!param.value;
			ctl_name = "Mic %u 48V Phantom Power Switch";
			break;
		case UA_PREAMP_PARAM_LOWCUT:
			ua->audio.preamp[ch].lowcut = !!param.value;
			ctl_name = "Line %u LowCut Switch";
			break;
		case UA_PREAMP_PARAM_PHASE:
			/*
			 * Phase uses IEEE 754 float: 0x3f800000 = +1.0 (normal),
			 * 0xbf800000 = -1.0 (inverted).  For ALSA cache, treat
			 * negative float (bit 31 set) as "inverted".
			 */
			ua->audio.preamp[ch].phase = !!(param.value & BIT(31));
			ctl_name = "Line %u Phase Invert Switch";
			break;
		case UA_PREAMP_PARAM_GAIN_C:
			ua->audio.preamp[ch].gain = (int)param.value;
			ctl_name = "Line %u Capture Volume";
			break;
		}
		if (ctl_name)
			notify_alsa = true;
		break;

	case 2:  /* Monitor → DSP mixer settings */
		ret = ua_monitor_set_param(ua, param.channel_idx,
					   param.param_id, param.value);
		if (ret)
			break;

		/* Update ALSA cache and prepare notification */
		switch (param.param_id) {
		case UA_MON_PARAM_LEVEL:
			ua->audio.monitor.level = (int)param.value;
			ctl_name = "Monitor Playback Volume";
			notify_alsa = true;
			break;
		case UA_MON_PARAM_MUTE:
			/*
			 * Param 0x03 3-state: 0=off, 1=mono, 2=muted
			 * Update both mute and mono cache accordingly.
			 */
			if (param.value == 1) {
				/* MixToMono: not muted, mono on */
				ua->audio.monitor.mute = UA_MON_MUTE_OFF;
				ua->audio.monitor.mono = true;
				ctl_name = "Monitor Mono Switch";
			} else {
				ua->audio.monitor.mute = (int)param.value;
				ua->audio.monitor.mono = false;
				ctl_name = "Monitor Playback Switch";
			}
			notify_alsa = true;
			break;
		case UA_MON_PARAM_DIM:
			ua->audio.monitor.dim = !!param.value;
			ctl_name = "Monitor Dim Switch";
			notify_alsa = true;
			break;
		case UA_MON_PARAM_MONO:
			/* Legacy kext property ID 0x36 */
			ua->audio.monitor.mono = !!param.value;
			ctl_name = "Monitor Mono Switch";
			notify_alsa = true;
			break;
		case UA_MON_PARAM_SOURCE:
			ua->audio.monitor.source = (int)param.value;
			ctl_name = "Monitor Source";
			notify_alsa = true;
			break;
		case UA_MON_PARAM_HP1_SOURCE:
			ua->audio.monitor.hp1_source = (int)param.value;
			ctl_name = "Headphone 1 Source";
			notify_alsa = true;
			break;
		case UA_MON_PARAM_HP2_SOURCE:
			ua->audio.monitor.hp2_source = (int)param.value;
			ctl_name = "Headphone 2 Source";
			notify_alsa = true;
			break;
		case UA_MON_PARAM_DIM_LEVEL:
			ua->audio.monitor.dim_level = (int)param.value;
			ctl_name = "Monitor Dim Level";
			notify_alsa = true;
			break;
		case UA_MON_PARAM_TALKBACK:
			ua->audio.monitor.talkback = !!param.value;
			ctl_name = "Monitor Talkback Switch";
			notify_alsa = true;
			break;
		case UA_MON_PARAM_DIGITAL_MODE:
			ua->audio.monitor.digital_mode = (int)param.value;
			ctl_name = "Digital Output Mode";
			notify_alsa = true;
			digital_mode_changed = true;
			break;
		case UA_MON_PARAM_OUTPUT_REF:
			ua->audio.monitor.output_ref = (int)param.value;
			ctl_name = "Output Reference Level";
			notify_alsa = true;
			break;
		case UA_MON_PARAM_CUE1_MIRROR:
			ua->audio.monitor.cue1_mirror = (int)param.value;
			ctl_name = "CUE 1 Mirror Source";
			notify_alsa = true;
			break;
		case UA_MON_PARAM_CUE2_MIRROR:
			ua->audio.monitor.cue2_mirror = (int)param.value;
			ctl_name = "CUE 2 Mirror Source";
			notify_alsa = true;
			break;
		case UA_MON_PARAM_MIRROR_ENABLE_A:
			ua->audio.monitor.cue1_mirror_en = !!param.value;
			ctl_name = "CUE 1 Mirror Switch";
			notify_alsa = true;
			break;
		case UA_MON_PARAM_MIRROR_ENABLE_B:
			ua->audio.monitor.cue2_mirror_en = !!param.value;
			ctl_name = "CUE 2 Mirror Switch";
			notify_alsa = true;
			break;
		case UA_MON_PARAM_DIGITAL_MIRROR:
			ua->audio.monitor.digital_mirror = !!param.value;
			ctl_name = "Digital Mirror Switch";
			notify_alsa = true;
			break;
		case UA_MON_PARAM_IDENTIFY:
			ua->audio.monitor.identify = !!param.value;
			ctl_name = "Identify Switch";
			notify_alsa = true;
			break;
		case UA_MON_PARAM_SR_CONVERT:
			ua->audio.monitor.sr_convert = (int)param.value;
			ctl_name = "S/PDIF SRC Switch";
			notify_alsa = true;
			break;
		case UA_MON_PARAM_DSP_SPANNING:
			ua->audio.monitor.dsp_spanning = (int)param.value;
			ctl_name = "DSP Spanning Switch";
			notify_alsa = true;
			break;
		case UA_MON_PARAM_CUE1_MONO:
			ua->audio.monitor.cue1_mono = !!param.value;
			ctl_name = "CUE 1 Mono Switch";
			notify_alsa = true;
			break;
		case UA_MON_PARAM_CUE2_MONO:
			ua->audio.monitor.cue2_mono = !!param.value;
			ctl_name = "CUE 2 Mono Switch";
			notify_alsa = true;
			break;
		case UA_MON_PARAM_CUE1_MIX:
			ua->audio.monitor.cue1_mix = (int)param.value;
			ctl_name = "CUE 1 Mix Switch";
			notify_alsa = true;
			break;
		case UA_MON_PARAM_CUE2_MIX:
			ua->audio.monitor.cue2_mix = (int)param.value;
			ctl_name = "CUE 2 Mix Switch";
			notify_alsa = true;
			break;
		}
		break;

	case 0:  /* DSP bus enable (ch_type=0) */
		/*
		 * Kext RE: SetBusEnable(bus_idx=ch_idx, enable=param_id).
		 * Sends ring buffer cmd 0x1C to ALL DSPs.
		 * Note: param_id IS the enable value; value field ignored.
		 *
		 * macOS init sends: bus 1,2 enable=1 (S/PDIF, ADAT)
		 *                   bus 5,6 enable=1 (CUE routing)
		 */
		ret = ua_dsp_set_bus_enable(ua, param.channel_idx,
					    param.param_id);
		if (ret)
			dev_warn(&ua->pdev->dev,
				 "bus_enable failed: bus=%u en=%u (%d)\n",
				 param.channel_idx, param.param_id, ret);
		break;

	default:
		ret = -EINVAL;
	}

	/*
	 * The TOSLink output type picks the playback route channels
	 * (io_map=1).  Under ua->lock: the layout only changes under it.
	 */
	if (digital_mode_changed)
		ua_io_map_digital_mode_changed(ua);

out_unlock:
	mutex_unlock(&ua->lock);

	/* Notify ALSA userspace after releasing the lock */
	if (notify_alsa && ua->audio.card) {
		struct snd_ctl_elem_id eid;
		char name[44];

		memset(&eid, 0, sizeof(eid));
		eid.iface = SNDRV_CTL_ELEM_IFACE_MIXER;
		snprintf(name, sizeof(name), ctl_name, ch + 1);
		strscpy(eid.name, name, sizeof(eid.name));
		snd_ctl_notify(ua->audio.card,
			       SNDRV_CTL_EVENT_MASK_VALUE, &eid);
	}

	return ret;
}

static long ua_ioctl_get_mixer_param(struct ua_device *ua, unsigned long arg)
{
	struct ua_mixer_param param;
	u32 cli_cmd[9], hw_id, resp_len;
	u8 resp_data[UA_CLI_RESP_BUF_SIZE];
	int ret;

	if (copy_from_user(&param, (void __user *)arg, sizeof(param)))
		return -EFAULT;

	mutex_lock(&ua->lock);
	ret = ua_cli_enable_locked(ua);
	if (ret)
		goto out_unlock;

	switch (param.channel_type) {
	case 1:  /* Preamp → CLI query */
		if (param.channel_idx >= ua->audio.num_preamps ||
		    param.channel_idx >= UA_MAX_PREAMP_CH) {
			ret = -EINVAL;
			goto out_unlock;
		}

		hw_id = param.channel_idx * 0x800;
		memset(cli_cmd, 0, sizeof(cli_cmd));
		cli_cmd[0] = param.param_id;
		cli_cmd[5] = hw_id;
		cli_cmd[8] = 8;

		ret = ua_cli_send_locked(ua, (u8 *)cli_cmd, 36,
					 resp_data, &resp_len);
		if (ret)
			goto out_unlock;

		if (resp_len >= 4)
			memcpy(&param.value, resp_data, sizeof(u32));
		else
			param.value = 0;
		break;

	case 2:  /* Monitor → not yet implemented */
		ret = -ENOSYS;
		goto out_unlock;

	default:
		ret = -EINVAL;
		goto out_unlock;
	}

	mutex_unlock(&ua->lock);

	if (copy_to_user((void __user *)arg, &param, sizeof(param)))
		return -EFAULT;
	return 0;

out_unlock:
	mutex_unlock(&ua->lock);
	return ret;
}

static long ua_ioctl_set_mixer_bus_param(struct ua_device *ua,
					 unsigned long arg)
{
	struct ua_mixer_bus_param bp;

	if (copy_from_user(&bp, (void __user *)arg, sizeof(bp)))
		return -EFAULT;

	/*
	 * Bus param → DSP ring buffer command.
	 *
	 * From kext CUAD2DeviceMixer::SetMixerBusParam disassembly:
	 * bus coefficients are submitted as ring buffer commands (type
	 * 0x1D, 4 dwords) to DSP 0, NOT via mixer setting registers.
	 */
	{
		int bus_ret;

		mutex_lock(&ua->lock);
		bus_ret = ua_dsp_set_bus_param(ua, bp.bus_id,
					       bp.sub_param, bp.value);
		mutex_unlock(&ua->lock);
		return bus_ret;
	}

	/* Accept but do not write — bus coefficient DSP path unknown */
	return 0;
}

static long ua_ioctl_get_mixer_readback(struct ua_device *ua,
					unsigned long arg)
{
	struct ua_mixer_readback rb;
	u32 value, mask, checksum;
	unsigned int i;
	int ret;

	if (copy_from_user(&rb, (void __user *)arg, sizeof(rb)))
		return -EFAULT;

	mutex_lock(&ua->lock);

	/* Read first 38 settings into data array (readback struct is 41 words) */
	for (i = 0; i < 38; i++) {
		ret = ua_mixer_read_setting_locked(ua, i, &value, &mask);
		if (ret) {
			mutex_unlock(&ua->lock);
			return ret;
		}
		rb.data[i] = value;
	}

	mutex_unlock(&ua->lock);

	/* Checksum: word[38] = XOR of [0..37], word[39] = ~word[38] */
	checksum = 0;
	for (i = 0; i < 38; i++)
		checksum ^= rb.data[i];
	rb.data[38] = checksum;
	rb.data[39] = ~checksum;
	rb.data[40] = 0;

	if (copy_to_user((void __user *)arg, &rb, sizeof(rb)))
		return -EFAULT;
	return 0;
}

static long ua_ioctl_get_hw_readback(struct ua_device *ua, unsigned long arg)
{
	struct ua_hw_readback rb;

	mutex_lock(&ua->lock);

	/*
	 * Return cached readback from the service loop.
	 * The service loop reads BAR0 at 100Hz and caches results.
	 * This avoids racing with the service loop for the one-shot
	 * readback status register (only one reader can consume it).
	 */
	rb.status = ua->mixer_rb_status;
	memcpy(rb.data, ua->mixer_rb_data, sizeof(rb.data));

	mutex_unlock(&ua->lock);

	if (copy_to_user((void __user *)arg, &rb, sizeof(rb)))
		return -EFAULT;
	return 0;
}

static long ua_ioctl_write_mixer_setting(struct ua_device *ua,
					 unsigned long arg)
{
	struct ua_mixer_setting ms;
	int ret;

	if (copy_from_user(&ms, (void __user *)arg, sizeof(ms)))
		return -EFAULT;

	if (ms.index >= UA_MIXER_BATCH_COUNT)
		return -EINVAL;

	mutex_lock(&ua->lock);
	ret = ua_mixer_write_setting_locked(ua, ms.index, ms.value, ms.mask);
	mutex_unlock(&ua->lock);
	return ret;
}

static long ua_ioctl_read_mixer_setting(struct ua_device *ua,
					unsigned long arg)
{
	struct ua_mixer_setting ms;
	int ret;

	if (copy_from_user(&ms, (void __user *)arg, sizeof(ms)))
		return -EFAULT;

	if (ms.index >= UA_MIXER_NUM_SETTINGS)
		return -EINVAL;

	mutex_lock(&ua->lock);
	ret = ua_mixer_read_setting_locked(ua, ms.index, &ms.value, &ms.mask);
	mutex_unlock(&ua->lock);

	if (ret)
		return ret;

	if (copy_to_user((void __user *)arg, &ms, sizeof(ms)))
		return -EFAULT;
	return 0;
}

static long ua_ioctl_set_driver_param(struct ua_device *ua, unsigned long arg)
{
	struct ua_driver_param dp;

	if (copy_from_user(&dp, (void __user *)arg, sizeof(dp)))
		return -EFAULT;

	switch (dp.param_id) {
	case 0: {  /* Sample rate */
		int ret;

		if (ua->audio.transport_running)
			dev_warn(&ua->pdev->dev,
				 "changing sample rate while transport running!\n");
		ret = ua_audio_set_clock(ua, (unsigned int)dp.value);
		if (!ret)
			ua_notify_clock_controls(ua);
		return ret;
	}

	case 1: {  /* Clock source */
		unsigned int old = ua->audio.clock_source;
		int ret;

		ua->audio.clock_source = (unsigned int)dp.value;
		ret = ua_audio_set_clock(ua, ua->audio.sample_rate);
		if (!ret)
			ua_notify_clock_controls(ua);
		else
			ua->audio.clock_source = old;	/* not acknowledged */
		return ret;
	}

	default:
		return -EINVAL;
	}
}

static long ua_ioctl_get_driver_param(struct ua_device *ua, unsigned long arg)
{
	struct ua_driver_param dp;

	if (copy_from_user(&dp, (void __user *)arg, sizeof(dp)))
		return -EFAULT;

	switch (dp.param_id) {
	case 0:  /* Sample rate */
		dp.value = ua->audio.sample_rate;
		break;
	case 1:  /* Clock source */
		dp.value = ua->audio.clock_source;
		break;
	case 2:  /* Transport running */
		dp.value = ua->audio.transport_running ? 1 : 0;
		break;
	default:
		return -EINVAL;
	}

	if (copy_to_user((void __user *)arg, &dp, sizeof(dp)))
		return -EFAULT;
	return 0;
}

/* ----------------------------------------------------------------
 * DSP firmware ioctls
 * ---------------------------------------------------------------- */

static long ua_ioctl_load_firmware(struct ua_device *ua, unsigned long arg)
{
	struct ua_fw_load fl;
	void *fw_buf;
	int ret;

	if (copy_from_user(&fl, (void __user *)arg, sizeof(fl)))
		return -EFAULT;

	if (!fl.fw_size || fl.fw_size > UA_FW_MAX_SIZE)
		return -EINVAL;

	fw_buf = kvmalloc(fl.fw_size, GFP_KERNEL);
	if (!fw_buf)
		return -ENOMEM;

	if (copy_from_user(fw_buf, (void __user *)(unsigned long)fl.fw_data,
			   fl.fw_size)) {
		kvfree(fw_buf);
		return -EFAULT;
	}

	mutex_lock(&ua->lock);
	ret = ua_dsp_load_firmware(ua, fw_buf, fl.fw_size);
	mutex_unlock(&ua->lock);

	kvfree(fw_buf);
	return ret;
}

/*
 * Does this device need the ring-buffer DSP connect (_connectToDsps)?
 * Default: the v11.8.1 set, which is exactly the complement of
 * ua_uses_audio_extension().  v12_connect=1: the v12 set, which also
 * covers Solo TB / Arrow / 0x29 — those keep their AudioExtension
 * bring-up and get the ring connect on top, as in the kext's Connect().
 */
static bool ua_needs_ring_connect(struct ua_device *ua)
{
	if (v12_connect)
		return ua_ring_connect_v12(ua->device_type);
	return !ua_uses_audio_extension(ua->device_type);
}

/*
 * v12_connect=1: CPcieDevice::Connect (kext 12.0.0 x86 0x5f84) always runs
 * CPcieAudioExtension::Connect — the ACEFACE handshake — before
 * _connectToDsps.  Linux used to ring-connect first and ACEFACE later
 * (from ua_audio_init / pcm_prepare).  Caller holds ua->lock.
 */
static void ua_v12_aceface_first(struct ua_device *ua)
{
	int ret;

	if (!v12_connect || ua->aceface_done)
		return;

	ret = ua_aceface_handshake(ua);
	dev_info(&ua->pdev->dev,
		 "v12_connect: ACEFACE before ring connect: %s (%d)\n",
		 ret ? "failed" : "ok", ret);
}

static long ua_ioctl_dsp_connect(struct ua_device *ua)
{
	int ret;

	if (ua_uses_audio_extension(ua->device_type)) {
		/*
		 * AudioExtension devices use ACEFACE handshake via
		 * direct BAR0 register writes, not ring buffer connect.
		 * ua_audio_reconnect() resets the connected flag and
		 * re-does the ACEFACE handshake + routing + service.
		 */
		ret = ua_audio_reconnect(ua);
		if (ret || !ua_needs_ring_connect(ua))
			return ret;
	}

	mutex_lock(&ua->lock);
	ua_v12_aceface_first(ua);
	ret = ua_dsp_connect_all(ua);
	mutex_unlock(&ua->lock);

	return ret;
}

static long ua_ioctl_dsp_send_block(struct ua_device *ua, unsigned long arg)
{
	struct ua_dsp_block blk;
	void *data_buf = NULL;
	int ret;

	if (copy_from_user(&blk, (void __user *)arg, sizeof(blk)))
		return -EFAULT;

	/* Validate parameters */
	if (blk.dsp_index >= ua->num_dsps)
		return -EINVAL;
	if (blk.data_size & 3)  /* Must be 4-byte aligned */
		return -EINVAL;
	if (blk.data_size > 256 * 1024)  /* 256 KB max */
		return -EINVAL;

	/* Allocate kernel buffer for data payload */
	if (blk.data_size > 0) {
		data_buf = kmalloc(blk.data_size, GFP_KERNEL);
		if (!data_buf)
			return -ENOMEM;

		if (copy_from_user(data_buf,
				   (void __user *)(unsigned long)blk.data,
				   blk.data_size)) {
			kfree(data_buf);
			return -EFAULT;
		}
	}

	/* Send block via DSP ring buffer */
	mutex_lock(&ua->lock);
	ret = ua_dsp_send_block(ua, blk.dsp_index, blk.cmd, blk.param,
				data_buf, blk.data_size);
	mutex_unlock(&ua->lock);

	kfree(data_buf);
	return ret;
}

/*
 * UA_IOCTL_GET_IO_MAP: firmware IO descriptors + ALSA channel map.
 * Built at -O2: under this file's -Og, GCC 12 (Debian 12, 6.1 headers)
 * reports a bogus "copy source size is too small" for the heap buffer.
 */
static noinline __attribute__((optimize("O2"))) long
ua_ioctl_get_io_map(struct ua_device *ua, unsigned long arg)
{
	struct ua_io_map_info *info;
	long ret = 0;

	info = kzalloc(sizeof(*info), GFP_KERNEL);
	if (!info)
		return -ENOMEM;

	ua_io_map_get_info(ua, info);
	if (copy_to_user((void __user *)arg, info, sizeof(*info)))
		ret = -EFAULT;

	kfree(info);
	return ret;
}

/* ----------------------------------------------------------------
 * Main ioctl dispatch
 * ---------------------------------------------------------------- */

static long ua_ioctl(struct file *filp, unsigned int cmd, unsigned long arg)
{
	struct ua_device *ua = filp->private_data;

	/* Device removed — reject all ioctls */
	if (atomic_read(&ua->shutdown))
		return -ENODEV;

	switch (cmd) {
	case UA_IOCTL_GET_DEVICE_INFO:
		return ua_ioctl_get_device_info(ua, arg);

	case UA_IOCTL_GET_DSP_INFO:
		return ua_ioctl_get_dsp_info(ua, arg);

	case UA_IOCTL_GET_TRANSPORT_INFO:
		return ua_ioctl_get_transport_info(ua, arg);

	case UA_IOCTL_READ_REG:
		return ua_ioctl_read_reg(ua, arg);

	case UA_IOCTL_WRITE_REG:
		return ua_ioctl_write_reg(ua, arg);

	case UA_IOCTL_RESET_DMA:
		mutex_lock(&ua->lock);
		ua_reset_dma(ua);
		mutex_unlock(&ua->lock);
		return 0;

	/* Mixer / CLI ioctls */
	case UA_IOCTL_SET_MIXER_BUS_PARAM:
		return ua_ioctl_set_mixer_bus_param(ua, arg);

	case UA_IOCTL_SET_MIXER_PARAM:
		return ua_ioctl_set_mixer_param(ua, arg);

	case UA_IOCTL_GET_MIXER_PARAM:
		return ua_ioctl_get_mixer_param(ua, arg);

	case UA_IOCTL_GET_MIXER_READBACK:
		return ua_ioctl_get_mixer_readback(ua, arg);

	case UA_IOCTL_SET_DRIVER_PARAM:
		return ua_ioctl_set_driver_param(ua, arg);

	case UA_IOCTL_GET_DRIVER_PARAM:
		return ua_ioctl_get_driver_param(ua, arg);

	case UA_IOCTL_CLI_COMMAND:
		return ua_ioctl_cli_command(ua, arg);

	case UA_IOCTL_WRITE_MIXER_SETTING:
		return ua_ioctl_write_mixer_setting(ua, arg);

	case UA_IOCTL_READ_MIXER_SETTING:
		return ua_ioctl_read_mixer_setting(ua, arg);

	case UA_IOCTL_GET_HW_READBACK:
		return ua_ioctl_get_hw_readback(ua, arg);

	/* DSP firmware ioctls */
	case UA_IOCTL_LOAD_FIRMWARE:
		return ua_ioctl_load_firmware(ua, arg);

	case UA_IOCTL_DSP_CONNECT:
		return ua_ioctl_dsp_connect(ua);

	case UA_IOCTL_DSP_TEST_DMA: {
		int dma_ret;

		mutex_lock(&ua->lock);
		dma_ret = ua_dsp_test_dma_ref(ua);
		mutex_unlock(&ua->lock);
		return dma_ret;
	}

	case UA_IOCTL_DSP_TEST_SEND: {
		int send_ret;

		mutex_lock(&ua->lock);
		send_ret = ua_dsp_test_send_block(ua);
		mutex_unlock(&ua->lock);
		return send_ret;
	}

	case UA_IOCTL_DSP_SEND_BLOCK:
		return ua_ioctl_dsp_send_block(ua, arg);

	case UA_IOCTL_SEND_ROUTING: {
		int route_ret;

		mutex_lock(&ua->lock);
		route_ret = ua_dsp_send_routing(ua);
		mutex_unlock(&ua->lock);
		return route_ret;
	}

	case UA_IOCTL_GET_IO_MAP:
		return ua_ioctl_get_io_map(ua, arg);

	case UA_IOCTL_RING_SEND: {
		struct ua_ring_cmd rc;
		int ring_ret;

		if (copy_from_user(&rc, (void __user *)arg, sizeof(rc)))
			return -EFAULT;

		mutex_lock(&ua->lock);
		ring_ret = ua_dsp_ring_send_raw(ua, rc.dsp_idx,
						rc.word0, rc.word1,
						rc.word2, rc.word3);
		dev_info(&ua->pdev->dev,
			 "RING_SEND[%u]: 0x%08x %08x %08x %08x → %s\n",
			 rc.dsp_idx, rc.word0, rc.word1, rc.word2, rc.word3,
			 ring_ret ? "FAILED" : "OK");
		mutex_unlock(&ua->lock);
		return ring_ret;
	}

	case UA_IOCTL_LOAD_FW_DSP: {
		u32 dsp_idx;
		int fw_ret;

		if (copy_from_user(&dsp_idx, (void __user *)arg,
				   sizeof(dsp_idx)))
			return -EFAULT;

		if (dsp_idx >= ua->num_dsps)
			return -EINVAL;

		mutex_lock(&ua->lock);
		fw_ret = ua_dsp_load_mixer_blocks_to(ua, dsp_idx);
		mutex_unlock(&ua->lock);
		dev_info(&ua->pdev->dev,
			 "LOAD_FW_DSP[%u]: %s\n",
			 dsp_idx, fw_ret ? "FAILED" : "OK");
		return fw_ret;
	}

	case UA_IOCTL_DSP_SRAM_DATA: {
		struct ua_dsp_block blk;
		void __user *udata;
		void *kdata;
		int sram_ret;

		if (copy_from_user(&blk, (void __user *)arg, sizeof(blk)))
			return -EFAULT;
		if (blk.dsp_index >= ua->num_dsps || !blk.data_size ||
		    (blk.data_size & 3) || blk.data_size > 4096)
			return -EINVAL;

		kdata = kmalloc(blk.data_size, GFP_KERNEL);
		if (!kdata)
			return -ENOMEM;

		udata = (void __user *)(unsigned long)blk.data;
		if (copy_from_user(kdata, udata, blk.data_size)) {
			kfree(kdata);
			return -EFAULT;
		}

		mutex_lock(&ua->lock);
		sram_ret = ua_dsp_send_sram_data(ua, blk.dsp_index,
						 blk.cmd, kdata,
						 blk.data_size);
		mutex_unlock(&ua->lock);
		kfree(kdata);
		return sram_ret;
	}

	case UA_IOCTL_DSP_SEND_RAW: {
		struct ua_dsp_block blk;
		void __user *udata;
		void *kdata;
		int raw_ret;

		if (copy_from_user(&blk, (void __user *)arg, sizeof(blk)))
			return -EFAULT;
		if (blk.dsp_index >= ua->num_dsps || !blk.data_size ||
		    (blk.data_size & 3) || blk.data_size > 4096)
			return -EINVAL;

		kdata = kmalloc(blk.data_size, GFP_KERNEL);
		if (!kdata)
			return -ENOMEM;

		udata = (void __user *)(unsigned long)blk.data;
		if (copy_from_user(kdata, udata, blk.data_size)) {
			kfree(kdata);
			return -EFAULT;
		}

		mutex_lock(&ua->lock);
		raw_ret = ua_dsp_send_raw_dma(ua, blk.dsp_index,
					      kdata, blk.data_size);
		mutex_unlock(&ua->lock);
		kfree(kdata);
		return raw_ret;
	}

	case UA_IOCTL_DMA_TEST: {
		struct ua_dma_test dt;

		if (copy_from_user(&dt, (void __user *)arg, sizeof(dt)))
			return -EFAULT;

		mutex_lock(&ua->lock);
		{
			int ret = ua_audio_dma_test(ua, &dt);

			mutex_unlock(&ua->lock);
			if (ret)
				return ret;
		}

		if (copy_to_user((void __user *)arg, &dt, sizeof(dt)))
			return -EFAULT;
		return 0;
	}

	default:
		return -ENOTTY;
	}
}

static const struct file_operations ua_fops = {
	.owner		= THIS_MODULE,
	.open		= ua_open,
	.release	= ua_release,
	.unlocked_ioctl	= ua_ioctl,
#ifdef CONFIG_COMPAT
	.compat_ioctl	= compat_ptr_ioctl,
#endif
};

/* ----------------------------------------------------------------
 * DSP init and cold boot firmware loading
 *
 * Extracted from ua_probe() to reduce function complexity and
 * avoid GCC-13 internal compiler errors (cfgcleanup.cc ICE).
 * ---------------------------------------------------------------- */

/*
 * Is the mixer DSP running (gentle-init check)?  SEQ_RD > 0 and equal to
 * SEQ_WR, and — since the counters can match on a zombie DSP left by a
 * broken session — a readback within 300 ms.
 */
static bool ua_mixer_dsp_alive(struct ua_device *ua)
{
	struct pci_dev *pdev = ua->pdev;
	u32 seq_wr, seq_rd, mixer_state;
	unsigned int rb_polls;
	u32 rb = 0;

	seq_wr = ua_read(ua, UA_REG_MIXER_SEQ_WR);
	seq_rd = ua_read(ua, UA_REG_MIXER_SEQ_RD);
	mixer_state = ua_read(ua, UA_REG_MIXER_BASE);

	dev_info(&pdev->dev,
		 "pre-init: SEQ_WR=%u SEQ_RD=%u mixer_state=0x%08x\n",
		 seq_wr, seq_rd, mixer_state);

	if (!(seq_rd > 0 && seq_rd == seq_wr))
		return false;

	ua_write(ua, UA_REG_MIXER_RB_STATUS, 0);
	for (rb_polls = 0; rb_polls < 30; rb_polls++) {
		usleep_range(10000, 12000);
		rb = ua_read(ua, UA_REG_MIXER_RB_STATUS);
		if (rb == 1)
			return true;
	}

	dev_warn(&pdev->dev,
		 "SEQ counters look alive (%u/%u) but readback dead — forcing cold boot\n",
		 seq_wr, seq_rd);
	return false;
}

/*
 * @resume: called on system resume — the DMA rings and DSP rings already
 * exist and are only reprogrammed (the probe path allocates them).
 * @dsp_was_dead: !ua_mixer_dsp_alive(), i.e. take the cold path
 * (DMA reset + firmware load) rather than gentle init.
 */
static int ua_dsp_init_and_load(struct ua_device *ua, bool resume,
				bool dsp_was_dead)
{
	struct pci_dev *pdev = ua->pdev;
	u32 seq_wr, seq_rd;
	int ret;

	if (!dsp_was_dead) {
		dev_info(&pdev->dev,
			 "mixer DSP alive! Skipping DMA reset to preserve state\n");
		if (ua_read(ua, UA_REG_FPGA_REV) != ua->fpga_rev) {
			dev_err(&pdev->dev, "FPGA revision mismatch\n");
			return -EIO;
		}
		ua_check_dsps(ua);
		/* ACEFACE does NOT persist across OS reboots.
		 * Leave aceface_done = false so ua_audio_connect()
		 * does the full ACEFACE handshake even on warm boot. */

		/*
		 * Transport and IRQs were already stopped and cleared by
		 * ua_hw_stop_xport_irqs() before bus mastering came on.
		 * warm_dma_reset=1 adds the rest of the kext's
		 * ProgramRegisters before the SG rewrite: TRANSPORT = 0,
		 * ResetDMAEngines (kext masks), ISR = ~0.
		 */
		if (warm_dma_reset) {
			dev_info(&pdev->dev,
				 "warm_dma_reset: TRANSPORT=0 + kext ResetDMAEngines\n");
			ua_write(ua, UA_REG_TRANSPORT, 0);
			ua_reset_dma_engines(ua);
			ua_write(ua, UA_REG_IRQ_STATUS, UA_IRQ_CLEAR_ALL);
		}

		/* SG table must be programmed even on warm boot —
		 * the previous OS's SG entries point to invalid
		 * physical addresses.  Without this, SAMPLE_POS
		 * stays at 0 after transport start.
		 * Applies to every device class: the SG SRAM and the
		 * DMA engine enables are FPGA-side, independent of the
		 * AudioExtension vs ring-connect DSP handshake. */
		ret = resume ? ua_audio_reprogram_dma(ua) :
			       ua_audio_preinit_dma(ua);
		if (ret)
			dev_warn(&pdev->dev,
				 "warm boot DMA preinit failed: %d\n",
				 ret);
	} else {
		ret = ua_program_registers(ua);
		if (ret) {
			dev_err(&pdev->dev, "hardware init failed: %d\n", ret);
			return ret;
		}
		/* The DSP is (re)loaded below: everything sent to the
		 * previous firmware instance has to be sent again. */
		ua->fw_loaded = false;
		ua->dsps_connected = false;
		ua->plugins_activated = false;
	}

	seq_wr = ua_read(ua, UA_REG_MIXER_SEQ_WR);
	seq_rd = ua_read(ua, UA_REG_MIXER_SEQ_RD);
	dev_info(&pdev->dev,
		 "post-init: SEQ_WR=%u SEQ_RD=%u\n", seq_wr, seq_rd);

	/*
	 * Always allocate DSP ring buffers — needed for firmware
	 * loading on cold boot AND for routing config on warm boot.
	 * On resume the existing rings are reprogrammed instead.
	 */
	ret = ua_dsp_rings_init(ua);
	if (ret)
		dev_warn(&pdev->dev, "DSP ring init failed (%d)\n", ret);

	if (dsp_was_dead) {
		int fw_ret;

		ua_write(ua, UA_REG_MIXER_SEQ_WR, 0);

		/*
		 * AudioExtension devices: the FPGA ring buffer engine
		 * requires the FULL PrepareTransport register sequence
		 * before it processes ring entries.
		 *
		 * ua_boot_transport_start() programs all transport regs,
		 * starts transport, does ACEFACE, and pulses DMA reset.
		 *
		 * Discovered 2026-03-01: just AX_CONTROL=0x20F alone is
		 * insufficient — all transport regs must be set first.
		 *
		 * IOMMU FIX (2026-03-09): ua_audio_preinit_dma() MUST be
		 * called before ua_boot_transport_start().  When transport
		 * starts, the FPGA immediately begins audio DMA using the
		 * SG table in BAR0 SRAM.  Without pre-allocation, all SG
		 * entries are zero → FPGA DMA to address 0x0 → IOMMU fault
		 * → PCIe Uncorrectable Fatal error → kernel lockup on Fedora
		 * 43 with Intel VT-d enforced.
		 */
		/*
		 * Ring-connect devices (x8/x8p/x16/Twin X, issue #46)
		 * need this too: nothing else programs the audio SG
		 * table or clears the DMA reset strobes for them, so
		 * the FPGA was left DMAing to whatever the SG SRAM
		 * held — SAMPLE_POS frozen at 0, and a host lockup as
		 * soon as a stream opened.
		 */
		ret = resume ? ua_audio_reprogram_dma(ua) :
			       ua_audio_preinit_dma(ua);
		if (ret) {
			dev_err(&pdev->dev,
				"DMA pre-alloc failed: %d\n", ret);
			return ret;
		}

		if (ua_uses_audio_extension(ua->device_type)) {
			/*
			 * Set clock source for Apollo x4 internal clock.
			 * DTrace RE (2026-03-09): macOS uses source=0xC
			 * (value 0x020C for 48kHz). Source=0 (0x0200) gets
			 * no FPGA ack. Pre-ACEFACE clock write doesn't work
			 * because notification status (0xC030) isn't configured
			 * until ACEFACE writes config dwords.
			 */
			ua->audio.clock_source = 0xC;

			ua_boot_transport_start(ua);

			/*
			 * Re-write ring page addresses after DMA reset.
			 * The DMA pulse deactivates the FPGA ring engine;
			 * re-writing page addresses re-initializes it.
			 */
			ua_dsp_rings_reprogram(ua);
		}

		/* DMA locality test removed from init path — it consumed
		 * ring entries and the size sweep permanently stalled the
		 * ring before FW loading could happen.
		 */

		fw_ret = ua_dsp_load_mixer_blocks(ua);

		/* TODO: DSPs 1-3 need firmware too.
		 * Windows warm-boot shows 612/375/350 cmd entries to
		 * DSPs 1-3. Loading the SAME firmware crashes the FPGA.
		 * Need to study what Windows actually sends to each DSP
		 * (likely different firmware segments or different cmds).
		 */

		if (fw_ret == 0) {
			/*
			 * Load captured plugin-chain DMA payloads so
			 * ua_dsp_activate_plugin_chain() can substitute
			 * Linux dma_addrs for the Windows-era host PAs
			 * baked into ua_plugin_chain_data[].  Soft-fails:
			 * if the firmware blob is missing, DMA_REF entries
			 * are skipped and preamp relays (PAD/48V/MicLine)
			 * will not toggle, but the rest of the driver
			 * still operates.
			 */
			int plugin_ret = ua_dsp_load_plugin_payloads(ua);

			if (plugin_ret && plugin_ret != -ENOENT)
				dev_warn(&pdev->dev,
					 "plugin chain fw load failed: %d\n",
					 plugin_ret);

			/*
			 * No DSP program upload here any more.  The old
			 * ua_dsp_load_programs() step (command 0x00360000 on
			 * DSP 0) has no counterpart in the kext, never got a
			 * response on the x4 (sendBlock timed out after 300
			 * ms, "DSP program load failed: -110" on every cold
			 * start, titan 2026-10-10), and the cold start works
			 * the same without it.
			 */

			if (ua_uses_audio_extension(ua->device_type)) {
				/*
				 * On cold boot, ACEFACE fails before FW
				 * load because the DSP bootloader can't
				 * respond.  Now that FW is loaded, retry
				 * with transport still running.
				 */
				if (!ua->aceface_done) {
					dev_info(&pdev->dev,
						 "retrying ACEFACE after FW load\n");
					ret = ua_aceface_handshake(ua);
					if (ret)
						dev_warn(&pdev->dev,
							 "post-FW ACEFACE failed: %d\n",
							 ret);
				}
				dev_info(&pdev->dev,
					 "AudioExtension: ACEFACE %s, "
					 "full connect deferred to "
					 "ua_audio_init()\n",
					 ua->aceface_done ? "done" : "FAILED");
			}

			if (ua_needs_ring_connect(ua)) {
				/* v12_connect: ACEFACE before ring connect */
				ua_v12_aceface_first(ua);

				fw_ret = ua_dsp_connect_all(ua);
				/*
				 * Non-AudioExtension devices connect in
				 * probe; activate the plugin chain now so
				 * its DSP programs load before userspace
				 * opens the device.  AudioExtension devices
				 * defer activation to ua_audio_connect()
				 * (after ACEFACE completes).
				 */
				if (fw_ret == 0 && !ua->plugins_activated) {
					int pc_ret =
						ua_dsp_activate_plugin_chain(ua);
					if (pc_ret)
						dev_warn(&pdev->dev,
							 "plugin chain activate failed: %d\n",
							 pc_ret);
					else
						ua->plugins_activated = true;
				}
			}

			if (fw_ret == 0) {
				unsigned int polls;
				u32 rb;

				ua_write(ua, UA_REG_MIXER_RB_STATUS, 0);
				for (polls = 0; polls < 50; polls++) {
					usleep_range(10000, 12000);
					rb = ua_read(ua,
						     UA_REG_MIXER_RB_STATUS);
					if (rb == 1)
						break;
				}

				if (rb == 1)
					dev_info(&pdev->dev,
						 "mixer DSP alive (readback OK after %u polls)\n",
						 polls);
				else
					dev_warn(&pdev->dev,
						 "mixer DSP: readback not responding "
						 "after firmware load (rb=%u)\n",
						 rb);

				seq_wr = ua_read(ua, UA_REG_MIXER_SEQ_WR);
				seq_rd = ua_read(ua, UA_REG_MIXER_SEQ_RD);
				dev_info(&pdev->dev,
					 "post-connect: SEQ_WR=%u SEQ_RD=%u\n",
					 seq_wr, seq_rd);
			} else {
				dev_warn(&pdev->dev,
					 "DSP connect failed: %d\n", fw_ret);
			}
		} else if (fw_ret != -ENOENT) {
			dev_warn(&pdev->dev,
				 "mixer firmware load failed: %d\n", fw_ret);
		}
	}

	/*
	 * Stop the cold boot transport.  Real transport start
	 * happens in pcm_prepare when ALSA opens the device.
	 */
	if (ua_uses_audio_extension(ua->device_type))
		ua_boot_transport_stop(ua);

	/*
	 * Second clock write after transport stop (macOS Phase G).
	 * The early clock write (pre-transport) matches Phase A.
	 * clock_source already set to 0xC above.
	 */
	/*
	 * EXPERIMENT: Skip clock write — playback works without it.
	 * Clock write (source=0xC) transitions DSP to active processing
	 * mode but BREAKS playback (no audio output). Without it, DSP
	 * stays in default passthrough mode where playback works.
	 * Capture needs this write but it breaks playback.
	 * (2026-03-18: confirmed playback broken with clock write)
	 */
#if 0
	if (ua_uses_audio_extension(ua->device_type) && ua->aceface_done) {
		ret = ua_audio_set_clock(ua, 48000);
		dev_info(&pdev->dev,
			 "post-boot clock config: %s (ret=%d)\n",
			 ret == 0 ? "OK" : "FAILED", ret);
	}
#endif

	seq_wr = ua_read(ua, UA_REG_MIXER_SEQ_WR);
	seq_rd = ua_read(ua, UA_REG_MIXER_SEQ_RD);
	dev_info(&pdev->dev,
		 "post-ring-init: SEQ_WR=%u SEQ_RD=%u\n",
		 seq_wr, seq_rd);

	return 0;
}

/* ----------------------------------------------------------------
 * PCI probe / remove
 * ---------------------------------------------------------------- */

/*
 * UAD2Pcie2::_initializeInterrupts (kext 12.0.0 x86 0xbe40) reads config
 * 0x02 (device ID) and 0x2E (subsystem ID): device 0x0001 always gets the
 * legacy interrupt; device 0x0002 gets it when the subsystem is 0x05, 0x0A
 * or 0x0B (bit mask 0xc20).  Every other device uses MSI.
 */
static bool ua_wants_intx(struct pci_dev *pdev)
{
	if (pdev->device == PCI_DEVICE_ID_UA_UAD2)
		return true;

	switch (pdev->subsystem_device) {
	case 0x05:
	case 0x0A:
	case 0x0B:
		return true;
	default:
		return false;
	}
}

static int ua_probe(struct pci_dev *pdev, const struct pci_device_id *id)
{
	struct ua_device *ua;
	bool intx;
	int ret, devno;

	ua = devm_kzalloc(&pdev->dev, sizeof(*ua), GFP_KERNEL);
	if (!ua)
		return -ENOMEM;

	ua->pdev = pdev;
	mutex_init(&ua->lock);
	spin_lock_init(&ua->irq_lock);
	INIT_LIST_HEAD(&ua->dma_orphans);
	INIT_WORK(&ua->resume_work, ua_resume_work);
	pci_set_drvdata(pdev, ua);

	/* Enable device */
	ret = pcim_enable_device(pdev);
	if (ret) {
		dev_err(&pdev->dev, "failed to enable PCI device: %d\n", ret);
		return ret;
	}

	/*
	 * Bus mastering is enabled only after the transport left running
	 * by the previous owner (warm reboot, kexec, macOS, rmmod) has been
	 * stopped — see ua_hw_stop_xport_irqs() below.  Otherwise it DMAs
	 * into stale addresses the moment BME comes on.
	 */

	/*
	 * Force 32-bit DMA mask.
	 *
	 * Although the SG registers have hi/lo pairs (64-bit capable),
	 * the FPGA may only use the low 32 bits internally.  With 64-bit
	 * DMA, buffers land above 4 GiB (e.g. 0x19f800000) and the FPGA
	 * may truncate, causing DMA to wrong addresses.
	 *
	 * Force 32-bit to keep buffers below 4 GiB.
	 */
	ret = dma_set_mask_and_coherent(&pdev->dev, DMA_BIT_MASK(32));
	if (ret) {
		dev_err(&pdev->dev, "failed to set DMA mask: %d\n", ret);
		return ret;
	}

	/* Map BAR0 — kext asserts minimum 0x2800 (10 KiB) */
	ua->regs_size = pci_resource_len(pdev, 0);
	if (ua->regs_size < UA_MIN_BAR_SIZE) {
		dev_err(&pdev->dev, "BAR0 too small: %llu < %u\n",
			(unsigned long long)ua->regs_size, UA_MIN_BAR_SIZE);
		return -ENODEV;
	}

	ua->regs = pcim_iomap(pdev, 0, 0);
	if (!ua->regs) {
		dev_err(&pdev->dev, "failed to map BAR0\n");
		return -ENOMEM;
	}

	ua->regs_phys = pci_resource_start(pdev, 0);

	/* Read and cache FPGA revision — first thing the kext does after mapping */
	ua->fpga_rev = ua_read(ua, UA_REG_FPGA_REV);
	ua->subsystem_id = pdev->subsystem_device;

	/* Detect firmware version, device type, and DSP count from hardware regs */
	ua_detect_capabilities(ua);

	/* tools/contribute/device-probe.sh parses this line; keep its shape */
	dev_info(&pdev->dev,
		 "%s: FPGA rev 0x%08x, subsys 0x%04x, %u DSPs, FW %s\n",
		 ua_device_name(ua->device_type),
		 ua->fpga_rev, ua->subsystem_id, ua->num_dsps,
		 ua->fw_v2 ? "v2" : "v1");
	dev_info(&pdev->dev, "device type 0x%02x\n", ua->device_type);

	/*
	 * Previous-session ring state, for warm-boot debugging.  Registers
	 * only: the old WARM SNAPSHOT also memremap()ed and read whatever
	 * physical address the device reported for ring page 0 — memory
	 * left over from another OS, possibly not RAM at all.
	 */
	{
		int d;

		for (d = 0; d < 4; d++) {
			u32 bank = ua_dsp_base(d);

			dev_dbg(&pdev->dev,
				"previous session: DSP%d cmd wp=%u pos=%u pg0=%08x:%08x\n",
				d, ua_read(ua, bank + UA_RING_WRITE_PTR),
				ua_read(ua, bank + UA_RING_POSITION),
				ua_read(ua, bank + UA_RING_PAGE0_HI),
				ua_read(ua, bank + UA_RING_PAGE0_LO));
		}
		dev_dbg(&pdev->dev,
			"previous session: SEQ WR=%u RD=%u rb32=0x%08x\n",
			ua_read(ua, UA_REG_MIXER_SEQ_WR),
			ua_read(ua, UA_REG_MIXER_SEQ_RD),
			ua_read(ua, 0x3894));
	}

	/*
	 * Quiesce before bus mastering (kext ProgramRegisters order):
	 * stop the transport, mask and clear all interrupts, and pull every
	 * DSP ring's target back to its position so no descriptor left by
	 * the previous owner (whose ring pages may be freed memory by now)
	 * is fetched once BME is on.  Leaves a live mixer DSP alone, so the
	 * warm path below still applies.
	 */
	ua_hw_stop_xport_irqs(ua);
	ua_dsp_rings_neutralize(ua);
	pci_set_master(pdev);

	/* A cold re-init after system resume needs the firmware files */
	ua_dsp_cache_firmware(ua);

	/*
	 * Interrupt type, as UAD2Pcie2::_initializeInterrupts (kext 12.0.0
	 * x86 0xbe40): legacy INTx for device 0x0001 (UAD-2 PCIe) and for
	 * subsystems 0x05, 0x0A and 0x0B; MSI for every other device.
	 */
	intx = ua_wants_intx(pdev);
	if (intx) {
		dev_info(&pdev->dev,
			 "device 0x%04x subsys 0x%04x: legacy INTx (as the macOS driver)\n",
			 pdev->device, pdev->subsystem_device);
		ua->num_irq_vectors = pci_alloc_irq_vectors(pdev, 1, 1,
							     PCI_IRQ_LEGACY);
	} else {
		/* Allocate MSI vectors — device supports 1-2 */
		ua->num_irq_vectors = pci_alloc_irq_vectors(pdev,
							     UA_MSI_VECTORS_MIN,
							     UA_MSI_VECTORS_MAX,
							     PCI_IRQ_MSI | PCI_IRQ_MSIX);
		if (ua->num_irq_vectors < 0) {
			dev_warn(&pdev->dev, "MSI allocation failed (%d), trying legacy\n",
				 ua->num_irq_vectors);
			intx = true;
			ua->num_irq_vectors = pci_alloc_irq_vectors(pdev, 1, 1,
								     PCI_IRQ_LEGACY);
		}
	}
	if (ua->num_irq_vectors < 0) {
		dev_err(&pdev->dev, "no interrupt vectors available\n");
		return ua->num_irq_vectors;
	}

	/* An INTx line may be shared; the handler returns IRQ_NONE for
	 * interrupts that are not ours */
	ret = request_irq(pci_irq_vector(pdev, 0), ua_irq_handler,
			  intx ? IRQF_SHARED : 0, DRIVER_NAME, ua);
	if (ret) {
		dev_err(&pdev->dev, "failed to request IRQ: %d\n", ret);
		goto err_free_irq_vectors;
	}

	ret = ua_dsp_init_and_load(ua, false, !ua_mixer_dsp_alive(ua));
	if (ret)
		goto err_free_irq;

	/* Create character device /dev/ua_apolloN */
	devno = ida_alloc_max(&ua_ida, UA_MAX_DEVICES - 1, GFP_KERNEL);
	if (devno < 0) {
		dev_err(&pdev->dev, "no free device numbers\n");
		ret = devno;
		goto err_free_irq;
	}
	ua->dev_id = devno;

	cdev_init(&ua->cdev, &ua_fops);
	ua->cdev.owner = THIS_MODULE;
	ret = cdev_add(&ua->cdev, MKDEV(MAJOR(ua_devno), devno), 1);
	if (ret) {
		dev_err(&pdev->dev, "cdev_add failed: %d\n", ret);
		goto err_ida;
	}

	ua->dev = device_create(ua_class, &pdev->dev,
				MKDEV(MAJOR(ua_devno), devno),
				ua, "ua_apollo%d", devno);
	if (IS_ERR(ua->dev)) {
		ret = PTR_ERR(ua->dev);
		dev_err(&pdev->dev, "device_create failed: %d\n", ret);
		goto err_cdev;
	}

	/* Initialize audio subsystem (ALSA + DMA) */
	ret = ua_audio_init(ua);
	if (ret) {
		dev_err(&pdev->dev, "audio init failed: %d\n", ret);
		goto err_device;
	}

	dev_info(&pdev->dev, "registered as /dev/ua_apollo%d\n", devno);
	return 0;

err_device:
	device_destroy(ua_class, MKDEV(MAJOR(ua_devno), devno));
err_cdev:
	cdev_del(&ua->cdev);
err_ida:
	ida_free(&ua_ida, devno);
err_free_irq:
	free_irq(pci_irq_vector(pdev, 0), ua);
err_free_irq_vectors:
	pci_free_irq_vectors(pdev);
	return ret;
}

static void ua_remove(struct pci_dev *pdev)
{
	struct ua_device *ua = pci_get_drvdata(pdev);
	bool dead = !pci_device_is_present(pdev);

	dev_info(&pdev->dev, "ua_remove: surprise=%d\n", dead);

	/*
	 * 1. Stop the software that touches BAR0 on its own: a pending
	 *    resume re-init, the DSP service loop (both block until the
	 *    handler returns — they take ua->lock, so this must happen
	 *    without the lock) and the period timer (process context,
	 *    hrtimer_cancel is safe).
	 */
	cancel_work_sync(&ua->resume_work);
	ua_dsp_service_stop(ua);
	ua->audio.period_timer_running = false;
	hrtimer_cancel(&ua->audio.period_timer);

	/*
	 * 2. If the device is still present (unbind / rmmod), run the
	 *    kext Shutdown sequence BEFORE any DMA buffer is freed:
	 *    mute, transport stop, P2P off, all vectors masked (see
	 *    ua_hw_quiesce_locked()).  The transport register is cleared
	 *    as remove always did.  Under ua->lock, and the shutdown flag
	 *    is raised before the lock is dropped, so an ioctl or
	 *    pcm_prepare that was waiting for the lock cannot restart the
	 *    transport afterwards.
	 *
	 *    The flag blocks new BAR0 access via the ua_read/ua_write
	 *    guards, rejects new ioctls, and stops PCM ops.
	 */
	mutex_lock(&ua->lock);
	if (!dead) {
		ua_hw_quiesce_locked(ua);
		ua_write(ua, UA_REG_TRANSPORT, 0);
	}
	atomic_set(&ua->shutdown, 1);
	mutex_unlock(&ua->lock);

	/*
	 * 3. Give the firmware time to drain, then take bus mastering
	 *    away: from here on the device cannot DMA at all, so the SG
	 *    table and ring page registers that still point at the
	 *    buffers freed below are harmless.  (pcim's devres would
	 *    clear it too, but only after remove() has freed them.)  The
	 *    rings were left with nothing queued, so a later probe that
	 *    turns bus mastering on before reprogramming them finds no
	 *    descriptor in these pages to fetch (and neutralizes the
	 *    rings again first anyway).
	 */
	if (!dead) {
		msleep(100);
		pci_clear_master(pdev);
	}

	/*
	 * 4. Drain IRQ — free_irq waits for any in-flight handler,
	 *    then removes it.  Must happen before tearing down any
	 *    state the handler references.
	 */
	ua_irq_mask_all(ua, false);	/* caches only: flag is set */
	free_irq(pci_irq_vector(pdev, 0), ua);
	pci_free_irq_vectors(pdev);

	/*
	 * 5. A pcm_prepare or trigger that ran between step 1 and the
	 *    shutdown flag could have restarted the service loop or the
	 *    timer; both see the flag now, stop them for good.
	 */
	ua->audio.period_timer_running = false;
	hrtimer_cancel(&ua->audio.period_timer);
	ua_dsp_service_stop(ua);

	/*
	 * 6. Tear down audio subsystem — disconnects ALSA card,
	 *    frees DMA buffers.  ua_write calls inside are noops
	 *    (shutdown flag set), which is correct for surprise removal.
	 */
	ua_audio_fini(ua);

	/*
	 * 7. Free DSP ring pages and the buffers parked by timed-out
	 *    commands.  Those still unaccounted for are only safe to free
	 *    when nothing can write them: device gone, or DMA engines reset
	 *    by full_shutdown's quiesce.
	 */
	ua_dsp_rings_fini(ua, dead || full_shutdown);

	/* 8. Release plugin-chain DMA payload buffers */
	ua_dsp_free_plugin_payloads(ua);

	/* 9. Cleanup chardev and IDA */
	device_destroy(ua_class, MKDEV(MAJOR(ua_devno), ua->dev_id));
	cdev_del(&ua->cdev);
	ida_free(&ua_ida, ua->dev_id);
}

/* ----------------------------------------------------------------
 * PCIe error recovery — needed for Thunderbolt hot-plug
 * ---------------------------------------------------------------- */

static pci_ers_result_t ua_error_detected(struct pci_dev *pdev,
					  pci_channel_state_t state)
{
	struct ua_device *ua = pci_get_drvdata(pdev);

	dev_warn(&pdev->dev, "PCIe error detected (state %d)\n", state);

	/* Block all new BAR0 access */
	atomic_set(&ua->shutdown, 1);

	/* Stop DSP service loop before touching hardware state */
	ua_dsp_service_stop(ua);

	/* Mark audio as disconnected — hardware state is gone */
	ua->audio.connected = false;
	ua->audio.transport_running = false;

	/* Clear cached IRQ masks (register writes are no-ops by now) */
	ua_irq_mask_all(ua, false);

	if (state == pci_channel_io_perm_failure)
		return PCI_ERS_RESULT_DISCONNECT;

	return PCI_ERS_RESULT_NEED_RESET;
}

static pci_ers_result_t ua_slot_reset(struct pci_dev *pdev)
{
	struct ua_device *ua = pci_get_drvdata(pdev);
	int ret;

	dev_info(&pdev->dev, "slot reset\n");

	ret = pcim_enable_device(pdev);
	if (ret) {
		dev_err(&pdev->dev, "re-enable after slot reset failed: %d\n", ret);
		return PCI_ERS_RESULT_DISCONNECT;
	}

	pci_set_master(pdev);

	/* Verify device is actually reachable after re-enable */
	ua->fpga_rev = ua_read(ua, UA_REG_FPGA_REV);
	if (ua->fpga_rev == 0xFFFFFFFF) {
		dev_err(&pdev->dev,
			"device still unreachable after slot reset\n");
		return PCI_ERS_RESULT_DISCONNECT;
	}

	ua_detect_capabilities(ua);

	ret = ua_program_registers(ua);
	if (ret) {
		dev_err(&pdev->dev, "hardware re-init after slot reset failed\n");
		return PCI_ERS_RESULT_DISCONNECT;
	}

	return PCI_ERS_RESULT_RECOVERED;
}

static void ua_io_resume(struct pci_dev *pdev)
{
	struct ua_device *ua = pci_get_drvdata(pdev);

	dev_info(&pdev->dev, "I/O resumed\n");

	/* Clear shutdown and failure flags — device is back */
	atomic_set(&ua->shutdown, 0);
	ua->aceface_failed = false;

	/*
	 * After PCIe AER slot reset, the DSP service was stopped.
	 * Restart it if ACEFACE was previously done — the DSP
	 * state (plugin chains, mixer settings) survives the
	 * PCIe link reset since the Apollo stays powered.
	 */
	if (ua->aceface_done) {
		ua->audio.connected = true;
		ua_dsp_service_start(ua);
		dev_info(&pdev->dev,
			 "restarted DSP service after AER recovery\n");
	}
}

static const struct pci_error_handlers ua_err_handlers = {
	.error_detected	= ua_error_detected,
	.slot_reset	= ua_slot_reset,
	.resume		= ua_io_resume,
};

/* ----------------------------------------------------------------
 * Reboot / power-off and system sleep
 *
 * UAD2System.kext quiesces the device on every path the host stops
 * driving it: stop(), the reboot notifier (_rebootHandler) and
 * setPowerState(off) → _stopPowerGated all run CPcieDevice::Shutdown;
 * setPowerState(on) → _startPowerGated re-enables memory + bus master,
 * then runs ProgramRegisters and Connect(1).  Linux had none of this, so
 * the transport kept DMAing through reboot and suspend (issue #33:
 * freeze at login / after resume that clears when the Apollo is
 * switched off).
 * ---------------------------------------------------------------- */

/*
 * Common quiesce for .shutdown and suspend: stop the software side,
 * run the kext Shutdown sequence, drop bus mastering.  Returns with the
 * device unable to DMA or interrupt.  @sleep also parks the ALSA card in
 * D3hot until resume (not for .shutdown: nothing would ever wake it).
 */
static void ua_quiesce_device(struct ua_device *ua, const char *why,
			      bool sleep)
{
	struct pci_dev *pdev = ua->pdev;
	bool present = pci_device_is_present(pdev);

	dev_info(&pdev->dev, "%s: quiescing (present=%d)\n", why, present);

	/*
	 * Software side first, without ua->lock (the resume work and the
	 * service handler take it): an unfinished resume re-init, the DSP
	 * service loop, the period timer.
	 */
	cancel_work_sync(&ua->resume_work);
	if (sleep) {
		ua_audio_suspend(ua);
	} else {
		ua_dsp_service_stop(ua);
		ua->audio.period_timer_running = false;
		hrtimer_cancel(&ua->audio.period_timer);
	}

	mutex_lock(&ua->lock);
	ua->resume_pending = false;
	ua->resume_need_init = false;
	if (present && !atomic_read(&ua->shutdown))
		ua_hw_quiesce_locked(ua);
	/* ACEFACE does not survive the link going down; redo it on resume */
	ua->aceface_done = false;
	ua->aceface_failed = false;
	ua->audio.connected = false;
	ua->audio.transport_running = false;
	mutex_unlock(&ua->lock);

	if (present)
		pci_clear_master(pdev);
}

/*
 * .shutdown — reboot, power-off and kexec.  Leaves the device stopped so
 * nothing DMAs into memory the next kernel or firmware owns.  The mixer
 * DSP keeps running unless full_shutdown=1, so the next boot can take the
 * gentle warm path.
 */
static void ua_shutdown(struct pci_dev *pdev)
{
	struct ua_device *ua = pci_get_drvdata(pdev);

	if (!ua)
		return;

	ua_quiesce_device(ua, "shutdown", false);

	/* Nothing may touch the device any more (late ioctl, PCM op) */
	atomic_set(&ua->shutdown, 1);
}

/* System sleep / hibernation: the kext's _stopPowerGated */
static int __maybe_unused ua_suspend(struct device *dev)
{
	struct pci_dev *pdev = to_pci_dev(dev);
	struct ua_device *ua = pci_get_drvdata(pdev);

	ua_quiesce_device(ua, "suspend", true);

	/* The PCI core saves config space and picks the low-power state */
	return 0;
}

/*
 * Deferred half of resume: everything that may block for seconds — the
 * cold re-init when the mixer DSP did not survive (DMA reset, firmware
 * load, plugin chain, ACEFACE retries) and the full ACEFACE connect
 * (UA_CONNECT_RETRIES x ~1 s).  Queued on system_freezable_wq, so it runs
 * once tasks are thawed and the filesystem is up (request_firmware() is
 * also served from the firmware cache, see ua_dsp_cache_firmware()).
 * pcm_prepare fails with -EBUSY while resume_pending is set.
 */
static void ua_resume_work(struct work_struct *work)
{
	struct ua_device *ua = container_of(work, struct ua_device,
					    resume_work);
	int ret = 0;

	mutex_lock(&ua->lock);
	if (atomic_read(&ua->shutdown)) {
		ua->resume_pending = false;
		mutex_unlock(&ua->lock);
		return;
	}
	if (ua->resume_need_init) {
		ua->resume_need_init = false;
		ret = ua_dsp_init_and_load(ua, true, !ua_mixer_dsp_alive(ua));
		if (ret) {
			dev_err(&ua->pdev->dev,
				"resume: DSP re-init failed: %d\n", ret);
			/* pcm_prepare fails fast until a power cycle */
			ua->aceface_failed = true;
		}
	}
	mutex_unlock(&ua->lock);

	/* Connect(1), as at probe (takes ua->lock itself) */
	if (!ret)
		ret = ua_audio_resume(ua, UA_RESUME_CONNECT);

	mutex_lock(&ua->lock);
	ua->resume_pending = false;
	mutex_unlock(&ua->lock);

	dev_info(&ua->pdev->dev, "resume re-init done (%d)\n", ret);
}

/*
 * The kext's _startPowerGated: bus master back on, ProgramRegisters,
 * Connect(1) — here the same init path as probe (gentle warm path when
 * the mixer DSP survived, cold boot with firmware reload when it did
 * not), reusing the DMA and DSP rings allocated at probe.
 *
 * Kept fast: only the warm path runs here (register reprogramming, the
 * <= 300 ms liveness check and the <= 1 s early ACEFACE handshake).  A cold
 * re-init, or a connect that needs the full ACEFACE loop, is deferred to
 * ua_resume_work().
 */
static int __maybe_unused ua_resume(struct device *dev)
{
	struct pci_dev *pdev = to_pci_dev(dev);
	struct ua_device *ua = pci_get_drvdata(pdev);
	bool defer = false;
	u32 fpga;
	int ret = 0;

	if (atomic_read(&ua->shutdown)) {
		/* Error recovery / removal owns the device; only reopen
		 * the ALSA controls so nothing waits on D0 forever */
		ua_audio_resume(ua, UA_RESUME_CONTROLS);
		return 0;
	}

	fpga = ua_read(ua, UA_REG_FPGA_REV);
	if (fpga == 0xFFFFFFFF) {
		/* Unplugged or switched off while asleep; hotplug removes it */
		dev_warn(dev, "resume: device not reachable, skipping re-init\n");
		ret = -ENODEV;
		goto fail;
	}
	if (fpga != ua->fpga_rev) {
		dev_err(dev, "resume: FPGA revision 0x%08x, expected 0x%08x\n",
			fpga, ua->fpga_rev);
		ret = -EIO;
		goto fail;
	}

	mutex_lock(&ua->lock);

	/*
	 * ProgramRegisters front half, before bus mastering comes back:
	 * transport stopped, interrupts masked and cleared, nothing queued
	 * on any DSP ring (the quiesce left none; stale entries a power
	 * glitch could have produced are dropped too).
	 */
	ua_hw_stop_xport_irqs(ua);
	ua_dsp_rings_neutralize(ua);
	pci_set_master(pdev);

	if (ua_mixer_dsp_alive(ua)) {
		/* Warm: SG + ring reprogramming only */
		ret = ua_dsp_init_and_load(ua, true, false);
		if (ret)
			dev_err(dev, "resume: re-init failed: %d\n", ret);
	} else {
		dev_info(dev, "resume: mixer DSP not running, cold re-init deferred\n");
		ua->resume_need_init = true;
		defer = true;
	}
	if (defer || !ret)
		ua->resume_pending = true;
	mutex_unlock(&ua->lock);

	if (ret)
		goto fail;

	/* Connect(1) when it is quick; otherwise the work does the full one */
	if (!defer && ua_audio_resume(ua, UA_RESUME_CONNECT_FAST) != -EAGAIN) {
		mutex_lock(&ua->lock);
		ua->resume_pending = false;
		mutex_unlock(&ua->lock);
		dev_info(dev, "resumed (warm)\n");
		return 0;
	}

	if (defer)
		ua_audio_resume(ua, UA_RESUME_CONTROLS);
	queue_work(system_freezable_wq, &ua->resume_work);
	dev_info(dev, "resumed; %s deferred to a worker\n",
		 defer ? "DSP re-init and connect" : "the full ACEFACE connect");
	return 0;

fail:
	/* Skip the connect and make pcm_prepare fail fast until the
	 * Apollo is power-cycled; reopen the ALSA controls regardless */
	ua->aceface_failed = true;
	ua_audio_resume(ua, UA_RESUME_CONTROLS);
	dev_info(dev, "resumed (%d)\n", ret);

	/* A device that vanished during sleep is hotplug's business */
	return ret == -ENODEV ? 0 : ret;
}

/* DEFINE_SIMPLE_DEV_PM_OPS / pm_sleep_ptr() appeared in 5.17 */
#ifdef DEFINE_SIMPLE_DEV_PM_OPS
static DEFINE_SIMPLE_DEV_PM_OPS(ua_pm_ops, ua_suspend, ua_resume);
#else
static SIMPLE_DEV_PM_OPS(ua_pm_ops, ua_suspend, ua_resume);
#endif
#ifndef pm_sleep_ptr
#define pm_sleep_ptr(_ptr)	(IS_ENABLED(CONFIG_PM_SLEEP) ? (_ptr) : NULL)
#endif

/* ----------------------------------------------------------------
 * Module init / exit
 * ---------------------------------------------------------------- */

static struct pci_driver ua_pci_driver = {
	.name		= DRIVER_NAME,
	.id_table	= ua_pci_ids,
	.probe		= ua_probe,
	.remove		= ua_remove,
	.shutdown	= ua_shutdown,
	.err_handler	= &ua_err_handlers,
	.driver.pm	= pm_sleep_ptr(&ua_pm_ops),
};

static int __init ua_init(void)
{
	int ret;

	ret = alloc_chrdev_region(&ua_devno, 0, UA_MAX_DEVICES, DRIVER_NAME);
	if (ret)
		return ret;

	ua_class = ua_class_create(DRIVER_NAME);
	if (IS_ERR(ua_class)) {
		ret = PTR_ERR(ua_class);
		goto err_chrdev;
	}

	ret = pci_register_driver(&ua_pci_driver);
	if (ret)
		goto err_class;

	pr_info(DRIVER_NAME ": " DRIVER_DESC " v" DRIVER_VERSION "\n");
	return 0;

err_class:
	class_destroy(ua_class);
err_chrdev:
	unregister_chrdev_region(ua_devno, UA_MAX_DEVICES);
	return ret;
}

static void __exit ua_exit(void)
{
	pci_unregister_driver(&ua_pci_driver);
	class_destroy(ua_class);
	unregister_chrdev_region(ua_devno, UA_MAX_DEVICES);
}

module_init(ua_init);
module_exit(ua_exit);

MODULE_LICENSE("GPL");
MODULE_DESCRIPTION(DRIVER_DESC);
MODULE_VERSION(DRIVER_VERSION);
MODULE_AUTHOR("apollo-linux contributors");
