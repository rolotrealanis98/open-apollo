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
    ('/SampleRate/value', 96000),
    ('/ClockSource/value', 'S/PDIF'),
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
    assert 'from Analog 1 and Analog 3' in native_refusal_detail('/devices/0/inputs/1/Stereo')
    assert native_refusal_detail('/SampleRate') is None


def test_a_refused_link_carries_its_reason(lab):
    lab.daemon._handle_set(lab.client, Command('set', '/devices/0/inputs/3/Stereo/value', True,
                                               raw_params={'func_id': 1}))
    assert refused(lab)
    assert 'Analog 3' in lab.daemon._send_local_plugin_error.call_args.kwargs['detail']


@pytest.mark.parametrize('path', [
    '/devices/0/inputs/14/Stereo',      # Virtual 1-2 (x4)
    '/devices/0/inputs/15/Stereo',      # its right leg: forwarded to the leader
    '/devices/0/inputs/20/Stereo',      # Virtual 7-8
    '/devices/0/inputs/12/Stereo',      # S/PDIF
    '/devices/0/inputs/4/Stereo',       # ADAT 1-2
    '/devices/1/inputs/0/Stereo',       # a second unit: native hosting is unit 0's
])
def test_links_without_a_native_chain_pass(lab, path):
    """Inputs native hosting never puts a chain on link like any mixer control."""
    assert not native_refuses_set(path)
    lab.daemon._handle_set(lab.client, Command('set', path + '/value', True,
                                               raw_params={'func_id': 1}))
    assert not refused(lab)


def test_native_chains_stay_on_inputs_0_to_3():
    """The link exemption above relies on prepare taking inputs 0-3 only."""
    _, _, _, _, backend, _ = model()
    for slot in ('/devices/0/inputs/4/effects/0', '/devices/0/inputs/14/effects/0'):
        outcome = backend.prepare(slot, 'anything')
        assert outcome.status.name == 'FAILED'
        assert 'no native recipe' in outcome.detail
