"""Monitor / headphone level readback publishes dB and tapered on every raw
step (ported from the Codex daemon's 286eee4, test_monitor_level_sync.py).

Below -55.5 dB a 0.5 dB step moves the tapered value by 0.00044, under the
old 0.002 threshold, so a slow knob turn there never reached Console.
"""

import pytest

from conftest import make_readback
from hardware import monitor_hw_to_tapered
from ua_mixer_daemon import MixerDaemon

MON = "/devices/0/outputs/18"


@pytest.fixture
def daemon(router, tree, backend):
    d = MixerDaemon(tree, router)
    writes = []
    tree.on_set = lambda *a: writes.append(a)
    d.writes = writes
    return d


def sync(d, backend, router, last=None, **kw):
    backend.readback = (1, make_readback(**kw))
    rb = router.poll_hw_readback()
    d._sync_hw_readback(rb, last if last is not None else {})
    return rb


def test_half_db_step_updates_both_without_writes(daemon, backend, router):
    sync(daemon, backend, router, mon_raw=106)
    assert daemon.state.get_value(f"{MON}/CRMonitorLevel/value") == -43.0
    assert daemon.state.get_value(f"{MON}/CRMonitorLevelTapered/value") == \
        monitor_hw_to_tapered(106)
    assert daemon.writes == [] and backend.mixer_params == []


def test_slow_turn_below_minus_55_db_is_published(daemon, backend, router):
    first = sync(daemon, backend, router, mon_raw=50)
    seen = []
    daemon.state.register_callback("c", lambda p, v: seen.append((p, v)))
    daemon.state.subscribe("c", f"{MON}/CRMonitorLevel/value")
    sync(daemon, backend, router, last=first, mon_raw=51)
    assert seen == [(f"{MON}/CRMonitorLevel/value", -70.5)]


def test_unchanged_raw_pushes_nothing(daemon, backend, router):
    first = sync(daemon, backend, router, mon_raw=120)
    seen = []
    daemon.state.register_callback("c", lambda p, v: seen.append(p))
    daemon.state.subscribe("c", MON)
    sync(daemon, backend, router, last=first, mon_raw=120)
    assert not [p for p in seen if "CRMonitorLevel" in p]


def test_client_set_suppresses_the_level_readback(daemon, backend, router):
    first = sync(daemon, backend, router, mon_raw=120)
    daemon._readback_suppress[f"{MON}/CRMonitorLevel/value"] = \
        __import__("time").monotonic()
    sync(daemon, backend, router, last=first, mon_raw=140)
    assert daemon.state.get_value(f"{MON}/CRMonitorLevel/value") == -36.0


def test_headphone_levels_publish_db_too(daemon, backend, router, tree):
    for out in (19, 20):
        for ctl, val in (("CRMonitorLevel", -96.0),
                         ("CRMonitorLevelTapered", 0.0)):
            tree.add_runtime_property(f"/devices/0/outputs/{out}/{ctl}",
                                      {"type": "float", "value": val})
    sync(daemon, backend, router, mon_raw=100, hp1_raw=120, hp2_raw=60)
    get = daemon.state.get_value
    assert get("/devices/0/outputs/19/CRMonitorLevel/value") == -36.0
    assert get("/devices/0/outputs/20/CRMonitorLevel/value") == -66.0
    assert get("/devices/0/outputs/20/CRMonitorLevelTapered/value") == \
        monitor_hw_to_tapered(60)
    assert daemon.writes == []
