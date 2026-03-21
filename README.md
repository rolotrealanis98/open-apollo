# Open Apollo

**Open-source Linux driver for Universal Audio Apollo Thunderbolt interfaces**

Open Apollo brings full Linux support to Universal Audio's Apollo Thunderbolt
audio interfaces. Built through clean-room reverse engineering, this
community-driven project delivers a native kernel driver, userspace mixer
daemon, and mixer control surface — so Apollo owners can use their hardware
on Linux without compromise.

## Supported Devices

| Device | Status |
|--------|--------|
| Apollo x4 | **Verified** — playback, capture, preamps, gain, DSP settings |
| Apollo x4 Gen 2 | Needs Testing |
| Apollo x6 | Needs Testing |
| Apollo x6 Gen 2 | Needs Testing |
| Apollo x8 | Needs Testing |
| Apollo x8 Gen 2 | Needs Testing |
| Apollo x8p | Needs Testing |
| Apollo x8p Gen 2 | Needs Testing |
| Apollo x16 | Needs Testing |
| Apollo x16 Gen 2 | Needs Testing |
| Apollo x16D | Needs Testing |
| Apollo Twin X | Needs Testing |
| Apollo Twin X Gen 2 | Needs Testing |
| Apollo Solo | Needs Testing |
| Arrow | Needs Testing |

Have one of these devices? See [Contributing](#contributing) to help us expand
support.

## Quick Start

### Prerequisites

- Linux kernel 6.1+ with headers installed
- Thunderbolt 3 or 4 connection
- Python 3.10+ (for mixer daemon)

### Build and Install

```bash
# Check dependencies
./scripts/check-deps.sh

# Build and install
./scripts/install.sh

# Or build manually
cd driver && make
sudo insmod ua_apollo.ko
```

### Start the Mixer Daemon

```bash
cd mixer-engine
python3 ua_mixer_daemon.py -v
```

## Components

| Component | Path | Description |
|-----------|------|-------------|
| **Driver** | `driver/` | Linux PCIe kernel module (`ua_apollo.ko`) — handles hardware register access, DMA, ALSA integration |
| **Mixer Daemon** | `mixer-engine/` | Userspace daemon (TCP:4710 + WS:4720) — mixer state management, hardware routing, metering |
| **Console UI** | *coming soon* | Electron + React mixer control surface |

## Documentation

Full documentation at the [Open Apollo docs site](https://open-apollo-docs.pages.dev/).

## Contributing

We welcome contributions of all kinds — from device testing reports to driver
code. See [CONTRIBUTING.md](CONTRIBUTING.md) for details on how to get involved.

The most impactful thing you can do right now is **test on your hardware** and
submit a device report.

## License

This project is licensed under the [GNU General Public License v2.0](LICENSE).

See [NOTICE.md](NOTICE.md) for information about trademarks and intellectual
property.
