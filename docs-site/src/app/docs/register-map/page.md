---
title: Register Map
---

The Apollo communicates with the host via memory-mapped I/O (MMIO) registers at PCI BAR0. All register access from userspace goes through the kernel driver's ioctl interface.

---

## BAR0 layout overview

The BAR0 address space is divided into several functional regions:

| Region | Offset range | Purpose |
|---|---|---|
| Global / ID | `0x0000`–`0x0034` | Firmware version, serial number, hardware UID |
| DSP ring banks | `0x2000`–`0x23FF` | Per-DSP command and response ring buffers (DSP 0–3) |
| Interrupt / DMA | `0x2200`–`0x22FF` | Interrupt controller, DMA control, transport |
| Audio extension | `0x2240`–`0x2284` | Audio transport, channel counts, clock |
| Mixer settings | `0x3800`–`0x3A5C` | DSP mixer parameter registers (52 settings) |
| Mixer readback | `0x3810`–`0x3914` | Hardware readback status and data |
| DSP ring banks (high) | `0x5E00`–`0x61FF` | Per-DSP ring buffers (DSP 4–7) |
| DMA scatter-gather | `0x8000`–`0xBFFF` | Playback and capture DMA descriptor tables |
| Notification / SRAM | `0xC000`–`0xC4FF` | Config, notification status, IO descriptors, CLI |

---

## Global and identification registers

| Offset | Name | Access | Description |
|---|---|---|---|
| `0x0000` | `FW_VERSION` | R | Firmware version / identification word |
| `0x0020` | `SERIAL_BASE` | R | 16-byte ASCII serial string (4 registers) |
| `0x0030` | `HW_UID_LO` | R | Hardware unique ID, low 32 bits |
| `0x0034` | `HW_UID_HI` | R | Hardware unique ID, high 32 bits |
| `0x2218` | `FPGA_REV` | R | FPGA revision (v1 devices: bits[31:28] = device type + 1) |
| `0x2234` | `EXT_CAPS` | R | Extended capabilities (v2 FW). Bits[25:20] = device type, bits[15:8] = DSP count |
| `0x2238` | `SERIAL` | R | Serial number / device info |

The serial prefix (first 4 ASCII digits) identifies the device model. For example, prefix `2005` maps to Apollo x4 (device_type `0x1F`).

---

## Interrupt and DMA control

| Offset | Name | Access | Description |
|---|---|---|---|
| `0x2200` | `DMA_CTRL` | R/W | DMA engine control. Enable bits[8:1], reset strobes bits[16:9] |
| `0x2204` | `IRQ_ENABLE` | R/W | Interrupt mask register, low 32 vectors |
| `0x2208` | `IRQ_STATUS` | R/W | Interrupt status register. Write-back to acknowledge |
| `0x220C` | `WALLCLOCK` | R | Wallclock / interrupt pending |
| `0x221C` | `DSP_RESET` | R/W | DSP reset control |
| `0x2220` | `TRANSPORT` | R/W | Transport control (0 = stop) |
| `0x2264` | `EXT_IRQ_STATUS` | R/W | Interrupt status, high 32 vectors (v2 firmware) |
| `0x2268` | `EXT_IRQ_ENABLE` | R/W | Interrupt mask, high 32 vectors (v2 firmware) |

### DMA control bitmask (v2 firmware)

- Bits [8:1] — DMA engine enables (16 engines total with v2 mask `0x1FFFE`)
- Bits [16:9] — Reset strobes. Write 1 to reset, then clear

A DMA reset pulse (set strobes then clear) while engines are enabled kicks the ring buffer DMA engine into an active state. Without this pulse, the FPGA ignores ring buffer doorbells.

---

## Audio extension transport registers

These registers control DMA audio streaming, not the DSP mixer.

| Offset | Name | Access | Description |
|---|---|---|---|
| `0x2240` | `AX_BUF_SIZE` | R/W | Buffer frame size minus 1 |
| `0x2244` | `AX_SAMPLE_POS` | R | Current sample position |
| `0x2248` | `AX_CONTROL` | R/W | Main transport control |
| `0x224C` | `AX_P2P_ROUTE` | R/W | Peer-to-peer routing |
| `0x2250` | `AX_PLAY_CH` | R/W | Total playback channel count |
| `0x2254` | `AX_FRAME_CTR` | R | Frame counter |
| `0x2258` | `AX_FRAME_CFG` | R/W | Frame counter config |
| `0x225C` | `AX_REC_CH` | R/W | Total record channel count |
| `0x2260` | `AX_CONNECT` | W | Connection doorbell |
| `0x226C` | `AX_DMA_BLKSZ` | R/W | DMA block size |
| `0x2270` | `AX_TIMER_CFG` | R/W | Periodic timer interval |
| `0x22C0` | `AX_STATUS` | R | Transport status (read fence) |

### AX_CONTROL bits

| Bit | Name | Description |
|---|---|---|
| 0 | `DMA_EN` | DMA enable |
| 1 | `PLAY_EN` | Playback enable |
| 2 | `REC_EN` | Record enable |
| 3 | `IRQ_EN` | Interrupt enable |
| 9 | `EXT_MODE` | Extended mode (firmware >= 10) |

Start all: write `0x20F` (DMA + play + rec + IRQ + extended mode).

### AX_CONNECT doorbell values

| Value | Action |
|---|---|
| `0x01` | Initiate connection |
| `0x04` | Trigger clock change notification |
| `0x10` | Initiate disconnection |

---

## Clock registers

| Offset | Name | Access | Description |
|---|---|---|---|
| `0x2280` | `CLOCK_SRC0` | R/W | Clock source register 0 |
| `0x2284` | `CLOCK_SRC1` | R/W | Clock source register 1 |

---

## Mixer settings window (`0x3800`)

The DSP mixer engine uses a shared memory window at BAR0 + `0x3800` for parameter exchange between host and DSP.

| Offset | Name | Access | Description |
|---|---|---|---|
| `0x3808` | `MIXER_SEQ_WR` | R/W | Sequence counter, host to DSP |
| `0x380C` | `MIXER_SEQ_RD` | R | Sequence counter, DSP readback |
| `0x38B4`–`0x3A5C` | `MIXER_SETTING[0..51]` | R/W | 52 settings, 8 bytes each (2 x u32) |

Each setting occupies two 32-bit words with paired value/mask encoding:

```
wordA = (changed_mask[15:0]  << 16) | value[15:0]
wordB = (changed_mask[31:16] << 16) | value[31:16]
```

The upper 16 bits of each word are the changed-bits mask. If the mask is zero for a word, the DSP ignores that half. See [DSP Protocol](/docs/dsp-protocol) for the sequence counter handshake.

---

## Hardware readback registers

| Offset | Name | Access | Description |
|---|---|---|---|
| `0x3810` | `MIXER_RB_STATUS` | R/W | 1 = data ready. Write 0 to re-arm |
| `0x3814`+ | `MIXER_RB_DATA` | R | 40 consecutive u32 words of readback data |

### Readback data fields

**rb_data[0] — Preamp flags** (4 channels, 6-bit stride):

| Bits | Field |
|---|---|
| `ch*6 + 0` | Mic/Line (1=Line, 0=Mic) |
| `ch*6 + 1` | PAD |
| `ch*6 + 2` | Link |
| `ch*6 + 3` | 48V phantom |
| `ch*6 + 4` | Low-cut filter |
| `ch*6 + 5` | Phase invert |
| `24` | HiZ active (hardware auto-detect) |

**rb_data[2] — Monitor section:**

| Bits | Field |
|---|---|
| `[7:0]` | Monitor volume (raw 8-bit) |
| `[15:8]` | HP1 volume (raw 8-bit) |
| `16` | Mute |
| `17` | Mono |
| `31` | Dim |

**rb_data[3] — Preamp gain** (4 channels, 8-bit stride):

| Bits | Field |
|---|---|
| `ch*8 + [7:0]` | Gain value per channel |

**rb_data[6]:** bit 8 = Talkback active.

**rb_data[7]:** bits [7:0] = HP2 volume.

**rb_data[8]–rb_data[17]:** Static `0xf0000000`. These are NOT audio meters — audio metering is computed in software from PCM samples.

---

## Notification and SRAM region (`0xC000`)

### Bank offset

Many notification registers are bank-shifted. For Apollo x4, the bank shift is `0x0A`, adding an offset of `0x28` bytes:

```
effective_address = 0xC000 + bank_shift * 4 + register_offset
```

### Bank-shifted registers

| Effective offset | Name | Description |
|---|---|---|
| `0xC028` | `NOTIF_READBACK` | Readback buffer (up to 95 dwords) |
| `0xC02C` | `NOTIF_ACEFACE` | ACEFACE handshake write target |
| `0xC030` | `NOTIF_STATUS` | Notification status (connect ack, events) |
| `0xC074` | `NOTIF_CLOCK_CFG` | Sample rate / clock source write |
| `0xC07C` | `NOTIF_RATE_INFO` | Rate info readback |
| `0xC080` | `NOTIF_XPORT_INFO` | Transport info readback |
| `0xC084` | `NOTIF_CLOCK_INFO` | Clock source info readback |

### Config write area (no bank shift)

| Offset | Name | Description |
|---|---|---|
| `0xC000` | `NOTIF_CONFIG[0]` | Bank shift value (written after connect) |
| `0xC004` | `NOTIF_CONFIG[1]` | Readback count (`0x17C` = 380) |
| `0xC008`–`0xC024` | `NOTIF_CONFIG[2..9]` | Reserved (zero) |

### Notification status bits (`0xC030`)

| Bit | Name | Description |
|---|---|---|
| 0 | `OUT_IO` | Output IO descriptors ready |
| 1 | `IN_IO` | Input IO descriptors ready |
| 4 | `TRANSPORT` | Transport/clock change |
| 5 | `EVENT` | Generic event |
| 21 | `CONNECT` | Connect response (ACEFACE ack) |
| 22 | `SRATE` | Sample rate change |

---

## CLI register interface (`0xC3F4`)

The CLI (Command Line Interface) provides communication with the ARM microcontroller that controls preamps (PGA2500 SPI), phantom power relays, and the identify LED.

| Offset | Name | Access | Description |
|---|---|---|---|
| `0xC3F4` | `CLI_ENABLE` | W | Write 1 to enable CLI |
| `0xC3F8` | `CLI_STATUS` | R | Command status / length |
| `0xC3FC` | `CLI_RESP_LEN` | R | Response data length (bytes) |
| `0xC400` | `CLI_CMD_BUF` | W | Command buffer (128 bytes, 32 x u32) |
| `0xC480` | `CLI_RESP_BUF` | R | Response buffer (128 bytes, 32 x u32) |

---

## DMA scatter-gather regions

| Offset | Name | Description |
|---|---|---|
| `0x8000` | `PLAY_DMA_BASE` | Playback DMA descriptor base (1024 entries) |
| `0xA000` | `REC_DMA_BASE` | Record DMA descriptor base (1024 entries) |

Each direction uses a 4 MiB DMA buffer. Descriptors point to 4 KiB pages.

---

## Register access from userspace

All register access goes through the kernel driver's ioctl interface on `/dev/ua_apollo0`:

| Ioctl | Number | Struct size | Description |
|---|---|---|---|
| `READ_REG` | `0x10` | 8 bytes | Read BAR0 register (offset, value) |
| `WRITE_REG` | `0x11` | 8 bytes | Write BAR0 register (offset, value) |
| `SET_MIXER_BUS_PARAM` | `0x30` | 16 bytes | Write bus coefficient via DSP ring |
| `SET_MIXER_PARAM` | `0x31` | 16 bytes | Write mixer parameter (preamp, monitor) |
| `WRITE_MIXER_SETTING` | `0x36` | 16 bytes | Atomic mixer setting write with seq handshake |
| `READ_MIXER_SETTING` | `0x37` | 16 bytes | Atomic mixer setting read |
| `CLI_COMMAND` | `0x40` | 264 bytes | CLI command to ARM MCU |
| `GET_HW_READBACK` | `0x41` | 164 bytes | Read DSP readback (status + 40 data words) |

---

## Further reading

- [DSP Protocol](/docs/dsp-protocol) — ring buffer commands and settings batch protocol
- [Architecture Overview](/docs/architecture-overview) — system diagram and data flow
