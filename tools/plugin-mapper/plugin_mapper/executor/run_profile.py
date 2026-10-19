"""One generated-profile offline execution. Parent batch runner owns cohort/results."""
from pathlib import Path
import argparse, hashlib, json, sys, time, traceback
from generic_capture import GenericCapture
from unicorn.x86_const import UC_X86_REG_RIP


def shadow_json(shadow):return {hex(k):v for k,v in sorted(shadow.items())}
def digest(value):return hashlib.sha256(json.dumps(value,sort_keys=True,separators=(',',':')).encode()).hexdigest()

def sequence_word(words,shadow,ref):
    """Ref whose current value was written by set_int as the live original bracket counter."""
    return words.get(ref) is not None and words[ref]==shadow.get(ref)

def setter_recomputed(profile,descriptor_extractor,index,original,refs,before,after,cache):
    """Refs whose restored value is exactly what the original setter writes when it
    re-applies the unchanged original value on a fresh, identically started instance,
    while their pre-change value is still the original startup (resume-path) value.
    The difference then comes from startup vs setter code paths, not the tested value."""
    if not refs:return set()
    if index not in cache:
        fresh=GenericCapture.__new__(GenericCapture,profile);fresh.__init__(profile,auto_start=False)
        if descriptor_extractor is not None:fresh.descriptor=descriptor_extractor(fresh,profile)
        fresh.start()
        if descriptor_extractor is not None:descriptor_extractor(fresh,profile)
        startup=dict(fresh.shadow)
        if fresh.read_values()[index]!=original:raise ValueError('fresh startup original value differs')
        fresh.set_control(index,original);cache[index]=(startup,dict(fresh.shadow))
    startup,reapplied=cache[index]
    if all(before.get(k)==startup.get(k)!=after.get(k)==reapplied.get(k) for k in refs):return set(refs)
    return set()

LOAD_UNITS=0xaa8  # original AllocInfo DSP load units (HANDOFF 5.5); +0xaac is flags

def watch_load_units(descriptor_extractor):
    """Record the original descriptor's load-unit word on every decode (carrier untouched)."""
    seen=[]
    module=getattr(descriptor_extractor,'__globals__',None)
    if module is None or 'decode_descriptor' not in module:return seen,lambda:None
    original=module['decode_descriptor']
    def decode(cap,address,**kwargs):
        seen.append(int.from_bytes(bytes(cap.uc.mem_read(address+LOAD_UNITS,4)),'little'))
        return original(cap,address,**kwargs)
    module['decode_descriptor']=decode
    return seen,lambda:module.__setitem__('decode_descriptor',original)

def load_units_over_controls(cap,profile,descriptor_extractor,seen):
    """After every recorded result: descriptor load units at each swept control value,
    restoring the original vector after each index. Additive only."""
    values={};original=tuple(cap.read_values())
    for index,maximum in enumerate(cap.maxima):
        for value in sorted(set((0,maximum//2,maximum))):
            cap.set_control(index,value);descriptor_extractor(cap,profile)
            values.setdefault(seen[-1],[]).append([index,value])
        for _ in range(len(original)):
            changed=[i for i,(a,b) in enumerate(zip(cap.read_values(),original)) if a!=b]
            if not changed:break
            for i in changed:cap.set_control(i,original[i])
    return values

TIMING_WORDS=(0,1,2,3,4,6)  # header_timing writes these offsets from its ref

def note_writers(last_writer,events,source,shadow):
    for event in events:
        if event[0]=='write':
            for offset,value in enumerate(event[2]):last_writer[event[1]+offset]=(dict(source),value)
        elif event[0]=='timing':
            for offset in TIMING_WORDS:last_writer[event[1]+offset]=(dict(source,event='timing'),shadow.get(event[1]+offset))

def host_pointer(value,cap):
    """Value that addresses host-owned memory this capture actually uses: a live
    private allocation, the parent object, or the host fixtures (interface, vtables,
    lock, sample-rate and string buffers, TLS)."""
    if any(start<=value<start+size for start,size in cap.allocations.items()):return True
    if cap.parent<=value<cap.parent+cap.parent_span():return True
    return cap.iface<=value<cap.tls_slot0+0x1000

def history_words(refs,startup,shadow,last_writer,random_words,request,cap):
    """Owner rule: restore diffs mirror UA's host, which keeps whatever words the
    original setters leave. A differing word is accepted when its final value was
    written by original code in this request's change or restore step (setter,
    loader or header timing); CRT rand() values are class nondeterministic_original,
    others original_history; values pointing into host memory are flagged
    host_pointer. One unattributed word refuses the request (returns [])."""
    words=[]
    for ref in sorted(refs):
        writer=last_writer.get(ref);value=shadow.get(ref)
        if writer is None or writer[1]!=value or (writer[0].get('index'),writer[0].get('requested'))!=request:return []
        kind='nondeterministic_original' if random_words.get(ref) is not None and random_words[ref]==value else 'original_history'
        word=dict(ref=hex(ref),writer=writer[0],value=value,startup_written=ref in startup,**{'class':kind})
        if value is not None and host_pointer(value,cap):word['host_pointer']=True
        words.append(word)
    return words

def program_select(cap,refs,first,loader_calls):
    """A restore whose original setter invoked the original program loader: accepted
    when every remaining changed word was written while that loader was executing
    during the restore step (the readback check already passed)."""
    if not refs or cap.loader_calls==loader_calls:return None
    written=set()
    for position in range(first,len(cap.events)):
        event=cap.events[position]
        if position not in cap.loader_event_indices:continue
        if event[0]=='write':written.update(range(event[1],event[1]+len(event[2])))
        elif event[0]=='timing':written.update(event[1]+offset for offset in TIMING_WORDS)
    if not refs<=written:return None
    return dict(loader=hex(cap._watched_loader),loader_calls=cap.loader_calls-loader_calls,
                refs=[hex(k)for k in sorted(refs)])

def execute(profile,descriptor_extractor=None):
    start=time.monotonic();cap=GenericCapture.__new__(GenericCapture,profile)
    seen,unwatch=watch_load_units(descriptor_extractor)if descriptor_extractor is not None else ([],lambda:None)
    result=dict(schema='openapollo-generic-execution/1',name=profile.get('name'),status='blocked',
                hardware_executed=False,profile_sha256=digest(profile),controls=[],stages={})
    try:
        cap.__init__(profile,auto_start=False)
        result['stages']['constructor_setup']='pass'
        result['crt_initialization']=cap.crt_initialization
        result['crt_thread_initialization']=getattr(cap,'crt_thread_initialization',[])
        result['parent_identity_bindings']=cap.parent_identity_bindings
        result['parameters']=dict(count=len(cap.maxima),effective_maxima=list(cap.maxima),
                                  constructor_values=list(cap.constructor_values))
        if cap.constructor_values_outside_domain:
            result['parameters']['constructor_values_outside_domain']=cap.constructor_values_outside_domain
        if descriptor_extractor is not None:
            cap.phase='original_descriptor'
            result['descriptor']=descriptor_extractor(cap,profile)
            cap.descriptor=result['descriptor'];cap.check_deferred_reads();result['stages']['descriptor']='pass'
        else:result['stages']['descriptor']='not_attempted'
        cap.start();result['stages']['startup']='pass'
        result['startup']=dict(shadow=shadow_json(cap.defaults),events=cap.startup_events,
                    integer_values=list(cap.defaults_values),formatted=cap.startup_formatted,
                    words=len(cap.defaults),initialization_policy='original_constructor_setup_captured_program_loader_resume',metadata_comparisons=dict(original_integer_values=len(cap.maxima),formatted_strings=len(cap.maxima)))
        if descriptor_extractor is not None:
            cap.phase='post_resume_descriptor';result['post_resume_descriptor']=descriptor_extractor(cap,profile)
        write_refs=set();atomic_refs=set(cap.atomic_refs);fresh_cache={};last_writer={}
        for index,maximum in enumerate(cap.maxima):
            original=cap.defaults_values[index]
            for value in sorted(set((0,maximum//2,maximum))):
                first=len(cap.events);before=dict(cap.shadow);before_values=tuple(cap.read_values());before_sequence=dict(cap.sequence_words)
                cap.set_control(index,value);text=cap.format_parameter(index)
                events=cap.events[first:]
                touched=set()
                for e in events:
                    if e[0]=='write':touched.update(range(e[1],e[1]+len(e[2])))
                write_refs.update(touched);atomic_refs.update(cap.atomic_refs)
                note_writers(last_writer,events,dict(index=index,requested=value,step='change'),dict(cap.shadow))
                changed={k for k in before.keys()|cap.shadow.keys()if before.get(k)!=cap.shadow.get(k)}
                request=dict(index=index,requested=value,integer_readback=cap.vcall(0x10,index),
                    formatted=text,events=events,write_refs=[hex(k)for k in sorted(touched)],
                    changed_refs=[hex(k)for k in sorted(changed)],shadow_sha256=digest(shadow_json(cap.shadow)))
                first=len(cap.events)
                restore_steps=[];loader_calls=cap.loader_calls
                for _ in range(len(before_values)):
                    current=tuple(cap.read_values())
                    changed_indices=[item for item,(actual,expected) in enumerate(zip(current,before_values)) if actual!=expected]
                    if not changed_indices:break
                    for restore_index in changed_indices:
                        cap.set_control(restore_index,before_values[restore_index])
                        restore_steps.append(restore_index)
                if tuple(cap.read_values())!=before_values:
                    raise ValueError('restore changed original captured integer vector')
                request['restore']=dict(integer=cap.vcall(0x10,index),formatted=cap.format_parameter(index),
                    events=cap.events[first:],indices=restore_steps)
                differing={k for k in before.keys()|cap.shadow.keys() if k not in cap.atomic_refs and before.get(k)!=cap.shadow.get(k)}
                sequence={k for k in differing if sequence_word(before_sequence,before,k) and sequence_word(cap.sequence_words,cap.shadow,k)}
                if sequence:request['restore']['sequence_counter_refs']=[hex(k)for k in sorted(sequence)]
                recomputed=setter_recomputed(profile,descriptor_extractor,index,original,differing-sequence,before,cap.shadow,fresh_cache)
                if recomputed:request['restore']['setter_recomputed_startup_refs']=[hex(k)for k in sorted(recomputed)]
                note_writers(last_writer,request['restore']['events'],dict(index=index,requested=value,step='restore'),cap.shadow)
                remaining=differing-sequence-recomputed
                history=history_words(remaining,cap.defaults,cap.shadow,last_writer,cap.random_words,(index,value),cap)
                if history:request['restore']['history_words']=history;remaining=remaining-{int(h['ref'],16)for h in history}
                selected=program_select(cap,remaining,first,loader_calls)
                if selected:request['restore']['program_select']=selected;remaining=set()
                differences={hex(k):[before.get(k),cap.shadow.get(k)]for k in remaining}
                if differences:raise ValueError('non-atomic restore shadow differs '+json.dumps(differences,sort_keys=True))
                request['restore']['non_atomic_shadow_equal']=True
                result['controls'].append(request)
        result['stages']['controls']='pass'
        result['original_write_refs']=[hex(k)for k in sorted(write_refs)]
        result['atomic_refs']=[hex(k)for k in sorted(atomic_refs)]
        result['new_control_refs_absent_from_startup']=[hex(k)for k in sorted(write_refs-cap.defaults.keys())]
        result['final_shadow']=shadow_json(cap.shadow)
        result['output_notifications']=cap.notification_events
        result['status']='offline_pass' if descriptor_extractor is not None else 'controls_pass_descriptor_missing'
        result['limitations']=['No device load, licensing admission, audio, meter or active phase qualification',
            'Atomic refs identified from original brackets; active exclusion requires source/runtime qualification',
            'Relative write footprint is original output, not a provider authorization']
        result['output_notifications']=list(cap.notification_events)  # freeze before the additive sweep
        if descriptor_extractor is not None and len(seen)>=2:
            units=dict(load_units_initial=seen[0],load_units=seen[1])
            try:
                cap.phase='load_units_sweep';swept=load_units_over_controls(cap,profile,descriptor_extractor,seen)
                units.update(load_units_max_over_controls=max(swept),load_units_by_value={str(k):v for k,v in sorted(swept.items())})
            except Exception as exc:units['load_units_sweep_error']=type(exc).__name__+': '+str(exc)
            result['descriptor'].update(units)
    except Exception as exc:
        result['error']=dict(type=type(exc).__name__,message=str(exc),phase=getattr(cap,'phase','construction'),
             rip=hex(cap.uc.reg_read(UC_X86_REG_RIP))if hasattr(cap,'uc')else None,
             unknown=getattr(cap,'last_unknown',None),last_host_calls=getattr(cap,'calls',[])[-20:],last_blocks=[hex(x)for x in getattr(cap,'last_blocks',[])],traceback=traceback.format_exc())
        result['partial_shadow']=shadow_json(getattr(cap,'shadow',{}))
        if hasattr(cap,'defaults'):
            result['partial_startup']=dict(shadow=shadow_json(cap.defaults),events=cap.startup_events,integer_values=list(cap.defaults_values))
    unwatch()
    result['seconds']=time.monotonic()-start
    result['executor_sha256']=hashlib.sha256(Path(__file__).with_name('generic_capture.py').read_bytes()).hexdigest()
    return result

def main():
    parser=argparse.ArgumentParser();parser.add_argument('--profile',required=True);parser.add_argument('--output',required=True)
    parser.add_argument('--descriptor-module',help='Generic extractor module file, exports extract_descriptor(cap,profile)')
    args=parser.parse_args();extractor=None
    if args.descriptor_module:
        import importlib.util
        spec=importlib.util.spec_from_file_location('generic_batch_descriptor',args.descriptor_module)
        module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
        extractor=module.extract_descriptor
    result=execute(json.loads(Path(args.profile).read_text()),extractor)
    output=Path(args.output);output.parent.mkdir(parents=True,exist_ok=True)
    output.write_text(json.dumps(result,indent=2)+'\n')
    print(json.dumps(dict(name=result['name'],status=result['status'],stages=result['stages'],controls=len(result['controls']),seconds=result['seconds'],error=result.get('error',{}).get('message'))))
if __name__=='__main__':main()
