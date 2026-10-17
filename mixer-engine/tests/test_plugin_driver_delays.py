"""Retained-strip ioctl wire contract, using synthetic replies only."""
import errno
from pathlib import Path
import shutil
import subprocess

import pytest

from plugin_driver import PluginDriver, StripDelays, STRIP_DELAYS, PLUGIN_STRIP_DELAYS


class DelayIoctl:
    def __init__(self, *, generation=0):
        self.calls = []
        self.epoch = 0x123456789ABCDEF0
        self.generation = generation
        self.values = [[200] * 4, [0] * 4, [256] * 4]
        self.error = None
        self.corrupt = None

    def __call__(self, fd, number, buf, mutate):
        assert number == PLUGIN_STRIP_DELAYS and mutate is True
        assert type(buf) is bytearray and len(buf) == 72
        q = list(STRIP_DELAYS.unpack(buf))
        self.calls.append((fd, q.copy()))
        if self.error:
            raise self.error
        if q[1]:
            assert q[2:5] == [self.epoch, self.generation, 15]
            for i in range(3):
                if q[1] & (1 << i):
                    self.values[i] = q[5 + 4*i:9 + 4*i]
        q[2:5] = [self.epoch, self.generation, 15]
        q[5:] = sum(self.values, [])
        if self.corrupt:
            q[self.corrupt[0]] = self.corrupt[1]
        buf[:] = STRIP_DELAYS.pack(*q)
        return 0


def test_first_generation_zero_query_and_partial_update_preserve_identity():
    io = DelayIoctl()
    driver = PluginDriver(37, ioctl=io)
    observed = driver.strip_delays()
    assert observed == StripDelays(io.epoch, 0, (200,) * 4, (0,) * 4, (256,) * 4)
    assert io.calls[0] == (37, [72, 0, 0, 0, 0] + [0] * 12)
    updated = driver.strip_delays(observed, record=(100, 101, 102, 103))
    assert updated.record == (100, 101, 102, 103)
    assert updated.monitor == observed.monitor and updated.capacity == observed.capacity
    assert io.calls[1] == (37, [72, 1, io.epoch, 0, 15] + [100, 101, 102, 103] + [0] * 8)


@pytest.mark.parametrize('mask', range(1, 8))
def test_each_field_mask_places_four_samples_in_header_defined_offsets(mask):
    io = DelayIoctl(generation=17)
    driver = PluginDriver(19, ioctl=io)
    old = driver.strip_delays()
    arrays = ((100, 101, 102, 103), (1, 2, 3, 4), (256, 512, 1024, 2048))
    kwargs = {name: arrays[i] for i, name in enumerate(('record', 'monitor', 'capacity'))
              if mask & (1 << i)}
    result = driver.strip_delays(old, **kwargs)
    q = io.calls[-1][1]
    assert q[:5] == [72, mask, io.epoch, 17, 15]
    for i, name in enumerate(('record', 'monitor', 'capacity')):
        assert q[5+4*i:9+4*i] == (list(arrays[i]) if mask & (1 << i) else [0] * 4)
        assert getattr(result, name) == (arrays[i] if mask & (1 << i) else getattr(old, name))


@pytest.mark.parametrize('kwargs', [
    {'record': (1, 2, 3)}, {'monitor': (0,) * 5}, {'capacity': ()},
    {'record': (False, 1, 2, 3)}, {'monitor': (-1, 0, 0, 0)},
    {'capacity': (1 << 32, 0, 0, 0)}, {'record': (1.0, 0, 0, 0)},
])
def test_invalid_arrays_are_rejected_before_io_and_leave_session_usable(kwargs):
    io = DelayIoctl(generation=1)
    driver = PluginDriver(19, ioctl=io)
    old = driver.strip_delays()
    with pytest.raises(ValueError):
        driver.strip_delays(old, **kwargs)
    assert len(io.calls) == 1
    assert driver.strip_delays() == old


def test_query_and_apply_argument_shapes_are_distinct():
    io = DelayIoctl(generation=1)
    driver = PluginDriver(19, ioctl=io)
    old = driver.strip_delays()
    for action in (lambda: driver.strip_delays(old),
                   lambda: driver.strip_delays(record=(0,) * 4),
                   lambda: driver.strip_delays(object(), record=(0,) * 4),
                   lambda: driver.strip_delays(StripDelays(-1, 0, (), (), ()), record=(0,) * 4)):
        with pytest.raises(ValueError):
            action()
    assert len(io.calls) == 1


@pytest.mark.parametrize('error', [OSError(errno.ETIMEDOUT, 'unknown outcome'),
                                  OSError(errno.EBUSY, 'unpublished'),
                                  InterruptedError(errno.EINTR, 'interrupted')])
def test_any_ioctl_error_preserves_exception_and_invalidates_without_retry(error):
    io = DelayIoctl(generation=1)
    driver = PluginDriver(19, ioctl=io)
    old = driver.strip_delays()
    io.error = error
    with pytest.raises(OSError) as caught:
        driver.strip_delays(old, record=(0,) * 4)
    assert caught.value is error
    count = len(io.calls)
    for action in (lambda: driver.strip_delays(),
                   lambda: driver.strip_delays(old, capacity=(0,) * 4)):
        with pytest.raises(OSError) as caught:
            action()
        assert caught.value.errno == errno.ESTALE
    assert len(io.calls) == count


@pytest.mark.parametrize('corrupt', [(0, 71), (2, 0), (4, 7)])
def test_malformed_identity_reply_invalidates_session(corrupt):
    io = DelayIoctl(generation=1)
    io.corrupt = corrupt
    driver = PluginDriver(19, ioctl=io)
    with pytest.raises(OSError) as caught:
        driver.strip_delays()
    assert caught.value.errno == errno.EPROTO
    with pytest.raises(OSError) as caught:
        driver.strip_delays()
    assert caught.value.errno == errno.ESTALE and len(io.calls) == 1


def test_delay_wire_layout_matches_compiled_driver_header(tmp_path):
    cc = shutil.which('cc')
    if cc is None:
        pytest.skip('C compiler unavailable')
    header_dir = Path(__file__).resolve().parents[2] / 'driver'
    source = tmp_path / 'delay_abi.c'
    source.write_text('''#include <stdio.h>
#include <stddef.h>
#include "ua_ioctl.h"
int main(void) {
 printf("%zu %lu %zu %zu %zu %zu %zu %zu %u %u %u",
 sizeof(struct ua_plugin_delay), (unsigned long)UA_IOCTL_PLUGIN_DELAY,
 offsetof(struct ua_plugin_delay, epoch), offsetof(struct ua_plugin_delay, generation),
 offsetof(struct ua_plugin_delay, input_mask), offsetof(struct ua_plugin_delay, record),
 offsetof(struct ua_plugin_delay, monitor), offsetof(struct ua_plugin_delay, capacity),
 UA_PLUGIN_DELAY_RECORD, UA_PLUGIN_DELAY_MONITOR, UA_PLUGIN_DELAY_CAPACITY);
}
''')
    binary = tmp_path / 'delay_abi'
    subprocess.run([cc, '-I', str(header_dir), str(source), '-o', str(binary)],
                   check=True, capture_output=True)
    assert list(map(int, subprocess.check_output([str(binary)], text=True).split())) == [
        STRIP_DELAYS.size, PLUGIN_STRIP_DELAYS, 8, 16, 20, 24, 40, 56, 1, 2, 4]
