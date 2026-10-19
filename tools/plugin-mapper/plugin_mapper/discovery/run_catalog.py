"""Bounded static catalog discovery (one worker process per plug-in). Never loads or executes vendor code.

  run_catalog.py --cohort COHORT.json [--workers N] [--timeout S] [--retry-failures]

The cohort lists {name, dll_sha256, captured_metadata{path, sha256}, family} per plug-in (plugin_mapper
writes it). DLLs come from $OA_PLUGIN_DLLS; runs, the cache and index.json go under
$OA_MAPPER_WORK/discovery.
"""
from pathlib import Path
import argparse,concurrent.futures,fcntl,hashlib,json,os,platform,signal,subprocess,sys,time,uuid
HERE=Path(__file__).resolve().parent
WORK=Path(os.environ.get('OA_MAPPER_WORK',str(Path.home()/'.cache/open-apollo/plugin-mapper')))/'discovery'
STAGES=['generate_profiles.py','discover_programs.py','discover_notifications.py','augment_profiles.py','discover_host_context.py','seal_host_context.py','discover_crt_locks.py','discover_crt_aggregate.py','discover_parent_binding.py','probe_formatter_abi.py']
def sha(p):return hashlib.sha256(Path(p).read_bytes()).hexdigest()
def atomic(p,value):
 p=Path(p);p.parent.mkdir(parents=True,exist_ok=True);tmp=p.with_name(p.name+'.tmp-'+uuid.uuid4().hex);tmp.write_text(json.dumps(value,indent=2)+'\n');tmp.replace(p)
def snapshot_sources(source):
 raw={name:(source/name).read_text()for name in STAGES+['discover_abi.py','worker.py']}
 # Only relocate the storage root (each attempt's own directory). All discovery rules remain unchanged.
 for name in STAGES:
  raw[name]=raw[name].replace('HERE=Path(__file__).resolve().parent',"HERE=Path(__import__('os').environ['APOLLO_STATIC_WORK_DIR'])")
 key=hashlib.sha256(json.dumps(raw,sort_keys=True).encode()).hexdigest();dest=WORK/'source-snapshots'/key
 if not dest.exists():
  dest.mkdir(parents=True)
  for name,content in raw.items():(dest/name).write_text(content)
  atomic(dest/'manifest.json',{name:sha(dest/name)for name in raw})
 return dest,key

def main():
 ap=argparse.ArgumentParser();ap.add_argument('--cohort',type=Path,required=True);ap.add_argument('--workers',type=int,default=8);ap.add_argument('--timeout',type=float,default=120);ap.add_argument('--retry-failures',action='store_true');args=ap.parse_args()
 if not 1<=args.workers<=64 or not 5<=args.timeout<=3600:raise ValueError('bounded workers/timeout required')
 WORK.mkdir(parents=True,exist_ok=True)
 lock=(WORK/'.runner.lock').open('a');fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB)
 dllroot=Path(os.environ['OA_PLUGIN_DLLS'])
 records=json.loads(args.cohort.read_text())['cohort']
 if len({r['name']for r in records})!=len(records):raise ValueError('duplicate cohort names')
 snap,sourcehash=snapshot_sources(HERE/'stages');started=time.monotonic();run_id=time.strftime('%Y%m%dT%H%M%S')+'-'+uuid.uuid4().hex[:8];run=WORK/'runs'/run_id;run.mkdir(parents=True)
 atomic(run/'cohort.json',dict(schema='openapollo-catalog-cohort/1',cohort=records,source_sha256=sourcehash));entries=[];work=[]
 for ordinal,r in enumerate(records):
  dll=dllroot/(r['name']+'.dll');meta=Path(r['captured_metadata']['path'])if r['captured_metadata']['path']else None;problem=None
  if not dll.is_file():problem='original DLL missing'
  elif sha(dll)!=r['dll_sha256']:problem='original DLL hash differs from inventory'
  elif meta is None or not meta.is_file():problem='parameter metadata missing'
  elif sha(meta)!=r['captured_metadata']['sha256']:problem='parameter metadata hash differs from cohort'
  record={k:v for k,v in r.items() if k!='family'}
  itemkey=hashlib.sha256(json.dumps({'record':record,'sources':sourcehash,'python':platform.python_version()},sort_keys=True).encode()).hexdigest();cache=WORK/'cache'/itemkey;cache.mkdir(parents=True,exist_ok=True)
  e=dict(ordinal=ordinal,name=r['name'],cache_key=itemkey,status='pending',stage='pending',reason=None,family=r.get('family','unknown'),dll_sha256=r['dll_sha256'],metadata_sha256=r['captured_metadata']['sha256'],result_path=None,profile_path=None,cached=False);entries.append(e)
  if problem:e.update(status='failed',stage='inventory',reason=problem);continue
  pointer=cache/'latest.json'
  if pointer.exists():
   old=json.loads(pointer.read_text());rp=Path(old['result_path'])
   if rp.is_file():
    result=json.loads(rp.read_text())
    if result.get('cache_key')==itemkey and(result['status']=='discovered'or not args.retry_failures):
     e.update({k:result.get(k)for k in('status','stage','reason','profile_path')});e.update(result_path=str(rp),cached=True);continue
  attempt=cache/'attempts'/uuid.uuid4().hex;attempt.mkdir(parents=True);atomic(attempt/'cohort.json',dict(cohort=[record]));work.append((e,attempt,cache))
 def publish():
  from collections import Counter
  summary=dict(complete=all(e['status']!='pending'for e in entries),schema='openapollo-catalog-discovery-index/1',run_id=run_id,selected=len(entries),completed=sum(e['status']!='pending'for e in entries),counts=dict(Counter(e['status']for e in entries)),stage_failures=dict(Counter(e['stage']for e in entries if e['status']in('failed','timeout'))),reason_failures=dict(Counter(str(e['reason'])for e in entries if e['status']in('failed','timeout'))),source_sha256=sourcehash,seconds=time.monotonic()-started,entries=entries)
  atomic(WORK/'index.json',summary);atomic(run/'index.json',summary)
 publish()
 def execute(job):
  e,attempt,cache=job;env=dict(os.environ,APOLLO_STATIC_WORK_DIR=str(attempt),APOLLO_STATIC_SOURCE_DIR=str(snap),APOLLO_STATIC_CACHE_KEY=e['cache_key'],PYTHONDONTWRITEBYTECODE='1');tick=time.monotonic()
  with (attempt/'worker.log').open('w')as log:
   proc=subprocess.Popen([sys.executable,str(snap/'worker.py')],env=env,stdout=log,stderr=subprocess.STDOUT,start_new_session=True)
   try:proc.wait(timeout=args.timeout)
   except subprocess.TimeoutExpired:
    os.killpg(proc.pid,signal.SIGKILL);proc.wait();progress=json.loads((attempt/'progress.json').read_text())if(attempt/'progress.json').exists()else{};atomic(attempt/'result.json',dict(status='timeout',stage=progress.get('stage','unknown'),reason='per-plugin timeout',seconds=time.monotonic()-tick,cache_key=e['cache_key'],profile_path=None))
  rp=attempt/'result.json'
  if not rp.exists():atomic(rp,dict(status='failed',stage='worker',reason='worker exited without result '+str(proc.returncode),cache_key=e['cache_key'],profile_path=None))
  result=json.loads(rp.read_text());atomic(cache/'latest.json',dict(result_path=str(rp)));return e,rp,result
 with concurrent.futures.ThreadPoolExecutor(max_workers=args.workers)as pool:
  futures=[pool.submit(execute,job)for job in work]
  for future in concurrent.futures.as_completed(futures):
   e,rp,r=future.result();e.update({k:r.get(k)for k in('status','stage','reason','profile_path')});e['result_path']=str(rp);publish();print(json.dumps(dict(completed=sum(x['status']!='pending'for x in entries),total=len(entries),name=e['name'],status=e['status'],stage=e['stage'],reason=e['reason'])),flush=True)
 publish();print(json.dumps({'run_id':run_id,'counts':json.loads((WORK/'index.json').read_text())['counts'],'seconds':time.monotonic()-started}),flush=True)
if __name__=='__main__':main()
