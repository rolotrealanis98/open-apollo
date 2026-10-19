"""Which plug-ins the Apollo authorizes, for setup and reports (not for the daemon's own gate).

The answer is the device's board table (plugin_license), never a guess:
  1. a running mixer daemon owns the device and already publishes it as /plugins/N/Authorized,
     /Demoing and /Status, so ask it first (one recursive get, read-only);
  2. with no daemon answering, read the table directly (UA_IOCTL_BOARD_QUERY, read-only, needs
     the driver loaded with plugin_host=1 and the device free) and decode it the way the daemon
     does, with render ids from the oa-assets DSP manifest.
When neither works the answer is None: callers decide what "unknown" means for them (setup maps
every known-good plug-in; a report says nothing). Only counts are ever logged, never the table.

  python3 mixer-engine/plugin_authorization.py [--json] [--names-only]
"""
from __future__ import annotations

import argparse
import json
import logging
import socket
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import plugin_license  # noqa: E402

log = logging.getLogger(__name__)

AUTHORIZED, DEMO, UNAUTHORIZED, UNKNOWN = 'authorized', 'demo', 'unauthorized', 'unknown'
_STATES = {plugin_license.AUTHORIZED: AUTHORIZED, plugin_license.DEMO: DEMO}


def from_daemon(host='127.0.0.1', port=4710, timeout=10.0):
    """{name: state} from a running daemon's /plugins tree, or None (no daemon, no answer yet)."""
    try:
        with socket.create_connection((host, port), timeout) as s:
            s.settimeout(timeout)
            s.sendall(b'get /plugins?func_id=oa-auth&recursive=1\0')
            buf = b''
            while True:
                while b'\0' not in buf:
                    chunk = s.recv(1 << 16)
                    if not chunk:
                        return None
                    buf += chunk
                raw, buf = buf.split(b'\0', 1)
                if not raw:
                    continue
                reply = json.loads(raw)
                if str((reply.get('parameters') or {}).get('func_id')) == 'oa-auth':
                    break
    except (OSError, ValueError):
        return None
    children = ((reply.get('data') or {}).get('children') or {}) if 'error' not in reply else {}
    out = {}
    for node in children.values():
        props = node.get('properties') or {}
        value = lambda key: (props.get(key) or {}).get('value')  # noqa: E731
        name = value('Name')
        if not isinstance(name, str) or not name:
            continue
        if value('Authorized') is True:
            out[name] = AUTHORIZED
        elif value('Demoing') is True:
            out[name] = DEMO
        elif value('Status'):
            out[name] = UNAUTHORIZED
        else:
            out[name] = UNKNOWN  # the daemon has no answer for it (yet)
    return _answer(out)


def from_device(device='/dev/ua_apollo0', manifest=None):
    """{name: state} from the device's own board table, or None (busy, no plug-in host, no manifest)."""
    import os
    sys.path.insert(0, str(Path(__file__).resolve().parent))
    try:
        from hardware import HardwareBackend
    except Exception:
        return None
    path = Path(manifest) if manifest else plugin_license.DEFAULT_MANIFEST
    try:
        entries = json.loads(path.read_text())['plugins']
    except (OSError, ValueError, KeyError):
        return None
    if not os.path.exists(device):
        return None
    backend = HardwareBackend(device)
    try:
        if not backend.open() or not plugin_license.plugin_host_available(backend):
            return None
        _date, _epoch, words = backend.board_query()
    except Exception:
        return None
    finally:
        try:
            backend.close()
        except Exception:
            pass
    out = {}
    for entry in entries:
        name = entry.get('name')
        if not isinstance(name, str):
            continue
        state, _props = plugin_license.decide(words, plugin_license._licence_id(entry))
        out[name] = _STATES.get(state, UNKNOWN if state is None else UNAUTHORIZED)
    return _answer(out)


def _answer(states):
    """An answer only when the device authorized something: a table with nothing authorized is
    what an engine without a device reports (UA's own says "No UAD-2 devices found" for every
    plug-in), and "unknown" is the safe reading of it."""
    return states if any(v in (AUTHORIZED, DEMO) for v in states.values()) else None


def authorized_plugins(*, host='127.0.0.1', port=4710, device='/dev/ua_apollo0', manifest=None, timeout=10.0):
    """{'source': 'daemon'|'device'|None, 'plugins': {name: state}} (plugins empty when unknown)."""
    for source, read in (('daemon', lambda: from_daemon(host, port, timeout)),
                         ('device', lambda: from_device(device, manifest))):
        plugins = read()
        if plugins:
            counts = {s: sum(v == s for v in plugins.values()) for s in (AUTHORIZED, DEMO, UNAUTHORIZED, UNKNOWN)}
            log.info('plug-in authorization from the %s: %s', source, counts)
            return dict(source=source, plugins=plugins)
    return dict(source=None, plugins={})


def authorized_names(answer):
    """The names a build may map: authorized or demoing (a demo is a licence the user started)."""
    return {name for name, state in answer['plugins'].items() if state in (AUTHORIZED, DEMO)}


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split('\n\n')[0])
    ap.add_argument('--json', action='store_true', help='the whole answer as JSON')
    ap.add_argument('--names-only', action='store_true', help='one authorized (or demo) name per line')
    ap.add_argument('--port', type=int, default=4710)
    ap.add_argument('--device', default='/dev/ua_apollo0')
    args = ap.parse_args(argv)
    answer = authorized_plugins(port=args.port, device=args.device)
    if args.json:
        print(json.dumps(answer, indent=1, sort_keys=True))
    elif args.names_only:
        print('\n'.join(sorted(authorized_names(answer))))
    else:
        names = authorized_names(answer)
        print(f"source: {answer['source'] or 'none (no daemon answer, device not readable)'}; "
              f"{len(names)} of {len(answer['plugins'])} plug-ins authorized or demoing")
    return 0 if answer['source'] else 1


if __name__ == '__main__':
    sys.exit(main())
