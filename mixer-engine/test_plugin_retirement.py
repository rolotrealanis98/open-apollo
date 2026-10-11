"""Injected terminal-retirement attestations, never DSP execution."""
import copy,json,unittest
from threading import Thread
from unittest.mock import Mock
import test_plugin_transactions as fixtures
from plugin_transactions import RetireOutcome,Outcome
from plugin_publication import OwnedHandle
from ua_mixer_daemon import MixerDaemon,MixerClient
from protocol import Command
from plugin_test_support import MemoryWriter
from ubjson_codec import decode_frame
class Retirement(unittest.TestCase):
    def test_disconnect_during_empty_publication_suppresses_stale_events(self):
        f,s,h,p,b,c=self.model()
        c.publication_fault=lambda phase:p.disconnect() if phase=='both_swapped' else None
        result=c.unload(f.slot)
        self.assertTrue(result.committed)
        self.assertTrue(result.quarantined)
        self.assertEqual(result.events,())
        self.assertEqual(len(c.unresolved),1)
        self.assertEqual(c.unresolved[0].generation,0)
        self.assertEqual(s.get_value(f.slot+'/EffectInstance'),0)

    def model(self):
        f,s,h,p,b,c=fixtures.Transactions().model(True)
        def retire(slot,lease):
            b.calls.append('retire');self.assertEqual(s.get_value(slot+'/EffectInstance'),42)
            return RetireOutcome(Outcome.CONFIRMED,lease,retired=True)
        b.retire=retire;return f,s,h,p,b,c
    def test_confirmed_clears_both_views(self):
        f,s,h,p,b,c=self.model();r=c.unload(f.slot)
        self.assertTrue(r.committed);self.assertIsNone(r.error);self.assertEqual(b.calls,['retire'])
        for tree in (s,h):
            self.assertEqual(tree.get_value(f.slot+'/EffectInstance'),0)
            self.assertEqual(tree.get_value(f.slot+'/EffectName'),'')
            self.assertIsNone(tree.get_value(f.slot+'/parameters/0/NormalizedValue'))
        self.assertEqual(r.events[-1],(f.slot+'/EffectInstance/value',0))
    def test_explicit_failure_unchanged(self):
        f,s,h,p,b,c=self.model();before=copy.deepcopy((s.root,h.root))
        b.retire=lambda slot,lease:RetireOutcome(Outcome.FAILED,lease,previous_retained=True)
        r=c.unload(f.slot);self.assertFalse(r.committed);self.assertFalse(r.quarantined)
        self.assertEqual((s.root,h.root),before)
    def test_unknown_blocks_followups(self):
        for mode in ('unknown','exception','malformed','false_generation','contradictory'):
            f,s,h,p,b,c=self.model();before=copy.deepcopy((s.root,h.root))
            def retire(slot,lease):
                b.calls.append('retire')
                if mode=='exception':raise RuntimeError('lost connection')
                if mode=='malformed':return None
                if mode=='false_generation':return RetireOutcome(Outcome.CONFIRMED,OwnedHandle(lease.value,lease.owner,False),retired=True)
                if mode=='contradictory':return RetireOutcome(Outcome.FAILED,lease,previous_retained=True,retired=True)
                return RetireOutcome(Outcome.INDETERMINATE,lease)
            b.retire=retire;r=c.unload(f.slot)
            self.assertTrue(r.quarantined);self.assertFalse(r.committed);self.assertEqual((s.root,h.root),before)
            self.assertEqual(len(c.unresolved),1);c.unload(f.slot);c.load(f.slot,'fixture');self.assertEqual(b.calls,['retire'])
    def test_post_retirement_publication_fault_never_rolls_back_hardware(self):
        f,s,h,p,b,c=self.model();before=copy.deepcopy((s.root,h.root))
        def fault(phase):raise RuntimeError('publication failed')
        c.publication_fault=fault;r=c.unload(f.slot)
        self.assertTrue(r.quarantined);self.assertFalse(r.committed);self.assertEqual((s.root,h.root),before)
        self.assertEqual(b.calls,['retire']);self.assertIn('Retired;',r.detail)
    def test_mismatch_or_missing_adapter_never_retires(self):
        f,s,h,p,b,c=self.model();h._resolve(f.slot)['properties']['EffectInstance']['value']=43
        self.assertFalse(c.unload(f.slot).committed);self.assertEqual(b.calls,[])
        f,s,h,p,b,c=self.model();del b.retire
        self.assertEqual(c.unload(f.slot).error,'openapollo.plugin_backend_unavailable')
    def test_thread_and_reentrancy_guard(self):
        f,s,h,p,b,c=self.model();results=[]
        t=Thread(target=lambda:results.append(c.unload(f.slot)));t.start();t.join()
        self.assertEqual(results[0].error,'openapollo.plugin_publication_owner_mismatch');self.assertEqual(b.calls,[])
        original=b.retire
        def retire(slot,lease):results.append(c.unload(slot));return original(slot,lease)
        b.retire=retire;self.assertTrue(c.unload(f.slot).committed)
        self.assertEqual(results[-1].error,'openapollo.plugin_transaction_reentrant')
    def test_both_wire_protocols_dirty_persistence_identity_last(self):
        for binary in (False,True):
            f,s,h,p,b,c=self.model();s.add_runtime_property('/Dirty',{'type':'bool','value':False})
            h.root.setdefault('properties',{})['Dirty']={'type':'bool','value':False}
            d=MixerDaemon(s,None,helper_tree=h,plugin_transactions=c);d._session_ready=True
            s._schedule_save=Mock();s.on_set=Mock();s._dirty[f.slot+'/EffectInstance/value']=42
            s._dirty[f.slot+'/parameters/0/NormalizedValue/value']=.5;events=[]
            def observe(path,value):
                self.assertEqual(s.get_value(f.slot+'/EffectInstance'),0);self.assertEqual(h.get_value(f.slot+'/EffectInstance'),0)
                events.append((path,value))
            s.register_callback('observer',observe)
            for field in ('EffectName','Power','Latency','Status','EffectInstance'):s.subscribe('observer',f.slot+'/'+field+'/value')
            writer=MemoryWriter();client=MixerClient(None,writer,ubjson=binary);client.ubjson=binary
            d._handle_set(client,Command('set',f.slot+'/EffectName/value','',raw_params={'func_id':'7'}))
            messages=[decode_frame(frame)[0] if binary else json.loads(frame.rstrip(b'\0')) for frame in writer.frames]
            self.assertEqual(messages[-1]['data'],'');self.assertEqual(events[-1],(f.slot+'/EffectInstance/value',0))
            self.assertTrue(s.get_value('/Dirty'));self.assertTrue(h.get_value('/Dirty'))
            self.assertNotIn(f.slot+'/EffectInstance/value',s._dirty);self.assertNotIn(f.slot+'/parameters/0/NormalizedValue/value',s._dirty)
            s.on_set.assert_not_called()
if __name__=='__main__':unittest.main()
