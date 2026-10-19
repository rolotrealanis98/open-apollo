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
from pathlib import Path

from native_carrier import MODULES
from native_profiled_carrier_provider import ProfiledCarrierProvider, StereoProfiledCarrierProvider
from native_chain_composer import compose

log = logging.getLogger(__name__)
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


class BundleWatcher:
    """The maps in `directory` the daemon offers, and the ones that landed since it last looked.

    The mapper rewrites index.json after every plug-in it maps; poll() reads it again when it
    changed and returns providers for the (name, layout) pairs not offered yet."""

    def __init__(self, directory=DEFAULT_DIR, *, advertise_all=False, inputs=(0, 1, 2, 3)):
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
            if (entry.get('name'), entry.get('layout')) in self.offered:
                continue
            path = self.directory / entry['file']
            try:
                data = json.loads(path.read_text())
            except (OSError, ValueError) as error:
                log.warning('native bundle %s not offered: %s', path.name, error)
                continue
            name, layout = data.get('name'), data.get('layout')
            if data.get('status') != 'pass' or not isinstance(name, str) or layout not in ('mono', 'stereo'):
                continue
            if self.allowed is not None and layout not in self.allowed.get(name, {}).get('layouts', ()):
                continue
            if (name, layout) in self.offered | landed:
                raise ValueError(f'duplicate profiled bundle: {name} ({layout})')
            if data.get('carrier_channels') != (2 if layout == 'stereo' else 1):
                log.warning('native bundle %s not offered: carrier_channels do not match its layout', path.name)
                continue
            # Each layout runs from its own bundle: the plug-in built a mono or a stereo state.
            offers = ([(StereoProfiledCarrierProvider, i) for i in (0, 2) if i in self.inputs and i + 1 in self.inputs]
                      if layout == 'stereo' else [(ProfiledCarrierProvider, i) for i in self.inputs])
            try:
                providers += [kind(path, i, data=data, module_store=store) for kind, i in offers]
                landed.add((name, layout))
            except (OSError, ValueError) as error:
                # One bad bundle must not take the others down; it is simply not offered.
                log.warning('native bundle %s not offered: %s', path.name, error)
        self.offered |= landed
        return providers

    def summary(self):
        names = {n for n, _ in self.offered}
        both = {n for n, lay in self.offered if lay == 'mono'} & {n for n, lay in self.offered if lay == 'stereo'}
        return (f'{len(names)} plug-ins ({len(both)} in both layouts) from {self.directory} '
                f'({"all built bundles" if self.advertise_all else "known-good list"})')


def create_profiled_providers(directory=DEFAULT_DIR, *, advertise_all=False, inputs=(0, 1, 2, 3), watcher=None):
    """The providers for the maps there now (possibly none yet: the mapper may still be running)."""
    watcher = watcher or BundleWatcher(directory, advertise_all=advertise_all, inputs=inputs)
    providers = ProfiledProviders(watcher.poll())
    log.info('native hosting: %s', watcher.summary())
    return providers
