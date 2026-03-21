---
title: DSP Protocol
---

The Apollo's onboard SHARC DSP processors are controlled through two mechanisms: a ring buffer command interface for real-time operations, and a shared SRAM settings window for mixer parameters.

---

## Ring buffer architecture

Each DSP has a 128-byte register bank split into two rings:

| Ring | Offset from bank base | Direction | Purpose |
|---|---|---|---|
| Command | `+0x00` | Host to DSP | Send commands, firmware, routing data |
| Response | `+0x40` | DSP to host | Receive completion status, readback |

### DSP bank addresses

| DSP | Bank base |
|---|---|
| DSP 0 | `0x2000` |
| DSP 1 | `0x2080` |
| DSP 2 | `0x2100` |
| DSP 3 | `0x2180` |
| DSP 4–7 | `0x5E00` + `idx * 0x80` |

### Ring registers (per ring)

| Offset | Name | Description |
|---|---|---|
| `0x00`–`0x1C` | Page addresses | Up to 4 DMA page addresses (low/high pairs) |
| `0x20` | Write pointer | Host write position |
| `0x24` | Doorbell | Write triggers DSP processing |
| `0x28` | Position | Hardware current position (read-only) |

### Ring entry format

Each ring entry is 16 bytes (4 x u32):

**Inline command:**

```
word0: command_code
word1: parameter
word2: 0
word3: 0
```

**DMA reference (BIT31 set in word0):**

```
word0: (size_in_dwords | 0x80000000)
word1: 0
word2: bus_address_low
word3: bus_address_high
```

DMA references point to coherent DMA buffers in host memory. The FPGA reads the referenced data via PCIe DMA.

### Ring capacity

Each ring page holds 256 entries (4096 bytes / 16 bytes per entry). DSP 0 uses 4 pages (1024 entries) for its command ring to accommodate firmware loading. Other DSPs use 1 page (256 entries).

---

## Doorbell protocol

The doorbell write order is critical:

1. Write all entries to the ring pages
2. Issue a write memory barrier (`wmb()`)
3. Write `DOORBELL` (offset `0x24`) with the new write pointer value
4. Write `WRITE_PTR` (offset `0x20`) with the same value

Writing `DOORBELL` first, then `WRITE_PTR` matches the working driver behavior. Reversing the order causes the FPGA to begin DMA reads immediately, and the subsequent doorbell write during an active transfer resets the DMA engine.

---

## Key ring buffer commands

### DSP connect (`0x00230002`)

Sent as an inline command to initialize each DSP after firmware loading:

```
Phase 1: For each DSP i:
    {0x00230002, 1, 0, 0} + doorbell

Phase 2: DSP 0 only (finalize):
    {0x00100002, 0, 0, 0} + doorbell
```

### Bus coefficient write (`0x001D0004`)

Sets mixer bus parameters (fader levels, pan coefficients, send gains):

```
word0: 0x001D0004    (command type 0x1D, 4 dwords)
word1: bus_id        (0x00–0x1F)
word2: sub_param     (0=main, 1=CUE1, 2=CUE2, 3=gainL, 4=gainR)
word3: value         (IEEE 754 float as u32)
```

Bus coefficients go through the ring buffer, not through the mixer settings registers. This is a completely separate hardware path.

### SRAM config (`0x00080004`)

Writes data to SHARC DSP SRAM at a specified address:

```
Entry 1 (inline):
    word0: 0x00080004
    word1: sram_destination_address
    word2: 0
    word3: data_size_in_dwords

Entry 2 (DMA reference):
    word0: (size_in_dwords | 0x80000000)
    word1: 0
    word2: dma_address_low
    word3: dma_address_high
```

Both entries are submitted as a single atomic pair with one doorbell. Used for routing table and module configuration writes.

---

## Firmware loading

Firmware is loaded to DSP 0 via DMA:

1. Allocate a single contiguous DMA buffer containing: `[cmd_word, count_word, param_word, firmware_data...]`
2. Allocate a response DMA buffer
3. Submit one DMA reference entry to the response ring, ring its doorbell
4. Submit one DMA reference entry to the command ring, ring its doorbell
5. Poll the response buffer for completion (upper 16 bits of response word 0 == `0x8004`)

### Command buffer layout

```
word[0]: 0x40120000    (FW_CMD | LARGE_FLAG)
word[1]: fw_dwords + 2 (total count)
word[2]: 0x80040000    (FW_PARAM)
word[3..N]: firmware binary data
```

The FPGA processes the entire payload as a single DMA reference entry. Chained entries do not work for firmware loading.

### Timeout

Firmware loading polls for up to 30 seconds at 10ms intervals. Typical completion time is under 5 seconds.

---

## sendBlock protocol

For sending data blocks (routing tables, module activation, PostConnect descriptors) to any DSP:

1. **Response ring:** Submit a 4-dword DMA reference pointing to a response buffer. Ring doorbell.
2. **Command ring:** Submit a header DMA reference (1 dword: `(data_dwords + 1) | cmd`)
3. **Command ring:** Submit data as DMA reference entries, split at 1 KB chunk boundaries
4. **Command ring:** Single doorbell for all command entries (batch submission)
5. **Poll:** Check response buffer for non-zero word (completion)

The response ring entry must be submitted before the command ring entries — the FPGA needs to know where to write its response before processing the command.

---

## ACEFACE connect sequence

Apollo devices that use the AudioExtension path (including Apollo x4, x6) use a register-based handshake instead of ring buffer connect commands:

1. Write `0xACEFACE` to `NOTIF_ACEFACE` (`0xC02C`)
2. Write `0x01` to `AX_CONNECT` (`0x2260`) — doorbell
3. Poll `NOTIF_STATUS` (`0xC030`) for bit 21 (`CONNECT`) to be set
4. Read notification status bits for IO descriptor readiness
5. Write config words to `NOTIF_CONFIG` (`0xC000`): bank shift, readback count
6. Read IO channel descriptors from fixed SRAM offsets

After ACEFACE completes, the DSP ring buffers and DMA engine are initialized.

---

## Settings batch protocol

Mixer settings (monitor volume, preamp flags, mute, dim) use the shared SRAM window at BAR0 + `0x3800`, not the ring buffer.

### Sequence counter handshake

The protocol uses two sequence counters for synchronization:

| Register | Offset | Direction |
|---|---|---|
| `MIXER_SEQ_WR` | `0x3808` | Host writes, DSP reads |
| `MIXER_SEQ_RD` | `0x380C` | DSP writes, host reads |

### Write procedure

1. **Wait for DSP idle:** Poll `SEQ_RD` until it equals `SEQ_WR` (DSP has processed previous batch)
2. **Write all settings:** Write paired (value, mask) words to all 38 setting registers
3. **Bump sequence counter:** Increment `SEQ_WR` and write it
4. **Wait for acknowledgment:** Poll `SEQ_RD` until it matches the new `SEQ_WR` value

### Why batch writes matter

The DSP processes all settings as an atomic batch when the sequence counter changes. Writing individual settings with per-setting sequence bumps crashes the DSP. All 38 settings must be cached and written together with a single `SEQ_WR` increment.

A clock write with source value `0x0C` is used to activate DSP settings processing.

### Setting value encoding

Each setting occupies 8 bytes (2 x u32) with paired value/mask encoding:

```
wordA = (changed_mask[15:0]  << 16) | value[15:0]
wordB = (changed_mask[31:16] << 16) | value[31:16]
```

If the mask bits are zero, the DSP ignores the corresponding value bits. This allows selective field updates within a setting without disturbing other fields.

### Word order

Settings 0–31 use word B for the active value. Settings 32+ use word A. This asymmetry was confirmed via BAR0 register tracing on a working system.

---

## Plugin chain activation

After firmware loading and DSP connect, the DSP kernel runs but the mixer task is inactive. Activating it requires replaying a captured sequence of inline ring buffer commands:

- `SRAM_CFG` commands — write routing configuration to SRAM
- `MODULE_ACTIVATE` commands — start DSP processing modules
- `SYNCH` commands — synchronize module states

These commands are sent in batches of 64 to DSP 0, with the ring reprogrammed between batches. The complete plugin chain for Apollo x4 contains over 1,300 commands.

Without plugin chain activation, the DSP ignores SRAM-based parameter writes (monitor volume, preamp flags). Ring buffer commands like bus coefficients still work because they bypass the mixer task.

---

## Routing table loading

Audio routing (which inputs map to which outputs, DMA channel assignments) is configured via `sendBlock` with routing table data. The driver maintains static routing tables for each direction:

- **Record routing:** 22 channels — maps ADC/mixer outputs to DMA capture channels
- **Playback routing:** 24 channels — maps DMA playback channels to DAC/mixer inputs

Routing tables are loaded after DSP connect and before transport start.

---

## Further reading

- [Register Map](/docs/register-map) — complete BAR0 register layout
- [Architecture Overview](/docs/architecture-overview) — system diagram and hardware write paths
