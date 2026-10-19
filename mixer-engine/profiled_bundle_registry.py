"""Native plug-in providers from the runtime bundles tools/plugin-mapper built on this machine.

  python3 -m plugin_mapper --from <install> --out ~/.local/share/open-apollo/plugin-bundles
  ua_mixer_daemon.py --native-plugin-bundles ~/.local/share/open-apollo/plugin-bundles ...

Each passing bundle is offered mono on every analog input, and on both input pairs as well when
its original descriptor is 2-in/2-out.
Only plug-ins on device_maps/native_known_good.json are advertised unless advertise_all is set:
the others built offline but have not been through a live pass. The Apollo's licence answer
still decides every load.
"""
import json
import logging
import os
from pathlib import Path

from native_carrier import MODULES
from native_profiled_carrier_provider import ProfiledCarrierProvider, StereoProfiledCarrierProvider
from native_chain_composer import compose

log = logging.getLogger(__name__)
DEFAULT_DIR = Path.home() / '.local/share/open-apollo/plugin-bundles'
KNOWN_GOOD = Path(__file__).resolve().parent / 'device_maps/native_known_good.json'


class ProfiledProviders(tuple):
    """The provider tuple the native backend takes; `composer` builds multi-insert chains."""
    composer = staticmethod(compose)


def known_good(path=KNOWN_GOOD):
    return {e['name']: e for e in json.loads(Path(path).read_text())['plugins']}


def create_profiled_providers(directory=DEFAULT_DIR, *, advertise_all=False, inputs=(0, 1, 2, 3)):
    directory = Path(directory)
    index = json.loads((directory / 'index.json').read_text())
    # The emulator reads these when it first plans a control (prove_sdk_constructor, descriptor).
    os.environ.setdefault('OA_UAD_SDK', index['uad_sdk'])
    os.environ.setdefault('OA_MODULE_STORE', index.get('module_store', str(MODULES)))
    store = Path(os.environ['OA_MODULE_STORE'])
    allowed = None if advertise_all else known_good()
    providers, names = [], set()
    for entry in index['bundles']:
        path = directory / entry['file']
        data = json.loads(path.read_text())
        name = data.get('name')
        if data.get('status') != 'pass' or not isinstance(name, str):
            continue
        if allowed is not None and name not in allowed:
            continue
        if name in names:
            raise ValueError(f'duplicate profiled bundle name: {name}')
        names.add(name)
        channels = data.get('carrier_channels', 1)
        if channels not in (1, 2):
            raise ValueError(f'profile bundle carrier_channels must be 1 or 2: {path}')
        offers = [(ProfiledCarrierProvider, i) for i in inputs]
        if channels == 2:  # an original 2/2 descriptor also runs as a stereo carrier on linked pairs
            offers += [(StereoProfiledCarrierProvider, i) for i in (0, 2) if i in inputs and i + 1 in inputs]
        try:
            providers += [kind(path, i, data=data, module_store=store) for kind, i in offers]
        except (OSError, ValueError) as error:
            # One bad bundle must not take the others down; it is simply not offered.
            log.warning('native bundle %s not offered: %s', path.name, error)
    log.info('native hosting: %d plug-ins from %s (%s)', len(names), directory,
             'all built bundles' if advertise_all else 'known-good list')
    return ProfiledProviders(providers)
