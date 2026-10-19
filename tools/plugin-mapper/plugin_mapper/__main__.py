"""Build native plug-in runtime bundles from the user's own UAD install.

  python3 -m plugin_mapper --from ROOT [--out DIR] [--all | --only NAME ...] [--workers N]

ROOT is the Windows install root (a mounted Windows partition or a Wine drive_c). Each
selected Powered plug-in DLL is run offline under Unicorn (no Wine, no device, no network):
discovery finds its parameter block and descriptor, execution sweeps every control, and one
`<slug>-runtime-bundle.json` per passing plug-in is written to --out together with index.json.
Nothing leaves this computer. Bundles are built from UA's code on this machine and must
never be shared or committed.

Inputs it needs besides ROOT: the panel specs setup installs (/usr/share/open-apollo/plugin-specs:
parameter names, steps and defaults) and the module store `oa-assets dsp` writes
(/lib/firmware/universal-audio/plugins).
"""
import argparse, hashlib, json, os, re, subprocess, sys, time
from pathlib import Path

PKG = Path(__file__).resolve().parent
sys.path[:0] = [str(PKG), str(PKG / 'carrier')]
from build_bundle import build  # noqa: E402

REPO = PKG.parents[2]
DEFAULT_OUT = Path.home() / '.local/share/open-apollo/plugin-bundles'
DEFAULT_WORK = Path.home() / '.cache/open-apollo/plugin-mapper'
DEFAULT_SPECS = next((p for p in (Path('/usr/share/open-apollo/plugin-specs'),
                                  Path.home() / '.local/share/open-apollo/plugin-specs') if p.is_dir()),
                     Path('/usr/share/open-apollo/plugin-specs'))  # where scripts/install-panels.sh puts them
DEFAULT_STORE = Path('/lib/firmware/universal-audio/plugins')
DEFAULT_TEXTS = Path(os.environ.get('OA_PLUGIN_ART') or Path.home() / '.local/share/open-apollo/generated') / 'Text'
KNOWN_GOOD = REPO / 'mixer-engine/device_maps/native_known_good.json'
GENERATORS = ('dspaudioprocessor', 'vst2', 'vst2_bx', 'vst2_relab', 'vstmain', 'paramobj', 'sonnox')
WHOLE_SET = ('paramobj', 'vst2_relab')  # these generators take no --plugin and build their pinned set
SLOT = '/devices/0/inputs/0/effects/0'


def ci_path(root, *parts):
    """root/parts matched case-insensitively (Windows names on a case-sensitive filesystem)."""
    path = Path(root)
    for part in parts:
        if (path / part).exists():
            path = path / part
            continue
        match = next((p for p in path.iterdir() if p.name.lower() == part.lower()), None) if path.is_dir() else None
        if match is None:
            return None
        path = match
    return path


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def slug(name):
    return re.sub(r'[^a-z0-9]+', '-', name.lower().removeprefix('uad ')).strip('-')


def save(path, value):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(path.name + '.tmp')
    tmp.write_text(json.dumps(value, indent=1) + '\n')
    tmp.replace(path)


def metadata_from_spec(spec, texts):
    """The parameter metadata the pipeline reads, from an installed panel spec: names, step
    labels and Console defaults per parameter (the same schema a Console capture has).
    Step labels a spec only references (`stepsRef`, e.g. preset names) come from the texts
    `oa-assets plugins` extracted from the user's install into `texts`."""
    controls = []
    for p in spec['parameters']:
        steps = p.get('steps')
        if p.get('stepsRef') and not steps:
            ref = Path(texts) / f"{p['stepsRef']}.json"
            if not ref.is_file():
                raise ValueError(f"parameter {p['index']} step labels {p['stepsRef']} not imported (run oa-assets plugins)")
            steps = json.loads(ref.read_text())
        base = f"{SLOT}/parameters/{p['index']}/"
        controls += [dict(path=base + 'Type', type='string', value='parameter'),
                     dict(path=base + 'Name', type='string', value=p['name']),
                     dict(path=base + 'NormalizedValue', type='float', min=0.0, max=1.0, value=p['default'])]
        if steps:
            controls.append(dict(path=base + 'StepValue', type='string', values=steps, value=None))
    return dict(schema='openapollo-synthetic-metadata/1', source='panel-spec', path=SLOT, controls=controls)


def run(command, env, cwd=None, log=None):
    if log:
        Path(log).parent.mkdir(parents=True, exist_ok=True)
    with open(log, 'w') if log else open(os.devnull, 'w') as out:
        return subprocess.run(command, env=env, cwd=cwd, stdout=out, stderr=subprocess.STDOUT).returncode


def main(argv=None):
    ap = argparse.ArgumentParser(prog='python3 -m plugin_mapper', description=__doc__.split('\n\n')[0])
    ap.add_argument('--from', dest='root', type=Path, required=True, help='Windows install root (partition or drive_c)')
    ap.add_argument('--out', type=Path, default=DEFAULT_OUT)
    ap.add_argument('--work', type=Path, default=DEFAULT_WORK, help='discovery/execution cache (safe to delete)')
    ap.add_argument('--specs', type=Path, default=DEFAULT_SPECS, help='installed panel specs')
    ap.add_argument('--module-store', type=Path, default=DEFAULT_STORE, help='oa-assets dsp output')
    ap.add_argument('--texts', type=Path, default=DEFAULT_TEXTS, help='texts oa-assets plugins imported (step lists)')
    ap.add_argument('--known-good', type=Path, default=KNOWN_GOOD)
    pick = ap.add_mutually_exclusive_group()
    pick.add_argument('--all', action='store_true', help='every installed plug-in with a panel spec, not only the known-good list')
    pick.add_argument('--only', action='append', metavar='NAME', help='just this plug-in (repeatable)')
    ap.add_argument('--workers', type=int, default=max(1, min(8, (os.cpu_count() or 2) - 1)))
    ap.add_argument('--timeout', type=int, default=900, help='seconds per plug-in')
    args = ap.parse_args(argv)
    started = time.monotonic()

    dlls = ci_path(args.root, 'Program Files', 'Universal Audio', 'Powered Plugins', 'UAD-2 Powered Plugins')
    sdk = ci_path(args.root, 'Windows', 'System32', 'UAD2SDK.dll')
    manifest = args.module_store / 'manifest.json'
    for what, path in (('UAD-2 Powered Plugins folder', dlls), ('Windows/System32/UAD2SDK.dll', sdk),
                       ('module store manifest (run oa-assets dsp first)', manifest if manifest.is_file() else None),
                       ('panel specs', args.specs if args.specs.is_dir() else None)):
        if path is None:
            ap.error(f'{what} not found')
    render_ids = {p['name']: p.get('render_id') for p in json.loads(manifest.read_text())['plugins']}
    specs = {}
    for f in args.specs.glob('PluginSpec_*.json'):
        spec = json.loads(f.read_text())
        specs[spec.get('effectName')] = (f.stem.removeprefix('PluginSpec_'), spec)
    known = {e['name']: e for e in json.loads(args.known_good.read_text())['plugins']}
    installed = {p.stem for p in dlls.glob('*.dll')}
    if args.only:
        names = args.only
    elif args.all:
        names = sorted(installed & set(specs))
    else:
        names = sorted(installed & set(known))

    work = args.work
    env = dict(os.environ, OA_PLUGIN_DLLS=str(dlls), OA_UAD_SDK=str(sdk), OA_UAD_ROOT=str(args.root),
               OA_MODULE_STORE=str(args.module_store / 'modules'), OA_MAPPER_WORK=str(work),
               OA_METADATA=str(args.out / 'metadata'), PYTHONDONTWRITEBYTECODE='1')
    failures, inventory, cohort, by_family = {}, [], [], {}
    from inventory import Image
    for name in names:
        dll = dlls / (name + '.dll')
        if name not in installed:
            failures[name] = 'not installed'
            continue
        if name not in specs:
            failures[name] = 'no panel spec'
            continue
        stem, spec = specs[name]
        try:
            metadata = metadata_from_spec(spec, args.texts)
        except ValueError as error:
            failures[name] = str(error)
            continue
        # Next to the bundles: the daemon's emulator re-reads it (by hash) when it plans controls.
        meta_path = args.out / 'metadata' / (stem + '.json')
        save(meta_path, metadata)
        digest = sha(dll)
        inventory.append(dict(name=name, dll=dll.name, dll_sha256=digest, render_id_manifest=render_ids.get(name),
                              framework='unknown', capture=stem, parameter_count=len(spec['parameters']),
                              discovery=Image(dll).discover()))
        family = known.get(name, {}).get('family', 'generic')
        by_family.setdefault(family, []).append(name)
        if family == 'generic':
            cohort.append(dict(name=name, dll_sha256=digest, family=family,
                               captured_metadata=dict(path=str(meta_path), sha256=sha(meta_path))))
    save(work / 'families/inventory.json', dict(schema='openapollo-local-inventory/1', plugins=inventory))
    print(f'{len(inventory)} plug-ins selected, {len(failures)} skipped', flush=True)

    # Discovery: the generic stages (one worker per plug-in), then the hand-profiled families.
    profiles = []
    if cohort:
        save(work / 'cohort.json', dict(cohort=cohort))
        run([sys.executable, str(PKG / 'discovery/run_catalog.py'), '--cohort', str(work / 'cohort.json'),
             '--workers', str(args.workers), '--timeout', str(min(args.timeout, 600))], env, log=work / 'discovery.log')
        index = json.loads((work / 'discovery/index.json').read_text())
        for entry in index['entries']:
            if entry['status'] == 'discovered':
                profiles += json.loads(Path(entry['profile_path']).read_text())['profiles']
            else:
                failures[entry['name']] = f"discovery {entry['stage']}: {entry['reason']}"
    for family in GENERATORS:
        for name in by_family.get(family, ()):
            output = work / 'profiles' / f'{slug(name)}.json'
            command = [sys.executable, str(PKG / 'discovery' / f'{family}_profiles.py'), '--output', str(output)]
            if family not in WHOLE_SET:
                command += ['--plugin', name]
            if run(command, env, cwd=PKG / 'discovery', log=work / 'profiles' / f'{slug(name)}.log') or not output.is_file():
                failures[name] = f'{family} discovery failed (see {work}/profiles/{slug(name)}.log)'
                continue
            profiles += [p for p in json.loads(output.read_text())['profiles'] if p['name'] == name]

    # Execution: every control of every discovered plug-in, offline.
    save(work / 'profiles-all.json', dict(profiles=profiles))
    if profiles:
        run([sys.executable, str(PKG / 'execution/run_catalog.py'), '--profiles', str(work / 'profiles-all.json'),
             '--workers', str(args.workers), '--timeout', str(args.timeout)], env, log=work / 'execution.log')
        latest = json.loads((work / 'execution/latest.json').read_text())
        results = json.loads(Path(latest['ledger']).read_text())['results']
    else:
        results = []

    # Bundles.
    args.out.mkdir(parents=True, exist_ok=True)
    built = []
    for row in results:
        name = row['name']
        if row['status'] != 'offline_pass':
            failures[name] = f"execution {row['status']}: {(row.get('failure_bucket') or {}).get('subject', '')}"
            continue
        job = Path(row['execution']).parent
        execution = json.loads((job / 'execution.json').read_text())
        profile = json.loads((job / 'profile.json').read_text())
        metadata = json.loads(Path(profile['captured_metadata']['path']).read_text())
        entry = known.get(name, {})
        try:
            bundle = build(execution, profile, metadata, entry.get('latency_samples'))
        except ValueError as error:
            failures[name] = str(error)
            continue
        if entry.get('dynamic_refs'):
            bundle['dynamic_observation_refs'] = entry['dynamic_refs']
        save(args.out / f'{slug(name)}-runtime-bundle.json', bundle)
        built.append(dict(name=name, file=f'{slug(name)}-runtime-bundle.json', known_good=name in known,
                          latency_source=bundle['latency_source']))
    save(args.out / 'index.json', dict(schema='open-apollo-plugin-bundles/1', created=time.strftime('%Y-%m-%dT%H:%M:%S'),
                                       uad_sdk=str(sdk), uad_sdk_sha256=sha(sdk), plugin_dlls=str(dlls),
                                       module_store=str(args.module_store / 'modules'),
                                       bundles=built, failures=dict(sorted(failures.items()))))
    print(f'{len(built)} bundles written to {args.out}, {len(failures)} not built '
          f'({time.monotonic() - started:.0f} s); details in {args.out / "index.json"}', flush=True)
    return 0 if built else 1


if __name__ == '__main__':
    sys.exit(main())
