"""tools/plugin-mapper: what is mapped again, how the index changes, what the user sees."""
import importlib.util
import io
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
PKG = ROOT / 'tools/plugin-mapper/plugin_mapper'
spec = importlib.util.spec_from_file_location('plugin_mapper_main', PKG / '__main__.py')
mapper = importlib.util.module_from_spec(spec)
sys.modules['plugin_mapper_main'] = mapper
spec.loader.exec_module(mapper)


def key(dll='a' * 64, sdk='s' * 64, entry=None):
    return mapper.cache_key(dll, sdk, entry or {'family': 'generic', 'latency_samples': 12})


def test_a_map_is_current_until_one_of_its_inputs_changes(tmp_path):
    k = key()
    (tmp_path / 'x-mono-runtime-bundle.json').write_text('{}')
    (tmp_path / 'x-stereo-runtime-bundle.json').write_text('{}')
    index = mapper.load_index(tmp_path)
    index['bundles'] = [dict(name='UAD X', layout=lay, file=f'x-{lay}-runtime-bundle.json', key=k)
                        for lay in ('mono', 'stereo')]
    assert mapper.cached(index, tmp_path, 'UAD X', k) == 'built'
    assert mapper.cached(index, tmp_path, 'UAD X', key(dll='b' * 64)) is None           # DLL updated
    assert mapper.cached(index, tmp_path, 'UAD X', key(sdk='t' * 64)) is None           # UAD2SDK updated
    assert mapper.cached(index, tmp_path, 'UAD X', key(entry={'family': 'generic', 'latency_samples': 30})) is None
    assert key()['mapper'] == mapper.MAPPER_VERSION
    (tmp_path / 'x-stereo-runtime-bundle.json').unlink()
    assert mapper.cached(index, tmp_path, 'UAD X', k) is None                            # a map went missing
    assert mapper.cached(index, tmp_path, 'UAD Y', k) is None                            # never mapped


def test_a_failure_is_not_retried_with_the_same_inputs(tmp_path):
    index = mapper.load_index(tmp_path)
    index['failures']['UAD X'] = dict(key=key(), reasons={'mono': 'execution failed'})
    assert mapper.cached(index, tmp_path, 'UAD X', key()) == 'failed'
    assert mapper.cached(index, tmp_path, 'UAD X', key(), retry_failures=True) is None
    assert mapper.cached(index, tmp_path, 'UAD X', key(dll='c' * 64)) is None


def test_v1_index_is_mapped_again(tmp_path):
    (tmp_path / 'index.json').write_text(json.dumps(dict(
        schema='open-apollo-plugin-bundles/1', bundles=[dict(name='UAD X', layout='mono', file='x.json')], failures={'UAD Y': 'x'})))
    index = mapper.load_index(tmp_path)
    assert index['bundles'] == [] and index['failures'] == {}


def test_merge_replaces_one_plugin_only():
    index = dict(schema=mapper.INDEX_SCHEMA, bundles=[
        dict(name='UAD X', layout='mono', file='x-mono'), dict(name='UAD X', layout='stereo', file='x-stereo'),
        dict(name='UAD Y', layout='mono', file='y-mono')],
        failures={'UAD X': dict(key=None, reasons={'stereo': 'old'})}, mono_only=['UAD X'],
        waiting_for_licence=['UAD X', 'UAD Z'])
    result = dict(name='UAD X', key=key(), built=[dict(name='UAD X', layout='mono', file='x-mono', key=key())],
                  failures={'stereo': 'execution failed'}, mono_only=False)
    merged = mapper.merge(index, result)
    assert [(b['name'], b['layout']) for b in merged['bundles']] == [('UAD Y', 'mono'), ('UAD X', 'mono')]
    assert merged['failures'] == {'UAD X': dict(key=key(), reasons={'stereo': 'execution failed'})}
    assert merged['mono_only'] == [] and merged['waiting_for_licence'] == ['UAD Z']


class Clock:
    def __init__(self):
        self.t = 0.0

    def __call__(self):
        return self.t


def test_progress_lines_count_eta_and_markers(tmp_path):
    out, clock = io.StringIO(), Clock()
    p = mapper.Progress(tmp_path, total=4, workers=2, markers=True, stream=out, clock=clock)
    p.finish('UAD Cached', 'cached')
    p.start('UAD A')
    p.start('UAD B')
    clock.t = 40
    p.finish('UAD A', 'mapped')
    clock.t = 50
    p.heartbeat()
    lines = out.getvalue().splitlines()
    assert lines[0] == '1/4: UAD Cached: already mapped'
    assert lines[1].startswith('::progress plugins 1 4 ')
    assert 'Mapping 2/4: UAD A (mono, stereo)…' in lines
    assert any(l.startswith('2/4: UAD A: done 40.0 s · ~') for l in lines)
    assert any(l.startswith('Still mapping UAD B (50 s) · 2/4 done') for l in lines)
    markers = [l.split() for l in lines if l.startswith('::progress')]
    assert all(m[2].isdigit() and m[3] == '4' and m[4].lstrip('-').isdigit() for m in markers)
    state = json.loads((tmp_path / 'progress.json').read_text())
    assert state['done'] == 2 and state['total'] == 4 and state['running'] == ['UAD B'] and state['cached'] == 1
    # ETA: B has ~0 s of its 40 s left (10% floor = 4 s), one more queued at 40 s, over 2 workers.
    assert state['eta_seconds'] == int((4 + 40) / 2)


def test_eta_unknown_before_the_first_map_and_human_text():
    p = mapper.Progress('/nonexistent', total=3, workers=2, stream=io.StringIO())
    assert p.eta() is None and p.human(None) == 'estimating time left'
    assert p.human(30) == '~30 s left' and p.human(600) == '~10 min left'


def test_workers_default_to_cores_minus_one(monkeypatch):
    monkeypatch.delenv('OA_PLUGIN_MAP_WORKERS', raising=False)
    monkeypatch.setattr(mapper.os, 'cpu_count', lambda: 12)
    assert mapper.default_workers() == 11
    monkeypatch.setattr(mapper.os, 'cpu_count', lambda: 1)
    assert mapper.default_workers() == 1
    monkeypatch.setenv('OA_PLUGIN_MAP_WORKERS', '3')
    assert mapper.default_workers() == 3


def test_authorized_file_and_all(tmp_path):
    f = tmp_path / 'names'
    f.write_text('UAD A\n\nUAD B\n')
    assert mapper.authorized_set(str(f)) == {'UAD A', 'UAD B'}
    assert mapper.authorized_set('all') is None


def test_stage_copies_what_mapping_reads(tmp_path):
    root = tmp_path / 'win'
    dlls = root / 'Program Files/Universal Audio/Powered Plugins/UAD-2 Powered Plugins'
    dlls.mkdir(parents=True)
    (root / 'Windows/System32').mkdir(parents=True)
    (root / 'Windows/System32/UAD2SDK.dll').write_bytes(b'sdk')
    names = [e['name'] for e in json.loads(mapper.KNOWN_GOOD.read_text())['plugins']][:2]
    for n in names + ['UAD Not On The List']:
        (dlls / f'{n}.dll').write_bytes(n.encode())
    to = tmp_path / 'staged'
    assert mapper.stage(['--from', str(root), '--to', str(to)]) == 0
    staged = sorted(p.stem for p in (to / 'Program Files/Universal Audio/Powered Plugins/UAD-2 Powered Plugins').glob('*.dll'))
    assert staged == sorted(names)
    assert (to / 'Windows/System32/UAD2SDK.dll').read_bytes() == b'sdk'
    (dlls / f'{names[0]}.dll').unlink()   # uninstalled since
    assert mapper.stage(['--from', str(root), '--to', str(to)]) == 0
    assert not (to / f'Program Files/Universal Audio/Powered Plugins/UAD-2 Powered Plugins/{names[0]}.dll').exists()
