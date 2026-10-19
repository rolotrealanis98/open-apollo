"""Data-only Win64 CHyp profile generation. Fixed cohort; no plugin address tables."""
from pathlib import Path
import sys,json,hashlib,re,struct,bisect,time
import pefile
from capstone import Cs,CS_ARCH_X86,CS_MODE_64
from capstone.x86 import X86_OP_REG,X86_OP_MEM,X86_OP_IMM
HERE=Path(__file__).resolve().parent
from discover_abi import discover,ROOT
SDK_CTOR='??0CHypParamBlock@@QEAA@XZ'
NONVOL=('rbx','rbp','rsi','rdi','r12','r13','r14','r15')
def add(a,b):
 if isinstance(a,int):return a+b
 if isinstance(a,tuple):return(a[0],a[1]+b)
 return None
def canon(r):
 if r is None:return ''
 table={'eax':'rax','ax':'rax','al':'rax','ecx':'rcx','cx':'rcx','cl':'rcx','edx':'rdx','dx':'rdx','dl':'rdx','ebx':'rbx','bx':'rbx','bl':'rbx','esi':'rsi','edi':'rdi','ebp':'rbp','esp':'rsp'}
 return table.get(r,r[:-1]if re.fullmatch(r'r\d+[dwb]',r)else r)
class Image:
 def __init__(self,path):
  self.raw=path.read_bytes();self.p=pefile.PE(data=self.raw,fast_load=True);self.p.parse_data_directories(directories=[1,3,5]);self.im=self.p.get_memory_mapped_image();self.base=self.p.OPTIONAL_HEADER.ImageBase
  self.funcs=sorted((e.struct.BeginAddress,e.struct.EndAddress)for e in self.p.DIRECTORY_ENTRY_EXCEPTION);self.starts=[a for a,b in self.funcs];self.cs=Cs(CS_ARCH_X86,CS_MODE_64);self.cs.detail=True
  self.imports={i.address:i.name.decode()if i.name else str(i.ordinal)for d in self.p.DIRECTORY_ENTRY_IMPORT for i in d.imports}
 def function(self,va):
  r=va-self.base;j=bisect.bisect_right(self.starts,r)-1
  if j<0 or not self.funcs[j][0]<=r<self.funcs[j][1]:raise ValueError('no PE function boundary '+hex(va))
  a,b=self.funcs[j];return list(self.cs.disasm(self.im[a:b],self.base+a))
 def imported(self,va):
  if not isinstance(va,int):return None
  if va in self.imports:return self.imports[va]
  r=va-self.base
  if not 0<=r<len(self.im)-6:return None
  i=next(self.cs.disasm(self.im[r:r+16],va),None)
  if i and i.mnemonic=='jmp' and i.operands[0].type==X86_OP_MEM and self.cs.reg_name(i.operands[0].mem.base)=='rip':return self.imports.get(i.address+i.size+i.operands[0].mem.disp)
 def references(self,target):
  out=set()
  for m in re.finditer(rb'[\x48\x4c]\x8d[\x05\x0d\x15\x1d\x25\x2d\x35\x3d]',self.im):
   n=m.start()
   if self.base+n+7+struct.unpack_from('<i',self.im,n+3)[0]==target:
    try:out.add(self.function(self.base+n)[0].address)
    except ValueError:pass
  return out
 def trace(self,ins):
  regs={'rcx':('arg0',0),'rdx':('arg1',0),'r8':('arg2',0),'r9':('arg3',0),'rsp':('stack',0)};mem={};out=[]
  def address(i,o):
   m=o.mem;name=self.cs.reg_name(m.base)
   if name=='rip':return i.address+i.size+m.disp
   value=regs.get(canon(name))
   if m.index:
    index=regs.get(canon(self.cs.reg_name(m.index)))
    if not isinstance(index,int):return None
    value=add(value,index*m.scale)
   return add(value,m.disp)
  def val(i,o):
   if o.type==X86_OP_IMM:return o.imm
   if o.type==X86_OP_REG:return regs.get(canon(self.cs.reg_name(o.reg)))
   if o.type==X86_OP_MEM:
    addr=address(i,o)
    if addr in self.imports:return addr
    if addr in mem:return mem[addr]
    if isinstance(addr,tuple)and addr[0]=='stack':return ('stack_argument',addr[1])
    if isinstance(addr,int)and self.base<=addr<=self.base+len(self.im)-o.size:return int.from_bytes(self.im[addr-self.base:addr-self.base+o.size],'little')
   return None
  for i in ins:
   op=i.mnemonic;os=i.operands
   rec=dict(ins=i,regs=dict(regs),mem=dict(mem))
   if op=='call':
    rec.update(target=val(i,os[0]),import_name=self.imported(val(i,os[0])))
    for r in('rax','rcx','rdx','r8','r9','r10','r11'):regs.pop(r,None)
    regs['rax']=(f'call_return_{i.address:x}',0)
   elif op=='push':regs['rsp']=add(regs.get('rsp'),-8)
   elif op=='pop':regs.pop(canon(self.cs.reg_name(os[0].reg)),None);regs['rsp']=add(regs.get('rsp'),8)
   elif op in('add','sub')and os[0].type==X86_OP_REG and os[1].type==X86_OP_IMM:
    r=canon(self.cs.reg_name(os[0].reg));regs[r]=add(regs.get(r),os[1].imm*(1 if op=='add'else-1))
   elif op in('xor','pxor')and len(os)==2 and os[0].type==X86_OP_REG and os[1].type==X86_OP_REG and os[0].reg==os[1].reg:regs[canon(self.cs.reg_name(os[0].reg))]=0
   elif op in('lea','mov','movabs')and len(os)==2:
    value=address(i,os[1])if op=='lea'else val(i,os[1])
    if os[0].type==X86_OP_REG:
     r=canon(self.cs.reg_name(os[0].reg))
     if value is None:regs.pop(r,None)
     else:regs[r]=value
    elif os[0].type==X86_OP_MEM:
     addr=address(i,os[0]);rec.update(store_address=addr,store_value=value,store_size=os[0].size)
     if addr is not None:mem[addr]=value
   out.append(rec)
  return out
 def dump(self,name,ins):
  p=HERE/'sources'/name;p.parent.mkdir(exist_ok=True);p.write_text('\n'.join(f'{x.address:#x}: {x.mnemonic} {x.op_str}'for x in ins)+'\n');return str(p.relative_to(HERE))
def anchor_calls(im,trace,owner,base):
 """Parent calls passing count/table stack args to the base storing the max table at+348."""
 out=[]
 for r in trace:
  if r['ins'].mnemonic!='call'or r['regs'].get('rcx')!=owner:continue
  sp=r['regs']['rsp'];candidate_args={off:r['mem'].get(add(sp,off))for off in range(0x20,0xd0,8)}
  candidate_count=candidate_args[0x60];candidate_table=candidate_args[0x88]
  if not(isinstance(candidate_count,int)and 0<candidate_count<=128 and isinstance(candidate_table,int)and base<=candidate_table<=base+len(im.im)-candidate_count*4 and isinstance(r.get('target'),int)):continue
  try:callee_trace=im.trace(im.function(r['target']))
  except ValueError:continue
  max_table_writes=[x for x in callee_trace if x.get('store_address')==('arg0',0x348)and x.get('store_value')==('stack_argument',0x90)]
  if len(max_table_writes)==1:out.append((r,candidate_args))
 return out
def anchor_pointers(im,trace,owner,base):
 """Parent-relative pointer arguments of the unique original anchor call."""
 calls=anchor_calls(im,trace,owner,base)
 if len(calls)!=1:return set()
 return {v[1]-owner[1]for v in calls[0][1].values()if isinstance(v,tuple)and v[0]==owner[0]and v!=owner}
def entry_sdk(im,trace):
 """Unique SDK CHypParamBlock construction of the entry receiver, direct or one base-constructor call deep."""
 found=[(None,r)for r in trace if r.get('import_name')==SDK_CTOR and r['regs'].get('rcx')==('arg0',0)]
 for r in trace:
  if r['ins'].mnemonic!='call'or r.get('import_name')or r['regs'].get('rcx')!=('arg0',0)or not isinstance(r.get('target'),int):continue
  try:callee=im.trace(im.function(r['target']))
  except ValueError:continue
  found+=[(r['target'],x)for x in callee if x.get('import_name')==SDK_CTOR and x['regs'].get('rcx')==('arg0',0)]
 return found[0]if len(found)==1 else None
def leaf_dispatch(im,va):
 """Control-flow walk of an unwind-less leaf that range-checks the parameter
 index (edx) and dispatches through an MSVC image-relative jump table.
 Returns (instructions, external direct tail jumps) or None when any path
 calls, leaves the bounded leaf indirectly, or is not decodable."""
 base=im.base;seen={};exits=[];work=[va];bound=None;switch=False
 def internal(t):
  r=t-base;j=bisect.bisect_right(im.starts,r)-1
  return 0<=t-va<0x200 and not(j>=0 and im.funcs[j][0]<=r<im.funcs[j][1])
 while work:
  pc=work.pop()
  while pc not in seen:
   if len(seen)>256 or not internal(pc):return None
   i=next(im.cs.disasm(im.im[pc-base:pc-base+16],pc),None)
   if i is None or i.mnemonic in('call','int3'):return None
   seen[pc]=i;m=i.mnemonic;nxt=i.address+i.size
   if m=='cmp'and bound is None and i.operands[1].type==X86_OP_IMM:
    prior=[seen[a]for a in sorted(seen)if a<pc]
    reg=i.op_str.split(',')[0]
    if reg=='edx'or(reg=='eax'and prior and prior[-1].mnemonic=='lea'and prior[-1].op_str.startswith('eax, [rdx')):bound=i.operands[1].imm
   if m=='ret':break
   if m=='jmp':
    o=i.operands[0]
    if o.type==X86_OP_IMM:
     if internal(o.imm):pc=o.imm;continue
     exits.append(i);break
    if o.type!=X86_OP_REG or bound is None or switch:return None
    # Resolve the original jump table: r9 holds the image base; optional byte
    # index table at base+D1, dword RVA target table at base+D2.
    body=[seen[a]for a in sorted(seen)];lea=[x for x in body if x.mnemonic=='lea'and x.op_str.startswith('r9, [rip')]
    if len(lea)!=1 or lea[0].address+lea[0].size+lea[0].operands[1].mem.disp!=base:return None
    bytetab=[x for x in body if x.mnemonic=='movzx'and x.operands[1].type==X86_OP_MEM and x.operands[1].size==1 and im.cs.reg_name(x.operands[1].mem.base)=='r9']
    dwtab=[x for x in body if x.mnemonic=='mov'and x.operands[1].type==X86_OP_MEM and x.operands[1].size==4 and im.cs.reg_name(x.operands[1].mem.base)=='r9'and x.operands[1].mem.scale==4]
    if len(dwtab)!=1 or len(bytetab)>1:return None
    indices=list(im.im[bytetab[0].operands[1].mem.disp:bytetab[0].operands[1].mem.disp+bound+1])if bytetab else list(range(bound+1))
    if len(indices)!=bound+1:return None
    d2=dwtab[0].operands[1].mem.disp;switch=True
    work+=[base+struct.unpack_from('<I',im.im,d2+4*k)[0]for k in sorted(set(indices))];break
   if m.startswith('j'):
    if i.operands[0].type!=X86_OP_IMM:return None
    work.append(i.operands[0].imm)
   pc=nxt
 if bound is None or not switch or not exits:return None
 return [seen[a]for a in sorted(seen)],exits
def leaf_end(im,va):
 """End of an unwind-less (no .pdata) leaf at va, or None. The Win64 ABI lets a function omit unwind data only
 if it never calls, pushes or adjusts rsp; decode every path from va under exactly those constraints. Paths end at
 ret, a direct tail jump outside the leaf or a memory-indirect tail jump; a register jump, pdata overlap or
 non-contiguous body refuses."""
 base=im.base;seen={};work=[va]
 def free(t):
  r=t-base;j=bisect.bisect_right(im.starts,r)-1
  return 0<=t-va<0x400 and not(j>=0 and im.funcs[j][0]<=r<im.funcs[j][1])
 while work:
  pc=work.pop()
  while pc not in seen:
   if len(seen)>512 or not free(pc):return None
   i=next(im.cs.disasm(im.im[pc-base:pc-base+16],pc),None)
   if i is None or i.mnemonic in('call','push','pop','int3','enter','leave','syscall')or i.mnemonic.startswith(('push','pop')):return None
   if i.operands and i.operands[0].type==X86_OP_REG and canon(im.cs.reg_name(i.operands[0].reg))=='rsp':return None
   seen[pc]=i;m=i.mnemonic
   if m=='ret':break
   if m.startswith('j'):
    o=i.operands[0]
    # A memory-indirect jmp is a virtual/IAT tail call that ends the path; a register jmp may be a switch table.
    if m=='jmp'and o.type==X86_OP_MEM:break
    if o.type!=X86_OP_IMM:return None
    if m=='jmp':
     if free(o.imm):pc=o.imm;continue
     break
    if free(o.imm):work.append(o.imm)
   pc=i.address+i.size
 end=va
 for a in sorted(seen):
  if a!=end:return None
  end=a+seen[a].size
 return end
def profile_leaf_ends(im,rtti):
 for v in rtti['vtables']:
  for slot in v['slots'].values():
   if slot.get('function_end')is None and not slot.get('import'):
    end=leaf_end(im,int(slot['va'],16))
    if end is not None:slot.update(function_end=hex(end),function_end_source='unwindless_leaf_decode')
def crt_dynamic_initializers(im,profile):
 """Source-attested C++ dynamic initializers (__xc_a..__xc_z) for globals the profile's original code reads.
 Table: the void _initterm call that follows the int-returning _initterm_e (__xi) call in one startup function.
 Reads: rip-relative writable-data operands in functions reachable by direct calls/jumps from profile functions.
 Entry: only the MSVC thunk shape `[lea rdx,K;] lea rcx,G; call CTOR [; lea rcx,DTOR; jmp ATEXIT]` with G read.
 Returns (entries, None) or (None, reason)."""
 base=im.base;starts={base+a for a in im.starts};secs=im.p.sections
 def writable(va):return any(s.VirtualAddress<=va-base<s.VirtualAddress+max(s.Misc_VirtualSize,s.SizeOfRawData)and s.Characteristics&0x80000000 and not s.Characteristics&0x20000000 for s in secs)
 def executable(va):return any(s.VirtualAddress<=va-base<s.VirtualAddress+s.Misc_VirtualSize and s.Characteristics&0x20000000 for s in secs)
 def rip(i,o):return i.address+i.size+o.mem.disp
 calls=[]
 for m in re.finditer(rb'\x48\x8d\x15....\x48\x8d\x0d....\xe8',im.im,re.DOTALL):
  r=m.start();ins=list(im.cs.disasm(im.im[r:r+19],base+r))
  if len(ins)!=3:continue
  z,a=rip(ins[0],ins[0].operands[1]),rip(ins[1],ins[1].operands[1])
  if not base<a<z<base+len(im.im)or(z-a)%8 or(z-a)//8>4096:continue
  try:loop=[(i.mnemonic,i.op_str)for i in im.function(ins[2].operands[0].imm)];caller=im.function(base+r)[0].address
  except ValueError:continue
  if all(x in loop for x in[('cmp','rcx, rdx'),('mov','rax, qword ptr [rbx]'),('test','rax, rax'),('add','rbx, 8'),('cmp','rbx, rdi')]):calls.append(dict(site=base+r,start=a,end=z,initterm=ins[2].operands[0].imm,returns_int=('test','eax, eax')in loop,caller=caller))
 xc=[c for c in calls if not c['returns_int']and any(e['returns_int']and e['caller']==c['caller']and e['site']<c['site']for e in calls)]
 if len(xc)!=1:return None,'C++ initializer table not unique count='+str(len(xc))
 xc=xc[0];roots=set()
 def walk(x):
  if isinstance(x,dict):[walk(v)for v in x.values()]
  elif isinstance(x,list):[walk(v)for v in x]
  elif isinstance(x,str)and re.fullmatch(r'0x[0-9a-f]+',x)and int(x,16)in starts:roots.add(int(x,16))
 walk(profile);seen=set(roots);work=list(roots);reads=set()
 while work:
  for i in im.function(work.pop()):
   for o in i.operands:
    if o.type==X86_OP_MEM and im.cs.reg_name(o.mem.base)=='rip'and writable(rip(i,o)):reads.add(rip(i,o))
   if i.mnemonic in('call','jmp')and i.operands[0].type==X86_OP_IMM and i.operands[0].imm in starts and i.operands[0].imm not in seen:seen.add(i.operands[0].imm);work.append(i.operands[0].imm)
 table=im.im[xc['start']-base:xc['end']-base];out=[]
 for k,va in enumerate(struct.unpack('<'+'Q'*(len(table)//8),table)):
  if not va or not executable(va):continue
  body=[]
  for i in im.cs.disasm(im.im[va-base:va-base+0x40],va):
   body.append(i)
   if i.mnemonic in('jmp','ret','int3'):break
  s=[x for x in body if not(x.mnemonic in('sub','add')and x.op_str.startswith('rsp, 0x28'))]
  if s and s[0].mnemonic=='lea'and s[0].op_str.startswith('rdx, [rip'):konst=rip(s[0],s[0].operands[1]);s=s[1:]
  else:konst=None
  if len(s)not in(2,4)or s[0].mnemonic!='lea'or not s[0].op_str.startswith('rcx, [rip')or s[1].mnemonic!=('call'if len(s)==4 else'jmp')or s[1].operands[0].type!=X86_OP_IMM:continue
  g=rip(s[0],s[0].operands[1])
  if g not in reads or not writable(g):continue
  entry=dict(ordinal=k,table_slot=hex(xc['start']+8*k),va=hex(va),function_end=hex(body[-1].address+body[-1].size),constructs=hex(g),constructor=hex(s[1].operands[0].imm),constant_argument=hex(konst)if konst else None,destructor_registration=None)
  if len(s)==4:
   if not(s[2].mnemonic=='lea'and s[2].op_str.startswith('rcx, [rip')and executable(rip(s[2],s[2].operands[1]))and s[3].mnemonic=='jmp'and s[3].operands[0].type==X86_OP_IMM):continue
   entry['destructor_registration']=dict(destructor=hex(rip(s[2],s[2].operands[1])),registrar=hex(s[3].operands[0].imm))
  out.append(entry)
 return dict(table_call=hex(xc['site']),initterm=hex(xc['initterm']),startup_function=hex(xc['caller']),table_start=hex(xc['start']),table_end=hex(xc['end']),table_sha256=hashlib.sha256(table).hexdigest(),reachable_functions=len(seen),entries=out,order='original table order; each entry runs once after CRT startup',execution_status='source_proven_not_executed'),None
def allocation_size(trace,obj):
 # obj is the return of exactly one call in trace made with an immediate size in ecx (the original allocation).
 if not(isinstance(obj,tuple)and obj[1]==0 and obj[0].startswith('call_return_')):return None
 calls=[r for r in trace if r['ins'].mnemonic=='call'and r['ins'].address==int(obj[0][12:],16)]
 size=calls[0]['regs'].get('rcx')if len(calls)==1 else None
 return size if isinstance(size,int)and 0x10000<size<=0x4000000 else None
def parent_span(im,trace,owner):
 """Bound for a parent-embedded PB offset: the original allocation size of the parent object, when the parent
 constructor is its own factory or has exactly one direct caller that passes it one sized allocation as `this`;
 otherwise the conservative 0x10000 embedded bound."""
 size=allocation_size(trace,owner)
 if size is None and owner==('arg0',0):
  ctor=trace[0]['ins'].address;cache=im.__dict__.setdefault('_parent_span',{})
  if ctor in cache:return cache[ctor]
  sites=[]
  for m in re.finditer(rb'\xe8',im.im):
   r=m.start()
   if r+5<=len(im.im)and im.base+r+5+struct.unpack_from('<i',im.im,r+1)[0]==ctor:sites.append(im.base+r)
  if len(sites)==1:
   try:caller=im.trace(im.function(sites[0]))
   except ValueError:caller=[]
   at=[r for r in caller if r['ins'].address==sites[0]]
   if len(at)==1:size=allocation_size(caller,at[0]['regs'].get('rcx'))
  cache[ctor]=size or 0x10000
 return size or 0x10000
def generate(record,rtti):
 path=ROOT/(record['name']+'.dll');im=Image(path);base=im.base;profile_leaf_ends(im,rtti)
 if hashlib.sha256(im.raw).hexdigest()!=record['dll_sha256']:raise ValueError('frozen DLL hash changed')
 parents=[v for v in rtti['vtables']if v['type']=='.?AVPlugIn@@'and v['object_offset']==0]
 if len(parents)!=1:raise ValueError('missing_or_ambiguous_primary_PlugIn_RTTI count='+str(len(parents)))
 parent=parents[0];pvt=int(parent['vtable'],16);parentrefs=im.references(pvt)
 blocks=[]
 for v in rtti['vtables']:
  if v['type']in('.?AVPlugIn@@','.?AVCHypParamBlock@@')or v['object_offset']!=0 or '0x60'not in v['slots']:continue
  for va in im.references(int(v['vtable'],16))&parentrefs:
   trace=im.trace(im.function(va));sdk=[r for r in trace if r.get('import_name')==SDK_CTOR]
   if len(sdk)!=1:continue
   blocks.append((v,va,trace,sdk[0]))
 constructor_kind='inline_span';pb_ctor=None;pb_trace=None;pb_sdk=None;pb_sdk_function=None;pb_selection=None
 if len(blocks)==1:
  block,ctor,trace,sdk=blocks[0];bvt=int(block['vtable'],16)
  owners=[r['store_address']for r in trace if r.get('store_value')==pvt];assert len(owners)==1;owner=owners[0]
  receiver=sdk['regs'].get('rcx');assert isinstance(owner,tuple)and isinstance(receiver,tuple)and owner[0]==receiver[0]
  embedded=receiver[1]-owner[1];assert 0<embedded<parent_span(im,trace,owner)-8
  sdkidx=trace.index(sdk)
  finalizers=[r for r in trace[sdkidx+1:]if r['ins'].mnemonic=='call'and r['regs'].get('rcx')==owner]
  if len(finalizers)!=1:raise ValueError('parent-finalizer boundary not unique')
  finish=finalizers[0]
 else:
  # Newer JUCE-family products keep the genuine CHyp construction in a
  # standalone PB constructor. Select it only when the original parent calls
  # exactly one candidate with a source-derived parent-relative receiver.
  # A derived PB constructor may reach that SDK construction of its entry
  # receiver through exactly one base-PB constructor call on the same receiver.
  standalone=[];pbvts=[v for v in rtti['vtables']if not(v['type']in('.?AVPlugIn@@','.?AVCHypParamBlock@@')or v['object_offset']!=0 or '0x60'not in v['slots'])]
  parent_traces={}
  for parent_ctor in parentrefs:
   parent_trace=im.trace(im.function(parent_ctor));owners=[r['store_address']for r in parent_trace if r.get('store_value')==pvt]
   if len(owners)==1 and isinstance(owners[0],tuple):parent_traces[parent_ctor]=(parent_trace,owners[0])
  for v in pbvts:
   for candidate_va in im.references(int(v['vtable'],16)):
    candidate_trace=im.trace(im.function(candidate_va));found=entry_sdk(im,candidate_trace)
    if found is None:continue
    sdk_function,candidate_sdk=found
    writes=[r for r in candidate_trace if r.get('store_value')==int(v['vtable'],16)and r.get('store_address')==('arg0',0)]
    if len(writes)!=1:continue
    for parent_ctor,(parent_trace,owner) in parent_traces.items():
     for link in parent_trace:
      if link.get('target')!=candidate_va:continue
      receiver=link['regs'].get('rcx')
      if not(isinstance(receiver,tuple)and owner[0]==receiver[0]):continue
      embedded=receiver[1]-owner[1]
      if 0<embedded<parent_span(im,parent_trace,owner)-8:standalone.append(('standalone_pb_constructor',v,parent_ctor,parent_trace,candidate_va,candidate_trace,candidate_sdk,owner,embedded,sdk_function))
  if len(standalone)!=1:
   # Several CHyp blocks (or several linked PB constructors): the genuine PB is
   # the one receiver the original parent passes as a pointer argument to its
   # unique max-table base constructor (the later parent PB binding member).
   inline=[]
   for parent_ctor,(parent_trace,owner) in parent_traces.items():
    for idx,r in enumerate(parent_trace):
     receiver=r['regs'].get('rcx')
     if r.get('import_name')!=SDK_CTOR or not(isinstance(receiver,tuple)and receiver[0]==owner[0]and receiver[1]>owner[1]):continue
     for v in pbvts:
      if len([x for x in parent_trace[idx+1:]if x.get('store_value')==int(v['vtable'],16)and x.get('store_address')==receiver])==1:inline.append(('inline_span',v,parent_ctor,parent_trace,None,None,r,owner,receiver[1]-owner[1],None))
   linked=[]
   for c in standalone+inline:
    pointers=anchor_pointers(im,c[3],c[7],base)
    if len(pointers)==1 and c[8]in pointers:linked.append(c)
   if len(linked)!=1:raise ValueError('ambiguous primary constructor/PB '+str(len(blocks))+' standalone='+str(len(standalone))+' inline='+str(len(inline))+' anchor_linked='+str(len(linked)))
   if not 0<linked[0][8]<parent_span(im,linked[0][3],linked[0][7])-8:raise ValueError('anchor-designated PB offset outside bounded parent range '+str(linked[0][8])+' span='+str(parent_span(im,linked[0][3],linked[0][7])))
   standalone=linked;pb_selection='parent_anchor_pointer_argument'
  constructor_kind,block,ctor,trace,pb_ctor,pb_trace,sdk,owner,embedded,pb_sdk_function=standalone[0];bvt=int(block['vtable'],16)
  if constructor_kind=='inline_span':
   # The original span starts at the parent's first CHyp construction, as in
   # the single-block contract, so sibling blocks are constructed genuinely.
   pb_sdk=sdk;sdk=next(r for r in trace if r.get('import_name')==SDK_CTOR);sdkidx=trace.index(sdk)
   finalizers=[r for r in trace[sdkidx+1:]if r['ins'].mnemonic=='call'and r['regs'].get('rcx')==owner]
   if len(finalizers)!=1:raise ValueError('parent-finalizer boundary not unique')
   finish=finalizers[0]
 if constructor_kind=='inline_span':
  def reloc(value):
   if isinstance(value,int):return dict(value=value)
   if isinstance(value,tuple)and value[0]==owner[0]:return dict(base='parent',offset=value[1]-owner[1])
   if isinstance(value,tuple)and value[0]=='stack':return dict(base='entry_stack',offset=value[1]-sdk['regs']['rsp'][1])
   return None
  registers={r:reloc(sdk['regs'].get(r))for r in(*NONVOL,'rcx')};registers={r:v for r,v in registers.items()if v is not None};assert registers['rcx']==dict(base='parent',offset=sdk['regs']['rcx'][1]-owner[1])and(pb_sdk is not None or registers['rcx']['offset']==embedded)
  stackwrites=[]
  for key,value in sdk['mem'].items():
   if isinstance(key,tuple)and key[0]=='stack'and sdk['regs']['rsp'][1]<=key[1]<=sdk['regs']['rsp'][1]+0x180 and reloc(value)is not None:
    stackwrites.append(dict(offset=key[1]-sdk['regs']['rsp'][1],size=8,value=reloc(value)))
 else:
  registers={};stackwrites=[]
 basecalls=anchor_calls(im,trace[:sdkidx]if constructor_kind=='inline_span'else trace,owner,base)
 if len(basecalls)!=1:raise ValueError('parent constructor anchor not unique '+str(len(basecalls)))
 bc,args=basecalls[0]
 count=args[0x60];table=args[0x88];assert isinstance(count,int)and 0<count<=128 and isinstance(table,int)
 raw=struct.unpack_from('<'+str(count)+'I',im.im,table-base);assert all(0<=n<=1024 for n in raw)
 capture_path=Path(record['captured_metadata']['path']);capraw=capture_path.read_bytes();assert hashlib.sha256(capraw).hexdigest()==record['captured_metadata']['sha256'];captured=json.loads(capraw);indices=sorted({int(x['path'].split('/parameters/')[1].split('/')[0])for x in captured['controls']if'/parameters/'in x['path']});assert indices==list(range(count))
 prefix=record['name'].replace(' ','_');sources=[im.dump(prefix+'-constructor.asm',[r['ins']for r in trace]),im.dump(prefix+'-parent-base-constructor.asm',im.function(bc['target']))]
 if pb_trace is not None:sources.append(im.dump(prefix+'-parameter-block-constructor.asm',[r['ins']for r in pb_trace]))
 # The proven framework normalizer is the first parent vtable method that loads
 # its raw-max table from+348 and references both0.5 and32768 double constants.
 def framework_normalizer(ins):
  if not any(o.type==X86_OP_MEM and o.mem.disp==0x348 for i in ins for o in i.operands):return False
  constants=[]
  for i in ins:
   for o in i.operands:
    if o.type==X86_OP_MEM and im.cs.reg_name(o.mem.base)=='rip'and o.size==8:
     addr=i.address+i.size+o.mem.disp
     if base<=addr<=base+len(im.im)-8:constants.append(struct.unpack_from('<d',im.im,addr-base)[0])
  return .5 in constants and 32768. in constants
 normalizers=[];normalizer_source={}
 for off,slot in parent['slots'].items():
  va=int(slot['va'],16)
  try:ins=im.function(va)
  except ValueError:
   # A product override may be an unwind-less leaf index dispatch whose only
   # external direct tail jump is the genuine framework normalizer; the slot
   # itself is then the original normalization boundary.
   found=leaf_dispatch(im,va)
   if found is None:continue
   leaf,exits=found
   if len({i.operands[0].imm for i in exits})!=1:continue
   try:target=im.function(exits[0].operands[0].imm)
   except ValueError:continue
   if target[0].address==exits[0].operands[0].imm and framework_normalizer(target):
    normalizers.append(dict(slot=off,va=slot['va'],kind='leaf_index_dispatch',framework_normalizer=hex(target[0].address),framework_tail_jumps=[hex(i.address)for i in exits]));normalizer_source[slot['va']]=leaf
   continue
  if framework_normalizer(ins):normalizers.append(dict(slot=off,va=slot['va']));normalizer_source[slot['va']]=ins
 if len(normalizers)!=1:raise ValueError('genuine parent normalization boundary not unique count='+str(len(normalizers)))
 sources.append(im.dump(prefix+'-normalizer.asm',normalizer_source[normalizers[0]['va']]))
 # Confirm the parent base constructor copies the supplied max table pointer to+348.
 basetrace=im.trace(im.function(bc['target']));writes=[r for r in basetrace if isinstance(r.get('store_address'),tuple)and r['store_address']==('arg0',0x348)]
 source348=[dict(va=hex(r['ins'].address),instruction=r['ins'].op_str)for r in writes]
 if len(writes)!=1:raise ValueError('max table parent member store not unique')
 if writes[0]['store_value']!=('stack_argument',0x90):raise ValueError('max table does not flow from original caller stack+88')
 descriptor_boundary=block['slots'].get('0x60')
 secondary=[v for v in rtti['vtables']if v['type']=='.?AVPlugIn@@'and v['object_offset']>0 and'0x18'in v['slots']]
 if len(secondary)!=1:raise ValueError('secondary descriptor interface not unique')
 secondary=secondary[0];wrapper=int(secondary['slots']['0x18']['va'],16)
 leaf=[]
 for instruction in im.cs.disasm(im.im[wrapper-base:wrapper-base+64],wrapper):
  leaf.append(instruction)
  if instruction.mnemonic in('ret','jmp'):break
 if len(leaf)==2 and leaf[0].mnemonic=='add' and leaf[0].op_str.split(',')[0]=='rcx' and leaf[1].mnemonic=='jmp' and leaf[1].operands[0].type==X86_OP_IMM:
  adjust=leaf[0].operands[1].imm;target=leaf[1].operands[0].imm
  sources.append(im.dump(prefix+'-descriptor-adjustor.asm',leaf));sources.append(im.dump(prefix+'-descriptor-producer.asm',im.function(target)))
  descriptor=dict(status='original_adjustor_discovered',interface_slot='0x18',secondary_vtable=secondary['vtable'],secondary_object_offset=secondary['object_offset'],getter_va=hex(wrapper),receiver='parent_secondary',receiver_from_parameter_block=secondary['object_offset']-embedded,original_adjustment=adjust,target_va=hex(target),target_receiver_from_parameter_block=secondary['object_offset']+adjust-embedded)
 elif len(leaf)==3 and leaf[1].mnemonic=='mov' and leaf[1].op_str=='rax, qword ptr [rcx]' and leaf[2].mnemonic=='jmp' and leaf[2].operands[0].type==X86_OP_MEM and im.cs.reg_name(leaf[2].operands[0].mem.base)=='rax' and not leaf[2].operands[0].mem.index:
  first=leaf[0];slot=leaf[2].operands[0].mem.disp
  if slot<0 or slot>0x600 or slot%8:raise ValueError('descriptor virtual slot outside bounded vtable')
  if first.mnemonic=='add' and first.operands[0].type==X86_OP_REG and im.cs.reg_name(first.operands[0].reg)=='rcx' and first.operands[1].type==X86_OP_IMM:
   kind='inline';offset=first.operands[1].imm
  elif first.mnemonic=='mov' and first.operands[0].type==X86_OP_REG and im.cs.reg_name(first.operands[0].reg)=='rcx' and first.operands[1].type==X86_OP_MEM and im.cs.reg_name(first.operands[1].mem.base)=='rcx' and not first.operands[1].mem.index:
   kind='member_pointer';offset=first.operands[1].mem.disp
  else:raise ValueError('unsupported virtual descriptor receiver transform')
  if not 0<=offset<=0x10000:raise ValueError('descriptor receiver adjustment outside bound')
  sources.append(im.dump(prefix+'-descriptor-virtual-dispatch.asm',leaf))
  descriptor=dict(status='original_secondary_virtual_dispatch_discovered',interface_slot='0x18',secondary_vtable=secondary['vtable'],secondary_object_offset=secondary['object_offset'],getter_va=hex(wrapper),receiver='parent_secondary',receiver_from_parameter_block=secondary['object_offset']-embedded,dispatch=dict(receiver_kind=kind,receiver_offset=offset,virtual_slot=hex(slot)),qualification='Exact original wrapper; target vtable/pointer remains original constructor-owned state and must pass bounded runtime descriptor validation')
 else:
  body=im.function(wrapper);bodytrace=im.trace(body)
  sizes=[r for r in bodytrace if r.get('store_address')==('arg1',0)and r.get('store_value')==0xaf8]
  if len(sizes)!=1:raise ValueError('secondary descriptor body lacks unique original output-size store')
  sources.append(im.dump(prefix+'-descriptor-producer.asm',body))
  descriptor=dict(status='original_secondary_body_discovered',interface_slot='0x18',secondary_vtable=secondary['vtable'],secondary_object_offset=secondary['object_offset'],getter_va=hex(wrapper),receiver='parent_secondary',receiver_from_parameter_block=secondary['object_offset']-embedded,size_store=hex(sizes[0]['ins'].address))

 constructor=dict(kind='inline_span',va=hex(ctor),receiver='parent',embedded_offset=embedded,entry=hex(sdk['ins'].address),stop=hex(finish['ins'].address),registers=registers,stack_writes=stackwrites,sdk_import=SDK_CTOR) if constructor_kind=='inline_span' else dict(kind='standalone_pb_constructor',va=hex(pb_ctor),parent_constructor_va=hex(ctor),receiver='parameter_block',embedded_offset=embedded,sdk_import=SDK_CTOR)
 if pb_sdk is not None:constructor.update(parameter_block_sdk_call=hex(pb_sdk['ins'].address))
 if pb_sdk_function is not None:constructor.update(base_pb_constructor_va=hex(pb_sdk_function),sdk_call=hex(sdk['ins'].address))
 if pb_selection:constructor.update(selection=pb_selection)
 result=dict(schema='openapollo-discovered-control-profile/1',status='discovered',name=record['name'],dll=dict(path=str(path),sha256=record['dll_sha256']),captured_metadata=record['captured_metadata'],parameter_block=dict(type=block['type'],vtable=block['vtable'],constructor=constructor,slots=block['slots']),parameters=dict(count=count,raw_maxima=raw,effective_maxima=[n or 32768 for n in raw],table_va=hex(table),parent_callsite=hex(bc['ins'].address),count_stack_offset=0x60,table_stack_offset=0x88,source_member_store=source348),parent=dict(vtable=parent['vtable'],constructor_base=hex(bc['target']),normalizer=normalizers[0],set_parameter=parent['slots'].get('0x50'),slots=parent['slots']),descriptor=descriptor,startup=dict(program_load_status='not_yet_discovered',crt_initializers='unknown_until_original_calls_observed'),source_files=sources,manual_plugin_overrides=0)
 result['crt_dynamic_initializers'],why=crt_dynamic_initializers(im,result)
 if why:result['crt_dynamic_initializers']=dict(status='unresolved',reason=why)
 return result
def main():
 started=time.monotonic();cohort=json.loads((HERE/'cohort.json').read_text());rtti={x['name']:x for x in json.loads((HERE/'rtti.json').read_text())};profiles=[]
 for record in cohort['cohort']:
  try:result=generate(record,rtti[record['name']])
  except Exception as e:
   import traceback
   result=dict(name=record['name'],status='discovery_failed',error=str(e),traceback=traceback.format_exc(),dll_sha256=record['dll_sha256'])
  profiles.append(result);print(result['name'],result['status'],result.get('error',result.get('parameters',{}).get('count')),flush=True)
 (HERE/'profiles.json').write_text(json.dumps(dict(schema='openapollo-batch-discovery/1',profiles=profiles,counts=dict(selected=len(profiles),discovered=sum(p['status']=='discovered'for p in profiles),manual_plugin_overrides=0),seconds=time.monotonic()-started),indent=2)+'\n')
if __name__=='__main__':main()
