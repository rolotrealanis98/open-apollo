"""The chain composer's UAD REC / MON record tap, over synthetic carriers (no UA files, no device).

Evidence: HARDWARE-SESSIONS.md "Results, 2026-10-19: record tap flag"; mixer-engine/RECORD-PRE-EFFECTS.md.
"""
import hashlib

import pytest

import native_carrier
import native_chain_composer as ncc
from native_profiled_carrier_provider import ProfiledCarrierProvider
from test_native_bundles import descriptor


class Provider(ProfiledCarrierProvider):
    """The composer's view of a profiled provider, built on a made-up carrier."""
    remote_reads = {}
    sample_rate = 48000

    def __init__(self, carrier, name):
        self.carrier, self.name, self.input_index = carrier, name, carrier.input_index

    def composition_arguments(self):
        return self.carrier.composition_arguments()

    def observations(self):
        return ((10, 5, (7, 9)),)


@pytest.fixture
def providers(tmp_path, monkeypatch):
    strips = tmp_path / 'dsp'
    strips.mkdir()
    for name in native_carrier.STRIP_MODULES:
        (strips / f'{name}.bill').write_bytes(name.encode())
    monkeypatch.setattr(native_carrier, 'DSPDIR', str(strips))

    def make(count, first=0):
        return [Provider(native_carrier.GeneratedOrdinaryCarrier(
            descriptor(tmp_path), {'shadow': {'0x5': 7, '0x6': 9}}, input_index=first,
            module_store=tmp_path), f'Test {i}') for i in range(count)]
    return make


CASES = [(1, (0,), None), (2, (0,), None), (3, (0,), None), (4, (0,), None),
         (2, (0,), (1, 3)), (3, (2,), (0, 2, 3)), (1, (0, 1), None), (4, (2, 3), None)]


def digest(chain):
    return hashlib.sha256(repr((sorted(chain.arguments.items()), chain.observations)).encode()).hexdigest()[:16]


# Today's recipe (UAD REC), recorded before the record tap became a parameter.
GOLDEN = ['3d7bf4ffe95125e5', '0c49ecbcc64d7234', 'cd48208029ab71d1', '448561a19991857b',
          '0c49ecbcc64d7234', '4e6e5af21882912d', 'ab79678d05b363e1', 'f6a146fac3bb922f']


@pytest.mark.parametrize('case', range(len(CASES)))
def test_rec_is_todays_recipe(providers, case):
    count, channels, slots = CASES[case]
    chain = ncc.compose(providers(count, channels[0]), channels=channels, slots=slots)
    assert digest(chain) == GOLDEN[case]
    assert digest(ncc.compose(providers(count, channels[0]), channels=channels, slots=slots,
                              record_pre_effects=False)) == GOLDEN[case]


def words(chain):
    return ncc._shadow(chain.arguments['commands'])


@pytest.mark.parametrize('case', range(len(CASES)))
def test_mon_moves_the_tap_to_the_mic_strip(providers, case):
    count, channels, slots = CASES[case]
    rec = ncc.compose(providers(count, channels[0]), channels=channels, slots=slots)
    mon = ncc.compose(providers(count, channels[0]), channels=channels, slots=slots, record_pre_effects=True)
    native_carrier.check(mon.arguments['private_rows'], list(mon.arguments['commands']))
    before, after = words(rec), words(mon)
    bases = [effect.base_row for effect in mon.effects]
    assert before.get(ncc.MON_TAP, 0) == 0 and after[ncc.MON_TAP] == ncc.RECORD_TAP
    assert before[bases[-1], 4] & ncc.RECORD_TAP
    assert all(not after[base, 4] & ncc.RECORD_TAP for base in bases)
    assert {k for k in after if after[k] != before.get(k)} == {ncc.MON_TAP, (bases[-1], 4)}
    assert (*ncc.MON_TAP, (ncc.RECORD_TAP,)) in mon.observations
    assert all((base, 4, (after[base, 4],)) in mon.observations for base in bases)
    assert mon.arguments['private_rows'] == rec.arguments['private_rows']


@pytest.mark.parametrize('pre', [False, True])
@pytest.mark.parametrize('count', [1, 3])
def test_live_plan_writes_the_composed_words_outside_the_directory(providers, pre, count):
    chain = ncc.compose(providers(count), channels=(0,))
    plan = chain.record_tap_plan(pre)
    target = words(ncc.compose(providers(count), channels=(0,), record_pre_effects=pre))
    written = ncc._shadow(plan.writes)
    assert written == {key: target.get(key, 0) for key in written}
    assert set(written) == {ncc.MON_TAP} | {(effect.base_row, 4) for effect in chain.effects}
    native_carrier.check(chain.arguments['private_rows'], list(plan.writes))
    assert all(row != 0 for row, _ in written)   # never the chain head / directory
    assert plan.observations == tuple((row, word, (value,)) for (row, word), value in sorted(written.items()))
    assert plan.values == {'RecordPreEffects': pre}


def test_record_pre_effects_must_be_bool(providers):
    with pytest.raises(ValueError):
        ncc.compose(providers(1), channels=(0,), record_pre_effects=1)


@pytest.mark.parametrize('count,channels,pair', [(1, (14,), (0x18, 32)), (3, (17,), (0x1b, 32)),
                                                 (2, (14, 15), (0x18, 0x19)), (1, (16, 17), (0x1a, 0x1b))])
@pytest.mark.parametrize('pre', [False, True])
def test_a_virtual_input_chain_carries_the_input_strip(providers, count, channels, pair, pre):
    """Virtual 1-4: head word 7 is the Input Strip (0x83E), rows 1-5 its m0 and commons, the
    effects follow at row 6, and the driver gets DSP inputs 0x18-0x1b."""
    chain = ncc.compose(providers(count, channels[0]), channels=channels, record_pre_effects=pre)
    analog = ncc.compose(providers(count, 0), channels=(0,) if len(channels) == 1 else (0, 1),
                         record_pre_effects=pre)
    rows, commands = chain.arguments['private_rows'], list(chain.arguments['commands'])
    native_carrier.check(rows, commands)
    assert chain.arguments['input_pair'] == pair
    assert rows[1:6] == tuple(native_carrier.input_strip_rows())
    assert len(rows) == len(analog.arguments['private_rows']) - 4
    assert [e.base_row for e in chain.effects] == [e.base_row - 4 for e in analog.effects]
    assert rows[6][2] == native_carrier.L(0, 8)                     # first effect root -> head[8]
    w, a = words(chain), words(analog)
    assert w[0, 7] == native_carrier.INPUT_CARRIER and a[0, 7] == native_carrier.MIC_CARRIER
    assert {(r, k): v for (r, k), v in w.items() if r == 1} == {
        (1, 0): 0x40000000, (1, 1): len(channels), (1, 2): len(channels), (1, 5): 256,
        (1, 8): 0, (1, 16): 200, (1, 22): 0x3a888889, (1, 25): 0x3b83126f,
        **({ncc.MON_TAP: ncc.RECORD_TAP} if pre else {})}
    # each effect's words are the analog chain's, four rows down
    assert {(r - 4, k): v for (r, k), v in a.items() if r >= 10} == {k: v for k, v in w.items() if k[0] >= 6}
    assert [e.slot for e in chain.effects] == [f'/devices/0/inputs/{channels[0]}/effects/{i}' for i in range(count)]


@pytest.mark.parametrize('channels', [(15, 16), (18,), (4,), (13, 14), (1, 2)])
def test_inputs_without_a_native_strip_or_pair_are_refused(providers, channels):
    with pytest.raises(ValueError):
        ncc.compose(providers(1, 0), channels=channels)
