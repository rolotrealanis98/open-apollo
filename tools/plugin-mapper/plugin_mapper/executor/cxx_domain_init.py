"""Pure C++ dynamic initializers that rewrite the original parameter-domain table.

Some UA plug-ins build per-parameter step counts at load time (`mov eax,[count]; dec eax;
mov [domain_table+4*i],eax` in a __xc_a..__xc_z initializer), so the static table bytes
understate the domain the original normalizer/setter/formatter run on. Only entries that
(1) sit in the unique MSVC C++ initializer table and (2) consist solely of straight-line
register/rip-relative-memory moves and arithmetic with at least one store into the domain table
are returned; nothing else in the table is selected.
"""
import re,struct
import pefile
from capstone import Cs,CS_ARCH_X86,CS_MODE_64
from capstone.x86 import X86_OP_MEM,X86_OP_REG,X86_OP_IMM

_PURE={'mov','dec','inc','add','sub','xor','nop'}
_LOOP=[('cmp','rcx, rdx'),('mov','rax, qword ptr [rbx]'),('test','rax, rax'),('add','rbx, 8'),('cmp','rbx, rdi')]

def _rip(i,o):return i.address+i.size+o.mem.disp

def domain_initializers(path,table_va,count):
    pe=pefile.PE(path,fast_load=True);pe.parse_data_directories(directories=[3])
    im=pe.get_memory_mapped_image();base=pe.OPTIONAL_HEADER.ImageBase
    cs=Cs(CS_ARCH_X86,CS_MODE_64);cs.detail=True
    funcs=sorted((e.struct.BeginAddress,e.struct.EndAddress)for e in pe.DIRECTORY_ENTRY_EXCEPTION)
    def func(va):
        r=va-base
        for a,b in funcs:
            if a<=r<b:return a+base,list(cs.disasm(im[a:b],base+a))
        raise ValueError('no PE function boundary')
    def executable(va):
        return any(s.VirtualAddress<=va-base<s.VirtualAddress+s.Misc_VirtualSize and s.Characteristics&0x20000000 for s in pe.sections)
    calls=[]
    for m in re.finditer(rb'\x48\x8d\x15....\x48\x8d\x0d....\xe8',im,re.DOTALL):
        r=m.start();ins=list(cs.disasm(im[r:r+19],base+r))
        if len(ins)!=3:continue
        z,a=_rip(ins[0],ins[0].operands[1]),_rip(ins[1],ins[1].operands[1])
        if not base<a<z<base+len(im) or (z-a)%8 or (z-a)//8>4096:continue
        try:
            loop=[(i.mnemonic,i.op_str)for i in func(ins[2].operands[0].imm)[1]];caller=func(base+r)[0]
        except ValueError:continue
        if all(x in loop for x in _LOOP):
            calls.append(dict(site=base+r,start=a,end=z,returns_int=('test','eax, eax')in loop,caller=caller))
    xc=[c for c in calls if not c['returns_int'] and any(e['returns_int'] and e['caller']==c['caller'] and e['site']<c['site'] for e in calls)]
    if len(xc)!=1:return []
    xc=xc[0];lo,hi=table_va,table_va+4*count;out=[]
    for k,va in enumerate(struct.unpack('<'+'Q'*((xc['end']-xc['start'])//8),im[xc['start']-base:xc['end']-base])):
        if not va or not executable(va):continue
        body=[]
        for i in cs.disasm(im[va-base:va-base+0x80],va):
            body.append(i)
            if i.mnemonic in('ret','jmp','int3'):break
        if not body or body[-1].mnemonic!='ret':continue
        stores=0;pure=True
        for i in body[:-1]:
            if i.mnemonic not in _PURE:pure=False;break
            for o in i.operands:
                if o.type==X86_OP_MEM and (o.mem.index or i.reg_name(o.mem.base)!='rip'):pure=False
            if i.mnemonic in('sub','add') and i.op_str.startswith('rsp,'):continue
            if i.mnemonic=='mov' and i.operands[0].type==X86_OP_MEM and lo<=_rip(i,i.operands[0])<hi:stores+=1
        if pure and stores:out.append(dict(ordinal=k,table_slot=xc['start']+8*k,va=va,stores=stores))
    return out
