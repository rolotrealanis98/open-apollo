# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added
- `devices/<model>.json` for every Apollo model (identity, capabilities, Mixer Engine route tables as channel IDs) and `tools/gen-driver-models.py`, which generates `driver/ua_models_gen.h` from them.
- `io_map` module parameter: the descriptor layout reads the firmware IO descriptor and maps each ALSA channel to its DMA slot by channel ID, as the macOS kext does. The default `io_map=-1` uses it on the Apollo x4, where it is hardware-verified, and keeps the legacy layout on every other model; `io_map=1` or `io_map=0` force one layout on every model. On the x4 the ALSA channel order and names stay the same, but S/PDIF, Virtual, MON, AUX and Talkback now carry the audio their names say (the legacy layout mislabelled every channel past MIC 4).
  - The route lists follow the TOSLink mode per direction, as the Mixer Engine does: on the x4, x8p and 8p record follows the input type and playback the output type (`Digital Output Mode`); on the Twin family both follow the input type. The input type is host-side only and set with the new `Digital Input Mode` control (default: the Mixer Engine's per-model default).
  - A layout change while streaming (firmware republish, TOSLink mode or sample-rate change) puts the open streams into XRUN so they re-prepare on the new layout. A descriptor read that is not usable keeps the last valid descriptor instead of falling back to the legacy layout.
- `/proc/asound/cardN/io_map` and `UA_IOCTL_GET_IO_MAP`: channel names, slots and the firmware IO descriptors.

### Changed
- Device-type names follow the kext (`0x20` x6, `0x21` x8, `0x22` x8p, `0x23` x16, `0x35` x16D, `0x37`–`0x3a` Gen 2 rack units); serial prefixes 2107–2110 added. The x8p resolves by serial (device type `0x22`); PCI subsystem `0x0014` is only a fallback.
- The legacy layout (`io_map=0`, and the default on every model except the x4) is unchanged: every device type keeps its DMA channel counts and its preamp/HiZ controls, and the x8p keeps 34/32 and 8 preamps under its corrected type `0x22`. Device types the old model table did not list (`0x3B`, `0x3C`, `0x3E`–`0x41`: serials 2090, 2091, 2107–2110) take their preamp counts from the generated model data. With the descriptor layout, all preamp/HiZ counts come from the model data and the channel counts from the firmware (the x4 keeps its 4 preamps and 2 HiZ inputs).
- The cold start no longer uploads the captured DSP "programs" (command `0x00360000`): the kext never sends it, and on the x4 it timed out with `DSP program load failed: -110` on every cold start.
- Documentation moved to the [GitHub wiki](https://github.com/rolotrealanis98/open-apollo/wiki). The `docs/` directory and the open-apollo-docs site are retired; old `open-apollo-docs.pages.dev` links redirect to the wiki.

### Fixed
- Reboot, power-off and suspend now stop the Apollo first (`.shutdown` and suspend/resume): mute, transport off, DMA quiesced, disconnect signal. Unloading the module no longer leaves the audio extension connected, so `rmmod` + `insmod` reconnects and keeps the mixer DSP running.
- Preamp PAD, 48V and Mic/Line work from Linux. Mixer settings are now written as the macOS driver writes them, each change once and then cleared (`mixer_mailbox`, default on), instead of re-sending every touched field on every update; with the old method the DSP ignored PAD, 48V and Mic/Line. The front-panel buttons and the monitor knob also keep their changes instead of being overwritten by the next settings update. `mixer_mailbox=0` restores the old behaviour.
- The DSP command ring is circular: mixer commands no longer stop for good after about 1,000 changes.
- ASPM L0s is turned off on the Apollo's PCIe link; with it on, the link logged a steady stream of correctable errors while streaming.
- Recording works after every sample-rate change on the x4: a 192 kHz stream started while the clock was already at 176.4 or 192 kHz used to record nothing, and a 176.4 kHz one did about 1 time in 6.
- Mixer daemon: talkback works from a client. The device maps keep `TalkbackOn` at the tree root (`/TalkbackOn`), and the daemon only routed `/devices/0/TalkbackOn` to the hardware; the talkback button's state is now also reported on `/TalkbackOn`.
- Mixer daemon: a front-panel monitor-knob change now updates `CRMonitorLevel` (dB) as well as `CRMonitorLevelTapered`, so clients reading the dB value no longer see a stale level.
- Mixer daemon: level meters work while PipeWire is running. They used to need the ALSA capture device to themselves (and pyalsaaudio), so every meter read silence on a desktop; they now come from the Apollo's PipeWire capture node, falling back to direct ALSA capture without PipeWire. Each meter reads the channel it names (input meters used to read the record channel with the same index, so ADAT 1 showed S/PDIF L and so on), the monitor and AUX bus meters work, and peak hold holds while the level stays at the peak.
- Mixer daemon: a Console sample-rate change works while PipeWire holds the Apollo. The daemon sets PipeWire's `clock.force-rate` (cleared again when the rate goes back to PipeWire's default) instead of changing the clock under PipeWire's streams, which PipeWire undid at its next prepare; `/SampleRate` now follows the driver's rate. `clock.force-rate` is global, so PipeWire's other devices follow too.
- WirePlumber rules: the Apollo nodes allow every rate (`audio.allowed-rates`) instead of pinning `audio.rate = 48000`, which kept the Apollo at 48 kHz, and both nodes are in one `node.group`. Playback and capture share one clock; without the group a forced rate moved only the playback node, and the two re-prepared at different rates in a loop, about 5 changes a second, which left the DSP recording silence until the Apollo was power-cycled.
- Mixer daemon: starting it no longer resets the Apollo. Preamp gain and switches, link, and the monitor level, mute, mono and dim are taken from the hardware, so front-panel changes made while no daemon ran are kept; safe defaults (monitor at minimum) are only used when there is no hardware readback. Input faders and mutes a client set are restored from the saved state instead of every input being muted, and the init sequence is sent once instead of twice when a client connects during startup.
- Mixer daemon: after the front-panel gain knob moves, setting a preamp's gain back to the value a client last set now reaches the hardware; the daemon used to drop it as a duplicate.

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
