"""Plug-in emulators in worker processes.

A native control change runs the plug-in's own setter in an emulator (tools/plugin-mapper's
GenericCapture). Building one parses the plug-in's DLL and runs its constructor: seconds of pure
Python. In the daemon's process that held the GIL and stalled the event loop for every client
(14.7 s after a load, 2026-10-19). Here each emulator lives in a worker process; the daemon keeps a
RemoteCapture with the same surface the provider uses (defaults_values, read_values, set_control,
events, format_parameter), so the daemon's threads only wait on a pipe.

Parent side: EmulatorPool (a few workers, started on first use) and RemoteCapture. Child side:
`python3 emulator_worker.py READ_FD WRITE_FD`, a loop answering pickled requests from the daemon
that started it (trusted: only that daemon holds the pipe ends).
"""
import itertools
import logging
import os
import pickle
import struct
import subprocess
import sys
import threading
import zlib
from pathlib import Path

log = logging.getLogger(__name__)

EXECUTOR = Path(__file__).resolve().parents[1] / 'tools/plugin-mapper/plugin_mapper/executor'
_HEADER = struct.Struct('<I')


def _send(fd, message):
    data = pickle.dumps(message, pickle.HIGHEST_PROTOCOL)
    view = memoryview(_HEADER.pack(len(data)) + data)
    while view:
        view = view[os.write(fd, view):]


def _receive(fd):
    def exactly(n):
        chunks, left = [], n
        while left:
            chunk = os.read(fd, min(left, 1 << 20))
            if not chunk:
                raise EOFError('emulator worker pipe closed')
            chunks.append(chunk)
            left -= len(chunk)
        return b''.join(chunks)
    return pickle.loads(exactly(_HEADER.unpack(exactly(_HEADER.size))[0]))


class WorkerError(ValueError):
    """The emulator refused or failed a call (the plug-in's code, a bound check): a ValueError,
    so a load or control fails as it would in-process."""


# ── child ────────────────────────────────────────────────────────────────

def _capture_class():
    """tools/plugin-mapper's GenericCapture; OA_EMULATOR_CAPTURE=module:Class (tests) instead."""
    spec = os.environ.get('OA_EMULATOR_CAPTURE')
    if spec:
        import importlib
        module, name = spec.split(':')
        return getattr(importlib.import_module(module), name)
    if str(EXECUTOR) not in sys.path:
        sys.path.append(str(EXECUTOR))
    from generic_capture import GenericCapture
    return GenericCapture


def _serve(read_fd, write_fd):
    # The emulator reads its settings (OA_UAD_SDK, OA_MODULE_STORE, ...) from the environment
    # when imported; the daemon sets them once it has read the plug-in maps, possibly after this
    # worker started. So each build brings the daemon's OA_* variables and the emulator is
    # imported at the first build, after they are applied.
    GenericCapture = None
    captures = {}
    while True:
        try:
            request = _receive(read_fd)
        except EOFError:
            return
        op, ident, args = request
        try:
            if op == 'new':
                profile, values, environment = args
                os.environ.update(environment)
                if GenericCapture is None:
                    GenericCapture = _capture_class()
                cap = GenericCapture(profile, auto_start=False)
                cap.start()
                if values is not None:
                    if len(values) != len(cap.defaults_values):
                        raise ValueError('complete original parameter vector required')
                    for index, (default, target) in enumerate(zip(cap.defaults_values, values)):
                        if target != default:
                            cap.set_control(index, target)
                captures[ident] = cap
                reply = (tuple(cap.defaults_values), tuple(cap.read_values()), len(cap.events))
            elif op == 'set':
                cap = captures[ident]
                first = len(cap.events)
                cap.set_control(*args)
                reply = (cap.events[first:], tuple(cap.read_values()))
            elif op == 'formats':
                cap = captures[ident]
                reply = tuple(cap.format_parameter(i) for i in range(len(cap.maxima)))
            elif op == 'ping':
                # The libraries the emulator needs (most of its import time), without the
                # emulator itself: its settings may not be in the environment yet.
                for module in ('numpy', 'unicorn', 'capstone', 'pefile'):
                    try:
                        __import__(module)
                    except ImportError:
                        pass
                reply = os.getpid()
            elif op == 'drop':
                captures.pop(ident, None)
                continue  # no answer
            else:
                raise ValueError(f'unknown emulator request {op!r}')
            _send(write_fd, ('ok', reply))
        except Exception as error:  # the daemon raises it as WorkerError
            import traceback
            print(f'emulator worker {os.getpid()}: {op} {ident} failed:', file=sys.stderr)
            traceback.print_exc(file=sys.stderr)
            sys.stderr.flush()
            _send(write_fd, ('error', f'{type(error).__name__}: {error}'))


# ── parent ───────────────────────────────────────────────────────────────

class _Restarted(Exception):
    """The worker restarted since the caller's emulator was built there: build it again."""


class _Worker:
    def __init__(self, index):
        self.index = index
        self.lock = threading.Lock()
        self.proc = None
        self.generation = 0
        self.write_fd = self.read_fd = None
        self.drops = []  # (generation, ident) to free, sent with the next call

    def _start(self):
        to_child_r, to_child_w = os.pipe()
        from_child_r, from_child_w = os.pipe()
        try:
            self.proc = subprocess.Popen(
                [sys.executable, str(Path(__file__).resolve()), str(to_child_r), str(from_child_w)],
                pass_fds=(to_child_r, from_child_w), stdin=subprocess.DEVNULL,
                cwd=str(Path(__file__).resolve().parent))
        except BaseException:
            os.close(to_child_w)
            os.close(from_child_r)
            raise
        finally:
            os.close(to_child_r)
            os.close(from_child_w)
        self.write_fd, self.read_fd = to_child_w, from_child_r
        self.generation += 1
        log.info('emulator worker %d started (pid %d)', self.index, self.proc.pid)

    def _stop(self):
        for fd in (self.write_fd, self.read_fd):
            if fd is not None:
                try:
                    os.close(fd)
                except OSError:
                    pass
        self.write_fd = self.read_fd = None
        if self.proc is not None:
            try:
                self.proc.kill()
                self.proc.wait(timeout=5)
            except (OSError, subprocess.TimeoutExpired):
                pass
            self.proc = None

    def drop(self, generation, ident):
        """Free an emulator (from __del__: never takes the lock, which its thread may hold)."""
        self.drops.append((generation, ident))

    def call(self, op, ident, args, expected=None):
        """(generation, reply). `expected`: the generation the caller's emulator was built in;
        a worker restarted since raises _Restarted instead of answering for an emulator it lacks."""
        with self.lock:
            if self.proc is None or self.proc.poll() is not None:
                if self.proc is not None:
                    log.warning('emulator worker %d exited (%s); restarting', self.index,
                                self.proc.returncode)
                self._stop()
                self._start()
            if expected is not None and expected != self.generation:
                raise _Restarted()
            try:
                while self.drops:
                    generation, dropped = self.drops.pop()
                    if generation == self.generation:
                        _send(self.write_fd, ('drop', dropped, ()))
                _send(self.write_fd, (op, ident, args))
                status, reply = _receive(self.read_fd)
            except (OSError, EOFError) as error:
                self._stop()
                raise ConnectionError(f'emulator worker {self.index}: {error}') from error
            if status == 'error':
                log.warning('emulator worker %d: %s %s failed: %s', self.index, op, ident, reply)
                raise WorkerError(reply)
            return self.generation, reply

    def close(self):
        with self.lock:
            self._stop()


class EmulatorPool:
    """A few worker processes; a plug-in's emulators always go to the same one (its DLLs stay
    parsed there)."""

    def __init__(self, workers=2):
        if type(workers) is not int or workers < 1:
            raise ValueError('at least one emulator worker required')
        self.workers = [_Worker(i) for i in range(workers)]
        self._ids = itertools.count(1)

    def start(self):
        """Start every worker now (each imports the emulator), so the first load does not wait."""
        for worker in self.workers:
            worker.call('ping', 0, ())

    def capture(self, profile, values=None):
        name = str(profile.get('name', ''))
        worker = self.workers[zlib.crc32(name.encode()) % len(self.workers)]
        return RemoteCapture(worker, next(self._ids), profile, values)

    def close(self):
        for worker in self.workers:
            worker.close()


class _EventLog:
    """cap.events for the provider: its length counts the startup events, slices past it are
    the events of later set_control calls (all the provider reads: events[first:])."""

    def __init__(self, start):
        self.start, self.tail = start, []

    def __len__(self):
        return self.start + len(self.tail)

    def __getitem__(self, item):
        if not isinstance(item, slice) or item.step not in (None, 1) or item.stop is not None:
            raise TypeError('events[first:] only')
        first = item.start or 0
        if first < self.start:
            raise IndexError('startup events stay in the emulator worker')
        return self.tail[first - self.start:]


class RemoteCapture:
    """The provider's view of one emulator in a worker: the GenericCapture calls it makes.
    read_values() and the formatter texts are the worker's answers to the last change."""

    def __init__(self, worker, ident, profile, values=None):
        self.worker, self.ident, self.profile = worker, ident, profile
        self.generation = None
        self._build(values)

    def _build(self, values):
        environment = {key: value for key, value in os.environ.items() if key.startswith('OA_')}
        generation, (defaults, current, events) = self.worker.call(
            'new', self.ident, (self.profile, None if values is None else tuple(values), environment))
        if self.generation is None:
            self.defaults_values = defaults
            self.events = _EventLog(events)
        self.generation = generation
        self._values, self._formats = current, None

    def _call(self, op, args):
        """A worker that exited is restarted and this emulator rebuilt at the values it had (the
        plug-in's setter is deterministic; the events already returned stay as they were)."""
        for attempt in range(3):
            try:
                return self.worker.call(op, self.ident, args, expected=self.generation)[1]
            except _Restarted:
                self._build(self._values)
            except ConnectionError:
                if attempt == 2:
                    raise
        raise ConnectionError('emulator worker keeps restarting')

    def start(self):
        pass  # built (and started) in the worker

    def read_values(self):
        return self._values

    def set_control(self, index, value):
        events, values = self._call('set', (index, value))
        self.events.tail.extend(events)
        self._values, self._formats = values, None

    def format_parameter(self, index):
        if self._formats is None:
            self._formats = self._call('formats', ())
        return self._formats[index]

    def __del__(self):
        if getattr(self, 'generation', None) is not None:
            self.worker.drop(self.generation, self.ident)


if __name__ == '__main__':
    _serve(int(sys.argv[1]), int(sys.argv[2]))
