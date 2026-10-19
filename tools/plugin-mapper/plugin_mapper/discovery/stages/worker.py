"""One bounded process owns one original image's static discovery stages."""
from pathlib import Path
import contextlib,hashlib,json,os,runpy,sys,time,traceback
HERE=Path(os.environ['APOLLO_STATIC_WORK_DIR']);SOURCE=Path(os.environ['APOLLO_STATIC_SOURCE_DIR']);sys.path.insert(0,str(SOURCE))
STAGES=['generate_profiles.py','discover_programs.py','discover_notifications.py','augment_profiles.py','discover_host_context.py','seal_host_context.py','discover_crt_locks.py','discover_crt_aggregate.py','discover_parent_binding.py','probe_formatter_abi.py']
def write(path,x):
 temp=path.with_suffix('.tmp');temp.write_text(json.dumps(x,indent=2)+'\n');temp.replace(path)
started=time.monotonic();stage='rtti';timings=[];record=json.loads((HERE/'cohort.json').read_text())['cohort'][0]
try:
 write(HERE/'progress.json',{'stage':stage});from discover_abi import discover,ROOT
 tick=time.monotonic();rtti=discover(ROOT/(record['name']+'.dll'));write(HERE/'rtti.json',[rtti]);timings.append(dict(stage=stage,seconds=time.monotonic()-tick));family=[x['type']for x in rtti['vtables']if'.?AVCHypParamBlock@@'in x.get('bases',[])]
 for stage in STAGES:
  write(HERE/'progress.json',{'stage':stage});tick=time.monotonic()
  with (HERE/(stage+'.log')).open('w')as log,contextlib.redirect_stdout(log):runpy.run_path(str(SOURCE/stage),run_name='__main__')
  timings.append(dict(stage=stage,seconds=time.monotonic()-tick))
  if stage=='generate_profiles.py':
   p=json.loads((HERE/'profiles.json').read_text())['profiles'][0]
   if p['status']!='discovered':raise ValueError(p.get('error','profile generation failed'))
  if stage=='discover_programs.py':
   p=json.loads((HERE/'program-discovery.json').read_text())[0]
   if p['status']!='discovered':raise ValueError('original program loader '+p['status']+' matches='+str(len(p['matches'])))
  if stage=='probe_formatter_abi.py':
   p=json.loads((HERE/'formatter-abi-proof.json').read_text())[0]
   if p['status']=='unresolved':raise ValueError('formatter third argument unresolved')
 profile=HERE/'profiles.json';result=dict(status='discovered',stage='complete',reason=None,profile_path=str(profile),profile_sha256=hashlib.sha256(profile.read_bytes()).hexdigest(),parameter_classes=family)
except BaseException as exc:
 result=dict(status='failed',stage=stage,reason=type(exc).__name__+': '+str(exc),traceback=traceback.format_exc(),profile_path=None)
result.update(cache_key=os.environ['APOLLO_STATIC_CACHE_KEY'],name=record['name'],seconds=time.monotonic()-started,stages=timings);write(HERE/'result.json',result)
