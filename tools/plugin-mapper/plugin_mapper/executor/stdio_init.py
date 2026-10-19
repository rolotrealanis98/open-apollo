"""Locate the original statically linked UCRT stdio initializer (.CRT$XIC pre-C initializer).

It gives each std stream (0x58-byte FILE, CRITICAL_SECTION at +0x30) its lock: lea rcx,[reg+0x30]; xor r8d,r8d;
mov edx,4000; call. Exactly one function holds that sequence and is listed exactly once in a non-code table.
Nothing is selected otherwise. Original code that printf()s needs these locks."""
import re,struct
import pefile

PATTERN=re.compile(rb"[\x48-\x4f]\x8d[\x40-\x7f]\x30\x45\x33\xc0\xba\xa0\x0f\x00\x00\xe8",re.DOTALL)

def find_initializer(path):
    pe=pefile.PE(path,fast_load=True);pe.parse_data_directories(directories=[3])
    im=pe.get_memory_mapped_image();base=pe.OPTIONAL_HEADER.ImageBase
    funcs=sorted((e.struct.BeginAddress,e.struct.EndAddress)for e in pe.DIRECTORY_ENTRY_EXCEPTION)
    found={a for m in PATTERN.finditer(im)for a,b in funcs if a<=m.start()<b}
    if len(found)!=1:return None
    va=base+found.pop();ptr=struct.pack("<Q",va)
    code=[(s.VirtualAddress,s.VirtualAddress+s.Misc_VirtualSize)for s in pe.sections if s.Characteristics&0x20000000]
    refs=[m.start()for m in re.finditer(re.escape(ptr),im)if not any(a<=m.start()<b for a,b in code)]
    return va if len(refs)==1 else None
