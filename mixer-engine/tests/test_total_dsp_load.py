"""/TotalDSPLoad, /TotalPGMLoad, /TotalMEMLoad from the driver's GET_DSP_INFO
load words (ABI 1.6, UA_FEAT_DSP_LOAD), averaged over the DSPs as UA's
Mixer Engine does (CUAD2Info::GetAllocatedLoads)."""

import errno
import struct

import pytest

import hardware
from hardware import HardwareBackend
from ua_mixer_daemon import MixerDaemon, apply_runtime_properties


class DspDriver:
    def __init__(self, monkeypatch, features, loads, ndsps=4):
        self.features = features
        self.loads = loads          # per DSP: (alloc, measured, pgm, mem)
        self.ndsps = ndsps
        monkeypatch.setattr(hardware.fcntl, "ioctl", self.ioctl)

    def ioctl(self, fd, req, buf):
        if req == hardware.UA_IOCTL_GET_VERSION:
            return struct.pack('<IIII16s16x', 1, 6, 0x200, self.features,
                               b"0.2.0")
        if req == hardware.UA_IOCTL_GET_DSP_INFO:
            idx = struct.unpack_from('<I', buf)[0]
            if idx >= self.ndsps:
                raise OSError(errno.EINVAL, "no such DSP")
            a, m, p, mem = self.loads[idx]
            flags = hardware.UA_DSP_INFO_F_LOADS \
                if self.features & hardware.UA_FEAT_DSP_LOAD else 0
            return struct.pack('<8I', idx, 0x2000 + idx * 0x80, 1, a, m, p,
                               mem, flags)
        raise OSError(errno.ENOTTY, "unexpected")


X4 = [(119, 1923, 0, 0), (119, 0, 0, 0), (119, 0, 0, 0), (119, 0, 0, 0)]


def backend(monkeypatch, features, loads=X4, **kw):
    DspDriver(monkeypatch, features, loads, **kw)
    be = HardwareBackend()
    be.fd, be.connected = 3, True
    return be


def test_feature_bit_value():
    assert hardware.UA_FEAT_DSP_LOAD == 1 << 16


def test_dsp_info_decodes_the_loads(monkeypatch):
    be = backend(monkeypatch, hardware.UA_FEAT_DSP_LOAD)
    info = be.get_dsp_info(0)
    assert info["alloc_load"] == 119 and info["measured_load"] == 1923
    assert info["status"] == 1


def test_old_driver_has_no_loads(monkeypatch):
    be = backend(monkeypatch, 0)
    assert "alloc_load" not in be.get_dsp_info(0)
    assert be.get_dsp_loads() is None


def test_average_over_the_dsps(monkeypatch):
    be = backend(monkeypatch, hardware.UA_FEAT_DSP_LOAD)
    assert be.get_dsp_loads() == pytest.approx((1.19, 0.0, 0.0))
    two = [(100, 0, 500, 200), (300, 0, 1500, 0)]
    be = backend(monkeypatch, hardware.UA_FEAT_DSP_LOAD, two, ndsps=2)
    assert be.get_dsp_loads() == pytest.approx((2.0, 10.0, 1.0))


@pytest.fixture
def daemon(router, tree):
    apply_runtime_properties(tree)
    return MixerDaemon(tree, router)


def test_poll_publishes_and_does_not_save(daemon, tree, monkeypatch):
    be = daemon.hw_router.backend
    monkeypatch.setattr(be, "get_dsp_loads", lambda: (1.19, 0.0, 0.0),
                        raising=False)
    monkeypatch.setattr(be, "connected", True, raising=False)
    daemon._poll_dsp_load()
    assert tree.get_value("/TotalDSPLoad/value") == pytest.approx(1.19)
    assert tree.get_value("/TotalPGMLoad/value") == 0.0
    assert "/TotalDSPLoad/value" not in tree._dirty


def test_poll_without_figures_reports_zero(daemon, tree, monkeypatch):
    be = daemon.hw_router.backend
    monkeypatch.setattr(be, "get_dsp_loads", lambda: None, raising=False)
    monkeypatch.setattr(be, "connected", True, raising=False)
    daemon._poll_dsp_load()
    assert tree.get_value("/TotalDSPLoad/value") == 0.0
