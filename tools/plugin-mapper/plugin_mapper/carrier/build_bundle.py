"""Runtime bundle from one offline execution: what the daemon's profiled carrier provider loads.

The bundle is built on the user's machine from their own install and never leaves it. It is
self-contained except for the plug-in DLL, UAD2SDK.dll and the module store, which the
provider reads where the user's install and `oa-assets dsp` put them.
"""
import hashlib, json
from pathlib import Path

SCHEMA = 'openapollo-profiled-runtime-bundle/1'


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def build(execution, profile, metadata, latency):
    """execution: run_profile.py result (offline_pass); profile: the executed profile;
    metadata: the parameter metadata document the run used; latency: samples at 48 kHz or None."""
    name = execution['name']
    if execution.get('status') != 'offline_pass':
        raise ValueError(f'{name}: offline pass required')
    if profile.get('name') != name:
        raise ValueError(f'{name}: executed profile identity differs')
    ids = sorted({int(item['path'].split('/parameters/')[1].split('/')[0])
                  for item in metadata['controls'] if '/parameters/' in item['path']})
    if ids != list(range(execution['parameters']['count'])):
        raise ValueError(f'{name}: parameter metadata ordinals do not match the execution')
    descriptor = dict(execution['descriptor'])
    resources, seen = [], {}
    for resource in descriptor.get('resources', ()):
        identity = (resource['id'], resource['architecture'], resource['kind'])
        if identity in seen:
            if seen[identity] != resource:
                raise ValueError(f'{name}: conflicting original resource identity {identity}')
            continue
        seen[identity] = resource
        resources.append(resource)
    descriptor['resources'] = resources
    descriptor['latency_samples'] = latency or 0
    if not 0 <= descriptor['latency_samples'] <= 0x7fffffff:
        raise ValueError(f'{name}: latency outside the descriptor range')
    sweep = []
    for index, maximum in enumerate(execution['parameters']['effective_maxima']):
        samples = [{'integer': c['requested'], 'readback': c['integer_readback'],
                    'formatted': c['formatted'], 'events': c['events']}
                   for c in execution['controls'] if c['index'] == index]
        if not samples:
            raise ValueError(f'{name}: missing control samples for parameter {index}')
        sweep.append({'index': index, 'maximum': maximum, 'samples': samples})
    return {
        'schema': SCHEMA,
        'status': 'pass',
        'scope': 'built offline from the local install; not a hardware qualification claim',
        'name': name,
        'carrier_channels': 2 if tuple(descriptor.get('io', ())) == (2, 2) else 1,
        'latency_source': 'known_good' if latency is not None else 'unknown',
        'dll': {'path': profile['dll']['path'], 'sha256': descriptor['plugin_sha256']},
        'descriptor': descriptor,
        'startup': execution['startup'],
        'parameter_maxima': execution['parameters']['effective_maxima'],
        'control_sweep': sweep,
        'parameter_metadata': metadata,
        'profile': profile,
    }
