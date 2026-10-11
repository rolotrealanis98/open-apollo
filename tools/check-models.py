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
     rules against the routes; the shape of the per-rate converter latency
     tables ("latency"), the monitor taper, the engine feature flags and
     the per-rate channel counts ("rate_channels") against the routes.
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


# Mixer classes that never have the monitor's digital mirror (parity map 16
# F13): the Twin family, the Conrad family, and the M64 (no outputs).
NO_MIRROR_CLASSES = frozenset((
    'Armstrong', 'Stretch', 'Lavern', 'LavernG2', 'Shirley',
    'Conrad', 'ApolloSolo', 'Cauldron', 'M64'))


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
    # public_types: the kext's UAD2DeviceType values that are this model
    # (the Mixer Engine's CardType classes).  A device type of 0x80 or more
    # is 0x80 + one of them (driver ua_resolve_public_model()).
    pts = []
    for t in d.get('public_types') or []:
        try:
            v = hexint(t)
        except ValueError:
            rep.err(rel, f'bad public type {t!r}')
            continue
        if not 0 < v <= 0x29:
            rep.err(rel, f'public type {t} out of range (1-0x29)')
        pts.append(v)
    if len(set(pts)) != len(pts):
        rep.err(rel, 'duplicate public type')
    for t in dts:
        v = hexint(t)
        if v >= 0x80:
            if v - 0x80 not in pts:
                rep.err(rel, f'device type {t} is not 0x80 + one of its '
                             f'public_types')
        elif not 0 < v <= 0x3F + 0x10:
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
    # --- digital mirror and output pad (gen-device-map)
    for key in ('mirror_to_digital', 'output_pad'):
        if key not in d:
            rep.err(rel, f'missing field {key!r}')
    mirror = d.get('mirror_to_digital')
    if mirror is not None and not isinstance(mirror, bool):
        rep.err(rel, f'mirror_to_digital {mirror!r}: must be a boolean')
    elif mirror is True and d.get('mixer_class') in NO_MIRROR_CLASSES:
        rep.err(rel, f'mirror_to_digital on {d.get("mixer_class")}, which '
                     f'has no digital mirror (parity map 16 F13)')
    elif mirror is False and d.get('mixer_class') not in NO_MIRROR_CLASSES:
        rep.warn(rel, f'mirror_to_digital false on {d.get("mixer_class")}: '
                      f'the engine offers the mirror on this class')
    pad = d.get('output_pad')
    if pad is not None:
        if (not isinstance(pad, dict) or set(pad) != {'line', 'monitor'}
                or not all(isinstance(v, bool) for v in pad.values())):
            rep.err(rel, f'output_pad {pad!r}: must be {{"line": bool, '
                         f'"monitor": bool}}')
    vmax = rules.get('virtual_max')
    virt = [c & 0xff for c in rec if c >> 8 == gen.TYPE_VIRTUAL]
    if vmax and virt and max(virt) <= vmax:
        rep.warn(rel, f'virtual_max {vmax} hides nothing '
                      f'(highest virtual channel {max(virt)})')
    check_latency(d, rep)
    check_monitor_taper(d, rep)
    check_engine_features(d, rep)
    check_rate_channels(d, rep)
    return lists


# The six sample rates every per-rate table in devices/*.json is indexed by.
RATE_LIST = [44100, 48000, 88200, 96000, 176400, 192000]
LATENCY_OUTPUTS = ('line', 'cue', 'monitor', 'dsp')


def check_rate_table(rel, what, t, rep):
    """{"default": [6 frames], "boards": {"1,2": [6 frames]}}"""
    if not isinstance(t, dict) or 'default' not in t or \
            set(t) - {'default', 'boards'}:
        rep.err(rel, f'{what}: must be {{"default": [...], "boards": ...}}')
        return
    rows = [('default', t['default'])] + list((t.get('boards') or {}).items())
    seen = set()
    for key, row in rows:
        if not (isinstance(row, list) and len(row) == len(RATE_LIST) and
                all(isinstance(v, (int, float)) and not isinstance(v, bool)
                    and 0 < v < 1000 for v in row)):
            rep.err(rel, f'{what}[{key}]: need {len(RATE_LIST)} positive '
                         f'frame counts, one per rate')
        if key == 'default':
            continue
        try:
            ids = [int(x) for x in key.split(',')]
        except ValueError:
            ids = [-1]
        for i in ids:
            if not 1 <= i <= 7 or i in seen:
                rep.err(rel, f'{what}: bad or repeated analog board id in '
                             f'{key!r} (ids 1-7, board 0 is "default")')
            seen.add(i)
        if row == t['default']:
            rep.warn(rel, f'{what}[{key}] repeats the default row')


def check_latency(d, rep):
    """Converter latencies per rate and analog board (Mixer Engine
    GetInputDelay / GetOutputDelay / GetOutputDelayForDelayComp, A10)."""
    rel = d['_rel']
    lat = d.get('latency')
    if lat is None:
        rep.err(rel, 'missing field \'latency\'')
        return
    if lat.get('rates') != RATE_LIST:
        rep.err(rel, f'latency.rates must be {RATE_LIST}')
    check_rate_table(rel, 'latency.input', lat.get('input'), rep)
    outs = lat.get('output')
    if not isinstance(outs, dict) or set(outs) != set(LATENCY_OUTPUTS):
        rep.err(rel, f'latency.output needs exactly {LATENCY_OUTPUTS}')
    else:
        for k in LATENCY_OUTPUTS:
            check_rate_table(rel, f'latency.output.{k}', outs[k], rep)
    check_rate_table(rel, 'latency.output_delay_comp',
                     lat.get('output_delay_comp'), rep)
    for k in ('dsp_to_fpga', 'fpga_to_dsp'):
        v = lat.get(k)
        if not isinstance(v, (int, float)) or isinstance(v, bool) or v <= 0:
            rep.err(rel, f'latency.{k} must be a positive frame count')


def check_monitor_taper(d, rep):
    """Control-room level taper: 48 or 54 steps (GetCRPGATaperTodBTable),
    steps + 1 dB values from -96 up to 0 dB (N28)."""
    rel = d['_rel']
    t = d.get('monitor_taper')
    if t is None:
        rep.err(rel, 'missing field \'monitor_taper\'')
        return
    steps, db = t.get('steps'), t.get('db')
    if steps not in (48, 54):
        rep.err(rel, f'monitor_taper.steps {steps!r}: the engine has 48 and '
                     f'54-step tapers only')
        return
    if not isinstance(db, list) or len(db) != steps + 1:
        rep.err(rel, f'monitor_taper.db needs {steps + 1} values')
        return
    if db[0] != -96 or db[-1] != 0 or any(b < a for a, b in zip(db, db[1:])):
        rep.err(rel, 'monitor_taper.db must rise from -96 to 0 dB')
    if d.get('desktop') and steps != 54:
        rep.warn(rel, 'desktop model with a 48-step taper (every desktop '
                      'class seen uses 54)')


ES9028_OUTPUTS = ('headphone', 'cue', 'monitor', 'monitor_r_surround', 'all')
FUNC_SWITCH_MODES = ('none', 'alt 2', 'mono', 'dim', 'talkback', 'alt 1')


def check_engine_features(d, rep):
    """Engine feature flags from the MixerType* bodies (N30, N18)."""
    rel = d['_rel']
    keys = ('line_ref_level_inputs', 'line_pga_disable', 'es9028_outputs',
            'func_switch', 'front_panel_switches', 'direct_outputs',
            'volt876_adat_preamps', 'surround_max_mode')
    for k in keys:
        if k not in d:
            rep.err(rel, f'missing field {k!r}')
    if any(k not in d for k in keys):
        return
    analog = {e['synch_in'] for e in d.get('mixer_inputs') or []
              if e['name'].startswith('ANALOG')}
    ref = d['line_ref_level_inputs']
    if not isinstance(ref, list) or not set(ref) <= analog:
        rep.err(rel, f'line_ref_level_inputs {ref!r}: must be synch-ins of '
                     f'ANALOG mixer inputs')
    pga = d['line_pga_disable']
    if not isinstance(pga, bool) or (pga and not d.get('preamps')):
        rep.err(rel, 'line_pga_disable must be a boolean, false without '
                     'preamps')
    es = d['es9028_outputs']
    if not isinstance(es, list) or not set(es) <= set(ES9028_OUTPUTS) or \
            ('all' in es and len(es) > 1):
        rep.err(rel, f'es9028_outputs {es!r}: use {ES9028_OUTPUTS}')
    fs = d['func_switch']
    if fs is not None:
        modes = fs.get('modes') if isinstance(fs, dict) else None
        if not isinstance(modes, list) or 'none' not in modes or \
                not set(modes) <= set(FUNC_SWITCH_MODES) or \
                not isinstance(fs.get('second'), bool):
            rep.err(rel, f'func_switch {fs!r}: {{"modes": [...], '
                         f'"second": bool}} with modes from '
                         f'{FUNC_SWITCH_MODES}')
    fp = d['front_panel_switches']
    if not isinstance(fp, bool) or (fp and not d.get('desktop')):
        rep.err(rel, 'front_panel_switches must be a boolean, true only on '
                     'desktop models')
    if d['direct_outputs'] not in (0, 8, 16):
        rep.err(rel, f'direct_outputs {d["direct_outputs"]!r}: 0, 8 or 16')
    volt = d['volt876_adat_preamps']
    if volt not in (False, 'hidden'):
        rep.err(rel, 'volt876_adat_preamps must be false or "hidden"')
    rec = d['routes']['LoMid'].get('record', [])
    if volt and not any(int(e['ch_id'], 16) >> 8 == gen.TYPE_ADAT
                        for e in rec):
        rep.err(rel, 'volt876_adat_preamps without ADAT inputs')
    sm = d['surround_max_mode']
    if sm is not None and (not isinstance(sm, int) or not 1 <= sm <= 10):
        rep.err(rel, f'surround_max_mode {sm!r}: null or 1-10')


RATE_CHANNEL_TYPES = {'adat': gen.TYPE_ADAT, 'spdif': gen.TYPE_SPDIF,
                      'aes': 0x06, 'virtual': gen.TYPE_VIRTUAL, 'madi': 0x08,
                      'dante': 0x0D}


def check_rate_channels(d, rep):
    """Digital/virtual channel counts per rate (ChannelCount, N29): one
    count per rate for each channel type the routes carry, never above the
    route count, falling or flat as the rate rises."""
    rel = d['_rel']
    rc = d.get('rate_channels')
    if rc is None:
        rep.err(rel, 'missing field \'rate_channels\'')
        return
    if rc.get('rates') != RATE_LIST:
        rep.err(rel, f'rate_channels.rates must be {RATE_LIST}')
    rules = d.get('route_rules') or {}
    for dk in DIRS:
        row = rc.get(dk)
        if not isinstance(row, dict):
            rep.err(rel, f'rate_channels.{dk} missing')
            continue
        routes = d['routes']['LoMid'].get(dk, [])
        for k, t in RATE_CHANNEL_TYPES.items():
            n = sum(1 for e in routes if int(e['ch_id'], 16) >> 8 == t)
            if t == gen.TYPE_VIRTUAL and rules.get('virtual_max'):
                n = min(n, rules['virtual_max'])
            v = row.get(k)
            if v is None:
                if n:
                    rep.err(rel, f'rate_channels.{dk} lacks {k} ({n} routes)')
                continue
            if not n:
                rep.err(rel, f'rate_channels.{dk}.{k} without {k} routes')
            if not (isinstance(v, list) and len(v) == len(RATE_LIST) and
                    all(isinstance(x, int) and 0 <= x <= n for x in v)):
                rep.err(rel, f'rate_channels.{dk}.{k} {v!r}: one count per '
                             f'rate, 0-{n}')
                continue
            if v[0] != n:
                rep.err(rel, f'rate_channels.{dk}.{k} at 44.1 kHz is {v[0]}, '
                             f'the routes have {n}')
            if any(b > a for a, b in zip(v, v[1:])):
                rep.err(rel, f'rate_channels.{dk}.{k} {v}: rises with the '
                             f'rate')
            mid = rules.get('adat_mid_rate_max')
            if k == 'adat' and dk == 'record' and mid and v[2] != mid:
                rep.err(rel, f'rate_channels.record.adat at 88.2 kHz is '
                             f'{v[2]}, adat_mid_rate_max {mid}')


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
        for t in d.get('public_types') or []:
            try:
                v = hexint(t)
            except ValueError:
                continue
            if v in seen:
                rep.err(d['_rel'], f'public type {t} also in {seen[v]}')
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
    try:
        with open(os.path.join(ref, 'templates_summary.json'),
                  encoding='utf-8') as f:
            templates = json.load(f)
    except (OSError, ValueError):
        templates = None
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
        tpl = templates.get(d['mixer_class'].lower()) if templates else None
        pad = d.get('output_pad')
        if tpl and isinstance(pad, dict):
            has_pad = any('kPropPad' in o.get('_props', [])
                          for o in tpl.get('outputs', []))
            if pad.get('line') != has_pad:
                rep.err(rel, f'output_pad.line {pad.get("line")}, {where}/'
                             f'templates_summary.json says the line outputs '
                             f'{"have" if has_pad else "have no"} a Pad')
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
