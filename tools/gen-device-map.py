#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Generate a mixer-engine device map for a model from devices/<model>.json.

mixer-engine/device_maps/ holds two maps captured from a live UA Mixer Engine
session (the x4 and the x8p).  Every other model has no capture, so the
daemon used to fall back to the x4 map with the wrong channel counts.  This
tool derives a map for any other model:

  * the captured x4 map is the structural template: every control that is
    not model specific (clock, metering, per-device switches) is copied as
    it is;
  * the per-channel tables are rebuilt from devices/<model>.json: the mixer
    inputs and outputs (one template row per role -- mic with/without HiZ,
    line, ADAT, S/PDIF/AES, virtual, talkback, cue, monitor, headphone, ...,
    cloned and renumbered), their sends, the aux buses, the driver route
    lists (/inputroutes, /outputroutes, the ASIO lists) and the route totals;
  * plugin and licensing data is left out entirely (the plugin list, scene
    tables, per-channel insert chains, authorization/demo properties);
  * preamp and HiZ controls exist only where the model has them (the first
    `preamps` analog inputs get a preamp, the first `hiz` of them HiZ), the
    TOSLink switches only where the route rules have them, the talkback
    controls only on models with talkback, the clock source list follows the
    model's digital I/O, the monitor's MirrorsToDigital only where the model
    has "mirror_to_digital", and the output Pad on the line outputs and the
    monitor only where "output_pad" says so.

A generated map is structure from the x4 plus counts and names from the JSON;
nothing in it was observed on the model.  It is marked "generated": true and
is never written over a captured map.

The daemon calls ensure_cached() when no captured map matches the attached
device: the map is generated once into $XDG_CACHE_HOME/open-apollo/device_maps
(default ~/.cache/open-apollo/device_maps), keyed on a hash of the x4
template, the model's JSON and the generator version, so it is regenerated
when any of them changes.  Generated maps are not committed.

Usage:
  tools/gen-device-map.py                 fill the cache for the models that
                                          have a driver device type and no
                                          captured map
  tools/gen-device-map.py --all           ... for every model in devices/
  tools/gen-device-map.py --model SLUG..  only these (apollo-x6, ...)
  tools/gen-device-map.py --out DIR       write plain files to DIR instead
  tools/gen-device-map.py --compare SLUG  generate SLUG and diff its control
                                          paths against its captured map
  tools/gen-device-map.py --list          show model and map status

Standard library only.  Output is deterministic.
"""
import argparse
import copy
import glob
import hashlib
import importlib.util
import json
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DEVICES = os.path.join(ROOT, 'devices')
MAPS = os.path.join(ROOT, 'mixer-engine', 'device_maps')
TEMPLATE_NAME = 'device_map_apollo_x4.json'
# Bump when the generator's output changes without its inputs changing.
GENERATOR_VERSION = 3


def _load_gen():
    path = os.path.join(ROOT, 'tools', 'gen-driver-models.py')
    spec = importlib.util.spec_from_file_location('gen_driver_models', path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


gen = _load_gen()

SECTIONS = ('/devices/0/inputs', '/devices/0/outputs', '/devices/0/auxs',
            '/devices/0/asioinputs', '/devices/0/asiooutputs',
            '/inputroutes', '/outputroutes')
ROW_RE = re.compile(r'^(' + '|'.join(re.escape(s) for s in SECTIONS) +
                    r')/(\d+)(/.*)?$')
SEND_RE = re.compile(r'^/sends/(\d+)(/.*)?$')

# Controls that only exist on models with talkback.
TALKBACK_GLOBALS = ('/TalkbackOn', '/TalkbackInPhysicalCR', '/TalkbackMaster',
                    '/TalkbackMicSelect', '/devices/0/TalkbackMaster')
# Reserved ASIO output channels (the template's LLMDisk tail): the captured
# x4 has 12 after 32 playback routes and the x8p 16 after 36, i.e. the
# playback route count minus 20; models with fewer routes have none.
ASIO_RESERVED_BASE = 20


class MapError(Exception):
    pass


# Per-model /devices/0 capability flags (parity map 16 sec. 3.4, resolved
# from the MixerType* classes; checked against the two captured trees, which
# agree on every flag they carry).  The captured x4 values used to be copied
# into every generated map.
#   out_dsp    SupportsSpeakerCorrection and SupportsBassManagement
#              (the engine gates both on SupportsOutputDSP)
#   hp_corr    SupportsHeadphoneCorrection
#   hp2_corr   SupportsHeadphone2Correction.  Unexplained in the engine (the
#              x4 and x8p classes share one SupportsHPCorrection body); only
#              the captured x8p says True, so True is assumed for the
#              Rene2/4/8 family (HEXA) and False elsewhere
#   autogain   SupportsAutogain (Gen 2 only)
#   dante      SupportsDante
#   alt_mons   MaxAltMons (x4 and Twin X drop by one while bass management
#              is on; these are the bass-management-off values)
#   spans      (MaxSpans, DefaultSpans)
#   spanning   DSPSpanning (Apollo/A16: false in FireWire mode only)
#   llm        AvailableLLMDiskChannels
#   madi_first FirstMADIChannelBlockIndex
CAPS_FIELDS = ('out_dsp', 'hp_corr', 'hp2_corr', 'autogain', 'dante',
               'alt_mons', 'spans', 'spanning', 'llm', 'madi_first')


def _caps(out_dsp, hp_corr, autogain, dante, alt_mons, spans, spanning, llm,
          madi_first=0, hp2_corr=False):
    return dict(zip(CAPS_FIELDS, (out_dsp, hp_corr, hp2_corr, autogain, dante,
                                  alt_mons, spans, spanning, llm, madi_first)))


CLASS_CAPS = {
    # original Apollo line
    'Apollo':      _caps(False, False, False, False, 2, (4, 4), True, 10),
    'ApolloBlack': _caps(False, False, False, False, 2, (4, 4), True, 16),
    'Peyton':      _caps(False, False, False, False, 2, (4, 4), True, 16),
    'A16':         _caps(False, False, False, False, 2, (4, 4), True, 16),
    'A16Black':    _caps(False, False, False, False, 2, (4, 4), True, 16),
    # desktop: Twin family (2 spans) and the Conrad family (no spanning)
    'Armstrong':   _caps(False, False, False, False, 1, (2, 2), True, 10),
    'Stretch':     _caps(False, False, False, False, 1, (2, 2), True, 10),
    'Lavern':      _caps(True, True, False, False, 1, (2, 2), True, 10),
    'Shirley':     _caps(True, True, False, False, 1, (2, 2), True, 10),
    'LavernG2':    _caps(True, True, True, False, 1, (2, 2), True, 10),
    'Conrad':      _caps(False, False, False, False, 0, (4, 4), False, 6),
    'ApolloSolo':  _caps(False, False, False, False, 0, (4, 4), False, 6),
    'Cauldron':    _caps(False, False, False, False, 0, (4, 4), False, 6),
    # x4
    'Freud':       _caps(True, True, False, False, 2, (4, 4), True, 16),
    'FreudG2':     _caps(True, True, True, False, 2, (4, 4), True, 16),
    # x6 / x8 / x8p (+ Gen 2)
    'Rene2':       _caps(True, True, False, False, 2, (4, 4), True, 16,
                         hp2_corr=True),
    'Rene4':       _caps(True, True, False, False, 2, (4, 4), True, 16,
                         hp2_corr=True),
    'Rene8':       _caps(True, True, False, False, 2, (4, 4), True, 16,
                         hp2_corr=True),
    'Rene2G2':     _caps(True, True, True, False, 2, (4, 4), True, 16,
                         hp2_corr=True),
    'Rene4G2':     _caps(True, True, True, False, 2, (4, 4), True, 16,
                         hp2_corr=True),
    'Rene8G2':     _caps(True, True, True, False, 2, (4, 4), True, 16,
                         hp2_corr=True),
    # x16 / x16D: output DSP but no headphone correction
    'Rene16':      _caps(True, False, False, False, 2, (4, 4), True, 16),
    'Rene16G2':    _caps(True, False, False, False, 2, (4, 4), True, 16),
    'ReneLax':     _caps(True, False, False, True, 2, (4, 4), True, 16),
    # Realtime Rack (MADI): no DSP spans, MADI channel blocks start at 1
    'M64':         _caps(False, False, False, False, 0, (0, 0), True, 16,
                         madi_first=1),
}


def device_caps(d):
    """Capability flags for a model: {map path: value} (see CLASS_CAPS)."""
    cls = d.get('mixer_class')
    if cls not in CLASS_CAPS:
        raise MapError(f'{d["_slug"]}: no capability row for mixer class '
                       f'{cls!r}')
    c = CLASS_CAPS[cls]
    return {
        '/devices/0/SupportsSpeakerCorrection': c['out_dsp'],
        '/devices/0/SupportsBassManagement': c['out_dsp'],
        '/devices/0/SupportsHeadphoneCorrection': c['hp_corr'],
        '/devices/0/SupportsHeadphone2Correction': c['hp2_corr'],
        '/devices/0/SupportsAutogain': c['autogain'],
        '/devices/0/SupportsDante': c['dante'],
        '/devices/0/MaxAltMons': c['alt_mons'],
        '/devices/0/MaxSpans': c['spans'][0],
        '/devices/0/DefaultSpans': c['spans'][1],
        '/devices/0/DSPSpanning': c['spanning'],
        '/devices/0/AvailableLLMDiskChannels': c['llm'],
        '/FirstMADIChannelBlockIndex': c['madi_first'],
    }


# Plugin and licensing data is never carried into a generated map: the plugin
# list and scene tables, the per-channel insert (effects) chains, and any
# authorization/licence/demo property.  The daemon does not use them for
# device control.
PLUGIN_SEGMENTS = frozenset(('plugins', 'pluginscenes', 'effects'))
LICENSE_LEAF = re.compile(r'auth|licen|demo|vstid|pluginuuid|plugin', re.I)


def is_plugin_data(path):
    parts = path.strip('/').split('/')
    return (any(p in PLUGIN_SEGMENTS for p in parts) or
            bool(LICENSE_LEAF.search(parts[-1])))


def hexint(v):
    return int(str(v), 16)


def slug_to_file(slug):
    return 'device_map_' + slug.replace('-', '_') + '.json'


# ------------------------------------------------------------------ loading

def load_devices():
    out = {}
    for path in sorted(glob.glob(os.path.join(DEVICES, '*.json'))):
        with open(path, encoding='utf-8') as f:
            d = json.load(f)
        d['_slug'] = os.path.splitext(os.path.basename(path))[0]
        out[d['_slug']] = d
    return out


def load_map(path):
    with open(path, encoding='utf-8') as f:
        return json.load(f)


def captured_slugs():
    """Slugs whose map file exists and is not generated."""
    out = set()
    for path in glob.glob(os.path.join(MAPS, 'device_map_*.json')):
        try:
            with open(path, encoding='utf-8') as f:
                head = f.read(4096)
        except OSError:
            continue
        if '"generated": true' in head or '"generated":true' in head:
            continue
        name = os.path.basename(path)[len('device_map_'):-len('.json')]
        out.add(name.replace('_', '-'))
    return out


# --------------------------------------------------------------- the template

class Template:
    """The captured x4 map split into global controls and per-row sections."""

    def __init__(self, data):
        self.data = data
        self.layout = []                 # ('ctrl', c) | ('section', base)
        self.rows = {s: {} for s in SECTIONS}
        seen = set()
        for c in data['controls']:
            m = ROW_RE.match(c['path'])
            if not m:
                self.layout.append(('ctrl', c))
                continue
            base, idx, rel = m.group(1), int(m.group(2)), m.group(3) or ''
            if base not in seen:
                seen.add(base)
                self.layout.append(('section', base))
            self.rows[base].setdefault(idx, []).append((rel, c))
        self.paths = {c['path']: c for kind, c in self.layout
                      if kind == 'ctrl'}

    def value(self, base, idx, rel):
        for r, c in self.rows[base][idx]:
            if r == rel:
                return c['value']
        return None

    def roles(self, base):
        """role -> template row indexes."""
        out = {}
        for idx in sorted(self.rows[base]):
            io = self.value(base, idx, '/IOType')
            has = {r for r, _ in self.rows[base][idx]}
            if base == '/devices/0/inputs':
                if io == 'Mic':
                    role = 'mic_hiz' if '/preamps/0/HiZ' in has else 'mic'
                elif io == 'ADAT':
                    role = 'adat'
                elif io == 'S/PDIF':
                    role = 'spdif'
                elif io == 'Virtual':
                    role = 'virtual'
                elif io == 'TalkbackMic':
                    role = 'talkback'
                else:
                    role = 'na'
            else:
                role = {'Line': 'line', 'ADAT': 'adat', 'S/PDIF': 'spdif',
                        'Cue': 'cue', 'Monitor': 'monitor',
                        'Headphone': 'hp',
                        'Monitor R for Surround': 'surround'}.get(io, 'other')
            out.setdefault(role, []).append(idx)
        return out


# ------------------------------------------------------------ model -> layout

def digital_kind(name):
    """'adat' | 'spdif' | 'aes' | 'other:<PREFIX>' | None for a channel name."""
    n = name.upper()
    if n.startswith('ADAT'):
        return 'adat'
    if n.startswith('S/PDIF'):
        return 'spdif'
    if n.startswith('AES'):
        return 'aes'
    m = re.match(r'([A-Z]+)\s*\d+$', n)
    if m and m.group(1) not in ('ANALOG', 'VIRTUAL', 'LINE', 'MIC', 'CUE',
                                'HP', 'AUX', 'MON', 'TALKBACK'):
        return 'other:' + m.group(1)
    return None


def plan_inputs(d):
    """Mixer input entries: dict(role, name, stereo, io, hiz)."""
    pre, hiz = d.get('preamps', 0), d.get('hiz', 0)
    out = []
    analog = 0
    for e in d.get('mixer_inputs') or []:
        name, n = e['name'], e['name'].upper()
        ent = {'name': name, 'stereo': bool(e.get('stereo')),
               'hiz': False, 'io': None}
        if n.startswith('ANALOG'):
            if analog < pre:
                ent.update(role='mic_hiz' if analog < hiz else 'mic',
                           io='Mic', hiz=analog < hiz)
            else:
                ent.update(role='line', io='Line')
            analog += 1
        elif n.startswith('VIRTUAL'):
            ent.update(role='virtual', io='Virtual')
        elif n == 'TALKBACK':
            if not d.get('talkback'):
                continue
            ent.update(role='talkback', io='TalkbackMic')
        elif n == 'UNAVAILABLE':
            ent.update(role='na', name='N/A 1', io=None)
        else:
            kind = digital_kind(name)
            if kind == 'adat':
                ent.update(role='adat', io='ADAT')
            elif kind == 'spdif':
                ent.update(role='spdif', io='S/PDIF')
            elif kind == 'aes':
                ent.update(role='spdif', io='AES/EBU')
            elif kind:
                ent.update(role='adat', io=kind.split(':')[1].title()
                           if kind.split(':')[1] != 'MADI' else 'MADI')
            else:
                ent.update(role='adat', io='Line')
        out.append(ent)
    return out


def stereo_names(entries):
    """StereoName per entry: pairs of consecutive mono channels, else the name."""
    out = []
    k = 0
    for i, e in enumerate(entries):
        name = e['name']
        if e['stereo']:
            out.append(re.sub(r'\s*[LR\d]+$', '', name) or name)
            continue
        m = re.match(r'^(.*?)(\d+)$', name)
        if not m:
            out.append(name)
            continue
        prefix, num = m.group(1), int(m.group(2))
        # first of a pair: odd number with a following number in the run
        nxt = entries[i + 1]['name'] if i + 1 < len(entries) else ''
        if num % 2 == 1 and nxt == f'{prefix}{num + 1}' and \
                not entries[i + 1]['stereo']:
            out.append(f'{prefix}{num}/{num + 1}')
        elif num == 1 and prefix.startswith('N/A'):
            out.append(f'{prefix}1/2')
        else:
            out.append(name)
        k += 1
    return out


def route_names(d, direction, mode):
    rules = d.get('route_rules') or {}
    ents = d['routes']['LoMid'][direction]
    ids = [hexint(e['ch_id']) for e in ents]
    by_id = {hexint(e['ch_id']): e for e in ents}
    keep = gen.shown(ids, mode, rules)
    return [by_id[i] for i in keep]


def plan_outputs(d):
    """Mixer output entries: dict(role, name, stereo, io)."""
    out = []
    play = d['routes']['LoMid'].get('playback', [])
    names = [e['name'] for e in play]
    lines = [n for n in names if re.match(r'LINE \d+$', n)]
    for n in lines:
        out.append({'role': 'line', 'name': 'ANALOG ' + n.split()[1],
                    'stereo': False, 'io': 'Line'})
    for kind, role, io in (('adat', 'adat', 'ADAT'), ('spdif', 'spdif', 'S/PDIF'),
                           ('aes', 'spdif', 'AES/EBU')):
        for n in names:
            if digital_kind(n) == kind:
                nm = n
                if kind == 'aes':
                    # AES/EBU L/R are one stereo pair
                    nm = n
                out.append({'role': role, 'name': nm, 'stereo': False,
                            'io': io})
    for n in names:
        k = digital_kind(n)
        if k and k.startswith('other:'):
            out.append({'role': 'adat', 'name': n, 'stereo': False,
                        'io': k.split(':')[1].title()})
    cues = (d.get('cue_buses') or {}).get('max') or 0
    for i in range(1, cues + 1):
        out.append({'role': 'cue', 'name': f'CUE {i}', 'stereo': True,
                    'io': 'Cue'})
    mo = d.get('mixer_outputs') or []
    has_mon = any(n.startswith('MON') for n in names)
    if has_mon:
        out.append({'role': 'monitor', 'name': 'MONITOR', 'stereo': True,
                    'io': 'Monitor'})
    if 'HP 1' in mo:
        hp = 2
    elif 'HP' in mo or any(n.startswith('HP') for n in names):
        hp = 1
    else:
        hp = 0
    for i in range(1, hp + 1):
        out.append({'role': 'hp', 'name': f'HP {i}' if hp > 1 else 'HP',
                    'stereo': True, 'io': 'Headphone'})
    if has_mon:
        out.append({'role': 'surround', 'name': 'MONITOR R FOR SURROUND',
                    'stereo': False, 'io': 'Monitor R for Surround'})
    return out


# ------------------------------------------------------------- row cloning

def pick(rows_by_role, role, k):
    lst = rows_by_role.get(role)
    if not lst:
        raise MapError(f'template has no {role!r} row')
    return lst[min(k, len(lst) - 1)]


def clone_row(tpl, base, tidx, newidx, drop=(), edits=None):
    """Template row `tidx` of `base` as row `newidx`: [(rel, ctrl)]."""
    out = []
    for rel, c in tpl.rows[base][tidx]:
        if any(rel == p or rel.startswith(p + '/') for p in drop):
            continue
        c2 = copy.deepcopy(c)
        c2['path'] = f'{base}/{newidx}{rel}'
        out.append((rel, c2))
    for rel, c in out:
        if edits and rel in edits:
            c['value'] = edits[rel]
    return out


def select_sends(row, keep):
    """Keep the sends whose template index is in `keep`, renumbered 0.."""
    remap = {t: n for n, t in enumerate(keep)}
    out = []
    for rel, c in row:
        m = SEND_RE.match(rel)
        if not m:
            out.append((rel, c))
            continue
        t = int(m.group(1))
        if t not in remap:
            continue
        new_rel = f'/sends/{remap[t]}{m.group(2) or ""}'
        head = c['path'][:len(c['path']) - len(rel)]
        c['path'] = head + new_rel
        out.append((new_rel, c))
    return out


def set_value(row, rel, value):
    for r, c in row:
        if r == rel:
            c['value'] = value
            return True
    return False


def clock_sources(d, digital, rec_ents):
    """The /ClockSource names a model offers.

    devices/<model>.json "clock_sources" is the list (parity map 16 finding
    5: ADAT and S/PDIF both where the TOSLink type is not settable, the
    active one where it is, AES/EBU on the Apollo 16 and x16, Dante too on
    the x16D).  Without it: Internal, Word Clock (not on desktop units) and
    one digital name chosen from the default mode.
    """
    if d.get('clock_sources'):
        return list(d['clock_sources'])
    clock = ['Internal']
    if not d.get('desktop'):
        clock.append('Word Clock')
    if any(digital_kind(e['name']) for e in rec_ents):
        clock.append(digital)
    return clock


# --------------------------------------------------------------- generation

def generate(d, tpl):
    slug = d['_slug']
    rules = d.get('route_rules') or {}
    toslink = rules.get('toslink', 'none')
    default_mode = rules.get('default_input', 'spdif')
    talkback = bool(d.get('talkback'))
    cue_max = (d.get('cue_buses') or {}).get('max') or 0
    cue_def = (d.get('cue_buses') or {}).get('default') or 0
    rec_ents = d['routes']['LoMid'].get('record', [])
    pla_ents = d['routes']['LoMid'].get('playback', [])
    aux_n = sum(1 for e in rec_ents if e.get('io') == 'Aux') // 2

    # devices/<model>.json: "mirror_to_digital" (the monitor's MirrorsToDigital
    # switch; for a model with a settable TOSLink output it is only live
    # while the output is S/PDIF, the control itself exists either way) and
    # "output_pad": {"line": .., "monitor": ..}.  A model without the keys
    # keeps the x4 behaviour (both present).
    mirror = d.get('mirror_to_digital', True)
    out_pad = d.get('output_pad') or {}

    in_roles = tpl.roles('/devices/0/inputs')
    out_roles = tpl.roles('/devices/0/outputs')
    ins = plan_inputs(d)
    outs = plan_outputs(d)
    sn = stereo_names(ins)

    def digital_active(role, io):
        if role == 'adat':
            if toslink == 'none':
                return True
            return default_mode == 'adat'
        if role == 'spdif':
            if toslink == 'none' or io == 'AES/EBU':
                return True
            return default_mode == 'spdif'
        return None

    sections = {}

    # --- mixer inputs
    rows, counters = [], {}
    for n, e in enumerate(ins):
        k = counters.get(e['role'], 0)
        counters[e['role']] = k + 1
        drop = []
        if e['role'] == 'line':
            # line-only analog input: a mic row without the preamp
            t = pick(in_roles, 'mic', 2)
            drop.append('/preamps')
        else:
            t = pick(in_roles, e['role'], k)
        edits = {'/Name': e['name'], '/Stereo': e['stereo'],
                 '/StereoName': sn[n]}
        if e['io'] is not None:
            edits['/IOType'] = e['io']
        if not talkback:
            drop.append('/TalkbackEnabled')
        act = digital_active(e['role'], e['io'])
        if act is not None:
            edits['/Active'] = act
        row = clone_row(tpl, '/devices/0/inputs', t, n, drop=drop, edits=edits)
        keep = list(range(min(aux_n, 2))) + \
            [2 + i for i in range(min(cue_max, 4))]
        row = select_sends(row, keep)
        rows.append(row)
    sections['/devices/0/inputs'] = rows

    # --- mixer outputs
    rows, counters = [], {}
    n_lines = sum(1 for o in outs if o['role'] == 'line')
    dest = [{'value': 'None', 'enabled': True}]
    line_names = [o['name'] for o in outs if o['role'] == 'line']
    for i in range(0, len(line_names), 2):
        dest.append({'value': f'Line {i + 1}-{min(i + 2, len(line_names))}',
                     'enabled': True})
    adat_n = sum(1 for o in outs if o['role'] == 'adat' and o['io'] == 'ADAT')
    for i in range(0, adat_n, 2):
        dest.append({'value': f'ADAT {i + 1}-{min(i + 2, adat_n)}',
                     'enabled': digital_active('adat', 'ADAT')})
    if any(o['io'] == 'S/PDIF' for o in outs):
        dest.append({'value': 'S/PDIF',
                     'enabled': digital_active('spdif', 'S/PDIF')})
    if any(o['io'] == 'AES/EBU' for o in outs):
        dest.append({'value': 'AES/EBU', 'enabled': True})
    for n, e in enumerate(outs):
        k = counters.get(e['role'], 0)
        counters[e['role']] = k + 1
        t = pick(out_roles, e['role'], k)
        drop = []
        surround_add = False
        if e['role'] == 'line':
            if k >= 4:
                # alt-monitor trims exist on the first four line outputs only
                drop += ['/AltMonTrim', '/AltMonEnabled']
            if n_lines > 4:
                # More than four line outputs: the captured x8p has the
                # surround controls on lines 1-6 and the last line carries
                # the stereo-sub switch (the x4's last line, template row 3).
                if k == n_lines - 1:
                    t = out_roles['line'][-1]
                else:
                    t = out_roles['line'][min(k, 2)]
                    surround_add = k < 6
        # Per-model output reference level (Pad) and digital mirror
        # (parity/16 F22, F13): the x4 rows carry both, so a model without
        # them has the controls dropped, not inherited.
        if e['role'] == 'line' and not out_pad.get('line', True):
            drop.append('/Pad')
        if e['role'] == 'monitor':
            if not out_pad.get('monitor', True):
                drop.append('/Pad')
            if not mirror:
                drop.append('/MirrorsToDigital')
        edits = {'/Name': e['name'], '/Stereo': e['stereo'],
                 '/IOType': e['io']}
        act = digital_active(e['role'], e['io'])
        if act is not None:
            edits['/Active'] = act
        row = clone_row(tpl, '/devices/0/outputs', t, n, drop=drop,
                        edits=edits)
        if surround_add:
            have = {r for r, _ in row}
            for r, c in tpl.rows['/devices/0/outputs'][out_roles['line'][-1]]:
                if r.startswith('/Surround') and r not in have:
                    c2 = copy.deepcopy(c)
                    c2['path'] = f'/devices/0/outputs/{n}{r}'
                    row.append((r, c2))
        if e['role'] == 'cue':
            for r, c in row:
                if r == '/OutputDestination':
                    c['values'] = copy.deepcopy(dest)
        rows.append(row)
    sections['/devices/0/outputs'] = rows

    # --- aux buses
    rows = []
    for n in range(min(aux_n, len(tpl.rows['/devices/0/auxs']))):
        row = clone_row(tpl, '/devices/0/auxs', n, n)
        rows.append(select_sends(row, list(range(min(cue_max, 4)))))
    sections['/devices/0/auxs'] = rows

    # --- driver route lists
    def route_rows(base, ents, tidx_map):
        rows = []
        for n, e in enumerate(ents):
            cid = hexint(e['ch_id'])
            row = clone_row(tpl, base, 0, n, edits={
                '/Name': e['name'], '/IOType': e.get('io'),
                '/ChannelIndex': cid & 0xff})
            rows.append(row)
        return rows

    in_routes = route_names(d, 'record', default_mode)
    out_routes = route_names(d, 'playback', default_mode)
    sections['/inputroutes'] = route_rows('/inputroutes', in_routes, None)
    sections['/outputroutes'] = route_rows('/outputroutes', out_routes, None)
    sections['/devices/0/asioinputs'] = route_rows(
        '/devices/0/asioinputs', rec_ents, None)
    asio_out = route_rows('/devices/0/asiooutputs', pla_ents, None)
    reserved = max(0, len(pla_ents) - ASIO_RESERVED_BASE)
    tail_src = [i for i in sorted(tpl.rows['/devices/0/asiooutputs'])
                if tpl.value('/devices/0/asiooutputs', i, '/IOType') ==
                'LLMDisk']
    for i in range(reserved):
        t = tail_src[min(i, len(tail_src) - 1)]
        asio_out.append(clone_row(tpl, '/devices/0/asiooutputs', t,
                                  len(pla_ents) + i, edits={
                                      '/Name': f'RESERVED {i + 1}',
                                      '/ChannelIndex': i + 1}))
    sections['/devices/0/asiooutputs'] = asio_out

    # --- global controls
    model = d['model']
    digital = ('AES/EBU' if d.get('digital_io') == 'aes' else
               'ADAT' if default_mode == 'adat' else 'S/PDIF')
    clock = clock_sources(d, digital, rec_ents)
    caps = device_caps(d)

    controls = []
    for kind, item in tpl.layout:
        if kind == 'section':
            for row in sections[item]:
                controls.extend(c for _, c in row)
            continue
        c = copy.deepcopy(item)
        p = c['path']
        if p in TALKBACK_GLOBALS and not talkback:
            continue
        if p == '/devices/0/RemoteMonitorController' and not d.get('desktop'):
            continue
        if p == '/devices/0/TOSLink':
            if toslink == 'none':
                continue
            c['value'] = 'ADAT' if default_mode == 'adat' else 'S/PDIF'
        elif p == '/devices/0/TOSLinkOutput':
            if toslink != 'per-direction':
                continue
            c['value'] = 'ADAT' if default_mode == 'adat' else 'S/PDIF'
        elif p in ('/devices/0/DeviceName', '/devices/0/DeviceTypeID'):
            c['value'] = model
        elif p in ('/CueBusCount', '/devices/0/CueBusCount'):
            c['value'] = cue_def
        elif p == '/TotalDriverInputRoutes':
            c['value'] = len(in_routes)
        elif p == '/TotalDriverOutputRoutes':
            c['value'] = len(out_routes)
        elif p == '/ClockSource':
            c['values'] = list(clock)
            c['value'] = 'Internal'
        elif p in caps:
            c['value'] = caps[p]
        controls.append(c)

    controls = [c for c in controls if not is_plugin_data(c['path'])]

    return {
        'generated': True,
        'generated_from': {'template': TEMPLATE_NAME,
                           'device': f'devices/{slug}.json'},
        'confidence': 'structure from the captured Apollo x4 map; channel '
                      'counts, names and preamp/HiZ/TOSLink/talkback controls '
                      'from the device JSON; nothing observed on this model',
        'host': tpl.data.get('host'),
        'port': tpl.data.get('port'),
        'propfilter': tpl.data.get('propfilter'),
        'message_count': tpl.data.get('message_count'),
        'tree_path': tpl.data.get('tree_path'),
        'device_name': model,
        'controls': controls,
    }


def dump(m):
    return json.dumps(m, ensure_ascii=False, separators=(',', ':')) + '\n'


# ------------------------------------------------------------ library / cache

def default_cache_dir():
    """$XDG_CACHE_HOME/open-apollo/device_maps, else ~/.cache/open-apollo/..."""
    base = os.environ.get('XDG_CACHE_HOME', '')
    if not base or not os.path.isabs(base):
        base = os.path.join(os.path.expanduser('~'), '.cache')
    return os.path.join(base, 'open-apollo', 'device_maps')


def cache_key(slug, root=ROOT):
    """Hash of everything a generated map depends on: the x4 template, the
    model's JSON and the generator itself (version and driver-model helpers)."""
    h = hashlib.sha256()
    h.update(f'gen-device-map v{GENERATOR_VERSION}\0'.encode())
    with open(__file__, 'rb') as f:
        h.update(f.read())
    h.update(b'\0')
    for path in (os.path.join(root, 'mixer-engine', 'device_maps',
                              TEMPLATE_NAME),
                 os.path.join(root, 'devices', slug + '.json')):
        with open(path, 'rb') as f:
            h.update(f.read())
        h.update(b'\0')
    return h.hexdigest()[:16]


def generate_for(slug, root=ROOT):
    """The generated map (dict) for devices/<slug>.json."""
    with open(os.path.join(root, 'devices', slug + '.json'),
              encoding='utf-8') as f:
        d = json.load(f)
    d['_slug'] = slug
    tpl = Template(load_map(os.path.join(root, 'mixer-engine', 'device_maps',
                                         TEMPLATE_NAME)))
    return generate(d, tpl)


def ensure_cached(slug, cache_dir=None, root=ROOT):
    """Path of the cached generated map for `slug`, writing it if missing.

    The file name carries cache_key(), so a changed template, device JSON or
    generator gets a new file; older files of the same model are removed.
    """
    cache_dir = cache_dir or default_cache_dir()
    stem = slug_to_file(slug)[:-len('.json')]
    key = cache_key(slug, root)
    path = os.path.join(cache_dir, f'{stem}.{key}.json')
    if os.path.exists(path):
        return path
    text = dump(generate_for(slug, root))
    os.makedirs(cache_dir, exist_ok=True)
    tmp = f'{path}.{os.getpid()}.tmp'
    with open(tmp, 'w', encoding='utf-8') as f:
        f.write(text)
    os.replace(tmp, path)
    for old in glob.glob(os.path.join(glob.escape(cache_dir),
                                      glob.escape(stem) + '.*.json')):
        if old != path and re.fullmatch(re.escape(stem) + r'\.[0-9a-f]{16}\.json',
                                        os.path.basename(old)):
            try:
                os.remove(old)
            except OSError:
                pass
    return path


# ---------------------------------------------------------------------- CLI

def selection(args, devs):
    if args.model:
        for s in args.model:
            if s not in devs:
                raise MapError(f'no devices/{s}.json')
        return list(args.model)
    cap = captured_slugs()
    out = []
    for slug, d in devs.items():
        if slug in cap:
            continue
        if args.all or d.get('device_types'):
            out.append(slug)
    return out


def main():
    ap = argparse.ArgumentParser(description=__doc__.split('\n')[0])
    ap.add_argument('--all', action='store_true',
                    help='every model without a captured map, not only those '
                         'with a driver device type')
    ap.add_argument('--model', nargs='+', metavar='SLUG')
    ap.add_argument('--out', metavar='DIR',
                    help='write device_map_<model>.json files here instead '
                         'of filling the cache')
    ap.add_argument('--compare', metavar='SLUG',
                    help='diff the control paths of the generated map of SLUG '
                         'against its captured map')
    ap.add_argument('--list', action='store_true')
    args = ap.parse_args()

    try:
        devs = load_devices()
        cap = captured_slugs()

        if args.list:
            for slug, d in devs.items():
                state = 'captured' if slug in cap else 'generated at runtime'
                print(f'{slug:36} {d.get("device_types") or "-"!s:34} '
                      f'{state}')
            return 0

        if args.compare:
            slug = args.compare
            if slug not in devs:
                raise MapError(f'no devices/{slug}.json')
            if slug not in cap:
                raise MapError(f'{slug} has no captured map to compare with')
            new = generate_for(slug)
            old = load_map(os.path.join(MAPS, slug_to_file(slug)))
            a = {c['path'] for c in old['controls']}
            b = {c['path'] for c in new['controls']}
            only_old = sorted(a - b)
            only_new = sorted(b - a)
            print(f'{slug}: captured {len(a)} controls, generated {len(b)}; '
                  f'{len(only_old)} only in captured, {len(only_new)} only in '
                  f'generated')
            for p in only_old[:40]:
                print('  - ' + p)
            for p in only_new[:40]:
                print('  + ' + p)
            return 0

        for slug in selection(args, devs):
            if slug in cap:
                print(f'skip {slug}: captured map, never overwritten')
                continue
            if args.out:
                os.makedirs(args.out, exist_ok=True)
                path = os.path.join(args.out, slug_to_file(slug))
                with open(path, 'w', encoding='utf-8') as f:
                    f.write(dump(generate_for(slug)))
            else:
                path = ensure_cached(slug)
            print(f'wrote {path}')
        return 0
    except (MapError, KeyError, ValueError, OSError) as e:
        print(f'gen-device-map: {e}', file=sys.stderr)
        return 2


if __name__ == '__main__':
    sys.exit(main())
