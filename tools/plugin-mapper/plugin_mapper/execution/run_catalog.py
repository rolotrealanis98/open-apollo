"""Bounded unattended catalog execution over source-generated profiles; no hardware."""
from pathlib import Path
from concurrent.futures import ThreadPoolExecutor,wait,FIRST_COMPLETED
import argparse,datetime,fcntl,hashlib,importlib,json,os,re,shutil,subprocess,sys,time

HERE=Path(__file__).resolve().parent
BATCH=HERE.parent
# Runs and the cache live in the user's work directory (plugin_mapper sets it), never next to the code.
WORK=Path(os.environ.get('OA_MAPPER_WORK',str(Path.home()/'.cache/open-apollo/plugin-mapper')))/'execution'
STORE=Path(os.environ.get('OA_MODULE_STORE','/lib/firmware/universal-audio/plugins/modules'))

def digest(data):return hashlib.sha256(data).hexdigest()
def canonical(value):return json.dumps(value,sort_keys=True,separators=(',',':')).encode()
def file_hash(path):return digest(Path(path).read_bytes())
def save(path,value):
    path=Path(path);path.parent.mkdir(parents=True,exist_ok=True)
    tmp=path.with_name(path.name+'.tmp')
    tmp.write_text(json.dumps(value,indent=2)+'\n');tmp.replace(path)

def runtime_manifest(executor,descriptor):
    # Import only Python definitions; none of these imports constructs a plugin.
    sys.path.insert(0,str(executor.parent))
    for name in ('generic_capture','dspap_capture','sonnox_capture','softube_capture','vst2_host','vst2_bx_host','vst2_relab_host','base_init','run_profile','thread_runtime','crt_init','parent_fields','capstone','pefile','unicorn'):
        importlib.import_module(name)
    from prove_sdk_constructor import SDK
    files={Path(__file__).resolve(),executor.resolve(),Path(SDK).resolve(),
           executor.parent/'win32-ctype.json',Path(sys.executable).resolve()}
    if descriptor:files.add(descriptor.resolve())
    for module in list(sys.modules.values()):
        name=getattr(module,'__file__',None)
        if name and Path(name).is_file():files.add(Path(name).resolve())
    for package in ('capstone','unicorn'):
        directory=Path(importlib.import_module(package).__file__).parent
        files.update(path.resolve()for path in directory.rglob('*.so*')if path.is_file())
    return dict(schema='openapollo-catalog-executor-runtime/1',
        files={str(path):file_hash(path)for path in sorted(files)},
        python_version=sys.version,pythonpath=os.environ.get('PYTHONPATH',''),
        module_store_index=digest(canonical(sorted(path.name for path in STORE.glob('*.bill')))),
        original_sdk_sha256=file_hash(SDK))

def verify_runtime(manifest):
    return all(Path(path).is_file()and file_hash(path)==sha for path,sha in manifest['files'].items())

def profile_key(profile,manifest):
    return digest(canonical(dict(profile=profile,runtime=manifest)))

def validate_inputs(profile):
    if profile.get('status')!='discovered':raise ValueError('complete discovered profile required')
    for key in ('dll','captured_metadata'):
        item=profile[key]
        if file_hash(item['path'])!=item['sha256']:raise ValueError(key+' identity differs')

def validate_resources(result):
    for key in ('descriptor','post_resume_descriptor'):
        for resource in result.get(key,{}).get('resources',[]):
            sha=resource['sha256'];path=STORE/(sha+'.bill')
            if not path.is_file()or file_hash(path)!=sha:return False
    return True

def bucket(result):
    error=result.get('error')or{}
    phase=error.get('phase',result.get('phase','executor'))
    message=error.get('message','')
    unknown=(error.get('unknown')or{}).get('name')
    if unknown:return dict(stage=phase,kind='missing_host_call',subject=unknown)
    for pattern in (r'(?:unknown import|unmodeled SDK import) ([^\n]+)',
                    r'(?:unmodeled dynamic Win32 export|unmodeled virtual Win32 module) ([^\n]+)'):
        match=re.search(pattern,message)
        if match:return dict(stage=phase,kind='missing_host_call',subject=match.group(1))
    subject=re.sub(r'0x[0-9a-fA-F]+','<address>',message)
    subject=re.sub(r'\b\d{4,}\b','<number>',subject)
    return dict(stage=phase,kind=error.get('type',result.get('status','blocked')),subject=subject[:240])

SLOT_ATTEMPTS=4

def execute_with_slots(profile,job,cfg):
    """Execute the discovery profile; if the original descriptor code traps on a parent
    vtable slot whose original function range the profile attests, retry with that slot
    bound (descriptor.required_parent_slots). A chain is accepted only when the retried
    descriptor stage passes; otherwise the unaugmented result stands. Discovery files are
    never edited: the executed profile and every attempt live in this job."""
    from augment_descriptor_slots import candidate_slot
    def run(candidate,stem):
        save(job/(stem+'.json'),candidate)
        output=job/(stem.replace('profile','execution')+'.json')
        command=[sys.executable,str(cfg['executor']),'--profile',str(job/(stem+'.json')),'--output',str(output)]
        if cfg['descriptor']:command.extend(['--descriptor-module',str(cfg['descriptor'])])
        with (job/(stem+'.stdout.log')).open('w')as stdout,(job/(stem+'.stderr.log')).open('w')as stderr:
            completed=subprocess.run(command,stdout=stdout,stderr=stderr,timeout=cfg['timeout'])
        if completed.returncode:raise RuntimeError('executor exited '+str(completed.returncode))
        result=json.loads(output.read_text())
        if result.get('name')!=profile['name']:raise ValueError('executor result identity differs')
        return result
    original=run(profile,'discovery-profile');attempts=[]
    current,result=profile,original
    progress=lambda value:sum(stage=='pass' for stage in value.get('stages',{}).values())
    for number in range(1,SLOT_ATTEMPTS+1):
        if result['status']=='offline_pass':break
        slot=candidate_slot(result,current)
        required=(current.get('descriptor')or{}).get('required_parent_slots',[])
        if slot is None or slot in required:break
        trap=(result.get('error')or{}).get('unknown')or{}
        current=json.loads(json.dumps(current))
        current.setdefault('descriptor',{})['required_parent_slots']=list(required)+[slot]
        result=run(current,'profile.slot'+str(number))
        attempts.append(dict(slot=slot,original_caller_return_va=trap.get('return_va'),status=result['status'],
            descriptor_stage=result.get('stages',{}).get('descriptor'),next_error=(result.get('error')or{}).get('message')))
    # Accepted when the bound original slot lets the original code get further: the
    # descriptor now passes where it had trapped, or more stages pass overall.
    accepted=bool(attempts) and (result['status']=='offline_pass' or progress(result)>progress(original)
        or (original.get('error')or{}).get('phase')=='original_descriptor' and result.get('stages',{}).get('descriptor')=='pass')
    if not accepted:current,result=profile,original
    augmentation=dict(attempted=bool(attempts),accepted=accepted,
        slots=(current.get('descriptor')or{}).get('required_parent_slots',[])if accepted else [],attempts=attempts)
    return result,current,augmentation

def run_one(profile,cfg):
    began=time.monotonic();key=profile_key(profile,cfg['runtime'])
    job=cfg['output']/'jobs'/key;job.mkdir(parents=True,exist_ok=True)
    summary=dict(name=profile.get('name'),cache_key=key,cache_hit=False,status='blocked',
                 hardware_executed=False,execution=str(job/'execution.json'))
    cache=cfg['cache']/key;cache.parent.mkdir(parents=True,exist_ok=True)
    lock=cfg['cache']/(key+'.lock')
    try:
        validate_inputs(profile)
        if not verify_runtime(cfg['runtime']):raise RuntimeError('runtime changed before execution')
        with lock.open('a+')as handle:
            fcntl.flock(handle,fcntl.LOCK_EX)
            if (cache/'receipt.json').exists():
                receipt=json.loads((cache/'receipt.json').read_text())
                cached=cache/'execution.json'
                if (receipt.get('cache_key')==key and cached.is_file()
                        and file_hash(cached)==receipt.get('execution_sha256')):
                    result=json.loads(cached.read_text())
                    executed=json.loads((cache/'profile.json').read_text())if (cache/'profile.json').is_file()else profile
                    if validate_resources(result)and digest(canonical(executed))==receipt.get('executed_profile_sha256',digest(canonical(profile))):
                        save(job/'discovery-profile.json',profile)
                        save(job/'profile.json',executed);save(job/'execution.json',result)
                        summary['cache_hit']=True
                        if receipt.get('slot_augmentation',{}).get('attempted'):summary['slot_augmentation']=receipt['slot_augmentation']
            if not summary['cache_hit']:
                result,executed,augmentation=execute_with_slots(profile,job,cfg)
                save(job/'profile.json',executed);save(job/'execution.json',result)
                if not verify_runtime(cfg['runtime']):raise RuntimeError('runtime changed during execution')
                if not validate_resources(result):raise RuntimeError('descriptor resource identity changed')
                cache.mkdir(exist_ok=True);save(cache/'execution.json',result);save(cache/'profile.json',executed)
                receipt=dict(cache_key=key,profile_sha256=digest(canonical(profile)),
                    executed_profile_sha256=digest(canonical(executed)),
                    execution_sha256=file_hash(cache/'execution.json'),status=result['status'])
                if augmentation['attempted']:
                    receipt['slot_augmentation']=augmentation;summary['slot_augmentation']=augmentation
                save(cache/'receipt.json',receipt)
            summary.update(status=result['status'],stages=result.get('stages',{}),
                controls=len(result.get('controls',[])),producer_seconds=result.get('seconds'),
                error=result.get('error'))
            if result['status']!='offline_pass':summary['failure_bucket']=bucket(result)
    except subprocess.TimeoutExpired:
        summary.update(status='timeout',error=dict(type='TimeoutExpired',phase='executor',
                       message='child exceeded '+str(cfg['timeout'])+' seconds'))
        summary['failure_bucket']=bucket(summary)
    except Exception as exc:
        summary['error']=dict(type=type(exc).__name__,phase='queue_admission',message=str(exc))
        summary['failure_bucket']=bucket(summary)
    summary['seconds']=time.monotonic()-began
    save(job/'summary.json',summary)
    return summary

STEREO_REQUEST=json.loads((BATCH/'carrier/stereo-channel-request.json').read_text())['plugins']

def apply_overrides(profiles):
    # Opt-in stereo channel request (carrier/stereo-channel-request.json); executed profile records it.
    return [dict(p,stereo_channel_request=True)if p.get('name')in STEREO_REQUEST else p for p in profiles]

def load_profiles(args):
    if args.profiles:
        doc=json.loads(args.profiles.read_text())
        return apply_overrides(doc.get('profiles',[doc])),True,[]
    if not args.discovery_index.exists():return [],False,[]
    index=json.loads(args.discovery_index.read_text());profiles=[];upstream=[]
    for item in index.get('entries',[]):
        if item.get('status')!='discovered':
            upstream.append({key:item.get(key)for key in('name','status','stage','reason','family')});continue
        path=Path(item['profile_path'])
        if not path.is_absolute():path=args.discovery_index.parent/path
        doc=json.loads(path.read_text());profiles.extend(doc.get('profiles',[doc]))
    complete=bool(index.get('complete')or index.get('finished')or index.get('status')in('complete','finished'))
    return apply_overrides(profiles),complete,upstream

def summarize(ledger):
    results=ledger['results'];ledger['counts']={}
    ledger['stage_counts']={};groups={}
    for row in results:
        status=row['status'];ledger['counts'][status]=ledger['counts'].get(status,0)+1
        for stage,value in row.get('stages',{}).items():
            counts=ledger['stage_counts'].setdefault(stage,{})
            counts[value]=counts.get(value,0)+1
        if row.get('failure_bucket'):
            b=row['failure_bucket'];key=digest(canonical(b))
            group=groups.setdefault(key,dict(**b,count=0,names=[]))
            group['count']+=1;group['names'].append(row['name'])
    ledger['failure_buckets']=sorted(groups.values(),key=lambda x:(-x['count'],x['stage'],x['subject']))
    ledger['cache_hits']=sum(row['cache_hit']for row in results)
    ledger['control_requests']=sum(row.get('controls',0)for row in results)
    ledger['executed_control_requests']=sum(row.get('controls',0)for row in results if not row['cache_hit'])
    ledger['reused_control_requests']=sum(row.get('controls',0)for row in results if row['cache_hit'])
    ledger['executed_profiles']=sum(not row['cache_hit']for row in results)

def main():
    parser=argparse.ArgumentParser()
    parser.add_argument('--profiles',type=Path)
    parser.add_argument('--discovery-index',type=Path,default=WORK.parent/'discovery/index.json')
    parser.add_argument('--workers',type=int,default=8)
    parser.add_argument('--timeout',type=int,default=120)
    parser.add_argument('--watch-seconds',type=int,default=0)
    parser.add_argument('--poll-seconds',type=float,default=2)
    parser.add_argument('--cache',type=Path,default=WORK/'cache')
    args=parser.parse_args()
    if not 1<=args.workers<=64 or not 1<=args.timeout<=3600 or args.poll_seconds<.1:
        parser.error('bounded workers1..64, timeout1..3600 and positive polling required')
    executor=BATCH/'executor/run_profile.py';descriptor=BATCH/'carrier/descriptor.py'
    runtime=runtime_manifest(executor,descriptor)
    stamp=datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ')
    output=WORK/'runs'/stamp;output.mkdir(parents=True)
    cfg=dict(output=output,cache=args.cache,executor=executor,descriptor=descriptor,
             timeout=args.timeout,runtime=runtime)
    ledger=dict(schema='openapollo-catalog-execution/1',started=stamp,workers=args.workers,
        timeout_seconds=args.timeout,runtime_digest=digest(canonical(runtime)),results=[],
        hardware_executed=False,per_plugin_overrides=0)
    save(output/'runtime-manifest.json',runtime)
    began=time.monotonic();seen=set();pending={};complete=False
    with ThreadPoolExecutor(max_workers=args.workers)as pool:
        while True:
            profiles,discovery_complete,upstream=load_profiles(args)
            ledger['discovery_not_executable']=upstream
            for profile in profiles:
                key=profile_key(profile,runtime)
                if key not in seen:
                    seen.add(key);pending[pool.submit(run_one,profile,cfg)]=key
            ledger['queued_profiles']=len(seen)
            if pending:
                finished,_=wait(pending,timeout=args.poll_seconds,return_when=FIRST_COMPLETED)
                for future in finished:
                    key=pending.pop(future)
                    try:row=future.result()
                    except Exception as exc:
                        row=dict(name='queue_job_'+key[:12],cache_key=key,cache_hit=False,status='runner_failure',
                            error=dict(type=type(exc).__name__,message=str(exc)))
                        row['failure_bucket']=bucket(row)
                    ledger['results'].append(row)
                    print(json.dumps({k:row.get(k)for k in('name','status','cache_hit','controls','failure_bucket')}),flush=True)
            else:time.sleep(min(args.poll_seconds,.2))
            summarize(ledger);ledger['elapsed_seconds']=time.monotonic()-began
            ledger['pending']=len(pending);save(output/'ledger.json',ledger)
            watch_ended=not args.watch_seconds or time.monotonic()-began>=args.watch_seconds
            if not pending and(discovery_complete or watch_ended):
                complete=discovery_complete;break
    ledger['discovery_complete']=complete
    ledger['stable_runtime']=verify_runtime(runtime)
    ledger['status']='complete'if complete else'current_queue_drained'
    save(output/'ledger.json',ledger)
    save(WORK/'latest.json',dict(run=str(output),ledger=str(output/'ledger.json')))
    print(json.dumps(dict(ledger=str(output/'ledger.json'),counts=ledger['counts'],
        cache_hits=ledger['cache_hits'],seconds=ledger['elapsed_seconds'],status=ledger['status'])),flush=True)

if __name__=='__main__':main()

