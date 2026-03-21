---
title: Configuration
---

This page covers all configuration options for the Open Apollo driver, including kernel module parameters, PipeWire/WirePlumber integration, UCM2 profiles, and systemd service setup.

---

## Module Parameters

The `ua_apollo` module accepts the following parameters, which can be set when loading the module with `insmod` or `modprobe`.

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `play_ch` | int | `-1` (auto) | Override playback channel count |
| `rec_ch` | int | `-1` (auto) | Override record channel count |
| `no_connect` | bool | `false` | Skip the ACEFACE DSP connect handshake at probe (for debugging) |
| `warm_boot` | bool | `false` | Skip firmware load and DSP connect; assume device is already initialized from a previous boot |

### Channel Count Override

By default, the driver auto-detects channel counts from the device model:

| Model | Playback | Capture | Preamps | HiZ |
|-------|----------|---------|---------|-----|
| Apollo Solo | 3 | 2 | 1 | 0 |
| Arrow | 3 | 2 | 1 | 0 |
| Apollo Twin X | 8 | 8 | 2 | 2 |
| Apollo x4 | 24 | 22 | 4 | 2 |
| Apollo x6 | 24 | 22 | 4 | 2 |
| Apollo x8 | 26 | 26 | 4 | 2 |
| Apollo x8p | 26 | 26 | 8 | 2 |
| Apollo x16 | 34 | 34 | 8 | 2 |

To override (e.g., to limit to stereo for testing):

```bash
sudo modprobe ua_apollo play_ch=2 rec_ch=2
```

If only `play_ch` is specified, `rec_ch` defaults to the same value. When overriding channel counts, preamp ALSA controls are disabled.

### Warm Boot Mode

If the Apollo was previously initialized by another operating system in a dual-boot configuration, you can skip firmware loading:

```bash
sudo modprobe ua_apollo warm_boot=1
```

This skips both firmware upload and the ACEFACE connection handshake, assuming the DSP is already running. The driver will still re-program the DMA scatter-gather table (since the previous OS's physical addresses are invalid) and create the ALSA card.

### Debug Mode (no_connect)

For crash debugging, you can load the driver without attempting the DSP connection:

```bash
sudo modprobe ua_apollo no_connect=1
```

The driver will probe the device, map registers, and create the chardev, but will not start audio transport. Audio will not work in this mode.

### Making Parameters Persistent

Create a modprobe configuration file:

```bash
sudo tee /etc/modprobe.d/ua-apollo.conf <<EOF
options ua_apollo play_ch=24 rec_ch=22
EOF
```

---

## PipeWire Configuration

### WirePlumber Rules

The project includes a WirePlumber configuration file that sets optimal defaults for Apollo interfaces. Deploy it to `/etc/wireplumber/main.lua.d/51-ua-apollo.lua`:

```bash
sudo cp configs/wireplumber/51-ua-apollo.lua /etc/wireplumber/main.lua.d/
```

This configuration applies the following properties to all Apollo ALSA nodes:

| Property | Value | Reason |
|----------|-------|--------|
| `api.alsa.disable-mmap` | `true` | Driver uses copy callbacks (no mmap support) |
| `session.suspend-timeout-seconds` | `0` | Keeps device open; avoids repeated transport resets |
| `audio.format` | `S32LE` | Native Apollo sample format (32-bit signed, little-endian) |
| `audio.rate` | `48000` | Default sample rate |
| `api.alsa.soft-mixer` | `true` | Use software volume; don't touch hardware monitor level |
| `node.nick` | `Apollo` | Friendly name in desktop audio settings |

### Pro-Audio Profile

WirePlumber does not auto-select the pro-audio profile. After PipeWire starts, set it manually:

```bash
# Find the Apollo device ID
wpctl status

# Set pro-audio profile (replace <id> with the device ID)
wpctl set-profile <id> pro-audio
```

To make this automatic, you can create a WirePlumber script or a systemd user service that runs after `wireplumber.service`.

---

## UCM2 Profiles

UCM2 (Use Case Manager 2) profiles tell ALSA and PipeWire about the device's channel layout and capabilities.

### Deployment

```bash
sudo mkdir -p /usr/share/alsa/ucm2/ua_apollo
sudo mkdir -p /usr/share/alsa/ucm2/conf.d/ua_apollo
sudo cp configs/ucm2/ua_apollo/ua_apollo.conf /usr/share/alsa/ucm2/ua_apollo/
sudo cp configs/ucm2/ua_apollo/HiFi.conf /usr/share/alsa/ucm2/ua_apollo/
sudo ln -sf ../../ua_apollo/ua_apollo.conf \
    /usr/share/alsa/ucm2/conf.d/ua_apollo/ua_apollo.conf
```

Or use the included deployment script:

```bash
sudo bash configs/deploy.sh
```

### Channel Layout (Apollo x4)

The HiFi profile exposes all DMA channels:

**Playback (24 channels):**

| DMA | Assignment | DMA | Assignment |
|-----|------------|-----|------------|
| 0-1 | Monitor L/R | 2-5 | Line 1-4 |
| 6-7 | S/PDIF L/R | 8-15 | Virtual 1-8 |
| 16-17 | CUE1 L/R | 18-19 | CUE2 L/R |
| 20-21 | CUE3 L/R | 22-23 | CUE4 L/R |

**Capture (22 channels):**

| DMA | Assignment | DMA | Assignment |
|-----|------------|-----|------------|
| 0-1 | Mic 1-2 | 2-3 | Line 3-4 |
| 4-5 | S/PDIF L/R | 6-13 | Virtual 1-8 |
| 14-15 | Monitor L/R | 16-17 | Aux1 L/R |
| 18-19 | Aux2 L/R | 20-21 | Talkback 1-2 |

---

## ALSA Controls

The driver registers ALSA mixer controls dynamically based on the detected model. Use `amixer` or `alsamixer` to view and modify them.

### List All Controls

```bash
amixer -c Apollo contents
```

### Per-Preamp Controls

For each preamp channel (e.g., 4 channels on Apollo x4), the following controls are created:

| Control Name | Type | Range | Description |
|-------------|------|-------|-------------|
| `Line N Capture Volume` | Integer | 10-65 (dB) | Preamp gain |
| `Mic N 48V Phantom Power Switch` | Boolean | 0/1 | Phantom power |
| `Line N Pad Switch` | Boolean | 0/1 | Input pad |
| `Line N Phase Invert Switch` | Boolean | 0/1 | Phase inversion |
| `Line N LowCut Switch` | Boolean | 0/1 | High-pass filter |
| `Line N Input Select` | Enum | Mic/Line | Input source |

### Monitor Controls

| Control Name | Type | Range | Description |
|-------------|------|-------|-------------|
| `Monitor Playback Volume` | Integer | 0-192 | Monitor output level |
| `Monitor Playback Switch` | Boolean | 0/1 | Monitor mute |
| `Monitor Dim Switch` | Boolean | 0/1 | Dim mode |
| `Monitor Dim Level` | Integer | 1-7 | Dim attenuation |
| `Monitor Mono Switch` | Boolean | 0/1 | Mono fold-down |
| `Monitor Source` | Enum | Mix/CUE1/CUE2 | Monitor source select |
| `Monitor Talkback Switch` | Boolean | 0/1 | Talkback on/off |

### Routing Controls

| Control Name | Type | Options | Description |
|-------------|------|---------|-------------|
| `Headphone 1 Source` | Enum | CUE1/CUE2 | HP1 output source |
| `Headphone 2 Source` | Enum | CUE1/CUE2 | HP2 output source |
| `CUE 1 Mono Switch` | Boolean | 0/1 | CUE1 mono fold-down |
| `CUE 2 Mono Switch` | Boolean | 0/1 | CUE2 mono fold-down |
| `CUE 1 Mix Switch` | Boolean | 0/1 | CUE1 mix enable |
| `CUE 2 Mix Switch` | Boolean | 0/1 | CUE2 mix enable |
| `CUE 1 Mirror Source` | Enum | — | CUE1 mirror output pair |
| `CUE 2 Mirror Source` | Enum | — | CUE2 mirror output pair |
| `CUE 1 Mirror Switch` | Boolean | 0/1 | CUE1 mirror enable |
| `CUE 2 Mirror Switch` | Boolean | 0/1 | CUE2 mirror enable |
| `Digital Mirror Switch` | Boolean | 0/1 | Digital output mirroring |

### Device Controls

| Control Name | Type | Options | Description |
|-------------|------|---------|-------------|
| `Digital Output Mode` | Enum | S/PDIF/ADAT | Digital output format |
| `Output Reference Level` | Enum | +4dBu/-10dBV | Line output reference |
| `Identify Switch` | Boolean | 0/1 | Identify/locate blink |
| `S/PDIF SRC Switch` | Boolean | 0/1 | Sample rate converter |
| `DSP Spanning Switch` | Boolean | 0/1 | DSP pairing mode |
| `Clock Mode` | Enum | Internal/External | Clock mode |
| `Sample Rate` | Enum | 44100-192000 | Device sample rate |
| `Clock Source` | Enum | Internal/S/PDIF/ADAT/Word Clock | Clock source |

### Example: Set Gain and Enable Phantom Power

```bash
# Set preamp 1 gain to 40 dB
amixer -c Apollo cset name='Line 1 Capture Volume' 40

# Enable 48V phantom power on channel 1
amixer -c Apollo cset name='Mic 1 48V Phantom Power Switch' 1

# Set monitor volume to -10 dB (raw value = 192 + (-10 * 2) = 172)
amixer -c Apollo cset name='Monitor Playback Volume' 172
```

---

## Systemd Service for the Mixer Daemon

The mixer daemon (`ua_mixer_daemon.py`) bridges the hardware driver with network control protocols (TCP:4710 and WS:4720). To run it as a system service:

### Create the Service File

```bash
sudo tee /etc/systemd/system/ua-mixer-daemon.service <<EOF
[Unit]
Description=Universal Audio Apollo Mixer Daemon
After=sound.target
Wants=sound.target
ConditionPathExists=/dev/ua_apollo0

[Service]
Type=simple
ExecStart=/usr/bin/python3 /opt/open-apollo/mixer-engine/ua_mixer_daemon.py -v
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
EOF
```

### Enable and Start

```bash
sudo systemctl daemon-reload
sudo systemctl enable ua-mixer-daemon.service
sudo systemctl start ua-mixer-daemon.service
```

### Check Status

```bash
sudo systemctl status ua-mixer-daemon.service
journalctl -u ua-mixer-daemon.service -f
```

### Running Without Hardware

For development or testing without the Apollo connected:

```bash
python3 mixer-engine/ua_mixer_daemon.py --no-hardware --no-bonjour -v
```

---

## Audio Routing Basics

The Apollo's internal mixer operates independently from the ALSA PCM streams. Audio routing is controlled through the mixer daemon, which translates high-level mix commands into hardware register writes.

### Signal Flow

```
Physical Inputs → DSP Mixer → Monitor Outputs
                            → Headphone Outputs
                            → S/PDIF / ADAT Outputs

DAW Playback (Virtual Inputs) → DSP Mixer → Outputs

Physical Inputs → DMA Capture → DAW Recording
```

### Key Concepts

- **Monitor outputs** are the main line outputs, controlled by the Monitor Volume/Mute/Dim controls
- **CUE mixes** are independent headphone/aux mixes that can be assigned to headphone outputs
- **Virtual inputs** are DAW playback channels routed through the DSP mixer before reaching outputs
- **The hardware monitor knob** controls the analog output level independently from PipeWire's software volume
