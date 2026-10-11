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

static bool mixer_fw_stage;
module_param(mixer_fw_stage, bool, 0644);
MODULE_PARM_DESC(mixer_fw_stage,
		 "On a cold start, send ua-apollo-mixer.bin to DSP 0 as 0x12 firmware transactions (default: off, as the macOS driver, which sends nothing at start; the x4 answers every block with DSP status 0xF0010006 and cold-starts the same without it). 1 restores the stage");

static bool warm_transport0;
module_param(warm_transport0, bool, 0444);
MODULE_PARM_DESC(warm_transport0,
		 "Write TRANSPORT (0x2220) = 0 at every probe/resume, also with a live mixer DSP, as the macOS MapHardware does (default: off; a warm probe leaves the previous owner's value)");

bool ua_kext_connect = true;
module_param_named(kext_connect, ua_kext_connect, bool, 0444);
MODULE_PARM_DESC(kext_connect,
		 "Ring-connect every device the macOS driver does, chosen by its public device type (every v2 Apollo, the x4 included): per-DSP {0x230002,1} plus the dated finalize after the ACEFACE handshake on every start, and the date-only disconnect at stop; supersedes v12_connect (default: on; titan's x4 cold- and warm-starts the same with it. 0 = the old choice by serial type, which never ring-connects the x4)");

static bool v12_connect;
module_param(v12_connect, bool, 0444);
MODULE_PARM_DESC(v12_connect,
		 "EXPERIMENTAL, needs Solo/Arrow/x8p testers: use the v12 ring-connect device set (adds Solo TB, Arrow, type 0x29) and do the ACEFACE handshake before the ring connect (default: off)");

/*
 * Mixer settings are an edge-triggered mailbox, as CPcieDeviceMixer::
 * _writeSetting (kext 0x4b454) treats them: a changed setting is written
 * with its mask once, its mask is cleared, and on the next flush the
 * setting is written as zeros once.  Re-sending every field ever touched
 * on every flush (the old Linux behaviour, mixer_mailbox=0) left PAD, 48V
 * and Mic/Line dead: the DSP never reported them and the relays never
 * moved, while LowCut and Phase worked.  With the mailbox all five follow
 * the host, the front-panel buttons and the monitor knob keep working
 * between host writes instead of being overwritten on the next flush,
 * and gain, monitor level/mute/mono/dim and talkback behave as before
 * (x4 on titan, 2026-10-10).
 */
static bool mixer_mailbox = true;
module_param(mixer_mailbox, bool, 0644);
MODULE_PARM_DESC(mixer_mailbox,
		 "Write mixer settings as the macOS driver does: each change once with its mask, then zeros once (default: on); 0 = re-send every touched field on every flush (old behaviour, PAD/48V/Mic-Line do not work)");

/*
 * UAD-2 Satellite Thunderbolt (device 0x0002, subsystems 0x0A/0x0B): the
 * macOS kext forces legacy INTx for them, the Windows INF ships
 * MSISupported=1 (parity/18 B12, W4).  Windows is the newer driver and
 * the INF is an explicit decision, so MSI is the default; the Apollo x4
 * (and every other model) is not affected either way.
 */
static bool satellite_msi = true;
module_param(satellite_msi, bool, 0444);
MODULE_PARM_DESC(satellite_msi,
		 "Use MSI instead of legacy INTx for the UAD-2 Satellite Thunderbolt (PCI subsystem 0x0A/0x0B), as the Windows driver does; 0 = INTx as the macOS driver (default: 1; no effect on Apollos)");

/*
 * Unit group (K1, parity/15): every probed unit is in one group for now.
 * The master is the first registered unit; when it is removed the next in
 * registration order takes over (the kext's _removeAudioDevice promotes
 * extension [0]).  Nothing acts on the role yet, it is reported only.
 */
static LIST_HEAD(ua_units);
static DEFINE_MUTEX(ua_units_lock);

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
	bool wake;

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
	status_lo = ua_read(ua, UA_REG_IRQ_STATUS);

	/*
	 * All-ones: the device dropped off the bus (surprise unplug before
	 * remove raised the shutdown flag).  Not ours, and acking it would
	 * only feed a shared INTx line spurious work.
	 */
	if (status_lo == 0xFFFFFFFF) {
		spin_unlock_irqrestore(&ua->irq_lock, flags);
		return IRQ_NONE;
	}
	status_lo &= ua->irq_mask_lo;

	if (ua_irq64(ua))
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

	/* First interrupt since request_irq: the probe self-test watches it */
	if (!atomic64_read(&ua->irq_first_ns))
		atomic64_cmpxchg(&ua->irq_first_ns, 0, ktime_get_ns());

	/* Dispatch audio-related interrupts */
	wake = ua_audio_irq(ua, status_lo, status_hi);

	/* DSP message FIFO / error vectors (dsp_msg_irq, default off) */
	ua_dsp_irq(ua, status_lo, status_hi);

	/* Notification vector: run the DSP service pass now (notif_irq) */
	ua_audio_notif_irq(ua, status_hi);

	return wake ? IRQ_WAKE_THREAD : IRQ_HANDLED;
}

/*
 * Threaded half: the period wakeups of period_irq=1 streams.  They read
 * SAMPLE_POS (BAR0), which stays out of hard-IRQ context; the device is
 * already acked by ua_irq_handler(), so no IRQF_ONESHOT.
 */
static irqreturn_t ua_irq_thread(int irq, void *data)
{
	ua_audio_irq_thread(data);
	return IRQ_HANDLED;
}

/* Forward declarations */
static void ua_read_serial_type(struct ua_device *ua);
static u32 ua_compute_public_type(struct ua_device *ua);
static void ua_resolve_public_model(struct ua_device *ua);
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
 * CPcieIntrManager::EnableDMA(idx) (kext x86 0x9d5d): set the one enable
 * bit of engine @idx (bit idx + 1) and write DMA_CTRL.  Strobes are not
 * kept on the way in (the kext cache never holds any), so a strobe left
 * asserted by an earlier all-bits write is dropped here.
 */
void ua_dma_enable_engine(struct ua_device *ua, unsigned int idx)
{
	u32 v;

	if (idx >= 8)
		return;
	v = (ua_read(ua, UA_REG_DMA_CTRL) & ~UA_DMA_RESET_V2_MASK) |
	    BIT(idx + 1);
	ua_write(ua, UA_REG_DMA_CTRL, v);
	ua->dma_ctrl_cache = v;
}

/*
 * CPcieIntrManager::ResetDMAEngine(idx) (kext x86 0x9a98, also
 * CPcieDSP::ResetDSP): clear engine @idx's enable bit, pulse its strobe
 * (bit idx + 9), read back as a flush.  Leaves the engine disabled; the
 * caller re-enables it (ua_dma_enable_engine()) once its ring is
 * reprogrammed, as CPcieDSP::ProgramRegisters does.
 */
void ua_dma_reset_engine(struct ua_device *ua, unsigned int idx)
{
	u32 v;

	if (idx >= 8)
		return;
	v = ua_read(ua, UA_REG_DMA_CTRL) &
	    ~(UA_DMA_RESET_V2_MASK | BIT(idx + 1));

	ua_write(ua, UA_REG_DMA_CTRL, v | BIT(idx + 9));	/* Assert */
	ua_write(ua, UA_REG_DMA_CTRL, v);			/* Deassert */
	(void)ua_read(ua, UA_REG_DMA_CTRL);			/* Flush */
	ua->dma_ctrl_cache = v;
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

	if (ua_irq64(ua))
		ua_write(ua, UA_REG_EXT_IRQ_STATUS, UA_IRQ_CLEAR_ALL);
}

/*
 * Detect firmware version, device type, and DSP count.
 *
 * Mirrors CPcieDevice::Name() and GetCapabilities():
 *   - FPGA rev (0x2218) bit 31 clear → v1 firmware, type from bits[31:28]-1
 *   - FPGA rev bit 31 set → v2 firmware, read ext_caps (0x2234):
 *       bits[25:20] - 1 = device family index
 *       bits[15:8]      = DSP count (at most 8)
 *   - v1 DSP count: table on FPGA rev bits[31:28], see below
 *   - Some v2 devices need serial prefix (0x20-0x2C) to get exact type
 */
static int ua_detect_capabilities(struct ua_device *ua)
{
	u32 fpga = ua->fpga_rev;
	u32 ext_caps;

	/*
	 * Offline detector: GetCapabilities (kext x86 0x464c) treats an
	 * FPGA_REV or ext_caps read of 0xFFFFFFFF as "device gone" and
	 * MapHardware fails (-0x5c) before anything is touched.  0xFFFFFFFF
	 * has bit 31 set, so without this the code below would take the v2
	 * path with garbage capabilities.
	 */
	if (fpga == 0xFFFFFFFF) {
		dev_err(&ua->pdev->dev,
			"FPGA_REV reads 0xFFFFFFFF: device offline\n");
		return -ENODEV;
	}

	if ((s32)fpga >= 0) {
		/* v1 firmware: type from FPGA rev bits[31:28] - 1 */
		ua->fw_v2 = false;
		ua->device_type = (fpga >> 28) - 1;

		/*
		 * v1 DSP count: CPcieDevice::_allocateDSPs (kext 12.0.0 x86
		 * 0x7678) indexes a table by FPGA_REV[31:28], valid for 1-4:
		 * {1: 1, 2: 2, 3: 4, 4: 1}.  Any other nibble means no DSPs and
		 * an error log.  The PCI subsystem id plays no part.
		 */
		switch (fpga >> 28) {
		case 1:
		case 4:
			ua->num_dsps = 1;
			break;
		case 2:
			ua->num_dsps = 2;
			break;
		case 3:
			ua->num_dsps = 4;
			break;
		default:
			ua->num_dsps = 0;
			dev_err(&ua->pdev->dev,
				"v1 firmware: FPGA_REV[31:28]=%u has no DSP count\n",
				fpga >> 28);
			break;
		}
	} else {
		/* v2 firmware */
		ua->fw_v2 = true;
		ext_caps = ua_read(ua, UA_REG_EXT_CAPS);
		if (ext_caps == 0xFFFFFFFF) {
			dev_err(&ua->pdev->dev,
				"ext_caps reads 0xFFFFFFFF: device offline\n");
			return -ENODEV;
		}
		ua->ext_caps = ext_caps;
		ua->num_dsps = (ext_caps >> 8) & 0xFF;

		/*
		 * _allocateDSPs (kext x86 0x7678) refuses a DSP count of 9 or
		 * more (no Apollo has that many; the field is garbage).  The
		 * driver used to clamp it to UA_MAX_DSPS and carry on.
		 */
		if (ua->num_dsps > UA_MAX_DSPS) {
			dev_err(&ua->pdev->dev,
				"ext_caps 0x%08x reports %u DSPs (max %u): not an Apollo\n",
				ext_caps, ua->num_dsps, UA_MAX_DSPS);
			return -ENODEV;
		}

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

	ua->public_type = ua_compute_public_type(ua);
	ua_resolve_public_model(ua);
	return 0;
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
static u32 ua_serial_lookup(struct ua_device *ua, bool verbose)
{
	char serial[UA_REG_SERIAL_LEN + 1];
	u32 regs[4];
	int i;

	for (i = 0; i < 4; i++)
		regs[i] = ua_read(ua, UA_REG_SERIAL_BASE + i * 4);

	memcpy(serial, regs, UA_REG_SERIAL_LEN);
	serial[UA_REG_SERIAL_LEN] = '\0';

	if (verbose)
		dev_info(&ua->pdev->dev, "serial: %.16s\n", serial);

	/* Match the 4-char model prefix at digits 5-8 against the lookup table */
	for (i = 0; i < ARRAY_SIZE(ua_serial_table); i++)
		if (!strncmp(serial + 4, ua_serial_table[i].prefix, 4))
			return ua_serial_table[i].device_type;

	return 0;
}

static void ua_read_serial_type(struct ua_device *ua)
{
	ua->device_type = ua_serial_lookup(ua, true);
	if (!ua->device_type)
		dev_warn(&ua->pdev->dev,
			 "unknown serial prefix, type unresolved\n");
}

/*
 * CUAD2Device::_deviceTypeFromFPGAVersion (kext x86 0x14528): a new-format
 * FPGA_REV ((rev & 0x60) == 0x20) carries the serial-table type in bits
 * [30:24] (titan's x4: 0x9f0ea929 -> 0x1F), an old one only rev >> 28.
 */
static u32 ua_type_from_fpga_rev(u32 rev)
{
	return (rev & 0x60) == 0x20 ? (rev >> 24) & 0x7f : rev >> 28;
}

/*
 * ua_compute_public_type - the kext's UAD2DeviceType (CPcieDevice::
 * GetCapabilities, x86 0x464c; parity/11 rows B5 and section 2.3).
 *
 * v1 firmware: 0.  v2: the ext_caps family ((ext_caps >> 20) & 0x3f) - 1
 * indexes a 22-entry jump table at 0x4d5c (families 20 and 21 give 0,
 * family 22 the x16D's 0x20); the entries for the x-series families are
 * then refined by the serial-table type (_deviceTypeFromSerialNumber,
 * tables at 0x4db4, 0x4de4, 0x4dd4) and, for QUAD and HEXA, by the
 * FPGA_REV type when the serial does not decide.  HEXA serial 0x35 (x16D)
 * gives 0x21, not 0x20: the kext stores 0x20 and falls through into the
 * 0x37 case (x86 0x4a30, arm64e 0xcda8), so the value is mirrored; an
 * x16D on its own family 22 gets 0x20.  The serial is read independently
 * of device_type, which the Solo and 8p subsystem pins override.
 */
static u32 ua_compute_public_type(struct ua_device *ua)
{
	static const u8 family_map[] = {
		0x05, 0x04, 0x00, 0x07, 0x08, 0x0B, 0x04, 0x0C, 0x0D, 0x0F,
		0x0E, 0x00, 0x00, 0x12, 0x13, 0x1A, 0x16, 0x18, 0x19, 0x00,
		0x00, 0x20,
	};
	u32 fam, base, st, ft;

	if (!ua->fw_v2)
		return 0;

	fam = (ua->ext_caps >> 20) & 0x3f;
	if (fam == 0x3f)
		return UA_PUBTYPE_NONE;
	if (fam == 0 || fam > ARRAY_SIZE(family_map))
		return 0;

	base = family_map[fam - 1];
	if (base != 0x13 && base != 0x16 && base != 0x19 && base != 0x1A)
		return base;

	st = ua_serial_lookup(ua, false);
	ft = ua_type_from_fpga_rev(ua->fpga_rev);

	switch (base) {
	case 0x13:
		if (st == UA_DEV_SERIAL_2016)
			return 0x13;
		if (st == UA_DEV_SERIAL_2110)
			return 0x27;
		return 0x1C;		/* serial 2032 or any other */
	case 0x1A:
		if (st != UA_DEV_APOLLO_X4 && st != UA_DEV_APOLLO_X4_GEN2 &&
		    st != UA_DEV_APOLLO_X4_GEN2_TB4)
			st = ft;
		if (st == UA_DEV_APOLLO_X4)
			return 0x1A;
		if (st == UA_DEV_APOLLO_X4_GEN2)
			return 0x26;
		if (st == UA_DEV_APOLLO_X4_GEN2_TB4)
			return 0x28;
		return UA_PUBTYPE_NONE;
	case 0x16:
		if (st >= UA_DEV_APOLLO_X6 && st <= UA_DEV_APOLLO_X16)
			return 0x14 + (st - UA_DEV_APOLLO_X6);
		if (st == UA_DEV_APOLLO_X16D)
			return 0x21;	/* the kext's fall-through, see above */
		if (st >= UA_DEV_APOLLO_X6_GEN2 &&
		    st <= UA_DEV_APOLLO_X16_GEN2)
			return 0x21 + (st - UA_DEV_APOLLO_X6_GEN2);
		return ft == UA_DEV_APOLLO_X16D ? 0x20 : 0x16;
	default:			/* 0x19 */
		if (st == UA_DEV_APOLLO_SOLO || st == UA_DEV_ARROW ||
		    st == 0x26 || st == 0x2E || st == 0x2F)
			return 0x19;
		if (st == UA_DEV_APOLLO_TWIN_X_GEN2 ||
		    st == UA_DEV_APOLLO_TWIN_X_GEN2_B || st == 0x3D)
			return 0x25;
		if (st == UA_DEV_APOLLO_TWIN_X_GEN2_TB4 ||
		    st == UA_DEV_APOLLO_TWIN_X_GEN2_TB4_B)
			return 0x29;
		return 0x19;
	}
}

/*
 * Models the serial table cannot name (the pre-x Apollos, the Twin family,
 * the Rack) are named by the public type, as the kext and UA's Mixer
 * Engine do: GetCapabilities gives each its own ext_caps family and type
 * (Apollo 5, Apollo 16 7, Twin 8/0xF, Rack 0xB, Apollo 8 0xC, Apollo 16
 * MkII 0xD, 8p 0xE, Twin MkII 0x12, Twin X 0x19) and CUAD2Info::CardType
 * (arm64 ME 0x1002b5ef0) picks the mixer class from it.  Such a device gets
 * device type UA_DEV_PUBLIC(public type), which devices/<model>.json lists;
 * a type >= 0x80 keeps every path an unresolved type 0 takes (AudioExtension
 * connect, no legacy DMA row, no output mute).
 *
 * Serials 2024/2025 (0x27/0x28) on the Twin X family (public 0x19) are Twin
 * X units, not a Solo/Arrow: the kext refines family 19 to 0x19 for them
 * (x86 0x4b03) and the Mixer Engine's card types for 0x19 are the Lavern
 * (Twin X) class.  Their legacy DMA row (3/2) is the 3/2 fallback, so only
 * the name and the preamp counts change.
 *
 * On the Conrad family (ext_caps family 15) the public type names the
 * product too: 0x13 (serial 2016) is the Arrow (card type 31), 0x1C
 * (serial 2032 or other) and 0x27 (serial 2110) the Solo (cards 32/54).
 * The serial-table types 0x1E/0x2A/0x3E there, which the driver never
 * named, and the 0x000F subsystem pin on an Arrow (public 0x13) take
 * UA_DEV_PUBLIC(public type); a pinned Solo keeps 0x27.  Their legacy DMA
 * rows (0x1E 24/22, 0x2A 34/34, frozen from wrong names) become the 3/2
 * fallback the Solo row has; the descriptor layout (io_map default) is
 * not affected.
 */
static void ua_resolve_public_model(struct ua_device *ua)
{
	u32 pt = ua->public_type, dt = ua->device_type;
	const char *why = NULL;

	if (pt == 0 || pt == UA_PUBTYPE_NONE || pt >= UA_PUBLIC_DEVTYPE_BASE)
		return;
	if (pt == 0x19 && (dt == UA_DEV_APOLLO_SOLO || dt == UA_DEV_ARROW))
		why = "the Twin X family (public type 0x19): Twin X";
	else if ((pt == 0x13 || pt == 0x1C || pt == 0x27) &&
		 (dt == UA_DEV_SERIAL_2016 || dt == UA_DEV_SERIAL_2032 ||
		  dt == UA_DEV_SERIAL_2110 ||
		  (pt == 0x13 && dt == UA_DEV_APOLLO_SOLO)))
		why = "the Conrad family: Arrow (0x13) or Solo (0x1C/0x27)";
	if (why) {
		dev_info(&ua->pdev->dev,
			 "device type 0x%02x on %s, public type 0x%02x\n",
			 dt, why, pt);
		ua->device_type = UA_DEV_PUBLIC(pt);
		return;
	}
	if (!ua->device_type) {
		ua->device_type = UA_DEV_PUBLIC(pt);
		dev_info(&ua->pdev->dev,
			 "no serial-table type: device type 0x%02x from public type 0x%02x\n",
			 ua->device_type, pt);
	}
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
 * dsp_status_check: what to do with the DSP status fault bits.
 *   0 = do not read the windows; 1 = read and log (default);
 *   2 = also fail probe after the connect, as the kext does.
 * The kext fails start on a fault (-21): _checkDSPs is the tail of
 * Connect(), and _allocateDSPs applies the same test to DSP 0 in
 * MapHardware.  The healthy values of the two bit groups (low 7 and bit
 * 23) are not known for the x4 yet, so the fatal mode is opt-in until
 * titan has logged them (parity/11 row E4).
 */
static unsigned int dsp_status_check = 1;
module_param(dsp_status_check, uint, 0444);
MODULE_PARM_DESC(dsp_status_check,
		 "DSP status word check (kext _checkDSPs, fault mask 0x80007F): 0 = off, 1 = log (default), 2 = fail probe on a fault as the macOS driver does");

#define UA_DSP_FAULT_MASK	0x0080007FU
#define UA_DSP_SLAVE_MASK	0x1C00U

/* DSP n's register window: n * 0x800, +0x2000 above DSP 3 (CPcieDSP::ProgramRegisters) */
static u32 ua_dsp_window(unsigned int idx)
{
	return idx * 0x800 + (idx > 3 ? 0x2000 : 0);
}

/*
 * Mirrors CPcieDevice::_checkDSPs() (kext x86 0x61bc): word 0 of every
 * DSP's register window; a word & 0x80007F is a fault (-21 there, -EIO
 * here).  The old code read the ring register block instead, which is
 * unrelated.  Only the @final call (after the connect, where the kext
 * runs it) can fail, and only with dsp_status_check=2.
 */
static int ua_check_dsps(struct ua_device *ua, bool final)
{
	unsigned int i, faults = 0;

	if (!dsp_status_check || !ua->num_dsps)
		return 0;

	for (i = 0; i < ua->num_dsps; i++) {
		u32 off = ua_dsp_window(i);
		u32 val;

		if ((resource_size_t)off + 4 > ua->regs_size) {
			dev_dbg(&ua->pdev->dev,
				"DSP %u: window 0x%04x is outside BAR0\n",
				i, off);
			continue;
		}

		val = ua_read(ua, off);
		if (val & UA_DSP_FAULT_MASK)
			faults++;
		dev_info(&ua->pdev->dev,
			 "DSP %u status (%s): window 0x%04x word0 = 0x%08x%s\n",
			 i, final ? "after connect" : "early", off, val,
			 (val & UA_DSP_FAULT_MASK) ? " (fault bits set)" : "");
		if (ua->num_dsps == 1 && (val & UA_DSP_SLAVE_MASK))
			dev_warn(&ua->pdev->dev,
				 "single-DSP device reports slave bits 0x%x\n",
				 val & UA_DSP_SLAVE_MASK);
	}

	if (final && faults && dsp_status_check >= 2) {
		dev_err(&ua->pdev->dev,
			"%u DSP(s) report a fault (mask 0x%x): failing as the macOS driver does\n",
			faults, UA_DSP_FAULT_MASK);
		return -EIO;
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
	ua_check_dsps(ua, false);

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

	/*
	 * CPcieDevice::MapHardware's first write (kext x86 0x72ca) is
	 * TRANSPORT = 0, on every start.  The cold path always does it
	 * (ua_program_registers); a warm one only with warm_dma_reset, so
	 * this makes it unconditional on request (parity/11 row B1).
	 */
	if (warm_transport0) {
		dev_info(&ua->pdev->dev,
			 "warm_transport0: TRANSPORT 0x%08x -> 0\n",
			 ua_read(ua, UA_REG_TRANSPORT));
		ua_write(ua, UA_REG_TRANSPORT, 0);
	}

	ua_irq_mask_all(ua, true);
}

/* ----------------------------------------------------------------
 * Character device file operations
 * ---------------------------------------------------------------- */

static int ua_mixer_kind(const struct ua_device *ua);

/* ----------------------------------------------------------------
 * Unit group (K1)
 * ---------------------------------------------------------------- */

static void ua_unit_register(struct ua_device *ua)
{
	struct ua_device *master;
	unsigned int idx = 0;
	struct ua_device *u;

	mutex_lock(&ua_units_lock);
	list_add_tail(&ua->unit_node, &ua_units);
	master = list_first_entry(&ua_units, struct ua_device, unit_node);
	list_for_each_entry(u, &ua_units, unit_node)
		if (u == ua)
			break;
		else
			idx++;
	dev_info(&ua->pdev->dev, "unit group 0: unit %u, %s\n", idx,
		 master == ua ? "master" : "follower");
	mutex_unlock(&ua_units_lock);
}

/* Leave the group; the next unit in registration order becomes master */
static void ua_unit_unregister(struct ua_device *ua)
{
	mutex_lock(&ua_units_lock);
	if (!list_empty(&ua->unit_node)) {
		bool was_master = list_first_entry(&ua_units, struct ua_device,
						   unit_node) == ua;

		list_del_init(&ua->unit_node);
		dev_info(&ua->pdev->dev, "left unit group 0%s\n",
			 was_master ? " (was master)" : "");
		if (was_master && !list_empty(&ua_units)) {
			struct ua_device *m = list_first_entry(&ua_units,
							       struct ua_device,
							       unit_node);

			dev_info(&m->pdev->dev,
				 "unit group 0: now master\n");
			ua_event_post(m, UA_EVT_CLOCK_MASTER, 0);
		}
	}
	mutex_unlock(&ua_units_lock);
}

static void ua_unit_info(struct ua_device *ua, u32 *index, u32 *count,
			 u32 *role)
{
	struct ua_device *u;
	u32 i = 0, n = 0;

	*index = 0xFFFFFFFFu;
	*count = 0;
	*role = UA_UNIT_ROLE_NONE;

	mutex_lock(&ua_units_lock);
	if (!list_empty(&ua->unit_node)) {
		list_for_each_entry(u, &ua_units, unit_node) {
			if (u == ua)
				*index = i;
			i++;
			n++;
		}
		*count = n;
		*role = *index == 0 ? UA_UNIT_ROLE_MASTER :
				      UA_UNIT_ROLE_FOLLOWER;
	}
	mutex_unlock(&ua_units_lock);
}

/* ----------------------------------------------------------------
 * Pause reasons (K6/F12)
 * ---------------------------------------------------------------- */

void ua_pause_set(struct ua_device *ua, u32 reason)
{
	u32 old = atomic_fetch_or(reason, &ua->pause_mask);

	if (!(old & reason))
		ua_event_post(ua, UA_EVT_PAUSED, reason);
}

void ua_pause_clear(struct ua_device *ua, u32 reason)
{
	u32 old = atomic_fetch_andnot(reason, &ua->pause_mask);

	if (old & reason)
		ua_event_post(ua, UA_EVT_RESUMED, old & reason);
}

static long ua_ioctl_get_version(struct ua_device *ua, unsigned long arg)
{
	struct ua_version v = {
		.abi_major = UA_ABI_MAJOR,
		.abi_minor = UA_ABI_MINOR,
		.driver_version = 0x000200,	/* DRIVER_VERSION 0.2.0 */
		.features = UA_FEAT_EVENTS | UA_FEAT_PUBLIC_TYPE |
			    UA_FEAT_BUS_PARAM_SIDE | UA_FEAT_MIXER_PARAM_T0 |
			    UA_FEAT_KIND_GATES | UA_FEAT_MIXER_STATE |
			    UA_FEAT_SYSTEM_INFO | UA_FEAT_BUS_PARAM_BATCH |
			    UA_FEAT_PAUSE_INFO | UA_FEAT_PAUSE_EVENTS |
			    UA_FEAT_UNIT_GROUP | UA_FEAT_SYSINFO_EXT |
			    UA_FEAT_PLUGIN_CAPS | UA_FEAT_FW_RELEASE |
			    UA_FEAT_KEXT_DRIVER_PARAMS | UA_FEAT_DSP_LOAD |
			    UA_FEAT_CLOCK_SOURCES,
	};

	/*
	 * The solo-mute flag only mutes when bus_solo_sign is on; a client
	 * that relied on it with the knob off would leave soloed-out inputs
	 * audible, so the bit is offered only while the driver honours it.
	 */
	if (ua_dsp_solo_flag_honoured())
		v.features |= UA_FEAT_BUS_SOLO_FLAG;

	strscpy(v.driver_name, DRIVER_VERSION, sizeof(v.driver_name));
	if (copy_to_user((void __user *)arg, &v, sizeof(v)))
		return -EFAULT;
	return 0;
}

/*
 * UAD2DeviceInfo+0x80 as CPcieDevice::GetDeviceInfo (kext x86 0x5156)
 * fills it: BAR0 0x01AC for the public types in UA_DEVINFO_TYPES,
 * 0xFFFFFFFF for every other type.
 */
static u32 ua_read_dev_info(struct ua_device *ua)
{
	u32 pt = ua->public_type;

	if (pt > 0x29 || !((UA_DEVINFO_TYPES >> pt) & 1))
		return 0xFFFFFFFFu;
	return ua_read(ua, UA_REG_DEV_INFO);
}

static long ua_ioctl_get_system_info(struct ua_device *ua, unsigned long arg)
{
	struct ua_system_info si = {};
	struct ua_audio *audio = &ua->audio;
	u32 words[UA_REG_SERIAL_LEN / 4];
	char *ser = si.serial;
	unsigned int i;
	u16 lnksta;
	int kind;

	si.fpga_rev = ua->fpga_rev;
	si.device_type = ua->device_type;
	si.public_type = ua->public_type;
	si.subsystem_id = ua->subsystem_id;
	si.pci_device = ua->pdev->device;
	si.num_dsps = ua->num_dsps;
	si.fw_version = ua_read(ua, UA_REG_FW_VERSION);
	si.fw_release = ua_read(ua, UA_REG_FW_RELEASE);
	si.dev_info = ua_read_dev_info(ua);
	si.fw_v2 = ua->fw_v2 ? 1 : 0;
	si.ext_caps = ua_read(ua, UA_REG_EXT_CAPS);
	si.hw_uid_lo = ua_read(ua, UA_REG_HW_UID_LO);
	si.hw_uid_hi = ua_read(ua, UA_REG_HW_UID_HI);
	kind = ua_mixer_kind(ua);
	si.mixer_kind = kind < 0 ? 0xFFFFFFFFu : kind;

	/* Same bytes as ua_serial_lookup(): 16 ASCII chars at BAR0 0x20 */
	for (i = 0; i < ARRAY_SIZE(words); i++)
		words[i] = ua_read(ua, UA_REG_SERIAL_BASE + i * 4);
	memcpy(ser, words, sizeof(si.serial));
	for (i = 0; i < sizeof(si.serial); i++)
		if (!ser[i]) {
			memset(ser + i, 0, sizeof(si.serial) - i);
			break;
		}

	mutex_lock(&ua->lock);
	si.sample_rate = audio->sample_rate;
	si.rate_index = audio->rate_index;
	si.clock_source = audio->clock_source;
	/* Readback word 6: bit 31 = locked, valid when bit 30 is set */
	if (ua->mixer_rb_seen && (ua->mixer_rb_data[6] & BIT(30))) {
		si.clock_lock_valid = 1;
		si.clock_locked = !!(ua->mixer_rb_data[6] & BIT(31));
	}
	si.connected = audio->connected ? 1 : 0;
	si.transport_running = audio->transport_running ? 1 : 0;
	si.play_channels = audio->play_channels;
	si.rec_channels = audio->rec_channels;
	si.clock_src_mask = audio->clock_src_mask;
	si.fw_rate_mask = audio->fw_rate_mask;
	mutex_unlock(&ua->lock);

	if (!pcie_capability_read_word(ua->pdev, PCI_EXP_LNKSTA, &lnksta)) {
		si.pcie_link_speed = lnksta & PCI_EXP_LNKSTA_CLS;
		si.pcie_link_width = (lnksta & PCI_EXP_LNKSTA_NLW) >>
				     PCI_EXP_LNKSTA_NLW_SHIFT;
	}

	si.unit_group = 0;
	ua_unit_info(ua, &si.unit_index, &si.unit_count, &si.unit_role);

	if (copy_to_user((void __user *)arg, &si, sizeof(si)))
		return -EFAULT;
	return 0;
}

static long ua_ioctl_get_pause_info(struct ua_device *ua, unsigned long arg)
{
	struct ua_pause_info pi = {};

	pi.reasons = atomic_read(&ua->pause_mask);
	if (!READ_ONCE(ua->audio.transport_running))
		pi.reasons |= UA_PAUSE_IDLE;
	pi.can_stream = !(pi.reasons & (UA_PAUSE_PHASE | UA_PAUSE_RATE));
	pi.lock_level = READ_ONCE(ua->lock_level);

	if (copy_to_user((void __user *)arg, &pi, sizeof(pi)))
		return -EFAULT;
	return 0;
}

/*
 * UA_IOCTL_GET_PLUGIN_CAPS: no plugin operation exists (operations = 0);
 * the state bits are what the driver last did.  No register access.
 */
static long ua_ioctl_get_plugin_caps(struct ua_device *ua, unsigned long arg)
{
	struct ua_plugin_caps caps = { .struct_size = sizeof(caps) };
	unsigned int i;

	BUILD_BUG_ON(sizeof(struct ua_plugin_caps) != 64);

	mutex_lock(&ua->lock);
	if (ua->fw_loaded)
		caps.state |= UA_PLUGIN_STATE_FW_LOADED;
	if (ua->dsps_connected)
		caps.state |= UA_PLUGIN_STATE_DSPS_CONNECTED;
	if (ua->plugins_activated)
		caps.state |= UA_PLUGIN_STATE_INIT_CHAIN;
	if (ua->audio.connected)
		caps.state |= UA_PLUGIN_STATE_AUDIO_CONNECTED;
	caps.num_dsps = ua->num_dsps;
	for (i = 0; i < ua->num_dsps && i < UA_MAX_DSPS; i++)
		if (ua->dsp[i].rings_allocated)
			caps.dsp_rings |= BIT(i);
	caps.dma_orphans = ua->dma_orphan_count;
	mutex_unlock(&ua->lock);

	if (copy_to_user((void __user *)arg, &caps, sizeof(caps)))
		return -EFAULT;
	return 0;
}

/*
 * UA_IOCTL_GET_CLOCK_SOURCES: the clock-source menu ua_clock_list_build()
 * read at the last connect (kext GetClockSourceInfo).  No register access.
 */
static long ua_ioctl_get_clock_sources(struct ua_device *ua, unsigned long arg)
{
	struct ua_audio *audio = &ua->audio;
	struct ua_clock_sources cs = { .struct_size = sizeof(cs) };
	unsigned int i, n;

	BUILD_BUG_ON(sizeof(struct ua_clock_sources) != 656);
	BUILD_BUG_ON(UA_CLOCK_LIST_MAX > UA_CLOCK_SOURCES_MAX);
	BUILD_BUG_ON(UA_CLOCK_NAME_LEN > sizeof(cs.src[0].name));

	spin_lock(&audio->clock_list_lock);
	n = min_t(unsigned int, audio->clock_list_n, UA_CLOCK_LIST_MAX);
	for (i = 0; i < n; i++) {
		cs.src[i].value = audio->clock_list_val[i];
		if (audio->clock_list_named & BIT(i))
			cs.src[i].flags |= UA_CLOCK_SRC_F_DEVICE_NAME;
		memcpy(cs.src[i].name, audio->clock_list_name[i],
		       UA_CLOCK_NAME_LEN);
		cs.src[i].name[sizeof(cs.src[i].name) - 1] = '\0';
	}
	cs.count = n;
	cs.mask = audio->clock_src_mask;
	spin_unlock(&audio->clock_list_lock);

	if (copy_to_user((void __user *)arg, &cs, sizeof(cs)))
		return -EFAULT;
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
	info.public_type = ua->public_type;

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
	/* UA_FEAT_DSP_LOAD: no plugin allocations, so pools stay unused */
	info.alloc_load = ua_dsp_alloc_load(ua,
					    READ_ONCE(ua->audio.sample_rate));
	info.measured_load = ua_dsp_measured_load(ua, info.index);
	info.pgm_load = 0;
	info.mem_load = 0;
	info.flags = UA_DSP_INFO_F_LOADS;

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

/**
 * ua_cli_send_locked - Send CLI command (kernel-internal helper)
 * @ua: device (caller must hold ua->lock)
 * @cmd_data: command bytes to write to the CLI buffer
 * @cmd_len: length of @cmd_data in bytes (1..UA_CLI_CMD_BUF_SIZE)
 * @resp_data: buffer for the response bytes, or NULL to ignore the response
 * @resp_len: out: response length in bytes (capped at UA_CLI_RESP_BUF_SIZE),
 *            or NULL to ignore the response
 *
 * Must be called with ua->lock held.
 *
 * Return: 0 on success, -EINVAL for a bad @cmd_len, -ETIMEDOUT if the ARM
 * does not answer.  Currently every call returns -ENOSYS: CLI access is
 * blocked unconditionally because it freezes the ARM MCU.
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
 *      write the settings with a pending change (mask != 0) and the
 *      zero pass of those written last time, then bump SEQ_WR once
 *   4. DSP reads all 38 atomically per SEQ_WR advance
 *
 * The mask merge is critical — setting[2] has monitor volume ([7:0]),
 * mute ([16]), mono ([17]), and dim ([31]) sharing one register.
 * Each caller provides a partial mask; the cache accumulates the
 * fields changed since the last flush (mixer_mailbox, the default,
 * then clears the mask: see ua_mixer_flush_settings()).
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
 * With mixer_mailbox (the default) each changed setting is written
 * once and then cleared to zeros on the next flush, as the kext's
 * _writeSetting does; mixer_mailbox=0 keeps the old persistent val/mask
 * behaviour, which left the PAD/48V/Mic-Line relays dead.
 */
void ua_mixer_flush_settings(struct ua_device *ua)
{
	u32 seq_rd, word_a, word_b, reg;
	u64 zero_next = 0;
	int i;

	if (!ua->mixer_ready || !ua->mixer_dirty)
		return;

	/* Check DSP idle: SEQ_RD must match our cached SEQ_WR */
	seq_rd = ua_read(ua, UA_REG_MIXER_SEQ_RD);
	if (seq_rd != ua->mixer_seq_wr)
		return;  /* DSP still processing previous batch */

	/*
	 * Write the changed settings to SRAM, then one SEQ_WR bump.
	 * Word A = (mask[15:0] << 16) | val[15:0]
	 * Word B = (mask[31:16] << 16) | val[31:16]
	 *
	 * With mixer_mailbox (default) a setting's mask is cleared once it
	 * has been written and the setting goes out as zeros on the next
	 * flush, as the kext does.  The 2026-03 note that gain needs
	 * persistent masks did not hold up: gain reads back identically
	 * either way (titan, 2026-10-10).
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
		if (mixer_mailbox && !ua->mixer_mask[i] &&
		    (ua->mixer_zero_pending & BIT_ULL(i))) {
			/* mixer_mailbox: the one zero pass after a write */
			reg = ua_mixer_setting_reg(i);
			ua_write(ua, reg, 0);
			ua_write(ua, reg + 4, 0);
			continue;
		}
		if (!ua->mixer_mask[i])
			continue;
		reg = ua_mixer_setting_reg(i);
		word_a = ((ua->mixer_mask[i] & 0xFFFF) << 16) |
			 (ua->mixer_val[i] & 0xFFFF);
		word_b = (((ua->mixer_mask[i] >> 16) & 0xFFFF) << 16) |
			 ((ua->mixer_val[i] >> 16) & 0xFFFF);
		ua_write(ua, reg, word_a);
		ua_write(ua, reg + 4, word_b);
		if (mixer_mailbox) {
			ua->mixer_mask[i] = 0;
			zero_next |= BIT_ULL(i);
		}
	}

	/* Single SEQ_WR bump for entire batch */
	ua->mixer_seq_wr++;
	ua_write(ua, UA_REG_MIXER_SEQ_WR, ua->mixer_seq_wr);
	ua->mixer_zero_pending = zero_next;
	ua->mixer_dirty = zero_next != 0;

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
#define UA_OUTPUT_MUTE_TYPES	UA_PUBLIC_OUTPUT_MUTE_MASK
#define UA_OUTPUT_MUTE_MAX_TYPE	UA_PUBLIC_OUTPUT_MUTE_MAX

/*
 * The test runs on the kext's PUBLIC type (CPcieDevice+0xc48, from
 * GetCapabilities), not the serial-table type: x4 = 0x1A (no mute),
 * serial 2032 -> 0x1C, 2016 -> 0x13, 2110 -> 0x27.  Only when the public
 * type could not be computed does the serial-table type decide, as the
 * driver did before the public type existed.
 */
static bool ua_has_output_mute(const struct ua_device *ua)
{
	if (ua->public_type != UA_PUBTYPE_NONE)
		return ua_public_output_mute(ua->public_type);

	return ua->device_type <= UA_OUTPUT_MUTE_MAX_TYPE &&
	       (UA_OUTPUT_MUTE_TYPES & BIT_ULL(ua->device_type));
}

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

	if (!ua_has_output_mute(ua))
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

	memset(cli_cmd, 0, sizeof(cli_cmd));
	cli_cmd[0] = param_id;
	cli_cmd[1] = value;
	cli_cmd[5] = UA_ARM_HW_ID(ch);
	cli_cmd[8] = 8;

	ret = ua_cli_send_locked(ua, (u8 *)cli_cmd, 36, NULL, NULL);

	mutex_unlock(&ua->lock);
	return ret;
}

/*
 * Mixer "kind": CUAD2DeviceMixer::Initialize derives it from the public
 * UAD2DeviceType (ua->public_type, from GetCapabilities), and the kext's
 * parameter handlers gate on it (parity/14 section 2).  The Apollo x4 is
 * public type 0x1a, kind 14 (32 buses, 7 bus params, preamp channels 0-3).
 * Types the kext creates no mixer for, v1 parts and an unresolved public
 * type are UA_MIXER_KIND_NONE and are not gated.
 */
#define UA_MIXER_KIND_NONE	(-1)

static int ua_mixer_kind(const struct ua_device *ua)
{
	u32 pt = ua->public_type;

	switch (pt) {
	case 0x03: case 0x05:
		return 0;
	case 0x06: case 0x07:
		return 1;
	case 0x08: case 0x09: case 0x10:
		return 2;
	case 0x0a: case 0x0b:		/* original Apollo: 24 buses, 1 param */
		return 3;
	case 0x0c:
		return 4;
	case 0x0d:
		return 5;
	case 0x0e:
		return 6;
	case 0x0f:
		return 7;
	case 0x12:
		return 8;
	case 0x13: case 0x1b: case 0x1c: case 0x27:
		return 9;
	case 0x14 ... 0x17:		/* x6, x8, x8p, x16 */
		return 0x0a + (pt - 0x14);
	case 0x19: case 0x1d:
		return 0x0f;
	case 0x1a:			/* Apollo x4 */
		return 14;
	case 0x20:			/* x16D */
		return 0x10;
	case 0x21 ... 0x24:		/* x6/x8/x8p/x16 Gen 2 */
		return 0x11 + (pt - 0x21);
	case 0x25: case 0x29:
		return 0x15;
	case 0x26: case 0x28:		/* x4 Gen 2, Gen 2 TB4 */
		return 22;
	default:
		return UA_MIXER_KIND_NONE;
	}
}

/*
 * Highest ARM-preamp channel per mixer kind (CUAD2DeviceMixer +0x16ec,
 * parity/14 section 2); -1 = the kind has no ARM preamps.  Kind 0x10
 * (x16D) says 23, but only the two four-channel banks of channels 0-7
 * are known, so ua_kind_max_arm_ch() stops there.
 */
static const s8 ua_kind_arm_ch[] = {
	3, -1, 1, -1, 3, -1, 7, 1,	/* kinds 0-7 */
	1, 1, 1, 3, 7, -1, 3, 1,	/* 8-15: x6 1, x8 3, x8p 7, x16 -, x4 3 */
	23, 1, 3, 7, -1, 1, 3,		/* 16-22: x16D, Gen 2 x6..x16, x4 Gen 2 */
};

/* -1: no ARM preamps; an unknown kind keeps the old limit, channel 7 */
static int ua_kind_max_arm_ch(int kind)
{
	if (kind < 0 || kind >= (int)ARRAY_SIZE(ua_kind_arm_ch))
		return UA_MAX_PREAMP_CH - 1;
	return min_t(int, ua_kind_arm_ch[kind], UA_MAX_PREAMP_CH - 1);
}

/* Allowed-kind bit sets from the kext's gates (parity/14 section 2) */
#define UA_KIND_SET_2A_2D	0x00112022u	/* 1,5,13,16,20 */
#define UA_KIND_SET_33_35	0x0011202au	/* 1,3,5,13,16,20 */
#define UA_KIND_SET_42		0x001f3c50u	/* 4,6,10-13,16-20 */
#define UA_KIND_SET_49		0x001f3c00u	/* 10-13,16-20 */
#define UA_KIND_SET_3E		0x0060c1f4u	/* 2,4-8,14,15,21,22 */
#define UA_KIND_SET_41		0x0060c184u	/* 2,7,8,14,15,21,22 */
#define UA_KIND_SET_69		0x005f7c00u	/* 10-14,16-20,22 */
#define UA_KIND_SET_10_22	0x007ffc00u	/* 10-22 */
#define UA_KIND_SET_IN_15	0x006e1c50u	/* 4,6,10-12,17-19, 21, 22 */

static bool ua_kind_allows(int kind, u32 set)
{
	return kind == UA_MIXER_KIND_NONE || (set & BIT(kind));
}

/* Would the kext's SetOtherParam accept this param on this device? */
static bool ua_other_param_allowed(const struct ua_device *ua, u32 param)
{
	int kind = ua_mixer_kind(ua);

	switch (param) {
	case 0x36:	/* S4 / S5 full words: the original Apollo (kind 3) only */
	case 0x37:
		return kind == UA_MIXER_KIND_NONE || kind == 3;
	case 0x2a ... 0x2d:
		return ua_kind_allows(kind, UA_KIND_SET_2A_2D);
	case 0x33 ... 0x35:
		return ua_kind_allows(kind, UA_KIND_SET_33_35);
	case 0x42:
		return ua_kind_allows(kind, UA_KIND_SET_42);
	case 0x49:
		return ua_kind_allows(kind, UA_KIND_SET_49);
	case 0x32:
		return kind == UA_MIXER_KIND_NONE || (kind != 0 && kind != 3);
	case 0x3e:
		return ua_kind_allows(kind, UA_KIND_SET_3E);
	case 0x41:
		return ua_kind_allows(kind, UA_KIND_SET_41);
	case 0x69:
		return ua_kind_allows(kind, UA_KIND_SET_69);
	case 0x67:
	case 0x68:
	case 0x87 ... 0x8a:
		return ua_kind_allows(kind, UA_KIND_SET_10_22);
	case 0x83 ... 0x86:	/* Dante-class kind 16 only */
		return kind == UA_MIXER_KIND_NONE || kind == 16;
	default:
		return true;
	}
}

/*
 * The kext's 2-bit "swap" fields: value bit 1 goes to the lower field bit
 * and value bit 0 to the upper one (other params 0x03, 0x06, 0x08, 0x0a,
 * 0x0f, 0x23, 0x25).  @lo is the lower field bit.
 */
static u32 ua_swap2(u32 value, unsigned int lo)
{
	return ((value & 2) ? BIT(lo) : 0) | ((value & 1) ? BIT(lo + 1) : 0);
}

/*
 * SetOtherParam families that are plain "value into one field" writes with
 * no bit swap: surround/HP-correction/Dante/trim ids (parity/14 section 4,
 * reverse index in section 6).  One row covers a run of ids; id N of the
 * run lands at bit (shift + (N - first) * stride) of @setting, @width bits
 * wide (width 1 = bool, value != 0).  Kind gates live in
 * ua_other_param_allowed() (0x69: kinds 10-14,16-20,22; 0x88-0x8a: 10-22);
 * the kext accepts the other ids on every kind.
 *
 *   ids          setting  field                          kext meaning
 *   0x4d-0x4e    S33      bytes 2,3  (shift 16,24)       surround trim ch 2/3
 *   0x4f-0x52    S34      bytes 0-3  (shift 0,8,16,24)   surround trim
 *   0x55-0x5a    S32      bits 26-31                     surround flags
 *   0x5d-0x62    S32      bits 18-23                     surround flags
 *   0x65         S31      [25:24], 2 bits                root property (L)
 *   0x69         S5       bit 5                          HP2 correction enable
 *   0x6b-0x6e    S36      bytes 0-3                      surround trim
 *   0x6f-0x72    S37      bytes 0-3                      surround trim
 *   0x73-0x7a    S35      bits 16-23                     per-channel flags
 *   0x7b-0x82    S35      bits 8-15                      per-channel flags
 *   0x88         S5       [31:24], 8 bits                surround LFE/sub trim
 *   0x89-0x8a    S5       bits 10,11                     surround channel flags
 *
 * The Mixer Engine sends these to its multi-channel surround models; none
 * is known to be sent to a first-generation x4.  Ids 0x4a-0x4c, 0x53/0x54,
 * 0x5b/0x5c, 0x63, 0x64, 0x66-0x68, 0x6a and 0x87 stay explicit cases.
 */
struct ua_other_field {
	u8 first, last;		/* id run, inclusive */
	u8 setting;
	u8 shift;		/* shift of the first id */
	u8 stride;		/* shift step per id */
	u8 width;		/* bits */
};

static const struct ua_other_field ua_other_fields[] = {
	{ UA_MON_PARAM_SURR_TRIM_2,	0x4e, 33, 16, 8, 8 },
	{ UA_MON_PARAM_SURR_TRIM_4,	0x52, 34,  0, 8, 8 },
	{ UA_MON_PARAM_SURR_FLAG_55,	0x5a, 32, 26, 1, 1 },
	{ UA_MON_PARAM_SURR_FLAG_5D,	0x62, 32, 18, 1, 1 },
	{ UA_MON_PARAM_ROOT_65,		0x65, 31, 24, 0, 2 },
	{ UA_MON_PARAM_HP2_CORRECTION,	0x69,  5,  5, 0, 1 },
	{ UA_MON_PARAM_SURR_TRIM_6B,	0x6e, 36,  0, 8, 8 },
	{ UA_MON_PARAM_SURR_TRIM_6F,	0x72, 37,  0, 8, 8 },
	{ UA_MON_PARAM_CHAN_FLAG_73,	0x7a, 35, 16, 1, 1 },
	{ UA_MON_PARAM_CHAN_FLAG_7B,	0x82, 35,  8, 1, 1 },
	{ UA_MON_PARAM_SURR_LFE_TRIM,	0x88,  5, 24, 0, 8 },
	{ UA_MON_PARAM_SURR_FLAG_89,	0x8a,  5, 10, 1, 1 },
};

static bool ua_other_field_lookup(u32 param, u32 value, unsigned int *setting,
				  u32 *hw_val, u32 *hw_mask)
{
	unsigned int i;

	for (i = 0; i < ARRAY_SIZE(ua_other_fields); i++) {
		const struct ua_other_field *f = &ua_other_fields[i];
		u32 vmask, shift;

		if (param < f->first || param > f->last)
			continue;
		shift = f->shift + (param - f->first) * f->stride;
		vmask = GENMASK(f->width - 1, 0);
		*setting = f->setting;
		*hw_mask = vmask << shift;
		*hw_val = (f->width == 1 ? !!value : (value & vmask)) << shift;
		return true;
	}
	return false;
}

/*
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
UA_GCC_OPTIMIZE("Os") int
ua_monitor_set_param(struct ua_device *ua, unsigned int ch_idx,
		     u32 param_id, u32 value)
{
	unsigned int setting_idx;
	u32 hw_val, hw_mask;

	/* SetOtherParam: param > 0x8a is -8; some params are kind-gated */
	if (param_id > 0x8a || !ua_other_param_allowed(ua, param_id)) {
		dev_dbg(&ua->pdev->dev,
			"monitor param 0x%x: rejected for this device (kind %d)\n",
			param_id, ua_mixer_kind(ua));
		return -EINVAL;
	}

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
		 * Bit 1 of the value is the mute (setting[2] bit 16), bit 0
		 * the mono (bit 17); the readback reports them as two
		 * independent bits (the front panel can set both), so 3 is
		 * both.  0, 1 and 2 are what the Mixer Engine sends
		 * (VERIFIED 2026-02-20): off, MixToMono, muted.
		 */
		setting_idx = 2;
		hw_val = ((value & UA_MON_MUTE_ON) ? BIT(16) : 0) |
			 ((value & UA_MON_MONO) ? BIT(17) : 0);
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
	case UA_MON_PARAM_CUE1_MONO:	/* 0x06 -> bits[21:20], swapped */
		/* Value bit 0 = mono = field bit 21; bit 1 -> bit 20 */
		setting_idx = 2;
		hw_val  = ua_swap2(value, 20);
		hw_mask = 0x00300000;
		break;
	case UA_MON_PARAM_CUE2_MIX:	/* 0x07 -> bits[27:26] */
		setting_idx = 2;
		hw_val  = (value & 3) << 26;
		hw_mask = 0x0C000000;
		break;
	case UA_MON_PARAM_CUE2_MONO:	/* 0x08 -> bits[23:22], swapped */
		setting_idx = 2;
		hw_val  = ua_swap2(value, 22);
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
		 * Kext property ID 0x36, mixer kind 3 (original Apollo) only;
		 * ua_other_param_allowed() rejects it everywhere else.  On the
		 * x4, mono is param 0x03 value=1 (MixToMono).
		 */
		setting_idx = 4;
		hw_val  = value;
		hw_mask = 0xFFFFFFFF;
		break;

	/* ---- Setting[6]: Mirror A / Identify / MirrorsToDigital ---- */

	case UA_MON_PARAM_AUX1_MONO:	/* 0x0a -> bits[9:8], swapped */
		setting_idx = 6;
		hw_val  = ua_swap2(value, 8);
		hw_mask = 0x00000300;
		break;
	case UA_MON_PARAM_IDENTIFY:	/* 0x1d -> bits[13:11], mask [15:11] */
		/* IdentifyDevice: 3-bit value, bits 14-15 are cleared */
		setting_idx = 6;
		hw_val  = (value & 7) << 11;
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

	case UA_MON_PARAM_AUX2_MONO:	/* 0x0f -> bits[9:8], swapped */
		setting_idx = 7;
		hw_val  = ua_swap2(value, 8);
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
		hw_val  = ua_device_mode_word(ua, value);
		hw_mask = 0x000000FF;
		break;
	case UA_MON_PARAM_UNKNOWN_22:	/* 0x22 -> bits[29:28] */
		setting_idx = 8;
		hw_val  = (value & 3) << 28;
		hw_mask = 0x30000000;
		break;
	case UA_MON_PARAM_SYNCH18_MONO:/* 0x23 -> bits[25:24], swapped */
		setting_idx = 8;
		hw_val  = ua_swap2(value, 24);
		hw_mask = 0x03000000;
		break;
	case UA_MON_PARAM_UNKNOWN_24:	/* 0x24 -> bits[31:30] */
		setting_idx = 8;
		hw_val  = (value & 3) << 30;
		hw_mask = 0xC0000000;
		break;
	case UA_MON_PARAM_SYNCH1A_MONO:/* 0x25 -> bits[27:26], swapped */
		setting_idx = 8;
		hw_val  = ua_swap2(value, 26);
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

	case UA_MON_PARAM_ALTMON_SELECT: /* 0x3a -> bits[29:28] */
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
	case UA_MON_PARAM_HP_LEVEL:	/* 0x66 -> bits[7:0] */
		setting_idx = 14;
		hw_val  = value & 0xFF;
		hw_mask = 0x000000FF;
		break;

	/* ---- Setting[15]: Mirror enables / clock-HP routing ---- */

	case UA_MON_PARAM_ALTMON1_TRIM:	/* 0x38 -> bits[6:0] */
		setting_idx = 15;
		hw_val  = value & 0x7F;
		hw_mask = 0x0000007F;
		break;
	case UA_MON_PARAM_ALTMON2_TRIM:	/* 0x39 -> bits[22:16] */
		setting_idx = 15;
		hw_val  = (value & 0x7F) << 16;
		hw_mask = 0x007F0000;
		break;
	case UA_MON_PARAM_ALTMON1_ENABLE: /* 0x3b -> bit8 */
		setting_idx = 15;
		hw_val  = value ? BIT(8) : 0;
		hw_mask = BIT(8);
		break;
	case UA_MON_PARAM_ALTMON2_ENABLE: /* 0x3c -> bit12 */
		setting_idx = 15;
		hw_val  = value ? BIT(12) : 0;
		hw_mask = BIT(12);
		break;
	case UA_MON_PARAM_UNKNOWN_3E:	/* 0x3e -> bit9 */
		setting_idx = 15;
		hw_val  = value ? BIT(9) : 0;
		hw_mask = BIT(9);
		break;
	case UA_MON_PARAM_REMOTE_MON:	/* 0x41 -> bit7 */
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
		if (ua_other_field_lookup(param_id, value, &setting_idx,
					  &hw_val, &hw_mask))
			break;
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

/**
 * ua_preamp_write_locked - write one input parameter (SEL131 type 1)
 * @ua: device (caller holds ua->lock)
 * @ch: input (0-31 for the DSP params, else a preamp channel)
 * @param_id: UA_PREAMP_PARAM_* (0x00-0x17)
 * @value: see below
 *
 * Mirrors CUAD2DeviceMixer::SetInputParam (parity/14-mixer-params.md s3).
 * Preamp controls go through DSP mixer settings only; CLI commands freeze
 * the ARM MCU (setInputParamARM routes everything to CPcieDeviceMixer::
 * WriteSetting in the kext).  Settings S+12 / S+8 are the channel 4-7 banks.
 *
 *   0x00-0x04, 0x05 (ch 0-3)  flags: S0 (S12), bit param << 6*(ch&3);
 *                             phase takes +-1.0f (or 0/1)
 *   0x02 Link                 even channel only; both pair bits (0x104 /
 *                             0x104000) in one write; an odd channel is a no-op
 *   0x06 GainC                S3 (S14), 6-bit at shifts 0/6/16/22
 *   0x07 mic impedance        S1 (S13), nibble at 9/16/20/24
 *   0x08 custom preamp        S11 bit 8+ch
 *   0x09-0x0f                 S16-S22 (S24-S30) byte lanes
 *   0x10 knob steps           S23 (S31), nibble at 4*(ch&3)
 *   0x11 HiZ impedance        S13, nibble 0 / 1 for inputs 1 / 2 only
 *   0x12 selected param       S23 (S31), 2 bits at 16+2*(ch&3)
 *   0x13 route, 0x16 span     DSP ring {0x1E0004, ch, 7|8, v}
 *   0x14 HiZ host write       S0 bit 24+ch
 *   0x15 PGA line disable     S15 bit 24+ch; -EINVAL where the kext says -8
 *   0x17 raw record enable    S5 bit 16+ch
 *   0x05 (ch >= 4 on the x4)  DSP ring {0x1E0004, ch, 5, +-1.0f}
 *
 * Returns the setting write result, or -EINVAL where the kext returns -8
 * (param > 0x17, channel beyond the device's preamps, param not on the kind).
 */
int ua_preamp_write_locked(struct ua_device *ua, unsigned int ch,
			   u32 param_id, u32 value)
{
	static const u8 imp_shift[4] = { 9, 16, 20, 24 };
	unsigned int setting_idx, shift;
	unsigned int dsp_phase_ch, hiz_inputs;
	int kind = ua_mixer_kind(ua);
	int max_arm_ch;
	bool hi_bank = ch > 3;
	u32 hw_mask, hw_val;

	if (param_id > UA_PREAMP_PARAM_RAW_REC)
		return -EINVAL;

	/* DSP-side params: no mixer setting at all */
	if (param_id == UA_PREAMP_PARAM_ROUTE)
		return ua_dsp_set_input_param(ua, ch, UA_DSP_INPUT_ROUTE,
					      value);
	if (param_id == UA_PREAMP_PARAM_LEVEL)
		return ua_dsp_set_input_param(ua, ch, UA_DSP_INPUT_SPAN, value);

	/* Highest ARM preamp channel by kind: 3 on the x4 (kind 14) */
	max_arm_ch = ua_kind_max_arm_ch(kind);
	/* Phase goes to the DSP from the first channel without a preamp */
	dsp_phase_ch = ua->audio.num_preamps ? ua->audio.num_preamps :
					       max_arm_ch + 1;
	if (param_id == UA_PREAMP_PARAM_PHASE && ch >= dsp_phase_ch) {
		/* Scale float: +1.0f normal, -1.0f invert (0 / 1 accepted) */
		if (value == 0)
			value = 0x3f800000;
		else if (value == 1)
			value = 0xbf800000;
		return ua_dsp_set_input_param(ua, ch, UA_DSP_INPUT_SCALE,
					      value);
	}

	if ((int)ch > max_arm_ch)
		return -EINVAL;

	/* HiZ inputs of the model (2 without model data, as before) */
	hiz_inputs = ua->audio.model ? min_t(unsigned int,
					     ua->audio.num_hiz, 2) : 2;

	switch (param_id) {
	case UA_PREAMP_PARAM_MIC_LINE:
	case UA_PREAMP_PARAM_PAD:
	case UA_PREAMP_PARAM_48V:
	case UA_PREAMP_PARAM_LOWCUT:
	case UA_PREAMP_PARAM_PHASE: {
		bool flag_set;

		setting_idx = hi_bank ? 12 : 0;
		shift = (ch & 3) * 6;
		hw_mask = (1u << param_id) << shift;

		/*
		 * Phase: the kext takes a float (+1.0f normal, -1.0f invert)
		 * but the daemon sends an int (0 normal, 1 invert).
		 */
		if (param_id == UA_PREAMP_PARAM_PHASE)
			flag_set = !!(value & BIT(31)) || value == 1;
		else
			flag_set = !!value;

		hw_val = flag_set ? hw_mask : 0;
		break;
	}
	case UA_PREAMP_PARAM_LINK:
		/* The kext links a pair from its even channel, both bits at once */
		if (ch & 1)
			return 0;
		setting_idx = hi_bank ? 12 : 0;
		hw_mask = 0x104u << (12 * ((ch & 3) >> 1));
		hw_val = value ? hw_mask : 0;
		break;
	case UA_PREAMP_PARAM_GAIN_C:
		/*
		 * Kext disasm (sub_48734): mask 0x3F with a 4-bit gap between
		 * the pairs: inputs 1-4 at shifts 0/6/16/22.
		 */
		setting_idx = hi_bank ? 14 : 3;
		shift = (ch & 3) * 6;
		if (ch & 2)
			shift += 4;
		hw_mask = 0x3Fu << shift;
		hw_val = (value & 0x3F) << shift;
		break;
	case UA_PREAMP_PARAM_MIC_IMP:
		setting_idx = hi_bank ? 13 : 1;
		shift = imp_shift[ch & 3];
		hw_mask = 0xFu << shift;
		hw_val = (value & 0xF) << shift;
		break;
	case UA_PREAMP_PARAM_CUSTOM:
		setting_idx = 11;
		hw_mask = BIT(8 + ch);
		hw_val = value ? hw_mask : 0;
		break;
	case 0x09 ... 0x0f:
		/* S16-S22 for ch 0-3, S24-S30 above: byte lane per channel */
		setting_idx = param_id + 7 + (hi_bank ? 8 : 0);
		shift = (ch & 3) * 8;
		hw_mask = 0xFFu << shift;
		hw_val = (value & 0xFF) << shift;
		break;
	case UA_PREAMP_PARAM_KNOB_STEPS:
		setting_idx = hi_bank ? 31 : 23;
		shift = 4 * (ch & 3);
		hw_mask = 0xFu << shift;
		hw_val = (value & 0xF) << shift;
		break;
	case UA_PREAMP_PARAM_HIZ_IMP:
		/* The model's HiZ inputs only (two on the x4, one on a Twin) */
		if (ch >= hiz_inputs)
			return -EINVAL;
		setting_idx = 13;
		shift = 4 * ch;
		hw_mask = 0xFu << shift;
		hw_val = (value & 0xF) << shift;
		break;
	case UA_PREAMP_PARAM_SEL_PARAM:
		setting_idx = hi_bank ? 31 : 23;
		shift = 16 + 2 * (ch & 3);
		hw_mask = 3u << shift;
		hw_val = (value & 3) << shift;
		break;
	case UA_PREAMP_PARAM_HIZ:
		if (hi_bank)
			return -EINVAL;
		setting_idx = 0;
		hw_mask = BIT(24 + ch);
		hw_val = value ? hw_mask : 0;
		break;
	case UA_PREAMP_PARAM_PGA_LINE:
		if (!ua_kind_allows(kind, UA_KIND_SET_IN_15))
			return -EINVAL;
		setting_idx = 15;
		hw_mask = BIT(24 + ch);
		hw_val = value ? hw_mask : 0;
		break;
	case UA_PREAMP_PARAM_RAW_REC:
		if (hi_bank)
			return -EINVAL;
		setting_idx = 5;
		hw_mask = BIT(16 + ch);
		hw_val = value ? hw_mask : 0;
		break;
	default:
		return -EINVAL;
	}

	return ua_mixer_write_setting_locked(ua, setting_idx, hw_val, hw_mask);
}

/* ----------------------------------------------------------------
 * Mixer / CLI ioctl handlers
 * ---------------------------------------------------------------- */

/*
 * Raw ARM CLI commands freeze the ARM MCU on the Apollo x4 (front panel
 * and relays dead until a power cycle), which is why ua_cli_send_locked()
 * refuses everything.  The ioctl used to write the CLI registers itself
 * and bypass that; it is refused the same way now.
 */
static long ua_ioctl_cli_command(struct ua_device *ua, unsigned long arg)
{
	return -ENOSYS;
}

/* GCC 13.3 ICE in try_forward_edges on this large switch — reduce optimization */
UA_GCC_OPTIMIZE("O1")
static noinline UA_GCC_OPTIMIZE("O0") long
ua_ioctl_set_mixer_param(struct ua_device *ua, unsigned long arg)
{
	struct ua_mixer_param param;
	int ret = 0;
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
		 * The setting and lane of every param are in
		 * ua_preamp_write_locked().
		 *
		 * Route (0x13) and Level (0x16) are DSP ring commands for ALL
		 * input channels (0-31), not just physical preamps (0-3); so
		 * is the phase scale (0x05) above the last preamp channel.
		 */
		if (param.param_id == UA_PREAMP_PARAM_ROUTE ||
		    param.param_id == UA_PREAMP_PARAM_LEVEL ||
		    (param.param_id == UA_PREAMP_PARAM_PHASE &&
		     param.channel_idx >= ua->audio.num_preamps)) {
			if (param.channel_idx > 31) {
				ret = -EINVAL;
				goto out_unlock;
			}
		} else if (param.channel_idx >= ua->audio.num_preamps ||
			   param.channel_idx >= UA_MAX_PREAMP_CH) {
			ret = -EINVAL;
			goto out_unlock;
		}

		ret = ua_preamp_write_locked(ua, param.channel_idx,
					     param.param_id, param.value);

		/*
		 * Not written (mixer not ready: quiesced, or a resume
		 * re-init still running): report it, and keep the ALSA cache
		 * on what the hardware has.
		 */
		if (ret)
			break;

		/* Update ALSA cache and prepare notification */
		ch = param.channel_idx;
		if (ch >= ua->audio.num_preamps)	/* DSP-only input */
			break;
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
		case UA_PREAMP_PARAM_HIZ_IMP:
			if (param.value < 8)
				ua->audio.preamp[ch].hiz_imp = param.value;
			ctl_name = "Line %u HiZ Impedance";
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
			ua->audio.monitor.level = min_t(int, param.value & 0xFF, 192);
			ctl_name = "Monitor Playback Volume";
			notify_alsa = true;
			break;
		case UA_MON_PARAM_MUTE:
			/* Param 0x03: bit 1 = mute, bit 0 = mono (see the write) */
			ua->audio.monitor.mute = (param.value & UA_MON_MUTE_ON) ?
						 UA_MON_MUTE_ON : UA_MON_MUTE_OFF;
			ua->audio.monitor.mono = !!(param.value & UA_MON_MONO);
			/* Both controls follow it; the second goes out below */
			ua_ctl_notify_name(ua, "Monitor Mono Switch");
			ctl_name = "Monitor Playback Switch";
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
		case UA_MON_PARAM_HP1_VOL:
			ua->audio.monitor.hp1_vol = min_t(int, param.value & 0xFF, 192);
			ctl_name = "Headphone 1 Playback Volume";
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
			/* Bit 0 is mono; bit 1 is the kext's other field bit */
			ua->audio.monitor.cue1_mono = !!(param.value & 1);
			ctl_name = "CUE 1 Mono Switch";
			notify_alsa = true;
			break;
		case UA_MON_PARAM_CUE2_MONO:
			ua->audio.monitor.cue2_mono = !!(param.value & 1);
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
		 * Sends ring buffer cmd 0x1C to ALL DSPs, but only when the
		 * state differs from the cache (which starts all-enabled).
		 * Note: param_id IS the enable value; value field ignored.
		 *
		 * The Mixer Engine asks for bus 1,2,5,6 enable=1 at start.
		 * The first request per bus since probe is always sent (a
		 * warm probe cannot know what the running DSP holds); later
		 * ones only when they change the state.
		 */
		ret = ua_dsp_set_bus_enable(ua, param.channel_idx,
					    param.param_id);
		if (ret)
			dev_warn(&ua->pdev->dev,
				 "bus_enable failed: bus=%u en=%u (%d)\n",
				 param.channel_idx, param.param_id, ret);
		break;

	case 3:  /* Rate ack: {0x001A0002, current rate} to every DSP */
		/* channel_idx, param_id and value are ignored */
		ret = ua_audio_rate_ack_request(ua);
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
		char name[44];

		snprintf(name, sizeof(name), ctl_name, ch + 1);
		ua_ctl_notify_name(ua, name);
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

	switch (param.channel_type) {
	case 0:  /* GetBusEnabled: mask of enabled buses, bit n = bus n (n < 7) */
		param.value = ua_dsp_get_bus_enable_mask(ua);
		ret = 0;
		break;

	case 1:  /* Preamp → CLI query (refused: see ua_cli_send_locked()) */
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
	 *
	 * One call writes one (param, side) entry.  The kext's API takes
	 * two per call, left and right; the struct has room for one value,
	 * so the caller makes two calls and sets UA_BUS_PARAM_F_SIDE_R on
	 * the second.  Without the flag (every caller so far sends flags
	 * 0x02) the entry is side 0, exactly what was always written.
	 */
	{
		u32 side = (bp.flags & UA_BUS_PARAM_F_SIDE_R) ? 1 : 0;
		int bus_ret;

		mutex_lock(&ua->lock);
		bus_ret = ua_dsp_set_bus_param_flags(ua, bp.bus_id,
				bp.sub_param, side, bp.value,
				(bp.flags & UA_BUS_PARAM_F_SOLO_MUTE) ?
					UA_DSP_BUSP_SOLO_MUTE : 0);
		mutex_unlock(&ua->lock);
		return bus_ret;
	}
}

/*
 * Batched SetMixerBusParam (kext selector 130).  The ring helper takes
 * one entry and rings the doorbell itself (ring helpers are static in
 * ua_dsp.c), so the batch is N calls of the single-entry path under one
 * ua->lock hold: validated up front, applied in order, nothing else
 * interleaves.
 */
static long ua_ioctl_set_mixer_bus_params(struct ua_device *ua,
					  unsigned long arg)
{
	struct ua_mixer_bus_params bp;
	struct ua_mixer_bus_entry *ent;
	unsigned int i;
	long ret = 0;

	if (copy_from_user(&bp, (void __user *)arg, sizeof(bp)))
		return -EFAULT;
	if (bp.flags || !bp.count || bp.count > UA_BUS_PARAM_BATCH_MAX)
		return -EINVAL;
	if ((bp.hwid_lo || bp.hwid_hi) &&
	    (bp.hwid_lo != ua_read(ua, UA_REG_HW_UID_LO) ||
	     bp.hwid_hi != ua_read(ua, UA_REG_HW_UID_HI)))
		return -ENXIO;

	ent = memdup_user(u64_to_user_ptr(bp.entries),
			  bp.count * sizeof(*ent));
	if (IS_ERR(ent))
		return PTR_ERR(ent);

	for (i = 0; i < bp.count; i++)
		if (ent[i].bus_id > 0x1f || ent[i].param > 6 ||
		    ent[i].side > 1) {
			kfree(ent);
			return -EINVAL;
		}

	bp.applied = 0;
	mutex_lock(&ua->lock);
	for (i = 0; i < bp.count; i++) {
		ret = ua_dsp_set_bus_param(ua, ent[i].bus_id, ent[i].param,
					   ent[i].side, ent[i].value);
		if (ret)
			break;
		bp.applied++;
	}
	mutex_unlock(&ua->lock);
	kfree(ent);

	if (copy_to_user((void __user *)arg, &bp, sizeof(bp)))
		return -EFAULT;
	return ret;
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

/*
 * The kext's driver parameters 1-5 (UA_DRIVER_PARAM_KEXT(n), ua_ioctl.h):
 * CDeviceManager::SetDriverParameter (kext x86 0x1dd38) stores them and
 * GetDriverParameter (0x1de68) returns them; nothing here acts on them.
 * Initial values as CDeviceManager::Initialize leaves them: 5 reads 1,
 * the others 0.
 */
static int ua_kext_param_set(struct ua_device *ua, u32 id, u64 value)
{
	unsigned int n = id - UA_DRIVER_PARAM_KEXT_BASE;

	if (id < UA_DRIVER_PARAM_KEXT(1) || id > UA_DRIVER_PARAM_KEXT(5))
		return -EINVAL;
	if (n == 2)
		value &= 0xfffffffeu;	/* bit 0: clear 3, not kept (0x18db6) */
	else if (n == 3)
		value = 0;		/* Set clears the counter (0x1ddbc) */
	else if (n != 4)
		value = (u32)value;
	mutex_lock(&ua->lock);
	ua->kext_param[n] = value;
	ua->kext_param_set |= BIT(n);
	if (n == 2) {			/* bit 0 cleared the counter */
		ua->kext_param[3] = 0;
		ua->kext_param_set |= BIT(3);
	}
	mutex_unlock(&ua->lock);
	return 0;
}

static int ua_kext_param_get(struct ua_device *ua, u32 id, u64 *value)
{
	unsigned int n = id - UA_DRIVER_PARAM_KEXT_BASE;
	u64 v;

	if (id < UA_DRIVER_PARAM_KEXT(1) || id > UA_DRIVER_PARAM_KEXT(5))
		return -EINVAL;
	mutex_lock(&ua->lock);
	v = (ua->kext_param_set & BIT(n)) ? ua->kext_param[n] :
	    (n == 5 ? 1 : 0);
	mutex_unlock(&ua->lock);
	if (n == 1 || n == 5)		/* movslq in GetDriverParameter */
		v = (u64)(s64)(s32)(u32)v;
	*value = v;
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

		if (dp.value > 192000)
			return -EINVAL;
		if (ua->audio.transport_running)
			dev_warn(&ua->pdev->dev,
				 "changing sample rate while transport running!\n");
		ret = ua_audio_set_clock(ua, (unsigned int)dp.value);
		if (!ret)
			ua_notify_clock_controls(ua);
		return ret;
	}

	case 1: {  /* Clock source: UA_CLOCK_* (clock_config low byte) */
		int ret;

		if (dp.value > 0xff)
			return -EINVAL;
		/* Validates it and commits it under ua->lock once acked */
		ret = ua_audio_set_clock_source(ua, (unsigned int)dp.value);
		if (!ret)
			ua_notify_clock_controls(ua);
		return ret;
	}

	default:
		return ua_kext_param_set(ua, dp.param_id, dp.value);
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
	default: {
		int ret = ua_kext_param_get(ua, dp.param_id, &dp.value);

		if (ret)
			return ret;
		break;
	}
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
 * kext_connect (default): the kext's rule, the public type in the
 * ring-connect set, which holds every type a v2 Apollo gets (parity/11
 * R1).  An unresolved public type keeps the serial-type rule below, so a
 * device the decode misses still connects as it did before.
 * kext_connect=0: the v11.8.1 set on the serial type, the complement of
 * ua_uses_audio_extension().  v12_connect=1: the v12 set, which also
 * covers Solo TB / Arrow / 0x29 — those keep their AudioExtension
 * bring-up and get the ring connect on top, as in the kext's Connect().
 */
static bool ua_needs_ring_connect(struct ua_device *ua)
{
	if (ua_kext_connect && ua->public_type != UA_PUBTYPE_NONE)
		return ua_public_ring_connect(ua->public_type);
	if (v12_connect)
		return ua_ring_connect_v12(ua->device_type);
	return !ua_uses_audio_extension(ua->device_type);
}

/*
 * kext_connect, AudioExtension devices (the x4 among them): the kext runs
 * the ring connect inside Connect(), after the ACEFACE handshake and on
 * every start, cold or warm.  Here the handshake completes in
 * ua_audio_connect(), which calls ua_kext_ring_connect() right after it,
 * so probe leaves the ring connect to that call.
 */
static bool ua_ring_connect_deferred(struct ua_device *ua)
{
	return ua_kext_connect && ua_uses_audio_extension(ua->device_type) &&
	       ua_needs_ring_connect(ua);
}

/* Caller holds ua->lock; ACEFACE has just completed. */
void ua_kext_ring_connect(struct ua_device *ua)
{
	int ret;

	if (!ua_ring_connect_deferred(ua) || atomic_read(&ua->shutdown))
		return;

	ret = ua_dsp_connect_retry(ua);
	dev_info(&ua->pdev->dev,
		 "kext_connect: ring connect + dated finalize after ACEFACE (public type 0x%02x): %s (%d)\n",
		 ua->public_type, ret ? "failed" : "ok", ret);
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
		if (ret || !ua_needs_ring_connect(ua) ||
		    ua_ring_connect_deferred(ua))
			return ret;
	}

	mutex_lock(&ua->lock);
	ua_v12_aceface_first(ua);
	ret = ua_dsp_connect_all(ua);
	if (!ret && !ua_uses_audio_extension(ua->device_type))
		(void)ua_dsp_replay_if_pending(ua, "dsp connect ioctl");
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
	if (blk.data_size > UA_FW_MAX_SIZE)  /* 4 MiB, as LoadFirmware */
		return -EINVAL;

	/* Allocate kernel buffer for data payload */
	if (blk.data_size > 0) {
		data_buf = kvmalloc(blk.data_size, GFP_KERNEL);
		if (!data_buf)
			return -ENOMEM;

		if (copy_from_user(data_buf,
				   (void __user *)(unsigned long)blk.data,
				   blk.data_size)) {
			kvfree(data_buf);
			return -EFAULT;
		}
	}

	/* Send block via DSP ring buffer */
	mutex_lock(&ua->lock);
	ret = ua_dsp_send_block(ua, blk.dsp_index, blk.cmd, blk.param,
				data_buf, blk.data_size);
	mutex_unlock(&ua->lock);

	kvfree(data_buf);
	return ret;
}

/*
 * UA_IOCTL_DMA_TEST.  The struct carries 1 KiB of capture data, so it is
 * kept off ua_ioctl_dispatch()'s stack (the frame went past 1024 bytes
 * with gcc 15) and built at -O2 for the same bogus GCC 12 copy-size
 * error as the IO map below.
 */
static noinline UA_GCC_OPTIMIZE("O2,no-omit-frame-pointer") long
ua_ioctl_dma_test(struct ua_device *ua, unsigned long arg)
{
	struct ua_dma_test *dt;
	long ret;

	dt = memdup_user((void __user *)arg, sizeof(*dt));
	if (IS_ERR(dt))
		return PTR_ERR(dt);

	mutex_lock(&ua->lock);
	ret = ua_audio_dma_test(ua, dt);
	mutex_unlock(&ua->lock);

	if (!ret && copy_to_user((void __user *)arg, dt, sizeof(*dt)))
		ret = -EFAULT;
	kfree(dt);
	return ret;
}

/*
 * UA_IOCTL_GET_IO_MAP: firmware IO descriptors + ALSA channel map.
 * Built at -O2: under this file's -Og, GCC 12 (Debian 12, 6.1 headers)
 * reports a bogus "copy source size is too small" for the heap buffer.
 */
static noinline UA_GCC_OPTIMIZE("O2") long
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

/*
 * Raw register, DMA, DSP-ring and firmware ioctls can point the device's
 * DMA at any memory or wedge the DSP, so they need CAP_SYS_RAWIO.  The
 * mixer daemon's ioctls (device info, readback, mixer params and
 * settings, driver params, IO map) stay open to whoever may open the node.
 */
static bool ua_ioctl_needs_rawio(unsigned int cmd)
{
	switch (cmd) {
	case UA_IOCTL_WRITE_REG:
	case UA_IOCTL_RESET_DMA:
	case UA_IOCTL_CLI_COMMAND:
	case UA_IOCTL_LOAD_FIRMWARE:
	case UA_IOCTL_DSP_CONNECT:
	case UA_IOCTL_DSP_TEST_DMA:
	case UA_IOCTL_DSP_TEST_SEND:
	case UA_IOCTL_DSP_SEND_BLOCK:
	case UA_IOCTL_SEND_ROUTING:
	case UA_IOCTL_RING_SEND:
	case UA_IOCTL_LOAD_FW_DSP:
	case UA_IOCTL_DSP_SRAM_DATA:
	case UA_IOCTL_DSP_SEND_RAW:
	case UA_IOCTL_DMA_TEST:
		return true;
	default:
		return false;
	}
}

static long ua_ioctl_dispatch(struct ua_device *ua, unsigned int cmd,
			      unsigned long arg)
{
	switch (cmd) {
	case UA_IOCTL_GET_DEVICE_INFO:
		return ua_ioctl_get_device_info(ua, arg);

	case UA_IOCTL_GET_VERSION:
		return ua_ioctl_get_version(ua, arg);

	case UA_IOCTL_GET_SYSTEM_INFO:
		return ua_ioctl_get_system_info(ua, arg);

	case UA_IOCTL_GET_PAUSE_INFO:
		return ua_ioctl_get_pause_info(ua, arg);

	case UA_IOCTL_GET_PLUGIN_CAPS:
		return ua_ioctl_get_plugin_caps(ua, arg);

	case UA_IOCTL_GET_CLOCK_SOURCES:
		return ua_ioctl_get_clock_sources(ua, arg);

	case UA_IOCTL_SET_MIXER_BUS_PARAMS:
		return ua_ioctl_set_mixer_bus_params(ua, arg);

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

	case UA_IOCTL_LOAD_FW_DSP:
		/*
		 * Retired: the kext loads firmware to DSP 0 only and DSPs 1+
		 * boot themselves.  The number stays reserved so a stale tool
		 * fails cleanly.
		 */
		return -ENOTTY;

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

	case UA_IOCTL_DMA_TEST:
		return ua_ioctl_dma_test(ua, arg);

	default:
		return -ENOTTY;
	}
}

static long ua_ioctl(struct file *filp, unsigned int cmd, unsigned long arg)
{
	struct ua_file *uf = filp->private_data;
	struct ua_device *ua = uf->ua;
	long ret;

	if (ua_ioctl_needs_rawio(cmd) && !capable(CAP_SYS_RAWIO))
		return -EPERM;

	/* Held across the ioctl so remove() can wait it out (ioctl_sem) */
	down_read(&ua->ioctl_sem);
	if (ua->removed || atomic_read(&ua->shutdown))
		ret = -ENODEV;
	else
		ret = ua_ioctl_dispatch(ua, cmd, arg);
	up_read(&ua->ioctl_sem);
	return ret;
}

static const struct file_operations ua_fops = {
	.owner		= THIS_MODULE,
	.open		= ua_event_open,
	.release	= ua_event_release,
	.read		= ua_event_read,
	.poll		= ua_event_poll,
	.fasync		= ua_event_fasync,
	.unlocked_ioctl	= ua_ioctl,
#ifdef CONFIG_COMPAT
	.compat_ioctl	= compat_ptr_ioctl,
#endif
};

/* ----------------------------------------------------------------
 * DSP init and the cold-start firmware stage
 *
 * The "firmware" stage sends ua-apollo-mixer.bin (captured SEL127 Bill
 * resource streams) to DSP 0 as 0x12 transactions.  The macOS kext sends
 * nothing at start; what the stage does on a cold x4 is unproven, so it
 * stays.  See ua_dsp_load_mixer_blocks().
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

	/* DSP 0's boot word (+0x1A4, the kext's ready word) and +0x1AC */
	dev_info(&pdev->dev,
		 "pre-init: SEQ_WR=%u SEQ_RD=%u mixer_state=0x%08x boot=0x%08x/0x%08x\n",
		 seq_wr, seq_rd, mixer_state, ua_read(ua, 0x1A4),
		 ua_read(ua, 0x1AC));

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
		ua_check_dsps(ua, false);
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
	dev_dbg(&pdev->dev,
		"post-init: SEQ_WR=%u SEQ_RD=%u\n", seq_wr, seq_rd);

	/*
	 * Always allocate DSP ring buffers — needed for firmware
	 * loading on cold boot AND for routing config on warm boot.
	 * On resume the existing rings are reprogrammed instead.
	 */
	/* Kext ProgramRegisters waits for the DSP boot word first (D14). */
	if (dsp_was_dead)
		(void)ua_dsp_wait_ready(ua);

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

		/*
		 * The firmware stage sends ua-apollo-mixer.bin (captured SEL127
		 * "Bill" streams) to DSP 0 as 0x12 transactions.  The kext sends
		 * nothing at start, and on titan's x4 every one of the 15 blocks
		 * comes back with DSP status 0xF0010006 (kext result -68) on
		 * every cold start.  Without the stage two cold starts (Apollo
		 * power cycles) connected, ran every rate and the mixer, so it
		 * is off by default; mixer_fw_stage=1 brings it back.
		 */
		if (mixer_fw_stage) {
			fw_ret = ua_dsp_load_mixer_blocks(ua);
		} else {
			dev_info(&pdev->dev,
				 "mixer firmware stage skipped (mixer_fw_stage=0)\n");
			fw_ret = 0;
		}

		/*
		 * DSP 0 only.  DSPs 1+ need no firmware: the kext never sends
		 * any (its _sendBlock is hard-wired to DSP 0), they boot on
		 * their own.  The Windows ring traffic seen on them is plugin
		 * and strip work.
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
			int plugin_ret = ua_plugin_chain_enabled() ?
				ua_dsp_load_plugin_payloads(ua) : 0;

			if (plugin_ret && plugin_ret != -ENOENT)
				dev_warn(&pdev->dev,
					 "plugin chain fw load failed: %d\n",
					 plugin_ret);

			/*
			 * No DSP program upload here any more.  The old
			 * DSP program step (command 0x00360000 on
			 * DSP 0) has no counterpart in the kext, never got a
			 * response on the x4 (sendBlock timed out after 300
			 * ms, "DSP program load failed: -110" on every cold
			 * start, titan 2026-10-10), and the cold start works
			 * the same without it.
			 */

			if (ua_uses_audio_extension(ua->device_type)) {
				/*
				 * If the early ACEFACE handshake did not
				 * complete, retry it once with the transport
				 * still running.  (An old x4 note had it fail
				 * before the firmware stage on every cold
				 * boot; on titan 2026-10-11 it connected
				 * first time on three cold starts, with and
				 * without the stage.)
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

			if (ua_needs_ring_connect(ua) &&
			    !ua_ring_connect_deferred(ua)) {
				/* v12_connect: ACEFACE before ring connect */
				ua_v12_aceface_first(ua);

				fw_ret = ua_dsp_connect_retry(ua);
				/*
				 * Non-AudioExtension devices connect in
				 * probe; activate the plugin chain now so
				 * its DSP programs load before userspace
				 * opens the device.  AudioExtension devices
				 * defer activation to ua_audio_connect()
				 * (after ACEFACE completes).
				 */
				if (fw_ret == 0 && !ua->plugins_activated &&
				    ua_plugin_chain_enabled()) {
					int pc_ret =
						ua_dsp_activate_plugin_chain(ua);
					if (pc_ret)
						dev_warn(&pdev->dev,
							 "plugin chain activate failed: %d\n",
							 pc_ret);
					else {
						ua->plugins_activated = true;
						ua->dspx.replay_pending = true;
					}
				}
				/*
				 * Non-AudioExtension: the DSP is connected
				 * now.  AudioExtension devices replay from
				 * ua_audio_connect() (ring_connect_deferred).
				 */
				if (fw_ret == 0 &&
				    !ua_uses_audio_extension(ua->device_type) &&
				    ua_dsp_replay_if_pending(ua, "dsp init"))
					dev_warn(&pdev->dev,
						 "bus coefficient replay incomplete\n");
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
				dev_dbg(&pdev->dev,
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

	seq_wr = ua_read(ua, UA_REG_MIXER_SEQ_WR);
	seq_rd = ua_read(ua, UA_REG_MIXER_SEQ_RD);
	dev_dbg(&pdev->dev,
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
 * or 0x0B (bit mask 0xc20).  Every other device uses MSI.  The Windows INF
 * ships MSI for 0x0A/0x0B, so those two follow satellite_msi (default MSI).
 */
static bool ua_wants_intx(struct pci_dev *pdev)
{
	if (pdev->device == PCI_DEVICE_ID_UA_UAD2)
		return true;

	switch (pdev->subsystem_device) {
	case 0x0A:
	case 0x0B:
		/* UAD-2 Satellite TB: INTx on macOS, MSI on Windows */
		return !satellite_msi;
	case 0x05:
		return true;
	default:
		return false;
	}
}

/*
 * The chardev's release: the last reference is gone (remove() has run and
 * /dev/ua_apolloN is closed everywhere), so nothing can reach the struct.
 */
static void ua_chardev_release(struct device *dev)
{
	struct ua_device *ua = container_of(dev, struct ua_device, chardev);

	pci_dev_put(ua->pdev);
	kfree(ua);
}

/*
 * Probe failed after ua_dsp_init_and_load(): stop the device as remove()
 * does, then free what probe allocated (the ALSA card when @audio, the
 * DSP rings and the buffers parked on them, plugin payloads, the audio
 * DMA rings).  @audio: ua_audio_init() was called, successfully or not
 * (it sets up its work items and timer before anything can fail).
 *
 * The order is remove()'s: the service loop and the period timers
 * stopped, the kext Shutdown sequence and TRANSPORT = 0 under ua->lock,
 * the shutdown flag raised before the lock is dropped (a PCM opened on
 * the card ua_audio_init() registered, or the IRQ, cannot reach BAR0 and
 * restart anything after that), an IRQ handler or thread already running
 * waited for (remove() frees the IRQ at this point; here it stays until
 * the caller's free_irq()), 100 ms for the firmware, bus mastering off,
 * and only then ua_audio_fini() and the frees.  Buffers parked by a
 * timed-out ring command are freed only when the device can no longer
 * write them, as remove() decides (gone, or DMA engines reset by
 * full_shutdown); otherwise they are leaked, since the next probe turns
 * bus mastering back on.  The audio rings used to be freed by
 * ua_audio_fini() with bus mastering still on and the DSP rings live.
 */
static void ua_probe_unwind_dsp(struct ua_device *ua, bool audio)
{
	bool dead = !pci_device_is_present(ua->pdev);

	if (audio) {
		ua_dsp_service_stop(ua);
		ua_audio_period_wd_cancel(ua);
		ua_audio_stop_period_timer(ua);
	}

	mutex_lock(&ua->lock);
	ua_hw_quiesce_locked(ua);
	ua_write(ua, UA_REG_TRANSPORT, 0);
	atomic_set(&ua->shutdown, 1);
	mutex_unlock(&ua->lock);
	synchronize_irq(pci_irq_vector(ua->pdev, 0));
	msleep(100);
	pci_clear_master(ua->pdev);

	if (audio)
		ua_audio_fini(ua);
	ua_dsp_rings_fini(ua, dead || full_shutdown);
	ua_dsp_free_plugin_payloads(ua);
	ua_audio_free_dma(ua);
}

/*
 * Interrupt self-test (N22, parity/02 testInterrupt), log only.
 *
 * The kext's testInterrupt is not an IRQ test (parity/13 R49: it refreshes
 * plugin authorisation), and the hardware has no software-trigger vector:
 * ISR is write-1-to-clear only, and no register is known that raises a
 * vector on demand.  Triggering the notification vector would need a DSP
 * action that is not harmless (a clock write or a transport change), so
 * this test is passive: it writes NOTHING to the device.  It reports
 * whether any interrupt of ours (an enabled, unmasked vector, as seen by
 * ua_irq_handler) arrived since request_irq, waiting up to 50 ms for one.
 * After a normal probe the connect and DSP load have already raised some,
 * so the wait is usually zero; a quiet device logs "no interrupt", which
 * on an idle unit is information, not a fault.  The result never fails the
 * probe.
 */
static bool irq_selftest = true;
module_param(irq_selftest, bool, 0444);
MODULE_PARM_DESC(irq_selftest,
		 "Log at the end of probe whether an interrupt reached the handler since request_irq, waiting up to 50 ms for one (passive: no device register is written; default: on)");

static void ua_irq_selftest(struct ua_device *ua, u64 req_ns)
{
	u64 first;
	int i;

	if (!irq_selftest)
		return;
	for (i = 0; i < 50; i++) {
		first = atomic64_read(&ua->irq_first_ns);
		if (first)
			break;
		usleep_range(1000, 1500);
	}
	first = atomic64_read(&ua->irq_first_ns);
	if (first)
		dev_info(&ua->pdev->dev,
			 "interrupt self-test: ok (%llu us after request_irq, %u interrupt(s) so far)\n",
			 div_u64(first - req_ns, 1000), ua_irq_count);
	else
		dev_info(&ua->pdev->dev,
			 "interrupt self-test: no interrupt in 50 ms (passive test, nothing was triggered)\n");
}

static int ua_probe(struct pci_dev *pdev, const struct pci_device_id *id)
{
	struct ua_device *ua;
	u64 irq_req_ns;
	bool intx;
	int ret, devno;

	/*
	 * Not devm: the struct has to outlive remove() while /dev/ua_apolloN
	 * is still open; ua_chardev_release() frees it.
	 */
	ua = kzalloc(sizeof(*ua), GFP_KERNEL);
	if (!ua)
		return -ENOMEM;

	ua->pdev = pci_dev_get(pdev);
	ua->dev_id = -1;
	device_initialize(&ua->chardev);
	ua->chardev.class = ua_class;
	ua->chardev.parent = &pdev->dev;
	ua->chardev.release = ua_chardev_release;
	mutex_init(&ua->lock);
	spin_lock_init(&ua->irq_lock);
	init_rwsem(&ua->ioctl_sem);
	ua_event_init(ua);
	INIT_LIST_HEAD(&ua->dma_orphans);
	INIT_WORK(&ua->resume_work, ua_resume_work);
	INIT_LIST_HEAD(&ua->unit_node);
	atomic_set(&ua->pause_mask, 0);
	pci_set_drvdata(pdev, ua);

	/* Enable device */
	ret = pcim_enable_device(pdev);
	if (ret) {
		dev_err(&pdev->dev, "failed to enable PCI device: %d\n", ret);
		goto err_put;
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
		goto err_put;
	}

	/* Map BAR0 — kext asserts minimum 0x2800 (10 KiB) */
	ua->regs_size = pci_resource_len(pdev, 0);
	if (ua->regs_size < UA_MIN_BAR_SIZE) {
		dev_err(&pdev->dev, "BAR0 too small: %llu < %u\n",
			(unsigned long long)ua->regs_size, UA_MIN_BAR_SIZE);
		ret = -ENODEV;
		goto err_put;
	}

	ua->regs = pcim_iomap(pdev, 0, 0);
	if (!ua->regs) {
		dev_err(&pdev->dev, "failed to map BAR0\n");
		ret = -ENOMEM;
		goto err_put;
	}

	ua->regs_phys = pci_resource_start(pdev, 0);

	/* Read and cache FPGA revision — first thing the kext does after mapping */
	ua->fpga_rev = ua_read(ua, UA_REG_FPGA_REV);
	ua->subsystem_id = pdev->subsystem_device;

	/* Detect firmware version, device type, and DSP count from hardware regs */
	ret = ua_detect_capabilities(ua);
	if (ret)
		goto err_put;

	/* tools/contribute/device-probe.sh parses this line; keep its shape */
	dev_info(&pdev->dev,
		 "%s: FPGA rev 0x%08x, subsys 0x%04x, %u DSPs, FW %s\n",
		 ua_device_name(ua->device_type),
		 ua->fpga_rev, ua->subsystem_id, ua->num_dsps,
		 ua->fw_v2 ? "v2" : "v1");
	dev_info(&pdev->dev, "device type 0x%02x, public type 0x%02x%s\n",
		 ua->device_type, ua->public_type,
		 ua->public_type == UA_PUBTYPE_NONE ? " (unresolved)" : "");
	{
		u32 rel = ua_read(ua, UA_REG_FW_RELEASE);

		/* UA's Mixer Engine shows this word "%d.%02d.%03d" */
		dev_info(&pdev->dev,
			 "firmware %u.%02u.%03u (BAR0 0x0008 = 0x%08x)\n",
			 UA_FW_MAJOR(rel), UA_FW_MINOR(rel), UA_FW_BUILD(rel),
			 rel);
	}

	if (ua->fw_v2) {
		/*
		 * CPcieDevice::MapHardware creates the audio extension only
		 * for ext_caps bit 31 set and family != 6 (row B6).  No such
		 * product is supported, and v1 parts keep their old path, so
		 * this is only reported.
		 */
		if (!(ua->ext_caps & BIT(31)) ||
		    ((ua->ext_caps >> 20) & 0x3f) == 6)
			dev_warn(&pdev->dev,
				 "ext_caps 0x%08x: the macOS driver creates no audio extension for this device\n",
				 ua->ext_caps);

		/*
		 * DSP 0 window +0x1A4 (boot word, d14) and +0x1AC (the
		 * device-info word the kext publishes for ring-connect
		 * types, row G2), as the previous owner left them.
		 */
		dev_info(&pdev->dev,
			 "DSP0 window: +0x1A4 = 0x%08x, +0x1AC = 0x%08x\n",
			 ua_read(ua, 0x1A4), ua_read(ua, 0x1AC));
	}

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
	ua_dsp_cache_firmware(ua, mixer_fw_stage, ua_plugin_chain_enabled());

	/*
	 * Interrupt type, as UAD2Pcie2::_initializeInterrupts (kext 12.0.0
	 * x86 0xbe40): legacy INTx for device 0x0001 (UAD-2 PCIe) and for
	 * subsystems 0x05, 0x0A and 0x0B; MSI for every other device.
	 * 0x0A/0x0B use MSI unless satellite_msi=0 (Windows INF).
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
		ret = ua->num_irq_vectors;
		goto err_put;
	}

	/* An INTx line may be shared; the handler returns IRQ_NONE for
	 * interrupts that are not ours */
	ret = request_threaded_irq(pci_irq_vector(pdev, 0), ua_irq_handler,
				   ua_irq_thread, intx ? IRQF_SHARED : 0,
				   DRIVER_NAME, ua);
	if (ret) {
		dev_err(&pdev->dev, "failed to request IRQ: %d\n", ret);
		goto err_free_irq_vectors;
	}

	irq_req_ns = ktime_get_ns();

	ret = ua_dsp_init_and_load(ua, false, !ua_mixer_dsp_alive(ua));
	if (ret)
		goto err_dsp;

	/* Initialize audio subsystem (ALSA + DMA) */
	ret = ua_audio_init(ua);
	if (ret) {
		dev_err(&pdev->dev, "audio init failed: %d\n", ret);
		/* Its work items and timer exist: the audio-side unwind */
		goto err_audio;
	}

	/* The kext's _checkDSPs is the tail of Connect(): fatal there */
	ret = ua_check_dsps(ua, true);
	if (ret)
		goto err_audio;

	/*
	 * Character device /dev/ua_apolloN, last: an ioctl may reach the
	 * service loop, the period timer and the ALSA state, which
	 * ua_audio_init() has set up by now.
	 */
	devno = ida_alloc_max(&ua_ida, UA_MAX_DEVICES - 1, GFP_KERNEL);
	if (devno < 0) {
		dev_err(&pdev->dev, "no free device numbers\n");
		ret = devno;
		goto err_audio;
	}
	ua->dev_id = devno;
	ua->chardev.devt = MKDEV(MAJOR(ua_devno), devno);
	ret = dev_set_name(&ua->chardev, "ua_apollo%d", devno);
	if (ret)
		goto err_ida;

	cdev_init(&ua->cdev, &ua_fops);
	ua->cdev.owner = THIS_MODULE;

	/* In the group before the node exists, so no ioctl sees a stranger */
	ua_unit_register(ua);
	ret = cdev_device_add(&ua->cdev, &ua->chardev);
	if (ret) {
		dev_err(&pdev->dev, "cdev_device_add failed: %d\n", ret);
		ua_unit_unregister(ua);
		goto err_ida;
	}

	dev_info(&pdev->dev, "registered as /dev/ua_apollo%d\n", devno);
	ua_irq_selftest(ua, irq_req_ns);
	return 0;

err_ida:
	ida_free(&ua_ida, devno);
err_audio:
	ua_probe_unwind_dsp(ua, true);
	goto err_irq;
err_dsp:
	ua_probe_unwind_dsp(ua, false);
err_irq:
	free_irq(pci_irq_vector(pdev, 0), ua);
err_free_irq_vectors:
	pci_free_irq_vectors(pdev);
err_put:
	put_device(&ua->chardev);
	return ret;
}

static void ua_remove(struct pci_dev *pdev)
{
	struct ua_device *ua = pci_get_drvdata(pdev);
	bool dead = !pci_device_is_present(pdev);

	dev_info(&pdev->dev, "ua_remove: surprise=%d\n", dead);

	/* Clients stop audio, then the unit leaves the group (master
	 * re-election) before anything is torn down */
	ua_pause_set(ua, UA_PAUSE_DEVICE);
	ua_unit_unregister(ua);

	/*
	 * 0. Wait out an ioctl in flight and refuse new ones: nothing below
	 *    may race an ioctl into the rings, buffers or registers it frees.
	 */
	down_write(&ua->ioctl_sem);
	ua->removed = true;
	up_write(&ua->ioctl_sem);
	ua_event_offline(ua, dead);

	/*
	 * 1. Stop the software that touches BAR0 on its own: a pending
	 *    resume re-init, the DSP service loop (both block until the
	 *    handler returns — they take ua->lock, so this must happen
	 *    without the lock) and the period timer (process context,
	 *    hrtimer_cancel is safe).
	 */
	cancel_work_sync(&ua->resume_work);
	ua_dsp_service_stop(ua);
	ua_audio_period_wd_cancel(ua);
	ua_audio_stop_period_timer(ua);

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
	ua_audio_period_wd_cancel(ua);	/* first: it can start the timer */
	ua_audio_stop_period_timer(ua);
	ua_dsp_service_stop(ua);

	/*
	 * 6. Tear down audio subsystem — frees the ALSA card (waiting for
	 *    open PCM and control files to close) and the DMA buffers.
	 *    ua_write calls inside are noops (shutdown flag set), which is
	 *    correct for surprise removal.
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

	/*
	 * 9. Chardev and IDA.  An fd still open keeps the struct alive (its
	 *    ioctls return -ENODEV); the last reference frees it.
	 */
	cdev_device_del(&ua->cdev, &ua->chardev);
	ida_free(&ua_ida, ua->dev_id);
	put_device(&ua->chardev);
}

/* ----------------------------------------------------------------
 * PCIe error recovery — needed for Thunderbolt hot-plug
 * ---------------------------------------------------------------- */

static pci_ers_result_t ua_error_detected(struct pci_dev *pdev,
					  pci_channel_state_t state)
{
	struct ua_device *ua = pci_get_drvdata(pdev);

	dev_warn(&pdev->dev, "PCIe error detected (state %d)\n", state);
	ua_err_count(ua, UA_ERR_PCIE);

	/* Block all new BAR0 access */
	atomic_set(&ua->shutdown, 1);

	/*
	 * Stop the software that touches BAR0: a resume re-init, the DSP
	 * service loop.  The period timer (or, for period_irq=1 streams,
	 * the period watchdog) sees the flag and pushes the open streams
	 * into XRUN.
	 */
	cancel_work_sync(&ua->resume_work);
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

/*
 * The same bring-up as system resume (ua_resume()): with bus mastering
 * still off, stop the transport, mask the interrupts and pull the DSP
 * rings back to their positions; then the warm path (SG table and rings
 * reprogrammed, mixer DSP left running) when the DSP survived, or a cold
 * re-init deferred to resume_work.  The connect runs from resume_work,
 * queued by ua_io_resume().  The old code turned bus mastering on first,
 * reset all DMA engines (which kills a live mixer DSP), never pointed
 * the device back at the SG table or rings, and read the FPGA revision
 * through the shutdown guard, so it never recovered.
 */
static pci_ers_result_t ua_slot_reset(struct pci_dev *pdev)
{
	struct ua_device *ua = pci_get_drvdata(pdev);
	u32 fpga;
	int ret;

	dev_info(&pdev->dev, "slot reset\n");

	ret = pcim_enable_device(pdev);
	if (ret) {
		dev_err(&pdev->dev, "re-enable after slot reset failed: %d\n", ret);
		return PCI_ERS_RESULT_DISCONNECT;
	}
	pci_restore_state(pdev);

	/* The shutdown guard is still up: read past it */
	fpga = ioread32(ua->regs + UA_REG_FPGA_REV);
	if (fpga == 0xFFFFFFFF || fpga != ua->fpga_rev) {
		dev_err(&pdev->dev,
			"device not back after slot reset (FPGA revision 0x%08x)\n",
			fpga);
		return PCI_ERS_RESULT_DISCONNECT;
	}

	mutex_lock(&ua->lock);
	atomic_set(&ua->shutdown, 0);
	ua->aceface_done = false;
	ua->aceface_failed = false;
	ua->audio.connected = false;
	ua->audio.transport_running = false;

	ua_hw_stop_xport_irqs(ua);
	ua_dsp_rings_neutralize(ua);
	pci_set_master(pdev);

	if (ua_mixer_dsp_alive(ua)) {
		ret = ua_dsp_init_and_load(ua, true, false);
	} else {
		dev_info(&pdev->dev,
			 "mixer DSP not running after the reset, cold re-init deferred\n");
		ua->resume_need_init = true;
		ret = 0;
	}
	if (ret) {
		atomic_set(&ua->shutdown, 1);
		mutex_unlock(&ua->lock);
		dev_err(&pdev->dev, "re-init after slot reset failed: %d\n",
			ret);
		return PCI_ERS_RESULT_DISCONNECT;
	}
	ua->resume_pending = true;	/* pcm_prepare waits for the connect */
	mutex_unlock(&ua->lock);

	return PCI_ERS_RESULT_RECOVERED;
}

static void ua_io_resume(struct pci_dev *pdev)
{
	struct ua_device *ua = pci_get_drvdata(pdev);

	dev_info(&pdev->dev, "I/O resumed, reconnecting\n");

	/* The connect (and a deferred cold re-init), as after system resume */
	queue_work(system_freezable_wq, &ua->resume_work);
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
		ua_audio_period_wd_cancel(ua);
		ua_audio_stop_period_timer(ua);
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

	ua_pause_set(ua, UA_PAUSE_DEVICE);
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
	ua_pause_clear(ua, UA_PAUSE_DEVICE);

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
		ua_pause_clear(ua, UA_PAUSE_DEVICE);
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
