"""Replacement keeps old routing through preparation and reverses publication failure."""
import errno
from unittest.mock import Mock
import pytest
from test_plugin_native_layouts import rig, provider
from test_plugin_native import FIELD
from test_plugin_driver import Ioctl, create
from plugin_driver import PluginDriver

def fixture():
    a,b=provider(2),provider(2,'replacement')
    state,driver,backend,tx=rig((a,b),replacement=True)
    driver.routes={};original_create=driver.create
    def create_(**args):
        instance=original_create(**args)
        if not args.get('stage'):driver.routes[args['input_pair']]=instance.handle
        return instance
    def swap(old,new):
        assert old.handle in driver.instances and new.handle in driver.instances
        pair=(2,32);assert driver.routes[pair]==old.handle
        driver.calls.append(('swap',old.handle,new.handle));driver.routes[pair]=new.handle
    driver.create=create_;driver.swap=swap
    assert tx.load(a.slot,a.name).committed
    return a,b,state,driver,backend,tx

def test_replacement_stages_then_routes_then_retires_old():
    a,b,state,d,backend,tx=fixture()
    assert tx.control(a.slot,FIELD,.75).committed
    assert tx.load(a.slot,b.name).committed
    assert state.get_value(a.slot+'/EffectName')==b.name
    assert set(d.instances)=={2} and d.routes[(2,32)]==2
    assert d.calls.index(('swap',1,2)) < d.calls.index(('destroy',1))
    assert tx.control(a.slot,FIELD,.25).committed and d.instances[2]==25
    assert not backend._pending

def test_readback_failure_never_switches_the_old_route():
    a,b,state,d,backend,tx=fixture();d.bad_next=True
    result=tx.load(a.slot,b.name)
    assert result.error and not result.quarantined
    assert d.routes[(2,32)]==1 and set(d.instances)=={1}
    assert not any(call[0]=='swap' for call in d.calls)
    assert state.get_value(a.slot+'/EffectName')==a.name
    assert tx.control(a.slot,FIELD,.75).committed

def test_publication_failure_swaps_back_before_candidate_retirement():
    a,b,state,d,backend,tx=fixture()
    def fail(*_):raise RuntimeError('publication fault')
    tx.publication_fault=fail
    result=tx.load(a.slot,b.name)
    assert result.error and not result.quarantined
    assert d.routes[(2,32)]==1 and set(d.instances)=={1}
    assert d.calls.index(('swap',1,2)) < d.calls.index(('swap',2,1)) < d.calls.index(('destroy',2))
    assert state.get_value(a.slot+'/EffectName')==a.name and not backend._pending

def test_same_type_candidate_has_independent_encoder_before_publication():
    a,b,state,d,backend,tx=fixture()
    assert tx.control(a.slot,FIELD,.75).committed
    old=backend._leases[a.slot]
    candidate=backend.prepare(a.slot,a.name).prepared
    assert candidate.token.provider is not old.provider
    assert old.provider.word==75 and candidate.token.provider.word==50
    assert backend.rollback(candidate).candidate_retired
    assert old.provider.word==75 and set(d.instances)=={1}

def test_swap_ioctl_uses_owned_identities_and_shared_epoch():
    io=Ioctl();d=PluginDriver(19,ioctl=io)
    args=dict(dsp=1,input_pair=(2,3),commands=(0x00140004,0,0,42))
    a=create(d,**args);io.handle=8;b=create(d,stage=True,**args)
    assert io.calls[-1][1][2]==4
    d.swap(a,b);q=io.calls[-1][1]
    assert q[1]==4 and q[3]==7 and q[10]==23 and q[13]==8
    assert all(v==0 for i,v in enumerate(q) if i not in (0,1,3,10,13))
    before=len(io.calls)
    with pytest.raises(ValueError):d.swap(a,a)
    assert len(io.calls)==before
    io.fail=OSError(errno.ETIMEDOUT,'unknown hardware result')
    with pytest.raises(OSError):d.swap(b,a)
    with pytest.raises(OSError,match='invalidated'):d.service(a,read=(0,0,1))
