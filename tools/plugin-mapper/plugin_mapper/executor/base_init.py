"""Original base-class init allocation prefix (structurally detected; no per-plug-in data).

Some parent classes allocate per-program storage (arrays of program-name / parameter-value buffers
at fields of the parent) in an init function that the derived parent constructor calls right after
the base constructor. That init ends with the same virtual sequence the host already performs
(parent slots 0x578 then 0x538) but cannot run whole offline (hardware-cache and process calls).

Only the allocation prefix is executed here, from the original code:
  1. Scalar fields the derived constructor passes to the base constructor as stack immediates and
     the base constructor stores unconditionally (e.g. program and parameter counts) are written.
  2. The init runs from its entry up to its first parent virtual call whose result is consumed.
     Earlier parent virtual calls are void side-effect calls (result registers dead) on slots the
     host does not model; they are elided and recorded.
Applied only when the default program-state init (slot 0x538) reads a field the prefix allocates.
Anything not matching the exact shape returns None and the original failure stands."""
import struct
import pefile
from capstone import Cs,CS_ARCH_X86,CS_MODE_64
from capstone.x86 import X86_OP_MEM,X86_OP_IMM,X86_OP_REG
from unicorn import UC_HOOK_CODE,UcError
from unicorn.x86_const import *
from offline_host import BASE,STACK,STUBS,STOP

_RAX={'rax','eax','ax','al','ah'}


def _md():
    md=Cs(CS_ARCH_X86,CS_MODE_64);md.detail=True;return md


def _pe(path):
    pe=pefile.PE(path,fast_load=True);pe.parse_data_directories(directories=[3]);return pe


def _function(pe,va):
    base=pe.OPTIONAL_HEADER.ImageBase
    for f in pe.DIRECTORY_ENTRY_EXCEPTION:
        s,e=base+f.struct.BeginAddress,base+f.struct.EndAddress
        if s<=va<e:return s,e
    return None


def _insns(pe,s,e):
    return list(_md().disasm(pe.get_data(s-pe.OPTIONAL_HEADER.ImageBase,e-s),s))


def _is_vcall(ins):
    if ins.mnemonic!='call' or not ins.operands or ins.operands[0].type!=X86_OP_MEM:return None
    op=ins.operands[0].mem
    return op.disp if not op.index and op.disp%8==0 and ins.reg_name(op.base) not in('rsp','rip') else None


def _result_ignored(insns,k):
    """The integer result (rax) is overwritten without being read and xmm0 is not read, within the linear
    window after insns[k] (ends at the next call or branch)."""
    rax_dead=False
    for ins in insns[k+1:k+13]:
        if ins.mnemonic=='call':return True
        read,written=ins.regs_access()
        read={'rax' if ins.reg_name(r) in _RAX else ins.reg_name(r) for r in read}
        if 'xmm0' in read or ('rax' in read and not rax_dead):return False
        if any(ins.reg_name(r) in('rax','eax') for r in written):rax_dead=True
        if ins.mnemonic in('ret','jmp') or ins.mnemonic.startswith('j'):return rax_dead and ins.mnemonic!='ret'
    return rax_dead


def base_ctor_scalar_fields(path,parent_ctor,base_ctor):
    """{field offset: 32-bit immediate} stored unconditionally by the base constructor from stack arguments
    that the derived constructor passes as immediates."""
    pe=_pe(path);fn=_function(pe,parent_ctor)
    if fn is None:return {}
    caller=_insns(pe,*fn);site=next((k for k,i in enumerate(caller) if i.mnemonic=='call' and i.op_str==hex(base_ctor)),None)
    if site is None:return {}
    args={}
    for ins in reversed(caller[:site]):
        if ins.mnemonic=='call':break
        if ins.mnemonic=='mov' and len(ins.operands)==2 and ins.operands[0].type==X86_OP_MEM and ins.operands[0].size==4 \
           and ins.reg_name(ins.operands[0].mem.base)=='rsp' and ins.operands[1].type==X86_OP_IMM and ins.operands[0].mem.disp>=0x20:
            args.setdefault(ins.operands[0].mem.disp,ins.operands[1].imm&0xffffffff)
    fn=_function(pe,base_ctor)
    if fn is None:return {}
    body=_insns(pe,*fn)
    frame=next((int(i.op_str.split(', ')[1],0) for i in body if i.mnemonic=='sub' and i.op_str.startswith('rsp, ')),None)
    if frame is None:return {}
    regs={};fields={};this_slot=None
    for ins in body:
        if ins.mnemonic=='mov' and ins.op_str.startswith('qword ptr [rsp + ') and ins.op_str.endswith(', rcx') and this_slot is None:
            this_slot=int(ins.op_str.split('[rsp + ')[1].split(']')[0],0)+frame
        if ins.mnemonic=='ret' or ins.mnemonic.startswith('j'):break   # unconditional straight-line prefix only
        if ins.mnemonic=='call':regs.clear();continue
        if len(ins.operands)!=2:
            for r in ins.regs_access()[1]:regs.pop(ins.reg_name(r),None)
            continue
        dst,src=ins.operands
        if ins.mnemonic=='mov' and dst.type==X86_OP_REG and src.type==X86_OP_MEM and ins.reg_name(src.mem.base)=='rsp' and not src.mem.index:
            name=ins.reg_name(dst.reg);d=src.mem.disp
            if src.size==8 and d==this_slot:regs[name]='this'
            elif src.size==4 and d-frame-8 in args:regs[name]=args[d-frame-8]
            else:regs.pop(name,None)
        elif ins.mnemonic=='mov' and dst.type==X86_OP_MEM and dst.size==4 and src.type==X86_OP_REG \
             and regs.get(ins.reg_name(dst.mem.base))=='this' and isinstance(regs.get(ins.reg_name(src.reg)),int):
            fields[dst.mem.disp]=regs[ins.reg_name(src.reg)]
        elif dst.type==X86_OP_REG:regs.pop(ins.reg_name(dst.reg),None)
    return fields


def find_init(path,parent_ctor,base_ctor):
    """The unique function the derived constructor calls with `this` after its base-constructor call that
    itself virtually dispatches parent slots 0x578 and 0x538 (the framework open/default-state sequence)."""
    pe=_pe(path);fn=_function(pe,parent_ctor)
    if fn is None:return None
    code=_insns(pe,*fn);site=next((k for k,i in enumerate(code) if i.mnemonic=='call' and i.op_str==hex(base_ctor)),None)
    if site is None:return None
    holders={'rcx'};found=set()
    for ins in code[:site]:
        reg=ins.op_str.split(',')[0]
        if ins.mnemonic=='mov' and ins.op_str.endswith(', rcx') and reg in('rbx','rsi','rdi','rbp','r12','r13','r14','r15'):holders.add(reg)
    for k in range(site+1,len(code)):
        ins=code[k]
        if ins.mnemonic=='call' and ins.op_str.startswith('0x') and code[k-1].mnemonic=='mov' and code[k-1].op_str.startswith('rcx, ') \
           and code[k-1].op_str[5:] in holders:
            target=int(ins.op_str,16);tf=_function(pe,target)
            if tf is None or tf[0]!=target:continue
            slots={_is_vcall(i)for i in _insns(pe,*tf)}
            if 0x578 in slots and 0x538 in slots:found.add(target)
    return found.pop() if len(found)==1 else None


def plan(path,init):
    """(stop_va, elided_slots, allocated_fields) for the init's allocation prefix, or None."""
    pe=_pe(path);code=_insns(pe,*_function(pe,init))
    elided=set();stop=None
    for k,ins in enumerate(code):
        slot=_is_vcall(ins)
        if slot is None:continue
        if _result_ignored(code,k):elided.add(slot)
        else:stop=ins.address;break
    if stop is None:return None
    fields=set()
    for ins in code:
        if ins.address>=stop:break
        if ins.mnemonic=='mov' and len(ins.operands)==2 and ins.operands[0].type==X86_OP_MEM and ins.operands[0].size==8 \
           and ins.operands[1].type==X86_OP_REG and ins.reg_name(ins.operands[0].mem.base)not in('rsp','rip') and ins.operands[0].mem.disp>=0x100:
            fields.add(ins.operands[0].mem.disp)
    return stop,elided,fields


def reads_fields(path,va,end,fields):
    code=_insns(_pe(path),va,end)
    return sorted({o.mem.disp for i in code for o in i.operands if o.type==X86_OP_MEM and o.mem.disp in fields and not o.mem.index
                   and i.reg_name(o.mem.base) not in('rsp','rip')})


def apply(cap,profile):
    """Run the original init's allocation prefix when slot 0x538 needs what it allocates. Returns evidence or None."""
    path=profile['dll']['path']
    constructor=profile['parameter_block']['constructor']
    parent_ctor=constructor.get('parent_constructor_va',constructor.get('va'))
    base_ctor=profile['parent'].get('constructor_base');state=profile['parent']['slots'].get('0x538')
    if not (parent_ctor and base_ctor and isinstance(state,dict) and state.get('function_end')):return None
    parent_ctor,base_ctor=int(str(parent_ctor),0),int(str(base_ctor),0)
    init=find_init(path,parent_ctor,base_ctor)
    shape=plan(path,init) if init is not None else None
    if shape is None:return None
    stop,elided,fields=shape
    needed=reads_fields(path,int(state['va'],0),int(state['function_end'],0),fields)
    if not needed:return None
    scalars=base_ctor_scalar_fields(path,parent_ctor,base_ctor)
    for off,value in scalars.items():
        if 8<=off<0x10000 and struct.unpack('<I',bytes(cap.uc.mem_read(cap.parent+off,4)))[0]==0:cap.wd(cap.parent+off,value)
    cap.phase='original_base_init_allocation_prefix'
    saved={}
    for slot in elided:
        stub=STUBS+0x2000+(slot//8)*16
        if 0<slot<0xc0*8 and cap.rq(cap.parentvt+slot)==stub and cap.handlers.get(stub,('',))[0].startswith('unknown parent'):
            saved[stub]=cap.handlers[stub];cap.handlers[stub]=('elided_void_parent_call '+hex(slot),lambda:0)
    stopper=cap.uc.hook_add(UC_HOOK_CODE,lambda u,a,s,d:u.emu_stop(),begin=stop,end=stop)
    try:
        cap.failure=None;rsp=STACK+0xff008;cap.wq(rsp,STOP)
        cap.uc.reg_write(UC_X86_REG_RCX,cap.parent);cap.uc.reg_write(UC_X86_REG_RSP,rsp)
        try:cap.uc.emu_start(init,STOP,count=20000000)
        except UcError as exc:raise RuntimeError('emulator at '+hex(cap.uc.reg_read(UC_X86_REG_RIP))+': '+str(exc))from exc
        if cap.failure:raise RuntimeError(str(cap.failure))from cap.failure
        if cap.uc.reg_read(UC_X86_REG_RIP)!=stop:raise RuntimeError('original base init allocation prefix did not reach its stop')
    finally:
        cap.uc.hook_del(stopper)
        cap.handlers.update(saved)
    missing=[hex(f) for f in needed if cap.rq(cap.parent+f)==0]
    if missing:raise RuntimeError('original base init left needed fields unallocated '+repr(missing))
    return dict(init=hex(init),stop=hex(stop),elided_void_slots=sorted(hex(s)for s in elided),
                scalar_fields={hex(k):v for k,v in scalars.items()},allocated_fields=sorted(hex(f)for f in fields),needed=[hex(f)for f in needed])


def lazy_slot_target(cap,addr):
    """record_copy_chunk program-loader family: the original program-state/readback/record code dispatches
    further parent slots (e.g. current-program and record accessors) that the host does not model. A reached
    unmodeled parent slot whose original function range the profile attests is bound to that original
    function (persistently, so later calls dispatch directly); everything else still rejects."""
    # A profile with the source-proven sample-rate context also runs its originals with fs set (generic_capture
    # writes parent+0x1e0), which takes fs-dependent branches that dispatch more attested parent slots.
    if (cap.profile.get('startup',{}).get('program_loader',{}).get('kind')!='record_copy_chunk' and getattr(cap,'phase','')!='original_program_state_init'
            and cap.profile.get('host_context',{}).get('sample_rate') is None):return None
    base=STUBS+0x2000
    if not base<=addr<base+0xc0*16 or (addr-base)%16:return None
    slot=(addr-base)//16*8
    spec=cap.profile['parent']['slots'].get(hex(slot))
    if not isinstance(spec,dict) or 'va' not in spec:return None
    # function_end null: a jump-table leaf without unwind info; bounded by the image only.
    va=int(str(spec['va']),0);end=int(str(spec['function_end']),0) if spec.get('function_end') else va
    if not BASE<=va<=end<=BASE+0x4000000:return None
    cap.wq(cap.parentvt+slot,va)
    cap.lazy_bound_slots=getattr(cap,'lazy_bound_slots',[])+[dict(slot=hex(slot),va=hex(va),function_end=hex(end))]
    return va


def loader_extra_args(spec):
    """record_copy_chunk loaders are the original set_chunk(parent, chunk, unused, preset=1): copy the chunk
    into the current program record, then apply it through the original set_parameter. Other loaders: none."""
    return (0,1) if spec.get('kind')=='record_copy_chunk' else ()


# --- record_copy_chunk program load ------------------------------------------------------------------
# The original set_chunk(parent, chunk, _, preset) only copies the chunk into its program record and then
# tail-dispatches the original apply-program slot, which feeds the per-program storage floats (allocated by
# the base init) to the original set_parameter. The chunk never reaches the parameters, so the captured
# vector enters the same way the original does: its floats are stored where the apply slot reads them,
# and the original apply slot runs. Where each parameter is read from is recovered by tagging every
# storage float and tracing the (index, value) set_parameter calls of the original apply slot.

def apply_slot(cap,spec):
    """The parent slot the original loader tail-jumps to (`jmp [reg+S]`); unique or None."""
    pe=_pe(cap.profile['dll']['path']);va=int(str(spec['va']),0);body=[]
    for ins in _md().disasm(pe.get_data(va-pe.OPTIONAL_HEADER.ImageBase,0x600),va):   # to the int3 padding
        if ins.mnemonic=='int3':break
        body.append(ins)
    slots={i.operands[0].mem.disp for i in body if i.mnemonic=='jmp' and i.operands and i.operands[0].type==X86_OP_MEM
           and not i.operands[0].mem.index and i.reg_name(i.operands[0].mem.base) not in('rip','rsp') and i.operands[0].mem.disp%8==0}
    return slots.pop() if len(slots)==1 else None


def _storage(cap):
    needed=(getattr(cap,'base_init_prefix',None) or {}).get('needed') or []
    if len(needed)!=1:return None
    field=int(needed[0],16);arr=cap.rq(cap.parent+field)
    if arr not in cap.allocations or cap.allocations[arr]%8:return None
    rows=cap.allocations[arr]//8
    if not rows or any(cap.rq(arr+8*p) not in cap.allocations for p in range(rows)):return None
    return field,arr,rows


def recover_storage_layout(cap,spec):
    """{'storage': ..., 'fields': {index: (row, byte offset)}, 'derived': (...)}; row is 'current' or 'first'."""
    slot=apply_slot(cap,spec);source=cap.profile['parent']['slots'].get(hex(slot)) if slot is not None else None
    if source is None or not source.get('function_end') or _storage(cap) is None:return None
    probe=type(cap)(cap.profile,auto_start=False)
    field,arr,rows=_storage(probe);index_field=int(spec['get_chunk']['program_index_field'])
    prog=struct.unpack('<i',bytes(probe.uc.mem_read(probe.parent+index_field,4)))[0]
    if not 0<=prog<rows:return None
    tags={};n=len(probe.maxima)
    for p in range(rows):
        row=probe.rq(arr+8*p)
        for k in range(probe.allocations[row]//4):
            v=2.0+len(tags)/4096.;probe.wf(row+4*k,v);tags[struct.unpack('<I',struct.pack('<f',v))[0]]=(p,4*k)
    calls=[];stub=STUBS+0xe000
    def traced():
        calls.append((probe.arg(1)&0xffffffff,tags.get(probe.uc.reg_read(UC_X86_REG_XMM2)&0xffffffff)));return 0
    probe.handlers[stub]=('source_program_storage_setter',traced);probe.wq(probe.parentvt+0x50,stub)
    probe.wd(probe.parent+index_field,0xffffffff)
    probe.call(int(str(source['va']),0),probe.parent,prog)
    seen={}
    for index,tag in calls:
        if index<n:seen.setdefault(index,set()).add(tag)
    fields={}
    for index,tagset in seen.items():
        if len(tagset)!=1 or None in tagset:continue          # value computed by the original apply code
        p,off=next(iter(tagset))
        if p==prog:fields[index]=('current',off)
        elif p==0:fields[index]=('first',off)
        else:return None
    if not fields:return None
    derived=tuple(i for i in range(n) if i not in fields)
    return dict(size=0,fields=fields,derived=derived,unloaded=tuple(i for i in range(n) if i not in seen),
                storage=dict(field=hex(field),program=prog,apply_slot=hex(slot),apply_va=source['va'],program_index_field=index_field))


def apply_storage_program(cap,spec,layout,props):
    st=layout['storage'];field=int(st['field'],16);arr=cap.rq(cap.parent+field)
    rows={'current':cap.rq(arr+8*st['program']),'first':cap.rq(arr)}
    for index,(row,off) in layout['fields'].items():cap.wf(rows[row]+off,props[index]['NormalizedValue']['value'])
    parent=cap.profile['parent'];normalizer=parent['normalizer']
    cap.wq(cap.parentvt+0x50,int(str(parent['set_parameter']['va']),0))
    cap.wq(cap.parentvt+int(str(normalizer['slot']),0),int(str(normalizer['va']),0))
    cap.wd(cap.parent+st['program_index_field'],0xffffffff)
    va=int(str(st['apply_va']),0);cap.phase='original_captured_program_load';cap.watch_program_loader(va)
    cap.call(va,cap.parent,st['program'])


# --- record_copy_chunk loaders whose apply slot copies the chunk straight into parameter state --------------
# No set_parameter is traced here, so the chunk layout is recovered by single-word perturbation of the
# original loader. From a version-1 chunk holding the plug-in's own defaults for every word the loader copies
# verbatim, each word is set to a float tag and, separately, to an integer tag; a word that changes exactly
# one parameter's original getter is that parameter's chunk field. Some plug-ins double-buffer their state:
# the loader writes the live copy and a staged copy that a later parameter update commits over it. The
# perturbation is therefore also observed after one such update (the first PB setter that disturbs another
# parameter after a load), and words that matter only then are recorded as extra fields of that parameter.
# A field is accepted only if it reads back unchanged (a float as the original normalizer of the written
# float, an integer as itself); the loaded vector is then checked against the captured vector by the
# executor's normal original-normalizer/getter comparison.

def recover_chunk_layout(cap,spec):
    if loader_extra_args(spec)==():return None
    size=int(spec['record_size']);words=size//4;n=len(cap.maxima)
    parent=cap.profile['parent'];norm=parent['normalizer'];va=int(str(spec['va']),0)
    def fresh():
        probe=type(cap)(cap.profile,auto_start=False)
        probe.wq(probe.parentvt+0x50,int(str(parent['set_parameter']['va']),0))
        probe.wq(probe.parentvt+int(str(norm['slot']),0),int(str(norm['va']),0))
        return probe
    probe=fresh()
    # Words the loader copies verbatim into the parent keep the plug-in's own default there: tag every word with
    # two different integer patterns, find where each lands in the parent, and take the pre-load value.
    span=probe.parent_span();before=bytes(probe.uc.mem_read(probe.parent,span));landing={}
    for rnd,(mul,add) in enumerate(((3,1000),(7,5000))):
        ptr=probe.reserve(size+64);probe.uc.mem_write(ptr,b'\0'*(size+64));probe.wd(ptr,int(spec['program_version']))
        for w in range(1,words):probe.wd(ptr+4*w,add+mul*w)
        probe.call(va,probe.parent,ptr,*loader_extra_args(spec))
        after=bytes(probe.uc.mem_read(probe.parent,span));tag={add+mul*w:w for w in range(1,words)}
        for off in range(0,span-3,4):
            w=tag.get(struct.unpack_from('<I',after,off)[0])
            if w is not None:landing.setdefault(w,[set(),set()])[rnd].add(off)
    baseline={}
    for w,(first,second) in landing.items():
        defaults={struct.unpack_from('<I',before,off)[0] for off in first&second}-{0}     # zero default needs no baseline
        if len(defaults)==1:baseline[4*w]=defaults.pop()
    probe=fresh()
    def getters():return tuple(probe.vcall(0x10,i)&0xffffffff for i in range(n))
    def load(word=None,kind=None,value=None):
        ptr=probe.reserve(size+64);probe.uc.mem_write(ptr,b'\0'*(size+64));probe.wd(ptr,int(spec['program_version']))
        for off,raw in baseline.items():probe.wd(ptr+off,raw)
        if word is not None:
            if kind=='f':probe.wf(ptr+4*word,0.9 if value is None else value)
            else:probe.wd(ptr+4*word,1 if value is None else value)
        probe.call(va,probe.parent,ptr,*loader_extra_args(spec))
    def normalized(index,value):
        probe.uc.reg_write(UC_X86_REG_XMM2,struct.unpack('<I',struct.pack('<f',value))[0])
        return probe.call(int(str(norm['va']),0),probe.parent,index)&0xffffffff
    def run(word=None,kind=None,value=None,trigger=None):
        load(word,kind,value);pre=getters()
        if trigger is None:return pre,None
        cur=pre[trigger];probe.vcall(8,trigger,cur+1 if cur<cap.maxima[trigger] else cur-1,0)
        return pre,getters()
    basepre,_=run();owners={}
    def survey(trigger,reference):
        for word in range(1,words):
            for kind in('f','i'):
                pre,post=run(word,kind,trigger=trigger)
                got,ref,phase=(pre,basepre,0) if trigger is None else (post,reference,1)
                changed=[i for i,(a,b) in enumerate(zip(ref,got)) if a!=b and i!=trigger]
                if len(changed)==1:owners.setdefault(changed[0],set()).add((word,kind,phase))
    survey(None,None)
    # The parameter update that commits staged state: loading a chunk that differs from the defaults in every
    # field found so far (so live and staged copies disagree), the PB setter that disturbs the most others.
    provisional=sorted({(w,k) for found in owners.values() for w,k,_ in found})
    trigger=None;best=0
    def load_with(items):
        ptr=probe.reserve(size+64);probe.uc.mem_write(ptr,b'\0'*(size+64));probe.wd(ptr,int(spec['program_version']))
        for off,raw in baseline.items():probe.wd(ptr+off,raw)
        for w,k in items:
            if k=='f':probe.wf(ptr+4*w,0.9)
            else:probe.wd(ptr+4*w,1)
        probe.call(va,probe.parent,ptr,*loader_extra_args(spec))
    load_with(provisional);loaded=getters()
    for t in range(n):
        load_with(provisional);cur=getters()[t];probe.vcall(8,t,cur+1 if cur<cap.maxima[t] else cur-1,0)
        moved=sum(g!=l for i,(g,l) in enumerate(zip(getters(),loaded)) if i!=t and i in owners)    # loaded fields reverted
        if moved>best:trigger,best=t,moved
    if trigger is not None:
        basepost=run(trigger=trigger)[1];survey(trigger,basepost)
    fields={};int_fields={};extra=[]
    for index,found in owners.items():
        valid=[]
        for word,kind,phase in sorted(found):
            vals=(0.3,0.8) if kind=='f' else sorted({1,min(int(cap.maxima[index]),32767)})
            ok=True
            for v in vals:
                got=run(word,kind,v,trigger=trigger if phase else None)[phase][index]
                if got!=(normalized(index,v) if kind=='f' else v):ok=False;break
            if ok:valid.append((word,kind))
        valid=sorted(set(valid))
        if not valid:continue
        kinds={k for _,k in valid}
        if len(kinds)!=1:continue
        kind=kinds.pop();(fields if kind=='f' else int_fields)[index]=4*valid[0][0]
        extra+=[(index,4*w,kind) for w,_ in valid[1:]]
    if not fields and not int_fields:return None
    done=set(fields)|set(int_fields);used=set(fields.values())|set(int_fields.values())|{o for _,o,_ in extra}
    return dict(size=size+64,fields=fields,int_fields=int_fields,extra_words=extra,
                baseline={o:v for o,v in baseline.items() if o not in used},
                derived=tuple(i for i in range(n) if i not in done),commit_trigger=trigger)


def write_int_fields(cap,ptr,layout,props):
    """Integer chunk fields hold the original-normalizer domain integer of the captured value; further words
    of a parameter (staged copies) take the same value in their own kind."""
    from unicorn.x86_const import UC_X86_REG_XMM2
    norm=cap.profile['parent']['normalizer']
    for offset,raw in layout.get('baseline',{}).items():cap.wd(ptr+offset,raw)
    def integer_value(index):
        bits=struct.unpack('<I',struct.pack('<f',props[index]['NormalizedValue']['value']))[0]
        cap.uc.reg_write(UC_X86_REG_XMM2,bits)
        return cap.call(int(str(norm['va']),0),cap.parent,index)&0xffffffff
    for index,offset in layout.get('int_fields',{}).items():cap.wd(ptr+offset,integer_value(index))
    for index,offset,kind in layout.get('extra_words',()):
        if kind=='f':cap.wf(ptr+offset,props[index]['NormalizedValue']['value'])
        else:cap.wd(ptr+offset,integer_value(index))


def apply_derived_parameters(cap,layout,props):
    """After the chunk copy, every parameter is applied through the original set_parameter with its captured
    normalized value (the call the original apply code makes), so the master state that later parameter
    updates re-derive from agrees with the cached state the chunk copy wrote."""
    from unicorn.x86_const import UC_X86_REG_XMM2
    va=int(str(cap.profile['parent']['set_parameter']['va']),0)
    for index in range(len(cap.maxima)):
        cap.uc.reg_write(UC_X86_REG_XMM2,struct.unpack('<I',struct.pack('<f',props[index]['NormalizedValue']['value']))[0])
        cap.call(va,cap.parent,index)


# --- staged-state settling for record_copy_chunk loaders with a commit trigger -----------------------------
# The loader writes the live copy of every parameter (and the staged words it can attribute to a chunk field),
# but the staged copy that a commit-trigger update re-applies over the live state keeps its constructor values
# for parameters the chunk does not carry verbatim (derived ones). The original plug-in never sees that: it
# reaches any loaded state through its public setter, which writes both copies. Whether the loaded state
# survives its own commit trigger is probed on a throw-away instance; only when it does not, every other
# parameter is driven through the original public setter (away from its loaded value and back) so both agree.

def _roundtrip(cap, index, alt, back):
    cap.set_control(index, alt); cap.set_control(index, back)


def _other_value(value, maximum):
    return value + 1 if value < maximum else value - 1


def commit_trigger_loses_state(cap):
    """True when toggling the loaded program's commit trigger (and back) on a fresh instance rejects or changes the vector."""
    trigger = (getattr(cap, 'program_layout', None) or {}).get('commit_trigger')
    if trigger is None:
        return False
    probe = type(cap)(cap.profile, auto_start=False); probe.settling = True; probe.start()
    loaded = probe.read_values()
    try:
        _roundtrip(probe, trigger, _other_value(loaded[trigger], probe.maxima[trigger]), loaded[trigger])
    except (ValueError, RuntimeError):
        return True
    return probe.read_values() != loaded


def settle_commit_state(cap):
    """Make the staged copies agree with the loaded vector (see above); refuses if the vector cannot be kept."""
    if getattr(cap, 'settling', False) or not commit_trigger_loses_state(cap):
        return False
    trigger = cap.program_layout['commit_trigger']; loaded = cap.read_values(); maxima = cap.maxima
    cap.phase = 'staged_state_settle'
    for index in range(len(maxima)):
        if index == trigger:
            continue
        quarter = round(.25 * maxima[index])
        _roundtrip(cap, index, quarter if quarter != loaded[index] else round(.75 * maxima[index]), loaded[index])
    if cap.read_values() != loaded:
        raise ValueError('staged-state settling changed the loaded vector')
    _roundtrip(cap, trigger, _other_value(loaded[trigger], maxima[trigger]), loaded[trigger])
    if cap.read_values() != loaded:
        raise ValueError('commit trigger still changes the settled vector')
    cap.staged_state_settled = True
    return True
