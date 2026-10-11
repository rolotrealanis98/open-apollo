import math
import unittest
from plugin_remote_codec import decode_scalar_response, encode_scalar_data, RemoteCodecError
P='/devices/0/inputs/0/effects/0/VendorSpecific/value'
class CodecTests(unittest.TestCase):
    def decode(self,data,params,path=P):
        return decode_scalar_response(path,data,params,expected_path=P)
    def test_float_bits_and_round_trip(self):
        r=self.decode('3f800000,80000000,7fc01234',{'floats':'1,2,3'})
        self.assertEqual(r.words,(0x3f800000,0x80000000,0x7fc01234))
        self.assertEqual(r.float_values()[0],1.0)
        self.assertLess(math.copysign(1,r.float_values()[1]),0)
        self.assertTrue(math.isnan(r.float_values()[2]))
        self.assertEqual(encode_scalar_data(r),'3f800000,80000000,7fc01234')
    def test_ints_exact_no_native_precision_loss(self):
        r=self.decode('-2147483648,16777217,2147483647',{'ints':'1,2,3'})
        self.assertEqual(r.int_values(),(-2147483648,16777217,2147483647))
        self.assertEqual(encode_scalar_data(r),'-2147483648,16777217,2147483647')
    def test_scalar_metadata(self):
        self.assertEqual(self.decode('3F800000',{'floats':34}).ids,(34,))
        self.assertEqual(self.decode(-1,{'ints':-2}).words,(0xffffffff,))
    def test_malformed_is_never_zero_default(self):
        bad=[('garbage',{'floats':'1'}),('3f800000,',{'floats':'1,2'}),
             ('3f800000',{'floats':'1,2'}),('123456789',{'floats':'1'}),
             ('0x3f800000',{'floats':'1'}),(1.0,{'floats':'1'}),
             ('1',{'ints':True}),('1',{'ints':'2147483648'}),
             ('1,2',{'ints':'1,1'}),('1',{'ints':'1','floats':'1'}),
             ('1',{'opcode':518}),('1',{'ints':['1']}),('2147483648',{'ints':'1'})]
        for data,params in bad:
            with self.subTest(data=data,params=params):
                with self.assertRaises(RemoteCodecError):self.decode(data,params)
    def test_path_fence(self):
        with self.assertRaises(RemoteCodecError):self.decode('1',{'ints':'1'},P.replace('/effects/0/','/effects/1/'))
    def test_namespace_is_explicit(self):
        r=self.decode('1',{'ints':'1'})
        with self.assertRaises(RemoteCodecError):r.float_values()

class ArrayCodecTests(unittest.TestCase):
    def decode(self,data,params):
        from plugin_remote_codec import decode_array_response
        return decode_array_response(P,data,params,expected_path=P)
    def params(self,op=519,count=3): return dict(opcode=op,p0=0x05000004,p1=count,p2=1)
    def test_both_array_formats_preserve_all_bits(self):
        from plugin_remote_codec import encode_array_data
        for op,data in [(519,'["3f800000","80000000","7fc01234"]'),
                        (518,'[1065353216,-2147483648,2143294004]')]:
            r=self.decode(data,self.params(op))
            self.assertEqual(r.words,(0x3f800000,0x80000000,0x7fc01234))
            self.assertEqual(self.decode(encode_array_data(r),self.params(op)),r)
    def test_metadata_is_numeric_and_header_opaque(self):
        p=self.params(count=0);p['p2']=0xffffffff
        self.assertEqual(self.decode('[]',p).header_word,0xffffffff)
        p['p0']='83886084'
        with self.assertRaises(RemoteCodecError):self.decode('[]',p)
    def test_partial_extra_invalid_and_ambiguous_rejected(self):
        for data,params in [('[]',self.params()),('["0","1","2","3"]',self.params()),
                            ('["0","bad hex","2"]',self.params()),
                            ('[1,true,2]',self.params(518)),('[1.0,2,3]',self.params(518)),
                            ('[4294967296,2,3]',self.params(518)),('[1,2,3]',self.params()),
                            ('["0","1","2"]',dict(self.params(),floats='1'))]:
            with self.subTest(data=data,params=params):
                with self.assertRaises(RemoteCodecError):self.decode(data,params)

if __name__ == '__main__': unittest.main()
