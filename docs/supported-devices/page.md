---
title: Supported Devices
---

Open Apollo supports Universal Audio Apollo Thunderbolt and USB interfaces. Thunderbolt models use a native Linux kernel driver. USB models use a pure-userspace stack (UAC 2.0 audio + patched `snd-usb-audio` — no kernel module required).

---

## Thunderbolt devices

| Model | Device Type | Playback | Record | Preamps | HiZ | Status |
|---|---|---|---|---|---|---|
| Apollo 8P (original) | 0x0A | 26 | 26 | 8 | 2 | Needs Testing |
| Apollo Solo | 0x27 | 3 | 2 | 1 | 0 | Needs Testing |
| Arrow | 0x28 | 3 | 2 | 1 | 0 | Needs Testing |
| Apollo Twin X | 0x23 | 8 | 8 | 2 | 2 | Needs Testing |
| Apollo Twin X Gen 2 | 0x3A | 8 | 8 | 2 | 2 | Needs Testing |
| Apollo x4 | 0x1F | 24 | 22 | 4 | 2 | **Partially Verified** |
| Apollo x4 Gen 2 | 0x36 | 24 | 22 | 4 | 2 | Needs Testing |
| Apollo x6 | 0x1E | 24 | 22 | 4 | 2 | Needs Testing |
| Apollo x6 Gen 2 | 0x35 | 24 | 22 | 4 | 2 | Needs Testing |
| Apollo x8 | 0x22 | 26 | 26 | 4 | 2 | Needs Testing |
| Apollo x8 Gen 2 | 0x37 | 26 | 26 | 4 | 2 | Needs Testing |
| Apollo x8p | 0x20 | 26 | 26 | 8 | 2 | Needs Testing |
| Apollo x8p Gen 2 | 0x38 | 26 | 26 | 8 | 2 | Needs Testing |
| Apollo x16 | 0x21 | 34 | 34 | 8 | 2 | Needs Testing |
| Apollo x16 Gen 2 | 0x39 | 34 | 34 | 8 | 2 | Needs Testing |
| Apollo x16D | 0x2A | 34 | 34 | 0 | 0 | Needs Testing |

---

## USB devices

USB Apollo models use UAC 2.0 audio. No kernel module is required — the FX3 firmware is uploaded from userspace on each power-on, and `snd-usb-audio` (with a 3-line patch) handles audio streaming. See [USB RE findings](/docs/usb-apollo-re) for protocol details.

| Model | VID | PID (live) | Playback | Record | Preamps | HiZ | Status |
|---|---|---|---|---|---|---|---|
| Apollo Solo USB | 0x2B5A | 0x000D | 6 | 10 | 2 | 2 | **Partially Verified** |
| Apollo Twin USB | 0x2B5A | 0x0002 | — | — | 2 | 2 | Needs Testing |
| Apollo Twin X USB | 0x2B5A | 0x000F | — | — | 2 | 2 | Needs Testing |

{% callout type="note" %}
USB setup uses `sudo bash scripts/install-usb.sh`. See [Installation](/docs/installation) for details, or [USB RE findings](/docs/usb-apollo-re) for protocol details.
{% /callout %}

---

### Column definitions

- **Playback**: Number of playback (output) channels at 48 kHz
- **Record**: Number of record (input) channels at 48 kHz
- **Preamps**: Number of analog preamp inputs with gain/48V/PAD controls
- **HiZ**: Number of preamp channels that support Hi-Z (instrument) input
- **Status**: Whether the model has been tested on Linux with this driver

---

## What "Verified" means

### Thunderbolt (Apollo x4)

The Apollo x4 is the primary development and test device. On this model, the following features are confirmed working:

- Full duplex audio with **4 analog inputs** (Mic 1-4) and **6 analog outputs** (Monitor L/R, Line Out 1-4) verified
- The driver exposes all 24 playback / 22 record ALSA channels, but S/PDIF, ADAT, and virtual (DAW playback) channels are **unverified and likely not working**
- All six sample rates (44.1, 48, 88.2, 96, 176.4, 192 kHz)
- Preamp gain control (all 4 channels)
- Preamp flags: 48V phantom power, PAD, low cut, phase invert, mic/line switching
- Monitor volume, mute, dim, mono, talkback
- DSP mixer routing (analog buses only — digital bus routing untested)
- ALSA integration with 50+ mixer controls

### USB (Apollo Solo USB)

The Apollo Solo USB is "Partially Verified" — playback is confirmed working; capture has known issues:

- **6ch playback** (S32_LE 48kHz) confirmed on Ubuntu Studio 24.04 (kernel 6.17, Intel) and CachyOS (kernel 6.19, AMD)
- **PipeWire playback** — browser audio, system audio working
- **Preamp gain, 48V, monitor level/mute** — working via mixer daemon
- **10ch capture** — works with `usb-full-init.py` but the init sequence is firmware-version-specific; some firmware builds crash at packet 28
- **Capture through PipeWire** — returns zeros even when raw ALSA capture works (under investigation)
- Hardware monitoring (mic → headphones) works when PipeWire is stopped

See [USB RE findings](/docs/usb-apollo-re) for the full protocol details and known issues.

---

## What "Needs Testing" means

These models are recognized by the driver and have correct channel count configurations, but have not been tested on actual hardware. They likely work — the Apollo product line shares a common architecture — but may need adjustments to routing tables or feature flags.

---

## How to help

If you own an Apollo model marked "Needs Testing", your contribution would be extremely valuable. Even basic testing (does the driver load? does ALSA see the device? does audio play?) helps us mark models as verified.

See [How to Contribute](/docs/how-to-contribute) for details, or jump straight to the device capture guides:

- [Device Capture (macOS)](/docs/device-capture-macos) — capture routing data using DTrace
- [Device Capture (Windows)](/docs/device-capture-windows) — capture register snapshots using WinRing0

You can also run the quick probe script on Linux:

```bash
sudo ./tools/contribute/device-probe.sh
```

This reads basic device information and outputs a report you can submit.
