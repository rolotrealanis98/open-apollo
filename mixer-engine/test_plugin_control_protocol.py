"""Daemon framing and persistence with injected control attestations, no DSP."""
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import Mock
import test_plugin_control_transactions as fixtures
from plugin_test_support import MemoryWriter
from protocol import Command
from ua_mixer_daemon import MixerDaemon, MixerClient
from ubjson_codec import decode_frame
from plugin_transactions import Outcome


class ControlProtocolTests(unittest.TestCase):
    def model(self,binary):
        f,s,h,p,b,c=fixtures.Controls().model()
        s.add_runtime_property('/Dirty',{'type':'bool','value':False})
        h.root.setdefault('properties',{})['Dirty']={'type':'bool','value':False}
        d=MixerDaemon(s,None,helper_tree=h,plugin_transactions=c)
        d._session_ready=True
        writer=MemoryWriter();client=MixerClient(None,writer,ubjson=binary)
        client.ubjson=binary;client.helper=binary
        s.on_set=Mock();s._schedule_save=Mock()
        return f,s,h,b,c,d,writer,client

    def messages(self,writer,binary):
        return [decode_frame(frame)[0] if binary else json.loads(frame.rstrip(b'\0')) for frame in writer.frames]

    def test_quantized_values_notify_coherently_and_persist_actual_readback(self):
        for binary in (False,True):
            f,s,h,b,c,d,w,client=self.model(binary);seen=[]
            def observe(path,value):
                self.assertEqual(s.get_value(f.slot+'/EffectInstance'),42)
                self.assertEqual(h.get_value(f.slot+'/EffectInstance'),42)
                self.assertEqual(s.get_value(f.slot+'/parameters/0/NormalizedValue'),.75)
                self.assertEqual(h.get_value(f.slot+'/parameters/0/StringValue'),'confirmed')
                seen.append((path,value))
            s.register_callback('watch',observe)
            for field in ('NormalizedValue','StringValue','StepValue'):
                s.subscribe('watch',f.slot+'/parameters/0/'+field+'/value')
            with tempfile.TemporaryDirectory() as directory:
                path=Path(directory)/'session.json';s.enable_persistence(path)
                d._handle_set(client,Command('set',f.slot+'/parameters/0/NormalizedValue/value',.8,raw_params={'func_id':'72'}))
                self.assertEqual(len(seen),3)
                self.assertEqual(b.calls,['prepare_control','apply_control'])
                self.assertTrue(s.get_value('/Dirty'));self.assertTrue(h.get_value('/Dirty'))
                s.on_set.assert_not_called()
                s.save_now();saved=json.loads(path.read_text())
                self.assertEqual(saved[f.slot+'/parameters/0/NormalizedValue/value'],.75)
                # Existing protocol acknowledges the request. Only notifications
                # and published tree values are authoritative readback here.
                response=self.messages(w,binary)[-1]
                self.assertNotIn('error',response)
                self.assertAlmostEqual(response['data'],.8,places=6)

    def test_failed_control_does_not_dirty_or_persist(self):
        for binary in (False,True):
            f,s,h,b,c,d,w,client=self.model(binary)
            b.apply_status=Outcome.INDETERMINATE
            d._handle_set(client,Command('set',f.slot+'/Power/value',False))
            self.assertTrue(s.get_value(f.slot+'/Power'))
            self.assertFalse(s.get_value('/Dirty'))
            s._schedule_save.assert_not_called()
            self.assertIn('error',self.messages(w,binary)[-1])

    def test_confirmed_unchanged_control_does_not_dirty_or_persist(self):
        for binary in (False,True):
            f,s,h,b,c,d,w,client=self.model(binary)
            d._handle_set(client,Command('set',f.slot+'/Power/value',True,raw_params={'func_id':'73'}))
            self.assertEqual(b.calls,['prepare_control','apply_control'])
            self.assertFalse(s.get_value('/Dirty'))
            s._schedule_save.assert_not_called()
            self.assertNotIn('error',self.messages(w,binary)[-1])

    def test_notification_reentry_cannot_apply_second_control(self):
        for binary in (False,True):
            f,s,h,b,c,d,w,client=self.model(binary)
            def reenter(path,value):
                d._handle_set(client,Command('set',f.slot+'/Power/value',True))
            s.register_callback('reenter',reenter);s.subscribe('reenter',f.slot+'/Power/value')
            d._handle_set(client,Command('set',f.slot+'/Power/value',False))
            self.assertEqual(b.calls,['prepare_control','apply_control'])
            self.assertFalse(s.get_value(f.slot+'/Power'))
            errors=[response['error'] for response in self.messages(w,binary) if 'error' in response]
            self.assertEqual(errors,['openapollo.plugin_transaction_reentrant'])


if __name__=='__main__':unittest.main()
