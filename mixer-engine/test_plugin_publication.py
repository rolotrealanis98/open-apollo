import copy
import unittest
from state_tree import StateTree
from helper_tree import HelperTree
from plugin_publication import Publisher, LoadedPlugin, OwnedHandle, PublicationError

class PublicationTests(unittest.TestCase):
    slot='/devices/0/auxs/0/effects/0'
    def model(self):
        s=StateTree();h=HelperTree()
        for name,kind,value in [('EffectName','string',''),('EffectInstance','pointer',0),('Power','bool',False),('Latency','int',0),('Status','string','')]:s.add_runtime_property(self.slot+'/'+name,{'type':kind,'value':value})
        s.add_runtime_property('/plugins/0/Authorized',{'type':'bool','value':False})
        return s,h,Publisher(s,h,owner='test')
    def result(self,handle=42,ident=0,value=.5):
        props={'type':{'type':'string','value':'parameter'},'control_id':{'type':'string','value':'original-gain-control'},'Name':{'type':'string','value':'Gain'},'StringValue':{'type':'string','value':'0 dB'},'NormalizedValue':{'type':'float','value':value,'min':0.,'max':1.},'StepValue':{'type':'int','value':1,'values':[0,1]}}
        return LoadedPlugin('fixture',OwnedHandle(handle,'test',0),True,55,({'id':ident,'properties':props},))
    def test_stage_then_coherent_commit(self):
        s,h,p=self.model();before=copy.deepcopy((s.root,h.root));tx=p.stage(self.slot,self.result());self.assertEqual(before,(s.root,h.root));events=tx.commit()
        for name in ('EffectName','EffectInstance','Power','Latency','Status'):self.assertEqual(s.get_value(self.slot+'/'+name+'/value'),h.get_value(self.slot+'/'+name))
        self.assertEqual(h.get_value(self.slot+'/parameters/0/NormalizedValue'),.5)
        self.assertEqual(events[-1],(self.slot+'/EffectInstance/value',42));self.assertFalse(s.get_value('/plugins/0/Authorized/value'))
    def test_name_and_ordinal_without_control_id_remain_uninvented(self):
        s,h,p=self.model();result=self.result(ident=3)
        del result.parameters[0]['properties']['control_id']
        p.stage(self.slot,result).commit()
        self.assertEqual(s.get_value(self.slot+'/parameters/3/Name/value'),'Gain')
        self.assertEqual(h.get_value(self.slot+'/parameters/3/Name'),'Gain')
        self.assertFalse(s.path_exists(self.slot+'/parameters/3/control_id'))
    def test_original_metadata_reaches_both_parameter_paths(self):
        for kind in ('parameter','effect_control'):
            s,h,p=self.model();result=self.result(ident=37)
            props=result.parameters[0]['properties']
            props['type']['value']=kind
            props['control_id']['value']='vendor:threshold-control'
            p.stage(self.slot,result).commit()
            path=self.slot+'/parameters/37'
            for key in ('type','control_id','Name','NormalizedValue','StringValue'):
                self.assertEqual(s.get_value(path+'/'+key+'/value'),props[key]['value'])
                self.assertEqual(h.get_value(path+'/'+key),props[key]['value'])
            self.assertNotEqual(s.get_value(path+'/control_id/value'),'37')
            # Both recursive representations retain the walker-required properties.
            state_node,_=s._resolve_path(path)
            helper_node=h.root
            for part in path.strip('/').split('/'):
                helper_node=next(child for child in helper_node['children'] if child['path']==part)
            self.assertEqual(state_node['properties'],helper_node['properties'])
            props['control_id']['value']='mutated after commit'
            self.assertEqual(s.get_value(path+'/control_id/value'),'vendor:threshold-control')
            self.assertEqual(h.get_value(path+'/control_id'),'vendor:threshold-control')
    def test_missing_or_invalid_original_metadata_rejected_without_publication(self):
        invalid=[('type',None),
                 ('type',{'type':'string','value':'plugin'}),
                 ('type',{'type':'int','value':'parameter'}),
                 ('type',{'type':'string','value':True}),
                 ('control_id',{'type':'string','value':''}),
                 ('control_id',{'type':'string','value':37}),
                 ('control_id',{'type':'int','value':'original'}),
                 ('control_id',{'type':'string','value':'bad\x00control'})]
        for field,meta in invalid:
            with self.subTest(field=field,meta=meta):
                s,h,p=self.model();before=copy.deepcopy((s.root,h.root));result=self.result()
                props=result.parameters[0]['properties']
                if meta is None:props.pop(field)
                else:props[field]=meta
                with self.assertRaises(PublicationError):p.stage(self.slot,result)
                self.assertEqual(before,(s.root,h.root));self.assertEqual(p.version,0)
    def test_replacement_unload(self):
        s,h,p=self.model();p.stage(self.slot,self.result()).commit();p.stage(self.slot,self.result(43,1)).commit()
        self.assertIsNone(s.get_value(self.slot+'/parameters/0/Name/value'));self.assertIsNone(h.get_value(self.slot+'/parameters/0/Name'))
        self.assertEqual(s.get_value(self.slot+'/parameters/1/Name/value'),'Gain');p.stage(self.slot).commit()
        self.assertEqual(s.get_value(self.slot+'/EffectInstance/value'),0);self.assertIsNone(h.get_value(self.slot+'/parameters/1/Name'))
    def test_injected_failure_rollback(self):
        for phase in ('state_swapped','both_swapped'):
            s,h,p=self.model();p.stage(self.slot,self.result()).commit();before=copy.deepcopy((s.root,h.root,s._props));tx=p.stage(self.slot,self.result(43,1))
            def fault(current):
                if current==phase:raise RuntimeError('injected')
            with self.assertRaises(RuntimeError):tx.commit(fault=fault)
            self.assertEqual(before,(s.root,h.root,s._props));self.assertEqual(p.version,1)
    def test_disconnect(self):
        s,h,p=self.model();tx=p.stage(self.slot,self.result());before=copy.deepcopy((s.root,h.root));p.disconnect()
        with self.assertRaises(PublicationError):tx.commit()
        self.assertEqual(before,(s.root,h.root))
        with self.assertRaises(PublicationError):p.stage(self.slot,self.result())
    def test_stale_and_recommit(self):
        s,h,p=self.model();a=p.stage(self.slot,self.result());b=p.stage(self.slot,self.result(43));a.commit()
        with self.assertRaises(PublicationError):b.commit()
        with self.assertRaises(PublicationError):a.commit()
    def test_invalid_handle_parameter(self):
        s,h,p=self.model()
        for lease in [OwnedHandle(0,'test',0),OwnedHandle(5,'other',0),OwnedHandle(5,'test',1),OwnedHandle(True,'test',0)]:
            r=self.result();bad=LoadedPlugin(r.name,lease,r.power,r.latency,r.parameters)
            with self.assertRaises(PublicationError):p.stage(self.slot,bad)
        for value in (float('nan'),float('inf'),'0.5',True,-1,2):
            with self.assertRaises(PublicationError):p.stage(self.slot,self.result(value=value))
    def test_intervening_mixer_change_not_lost(self):
        s,h,p=self.model();tx=p.stage(self.slot,self.result())
        s.add_runtime_property('/mixer/Level',{'type':'float','value':.25})
        with self.assertRaises(PublicationError):tx.commit()
        self.assertEqual(s.get_value('/mixer/Level/value'),.25)
        self.assertEqual(s.get_value(self.slot+'/EffectInstance/value'),0)
    def test_bounded_schema(self):
        s,h,p=self.model();good=self.result()
        for latency in (-1,100000001):
            bad=LoadedPlugin(good.name,good.handle,good.power,latency,good.parameters)
            with self.assertRaises(PublicationError):p.stage(self.slot,bad)
        for ident in (-1,0x100000000):
            with self.assertRaises(PublicationError):p.stage(self.slot,self.result(ident=ident))
        bad=LoadedPlugin(good.name,good.handle,good.power,good.latency,good.parameters*4097)
        with self.assertRaises(PublicationError):p.stage(self.slot,bad)
    def test_staged_payload_tampering_rejected(self):
        for target in ('state_root','helper_root','events','expected_state'):
            s,h,p=self.model();tx=p.stage(self.slot,self.result());before=copy.deepcopy((s.root,h.root))
            payload=getattr(tx,target)
            if isinstance(payload,list):payload.append(('fake',123))
            else:payload['tampered']=True
            with self.assertRaises(PublicationError):tx.commit()
            self.assertEqual(before,(s.root,h.root))
    def test_no_callbacks_or_hardware(self):
        s,h,p=self.model();calls=[];s.on_set=lambda *a:calls.append(a)
        s.register_callback('test',lambda *a:calls.append(a));s.subscribe('test',self.slot+'/EffectInstance/value')
        p.stage(self.slot,self.result()).commit();self.assertEqual(calls,[])

if __name__=='__main__':unittest.main()
