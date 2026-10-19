"""Native hosting refuses only what it has no adapter for; mixer controls work."""
from types import SimpleNamespace
from unittest.mock import Mock

import pytest

from protocol import Command
from test_plugin_native import model
from ua_mixer_daemon import MixerDaemon, native_allows_post, native_refusal_detail, native_refuses_set

UNSUPPORTED = 'openapollo.native_operation_unsupported'


@pytest.fixture
def lab():
    state, helper, driver, provider, backend, transactions = model()
    daemon = MixerDaemon(state, None, helper_tree=helper,
                         plugin_transactions=transactions, plugin_hosting=True)
    daemon._send_local_plugin_error = Mock()
    daemon._send_error = Mock()
    daemon._send_response = Mock()
    daemon._send_post_reply = Mock()
    # model() has no device map: an accepted set then ends as "unresolved path"
    daemon._send_unresolved_set = Mock()
    daemon._send_engine_error = Mock()
    client = SimpleNamespace(id='console', helper=False, ubjson=False, sleep=False, send=Mock())
    return SimpleNamespace(daemon=daemon, client=client)


def refused(lab):
    call = lab.daemon._send_local_plugin_error.call_args
    return call is not None and call.args[2] == UNSUPPORTED


@pytest.mark.parametrize('path,value', [
    ('/devices/0/inputs/0/FaderLevelTapered/value', 0.5),
    ('/devices/0/inputs/0/Mute/value', True),
    ('/devices/0/inputs/0/Pan/value', 0.2),
    ('/devices/0/inputs/0/Solo/value', True),
    ('/devices/0/inputs/0/preamps/0/48V/value', True),
    ('/devices/0/inputs/0/sends/0/GainTapered/value', 0.4),
    ('/devices/0/outputs/18/CRMonitorLevelTapered/value', 0.4),
    ('/devices/0/outputs/18/DimOn/value', True),
    ('/TalkbackOn/value', True),
    ('/UndoRecording/value', True),
])
def test_mixer_controls_pass_the_native_gate(lab, path, value):
    lab.daemon._handle_set(lab.client, Command('set', path, value, raw_params={'func_id': 1}))
    assert not refused(lab)


@pytest.mark.parametrize('path,value', [
    ('/devices/0/inputs/0/Preset/value', 'Vocal'),
    ('/UndoRedo/value', 3),
    ('/Session/value', 'x.uadmix'),
])
def test_insert_owning_state_is_refused(lab, path, value):
    lab.daemon._handle_set(lab.client, Command('set', path, value, raw_params={'func_id': 1}))
    assert refused(lab)


def test_predicates():
    assert native_refuses_set('/devices/0/inputs/1/Stereo')
    assert not native_refuses_set('/devices/0/inputs/1/StereoName')
    assert not native_refuses_set('/devices/0/inputs/1/Mute')
    assert native_allows_post('/ClearSolos') and native_allows_post('/devices/0/Identify')
    assert not native_allows_post('/MoveInsert') and not native_allows_post('/SceneOperation')


@pytest.mark.parametrize('path,allowed', [
    ('/ClearSolos', True), ('/CopyToSends', True),
    ('/MoveInsert', False), ('/SaveSessionAs', False)])
def test_posts(lab, path, allowed):
    lab.daemon._handle_post(lab.client, Command('post', path, {}, raw_params=None))
    errors = [c.args[3] for c in lab.daemon._send_post_reply.call_args_list]
    assert (UNSUPPORTED not in errors) == allowed


def test_link_refusals_say_why():
    assert 'Remove the plug-ins first' in native_refusal_detail('/devices/0/inputs/0/Stereo')
    assert 'Analog 1-2 and Analog 3-4' in native_refusal_detail('/devices/0/inputs/4/Stereo')
    assert 'Remove the plug-ins first' in native_refusal_detail('/SampleRate')
    assert native_refusal_detail('/BufferSize') is None


def test_a_refused_link_carries_its_reason(lab):
    lab.daemon._handle_set(lab.client, Command('set', '/devices/0/inputs/4/Stereo/value', True,
                                               raw_params={'func_id': 1}))
    assert refused(lab)
    assert 'Analog 1-2' in lab.daemon._send_local_plugin_error.call_args.kwargs['detail']


@pytest.mark.parametrize('path,value', [
    ('/SampleRate/value', 96000),
    ('/ClockSource/value', 'S/PDIF'),
])
def test_rate_and_clock_pause_idle_hosting(lab, path, value):
    """Nothing loaded: the rate changes and hosting pauses, to come back by itself."""
    native = lab.daemon._native_plugin_backend
    attach = Mock(side_effect=ValueError('native provider sample rate does not match hardware'))
    lab.daemon._native_attach_fn = attach
    lab.daemon.hw_router = SimpleNamespace(backend=SimpleNamespace(connected=True))
    lab.daemon._publish_license = Mock()
    lab.daemon._handle_clock_set = Mock(return_value=True)
    lab.daemon._handle_set(lab.client, Command('set', path, value, raw_params={'func_id': 1}))
    assert not refused(lab)
    lab.daemon._handle_clock_set.assert_called_once()
    assert lab.daemon._native_plugin_backend is None
    assert lab.daemon._native_attach is attach
    assert not native.publisher.connected
    # At another rate the retry says what to do; it never raises
    assert lab.daemon._native_hosting_try() is False
    assert '48 kHz' in lab.daemon._native_unavailable_reason


@pytest.mark.parametrize('path,value', [
    ('/SampleRate/value', 96000),
    ('/ClockSource/value', 'S/PDIF'),
])
def test_rate_and_clock_are_refused_with_plugins_loaded(lab, path, value):
    native = lab.daemon._native_plugin_backend
    native._leases['/devices/0/inputs/0/effects/0'] = object()
    lab.daemon._native_attach_fn = Mock()
    lab.daemon._handle_set(lab.client, Command('set', path, value, raw_params={'func_id': 1}))
    assert refused(lab)
    assert 'Remove the plug-ins first' in lab.daemon._send_local_plugin_error.call_args.kwargs['detail']
    assert lab.daemon._native_plugin_backend is native
