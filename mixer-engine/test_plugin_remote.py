import unittest
from plugin_remote import PluginRemote, parse_remote_query, RemoteQueryError, MAX_IDS
SLOT='/devices/0/inputs/0/effects/0'
class State:
    def path_exists(self,p): return p==SLOT
    def get_value(self,p): raise AssertionError('No captured value replay')
class RemoteTests(unittest.TestCase):
    def setUp(self): self.remote=PluginRemote(State())
    def test_scalar_list(self):
        q=parse_remote_query({'floats':'1,83886084,4294967295,-1','func_id':'9'})
        self.assertEqual(q.ids,(1,83886084,4294967295,-1))
        self.assertEqual(q.kind,'floats')
        self.assertEqual(parse_remote_query({'ints':'1,1'}).ids,(1,1))
    def test_array(self):
        for op in range(516,520):
            q=parse_remote_query({'opcode':str(op),'p0':'83886084','p1':'11','p2':'1'})
            self.assertEqual((q.opcode,q.variable_id,q.count,q.header_word),(op,83886084,11,1))
    def test_unrelated(self):
        for p,q in [(SLOT+'/VendorSpecific/value',{}),(SLOT+'/VendorSpecific',{'func_id':'8'}),
                    (SLOT+'/Power/value',{'floats':'1'}),('/plugins/1/VendorSpecific',{'floats':'1'})]:
            self.assertIsNone(self.remote.handle(p,q))
    def test_valid_no_data(self):
        r=self.remote.handle(SLOT+'/VendorSpecific/value',{'ints':'1'})
        self.assertEqual(r.error,'openapollo.plugin_remote_backend_unavailable')
        self.assertFalse(hasattr(r,'data'))
        self.assertEqual(self.remote.handle('//devices/0/inputs/0/effects/0//VendorSpecific/value/',{'floats':'1'}),
                         self.remote.handle(SLOT+'/VendorSpecific',{'floats':'1'}))
    def test_unknown_slot(self):
        self.assertEqual(self.remote.handle(SLOT.replace('/inputs/0/','/inputs/1/')+'/VendorSpecific',{'ints':'1'}).error,
                         'openapollo.invalid_plugin_slot')
    def test_bad_shapes(self):
        bad=[{'p0':'1'}, {'ints':''},{'ints':'1,'},{'ints':'0x10'},{'ints':'1 2'},{'ints':['1','2']},
             {'ints':'4294967296'},{'ints':'-2147483649'},{'floats':'1','ints':'2'},
             {'floats':','.join(['1']*(MAX_IDS+1))},{'floats':'1','p0':'1'},
             {'opcode':'519','p0':'1','p1':'2'},
             {'opcode':'519','p0':'1','p1':'4097','p2':'1'},
             {'opcode':'519','p0':'1','p1':'2','p2':'[1]'},
             {'opcode':519,'p0':'1','p1':'2','p2':'1'}]
        for q in bad:
            with self.subTest(q=q):
                with self.assertRaises(RemoteQueryError): parse_remote_query(q)
                self.assertEqual(self.remote.handle(SLOT+'/VendorSpecific',q).error,'openapollo.invalid_remote_query')
    def test_unknown_opcode(self):
        self.assertEqual(self.remote.handle(SLOT+'/VendorSpecific',{'opcode':'529'}).error,
                         'openapollo.unsupported_remote_opcode')
if __name__=='__main__': unittest.main()
