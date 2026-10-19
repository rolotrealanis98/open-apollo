"""Original cache ABI events: Win64 callback arguments decoded before the fixture handlers run.
One replay implementation, without effect-specific coefficient/offset knowledge,
recreates every original fixture's entire word shadow, including counters.
"""
import struct
from offline_host import STUBS
from unicorn.x86_const import UC_X86_REG_XMM2,UC_X86_REG_XMM3

def f32(x):return struct.unpack('<f',struct.pack('<f',x))[0]
def bits(x):return struct.unpack('<I',struct.pack('<f',x))[0]
def double(x):return struct.unpack('<d',struct.pack('<Q',x&0xffffffffffffffff))[0]

class Recorder:
 def _map_image(self,image):
  self.events=[];self.callback_names=set();super()._map_image(image)
 def hook(self,u,addr,size,unused):
  name,fn=self.handlers.get(addr,('',None));self.callback_names.add(name)
  event=None
  if fn is not None and STUBS<=addr<STUBS+0x800:
   slot=(addr-STUBS)//16*8
   if slot==0x90:event=['begin',self.arg(1),self.arg(2),(self.rd(self.arg(3))+1)&0xffffffff]
   elif slot==0x98:event=['end',self.arg(1),self.arg(2)&0xffffffff]
   elif slot==0xa0:event=['write',self.arg(1),[self.arg(2)&0xffffffff]]
   elif slot==0xa8:event=['write',self.arg(1),[u.reg_read(UC_X86_REG_XMM2)&0xffffffff]]
   elif slot==0xb8:
    ref,ptr,n,stride,flag=[self.arg(i) for i in range(1,6)];n&=0xffffffff;stride&=0xffffffff;flag&=0xffffffff
    if not 0<n<=4096 or stride!=1 or flag!=0:raise ValueError('unsupported array contract')
    event=['write',ref,[self.rd(ptr+4*i) for i in range(n)]]
   elif slot==0xe0:event=['timing',self.arg(1),self.arg(2),double(u.reg_read(UC_X86_REG_XMM3)),double(self.arg(4))]
  super().hook(u,addr,size,unused)
  if event is not None and self.failure is None:self.events.append(event)

class Replay:
 def __init__(self,quantum=32,rate=48000):self.words={};self.atomic=None;self.atomic_refs=set();self.quantum=quantum;self.rate=rate
 def apply(self,events):
  for e in events:
   if e[0]=='begin':
    _,start,end,value=e
    if self.atomic is not None or start>=end:raise ValueError('bad begin')
    self.atomic=(start,end,value);self.words[start]=value;self.atomic_refs.update((start,end))
   elif e[0]=='end':
    _,end,value=e
    if self.atomic is None or self.atomic[1:]!=(end,value):raise ValueError('bad end')
    self.words[end]=value;self.atomic=None
   elif e[0]=='write':
    _,ref,values=e
    for i,v in enumerate(values):
     if not 0<=v<=0xffffffff:raise ValueError('bad raw word')
     self.words[ref+i]=v
   elif e[0]=='timing':
    _,ref,kind,divisor,duration=e
    count=int(self.rate/self.quantum*duration+.5);inc=f32(self.quantum/divisor)
    for off,v in ((0,kind),(1,count),(2,bits(inc)),(3,bits(float('inf') if count==0 else f32(1/f32(count*inc)))),(4,0xffffffff),(6,0xffffffff)):self.words[ref+off]=v
   else:raise ValueError('unknown event')
  if self.atomic is not None:raise ValueError('incomplete transaction')
