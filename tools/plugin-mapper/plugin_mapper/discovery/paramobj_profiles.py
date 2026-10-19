"""Static profile discovery for the UAD plug-in whose parameters are JUCE `AudioProcessorParameter` objects driven through the
original VST2 wrapper (executor/paramobj_capture.py): Auto-Tune Realtime X.

Sibling of vstmain_profiles.py: same output envelope ({'schema': 'openapollo-batch-discovery/1', 'profiles': [...]}) consumable by
`catalog-execution/run_catalog.py --profiles <file>`, profile schema `openapollo-dspaudioprocessor-profile/1` with a `dspap.vstmain` block
(the VST2 entry builds the whole object graph) plus a `dspap.paramobjects` block that the executor consumes. Never loads or executes
vendor code.

Derived from the image and re-verified on every run:
  * identity: DLL and capture hashes against families/inventory.json, render id against the manifest;
  * the CUADPlugCallbackHandler: the `AutoTunePluginAudioProcessor` RTTI sub-object at +0x148 (19+ slots), whose slot 3 is the function that
    fills a 0xae8-byte AllocInfo with the manifest render id (the inventory's own descriptor entry for this DLL is a rejected candidate);
  * the replaced original routine: the one function that uses the five MSVCP140 iostream imports (basic_streambuf ctor, locale::classic,
    basic_ostream ctor, operator<<(double), basic_ostream vbase dtor) is juce::String::String(double, int, bool); its head bytes and the
    `String(const char*, size_t)` constructor it ends in (mov r8,rbx / lea rdx,[rbp] / mov rcx,rdi / call) are located here;
  * the Console schema from the capture (names, defaults, texts, step lists).
Pinned from a probe of the plug-in under the executor (scratch/probe, 2026-10-18, 6001-point sweep of AEffect.setParameter then getParameter
for every parameter): the integer domain of each parameter is its number of plateaus minus one; the two continuous parameters (Detune and
Retune Speed keep every swept value) use the 32768 grid every family uses for continuous ones. The executor reads every value back
after setting it, so a wrong domain fails the run.

Usage: python3 paramobj_profiles.py [--output profiles-paramobj.json]
"""
from pathlib import Path
import argparse, json, struct
from capstone.x86 import X86_OP_IMM, X86_OP_MEM
from dspaudioprocessor_profiles import Image, sha, DLL_ROOT, CAPTURES, PATTERNS, SCHEMA, captured_parameters, console_text, text_or_none
import vstmain_profiles as V

HERE = Path(__file__).resolve().parent
NAME = 'UAD Auto-Tune Realtime X'
CONTINUOUS = 32768
ALLOC_INFO_SIZE = 0xae8
BUDGET = 500_000_000            # the whole construction (DllMain, VSTPluginMain, resume, startup) is ~3e7 instructions
MSVCP_IOSTREAM = (b'??0?$basic_streambuf@DU?$char_traits@D@std@@@std@@IEAA@XZ', b'?classic@locale@std@@SAAEBV12@XZ',
                  b'??0?$basic_ostream@DU?$char_traits@D@std@@@std@@QEAA@PEAV?$basic_streambuf@DU?$char_traits@D@std@@@1@_N@Z',
                  b'??6?$basic_ostream@DU?$char_traits@D@std@@@std@@QEAAAEAV01@N@Z', b'??_D?$basic_ostream@DU?$char_traits@D@std@@@std@@QEAAXXZ')
KNOWN = {
    NAME: dict(sha256='c9a71eb51fb6', render_id=0x8ef, handler=dict(cls='AutoTunePluginAudioProcessor', offset=0x148), latency=0,
               # plateaus of the stored value per parameter (see the module docstring): number of distinct getParameter results - 1
               maxima={0: 3, 1: 11, 2: 28, 4: 100, 6: 100, 7: 240, 8: 1, 9: 1, 10: 1, 11: 3, 12: 99, 13: 100, 14: 1500, 15: 1500, 16: 100,
                       17: 30, 18: 100, 19: 99, 20: 1, 21: 1, 22: 1, 23: 1, 24: 1, 25: 1, 26: 1, 27: 100, 28: 1}),
}


def importers(img, names):
    """{function start: set of imported names it calls through the IAT}."""
    pe = img.pe
    pe.parse_data_directories(directories=[1])
    iat = {imp.address: imp.name for entry in pe.DIRECTORY_ENTRY_IMPORT for imp in entry.imports if imp.name in names}
    text = next(s for s in pe.sections if s.Name.startswith(b'.text'))
    lo, hi = text.VirtualAddress, text.VirtualAddress + text.Misc_VirtualSize
    out = {}
    for pos in range(lo, hi - 6):
        if img.im[pos] == 0xff and img.im[pos + 1] in (0x15, 0x25):
            target = img.base + pos + 6 + struct.unpack_from('<i', img.im, pos + 2)[0]
            if target in iat:
                out.setdefault(img.function(img.base + pos)[0], set()).add(iat[target])
    return out


def string_from_double(img):
    """The replaced original routine and its tail-call target (see the module docstring)."""
    users = importers(img, set(MSVCP_IOSTREAM))
    if len(users) != 1 or next(iter(users.values())) != set(MSVCP_IOSTREAM):
        raise ValueError('the five iostream imports must belong to exactly one function: ' + repr({hex(k): sorted(v) for k, v in users.items()}))
    start, end, ins = img.function(next(iter(users)))
    text = [(i.mnemonic, i.op_str) for i in ins]
    if ('shl', 'eax, 0xc') not in text or ('add', 'eax, 0x1000') not in text:
        raise ValueError('float-field flag computation (fixed 0x2000 / scientific 0x1000) differs')
    tails = [ins[k + 3].operands[0].imm for k in range(len(ins) - 3)
             if [(i.mnemonic, i.op_str) for i in ins[k:k + 3]] == [('mov', 'r8, rbx'), ('lea', 'rdx, [rbp]'), ('mov', 'rcx, rdi')]
             and ins[k + 3].mnemonic == 'call' and ins[k + 3].operands[0].type == X86_OP_IMM]
    if len(tails) != 1:
        raise ValueError('String(const char*, size_t) tail call not unique')
    return dict(name='juce_string_from_double', va=hex(start), tail_va=hex(tails[0]), head=img.im[start - img.base:start - img.base + 12].hex(),
                size=end - start, imports=[n.decode() for n in MSVCP_IOSTREAM])


def build(name, inventory):
    known = KNOWN[name]
    item = inventory[name]
    dll = DLL_ROOT / (name + '.dll')
    if sha(dll) != item['dll_sha256'] or not item['dll_sha256'].startswith(known['sha256']):
        raise ValueError('original DLL identity differs')
    if item['render_id_manifest'] != known['render_id']:
        raise ValueError('render id differs from the manifest')
    capture = CAPTURES / (item['capture'] + '.json')
    img = Image(dll)
    props = captured_parameters(capture)
    count = item['parameter_count']
    if sorted(props) != list(range(count)):
        raise ValueError('captured parameter schema differs')
    # --- the handler (CUADPlugCallbackHandler sub-object) ------------------------------------------------------------
    handlers = [(off, vt, img.vtable(vt, 64)) for off, vts in V.class_vtables(img, known['handler']['cls']).items() for vt in vts
                if off == known['handler']['offset']]
    handlers = [h for h in handlers if len(h[2]) >= 19]
    if len(handlers) != 1:
        raise ValueError('unique handler sub-object at the pinned offset required')
    offset, vtable, slots = handlers[0]
    getter = slots[V.SLOTS['descriptor']]
    renders = [i for i in img.function(getter)[2] if i.mnemonic == 'mov' and len(i.operands) == 2 and i.operands[0].type == X86_OP_MEM
               and i.operands[0].mem.disp == 0xaa4 and i.operands[1].type == X86_OP_IMM and i.operands[1].imm == known['render_id']]
    if not V.uses_immediate(img, getter, ALLOC_INFO_SIZE) or len(renders) != 1:
        raise ValueError('handler slot 3 is not the descriptor getter (AllocInfo size / render id at +0xaa4)')
    # --- Console schema ---------------------------------------------------------------------------------------------
    params, text = [], []
    for i in range(count):
        p = props[i]
        steps = p.get('StepValue', {}).get('values')
        maximum = known['maxima'].get(i, CONTINUOUS)
        if steps and len(steps) - 1 != maximum:
            raise ValueError('captured step list differs from the probed domain of ' + p['Name']['value'])
        norm = p['NormalizedValue']['value']
        default = round(norm * maximum)
        if not 0 <= default <= maximum or abs(default / maximum - norm) > 0.5 / maximum * (1 + 1e-6):
            raise ValueError('captured default is off the integer grid for ' + p['Name']['value'])
        params.append(dict(name=p['Name']['value'], maximum=maximum, default=default, steps=len(steps) if steps else 0))
        text.append(console_text(p))
    latency = [c['value'] for c in json.loads(capture.read_text())['controls'] if c['path'].endswith('/Latency')]
    if latency and latency != [known['latency']]:
        raise ValueError('captured latency differs from the pinned one')
    return dict(
        schema=SCHEMA, status='discovered', name=name, framework='DSPAudioProcessor',
        dll=dict(path=str(dll), sha256=item['dll_sha256']),
        captured_metadata=dict(path=str(capture), sha256=sha(capture)),
        dspap=dict(
            handler=dict(offset=hex(offset), vtable=hex(vtable), slots={hex(8 * i): hex(v) for i, v in enumerate(slots[:19])}),
            descriptor=dict(getter_va=hex(getter), inner_va=None, render_id=known['render_id'],
                            evidence='handler slot 3 = the descriptor getter (AllocInfo 0xae8, render id at +0xaa4 equals the manifest)'),
            formatter=dict(slot=hex(8 * V.SLOTS['formatter']), va=hex(slots[V.SLOTS['formatter']])),
            startup=[dict(slot=hex(8 * V.SLOTS['startup']), va=hex(slots[V.SLOTS['startup']]), arguments=['handler'])],
            parameters=params, captured_text=text_or_none(text), plain_fields={}, quiet_iface_slots=[],
            vstmain=dict(interface_version=0x67, host=V.HOST, instruction_budget=BUDGET, latency_samples=known['latency'],
                         evidence='RTTI complete-object locators of the handler class; families-dspaudioprocessor.md section 9'),
            paramobjects=dict(
                text_source='VST2 effGetParamDisplay through the original JuceVSTWrapper (the parameter objects own text; handler slot 15 is the plug-in bar short text)',
                host_services=[string_from_double(img)],
                domain_evidence='AEffect.setParameter/getParameter sweep, 6001 points per parameter; stepped parameters keep (maximum + 1) plateaus, Detune and Retune Speed keep every point',
                maxima={str(k): v for k, v in sorted(known['maxima'].items())}),
            evidence=dict(source='families-dspaudioprocessor.md section 9', capture_text_corrections=[])),
    )


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--output', type=Path, default=HERE / 'profiles-paramobj.json')
    args = ap.parse_args()
    inventory = {p['name']: p for p in json.loads((PATTERNS / 'families/inventory.json').read_text())['plugins']}
    profiles = [build(n, inventory) for n in sorted(KNOWN)]
    doc = dict(schema='openapollo-batch-discovery/1', profiles=profiles, counts=dict(selected=len(profiles), discovered=len(profiles)),
               source_sha256=sha(__file__))
    args.output.write_text(json.dumps(doc, indent=2) + '\n')
    print(json.dumps(dict(output=str(args.output), profiles=[p['name'] for p in profiles])))


if __name__ == '__main__':
    main()
