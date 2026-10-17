"""Unison path, ordering and ioctl gates; synthetic devices only."""
import pytest
import ctypes

from plugin_driver import INSTANCE, PluginDriver, UnisonReceipt
from plugin_native_chain import NativeChainBackend
from plugin_transactions import PluginTransactions
from test_plugin_driver import Ioctl, create
from test_plugin_native import FIELD
from test_plugin_native_chain import Composition, Source, model, slot


PREAMP = '/devices/0/inputs/2/preamps/0/effects/0'


def setup_unison():
    state, helper, driver, old, _ = model()
    for key, kind, value in [('EffectName', 'string', ''), ('EffectInstance', 'pointer', 0),
                              ('Power', 'bool', False), ('Latency', 'int', 0),
                              ('Status', 'string', '')]:
        state.add_runtime_property(PREAMP+'/'+key, {'type': kind, 'value': value})
    preamp = Source('preamp')
    preamp.slot = PREAMP
    calls = []

    def compose(providers, *, channels, slots, record_delay_samples):
        calls.append(slots)
        result = Composition(providers, channels, tuple(max(0, s) for s in slots))
        for view, index in zip(result.effects, slots):
            if index == -1:
                view.slot = PREAMP
        return result

    backend = NativeChainBackend(old.publisher, driver, [preamp, Source('insert')],
        composer=compose, unison=True, replacement=True)
    return state, helper, driver, backend, PluginTransactions(old.publisher, backend), calls


def test_unison_composes_before_insert_zero_and_preserves_both_handles():
    state, helper, driver, backend, tx, calls = setup_unison()
    assert tx.load(PREAMP, 'preamp').committed
    assert tx.control(PREAMP, FIELD, .72).committed
    handle = tx.current_remote_handle(PREAMP)
    assert tx.load(slot(0), 'insert').committed
    assert calls[-1] == (-1, 0)
    assert tx.current_remote_handle(PREAMP) == handle
    assert state.get_value(PREAMP+'/'+FIELD) == helper.get_value(PREAMP+'/'+FIELD) == .72
    assert backend._leases[PREAMP].native is backend._leases[slot(0)].native
    insert_handle = tx.current_remote_handle(slot(0))
    assert tx.unload(PREAMP).committed
    assert calls[-1] == (0,)
    assert tx.current_remote_handle(slot(0)) == insert_handle
    assert len(driver.active) == 1
    assert tx.unload(slot(0)).committed
    assert not driver.active


def test_preamp_capability_is_one_slot_and_uses_the_input_stereo_property():
    state, _, _, backend, _, _ = setup_unison()
    assert next(c for c in backend.capabilities() if c['name'] == 'preamp')['slots'] == [PREAMP]
    state.set_value('/devices/0/inputs/2/Stereo', True)
    assert backend._channels(PREAMP) == (2, 3)
    assert backend.prepare(PREAMP.replace('/effects/0', '/effects/1'), 'preamp').prepared is None


def test_preamp_registration_requires_explicit_unison_support():
    _, _, _, backend, _, _ = setup_unison()
    provider = Source('preamp')
    provider.slot = PREAMP
    with pytest.raises(ValueError, match='supported input'):
        NativeChainBackend(backend.publisher, backend.driver, [provider], composer=lambda: None)


def test_unison_flag_requires_chain_and_is_encoded_without_changing_stage():
    ioctl = Ioctl()
    driver = PluginDriver(19, ioctl=ioctl)
    kwargs = dict(dsp=1, input_pair=(2, 32), commands=(0x00140004, 0, 0, 42))
    with pytest.raises(ValueError, match='admitted chain'):
        create(driver, unison=True, **kwargs)
    assert not ioctl.calls
    create(driver, unison=True, chain=True, stage=True, **kwargs)
    assert ioctl.calls[-1][1][2] == 4 | 8 | 16


def test_owned_unison_journal_encodes_records_and_validates_fence_provenance():
    class JournalIoctl(Ioctl):
        def __init__(self):
            super().__init__()
            self.journals, self.provenance = [], 1

        def __call__(self, fd, number, buf, mutate):
            q = list(INSTANCE.unpack(buf))
            if q[1] != 5:
                return super().__call__(fd, number, buf, mutate)
            assert fd == 19 and q[3] == 7 and q[10] == 23
            pointer = q[15] | q[16] << 32
            self.journals.append(tuple((ctypes.c_uint32*q[14]).from_address(pointer)) if q[14] else ())
            q[17], q[18] = 42, self.provenance
            buf[:] = INSTANCE.pack(*q)
            return 0

    ioctl = JournalIoctl()
    driver = PluginDriver(19, ioctl=ioctl)
    instance = create(driver, unison=True, chain=True, dsp=1, input_pair=(2, 32),
                      commands=(0x00140004, 0, 0, 42))
    assert driver.unison_parameters(instance, [(2, 8, 1), (2, 6, 0)]) == UnisonReceipt(42, 1)
    assert ioctl.journals == [(2, 8, 1, 2, 6, 0)]
    assert driver.unison_parameters(instance) == UnisonReceipt(42, 1)
    assert ioctl.journals[-1] == ()
    for records in ([(4, 6, 0)], [(2, 24, 0)], [(2, 6, -1)], [(2, 6, 0)]*65):
        with pytest.raises(ValueError):
            driver.unison_parameters(instance, records)
    assert len(ioctl.journals) == 2
    ioctl.provenance = 0
    with pytest.raises(OSError):
        driver.unison_parameters(instance)
    with pytest.raises(OSError):
        driver.unison_parameters(instance)
    assert len(ioctl.journals) == 3


def test_unison_begin_activate_require_owned_epoch_and_confirmed_provenance():
    class TransitionIoctl(Ioctl):
        def __init__(self):
            super().__init__()
            self.operations = []
            self.provenance = 1
        def __call__(self, fd, number, buf, mutate):
            q = list(INSTANCE.unpack(buf))
            if q[1] not in (6, 7):
                return super().__call__(fd, number, buf, mutate)
            assert fd == 19 and q[3] == 7 and q[10] == 23
            assert not any(q[13:])
            self.operations.append(q[1])
            q[17], q[18] = 51, self.provenance
            buf[:] = INSTANCE.pack(*q)
            return 0
    ioctl = TransitionIoctl()
    driver = PluginDriver(19, ioctl=ioctl)
    instance = create(driver, chain=True, unison=True, stage=True,
                      dsp=1, input_pair=(2,32), commands=(0x00140004,0,0,42))
    assert driver.begin_unison(instance) == UnisonReceipt(51,1)
    assert driver.activate_unison(instance) == UnisonReceipt(51,1)
    assert ioctl.operations == [6,7]
    ioctl.provenance = 0
    with pytest.raises(OSError): driver.activate_unison(instance)
    with pytest.raises(OSError): driver.begin_unison(instance)
    assert ioctl.operations == [6,7,7]
