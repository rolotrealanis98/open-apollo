# Open Apollo

**Open-source Linux driver for Universal Audio Apollo Thunderbolt interfaces**

Open Apollo is working toward Linux support for Universal Audio's Apollo
Thunderbolt audio interfaces. Built through clean-room reverse engineering,
this community-driven project is developing a native kernel driver and
userspace mixer daemon. Currently verified on the Apollo x4 with playback,
recording, preamp control, and DSP settings — additional devices and
features are in active development.

## Current Status (Apollo x4)

- **Full duplex audio** — 24 playback + 22 capture channels via ALSA
- **All sample rates** — 44.1, 48, 88.2, 96, 176.4, 192 kHz
- **Preamp control** — gain, 48V phantom power, PAD, low cut, phase invert, mic/line switching
- **Monitor control** — volume, mute, dim, mono, headphone routing
- **DSP mixer** — input faders, pan, sends (AUX1/AUX2, CUE1/CUE2), solo, mute
- **PipeWire/ALSA** — 50 ALSA mixer controls, WirePlumber and UCM2 configs included

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

- Linux kernel 6.1+ with headers installed
- Thunderbolt 3 or 4 connection
- GCC, Make
- Python 3.10+ (for mixer daemon)

### Build and Install

```bash
git clone https://github.com/rolotrealanis98/open-apollo.git
cd open-apollo

# Check dependencies
./scripts/check-deps.sh

# Build and install
./scripts/install.sh

# Or build manually
cd driver && make
sudo insmod ua_apollo.ko
```

### Verify

```bash
# Check driver loaded
dmesg | grep ua_apollo

# Check ALSA card
aplay -l
```

### Start the Mixer Daemon

```bash
cd mixer-engine
python3 ua_mixer_daemon.py -v
```

The daemon exposes TCP:4710 (ConsoleLink protocol) and WS:4721 (WebSocket)
for mixer control from any client application.

## Architecture

```
┌──────────────┐   ┌──────────────┐
│ Console UI   │   │ ConsoleLink  │
│ (WS:4721)    │   │ (TCP:4710)   │
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
| **Mixer Daemon** | `mixer-engine/` | TCP:4710 + WS:4721 daemon — state tree, hardware routing, metering |
| **Configs** | `configs/` | PipeWire, WirePlumber, and UCM2 configuration profiles |
| **Tools** | `tools/contribute/` | Device probe and capture scripts for community contributions |
| **Console UI** | *coming soon* | Mixer control surface (in development) |

## Documentation

Full documentation at **[open-apollo-docs.pages.dev](https://open-apollo-docs.pages.dev/)**, including:

- [Installation guide](https://open-apollo-docs.pages.dev/docs/installation) — build, install, configure
- [Supported devices](https://open-apollo-docs.pages.dev/docs/supported-devices) — model compatibility table
- [Architecture overview](https://open-apollo-docs.pages.dev/docs/architecture-overview) — how the pieces fit together
- [Register map](https://open-apollo-docs.pages.dev/docs/register-map) — BAR0 hardware register documentation
- [DSP protocol](https://open-apollo-docs.pages.dev/docs/dsp-protocol) — ring buffer commands and settings batch protocol
- [How to contribute](https://open-apollo-docs.pages.dev/docs/how-to-contribute) — testing, device captures, code contributions

## Contributing

We welcome contributions of all kinds — from device testing reports to driver
code. See [CONTRIBUTING.md](CONTRIBUTING.md) for details.

**The most impactful thing you can do right now is test on your hardware.** If you
own any Apollo model besides the x4, your testing report directly enables support
for that device.

### Quick contribution path

1. Build and load the driver
2. Run `./tools/contribute/device-probe.sh`
3. [Submit a device report](https://github.com/rolotrealanis98/open-apollo/issues/new?template=device-report.yml)

## License

This project is licensed under the [GNU General Public License v2.0](LICENSE).

See [NOTICE.md](NOTICE.md) for information about trademarks and intellectual
property.
