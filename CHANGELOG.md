# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added
- `dsp_ready_wait` module parameter (experimental, default 0): on a cold start, poll each DSP's ready word before setting up its rings and retry a failed DSP connect once, as the macOS driver does when `ext_caps[7:4] == 1` (`1` = that rule, `2` = every v2 device). The Apollo x4 reports 1, so macOS polls on it; the default stays off until a cold-start test.
- ALSA controls follow the front panel: monitor level, mute, mono, dim, talkback, both headphone volumes, and each preamp's switches, gain and HiZ detect update (with change events) when the hardware changes them. New controls: `Headphone 1 Playback Volume`, read-only `Headphone 2 Playback Volume` and `Line N HiZ Detect Switch`.
- Installer: the mixer daemon runs as a systemd user service (`ua-mixer-daemon.service`, restarted on failure) instead of a background process started once; `driver/dkms.conf` is tracked; old `io_map=`/`mixer_mailbox=` overrides in `/etc/modprobe.d` are reported.
- `configs/pipewire/setup-apollo-io.sh` builds its loopbacks from the driver's `/proc/asound/cardN/io_map` (same node names), with `--dry-run`, `--io-map FILE`, `--card N` and opt-in Virtual loopbacks (`--virtual`).
- `tools/apollo-report.sh` (read-only diagnostic report, no personal data) and `tools/apollo-selftest.sh` (opt-in duplex test per sample rate with PASS/SILENT/DEAD/ROUTE verdicts) for owners of other models; see `tools/TESTING.md`.
- `tools/check-models.py` checks `devices/*.json` against itself, the generated header and the driver's hand-written tables (run in CI).
- Mixer daemon: models without a captured device map get one generated at runtime from `devices/<model>.json` (cached under `~/.cache/open-apollo/device_maps`, with a warning), instead of falling back to the x4 map. Generated maps carry no plugin or authorization data.
- Mixer daemon: a pytest suite (`mixer-engine/tests`, no hardware needed), run in CI.
- Experimental module parameters for the sample-rate change, for testing the macOS driver's sequence (all default to the current behaviour): `rate_change=1` changes the rate with the transport stopped, waits for the firmware rate, sends the rate to every DSP and holds 1.5 s, without the reconnect or the 48 kHz pre-session; `post_start_clock=0` skips the clock write after each transport start; `ax_control_ext=0` starts the transport with AX_CONTROL 0xF instead of 0x20F.
- `devices/<model>.json` for every Apollo model (identity, capabilities, Mixer Engine route tables as channel IDs) and `tools/gen-driver-models.py`, which generates `driver/ua_models_gen.h` from them.
- `io_map` module parameter: the descriptor layout reads the firmware IO descriptor and maps each ALSA channel to its DMA slot by channel ID, as the macOS kext does. The default `io_map=-1` uses it on the Apollo x4, where it is hardware-verified, and keeps the legacy layout on every other model; `io_map=1` or `io_map=0` force one layout on every model. On the x4 the ALSA channel order and names stay the same, but S/PDIF, Virtual, MON, AUX and Talkback now carry the audio their names say (the legacy layout mislabelled every channel past MIC 4).
  - The route lists follow the TOSLink mode per direction, as the Mixer Engine does: on the x4, x8p and 8p record follows the input type and playback the output type (`Digital Output Mode`); on the Twin family both follow the input type. The input type is host-side only and set with the new `Digital Input Mode` control (default: the Mixer Engine's per-model default).
  - A layout change while streaming (firmware republish, TOSLink mode or sample-rate change) puts the open streams into XRUN so they re-prepare on the new layout. A descriptor read that is not usable keeps the last valid descriptor instead of falling back to the legacy layout.
- `/proc/asound/cardN/io_map` and `UA_IOCTL_GET_IO_MAP`: channel names, slots and the firmware IO descriptors.

### Changed
- Kernel log: per-stream, per-start and per-rate-change register dumps, IO descriptor slot lists and the DSP service ticks are debug-level (dynamic debug brings them back); a missing optional plugin-chain blob is reported once. A run of 14 duplex rate changes logs about 25 lines.
- UCM2: the `HiFi` profile matches the Apollo x4 only (it describes the x4's layout) and documents the v2 channel layout; its devices are named `Outputs` and `Inputs`.
- Device-type names follow the kext (`0x20` x6, `0x21` x8, `0x22` x8p, `0x23` x16, `0x35` x16D, `0x37`–`0x3a` Gen 2 rack units); serial prefixes 2107–2110 added. The x8p resolves by serial (device type `0x22`); PCI subsystem `0x0014` is only a fallback.
- The legacy layout (`io_map=0`, and the default on every model except the x4) is unchanged: every device type keeps its DMA channel counts and its preamp/HiZ controls, and the x8p keeps 34/32 and 8 preamps under its corrected type `0x22`. Device types the old model table did not list (`0x3B`, `0x3C`, `0x3E`–`0x41`: serials 2090, 2091, 2107–2110) take their preamp counts from the generated model data. With the descriptor layout, all preamp/HiZ counts come from the model data and the channel counts from the firmware (the x4 keeps its 4 preamps and 2 HiZ inputs).
- The cold start no longer uploads the captured DSP "programs" (command `0x00360000`): the kext never sends it, and on the x4 it timed out with `DSP program load failed: -110` on every cold start.
- `/dev/ua_apolloN` is no longer world-writable: the new `70-ua-apollo-access.rules` gives it to the logged-in desktop user (uaccess) and the `audio` group, and the raw register, DMA, DSP-ring and firmware ioctls need `CAP_SYS_RAWIO`. The ioctls the mixer daemon uses are unchanged. The raw ARM CLI ioctl is refused (`-ENOSYS`), as the driver's own CLI path already was, since CLI commands freeze the ARM MCU.
- Documentation moved to the [GitHub wiki](https://github.com/rolotrealanis98/open-apollo/wiki). The `docs/` directory and the open-apollo-docs site are retired; old `open-apollo-docs.pages.dev` links redirect to the wiki.

### Fixed
- Reboot, power-off and suspend now stop the Apollo first (`.shutdown` and suspend/resume): mute, transport off, DMA quiesced, disconnect signal. Unloading the module no longer leaves the audio extension connected, so `rmmod` + `insmod` reconnects and keeps the mixer DSP running.
- Preamp PAD, 48V and Mic/Line work from Linux. Mixer settings are now written as the macOS driver writes them, each change once and then cleared (`mixer_mailbox`, default on), instead of re-sending every touched field on every update; with the old method the DSP ignored PAD, 48V and Mic/Line. The front-panel buttons and the monitor knob also keep their changes instead of being overwritten by the next settings update. `mixer_mailbox=0` restores the old behaviour.
- The DSP command ring is circular: mixer commands no longer stop for good after about 1,000 changes.
- ASPM L0s is turned off on the Apollo's PCIe link; with it on, the link logged a steady stream of correctable errors while streaming.
- Recording works after every sample-rate change on the x4: a 192 kHz stream started while the clock was already at 176.4 or 192 kHz used to record nothing, and a 176.4 kHz one did about 1 time in 6.
- Unplugging, powering off or unbinding the Apollo while the mixer daemon, PipeWire or another program has it open no longer leaves the driver using freed memory: the device structure lives until the last `/dev/ua_apolloN` file is closed, ioctls in flight finish before the teardown and later ones get `-ENODEV`, and the ALSA card is freed only after its open files are closed.
- Recording and playback at the same time: a stream that stops (a short `aplay` ending) no longer stops the other one's period wakeups, which used to make it fail with an I/O error after 10 s. Streams that lose the transport (AER, a stopped transport) are put into XRUN instead of stalling.
- A preamp switch or gain set through the ioctl while the mixer is not ready (during a resume re-init) now returns an error instead of reporting success and updating the ALSA control.
- After a DSP command that got no response, the next commands on that DSP no longer time out: the leftover response slot is dropped first.
- PCIe error recovery (AER) reconnects the Apollo the way system resume does; before, it reset every DMA engine (killing the mixer DSP) and in practice never recovered.
- Firmware blob parsing checks every offset and length for overflow, and the plugin chain never asks the device to read past a payload buffer.
- `UA_IOCTL_DMA_TEST` refuses to run while a stream is open and always stops the transport it started.
- The driver builds with clang: the GCC-only optimisation overrides are skipped there.
- Mixer daemon: talkback works from a client. The device maps keep `TalkbackOn` at the tree root (`/TalkbackOn`), and the daemon only routed `/devices/0/TalkbackOn` to the hardware; the talkback button's state is now also reported on `/TalkbackOn`.
- Mixer daemon: a front-panel monitor-knob change now updates `CRMonitorLevel` (dB) as well as `CRMonitorLevelTapered`, so clients reading the dB value no longer see a stale level.
- Mixer daemon: `/ClockLocked` follows the hardware's clock-lock flag (readback word 6 bit 31, valid with bit 30) instead of always reading locked.
- ALSA preamp controls (gain, 48V, PAD, phase, low cut, Mic/Line) write the hardware; they used to change only the driver's cached value. Gain uses the macOS triplet and the 10–65 dB range.
- Monitor mute and mono no longer clear each other (both go through the same parameter).
- ALSA control change events carry the control's numid, so alsa-lib clients see them; enum controls never report a value outside their list.
- Mixer daemon: a client SET of the monitor level in dB (or tapered) updates the other value too, so a later knob change to the same position is still reported.
- Mixer daemon: `?recursive=0` is not recursive.
- Model data: duplicate and leaked mixer input names in five `devices/*.json` files, and missing subsystem IDs for the 8p and Solo.
- Mixer daemon: send meters (AUX 1/2, CUE 1-4) and post-fader input metering (`/PostFaderMetering`) are computed from the input meters as the Mixer Engine does: input level plus the send gain, plus the fader for a post-fader send (the send's override, else the AUX bus's or HP n's SendPostFader); muted or bypassed paths read silent. Send meters used to show the raw input level.
- Mixer daemon: on the Apollo x4 the meters now come from the hardware, as on macOS: the DSP writes every input's level (Virtual included) and the monitor and AUX bus levels into readback words 8-25, which the daemon used to treat as unused. The PipeWire/ALSA capture meters stay as the fallback for other models.
- Mixer daemon: HiZ jack detection is reported per input (readback word 0 bit 24 = input 1, bit 25 = input 2; only bit 24 was decoded), and the init sequence enables the CUE bus routing paths 5 and 6 instead of disabling them (the driver takes the parameter ID as the enable value).
- Mixer daemon: level meters work while PipeWire is running. They used to need the ALSA capture device to themselves (and pyalsaaudio), so every meter read silence on a desktop; they now come from the Apollo's PipeWire capture node, falling back to direct ALSA capture without PipeWire. Each meter reads the channel it names (input meters used to read the record channel with the same index, so ADAT 1 showed S/PDIF L and so on), the monitor and AUX bus meters work, and peak hold holds while the level stays at the peak.
- Clock Source: the control, `UA_IOCTL_SET_DRIVER_PARAM` (param 1) and the mixer daemon use the device's source values (Internal 0x0C, S/PDIF 0x00, ADAT 0x05, Word Clock 0x07) instead of the index 0–3. The control read 12, a value outside its list (so `alsactl` could not restore it), and choosing Internal selected S/PDIF. Sources the device doesn't offer (it lists them at connect; the x4 has no Word Clock) are refused, and the source is committed only once the device acknowledges it.
- Playback and capture can no longer run at different rates; they share one clock. Once one direction has a rate, the other is offered only that rate, and the Sample Rate control and `UA_IOCTL_SET_DRIVER_PARAM` (param 0) return -EBUSY for another rate while a stream is open. Before, the two re-prepared each other's rate in a loop, and the DSP could end up recording silence until a power cycle.
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
