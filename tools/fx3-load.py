#!/usr/bin/env python3
"""Upload Cypress FX3 firmware to UA Apollo USB devices.

Reads a standard FX3 boot image (.bin with 'CY' magic) and uploads it
via USB vendor control transfers (bRequest=0xA0). After upload, sends
a jump to the firmware entry point causing the device to re-enumerate.

Usage: sudo python3 fx3-load.py ApolloSolo.bin
"""
import sys
import struct
import usb.core
import usb.util

# UA USB vendor ID and FX3 loader PIDs
UA_VID = 0x2B5A
FX3_LOADER_PIDS = {
    0x0001: "Apollo/Twin USB",
    0x0004: "Satellite USB",
    0x000C: "Arrow/Solo USB",
    0x000E: "Twin X USB",
}

# Other 2B5A PIDs seen in the Windows INFs (UAD2Usb.inf), recorded here only;
# this tool does not load firmware onto them:
#   0x0003  Twin USB, "updating" state (old-firmware Twin)
#   0x0005  UAD-2 Satellite USB QUAD  (DSP accelerator, no audio)
#   0x0006  UAD-2 Satellite USB OCTO  (DSP accelerator, no audio)

# FX3 vendor request for firmware download
FX3_FW_DOWNLOAD = 0xA0
MAX_CHUNK = 4096


def load_fx3_firmware(fw_path):
    """Parse and upload FX3 firmware image to the device."""
    # Find the FX3 loader device
    dev = None
    for pid, name in FX3_LOADER_PIDS.items():
        dev = usb.core.find(idVendor=UA_VID, idProduct=pid)
        if dev:
            print(f"Found {name} (VID={UA_VID:#06x} PID={pid:#06x})")
            break

    if dev is None:
        print("No UA FX3 loader device found. Is the Apollo powered on?")
        sys.exit(1)

    # Detach kernel driver if attached
    if dev.is_kernel_driver_active(0):
        dev.detach_kernel_driver(0)

    # Read firmware file
    with open(fw_path, "rb") as f:
        fw = f.read()

    # Validate FX3 image header
    if fw[0:2] != b"CY":
        print(f"Invalid FX3 image: expected 'CY' magic, got {fw[0:2]!r}")
        sys.exit(1)

    img_type = fw[3]
    if img_type != 0xB0:
        print(f"Warning: unexpected image type {img_type:#04x} (expected 0xB0)")

    print(f"Firmware: {fw_path} ({len(fw)} bytes, type={img_type:#04x})")

    # Parse and send firmware sections
    # Format after 4-byte header: repeated [length:4][address:4][data:length*4]
    # When length=0, address is the entry point (jump target)
    offset = 4
    section = 0
    total_sent = 0

    while offset < len(fw):
        if offset + 8 > len(fw):
            print(f"Truncated section header at offset {offset}")
            break

        length, address = struct.unpack_from("<II", fw, offset)
        offset += 8

        if length == 0:
            # Entry point — send jump command
            print(f"Jumping to entry point {address:#010x}")
            dev.ctrl_transfer(0x40, FX3_FW_DOWNLOAD, address & 0xFFFF, address >> 16, b"")
            print("Firmware upload complete! Device should re-enumerate.")
            return True

        # Length is in 32-bit words for some FX3 images, bytes for others
        # The CY header byte 2 bit pattern determines this
        # For UA firmware: appears to be in bytes based on file structure
        data_len = length * 4  # FX3 standard: length is in 32-bit words
        if offset + data_len > len(fw):
            # Try raw byte length
            data_len = length
            if offset + data_len > len(fw):
                print(f"Section {section} data exceeds file (offset={offset}, len={data_len})")
                break

        data = fw[offset:offset + data_len]
        offset += data_len

        # Send in chunks via vendor control transfer
        sent = 0
        while sent < len(data):
            chunk = data[sent:sent + MAX_CHUNK]
            addr = address + sent
            dev.ctrl_transfer(0x40, FX3_FW_DOWNLOAD,
                              addr & 0xFFFF, addr >> 16, chunk)
            sent += len(chunk)

        total_sent += len(data)
        section += 1
        print(f"  Section {section}: {len(data)} bytes -> {address:#010x}")

    print(f"Warning: reached end of file without entry point (sent {total_sent} bytes)")
    return False


if __name__ == "__main__":
    if len(sys.argv) != 2:
        print(f"Usage: sudo {sys.argv[0]} <firmware.bin>")
        sys.exit(1)
    load_fx3_firmware(sys.argv[1])
