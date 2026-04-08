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
- [x] `snd-usb-audio` claims device with three patches applied (see below)
- [x] Clock source responds at 48kHz after DSP init
- [x] **6ch ALSA playback** (S32_LE 48kHz) — confirmed on Ubuntu Studio 24.04 (kernel 6.17, Intel Tiger Lake-H) and CachyOS (kernel 6.19, AMD USB)
- [x] PipeWire playback — browser audio, system audio working
- [x] DSP init — FPGA activation, CONFIG_A/B, identity routing table, clock set, monitor level (`tools/usb-dsp-init.py`)
- [x] EP6 drain daemon — prevents Intel xHCI buffer overruns from JFK notification packets
- [x] Mixer settings via vendor control 0x03 — preamp gain, 48V, monitor level/mute

### Partially Working
- [~] **10ch ALSA capture** — works with full DSP program load (`tools/usb-full-init.py`), but the init sequence is firmware-version-specific. Some firmware builds crash at packet 28 (IIR biquad writes to a moved SRAM address)
- [~] **Hardware monitoring** (mic → headphones) — works when PipeWire is stopped
- [~] **Mixer settings** — work via vendor control 0x03 but may stop responding after full DSP init (`usb-full-init.py`)

### Known Issues
- **SET_INTERFACE resets FPGA state** — when `snd-usb-audio` opens/closes streams, SET_INTERFACE can wipe the routing table. DSP init must run AFTER module probe, not before
- **EP6 interrupt flood on Intel xHCI** — the Apollo pushes JFK notifications at ~2000/sec. Intel xHCI controllers overflow; AMD handles it gracefully. Fix: EP6 drain daemon in `tools/usb-dsp-init.py`
- **Interface 0 contention** — the DSP init daemon and `snd-usb-audio` can conflict on Interface 0 (vendor DSP). Correct ordering: load patched module first, then start daemon
- **Firmware-specific init sequence** — the captured 38-packet init from Cauldron 1.3 build 3 works on some devices but crashes others at packet 28 (SRAM address mismatch in IIR biquad writes)
- **Capture through PipeWire returns zeros** — raw ALSA capture works but PipeWire returns silence. Channel mapping or stream-open interaction needs investigation

### Required Init Order
1. Upload firmware (`tools/fx3-load.py` or udev auto-trigger)
2. Load patched `snd-usb-audio` module
3. Start `tools/usb-dsp-init.py --daemon` (or `tools/usb-full-init.py` for capture)
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
Header (4 bytes):
  [word_count : u16 LE]  — number of 32-bit words in payload
  [type       : u8]      — command/response sub-type
  [magic      : u8]      — 0xDC = command (OUT), 0xDD = response (IN)

Payload (word_count * 4 bytes):
  Repeated sub-commands, each 8 bytes:
  [opcode : u16 LE]  — operation code (0x0001 = query, 0x0002 = read/write)
  [param  : u16 LE]  — register/parameter index
  [value  : u32 LE]  — value (write) or 0 (read)
```

### Observed Init Sequence
```
# Frame 77 — Host sends init command (type=0)
OUT: 04 00 00 DC  02 00 23 00 01 00 00 00  02 00 10 00 48 14 B7 01
     ^header      ^write reg 0x23 = 1       ^write reg 0x10 = 0x01B71448

# Frame 78 — Host sends readback request (type=1)
OUT: 04 00 01 DC  02 00 10 00 48 14 B7 01  02 00 11 00 20 24 E5 0A
     ^header      ^reg 0x10                 ^reg 0x11

# Frames 81-94 — Device returns full register dump (3,116 bytes total)
IN:  26 00 00 DD  02 03 03 80 20 24 E5 0A  00 00 00 81 ...  (38 words)
IN:  64 00 01 DD  ...  (100 words)
IN:  75 00 02 DD  ...  (117 words)
...continues across 8 response packets...

# Frame 309 — Host queries status (type=1)
OUT: 01 00 01 DC  01 00 27 00
     ^1 word      ^opcode=1 param=0x27

# Frame 311 — Device responds with status
IN:  02 00 08 DD  02 00 0D 80  42 01 00 00
     ^2 words     ^param=0x800D  value=0x142 (322)
```

### Key Registers
| Param | Direction | Purpose |
|-------|-----------|---------|
| 0x0023 | Write | Init/connect (value=1 to activate, 0 to query) |
| 0x0010 | Read/Write | Config word A (0x01B71448) |
| 0x0011 | Read/Write | Config word B (0x0AE52420) |
| 0x0027 | Query | Status request (opcode 0x01) |
| 0x800D | Response | Status response (value 0x142) |

### Register Dump Structure
The init response returns ~780 32-bit words across 8 bulk packets, containing
the full mixer/DSP state. Values are mostly `0x80000000` (muted/default) and
`0x83000000`, similar to the Thunderbolt mixer settings format.

## Linux DSP Init — Working
Replay of Windows init sequence via `tools/usb-dsp-init.py` **works**:
- Init command (reg 0x23=1) accepted
- Readback returns 476-500B register dumps across 8 packets
- Status query returns 388B state
- Clock reads 48000 Hz after init

## snd-usb-audio Kernel Patches — Three Patches Applied

Compiled out-of-tree module from v6.17 kernel source with three patches:

1. **`format.c`** — fixed-rate quirk: bypasses `GET_RANGE` STALL by falling back to hardcoded rates (44100, 48000, 88200, 96000, 176400, 192000 Hz) for VID `0x2B5A`
2. **`implicit.c`** — adds `IMPLICIT_FB_SKIP_DEV` flag to prevent EP 0x83 feedback endpoint conflict
3. **`endpoint.c`** — skips `endpoint_compatible()` check for UA VID, preventing "Incompatible EP setup" errors during stream open

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
- [x] ~~EP6 drain daemon~~ — prevents Intel xHCI JFK notification flood
- [ ] Fix firmware-version-specific SRAM crash at packet 28 (IIR biquad address)
- [ ] Fix capture through PipeWire (returns zeros — channel mapping or stream-open interaction)
- [ ] Map remaining settings: dim, pad, hiz, lowcut, phase, fader, pan, sends
- [ ] Submit kernel patches upstream (alsa-devel) — 3 patches ready
- [ ] Investigate mixer settings lockup after `usb-full-init.py`

## Test Environments

| System | Kernel | USB Controller | Status |
|--------|--------|---------------|--------|
| Ubuntu Studio 24.04, Intel Tiger Lake-H | 6.17.0-20-generic | Intel xHCI | Playback + capture confirmed |
| CachyOS, AMD | 6.19.10-1-cachyos | AMD USB | Playback confirmed; capture needs `usb-full-init.py` |

- **Model**: Apollo Solo USB (USB-C edition)
- **Firmware Loader Serial**: `0000000004BE`
- **Firmware**: Cauldron v1.3 build 3 (Feb 18 2026)
- **Connection**: USB 3.0+ required (failed with USB 2.0 cable)
