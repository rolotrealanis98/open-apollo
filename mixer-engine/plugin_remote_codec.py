"""Strict offline subset of recovered Rack scalar response grammar.

Not a live adapter. No defaults or capture replay. Native permissive parsing,
int->float->int precision loss, and unknown coercions are not emulated.
"""
from dataclasses import dataclass
import re
import struct
from plugin_remote import MAX_IDS

_DEC = re.compile(r'-?[0-9]+\Z', re.ASCII)
_HEX = re.compile(r'[0-9a-fA-F]{1,8}\Z', re.ASCII)

class RemoteCodecError(ValueError): pass

@dataclass(frozen=True)
class ScalarReply:
    path: str
    kind: str
    ids: tuple[int, ...]
    words: tuple[int, ...]

    def float_values(self):
        if self.kind != 'floats': raise RemoteCodecError('Reply is not float data')
        return tuple(struct.unpack('<f', struct.pack('<I', word))[0] for word in self.words)

    def int_values(self):
        if self.kind != 'ints': raise RemoteCodecError('Reply is not integer data')
        return tuple(word if word < 0x80000000 else word - 0x100000000 for word in self.words)


def _int32(value):
    if isinstance(value, str):
        if len(value) > 11 or not _DEC.fullmatch(value): raise RemoteCodecError('Expected signed decimal int32')
        value = int(value)
    if type(value) is not int or not -0x80000000 <= value <= 0x7fffffff:
        raise RemoteCodecError('Expected signed decimal int32')
    return value


def _tokens(value):
    if isinstance(value, str):
        if len(value) > MAX_IDS * 12: raise RemoteCodecError('Payload exceeds local bound')
        result = value.split(',')
    elif type(value) is int:
        result = [value]
    else: raise RemoteCodecError('Expected scalar integer or comma-separated string')
    if not 1 <= len(result) <= MAX_IDS: raise RemoteCodecError('Element count exceeds local bound')
    return result


def decode_scalar_response(path, data, parameters, *, expected_path):
    """Decode callback (path,data,parameters), never mutate state.

    Caller must provide the exact registered path. Float words remain uint32 so
    NaN payloads and signed zero survive. Outer network dispatcher integration is
    unimplemented; fields map to recovered callback arguments, not an ABI claim.
    """
    if not isinstance(path, str) or path != expected_path:
        raise RemoteCodecError('Response path differs from registered slot property')
    if not isinstance(parameters, dict): raise RemoteCodecError('Parameters must be an object')
    keys = [key for key in ('floats', 'ints', 'opcode') if key in parameters]
    if len(keys) != 1 or keys[0] == 'opcode':
        raise RemoteCodecError('Exactly one scalar namespace is required')
    kind = keys[0]
    ids = tuple(_int32(token) for token in _tokens(parameters[kind]))
    if len(set(ids)) != len(ids): raise RemoteCodecError('Duplicate IDs are ambiguous')
    if kind == 'floats':
        if not isinstance(data, str): raise RemoteCodecError('Float response requires hex-word string')
        tokens = _tokens(data)
        if any(not isinstance(t, str) or not _HEX.fullmatch(t) for t in tokens):
            raise RemoteCodecError('Float token must be one to eight hexadecimal digits')
        words = tuple(int(t, 16) for t in tokens)
    else:
        words = tuple(_int32(t) & 0xffffffff for t in _tokens(data))
    if len(ids) != len(words): raise RemoteCodecError('ID and value vector lengths differ')
    return ScalarReply(path, kind, ids, words)


def encode_scalar_data(reply):
    """Inverse strict subset for offline fixtures only; no transport sending."""
    if type(reply) is not ScalarReply: raise RemoteCodecError('Expected validated scalar reply')
    if (type(reply.ids) is not tuple or type(reply.words) is not tuple
            or not 1 <= len(reply.ids) <= MAX_IDS or len(reply.ids) != len(reply.words)
            or any(type(w) is not int or not 0 <= w <= 0xffffffff for w in reply.words)):
        raise RemoteCodecError('Invalid reply vector')
    for variable in reply.ids: _int32(variable)
    if len(set(reply.ids)) != len(reply.ids): raise RemoteCodecError('Duplicate IDs are ambiguous')
    if reply.kind == 'floats': return ','.join(f'{word:08x}' for word in reply.words)
    if reply.kind == 'ints': return ','.join(str(v) for v in reply.int_values())
    raise RemoteCodecError('Unknown scalar namespace')

@dataclass(frozen=True)
class ArrayReply:
    path: str
    opcode: int
    variable_id: int
    count: int
    header_word: int
    words: tuple[int, ...]


def _word(value):
    if type(value) is not int or not -0x80000000 <= value <= 0xffffffff:
        raise RemoteCodecError('Expected 32-bit integer word')
    return value & 0xffffffff


def _array_header(path, parameters, expected_path):
    if not isinstance(path, str) or path != expected_path:
        raise RemoteCodecError('Response path differs from registered slot property')
    if not isinstance(parameters, dict) or any(k in parameters for k in ('ints', 'floats')):
        raise RemoteCodecError('Array parameters must be an unambiguous object')
    if any(k not in parameters for k in ('opcode', 'p0', 'p1', 'p2')):
        raise RemoteCodecError('Array response requires opcode, p0, p1, p2')
    op, variable, count, header = (parameters[k] for k in ('opcode', 'p0', 'p1', 'p2'))
    if type(op) is not int or op not in (518, 519): raise RemoteCodecError('Expected array opcode518 or519')
    variable = _word(variable)
    header = _word(header)
    if type(count) is not int or not 0 <= count <= MAX_IDS: raise RemoteCodecError('Invalid array count')
    return op, variable, count, header


def decode_array_response(path, data, parameters, *, expected_path):
    """Decode strict native callback subset: JSON-array string and integer headers.

    Unlike native min(count,len(array)) copying, partial/extra arrays are rejected
    to avoid stale tail data. p2 remains opaque; it is not assumed to be stride.
    """
    import json
    op, variable, count, header = _array_header(path, parameters, expected_path)
    if not isinstance(data, str) or len(data) > MAX_IDS * 16 + 2:
        raise RemoteCodecError('Expected bounded JSON-array string')
    try: values = json.loads(data)
    except (ValueError, RecursionError) as exc: raise RemoteCodecError('Malformed JSON array') from exc
    if type(values) is not list or len(values) != count:
        raise RemoteCodecError('Array length must equal requested count')
    if op == 518:
        words = tuple(_word(v) for v in values)
    else:
        if any(not isinstance(v, str) or not _HEX.fullmatch(v) for v in values):
            raise RemoteCodecError('Float array elements must be hexadecimal uint32 strings')
        words = tuple(int(v, 16) for v in values)
    return ArrayReply(path, op, variable, count, header, words)


def encode_array_data(reply):
    """Build only offline data fixture; transport/backend wiring remains absent."""
    import json
    if type(reply) is not ArrayReply: raise RemoteCodecError('Expected array reply')
    _array_header(reply.path, dict(opcode=reply.opcode, p0=reply.variable_id,
                                  p1=reply.count, p2=reply.header_word), reply.path)
    if type(reply.words) is not tuple or len(reply.words) != reply.count:
        raise RemoteCodecError('Invalid array vector')
    if any(type(w) is not int or not 0 <= w <= 0xffffffff for w in reply.words):
        raise RemoteCodecError('Array words must be uint32')
    # Opcode518's native consumer reads integer bits; signed spelling avoids
    # depending on JSON library uint32-to-int32 overflow/coercion behavior.
    data = ([w if w < 0x80000000 else w - 0x100000000 for w in reply.words]
            if reply.opcode == 518 else [f'{w:08x}' for w in reply.words])
    return json.dumps(data, separators=(',', ':'))
