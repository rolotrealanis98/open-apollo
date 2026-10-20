"""Native plug-in providers from the runtime bundles tools/plugin-mapper built on this machine.

  python3 -m plugin_mapper --from <install>          (maps into ~/.local/share/open-apollo-plugin-maps)
  ua_mixer_daemon.py --native-plugin-bundles ~/.local/share/open-apollo-plugin-maps ...

A plug-in has a mono bundle (offered on every analog input) and, when its original descriptor
is 2-in/2-out, a stereo bundle (offered on the linked pairs 1/2 and 3/4). Only the layouts
device_maps/native_known_good.json lists for a plug-in are advertised unless advertise_all is
set: the others built offline but have not been through a live pass. The Apollo's licence
answer still decides every load. The mapper may still be running when the daemon starts:
BundleWatcher hands the daemon each plug-in as its maps land.
"""
import json
import logging
import os
import threading
from copy import deepcopy
from pathlib import Path

from native_carrier import DSP_INPUTS, MODULES, STEREO_FIRST
from native_profiled_carrier_provider import ProfiledCarrierProvider, StereoProfiledCarrierProvider
from native_chain_composer import compose

log = logging.getLogger(__name__)
# Every input a chain can go on (analog 1-4, Virtual 1-4); the backend drops the Virtual ones
# when the driver cannot replace a Virtual strip.
NATIVE_INPUTS = tuple(DSP_INPUTS)
# Where tools/plugin-mapper puts the maps (kept by `uninstall.sh --purge`).
DEFAULT_DIR = Path(os.environ.get('XDG_DATA_HOME') or Path.home() / '.local/share') / 'open-apollo-plugin-maps'
KNOWN_GOOD = Path(__file__).resolve().parent / 'device_maps/native_known_good.json'


class ProfiledProviders(tuple):
    """The provider tuple the native backend takes; `composer` builds multi-insert chains.
    `growable`: maps keep landing while the mapper runs (BundleWatcher), so it may start empty."""
    composer = staticmethod(compose)
    growable = True


def known_good(path=KNOWN_GOOD):
    return {e['name']: e for e in json.loads(Path(path).read_text())['plugins']}


class _Bundle:
    """One runtime bundle on disk, parsed once (on first use) for every input it is offered on."""

    def __init__(self, path, name, layout):
        self.path, self.name, self.layout = Path(path), name, layout
        self._data = None
        self._lock = threading.Lock()

    def data(self):
        with self._lock:
            if self._data is None:
                data = json.loads(self.path.read_text())
                if data.get('status') != 'pass':
                    raise ValueError(f'{self.path.name}: passing runtime bundle required')
                if (data.get('name'), data.get('layout')) != (self.name, self.layout):
                    raise ValueError(f'{self.path.name}: bundle differs from its index entry')
                if data.get('carrier_channels') != (2 if self.layout == 'stereo' else 1):
                    raise ValueError(f'{self.path.name}: carrier_channels do not match its layout')
                self._data = data
            return self._data


class LazyProvider:
    """A provider the daemon can offer before its bundle is read.

    Offering a plug-in needs only its name, slot, channels and sample rate, which the index
    gives; building a provider parses its bundle (up to tens of MB) and its carrier recipe.
    The daemon offers every map at startup, so that work waits for the first load of each
    plug-in on each input, on the native worker thread. Every other attribute is the built
    provider's; a deepcopy (a load's private copy) is a copy of the built provider."""

    def __init__(self, bundle, kind, input_index, module_store):
        self._bundle, self.kind, self.input_index = bundle, kind, input_index
        self._module_store = module_store
        self.name, self.bundle_path = bundle.name, bundle.path
        self.slot = f'/devices/0/inputs/{input_index}/effects/0'
        self.sample_rate = kind.sample_rate
        if bundle.layout == 'stereo':
            self.channels = (input_index, input_index + 1)
        self._provider = None
        self._lock = threading.Lock()

    def built(self):
        """The provider itself, built on first call."""
        with self._lock:
            if self._provider is None:
                try:
                    self._provider = self.kind(self._bundle.path, self.input_index, data=self._bundle.data(),
                                               module_store=self._module_store)
                except (OSError, ValueError) as error:
                    # Offered from its index entry; the load that found it unusable fails.
                    log.warning('native bundle %s unusable: %s', self._bundle.path.name, error)
                    raise
            return self._provider

    def __getattr__(self, attribute):
        # Only reached for attributes this proxy does not hold itself.
        if attribute.startswith('__') or attribute in ('channels', '_provider', '_lock', '_bundle'):
            raise AttributeError(attribute)
        return getattr(self.built(), attribute)

    def __deepcopy__(self, memo):
        return deepcopy(self.built(), memo)

    def __repr__(self):
        return f'<LazyProvider {self.name} {self._bundle.layout} input {self.input_index}>'


class BundleWatcher:
    """The maps in `directory` the daemon offers, and the ones that landed since it last looked.

    The mapper rewrites index.json after every plug-in it maps; poll() reads it again when it
    changed and returns providers for the (name, layout) pairs not offered yet."""

    def __init__(self, directory=DEFAULT_DIR, *, advertise_all=False, inputs=NATIVE_INPUTS):
        self.directory = Path(directory)
        self.advertise_all, self.inputs = advertise_all, tuple(inputs)
        self.allowed = None if advertise_all else known_good()
        self.offered = set()
        self._seen = None

    def _stamp(self):
        try:
            st = (self.directory / 'index.json').stat()
            return st.st_mtime_ns, st.st_size
        except OSError:
            return None

    def progress(self):
        """The mapper's progress.json ({} without one)."""
        try:
            return json.loads((self.directory / 'progress.json').read_text())
        except (OSError, ValueError):
            return {}

    def poll(self):
        """Providers for maps that landed since the last call ([] when nothing changed)."""
        stamp = self._stamp()
        if stamp is None or stamp == self._seen:
            return []
        try:
            index = json.loads((self.directory / 'index.json').read_text())
        except (OSError, ValueError):
            return []  # caught mid-write by a tool that does not write atomically: next time
        self._seen = stamp
        # The emulator reads these when it first plans a control (prove_sdk_constructor, descriptor).
        if index.get('uad_sdk'):
            os.environ.setdefault('OA_UAD_SDK', index['uad_sdk'])
        os.environ.setdefault('OA_MODULE_STORE', index.get('module_store') or str(MODULES))
        store = Path(os.environ['OA_MODULE_STORE'])
        providers, landed = [], set()
        for entry in index.get('bundles', ()):
            name, layout, file = entry.get('name'), entry.get('layout'), entry.get('file')
            if (name, layout) in self.offered:
                continue
            if not isinstance(name, str) or layout not in ('mono', 'stereo') or not isinstance(file, str):
                continue
            if self.allowed is not None and layout not in self.allowed.get(name, {}).get('layouts', ()):
                continue
            if (name, layout) in landed:
                raise ValueError(f'duplicate profiled bundle: {name} ({layout})')
            path = self.directory / file
            if not path.is_file():
                log.warning('native bundle %s not offered: missing', path.name)
                continue
            # Each layout runs from its own bundle: the plug-in built a mono or a stereo state.
            # The bundle is read and its recipe built when a load first needs it (LazyProvider).
            bundle = _Bundle(path, name, layout)
            offers = ([(StereoProfiledCarrierProvider, i) for i in STEREO_FIRST if i in self.inputs and i + 1 in self.inputs]
                      if layout == 'stereo' else [(ProfiledCarrierProvider, i) for i in self.inputs])
            providers += [LazyProvider(bundle, kind, i, store) for kind, i in offers]
            landed.add((name, layout))
        self.offered |= landed
        return providers

    def summary(self):
        names = {n for n, _ in self.offered}
        both = {n for n, lay in self.offered if lay == 'mono'} & {n for n, lay in self.offered if lay == 'stereo'}
        return (f'{len(names)} plug-ins ({len(both)} in both layouts) from {self.directory} '
                f'({"all built bundles" if self.advertise_all else "known-good list"})')


def create_profiled_providers(directory=DEFAULT_DIR, *, advertise_all=False, inputs=NATIVE_INPUTS, watcher=None):
    """The providers for the maps there now (possibly none yet: the mapper may still be running)."""
    watcher = watcher or BundleWatcher(directory, advertise_all=advertise_all, inputs=inputs)
    providers = ProfiledProviders(watcher.poll())
    log.info('native hosting: %s', watcher.summary())
    return providers
