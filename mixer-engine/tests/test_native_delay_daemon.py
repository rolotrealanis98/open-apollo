"""Native delay-setting dispatch/publication over socket-free synthetic I/O."""
from types import SimpleNamespace
from unittest.mock import Mock

import pytest

from model_features import MAX_DELAY_COMP_SAMPLES
from plugin_delay import InputLatency, plan_compensation
from plugin_native_chain import CompensationOutcome
from plugin_transactions import Outcome
from protocol import Command
from test_plugin_native import model
from ua_mixer_daemon import MixerDaemon


@pytest.fixture
def delay_daemon():
    state, helper, driver, provider, backend, transactions = model()
    state.add_runtime_property('/MaxDelayComp', {
        'type': 'string', 'value': 'Medium', 'values': list(MAX_DELAY_COMP_SAMPLES)})
    helper.root.setdefault('properties', {})['MaxDelayComp'] = {
        'type': 'string', 'value': 'Medium'}
    backend.compensation_supported = True
    backend.compensation_inputs = plan_compensation(
        'Medium', (InputLatency((2, 3), 101, 101),))

    def apply(setting):
        backend.compensation_inputs = plan_compensation(
            setting, (InputLatency((2, 3), 101, 101),))
        return CompensationOutcome(Outcome.CONFIRMED, setting,
                                   backend.compensation_inputs)

    backend.set_compensation = Mock(side_effect=apply)
    backend.invalidate = Mock(wraps=backend.invalidate)
    daemon = MixerDaemon(state, None, helper_tree=helper,
                         plugin_transactions=transactions, plugin_hosting=True)
    daemon._refresh_latency = Mock()
    daemon._send_local_plugin_error = Mock()
    daemon._send_response = Mock()
    state.on_set = Mock()
    client = SimpleNamespace(id='delay-test')

    def send(value, path='/MaxDelayComp', acknowledge=True):
        params = {'func_id': 'delay-1'} if acknowledge else {}
        daemon._handle_set(client, Command('set', path, value, raw_params=params))

    return SimpleNamespace(daemon=daemon, state=state, helper=helper,
                           driver=driver, backend=backend, tx=transactions,
                           client=client, send=send)


@pytest.mark.parametrize('setting', MAX_DELAY_COMP_SAMPLES)
@pytest.mark.parametrize('path', ['/MaxDelayComp', '/MaxDelayComp/value'])
def test_confirmed_settings_publish_after_backend_acceptance(delay_daemon, setting, path):
    lab = delay_daemon
    lab.send(setting, path)
    lab.backend.set_compensation.assert_called_once_with(setting)
    assert lab.state.get_value('/MaxDelayComp') == setting
    assert lab.helper.get_value('/MaxDelayComp') == setting
    assert not lab.tx.quarantined
    lab.state.on_set.assert_not_called()
    lab.daemon._refresh_latency.assert_called_once_with(force=True)
    lab.daemon._send_response.assert_called_once_with(
        lab.client, path, setting, {'func_id': 'delay-1'})
    assert lab.driver.calls == []


def test_set_without_func_id_does_not_send_success_response(delay_daemon):
    lab = delay_daemon
    lab.send('Long', acknowledge=False)
    assert lab.state.get_value('/MaxDelayComp') == 'Long'
    lab.daemon._send_response.assert_not_called()


@pytest.mark.parametrize('value', ['short', 'MediumLong', '', 'default', 100, True, None, {}, []])
def test_invalid_setting_never_reaches_backend(delay_daemon, value):
    lab = delay_daemon
    lab.send(value)
    lab.backend.set_compensation.assert_not_called()
    assert lab.state.get_value('/MaxDelayComp') == 'Medium'
    assert lab.helper.get_value('/MaxDelayComp') == 'Medium'
    assert lab.daemon._send_local_plugin_error.call_args.args[2] == 'openapollo.invalid_plugin_value'
    lab.daemon._refresh_latency.assert_not_called()


def test_missing_capability_blocks_even_valid_setting(delay_daemon):
    lab = delay_daemon
    lab.backend.compensation_supported = False
    lab.send('Long')
    lab.backend.set_compensation.assert_not_called()
    assert lab.daemon._send_local_plugin_error.call_args.args[2] == 'openapollo.native_operation_unsupported'
    assert lab.state.get_value('/MaxDelayComp') == 'Medium'


@pytest.mark.parametrize('gate', ['quarantined', 'publication'])
def test_session_gate_prevents_device_work(delay_daemon, gate):
    lab = delay_daemon
    if gate == 'quarantined':
        lab.tx.quarantined = True
    else:
        lab.daemon._plugin_publication_active = True
    lab.send('Long')
    lab.backend.set_compensation.assert_not_called()
    assert lab.state.get_value('/MaxDelayComp') == 'Medium'
    lab.daemon._send_response.assert_not_called()
    lab.daemon._send_local_plugin_error.assert_called_once()


@pytest.mark.parametrize('status', [Outcome.FAILED, Outcome.INDETERMINATE])
def test_unconfirmed_result_preserves_public_setting(delay_daemon, status):
    lab = delay_daemon
    lab.backend.set_compensation.side_effect = None
    lab.backend.set_compensation.return_value = CompensationOutcome(
        status, 'Long', detail='synthetic unknown device result')
    lab.send('Long')
    assert lab.state.get_value('/MaxDelayComp') == 'Medium'
    assert lab.helper.get_value('/MaxDelayComp') == 'Medium'
    assert lab.tx.quarantined is (status is Outcome.INDETERMINATE)
    if status is Outcome.INDETERMINATE:
        assert lab.tx.quarantine_reason == 'synthetic unknown device result'
        lab.send('Short')
        lab.backend.set_compensation.assert_called_once_with('Long')
    lab.daemon._send_response.assert_not_called()
    lab.daemon._refresh_latency.assert_not_called()


def test_state_publication_failure_invalidates_and_quarantines(delay_daemon, monkeypatch):
    lab = delay_daemon
    monkeypatch.setattr(lab.state, 'set', Mock(return_value=False))
    lab.send('Long')
    lab.backend.invalidate.assert_called_once()
    assert lab.tx.quarantined
    assert lab.state.get_value('/MaxDelayComp') == 'Medium'
    assert lab.helper.get_value('/MaxDelayComp') == 'Medium'
    lab.daemon._send_response.assert_not_called()


def test_warning_uses_channel_names_suggestion_and_clears_when_disabled(delay_daemon):
    lab = delay_daemon
    lab.send('Short')
    assert lab.state.get_value('/InputDelayCompOverloaded') is True
    text = lab.state.get_value('/InputDelayCompWarning')
    assert 'Input 3/4 needs 101 samples.' in text
    assert 'increase input delay compensation to medium' in text.lower()
    assert lab.state.get_value('/MaxDelayComp') == 'Short'
    lab.send('Off')
    assert lab.state.get_value('/InputDelayCompOverloaded') is False
    assert lab.state.get_value('/InputDelayCompWarning') == ''


def test_warning_above_largest_setting_suggests_removal(delay_daemon):
    lab = delay_daemon
    lab.backend.compensation_inputs = plan_compensation(
        'Long', (InputLatency((0,), 1001, 1001),))
    lab.daemon._publish_native_delay_status()
    text = lab.state.get_value('/InputDelayCompWarning')
    assert 'Input 1 needs 1001 samples.' in text
    assert 'Remove a plug-in' in text
    assert not any('InputDelayComp' in key for key in lab.state._dirty)


@pytest.mark.parametrize('path', [
    '/NativeDelayCompSupported', '/NativeDelayCompSupported/value',
    '/InputDelayCompOverloaded', '/InputDelayCompOverloaded/value',
    '/InputDelayCompWarning', '/InputDelayCompWarning/value'])
def test_runtime_delay_status_is_read_only(delay_daemon, path):
    lab = delay_daemon
    before = lab.state.get_value(path)
    lab.send('forged', path)
    assert lab.state.get_value(path) == before
    lab.backend.set_compensation.assert_not_called()
    assert lab.daemon._send_local_plugin_error.call_args.args[2] == 'openapollo.read_only'


def test_subscription_reentry_cannot_split_state_and_helper(delay_daemon):
    lab = delay_daemon
    lab.state.register_callback('nested-delay', lambda *_: lab.send('Long'))
    lab.state.subscribe('nested-delay', '/MaxDelayComp')
    lab.send('Short')
    lab.backend.set_compensation.assert_called_once_with('Short')
    assert lab.state.get_value('/MaxDelayComp') == 'Short'
    assert lab.helper.get_value('/MaxDelayComp') == 'Short'
    assert not lab.daemon._plugin_publication_active


def test_helper_publication_failure_is_not_reported_as_success(delay_daemon, monkeypatch):
    lab = delay_daemon
    real_set = lab.helper.set_value

    def reject(path, value):
        return 'read_only' if path == '/MaxDelayComp' else real_set(path, value)

    monkeypatch.setattr(lab.helper, 'set_value', reject)
    lab.send('Long')
    assert lab.tx.quarantined
    lab.backend.invalidate.assert_called_once()
    lab.daemon._send_response.assert_not_called()


@pytest.mark.parametrize('stage', ['helper', 'status', 'latency'])
def test_reentry_is_blocked_through_final_publication_stage(delay_daemon, monkeypatch, stage):
    lab = delay_daemon
    target, name = {
        'helper': (lab.helper, 'set_value'),
        'status': (lab.daemon, '_publish_native_delay_status'),
        'latency': (lab.daemon, '_refresh_latency'),
    }[stage]
    original = getattr(target, name)

    def reenter(*args, **kwargs):
        assert lab.daemon._plugin_publication_active
        lab.send('Long')
        return original(*args, **kwargs)

    monkeypatch.setattr(target, name, reenter)
    lab.send('Short')
    lab.backend.set_compensation.assert_called_once_with('Short')
    assert lab.state.get_value('/MaxDelayComp') == 'Short'
    assert lab.helper.get_value('/MaxDelayComp') == 'Short'
    assert not lab.tx.quarantined
    assert not lab.daemon._plugin_publication_active


@pytest.mark.parametrize('stage', ['backend', 'state', 'helper', 'status', 'latency'])
def test_exceptions_quarantine_and_release_publication_guard(delay_daemon, monkeypatch, stage):
    lab = delay_daemon
    target, name = {
        'backend': (lab.backend, 'set_compensation'),
        'state': (lab.state, 'set'),
        'helper': (lab.helper, 'set_value'),
        'status': (lab.daemon, '_publish_native_delay_status'),
        'latency': (lab.daemon, '_refresh_latency'),
    }[stage]
    monkeypatch.setattr(target, name, Mock(side_effect=RuntimeError('synthetic failure')))
    lab.send('Long')
    assert lab.tx.quarantined
    assert 'synthetic failure' in lab.tx.quarantine_reason
    lab.backend.invalidate.assert_called_once()
    assert not lab.daemon._plugin_publication_active
    lab.daemon._send_response.assert_not_called()
    assert lab.daemon._send_local_plugin_error.call_args.args[2] == 'openapollo.plugin_transaction_indeterminate'


@pytest.mark.parametrize('result', [
    None, SimpleNamespace(status='confirmed', setting='Long'),
    CompensationOutcome(Outcome.CONFIRMED, 'Short')])
def test_unknown_or_mismatched_backend_result_is_quarantined(delay_daemon, result):
    lab = delay_daemon
    lab.backend.set_compensation.side_effect = None
    lab.backend.set_compensation.return_value = result
    lab.send('Long')
    assert lab.tx.quarantined
    lab.backend.invalidate.assert_called_once()
    assert not lab.daemon._plugin_publication_active
    assert lab.state.get_value('/MaxDelayComp') == 'Medium'
    assert lab.helper.get_value('/MaxDelayComp') == 'Medium'
    lab.daemon._send_response.assert_not_called()


@pytest.mark.parametrize('result', ['not_found', False, None])
def test_helper_only_accepts_canonical_ok_result(delay_daemon, monkeypatch, result):
    lab = delay_daemon
    monkeypatch.setattr(lab.helper, 'set_value', Mock(return_value=result))
    lab.send('Long')
    assert lab.tx.quarantined
    lab.backend.invalidate.assert_called_once()
    assert not lab.daemon._plugin_publication_active
    lab.daemon._send_response.assert_not_called()


def test_failed_invalidation_still_quarantines_and_releases_guard(delay_daemon, monkeypatch):
    lab = delay_daemon
    monkeypatch.setattr(lab.state, 'set', Mock(return_value=False))
    lab.backend.invalidate.side_effect = RuntimeError('synthetic invalidation failure')
    lab.send('Long')
    assert lab.tx.quarantined
    assert not lab.daemon._plugin_publication_active
    lab.daemon._send_response.assert_not_called()


def test_standard_helper_absence_does_not_quarantine_confirmed_setting(delay_daemon):
    lab = delay_daemon
    del lab.helper.root['properties']['MaxDelayComp']
    assert lab.helper.get_value('/MaxDelayComp') is None
    lab.send('Long')
    lab.backend.set_compensation.assert_called_once_with('Long')
    assert lab.state.get_value('/MaxDelayComp') == 'Long'
    assert lab.helper.get_value('/MaxDelayComp') is None
    assert not lab.tx.quarantined
    assert not lab.daemon._plugin_publication_active
    lab.backend.invalidate.assert_not_called()
    lab.daemon._send_response.assert_called_once()


@pytest.mark.parametrize('path', ['/Sleep', '/sleep', '/Sleep/value'])
def test_sleep_hint_is_per_client_in_native_mode(delay_daemon, path):
    lab = delay_daemon
    lab.client.sleep = False
    lab.send(True, path)
    assert lab.client.sleep is True
    lab.daemon._send_local_plugin_error.assert_not_called()
    lab.state.on_set.assert_not_called()
    assert lab.driver.calls == []
