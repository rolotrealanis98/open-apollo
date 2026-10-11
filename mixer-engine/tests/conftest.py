"""Shared fixtures for the mixer-engine tests (no hardware, no network)."""

import sys
from pathlib import Path

import pytest

ENGINE_DIR = Path(__file__).resolve().parent.parent
if str(ENGINE_DIR) not in sys.path:
    # The engine's modules import each other as top-level modules
    sys.path.insert(0, str(ENGINE_DIR))

DEVICE_MAP_X4 = ENGINE_DIR / "device_maps" / "device_map_apollo_x4.json"


class FakeBackend:
    """Stands in for hardware.HardwareBackend: records every write.

    Anything the router calls that is not modelled explicitly is recorded
    in `other` and reports success, so a router path can run end to end.
    """

    # The real fader/pan helpers, run on this recorder: tests then see the
    # sub-params HardwareBackend itself would write.
    @property
    def set_bus_fader(self):
        from hardware import HardwareBackend
        return HardwareBackend.set_bus_fader.__get__(self)

    @property
    def set_bus_pan(self):
        from hardware import HardwareBackend
        return HardwareBackend.set_bus_pan.__get__(self)

    def __init__(self, connected=True):
        self.connected = connected
        self.device_path = None
        self.mixer_params = []       # (channel_type, channel_idx, param_id, value)
        self.bus_params = []         # (bus_id, sub_param, value)
        self.driver_params = []      # (param_id, value)
        self.driver_values = {}      # param_id -> value returned by get_driver_param
        self.readback = None         # what get_hw_readback() returns
        self.other = []              # (method name, args)

    def set_mixer_param(self, channel_type, channel_idx, param_id, value):
        self.mixer_params.append((channel_type, channel_idx, param_id, value))
        return True

    def set_mixer_bus_param(self, bus_id, sub_param, value):
        self.bus_params.append((bus_id, sub_param, value))
        return True

    def set_driver_param(self, param_id, value):
        self.driver_params.append((param_id, value))
        return True

    def get_driver_param(self, param_id):
        return self.driver_values.get(param_id)

    def get_hw_readback(self):
        return self.readback

    def __getattr__(self, name):
        if name.startswith("__"):
            raise AttributeError(name)

        def record(*args, **kwargs):
            self.other.append((name, args))
            return True
        return record

    def clear(self):
        self.mixer_params.clear()
        self.bus_params.clear()
        self.driver_params.clear()
        self.other.clear()


def make_readback(*, words=None, mon_raw=None, mute=False, mono=False,
                  dim=False, hp1_raw=0, hp2_raw=0, talkback=False,
                  clock_lock=None, flags=None, gains_c=None, hiz=(False, False)):
    """Build a 40-word hardware readback block.

    flags: {ch: set of 'micline','pad','link','48v','lowcut','phase'}
    gains_c: four GainC values (dB - 9); ch1 takes 7 bits at shift 6, the
    others 6 bits at shifts 0/16/22.
    clock_lock: None (bit 30 clear) or the lock state (bit 30 set).
    """
    w = list(words) if words else [0] * 40
    names = ("micline", "pad", "link", "48v", "lowcut", "phase")
    for ch, fl in (flags or {}).items():
        for n in fl:
            w[0] |= 1 << (ch * 6 + names.index(n))
    if hiz[0]:
        w[0] |= 1 << 24
    if hiz[1]:
        w[0] |= 1 << 25
    d2 = 0
    if mon_raw is not None:
        d2 |= mon_raw & 0xFF
    d2 |= (hp1_raw & 0xFF) << 8
    d2 |= int(mute) << 16 | int(mono) << 17 | int(dim) << 31
    w[2] = d2
    if gains_c is not None:
        g = gains_c
        w[3] = (g[0] & 0x3F) | (g[1] & 0x7F) << 6 | (g[2] & 0x3F) << 16 \
            | (g[3] & 0x3F) << 22
    w[6] = int(talkback) << 8
    if clock_lock is not None:
        w[6] |= 1 << 30 | int(clock_lock) << 31
    w[7] = (hp2_raw & 0xFF) | 0xA0 << 24
    w[38] = w[39] = 0x12345678
    return w


@pytest.fixture
def backend():
    return FakeBackend()


@pytest.fixture
def tree():
    from state_tree import StateTree
    t = StateTree()
    t.load_device_map(DEVICE_MAP_X4)
    return t


@pytest.fixture
def router(backend, tree):
    from hardware import HardwareRouter
    return HardwareRouter(backend, tree)
