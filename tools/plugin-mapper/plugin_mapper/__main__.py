"""Build native plug-in runtime bundles ("maps") from the user's own UAD install.

  python3 -m plugin_mapper --from ROOT [--out DIR] [--all | --only NAME ...] [--authorized auto|all|FILE]
                           [--workers N] [--markers] [--force] [--retry-failures]
  python3 -m plugin_mapper stage --from ROOT --to DIR [--all]
  python3 -m plugin_mapper tables [--out DIR] [--only NAME ...] [--force] [--retry-failures]

ROOT is the Windows install root (a mounted Windows partition or a Wine drive_c), or a copy
`stage` made of the files the mapper reads from it (the plug-in DLLs, UAD2SDK.dll and the few data
files some plug-ins load), so maps can still be built after setup closes the partition.

Each selected Powered plug-in DLL is run offline under Unicorn (no Wine, no device, no network):
discovery finds its parameter block and descriptor, execution sweeps every control in a mono and
a stereo host, and `<slug>-<layout>-runtime-bundle.json` is written to --out for each layout that
passes. Plug-ins are mapped one per worker, and index.json is rewritten after each one, so a
daemon watching --out can offer each plug-in as soon as its maps land. A map is kept until the
plug-in's DLL, UAD2SDK.dll, the mapper or its known-good entry changes: a rerun after a reinstall
or a UAD update only maps what changed.

Nothing leaves this computer. Maps are built from UA's code on this machine and must never be
shared or committed.

Inputs besides ROOT: the panel specs setup installs (/usr/share/open-apollo/plugin-specs: parameter
names, steps and defaults) and the module store `oa-assets dsp` writes (/lib/firmware/universal-audio/plugins).
"""
import argparse, concurrent.futures, fcntl, hashlib, json, os, re, shutil, subprocess, sys, threading, time
from pathlib import Path

PKG = Path(__file__).resolve().parent
sys.path[:0] = [str(PKG), str(PKG / 'carrier')]
from build_bundle import build  # noqa: E402

REPO = PKG.parents[2]
# Bumped whenever a change here, in discovery, execution or the carrier changes what a map holds.
MAPPER_VERSION = 'open-apollo-plugin-mapper/2.1'
# Outside ~/.local/share/open-apollo on purpose: `uninstall.sh --purge` keeps the maps
# (`--purge-plugin-maps` removes them), so a reinstall only maps what changed.
DEFAULT_OUT = Path(os.environ.get('XDG_DATA_HOME') or Path.home() / '.local/share') / 'open-apollo-plugin-maps'
DEFAULT_WORK = Path(os.environ.get('XDG_CACHE_HOME') or Path.home() / '.cache') / 'open-apollo/plugin-mapper'
DEFAULT_SPECS = next((p for p in (Path('/usr/share/open-apollo/plugin-specs'),
                                  Path.home() / '.local/share/open-apollo/plugin-specs') if p.is_dir()),
                     Path('/usr/share/open-apollo/plugin-specs'))  # where scripts/install-panels.sh puts them
DEFAULT_STORE = Path('/lib/firmware/universal-audio/plugins')
DEFAULT_TEXTS = Path(os.environ.get('OA_PLUGIN_ART') or Path.home() / '.local/share/open-apollo/generated') / 'Text'
KNOWN_GOOD = REPO / 'mixer-engine/device_maps/native_known_good.json'
GENERATORS = ('dspaudioprocessor', 'vst2', 'vst2_bx', 'vst2_relab', 'vstmain', 'paramobj', 'sonnox')
WHOLE_SET = ('paramobj', 'vst2_relab')  # these generators take no --plugin and build their pinned set
SLOT = '/devices/0/inputs/0/effects/0'
DLL_DIR = ('Program Files', 'Universal Audio', 'Powered Plugins', 'UAD-2 Powered Plugins')
SDK = ('Windows', 'System32', 'UAD2SDK.dll')
# Data files a plug-in's own code opens (relative to ROOT); staged with the DLLs.
DATA_FILES = (('Program Files (x86)', 'Universal Audio', 'Powered Plugins', 'Data', 'Ocean Way Studios',
               'OceanWayStudios.irz'),)
INDEX = 'index.json'
INDEX_SCHEMA = 'open-apollo-plugin-bundles/2'


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
    h = hashlib.sha256()
    with open(path, 'rb') as f:
        for block in iter(lambda: f.read(1 << 20), b''):
            h.update(block)
    return h.hexdigest()


def slug(name):
    return re.sub(r'[^a-z0-9]+', '-', name.lower().removeprefix('uad ')).strip('-')


def save(path, value):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(f'{path.name}.tmp-{os.getpid()}-{threading.get_ident()}')
    tmp.write_text(json.dumps(value, indent=1) + '\n')
    tmp.replace(path)


def default_workers():
    """One worker per CPU core but one, so the desktop stays usable (OA_PLUGIN_MAP_WORKERS overrides)."""
    env = os.environ.get('OA_PLUGIN_MAP_WORKERS', '')
    if env.isdigit() and int(env) > 0:
        return min(int(env), 64)
    return max(1, min(64, (os.cpu_count() or 2) - 1))


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


# ── Cache ──────────────────────────────────────────────────────────────

def cache_key(dll_sha256, sdk_sha256, known_entry):
    """What a plug-in's maps depend on besides the mapper's own code: its DLL, UAD2SDK.dll and the
    known-good facts built into the bundle (family, latency, DSP-owned word positions)."""
    facts = {k: known_entry.get(k) for k in ('family', 'latency_samples', 'dynamic_refs')}
    digest = hashlib.sha256(json.dumps(facts, sort_keys=True).encode()).hexdigest()[:16]
    return dict(dll_sha256=dll_sha256, uad_sdk_sha256=sdk_sha256, mapper=MAPPER_VERSION, known_good=digest)


def load_index(out):
    try:
        index = json.loads((Path(out) / INDEX).read_text())
    except (OSError, ValueError):
        return dict(schema=INDEX_SCHEMA, bundles=[], failures={}, mono_only=[], waiting_for_licence=[])
    index.setdefault('failures', {})
    index.setdefault('mono_only', [])
    index.setdefault('waiting_for_licence', [])
    if index.get('schema') != INDEX_SCHEMA:  # v1 maps carry no cache key: they are mapped again
        index['bundles'] = []
        index['failures'] = {}
    return index


def cached(index, out, name, key, retry_failures=False):
    """'built' when this plug-in's maps for `key` are all there, 'failed' when it failed with this
    same key (not retried unless asked), else None (map it)."""
    mine = [b for b in index['bundles'] if b['name'] == name]
    if mine and all(b.get('key') == key and (Path(out) / b['file']).is_file() for b in mine):
        return 'built'
    failure = index['failures'].get(name)
    if not retry_failures and isinstance(failure, dict) and failure.get('key') == key:
        return 'failed'
    return None


def merge(index, result):
    """index with one plug-in's outcome in place of its earlier one."""
    name = result['name']
    index['bundles'] = [b for b in index['bundles'] if b['name'] != name] + result['built']
    index['failures'].pop(name, None)
    for k in [k for k in index['failures'] if k.startswith(name + ' (')]:
        del index['failures'][k]
    if result['failures']:
        index['failures'][name] = dict(key=result['key'], reasons=result['failures'])
    index['mono_only'] = sorted((set(index['mono_only']) - {name}) | ({name} if result['mono_only'] else set()))
    index['waiting_for_licence'] = sorted(set(index['waiting_for_licence']) - {name})
    return index


# ── Progress ───────────────────────────────────────────────────────────

class Progress:
    """Per-plug-in lines, an overall count and an ETA, on stdout (setup and the install log read
    it) and in OUT/progress.json (the daemon publishes it). With `markers`, also
    `::progress plugins DONE TOTAL ETA_SECONDS TEXT` lines for the setup wizard."""

    def __init__(self, out, total, workers, markers=False, stream=None, clock=time.monotonic):
        self.out, self.total, self.workers, self.markers = Path(out), total, workers, markers
        self.stream, self.clock = stream or sys.stdout, clock
        self.done = self.mapped = self.failed = self.cached = 0
        self.running, self.durations = {}, []
        self.started = clock()
        self.lock = threading.Lock()

    def eta(self):
        remaining = self.total - self.done
        if not remaining:
            return 0
        if not self.durations:
            return None
        per = sum(self.durations) / len(self.durations)
        now = self.clock()
        # The running ones are partly done; the rest go through `workers` at a time.
        left = sum(max(per - (now - t), per * 0.1) for t in self.running.values())
        queued = max(0, remaining - len(self.running))
        return int((left + queued * per) / max(1, min(self.workers, remaining)))

    @staticmethod
    def human(seconds):
        if seconds is None:
            return 'estimating time left'
        if seconds < 90:
            return f'~{max(seconds, 1)} s left'
        return f'~{round(seconds / 60)} min left'

    def emit(self, text, current=''):
        print(text, file=self.stream, flush=True)
        if self.markers:
            eta = self.eta()
            print(f'::progress plugins {self.done} {self.total} {-1 if eta is None else eta} {current or text}',
                  file=self.stream, flush=True)
        self.write()

    def write(self, state='running'):
        save(self.out / 'progress.json', dict(
            state=state, done=self.done, total=self.total, mapped=self.mapped, cached=self.cached,
            failed=self.failed, running=sorted(self.running), eta_seconds=self.eta(),
            updated=time.strftime('%Y-%m-%dT%H:%M:%S')))

    def start(self, name):
        with self.lock:
            self.running[name] = self.clock()
            self.emit(f'Mapping {self.done + len(self.running)}/{self.total}: {name} (mono, stereo)…', name)

    def finish(self, name, outcome, detail=''):
        with self.lock:
            started = self.running.pop(name, None)
            seconds = self.clock() - started if started is not None else 0.0
            self.done += 1
            if outcome in ('cached', 'kept_failure'):
                self.cached += 1
            elif outcome == 'mapped':
                self.mapped += 1
                self.durations.append(seconds)
            else:
                self.failed += 1
                if seconds:
                    self.durations.append(seconds)
            what = {'cached': 'already mapped', 'mapped': f'done {seconds:.1f} s',
                    'kept_failure': 'not mapped last time with these same files (--retry-failures maps it again)',
                    'failed': f'not mapped ({detail})'}[outcome]
            if outcome in ('cached', 'kept_failure'):
                text = f'{self.done}/{self.total}: {name}: {what}'
            else:
                text = f'{self.done}/{self.total}: {name}: {what} · {self.human(self.eta())}'
            self.emit(text, name)

    def heartbeat(self):
        with self.lock:
            if self.running:
                now = self.clock()
                names = ', '.join(f'{n} ({now - t:.0f} s)' for n, t in sorted(self.running.items()))
                self.emit(f'Still mapping {names} · {self.done}/{self.total} done · {self.human(self.eta())}')


# ── One plug-in ────────────────────────────────────────────────────────

class Mapper:
    """Maps one plug-in at a time per call, each in its own work directory (the discovery and
    execution runners keep their state under $OA_MAPPER_WORK)."""

    def __init__(self, args, env, known, inventory, sdk_sha256):
        self.args, self.env, self.known, self.inventory, self.sdk_sha256 = args, env, known, inventory, sdk_sha256

    def map(self, name, key):
        args, work = self.args, self.args.work / 'jobs' / slug(name)
        shutil.rmtree(work, ignore_errors=True)
        env = dict(self.env, OA_MAPPER_WORK=str(work))
        item = self.inventory[name]
        family = self.known.get(name, {}).get('family', 'generic')
        # Whole-set generators check every member of their set against the inventory.
        members = [i for i in self.inventory.values() if i['family'] == family] if family in WHOLE_SET else [item]
        save(work / 'families/inventory.json', dict(schema='openapollo-local-inventory/1',
                                                    plugins=[i['record'] for i in members]))
        result = dict(name=name, key=key, built=[], failures={}, mono_only=False)
        if family == 'generic':
            save(work / 'cohort.json', dict(cohort=[item['cohort']]))
            run([sys.executable, str(PKG / 'discovery/run_catalog.py'), '--cohort', str(work / 'cohort.json'),
                 '--workers', '1', '--timeout', str(min(args.timeout, 600))], env, log=work / 'discovery.log')
            try:
                entries = json.loads((work / 'discovery/index.json').read_text())['entries']
            except (OSError, ValueError, KeyError):
                entries = []
            profiles = []
            for entry in entries:
                if entry['status'] == 'discovered':
                    profiles += json.loads(Path(entry['profile_path']).read_text())['profiles']
                else:
                    result['failures']['discovery'] = f"{entry['stage']}: {entry['reason']}"
            if not entries:
                result['failures']['discovery'] = f'no result (see {work}/discovery.log)'
        else:
            output = work / 'profile.json'
            command = [sys.executable, str(PKG / 'discovery' / f'{family}_profiles.py'), '--output', str(output)]
            if family not in WHOLE_SET:
                command += ['--plugin', name]
            if run(command, env, cwd=PKG / 'discovery', log=work / 'profile.log') or not output.is_file():
                result['failures']['discovery'] = f'{family} profile failed (see {work}/profile.log)'
                profiles = []
            else:
                profiles = [p for p in json.loads(output.read_text())['profiles'] if p['name'] == name]
        if not profiles:
            result['failures'].setdefault('discovery', 'no profile')
            return result
        # Execution: every control, offline, once per host layout. The plug-in's own code asks the
        # host for its channel count (parent 0x518/0x520) and builds a different state for a stereo
        # insert, so mono and stereo are separate maps.
        profiles = [dict(p, stereo_channel_request=stereo) for p in profiles for stereo in (False, True)]
        save(work / 'profiles.json', dict(profiles=profiles))
        run([sys.executable, str(PKG / 'execution/run_catalog.py'), '--profiles', str(work / 'profiles.json'),
             '--workers', '1', '--timeout', str(args.timeout)], env, log=work / 'execution.log')
        try:
            latest = json.loads((work / 'execution/latest.json').read_text())
            rows = json.loads(Path(latest['ledger']).read_text())['results']
        except (OSError, ValueError, KeyError):
            result['failures']['execution'] = f'no result (see {work}/execution.log)'
            return result
        entry = self.known.get(name, {})
        for row in rows:
            job = Path(row['execution']).parent
            profile = json.loads((job / 'profile.json').read_text())
            layout = 'stereo' if profile.get('stereo_channel_request') else 'mono'
            if row['status'] != 'offline_pass':
                result['failures'][layout] = f"execution {row['status']}: {(row.get('failure_bucket') or {}).get('subject', '')}"
                continue
            execution = json.loads((job / 'execution.json').read_text())
            if layout == 'stereo' and tuple(execution['descriptor'].get('io', ())) != (2, 2):
                result['mono_only'] = True  # the original descriptor is not 2-in/2-out: no stereo insert
                continue
            metadata = json.loads(Path(profile['captured_metadata']['path']).read_text())
            try:
                bundle = build(execution, profile, metadata, entry.get('latency_samples'), layout)
            except ValueError as error:
                result['failures'][layout] = str(error)
                continue
            if entry.get('dynamic_refs'):
                bundle['dynamic_observation_refs'] = entry['dynamic_refs']
            file = f'{slug(name)}-{layout}-runtime-bundle.json'
            save(args.out / file, bundle)
            result['built'].append(dict(name=name, layout=layout, file=file, known_good=layout in entry.get('layouts', ()),
                                        latency_source=bundle['latency_source'], key=key))
        shutil.rmtree(work, ignore_errors=True)  # keep the logs of failures only
        return result


# ── Selection ──────────────────────────────────────────────────────────

def authorized_set(choice):
    """The plug-in names the Apollo authorizes (or demos), or None for "map them all"."""
    if choice == 'all':
        return None
    if choice != 'auto':
        return {line.strip() for line in Path(choice).read_text().splitlines() if line.strip()}
    sys.path.insert(0, str(REPO / 'mixer-engine'))
    try:
        import plugin_authorization
    except Exception as error:  # the mapper also runs from a bare checkout
        print(f'Licence check unavailable ({type(error).__name__}): mapping every selected plug-in', flush=True)
        return None
    answer = plugin_authorization.authorized_plugins(wait=90)  # a daemon just (re)started asks the device first
    if not answer['source']:
        print('The Apollo\'s licence answer is not available (no daemon or device answer): '
              'mapping every selected plug-in', flush=True)
        return None
    names = plugin_authorization.authorized_names(answer)
    print(f"The Apollo authorizes {len(names)} plug-ins (answer from the {answer['source']})", flush=True)
    return names


def select(args, installed, specs, known):
    if args.only:
        return list(args.only)
    if args.all:
        return sorted(installed & set(specs))
    return sorted(installed & set(known))


# ── Stage ──────────────────────────────────────────────────────────────

def stage(argv):
    """Copy what mapping reads from the install into DIR, laid out like the install, so maps can be
    built later (in the background, after a UAD update, when a licence appears) without it."""
    ap = argparse.ArgumentParser(prog='python3 -m plugin_mapper stage')
    ap.add_argument('--from', dest='root', type=Path, required=True)
    ap.add_argument('--to', type=Path, required=True)
    ap.add_argument('--all', action='store_true', help='every DLL, not only the known-good list')
    ap.add_argument('--known-good', type=Path, default=KNOWN_GOOD)
    args = ap.parse_args(argv)
    dlls, sdk = ci_path(args.root, *DLL_DIR), ci_path(args.root, *SDK)
    if dlls is None or sdk is None:
        ap.error('no UAD-2 Powered Plugins folder or UAD2SDK.dll under ' + str(args.root))
    known = {e['name'] for e in json.loads(args.known_good.read_text())['plugins']}
    files = [(sdk, Path(*SDK))] + [(p, Path(*DLL_DIR) / p.name) for p in sorted(dlls.glob('*.dll'))
                                   if args.all or p.stem in known]
    files += [(src, Path(*parts)) for parts in DATA_FILES if (src := ci_path(args.root, *parts))]
    copied = kept = 0
    total = sum(src.stat().st_size for src, _ in files)
    print(f'Copying {len(files)} files ({total / 1e6:.0f} MB) the mapper reads into {args.to}', flush=True)
    for n, (src, rel) in enumerate(files, 1):
        dst = args.to / rel
        st = src.stat()
        if dst.is_file() and dst.stat().st_size == st.st_size and int(dst.stat().st_mtime) == int(st.st_mtime):
            kept += 1
            continue
        dst.parent.mkdir(parents=True, exist_ok=True)
        tmp = dst.with_name(dst.name + '.part')
        shutil.copyfile(src, tmp)
        os.utime(tmp, (st.st_atime, st.st_mtime))
        tmp.replace(dst)
        copied += 1
        if n % 20 == 0:
            print(f'  {n}/{len(files)} files', flush=True)
    for stale in (args.to / Path(*DLL_DIR)).glob('*.dll'):
        if not (dlls / stale.name).exists():  # uninstalled since: its maps stay until rebuilt
            stale.unlink()
    print(f'Staged {copied} new or changed files, {kept} unchanged', flush=True)
    return 0


# ── Tables ─────────────────────────────────────────────────────────────

def _learn_tables(path, changes):
    import tables
    return tables.learn_bundle(path, changes=changes, extra=dict(mapper=MAPPER_VERSION),
                               log=lambda text: print(f'  {Path(path).name}: {text}', flush=True))


def tables_command(argv):
    """Emulator-free control tables (`<bundle>.tables`, tables.py) for mapped plug-ins: learned from
    the emulator, written only when the differential gate passes; a refusal's reason goes to
    index.json `tables`. The daemon then controls that plug-in without an emulator."""
    ap = argparse.ArgumentParser(prog='python3 -m plugin_mapper tables', description=tables_command.__doc__)
    ap.add_argument('--out', type=Path, default=DEFAULT_OUT)
    ap.add_argument('--only', action='append', metavar='NAME', help='just this plug-in (repeatable; default every bundle)')
    ap.add_argument('--changes', type=int, default=3000, help='random changes the gate compares (default 3000)')
    ap.add_argument('--workers', type=int, default=default_workers())
    ap.add_argument('--force', action='store_true', help='learn again even when the tables are current')
    ap.add_argument('--retry-failures', action='store_true', help='learn again bundles refused with the same bundle')
    args = ap.parse_args(argv)
    lock = open(args.out / '.build.lock', 'a')
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError:
        print(f'A plug-in map build is running for {args.out}; try again when it is done.', flush=True)
        return 1
    index = load_index(args.out)
    if not index.get('uad_sdk'):
        ap.error(f'no plug-in maps in {args.out} (build them first)')
    os.environ.setdefault('OA_UAD_SDK', index['uad_sdk'])  # read by the emulator at import
    sys.path.insert(0, str(PKG / 'executor'))
    from table_capture import load
    done = index.setdefault('tables', {})
    todo = []
    for bundle in index['bundles']:
        if args.only and bundle['name'] not in args.only:
            continue
        path = args.out / bundle['file']
        digest = sha(path)
        old = done.get(bundle['file'], {})
        if not args.force and (load(path, digest) is not None
                               or (old.get('status') == 'refused' and old.get('bundle_sha256') == digest
                                   and not args.retry_failures)):
            print(f"{bundle['file']}: {old.get('status', 'pass')} (unchanged)", flush=True)
            continue
        todo.append(path)
    print(f'Learning tables for {len(todo)} bundles ({args.workers} at a time)…', flush=True)
    import multiprocessing  # fork: the workers run this module's _learn_tables (no threads run here)
    with concurrent.futures.ProcessPoolExecutor(max(1, min(args.workers, len(todo) or 1)),
                                                mp_context=multiprocessing.get_context('fork')) as pool:
        futures = {pool.submit(_learn_tables, path, args.changes): path for path in todo}
        for future in concurrent.futures.as_completed(futures):
            path = futures[future]
            try:
                record = future.result()
            except Exception as error:  # a worker that died: one bundle must not stop the others
                record = dict(bundle_sha256=sha(path), status='refused', reason=f'{type(error).__name__}: {error}')
            index = load_index(args.out)
            index.setdefault('tables', {})[path.name] = record
            save(args.out / INDEX, index)
            print(f"{path.name}: {record['status']}" + (f": {record['reason']}" if record['status'] != 'pass' else
                  f" ({record['bytes'] / 1e6:.1f} MB, gate {record['changes']} changes, {record['seconds']} s)"), flush=True)
    return 0


# ── Main ───────────────────────────────────────────────────────────────

def main(argv=None):
    argv = sys.argv[1:] if argv is None else argv
    if argv[:1] == ['stage']:
        return stage(argv[1:])
    if argv[:1] == ['tables']:
        return tables_command(argv[1:])
    ap = argparse.ArgumentParser(prog='python3 -m plugin_mapper', description=__doc__.split('\n\n')[0])
    ap.add_argument('--from', dest='root', type=Path, required=True, help='Windows install root, or a staged copy')
    ap.add_argument('--out', type=Path, default=DEFAULT_OUT)
    ap.add_argument('--work', type=Path, default=DEFAULT_WORK, help='discovery/execution scratch (safe to delete)')
    ap.add_argument('--specs', type=Path, default=DEFAULT_SPECS, help='installed panel specs')
    ap.add_argument('--module-store', type=Path, default=DEFAULT_STORE, help='oa-assets dsp output')
    ap.add_argument('--texts', type=Path, default=DEFAULT_TEXTS, help='texts oa-assets plugins imported (step lists)')
    ap.add_argument('--known-good', type=Path, default=KNOWN_GOOD)
    pick = ap.add_mutually_exclusive_group()
    pick.add_argument('--all', action='store_true', help='every installed plug-in with a panel spec, not only the known-good list')
    pick.add_argument('--only', action='append', metavar='NAME', help='just this plug-in (repeatable)')
    ap.add_argument('--authorized', default='auto', metavar='auto|all|FILE',
                    help="map only what the Apollo authorizes (auto: ask the daemon or the device; "
                         "FILE: one name per line; all: no check). Default auto")
    ap.add_argument('--workers', type=int, default=default_workers(),
                    help='plug-ins mapped at once (default: CPU cores - 1; env OA_PLUGIN_MAP_WORKERS)')
    ap.add_argument('--timeout', type=int, default=900, help='seconds per plug-in stage')
    ap.add_argument('--markers', action='store_true', help='also print ::progress lines for the setup wizard')
    ap.add_argument('--force', action='store_true', help='map again even when a plug-in\'s maps are current')
    ap.add_argument('--retry-failures', action='store_true', help='map again plug-ins that failed with the same inputs')
    args = ap.parse_args(argv)
    if not 1 <= args.workers <= 64:
        ap.error('--workers must be 1..64')
    started = time.monotonic()
    args.out.mkdir(parents=True, exist_ok=True)
    lock = open(args.out / '.build.lock', 'a')
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError:
        print(f'Another plug-in map build is running for {args.out}; leaving it to finish.', flush=True)
        return 0

    dlls, sdk = ci_path(args.root, *DLL_DIR), ci_path(args.root, *SDK)
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
    names = select(args, installed, specs, known)
    allowed = authorized_set(args.authorized) if not args.only else None
    waiting = sorted(n for n in names if allowed is not None and n not in allowed)
    names = [n for n in names if n not in waiting]

    env = dict(os.environ, OA_PLUGIN_DLLS=str(dlls), OA_UAD_SDK=str(sdk), OA_UAD_ROOT=str(args.root),
               OA_MODULE_STORE=str(args.module_store / 'modules'),
               OA_METADATA=str(args.out / 'metadata'), PYTHONDONTWRITEBYTECODE='1')
    sdk_sha256 = sha(sdk)
    index = load_index(args.out)
    index.update(schema=INDEX_SCHEMA, mapper=MAPPER_VERSION, uad_sdk=str(sdk), uad_sdk_sha256=sdk_sha256,
                 plugin_dlls=str(dlls), module_store=str(args.module_store / 'modules'))
    index['waiting_for_licence'] = waiting
    from inventory import Image
    inventory, todo, keys, skipped = {}, [], {}, {}
    for name in names:
        dll = dlls / (name + '.dll')
        if name not in installed:
            skipped[name] = 'not installed'
            continue
        if name not in specs:
            skipped[name] = 'no panel spec'
            continue
        digest = sha(dll)
        key = keys[name] = cache_key(digest, sdk_sha256, known.get(name, {}))
        state = None if args.force else cached(index, args.out, name, key, args.retry_failures)
        stem, spec = specs[name]
        try:
            metadata = metadata_from_spec(spec, args.texts)
        except ValueError as error:
            skipped[name] = str(error)
            continue
        # Next to the maps: the daemon's emulator re-reads it (by hash) when it plans controls.
        meta_path = args.out / 'metadata' / (stem + '.json')
        if not meta_path.is_file() or json.loads(meta_path.read_text()) != metadata:
            save(meta_path, metadata)
        family = known.get(name, {}).get('family', 'generic')
        if state is None or family in WHOLE_SET:
            # Whole-set families need every member's facts even when only one is mapped.
            record = dict(name=name, dll=dll.name, dll_sha256=digest, render_id_manifest=render_ids.get(name),
                          framework='unknown', capture=stem, parameter_count=len(spec['parameters']),
                          discovery=Image(dll).discover())
            inventory[name] = dict(family=family, record=record, cohort=dict(
                name=name, dll_sha256=digest, family=family,
                captured_metadata=dict(path=str(meta_path), sha256=sha(meta_path))))
        todo.append((name, state))
    for name, why in skipped.items():
        index['failures'][name] = dict(key=None, reasons={'setup': why})
    save(args.out / INDEX, index)

    pending = [n for n, state in todo if state is None]
    workers = max(1, min(args.workers, len(pending) or 1))
    progress = Progress(args.out, len(todo), workers, markers=args.markers)
    summary = (f'{len(todo)} plug-ins to check: {len(todo) - len(pending)} already mapped or unchanged, '
               f'{len(pending)} to map with {workers} worker{"s" if workers != 1 else ""}')
    if waiting:
        summary += f'; {len(waiting)} wait for a licence (mapped when the Apollo authorizes them)'
    if skipped:
        summary += f'; {len(skipped)} cannot be mapped here'
    progress.emit(summary)
    for name, state in todo:
        if state is not None:
            progress.finish(name, 'cached' if state == 'built' else 'kept_failure')

    mapper = Mapper(args, env, known, inventory, sdk_sha256)
    write_lock = threading.Lock()
    stop = threading.Event()

    def beat():
        while not stop.wait(15):
            progress.heartbeat()

    def one(name):
        progress.start(name)
        try:
            result = mapper.map(name, keys[name])
        except Exception as error:  # one plug-in must not stop the others
            result = dict(name=name, key=keys[name], built=[], failures={'mapper': f'{type(error).__name__}: {error}'},
                          mono_only=False)
        with write_lock:
            save(args.out / INDEX, merge(load_index(args.out) | {k: index[k] for k in (
                'schema', 'mapper', 'uad_sdk', 'uad_sdk_sha256', 'plugin_dlls', 'module_store', 'waiting_for_licence')},
                result))
        layouts = ', '.join(b['layout'] for b in result['built'])
        if result['built']:
            progress.finish(name, 'mapped')
            if result['failures']:
                print(f'  {name}: {layouts} only ({"; ".join(f"{k}: {v}" for k, v in result["failures"].items())})', flush=True)
        else:
            progress.finish(name, 'failed', '; '.join(f'{k}: {v}' for k, v in result['failures'].items())[:200])
        return result

    heart = threading.Thread(target=beat, daemon=True)
    heart.start()
    try:
        with concurrent.futures.ThreadPoolExecutor(workers, thread_name_prefix='map') as pool:
            list(pool.map(one, pending))
    finally:
        stop.set()
    final = load_index(args.out)
    final['updated'] = time.strftime('%Y-%m-%dT%H:%M:%S')
    save(args.out / INDEX, final)
    built = final['bundles']
    layouts = {lay: sum(b['layout'] == lay for b in built) for lay in ('mono', 'stereo')}
    progress.emit(f"Plug-in maps: {len(built)} ({layouts['mono']} mono, {layouts['stereo']} stereo) in {args.out}; "
                  f"{progress.mapped} mapped now, {progress.cached} unchanged, {progress.failed} not mapped "
                  f'({time.monotonic() - started:.0f} s). Details: {args.out / INDEX}')
    progress.write('done')
    return 0 if built or not todo else 1


if __name__ == '__main__':
    sys.exit(main())
