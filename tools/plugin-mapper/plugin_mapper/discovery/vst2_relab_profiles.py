"""Static profile discovery for the UAD `UAD2AGain` / `ParameterHandler` / `IUADPlugRelab` template (sub-family C: UAD Lexicon 480L).

Sibling of vst2_profiles.py / vst2_bx_profiles.py (same envelope and schema; `vst2.variant = 'relab'` selects executor/vst2_relab_host.py).
Never loads or executes vendor code. Re-verified on every run: DLL/capture hashes, the PE export `VSTPluginMain` and entry point, the
`UAD2AGain` primary vtable and the CUADPlugCallbackHandler base sub-object vtable (RTTI), handler slot 3 = the inventory descriptor getter
(and the render id immediate in it), pinned prologues of the intercepted functions and the captured Console names/defaults against the grid.

Usage: python3 vst2_relab_profiles.py [--output profiles-vst2-relab.json]
"""
from pathlib import Path
import argparse, json, struct
from dspaudioprocessor_profiles import Image, DLL_ROOT, CAPTURES, PATTERNS, sha, console_text, text_or_none
from vst2_bx_profiles import rtti_vtable, COMMON, CONTINUOUS_GRID

HERE = Path(__file__).resolve().parent
SCHEMA = 'openapollo-vst2-iuadplug-profile/1'

KNOWN = {
    'UAD Lexicon 480L': dict(
        COMMON, sha256='b5473c54', interface_version=0x67, handler_offset=0xf0, iface_offset=0x5290,
        iface_answers={0x28: 'host_channels', 0x30: 0, 0x138: 0, 0x160: 0, 0x168: 0},   # +0x28 channel count; validity query (0 = valid) after UAD_CreateIUADPlug; Resume (+0x160) / Suspend (+0x168) notifications
        quiet_iface_slots=[0x118],               # flush call in the push; no DSP content
        heap=dict(malloc=0x18000fc78, free=0x18000fc38, realloc=0x18001cff8),
        inactive=dict(va=0x180055fb0, receiver_offset=0xf8),   # ParameterHandler::isInactive(index) on the handler at this+0xf8
        no_editor=0x18006f6d0,                   # editor factory the constructor hands to AudioEffect::setEditor
        steps=None),
}


def build(name, inventory):
    known = KNOWN[name]
    item = inventory[name]
    dll = DLL_ROOT / (name + '.dll')
    if sha(dll) != item['dll_sha256'] or not item['dll_sha256'].startswith(known['sha256']):
        raise ValueError('original DLL identity differs')
    capture = CAPTURES / (item['capture'] + '.json')
    img = Image(dll)
    img.pe.parse_data_directories(directories=[0])
    exports = {e.name.decode(): img.base + e.address for e in img.pe.DIRECTORY_ENTRY_EXPORT.symbols if e.name}
    entry = exports.get('VSTPluginMain')
    if entry is None or exports.get('main') != entry:
        raise ValueError('VST2 entry points VSTPluginMain/main missing or different')
    dllmain = img.base + img.pe.OPTIONAL_HEADER.AddressOfEntryPoint
    processor_vt = rtti_vtable(img, 'UAD2AGain')
    descriptor = item['discovery']['descriptors'][0]
    getter = int(descriptor['function']['va'], 16)
    sub = descriptor['descriptor_subobject_vtables'][0]
    handler_vt = int(sub['va'], 16)
    if sub['slot'] != '0x18' or sub['rtti']['subobject_offset'] != known['handler_offset']:
        raise ValueError('inventory descriptor receiver is not the UAD2AGain handler sub-object slot 3')
    slots = img.vtable(handler_vt, 32)
    if slots[3] != getter or len(img.vtable(processor_vt, 128)) < 40:
        raise ValueError('handler slot 3 is not the discovered descriptor getter / implausible vtables')
    _, _, ins = img.function(getter)
    render = [i.operands[1].imm for i in ins if i.mnemonic == 'mov' and len(i.operands) == 2 and i.operands[0].type == 3
              and i.operands[0].mem.disp == 0xaa4]
    if render != [item['render_id_manifest']] or descriptor['render_id'] != item['render_id_manifest']:
        raise ValueError('descriptor render id differs from the manifest ' + repr(render))
    intercepts = []
    for kind, va in sorted(known['heap'].items()) + [('no_editor', known['no_editor'])]:
        start, end, _ = img.function(va)
        if start != va or end - start < 16:
            raise ValueError('intercept target is not a function start ' + kind)
        o = start - img.base
        intercepts.append(dict(kind=kind, va=hex(start), size=end - start, prologue=img.im[o:o + 16].hex()))
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
    maxima = known.get('maxima') or [(len(props[i].get('StepValue', {}).get('values') or []) or CONTINUOUS_GRID + 1) - 1 for i in range(count)]
    params, text = [], []
    for i in range(count):
        maximum = maxima[i]
        norm = props[i]['NormalizedValue']['value']
        default = round(norm * maximum)
        error = abs(struct.unpack('<f', struct.pack('<f', default / maximum))[0] - norm)
        if known.get('maxima') and error > 0.5 / maximum + 1e-6:   # bootstrap (no pinned maxima yet) takes the nearest step
            raise ValueError('captured default is not on the grid for parameter %d' % i)
        params.append(dict(name=props[i]['Name']['value'], maximum=maximum, default=default, exact=False, steps=0,   # half-step tolerance: Program select loads arbitrary stored floats into the other parameters
                           
                           grid_source='grid %d' % maximum))
        text.append(console_text(props[i]))
    vst2 = dict(
        variant='relab', family='UAD2AGain / ParameterHandler / IUADPlugRelab template', entry_va=hex(entry), dllmain_va=hex(dllmain),
        interface_version=hex(known['interface_version']), aeffect_offset=hex(known['aeffect_offset']), processor_vtable=hex(processor_vt),
        handler=dict(offset=hex(known['handler_offset']), vtable=hex(handler_vt), slots={hex(8 * i): hex(v) for i, v in enumerate(slots[:18])}),
        iface_offset=hex(known['iface_offset']),
        descriptor=dict(getter_va=hex(getter), render_store_va=descriptor['render_store_va'], render_id=descriptor['render_id'], evidence=descriptor['status']),
        iface_answers={hex(k): v for k, v in known['iface_answers'].items()}, quiet_iface_slots=[hex(x) for x in known['quiet_iface_slots']],
        inactive=dict(va=hex(known['inactive']['va']), receiver_offset=hex(known['inactive']['receiver_offset'])),
        host=known['host'], module_path=known['module_path'] % name, intercepts=intercepts, parameters=params, captured_text=text_or_none(text),
        text_waivers={}, evidence=dict(source='families-vst2-iuadplug.md'))
    return dict(schema=SCHEMA, status='discovered', name=name, framework='VST2IUADPlug', dll=dict(path=str(dll), sha256=item['dll_sha256']),
                captured_metadata=dict(path=str(capture), sha256=sha(capture)), vst2=vst2)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--output', type=Path, default=HERE / 'profiles-vst2-relab.json')
    args = ap.parse_args()
    inventory = {p['name']: p for p in json.loads((PATTERNS / 'families/inventory.json').read_text())['plugins']}
    profiles = [build(n, inventory) for n in sorted(KNOWN)]
    doc = dict(schema='openapollo-batch-discovery/1', profiles=profiles, counts=dict(selected=len(profiles), discovered=len(profiles)),
               source_sha256=sha(__file__))
    args.output.write_text(json.dumps(doc, indent=2) + '\n')
    print(json.dumps(dict(output=str(args.output), profiles=[p['name'] for p in profiles])))


if __name__ == '__main__':
    main()
