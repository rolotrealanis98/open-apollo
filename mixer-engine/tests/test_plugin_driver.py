"""Synthetic ioctl/pointer tests only: no device, daemon, socket or UA payload."""

import ctypes
import errno
from pathlib import Path
import shutil
import subprocess

import pytest

from plugin_driver import INSTANCE, PLUGIN_INSTANCE, NativeInstance, PluginDriver


class Ioctl:
    def __init__(self):
        self.calls = []
        self.fail = None
        self.handle = 7

    def __call__(self, fd, number, buf, mutate):
        q = list(INSTANCE.unpack(buf))
        assert number == PLUGIN_INSTANCE and mutate is True
        self.calls.append((fd, q))
        if q[1] == 1:
            assert ctypes.string_at(q[8], 32) == bytes(range(32))
            assert tuple((ctypes.c_uint32 * 4).from_address(q[9])) == (
                1, 16, 0xFFFFFFFF, 0)
            q[3], q[5], q[10] = self.handle, 1, 23
            buf[:] = INSTANCE.pack(*q)
        if q[14]:
            pointer = q[15] | q[16] << 32
            assert tuple((ctypes.c_uint32 * q[14]).from_address(pointer)) == (
                0x00140004, 0, 0, 42)
        if q[1] == 3 and q[17]:
            values = (ctypes.c_uint32 * q[17]).from_address(q[9])
            for i in range(q[17]):
                values[i] = 0x12340000 + i
        if self.fail:
            raise self.fail
        return 0


def create(client, **kwargs):
    return client.create(0x806, [bytes(range(32))],
                         [(1, 16, 0xFFFFFFFF, 0)], **kwargs)


def test_create_service_destroy_uses_one_fd_and_real_buffers():
    ioctl = Ioctl()
    client = PluginDriver(19, ioctl=ioctl)
    instance = create(client, dsp=1, input_pair=(2, 3),
                      commands=(0x00140004, 0, 0, 42), scratch=(4, 8, 16))
    assert instance == NativeInstance(7, 1, 23)
    q = ioctl.calls[-1][1]
    assert q[2] == 2 and q[13] == (2 | 3 << 16)
    assert q[17:19] == [4 | 8 << 16, 16]
    assert client.service(instance, writes=(0x00140004, 0, 0, 42),
                          read=(0, 4, 3)) == (
                              0x12340000, 0x12340001, 0x12340002)
    q = ioctl.calls[-1][1]
    assert q[3] == 7 and q[10] == 23 and q[13] == 4
    assert client.service(instance, writes=(0x00140004, 0, 0, 42)) == ()
    assert client.service(instance, read=(0, 0, 1)) == (0x12340000,)
    client.destroy(instance)
    assert all(fd == 19 for fd, _ in ioctl.calls)
    assert ioctl.calls[-1][1] == [88, 2, 0, 7] + [0] * 15
    before = len(ioctl.calls)
    with pytest.raises(ValueError):
        client.service(instance, read=(0, 0, 1))
    assert len(ioctl.calls) == before


def test_forged_foreign_and_retired_tokens_never_reach_ioctl():
    ioctl = Ioctl()
    a, b = PluginDriver(19, ioctl=ioctl), PluginDriver(19, ioctl=ioctl)
    first, foreign = create(a), create(b)
    for token in (foreign, NativeInstance(7, 1, 23), object()):
        with pytest.raises(ValueError):
            a.destroy(token)
    a.destroy(first)
    replacement = create(a)
    with pytest.raises(ValueError):
        a.destroy(first)
    a.destroy(replacement)


@pytest.mark.parametrize('operation', ['create', 'service', 'destroy'])
def test_failure_preserves_errno_and_disables_uncertain_session(operation):
    ioctl = Ioctl()
    client = PluginDriver(19, ioctl=ioctl)
    instance = create(client)
    error = OSError(errno.ETIMEDOUT, 'synthetic failure')
    ioctl.fail = error
    with pytest.raises(OSError) as caught:
        if operation == 'create':
            create(client)
        elif operation == 'service':
            client.service(instance, read=(0, 0, 1))
        else:
            client.destroy(instance)
    assert caught.value is error
    count = len(ioctl.calls)
    for call in (lambda: client.destroy(instance),
                 lambda: client.service(instance, read=(0, 0, 1)),
                 lambda: create(client)):
        with pytest.raises(OSError) as caught:
            call()
        assert caught.value.errno == errno.ESTALE
    assert len(ioctl.calls) == count


@pytest.mark.parametrize('read', [(0, 0, 0), (0, 0, 65), (128, 0, 1),
                                  (0, 1 << 24, 1), (False, 0, 1)])
def test_bad_reads_are_rejected_without_io(read):
    ioctl = Ioctl()
    client = PluginDriver(19, ioctl=ioctl)
    instance = create(client)
    with pytest.raises(ValueError):
        client.service(instance, read=read)
    assert len(ioctl.calls) == 1


@pytest.mark.parametrize('kwargs', [dict(commands=(1, 2, 3)),
    dict(commands=(1, 2, 3, True)), dict(commands=(1, 2, 3, 1 << 32)),
    dict(commands=(0,) * 1017), dict(scratch=(1, 0, 0)),
    dict(input_pair=(0, 32)),
    dict(input_pair=(0, 32), commands=(1, 2, 3, 4), scratch=(1, 0, 0))])
def test_bad_create_arguments_do_not_reach_ioctl(kwargs):
    ioctl = Ioctl()
    with pytest.raises(ValueError):
        create(PluginDriver(19, ioctl=ioctl), **kwargs)
    assert ioctl.calls == []


def test_invalidate_never_closes_or_reuses_borrowed_fd():
    ioctl = Ioctl()
    client = PluginDriver(19, ioctl=ioctl)
    instance = create(client)
    client.invalidate()
    with pytest.raises(OSError) as caught:
        client.destroy(instance)
    assert caught.value.errno == errno.ESTALE
    assert len(ioctl.calls) == 1


def test_malformed_create_identity_invalidates_session():
    ioctl = Ioctl()
    ioctl.handle = 0
    client = PluginDriver(19, ioctl=ioctl)
    with pytest.raises(OSError) as caught:
        create(client)
    assert caught.value.errno == errno.EPROTO
    with pytest.raises(OSError) as caught:
        create(client)
    assert caught.value.errno == errno.ESTALE
    assert len(ioctl.calls) == 1


def test_instance_layout_matches_compiled_driver_header(tmp_path):
    cc = shutil.which('cc')
    if cc is None:
        pytest.skip('C compiler unavailable')
    driver = Path(__file__).resolve().parents[2] / 'driver'
    source = tmp_path / 'abi.c'
    source.write_text('''#include <stdio.h>
#include <stddef.h>
#include "ua_ioctl.h"
int main(void) {
    printf("%zu %lu %zu %zu %zu %u %u %u", sizeof(struct ua_plugin_instance),
        (unsigned long)UA_IOCTL_PLUGIN_INSTANCE,
        offsetof(struct ua_plugin_instance, resources),
        offsetof(struct ua_plugin_instance, epoch),
        offsetof(struct ua_plugin_instance, reserved),
        UA_PLUGIN_INST_CREATE, UA_PLUGIN_INST_DESTROY, UA_PLUGIN_INST_SERVICE);
    return 0;
}
''')
    binary = tmp_path / 'abi'
    subprocess.run([cc, '-I', str(driver), str(source), '-o', str(binary)],
                   check=True, capture_output=True)
    output = subprocess.check_output([str(binary)], text=True)
    assert list(map(int, output.split())) == [
        INSTANCE.size, PLUGIN_INSTANCE, 32, 48, 64, 1, 2, 3]
