#!/usr/bin/env python3
"""Exercise the existing pure resource model with nonexecutable owned fixtures.

The model directory is explicit because the earlier Wine investigation retains
these source-owned Python modules outside this repo. No vendor binaries are
loaded. This prints metadata, never a hardware request or executable image.
"""
import argparse
import dataclasses
import hashlib
import importlib
import json
from pathlib import Path
import struct
import sys


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def rejected(operation):
    try:
        operation()
    except ValueError:
        return
    raise RuntimeError('invalid model input was accepted')


def fixture(identifier, kind, architecture, functional, total):
    # These patterns have no established ISA or entry point. Do not upload them.
    return struct.pack('<4s4I', b'Bill', identifier, kind | architecture << 24,
                       total - 20, functional) + bytes((i * 17 + 3) & 255 for i in range(total - 20))


def run(model_dir):
    sys.path.insert(0, str(model_dir))
    catalog = importlib.import_module('windows_resource_catalog')
    upload = importlib.import_module('windows_resource_upload')
    provenance = {}
    for name in ('windows_resource_catalog', 'windows_resource_upload',
                 'windows_resource_plan', 'windows_4k_resource_manifest'):
        module = importlib.import_module(name)
        path = Path(module.__file__).resolve()
        require(path.parent == model_dir, f'unexpected module location: {name}')
        provenance[name] = hashlib.sha256(path.read_bytes()).hexdigest()
    rows = []
    cases = (
        ('code', fixture(0xF00001, 0, 2, 3, 64), 2, 0x10012),
        ('data', fixture(0xF00002, 1, 0, 2, 60), 2, 0x40011),
        # Native conversion floors 2*functional/3; divisibility is not required.
        ('code-floor', fixture(0xF00003, 0, 2, 4, 68), 2, 0x10013),
        ('code-page-boundary', fixture(0xF00004, 0, 2, 3, 4096), 2, 0x10402),
    )
    for name, original, allocation, expected_header in cases:
        bill = catalog.validate_bill(original)
        arguments = dict(mapped_dsp_word=0x1000, pool_kind=bill.marker & 65535,
                         pool_start_word=0x1000, pool_capacity_words=16,
                         manager_state=1, observation='synthetic offline pool; no device lease',
                         source_sha256=hashlib.sha256(original).hexdigest())
        result = upload.compile_catalog_upload(bill, **arguments)
        require(bill.allocation_words == allocation, 'functional allocation mismatch')
        require(struct.unpack_from('<II', result.contiguous_command) == (expected_header, 0x1000),
                'source-bound framing differs from expected prefix')
        expected = original[:10] + bytes(2) + original[12:]
        require(result.normalized_bill == expected, 'normalization changed unexpected bytes')
        require(result.contiguous_command[8:] == expected, 'full Bill was not retained')
        require(bill.blob == original, 'owned original was mutated')
        require(b''.join(result.chunks) == result.contiguous_command, 'chunk stream differs')
        require(all(0 < len(chunk) <= 4096 and len(chunk) % 4 == 0 for chunk in result.chunks),
                'invalid descriptor chunk extent')
        require(not result.hardware_executed and not result.hardware_completed,
                'model claimed hardware execution')
        reply = upload.parse_reply(struct.pack('<4I', 0x80070004, 0, bill.identifier, 0x12345678),
                                   original_resource_id=bill.identifier)
        require(reply['reply_valid'] and not reply['transport_completed'] and not reply['hardware_executed'],
                'reply shape was promoted to execution')
        for words in ((0, 0, bill.identifier, 0), (0x80070004, 1, bill.identifier, 0),
                      (0x80070004, 0, bill.identifier + 1, 0)):
            rejected(lambda: upload.parse_reply(struct.pack('<4I', *words), original_resource_id=bill.identifier))
        rejected(lambda: upload.parse_reply(bytes(12), original_resource_id=bill.identifier))
        for changes in ({'pool_kind': 3}, {'pool_capacity_words': 1},
                        {'mapped_dsp_word': 0xFFFFFFFF}, {'observation': ''}):
            rejected(lambda: upload.compile_catalog_upload(bill, **(arguments | changes)))
        rejected(lambda: upload.compile_catalog_upload(dataclasses.replace(bill, normalized_blob=b'bad'), **arguments))
        if bill.architecture == 2:
            rejected(lambda: upload.compile_catalog_upload(bill, **(arguments | {'manager_state': 0})))
        rows.append({'fixture': name, 'source_sha256': arguments['source_sha256'],
                     'bill_bytes': len(original), 'functional_count': bill.functional_count,
                     'allocation_units': allocation, 'prefix_word': hex(expected_header),
                     'stream_bytes': len(result.contiguous_command),
                     'chunk_bytes': list(map(len, result.chunks)), 'nonexecutable': True})
    original = cases[0][1]
    registry = catalog.ResourceCatalog()
    registry.register('fixture-session', original)
    conflicting = original[:-1] + bytes([original[-1] ^ 1])
    rejected(lambda: registry.register('fixture-session', conflicting))
    require(len(registry.resources) == 1 and next(iter(registry.resources.values())).blob == original,
            'conflict changed registered source')
    rejected(lambda: catalog.validate_bill(original[:-4]))
    rejected(lambda: catalog.validate_bill(bytearray(original)))
    flagged = bytearray(original)
    flagged[10] = 1
    rejected(lambda: catalog.validate_bill(bytes(flagged)))
    return {'model_modules_sha256': provenance, 'fixtures': rows,
            'framing_and_rejection_checks': 'passed', 'hardware_emission_allowed': False,
            'runtime_execution_verified': False, 'device_signature_policy_known': False,
            'limitations': ['Synthetic pool geometry, no runtime lease or DMA owner',
                            'Fixture bodies are not SHARC instructions or encrypted-code recovery',
                            'Reply parser checks Windows12 shape, not Mac transport or device completion',
                            'Model supports an immutable subset; it does not reproduce every native resource kind/flag',
                            'No authorization, program entry point, process dispatch or SYNCH activation']}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--model-dir', required=True, type=Path,
                        help='existing source-owned tools-wine pure model directory')
    args = parser.parse_args()
    print(json.dumps(run(args.model_dir.resolve()), indent=2))


if __name__ == '__main__':
    main()
