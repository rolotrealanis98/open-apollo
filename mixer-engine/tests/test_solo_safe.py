"""SoloSafe / Isolate / RemoteSolo in the solo recalculation (parity M5)."""

import re

import pytest

IN = "/devices/0/inputs"


@pytest.fixture
def wired(router, tree):
    tree.on_set = router.on_set
    return tree


def sub0(backend, bus):
    return [v for b, s, v in backend.bus_params if (b, s) == (bus, 0)]


def test_x4_map_has_24_solosafe_inputs(tree):
    n = sum(1 for p in tree.all_paths() if re.fullmatch(IN + r"/\d+/SoloSafe", p))
    assert n == 24


def test_solo_mutes_the_others(wired, backend):
    wired.set(f"{IN}/0/FaderLevel/value", 0.0)
    wired.set(f"{IN}/1/Solo/value", True)
    assert sub0(backend, 0x00)[-1] == 0.0


@pytest.mark.parametrize("ctl", ["SoloSafe", "Isolate"])
def test_exempt_channel_is_not_muted_by_a_solo(wired, backend, ctl):
    wired.set(f"{IN}/0/FaderLevel/value", 0.0)
    wired.set(f"{IN}/0/{ctl}/value", True)
    backend.clear()
    wired.set(f"{IN}/1/Solo/value", True)
    assert sub0(backend, 0x00)[-1] != 0.0
    # a non-exempt channel is still silenced
    assert sub0(backend, 0x02)[-1] == 0.0


@pytest.mark.parametrize("ctl", ["SoloSafe", "Isolate"])
def test_exempt_change_during_a_solo_recalculates(wired, backend, ctl):
    wired.set(f"{IN}/0/FaderLevel/value", 0.0)
    wired.set(f"{IN}/1/Solo/value", True)
    assert sub0(backend, 0x00)[-1] == 0.0
    backend.clear()
    wired.set(f"{IN}/0/{ctl}/value", True)
    assert sub0(backend, 0x00)[-1] != 0.0
    backend.clear()
    wired.set(f"{IN}/0/{ctl}/value", False)
    assert sub0(backend, 0x00)[-1] == 0.0


def test_exempt_change_without_a_solo_writes_nothing(wired, backend):
    backend.clear()
    wired.set(f"{IN}/0/SoloSafe/value", True)
    assert backend.bus_params == []


def test_mute_beats_solo_safe(wired, backend):
    wired.set(f"{IN}/0/FaderLevel/value", 0.0)
    wired.set(f"{IN}/0/SoloSafe/value", True)
    wired.set(f"{IN}/0/Mute/value", True)
    wired.set(f"{IN}/1/Solo/value", True)
    assert sub0(backend, 0x00)[-1] == 0.0


def test_remote_solo_engages_the_rule(wired, backend):
    wired.set(f"{IN}/0/FaderLevel/value", 0.0)
    wired.set(f"{IN}/1/SoloSafe/value", True)
    backend.clear()
    wired.set("/RemoteSolo/value", True)
    assert sub0(backend, 0x00)[-1] == 0.0          # muted by the remote solo
    assert sub0(backend, 0x01)[-1] != 0.0          # SoloSafe stays audible
    backend.clear()
    wired.set("/RemoteSolo/value", False)
    assert sub0(backend, 0x00)[-1] != 0.0


def test_paused_router_ignores_it(wired, backend, router):
    router.set_paused(True)
    backend.clear()
    wired.set(f"{IN}/0/SoloSafe/value", True)
    assert backend.bus_params == []


def test_talkback_is_isolated_in_the_capture(wired, backend):
    # the captured x4 map has Isolate on the talkback input: a solo leaves
    # it audible
    wired.set(f"{IN}/22/FaderLevel/value", 0.0)
    backend.clear()
    wired.set(f"{IN}/1/Solo/value", True)
    assert [v for b, s, v in backend.bus_params if b == 0x0A][-1] != 0.0
