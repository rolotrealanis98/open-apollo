"""Structural classification of the VST2 / IUADPlug UAD plug-ins (RTTI + vtable shapes; never executes vendor code).

For every inventory DLL that imports UAD_CreateIUADPlug but is not a classic CHyp plug-in, find the RTTI complete-object
locators (class, sub-object offset, vtable, base-class hierarchy) and classify by structure:

  Softube  : a class derived from CSoftubeEffect*            (owned by softube_profiles.py)
  DSPAP    : DSPAudioProcessor / JuceVSTWrapper in the bases  (owned by dspaudioprocessor_profiles.py)
  B        : UADAdapter (VSTAdapter) with a BXEffect          (Brainworx)
  C        : IUADPlugRelab in the hierarchy
  A        : AudioEffect descendant (plain VST SDK 2.x effect) with a handler sub-object whose setParameter funnels into one push function
  other    : none of the above (e.g. AudioEffect with a parameter-object setter)

Also reports the structural facts sub-family A derivation needs (effect class, handler offset/vtable, primary vtable).
Usage: python3 classify_vst2_iuadplug.py [--output vst2-classification.json]
"""
from pathlib import Path
import argparse, json, re, struct
import collections
from capstone.x86 import X86_OP_IMM, X86_OP_MEM
from dspaudioprocessor_profiles import Image, DLL_ROOT, PATTERNS

HANDLER = '.?AVCUADPlugCallbackHandler@@'
IFACE_WRITE_SLOTS = (0xa0, 0xa8, 0xb8)        # IUADPlug set_int, set_float, set_float_array


def cols(img):
    """{col_rva: dict(offset, type, bases)} for every RTTI complete-object locator in the image."""
    im, out = img.im, {}
    pos = im.find(b'\x01\x00\x00\x00')
    while pos != -1:
        if pos % 4 == 0 and pos + 24 <= len(im):
            sig, off, cd, td, ch, self_rva = struct.unpack_from('<6I', im, pos)
            if sig == 1 and self_rva == pos and 0 < td < len(im) and 0 < ch < len(im):
                name = type_name(img, td)
                if name:
                    out[pos] = dict(offset=off, type=name, bases=bases(img, ch))
        pos = im.find(b'\x01\x00\x00\x00', pos + 1)
    return out


def type_name(img, td):
    o = td + 16
    if o + 4 > len(img.im) or img.im[o:o + 4] != b'.?AV' and img.im[o:o + 4] != b'.?AU':
        return None
    return img.im[o:img.im.index(b'\0', o)].decode('ascii')


def bases(img, ch):
    sig, attr, count, array = struct.unpack_from('<4I', img.im, ch)
    if count > 64 or array >= len(img.im):
        return []
    names = []
    for i in range(count):
        bcd = struct.unpack_from('<I', img.im, array + 4 * i)[0]
        if bcd + 4 > len(img.im):
            return names
        n = type_name(img, struct.unpack_from('<I', img.im, bcd)[0])
        if n:
            names.append(n)
    return names


def vtables(img, col_map):
    """[(class, offset, vtable_va, bases)] by locating `pointer to COL` qwords (one pass over the image)."""
    wanted = {img.base + rva: c for rva, c in col_map.items()}
    n = len(img.im) // 8
    out = []
    for i, word in enumerate(struct.unpack_from('<%dQ' % n, img.im, 0)):
        c = wanted.get(word)
        if c:
            out.append(dict(type=c['type'], offset=c['offset'], vtable=img.base + 8 * i + 8, bases=c['bases']))
    return out


def fragments(img):
    """{primary_begin_rva: [(begin, end), ...]} following UNW_FLAG_CHAININFO from each fragment to its parent."""
    parent, spans = {}, {}
    for e in img.pe.DIRECTORY_ENTRY_EXCEPTION:
        b, f, u = e.struct.BeginAddress, e.struct.EndAddress, e.struct.UnwindData
        spans[b] = (b, f)
        if img.im[u] >> 3 & 4:                                    # chained: parent RUNTIME_FUNCTION after the unwind codes
            codes = img.im[u + 2]
            parent[b] = struct.unpack_from('<I', img.im, u + 4 + 2 * ((codes + 1) & ~1))[0]
    def root(b):
        while b in parent:
            b = parent[b]
        return b
    out = collections.defaultdict(list)
    for b, span in spans.items():
        out[root(b)].append(span)
    return out


def code(img, va, frag):
    """All instructions of the function at `va`, chained fragments included, in address order."""
    out = []
    for b, f in sorted(frag[va - img.base]):
        out += list(img.cs.disasm(img.im[b:f], img.base + b))
    return out


def follow(img, va):
    """Resolve tiny adjustor/forwarding thunks (`... ; jmp rel`, at most 4 instructions; unwind-less leaves included)."""
    for _ in range(4):
        o = va - img.base
        ins = []
        for i in img.cs.disasm(img.im[o:o + 32], va):
            ins.append(i)
            if i.mnemonic in ('jmp', 'ret', 'call') or len(ins) > 4:
                break
        if len(ins) > 4 or ins[-1].mnemonic != 'jmp' or ins[-1].operands[0].type != X86_OP_IMM:
            return va
        va = ins[-1].operands[0].imm
    return va


def direct_calls(ins):
    return [i.operands[0].imm for i in ins if i.mnemonic == 'call' and i.operands[0].type == X86_OP_IMM]


def writes_iface(img, va, frag):
    if va - img.base not in frag:      # leaf without unwind info: cannot be a push function
        return False
    ins = code(img, va, frag)
    return any(i.mnemonic == 'call' and i.operands[0].type == X86_OP_MEM and i.operands[0].mem.disp in IFACE_WRITE_SLOTS for i in ins)


def effect_class(vt):
    """The unique AudioEffect descendant that has a CUADPlugCallbackHandler sub-object, or ValueError."""
    effects = {v['type'] for v in vt if HANDLER in v['bases'] and v['type'] != HANDLER
               and any(b in ('.?AVAudioEffect@@', '.?AVAudioEffectX@@') for b in v['bases'])}
    if len(effects) != 1:
        raise ValueError('expected exactly one AudioEffect class with a CUADPlugCallbackHandler base ' + repr(effects))
    return next(iter(effects))


def push_function(img, vt, effect, frag):
    """The single IUADPlug-writing function the effect's setParameter (vtable slot 10) calls, or ValueError."""
    (vtable,) = [v['vtable'] for v in vt if v['type'] == effect and v['offset'] == 0]
    set_parameter = follow(img, img.vtable(vtable, 16)[10])
    start, _, _ = img.function(set_parameter)
    pushes = [c for c in sorted(set(direct_calls(code(img, start, frag)))) if writes_iface(img, c, frag)]
    if len(pushes) != 1:
        raise ValueError('setParameter does not funnel into exactly one IUADPlug push function ' + repr([hex(c) for c in pushes]))
    return pushes[0]


def short(name):
    return re.sub(r'^\.\?A[VU]|@@$', '', name)


def classify(img, vt):
    """(sub_family, evidence) from structure only."""
    every = {b for v in vt for b in v['bases']} | {v['type'] for v in vt}
    has = lambda part: sorted(short(n) for n in every if part in n)
    if has('CSoftubeEffect'):
        return 'Softube', has('CSoftubeEffect')[:2]
    if has('DSPAudioProcessor') or has('JuceVSTWrapper'):
        return 'DSPAP', has('DSPAudioProcessor') + has('JuceVSTWrapper')
    if has('UADAdapter') and has('BXEffect'):
        return 'B', has('UADAdapter')[:2] + has('BXEffect')[:1]
    if has('IUADPlugRelab'):
        return 'C', has('IUADPlugRelab')
    try:
        effect = effect_class(vt)
    except ValueError:
        return 'other', has('AudioEffect')[:3]
    try:
        push_function(img, vt, effect, fragments(img))
    except (ValueError, StopIteration):
        return 'other', [short(effect), 'AudioEffect, but setParameter is not a single-push function']
    return 'A', [short(effect)]


def facts(img, vt):
    """Structural facts for sub-family A: effect class, primary vtable, handler sub-object offset/vtable."""
    handler = [v for v in vt if '.?AVCUADPlugCallbackHandler@@' in v['bases'] and v['type'] != '.?AVCUADPlugCallbackHandler@@']
    out = []
    for h in handler:
        primary = [v for v in vt if v['type'] == h['type'] and v['offset'] == 0]
        out.append(dict(effect=short(h['type']), handler_offset=h['offset'], handler_vtable=hex(h['vtable']),
                        processor_vtable=[hex(p['vtable']) for p in primary]))
    return out


def survey():
    """{plug-in name: dict(framework, sub_family, evidence, handler)} for every non-classic IUADPlug inventory DLL."""
    inv = json.loads((PATTERNS / 'families/inventory.json').read_text())['plugins']
    wrap = json.loads((PATTERNS / 'families/host-wrapper-families.json').read_text())
    members = [m for g in wrap['windows_groups'] if g['bucket'] != 'classic_chyp_imports' for m in g['members']]
    by_name = {p['name']: p for p in inv}
    result = {}
    for name in sorted(members):
        img = Image(DLL_ROOT / by_name[name]['dll'])
        vt = vtables(img, cols(img))
        family, evidence = classify(img, vt)
        result[name] = dict(framework=by_name[name]['framework'], sub_family=family, evidence=evidence, handler=facts(img, vt))
    return result


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--output', type=Path)
    args = ap.parse_args()
    result = survey()
    counts = collections.Counter(r['sub_family'] for r in result.values())
    doc = dict(schema='openapollo-vst2-iuadplug-classification/1', counts=dict(counts), plugins=result)
    if args.output:
        args.output.write_text(json.dumps(doc, indent=1) + '\n')
    print(json.dumps(doc['counts']))
    for name, r in result.items():
        print('%-5s %-8s %-42s %s' % (r['sub_family'], r['framework'], name, ','.join(r['evidence'])[:70]))


if __name__ == '__main__':
    main()
