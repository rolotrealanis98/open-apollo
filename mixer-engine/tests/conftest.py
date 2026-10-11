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
        self.bus_params = []         # (bus_id, sub_param, value): left entries
        self.bus_params_r = []       # (bus_id, sub_param, value): right entries
        self.driver_params = []      # (param_id, value)
        self.driver_values = {}      # param_id -> value returned by get_driver_param
        self.readback = None         # what get_hw_readback() returns
        self.other = []              # (method name, args)
        self.features = 0            # UA_FEAT_* bits has_feature() reports
        self.bus_flags = []          # (bus_id, sub_param, side, solo_mute)
        self.system_info = None      # what get_system_info() returns
        self.toslink_in_bit = False  # the driver's toslink_in_bit parameter

    def get_system_info(self):
        return self.system_info

    def toslink_in_bit_enabled(self):
        return self.toslink_in_bit

    def has_feature(self, bit):
        return bool(self.features & bit)

    def set_mixer_param(self, channel_type, channel_idx, param_id, value):
        self.mixer_params.append((channel_type, channel_idx, param_id, value))
        return True

    def set_mixer_bus_param(self, bus_id, sub_param, value, side=0,
                            solo_mute=False):
        (self.bus_params_r if side else self.bus_params).append(
            (bus_id, sub_param, value))
        self.bus_flags.append((bus_id, sub_param, side, solo_mute))
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
        self.bus_params_r.clear()
        self.bus_flags.clear()
        self.driver_params.clear()
        self.other.clear()


def make_readback(*, words=None, mon_raw=None, mute=False, mono=False,
                  dim=False, hp1_raw=0, hp2_raw=0, talkback=False,
                  clock_lock=None, flags=None, gains_c=None, hiz=(False, False),
                  focus=None, sel_input=0, altmon=0, low_noise=False,
                  temperature=0, board_id=0):
    """Build a 40-word hardware readback block.

    flags: {ch: set of 'micline','pad','link','48v','lowcut','phase'};
    ch 0-3 go to word 0, 4-7 to word 5.
    gains_c: four GainC values (dB - 9), 6 bits at shifts 0/6/16/22.
    clock_lock: None (bit 30 clear) or the lock state (bit 30 set).
    focus: None (word 0 bit 27 clear), 'monitor' or 'headphone'.
    sel_input: front-panel preamp, word 3 [30:28].
    """
    w = list(words) if words else [0] * 40
    names = ("micline", "pad", "link", "48v", "lowcut", "phase")
    for ch, fl in (flags or {}).items():
        for n in fl:
            w[0 if ch < 4 else 5] |= 1 << ((ch & 3) * 6 + names.index(n))
    if focus is not None:
        w[0] |= 1 << 27 | int(focus == "headphone") << 26
    if hiz[0]:
        w[0] |= 1 << 24
    if hiz[1]:
        w[0] |= 1 << 25
    d2 = 0
    if mon_raw is not None:
        d2 |= mon_raw & 0xFF
    d2 |= (hp1_raw & 0xFF) << 8
    d2 |= int(mute) << 16 | int(mono) << 17 | int(low_noise) << 30 \
        | int(dim) << 31
    w[2] = d2
    if gains_c is not None:
        g = gains_c
        w[3] = (g[0] & 0x3F) | (g[1] & 0x3F) << 6 | (g[2] & 0x3F) << 16 \
            | (g[3] & 0x3F) << 22
    w[3] |= (sel_input & 7) << 28
    w[6] = int(talkback) << 8 | (altmon & 3) << 28
    if clock_lock is not None:
        w[6] |= 1 << 30 | int(clock_lock) << 31
    w[7] = (hp2_raw & 0xFF) | 0xA0 << 24
    w[17] |= (temperature & 0xFF) << 8
    w[33] |= (board_id & 7) << 26
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
