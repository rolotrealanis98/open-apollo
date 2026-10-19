"""Host callback ABI shared by the executors (first modeled for the Pultec EQP-1A Legacy).
Unicorn only: no Wine, devices or sockets. Imports and unknown host methods fail.
GenericCapture builds its own fixtures; this class only contributes the callbacks.
"""
import struct
from offline_host import OfflineHost, STUBS
from unicorn.x86_const import *


class CapturePultec(OfflineHost):
    def base_constructor(self):
        # UAD2SDK CHypParamBlock ctor0x188610..188706. The listener/vector
        # bookkeeping is not used by coefficient math; notification calls are
        # separately trapped. Derived ctor replaces the primary vtable.
        if self.arg(0)!=self.this:raise ValueError('unexpected base constructor')
        for off in (0x20,0x40,0xe8,0xf0):self.wd(self.this+off,0)
        for off in (0x28,0x30,0x38):self.wq(self.this+off,0)
        self.wd(self.this+0xec,1)
        for i in range(8):
            self.wq(self.this+0x48+i*8,0);self.wq(self.this+0x88+i*8,0);self.wd(self.this+0xc8+i*4,0)
        return self.this

    def parent_callback(self):
        if self.arg(0)!=self.parent or self.arg(1)!=0:
            raise ValueError('unexpected parent lifecycle request')
        return 0  # explicit acceptance for coefficient-only offline state transitions

    def notify(self):
        if self.arg(0)!=self.this or not 0 <= self.arg(1) <= 9:
            raise ValueError('unexpected notification target')
        return 0

    def bind(self,slot,name,fn):
        self.handlers[STUBS+(slot//8)*16]=(name,fn)
    def begin(self):
        start,end,counter=self.arg(1),self.arg(2),self.arg(3)
        if self.atomic is not None:
            raise ValueError('unexpected atomic bracket')
        value=(self.rd(counter)+1)&0xffffffff
        self.wd(counter,value)
        self.shadow[start]=value
        self.atomic=(start,end,value)
        return 0
    def end(self):
        end,value=self.arg(1),self.arg(2)&0xffffffff
        if self.atomic[1:] != (end,value):raise ValueError('mismatched atomic end')
        self.shadow[end]=value
        self.atomic=None
        return 0
    def set_int(self):
        self.shadow[self.arg(1)]=self.arg(2)&0xffffffff
        return 0
    def set_float(self):
        self.shadow[self.arg(1)]=self.uc.reg_read(UC_X86_REG_XMM2)&0xffffffff
        return 0
    def header_timing(self):
        ref, kind = self.arg(1), self.arg(2)
        a=struct.unpack('<d',struct.pack('<Q',self.uc.reg_read(UC_X86_REG_XMM3)&0xffffffffffffffff))[0]
        b=struct.unpack('<d',struct.pack('<Q',self.arg(4)))[0]
        count=int(48000/32*b+.5)
        rate=32/a
        f=lambda v: struct.unpack('<I',struct.pack('<f',v))[0]
        ff=lambda v: struct.unpack('<f',struct.pack('<f',v))[0]
        # Native SSE division yields infinity when the rounded count is zero.
        for off,val in [(0,kind),(1,count),(2,f(rate)),(3,f(float("inf") if count==0 else ff(1/ff(count*rate)))),(4,0xffffffff),(6,0xffffffff)]:
            self.shadow[ref+off]=val
        return 0

    def set_array(self):
        ref,p,count,stride,flag=[self.arg(i) for i in range(1,6)]
        count,stride,flag=(x&0xffffffff for x in (count,stride,flag))
        if not (0<=ref<0x9000000 and 0<count<=128 and stride==1 and flag==0):
            raise ValueError('unexpected array '+str((ref,count,stride,flag)))
        for i in range(count):self.shadow[ref+i]=self.rd(p+4*i)
        return 0
    def _check_completed_call(self):
        if self.atomic is not None:raise RuntimeError('unterminated atomic bracket')
