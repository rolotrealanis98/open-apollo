"""Derive original parent members receiving the genuine embedded parameter-block pointer."""
from generate_profiles import *
def distinct_parent_pb(im,trace,call,sp,base,owner,receiver):
 # The base-constructor pointer argument names another parent-embedded object whose own original
 # constructor calls the SDK CHypParamBlock constructor on its receiver and installs a CHyp-derived vtable.
 path=HERE/'rtti.json'
 if not path.exists():return[]
 vtables={int(str(v['vtable']),0):v for r in json.loads(path.read_text())for v in r['vtables']if'.?AVCHypParamBlock@@'in v.get('bases',[])};out=[]
 for r in base:
  address=r.get('store_address');value=r.get('store_value')
  if not isinstance(address,tuple)or address[0]!='arg0'or not isinstance(value,tuple)or value[0]!='stack_argument'or r.get('store_size')!=8:continue
  target=call['mem'].get(add(sp,value[1]-8))
  if not isinstance(target,tuple)or target[0]!=owner[0]or target==receiver:continue
  for c in trace:
   if c['ins'].mnemonic!='call'or c['regs'].get('rcx')!=target or not isinstance(c.get('target'),int):continue
   sub=im.trace(im.function(c['target']))
   if not any(s.get('import_name')==SDK_CTOR and s['regs'].get('rcx')==('arg0',0)for s in sub):continue
   found={s['store_value']for s in sub if s.get('store_address')==('arg0',0)and s.get('store_size')==8 and s.get('store_value')in vtables}
   if len(found)==1:
    vt=found.pop();out.append(dict(field_offset=address[1],embedded_offset=target[1]-owner[1],constructor=hex(c['target']),type=vtables[vt]['type'],vtable=hex(vt),constructor_callsite=c['ins'].address,call_regs=c['regs'],parent_store_va=hex(r['ins'].address),callee_stack_argument=value[1],caller_stack_argument=value[1]-8))
 return out
def fn_body(im,va):
 try:return im.function(va)
 except ValueError:
  end=leaf_end(im,va)
  return None if end is None else list(im.cs.disasm(im.im[va-im.base:end-im.base],va))
def forwards(body,member,slot):
 # call/jmp qword [[reg+member]+slot]: the slot dispatches to the same slot of the object held at this+member.
 tags={}
 for i in body:
  o=i.operands
  if i.mnemonic in('call','jmp')and o and o[0].type==X86_OP_MEM and tags.get(canon(i.reg_name(o[0].mem.base)))=='member_vtable'and o[0].mem.disp==slot:return True
  if len(o)==2 and o[0].type==X86_OP_REG and i.mnemonic not in('cmp','test'):
   dst=canon(i.reg_name(o[0].reg));src=canon(i.reg_name(o[1].mem.base))if o[1].type==X86_OP_MEM else None;tag=tags.get(src);tags.pop(dst,None)
   if i.mnemonic=='mov'and o[1].type==X86_OP_MEM and o[1].size==8 and not o[1].mem.index:
    if o[1].mem.disp==member and src!='rsp':tags[dst]='member'
    elif o[1].mem.disp==0 and tag=='member':tags[dst]='member_vtable'
 return False
def public_block(p,im,pair,field):
 """Which embedded PB implements the public parameter roles (setter/getter/parse/format slots).
 profile: every parent-bound role slot forwards to the same slot of the profile PB held in the member the
 original attach stores. parent_bound: no attach, every profile-PB role slot is one constant stub (no memory
 operand) and the parent-bound slots are its own functions. Either way the original parent setter must dispatch
 only to the parent-bound PB."""
 q=lambda a:struct.unpack_from('<Q',im.im,a-im.base)[0];vt=int(pair['vtable'],16);slots=p['parameter_block']['slots']
 roles=sorted((k for k,v in slots.items()if isinstance(v,dict)and v.get('candidate_role')in('integer_setter','integer_getter','parse','format')),key=lambda k:int(k,16))
 setter=fn_body(im,int(p['parent']['set_parameter']['va'],16))if p['parent'].get('set_parameter')else None
 if not roles or not setter:return dict(block='unresolved',reason='profiled PB roles or parent setter absent')
 outer_off,inner_off=pair['embedded_offset'],p['parameter_block']['constructor']['embedded_offset']
 routed={r['regs']['rcx'][1]for r in im.trace(setter)if r['ins'].mnemonic in('call','jmp')and isinstance(r['regs'].get('rcx'),tuple)and r['regs']['rcx'][0]=='arg0'and r['regs']['rcx'][1]in(outer_off,inner_off)}
 if routed!={outer_off}:return dict(block='unresolved',reason='parent setter does not dispatch only to the parent-bound PB: '+str(sorted(routed)))
 uses=[hex(r['ins'].address)for r in im.trace(setter)if r['ins'].mnemonic in('call','jmp')and r['regs'].get('rcx')==('arg0',outer_off)]
 public={}
 for k in roles:
  b=fn_body(im,q(vt+int(k,16)))
  if b is None:return dict(block='unresolved',reason='parent-bound slot '+k+' not decodable')
  public[k]=dict(va=hex(b[0].address),function_end=hex(b[-1].address+b[-1].size),candidate_role=slots[k]['candidate_role'])
 evidence=dict(role_slots=roles,parent_setter=p['parent']['set_parameter']['va'],parent_setter_dispatch_to_parent_bound=uses,parent_pb_field=hex(field))
 attach=pair['setup']['attach']
 if attach:
  stores=[i.operands[0].mem.disp for i in fn_body(im,int(attach['target'],16))or[]if i.mnemonic=='mov'and i.operands[0].type==X86_OP_MEM and i.operands[0].size==8 and i.reg_name(i.operands[0].mem.base)!='rsp']
  if len(stores)!=1:return dict(block='unresolved',reason='original attach member store not unique')
  if all(forwards(fn_body(im,int(public[k]['va'],16)),stores[0],int(k,16))for k in roles):
   return dict(block='profile',kind='parent_bound_forwards_through_attach_member',attach_member_offset=hex(stores[0]),parent_bound_slots=public,**evidence)
  return dict(block='unresolved',reason='parent-bound role slots do not all forward through the attach member')
 stub={int(slots[k]['va'],16)for k in roles}
 if len(stub)==1:
  b=fn_body(im,next(iter(stub)))
  if b and b[-1].mnemonic=='ret'and not any(o.type==X86_OP_MEM for i in b for o in i.operands)and not any(int(v['va'],16)in stub for v in public.values()):
   return dict(block='parent_bound',kind='profile_pb_roles_are_one_constant_stub',profile_role_stub=hex(b[0].address),profile_role_stub_body='; '.join(i.mnemonic+' '+i.op_str for i in b),public_slots=public,**evidence)
 return dict(block='unresolved',reason='neither forwarding nor constant-stub evidence')
def two_pb_contract(p,im,trace,owner,ctor,d):
 """Original two-PB parent: the parent constructor builds the parent-bound PB (stored by the base constructor
 into its pointer field), then the profile PB inline; original parent slot 0x578 sets up both with the same
 (parent+fs field, parent, 0) Setup ABI and, when present, attaches the profile PB to the parent-bound PB."""
 if ctor.get('kind')!='inline_span':return None,'profile PB constructor is not an inline span'
 entry=d['constructor_callsite'];inner_entry=int(ctor['entry'],16);stop=int(ctor['stop'],16);outer=d['embedded_offset'];inner=ctor['embedded_offset']
 if not entry<inner_entry<stop:return None,'parent-bound PB constructor does not precede the profile PB span'
 span=[r for r in trace if entry<=r['ins'].address<stop]
 if not span or span[0]['ins'].address!=entry or any(r['ins'].mnemonic.startswith('j')or r['ins'].mnemonic=='ret'for r in span):return None,'parent constructor span is not linear'
 registers={k:dict(base='parent',offset=v[1]-owner[1])for k,v in d['call_regs'].items()if k!='rsp'and isinstance(v,tuple)and v[0]==owner[0]}
 if registers.get('rcx')!=dict(base='parent',offset=outer):return None,'parent-bound PB receiver register differs'
 slot=p['parent']['slots'].get('0x578')
 if not isinstance(slot,dict)or not slot.get('function_end'):return None,'original parent slot 0x578 absent'
 setup_ins=im.function(int(slot['va'],16));setup=[r for r in im.trace(setup_ins)if r['ins'].mnemonic=='call']
 def setups(offset):return[r for r in setup if r['regs'].get('rcx')==('arg0',offset)and r['regs'].get('r8')==('arg0',0)and r['regs'].get('r9')==0 and isinstance(r['regs'].get('rdx'),tuple)and r['regs']['rdx'][0]=='arg0']
 si,so=setups(inner),setups(outer)
 if len(si)!=1 or len(so)!=1 or si[0]['regs']['rdx']!=so[0]['regs']['rdx']:return None,'original slot 0x578 does not set up both PBs with one Setup ABI'
 attach=[r for r in setup if r['regs'].get('rcx')==('arg0',outer)and r['regs'].get('rdx')==('arg0',inner)]
 if len(attach)>1:return None,'ambiguous original PB attach'
 name=p['name'].replace(' ','_');span_src=im.dump(name+'-two-pb-constructor-span.asm',[r['ins']for r in span]);setup_src=im.dump(name+'-two-pb-parent-setup.asm',setup_ins)
 def site(r):return dict(callsite=hex(r['ins'].address),target=hex(r['target'])if isinstance(r.get('target'),int)else'virtual_slot_'+hex(r['ins'].operands[0].mem.disp))
 return dict(type=d['type'],vtable=d['vtable'],constructor=d['constructor'],constructor_callsite=hex(entry),embedded_offset=outer,
  constructor_span=dict(kind='inline_span',entry=hex(entry),stop=ctor['stop'],registers=registers,stack_writes=ctor.get('stack_writes',[]),source=span_src,source_sha256=hashlib.sha256((HERE/span_src).read_bytes()).hexdigest()),
  setup=dict(parent_slot='0x578',va=slot['va'],function_end=slot['function_end'],sample_rate_field_offset=si[0]['regs']['rdx'][1],profile_pb_setup=site(si[0]),parent_bound_pb_setup=site(so[0]),attach=site(attach[0])if attach else None,source=setup_src,source_sha256=hashlib.sha256((HERE/setup_src).read_bytes()).hexdigest())),None
def main():
 data=json.loads((HERE/'profiles.json').read_text());out=[]
 for p in data['profiles']:
  im=Image(Path(p['dll']['path']));ctor=p['parameter_block']['constructor'];parent_ctor=ctor.get('parent_constructor_va',ctor['va']);trace=im.trace(im.function(int(parent_ctor,16)));pvt=int(p['parent']['vtable'],16)
  owners=[r['store_address']for r in trace if r.get('store_value')==pvt]
  if len(owners)!=1:raise ValueError('ambiguous original parent owner')
  owner=owners[0];receiver=add(owner,ctor['embedded_offset']);calls=[r for r in trace if r['ins'].address==int(p['parameters']['parent_callsite'],16)]
  if len(calls)!=1 or calls[0]['regs'].get('rcx')!=owner:raise ValueError('parent constructor call context mismatch')
  call=calls[0];sp=call['regs']['rsp'];baseins=im.function(int(p['parent']['constructor_base'],16));base=im.trace(baseins);bindings=[];pair=None
  for r in base:
   address=r.get('store_address');value=r.get('store_value')
   if not isinstance(address,tuple)or address[0]!='arg0'or not isinstance(value,tuple)or value[0]!='stack_argument'or r.get('store_size')!=8:continue
   caller_offset=value[1]-8
   if call['mem'].get(add(sp,caller_offset))!=receiver:continue
   bindings.append(dict(field_offset=address[1],storage='pointer64',points_to='parameter_block',embedded_offset=ctor['embedded_offset'],parent_store_va=hex(r['ins'].address),callee_stack_argument=value[1],caller_stack_argument=caller_offset,parent_callsite=hex(call['ins'].address),constructor_base=p['parent']['constructor_base']))
  if not bindings:
   distinct=distinct_parent_pb(im,trace,call,sp,base,owner,receiver)
   if len(distinct)==1:
    d=distinct[0];pair,why=two_pb_contract(p,im,trace,owner,ctor,d)
    if pair is None:raise ValueError('PB parent binding targets a distinct original CHypParamBlock object, not the profile PB: parent field '+hex(d['field_offset'])+' <- parent+'+hex(d['embedded_offset'])+' '+d['type']+' (constructor '+d['constructor']+'); profile PB '+p['parameter_block']['type']+' at parent+'+hex(ctor['embedded_offset'])+'; two-PB parent contract unresolved: '+why)
    bindings.append(dict(field_offset=d['field_offset'],storage='pointer64',points_to='parent_bound_parameter_block',embedded_offset=d['embedded_offset'],parent_store_va=d['parent_store_va'],callee_stack_argument=d['callee_stack_argument'],caller_stack_argument=d['caller_stack_argument'],parent_callsite=hex(call['ins'].address),constructor_base=p['parent']['constructor_base']))
  if len(bindings)!=1:raise ValueError('PB parent binding not unique '+p['name']+' '+str(bindings))
  b=bindings[0];source=p['name'].replace(' ','_')+'-parent-pb-binding.asm';b['source']=im.dump(source,baseins);b['source_sha256']=hashlib.sha256((HERE/b['source']).read_bytes()).hexdigest();p['host_context']['parameter_block_pointer']=b
  if pair is not None:p['host_context']['parent_bound_parameter_block']=pair;p['parameter_block']['public_block']=public_block(p,im,pair,b['field_offset'])
  # seal_host_context rebuilds host_context; carry generate_profiles' source-attested dynamic initializers into it.
  dyn=p.pop('crt_dynamic_initializers',None)
  if isinstance(dyn,dict)and dyn.get('entries'):p['host_context']['crt_dynamic_initializers']=dyn
  out.append(dict(name=p['name'],binding=b,parent_bound_parameter_block=pair));print(p['name'],b['field_offset'],b['caller_stack_argument'],b['points_to'],flush=True)
 path=HERE/'profiles.json';before=HERE/'profiles-before-parent-binding.json'
 if not before.exists():before.write_bytes(path.read_bytes())
 tmp=path.with_suffix('.tmp');tmp.write_text(json.dumps(data,indent=2)+'\n');tmp.replace(path);(HERE/'parent-binding-discovery.json').write_text(json.dumps(out,indent=2)+'\n');print('PROFILE_SHA256',hashlib.sha256(path.read_bytes()).hexdigest())
if __name__=='__main__':main()
