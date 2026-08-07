# Apollo Twin X DUO — measured results on Linux

Hardware: Apollo Twin X DUO, Thunderbolt 3, on a GIGABYTE GC-TITAN RIDGE
(JHL7540) in an Arch/Omarchy host, kernel 7.1.5-arch1-2, GCC 16.1.1.
PCI address `0000:3e:00.0`. IOMMU **inactive** (`iommu=pt` unset, 0 groups).

All runs used manual `insmod` only — no DKMS, no `modules-load.d`, no
initramfs changes, no `install.sh`. Module never left resident.

## Identification (warm and cold — identical)

```
model        : Apollo Twin X
device_type  : 0x23
subsys id    : 0x0019
FPGA rev     : 0xae14c625
firmware     : v2 (extended)
SHARC DSPs   : 2
serial raw   : '24032024227567'
BAR0         : 0xb7100000 size 0x40000
```

Cross-validated against macOS IOKit on the same unit (`device_type`, subsystem
ID and DSP count all agree).

### The serial table misidentifies this unit

Serial digits 5–8 are **`2024`**, which `ua_serial_table` maps to
`UA_DEV_APOLLO_SOLO` (`0x27`) — the Twin X entry is `2020`. Unpatched, this unit
identifies as a Solo, which would give:

- `device_type 0x27` = 39, outside `UA_RING_CONNECT_BITMASK_HI` (bits 32–38), so
  `ua_uses_audio_extension()` returns true → **wrong connect path**
- `play=3 / rec=2` instead of `10/16`

The `UA_SUBSYS_APOLLO_TWIN_X_DUO 0x0019` pin is what yields the correct `0x23`.
This is hardware evidence that subsystem-ID pinning is required, not cosmetic.

## Transport clocks — warm *and* cold

Loaded `no_connect=1 no_plugins=1`.

```
=== START TRANSPORT ===
  after start: CTRL=0x0000020f SAMPLE_POS=0x00000000 FRAME_CTR=0x00000008
  after 100us: SAMPLE_POS=0x00000005 FRAME_CTR=0x0000000d
transport stopped: AX_CTRL=0x00000000 SP=0x0000000b   (warm)
transport stopped: AX_CTRL=0x00000000 SP=0x0000000d   (cold, later 0x10)
```

5 samples per 100 µs is 4.8 expected at 48 kHz — real-time rate, not a stray
increment. Reproduced across several prepare/start/stop cycles per load, in both
directions (`play` prepares at 10 ch, `rec` at 16 ch).

**Upstream issue #46 reports that ring-connect devices never clock** — the x8p
(`0x22`, same path) sits with `SAMPLE_POS` and `FRAME_CTR` frozen at 0. This is
therefore the first ring-connect Apollo observed clocking on Linux, as far as the
issue tracker records.

### Cold really was cold

| | warm (after macOS) | cold (after power cycle) |
|---|---|---|
| `pre-init` | `SEQ_WR=10 SEQ_RD=10` | `SEQ_WR=0 SEQ_RD=0` |
| DSP branch | `mixer DSP alive! Skipping DMA reset` | absent → `dsp_was_dead`, full `ua_program_registers()` |
| `DMA_CTRL` before engine enable | `0x000001ff` | `0x00000001` |

### It clocks with no firmware at all

```
mixer firmware 'ua-apollo-mixer.bin' not found in /lib/firmware/
plugin chain firmware 'ua-apollo-plugin-chain.bin' not found; DMA_REF entries
  will be skipped (preamp PAD/48V/MicLine relays will not work)
```

`configs/firmware/` does not exist in this repo, so neither blob is available —
only the plugin-chain one has a generator
(`tools/build-plugin-chain-firmware.py`). So transport clocking does **not**
depend on DSP firmware. Firmware will matter for mixing, routing, and the preamp
relays, not for the sample counter advancing.

## The DMA_CTRL reset-strobe latch is real

Predicted from code reading, then confirmed on hardware on both paths:

```
DMA_CTRL before engine enable:    0x00000001   (cold)
DMA_CTRL after engine enable:     0x0001ffff   <- strobes [16:9] SET
DMA_CTRL after ring enable pulse: 0x0001ffff   <- still set; not pulsed
...
settles to                        0x000001ff
```

`ua_dsp_rings_init()` writes `dma | 0x1FFFE`, caches the readback (strobes now
set), then "pulses" by writing that same cached value back — so the deassert
never happens. The strobes do **not** auto-clear, which was the open question.

Both paths recover, by different routes:
- **cold** — `ua_reset_dma()` (via `ua_program_registers()`) brings it to `0x1ff`
  before `POST-CONNECT`
- **warm** — the `DMA_CTRL stale 0x0001ffff -> 0x000001ff (reset strobes
  cleared)` fixup in the prepare-transport path

The recovery is why transport clocks here. The latch remains a live defect in
`ua_dsp_rings_init()`, and `ua_audio_reset_dma()` in `ua_audio.c` — whose comment
describes exactly this bug as a "BUG FIX" — is still `__maybe_unused` and called
from nowhere.

## PCIe endpoint appears cold

Returned at `0000:3e:00.0` roughly 22 s after power-on, no Mac involved. This
**disproves** the earlier hypothesis in this branch that the Apollo withholds its
PCIe function until firmware is uploaded. Whatever left the endpoint missing at
the very start of this work, it was not that, and it was not VT-d either
(enumeration and BAR0 reads both succeed with the IOMMU inactive).

## Stability

Across all runs — `probe_only`, warm normal load, cold normal load — there were
no oopses, no `BUG:`, no call traces, and no PCIe uncorrectable errors. Every
load unloaded cleanly and the ALSA card was removed each time. The Apollo needed
no recovery beyond the one deliberate power cycle.

## Linux audio (follow-up — verified)

After the identification/transport work above, the same Twin X DUO has been used
for real PipeWire playback and capture on Arch/Omarchy (kernel 7.1.5-arch1-2,
PipeWire 1.6.8):

- ALSA/PipeWire exposes **Apollo Twin X Multichannel** (`s32le` 10ch play /
  16ch capture @ 48 kHz)
- Desktop session defaults to Apollo for input and output
- **Discord** official Linux client voice calls work with Apollo as the mic/speaker

This upgrades the device beyond transport-counter observation. Remaining gaps
below still apply for full mixer/preamp parity with the x4.

## What is NOT established

- **No routing config exists for `0x23`.** `ua_get_routing_config()` returns
  `NULL`, so UA-style channel labels/routing are incomplete (raw Multichannel
  I/O still works for apps that can use it, including Discord).
- Preamp relays (PAD / 48V / mic-line) cannot work without the plugin-chain blob.
- Nothing here is verified for the **QUAD** variant or **Twin X Gen 2** (`0x3A`).
- IOMMU was off for the early transport runs. Enabling `iommu=pt` remains the
  safer long-term configuration on VT-d hosts.
