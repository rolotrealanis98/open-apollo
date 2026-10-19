"""Static parent base-constructor pointer fields fed by rip-relative constant arguments.

The derived parent constructor calls the source-profiled base constructor with
`lea rdx/r8/r9, [rip+X]`; the base constructor spills its register arguments and
stores the spilled argument into `[this+off]`. Returns {off: X} or {} when the shape
differs anywhere (refuse rather than guess)."""
import functools
import pefile


@functools.lru_cache(maxsize=None)
def base_static_pointer_fields(path, parent_ctor_va, base_ctor_va):
    from capstone import Cs, CS_ARCH_X86, CS_MODE_64
    pe = pefile.PE(path, fast_load=True); image = pe.OPTIONAL_HEADER.ImageBase
    md = Cs(CS_ARCH_X86, CS_MODE_64)
    def decode(va, size, stop=None):
        out = []
        for ins in md.disasm(pe.get_data(va - image, size), va):
            out.append(ins)
            if ins.mnemonic == 'ret' or (stop and stop(ins)): break
        return out
    caller = decode(parent_ctor_va, 0x600, lambda i: i.mnemonic == 'call' and i.op_str == hex(base_ctor_va))
    if not caller or caller[-1].op_str != hex(base_ctor_va): return {}
    constants = {}
    for ins in reversed(caller[:-1]):
        if ins.mnemonic == 'call': break
        reg, _, src = ins.op_str.partition(', ')
        if reg in ('rdx', 'r8', 'r9') and reg not in constants:
            constants[reg] = (ins.address + ins.size + int(src[1:-1].replace('rip', '').replace(' ', ''), 0)
                              if ins.mnemonic == 'lea' and src.startswith('[rip ') else None)
    constants = {k: v for k, v in constants.items() if v is not None}
    if not constants: return {}
    body = decode(base_ctor_va, 0x200, lambda i: i.mnemonic == 'call')
    spills = {}; frame = None; this_slot = None
    for ins in body:
        dst, _, src = ins.op_str.partition(', ')
        if frame is None and ins.mnemonic == 'mov' and dst.startswith('qword ptr [rsp + ') and src in ('rcx', 'rdx', 'r8', 'r9'):
            spills[int(dst[17:-1], 0)] = src
        elif frame is None and ins.mnemonic == 'sub' and dst == 'rsp':
            frame = int(src, 0)
    if frame is None or spills.get(8) != 'rcx': return {}
    args = {off + frame: reg for off, reg in spills.items()}
    fields = {}; regs = {}
    for ins in decode(base_ctor_va, 0x400):
        dst, _, src = ins.op_str.partition(', ')
        if ins.mnemonic == 'mov' and src.startswith('qword ptr [rsp + ') and dst in ('rax', 'rcx', 'rdx'):
            regs[dst] = args.get(int(src[17:-1], 0))
        elif ins.mnemonic == 'mov' and dst.startswith('qword ptr [') and src in regs and regs[src] in constants:
            base, _, disp = dst[11:-1].partition(' + ')
            if regs.get(base) == 'rcx' and disp:
                fields[int(disp, 0)] = constants[regs[src]]
        elif ins.mnemonic == 'call':
            regs = {}
        elif dst in regs:
            regs.pop(dst)
    return fields


@functools.lru_cache(maxsize=None)
def base_stack_int_fields(path, parent_ctor_va, base_ctor_va):
    """Integer fields the base constructor stores from constant stack arguments of the derived
    parent constructor's call (`mov dword ptr [rsp+K], imm` before the call; in the base
    `mov ecx, [rsp+K+frame+8]` then `mov dword ptr [this+off], ecx`). Returns {off: imm}, {} if the
    shape differs (e.g. the framework input/output channel counts stored at +0x1d8/+0x1dc)."""
    from capstone import Cs, CS_ARCH_X86, CS_MODE_64
    from capstone.x86 import X86_OP_IMM
    pe = pefile.PE(path, fast_load=True); image = pe.OPTIONAL_HEADER.ImageBase
    md = Cs(CS_ARCH_X86, CS_MODE_64); md.detail = True
    def decode(va, size, stop=None):
        out = []
        for ins in md.disasm(pe.get_data(va - image, size), va):
            out.append(ins)
            if ins.mnemonic == 'ret' or (stop and stop(ins)): break
        return out
    caller = decode(parent_ctor_va, 0x600, lambda i: i.mnemonic == 'call' and i.op_str == hex(base_ctor_va))
    if not caller or caller[-1].op_str != hex(base_ctor_va): return {}
    args = {}
    for ins in reversed(caller[:-1]):
        if ins.mnemonic == 'call': break
        dst, _, src = ins.op_str.partition(', ')
        if ins.mnemonic == 'mov' and dst.startswith('dword ptr [rsp + ') and ins.operands[-1].type == X86_OP_IMM:
            args.setdefault(int(dst[17:-1], 0), ins.operands[-1].imm & 0xffffffff)
    body = decode(base_ctor_va, 0x400)
    frame = next((int(i.op_str.partition(', ')[2], 0) for i in body if i.mnemonic == 'sub' and i.op_str.startswith('rsp, ')), None)
    if frame is None or not args: return {}
    this_slot = 8 + frame; fields = {}; this_reg = set(); value = {}
    for ins in body:
        dst, _, src = ins.op_str.partition(', ')
        if ins.mnemonic == 'call': this_reg.clear(); value.clear()
        elif ins.mnemonic == 'mov' and src == 'qword ptr [rsp + %s]' % hex(this_slot) and dst in ('rax', 'rcx', 'rdx'):
            this_reg.add(dst); value.pop(dst, None)
        elif ins.mnemonic == 'mov' and src.startswith('dword ptr [rsp + ') and dst in ('eax', 'ecx', 'edx'):
            value[dst] = args.get(int(src[17:-1], 0) - frame - 8)
        elif ins.mnemonic == 'mov' and dst.startswith('dword ptr [') and value.get(src) is not None:
            base, _, disp = dst[11:-1].partition(' + ')
            if base in this_reg and disp: fields[int(disp, 0)] = value[src]
        elif dst in this_reg: this_reg.discard(dst)
        elif dst in value: value.pop(dst)
    return fields
