"""A load publishes a plug-in's learned load-time values and texts instead of starting its
emulator (seconds); the emulator starts in the background for the first control change."""
import json
import threading

import native_profiled_carrier_provider as npc


def provider(tmp_path, monkeypatch):
    bundle = tmp_path / 'x-mono-runtime-bundle.json'
    bundle.write_text(json.dumps({'name': 'UAD X'}))
    p = npc.ProfiledCarrierProvider.__new__(npc.ProfiledCarrierProvider)
    p.bundle_path, p.maxima, p.cap, p.state, p._texts = bundle, (10, 4), None, None, None
    p._lock = threading.RLock()
    p.data = {'startup': {'shadow': {'0x5': 7}}}
    p.params = {0: {'NormalizedValue': {'type': 'float'}, 'StringValue': {'type': 'string'}},
                1: {'NormalizedValue': {'type': 'float'}, 'StringValue': {'type': 'string'},
                    'StepValue': {'type': 'string', 'values': ['A', 'B', 'C', 'D', 'E']}}}
    started = []
    monkeypatch.setattr(p, 'warm', lambda background=True: started.append(background), raising=False)
    return p, bundle, started


def test_learned_defaults_load_without_the_emulator(tmp_path, monkeypatch):
    npc._DEFAULTS.clear()
    p, bundle, started = provider(tmp_path, monkeypatch)
    npc.save_defaults(bundle, (5, 2), ('-3.0 dB', 'C'))
    npc._DEFAULTS.clear()                                  # a new daemon: read from disk
    monkeypatch.setattr(p, '_ensure', lambda: (_ for _ in ()).throw(AssertionError('emulator started at load')),
                        raising=False)
    params = p._parameters()
    assert [x['properties']['StringValue']['value'] for x in params] == ['-3.0 dB', 'C']
    assert params[0]['properties']['NormalizedValue']['value'] == 0.5
    assert params[1]['properties']['StepValue']['value'] == 'C'
    assert p.state.values == (5, 2) and p.state.shadow_dict() == {5: 7}
    assert started == [True]                               # the emulator warms in the background


def test_a_rebuilt_bundle_is_learned_again(tmp_path, monkeypatch):
    npc._DEFAULTS.clear()
    p, bundle, _ = provider(tmp_path, monkeypatch)
    npc.save_defaults(bundle, (5, 2), ('a', 'b'))
    npc._DEFAULTS.clear()
    bundle.write_text(json.dumps({'name': 'UAD X', 'rebuilt': True}))
    assert npc.load_defaults(bundle) is None
    assert npc.load_defaults(tmp_path / 'missing.json') is None


def test_unknown_defaults_start_the_emulator_as_before(tmp_path, monkeypatch):
    npc._DEFAULTS.clear()
    p, bundle, started = provider(tmp_path, monkeypatch)
    calls = []

    def ensure():
        calls.append(1)
        p.state = npc.ProfiledState.make((0, 0), {})

    monkeypatch.setattr(p, '_ensure', ensure, raising=False)
    monkeypatch.setattr(p, '_texts_now', lambda: ('x', 'A'), raising=False)
    p._parameters()
    assert calls and not started
