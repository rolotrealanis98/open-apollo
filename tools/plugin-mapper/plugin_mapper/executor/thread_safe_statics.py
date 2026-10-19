"""Locate the original MSVC thread-safe-statics startup initializer in a statically linked CRT.

It is the one function that references the string SleepConditionVariableCS and is listed exactly once
as a pointer in a non-code section (the __xc table). Nothing is selected if that is not unique."""
import re,struct
import pefile

def find_initializer(path):
    pe=pefile.PE(path,fast_load=True);pe.parse_data_directories(directories=[3])
    im=pe.get_memory_mapped_image();base=pe.OPTIONAL_HEADER.ImageBase
    needle=b'SleepConditionVariableCS\0'
    strings=[m.start() for m in re.finditer(re.escape(needle),im)]
    funcs=sorted((e.struct.BeginAddress,e.struct.EndAddress)for e in pe.DIRECTORY_ENTRY_EXCEPTION)
    found=set()
    for rva_text in strings:
        for m in re.finditer(rb'[\x48\x4c]\x8d[\x05\x0d\x15\x1d\x25\x2d\x35\x3d]....',im,re.DOTALL):
            end=m.start()+7
            if end+struct.unpack('<i',m.group()[3:7])[0]!=rva_text:continue
            fn=[a for a,b in funcs if a<=m.start()<b]
            if fn:found.add(fn[0])
    if len(found)!=1:return None
    va=base+found.pop();ptr=struct.pack('<Q',va)
    code=[(s.VirtualAddress,s.VirtualAddress+s.Misc_VirtualSize)for s in pe.sections if s.Characteristics&0x20000000]
    refs=[m.start()for m in re.finditer(re.escape(ptr),im)if not any(a<=m.start()<b for a,b in code)]
    return va if len(refs)==1 else None
