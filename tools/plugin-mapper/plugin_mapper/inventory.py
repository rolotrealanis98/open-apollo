"""Static per-DLL facts the profile generators read (families/inventory.json), from the user's own install.
No plug-in execution. Image.discover() finds descriptor-producing functions and SDK parameter blocks.
"""
import hashlib,json,re,struct
import pefile
from capstone import Cs,CS_ARCH_X86,CS_MODE_64
from capstone.x86 import X86_OP_MEM,X86_OP_IMM,X86_OP_REG,X86_REG_RIP
load=lambda p:json.loads(p.read_text())
sha=lambda b:hashlib.sha256(b).hexdigest()
fp=lambda x:sha(json.dumps(x,sort_keys=True,separators=(',',':')).encode())
cs=Cs(CS_ARCH_X86,CS_MODE_64);cs.detail=True
ROLES={8:'setter',16:'getter',24:'parse',32:'formatter',40:'coefficient_update',48:'update_forward',56:'resume',80:'cpu_weight',96:'setup'}

class Image:
 def __init__(self,path):
  self.function_cache={};self.raw=path.read_bytes();self.pe=pefile.PE(data=self.raw,fast_load=True)
  self.pe.parse_data_directories(directories=[1,3]);self.base=self.pe.OPTIONAL_HEADER.ImageBase
  self.sections=self.pe.sections;self.text=next(s for s in self.sections if s.Characteristics&0x20000000);self.code=self.text.get_data();self.tva=self.base+self.text.VirtualAddress
  self.imports={i.address:(e.dll.decode()+'!'+(i.name or str(i.ordinal).encode()).decode()) for e in getattr(self.pe,'DIRECTORY_ENTRY_IMPORT',[]) for i in e.imports}
  self.ranges=[(self.base+x.struct.BeginAddress,self.base+x.struct.EndAddress,x.struct.UnwindData) for x in getattr(self.pe,'DIRECTORY_ENTRY_EXCEPTION',[])]
  self.thunks={}
  for m in re.finditer(b'\xff\x25....',self.code,re.S):
   va=self.tva+m.start();target=va+6+struct.unpack_from('<i',self.code,m.start()+2)[0]
   if target in self.imports:self.thunks[va]=self.imports[target]
 def bytes(self,va,n):return self.pe.get_data(va-self.base,n)
 def preview(self,va,n):
  try:return self.bytes(va,n).hex()
  except pefile.PEFormatError:return None
 def qword(self,va):return struct.unpack('<Q',self.bytes(va,8))[0]
 def codeptr(self,va):return self.tva<=va<self.tva+len(self.code)
 def extent(self,va):
  r=next((r for r in self.ranges if r[0]<=va<r[1]),None)
  if r is None:return None
  primary=r[0]
  def parent(row):
   x=self.bytes(self.base+row[2],4);flags=x[0]>>3
   if flags&4:return self.base+struct.unpack('<I',self.bytes(self.base+row[2]+((4+2*x[2]+3)&~3),4))[0]
   return row[0]
  primary=parent(r)
  rows=[(a,b) for a,b,u in self.ranges if a==primary or parent((a,b,u))==primary]
  return rows
 def rtti(self,vt):
  try:
   col=self.qword(vt-8)
   sig,off,cd,td,ch,selfr=struct.unpack('<6I',self.bytes(col,24))
   if sig!=1 or self.base+selfr!=col:return None
   name=self.bytes(self.base+td+16,256).split(b'\0')[0].decode()
   if not name.startswith('.?A'):return None
   return dict(name=name,subobject_offset=off,locator=hex(col))
  except Exception:return None
 def data_refs(self,target):
  needle=struct.pack('<Q',target);out=[]
  for sec in self.sections:
   if sec.Characteristics&0x20000000:continue
   data=sec.get_data();at=data.find(needle)
   while at>=0:
    if at%8==0:out.append(self.base+sec.VirtualAddress+at)
    at=data.find(needle,at+1)
  return out
 def function_record(self,va):
  if va in self.function_cache:return self.function_cache[va]
  ex=self.extent(va);record=dict(va=hex(va),pe_ranges=[[hex(a),hex(b)] for a,b in ex] if ex else None,exact_unwind_bytes_sha256=sha(b''.join(self.bytes(a,b-a) for a,b in ex)) if ex else None,coverage='PE ranges including chained fragments; may include embedded jump-table bytes' if ex else 'leaf/tailcall without unwind extent; no invented hash')
  tokens=[];deps=[];covered=0;tables={}
  if ex:
   start=ex[0][0]
   decoded=[ins for a,b in ex for ins in cs.disasm(self.bytes(a,b-a),a)]
   # MSVC indexed RVA jump tables are data, not instructions. Recognize them
   # only when a real memory operand references at least two in-function RVAs.
   for ins in decoded:
    for op in ins.operands:
     if op.type!=X86_OP_MEM:continue
     target=(ins.address+ins.size+op.mem.disp) if op.mem.base==X86_REG_RIP else self.base+op.mem.disp
     if not any(a<=target<b for a,b in ex):continue
     entries=[]
     for n in range(256):
      if not any(a<=target+4*n and target+4*n+4<=b for a,b in ex):break
      dest=self.base+struct.unpack('<I',self.bytes(target+4*n,4))[0]
      if not any(a<=dest<b for a,b in ex):break
      entries.append(dest-start)
     if len(entries)>=2:tables[target]=entries
   for a,b in ex:
    for ins in cs.disasm(self.bytes(a,b-a),a):
     if any(t<=ins.address<t+4*len(entries) for t,entries in tables.items()):continue
     covered+=ins.size;ops=[]
     for op in ins.operands:
      if op.type==X86_OP_REG:ops.append(['reg',ins.reg_name(op.reg),op.size])
      elif op.type==X86_OP_IMM:
       v=op.imm
       if (ins.mnemonic.startswith('j') or ins.mnemonic=='call') and self.codeptr(v):
        if any(x<=v<y for x,y in ex):ops.append(['local_branch',v-start])
        else:
         imp=self.thunks.get(v);ops.append(['call_or_branch',imp or 'internal_function'])
         deps.append(dict(kind='import' if imp else 'internal_code_unresolved_closure',target=hex(v),identity=imp,prefix_sha256=None if imp else sha(self.bytes(v,64))))
       else:ops.append(['imm',v,op.size])
      elif op.type==X86_OP_MEM:
       mem=op.mem
       if mem.base==X86_REG_RIP:
        target=ins.address+ins.size+mem.disp;imp=self.imports.get(target);ops.append(['rip_memory',op.size,imp or 'data'])
        deps.append(dict(kind='import_pointer' if imp else 'data_preview_not_full_closure',target=hex(target),identity=imp,bytes=self.preview(target,max(op.size,16)) if not imp else None))
       else:
        displacement=['local_rva_table',self.base+mem.disp-start] if self.base+mem.disp in tables else mem.disp
        ops.append(['mem',ins.reg_name(mem.base),ins.reg_name(mem.index),mem.scale,displacement,op.size])
      else:ops.append(['other',op.type])
     tokens.append([ins.mnemonic,ops])
   tokens.append(['normalized_rva_tables',sorted((t-start,values) for t,values in tables.items())])
   record.update(recognized_rva_jump_tables={hex(t):v for t,v in tables.items()},normalized_instruction_shape_sha256=fp(tokens),normalized_shape_with_dependency_previews_sha256=fp([tokens,[{k:v for k,v in d.items() if k!='target'} for d in deps]]),decoded_bytes=covered,extent_bytes=sum(b-a for a,b in ex),dependencies=deps,normalization_warning='Address normalization is a candidate grouping only. Calls and data closures are not proven equal; PE extents may contain embedded tables.')
  self.function_cache[va]=record;return record
 def discover(self):
  descriptors=[]
  # Shape alone selects candidates; render_id is deliberately not a lookup key.
  for m in re.finditer(b'\xc7[\x80-\x87](?:\x24)?\xa4\x0a\x00\x00....',self.code,re.S):
   va=self.tva+m.start();ins=next(cs.disasm(self.bytes(va,16),va,count=1),None)
   if ins is None or ins.mnemonic!='mov' or len(ins.operands)!=2:continue
   a,b=ins.operands
   if a.type!=X86_OP_MEM or a.mem.disp!=0xaa4 or b.type!=X86_OP_IMM:continue
   extent=self.extent(va)
   if not extent:continue
   fn=extent[0][0];vts=[]
   for ref in self.data_refs(fn):
    for slot in (0x08,0x10,0x18):
     vt=ref-slot;rtti=self.rtti(vt)
     if rtti and all(self.codeptr(self.qword(vt+off)) for off in range(0,slot+1,8)):
      vts.append(dict(va=hex(vt),slot=hex(slot),rtti=rtti,abi_shape=f"receiver+{rtti['subobject_offset']:x}/vtable+{slot:x}"))
   descriptors.append(dict(function=self.function_record(fn),render_store_va=hex(va),render_id=b.imm,descriptor_subobject_vtables=vts,status='RTTI_vtable_descriptor_slot_confirmed' if vts else 'store_candidate_no_receiver_proof'))
  blocks=[]
  for thunk,imp in self.thunks.items():
   if 'convParamNorm2Int@CHypParamBlock' not in imp:continue
   for ref in self.data_refs(thunk):
    vt=ref-0x98;rtti=self.rtti(vt)
    if not rtti:continue
    if 'CHypParamBlock@@' in rtti['name'] or 'AuthKey@@' in rtti['name']:continue
    if 'convParamInt2Norm@CHypParamBlock' not in self.thunks.get(self.qword(vt+0xa0),''):continue
    methods={name:self.function_record(self.qword(vt+off)) for off,name in ROLES.items()}
    if not all(self.codeptr(int(x['va'],16)) for x in methods.values()):continue
    constructors=[]
    for m in re.finditer(b'[\x48\x4c]\x8d[\x05\x0d\x15\x1d\x25\x2d\x35\x3d]....',self.code,re.S):
     use=self.tva+m.start();dest=use+7+struct.unpack_from('<i',self.code,m.start()+3)[0]
     if dest!=vt:continue
     ex=self.extent(use)
     if not ex:continue
     data=self.bytes(ex[0][0],ex[-1][1]-ex[0][0]);calls=[]
     for ins in cs.disasm(data,ex[0][0]):
      if ins.mnemonic=='call' and ins.operands and ins.operands[0].type==X86_OP_IMM:
       target=ins.operands[0].imm
       if '??0CHypParamBlock@@' in self.thunks.get(target,''):calls.append(hex(ins.address))
     if calls:constructors.append(dict(containing_function=hex(ex[0][0]),vtable_reference=hex(use),base_constructor_calls=calls,note='Containing function may be composite wrapper; not automatically safe to invoke as core constructor.'))
    blocks.append(dict(vtable=hex(vt),rtti=rtti,methods=methods,constructor_containers=constructors,method_body_bundle=fp({k:v['exact_unwind_bytes_sha256'] for k,v in methods.items()}) if all(v['exact_unwind_bytes_sha256'] for v in methods.values()) else None,status='SDK_conversion_tail_and_RTTI_confirmed',unknown=['Receiver construction, notifications, cache callbacks, coefficient state/rounding, resource lifecycle and DSP routing require proof.']))
  return dict(descriptors=descriptors,parameter_blocks=blocks,base_sdk_imports=sorted({s.split('!')[0] for s in self.imports.values() if 'CHypParamBlock' in s}),directly_executable=False)
