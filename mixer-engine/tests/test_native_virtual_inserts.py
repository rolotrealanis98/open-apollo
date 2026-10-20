"""Inserts on the Apollo x4's Virtual strips (VIRTUAL 1-4 = /devices/0/inputs/14-17, DSP inputs
0x18-0x1b): the chain carries the Input Strip (0x83E, modules A5 C2) instead of the Mic strip.
Synthetic carriers and a memory device; no UA files."""
import pytest

import native_carrier
import native_chain_composer as ncc
from plugin_driver import NativeInstance, StripDelays
from plugin_native_chain import NativeChainBackend
from plugin_publication import Publisher
from plugin_transactions import PluginTransactions
from state_tree import StateTree
from helper_tree import HelperTree
from test_native_chain_composer import Provider
from test_native_bundles import descriptor
from test_plugin_native import Provider as Synthetic

V1 = '/devices/0/inputs/14/effects/0'


class Effect(Provider):
    """A composer-ready provider the chain backend can load (name, initial, latency)."""
    descriptor_latency_samples = 0

    def __init__(self, carrier, name, channels=None):
        super().__init__(carrier, name)
        self.slot = f'/devices/0/inputs/{self.input_index}/effects/0'
        if channels:
            self.channels = channels

    def reset(self):
        pass

    def __deepcopy__(self, memo):  # a load's private copy (the real one re-reads its bundle)
        return Effect(self.carrier, self.name, getattr(self, 'channels', None))

    def initial(self, lease):
        return Synthetic.initial(self, lease)


@pytest.fixture
def make(tmp_path, monkeypatch):
    strips = tmp_path / 'dsp'
    strips.mkdir()
    for name in native_carrier.STRIP_MODULES:
        (strips / f'{name}.bill').write_bytes(name.encode())
    monkeypatch.setattr(native_carrier, 'DSPDIR', str(strips))

    def make(first, name='Test', channels=None):
        carrier = native_carrier.GeneratedOrdinaryCarrier(
            descriptor(tmp_path), {'shadow': {'0x5': 7, '0x6': 9}}, input_index=first, module_store=tmp_path)
        return Effect(carrier, name, channels)
    return make


def modules(*names):
    return set(native_carrier.module_hashes(names))


@pytest.mark.parametrize('channels,pair', [((14,), (0x18, 32)), ((17,), (0x1b, 32)),
                                           ((14, 15), (0x18, 0x19)), ((16, 17), (0x1a, 0x1b))])
def test_a_virtual_chain_carries_the_input_strip(make, channels, pair):
    chain = ncc.compose([make(channels[0]), make(channels[0], 'Two')], channels=channels)
    args, words = chain.arguments, ncc._shadow(chain.arguments['commands'])
    rows = args['private_rows']
    assert args['input_pair'] == pair
    assert rows[1:6] == tuple(native_carrier.input_strip_rows())           # header + Input Strip, 6 rows
    assert [e.base_row for e in chain.effects] == [6, 7] and rows[6][2] == native_carrier.L(0, 8)
    assert words[0, 7] == native_carrier.INPUT_STRIP_PLUGIN and words[0, 5] == 3
    assert words[0, 1] == words[0, 2] == words[1, 1] == words[1, 2] == len(channels)
    assert {(1, w): v for w, v in native_carrier.INPUT_STRIP_WORDS.items() if w not in (1, 2, 5)}.items() <= words.items()
    assert words[1, 16] == 200 and (1, 16, (200,)) in chain.observations
    assert words[7, 4] & ncc.RECORD_TAP and not words.get((1, 4), 0)   # the last effect records
    assert set(args['resources']) & modules('0eb', '0da', '12b') == set()
    assert set(args['resources']) >= modules('0a5', '0c2')
    assert not any(row >= len(rows) for row, _, _ in chain.observations)
    native_carrier.check(rows, list(args['commands']))


def test_a_virtual_chain_in_mon_taps_the_input_strip(make):
    chain = ncc.compose([make(15)], channels=(15,), record_pre_effects=True)
    words = ncc._shadow(chain.arguments['commands'])
    assert words[ncc.MON_TAP] == ncc.RECORD_TAP and not words[6, 4] & ncc.RECORD_TAP


def test_a_virtual_recipe_never_goes_to_the_device_without_the_composer(make):
    with pytest.raises(ValueError, match='chain composer'):
        make(14).carrier.create_arguments()
    assert make(3).carrier.create_arguments()['input_pair'] == (3, 32)
    with pytest.raises(ValueError):
        make(18)                                    # Virtual 5: no strip, no recipe
    with pytest.raises(ValueError):
        ncc.compose([make(14)], channels=(15, 16))  # not a pair


class Device:
    """Native instances in memory: every masked WRITE lands, every read returns it."""

    def __init__(self):
        self.memory, self.active, self.created, self.sequence = {}, set(), [], 6
        self.strips = StripDelays(23, 1, (200,)*4, (0,)*4, (256,)*4)

    def invalidate(self):
        raise AssertionError('quarantined')

    def create(self, **arguments):
        self.sequence += 1
        native = NativeInstance(self.sequence, 1, 23)
        self.created.append(arguments)
        self.memory[native.handle] = {}
        self.service(native, writes=arguments['commands'])
        if not arguments.get('stage'):
            self.active.add(native.handle)
        return native

    def service(self, native, *, writes=(), read=None):
        if writes:
            self.memory[native.handle].update(ncc._shadow(writes))
            return ()
        row, offset, count = read
        return tuple(self.memory[native.handle].get((row, offset + i), 0) for i in range(count))

    def start(self, native):
        self.active.add(native.handle)

    def swap(self, current, candidate):
        self.active.remove(current.handle)
        self.active.add(candidate.handle)

    def destroy(self, native):
        self.active.discard(native.handle)
        del self.memory[native.handle]

    def strip_delays(self, previous=None, *, record=None, monitor=None, capacity=None):
        if previous is not None:
            self.strips = StripDelays(23, 1, record, monitor, capacity)
        return self.strips


def rig(providers, *, virtual=True):
    state, helper = StateTree(), HelperTree()
    for first in (0, 1, 2, 3, 14, 15, 16, 17):
        state.add_runtime_property(f'/devices/0/inputs/{first}/Stereo', {'type': 'bool', 'value': False})
        for index in range(4):
            for name, kind, value in [('EffectName', 'string', ''), ('EffectInstance', 'pointer', 0),
                                      ('Power', 'bool', False), ('Latency', 'int', 0), ('Status', 'string', '')]:
                state.add_runtime_property(f'/devices/0/inputs/{first}/effects/{index}/{name}',
                                           {'type': kind, 'value': value})
    publisher, device = Publisher(state, helper, owner='virtual-test'), Device()
    backend = NativeChainBackend(publisher, device, providers, composer=ncc.compose, max_instances=4,
                                 stereo=True, replacement=True, compensation_supported=True, virtual=virtual)
    return state, device, backend, PluginTransactions(publisher, backend)


def test_virtual_providers_need_the_driver_feature(make):
    providers = [make(0), make(14), make(14, 'Stereo', (14, 15))]
    *_, backend, _ = rig(providers, virtual=False)
    assert [c['slots'][0] for c in backend.capabilities()] == ['/devices/0/inputs/0/effects/0']
    *_, backend, _ = rig(providers)
    assert sorted((c['slots'][0], tuple(c['channels'])) for c in backend.capabilities()) == [
        ('/devices/0/inputs/0/effects/0', (0,)), (V1, (14,)), (V1, (14, 15))]


def test_a_plug_in_loads_on_virtual_1_and_unloads_to_the_plain_strip(make):
    state, device, backend, tx = rig([make(0), make(14)])
    assert tx.load('/devices/0/inputs/0/effects/0', 'Test').committed
    strips = device.strips
    assert tx.load(V1, 'Test').committed
    created = device.created[-1]
    assert created['input_pair'] == (0x18, 32) and created['chain'] is True
    words = ncc._shadow(created['commands'])
    assert words[0, 7] == native_carrier.INPUT_STRIP_PLUGIN and words[0, 9] == 0x456
    assert len(created['private_rows']) == 7
    assert state.get_value(V1 + '/EffectName') == 'Test'
    assert device.strips == strips                # the Mic strips' delays stay as they were
    assert len(device.active) == 2                # analog 1 and Virtual 1, one chain each
    # Its link is its own: Virtual 3/4 and the analog pairs are not held by it
    assert not backend.allows_idle_mixer_link_change('/devices/0/inputs/14/Stereo')
    assert backend.allows_idle_mixer_link_change('/devices/0/inputs/16/Stereo')
    assert not backend.allows_idle_mixer_link_change('/devices/0/inputs/2/Stereo')
    assert tx.unload(V1).committed
    assert state.get_value(V1 + '/EffectName') == '' and len(device.active) == 1


def test_a_stereo_plug_in_loads_on_virtual_1_2(make):
    state, device, backend, tx = rig([make(14, 'Test', (14, 15))])
    state.set_value('/devices/0/inputs/14/Stereo', True)
    assert tx.load(V1, 'Test').committed
    assert device.created[-1]['input_pair'] == (0x18, 0x19)
    assert not tx.load('/devices/0/inputs/15/effects/0', 'Test').committed
