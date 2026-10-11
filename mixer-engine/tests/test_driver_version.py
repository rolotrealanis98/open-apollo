"""HardwareBackend.get_version(): UA_IOCTL_GET_VERSION and the old-driver case."""

import errno
import struct

import pytest

import hardware
from hardware import HardwareBackend


@pytest.fixture
def node(tmp_path):
    """A regular file standing in for /dev/ua_apollo0 (ioctl -> ENOTTY,
    which is what a driver without GET_VERSION answers)."""
    p = tmp_path / "ua_apollo0"
    p.write_bytes(b"")
    return p


def test_ioctl_number():
    # _IOR('U', 0x04, 48 bytes)
    assert hardware.UA_IOCTL_GET_VERSION == \
        (2 << 30) | (48 << 16) | (0x55 << 8) | 0x04


def test_old_driver_returns_none(node):
    be = HardwareBackend(str(node))
    assert be.open()
    assert be.get_version() is None


def test_no_device_returns_none():
    assert HardwareBackend("/nonexistent/ua").get_version() is None


def test_decodes_version(node, monkeypatch):
    be = HardwareBackend(str(node))
    be.open()
    feats = hardware.UA_FEAT_EVENTS | hardware.UA_FEAT_BUS_PARAM_BATCH
    reply = struct.pack('<IIII16s16x', 1, 3, 0x000200, feats, b"0.2.0")

    def fake(fd, req, buf, *a):
        assert req == hardware.UA_IOCTL_GET_VERSION
        assert len(buf) == 48
        return reply
    monkeypatch.setattr(hardware.fcntl, "ioctl", fake)
    v = be.get_version()
    assert v == {"abi_major": 1, "abi_minor": 3, "driver_version": 0x200,
                 "features": feats, "driver_name": "0.2.0"}


def test_other_errors_return_none(node, monkeypatch):
    be = HardwareBackend(str(node))
    be.open()

    def boom(*a, **k):
        raise OSError(errno.ENODEV, "gone")
    monkeypatch.setattr(hardware.fcntl, "ioctl", boom)
    assert be.get_version() is None
