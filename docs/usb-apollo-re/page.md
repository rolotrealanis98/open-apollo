# Apollo USB Series — Reverse Engineering Findings

## Device Family (VID 0x2B5A)

### Firmware Loader PIDs (Cypress FX3 stub)
| PID | Device |
|-----|--------|
| `0x0001` | Apollo / Twin USB |
| `0x0004` | Satellite USB |
| `0x000C` | Arrow / Solo USB |
| `0x000E` | Twin X USB |

### Live Device PIDs (after firmware load)
| PID | Device | Audio | Device Type |
|-----|--------|-------|-------------|
| `0x0002` | Twin USB / UAD2 USB Duo | UAC 2.0 | ? |
| `0x0003` | UAD2 USB Duo (legacy) | None | ? |
| `0x0005` | UAD2 Satellite USB Quad | None | ? |
| `0x0006` | UAD2 Satellite USB Octo | None | ? |
| `0x000D` | **Apollo Solo USB** | UAC 2.0 | `0x41` |
| `0x000F` | Twin X USB | UAC 2.0 | ? |

## Architecture

### Hardware Stack
```
Host USB ←→ Cypress FX3 "Cauldron" firmware ←→ FPGA (via GPIF parallel bus)
```
- FX3 handles USB protocol, firmware loading, DMA
- FPGA handles audio I/O, DSP, mixer, preamp control
- FX3↔FPGA communicate via Cypress GPIF-II interface + DMA sockets

### Boot Sequence
1. Cypress FX3 chip powers on → presents as VID `0x2B5A` PID `0x000C` (Solo)
2. Descriptor: USB 2.0, 1 interface, 0 endpoints, Vendor Specific Class (0xFF)
3. Host uploads `ApolloSolo.bin` via vendor request 0xA0 (standard FX3 protocol)
4. FX3 re-enumerates on **USB 3.0 SuperSpeed** as VID `0x2B5A` PID `0x000D`
5. USB Composite Device with 4 interfaces (1 DSP + 3 audio)

### USB Interfaces (post-firmware)
| Interface | Name | USB Class | Endpoints | Purpose |
|-----------|------|-----------|-----------|---------|
| 0 | DSP Accelerator | `0xFF` Vendor | EP1 OUT (bulk), EP1 IN (bulk), EP6 IN (interrupt) | DSP/mixer control |
| 1 | Audio Control | `0x01` Audio | EP5 IN (interrupt) | UAC 2.0 control |
| 2 | Audio Streaming (Play) | `0x01` Audio | EP3 OUT (isoc async) | Playback |
| 3 | Audio Streaming (Rec) | `0x01` Audio | EP3 IN (isoc async) | Recording |

### Audio I/O Map
| Direction | Alt Setting | Channels | Bit Depth | Description |
|-----------|-------------|----------|-----------|-------------|
| Play Alt1 | 1 | **6ch** | 24-bit | 48k playback (MON L, ...) |
| Play Alt2 | 2 | **4ch** | 24-bit | 192k playback |
| Rec Alt1 | 1 | **10ch** | 24-bit | 48k recording (MIC/LINE/HIZ 1, ...) |
| Rec Alt2 | 2 | **10ch** | 24-bit | 96k recording |
| Rec Alt3 | 3 | **6ch** | 24-bit | 192k recording |

- Audio control: 10ch playback input terminal, 20ch record input terminal
- Clock source ID 128: internal programmable, read/write frequency control
- USB 3.0 SuperSpeed + High Speed capable
- Self-powered, asynchronous isochronous

### Firmware Details
- **Name**: "Cauldron" (firmware v1.3 build 3, Feb 18 2026)
- **SDK**: Cypress FX3 SDK
- **Architecture**: ARM32
- **Entry point**: 0x40011500
- **Main loop**: "Aldrin"
- **FX3 GPIOs**: CAPTURE_REQUEST, AUDIO_THREAD_RUNNING, USB_CLOCK, ASYNC_INTR, FPGA_DETECT
- **FPGA communication**: opcode-based with magic headers, DMA sockets for bulk data
- **Clock sources**: Internal, S/PDIF, ADAT

### Firmware Files
Required — FX3 has no onboard flash, host must upload every power-on.

**How users obtain firmware:**
- Download from [UA firmware page](https://help.uaudio.com/hc/en-us/articles/26454031439892)
- Or extract from UA Connect (free): `C:\Program Files (x86)\Universal Audio\Powered Plugins\Firmware\USB\`
- Place in `/lib/firmware/universal-audio/` on Linux

Files located at `C:\Program Files (x86)\Universal Audio\Powered Plugins\Firmware\USB\`

| File | Size | Device |
|------|------|--------|
| `ApolloSolo.bin` | 145,816 B | Apollo Solo USB |
| `ApolloTwin.bin` | 147,448 B | Twin USB |
| `ApolloTwinX.bin` | 147,416 B | Twin X USB |
| `Satellite.bin` | 128,404 B | Satellite USB |

- **Format**: Standard Cypress FX3 boot image (`CY` magic at offset 0)
- Firmware sections: 0x100 (vectors), 0x40003000 (main), 0x40013000 (ext), 0x40030000 (data)
- Locally archived in `tools/usb-re/*.bin`

### Firmware Loader
- UMDF (User-Mode Driver Framework) via WinUSB
- Binary: `uad2fx3ldr.dll` (149 KB) — reads firmware from disk
- Linux loader: `tools/fx3-load.py` — works, tested successfully

## Vendor Control Requests (Interface 0)

| Request | Dir | Size | Description |
|---------|-----|------|-------------|
| 0x00 | IN | 4B | Protocol version: `02 00 01 00` |
| 0x02 | IN | 2-4B | Status / current sample rate (LE uint32 after clock init) |
| 0x04 | IN | 512B | Buffer/state (all zeros until DSP init) |
| 0x0A | IN | 48B | JKMK register read (wValue selects index) |
| 0x0E | IN | 4B | Config A: `03 00 03 01` |
| 0x0F | IN | 4B | Config B: `EC 03 03 01` |
| 0x10 | IN | 512B | Device info block (device type, FPGA addresses, FX3 config) |

### Device Info Block (request 0x10, 512 bytes)
- Offset 0x0C: `0x41` — device type for Apollo Solo USB
- Offset 0x68: `fefe fefe fefe` — default/uninitialized values
- Offset 0x170: `" 3XF"` — FX3 identifier string
- Offset 0x1C5: `0x1F` — possibly DSP type (matches Thunderbolt x4)
- Contains FPGA memory addresses (0xE000xxxx range)
- Contains firmware addresses (0x4000xxxx range matching loaded sections)

### Protocol Magic Values
- **"JKMK"** — register/parameter read response header (vendor ctrl 0x0A)
- **"JFK"** — interrupt notification header (seen on EP6 IN)

## UAC 2.0 Clock Status
- Clock ID 128 responds to GET_CUR: returns **48000 Hz**
- GET_RANGE: **STALL** (not implemented) — blocks `snd-usb-audio` enumeration
- SET_CUR: timeout (may need DSP initialization first)

## Linux Status

### Working
- [x] FX3 firmware upload from Linux (`tools/fx3-load.py`), udev auto-load on device plug
- [x] Device re-enumerates as UAC 2.0 audio + vendor DSP
- [x] `snd-usb-audio` claims device with four patches applied (see below)
- [x] Clock source responds at 48kHz after DSP init
- [x] **6ch ALSA playback** (S32_LE 48kHz) — confirmed on Ubuntu Studio 24.04 (kernel 6.17, Intel Tiger Lake-H) and CachyOS (kernel 6.19, AMD USB)
- [x] **10ch ALSA capture** — confirmed on Ubuntu Studio 24.04 with `usb-full-init.py` (one-shot, 38 packets including DSP program load)
- [x] **PipeWire capture** — mic input working end-to-end (Discord voice calls, `pw-record`); confirmed on Ubuntu Studio 24.04 / Intel Tiger Lake-H
- [x] PipeWire playback — browser audio, system audio working
- [x] **Hardware monitoring** (mic → headphones) simultaneous with PipeWire streams
- [x] DSP init — FPGA activation, CONFIG_A/B, routing table, DSP program load, clock set, monitor level (`tools/usb-full-init.py`, one-shot, no daemon required)
- [x] Mixer settings via vendor control 0x03 — preamp gain, 48V, monitor level/mute (seq counter fix applied)

### Known Issues
- **SET_INTERFACE resets FPGA state on stream close** — fixed by `QUIRK_FLAG_IFACE_SKIP_CLOSE` patch (quirks.c, patch 4). Without it, closing a capture stream resets Interface 3 to alt=0 and wipes FPGA capture routing
- **EP6 interrupt flood on Intel xHCI** — the Apollo pushes JFK notifications at ~2000/sec. No longer requires a drain daemon; the one-shot `usb-full-init.py` init stabilizes the device. AMD controllers handle it gracefully without any workaround
- **Firmware-specific init sequence** — the captured 38-packet init from Cauldron 1.3 build 3 works on some devices but crashes others at packet 28 (SRAM address mismatch in IIR biquad writes). Observed with CachyOS/AMD contributor (@ariahello) using a different Cauldron build
- **Cold DSP needs ~30 s after firmware boot before the init** — on an Apollo Twin USB, a full init replay started ~5 s after the device re-enumerates as `0x0002` brings up converters, routing and monitoring but silently never instantiates the UAD chain; the same replay after a 30 s settle does. Instrumented cold vs. warm runs are identical at the USB layer (934 vs 919 responses, 0 errors either way), so the only variable is elapsed time. 30 s is known-good, not bisected; the true minimum is somewhere between 5 and 30 s. Likely the same variable as the packet-28 stall above (see #62)
- **Interface 0 contention (resolved)** — EP6 drain daemon was causing Interface 0 conflicts with `snd-usb-audio`. Daemon has been removed from the stack entirely
- **udev `RUN+=` silently fails to import pyusb (fixed)** — `systemd-udevd.service` runs with `MemoryDenyWriteExecute` and a restrictive `SystemCallFilter`; `RUN+=` children inherit that sandbox. On systemd with these hardening defaults (confirmed on Ubuntu 26.04), this makes every `RUN+=`-invoked Python init script fail with `ModuleNotFoundError: No module named 'usb'` on every boot/hotplug — 100% reproducible, not transient — even though the same command succeeds when run manually. Since `usb-full-init.py` never ran, FPGA capture routing was never programmed automatically, so the device would enumerate and play back audio fine but capture nothing. Fixed by triggering init via `TAG+="systemd", ENV{SYSTEMD_WANTS}=...}` instead of `RUN+=` (see `configs/systemd/`), since plain systemd services don't inherit udevd's sandbox

### Required Init Order
1. Upload firmware (`tools/fx3-load.py` or udev auto-trigger)
2. Run `tools/usb-full-init.py` (one-shot: FPGA activate, CONFIG_A/B, routing table, DSP program load, clock, monitor level — 38 packets)
3. Load patched `snd-usb-audio` module (`modprobe snd_usb_audio`)
4. Start PipeWire

## Windows Driver Stack

| INF | Original Name | Version | Purpose |
|-----|---------------|---------|---------|
| oem31 | `uad2fx3ldr.inf` | 0.27.42.864 (Apr 2025) | FX3 firmware loader |
| oem34 | `uad2usb.inf` | 21.26.41.714 (Jan 2026) | USB DSP accelerator |
| oem36 | `uausbaudio.inf` | 6.0.0.40878 (Dec 2025) | USB audio (Thesycon) |
| oem37 | `uausbaudioks.inf` | 6.0.0.40878 (Dec 2025) | USB audio KS |

- Build system: `D:\bh\xml-data\build-dir\UAD2I-UAD2DN-WIN\`
- Key function: `_sendDSPCommandWithResponse` — sends DSP blocks via bulk
- Shares code with PCIe driver: `PcieDevice sendBlock`
- `CUsbDevice::ProgramRegisters` — register programming function

## DSP Bulk Protocol (EP1 IN/OUT)

**Decoded from USBPcap capture (`tools/usb-re/apollo-init.pcap`, 14MB, 16654 packets)**

### Packet Format
```
Header (4 bytes — four independent fields, not a u16 word count):
  [word_count : u8]  — payload length in 32-bit words (255 max, so 1024-byte packets)
  [flags      : u8]  — 0x00 or 0x10: which of two logical streams this packet belongs to
  [seq        : u8]  — sequence counter, +1 per packet, wraps at 256
  [magic      : u8]  — 0xDC = command (OUT), 0xDD = response (IN)

Payload (word_count * 4 bytes): a stream of entries, each starting with one dword:
  [wordcount : u16 LE]  — entry length in dwords, INCLUDING this dword
  [reg       : u16 LE]  — register

  wordcount 1  : register read        [hdr]
  wordcount 2  : register write       [hdr][value]
  wordcount 4  : routing/table entry  [hdr][DSP address][mask][value]
  wordcount 5+ : block write          [hdr][DSP address][flags/mask][payload ...]
```

The low 16 bits of an entry are a **length**, not an opcode. Reading them as "0x0001 = query, 0x0002 = write" is right for those two lengths and only those — a parser that steps 4 or 8 bytes resumes mid-entry on anything longer and reads payload data as fresh headers, which is what "register 0xC4B5" or "opcode 542467070" in a decode means. (`tools/usb-re/pcap-mixer-decode.py` did both of these until #70/#76.)

**Entries span packets, per stream.** A block write can be far longer than one packet — the largest seen is 6603 dwords (26 KB, register `0x0001`) — and the remainder simply continues in the next packet **with the same flags byte**. The two flags values are two independent streams multiplexed on EP1; reassemble each on its own and every entry parses. Verified on two Apollo Twin USB init captures (2341 and 2596 command packets): split by flags and walked as streams they yield 5482 and 5616 entries that consume every dword with nothing left over. Walked packet-by-packet, the same captures fail on the 447 1024-byte program-load packets that are continuations.

### Observed Init Sequence
```
# Frame 77 — Host sends init command (seq=0)
OUT: 04 00 00 DC  02 00 23 00 01 00 00 00  02 00 10 00 48 14 B7 01
     ^header      ^write reg 0x23 = 1       ^write reg 0x10 = 0x01B71448

# Frame 78 — Host sends readback request (seq=1)
OUT: 04 00 01 DC  02 00 10 00 48 14 B7 01  02 00 11 00 20 24 E5 0A
     ^header      ^reg 0x10                 ^reg 0x11

# Frames 81-94 — Device returns full register dump (3,116 bytes total)
IN:  26 00 00 DD  02 03 03 80 20 24 E5 0A  00 00 00 81 ...  (38 words)
IN:  64 00 01 DD  ...  (100 words)
IN:  75 00 02 DD  ...  (117 words)
...continues across 8 response packets...

# Frame 309 — Host queries status (seq=1)
OUT: 01 00 01 DC  01 00 27 00
     ^1 word      ^wordcount=1 (read) reg=0x27

# Frame 311 — Device responds with status
IN:  02 00 08 DD  02 00 0D 80  42 01 00 00
     ^2 words     ^wordcount=2 reg=0x800D  value=0x142 (322)
```

### Key Registers
| Register | Direction | Purpose |
|-------|-----------|---------|
| 0x0023 | Write | Init/connect (value=1 to activate, 0 to query) |
| 0x0010 | Read/Write | Config word A (0x01B71448) |
| 0x0011 | Read/Write | Config word B (0x0AE52420) |
| 0x0027 | Read | Status request (wordcount 1) |
| 0x800D | Response | Status response (value 0x142) |

Additional registers seen in the Apollo Twin USB init (two captures, Console 11.7.0). Purposes marked with `?` are inferred from where the writes sit in the sequence, not decoded:

| Register | Entry sizes | Purpose |
|-------|-----------|---------|
| 0x0001 | up to 6603 dwords | Block loads: 156 entries, ~389 KB per init, byte-identical between the two captures; every payload begins with an ASCII `Bill` tag — DSP program image? |
| 0x0004 | up to 1115 dwords | Block loads of the same shape (`Bill` tag), 24 entries, ~39 KB per init |
| 0x0003 | 2 | DSP / clock parameters (117 register writes) |
| 0x0008 | 4 | DSP program load, one `[address][mask][value]` per entry (584 of them) |
| 0x000C | 4 | UAD plugin chain upload (1433 packets, last phase). Also ~180 entries/s of steady-state polling while Console is open |
| 0x0014 | 4–347 | Plugin parameter / coefficient writes (~400 per init); the sweep target for UAD parameter control |
| 0x001A | 2 | **Sample rate as a plain integer Hz**: `0x00AC44`=44100, `0x00BB80`=48000, `0x015888`=88200, `0x017700`=96000, `0x02B110`=176400, `0x02EE00`=192000 — all six confirmed from a rate sweep |
| 0x001D / 0x001E / 0x001F | 4 | Routing matrix (1284 / 399 / 40 entries per init) |
| 0x0006, 0x0015, 0x001C | 2–153 | Seen, not decoded |

### Register Dump Structure
The init response returns ~780 32-bit words across 8 bulk packets, containing
the full mixer/DSP state. Values are mostly `0x80000000` (muted/default) and
`0x83000000`, similar to the Thunderbolt mixer settings format.

### The init is bulk AND control — Apollo Twin USB

**Decoded from two USBPcap captures of an Apollo Twin USB DUO (`2b5a:0002`) powering on under Console 11.7.0; replayed on Linux with `tools/usb-re/init-replay.py` (#77).**

The bulk stream is not the whole init. Interleaved with the 2341 bulk OUT packets are **45 vendor control OUT transfers** (`bmRequestType 0x41`) that carry state the bulk stream never touches:

| bRequest | wValue | Length | Purpose |
|----------|--------|--------|---------|
| 3 | `0x062D` | 128 | Settings mask+value block — same protocol as the mixer section below; monitor level at slot 2 |
| 3 | `0x064F` | 128 | Value block — preamp gain, slots 0 and 1 |
| 3 | `0x0670` | 48 | Extended block |
| 3 | `0x0602` | 4 | Commit — incrementing sequence counter |
| 5 | `0x0001` | 0 | **Arm** |
| 7 | `0x000C` | 0 | **Commit register `0x000C`** (the plugin-chain register) |
| 12 | `0x012E` | 0 | Apply (pre-commit) |
| 13 | `0x0000` | 0 | Apply (post-commit), always paired with 12 |

The `wValue`s of 5 and 12 are session counters, not constants: `bReq 5` carried `0x0001` in one capture and `0x0002` in the other, `bReq 12` carried `0x012E` and `0x012B`. The Solo table below lists `0x0115` / `0x0010` for the same pair.

**A bulk-only replay is about half the init.** It brings up converters, routing and monitoring — audio works — but preamp state and the UAD chain do not come up. With the control transfers included, the preamp gain lands (input level on ch0 rises ~55 dB) and the Unison chain runs.

**The `bReq 5` / `bReq 7` pair is the reason the plugin chain looked PACE-protected.** Without it the DSP accepts all 1433 plugin-chain packets, acknowledges every one, and does nothing. That is exactly what "the replay is authorised on Windows but ignored on Linux" looks like from outside — and it is not PACE, it is a missing arm/commit handshake. With the pair sent immediately before the chain upload, the chain instantiates and processes audio (confirmed by third-octave A/B against a cold start: Neve 1073 high-pass at 50 Hz, VOXBOX peak at 12.5 kHz, 6σ). The pair appears four times in one capture and once in the other; once is enough.

Phases of the Twin init as the extractor splits them (idle gap > 150 ms):

| Phase | Bulk | Control | What |
|-------|------|---------|------|
| 0–2 | 5 | 0 | INIT, CONFIG_A/B, STATUS read |
| 3 | 75 | 0 | Routing matrix |
| 4 | 176 | 8 | Block loads (`0x0001`), DSP program (`0x0008`), settings blocks |
| 5 | 652 | 11 | Main DSP / routing bring-up, first apply |
| 6–9 | 0 | 8 | Arm / commit handshake (`bReq 5`, `bReq 7`) |
| 10 | 1433 | 18 | Plugin chain (`0x000C`) |

Two things that did **not** carry over from the Solo notes: the plugin chain did not corrupt capture routing on the Twin (`setting[24] = 0x20` never appeared), and the amber front-panel indicator is not evidence the chain loaded — it reflects the Unison preamp mode set by the phase 4–5 control blocks, and comes on with phases 0–5 alone.

Everything above the firmware survives a host restart as long as the Apollo keeps its own power: converters, routing, preamp state and the chain were all still in place after a reboot with the init inhibited (ch1 noise floor matched to 0.00 dB, chain-signature bands unchanged). Cutting the Apollo's power drops it back to the loader PID and clears all of it.

## Linux DSP Init — Working
Replay of Windows init sequence via `tools/usb-dsp-init.py` **works**:
- Init command (reg 0x23=1) accepted
- Readback returns 476-500B register dumps across 8 packets
- Status query returns 388B state
- Clock reads 48000 Hz after init

## snd-usb-audio Kernel Patches — Four Patches Applied

Compiled out-of-tree module from v6.17 kernel source with four patches:

1. **`format.c`** — fixed-rate quirk: bypasses `GET_RANGE` STALL with `set_fixed_rate(fp, 48000, SNDRV_PCM_RATE_48000)` for VID `0x2B5A` — **48 kHz only**; the device itself accepts all six rates via register `0x001A` (see Key Registers), but the quirk has not been taught the list
2. **`implicit.c`** — adds `IMPLICIT_FB_SKIP_DEV` flag to prevent EP 0x83 feedback endpoint conflict
3. **`endpoint.c`** — skips `endpoint_compatible()` check for UA VID, preventing "Incompatible EP setup" errors during stream open
4. **`quirks.c`** — `QUIRK_FLAG_IFACE_SKIP_CLOSE` for VID `0x2B5A`: prevents `snd-usb-audio` from resetting Interface 3 to alt=0 when PipeWire closes capture streams. Without this, stream close wipes the FPGA capture routing programmed by `usb-full-init.py`, causing subsequent captures to return silence

Build notes:
- Add `#include <linux/usb/audio-v2.h>` and `<linux/usb/audio-v3.h>` to mixer_maps.c
- Remove midi.o from objects (separate snd_usbmidi_lib module)
- Add `CFLAGS_mixer.o := -O1` and `CFLAGS_implicit.o := -O1` (compiler ICE at -O2)
- Strip signature for non-Secure-Boot systems

**Result**: ALSA enumerates playback (6ch S32_LE 48kHz) and capture (10ch S32_LE 48kHz).
PipeWire detects "Apollo Solo USB" with pro-audio profile.

## Audio Streaming — Root Cause Found
Isochronous transfers fail with `-EIO` on Linux. Captured working audio on Windows
(18MB pcap, `tools/usb-re/apollo-streaming.pcap`): 6ch play @ 1152B/pkt, 10ch rec @ 1920B/pkt.

**Required sequence before audio streams:**
1. DSP init via bulk (reg 0x23=1) — already working
2. **UAC2 SET_CUR sample rate = 48000 Hz** — `bmReqType=0x21 bReq=0x01 wVal=0x0100 wIdx=0x8001 data=0x0000BB80`
3. SET_INTERFACE(2, alt=1) — activate playback endpoint
4. SET_INTERFACE(3, alt=1) — activate capture endpoint

The key missing piece is step 2. The Thesycon driver on Windows explicitly sets the sample rate
before activating endpoints. On Linux, `snd-usb-audio` may skip this because GET_RANGE failed.

**Vendor status polling (between DSP init and audio):**
The Thesycon driver polls device info via vendor ctrl requests with `JK+*` and `JK<;` headers:
- `JK+*` responses contain: sample rate (0xBB80=48000), channel counts (10 rec, 6 play)
- `JK<;` responses contain: clock source names ("Internal", "S/PDIF", "ADAT")

**During audio streaming:** No bulk DSP commands — purely standard UAC2 isochronous.
EP 0x80 control transfers (160B every 20ms) carry metering/status data.

## Blocking: snd-usb-audio GET_RANGE
Device doesn't implement UAC 2.0 `GET_RANGE` for clock frequencies (STALLs).
`snd-usb-audio` requires this to enumerate sample rates → no PCM streams created.

**Fix needed**: Patch `sound/usb/clock.c` in `parse_audio_format_rates_v2v3()` to
fall back to hardcoded rates (44100, 48000, 88200, 96000, 176400, 192000) when
GET_RANGE fails for VID `0x2B5A`. The kernel already has a "predefined value"
fallback path but it's gated behind compile-time quirk entries, not `quirk_flags`.

Module `quirk_flags` parameter (including 0xFFFF) does NOT fix this.

## Mixer Settings Protocol (Vendor Control Requests)

**Decoded from `tools/usb-re/mixer-knobs.pcap` (2MB, manual UA Console capture)**

Unlike the Thunderbolt interface (which uses BAR0 register writes), the USB interface
uses **vendor control requests** for mixer settings:

```
bmRequestType = 0x41 (vendor, host-to-device)
bRequest      = 0x03 (settings write)
wValue        = FPGA address (see table)
wIndex        = 0x0000
data          = settings payload
```

### FPGA Address Map
| wValue | Size | Purpose |
|--------|------|---------|
| `0x0602` | 4B | Sequence counter (u32 LE, increment per batch) |
| `0x062D` | 128B | Mask+value buffer (which settings changed) |
| `0x064F` | 128B | Gain value buffer |
| `0x0670` | 48B | Extended settings |

### Batch Write Protocol
1. Write 128-byte mask buffer to `0x062D`
2. Optionally write 128-byte value buffer to `0x064F` (for gain changes)
3. Optionally write 48-byte extended buffer to `0x0670`
4. Write 4-byte sequence counter to `0x0602`

Same concept as Thunderbolt `MIXER_SEQ_WR` — cache all changes, single atomic bump.

### Settings Buffer Layout (128 bytes, 16 settings × 8 bytes each)
Each setting is 2 × u32 LE words (wordA + wordB):
```
word = (changed_mask[15:0] << 16) | value[15:0]
```
If mask = 0, the DSP ignores that setting.

| Setting | Offset | Register | Purpose |
|---------|--------|----------|---------|
| set[0] | +0 | wordA | **Preamp Ch0**: gain (mask=0xFF), 48V (bit3), Mic/Line (bit0) |
| set[1] | +8 | wordA | **Preamp Ch1**: same layout as set[0] |
| set[2] | +16 | wordA | **Monitor Level**: raw = 192 + (dB × 2), range 0x00–0xC0 |
| set[2] | +20 | wordB | **Monitor Mute/Mono**: bit1=mute (val 2/0), bit0=mono (val 1/0) |
| set[3] | +24 | wordA | **Gain C**: val_a + 0x41, mask=0x3F |

### Gain Encoding
```
val_a = max(0, min(54, gain_dB - 10))    → setting[ch].wordA, mask=0x00FF
val_c = val_a + 0x41                     → setting[3].wordA, mask=0x003F
```
Verified across full 10–65 dB range (19 data points, diff consistently = 0x41).

### Other Vendor Requests
| Request | Dir | Purpose |
|---------|-----|---------|
| 0x0C | OUT | Pre-commit trigger (wValue=0x0115) |
| 0x0D | OUT | Post-commit trigger (wValue=0x0010) |
| 0x0A | IN | JKMK register read (metering) |
| 0x10 | IN | Device info / status (160B, polled every ~20ms) |

### Apollo Solo USB Device Tree (via TCP:4710)
```
Inputs:  0=ANALOG 1, 1=ANALOG 2 (preamps, HiZ-capable)
         2-9=VIRTUAL 1-8
Outputs: 0=HP, 1=CUE 2, 2=CUE 3, 3=CUE 4, 4=MONITOR, 5=HP
```

## Next Steps
- [x] ~~Capture USB traffic~~ — 14MB pcap with full init sequence
- [x] ~~Decode DSP bulk protocol~~ — header + sub-command format identified
- [x] ~~Replay init sequence on Linux~~ — DSP responds, clock active
- [x] ~~Patch `snd-usb-audio`~~ — three patches: fixed-rate quirk, implicit FB skip, endpoint compat skip
- [x] ~~Build USB hardware backend~~ — `mixer-engine/hardware_usb.py`, reads/writes DSP state
- [x] ~~Create udev rule~~ — `configs/udev/99-apollo-usb.rules` + init scripts, auto-init on plug-in
- [x] ~~Playback verified~~ — 6ch S32_LE 48kHz confirmed on Intel Tiger Lake-H and AMD USB
- [x] ~~Decode mixer settings protocol~~ — vendor ctrl 0x03 with batch writes to FPGA addresses
- [x] ~~Map preamp gain, 48V, mic/line, monitor level/mute/mono~~ — from mixer-knobs.pcap
- [x] ~~Test mixer settings writes on Linux~~ — verified, capture routing confirmed with signal
- [x] ~~PipeWire integration~~ — `configs/pipewire/setup-apollo-solo-usb.sh`, Mic 1/2, Monitor, Headphone devices
- [x] ~~EP6 drain daemon~~ — no longer needed; one-shot `usb-full-init.py` stabilizes device; daemon removed from stack
- [x] ~~Fix capture through PipeWire~~ — resolved by `QUIRK_FLAG_IFACE_SKIP_CLOSE` patch (quirks.c) preventing Interface 3 reset on stream close
- [x] ~~Fix mixer settings lockup after `usb-full-init.py`~~ — resolved by seq counter fix in vendor request 0x03
- [ ] Fix firmware-version-specific SRAM crash at packet 28 (IIR biquad address) — active issue for CachyOS/AMD contributor (@ariahello, different Cauldron build)
- [ ] Map remaining settings: dim, pad, hiz, lowcut, phase, fader, pan, sends
- [ ] Submit kernel patches upstream (alsa-devel) — 4 patches ready
- [ ] Tag v1.4.0 once clean install from scratch is validated

## Test Environments

| System | Kernel | USB Controller | Status |
|--------|--------|---------------|--------|
| Ubuntu Studio 24.04, Intel Tiger Lake-H | 6.17.0-20-generic (gcc) | Intel xHCI | Full duplex confirmed — 6ch play + 10ch capture at 48kHz; PipeWire capture, Discord, hardware monitoring all working |
| CachyOS, AMD | 6.19.10-1-cachyos | AMD USB | Playback confirmed; `usb-full-init.py` crashes at packet 28 (firmware version mismatch — Cauldron build differs) |

- **Model**: Apollo Solo USB (USB-C edition)
- **Firmware Loader Serial**: `0000000004BE`
- **Firmware**: Cauldron v1.3 build 3 (Feb 18 2026)
- **Connection**: USB 3.0+ required (failed with USB 2.0 cable)
