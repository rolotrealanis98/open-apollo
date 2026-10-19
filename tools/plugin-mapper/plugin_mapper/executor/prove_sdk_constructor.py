"""The user's own UAD2SDK.dll, relocated, runs the original CHypParamBlock constructor."""
import hashlib,os
from pathlib import Path
import pefile
import pe_cache
from unicorn import UC_HOOK_CODE
from unicorn.x86_const import UC_X86_REG_RIP
from offline_host import BASE
# Set by plugin_mapper from the user's install (Windows\System32\UAD2SDK.dll).
SDK=Path(os.environ.get('OA_UAD_SDK','UAD2SDK.dll'))
# The SDK the pipeline was qualified with (UAD 11.8.3). Another version still runs;
# receipts record its hash so a difference is visible.
PIN='6ce5e63fc5153f215d4329c6b611ddabe148a28fe8d7989035e06c6c23c15cac'
SDKBASE=0x190000000;TRAPS=0x73000000
class OriginalSdkConstructor:
 def _map_image(self,image):
  super()._map_image(image);raw=SDK.read_bytes();self.sdk_sha256=hashlib.sha256(raw).hexdigest()
  p,im=pe_cache.from_bytes(self.sdk_sha256,raw);assert p.OPTIONAL_HEADER.ImageBase==BASE;p,im=pe_cache.from_bytes(self.sdk_sha256,raw,SDKBASE)
  self.uc.mem_map(SDKBASE,(len(im)+4095)&~4095);self.uc.mem_write(SDKBASE,im)
  self.uc.mem_map(TRAPS,0x10000);self.uc.mem_write(TRAPS,b'\xc3'*0x10000);self.uc.hook_add(UC_HOOK_CODE,self.hook,begin=TRAPS,end=TRAPS+0xffff)
  for n,imp in enumerate(i for d in p.DIRECTORY_ENTRY_IMPORT for i in d.imports):
   stub=TRAPS+n*16;assert stub<TRAPS+0x10000;self.wq(imp.address,stub);self.handlers[stub]=('unmodeled SDK import '+str(imp.name),None)
  exports={s.name:s.address for s in p.DIRECTORY_ENTRY_EXPORT.symbols};self.sdk_ctor=SDKBASE+exports[b'??0CHypParamBlock@@QEAA@XZ'];self.sdk_calls=0
 def hook(self,u,addr,size,unused):
  name,fn=self.handlers.get(addr,('',None))
  if name=='base_constructor_coefficient_fields':
   # Preserve original caller stack/registers, redirect only this named import.
   self.sdk_calls+=1;u.reg_write(UC_X86_REG_RIP,self.sdk_ctor);return
  return super().hook(u,addr,size,unused)
