"""Static MSVC RTTI/PE discovery. Does not execute DLLs."""
from pathlib import Path
import pefile,struct,hashlib,os
ROOT=Path(os.environ.get('OA_PLUGIN_DLLS','UAD-2 Powered Plugins'))  # the user's own install, set by plugin_mapper
ROLES={8:'integer_setter',16:'integer_getter',24:'parse',32:'format',40:'notify',48:'notify_parent',56:'resume',64:'suspend',80:'load',96:'setup'}
def discover(path):
 raw=path.read_bytes();p=pefile.PE(data=raw);im=p.get_memory_mapped_image();base=p.OPTIONAL_HEADER.ImageBase
 u=lambda o:struct.unpack_from('<I',im,o)[0];q=lambda o:struct.unpack_from('<Q',im,o)[0]
 text=lambda r:im[r+16:im.index(b'\0',r+16)].decode()
 def bases(ch):
  num,arr=u(ch+8),u(ch+12)
  if not 1<=num<=128:raise ValueError('implausible hierarchy')
  return [text(u(u(arr+4*i)))for i in range(num)]
 executable=[(s.VirtualAddress,s.VirtualAddress+max(s.Misc_VirtualSize,s.SizeOfRawData))for s in p.sections if s.Characteristics&0x20000000]
 iscode=lambda va:any(a<=va-base<b for a,b in executable)
 functions={e.struct.BeginAddress:e.struct.EndAddress for e in p.DIRECTORY_ENTRY_EXCEPTION}
 imports={i.address:i.name.decode()if i.name else str(i.ordinal) for d in p.DIRECTORY_ENTRY_IMPORT for i in d.imports}
 found=[]
 for section in p.sections:
  if section.Characteristics&0x20000000:continue
  for r in range(section.VirtualAddress,section.VirtualAddress+section.Misc_VirtualSize-24,4):
   if u(r)!=1 or u(r+20)!=r:continue
   try:
    ty=text(u(r+12));bs=bases(u(r+16))
    if '.?AVCHypParamBlock@@' not in bs and ty!='.?AVPlugIn@@':continue
   except (ValueError,UnicodeError,struct.error,IndexError):continue
   needle=struct.pack('<Q',base+r);start=0
   while True:
    ref=im.find(needle,start)
    if ref<0:break
    start=ref+1;vt=ref+8
    if vt%8 or not iscode(q(vt)):continue
    slots={}
    for off in range(0,0x600,8):
     va=q(vt+off)
     if not iscode(va):break
     rva=va-base
     slot=dict(va=hex(va),function_end=hex(base+functions[rva])if rva in functions else None)
     if im[rva:rva+2]==b'\xff\x25':
      iat=va+6+struct.unpack_from('<i',im,rva+2)[0];slot['import']=imports.get(iat)
     if off in ROLES and '.?AVCHypParamBlock@@'in bs:slot['candidate_role']=ROLES[off]
     slots[hex(off)]=slot
    found.append(dict(type=ty,bases=bs,object_offset=u(r+4),vtable=hex(base+vt),rtti_locator=hex(base+r),slots=slots))
 return dict(name=path.stem,sha256=hashlib.sha256(raw).hexdigest(),vtables=found)
