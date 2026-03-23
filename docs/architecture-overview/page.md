---
title: Architecture Overview
---

Open Apollo consists of two main components that work together to provide full mixer control over Universal Audio Apollo Thunderbolt interfaces on Linux.

---

## System diagram

```
┌─────────────────────┐  ┌─────────────────────┐
│   Control Client    │  │   Control Client    │
│   (any TCP/WS app)  │  │   (any TCP/WS app)  │
└────────┬────────────┘  └────────┬────────────┘
         │ TCP:4710               │ TCP:4720
         │                        │
         └────────────┬───────────┘
                      │
         ┌────────────▼─────────────┐
         │   Mixer Daemon           │
         │   (ua_mixer_daemon.py)   │
         │                          │
         │   - State Tree           │
         │   - Hardware Router      │
         │   - Protocol Server      │
         └──────────┬───────────────┘
                    │ ioctl
         ┌──────────▼───────────────┐
         │   Kernel Driver          │
         │   (ua_apollo.ko)         │
         │                          │
         │   - ALSA PCM + Mixer     │
         │   - DSP Ring Buffer      │
         │   - DMA Engine           │
         └──────────┬───────────────┘
                    │ PCIe MMIO
         ┌──────────▼───────────────┐
         │   Apollo Hardware        │
         │   (BAR0 Registers)       │
         │                          │
         │   - DSP / FPGA           │
         │   - ARM MCU              │
         │   - Preamp (PGA2500)     │
         │   - DMA Buffers          │
         └──────────────────────────┘
```

---

## Components

### Kernel driver (`driver/ua_apollo.ko`)

The Linux kernel module handles all direct hardware communication. It provides:

- **PCIe device management**: Probe, initialization, BAR0 register access
- **ALSA integration**: PCM playback/capture streams and mixer controls (50 controls on Apollo x4)
- **DMA engine**: Manages playback and capture ring buffers for audio data transfer
- **DSP ring buffer**: Command interface for sending configuration to the onboard DSP
- **Firmware loading**: Automatic firmware loading via Linux `request_firmware()`
- **ioctl interface**: Exposes `/dev/ua_apollo0` for userspace daemon communication

The driver communicates with hardware exclusively through memory-mapped I/O (MMIO) reads and writes to BAR0 registers.

### Mixer daemon (`mixer-engine/ua_mixer_daemon.py`)

The userspace daemon is the control plane. It translates high-level mixer operations into low-level hardware commands:

- **TCP server (port 4710)**: Mixer Engine protocol (null-terminated JSON text)
- **TCP server (port 4720)**: Mixer Helper protocol (text commands, UBJSON binary responses)
- **State tree**: Maintains a hierarchical tree of all mixer controls (11,000+ nodes for Apollo x4)
- **Hardware router**: Translates state tree changes into ioctl calls to the kernel driver
- **Metering**: Computes audio level meters from PCM sample data

The daemon implements the same network protocols that UA's own software uses, so it's compatible with third-party tools that speak those protocols.

---

## Data flow

### Audio playback

1. Application sends PCM audio via ALSA
2. Driver writes samples to DMA playback ring buffer
3. FPGA reads from ring buffer and routes to DAC / monitor outputs
4. DSP applies mixer routing (faders, panning, sends)

### Audio capture

1. ADC samples are routed through the DSP mixer (applying monitoring, routing)
2. FPGA writes processed samples to DMA capture ring buffer
3. Driver reads from ring buffer and delivers to ALSA
4. Application receives PCM audio

### Mixer control

1. User adjusts a control (e.g., preamp gain) in a client application
2. Client sends command via TCP or WebSocket to daemon
3. Daemon updates state tree and determines required hardware change
4. Daemon sends ioctl to kernel driver
5. Driver writes to appropriate BAR0 register or DSP ring buffer command
6. DSP / ARM MCU processes the change (e.g., adjusts PGA2500 gain relay)
7. Driver reads back hardware state and confirms the change

### DSP settings

Mixer settings (monitor volume, preamp flags, routing coefficients) are written to DSP shared memory (SRAM) in a batch:

1. All settings are cached and written together
2. A single sequence counter bump signals the DSP to process the batch
3. The DSP reads the settings, applies them, and increments its own counter to acknowledge

This batch protocol is critical — writing individual settings without the proper sequence handshake can crash the DSP.

---

## Hardware write paths

The driver uses four distinct paths to write to hardware, depending on what is being controlled:

| Path | Mechanism | Used for |
|---|---|---|
| DSP settings (SRAM) | Direct BAR0 register write | Monitor volume, mute, dim, preamp flags |
| DSP ring buffer | Command queue to DSP | Mixer bus coefficients, routing, module activation |
| ARM CLI | Text command to ARM MCU | Preamp gain (PGA2500 SPI), identify LED |
| Clock register | BAR0 register write | Sample rate changes |

---

## Further reading

- [Register Map](/docs/register-map) — BAR0 register layout
- [DSP Protocol](/docs/dsp-protocol) — DSP command ring buffer details
- [Protocol Reference](/docs/protocol-reference) — TCP and WebSocket protocol documentation
