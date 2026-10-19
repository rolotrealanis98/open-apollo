"""Derived host rate getter and original CRT FLS allocation initializer candidates."""
from generate_profiles import *
def main():
 data=json.loads((HERE/'profiles.json').read_text());results=[]
 for x in data['profiles']:
  if x['status']!='discovered':continue
  im=Image(Path(x['dll']['path']));slot=x['parent']['slots']['0xf8'];va=int(slot['va'],16);leaf=[]
  for i in im.cs.disasm(im.im[va-im.base:va-im.base+32],va):
   leaf.append(i)
   if i.mnemonic in('ret','jmp'):break
  if len(leaf)!=2 or leaf[0].mnemonic!='movss' or leaf[1].mnemonic!='ret':raise ValueError('parent sample rate getter shape changed')
  a,b=leaf[0].operands
  if a.type!=X86_OP_REG or im.cs.reg_name(a.reg)!='xmm0'or b.type!=X86_OP_MEM or im.cs.reg_name(b.mem.base)!='rcx'or b.mem.index:raise ValueError('sample rate source expression unsupported')
  rate=dict(slot='0xf8',va=hex(va),role='sample_rate',field_offset=b.mem.disp,storage='float32',return_register='xmm0',initialize_from='configured_sample_rate',source=im.dump(x['name'].replace(' ','_')+'-parent-sample-rate.asm',leaf))
  # Locate actual FlsAlloc callsites from the import relocation, including thunk calls.
  fls_iats={a for a,n in im.imports.items()if n in('FlsAlloc','GetProcessHeap','InitializeCriticalSectionAndSpinCount','InitializeCriticalSectionEx','InitializeCriticalSection')};thunks=set();calls=set()
  for m in re.finditer(rb'\xff[\x15\x25]',im.im):
   r=m.start();target=im.base+r+6+struct.unpack_from('<i',im.im,r+2)[0]
   if target in fls_iats:
    if im.im[r+1]==0x25:
     thunks.add(im.base+r)
     if r and 0x40<=im.im[r-1]<=0x4f:thunks.add(im.base+r-1)
    else:calls.add(im.base+r)
  for m in re.finditer(rb'\xe8',im.im):
   r=m.start()
   if r+5<=len(im.im)and im.base+r+5+struct.unpack_from('<i',im.im,r+1)[0]in thunks:calls.add(im.base+r)
  for m in re.finditer(rb'[\x48\x4c]\x8b[\x05\x0d\x15\x1d\x25\x2d\x35\x3d]',im.im):
   r=m.start();target=im.base+r+7+struct.unpack_from('<i',im.im,r+3)[0]
   if target in fls_iats:calls.add(im.base+r)
  candidates=[];seen=set()
  for site in calls:
   try:ins=im.function(site)
   except ValueError:continue
   if ins[0].address in seen:continue
   seen.add(ins[0].address);trace=im.trace(ins)
   actual=[r for r in trace if r.get('import_name')in('FlsAlloc','GetProcessHeap','InitializeCriticalSectionAndSpinCount','InitializeCriticalSectionEx','InitializeCriticalSection')]
   if not actual:continue
   site=actual[0]['ins'].address;globals=[]
   for r in trace:
    addr=r.get('store_address');value=r.get('store_value')
    if isinstance(addr,int)and isinstance(value,tuple)and value[0]==f'call_return_{site:x}':
     globals.append(dict(va=hex(addr),store_va=hex(r['ins'].address),size=r['store_size'],initial=im.im[addr-im.base:addr-im.base+r['store_size']].hex()))
   candidates.append(dict(va=hex(ins[0].address),callsite=hex(site),import_name=actual[0]['import_name'],return_globals=globals,source=im.dump(x['name'].replace(' ','_')+'-fls-init-'+hex(ins[0].address)+'.asm',ins),direct_calls=[dict(va=hex(r['ins'].address),target=hex(r['target'])if isinstance(r.get('target'),int)else None,import_name=r.get('import_name'))for r in trace if r['ins'].mnemonic=='call']))
  results.append(dict(name=x['name'],sample_rate=rate,fls_allocation_candidates=candidates))
  print(x['name'],'rate',b.mem.disp,'FLS',[(c['va'],c['return_globals'])for c in candidates],flush=True)
 (HERE/'host-context-discovery.json').write_text(json.dumps(results,indent=2)+'\n')
if __name__=='__main__':main()
