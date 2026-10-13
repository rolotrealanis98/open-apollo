#!/usr/bin/env python3
"""Source-backed enable-only CLI experiment. No command text or firmware upload.

Vendor CPcieAudioExtension::EnableCLI (arm64 0x4f124): C3F4 <- enable,
C3F8 <- 0, C3FC <- 0. Linux raw register writes require CAP_SYS_RAWIO.
This deliberately uses a separately authorized diagnostic path; it does not
change the driver's default CLI refusal. Device/driver ownership stays intact.
"""
import argparse
import fcntl
import json
import os
from pathlib import Path
import struct
import time

ENABLE, COMMAND_LENGTH, RESPONSE_LENGTH = 0xC3F4, 0xC3F8, 0xC3FC
RESPONSE_BUFFER = 0xC480
CONTROL = (ENABLE, COMMAND_LENGTH, RESPONSE_LENGTH)
READ_REG, WRITE_REG = 0xC0085510, 0x40085511


class Registers:
    def __init__(self, writable):
        self.fd = os.open('/dev/ua_apollo0', (os.O_RDWR if writable else os.O_RDONLY) | os.O_CLOEXEC)

    def read(self, offset):
        if offset % 4 or (offset not in CONTROL and not RESPONSE_BUFFER <= offset <= RESPONSE_BUFFER + 296):
            raise ValueError('register outside diagnostic allowlist')
        data = bytearray(struct.pack('<II', offset, 0))
        fcntl.ioctl(self.fd, READ_REG, data, True)
        returned, value = struct.unpack('<II', data)
        if returned != offset:
            raise RuntimeError('register ABI mismatch')
        return value

    def write(self, offset, value):
        if offset not in CONTROL or (offset == ENABLE and value not in (0, 1)) or (offset != ENABLE and value != 0):
            raise ValueError('only verified enable/disable and zero-length writes allowed')
        fcntl.ioctl(self.fd, WRITE_REG, struct.pack('<II', offset, value))

    def close(self):
        os.close(self.fd)


def state(registers):
    return {hex(offset): registers.read(offset) for offset in CONTROL}


def probe(registers, duration=0.2, now=time.monotonic, sleep=time.sleep):
    """Require idle disabled state; restore all three recorded words in finally.

    This rejects an already enabled/busy mailbox instead of interfering with
    another CLI owner. A nonzero command length aborts; this tool never submits
    command bytes or a positive command length.
    """
    before = state(registers)
    if any(before.values()):
        raise RuntimeError(f'CLI mailbox not idle/disabled: {before}')
    result = {'before': before, 'response_hex': '', 'response_length': 0,
              'sent_command': False, 'duration_seconds': duration, 'writes': []}
    changed = False
    try:
        for offset, value in ((ENABLE, 1), (COMMAND_LENGTH, 0), (RESPONSE_LENGTH, 0)):
            changed = True  # conservatively attempt cleanup after an uncertain ioctl
            registers.write(offset, value)
            result['writes'].append([hex(offset), value])
        result['enabled_latch'] = registers.read(ENABLE)
        if result['enabled_latch'] != 1:
            raise RuntimeError('enable register did not latch 1')
        deadline = now() + duration
        while now() < deadline:
            if registers.read(COMMAND_LENGTH):
                raise RuntimeError('unexpected command publication; no text was sent by this probe')
            length = registers.read(RESPONSE_LENGTH)
            if length:
                if length > 299:
                    raise RuntimeError(f'oversized unsolicited response: {length}')
                data = b''.join(struct.pack('<I', registers.read(RESPONSE_BUFFER + offset))
                                for offset in range(0, (length + 3) // 4 * 4, 4))[:length]
                if registers.read(RESPONSE_LENGTH) != length:
                    raise RuntimeError('response changed during capture')
                result.update(response_length=length, response_hex=data.hex())
                break
            sleep(0.005)
    except Exception as error:
        result['error'] = f'{type(error).__name__}: {error}'
    finally:
        if changed:
            failures = []
            for offset in CONTROL:
                try:
                    registers.write(offset, before[hex(offset)])
                    result['writes'].append([hex(offset), before[hex(offset)]])
                except Exception as error:
                    failures.append(f'{hex(offset)}: {error}')
            if failures:
                result['restore_errors'] = failures
        try:
            result['after'] = state(registers)
            result['restored'] = result['after'] == before
        except Exception as error:
            result['restore_read_error'] = f'{type(error).__name__}: {error}'
            result['restored'] = False
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    modes = parser.add_mutually_exclusive_group(required=True)
    modes.add_argument('--status', action='store_true', help='read-only mailbox registers')
    modes.add_argument('--enable-banner', action='store_true', help='authorized, bounded enable/restore; sends no text')
    parser.add_argument('--output', type=Path, help='new capture file outside git')
    args = parser.parse_args()
    if args.enable_banner and args.output is None:
        parser.error('--enable-banner requires --output')
    output = args.output.open('x') if args.output else None
    registers = None
    try:
        registers = Registers(args.enable_banner)
        result = probe(registers) if args.enable_banner else state(registers)
        text = json.dumps(result, indent=2)
        if output:
            output.write(text + '\n')
            output.flush()
        print(text)
        return int(args.enable_banner and (not result.get('restored') or 'error' in result or 'restore_errors' in result))
    except (OSError, RuntimeError) as error:
        result = {'error': f'{type(error).__name__}: {error}',
                  'preflight_failed': True, 'sent_command': False}
        text = json.dumps(result, indent=2)
        if output:
            output.write(text + '\n')
            output.flush()
        print(text)
        return 1
    finally:
        if registers:
            registers.close()
        if output:
            output.close()


if __name__ == '__main__':
    raise SystemExit(main())
