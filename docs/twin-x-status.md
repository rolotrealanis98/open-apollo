# Apollo Twin X (Thunderbolt) — status

Contributor branch work targeting Apollo Twin X **DUO** (`device_type` `0x23`,
PCI subsystem `0x0019`).

## What this PR changes

| Change | File | Why |
|---|---|---|
| `UA_SUBSYS_APOLLO_TWIN_X_DUO` + probe pin | `driver/ua_apollo.h`, `driver/ua_core.c` | Serial-prefix matching mis-IDs some units as Solo; subsystem `0x0019` is authoritative for DUO |
| Channel map `10/16`, `num_hiz` `1` | `driver/ua_audio.c` | Measured on real hardware (macOS IOKit + Linux ALSA); replaces `8/8` placeholders and wrong Hi-Z count |
| `probe_only=1` | `driver/ua_core.c` | Safe identification-only first contact (no IRQ/DMA/ALSA) |
| `twinx_dsp=1` (default off) | `driver/ua_dsp.c` | Optional x4-derived DSP0 programs; not known-correct for Twin X silicon |
| Device descriptor | `devices/apollo-twin-x.json` | Upstream contribution schema for a verified Thunderbolt model |
| Test notes / helper scripts | `docs/twin-x-*.md`, `scripts/twinx-*.sh` | Reproduce identification and audio checks |

## Verified on Linux

- Thunderbolt 3 Twin X DUO on Arch/Omarchy, kernel 7.1.5
- Ring-buffer connect path clocks (`SAMPLE_POS` advances)
- PipeWire Multichannel play/capture
- Discord voice with Apollo as default input/output

## Still open

- Twin X routing tables / DSP program blobs
- Preamp PAD/48V/mic-line relays (plugin-chain firmware)
- QUAD and Twin X Gen 2
- Optional host-specific Thunderbolt PCIe tunnel-at-boot helpers (kept out of this PR)

## Safe first contact

```bash
cd driver && make
sudo insmod ./ua_apollo.ko probe_only=1
sudo dmesg | tail -50
sudo rmmod ua_apollo
```

For automated checks: `sudo ./scripts/twinx-probe-test.sh` from the repo root
after building the module.
