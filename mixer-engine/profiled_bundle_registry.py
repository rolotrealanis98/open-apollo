"""Native plug-in providers from the runtime bundles tools/plugin-mapper built on this machine.

  python3 -m plugin_mapper --from <install> --out ~/.local/share/open-apollo/plugin-bundles
  ua_mixer_daemon.py --native-plugin-bundles ~/.local/share/open-apollo/plugin-bundles ...

A plug-in has a mono bundle (offered on every analog input) and, when its original descriptor
is 2-in/2-out, a stereo bundle (offered on the linked pairs 1/2 and 3/4). Only the layouts
device_maps/native_known_good.json lists for a plug-in are advertised unless advertise_all is
set: the others built offline but have not been through a live pass. The Apollo's licence
answer still decides every load.
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
    providers, offered = [], set()
    for entry in index['bundles']:
        path = directory / entry['file']
        data = json.loads(path.read_text())
        name, layout = data.get('name'), data.get('layout')
        if data.get('status') != 'pass' or not isinstance(name, str) or layout not in ('mono', 'stereo'):
            continue
        if allowed is not None and layout not in allowed.get(name, {}).get('layouts', ()):
            continue
        if (name, layout) in offered:
            raise ValueError(f'duplicate profiled bundle: {name} ({layout})')
        if data.get('carrier_channels') != (2 if layout == 'stereo' else 1):
            raise ValueError(f'profile bundle carrier_channels do not match its layout: {path}')
        # Each layout runs from its own bundle: the plug-in built a mono or a stereo state.
        offers = ([(StereoProfiledCarrierProvider, i) for i in (0, 2) if i in inputs and i + 1 in inputs]
                  if layout == 'stereo' else [(ProfiledCarrierProvider, i) for i in inputs])
        try:
            providers += [kind(path, i, data=data, module_store=store) for kind, i in offers]
            offered.add((name, layout))
        except (OSError, ValueError) as error:
            # One bad bundle must not take the others down; it is simply not offered.
            log.warning('native bundle %s not offered: %s', path.name, error)
    both = {n for n, lay in offered if lay == 'mono'} & {n for n, lay in offered if lay == 'stereo'}
    log.info('native hosting: %d plug-ins (%d in both layouts) from %s (%s)', len({n for n, _ in offered}),
             len(both), directory, 'all built bundles' if advertise_all else 'known-good list')
    return ProfiledProviders(providers)
