#!/usr/bin/env python3
"""Sweep Apollo Solo USB mixer parameters via TCP:4710 for USB protocol RE.

Connects to UA Console on a Windows machine via TCP:4710, then systematically
sets each mixer parameter to known values with delays between changes.
Run USBPcap on the Windows machine simultaneously to capture the resulting
USB bulk OUT commands on the Apollo's DSP endpoint.

Each SET is logged with a microsecond timestamp so that the companion
pcap-mixer-decode.py script can correlate TCP commands with USB packets.

Usage:
    # On Windows: start USBPcap on the Apollo's USB bus
    # Then from Mac (or any machine that can reach the Windows box):
    python mixer-sweep.py --host 192.0.2.3 [--port 4710] [--delay 1.5]

    # Selective sweeps:
    python mixer-sweep.py --host 192.0.2.3 --only preamp-gain
    python mixer-sweep.py --host 192.0.2.3 --only phantom,monitor,mute

Protocol (TCP:4710):
    Commands:  "set <path> <value>\\0"
    Responses: '{"path":"<path>","data":<value>}\\0'
    Paths follow: /devices/0/inputs/<ch>/preamps/0/<control>/value
                  /devices/0/outputs/<ch>/<control>/value
"""

import argparse
import csv
import json
import os
import socket
import sys
import time

SWEEP_LOG_FILE = "sweep-log.csv"

# Apollo Solo USB layout (discovered via TCP:4710 GET):
#   Inputs:  0=ANALOG 1, 1=ANALOG 2 (both with preamps, HiZ-capable)
#            2-9=VIRTUAL 1-8
#   Outputs: 0=HP, 1=CUE 2, 2=CUE 3, 3=CUE 4, 4=MONITOR, 5=HP
#
# Monitor controls (CRMonitorLevel, Mute, DimOn, MixToMono) are on output 4.

SOLO_PREAMP_CHANNELS = [0, 1]
SOLO_MONITOR_OUTPUT = 4
SOLO_HP_OUTPUT = 0


class MixerClient:
    """TCP:4710 client for UA Console / Mixer Engine."""

    def __init__(self, host, port=4710, timeout=5.0):
        self.host = host
        self.port = port
        self.timeout = timeout
        self.sock = None
        self.buf = b""

    def connect(self):
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self.sock.settimeout(self.timeout)
        self.sock.connect((self.host, self.port))
        # Drain any initial data (welcome messages, subscription floods)
        self._drain(1.0)
        print(f"Connected to {self.host}:{self.port}")

    def close(self):
        if self.sock:
            self.sock.close()
            self.sock = None

    def send_set(self, path, value):
        """Send a SET command and return timestamp."""
        cmd = f"set {path} {value}\0"
        ts = time.time()
        self.sock.sendall(cmd.encode("utf-8"))
        return ts

    def send_get(self, path, recursive=False):
        """Send a GET command and read response."""
        query = "?recursive" if recursive else ""
        cmd = f"get {path}{query}\0"
        self.sock.sendall(cmd.encode("utf-8"))
        return self._read_responses(timeout=2.0)

    def _drain(self, timeout=0.5):
        """Drain pending data from socket."""
        self.sock.settimeout(timeout)
        try:
            while True:
                data = self.sock.recv(4096)
                if not data:
                    break
        except (socket.timeout, BlockingIOError):
            pass
        self.sock.settimeout(self.timeout)

    def _read_responses(self, timeout=2.0):
        """Read null-terminated responses until timeout."""
        responses = []
        self.sock.settimeout(timeout)
        try:
            while True:
                data = self.sock.recv(8192)
                if not data:
                    break
                self.buf += data
                while b"\x00" in self.buf:
                    raw, self.buf = self.buf.split(b"\x00", 1)
                    if raw:
                        try:
                            responses.append(json.loads(raw.decode("utf-8")))
                        except (json.JSONDecodeError, UnicodeDecodeError):
                            responses.append({"raw": raw.decode("utf-8", errors="replace")})
        except (socket.timeout, BlockingIOError):
            pass
        self.sock.settimeout(self.timeout)
        return responses


class SweepLogger:
    """CSV logger for sweep commands with timestamps."""

    def __init__(self, filepath):
        self.filepath = filepath
        self.entries = []
        self.f = open(filepath, "w", newline="")
        self.writer = csv.writer(self.f)
        self.writer.writerow(["timestamp", "elapsed_us", "group", "path", "value"])
        self.start_time = time.time()

    def log(self, group, path, value, ts):
        elapsed = int((ts - self.start_time) * 1_000_000)
        self.writer.writerow([f"{ts:.6f}", elapsed, group, path, value])
        self.f.flush()
        self.entries.append((ts, group, path, value))

    def close(self):
        self.f.close()
        print(f"Wrote {len(self.entries)} entries to {self.filepath}")


def sweep_preamp_gain(client, logger, delay):
    """Sweep preamp gain 0 -> 65 dB in 5 dB steps for each input."""
    group = "preamp-gain"
    for ch in SOLO_PREAMP_CHANNELS:
        path = f"/devices/0/inputs/{ch}/preamps/0/Gain/value"
        print(f"\n  [{group}] Input {ch}: 0 -> 65 dB (5 dB steps)")
        for gain_db in range(0, 66, 5):
            ts = client.send_set(path, float(gain_db))
            logger.log(group, path, gain_db, ts)
            print(f"    ch{ch} gain={gain_db} dB  t={ts:.6f}")
            time.sleep(delay)
        # Reset to default (10 dB)
        ts = client.send_set(path, 10.0)
        logger.log(group, path, 10, ts)
        time.sleep(delay * 2)


def sweep_phantom_power(client, logger, delay):
    """Toggle 48V phantom power on/off for each input."""
    group = "phantom"
    for ch in SOLO_PREAMP_CHANNELS:
        path = f"/devices/0/inputs/{ch}/preamps/0/48V/value"
        print(f"\n  [{group}] Input {ch}: toggle 48V")
        for val in [True, False, True, False]:
            ts = client.send_set(path, str(val).lower())
            logger.log(group, path, val, ts)
            print(f"    ch{ch} 48V={val}  t={ts:.6f}")
            time.sleep(delay)


def sweep_pad(client, logger, delay):
    """Toggle pad on/off for each input."""
    group = "pad"
    for ch in SOLO_PREAMP_CHANNELS:
        path = f"/devices/0/inputs/{ch}/preamps/0/Pad/value"
        print(f"\n  [{group}] Input {ch}: toggle Pad")
        for val in [True, False, True, False]:
            ts = client.send_set(path, str(val).lower())
            logger.log(group, path, val, ts)
            print(f"    ch{ch} Pad={val}  t={ts:.6f}")
            time.sleep(delay)


def sweep_hiz(client, logger, delay):
    """Toggle HiZ on/off for each input."""
    group = "hiz"
    for ch in SOLO_PREAMP_CHANNELS:
        path = f"/devices/0/inputs/{ch}/preamps/0/HiZ/value"
        print(f"\n  [{group}] Input {ch}: toggle HiZ")
        for val in [True, False]:
            ts = client.send_set(path, str(val).lower())
            logger.log(group, path, val, ts)
            print(f"    ch{ch} HiZ={val}  t={ts:.6f}")
            time.sleep(delay)


def sweep_lowcut(client, logger, delay):
    """Toggle LowCut on/off for each input."""
    group = "lowcut"
    for ch in SOLO_PREAMP_CHANNELS:
        path = f"/devices/0/inputs/{ch}/preamps/0/LowCut/value"
        print(f"\n  [{group}] Input {ch}: toggle LowCut")
        for val in [True, False]:
            ts = client.send_set(path, str(val).lower())
            logger.log(group, path, val, ts)
            print(f"    ch{ch} LowCut={val}  t={ts:.6f}")
            time.sleep(delay)


def sweep_phase(client, logger, delay):
    """Toggle Phase on/off for each input."""
    group = "phase"
    for ch in SOLO_PREAMP_CHANNELS:
        path = f"/devices/0/inputs/{ch}/preamps/0/Phase/value"
        print(f"\n  [{group}] Input {ch}: toggle Phase")
        for val in [True, False]:
            ts = client.send_set(path, str(val).lower())
            logger.log(group, path, val, ts)
            print(f"    ch{ch} Phase={val}  t={ts:.6f}")
            time.sleep(delay)


def sweep_iotype(client, logger, delay):
    """Switch Mic/Line/HiZ input type for each input."""
    group = "iotype"
    for ch in SOLO_PREAMP_CHANNELS:
        path = f"/devices/0/inputs/{ch}/IOType/value"
        print(f"\n  [{group}] Input {ch}: cycle input types")
        for val in ["mic", "line", "mic"]:
            ts = client.send_set(path, val)
            logger.log(group, path, val, ts)
            print(f"    ch{ch} IOType={val}  t={ts:.6f}")
            time.sleep(delay)


def sweep_monitor_level(client, logger, delay):
    """Sweep monitor level from -inf to 0 dB on output 4 (MONITOR)."""
    group = "monitor-level"
    path = f"/devices/0/outputs/{SOLO_MONITOR_OUTPUT}/CRMonitorLevel/value"
    print(f"\n  [{group}] Output {SOLO_MONITOR_OUTPUT} (MONITOR): -inf -> 0 dB")
    for db in [-144, -60, -48, -36, -24, -18, -12, -6, 0]:
        ts = client.send_set(path, float(db))
        logger.log(group, path, db, ts)
        print(f"    level={db} dB  t={ts:.6f}")
        time.sleep(delay)
    # Reset to safe level
    ts = client.send_set(path, -12.0)
    logger.log(group, path, -12, ts)
    time.sleep(delay)


def sweep_monitor_mute(client, logger, delay):
    """Toggle monitor mute on output 4 (MONITOR)."""
    group = "mute"
    path = f"/devices/0/outputs/{SOLO_MONITOR_OUTPUT}/Mute/value"
    print(f"\n  [{group}] Output {SOLO_MONITOR_OUTPUT} (MONITOR): toggle mute")
    for val in [True, False, True, False]:
        ts = client.send_set(path, str(val).lower())
        logger.log(group, path, val, ts)
        print(f"    Mute={val}  t={ts:.6f}")
        time.sleep(delay)


def sweep_dim(client, logger, delay):
    """Toggle DimOn on output 4 (MONITOR)."""
    group = "dim"
    path = f"/devices/0/outputs/{SOLO_MONITOR_OUTPUT}/DimOn/value"
    print(f"\n  [{group}] Output {SOLO_MONITOR_OUTPUT} (MONITOR): toggle dim")
    for val in [True, False]:
        ts = client.send_set(path, str(val).lower())
        logger.log(group, path, val, ts)
        print(f"    DimOn={val}  t={ts:.6f}")
        time.sleep(delay)


def sweep_mono(client, logger, delay):
    """Toggle MixToMono on output 4 (MONITOR)."""
    group = "mono"
    path = f"/devices/0/outputs/{SOLO_MONITOR_OUTPUT}/MixToMono/value"
    print(f"\n  [{group}] Output {SOLO_MONITOR_OUTPUT} (MONITOR): toggle mono")
    for val in [True, False]:
        ts = client.send_set(path, str(val).lower())
        logger.log(group, path, val, ts)
        print(f"    MixToMono={val}  t={ts:.6f}")
        time.sleep(delay)


def sweep_input_fader(client, logger, delay):
    """Sweep input fader levels."""
    group = "input-fader"
    for ch in SOLO_PREAMP_CHANNELS:
        path = f"/devices/0/inputs/{ch}/FaderLevel/value"
        print(f"\n  [{group}] Input {ch}: -inf -> 0 dB")
        for db in [-144, -60, -36, -18, -12, -6, 0]:
            ts = client.send_set(path, float(db))
            logger.log(group, path, db, ts)
            print(f"    ch{ch} fader={db} dB  t={ts:.6f}")
            time.sleep(delay)
        # Reset
        ts = client.send_set(path, 0.0)
        logger.log(group, path, 0, ts)
        time.sleep(delay)


def sweep_input_pan(client, logger, delay):
    """Sweep input pan L-R."""
    group = "input-pan"
    for ch in SOLO_PREAMP_CHANNELS:
        path = f"/devices/0/inputs/{ch}/Pan/value"
        print(f"\n  [{group}] Input {ch}: pan L -> R")
        for pan in [-1.0, -0.5, 0.0, 0.5, 1.0]:
            ts = client.send_set(path, pan)
            logger.log(group, path, pan, ts)
            print(f"    ch{ch} pan={pan}  t={ts:.6f}")
            time.sleep(delay)
        # Reset to center
        ts = client.send_set(path, 0.0)
        logger.log(group, path, 0.0, ts)
        time.sleep(delay)


def discover_device_tree(client):
    """GET the device tree to discover available paths."""
    print("\n=== Device Discovery ===")
    for path in ["/devices/0", "/devices/0/inputs", "/devices/0/outputs"]:
        responses = client.send_get(path, recursive=False)
        for r in responses:
            if "data" in r:
                print(f"  {r.get('path', path)}: {json.dumps(r['data'])[:120]}")


# All sweep functions keyed by name
SWEEPS = {
    "preamp-gain": sweep_preamp_gain,
    "phantom": sweep_phantom_power,
    "pad": sweep_pad,
    "hiz": sweep_hiz,
    "lowcut": sweep_lowcut,
    "phase": sweep_phase,
    "iotype": sweep_iotype,
    "monitor-level": sweep_monitor_level,
    "mute": sweep_monitor_mute,
    "dim": sweep_dim,
    "mono": sweep_mono,
    "input-fader": sweep_input_fader,
    "input-pan": sweep_input_pan,
}


def main():
    parser = argparse.ArgumentParser(
        description="Sweep Apollo Solo USB mixer params via TCP:4710")
    parser.add_argument("--host", required=True,
                        help="Windows VM IP running UA Console")
    parser.add_argument("--port", type=int, default=4710)
    parser.add_argument("--delay", type=float, default=1.5,
                        help="Seconds between SET commands (default: 1.5)")
    parser.add_argument("--only", type=str, default=None,
                        help=f"Comma-separated sweep names: {','.join(SWEEPS)}")
    parser.add_argument("--discover", action="store_true",
                        help="GET device tree before sweeping")
    parser.add_argument("--output", type=str, default=None,
                        help=f"CSV log file (default: {SWEEP_LOG_FILE})")
    args = parser.parse_args()

    log_path = args.output or os.path.join(os.path.dirname(__file__), SWEEP_LOG_FILE)

    # Select sweeps
    if args.only:
        names = [n.strip() for n in args.only.split(",")]
        for n in names:
            if n not in SWEEPS:
                print(f"Unknown sweep: {n}")
                print(f"Available: {', '.join(SWEEPS)}")
                sys.exit(1)
        selected = [(n, SWEEPS[n]) for n in names]
    else:
        selected = list(SWEEPS.items())

    client = MixerClient(args.host, args.port)
    try:
        client.connect()
    except Exception as e:
        print(f"Failed to connect to {args.host}:{args.port}: {e}")
        sys.exit(1)

    if args.discover:
        discover_device_tree(client)

    logger = SweepLogger(log_path)

    total = len(selected)
    print(f"\n=== Starting {total} sweep(s), delay={args.delay}s ===")
    print(f"    Log: {log_path}")
    print(f"    Start time: {time.time():.6f}")
    print(f"    Make sure USBPcap is running on the Windows VM!")

    # 3-second countdown to sync with USB capture start
    for i in range(3, 0, -1):
        print(f"    Starting in {i}...")
        time.sleep(1)

    marker_ts = client.send_set("/devices/0/inputs/0/preamps/0/Gain/value", 10.0)
    logger.log("marker", "/devices/0/inputs/0/preamps/0/Gain/value", 10.0, marker_ts)
    print(f"\n  [marker] Sync marker sent at t={marker_ts:.6f}")
    time.sleep(args.delay * 2)

    for i, (name, fn) in enumerate(selected):
        print(f"\n{'='*60}")
        print(f"  Sweep {i+1}/{total}: {name}")
        print(f"{'='*60}")
        fn(client, logger, args.delay)
        # Extra gap between sweep groups
        time.sleep(args.delay * 2)

    # Final sync marker
    marker_ts = client.send_set("/devices/0/inputs/0/preamps/0/Gain/value", 10.0)
    logger.log("marker", "/devices/0/inputs/0/preamps/0/Gain/value", 10.0, marker_ts)
    print(f"\n  [marker] Final sync marker at t={marker_ts:.6f}")

    logger.close()
    client.close()
    print("\nDone. Now copy the USBPcap .pcap file and run:")
    print(f"  python pcap-mixer-decode.py capture.pcap --sweep-log {log_path}")


if __name__ == "__main__":
    main()
