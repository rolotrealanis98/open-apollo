#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Owner-attended checks of the driver's plug-in host (HARDWARE-SESSIONS.md).

HARDWARE TOOL: it opens /dev/ua_apolloN.  Run it only during an attended
hardware session, as the plan's runbook says; never from CI or an agent.

  version            GET_VERSION: ABI, features, the second feature word
  caps               GET_PLUGIN_CAPS: accepted operations, state, owned and
                     quarantined DSPs, instances (reads only)
  query [IDS...]     UA_IOCTL_BOARD_QUERY (plugin_host=1): sends the date
                     and the query to DSP 0, as UA's own driver does on
                     every refresh, and prints how many plug-ins are in each
                     state, then the state of each render id or plug-in
                     name given (names from the module store's manifest).
                     The table itself is never printed.
  check IDS...       UA_PLUGIN_INST_CREATE with UA_PLUGIN_CREATE_F_CHECK:
                     the driver asks the device and decides; nothing is
                     placed or uploaded.  Prints the decision per id.
  proc               /proc/asound/cardN/plugin_host

IDS are render ids (2054 or 0x806) or plug-in names as in the manifest
("UAD UA 1176LN Legacy").  Nothing here creates an instance, starts a
demo or changes what the device holds.
"""
import argparse
import errno
import fcntl
import glob
import json
import os
import struct
import sys

MAGIC = ord('U')
MANIFEST = '/lib/firmware/universal-audio/plugins/manifest.json'

STATES = {0: 'authorized', 1: 'demo running', 2: 'demo not started',
          3: 'demo expired', 4: 'unavailable', 0xffffffff: 'no answer'}

# struct layouts (driver/ua_ioctl.h, ABI 1.8)
VERSION = struct.Struct('<4I16s4I')                  # 48 bytes
CAPS = struct.Struct('<IIQ6I6I')                     # 64 bytes
BOARD_QUERY = struct.Struct('<4IQ768I')              # 3096 bytes
INSTANCE = struct.Struct('<8IQQQII6I')               # 88 bytes


def _ioc(direction, nr, size):
    return (direction << 30) | (size << 16) | (MAGIC << 8) | nr


def _ior(nr, size):
    return _ioc(2, nr, size)


def _iowr(nr, size):
    return _ioc(3, nr, size)


GET_VERSION = _ior(0x04, VERSION.size)
GET_PLUGIN_CAPS = _ior(0x07, CAPS.size)
BOARD_QUERY_IOCTL = _iowr(0x90, BOARD_QUERY.size)
PLUGIN_INSTANCE = _iowr(0x94, INSTANCE.size)

UA_FEAT_FEATURES2 = 1 << 31
UA_FEAT2_PLUGIN_HOST = 1
OPS = {1: 'BOARD_QUERY', 2: 'CREATE', 4: 'DESTROY'}
STATE_BITS = {1: 'fw_loaded', 2: 'dsps_connected', 4: 'init_chain',
              8: 'audio_connected', 16: 'plugin_host', 32: 'input_strips'}
UA_PLUGIN_INST_CREATE = 1
UA_PLUGIN_CREATE_F_CHECK = 1
UA_PLUGIN_DSP_ANY = 0xffffffff


def state_of(word):
    """UA's SDK states (PCIe f640), for the summary only."""
    if word in (0x81000000, 0x82000000):
        return 0
    return {0x80000000: 2, 0: 3, 0x83000000: 4}.get(word, 1)


def unpack_version(buf):
    abi_major, abi_minor, drv, features, name, r0, r1, r2, r3 = VERSION.unpack(buf)
    return {'abi': '%d.%d' % (abi_major, abi_minor), 'driver': name.split(b'\0')[0].decode(),
            'features': features,
            'features2': r0 if features & UA_FEAT_FEATURES2 else 0}


def unpack_caps(buf):
    f = CAPS.unpack(buf)
    return {'struct_size': f[0], 'state': f[1], 'operations': f[2],
            'num_dsps': f[3], 'dsp_rings': f[4], 'dma_orphans': f[5],
            'owned_dsps': f[6], 'quarantined_dsps': f[7], 'instances': f[8]}


def pack_board_query():
    return bytearray(BOARD_QUERY.pack(BOARD_QUERY.size, 0, 0, 0, 0, *([0] * 768)))


def unpack_board_query(buf):
    f = BOARD_QUERY.unpack(buf)
    return {'date': f[2], 'epoch': f[4], 'words': f[5:]}


def pack_check(render_id):
    return bytearray(INSTANCE.pack(INSTANCE.size, UA_PLUGIN_INST_CREATE,
                                   UA_PLUGIN_CREATE_F_CHECK, 0, render_id,
                                   UA_PLUGIN_DSP_ANY, 0, 0, 0, 0, 0, 0, 0,
                                   *([0] * 6)))


def unpack_instance(buf):
    f = INSTANCE.unpack(buf)
    return {'handle': f[3], 'dsp': f[5], 'epoch': f[10],
            'license_state': f[11], 'license_word': f[12]}


def node(path):
    if path:
        return path
    nodes = sorted(glob.glob('/dev/ua_apollo*'))
    if not nodes:
        sys.exit('no /dev/ua_apollo* node')
    return nodes[0]


def names():
    try:
        with open(MANIFEST, encoding='utf-8') as f:
            doc = json.load(f)
    except (OSError, ValueError):
        return {}
    return {p['name']: p['render_id'] for p in doc.get('plugins', [])
            if isinstance(p.get('render_id'), int) and p.get('name')}


def render_ids(args):
    known = names()
    out = []
    for a in args:
        try:
            out.append((a, int(a, 0)))
        except ValueError:
            if a not in known:
                sys.exit('%s: not a render id and not in %s' % (a, MANIFEST))
            out.append((a, known[a]))
    return out


def cmd_version(fd):
    v = unpack_version(fcntl.ioctl(fd, GET_VERSION, bytes(VERSION.size)))
    print('ABI %s, driver %s, features 0x%08x, features2 0x%08x (plugin_host %s)'
          % (v['abi'], v['driver'], v['features'], v['features2'],
             'on' if v['features2'] & UA_FEAT2_PLUGIN_HOST else 'off'))


def cmd_caps(fd):
    c = unpack_caps(fcntl.ioctl(fd, GET_PLUGIN_CAPS, bytes(CAPS.size)))
    ops = [n for b, n in OPS.items() if c['operations'] & b] or ['none']
    state = [n for b, n in STATE_BITS.items() if c['state'] & b] or ['none']
    print('operations: %s' % ' '.join(ops))
    print('state: %s' % ' '.join(state))
    print('DSPs %d, rings 0x%x, owned 0x%x, quarantined 0x%x, instances %d, parked DMA %d'
          % (c['num_dsps'], c['dsp_rings'], c['owned_dsps'],
             c['quarantined_dsps'], c['instances'], c['dma_orphans']))


def cmd_query(fd, ids):
    buf = pack_board_query()
    fcntl.ioctl(fd, BOARD_QUERY_IOCTL, buf)
    q = unpack_board_query(bytes(buf))
    counts = {}
    for w in q['words']:
        s = state_of(w)
        counts[s] = counts.get(s, 0) + 1
    print('date word 0x%08x, epoch %d' % (q['date'], q['epoch']))
    for s in sorted(counts):
        print('  %-18s %d' % (STATES[s], counts[s]))
    for label, rid in render_ids(ids):
        if not 0x800 <= rid < 0xb00:
            print('%s (0x%x): outside the table' % (label, rid))
            continue
        print('%s (0x%x): %s' % (label, rid, STATES[state_of(q['words'][rid - 0x800])]))


def cmd_check(fd, ids):
    for label, rid in render_ids(ids):
        buf = pack_check(rid)
        try:
            fcntl.ioctl(fd, PLUGIN_INSTANCE, buf)
            r = unpack_instance(bytes(buf))
            print('%s (0x%x): admitted (%s)' % (label, rid, STATES.get(r['license_state'], '?')))
        except OSError as e:
            if e.errno == errno.EACCES:
                r = unpack_instance(bytes(buf))
                print('%s (0x%x): refused (%s)' % (label, rid,
                                                    STATES.get(r['license_state'], '?')))
            else:
                print('%s (0x%x): no answer (%s)' % (label, rid, os.strerror(e.errno)))


def cmd_proc():
    for path in sorted(glob.glob('/proc/asound/card*/plugin_host')):
        print('== %s' % path)
        with open(path, encoding='utf-8') as f:
            sys.stdout.write(f.read())


def main():
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--device', help='/dev/ua_apolloN (default: the first)')
    parser.add_argument('command', choices=('version', 'caps', 'query', 'check', 'proc'))
    parser.add_argument('ids', nargs='*')
    args = parser.parse_args()
    if args.command == 'proc':
        cmd_proc()
        return
    fd = os.open(node(args.device), os.O_RDWR)
    try:
        if args.command == 'version':
            cmd_version(fd)
        elif args.command == 'caps':
            cmd_caps(fd)
        elif args.command == 'query':
            cmd_query(fd, args.ids)
        else:
            if not args.ids:
                sys.exit('check needs render ids or names')
            cmd_check(fd, args.ids)
    finally:
        os.close(fd)


if __name__ == '__main__':
    main()
