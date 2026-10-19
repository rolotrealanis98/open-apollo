"""Native hosting the driver cannot give yet never takes the daemon down (release blocker,
2026-10-19: a fresh install's driver had not cold-started the DSPs, strip_delays answered ESTALE
and the daemon exited). It runs without plug-ins, says why, and turns them on later."""
import errno

from state_tree import StateTree
from ua_mixer_daemon import MixerDaemon, native_unavailable_reason
from test_plugin_native_layouts import provider, rig


class Router:
    class backend:
        connected = True


def test_reasons_the_user_can_act_on():
    stale = ValueError('initial native compensation could not be confirmed: Compensation device operation unconfirmed')
    assert native_unavailable_reason(stale).startswith('Power-cycle the Apollo')
    chained = RuntimeError('device operation unconfirmed')
    chained.__cause__ = OSError(errno.ESTALE, 'Stale file handle')
    assert native_unavailable_reason(chained).startswith('Power-cycle the Apollo')
    rate = ValueError('native provider sample rate does not match hardware')
    assert '48 kHz' in native_unavailable_reason(rate)
    assert native_unavailable_reason(KeyError('x')).startswith('Plug-in hosting unavailable')


def test_stale_strips_then_cold_start_turns_hosting_on():
    p = provider(1)
    _, driver, backend, tx = rig((p,), max_instances=4)
    state = backend.publisher.state
    daemon = MixerDaemon(state, None, helper_tree=backend.publisher.helper, plugin_hosting=True)
    daemon.hw_router = Router()
    attempts = []

    def attach():
        attempts.append(1)
        if len(attempts) == 1:    # the driver has not cold-started the DSPs since it loaded
            error = RuntimeError('device operation unconfirmed')
            error.__cause__ = OSError(errno.ESTALE, 'Stale file handle')
            raise ValueError('initial native compensation could not be confirmed: '
                             'Compensation device operation unconfirmed') from error
        return backend, tx
    daemon._native_attach = attach

    assert daemon._native_hosting_try() is False
    assert state.get_value('/NativePluginHosting') is False
    assert state.get_value('/NativePluginHostingReason').startswith('Power-cycle the Apollo')
    # Meanwhile a load is refused cleanly instead of pretending to load.
    result = daemon.plugin_lifecycle.handle(p.slot + '/EffectName/value', p.name)
    assert result.error and not driver.instances

    assert daemon._native_hosting_try() is True       # after the cold start
    assert state.get_value('/NativePluginHosting') is True
    assert state.get_value('/NativePluginHostingReason') == ''
    assert daemon._native_plugin_backend is backend and daemon._native_attach is None
    assert daemon.plugin_lifecycle.transactions.load(p.slot, p.name).committed


def test_no_retry_while_the_device_is_away():
    daemon = MixerDaemon(StateTree(), None, plugin_hosting=True)
    daemon._native_attach = lambda: (_ for _ in ()).throw(AssertionError('must not be called'))
    assert daemon._native_hosting_try() is False


def test_a_quarantine_is_logged_and_published(caplog):
    p = provider(1)
    _, driver, backend, tx = rig((p,), max_instances=4)
    state = backend.publisher.state
    daemon = MixerDaemon(state, None, helper_tree=backend.publisher.helper,
                         plugin_transactions=tx, plugin_hosting=True)
    with caplog.at_level('ERROR'):
        result = tx._quarantine('Activation unconfirmed')
    assert 'quarantined' in caplog.text and 'Activation unconfirmed' in caplog.text
    from protocol import Command
    daemon._finish_plugin_set(None, p.slot + '/EffectName/value', Command('set', p.slot + '/EffectName/value', p.name),
                              {}, type('R', (), {'committed': False, 'error': None})())
    assert state.get_value('/NativePluginHosting') is False
    assert 'restart the mixer daemon' in state.get_value('/NativePluginHostingReason')


def test_retries_are_quiet_and_recovery_is_one_line(caplog):
    import logging
    p = provider(1)
    _, driver, backend, tx = rig((p,), max_instances=4)
    daemon = MixerDaemon(backend.publisher.state, None, helper_tree=backend.publisher.helper, plugin_hosting=True)
    daemon.hw_router = Router()
    tries = []

    def attach():
        tries.append(1)
        if len(tries) < 4:
            logging.getLogger('plugin_native_chain').exception('native chain: compensation device operation raised')
            raise ValueError('initial native compensation could not be confirmed')
        return backend, tx
    daemon._native_attach = attach
    with caplog.at_level('INFO'):
        while not daemon._native_hosting_try():
            pass
    text = caplog.text
    assert 'compensation device operation raised' not in text           # no traceback every 5 s
    assert text.count('still unavailable') == 1 and text.count('Native plug-in hosting on') == 1


def test_preparation_progress_and_preparing_status(tmp_path, monkeypatch):
    import native_profiled_carrier_provider as npc
    p = provider(1)
    state, driver, backend, tx = rig((p,), max_instances=4)
    daemon = MixerDaemon(state, None, helper_tree=backend.publisher.helper,
                         plugin_transactions=tx, plugin_hosting=True)
    learned = set()
    monkeypatch.setattr(npc, 'load_defaults', lambda path: (1,) if path in learned else None)
    daemon._native_preparing = {'/maps/x-mono': p.name}
    daemon._native_prepare_poll()
    assert state.get_value('/NativePluginsPreparing') == 1 and state.get_value('/NativePluginsReady') == 0
    assert state.get_value('/NativePluginHostingStatus').startswith('Preparing plug-ins: 0 of 1 ready')
    learned.add('/maps/x-mono')
    daemon._native_prepare_poll()
    assert state.get_value('/NativePluginsPreparing') == 0 and state.get_value('/NativePluginsReady') == 1
    assert state.get_value('/NativePluginHostingStatus') == ''
