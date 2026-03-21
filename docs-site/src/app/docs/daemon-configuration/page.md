---
title: Daemon Configuration
---

The mixer daemon is configured entirely through command-line flags. There is no configuration file.

---

## Command-line reference

```bash
python3 ua_mixer_daemon.py [OPTIONS]
```

### Device and hardware

| Flag | Default | Description |
|---|---|---|
| `--device-map`, `-m` | Auto-detect | Path to device map JSON file |
| `--device`, `-d` | Auto-detect `/dev/ua_apollo*` | Device node path for hardware I/O |
| `--no-hardware` | (disabled) | Software-only mode — no hardware writes, no device node required |
| `--safe-mode` | (disabled) | Block unmapped DSP writes for safety |

### Network binding

| Flag | Default | Description |
|---|---|---|
| `--host` | `0.0.0.0` | Bind address for all servers |
| `--port`, `-p` | `4710` | TCP port for Mixer Engine (text protocol) |
| `--helper-port` | `4720` | TCP port for Mixer Helper (UBJSON protocol) |
| `--ws-port` | `4721` | WebSocket port for UA Connect |
| `--no-helper` | (disabled) | Disable the Mixer Helper TCP:4720 server |
| `--no-ws` | (disabled) | Disable the WebSocket server |

### Service discovery

| Flag | Default | Description |
|---|---|---|
| `--no-bonjour` | (disabled) | Disable Bonjour/mDNS service advertisement |
| `--name`, `-n` | Hostname | Bonjour service name (displayed in ConsoleLink device list) |

### Metering

| Flag | Default | Description |
|---|---|---|
| `--alsa-device` | Auto-detect | ALSA capture device for software metering (e.g., `hw:2,0`) |

### Logging

| Flag | Default | Description |
|---|---|---|
| `--verbose`, `-v` | (disabled) | Enable debug-level logging |
| `--trace`, `-t` | (disabled) | Log every protocol message sent and received |

---

## Network binding

By default the daemon binds to `0.0.0.0` (all interfaces). To restrict to localhost:

```bash
python3 ua_mixer_daemon.py --host 127.0.0.1
```

To use non-standard ports:

```bash
python3 ua_mixer_daemon.py --port 4711 --helper-port 4721 --ws-port 4722
```

---

## Hardware backend

### Auto-detection

When `--device` is not specified, the daemon scans for `/dev/ua_apollo*` device nodes. The first match is used.

### Software-only mode

```bash
python3 ua_mixer_daemon.py --no-hardware
```

In this mode:

- No device node is opened
- The full state tree is loaded from the device map
- Clients can connect and interact with controls
- SET commands update the state tree but do not reach hardware
- Metering returns silence (-77 dBFS)

This is useful for:

- Protocol development and testing
- UI development without hardware
- Running the daemon on a development machine (e.g., laptop without Thunderbolt)

### Safe mode

```bash
python3 ua_mixer_daemon.py --safe-mode
```

Safe mode blocks DSP writes that don't have a verified mapping. This prevents accidental writes to unknown registers during development.

---

## Bonjour / mDNS discovery

The daemon advertises `_uamixer._tcp.` via Bonjour/mDNS when the `zeroconf` Python package is installed. This allows ConsoleLink (iOS/iPad) to auto-discover the daemon on the local network.

```bash
# Install the optional dependency
pip install zeroconf
```

The service name defaults to the machine hostname. Override it:

```bash
python3 ua_mixer_daemon.py --name "Studio Apollo"
```

To disable advertisement entirely:

```bash
python3 ua_mixer_daemon.py --no-bonjour
```

---

## Logging

### Levels

| Flag | Level | Output |
|---|---|---|
| (none) | INFO | Connections, hardware events, errors |
| `-v` | DEBUG | All of the above plus control changes, state tree operations |
| `-v -t` | DEBUG + trace | All of the above plus every protocol message (raw bytes) |

### Format

Log lines include timestamp, logger name, level, and message:

```
14:32:05 ua-mixer-daemon INFO TCP:4710 (Mixer Engine, text) listening on [('0.0.0.0', 4710)]
14:32:05 ua-mixer-daemon INFO Device: Apollo x4 (11244 controls)
14:32:10 ua-mixer-daemon INFO Connected: <MixerClient client-1 :4710 text ('192.168.1.50', 49832)>
```

---

## ALSA metering

The daemon computes audio meters in software by reading PCM samples from the ALSA capture device. This requires:

1. The `pyalsaaudio` Python package
2. The kernel driver loaded with audio transport running
3. A valid ALSA capture device

Auto-detection scans `/proc/asound/cards` for the `ua_apollo` driver. Override with:

```bash
python3 ua_mixer_daemon.py --alsa-device hw:2,0
```

If ALSA metering is unavailable, meters report silence (-77 dBFS) for all channels.

---

## Device maps

The daemon loads a device map JSON file that defines the complete state tree structure. This file describes all controls, their types, ranges, and default values.

Default location: `mixer-engine/device_maps/device_map_apollo_x4.json`

The device map is a capture of the state tree from a working UA Mixer Engine. Different Apollo models require different device maps.

---

## Persistent state

The daemon automatically saves modified control values to `mixer-engine/state/<device_map_name>.state.json`. On restart, saved values are restored.

This preserves:

- Preamp gain, 48V, pad, low-cut, phase settings
- Monitor volume, mute, dim, source selection
- Fader positions and pan values
- Headphone routing and volume

---

## Further reading

- [Daemon Setup](/docs/daemon-setup) — getting started with the daemon
- [Protocol Reference](/docs/protocol-reference) — protocol wire formats
