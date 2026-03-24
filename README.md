[![Release](https://img.shields.io/github/v/release/rolotrealanis98/open-apollo?style=flat-square)](https://github.com/rolotrealanis98/open-apollo/releases)
[![License: GPL v2](https://img.shields.io/badge/License-GPL_v2-blue.svg?style=flat-square)](https://www.gnu.org/licenses/old-licenses/gpl-2.0.en.html)
[![Ubuntu 24.04](https://img.shields.io/badge/Ubuntu-24.04-E95420?style=flat-square&logo=ubuntu&logoColor=white)](https://github.com/rolotrealanis98/open-apollo/wiki/Hardware-Compatibility)
[![Docs](https://img.shields.io/badge/docs-open--apollo-blue?style=flat-square)](https://open-apollo-docs.pages.dev)
[![Issues](https://img.shields.io/github/issues/rolotrealanis98/open-apollo?style=flat-square)](https://github.com/rolotrealanis98/open-apollo/issues)

# Open Apollo

**Open-source Linux driver for Universal Audio Apollo Thunderbolt interfaces**

Open Apollo brings Linux support to Universal Audio's Apollo
Thunderbolt audio interfaces. Built through clean-room reverse engineering,
this community-driven project provides a native kernel driver, userspace
mixer daemon, and system tray indicator.

> **⚠️ Experimental** — This project is under active development and not yet ready
> for production use. Expect rough edges, kernel stability issues on some
> configurations, and incomplete features. We're shipping early to get community
> feedback and testing across different Apollo models.

## What Works (Apollo x4 Analog I/O, Ubuntu 24.04)

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
| Apollo Solo | Needs Testing |
| Arrow | Needs Testing |

All Thunderbolt Apollo models share the same register map and protocol — the driver
includes device detection and channel configuration for every model listed above.
We need community testers to verify each one.

## Quick Start

### Prerequisites

- Ubuntu 24.04 LTS (primary tested platform) or Fedora 43 / Arch Linux
- Linux kernel 6.1+ with headers installed
- Thunderbolt 3 or 4 connection
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
