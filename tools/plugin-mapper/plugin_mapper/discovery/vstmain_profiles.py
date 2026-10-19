"""Static profile discovery for the UAD plug-ins whose original VST2 entry builds the whole effect (executor/vstmain_capture.py):
the JUCE 3.2 Sphere Mic Collection family (`SphereUADProcessor`) and the Sonnox "Soup" plug-ins that are not driven through the
synthetic sonnox_capture.py (Oxford Dynamic EQ, SuprEsser DS: JUCE 7.0.3 `JuceVSTWrapper`).

Sibling of dspaudioprocessor_profiles.py / sonnox_profiles.py: same output envelope ({'schema': 'openapollo-batch-discovery/1',
'profiles': [...]}) consumable by `catalog-execution/run_catalog.py --profiles <file>`, profile schema
`openapollo-dspaudioprocessor-profile/1` plus a `dspap.vstmain` block that executor/vstmain_capture.py consumes. Never loads or
executes vendor code; the executor then runs the original DllMain and VSTPluginMain, so no object layout is declared here.

Derived from the image and re-verified on every run:
  * identity: DLL and capture hashes against families/inventory.json, render id against the manifest;
  * the CUADPlugCallbackHandler: the RTTI complete-object locators give every sub-object vtable. Sphere: the `SphereUADProcessor`
    one with >= 19 slots whose slot 3 (through the thunk) is the function that clears and fills a 0xae8-byte AllocInfo (the
    inventory's own descriptor sub-object is the 2-slot vtable behind it, which slot 3 calls). JUCE wrapper: the 19-slot
    `JuceVSTWrapper` sub-object at +0x128 (the wrapper constructor passes `this+0x128` to UAD_CreateIUADPlug; the executor checks it);
  * the Console schema (names, step lists, defaults, text) from the capture: stepped parameters have step count - 1 as integer
    domain, continuous ones 32768 (the grid every family uses; a plug-in default like 0.1f is the nearest grid point, within half a
    step); the executor checks each against the plug-in's own getParameter.
Pinned (reverse engineered, each cross-checked above or at run time): the Soup processors' JUCE latencySamples field (+0x3c of the
processor the wrapper holds at handler+8; equals the captured Console latency, checked at run time), handler slot numbers (3 descriptor, 15 text, 17 startup),
interface version 0x67, host sample rate / block size, the instruction budget of the original construction.

Usage: python3 vstmain_profiles.py [--plugin NAME ...] [--output profiles.json]
"""
from pathlib import Path
import argparse, json, os, re, struct
from dspaudioprocessor_profiles import Image, sha, DLL_ROOT, CAPTURES, PATTERNS, SCHEMA, captured_parameters, console_text, text_or_none
from capstone.x86 import X86_OP_IMM

HERE = Path(__file__).resolve().parent
CONTINUOUS_DOMAIN = 32768
ALLOC_INFO_SIZE = 0xae8

# Ocean Way: the studio catalog is an XML tree walked with an O(n^2) child accessor (5M emulated allocations); pinned to the image bytes, replaced by the host (vstmain_capture.native_xml_nth_child)
NATIVE_OCEAN = [
{
    "name": "xml_nth_child",
    "va": "0x180119060",
    "size": 172,
    "sha256": "33d8f65e76194d79c44aabd47c991a73c1b212ee2b20ebd59d704401b768917c",
    "literal_va": "0x180178b28",
    "literal": "ImpulseResponse",
    "callees": [
        {
            "va": "0x180013210",
            "size": 70,
            "sha256": "45c90b6fb5fd9e2f6a502f802b31e9bc4566d56474cf49b919ae733a20c06b9b"
        },
        {
            "va": "0x180012dc0",
            "size": 70,
            "sha256": "f8f1dd666f6cb2e09d462a0097f6b87dc5d8d7faa26c25b12b5b408d53e6e3b8"
        },
        {
            "va": "0x180008e60",
            "size": 5,
            "sha256": "ec4430cf0d4e58f59f0e0e41827fcb0d349becc91a1b06663a32fb46ccd1a271"
        },
        {
            "va": "0x180004c50",
            "size": 38,
            "sha256": "c449e2ae1073fb78e0ebea7a97693665ed43243ee84869a5551494045b787865"
        },
        {
            "va": "0x180062f20",
            "size": 5,
            "sha256": "d25e8de98bd9c65b3cdfcc72e04d568e4897e8d38cbe6e4c0667b6aa159caa3a"
        },
        {
            "va": "0x18014d85c",
            "size": 76,
            "sha256": "db7870727102db8be8296c0b9e5fd81b4bb42e51c2f56c91880eb330b6772280"
        }
    ],
    "evidence": "original 0x180119060 returns the index-th child element named ImpulseResponse of the XML element at [rcx] (first-child 0x180013210, next-sibling 0x180012dc0, name compare = wcscmp); it rebuilds a juce::String per step, so a catalog of thousands of children costs ~5M emulated allocations"
}
]

KNOWN = {
    'UAD Oxford Dynamic EQ': dict(sha256='2d566c78e0075f6f09c9f714521767829c02f8ad8b44bf191e89823161dfecd0', render_id=2215, handler=dict(kind='class', cls='JuceVSTWrapper', offset=0x128),
                                  latency=50, latency_check=True),
    'UAD Oxford SuprEsser DS': dict(sha256='a639d0dce2f251147d46d83e9b2dccc16d90d5fd0a6b95681e0292a9053dfcb5', render_id=2267, handler=dict(kind='class', cls='JuceVSTWrapper', offset=0x128),
                                    latency=178, latency_check=True),
    'UAD Sphere Mic Collection': dict(sha256='bf25d61785fe0d985564468408521eef55d9fe5c6f93ca190c3227b79510443b', render_id=0x87a,
                                      handler=dict(kind='sphere', offset=0x1da8), latency=24),
    'UAD Sphere Mic Collection 180': dict(sha256='c7f451272a46fdc582af689efecb628b9089e65eaf0a764fa61f5f8ef35656d0', render_id=0x87b,
                                          handler=dict(kind='sphere', offset=0x1da8), latency=24),
    # Older interface (0x66): PluginConvolution handler at +0xa8, startup is handler slot 4 (slot 17 is a destructor there). The
    # constructor scans the studio data folder and loads the .irz (a zip) synchronously through JUCE FileInputStream.
    'UAD Ocean Way Studios': dict(sha256='0eef9560524312d08984cac19fb77ed9eeffd9e731666707e6aac46ab3bf3795', render_id=2106,
                                  handler=dict(kind='class', cls='PluginConvolution', offset=0xa8), latency=None, interface_version=0x66,
                                  startup_slot=4,
                                  # the original setter (processor vtable slot 0x1800fd380) zeroes Predelay (35) and forces Wet Solo (38) and Dry/Wet (39) to 1 unless Mode (0) is Reverb (1)
                                  control_requires={'35': [(0, 1)], '38': [(0, 1)], '39': [(0, 1)]},
                                  native_routines=NATIVE_OCEAN,
                                  data_files=[dict(guest='C:\\Program Files (x86)\\Universal Audio\\Powered Plugins\\Data\\Ocean Way Studios\\OceanWayStudios.irz',
                                                   host=str(Path(os.environ.get('OA_UAD_ROOT', '/')) / 'Program Files (x86)/Universal Audio/Powered Plugins/Data/Ocean Way Studios/OceanWayStudios.irz'))]),
}
SLOTS = dict(descriptor=3, formatter=15, startup=17)      # CUADPlugCallbackHandler slot numbers (same ABI as the other families)
HOST = dict(sample_rate=48000, block_size=32)
BUDGET = 4_000_000_000                                    # the original construction of the whole mic-model object graph runs ~2e9


def class_vtables(img, class_name):
    """{sub-object offset: vtable VA} from the RTTI complete-object locators of the class."""
    tds = [m.start() - 16 for m in re.finditer(rb'\.\?AV' + class_name.encode() + rb'@@\x00', img.im)]
    if len(tds) != 1:
        raise ValueError('one type descriptor required for ' + class_name)
    out = {}
    for m in re.finditer(struct.pack('<I', 1), img.im):
        o = m.start()
        if o % 4 or o + 24 > len(img.im):
            continue
        sig, off, cd, td, cdr, selfr = struct.unpack_from('<6I', img.im, o)
        if sig == 1 and td == tds[0] and selfr == o:
            ptr = struct.pack('<Q', img.base + o)
            p = img.im.find(ptr)
            while p >= 0:
                out.setdefault(off, []).append(img.base + p + 8)
                p = img.im.find(ptr, p + 1)
    return out


def thunk_target(img, va):
    """Function behind a this-adjusting thunk: the first direct call / jmp inside it."""
    o = va - img.base           # thunks are leaf routines without an unwind record: linear scan to the first transfer
    for i in img.cs.disasm(img.im[o:o + 64], va):
        if i.mnemonic in ('call', 'jmp') and i.operands[0].type == X86_OP_IMM:
            return i.operands[0].imm
        if i.mnemonic == 'ret':
            break
    return None


def uses_immediate(img, va, value):
    try:
        _, _, ins = img.function(va)
    except (ValueError, TypeError):
        return False
    return any(i.mnemonic == 'mov' and len(i.operands) == 2 and i.operands[1].type == X86_OP_IMM and i.operands[1].imm == value for i in ins)


def build(name, inventory):
    known = KNOWN.get(name)
    if known is None:
        raise ValueError('no Sphere facts for ' + name)
    item = inventory[name]
    dll = DLL_ROOT / (name + '.dll')
    if sha(dll) != item['dll_sha256'] or item['dll_sha256'] != known['sha256']:
        raise ValueError('original DLL identity differs')
    if item['render_id_manifest'] != known['render_id']:
        raise ValueError('render id differs from the manifest')
    capture = CAPTURES / (item['capture'] + '.json')
    img = Image(dll)
    props = captured_parameters(capture)
    count = item['parameter_count']
    if sorted(props) != list(range(count)):
        raise ValueError('captured parameter schema differs')
    # --- the handler (CUADPlugCallbackHandler sub-object) ----------------------------------------------------------
    kind, want = known['handler']['kind'], known['handler']['offset']
    handlers = []
    if kind == 'sphere':        # the >= 19-slot sub-object whose slot 3 builds the 0xae8-byte AllocInfo
        for off, vts in class_vtables(img, 'SphereUADProcessor').items():
            for vt in vts:
                slots = img.vtable(vt, 64)
                if len(slots) >= 19 and uses_immediate(img, thunk_target(img, slots[SLOTS['descriptor']]), ALLOC_INFO_SIZE):
                    handlers.append((off, vt, slots))
    elif kind == 'class':   # a named class's sub-object at the pinned offset, 19 slots: JuceVSTWrapper+0x128 (the wrapper constructor hands
        for off, vts in class_vtables(img, known['handler']['cls']).items():     # this to UAD_CreateIUADPlug), PluginConvolution+0xa8
            for vt in vts:
                slots = img.vtable(vt, 64)
                if off == want and len(slots) >= 19 and all(img.base + 0x1000 <= v < img.base + 0x1000000 for v in slots[:19]):
                    handlers.append((off, vt, slots))
    else:
        raise ValueError('unknown handler kind')
    if len(handlers) != 1 or handlers[0][0] != want:
        raise ValueError('unique handler sub-object at the pinned offset required: ' + repr([(hex(o), hex(v)) for o, v, _ in handlers]))
    offset, vtable, slots = handlers[0]
    getter = slots[SLOTS['descriptor']]
    inner = thunk_target(img, getter) if kind == 'sphere' else None
    # --- Console schema ---------------------------------------------------------------------------------------------
    params, text, corrections = [], [], []
    for i in range(count):
        p = props[i]
        steps = p.get('StepValue', {}).get('values')
        maximum = len(steps) - 1 if steps else CONTINUOUS_DOMAIN
        norm = p['NormalizedValue']['value']
        default = round(norm * maximum)
        if not 0 <= default <= maximum or abs(default / maximum - norm) > 0.5 / maximum:
            raise ValueError('captured default is off the integer grid for ' + p['Name']['value'])
        params.append(dict(name=p['Name']['value'], maximum=maximum, default=default, steps=len(steps) if steps else 0))
        label = console_text(p)
        if steps and label is not None and label != steps[default].strip():
            # The capture is inconsistent for this parameter (Ocean Way Mono Sum: StringValue 'In' beside NormalizedValue 0.0 and
            # StepValue 'Out'): the step list at the captured default is the Console text; recorded in the profile evidence.
            corrections.append(dict(parameter=i, name=p['Name']['value'], captured_string=label, step_label=steps[default].strip()))
            label = steps[default].strip()
        text.append(label)
    latency = [c['value'] for c in json.loads(capture.read_text())['controls'] if c['path'].endswith('/Latency')]
    if known['latency'] is not None and latency and latency != [known['latency']]:
        raise ValueError('captured latency differs from the pinned one')
    return dict(
        schema=SCHEMA, status='discovered', name=name, framework='DSPAudioProcessor',
        dll=dict(path=str(dll), sha256=item['dll_sha256']),
        captured_metadata=dict(path=str(capture), sha256=sha(capture)),
        dspap=dict(
            handler=dict(offset=hex(offset), vtable=hex(vtable), slots={hex(8 * i): hex(v) for i, v in enumerate(slots[:19])}),
            descriptor=dict(getter_va=hex(getter), inner_va=hex(inner) if inner else None, render_id=known['render_id'],
                            evidence='handler slot 3 = the descriptor getter (original handler vtable); render id equals the manifest'),
            formatter=dict(slot=hex(8 * SLOTS['formatter']), va=hex(slots[SLOTS['formatter']])),
            startup=[dict(slot=hex(8 * known.get('startup_slot', SLOTS['startup'])), va=hex(slots[known.get('startup_slot', SLOTS['startup'])]), arguments=['handler'])],
            parameters=params, captured_text=text_or_none(text), plain_fields={}, quiet_iface_slots=[],
            vstmain=dict(interface_version=known.get('interface_version', 0x67), host=HOST, instruction_budget=BUDGET, latency_samples=latency[0] if latency else known['latency'],
                        **({'data_files': [dict(f, sha256=sha(f['host'])) for f in known['data_files']]} if known.get('data_files') else {}),
                        **({'control_requires': known['control_requires']} if known.get('control_requires') else {}),
                        **({'native_routines': known['native_routines']} if known.get('native_routines') else {}),
                        **({'latency_check': dict(handler_field=hex(8), offset=hex(0x3c), expect=known['latency'])} if known.get('latency_check') else {}),
                        evidence='RTTI complete-object locators of the handler class; families-dspaudioprocessor.md section 6a'),
            evidence=dict(source='families-dspaudioprocessor.md section 6a', capture_text_corrections=corrections)),
    )


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--plugin', action='append')
    ap.add_argument('--output', type=Path, default=HERE / 'profiles-vstmain.json')
    args = ap.parse_args()
    inventory = {p['name']: p for p in json.loads((PATTERNS / 'families/inventory.json').read_text())['plugins']}
    names = args.plugin or sorted(KNOWN)
    profiles = [build(n, inventory) for n in names]
    doc = dict(schema='openapollo-batch-discovery/1', profiles=profiles, counts=dict(selected=len(names), discovered=len(profiles)),
               source_sha256=sha(__file__))
    args.output.write_text(json.dumps(doc, indent=2) + '\n')
    print(json.dumps(dict(output=str(args.output), profiles=[p['name'] for p in profiles])))


if __name__ == '__main__':
    main()
