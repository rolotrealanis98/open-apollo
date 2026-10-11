/* SPDX-License-Identifier: GPL-2.0-only */
/*
 * Universal Audio Apollo Thunderbolt - Linux PCIe Driver
 *
 * Copyright (c) 2026 apollo-linux contributors
 *
 * Reverse engineered from UAD2System.kext v11.8.1
 */

#ifndef UA_APOLLO_H
#define UA_APOLLO_H

#include <linux/pci.h>
#include <linux/interrupt.h>
#include <linux/mutex.h>
#include <linux/rwsem.h>
#include <linux/cdev.h>
#include <linux/completion.h>
#include <linux/workqueue.h>
#include <linux/dma-mapping.h>
#include <linux/io.h>
#include <linux/kernel.h>
#include <linux/list.h>
#include <linux/spinlock.h>
#include <linux/kfifo.h>
#include <linux/wait.h>
#include <linux/fs.h>
/* <linux/hrtimer_types.h> only exists from ~6.9; hrtimer.h pulls it in */
#include <linux/hrtimer.h>
#include "ua_ioctl.h"
#include <sound/core.h>
#include <sound/pcm.h>

/*
 * Per-function optimisation levels that keep GCC 12/13 clear of internal
 * compiler errors and bogus warnings on a few large functions.  Clang has
 * no optimize attribute (it warns, and a -Werror kernel fails the build),
 * so they apply to GCC only.
 */
#ifdef __clang__
#define UA_GCC_OPTIMIZE(level)
#else
#define UA_GCC_OPTIMIZE(level)	__attribute__((optimize(level)))
#endif

/* PCI Device IDs */
#define PCI_VENDOR_ID_UA        0x1A00
#define PCI_DEVICE_ID_UA_UAD2   0x0001  /* UAD-2 PCIe cards */
#define PCI_DEVICE_ID_UA_APOLLO 0x0002  /* Apollo Thunderbolt series */

/* Subsystem IDs (select device model within family)
 * From PCI config space — more reliable than serial prefix for device
 * identification since serial strings can have ambiguous prefixes.
 */
#define UA_SUBSYS_APOLLO_X4_QUAD        0x0011
#define UA_SUBSYS_APOLLO_SOLO           0x000F
#define UA_SUBSYS_APOLLO_8P             0x0006
#define UA_SUBSYS_APOLLO_X8P            0x0014  /* x8p; fallback if serial unresolved */

/*
 * Internal device type (what the kext calls the serial type) — the value
 * returned by _deviceTypeFromSerialNumber() and named by CPcieDevice::Name().
 *
 * v1 devices: type derived from FPGA rev register (0x2218) bits[31:28] - 1
 * v2 devices: ext_caps register (0x2234) bits[25:20] - 1 selects a family;
 *             x-series families are then refined via the serial prefix table.
 *
 * DSP variant (SOLO/DUO/QUAD/HEXA/OCTO) comes from ext_caps bits[15:8].
 *
 * Names below follow UAD2System.kext 12.0.0: the serial table at
 * __TEXT,__const 0x3f800 (x86_64 slice, 22 entries) and the Name() jump
 * tables.  Earlier revisions of this header (from a v11 analysis) put the
 * wrong model on most HEXA and Gen 2 types.
 *
 * "x16D per FPGA check": in the HEXA family Name() calls a serial type
 * outside 0x20-0x23 / 0x35-0x3a an x16D only when
 * _deviceTypeFromFPGAVersion() returns 0x35, else "Unknown serial number";
 * that check is not ported.  "unnamed in v12": Name() has no case for the
 * type; the attribution comes from the older analysis (and, for the Solo,
 * the subsystem pin).  The observed x8p unit reads "2008 2017 ...".
 */

/* Original (pre-x) Thunderbolt device types (ext_caps family) */
#define UA_DEV_APOLLO_8P            0x0A  /* Subsys 0x0006, serial 1526xxxx */

/* x-series Thunderbolt device types (serial prefix table) */
#define UA_DEV_SERIAL_2016          0x1E  /* Serial 2016; x16D per FPGA check */
#define UA_DEV_APOLLO_X4            0x1F  /* Serial 2005 */
#define UA_DEV_APOLLO_X6            0x20  /* Serial 2019 */
#define UA_DEV_APOLLO_X8            0x21  /* Serial 2018 */
#define UA_DEV_APOLLO_X8P           0x22  /* Serial 2017 */
#define UA_DEV_APOLLO_X16           0x23  /* Serial 2020 */
#define UA_DEV_APOLLO_SOLO          0x27  /* Serial 2024; unnamed in v12 */
#define UA_DEV_ARROW                0x28  /* Serial 2025; unnamed in v12 */
#define UA_DEV_SERIAL_2032          0x2A  /* Serial 2032; x16D per FPGA check */
#define UA_DEV_APOLLO_X16D          0x35  /* Serial 2073/2082 */
#define UA_DEV_APOLLO_X4_GEN2       0x36  /* Serial 2092 */
#define UA_DEV_APOLLO_X6_GEN2       0x37  /* Serial 2086 */
#define UA_DEV_APOLLO_X8_GEN2       0x38  /* Serial 2087 */
#define UA_DEV_APOLLO_X8P_GEN2      0x39  /* Serial 2088 */
#define UA_DEV_APOLLO_X16_GEN2      0x3A  /* Serial 2089 */
#define UA_DEV_APOLLO_TWIN_X_GEN2   0x3B  /* Serial 2090 */
#define UA_DEV_APOLLO_TWIN_X_GEN2_B 0x3C  /* Serial 2091 */
#define UA_DEV_SERIAL_2110          0x3E  /* Serial 2110 (v12); x16D per FPGA check */
#define UA_DEV_APOLLO_X4_GEN2_TB4   0x3F  /* Serial 2109 (v12) */
#define UA_DEV_APOLLO_TWIN_X_GEN2_TB4   0x40  /* Serial 2107 (v12) */
#define UA_DEV_APOLLO_TWIN_X_GEN2_TB4_B 0x41  /* Serial 2108 (v12) */

/*
 * AudioExtension vs legacy ring-buffer connect mechanism.
 *
 * From kext CPcieDevice::Connect() disassembly: a 40-bit bitmask at
 * offset 0xe2dc determines which device types use _connectToDsps()
 * (ring buffer inline connect commands) vs CPcieAudioExtension::Connect()
 * (ACEFACE register-based handshake at 0xC02C/0x2260, bank-shifted).
 *
 * Devices NOT in the bitmask use the AudioExtension path.
 * Apollo x4 (0x1F=31) and x6 (0x1E=30) are AudioExtension devices.
 *
 * Bitmask 0x7f17fcf9b0 → ring buffer connect types:
 *   4,5,7,8,11-15,18-26,28,32-38
 * AudioExtension types (not in mask):
 *   0-3,6,9,10,16,17,27,29-31, and all types >= 39
 */
#define UA_RING_CONNECT_BITMASK_LO  0x17fcf9b0UL  /* bits 0-31 */
#define UA_RING_CONNECT_BITMASK_HI  0x7fUL         /* bits 32-38 */

static inline bool ua_uses_audio_extension(u32 device_type)
{
	if (device_type >= 64)
		return true; /* Unknown high type — assume AudioExtension */
	if (device_type >= 32)
		return !(UA_RING_CONNECT_BITMASK_HI &
			 (1UL << (device_type - 32)));
	return !(UA_RING_CONNECT_BITMASK_LO & (1UL << device_type));
}

/*
 * UAD2System.kext 12.0.0 widened the ring-connect set: CPcieDevice::Connect
 * (x86 0x5f84) and HardResetDSPs (0x44f2) test type <= 0x29 against
 * 0x3ff17fcf9b0, adding 0x27 (Apollo Solo TB), 0x28 (Arrow) and 0x29 to the
 * v11.8.1 set above.  Only consulted with the v12_connect module parameter.
 */
#define UA_RING_CONNECT_V12_BITMASK_HI  0x3ffUL     /* bits 32-41 */
#define UA_RING_CONNECT_V12_MAX_TYPE    0x29

static inline bool ua_ring_connect_v12(u32 device_type)
{
	if (device_type > UA_RING_CONNECT_V12_MAX_TYPE)
		return false;
	if (device_type >= 32)
		return UA_RING_CONNECT_V12_BITMASK_HI &
		       (1UL << (device_type - 32));
	return UA_RING_CONNECT_BITMASK_LO & (1UL << device_type);
}

/*
 * Public device type: the kext's UAD2DeviceType as CPcieDevice::
 * GetCapabilities (x86 0x464c) computes it -- ext_caps family refined by
 * the serial type -- and stores in CPcieDevice+0xc48.  Every kext mask
 * test (ring connect in Connect/StartTransport, HardResetDSPs,
 * SetOutputMute) runs on this number, not on the serial-table type that
 * ua_device.device_type holds (x4: public 0x1A, serial type 0x1F).
 * ua_compute_public_type() in ua_core.c; parity/11 section 2.3.
 */
#define UA_PUBTYPE_NONE		0xFFFFFFFFu	/* GetCapabilities: -1 */
#define UA_PUBTYPE_X4		0x1A
#define UA_PUBLIC_RING_CONNECT_MASK	0x3ff17fcf9b0ULL	/* types <= 0x29 */
#define UA_PUBLIC_OUTPUT_MUTE_MASK	0x8010080000ULL		/* 0x13, 0x1C, 0x27 */
#define UA_PUBLIC_OUTPUT_MUTE_MAX	0x27

static inline bool ua_public_ring_connect(u32 pt)
{
	return pt <= UA_RING_CONNECT_V12_MAX_TYPE &&
	       (UA_PUBLIC_RING_CONNECT_MASK >> pt) & 1;
}

static inline bool ua_public_output_mute(u32 pt)
{
	return pt <= UA_PUBLIC_OUTPUT_MUTE_MAX &&
	       (UA_PUBLIC_OUTPUT_MUTE_MASK >> pt) & 1;
}

/*
 * Serial prefix register: hardware registers at BAR0 + 0x20..0x2C
 * contain a 16-byte serial string. ASCII digits 5-8 (serial + 4, NOT the
 * leading four) are the model prefix matched by _deviceTypeFromSerialNumber()
 * and by ua_read_serial_type().
 *
 * The leading four digits are a separate field whose meaning is unknown, but
 * they are demonstrably not a model ID: matching against them misidentifies
 * hardware. Two observed units:
 *
 *   Apollo x4   "2019 2005 01xxxx"  -> matches 2005 -> 0x1F, correct
 *   Apollo x8p  "2008 2017 00xxxx"  -> matches 2017 -> 0x22, correct
 *
 * Note the x4's leading group is 2019, which is this table's x6 entry: read
 * the wrong four digits and a QUAD x4 identifies as an x6.  The x8p used to
 * look like a collision with the x8 only because this header named 0x22 an
 * x8; the kext names 0x22 the x8p, so the serial resolves it directly.
 */
#define UA_REG_SERIAL_BASE          0x0020
#define UA_REG_SERIAL_LEN           16

/*
 * BAR0 Register Map
 * Extracted from CPcieDevice, CPcieIntrManager, CPcieRingBuffer disassembly.
 * All offsets from BAR0 base.
 */

/* Global / Identification */
#define UA_REG_FW_VERSION       0x0000  /* Firmware version / ID */
#define UA_REG_HW_UID_LO        0x0030  /* Hardware UID low 32 bits */
#define UA_REG_HW_UID_HI        0x0034  /* Hardware UID high 32 bits */
#define UA_REG_FPGA_REV         0x2218  /* FPGA revision (cached on init) */
#define UA_REG_EXT_CAPS         0x2234  /* Extended caps (v2 FW), DSP count bits[15:8] */
#define UA_REG_SERIAL           0x2238  /* Serial number / device info */

/* Interrupt Controller (CPcieIntrManager) */
#define UA_REG_DMA_CTRL         0x2200  /* DMA engine control (reset/enable bitmask) */
#define UA_REG_IRQ_ENABLE       0x2204  /* IMR — interrupt mask register (low 32 vectors) */
#define UA_REG_IRQ_STATUS       0x2208  /* ISR — interrupt status register (write-back to ack) */
#define UA_REG_WALLCLOCK        0x220C  /* Wallclock / interrupt pending */

/* Transport / DSP Control */
#define UA_REG_DSP_RESET        0x221C  /* DSP reset control */
#define UA_REG_TRANSPORT        0x2220  /* Transport control (0 = stop) */

/* Extended Interrupt (v2 firmware, 64 vectors total) */
#define UA_REG_EXT_IRQ_STATUS   0x2264  /* ISR_HI — interrupt status register (high 32) */
#define UA_REG_EXT_IRQ_ENABLE   0x2268  /* IMR_HI — interrupt mask register (high 32 vectors) */

/*
 * Audio Extension Transport Registers (CPcieAudioExtension)
 * These control DMA streaming, NOT the device mixer.
 */
#define UA_REG_AX_BUF_SIZE      0x2240  /* Buffer frame size - 1 */
#define UA_REG_AX_SAMPLE_POS    0x2244  /* Current sample position */
#define UA_REG_AX_CONTROL       0x2248  /* Main control: 0=stop, 1=arm, 0xF=start */
#define UA_REG_AX_P2P_ROUTE     0x224C  /* Peer-to-peer routing */
#define UA_REG_AX_PLAY_CH       0x2250  /* Total playback channel count */
#define UA_REG_AX_FRAME_CTR     0x2254  /* Frame counter (read) */
#define UA_REG_AX_FRAME_CFG     0x2258  /* Frame counter config */
#define UA_REG_AX_REC_CH        0x225C  /* Total record channel count */
#define UA_REG_AX_CONNECT       0x2260  /* Connection: 1=connect, 4=clock, 0x10=disconnect */
#define UA_REG_AX_DMA_BLKSZ     0x226C  /* DMA block size */
#define UA_REG_AX_TIMER_CFG     0x2270  /* Periodic timer interval */

/* Audio Extension Status — transport fence only, NOT notification status!
 * Notification status is at UA_REG_NOTIF_STATUS (0xC030, bank-shifted).
 * This register is used as a read fence in PrepareTransport step 12. */
#define UA_REG_AX_STATUS        0x22C0  /* Transport status (read fence only) */

/* Peer-to-peer route enable mask (kext EnablePeerToPeer writes the slot
 * mask, ClearPeerToPeerAudioRoutings and AudioExtension::Shutdown write 0).
 * 0 = no peer routes; Linux never sets it. */
#define UA_REG_AX_P2P_ENABLE    0x22C4

/* Audio Clock */
#define UA_REG_P2P_SLOT0        0x2280  /* Peer-to-peer slot address 0 (multi-unit; not a clock source) */
#define UA_REG_P2P_SLOT1        0x2284  /* Peer-to-peer slot address 1 */

/*
 * DSP Mixer Registers (BAR0 + 0x3800 window)
 *
 * The mixer engine uses a sequence-counter handshake to update settings:
 *   1. Read MIXER_SEQ_RD (wait for DSP idle: RD == WR)
 *   2. Write paired (value, mask) words to MIXER_SETTING[index]
 *   3. Increment and write MIXER_SEQ_WR
 *   4. Poll MIXER_SEQ_RD until DSP acknowledges (RD == new WR)
 *
 * 38 settings × 8 bytes each (two 32-bit words per setting).
 * Setting registers span 0x38B4..0x39EC.
 *
 * Paired word encoding:
 *   wordA = (mask[15:0]  << 16) | value[15:0]
 *   wordB = (mask[31:16] << 16) | value[31:16]
 */
#define UA_REG_MIXER_BASE       0x3800  /* Mixer register window base */
#define UA_REG_MIXER_SEQ_WR     0x3808  /* Mixer sequence counter (host → DSP) */
#define UA_REG_MIXER_SEQ_RD     0x380C  /* Mixer sequence counter (DSP readback) */
#define UA_MIXER_SETTING_STRIDE 8       /* 2 × u32 per setting */
#define UA_MIXER_NUM_SETTINGS   52      /* Total mixer settings (per SEL189) */
#define UA_MIXER_BATCH_COUNT    38      /* Settings flushed per batch (kext CPcieDeviceMixer) */
#define UA_REG_MIXER_LAST       0x3A5C  /* Last mixer setting register (setting 51, word 1) */

/* CLI polling limits */
#define UA_CLI_TIMEOUT_POLLS    500     /* × 1ms = 500ms timeout */
#define UA_MIXER_SEQ_TIMEOUT    100     /* × 1ms = 100ms DSP ack timeout */

/* DSP service loop interval (readback drain as host liveness signal).
 * Kext SEL119 ProcessPlugin runs at ~103Hz (~10ms).  Must be fast
 * enough to keep the DSP processing loop cycling.  */
#define UA_DSP_SERVICE_INTERVAL_MS  10

/* Audio Extension Control bits for UA_REG_AX_CONTROL */
#define UA_AX_CTRL_DMA_EN       BIT(0)  /* DMA enable */
#define UA_AX_CTRL_PLAY_EN      BIT(1)  /* Playback enable */
#define UA_AX_CTRL_REC_EN       BIT(2)  /* Record enable */
#define UA_AX_CTRL_IRQ_EN       BIT(3)  /* Interrupt enable */
#define UA_AX_CTRL_EXT_MODE     BIT(9)  /* Extended mode (FW >= 10) */
#define UA_AX_CTRL_START        0x0F    /* Start all (DMA+play+rec+irq) */
#define UA_AX_CTRL_START_EXT    0x20F   /* Start all + extended mode */

/* Connection doorbell values for UA_REG_AX_CONNECT */
#define UA_AX_CONNECT           1       /* Initiate connection */
#define UA_AX_CLOCK_DOORBELL    4       /* Trigger clock change notification */
#define UA_AX_DISCONNECT        0x10    /* Initiate disconnection */
#define UA_AX_CONNECT_MAGIC     0xACEFACE  /* Handshake magic written to notification reg */

/* DMA Scatter-Gather Registers (4 pages x 64-bit addresses) */
#define UA_REG_PLAY_DMA_BASE    0x8000  /* Playback DMA descriptor base */
#define UA_REG_REC_DMA_BASE     0xA000  /* Record DMA descriptor base */
#define UA_DMA_SG_ENTRIES       1024    /* 4 MiB / 4K pages = 1024 entries per direction */
#define UA_DMA_BUF_SIZE         0x400000  /* 4 MiB DMA buffer per direction */

/*
 * Notification / SRAM Registers
 *
 * CRITICAL: Bank Offset (from kext CPcieAudioExtension disassembly)
 *
 * Apollo x4 uses bank_shift = 0x0A.  Many notification/config registers
 * are accessed at:  BAR0 + bank_shift * 4 + base_offset
 *                 = BAR0 + 0x28 + base_offset  (for bank_shift=0x0A)
 *
 * Register layout:
 *   CONFIG WRITE (NO bank shift — fixed at BAR0+0xC000):
 *     +0x0000: config[0] = bank_shift (written after connect)
 *     +0x0004: config[1] = readback_count (0x17C = 380)
 *     +0x0008..+0x0024: config[2..9] = 0
 *
 *   CONFIG/STATUS READ (WITH bank shift):
 *     +0x0000 (=0xC028): Readback buffer (up to 95 dwords)
 *     +0x0004 (=0xC02C): ACEFACE write target
 *     +0x0008 (=0xC030): NOTIFICATION STATUS (the critical read!)
 *     +0x004C (=0xC074): Sample rate / clock source write
 *     +0x0054 (=0xC07C): Rate info readback
 *     +0x0058 (=0xC080): Transport info readback
 *     +0x005C (=0xC084): Clock source info readback
 *
 *   IO DESCRIPTORS (NO bank shift — fixed offsets):
 *     +0x01A4: Input IO descriptors (72 dwords)
 *     +0x02C4: Output IO descriptors (72 dwords)
 */
#define UA_REG_NOTIF_BASE       0xC000  /* Notification config/status area */
#define UA_NOTIF_BANK_SHIFT     0x0A    /* Bank shift for Apollo x4 (slot ID) */
#define UA_NOTIF_BANK_OFFSET    (UA_NOTIF_BANK_SHIFT * 4)  /* = 0x28 bytes */

/* Bank-shifted notification registers (add UA_NOTIF_BANK_OFFSET to base) */
#define UA_REG_NOTIF_ACEFACE    (UA_REG_NOTIF_BASE + UA_NOTIF_BANK_OFFSET + 0x04)  /* 0xC02C */
#define UA_REG_NOTIF_STATUS     (UA_REG_NOTIF_BASE + UA_NOTIF_BANK_OFFSET + 0x08)  /* 0xC030 */
#define UA_REG_NOTIF_CLOCK_CFG  (UA_REG_NOTIF_BASE + UA_NOTIF_BANK_OFFSET + 0x4C)  /* 0xC074 */
#define UA_REG_NOTIF_RATE_INFO  (UA_REG_NOTIF_BASE + UA_NOTIF_BANK_OFFSET + 0x54)  /* 0xC07C */
#define UA_REG_NOTIF_XPORT_INFO (UA_REG_NOTIF_BASE + UA_NOTIF_BANK_OFFSET + 0x58)  /* 0xC080 */
#define UA_REG_NOTIF_CLOCK_INFO (UA_REG_NOTIF_BASE + UA_NOTIF_BANK_OFFSET + 0x5C)  /* 0xC084 */
/* Clock sources the device offers: bits 16-31, bit n = source value n */
#define UA_REG_NOTIF_CLOCK_SRCS (UA_REG_NOTIF_BASE + UA_NOTIF_BANK_OFFSET + 0x64)  /* 0xC08C */
#define UA_REG_NOTIF_READBACK   (UA_REG_NOTIF_BASE + UA_NOTIF_BANK_OFFSET + 0x00)  /* 0xC028 */

/* Config write area (NO bank shift — always at 0xC000) */
#define UA_REG_NOTIF_CONFIG     UA_REG_NOTIF_BASE  /* 0xC000 */
#define UA_NOTIF_CONFIG_WORDS   10      /* 10 dwords written after connect */
#define UA_NOTIF_READBACK_WORDS 95      /* 95 dwords read back after connect */
#define UA_NOTIF_READBACK_COUNT 0x17C   /* config[1] = readback buffer word count */

/* Legacy defines — kept for reference but DO NOT USE for notification I/O */
#define UA_REG_CLOCK_CFG_BASE   0xC04C  /* WRONG: missing bank shift. Use UA_REG_NOTIF_CLOCK_CFG */

/* IO descriptor areas (NO bank shift — fixed offsets) */
#define UA_REG_IN_NAMES_BASE    0xC1A4  /* Input IO descriptor (72 x u32) */
#define UA_REG_OUT_NAMES_BASE   0xC2C4  /* Output IO descriptor (72 x u32) */

/*
 * IO descriptor layout (firmware-written; the kext only reads it).
 *
 * CPcieAudioExtension::_handleNotificationInterrupt copies all 72 words
 * of each area when notification bit 0 (output) / bit 1 (input) is set,
 * and after connect (bit 21 handling ORs in 0x400003).  Word layout:
 *
 *   [0]  1           (version)
 *   [1]  0x46        (70 = words that follow)
 *   [2]  0xFFFFFFFF
 *   [3]  input slot count   (meaningful in the input area)
 *   [4]  output slot count  (meaningful in the output area)
 *   [5]  2 in the input area, 0 in the output area (not used by the kext)
 *   [6+] u16 channel IDs, slot order, low half-word first
 *
 * The kext takes the count from word 3 (input, this+0x1d4) / word 4
 * (output, this+0x2f8), clamps it to 128 for _parseIONames(), and uses it
 * for AX_REC_CH / AX_PLAY_CH and the DMA frame stride.  Channel ID =
 * UAChanType << 8 | number (1-based); the type byte indexes the devNames
 * table (see ua_chan_name()).  The macOS-captured x4 descriptor holds 34
 * input and 48 output slots, the x8p's 34 and 52.
 */
#define UA_IO_DESC_WORDS        72
#define UA_IO_DESC_HDR_WORDS    6
#define UA_IO_DESC_VERSION      1
#define UA_IO_DESC_LENGTH       0x46
#define UA_IO_DESC_IN_COUNT     3       /* word index of the input count */
#define UA_IO_DESC_OUT_COUNT    4       /* word index of the output count */
#define UA_IO_DESC_FLAGS        5

/*
 * AX_*_CH extra channel (kext PrepareTransport, CPcieAudioExtension
 * +0x28b8): a flag channel is added only when the diagnostic flags set by
 * SetDiagnosticFlags() ask for it — play gets +1 when (flags & 6), record
 * +1 when (flags & 4), and P2P_ROUTE is written only when (flags & 2).
 * The constructor zeroes the flags and nothing but the diagnostic
 * selector changes them, so a normal macOS session runs with 0.
 */
#define UA_AX_DIAG_FLAGS        0
#define UA_REG_CLI_ENABLE       0xC3F4  /* CLI enable register */
#define UA_REG_CLI_STATUS       0xC3F8  /* CLI command status / length */
#define UA_REG_CLI_RESP_LEN     0xC3FC  /* CLI response length */
#define UA_REG_CLI_CMD_BUF      0xC400  /* CLI command data buffer */
#define UA_REG_CLI_RESP_BUF     0xC480  /* CLI response data buffer */

/*
 * Interrupt Vector Numbers
 *
 * The kext uses logical vector indices (0-71) mapped to hardware bit
 * positions (0-63) via m_vectorMap[72].  Logical vectors 0x28, 0x46, 0x47
 * are used for audio, but their hardware bit positions need to be
 * determined from the live kext or by trial.
 *
 * For now, use the logical vector numbers for documentation.  The actual
 * hardware enable bits are set in ua_audio_enable_irqs() which tries
 * enabling common bit positions until we can dump the real m_vectorMap.
 */
#define UA_IRQ_VEC_NOTIFICATION 0x28    /* Audio extension notification (logical 40) */
#define UA_IRQ_VEC_PERIODIC     0x46    /* Periodic timer (logical 70) */
#define UA_IRQ_VEC_END_BUFFER   0x47    /* End-of-buffer / DMA completion (logical 71) */

/* DSP Bank Addressing */
#define UA_DSP_BANK_LOW         0x2000  /* DSP 0-3: base = 0x2000 + idx*0x80 */
#define UA_DSP_BANK_HIGH        0x5E00  /* DSP 4-7: base = 0x5E00 + idx*0x80 */
#define UA_DSP_BANK_STRIDE      0x0080  /* 128 bytes per DSP */

/* Ring Buffer Per-Channel Registers (offsets from channel base) */
#define UA_RING_PAGE0_LO        0x00    /* Descriptor page 0 addr low */
#define UA_RING_PAGE0_HI        0x04    /* Descriptor page 0 addr high */
#define UA_RING_PAGE1_LO        0x08    /* Descriptor page 1 addr low */
#define UA_RING_PAGE1_HI        0x0C    /* Descriptor page 1 addr high */
#define UA_RING_PAGE2_LO        0x10    /* Descriptor page 2 addr low */
#define UA_RING_PAGE2_HI        0x14    /* Descriptor page 2 addr high */
#define UA_RING_PAGE3_LO        0x18    /* Descriptor page 3 addr low */
#define UA_RING_PAGE3_HI        0x1C    /* Descriptor page 3 addr high */
#define UA_RING_WRITE_PTR       0x20    /* Host write pointer */
#define UA_RING_READ_PTR        0x24    /* Read pointer / doorbell */
#define UA_RING_DOORBELL        0x24    /* Write doorbell — triggers DSP processing */
/* Note: UA_RING_READ_PTR (0x24) is actually the WRITE doorbell per kext RE */
#define UA_RING_POSITION        0x28    /* HW current position */

/* Ring buffer entry format */
#define UA_RING_ENTRY_SIZE      16      /* 4 × u32 per entry */
#define UA_RING_ENTRIES_PER_PAGE (UA_PCIE_PAGE_SIZE / UA_RING_ENTRY_SIZE) /* 256 */
#define UA_RING_CMD_OFFSET      0x00    /* Command ring within DSP bank */
#define UA_RING_RESP_OFFSET     0x40    /* Response ring within DSP bank */
/*
 * Every ring is 4 pages = 1024 entries and circular, as in the kext:
 * CPcieRingBuffer::Initialize (12.0.0 x86 0xa84e) allocates 0x4000 bytes
 * for every command and response ring, Service (0xad12) wraps the index
 * with & 0x3ff and treats a POSITION >= 0x400 as invalid.  A 1-page ring
 * would leave pages 1-3 programmed as bus address 0 for the FPGA to walk.
 */
#define UA_RING_NUM_PAGES       4
#define UA_FW_RING_NUM_PAGES    4       /* DSP 0 cmd ring (same as all rings) */
#define UA_RING_DMA_REF         BIT(31) /* BIT31=1 in word0 → DMA reference entry */

/* Firmware loading */
#define UA_FW_CMD               0x120000
#define UA_FW_PARAM             0x80040000
#define UA_FW_LARGE_THRESHOLD   0x3FFFC     /* ≥256KB-4: use two-word command */
#define UA_FW_LARGE_FLAG        0x40000000  /* BIT30 set for large transfers */
#define UA_FW_RESP_MATCH        0x8004      /* Upper 16 bits of response word 0 */
#define UA_FW_TIMEOUT_MS        150000      /* kext LoadFirmware: 150 s */
#define UA_BLOCK_TIMEOUT_MS     5000        /* kext authorization/demo blocks: 5 s */
#define UA_FW_POLL_MS           10          /* Poll every 10ms */
#define UA_FW_MAX_SIZE          (4 * 1024 * 1024)

/* DSP connect */
#define UA_DSP_CMD_CONNECT      0x00230002
#define UA_DSP_CMD_FINALIZE     0x00100002

/*
 * Mixer blob format (ua-apollo-mixer.bin): captured SEL127 "Bill" resource
 * streams, sent to DSP 0 as 0x12 transactions.  Not a DSP boot image.
 */
#define UA_MIXER_FW_MAGIC       0x5541464D      /* 'UAFM' */
#define UA_MIXER_FW_VERSION     1
#define UA_MIXER_FW_NAME        "ua-apollo-mixer.bin"

/* Plugin-chain firmware blob format (ua-apollo-plugin-chain.bin).
 * Contains captured DMA_REF payloads from the Windows driver; the
 * kernel substitutes each payload's Linux dma_addr into cmd[2]/cmd[3]
 * of the ring entries before submitting the plugin chain.  Without
 * this, DMA_REF entries in ua_plugin_chain.h carry raw Windows host
 * physical addresses (0x9_492Cxxxx..0x9_493Fxxxx) that do not exist
 * on Linux — the FPGA fetches garbage and any DSP program loaded via
 * the plugin chain (including the DSP→ARM flag-forwarder that drives
 * PAD/48V/MicLine relays) never initializes. */
#define UA_PLUGIN_CHAIN_FW_MAGIC    0x43504155  /* 'UAPC' */
#define UA_PLUGIN_CHAIN_FW_VERSION  1
#define UA_PLUGIN_CHAIN_FW_NAME     "ua-apollo-plugin-chain.bin"

/* DMA Constants */
#define UA_PCIE_PAGE_SIZE       0x1000  /* 4 KiB page alignment for DMA */
#define UA_RING_MAX_SIZE        0x0400  /* Max 1024 descriptors per ring */
#define UA_RING_MAX_PAGES       4       /* Max pages per ring */
#define UA_MIN_BAR_SIZE         0x2800  /* Minimum BAR size (10 KiB) */

/* Interrupt Bitmasks */
#define UA_IRQ_CLEAR_ALL        0xFFFFFFFF
#define UA_DMA_RESET_V1_MASK    0x1E00   /* Reset strobes [12:9] */
#define UA_DMA_RESET_V1_CLR     0x1E1E   /* Enables [4:1] + strobes [12:9] */
#define UA_DMA_RESET_V2_MASK    0x1FE00  /* Reset strobes [16:9] */
#define UA_DMA_RESET_V2_CLR     0x1FFFE  /* Enables [8:1] + strobes [16:9] */

/* Max device type value (from bitmask analysis) */
#define UA_MAX_DEVICE_TYPE      38

/* Max DSPs per device */
#define UA_MAX_DSPS             8

/* Max preamp channels across all Apollo models */
#define UA_MAX_PREAMP_CH        8
#define UA_MAX_KCTLS            96      /* ALSA controls ua_mixer_init() adds */

/*
 * Preamp parameter IDs (from DTrace SEL131 SetMixerParam captures).
 * These go through DSP mixer settings, NOT through ARM CLI.
 * chan_type=1, chan_idx=preamp channel, param_id=one of these.
 *
 * Verified via DTrace + live hardware 2026-02-16:
 *   Mic/Line=0x00 (1=Line, 0=Mic)
 *   PAD=0x01 (on=1, off=0)
 *   48V=0x03 (on=1, off=0 — triggers ARM safety blink sequence)
 *   LowCut=0x04 (on=1, off=0)
 *   Phase=0x05 (invert=0xbf800000 (-1.0f), normal=0x3f800000 (+1.0f))
 *   Gain=0x06/0x09/0x0A (three gain params, written as triplet)
 *   HiZ: NO software control — hardware auto-detect only (rb_data[0] bit 24)
 */
#define UA_PREAMP_PARAM_MIC_LINE 0x00   /* 1=Line, 0=Mic */
#define UA_PREAMP_PARAM_PAD     0x01
#define UA_PREAMP_PARAM_48V     0x03
#define UA_PREAMP_PARAM_LOWCUT  0x04    /* NOT HiZ! Verified via DTrace */
#define UA_PREAMP_PARAM_PHASE   0x05    /* Float: -1.0f=invert, +1.0f=normal */
/*
 * The gain is written as a triplet.  GainC (0x06, dB-9) is the 6-bit field
 * the readback reports in word 3; GainA (0x0A) and GainB (0x09) are dB-10.
 * (This header used to call 0x06 "A" and 0x0A "C", opposite to the Mixer
 * Engine's names and to what the ALSA cache update keyed on.)
 */
#define UA_PREAMP_PARAM_GAIN_C  0x06    /* GainC: dB-9, 6 bits, in readback */
#define UA_PREAMP_PARAM_GAIN_B  0x09    /* GainB: dB-10 */
#define UA_PREAMP_PARAM_GAIN_A  0x0A    /* GainA: dB-10 */
/* Mixer gain range the preamps take (dB) */
#define UA_PREAMP_GAIN_MIN_DB   10
#define UA_PREAMP_GAIN_MAX_DB   65
/*
 * The rest of the SetInputParam (SEL131 type 1) ids, from the kext's
 * setInputParamARM jump table (parity/14-mixer-params.md s3).  Settings are
 * mailbox indices; "S+12/8" means the +12 (flags) or +8 (byte lanes)
 * channel 4-7 bank, which no x4 channel reaches.
 */
#define UA_PREAMP_PARAM_LINK    0x02    /* stereo link, even channel only: both pair bits */
#define UA_PREAMP_PARAM_MIC_IMP 0x07    /* mic impedance code, S1 nibble 9/16/20/24 */
#define UA_PREAMP_PARAM_CUSTOM  0x08    /* custom (Unison) preamp on, S11 bit 8+ch */
/* 0x09-0x0f: custom-preamp byte lanes S16-S22 (GainB 0x09, GainA 0x0a) */
#define UA_PREAMP_PARAM_KNOB_STEPS 0x10 /* preamp knob steps, S23 nibble */
#define UA_PREAMP_PARAM_HIZ_IMP 0x11    /* HiZ impedance code, S13 nibble, inputs 1/2 only */
#define UA_PREAMP_PARAM_SEL_PARAM 0x12  /* preamp selected param, S23 2-bit lane at 16+2*ch */
#define UA_PREAMP_PARAM_ROUTE   0x13    /* DSP ring: direct-output route mask (default 0x1ffff) */
#define UA_PREAMP_PARAM_HIZ     0x14    /* HiZ host write, S0 bit 24+ch */
#define UA_PREAMP_PARAM_PGA_LINE 0x15   /* not on the x4 (kind 14): rejected */
#define UA_PREAMP_PARAM_LEVEL   0x16    /* DSP ring: span info (0x20 = none) */
#define UA_PREAMP_PARAM_RAW_REC 0x17    /* raw record enable, S5 bit 16+ch */

/*
 * DSP-side input parameters: ring command {0x001E0004, input, sub, value}.
 * Param 0x05 (phase scale) reaches the ring on channels >= 4 of the x4;
 * 0x13 and 0x16 always do.  A route of UA_DSP_ROUTE_NONE means "not routed
 * from this DSP".
 */
#define UA_DSP_INPUT_SCALE      5
#define UA_DSP_INPUT_ROUTE      7
#define UA_DSP_INPUT_SPAN       8
#define UA_DSP_ROUTE_NONE       0x0001FFFF
#define UA_DSP_SPAN_NONE        0x20

/* Legacy aliases — do NOT use for new code */
#define UA_ARM_PARAM_PAD        UA_PREAMP_PARAM_PAD
#define UA_ARM_PARAM_48V        UA_PREAMP_PARAM_48V
#define UA_ARM_PARAM_LOWCUT     UA_PREAMP_PARAM_LOWCUT
#define UA_ARM_PARAM_PHASE      UA_PREAMP_PARAM_PHASE

/* ARM hardware ID encoding: channel index → hw_id */
#define UA_ARM_HW_ID(ch)        ((ch) * 0x800)

/*
 * Monitor parameter IDs (from IOKit SEL131 captures + kext SetOtherParam
 * disassembly, ch_type=2).  Grouped by DSP mixer setting index.
 */

/* --- Setting[0]: Output pad flags --- */
#define UA_MON_PARAM_OUT_PAD_A  0x19  /* ch_idx=1, Line 1-2 pad, bool */
#define UA_MON_PARAM_OUT_PAD_B  0x1a  /* ch_idx=1, Line 3-4 pad, bool */
#define UA_MON_PARAM_OUT_PAD_C  0x1b  /* setting[0] bit30, bool */
#define UA_MON_PARAM_OUT_PAD_D  0x1c  /* setting[0] bit31, bool */

/* --- Setting[1]: SR convert + misc 3-bit fields --- */
#define UA_MON_PARAM_UNKNOWN_13 0x13  /* setting[1] bits[30:28], 3-bit */
#define UA_MON_PARAM_UNKNOWN_14 0x14  /* setting[1] bits[15:13], 3-bit */
#define UA_MON_PARAM_SR_CONVERT 0x1f  /* ch_idx=1, S/PDIF SR converter, bool */

/* --- Setting[2]: Monitor core (level/mute/dim/source/CUE) --- */
#define UA_MON_PARAM_LEVEL      0x01  /* ch_idx=1, raw 8-bit (192+dB*2) */
#define UA_MON_PARAM_HP1_VOL    0x02  /* ch_idx=1, HP1 volume, 8-bit */
#define UA_MON_PARAM_MUTE       0x03  /* ch_idx=0, 2=muted/0=unmuted; 1=mono */
#define UA_MON_PARAM_SOURCE     0x04  /* ch_idx=1, 0=MIX/1=CUE1/2=CUE2 */
#define UA_MON_PARAM_CUE1_MIX   0x05  /* ch_idx=0, 0=on/2=off (inverted!) */
#define UA_MON_PARAM_CUE1_MONO  0x06  /* ch_idx=0, 1=mono (field bit 21), 2=bit 20; swapped */
#define UA_MON_PARAM_CUE2_MIX   0x07  /* ch_idx=0, 0=on/2=off */
#define UA_MON_PARAM_CUE2_MONO  0x08  /* ch_idx=0, 1=mono (field bit 23), 2=bit 22; swapped */
#define UA_MON_PARAM_UNKNOWN_15 0x15  /* setting[2] bit28, bool */
#define UA_MON_PARAM_DIM        0x44  /* ch_idx=0, 1=on/0=off, bit31 */

/* --- Setting[4]: Mono (dev_type==3 ONLY, NOP on x4) --- */
#define UA_MON_PARAM_MONO       0x36  /* kext property ID (NOT used on x4) */
#define UA_MON_PARAM_MIX_TO_MONO UA_MON_PARAM_MUTE /* uses param 0x03 val=1 */
/* Param 0x03 value bits: UA_MON_MUTE_ON (2) = mute (bit 16), UA_MON_MONO (1)
 * = mono (bit 17); 3 sets both, as the front panel can */
#define UA_MON_MONO             1

/* --- Setting[6]: Mirror A / Identify / MirrorsToDigital --- */
/* Aux bus 1 MixToMono; swapped 2-bit field (value bit 0 = mono = bit 9) */
#define UA_MON_PARAM_AUX1_MONO  0x0a  /* setting[6] bits[9:8], 2-bit */
#define UA_MON_PARAM_UNKNOWN_0A UA_MON_PARAM_AUX1_MONO
#define UA_MON_PARAM_IDENTIFY   0x1d  /* ch_idx=0, identify/locate, 3-bit value */
#define UA_MON_PARAM_DIGITAL_MIRROR 0x1e /* ch_idx=9, MirrorsToDigital */
#define UA_MON_PARAM_CUE1_MIRROR 0x2e /* ch_idx=9, MirrorA value, 8-bit */

/* --- Setting[7]: Mirror B / TBConfig / misc --- */
/* Aux bus 2 MixToMono; swapped like 0x0a */
#define UA_MON_PARAM_AUX2_MONO  0x0f  /* setting[7] bits[9:8], 2-bit */
#define UA_MON_PARAM_UNKNOWN_0F UA_MON_PARAM_AUX2_MONO
#define UA_MON_PARAM_UNKNOWN_20 0x20  /* setting[7] bit10, bool (flush) */
/* Param 0x20 is the output mute: CPcieDevice::SetOutputMute() (kext 12.0.0
 * x86 0x6140) calls CUAD2DeviceMixer::SetOtherParam(0x20, mute). */
#define UA_MON_PARAM_OUTPUT_MUTE UA_MON_PARAM_UNKNOWN_20
#define UA_MON_PARAM_CUE2_MIRROR 0x2f /* ch_idx=9, MirrorB value, 8-bit */
#define UA_MON_PARAM_UNKNOWN_45 0x45  /* setting[7] bit11, bool */
#define UA_MON_PARAM_TB_CONFIG  0x47  /* ch_idx=0, talkback config, bit12 */
#define UA_MON_PARAM_UNKNOWN_48 0x48  /* setting[7] bit13, bool */
#define UA_MON_PARAM_UNKNOWN_63 0x63  /* setting[7] bit14, bool */

/* --- Setting[8]: Digital out mode / CUE alt --- */
#define UA_MON_PARAM_DIGITAL_MODE 0x21 /* ch_idx=0, 0=S/PDIF/8=ADAT */
#define UA_MON_PARAM_UNKNOWN_22 0x22  /* setting[8] bits[29:28], 2-bit */
/* 0x23 / 0x25 are MixToMono of output synch 0x18 / 0x1a (swapped 2-bit
 * fields), not mix sources; 0x22 / 0x24 are those outputs' mix source. */
#define UA_MON_PARAM_SYNCH18_MONO 0x23 /* setting[8] bits[25:24], swapped */
#define UA_MON_PARAM_UNKNOWN_24 0x24  /* setting[8] bits[31:30], 2-bit */
#define UA_MON_PARAM_SYNCH1A_MONO 0x25 /* setting[8] bits[27:26], swapped */
#define UA_MON_PARAM_CUE1_MIX_ALT UA_MON_PARAM_SYNCH18_MONO	/* old name */
#define UA_MON_PARAM_CUE2_MIX_ALT UA_MON_PARAM_SYNCH1A_MONO	/* old name */

/* --- Setting[11]: Mirror config / OutputRef --- */
#define UA_MON_PARAM_MIRROR_CFG_A 0x2a /* setting[11] bit0, bool; kind-gated: -EINVAL on the x4 */
#define UA_MON_PARAM_MIRROR_CFG_B 0x2b /* setting[11] bit1, bool */
#define UA_MON_PARAM_MIRROR_CFG_C 0x2c /* setting[11] bit2, bool */
#define UA_MON_PARAM_MIRROR_CFG_D 0x2d /* setting[11] bit3, bool */
#define UA_MON_PARAM_UNKNOWN_30 0x30  /* setting[11] bits[23:16], 8-bit */
#define UA_MON_PARAM_UNKNOWN_31 0x31  /* setting[11] bits[31:24], 8-bit */
#define UA_MON_PARAM_OUTPUT_REF 0x32  /* ch_idx=1, 0=+4dBu/1=-10dBV */
#define UA_MON_PARAM_UNKNOWN_33 0x33  /* setting[11] bit5, bool; 0x33-0x35 kind-gated: -EINVAL on the x4 */
#define UA_MON_PARAM_UNKNOWN_34 0x34  /* setting[11] bit6, bool */
#define UA_MON_PARAM_UNKNOWN_35 0x35  /* setting[11] bit7, bool */

/* --- Setting[12]: HP cue source --- */
#define UA_MON_PARAM_HP1_SOURCE 0x3f  /* ch_idx=1, 0=CUE1/1=CUE2 */
#define UA_MON_PARAM_HP2_SOURCE 0x40  /* ch_idx=1, 0=CUE1/1=CUE2 */

/* --- Setting[13]: Talkback / misc --- */
#define UA_MON_PARAM_ALTMON_SELECT 0x3a /* setting[13] bits[29:28], kPropAltMonSelection */
#define UA_MON_PARAM_UNKNOWN_3A UA_MON_PARAM_ALTMON_SELECT
#define UA_MON_PARAM_UNKNOWN_42 0x42  /* setting[13] bits[15:13], FuncSwitchMode; -EINVAL on the x4 */
#define UA_MON_PARAM_TALKBACK   0x46  /* ch_idx=0, val=1 ON / val=0 OFF */

/* --- Setting[14]: Dim level / misc --- */
#define UA_MON_PARAM_DIM_LEVEL  0x43  /* ch_idx=0, 1-7 stepped */
#define UA_MON_PARAM_UNKNOWN_49 0x49  /* setting[14] bit31, Enable24dBMode; -EINVAL on the x4 */
/* CRMonitorLevel of the HP-class outputs 0x02 does not cover (HP2, ...) */
#define UA_MON_PARAM_HP_LEVEL   0x66  /* setting[14] bits[7:0], 8-bit */
#define UA_MON_PARAM_UNKNOWN_66 UA_MON_PARAM_HP_LEVEL

/*
 * --- Setting[15]: Alt Monitor ---
 * kPropAltMonTrim 0xf4 (clamp(trim*2 + 60.5, 0, 0x78)), AltMonEnabled 0xf6,
 * AltMonSelection 0xf5 (S13).  Medium confidence (parity/14: names resolved
 * from Mixer Engine call sites).  The ALSA "CUE n Mirror Switch" controls
 * and the daemon's "MirrorEnable" write 0x3b / 0x3c: those are these
 * AltMonEnabled bits, whatever the control is called.
 */
#define UA_MON_PARAM_ALTMON1_TRIM 0x38 /* setting[15] bits[6:0], 7-bit */
#define UA_MON_PARAM_ALTMON2_TRIM 0x39 /* setting[15] bits[22:16], 7-bit */
#define UA_MON_PARAM_ALTMON1_ENABLE 0x3b /* setting[15] bit 8 */
#define UA_MON_PARAM_ALTMON2_ENABLE 0x3c /* setting[15] bit 12 */
#define UA_MON_PARAM_UNKNOWN_38 UA_MON_PARAM_ALTMON1_TRIM
#define UA_MON_PARAM_UNKNOWN_39 UA_MON_PARAM_ALTMON2_TRIM
#define UA_MON_PARAM_MIRROR_ENABLE_A UA_MON_PARAM_ALTMON1_ENABLE /* old name */
#define UA_MON_PARAM_MIRROR_ENABLE_B UA_MON_PARAM_ALTMON2_ENABLE /* old name */
#define UA_MON_PARAM_UNKNOWN_3E 0x3e  /* setting[15] bit9, kPropCRPGABypass (inverted) */
#define UA_MON_PARAM_REMOTE_MON 0x41  /* setting[15] bit7, kPropRemoteMonitorController */
#define UA_MON_PARAM_UNKNOWN_41 UA_MON_PARAM_REMOTE_MON

/* --- Setting[5]: Sound/config flags (type >= 0xa) --- */
#define UA_MON_PARAM_UNKNOWN_67 0x67  /* setting[5] bit3, bool */
#define UA_MON_PARAM_UNKNOWN_68 0x68  /* setting[5] bit4, bool */
#define UA_MON_PARAM_UNKNOWN_87 0x87  /* setting[5] bit9, bool */
#define UA_MON_PARAM_HP2_CORRECTION 0x69 /* setting[5] bit5; kinds 10-14,16-20,22 */
#define UA_MON_PARAM_SURR_LFE_TRIM 0x88 /* setting[5] bits[31:24]; kinds 10-22 */
#define UA_MON_PARAM_SURR_FLAG_89 0x89  /* 0x89/0x8a: setting[5] bits 10/11; kinds 10-22 */

/*
 * --- Surround / HP-correction trim and flag families (parity/14 s4) ---
 * First id of each run handled by the ua_other_fields[] table in ua_core.c.
 */
#define UA_MON_PARAM_SURR_TRIM_2  0x4d  /* 0x4d-0x4e: setting[33] bytes 2,3 */
#define UA_MON_PARAM_SURR_TRIM_4  0x4f  /* 0x4f-0x52: setting[34] bytes 0-3 */
#define UA_MON_PARAM_SURR_FLAG_55 0x55  /* 0x55-0x5a: setting[32] bits 26-31 */
#define UA_MON_PARAM_SURR_FLAG_5D 0x5d  /* 0x5d-0x62: setting[32] bits 18-23 */
#define UA_MON_PARAM_ROOT_65      0x65  /* setting[31] bits[25:24] (low confidence) */
#define UA_MON_PARAM_SURR_TRIM_6B 0x6b  /* 0x6b-0x6e: setting[36] bytes 0-3 */
#define UA_MON_PARAM_SURR_TRIM_6F 0x6f  /* 0x6f-0x72: setting[37] bytes 0-3 */
#define UA_MON_PARAM_CHAN_FLAG_73 0x73  /* 0x73-0x7a: setting[35] bits 16-23 */
#define UA_MON_PARAM_CHAN_FLAG_7B 0x7b  /* 0x7b-0x82: setting[35] bits 8-15 */

/* --- Setting[35] (0x23): Channel config count --- */
#define UA_MON_PARAM_CHAN_CFG   0x6a  /* setting[35] bits[4:0], 5-bit */

/* --- NOP params (cache-only, no WriteSetting in kext) --- */
#define UA_MON_PARAM_DSP_SPANNING  0x16   /* ch_idx=0, DSP pairing mode */
/* 0x0b, 0x0c, 0x0d, 0x10, 0x11, 0x12, 0x17, 0x18, 0x26-0x29 */


/* Monitor mute values (NOT boolean — uses 2/0 encoding) */
#define UA_MON_MUTE_ON          2
#define UA_MON_MUTE_OFF         0

/*
 * Complete Bus ID Map (verified via DTrace Phase 3e, 2026-02-19)
 * Used in SEL130 (SetMixerBusParam) for fader/pan/send control.
 */
#define UA_BUS_ANALOG_IN(n)     (n)           /* 0x0000-0x0003 */
#define UA_BUS_CUE1_L           0x0004
#define UA_BUS_CUE1_R           0x0005
#define UA_BUS_CUE2_L           0x0006
#define UA_BUS_CUE2_R           0x0007
#define UA_BUS_SPDIF_IN_L       0x0008
#define UA_BUS_SPDIF_IN_R       0x0009
#define UA_BUS_TALKBACK1        0x000a
#define UA_BUS_TALKBACK2        0x000b
#define UA_BUS_ADAT_IN(n)       (((n) < 4) ? 0x000c + (n) : 0x0010 + (n))
#define UA_BUS_AUX1_RETURN      0x0010
#define UA_BUS_AUX2_RETURN      0x0012
#define UA_BUS_VIRTUAL_IN(n)    (0x0018 + (n))  /* 0x0018-0x001f */
#define UA_BUS_COUNT            32

/*
 * SEL130 sub-param IDs (verified via DTrace)
 * Each bus write uses one of these sub-parameters.
 */
#define UA_BUS_SUB_MAIN         0  /* control-room mix coefficient (fader x pan) */
#define UA_BUS_SUB_CUE1         1  /* HP1 (CUE 1) send level */
#define UA_BUS_SUB_CUE2         2  /* HP2 (CUE 2) send level */
#define UA_BUS_SUB_AUX1         3  /* Aux 1 send level */
#define UA_BUS_SUB_AUX2         4  /* Aux 2 send level */
#define UA_BUS_SUB_CUE3         5  /* HP3 (CUE 3) send level */
#define UA_BUS_SUB_CUE4         6  /* HP4 (CUE 4) send level */
/* Old names: 3 and 4 were never a gain L/R pair (the kext's Aux 1 / Aux 2) */
#define UA_BUS_SUB_GAIN_L       UA_BUS_SUB_AUX1
#define UA_BUS_SUB_GAIN_R       UA_BUS_SUB_AUX2

/*
 * Hardware Readback Field Macros
 * Extract individual fields from rb_data[] words returned by
 * UA_IOCTL_GET_HW_READBACK.
 */

/* rb_data[0]: Preamp flags, 6-bit stride per channel */
#define UA_RB_PREAMP_MICLINE(data0, ch)  (((data0) >> ((ch)*6 + 0)) & 1)
#define UA_RB_PREAMP_PAD(data0, ch)      (((data0) >> ((ch)*6 + 1)) & 1)
#define UA_RB_PREAMP_LINK(data0, ch)     (((data0) >> ((ch)*6 + 2)) & 1)
#define UA_RB_PREAMP_48V(data0, ch)      (((data0) >> ((ch)*6 + 3)) & 1)
#define UA_RB_PREAMP_LOWCUT(data0, ch)   (((data0) >> ((ch)*6 + 4)) & 1)
#define UA_RB_PREAMP_PHASE(data0, ch)    (((data0) >> ((ch)*6 + 5)) & 1)
#define UA_RB_HIZ_ACTIVE(data0)          (((data0) >> 24) & 1)

/*
 * HiZ detect: word 0 bit 24 is input 1 and bit 25 input 2 (bit 25 is not
 * hardware-confirmed yet); UA_RB_HIZ_ACTIVE() is input 1 only.
 */
#define UA_RB_HIZ_DETECT(data0, ch)      (((data0) >> (24 + (ch))) & 1)

/* rb_data[2]: Monitor section */
#define UA_RB_MON_VOLUME(data2)          ((data2) & 0xFF)
#define UA_RB_HP1_VOLUME(data2)          (((data2) >> 8) & 0xFF)
#define UA_RB_MON_MUTE(data2)            (((data2) >> 16) & 1)
#define UA_RB_MON_MONO(data2)            (((data2) >> 17) & 1)
#define UA_RB_MON_DIM(data2)             (((data2) >> 31) & 1)

/*
 * rb_data[3]: GainC, 6-bit fields at shifts 0/6/16/22 for inputs 1-4
 * (the same layout setting[3] is written with); dB = GainC + 9.  Bits
 * 15:12 are a gap and 31:28 track the front-panel selection.
 */
#define UA_RB_PREAMP_GAINC(data3, ch) \
	(((data3) >> (((ch) & 3) * 6 + (((ch) & 2) ? 4 : 0))) & 0x3F)
#define UA_RB_PREAMP_GAIN_DB(data3, ch)  (UA_RB_PREAMP_GAINC(data3, ch) + 9)

/* rb_data[0] 27/26: front-panel headphone focus (valid / on the headphone) */
#define UA_RB_HP_FOCUS_VALID(data0)      (((data0) >> 27) & 1)
#define UA_RB_HP_FOCUS_HP(data0)         (((data0) >> 26) & 1)
/* rb_data[2] bit 30: LowNoisePreamps */
#define UA_RB_LOW_NOISE(data2)           (((data2) >> 30) & 1)
/* rb_data[3] [30:28]: input selected on the front panel */
#define UA_RB_SEL_INPUT(data3)           (((data3) >> 28) & 7)

/* rb_data[6]: bit 8 talkback; [29:28] alt-monitor selection; bit 31 clock
 * locked, valid when bit 30 set */
#define UA_RB_TALKBACK(data6)            (((data6) >> 8) & 1)
#define UA_RB_ALTMON_SEL(data6)          (((data6) >> 28) & 3)

/* rb_data[17] [15:8]: temperature byte (kPropTemperature, raw) */
#define UA_RB_TEMPERATURE(data17)        (((data17) >> 8) & 0xFF)
/* rb_data[33] [28:26]: analog board id */
#define UA_RB_BOARD_ID(data33)           (((data33) >> 26) & 7)

/* rb_data[7]: bits 7:0 headphone 2 volume (same raw scale as the monitor) */
#define UA_RB_HP2_VOLUME(data7)          ((data7) & 0xFF)

/* Hardware Readback Registers (GetReadback in kext) */
#define UA_REG_MIXER_RB_STATUS  0x3810  /* Readback: 1=ready, write 0 to re-arm */
#define UA_REG_MIXER_RB_DATA    0x3814  /* Readback: 40 consecutive u32 words */
#define UA_MIXER_RB_WORDS       40

/* MSI vectors */
#define UA_MSI_VECTORS_MIN      1
#define UA_MSI_VECTORS_MAX      2

/* Audio sample format: 32-bit signed, little-endian */
#define UA_SAMPLE_BYTES         4

/* Maximum channels supported by any Apollo model */
#define UA_MAX_CHANNELS         64

/* Default channel counts (safe for all models) */
#define UA_DEFAULT_PLAY_CH      2
#define UA_DEFAULT_REC_CH       2

/* Max buffer frame size (from kext PrepareTransport) */
#define UA_MAX_BUF_FRAMES       8192

/*
 * Clock source values (clock_config low byte, kext UAD2ClockSource).  The
 * device names them at 0xC090 by the same number and offers the ones set
 * in UA_REG_NOTIF_CLOCK_SRCS (the x4: S/PDIF, ADAT, Peer2Peer 0x0A,
 * Internal).
 */
#define UA_CLOCK_SPDIF      0x00
#define UA_CLOCK_ADAT       0x05
#define UA_CLOCK_WORDCLOCK  0x07
#define UA_CLOCK_INTERNAL   0x0C

/* Sample rate table indices (for UA_REG_AX_CONNECT clock doorbell) */
#define UA_RATE_44100           1
#define UA_RATE_48000           2
#define UA_RATE_88200           3
#define UA_RATE_96000           4
#define UA_RATE_176400          5
#define UA_RATE_192000          6

/* Notification Status Bits (read from UA_REG_NOTIF_STATUS = 0xC030) */
#define UA_NOTIF_BIT_OUT_IO     BIT(0)   /* Output IO descriptors ready */
#define UA_NOTIF_BIT_IN_IO      BIT(1)   /* Input IO descriptors ready */
#define UA_NOTIF_BIT_TRANSPORT  BIT(4)   /* Transport/clock change */
#define UA_NOTIF_BIT_EVENT      BIT(5)   /* Clock-change event: the only bit that
					  * signals the kext's ClockChangeEvent
					  * (_handleNotificationInterrupt) */
#define UA_NOTIF_BIT_CONNECT    BIT(21)  /* Connect response (ACEFACE ack) */
#define UA_NOTIF_BIT_SRATE      BIT(22)  /* Sample rate change */

/* Connection handshake timeouts */
#define UA_CONNECT_RETRIES      20
#define UA_CONNECT_POLLS        10
#define UA_CONNECT_POLL_MS      100
#define UA_PREPARE_TIMEOUT_US   2000

/* Clock change timeout */
#define UA_CLOCK_TIMEOUT_MS     2000

/* DSP ring buffer state (circular, see UA_RING_NUM_PAGES) */
struct ua_dsp_ring {
	void *pages[UA_RING_MAX_PAGES];
	dma_addr_t page_addrs[UA_RING_MAX_PAGES];
	unsigned int num_pages;
	unsigned int write_ptr;     /* producer index, 0..capacity-1 */
	unsigned int hw_pos;        /* last valid POSITION read (consumer) */
	unsigned int capacity;      /* num_pages × 256, a power of two */
	u32 reg_base;               /* BAR0 offset of this ring's registers */
	u32 gen;                    /* bumped when a resync abandons entries */
	bool live;                  /* indices valid since the last (re)program */
};

struct ua_dsp_state {
	struct ua_dsp_ring cmd;
	struct ua_dsp_ring resp;
	bool rings_allocated;
};

/* Per-channel preamp state cache (write-through) */
struct ua_preamp_state {
	int gain;       /* dB value (-144..65) */
	bool phantom;   /* 48V phantom power */
	bool pad;       /* Pad engaged */
	bool hiz;       /* Hi-Z mode (hardware auto-detect only) */
	bool phase;     /* Phase invert */
	bool lowcut;    /* Low-cut filter */
	bool mic_line;  /* false=Mic, true=Line */
};

/* Monitor section state cache (write-through) */
struct ua_monitor_state {
	int level;       /* 0..192 (raw units: 192 + dB × 2) */
	int mute;        /* UA_MON_MUTE_ON / UA_MON_MUTE_OFF */
	bool dim;        /* Dim engaged */
	bool mono;       /* Monitor mono (setting[2] bit 17) */
	int source;      /* 0=MIX, 1=CUE1, 2=CUE2 */
	int hp1_source;  /* 0=CUE1, 1=CUE2 */
	int hp2_source;  /* 0=CUE1, 1=CUE2 */
	int dim_level;   /* 1-7 stepped */
	bool talkback;   /* Talkback engaged */
	int digital_mode; /* 0=S/PDIF, 8=ADAT */
	int output_ref;  /* 0=+4dBu, 1=-10dBV */
	int hp1_vol;     /* 0..192, same raw scale as level (setting[2] 15:8) */
	int hp2_vol;     /* 0..192, readback only (word 7 7:0) */
	int cue1_mirror;     /* Output pair value (SPDIF=6, LINE1-2=8, etc.) or 0xFFFFFFFF=NONE */
	int cue2_mirror;     /* Same encoding */
	bool cue1_mirror_en; /* CUE1 mirror enable */
	bool cue2_mirror_en; /* CUE2 mirror enable */
	bool digital_mirror; /* MirrorsToDigital */
	bool identify;       /* Identify/locate flash */
	int sr_convert;      /* S/PDIF SR converter on/off */
	int dsp_spanning;    /* DSP pairing/spanning mode */
	bool cue1_mono;      /* CUE1 mono mix */
	bool cue2_mono;      /* CUE2 mono mix */
	int cue1_mix;        /* CUE1 mix: 0=on, 2=off (inverted) */
	int cue2_mix;        /* CUE2 mix: 0=on, 2=off */
};

/*
 * Per-model route data (generated: driver/ua_models_gen.h from
 * devices/<model>.json by tools/gen-driver-models.py).
 *
 * Route tables come from the UA Mixer Engine's per-model IORouteInfo
 * tables: the channel IDs, in route order, that macOS exposes to
 * CoreAudio.  The Mixer Engine picks the Hi table at >= 176.4 kHz and the
 * LoMid table below (MixerTypeBase::GetCurrentRouteTableIndex).  The
 * per-class IsChannelShownForIORoute() rules that only depend on the
 * TOSLink mode and the channel number are applied by the generator; the
 * one rate rule (ADAT 5-8 hidden at 88.2/96 kHz on the x4 and Twin
 * classes) is applied at runtime from adat_mid_rate_max.
 */
#define UA_RATE_BAND_LOMID      0       /* < 176.4 kHz */
#define UA_RATE_BAND_HI         1       /* >= 176.4 kHz */
#define UA_RATE_BANDS           2

#define UA_DIG_SPDIF            0       /* TOSLink / digital mode S/PDIF */
#define UA_DIG_ADAT             1       /* TOSLink / digital mode ADAT */
#define UA_DIG_MODES            2

/*
 * Which TOSLink mode picks a direction's route list, after the class's
 * IsChannelShownForIORoute() (UA Mixer Engine 11.9.0): Freud (x4), Rene8
 * (x8p) and Peyton (8p) test the input type (property 0xcc) for record
 * routes and the output type (0x108) for playback; Armstrong/Stretch
 * (Twin family) test the input type for both; every other class shows
 * all entries.
 */
#define UA_TOSLINK_NONE         0
#define UA_TOSLINK_PER_DIR      1
#define UA_TOSLINK_INPUT        2

#define UA_DIR_REC              0       /* device -> host (kext dir 1) */
#define UA_DIR_PLAY             1       /* host -> device (kext dir 0) */
#define UA_DIRS                 2

#define UA_MODEL_F_TALKBACK     BIT(0)
#define UA_MODEL_F_AES          BIT(1)  /* digital I/O is AES/EBU */
#define UA_MODEL_F_DESKTOP      BIT(2)

struct ua_route_list {
	const u16 *ids;         /* UA channel IDs, ALSA channel order */
	unsigned int count;
};

struct ua_model_desc {
	const char *name;
	unsigned int preamps;           /* preamp channels with controls */
	unsigned int hiz;               /* HiZ-capable inputs */
	unsigned int flags;             /* UA_MODEL_F_* */
	unsigned int legacy_mode;       /* UA_DIG_* labelling the legacy layout */
	unsigned int toslink;           /* UA_TOSLINK_* route-list rule */
	unsigned int default_in_mode;   /* UA_DIG_*: default TOSLink input */
	unsigned int adat_mid_rate_max; /* 0, or last ADAT shown at 88.2/96k */
	struct ua_route_list routes[UA_RATE_BANDS][UA_DIG_MODES][UA_DIRS];
};

struct ua_model_type {
	u32 device_type;
	const char *name;               /* NULL: model->name */
	const struct ua_model_desc *model;
};

/* One parsed firmware IO descriptor (see UA_IO_DESC_*) */
struct ua_io_desc {
	bool valid;                     /* header and count passed checks */
	u32 hdr[UA_IO_DESC_HDR_WORDS];  /* raw header words */
	unsigned int count;             /* slots in the DMA frame */
	u16 ids[UA_MAX_CHANNELS];       /* channel ID per slot */
};

/*
 * ALSA channel -> DMA frame slot map for one direction.  A route channel the
 * firmware does not carry at the current rate (x4 at 176.4/192 kHz drops
 * VIRTUAL 1-8, AUX2 and CUE3/4) keeps its ALSA channel with slot
 * UA_CHAN_NO_SLOT: capture reads silence, playback is discarded.  That keeps
 * the channel count fixed across rates, as CoreAudio does (22/24 on an x4 at
 * every rate), so a stream can always be reopened at a new rate.
 */
#define UA_CHAN_NO_SLOT 0xFF

struct ua_chan_map {
	unsigned int channels;          /* ALSA-visible channels */
	u8 slot[UA_MAX_CHANNELS];       /* frame slot of ALSA channel i */
	u16 ch_id[UA_MAX_CHANNELS];     /* ID of ALSA channel i, 0 = none */
};

/* Page-granular backing store for one audio DMA ring (ua_audio.c) */
struct ua_dma_ring {
	struct page **pages;        /* UA_DMA_SG_ENTRIES entries, kvcalloc()ed */
	dma_addr_t *dmas;           /* device address of pages[i] */
	unsigned int nr_pages;      /* pages successfully allocated */
};

struct ua_audio {
	/* ALSA */
	struct snd_card *card;
	struct snd_pcm *pcm;

	/*
	 * Audio DMA rings — UA_DMA_BUF_SIZE (4 MiB) per direction, each
	 * UA_DMA_SG_ENTRIES separate pages (see "Audio DMA rings" in
	 * ua_audio.c) vmap()ed into one flat CPU range so the PCM copy
	 * path and the dma_test ioctl keep treating a ring as a single
	 * buffer.  Page bookkeeping lives in separately allocated arrays
	 * so this struct stays a small slab object.
	 */
	struct ua_dma_ring play_ring;
	struct ua_dma_ring rec_ring;
	void *play_buf;             /* vmap()ed flat view of play_ring */
	void *rec_buf;              /* vmap()ed flat view of rec_ring */
	struct rw_semaphore dma_sem; /* read: PCM copy/silence, dma_test;
				      * write: ua_audio_fini() teardown */

	/* Audio state */
	bool connected;             /* DSP firmware connection established */
	bool transport_running;     /* Transport started (DMA active) */
	bool rec_debug_done;        /* One-shot record DMA dump done */
	bool dma_reset_done;        /* DMA engines reset for warm boot */
	struct completion connect_done;

	/* Configuration */
	unsigned int play_channels;
	unsigned int rec_channels;
	unsigned int sample_rate;
	unsigned int rate_index;    /* Sample rate table index */
	unsigned int clock_source;  /* UA_CLOCK_INTERNAL, etc. */
	unsigned int buf_frame_size;

	/* Preamp / monitor model info */
	unsigned int num_preamps;   /* Number of preamp channels */
	unsigned int num_hiz;       /* Number of HiZ-capable channels */

	/*
	 * Model data and IO map (ua_audio.c "IO descriptor and channel map").
	 *
	 * The layout (chan_map, play/rec_channels, buf_frame_size,
	 * io_map_active) is set once at probe and then changes only in
	 * ua_io_map_commit(), with ua->lock, io_lock and dma_sem (write) held
	 * and the transport stopped; the PCM copy path reads it under dma_sem
	 * (read).  io_lock alone guards the descriptor copies and the readers
	 * (proc, ioctl, pcm_open's count).
	 * Lock order: ua->lock -> io_lock -> dma_sem, and
	 * ua->lock -> io_lock -> audio->lock -> PCM stream lock (forced
	 * re-prepare, see ua_io_map_force_reprepare()).
	 */
	const struct ua_model_desc *model;     /* NULL: no generated data */
	bool io_ready;              /* io_lock initialised (ua_audio_init) */
	struct mutex io_lock;
	struct ua_io_desc io_desc[UA_DIRS];    /* last valid firmware descriptor */
	bool io_desc_bad[UA_DIRS];  /* last read not usable, io_desc kept */
	u32 io_desc_bad_hdr[UA_DIRS][UA_IO_DESC_HDR_WORDS]; /* its header */
	struct ua_chan_map chan_map[UA_DIRS];  /* ALSA channel -> slot */
	bool io_map_active;         /* io_map=1 layout applied to transport */
	bool io_layout_stale;       /* layout changed while running */
	unsigned int io_map_rate;   /* sample rate the map was built for */
	unsigned int io_map_mode[UA_DIRS];  /* UA_DIG_* each list was built for */
	unsigned int dig_in_mode;   /* UA_DIG_*: TOSLink input type (host side) */

	/* ALSA mixer control cache (write-through) */
	struct ua_preamp_state preamp[UA_MAX_PREAMP_CH];
	struct ua_monitor_state monitor;

	/*
	 * Every kcontrol ua_mixer_init() registered, so a notification can
	 * carry the numid (alsa-lib matches events by numid) and the
	 * readback sync can find controls by name.  Set at probe, then
	 * read-only.
	 */
	struct snd_kcontrol *kctls[UA_MAX_KCTLS];
	unsigned int num_kctls;
	/* Last readback words ua_mixer_sync_readback() acted on */
	u32 rb_synced[UA_MIXER_RB_WORDS];
	bool rb_synced_valid;

	/* Position tracking (updated from IRQ) */
	snd_pcm_uframes_t play_pos;
	snd_pcm_uframes_t rec_pos;

	/* Substream pointers (set when open, NULL when closed) */
	struct snd_pcm_substream *play_sub;
	struct snd_pcm_substream *rec_sub;
	spinlock_t lock;

	/* DSP service — periodic readback drain (host liveness signal) */
	struct delayed_work dsp_service_work;
	bool dsp_service_running;
	unsigned int dsp_service_count;

	/* Period elapsed polling timer (replaces unreliable HW timer IRQ) */
	struct hrtimer period_timer;
	bool period_timer_running;
	/*
	 * Substreams between trigger START and STOP (bit = UA_DIR_*): the
	 * shared timer stops only when neither runs.  timer_lock serialises
	 * the triggers' timer start/stop; taken under the PCM stream lock,
	 * nothing is taken inside it.
	 */
	unsigned long running_subs;
	spinlock_t timer_lock;
	bool dma_test_active;       /* UA_IOCTL_DMA_TEST owns the transport */
	snd_pcm_uframes_t last_play_period_pos;
	snd_pcm_uframes_t last_rec_period_pos;

	/* Prepare-churn guard (issue #42).  A session manager opening the
	 * raw PCM without the protective WirePlumber rule churns
	 * open/prepare cycles against a transport whose SAMPLE_POS never
	 * advances; the register traffic eventually wedges the PCIe link
	 * (hard freeze, no oops).  Track SAMPLE_POS across prepares and
	 * back off when it is frozen. */
	unsigned long churn_last_jiffies;   /* jiffies of last pcm_prepare */
	u32 churn_last_sample_pos;          /* SAMPLE_POS at last pcm_prepare */
	unsigned int churn_count;           /* consecutive stalled prepares */
	unsigned long churn_block_until;    /* refuse prepares until (jiffies) */

	/* Playback and capture share one clock: the rate each direction's
	 * hw_params set (0 = none), so the other can't pick another one */
	unsigned int stream_rate[2];        /* [UA_DIR_PLAY], [UA_DIR_REC] */
	u16 clock_src_mask;                 /* UA_REG_NOTIF_CLOCK_SRCS >> 16 */
};

/* One allocated DMA buffer carrying a plugin-chain DMA_REF payload.
 * Lifetime tied to the ua_device — allocated at probe, freed at remove. */
struct ua_plugin_dma_buf {
	unsigned int ring_idx;      /* Index into ua_plugin_chain_data[] */
	size_t size;                /* Buffer size in bytes */
	dma_addr_t dma;             /* DMA address handed to the FPGA */
	void *cpu;                  /* Kernel-visible mapping (coherent) */
};

/*
 * DSP-ring extras (ua_dsp.c): the SEL130 coefficient cache and bus owner
 * table (parity/14 B3/B4), and the DSP message-interrupt state (parity/03
 * A07, I01-I05).  One member at the end of struct ua_device so other
 * branches' edits to that struct do not collide.
 */
#define UA_BUS_COEF_BUSES	32
#define UA_BUS_COEF_PARAMS	7
#define UA_BUS_COEF_SIDES	2
#define UA_BUS_COEF_CELLS	(UA_BUS_COEF_BUSES * UA_BUS_COEF_PARAMS * \
				 UA_BUS_COEF_SIDES)

/* ua_dsp_set_bus_param_flags() flags */
#define UA_DSP_BUSP_SOLO_MUTE	BIT(0)	/* entry is a solo-muted weight */

struct ua_dsp_ext {
	/* SEL130 coefficient cache: [bus][param][side] -> float bits */
	u32 bus_coef[UA_BUS_COEF_BUSES][UA_BUS_COEF_PARAMS][UA_BUS_COEF_SIDES];
	DECLARE_BITMAP(bus_coef_valid, UA_BUS_COEF_CELLS);
	u8 bus_owner[UA_BUS_COEF_BUSES];	/* hosting DSP, 0 = DSP 0 */
	u32 bus_replays;			/* ua_dsp_replay_bus_params() runs */

	/* Message FIFO / DSP vectors (dsp_msg_irq) */
	bool inited;			/* msg_work initialised */
	bool msg_armed;			/* irq_lock: vectors enabled by us */
	bool storm_tripped;		/* vectors masked by the storm guard */
	u32 vec_mask;			/* irq_lock: IMR bits we enabled */
	struct work_struct msg_work;
	unsigned long pend_fifo;	/* bit n: DSP n message FIFO vector */
	unsigned long pend_err;		/* bit 2n: error A, 2n+1: error B */
	unsigned long pend_rearm;	/* bit 2n: cmd, 2n+1: resp (one-shot) */
	unsigned long storm_window;	/* jiffies at the window start */
	u32 storm_count;		/* DSP vectors in this window */
	u8 msg_rd[UA_MAX_DSPS];		/* software read index into the FIFO */
	atomic_t vec_count[UA_MAX_DSPS][5];	/* fires per vector 5n+j */
	u32 msg_total[UA_MAX_DSPS];	/* FIFO messages read */
	u32 msg_f001[UA_MAX_DSPS];	/* class 0xF001xxxx */
	u32 msg_f002[UA_MAX_DSPS];	/* class 0xF002xxxx */
	u32 msg_stale[UA_MAX_DSPS];	/* write index > 0xF seen */
	u32 msg_last[UA_MAX_DSPS][5];	/* last message, raw words */
};

struct ua_device {
	struct pci_dev *pdev;
	void __iomem *regs;         /* BAR0 MMIO base */
	resource_size_t regs_phys;  /* BAR0 physical address */
	resource_size_t regs_size;  /* BAR0 mapped size */

	/* Device identity */
	u32 fpga_rev;               /* Cached FPGA revision from 0x2218 */
	u32 device_type;            /* Serial-table type (naming, models) */
	u32 public_type;            /* Kext UAD2DeviceType (GetCapabilities);
				     * UA_PUBTYPE_NONE when unresolved */
	u32 ext_caps;               /* ext_caps (0x2234) at probe, v2 only */
	u32 subsystem_id;           /* PCI subsystem ID = model selector */
	u32 num_dsps;               /* Number of SHARC DSPs */
	bool fw_v2;                 /* Extended (v2) firmware */
	bool cli_enabled;           /* CLI register interface active */

	/* Interrupts */
	int num_irq_vectors;
	u32 dma_ctrl_cache;         /* Cached DMA engine control */
	u32 irq_mask_lo;            /* Cached IMR (low 32 vectors) */
	u32 irq_mask_hi;            /* Cached IMR_HI (high 32 vectors, v2) */

	/* Audio subsystem */
	struct ua_audio audio;

	/* DSP ring buffers and firmware state */
	struct ua_dsp_state dsp[UA_MAX_DSPS];
	bool fw_loaded;
	bool dsps_connected;
	bool aceface_done;          /* ACEFACE handshake completed */
	bool aceface_failed;        /* ACEFACE timed out — don't retry from pcm_prepare */
	bool pcie_setup_done;       /* PCIe ASPM/timeout configured */
	bool plugins_activated;     /* Plugin chain sent to DSP */
	bool skip_bus_coeff;        /* Skip BUS_COEFF in plugin chain */

	/* Plugin-chain DMA payload buffers (one per DMA_REF ring entry).
	 * Allocated by ua_dsp_load_plugin_payloads() when the firmware
	 * blob is present; freed by ua_dsp_free_plugin_payloads().  The
	 * array is indexed by ring_idx (sparse — only populated for
	 * entries whose cmd[0] has bit31 set). */
	struct ua_plugin_dma_buf *plugin_bufs;
	unsigned int plugin_bufs_count;

	/* Coherent buffers a timed-out ring command may still reference.
	 * Freeing them would let a late descriptor fetch or response write
	 * land in reused memory, so they are parked here until the ring has
	 * provably moved past them (ua_dsp_reclaim_orphans()) or remove
	 * tears them down (ua_dsp_rings_fini()).
	 * Bounded by UA_DMA_ORPHAN_MAX / UA_DMA_ORPHAN_MAX_BYTES: at the cap
	 * new DMA-carrying ring commands fail with -EBUSY.  ua->lock. */
	struct list_head dma_orphans;
	unsigned int dma_orphan_count;
	size_t dma_orphan_bytes;

	/* System resume: anything that may block for seconds (cold DSP
	 * re-init with firmware load, the full ACEFACE connect) runs from
	 * resume_work; until it has finished pcm_prepare fails -EBUSY. */
	struct work_struct resume_work;
	bool resume_pending;        /* resume_work queued or running */
	bool resume_need_init;      /* resume_work must (re)init the DSP */

	/* Mixer batch write state (matches kext CPcieDeviceMixer protocol) */
	u32 mixer_val[UA_MIXER_BATCH_COUNT];   /* Cached setting values */
	u32 mixer_mask[UA_MIXER_BATCH_COUNT];  /* Cached setting masks */
	u64 mixer_zero_pending;                /* mixer_mailbox: settings to zero next */
	u32 mixer_seq_wr;                      /* Our cached SEQ_WR */
	bool mixer_dirty;                      /* Any setting changed since last flush */
	bool mixer_ready;                      /* Batch protocol initialized */
	bool mixer_rb_seen;                    /* DSP produced at least one readback */
	u32 mixer_rb_data[UA_MIXER_RB_WORDS];  /* Cached readback (filled by service loop) */
	u32 mixer_rb_status;                   /* Cached readback status */
	u32 bus_disabled_mask;                 /* SetBusEnable cache: bit n = bus n off (0 = all on, as the kext starts) */
	u32 bus_enable_known;                  /* SetBusEnable cache: bit n = bus n sent since probe */

	/*
	 * Character device.  chardev owns this struct: its release frees it
	 * after remove() and the last close of /dev/ua_apolloN, so open
	 * files and the cdev never point at freed memory.
	 */
	struct cdev cdev;
	struct device chardev;
	int dev_id;                 /* -1 until allocated */

	/*
	 * ioctls run with ioctl_sem held for reading; remove() takes it for
	 * writing and sets removed, so it waits out an ioctl in flight and
	 * none starts during or after the teardown.
	 */
	struct rw_semaphore ioctl_sem;
	bool removed;

	/*
	 * Event channel (ua_event.c): the open files, each with its own
	 * queue.  event_lock is taken with interrupts off; nothing is
	 * taken inside it.
	 */
	spinlock_t event_lock;
	struct list_head event_files;	/* struct ua_file, event_lock */
	wait_queue_head_t event_wq;
	bool event_offline;		/* event_lock; DEVICE_OFFLINE posted */

	/* Synchronization */
	struct mutex lock;          /* Device access lock */
	spinlock_t irq_lock;        /* Interrupt handler lock */

	/* Surprise removal (Thunderbolt hot-unplug) */
	atomic_t shutdown;          /* Set on remove/disconnect — blocks BAR0 access */

	/*
	 * Unit group (ua_core.c): membership in the module-global list of
	 * probed units, under ua_units_lock.  Empty node = not in the group
	 * (not yet registered, or being removed).
	 */
	struct list_head unit_node;

	/* Pause reasons (UA_PAUSE_*): see ua_pause_set()/ua_pause_clear() */
	atomic_t pause_mask;
	u32 lock_level;             /* LockSampleRateAndClock level; 0 */

	/* DSP ring extras (ua_dsp.c); keep last */
	struct ua_dsp_ext dspx;
};

/* Pause reasons: posts UA_EVT_PAUSED / UA_EVT_RESUMED on a change (ua_core.c) */
void ua_pause_set(struct ua_device *ua, u32 reason);
void ua_pause_clear(struct ua_device *ua, u32 reason);

/* Per-open-file state: filp->private_data (ua_event.c) */
#define UA_EVENT_QUEUE_LEN	64
struct ua_file {
	struct ua_device *ua;		/* kept alive by the cdev while open */
	struct list_head node;		/* ua->event_files */
	DECLARE_KFIFO(events, struct ua_event, UA_EVENT_QUEUE_LEN);
	u32 lost;			/* dropped since the last LOST event */
	struct fasync_struct *fasync;
};

void ua_event_init(struct ua_device *ua);
void ua_event_post(struct ua_device *ua, u32 code, u32 arg);
void ua_event_post_notif(struct ua_device *ua, u32 notif);
void ua_event_offline(struct ua_device *ua, bool surprise);
int ua_event_open(struct inode *inode, struct file *filp);
int ua_event_release(struct inode *inode, struct file *filp);
ssize_t ua_event_read(struct file *filp, char __user *buf, size_t len,
		      loff_t *ppos);
__poll_t ua_event_poll(struct file *filp, struct poll_table_struct *wait);
int ua_event_fasync(int fd, struct file *filp, int on);

/* Register access helpers — guarded against surprise removal */
static inline u32 ua_read(struct ua_device *ua, u32 offset)
{
	if (unlikely(atomic_read(&ua->shutdown)))
		return 0xFFFFFFFF;
	return ioread32(ua->regs + offset);
}

static inline void ua_write(struct ua_device *ua, u32 offset, u32 value)
{
	if (unlikely(atomic_read(&ua->shutdown)))
		return;
	iowrite32(value, ua->regs + offset);
}

/*
 * Mixer setting register offset (3-range formula).
 *
 * From CPcieDeviceMixer::_writeSetting disassembly, settings live in
 * 3 non-contiguous ranges within the 0x3800 mixer window:
 *   Settings  0-15: base = 0x3800 + 0xB4 + index * 8
 *   Settings 16-31: base = 0x3800 + 0xBC + index * 8  (+8 gap)
 *   Settings 32-37: base = 0x3800 + 0xC0 + index * 8  (+4 more gap)
 *
 * Each setting is a pair of 32-bit words (wordA at offset, wordB at +4).
 */
static inline u32 ua_mixer_setting_reg(unsigned int index)
{
	if (index <= 15)
		return UA_REG_MIXER_BASE + 0xB4 + index * UA_MIXER_SETTING_STRIDE;
	else if (index <= 31)
		return UA_REG_MIXER_BASE + 0xBC + index * UA_MIXER_SETTING_STRIDE;
	else
		return UA_REG_MIXER_BASE + 0xC0 + index * UA_MIXER_SETTING_STRIDE;
}

/* DSP bank base address for a given DSP index */
static inline u32 ua_dsp_base(unsigned int dsp_idx)
{
	if (dsp_idx < 4)
		return UA_DSP_BANK_LOW + dsp_idx * UA_DSP_BANK_STRIDE;
	else
		return UA_DSP_BANK_HIGH + dsp_idx * UA_DSP_BANK_STRIDE;
}

/*
 * Interrupt vector mapping — logical vector index → hardware bit position.
 *
 * The kext's CPcieIntrManager maintains m_vectorMap[72] mapping logical
 * vector indices to hardware bit positions (0-63).  In extended mode (v2):
 *
 *   DSP n (n=0..7) uses 5 logical slots at n*5, mapped to HW bits n*4..n*4+3
 *   (with every 5th slot unmapped at -1).
 *
 *   Audio extension vectors:
 *     Logical 0x28 (40) → HW bit 32  (notification)
 *     Logical 0x46 (70) → HW bit 62  (periodic timer)
 *     Logical 0x47 (71) → HW bit 63  (end-of-buffer / DMA completion)
 *
 *   HW bits 0-31 → ISR/IMR at 0x2208/0x2204
 *   HW bits 32-63 → ISR_HI/IMR_HI at 0x2264/0x2268
 */
static inline int ua_vector_to_hwbit(unsigned int vector)
{
	switch (vector) {
	case UA_IRQ_VEC_NOTIFICATION: return 32;
	case UA_IRQ_VEC_PERIODIC:    return 62;
	case UA_IRQ_VEC_END_BUFFER:  return 63;
	default:
		/* DSP vectors: identity mapping for vectors 0-31 */
		if (vector < 32)
			return vector;
		return -1;
	}
}

/*
 * Mirrors CPcieIntrManager::EnableVector(vector, clearPending) (kext 12.0.0
 * x86 0x9ecc): under the interrupt lock, set the cached enable bit, W1C the
 * vector's pending ISR bit when @clear_pending, write the IMR, then read
 * FPGA_REV to flush the posted writes.  irq_lock is the lock the ISR holds
 * while it reads ISR & mask, so the cached masks and the IMR cannot be
 * torn by a concurrent hard-IRQ re-arm.
 *
 * Pass clear_pending=true when (re)arming a vector from a stopped state, so
 * an event left over from a previous session does not fire immediately;
 * false when re-arming from the ISR, where clearing could drop a new event.
 * The flush read is done only with clear_pending: the ISR re-arm runs on
 * every period, and a BAR0 read there would put an MMIO round trip (a
 * completion-timeout stall on a dead link) back into hard-IRQ context.
 */
static inline void ua_enable_vector(struct ua_device *ua, unsigned int vector,
				    bool clear_pending)
{
	int hwbit = ua_vector_to_hwbit(vector);
	unsigned long flags;

	if (hwbit < 0 || (hwbit >= 32 && !ua->fw_v2))
		return;

	spin_lock_irqsave(&ua->irq_lock, flags);
	if (hwbit < 32) {
		ua->irq_mask_lo |= BIT(hwbit);
		if (clear_pending)
			ua_write(ua, UA_REG_IRQ_STATUS, BIT(hwbit));
		ua_write(ua, UA_REG_IRQ_ENABLE, ua->irq_mask_lo);
	} else {
		ua->irq_mask_hi |= BIT(hwbit - 32);
		if (clear_pending)
			ua_write(ua, UA_REG_EXT_IRQ_STATUS, BIT(hwbit - 32));
		ua_write(ua, UA_REG_EXT_IRQ_ENABLE, ua->irq_mask_hi);
	}
	if (clear_pending)
		(void)ua_read(ua, UA_REG_FPGA_REV);	/* posted-write flush */
	spin_unlock_irqrestore(&ua->irq_lock, flags);
}

/*
 * Mask every vector (IMR = IMR_HI = 0) and, with @clear_pending, W1C
 * everything pending (ISR = ISR_HI = ~0), keeping the cached masks the ISR
 * filters with equal to the hardware.  CPcieIntrManager::DisableInterrupts
 * (x86 0xa154) / ProgramRegisters (0x996a), plus CPcieDevice::
 * ProgramRegisters' ISR = ~0.  The one place "mask all" is implemented, so
 * the cache == IMR invariant ua_enable_vector() relies on holds everywhere.
 * With the shutdown flag set the register writes are no-ops and only the
 * caches are cleared.
 */
static inline void ua_irq_mask_all(struct ua_device *ua, bool clear_pending)
{
	unsigned long flags;

	spin_lock_irqsave(&ua->irq_lock, flags);
	ua->irq_mask_lo = 0;
	ua->irq_mask_hi = 0;
	ua_write(ua, UA_REG_IRQ_ENABLE, 0);
	if (ua->fw_v2)
		ua_write(ua, UA_REG_EXT_IRQ_ENABLE, 0);
	if (clear_pending) {
		ua_write(ua, UA_REG_IRQ_STATUS, UA_IRQ_CLEAR_ALL);
		if (ua->fw_v2)
			ua_write(ua, UA_REG_EXT_IRQ_STATUS, UA_IRQ_CLEAR_ALL);
	}
	spin_unlock_irqrestore(&ua->irq_lock, flags);
}

/* CPcieIntrManager::DisableVector (x86 0xa088): same lock, no flush */
static inline void ua_disable_vector(struct ua_device *ua, unsigned int vector)
{
	int hwbit = ua_vector_to_hwbit(vector);
	unsigned long flags;

	if (hwbit < 0 || (hwbit >= 32 && !ua->fw_v2))
		return;

	spin_lock_irqsave(&ua->irq_lock, flags);
	if (hwbit < 32) {
		ua->irq_mask_lo &= ~BIT(hwbit);
		ua_write(ua, UA_REG_IRQ_ENABLE, ua->irq_mask_lo);
	} else {
		ua->irq_mask_hi &= ~BIT(hwbit - 32);
		ua_write(ua, UA_REG_EXT_IRQ_ENABLE, ua->irq_mask_hi);
	}
	spin_unlock_irqrestore(&ua->irq_lock, flags);
}

/* Audio subsystem interface (ua_audio.c) */
int ua_audio_preinit_dma(struct ua_device *ua);
int ua_audio_reprogram_dma(struct ua_device *ua);
int ua_audio_init(struct ua_device *ua);
void ua_audio_fini(struct ua_device *ua);
void ua_audio_free_dma(struct ua_device *ua);
void ua_audio_suspend(struct ua_device *ua);

/* ua_audio_resume() modes */
enum ua_resume_how {
	UA_RESUME_CONTROLS,	/* reopen the ALSA controls only */
	UA_RESUME_CONNECT_FAST,	/* connect if it cannot block long, else -EAGAIN */
	UA_RESUME_CONNECT,	/* the full connect, as at probe */
};
int ua_audio_resume(struct ua_device *ua, enum ua_resume_how how);
void ua_audio_irq(struct ua_device *ua, u32 status_lo, u32 status_hi);
int ua_audio_set_clock(struct ua_device *ua, unsigned int rate);
int ua_audio_set_clock_source(struct ua_device *ua, unsigned int source);
bool ua_clock_source_valid(const struct ua_device *ua, unsigned int source);
int ua_audio_reconnect(struct ua_device *ua);
void ua_notify_clock_controls(struct ua_device *ua);
void ua_ctl_notify_name(struct ua_device *ua, const char *name);
void ua_mixer_sync_readback(struct ua_device *ua);	/* ua->lock */
void ua_dsp_service_start(struct ua_device *ua);
void ua_dsp_service_stop(struct ua_device *ua);
const char *ua_device_name(u32 device_type);

/* Model data and IO map (ua_audio.c) */
const struct ua_model_desc *ua_model_lookup(u32 device_type,
					    const char **type_name);
bool ua_io_map_enabled(const struct ua_device *ua);
void ua_io_map_digital_mode_changed(struct ua_device *ua);	/* ua->lock */
int ua_chan_name(u16 ch_id, char *buf, size_t len);
struct ua_io_map_info;
void ua_io_map_get_info(struct ua_device *ua, struct ua_io_map_info *info);

/* Mixer batch protocol (ua_core.c) */
int ua_mixer_write_setting_locked(struct ua_device *ua,
				  unsigned int index, u32 value, u32 mask);
void ua_mixer_flush_settings(struct ua_device *ua);
void ua_set_output_mute_locked(struct ua_device *ua, bool mute,
			       bool flush_now);

/* Preamp/monitor hardware access wrappers (ua_core.c) */
int ua_preamp_set_param(struct ua_device *ua, unsigned int ch,
			u32 param_id, u32 value);
int ua_preamp_write_locked(struct ua_device *ua, unsigned int ch,
			   u32 param_id, u32 value);	/* ua->lock */
int ua_monitor_set_param(struct ua_device *ua, unsigned int ch_idx,
			 u32 param_id, u32 value);

/* DSP ring buffer and firmware (ua_dsp.c) */
int ua_dsp_rings_init(struct ua_device *ua);	/* also re-arms on resume */
void ua_dsp_rings_fini(struct ua_device *ua, bool dma_quiet);
void ua_dsp_rings_quiesce(struct ua_device *ua);
void ua_dsp_rings_neutralize(struct ua_device *ua);
void ua_dsp_cache_firmware(struct ua_device *ua);
void ua_dsp_rings_reprogram(struct ua_device *ua);
int ua_dsp_load_firmware(struct ua_device *ua, const void *fw_data,
			 size_t fw_size);
int ua_dsp_load_mixer_blocks(struct ua_device *ua);
int ua_dsp_connect_all(struct ua_device *ua);
int ua_dsp_connect_retry(struct ua_device *ua);
int ua_dsp_wait_ready(struct ua_device *ua);
int ua_dsp_load_plugin_payloads(struct ua_device *ua);
void ua_dsp_free_plugin_payloads(struct ua_device *ua);
int ua_dsp_activate_plugin_chain(struct ua_device *ua);
void ua_dump_mixer_sram(struct ua_device *ua, const char *label);
int ua_dsp_test_dma_ref(struct ua_device *ua);
int ua_dsp_send_block(struct ua_device *ua, unsigned int dsp_idx,
		      u32 cmd, u32 param, const void *data,
		      size_t data_size);
int ua_dsp_send_sram_data(struct ua_device *ua, unsigned int dsp_idx,
			  u32 sram_addr, const void *data, size_t data_size);
int ua_dsp_send_raw_dma(struct ua_device *ua, unsigned int dsp_idx,
			const void *data, size_t data_size);
int ua_dsp_test_send_block(struct ua_device *ua);
int ua_dsp_send_routing(struct ua_device *ua);
int ua_dsp_set_bus_param(struct ua_device *ua, u32 bus_id,
			 u32 param, u32 side, u32 value_u32);
int ua_dsp_set_bus_param_flags(struct ua_device *ua, u32 bus_id, u32 param,
			       u32 side, u32 value_u32, u32 flags);
int ua_dsp_replay_bus_params(struct ua_device *ua);	/* ua->lock */
int ua_dsp_set_bus_owner(struct ua_device *ua, u32 bus_id, u32 dsp_idx);
int ua_dsp_set_bus_enable(struct ua_device *ua, u32 bus_idx, int enable);
u32 ua_dsp_get_bus_enable_mask(struct ua_device *ua);
int ua_dsp_set_input_param(struct ua_device *ua, u32 input, u32 sub,
			   u32 value);
int ua_dsp_send_rate_ack(struct ua_device *ua, u32 rate);
void ua_dsp_ring_reset(struct ua_device *ua, unsigned int dsp_idx);
int ua_dsp_ring_send_raw(struct ua_device *ua, unsigned int dsp_idx,
			  u32 w0, u32 w1, u32 w2, u32 w3);
/* DSP message FIFO / vectors (dsp_msg_irq), kext_connect notifications */
void ua_dsp_irq(struct ua_device *ua, u32 status_lo, u32 status_hi);
void ua_dsp_irq_arm(struct ua_device *ua);		/* ua->lock */
int ua_dsp_start_transport_notify(struct ua_device *ua);	/* ua->lock */
int ua_dsp_disconnect_notify(struct ua_device *ua);	/* ua->lock */
void ua_dsp_flush_bus_params(struct ua_device *ua);
int ua_dsp_configure_modules(struct ua_device *ua);
struct ua_dma_test;
int ua_audio_dma_test(struct ua_device *ua, struct ua_dma_test *dt);

/* Early ACEFACE handshake (called from core probe before FW load) */
int ua_aceface_handshake(struct ua_device *ua);

/* Cold boot transport start/stop (full PrepareTransport + ACEFACE + DMA pulse) */
int ua_boot_transport_start(struct ua_device *ua);
void ua_boot_transport_stop(struct ua_device *ua);
void ua_reset_dma(struct ua_device *ua);

/* Per-engine DMA_CTRL ownership (CPcieIntrManager::EnableDMA/ResetDMAEngine) */
void ua_dma_enable_engine(struct ua_device *ua, unsigned int idx);
void ua_dma_reset_engine(struct ua_device *ua, unsigned int idx);

/* kext_connect: ring connect after ACEFACE (ua_core.c) */
extern bool ua_kext_connect;
void ua_kext_ring_connect(struct ua_device *ua);

#endif /* UA_APOLLO_H */
