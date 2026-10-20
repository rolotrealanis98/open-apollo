"""Emulator-free control tables: the mapper learns them from an emulator and gates them with a
differential test; TableCapture answers like the emulator. A synthetic plug-in stands in for the
emulator (made-up words and texts, nothing from a UA plug-in)."""
import json
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[2]
for path in (ROOT / 'tools/plugin-mapper/plugin_mapper', ROOT / 'tools/plugin-mapper/plugin_mapper/executor'):
    if str(path) not in sys.path:
        sys.path.append(str(path))
import table_capture  # noqa: E402
import tables  # noqa: E402
from synthetic_plugin import Synthetic  # noqa: E402


def learn(kind=Synthetic, **kw):
    return tables.learn_and_gate(kind, changes=600, **kw)


def test_learned_tables_answer_like_the_plugin(tmp_path):
    header, words = learn()
    blocks = {b['ref']: b for b in header['blocks']}
    assert blocks[0x100]['context'] == [0, 3] and blocks[0x100]['lazy'] == []
    assert {k: e[0] for k, e in blocks[0x100]['entries'].items()} == {'0,0': 2, '0,1': 2, '1,0': 1, '1,1': 1}
    assert blocks[0x200]['lazy'] == [5]                       # Power's load-time copy found
    assert header['counter'] == {'mode': 'global', 'last': 6}
    assert header['gate']['status'] == 'pass' and header['gate']['changes'] >= 600
    assert header['controls'][4]['actual'] is not None and header['controls'][1]['actual'] is None

    table_capture.write(tmp_path / 'x.json.tables', dict(header, bundle_sha256='ab'), words)
    loaded = table_capture.load(tmp_path / 'x.json', 'ab')
    assert loaded is not None and table_capture.load(tmp_path / 'x.json', 'cd') is None
    cap, tab = Synthetic(), table_capture.TableCapture(loaded)
    for i, v in [(4, 120), (5, 1), (4, 300), (0, 0), (2, 77), (3, 1), (1, 9), (5, 0)]:
        a, b = len(cap.events), len(tab.events)
        cap.set_control(i, v)
        tab.set_control(i, v)
        assert cap.events[a:] == tab.events[b:] and cap.read_values() == tab.read_values()
        assert [cap.format_parameter(j) for j in range(6)] == [tab.format_parameter(j) for j in range(6)]
    with pytest.raises(ValueError):
        tab.set_control(4, 301)


def test_hidden_load_state_is_caught_by_the_gate(monkeypatch):
    """Without the load-time state step the tables are wrong right after a load; the gate, whose
    sequences each start from a freshly loaded plug-in, refuses them."""
    def no_load_state(self):
        self.lazy = {b: set() for b in self.blocks}
    monkeypatch.setattr(tables.Learner, 'load_state', no_load_state)
    with pytest.raises(tables.Refused, match='gate: events differ'):
        learn()


def test_a_block_on_two_continuous_controls_is_refused():
    class TwoGains(Synthetic):
        two_gains = True
    with pytest.raises(tables.Refused, match='two continuous controls'):
        learn(TwoGains)


def test_a_knob_flat_over_most_of_its_range_is_still_found():
    class Flat(Synthetic):
        def _gain(self):
            words = super()._gain()
            selected = self.values[1] if self.values[0] else self.values[2]
            return words if selected > 280 else [0, 0, words[2]]
    header, _ = learn(Flat)
    assert {b['ref']: b for b in header['blocks']}[0x100]['entries']['1,0'][0] == 1


def test_coupled_controls_are_refused():
    class Coupled(Synthetic):
        coupled = True
    with pytest.raises(tables.Refused, match='moves control'):
        learn(Coupled)


def test_a_damaged_or_foreign_file_is_not_used(tmp_path):
    header, words = learn()
    path = tmp_path / 'x.json.tables'
    table_capture.write(path, dict(header, bundle_sha256='ab'), words)
    raw = bytearray(path.read_bytes())
    raw[-3] ^= 0xff
    path.write_bytes(bytes(raw))
    assert table_capture.load(tmp_path / 'x.json', 'ab') is None
    table_capture.write(path, dict(header, bundle_sha256='ab', gate={'status': 'refused'}), words)
    assert table_capture.load(tmp_path / 'x.json', 'ab') is None


# ── the daemon: a bundle with passing tables is controlled without the emulator ──

def synthetic_bundle(tmp_path, monkeypatch):
    import native_carrier
    from test_native_bundles import descriptor
    strips = tmp_path / 'dsp'
    strips.mkdir()
    for name in native_carrier.STRIP_MODULES:
        (strips / f'{name}.bill').write_bytes(name.encode())
    monkeypatch.setattr(native_carrier, 'DSPDIR', str(strips))
    plugin = Synthetic()
    first = len(plugin.events)
    plugin.set_control(0, 1)
    controls = []
    for i, name in enumerate(Synthetic.names):
        base = f'/devices/0/inputs/0/effects/0/parameters/{i}/'
        controls += [dict(path=base + 'Type', type='string', value='parameter'),
                     dict(path=base + 'Name', type='string', value=name),
                     dict(path=base + 'NormalizedValue', type='float', value=0.0)]
    data = dict(name='UAD Synthetic', status='pass', layout='mono', carrier_channels=1,
                descriptor=dict(descriptor(tmp_path), rows=[[1, 0x400, 0xffffffff, 0]]),
                startup=dict(shadow={'0x10': 6, '0x20': 6}, events=Synthetic().events),
                control_sweep=[dict(samples=[dict(events=plugin.events[first:])])],
                parameter_maxima=list(Synthetic.maxima), parameter_metadata=dict(controls=controls),
                profile=dict(name='UAD Synthetic'))
    bundle = tmp_path / 'synthetic-mono-runtime-bundle.json'
    bundle.write_text(json.dumps(data))
    return bundle


def plans(provider, changes):
    out = []
    provider.reset()
    for index, value in changes:
        plan = provider.plan_control(f'parameters/{index}/NormalizedValue', value)
        provider.accept(plan)
        out.append((plan.writes, plan.observations, plan.values))
    return out


def test_the_daemon_plans_from_tables_exactly_as_from_the_emulator(tmp_path, monkeypatch):
    import native_profiled_carrier_provider as npc
    bundle = synthetic_bundle(tmp_path, monkeypatch)
    changes = [(4, 0.5), (5, 1.0), (4, 1.0), (0, 0.0), (2, 0.25), (3, 1.0), (1, 0.1), (5, 0.0), (4, 0.3)]
    monkeypatch.setattr(npc, '_emulator', lambda: Synthetic)
    expected = plans(npc.ProfiledCarrierProvider(bundle, 0, module_store=tmp_path), changes)

    header, words = learn()
    table_capture.write(table_capture.sidecar_path(bundle), dict(header, bundle_sha256=npc._bundle_sha256(bundle)), words)
    npc._DEFAULTS.clear()
    npc.defaults_path(bundle).unlink()
    for module in ('unicorn', 'numpy', 'capstone', 'pefile', 'generic_capture'):
        monkeypatch.setitem(sys.modules, module, None)          # no venv: importing any of them fails
    monkeypatch.setattr(npc, '_emulator', lambda: (_ for _ in ()).throw(AssertionError('emulator used')))
    monkeypatch.setattr(npc, '_WORKERS', type('NoWorkers', (), {'capture': lambda *a: 1 / 0})())
    provider = npc.ProfiledCarrierProvider(bundle, 0, module_store=tmp_path)
    assert plans(provider, changes) == expected
    assert any(plan[0] for plan in expected)                   # words were written


def test_a_bundle_rebuilt_after_its_tables_goes_back_to_the_emulator(tmp_path, monkeypatch):
    import native_profiled_carrier_provider as npc
    bundle = synthetic_bundle(tmp_path, monkeypatch)
    header, words = learn()
    table_capture.write(table_capture.sidecar_path(bundle), dict(header, bundle_sha256=npc._bundle_sha256(bundle)), words)
    assert npc.load_tables(bundle) is not None
    bundle.write_text(bundle.read_text().replace('"UAD Synthetic"', '"UAD Synthetic" ', 1))
    assert npc.load_tables(bundle) is None
    assert npc.load_tables(tmp_path / 'missing.json') is None
