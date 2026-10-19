"""Native adapter and real daemon dispatch over synthetic, socket-free I/O."""
import copy
import errno
import json
from unittest.mock import Mock

import pytest

from plugin_driver import NativeInstance
from plugin_native import ControlPlan, NativePluginBackend, register_native
from plugin_publication import LoadedPlugin, Publisher
from plugin_transactions import PluginTransactions
from protocol import Command
from state_tree import StateTree
from helper_tree import HelperTree
from plugin_test_support import MemoryWriter
from ua_mixer_daemon import MixerClient, MixerDaemon

SLOT = '/devices/0/inputs/2/effects/0'
FIELD = 'parameters/0/NormalizedValue'


class Driver:
    def __init__(self):
        self.calls = []
        self.word = 50
        self.valid = True
        self.fail = None
        self.mismatch = False

    def invalidate(self):
        self.valid = False

    def create(self, **arguments):
        self.calls.append('create')
        if self.fail == 'create':
            self.invalidate()
            raise OSError(errno.EFAULT, 'synthetic copy failure')
        return NativeInstance(7, 1, 23)

    def service(self, native, *, writes=(), read=None):
        if not self.valid:
            raise OSError(errno.ESTALE, 'invalidated')
        self.calls.append('write' if writes else 'read')
        if self.fail == 'service':
            self.invalidate()
            raise OSError(errno.ETIMEDOUT, 'synthetic timeout')
        if writes:
            self.word = writes[-1]
            return ()
        return (self.word + int(self.mismatch),)

    def destroy(self, native):
        self.calls.append('destroy')
        if not self.valid or self.fail == 'destroy':
            self.invalidate()
            raise OSError(errno.ETIMEDOUT, 'synthetic retirement failure')


class Provider:
    slot, name, sample_rate = SLOT, 'synthetic', 48000

    def reset(self):
        self.word = 50

    def observations(self):
        return ((0, 0, (self.word,)),)

    def create_arguments(self):
        return dict(render_id=0x806, resources=(bytes(32),),
                    private_rows=((1, 8, 0xFFFFFFFF, 0),), dsp=1,
                    input_pair=(2, 32), commands=(0x00140004, 0, 0, 50))

    def initial(self, lease):
        return LoadedPlugin(self.name, lease, True, 0, ({'id': 0, 'properties': {
            'type': {'type': 'string', 'value': 'parameter'},
            'Name': {'type': 'string', 'value': 'Synthetic'},
            'NormalizedValue': {'type': 'float', 'value': 0.5},
            'StringValue': {'type': 'string', 'value': '50'},
        }},))

    def plan(self, field, word):
        if field != FIELD:
            raise ValueError('unsupported synthetic control')
        return ControlPlan((0x00140004, 0, 0, word), ((0, 0, (word,)),),
                           {'NormalizedValue': word / 100, 'StringValue': str(word)}, word)

    def snapshot(self, field):
        return self.plan(field, self.word)

    def plan_control(self, field, value):
        return self.plan(field, round(value * 100))

    def plan_restore(self, prior, proposal):
        return copy.deepcopy(prior)

    def accept(self, plan):
        self.word = plan.state


def model(fault=None, provider_class=Provider):
    state, helper = StateTree(), HelperTree()
    for name, kind, value in [('EffectName', 'string', ''),
                              ('EffectInstance', 'pointer', 0),
                              ('Power', 'bool', False), ('Latency', 'int', 0),
                              ('Status', 'string', '')]:
        state.add_runtime_property(SLOT + '/' + name, {'type': kind, 'value': value})
    publisher = Publisher(state, helper, owner='synthetic')
    driver, provider = Driver(), provider_class()
    backend = NativePluginBackend(publisher, driver, provider)
    transactions = PluginTransactions(publisher, backend, publication_fault=fault)
    return state, helper, driver, provider, backend, transactions


def test_real_coordinator_load_control_unload_and_replacement_refusal():
    state, helper, driver, provider, backend, tx = model()
    assert tx.load(SLOT, provider.name).committed
    assert state.get_value(SLOT + '/EffectInstance') == 7
    before = list(driver.calls)
    assert tx.load(SLOT, provider.name).error
    assert driver.calls == before
    result = tx.control(SLOT, FIELD, 0.751)
    assert result.committed and driver.word == 75
    assert state.get_value(SLOT + '/' + FIELD) == 0.75
    assert helper.get_value(SLOT + '/parameters/0/StringValue') == '75'
    before = list(driver.calls)
    assert tx.control(SLOT, 'Power', False).error
    assert driver.calls == before
    assert tx.unload(SLOT).committed
    assert state.get_value(SLOT + '/EffectInstance') == 0
    assert driver.calls[-1] == 'destroy'


def test_initial_readback_failure_destroys_before_publishing():
    state, helper, driver, provider, backend, tx = model()
    driver.mismatch = True
    result = tx.load(SLOT, provider.name)
    assert result.error and not result.committed and not result.quarantined
    assert driver.calls == ['create', 'read', 'read', 'destroy']  # a mismatch is read twice
    assert state.get_value(SLOT + '/EffectInstance') == 0


def test_provider_metadata_rejected_before_create():
    state, helper, driver, provider, backend, tx = model()
    provider.initial = lambda lease: LoadedPlugin('synthetic', lease, True, -1, ())
    assert tx.load(SLOT, provider.name).error
    assert driver.calls == []


def test_unknown_create_or_destroy_outcome_quarantines():
    for operation in ('create', 'destroy'):
        state, helper, driver, provider, backend, tx = model()
        if operation == 'create':
            driver.fail = operation
            result = tx.load(SLOT, provider.name)
        else:
            assert tx.load(SLOT, provider.name).committed
            driver.fail = operation
            result = tx.unload(SLOT)
        assert result.quarantined and not backend.publisher.connected
        before = list(driver.calls)
        assert tx.load(SLOT, provider.name).quarantined
        assert driver.calls == before


def test_control_publication_failure_restores_prior_coefficients():
    state, helper, driver, provider, backend, tx = model()
    assert tx.load(SLOT, provider.name).committed
    before = copy.deepcopy((state.root, helper.root))
    tx.publication_fault = lambda stage: (_ for _ in ()).throw(ValueError('fault'))
    result = tx.control(SLOT, FIELD, 0.75)
    assert result.error and not result.quarantined
    assert driver.word == provider.word == 50
    assert (state.root, helper.root) == before


def test_service_failure_never_fabricates_restore():
    state, helper, driver, provider, backend, tx = model()
    assert tx.load(SLOT, provider.name).committed
    original = driver.service
    def service(native, *, writes=(), read=None):
        if writes:
            driver.fail = 'service'
        return original(native, writes=writes, read=read)
    driver.service = service
    result = tx.control(SLOT, FIELD, 0.75)
    assert result.quarantined and state.get_value(SLOT + '/' + FIELD) == 0.5


def test_real_daemon_dispatch_and_bulk_paths_are_blocked():
    state, helper, driver, provider, backend, tx = model()
    daemon = MixerDaemon(state, None, helper_tree=helper,
                         plugin_transactions=tx, plugin_hosting=True)
    writer = MemoryWriter()
    client = MixerClient(None, writer)
    daemon._handle_set(client, Command('set', SLOT + '/EffectName/value',
                                      provider.name, raw_params={'func_id': '1'}))
    assert state.get_value(SLOT + '/EffectInstance') == 7
    daemon._handle_set(client, Command('set', SLOT + '/' + FIELD, 0.75,
                                      raw_params={'func_id': '2'}))
    assert driver.word == 75
    daemon.console.set = Mock(side_effect=AssertionError('bypassed lifecycle'))
    daemon.console.post = Mock(side_effect=AssertionError('bypassed lifecycle'))
    daemon.commands.execute = Mock(side_effect=AssertionError('bypassed lifecycle'))
    for path in ('/UndoRedo', '/SampleRate', '/MaxDelayComp',
                 '/MaxDelayComp/value', '/devices/0/inputs/2/Preset'):
        daemon._handle_set(client, Command('set', path, 'unsafe'))
        assert json.loads(writer.frames[-1].rstrip(b'\0'))['error']
    for path in ('/MoveInsert', '/PasteInsert', '/SceneOperation', '/OpenSession', '/Undo'):
        daemon._handle_post(client, Command('post', path, {}))
        assert json.loads(writer.frames[-1].rstrip(b'\0'))['error']
    daemon.console.set.assert_not_called()
    daemon.console.post.assert_not_called()
    daemon.commands.execute.assert_not_called()


def test_registration_rejects_wrong_rate_without_opening_device():
    hardware = Mock(connected=True, fd=19)
    hardware.get_driver_param.return_value = 44100
    with pytest.raises(ValueError, match='sample rate'):
        register_native(StateTree(), HelperTree(), hardware, Provider())
    hardware.open.assert_not_called()


def test_session_change_revokes_publication_before_another_write():
    state, helper, driver, provider, backend, tx = model()
    assert tx.load(SLOT, provider.name).committed
    backend.session_valid = lambda: False
    before = list(driver.calls)
    assert tx.control(SLOT, FIELD, 0.75).error
    assert not backend.publisher.connected and not driver.valid
    assert driver.calls == before
    assert tx.current_remote_handle(SLOT) is None


def test_native_cli_requires_explicit_preserve_before_any_hardware(monkeypatch):
    import ua_mixer_daemon
    monkeypatch.setattr('sys.argv', ['daemon', '--native-plugin-provider', 'missing.py'])
    monkeypatch.setattr(ua_mixer_daemon.HardwareBackend, 'open',
                        Mock(side_effect=AssertionError('hardware opened')))
    with pytest.raises(SystemExit) as caught:
        ua_mixer_daemon.main()
    assert caught.value.code == 2
    ua_mixer_daemon.HardwareBackend.open.assert_not_called()


def test_noop_control_still_reads_and_publishes_without_empty_service():
    from dataclasses import replace
    state, helper, driver, provider, backend, tx = model()
    assert tx.load(SLOT, provider.name).committed
    provider.plan_control = lambda field, value: replace(
        provider.plan(field, provider.word), writes=())
    driver.calls.clear()
    assert tx.control(SLOT, FIELD, 0.5).committed
    assert driver.calls == ['read', 'read']
    assert state.get_value(SLOT + '/' + FIELD) == 0.5


def test_real_remote_protocol_fresh_allowlisted_scalar_only():
    from plugin_remote_codec import decode_scalar_response
    state, helper, driver, provider, backend, tx = model()
    provider.remote_reads = {('floats', 9): (0, 3)}
    assert tx.load(SLOT, provider.name).committed
    daemon = MixerDaemon(state, None, helper_tree=helper,
        plugin_transactions=tx, plugin_remote_backend=backend, plugin_hosting=True)
    writer = MemoryWriter()
    client = MixerClient(None, writer)
    path = SLOT + '/VendorSpecific/value'
    driver.calls.clear()
    before = copy.deepcopy((state.root, helper.root))
    for word in (0x3F800000, 0x3F000000):
        driver.word = word
        daemon._handle_get(client, Command('get', path, raw_params={'floats': '9'}))
        message = json.loads(writer.frames[-1].rstrip(b'\0'))
        assert 'error' not in message
        reply = decode_scalar_response(path, message['data'], message['parameters'],
                                       expected_path=path)
        assert reply.words == (word,)
    assert driver.calls == ['read', 'read']
    assert (state.root, helper.root) == before
    for params in ({'ints': '9'}, {'floats': '8'}, {'floats': '9,8'},
                   {'opcode': '519', 'p0': '9', 'p1': '1', 'p2': '0'}):
        daemon._handle_get(client, Command('get', path, raw_params=params))
        assert json.loads(writer.frames[-1].rstrip(b'\0'))['error']
    assert driver.calls == ['read', 'read']
    driver.fail = 'service'
    daemon._handle_get(client, Command('get', path, raw_params={'floats': '9'}))
    assert json.loads(writer.frames[-1].rstrip(b'\0'))['error']
    assert not backend.publisher.connected


class Coupled(Provider):
    """Two parameters; writing parameter 0 also moves parameter 1 (a Sync-style switch)."""

    def __init__(self):
        self.words = [50, 10]

    def initial(self, lease):
        loaded = super().initial(lease)
        second = copy.deepcopy(loaded.parameters[0])
        second['id'] = 1
        second['properties']['NormalizedValue']['value'] = 0.1
        second['properties']['StringValue']['value'] = '10'
        return LoadedPlugin(loaded.name, lease, True, 0, loaded.parameters + (second,))

    def _values(self, word):
        return {'NormalizedValue': word / 100, 'StringValue': str(word)}

    def plan(self, field, word, seen=None):
        # the fake driver has one shared word; `seen` is what it reads back for this plan
        return ControlPlan((0x00140004, 0, 0, word), ((0, 0, (word if seen is None else seen,)),),
                           self._values(word), (int(field.split('/')[1]), word))

    def snapshot(self, field):
        index = int(field.split('/')[1])
        return self.plan(field, self.words[index], self.words[0])

    def accept(self, plan):
        index, word = plan.state
        self.words[index] = word
        if index == 0:
            self.words[1] = word // 5

    def current_values(self, field):
        return self._values(self.words[int(field.split('/')[1])])


class Uncoupled(Coupled):
    current_values = None  # provider without the optional hook


def test_coupled_parameter_is_published_and_next_control_succeeds():
    state, helper, driver, provider, backend, tx = model(provider_class=Coupled)
    assert tx.load(SLOT, provider.name).committed
    second = SLOT + '/parameters/1/NormalizedValue'
    assert tx.control(SLOT, FIELD, 0.5 + 0.2).committed
    assert state.get_value(second) == 0.14
    assert helper.get_value(SLOT + '/parameters/1/StringValue') == '14'
    result = tx.control(SLOT, 'parameters/1/NormalizedValue', 0.3)
    assert result.committed and not result.error
    assert state.get_value(second) == 0.3


def test_provider_without_current_values_publishes_no_coupled_change():
    state, helper, driver, provider, backend, tx = model(provider_class=Uncoupled)
    assert tx.load(SLOT, provider.name).committed
    assert tx.control(SLOT, FIELD, 0.7).committed
    assert state.get_value(SLOT + '/parameters/1/NormalizedValue') == 0.1
