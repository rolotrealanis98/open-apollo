import pytest
from plugin_delay import InputLatency,plan_compensation

def test_group_max_is_shared_but_record_and_monitor_are_distinct():
    a,b,c=plan_compensation('Medium',(InputLatency((0,1),31,31),InputLatency((2,),0,0),InputLatency((3,),80,0,group=1)))
    assert (a.record,a.monitor)==(169,0)
    assert (b.record,b.monitor)==(200,31)
    assert (c.record,c.monitor)==(200,0)

def test_stereo_latency_is_counted_once_and_opt_out_does_not_raise_group():
    a,b=plan_compensation('Medium',(InputLatency((0,1),180,180),InputLatency((2,3),500,500,group=-1)))
    assert a.monitor==0 and not a.overloaded
    assert b.monitor==0 and b.overloaded and b.suggested=='Long'

@pytest.mark.parametrize('latency,overload,suggestion',[(99,False,None),(100,False,None),(101,True,'Medium'),(1001,True,None)])
def test_warning_uses_strict_greater_than_and_never_changes_selected_budget(latency,overload,suggestion):
    p,=plan_compensation('Short',(InputLatency((2,),latency,latency),))
    assert p.overloaded is overload and p.suggested==suggestion
    assert p.record==max(100-latency,0)

def test_all_five_settings_capacity_off_reset_and_span_overload():
    for setting,record,capacity in [('Off',0,0),('Short',100,256),('Medium',200,256),('Medium-Long',300,512),('Long',1000,2048)]:
        p,=plan_compensation(setting,(InputLatency((2,),0,0,span=1001),))
        assert (p.record,p.monitor,p.capacity)==(record,0,capacity)
        assert p.overloaded is (setting!='Off')

@pytest.mark.parametrize('items',[(InputLatency((2,),0,1),),(InputLatency((2,),0,0,group=32),),(InputLatency((2,3),0,0),InputLatency((3,),0,0)),(InputLatency((2,),True,0),)])
def test_invalid_topology_or_descriptor_values_are_refused(items):
    with pytest.raises(ValueError):plan_compensation('Medium',items)
