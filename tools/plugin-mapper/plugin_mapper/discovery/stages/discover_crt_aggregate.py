"""Find original forward CRT table dispatcher from already proven initializer anchors."""
from generate_profiles import *
data=json.loads((HERE/'profiles.json').read_text());results=[]
for p in data['profiles']:
 im=Image(Path(p['dll']['path']));anchors={int(x['va'],16)for x in p['host_context']['crt_initializers']};matches=[]
 for m in re.finditer(rb'\x48\x8d\x15....\x48\x8d\x0d....\xe9',im.im,re.DOTALL):
  r=m.start();ins=list(im.cs.disasm(im.im[r:r+19],im.base+r))
  if len(ins)!=3 or ins[-1].mnemonic!='jmp':continue
  end=ins[0].address+ins[0].size+ins[0].operands[1].mem.disp;start=ins[1].address+ins[1].size+ins[1].operands[1].mem.disp
  if not im.base<=start<end<=im.base+len(im.im)or(end-start)%16 or not 1<=(end-start)//16<=128:continue
  ptrs=struct.unpack_from('<'+'Q'*((end-start)//8),im.im,start-im.base)
  if not anchors.issubset(set(ptrs[::2])):continue
  target=ins[-1].operands[0].imm;body=im.function(target);shape=[(i.mnemonic,i.op_str)for i in body]
  if not all(x in shape for x in [('mov','rax, qword ptr [rbx]'),('add','rbx, 0x10'),('test','al, al'),('mov','al, 1'),('xor','al, al')]):raise ValueError('CRT table executor shape changed')
  entries=[]
  for i,(init,uninit)in enumerate(zip(ptrs[::2],ptrs[1::2])):
   for va in(init,uninit):
    if va and not any(s.VirtualAddress<=va-im.base<s.VirtualAddress+s.Misc_VirtualSize and s.Characteristics&0x20000000 for s in im.p.sections):raise ValueError('nonexecutable initializer')
   entries.append(dict(ordinal=i,initialize=hex(init),uninitialize=hex(uninit)))
  prefix=p['name'].replace(' ','_');sources=[im.dump(prefix+'-crt-aggregate-entry.asm',ins),im.dump(prefix+'-crt-aggregate-executor.asm',body)]
  matches.append(dict(va=hex(im.base+r),executor_va=hex(target),table_start=hex(start),table_end=hex(end),entry_stride=16,entries=entries,return_mask=255,success_return=1,arguments=[],source=sources,source_sha256={s:hashlib.sha256((HERE/s).read_bytes()).hexdigest()for s in sources},table_sha256=hashlib.sha256(im.im[start-im.base:end-im.base]).hexdigest(),failure_behavior='original executor stops on AL zero and invokes prior cleanup entries in reverse order',execution_status='source_proven_not_executed'))
 if len(matches)!=1:raise ValueError('aggregate initializer not unique '+p['name']+str(len(matches)))
 p['host_context']['crt_aggregate']=matches[0];results.append(dict(name=p['name'],aggregate=matches[0]));print(p['name'],matches[0]['va'],len(matches[0]['entries']),flush=True)
(HERE/'crt-aggregate-discovery.json').write_text(json.dumps(results,indent=2)+'\n');path=HERE/'profiles.json';before=HERE/'profiles-before-crt-aggregate.json'
if not before.exists():before.write_bytes(path.read_bytes())
tmp=path.with_suffix('.tmp');tmp.write_text(json.dumps(data,indent=2)+'\n');tmp.replace(path);print('PROFILE_SHA256',hashlib.sha256(path.read_bytes()).hexdigest())
