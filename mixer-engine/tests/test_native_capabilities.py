"""Native catalogue metadata is separate from licensing and has no hardware effects."""
from test_plugin_native import model, SLOT
from ua_mixer_daemon import MixerDaemon
from state_tree import StateTree


def catalogue(state):
    for index, name in enumerate(('synthetic', 'not configured')):
        for key, value, kind in (('Name', name, 'string'),
                                 ('Authorized', True, 'bool')):
            state.add_runtime_property(f'/plugins/{index}/{key}',
                                       {'type': kind, 'value': value})


def test_native_capabilities_publish_all_slots_without_licence_changes():
    state, helper, driver, provider, backend, tx = model()
    catalogue(state)
    second = '/devices/0/inputs/0/effects/0'
    backend.capabilities = lambda: [
        dict(name='synthetic', slots=[SLOT], sample_rate=48000),
        dict(name='synthetic', slots=[second], sample_rate=48000)]
    MixerDaemon(state, None, helper_tree=helper, plugin_transactions=tx,
                plugin_hosting=True)
    assert state.get_value('/NativePluginHosting') is True
    assert state.get_value('/plugins/0/NativeLoadSupported') is True
    assert state.get_value('/plugins/0/NativeLoadSlots') == SLOT + ',' + second
    assert state.get_value('/plugins/0/NativeSampleRate') == 48000
    assert 'input 3, insert 1' in state.get_value('/plugins/0/NativeLoadHint')
    assert 'input 1, insert 1' in state.get_value('/plugins/0/NativeLoadHint')
    assert state.get_value('/plugins/1/NativeLoadSupported') is False
    assert state.get_value('/plugins/1/NativeLoadSlots') == ''
    for index in (0, 1):
        assert state.get_value(f'/plugins/{index}/Authorized') is True
    assert driver.calls == []
    assert not any('Native' in key for key in state._dirty)


def test_legacy_engine_does_not_advertise_native_restrictions():
    state = StateTree()
    catalogue(state)
    MixerDaemon(state, None)
    assert state.get_prop_dict('/NativePluginHosting') is None
    assert state.get_prop_dict('/plugins/0/NativeLoadSupported') is None
