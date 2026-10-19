"""Static profile discovery for the Sonnox "Soup" layer UAD plug-ins (JUCE 7 VST2 wrapper + Soup processor).

Sibling of dspaudioprocessor_profiles.py: same output envelope ({'schema': 'openapollo-batch-discovery/1',
'profiles': [...]}) consumable by `catalog-execution/run_catalog.py --profiles <file>`, profile schema
`openapollo-dspaudioprocessor-profile/1` plus a `dspap.sonnox` block that executor/sonnox_capture.py consumes.
Never loads or executes vendor code.

Derived from the image and re-verified on every run:
  * identity: DLL and capture hashes against families/inventory.json; render id against the descriptor getter;
  * the handler vtable (JuceVSTWrapper+0x128): slot 3 is the descriptor getter, slots 15/17 the text and reload slots;
  * the Soup parameter table: the static `Soup::Defs` data (law id, range, step count per parameter) read from the
    PE data section, against the captured Console schema (names, step labels, defaults);
  * the calculation object (`*_UAParameterCalculation`): vtable, and the exact IUADPlug write lists of its two
    writer slots (slot 9 steady-state, slot 10 setup) and of the listener update chain;
  * every host-service routine the executor replaces (allocator family, printf helper, host detection, clock,
    latency broadcast, dynamic_cast), each with the hash of its leading bytes.
What is reverse-engineered (Ghidra, see families-dspaudioprocessor.md) and pinned by identity checks here is in `KNOWN`.

Usage: python3 sonnox_profiles.py [--plugin NAME ...] [--output profiles.json]
"""
from pathlib import Path
import argparse, hashlib, json, math, struct
from dspaudioprocessor_profiles import Image, sha, DLL_ROOT, CAPTURES, PATTERNS, SCHEMA, IFACE_WRITE_SLOTS, console_text, text_or_none

HERE = Path(__file__).resolve().parent
LAW_NAMES = {0: 'square', 1: 'log', 2: 'linear', 3: 'binary', 4: 'one_minus_square', 5: 'one_over', 6: 'one_over_legacy', 7: 'multi_slope',
             8: 'asym_square', 9: 'asym_linear', 10: 'reversed_one_minus_square'}
CONTINUOUS_DOMAIN = 32768  # same grid the CHyp families use for continuous Console parameters

KNOWN = {
    'UAD Oxford Limiter': dict(
        sha256='f6ffec25cfcd7e32e1701ae48e40afa9fc4f228de7a68d1cb1c6e6fa72ed36f5',
        render_id=0x8a1,
        handler=dict(offset=0x128, vtable=0x180c57138, getter=0x18000ae70, formatter_slot=0x78, reload_slot=0x88,
                     processor_field=0x8, iface_field=0x228, io_in=0x60, io_out=0x64),
        vtables=dict(primary=0x180c56948, listener=0x180c56630, iface=0x180c56890),    # processor, +0x150, +0xf98
        fields=dict(listener=0x150, iface=0xf98, calc=0xfb8, iuad=0xfc0, counter=0xfa8, latency_cache=0x3c, rate=0x30, block=0x38, defs=0x1b8),
        casts={0x180d23d78: 0xf98},                  # Soup::IUASonnoxAudioProcessor RTTI -> sub-object offset
        listener_set=0x18010fca0, listener_slot=0x28,  # listener slot 5: calc.setParameter + flush
        flush=0x18010f530, flush_range=(0x1000007, 0x1000012),   # processor slots 0x2e8 / 0x2f0 return these refs
        calc=dict(constructor=0x18010c290, size=0x5b0, vtable=0x180c562e0, setup_slot=0x8, set_slot=0x18, latency_slot=0x38,
                  steady_slot=0x48, setup_writer_slot=0x50, steady_va=0x18010b010, steady_size=1083, setup_va=0x18010ad20, setup_size=737),
        calc_steady_refs=[0x1000008, 0x1000009, 0x100000a, 0x100000b, 0x789, 0x100000c, 0x100000d, 0x100000e, 0x100000f, 0x1000010, 0x1000011,
                          0x78d, 0x78e, 0x78f, 0x790, 0x792, 0x793, 0x799, 0x79c, 0x79d, 0x785, 0x787, 0x786, 0x79e, 0x79f, 0x7d7, 0x80f, 0x847,
                          0x885, 0x886, 0x887, 0x888, 0x889],
        calc_setup_refs=[0x781, 0x88e, 0x88f, 0x782, 0x783, 0x784, 0x788, 0x78a, 0x78b, 0x78c, 0x791, 0x794, 0x795, 0x796, 0x797, 0x798, 0x79a, 0x79b,
                         0x87f, 0x880, 0x881, 0x882, 0x883, 0x884],
        defs=dict(factory=0x18010c9d0, parameter_vtable=0x180c5b748, static_table=0x180d1f770, static_stride=0x14),
        static_initializers=[0x1800012c0, 0x180001330, 0x180001420, 0x180001680],  # step-label tables (Off/On, dither type, depth), then the Defs data
        host_id=dict(**{'global': 0x180d36998}, value=3),   # VSTPluginMain stores 3 before anything else
        host=dict(sample_rate=48000, block_size=32, io_channels=2),
        ids=['Inpt', 'Atck', 'Rlse', 'Crve', 'WLvl', 'Outp', 'Auto', 'Safe', 'Metr', 'ACmp', 'Peak', 'Dith', 'DTyp', 'DiDp', 'Thrs', 'PwBy'],
        # (role, va): host services of the original image the executor answers itself
        hooks=[('alloc', 0x1800d7fb0), ('alloc', 0x1800f74b8), ('calloc', 0x1800f7518), ('realloc', 0x1800f6ce4), ('free', 0x1800e22d8),
               ('free', 0x1800f6d60), ('msize', 0x180108984), ('printf', 0x18000d230), ('host_detect', 0x1801ae030), ('clock', 0x18000b970),
               ('broadcast', 0x1801a9760), ('cast', 0x1800dd2d0)],
    ),
    # Same Soup layer, same host glue (the processor class layout, listener, flush and reload routines are byte-identical
    # to the Limiter's); different calculation object (inline constructor, setParameter re-normalizes through the Defs
    # singleton) and the UA flavour of the Defs (`Inflator_UADefs`, 8 parameters, replaces the generic 7).
    'UAD Oxford Inflator': dict(
        sha256='057f210b445db767f099aab737d9d97c331a96aa1e37bbb9d4a7d08d0f4ebdcd',
        render_id=0x849,
        handler=dict(offset=0x128, vtable=0x180be4400, getter=0x18000bd20, formatter_slot=0x78, reload_slot=0x88,
                     processor_field=0x8, iface_field=0x228, io_in=0x60, io_out=0x64),
        vtables=dict(primary=0x180be3778, listener=0x180be3d90, iface=0x180be3130),
        fields=dict(listener=0x150, iface=0xf98, calc=0xfb8, iuad=0xfc0, counter=0xfa8, latency_cache=0x3c, rate=0x30, block=0x38, defs=0x1b8),
        casts={0x180cabfb8: 0xf98},
        listener_set=0x18010fea0, listener_slot=0x28,
        flush=0x18010f800, flush_range=(0x1000007, 0x1000010),
        calc=dict(constructor=None, plain=[dict(offset=0x34, size=4, value=0x3f800000)], size=0x38, vtable=0x180be40e8, setup_slot=0x8, set_slot=0x18,
                  latency_slot=0x38, steady_slot=0x48, setup_writer_slot=0x50, steady_va=0x18010c780, steady_size=302, setup_va=0x18010c3a0, setup_size=978),
        calc_steady_refs=[0x1000009, 0x100000d, 0x100000c, 0x100000a, 0x100000b, 0x100000f, 0x100000e, 0x1000008],
        calc_setup_refs=[0x2b, 0x2c, 0x5, 0xa, 0xf, 0x14, 0x19, 0x1a, 0x1b],
        defs=dict(factory=0x18010b7e0, parameter_vtable=0x180bf0fe8, static_table=0x180ca7c80, static_stride=0x14),
        static_initializers=[0x1800012c0, 0x180001320, 0x180001380, 0x1800013e0, 0x180001940],  # label tables, generic Defs data, UA Defs data
        host_id=dict(**{'global': 0x180cbeccc}, value=3),
        host=dict(sample_rate=48000, block_size=32, io_channels=2),
        ids=['inGn', 'crv2', 'crv1', 'otGn', 'INBT', 'SPLT', 'LIMT', 'PwBy'],
        hooks=[('alloc', 0x1800dbbfc), ('alloc', 0x1800f82d0), ('calloc', 0x1800f8330), ('realloc', 0x1800f7afc), ('free', 0x1800e3108),
               ('free', 0x1800f7b78), ('msize', 0x1801098e4), ('printf', 0x180025b50), ('host_detect', 0x1801b1ea0), ('clock', 0x18000c820),
               ('broadcast', 0x1801af230), ('cast', 0x1800de0d8), ('defs', 0x18010b750)],
    ),
}


def iuad_writes(img, va, size):
    """(kind, ref) of every IUADPlug write in the routine's linear span (chained PE fragments make the unwind
    range too short), in code order: `call/jmp [reg+slot]` and the compiler's split form (function pointer
    loaded from [reg+slot] into a register, called later). The ref (edx) is resolved by tracking constant
    registers (`mov r, imm`, `xor r, r`, `mov r, r2`, `lea r, [r2+imm]`); an unresolved ref raises."""
    from capstone.x86 import X86_OP_IMM, X86_OP_MEM, X86_OP_REG
    ins = list(img.cs.disasm(img.im[va - img.base:va - img.base + size], va))
    def base(name):  # eax/ax/al -> rax, r9d -> r9
        if name.startswith('r') and name[1:].rstrip('dwb').isdigit():
            return 'r' + name[1:].rstrip('dwb')
        return {'eax': 'rax', 'ebx': 'rbx', 'ecx': 'rcx', 'edx': 'rdx', 'esi': 'rsi', 'edi': 'rdi', 'ebp': 'rbp'}.get(name, name)
    const, loaded, out = {}, {}, []
    for i in ins:
        ops = i.operands
        if i.mnemonic == 'xor' and len(ops) == 2 and ops[0].type == X86_OP_REG and ops[1].type == X86_OP_REG and ops[0].reg == ops[1].reg:
            const[base(i.reg_name(ops[0].reg))] = 0
        elif i.mnemonic == 'mov' and len(ops) == 2 and ops[0].type == X86_OP_REG:
            dst = base(i.reg_name(ops[0].reg))
            src = ops[1]
            if src.type == X86_OP_IMM:
                const[dst] = src.imm & 0xffffffff
            elif src.type == X86_OP_REG and base(i.reg_name(src.reg)) in const:
                const[dst] = const[base(i.reg_name(src.reg))]
            else:
                const.pop(dst, None)
            if src.type == X86_OP_MEM and src.mem.disp in IFACE_WRITE_SLOTS:
                loaded[dst] = src.mem.disp
            else:
                loaded.pop(dst, None)
        elif i.mnemonic == 'lea' and len(ops) == 2 and ops[0].type == X86_OP_REG and ops[1].type == X86_OP_MEM and not ops[1].mem.index and ops[1].mem.base and base(i.reg_name(ops[1].mem.base)) in const:
            const[base(i.reg_name(ops[0].reg))] = (const[base(i.reg_name(ops[1].mem.base))] + ops[1].mem.disp) & 0xffffffff
        elif ops and ops[0].type == X86_OP_REG and i.mnemonic not in ('call', 'jmp', 'cmp', 'test', 'push', 'ucomisd', 'comisd', 'ucomiss', 'comiss'):
            const.pop(base(i.reg_name(ops[0].reg)), None)
            loaded.pop(base(i.reg_name(ops[0].reg)), None)
        target = None
        if i.mnemonic in ('call', 'jmp') and ops[0].type == X86_OP_MEM and ops[0].mem.disp in IFACE_WRITE_SLOTS:
            target = ops[0].mem.disp
        elif i.mnemonic in ('call', 'jmp') and ops[0].type == X86_OP_REG and base(i.reg_name(ops[0].reg)) in loaded:
            target = loaded[base(i.reg_name(ops[0].reg))]
        if target is not None:
            if 'rdx' not in const:
                raise ValueError('unresolved IUADPlug write ref at ' + hex(i.address))
            out.append((IFACE_WRITE_SLOTS[target], const['rdx']))
        if i.mnemonic == 'call' and target is None:
            for r in ('rax', 'rcx', 'rdx', 'r8', 'r9', 'r10', 'r11'):  # volatile registers die across a call
                const.pop(r, None)
                loaded.pop(r, None)
    return out


def leaf(img, va):
    """Instructions of a tiny constant-returning routine (no unwind record, so no PE function boundary)."""
    return list(img.cs.disasm(img.im[va - img.base:va - img.base + 16], va))


def build(name, inventory):
    known = KNOWN.get(name)
    if known is None:
        raise ValueError('no Sonnox-layer facts for ' + name)
    item = inventory[name]
    dll = DLL_ROOT / (name + '.dll')
    if sha(dll) != item['dll_sha256'] or item['dll_sha256'] != known['sha256']:
        raise ValueError('original DLL identity differs')
    capture = CAPTURES / (item['capture'] + '.json')
    img = Image(dll)
    h = known['handler']
    slots = img.vtable(h['vtable'], 19)
    if len(slots) < 19 or slots[3] != h['getter']:
        raise ValueError('handler slot 3 is not the pinned descriptor getter')
    # descriptor getter: render id comes from IUASonnoxAudioProcessor slot 2; it must return the manifest render id
    iface = img.vtable(known['vtables']['iface'], 17)
    ins = leaf(img, iface[2])
    if not (ins[0].mnemonic == 'mov' and ins[0].op_str == 'eax, 0x%x' % known['render_id'] and known['render_id'] == item['render_id_manifest']):
        raise ValueError('IUASonnoxAudioProcessor render id differs from the manifest')
    listener = img.vtable(known['vtables']['listener'], 8)
    if listener[known['listener_slot'] // 8] != known['listener_set']:
        raise ValueError('listener slot is not the pinned set-parameter routine')
    calc = img.vtable(known['calc']['vtable'], 12)
    c = known['calc']
    if calc[c['steady_slot'] // 8] != c['steady_va'] or calc[c['setup_writer_slot'] // 8] != c['setup_va']:
        raise ValueError('calculation writer slots differ')
    steady = iuad_writes(img, c['steady_va'], c['steady_size'])
    if [r for _, r in steady] != known['calc_steady_refs']:
        raise ValueError('steady-state DSP write list differs: ' + repr(steady))
    setup = iuad_writes(img, c['setup_va'], c['setup_size'])
    if [r for _, r in setup] != known['calc_setup_refs']:
        raise ValueError('setup DSP write list differs: ' + repr(setup))
    # flush range: processor slots 0x2e8 / 0x2f0 are `mov eax, imm; ret`
    primary = img.vtable(known['vtables']['primary'], 100)
    flush = []
    for slot in (0x2e8, 0x2f0):
        fi = leaf(img, primary[slot // 8])
        if fi[0].mnemonic != 'mov' or fi[1].mnemonic != 'ret':
            raise ValueError('flush range slot is not a constant')
        flush.append(int(fi[0].op_str.split(', ')[1], 16))
    if tuple(flush) != tuple(known['flush_range']):
        raise ValueError('flush bracket range differs')
    # --- parameters: static Soup table + captured Console schema -----------------------------------------
    cap = json.loads(capture.read_text())
    props = {}
    for ctl in cap['controls']:
        p = ctl['path'].split('/parameters/')
        if len(p) == 2:
            idx, field = p[1].split('/')
            props.setdefault(int(idx), {})[field] = ctl
    count = item['parameter_count']
    if sorted(props) != list(range(count)) or count != len(known['ids']):
        raise ValueError('captured parameter schema differs')
    params, text = [], []
    for i in range(count):
        row = struct.unpack_from('<ffIfI', img.im, known['defs']['static_table'] + i * known['defs']['static_stride'] - img.base)
        law, lo, hi = row[2], row[0], row[1]
        steps = struct.unpack_from('<I', img.im, known['defs']['static_table'] + i * known['defs']['static_stride'] + 12 - img.base)[0]
        if law not in LAW_NAMES:
            raise ValueError('unknown Soup law id')
        labels = props[i].get('StepValue', {}).get('values')
        if steps:
            if not labels or len(labels) != steps:
                raise ValueError('captured step labels differ from the Soup step count for ' + props[i]['Name']['value'])
            maximum = steps - 1
        else:
            if labels:
                raise ValueError('captured step labels for a continuous Soup parameter')
            maximum = CONTINUOUS_DOMAIN
        norm = props[i]['NormalizedValue']['value']
        default = round(norm * maximum)
        if not 0 <= default <= maximum or abs(default / maximum - norm) > 1.0 / maximum:
            raise ValueError('captured default is off the integer grid for ' + props[i]['Name']['value'])
        params.append(dict(name=props[i]['Name']['value'], id=known['ids'][i], maximum=maximum, default=default, law=LAW_NAMES[law], steps=steps,
                           table_range=[lo, hi], captured_normalized=norm))
        text.append(console_text(props[i]))
    latency = [c['value'] for c in cap['controls'] if c['path'] == cap['path'] + '/Latency']
    if latency and (len(latency) != 1 or type(latency[0]) is not int):
        raise ValueError('one captured integer latency required')
    hooks = [dict(role=role, va=hex(va), head_sha256=hashlib.sha256(img.im[va - img.base:va - img.base + 16]).hexdigest()) for role, va in known['hooks']]
    return dict(
        schema=SCHEMA, status='discovered', name=name, framework='DSPAudioProcessor',
        dll=dict(path=str(dll), sha256=item['dll_sha256']),
        captured_metadata=dict(path=str(capture), sha256=sha(capture)),
        dspap=dict(
            processor_vtable=hex(known['vtables']['primary']),
            handler=dict(offset=hex(h['offset']), vtable=hex(h['vtable']), slots={hex(8 * k): hex(v) for k, v in enumerate(slots)}),
            iface_offset=hex(known['fields']['iuad']), wrapper_offset=hex(h['offset']), sample_rate_offset=hex(known['fields']['rate']),
            descriptor=dict(getter_va=hex(h['getter']), render_store_va=None, render_id=known['render_id'],
                            evidence='Soup IUASonnoxAudioProcessor slot 2 constant == manifest render id'),
            update=dict(va=hex(known['listener_set']), writes=[dict(kind=k, ref=hex(r)) for k, r in steady], setup_writes=[dict(kind=k, ref=hex(r)) for k, r in setup],
                        flush=dict(va=hex(known['flush']), begin=hex(flush[0]), end=hex(flush[1])), pointer_fields=[]),
            formatter=dict(slot=hex(h['formatter_slot']), va=hex(slots[h['formatter_slot'] // 8])),
            startup=[dict(slot=hex(h['reload_slot']), va=hex(slots[h['reload_slot'] // 8]), arguments=['handler'])],
            parameters=params, plain_fields={}, quiet_iface_slots=[], captured_text=text_or_none(text),
            sonnox=dict(vtables={k: hex(v) for k, v in known['vtables'].items()}, fields={k: hex(v) for k, v in known['fields'].items()},
                        casts={hex(k): hex(v) for k, v in known['casts'].items()},
                        calc=dict(constructor=hex(c['constructor']) if c['constructor'] else None, size=hex(c['size']), vtable=hex(c['vtable']),
                                  plain=[dict(offset=hex(f['offset']), size=f['size'], value=f['value']) for f in c.get('plain', [])]),
                        defs=dict(factory=hex(known['defs']['factory']), parameter_vtable=hex(known['defs']['parameter_vtable'])),
                        static_initializers=[hex(v) for v in known['static_initializers']],
                        host_id={'global': hex(known['host_id']['global']), 'value': known['host_id']['value']},
                        host=known['host'], latency_samples=latency[0] if latency else None,
                        wrapper=dict(handler_offset=hex(h['offset']), processor_field=hex(h['processor_field']), iface_field=hex(h['iface_field']),
                                     io_in=hex(h['io_in']), io_out=hex(h['io_out'])),
                        hooks=hooks),
            evidence=dict(source='families-dspaudioprocessor.md section 5')),
    )


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--plugin', action='append')
    ap.add_argument('--output', type=Path, default=HERE / 'profiles-sonnox.json')
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
