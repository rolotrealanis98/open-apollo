import copy
import unittest
from plugin_publication import ControlReadback, OwnedHandle, PublicationError
import test_plugin_publication as fixtures


class ControlPublicationTests(unittest.TestCase):
    def model(self):
        fixture=fixtures.PublicationTests()
        state,helper,publisher=fixture.model()
        result=fixture.result()
        publisher.stage(fixture.slot,result).commit()
        return fixture.slot,state,helper,publisher,result.handle

    def test_quantized_readback_preserves_identity_metadata_and_other_values(self):
        slot,s,h,p,lease=self.model()
        before=copy.deepcopy((s.root,h.root))
        field='parameters/0/NormalizedValue'
        snap=p.snapshot_control(slot,lease,field)
        self.assertEqual(snap.values,{'NormalizedValue':.5,'StringValue':'0 dB','StepValue':1})
        result=ControlReadback(lease,field,{'NormalizedValue':.75,'StringValue':'6 dB','StepValue':0})
        tx=p.stage_control(slot,result)
        self.assertEqual(before,(s.root,h.root))
        result.values['StringValue']='tampered adapter output'
        events=tx.commit()
        self.assertEqual(len(events),3)
        self.assertEqual(s.get_value(slot+'/parameters/0/StringValue'),'6 dB')
        for field,value in [('EffectInstance',42),('EffectName','fixture'),('Latency',55),('Power',True),('parameters/0/Name','Gain'),('parameters/0/type','parameter'),('parameters/0/control_id','original-gain-control')]:
            self.assertEqual(s.get_value(slot+'/'+field),value)
            self.assertEqual(h.get_value(slot+'/'+field),value)
        node,_=s._resolve_path(slot+'/parameters/0')
        self.assertEqual(node['properties']['NormalizedValue']['min'],0.)
        self.assertEqual(node['properties']['NormalizedValue']['max'],1.)
        self.assertFalse(any('EffectInstance' in path for path,_ in events))

    def test_power_and_noop_have_only_target_events(self):
        slot,s,h,p,lease=self.model()
        self.assertEqual(p.stage_control(slot,p.snapshot_control(slot,lease,'Power')).commit(),[])
        events=p.stage_control(slot,ControlReadback(lease,'Power',{'Power':False})).commit()
        self.assertEqual(events,[(slot+'/Power/value',False)])
        self.assertFalse(s.get_value(slot+'/Power'))
        self.assertEqual(h.get_value(slot+'/EffectInstance'),42)

    def test_invalid_identity_field_schema_and_readbacks_do_not_mutate(self):
        slot,s,h,p,lease=self.model()
        before=copy.deepcopy((s.root,h.root))
        bad=[ControlReadback(OwnedHandle(43,'test',0),'Power',{'Power':False}),
             ControlReadback(OwnedHandle(42,'test',1),'Power',{'Power':False}),
             ControlReadback(lease,'Power',{'Power':1}),
             ControlReadback(lease,'parameters/00/NormalizedValue',{}),
             ControlReadback(lease,'parameters/1/NormalizedValue',{}),
             ControlReadback(lease,'parameters/0/Name',{}),
             ControlReadback(lease,'parameters/0/NormalizedValue',{'NormalizedValue':.2})]
        for value in (True,float('nan'),-1,2,10**1000):
            bad.append(ControlReadback(lease,'parameters/0/NormalizedValue',{'NormalizedValue':value,'StringValue':'x','StepValue':1}))
        bad.append(ControlReadback(lease,'parameters/0/StepValue',{'NormalizedValue':.2,'StringValue':'x','StepValue':True}))
        for result in bad:
            with self.assertRaises(PublicationError):p.stage_control(slot,result)
            self.assertEqual(before,(s.root,h.root))
        helper_node=h.root
        for part in (slot+'/parameters/0').strip('/').split('/'):
            helper_node=next(child for child in helper_node['children'] if child['path']==part)
        helper_node['properties']['NormalizedValue']['max']=2
        with self.assertRaises(PublicationError):p.snapshot_control(slot,lease,'parameters/0/NormalizedValue')
        slot,s,h,p,lease=self.model()
        helper_node=h.root
        for part in slot.strip('/').split('/'):
            helper_node=next(child for child in helper_node['children'] if child['path']==part)
        helper_node['properties']['Power']['read_only']=True
        with self.assertRaises(PublicationError):p.snapshot_control(slot,lease,'Power')

    def test_parameter_identity_metadata_must_agree_before_readback(self):
        for field,value in [('type','effect_control'),('control_id','different-original-id')]:
            with self.subTest(field=field):
                slot,s,h,p,lease=self.model()
                helper=h.root
                for part in (slot+'/parameters/0').strip('/').split('/'):
                    helper=next(child for child in helper['children'] if child['path']==part)
                helper['properties'][field]['value']=value
                before=copy.deepcopy((s.root,h.root))
                with self.assertRaises(PublicationError):
                    p.snapshot_control(slot,lease,'parameters/0/NormalizedValue')
                self.assertEqual(before,(s.root,h.root))

    def test_atomic_fault_stale_generation_and_intervening_changes(self):
        for phase in ('state_swapped','both_swapped'):
            slot,s,h,p,lease=self.model();before=copy.deepcopy((s.root,h.root,s._props))
            tx=p.stage_control(slot,ControlReadback(lease,'Power',{'Power':False}))
            def fail(current):
                if current==phase:raise RuntimeError('injected')
            with self.assertRaises(RuntimeError):tx.commit(fault=fail)
            self.assertEqual(before,(s.root,h.root,s._props))
        slot,s,h,p,lease=self.model()
        tx=p.stage_control(slot,ControlReadback(lease,'Power',{'Power':False}))
        s.add_runtime_property('/other/Value',{'type':'int','value':7})
        with self.assertRaises(PublicationError):tx.commit()
        self.assertEqual(s.get_value('/other/Value'),7)
        tx=p.stage_control(slot,ControlReadback(lease,'Power',{'Power':False}))
        p.disconnect()
        with self.assertRaises(PublicationError):tx.commit()
        with self.assertRaises(PublicationError):p.snapshot_control(slot,lease,'Power')


if __name__=='__main__':unittest.main()
