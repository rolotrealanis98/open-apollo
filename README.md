# Open Apollo

**Open-source Linux driver for Universal Audio Apollo Thunderbolt interfaces**

Open Apollo brings full Linux support to Universal Audio's Apollo
Thunderbolt audio interfaces. Built through clean-room reverse engineering,
this community-driven project provides a native kernel driver, userspace
mixer daemon, and system tray indicator. Verified on the Apollo x4 with
clean playback and recording on Ubuntu 24.04 and Fedora 43.

## Current Status (Apollo x4)

- **Full duplex audio** — 24 playback + 22 capture channels via ALSA
- **All sample rates** — 44.1, 48, 88.2, 96, 176.4, 192 kHz
- **Preamp control** — gain, 48V phantom power, PAD, low cut, phase invert, mic/line switching
- **Monitor control** — volume, mute, dim, mono, headphone routing
- **DSP mixer** — input faders, pan, sends (AUX1/AUX2, CUE1/CUE2), solo, mute
- **PipeWire/ALSA** — 50 ALSA mixer controls, WirePlumber and UCM2 configs included; virtual I/O devices via `apollo-setup-io`
- **System tray indicator** — real-time status, one-click init, daemon control
- **One-command install** — `sudo bash scripts/install.sh` handles everything; tested 8/8 cycles on Ubuntu 24.04

**Not yet implemented:** talkback, virtual/monitor loopback, console UI, multi-device support

## Supported Devices

| Device | Status |
|--------|--------|
| Apollo x4 | **Verified** — playback, capture, preamps, gain, DSP settings |
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

**Power off the Apollo before running the installer** — DKMS auto-loading while the device is connected can hang the system.

```bash
git clone https://github.com/open-apollo/open-apollo.git
cd open-apollo
sudo bash scripts/install.sh
```

The installer handles dependencies, kernel module build, DKMS registration, IOMMU detection, and PipeWire/WirePlumber config deployment.

### Post-install: power on and configure I/O

After install completes:

1. Power on the Apollo and wait ~20 seconds for Thunderbolt enumeration
2. Run the init script (loads firmware, activates DSP, starts the daemon):
   ```bash
   sudo bash tools/apollo-init.sh
   ```
3. Set up virtual I/O devices (run once per session, or automate via udev/systemd):
   ```bash
   apollo-setup-io
   ```

The `apollo-setup-io` command discovers the Apollo's PCI address at runtime, sets the pro-audio PipeWire profile, and creates named virtual devices for each input and output group.

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

The daemon exposes TCP:4710 and TCP:4720 for mixer control from any client application.

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
