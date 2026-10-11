"""The connect/init sequence follows the model (parity audit rows 8, 10, 11).

The Apollo x4 must send exactly the writes, in the order, that the DTrace
replay did before the sequence was derived from the model: the fixture
tests/fixtures/x4_init_sequence.json was captured from that code.
"""

import json

import pytest

from conftest import ENGINE_DIR
from hardware import HardwareRouter, MONITOR_PARAM
from init_recorder import (FIXTURES, SCENARIOS, RecordingBackend, run_init,
                           x4_router)
from model_info import ModelInfo, monitor_param_allowed
from state_tree import StateTree
from test_generated_device_maps import DEVICES, ROOT, load_generator
from ua_mixer_daemon import (descriptor_input_buses, descriptor_model,
                             MixerDaemon)

X8P_MAP = ENGINE_DIR / "device_maps" / "device_map_apollo_x8p.json"
X4_MAP = ENGINE_DIR / "device_maps" / "device_map_apollo_x4.json"
PREAMP, MON = 1, 2


def model_router(map_path, slug, kind=None):
    """A router as main() builds it for the model `slug`."""
    tree = StateTree()
    tree.load_device_map(map_path)
    backend = RecordingBackend()
    if kind is not None:
        backend.system_info = {"mixer_kind": kind}
    return (HardwareRouter(backend, tree,
                           input_buses=descriptor_input_buses(slug),
                           model=descriptor_model(slug)), backend, tree)


@pytest.fixture(scope="module")
def maps(tmp_path_factory):
    gen = load_generator()
    d = tmp_path_factory.mktemp("maps")
    out = {}
    for p in DEVICES:
        mp = d / f"{p.stem}.json"
        mp.write_text(gen.dump(gen.generate_for(p.stem)))
        out[p.stem] = mp
    return out


def init_calls(router_factory, scenario="no_readback"):
    return run_init(router_factory, scenario)


def mixer(calls, ch_type=None):
    return [c[1:] for c in calls if c[0] == "mixer"
            and (ch_type is None or c[1] == ch_type)]


# ── x4: byte-identical to the replay ─────────────────────────────────

@pytest.mark.parametrize("scenario", sorted(SCENARIOS))
def test_x4_sequence_identical_to_dtrace_replay(scenario):
    fixture = json.loads((FIXTURES / "x4_init_sequence.json").read_text())
    assert init_calls(x4_router, scenario) == fixture[scenario]


@pytest.mark.parametrize("scenario", sorted(SCENARIOS))
def test_x4_sequence_identical_with_main_wiring(scenario):
    """Router built from devices/apollo-x4.json, as main() does it, and
    with the driver reporting mixer kind 14."""
    fixture = json.loads((FIXTURES / "x4_init_sequence.json").read_text())
    assert init_calls(lambda: model_router(X4_MAP, "apollo-x4", kind=14),
                      scenario) == fixture[scenario]


def test_x4_gen2_sequence_identical_to_x4_on_x4_map():
    fixture = json.loads((FIXTURES / "x4_init_sequence.json").read_text())
    got = init_calls(lambda: model_router(X4_MAP, "apollo-x4-gen-2", kind=22))
    assert got == fixture["no_readback"]


def test_builtin_x4_model_matches_the_json():
    desc = json.loads((ROOT / "devices" / "apollo-x4.json").read_text())
    j = ModelInfo.from_descriptor("apollo-x4", desc)
    b = ModelInfo.x4()
    assert (j.inputs, j.preamps, j.talkback, j.talkback_bus,
            j.mirror_to_digital, j.mixer_class) == \
        (b.inputs, b.preamps, b.talkback, b.talkback_bus,
         b.mirror_to_digital, b.mixer_class)


def test_x4_channel_lists():
    m = ModelInfo.x4()
    assert m.route_channels() == [0, 1, 2, 3, 8, 9, 10, 12, 13, 14, 15,
                                  20, 21, 22, 23, 24, 25, 26, 27]
    assert m.level_channels() == list(range(16)) + list(range(20, 32))
    assert m.spdif_left_channel() == 8
    assert m.link_channels(range(4)) == [0, 2]


# ── x8p ──────────────────────────────────────────────────────────────

def test_x8p_inits_all_eight_preamps():
    calls = init_calls(lambda: model_router(X8P_MAP, "apollo-x8p", kind=12))
    pre = mixer(calls, PREAMP)
    for ch in range(8):
        assert (PREAMP, ch, 0x06, 1) in [tuple(c) for c in pre]  # GainC
    link = [c for c in pre if c[2] == 0x02]
    assert [c[1] for c in link] == [0, 2, 4, 6]
    # route + level for analog 5-8 (synch 4-7)
    assert all((ch, 0x13, 0x0001FFFF) in [(c[1], c[2], c[3]) for c in pre]
               for ch in range(8))


def test_x8p_does_not_force_digout_mode():
    calls = init_calls(lambda: model_router(X8P_MAP, "apollo-x8p", kind=12))
    assert not [c for c in mixer(calls, MON) if c[2] == 0x21]


def test_x4_still_writes_digout_mode_spdif():
    calls = init_calls(x4_router)
    assert [1, 0x21, 0] in [c[1:] for c in mixer(calls, MON)
                            if c[2] == 0x21]


def test_kind_gated_params_skipped_where_the_kext_refuses_them():
    # kind 3 (original Apollo) refuses 0x87 and 0x3e; kind 12 (x8p) takes
    # 0x87 but not 0x3e
    assert not monitor_param_allowed(0x87, 3)
    assert not monitor_param_allowed(0x3E, 3)
    assert monitor_param_allowed(0x87, 12)
    assert not monitor_param_allowed(0x3E, 12)
    assert monitor_param_allowed(0x3E, 14) and monitor_param_allowed(0x87, 14)
    assert monitor_param_allowed(0x3E, None)
    calls = init_calls(lambda: model_router(X8P_MAP, "apollo-x8p", kind=12))
    sent = {c[2] for c in mixer(calls, MON)}
    assert 0x87 in sent and 0x3E not in sent


# ── Twin models ──────────────────────────────────────────────────────

def test_twin_mkii_init(maps):
    calls = init_calls(lambda: model_router(
        maps["apollo-twin-mkii"], "apollo-twin-mkii"))
    m = mixer(calls)
    pre = [c for c in m if c[0] == PREAMP]
    assert {c[1] for c in pre if c[2] == 0x06} == {0, 1}      # 2 preamps
    assert [c[1] for c in pre if c[2] == 0x02] == [0]         # one pair
    route = [c[1] for c in pre if c[2] == 0x13 and c[3] == 0x0001FFFF]
    assert 2 in route                       # talkback is synch-in 2
    assert 10 not in route                  # not the x4's talkback slot
    assert 4 not in route
    levels = {c[1] for c in pre if c[2] == 0x16}
    assert 4 not in levels and 3 in levels and 10 not in levels
    # no HP2 / mirror-to-digital / DigOutMode
    mon = {c[2] for c in m if c[0] == MON}
    assert not mon & {0x1E, 0x21}


def test_twin_x_without_talkback_params_for_a_talkbackless_twin(maps):
    # The Apollo Twin (Armstrong) has no talkback: no TB params
    calls = init_calls(lambda: model_router(maps["apollo-twin"], "apollo-twin"))
    mon = {c[2] for c in mixer(calls, MON)}
    assert not mon & {0x46, 0x47, 0x48}
    assert 0x44 in mon


# ── talkback bus (O6) ────────────────────────────────────────────────

def test_talkback_bus_default_and_twin_mkii(maps):
    assert x4_router()[0].talkback_bus() == 0x0A
    r, _, _ = model_router(maps["apollo-twin-mkii"], "apollo-twin-mkii")
    assert r.talkback_bus() == 2


def test_twin_mkii_talkback_fader_uses_talkback_encoding(maps):
    r, be, tree = model_router(maps["apollo-twin-mkii"], "apollo-twin-mkii")
    names = {}
    i = 0
    while tree.get_value(f"/devices/0/inputs/{i}/Name/value") is not None:
        names[tree.get_value(f"/devices/0/inputs/{i}/Name/value")] = i
        i += 1
    tb = names["TALKBACK"]
    r.on_set(f"/devices/0/inputs/{tb}/FaderLevel/value", 0.0)
    # sub 0 only, unity 3.976353 on bus 2
    assert [(b, s) for b, s, _ in be.bus_params] == [(2, 0)]
    assert be.bus_params[0][2] == pytest.approx(3.976353, rel=1e-4)


def test_x4_talkback_fader_unchanged():
    r, be, tree = x4_router()[0:3]
    r.on_set("/devices/0/inputs/22/FaderLevel/value", 0.0)
    assert [(b, s) for b, s, _ in be.bus_params] == [(0x0A, 0)]


# ── every model: the init runs ───────────────────────────────────────

@pytest.mark.parametrize("slug", [p.stem for p in DEVICES])
def test_init_runs_on_every_model(slug, maps):
    calls = init_calls(lambda: model_router(maps[slug], slug))
    assert calls
    desc = json.loads((ROOT / "devices" / f"{slug}.json").read_text())
    pre = {c[1] for c in mixer(calls, PREAMP) if c[2] == 0x06}
    assert len(pre) == min(8, desc["preamps"])


# ── hardware meters (O9) ─────────────────────────────────────────────

@pytest.mark.parametrize("device_type,readback", [
    (0x1F, True), (0x36, True), (0x3F, True), (0x22, False), (0x27, False),
    (None, False)])
def test_readback_meters_for_x4_family(device_type, readback, monkeypatch):
    import metering
    monkeypatch.setattr(metering, "find_apollo_card", lambda device=None: None)
    meter = metering.make_meter(None, device_type, None)
    assert isinstance(meter, metering.ReadbackMeter) is readback
