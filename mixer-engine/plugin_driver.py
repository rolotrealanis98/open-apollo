"""Bounded INSTANCE ioctl client; the caller owns and closes the device fd.

No plugin recipes, routing recovery, publication or device discovery live here.
Call invalidate before the fd is closed, replaced or its device session changes.
Any ioctl failure invalidates this client: the owner must reconcile/close the fd,
not retry an operation whose hardware outcome may be unknown.
"""

import ctypes
from dataclasses import dataclass
import errno
import fcntl
import struct
from threading import RLock

INSTANCE = struct.Struct('<8IQQQII6I')
PLUGIN_INSTANCE = (3 << 30) | (INSTANCE.size << 16) | (ord('U') << 8) | 0x94
DSP_ANY = 0xFFFFFFFF
STRIP_DELAYS = struct.Struct('<IIQ14I')
PLUGIN_STRIP_DELAYS = (3 << 30) | (STRIP_DELAYS.size << 16) | (ord('U') << 8) | 0x95


@dataclass(frozen=True)
class StripDelays:
    epoch: int
    generation: int
    record: tuple
    monitor: tuple
    capacity: tuple



@dataclass(frozen=True)
class NativeInstance:
    handle: int
    dsp: int
    epoch: int


@dataclass(frozen=True)
class UnisonReceipt:
    sequence: int
    provenance: int


def _uint(value, bits=32):
    if type(value) is not int or not 0 <= value < 1 << bits:
        raise ValueError(f'expected uint{bits}')
    return value


def _words(values):
    values = tuple(values)
    if values and not 4 <= len(values) <= 1016:
        raise ValueError('command count must be 4..1016')
    return (ctypes.c_uint32 * len(values))(*map(_uint, values))


def _request(op):
    fields = [0] * 19
    fields[0:2] = [INSTANCE.size, op]
    return fields


def _commands(fields, words):
    if len(words):
        pointer = ctypes.addressof(words)
        fields[14:17] = [len(words), pointer & 0xFFFFFFFF, pointer >> 32]


class PluginDriver:
    """Serialize calls on one borrowed fd; returned leases are identity tokens.

    The caller must exclude external close/reopen and other owners of this fd.
    Invalidation does not close the fd or claim that allocations were retired.
    """

    def __init__(self, fd, *, ioctl=None):
        if type(fd) is not int or fd < 0:
            raise ValueError('nonnegative borrowed fd required')
        self._fd = fd
        self._ioctl = ioctl if ioctl is not None else fcntl.ioctl
        self._lock = RLock()
        self._valid = True
        self._instances = {}

    def invalidate(self):
        with self._lock:
            self._valid = False

    def _check(self, instance=None):
        if not self._valid:
            raise OSError(errno.ESTALE, 'client session invalidated')
        if instance is not None and (
            type(instance) is not NativeInstance
            or self._instances.get(instance.handle) is not instance
        ):
            raise ValueError('current client-owned instance required')

    def _call(self, fields):
        buf = bytearray(INSTANCE.pack(*fields))
        try:
            self._ioctl(self._fd, PLUGIN_INSTANCE, buf, True)
        except BaseException:
            # Interrupted or failed calls can have changed the device already.
            self._valid = False
            raise
        return INSTANCE.unpack(buf)

    def create(self, render_id, resources, private_rows, *, dsp=DSP_ANY,
               input_pair=None, commands=(), scratch=(0, 0, 0), stage=False, chain=False,
               unison=False):
        """Create from caller-supplied hashes and (pool, words, link, flags) rows.

        input_pair=(first, second) enables START; second=32 denotes mono.
        DSP command validation and memory/link validation remain driver-owned.
        """
        if type(chain) is not bool or (chain and input_pair is None):
            raise ValueError("chaining requires an explicit input pair")
        if type(unison) is not bool or (unison and not chain):
            raise ValueError('Unison requires an admitted chain')
        if type(stage) is not bool or (stage and input_pair is None):
            raise ValueError("staging requires an explicit input pair")
        resources, private_rows = tuple(resources), tuple(private_rows)
        if not 1 <= len(resources) <= 64 or not 1 <= len(private_rows) <= 128:
            raise ValueError('resource/private count outside ABI bounds')
        if any(type(item) is not bytes or len(item) != 32 for item in resources):
            raise ValueError('resources must be 32-byte hashes')
        rows = []
        for row in private_rows:
            if len(row) != 4:
                raise ValueError('private row requires four uint32 fields')
            rows.extend(_uint(v) for v in row)
        hashes = ctypes.create_string_buffer(b''.join(resources))
        privates = (ctypes.c_uint32 * len(rows))(*rows)
        words = _words(commands)
        fields = _request(1)
        fields[4:10] = [_uint(render_id), _uint(dsp), len(resources),
                        len(private_rows), ctypes.addressof(hashes),
                        ctypes.addressof(privates)]
        scratch = tuple(scratch)
        if len(scratch) != 3:
            raise ValueError('three scratch pool sizes required')
        scratch = tuple(_uint(v, 16 if i < 2 else 32)
                        for i, v in enumerate(scratch))
        if input_pair is not None:
            if len(input_pair) != 2 or not len(words):
                raise ValueError('START needs two input indices and commands')
            first, second = (_uint(v, 16) for v in input_pair)
            fields[2] = (4 if stage else 2) | (8 if chain else 0) | (16 if unison else 0)
            fields[13] = first | second << 16
            fields[17:19] = [scratch[0] | scratch[1] << 16, scratch[2]]
            if any(scratch) and dsp == DSP_ANY:
                raise ValueError('scratch requires an explicit DSP')
            _commands(fields, words)
        elif len(words) or any(scratch):
            raise ValueError('commands and scratch require START')
        with self._lock:
            self._check()
            reply = self._call(fields)
            instance = NativeInstance(reply[3], reply[5], reply[10])
            if (not instance.handle or not instance.epoch
                    or instance.dsp == DSP_ANY
                    or instance.handle in self._instances):
                self._valid = False
                raise OSError(errno.EPROTO, 'invalid CREATE identity')
            self._instances[instance.handle] = instance
            return instance

    def strip_delays(self, previous=None, *, record=None, monitor=None, capacity=None):
        """Query or update only the four driver-owned retained input strips."""
        fields = [STRIP_DELAYS.size, 0, 0, 0, 0] + [0] * 12
        values = (record, monitor, capacity)
        if any(value is not None for value in values):
            if type(previous) is not StripDelays:
                raise ValueError('observed strip identity required for update')
            fields[2:5] = [_uint(previous.epoch, 64), _uint(previous.generation), 15]
            for i, value in enumerate(values):
                if value is not None:
                    value = tuple(value)
                    if len(value) != 4:
                        raise ValueError('exactly four strip values required')
                    fields[1] |= 1 << i
                    fields[5+4*i:9+4*i] = [_uint(word) for word in value]
        elif previous is not None:
            raise ValueError('query does not accept an old identity')
        with self._lock:
            self._check()
            buf = bytearray(STRIP_DELAYS.pack(*fields))
            try:
                self._ioctl(self._fd, PLUGIN_STRIP_DELAYS, buf, True)
            except BaseException:
                self._valid = False
                raise
            reply = STRIP_DELAYS.unpack(buf)
            if (reply[0] != STRIP_DELAYS.size or not reply[2]
                    or reply[4] != 15):
                self._valid = False
                raise OSError(errno.EPROTO, 'invalid retained strip identity')
            return StripDelays(reply[2], reply[3], tuple(reply[5:9]),
                              tuple(reply[9:13]), tuple(reply[13:17]))

    def swap(self, current, candidate):
        """Atomically route an initialized candidate, retaining the old lease."""
        with self._lock:
            self._check(current)
            self._check(candidate)
            if (current is candidate or current.dsp != candidate.dsp
                    or current.epoch != candidate.epoch):
                raise ValueError('distinct instances on the same DSP and epoch required')
            fields = _request(4)
            fields[3], fields[10], fields[13] = current.handle, current.epoch, candidate.handle
            self._call(fields)

    def _unison_transition(self, instance, operation):
        fields = _request(operation)
        with self._lock:
            self._check(instance)
            fields[3], fields[10] = instance.handle, instance.epoch
            reply = self._call(fields)
            if reply[18] != 1:
                self._valid = False
                raise OSError(errno.EPROTO, 'unconfirmed Unison transition provenance')
            return UnisonReceipt(reply[17], reply[18])

    def begin_unison(self, instance):
        """Lease a staged preamp, settle PGA zero, then allow startup writes."""
        return self._unison_transition(instance, 6)

    def activate_unison(self, instance):
        """Route a begun staged header; caller separately enables Custom mode."""
        return self._unison_transition(instance, 7)

    def unison_parameters(self, instance, parameters=()):
        """Apply a bounded final input image to an active or begun Unison lease.

        An empty journal fences prior mixer writes. The receipt confirms firmware
        mailbox consumption; only flags/gain have independent hardware readback.
        Duplicate settings coalesce. Separate calls preserve source ordering.
        """
        parameters = tuple(parameters)
        if len(parameters) > 64:
            raise ValueError('at most 64 input parameter writes required')
        values = []
        for record in parameters:
            if type(record) not in (tuple, list) or len(record) != 3:
                raise ValueError('input, parameter and value required')
            channel, parameter, value = record
            if type(channel) is not int or not 0 <= channel < 4:
                raise ValueError('analog input identity required')
            if type(parameter) is not int or not 0 <= parameter <= 0x17:
                raise ValueError('bounded input parameter required')
            values.extend((channel, parameter, _uint(value)))
        words = (ctypes.c_uint32 * len(values))(*values)
        fields = _request(5)
        _commands(fields, words)
        with self._lock:
            self._check(instance)
            fields[3], fields[10] = instance.handle, instance.epoch
            reply = self._call(fields)
            if reply[18] != 1:
                self._valid = False
                raise OSError(errno.EPROTO, 'unconfirmed Unison journal provenance')
            return UnisonReceipt(reply[17], reply[18])

    def service(self, instance, *, writes=(), read=None):
        """Write then optionally read (private row, word offset, count)."""
        words = _words(writes)
        fields = _request(3)
        _commands(fields, words)
        count = 0
        if read is not None:
            if len(read) != 3:
                raise ValueError('read requires row, offset, count')
            row, offset, count = read
            _uint(row, 8)
            _uint(offset, 24)
            _uint(count)
            if row >= 128 or not 1 <= count <= 64:
                raise ValueError('read outside ABI bounds')
            fields[13], fields[17] = row << 24 | offset, count
        if not len(words) and not count:
            raise ValueError('at least one write or read required')
        values = (ctypes.c_uint32 * count)()
        if count:
            fields[9] = ctypes.addressof(values)
        with self._lock:
            self._check(instance)
            fields[3], fields[10] = instance.handle, instance.epoch
            self._call(fields)
            return tuple(values)

    def destroy(self, instance):
        """Retire an exact lease. Failure disables all further client I/O."""
        with self._lock:
            self._check(instance)
            fields = _request(2)
            fields[3] = instance.handle
            self._call(fields)
            del self._instances[instance.handle]
