# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Changed
- Documentation moved to the [GitHub wiki](https://github.com/rolotrealanis98/open-apollo/wiki). The `docs/` directory and the open-apollo-docs site are retired; old `open-apollo-docs.pages.dev` links redirect to the wiki.

### Fixed
- Require explicit hazard acknowledgement for development helper full-mode unloads.
- Diagnose development helper claims with the existing initializer and warn about autoload.
- Detect autoload references without early-exit pipeline errors; report inspection failures as unknown.
- Stop initialization on failed firmware replay or invalid health reads; keep development claims from changing audio services.
- Keep Twin X Gen 2 outside the untested DSP opt-in and DUO channel measurements.
- Check device reachability during identification-only PCI recovery.
- Resolve multi-word device models to their complete mixer-map filenames.
- Keep bus mastering off during identification-only probes so a probe_only load cannot start DMA.
- Report development-helper probe side effects, initializer open errors and unreadable SIP status accurately.

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
