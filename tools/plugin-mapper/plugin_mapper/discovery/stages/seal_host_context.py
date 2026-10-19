"""Bind source-derived host context into data-only profiles; no plugin overrides."""
from generate_profiles import *
def main():
 p=HERE/'profiles.json'; data=json.loads(p.read_text()); contexts=json.loads((HERE/'host-context-discovery.json').read_text()); by={x['name']:x for x in contexts}
 for profile in data['profiles']:
  c=by[profile['name']]; im=Image(Path(profile['dll']['path'])); initializers=[]
  for name,role,size,initial in [('GetProcessHeap','heap',8,'0000000000000000'),('FlsAlloc','fls',4,'ffffffff')]:
   candidates=[x for x in c['fls_allocation_candidates'] if x['import_name']==name]
   if len(candidates)!=1:raise ValueError('nonunique original CRT initializer '+name)
   x=candidates[0]; ins=im.function(int(x['va'],16)); tr=im.trace(ins); calls=[r for r in tr if r.get('import_name')==name]
   if len(calls)!=1 or len(x['return_globals'])!=1:raise ValueError('CRT return-global shape changed')
   g=x['return_globals'][0]
   if (g['size'],g['initial'])!=(size,initial):raise ValueError('CRT original state mismatch')
   result=dict(role=role,va=x['va'],source=x['source'],return_mask=255,success_return=1,global_va=g['va'],global_size=size,global_initial=initial)
   if role=='heap':
    if not any(i.mnemonic=='setne' and i.op_str=='al' for i in ins):raise ValueError('heap bool shape')
   else:
    if not any(i.mnemonic=='cmp' and i.op_str=='eax, -1' for i in ins) or not any(i.mnemonic=='mov' and i.op_str=='al, 1' for i in ins):raise ValueError('FLS bool shape')
    call=calls[0]; callback=call['regs'].get('rcx') if 'regs' in call else None
    # Source LEA immediately before the genuine allocation call defines callback.
    k=next(j for j,i in enumerate(ins) if i.address==call['ins'].address); prev=ins[k-1]
    if prev.mnemonic!='lea' or prev.op_str.split(',')[0]!='rcx' or prev.operands[1].type!=X86_OP_MEM or im.cs.reg_name(prev.operands[1].mem.base)!='rip':raise ValueError('FLS callback expression')
    result['callback_va']=hex(prev.address+prev.size+prev.operands[1].mem.disp)
    result['initializes_current_thread']=True
   source=Path(x['source']); source=source if source.is_absolute() else HERE/source
   result['source_sha256']=hashlib.sha256(source.read_bytes()).hexdigest();initializers.append(result)
  profile['host_context']={'sample_rate':c['sample_rate'],'crt_initializers':initializers}
 before=HERE/'profiles-before-host-context.json'
 if not before.exists():before.write_bytes(p.read_bytes())
 temp=p.with_suffix('.tmp');temp.write_text(json.dumps(data,indent=2)+'\n');temp.replace(p)
 print(json.dumps({'profiles':len(data['profiles']),'sample_rate_getters':len(contexts),'original_crt_initializers':sum(len(x['host_context']['crt_initializers']) for x in data['profiles']),'sha256':hashlib.sha256(p.read_bytes()).hexdigest()}))
if __name__=='__main__':main()
