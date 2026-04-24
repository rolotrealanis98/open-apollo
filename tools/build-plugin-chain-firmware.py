#!/usr/bin/env python3
"""Build ua-apollo-plugin-chain.bin from Windows DMA capture.

Reads the captured ring+dma JSON, extracts each DMA_REF payload,
and serializes to a firmware blob the Linux driver can load via
request_firmware() and substitute for the Windows physical
addresses that ua_plugin_chain.h currently carries verbatim.

Blob format (little-endian):
    Header (16 bytes):
        magic          : 'UAPC' (0x43504155)
        version        : 1
        entry_count    : u32
        data_offset    : u32    (offset to concatenated payloads)
    Entry table (12 bytes * entry_count):
        ring_idx       : u32    (index into ua_plugin_chain_data[])
        payload_bytes  : u32
        payload_offset : u32    (relative to start of blob)
    Payloads:
        concatenated, each 4-byte aligned

Usage:
    build-plugin-chain-firmware.py [input.json] [output.bin]

Defaults:
    input  = ../../apollo-linux/tools/captures/win-dsp0-ring-complete-20260319.json
    output = configs/firmware/ua-apollo-plugin-chain.bin
"""
import json
import os
import struct
import sys

MAGIC = 0x43504155  # 'UAPC'
VERSION = 1
HEADER_SIZE = 16
ENTRY_SIZE = 12

DEFAULT_JSON = os.path.join(
    os.path.dirname(__file__), '..', '..', 'apollo-linux',
    'tools', 'captures', 'win-dsp0-ring-complete-20260319.json'
)
DEFAULT_OUT = os.path.join(
    os.path.dirname(__file__), '..', 'configs',
    'firmware', 'ua-apollo-plugin-chain.bin'
)


def main():
    in_path = sys.argv[1] if len(sys.argv) > 1 else DEFAULT_JSON
    out_path = sys.argv[2] if len(sys.argv) > 2 else DEFAULT_OUT

    with open(in_path) as f:
        cap = json.load(f)

    ring = cap['ring']
    dma = cap['dma']

    # Validate: every DMA_REF in ring must have a payload in dma{}.
    dma_ref_idxs = []
    for i in range(len(ring) // 4):
        if ring[i * 4] & 0x80000000:
            dma_ref_idxs.append(i)
    dma_keys = set(int(k) for k in dma.keys())
    missing = set(dma_ref_idxs) - dma_keys
    if missing:
        sys.exit(f'ERROR: {len(missing)} DMA_REF entries lack payloads: '
                 f'{sorted(missing)[:10]}...')

    entries = []
    for idx in dma_ref_idxs:
        payload = dma[str(idx)]
        w0_size_dwords = ring[idx * 4] & 0x7FFFFFFF
        # Captured may exceed w0_size due to kd.exe output artifacts.
        # FPGA only fetches w0_size dwords.  Use max so we never lose
        # legitimate content (3 entries have non-zero bytes past w0_size).
        dwords = max(len(payload), w0_size_dwords)
        # Zero-pad captured payload to 'dwords' length
        padded = list(payload) + [0] * (dwords - len(payload))
        data = struct.pack(f'<{dwords}I', *padded)
        entries.append((idx, data))

    # Layout: header, entry table, payload data
    table_size = ENTRY_SIZE * len(entries)
    data_offset = HEADER_SIZE + table_size

    # Build payloads section with per-entry offsets
    data_bytes = bytearray()
    entry_table = bytearray()
    for ring_idx, data in entries:
        payload_offset = data_offset + len(data_bytes)
        entry_table.extend(struct.pack('<III',
                                       ring_idx, len(data), payload_offset))
        data_bytes.extend(data)
        # 4-byte align (should already be — safety)
        while len(data_bytes) & 3:
            data_bytes.append(0)

    header = struct.pack('<IIII', MAGIC, VERSION,
                         len(entries), data_offset)

    blob = header + bytes(entry_table) + bytes(data_bytes)

    os.makedirs(os.path.dirname(out_path), exist_ok=True)
    with open(out_path, 'wb') as f:
        f.write(blob)

    nonzero = sum(1 for _, d in entries if any(d))
    print(f'wrote {out_path}')
    print(f'  entries    : {len(entries)}')
    print(f'  non-zero   : {nonzero}')
    print(f'  all-zero   : {len(entries) - nonzero}')
    print(f'  blob size  : {len(blob)} bytes')
    print(f'  header     : {HEADER_SIZE} bytes')
    print(f'  entry tbl  : {table_size} bytes')
    print(f'  payloads   : {len(data_bytes)} bytes')


if __name__ == '__main__':
    main()
