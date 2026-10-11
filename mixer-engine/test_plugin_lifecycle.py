"""Socket-free checks of truthful plugin failure and protocol preservation."""
import copy
import json
from pathlib import Path
import unittest
from helper_tree import HelperTree
from plugin_lifecycle import PluginResult, PluginLifecycle, is_plugin_slot_path
from protocol import Command
from state_tree import StateTree
from ua_mixer_daemon import MixerClient, MixerDaemon
from ubjson_codec import decode_frame

class Writer:
    def __init__(self): self.frames = []
    def get_extra_info(self, name): return ('127.0.0.1', 4720)
    def write(self, data): self.frames.append(data)

class FailureBackend:
    diagnostic_only = True
    def __init__(self): self.calls = []
    def load(self, *args):
        self.calls.append(args)
        return PluginResult('openapollo.test_allocation_failed')

class PluginTests(unittest.TestCase):
    slot = '/devices/0/auxs/0/effects/0'
    def setup_connection(self, binary=False, helper_client=False, backend=None):
        state = StateTree()
        state.load_device_map(Path(__file__).parent / 'device_maps/device_map_apollo_x4.json')
        helper = HelperTree()
        helper.load(Path(__file__).parent / 'device_maps/helper_tree.json')
        writes = []
        state.on_set = lambda *args: writes.append(args)
        daemon = MixerDaemon(state, None, helper_tree=helper, plugin_backend=backend)
        writer = Writer()
        client = MixerClient(None, writer, ubjson=binary)
        client.ubjson = binary
        client.helper = helper_client
        return state, helper, daemon, client, writer, writes
    def decode(self, writer, binary):
        if binary:
            result, end = decode_frame(writer.frames[-1])
            self.assertEqual(end, len(writer.frames[-1]))
            return result
        self.assertTrue(writer.frames[-1].endswith(b'\0'))
        return json.loads(writer.frames[-1][:-1])
    def test_missing_backend_preserves_both_trees_all_transports(self):
        for binary in (False, True):
            for helper_client in (False, True):
                with self.subTest(binary=binary, helper=helper_client):
                    state, helper, daemon, client, writer, writes = self.setup_connection(binary, helper_client)
                    before = copy.deepcopy(state.root), copy.deepcopy(helper.root)
                    path = self.slot + '/EffectName/value'
                    daemon._handle_set(client, Command('set', path, 'UAD 4K Buss Compressor', raw_params={'func_id': '17'}))
                    self.assertEqual(self.decode(writer, binary), {'path': path, 'error': 'openapollo.plugin_backend_unavailable', 'parameters': {'func_id': 17}})
                    self.assertEqual(before, (state.root, helper.root))
                    self.assertEqual(writes, [])
    def test_failure_preserves_prior_slot_and_authorization(self):
        backend = FailureBackend()
        state, helper, daemon, client, writer, writes = self.setup_connection(backend=backend)
        state.get_prop_dict(self.slot+'/EffectName')['value'] = 'prior fixture'
        before = copy.deepcopy(state.root)
        daemon._handle_set(client, Command('set', self.slot+'/EffectName/value', 'new fixture'))
        self.assertEqual(state.root, before)
        self.assertEqual(len(backend.calls), 1)
        self.assertEqual(self.decode(writer, False)['error'], 'openapollo.test_allocation_failed')
        self.assertEqual(writes, [])
    def test_empty_remove_idempotent(self):
        state, helper, daemon, client, writer, writes = self.setup_connection(True, True)
        daemon._handle_set(client, Command('set', self.slot+'/EffectName', '', raw_params={'func_id':'9'}))
        self.assertEqual(self.decode(writer, True)['data'], '')
        self.assertEqual(writes, [])
    def test_unknown_slot_identity_cannot_acknowledge_empty_removal(self):
        for field, invalid in [('EffectName', None), ('EffectName', False),
                               ('EffectInstance', None), ('EffectInstance', False),
                               ('EffectInstance', '0'), ('EffectInstance', -1)]:
            with self.subTest(field=field, invalid=invalid):
                state, helper, daemon, client, writer, writes = self.setup_connection()
                state.get_prop_dict(self.slot+'/'+field)['value'] = invalid
                before = copy.deepcopy(state.root), copy.deepcopy(helper.root)
                daemon._handle_set(client, Command('set', self.slot+'/EffectName/value', '',
                                                   raw_params={'func_id': '29'}))
                reply = self.decode(writer, False)
                self.assertEqual(reply['error'], 'openapollo.plugin_publication_unavailable')
                self.assertEqual(reply['parameters'], {'func_id': 29})
                self.assertNotIn('data', reply)
                self.assertEqual(before, (state.root, helper.root))
                self.assertEqual(writes, [])

    def test_power_parameter_and_derived_fields_intercepted(self):
        for field, value, error in [('Power', True, 'openapollo.plugin_backend_unavailable'), ('parameters/0/NormalizedValue', .5, 'openapollo.plugin_backend_unavailable'), ('parameters/0/StepValue', 'On', 'openapollo.plugin_backend_unavailable'), ('EffectInstance', 123, 'openapollo.plugin_read_only'), ('Latency', 55, 'openapollo.plugin_read_only'), ('parameters/0/StringValue', 'On', 'openapollo.plugin_read_only')]:
            with self.subTest(field=field):
                state, helper, daemon, client, writer, writes = self.setup_connection()
                before = copy.deepcopy(state.root)
                daemon._handle_set(client, Command('set', self.slot+'/'+field+'/value', value))
                self.assertEqual(self.decode(writer, False)['error'], error)
                self.assertEqual(state.root, before)
                self.assertEqual(writes, [])
    def test_input_unison_and_unknown_slot(self):
        state, helper, daemon, client, writer, writes = self.setup_connection()
        for slot in ['/devices/0/inputs/0/effects/0', '/devices/0/inputs/0/preamps/0/effects/0']:
            daemon._handle_set(client, Command('set', slot+'/EffectName/value', 'UAD API Preamp'))
            self.assertEqual(self.decode(writer,False)['error'], 'openapollo.plugin_backend_unavailable')
        daemon._handle_set(client, Command('set','/devices/0/auxs/999/effects/0/EffectName/value','x'))
        self.assertEqual(self.decode(writer,False)['error'],'openapollo.invalid_plugin_slot')
    def test_loaded_remove_and_backend_exception_preserve_state(self):
        class Backend:
            diagnostic_only = True
            def unload(self, *args): raise RuntimeError('test failure')
        state, helper, daemon, client, writer, writes = self.setup_connection(backend=Backend())
        state.get_prop_dict(self.slot+'/EffectName')['value'] = 'fixture loaded name'
        before = copy.deepcopy(state.root)
        with self.assertLogs('plugin_lifecycle', level='ERROR'):
            daemon._handle_set(client, Command('set',self.slot+'/EffectName/value',''))
        self.assertEqual(self.decode(writer,False)['error'],'openapollo.plugin_backend_failed')
        self.assertEqual(state.root,before)
        self.assertEqual(writes,[])
    def test_invalid_values_never_reach_backend(self):
        for field, value in [('EffectName', None), ('Power', 'false')]:
            state, helper, daemon, client, writer, writes = self.setup_connection()
            daemon._handle_set(client, Command('set',self.slot+'/'+field+'/value',value))
            self.assertEqual(self.decode(writer,False)['error'],'openapollo.invalid_plugin_value')
            self.assertEqual(writes,[])
    def test_divergent_helper_prevents_empty_remove_success(self):
        state, helper, daemon, client, writer, writes = self.setup_connection()
        # Use an explicitly readable alternate helper view: captured helper trees
        # may not contain any DSP effect slots in the offline fixture.
        class DivergentHelper:
            def get_value(self, path):
                return 'prior helper plugin' if path.endswith('/EffectName') else 0
        boundary = PluginLifecycle(state, helper_tree=DivergentHelper())
        before = copy.deepcopy(state.root)
        result = boundary.handle(self.slot+'/EffectName/value', '')
        self.assertEqual(result.error,'openapollo.plugin_publication_unavailable')
        self.assertEqual(state.root,before)
        self.assertEqual(writes,[])
    def test_partial_helper_identity_is_not_an_absent_slot(self):
        state, helper, daemon, client, writer, writes = self.setup_connection()
        for name, instance in [(None, 0), ('', None), ('', False), (False, 0)]:
            with self.subTest(name=name, instance=instance):
                class PartialHelper:
                    def get_value(self, path):
                        return name if path.endswith('/EffectName') else instance
                result = PluginLifecycle(state, helper_tree=PartialHelper()).handle(
                    self.slot+'/EffectName', '')
                self.assertEqual(result.error, 'openapollo.plugin_publication_unavailable')
        self.assertEqual(writes, [])

    def test_noncanonical_aliases_cannot_bypass_boundary(self):
        for path in ['/devices//0/auxs/0/effects/0/EffectName/value/', 'devices/0/auxs/0/effects/0/EffectName/value']:
            self.assertTrue(is_plugin_slot_path(path))
            state, helper, daemon, client, writer, writes = self.setup_connection()
            daemon._handle_set(client, Command('set',path,'x'))
            self.assertEqual(self.decode(writer,False)['error'],'openapollo.plugin_backend_unavailable')
            self.assertEqual(state.get_value(self.slot+'/EffectName/value'),'')
            self.assertEqual(writes,[])
    def test_public_slot_matcher(self):
        for path in [self.slot+'/EffectName/value', '/devices/0/inputs/0/preamps/0/effects/0/EffectInstance', '/devices/0/outputs/0/effects/1/parameters/2/NormalizedValue/value']:
            self.assertTrue(is_plugin_slot_path(path))
        for path in ['/plugins/0/Authorized/value', '/devices/0/inputs/0/preamps/0/Gain/value', '/devices/0/inputs/0/effects', None]:
            self.assertFalse(is_plugin_slot_path(path))
    def test_unregistered_backend_never_called(self):
        class Backend:
            def load(self, *args): raise AssertionError('must not allocate')
        state, helper, daemon, client, writer, writes = self.setup_connection(backend=Backend())
        daemon._handle_set(client, Command('set',self.slot+'/EffectName/value','x'))
        self.assertEqual(self.decode(writer,False)['error'],'openapollo.plugin_publication_unavailable')
    def test_invalid_parameter_values(self):
        for field, values in [('NormalizedValue', [float('nan'), float('inf'), -0.1, 1.1, '0.5', True, None]), ('StepValue', [None, [], {}, .5])]:
            for value in values:
                state, helper, daemon, client, writer, writes = self.setup_connection()
                daemon._handle_set(client, Command('set',self.slot+'/parameters/0/'+field+'/value',value))
                self.assertEqual(self.decode(writer,False)['error'],'openapollo.invalid_plugin_value')
                self.assertEqual(writes,[])
    def test_claimed_backend_success_fails_closed(self):
        class Backend:
            diagnostic_only = True
            def load(self,*args): return PluginResult()
        state, helper, daemon, client, writer, writes = self.setup_connection(backend=Backend())
        daemon._handle_set(client, Command('set',self.slot+'/EffectName/value','x'))
        self.assertEqual(self.decode(writer,False)['error'],'openapollo.plugin_publication_unavailable')
        self.assertEqual(state.get_value(self.slot+'/EffectInstance/value'),0)

    def test_remote_get_never_returns_captured_placeholders(self):
        for binary in (False, True):
            for helper_client in (False, True):
                state, helper, daemon, client, writer, writes = self.setup_connection(binary, helper_client)
                before = copy.deepcopy(state.root), copy.deepcopy(helper.root)
                path = self.slot + '/VendorSpecific/value'
                params = {'opcode': '519', 'p0': str(0x05000004), 'p1': '11', 'p2': '1', 'func_id': '42'}
                daemon._handle_get(client, Command('get', path, raw_params=params))
                response = self.decode(writer, binary)
                self.assertEqual(response['error'], 'openapollo.plugin_remote_backend_unavailable')
                self.assertNotIn('data', response)
                self.assertEqual(response['parameters']['func_id'], 42)
                self.assertEqual(response['parameters']['opcode'], '519')
                self.assertEqual(response['parameters']['p0'], str(0x05000004))
                self.assertEqual(response['parameters']['p1'], '11')
                self.assertEqual(before, (state.root, helper.root))
                self.assertEqual(writes, [])

    def test_queryless_vendor_property_remains_readable(self):
        state, helper, daemon, client, writer, writes = self.setup_connection()
        path = self.slot + '/VendorSpecific/value'
        state.add_runtime_property(self.slot + '/VendorSpecific',
                                   {'type': 'string', 'value': 'fixture property'})
        expected = state.get_value(path)
        daemon._handle_get(client, Command('get', path))
        self.assertEqual(self.decode(writer, False)['data'], expected)
        self.assertEqual(writes, [])

if __name__ == '__main__': unittest.main()
