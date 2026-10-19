"""Generated-profile Win64 CHyp executor. No plugin names, RVAs, device or OS loader.

Only the shared cache/notification/channel ABI is modeled. Unknown calls reject.
Original constructors, setters, getters and formatters execute under Unicorn.
"""
from pathlib import Path
import sys, json, struct, hashlib
HERE=Path(__file__).resolve().parent
sys.path.insert(0,str(HERE))
from offline_host import OfflineHost,BASE,HEAP,STACK,STUBS,STOP,_ARGUMENTS
from capture_pultec import CapturePultec
from prove_sdk_constructor import OriginalSdkConstructor,SDK,SDKBASE
from probe_transactions import Replay
import pefile
from unicorn import UcError,UC_HOOK_BLOCK,UC_HOOK_CODE,UC_HOOK_MEM_READ
from unicorn.x86_const import *


def integer(value):
    return int(value,0) if isinstance(value,str) else int(value)


PARENT_SLOT=0x100000          # fixed parent fixture span below the host interface fixtures
LARGE_PARENT=0x40000000       # dedicated mapping for larger original allocations
HEAP_OVERFLOW=0x48000000      # private-allocation arena after the primary 4MB
HEAP_OVERFLOW_SIZE=0x4000000


import functools
@functools.lru_cache(maxsize=None)
def parent_allocation_size(path,ctor_va,embedded_offset=None):
    """Original factory allocation for the whole parent object, or None.

    Either the source parent constructor is itself the factory (`mov ecx, imm32; call
    allocator` whose rax, before any other call, yields the PB via `lea r, [rax +
    embedded_offset]`), or exactly one direct `call ctor` exists in the image, preceded
    in the same linear block by `mov ecx, imm32; call allocator` whose rax reaches rcx
    via `mov rcx, rax` with neither register rewritten before the constructor call."""
    inline=_inline_factory_size(path,ctor_va,embedded_offset)
    if inline is not None:return inline
    import numpy as np
    from capstone import Cs,CS_ARCH_X86,CS_MODE_64
    pe=pefile.PE(path,fast_load=True);base=pe.OPTIONAL_HEADER.ImageBase;sites=[]
    for section in pe.sections:
        if not section.Characteristics&0x20000000:continue
        data=section.get_data();va=base+section.VirtualAddress
        raw=np.frombuffer(data,np.uint8)
        if len(raw)<5:continue
        at=np.nonzero(raw[:-4]==0xe8)[0]
        rel=(raw[at+1].astype(np.int64)|raw[at+2].astype(np.int64)<<8|raw[at+3].astype(np.int64)<<16|raw[at+4].astype(np.int64)<<24)
        rel=np.where(rel>=1<<31,rel-(1<<32),rel)
        sites+=[(va+int(i),data,va)for i in at[(va+at+5+rel)==ctor_va]]
    if len(sites)!=1:return None
    site,data,va=sites[0];md=Cs(CS_ARCH_X86,CS_MODE_64);md.detail=True
    rax={'rax','eax','ax','al','ah'};rcx={'rcx','ecx','cx','cl','ch'}
    for back in range(10,0x80):
        start=site-back
        if start<va or data[start-va]!=0xb9:continue
        insns=list(md.disasm(data[start-va:site-va+5],start))
        if len(insns)<4 or insns[-1].address!=site or insns[0].op_str.split(',')[0]!='ecx' or insns[1].mnemonic!='call':continue
        moved=False;valid=True
        for ins in insns[2:-1]:
            written={ins.reg_name(r)for r in ins.regs_access()[1]}
            if written&rax or ins.mnemonic=='call':valid=False;break
            if ins.mnemonic=='mov' and ins.op_str=='rcx, rax':moved=True
            elif written&rcx:moved=False
        if valid and moved:return insns[0].operands[1].imm&0xffffffff
    return None


@functools.lru_cache(maxsize=None)
def crt_rand_returns(path):
    """Return-instruction VAs of MSVC CRT rand(): `imul ecx, [rax+0x28], 0x343fd;
    add ecx, 0x269ec3; mov [rax+0x28], ecx; shr ecx, 0x10; and ecx, 0x7fff;
    mov eax, ecx; ...; ret` (the per-thread LCG that srand seeds)."""
    pe=pefile.PE(path,fast_load=True);base=pe.OPTIONAL_HEADER.ImageBase;found=[]
    signature=bytes.fromhex('6948 28 fd430300 81c1 c39e2600 894828 c1e910 81e1ff7f0000 8bc1'.replace(' ',''))
    for section in pe.sections:
        if not section.Characteristics&0x20000000:continue
        data=section.get_data();start=0
        while True:
            at=data.find(signature,start)
            if at<0:break
            end=data.find(b'\xc3',at+len(signature),at+len(signature)+8)
            if end>0:found.append(base+section.VirtualAddress+end)
            start=at+1
    return tuple(found)


def _inline_factory_size(path,ctor_va,embedded_offset):
    if not embedded_offset:return None
    from capstone import Cs,CS_ARCH_X86,CS_MODE_64
    from capstone.x86 import X86_OP_MEM,X86_OP_REG
    pe=pefile.PE(path,fast_load=True);rva=ctor_va-pe.OPTIONAL_HEADER.ImageBase
    md=Cs(CS_ARCH_X86,CS_MODE_64);md.detail=True
    insns=[]
    for ins in md.disasm(pe.get_data(rva,0x400),ctor_va):
        insns.append(ins)
        if ins.mnemonic in('ret','jmp')or len(insns)>=96:break
    family=lambda name:{name,'e'+name[1:],name[1:]}if name in('rax','rbx','rcx','rdx','rsi','rdi','rbp')else{name,name+'d',name+'w',name+'b'}
    for k in range(len(insns)-1):
        first,call=insns[k],insns[k+1]
        if first.mnemonic!='mov' or first.op_str.split(',')[0]!='ecx' or call.mnemonic!='call' or len(first.operands)!=2:continue
        aliases={'rax'}
        for ins in insns[k+2:]:
            if ins.mnemonic in('call','ret','jmp'):break
            if ins.mnemonic=='lea':
                mem=ins.operands[1].mem
                if ins.operands[1].type==X86_OP_MEM and not mem.index and ins.reg_name(mem.base)in aliases and mem.disp==embedded_offset:
                    return first.operands[1].imm&0xffffffff
            written={ins.reg_name(r)for r in ins.regs_access()[1]}
            source=ins.op_str.split(', ')[1] if ins.mnemonic=='mov' and ', ' in ins.op_str else None
            for alias in list(aliases):
                if written&family(alias):aliases.discard(alias)
            if ins.mnemonic=='mov' and source in aliases and len(ins.operands)==2 and ins.operands[0].type==X86_OP_REG:
                aliases.add(ins.reg_name(ins.operands[0].reg))
            if not aliases:break
        return None
    return None


class GenericCapture(OriginalSdkConstructor,CapturePultec):
    """One host class for all generated profiles; profile differences are data."""
    def __new__(cls,profile=None,*args,**kwargs):
        # DSPAudioProcessor-family profiles (no CHypParamBlock) run on their own host class (dspap_capture.py).
        if cls is GenericCapture and isinstance(profile,dict) and profile.get('framework')=='DSPAudioProcessor':
            if "sonnox" in (profile.get("dspap") or {}):  # Soup layer (Oxford family): sonnox_capture.py
                from sonnox_capture import SonnoxCapture
                return object.__new__(SonnoxCapture)
            from dspap_capture import DspApCapture
            return object.__new__(DspApCapture)
        if cls is GenericCapture and isinstance(profile,dict) and profile.get('framework')=='VST2IUADPlug':
            if (profile.get('vst2') or {}).get('variant')=='bx':  # sub-family B: BXEffect behind UADAdapter (vst2_bx_host.py)
                from vst2_bx_host import Vst2BxCapture
                return object.__new__(Vst2BxCapture)
            if (profile.get('vst2') or {}).get('variant')=='relab':  # sub-family C: UAD2AGain/ParameterHandler template (vst2_relab_host.py)
                from vst2_relab_host import Vst2RelabCapture
                return object.__new__(Vst2RelabCapture)
            from vst2_host import Vst2Capture
            return object.__new__(Vst2Capture)
        if cls is GenericCapture and isinstance(profile,dict) and profile.get('framework')=='Softube':
            import dspap_capture  # noqa: F401  (imported first: its tail loads sphere_capture, which subclasses softube_capture)
            from softube_capture import SoftubeCapture  # Softube-engine family: the original VSTPluginMain builds the effect
            return object.__new__(SoftubeCapture)
        return object.__new__(cls)
    def __init__(self,profile,auto_start=True):
        self.profile=profile;self.phase='profile_validation'
        self.carrier_channels=profile.get('carrier_channels',1)
        # Channel count answered to parent 0x518/0x520 (requested chain IO); descriptor extraction sets it to 2
        # for plug-ins whose original descriptor IO is (2,2), matching the stereo carrier (see interventions).
        self.host_channels=2 if profile.get('stereo_channel_request') is True else 1  # opt-in (carrier/stereo-channel-request.json); a real host answers it from construction on
        self.descriptor=None  # the 0x538 program-state init runs the formatter before extraction sets it
        self.deferred_reads=set()  # refs read while self.descriptor is None; checked by check_deferred_reads
        if self.carrier_channels not in (1,2):raise ValueError('original mono or stereo carrier channels required')
        self.parameter_block=profile['parameter_block']
        self.maxima=tuple(map(integer,profile['parameters']['effective_maxima']))
        if not self.maxima or len(self.maxima)>256 or any(not 0<m<=32768 for m in self.maxima):
            raise ValueError('bounded original effective parameter maxima required')
        source=profile['dll'];self.phase='map_original_images'
        OfflineHost.__init__(self,source['path'],source['sha256'])
        self.uc.mem_map(HEAP+0x10000,0x7f0000)
        from collections import deque
        self.last_blocks=deque(maxlen=32)
        self.uc.hook_add(UC_HOOK_BLOCK,lambda u,a,size,data:self.last_blocks.append(a))
        self.this=HEAP;self.parent=HEAP+0x200000
        constructor=profile['parameter_block']['constructor']
        ctor=constructor.get('parent_constructor_va',constructor.get('va'))
        embedded=constructor.get('embedded_offset')
        self.parent_allocation=parent_allocation_size(source['path'],integer(ctor),integer(embedded)if embedded is not None else None)if ctor else None
        if self.parent_allocation is not None and PARENT_SLOT<self.parent_allocation<=0x4000000:
            # The original object would overlap host fixtures/heap: give it its own span.
            self.uc.mem_map(LARGE_PARENT,(self.parent_allocation+0xffff)&~0xffff)
            self.parent=LARGE_PARENT
        self.iface=HEAP+0x300000;self.vtable=HEAP+0x310000
        self.lock=HEAP+0x320000;self.lockvt=HEAP+0x330000
        self.parentvt=HEAP+0x340000;self.fs=HEAP+0x350000
        self.string_buffer=HEAP+0x360000
        self.alloc_next=HEAP+0x400000;self.allocations={}
        self.shadow={};self.atomic=None;self.trace=[];self.events=[]
        self.notifications=[];self.notification_events=[];self.atomic_refs=set();self.last_error=0
        # Original bracket counter memories (begin arg3) and, per ref, whether its last
        # set_int/set_array value was that live counter (kept as the value, so any later write by
        # another path no longer matches the shadow): a non-bracketed sequence endpoint word.
        self.sequence_counters=set();self.sequence_words={}
        # Words written with the value just returned by the original CRT rand().
        self.random_words={};self.pending_rand=None
        for ret in crt_rand_returns(source['path']):
            self.uc.hook_add(UC_HOOK_CODE,lambda u,a,size,d:setattr(self,'pending_rand',u.reg_read(UC_X86_REG_RAX)&0xffffffff),begin=ret,end=ret)
        # Event indices produced while the original program loader is executing.
        self.loader_event_indices=set();self.loader_depth=0;self.loader_calls=0
        self.tls_vector=HEAP+0x380000;self.tls_slot0=HEAP+0x381000
        self.uc.reg_write(UC_X86_REG_GS_BASE,HEAP+0x370000)
        self.wq(HEAP+0x370010,STACK)
        # Original code may read the TEB TLS-vector pointer at GS:[0x58].
        # Model only bounded slot zero, backed by zero-initialized fixture storage.
        self.wq(HEAP+0x370058,self.tls_vector);self.wq(self.tls_vector,self.tls_slot0)
        # Loader TLS semantics: the module's TLS block starts as a copy of the PE TLS template
        # (e.g. MSVC _Init_thread_epoch = INT_MIN), the rest zero. Slot zero is the only TLS slot.
        tls_pe=pefile.PE(source['path'],fast_load=True);tls_pe.parse_data_directories(directories=[9])
        tls=getattr(tls_pe,'DIRECTORY_ENTRY_TLS',None)
        if tls is not None:
            template=bytes(tls_pe.get_memory_mapped_image()[tls.struct.StartAddressOfRawData-BASE:tls.struct.EndAddressOfRawData-BASE])
            if len(template)+tls.struct.SizeOfZeroFill>0x1000:raise ValueError('bounded TLS template required')
            self.uc.mem_write(self.tls_slot0,template)
        self.uc.reg_write(UC_X86_REG_MXCSR,0x1f80)
        self.uc.reg_write(UC_X86_REG_FPCW,0x37f)
        self.wq(self.parent,self.parentvt);self.wq(self.parent+0x100,self.iface)
        self.wq(self.iface,self.vtable);self.wq(self.lock,self.lockvt)
        # Original base-constructor fields holding rip-relative constant arguments
        # (the host does not run the parent base constructor itself).
        base_ctor=profile["parent"].get("constructor_base")
        parent_ctor=constructor.get("parent_constructor_va",constructor.get("va"))
        if base_ctor and parent_ctor:
            from parent_fields import base_static_pointer_fields
            for off,va in base_static_pointer_fields(source["path"],integer(parent_ctor),integer(base_ctor)).items():
                if off not in (0,0x100,0x348) and 8<=off<PARENT_SLOT and self.rq(self.parent+off)==0:self.wq(self.parent+off,va)
        self.wf(self.fs,48000);self.wf(self.fs+4,1/48000)
        if profile['parameters'].get('table_va'):
            table=integer(profile['parameters']['table_va'])
            actual=struct.unpack('<'+str(len(self.maxima))+'I',self.uc.mem_read(table,4*len(self.maxima)))
            if actual!=tuple(profile['parameters']['raw_maxima']) or tuple(v or 32768 for v in actual)!=self.maxima:
                raise ValueError('generated original domain table differs from pinned image')
            self.wq(self.parent+0x348,table)
        for vt,start,label,count in ((self.vtable,STUBS,'cache',0x40),
                (self.lockvt,STUBS+0x1000,'lock',0x40),
                (self.parentvt,STUBS+0x2000,'parent',0xc0)):
            for i in range(count):
                addr=start+i*16;self.wq(vt+i*8,addr)
                self.handlers[addr]=('unknown '+label+' '+hex(i*8),None)
        self.bind(0x18,'cache_lock',lambda:self.lock)
        self.bind(0x130,'processing_quantum',lambda:32)
        # IUADPlug subobject getters (Mac UAD-2 SDK Support CUADPlug, Windows slot =
        # Mac slot - 8): topology from CreateUAD2Topology(in, out, bs, fs). Values are
        # the configured carrier topology; block size (0x38) stays unmodeled.
        self.bind(0x28,'topology_input_channels',lambda:self.carrier_channels)
        self.bind(0x30,'topology_output_channels',lambda:self.carrier_channels)
        self.bind(0x40,'topology_sample_rate',self.topology_sample_rate)
        # 0x118 UpdateRsrcProfile(int showMessage): 0 = success / nothing to do.
        self.bind(0x118,'update_resource_profile',self.update_resource_profile)
        # 0x138 IsInvalid(): BOOL state==0; a loaded headless host is past UADPlugInit.
        self.bind(0x138,'plug_is_invalid',lambda:0)
        for off in (8,16):self.handlers[STUBS+0x1000+(off//8)*16]=('lock_ref',lambda:0)
        for off,name in ((0x90,'begin'),(0x98,'end'),(0xa0,'set_int'),
                         (0xa8,'set_float'),(0xb0,'set_array'),(0xb8,'set_array'),
                         (0xc0,'get_var'),(0xc8,'get_var'),(0xd0,'get_array'),(0xd8,'get_array'),
                         (0xe0,'header_timing')):
            self.bind(off,name,getattr(self,name))
        # 0x5a0 is the original composition of the source-proven 0x520 and
        # 0x518 success checks; it returns true only when both do.
        for off,fn in ((0x4e0,self.parent_callback),(0x518,lambda:self.host_channels),(0x520,lambda:self.host_channels),(0x5a0,lambda:int(self.carrier_channels==1))):
            self.handlers[STUBS+0x2000+(off//8)*16]=('known_parent '+hex(off),fn)
        # Slot 0x5a0 is only the composition of the 0x518/0x520 checks when its original body
        # dispatches through those slots; plug-ins whose 0x5a0 is another function (e.g. a
        # program-bank store called by the original set_parameter) keep the original body.
        slot5a0=profile['parent']['slots'].get('0x5a0')
        if isinstance(slot5a0,dict) and slot5a0.get('function_end'):
            from capstone import Cs,CS_ARCH_X86,CS_MODE_64
            va5a0,end5a0=integer(slot5a0['va']),integer(slot5a0['function_end'])
            if BASE<=va5a0<end5a0<=BASE+0x4000000:
                body5a0=' '.join(i.op_str for i in Cs(CS_ARCH_X86,CS_MODE_64).disasm(bytes(self.uc.mem_read(va5a0,end5a0-va5a0)),va5a0))
                if '+ 0x518]' not in body5a0 and '+ 0x520]' not in body5a0 and 'call' not in body5a0 and self.handlers.get(STUBS+0x2000+(0x5a0//8)*16,('',))[0].startswith('known_parent'):
                    del self.handlers[STUBS+0x2000+(0x5a0//8)*16]
                    self.wq(self.parentvt+0x5a0,va5a0)
        # Channel-activity slots 0x180/0x188: the source-profiled originals ask the host callback (request 4,
        # channel in edx) and answer true when it returns 0; the host answers 0 for the configured channels
        # only (mono unless a stereo request). The original counting helpers loop over the channel counts the
        # base constructor stores from the derived constructor's constant stack arguments (+0x1d8/+0x1dc),
        # which this host does not run, so those two fields are written from the same constants.
        def channel_slot(spec):
            from capstone import Cs,CS_ARCH_X86,CS_MODE_64
            if not isinstance(spec,dict) or not spec.get('function_end'):return False
            va,end=integer(spec['va']),integer(spec['function_end'])
            if not BASE<=va<end<=BASE+0x4000000:return False
            body=' | '.join(i.mnemonic+' '+i.op_str for i in Cs(CS_ARCH_X86,CS_MODE_64).disasm(bytes(self.uc.mem_read(va,end-va)),va))
            return all(t in body for t in ('cmp qword ptr [rax + 8], 0','mov edx, 4','call qword ptr [rax + 8]'))
        if base_ctor and parent_ctor and all(channel_slot(profile['parent']['slots'].get(hex(o)))for o in (0x180,0x188)):
            from parent_fields import base_stack_int_fields
            counts=base_stack_int_fields(source["path"],integer(parent_ctor),integer(base_ctor))
            if all(o in counts for o in (0x1d8,0x1dc)):
                for o in (0x1d8,0x1dc):self.wd(self.parent+o,counts[o])
                for off in (0x180,0x188):
                    self.handlers[STUBS+0x2000+(off//8)*16]=('known_parent '+hex(off),lambda:int((self.arg(1)&0xffffffff)<self.host_channels))
        context=profile.get('host_context',{}).get('sample_rate')
        if context is not None:
            if (context['storage']!='float32' or context['return_register']!='xmm0'
                    or context['initialize_from']!='configured_sample_rate'):
                raise ValueError('source-proven parent samplerate context required')
            self.wf(self.parent+integer(context['field_offset']),48000.)
            # The framework base stores getSampleRate() at parent+0x1e0 and 1/fs at +0x1e4 on open; the originals
            # size their delay lines from it (UA: Ampex 4*fs, EMT 250/Cooper fs-scaled rows). Unset it reads 0.
            self.wf(self.parent+0x1e0,48000.);self.wf(self.parent+0x1e4,1/48000)
            self.wq(self.parentvt+integer(context['slot']),integer(context['va']))
        descriptor=profile.get('descriptor',{})
        self.parent_identity_bindings=[]
        for slot,spec in profile['parent']['slots'].items():
            va=integer(spec['va']);offset=integer(slot)
            if bytes(self.uc.mem_read(va,3))==b'\x8b\xc2\xc3' and offset not in (0x4e0,0x518,0x520):
                self.wq(self.parentvt+offset,va)
                self.parent_identity_bindings.append(dict(slot=slot,va=hex(va),body='mov eax, edx; ret'))
        required_parent_slots=descriptor.get('required_parent_slots',[])
        if not isinstance(required_parent_slots,list):
            raise ValueError('descriptor required parent slots must be an explicit list')
        self.parent_required_bindings=[]
        for raw_slot in required_parent_slots:
            slot=integer(raw_slot)
            if slot%8 or not 0<=slot<0xc0*8:
                raise ValueError('bounded parent vtable slot required')
            source_slot=profile['parent']['slots'].get(hex(slot))
            if not isinstance(source_slot,dict) or 'va' not in source_slot or 'function_end' not in source_slot:
                raise ValueError('source-profiled parent slot identity required')
            va,end=integer(source_slot['va']),integer(source_slot['function_end'])
            if not BASE<=va<end<=BASE+0x4000000:
                raise ValueError('source parent slot must remain inside mapped original image')
            self.wq(self.parentvt+slot,va)
            self.parent_required_bindings.append(dict(slot=hex(slot),va=hex(va),function_end=hex(end)))
        exports={s.name:SDKBASE+s.address for s in pefile.PE(str(SDK)).DIRECTORY_ENTRY_EXPORT.symbols if s.name}
        self.sdk_redirects={}
        for name,label,stub in self.imports:
            if label.lower().startswith('uad2sdk.dll!') and name in exports:
                self.sdk_redirects[stub]=exports[name]
            if name in (b'?PropagateUpdateToRte@CHypParamBlock@@UEAAXII@Z',
                        b'?PropagateUpdateToControls@CHypParamBlock@@QEAAXIPEAX@Z'):
                self.sdk_redirects.pop(stub,None);self.handlers[stub]=('parameter_notification',lambda kind=name:self.notify(kind))
            elif name==b'GetKeyState':self.handlers[stub]=('no_shift_keyboard',self.key_state)
            elif name==b'GetLastError':self.handlers[stub]=('thread_GetLastError',lambda:self.last_error)
            elif name==b'SetLastError':self.handlers[stub]=('thread_SetLastError',self.set_last_error)
            elif name==b'RegOpenKeyExA':self.handlers[stub]=('registry_key_absent',self.registry_key_absent)
            elif name==b'RegCreateKeyExA':self.handlers[stub]=('registry_create_denied',lambda:self.registry_key_absent(7,5))
            elif name==b'SHGetSpecialFolderPathW':self.handlers[stub]=('shell_folder_absent',self.shell_folder_absent)
            elif name==b'GetCurrentDirectoryW':self.handlers[stub]=('current_directory_absent',self.current_directory_absent)
            elif name==b'FindFirstFileW':self.handlers[stub]=('find_file_absent',self.find_file_absent)
        self.phase='original_crt_encoded_null_initializer'
        from crt_init import initialize,discover
        aggregate=profile.get('host_context',{}).get('crt_aggregate')
        if aggregate is None:self.crt_initialization=initialize(self,source['path'])
        else:
            self.crt_initialization=discover(source['path'])
            if self.crt_initialization['va']not in[i['initialize']for i in aggregate['entries']]:
                raise ValueError('original aggregate must include encoded-null initializer')
        self.initialize_crt_thread()
        self.run_thread_safe_statics_initializer()
        self.run_stdio_initializer()
        dynamic=profile.get('host_context',{}).get('crt_dynamic_initializers')
        if dynamic is not None:self.run_crt_dynamic_initializers(dynamic)
        self.run_domain_initializers(profile)
        constructor=self.parameter_block['constructor']
        if constructor.get('kind')!='inline_span' and constructor.get('receiver','parameter_block')!='parameter_block':
            raise ValueError('constructor receiver requires unresolved whole-parent ABI')
        if constructor.get('kind') in ('inline_span','standalone_pb_constructor'):
            offset=integer(constructor['embedded_offset'])
            # The parent is either the fixed fixture slot or, for a factory-sized original object, its own mapping
            # of the original allocation size; the embedded PB pointer must lie inside that object.
            if not 0<offset<self.parent_span()-8:raise ValueError('bounded source-derived embedded PB offset required')
            self.this=self.parent+offset
        self.phase='original_constructor'
        # Two-PB parent: the original parent constructor builds a parent-bound PB, then the profile PB.
        # One source-derived span runs both original constructors in their original order.
        pair=profile.get('host_context',{}).get('parent_bound_parameter_block')
        self.parent_bound=None
        self.public_receiver=None;self.public_slots={};self.routes=None
        if pair is not None:
            span=pair['constructor_span']
            if constructor.get('kind')!='inline_span' or span['stop']!=constructor['stop'] or not integer(span['entry'])<integer(constructor['entry'])<integer(span['stop']):
                raise ValueError('source-derived two-PB constructor span required')
            offset=integer(pair['embedded_offset'])
            if not 0<offset<self.parent_span()-8 or self.parent+offset==self.this:raise ValueError('bounded distinct parent-bound PB offset required')
            self.parent_bound=self.parent+offset
            self.constructor_span(span)
            if self.rq(self.parent_bound)!=integer(pair['vtable']):
                raise ValueError('original parent-bound PB vtable identity differs '+hex(self.rq(self.parent_bound)))
            # Source-derived public value roles (setter/getter/parse/format): either the profile PB (the parent-bound
            # PB forwards to it) or the parent-bound PB itself (profile PB roles are one constant stub).
            public=self.parameter_block.get('public_block') or {}
            if public.get('block')=='parent_bound':
                self.public_receiver=self.parent_bound;self.public_slots=dict(public['public_slots'])
            elif public.get('block')!='profile':raise ValueError('source-derived public parameter block required for two-PB parent')
        elif constructor.get('kind')=='inline_span':self.constructor_span(constructor)
        else:self.call(integer(constructor['va']),self.this)
        actual=self.rq(self.this);expected=integer(self.parameter_block['vtable'])
        if actual!=expected:raise ValueError('original constructor vtable identity differs '+hex(actual))
        binding=profile.get('host_context',{}).get('parameter_block_pointer')
        if binding is not None:
            target={'parameter_block':self.this,'parent_bound_parameter_block':self.parent_bound}.get(binding['points_to'])
            if binding['storage']!='pointer64' or target is None or target!=self.parent+integer(binding['embedded_offset']):
                raise ValueError('source-derived parent PB receiver binding required')
            self.wq(self.parent+integer(binding['field_offset']),target)
            self.parent_pb_pointer_target=binding['points_to']
        elif pair is not None:raise ValueError('two-PB parent requires its source-derived pointer field')
        if descriptor.get('status')=='original_secondary_virtual_dispatch_discovered':
            offset=integer(descriptor['secondary_object_offset'])
            receiver_offset=integer(descriptor['receiver_from_parameter_block'])
            secondary=self.parent+offset
            storage=secondary+0x368 if descriptor.get('dispatch',{}).get('receiver_kind') == 'member_pointer' else None
            if not (0 < offset < 0x10000 and receiver_offset+ self.this == secondary):
                raise ValueError('source-derived secondary descriptor receiver binding required')
            if storage is not None and self.rq(storage) not in (0, secondary):
                raise ValueError('secondary descriptor member pointer differs after constructor')
            self.wq(secondary,integer(descriptor['secondary_vtable']))
            if storage is not None:self.wq(storage,secondary)
        from base_init import apply as base_init_prefix
        self.base_init_prefix=base_init_prefix(self,profile)
        self.phase='original_setup'
        framework=self.framework_setup_slot(profile) if pair is None else None
        if pair is not None:
            # Original parent slot 0x578 sets up both PBs with (parent+fs field, parent, 0) and attaches them.
            setup=pair['setup'];source=profile['parent']['slots'].get(setup['parent_slot'])
            if not isinstance(source,dict) or source.get('va')!=setup['va'] or source.get('function_end')!=setup['function_end']:
                raise ValueError('source-profiled two-PB parent setup slot required')
            field=integer(setup['sample_rate_field_offset'])
            if not 0<field<0x1000:raise ValueError('bounded framework sample-rate field required')
            self.wf(self.parent+field,48000.);self.wf(self.parent+field+4,1/48000)
            self.call(integer(setup['va']),self.parent)
        elif framework is None:self.vcall(0x60,self.fs,self.parent,0)
        else:
            # Framework open stores getSampleRate() at parent+0x1e0 and (float)(1.0/fs)
            # at parent+0x1e4, then calls parent slot 0x578, which sets up the PB/DSP
            # objects with those fields. Member objects 0x578 dispatches through are
            # first built by their original constructors from the parent constructor.
            va,members=framework
            self.framework_members=[]
            for offset,ctor in members:
                self.phase='original_member_constructor';self.call(ctor,self.parent+offset)
                if not BASE<=self.rq(self.parent+offset)<BASE+0x4000000:
                    raise ValueError('original member constructor left no image vtable '+hex(offset))
                self.framework_members.append(dict(offset=hex(offset),constructor=hex(ctor)))
            self.phase='original_setup'
            self.wf(self.parent+0x1e0,48000.);self.wf(self.parent+0x1e4,1/48000)
            self.call(va,self.parent)
            # The original framework initializer calls parent slot 0x538 (default program-state
            # init) right after 0x578; plug-ins whose set_parameter stores into that state need it.
            state=profile['parent']['slots'].get('0x538')
            if isinstance(state,dict) and state.get('function_end') and self.rq(self.parentvt+0x538)!=integer(state['va']):
                sva,send=integer(state['va']),integer(state['function_end'])
                if BASE<=sva<send<=BASE+0x4000000 and send-sva>8:
                    self.phase='original_program_state_init';self.wq(self.parentvt+0x538,sva)
                    # Same original set_parameter / normalizer the program loader uses.
                    pp=profile['parent'];self.wq(self.parentvt+0x50,integer(pp['set_parameter']['va']))
                    self.wq(self.parentvt+integer(pp['normalizer']['slot']),integer(pp['normalizer']['va']))
                    self.bind_parent_slots_called_from(profile,sva,send,{0x538});self.call(sva,self.parent)
        # Pre-program state: recorded raw. Some original setups write raw defaults
        # (e.g. seconds, not domain integers) that only the captured program replaces;
        # every read from program load onward stays domain-checked.
        self.phase='constructor_values'
        self.constructor_values=tuple(self.vcall(0x10,i)&0xffffffff for i in range(len(self.maxima)))
        self.constructor_values_outside_domain=[i for i,(v,m) in enumerate(zip(self.constructor_values,self.maxima)) if v>m]
        self.descriptor=None
        if auto_start:self.start()

    def framework_setup_slot(self,profile):
        """(original parent slot 0x578, [(member offset, original ctor)]) or None.

        Used when the source-profiled 0x578 passes `[parent+0x1e0]` (framework
        sample-rate field) to the setups it dispatches, and either the PB Setup slot
        (0x60) is a bare `ret`, or 0x578 dispatches virtually through member objects
        other than the PB whose unique `lea rcx,[this+Y]; call C` constructor exists
        in the original parent constructor. Otherwise None (PB Setup is called)."""
        source=profile['parent']['slots'].get('0x578')
        if not isinstance(source,dict) or not source.get('function_end'):return None
        va,end=integer(source['va']),integer(source['function_end'])
        if not BASE<=va<end<=BASE+0x4000000:return None
        from capstone import Cs,CS_ARCH_X86,CS_MODE_64
        md=Cs(CS_ARCH_X86,CS_MODE_64)
        body=list(md.disasm(bytes(self.uc.mem_read(va,end-va)),va))
        if not any(i.mnemonic=='lea' and i.op_str.endswith('+ 0x1e0]') and i.op_str.split(',')[0] in ('rdx','rsi','rdi','r8','r9','rbx','rbp','r12','r13','r14','r15') for i in body):return None
        setup=self.rq(self.rq(self.this)+0x60)
        stub=bytes(self.uc.mem_read(setup,3))==b'\xc2\x00\x00' or bytes(self.uc.mem_read(setup,1))==b'\xc3'
        embedded=self.this-self.parent;offsets=set()
        for k,ins in enumerate(body):
            if ins.mnemonic!='call' or not ins.op_str.startswith('qword ptr [rax + '):continue
            for prior in reversed(body[max(0,k-6):k]):
                if prior.mnemonic=='lea' and prior.op_str.startswith('rcx, [') and ' + 0x' in prior.op_str:
                    offsets.add(int(prior.op_str.rsplit(' + ',1)[1][:-1],16));break
                if prior.mnemonic=='add' and prior.op_str.startswith('rcx, 0x'):
                    offsets.add(int(prior.op_str[5:],16));break
        offsets.discard(embedded)
        if not offsets:return (va,[])if stub else None
        constructor=profile['parameter_block']['constructor']
        parent_ctor=integer(constructor.get('parent_constructor_va',constructor.get('va')))
        ctor_body=list(md.disasm(bytes(self.uc.mem_read(parent_ctor,0x2000)),parent_ctor))
        members=[]
        for offset in sorted(offsets):
            if self.rq(self.parent+offset):continue
            calls={ctor_body[k+1].op_str for k,ins in enumerate(ctor_body[:-1])
                   if ins.mnemonic=='lea' and ins.op_str.startswith('rcx, [') and ins.op_str.endswith('+ '+hex(offset)+']')
                   and ctor_body[k+1].mnemonic=='call' and ctor_body[k+1].op_str.startswith('0x')}
            if len(calls)!=1:return (va,[])if stub else None
            members.append((offset,int(calls.pop(),16)))
        return va,members

    def run_domain_initializers(self,profile):
        """Run original pure C++ initializers that store into the parameter-domain table (e.g. step
        count - 1), then re-derive the effective maxima from the live table the original normalizer reads."""
        self.domain_initialization=[]
        table=profile['parameters'].get('table_va')
        if not table:return
        from cxx_domain_init import domain_initializers
        n=len(self.maxima);table=integer(table)
        for entry in domain_initializers(profile['dll']['path'],table,n):
            self.phase='original_cxx_domain_initializer_'+str(entry['ordinal'])
            slot=entry['table_slot']
            if self.rq(slot)!=entry['va']:raise ValueError('C++ domain initializer entry differs from original table')
            self.call(entry['va'])
            self.domain_initialization.append(dict(ordinal=entry['ordinal'],va=hex(entry['va']),stores=entry['stores']))
        if self.domain_initialization:
            actual=struct.unpack('<'+str(n)+'I',self.uc.mem_read(table,4*n))
            maxima=tuple(v or 32768 for v in actual)
            if any(not 0<m<=32768 for m in maxima):raise ValueError('original C++ domain initializer produced out-of-bound maxima')
            self.maxima=maxima

    def run_thread_safe_statics_initializer(self):
        """Original statically linked MSVC thread-safe-statics startup (first __xc entry: global init lock,
        condition-variable exports, onexit tables). Local-static guards compare against the TLS epoch, so the
        original lock must be initialized before any constructor reaches _Init_thread_header."""
        from thread_safe_statics import find_initializer
        self.thread_safe_statics=None
        va=find_initializer(self.profile['dll']['path'])
        if va is None:return
        self.phase='original_thread_safe_statics_initializer'
        result=self.call(va)
        if result&0xffffffff:raise ValueError('original thread-safe statics initializer failed')
        self.thread_safe_statics=hex(va)

    def run_crt_dynamic_initializers(self,dynamic):
        """Source-attested C++ dynamic initializers (__xc table), original table order, after CRT startup.
        Each runs only if the original table bytes and its slot still name it; nothing else in the table runs.
        Destructor (atexit) registration is recorded by discovery but not executed."""
        start,end=integer(dynamic['table_start']),integer(dynamic['table_end'])
        if not BASE<=start<end<=BASE+0x4000000 or (end-start)%8:raise ValueError('bounded original C++ initializer table required')
        if hashlib.sha256(bytes(self.uc.mem_read(start,end-start))).hexdigest()!=dynamic['table_sha256']:
            raise ValueError('original C++ initializer table identity differs')
        self.crt_dynamic_initialization=[]
        for entry in sorted(dynamic['entries'],key=lambda e:int(e['ordinal'])):
            slot=integer(entry['table_slot'])
            if slot!=start+8*int(entry['ordinal']) or self.rq(slot)!=integer(entry['va']):
                raise ValueError('C++ initializer entry differs from original table')
            self.phase='original_cxx_dynamic_initializer_'+str(entry['ordinal'])
            # Run the initializer's original constructor call exactly as the thunk makes it. The thunk's trailing
            # atexit registration only queues process-exit teardown, which offline execution never reaches.
            args=[integer(entry['constructs'])]+([integer(entry['constant_argument'])] if entry.get('constant_argument') else [])
            self.call(integer(entry['constructor']),*args)
            self.crt_dynamic_initialization.append(dict(ordinal=entry['ordinal'],va=entry['va'],constructs=entry['constructs'],
                value=hex(self.rq(integer(entry['constructs'])))))

    def run_stdio_initializer(self):
        """Original CRT stdio startup (.CRT$XIC): the std streams own CRITICAL_SECTIONs the original printf locks."""
        if getattr(self,'thread_runtime',None) is None:return
        from stdio_init import find_initializer
        va=find_initializer(self.profile['dll']['path'])
        if va is None:return
        self.phase='original_stdio_initializer'
        if self.call(va)&0xffffffff:raise ValueError('original stdio initializer failed')
        self.stdio_initializer=hex(va)

    def initialize_crt_thread(self):
        initializers=self.profile.get('host_context',{}).get('crt_initializers',[])
        if not initializers:return
        if [i['role']for i in initializers]not in (['heap','fls'],['heap','locks','fls'],['locks','heap','fls']):
            raise ValueError('original ordered heap/FLS startup required')
        from thread_runtime import ThreadRuntime
        self.thread_runtime=ThreadRuntime(self);self.thread_runtime.bind()
        self.crt_thread_initialization=[]
        aggregate=self.profile.get('host_context',{}).get('crt_aggregate')
        if aggregate is not None:
            self.phase='original_crt_aggregate_initializer'
            table=bytes(self.uc.mem_read(integer(aggregate['table_start']),integer(aggregate['table_end'])-integer(aggregate['table_start'])))
            if hashlib.sha256(table).hexdigest()!=aggregate['table_sha256']:raise ValueError('original aggregate table identity differs')
            result=self.call(integer(aggregate['va']),*[integer(a)for a in aggregate.get('arguments',[])])
            if result&integer(aggregate['return_mask'])!=integer(aggregate['success_return']):
                raise ValueError('original CRT aggregate initializer returned failure')
            cookie=self.rq(integer(self.crt_initialization['cookie']))
            if any(self.rq(integer(ref))!=cookie for ref in self.crt_initialization['writes']):raise ValueError('aggregate encoded-null output mismatch')
            self.crt_thread_initialization.append(dict(role='aggregate',va=aggregate['va'],return_value=result,host_events=self.thread_runtime.events))
            return
        for item in initializers:
            self.phase='original_crt_'+item['role']+'_initializer'
            address=integer(item['global_va'])if 'global_va'in item else None
            size=integer(item.get('global_size',0))
            if address is not None and bytes(self.uc.mem_read(address,size)).hex()!=item['global_initial']:
                raise ValueError('original CRT startup global differs before initialization')
            result=self.call(integer(item['va']))
            if result&integer(item['return_mask'])!=integer(item['success_return']):
                raise ValueError('original CRT initializer returned failure')
            value=int.from_bytes(self.uc.mem_read(address,size),'little')if address is not None else None
            if item['role']=='locks' and (not self.thread_runtime.critical_sections.registry or any(owner or depth for owner,depth in self.thread_runtime.critical_sections.registry.values())):
                raise ValueError('original CRT locks were not initialized idle')
            if item['role']=='heap' and value!=self.thread_runtime.heap:
                raise ValueError('original CRT did not retain obtained heap')
            if item['role']=='fls' and (value not in self.thread_runtime.fls or self.thread_runtime.callbacks[value]!=integer(item['callback_va'])or self.thread_runtime.fls[value]in(0,0xffffffffffffffff)):
                raise ValueError('original CRT did not initialize current thread FLS data')
            self.crt_thread_initialization.append(dict(role=item['role'],va=item['va'],return_value=result,global_value=value))

    def bind_span_host_callback(self,spec,registers,entry,stop):
        """The original constructor's second argument (rdx) is a host callback. When the span
        itself does `call <reg>` on a nonvolatile register the original prologue filled from rdx
        (and the profile left it unset), point that register at an offline host-callback stub:
        `cb(effect,opcode,index,value,ptr,opt)` answers 0 (opcode/handshake not recognised by
        this headless host), the unmodelled-request reply of the callback ABI."""
        from capstone import Cs,CS_ARCH_X86,CS_MODE_64
        from capstone.x86 import X86_OP_REG
        cs=Cs(CS_ARCH_X86,CS_MODE_64);cs.detail=True
        names={'rbx','rbp','rsi','rdi','r12','r13','r14','r15'}
        regname=lambda i,o:cs.reg_name(i.operands[o].reg)
        called={regname(i,0) for i in cs.disasm(bytes(self.uc.mem_read(entry,stop-entry)),entry)
                if i.mnemonic=='call' and i.operands and i.operands[0].type==X86_OP_REG}&names
        if not called:return
        va=integer(spec['va']);copies={regname(i,0) for i in cs.disasm(bytes(self.uc.mem_read(va,entry-va)),va)
                if i.mnemonic=='mov' and len(i.operands)==2 and i.operands[0].type==X86_OP_REG
                and i.operands[1].type==X86_OP_REG and regname(i,1)=='rdx'}
        stub=STUBS+0xd000
        self.handlers[stub]=('offline_host_callback',lambda:0)
        for name in called&copies:
            if name not in spec['registers']:self.uc.reg_write(registers[name],stub)

    def constructor_span(self, spec):
        """Execute only a source-discovered inline ctor, never guessed field writes."""
        registers={name:globals()['UC_X86_REG_'+name.upper()]for name in
                   ('rax','rbx','rcx','rdx','rsi','rdi','rbp','r8','r9','r10','r11','r12','r13','r14','r15')}
        rsp=STACK+0xf0000
        bases={'parent':self.parent,'parameter_block':self.this,'stack':rsp,'entry_stack':rsp}
        def resolve(value):
            if isinstance(value,dict):
                if 'value'in value:return integer(value['value'])
                return bases[value['base']]+integer(value.get('offset',0))
            return integer(value)
        for reg in registers.values():self.uc.reg_write(reg,0)
        self.uc.reg_write(UC_X86_REG_RSP,rsp)
        for name,value in spec['registers'].items():
            if name.lower()not in registers:raise ValueError('unknown generated register '+name)
            self.uc.reg_write(registers[name.lower()],resolve(value))
        for item in spec.get('stack_writes',[]):
            offset=integer(item['offset']);size=integer(item['size'])
            if abs(offset)>0x4000 or size not in (4,8):raise ValueError('bounded original stack initializer required')
            self.uc.mem_write(rsp+offset,resolve(item['value']).to_bytes(size,'little'))
        entry,stop=integer(spec['entry']),integer(spec['stop'])
        if not BASE<=entry<stop<BASE+0x4000000:raise ValueError('bounded image constructor span required')
        self.bind_span_host_callback(spec,registers,entry,stop)
        self.failure=None
        try:self.uc.emu_start(entry,stop,count=20000000)
        except UcError as exc:raise RuntimeError('constructor span at '+hex(self.uc.reg_read(UC_X86_REG_RIP))+': '+str(exc))from exc
        if self.failure:raise RuntimeError(str(self.failure))from self.failure
        if self.uc.reg_read(UC_X86_REG_RIP)!=stop:raise RuntimeError('constructor span stop not reached')
        self._check_completed_call()

    def reserve(self,size):
        if type(size)is not int or not 0<size<=0x100000:
            raise ValueError('bounded private object allocation required')
        if HEAP+0x400000<=self.alloc_next<=HEAP+0x800000 and self.alloc_next+size>HEAP+0x800000:
            # Original code needs more than the 4MB primary arena: continue in a
            # bounded overflow arena (used only once the primary arena is exhausted).
            self.uc.mem_map(HEAP_OVERFLOW,HEAP_OVERFLOW_SIZE);self.alloc_next=HEAP_OVERFLOW
        limit=HEAP+0x800000 if self.alloc_next<HEAP+0x800000 else HEAP_OVERFLOW+HEAP_OVERFLOW_SIZE
        if self.alloc_next+size>limit:
            raise ValueError('bounded private object allocation required')
        ptr=self.alloc_next;self.alloc_next=(ptr+size+15)&~15
        self.allocations[ptr]=size
        return ptr

    def rq(self,p):return struct.unpack('<Q',self.uc.mem_read(p,8))[0]

    def hook(self,u,addr,size,unused):
        if addr in getattr(self,'sdk_redirects',{}):
            u.reg_write(UC_X86_REG_RIP,self.sdk_redirects[addr]);return
        name,fn=self.handlers.get(addr,('',None))
        if fn is None and addr in self.handlers:
            from base_init import lazy_slot_target
            target=lazy_slot_target(self,addr)
            if target is not None:u.reg_write(UC_X86_REG_RIP,target);return
        if fn is None and addr in self.handlers:
            self.last_unknown=dict(name=name,args=[self.arg(i)for i in range(4)],return_va=hex(self.rq(self.uc.reg_read(UC_X86_REG_RSP))))
        return super().hook(u,addr,size,unused)

    def call(self,fn,*args):
        self.failure=None;rsp=STACK+0xff008;self.wq(rsp,STOP)
        for i,value in enumerate(args):
            if i<4:self.uc.reg_write(_ARGUMENTS[i],value)
            else:self.wq(rsp+0x28+(i-4)*8,value)
        self.uc.reg_write(UC_X86_REG_RSP,rsp)
        try:self.uc.emu_start(fn,STOP,count=20000000)
        except UcError as exc:raise RuntimeError('emulator at '+hex(self.uc.reg_read(UC_X86_REG_RIP))+': '+str(exc))from exc
        if self.failure:raise RuntimeError(str(self.failure))from self.failure
        if self.uc.reg_read(UC_X86_REG_RIP)!=STOP:raise RuntimeError('20M instruction budget exceeded')
        self._check_completed_call();return self.uc.reg_read(UC_X86_REG_RAX)

    def parent_span(self):
        """Bytes of the original parent object: its source-proven factory allocation (mapped in the fixed
        PARENT_SLOT span or, when larger, its own LARGE_PARENT mapping); otherwise the historical 0x10000 bound."""
        size=getattr(self,'parent_allocation',None)
        if size and (size<=PARENT_SLOT or self.parent==LARGE_PARENT):return size
        return 0x10000

    def route_public_getter(self):
        """index -> (receiver, sub-index) for parameters whose original public getter (parent slot 0x58) reads a
        different embedded block than the profile PB (e.g. a channel strip whose set_parameter dispatches across
        EQ/dynamics/delay blocks). Probed by running the original slot 0x58 up to its call of a block getter (vtable
        slot 0x10); identity routes are dropped."""
        slot=self.profile['parent']['slots'].get('0x58')
        if not isinstance(slot,dict) or not slot.get('function_end'):return {}
        from capstone import Cs,CS_ARCH_X86,CS_MODE_64
        from capstone.x86 import X86_OP_MEM
        va,end=integer(slot['va']),integer(slot['function_end'])
        if not BASE<=va<end<=BASE+0x4000000:return {}
        cs=Cs(CS_ARCH_X86,CS_MODE_64);cs.detail=True
        sites={i.address for i in cs.disasm(bytes(self.uc.mem_read(va,end-va)),va)
               if i.mnemonic=='call' and i.operands and i.operands[0].type==X86_OP_MEM and i.operands[0].mem.disp==0x10
               and i.reg_name(i.operands[0].mem.base) not in('rip','rsp')}
        found={};current=[0];before=dict(self.shadow)
        def at_site(u,address,size,data):
            if address in sites:
                found[current[0]]=(u.reg_read(UC_X86_REG_RCX),u.reg_read(UC_X86_REG_EDX))
                u.reg_write(UC_X86_REG_RIP,STOP);u.emu_stop()
        handle=self.uc.hook_add(UC_HOOK_CODE,at_site,begin=va,end=end-1)
        try:
            for i in range(len(self.maxima)):
                current[0]=i
                try:self.call(va,self.parent,i)
                except RuntimeError:self.failure=None
        finally:self.uc.hook_del(handle)
        if before!=self.shadow:raise ValueError('original public getter probe altered coefficient shadow')
        default=self.public_receiver or self.this
        return {i:r for i,r in found.items()
                if r!=(default,i) and self.parent<=r[0]<self.parent+self.parent_span() and self.is_parameter_block_object(r[0])}

    def read_loaded(self,expected):
        """Read back the loaded vector; when the profile PB's getter disagrees with the original normalizer, read
        through the original public getter's block routing instead."""
        try:actual=self.read_values()
        except ValueError:actual=None
        if actual!=tuple(expected) and self.routes is None:
            self.routes=self.route_public_getter();actual=None
        return actual if actual is not None and self.routes is None else self.read_values()

    def vcall(self,slot,*args):
        route=self.routes.get(args[0]) if self.routes and args and slot in(8,0x10,0x20) else None
        if route:
            fn=self.rq(self.rq(route[0])+slot)
            if not BASE<=fn<BASE+0x4000000:raise ValueError('routed block slot outside original image '+hex(slot))
            return self.call(fn,route[0],route[1],*args[1:])
        public=hex(slot) in getattr(self,'public_slots',{})
        this=self.public_receiver if public else self.this
        fn=self.rq(self.rq(this)+slot)
        expected=(self.public_slots if public else self.parameter_block.get('slots',{})).get(hex(slot))
        if expected and fn!=integer(expected['va']if isinstance(expected,dict)else expected):
            raise ValueError('discovered virtual slot differs '+hex(slot))
        return self.call(fn,this,*args)

    def read_ceiling(self):
        """Readback bound per index. A zero raw maximum means the original table defines no domain (the normalizer
        only scales by 32768), so the original setter's own clamp may read back slightly above 32768 (e.g. delay
        times); those indices are bounded by the 16-bit word the original block stores."""
        raw=self.profile['parameters']['raw_maxima']
        return tuple(m if r else 0xffff for m,r in zip(self.maxima,raw))

    def read_values(self):
        values=tuple(self.vcall(0x10,i)&0xffffffff for i in range(len(self.maxima)))
        if any(v>m for v,m in zip(values,self.read_ceiling())):
            raise ValueError('original getter outside original domain: '+repr(values))
        return values

    def format_parameter(self,index):
        before=dict(self.shadow);ptr=self.vcall(0x20,index,0xffffffff)
        raw=bytes(self.uc.mem_read(ptr,512));end=raw.find(b'\0')
        if end<0:raise ValueError('original formatter missing bounded terminator')
        value=raw[:end].decode('ascii')
        if before!=self.shadow:raise ValueError('original formatter altered coefficient shadow')
        return dict(raw=value,console_trimmed=value.strip())

    def discover_program_layout(self,spec):
        """Trace the source loader with tagged float fields to recover its persisted layout.

        When the tagged vector leaves parameters derived or unread, the loader's own
        integer header compares (`cmp dword ptr [program+k], imm`) select other branches.
        Exactly one header value whose branch reads every parameter from the persisted
        vector is accepted; none or several keeps the tagged result (refusal downstream).
        A tagged probe that the original loader traps or rejects in a header-selected
        branch is retried the same way; then the single completing branch is used (its
        derived indices stay subject to the exact captured-vector check), otherwise
        the original error stands."""
        if spec.get('kind')=='record_copy_chunk':
            from base_init import recover_storage_layout
            layout=recover_storage_layout(self,spec)
            if layout is not None:self.program_layout=layout;return layout
            from base_init import recover_chunk_layout
            layout=recover_chunk_layout(self,spec)
            if layout is not None:self.program_layout=layout;return layout
        layout,compares,failure=self._probe_program_layout(spec,{})
        if layout is None or layout['derived']:
            completed=[]
            for offset,value in sorted(compares):
                candidate,_,_=self._probe_program_layout(spec,{offset:value})
                if candidate is not None:completed.append(candidate)
            variants=[c for c in completed if not c['derived']]
            if len(variants)==1:layout=variants[0]
            elif failure is not None and len(completed)==1:layout=completed[0]
            elif failure is not None:raise failure
        if layout is not None and not layout.get('header'):
            # A flag word the loader bit-tests (`test al, imm`) reads as whatever the float tag's bits happen to
            # be in the probe, but the real program image leaves it zero. When setting every bit of that word
            # loads at least the same fields at the same offsets, record it as the deterministic header.
            for offset,value in sorted(c for c in compares if c[1]==0xffffffff):
                candidate,_,_=self._probe_program_layout(spec,{offset:value})
                if candidate is not None and candidate['fields'].items()>=layout['fields'].items() \
                   and set(candidate['derived'])<=set(layout['derived']):layout=candidate;break
        if (layout is None or layout.get('unloaded')) and failure is None and spec.get('layout_contract')=='tagged_dynamic_required':
            # Loaders that clamp values to [0,1] and convert them through the original
            # normalizer slot erase out-of-range tags: retry once with in-range tags and
            # the normalizer traced as an (index, value) reader too; prefer it when it
            # recovers more indices from the program vector.
            retry,_,_=self._probe_program_layout(spec,{},in_range=True)
            if retry is not None and (layout is None or len(retry.get('unloaded',()))<len(layout['unloaded'])):layout=retry
        if layout is not None:self.program_layout=layout
        return layout

    def _probe_program_layout(self,spec,header,in_range=False):
        probe=GenericCapture(self.profile,auto_start=False)
        size=0x1000;ptr=probe.reserve(size);probe.wd(ptr,integer(spec['program_version']))
        raw_to_offset={}
        for offset in range(4,size,4):
            if offset in header:probe.wd(ptr+offset,header[offset]);continue
            value=(0.25+offset/100000.)if in_range else 1.0+offset/10000.
            probe.wf(ptr+offset,value)
            raw_to_offset[struct.unpack('<I',struct.pack('<f',value))[0]]=offset
        reads=set()
        probe.uc.hook_add(UC_HOOK_MEM_READ,lambda u,access,address,size_,value,data:
            reads.add((address-ptr,size_,u.reg_read(UC_X86_REG_RIP))),begin=ptr,end=ptr+size-1)
        calls=[];stub=STUBS+0xe000
        def traced_setter():
            raw=probe.uc.reg_read(UC_X86_REG_XMM2)&0xffffffff
            calls.append((probe.arg(1)&0xffffffff,raw_to_offset.get(raw)))
            return 0
        probe.handlers[stub]=('source_program_layout_setter',traced_setter)
        probe.wq(probe.parentvt+0x50,stub)
        for slot in self.loader_vector_setter_slots(spec):probe.wq(probe.parentvt+slot,stub)
        if in_range:probe.wq(probe.parentvt+integer(self.profile['parent']['normalizer']['slot']),stub)
        failure=None
        try:
            from base_init import loader_extra_args
            extra=loader_extra_args(spec);rv=probe.call(integer(spec['va']),probe.parent,ptr,*extra)
            if not extra and rv!=1:
                failure=ValueError('source program loader rejected tagged layout probe')
        except RuntimeError as exc:
            failure=exc
            # The probe is a separate instance: surface its trapped host call as ours.
            if getattr(probe,'last_unknown',None) is not None:self.last_unknown=probe.last_unknown
        compares=set()
        from capstone import Cs,CS_ARCH_X86,CS_MODE_64
        from capstone.x86 import X86_OP_MEM,X86_OP_IMM
        md=Cs(CS_ARCH_X86,CS_MODE_64);md.detail=True
        for offset,width,pc in reads:
            if width!=4 or offset<=0 or offset%4 or offset in header:continue
            ins=next(md.disasm(bytes(probe.uc.mem_read(pc,16)),pc),None)
            if ins is not None and ins.mnemonic=='cmp' and len(ins.operands)==2 and ins.operands[0].type==X86_OP_MEM \
               and ins.operands[0].size==4 and ins.operands[1].type==X86_OP_IMM:
                compares.add((offset,ins.operands[1].imm&0xffffffff))
            elif ins is not None and ins.mnemonic in('mov','test','and','bt') and ins.operands and ins.operands[0].type!=X86_OP_IMM \
                 and any(o.type==X86_OP_MEM and o.size==4 for o in ins.operands):
                # Integer (flag-word) read into a register or bit-tested in memory: the loader
                # gates whole vector blocks on its bits (`test al, imm`), which a float tag
                # leaves arbitrary. Try all bits set as one more header branch.
                compares.add((offset,0xffffffff))
        if failure is not None:return None,compares,failure
        observed={}
        for index,offset in calls:
            if index<len(self.maxima):observed.setdefault(index,[]).append(offset)
        fields={};derived=[];unloaded=[]
        for index in range(len(self.maxima)):
            values=observed.get(index)
            # Never set by the loader (e.g. excluded by its own index mask): like a
            # derived index, a program cannot carry it.
            if not values:unloaded.append(index);continue
            offsets={value for value in values if value is not None}
            if len(offsets)>1:return None,compares,None
            if offsets:fields[index]=offsets.pop()
            elif all(value is None for value in values):derived.append(index)
            else:return None,compares,None
        if not fields:return None,compares,None
        layout=dict(size=size,fields=fields,derived=tuple(sorted(derived+unloaded)))
        if unloaded:layout['unloaded']=tuple(unloaded)
        if header:layout['header']=dict(header)
        return layout,compares,None

    @staticmethod
    def _parent_vtable_registers(prefix):
        """Registers holding the loader receiver's (parent's) vtable after a linear prefix:
        receiver copies start from rcx (`mov X, holder`); `mov Y, qword ptr [holder]`
        loads its vtable. Any other write to a register drops it."""
        holders={'rcx'};vtables=set()
        for ins in prefix:
            dst,_,src=ins.op_str.partition(', ')
            if ins.mnemonic=='call':holders&={'rbx','rsi','rdi','rbp','r12','r13','r14','r15'};vtables.clear();continue
            if ins.mnemonic not in('mov','lea','add','sub','xor','and','or','inc','dec','movsxd','movzx','imul','shl','shr','sar','neg'):continue
            if dst.startswith(('qword ptr','dword ptr','byte ptr','word ptr','xmm')):continue
            holders.discard(dst);vtables.discard(dst)
            if ins.mnemonic=='mov' and src in holders:holders.add(dst)
            elif ins.mnemonic=='mov' and src.startswith('qword ptr [') and src[11:-1] in holders:vtables.add(dst)
        return vtables

    def loader_vector_setter_slots(self,spec):
        """Parent slots other than 0x50 that the source loader calls right after a
        source-attested vector load (`movss xmm2, [program...]`), with no other call in
        between: the same (index, value) setter ABI reached through another slot."""
        from capstone import Cs,CS_ARCH_X86,CS_MODE_64
        from capstone.x86 import X86_OP_MEM
        md=Cs(CS_ARCH_X86,CS_MODE_64);md.detail=True
        start=integer(spec['va']);insns=list(md.disasm(bytes(self.uc.mem_read(start,0x1000)),start))
        at={ins.address:i for i,ins in enumerate(insns)};slots=set()
        for load in spec.get('vector_loads',[]):
            i=at.get(integer(load['va']))
            if i is None or not insns[i].op_str.startswith('xmm2,'):continue
            for ins in insns[i+1:i+12]:
                if ins.mnemonic in('ret','jmp')or ins.mnemonic.startswith('j'):break
                if ins.mnemonic=='call':
                    op=ins.operands[0]
                    if op.type==X86_OP_MEM and not op.mem.index and op.mem.disp%8==0 and 0<op.mem.disp<0xc0*8 and op.mem.disp!=0x50 \
                       and ins.reg_name(op.mem.base) in self._parent_vtable_registers(insns[:at[ins.address]]):
                        slots.add(op.mem.disp)
                    break
        return sorted(slots)

    def bind_parent_slots_called_from(self,profile,va,end,seen):
        """Bind the source-profiled original functions of parent vtable slots that the original
        function [va,end) calls through the parent's own vtable (transitively), when those slots
        are still unmodeled host stubs."""
        from capstone import Cs,CS_ARCH_X86,CS_MODE_64
        from capstone.x86 import X86_OP_MEM,X86_OP_IMM
        cs=Cs(CS_ARCH_X86,CS_MODE_64);cs.detail=True
        # Profiled function_end can be a single unwind chunk (e.g. set_parameter 0x50 on ATR-102 ends before
        # its direct-call tail); extend to the first int3 padding.
        for ins in cs.disasm(bytes(self.uc.mem_read(va,0x1000)),va):
            if ins.mnemonic=='int3':break
            end=max(end,ins.address+ins.size)
        insns=list(cs.disasm(bytes(self.uc.mem_read(va,end-va)),va));md_slots=profile['parent']['slots']
        for k,ins in enumerate(insns):
            if ins.mnemonic in('call','jmp') and ins.operands and ins.operands[0].type==X86_OP_IMM:
                # Direct call into another original function: follow it.
                target=ins.operands[0].imm
                if BASE<=target<BASE+0x4000000 and not va<=target<end and target not in seen:
                    seen.add(target);self.bind_parent_slots_called_from(profile,target,target,seen)
                continue
            if ins.mnemonic not in('call','jmp') or not ins.operands or ins.operands[0].type!=X86_OP_MEM:continue
            op=ins.operands[0];disp=op.mem.disp
            if op.mem.index or disp%8 or not 0<disp<0xc0*8 or disp in seen:continue
            if ins.reg_name(op.mem.base) not in self._parent_vtable_registers(insns[:k]):continue
            if self.rq(self.parentvt+disp)!=STUBS+0x2000+(disp//8)*16:continue
            slot=md_slots.get(hex(disp))
            if not isinstance(slot,dict) or not slot.get('function_end'):continue
            sva,send=integer(slot['va']),integer(slot['function_end'])
            if not BASE<=sva<send<=BASE+0x4000000:continue
            seen.add(disp);self.wq(self.parentvt+disp,sva);self.bind_parent_slots_called_from(profile,sva,send,seen)

    def load_program(self):
        spec=self.profile['startup']['program_loader'];capture=self.profile['captured_metadata']
        raw=Path(capture['path']).read_bytes()
        if hashlib.sha256(raw).hexdigest()!=capture['sha256']:raise ValueError('captured program metadata identity differs')
        props={}
        for item in json.loads(raw)['controls']:
            if '/parameters/'not in item['path']:continue
            index,key=item['path'].split('/parameters/')[1].split('/')
            props.setdefault(int(index),{})[key]=item
        if set(props)!=set(range(len(self.maxima))):raise ValueError('complete captured program vector required')
        self.captured_properties=props
        self.synthetic_metadata=json.loads(raw).get('schema')=='openapollo-synthetic-metadata/1'
        self.phase='original_captured_program_load'
        layout=self.discover_program_layout(spec)
        if layout is not None and layout.get('storage'):
            from base_init import apply_storage_program
            apply_storage_program(self,spec,layout,props);return self._check_loaded_program(layout,props)
        if layout is None:
            if spec.get('layout_contract')=='tagged_dynamic_required':
                raise ValueError('source program loader tagged layout was not dynamically recovered')
            ptr=self.reserve(integer(spec['value_offset'])+4*len(self.maxima))
            self.wd(ptr,integer(spec['program_version']))
            for i in range(len(self.maxima)):self.wf(ptr+integer(spec['value_offset'])+4*i,props[i]['NormalizedValue']['value'])
        else:
            ptr=self.reserve(layout['size'])
            self.wd(ptr,integer(spec['program_version']))
            for offset,value in layout.get('header',{}).items():self.wd(ptr+offset,value)
            for i,offset in layout['fields'].items():
                self.wf(ptr+offset,props[i]['NormalizedValue']['value'])
            from base_init import write_int_fields
            write_int_fields(self,ptr,layout,props)
        parent=self.profile['parent'];normalizer=parent['normalizer']
        self.wq(self.parentvt+0x50,integer(parent['set_parameter']['va']))
        sp=parent['set_parameter'];self.bind_parent_slots_called_from(self.profile,integer(sp['va']),integer(sp.get("function_end") or sp["va"]),{0x50})
        for slot in self.loader_vector_setter_slots(spec):
            source_slot=parent['slots'].get(hex(slot))
            if not isinstance(source_slot,dict) or not source_slot.get('function_end'):
                raise ValueError('loader vector setter slot lacks source-profiled function '+hex(slot))
            va,end=integer(source_slot['va']),integer(source_slot['function_end'])
            if not BASE<=va<end<=BASE+0x4000000:raise ValueError('loader vector setter slot outside original image')
            self.wq(self.parentvt+slot,va)
            # Attested parent slots reached from the setter by (nested) direct calls.
            self.bind_parent_slots_called_from(self.profile,va,end,{slot})
        self.wq(self.parentvt+integer(normalizer['slot']),integer(normalizer['va']))
        self.phase='original_captured_program_load';self.watch_program_loader(integer(spec['va']))
        from base_init import loader_extra_args
        extra=loader_extra_args(spec)
        if extra and layout is not None:
            from base_init import apply_derived_parameters
            apply_derived_parameters(self,layout,props)
        result=self.call(integer(spec['va']),self.parent,ptr,*extra)
        if not extra and result!=1:raise ValueError('original captured program loader rejected version/vector')
        self._check_loaded_program(layout,props)

    def _check_loaded_program(self,layout,props):
        normalizer=self.profile['parent']['normalizer']
        expected=[]
        for i in range(len(self.maxima)):
            bits=struct.unpack('<I',struct.pack('<f',props[i]['NormalizedValue']['value']))[0]
            self.uc.reg_write(UC_X86_REG_XMM2,bits)
            expected.append(self.call(integer(normalizer['va']),self.parent,i)&0xffffffff)
        actual=self.read_loaded(expected)
        derived=set((layout or {}).get('derived',()))
        preserved=[i for i,(e,a) in enumerate(zip(expected,actual)) if e!=a and i in derived]
        if preserved and len(preserved)==sum(e!=a for e,a in zip(expected,actual)):
            # The original loader never reads these indices from the program vector (the
            # tagged probe saw them set from object state, e.g. Wet Solo), so a program
            # cannot carry them; their captured state reached the plug-in through the
            # public setter, which is applied here. Any other mismatch still refuses.
            self.phase='captured_non_program_parameters'
            for i in preserved:self.vcall(8,i,expected[i],0)
            self.captured_non_program_indices=preserved
            actual=self.read_values()
        if tuple(expected)!=actual and getattr(self,'synthetic_metadata',False):
            # No Console capture: the plug-in's own loader is the authority on the vector it accepts.
            self.synthetic_loader_corrections=[(i,e,a) for i,(e,a) in enumerate(zip(expected,actual)) if e!=a]
            expected=list(actual)
        if tuple(expected)!=actual:raise ValueError('captured vector original-normalizer/getter mismatch '+repr([(i,e,a) for i,(e,a) in enumerate(zip(expected,actual)) if e!=a]))
        self.captured_integer_values=tuple(expected)

    def start(self):
        self.load_program()
        self.phase='original_resume';self.vcall(0x38)
        from base_init import settle_commit_state
        settle_commit_state(self)
        self.phase='startup_readback';self.defaults_values=self.read_values()
        self.defaults=dict(self.shadow);self.startup_events=list(self.events)
        self.startup_formatted=[self.format_parameter(i)for i in range(len(self.maxima))]
        self._verify_replay()
        self.host_step_text=[]
        for i,text in enumerate(self.startup_formatted):
            captured=self.captured_properties[i]
            if "StringValue" not in captured:continue  # synthetic metadata: no Console text oracle
            if text['console_trimmed']!=captured['StringValue']['value'].strip():
                # A stepped parameter the original formatter never formats (its default branch writes an empty
                # string) is displayed by the host from the captured step list: accept only when the captured
                # text is exactly the step the loaded normalized value selects.
                steps=(captured.get('StepValue')or{}).get('values')or[]
                shown=captured['StringValue']['value']
                if text['raw']==''and shown in steps and steps.index(shown)==round(captured['NormalizedValue']['value']*(len(steps)-1)):
                    self.host_step_text.append(i);continue
                raise ValueError('captured original formatter mismatch '+repr((i,text,shown)))
        if self.defaults_values!=self.captured_integer_values:raise ValueError('resume changed captured program values')
        self.phase='ready'

    def _verify_replay(self):
        replay=Replay();replay.apply(self.events)
        if replay.words!=self.shadow:raise ValueError('shared event replay differs from complete original shadow')

    def set_control(self,index,value):
        if type(index)is not int or not 0<=index<len(self.maxima)or type(value)is not int or not 0<=value<=self.maxima[index]:
            raise ValueError('integer original domain required')
        self.phase='control_'+str(index);self.vcall(8,index,value,0)
        actual=self.vcall(0x10,index)&0xffffffff
        if actual>self.read_ceiling()[index]:raise ValueError('setter/getter outside original domain '+repr((index,value,actual)))
        self._verify_replay();return actual,dict(self.shadow)

    def registry_key_absent(self,out_arg=4,status=2):
        """Bounded headless RegOpenKeyExA (status 2) / RegCreateKeyExA (out arg 7, ERROR_ACCESS_DENIED 5):
        no registry fixture is supplied, so the key is absent / not writable and no handle is returned."""
        out=self.arg(out_arg)
        large=self.parent==LARGE_PARENT and LARGE_PARENT<=out<=LARGE_PARENT+self.parent_allocation-8
        if not (HEAP <= out <= HEAP + 0x7efff8 or STACK <= out <= STACK + 0xffff8 or large):
            raise ValueError('registry output handle is outside bounded fixture memory')
        self.wq(out,0)
        self.last_error=status
        return status

    def is_parameter_block_object(self,address):
        """True when the object's original image vtable carries MSVC RTTI whose class
        hierarchy includes CHypParamBlock (an embedded child parameter block)."""
        vt=self.rq(address)
        if not BASE<=vt<BASE+0x4000000:return False
        try:
            col=self.rq(vt-8);rd=lambda a:struct.unpack('<I',bytes(self.uc.mem_read(a,4)))[0]
            if rd(col)!=1:return False
            hierarchy=BASE+rd(col+16);count=rd(hierarchy+8);bases=BASE+rd(hierarchy+12)
            for i in range(min(count,64)):
                td=BASE+rd(BASE+rd(bases+4*i))
                if bytes(self.uc.mem_read(td+16,24)).split(b'\0')[0]==b'.?AVCHypParamBlock@@':return True
        except Exception:return False
        return False

    def parent_callback(self):
        """Parent slot 0x4e0: original bodies forward `[this+0x100]->vt[0x118](edx)`,
        i.e. IUADPlug UpdateRsrcProfile(showMessage): 0 = success/nothing to do."""
        if self.arg(0)!=self.parent or self.arg(1)&0xffffffff not in (0,1):
            raise ValueError('unexpected parent lifecycle request')
        return 0

    def update_resource_profile(self):
        if self.arg(1)&0xffffffff not in (0,1):raise ValueError('UpdateRsrcProfile argument must be 0 or 1')
        return 0

    def topology_sample_rate(self):
        rate=struct.unpack('<f',bytes(self.uc.mem_read(self.fs,4)))[0]
        self.uc.reg_write(UC_X86_REG_XMM0,struct.unpack('<I',struct.pack('<f',rate))[0])
        return 0

    def current_directory_absent(self):
        """Headless GetCurrentDirectoryW: no process directory; returns 0 and leaves the
        buffer untouched. The observed caller pre-terminates the buffer (L"") and
        ignores the result, using the buffer as a string either way."""
        size,buffer=self.arg(0)&0xffffffff,self.arg(1)
        if not 0<size<=0x8000 or not buffer:raise ValueError('bounded GetCurrentDirectoryW buffer required')
        self.last_error=2
        return 0

    def find_file_absent(self):
        """Headless FindFirstFileW: no files; INVALID_HANDLE_VALUE with ERROR_FILE_NOT_FOUND.
        The observed enumerator stores the handle, compares it with -1 and returns false."""
        if not self.arg(0) or not self.arg(1):raise ValueError('FindFirstFileW requires pattern and data pointers')
        self.last_error=2
        return 0xffffffffffffffff

    def shell_folder_absent(self):
        """Headless SHGetSpecialFolderPathW: no shell folders; returns FALSE and leaves
        the buffer untouched. Original callers test the BOOL and take their own default."""
        if self.arg(0)!=0 or self.arg(3)&0xffffffff:raise ValueError('only hwnd=NULL, fCreate=FALSE shell folder queries are modeled')
        return 0

    def set_last_error(self):
        self.last_error=self.arg(0)&0xffffffff
        return 0

    def notify(self,kind=b'?PropagateUpdateToRte@CHypParamBlock@@UEAAXII@Z'):
        # Shared SDK uint32 output notification ABI, under the explicit headless
        # contract: no registered observer callbacks or UI listener dirty lists.
        # Public set_control domains remain independent and unchanged.
        receiver=self.arg(0)
        # Two-PB parents: the source-derived parent PB pointer field designates the
        # parent-bound PB, and the original framework setter notifies on that PB.
        bound=getattr(self,'parent_bound',None) if getattr(self,'parent_pb_pointer_target',None)=='parent_bound_parameter_block' else None
        # Attached delegate: a framework member built by its original constructor and
        # stored at [PB+0x100] by the original attach path (parent slot 0x578).
        members={self.parent+int(m['offset'],16) for m in getattr(self,'framework_members',[])}
        delegate=receiver if receiver in members and self.rq(self.this+0x100)==receiver else None
        # Embedded child PBs live anywhere in the parent object, before or after the primary PB.
        nested=receiver if self.parent<=receiver<self.this+0x10000 and self.is_parameter_block_object(receiver) else None
        if receiver!=self.this and receiver!=bound and receiver!=delegate and receiver!=nested:
            raise ValueError('notification receiver is not the initialized parameter block')
        if self.rq(receiver+8)!=self.rq(receiver+16)or self.rd(receiver+0x20)!=0:
            raise ValueError('headless notification sink requires zero registered observers and UI listeners')
        index=self.arg(1)&0xffffffff
        if kind==b'?PropagateUpdateToRte@CHypParamBlock@@UEAAXII@Z':
            category='rte';payload=self.arg(2)&0xffffffff;payload_type='uint32_value'
        elif kind==b'?PropagateUpdateToControls@CHypParamBlock@@QEAAXIPEAX@Z':
            category='controls';payload=self.arg(2);payload_type='observer_context_pointer'
        else:raise ValueError('unknown notification ABI')
        return_va=self.rq(self.uc.reg_read(UC_X86_REG_RSP))
        self.notifications.append((index,payload))
        self.notification_events.append(dict(index=index,category=category,payload=payload,
            payload_type=payload_type,index_class=('public'if index<len(self.maxima)else'opaque_internal')if receiver==self.this else('parent_bound_block'if receiver==bound else'attached_delegate'if receiver==delegate else'nested_parameter_block'),
            return_va=hex(return_va),receiver=hex(receiver)))
        return 0

    def key_state(self):
        # Headless host: no key is pressed or toggled, so GetKeyState returns 0 for
        # any valid virtual-key code (observed callers test bit 15 for Shift/Ctrl).
        if not 0x01<=self.arg(0)&0xffffffff<=0xfe:raise ValueError('unmodeled key query')
        return 0

    def begin(self):
        start,end=self.arg(1),self.arg(2)
        if not (0<=start<end<=0x80000000 and (start & 0xff000000)==((end-1) & 0xff000000)):raise ValueError(f'bounded source atomic range required start={start:#x} end={end:#x}')
        self.sequence_counters.add(self.arg(3))
        result=super().begin();self.atomic_refs.update(range(start,end+1))
        self.events.append(['begin',start,end,self.shadow[start]])
        return result

    def end(self):
        end,value=self.arg(1),self.arg(2)&0xffffffff
        result=super().end();self.events.append(['end',end,value]);return result

    def set_int(self):
        result=super().set_int();ref=self.arg(1)
        counter=self.incremented_counter()
        if counter is not None and self.rd(counter)==self.shadow[ref]:self.sequence_counters.add(counter)
        self.sequence_words[ref]=self.shadow[ref] if any(self.shadow[ref]==self.rd(c) for c in self.sequence_counters) else None
        self.random_words[ref]=self.shadow[ref] if self.pending_rand is not None and self.pending_rand==self.shadow[ref] else None
        self.pending_rand=None
        self.note_loader_event();self.events.append(['write',ref,[self.shadow[ref]]]);return result

    def note_loader_event(self):
        if self.loader_depth>0:self.loader_event_indices.add(len(self.events))

    def watch_program_loader(self,va):
        """Track original program-loader activity (entry .. its own return)."""
        if getattr(self,'_watched_loader',None)==va:return
        self._watched_loader=va
        def entered(u,address,size,data):
            ret=self.rq(u.reg_read(UC_X86_REG_RSP));self.loader_depth+=1;self.loader_calls+=1
            holder=[]
            def left(u2,a2,s2,d2):
                self.loader_depth-=1;u2.hook_del(holder[0])
            holder.append(u.hook_add(UC_HOOK_CODE,left,begin=ret,end=ret))
        self.uc.hook_add(UC_HOOK_CODE,entered,begin=va,end=va)

    _ALIASES={'rax':'eax ax al ah','rbx':'ebx bx bl bh','rcx':'ecx cx cl ch','rdx':'edx dx dl dh',
              'rsi':'esi si sil','rdi':'edi di dil','rbp':'ebp bp bpl','rsp':'esp sp spl'}

    @classmethod
    def _family(cls,name):
        if name in cls._ALIASES:return {name,*cls._ALIASES[name].split()}
        return {name,name+'d',name+'w',name+'b'}

    def incremented_counter(self):
        """Counter address when the original call block is `inc dword ptr [base+disp]`
        followed by `mov r8d, dword ptr [base+disp]` (the set_int value operand), with
        base and r8d unmodified up to the call. Any other shape returns None."""
        if len(self.last_blocks)<2:return None
        start=self.last_blocks[-2];ret=self.rq(self.uc.reg_read(UC_X86_REG_RSP))
        if not 0<ret-start<=0x400:return None
        sites=self.__dict__.setdefault('_counter_sites',{})
        if (start,ret) not in sites:sites[(start,ret)]=self._decode_counter_site(start,ret)
        site=sites[(start,ret)]
        if site is None:return None
        import unicorn.x86_const as x86
        return (self.uc.reg_read(getattr(x86,'UC_X86_REG_'+site[0].upper()))+site[1])&0xffffffffffffffff

    def _decode_counter_site(self,start,ret):
        from capstone import Cs,CS_ARCH_X86,CS_MODE_64
        from capstone.x86 import X86_OP_MEM
        md=Cs(CS_ARCH_X86,CS_MODE_64);md.detail=True
        insns=list(md.disasm(bytes(self.uc.mem_read(start,ret-start)),start))
        if not insns or insns[-1].address+insns[-1].size!=ret or insns[-1].mnemonic!='call':return None
        def memory(ins,index):
            op=ins.operands[index]
            if op.type!=X86_OP_MEM or op.size!=4 or op.mem.index or not op.mem.base or op.mem.segment:return None
            base=ins.reg_name(op.mem.base)
            return None if base=='rip' else (base,op.mem.disp)
        incremented=set();site=None
        for ins in insns[:-1]:
            written={ins.reg_name(r)for r in ins.regs_access()[1]}
            if site is not None and (written&self._family('r8') or written&self._family(site[0])):site=None
            incremented={m for m in incremented if not written&self._family(m[0])}
            if ins.mnemonic=='inc' and len(ins.operands)==1 and memory(ins,0):incremented.add(memory(ins,0))
            elif ins.mnemonic=='mov' and len(ins.operands)==2 and ins.reg_name(ins.operands[0].reg)=='r8d' and memory(ins,1) in incremented:
                site=memory(ins,1)
        return site

    def set_float(self):
        result=super().set_float();ref=self.arg(1)
        self.note_loader_event();self.events.append(['write',ref,[self.shadow[ref]]]);return result

    def shadow_word(self,ref):
        """Initial-zero ordinary shadow word for a readable ref, else refuse.
        SDK Init memsets ordinary (flags&2==0) memories to 0 (analysis/chain-shadow-
        initialization-contract.txt); _getMemInfo rejects memory>=memCount, offset past the
        descriptor dwords and flags&2 memories (analysis/chain-variable-contract.txt).
        Memory index = row index of the captured descriptor rows (pool,dwords,link,flags)."""
        if self.descriptor is None:
            # Constructor-time reads (parent 0x538 program-state init) precede descriptor extraction, so the
            # rows are not known yet. Ordinary memories read 0 until written; record the ref and let
            # check_deferred_reads reject it once the rows exist if it was not readable (same bounds below).
            self.deferred_reads.add(ref);return self.shadow.get(ref,0)
        rows=self.descriptor.get('rows')
        mem,off=ref>>24,ref&0xffffff
        if not rows or mem>=len(rows) or off>=rows[mem][1] or rows[mem][3]&2:
            raise ValueError('cache getter of unreadable ref %#x'%ref)
        return self.shadow.get(ref,0)

    def check_deferred_reads(self):
        """Apply shadow_word readability to refs read before the descriptor rows existed."""
        for ref in sorted(self.deferred_reads):self.shadow_word(ref)

    def get_var(self):
        # Cache slots 0xc0/0xc8 = GetIntVar/GetFloatVar(ref, out*): read counterparts of
        # set_int(0xa0)/set_float(0xa8); a float shares the raw 32-bit word (same doc).
        ref,out=self.arg(1)&0xffffffff,self.arg(2)
        self.wd(out,self.shadow_word(ref));return 0

    def get_array(self):
        # Cache slots 0xd0/0xd8 = GetIntArray/GetFloatArray(ref, out*, count, stride, flags):
        # read counterparts of set_array (0xb0/0xb8); contiguous out words, bounds as get_var.
        ref,ptr,count,stride,flags=[self.arg(i)for i in range(1,6)]
        ref&=0xffffffff;count&=0xffffffff;stride&=0xffffffff;flags&=0xffffffff
        if not (0<count and stride==1 and flags==0):
            raise ValueError('unsupported shared array ABI '+repr((ref,count,stride,flags)))
        words=[self.shadow_word(ref+i)for i in range(count)]
        for i,w in enumerate(words):self.wd(ptr+4*i,w)
        return 0

    def set_array(self):
        ref,ptr,count,stride,flags=[self.arg(i)for i in range(1,6)]
        count&=0xffffffff;stride&=0xffffffff;flags&=0xffffffff
        if not (0<=ref<0x80000000 and 0<count and stride==1 and flags==0 and (ref&0xffffff)+count<=0x1000000):
            raise ValueError('unsupported shared array ABI '+repr((ref,count,stride,flags)))
        values=[self.rd(ptr+4*i)for i in range(count)]
        self.shadow.update({ref+i:v for i,v in enumerate(values)})
        live={self.rd(c) for c in self.sequence_counters}
        self.sequence_words.update({ref+i:(v if v in live else None) for i,v in enumerate(values)})
        self.note_loader_event();self.events.append(['write',ref,values]);return 0

    def header_timing(self):
        result=super().header_timing()
        double=lambda v:struct.unpack('<d',struct.pack('<Q',v&0xffffffffffffffff))[0]
        self.note_loader_event();self.events.append(['timing',self.arg(1),self.arg(2),double(self.uc.reg_read(UC_X86_REG_XMM3)),double(self.arg(4))])
        return result
