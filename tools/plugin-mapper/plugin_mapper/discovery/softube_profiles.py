"""Static profile discovery for the Softube-engine family (`CSoftubeEffectUAD`: AudioEffectX + CUADPlugCallbackHandler),
executed by executor/softube_capture.py.

Sibling of vstmain_profiles.py: same output envelope ({'schema': 'openapollo-batch-discovery/1', 'profiles': [...]})
consumable by `execution/run_catalog.py --profiles <file>`, profile schema `openapollo-softube-profile/1`. Never loads or
executes vendor code; the executor runs the original DllMain and VSTPluginMain, which build the whole object graph, and
checks every fact below against it.

Derived from the image (every plug-in of the family, no per-plug-in table):
  * identity: the DLL hash against families/inventory.json;
  * the object: the RTTI complete-object locators of `CSoftubeEffectUAD` give each sub-object's vtable. The primary one is
    at +0. The CUADPlugCallbackHandler is the unique sub-object with >= 19 slots whose slot 3 (the descriptor getter) clears
    and fills a 0xae8-byte AllocInfo; the executor checks that the original constructor passes exactly this sub-object to
    UAD_CreateIUADPlug;
  * the getter's channel request: the getter counts connected inputs and outputs up to two plug-in fields it reads at
    negative displacements from the handler (`cmp ebx, [r14 - d]` around the calls to the effect's vtable +0x180 and
    +0x188, isInputConnected / isOutputConnected); the executor writes the host's channel request there;
  * the Console schema (names, step lists, defaults, text) from the metadata: stepped parameters have step count - 1 as
    integer domain, continuous ones 32768 (a default off that grid is the nearest point, within half a step, and marked
    inexact); the executor checks each against the plug-in's own getParameter.
Pinned (VST2 SDK 2.4 and UAD2SDK ABI, checked at run time): the AEffect at +0x30 of the AudioEffect (the executor checks
the primary vtable at effect - 0x30 and the AEffect magic), interface version 0x67, handler slots 3 (descriptor), 15
(formatter), 17 (startup), the host's sample rate and block size, and a per-call instruction budget (the amp rooms' and
Marshalls' construction runs past the executor's default 20M).

Usage: python3 softube_profiles.py [--plugin NAME ...] [--output profiles.json]
"""
from pathlib import Path
import argparse, json
from capstone.x86 import X86_OP_IMM, X86_OP_MEM
from dspaudioprocessor_profiles import Image, sha, DLL_ROOT, CAPTURES, PATTERNS, captured_parameters, console_text, text_or_none
from vstmain_profiles import class_vtables

HERE = Path(__file__).resolve().parent
SCHEMA = 'openapollo-softube-profile/1'
FRAMEWORK = 'Softube'
CLASS = 'CSoftubeEffectUAD'
CONTINUOUS_DOMAIN = 32768
ALLOC_INFO_SIZE = 0xae8
AEFFECT_OFFSET = 0x30                 # AudioEffect: vtable, audioMaster, editor, sampleRate, blockSize, numPrograms, numParams, curProgram, cEffect
INTERFACE_VERSION = 0x67
SLOTS = dict(descriptor=3, formatter=15, startup=17)
CONNECTED = (0x180, 0x188)            # AudioEffectX vtable: isInputConnected, isOutputConnected
HOST = dict(sample_rate=48000, block_size=32)
BUDGET = 1_000_000_000                # per emulated call (executor/softube_capture.py call_budget)


def fills_alloc_info(img, va):
    """The function at va sets a size of 0xae8 before its first return (linear scan: the getters' unwind data is chained,
    so the first PE function record covers only their prologue)."""
    o = va - img.base
    if not 0x1000 <= o < len(img.im):
        return False
    for i in img.cs.disasm(img.im[o:o + 0x400], va):
        if i.mnemonic == 'mov' and len(i.operands) == 2 and i.operands[1].type == X86_OP_IMM and i.operands[1].imm == ALLOC_INFO_SIZE:
            return True
        if i.mnemonic in ('ret', 'int3'):
            return False
    return False


def handler_of(img):
    """(offset, vtable, slots) of the CUADPlugCallbackHandler sub-object of CSoftubeEffectUAD."""
    vts = class_vtables(img, CLASS)
    if list(vts.get(0, ())) == [] or len(vts[0]) != 1:
        raise ValueError('one primary vtable at +0 required')
    found = []
    for off, vas in vts.items():
        for vt in vas:
            slots = img.vtable(vt, 64)
            if off and len(slots) >= 19 and fills_alloc_info(img, slots[SLOTS['descriptor']]):
                found.append((off, vt, slots))
    if len(found) != 1:
        raise ValueError('unique handler sub-object required: ' + repr([(hex(o), hex(v)) for o, v, _ in found]))
    return vts[0][0], found[0]


def getter_fields(img, getter):
    """The two handler-relative fields the getter loops over (inputs, then outputs), each loop calling the effect's
    vtable at +0x180 / +0x188."""
    o = getter - img.base
    disps, calls = [], []
    for i in img.cs.disasm(img.im[o:o + 0x200], getter):
        if i.mnemonic == 'cmp':
            for op in i.operands:
                if op.type == X86_OP_MEM and op.size == 4 and op.mem.disp < 0 and op.mem.index == 0:
                    if op.mem.disp not in disps:
                        disps.append(op.mem.disp)
        elif i.mnemonic == 'call' and i.operands[0].type == X86_OP_MEM:
            calls.append(i.operands[0].mem.disp)
        if len(disps) >= 2 and len(calls) >= 2:
            break
    if len(disps) < 2 or tuple(calls[:2]) != CONNECTED or disps[1] != disps[0] + 4:
        raise ValueError('getter channel loops not recognized: ' + repr((disps, calls)))
    return dict(num_inputs_disp=disps[0], num_outputs_disp=disps[1])


def build(name, inventory):
    item = inventory[name]
    dll = DLL_ROOT / (name + '.dll')
    if sha(dll) != item['dll_sha256']:
        raise ValueError('original DLL identity differs')
    capture = CAPTURES / (item['capture'] + '.json')
    img = Image(dll)
    primary, (offset, vtable, slots) = handler_of(img)
    getter = slots[SLOTS['descriptor']]
    fields = getter_fields(img, getter)
    props = captured_parameters(capture)
    count = item['parameter_count']
    if sorted(props) != list(range(count)):
        raise ValueError('captured parameter schema differs')
    params, text = [], []
    for i in range(count):
        p = props[i]
        steps = p.get('StepValue', {}).get('values')
        maximum = len(steps) - 1 if steps else CONTINUOUS_DOMAIN
        norm = p['NormalizedValue']['value']
        default = round(norm * maximum)
        if not 0 <= default <= maximum or abs(default / maximum - norm) > 0.5 / maximum:
            raise ValueError('captured default is off the integer grid for ' + p['Name']['value'])
        exact = abs(default / maximum - norm) <= 1e-6
        label = console_text(p)
        params.append(dict(name=p['Name']['value'], maximum=maximum, default=default, steps=len(steps) if steps else 0,
                           exact=exact, captured_text=label))
        text.append(label)
    return dict(
        schema=SCHEMA, status='discovered', name=name, framework=FRAMEWORK,
        dll=dict(path=str(dll), sha256=item['dll_sha256']),
        captured_metadata=dict(path=str(capture), sha256=sha(capture)),
        softube=dict(
            primary_vtable=hex(primary), aeffect_offset=hex(AEFFECT_OFFSET), interface_version=INTERFACE_VERSION,
            handler=dict(offset=hex(offset), vtable=hex(vtable), slots={hex(8 * i): hex(v) for i, v in enumerate(slots[:19])}),
            descriptor=dict(getter_va=hex(getter),
                            evidence='handler slot 3 fills a 0xae8-byte AllocInfo (original handler vtable)'),
            getter_fields=fields,
            formatter=dict(slot=hex(8 * SLOTS['formatter']), va=hex(slots[SLOTS['formatter']])),
            startup=dict(slot=hex(8 * SLOTS['startup']), va=hex(slots[SLOTS['startup']])),
            parameters=params, captured_text=text_or_none(text), host=HOST, instruction_budget=BUDGET,
            evidence='RTTI complete-object locators of CSoftubeEffectUAD; descriptor getter channel loops'),
    )


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--plugin', action='append')
    ap.add_argument('--output', type=Path, default=HERE / 'profiles-softube.json')
    args = ap.parse_args()
    inventory = {p['name']: p for p in json.loads((PATTERNS / 'families/inventory.json').read_text())['plugins']}
    names = args.plugin or sorted(inventory)
    profiles = [build(n, inventory) for n in names]
    doc = dict(schema='openapollo-batch-discovery/1', profiles=profiles, counts=dict(selected=len(names), discovered=len(profiles)),
               source_sha256=sha(__file__))
    args.output.write_text(json.dumps(doc, indent=2) + '\n')
    print(json.dumps(dict(output=str(args.output), profiles=[p['name'] for p in profiles])))


if __name__ == '__main__':
    main()
