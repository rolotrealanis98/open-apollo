from generate_profiles import *
import struct
def body(im,va):
 # Original x64 unwind data: a function body continues in RUNTIME_FUNCTION chunks whose UNW_FLAG_CHAININFO parent is the
 # entry chunk (directly or transitively). The frozen probe read only the entry chunk, so a third-argument copy
 # (mov ebx,r8d) whose cmp lives in a later chunk was seen as an unresolved read.
 ins=im.function(va);owned={va-im.base};chunks=[];rf={e.struct.BeginAddress:e.struct.UnwindData for e in im.p.DIRECTORY_ENTRY_EXCEPTION}
 def parent(u):
  if im.im[u]>>3&4==0:return None
  return struct.unpack_from('<I',im.im,u+4+((im.im[u+2]+1)&~1)*2)[0]
 grew=True
 while grew:
  grew=False
  for a,u in sorted(rf.items()):
   if a not in owned and parent(u) in owned:owned.add(a);chunks.append(a);grew=True
 for a in sorted(chunks):ins+=im.function(im.base+a)
 return sorted(ins,key=lambda x:x.address)
def leaf(im,va):
 # A function with no .pdata is a leaf/thunk by construction (x64 non-leaf functions must have unwind data): decode to its first ret / unconditional jmp.
 out=[]
 for i in im.cs.disasm(im.im[va-im.base:va-im.base+0x200],va):
  out.append(i)
  if i.mnemonic=='ret'or i.mnemonic=='jmp'and i.operands[0].type!=X86_OP_IMM:break
 return out
def flow(im,ins,entry,slots,init=None,depth=0,seen=()):
 # Forward must-dataflow over the original control flow (fallthrough + direct branch targets). State = (register-or-slot,tag) pairs.
 # Tags: a0/a1/a2 = unmodified this/second/third argument; vt = this's vtable pointer; oK = pointer loaded from [this+K];
 # ovK = vtable of that object; spD = rsp delta from entry; mD = stack slot (entry-relative D) that holds a0/a1/a2.
 # A ret/jmp ends the path, so an early-exit epilogue's register restore cannot kill the alias. Zero idioms are writes.
 # A tail jmp through [vt+disp] continues into this object's own vtable slot (disp) with the same registers.
 # A call [ovK+S] with rcx=oK and r8d=a2 is recorded as a forward of the unmodified third argument to slot S of the object at this+K.
 by={i.address:i for i in ins};base=frozenset(init or [('r8','a2'),('rcx','a0'),('rdx','a1')]);state={entry:base|{('rsp','sp0')}};work=[entry];comparisons=[];reads=[];tails=[];forwards=[];stores=[]
 volatile={'rax','rcx','rdx','r8','r9','r10','r11'}
 R=lambda r:canon(im.cs.reg_name(r))
 while True:
  if not work:
   # Jump-table targets (indirect jmp) are not direct successors: seed unreached code with the state of the nearest preceding indirect jmp.
   # ponytail: ceiling = no jump-table parse; upgrade by decoding the table the jmp indexes.
   jm=[a for a in state if by[a].mnemonic=='jmp'and by[a].operands[0].type!=X86_OP_IMM]
   for u in sorted(by):
    if u in state:continue
    p=[a for a in jm if a<u]
    if p:state[u]=state[max(p)];work.append(u);break
   if not work:break
  a=work.pop();i=by[a];al=state[a];op=i.mnemonic;os=i.operands;nxt=set(al);T=dict(al)
  d=int(T['rsp'][2:]);a2={r for r,t in al if t=='a2'}
  def mem_slot(o):
   m=o.mem
   return d+m.disp if m.base and R(m.base)=='rsp'and not m.index else None
  zero=op in('xor','pxor','sub')and len(os)==2 and os[0].type==X86_OP_REG and os[1].type==X86_OP_REG and os[0].reg==os[1].reg
  rr,ww=i.regs_access()
  if not zero and any(R(r)in a2 for r in rr):reads.append(dict(va=hex(a),instruction=i.mnemonic+' '+i.op_str))
  if op=='cmp'and len(os)==2 and os[1].type==X86_OP_IMM and os[1].imm==-1:
   if os[0].type==X86_OP_REG and R(os[0].reg)in a2:comparisons.append(hex(a))
   elif os[0].type==X86_OP_MEM and T.get('m%s'%mem_slot(os[0]))=='a2':comparisons.append(hex(a))
  tag=None;store_slot=None
  if op=='mov'and len(os)==2:
   src,dst=os[1],os[0]
   if dst.type==X86_OP_REG:
    if src.type==X86_OP_REG and dst.size==src.size:tag=T.get(R(src.reg))
    elif src.type==X86_OP_MEM:
     m=src.mem
     if mem_slot(src)is not None:tag=T.get('m%d'%mem_slot(src))
     elif m.base and not m.index and src.size==8:
      bt=T.get(R(m.base))
      if bt=='a0':tag='vt'if m.disp==0 else'o%d'%m.disp
      elif bt and bt.startswith('o')and m.disp==0:tag='ov'+bt[1:]
   elif dst.type==X86_OP_MEM:
    if mem_slot(dst)is not None:store_slot=mem_slot(dst);tag=T.get(R(src.reg))if src.type==X86_OP_REG else None
    elif src.type==X86_OP_REG and dst.mem.base and T.get(R(dst.mem.base))=='a0'and T.get(R(src.reg))=='a1':stores.append(dst.mem.disp)
  if store_slot is not None:
   nxt={x for x in nxt if x[0]!='m%d'%store_slot}
   if tag in('a0','a1','a2'):nxt.add(('m%d'%store_slot,tag));tag=None
   else:tag=None
  for w in ww:nxt={x for x in nxt if x[0]!=R(w)}
  if tag and os[0].type==X86_OP_REG:nxt.add((R(os[0].reg),tag))
  nd=d-os[1].imm if op=='sub'and os[0].type==X86_OP_REG and R(os[0].reg)=='rsp'and os[1].type==X86_OP_IMM else d+os[1].imm if op=='add'and os[0].type==X86_OP_REG and R(os[0].reg)=='rsp'and os[1].type==X86_OP_IMM else d-8 if op=='push'else d+8 if op=='pop'else d
  nxt={x for x in nxt if x[0]!='rsp'}|{('rsp','sp%d'%nd)}
  if op=='call':
   if os[0].type==X86_OP_MEM and os[0].mem.base and not os[0].mem.index:
    vb=T.get(R(os[0].mem.base))
    if vb and vb.startswith('ov')and T.get('rcx')=='o'+vb[2:]and T.get('r8')=='a2':forwards.append((int(vb[2:]),os[0].mem.disp))
   nxt={x for x in nxt if x[0]not in volatile}
  if op=='jmp'and os[0].type==X86_OP_MEM and not os[0].mem.index and T.get(R(os[0].mem.base))=='vt':tails.append((os[0].mem.disp,frozenset(al)))
  succ=[]
  if op not in('ret','jmp'):succ.append(a+i.size)
  if op=='jmp'or op.startswith('j')and op!='jmp':
   if os[0].type==X86_OP_IMM:succ.append(os[0].imm)
  for t in succ:
   if t not in by:continue
   new=frozenset(nxt)if t not in state else state[t]&nxt
   if t not in state or new!=state[t]:state[t]=new;work.append(t)
 for disp,al in tails:
  tgt=slots.get(hex(disp));keep=al&{('r8','a2'),('rcx','a0'),('rdx','a1')}
  if tgt is None or tgt in seen or depth>=4 or not{('r8','a2'),('rcx','a0')}<=al:continue
  try:sub=body(im,tgt)
  except ValueError:sub=leaf(im,tgt)
  c,r,_,f,s2=flow(im,sub,tgt,slots,keep,depth+1,seen+(entry,));comparisons+=c;reads+=r;forwards+=f;stores+=s2;ins.extend(x for x in sub if x.address not in by)
 return sorted(set(comparisons)),[dict(t)for t in sorted({tuple(r.items())for r in reads})],ins,sorted(set(forwards)),sorted(set(stores))
def fn(im,va):
 try:return body(im,va)
 except ValueError:return leaf(im,va)
def delegate(im,p,rtti,K):
 # Original attach path: a (usually pdata-less leaf) setter stores its second argument into [first argument+K]. Some parent vtable
 # slot calls it with rcx=lea [b+PBoffset], rdx=lea [b+Y]; the delegate at parent+Y is built by a ctor call lea rcx,[b+Y] whose
 # body installs the vtable of a CHypParamBlock-derived class. Return (vtable_va, slots) or (None, reason).
 pbo=p['parameter_block']['constructor'].get('embedded_offset')
 if pbo is None:return None,'no embedded PB offset'
 ys=set()
 for s in p['parent']['slots'].values():
  if not isinstance(s,dict)or not s.get('va'):continue
  try:F=fn(im,int(s['va'],16))
  except Exception:continue
  for j,x in enumerate(F):
   if x.mnemonic!='call'or x.operands[0].type!=X86_OP_IMM:continue
   try:T=fn(im,x.operands[0].imm)
   except Exception:continue
   if K not in flow(im,list(T),x.operands[0].imm,{})[4]:continue
   lea={}
   for y in F[max(0,j-12):j]:
    if y.mnemonic=='lea'and y.operands[0].type==X86_OP_REG and y.operands[1].type==X86_OP_MEM:lea[R_(im,y.operands[0].reg)]=(R_(im,y.operands[1].mem.base),y.operands[1].mem.disp)
   if 'rcx'in lea and 'rdx'in lea and lea['rcx'][0]==lea['rdx'][0]and lea['rcx'][1]==pbo:ys.add(lea['rdx'][1])
 if len(ys)!=1:return None,'attach site not unique/found: '+str(sorted(ys))
 Y=ys.pop();cands=[v for v in rtti['vtables']if'.?AVCHypParamBlock@@'in v.get('bases',[])and v.get('object_offset')==0 and v['vtable']!=p['parameter_block']['vtable']and v['type']!='.?AVCHypParamBlock@@']
 hit=[]
 for v in cands:
  ctors=im.references(int(v['vtable'],16))
  for m in re.finditer(rb'\xe8',im.im):
   n=m.start()
   if n+5>len(im.im):break
   t=im.base+n+5+struct.unpack_from('<i',im.im,n+1)[0]
   if t not in ctors:continue
   try:G=im.function(im.base+n)
   except ValueError:continue
   k=[j for j,x in enumerate(G)if x.address==im.base+n]
   if not k:continue
   for y in reversed(G[max(0,k[0]-8):k[0]]):
    if y.mnemonic=='lea'and R_(im,y.operands[0].reg)=='rcx':
     if y.operands[1].type==X86_OP_MEM and y.operands[1].mem.disp==Y:hit.append(v)
     break
 if len({v['vtable']for v in hit})!=1:return None,'delegate ctor not unique/found'
 v=hit[0];return v,Y
def R_(im,r):return canon(im.cs.reg_name(r))
data=json.loads((HERE/'profiles.json').read_text());out=[];rtti=json.loads((HERE/'rtti.json').read_text())[0]
for p in data['profiles']:
 im=Image(Path(p['dll']['path']));va=int(p['parameter_block']['slots']['0x20']['va'],16);slots={k:int(v['va'],16)for k,v in p['parameter_block']['slots'].items()if isinstance(v,dict)and v.get('va')}
 comparisons,reads,ins,forwards,_=flow(im,list(fn(im,va)),va,slots);ins=sorted(ins,key=lambda x:x.address);deleg=None;note=None
 if not comparisons and reads and len(forwards)==1:
  # The formatter only forwards the unmodified third argument to slot S of the object at this+K: the verdict is that object's real formatter.
  K,S=forwards[0];v,Y=delegate(im,p,rtti,K)
  if v is None:note='forwarded to object at this+%#x slot %#x; delegate unresolved: %s'%(K,S,Y)
  else:
   dslots={k:int(x['va'],16)for k,x in v['slots'].items()if isinstance(x,dict)and x.get('va')};dva=dslots.get(hex(S))
   if dva is None:note='delegate vtable %s has no slot %#x'%(v['vtable'],S)
   else:
    c2,r2,i2,f2,_=flow(im,list(fn(im,dva)),dva,dslots)
    if f2:note='delegate formatter forwards again (not followed)'
    else:
     comparisons,reads,ins=c2,r2,sorted(i2,key=lambda x:x.address);deleg=dict(attach_setter_offset=K,wrapper_slot=hex(S),delegate_parent_offset=hex(Y),delegate_type=v['type'],delegate_vtable=v['vtable'],delegate_formatter_va=hex(dva),wrapper_formatter_va=hex(va));va_out=dva
 source=im.dump(p['name'].replace(' ','_')+'-formatter-abi.asm',ins);status='original_minus_one_current_state_branch'if comparisons else 'third_argument_not_read'if not reads and not note else 'unresolved'
 out.append(dict(name=p['name'],status=status,formatter_va=hex(deleg and int(deleg['delegate_formatter_va'],16) or va),third_argument_register='r8d',current_state_sentinel=4294967295 if comparisons else None,comparisons=comparisons,original_argument_reads=reads,delegate=deleg,unresolved_note=note,source=source,source_sha256=hashlib.sha256((HERE/source).read_bytes()).hexdigest()))
 print(p['name'],status,comparisons,len(reads),deleg,note,flush=True)
(HERE/'formatter-abi-proof.json').write_text(json.dumps(out,indent=2)+'\n')
