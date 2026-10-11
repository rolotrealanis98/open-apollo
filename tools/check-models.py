#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Consistency checker for the Apollo model data.

Checks, and fails (exit 1) on any error:

  1. devices/*.json against themselves: required fields, unique model names,
     mixer classes/type IDs, device types and serial prefixes; preamp/HiZ
     counts against the mixer inputs and the MicLine record route channel
     IDs; per rate band x TOSLink mode record/playback route counts against
     `channel_counts` / `channels`; route channel IDs (unique, type byte
     <-> io kind, name <-> channel number); cue buses, talkback, TOSLink
     rules against the routes.
  2. driver/ua_models_gen.h against the JSON (the same test as
     `gen-driver-models.py --check`).
  3. The hand-written per-model tables against the JSON: the UA_DEV_*
     names/serials in driver/ua_apollo.h, the serial table, the subsystem
     pins in ua_detect_capabilities(), UA_MAX_PREAMP_CH and the frozen
     legacy DMA table (ua_legacy_dma[], preamps/HiZ) in driver/ua_audio.c.
  4. Optional: the extracted facts in apollo-linux/reference/model-data
     (--reference DIR, or the default sibling checkout when present).

Warnings (printed, exit 0) are things that look odd but have an explanation
on record, e.g. the legacy io_map=0 rows that were frozen on purpose.

Usage:
  tools/check-models.py                  check, print errors and warnings
  tools/check-models.py -q               print errors only
  tools/check-models.py --reference DIR  also check against model-data DIR

Standard library only.
"""
import argparse
import glob
import importlib.util
import json
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DEVICES = os.path.join(ROOT, 'devices')
DEFAULT_REF = os.path.join(os.path.dirname(ROOT), 'apollo-linux',
                           'reference', 'model-data')

BANDS = ('LoMid', 'Hi')
MODES = ('spdif', 'adat')
DIRS = ('record', 'playback')

# Device types whose io_map=0 DMA row is hardware-verified; every other row is
# frozen on purpose to whatever the old hand-written table said (see the
# comment above ua_legacy_dma[] in driver/ua_audio.c), so a preamp/HiZ
# difference there is a warning, on these two it is an error.
LEGACY_VERIFIED = {0x1F, 0x22}

# Mixer classes whose engine exposes the HiZ control on the first input
# only (parity map 16 sec. 3.1): their route tables tag both analog inputs
# MicLineHiZ, so the route-derived count over-counts.
HIZ_FIRST_INPUT_ONLY = frozenset((
    'Armstrong', 'Stretch', 'Lavern', 'LavernG2', 'Shirley',
    'Conrad', 'ApolloSolo', 'Cauldron'))

# Names a "clock_sources" list may use (the device map's /ClockSource names).
CLOCK_SOURCE_NAMES = ('Internal', 'Word Clock', 'ADAT', 'S/PDIF', 'AES/EBU',
                      'Dante', 'MADI')


def expected_hiz(d, route_hiz):
    """HiZ inputs the engine exposes, given the MicLineHiZ route count."""
    if d.get('mixer_class') in HIZ_FIRST_INPUT_ONLY:
        return min(route_hiz, 1)
    return route_hiz


def load_generator():
    path = os.path.join(ROOT, 'tools', 'gen-driver-models.py')
    spec = importlib.util.spec_from_file_location('gen_driver_models', path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


gen = load_generator()


class Report:
    def __init__(self):
        self.errors = []
        self.warnings = []

    def err(self, where, msg):
        self.errors.append(f'{where}: {msg}')

    def warn(self, where, msg):
        self.warnings.append(f'{where}: {msg}')


def hexint(v):
    return int(str(v), 16)


def read(path):
    with open(os.path.join(ROOT, path), encoding='utf-8') as f:
        return f.read()


def slug_of(path):
    return os.path.splitext(os.path.basename(path))[0]


def ident(s):
    """Normalise a model name / UA_DEV identifier for comparison."""
    s = s.lower().replace('gen 2', 'gen2')
    return re.sub(r'[^a-z0-9]+', '_', s).strip('_').removeprefix('apollo_')


# ---------------------------------------------------------------- JSON side

def load_devices(rep):
    devs = {}
    for path in sorted(glob.glob(os.path.join(DEVICES, '*.json'))):
        rel = os.path.relpath(path, ROOT)
        try:
            with open(path, encoding='utf-8') as f:
                d = json.load(f)
        except ValueError as e:
            rep.err(rel, f'unreadable JSON: {e}')
            continue
        d['_rel'] = rel
        d['_slug'] = slug_of(path)
        devs[d['_slug']] = d
    return devs


def route_lists(d, rep):
    """{(band, mode, dir): [ch_id,...]} with the static visibility applied."""
    rel = d['_rel']
    rules = d.get('route_rules') or {}
    out = {}
    for band in BANDS:
        b = (d.get('routes') or {}).get(band)
        if b is None:
            rep.err(rel, f'no {band} routes')
            continue
        for dk in DIRS:
            ids = []
            for e in b.get(dk, []):
                try:
                    ids.append(hexint(e['ch_id']))
                except (KeyError, ValueError):
                    rep.err(rel, f'{band}/{dk}: bad route entry {e!r}')
            for mode in MODES:
                try:
                    out[(band, mode, dk)] = gen.shown(ids, mode, rules)
                except gen.GenError as e:
                    rep.err(rel, str(e))
    return out


def check_device(d, rep, type_io):
    rel = d['_rel']
    slug = d['_slug']
    for key in ('model', 'mixer_class', 'mixer_type_id', 'preamps', 'hiz',
                'routes', 'route_rules', 'device_types', 'serial_prefixes',
                'cue_buses', 'talkback', 'digital_io'):
        if key not in d:
            rep.err(rel, f'missing field {key!r}')
    if 'model' not in d or 'routes' not in d:
        return None

    if ident(d['model']).replace('_', '-') != slug.removeprefix('apollo-'):
        rep.warn(rel, f'file name does not match model {d["model"]!r}')

    # --- identity fields
    dts = d.get('device_types') or []
    if 'device_type' in d:
        if not dts or hexint(d['device_type']) != hexint(dts[0]):
            rep.err(rel, f'device_type {d["device_type"]} is not '
                         f'device_types[0] {dts}')
    elif dts:
        rep.err(rel, 'device_types set but no device_type')
    for t in (d.get('device_type_names') or {}):
        if t not in dts:
            rep.err(rel, f'device_type_names key {t} not in device_types')
    for t in dts:
        v = hexint(t)
        if not 0 < v <= 0x3F + 0x10:
            rep.err(rel, f'device type {t} out of range')
    sp = d.get('serial_prefixes') or []
    for p in sp:
        if not re.fullmatch(r'\d{4}', str(p)):
            rep.err(rel, f'serial prefix {p!r} is not 4 digits')
    if len(set(sp)) != len(sp):
        rep.err(rel, 'duplicate serial prefix')
    if 'serial_prefix' in d and d['serial_prefix'] not in sp:
        rep.err(rel, f'serial_prefix {d["serial_prefix"]} not in '
                     f'serial_prefixes {sp}')
    if 'serial_leading' in d and d['serial_leading'] in sp and \
            d['serial_leading'] != d.get('serial_prefix'):
        rep.err(rel, 'serial_leading collides with the model prefixes: it '
                     'is not a model code (see the x4 notes)')
    if dts and not sp:
        rep.warn(rel, 'has a device type but no serial prefix (identified '
                      'by subsystem/ext_caps instead)')
    if 'subsystem_id' in d:
        if not 0 <= hexint(d['subsystem_id']) <= 0xFFFF:
            rep.err(rel, f'bad subsystem_id {d["subsystem_id"]}')

    # --- route lists
    lists = route_lists(d, rep)
    if len(lists) != len(BANDS) * len(MODES) * len(DIRS):
        return None
    rules = d.get('route_rules') or {}
    for band in BANDS:
        for dk in DIRS:
            entries = d['routes'][band].get(dk, [])
            ids = [hexint(e['ch_id']) for e in entries]
            names = [e.get('name') for e in entries]
            if len(set(ids)) != len(ids):
                rep.err(rel, f'{band}/{dk}: duplicate ch_id')
            if len(set(names)) != len(names):
                rep.err(rel, f'{band}/{dk}: duplicate route name')
            for e in entries:
                cid = hexint(e['ch_id'])
                io = e.get('io')
                tb = cid >> 8
                # io kind per type byte is global across models, except the
                # mic/line family which splits one type byte by HiZ.
                if tb in (0, 1):
                    kind = ('MicLineHiZ' if tb == 0 else 'MicLine')
                    if io != kind:
                        rep.err(rel, f'{band}/{dk} {e["ch_id"]}: type byte '
                                     f'{tb:#x} is {kind}, io says {io!r}')
                else:
                    prev = type_io.setdefault(tb, (io, rel))
                    if prev[0] != io:
                        rep.err(rel, f'{band}/{dk} {e["ch_id"]}: io {io!r} '
                                     f'but {prev[1]} uses {prev[0]!r} for '
                                     f'type byte {tb:#x}')
                m = re.search(r'(\d+)(?:\D*)$', e.get('name', ''))
                if m and e.get('io') not in ('Aux', 'Cue') and \
                        int(m.group(1)) != (cid & 0xff) and \
                        not e.get('name', '').endswith(('L', 'R')):
                    rep.warn(rel, f'{band}/{dk}: name {e["name"]!r} vs '
                                  f'channel {cid & 0xff} of {e["ch_id"]}')
    # the two bands list the same channels for every Apollo seen so far
    for mode in MODES:
        for dk in DIRS:
            lo, hi = lists[('LoMid', mode, dk)], lists[('Hi', mode, dk)]
            if len(lo) != len(hi):
                rep.warn(rel, f'{dk}/{mode}: LoMid has {len(lo)} channels, '
                              f'Hi {len(hi)}')

    # --- recorded route counts vs channel_counts / channels
    cc = d.get('channel_counts')
    if cc:
        for band in BANDS:
            for mode in MODES:
                for dk in DIRS:
                    want = cc.get(band, {}).get(mode, {}).get(dk)
                    got = len(lists[(band, mode, dk)])
                    if want is not None and want != got:
                        rep.err(rel, f'channel_counts {band}/{mode}/{dk} = '
                                     f'{want}, routes give {got}')
    ch = d.get('channels')
    if ch:
        mode = d.get('legacy_digital_mode', 'spdif')
        for key, dk in (('play', 'playback'), ('rec', 'record')):
            got = len(lists[('LoMid', mode, dk)])
            if ch.get(key) != got:
                rep.err(rel, f'channels.{key} = {ch.get(key)}, LoMid/{mode} '
                             f'{dk} routes give {got}')

    # --- preamps / HiZ
    rec = [hexint(e['ch_id']) for e in d['routes']['LoMid'].get('record', [])]
    hiz_routes = sum(1 for c in rec if c >> 8 == 0)
    # preamp channels: the MIC/LINE record routes (the x8p lists MIC/LINE 5-8
    # under the Line type byte 0x03, so the type byte alone undercounts)
    mic_routes = sum(1 for e in d['routes']['LoMid'].get('record', [])
                     if e.get('name', '').startswith('MIC/LINE'))
    pre, hiz = d.get('preamps', 0), d.get('hiz', 0)
    if hiz > pre:
        rep.err(rel, f'hiz {hiz} > preamps {pre}')
    if pre > 8:
        rep.err(rel, f'preamps {pre} > UA_MAX_PREAMP_CH')
    if hiz != expected_hiz(d, hiz_routes):
        rep.err(rel, f'hiz {hiz} but {hiz_routes} MicLineHiZ record routes'
                     f' (engine exposes {expected_hiz(d, hiz_routes)})')
    if pre != mic_routes:
        rep.err(rel, f'preamps {pre} but {mic_routes} MicLine/MicLineHiZ '
                     f'record routes')
    mi = d.get('mixer_inputs')
    if mi is not None:
        # (the per-input 'preamp' flag marks every analog input, line-only
        # inputs of the Apollo 16 included, so it is not a preamp count)
        names = [e['name'] for e in mi]
        if len(set(names)) != len(names):
            rep.err(rel, 'duplicate mixer input name')
        sy = [e['synch_in'] for e in mi]
        if len(set(sy)) != len(sy):
            rep.err(rel, 'duplicate mixer input synch_in')
        for e in mi:
            if not re.fullmatch(r'[A-Z/]+( \d+| [LR])?', e['name']):
                rep.err(rel, f'mixer input name {e["name"]!r} is not '
                             f'"<KIND> <n>" (a mixer class name leaked in?)')
        tb_in = [e for e in mi if e['name'] == 'TALKBACK']
        if d.get('talkback') and not tb_in:
            rep.err(rel, 'talkback model without a TALKBACK mixer input')
        if tb_in and d.get('talkback_synch_in') not in (None, tb_in[0]['synch_in']):
            rep.warn(rel, f'talkback_synch_in {d.get("talkback_synch_in")} '
                         f'but the TALKBACK input uses {tb_in[0]["synch_in"]}')
        if not d.get('talkback') and tb_in:
            rep.warn(rel, 'TALKBACK mixer input on a non-talkback model '
                          '(the mixer class lists it; the model has no '
                          'talkback routes)')

    # --- cue buses, talkback, TOSLink, digital I/O
    cb = d.get('cue_buses') or {}
    if cb.get('max') is not None and cb['default'] > cb['max']:
        rep.warn(rel, f'cue default {cb["default"]} > max {cb["max"]}')
    tb_routes = [c for c in rec if c >> 8 == 0x0B]
    if bool(tb_routes) != bool(d.get('talkback')):
        rep.err(rel, f'talkback={d.get("talkback")} but {len(tb_routes)} '
                     f'Talkback record routes')
    all_ids = [hexint(e['ch_id']) for dk in DIRS
               for e in d['routes']['LoMid'].get(dk, [])]
    has_adat = any(c >> 8 == gen.TYPE_ADAT for c in all_ids)
    has_spdif = any(c >> 8 == gen.TYPE_SPDIF for c in all_ids)
    toslink = rules.get('toslink', 'none')
    if toslink != 'none' and not (has_adat and has_spdif):
        rep.err(rel, f'toslink rule {toslink!r} needs both ADAT and S/PDIF '
                     f'routes')
    if toslink == 'none' and has_adat and has_spdif:
        rep.warn(rel, 'ADAT and S/PDIF routes but toslink rule is none')
    if rules.get('default_input') == 'adat' and not has_adat:
        rep.err(rel, 'default_input adat without ADAT routes')
    if rules.get('adat_mid_rate_max') and not has_adat:
        rep.err(rel, 'adat_mid_rate_max without ADAT routes')
    if d.get('legacy_digital_mode', 'spdif') == 'adat' and not has_adat:
        rep.err(rel, 'legacy_digital_mode adat without ADAT routes')
    aes = any(e.get('name', '').startswith('AES')
              for e in d['routes']['LoMid'].get('record', []))
    if (d.get('digital_io') == 'aes') != aes:
        rep.err(rel, f'digital_io={d.get("digital_io")!r} but AES record '
                     f'routes: {aes}')
    madi = any(e.get('name', '').startswith('MADI')
               for e in d['routes']['LoMid'].get('record', []))
    if (d.get('digital_io') == 'madi') != madi:
        rep.err(rel, f'digital_io={d.get("digital_io")!r} but MADI record '
                     f'routes: {madi}')
    cs = d.get('clock_sources')
    if cs is not None:
        bad = [n for n in cs if n not in CLOCK_SOURCE_NAMES]
        if bad or not cs or cs[0] != 'Internal' or len(set(cs)) != len(cs):
            rep.err(rel, f'clock_sources {cs!r}: must start with Internal, '
                         f'be unique and use {CLOCK_SOURCE_NAMES}')
        digital = {'aes': 'AES/EBU', 'madi': 'MADI'}.get(d.get('digital_io'))
        if digital and digital not in cs:
            rep.err(rel, f'clock_sources lacks {digital} (digital_io '
                         f'{d.get("digital_io")!r})')
        if has_adat and 'ADAT' not in cs and rules.get('default_input') == 'adat':
            rep.err(rel, 'default_input adat but no ADAT clock source')
    vmax = rules.get('virtual_max')
    virt = [c & 0xff for c in rec if c >> 8 == gen.TYPE_VIRTUAL]
    if vmax and virt and max(virt) <= vmax:
        rep.warn(rel, f'virtual_max {vmax} hides nothing '
                      f'(highest virtual channel {max(virt)})')
    return lists


def check_cross(devs, rep):
    seen = {}
    for field in ('model', 'mixer_class', 'mixer_type_id'):
        seen.clear()
        for slug, d in devs.items():
            v = d.get(field)
            if v is None:
                continue
            if v in seen:
                rep.err(d['_rel'], f'{field} {v!r} also in {seen[v]}')
            seen[v] = d['_rel']
    seen.clear()
    for slug, d in devs.items():
        for t in d.get('device_types') or []:
            v = hexint(t)
            if v in seen:
                rep.err(d['_rel'], f'device type {t} also in {seen[v]}')
            seen[v] = d['_rel']
    seen.clear()
    for slug, d in devs.items():
        for p in d.get('serial_prefixes') or []:
            if p in seen:
                rep.err(d['_rel'], f'serial prefix {p} also in {seen[p]}')
            seen[p] = d['_rel']
    seen.clear()
    for slug, d in devs.items():
        if 'subsystem_id' in d:
            v = hexint(d['subsystem_id'])
            if v in seen:
                rep.err(d['_rel'], f'subsystem_id {d["subsystem_id"]} also '
                                   f'in {seen[v]}')
            seen[v] = d['_rel']


# --------------------------------------------------------------- header side

def check_generated_header(rep):
    try:
        text = gen.generate(gen.load_models())
    except (gen.GenError, KeyError, ValueError) as e:
        rep.err('driver/ua_models_gen.h', f'generator failed: {e}')
        return
    try:
        cur = read('driver/ua_models_gen.h')
    except OSError:
        cur = None
    if cur != text:
        rep.err('driver/ua_models_gen.h', 'out of date; run '
                                          'tools/gen-driver-models.py')


# ------------------------------------------------------------ hand-written C

def c_function(src, name):
    m = re.search(r'^\w[^\n;{]*\b' + re.escape(name) + r'\([^)]*\)\s*\{',
                  src, re.M)
    if not m:
        return ''
    depth, i = 0, m.end() - 1
    while i < len(src):
        if src[i] == '{':
            depth += 1
        elif src[i] == '}':
            depth -= 1
            if depth == 0:
                return src[m.start():i + 1]
        i += 1
    return ''


def check_hand_tables(devs, rep):
    hdr = read('driver/ua_apollo.h')
    core = read('driver/ua_core.c')
    audio = read('driver/ua_audio.c')

    by_type = {}
    for d in devs.values():
        for t in d.get('device_types') or []:
            by_type[hexint(t)] = d

    # UA_DEV_* defines
    defs = {}
    for m in re.finditer(r'#define\s+(UA_DEV_\w+)\s+(0x[0-9A-Fa-f]+)'
                         r'\s*(?:/\*(.*?)\*/)?', hdr):
        defs[m.group(1)] = (int(m.group(2), 16), m.group(3) or '')
    type_ident = {}
    for name, (val, cmt) in defs.items():
        type_ident[val] = name
        d = by_type.get(val)
        sm = re.search(r'Serial\s+(\d{4})(?:/(\d{4}))?', cmt)
        if d is None:
            if not name.startswith('UA_DEV_SERIAL_'):
                rep.err('driver/ua_apollo.h', f'{name} = {val:#04x} has no '
                                              f'devices/*.json')
            continue
        if not name.startswith('UA_DEV_SERIAL_'):
            nm = ident(d.get('device_type_names', {}).get(f'0x{val:02X}',
                                                          d['model']))
            if not ident(name.removeprefix('UA_DEV_')).startswith(nm):
                rep.err('driver/ua_apollo.h',
                        f'{name} = {val:#04x} but {d["_rel"]} names it '
                        f'{d["model"]!r}')
        if sm:
            for p in filter(None, sm.groups()):
                if p not in (d.get('serial_prefixes') or []):
                    rep.err('driver/ua_apollo.h',
                            f'{name} comment says serial {p}, '
                            f'{d["_rel"]} has {d.get("serial_prefixes")}')
    for val, d in by_type.items():
        if val not in type_ident:
            rep.err(d['_rel'], f'device type {val:#04x} has no UA_DEV_* in '
                               f'driver/ua_apollo.h')

    # serial table: prefix -> type
    m = re.search(r'ua_serial_table\[\]\s*=\s*\{(.*?)\};', core, re.S)
    table = {}
    if not m:
        rep.err('driver/ua_core.c', 'ua_serial_table[] not found')
    else:
        for pm in re.finditer(r'\{\s*"(\d{4})"\s*,\s*(UA_DEV_\w+)\s*\}',
                              m.group(1)):
            if pm.group(1) in table:
                rep.err('driver/ua_core.c',
                        f'duplicate serial prefix {pm.group(1)}')
            if pm.group(2) not in defs:
                rep.err('driver/ua_core.c', f'{pm.group(2)} undefined')
                continue
            table[pm.group(1)] = defs[pm.group(2)][0]
    json_prefix = {}
    for d in devs.values():
        for p in d.get('serial_prefixes') or []:
            json_prefix[p] = (d, [hexint(t) for t in d['device_types']])
    for p, (d, types) in json_prefix.items():
        if p not in table:
            rep.err(d['_rel'], f'serial prefix {p} is not in ua_serial_table')
        elif table[p] not in types:
            rep.err(d['_rel'], f'serial prefix {p} maps to {table[p]:#04x} in '
                               f'ua_serial_table, device_types are {types}')
    for p, val in table.items():
        if p not in json_prefix:
            d = by_type.get(val)
            if d is not None:
                rep.err('driver/ua_core.c',
                        f'serial {p} -> {val:#04x} ({d["model"]}) but '
                        f'{d["_rel"]} does not list the prefix')
            else:
                rep.warn('driver/ua_core.c',
                         f'serial {p} -> {val:#04x} has no model data (a '
                         f'HEXA type the driver leaves unidentified)')

    # subsystem pins
    subsys = {m.group(1): int(m.group(2), 16) for m in re.finditer(
        r'#define\s+(UA_SUBSYS_\w+)\s+(0x[0-9A-Fa-f]+)', hdr)}
    fn = c_function(core, 'ua_detect_capabilities')
    pins = {}
    for cm in re.finditer(r'case\s+(UA_SUBSYS_\w+)\s*:(.*?)(?=\bcase\b|'
                          r'\bdefault\b)', fn, re.S):
        tm = re.search(r'device_type\s*=\s*(UA_DEV_\w+)', cm.group(2))
        if tm:
            pins[cm.group(1)] = tm.group(1)
    for fm in re.finditer(r'subsystem_id\s*==\s*(UA_SUBSYS_\w+)\)\s*\{(.*?)\}',
                          fn, re.S):
        tm = re.search(r'device_type\s*=\s*(UA_DEV_\w+)', fm.group(2))
        if tm:
            pins[fm.group(1)] = tm.group(1)
    if not pins:
        rep.err('driver/ua_core.c', 'no subsystem -> device type pins found '
                                    'in ua_detect_capabilities()')
    for sname, tname in pins.items():
        if sname not in subsys or tname not in defs:
            rep.err('driver/ua_core.c', f'{sname} / {tname} undefined')
            continue
        d = by_type.get(defs[tname][0])
        if d is None:
            rep.err('driver/ua_core.c', f'{sname} pins {tname}, which has '
                                        f'no devices/*.json')
        elif 'subsystem_id' not in d:
            rep.err(d['_rel'], f'driver pins subsystem {subsys[sname]:#06x} '
                               f'({sname}) to this model; add subsystem_id')
        elif hexint(d['subsystem_id']) != subsys[sname]:
            rep.err(d['_rel'], f'subsystem_id {d["subsystem_id"]} but the '
                               f'driver pins {subsys[sname]:#06x} ({sname})')
    pinned = {defs[t][0] for t in pins.values() if t in defs}
    for d in devs.values():
        if 'subsystem_id' in d and not (
                {hexint(t) for t in d['device_types']} & pinned):
            rep.err(d['_rel'], f'subsystem_id {d["subsystem_id"]} is not '
                               f'used by the driver')

    # UA_MAX_PREAMP_CH
    mm = re.search(r'#define\s+UA_MAX_PREAMP_CH\s+(\d+)', hdr)
    top = max((d.get('preamps', 0) for d in devs.values()), default=0)
    if not mm or int(mm.group(1)) < top:
        rep.err('driver/ua_apollo.h',
                f'UA_MAX_PREAMP_CH {mm.group(1) if mm else "?"} < most '
                f'preamps in devices/ ({top})')

    # frozen legacy DMA table
    m = re.search(r'ua_legacy_dma\[\]\s*=\s*\{(.*?)\n\};', audio, re.S)
    if not m:
        rep.err('driver/ua_audio.c', 'ua_legacy_dma[] not found')
        return
    for rm in re.finditer(r'\{\s*(UA_DEV_\w+)\s*,\s*(\d+)\s*,\s*(\d+)\s*,'
                          r'\s*(\d+)\s*,\s*(\d+)\s*\}', m.group(1)):
        name = rm.group(1)
        if name not in defs:
            rep.err('driver/ua_audio.c', f'ua_legacy_dma: {name} undefined')
            continue
        val = defs[name][0]
        play, recn, pre, hiz = (int(rm.group(i)) for i in (2, 3, 4, 5))
        d = by_type.get(val)
        if d is None:
            continue
        diffs = []
        if pre != d.get('preamps', 0):
            diffs.append(f'preamps {pre} vs JSON {d.get("preamps", 0)}')
        if hiz != d.get('hiz', 0):
            diffs.append(f'hiz {hiz} vs JSON {d.get("hiz", 0)}')
        if val in LEGACY_VERIFIED:
            lists = route_lists(d, Report())
            mode = d.get('legacy_digital_mode', 'spdif')
            if play != len(lists[('LoMid', mode, 'playback')]):
                diffs.append(f'play {play} vs routes '
                             f'{len(lists[("LoMid", mode, "playback")])}')
            if recn != len(lists[('LoMid', mode, 'record')]):
                diffs.append(f'rec {recn} vs routes '
                             f'{len(lists[("LoMid", mode, "record")])}')
        for df in diffs:
            msg = f'ua_legacy_dma {name} ({d["model"]}): {df}'
            if val in LEGACY_VERIFIED:
                rep.err('driver/ua_audio.c', msg)
            else:
                rep.warn('driver/ua_audio.c', msg + ' (frozen io_map=0 row)')


# ------------------------------------------------------------ reference data

def check_reference(devs, ref, rep):
    caps_p = os.path.join(ref, 'caps.json')
    idx_p = os.path.join(ref, 'generated', 'models_index.json')
    try:
        with open(caps_p, encoding='utf-8') as f:
            caps = json.load(f)
        with open(idx_p, encoding='utf-8') as f:
            index = json.load(f)
    except (OSError, ValueError) as e:
        rep.warn(ref, f'reference data not usable: {e}')
        return False
    where = 'model-data'
    idx = {row[0]: row for row in index}
    for d in devs.values():
        rel = d['_rel']
        c = caps.get('MixerType' + d['mixer_class'])
        if c is None:
            rep.err(rel, f'mixer class {d["mixer_class"]} not in caps.json')
            continue
        want = {'preamps': c['PreampCount'][0],
                'talkback': bool(c['SupportsTalkback'][0]),
                'desktop': bool(c['IsDesktop'][0]),
                'digital_io': ('madi' if d['mixer_class'] == 'M64' else
                               'aes' if c['SPDIFIsAESEBU'][0] else 'spdif')}
        for k, v in want.items():
            if d.get(k) != v:
                rep.err(rel, f'{k} = {d.get(k)!r}, {where}/caps.json says '
                             f'{v!r}')
        if c['DefaultCueBusCount'][0] != d['cue_buses']['default']:
            rep.err(rel, f'cue default {d["cue_buses"]["default"]}, '
                         f'{where}/caps.json says {c["DefaultCueBusCount"][0]}')
        row = idx.get(d['model'])
        if row is None:
            rep.err(rel, f'{d["model"]!r} not in models_index.json')
            continue
        if row[1] != d['mixer_class'] or row[2] != d['mixer_type_id']:
            rep.err(rel, f'mixer class/type {d["mixer_class"]}/'
                         f'{d["mixer_type_id"]}, {where} says {row[1]}/{row[2]}')
        if (row[5], expected_hiz(d, row[6])) != (d['preamps'], d['hiz']):
            rep.err(rel, f'preamps/hiz {d["preamps"]}/{d["hiz"]}, {where} '
                         f'says {row[5]}/{row[6]}')
        lists = route_lists(d, Report())
        if len(lists) == 12:
            got = [f'{len(lists[("LoMid", "spdif", "record")])}/'
                   f'{len(lists[("LoMid", "spdif", "playback")])}',
                   f'{len(lists[("LoMid", "adat", "record")])}/'
                   f'{len(lists[("LoMid", "adat", "playback")])}',
                   f'{len(lists[("Hi", "spdif", "record")])}/'
                   f'{len(lists[("Hi", "spdif", "playback")])}']
            if got != list(row[9:12]):
                rep.warn(rel, f'route counts (LoMid S/PDIF, LoMid ADAT, Hi '
                              f'S/PDIF) {got}, {where} index says '
                              f'{list(row[9:12])}')
    return True


def main():
    ap = argparse.ArgumentParser(description=__doc__.split('\n')[0])
    ap.add_argument('-q', '--quiet', action='store_true',
                    help='do not print warnings')
    ap.add_argument('--reference', metavar='DIR',
                    help='apollo-linux/reference/model-data to check against '
                         '(default: the sibling checkout, when present)')
    args = ap.parse_args()

    rep = Report()
    devs = load_devices(rep)
    type_io = {}
    for d in devs.values():
        check_device(d, rep, type_io)
    check_cross(devs, rep)
    check_generated_header(rep)
    check_hand_tables(devs, rep)
    ref = args.reference or (DEFAULT_REF if os.path.isdir(DEFAULT_REF)
                             else None)
    used_ref = check_reference(devs, ref, rep) if ref else False

    if not args.quiet:
        for w in rep.warnings:
            print(f'warning: {w}')
    for e in rep.errors:
        print(f'error: {e}', file=sys.stderr)
    print(f'check-models: {len(devs)} models, {len(rep.errors)} errors, '
          f'{len(rep.warnings)} warnings'
          f'{"" if used_ref else " (no reference data)"}')
    return 1 if rep.errors else 0


if __name__ == '__main__':
    sys.exit(main())
