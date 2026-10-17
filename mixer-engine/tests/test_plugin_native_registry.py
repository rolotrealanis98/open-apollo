"""Registry selection and exact tagged-meter routing over synthetic I/O."""
import json
from unittest.mock import Mock

import pytest

from plugin_native import NativePluginBackend, load_provider, register_native
from plugin_transactions import PluginTransactions
from plugin_remote_codec import decode_array_response
from protocol import Command
from state_tree import StateTree
from helper_tree import HelperTree
from plugin_test_support import MemoryWriter
from ua_mixer_daemon import MixerClient, MixerDaemon
from test_plugin_native import model, Provider, SLOT, FIELD


def registry():
    state, helper, driver, first, backend, _ = model()
    second = Provider()
    second.name = 'other synthetic'
    backend = NativePluginBackend(backend.publisher, driver, (first, second))
    return state, helper, driver, first, second, backend, PluginTransactions(backend.publisher, backend)


def test_registry_selects_encoder_and_changes_type_only_after_retirement():
    state, helper, driver, first, second, backend, tx = registry()
    first.plan_control = Mock(wraps=first.plan_control)
    second.plan_control = Mock(wraps=second.plan_control)
    assert [c['name'] for c in backend.capabilities()] == [first.name, second.name]
    assert tx.load(SLOT, first.name).committed
    before = list(driver.calls)
    assert tx.load(SLOT, second.name).error
    assert driver.calls == before and backend._leases[SLOT].provider is first
    assert tx.unload(SLOT).committed
    assert tx.load(SLOT, second.name).committed
    assert backend._leases[SLOT].provider is second
    assert tx.control(SLOT, FIELD, .75).committed
    second.plan_control.assert_called_once()
    first.plan_control.assert_not_called()
    assert state.get_value(SLOT + '/EffectName') == second.name
    assert helper.get_value(SLOT + '/EffectName') == second.name
    assert tx.unload(SLOT).committed


def test_registry_unknown_name_slot_and_duplicates_never_create():
    state, helper, driver, first, second, backend, tx = registry()
    assert tx.load(SLOT, 'unknown').error
    assert tx.load('/devices/0/inputs/0/effects/0', second.name).error
    assert driver.calls == []
    for providers in ([], [first, first]):
        with pytest.raises(ValueError):
            NativePluginBackend(backend.publisher, driver, providers)


def test_registry_registration_checks_every_rate_without_device_open():
    first, second = Provider(), Provider()
    second.name, second.sample_rate = 'other synthetic', 44100
    hardware = Mock(connected=True, fd=19)
    hardware.get_driver_param.return_value = 48000
    with pytest.raises(ValueError, match='sample rate'):
        register_native(StateTree(), HelperTree(), hardware, (first, second))
    hardware.open.assert_not_called()


def test_explicit_module_supports_plural_factory_and_legacy_factory(tmp_path):
    module = tmp_path / 'provider.py'
    module.write_text('def create_providers():\n    return ("first", "second")\n')
    assert load_provider(module) == ('first', 'second')
    module.write_text('def create_provider():\n    return "legacy"\n')
    assert load_provider(module) == 'legacy'


def test_tagged_meter_requires_exact_readonly_query_and_reads_fresh():
    state, helper, driver, first, second, backend, tx = registry()
    tagged = 0x0400001A
    second.remote_reads = {('opcode', 519, tagged, 1, 1): (14, 26)}
    assert tx.load(SLOT, second.name).committed
    daemon = MixerDaemon(state, None, helper_tree=helper, plugin_transactions=tx,
                         plugin_remote_backend=backend, plugin_hosting=True)
    writer = MemoryWriter()
    client = MixerClient(None, writer)
    path = SLOT + '/VendorSpecific/value'
    params = {'opcode': '519', 'p0': str(tagged), 'p1': '1', 'p2': '1'}
    driver.service = Mock(wraps=driver.service)
    for word in (0x3F000000, 0x3F800000):
        driver.word = word
        daemon._handle_get(client, Command('get', path, raw_params=params))
        response = json.loads(writer.frames[-1].rstrip(b'\0'))
        assert 'error' not in response
        decoded = decode_array_response(path, response['data'], response['parameters'], expected_path=path)
        assert decoded.words == (word,)
        assert driver.service.call_args.kwargs == {'read': (14, 26, 1)}
    reads = driver.service.call_count
    for changed in ({'p0': str(tagged + 1)}, {'p1': '2'}, {'p2': '0'}, {'opcode': '518'}):
        daemon._handle_get(client, Command('get', path, raw_params=params | changed))
        assert json.loads(writer.frames[-1].rstrip(b'\0'))['error']
    daemon._handle_get(client, Command('get', path, raw_params={'floats': str(tagged)}))
    assert json.loads(writer.frames[-1].rstrip(b'\0'))['error']
    assert driver.service.call_count == reads


def test_registry_moves_between_declared_inputs_after_unload():
    state, helper, driver, first, second, old, _ = registry()
    second.slot = '/devices/0/inputs/0/effects/0'
    original_arguments = second.create_arguments
    second.create_arguments = lambda: original_arguments() | {'input_pair': (0, 32)}
    for name, kind, value in [('EffectName', 'string', ''), ('EffectInstance', 'pointer', 0),
                              ('Power', 'bool', False), ('Latency', 'int', 0), ('Status', 'string', '')]:
        state.add_runtime_property(second.slot + '/' + name, {'type': kind, 'value': value})
    backend = NativePluginBackend(old.publisher, driver, (first, second))
    tx = PluginTransactions(backend.publisher, backend)
    assert tx.load(SLOT, first.name).committed
    before = list(driver.calls)
    assert tx.load(second.slot, second.name).error
    assert driver.calls == before
    assert tx.unload(SLOT).committed
    assert tx.load(second.slot, second.name).committed
    assert tx.control(SLOT, FIELD, .75).error
    assert tx.control(second.slot, FIELD, .75).committed
    assert tx.unload(second.slot).committed
