---
title: Codebase Summary
---

Open-source Linux driver and mixer daemon for Universal Audio Apollo Thunderbolt and USB audio interfaces. Built through clean-room reverse engineering of the macOS kext and Windows drivers.

**Current version:** 1.0.0 (Thunderbolt driver released 2026-03-22)

---

## Architecture

Two distinct hardware paths — Thunderbolt models use a kernel driver, USB models are pure userspace:

```
Thunderbolt path:
  Control clients (TCP:4710 / WS:4721)
       ↓
  ua_mixer_daemon.py (Python)
       ↓ ioctl
  ua_apollo.ko (Linux kernel module)
       ↓ PCIe MMIO (BAR0)
  Apollo FPGA + DSP + ARM MCU

USB path:
  Control clients (TCP:4710 / WS:4721)
       ↓
  ua_mixer_daemon.py + hardware_usb.py
       ↓ USB bulk (EP1) + vendor control (0x03)
  Cypress FX3 "Cauldron" firmware
       ↓ GPIF parallel bus
  Apollo FPGA + DSP
```

Audio streaming on USB uses a patched `snd-usb-audio` kernel module with a GET_RANGE quirk for VID `0x2B5A`. The patch is in `tools/usb-re/0001-ALSA-usb-audio-Add-quirk-for-Universal-Audio-USB-devices.patch`.

---

## Directory Structure

| Path | Language | Purpose |
|------|----------|---------|
| `driver/` | C | Linux PCIe kernel module (`ua_apollo.ko`) |
| `mixer-engine/` | Python | TCP:4710 + TCP:4720 + WS:4721 daemon |
| `console-web/` | Svelte/JS | Browser-based mixer UI |
| `configs/` | Shell/conf | PipeWire, WirePlumber, UCM2, udev configs |
| `scripts/` | Bash | Installer, uninstaller, USB init scripts |
| `tools/` | Python/Bash | Tray indicator, USB RE tools, diagnostics |
| `usb-firmware/` | Binary | FX3 firmware blobs (ApolloSolo.bin, etc.) |
| `nix/` | Nix | NixOS module for declarative installation |
| `tests/docker/` | Dockerfile | Install matrix test containers (11 distros) |
| `docs/` | Markdown | Documentation (Markdoc, subdirectory/page.md structure) |

---

## Kernel Driver (`driver/`)

Four source files compiled into `ua_apollo.ko`:

| File | Lines | Role |
|------|-------|------|
| `ua_core.c` | 2,954 | PCIe probe, device detection, chardev `/dev/ua_apollo0`, ioctls, DSP settings batch writes, monitor routing |
| `ua_audio.c` | 4,297 | ALSA PCM (playback + capture), mixer controls (50+ on x4), DMA ring buffers, transport, clock |
| `ua_dsp.c` | 3,027 | DSP command ring buffer, firmware loading via `request_firmware()`, ACEFACE connect, routing tables |
| `ua_apollo.h` | — | Register map, device type constants, hardware structs |
| `ua_routing.h` | — | Static RT171 routing tables (22 rec + 24 play channels) |

**Kernel requirement:** Linux 6.8+ (uses `linux/hrtimer_types.h` introduced in 6.8)

**Compiler support:** GCC (with `-Og` workaround for GCC 13.3 ICE on `ua_core.c` and `ua_audio.c`) and Clang (no workaround needed, conditional on `CONFIG_CC_IS_CLANG`)

**Hardware write paths:**

| Path | Mechanism | Used for |
|------|-----------|---------|
| DSP settings (SRAM) | BAR0 write at `0x3800+` | Monitor vol/mute/dim, preamp flags |
| DSP ring buffer | Command queue | Mixer bus coefficients, routing, module activation |
| ARM CLI (`0xC3F4`) | Text command to ARM MCU | Preamp gain (PGA2500 SPI), identify LED |
| Clock register | BAR0 write | Sample rate changes |

**Device detection:** Thunderbolt models identified by PCI subsystem ID (more reliable than serial prefix for some models). Supported types range from `0x0A` (Apollo 8P original) through `0x3A` (Twin X Gen 2).

---

## Mixer Daemon (`mixer-engine/`)

| File | Lines | Role |
|------|-------|------|
| `ua_mixer_daemon.py` | 1,662 | Main daemon: async TCP + WS servers, protocol dispatch |
| `hardware.py` | 2,096 | Thunderbolt hardware backend: ioctl wrapper, DSP settings, preamp, bus params |
| `hardware_usb.py` | 381 | USB hardware backend (staged): pyusb, vendor ctrl 0x03 batch writes |
| `state_tree.py` | 539 | Hierarchical control tree (11,244 nodes for Apollo x4) |
| `helper_tree.py` | — | Protocol helper tree (UA Mixer Helper protocol) |
| `protocol.py` | — | TCP:4710 framing and command parsing |
| `ws_server.py` | — | WebSocket server (requires `websockets` package) |
| `metering.py` | — | Audio level metering from PCM samples |
| `bonjour.py` | — | Bonjour/mDNS announcement (via `avahi-publish`) |
| `ubjson_codec.py` | — | UBJSON encoder for TCP:4720 responses |

**Protocol details:**
- TCP:4710 — null-terminated JSON text (UA Mixer Engine / ConsoleLink protocol)
- TCP:4720 — text commands in, UBJSON binary responses out (UA Mixer Helper / Console protocol)
- WS:4721 — WebSocket for UA Connect and console-web

**Mixer settings batch protocol (Thunderbolt):**
All settings are cached; a single `MIXER_SEQ_WR` bump signals the DSP. Per-setting writes without the sequence handshake crash the DSP. Setting index 2 is shared (Monitor Level/Mute/Dim/Mono) — read-modify-write with per-field bitmasks.

**Mixer settings batch protocol (USB):**
Same concept via vendor control request 0x03: write mask buffer → value buffer → sequence counter to FPGA addresses `0x062D`/`0x064F`/`0x0602`.

---

## Console Web UI (`console-web/`)

Svelte 5 application, bundled with Vite. Components in `src/lib/`:

| Component | Purpose |
|-----------|---------|
| `ChannelStrip.svelte` | Full channel strip (fader + meter + preamp) |
| `PreampSection.svelte` | Gain, 48V, PAD, HiZ, phase, low cut controls |
| `Fader.svelte` | Level fader with dB display |
| `Meter.svelte` / `StereoMeter.svelte` | VU/peak level meters |
| `MonitorSection.svelte` | Monitor volume, mute, dim, mono |
| `SettingsPage.svelte` | Settings/config page |
| `ws-client.js` | WebSocket client connecting to mixer daemon WS:4721 |
| `device-store.svelte.js` | Svelte store for device state |

Build: `npm run build` in `console-web/`. The built `dist/` is committed for serving without a build step.

---

## USB Stack

**Boot sequence (every power-on):**
1. Apollo presents as VID `0x2B5A`, PID `0x000C` (stub/loader)
2. `ua-usb-init.sh` runs automatically via udev rule `99-apollo-usb.rules`
3. `tools/fx3-load.py` uploads firmware from `/lib/firmware/universal-audio/ApolloSolo.bin`
4. FX3 re-enumerates as PID `0x000D` (USB 3.0 SuperSpeed, 4 interfaces)
5. `ua-usb-dsp-init.sh` runs DSP bulk init (reg `0x23=1`) and SET_CUR sample rate

**Audio streaming:** Standard UAC 2.0 isochronous via patched `snd-usb-audio`. Channels: 6ch play + 10ch rec at 48 kHz (Alt Setting 1).

**Firmware files** (in `usb-firmware/`):

| File | Device |
|------|--------|
| `ApolloSolo.bin` | Apollo Solo USB |
| `ApolloTwin.bin` | Twin USB |
| `ApolloTwinX.bin` | Twin X USB |
| `Satellite.bin` | Satellite USB |

These are the Cypress FX3 "Cauldron" firmware images (ARM32, FX3 SDK). Sourced from UA firmware page or UA Connect on Windows.

---

## Configuration Files (`configs/`)

| Path | Purpose |
|------|---------|
| `wireplumber/51-ua-apollo.conf` | Disable MMAP, no suspend, S32LE/48kHz |
| `wireplumber/51-ua-apollo.lua` | WirePlumber 0.4 (Lua) equivalent |
| `ucm2/ua_apollo/` | ALSA UCM2 profile (HiFi mode) |
| `udev/91-ua-apollo.rules` | Thunderbolt device detection + profile setup |
| `udev/99-apollo-usb.rules` | USB device detection + auto-init trigger |
| `pipewire/setup-apollo-io.sh` | Dynamic PipeWire virtual I/O map (Thunderbolt) |
| `pipewire/setup-apollo-solo-usb.sh` | PipeWire virtual I/O (USB) |
| `autostart/open-apollo-tray.desktop` | Desktop autostart for tray indicator |
| `deploy.sh` | Deploys all configs to system directories |

---

## Installation

**Standard (Thunderbolt):**
```bash
sudo bash scripts/install.sh
```

**USB Apollo:**
```bash
sudo bash scripts/install-usb.sh
```

**NixOS:**
```nix
imports = [ inputs.open-apollo.nixosModules.default ];
hardware.ua-apollo.enable = true;
```

**Install matrix validation** — 11 Docker containers in `tests/docker/` covering Ubuntu 24.04/22.04/20.04, Fedora 40/41/42, Arch, Debian trixie/bookworm/bullseye, and openSUSE Tumbleweed:
```bash
bash tests/test-install-matrix.sh
```

**Kernel version requirement:** 6.8+ (Thunderbolt driver). CachyOS, Manjaro, and Arch rolling kernels are supported.

---

## Supported Devices

### Thunderbolt

| Model | Device Type | Play ch | Rec ch | Status |
|-------|-------------|---------|--------|--------|
| Apollo 8P (original) | `0x0A` | 26 | 26 | Needs Testing |
| Apollo x4 | `0x1F` | 24 | 22 | **Partially Verified** |
| Apollo x6 / Gen 2 | `0x1E` / `0x35` | 24 | 22 | Needs Testing |
| Apollo x8 / Gen 2 | `0x22` / `0x37` | 26 | 26 | Needs Testing |
| Apollo x8p / Gen 2 | `0x20` / `0x38` | 26 | 26 | Needs Testing |
| Apollo x16 / Gen 2 | `0x21` / `0x39` | 34 | 34 | Needs Testing |
| Apollo x16D | `0x2A` | 34 | 34 | Needs Testing |
| Apollo Twin X / Gen 2 | `0x23` / `0x3A` | 8 | 8 | Needs Testing |
| Apollo Solo (TB) | `0x27` | 3 | 2 | Needs Testing |
| Arrow | `0x28` | 3 | 2 | Needs Testing |

### USB

| Model | PID (live) | Play ch | Rec ch | Status |
|-------|-----------|---------|--------|--------|
| Apollo Solo USB | `0x000D` | 6 | 10 | **Verified** |
| Apollo Twin USB | `0x0002` | — | — | Needs Testing |
| Apollo Twin X USB | `0x000F` | — | — | Needs Testing |

---

## Encoding Conventions

| Value | Encoding |
|-------|----------|
| Volume (dB) | `raw = 192 + (dB * 2)`, range `0x00`–`0xC0` |
| HW readback | `rb_data[0]` uses 6-bit stride per channel |
| DSP settings word | `wordA = (mask[15:0] << 16) | value[15:0]` |
| USB gain | `val_a = max(0, min(54, gain_dB - 10))` |
| Quantum | 512 (stable default — 256 crashes, 1024 causes pitch drift) |

---

## Safety Rules

These rules encode hard-won reverse engineering lessons — violating them causes hardware damage or data loss:

- **Never write mixer settings with `mask=1`** in driver connect — corrupts monitor
- **Never send `SRAM_CFG` to DSP SRAM** — firmware populates during cold boot; zeroing breaks capture routing
- **Never `rmmod ua_apollo`** while device is connected — kills Thunderbolt link, requires cold boot
- **DSP setting[2] is shared** — Monitor Level/Mute/Dim/Mono all use setting index 2; read-modify-write with per-field bitmasks
- **Mixer settings must be written in batches** — per-setting writes crash the DSP
- **Clock write uses source `0x0C`** (not `0x00`) — required for FPGA ACK and DSP active processing
- **USB requires USB 3.0** — FX3 re-enumerates at SuperSpeed; USB 2.0 cables/ports fail

---

## Key Links

- [Architecture Overview](/docs/architecture-overview)
- [Installation](/docs/installation)
- [Supported Devices](/docs/supported-devices)
- [Register Map](/docs/register-map)
- [DSP Protocol](/docs/dsp-protocol)
- [USB RE Findings](/docs/usb-apollo-re)
- [Troubleshooting](/docs/troubleshooting)
- [How to Contribute](/docs/how-to-contribute)
