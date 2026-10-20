"""A plug-in's controls without the emulator: the tables `python3 -m plugin_mapper tables` learned
from the emulator on this machine and kept next to the bundle (`<bundle>.tables`).

Standard library only: the daemon reads it with no venv, DLL or emulator worker. TableCapture has
the surface the provider uses of GenericCapture and RemoteCapture (defaults_values, maxima, events,
start, read_values, set_control, format_parameter) and gives the same events, values and texts,
which the mapper's differential gate checked against the emulator before writing the file.

File: MAGIC, u32 little-endian header length, the JSON header, then a zlib stream of u32
little-endian words the header points into. The tables are made from UA's code: like the bundle,
the file stays on this machine.

What the header holds (see tables.py for how each part is learned):
- controls[i].events: the events setting control i emits, in order; a write names its block.
- blocks[b]: one written word run (ref, len); its words depend only on the controls in `context`
  (stepped: every step is a key) and, per key, on at most one continuous `axis` control, a row
  per value 0..maximum. A control in `lazy` reads as 'L' in the key until it is first set: the
  plug-in's own copy of it is not in sync with its value at load.
- texts[j]: the formatter text of parameter j as runs over its own value, per key of `context`.
- counter: how the begin/end transaction count moves ('global': one count for every
  transaction, 'per_start': one per transaction start word, 'none': no transactions).
"""
import bisect
import json
import struct
import sys
import zlib
from array import array
from pathlib import Path

MAGIC = b'OATABLE1'
SCHEMA = 'open-apollo-plugin-tables/1'
_LENGTH = struct.Struct('<I')


def sidecar_path(bundle_path):
    return Path(str(bundle_path) + '.tables')


def key(values):
    """A table key: the context's values in order, 'L' for a control not set since load."""
    return ','.join(str(v) for v in values)


def write(path, header, words):
    """Write a tables file (header: JSON-able dict; words: u32 sequence) atomically."""
    data = array('I', words)
    if sys.byteorder != 'little':
        data.byteswap()
    payload = zlib.compress(data.tobytes(), 9)
    head = json.dumps(dict(header, schema=SCHEMA, words=len(data), payload_crc32=zlib.crc32(payload)),
                      separators=(',', ':')).encode()
    path = Path(path)
    tmp = path.with_name(f'{path.name}.tmp')
    tmp.write_bytes(MAGIC + _LENGTH.pack(len(head)) + head + payload)
    tmp.replace(path)


def read(path):
    """(header, words) of a tables file; ValueError when it is not one or is damaged."""
    raw = Path(path).read_bytes()
    if raw[:len(MAGIC)] != MAGIC:
        raise ValueError('not a plug-in tables file')
    start = len(MAGIC) + _LENGTH.size
    (length,) = _LENGTH.unpack_from(raw, len(MAGIC))
    header = json.loads(raw[start:start + length])
    payload = raw[start + length:]
    if header.get('schema') != SCHEMA or zlib.crc32(payload) != header.get('payload_crc32'):
        raise ValueError('plug-in tables schema or checksum differs')
    words = array('I')
    words.frombytes(zlib.decompress(payload))
    if sys.byteorder != 'little':
        words.byteswap()
    if len(words) != header['words']:
        raise ValueError('plug-in tables length differs')
    return header, words


def load(bundle_path, bundle_sha256):
    """A passing tables file for this exact bundle, as (header, words), or None."""
    try:
        header, words = read(sidecar_path(bundle_path))
    except (OSError, ValueError, KeyError, TypeError, zlib.error):
        return None
    if header.get('bundle_sha256') != bundle_sha256 or (header.get('gate') or {}).get('status') != 'pass':
        return None
    return header, words


class TableCapture:
    """The provider's capture surface, answered from tables."""

    def __init__(self, tables, profile=None):
        header, self._words = tables
        self.profile = profile
        self.maxima = tuple(header['maxima'])
        self.defaults_values = tuple(header['defaults'])
        self._values = list(self.defaults_values)
        self._touched = set()
        self.events = []
        self._controls = header['controls']
        self._blocks = header['blocks']
        self._actual = [c.get('actual') for c in self._controls]
        counter = header['counter']
        self._mode = counter['mode']
        self._count = ({int(k): v for k, v in counter['last'].items()} if self._mode == 'per_start'
                       else counter.get('last'))
        self._texts = []
        for text in header['texts']:
            entries = {k: ([run[0] for run in runs], [run[1] for run in runs]) for k, runs in text['entries'].items()}
            self._texts.append((text['context'], entries))

    def start(self):
        pass  # the tables are the started plug-in

    def read_values(self):
        return tuple(self._values)

    def _key(self, context, lazy=()):
        return key('L' if c in lazy and c not in self._touched else self._values[c] for c in context)

    def _block(self, index):
        block = self._blocks[index]
        found = block['entries'].get(self._key(block['context'], block['lazy']))
        if found is None:
            raise ValueError(f'no table words for block {index} at {self._values}')
        axis, offset = found
        size = block['len']
        if axis >= 0:
            offset += self._values[axis] * size
        return list(self._words[offset:offset + size])

    def _next_count(self, start):
        if self._mode == 'global':
            self._count = (self._count + 1) & 0xffffffff
            return self._count
        if self._mode == 'per_start':
            self._count[start] = (self._count[start] + 1) & 0xffffffff
            return self._count[start]
        raise ValueError('transaction in a plug-in without transactions')

    def set_control(self, index, value):
        if (type(index) is not int or not 0 <= index < len(self.maxima) or type(value) is not int
                or not 0 <= value <= self.maxima[index]):
            raise ValueError('integer original domain required')
        actual = self._actual[index]
        self._values[index] = value if actual is None else self._words[actual + value]
        self._touched.add(index)
        events, open_count = [], None
        for event in self._controls[index]['events']:
            if event[0] == 'write':
                events.append(['write', event[1], self._block(event[3])])
            elif event[0] == 'begin':
                open_count = self._next_count(event[1])
                events.append(['begin', event[1], event[2], open_count])
            else:
                events.append(['end', event[1], open_count])
        self.events.extend(events)

    def format_parameter(self, index):
        context, entries = self._texts[index]
        starts, texts = entries[self._key(context)]
        text = texts[bisect.bisect_right(starts, self._values[index]) - 1]
        return dict(raw=text, console_trimmed=text.strip())
