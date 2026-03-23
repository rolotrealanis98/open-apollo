#!/usr/bin/env python3
"""
capture.py -- Read Apollo device registers on Windows via WinRing0

This script reads PCIe configuration space and BAR0 status registers from
a Universal Audio Apollo interface. It makes ZERO writes to hardware.
All output is saved to a local JSON file.

Prerequisites:
    - WinRing0 installed (run setup.ps1 first)
    - Python 3.8+
    - Run as Administrator

Usage:
    python capture.py [--output path/to/capture.json]
"""

import argparse
import ctypes
import ctypes.wintypes
import json
import os
import platform
import struct
import sys
import urllib.request
import urllib.error
from datetime import datetime, timezone

TELEMETRY_URL = "https://open-apollo-api.rolotrealanis.workers.dev/captures"


# ============================================================================
# WinRing0 interface
# ============================================================================

# TODO: These are placeholder definitions. The actual WinRing0 DLL interface
# needs to be verified against the installed version. WinRing0 provides:
#
#   - ReadPciConfigDword(bus, dev, func, offset) -> value
#   - MapPhysicalMemory(base, size) -> pointer  (for BAR0 reads)
#   - UnmapPhysicalMemory(pointer, size)
#
# The DLL is typically WinRing0x64.dll (64-bit) or WinRing0.dll (32-bit).

WINRING0_DLL = "WinRing0x64.dll" if platform.machine().endswith("64") else "WinRing0.dll"

# Universal Audio PCI vendor/device IDs
UA_VENDOR_ID = 0x1A00  # Universal Audio
UA_DEVICE_IDS = {
    0x0015: "Apollo Twin",
    0x0017: "Apollo x4",
    0x0019: "Apollo x6",
    0x001B: "Apollo x8",
    0x001D: "Apollo x8p",
    0x001F: "Apollo x16",
}

# BAR0 register offsets for read-only status (no writes)
BAR0_REGISTERS = {
    "firmware_version": 0x0004,
    "device_type":      0x0008,
    "serial_number":    0x000C,
    "dsp_status":       0x0040,
    "seq_rd":           0x0018,  # DSP sequence read counter
    "sample_rate":      0x0050,
}


def find_apollo_pci():
    """Scan PCI bus for Universal Audio devices.

    Returns (bus, dev, func) tuple or None if not found.
    """
    # TODO: Implement PCI bus scan using WinRing0's ReadPciConfigDword.
    # Scan bus 0-255, device 0-31, function 0-7 for UA_VENDOR_ID.
    #
    # For each (bus, dev, func):
    #   vendor = ReadPciConfigDword(bus, dev, func, 0x00) & 0xFFFF
    #   device = (ReadPciConfigDword(bus, dev, func, 0x00) >> 16) & 0xFFFF
    #   if vendor == UA_VENDOR_ID:
    #       return (bus, dev, func, device)

    print("ERROR: WinRing0 PCI scan not yet implemented.", file=sys.stderr)
    print("This is a template script. See TODO comments in source.", file=sys.stderr)
    return None


def read_pci_config(bus, dev, func):
    """Read PCI configuration space (256 bytes, read-only).

    Returns dict with parsed config fields.
    """
    # TODO: Read first 64 bytes of PCI config space using WinRing0.
    # Key fields:
    #   0x00: Vendor ID (2B) + Device ID (2B)
    #   0x04: Command (2B) + Status (2B)
    #   0x08: Revision + Class code
    #   0x10-0x24: BAR0-BAR5 (base address registers)
    #   0x2C: Subsystem Vendor ID + Subsystem Device ID

    return {
        "vendor_id": "TODO",
        "device_id": "TODO",
        "bars": [],
        "subsystem": "TODO",
    }


def read_bar0_snapshot(bar0_base):
    """Read key status registers from BAR0 (read-only).

    Maps physical memory at bar0_base and reads known status registers.
    NO writes are performed.
    """
    # TODO: Use WinRing0's MapPhysicalMemory to map BAR0, then read
    # each register offset defined in BAR0_REGISTERS.
    #
    # Example (pseudocode):
    #   handle = MapPhysicalMemory(bar0_base, 0x1000)
    #   for name, offset in BAR0_REGISTERS.items():
    #       value = read_dword(handle + offset)
    #       results[name] = hex(value)
    #   UnmapPhysicalMemory(handle, 0x1000)

    return {name: "TODO" for name in BAR0_REGISTERS}


# ============================================================================
# Main
# ============================================================================

def main():
    parser = argparse.ArgumentParser(
        description="Capture Apollo device info on Windows (read-only)"
    )
    parser.add_argument(
        "--output",
        default=f"./open-apollo-report-{datetime.now().strftime('%Y%m%d')}.json",
        help="Output JSON file path",
    )
    parser.add_argument(
        "--auto-submit",
        action="store_true",
        help="Automatically send report without prompting (non-interactive)",
    )
    args = parser.parse_args()

    print()
    print("Open Apollo -- Windows Device Capture")
    print("======================================")
    print()
    print("This script reads hardware identifiers and status registers from")
    print("your Apollo interface. It makes ZERO writes to hardware.")
    print()

    # Check admin
    try:
        is_admin = ctypes.windll.shell32.IsUserAnAdmin()
    except AttributeError:
        is_admin = False

    if not is_admin:
        print("ERROR: This script must be run as Administrator.")
        print("Right-click Command Prompt or PowerShell -> Run as Administrator")
        sys.exit(1)

    # Check WinRing0
    dll_path = os.path.join(
        os.environ.get("ProgramFiles", r"C:\Program Files"),
        "OpenApollo", "WinRing0", WINRING0_DLL,
    )
    if not os.path.exists(dll_path):
        print(f"ERROR: WinRing0 not found at {dll_path}")
        print("Run setup.ps1 first to install WinRing0.")
        sys.exit(1)

    # Find Apollo on PCI bus
    print("Scanning PCI bus for Universal Audio device...")
    result = find_apollo_pci()
    if result is None:
        print()
        print("No Universal Audio device found on PCI bus.")
        print("Make sure your Apollo is connected via Thunderbolt and powered on.")
        sys.exit(1)

    bus, dev, func, device_id = result
    model = UA_DEVICE_IDS.get(device_id, f"Unknown (0x{device_id:04X})")
    print(f"Found: {model} at PCI {bus:02X}:{dev:02X}.{func}")

    # Read PCI config
    print("Reading PCI configuration space...")
    pci_config = read_pci_config(bus, dev, func)

    # Read BAR0 registers
    print("Reading BAR0 status registers...")
    bar0_data = read_bar0_snapshot(pci_config.get("bars", [None])[0])

    # Build report
    report = {
        "report_version": "1.0",
        "platform": "windows",
        "generated_at": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "system": {
            "os": platform.platform(),
            "arch": platform.machine(),
            "python": platform.python_version(),
        },
        "device": {
            "model": model,
            "pci_location": f"{bus:02X}:{dev:02X}.{func}",
            "vendor_id": f"0x{UA_VENDOR_ID:04X}",
            "device_id": f"0x{device_id:04X}",
        },
        "pci_config": pci_config,
        "bar0_registers": bar0_data,
    }

    # Write output
    with open(args.output, "w") as f:
        json.dump(report, f, indent=2)

    print()
    print(f"Report saved to: {args.output}")
    print()

    # Telemetry — opt-in upload
    answer = "n"
    if args.auto_submit:
        answer = "y"
    else:
        try:
            answer = input("Help improve Open Apollo — send this device report anonymously? [y/N] ")
        except EOFError:
            answer = "y"  # Non-interactive — auto-send

    if answer.strip().lower().startswith("y"):
        print(f"Sending report to {TELEMETRY_URL}...")
        try:
            with open(args.output, "rb") as f:
                data = f.read()
            req = urllib.request.Request(
                TELEMETRY_URL,
                data=data,
                headers={"Content-Type": "application/json"},
                method="POST",
            )
            with urllib.request.urlopen(req, timeout=15) as resp:
                if resp.status in (200, 201):
                    print("Report sent — thank you!")
                else:
                    print(f"Upload failed (HTTP {resp.status}) — report saved locally at {args.output}")
        except (urllib.error.URLError, OSError) as e:
            print(f"Upload failed ({e}) — report saved locally at {args.output}")
    else:
        print(f"No data sent. Report saved locally at {args.output}")

    print()
    print("To also submit manually:")
    print("  1. Review the file -- it contains only hardware identifiers, no personal data")
    print("  2. Go to: https://github.com/open-apollo/open-apollo/issues/new?template=device-report.yml")
    print("  3. Attach the JSON file or paste its contents")
    print("  4. Add notes about your Apollo model and what works/doesn't work")
    print()


if __name__ == "__main__":
    main()
