---
title: Daemon Setup
---

The mixer daemon (`ua_mixer_daemon.py`) bridges the kernel driver and network control clients. It serves two TCP protocols and an optional WebSocket endpoint, allowing UA Console, ConsoleLink, and custom clients to control the Apollo mixer without modification.

---

## What the daemon does

The daemon provides:

- **TCP:4710** — UA Mixer Engine protocol (null-terminated JSON text). Used by ConsoleLink (iOS/iPad).
- **TCP:4720** — UA Mixer Helper protocol (text commands, UBJSON binary responses). Used by UA Console / UA Connect.
- **WS:4721** — WebSocket server for UA Connect (Electron app).
- **State tree** — Maintains 11,000+ mixer controls mirroring the structure expected by UA client software.
- **Hardware router** — Translates state tree changes into ioctl calls to `/dev/ua_apollo0`.
- **Metering** — Computes per-channel audio level meters from ALSA PCM capture data.
- **Bonjour/mDNS** — Advertises `_uamixer._tcp.` so ConsoleLink auto-discovers the daemon.

---

## Python dependencies

The daemon requires Python 3.10+ and the following packages:

```bash
# Required
pip install websockets    # WebSocket server for UA Connect

# Optional (enable Bonjour auto-discovery)
pip install zeroconf

# Optional (enable ALSA metering)
pip install pyalsaaudio
```

The daemon runs without optional dependencies — WebSocket, Bonjour, and hardware metering degrade gracefully when packages are missing.

---

## Running the daemon

### Basic usage

```bash
cd mixer-engine
python3 ua_mixer_daemon.py
```

This starts the daemon with auto-detected device map and hardware. It binds to `0.0.0.0` on ports 4710, 4720, and 4721.

### With verbose logging

```bash
python3 ua_mixer_daemon.py -v
```

### Protocol trace (log every message)

```bash
python3 ua_mixer_daemon.py -v -t
```

### Software-only mode (no hardware)

For development and testing without an Apollo connected:

```bash
python3 ua_mixer_daemon.py --no-hardware --no-bonjour -v
```

This starts the full protocol server with simulated hardware. Clients can connect and interact with the state tree, but no ioctl calls are made.

### Custom device map

```bash
python3 ua_mixer_daemon.py --device-map device_maps/device_map_apollo_x4.json
```

### Custom ports

```bash
python3 ua_mixer_daemon.py --port 4711 --helper-port 4721 --ws-port 4722
```

---

## Systemd service

Create a service file for automatic startup:

```ini
# /etc/systemd/system/ua-mixer-daemon.service
[Unit]
Description=UA Mixer Engine Daemon
After=sound.target
Wants=sound.target

[Service]
Type=simple
User=root
WorkingDirectory=/opt/open-apollo/mixer-engine
ExecStart=/usr/bin/python3 ua_mixer_daemon.py -v
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
```

Enable and start:

```bash
sudo systemctl daemon-reload
sudo systemctl enable ua-mixer-daemon
sudo systemctl start ua-mixer-daemon
```

The daemon requires root (or appropriate permissions) to open `/dev/ua_apollo0` for ioctl communication with the kernel driver.

---

## Verifying it works

### Check daemon is listening

```bash
ss -tlnp | grep -E '4710|4720|4721'
```

### Test with the included test client

The repository includes a ConsoleLink simulator:

```bash
python3 test_client.py
```

This replays the exact command sequence that ConsoleLink sends on connection. Use `--host` to test against a remote daemon:

```bash
python3 test_client.py --host 192.168.1.100 -v
```

### Test with netcat

```bash
# Connect and send a GET request
echo -ne 'get /devices/0/DeviceOnline/value\0' | nc localhost 4710
```

You should receive a JSON response:

```json
{"path":"/devices/0/DeviceOnline/value","data":true}
```

### Check Bonjour advertisement

If `zeroconf` is installed, the daemon advertises itself. On another machine:

```bash
avahi-browse -r _uamixer._tcp
```

---

## Persistent state

The daemon saves modified control values to disk and restores them on restart. State files are stored in `mixer-engine/state/` alongside the device map name.

This means mixer settings (preamp gain, monitor volume, fader positions) survive daemon restarts.

---

## Next steps

- [Daemon Configuration](/docs/daemon-configuration) — all command-line flags and options
- [Protocol Reference](/docs/protocol-reference) — TCP and WebSocket protocol details
- [Architecture Overview](/docs/architecture-overview) — how the daemon fits into the system
