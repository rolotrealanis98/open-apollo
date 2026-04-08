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
#include <linux/cdev.h>
#include <linux/completion.h>
#include <linux/workqueue.h>
#include <linux/dma-mapping.h>
#include <linux/io.h>
#include <linux/kernel.h>
#include <linux/hrtimer_types.h>
#include <linux/hrtimer.h>
#include <sound/core.h>
#include <sound/pcm.h>

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

/*
 * UAD2DeviceType enum — reconstructed from CPcieDevice::Name() and
 * _deviceTypeFromSerialNumber() lookup table at 0x3E840.
 *
 * v1 devices: type derived from FPGA rev register (0x2218) bits[31:28] - 1
 * v2 devices: type derived from ext_caps register (0x2234) bits[25:20] - 1,
 *             then refined via serial number prefix lookup.
 *
 * DSP variant (SOLO/DUO/QUAD/HEXA/OCTO) comes from ext_caps bits[15:8].
 */

/* Original (pre-x) Thunderbolt device types (from ext_caps register) */
#define UA_DEV_APOLLO_8P            0x0A  /* Subsys 0x0006, serial 1526xxxx */

/* v2 Thunderbolt device types (from serial prefix table) */
#define UA_DEV_APOLLO_X6            0x1E  /* Serial 2016 */
#define UA_DEV_APOLLO_X4            0x1F  /* Serial 2005 */
#define UA_DEV_APOLLO_X8P           0x20  /* Serial 2019 */
#define UA_DEV_APOLLO_X16           0x21  /* Serial 2018 */
#define UA_DEV_APOLLO_X8            0x22  /* Serial 2017 */
#define UA_DEV_APOLLO_TWIN_X        0x23  /* Serial 2020 */
#define UA_DEV_APOLLO_SOLO          0x27  /* Serial 2024 */
#define UA_DEV_ARROW                0x28  /* Serial 2025 */
#define UA_DEV_APOLLO_X16D          0x2A  /* Serial 2032 */
#define UA_DEV_APOLLO_X6_GEN2       0x35  /* Serial 2073/2082 */
#define UA_DEV_APOLLO_X4_GEN2       0x36  /* Serial 2092 */
#define UA_DEV_APOLLO_X8_GEN2       0x37  /* Serial 2086 */
#define UA_DEV_APOLLO_X8P_GEN2      0x38  /* Serial 2087 */
#define UA_DEV_APOLLO_X16_GEN2      0x39  /* Serial 2088 */
#define UA_DEV_APOLLO_TWIN_X_GEN2   0x3A  /* Serial 2089 */

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
 * Serial prefix register: hardware registers at BAR0 + 0x20..0x2C
 * contain a 16-byte serial string. The first 4 ASCII digits identify
 * the device model and are used by _deviceTypeFromSerialNumber().
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

/* Audio Clock */
#define UA_REG_CLOCK_SRC0       0x2280  /* Clock source register 0 */
#define UA_REG_CLOCK_SRC1       0x2284  /* Clock source register 1 */

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
#define UA_REG_NOTIF_READBACK   (UA_REG_NOTIF_BASE + UA_NOTIF_BANK_OFFSET + 0x00)  /* 0xC028 */

/* Config write area (NO bank shift — always at 0xC000) */
#define UA_REG_NOTIF_CONFIG     UA_REG_NOTIF_BASE  /* 0xC000 */
#define UA_NOTIF_CONFIG_WORDS   10      /* 10 dwords written after connect */
#define UA_NOTIF_READBACK_WORDS 95      /* 95 dwords read back after connect */
#define UA_NOTIF_READBACK_COUNT 0x17C   /* config[1] = readback buffer word count */

/* Legacy defines — kept for reference but DO NOT USE for notification I/O */
#define UA_REG_CLOCK_CFG_BASE   0xC04C  /* WRONG: missing bank shift. Use UA_REG_NOTIF_CLOCK_CFG */

/* IO descriptor areas (NO bank shift — fixed offsets) */
#define UA_REG_IN_NAMES_BASE    0xC1A4  /* Input channel name data (72 x u32) */
#define UA_REG_OUT_NAMES_BASE   0xC2C4  /* Output channel name data (72 x u32) */
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
#define UA_RING_NUM_PAGES       1       /* Default: 1 page = 256 entries */
#define UA_FW_RING_NUM_PAGES    4       /* DSP 0 cmd ring: 4 pages = 1024 entries */
#define UA_RING_DMA_REF         BIT(31) /* BIT31=1 in word0 → DMA reference entry */

/* Firmware loading */
#define UA_FW_CMD               0x120000
#define UA_FW_PARAM             0x80040000
#define UA_FW_LARGE_THRESHOLD   0x3FFFC     /* ≥256KB-4: use two-word command */
#define UA_FW_LARGE_FLAG        0x40000000  /* BIT30 set for large transfers */
#define UA_FW_RESP_MATCH        0x8004      /* Upper 16 bits of response word 0 */
#define UA_FW_TIMEOUT_MS        30000       /* 30 seconds (generous) */
#define UA_FW_POLL_MS           10          /* Poll every 10ms */
#define UA_FW_MAX_SIZE          (4 * 1024 * 1024)

/* DSP connect */
#define UA_DSP_CMD_CONNECT      0x00230002
#define UA_DSP_CMD_FINALIZE     0x00100002

/* Mixer firmware blob format (ua-apollo-mixer.bin) */
#define UA_MIXER_FW_MAGIC       0x5541464D      /* 'UAFM' */
#define UA_MIXER_FW_VERSION     1
#define UA_MIXER_FW_NAME        "ua-apollo-mixer.bin"

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
#define UA_PREAMP_PARAM_GAIN_A  0x06    /* Gain param A (1st write, dB-9) */
#define UA_PREAMP_PARAM_GAIN_B  0x09    /* Gain param B (2nd write, dB-10) */
#define UA_PREAMP_PARAM_GAIN_C  0x0A    /* Gain param C (3rd write, dB-10) */
#define UA_PREAMP_PARAM_ROUTE   0x13    /* Channel routing mask (0x0001ffff) */
#define UA_PREAMP_PARAM_LEVEL   0x16    /* Default level (0xa0 = -16dB) */

/* Legacy aliases — do NOT use for new code */
#define UA_ARM_PARAM_PAD        UA_PREAMP_PARAM_PAD
#define UA_ARM_PARAM_48V        UA_PREAMP_PARAM_48V
#define UA_ARM_PARAM_LOWCUT     UA_PREAMP_PARAM_LOWCUT
#define UA_ARM_PARAM_PHASE      UA_PREAMP_PARAM_PHASE
#define UA_ARM_PARAM_GAIN       UA_PREAMP_PARAM_GAIN_C

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
#define UA_MON_PARAM_CUE1_MONO  0x06  /* ch_idx=0, 1=on/0=off */
#define UA_MON_PARAM_CUE2_MIX   0x07  /* ch_idx=0, 0=on/2=off */
#define UA_MON_PARAM_CUE2_MONO  0x08  /* ch_idx=0, 1=on/0=off */
#define UA_MON_PARAM_UNKNOWN_15 0x15  /* setting[2] bit28, bool */
#define UA_MON_PARAM_DIM        0x44  /* ch_idx=0, 1=on/0=off, bit31 */

/* --- Setting[4]: Mono (dev_type==3 ONLY, NOP on x4) --- */
#define UA_MON_PARAM_MONO       0x36  /* kext property ID (NOT used on x4) */
#define UA_MON_PARAM_MIX_TO_MONO UA_MON_PARAM_MUTE /* uses param 0x03 val=1 */

/* --- Setting[6]: Mirror A / Identify / MirrorsToDigital --- */
#define UA_MON_PARAM_UNKNOWN_0A 0x0a  /* setting[6] bits[9:8], 2-bit */
#define UA_MON_PARAM_IDENTIFY   0x1d  /* ch_idx=0, identify/locate */
#define UA_MON_PARAM_DIGITAL_MIRROR 0x1e /* ch_idx=9, MirrorsToDigital */
#define UA_MON_PARAM_CUE1_MIRROR 0x2e /* ch_idx=9, MirrorA value, 8-bit */

/* --- Setting[7]: Mirror B / TBConfig / misc --- */
#define UA_MON_PARAM_UNKNOWN_0F 0x0f  /* setting[7] bits[9:8], 2-bit */
#define UA_MON_PARAM_UNKNOWN_20 0x20  /* setting[7] bit10, bool (flush) */
#define UA_MON_PARAM_CUE2_MIRROR 0x2f /* ch_idx=9, MirrorB value, 8-bit */
#define UA_MON_PARAM_UNKNOWN_45 0x45  /* setting[7] bit11, bool */
#define UA_MON_PARAM_TB_CONFIG  0x47  /* ch_idx=0, talkback config, bit12 */
#define UA_MON_PARAM_UNKNOWN_48 0x48  /* setting[7] bit13, bool */
#define UA_MON_PARAM_UNKNOWN_63 0x63  /* setting[7] bit14, bool */

/* --- Setting[8]: Digital out mode / CUE alt --- */
#define UA_MON_PARAM_DIGITAL_MODE 0x21 /* ch_idx=0, 0=S/PDIF/8=ADAT */
#define UA_MON_PARAM_UNKNOWN_22 0x22  /* setting[8] bits[29:28], 2-bit */
#define UA_MON_PARAM_CUE1_MIX_ALT 0x23 /* setting[8] bits[25:24], 2-bit */
#define UA_MON_PARAM_UNKNOWN_24 0x24  /* setting[8] bits[31:30], 2-bit */
#define UA_MON_PARAM_CUE2_MIX_ALT 0x25 /* setting[8] bits[27:26], 2-bit */

/* --- Setting[11]: Mirror config / OutputRef --- */
#define UA_MON_PARAM_MIRROR_CFG_A 0x2a /* setting[11] bit0, bool */
#define UA_MON_PARAM_MIRROR_CFG_B 0x2b /* setting[11] bit1, bool */
#define UA_MON_PARAM_MIRROR_CFG_C 0x2c /* setting[11] bit2, bool */
#define UA_MON_PARAM_MIRROR_CFG_D 0x2d /* setting[11] bit3, bool */
#define UA_MON_PARAM_UNKNOWN_30 0x30  /* setting[11] bits[23:16], 8-bit */
#define UA_MON_PARAM_UNKNOWN_31 0x31  /* setting[11] bits[31:24], 8-bit */
#define UA_MON_PARAM_OUTPUT_REF 0x32  /* ch_idx=1, 0=+4dBu/1=-10dBV */
#define UA_MON_PARAM_UNKNOWN_33 0x33  /* setting[11] bit5, bool */
#define UA_MON_PARAM_UNKNOWN_34 0x34  /* setting[11] bit6, bool */
#define UA_MON_PARAM_UNKNOWN_35 0x35  /* setting[11] bit7, bool */

/* --- Setting[12]: HP cue source --- */
#define UA_MON_PARAM_HP1_SOURCE 0x3f  /* ch_idx=1, 0=CUE1/1=CUE2 */
#define UA_MON_PARAM_HP2_SOURCE 0x40  /* ch_idx=1, 0=CUE1/1=CUE2 */

/* --- Setting[13]: Talkback / misc --- */
#define UA_MON_PARAM_UNKNOWN_3A 0x3a  /* setting[13] bits[29:28], 2-bit */
#define UA_MON_PARAM_UNKNOWN_42 0x42  /* setting[13] bits[15:13], 3-bit */
#define UA_MON_PARAM_TALKBACK   0x46  /* ch_idx=0, val=1 ON / val=0 OFF */

/* --- Setting[14]: Dim level / misc --- */
#define UA_MON_PARAM_DIM_LEVEL  0x43  /* ch_idx=0, 1-7 stepped */
#define UA_MON_PARAM_UNKNOWN_49 0x49  /* setting[14] bit31, bool */
#define UA_MON_PARAM_UNKNOWN_66 0x66  /* setting[14] bits[7:0], 8-bit */

/* --- Setting[15]: Mirror enables / clock-HP routing --- */
#define UA_MON_PARAM_UNKNOWN_38 0x38  /* setting[15] bits[6:0], 7-bit */
#define UA_MON_PARAM_UNKNOWN_39 0x39  /* setting[15] bits[22:16], 7-bit */
#define UA_MON_PARAM_MIRROR_ENABLE_A 0x3b /* ch_idx=9, CUE1 mirror enable */
#define UA_MON_PARAM_MIRROR_ENABLE_B 0x3c /* ch_idx=9, CUE2 mirror enable */
#define UA_MON_PARAM_UNKNOWN_3E 0x3e  /* setting[15] bit9, bool */
#define UA_MON_PARAM_UNKNOWN_41 0x41  /* setting[15] bit7, bool */

/* --- Setting[5]: Sound/config flags (type >= 0xa) --- */
#define UA_MON_PARAM_UNKNOWN_67 0x67  /* setting[5] bit3, bool */
#define UA_MON_PARAM_UNKNOWN_68 0x68  /* setting[5] bit4, bool */
#define UA_MON_PARAM_UNKNOWN_87 0x87  /* setting[5] bit9, bool */

/* --- Setting[35] (0x23): Channel config count --- */
#define UA_MON_PARAM_CHAN_CFG   0x6a  /* setting[35] bits[4:0], 5-bit */

/* --- NOP params (cache-only, no WriteSetting in kext) --- */
#define UA_MON_PARAM_DSP_SPANNING  0x16   /* ch_idx=0, DSP pairing mode */
/* 0x0b, 0x0c, 0x0d, 0x10, 0x11, 0x12, 0x17, 0x18, 0x26-0x29 */

/* Clock mode values (from SEL163 SetClockMode) */
#define UA_CLOCK_MODE_INTERNAL   0x00
#define UA_CLOCK_MODE_EXTERNAL   0x0C

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
#define UA_BUS_SUB_MAIN         0  /* pan-adjusted mix coefficient */
#define UA_BUS_SUB_CUE1         1  /* CUE 1 send level */
#define UA_BUS_SUB_CUE2         2  /* CUE 2 send level */
#define UA_BUS_SUB_GAIN_L       3  /* left gain (fader) */
#define UA_BUS_SUB_GAIN_R       4  /* right gain (fader) */

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

/* rb_data[2]: Monitor section */
#define UA_RB_MON_VOLUME(data2)          ((data2) & 0xFF)
#define UA_RB_HP1_VOLUME(data2)          (((data2) >> 8) & 0xFF)
#define UA_RB_MON_MUTE(data2)            (((data2) >> 16) & 1)
#define UA_RB_MON_MONO(data2)            (((data2) >> 17) & 1)
#define UA_RB_MON_DIM(data2)             (((data2) >> 31) & 1)

/* rb_data[3]: Preamp gain, 8 bits per channel */
#define UA_RB_PREAMP_GAIN(data3, ch)     (((data3) >> ((ch)*8)) & 0xFF)

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

/* Clock source values (for clock_config low byte) */
#define UA_CLOCK_INTERNAL   0
#define UA_CLOCK_SPDIF      1  /* Placeholder — verify on HW */
#define UA_CLOCK_ADAT       2  /* Placeholder */
#define UA_CLOCK_WORDCLOCK  3  /* Placeholder */

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
#define UA_NOTIF_BIT_EVENT      BIT(5)   /* Generic event */
#define UA_NOTIF_BIT_CONNECT    BIT(21)  /* Connect response (ACEFACE ack) */
#define UA_NOTIF_BIT_SRATE      BIT(22)  /* Sample rate change */

/* Connection handshake timeouts */
#define UA_CONNECT_RETRIES      20
#define UA_CONNECT_POLLS        10
#define UA_CONNECT_POLL_MS      100
#define UA_PREPARE_TIMEOUT_US   2000

/* Clock change timeout */
#define UA_CLOCK_TIMEOUT_MS     2000

/* DSP ring buffer state */
struct ua_dsp_ring {
	void *pages[UA_RING_MAX_PAGES];
	dma_addr_t page_addrs[UA_RING_MAX_PAGES];
	unsigned int num_pages;
	unsigned int write_ptr;
	unsigned int capacity;      /* num_pages × 256 */
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
	int clock_mode;      /* UA_CLOCK_MODE_INTERNAL / UA_CLOCK_MODE_EXTERNAL */
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

struct ua_audio {
	/* ALSA */
	struct snd_card *card;
	struct snd_pcm *pcm;

	/* DMA buffers — 4 MiB coherent per direction */
	void *play_buf;
	dma_addr_t play_addr;
	void *rec_buf;
	dma_addr_t rec_addr;

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

	/* ALSA mixer control cache (write-through) */
	struct ua_preamp_state preamp[UA_MAX_PREAMP_CH];
	struct ua_monitor_state monitor;

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
	snd_pcm_uframes_t last_play_period_pos;
	snd_pcm_uframes_t last_rec_period_pos;
};

struct ua_device {
	struct pci_dev *pdev;
	void __iomem *regs;         /* BAR0 MMIO base */
	resource_size_t regs_phys;  /* BAR0 physical address */
	resource_size_t regs_size;  /* BAR0 mapped size */

	/* Device identity */
	u32 fpga_rev;               /* Cached FPGA revision from 0x2218 */
	u32 device_type;            /* UAD2DeviceType enum */
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

	/* Mixer batch write state (matches kext CPcieDeviceMixer protocol) */
	u32 mixer_val[UA_MIXER_BATCH_COUNT];   /* Cached setting values */
	u32 mixer_mask[UA_MIXER_BATCH_COUNT];  /* Cached setting masks */
	u32 mixer_seq_wr;                      /* Our cached SEQ_WR */
	bool mixer_dirty;                      /* Any setting changed since last flush */
	bool mixer_ready;                      /* Batch protocol initialized */
	bool mixer_rb_seen;                    /* DSP produced at least one readback */
	u32 mixer_rb_data[UA_MIXER_RB_WORDS];  /* Cached readback (filled by service loop) */
	u32 mixer_rb_status;                   /* Cached readback status */

	/* Character device */
	struct cdev cdev;
	struct device *dev;
	int dev_id;

	/* Synchronization */
	struct mutex lock;          /* Device access lock */
	spinlock_t irq_lock;        /* Interrupt handler lock */

	/* Surprise removal (Thunderbolt hot-unplug) */
	atomic_t shutdown;          /* Set on remove/disconnect — blocks BAR0 access */
};

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

static inline void ua_enable_vector(struct ua_device *ua, unsigned int vector)
{
	int hwbit = ua_vector_to_hwbit(vector);

	if (hwbit < 0)
		return;

	if (hwbit < 32) {
		ua->irq_mask_lo |= BIT(hwbit);
		ua_write(ua, UA_REG_IRQ_ENABLE, ua->irq_mask_lo);
	} else if (ua->fw_v2) {
		ua->irq_mask_hi |= BIT(hwbit - 32);
		ua_write(ua, UA_REG_EXT_IRQ_ENABLE, ua->irq_mask_hi);
	}
}

static inline void ua_disable_vector(struct ua_device *ua, unsigned int vector)
{
	int hwbit = ua_vector_to_hwbit(vector);

	if (hwbit < 0)
		return;

	if (hwbit < 32) {
		ua->irq_mask_lo &= ~BIT(hwbit);
		ua_write(ua, UA_REG_IRQ_ENABLE, ua->irq_mask_lo);
	} else if (ua->fw_v2) {
		ua->irq_mask_hi &= ~BIT(hwbit - 32);
		ua_write(ua, UA_REG_EXT_IRQ_ENABLE, ua->irq_mask_hi);
	}
}

/* Audio subsystem interface (ua_audio.c) */
int ua_audio_preinit_dma(struct ua_device *ua);
int ua_audio_init(struct ua_device *ua);
void ua_audio_fini(struct ua_device *ua);
void ua_audio_irq(struct ua_device *ua, u32 status_lo, u32 status_hi);
int ua_audio_set_clock(struct ua_device *ua, unsigned int rate);
int ua_audio_reconnect(struct ua_device *ua);
void ua_notify_clock_controls(struct ua_device *ua);
void ua_dsp_service_start(struct ua_device *ua);
void ua_dsp_service_stop(struct ua_device *ua);
const char *ua_device_name(u32 device_type);

/* Mixer batch protocol (ua_core.c) */
int ua_mixer_write_setting_locked(struct ua_device *ua,
				  unsigned int index, u32 value, u32 mask);
void ua_mixer_flush_settings(struct ua_device *ua);

/* Preamp/monitor hardware access wrappers (ua_core.c) */
int ua_preamp_set_param(struct ua_device *ua, unsigned int ch,
			u32 param_id, u32 value);
int ua_monitor_set_param(struct ua_device *ua, unsigned int ch_idx,
			 u32 param_id, u32 value);

/* DSP ring buffer and firmware (ua_dsp.c) */
int ua_dsp_rings_init(struct ua_device *ua);
void ua_dsp_rings_fini(struct ua_device *ua);
void ua_dsp_rings_reprogram(struct ua_device *ua);
void ua_dsp_ring_diagnostic(struct ua_device *ua);
void ua_dsp_test_dma_locality(struct ua_device *ua);
int ua_dsp_load_firmware(struct ua_device *ua, const void *fw_data,
			 size_t fw_size);
int ua_dsp_load_firmware_to(struct ua_device *ua, unsigned int dsp_idx,
			     const void *fw_data, size_t fw_size);
int ua_dsp_load_mixer_blocks(struct ua_device *ua);
int ua_dsp_load_mixer_blocks_to(struct ua_device *ua, unsigned int dsp_idx);
int ua_dsp_connect_all(struct ua_device *ua);
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
int ua_dsp_load_programs(struct ua_device *ua);
int ua_dsp_set_bus_param(struct ua_device *ua, u32 bus_id,
			 u32 sub_param, u32 value_u32);
int ua_dsp_set_bus_enable(struct ua_device *ua, u32 bus_idx, int enable);
void ua_dsp_ring_reset(struct ua_device *ua, unsigned int dsp_idx);
int ua_dsp_ring_send_raw(struct ua_device *ua, unsigned int dsp_idx,
			  u32 w0, u32 w1, u32 w2, u32 w3);
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

#endif /* UA_APOLLO_H */
