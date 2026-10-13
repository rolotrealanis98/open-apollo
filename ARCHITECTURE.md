# Architecture Guide

Quick map for developers who want to contribute.

## Directory Structure

```
open-apollo/
├── driver/              # Linux kernel module (C)
│   ├── ua_core.c        # PCIe probe, ioctls, DSP settings, ACEFACE
│   ├── ua_audio.c       # ALSA PCM, mixer controls, DMA, transport
│   ├── ua_dsp.c         # DSP ring buffer, firmware, routing tables
│   ├── ua_event.c       # Events to the daemon (meters, jack detect, clock)
│   ├── ua_apollo.h      # Register map, device types, structs
│   ├── ua_ioctl.h       # The ioctl ABI the daemon uses
│   ├── ua_models_gen.h  # Per-model data, generated from devices/*.json
│   └── ua_routing.h     # Static channel routing tables
│
├── mixer-engine/        # Mixer daemon (Python)
│   ├── ua_mixer_daemon.py  # TCP 4710 + 4720 servers (WebSocket 4721 with --ws)
│   ├── hardware.py      # Thunderbolt backend (ioctl interface); hardware_usb.py for USB
│   ├── state_tree.py    # Hierarchical control tree
│   ├── console_*.py     # Console host commands: presets, sessions, inserts, network
│   ├── ua_catalogue.py  # Overlays the UA catalogue imported from the user's install
│   └── device_maps/     # Per-model control trees (structure only, no UA content)
│
├── console-rs/          # Rust workspace (see console-rs/README.md, console-rs/ARCHITECTURE.md)
│   └── crates/
│       ├── uaconsole/   # The native mixer console
│       ├── houston, rack, gfx  # Console art, layout and drawing
│       ├── plugin-ui/   # Plug-in windows drawn from open-apollo-panels specs
│       ├── ua-engine/   # Mixer Engine protocol and model (TCP 4710)
│       ├── engine-model/  # The console's mixer model over ua-engine
│       ├── oa-assets/   # Importer: reads the user's UAD install
│       └── oa-setup/    # Setup wizard (open-apollo-setup)
│
├── configs/             # PipeWire, WirePlumber, UCM2, udev, systemd; plugin-panels.conf (the pinned panels release)
│
├── scripts/
│   ├── setup.sh         # Builds and starts the setup wizard
│   ├── setup-root.sh    # The wizard's one privileged run
│   ├── install.sh       # Thunderbolt driver, DKMS, audio settings, mixer daemon
│   ├── install-usb.sh   # USB Apollos
│   ├── install-panels.sh     # Plug-in panel specs (pinned open-apollo-panels release)
│   ├── install-uaconsole.sh  # Per-user install of the console
│   └── uninstall.sh     # Removes every trace of an install
│
├── tools/
│   ├── uausb/, usb-full-init.py  # USB bring-up, built from the extracted DSP modules
│   ├── check-clean-repo.py       # Keeps UA files and personal data out of the repository
│   ├── apollo-report.sh, apollo-selftest.sh  # Device reports (TESTING.md)
│   └── contribute/      # Device probe and capture scripts
│
└── devices/             # Per-model facts (*.json), from UA's own Mixer Engine
```

## Setup Flow

```
scripts/setup.sh ──► open-apollo-setup (as you)
                        │ finds the UAD install (oa-assets scan), shows the plan
                        ▼
                     pkexec scripts/setup-root.sh (as root, one password)
                        ├─ uninstall.sh --yes --purge        remove the earlier install
                        ├─ oa-assets import                  Console art (as you)
                        ├─ install-panels.sh                 plug-in panel specs
                        ├─ oa-assets plugins                 plug-in panels' art (as you)
                        ├─ oa-assets firmware, dsp           DSP modules, USB boot images
                        └─ install.sh --yes                  driver, DKMS, configs, daemon
                        ▼
                     install-uaconsole.sh (as you), daemon restart
```

Nothing from Universal Audio is in this repository: everything on the right-hand side of
that flow is read from the user's own UAD install on their machine, and
`tools/check-clean-repo.py` (run in CI) refuses commits that would add UA files.

## Data Flow

```
App (PipeWire) → ALSA PCM → ua_audio.c → DMA buffer → Apollo FPGA
                                ↕
                         ua_core.c (ioctl)
                                ↕
                    ua_mixer_daemon.py (TCP 4710/4720) ◄── uaconsole
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

### Object commands (`post`) and the daemon's own files:
`post <object path>/<CommandName> [json]` runs a Mixer Engine object command (`mixer-engine/commands.py`). An unknown command answers `Unable to resolve the command for the post.`, a known one that fails `Unable to execute command.`, both in the get/set error framing (`{"path","error"}`); success is `{"path","data"}` with `true` or the command's result. The parameter keys (`name`, `index`, `file`, `seconds`, ...) are this daemon's own: the engine's `cmdparams` format is not known. Snapshots, presets and sessions are **the daemon's own JSON** in `mixer-engine/state/` (`<device>.snapshots.json`, `<device>.presets.json`, `<device>.sessions/<name>.session.json`), **not UA's session XML**; no UA tool reads them. Licensing commands are never implemented.

## Where to Start

- **Bug fix in audio routing?** → `driver/ua_audio.c` (pcm_prepare, transport start/stop)
- **New Apollo model?** → `driver/ua_apollo.h` (device_type, channel counts)
- **PipeWire config issue?** → `configs/pipewire/setup-apollo-io.sh`
- **Mixer protocol?** → `mixer-engine/ua_mixer_daemon.py`
- **Mixer console UI?** → `console-rs/` ([README](console-rs/README.md), [architecture](console-rs/ARCHITECTURE.md))
- **Install script?** → `scripts/install.sh` (driver), `scripts/setup-root.sh` (the wizard's run)
- **Setup wizard or importer?** → `console-rs/crates/oa-setup/`, `console-rs/crates/oa-assets/`
- **Plug-in windows?** → `console-rs/crates/plugin-ui/` ([RUNTIME-V2.md](console-rs/crates/plugin-ui/RUNTIME-V2.md))
