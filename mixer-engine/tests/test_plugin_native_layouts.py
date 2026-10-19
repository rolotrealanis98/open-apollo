"""Independent native leases, linked layouts, and kernel feature admission."""
from test_plugin_native import Provider, SLOT, FIELD
from plugin_native import NativePluginBackend, register_native
from plugin_driver import NativeInstance
from plugin_publication import Publisher
from plugin_transactions import PluginTransactions
from state_tree import StateTree
from helper_tree import HelperTree
from unittest.mock import Mock

class MemoryDriver:
    def __init__(self):
        self.instances={};self.next_handle=1;self.calls=[];self.bad_next=False
    def create(self,**args):
        instance=NativeInstance(self.next_handle,1,23);self.next_handle+=1
        self.instances[instance.handle]=args['commands'][-1]
        self.calls.append(('create',args['input_pair']))
        if self.bad_next:self.instances[instance.handle]+=1;self.bad_next=False
        return instance
    def service(self,instance,*,writes=(),read=None):
        assert instance.handle in self.instances
        self.calls.append(('service',instance.handle))
        if writes:self.instances[instance.handle]=writes[-1];return ()
        return (self.instances[instance.handle],)
    def destroy(self,instance):
        del self.instances[instance.handle];self.calls.append(('destroy',instance.handle))
    def invalidate(self):pass

def provider(channel,name='synthetic',stereo=False):
    p=Provider();p.slot=f'/devices/0/inputs/{channel}/effects/0';p.name=name
    if stereo:p.channels=(channel,channel+1)
    original=p.create_arguments
    p.create_arguments=lambda: original()|{'input_pair':(channel,channel+1 if stereo else 32)}
    return p

def rig(providers,linked=(),**options):
    state,helper=StateTree(),HelperTree()
    for p in providers:
        for name,kind,value in [('EffectName','string',''),('EffectInstance','pointer',0),('Power','bool',False),('Latency','int',0),('Status','string','')]:
            path=p.slot+'/'+name
            if not state.path_exists(path):state.add_runtime_property(path,{'type':kind,'value':value})
    for i in linked:state.add_runtime_property(f'/devices/0/inputs/{i}/Stereo',{'type':'bool','value':True})
    publisher=Publisher(state,helper,owner='layouts');driver=MemoryDriver()
    backend=NativePluginBackend(publisher,driver,providers,**options)
    return state,driver,backend,PluginTransactions(publisher,backend)

def test_independent_channels_keep_encoders_controls_and_retirement_separate():
    a,b=provider(2),provider(0,'second')
    state,driver,backend,tx=rig((a,b),max_instances=4)
    assert tx.load(a.slot,a.name).committed and tx.load(b.slot,b.name).committed
    assert tx.control(a.slot,FIELD,.25).committed
    assert tx.control(b.slot,FIELD,.75).committed
    assert list(driver.instances.values())==[25,75]
    assert tx.unload(a.slot).committed
    assert len(driver.instances)==1 and tx.control(b.slot,FIELD,.5).committed
    assert tx.unload(b.slot).committed and not driver.instances

def test_failed_second_initial_readback_retires_only_candidate():
    a,b=provider(2),provider(0,'second')
    state,driver,backend,tx=rig((a,b),max_instances=4)
    assert tx.load(a.slot,a.name).committed
    driver.bad_next=True
    result=tx.load(b.slot,b.name)
    assert result.error and not result.quarantined
    assert set(driver.instances)=={1} and set(backend._leases)=={a.slot}
    assert tx.control(a.slot,FIELD,.75).committed
    assert state.get_value(b.slot+'/EffectInstance')==0

def test_linked_pair_selects_stereo_recipe_and_refuses_slave_or_overlap():
    mono,stereo,slave=provider(2),provider(2,stereo=True),provider(3)
    state,driver,backend,tx=rig((mono,stereo,slave),linked=(2,3),max_instances=4,stereo=True)
    assert tx.load(stereo.slot,stereo.name).committed
    assert driver.calls[0]==('create',(2,3))
    assert backend._leases[stereo.slot].provider is stereo
    before=list(driver.calls)
    assert tx.load(slave.slot,slave.name).error and driver.calls==before
    assert tx.load(stereo.slot,stereo.name).error and driver.calls==before
    assert tx.unload(stereo.slot).committed

def test_two_stereo_pairs_can_coexist_and_remove_in_reverse_order():
    a,b=provider(0,stereo=True),provider(2,stereo=True)
    _,driver,backend,tx=rig((a,b),linked=range(4),max_instances=4,stereo=True)
    assert tx.load(a.slot,a.name).committed and tx.load(b.slot,b.name).committed
    assert tx.unload(b.slot).committed and tx.control(a.slot,FIELD,.75).committed
    assert tx.unload(a.slot).committed

def test_missing_stereo_recipe_or_kernel_support_never_reaches_create():
    for providers,options in [((provider(2),),{'stereo':True}),((provider(2,stereo=True),),{})]:
        _,driver,backend,tx=rig(providers,linked=(2,3),**options)
        assert tx.load(SLOT,'synthetic').error and driver.calls==[]

def test_old_kernel_instance_limit_still_refuses_second_input():
    a,b=provider(2),provider(0)
    _,driver,backend,tx=rig((a,b))
    assert tx.load(a.slot,a.name).committed
    before=list(driver.calls)
    assert tx.load(b.slot,b.name).error and driver.calls==before

def test_registration_uses_explicit_driver_features():
    for features,maximum,stereo in [(1,1,False),(7,4,True)]:
        hw=Mock(connected=True,fd=19);hw.get_driver_param.return_value=48000;hw.features2.return_value=features
        backend,_=register_native(StateTree(),HelperTree(),hw,(provider(2),))
        assert backend.max_instances==maximum and backend.stereo is stereo
        hw.open.assert_not_called()

def test_missing_layout_names_the_bundles_layout_and_the_fix():
    stereo_only,mono_only=provider(0,'UAD Stereo',stereo=True),provider(2,'UAD Mono')
    state,driver,backend,tx=rig((stereo_only,mono_only),linked=(2,3),max_instances=4,stereo=True)
    result=tx.load(stereo_only.slot,stereo_only.name)
    assert 'UAD Stereo: stereo only — link the input or use a mono bundle' == result.detail
    result=tx.load(mono_only.slot,mono_only.name)
    assert 'UAD Mono: mono only — unlink the input or use a stereo bundle' == result.detail
    assert not driver.calls


def test_words_the_running_plugin_owns_are_learned_not_failed():
    import plugin_native
    p = provider(1)
    state, driver, backend, tx = rig((p,), max_instances=4)
    assert tx.load(p.slot, p.name).committed
    native = backend._leases[p.slot].native
    reads = iter([(1, 2, 3), (1, 9, 3)])   # word 1 moves between reads, word 2 stays wrong
    backend.driver = type('D', (), {'service': lambda self, n, read: next(reads)})()
    import pytest
    with pytest.raises(ValueError, match='word 2'):
        backend._observe(native, ((10, 0, (1, 0, 0)),))
    assert (10, 1) in backend._dynamic[native.handle] and (10, 2) not in backend._dynamic[native.handle]
