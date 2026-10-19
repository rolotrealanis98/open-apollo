"""Static profile discovery for the UAD VST2 sub-family B: Brainworx `BXEffect` plug-ins behind a `UADAdapter`.

Sibling of vst2_profiles.py (same envelope, same `openapollo-vst2-iuadplug-profile/1` schema, `vst2.variant = 'bx'` selects the
host class executor/vst2_bx_host.py). Never loads or executes vendor code. Derived from the image and re-verified on every run:
DLL/capture hashes against families/inventory.json, the PE export `VSTPluginMain` and entry point (the original static-CRT
`DllMain`), `UADAdapter` / `UADAdapterPlugCallbackHandler` / effect vtables found through RTTI, handler slot 3 = the inventory
descriptor getter (and the render id immediate in it), the pinned prologue of every function the host intercepts, the effect's
`createEditor` virtual (slot 0xe0) = the pinned no-editor intercept, and the captured Console names/defaults/steps/text against
the parameter grid. Reverse-engineered (Ghidra, see families-vst2-iuadplug.md section 7) and identity-pinned: the KNOWN table.

Usage: python3 vst2_bx_profiles.py [--plugin NAME ...] [--output profiles-vst2-bx.json]
"""
from pathlib import Path
import argparse, json, re, struct
from dspaudioprocessor_profiles import Image, DLL_ROOT, CAPTURES, PATTERNS, sha, find_crt_heap, iat_callers, console_text, text_or_none

HERE = Path(__file__).resolve().parent
SCHEMA = 'openapollo-vst2-iuadplug-profile/1'
CONTINUOUS_GRID = 10000      # Console continuous values are k/100 or k/10000 floats; 1/10000 keeps every captured default exact

# Facts shared by every plug-in of the sub-family (same adapter/effect object layout; verified at construction by pinned vtables).
COMMON = dict(
    interface_version=0x65,                   # UADInterfaceVersion passed to UAD_CreateIUADPlug by the UADAdapter constructor
    aeffect_offset=0x30,                      # AEffect embedded in the adapter (param_1 + 6)
    effect_offset=0x100,                      # BXEffect* (adapter[0x20])
    handler_pointer_offset=0x1d8,             # heap UADAdapterPlugCallbackHandler* (adapter[0x3b])
    iface_offset=0x1e0,                       # IUADPlug* (adapter[0x3c])
    steps_slot=0x250,                         # BXEffect::getNumSteps(parameter): discrete value count (<= 1: continuous); verified at construction
    create_editor_slot=0xe0,                  # BXEffect::createEditor: the adapter skips its editor wrapper when it returns NULL
    iface_answers={0x30: 0, 0x78: 0, 0x88: 0},  # +0x78 fills a 256-byte (empty) host string; +0x30/+0x88 as in sub-family A
    quiet_iface_slots=[0x118],
    host=dict(sample_rate=48000.0, block_size=32),
    module_path='C:\\Program Files\\Universal Audio\\Powered Plugins\\UAD-2 Powered Plugins\\%s.dll',
)

# Only what cannot be derived from the image or the captured Console metadata. Everything else (effect class, CRT heap entry points, adapter
# pointer offsets, editor/steps slots, step counts, DLL identity) is derived in derive_facts() and re-verified against the live virtuals at construction.
# Brainworx TMT Chan (Tolerance Modeling channel; 20 steps): the generation's own setParameter override (Vitalizer MK3-T 0x1800612c0) answers a request of the
# top step by calling setParameter(index, 1.0 - field) with field = 0.05 (constructor store `mov [this+0x8b30], 0x3d4ccccd` at 0x18005d196), which the
# generic setter snaps to step 18. The host checks the live result against this pin (interface 0x67 plug-ins with the TMT parameter).
TMT_TOP_TO_18 = {19: 18}

# bx_masterdesk / Classic: setParameter(LimTurbo, on) is the plug-in's own override (0x180001de0, case index 15) which re-derives Out Trim (parameter 19) through
# its dB helpers (0x18003c7c0 / 0x18003c700) and writes it with setParameter(19, v): the coupled result is 59/60 = 0.98333 (an off-grid continuous value),
# while a direct setParameter(19, x) keeps x unchanged (probed: no quantisation inside the setter). So Out Trim is compared at half a grid step (exact=False).
KNOWN = {
    'UAD bx_masterdesk': dict(inexact=(19,)),
    'UAD SPL Vitalizer MK3-T': dict(remapped={7: TMT_TOP_TO_18}),
    'UAD Maag EQ4 MS': dict(remapped={59: TMT_TOP_TO_18}),
    'UAD elysia alpha compressor V2': dict(remapped={13: TMT_TOP_TO_18}),
    'UAD bx_saturator V2': dict(
        text_waivers={22: 'Console appends the kHz unit (8.0k); the original adapter display (effGetParamDisplay) returns the number only (8.0)',
                    43: 'Console shows 0 where the original adapter display (effGetParamDisplay) shows Off'}),
    'UAD ADA Flanger': dict(
        text_waivers={15: 'Console shows ---- for SycSpd while Sync is Off; the original adapter display (effGetParamDisplay) shows the tempo division'}),
}
# Written by bx_calibrate_steps.py from the original virtual: {plug-in: {'steps': {index: live getNumSteps of the parameters whose step list the
# Console does not show}, 'steps_slot': only when the layout is not the default editor + 0x170}. Re-verified against that virtual at construction.
STEPS_FILE = HERE / 'vst2-bx-steps.json'
STEPS_AFTER_EDITOR = 0x170   # getNumSteps sits 0x170 bytes after createEditor in the BXEffect layouts seen (0xe0 -> 0x250, bx_tuner 0x58 -> 0x1c8) ...
STEPS_AFTER_EDITOR_SHORT = 0x148   # ... except the 88/89-slot BXEffect base classes (TwinTube, Vitalizer MK2-T, bx_digital V2, Millennia, ...): 0x58 -> 0x1a0


def sha_prefix_ok(full, prefix):
    return full.startswith(prefix)


def rtti_vtable(img, class_name, nth=0):
    """Primary (subobject offset 0) vftable of an MSVC class through RTTI: TypeDescriptor -> COL -> pointer to COL -> vtable."""
    token = ('.?AV' + class_name + '@@').encode() + b'\0'
    at = img.im.find(token)
    if at < 0:
        raise ValueError('no RTTI type descriptor for ' + class_name)
    td_rva = at - 16
    cols = []
    for m in re.finditer(re.escape(struct.pack('<I', td_rva)), img.im):
        o = m.start() - 12
        if o >= 0 and struct.unpack_from('<III', img.im, o)[0] == 1 and struct.unpack_from('<I', img.im, o + 4)[0] == 0:
            cols.append(img.base + o)
    vts = []
    for col in cols:
        for m in re.finditer(re.escape(struct.pack('<Q', col)), img.im):
            if m.start() % 8 == 0:
                vts.append(img.base + m.start() + 8)
    if len(vts) <= nth:
        raise ValueError('no primary vftable for ' + class_name)
    return sorted(vts)[nth]


def class_bases(img, class_name):
    """Base-class names (RTTI class hierarchy descriptor) of an MSVC class, or None when the image has no such class."""
    at = img.im.find(('.?AV' + class_name + '@@').encode() + b'\0')
    if at < 0:
        return None
    for m in re.finditer(re.escape(struct.pack('<I', at - 16)), img.im):
        o = m.start() - 12
        if o >= 0 and struct.unpack_from('<III', img.im, o)[0] == 1:
            chd = struct.unpack_from('<I', img.im, o + 16)[0]
            n, arr = struct.unpack_from('<II', img.im, chd + 8)
            names = []
            for j in range(n):
                td = struct.unpack_from('<I', img.im, struct.unpack_from('<I', img.im, arr + 4 * j)[0])[0]
                names.append(img.im[td + 16:img.im.index(b'\0', td + 16)].decode())
            return names
    return []


def callers_of(img, target):
    out = set()
    for m in re.finditer(b'\xe8', img.im):
        o = m.start()
        if o + 5 <= len(img.im) and img.base + o + 5 + struct.unpack_from('<i', img.im, o + 1)[0] == target:
            try:
                out.add(img.function(img.base + o)[0])
            except ValueError:
                pass
    return out


def crt_heap(img):
    """malloc/free (find_crt_heap), realloc (the one HeapReAlloc caller) and calloc (newer CRT: the HeapAlloc caller that multiplies with overflow
    check; older CRT: the short wrapper around the other HeapAlloc caller). calloc is None when neither shape is found (the original then runs)."""
    heap = {k: int(v, 16) for k, v in find_crt_heap(img).items()}
    reallocs = iat_callers(img, b'kernel32.dll', b'HeapReAlloc')
    if len(reallocs) != 1:
        raise ValueError('static CRT realloc not identified: %r' % [hex(x) for x in reallocs])
    heap['realloc'] = reallocs.pop()
    # memset: the static CRT routine that broadcasts the fill byte with movabs 0x0101010101010101 and has the `rep stosb` path
    fills = set()
    for m in re.finditer(b'\\x49\\xb9' + b'\\x01' * 8, img.im):
        try:
            start, end, code = img.function(img.base + m.start())
        except ValueError:
            continue
        if any(i.mnemonic == 'rep stosb' or i.mnemonic.startswith('rep stos') for i in code):
            fills.add(start)
    if len(fills) == 1:
        heap['memset'] = fills.pop()
    cand = []
    for va in iat_callers(img, b'kernel32.dll', b'HeapAlloc') - {heap['malloc']}:
        start, end, code = img.function(va)
        mn = [i.mnemonic for i in code]
        if end - start > 200 or 'div' not in mn or 'imul' not in mn:
            continue
        if any(i.mnemonic == 'mov' and i.op_str.endswith(', r8') for i in code[:6]):   # older CRT: _calloc_impl(n, size, &errno) behind calloc
            cand += [c for c in callers_of(img, va) if img.function(c)[1] - c <= 100]
        else:
            cand.append(va)                                                           # newer CRT: calloc itself
    if len(cand) == 1:
        heap['calloc'] = cand[0]
    return heap


def adapter_layout(img):
    """From the UADAdapter constructor: handler/IUADPlug pointer offsets (r8 = handler for UAD_CreateIUADPlug(..., version), the stored result; version 0x65..0x67) and the
    BXEffect::createEditor virtual slot (the last `call [rax+d]; mov x, rax; test rax, rax; je` after the effect is built)."""
    vt = rtti_vtable(img, 'UADAdapter')
    ctors = set()
    for m in re.finditer(b'\x48\x8d', img.im):
        o = m.start()
        if o + 7 <= len(img.im) and img.im[o + 2] & 0xc7 == 5 and img.base + o + 7 + struct.unpack_from('<i', img.im, o + 3)[0] == vt:
            try:
                ctors.add(img.function(img.base + o)[0])
            except ValueError:
                pass
    found = []
    for fn in ctors:
        _, end, code = img.function(fn)
        if end - fn < 400:
            continue
        handler = iface = None
        for k, i in enumerate(code):
            if i.mnemonic == 'mov' and re.fullmatch(r'r9d, 0x6[0-9a-f]', i.op_str):
                version = int(i.op_str[4:], 16)
                for j in code[max(0, k - 4):k + 4]:   # the last r8 load before the call is the handler
                    mm = re.fullmatch(r'r8, qword ptr \[(\w+) \+ (0x[0-9a-f]+)\]', j.op_str) if j.mnemonic == 'mov' else None
                    handler = int(mm.group(2), 16) if mm else handler
                for j in code[k:k + 14]:              # the result is stored (from rax, or via r11 in unoptimized builds) at handler + 8
                    mm = re.fullmatch(r'qword ptr \[(\w+) \+ (0x[0-9a-f]+)\], (rax|r11)', j.op_str) if j.mnemonic == 'mov' else None
                    if mm and handler is not None and int(mm.group(2), 16) == handler + 8:
                        iface = handler + 8
                        break
        editors = []   # `call [rax+d]` whose result is null-tested right away (createEditor); the last one in the constructor
        for k, i in enumerate(code[:-4]):
            mm = re.fullmatch(r'qword ptr \[rax \+ (0x[0-9a-f]+)\]', i.op_str) if i.mnemonic == 'call' else None
            tail = code[k + 1:k + 5]
            if mm and any(j.mnemonic == 'test' and j.op_str == 'rax, rax' or j.mnemonic == 'cmp' and j.op_str.endswith(', 0') for j in tail[:3]) \
                    and any(j.mnemonic == 'je' for j in tail) and tail[0].mnemonic in ('mov', 'test', 'cmp'):
                editors.append(int(mm.group(1), 16))
        if handler is not None and iface == handler + 8 and editors:
            found.append((handler, iface, editors[-1], version))
    if len(set(found)) != 1:
        raise ValueError('UADAdapter constructor layout not identified: %r' % found)
    return found[0]


def vtable_length(img, va):
    """Number of code pointers in a vtable (stops at the first non-.text pointer, i.e. the next object's RTTI locator)."""
    text = [(s.VirtualAddress + img.base, s.VirtualAddress + img.base + s.Misc_VirtualSize) for s in img.pe.sections if s.Characteristics & 0x20000000]
    n = 0
    while any(a <= img.u64(va + 8 * n) < b for a, b in text):
        n += 1
    return n


def derive_facts(img):
    """Structure-derived replacements for the old per-plug-in pins."""
    derived = [n for n in sorted(set(x.decode() for x in re.findall(rb'\.\?AV([A-Za-z0-9_]+)@@\0', img.im)))
               if n != 'BXEffect' and '.?AVBXEffect@@' in (class_bases(img, n) or [])]
    if len(derived) != 1:
        raise ValueError('no unique BXEffect subclass in the image: %r' % derived)
    handler, iface, editor, version = adapter_layout(img)
    return dict(effect_class=derived[0], heap=crt_heap(img), handler_pointer_offset=handler, iface_offset=iface,
                create_editor_slot=editor,
                steps_slot=editor + (STEPS_AFTER_EDITOR_SHORT if vtable_length(img, rtti_vtable(img, 'BXEffect')) <= 89 else STEPS_AFTER_EDITOR), interface_version=version)


def build(name, inventory):
    item = inventory[name]
    dll = DLL_ROOT / (name + '.dll')
    if sha(dll) != item['dll_sha256']:
        raise ValueError('original DLL identity differs')
    capture = CAPTURES / (item['capture'] + '.json')
    if not capture.exists():
        raise ValueError('needs capture: ' + capture.name)
    img = Image(dll)
    img.pe.parse_data_directories(directories=[0, 1])
    known = dict(COMMON, **derive_facts(img), **KNOWN.get(name, {}))
    if known['interface_version'] == 0x67:   # newer IUADPlug (UAD2AGain-era): channel count query, validity query, Resume/Suspend notifications (as sub-family C)
        known['iface_answers'] = {**known['iface_answers'], 0x28: 'host_channels', 0x138: 0, 0x160: 0, 0x168: 0}
    pins = json.loads(STEPS_FILE.read_text()).get(name, {}) if STEPS_FILE.exists() else {}
    known['steps_overrides'] = {int(i): n for i, n in pins.get('steps', {}).items()}
    if 'steps_slot' in pins:
        known['steps_slot'] = int(pins['steps_slot'], 16)
    exports = {e.name.decode(): img.base + e.address for e in img.pe.DIRECTORY_ENTRY_EXPORT.symbols if e.name}
    entry = exports.get('VSTPluginMain')
    if entry is None or exports.get('main') != entry:
        raise ValueError('VST2 entry points VSTPluginMain/main missing or different')
    dllmain = img.base + img.pe.OPTIONAL_HEADER.AddressOfEntryPoint
    # --- vtables (RTTI), descriptor getter, render id ------------------------------------------------------------
    adapter_vt = rtti_vtable(img, 'UADAdapter')
    effect_vt = rtti_vtable(img, known['effect_class'])
    descriptor = item['discovery']['descriptors'][0]
    # Handler vtable and descriptor getter (slot 3) from RTTI. The inventory either attests the receiver (subobject vtable) or only lists the
    # function and its unwind ranges (chained-fragment layouts): then slot 3 must be the start of the inventory's first range and the render
    # id store the inventory points at must write the manifest id.
    handler_vt = rtti_vtable(img, 'UADAdapterPlugCallbackHandler')
    slots = img.vtable(handler_vt, 32)
    getter = slots[3]
    fn = descriptor['function']
    if getter not in (int(fn['va'], 16), int(fn['pe_ranges'][0][0], 16)):
        raise ValueError('handler slot 3 is not the discovered descriptor getter')
    subs = descriptor['descriptor_subobject_vtables']
    if subs and (int(subs[0]['va'], 16) != handler_vt or subs[0]['slot'] != '0x18'):
        raise ValueError('inventory descriptor receiver is not UADAdapterPlugCallbackHandler slot 3')
    if len(img.vtable(adapter_vt, 128)) < 40 or len(img.vtable(effect_vt, 128)) < 0x38:
        raise ValueError('implausible adapter/effect vtables')
    store = int(descriptor['render_store_va'], 16)
    o = store - img.base
    ins = list(img.cs.disasm(img.im[o:o + 15], store))[:1]
    render = [ins[0].operands[1].imm] if ins and ins[0].mnemonic == 'mov' and ins[0].operands[0].type == 3 and ins[0].operands[0].mem.disp == 0xaa4 else None
    if render != [item['render_id_manifest']] or descriptor['render_id'] != item['render_id_manifest']:
        raise ValueError('descriptor render id differs from the manifest ' + repr(render))
    # --- intercepted functions: prologue pinned ----------------------------------------------------------------------
    editor_va = img.vtable(effect_vt, 128)[known['create_editor_slot'] // 8]
    targets = [(kind, va) for kind, va in sorted(known['heap'].items())] + [('no_editor', editor_va)]
    intercepts = []
    for kind, va in targets:
        start, end, _ = img.function(va)
        if start != va or end - start < 16:
            raise ValueError('intercept target is not a function start ' + kind)
        o = start - img.base
        intercepts.append(dict(kind=kind, va=hex(start), size=end - start, prologue=img.im[o:o + 16].hex()))
    # --- parameters: grid from step lists (stepped) or the continuous 1/10000 grid; defaults/text from the capture -----
    cap = json.loads(capture.read_text())
    props = {}
    for c in cap['controls']:
        p = c['path'].split('/parameters/')
        if len(p) == 2:
            idx, field = p[1].split('/')
            props.setdefault(int(idx), {})[field] = c
    count = item['parameter_count']
    if sorted(props) != list(range(count)):
        raise ValueError('captured parameter schema differs')
    params, text = [], []
    # step counts: the captured StepValue list length (continuous: none); the few the Console does not list are pinned in steps_overrides.
    # Both are checked against the live BXEffect::getNumSteps virtual at construction.
    steps_all = [len(props[i].get('StepValue', {}).get('values') or []) or 1 for i in range(count)]
    for i, n in known.get('steps_overrides', {}).items():
        steps_all[i] = n
    for i in range(count):
        listed = props[i].get('StepValue', {}).get('values')
        steps = steps_all[i]
        if listed and len(listed) != steps:
            raise ValueError('captured step list differs from the pinned BXEffect step count for parameter %d' % i)
        maximum = steps - 1 if steps > 1 else CONTINUOUS_GRID
        norm = props[i]['NormalizedValue']['value']
        default = round(norm * maximum)
        error = abs(struct.unpack('<f', struct.pack('<f', default / maximum))[0] - norm)
        if error > 0.5 / maximum + 1e-6:
            raise ValueError('captured default is not on the grid for parameter %d' % i)
        # exact: the captured default sits on the grid (1e-6); otherwise (a continuous knob whose default is an arbitrary float,
        # e.g. a log-scaled frequency) the grid integer is the nearest step and constructor/readback state is compared at half a step
        params.append(dict(name=props[i]['Name']['value'], maximum=maximum, default=default, exact=steps <= 1 and error <= 1e-6 and i not in known.get('inexact', ()), steps=steps,
                           grid_source='BXEffect step count %d' % steps if steps > 1 else 'continuous; grid 1/%d' % CONTINUOUS_GRID))
        text.append(console_text(props[i]))
    vst2 = dict(
        variant='bx', family='BXEffect behind UADAdapter', entry_va=hex(entry), dllmain_va=hex(dllmain),
        interface_version=hex(known['interface_version']), aeffect_offset=hex(known['aeffect_offset']),
        processor_vtable=hex(adapter_vt), effect_vtable=hex(effect_vt), effect_offset=hex(known['effect_offset']),
        handler=dict(offset=hex(known['handler_pointer_offset']), pointer_offset=hex(known['handler_pointer_offset']), vtable=hex(handler_vt),
                     slots={hex(8 * i): hex(v) for i, v in enumerate(slots[:16])}),
        iface_offset=hex(known['iface_offset']), steps_slot=hex(known['steps_slot']),
        descriptor=dict(getter_va=hex(getter), render_store_va=descriptor['render_store_va'], render_id=descriptor['render_id'],
                        evidence=descriptor['status']),
        iface_answers={hex(k): v for k, v in known['iface_answers'].items()}, quiet_iface_slots=[hex(x) for x in known['quiet_iface_slots']],
        host=known['host'], module_path=known['module_path'] % name, intercepts=intercepts, parameters=params, captured_text=text_or_none(text),
        text_waivers={str(k): v for k, v in known.get('text_waivers', {}).items()},
        evidence=dict(source='families-vst2-iuadplug.md'))
    if known.get('remapped'):
        vst2['remapped'] = {str(i): {str(k): v for k, v in m.items()} for i, m in known['remapped'].items()}
    if known['interface_version'] == 0x67:
        # the original label table is empty for some steps of stepped parameters; the Console lists their captured step label (the decimal step)
        vst2['step_labels'] = {str(i): props[i]['StepValue']['values'] for i in range(count) if len(props[i].get('StepValue', {}).get('values') or []) > 1}
    return dict(schema=SCHEMA, status='discovered', name=name, framework='VST2IUADPlug',
                dll=dict(path=str(dll), sha256=item['dll_sha256']),
                captured_metadata=dict(path=str(capture), sha256=sha(capture)), vst2=vst2)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--plugin', action='append')
    ap.add_argument('--output', type=Path, default=HERE / 'profiles-vst2-bx.json')
    args = ap.parse_args()
    inventory = {p['name']: p for p in json.loads((PATTERNS / 'families/inventory.json').read_text())['plugins']}
    names = args.plugin or sorted(n for n, r in json.loads((HERE / 'vst2-classification.json').read_text())['plugins'].items() if r['sub_family'] == 'B')
    profiles, failed = [], {}
    for n in names:
        try:
            profiles.append(build(n, inventory))
        except Exception as exc:   # one plug-in's unmet structural fact must not hide the rest
            failed[n] = str(exc)[:200]
    doc = dict(schema='openapollo-batch-discovery/1', profiles=profiles, counts=dict(selected=len(names), discovered=len(profiles), failed=len(failed)),
               failed=failed, source_sha256=sha(__file__))
    args.output.write_text(json.dumps(doc, indent=2) + '\n')
    print(json.dumps(dict(output=str(args.output), profiles=[p['name'] for p in profiles], failed=failed)))


if __name__ == '__main__':
    main()
