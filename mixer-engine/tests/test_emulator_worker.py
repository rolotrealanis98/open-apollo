"""emulator_worker: real worker processes running a fake capture (no unicorn, no DLL)."""
import os
import time
from pathlib import Path

import pytest

import emulator_worker
from emulator_worker import EmulatorPool, WorkerError

PROFILE = {'name': 'fake', 'maxima': [10, 4, 100], 'defaults': [5, 1, 50]}


@pytest.fixture
def pool(monkeypatch):
    monkeypatch.setenv('OA_EMULATOR_CAPTURE', 'fake_capture:FakeCapture')
    monkeypatch.setenv('PYTHONPATH', os.pathsep.join(
        filter(None, [str(Path(__file__).parent), os.environ.get('PYTHONPATH')])))
    pool = EmulatorPool(1)
    yield pool
    pool.close()


def test_remote_capture_answers_like_the_capture(pool):
    pool.start()
    cap = pool.capture(PROFILE, (7, 1, 50))           # built at non-default values
    assert cap.defaults_values == (5, 1, 50) and cap.read_values() == (7, 1, 50)
    first = len(cap.events)
    cap.set_control(2, 60)
    assert cap.read_values() == (7, 1, 60)
    assert cap.events[first:] == [['write', 2, 180]]
    assert cap.format_parameter(2) == {'raw': ' 60 ', 'console_trimmed': '60'}
    with pytest.raises(WorkerError, match='original domain'):
        cap.set_control(1, 9)
    assert cap.read_values() == (7, 1, 60)             # a refused change leaves the state
    with pytest.raises(IndexError):
        cap.events[0:]                                 # startup events stay in the worker


def test_a_killed_worker_is_restarted_and_the_emulator_rebuilt(pool):
    cap = pool.capture(PROFILE)
    cap.set_control(0, 9)
    worker = pool.workers[0]
    worker.proc.kill()
    worker.proc.wait()
    first = len(cap.events)
    cap.set_control(1, 3)                              # rebuilt at (9, 1, 50), then changed
    assert cap.read_values() == (9, 3, 50)
    assert cap.events[first:] == [['write', 1, 9]]
    assert worker.generation == 2


def test_construction_failures_are_value_errors_and_dropped_emulators_freed(pool):
    with pytest.raises(WorkerError, match='synthetic constructor failure'):
        pool.capture(dict(PROFILE, fail_start=True))
    cap = pool.capture(PROFILE)
    ident = cap.ident
    del cap
    worker = pool.workers[0]
    assert (1, ident) in worker.drops                  # queued, never under the worker's lock
    again = pool.capture(PROFILE)                      # the next call sends it
    assert again.read_values() == (5, 1, 50) and (1, ident) not in worker.drops


def test_settings_made_after_the_workers_started_reach_the_emulator(monkeypatch, pool):
    # The daemon starts its workers before it reads the plug-in maps, which set OA_UAD_SDK.
    monkeypatch.setenv('OA_EMULATOR_CAPTURE', 'fake_capture:EnvironmentCapture')
    pool.start()
    monkeypatch.setenv('OA_TEST_SETTING', 'from the maps')
    assert pool.capture(PROFILE).format_parameter(0)['console_trimmed'] == 'from the maps'


def test_a_dropped_emulator_is_freed_although_it_is_a_reference_cycle(monkeypatch, tmp_path, pool):
    # GenericCapture and its unicorn instance are a cycle holding the emulated memory; a worker
    # left them to the collector, and one that met many plug-ins kept gigabytes (2026-10-20).
    freed = tmp_path / 'freed'
    monkeypatch.setenv('OA_EMULATOR_CAPTURE', 'fake_capture:CyclicCapture')
    monkeypatch.setenv('OA_TEST_FREED', str(freed))
    cap = pool.capture(PROFILE)
    del cap
    pool.workers[0].call('ping', 0, ())               # sends the drop; the worker frees it when idle
    deadline = time.monotonic() + 5
    while not freed.exists() and time.monotonic() < deadline:
        time.sleep(0.02)
    assert freed.exists()
