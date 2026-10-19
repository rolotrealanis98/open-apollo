"""Discover original bounded CRT static-lock array initialization by instruction shape."""
from generate_profiles import *
data=json.loads((HERE/'profiles.json').read_text()); contexts={x['name']:x for x in json.loads((HERE/'host-context-discovery.json').read_text())};out=[]
for p in data['profiles']:
 im=Image(Path(p['dll']['path']));wrappers={int(c['va'],16) for c in contexts[p['name']]['fls_allocation_candidates'] if c['import_name'].startswith('InitializeCriticalSection')};candidates=set()
 for m in re.finditer(rb'\xe8',im.im):
  r=m.start()
  if r+5<=len(im.im) and im.base+r+5+struct.unpack_from('<i',im.im,r+1)[0]in wrappers:
   try:candidates.add(im.function(im.base+r)[0].address)
   except ValueError:pass
 matches=[]
 for va in candidates:
  ins=im.function(va);shape=[(i.mnemonic,i.op_str)for i in ins]
  # Original UCRT loop: base + (index + index*4)*8; fixed bound, bool return.
  if ('lea','rcx, [rbx + rbx*4]') not in shape or ('lea','rcx, [rdx + rcx*8]')not in shape or ('mov','al, 1')not in shape:continue
  bases=[i.address+i.size+i.operands[1].mem.disp for i in ins if i.mnemonic=='lea' and i.op_str.startswith('rdx, [rip')]
  bounds=[i.operands[1].imm for i in ins if i.mnemonic=='cmp' and i.op_str.startswith('ebx,')and i.operands[1].type==X86_OP_IMM]
  if len(bases)!=1 or len(bounds)!=1 or not 1<=bounds[0]<=256:raise ValueError('lock loop unsupported')
  src=im.dump(p['name'].replace(' ','_')+'-crt-lock-array.asm',ins)
  matches.append(dict(role='locks',va=hex(va),return_mask=255,success_return=1,array_va=hex(bases[0]),count=bounds[0],stride=40,source=src,source_sha256=hashlib.sha256((HERE/src).read_bytes()).hexdigest()))
 if len(matches)!=1:raise ValueError('lock initializer not unique '+p['name']+str(matches))
 out.append(dict(name=p['name'],initializer=matches[0]));print(p['name'],matches[0]['va'],matches[0]['count'],flush=True)
(HERE/'crt-lock-discovery.json').write_text(json.dumps(out,indent=2)+'\n')
before=HERE/'profiles-before-crt-locks.json'
if not before.exists():before.write_bytes((HERE/'profiles.json').read_bytes())
by={x['name']:x['initializer']for x in out}
for p in data['profiles']:
 entries=[x for x in p['host_context']['crt_initializers']if x['role']!='locks']; entries.insert(0,by[p['name']]);p['host_context']['crt_initializers']=entries
path=HERE/'profiles.json';temp=path.with_suffix('.tmp');temp.write_text(json.dumps(data,indent=2)+'\n');temp.replace(path)
print('PROFILE_SHA256',hashlib.sha256(path.read_bytes()).hexdigest())
