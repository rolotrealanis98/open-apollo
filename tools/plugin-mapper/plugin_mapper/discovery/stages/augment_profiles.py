from pathlib import Path
import json,hashlib
HERE=Path(__file__).resolve().parent
p=HERE/'profiles.json';data=json.loads(p.read_text());programs={x['name']:x for x in json.loads((HERE/'program-discovery.json').read_text())};notifications={x['name']:x for x in json.loads((HERE/'notification-discovery.json').read_text())}
prior=HERE/'profiles-before-program-stage.json'
if not prior.exists():prior.write_bytes(p.read_bytes())
for x in data['profiles']:
 if x['status']!='discovered':continue
 program=programs[x['name']]
 if program['status']!='discovered' or len(program['matches'])!=1:raise ValueError('no unique program loader')
 x['startup']['program_load_status']='original_source_discovered';x['startup']['program_loader']=program['matches'][0]
 x['notifications']=notifications[x['name']]
 x['source_files'].append(program['matches'][0]['source'])
 x['source_hashes']={name:hashlib.sha256((HERE/name).read_bytes()).hexdigest()for name in x['source_files']}
new=HERE/'profiles.next.json';new.write_text(json.dumps(data,indent=2)+'\n');new.replace(p)
print('Program and notification data appended for',len(data['profiles']),'frozen profiles')
