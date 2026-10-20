"""Generic composite hosting over synthetic physical instances and real transactions."""
import copy
import errno
import os
from types import SimpleNamespace

import pytest

from native_carrier import input_pair
from plugin_driver import NativeInstance
from plugin_native import ControlPlan
from plugin_native_chain import NativeChainBackend
from plugin_publication import LoadedPlugin, Publisher
from plugin_transactions import PluginTransactions, Outcome
from state_tree import StateTree
from helper_tree import HelperTree
from test_plugin_native import Provider, FIELD


def slot(index, first=2):
    return f'/devices/0/inputs/{first}/effects/{index}'


class Source(Provider):
    def __init__(self, name='first', first=2, channels=None):
        self.name, self.input_index = name, first
        self.slot = slot(0, first)
        self.channels = (first,) if channels is None else channels
        self.reset()

    def initial(self, handle):
        result = super().initial(handle)
        parameter = copy.deepcopy(result.parameters[0])
        parameter['properties']['NormalizedValue']['value'] = self.word / 100
        parameter['properties']['StringValue']['value'] = str(self.word)
        return LoadedPlugin(self.name, handle, True, 0, (parameter,))


class View:
    def __init__(self, provider, index, row, channels):
        self.provider, self.base_row, self.channels = provider, row, channels
        self.slot, self.name, self.sample_rate = slot(index, channels[0]), provider.name, 48000
        self.remote_reads = {('floats', 45): (row, 0)}

    def initial(self, handle):
        return self.provider.initial(handle)

    def observations(self):
        return ((self.base_row, 0, (self.provider.word,)),)

    def plan(self, source):
        return ControlPlan((0x00140004, self.base_row << 24, 0, source.state),
                           ((self.base_row, 0, (source.state,)),), source.values, source)

    def snapshot(self, field):
        return self.plan(self.provider.snapshot(field))

    def plan_control(self, field, value):
        return self.plan(self.provider.plan_control(field, value))

    def plan_restore(self, prior, proposal):
        return self.plan(self.provider.plan_restore(prior.state, proposal.state))

    def accept(self, plan):
        self.provider.accept(plan.state)


class Composition:
    needs_staging = False  # fake startups stay under the single-CREATE command bound

    def __init__(self, providers, channels, slots, record_pre_effects=False):
        self.effects = tuple(View(p, index, 10+position, channels)
                             for position, (p, index) in enumerate(zip(providers, slots)))
        self.taps = self.record_tap_plan(record_pre_effects).observations if record_pre_effects else ()
        self.observations = tuple(o for view in self.effects for o in view.observations()) + self.taps
        self.channels = channels

    def create_arguments(self):
        return dict(render_id=0x806, dsp=1, input_pair=input_pair(self.channels),
            commands=tuple(w for view in self.effects for w in (0x00140004, view.base_row << 24, 0, view.provider.word))
                     + tuple(w for row, word, (value,) in self.taps for w in (0x00140004, row << 24 | word, 0, value)))

    def record_tap_plan(self, pre):
        words = {(1, 4): 4 if pre else 0, (self.effects[-1].base_row, 4): 0 if pre else 4}
        observations = tuple((row, word, (value,)) for (row, word), value in words.items())
        return ControlPlan(tuple(w for row, word, (value,) in observations
                                 for w in (0x00140004, row << 24 | word, 0, value)),
                           observations, {'RecordPreEffects': pre}, None)


def compose(providers, *, channels, slots, record_delay_samples, record_pre_effects=False):
    return Composition(providers, channels, slots, record_pre_effects)


class Driver:
    def __init__(self):
        self.calls, self.memory, self.active = [], {}, set()
        self.sequence, self.valid, self.fail, self.mismatch = 6, True, None, False

    def invalidate(self):
        self.valid = False

    def check(self, operation):
        if not self.valid or self.fail == operation:
            raise OSError(errno.EIO, 'synthetic device failure')

    def create(self, **arguments):
        self.check('create')
        self.pairs = getattr(self, 'pairs', []) + [arguments['input_pair']]
        self.sequence += 1
        native = NativeInstance(self.sequence, 1, 23)
        self.calls.append(('create', native.handle, arguments.get('stage', False), arguments.get('chain', False)))
        self.memory[native.handle] = {}
        self.service(native, writes=arguments['commands'])
        if not arguments.get('stage'):
            self.active.add(native.handle)
        return native

    def service(self, native, *, writes=(), read=None):
        self.check('service')
        if writes:
            for i in range(0, len(writes), 4):
                self.memory[native.handle][writes[i+1] >> 24, writes[i+1] & 0xffffff] = writes[i+3]
            self.calls.append(('write', native.handle, tuple(writes)))
            return ()
        self.calls.append(('read', native.handle, read))
        row, offset, count = read
        return tuple(self.memory[native.handle].get((row, offset+i), 0) + int(self.mismatch) for i in range(count))

    def start(self, native):
        self.check('start')
        assert native.handle not in self.active
        self.active.add(native.handle)
        self.calls.append(('start', native.handle))

    def swap(self, current, candidate):
        self.check('swap')
        if getattr(self, 'refuse_swap', None):
            raise OSError(self.refuse_swap, os.strerror(self.refuse_swap))
        assert current.handle in self.active and candidate.handle not in self.active
        self.active.remove(current.handle)
        self.active.add(candidate.handle)
        self.calls.append(('swap', current.handle, candidate.handle))

    def destroy(self, native):
        self.check('destroy')
        self.active.discard(native.handle)
        del self.memory[native.handle]
        self.calls.append(('destroy', native.handle))


def model(maximum=4):
    state, helper = StateTree(), HelperTree()
    for first in range(4):
        state.add_runtime_property(f'/devices/0/inputs/{first}/Stereo', {'type': 'bool', 'value': False})
        for index in range(4):
            for name, kind, value in [('EffectName', 'string', ''), ('EffectInstance', 'pointer', 0),
                                      ('Power', 'bool', False), ('Latency', 'int', 0), ('Status', 'string', '')]:
                state.add_runtime_property(slot(index, first)+'/'+name, {'type': kind, 'value': value})
    publisher, driver = Publisher(state, helper, owner='chain-test'), Driver()
    sources = [Source(name, first) for first in (0, 2) for name in ('first', 'second')]
    sources += [Source(name, first, (first, first+1)) for first in (0, 2) for name in ('first', 'second')]
    backend = NativeChainBackend(publisher, driver, sources, composer=compose,
                                 max_instances=maximum, stereo=True, replacement=True)
    return state, helper, driver, backend, PluginTransactions(publisher, backend)


def test_chain_rebuild_preserves_neighbors_identity_parameters_and_real_meter_rows():
    state, helper, driver, backend, tx = model()
    assert tx.load(slot(0), 'first').committed
    assert tx.control(slot(0), FIELD, .73).committed
    original = tx.current_remote_handle(slot(0))
    old_native = backend._leases[slot(0)].native
    assert tx.load(slot(2), 'second').committed
    assert tx.current_remote_handle(slot(0)) == original
    assert state.get_value(slot(0)+'/'+FIELD) == helper.get_value(slot(0)+'/'+FIELD) == .73
    current = backend._leases[slot(0)]
    assert current.native != old_native and old_native.handle not in driver.memory
    assert current.native == backend._leases[slot(2)].native
    assert len(driver.active) == 1
    query = SimpleNamespace(kind='floats', ids=(45,))
    assert backend.read(slot(0), original, query).words == (73,)
    assert backend.read(slot(2), tx.current_remote_handle(slot(2)), query).words == (50,)
    assert driver.calls[-1][2] == (11, 0, 1)
    assert tx.control(slot(2), FIELD, .22).committed
    assert backend.read(slot(0), original, query).words == (73,)
    assert any(call[:1] == ('swap',) for call in driver.calls)
    assert all(call[3] is True for call in driver.calls if call[0] == 'create')


def test_load_publication_failure_restores_whole_chain_and_original_adapters():
    state, helper, driver, backend, tx = model()
    assert tx.load(slot(0), 'first').committed
    assert tx.control(slot(0), FIELD, .81).committed
    old = backend._leases[slot(0)]
    old_tree = copy.deepcopy(state.root), copy.deepcopy(helper.root)
    tx.publication_fault = lambda stage: (_ for _ in ()).throw(RuntimeError('injected publication'))
    result = tx.load(slot(1), 'second')
    assert result.error and not result.quarantined and not result.committed
    assert backend._leases[slot(0)] is old and slot(1) not in backend._leases
    assert driver.active == {old.native.handle} and set(driver.memory) == driver.active
    assert (state.root, helper.root) == old_tree
    assert not backend._pending
    tx.publication_fault = None
    assert tx.control(slot(0), FIELD, .41).committed


def test_replacement_new_logical_identity_and_middle_removal_relocate_survivors():
    state, helper, driver, backend, tx = model()
    for index in range(3):
        assert tx.load(slot(index), 'first').committed
        assert tx.control(slot(index), FIELD, .6+index*.1).committed
    identities = [tx.current_remote_handle(slot(i)) for i in range(3)]
    assert tx.load(slot(1), 'second').committed
    assert tx.current_remote_handle(slot(1)) != identities[1]
    assert tx.current_remote_handle(slot(0)) == identities[0]
    assert tx.current_remote_handle(slot(2)) == identities[2]
    assert tx.unload(slot(1)).committed
    assert state.get_value(slot(1)+'/EffectInstance') == 0
    assert backend._leases[slot(2)].provider.base_row == 11
    assert tx.current_remote_handle(slot(2)) == identities[2]
    assert tx.control(slot(2), FIELD, .91).committed
    assert tx.unload(slot(0)).committed
    assert backend._leases[slot(2)].provider.base_row == 10
    assert tx.unload(slot(2)).committed
    assert not driver.active and not driver.memory and not backend._leases and not backend._chains


def test_failed_candidate_readback_retires_only_candidate():
    state, helper, driver, backend, tx = model()
    assert tx.load(slot(0), 'first').committed
    previous = backend._leases[slot(0)]
    driver.mismatch = True
    result = tx.load(slot(1), 'second')
    assert result.error and not result.quarantined
    assert backend._leases[slot(0)] is previous
    assert set(driver.memory) == driver.active == {previous.native.handle}
    result = tx.unload(slot(0))
    assert result.committed


def test_chain_capacity_counts_physical_layout_and_rejects_unknown_slots():
    state, helper, driver, backend, tx = model(maximum=1)
    for i in range(4):
        assert tx.load(slot(i), 'first').committed
    before = len(driver.calls)
    assert tx.load(slot(0, 0), 'first').error
    assert tx.load(slot(0), 'missing').error
    assert len(driver.calls) == before
    assert len(driver.active) == len(backend._chains) == 1
    assert len(backend._leases) == 4
    assert len(backend.capabilities()[0]['slots']) == 4


def test_logical_identity_does_not_alias_recycled_kernel_handles_or_exceed_double_precision():
    state, helper, driver, backend, tx = model()
    assert tx.load(slot(0), 'first').committed
    first = tx.current_remote_handle(slot(0))
    assert tx.unload(slot(0)).committed
    driver.sequence = 6
    assert tx.load(slot(0), 'first').committed
    second = tx.current_remote_handle(slot(0))
    assert first.value & 0xffffffff == second.value & 0xffffffff == 7
    assert second != first and int(float(second.value)) == second.value
    with pytest.raises(ValueError):
        backend.read(slot(0), first, SimpleNamespace(kind='floats', ids=(45,)))
    backend._serial = 0x1fffff
    before = len(driver.calls)
    assert tx.load(slot(1), 'first').error
    assert len(driver.calls) == before


def test_stereo_chain_uses_leader_and_shares_one_native_instance():
    state, helper, driver, backend, tx = model()
    for first in (2, 3):
        state.set_value(f'/devices/0/inputs/{first}/Stereo', True)
    assert tx.load(slot(0), 'first').committed
    assert tx.load(slot(3), 'second').committed
    assert len(backend._chains) == len(driver.active) == 1
    assert backend._leases[slot(3)].provider.channels == (2, 3)
    before = len(driver.calls)
    assert tx.load(slot(0, 3), 'first').error
    assert len(driver.calls) == before


@pytest.mark.parametrize('operation', ['create', 'swap', 'destroy'])
def test_unknown_physical_outcomes_quarantine(operation):
    state, helper, driver, backend, tx = model()
    assert tx.load(slot(0), 'first').committed
    driver.fail = operation
    result = tx.load(slot(1), 'second')
    assert result.quarantined and not backend.publisher.connected and not driver.valid


def test_unload_publication_failure_quarantines_after_real_retirement():
    state, helper, driver, backend, tx = model()
    assert tx.load(slot(0), 'first').committed
    assert tx.load(slot(1), 'second').committed
    prior = backend._leases[slot(0)].native
    tx.publication_fault = lambda stage: (_ for _ in ()).throw(RuntimeError('injected publication'))
    result = tx.unload(slot(0))
    assert result.quarantined
    assert prior.handle not in driver.memory
    assert slot(0) not in backend._leases
    assert backend._leases[slot(1)].native.handle in driver.active


def test_middle_removal_readback_failure_keeps_every_previous_lease_and_chain():
    state, helper, driver, backend, tx = model()
    for index in range(3):
        assert tx.load(slot(index), 'first').committed
    previous = dict(backend._leases)
    old_tree = copy.deepcopy(state.root), copy.deepcopy(helper.root)
    driver.mismatch = True
    result = tx.unload(slot(1))
    assert result.error and not result.quarantined and not result.committed
    assert backend._leases == previous
    assert (state.root, helper.root) == old_tree
    assert set(driver.memory) == driver.active == {previous[slot(0)].native.handle}


def test_old_control_token_cannot_write_relocated_rows_after_neighbor_rebuild():
    state, helper, driver, backend, tx = model()
    assert tx.load(slot(2), 'first').committed
    handle = tx.current_remote_handle(slot(2))
    prepared = backend.prepare_control(slot(2), handle, FIELD, .9).prepared
    assert tx.load(slot(0), 'second').committed
    assert tx.current_remote_handle(slot(2)) == handle
    before = len(driver.calls)
    with pytest.raises(ValueError, match='another lease'):
        backend.apply_control(prepared)
    assert len(driver.calls) == before
    assert tx.control(slot(2), FIELD, .9).committed
    assert backend._leases[slot(2)].provider.base_row == 11


def test_rebuilding_one_input_chain_does_not_move_an_independent_chain():
    state, helper, driver, backend, tx = model()
    assert tx.load(slot(0, 0), 'first').committed
    untouched = backend._leases[slot(0, 0)]
    assert tx.load(slot(0), 'first').committed
    assert tx.load(slot(1), 'second').committed
    assert backend._leases[slot(0, 0)] is untouched
    assert untouched.native.handle in driver.active
    assert len(driver.active) == len(backend._chains) == 2
    assert tx.control(slot(0, 0), FIELD, .66).committed


def test_composer_identity_mismatch_fails_before_acquiring_hardware():
    state, helper, driver, backend, tx = model()
    real = backend.composer
    def invalid(*args, **kwargs):
        result = real(*args, **kwargs)
        result.effects[0].slot = slot(3)
        return result
    backend.composer = invalid
    result = tx.load(slot(0), 'first')
    assert result.error and not result.quarantined
    assert not driver.calls


def test_retained_controls_survive_add_replace_remove():
    state, helper, driver, backend, tx = model()
    assert backend.native_single_insert is True and backend.chain_supported
    assert tx.load(slot(0), 'first').committed
    assert tx.control(slot(0), FIELD, .73).committed
    handle = tx.current_remote_handle(slot(0))
    for action in (lambda: tx.load(slot(1), 'second'),
                   lambda: tx.load(slot(1), 'first'), lambda: tx.unload(slot(1))):
        assert action().committed
        retained = backend._leases[slot(0)].loaded
        assert retained.handle == handle and retained.power is True
        assert retained.parameters[0]['properties']['NormalizedValue']['value'] == .73
        assert state.get_value(slot(0)+'/Power') is helper.get_value(slot(0)+'/Power') is True


def test_retained_public_encoder_disagreement_rejected_before_create():
    from plugin_publication import ControlReadback
    state, helper, driver, backend, tx = model()
    assert tx.load(slot(0), 'first').committed
    handle = tx.current_remote_handle(slot(0))
    backend.publisher.stage_control(slot(0), ControlReadback(handle, FIELD,
        {'NormalizedValue': .91, 'StringValue': '91'})).commit()
    before = list(driver.calls)
    result = tx.load(slot(1), 'second')
    assert result.error and not result.committed and not result.quarantined
    assert driver.calls == before
    assert backend._leases[slot(0)].provider.provider.word == 50


class DelayDriver(Driver):
    def __init__(self):
        super().__init__()
        from plugin_driver import StripDelays
        self.strips = StripDelays(23, 1, (200,)*4, (0,)*4, (256,)*4)
        self.strip_mismatch = False
        self.after_swap_mismatch = False

    def strip_delays(self, previous=None, *, record=None, monitor=None, capacity=None):
        from dataclasses import replace
        self.check('strips')
        if previous is not None:
            assert previous == self.strips
            self.strips = replace(previous, record=record, monitor=monitor, capacity=capacity)
            if self.strip_mismatch:
                self.strip_mismatch = False
                self.strips = replace(self.strips, record=(77,)*4)
        self.calls.append(('strips', previous, self.strips))
        return self.strips

    def swap(self, current, candidate):
        super().swap(current, candidate)
        if self.after_swap_mismatch:
            self.memory[candidate.handle][1, 8] = 999
            self.after_swap_mismatch = False


def delay_compose(providers, *, channels, slots, record_delay_samples,
                  monitor_delay_samples=0, active_capacity=256, record_pre_effects=False):
    result = compose(providers, channels=channels, slots=slots,
                     record_delay_samples=record_delay_samples, record_pre_effects=record_pre_effects)
    observations = ((1, 16, (record_delay_samples,)),
                    (1, 8, (monitor_delay_samples,)), (1, 5, (active_capacity,)))
    result.observations += observations
    original = result.create_arguments
    def arguments():
        args = original()
        args['commands'] += tuple(word for row, offset, (value,) in observations
                                 for word in (0x00140004, row << 24 | offset, 0, value))
        return args
    result.create_arguments = arguments
    return result


def delay_model():
    state, helper, _, prototype, _ = model()
    sources = [Source(name, first) for first in (0, 2) for name in ('first', 'second')]
    for source in sources:
        source.descriptor_latency_samples = 0
    driver = DelayDriver()
    backend = NativeChainBackend(prototype.publisher, driver, sources, composer=delay_compose,
        max_instances=4, stereo=True, replacement=True, compensation_supported=True)
    return state, helper, driver, backend, PluginTransactions(backend.publisher, backend)


def test_compensation_rebuilds_all_chains_and_plain_strips_preserving_ids_and_controls():
    state, helper, driver, backend, tx = delay_model()
    for first in (0, 2):
        assert tx.load(slot(0, first), 'first').committed
        assert tx.control(slot(0, first), FIELD, .71).committed
    old = dict(backend._leases)
    for setting, record, capacity in [('Off', 0, 0), ('Short', 100, 256),
                                      ('Long', 1000, 2048), ('Medium', 200, 256)]:
        outcome = backend.set_compensation(setting)
        assert outcome.status is Outcome.CONFIRMED
        assert driver.strips.record == (record,)*4
        assert driver.strips.monitor == (0,)*4
        assert driver.strips.capacity == (capacity,)*4
        assert backend.compensation_setting == setting
        for path, lease in backend._leases.items():
            assert lease.loaded.handle == old[path].loaded.handle
            assert lease.loaded.parameters[0]['properties']['NormalizedValue']['value'] == .71
            assert driver.memory[lease.native.handle][1, 16] == record
            assert driver.memory[lease.native.handle][1, 8] == 0
            assert driver.memory[lease.native.handle][1, 5] == capacity
            assert lease.native != old[path].native
        assert not backend._compensating and len(driver.memory) == len(driver.active) == 2
    assert tx.load(slot(1), 'second').committed
    assert tx.unload(slot(0)).committed
    assert driver.memory[backend._leases[slot(1)].native.handle][1,16] == 200


@pytest.mark.parametrize('failure', ['candidate_read', 'strip_read', 'activated_read'])
def test_compensation_known_mismatch_restores_whole_previous_graph(failure):
    state, helper, driver, backend, tx = delay_model()
    for first in (0, 2):
        assert tx.load(slot(0, first), 'first').committed
    old, strips = dict(backend._leases), driver.strips
    if failure == 'candidate_read': driver.mismatch = True
    if failure == 'strip_read': driver.strip_mismatch = True
    if failure == 'activated_read': driver.after_swap_mismatch = True
    outcome = backend.set_compensation('Long')
    assert outcome.status is Outcome.FAILED
    assert backend._leases == old and driver.strips == strips
    assert set(driver.memory) == driver.active == {lease.native.handle for lease in old.values()}
    assert backend.compensation_setting == 'Medium' and not backend._compensating


@pytest.mark.parametrize('operation', ['strips', 'create', 'swap', 'destroy'])
def test_compensation_uncertain_device_call_invalidates(operation):
    state, helper, driver, backend, tx = delay_model()
    assert tx.load(slot(0), 'first').committed
    driver.fail = operation
    assert backend.set_compensation('Off').status is Outcome.INDETERMINATE
    assert not driver.valid and backend._compensating


def test_compensation_groups_nonzero_descriptors_and_subsequent_topology():
    state, helper, driver, backend, tx = delay_model()
    for first in (0, 2):
        assert tx.load(slot(0, first), 'first').committed
    # Model authoritative descriptors before exercising setting and topology.
    backend._chains[(0,)].sources[slot(0, 0)].descriptor_latency_samples = 120
    backend._chains[(2,)].sources[slot(0, 2)].descriptor_latency_samples = 31
    result = backend.set_compensation('Short')
    assert result.status is Outcome.CONFIRMED
    plans = {item.channels: item for item in result.inputs}
    assert (plans[(0,)].record, plans[(0,)].monitor) == (0, 0)
    assert (plans[(2,)].record, plans[(2,)].monitor) == (69, 69)
    assert plans[(0,)].overloaded and plans[(0,)].suggested == 'Medium'
    assert plans[(1,)].monitor == plans[(3,)].monitor == 100
    assert tx.load(slot(1), 'second').committed
    assert tx.unload(slot(0)).committed
    assert driver.memory[backend._leases[slot(1)].native.handle][1,8] == 100
    assert tx.unload(slot(0, 0)).committed
    assert driver.strips.monitor == (0,)*4
    assert driver.memory[backend._leases[slot(1)].native.handle][1,8] == 0


def test_compensation_invalid_metadata_and_settings_fail_before_io():
    state, helper, driver, backend, tx = delay_model()
    assert tx.load(slot(0), 'first').committed
    calls = list(driver.calls)
    assert backend.set_compensation('invalid').status is Outcome.FAILED
    state.add_runtime_property('/devices/0/inputs/2/DelayCompGroup', {'type':'int','value':32})
    assert backend.set_compensation('Long').status is Outcome.FAILED
    assert driver.calls == calls


def test_nonzero_insert_realigns_neighbor_and_plain_inputs_then_removal_restores():
    state, helper, driver, backend, tx = delay_model()
    assert tx.load(slot(0, 0), 'first').committed
    assert tx.control(slot(0, 0), FIELD, .83).committed
    neighbor = backend._leases[slot(0, 0)]
    backend.providers[(slot(0), 'second', (2,))].descriptor_latency_samples = 31
    assert tx.load(slot(0), 'second').committed
    rebuilt = backend._leases[slot(0, 0)]
    assert rebuilt.loaded.handle == neighbor.loaded.handle and rebuilt.native != neighbor.native
    assert rebuilt.loaded.parameters[0]['properties']['NormalizedValue']['value'] == .83
    assert driver.memory[rebuilt.native.handle][1,8] == 31
    assert driver.memory[backend._leases[slot(0)].native.handle][1,16] == 169
    assert driver.strips.monitor == (31,31,0,31)
    assert driver.strips.record == (200,200,169,200)
    assert tx.unload(slot(0)).committed
    final = backend._leases[slot(0,0)]
    assert final.loaded.handle == neighbor.loaded.handle
    assert driver.memory[final.native.handle][1,8] == 0
    assert driver.strips.monitor == (0,)*4 and driver.strips.record == (200,)*4
    assert len(driver.active) == len(driver.memory) == 1


def test_nonzero_load_publication_failure_restores_neighbors_strips_and_identity():
    state, helper, driver, backend, tx = delay_model()
    assert tx.load(slot(0,0), 'first').committed
    old, strips = dict(backend._leases), driver.strips
    backend.providers[(slot(0), 'second', (2,))].descriptor_latency_samples = 31
    tx.publication_fault = lambda stage: (_ for _ in ()).throw(RuntimeError('publication'))
    result = tx.load(slot(0), 'second')
    assert result.error and not result.quarantined and not result.committed
    assert backend._leases == old and driver.strips == strips
    assert set(driver.memory) == driver.active == {lease.native.handle for lease in old.values()}
    assert not backend._pending


@pytest.mark.parametrize('failure', ['candidate_read', 'strip_read', 'activated_read'])
def test_nonzero_topology_known_failure_restores_all_neighbors(failure):
    state, helper, driver, backend, tx = delay_model()
    assert tx.load(slot(0,0), 'first').committed
    backend.providers[(slot(0), 'second', (2,))].descriptor_latency_samples = 31
    old, strips = dict(backend._leases), driver.strips
    if failure == 'candidate_read': driver.mismatch = True
    if failure == 'strip_read': driver.strip_mismatch = True
    if failure == 'activated_read': driver.after_swap_mismatch = True
    result = tx.load(slot(0), 'second')
    assert result.error and not result.committed and not result.quarantined
    assert backend._leases == old and driver.strips == strips
    assert set(driver.memory) == driver.active == {lease.native.handle for lease in old.values()}
    assert not backend._pending


def test_nonzero_middle_removal_realigns_remaining_effects_and_independent_chain():
    state, helper, driver, backend, tx = delay_model()
    backend.providers[(slot(0), 'second', (2,))].descriptor_latency_samples = 31
    assert tx.load(slot(0,0), 'first').committed
    assert tx.load(slot(0), 'first').committed
    assert tx.load(slot(1), 'second').committed
    assert tx.load(slot(2), 'first').committed
    identities = {path: lease.loaded.handle for path, lease in backend._leases.items()}
    assert tx.unload(slot(1)).committed
    for path, lease in backend._leases.items():
        assert lease.loaded.handle == identities[path]
        assert driver.memory[lease.native.handle][1,8] == 0
    assert backend._leases[slot(2)].provider.base_row == 11
    assert driver.strips.monitor == (0,)*4 and driver.strips.record == (200,)*4


def test_nonzero_replacement_can_reduce_group_maximum_and_keeps_other_ids():
    state, helper, driver, backend, tx = delay_model()
    backend.providers[(slot(0), 'second', (2,))].descriptor_latency_samples = 31
    assert tx.load(slot(0,0), 'first').committed
    assert tx.load(slot(0), 'second').committed
    old = {path: lease.loaded.handle for path, lease in backend._leases.items()}
    assert tx.load(slot(0), 'first').committed
    assert backend._leases[slot(0,0)].loaded.handle == old[slot(0,0)]
    assert backend._leases[slot(0)].loaded.handle != old[slot(0)]
    assert driver.strips.monitor == (0,)*4


@pytest.mark.parametrize('operation', ['strips', 'create', 'swap', 'destroy'])
def test_nonzero_topology_unknown_device_operation_quarantines(operation):
    state, helper, driver, backend, tx = delay_model()
    assert tx.load(slot(0,0), 'first').committed
    backend.providers[(slot(0), 'second', (2,))].descriptor_latency_samples = 31
    driver.fail = operation
    result = tx.load(slot(0), 'second')
    assert result.error and result.quarantined
    assert not driver.valid


def test_nonzero_removal_readback_failure_preserves_previous_graph():
    state, helper, driver, backend, tx = delay_model()
    backend.providers[(slot(0), 'second', (2,))].descriptor_latency_samples = 31
    assert tx.load(slot(0,0), 'first').committed
    assert tx.load(slot(0), 'second').committed
    before, strips = dict(backend._leases), driver.strips
    driver.after_swap_mismatch = True
    result = tx.unload(slot(0))
    assert result.error and not result.committed and not result.quarantined
    assert backend._leases == before and driver.strips == strips
    assert set(driver.memory) == driver.active == {lease.native.handle for lease in before.values()}


def test_compensation_disabled_rejects_nonzero_provider_before_io():
    state, helper, driver, backend, tx = model()
    backend.providers[(slot(0), 'second', (2,))].descriptor_latency_samples = 31
    assert not tx.load(slot(0), 'second').committed
    assert not driver.calls


@pytest.mark.parametrize('group', [1, -1])
def test_plain_input_nondefault_group_is_preserved_in_topology_and_setting(group):
    state, helper, driver, backend, tx = delay_model()
    state.add_runtime_property('/devices/0/inputs/1/DelayCompGroup', {'type':'int','value':group})
    backend.providers[(slot(0), 'second', (2,))].descriptor_latency_samples = 31
    assert tx.load(slot(0), 'second').committed
    assert driver.strips.monitor == (31,0,0,31)
    assert backend.set_compensation('Long').status is Outcome.CONFIRMED
    assert driver.strips.monitor == (31,0,0,31)
    assert tx.unload(slot(0)).committed
    assert driver.strips.monitor == (0,)*4


def test_plain_input_rejoins_its_configured_group_when_last_effect_removed():
    state, helper, driver, backend, tx = delay_model()
    for channel in (0, 2):
        state.add_runtime_property(f'/devices/0/inputs/{channel}/DelayCompGroup', {'type':'int','value':1})
    backend.providers[(slot(0), 'second', (2,))].descriptor_latency_samples = 31
    assert tx.load(slot(0,0), 'first').committed
    assert tx.load(slot(0), 'second').committed
    assert driver.strips.monitor == (31,0,0,0)
    assert tx.unload(slot(0,0)).committed
    assert driver.strips.monitor == (31,0,0,0)
    assert tx.unload(slot(0)).committed
    assert driver.strips.monitor == (0,)*4



class StagedComposition(Composition):
    """A startup over one command page: created staged, nothing routes it on its own."""
    needs_staging = True

    def stage_bundle(self):
        return dict(create_arguments=dict(self.create_arguments(), stage=True), initialize_chunks=())


def staged_model(start_staged):
    state, helper, driver, backend, tx = model()
    backend.composer = lambda providers, **kw: (StagedComposition if any(p.name == 'second' for p in providers)
                                                else Composition)(providers, kw['channels'], kw['slots'])
    backend.start_staged = start_staged
    return state, helper, driver, backend, tx


def test_a_staged_chain_on_an_empty_input_is_started_by_the_driver():
    state, helper, driver, backend, tx = staged_model(True)
    assert tx.load(slot(0), 'second').committed
    create = next(c for c in driver.calls if c[0] == 'create')
    assert create[2] is True and create[3] is True            # staged, and still a chain
    assert ('start', create[1]) in driver.calls and create[1] in driver.active


def test_an_old_driver_refuses_a_staged_load_instead_of_a_silent_plugin():
    state, helper, driver, backend, tx = staged_model(False)
    result = tx.load(slot(0), 'second')
    assert not result.committed and not result.quarantined and 'update the driver' in result.detail
    assert not driver.active and not any(c[0] == 'create' for c in driver.calls)
    assert tx.load(slot(0), 'first').committed                 # nothing else is affected


def test_a_swap_the_driver_refuses_rolls_back_without_quarantine():
    state, helper, driver, backend, tx = model()
    assert tx.load(slot(0), 'first').committed
    kept = backend._leases[slot(0)].native
    driver.refuse_swap = errno.EINVAL
    result = tx.load(slot(2), 'second')                         # rebuilds input 1's chain: a swap
    assert not result.committed and not result.quarantined and not tx.quarantined
    assert driver.valid and backend._leases[slot(0)].native == kept and kept.handle in driver.active
    driver.refuse_swap = None
    assert tx.load(slot(2), 'second').committed


# UAD REC / MON (RecordPreEffects): the record tap moves between the last insert and the Mic strip.

def publish_record_pre(state, channel, pre):
    state.add_runtime_property(f'/devices/0/inputs/{channel}/RecordPreEffects', {'type': 'bool', 'value': pre})


def test_record_tap_without_a_chain_does_no_device_io():
    state, helper, driver, backend, tx = model()
    assert backend.set_record_tap(2, True).status is Outcome.CONFIRMED
    state.add_runtime_property('/devices/0/inputs/0/Stereo', {'type': 'bool', 'value': True})
    assert tx.load(slot(0, 0), 'first').committed
    calls = list(driver.calls)
    # The second input of a pair follows the first: nothing to move for it.
    assert backend.set_record_tap(1, True).status is Outcome.CONFIRMED
    assert backend.set_record_tap(9, False).status is Outcome.CONFIRMED
    assert driver.calls == calls


def test_record_tap_flips_live_with_one_service_and_readback():
    state, helper, driver, backend, tx = model()
    assert tx.load(slot(0), 'first').committed
    native = backend._leases[slot(0)].native
    memory = driver.memory[native.handle]
    assert memory.get((1, 4), 0) == 0
    for pre, mic, effect in [(True, 4, 0), (False, 0, 4)]:
        calls = len(driver.calls)
        outcome = backend.set_record_tap(2, pre)
        assert outcome.status is Outcome.CONFIRMED
        new = driver.calls[calls:]
        assert [call[0] for call in new] == ['write', 'read', 'read']
        assert (memory[1, 4], memory[10, 4]) == (mic, effect)
        assert backend._leases[slot(0)].native is native
        publish_record_pre(state, 2, pre)


def test_record_tap_write_that_does_not_land_restores_the_previous_tap():
    state, helper, driver, backend, tx = model()
    assert tx.load(slot(0), 'first').committed
    assert backend.set_record_tap(2, False).status is Outcome.CONFIRMED
    service, dropped = driver.service, []

    def drop_first_write(native, *, writes=(), read=None):
        if writes and not dropped:
            dropped.append(writes)
            return ()
        return service(native, writes=writes, read=read)
    driver.service = drop_first_write
    outcome = backend.set_record_tap(2, True)
    assert outcome.status is Outcome.FAILED and outcome.detail == 'Previous record tap restored'
    memory = driver.memory[backend._leases[slot(0)].native.handle]
    assert (memory[1, 4], memory[10, 4]) == (0, 4) and driver.valid


def test_record_tap_device_error_is_indeterminate():
    state, helper, driver, backend, tx = model()
    assert tx.load(slot(0), 'first').committed
    driver.fail = 'service'
    assert backend.set_record_tap(2, True).status is Outcome.INDETERMINATE
    assert not driver.valid


@pytest.mark.parametrize('first,channels', [(2, (2,)), (0, (0, 1))])
def test_chain_composed_in_mon_starts_with_the_mic_strip_tap(first, channels):
    state, helper, driver, backend, tx = model()
    state.add_runtime_property(f'/devices/0/inputs/{first}/Stereo', {'type': 'bool', 'value': len(channels) == 2})
    publish_record_pre(state, first, True)
    assert tx.load(slot(0, first), 'first').committed
    memory = driver.memory[backend._leases[slot(0, first)].native.handle]
    assert memory[1, 4] == 4 and memory.get((10, 4), 0) == 0


def test_record_tap_with_compensation_rebuilds_when_the_record_delay_moves():
    state, helper, driver, backend, tx = delay_model()
    backend.providers[(slot(0), 'first', (2,))].descriptor_latency_samples = 31
    assert tx.load(slot(0), 'first').committed
    assert tx.control(slot(0), FIELD, .71).committed
    old = backend._leases[slot(0)]
    assert driver.memory[old.native.handle][1, 16] == 169 and driver.strips.record[2] == 169
    outcome = backend.set_record_tap(2, True)
    assert outcome.status is Outcome.CONFIRMED
    new = backend._leases[slot(0)]
    assert new.native != old.native and new.loaded.handle == old.loaded.handle
    assert new.loaded.parameters[0]['properties']['NormalizedValue']['value'] == .71
    memory = driver.memory[new.native.handle]
    assert memory[1, 16] == 200 and memory[1, 4] == 4 and driver.strips.record[2] == 200
    assert {item.channels: item.record for item in backend.compensation_inputs}[(2,)] == 200
    assert len(driver.memory) == len(driver.active) == 1 and not backend._pending
    publish_record_pre(state, 2, True)
    assert backend.set_record_tap(2, False).status is Outcome.CONFIRMED
    assert driver.memory[backend._leases[slot(0)].native.handle][1, 16] == 169


def test_record_tap_with_compensation_flips_live_when_the_delay_stays():
    state, helper, driver, backend, tx = delay_model()
    assert tx.load(slot(0), 'first').committed
    calls = len(driver.calls)
    assert backend.set_record_tap(2, True).status is Outcome.CONFIRMED
    assert [call[0] for call in driver.calls[calls:]] == ['write', 'read', 'read']


@pytest.mark.parametrize('failure', ['candidate_read', 'strip_read'])
def test_record_tap_rebuild_readback_failure_restores_the_previous_chain(failure):
    state, helper, driver, backend, tx = delay_model()
    backend.providers[(slot(0), 'first', (2,))].descriptor_latency_samples = 31
    assert tx.load(slot(0), 'first').committed
    old, strips = dict(backend._leases), driver.strips
    driver.mismatch = failure == 'candidate_read'
    driver.strip_mismatch = failure == 'strip_read'
    outcome = backend.set_record_tap(2, True)
    assert outcome.status is Outcome.FAILED and outcome.detail == 'Previous record tap restored'
    assert backend._leases == old and driver.strips == strips and not backend._pending
    assert set(driver.memory) == driver.active == {old[slot(0)].native.handle}


def test_refused_candidate_readback_fails_only_that_load():
    # The driver refusing a readback of a load's candidate (EPROTO: a reply that does not
    # match the read) fails that load and retires the candidate; hosting stays up.
    state, helper, driver, backend, tx = delay_model()
    assert tx.load(slot(0), 'first').committed
    previous = backend._leases[slot(0)]
    service = driver.service
    refused = []

    def refuse_reads(native, *, writes=(), read=None):
        if read is not None and not writes and native.handle != previous.native.handle and not refused:
            refused.append(read)
            raise OSError(errno.EPROTO, 'synthetic reply mismatch')
        return service(native, writes=writes, read=read)

    driver.service = refuse_reads
    result = tx.load(slot(1), 'second')
    assert refused and result.error and not result.quarantined and not tx.quarantined
    assert backend._leases[slot(0)] is previous
    assert set(driver.memory) == driver.active == {previous.native.handle}
    assert tx.load(slot(1), 'second').committed
    assert tx.control(slot(1), FIELD, .4).committed


def virtual_model(virtual_inputs=True, compensation=False):
    """model() plus Virtual 1-4 (tree inputs 14-17) with mono and Virtual 1/2 stereo sources."""
    state, helper, _, prototype, _ = model()
    for first in (14, 15, 16, 17):
        state.add_runtime_property(f'/devices/0/inputs/{first}/Stereo', {'type': 'bool', 'value': False})
        for index in range(4):
            for name, kind, value in [('EffectName', 'string', ''), ('EffectInstance', 'pointer', 0),
                                      ('Power', 'bool', False), ('Latency', 'int', 0), ('Status', 'string', '')]:
                state.add_runtime_property(slot(index, first)+'/'+name, {'type': kind, 'value': value})
    sources = [Source(name, first) for first in (0, 14, 15) for name in ('first', 'second')]
    sources += [Source('first', 14, (14, 15))]
    for source in sources:
        source.descriptor_latency_samples = 0
    driver = DelayDriver() if compensation else Driver()
    backend = NativeChainBackend(prototype.publisher, driver, sources, max_instances=4,
                                 composer=delay_compose if compensation else compose,
                                 compensation_supported=compensation,
                                 stereo=True, replacement=True, virtual_inputs=virtual_inputs)
    return state, driver, backend, PluginTransactions(backend.publisher, backend)


def test_virtual_inputs_take_chains_on_their_own_dsp_inputs():
    """Virtual 1 is tree input 14 and DSP input 0x18; its chain is independent of analog 1's."""
    state, driver, backend, tx = virtual_model()
    assert tx.load(slot(0, 14), 'first').committed
    assert tx.load(slot(1, 14), 'second').committed
    assert tx.load(slot(0, 0), 'first').committed
    assert tx.load(slot(0, 15), 'first').committed
    assert set(backend._chains) == {(14,), (0,), (15,)}
    assert driver.pairs[0] == (0x18, 32) and (0, 32) in driver.pairs and (0x19, 32) in driver.pairs
    assert tx.load(slot(2, 14), 'nothing').error
    assert backend.prepare('/devices/0/inputs/14/preamps/0/effects/0', 'first').status is Outcome.FAILED
    assert backend.prepare('/devices/0/inputs/18/effects/0', 'first').status is Outcome.FAILED
    for path in list(backend._leases):
        assert tx.unload(path).committed
    for first in (14, 15):
        state.set_value(f'/devices/0/inputs/{first}/Stereo', True)
    assert tx.load(slot(0, 14), 'first').committed
    assert backend._leases[slot(0, 14)].provider.channels == (14, 15) and driver.pairs[-1] == (0x18, 0x19)


def test_an_old_driver_offers_no_virtual_slot():
    _, driver, backend, tx = virtual_model(virtual_inputs=False)
    assert {p.slot for p in backend.providers.values()} == {slot(0, 0)}
    assert all('/inputs/0/' in s for c in backend.capabilities() for s in c['slots'])
    assert tx.load(slot(0, 14), 'first').error and not driver.calls


def test_a_virtual_chain_carries_its_own_delays_and_leaves_the_analog_strips():
    """Only analog 1-4 have retained Mic strip delay lines (ua_strips_delay); a Virtual chain's
    record and monitor delays are in its own m0, from the same compensation plan."""
    _, driver, backend, tx = virtual_model(compensation=True)
    assert backend.set_compensation('Medium').status is Outcome.CONFIRMED
    strips = driver.strips
    assert tx.load(slot(0, 14), 'first').committed
    assert driver.strips.record == strips.record and len(driver.strips.record) == 4
    assert driver.memory[backend._leases[slot(0, 14)].native.handle][1, 16] == 200
    assert backend.set_compensation('Short').status is Outcome.CONFIRMED
    assert driver.memory[backend._leases[slot(0, 14)].native.handle][1, 16] == 100
    assert driver.strips.record == (100,)*4
