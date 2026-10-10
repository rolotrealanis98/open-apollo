# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added
- `devices/<model>.json` for every Apollo model (identity, capabilities, Mixer Engine route tables as channel IDs) and `tools/gen-driver-models.py`, which generates `driver/ua_models_gen.h` from them.
- `io_map` module parameter. `io_map=1` reads the firmware IO descriptor and maps each ALSA channel to its DMA slot by channel ID, as the macOS kext does; `io_map=0` (default) keeps the legacy layout.
  - The route lists follow the TOSLink mode per direction, as the Mixer Engine does: on the x4, x8p and 8p record follows the input type and playback the output type (`Digital Output Mode`); on the Twin family both follow the input type. The input type is host-side only and set with the new `Digital Input Mode` control (default: the Mixer Engine's per-model default).
  - A layout change while streaming (firmware republish, TOSLink mode or sample-rate change) puts the open streams into XRUN so they re-prepare on the new layout. A descriptor read that is not usable keeps the last valid descriptor instead of falling back to the legacy layout.
- `/proc/asound/cardN/io_map` and `UA_IOCTL_GET_IO_MAP`: channel names, slots and the firmware IO descriptors.

### Changed
- Device-type names follow the kext (`0x20` x6, `0x21` x8, `0x22` x8p, `0x23` x16, `0x35` x16D, `0x37`–`0x3a` Gen 2 rack units); serial prefixes 2107–2110 added. The x8p resolves by serial (device type `0x22`); PCI subsystem `0x0014` is only a fallback.
- The default (`io_map=0`) layout is unchanged: every device type keeps its DMA channel counts and its preamp/HiZ controls, and the x8p keeps 34/32 and 8 preamps under its corrected type `0x22`. Device types the old model table did not list (`0x3B`, `0x3C`, `0x3E`–`0x41`: serials 2090, 2091, 2107–2110) take their preamp counts from the generated model data. With `io_map=1`, all preamp/HiZ counts come from the model data and the channel counts from the firmware.
- Documentation moved to the [GitHub wiki](https://github.com/rolotrealanis98/open-apollo/wiki). The `docs/` directory and the open-apollo-docs site are retired; old `open-apollo-docs.pages.dev` links redirect to the wiki.

## [1.0.0] - 2026-03-22

### Added
- Linux PCIe kernel module for Apollo Thunderbolt interfaces
- Full duplex audio: 24-channel playback + 22-channel capture (4 analog inputs and 6 analog outputs verified; S/PDIF, ADAT, and virtual channels unverified)
- All 6 sample rates (44.1kHz – 192kHz)
- Preamp control via ALSA mixer: 48V phantom, pad, gain, phase, low cut
- 50 ALSA mixer controls
- PipeWire integration with named virtual I/O devices (Mic 1–4, Monitor L/R, Line Out)
- Dynamic PCI address discovery (`apollo-setup-io`)
- One-command installer with DKMS support
- WirePlumber rules (disable-mmap, no suspend, S32LE/48kHz)
- UCM2 ALSA profile
- udev auto-detection and profile setup
- Systemd user service for virtual I/O auto-setup
- System tray indicator
- Anonymous install telemetry (opt-in)
- Documentation site at open-apollo-docs.pages.dev

### Fixed
- PipeWire loopback module wiring (capture/playback sides were swapped)
- PCIe ASPM/bridge setup moved out of real-time audio thread
- Clock write ordering (after transport start, not before)

### Known Issues
- Virtual I/O devices may not auto-appear on boot (run `apollo-setup-io` manually)
- `alsactl restore` can freeze the DSP — remove Apollo entries from asound.state
- Only tested on Apollo x4 — other models need community testing
