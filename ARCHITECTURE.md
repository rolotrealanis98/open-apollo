# Architecture Guide

Quick map for developers who want to contribute.

## Directory Structure

```
open-apollo/
├── driver/              # Linux kernel module (C)
│   ├── ua_audio.c       # ALSA PCM, mixer controls, DMA, transport
│   ├── ua_core.c        # PCIe probe, ioctls, DSP settings, ACEFACE
│   ├── ua_dsp.c         # DSP ring buffer, firmware, routing tables
│   ├── ua_apollo.h      # Register map, device types, structs
│   └── ua_routing.h     # Static channel routing tables
│
├── mixer-engine/        # Userspace daemon (Python)
│   ├── ua_mixer_daemon.py  # TCP:4710 + TCP:4720 servers
│   ├── hardware.py      # Hardware backend (ioctl interface)
│   ├── state_tree.py    # Hierarchical control tree (11K+ nodes)
│   └── helper_tree.py   # Protocol helper tree
│
├── console-rs/          # Native mixer console, uaconsole (Rust; see console-rs/README.md)
│
├── configs/             # System configs deployed by installer
│   ├── wireplumber/     # WirePlumber rules (disable-mmap, etc.)
│   ├── ucm2/            # ALSA UCM2 profile
│   ├── udev/            # Auto-detection + profile setup
│   ├── pipewire/        # Virtual I/O setup script
│   ├── autostart/       # Desktop autostart + systemd service
│   └── deploy.sh        # Config deployment script
│
├── scripts/
│   ├── install.sh       # One-command installer
│   ├── install-uaconsole.sh  # Per-user install of the native console
│   └── check-deps.sh    # Dependency checker
│
└── tools/               # Utilities
    ├── apollo-init.sh   # Hardware initialization
    ├── open-apollo-console.sh  # Opens uaconsole, or the Windows Console under Wine
    └── open-apollo-tray.py  # System tray indicator
```

## Data Flow

```
App (PipeWire) → ALSA PCM → ua_audio.c → DMA buffer → Apollo FPGA
                                ↕
                         ua_core.c (ioctl)
                                ↕
                    ua_mixer_daemon.py (TCP:4710/4720)
                                ↕
                         ua_dsp.c (ring buffer commands)
                                ↕
                         Apollo DSP / ARM MCU
```

## Key Concepts

### Hardware writes happen through 4 paths:
1. **BAR0 MMIO** — register reads/writes (driver ↔ FPGA)
2. **DSP ring buffer** — commands to the onboard DSP (mixer settings, routing)
3. **DMA** — audio sample data (playback buffer + capture buffer)
4. **ARM CLI** — preamp gain via SPI to PGA2500 relays

### The driver never guesses:
Every register write was traced from the macOS kext via DTrace or Windows via BAR0 captures. If you're adding hardware interaction, trace the working driver first.

## Where to Start

- **Bug fix in audio routing?** → `driver/ua_audio.c` (pcm_prepare, transport start/stop)
- **New Apollo model?** → `driver/ua_apollo.h` (device_type, channel counts)
- **PipeWire config issue?** → `configs/pipewire/setup-apollo-io.sh`
- **Mixer protocol?** → `mixer-engine/ua_mixer_daemon.py`
- **Mixer console UI?** → `console-rs/` ([README](console-rs/README.md), [architecture](console-rs/ARCHITECTURE.md))
- **Install script?** → `scripts/install.sh`
