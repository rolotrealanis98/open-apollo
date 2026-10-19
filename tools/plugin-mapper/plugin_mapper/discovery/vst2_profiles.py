"""Static profile discovery for the UAD VST2 `AudioEffect` + `IUADPlug` family, sub-family A (UAD AMS RMX16 first).

Sibling of dspaudioprocessor_profiles.py: same envelope ({'schema': 'openapollo-batch-discovery/1', 'profiles': [...]}),
consumable by `catalog-execution/run_catalog.py --profiles <file>`. Never loads or executes vendor code.

Everything address-shaped is derived from the image on every run (no per-plug-in address table):
  * RTTI complete-object locators: the effect class (an `AudioEffect` descendant with a `CUADPlugCallbackHandler`
    sub-object), its primary vtable, the handler sub-object offset and vtable (classify_vst2_iuadplug.py);
  * `VSTPluginMain` (PE export, same VA as `main`): the allocator is the call after the audioMaster handshake, the
    constructor the next call;
  * constructor: the `UAD_CreateIUADPlug` IAT call gives the interface version (r9d), the AEffect offset (rdx), the
    handler offset (r8, must equal RTTI) and the IUADPlug slot (`mov [this+d], rax`); the call after the second
    allocator call is the VSTGUI editor constructor and its result slot is the editor offset;
  * `setParameter` (effect vtable slot 10) and its single push function: the one direct callee that writes IUADPlug
    set_int/set_float/set_float_array (slots 0xa0/0xa8/0xb8); `resume` = vtable slot 5 (mainsChanged(1));
  * handler slot 3 = the inventory descriptor getter (and the render id immediate in it), slot 15 = the formatter, and
    `sprintf` = the call target the formatter feeds with C format strings;
  * the pinned prologue of every function the host intercepts, the display-quantiser constants that fix each
    continuous parameter's integer grid (read from the code that uses them), step-list grids from the Console capture.
Refusals: any step that is not unique or does not have the expected shape raises, so a plug-in outside the
single-push shape (e.g. Auto-Tune Realtime's parameter-object setter) is not silently profiled.

Usage: python3 vst2_profiles.py [--plugin NAME ...] [--output profiles-vst2.json]
"""
from pathlib import Path
import argparse, collections, json, struct
from capstone.x86 import X86_OP_IMM, X86_OP_MEM
from dspaudioprocessor_profiles import Image, DLL_ROOT, CAPTURES, PATTERNS, sha, console_text, text_or_none
from classify_vst2_iuadplug import cols, vtables, fragments, code, follow, direct_calls, writes_iface, HANDLER, effect_class, push_function, survey

HERE = Path(__file__).resolve().parent
SCHEMA = 'openapollo-vst2-iuadplug-profile/1'

# VST SDK 2.x layout and host-visible semantics shared by the whole sub-family (not per plug-in).
FAMILY = dict(
    editor_offset=0x10,                       # AudioEffect::editor
    formatter_slot=0x78,                      # handler slot 15 (this, index, float normalized, char* out)
    iface_answers={0x30: 0, 0x88: 0},         # IUADPlug queries: +0x30 selects a load-unit variant below 44.1 kHz, +0x88 is editor-only
    quiet_iface_slots=[0x118],                # void flush call in the push; no DSP content
    host=dict(sample_rate=48000.0, block_size=32),
)

# Per effect class (RTTI name): parameter name, grid maximum (None = the Console step list), display-quantiser float that
# fixes the grid (None for step lists). Decay uses grid 1000 so the constructor default 0.404 stays exact; its display step is 1/99.
GRIDS = {
    'RMX16': [('Input Gain', 100, 100.0), ('Output Gain', 100, 100.0), ('Program', None, None), ('Pre-delay-A', 1200, 1200.0),
              ('Decay-A', 1000, 99.0), ('DecayLO-A', None, None), ('DecayHI-A', None, None), ('Pre-delay-B', 1200, 1200.0),
              ('Decay-B', 1000, 99.0), ('DecayLO-B', None, None), ('DecayHI-B', None, None), ('Wet/Dry', 1000, 1000.0),
              ('Wet Solo', None, None), ('Power', None, None)],
}


# ---- image helpers live in classify_vst2_iuadplug.py (shared with the classifier) ----

def rip_target(i, k=0):
    o = i.operands[k]
    return i.address + i.size + o.mem.disp if o.type == X86_OP_MEM and i.reg_name(o.mem.base) == 'rip' else None


def derive(img, item, dll_name):
    """Every structural fact the host needs, or ValueError naming the first step that is not the sub-family A shape."""
    frag = fragments(img)
    vt = vtables(img, cols(img))
    effect = effect_class(vt)
    (processor_vtable,) = [v['vtable'] for v in vt if v['type'] == effect and v['offset'] == 0]
    handlers = [v for v in vt if v['type'] == effect and v['offset'] > 0 and HANDLER in v['bases']]
    if len(handlers) != 1:
        raise ValueError('effect class has no unique handler sub-object')
    handler_offset, handler_vtable = handlers[0]['offset'], handlers[0]['vtable']
    primary, slots = img.vtable(processor_vtable, 128), img.vtable(handler_vtable, 32)
    if len(primary) < 40 or len(slots) < 16:
        raise ValueError('implausible vtables')
    # --- VSTPluginMain: audioMaster handshake, allocator, constructor -------------------------------------------------
    img.pe.parse_data_directories(directories=[0])
    exports = {e.name.decode(): img.base + e.address for e in img.pe.DIRECTORY_ENTRY_EXPORT.symbols if e.name}
    entry = exports.get('VSTPluginMain')
    if entry is None or exports.get('main') != entry:
        raise ValueError('VST2 entry points VSTPluginMain/main missing or different')
    _, _, ins = img.function(entry)
    master = next(k for k, i in enumerate(ins) if i.mnemonic == 'call' and i.operands[0].type == 1)   # call reg: audioMaster
    after = [i for i in ins[master + 1:] if i.mnemonic == 'call' and i.operands[0].type == X86_OP_IMM]
    if len(after) < 2 or ins[ins.index(after[0]) - 1].mnemonic != 'mov':
        raise ValueError('VSTPluginMain is not `audioMaster; new(size); ctor`')
    new_va, ctor = after[0].operands[0].imm, after[1].operands[0].imm
    # --- constructor: UAD_CreateIUADPlug call, IUADPlug slot, editor constructor ------------------------------------
    iat = [imp.address for d in img.pe.DIRECTORY_ENTRY_IMPORT for imp in d.imports if imp.name and imp.name.startswith(b'?UAD_CreateIUADPlug@')]
    if len(iat) != 1:
        raise ValueError('UAD_CreateIUADPlug import missing')
    start, _, _ = img.function(ctor)
    cins = code(img, start, frag)
    site = [k for k, i in enumerate(cins) if i.mnemonic == 'call' and rip_target(i) == iat[0]]
    if len(site) != 1:
        raise ValueError('constructor does not call UAD_CreateIUADPlug exactly once')
    k = site[0]
    def lea(reg):
        return next(i for i in reversed(cins[max(0, k - 12):k]) if i.mnemonic == 'lea' and i.reg_name(i.operands[0].reg) == reg)
    version = next(i.operands[1].imm for i in reversed(cins[max(0, k - 12):k]) if i.mnemonic == 'mov' and i.reg_name(i.operands[0].reg) == 'r9d')
    aeffect_lea, handler_lea = lea('rdx'), lea('r8')
    if handler_lea.operands[1].mem.disp != handler_offset or handler_lea.operands[1].mem.base != aeffect_lea.operands[1].mem.base:
        raise ValueError('constructor handler argument differs from the RTTI sub-object')
    this = aeffect_lea.operands[1].mem.base
    store = next(i for i in cins[k + 1:k + 6] if i.mnemonic == 'mov' and i.operands[0].type == X86_OP_MEM and i.operands[0].mem.base == this
                 and i.reg_name(i.operands[1].reg) == 'rax')
    iface_offset = store.operands[0].mem.disp
    news = [j for j in range(k + 1, len(cins)) if cins[j].mnemonic == 'call' and cins[j].operands[0].type == X86_OP_IMM
            and cins[j].operands[0].imm == new_va]
    gui = next(i for i in cins[news[-1] + 1:] if i.mnemonic == 'call' and i.operands[0].type == X86_OP_IMM)
    editor = next(i for i in cins[cins.index(gui) + 1:] if i.mnemonic == 'mov' and i.operands[0].type == X86_OP_MEM and i.operands[0].mem.base == this)
    if editor.operands[0].mem.disp != FAMILY['editor_offset']:
        raise ValueError('editor slot differs from AudioEffect::editor')
    # --- setParameter -> one push function; resume ---------------------------------------------------------------------
    set_parameter = follow(img, primary[10])
    push = push_function(img, vt, effect, frag)
    resume = follow(img, primary[5])
    # --- formatter and sprintf -------------------------------------------------------------------------------------
    formatter = follow(img, slots[FAMILY['formatter_slot'] // 8])
    f_start, _, _ = img.function(formatter)
    f_ins = code(img, f_start, frag)
    formats = collections.Counter()
    for j, i in enumerate(f_ins):
        if i.mnemonic == 'call' and i.operands[0].type == X86_OP_IMM:
            for p in f_ins[max(0, j - 8):j]:
                t = rip_target(p, 1) if p.mnemonic == 'lea' and p.reg_name(p.operands[0].reg) == 'rdx' else None
                if t and img.inside(t) and b'%' in img.im[t - img.base:t - img.base + 24].split(b'\0')[0]:
                    formats[i.operands[0].imm] += 1
    if not formats:
        raise ValueError('formatter feeds no C format string to any function')
    sprintf = formats.most_common(1)[0][0]
    # --- editor-free host intercepts: allocator, editor constructor, sprintf ----------------------------------------
    # Display quantisers: float constants read through rip-relative SSE operands by the effect's code (parameter helpers and
    # editor display code), or the integer scale the code compares against (Pre-delay's program scale 1200 is an immediate).
    floats, ints = {}, {}
    for rva in frag:
        for i in code(img, img.base + rva, frag):
            for k2, o in enumerate(i.operands):
                t = rip_target(i, k2)
                if t and img.inside(t, 4) and i.mnemonic in ('movss', 'mulss', 'divss', 'addss', 'subss', 'comiss', 'ucomiss', 'cvtss2sd'):
                    floats[round(img.f32(t), 6)] = min(t, floats.get(round(img.f32(t), 6), t))
                elif o.type == X86_OP_IMM and i.mnemonic in ('cmp', 'mov'):
                    ints.setdefault(o.imm, i.address)
    consts = dict(floats=floats, ints=ints)
    return dict(effect=effect[4:-2], entry=entry, processor_vtable=processor_vtable, handler_offset=handler_offset,
                handler_vtable=handler_vtable, interface_version=version, aeffect_offset=aeffect_lea.operands[1].mem.disp,
                iface_offset=iface_offset, editor_offset=FAMILY['editor_offset'], resume_va=resume, push_va=push,
                set_parameter=set_parameter, formatter=formatter, intercepts=[dict(kind='new', va=new_va),
                dict(kind='gui_constructor', va=gui.operands[0].imm), dict(kind='sprintf', va=sprintf)], consts=consts)


def build(name, inventory):
    item = inventory[name]
    dll = DLL_ROOT / (name + '.dll')
    if sha(dll) != item['dll_sha256']:
        raise ValueError('original DLL identity differs')
    capture = CAPTURES / (item['capture'] + '.json')
    img = Image(dll)
    facts = derive(img, item, name)
    grid = GRIDS.get(facts['effect'])
    if grid is None:
        raise ValueError('no parameter grid for effect class ' + facts['effect'])
    # --- vtables, descriptor getter, render id ----------------------------------------------------------------
    descriptor = item['discovery']['descriptors'][0]
    getter = int(descriptor['function']['va'], 16)
    sub = descriptor['descriptor_subobject_vtables'][0]
    if (int(sub['va'], 16), sub['slot']) != (facts['handler_vtable'], '0x18'):
        raise ValueError('inventory descriptor vtable differs from the RTTI handler vtable')
    slots = img.vtable(facts['handler_vtable'], 32)
    if slots[3] != getter:
        raise ValueError('handler slot 3 is not the discovered descriptor getter')
    _, _, ins = img.function(getter)
    render = [i.operands[1].imm for i in ins if i.mnemonic == 'mov' and len(i.operands) == 2 and i.operands[0].type == 3
              and i.operands[0].mem.disp == 0xaa4]
    if render != [item['render_id_manifest']] or descriptor['render_id'] != item['render_id_manifest']:
        raise ValueError('descriptor render id differs from the manifest ' + repr(render))
    # --- intercepted functions: prologue pinned ----------------------------------------------------------------
    intercepts = []
    for entry_spec in facts['intercepts']:
        start, end, _ = img.function(entry_spec['va'])
        if start != entry_spec['va'] or end - start < 16:
            raise ValueError('intercept target is not a function start')
        o = start - img.base
        intercepts.append(dict(kind=entry_spec['kind'], va=hex(start), size=end - start, prologue=img.im[o:o + 16].hex()))
    # --- parameters: grid from step lists or the original quantiser constants; defaults/text from the capture -----
    cap = json.loads(capture.read_text())
    props = {}
    for c in cap['controls']:
        p = c['path'].split('/parameters/')
        if len(p) == 2:
            idx, field = p[1].split('/')
            props.setdefault(int(idx), {})[field] = c
    count = len(grid)
    if sorted(props) != list(range(count)) or item['parameter_count'] != count:
        raise ValueError('captured parameter schema differs')
    params, text = [], []
    for i, (pname, maximum, quantiser) in enumerate(grid):
        if props[i]['Name']['value'] != pname:
            raise ValueError('captured parameter name differs ' + pname)
        if quantiser is None:
            steps = props[i].get('StepValue', {}).get('values')
            if not steps:
                raise ValueError('captured step list missing for ' + pname)
            maximum, origin = len(steps) - 1, 'captured StepValue list'
        else:
            where, scale = facts['consts']['floats'].get(round(quantiser, 6)), facts['consts']['ints'].get(int(quantiser))
            if where:
                origin = 'display quantiser %s = %g; grid %d keeps the constructor default exact' % (hex(where), quantiser, maximum)
            elif scale and maximum == int(quantiser):
                origin = 'display scale %d compared at %s; grid %d keeps the constructor default exact' % (quantiser, hex(scale), maximum)
            else:
                raise ValueError('original display quantiser constant %g not used by the effect code (%s)' % (quantiser, pname))
        norm = props[i]['NormalizedValue']['value']
        default = round(norm * maximum)
        if abs(default / maximum - norm) > 1.0 / (2 * maximum) + 1e-6:
            raise ValueError('captured default is not on the grid for ' + pname)
        params.append(dict(name=pname, maximum=maximum, default=default, grid_source=origin, exact=quantiser is not None))
        text.append(console_text(props[i]))
    vst2 = dict(
        entry_va=hex(facts['entry']), interface_version=hex(facts['interface_version']), aeffect_offset=hex(facts['aeffect_offset']),
        processor_vtable=hex(facts['processor_vtable']),
        handler=dict(offset=hex(facts['handler_offset']), vtable=hex(facts['handler_vtable']),
                     slots={hex(8 * i): hex(v) for i, v in enumerate(slots[:16])}),
        iface_offset=hex(facts['iface_offset']), editor_offset=hex(facts['editor_offset']),
        descriptor=dict(getter_va=hex(getter), render_store_va=descriptor['render_store_va'], render_id=descriptor['render_id'],
                        evidence=descriptor['status']),
        resume_va=hex(facts['resume_va']), push_va=hex(facts['push_va']),
        formatter=dict(slot=hex(FAMILY['formatter_slot']), va=hex(slots[FAMILY['formatter_slot'] // 8])),
        iface_answers={hex(k): v for k, v in FAMILY['iface_answers'].items()}, quiet_iface_slots=[hex(x) for x in FAMILY['quiet_iface_slots']],
        host=FAMILY['host'], intercepts=intercepts, parameters=params, captured_text=text_or_none(text),
        evidence=dict(source='families-vst2-iuadplug.md'))
    return dict(schema=SCHEMA, status='discovered', name=name, framework='VST2IUADPlug',
                dll=dict(path=str(dll), sha256=item['dll_sha256']),
                captured_metadata=dict(path=str(capture), sha256=sha(capture)), vst2=vst2)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--plugin', action='append')
    ap.add_argument('--output', type=Path, default=HERE / 'profiles-vst2.json')
    args = ap.parse_args()
    inventory = {p['name']: p for p in json.loads((PATTERNS / 'families/inventory.json').read_text())['plugins']}
    names = args.plugin or sorted(n for n, r in survey().items() if r['sub_family'] == 'A')
    profiles = [build(n, inventory) for n in names]
    doc = dict(schema='openapollo-batch-discovery/1', profiles=profiles, counts=dict(selected=len(names), discovered=len(profiles)),
               source_sha256=sha(__file__))
    args.output.write_text(json.dumps(doc, indent=2) + '\n')
    print(json.dumps(dict(output=str(args.output), profiles=[p['name'] for p in profiles])))


if __name__ == '__main__':
    main()
