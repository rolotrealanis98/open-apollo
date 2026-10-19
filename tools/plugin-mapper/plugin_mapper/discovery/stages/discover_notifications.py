"""Original literal notification IDs reachable from PB and original parent setter methods; GUI only."""
from generate_profiles import *
def main():
 data=json.loads((HERE/'profiles.json').read_text());results=[]
 for x in data['profiles']:
  if x['status']!='discovered':continue
  im=Image(Path(x['dll']['path']));queue={int(v['va'],16)for k,v in x['parameter_block']['slots'].items()if int(k,16)<=0x60};queue.add(int(x['parent']['set_parameter']['va'],16));seen=set();events=[]
  for depth in range(3):
   following=set()
   for va in queue-seen:
    seen.add(va)
    try:ins=im.function(va)
    except ValueError:continue
    trace=im.trace(ins)
    for trace_index,r in enumerate(trace):
     if r['ins'].mnemonic!='call':continue
     name=r.get('import_name')or''
     if 'PropagateUpdateToControls' in name or 'PropagateUpdateToRte' in name:
      value=r['regs'].get('rdx')
      events.append(dict(callsite=hex(r['ins'].address),function=hex(ins[0].address),import_name=name,literal_index=value if isinstance(value,int)else None,receiver_expression=r['regs'].get('rcx')))
     # Source-proven self-recursion of the original PB setter propagates its
     # literal internal index through the setter's standard notification path.
     instruction=r['ins']; operand=instruction.operands[0]
     if va==int(x['parameter_block']['slots']['0x8']['va'],16) and operand.type==X86_OP_MEM and operand.mem.disp==8 and im.cs.reg_name(operand.mem.base)=='rax' and r['regs'].get('rcx')==('arg0',0) and isinstance(r['regs'].get('rdx'),int):
      for prior in reversed(trace[max(0,trace_index-12):trace_index]):
       pi=prior['ins'];po=pi.operands
       if po and po[0].type==X86_OP_REG and canon(im.cs.reg_name(po[0].reg))=='rax':
        if pi.mnemonic=='mov' and len(po)==2 and po[1].type==X86_OP_MEM and po[1].mem.disp==0 and not po[1].mem.index and prior['regs'].get(canon(im.cs.reg_name(po[1].mem.base)))==('arg0',0):
         events.append(dict(callsite=hex(instruction.address),function=hex(ins[0].address),import_name='original_pb_setter_virtual_recursion',literal_index=r['regs']['rdx'],receiver_expression=['arg0',0],vtable_load=hex(pi.address),slot='0x8'))
        break
     target=r.get('target')
     if isinstance(target,int)and im.base<=target<im.base+len(im.im)and not name:following.add(target)
   queue=following
  hidden=sorted({e['literal_index']for e in events if e['literal_index']is not None and e['literal_index']>=x['parameters']['count']})
  # Bind recursive hidden indices to the setter's existing virtual-notify call.
  setter_va=int(x['parameter_block']['slots']['0x8']['va'],16)
  try:setter=im.function(setter_va)
  except ValueError:setter=[]
  settertrace=im.trace(setter)
  recursive=sorted({e['literal_index']for e in events if e['import_name']=='original_pb_setter_virtual_recursion' and e['literal_index']>=x['parameters']['count']})
  for n,r in enumerate(settertrace):
   i=r['ins'];op=i.operands[0] if i.operands else None
   if not recursive or i.mnemonic!='call' or op.type!=X86_OP_MEM or im.cs.reg_name(op.mem.base)!='rax' or op.mem.disp!=0x28 or r['regs'].get('rcx')!=('arg0',0) or r['regs'].get('rdx')!=('arg1',0):continue
   for prior in reversed(settertrace[max(0,n-12):n]):
    pi=prior['ins'];po=pi.operands
    if po and po[0].type==X86_OP_REG and canon(im.cs.reg_name(po[0].reg))=='rax':
     if pi.mnemonic=='mov' and len(po)==2 and po[1].type==X86_OP_MEM and po[1].mem.disp==0 and not po[1].mem.index and prior['regs'].get(canon(im.cs.reg_name(po[1].mem.base)))==('arg0',0):
      events.append(dict(callsite=hex(i.address),return_address=hex(i.address+i.size),function=hex(setter_va),import_name='original_pb_virtual_notification',literal_index=None,allowed_internal_indices=recursive,receiver_expression=['arg0',0],argument_expression=['arg1',0],slot='0x28',vtable_load=hex(pi.address)))
     break
  for fn in sorted({e['function']for e in events if e['literal_index']in hidden}):im.dump(x['name'].replace(' ','_')+'-notification-'+fn+'.asm',im.function(int(fn,16)))
  results.append(dict(name=x['name'],hidden_literal_ids=hidden,source_events=events,functions_examined=len(seen),scope='Source-observed literal GUI/RTE notifications, not public parameter IDs; owner and exact source callsite must be validated.'))
  print(x['name'],hidden,flush=True)
 (HERE/'notification-discovery.json').write_text(json.dumps(results,indent=2)+'\n')
if __name__=='__main__':main()
