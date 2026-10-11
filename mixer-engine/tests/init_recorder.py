"""A recording backend for the connect/init sequence, in write order.

FakeBackend keeps one list per call kind; the init sequence's order across
kinds matters too, so this one adds a single chronological `calls` list.
Used by test_init_sequence.py and by the script that captured the x4
fixture from the code before the all-models change.
"""

import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parent))

from conftest import ENGINE_DIR, FakeBackend, make_readback  # noqa: E402

FIXTURES = HERE / "fixtures"


class RecordingBackend(FakeBackend):
    def __init__(self, connected=True):
        super().__init__(connected)
        self.calls = []

    def set_mixer_param(self, channel_type, channel_idx, param_id, value):
        self.calls.append(["mixer", channel_type, channel_idx, param_id, value])
        return super().set_mixer_param(channel_type, channel_idx, param_id, value)

    def set_mixer_bus_param(self, bus_id, sub_param, value, side=0,
                            solo_mute=False):
        self.calls.append(["bus", bus_id, sub_param, round(value, 6), side,
                           bool(solo_mute)])
        return super().set_mixer_bus_param(bus_id, sub_param, value, side,
                                           solo_mute)

    def set_driver_param(self, param_id, value):
        self.calls.append(["driver", param_id, value])
        return super().set_driver_param(param_id, value)

    def __getattr__(self, name):
        rec = super().__getattr__(name)

        def record(*args, **kwargs):
            self.calls.append(["other", name, repr(args)])
            return rec(*args, **kwargs)
        return record


SCENARIOS = {
    # no readback: safe defaults
    "no_readback": lambda: None,
    # a readback with every flag family, mixed gains, monitor state
    "readback": lambda: (1, make_readback(
        mon_raw=150, mute=False, mono=True, dim=True,
        flags={0: {"48v", "pad", "phase"}, 1: {"lowcut", "micline"},
               2: {"link"}, 3: {"48v"}},
        gains_c=[21, 31, 41, 51])),
}


def run_init(router_factory, scenario):
    """Run the daemon's init sequence; returns the ordered call list."""
    from ua_mixer_daemon import MixerDaemon
    router, backend, tree = router_factory()
    backend.readback = SCENARIOS[scenario]()
    MixerDaemon(tree, router)._push_defaults_to_hardware()
    return backend.calls


def x4_router():
    from hardware import HardwareRouter
    from state_tree import StateTree
    tree = StateTree()
    tree.load_device_map(
        ENGINE_DIR / "device_maps" / "device_map_apollo_x4.json")
    backend = RecordingBackend()
    return HardwareRouter(backend, tree), backend, tree


if __name__ == "__main__":
    # Capture the x4 fixture (run once, from the unmodified code)
    FIXTURES.mkdir(exist_ok=True)
    out = {name: run_init(x4_router, name) for name in SCENARIOS}
    (FIXTURES / "x4_init_sequence.json").write_text(json.dumps(out) + "\n")
    print({k: len(v) for k, v in out.items()})
