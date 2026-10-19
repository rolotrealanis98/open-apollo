"""Bounded original parent-method graph search; no image-specific addresses."""
from generate_profiles import *
def program_candidate(im,va,count):
 try:ins=im.function(va)
 except ValueError:return None
 if len(ins)>1200:return None
 trace=im.trace(ins);version=[];setter=[];vector=[];bound=[]
 for r in trace:
  i=r['ins'];os=i.operands
  if i.mnemonic=='cmp'and len(os)==2 and os[0].type==X86_OP_MEM and os[1].type==X86_OP_IMM and 0<os[1].imm<=16:
   m=os[0].mem;owner=r['regs'].get(canon(im.cs.reg_name(m.base)))
   if owner==('arg1',0)and m.disp==0:version.append((i.address,os[1].imm))
  if i.mnemonic=='call'and os[0].type==X86_OP_MEM and os[0].mem.disp==0x50:setter.append(i.address)
  if i.mnemonic=='movss'and len(os)==2 and os[1].type==X86_OP_MEM:
   m=os[1].mem;owner=r['regs'].get(canon(im.cs.reg_name(m.base)))
   if isinstance(owner,tuple)and owner[0]=='arg1':vector.append((i.address,owner[1]+m.disp,m.scale))
  if i.mnemonic=='cmp'and len(os)==2 and os[1].type==X86_OP_IMM and os[1].imm==count:bound.append(i.address)
 if version and setter and vector and bound:
  offsets={x[1]for x in vector};versions={x[1]for x in version}
  if len(offsets)!=1 or len(versions)!=1:return None
  return dict(va=hex(ins[0].address),version_check=[dict(va=hex(a),version=b)for a,b in version],setter_calls=list(map(hex,setter)),vector_loads=[dict(va=hex(a),offset=b,scale=c)for a,b,c in vector],count_checks=list(map(hex,bound)),program_version=versions.pop(),value_offset=offsets.pop(),parameter_count=count)
def tagged_layout_candidate(im,va,count):
 """Recover an original loader when linear tracing crosses its reject return.

 The loader itself must still prove its version gate, parent setter, complete
 parameter bound, and a float vector rooted at the original program argument.
 The resulting layout is intentionally verified later by tagged execution;
 no inferred offset becomes a fallback write contract.
 """
 try:ins=im.function(va)
 except ValueError:return None
 if len(ins)>1200:return None
 aliases={'rdx':0};version=[];setter=[];vector=[];bound=[]
 for i in ins:
  os=i.operands
  if i.mnemonic in ('mov','movabs','lea') and len(os)==2 and os[0].type==X86_OP_REG:
   dst=canon(im.cs.reg_name(os[0].reg));value=None
   if os[1].type==X86_OP_REG:value=aliases.get(canon(im.cs.reg_name(os[1].reg)))
   elif i.mnemonic=='lea' and os[1].type==X86_OP_MEM:
    m=os[1].mem;base=aliases.get(canon(im.cs.reg_name(m.base)));value=None if base is None else base+m.disp
   if value is not None:aliases[dst]=value
  elif i.mnemonic in ('add','sub') and len(os)==2 and os[0].type==X86_OP_REG and os[1].type==X86_OP_IMM:
   reg=canon(im.cs.reg_name(os[0].reg))
   if reg in aliases:aliases[reg]+=os[1].imm*(1 if i.mnemonic=='add'else-1)
  if i.mnemonic=='cmp'and len(os)==2 and os[1].type==X86_OP_IMM:
   if os[0].type==X86_OP_MEM:
    m=os[0].mem;base=aliases.get(canon(im.cs.reg_name(m.base)))
    if base is not None and base+m.disp==0 and 0<os[1].imm<=16:version.append((i.address,os[1].imm))
   if os[1].imm==count:bound.append(i.address)
  if i.mnemonic=='call'and os[0].type==X86_OP_MEM and os[0].mem.disp==0x50:setter.append(i.address)
  if i.mnemonic=='movss'and len(os)==2 and os[1].type==X86_OP_MEM:
   m=os[1].mem;base=aliases.get(canon(im.cs.reg_name(m.base)))
   if base is not None:vector.append((i.address,base+m.disp,m.scale))
 versions={x[1]for x in version}
 if not(version and setter and vector and bound and len(versions)==1):return None
 return dict(va=hex(ins[0].address),version_check=[dict(va=hex(a),version=b)for a,b in version],setter_calls=list(map(hex,setter)),vector_loads=[dict(va=hex(a),offset=b,scale=c)for a,b,c in vector],count_checks=list(map(hex,bound)),program_version=versions.pop(),parameter_count=count,layout_contract='tagged_dynamic_required')

def unwind_roots(im):
 """Map every PE function entry to its primary entry via UNW_FLAG_CHAININFO.

 MSVC splits shrink-wrapped functions into chained unwind fragments; the
 version gate and the setter loop of one original loader can sit in different
 .pdata entries. Returns {root_rva:[fragment (begin,end) ...]} in address order.
 """
 entries={e.struct.BeginAddress:e for e in im.p.DIRECTORY_ENTRY_EXCEPTION};parent={}
 for begin,e in entries.items():
  u=getattr(e,'unwindinfo',None)
  if u is not None and u.Flags&4 and isinstance(getattr(u,'FunctionEntry',None),int)and u.FunctionEntry in entries:parent[begin]=u.FunctionEntry
 roots={}
 for begin,e in entries.items():
  root=begin;hops=0
  while root in parent and hops<16:root=parent[root];hops+=1
  roots.setdefault(root,[]).append((begin,e.struct.EndAddress))
 return {r:sorted(f)for r,f in roots.items()}
def loader_contract_candidate(im,fragments):
 """Original loadProgram identity without a static parameter-count bound.

 Requires, over the whole original function (primary + chained fragments):
 a version gate on program word 0 (arg1+0, 1..16) before any setter call,
 a 0/1 boolean result, the parent virtual setter (+0x50) called with the
 original receiver (arg0) in rcx, and a float read rooted at the program
 argument. Index coverage is NOT inferred statically: these loaders use
 index tables, skip masks or member-sourced tails, so completeness is left to
 the existing tagged_dynamic_required executor gate, which rejects any
 parameter the original loader does not set.
 """
 ins=[i for b,e in fragments for i in im.cs.disasm(im.im[b:e],im.base+b)]
 if not ins or len(ins)>1200:return None
 alias={'rcx':('this',0),'rdx':('prog',0)};version=[];setter=[];vector=[];zero=one=False
 def reg(o):return canon(im.cs.reg_name(o.reg))
 def addr(m):
  if not m.base or im.cs.reg_name(m.base)=='rip':return None
  b=alias.get(canon(im.cs.reg_name(m.base)));x=alias.get(canon(im.cs.reg_name(m.index)))if m.index else('abs',0)
  if b is None or x is None:return None
  if b[0]=='abs'and x[0]!='abs':b,x=x,b
  if x[0]!='abs'or(m.index and m.scale!=1):return None
  return(b[0],b[1]+x[1]+m.disp)
 branch={};dead=False
 for i in ins:
  os=i.operands;op=i.mnemonic
  # Linear order crosses early returns; resume at a forward branch target
  # with the register state recorded at that branch.
  if dead and i.address in branch:alias=dict(branch[i.address])
  dead=False
  if op.startswith('j')and os and os[0].type==X86_OP_IMM and os[0].imm>i.address:branch.setdefault(os[0].imm,dict(alias))
  if op in('ret','jmp'):dead=True;continue
  if op=='cmp'and len(os)==2 and os[0].type==X86_OP_MEM and os[1].type==X86_OP_IMM and addr(os[0].mem)==('prog',0)and 0<os[1].imm<=16 and not setter:version.append((i.address,os[1].imm))
  if op=='call':
   if os[0].type==X86_OP_MEM and os[0].mem.disp==0x50 and alias.get('rcx',(None,))[0]=='this':setter.append(i.address)
   for r in('rax','rcx','rdx','r8','r9','r10','r11'):alias.pop(r,None)
   continue
  if op.startswith('movs')and len(os)==2 and os[0].type==X86_OP_REG and im.cs.reg_name(os[0].reg).startswith('xmm')and os[1].type==X86_OP_MEM:
   m=os[1].mem;a=addr(m)
   # A program-rooted base with a runtime index (e.g. an index permuted by
   # an original virtual call) is still a program float; offset left to
   # tagged execution.
   if a is None and m.base and m.index and alias.get(canon(im.cs.reg_name(m.base)),(None,))[0]=='prog'and canon(im.cs.reg_name(m.index))not in alias:a=('prog',None)
   if a and a[0]=='prog':vector.append((i.address,a[1]))
  if op=='xor'and len(os)==2 and os[0].type==os[1].type==X86_OP_REG and os[0].reg==os[1].reg and reg(os[0])=='rax':zero=True
  if op=='mov'and len(os)==2 and os[0].type==X86_OP_REG and reg(os[0])=='rax'and os[1].type==X86_OP_IMM and os[1].imm==1:one=True
  if not(len(os)==2 and os[0].type==X86_OP_REG):continue
  dst=reg(os[0]);value=None
  if op in('mov','movabs')and os[1].type==X86_OP_REG:value=alias.get(reg(os[1]))
  elif op=='lea':
   m=os[1].mem
   if m.base and im.cs.reg_name(m.base)=='rip':value=('abs',i.address+i.size+m.disp)
   else:value=addr(m)
  elif op in('add','sub')and dst in alias:
   s=1 if op=='add'else-1;src=os[1].imm if os[1].type==X86_OP_IMM else(alias.get(reg(os[1]))if os[1].type==X86_OP_REG else None)
   if isinstance(src,int):value=(alias[dst][0],alias[dst][1]+s*src)
   elif src and src[0]=='abs':value=(alias[dst][0],alias[dst][1]+s*src[1])
  if value is None:alias.pop(dst,None)
  else:alias[dst]=value
 versions={v for a,v in version}
 if not(version and setter and vector and zero and one and len(versions)==1):return None
 return dict(va=hex(im.base+fragments[0][0]),fragments=[[hex(im.base+b),hex(im.base+e)]for b,e in fragments],version_check=[dict(va=hex(a),version=v)for a,v in version],setter_calls=list(map(hex,setter)),vector_loads=[dict(va=hex(a),offset=o)for a,o in vector],count_checks=[],program_version=versions.pop(),layout_contract='tagged_dynamic_required',parameter_bound='deferred_to_tagged_dynamic')

def record_copy_candidate(im,x):
 """Framework chunk ABI whose loader copies a whole program record into object state instead of calling
 the 0x50 setter. Parent slot 0x500 getChunk(this, void **out, isPreset) stores, for a preset, the address of
 the current program record: this+base+[this+index]*stride. Slot 0x508 gates the record version
 (cmp dword [record], 1). Slot 0x510 setChunk(this, data, size, isPreset) consumes records of the same
 stride. No record bytes are inferred: the executor obtains them from the original getChunk."""
 slots=x['parent']['slots']
 if not all(isinstance(slots.get(k),dict)for k in('0x500','0x508','0x510','0x58')):return None
 def body(k):
  va=int(slots[k]['va'],16)
  try:return im.function(va)
  except ValueError:
   e=leaf_end(im,va);return None if e is None else list(im.cs.disasm(im.im[va-im.base:e-im.base],va))
 get,gate,put=body('0x500'),body('0x508'),body('0x510')
 if not(get and gate and put):return None
 def rcx_disp(i,mn):
  o=i.operands
  return o[1].mem.disp if i.mnemonic==mn and len(o)==2 and o[1].type==X86_OP_MEM and not o[1].mem.index and im.cs.reg_name(o[1].mem.base)=='rcx'else None
 index=[rcx_disp(i,'movsxd')for i in get if rcx_disp(i,'movsxd')is not None]
 base=[rcx_disp(i,'lea')for i in get if rcx_disp(i,'lea')is not None]
 stride=[i.operands[2].imm for i in get if i.mnemonic=='imul'and len(i.operands)==3 and i.operands[2].type==X86_OP_IMM]
 out=[i for i in get if i.mnemonic=='mov'and i.operands[0].type==X86_OP_MEM and im.cs.reg_name(i.operands[0].mem.base)=='rdx'and i.operands[0].mem.disp==0 and i.operands[0].size==8]
 if len(index)!=1 or len(stride)!=1 or len(base)<1 or len(out)<1 or not 0<stride[0]<=0x10000:return None
 # the preset record base is the lea that the imul product is added to (the one in the same branch as the imul)
 at=[k for k,i in enumerate(get)if i.mnemonic=='imul'][0];preset=[rcx_disp(i,'lea')for i in get[at:]if rcx_disp(i,'lea')is not None]
 if not preset:return None
 version=[i.operands[1].imm for i in gate if i.mnemonic=='cmp'and len(i.operands)==2 and i.operands[1].type==X86_OP_IMM and(i.operands[0].type==X86_OP_REG or(i.operands[0].type==X86_OP_MEM and im.cs.reg_name(i.operands[0].mem.base)=='rdx'and i.operands[0].mem.disp==0))]
 if 1 not in version:return None
 if not any(i.mnemonic=='imul'and len(i.operands)==3 and i.operands[2].type==X86_OP_IMM and i.operands[2].imm==stride[0]for i in put):return None
 # parent slots setChunk dispatches through the receiver's own vtable (e.g. a tail jump to the record handler)
 this={'rcx'};vt=set();dispatched=set()
 for i in put:
  o=i.operands
  if i.mnemonic in('call','jmp')and o and o[0].type==X86_OP_MEM and im.cs.reg_name(o[0].mem.base)in vt and not o[0].mem.index:dispatched.add(o[0].mem.disp);continue
  if i.mnemonic=='call':this&={'rbx','rsi','rdi','rbp','r12','r13','r14','r15'};vt.clear();continue
  if len(o)==2 and o[0].type==X86_OP_REG:
   d=canon(im.cs.reg_name(o[0].reg));this.discard(d);vt.discard(d)
   if i.mnemonic=='mov'and o[1].type==X86_OP_REG and canon(im.cs.reg_name(o[1].reg))in this:this.add(d)
   elif i.mnemonic=='mov'and o[1].type==X86_OP_MEM and o[1].mem.disp==0 and not o[1].mem.index and canon(im.cs.reg_name(o[1].mem.base))in this:vt.add(d)
 if any(not isinstance(slots.get(hex(k)),dict)or not slots[hex(k)].get('function_end')for k in dispatched):return None
 # parent slots that only return the program index getChunk uses (`mov eax, dword ptr [rcx+index]; ret`)
 current=[]
 for k,v in slots.items():
  if not isinstance(v,dict)or not v.get('function_end'):continue
  b=body(k)
  if b and len(b)==2 and b[1].mnemonic=='ret'and b[0].op_str=='eax, dword ptr [rcx + %s]'%hex(index[0]):current.append(k)
 return dict(va=slots['0x510']['va'],kind='record_copy_chunk',dispatched_parent_slots=sorted(hex(k)for k in dispatched),current_program_slots=sorted(current,key=lambda k:int(k,16)),program_version=1,record_size=stride[0],parameter_count=x['parameters']['count'],
  get_chunk=dict(slot='0x500',va=slots['0x500']['va'],record_base=preset[0],record_stride=stride[0],program_index_field=index[0]),
  validate_chunk=dict(slot='0x508',va=slots['0x508']['va'],version=1),set_chunk=dict(slot='0x510',va=slots['0x510']['va'],preset_flag='r9b'),
  readback=dict(parent_getter_slot='0x58',va=slots['0x58']['va']),layout_contract='original_get_chunk_record')
def main():
 p=json.loads((HERE/'profiles.json').read_text());results=[]
 for x in p['profiles']:
  if x['status']!='discovered':continue
  im=Image(Path(x['dll']['path']));queue={int(v['va'],16)for v in x['parent']['slots'].values()};seen=set();matches={};candidates={}
  # Source-pattern candidates supplement virtual call graph; loadProgram may be nonvirtual.
  for hit in re.finditer(rb'\x83\x3a[\x01-\x10]',im.im):
   try:queue.add(im.function(im.base+hit.start())[0].address)
   except ValueError:pass
  for depth in range(3):
   following=set()
   for va in queue-seen:
    seen.add(va)
    try:ins=im.function(va)
    except ValueError:continue
    result=program_candidate(im,va,x['parameters']['count'])or tagged_layout_candidate(im,va,x['parameters']['count'])
    if result:matches[result['va']]=result
    for i in ins:
     if i.mnemonic=='call'and i.operands[0].type==X86_OP_IMM:
      target=i.operands[0].imm
      if im.base<=target<im.base+len(im.im)and not im.imported(target):following.add(target)
   queue=following
  # Standalone parameter-block constructors place the genuine program loader
  # outside the parent virtual graph. Search PE function boundaries only after
  # that source-proven constructor contract is present.
  if not matches and x['parameter_block']['constructor'].get('kind')=='standalone_pb_constructor':
   for begin,end in im.funcs:
    va=im.base+begin
    if va in seen:continue
    result=tagged_layout_candidate(im,va,x['parameters']['count'])
    if result:matches[result['va']]=result
  # Last tier, only when every bounded rule above found nothing: whole
  # original functions (chained unwind fragments merged) proving the loader
  # contract, with parameter completeness deferred to tagged execution.
  fragments={}
  if not matches:
   for root,parts in unwind_roots(im).items():
    result=loader_contract_candidate(im,parts)
    if result:matches[result['va']]=result;fragments[result['va']]=parts
  if not matches:
   result=record_copy_candidate(im,x)
   if result:matches[result['va']]=result
  result=dict(name=x['name'],functions_examined=len(seen),matches=list(matches.values()),status='discovered'if len(matches)==1 else'ambiguous_or_absent')
  if len(matches)==1:
   candidate=next(iter(matches.values()));parts=fragments.get(candidate['va'])
   code=[i for b,e in parts for i in im.cs.disasm(im.im[b:e],im.base+b)]if parts else im.function(int(candidate['va'],16))
   candidate['source']=im.dump(x['name'].replace(' ','_')+'-program-loader.asm',code)
  results.append(result);print(x['name'],result['status'],len(matches),len(seen),flush=True)
 (HERE/'program-discovery.json').write_text(json.dumps(results,indent=2)+'\n')
if __name__=='__main__':main()
