#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Generate driver/ua_models_gen.h from devices/*.json.

devices/<model>.json is open-apollo's own description of each Apollo model
(identity, capabilities and the Mixer Engine route tables as channel-ID
lists).  This tool turns every model that has a driver device type into C
tables the kernel module can use:

  * per direction (record/playback) x rate band (LoMid < 176.4 kHz, Hi) x
    digital mode (S/PDIF, ADAT): the channel IDs the macOS Mixer Engine
    exposes, in route order, with the static route-visibility rules applied
    (TOSLink mode filter, highest VIRTUAL channel);
  * preamp/HiZ counts, capability flags, the digital mode that labels the
    legacy (io_map=0) layout, the runtime ADAT rate rule, which TOSLink
    mode picks each direction's list (route_rules.toslink) and the Mixer
    Engine's default TOSLink input type (route_rules.default_input).

The legacy DMA channel counts are not generated: they are frozen per
device type in driver/ua_audio.c (ua_legacy_dma[]) so io_map=0 keeps the
frame size every existing install runs with.

Usage:
  tools/gen-driver-models.py            write driver/ua_models_gen.h
  tools/gen-driver-models.py --check    exit 1 if the header is out of date
  tools/gen-driver-models.py --stdout   print the header

Standard library only.  Output is deterministic (models sorted by slug).
"""
import argparse
import glob
import json
import os
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DEVICES = os.path.join(ROOT, 'devices')
OUT = os.path.join(ROOT, 'driver', 'ua_models_gen.h')

MAX_CHANNELS = 64          # UA_MAX_CHANNELS
BANDS = ('LoMid', 'Hi')
MODES = ('spdif', 'adat')
DIRS = (('record', 'REC'), ('playback', 'PLAY'))
# route_rules.toslink -> UA_TOSLINK_* (which TOSLink mode picks a list)
TOSLINK_RULES = {'none': 'NONE', 'per-direction': 'PER_DIR',
                 'input-mode': 'INPUT'}
TYPE_ADAT = 0x04
TYPE_SPDIF = 0x05
TYPE_VIRTUAL = 0x07


class GenError(Exception):
    pass


def parse_hex(val, what, path, bits=16):
    try:
        v = int(str(val), 16)
    except ValueError:
        raise GenError(f'{path}: bad {what} {val!r}')
    if not 0 <= v < (1 << bits):
        raise GenError(f'{path}: {what} {val!r} out of range')
    return v


def shown(ids, mode, rules):
    """Static IsChannelShownForIORoute rules (see devices/*.json route_rules)."""
    out = []
    toslink = rules.get('toslink', 'none')
    vmax = rules.get('virtual_max')
    for cid in ids:
        t, n = cid >> 8, cid & 0xff
        if toslink in ('per-direction', 'input-mode'):
            if mode == 'spdif' and t == TYPE_ADAT:
                continue
            if mode == 'adat' and t == TYPE_SPDIF:
                continue
        elif toslink != 'none':
            raise GenError(f'unknown toslink rule {toslink!r}')
        if vmax and t == TYPE_VIRTUAL and n > vmax:
            continue
        out.append(cid)
    return out


def load_models():
    models = []
    seen_types = {}
    for path in sorted(glob.glob(os.path.join(DEVICES, '*.json'))):
        with open(path, encoding='utf-8') as f:
            d = json.load(f)
        types = d.get('device_types') or []
        if not types or 'routes' not in d:
            continue
        slug = os.path.splitext(os.path.basename(path))[0]
        rel = os.path.relpath(path, ROOT)
        dtypes = [parse_hex(t, 'device type', rel, 8) for t in types]
        for t in dtypes:
            if t in seen_types:
                raise GenError(f'{rel}: device type 0x{t:02X} also in '
                               f'{seen_types[t]}')
            seen_types[t] = rel
        names = {parse_hex(k, 'device type', rel, 8): v
                 for k, v in (d.get('device_type_names') or {}).items()}
        rules = d.get('route_rules') or {}
        routes = {}
        for band in BANDS:
            b = d['routes'].get(band)
            if b is None:
                raise GenError(f'{rel}: no {band} routes')
            for dkey, _ in DIRS:
                ids = []
                for e in b.get(dkey, []):
                    cid = parse_hex(e['ch_id'], 'ch_id', rel)
                    if cid == 0 or (cid & 0xff) == 0:
                        raise GenError(f'{rel}: bad ch_id {e["ch_id"]}')
                    if cid in ids:
                        raise GenError(f'{rel}: duplicate ch_id {e["ch_id"]}')
                    ids.append(cid)
                for mode in MODES:
                    lst = shown(ids, mode, rules)
                    if len(lst) > MAX_CHANNELS:
                        raise GenError(f'{rel}: {band}/{mode}/{dkey} has '
                                       f'{len(lst)} channels (> {MAX_CHANNELS})')
                    routes[(band, mode, dkey)] = lst
        legacy_mode = d.get('legacy_digital_mode', 'spdif')
        if legacy_mode not in MODES:
            raise GenError(f'{rel}: bad legacy_digital_mode {legacy_mode!r}')
        toslink = rules.get('toslink', 'none')
        if toslink not in TOSLINK_RULES:
            raise GenError(f'{rel}: bad route_rules.toslink {toslink!r}')
        default_in = rules.get('default_input', 'spdif')
        if default_in not in MODES:
            raise GenError(f'{rel}: bad route_rules.default_input '
                           f'{default_in!r}')
        preamps = int(d.get('preamps') or 0)
        hiz = int(d.get('hiz') or 0)
        if preamps > 8 or hiz > preamps:
            raise GenError(f'{rel}: preamps {preamps} / hiz {hiz} out of range')
        flags = []
        if d.get('talkback'):
            flags.append('UA_MODEL_F_TALKBACK')
        if d.get('digital_io') == 'aes':
            flags.append('UA_MODEL_F_AES')
        if d.get('desktop'):
            flags.append('UA_MODEL_F_DESKTOP')
        outputs = d.get('mixer_outputs') or []
        if 'HP 1' in outputs:
            flags.append('UA_MODEL_F_HP1')
        if 'HP 2' in outputs:
            flags.append('UA_MODEL_F_HP2')
        adc, adc_boards = adc_latency(d.get('latency'), rel)
        models.append({
            'slug': slug, 'path': rel, 'name': d['model'], 'types': dtypes,
            'adc': adc, 'adc_boards': adc_boards,
            'type_names': names, 'preamps': preamps, 'hiz': hiz,
            'flags': flags, 'routes': routes, 'legacy_mode': legacy_mode,
            'toslink': toslink, 'default_in': default_in,
            'adat_mid': int(rules.get('adat_mid_rate_max') or 0),
            'status': d.get('status', 'unverified'),
        })
    if not models:
        raise GenError('no models with device_types found in devices/')
    return models


LATENCY_RATES = [44100, 48000, 88200, 96000, 176400, 192000]


def adc_latency(lat, rel):
    """Input (ADC) delay in milli-frames per rate (UA_RATE_44100..192000),
    and {board id: row} where the Mixer Engine's GetInputDelay varies by
    analog board (devices/*.json "latency").  ([], {}) without data."""
    if not lat:
        return [], {}
    if lat.get('rates') != LATENCY_RATES:
        raise GenError(f'{rel}: latency.rates must be {LATENCY_RATES}')
    inp = lat.get('input') or {}

    def row(vals, what):
        if not isinstance(vals, list) or len(vals) != len(LATENCY_RATES):
            raise GenError(f'{rel}: latency.input.{what} needs 6 values')
        out = [int(round(float(v) * 1000)) for v in vals]
        if any(v < 0 or v > 0xFFFF for v in out):
            raise GenError(f'{rel}: latency.input.{what} out of range')
        return out

    default = row(inp.get('default'), 'default') if 'default' in inp else []
    boards = {}
    for k, vals in (inp.get('boards') or {}).items():
        # A key is one board id or a comma list ("1,2,3,4,5,6,7")
        for part in str(k).split(','):
            bid = int(part)
            if not 0 <= bid < 8:
                raise GenError(f'{rel}: latency board id {k} out of range')
            boards[bid] = row(vals, f'boards.{k}')
    return default, boards


def cident(s):
    return ''.join(c if c.isalnum() else '_' for c in s.lower())


def cstr(s):
    return '"' + s.replace('\\', '\\\\').replace('"', '\\"') + '"'


def generate(models):
    lines = []
    w = lines.append
    w('/* SPDX-License-Identifier: GPL-2.0-only */')
    w('/*')
    w(' * GENERATED by tools/gen-driver-models.py from devices/<model>.json.')
    w(' * DO NOT EDIT -- edit the device files and re-run the tool.')
    w(' *')
    w(' * Per-model UA channel-ID lists in ALSA channel order, indexed')
    w(' * [rate band][digital mode][direction] (see struct ua_model_desc in')
    w(' * ua_apollo.h).  Static route-visibility rules are already applied;')
    w(' * adat_mid_rate_max is applied at runtime.  Included by ua_audio.c only.')
    w(' */')
    w('')
    w('#ifndef UA_MODELS_GEN_H')
    w('#define UA_MODELS_GEN_H')
    w('')
    w('#define UA_GEN_ROUTE(ids)\t{ ids, ARRAY_SIZE(ids) }')
    w('')

    # Deduplicate identical ID lists across bands, modes and models.  Name
    # each list after the shortest slug that uses it (the Gen 1 model).
    arrays = {}            # tuple(ids) -> C name
    order = []
    for m in sorted(models, key=lambda m: (len(m['slug']), m['slug'])):
        for band in BANDS:
            for mode in MODES:
                for dkey, dshort in DIRS:
                    ids = tuple(m['routes'][(band, mode, dkey)])
                    if not ids or ids in arrays:
                        continue
                    short = cident(m['slug']).removeprefix('apollo_')
                    name = f'ua_gen_{short}_{dshort.lower()}_{mode}'
                    if band != 'LoMid':
                        name += '_' + band.lower()
                    arrays[ids] = name
                    order.append((name, ids, m['name'], dkey, band, mode))

    for name, ids, model, dkey, band, mode in order:
        w(f'/* {model}: {dkey}, {band}, {mode.upper()} mode ({len(ids)}) */')
        w(f'static const u16 {name}[] = {{')
        for i in range(0, len(ids), 8):
            chunk = ', '.join(f'0x{c:04x}' for c in ids[i:i + 8])
            w(f'\t{chunk},')
        w('};')
        w('')

    for m in models:
        mid = cident(m['slug'])
        w(f'/* {m["path"]} ({m["status"]}) */')
        w(f'static const struct ua_model_desc ua_gen_model_{mid} = {{')
        w(f'\t.name = {cstr(m["name"])},')
        w(f'\t.preamps = {m["preamps"]},')
        w(f'\t.hiz = {m["hiz"]},')
        w(f'\t.flags = {" | ".join(m["flags"]) or "0"},')
        w(f'\t.legacy_mode = UA_DIG_{m["legacy_mode"].upper()},')
        w(f'\t.toslink = UA_TOSLINK_{TOSLINK_RULES[m["toslink"]]},')
        w(f'\t.default_in_mode = UA_DIG_{m["default_in"].upper()},')
        w(f'\t.adat_mid_rate_max = {m["adat_mid"]},')
        if m['adc']:
            w('\t.adc_mframes = { 0, ' +
              ', '.join(str(v) for v in m['adc']) + ' },')
        if m['adc_boards']:
            mask = 0
            w('\t.adc_board_mframes = {')
            for b, vals in sorted(m['adc_boards'].items()):
                mask |= 1 << b
                w(f'\t\t[{b}] = {{ 0, ' + ', '.join(str(v) for v in vals)
                  + ' },')
            w('\t},')
            w(f'\t.adc_boards = 0x{mask:02x},')
        w('\t.routes = {')
        for band in BANDS:
            w(f'\t\t[UA_RATE_BAND_{band.upper()}] = {{')
            for mode in MODES:
                w(f'\t\t\t[UA_DIG_{mode.upper()}] = {{')
                for dkey, dshort in DIRS:
                    ids = tuple(m['routes'][(band, mode, dkey)])
                    if ids:
                        w(f'\t\t\t\t[UA_DIR_{dshort}] = '
                          f'UA_GEN_ROUTE({arrays[ids]}),')
                    else:
                        w(f'\t\t\t\t[UA_DIR_{dshort}] = {{ NULL, 0 }},')
                w('\t\t\t},')
            w('\t\t},')
        w('\t},')
        w('};')
        w('')

    w('static const struct ua_model_type ua_gen_model_types[] = {')
    entries = []
    for m in models:
        for t in m['types']:
            entries.append((t, m))
    for t, m in sorted(entries, key=lambda e: e[0]):
        tname = m['type_names'].get(t)
        nm = cstr(tname) if tname else 'NULL'
        w(f'\t{{ 0x{t:02X}, {nm}, &ua_gen_model_{cident(m["slug"])} }},')
    w('};')
    w('')
    w('#undef UA_GEN_ROUTE')
    w('')
    w('#endif /* UA_MODELS_GEN_H */')
    return '\n'.join(lines) + '\n'


def main():
    ap = argparse.ArgumentParser(description=__doc__.split('\n')[0])
    g = ap.add_mutually_exclusive_group()
    g.add_argument('--check', action='store_true',
                   help='fail if driver/ua_models_gen.h is out of date')
    g.add_argument('--stdout', action='store_true',
                   help='print the header instead of writing it')
    args = ap.parse_args()
    try:
        text = generate(load_models())
    except (GenError, KeyError, ValueError) as e:
        print(f'gen-driver-models: {e}', file=sys.stderr)
        return 2
    if args.stdout:
        sys.stdout.write(text)
        return 0
    if args.check:
        try:
            with open(OUT, encoding='utf-8') as f:
                cur = f.read()
        except OSError:
            cur = None
        if cur != text:
            print('gen-driver-models: driver/ua_models_gen.h is out of date',
                  file=sys.stderr)
            return 1
        return 0
    with open(OUT, 'w', encoding='utf-8') as f:
        f.write(text)
    print(f'wrote {os.path.relpath(OUT, ROOT)}')
    return 0


if __name__ == '__main__':
    sys.exit(main())
