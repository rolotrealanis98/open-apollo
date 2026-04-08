[![Release](https://img.shields.io/github/v/release/rolotrealanis98/open-apollo?style=flat-square)](https://github.com/rolotrealanis98/open-apollo/releases)
[![License: GPL v2](https://img.shields.io/badge/License-GPL_v2-blue.svg?style=flat-square)](https://www.gnu.org/licenses/old-licenses/gpl-2.0.en.html)
[![Ubuntu 24.04](https://img.shields.io/badge/Ubuntu-24.04-E95420?style=flat-square&logo=ubuntu&logoColor=white)](https://github.com/rolotrealanis98/open-apollo/wiki/Hardware-Compatibility)
[![Docs](https://img.shields.io/badge/docs-open--apollo-blue?style=flat-square)](https://open-apollo-docs.pages.dev)
[![Issues](https://img.shields.io/github/issues/rolotrealanis98/open-apollo?style=flat-square)](https://github.com/rolotrealanis98/open-apollo/issues)

# Open Apollo

**Open-source Linux driver and tools for Universal Audio Apollo Thunderbolt and USB interfaces**

Open Apollo brings Linux support to Universal Audio's Apollo audio interfaces.
Built through clean-room reverse engineering, this community-driven project
provides a native kernel driver for Thunderbolt models and a pure-userspace
stack for USB models, along with a mixer daemon and system tray indicator.

> **⚠️ Experimental** — This project is under active development and not yet ready
> for production use. Expect rough edges, kernel stability issues on some
> configurations, and incomplete features. We're shipping early to get community
> feedback and testing across different Apollo models.

## What Works (Apollo x4 — Thunderbolt, Ubuntu 24.04)

- **Full duplex audio** — 4 analog inputs + 6 analog outputs verified (24/22 total ALSA channels exposed; S/PDIF, ADAT, and virtual channels are unverified)
- **All sample rates** — 44.1, 48, 88.2, 96, 176.4, 192 kHz
- **Preamp control** — gain, 48V phantom power, PAD, low cut, phase invert, mic/line switching
- **Monitor control** — volume, mute, dim, mono, talkback, headphone routing
- **DSP mixer** — input faders, pan, sends (AUX1/AUX2, CUE1/CUE2), solo, mute
- **PipeWire virtual I/O** — named Mic 1-4, Line In 3+4, Monitor L/R, Line Out devices
- **Desktop audio** — YouTube, system sounds, GNOME volume control all work through Apollo Monitor
- **WebRTC capture** — clean audio in Chromium-based browsers (webcam mic tests, Discord web)
- **pw-record/pw-play** — zero-dropout capture and playback verified
- **Guided installer** — `sudo bash scripts/install.sh` with hardware validation gates + audio test
- **System tray indicator** — real-time hardware status

## What Works (Apollo Solo USB)

- **6ch playback** — S32_LE 48kHz, confirmed on Ubuntu Studio 24.04 (Intel) and CachyOS (AMD)
- **10ch capture** — requires full DSP program load (`usb-full-init.py`); firmware-version-specific
- **Preamp control** — gain, 48V phantom power, mic/line switching via vendor control
- **Monitor control** — level, mute, mono
- **PipeWire playback** — browser audio, system audio, DAWs all work
- **EP6 drain daemon** — prevents Intel xHCI buffer overruns from FPGA notification packets
- **Patched snd-usb-audio** — three out-of-tree patches (fixed-rate quirk, implicit feedback skip, endpoint compat bypass)
- **Automatic init via udev** — firmware upload + DSP init on device plug-in

### USB Known Issues

| Issue | Status | Notes |
|-------|--------|-------|
| **Capture requires full DSP init** | Partial | `usb-full-init.py` needed; init sequence is firmware-version-specific |
| **Intel xHCI EP6 flood** | Fixed | EP6 drain daemon prevents buffer overruns on Intel USB controllers |
| **PipeWire capture zeros** | Open | Raw ALSA capture works but PipeWire capture returns zeros; channel mapping under investigation |
| **Full init crash at packet 28** | Known | Some firmware builds crash on IIR biquad SRAM write (address mismatch); provide a fresh Windows capture if affected |
| **DSP init ordering** | Documented | Module must load before daemon starts; SET_INTERFACE wipes FPGA routing |

{% callout type="note" %}
USB support uses `sudo bash scripts/install-usb.sh`. See [USB Quick Start](#usb-quick-start-apollo-solo-usb) below.
{% /callout %}

## Known Issues

| Issue | Severity | Workaround |
|-------|----------|------------|
| **Firefox Snap crashes** — kernel `hrtimer` lockup when using WebRTC capture | Critical | Use Chromium or Firefox .deb instead of Snap |
| **Raw pro-audio node crash** — apps selecting "Apollo x4 Pro" (22ch) as input can hang the system | Critical | Default source is set to Mic 1; avoid selecting the raw multichannel device |
| **WebRTC capture pitch drift** — brief pitch wobble (~1-3s) when browser capture starts; stabilizes automatically. This is normal PipeWire DLL lock-in behavior affecting all pro-audio interfaces through the PulseAudio bridge. | Minor | Normal — wait for it to stabilize. Native PipeWire apps (`pw-record`, Ardour, OBS) have zero drift. |
| **Apollo hot-unplug** — powering off Apollo while driver is loaded | Fixed | Shutdown guards prevent crash; system stays stable ([#22](https://github.com/rolotrealanis98/open-apollo/issues/22)) |
| **Ardour JACK routing** — Ardour via JACK can steal ALSA device from PipeWire | Moderate | Launch with `pw-jack ardour7`; restart PipeWire if audio stops |
| **Audacity ALSA-only** — Ubuntu's Audacity package lacks PipeWire backend | Minor | Install Audacity via Flatpak for PipeWire support |
| **GNOME Sound Settings** — input level meter may not show activity | Cosmetic | Audio works; meter is a GNOME UI limitation with pro-audio devices |
| **Thunderbolt link instability** — fresh boot with Apollo on may need power cycle | Moderate | Install script guides through power cycle; `apollo-setup-io` recovers |

## App Compatibility

| App | Status | Notes |
|-----|--------|-------|
| **Chromium / Chrome** | Verified | Recommended browser for WebRTC capture |
| **Firefox .deb** | Untested | Should work; Snap version crashes (see Known Issues) |
| **Discord .deb** | Untested | Expected to work via PulseAudio bridge |
| **OBS Studio** | Untested | Should work natively with PipeWire |
| **Ardour** | Partial | Use `pw-jack ardour7`; don't let it grab ALSA directly |
| **Audacity** | Partial | Flatpak version only (Ubuntu .deb lacks PipeWire backend) |
| **pw-record / pw-play** | Verified | Zero-drift, cleanest capture path |
| **REAPER** | Untested | Expected to work via `pw-jack` |

**Recommendation:** Use Chromium-based browsers for WebRTC. Use native PipeWire apps (`pw-record`, Ardour, OBS) for studio-quality recording with zero clock drift.

## Not Yet Implemented

Virtual/monitor loopback, console UI, multi-device support, plugin chain (UAD plugins require PACE licensing — not planned)

## Supported Devices

### Thunderbolt Models

| Device | Status |
|--------|--------|
| Apollo x4 | **Partially Verified** — analog I/O (Mic 1-4, Monitor, Line Out), preamps, gain, DSP settings. S/PDIF, ADAT, virtual channels untested. |
| Apollo x4 Gen 2 | Needs Testing |
| Apollo x6 / Gen 2 | Needs Testing |
| Apollo x8 / Gen 2 | Needs Testing |
| Apollo x8p / Gen 2 | Needs Testing |
| Apollo x16 / Gen 2 | Needs Testing |
| Apollo x16D | Needs Testing |
| Apollo Twin X / Gen 2 | Needs Testing |
| Apollo Solo (Thunderbolt) | Needs Testing |
| Apollo 8P (original) | Needs Testing |
| Arrow | Needs Testing |

All Thunderbolt Apollo models share the same register map and protocol — the driver
includes device detection and channel configuration for every model listed above.
We need community testers to verify each one.

### USB Models

| Device | Status |
|--------|--------|
| Apollo Solo USB | **Verified** — USB, playback + capture, preamp control, PipeWire integration |
| Apollo Twin USB | Needs Testing |
| Apollo Twin X USB | Needs Testing |

### Thunderbolt 2 Devices (Not Supported)

Older Apollo models with **Thunderbolt 2** connections (Apollo Twin, Apollo 8, Apollo 16, Apollo Duo, Apollo Quad) are **not expected to work**, even with an Apple Thunderbolt 2 → 3 adapter.

The issue is not the driver — it's that **Linux does not enumerate Thunderbolt 2 PCIe devices** in most configurations. The Apollo will not appear in `lspci` output, so the driver has nothing to bind to. This has been confirmed by a community member testing an Apollo Twin (TB2) with an Apple adapter on a Linux laptop with Thunderbolt 3 ports — the install completed successfully but the device was never detected.

If your Apollo uses a Thunderbolt 2 connector (Mini DisplayPort-shaped, not USB-C), you will likely hit this limitation. This project targets **Thunderbolt 3/4 Apollo models** (USB-C connector) on systems with native Thunderbolt 3 or 4 ports.

## Quick Start

### Prerequisites

- Ubuntu 24.04+, Fedora 40+, Arch, openSUSE Tumbleweed, Linux Mint 22, Pop!_OS 24.04, Manjaro
- Linux kernel 6.8+ with headers installed (required — uses `hrtimer_types.h` introduced in 6.8)
- Thunderbolt 3 or 4 connection (see [Thunderbolt 2 note](#thunderbolt-2-devices-not-supported) below)
- GCC, Make
- Python 3.10+ (for mixer daemon)
- `iommu=pt` kernel parameter required on most systems (see below)

### Install

```bash
git clone https://github.com/rolotrealanis98/open-apollo.git
cd open-apollo
sudo bash scripts/install.sh
```

The installer is a guided walkthrough that:
1. Installs dependencies, builds the driver, sets up DKMS
2. Deploys PipeWire/WirePlumber/UCM2 configs
3. Guides you through an Apollo power cycle for clean Thunderbolt initialization
4. Loads the driver, verifies DSP handshake, sets up virtual I/O devices
5. Starts the mixer daemon and tray indicator
6. Plays a test tone so you can verify audio output

### NixOS (Thunderbolt)

Add to your `flake.nix` inputs:

```nix
inputs.open-apollo.url = "github:rolotrealanis98/open-apollo";
```

Then in your `configuration.nix`:

```nix
imports = [ inputs.open-apollo.nixosModules.default ];
hardware.ua-apollo.enable = true;
```

Run `nixos-rebuild switch` — the module handles the kernel module build, `iommu=pt`, Thunderbolt (bolt), PipeWire, and required packages.

### Uninstall

```bash
sudo bash scripts/uninstall.sh
```

Removes driver, DKMS, all configs, services, and guides you through Apollo power-off for clean module unload.

### Troubleshooting

If audio stops working after using a JACK app (Ardour, etc):
```bash
systemctl --user restart pipewire wireplumber pipewire-pulse
apollo-setup-io
```

### Virtual I/O devices (Apollo x4)

After running `apollo-setup-io`, these devices are available in PipeWire:

| Device | Type | Channels |
|--------|------|----------|
| Apollo Mic 1 | Source (capture) | Mono |
| Apollo Mic 2 | Source (capture) | Mono |
| Apollo Mic 1+2 | Source (capture) | Stereo |
| Apollo Mic 3 | Source (capture) | Mono |
| Apollo Mic 4 | Source (capture) | Mono |
| Apollo Line In 3+4 | Source (capture) | Stereo |
| Apollo Monitor L/R | Sink (playback) | Stereo |
| Apollo Line Out 1+2 | Sink (playback) | Stereo |
| Apollo Line Out 3+4 | Sink (playback) | Stereo |

### USB Quick Start (Apollo Solo USB)

```bash
git clone https://github.com/rolotrealanis98/open-apollo.git
cd open-apollo
sudo bash scripts/install-usb.sh
```

The installer handles dependencies, firmware setup, kernel module build, DSP initialization, and PipeWire configuration. You'll need the Apollo firmware file from UA's website — the installer will prompt you if it's missing.

After install, the following PipeWire devices are available:

| Device | Type | Channels |
|--------|------|----------|
| Apollo Solo USB Mic 1 | Source (capture) | Mono |
| Apollo Solo USB Mic 2 | Source (capture) | Mono |
| Apollo Solo USB Mic 1+2 | Source (capture) | Stereo |
| Apollo Solo USB Monitor | Sink (playback) | Stereo |
| Apollo Solo USB Headphone | Sink (playback) | Stereo |

### Start the Mixer Daemon

```bash
cd mixer-engine
python3 ua_mixer_daemon.py -v
```

The daemon exposes TCP:4710, TCP:4720, and WS:4721 for mixer control from any client application.

## Architecture

```
┌──────────────┐   ┌──────────────┐
│ Control App  │   │ Control App  │
│ (TCP:4710)   │   │ (TCP:4720)   │
└──────┬───────┘   └──────┬───────┘
       │                  │
       └────────┬─────────┘
                │
       ┌────────▼────────┐
       │  Mixer Daemon   │
       │  (Python)       │
       └────────┬────────┘
                │ ioctl
       ┌────────▼────────┐
       │  Kernel Driver  │
       │  (ua_apollo.ko) │
       └────────┬────────┘
                │ MMIO
       ┌────────▼────────┐
       │  Apollo Hardware │
       │  (BAR0 regs)    │
       └─────────────────┘
```

## Components

| Component | Path | Description |
|-----------|------|-------------|
| **Driver** | `driver/` | Linux PCIe kernel module — DMA, ALSA, DSP ring buffer, preamp control |
| **Mixer Daemon** | `mixer-engine/` | TCP:4710 + TCP:4720 + WS:4721 daemon — state tree, hardware routing, metering |
| **Configs** | `configs/` | PipeWire, WirePlumber, and UCM2 configuration profiles |
| **Tools** | `tools/contribute/` | Device probe and capture scripts for community contributions |

## Documentation

Full documentation at **[open-apollo-docs.pages.dev](https://open-apollo-docs.pages.dev/)**, including:

- [Installation guide](https://open-apollo-docs.pages.dev/docs/installation) — build, install, configure
- [Supported devices](https://open-apollo-docs.pages.dev/docs/supported-devices) — model compatibility table
- [Architecture overview](https://open-apollo-docs.pages.dev/docs/architecture-overview) — how the pieces fit together
- [Register map](https://open-apollo-docs.pages.dev/docs/register-map) — BAR0 hardware register documentation
- [DSP protocol](https://open-apollo-docs.pages.dev/docs/dsp-protocol) — ring buffer commands and settings batch protocol
- [How to contribute](https://open-apollo-docs.pages.dev/docs/how-to-contribute) — testing, device captures, code contributions

Documentation content lives in the [`docs/`](docs/) directory of this repo. Edit the
Markdown files there and submit a PR — changes are automatically deployed to the
docs site.

## Contributing

We welcome contributions of all kinds — from device testing reports to driver
code. See [CONTRIBUTING.md](CONTRIBUTING.md) for details.

**The most impactful thing you can do right now is test on your hardware.** If you
own any Apollo model besides the x4, your testing report directly enables support
for that device.

### Quick contribution path

1. Build and load the driver
2. Run `./tools/contribute/device-probe.sh`
3. [Submit a device report](https://github.com/open-apollo/open-apollo/issues/new?template=device-report.yml)

## License

This project is licensed under the [GNU General Public License v2.0](LICENSE).

See [NOTICE.md](NOTICE.md) for information about trademarks and intellectual
property.
