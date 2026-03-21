---
title: Sample Rates
---

The Apollo hardware supports six sample rates across three speed tiers. This page documents the supported rates, how to change them, clock source configuration, and important considerations for rate switching.

---

## Supported Sample Rates

| Rate | Tier | Rate Index |
|------|------|------------|
| 44,100 Hz | 1x (base) | 1 |
| 48,000 Hz | 1x (base) | 2 |
| 88,200 Hz | 2x (double) | 3 |
| 96,000 Hz | 2x (double) | 4 |
| 176,400 Hz | 4x (quad) | 5 |
| 192,000 Hz | 4x (quad) | 6 |

The default sample rate at driver load is **48,000 Hz**.

---

## How to Change Sample Rate

### Via ALSA Control

The driver exposes a `Sample Rate` ALSA control:

```bash
# View current rate
amixer -c Apollo cget name='Sample Rate'

# Set to 96 kHz (index 3, zero-based in ALSA enum)
amixer -c Apollo cset name='Sample Rate' 3
```

The ALSA enum values map as follows:

| ALSA Index | Rate |
|------------|------|
| 0 | 44,100 Hz |
| 1 | 48,000 Hz |
| 2 | 88,200 Hz |
| 3 | 96,000 Hz |
| 4 | 176,400 Hz |
| 5 | 192,000 Hz |

### Via PipeWire

PipeWire can request a specific rate when opening a stream. The driver handles rate changes in its PCM prepare callback. To set PipeWire's default rate:

```bash
mkdir -p ~/.config/pipewire/pipewire.conf.d
cat > ~/.config/pipewire/pipewire.conf.d/99-rate.conf <<EOF
context.properties = {
    default.clock.rate = 48000
    default.clock.allowed-rates = [ 44100 48000 88200 96000 176400 192000 ]
}
EOF
systemctl --user restart pipewire
```

With `default.clock.allowed-rates` set, PipeWire can automatically switch the hardware rate to match the content being played.

### Via the Mixer Daemon

The mixer daemon accepts sample rate changes through the TCP:4710 and WS:4720 control protocols, which are used by the console UI.

---

## Clock Source Configuration

The Apollo can synchronize to an internal clock or an external clock reference. The driver exposes two ALSA controls for clock configuration.

### Clock Source

Selects where the word clock comes from:

```bash
amixer -c Apollo cget name='Clock Source'
amixer -c Apollo cset name='Clock Source' 0  # Internal
```

| ALSA Index | Source | Description |
|------------|--------|-------------|
| 0 | Internal | Apollo's built-in crystal oscillator |
| 1 | S/PDIF | Lock to incoming S/PDIF signal |
| 2 | ADAT | Lock to incoming ADAT signal |
| 3 | Word Clock | Lock to external word clock input |

### Clock Mode

Selects between internal and external clocking modes:

```bash
amixer -c Apollo cget name='Clock Mode'
amixer -c Apollo cset name='Clock Mode' 0  # Internal
```

| ALSA Index | Mode |
|------------|------|
| 0 | Internal |
| 1 | External |

When using an external clock source, the Apollo locks to the incoming clock signal. If no valid external clock is detected, audio may not function correctly.

---

## How Rate Switching Works

Understanding the rate switching mechanism helps diagnose issues.

### Clock Configuration Register

The driver writes a combined clock configuration value to the hardware:

```
clock_config = clock_source | (rate_index << 8)
```

For example, 48 kHz with internal clock: `0x0C | (2 << 8)` = `0x020C`.

This value is written to the notification clock config register, followed by a clock doorbell write to signal the FPGA. The driver then waits up to 2 seconds for the DSP to acknowledge the rate change via the notification status register.

### Rate Change During Playback

When PipeWire or an ALSA application requests a different rate during playback, the driver performs a disconnect/reconnect cycle:

1. **Stop** the period timer
2. **Soft disconnect** the audio transport (stops DMA, preserves mixer DSP state)
3. **Update** the internal rate tracking
4. **Reconnect** the audio transport at the new rate
5. **Restart** transport and clock

This process takes a brief moment and will cause an audible gap. Applications should stop playback before switching rates when possible.

### Rate Change While Idle

When no streams are open, changing the rate via the ALSA control writes the new clock configuration immediately without a disconnect cycle.

---

## Channel Count at Higher Rates

The Apollo's PCIe DMA channel count is fixed regardless of sample rate. The driver always allocates the full channel count for your model (e.g., 24 playback / 22 capture on the Apollo x4).

However, at higher sample rates, the Apollo's internal DSP processing capacity is reduced. At 2x rates (88.2/96 kHz), some internal mixer channels may be unavailable. At 4x rates (176.4/192 kHz), the available DSP channels are further reduced.

The physical I/O availability at each tier depends on the specific Apollo model:

| Tier | Analog I/O | S/PDIF | ADAT |
|------|-----------|--------|------|
| 1x (44.1/48k) | All channels | Available | Available (8 channels) |
| 2x (88.2/96k) | All channels | Available | S/MUX (4 channels) |
| 4x (176.4/192k) | All channels | Available | Not available |

ADAT operates in S/MUX mode at double-speed rates, halving the available channel count. At quad-speed rates, ADAT is not available.

---

## Best Practices

- **Set the rate before opening streams.** Changing rates while audio is active causes a brief dropout.
- **Match your project rate.** If your DAW session is at 96 kHz, set the Apollo to 96 kHz before starting the session.
- **Use 48 kHz as default.** This is the most compatible rate and the driver's default. It provides the full channel count and lowest CPU overhead.
- **External clock requires a valid signal.** When switching to an external clock source, ensure the external device is connected and sending a valid clock at the expected rate before switching.
- **PipeWire rate switching** can be configured to automatically match content rates using `default.clock.allowed-rates`, but this triggers a full disconnect/reconnect cycle on each rate change.
