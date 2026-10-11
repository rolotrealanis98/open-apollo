"""The router follows the loaded device map, not the x4 layout.

Ground truth: apollo-linux docs/re-notes/parity/16-mixertypes-models.md F17,
F18, F19 (monitor/HP/cue outputs, preamp count, input bus table) and
devices/*.json mixer_inputs synch_in.  Tested on the captured x4 and x8p maps
and on the map generated for every model.
"""

import json

import pytest

from conftest import ENGINE_DIR, FakeBackend
from hardware import (APOLLO_PREAMP_HW_IDS, MAX_PREAMP_CHANNELS,
                      MONITOR_CHANNEL_TYPE, MONITOR_PARAM,
                      PREAMP_CHANNEL_TYPE, HardwareRouter)
from state_tree import StateTree
from test_generated_device_maps import DEVICES, ROOT, load_generator

X4 = ENGINE_DIR / "device_maps" / "device_map_apollo_x4.json"
X8P = ENGINE_DIR / "device_maps" / "device_map_apollo_x8p.json"
IN = "/devices/0/inputs"
OUT = "/devices/0/outputs"


def router_for(path):
    tree = StateTree()
    tree.load_device_map(path)
    backend = FakeBackend()
    return HardwareRouter(backend, tree), backend


@pytest.fixture(scope="module")
def gen():
    return load_generator()


@pytest.fixture(scope="module")
def generated(gen, tmp_path_factory):
    """slug -> (router, backend, descriptor) on the model's generated map."""
    out = {}
    d = tmp_path_factory.mktemp("maps")
    for p in DEVICES:
        slug = p.stem
        mp = d / f"{slug}.json"
        mp.write_text(gen.dump(gen.generate_for(slug)))
        r, be = router_for(mp)
        out[slug] = (r, be, json.loads(p.read_text()))
    return out


# ── output roles ─────────────────────────────────────────────────────

def test_x4_roles():
    r, _ = router_for(X4)
    assert r.monitor_output_index() == 18
    assert [r.output_index("hp", n) for n in (0, 1, 2)] == [19, 20, None]
    assert [r.output_index("cue", n) for n in range(4)] == [14, 15, 16, 17]
    assert r._output_role(21) is None          # MONITOR R FOR SURROUND
    assert r._output_role(0) is None           # a line output


def test_x8p_roles():
    r, _ = router_for(X8P)
    assert r.monitor_output_index() == 22
    assert [r.output_index("hp", n) for n in (0, 1)] == [23, 24]
    assert [r.output_index("cue", n) for n in range(4)] == [18, 19, 20, 21]
    assert r._output_role(25) is None          # MONITOR R FOR SURROUND


def test_x8p_monitor_is_output_22_not_18():
    r, be = router_for(X8P)
    r.on_set(f"{OUT}/22/CRMonitorLevel/value", -20.0)
    assert be.mixer_params == [(MONITOR_CHANNEL_TYPE, 1,
                                MONITOR_PARAM["CRMonitorLevel"], 152)]
    be.clear()
    r.on_set(f"{OUT}/22/Mute/value", True)
    r.on_set(f"{OUT}/22/DimOn/value", True)
    assert (MONITOR_CHANNEL_TYPE, 0, MONITOR_PARAM["MonitorMute"], 2) \
        in be.mixer_params
    be.clear()
    # output 18 is CUE 1 on the x8p: the x4's monitor index must not fire
    r.on_set(f"{OUT}/18/CRMonitorLevel/value", -20.0)
    r.on_set(f"{OUT}/18/MixToMono/value", True)
    assert be.mixer_params == [(MONITOR_CHANNEL_TYPE, 0, 0x06, 1)]


def test_x8p_headphone_level_param():
    r, be = router_for(X8P)
    r.on_set(f"{OUT}/23/CRMonitorLevel/value", -10.0)
    r.on_set(f"{OUT}/24/CRMonitorLevel/value", -10.0)
    params = [p for _, _, p, _ in be.mixer_params]
    assert params == [MONITOR_PARAM["HP1Level"], MONITOR_PARAM["HP2Level"]]
    assert MONITOR_PARAM["CRMonitorLevel"] not in params


def test_roles_without_iotype_fall_back_to_names_and_the_level_carrier():
    tree = StateTree()
    for i, (name, level) in enumerate([("LINE 1", False), ("CUE 1", False),
                                       ("MAIN", True), ("HP 1", False)]):
        tree.add_runtime_property(f"{OUT}/{i}/Name", {
            "type": "string", "value": name})
        if level:
            tree.add_runtime_property(f"{OUT}/{i}/CRMonitorLevel", {
                "type": "float", "value": -20.0})
    r = HardwareRouter(FakeBackend(), tree)
    assert r._output_roles() == {1: ("cue", 0), 2: ("monitor", 0),
                                 3: ("hp", 0)}


def test_monitor_iotype_without_level_is_not_the_monitor():
    r, _ = router_for(X4)
    # the x4's "Monitor R for Surround" has no IOType "Monitor" and no level;
    # give a second Monitor-typed output without a level: it is ignored
    r.state_tree.add_runtime_property(f"{OUT}/22/Name", {
        "type": "string", "value": "EXTRA"})
    r.state_tree.add_runtime_property(f"{OUT}/22/IOType", {
        "type": "string", "value": "Monitor"})
    assert r.monitor_output_index() == 18
    assert r._output_role(22) is None


def test_router_without_a_tree_keeps_the_x4_layout():
    r = HardwareRouter(FakeBackend())
    assert r.monitor_output_index() == 18
    assert r.preamp_inputs() == frozenset(range(4))


# ── preamp count ─────────────────────────────────────────────────────

def test_x4_has_four_preamps():
    r, be = router_for(X4)
    assert r.preamp_inputs() == frozenset(range(4))
    r.on_set(f"{IN}/4/preamps/0/48V/value", True)     # ADAT 1: no preamp
    assert be.mixer_params == []


def test_x8p_has_eight_preamps():
    r, be = router_for(X8P)
    assert r.preamp_inputs() == frozenset(range(8))
    for ch in (4, 7):
        be.clear()
        r.on_set(f"{IN}/{ch}/preamps/0/Gain/value", 30)
        assert [(t, c, p) for t, c, p, _ in be.mixer_params] == [
            (PREAMP_CHANNEL_TYPE, ch, 0x0A), (PREAMP_CHANNEL_TYPE, ch, 0x09),
            (PREAMP_CHANNEL_TYPE, ch, 0x06)]
    be.clear()
    r.on_set(f"{IN}/7/preamps/0/48V/value", True)
    assert (PREAMP_CHANNEL_TYPE, 7, 0x03, 1) in be.mixer_params
    be.clear()
    r.on_set(f"{IN}/8/preamps/0/48V/value", True)    # ADAT 1
    assert be.mixer_params == []


def test_x8p_iotype_on_input_five():
    r, be = router_for(X8P)
    r.on_set(f"{IN}/5/IOType/value", "Line")
    assert be.mixer_params == [(PREAMP_CHANNEL_TYPE, 5, 0x00, 1)]


def test_hw_id_table_covers_eight_preamps():
    assert MAX_PREAMP_CHANNELS == 8
    assert APOLLO_PREAMP_HW_IDS[0] == 0 and APOLLO_PREAMP_HW_IDS[3] == 0x1800
    assert APOLLO_PREAMP_HW_IDS[7] == 0x3800


# ── generated maps, every model ──────────────────────────────────────

@pytest.mark.parametrize("path", DEVICES, ids=lambda p: p.stem)
def test_generated_map_roles_match_the_generators_plan(gen, generated, path):
    r, _, d = generated[path.stem]
    plan = gen.plan_outputs(d)
    want, counts = {}, {}
    for i, o in enumerate(plan):
        kind = {"cue": "cue", "hp": "hp", "monitor": "monitor"}.get(o["role"])
        if kind:
            want[i] = (kind, counts.get(kind, 0))
            counts[kind] = counts.get(kind, 0) + 1
    assert r._output_roles() == want
    mon = [i for i, o in enumerate(plan) if o["role"] == "monitor"]
    assert r.monitor_output_index() == (mon[0] if mon else None)
    # cue outputs: one per bus the model can have
    assert counts.get("cue", 0) == (d["cue_buses"]["max"] or 0)


@pytest.mark.parametrize("path", DEVICES, ids=lambda p: p.stem)
def test_generated_map_preamps(generated, path):
    r, be, d = generated[path.stem]
    want = min(d["preamps"], MAX_PREAMP_CHANNELS)
    assert r.preamp_inputs() == frozenset(range(want))
    # a write to every preamp reaches the backend, the next input's does not
    for ch in range(want):
        be.clear()
        r.on_set(f"{IN}/{ch}/preamps/0/Pad/value", True)
        assert be.mixer_params, f"input {ch}"
    be.clear()
    r.on_set(f"{IN}/{want}/preamps/0/Pad/value", True)
    assert be.mixer_params == []


@pytest.mark.parametrize("path", DEVICES, ids=lambda p: p.stem)
def test_generated_map_input_buses_equal_synch_in(generated, path):
    r, _, d = generated[path.stem]
    synch = {e["name"]: e["synch_in"] for e in d["mixer_inputs"]
             if e["name"] not in ("UNAVAILABLE", "TALKBACK")}
    table = r._input_buses()
    names = []
    i = 0
    while (n := r._tree_value(f"{IN}/{i}/Name/value")) is not None:
        names.append(n)
        i += 1
    assert len(names) == len(table)
    for name, bus in zip(names, table):
        if name in synch:
            assert bus == synch[name], name
        elif name == "TALKBACK":
            assert bus == 10          # the name rule's default
        else:
            assert bus is None, name  # "N/A 1" placeholder


def test_every_json_input_follows_the_name_rule():
    from hardware import bus_id_for_input_name
    for p in DEVICES:
        d = json.loads(p.read_text())
        for e in d["mixer_inputs"]:
            if e["name"] in ("UNAVAILABLE", "TALKBACK"):
                continue
            assert bus_id_for_input_name(e["name"]) == e["synch_in"], \
                (p.stem, e["name"])


def test_x8p_analog_buses_follow_synch_in():
    r, _ = router_for(X8P)
    assert [r._input_bus(i) for i in range(8)] == list(range(8))


def test_input_bus_override_wins():
    # Twin MkII: the talkback input is synch-in 2, not the x4's 10
    tree = StateTree()
    tree.load_device_map(X4)
    r = HardwareRouter(FakeBackend(), tree, input_buses={"TALKBACK": 2})
    assert r._input_bus(22) == 2
    assert r._input_bus(0) == 0
    assert HardwareRouter(FakeBackend(), tree)._input_bus(22) == 10


def test_twin_mkii_descriptor_gives_its_talkback_bus():
    import ua_mixer_daemon
    buses = ua_mixer_daemon.descriptor_input_buses("apollo-twin-mkii")
    assert buses["TALKBACK"] == 2 and buses["ANALOG 1"] == 0
    assert "UNAVAILABLE" not in buses
    assert ua_mixer_daemon.descriptor_input_buses("no-such-model") == {}


def test_input_count():
    assert router_for(X4)[0].input_count() == 24
    assert router_for(X8P)[0].input_count() == 28


def test_x8p_init_defaults_go_to_its_monitor_output_and_all_inputs():
    from ua_mixer_daemon import MixerDaemon
    r, be = router_for(X8P)
    be.readback = None
    MixerDaemon(r.state_tree, r)._push_defaults_to_hardware()
    # monitor level (param 0x01, ch_idx 1) was written from output 22's path
    assert (MONITOR_CHANNEL_TYPE, 1, MONITOR_PARAM["CRMonitorLevel"], 0) \
        in be.mixer_params
    # every one of the x8p's 28 inputs got its fader/mute pushed (talkback
    # at index 26 writes bus 0x0A, analog 8 writes bus 0x07)
    buses = {b for b, _, _ in be.bus_params}
    assert 0x07 in buses and 0x0A in buses


def _tree_set(r, path, value):
    """Put a value in the tree without routing it (as after an earlier SET)."""
    r.state_tree.set_value(path, value)


@pytest.mark.parametrize("path,mon", [(X4, 18), (X8P, 22)])
def test_monitor_mute_and_mono_go_together(path, mon):
    # Param 0x03 is bit 1 mute, bit 0 mono, and the driver writes both bits
    # every time: setting one switch must carry the other's state
    r, be = router_for(path)
    _tree_set(r, f"{OUT}/{mon}/MixToMono/value", True)
    r.on_set(f"{OUT}/{mon}/Mute/value", True)
    assert be.mixer_params == [(MONITOR_CHANNEL_TYPE, 0,
                                MONITOR_PARAM["MonitorMute"], 3)]
    be.clear()
    _tree_set(r, f"{OUT}/{mon}/Mute/value", True)
    r.on_set(f"{OUT}/{mon}/MixToMono/value", False)
    assert be.mixer_params == [(MONITOR_CHANNEL_TYPE, 0,
                                MONITOR_PARAM["MixToMono"], 2)]
    be.clear()
    _tree_set(r, f"{OUT}/{mon}/MixToMono/value", False)
    r.on_set(f"{OUT}/{mon}/Mute/value", False)
    assert be.mixer_params == [(MONITOR_CHANNEL_TYPE, 0,
                                MONITOR_PARAM["MonitorMute"], 0)]


def test_device_mode_word_carries_every_field():
    # CMxMixer::PushDeviceMode: one word (other param 0x21) for TOSLink out
    # (0x08), TOSLink in (0x10), ConverterMode (0x20/0x28/0x30) and the
    # multi-unit order [7:6]; setting one keeps the others
    r, be = router_for(X4)
    _tree_set(r, "/devices/0/TOSLink/value", "ADAT")
    r.on_set("/devices/0/TOSLinkOutput/value", "ADAT")
    assert be.mixer_params == [(MONITOR_CHANNEL_TYPE, 0,
                                MONITOR_PARAM["DigitalOutputMode"], 0x18)]
    be.clear()
    _tree_set(r, "/devices/0/TOSLinkOutput/value", "ADAT")
    r.on_set("/devices/0/TOSLink/value", "S/PDIF")
    assert be.mixer_params[-1][3] == 0x08
    be.clear()
    _tree_set(r, "/devices/0/TOSLink/value", "S/PDIF")
    r.on_set("/devices/0/MultiUnitOrder/value", 2)
    assert be.mixer_params[-1][3] == 0x08 | (2 << 6)
    assert r._device_mode_word(out_adat=False, in_adat=False,
                               converter=3, order=0) == 0x30


def test_single_unit_defaults_keep_the_old_values():
    # A single x4 with S/PDIF in and out: 0, and 8 for ADAT out, as before
    r, be = router_for(X4)
    r.on_set("/devices/0/TOSLinkOutput/value", "S/PDIF")
    r.on_set("/devices/0/TOSLinkOutput/value", "ADAT")
    assert [p[3] for p in be.mixer_params] == [0, 8]
